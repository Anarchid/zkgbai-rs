//! Engine-independent terrain model for Zero-K maps.
//!
//! For each movement [`Tier`] the map is decomposed by [zkta] into regions
//! joined by chokepoints. On top of that this crate provides what an AI
//! needs at query time:
//!
//! - a dense *region map* (which region contains a position), derived by
//!   flood-filling the passable area with every chokepoint's gate line drawn
//!   in as a wall — robust even where zkta's traced region polygons are
//!   approximate;
//! - static *connectivity* (connected components of passable cells), so
//!   "can units of this tier get from A to B at all" is a lookup;
//! - the region graph (regions as nodes, chokepoints as edges), with
//!   shortest ground routes over it ([`paths`]).
//!
//! On top of the model, [`strategy`] analyses the game's territory (what
//! guards what, which chokepoints to hold or wall, where bridges, drops and
//! teleports pay) and [`threat`] reasons about forces on the move (where a
//! raid can get to and when, and where defenders can block it first).
//!
//! Positions are world coordinates in elmos (`x`, `z`); internally the model
//! works on the engine's slope-map grid, one cell per 16 elmos.

mod build;
mod graph;
pub mod paths;
pub mod strategy;
#[cfg(test)]
mod testmaps;
pub mod threat;
mod tier;

pub use tier::{MoveClass, Tier, engine_max_slope};
pub use zkta::spring_map::SpringMapData;

/// Elmos per model cell (the engine's slope-map resolution).
pub const CELL: f32 = 16.0;

const NONE: u32 = u32::MAX;

/// A ground-plane position in elmos.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Pos {
    pub x: f32,
    pub z: f32,
}

impl Pos {
    pub const fn new(x: f32, z: f32) -> Self {
        Pos { x, z }
    }

    pub fn distance(self, o: Pos) -> f32 {
        ((self.x - o.x).powi(2) + (self.z - o.z).powi(2)).sqrt()
    }
}

/// A navigable area for one movement tier.
#[derive(Clone, Debug)]
pub struct Region {
    pub id: usize,
    pub center: Pos,
    /// Passable cells assigned to this region.
    pub cells: usize,
    /// Connected component (static reachability class) of the region.
    pub component: u32,
    /// Ids of chokepoints on this region's boundary.
    pub chokes: Vec<usize>,
}

/// A narrow passage between regions.
#[derive(Clone, Debug)]
pub struct Choke {
    pub id: usize,
    pub pos: Pos,
    /// Clearance (passage width) in elmos.
    pub width: f32,
    /// End points of the gate line across the passage, if known.
    pub gate: Option<[Pos; 2]>,
    /// Ids of the regions this chokepoint joins (normally two).
    pub regions: Vec<usize>,
}

/// The terrain model for one movement tier.
#[derive(Clone, Debug)]
pub struct TierMap {
    pub tier: Tier,
    /// Grid size in cells.
    pub width: usize,
    pub height: usize,
    pub regions: Vec<Region>,
    pub chokes: Vec<Choke>,
    /// Connected component per cell, `NONE` for obstacles.
    component: Vec<u32>,
    /// Region per cell, `NONE` for obstacles and tiny isolated pockets.
    region: Vec<u32>,
    /// Fraction of the map passable for this tier.
    pub passable_fraction: f32,
}

impl TierMap {
    /// Grid cell containing a world position, clamped to the map.
    pub fn cell_at(&self, p: Pos) -> usize {
        let cx = ((p.x / CELL) as isize).clamp(0, self.width as isize - 1) as usize;
        let cz = ((p.z / CELL) as isize).clamp(0, self.height as isize - 1) as usize;
        cz * self.width + cx
    }

    /// World position of a cell's center.
    pub fn cell_center(&self, cell: usize) -> Pos {
        Pos::new(((cell % self.width) as f32 + 0.5) * CELL, ((cell / self.width) as f32 + 0.5) * CELL)
    }

    pub fn is_passable(&self, p: Pos) -> bool {
        self.component[self.cell_at(p)] != NONE
    }

    /// Region of a grid cell; `None` for obstacles.
    pub fn region_of_cell(&self, cell: usize) -> Option<usize> {
        self.region.get(cell).filter(|&&r| r != NONE).map(|&r| r as usize)
    }

    /// Region containing `p`; `None` on an obstacle.
    pub fn region_at(&self, p: Pos) -> Option<usize> {
        self.region_of_cell(self.cell_at(p))
    }

    /// Region containing `p`, or of the nearest passable cell within
    /// `radius` elmos (positions of units and buildings often sit on cells
    /// the coarse grid marks as obstacles).
    pub fn region_near(&self, p: Pos, radius: f32) -> Option<usize> {
        self.nearest_passable(p, radius).and_then(|c| self.region_of_cell(c))
    }

    /// Static connectivity class of `p`; `None` on an obstacle.
    pub fn component_at(&self, p: Pos) -> Option<u32> {
        let c = self.component[self.cell_at(p)];
        (c != NONE).then_some(c)
    }

    /// Whether units of this tier can travel between `a` and `b` at all,
    /// snapping each to passable ground within `snap` elmos.
    pub fn connected(&self, a: Pos, b: Pos, snap: f32) -> bool {
        match (self.nearest_passable(a, snap), self.nearest_passable(b, snap)) {
            (Some(ca), Some(cb)) => self.component[ca] == self.component[cb],
            _ => false,
        }
    }

    /// The point `dist` elmos from a chokepoint towards the middle of one of
    /// its regions (at most halfway there): where to stand to hold it from
    /// that side.
    pub fn setback(&self, choke: usize, region: usize, dist: f32) -> Pos {
        let (c, home) = (self.chokes[choke].pos, self.regions[region].center);
        let d = c.distance(home);
        let t = if d > 0.0 { (dist / d).min(0.5) } else { 0.0 };
        Pos::new(c.x + (home.x - c.x) * t, c.z + (home.z - c.z) * t)
    }

    /// Whether ground connected to `from` (snapped to passable ground within
    /// `snap` elmos) lies within `range` elmos of `to`: whether units of
    /// this tier can get there, or close enough to strike or build.
    pub fn reaches_within(&self, from: Pos, to: Pos, range: f32, snap: f32) -> bool {
        let Some(home) = self.nearest_passable(from, snap).map(|c| self.component[c]) else { return false };
        let start = self.cell_at(to);
        if self.component[start] == home {
            return true;
        }
        let (sx, sz) = ((start % self.width) as isize, (start / self.width) as isize);
        let reach = range / CELL;
        // Rings outwards, perimeter cells only.
        for ring in 1..=reach.ceil() as isize {
            for k in -ring..=ring {
                for (dx, dz) in [(k, -ring), (k, ring), (-ring, k), (ring, k)] {
                    let (x, z) = (sx + dx, sz + dz);
                    if x < 0 || z < 0 || x >= self.width as isize || z >= self.height as isize {
                        continue;
                    }
                    if ((dx * dx + dz * dz) as f32) <= reach * reach
                        && self.component[z as usize * self.width + x as usize] == home
                    {
                        return true;
                    }
                }
            }
        }
        false
    }

    /// Neighbouring regions of `region`, with the chokepoint leading there.
    pub fn neighbours(&self, region: usize) -> impl Iterator<Item = (usize, usize)> + '_ {
        self.regions[region]
            .chokes
            .iter()
            .flat_map(move |&c| self.chokes[c].regions.iter().filter(move |&&r| r != region).map(move |&r| (c, r)))
    }

    /// Nearest passable cell to `p` within `radius` elmos (ring search).
    fn nearest_passable(&self, p: Pos, radius: f32) -> Option<usize> {
        let start = self.cell_at(p);
        if self.component[start] != NONE {
            return Some(start);
        }
        let (sx, sz) = ((start % self.width) as isize, (start / self.width) as isize);
        let max_ring = (radius / CELL).ceil() as isize;
        for ring in 1..=max_ring {
            let mut best: Option<(isize, usize)> = None;
            for dz in -ring..=ring {
                for dx in -ring..=ring {
                    if dx.abs() != ring && dz.abs() != ring {
                        continue;
                    }
                    let (x, z) = (sx + dx, sz + dz);
                    if x < 0 || z < 0 || x >= self.width as isize || z >= self.height as isize {
                        continue;
                    }
                    let c = z as usize * self.width + x as usize;
                    let d = dx * dx + dz * dz;
                    if self.component[c] != NONE && best.is_none_or(|(bd, _)| d < bd) {
                        best = Some((d, c));
                    }
                }
            }
            if let Some((_, c)) = best {
                return Some(c);
            }
        }
        None
    }
}

/// Terrain models for every requested tier.
#[derive(Clone, Debug, Default)]
pub struct Terrain {
    tiers: Vec<TierMap>,
}

impl Terrain {
    /// Analyse `data` for each tier, using Zero-K's standard movement class
    /// per tier (the expensive step; run it off the game thread).
    pub fn build(data: &SpringMapData, tiers: &[Tier]) -> Terrain {
        let classes: Vec<_> = tiers.iter().map(|&t| (t, t.zero_k_class())).collect();
        Terrain::build_with(data, &classes)
    }

    /// Analyse `data` for explicit movement classes (e.g. taken from the
    /// game's actual unit definitions).
    pub fn build_with(data: &SpringMapData, classes: &[(Tier, MoveClass)]) -> Terrain {
        Terrain { tiers: classes.iter().map(|&(t, c)| build::tier_map(data, t, c)).collect() }
    }

    pub fn tier(&self, tier: Tier) -> Option<&TierMap> {
        self.tiers.iter().find(|m| m.tier == tier)
    }

    pub fn tiers(&self) -> &[TierMap] {
        &self.tiers
    }
}

#[cfg(test)]
mod tests {
    use crate::testmaps::grid_map;
    use crate::{CELL, Pos};

    #[test]
    fn reaching_across_a_gap_takes_range() {
        // Two plateaus four cells apart.
        let map = grid_map(&["aaaaaaaa####bbbbbbbb"; 4], &[]);
        let at = |x: f32| Pos::new((x + 0.5) * CELL, 1.5 * CELL);
        let (home, far_edge, deep) = (at(2.0), at(12.0), at(18.0));
        assert!(map.reaches_within(home, at(6.0), 0.0, 16.0));
        // The near edge of b is 5 cells from a's last column.
        assert!(!map.reaches_within(home, far_edge, 3.0 * CELL, 16.0));
        assert!(map.reaches_within(home, far_edge, 5.0 * CELL, 16.0));
        assert!(!map.reaches_within(home, deep, 5.0 * CELL, 16.0));
    }
}
