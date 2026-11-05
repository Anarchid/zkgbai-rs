//! Roles: which handler a unit belongs to, declared in one place.
//!
//! A factory says what it builds a unit for (`Factory::building_for`); the
//! role is recorded when the unit is created and the unit joins the role's
//! handler when it is finished (`war_unit_finished`, before the claims of
//! the other modules). [`Ai::assign_role`] is the only way into a role: it
//! takes the unit out of the handler of its previous role first. The way
//! out is the handler's own removal (death, or another module conscripting
//! the unit through it), which releases the role.
//!
//! A role is also enforced where orders are issued: the unit is *owned* by
//! its handler's module (see `springai::reserved::own`), and orders from any
//! other module, but the few that help every unit of the kind (the retreat
//! handler for cowards, the unit's own setup when it is finished or first
//! idle, ...), are dropped and counted per module (`foreign` in the order
//! stats, logged at the end of the game). Fleas that were the raider
//! handler's solo raiders and the stakeouts' pickets at once are what this
//! was for.

use std::collections::BTreeMap;

use serde_json::{Value, json};
use springai::Unit;
use springai::reserved::{self, Owner};

use crate::Ai;
use crate::military::Fighter;

/// What a unit is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Role {
    /// A picket or a looker (see `stakeouts`).
    Stakeout,
    /// A raider raiding alone: the raider handler's solo raiders.
    SoloRaider,
    /// A raider in the raider handler's squads (Venoms the spider factory
    /// builds to raid, see `fac_get_spiders`).
    SquadRaider,
}

/// Modules that set a unit up when it is finished or first idle.
const SETUP: [&str; 3] = ["fac.unitCreated", "war.unitFinished", "war.unitIdle"];

impl Role {
    pub fn name(self) -> &'static str {
        match self {
            Role::Stakeout => "stakeout",
            Role::SoloRaider => "solo_raider",
            Role::SquadRaider => "squad_raider",
        }
    }

    /// The module the unit belongs to and its helpers; `None` where the
    /// role only routes the unit (raider squads take orders from the QRF's
    /// moonlighting and the incursion planner's blocks too).
    fn owner(self) -> Option<Owner> {
        match self {
            // The retreat handler pulls back damaged pickets that cannot
            // cloak (cowards, see `stakeouts_add`), through a Djinn's lamp
            // if there is one (`teleport`). A raider gang that borrows an
            // armed picket orders it (`coalesce`; stakeouts skip it then).
            // An engine attack on an enemy factory's frame is turned to the
            // factory (`facframe`), for every role.
            Role::Stakeout => Some(Owner {
                module: "stakeouts",
                helpers: &[SETUP[0], SETUP[1], SETUP[2], "retreat", "teleport", "water", "coalesce", "facframe"],
            }),
            // Solo raiders also take the retreat handler's orders, fire-shadow
            // raids (`precision`, also on damage), jumps and dives, and a
            // raider gang's while it borrows them (`coalesce`; the raider
            // handler skips them then).
            Role::SoloRaider => Some(Owner {
                module: "raider",
                helpers: &[
                    SETUP[0],
                    SETUP[1],
                    SETUP[2],
                    "retreat",
                    "teleport",
                    "precision",
                    "war.unitDamaged",
                    "jumps",
                    "water",
                    "coalesce",
                    "facframe",
                ],
            }),
            Role::SquadRaider => None,
        }
    }

    /// Whether the raider handler's removal ends the role.
    fn raider_handler(self) -> bool {
        matches!(self, Role::SoloRaider | Role::SquadRaider)
    }
}

#[derive(Default)]
pub struct Roles {
    /// Roles units are being built for, by unit id, until finished.
    declared: BTreeMap<i32, Role>,
    /// Each unit's role.
    of: BTreeMap<i32, Role>,
    /// Units given each role, all game.
    assigned: BTreeMap<Role, u32>,
}

impl Roles {
    pub fn count(&self, role: Role) -> usize {
        self.of.values().filter(|&&r| r == role).count()
    }

    /// Units being built for `role`.
    pub fn declared_count(&self, role: Role) -> usize {
        self.declared.values().filter(|&&r| r == role).count()
    }

    /// Unit ids with `role`.
    pub fn units(&self, role: Role) -> impl Iterator<Item = i32> + '_ {
        self.of.iter().filter(move |(_, r)| **r == role).map(|(&id, _)| id)
    }
}

impl Ai {
    pub fn role_of(&self, id: i32) -> Option<Role> {
        self.roles.of.get(&id).copied()
    }

    /// A unit begun for `role` (by a factory that said so).
    pub fn role_declare(&mut self, id: i32, role: Role) {
        self.roles.declared.insert(id, role);
    }

    /// The role a finished unit was built for, if any (once).
    pub fn role_take_declared(&mut self, id: i32) -> Option<Role> {
        self.roles.declared.remove(&id)
    }

    /// Give `unit` `role`: out of its previous role's handler, into the new
    /// one's (as that module), and owned by it.
    pub fn assign_role(&mut self, unit: Unit, role: Role) {
        let Some(def) = unit.def() else { return };
        let name = self.defs.name(def);
        // These scouts have no squad formation slot. In particular, a
        // retired Kodachi must return to its dedicated solo controller.
        let role = if role == Role::SquadRaider
            && (matches!(name, "tankraid" | "shipscout") || self.unit_classes.solo_raiders.contains(name))
        {
            Role::SoloRaider
        } else {
            role
        };
        let small = self.unit_classes.small_raiders.contains(name);
        let id = unit.id();
        let old = self.role_of(id);
        if old == Some(role) {
            return;
        }
        match old {
            Some(Role::Stakeout) => self.stakeouts_forget(unit),
            Some(r) if r.raider_handler() => self.raider_remove_unit(unit),
            _ => {}
        }
        self.roles.of.insert(id, role);
        *self.roles.assigned.entry(role).or_default() += 1;
        match role.owner() {
            Some(o) => reserved::own(self.cb, id, o),
            None => reserved::disown(self.cb, id),
        };
        let (cost, team) = (def.cost(self.metal), self.team_id);
        match role {
            Role::Stakeout => reserved::as_module("stakeouts.add", || self.stakeouts_add(unit)),
            Role::SoloRaider => reserved::as_module("raider.add", || {
                unit.set_move_state(1, 0);
                self.raider_add_solo_raider(Fighter::new_raider(unit, cost, team));
            }),
            Role::SquadRaider => reserved::as_module("raider.add", || {
                unit.set_move_state(1, 0);
                let fighter = Fighter::new_raider(unit, cost, team);
                if small {
                    self.raider_add_small_raider(fighter);
                } else {
                    self.raider_add_medium_raider(fighter);
                }
            }),
        }
    }

    /// The unit left the raider handler (dead, or taken by another module
    /// through `raider_remove_unit`): a raider role ends.
    pub(crate) fn role_left_raiders(&mut self, id: i32) {
        if self.role_of(id).is_some_and(Role::raider_handler) {
            self.role_release(id);
        }
    }

    /// The unit left the stakeouts.
    pub(crate) fn role_left_stakeouts(&mut self, id: i32) {
        if self.role_of(id) == Some(Role::Stakeout) {
            self.role_release(id);
        }
    }

    /// A stakeout leaves for a module without a role (a Puppy for its
    /// herd, see `frontretire`): the stakeouts forget it, its role ends.
    pub(crate) fn role_end_stakeout(&mut self, unit: Unit) {
        self.stakeouts_forget(unit);
        if self.role_of(unit.id()) == Some(Role::Stakeout) {
            self.role_release(unit.id());
        }
    }

    /// End the unit's role: any module may order it again.
    fn role_release(&mut self, id: i32) {
        if self.roles.of.remove(&id).is_some() {
            reserved::disown(self.cb, id);
        }
    }

    pub fn roles_unit_destroyed(&mut self, id: i32) {
        self.roles.declared.remove(&id);
        self.role_release(id);
    }

    /// Units with `role`, built or being built, of def `name`.
    pub fn roles_count_named(&self, role: Role, name: &str) -> i32 {
        let named = |id: i32| Unit::from_id(self.cb, id).is_some_and(|u| self.defs.name_of(u) == name);
        (self.roles.of.iter().chain(&self.roles.declared)).filter(|&(&id, &r)| r == role && named(id)).count() as i32
    }

    /// Metal of the units with `role`, built or being built.
    pub fn roles_metal(&self, role: Role) -> f32 {
        let cost = |id: i32| Unit::from_id(self.cb, id).and_then(|u| u.def()).map_or(0.0, |d| d.cost(self.metal));
        (self.roles.of.iter().chain(&self.roles.declared)).filter(|&(_, &r)| r == role).map(|(&id, _)| cost(id)).sum()
    }

    /// Units per role now and all game, and the orders dropped because
    /// another module owned the unit, per module.
    pub fn roles_summary(&self) -> Value {
        let now: BTreeMap<&str, usize> = [Role::Stakeout, Role::SoloRaider, Role::SquadRaider]
            .into_iter()
            .map(|r| (r.name(), self.roles.count(r)))
            .collect();
        let assigned: BTreeMap<&str, u32> = self.roles.assigned.iter().map(|(r, n)| (r.name(), *n)).collect();
        let foreign: BTreeMap<&str, u64> = (reserved::order_stats(self.cb).into_iter())
            .filter(|(_, s)| s.foreign > 0)
            .map(|(m, s)| (m, s.foreign))
            .collect();
        json!({"now": now, "assigned": assigned, "foreign_orders_dropped": foreign})
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A raider gang borrows solo raiders and armed pickets and orders them
    /// from `coalesce.update`; before 2026-11-03 their owners dropped every
    /// such order (335 a game in the ordered A/B: the borrowed members stood
    /// idle, skipped by their own handler too).
    #[test]
    fn raider_gangs_can_order_what_they_borrow() {
        for role in [Role::SoloRaider, Role::Stakeout] {
            let o = role.owner().unwrap();
            assert!(o.admits("coalesce.update"), "{}", role.name());
            assert!(o.admits("retreat.update"), "{}", role.name());
            assert!(!o.admits("squad.update"), "{}", role.name());
        }
    }
}

#[cfg(test)]
pub(crate) mod retirement_tests {
    use super::*;
    use std::cell::Cell;
    use std::ffi::{CStr, c_char};

    thread_local! {
        static NAME: Cell<&'static CStr> = const { Cell::new(c"tankraid") };
        static DEFS_READY: Cell<bool> = const { Cell::new(false) };
    }

    pub(crate) fn raider_test_ai(name: &'static CStr) -> Ai {
        raider_test_ai_with(name, |_| {})
    }

    pub(crate) fn raider_test_ai_with(
        name: &'static CStr,
        customize: impl FnOnce(&mut springai::ffi::SSkirmishAICallback),
    ) -> Ai {
        NAME.with(|n| n.set(name));
        DEFS_READY.with(|r| r.set(false));
        unsafe extern "C" fn defs(_: i32, out: *mut i32, _: i32) -> i32 {
            if !DEFS_READY.with(Cell::get) {
                return 0;
            }
            if !out.is_null() {
                unsafe { *out = 1 };
            }
            1
        }
        unsafe extern "C" fn features(_: i32, _: *mut i32, _: i32) -> i32 {
            0
        }
        unsafe extern "C" fn def_name(_: i32, _: i32) -> *const c_char {
            NAME.with(|n| n.get().as_ptr())
        }
        unsafe extern "C" fn health(_: i32, _: i32) -> f32 {
            660.0
        }
        unsafe extern "C" fn team(_: i32, _: i32) -> i32 {
            2
        }
        unsafe extern "C" fn cost(_: i32, _: i32, _: i32) -> f32 {
            170.0
        }
        unsafe extern "C" fn no(_: i32, _: i32) -> bool {
            false
        }
        unsafe extern "C" fn zero(_: i32, _: i32) -> i32 {
            0
        }
        unsafe extern "C" fn float(_: i32, _: i32) -> f32 {
            100.0
        }
        unsafe extern "C" fn empty(_: i32, _: i32, _: *mut i32, _: i32) -> i32 {
            0
        }
        unsafe extern "C" fn params(_: i32, _: i32, _: *mut *const c_char, _: *mut *const c_char) -> i32 {
            0
        }
        unsafe extern "C" fn rules(_: i32, _: i32, _: *const c_char, default: f32) -> f32 {
            default
        }
        let mut ai = crate::economy::test_ai_with(|t| {
            t.getUnitDefs = Some(defs);
            t.getFeatures = Some(features);
            t.UnitDef_getName = Some(def_name);
            t.UnitDef_getCost = Some(cost);
            t.Unit_getTeam = Some(team);
            t.Unit_getHealth = Some(health);
            t.Unit_getMaxHealth = Some(health);
            t.UnitDef_getCustomParams = Some(params);
            t.UnitDef_getWeaponMounts = Some(zero);
            t.UnitDef_getBuildOptions = Some(empty);
            t.UnitDef_isAbleToCloak = Some(no);
            t.UnitDef_isAbleToFly = Some(no);
            t.UnitDef_isMoveDataAvailable = Some(no);
            t.UnitDef_getLosRadius = Some(float);
            t.UnitDef_getDecloakDistance = Some(float);
            t.UnitDef_getTooltip = Some(def_name);
            t.UnitDef_getMaxWeaponRange = Some(float);
            t.Unit_getWeapons = Some(zero);
            t.Unit_getCurrentCommands = Some(zero);
            t.Unit_getMaxRange = Some(float);
            t.Unit_getMaxSpeed = Some(float);
            t.Unit_getSpeed = Some(float);
            t.Unit_getRulesParamFloat = Some(rules);
            customize(t);
        });
        DEFS_READY.with(|r| r.set(true));
        ai.defs = crate::defs::Defs::new(ai.cb);
        ai
    }

    #[test]
    fn retired_scouts_join_a_controller_that_can_order_them() {
        for name in [c"tankraid", c"shipscout", c"spiderscout", c"vehscout", c"shieldscout"] {
            let mut ai = raider_test_ai(name);
            let unit = Unit::from_id(ai.cb, 1).unwrap();
            ai.assign_role(unit, Role::Stakeout);
            ai.assign_role(unit, Role::SquadRaider);
            assert_eq!(ai.role_of(1), Some(Role::SoloRaider), "{name:?}");
            assert_eq!(ai.stakeouts.count(), 0);
            assert!(!ai.raider_handler.medium_raiders.contains(&1));
            if name == c"tankraid" {
                assert_eq!(ai.raider_handler.kodachis, vec![1]);
            } else {
                assert!(ai.raider_handler.solo_raiders.contains(&1), "{name:?}");
            }
            ai.assign_role(unit, Role::SquadRaider);
            assert_eq!(ai.roles.count(Role::SoloRaider), 1);
        }
        for name in
            [c"cloakraid", c"shieldraid", c"amphraid", c"hoverraid", c"shiptorpraider", c"spideremp", c"tankheavyraid"]
        {
            let mut ai = raider_test_ai(name);
            let unit = Unit::from_id(ai.cb, 1).unwrap();
            ai.assign_role(unit, Role::Stakeout);
            ai.assign_role(unit, Role::SquadRaider);
            assert_eq!(ai.role_of(1), Some(Role::SquadRaider), "{name:?}");
            assert_eq!(ai.stakeouts.count(), 0);
            assert!(ai.fighters[&1].squad.is_some(), "{name:?} must actually join a squad");
        }
    }
}
