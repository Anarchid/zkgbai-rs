//! Conquest objectives: forward ground that a few turrets can hold.
//!
//! On small maps the middle often holds rich metal on the crossroads the
//! enemy's ground army has to pass (Wanderlust: three 3-metal spots on the
//! diagonal, where humans walk the commander after the first mexes and put
//! up two Stingers). An objective is a few turret sites and the neutral
//! spots near them. Its worth is measured per enemy and per movement tier
//! (user, 2026-10-31): with the turrets' reach closed to raiders, how much
//! longer the enemy's way gets to within raiding reach of each spot of our
//! metal, `1 - before / after` (cut off: 1), times the spot's income. The
//! objective's own spots count too: a spot the enemy cannot get at past the
//! turrets on its tier counts in full, one whose approach only gets longer
//! counts in part. Spiders that walk round over the cliffs get a smaller
//! number than vehicles held to the roads; summed over enemies, three
//! vehicle players of four keep three quarters of it.
//!
//! The objectives are computed once per map and start; the AI weighs each
//! tier's number with what it knows of each enemy's tier when it has to
//! commit the turrets.

use std::cmp::Reverse;
use std::collections::BinaryHeap;

use crate::strategy::{Asset, Side};
use crate::{CELL, Pos, Tier, TierMap};

/// How objectives are looked for.
#[derive(Clone, Copy, Debug)]
pub struct Params {
    /// Reach of the turret, elmos (Stinger 620).
    pub range: f32,
    /// Raiders keep out of a turret's reach less this, elmos.
    pub disc_margin: f32,
    /// A neutral spot this near a site belongs to the objective, elmos.
    pub spot_cover: f32,
    /// Raiders kill a mex from this far, elmos.
    pub raid_reach: f32,
    /// Turret sites an objective has at most.
    pub max_sites: usize,
    /// Contested ground: a site's share of the way from our start, our
    /// distance over ours plus the nearest enemy's, lies in this band.
    pub band: [f32; 2],
    /// Seeds valued in full (best by a cheap first look).
    pub shortlist: usize,
    /// Sites of one objective stand at most this far from the first, elmos:
    /// one place one commander fortifies, not both ends of the middle.
    pub cluster: f32,
    /// Also seed a lattice over the contested band about this many elmos
    /// apart, laid out the same from every side of the map (0: seeds come
    /// from the terrain model's chokepoints and spots only, which need not
    /// be symmetric where the map is).
    pub lattice: f32,
    /// Objectives kept per design tier, each further one clear of the
    /// earlier ones' sites by `range` (1: the best only).
    pub per_tier: usize,
}

impl Default for Params {
    fn default() -> Self {
        Params {
            range: 620.0,
            disc_margin: 40.0,
            spot_cover: 470.0,
            raid_reach: 200.0,
            max_sites: 2,
            band: [0.33, 0.67],
            shortlist: 48,
            cluster: 1000.0,
            lattice: 0.0,
            per_tier: 1,
        }
    }
}

/// What an objective's turrets do to one enemy moving on one tier.
#[derive(Clone, Debug)]
pub struct Denial {
    pub tier: Tier,
    /// Index into the enemies the objectives were found for.
    pub enemy: usize,
    /// Our income (with the objective's spots) weighted by how much longer
    /// the enemy's way to it gets, metal/s.
    pub worth: f32,
    /// Of that, the objective's own spots.
    pub own: f32,
    /// The objective's spots the enemy cannot get at past the turrets.
    pub sealed: usize,
}

/// A forward position: turret sites and the spots they take.
#[derive(Clone, Debug)]
pub struct Objective {
    pub sites: Vec<Pos>,
    /// Indices into the assets: the neutral spots near the sites.
    pub spots: Vec<usize>,
    /// Their income, metal/s.
    pub income: f32,
    pub denial: Vec<Denial>,
    /// The tier the sites were chosen against.
    pub design: Tier,
    /// Ground distance (on the walking tier) from our start to the nearest
    /// site, and from the nearest enemy start.
    pub walk: f32,
    pub enemy_walk: f32,
    /// Denial on the design tier, summed over enemies: the search's score.
    pub score: f32,
}

impl Objective {
    /// Denial against `enemy` on `tier` (0 for tiers not analysed).
    pub fn denial_for(&self, tier: Tier, enemy: usize) -> f32 {
        self.denial.iter().find(|d| d.tier == tier && d.enemy == enemy).map_or(0.0, |d| d.worth)
    }

    /// The least denial over the tiers analysed: what holds whatever the
    /// enemy fields.
    pub fn floor(&self, enemy: usize) -> f32 {
        self.denial.iter().filter(|d| d.enemy == enemy).map(|d| d.worth).fold(f32::INFINITY, f32::min).max(0.0)
    }

    /// The point in the middle of the sites.
    pub fn center(&self) -> Pos {
        let n = self.sites.len().max(1) as f32;
        let (x, z) = self.sites.iter().fold((0.0, 0.0), |(x, z), s| (x + s.x, z + s.z));
        Pos::new(x / n, z / n)
    }
}

/// Points across a chokepoint a turret must reach to cover it: the gate's
/// ends and middle (a border's ends and crossing).
pub fn front(map: &TierMap, choke: usize) -> Vec<Pos> {
    let c = &map.chokes[choke];
    match c.gate {
        Some([a, b]) => vec![a, c.pos, b],
        None => vec![c.pos],
    }
}

/// Chokepoints of `map` whose whole front lies within `range` of some site
/// (for the record: the measure itself is on the grid).
pub fn covered(map: &TierMap, sites: &[Pos], range: f32) -> Vec<usize> {
    (0..map.chokes.len())
        .filter(|&c| front(map, c).iter().all(|p| sites.iter().any(|s| s.distance(*p) <= range)))
        .collect()
}

/// Elmos per grid cell (two of the model's cells).
pub const GRID: f32 = 32.0;
/// Step costs, straight and diagonal, per [`GRID`] elmos: 10 and 14.
const STRAIGHT: u32 = 10;
const DIAGONAL: u32 = 14;
const UNREACHED: u32 = u32::MAX;

/// A tier's passable ground at [`GRID`] elmos.
pub struct Grid {
    pub tier: Tier,
    w: usize,
    h: usize,
    pass: Vec<bool>,
}

impl Grid {
    pub fn new(map: &TierMap) -> Grid {
        let (w, h) = (map.width.div_ceil(2), map.height.div_ceil(2));
        let pass = (0..w * h).map(|c| map.is_passable(Self::center_of(w, c))).collect();
        Grid { tier: map.tier, w, h, pass }
    }

    fn center_of(w: usize, c: usize) -> Pos {
        Pos::new(((c % w) as f32 + 0.5) * GRID, ((c / w) as f32 + 0.5) * GRID)
    }

    fn cell(&self, p: Pos) -> usize {
        let x = ((p.x / GRID) as isize).clamp(0, self.w as isize - 1) as usize;
        let z = ((p.z / GRID) as isize).clamp(0, self.h as isize - 1) as usize;
        z * self.w + x
    }

    /// Passable cells within `r` elmos of `p`.
    fn around(&self, p: Pos, r: f32) -> Vec<usize> {
        let c = self.cell(p);
        let (cx, cz) = ((c % self.w) as isize, (c / self.w) as isize);
        let k = (r / GRID).ceil() as isize;
        let mut out = Vec::new();
        for dz in -k..=k {
            for dx in -k..=k {
                let (x, z) = (cx + dx, cz + dz);
                if x < 0 || z < 0 || x >= self.w as isize || z >= self.h as isize {
                    continue;
                }
                let i = z as usize * self.w + x as usize;
                if self.pass[i] && Self::center_of(self.w, i).distance(p) <= r {
                    out.push(i);
                }
            }
        }
        out
    }

    /// Cells within `r` of any of `sites`, as a mask.
    fn discs(&self, sites: &[Pos], r: f32) -> Vec<bool> {
        let mut m = vec![false; self.w * self.h];
        for &s in sites {
            for i in self.around(s, r) {
                m[i] = true;
            }
        }
        m
    }

    /// Route costs (10 per [`GRID`] elmos) from `origin` over passable
    /// cells not `blocked`.
    fn costs(&self, origin: Pos, blocked: Option<&[bool]>) -> Vec<u32> {
        let mut d = vec![UNREACHED; self.w * self.h];
        let open = |i: usize| self.pass[i] && !blocked.is_some_and(|b| b[i]);
        let Some(start) = self.around(origin, 6.0 * GRID).into_iter().filter(|&i| open(i)).min_by(|&a, &b| {
            Self::center_of(self.w, a).distance(origin).total_cmp(&Self::center_of(self.w, b).distance(origin))
        }) else {
            return d;
        };
        let mut heap = BinaryHeap::new();
        d[start] = 0;
        heap.push(Reverse((0u32, start)));
        while let Some(Reverse((g, c))) = heap.pop() {
            if g > d[c] {
                continue;
            }
            let (x, z) = ((c % self.w) as isize, (c / self.w) as isize);
            for (dx, dz) in [(-1, 0), (1, 0), (0, -1), (0, 1), (-1, -1), (1, -1), (-1, 1), (1, 1)] {
                let (nx, nz) = (x + dx, z + dz);
                if nx < 0 || nz < 0 || nx >= self.w as isize || nz >= self.h as isize {
                    continue;
                }
                let n = nz as usize * self.w + nx as usize;
                if !open(n) {
                    continue;
                }
                // No corner cutting past an obstacle.
                if dx != 0
                    && dz != 0
                    && (!open(z as usize * self.w + nx as usize) || !open(nz as usize * self.w + x as usize))
                {
                    continue;
                }
                let step = if dx != 0 && dz != 0 { DIAGONAL } else { STRAIGHT };
                let ng = g + step;
                if ng < d[n] {
                    d[n] = ng;
                    heap.push(Reverse((ng, n)));
                }
            }
        }
        d
    }
}

/// Cost to come within reach of a spot: the least over its cells.
fn reach_cost(costs: &[u32], cells: &[usize]) -> u32 {
    cells.iter().map(|&i| costs[i]).min().unwrap_or(UNREACHED)
}

/// One tier's grid with what does not change between site sets: each
/// enemy's routes with no turrets, and each spot's raiding cells.
struct TierCase<'a> {
    grid: Grid,
    map: &'a TierMap,
    enemies: Vec<Pos>,
    before: Vec<Vec<u32>>,
    cells: Vec<Vec<usize>>,
}

impl<'a> TierCase<'a> {
    fn new(map: &'a TierMap, enemies: &[Pos], assets: &[Asset], p: &Params) -> Self {
        let grid = Grid::new(map);
        let before = enemies.iter().map(|&e| grid.costs(e, None)).collect();
        let cells = assets.iter().map(|a| grid.around(a.pos, p.raid_reach)).collect();
        TierCase { grid, map, enemies: enemies.to_vec(), before, cells }
    }

    /// Denial of `sites` against enemy `k`: (all, objective's own, sealed
    /// count) over the spots in `counted`.
    fn denial(
        &self,
        k: usize,
        sites: &[Pos],
        assets: &[Asset],
        counted: &[usize],
        own: &[usize],
        p: &Params,
    ) -> (f32, f32, usize) {
        let blocked = self.grid.discs(sites, (p.range - p.disc_margin).max(0.0));
        let after = self.grid.costs(self.enemies[k], Some(&blocked));
        let (mut all, mut mine, mut sealed) = (0.0, 0.0, 0);
        for &i in counted {
            let b = reach_cost(&self.before[k], &self.cells[i]);
            if b == UNREACHED || b == 0 {
                continue;
            }
            let a = reach_cost(&after, &self.cells[i]);
            let s = if a == UNREACHED { 1.0 } else { (1.0 - b as f32 / a as f32).max(0.0) };
            let v = assets[i].value.max(0.0) * s;
            all += v;
            if own.contains(&i) {
                mine += v;
                sealed += usize::from(a == UNREACHED);
            }
        }
        (all, mine, sealed)
    }
}

/// Neutral spots within `spot_cover` of a site.
fn taken(assets: &[Asset], sites: &[Pos], p: &Params) -> Vec<usize> {
    (0..assets.len())
        .filter(|&i| assets[i].side == Side::Neutral && sites.iter().any(|s| s.distance(assets[i].pos) <= p.spot_cover))
        .collect()
}

/// Walks on the commander's tier from our start and the enemies': the
/// contested band and the objectives' walks are measured on them.
struct Band {
    grid: Grid,
    ours: Vec<u32>,
    theirs: Vec<Vec<u32>>,
}

impl Band {
    fn new(walk: &TierMap, ally: Pos, enemies: &[Pos]) -> Band {
        let grid = Grid::new(walk);
        let ours = grid.costs(ally, None);
        let theirs = enemies.iter().map(|&e| grid.costs(e, None)).collect();
        Band { grid, ours, theirs }
    }

    fn elmos(c: u32) -> f32 {
        if c == UNREACHED { f32::INFINITY } else { c as f32 * GRID / STRAIGHT as f32 }
    }

    fn ours_to(&self, q: Pos) -> f32 {
        Self::elmos(self.ours[self.grid.cell(q)])
    }

    fn theirs_to(&self, q: Pos) -> f32 {
        self.theirs.iter().map(|t| Self::elmos(t[self.grid.cell(q)])).fold(f32::INFINITY, f32::min)
    }

    /// Our share of the way between us and the nearest enemy.
    fn share(&self, q: Pos) -> Option<f32> {
        let (a, e) = (self.ours_to(q), self.theirs_to(q));
        (a.is_finite() && e.is_finite()).then(|| a / (a + e).max(1.0))
    }

    fn in_band(&self, q: Pos, p: &Params) -> bool {
        self.share(q).is_some_and(|s| s >= p.band[0] && s <= p.band[1])
    }
}

/// The share of the way from our start to the nearest enemy's at points
/// `step` elmos apart over the walking tier's map, row by row (`None`
/// where either cannot walk): the contested band is where it lies within
/// the band's bounds.
pub fn shares(walk: &TierMap, ally: Pos, enemies: &[Pos], step: f32) -> (usize, usize, Vec<Option<f32>>) {
    let band = Band::new(walk, ally, enemies);
    let (w, h) = (walk.width as f32 * CELL, walk.height as f32 * CELL);
    let (nx, nz) = ((w / step).ceil() as usize, (h / step).ceil() as usize);
    let mut out = Vec::with_capacity(nx * nz);
    for j in 0..nz {
        for i in 0..nx {
            let q = Pos::new((i as f32 + 0.5) * step, (j as f32 + 0.5) * step);
            out.push(if walk.is_passable(q) { band.share(q) } else { None });
        }
    }
    (nx, nz, out)
}

/// One design tier's seeds after the first look, best first: contested
/// chokepoints and their setbacks, contested spots, midpoints of pairs one
/// turret could cover together, and with `lattice` a lattice over the band.
fn shortlist(design: &TierMap, walk: &TierMap, band: &Band, assets: &[Asset], p: &Params) -> Vec<(f32, Pos)> {
    let in_band = |q: Pos| band.in_band(q, p);
    let mut seeds: Vec<Pos> = Vec::new();
    for c in &design.chokes {
        if !in_band(c.pos) {
            continue;
        }
        seeds.push(c.pos);
        for &r in &c.regions {
            seeds.push(design.setback(c.id, r, 160.0));
        }
    }
    for a in assets.iter().filter(|a| a.side == Side::Neutral && in_band(a.pos)) {
        seeds.push(a.pos);
    }
    let base = seeds.len();
    for i in 0..base {
        for j in i + 1..base {
            if seeds[i].distance(seeds[j]) <= 1.6 * p.range {
                seeds.push(Pos::new((seeds[i].x + seeds[j].x) * 0.5, (seeds[i].z + seeds[j].z) * 0.5));
            }
        }
    }
    if p.lattice > 0.0 {
        // Spaced evenly between the map's edges, so the lattice mirrors
        // onto itself under the map's symmetries.
        let (w, h) = (design.width as f32 * CELL, design.height as f32 * CELL);
        let (nx, nz) = ((w / p.lattice).round().max(1.0), (h / p.lattice).round().max(1.0));
        for j in 0..nz as usize {
            for i in 0..nx as usize {
                seeds.push(Pos::new((i as f32 + 0.5) * w / nx, (j as f32 + 0.5) * h / nz));
            }
        }
    }
    seeds.retain(|&s| design.is_passable(s) && walk.is_passable(s) && in_band(s));
    let mut kept: Vec<Pos> = Vec::new();
    for s in seeds {
        if kept.iter().all(|k| k.distance(s) >= 64.0) {
            kept.push(s);
        }
    }
    // First look: neutral income near the seed and chokepoints it covers.
    let typical = assets.iter().map(|a| a.value.max(0.0)).sum::<f32>() / assets.len().max(1) as f32;
    let mut ranked: Vec<(f32, Pos)> = kept
        .into_iter()
        .map(|s| {
            let income: f32 = taken(assets, &[s], p).iter().map(|&i| assets[i].value.max(0.0)).sum();
            let gates = covered(design, &[s], p.range).len() as f32;
            (income + typical * gates, s)
        })
        .filter(|(v, _)| *v > 0.0)
        .collect();
    ranked.sort_by(|a, b| b.0.total_cmp(&a.0));
    ranked.truncate(p.shortlist);
    ranked
}

/// The seeds each design tier's search values in full (for the atlas).
pub fn shortlists(
    maps: &[&TierMap],
    walk: &TierMap,
    ally: Pos,
    enemies: &[Pos],
    assets: &[Asset],
    p: &Params,
) -> Vec<(Tier, Vec<(f32, Pos)>)> {
    let band = Band::new(walk, ally, enemies);
    maps.iter().map(|m| (m.tier, shortlist(m, walk, &band, assets, p))).collect()
}

/// Objectives against ground forces from `enemies`: sites chosen on each of
/// `maps` in turn (the design tier), every objective then valued on all of
/// them. `walk` is the commander's tier (bots), on which the contested
/// band and the walks are measured.
pub fn objectives(
    maps: &[&TierMap],
    walk: &TierMap,
    ally: Pos,
    enemies: &[Pos],
    assets: &[Asset],
    p: &Params,
) -> Vec<Objective> {
    if enemies.is_empty() {
        return Vec::new();
    }
    let band = Band::new(walk, ally, enemies);
    // Our metal: our spots and the neutral ones on our side of the middle.
    let mine: Vec<usize> = (0..assets.len())
        .filter(|&i| match assets[i].side {
            Side::Ally => true,
            Side::Neutral => band.share(assets[i].pos).is_some_and(|s| s < 0.5),
            Side::Enemy => false,
        })
        .collect();
    let counted = |own: &[usize]| -> Vec<usize> {
        let mut c = mine.clone();
        c.extend(own.iter().filter(|i| !mine.contains(i)));
        c
    };
    let cases: Vec<TierCase> = maps.iter().map(|&m| TierCase::new(m, enemies, assets, p)).collect();

    let mut found: Vec<Objective> = Vec::new();
    for case in &cases {
        let design = case.map;
        let ranked = shortlist(design, walk, &band, assets, p);
        let score = |sites: &[Pos]| -> f32 {
            let own = taken(assets, sites, p);
            if own.is_empty() {
                return 0.0;
            }
            let counted = counted(&own);
            (0..enemies.len()).map(|k| case.denial(k, sites, assets, &counted, &own, p).0).sum()
        };
        // Sites of this tier's objectives so far: a further one keeps clear.
        let mut placed: Vec<Pos> = Vec::new();
        for _ in 0..p.per_tier.max(1) {
            let mut sites: Vec<Pos> = Vec::new();
            let mut best = 0.0f32;
            while sites.len() < p.max_sites {
                let mut pick: Option<(Pos, f32)> = None;
                for &(_, s) in &ranked {
                    if sites.iter().any(|t| t.distance(s) < 300.0)
                        || sites.first().is_some_and(|f| f.distance(s) > p.cluster)
                        || placed.iter().any(|t| t.distance(s) <= p.range)
                    {
                        continue;
                    }
                    let mut trial = sites.clone();
                    trial.push(s);
                    let v = score(&trial);
                    // A further site must add a tenth.
                    if v > best * 1.1 + 0.05 && pick.is_none_or(|(_, pv)| v > pv) {
                        pick = Some((s, v));
                    }
                }
                let Some((s, v)) = pick else { break };
                sites.push(s);
                best = v;
            }
            if sites.is_empty() {
                break;
            }
            placed.extend(sites.iter().copied());
            let spots = taken(assets, &sites, p);
            let income: f32 = spots.iter().map(|&i| assets[i].value.max(0.0)).sum();
            let all = counted(&spots);
            let mut denials = Vec::new();
            for c in &cases {
                for k in 0..enemies.len() {
                    let (worth, own, sealed) = c.denial(k, &sites, assets, &all, &spots, p);
                    denials.push(Denial { tier: c.map.tier, enemy: k, worth, own, sealed });
                }
            }
            let walk_to = sites.iter().map(|&s| band.ours_to(s)).fold(f32::INFINITY, f32::min);
            let enemy_walk = sites.iter().map(|&s| band.theirs_to(s)).fold(f32::INFINITY, f32::min);
            found.push(Objective {
                sites,
                spots,
                income,
                denial: denials,
                design: design.tier,
                walk: walk_to,
                enemy_walk,
                score: best,
            });
        }
    }
    // One objective per place: the better of two whose centers are close.
    found.sort_by(|a, b| b.score.total_cmp(&a.score));
    let mut out: Vec<Objective> = Vec::new();
    for o in found {
        if out.iter().all(|k| k.center().distance(o.center()) > p.range) {
            out.push(o);
        }
    }
    out
}

/// Value given sites (the atlas's `--eval`): the same numbers as an
/// objective found by the search.
pub fn evaluate(
    maps: &[&TierMap],
    walk: &TierMap,
    ally: Pos,
    enemies: &[Pos],
    assets: &[Asset],
    sites: &[Pos],
    p: &Params,
) -> Objective {
    let wg = Grid::new(walk);
    let ours = wg.costs(ally, None);
    let theirs: Vec<Vec<u32>> = enemies.iter().map(|&e| wg.costs(e, None)).collect();
    let elmos = |c: u32| if c == UNREACHED { f32::INFINITY } else { c as f32 * GRID / STRAIGHT as f32 };
    let share = |q: Pos| {
        let a = elmos(ours[wg.cell(q)]);
        let e = theirs.iter().map(|t| elmos(t[wg.cell(q)])).fold(f32::INFINITY, f32::min);
        a / (a + e).max(1.0)
    };
    let spots = taken(assets, sites, p);
    let mut all: Vec<usize> = (0..assets.len())
        .filter(|&i| match assets[i].side {
            Side::Ally => true,
            Side::Neutral => share(assets[i].pos) < 0.5,
            Side::Enemy => false,
        })
        .collect();
    all.extend(spots.iter().filter(|i| !all.contains(i)).copied().collect::<Vec<_>>());
    let mut denials = Vec::new();
    for &m in maps {
        let case = TierCase::new(m, enemies, assets, p);
        for k in 0..enemies.len() {
            let (worth, own, sealed) = case.denial(k, sites, assets, &all, &spots, p);
            denials.push(Denial { tier: m.tier, enemy: k, worth, own, sealed });
        }
    }
    let income = spots.iter().map(|&i| assets[i].value.max(0.0)).sum();
    let walk_to = sites.iter().map(|&s| elmos(ours[wg.cell(s)])).fold(f32::INFINITY, f32::min);
    let enemy_walk = sites
        .iter()
        .map(|&s| theirs.iter().map(|t| elmos(t[wg.cell(s)])).fold(f32::INFINITY, f32::min))
        .fold(f32::INFINITY, f32::min);
    Objective {
        sites: sites.to_vec(),
        spots,
        income,
        denial: denials,
        design: walk.tier,
        walk: walk_to,
        enemy_walk,
        score: 0.0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testmaps::{at, ring, rooms};

    fn small() -> Params {
        Params { range: 300.0, disc_margin: 0.0, raid_reach: 100.0, spot_cover: 400.0, ..Params::default() }
    }

    #[test]
    fn a_turret_on_the_only_door_seals_what_lies_behind_it() {
        // a-b-c in a row (d under a and e under c do not meet): from a, c
        // is reached through b only. Our spot in c, the turret at b-c.
        let map = rooms();
        let assets = [Asset { pos: at(24.0, 1.0), value: 2.0, side: Side::Ally }];
        let door = map.chokes[1].pos;
        let o = evaluate(&[&map], &map, at(24.0, 1.0), &[at(2.0, 1.0)], &assets, &[door], &small());
        assert!((o.denial_for(Tier::Vehicle, 0) - 2.0).abs() < 1e-3, "{o:?}");
    }

    #[test]
    fn a_turret_on_one_way_of_two_denies_the_detour_share() {
        // The ring: closing the door a-b leaves the corridor round.
        let map = ring();
        let assets = [Asset { pos: at(20.0, 1.0), value: 4.0, side: Side::Ally }];
        let door = map.chokes[0].pos;
        // (A disc wider than ~180 would close the corridor under the door too.)
        let p = Params { range: 150.0, ..small() };
        let o = evaluate(&[&map], &map, at(20.0, 1.0), &[at(2.0, 1.0)], &assets, &[door], &p);
        let d = o.denial_for(Tier::Vehicle, 0);
        assert!(d > 0.5 && d < 4.0, "denial {d}");
    }

    #[test]
    fn no_turret_no_denial() {
        let map = ring();
        let assets = [Asset { pos: at(20.0, 1.0), value: 4.0, side: Side::Ally }];
        let o = evaluate(&[&map], &map, at(20.0, 1.0), &[at(2.0, 1.0)], &assets, &[], &small());
        assert_eq!(o.denial_for(Tier::Vehicle, 0), 0.0);
    }

    #[test]
    fn coverage_needs_the_whole_front() {
        let map = rooms();
        let mid = map.chokes[0].pos;
        assert!(covered(&map, &[mid], 10.0).contains(&0));
        let far = Pos::new(mid.x + 700.0, mid.z);
        assert!(!covered(&map, &[far], 620.0).contains(&0));
    }

    #[test]
    fn the_objective_takes_the_contested_spot_by_the_door() {
        // We start in a, the enemy in c; a rich spot sits in b by the door
        // to c, our spot in a behind it.
        let map = rooms();
        let assets = [
            Asset { pos: at(2.0, 1.0), value: 2.0, side: Side::Ally },
            Asset { pos: at(14.0, 2.0), value: 3.0, side: Side::Neutral },
            Asset { pos: at(24.0, 1.0), value: 2.0, side: Side::Enemy },
        ];
        let p = Params { band: [0.2, 0.8], ..small() };
        let found = objectives(&[&map], &map, at(1.0, 1.0), &[at(25.0, 1.0)], &assets, &p);
        let o = found.first().expect("an objective");
        assert_eq!(o.spots, vec![1]);
        assert!(o.denial_for(Tier::Vehicle, 0) > 0.0, "{o:?}");
    }
}
