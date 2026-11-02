//! Regions that hold many metal spots, cut into pieces around clusters of
//! them (user idea, 2026-11-02).
//!
//! zkta's regions follow the obstacles. On open maps that leaves regions
//! kilometres across holding a dozen or more spots (Comet Catcher Redux,
//! Red Comet, Agalta), and every per-region read lumps those spots
//! together: who holds the region, its center as the place to measure
//! distances from, routes taken straight across it. So such a region is cut
//! into one piece per cluster of its spots. The spots are clustered by
//! complete linkage (no two spots of a cluster more than [`CLUSTER_SPAN`]
//! apart), and every cell goes to the cluster whose nearest spot is the
//! shortest walk away inside the region: a geodesic Voronoi diagram, so a
//! piece never reaches across an obstacle. Where two pieces meet is a
//! [`ChokeKind::Seam`].
//!
//! Regions on maps of narrow passages are small already and stay whole; a
//! survey of 17 pool maps (zkgbai-runs/regionsplit) cut Comet Catcher
//! Redux's 15 bot regions into 41 and Calamity's 53 into 54.
//!
//! [`ChokeKind::Seam`]: crate::ChokeKind::Seam

use std::cmp::Reverse;
use std::collections::BinaryHeap;

use crate::borders::{FLANK_REACH, segment_distance};
use crate::{CELL, ChokeKind, NONE, Pos, Region, TierMap};

/// No two spots of one cluster are farther apart than this (elmos).
pub const CLUSTER_SPAN: f32 = 1000.0;

/// Regions with fewer metal spots stay whole.
pub const MIN_SPOTS: usize = 4;

/// A region is cut into at most one piece per this many cells (800 x 800
/// elmos), however many clusters its spots make.
const PIECE_CELLS: usize = 2500;

/// Pieces smaller than this (cells, 400 x 400 elmos) join the piece they
/// touch most.
const SLIVER_CELLS: usize = 625;

/// Spots this far (elmos) off passable ground still count for the region
/// nearest.
const SPOT_SNAP: f32 = 64.0;

/// On re-analysis a region holding the same spots as before keeps its
/// piece cap while its size stays within this fraction of the cap's range
/// of sizes ([`PIECE_CELLS`] wide): a structure levelling its footprint
/// must not change how many pieces a region is cut into.
const CAP_SLACK: f32 = 0.05;

/// The piece cap a region holding `spots` (sorted, as bits) was cut with.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Cap {
    spots: Vec<(u32, u32)>,
    pieces: usize,
}

/// Walking costs on the grid: straight and diagonal steps.
const STEP: u32 = 10;
const DIAGONAL: u32 = 14;

/// Cut every region of `map` holding at least [`MIN_SPOTS`] of `spots` whose
/// spots make more than one cluster. Runs on fresh region labels, before
/// the borders are linked: gates keep their regions (the piece on their
/// side), and the pieces' contacts become seams. The order of `spots` does
/// not matter. `prior`: the piece caps of the model this one replaces (see
/// [`CAP_SLACK`]); the caps used go into `map`.
pub(crate) fn split(map: &mut TierMap, spots: &[Pos], prior: &[Cap]) {
    if spots.is_empty() {
        return;
    }
    // Ties (equal distances, common on mirrored maps) go by position, not
    // by the order the spots came in.
    let mut spots = spots.to_vec();
    spots.sort_unstable_by(|a, b| a.x.total_cmp(&b.x).then(a.z.total_cmp(&b.z)));
    spots.dedup();
    let n = map.regions.len();
    let mut by_region: Vec<Vec<Pos>> = vec![Vec::new(); n];
    for &s in &spots {
        if let Some(r) = map.region_near(s, SPOT_SNAP) {
            by_region[r].push(s);
        }
    }
    for (r, spots) in by_region.iter().enumerate() {
        if spots.len() < MIN_SPOTS {
            continue;
        }
        let cells = map.regions[r].cells;
        let key: Vec<(u32, u32)> = spots.iter().map(|p| (p.x.to_bits(), p.z.to_bits())).collect();
        let max_pieces = prior
            .iter()
            .find(|c| c.spots == key)
            .map(|c| c.pieces)
            .filter(|&k| cap_holds(k, cells))
            .unwrap_or((cells / PIECE_CELLS).max(1));
        map.built.caps.push(Cap { spots: key, pieces: max_pieces });
        let clusters = clusters(spots, CLUSTER_SPAN, max_pieces);
        if clusters.len() > 1 {
            cut(map, r, spots, &clusters);
        }
    }
}

/// Whether a region of `cells` cells may keep a cap of `pieces`: whether
/// its size is within [`CAP_SLACK`] of the sizes that cap goes with.
fn cap_holds(pieces: usize, cells: usize) -> bool {
    let range = PIECE_CELLS as f32;
    let low = if pieces <= 1 { 0.0 } else { (pieces as f32 - CAP_SLACK) * range };
    let high = (pieces as f32 + 1.0 + CAP_SLACK) * range;
    (low..high).contains(&(cells as f32))
}

/// Complete-linkage clusters of `spots` (indices): the two clusters whose
/// farthest spots are nearest merge, while that is at most `span` apart or
/// while there are more than `max` clusters.
pub fn clusters(spots: &[Pos], span: f32, max: usize) -> Vec<Vec<usize>> {
    let mut clusters: Vec<Vec<usize>> = (0..spots.len()).map(|i| vec![i]).collect();
    let linkage = |a: &[usize], b: &[usize]| {
        a.iter().flat_map(|&i| b.iter().map(move |&j| spots[i].distance(spots[j]))).fold(0.0f32, f32::max)
    };
    while clusters.len() > 1 {
        let mut best: Option<(f32, usize, usize)> = None;
        for i in 0..clusters.len() {
            for j in i + 1..clusters.len() {
                let d = linkage(&clusters[i], &clusters[j]);
                if best.is_none_or(|(b, _, _)| d < b) {
                    best = Some((d, i, j));
                }
            }
        }
        let Some((d, i, j)) = best else { break };
        if d > span && clusters.len() <= max {
            break;
        }
        let merged = clusters.remove(j);
        clusters[i].extend(merged);
    }
    clusters
}

/// Cut region `r` into one piece per cluster of its `spots`.
fn cut(map: &mut TierMap, r: usize, spots: &[Pos], clusters: &[Vec<usize>]) {
    let (w, h) = (map.width, map.height);
    let label = r as u32;
    let cells: Vec<usize> = (0..w * h).filter(|&c| map.region[c] == label).collect();
    let inside = |c: usize| map.region[c] == label;

    // Geodesic Voronoi diagram of the clusters inside the region.
    let mut piece = vec![NONE; w * h];
    let mut dist = vec![u32::MAX; w * h];
    let mut heap = BinaryHeap::new();
    for (k, cluster) in clusters.iter().enumerate() {
        for &i in cluster {
            if let Some(c) = nearest_inside(map, spots[i], label) {
                heap.push(Reverse((0u32, k as u32, c)));
            }
        }
    }
    while let Some(Reverse((d, k, c))) = heap.pop() {
        if dist[c] <= d {
            continue;
        }
        dist[c] = d;
        piece[c] = k;
        let (x, z) = ((c % w) as isize, (c / w) as isize);
        for (dx, dz) in [(-1, 0), (1, 0), (0, -1), (0, 1), (-1, -1), (1, -1), (-1, 1), (1, 1)] {
            let (nx, nz) = (x + dx, z + dz);
            if nx < 0 || nz < 0 || nx as usize >= w || nz as usize >= h {
                continue;
            }
            let n = nz as usize * w + nx as usize;
            if !inside(n) {
                continue;
            }
            let step = if dx != 0 && dz != 0 {
                // No cutting corners past cells outside the region.
                if !inside(z as usize * w + nx as usize) || !inside(nz as usize * w + x as usize) {
                    continue;
                }
                DIAGONAL
            } else {
                STEP
            };
            if d + step < dist[n] {
                heap.push(Reverse((d + step, k, n)));
            }
        }
    }

    // Cells no spot's walk reaches (bits joined only through a corner) go
    // with the first piece.
    for &c in &cells {
        if piece[c] == NONE {
            piece[c] = 0;
        }
    }

    // Each gate goes with the piece that holds most of the ground within
    // FLANK_REACH of its line on this side, and that piece takes all of it
    // (ground near two gates goes with the nearer): the next region meets
    // one piece at the gate and its sides, measured as before the cut. (A
    // seam running into a wide gate made its flanks borders of their own,
    // and the gate look as if there were no way round it.)
    let gates: Vec<usize> =
        (map.regions[r].chokes.iter().copied()).filter(|&g| map.chokes[g].kind == ChokeKind::Gate).collect();
    let voronoi = piece.clone();
    let mut claim = vec![f32::INFINITY; w * h];
    let mut bands: Vec<(usize, Vec<usize>)> = Vec::new();
    for &g in &gates {
        let choke = &map.chokes[g];
        let off = |p: Pos| match choke.gate {
            Some([a, b]) => segment_distance(p, a, b),
            None => p.distance(choke.pos) - choke.width * 0.5,
        };
        let band: Vec<(usize, f32)> =
            (cells.iter()).map(|&c| (c, off(map.cell_center(c)))).filter(|&(_, d)| d <= FLANK_REACH + CELL).collect();
        if let Some(p) = most(clusters.len(), band.iter().map(|&(c, _)| voronoi[c])) {
            for &(c, d) in &band {
                if d < claim[c] {
                    claim[c] = d;
                    piece[c] = p;
                }
            }
        }
        bands.push((g, band.into_iter().map(|(c, _)| c).collect()));
    }

    // Stray bits of a piece (apart from its biggest part) and slivers join
    // the piece they touch most.
    let size = tidy(&mut piece, &cells, &inside, w, h, clusters.len());
    if size.iter().filter(|&&s| s > 0).count() < 2 {
        return;
    }
    // The piece that keeps the region's id: the one around its center.
    let center = map.cell_at(map.regions[r].center);
    let keep = if inside(center) {
        piece[center]
    } else {
        (0..size.len()).max_by_key(|&k| (size[k], Reverse(k))).unwrap_or(0) as u32
    };
    let gate_piece: Vec<(usize, u32)> = bands
        .iter()
        .map(|(g, band)| (*g, most(clusters.len(), band.iter().map(|&c| piece[c])).unwrap_or(keep)))
        .collect();

    // Ids: the kept piece stays `r`, the others are new regions.
    let mut id = vec![usize::MAX; clusters.len()];
    id[keep as usize] = r;
    for k in 0..clusters.len() {
        if size[k] > 0 && id[k] == usize::MAX {
            id[k] = map.regions.len();
            let whole = map.regions[r].whole;
            let component = map.regions[r].component;
            map.regions.push(Region {
                id: id[k],
                center: map.regions[r].center,
                cells: 0,
                component,
                chokes: Vec::new(),
                whole,
            });
        }
    }
    let mut members: Vec<Vec<usize>> = vec![Vec::new(); clusters.len()];
    for &c in &cells {
        let k = piece[c] as usize;
        map.region[c] = id[k] as u32;
        members[k].push(c);
    }
    for (k, m) in members.iter().enumerate().filter(|(_, m)| !m.is_empty()) {
        let reg = &mut map.regions[id[k]];
        reg.cells = m.len();
        reg.center = centre(m, w);
    }
    for (g, p) in gate_piece {
        let into = id[p as usize];
        let choke = &mut map.chokes[g];
        for x in choke.regions.iter_mut().filter(|x| **x == r) {
            *x = into;
        }
        choke.regions.sort_unstable();
        choke.regions.dedup();
        if into != r {
            map.regions[into].chokes.push(g);
        }
    }
    let chokes = &map.chokes;
    map.regions[r].chokes.retain(|&g| chokes[g].regions.contains(&r));
}

/// The most frequent of `labels` (below `n`), the lowest on a tie.
fn most(n: usize, labels: impl Iterator<Item = u32>) -> Option<u32> {
    let mut count = vec![0usize; n];
    labels.for_each(|k| count[k as usize] += 1);
    (0..n).filter(|&k| count[k] > 0).max_by_key(|&k| (count[k], Reverse(k))).map(|k| k as u32)
}

/// Give every piece's stray bits (all but its biggest connected part) and
/// then every sliver (under [`SLIVER_CELLS`]) to the piece they touch most;
/// returns the pieces' sizes. A bit that touches no other piece stays.
fn tidy(
    piece: &mut [u32],
    cells: &[usize],
    inside: &impl Fn(usize) -> bool,
    w: usize,
    h: usize,
    n: usize,
) -> Vec<usize> {
    // The piece most of a set of cells' neighbours outside it belong to.
    let touching = |piece: &[u32], part: &[usize], own: u32| {
        let outside = part.iter().flat_map(|&c| neighbours4(c, w, h)).filter(|&x| inside(x) && piece[x] != own);
        most(n, outside.map(|x| piece[x]))
    };
    // Connected parts of every piece.
    let mut seen = vec![false; piece.len()];
    let mut parts: Vec<Vec<usize>> = Vec::new();
    for &start in cells {
        if seen[start] {
            continue;
        }
        seen[start] = true;
        let mut part = vec![start];
        let mut i = 0;
        while i < part.len() {
            for x in neighbours4(part[i], w, h) {
                if !seen[x] && inside(x) && piece[x] == piece[start] {
                    seen[x] = true;
                    part.push(x);
                }
            }
            i += 1;
        }
        parts.push(part);
    }
    let mut biggest = vec![usize::MAX; n];
    for (i, part) in parts.iter().enumerate() {
        let k = piece[part[0]] as usize;
        if biggest[k] == usize::MAX || part.len() > parts[biggest[k]].len() {
            biggest[k] = i;
        }
    }
    for (i, part) in parts.iter().enumerate() {
        let own = piece[part[0]];
        if biggest[own as usize] != i
            && let Some(into) = touching(piece, part, own)
        {
            part.iter().for_each(|&c| piece[c] = into);
        }
    }

    let mut size = vec![0usize; n];
    cells.iter().for_each(|&c| size[piece[c] as usize] += 1);
    let mut stuck = vec![false; n];
    while size.iter().filter(|&&s| s > 0).count() > 1 {
        let small = (0..n).filter(|&k| !stuck[k] && size[k] > 0 && size[k] < SLIVER_CELLS);
        let Some(small) = small.min_by_key(|&k| (size[k], k)) else { break };
        let part: Vec<usize> = cells.iter().copied().filter(|&c| piece[c] == small as u32).collect();
        let Some(into) = touching(piece, &part, small as u32) else {
            stuck[small] = true;
            continue;
        };
        part.iter().for_each(|&c| piece[c] = into);
        size[into as usize] += size[small];
        size[small] = 0;
    }
    size
}

/// The cell of region `label` nearest `p` within [`SPOT_SNAP`].
fn nearest_inside(map: &TierMap, p: Pos, label: u32) -> Option<usize> {
    let reach = (SPOT_SNAP / CELL).ceil() as isize;
    let (cx, cz) = ((p.x / CELL) as isize, (p.z / CELL) as isize);
    let mut best: Option<(f32, usize)> = None;
    for dz in -reach..=reach {
        for dx in -reach..=reach {
            let (x, z) = (cx + dx, cz + dz);
            if x < 0 || z < 0 || x as usize >= map.width || z as usize >= map.height {
                continue;
            }
            let c = z as usize * map.width + x as usize;
            if map.region[c] != label {
                continue;
            }
            let d = map.cell_center(c).distance(p);
            if d <= SPOT_SNAP + CELL && best.is_none_or(|(b, _)| d < b) {
                best = Some((d, c));
            }
        }
    }
    best.map(|(_, c)| c)
}

/// The member cell nearest the centroid of `cells` (the centroid itself
/// may lie outside a concave piece).
fn centre(cells: &[usize], width: usize) -> Pos {
    let (sx, sz) = cells.iter().fold((0.0, 0.0), |(x, z), &c| (x + (c % width) as f32, z + (c / width) as f32));
    let k = cells.len() as f32;
    let centroid = Pos::new((sx / k + 0.5) * CELL, (sz / k + 0.5) * CELL);
    let at = |c: usize| Pos::new(((c % width) as f32 + 0.5) * CELL, ((c / width) as f32 + 0.5) * CELL);
    cells
        .iter()
        .map(|&c| at(c))
        .min_by(|a, b| a.distance(centroid).total_cmp(&b.distance(centroid)))
        .unwrap_or(centroid)
}

fn neighbours4(c: usize, w: usize, h: usize) -> impl Iterator<Item = usize> {
    let (x, z) = (c % w, c / w);
    [(x > 0).then(|| c - 1), (x + 1 < w).then(|| c + 1), (z > 0).then(|| c - w), (z + 1 < h).then(|| c + w)]
        .into_iter()
        .flatten()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::build::tier_map;
    use crate::strategy::{Analysis, Asset, Scenario, Side};
    use crate::{SpringMapData, Tier};

    /// A flat map `sw` x `sh` slope cells, with `wall` cells impassable.
    fn map(sw: u32, sh: u32, wall: impl Fn(u32, u32) -> bool) -> SpringMapData {
        let mut slope = vec![0.0f32; (sw * sh) as usize];
        for z in 0..sh {
            for x in 0..sw {
                if wall(x, z) {
                    slope[(z * sw + x) as usize] = 1.0;
                }
            }
        }
        SpringMapData {
            map_width: sw * 2,
            map_height: sh * 2,
            slope_width: sw,
            slope_height: sh,
            slope_map: slope,
            height_map: vec![50.0; (sw * sh * 4) as usize],
        }
    }

    /// Three spots around (`x`, `z`) elmos.
    fn triple(x: f32, z: f32) -> [Pos; 3] {
        [Pos::new(x, z), Pos::new(x + 150.0, z + 40.0), Pos::new(x + 60.0, z + 160.0)]
    }

    fn seams(t: &TierMap) -> Vec<usize> {
        t.chokes.iter().filter(|c| c.kind == ChokeKind::Seam).map(|c| c.id).collect()
    }

    #[test]
    fn complete_linkage_does_not_chain() {
        let chain: Vec<Pos> = (0..4).map(|i| Pos::new(600.0 * i as f32, 0.0)).collect();
        assert_eq!(clusters(&chain, 1000.0, 10), vec![vec![0, 1], vec![2, 3]]);
        // Single linkage would have made one cluster of the chain.
        assert_eq!(clusters(&chain, 1000.0, 1), vec![vec![0, 1, 2, 3]]);
        let far: Vec<Pos> = (0..4).map(|i| Pos::new(5000.0 * i as f32, 0.0)).collect();
        assert_eq!(clusters(&far, 1000.0, 2).len(), 2);
    }

    #[test]
    fn an_open_field_is_cut_between_its_clusters() {
        let data = map(192, 96, |_, _| false);
        let spots: Vec<Pos> = [triple(300.0, 600.0), triple(2600.0, 700.0)].concat();
        let whole = tier_map(&data, Tier::Bot, Tier::Bot.zero_k_class(), &[]);
        let cut = tier_map(&data, Tier::Bot, Tier::Bot.zero_k_class(), &spots);
        assert_eq!(whole.regions.len(), 1, "{:?}", whole.regions);
        assert!(seams(&whole).is_empty());

        assert_eq!(cut.regions.len(), 2);
        assert!(cut.regions.iter().all(|r| r.whole == 0 && r.cells > 0));
        let (a, b) = (cut.region_at(spots[0]).unwrap(), cut.region_at(spots[3]).unwrap());
        assert_ne!(a, b);
        assert!(spots[..3].iter().all(|&s| cut.region_at(s) == Some(a)));
        assert!(spots[3..].iter().all(|&s| cut.region_at(s) == Some(b)));
        // The seam runs down the middle, between the clusters' nearest spots.
        let s = seams(&cut);
        assert!(!s.is_empty());
        for &c in &s {
            let c = &cut.chokes[c];
            assert_eq!(c.regions, vec![0, 1]);
            assert!((1350.0..1650.0).contains(&c.pos.x), "seam at {:?}", c.pos);
            assert!(c.hold_width(0).is_infinite());
        }
        assert_eq!(whole.cells_total(), cut.cells_total());
    }

    #[test]
    fn few_spots_or_one_cluster_leave_a_region_whole() {
        let data = map(192, 96, |_, _| false);
        let three = triple(300.0, 600.0);
        let t = tier_map(&data, Tier::Bot, Tier::Bot.zero_k_class(), &three);
        assert_eq!(t.regions.len(), 1);
        let close: Vec<Pos> = [triple(300.0, 600.0), triple(600.0, 700.0)].concat();
        let t = tier_map(&data, Tier::Bot, Tier::Bot.zero_k_class(), &close);
        assert_eq!(t.regions.len(), 1);
    }

    /// Two rooms joined by a corridor through a wall at x 60..68 (cells);
    /// the right room is 3 km long.
    fn rooms() -> SpringMapData {
        map(256, 96, |x, z| (60..68).contains(&x) && !(44..52).contains(&z))
    }

    #[test]
    fn a_gate_stays_with_the_piece_beside_it() {
        let near = triple(1300.0, 700.0);
        let far = triple(3700.0, 700.0);
        let spots: Vec<Pos> = [near, far].concat();
        let whole = tier_map(&rooms(), Tier::Bot, Tier::Bot.zero_k_class(), &[]);
        let t = tier_map(&rooms(), Tier::Bot, Tier::Bot.zero_k_class(), &spots);
        assert_eq!(t.regions.len(), whole.regions.len() + 1);

        let left = t.region_at(Pos::new(300.0, 700.0)).unwrap();
        let (rn, rf) = (t.region_at(near[0]).unwrap(), t.region_at(far[0]).unwrap());
        assert_ne!(rn, rf);
        assert_eq!(t.regions[rn].whole, t.regions[rf].whole);
        assert_eq!(t.regions[left].whole, left);
        // The corridor's gate joins the left room and the near piece.
        let gate = t.chokes.iter().find(|c| c.kind == ChokeKind::Gate && c.side(left).is_some()).unwrap();
        assert_eq!(gate.side(rf), None);
        assert!(gate.side(rn).is_some(), "{gate:?}");
        assert!(t.regions[rn].chokes.contains(&gate.id));
        assert!(!t.regions[rf].chokes.contains(&gate.id));
        // No border between the left room and the far piece; the pieces
        // meet at seams only.
        assert!(t.neighbours(left).all(|(_, r)| r != rf));
        assert!(t.neighbours(rn).filter(|&(_, r)| r == rf).all(|(c, _)| t.chokes[c].kind == ChokeKind::Seam));
        assert!(t.neighbours(rn).any(|(_, r)| r == rf));
    }

    #[test]
    fn seams_are_never_held_walled_or_guards() {
        let near = triple(1300.0, 700.0);
        let far = triple(3700.0, 700.0);
        let spots: Vec<Pos> = [near, far].concat();
        let t = tier_map(&rooms(), Tier::Bot, Tier::Bot.zero_k_class(), &spots);
        let assets: Vec<Asset> = spots.iter().map(|&pos| Asset { pos, value: 1.0, side: Side::Ally }).collect();
        let s = Scenario { map: &t, ally_base: far[0], enemy_base: Pos::new(300.0, 700.0), assets: &assets };
        let a = Analysis::run(&s);
        let seam = |c: &usize| t.chokes[*c].kind == ChokeKind::Seam;
        assert!(!a.hold.iter().any(seam), "hold {:?}", a.hold);
        assert!(!a.seal.iter().any(seam), "seal {:?}", a.seal);
        assert!(!a.walls.iter().any(|w| seam(&w.choke)));
        assert!(!a.guards_vs_enemy.iter().flatten().any(seam));
        // The corridor still guards both pieces.
        let gate = t.chokes.iter().find(|c| c.kind == ChokeKind::Gate).unwrap().id;
        let (rn, rf) = (t.region_at(near[0]).unwrap(), t.region_at(far[0]).unwrap());
        assert!(a.guards_vs_enemy[rn].contains(&gate) && a.guards_vs_enemy[rf].contains(&gate));
    }

    #[test]
    fn deterministic() {
        let spots: Vec<Pos> = [triple(1300.0, 700.0), triple(3700.0, 700.0), triple(2500.0, 1300.0)].concat();
        let a = tier_map(&rooms(), Tier::Bot, Tier::Bot.zero_k_class(), &spots);
        let b = tier_map(&rooms(), Tier::Bot, Tier::Bot.zero_k_class(), &spots);
        assert_eq!(a.region, b.region);
        assert_eq!(a.chokes.len(), b.chokes.len());
        assert!(a.chokes.iter().zip(&b.chokes).all(|(x, y)| x.regions == y.regions && x.pos == y.pos));
    }

    #[test]
    fn the_order_of_the_spots_does_not_matter() {
        // A square of spots, sides inside a cluster's span, diagonals not:
        // two clusters, either both rows or both columns, a tie only the
        // order of the spots used to break.
        let (x, z, d) = (2000.0, 250.0, 900.0);
        let spots = [Pos::new(x, z), Pos::new(x + d, z), Pos::new(x, z + d), Pos::new(x + d, z + d)];
        let a = tier_map(&rooms(), Tier::Bot, Tier::Bot.zero_k_class(), &spots);
        assert_eq!(a.regions.len(), 3, "{:?}", a.regions);
        for order in [vec![spots[0], spots[2], spots[1], spots[3]], spots.iter().rev().copied().collect()] {
            let b = tier_map(&rooms(), Tier::Bot, Tier::Bot.zero_k_class(), &order);
            assert_eq!(a.region, b.region);
            assert!(a.chokes.iter().zip(&b.chokes).all(|(x, y)| x.regions == y.regions && x.pos == y.pos));
        }
    }

    #[test]
    fn a_cap_holds_while_the_size_stays_near_its_range() {
        let p = PIECE_CELLS;
        assert!(cap_holds(1, 0) && cap_holds(1, 2 * p + p / 40) && !cap_holds(1, 2 * p + p / 10));
        assert!(cap_holds(2, 2 * p - p / 40) && !cap_holds(2, 2 * p - p / 10));
        assert!(cap_holds(2, 3 * p + p / 40) && !cap_holds(2, 3 * p + p / 10));
    }

    impl TierMap {
        fn cells_total(&self) -> usize {
            self.regions.iter().map(|r| r.cells).sum()
        }
    }
}
