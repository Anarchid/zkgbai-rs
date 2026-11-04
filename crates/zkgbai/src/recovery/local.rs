//! Small footprint-expanded occupancy search, used only for stalled units.
//! Unlike strategic terrain connectivity, this includes live obstacles.

use std::collections::VecDeque;

use springai::Vec3;

use crate::util::distance;

pub(super) const CELL: f32 = 16.0;
const HALF: usize = 24;
const SIDE: usize = 2 * HALF + 1;
pub(super) const RADIUS: f32 = HALF as f32 * CELL;

pub(super) struct Obstacle {
    pub id: i32,
    pub at: Vec3,
    pub hx: f32,
    pub hz: f32,
    pub mobile: bool,
}

pub(super) struct Local {
    origin: Vec3,
    terrain: Vec<bool>,
    obstacles: Vec<Obstacle>,
}

impl Local {
    pub fn new(origin: Vec3, passable: impl Fn(Vec3) -> bool, obstacles: Vec<Obstacle>) -> Self {
        let terrain = (0..SIDE * SIDE).map(|i| passable(Self::point(origin, i))).collect();
        Self { origin, terrain, obstacles }
    }

    fn point(origin: Vec3, i: usize) -> Vec3 {
        Vec3::xz(origin.x + (i % SIDE) as f32 * CELL - RADIUS, origin.z + (i / SIDE) as f32 * CELL - RADIUS)
    }

    /// Path out of this neighbourhood; ignoring one obstacle is a causal
    /// check before moving/reclaiming it. Never cut diagonally through corners.
    pub fn escape(&self, goal: Vec3, ignore: Option<i32>, mobiles: bool) -> Option<Vec<Vec3>> {
        let start = HALF * SIDE + HALF;
        let mut free = self.terrain.clone();
        for (i, cell) in free.iter_mut().enumerate() {
            if !*cell {
                continue;
            }
            let p = Self::point(self.origin, i);
            *cell = !self.obstacles.iter().any(|o| {
                Some(o.id) != ignore
                    && (mobiles || !o.mobile)
                    && (p.x - o.at.x).abs() < o.hx
                    && (p.z - o.at.z).abs() < o.hz
            });
        }
        // Units can start partly overlapping a blocker. Allow their actual
        // occupied cell, but never snap the start through a wall.
        free[start] = true;
        let mut parent = vec![usize::MAX; SIDE * SIDE];
        parent[start] = start;
        let mut queue = VecDeque::from([start]);
        let mut exits = Vec::new();
        while let Some(i) = queue.pop_front() {
            let (x, z) = (i % SIDE, i / SIDE);
            if x == 0 || z == 0 || x == SIDE - 1 || z == SIDE - 1 {
                exits.push(i);
            }
            for (dx, dz) in [(0, -1), (-1, 0), (1, 0), (0, 1)] {
                let (nx, nz) = (x as i32 + dx, z as i32 + dz);
                if nx < 0 || nz < 0 || nx >= SIDE as i32 || nz >= SIDE as i32 {
                    continue;
                }
                let next = nz as usize * SIDE + nx as usize;
                if free[next] && parent[next] == usize::MAX {
                    parent[next] = i;
                    queue.push_back(next);
                }
            }
        }
        let mut at = exits.into_iter().min_by(|&a, &b| {
            distance(Self::point(self.origin, a), goal).total_cmp(&distance(Self::point(self.origin, b), goal))
        })?;
        let mut path = Vec::new();
        while at != start {
            path.push(Self::point(self.origin, at));
            at = parent[at];
        }
        path.reverse();
        Some(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reclaim_requires_opening_an_exit_not_merely_a_nearby_obstacle() {
        let wall = |id, x| Obstacle { id, at: Vec3::xz(x, 0.0), hx: 16.0, hz: 500.0, mobile: false };
        let grid =
            Local::new(Vec3::xz(0.0, 0.0), |p| p.z.abs() < 32.0, vec![wall(1, -64.0), wall(2, 64.0), wall(3, 96.0)]);
        assert!(grid.escape(Vec3::xz(300.0, 0.0), None, true).is_none());
        assert!(grid.escape(Vec3::xz(300.0, 0.0), Some(2), true).is_none());
        assert!(grid.escape(Vec3::xz(300.0, 0.0), Some(1), true).is_some());
    }

    #[test]
    fn traffic_and_terrain_are_distinct() {
        let crowd = Obstacle { id: 1, at: Vec3::xz(0.0, 0.0), hx: 48.0, hz: 48.0, mobile: true };
        let grid = Local::new(Vec3::xz(0.0, 0.0), |_| true, vec![crowd]);
        assert!(grid.escape(Vec3::xz(300.0, 0.0), None, true).is_none());
        assert!(grid.escape(Vec3::xz(300.0, 0.0), None, false).is_some());
        let pocket = Local::new(Vec3::xz(0.0, 0.0), |p| p.x.abs() < 100.0 && p.z.abs() < 100.0, vec![]);
        assert!(pocket.escape(Vec3::xz(300.0, 0.0), None, false).is_none());
    }

    #[test]
    fn start_is_not_snapped_across_a_wall() {
        let grid = Local::new(Vec3::xz(0.0, 0.0), |p| p.x.abs() > CELL || p.z.abs() > CELL, vec![]);
        assert!(grid.escape(Vec3::xz(300.0, 0.0), None, false).is_none());
    }
}
