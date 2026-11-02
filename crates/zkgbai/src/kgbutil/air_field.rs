//! Air routes over the threat grid: from one place, the cheapest way for
//! aircraft to everywhere (Dijkstra; a step costs its length, the more the
//! more anti-air covers it, and somewhat more over enemy armies), how far
//! each way is, and how much known anti-air it passes. `route_for` gives a
//! wide-turning airframe (the Odin) fewer waypoints, on turns it can fly.

use std::cmp::Ordering;
use std::collections::BinaryHeap;

use springai::Vec3;

use crate::military::MilitaryManager;

/// Elmos per threat grid cell.
const CELL: f32 = 64.0;

/// How much longer a step counts per unit of anti-air threat over it (as
/// for `PathCost::AirPath`), and of the enemy's ground threat (armies bring
/// anti-air we may not have seen).
const AA_TOLL: f32 = 30.0;
const GROUND_TOLL: f32 = 3.0;

/// Waypoints are at most this many cells apart.
const MAX_LEG: usize = 6;

const NONE: u32 = u32::MAX;

#[derive(PartialEq)]
struct Item(f32, u32);

impl Eq for Item {}

impl Ord for Item {
    fn cmp(&self, o: &Self) -> Ordering {
        o.0.total_cmp(&self.0).then_with(|| self.1.cmp(&o.1))
    }
}

impl PartialOrd for Item {
    fn partial_cmp(&self, o: &Self) -> Option<Ordering> {
        Some(self.cmp(o))
    }
}

pub struct AirField {
    w: i32,
    h: i32,
    cost: Vec<f32>,
    /// Elmos flown, and anti-air passed (threat times cells flown under
    /// it), on the cheapest way.
    length: Vec<f32>,
    exposure: Vec<f32>,
    prev: Vec<u32>,
    /// What a step into each cell costs per elmo.
    toll: Vec<f32>,
}

/// Airframes turning on at least this radius (elmos) get smoothed routes:
/// the Odin (about 400); the Likho (about 195) and the rest fly the cells.
pub const WIDE_TURN: f32 = 300.0;
/// A straight leg may cost this much more than the cheapest way it replaces
/// (a share, and a flat allowance of about two open cells), so it cuts across
/// open air but not into anti-air the cheapest way went round.
const LEG_SLACK: f32 = 0.05;
const LEG_ALLOWANCE: f32 = 2.0 * CELL;
/// Legs reach at most this many cells along the cheapest way.
const LEG_SCAN: usize = 48;
/// The sharpest turn a leg is planned with: a reversal is never a waypoint.
const MAX_TURN: f32 = 2.6;

impl AirField {
    /// The cheapest ways from `from` over a `w` x `h` cell threat grid.
    pub fn new(war: &MilitaryManager, w: i32, h: i32, from: Vec3) -> Self {
        Self::with_interceptors(war, w, h, from, |_| 0.0)
    }

    /// Include interception risk (fighters are deliberately absent from the
    /// ordinary AA map) in both route choice and accumulated exposure.
    pub fn with_interceptors(
        war: &MilitaryManager,
        w: i32,
        h: i32,
        from: Vec3,
        interceptors: impl Fn(Vec3) -> f32,
    ) -> Self {
        let n = (w * h).max(0) as usize;
        let at = |i: i32| Vec3::xz((i % w) as f32 * CELL, (i / w) as f32 * CELL);
        let aa: Vec<f32> = (0..n as i32).map(|i| war.get_aa_threat(at(i)) + interceptors(at(i))).collect();
        let ground: Vec<f32> = (0..n as i32).map(|i| war.get_threat(at(i))).collect();
        Self::solve(w, h, from, aa, &ground)
    }

    /// Dijkstra from `from` over per-cell anti-air and ground threat.
    fn solve(w: i32, h: i32, from: Vec3, aa: Vec<f32>, ground: &[f32]) -> Self {
        let n = (w * h).max(0) as usize;
        let toll: Vec<f32> = (0..n).map(|i| 1.0 + AA_TOLL * aa[i] + GROUND_TOLL * ground[i]).collect();
        let mut f = AirField {
            w,
            h,
            cost: vec![f32::INFINITY; n],
            length: vec![0.0; n],
            exposure: vec![0.0; n],
            prev: vec![NONE; n],
            toll,
        };
        let toll = f.toll.clone();
        let Some(start) = f.index(from) else { return f };
        f.cost[start] = 0.0;
        let mut heap = BinaryHeap::from([Item(0.0, start as u32)]);
        while let Some(Item(c, i)) = heap.pop() {
            let i = i as usize;
            if c > f.cost[i] {
                continue;
            }
            let (x, z) = (i as i32 % w, i as i32 / w);
            for (dx, dz) in [(-1, 0), (1, 0), (0, -1), (0, 1), (-1, -1), (1, -1), (-1, 1), (1, 1)] {
                let (nx, nz) = (x + dx, z + dz);
                if nx < 0 || nz < 0 || nx >= w || nz >= h {
                    continue;
                }
                let j = (nx + nz * w) as usize;
                let step = if dx != 0 && dz != 0 { std::f32::consts::SQRT_2 } else { 1.0 };
                let nc = c + step * CELL * toll[j];
                if nc < f.cost[j] {
                    f.cost[j] = nc;
                    f.length[j] = f.length[i] + step * CELL;
                    f.exposure[j] = f.exposure[i] + step * aa[j];
                    f.prev[j] = i as u32;
                    heap.push(Item(nc, j as u32));
                }
            }
        }
        f
    }

    fn index(&self, p: Vec3) -> Option<usize> {
        let (x, z) = ((p.x / CELL).round() as i32, (p.z / CELL).round() as i32);
        (x >= 0 && z >= 0 && x < self.w && z < self.h).then(|| (x + z * self.w) as usize)
    }

    /// Elmos flown on the cheapest way to `to`; `None` off the grid.
    pub fn length(&self, to: Vec3) -> Option<f32> {
        self.index(to).map(|i| self.length[i]).filter(|_| self.reached(to))
    }

    /// Anti-air the cheapest way to `to` passes (threat times cells flown
    /// under it); `None` off the grid.
    pub fn exposure(&self, to: Vec3) -> Option<f32> {
        self.index(to).map(|i| self.exposure[i]).filter(|_| self.reached(to))
    }

    fn reached(&self, to: Vec3) -> bool {
        self.index(to).is_some_and(|i| self.cost[i].is_finite())
    }

    /// Cells of the cheapest way to `to`, from the start (empty off the grid).
    fn cells(&self, to: Vec3) -> Vec<usize> {
        let Some(mut i) = self.index(to).filter(|_| self.reached(to)) else { return Vec::new() };
        let mut cells = vec![i];
        while self.prev[i] != NONE {
            i = self.prev[i] as usize;
            cells.push(i);
        }
        cells.reverse();
        cells
    }

    fn centre(&self, i: usize) -> Vec3 {
        Vec3::xz((i as i32 % self.w) as f32 * CELL, (i as i32 / self.w) as f32 * CELL)
    }

    /// What flying straight from `a` to `b` costs, sampled every half cell.
    fn leg_cost(&self, a: Vec3, b: Vec3) -> f32 {
        let len = ((b.x - a.x).powi(2) + (b.z - a.z).powi(2)).sqrt();
        let steps = (len / (CELL * 0.5)).ceil().max(1.0) as usize;
        (1..=steps)
            .map(|k| {
                let t = k as f32 / steps as f32;
                let p = Vec3::xz(a.x + (b.x - a.x) * t, a.z + (b.z - a.z) * t);
                self.index(p).map_or(f32::INFINITY, |i| self.toll[i])
            })
            .sum::<f32>()
            * len
            / steps as f32
    }

    /// Waypoints to `to` for an airframe turning on `radius` elmos, flying
    /// along unit `heading` if known. Below [`WIDE_TURN`] this is [`route`].
    /// Otherwise the cheapest way is pulled straight: from each waypoint the
    /// next is the farthest point of the way whose straight leg costs no more
    /// than the way there (by [`LEG_SLACK`] and [`LEG_ALLOWANCE`]), among
    /// those the airframe can turn onto: a turn of angle `a` needs
    /// `radius * tan(a / 2)` of straight flight before and after it, so it
    /// must fit in half of each leg it joins (the engine counts a waypoint
    /// reached within its arrival radius, which `turns` leaves for margin).
    /// When no safe leg's turn fits, the safe leg whose turn overruns its
    /// room least is taken (the plane will swing wide there).
    ///
    /// [`route`]: Self::route
    pub fn route_for(&self, to: Vec3, radius: f32, heading: Option<(f32, f32)>) -> Vec<Vec3> {
        if radius < WIDE_TURN {
            return self.route(to);
        }
        let cells = self.cells(to);
        if cells.len() < 2 {
            return self.route(to);
        }
        let point = |k: usize| if k + 1 == cells.len() { to } else { self.centre(cells[k]) };
        let lead = |angle: f32| radius * (angle.min(MAX_TURN) * 0.5).tan();
        let mut route = Vec::new();
        let (mut at, mut from) = (0, point(0));
        let (mut dir, mut last_leg) = (heading, f32::INFINITY);
        while at + 1 < cells.len() {
            let mut fallback: Option<(f32, usize)> = None;
            let mut chosen = None;
            for b in (at + 1..cells.len().min(at + 1 + LEG_SCAN)).rev() {
                let p = point(b);
                let way = self.cost[cells[b]] - self.cost[cells[at]];
                if self.leg_cost(from, p) > way * (1.0 + LEG_SLACK) + LEG_ALLOWANCE {
                    continue;
                }
                let len = ((p.x - from.x).powi(2) + (p.z - from.z).powi(2)).sqrt();
                let turn = dir.map_or(0.0, |(hx, hz)| {
                    let (nx, nz) = ((p.x - from.x) / len.max(1e-3), (p.z - from.z) / len.max(1e-3));
                    (hx * nx + hz * nz).clamp(-1.0, 1.0).acos()
                });
                let overrun = lead(turn) - 0.5 * last_leg.min(len) + if turn > MAX_TURN { 1e6 } else { 0.0 };
                if overrun <= 0.0 {
                    chosen = Some(b);
                    break;
                }
                if fallback.is_none_or(|(o, _)| overrun < o) {
                    fallback = Some((overrun, b));
                }
            }
            let b = chosen.or(fallback.map(|(_, b)| b)).unwrap_or(at + 1);
            let p = point(b);
            let len = ((p.x - from.x).powi(2) + (p.z - from.z).powi(2)).sqrt();
            if len > 1e-3 {
                dir = Some(((p.x - from.x) / len, (p.z - from.z) / len));
            }
            last_leg = len;
            route.push(p);
            (at, from) = (b, p);
        }
        route
    }

    /// Waypoints of the cheapest way to `to`, the last `to` itself (empty
    /// off the grid): where the way turns, and every few cells.
    pub fn route(&self, to: Vec3) -> Vec<Vec3> {
        let cells = self.cells(to);
        if cells.is_empty() {
            return Vec::new();
        }
        let at = |i: usize| (i as i32 % self.w, i as i32 / self.w);
        let mut route = Vec::new();
        let mut leg = 0;
        for k in 1..cells.len().saturating_sub(1) {
            let ((ax, az), (bx, bz), (cx, cz)) = (at(cells[k - 1]), at(cells[k]), at(cells[k + 1]));
            leg += 1;
            if (bx - ax, bz - az) != (cx - bx, cz - bz) || leg >= MAX_LEG {
                route.push(Vec3::xz(bx as f32 * CELL, bz as f32 * CELL));
                leg = 0;
            }
        }
        route.push(to);
        route
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 64 x 64 cell field (4096 elmos) from (200, 2048), with anti-air
    /// where `aa` says.
    fn field(aa: impl Fn(f32, f32) -> f32) -> AirField {
        let (w, h) = (64, 64);
        let cells: Vec<f32> = (0..w * h).map(|i| aa((i % w) as f32 * CELL, (i / w) as f32 * CELL)).collect();
        AirField::solve(w, h, Vec3::xz(200.0, 2048.0), cells, &vec![0.0; (w * h) as usize])
    }

    fn turns(from: Vec3, route: &[Vec3]) -> Vec<f32> {
        let mut pts = vec![from];
        pts.extend_from_slice(route);
        pts.windows(3)
            .map(|w| {
                let (a, b) = ((w[1].x - w[0].x, w[1].z - w[0].z), (w[2].x - w[1].x, w[2].z - w[1].z));
                let (la, lb) = ((a.0 * a.0 + a.1 * a.1).sqrt(), (b.0 * b.0 + b.1 * b.1).sqrt());
                ((a.0 * b.0 + a.1 * b.1) / (la * lb)).clamp(-1.0, 1.0).acos()
            })
            .collect()
    }

    #[test]
    fn open_air_is_one_straight_leg_for_an_odin() {
        let f = field(|_, _| 0.0);
        let to = Vec3::xz(3500.0, 3000.0);
        // The cell way staircases (eight directions): many waypoints.
        assert!(f.route(to).len() >= 4);
        // An Odin flies one or two legs, straight at it.
        let odin = f.route_for(to, 400.0, None);
        assert!(odin.len() <= 2, "{odin:?}");
        assert_eq!(*odin.last().unwrap(), to);
        // Nimble airframes keep the old route.
        assert_eq!(f.route_for(to, 100.0, None), f.route(to));
    }

    #[test]
    fn an_odin_still_goes_round_anti_air_on_turns_it_can_fly() {
        // A wall of anti-air across the middle, open only near the bottom.
        let f = field(|x, z| if (1900.0..2200.0).contains(&x) && z > 900.0 { 3.0 } else { 0.0 });
        let to = Vec3::xz(3800.0, 2048.0);
        let from = Vec3::xz(200.0, 2048.0);
        let odin = f.route_for(to, 400.0, Some((1.0, 0.0)));
        // Never across the wall.
        let mut prev = from;
        for &p in &odin {
            assert!(f.leg_cost(prev, p).is_finite());
            let mid = Vec3::xz((prev.x + p.x) / 2.0, (prev.z + p.z) / 2.0);
            assert!(!((1900.0..2200.0).contains(&mid.x) && mid.z > 900.0), "{odin:?}");
            prev = p;
        }
        // Few waypoints, and every turn fits its legs.
        assert!(odin.len() <= f.route(to).len() / 2, "{odin:?} vs {:?}", f.route(to));
        let mut pts = vec![from];
        pts.extend_from_slice(&odin);
        for (k, a) in turns(from, &odin).into_iter().enumerate() {
            let legs = distance(pts[k], pts[k + 1]).min(distance(pts[k + 1], pts[k + 2]));
            assert!(400.0 * (a * 0.5).tan() <= 0.5 * legs + 1.0, "turn {a} on legs {legs}: {odin:?}");
        }
    }

    use crate::util::distance;
}
