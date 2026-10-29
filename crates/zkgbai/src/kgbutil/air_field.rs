//! Air routes over the threat grid: from one place, the cheapest way for
//! aircraft to everywhere (Dijkstra; a step costs its length, the more the
//! more anti-air covers it, and somewhat more over enemy armies), how far
//! each way is, and how much known anti-air it passes.

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
}

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
        let mut f = AirField {
            w,
            h,
            cost: vec![f32::INFINITY; n],
            length: vec![0.0; n],
            exposure: vec![0.0; n],
            prev: vec![NONE; n],
        };
        let at = |i: i32| Vec3::xz((i % w) as f32 * CELL, (i / w) as f32 * CELL);
        let aa: Vec<f32> = (0..n as i32).map(|i| war.get_aa_threat(at(i)) + interceptors(at(i))).collect();
        let toll: Vec<f32> =
            (0..n as i32).map(|i| 1.0 + AA_TOLL * aa[i as usize] + GROUND_TOLL * war.get_threat(at(i))).collect();
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

    /// Waypoints of the cheapest way to `to`, the last `to` itself (empty
    /// off the grid): where the way turns, and every few cells.
    pub fn route(&self, to: Vec3) -> Vec<Vec3> {
        let Some(mut i) = self.index(to).filter(|_| self.reached(to)) else { return Vec::new() };
        let mut cells = vec![i];
        while self.prev[i] != NONE {
            i = self.prev[i] as usize;
            cells.push(i);
        }
        cells.reverse();
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
