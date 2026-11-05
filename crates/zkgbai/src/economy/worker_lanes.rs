//! Soft reservations for expansion corridors. A new mex assignment should
//! prefer another branch over following a constructor already expanding.

use super::{Ai, Worker, WorkerTask};
use crate::util::distance;
use springai::Vec3;
use std::collections::HashSet;

const CELL: f32 = 64.0;
const TAIL: f32 = 512.0;
const LOCAL: f32 = 256.0;
const LANE_SECONDS: f32 = 12.0;

pub(super) struct LaneRoute {
    cells: HashSet<(i32, i32)>,
    tail: Vec<(i32, i32)>,
    length: f32,
}

impl LaneRoute {
    fn new(path: &[Vec3]) -> Self {
        let cell = |p: Vec3| ((p.x / CELL).floor() as i32, (p.z / CELL).floor() as i32);
        let length = path.windows(2).map(|p| distance(p[0], p[1])).sum();
        let cells = path.iter().map(|&p| cell(p)).collect();
        let mut remaining = TAIL;
        let mut tail = Vec::new();
        for (i, &p) in path.iter().enumerate().rev() {
            if remaining <= 0.0 {
                break;
            }
            // A common factory exit is not an expansion lane.
            if distance(path[0], p) >= LOCAL {
                tail.push(cell(p));
            }
            if i > 0 {
                remaining -= distance(path[i - 1], p);
            }
        }
        Self { cells, tail, length }
    }

    fn covers(&self, (x, z): (i32, i32)) -> bool {
        // Allow neighbouring paths through an open corridor to count as
        // one lane even when equal-cost grid routes use different cells.
        (-3i32..=3).any(|dx| (-3i32..=3).any(|dz| dx * dx + dz * dz <= 9 && self.cells.contains(&(x + dx, z + dz))))
    }

    fn overlap(&self, other: &Self) -> f32 {
        let (short, long) = if self.length <= other.length { (self, other) } else { (other, self) };
        if short.tail.is_empty() {
            return 0.0;
        }
        // Compare the end of the shorter route. Routes sharing an exit
        // but then forking are independent; a mex farther along the same
        // corridor is still covered by the constructor ahead of us.
        short.tail.iter().filter(|&&p| long.covers(p)).count() as f32 / short.tail.len() as f32
    }
}

impl Ai {
    pub(super) fn eco_expansion_lanes(&self, worker: &Worker) -> Vec<LaneRoute> {
        if self.eco.hard_estalled {
            return Vec::new();
        }
        let Some(routes) = worker.routes.as_ref() else { return Vec::new() };
        let range = worker.unit.def().map_or(0.0, |d| d.build_distance());
        self.eco
            .workers
            .values()
            .filter_map(|w| {
                if w.id == worker.id
                    || w.is_chicken
                    || w.assigned_plop
                    || w.raid_defense.is_some()
                    || w.unit.health() <= 0.0
                    || w.unit.team() != self.team_id
                    || w.unit.is_being_built()
                    || self.recovery_owns(w.id)
                    || self.eco_worker_held(w.id)
                    || springai::reserved::is_reserved(self.cb, w.id)
                {
                    return None;
                }
                let task = self.eco.tasks.get(w.task?)?;
                let c = task.as_construction()?;
                if c.build_type.id() != self.eco.build_ids.mex_id || c.target.is_some_and(|u| !u.is_being_built()) {
                    return None;
                }
                // Price both routes from this worker's position and with its
                // movement profile: a cliff detour must not look like a new lane.
                routes.path(task.get_pos(), range).map(|p| LaneRoute::new(&p))
            })
            .collect()
    }

    /// Only ordinary expansion choices participate. Explicit missions and
    /// project crews keep their assigned destination.
    pub(super) fn eco_lane_mex(&self, worker: &Worker, task: &WorkerTask) -> bool {
        !self.op_priority(task.id)
            && self.conquest_bonus(worker.id, task.id).is_none()
            && self.logistics_bonus(worker.id, task.id).is_none()
            && task.as_construction().is_some_and(|c| c.build_type.id() == self.eco.build_ids.mex_id)
    }

    pub(super) fn eco_expansion_lane_cost(&self, worker: &Worker, task: &WorkerTask, lanes: &[LaneRoute]) -> f32 {
        if lanes.is_empty()
            || worker.task == Some(task.id)
            || self.eco.hard_estalled
            || !self.eco_lane_mex(worker, task)
        {
            return 0.0;
        }
        let Some(def) = worker.unit.def() else { return 0.0 };
        if distance(worker.get_pos(), task.get_pos()) <= def.build_distance() {
            return 0.0;
        }
        let Some(path) = worker.routes.as_ref().and_then(|r| r.path(task.get_pos(), def.build_distance())) else {
            return 0.0;
        };
        let route = LaneRoute::new(&path);
        let overlap = lanes.iter().map(|lane| route.overlap(lane)).fold(0.0f32, f32::max);
        // Bounded and non-stacking: still take the crowded route if the
        // alternatives are much farther away. Existing threat/reachability
        // filters remain authoritative, and in-progress jobs keep their lane.
        overlap * LANE_SECONDS * def.speed() / 4.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn route(points: &[(f32, f32)]) -> LaneRoute {
        let mut path = Vec::new();
        for pair in points.windows(2) {
            let (a, b) = (Vec3::xz(pair[0].0, pair[0].1), Vec3::xz(pair[1].0, pair[1].1));
            let steps = (distance(a, b) / CELL).ceil() as usize;
            for i in 0..steps {
                let t = i as f32 / steps as f32;
                path.push(Vec3::xz(a.x + (b.x - a.x) * t, a.z + (b.z - a.z) * t));
            }
        }
        let &(x, z) = points.last().unwrap();
        path.push(Vec3::xz(x, z));
        LaneRoute::new(&path)
    }

    #[test]
    fn different_mexes_down_one_corridor_share_a_lane() {
        let claimed = route(&[(0.0, 0.0), (1200.0, 0.0)]);
        let ahead = route(&[(0.0, 0.0), (2000.0, 64.0)]);
        assert!(claimed.overlap(&ahead) > 0.9);
        assert_eq!(claimed.overlap(&ahead), ahead.overlap(&claimed));
        let separate = route(&[(0.0, 0.0), (0.0, 1400.0)]);
        assert_eq!(claimed.overlap(&separate), 0.0);
        // The 12-second preference can pay for a modest alternate walk,
        // but cannot force an arbitrarily long detour.
        let speed = 60.0;
        let crowded = 1200.0 / 4.0 + claimed.overlap(&ahead) * LANE_SECONDS * speed / 4.0;
        assert!(1400.0 / 4.0 < crowded);
        assert!(3000.0 / 4.0 > crowded);
    }

    #[test]
    fn common_exit_then_separate_branches_do_not_reserve_each_other() {
        let north = route(&[(0.0, 0.0), (800.0, 0.0), (800.0, 1000.0)]);
        let south = route(&[(0.0, 0.0), (800.0, 0.0), (800.0, -1000.0)]);
        assert_eq!(north.overlap(&south), 0.0);
        let local = route(&[(0.0, 0.0), (128.0, 0.0)]);
        assert_eq!(north.overlap(&local), 0.0);
    }

    #[test]
    fn lane_uses_the_bent_route_instead_of_bearing_from_base() {
        let claimed = route(&[(0.0, 0.0), (800.0, 0.0), (800.0, 800.0)]);
        let around_cliff = route(&[(0.0, 0.0), (800.0, 0.0), (800.0, 800.0), (-200.0, 800.0)]);
        let other_way = route(&[(0.0, 0.0), (0.0, -800.0), (-800.0, -800.0)]);
        assert!(claimed.overlap(&around_cliff) > 0.9);
        assert_eq!(claimed.overlap(&other_way), 0.0);
    }
}
