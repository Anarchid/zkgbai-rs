//! A builder unit (constructor or commander).

use springai::{Unit, Vec3, Weapon};

use crate::economy::TaskId;
use crate::economy::tasks::{TaskKind, TaskStore};
use crate::kgbutil::{Nav, PathCost, kgb_move_to};
use crate::util::{OPTION_SHIFT_KEY, distance, get_angular_point, get_directional_point, get_speed};

/// A worker's way to its own task out of build range (see `approach` in
/// `assign`): the order sent, the nearest it has got, and when it last got
/// nearer or travelled (from `anchor`, where it stood then).
#[derive(Clone, Copy, Debug)]
pub struct Approach {
    pub task: TaskId,
    /// The command the order's queue ends in (a build: minus the def id).
    pub cmd: i32,
    pub sent: i32,
    pub best: f32,
    pub gained: i32,
    pub anchor: Vec3,
}

pub struct Worker {
    pub id: i32,
    pub unit: Unit,
    pub is_com: bool,
    pub assigned_plop: bool,
    pub is_greedy: bool,
    pub power: f32,
    pub bp: f32,
    pub build_range: i32,
    pub has_shields: bool,
    pub is_chicken: bool,
    pub chicken_frame: i32,
    pub last_retreat_frame: i32,
    pub task: Option<TaskId>,
    pub approach: Option<Approach>,
    /// Sites this worker could not get to, shunned until a frame.
    pub shunned: Vec<(Vec3, i32)>,
    shield: Option<Weapon>,
    last_task_frame: i32,
    last_stuck_frame: i32,
    team: i32,
}

impl Worker {
    /// Wrap a builder owned by `team`.
    pub fn new(unit: Unit, team: i32) -> Self {
        let ud = unit.def();
        let bp = ud.map_or(0.0, |d| d.build_speed());
        let build_range = ud.map_or(0, |d| (d.build_distance() / 64.0).round() as i32);

        let mut shield = None;
        for w in unit.weapons() {
            if w.def().is_some_and(|wd| wd.shield_power() > 0.0) {
                shield = Some(w);
            }
        }

        let mut power = 0.0;
        if let Some(ud) = ud {
            if ud.name().contains("dyn") {
                power = 155.0;
            } else {
                for wd in ud.weapon_mounts().iter().filter_map(|m| m.weapon_def()) {
                    let name = wd.name();
                    if name.contains("torpedo") || name.contains("fake") {
                        continue;
                    }
                    // take the max between burst damage and continuous dps as weapon power.
                    let base = wd.damage_types().first().copied().unwrap_or(0.0);
                    let mut wep_shot = base * wd.projectiles_per_shot() as f32 * wd.salvo_size() as f32;
                    // The original iterates a std::map, i.e. sorted by key.
                    let mut params = wd.custom_params();
                    params.sort();
                    for (k, v) in params {
                        let v: f32 = v.parse().unwrap_or(0.0);
                        match k.as_str() {
                            "extra_damage" => wep_shot += v,                             // emp
                            "impulse" => wep_shot += v / 10.0,                           // impulse
                            "timeslow_damagefactor" => wep_shot += (wep_shot * v) / 2.0, // slow
                            "disarmDamageOnly" => wep_shot /= 10.0,
                            "setunitsonfire" => wep_shot *= 2.0,
                            _ => {}
                        }
                    }
                    wep_shot *= 1.0 + wd.area_of_effect() / 100.0;
                    let reload = wd.reload();
                    // port: guard the division (the original divides unchecked).
                    let dps = if reload > 0.0 { wep_shot / reload } else { wep_shot };
                    power += wep_shot.max(dps);
                }
            }
        }

        Worker {
            id: unit.id(),
            unit,
            is_com: false,
            assigned_plop: false,
            is_greedy: false,
            power,
            bp,
            build_range,
            has_shields: shield.is_some(),
            is_chicken: false,
            chicken_frame: 0,
            last_retreat_frame: 0,
            task: None,
            approach: None,
            shunned: Vec::new(),
            shield,
            last_task_frame: 0,
            last_stuck_frame: -32768,
            team,
        }
    }

    pub fn get_unit(&self) -> Unit {
        self.unit
    }

    pub fn get_pos(&self) -> Vec3 {
        self.unit.pos()
    }

    /// Assign this worker to task `id` (and the task to this worker).
    pub fn set_task(&mut self, tasks: &mut TaskStore, id: TaskId, frame: i32) {
        self.task = Some(id);
        if let Some(t) = tasks.get_mut(id) {
            t.add_worker(self.id);
        }
        self.last_task_frame = frame;
    }

    /// Leave the current task and stop.
    pub fn clear_task(&mut self, tasks: &mut TaskStore) {
        if let Some(t) = self.task.take().and_then(|id| tasks.get_mut(id)) {
            t.remove_worker(self.id);
        }
        if self.unit.health() > 0.0 && self.unit.team() == self.team {
            self.unit.stop(0);
        }
    }

    /// The task ended (it clears its own worker list): forget it and stop.
    pub fn end_task(&mut self) {
        self.task = None;
        if self.unit.health() > 0.0 {
            self.unit.stop(0);
        }
    }

    /// If the worker seems stuck on its task, back off and drop the task.
    /// Returns whether the worker is (recently) unstuck and should be left alone.
    pub fn unstick(&mut self, frame: i32, tasks: &mut TaskStore) -> bool {
        let mut unstuck = false;
        if frame - self.last_stuck_frame < 60 {
            return true;
        }
        let Some(task_pos) = self.task.and_then(|id| tasks.get(id)).map(|t| t.get_pos()) else {
            return false;
        };
        if frame - self.last_task_frame < 120 {
            return false;
        }

        let pos = self.unit.pos();
        let jobdist = distance(pos, task_pos);
        let (speed, build_dist) = self.unit.def().map_or((0.0, 0.0), |d| (d.speed(), d.build_distance()));

        if (get_speed(self.unit) < speed / 10.0 && jobdist > build_dist * 1.1) || self.unit.num_current_commands() == 0
        {
            let unstick_point = get_angular_point(task_pos, pos, distance(task_pos, pos) + 150.0);
            kgb_move_to(self.unit, unstick_point, 0, frame);
            self.clear_task(tasks);
            unstuck = true;
            self.last_stuck_frame = frame;
        }
        self.last_task_frame = frame;
        unstuck
    }

    /// Path to build range of `pos`, avoiding enemies.
    pub fn move_to(&self, pos: Vec3, nav: &mut Nav) {
        let frame = nav.frame;
        let build_dist = self.unit.def().map_or(0.0, |d| d.build_distance());
        let mut path = nav.pathfinder.find_path(nav.war, self.unit, pos, PathCost::AvoidEnemies);
        match path.front() {
            Some(&first) if distance(pos, first) >= build_dist => {
                // skip first few waypoints if target actually found to prevent stuttering, otherwise use the first waypoint.
                kgb_move_to(self.unit, first, 0, frame);
                path.pop_front();
            }
            // port: an empty path (C++: UB on `front()`) is treated like a nearby target.
            _ => {
                kgb_move_to(self.unit, get_directional_point(pos, self.unit.pos(), build_dist * 0.95), 0, frame);
                return;
            }
        }

        let mut i = 0;
        while let Some(&next) = path.front() {
            if distance(pos, next) < build_dist || i >= 2 {
                break;
            }
            kgb_move_to(self.unit, next, OPTION_SHIFT_KEY, frame);
            path.pop_front();
            i += 1;
        }
    }

    /// Retreat along a path that avoids enemies.
    pub fn retreat_to(&mut self, pos: Vec3, nav: &mut Nav) {
        let frame = nav.frame;
        self.last_retreat_frame = frame;
        let mut path = nav.pathfinder.find_path(nav.war, self.unit, pos, PathCost::AvoidEnemies);
        let Some(first) = path.pop_front() else {
            return;
        };

        // skip first few waypoints if target actually found to prevent stuttering, otherwise use the first waypoint.
        kgb_move_to(self.unit, first, 0, frame);
        while !path.is_empty() {
            if path.len() > 1 {
                path.pop_front();
            }
            if let Some(p) = path.pop_front() {
                kgb_move_to(self.unit, p, OPTION_SHIFT_KEY, frame);
            }
        }
    }

    pub fn can_reach(&self, pos: Vec3, nav: &mut Nav) -> bool {
        nav.pathfinder.is_worker_reachable(nav.war, self.unit, pos)
    }

    /// Shield charge fraction (1 without a shield).
    pub fn get_shields(&self) -> f32 {
        let Some(shield) = self.shield.filter(|_| self.has_shields) else {
            return 1.0;
        };
        let max = shield.def().map_or(0.0, |d| d.shield_power());
        if max > 0.0 { shield.shield_power() / max } else { 1.0 }
    }

    /// Whether this commander still has its free factory.
    pub fn has_plop(&self) -> bool {
        self.unit.rules_param_float("facplop", 0.0) == 1.0
    }

    /// Whether the current task builds or repairs a static defense.
    pub fn is_working_on_porc(&self, tasks: &TaskStore) -> bool {
        let Some(task) = self.task.and_then(|id| tasks.get(id)) else {
            return false;
        };
        let ud = match &task.kind {
            TaskKind::Construction(c) => Some(c.build_type),
            TaskKind::Repair(r) => r.target.def(),
            _ => None,
        };
        ud.is_some_and(|ud| ud.speed() == 0.0 && ud.is_able_to_attack())
    }
}
