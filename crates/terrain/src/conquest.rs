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
//! Doors and lines (user, 2026-11-03, Mercurial: "two chokepoints, each
//! coverable by a single Stinger, slicing through two mexes each placed
//! right there, and dividing the map perfectly in half"). With
//! `door_slack` on, a turret also closes a chokepoint whose whole wall
//! lies within its reach plus the slack: raiders squeezing along the cliff
//! at the edge of a Stinger's reach do not count as a way through (the
//! discs alone left Mercurial's ~1300-elmo gaps open). With `lines` on,
//! the search also values sets of two or three such doors together: each
//! of Mercurial's gaps alone is worth its own mexes (raiders use the other
//! one), both together seal our half (~32 metal/s against bots), so a
//! search that grows one site at a time never finds them. A line is the
//! minimum cut across the contested band between the enemy's start and our
//! metal (each band cell a unit; the terrain model's chokepoints need not
//! lie where the cut does: it split Mercurial's eastern gap in two), its
//! stretches the doors, one turret each when the door is no wider than
//! twice the reach plus slack, at most three turrets.
//!
//! Contested metal (user, 2026-11-03, AlienDesert: the objective "on the
//! border of the map rather than near one of the mexes in the center where
//! it would really inconvenience the adversary"): on open ground a turret
//! on the middle's spots and one on a pair by our edge seal the same own
//! income, and the race factor then prefers the safer pair. With `contest`
//! an own spot also counts what holding it takes from the enemy: none on
//! our side of the band, rising to its full value again at the middle
//! (`1 + (share - band low) / (0.5 - band low)`, at most 2).
//!
//! Junctions (user, 2026-11-03, Fairyland: the centerward tuplets at the
//! ends of the S, where the ring road meets the middle, never win): a turret
//! there makes the enemy's ways to our metal longer without closing them, and
//! `1 - before / after` gives a fifth of a spot for a fifth longer, next to a
//! whole spot for any sealed corner pair. With `detour_sat` a detour of that
//! share or more counts as a seal (the raid takes the long way round past
//! our army, or does not come), less in proportion.
//!
//! The objectives are computed once per map and start; the AI weighs each
//! tier's number with what it knows of each enemy's tier when it has to
//! commit the turrets.

use std::cmp::Reverse;
use std::collections::BinaryHeap;

use crate::strategy::{Asset, Side};
use crate::{CELL, Choke, ChokeKind, Pos, Tier, TierMap};

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
    /// A turret closes a door whose whole wall lies within its reach plus
    /// this, elmos (see the module notes); negative: discs only.
    pub door_slack: f32,
    /// Also look for lines of two or three doors (see the module notes).
    pub lines: bool,
    /// Weigh the objective's own spots by how contested they are (see the
    /// module notes).
    pub contest: bool,
    /// A detour of this share (1 - before/after) or more counts as a seal,
    /// less in proportion (see the module notes); 0: the share itself.
    pub detour_sat: f32,
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
            door_slack: -1.0,
            lines: false,
            contest: false,
            detour_sat: 0.0,
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
    /// Every spot (index into the assets, whoever holds it) whose way from
    /// this enemy gets longer by at least [`SHIELD_MIN`] with the turrets'
    /// reach closed: `1 - before / after` (cut off: 1). What holding the
    /// objective shelters, ours to take while it is held.
    pub shield: Vec<(usize, f32)>,
}

/// A spot counts as shielded by an objective from this share on.
pub const SHIELD_MIN: f32 = 0.05;

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
    /// A line: doors far apart that hold together (see the module notes),
    /// one turret each.
    pub line: bool,
    /// With `contest`: what the objective's own spots add for being
    /// contested (metal/s, on top of the denial).
    pub contest: f32,
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

/// The two ends of the wall a turret must reach to close a chokepoint: its
/// gate line, lengthened to the wall (the gate and the passable slopes
/// beside it). None for seams (nothing to hold) and chokepoints without a
/// gate line.
pub fn door_ends(c: &Choke) -> Option<(Pos, Pos)> {
    if c.kind == ChokeKind::Seam {
        return None;
    }
    let [a, b] = c.gate?;
    let len = a.distance(b);
    if len < 1.0 {
        return None;
    }
    let half = len.max(c.wall) * 0.5;
    let (dx, dz) = ((b.x - a.x) / len, (b.z - a.z) / len);
    let mid = Pos::new((a.x + b.x) * 0.5, (a.z + b.z) * 0.5);
    Some((Pos::new(mid.x - dx * half, mid.z - dz * half), Pos::new(mid.x + dx * half, mid.z + dz * half)))
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

    /// What `sites` close on `map`: their discs, and with `door_slack` on
    /// the doors they reach end to end: chokepoint walls, and the narrowest
    /// wall-to-wall line through each site.
    fn closure(&self, map: &TierMap, sites: &[Pos], p: &Params) -> Vec<bool> {
        let mut m = self.discs(sites, (p.range - p.disc_margin).max(0.0));
        if p.door_slack < 0.0 || sites.is_empty() {
            return m;
        }
        let reach = p.range + p.door_slack;
        for c in &map.chokes {
            let Some((e1, e2)) = door_ends(c) else { continue };
            if sites.iter().any(|s| s.distance(e1) <= reach && s.distance(e2) <= reach) {
                self.block_line(&mut m, e1, e2);
            }
        }
        for &s in sites {
            if let Some((e1, e2)) = self.door_line(s, reach) {
                self.block_line(&mut m, e1, e2);
            }
        }
        // A door two turrets hold: each reaches a wall beyond itself, away
        // from the other, and the two reach each other's halfway point.
        for (i, &a) in sites.iter().enumerate() {
            for &b in &sites[i + 1..] {
                let d = a.distance(b);
                if d < 1.0 || d > 2.0 * reach {
                    continue;
                }
                let (Some(wa), Some(wb)) = (self.wall_along(a, a, b, reach), self.wall_along(b, b, a, reach)) else {
                    continue;
                };
                self.block_line(&mut m, wa, wb);
            }
        }
        m
    }

    /// The first wall from `s` going on from `from` past `s` (away from
    /// `toward`), within `reach`.
    fn wall_along(&self, s: Pos, from: Pos, toward: Pos, reach: f32) -> Option<Pos> {
        let len = from.distance(toward).max(1.0);
        let (dx, dz) = ((from.x - toward.x) / len, (from.z - toward.z) / len);
        let inside = |q: Pos| q.x >= 0.0 && q.z >= 0.0 && q.x < self.w as f32 * GRID && q.z < self.h as f32 * GRID;
        let mut d = GRID * 0.5;
        while d <= reach {
            let q = Pos::new(s.x + dx * d, s.z + dz * d);
            if !inside(q) || !self.pass[self.cell(q)] {
                return Some(q);
            }
            d += GRID * 0.5;
        }
        None
    }

    /// The narrowest wall-to-wall line through `s` (32 headings) whose
    /// ends both lie within `reach` of it: the door a turret at `s` closes.
    /// The map's edge is a wall.
    fn door_line(&self, s: Pos, reach: f32) -> Option<(Pos, Pos)> {
        if !self.pass[self.cell(s)] {
            return None;
        }
        let inside = |q: Pos| q.x >= 0.0 && q.z >= 0.0 && q.x < self.w as f32 * GRID && q.z < self.h as f32 * GRID;
        let mut best: Option<(f32, Pos, Pos)> = None;
        for k in 0..32 {
            let th = k as f32 * std::f32::consts::PI / 32.0;
            let (dx, dz) = (th.cos(), th.sin());
            let wall = |sign: f32| -> Option<f32> {
                let mut d = GRID * 0.5;
                while d <= reach {
                    let q = Pos::new(s.x + sign * dx * d, s.z + sign * dz * d);
                    if !inside(q) || !self.pass[self.cell(q)] {
                        return Some(d);
                    }
                    d += GRID * 0.5;
                }
                None
            };
            let (Some(a), Some(b)) = (wall(1.0), wall(-1.0)) else { continue };
            if best.is_none_or(|(l, _, _)| a + b < l) {
                best = Some((a + b, Pos::new(s.x + dx * a, s.z + dz * a), Pos::new(s.x - dx * b, s.z - dz * b)));
            }
        }
        best.map(|(_, a, b)| (a, b))
    }

    /// Mark the cells along `e1`-`e2`, a cell past each end (no corner
    /// cutting gets through a line of cells).
    fn block_line(&self, m: &mut [bool], e1: Pos, e2: Pos) {
        let len = e1.distance(e2).max(1.0);
        let (dx, dz) = ((e2.x - e1.x) / len, (e2.z - e1.z) / len);
        let a = Pos::new(e1.x - dx * GRID, e1.z - dz * GRID);
        let b = Pos::new(e2.x + dx * GRID, e2.z + dz * GRID);
        let n = (a.distance(b) / (GRID * 0.5)).ceil().max(1.0) as usize;
        for k in 0..=n {
            let t = k as f32 / n as f32;
            m[self.cell(Pos::new(a.x + (b.x - a.x) * t, a.z + (b.z - a.z) * t))] = true;
        }
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
        let blocked = self.grid.closure(self.map, sites, p);
        let after = self.grid.costs(self.enemies[k], Some(&blocked));
        let (mut all, mut mine, mut sealed) = (0.0, 0.0, 0);
        for &i in counted {
            let b = reach_cost(&self.before[k], &self.cells[i]);
            if b == UNREACHED || b == 0 {
                continue;
            }
            let a = reach_cost(&after, &self.cells[i]);
            let s = if a == UNREACHED { 1.0 } else { (1.0 - b as f32 / a as f32).max(0.0) };
            let v = assets[i].value.max(0.0) * saturate(s, p);
            all += v;
            if own.contains(&i) {
                mine += v;
                sealed += usize::from(a == UNREACHED);
            }
        }
        (all, mine, sealed)
    }

    /// [`Self::denial`], and the shield over every asset (see
    /// [`Denial::shield`]).
    fn denial_and_shield(
        &self,
        k: usize,
        sites: &[Pos],
        assets: &[Asset],
        counted: &[usize],
        own: &[usize],
        p: &Params,
    ) -> Denial {
        let blocked = self.grid.closure(self.map, sites, p);
        let after = self.grid.costs(self.enemies[k], Some(&blocked));
        let (mut all, mut mine, mut sealed) = (0.0, 0.0, 0);
        let mut shield = Vec::new();
        for (i, asset) in assets.iter().enumerate() {
            let b = reach_cost(&self.before[k], &self.cells[i]);
            if b == UNREACHED || b == 0 {
                continue;
            }
            let a = reach_cost(&after, &self.cells[i]);
            let s = if a == UNREACHED { 1.0 } else { (1.0 - b as f32 / a as f32).max(0.0) };
            if s >= SHIELD_MIN {
                shield.push((i, s));
            }
            if !counted.contains(&i) {
                continue;
            }
            let v = asset.value.max(0.0) * saturate(s, p);
            all += v;
            if own.contains(&i) {
                mine += v;
                sealed += usize::from(a == UNREACHED);
            }
        }
        Denial { tier: self.map.tier, enemy: k, worth: all, own: mine, sealed, shield }
    }
}

/// A detour share as worth (see `detour_sat`).
fn saturate(s: f32, p: &Params) -> f32 {
    if p.detour_sat > 0.0 { (s / p.detour_sat).min(1.0) } else { s }
}

/// An own spot's weight with `contest` on (see the module notes).
fn contest_weight(share: Option<f32>, p: &Params) -> f32 {
    match share {
        Some(s) if p.contest => 1.0 + ((s - p.band[0]) / (0.5 - p.band[0]).max(1e-3)).clamp(0.0, 1.0),
        _ => 1.0,
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
    let weight: Vec<f32> = assets.iter().map(|a| contest_weight(band.share(a.pos), p)).collect();
    let bonus = |own: &[usize]| -> f32 { own.iter().map(|&i| assets[i].value.max(0.0) * (weight[i] - 1.0)).sum() };
    let make = |sites: Vec<Pos>, score: f32, design: Tier, line: bool| -> Objective {
        let spots = taken(assets, &sites, p);
        let contest = bonus(&spots);
        let income: f32 = spots.iter().map(|&i| assets[i].value.max(0.0)).sum();
        let all = counted(&spots);
        let mut denials = Vec::new();
        for c in &cases {
            for k in 0..enemies.len() {
                denials.push(c.denial_and_shield(k, &sites, assets, &all, &spots, p));
            }
        }
        let walk_to = sites.iter().map(|&s| band.ours_to(s)).fold(f32::INFINITY, f32::min);
        let enemy_walk = sites.iter().map(|&s| band.theirs_to(s)).fold(f32::INFINITY, f32::min);
        Objective { sites, spots, income, denial: denials, design, walk: walk_to, enemy_walk, score, line, contest }
    };

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
            (0..enemies.len()).map(|k| case.denial(k, sites, assets, &counted, &own, p).0).sum::<f32>()
                + enemies.len() as f32 * bonus(&own)
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
            found.push(make(sites, best, design.tier, false));
        }
        if p.lines && p.door_slack >= 0.0 {
            // What the line keeps behind it: the metal on our side of the
            // band by the walks (contested spots go either way: by the
            // doors, the turrets cover them; a far spot the spot graph
            // calls ours does not drag the line round it).
            let mut sinks: Vec<Pos> = (assets.iter())
                .filter(|a| a.side != Side::Enemy && band.share(a.pos).is_some_and(|s| s < p.band[0]))
                .map(|a| a.pos)
                .collect();
            sinks.push(ally);
            if let Some(sites) = cut_line(&case.grid, walk, &band, enemies, &sinks, p) {
                let value = |sites: &[Pos]| -> f32 {
                    let own = taken(assets, sites, p);
                    let counted = counted(&own);
                    (0..enemies.len()).map(|k| case.denial(k, sites, assets, &counted, &own, p).0).sum::<f32>()
                        + enemies.len() as f32 * bonus(&own)
                };
                // It holds together, or it is just its turrets.
                let v = value(&sites);
                let apart: f32 = sites.iter().map(|&s| value(&[s])).sum();
                if v > 0.0 && v >= LINE_GAIN * apart {
                    found.push(make(sites, v, design.tier, true));
                }
            }
        }
    }
    // One objective per place: the better of two whose centers are close
    // (a line and another: whose sites are).
    found.sort_by(|a, b| b.score.total_cmp(&a.score));
    let mut out: Vec<Objective> = Vec::new();
    for o in found {
        let near = |k: &Objective| {
            if k.line || o.line {
                k.sites.iter().any(|a| o.sites.iter().any(|b| a.distance(*b) <= p.range))
            } else {
                k.center().distance(o.center()) <= p.range
            }
        };
        if !out.iter().any(near) {
            out.push(o);
        }
    }
    out
}

/// Turrets a line may take at most.
const LINE_TURRETS: usize = 3;
/// A line is worth this much more than its turrets apart.
const LINE_GAIN: f32 = 1.5;

/// The narrowest line across the contested band between the enemy and our
/// metal (see the module notes), as turret sites: a minimum cut of the
/// design tier's grid (each band cell one unit, cells outside the band and
/// round our base, our spots and the enemy's start uncuttable), its
/// stretches the doors, each held by turrets `2 * (range + door_slack)`
/// apart. None when no cut within `LINE_TURRETS` turrets exists.
fn cut_line(grid: &Grid, walk: &TierMap, band: &Band, enemies: &[Pos], sinks: &[Pos], p: &Params) -> Option<Vec<Pos>> {
    let reach = p.range + p.door_slack;
    let n = grid.w * grid.h;
    const INF: i32 = 1 << 28;
    // Nodes: cell i in = 2i, out = 2i + 1; source 2n, sink 2n + 1.
    let (src, snk) = (2 * n, 2 * n + 1);
    let mut head = vec![u32::MAX; 2 * n + 2];
    let mut to: Vec<u32> = Vec::new();
    let mut cap: Vec<i32> = Vec::new();
    let mut next: Vec<u32> = Vec::new();
    let mut add = |head: &mut Vec<u32>, a: usize, b: usize, c: i32| {
        for (x, y, k) in [(a, b, c), (b, a, 0)] {
            to.push(y as u32);
            cap.push(k);
            next.push(head[x]);
            head[x] = (to.len() - 1) as u32;
        }
    };
    let mut fixed = vec![false; n];
    for &q in sinks.iter().chain(enemies) {
        for i in grid.around(q, p.raid_reach.max(6.0 * GRID)) {
            fixed[i] = true;
        }
    }
    for (i, &fix) in fixed.iter().enumerate() {
        if !grid.pass[i] {
            continue;
        }
        let c = Grid::center_of(grid.w, i);
        let cuttable = !fix && band.in_band(c, p);
        add(&mut head, 2 * i, 2 * i + 1, if cuttable { 1 } else { INF });
        let (x, z) = ((i % grid.w) as isize, (i / grid.w) as isize);
        for (dx, dz) in [(-1, 0), (1, 0), (0, -1), (0, 1), (-1, -1), (1, -1), (-1, 1), (1, 1)] {
            let (nx, nz) = (x + dx, z + dz);
            if nx < 0 || nz < 0 || nx >= grid.w as isize || nz >= grid.h as isize {
                continue;
            }
            let j = nz as usize * grid.w + nx as usize;
            if !grid.pass[j]
                || (dx != 0
                    && dz != 0
                    && (!grid.pass[z as usize * grid.w + nx as usize] || !grid.pass[nz as usize * grid.w + x as usize]))
            {
                continue;
            }
            add(&mut head, 2 * i + 1, 2 * j, INF);
        }
    }
    for &e in enemies {
        for i in grid.around(e, 6.0 * GRID) {
            add(&mut head, src, 2 * i, INF);
        }
    }
    for &q in sinks {
        for i in grid.around(q, p.raid_reach) {
            add(&mut head, 2 * i + 1, snk, INF);
        }
    }
    // Augmenting paths, one unit each (every way crosses a unit cell, or
    // there is no cut in the band), until the line is too wide to hold.
    let limit = (LINE_TURRETS as f32 * 2.0 * reach / GRID * 1.5) as i32;
    let mut flow = 0;
    let mut prev = vec![u32::MAX; 2 * n + 2];
    loop {
        prev.iter_mut().for_each(|v| *v = u32::MAX);
        let mut queue = std::collections::VecDeque::from([src]);
        let mut seen = vec![false; 2 * n + 2];
        seen[src] = true;
        while let Some(u) = queue.pop_front() {
            if u == snk {
                break;
            }
            let mut e = head[u];
            while e != u32::MAX {
                let v = to[e as usize] as usize;
                if cap[e as usize] > 0 && !seen[v] {
                    seen[v] = true;
                    prev[v] = e;
                    queue.push_back(v);
                }
                e = next[e as usize];
            }
        }
        if !seen[snk] {
            break;
        }
        let mut push = INF;
        let mut v = snk;
        while v != src {
            let e = prev[v] as usize;
            push = push.min(cap[e]);
            v = to[e ^ 1] as usize;
        }
        if push >= INF {
            return None;
        }
        let mut v = snk;
        while v != src {
            let e = prev[v] as usize;
            cap[e] -= push;
            cap[e ^ 1] += push;
            v = to[e ^ 1] as usize;
        }
        flow += push;
        if flow > limit {
            return None;
        }
    }
    if flow == 0 {
        return None;
    }
    // The cut: cells whose way in is reached from the enemy, their way out not.
    let mut seen = vec![false; 2 * n + 2];
    let mut stack = vec![src];
    seen[src] = true;
    while let Some(u) = stack.pop() {
        let mut e = head[u];
        while e != u32::MAX {
            let v = to[e as usize] as usize;
            if cap[e as usize] > 0 && !seen[v] {
                seen[v] = true;
                stack.push(v);
            }
            e = next[e as usize];
        }
    }
    let cut: Vec<usize> = (0..n).filter(|&i| grid.pass[i] && seen[2 * i] && !seen[2 * i + 1]).collect();
    // Doors: the cut's stretches (cells touching, diagonals too).
    let mut door_of = vec![usize::MAX; n];
    let mut doors: Vec<Vec<usize>> = Vec::new();
    for &c in &cut {
        if door_of[c] != usize::MAX {
            continue;
        }
        let d = doors.len();
        let mut members = vec![c];
        door_of[c] = d;
        let mut k = 0;
        while k < members.len() {
            let u = members[k];
            k += 1;
            let (x, z) = ((u % grid.w) as isize, (u / grid.w) as isize);
            for &v in &cut {
                let (vx, vz) = ((v % grid.w) as isize, (v / grid.w) as isize);
                if door_of[v] == usize::MAX && (vx - x).abs() <= 1 && (vz - z).abs() <= 1 {
                    door_of[v] = d;
                    members.push(v);
                }
            }
        }
        doors.push(members);
    }
    let mut sites = Vec::new();
    for members in &doors {
        let pos: Vec<Pos> = members.iter().map(|&i| Grid::center_of(grid.w, i)).collect();
        let far = |from: Pos| *pos.iter().max_by(|a, b| a.distance(from).total_cmp(&b.distance(from))).unwrap_or(&from);
        let a = far(pos[0]);
        let b = far(a);
        let len = a.distance(b) + GRID;
        let k = (len / (2.0 * reach)).ceil().max(1.0) as usize;
        if std::env::var_os("ZK_CUT_DEBUG").is_some() {
            eprintln!(
                "cut {:?}: door ({:.0},{:.0})-({:.0},{:.0}) {len:.0} long, {} cells, {k} turrets",
                grid.tier,
                a.x,
                a.z,
                b.x,
                b.z,
                members.len()
            );
        }
        // Along the door from end to end (the cut itself need not run
        // straight), on the cut where that point cannot be built on.
        let open = |q: Pos| grid.pass[grid.cell(q)] && walk.is_passable(q);
        for j in 0..k {
            let t = (j as f32 + 0.5) / k as f32;
            let want = Pos::new(a.x + (b.x - a.x) * t, a.z + (b.z - a.z) * t);
            let at = if open(want) {
                want
            } else {
                *pos.iter().min_by(|x, y| x.distance(want).total_cmp(&y.distance(want))).unwrap_or(&want)
            };
            if !walk.is_passable(at) {
                return None;
            }
            sites.push(at);
        }
        if sites.len() > LINE_TURRETS {
            return None;
        }
    }
    Some(sites)
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
            denials.push(case.denial_and_shield(k, sites, assets, &all, &spots, p));
        }
    }
    let income = spots.iter().map(|&i| assets[i].value.max(0.0)).sum();
    let contest =
        spots.iter().map(|&i| assets[i].value.max(0.0) * (contest_weight(Some(share(assets[i].pos)), p) - 1.0)).sum();
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
        line: false,
        contest,
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
    fn the_shield_lists_the_spot_behind_the_only_door() {
        let map = rooms();
        let assets = [
            Asset { pos: at(24.0, 1.0), value: 2.0, side: Side::Ally },
            Asset { pos: at(2.0, 1.0), value: 2.0, side: Side::Enemy },
        ];
        let door = map.chokes[1].pos;
        let o = evaluate(&[&map], &map, at(24.0, 1.0), &[at(2.0, 1.0)], &assets, &[door], &small());
        let d = o.denial.iter().find(|d| d.tier == Tier::Vehicle).unwrap();
        assert!(d.shield.iter().any(|&(i, s)| i == 0 && (s - 1.0).abs() < 1e-3), "{:?}", d.shield);
        assert!(!d.shield.iter().any(|&(i, _)| i == 1), "{:?}", d.shield);
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

    /// Gate lines across the test maps' doors (`across`: true where the
    /// wall runs along x), a cell into the walls at each end.
    fn gated(mut map: TierMap, across: &[bool]) -> TierMap {
        for (c, &x) in map.chokes.iter_mut().zip(across) {
            let h = c.width * 0.5 + CELL;
            let (a, b) = if x {
                (Pos::new(c.pos.x - h, c.pos.z), Pos::new(c.pos.x + h, c.pos.z))
            } else {
                (Pos::new(c.pos.x, c.pos.z - h), Pos::new(c.pos.x, c.pos.z + h))
            };
            c.gate = Some([a, b]);
            c.wall = c.width + 2.0 * CELL;
        }
        map
    }

    #[test]
    fn a_turret_closes_a_door_its_reach_and_slack_span() {
        // The b-c door of the rooms, a turret too short to close it by its
        // disc: with the slack it closes the whole wall.
        let map = gated(rooms(), &[false, false, true, true]);
        let assets = [Asset { pos: at(24.0, 1.0), value: 2.0, side: Side::Ally }];
        let door = map.chokes[1].pos;
        let short = Params { range: 10.0, ..small() };
        let o = evaluate(&[&map], &map, at(24.0, 1.0), &[at(2.0, 1.0)], &assets, &[door], &short);
        assert!(o.denial_for(Tier::Vehicle, 0) < 1.0, "{o:?}");
        let slack = Params { door_slack: 200.0, ..short };
        let o = evaluate(&[&map], &map, at(24.0, 1.0), &[at(2.0, 1.0)], &assets, &[door], &slack);
        assert!((o.denial_for(Tier::Vehicle, 0) - 2.0).abs() < 1e-3, "{o:?}");
    }

    #[test]
    fn a_line_of_two_doors_seals_what_neither_does_alone() {
        // The ring: the door and either end of the corridor round are two
        // ways; closing one leaves the other.
        let map = gated(ring(), &[false, true, true]);
        let assets = [Asset { pos: at(20.0, 1.0), value: 4.0, side: Side::Ally }];
        let p = Params { range: 150.0, door_slack: 60.0, lines: true, band: [0.0, 1.0], ..small() };
        let found = objectives(&[&map], &map, at(20.0, 1.0), &[at(2.0, 1.0)], &assets, &p);
        let line = found.iter().find(|o| o.line).unwrap_or_else(|| panic!("no line in {found:?}"));
        assert_eq!(line.sites.len(), 2, "{line:?}");
        assert!((line.denial_for(Tier::Vehicle, 0) - 4.0).abs() < 1e-3, "{line:?}");
    }
}
