//! Lanes: how far a side's metal can be closed off from the enemy's ground
//! forces by holding narrow fronts, measured on the passable grid itself.
//!
//! The region decomposition is not symmetric (the same map analysed from
//! either start disagrees on which chokepoints to hold), so this works on
//! the 16-elmo cells directly, with vertex min-cuts: each passable cell is
//! split in two joined by its width, neighbours (8-connected) joined
//! without limit.
//!
//! - The **perimeter**: the cheapest fronts keeping the enemy's start from
//!   our spots, a front costing its width in elmos and a spot given up
//!   `lambda` elmos (about the front one turret holds). Fronts run only on
//!   our side of the map and only through passages at most `max_width`
//!   wide (a cell farther than half that from every obstacle cannot be
//!   cut): a broad waist across the map is army work, not a lane. Every
//!   front is part of one perimeter: our side is connected, so a front
//!   left open exposes everything behind the others.
//! - The **closure profile**: the narrowest full closure of the map near
//!   each depth (share of the way from our start to theirs by ground), over
//!   the front available at that depth. Lane maps close narrowly at several
//!   depths; open maps nowhere.
//!
//! [`Lanes::openness`] averages the two (0: closed off for free; 1: no lane
//! worth holding). The calibration behind the defaults is in
//! `zkgbai-runs/terrain/openness/NOTE.md`: Hide and Seek and Eye of Horus
//! read lane-like, Comet Catcher and Agalta open, from either start.

use std::collections::{BinaryHeap, HashSet, VecDeque};

use crate::{CELL, Pos, TierMap};

/// A spot's own ground, which a front cannot run through, elmos.
const SPOT_RADIUS: f32 = 100.0;
/// A start's base, which a front cannot run through, elmos.
const BASE_RADIUS: f32 = 400.0;
/// Spots at most this share of the way to the enemy's start (by ground)
/// are ours.
const OURS: f32 = 0.9;
/// Narrower runs of cut cells are slivers, not fronts, elmos.
const MIN_FRONT: f32 = 48.0;
/// Capacity units per cell (so half-cell prices stay integral).
const UNIT: i64 = 2;
const INF: i64 = i64::MAX / 4;

/// What [`analyse`] measures with.
#[derive(Clone, Debug)]
pub struct LaneParams {
    /// Price of a spot given up, elmos of front.
    pub lambda: f32,
    /// Widest passage a perimeter front may run through, elmos.
    pub max_width: f32,
    /// Depths of the closure profile.
    pub depths: Vec<f32>,
    /// Half-width of the depth band a closure may run in.
    pub band: f32,
    /// Front one defense site covers, elmos (sites per front: its span
    /// over this, at least one).
    pub cover: f32,
    /// How far behind a front (towards our start) its sites stand, elmos.
    pub setback: f32,
    /// Grid cells per side of a lane cell: 1 measures on the 16-elmo cells,
    /// 2 on 32-elmo ones (a quarter of the work and alike within a few
    /// hundredths, see the tests).
    pub coarse: usize,
}

impl LaneParams {
    /// The calibrated defaults for a tier: bots' lanes run up to 1200 wide
    /// (Eye of Horus), vehicles' up to 900.
    pub fn for_tier(tier: crate::Tier) -> Self {
        let max_width = match tier {
            crate::Tier::Vehicle | crate::Tier::Hover => 900.0,
            _ => 1200.0,
        };
        LaneParams {
            lambda: 600.0,
            max_width,
            depths: (2..=8).map(|i| i as f32 / 10.0).collect(),
            band: 0.05,
            cover: 700.0,
            setback: 120.0,
            coarse: 2,
        }
    }
}

/// A connected run of cut cells.
#[derive(Clone, Debug)]
pub struct Front {
    pub center: Pos,
    /// Elmos of cells cut (what the front costs).
    pub width: f32,
    /// Straight-line extent, elmos (what a line of defenses must cover).
    pub span: f32,
    /// Where defenses holding it stand, behind it on our side.
    pub sites: Vec<Pos>,
    /// Per site: the input spots whose shortest route from the enemy's
    /// start crosses this front nearest that site (where a raid on them
    /// most likely comes through).
    pub routes: Vec<Vec<usize>>,
    /// Its cut cells.
    cells: Vec<usize>,
}

/// The cheapest perimeter (see the module docs).
#[derive(Clone, Debug, Default)]
pub struct Perimeter {
    /// Front widths plus `lambda` per spot given up, elmos.
    pub cost: f32,
    /// `cost / (lambda x our spots)`.
    pub openness: f32,
    /// Per input spot: ours and kept behind the fronts.
    pub kept: Vec<bool>,
    pub fronts: Vec<Front>,
}

/// One depth of the closure profile.
#[derive(Clone, Copy, Debug)]
pub struct Closure {
    pub depth: f32,
    /// Narrowest full closure near this depth, elmos (`None`: none).
    pub width: Option<f32>,
    /// The front available at this depth, elmos.
    pub available: f32,
}

impl Closure {
    pub fn ratio(&self) -> Option<f32> {
        self.width.filter(|_| self.available > 0.0).map(|w| (w / self.available).min(1.0))
    }
}

/// A map's lanes for one tier, from our start.
#[derive(Clone, Debug)]
pub struct Lanes {
    pub tier: crate::Tier,
    /// Per input spot: ours (nearer our start, see [`OURS`]).
    pub ours: Vec<bool>,
    pub perimeter: Perimeter,
    pub profile: Vec<Closure>,
    /// Mean closure ratio over the profile (1 where it is empty).
    pub closure: f32,
    /// See the module docs.
    pub openness: f32,
    /// Usable (passable) area, square kilo-elmos.
    pub usable_km2: f32,
    /// Per input spot: ground distance from the enemy's start, elmos
    /// (`INFINITY` where it cannot be reached): how exposed a spot is to
    /// raids on a maze map, where a corner near as the crow flies can be far
    /// by any route.
    pub enemy_ground: Vec<f32>,
}

/// Measure the lanes of `map` between our start and theirs; `spots` are
/// every metal spot. `None` if either start is off the tier's ground.
pub fn analyse(map: &TierMap, ours: Pos, theirs: Pos, spots: &[Pos], p: &LaneParams) -> Option<Lanes> {
    let g = LaneGrid::new(map, p.coarse);
    let oc = g.cell_near(ours)?;
    let ec = g.cell_near(theirs)?;
    let d_o = g.distances(oc);
    let d_e = g.distances(ec);
    let cells: Vec<Option<usize>> = spots.iter().map(|&s| g.cell_near(s)).collect();
    let ours_flags: Vec<bool> =
        (cells.iter()).map(|c| c.is_some_and(|c| d_o[c].is_finite() && d_o[c] <= OURS * d_e[c])).collect();
    let mine: Vec<usize> = (cells.iter().zip(&ours_flags)).filter(|(_, o)| **o).filter_map(|(c, _)| *c).collect();
    let mut perimeter = g.perimeter(ec, &mine, p, &d_o, &d_e);
    // Back from our spots to the input's.
    let mut kept = Vec::with_capacity(spots.len());
    let mut it = perimeter.kept.iter();
    for &o in &ours_flags {
        kept.push(o && it.next().copied().unwrap_or(false));
    }
    perimeter.kept = kept;
    g.attribute_routes(&mut perimeter.fronts, &cells, &d_e);
    let profile = g.profile(ec, oc, &d_o, &d_e, &p.depths, p.band);
    let ratios: Vec<f32> = profile.iter().map(|c| c.ratio().unwrap_or(1.0)).collect();
    let closure = if ratios.is_empty() { 1.0 } else { ratios.iter().sum::<f32>() / ratios.len() as f32 };
    let lane = if mine.is_empty() { 1.0 } else { perimeter.openness.min(1.0) };
    let usable = g.pass.iter().filter(|&&p| p).count() as f32 * g.cell * g.cell / 1e6;
    let enemy_ground: Vec<f32> = cells.iter().map(|c| c.map_or(f32::INFINITY, |c| d_e[c])).collect();
    Some(Lanes {
        tier: map.tier,
        enemy_ground,
        ours: ours_flags,
        perimeter,
        profile,
        closure,
        openness: 0.5 * (lane + closure),
        usable_km2: usable,
    })
}

/// The passable grid of a tier map, with each cell's clearance.
pub struct LaneGrid {
    /// A lane cell's size, elmos.
    cell: f32,
    w: usize,
    h: usize,
    pass: Vec<bool>,
    /// Ground distance (elmos) to the nearest impassable cell or map edge.
    clearance: Vec<f32>,
}

impl LaneGrid {
    /// The grid of `map` in lane cells of `k` x `k` map cells, each
    /// passable where at least three quarters of its map cells are (thin
    /// cliffs stay walls).
    pub fn new(map: &TierMap, k: usize) -> Self {
        let k = k.max(1);
        let (w, h) = (map.width.div_ceil(k), map.height.div_ceil(k));
        let pass: Vec<bool> = (0..w * h)
            .map(|c| {
                let (x, z) = (c % w * k, c / w * k);
                let mut open = 0;
                let mut all = 0;
                for sz in z..(z + k).min(map.height) {
                    for sx in x..(x + k).min(map.width) {
                        all += 1;
                        open += usize::from(map.is_passable(map.cell_center(sz * map.width + sx)));
                    }
                }
                4 * open >= 3 * all
            })
            .collect();
        let mut g = LaneGrid { cell: CELL * k as f32, w, h, pass, clearance: Vec::new() };
        g.clearance = g.clearance_field();
        g
    }

    /// World position of a lane cell's center.
    fn center(&self, c: usize) -> Pos {
        Pos::new(((c % self.w) as f32 + 0.5) * self.cell, ((c / self.w) as f32 + 0.5) * self.cell)
    }

    /// Lane cell containing a world position, clamped to the map.
    fn at(&self, p: Pos) -> usize {
        let x = ((p.x / self.cell).max(0.0) as usize).min(self.w - 1);
        let z = ((p.z / self.cell).max(0.0) as usize).min(self.h - 1);
        z * self.w + x
    }

    fn clearance_field(&self) -> Vec<f32> {
        let n = self.w * self.h;
        let mut d = vec![f32::INFINITY; n];
        let mut heap = BinaryHeap::new();
        for (c, dc) in d.iter_mut().enumerate() {
            let (x, z) = (c % self.w, c / self.w);
            let edge = x == 0 || z == 0 || x == self.w - 1 || z == self.h - 1;
            if !self.pass[c] {
                *dc = 0.0;
                heap.push(Node(0.0, c));
            } else if edge {
                *dc = self.cell / 2.0;
                heap.push(Node(self.cell / 2.0, c));
            }
        }
        while let Some(Node(du, u)) = heap.pop() {
            if du > d[u] {
                continue;
            }
            for (v, l) in self.around(u) {
                if du + l < d[v] {
                    d[v] = du + l;
                    heap.push(Node(du + l, v));
                }
            }
        }
        d
    }

    /// All 8 neighbours (passable or not) with the step length.
    fn around(&self, c: usize) -> impl Iterator<Item = (usize, f32)> + '_ {
        let (x, z) = ((c % self.w) as i64, (c / self.w) as i64);
        STEPS.iter().filter_map(move |&(dx, dz, l)| {
            let (nx, nz) = (x + dx, z + dz);
            (nx >= 0 && nz >= 0 && nx < self.w as i64 && nz < self.h as i64)
                .then(|| (nz as usize * self.w + nx as usize, l * self.cell))
        })
    }

    /// Passable neighbours, never squeezing diagonally between two
    /// blocked cells.
    fn neighbours(&self, c: usize) -> impl Iterator<Item = (usize, f32)> + '_ {
        let (x, z) = ((c % self.w) as i64, (c / self.w) as i64);
        STEPS.iter().filter_map(move |&(dx, dz, l)| {
            let (nx, nz) = (x + dx, z + dz);
            if nx < 0 || nz < 0 || nx >= self.w as i64 || nz >= self.h as i64 {
                return None;
            }
            let n = nz as usize * self.w + nx as usize;
            if dx != 0 && dz != 0 {
                let a = z as usize * self.w + nx as usize;
                let b = nz as usize * self.w + x as usize;
                if !self.pass[a] && !self.pass[b] {
                    return None;
                }
            }
            self.pass[n].then_some((n, l * self.cell))
        })
    }

    /// Nearest passable cell to `p`, within 20 lane cells.
    pub fn cell_near(&self, p: Pos) -> Option<usize> {
        let c = self.at(p);
        if self.pass[c] {
            return Some(c);
        }
        let (x, z) = ((c % self.w) as i64, (c / self.w) as i64);
        for r in 1..=20i64 {
            let mut best: Option<(i64, usize)> = None;
            for dz in -r..=r {
                for dx in -r..=r {
                    if dx.abs() != r && dz.abs() != r {
                        continue;
                    }
                    let (nx, nz) = (x + dx, z + dz);
                    if nx < 0 || nz < 0 || nx >= self.w as i64 || nz >= self.h as i64 {
                        continue;
                    }
                    let n = nz as usize * self.w + nx as usize;
                    let d = dx * dx + dz * dz;
                    if self.pass[n] && best.is_none_or(|(bd, _)| d < bd) {
                        best = Some((d, n));
                    }
                }
            }
            if let Some((_, n)) = best {
                return Some(n);
            }
        }
        None
    }

    /// Ground distance (elmos) from cell `from` to every cell.
    pub fn distances(&self, from: usize) -> Vec<f32> {
        let mut d = vec![f32::INFINITY; self.w * self.h];
        let mut heap = BinaryHeap::new();
        d[from] = 0.0;
        heap.push(Node(0.0, from));
        while let Some(Node(du, u)) = heap.pop() {
            if du > d[u] {
                continue;
            }
            for (v, l) in self.neighbours(u) {
                if du + l < d[v] {
                    d[v] = du + l;
                    heap.push(Node(du + l, v));
                }
            }
        }
        d
    }

    /// The cheapest perimeter keeping the enemy (from cell `enemy`) off
    /// `spots` (cells of ours), on our side and through narrow passages.
    pub fn perimeter(&self, enemy: usize, spots: &[usize], p: &LaneParams, d_o: &[f32], d_e: &[f32]) -> Perimeter {
        let n = self.w * self.h;
        let (s, t) = (2 * n, 2 * n + 1);
        let mut net = Dinic::new(2 * n + 2);
        let ep = self.center(enemy);
        let spot_pos: Vec<Pos> = spots.iter().map(|&c| self.center(c)).collect();
        for c in 0..n {
            if !self.pass[c] {
                continue;
            }
            let at = self.center(c);
            let fixed = at.distance(ep) <= BASE_RADIUS
                || d_o[c] > d_e[c]
                || self.clearance[c] > p.max_width / 2.0
                || spot_pos.iter().any(|q| q.distance(at) <= SPOT_RADIUS);
            net.add(2 * c, 2 * c + 1, if fixed { INF } else { UNIT });
            for (v, _) in self.neighbours(c) {
                net.add(2 * c + 1, 2 * v, INF);
            }
        }
        net.add(s, 2 * enemy + 1, INF);
        let price = ((p.lambda / self.cell) * UNIT as f32).round() as i64;
        for &c in spots {
            net.add(2 * c + 1, t, price);
        }
        let flow = net.max_flow(s, t);
        let side = net.source_side(s);
        let cut: Vec<usize> = (0..n).filter(|&c| self.pass[c] && side[2 * c] && !side[2 * c + 1]).collect();
        let kept: Vec<bool> = spots.iter().map(|&c| !side[2 * c + 1]).collect();
        let cost = flow as f32 / UNIT as f32 * self.cell;
        let fronts = (self.components(&cut).into_iter())
            .filter(|f| f.len() as f32 * self.cell >= MIN_FRONT)
            .map(|f| self.front(&f, d_o, p.cover, p.setback))
            .collect();
        Perimeter { cost, openness: cost / (p.lambda * spots.len().max(1) as f32), kept, fronts }
    }

    /// Fill each front's `routes`: walk every spot's shortest route back
    /// towards the enemy's start (down the distance field `d_e`); the
    /// first front cell it meets is where a raid on the spot comes
    /// through, and the site nearest that cell holds it.
    fn attribute_routes(&self, fronts: &mut [Front], cells: &[Option<usize>], d_e: &[f32]) {
        let mut front_of: std::collections::HashMap<usize, usize> = std::collections::HashMap::new();
        for (i, f) in fronts.iter().enumerate() {
            for &c in &f.cells {
                front_of.insert(c, i);
            }
        }
        for (spot, cell) in cells.iter().enumerate() {
            let Some(mut c) = *cell else { continue };
            if !d_e[c].is_finite() {
                continue;
            }
            for _ in 0..self.w * self.h {
                if let Some(&i) = front_of.get(&c) {
                    let at = self.center(c);
                    let f = &mut fronts[i];
                    let near =
                        (0..f.sites.len()).min_by(|&a, &b| f.sites[a].distance(at).total_cmp(&f.sites[b].distance(at)));
                    if let Some(site) = near {
                        f.routes[site].push(spot);
                    }
                    break;
                }
                let next =
                    self.neighbours(c).filter(|&(v, _)| d_e[v] < d_e[c]).min_by(|a, b| d_e[a.0].total_cmp(&d_e[b.0]));
                match next {
                    Some((v, _)) => c = v,
                    None => break,
                }
            }
        }
    }

    /// The narrowest full closure near each depth (see the module docs).
    /// Only the band's cells are in each cut's graph: ground nearer us
    /// than the band is the sink, ground farther the source.
    pub fn profile(
        &self,
        enemy: usize,
        ours: usize,
        d_o: &[f32],
        d_e: &[f32],
        depths: &[f32],
        band: f32,
    ) -> Vec<Closure> {
        let n = self.w * self.h;
        let rel: Vec<f32> =
            (0..n)
                .map(|c| {
                    if d_o[c].is_finite() && d_e[c].is_finite() {
                        d_o[c] / (d_o[c] + d_e[c]).max(1.0)
                    } else {
                        f32::NAN
                    }
                })
                .collect();
        let thick = 2.0 * band * d_o[enemy].max(1.0);
        let _ = ours;
        depths
            .iter()
            .map(|&d| {
                let cells: Vec<usize> =
                    (0..n).filter(|&c| self.pass[c] && rel[c].is_finite() && (rel[c] - d).abs() <= band).collect();
                let mut id = vec![u32::MAX; n];
                for (i, &c) in cells.iter().enumerate() {
                    id[c] = i as u32;
                }
                let m = cells.len();
                let (s, t) = (2 * m, 2 * m + 1);
                let mut net = Dinic::new(2 * m + 2);
                for (i, &c) in cells.iter().enumerate() {
                    net.add(2 * i, 2 * i + 1, UNIT);
                    let (mut near, mut far) = (false, false);
                    for (v, _) in self.neighbours(c) {
                        if id[v] != u32::MAX {
                            net.add(2 * i + 1, 2 * id[v] as usize, INF);
                        } else if rel[v] < d - band {
                            near = true;
                        } else if rel[v] > d + band {
                            far = true;
                        }
                    }
                    if near {
                        net.add(2 * i + 1, t, INF);
                    }
                    if far {
                        net.add(s, 2 * i, INF);
                    }
                }
                let flow = net.max_flow(s, t);
                let width = (flow < INF).then(|| flow as f32 / UNIT as f32 * self.cell);
                Closure { depth: d, width, available: m as f32 * self.cell * self.cell / thick }
            })
            .collect()
    }

    /// A front from its cells, with its defense sites: the cells split
    /// along the front's long axis into runs of about `cover`, each run's
    /// middle moved `setback` towards our start.
    fn front(&self, cells: &[usize], d_o: &[f32], cover: f32, setback: f32) -> Front {
        let pos: Vec<Pos> = cells.iter().map(|&c| self.center(c)).collect();
        let k = pos.len() as f32;
        let center = Pos::new(pos.iter().map(|p| p.x).sum::<f32>() / k, pos.iter().map(|p| p.z).sum::<f32>() / k);
        let span = span(&pos, self.cell);
        // Long axis: the principal direction of the cells.
        let (mut sxx, mut sxz, mut szz) = (0.0, 0.0, 0.0);
        for p in &pos {
            let (dx, dz) = (p.x - center.x, p.z - center.z);
            sxx += dx * dx;
            sxz += dx * dz;
            szz += dz * dz;
        }
        let angle = 0.5 * (2.0 * sxz).atan2(sxx - szz);
        let (ax, az) = (angle.cos(), angle.sin());
        let mut order: Vec<usize> = (0..cells.len()).collect();
        order.sort_by(|&a, &b| {
            let pa = (pos[a].x - center.x) * ax + (pos[a].z - center.z) * az;
            let pb = (pos[b].x - center.x) * ax + (pos[b].z - center.z) * az;
            pa.total_cmp(&pb)
        });
        let runs = ((span / cover.max(self.cell)).ceil() as usize).max(1);
        let sites: Vec<Pos> = (0..runs)
            .filter_map(|i| {
                let lo = i * order.len() / runs;
                let hi = ((i + 1) * order.len() / runs).max(lo + 1).min(order.len());
                let mid = cells[order[(lo + hi) / 2]];
                self.behind(mid, d_o, setback)
            })
            .collect();
        let routes = vec![Vec::new(); sites.len()];
        Front { center, width: k * self.cell, span, sites, routes, cells: cells.to_vec() }
    }

    /// A passable cell about `setback` nearer our start than `cell`, close
    /// to it.
    fn behind(&self, cell: usize, d_o: &[f32], setback: f32) -> Option<Pos> {
        let want = d_o[cell] - setback;
        let from = self.center(cell);
        let r = (setback * 1.5 / self.cell).ceil() as i64;
        let (x, z) = ((cell % self.w) as i64, (cell / self.w) as i64);
        let mut best: Option<(f32, usize)> = None;
        for dz in -r..=r {
            for dx in -r..=r {
                let (nx, nz) = (x + dx, z + dz);
                if nx < 0 || nz < 0 || nx >= self.w as i64 || nz >= self.h as i64 {
                    continue;
                }
                let c = nz as usize * self.w + nx as usize;
                if !self.pass[c] || !d_o[c].is_finite() {
                    continue;
                }
                let score = (d_o[c] - want).abs() + 0.3 * self.center(c).distance(from);
                if best.is_none_or(|(b, _)| score < b) {
                    best = Some((score, c));
                }
            }
        }
        best.map(|(_, c)| self.center(c))
    }

    /// 8-connected components of `cells`, largest first.
    fn components(&self, cells: &[usize]) -> Vec<Vec<usize>> {
        let set: HashSet<usize> = cells.iter().copied().collect();
        let mut seen = HashSet::new();
        let mut out = Vec::new();
        for &c in cells {
            if !seen.insert(c) {
                continue;
            }
            let mut comp = vec![c];
            let mut queue = VecDeque::from([c]);
            while let Some(u) = queue.pop_front() {
                for (v, _) in self.around(u) {
                    if set.contains(&v) && seen.insert(v) {
                        comp.push(v);
                        queue.push_back(v);
                    }
                }
            }
            out.push(comp);
        }
        out.sort_by_key(|c| std::cmp::Reverse(c.len()));
        out
    }
}

const STEPS: [(i64, i64, f32); 8] = [
    (1, 0, 1.0),
    (-1, 0, 1.0),
    (0, 1, 1.0),
    (0, -1, 1.0),
    (1, 1, std::f32::consts::SQRT_2),
    (1, -1, std::f32::consts::SQRT_2),
    (-1, 1, std::f32::consts::SQRT_2),
    (-1, -1, std::f32::consts::SQRT_2),
];

/// Straight-line extent of a set of points (double sweep), plus a cell.
fn span(pos: &[Pos], cell: f32) -> f32 {
    let Some(&first) = pos.first() else { return 0.0 };
    let far =
        |from: Pos| pos.iter().copied().max_by(|a, b| a.distance(from).total_cmp(&b.distance(from))).unwrap_or(from);
    let a = far(first);
    a.distance(far(a)) + cell
}

#[derive(PartialEq)]
struct Node(f32, usize);
impl Eq for Node {}
impl PartialOrd for Node {
    fn partial_cmp(&self, o: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(o))
    }
}
impl Ord for Node {
    fn cmp(&self, o: &Self) -> std::cmp::Ordering {
        o.0.total_cmp(&self.0)
    }
}

/// Dinic's max flow with integer capacities.
struct Dinic {
    head: Vec<u32>,
    next: Vec<u32>,
    to: Vec<u32>,
    cap: Vec<i64>,
}

const NIL: u32 = u32::MAX;

impl Dinic {
    fn new(n: usize) -> Self {
        Dinic { head: vec![NIL; n], next: Vec::new(), to: Vec::new(), cap: Vec::new() }
    }

    fn add(&mut self, a: usize, b: usize, cap: i64) {
        for (u, v, c) in [(a, b, cap), (b, a, 0)] {
            self.to.push(v as u32);
            self.cap.push(c);
            self.next.push(self.head[u]);
            self.head[u] = self.to.len() as u32 - 1;
        }
    }

    fn levels(&self, s: usize, t: usize) -> Option<Vec<i32>> {
        let mut level = vec![-1; self.head.len()];
        level[s] = 0;
        let mut queue = VecDeque::from([s]);
        while let Some(u) = queue.pop_front() {
            let mut e = self.head[u];
            while e != NIL {
                let v = self.to[e as usize] as usize;
                if self.cap[e as usize] > 0 && level[v] < 0 {
                    level[v] = level[u] + 1;
                    queue.push_back(v);
                }
                e = self.next[e as usize];
            }
        }
        (level[t] >= 0).then_some(level)
    }

    fn max_flow(&mut self, s: usize, t: usize) -> i64 {
        let mut total = 0;
        while let Some(level) = self.levels(s, t) {
            let mut it = self.head.clone();
            loop {
                // One augmenting path in the level graph, by iterative DFS.
                let mut path: Vec<u32> = Vec::new();
                let mut u = s;
                let found = loop {
                    if u == t {
                        break true;
                    }
                    let mut advanced = false;
                    while it[u] != NIL {
                        let e = it[u] as usize;
                        let v = self.to[e] as usize;
                        if self.cap[e] > 0 && level[v] == level[u] + 1 {
                            path.push(e as u32);
                            u = v;
                            advanced = true;
                            break;
                        }
                        it[u] = self.next[e];
                    }
                    if advanced {
                        continue;
                    }
                    let Some(e) = path.pop() else { break false };
                    let prev = self.to[(e ^ 1) as usize] as usize;
                    it[prev] = self.next[e as usize];
                    u = prev;
                };
                if !found {
                    break;
                }
                let push = path.iter().map(|&e| self.cap[e as usize]).min().unwrap_or(0);
                if push >= INF {
                    return INF;
                }
                for &e in &path {
                    self.cap[e as usize] -= push;
                    self.cap[(e ^ 1) as usize] += push;
                }
                total += push;
            }
        }
        total
    }

    fn source_side(&self, s: usize) -> Vec<bool> {
        let mut seen = vec![false; self.head.len()];
        seen[s] = true;
        let mut queue = VecDeque::from([s]);
        while let Some(u) = queue.pop_front() {
            let mut e = self.head[u];
            while e != NIL {
                let v = self.to[e as usize] as usize;
                if self.cap[e as usize] > 0 && !seen[v] {
                    seen[v] = true;
                    queue.push_back(v);
                }
                e = self.next[e as usize];
            }
        }
        seen
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testmaps::{at, ring, rooms};

    /// Three rooms in a row: the enemy in c, our spots in a; the door a-b
    /// is narrow and on our side, so the perimeter is that door.
    #[test]
    fn rooms_close_at_the_door() {
        let map = rooms();
        let ours = at(2.0, 1.0);
        let theirs = at(22.0, 1.0);
        let spots = [at(1.0, 0.0), at(5.0, 2.0), at(20.0, 0.0)];
        let p = LaneParams::for_tier(crate::Tier::Bot);
        let l = analyse(&map, ours, theirs, &spots, &p).expect("both starts on the ground");
        assert_eq!(l.ours, vec![true, true, false]);
        assert!(l.perimeter.kept[0] && l.perimeter.kept[1] && !l.perimeter.kept[2]);
        assert!(!l.perimeter.fronts.is_empty());
        assert!(l.perimeter.openness < 0.5, "one narrow door keeps two spots: {}", l.perimeter.openness);
        // Our two spots' routes come through the door.
        let routed: Vec<usize> = l.perimeter.fronts.iter().flat_map(|f| f.routes.iter().flatten().copied()).collect();
        assert!(routed.contains(&0) && routed.contains(&1), "{routed:?}");
        // Every front has a site, on our side of the map.
        for f in &l.perimeter.fronts {
            assert!(!f.sites.is_empty());
            for s in &f.sites {
                assert!(s.distance(ours) < s.distance(theirs));
            }
        }
    }

    /// The same layout analysed from either start agrees.
    #[test]
    fn symmetric_from_either_start() {
        let map = ring();
        let (a, b) = (at(2.0, 1.0), at(22.0, 1.0));
        let spots = [at(1.0, 0.0), at(3.0, 2.0), at(21.0, 0.0), at(23.0, 2.0)];
        let p = LaneParams::for_tier(crate::Tier::Bot);
        let x = analyse(&map, a, b, &spots, &p).unwrap();
        let y = analyse(&map, b, a, &spots, &p).unwrap();
        assert!((x.openness - y.openness).abs() < 0.1, "{} vs {}", x.openness, y.openness);
    }
}
