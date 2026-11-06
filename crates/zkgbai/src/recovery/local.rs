//! Small footprint-expanded occupancy search, used only for stalled units.
//! Unlike strategic terrain connectivity, this includes live obstacles.

use std::{
    cmp::Reverse,
    collections::{BTreeMap, BinaryHeap, VecDeque},
};

use springai::Vec3;

use crate::util::distance;

pub(super) const CELL: f32 = 16.0;
const HALF: usize = 24;
const SIDE: usize = 2 * HALF + 1;
pub(super) const RADIUS: f32 = HALF as f32 * CELL;

#[checkpoint::state]
pub(super) struct Obstacle {
    pub id: i32,
    pub at: Vec3,
    pub hx: f32,
    pub hz: f32,
    pub mobile: bool,
}

#[checkpoint::state]
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
        self.path(goal, ignore, mobiles, None)
    }

    pub fn within(&self, goal: Vec3, radius: f32) -> Option<Vec<Vec3>> {
        self.path(goal, None, false, Some(radius))
    }

    fn path(&self, goal: Vec3, ignore: Option<i32>, mobiles: bool, within: Option<f32>) -> Option<Vec<Vec3>> {
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
            if within.map_or(x == 0 || z == 0 || x == SIDE - 1 || z == SIDE - 1, |r| {
                distance(Self::point(self.origin, i), goal) < r
            }) {
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
    /// A bounded corridor through expendable static obstacles. Cell costs prefer
    /// thin, cheap cuts; the returned unique blockers are independently budgeted
    /// by the caller. Non-candidates (including allied buildings) remain walls.
    pub fn clearance(&self, goal: Vec3, candidates: &BTreeMap<i32, u32>) -> Option<Vec<i32>> {
        let start = HALF * SIDE + HALF;
        let mut cost = vec![None; SIDE * SIDE];
        for (i, cell) in cost.iter_mut().enumerate() {
            if !self.terrain[i] {
                continue;
            }
            let p = Self::point(self.origin, i);
            let mut c = 1;
            let mut legal = true;
            for o in &self.obstacles {
                if !o.mobile && (p.x - o.at.x).abs() < o.hx && (p.z - o.at.z).abs() < o.hz {
                    if let Some(price) = candidates.get(&o.id) {
                        c += 100 * price;
                    } else {
                        legal = false;
                        break;
                    }
                }
            }
            if legal {
                *cell = Some(c);
            }
        }
        let mut best = vec![u32::MAX; SIDE * SIDE];
        let mut parent = vec![usize::MAX; SIDE * SIDE];
        best[start] = 0;
        let mut queue = BinaryHeap::from([Reverse((0, start))]);
        while let Some(Reverse((c, i))) = queue.pop() {
            if c != best[i] {
                continue;
            }
            let (x, z) = (i % SIDE, i / SIDE);
            for (dx, dz) in [(0, -1), (-1, 0), (1, 0), (0, 1)] {
                let (nx, nz) = (x as i32 + dx, z as i32 + dz);
                if nx < 0 || nz < 0 || nx >= SIDE as i32 || nz >= SIDE as i32 {
                    continue;
                }
                let next = nz as usize * SIDE + nx as usize;
                if let Some(step) = cost[next]
                    && c + step < best[next]
                {
                    best[next] = c + step;
                    parent[next] = i;
                    queue.push(Reverse((c + step, next)));
                }
            }
        }
        let mut at = (0..SIDE * SIDE)
            .filter(|&i| {
                (i % SIDE == 0 || i % SIDE == SIDE - 1 || i / SIDE == 0 || i / SIDE == SIDE - 1) && best[i] != u32::MAX
            })
            .min_by(|&a, &b| {
                best[a].cmp(&best[b]).then_with(|| {
                    distance(Self::point(self.origin, a), goal).total_cmp(&distance(Self::point(self.origin, b), goal))
                })
            })?;
        let mut path = Vec::new();
        while at != start {
            path.push(at);
            at = parent[at];
        }
        let mut blockers = Vec::new();
        for i in path.into_iter().rev() {
            let p = Self::point(self.origin, i);
            for o in &self.obstacles {
                if !o.mobile && (p.x - o.at.x).abs() < o.hx && (p.z - o.at.z).abs() < o.hz && !blockers.contains(&o.id)
                {
                    blockers.push(o.id);
                }
            }
        }
        (!blockers.is_empty() && blockers.len() <= 3 && blockers.iter().map(|id| candidates[id]).sum::<u32>() <= 150)
            .then_some(blockers)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dense_wind_corridor_requires_multiple_removals() {
        let wall = |id, x| Obstacle { id, at: Vec3::xz(x, 0.0), hx: 20.0, hz: 500.0, mobile: false };
        let grid = Local::new(
            Vec3::xz(0.0, 0.0),
            |p| p.z.abs() < 32.0,
            vec![wall(9, -64.0), wall(1, 64.0), wall(2, 112.0), wall(3, 160.0)],
        );
        let goal = Vec3::xz(500.0, 0.0);
        for id in 1..=3 {
            assert!(grid.escape(goal, Some(id), false).is_none());
        }
        assert_eq!(grid.clearance(goal, &BTreeMap::from([(1, 35), (2, 35), (3, 35)])), Some(vec![1, 2, 3]));
        // One protected/allied building makes this corridor unavailable.
        assert_eq!(grid.clearance(goal, &BTreeMap::from([(1, 35), (3, 35)])), None);
        assert_eq!(grid.clearance(goal, &BTreeMap::from([(1, 70), (2, 70), (3, 70)])), None);
        assert_eq!(grid.clearance(goal, &BTreeMap::from([(9, 35)])), Some(vec![9]));
    }

    #[test]
    fn reclaim_approach_stops_in_range_without_crossing_a_wall() {
        let wall = Obstacle { id: 1, at: Vec3::xz(64.0, 0.0), hx: 24.0, hz: 500.0, mobile: false };
        let grid = Local::new(Vec3::xz(0.0, 0.0), |_| true, vec![wall]);
        let path = grid.within(Vec3::xz(64.0, 100.0), 60.0).unwrap();
        assert!(path.iter().all(|p| p.x < 40.0));
        assert!(distance(*path.last().unwrap(), Vec3::xz(64.0, 100.0)) < 60.0);
        assert!(grid.within(Vec3::xz(200.0, 0.0), 60.0).is_none());
    }

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
