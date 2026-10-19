//! A mobile combat unit under AI control.
//!
//! The original's `Fighter`, `Raider`, `Strider` and `Krow` classes are
//! flattened into one type with a [`FighterKind`] tag; kind-specific methods
//! document which kinds they apply to.

use std::collections::{BTreeMap, VecDeque};

use springai::{Unit, Vec3};

use crate::kgbutil::{Nav, PathCost, kgb_move_to};
use crate::military::wrappers::weapon_power;
use crate::military::{ScoutTaskId, ScoutTasks, SquadId};
use crate::util::{
    NULLPOS, OPTION_SHIFT_KEY, distance, get_angular_point, get_directional_point, get_radial_point, get_speed,
};

/// All fighters by unit id.
pub type FighterStore = BTreeMap<i32, Fighter>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FighterKind {
    Basic,
    Raider,
    Strider,
    Krow,
}

pub struct Fighter {
    pub id: i32,
    pub unit: Unit,
    pub kind: FighterKind,
    pub metal_value: f32,
    pub index: i32,
    pub target: Vec3,
    pub squad: Option<SquadId>,
    pub shield_mob: bool,
    pub out_of_range: bool,
    pub power: f32,
    /// Raiders: current scout task.
    pub task: Option<ScoutTaskId>,
    /// Raiders.
    pub last_task_frame: i32,
    /// Raiders.
    pub last_rotation_frame: i32,
    stuck: i32,
    team: i32,
}

/// Skip up to `skip` leading waypoints (always keeping at least one), then
/// take the next one. Waypoints from the pathfinder are close together, so
/// the original thins them out before issuing orders.
pub(crate) fn next_waypoint(path: &mut VecDeque<Vec3>, skip: usize) -> Option<Vec3> {
    for _ in 0..skip {
        if path.len() > 1 {
            path.pop_front();
        }
    }
    path.pop_front()
}

impl Fighter {
    /// A plain fighter. `metal` is its metal cost.
    pub fn new(unit: Unit, metal: f32, team: i32) -> Self {
        // Sum of sustained weapon damage, ignoring torpedoes and fake weapons.
        let mut power = 0.0;
        if let Some(def) = unit.def() {
            for wd in def.weapon_mounts().iter().filter_map(|w| w.weapon_def()) {
                let name = wd.name();
                if !name.contains("torpedo") && !name.contains("fake") {
                    power += weapon_power(wd, false);
                }
            }
        }
        Fighter {
            id: unit.id(),
            unit,
            kind: FighterKind::Basic,
            metal_value: metal,
            index: 0,
            target: NULLPOS,
            squad: None,
            shield_mob: false,
            out_of_range: false,
            power,
            task: None,
            last_task_frame: 0,
            last_rotation_frame: 0,
            stuck: 0,
            team,
        }
    }

    pub fn new_raider(unit: Unit, metal: f32, team: i32) -> Self {
        Fighter { kind: FighterKind::Raider, ..Fighter::new(unit, metal, team) }
    }

    pub fn new_strider(unit: Unit, metal: f32, team: i32) -> Self {
        Fighter { kind: FighterKind::Strider, ..Fighter::new(unit, metal, team) }
    }

    pub fn new_krow(unit: Unit, metal: f32, team: i32) -> Self {
        Fighter { kind: FighterKind::Krow, ..Fighter::new(unit, metal, team) }
    }

    pub fn get_unit(&self) -> Unit {
        self.unit
    }

    pub fn get_pos(&self) -> Vec3 {
        self.unit.pos()
    }

    fn is_able_to_fly(&self) -> bool {
        self.unit.def().is_some_and(|d| d.is_able_to_fly())
    }

    /// Frame at which the last of the unit's weapons is reloaded.
    fn last_weapon_frame(&self) -> i32 {
        self.unit.weapons().iter().map(|w| w.reload_frame()).fold(0, i32::max)
    }

    /// Not shooting and barely moving although it should be.
    fn seems_stuck(&self, frame: i32) -> bool {
        let max_speed = self.unit.def().map_or(0.0, |d| d.speed());
        self.last_weapon_frame() < frame - 30 && get_speed(self.unit) < max_speed / 10.0 && !self.unit.is_paralyzed()
    }

    pub fn get_fight_path(&self, pos: Vec3, nav: &mut Nav) -> VecDeque<Vec3> {
        nav.pathfinder.find_path(nav.war, self.unit, pos, PathCost::AssaultPath)
    }

    /// Strider-kind fighters use strider pathing and standoff distances.
    pub fn fight_to(&mut self, pos: Vec3, nav: &mut Nav) {
        match self.kind {
            FighterKind::Strider | FighterKind::Krow => self.strider_fight_to(pos, nav),
            FighterKind::Basic | FighterKind::Raider => {
                let mut path =
                    nav.pathfinder.find_path(nav.war, self.unit, get_radial_point(pos, 150.0), PathCost::AssaultPath);
                // Skip every other waypoint since they're close together.
                let skip = if self.is_able_to_fly() { 2 } else { 1 };
                let Some(wp) = next_waypoint(&mut path, skip) else { return };
                self.unit.fight(wp, 0);
                // Add one extra waypoint.
                if let Some(wp) = next_waypoint(&mut path, skip) {
                    self.unit.fight(wp, OPTION_SHIFT_KEY);
                }
            }
        }
    }

    pub fn move_to(&mut self, pos: Vec3, nav: &mut Nav) {
        let mut path =
            nav.pathfinder.find_path(nav.war, self.unit, get_radial_point(pos, 100.0), PathCost::AvoidEnemies);
        // Skip every other waypoint since they're close together.
        let skip = if self.is_able_to_fly() { 2 } else { 1 };
        let Some(wp) = next_waypoint(&mut path, skip) else { return };
        kgb_move_to(self.unit, wp, 0, nav.frame);
        // Add one extra waypoint.
        if let Some(wp) = next_waypoint(&mut path, skip) {
            kgb_move_to(self.unit, wp, OPTION_SHIFT_KEY, nav.frame);
        }
    }

    pub fn is_stuck(&mut self, frame: i32) -> bool {
        if self.unit.health() <= 0.0 || self.target == NULLPOS {
            return false;
        }
        if self.seems_stuck(frame) {
            self.stuck += 1;
            if self.stuck >= 10 {
                let pos = self.unit.pos();
                let away = get_angular_point(self.target, pos, distance(self.target, pos) + 100.0);
                kgb_move_to(self.unit, away, 0, frame);
            }
            if self.stuck == 15 {
                return true;
            }
        } else {
            self.stuck = (self.stuck - 2).max(0);
        }
        false
    }

    pub fn is_dead(&self) -> bool {
        self.unit.health() <= 0.0 || self.unit.team() != self.team
    }

    // ── Raider ──────────────────────────────────────────────────────────

    /// Assign a scout task, leaving any previous one.
    ///
    /// Also registers the raider with the new task (the original left that to
    /// the caller via `ScoutTask::addRaider`; adding is idempotent).
    pub fn set_task(&mut self, task: ScoutTaskId, tasks: &mut ScoutTasks) {
        if let Some(t) = self.task.and_then(|old| tasks.get_mut(&old)) {
            t.remove_raider(self.id);
        }
        self.task = Some(task);
        if let Some(t) = tasks.get_mut(&task) {
            t.add_raider(self.id);
        }
    }

    /// Leave the current scout task and stop.
    pub fn clear_task(&mut self, tasks: &mut ScoutTasks) {
        if let Some(t) = self.task.and_then(|old| tasks.get_mut(&old)) {
            t.remove_raider(self.id);
        }
        self.end_task();
    }

    /// Forget the current task (the task itself is being ended) and stop.
    pub fn end_task(&mut self) {
        if self.unit.health() > 0.0 && self.unit.team() == self.team {
            self.unit.stop(0);
        }
        self.task = None;
    }

    pub fn get_raid_path(&self, pos: Vec3, nav: &mut Nav) -> VecDeque<Vec3> {
        let cost = if self.unit.is_cloaked() { PathCost::ScythePath } else { PathCost::RaiderPath };
        nav.pathfinder.find_path(nav.war, self.unit, pos, cost)
    }

    pub fn raid(&mut self, target: Vec3, nav: &mut Nav) {
        self.target = target;
        let mut path = self.get_raid_path(get_radial_point(target, 100.0), nav);
        let flier = self.is_able_to_fly();
        let Some(wp) = next_waypoint(&mut path, if flier { 3 } else { 1 }) else { return };
        self.unit.fight(wp, 0);
        if let Some(wp) = next_waypoint(&mut path, if flier { 2 } else { 0 }) {
            self.unit.fight(wp, OPTION_SHIFT_KEY);
        }
    }

    pub fn sneak(&mut self, target: Vec3, nav: &mut Nav) {
        self.target = target;
        let mut path = self.get_raid_path(get_radial_point(target, 100.0), nav);
        let Some(wp) = next_waypoint(&mut path, 1) else { return };
        kgb_move_to(self.unit, wp, 0, nav.frame);
        if let Some(wp) = path.front() {
            kgb_move_to(self.unit, *wp, OPTION_SHIFT_KEY, nav.frame);
        }
    }

    pub fn is_reloading(&self, frame: i32) -> bool {
        self.last_weapon_frame() > frame
    }

    /// Returns `true` once the raider has been stuck for good.
    pub fn unstick(&mut self, frame: i32, tasks: &mut ScoutTasks) -> bool {
        if self.unit.health() <= 0.0 {
            return false;
        }
        if self.seems_stuck(frame) {
            self.stuck += 1;
            if self.stuck == 15 {
                return true;
            }
            self.clear_task(tasks);
            kgb_move_to(self.unit, get_radial_point(self.unit.pos(), 100.0), 0, frame);
        } else {
            self.stuck = (self.stuck - 2).max(0);
        }
        false
    }

    // ── Strider / Krow ──────────────────────────────────────────────────

    pub fn is_dgun_ready(&self, frame: i32) -> bool {
        // Only the first manual-fire weapon counts.
        self.unit
            .weapons()
            .into_iter()
            .find(|w| w.def().is_some_and(|d| d.is_manual_fire()))
            .is_some_and(|w| w.reload_frame() <= frame)
    }

    /// Krow only.
    pub fn is_dgun_ready_or_firing(&self, frame: i32) -> bool {
        self.unit
            .weapons()
            .into_iter()
            .find(|w| w.def().is_some_and(|d| d.is_manual_fire()))
            .is_some_and(|w| w.reload_frame() <= frame || w.reload_frame() - frame > 720)
    }

    fn strider_fight_to(&mut self, pos: Vec3, nav: &mut Nav) {
        let standoff = match self.unit.def().map(|d| d.name()).as_deref() {
            Some("striderscorpion") => 250.0,
            Some("striderbantha" | "striderdetriment") => 350.0,
            _ => 50.0, // dante
        };
        let tpos = get_directional_point(pos, self.get_pos(), standoff);
        let mut path = nav.pathfinder.find_path(nav.war, self.unit, tpos, PathCost::StriderPath);
        // Skip every other waypoint except the last, since they're not very far apart.
        let Some(wp) = next_waypoint(&mut path, 1) else { return };
        kgb_move_to(self.unit, wp, 0, nav.frame);
        if let Some(wp) = next_waypoint(&mut path, 1) {
            kgb_move_to(self.unit, wp, OPTION_SHIFT_KEY, nav.frame);
        }
    }

    /// Krow only.
    pub fn fly_to(&mut self, pos: Vec3, nav: &mut Nav) {
        let my_pos = self.get_pos();
        if distance(pos, my_pos) > 1200.0 {
            let mut path = nav.pathfinder.find_path(nav.war, self.unit, pos, PathCost::AssaultPath);
            // Skip the first waypoints since flying units move pretty quick and
            // the waypoints are close together.
            let Some(wp) = next_waypoint(&mut path, 2) else { return };
            kgb_move_to(self.unit, wp, 0, nav.frame);
            if let Some(wp) = next_waypoint(&mut path, 2) {
                kgb_move_to(self.unit, wp, OPTION_SHIFT_KEY, nav.frame);
            }
        } else if self.is_dgun_ready_or_firing(nav.frame) {
            let target = get_directional_point(my_pos, pos, distance(pos, my_pos) + 300.0);
            kgb_move_to(self.unit, target, 0, nav.frame);
        } else {
            kgb_move_to(self.unit, get_directional_point(pos, my_pos, 375.0), 0, nav.frame);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn path(n: usize) -> VecDeque<Vec3> {
        (0..n).map(|i| Vec3::xz(i as f32, 0.0)).collect()
    }

    #[test]
    fn next_waypoint_skips_but_keeps_last() {
        let mut p = path(5);
        assert_eq!(next_waypoint(&mut p, 1), Some(Vec3::xz(1.0, 0.0)));
        assert_eq!(next_waypoint(&mut p, 1), Some(Vec3::xz(3.0, 0.0)));
        assert_eq!(next_waypoint(&mut p, 1), Some(Vec3::xz(4.0, 0.0)));
        assert_eq!(next_waypoint(&mut p, 1), None);

        let mut p = path(2);
        assert_eq!(next_waypoint(&mut p, 3), Some(Vec3::xz(1.0, 0.0)));
        assert!(p.is_empty());

        let mut p = path(1);
        assert_eq!(next_waypoint(&mut p, 2), Some(Vec3::xz(0.0, 0.0)));
        assert_eq!(next_waypoint(&mut VecDeque::new(), 2), None);
    }

    #[test]
    fn next_waypoint_matches_flier_raid_sequence() {
        // Raider::raid for a flier: pop, pop, pop, take; pop, pop, take.
        let mut p = path(8);
        assert_eq!(next_waypoint(&mut p, 3), Some(Vec3::xz(3.0, 0.0)));
        assert_eq!(next_waypoint(&mut p, 2), Some(Vec3::xz(6.0, 0.0)));
    }
}
