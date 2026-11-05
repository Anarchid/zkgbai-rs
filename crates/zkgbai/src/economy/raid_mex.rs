//! A mex remembers a scout or raider coming within Lotus weapon range and
//! requests normal sparse mexporc for the rest of its life.

use springai::{Unit, UnitDef, Vec3};

use crate::{Ai, util::distance};

impl Ai {
    /// LOS events arrive before MilitaryManager has identified the enemy.
    pub(super) fn eco_observe_raider(&mut self, enemy: Unit, def: UnitDef) {
        // Same scout/raider classification as Enemy::update_from_unit_def.
        let tooltip = def.tooltip();
        if tooltip.contains("aider") || tooltip.contains("cout") {
            self.eco_mexes_near_raider(enemy.pos());
        }
    }

    fn eco_mexes_near_raider(&mut self, at: Vec3) {
        let Some(lotus) = self.defs.get("turretlaser") else { return };
        let range = lotus.max_weapon_range();
        let exposed: Vec<_> = self
            .eco
            .mexes
            .iter()
            .chain(&self.eco.new_mexes)
            .filter(|m| !self.eco.raid_mexes.contains_key(&m.id()))
            .filter_map(|m| {
                let p = m.pos();
                (range > 0.0 && distance(p, at) <= range).then_some((m.id(), p))
            })
            .collect();
        for (id, p) in exposed {
            self.eco.raid_mexes.insert(id, p);
            self.eco_defend_mex(p);
        }
    }

    pub(super) fn eco_observe_raiders(&mut self) {
        // Poll too: an already observed raider can move near a mex. An
        // identified radar contact counts, but a remembered fog position does not.
        let contacts: Vec<_> = self
            .war
            .targets
            .values()
            .filter(|e| (e.visible || e.is_radar_visible) && e.identified && e.is_raider && !e.is_dead)
            .map(|e| e.unit.pos())
            .collect();
        for at in contacts {
            self.eco_mexes_near_raider(at);
        }
    }

    pub(super) fn eco_mex_porc_allowed(&self, at: Vec3) -> bool {
        self.op_porc_allowed(at) || self.eco.raid_mexes.values().any(|&p| distance(p, at) < super::SITE_SLACK)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::economy::test_ai_with;
    use std::{cell::Cell, ffi::c_char};

    thread_local! { static DEFS_READY: Cell<bool> = const { Cell::new(false) }; }

    fn ai() -> Ai {
        unsafe extern "C" fn defs(_: i32, out: *mut i32, max: i32) -> i32 {
            if !DEFS_READY.get() {
                return 0;
            }
            if !out.is_null() && max > 0 {
                unsafe { out.write(1) }
            }
            1
        }
        unsafe extern "C" fn name(_: i32, _: i32) -> *const c_char {
            c"turretlaser".as_ptr()
        }
        unsafe extern "C" fn range(_: i32, _: i32) -> f32 {
            300.0
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
        DEFS_READY.set(false);
        let mut ai = test_ai_with(|t| {
            t.getUnitDefs = Some(defs);
            t.UnitDef_getName = Some(name);
            t.UnitDef_getMaxWeaponRange = Some(range);
            t.Unit_getDef = Some(unit_def);
            t.UnitDef_getTooltip = Some(tooltip);
        });
        DEFS_READY.set(true);
        ai.defs = crate::defs::Defs::new(ai.cb);
        // Stub positions are (id * 100, 0): finished mex at 1000, frame at 2000.
        let finished = Unit::from_id(ai.cb, 10).unwrap();
        let frame = Unit::from_id(ai.cb, 20).unwrap();
        ai.eco.mexes.push(finished);
        ai.eco.new_mexes.push(frame);
        // Already covered: test eligibility without mocking turret placement.
        ai.eco.porcs.extend([finished, frame]);
        ai
    }

    #[test]
    fn scout_or_raider_marks_only_the_nearby_mex_and_memory_persists() {
        for def in [1, 2] {
            let mut ai = ai();
            let enemy = Unit::from_id(ai.cb, 12).unwrap();
            ai.eco_observe_raider(enemy, UnitDef::from_id(ai.cb, 3).unwrap());
            assert!(ai.eco.raid_mexes.is_empty(), "assault is not a raider");
            ai.eco_observe_raider(enemy, UnitDef::from_id(ai.cb, def).unwrap());
            assert!(ai.eco.raid_mexes.contains_key(&10));
            assert!(!ai.eco.raid_mexes.contains_key(&20));
            ai.eco_observe_raiders(); // No current contacts; memory survives.
            assert!(ai.eco_mex_porc_allowed(Vec3::xz(1000.0, 0.0)));
            assert!(!ai.eco_mex_porc_allowed(Vec3::xz(2000.0, 0.0)));
            assert!(!ai.op_porc_allowed(Vec3::xz(1000.0, 0.0)), "links still use the global setting");
            assert!(ai.eco.workers.is_empty(), "no constructor is needed");
        }
    }

    #[test]
    fn lotus_range_boundary_applies_to_finished_and_unfinished_mexes() {
        let mut ai = ai();
        ai.eco_mexes_near_raider(Vec3::xz(1500.0, 0.0));
        assert!(ai.eco.raid_mexes.is_empty());
        ai.eco_mexes_near_raider(Vec3::xz(1300.0, 0.0));
        assert!(ai.eco.raid_mexes.contains_key(&10));
        assert!(!ai.eco.raid_mexes.contains_key(&20));
        ai.eco_mexes_near_raider(Vec3::xz(1700.0, 0.0));
        assert!(ai.eco.raid_mexes.contains_key(&20));
    }

    #[test]
    fn current_identified_radar_contacts_count_but_fog_unknowns_and_dead_do_not() {
        let mut ai = ai();
        let mut enemy = crate::military::Enemy::new(Unit::from_id(ai.cb, 12).unwrap(), 50.0);
        enemy.identified = true;
        enemy.is_raider = true;
        ai.war.targets.insert(12, enemy);
        ai.eco_observe_raiders();
        assert!(ai.eco.raid_mexes.is_empty(), "fog memory is not a new encounter");
        ai.war.targets.get_mut(&12).unwrap().is_radar_visible = true;
        ai.war.targets.get_mut(&12).unwrap().identified = false;
        ai.eco_observe_raiders();
        assert!(ai.eco.raid_mexes.is_empty(), "unknown radar contact is not a raider");
        ai.war.targets.get_mut(&12).unwrap().identified = true;
        ai.war.targets.get_mut(&12).unwrap().is_dead = true;
        ai.eco_observe_raiders();
        assert!(ai.eco.raid_mexes.is_empty());
        ai.war.targets.get_mut(&12).unwrap().is_dead = false;
        ai.eco_observe_raiders();
        assert!(ai.eco.raid_mexes.contains_key(&10));
        assert!(!ai.eco.raid_mexes.contains_key(&20));
    }

    #[test]
    fn a_rebuilt_mex_does_not_inherit_the_previous_mexs_history() {
        let mut ai = ai();
        ai.eco_mexes_near_raider(Vec3::xz(1200.0, 0.0));
        let mex = Unit::from_id(ai.cb, 10).unwrap();
        ai.eco_unit_destroyed(mex, None);
        ai.eco.new_mexes.push(mex);
        ai.eco_observe_raiders();
        assert!(!ai.eco_mex_porc_allowed(mex.pos()));
    }
}
