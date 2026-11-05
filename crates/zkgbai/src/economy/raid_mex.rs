//! A worker remembers seeing a raider and requests normal sparse mexporc
//! for mexes it subsequently builds. No team-wide inference or decay.

use springai::{Unit, UnitDef, Vec3};

use crate::{Ai, util::distance};

impl Ai {
    /// LOS events arrive before MilitaryManager has identified the enemy.
    pub(super) fn eco_observe_raider(&mut self, enemy: Unit, def: UnitDef) {
        // Same scout/raider classification as Enemy::update_from_unit_def.
        let tooltip = def.tooltip();
        if tooltip.contains("aider") || tooltip.contains("cout") {
            self.eco_workers_see_raider(enemy.pos(), def.is_able_to_fly());
        }
    }

    fn eco_workers_see_raider(&mut self, at: Vec3, flying: bool) {
        for w in self.eco.workers.values_mut().filter(|w| !w.seen_raider) {
            let Some(def) = w.unit.def() else { continue };
            let sight = if flying { def.air_los_radius() } else { def.los_radius() };
            // The engine exposes allied LOS, not which unit supplied it.
            // A visible contact inside this worker's sight is its encounter.
            if sight > 0.0 && distance(w.get_pos(), at) <= sight {
                w.seen_raider = true;
            }
        }
    }

    pub(super) fn eco_observe_raiders(&mut self) {
        // A raider may enter a worker's sight while already in allied LOS.
        let contacts: Vec<_> = self
            .war
            .targets
            .values()
            .filter(|e| e.visible && e.identified && e.is_raider && !e.is_dead)
            .map(|e| (e.unit.pos(), e.can_fly()))
            .collect();
        for (at, flying) in contacts {
            self.eco_workers_see_raider(at, flying);
        }

        // Also cover a mex already under construction when the worker first
        // sees a raider, or one it takes over/assists after that encounter.
        let mexes: Vec<_> = self
            .eco
            .workers
            .values()
            .filter(|w| w.seen_raider)
            .filter_map(|w| {
                let c = self.eco.tasks.get(w.task?)?.as_construction()?;
                let mex = c.target?;
                (self.defs.name(c.build_type) == "staticmex"
                    && mex.is_being_built()
                    && w.unit.def().is_some_and(|d| distance(w.get_pos(), mex.pos()) <= d.build_distance()))
                .then_some((w.id, mex))
            })
            .collect();
        for (worker, mex) in mexes {
            self.eco_remember_raid_mex(worker, mex);
        }
    }

    pub(super) fn eco_remember_raid_mex(&mut self, worker: i32, mex: Unit) {
        if self.eco.workers.get(&worker).is_some_and(|w| w.seen_raider) {
            let at = mex.pos();
            if self.eco.raid_mexes.insert(mex.id(), at).is_none() {
                self.eco_defend_mex(at);
            }
        }
    }

    pub(super) fn eco_mex_porc_allowed(&self, at: Vec3) -> bool {
        self.op_porc_allowed(at) || self.eco.raid_mexes.values().any(|&p| distance(p, at) < super::SITE_SLACK)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::economy::{Worker, test_ai_with};
    use std::ffi::c_char;

    fn ai() -> Ai {
        unsafe extern "C" fn zero(_: i32, _: i32) -> i32 {
            0
        }
        unsafe extern "C" fn bp(_: i32, _: i32) -> f32 {
            10.0
        }
        unsafe extern "C" fn sight(_: i32, _: i32) -> f32 {
            300.0
        }
        unsafe extern "C" fn air_sight(_: i32, _: i32) -> f32 {
            600.0
        }
        unsafe extern "C" fn no(_: i32, _: i32) -> bool {
            false
        }
        unsafe extern "C" fn unit_def(_: i32, id: i32) -> i32 {
            if id == 12 { -1 } else { 1 }
        }
        unsafe extern "C" fn tooltip(_: i32, id: i32) -> *const c_char {
            match id {
                1 => c"Scout".as_ptr(),
                2 => c"Raider".as_ptr(),
                _ => c"Assault".as_ptr(),
            }
        }
        let mut ai = test_ai_with(|t| {
            t.Unit_getDef = Some(unit_def);
            t.Unit_getWeapons = Some(zero);
            t.UnitDef_getWeaponMounts = Some(zero);
            t.UnitDef_getBuildSpeed = Some(bp);
            t.UnitDef_getLosRadius = Some(sight);
            t.UnitDef_getAirLosRadius = Some(air_sight);
            t.UnitDef_getTooltip = Some(tooltip);
            t.UnitDef_isAbleToFly = Some(no);
        });
        // Stub positions are (id * 100, 0): workers at 1000 and 2000.
        for id in [10, 20] {
            let worker = Worker::new(Unit::from_id(ai.cb, id).unwrap(), ai.team_id);
            ai.eco.workers.insert(id, worker);
        }
        ai
    }

    #[test]
    fn encounter_is_local_and_lasts_after_contact_leaves() {
        for def in [1, 2] {
            let mut ai = ai();
            let enemy = Unit::from_id(ai.cb, 12).unwrap();
            ai.eco_observe_raider(enemy, UnitDef::from_id(ai.cb, 3).unwrap());
            assert!(!ai.eco.workers[&10].seen_raider, "assault is not a raider");
            ai.eco_observe_raider(enemy, UnitDef::from_id(ai.cb, def).unwrap());
            assert!(ai.eco.workers[&10].seen_raider);
            assert!(!ai.eco.workers[&20].seen_raider);
            ai.eco_observe_raiders(); // No current contacts; memory survives.
            assert!(ai.eco.workers[&10].seen_raider);
            let replacement = Worker::new(Unit::from_id(ai.cb, 10).unwrap(), ai.team_id);
            assert!(!replacement.seen_raider, "a new worker has its own history");
        }
    }

    #[test]
    fn ground_and_air_contacts_use_the_workers_respective_sight() {
        let mut ai = ai();
        ai.eco_workers_see_raider(Vec3::xz(1500.0, 0.0), false);
        assert!(ai.eco.workers.values().all(|w| !w.seen_raider));
        ai.eco_workers_see_raider(Vec3::xz(1500.0, 0.0), true);
        assert!(ai.eco.workers.values().all(|w| w.seen_raider));
    }

    #[test]
    fn polling_notices_existing_visible_contacts_but_not_radar_memory() {
        let mut ai = ai();
        let mut enemy = crate::military::Enemy::new(Unit::from_id(ai.cb, 12).unwrap(), 50.0);
        enemy.identified = true;
        enemy.is_raider = true;
        enemy.is_radar_visible = true;
        ai.war.targets.insert(12, enemy);
        ai.eco_observe_raiders();
        assert!(!ai.eco.workers[&10].seen_raider);
        ai.war.targets.get_mut(&12).unwrap().visible = true;
        ai.eco_observe_raiders();
        assert!(ai.eco.workers[&10].seen_raider);
        assert!(!ai.eco.workers[&20].seen_raider);
    }

    #[test]
    fn only_experienced_workers_mexes_gain_permission() {
        let mut ai = ai();
        let mex = Unit::from_id(ai.cb, 30).unwrap();
        assert!(!ai.op_porc_allowed(mex.pos()));
        ai.eco_remember_raid_mex(20, mex);
        assert!(!ai.eco_mex_porc_allowed(mex.pos()));
        ai.eco_workers_see_raider(Vec3::xz(1200.0, 0.0), false);
        ai.eco_remember_raid_mex(10, mex);
        assert!(ai.eco_mex_porc_allowed(mex.pos()));
        assert!(!ai.op_porc_allowed(mex.pos()), "links still use the global setting");
        assert!(!ai.eco_mex_porc_allowed(Vec3::xz(4000.0, 0.0)));
        ai.eco.workers.remove(&10);
        assert!(ai.eco_mex_porc_allowed(mex.pos()), "the built mex retains its defense request");
        ai.eco_unit_destroyed(mex, None);
        assert!(!ai.eco_mex_porc_allowed(mex.pos()), "a dead mex leaves no defense zone");
    }
}
