//! Strategic analysis of one tier's region graph: which chokepoints guard
//! which regions, and which terrain interventions would hurt the enemy most
//! (or protect us best).
//!
//! Everything is a small graph computation over regions (nodes) and
//! chokepoints (edges, weighted by travel distance through the choke);
//! only bridge discovery looks at the cell grid.

use std::collections::{BTreeMap, BinaryHeap, VecDeque};

use crate::graph::{FlowNet, HeapItem};
use crate::paths::{Paths, SNAP};
use crate::{CELL, Choke, NONE, Pos, TierMap};

/// Whose an asset is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    Ally,
    Enemy,
    Neutral,
}

/// Something worth holding or taking: a metal spot, a building...
#[derive(Clone, Copy, Debug)]
pub struct Asset {
    pub pos: Pos,
    pub value: f32,
    pub side: Side,
}

/// What the analysis needs to know about the game.
pub struct Scenario<'a> {
    pub map: &'a TierMap,
    pub ally_base: Pos,
    pub enemy_base: Pos,
    pub assets: &'a [Asset],
}

/// Value of the assets in a region, by side.
#[derive(Clone, Copy, Debug, Default)]
pub struct RegionValue {
    pub ally: f32,
    pub enemy: f32,
    pub neutral: f32,
}

/// Walling off a chokepoint.
#[derive(Clone, Debug)]
pub struct WallOption {
    pub choke: usize,
    /// Wall length needed (the passage width), elmos.
    pub width: f32,
    /// Allied value no longer reachable by enemy ground forces.
    pub protects: f32,
    /// Regions it seals from the enemy base.
    pub regions: Vec<usize>,
}

/// A terraformed ramp or land bridge between two regions.
#[derive(Clone, Debug)]
pub struct BridgeOption {
    pub from: usize,
    pub to: usize,
    /// The two passable cells the bridge would join.
    pub ends: [Pos; 2],
    /// Length of the impassable stretch to fill, elmos.
    pub span: f32,
    /// Enemy value weighted by how much closer it gets to our base
    /// (newly reachable value counts in full).
    pub enemy_exposed: f32,
    /// Enemy guards (chokepoints protecting their value from us) bypassed.
    pub guards_bypassed: usize,
    pub score: f32,
}

/// Landing ground forces by air.
#[derive(Clone, Debug)]
pub struct DropOption {
    pub region: usize,
    pub enemy_value: f32,
    /// Ground travel distance from our base, `None` if unreachable.
    pub ground_distance: Option<f32>,
    /// Chokepoints our ground forces would have to pass.
    pub guards: usize,
    pub score: f32,
}

/// Placing a teleporter exit (units travel from home to `region` at once).
#[derive(Clone, Debug)]
pub struct TeleportOption {
    pub region: usize,
    /// Enemy value weighted by the travel distance saved.
    pub gain: f32,
}

#[derive(Clone, Debug, Default)]
pub struct Analysis {
    pub ally_base: Option<usize>,
    pub enemy_base: Option<usize>,
    pub values: Vec<RegionValue>,
    /// Ground travel distance from each base, `f32::INFINITY` if none.
    pub dist_from_ally: Vec<f32>,
    pub dist_from_enemy: Vec<f32>,
    /// Chokepoints every enemy ground route from their base to the region
    /// passes (empty: exposed via independent routes, or unreachable).
    pub guards_vs_enemy: Vec<Vec<usize>>,
    /// Chokepoints every ground route from our base to the region passes.
    pub guards_vs_ally: Vec<Vec<usize>>,
    /// Walls by allied value protected per elmo of wall, best first.
    pub walls: Vec<WallOption>,
    /// The walls (on our side of the map) that best trade wall length for
    /// allied value cut off from the enemy base; empty if no wall is worth
    /// building.
    pub seal: Vec<usize>,
    /// The chokepoints (on our side, at most [`MAX_HOLD_WIDTH`] wide) that
    /// best trade the number held for allied value cut off from the enemy
    /// base: where static defense pays.
    pub hold: Vec<usize>,
    pub bridges: Vec<BridgeOption>,
    pub drops: Vec<DropOption>,
    pub teleports: Vec<TeleportOption>,
}

/// Longest obstacle stretch considered bridgeable, elmos.
pub const MAX_BRIDGE_SPAN: f32 = 320.0;

/// A bridge must save at least this much ground travel between its ends
/// (elmos) to be worth building.
pub const MIN_BRIDGE_SAVING: f32 = 800.0;

/// Wall length (elmos) worth one average asset when choosing a seal.
pub const WALL_ELMOS_PER_ASSET: f32 = 300.0;

/// Widest chokepoint static defense can hold, elmos.
pub const MAX_HOLD_WIDTH: f32 = 600.0;

/// Cost of holding a chokepoint, in average asset values (plus up to half
/// an asset for width).
pub const HOLD_COST: f32 = 1.0;

impl Analysis {
    pub fn run(s: &Scenario) -> Analysis {
        let map = s.map;
        let n = map.regions.len();
        if n == 0 {
            return Analysis::default();
        }
        let ally_base = map.region_near(s.ally_base, SNAP);
        let enemy_base = map.region_near(s.enemy_base, SNAP);
        let graph = RegionGraph::new(map);

        let mut values = vec![RegionValue::default(); n];
        for a in s.assets {
            if let Some(r) = map.region_near(a.pos, SNAP) {
                let v = &mut values[r];
                match a.side {
                    Side::Ally => v.ally += a.value,
                    Side::Enemy => v.enemy += a.value,
                    Side::Neutral => v.neutral += a.value,
                }
            }
        }

        let dist_from_ally = ally_base.map_or_else(|| vec![f32::INFINITY; n], |b| graph.distances(b, None));
        let dist_from_enemy = enemy_base.map_or_else(|| vec![f32::INFINITY; n], |b| graph.distances(b, None));
        let guards_vs_enemy = enemy_base.map_or_else(|| vec![Vec::new(); n], |b| graph.guards(b));
        let guards_vs_ally = ally_base.map_or_else(|| vec![Vec::new(); n], |b| graph.guards(b));

        let mut a = Analysis {
            ally_base,
            enemy_base,
            values,
            dist_from_ally,
            dist_from_enemy,
            guards_vs_enemy,
            guards_vs_ally,
            ..Analysis::default()
        };
        a.walls = a.wall_options(map);
        a.seal = a.seal_walls(map);
        let mean_asset = s.assets.iter().map(|a| a.value).sum::<f32>() / s.assets.len().max(1) as f32;
        a.hold = a.hold_chokes(map, mean_asset);
        a.bridges = a.bridge_options(map, &graph);
        a.drops = a.drop_options(map);
        a.teleports = a.teleport_options(&graph);
        a
    }

    /// Travel distance from the ally (`true`) or enemy base to a chokepoint.
    pub fn choke_distance(&self, map: &TierMap, choke: usize, ally: bool) -> f32 {
        let d = if ally { &self.dist_from_ally } else { &self.dist_from_enemy };
        let c = &map.chokes[choke];
        c.regions.iter().map(|&r| d[r] + map.regions[r].center.distance(c.pos)).fold(f32::INFINITY, f32::min)
    }

    /// Whether a chokepoint is nearer our base than theirs by ground.
    pub fn choke_on_our_side(&self, map: &TierMap, choke: usize) -> bool {
        self.choke_distance(map, choke, true) < self.choke_distance(map, choke, false)
    }

    /// The region next to a chokepoint on our side of it.
    pub fn home_side(&self, map: &TierMap, choke: usize) -> Option<usize> {
        map.chokes[choke]
            .regions
            .iter()
            .copied()
            .min_by(|&a, &b| self.dist_from_ally[a].total_cmp(&self.dist_from_ally[b]))
    }

    fn wall_options(&self, map: &TierMap) -> Vec<WallOption> {
        let mut by_choke: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
        for (r, guards) in self.guards_vs_enemy.iter().enumerate() {
            for &c in guards {
                by_choke.entry(c).or_default().push(r);
            }
        }
        let mut walls: Vec<WallOption> = by_choke
            .into_iter()
            .map(|(c, regions)| WallOption {
                choke: c,
                width: map.chokes[c].width,
                protects: regions.iter().map(|&r| self.values[r].ally + 0.5 * self.values[r].neutral).sum(),
                regions,
            })
            .filter(|w| w.protects > 0.0)
            .collect();
        walls.sort_by(|a, b| (b.protects / b.width).total_cmp(&(a.protects / a.width)));
        walls
    }

    /// The best walls to build, as a minimum cut (project selection): from
    /// the enemy base, every allied-valued region drains its value to the
    /// sink (the price of leaving it exposed) and every chokepoint costs its
    /// wall length, priced at [`WALL_ELMOS_PER_ASSET`] per average asset.
    /// The cheapest cut is the most valuable set of walls. Only chokepoints
    /// on our side (nearer our base than theirs) can be walled: walls at the
    /// enemy's doorstep would not survive, so the cut must not simply box
    /// them in.
    fn seal_walls(&self, map: &TierMap) -> Vec<usize> {
        self.value_cut(map, |choke, typical| Some(choke.width * typical / WALL_ELMOS_PER_ASSET))
    }

    fn hold_chokes(&self, map: &TierMap, mean_asset: f32) -> Vec<usize> {
        self.value_cut(map, |choke, _| {
            (choke.width <= MAX_HOLD_WIDTH).then(|| mean_asset * (HOLD_COST + 0.5 * choke.width / MAX_HOLD_WIDTH))
        })
    }

    /// The chokepoints on our side of the map whose closing best trades
    /// cost for allied value cut off from the enemy base (a min-cut in
    /// project-selection form). `cost` is the price of closing a chokepoint
    /// given the value of a typical valued region, `None` if it cannot be
    /// closed.
    fn value_cut(&self, map: &TierMap, cost: impl Fn(&Choke, f32) -> Option<f32>) -> Vec<usize> {
        let Some(source) = self.enemy_base else { return Vec::new() };
        let value = |r: usize| self.values[r].ally + 0.5 * self.values[r].neutral;
        let targets: Vec<usize> = (0..map.regions.len())
            .filter(|&r| r != source && value(r) > 0.0 && self.dist_from_enemy[r].is_finite())
            .collect();
        if targets.is_empty() {
            return Vec::new();
        }
        let typical = targets.iter().map(|&r| value(r)).sum::<f32>() / targets.len() as f32;
        let sink = map.regions.len();
        let mut flow = FlowNet::new(sink + 1);
        for (c, choke) in map.chokes.iter().enumerate() {
            let cap = if self.choke_on_our_side(map, c) { cost(choke, typical) } else { None };
            let cap = cap.map_or(f64::INFINITY, f64::from);
            for (i, &a) in choke.regions.iter().enumerate() {
                for &b in &choke.regions[i + 1..] {
                    flow.add_edge(a, b, cap, Some(c));
                }
            }
        }
        for &t in &targets {
            flow.add_edge(t, sink, value(t) as f64, None);
        }
        flow.min_cut(source, sink).map(|c| c.chokes).unwrap_or_default()
    }

    fn bridge_options(&self, map: &TierMap, graph: &RegionGraph) -> Vec<BridgeOption> {
        let Some(home) = self.ally_base else { return Vec::new() };
        let enemy_regions: Vec<usize> = (0..map.regions.len()).filter(|&r| self.values[r].enemy > 0.0).collect();
        if enemy_regions.is_empty() {
            return Vec::new();
        }
        let scale = ((map.width * map.height) as f32).sqrt() * CELL;
        let mut options: Vec<BridgeOption> = bridge_candidates(map)
            .into_iter()
            .filter_map(|cand| {
                // Distances from the far end, to see what the bridge brings closer.
                let (near, far) = if self.dist_from_ally[cand.from] <= self.dist_from_ally[cand.to] {
                    (cand.from, cand.to)
                } else {
                    (cand.to, cand.from)
                };
                if !self.dist_from_ally[near].is_finite() {
                    return None;
                }
                let from_far = graph.distances(far, None);
                // Only real shortcuts: the ends must be far apart by ground.
                if from_far[near] < cand.span + MIN_BRIDGE_SAVING {
                    return None;
                }
                let via = self.dist_from_ally[near] + cand.span;
                let mut exposed = 0.0;
                let mut bypassed = 0;
                for &r in &enemy_regions {
                    let before = self.dist_from_ally[r];
                    let after = before.min(via + from_far[r]);
                    if after < before {
                        exposed += self.values[r].enemy
                            * if before.is_finite() { ((before - after) / scale).min(1.0) } else { 1.0 };
                    }
                }
                // Guards of enemy value the new edge goes around.
                if far != home {
                    let guards_after = graph.guards_with_extra_edge(home, near, far);
                    for &r in &enemy_regions {
                        bypassed += self.guards_vs_ally[r].len().saturating_sub(guards_after[r].len());
                    }
                }
                let score = (exposed + 0.1 * bypassed as f32 * self.values_scale()) / (1.0 + cand.span / 160.0);
                (exposed > 0.0 || bypassed > 0).then_some(BridgeOption {
                    from: near,
                    to: far,
                    ends: cand.ends,
                    span: cand.span,
                    enemy_exposed: exposed,
                    guards_bypassed: bypassed,
                    score,
                })
            })
            .collect();
        options.sort_by(|a, b| b.score.total_cmp(&a.score));
        options
    }

    /// Typical asset value, so bypassed guards weigh like a share of it.
    fn values_scale(&self) -> f32 {
        let (sum, count) =
            self.values.iter().filter(|v| v.enemy > 0.0).fold((0.0, 0), |(s, c), v| (s + v.enemy, c + 1));
        if count == 0 { 1.0 } else { sum / count as f32 }
    }

    fn drop_options(&self, map: &TierMap) -> Vec<DropOption> {
        let Some(home) = self.ally_base else { return Vec::new() };
        let home_pos = map.regions[home].center;
        let mut drops: Vec<DropOption> = (0..map.regions.len())
            .filter(|&r| self.values[r].enemy > 0.0 && Some(r) != self.enemy_base)
            .map(|r| {
                let ground = self.dist_from_ally[r];
                let straight = home_pos.distance(map.regions[r].center).max(CELL);
                let guards = self.guards_vs_ally[r].len();
                let detour = if ground.is_finite() { (ground / straight).max(1.0) } else { 4.0 };
                DropOption {
                    region: r,
                    enemy_value: self.values[r].enemy,
                    ground_distance: ground.is_finite().then_some(ground),
                    guards,
                    score: self.values[r].enemy * detour * (1.0 + guards as f32),
                }
            })
            .collect();
        drops.sort_by(|a, b| b.score.total_cmp(&a.score));
        drops
    }

    fn teleport_options(&self, graph: &RegionGraph) -> Vec<TeleportOption> {
        let enemy_regions: Vec<usize> = (0..self.values.len()).filter(|&r| self.values[r].enemy > 0.0).collect();
        let mut options: Vec<TeleportOption> = (0..self.values.len())
            .filter(|&x| Some(x) != self.enemy_base && Some(x) != self.ally_base)
            .map(|x| {
                let from_exit = graph.distances(x, None);
                let gain = enemy_regions
                    .iter()
                    .map(|&r| {
                        let before = self.dist_from_ally[r];
                        let after = from_exit[r];
                        if !after.is_finite() {
                            0.0
                        } else if !before.is_finite() {
                            self.values[r].enemy
                        } else {
                            self.values[r].enemy * ((before - after).max(0.0) / before.max(1.0))
                        }
                    })
                    .sum();
                TeleportOption { region: x, gain }
            })
            .filter(|t| t.gain > 0.0)
            .collect();
        options.sort_by(|a, b| b.gain.total_cmp(&a.gain));
        options
    }
}

/// Ground travel distance between two positions, or `None` if no ground
/// route exists (see [`Paths`]).
pub fn ground_distance(map: &TierMap, a: Pos, b: Pos) -> Option<f32> {
    Paths::new(map, a, |_| false).distance_to(b)
}

/// Regions as nodes, chokepoints as weighted edges.
struct RegionGraph {
    /// Per region: (neighbour, choke, travel distance through the choke).
    adj: Vec<Vec<(usize, usize, f32)>>,
}

impl RegionGraph {
    fn new(map: &TierMap) -> Self {
        let mut adj = vec![Vec::new(); map.regions.len()];
        for c in &map.chokes {
            for (i, &a) in c.regions.iter().enumerate() {
                for &b in &c.regions[i + 1..] {
                    let w = map.regions[a].center.distance(c.pos) + c.pos.distance(map.regions[b].center);
                    adj[a].push((b, c.id, w));
                    adj[b].push((a, c.id, w));
                }
            }
        }
        RegionGraph { adj }
    }

    /// Dijkstra from `src`, optionally ignoring one chokepoint.
    fn distances(&self, src: usize, without: Option<usize>) -> Vec<f32> {
        let mut dist = vec![f32::INFINITY; self.adj.len()];
        let mut heap = BinaryHeap::new();
        dist[src] = 0.0;
        heap.push(HeapItem(0.0, src));
        while let Some(HeapItem(d, r)) = heap.pop() {
            if d > dist[r] {
                continue;
            }
            for &(n, c, w) in &self.adj[r] {
                if Some(c) == without {
                    continue;
                }
                let nd = d + w;
                if nd < dist[n] {
                    dist[n] = nd;
                    heap.push(HeapItem(nd, n));
                }
            }
        }
        dist
    }

    fn reachable(&self, src: usize, without: Option<usize>, extra: Option<(usize, usize)>) -> Vec<bool> {
        let mut seen = vec![false; self.adj.len()];
        let mut queue = VecDeque::from([src]);
        seen[src] = true;
        while let Some(r) = queue.pop_front() {
            let extra_n = extra.and_then(|(a, b)| {
                if r == a {
                    Some(b)
                } else if r == b {
                    Some(a)
                } else {
                    None
                }
            });
            let ns = self.adj[r].iter().filter(|&&(_, c, _)| Some(c) != without).map(|&(n, _, _)| n);
            for n in ns.chain(extra_n) {
                if !seen[n] {
                    seen[n] = true;
                    queue.push_back(n);
                }
            }
        }
        seen
    }

    /// For every region, the chokepoints all routes from `src` pass.
    fn guards(&self, src: usize) -> Vec<Vec<usize>> {
        self.guards_impl(src, None)
    }

    /// Like [`guards`](Self::guards), with an extra edge between two regions.
    fn guards_with_extra_edge(&self, src: usize, a: usize, b: usize) -> Vec<Vec<usize>> {
        self.guards_impl(src, Some((a, b)))
    }

    fn guards_impl(&self, src: usize, extra: Option<(usize, usize)>) -> Vec<Vec<usize>> {
        let base = self.reachable(src, None, extra);
        let mut guards = vec![Vec::new(); self.adj.len()];
        let mut chokes: Vec<usize> = self.adj.iter().flatten().map(|&(_, c, _)| c).collect();
        chokes.sort_unstable();
        chokes.dedup();
        for c in chokes {
            let cut = self.reachable(src, Some(c), extra);
            for r in 0..self.adj.len() {
                if base[r] && !cut[r] {
                    guards[r].push(c);
                }
            }
        }
        guards
    }
}

/// A pair of regions separated by a short impassable stretch.
struct BridgeCandidate {
    from: usize,
    to: usize,
    ends: [Pos; 2],
    span: f32,
}

/// Shortest obstacle crossings between distinct regions: a bounded
/// multi-source BFS from every region's boundary cells through obstacle
/// cells; the first passable cell of another region it meets gives a
/// candidate (shortest per region pair).
fn bridge_candidates(map: &TierMap) -> Vec<BridgeCandidate> {
    let (w, h) = (map.width, map.height);
    let max_steps = (MAX_BRIDGE_SPAN / CELL) as u32;
    // Per obstacle cell: (steps, origin passable cell).
    let mut origin = vec![(u32::MAX, usize::MAX); w * h];
    let mut queue = VecDeque::new();
    let passable = |c: usize| map.component[c] != NONE;
    let neighbours = |c: usize| {
        let (x, z) = (c % w, c / w);
        [(x > 0).then(|| c - 1), (x + 1 < w).then(|| c + 1), (z > 0).then(|| c - w), (z + 1 < h).then(|| c + w)]
            .into_iter()
            .flatten()
    };
    for c in 0..w * h {
        if passable(c) && map.region[c] != NONE && neighbours(c).any(|n| !passable(n)) {
            for n in neighbours(c).filter(|&n| !passable(n)) {
                if origin[n].0 == u32::MAX {
                    origin[n] = (1, c);
                    queue.push_back(n);
                }
            }
        }
    }
    let mut best: BTreeMap<(usize, usize), BridgeCandidate> = BTreeMap::new();
    let mut consider = |a_cell: usize, b_cell: usize, steps: u32| {
        let (ra, rb) = (map.region[a_cell], map.region[b_cell]);
        if ra == NONE || rb == NONE || ra == rb {
            return;
        }
        let (ra, rb) = (ra as usize, rb as usize);
        // Regions already joined by a chokepoint need no bridge.
        if map.neighbours(ra).any(|(_, r)| r == rb) {
            return;
        }
        let (pa, pb) = (map.cell_center(a_cell), map.cell_center(b_cell));
        let span = pa.distance(pb).max(steps as f32 * CELL);
        let key = (ra.min(rb), ra.max(rb));
        let better = best.get(&key).is_none_or(|b| span < b.span);
        if better {
            best.insert(key, BridgeCandidate { from: ra, to: rb, ends: [pa, pb], span });
        }
    };
    // Expand through obstacles; where two fronts from different regions
    // meet, or a front reaches another region's ground, record a crossing.
    while let Some(c) = queue.pop_front() {
        let (steps, from) = origin[c];
        for n in neighbours(c) {
            if passable(n) {
                if n != from {
                    consider(from, n, steps);
                }
            } else if origin[n].0 == u32::MAX {
                if steps < max_steps {
                    origin[n] = (steps + 1, from);
                    queue.push_back(n);
                }
            } else if origin[n].1 != from {
                consider(from, origin[n].1, steps + origin[n].0);
            }
        }
    }
    best.into_values().filter(|b| b.span <= MAX_BRIDGE_SPAN).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testmaps::{at, rooms};

    #[test]
    fn ground_distance_follows_chokepoints() {
        let map = rooms();
        let (a, b) = (at(2.0, 1.0), at(6.0, 1.0));
        assert_eq!(ground_distance(&map, a, b), Some(a.distance(b)));
        // Room a to room c: through both doorways, barely longer than the
        // straight line.
        let (a, c) = (at(2.0, 1.0), at(22.0, 1.0));
        let d = ground_distance(&map, a, c).unwrap();
        assert!(d >= a.distance(c) && d < a.distance(c) * 1.1, "{d}");
        // Strip d to strip e, split by a thin wall: all the way round
        // through the rooms.
        let (d_pos, e_pos) = (at(10.0, 6.0), at(15.0, 6.0));
        let d = ground_distance(&map, d_pos, e_pos).unwrap();
        assert!(d > 4.0 * d_pos.distance(e_pos), "{d}");
    }

    #[test]
    fn guards_walls_seal_bridges_drops() {
        let map = rooms();
        let assets = [
            Asset { pos: at(12.0, 1.0), value: 10.0, side: Side::Ally },
            Asset { pos: at(22.0, 1.0), value: 10.0, side: Side::Enemy },
            Asset { pos: at(20.0, 6.0), value: 6.0, side: Side::Enemy },
        ];
        let s = Scenario { map: &map, ally_base: at(2.0, 1.0), enemy_base: at(22.0, 0.0), assets: &assets };
        let a = Analysis::run(&s);
        let (ra, rb, rc, rd, re) = (0, 1, 2, 3, 4);
        let (ab, bc, ad, ce) = (0, 1, 2, 3);
        assert_eq!((a.ally_base, a.enemy_base), (Some(ra), Some(rc)));

        // Enemy routes from c: b only via bc; a via bc+ab; d via bc+ab+ad.
        assert_eq!(a.guards_vs_enemy[rb], vec![bc]);
        assert_eq!(a.guards_vs_enemy[ra], vec![ab, bc]);
        assert_eq!(a.guards_vs_enemy[rd], vec![ab, bc, ad]);
        assert_eq!(a.guards_vs_enemy[re], vec![ce]);

        // Walling bc protects our room b. It is not a seal option, though:
        // bc is on the enemy's doorstep.
        assert_eq!(a.walls[0].choke, bc);
        assert!(a.walls[0].regions.contains(&rb));
        assert!(a.seal.is_empty(), "seal: {:?}", a.seal);

        // With our value in a and d only, walling ab protects it all.
        let home_assets = [
            Asset { pos: at(4.0, 1.0), value: 10.0, side: Side::Ally },
            Asset { pos: at(5.0, 6.0), value: 10.0, side: Side::Ally },
            Asset { pos: at(22.0, 1.0), value: 10.0, side: Side::Enemy },
        ];
        let home = Analysis::run(&Scenario { assets: &home_assets, ..s });
        assert_eq!(home.seal, vec![ab]);
        // Holding ab with static defense pays as well; nothing on our side
        // of the map guards room b.
        assert_eq!(home.hold, vec![ab]);
        assert!(a.hold.is_empty(), "hold: {:?}", a.hold);

        // A bridge through the thin wall between d and e opens a second
        // route: it bypasses every guard on e (ab, bc, ce) and on their base
        // c (ab, bc), and shortens the way to e.
        let bridge = a.bridges.iter().find(|b| (b.from, b.to) == (rd, re)).expect("no d-e bridge");
        assert!(bridge.span <= 9.0 * CELL, "span {}", bridge.span);
        assert_eq!(bridge.guards_bypassed, 5);
        assert!(bridge.enemy_exposed > 0.0);

        // e is the only enemy-valued region besides their base: drop there.
        assert_eq!(a.drops.iter().map(|d| d.region).collect::<Vec<_>>(), vec![re]);
        // A teleporter exit next to their value beats walking there.
        assert!(a.teleports.first().is_some_and(|t| t.region == re || t.region == rb));
    }
}
