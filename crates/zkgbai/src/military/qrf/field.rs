//! Travel times over the ground, on a grid of 64-elmo cells: from some
//! sources (each with a head start), how many seconds a unit of a movement
//! class needs to get to every cell, by Dijkstra. A cell is passable for a
//! class when most of its slope squares are no steeper than the class
//! climbs and its ground is no deeper under water than it wades (hovers
//! and amphibians go anywhere flat enough).

use std::cmp::Ordering;
use std::collections::BinaryHeap;

use springai::{Callback, UnitDef, Vec3};

/// Elmos per cell.
pub const CELL: f32 = 64.0;

/// Slope squares (16 elmos) per cell, per axis.
const SUB: usize = 4;

/// A cell steeper than its class climbs in more than this share of its
/// slope squares is impassable for it.
const BLOCKED_SHARE: f32 = 0.5;

/// Seconds: unreachable.
pub const NEVER: f32 = f32::INFINITY;

/// How a unit gets over the ground.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Mobility {
    pub max_slope: f32,
    /// Deepest water it goes into (elmos below the surface); infinite for
    /// hovers and amphibians.
    pub max_depth: f32,
}

impl Mobility {
    /// The movement class of a ground unit; `None` for aircraft and
    /// buildings.
    pub fn of(def: UnitDef) -> Option<Mobility> {
        if def.is_able_to_fly() || def.speed() <= 0.0 {
            return None;
        }
        let md = def.move_data()?;
        let depth = def.max_water_depth();
        Some(Mobility { max_slope: md.max_slope(), max_depth: if depth > 1000.0 { f32::INFINITY } else { depth } })
    }

    /// A bot's: what raiders mostly are.
    pub const BOT: Mobility = Mobility { max_slope: 0.41, max_depth: 22.0 };
}

/// The ground: per cell, the share of its slope squares steeper than a few
/// slope levels, and its lowest height.
pub struct Grid {
    pub w: usize,
    pub h: usize,
    slopes: Vec<[f32; SUB * SUB]>,
    low: Vec<f32>,
}

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

impl Grid {
    pub fn new(cb: Callback) -> Grid {
        let map = cb.map();
        let (mw, mh) = (map.width() as usize, map.height() as usize);
        let slope = map.slope_map();
        let height = map.height_map();
        let (sw, sh) = (mw / 2, mh / 2);
        let (w, h) = (sw.div_ceil(SUB), sh.div_ceil(SUB));
        let mut slopes = vec![[0.0; SUB * SUB]; w * h];
        let mut low = vec![f32::INFINITY; w * h];
        for sz in 0..sh {
            for sx in 0..sw {
                let c = (sz / SUB) * w + sx / SUB;
                slopes[c][(sz % SUB) * SUB + sx % SUB] = slope.get(sz * sw + sx).copied().unwrap_or(1.0);
            }
        }
        for z in 0..mh {
            for x in 0..mw {
                let c = (z / (2 * SUB)) * w + x / (2 * SUB);
                if let (Some(l), Some(&y)) = (low.get_mut(c), height.get(z * mw + x)) {
                    *l = l.min(y);
                }
            }
        }
        Grid { w, h, slopes, low }
    }

    pub fn cell(&self, p: Vec3) -> usize {
        let x = ((p.x / CELL) as i64).clamp(0, self.w as i64 - 1) as usize;
        let z = ((p.z / CELL) as i64).clamp(0, self.h as i64 - 1) as usize;
        z * self.w + x
    }

    pub fn centre(&self, c: usize) -> Vec3 {
        Vec3::xz((c % self.w) as f32 * CELL + CELL / 2.0, (c / self.w) as f32 * CELL + CELL / 2.0)
    }

    /// How much slower than on the flat a unit crosses a cell (1 or more),
    /// or `None` where it cannot go.
    fn toll(&self, c: usize, m: Mobility) -> Option<f32> {
        if self.low[c] < -m.max_depth {
            return None;
        }
        let sq = &self.slopes[c];
        let steep = sq.iter().filter(|&&s| s > m.max_slope).count() as f32;
        if steep > BLOCKED_SHARE * (SUB * SUB) as f32 {
            return None;
        }
        let mean = sq.iter().sum::<f32>() / (SUB * SUB) as f32;
        Some(1.0 + 0.5 * (mean / m.max_slope).min(1.0) + steep / (SUB * SUB) as f32)
    }

    pub fn passable(&self, p: Vec3, m: Mobility) -> bool {
        self.toll(self.cell(p), m).is_some()
    }

    /// Seconds to every cell for a unit of `m` at `speed` (elmos/s) from
    /// the nearest of `sources` (a place and a head start in seconds).
    pub fn field(&self, m: Mobility, speed: f32, sources: &[(Vec3, f32)]) -> Field {
        let n = self.w * self.h;
        let mut t = vec![NEVER; n];
        let mut heap = BinaryHeap::new();
        let speed = speed.max(1.0);
        for &(p, t0) in sources {
            // A source on a cell it cannot cross (a unit on a cliff edge)
            // still starts there.
            let c = self.cell(p);
            if t0 < t[c] {
                t[c] = t0;
                heap.push(Item(t0, c as u32));
            }
        }
        let (w, h) = (self.w as i64, self.h as i64);
        while let Some(Item(d, c)) = heap.pop() {
            let c = c as usize;
            if d > t[c] {
                continue;
            }
            let (x, z) = ((c % self.w) as i64, (c / self.w) as i64);
            for (dx, dz) in [(1, 0), (-1, 0), (0, 1), (0, -1), (1, 1), (1, -1), (-1, 1), (-1, -1)] {
                let (nx, nz) = (x + dx, z + dz);
                if nx < 0 || nz < 0 || nx >= w || nz >= h {
                    continue;
                }
                let nc = (nz * w + nx) as usize;
                let Some(toll) = self.toll(nc, m) else { continue };
                let step = if dx != 0 && dz != 0 { std::f32::consts::SQRT_2 } else { 1.0 };
                let nd = d + step * CELL * toll / speed;
                if nd < t[nc] {
                    t[nc] = nd;
                    heap.push(Item(nd, nc as u32));
                }
            }
        }
        Field { w: self.w, h: self.h, t }
    }
}

/// Seconds to each cell; see [`Grid::field`].
pub struct Field {
    w: usize,
    h: usize,
    t: Vec<f32>,
}

impl Field {
    pub fn at(&self, p: Vec3) -> f32 {
        let x = ((p.x / CELL) as i64).clamp(0, self.w as i64 - 1) as usize;
        let z = ((p.z / CELL) as i64).clamp(0, self.h as i64 - 1) as usize;
        self.t[z * self.w + x]
    }

    pub fn cell(&self, c: usize) -> f32 {
        self.t.get(c).copied().unwrap_or(NEVER)
    }

    pub fn len(&self) -> usize {
        self.t.len()
    }

    pub fn is_empty(&self) -> bool {
        self.t.is_empty()
    }

    /// The way back from `to` to the nearest source, as cells, `to` first:
    /// down the field's gradient.
    pub fn descent(&self, to: Vec3, max: usize) -> Vec<usize> {
        let mut c = {
            let x = ((to.x / CELL) as i64).clamp(0, self.w as i64 - 1) as usize;
            let z = ((to.z / CELL) as i64).clamp(0, self.h as i64 - 1) as usize;
            z * self.w + x
        };
        let mut out = vec![c];
        if !self.t[c].is_finite() {
            return out;
        }
        let (w, h) = (self.w as i64, self.h as i64);
        while out.len() < max {
            let (x, z) = ((c % self.w) as i64, (c / self.w) as i64);
            let mut best = (self.t[c], c);
            for (dx, dz) in [(1, 0), (-1, 0), (0, 1), (0, -1), (1, 1), (1, -1), (-1, 1), (-1, -1)] {
                let (nx, nz) = (x + dx, z + dz);
                if nx < 0 || nz < 0 || nx >= w || nz >= h {
                    continue;
                }
                let nc = (nz * w + nx) as usize;
                if self.t[nc] < best.0 {
                    best = (self.t[nc], nc);
                }
            }
            if best.1 == c {
                break;
            }
            c = best.1;
            out.push(c);
        }
        out
    }
}
