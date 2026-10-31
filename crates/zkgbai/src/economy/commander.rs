//! Commanders keep working until a raid threatens something nearby. They
//! defend only around where they were working, without joining a roaming
//! QRF team or counting toward its production budget.

use springai::{Vec3, reserved};

use crate::Ai;
use crate::util::{distance, get_directional_point};

const LEASH: f32 = 650.0;
const ASSET_RANGE: f32 = 450.0;
const MIN_HEALTH: f32 = 0.65;
const FRESH: i32 = 30;
const REORDER: i32 = 90;

#[derive(Clone, Copy)]
pub(super) struct Defense {
    /// Fixed for the response: following a raid must not move the leash.
    anchor: Vec3,
    order: Vec3,
    at: i32,
}

/// Stay in weapon range on the home side of the raider. A move, rather
/// than an attack order, prevents pursuit outside the response area.
fn defense_point(anchor: Vec3, pos: Vec3, enemy: Vec3, range: f32) -> Option<Vec3> {
    if distance(anchor, enemy) > LEASH || distance(anchor, pos) > LEASH {
        return None;
    }
    if distance(pos, enemy) <= range {
        return Some(pos);
    }
    Some(get_directional_point(enemy, pos, range))
}

impl Ai {
    pub(crate) fn eco_defend_commanders(&mut self) {
        if self.dev_off("commander") {
            self.eco_release_commander_defense();
            return;
        }
        for id in self.eco.commanders.clone() {
            self.eco_defend_commander(id);
        }
    }

    fn eco_defend_commander(&mut self, id: i32) {
        let Some(w) = self.eco.workers.get(&id) else { return };
        let unit = w.unit;
        let old = w.raid_defense;
        let pos = unit.pos();
        let anchor = old.map_or(pos, |d| d.anchor);
        let available = unit.health() >= MIN_HEALTH * unit.max_health().max(1.0)
            && unit.team() == self.team_id
            && !unit.is_being_built()
            && !w.is_chicken
            && w.get_shields() >= 0.5
            && !w.has_plop()
            && !reserved::is_reserved(self.cb, id)
            && !self.qrf_claims(id)
            && !super::terraform::is_terraforming(unit)
            && !self.eco_in_transit(id);
        let def = unit.def();
        let view = self.war.view(def);
        // The same scale as the commander's paint in war_update_threats,
        // discounted by health. Its guns still count at an approach just
        // outside the current friendly paint.
        let own =
            (w.power + unit.max_health()) / 14.0 / 500.0 * (unit.health() / unit.max_health().max(1.0)).clamp(0.0, 1.0);
        let range = unit.max_range().clamp(80.0, 450.0) * 0.85;
        let target = available
            .then(|| {
                self.qrf
                    .groups
                    .iter()
                    .chain(&self.qrf.heavies)
                    .flat_map(|g| &g.members)
                    .filter_map(|id| self.qrf.tracks.get(id))
                    .filter(|t| {
                        self.frame - t.last_seen <= FRESH
                            && t.kind().is_some_and(|d| {
                                d.speed() > 0.0
                                    && !d.is_able_to_fly()
                                    && d.is_able_to_attack()
                                    && d.build_options().is_empty()
                            })
                            && self
                                .qrf
                                .assets
                                .iter()
                                .any(|a| distance(anchor, a.pos) <= LEASH && distance(t.pos, a.pos) <= ASSET_RANGE)
                    })
                    .filter_map(|t| {
                        let to = defense_point(anchor, pos, t.pos, range)?;
                        (view.get_porc_threat(to) <= 0.0
                            && view.get_porc_threat(t.pos) <= 0.0
                            && view.get_threat(to) <= view.get_total_friendly_threat(to).max(own)
                            && def.is_some_and(|d| self.terrain_reaches(d, pos, to, 32.0)))
                        .then_some((t.pos, to))
                    })
                    .min_by(|a, b| distance(pos, a.0).total_cmp(&distance(pos, b.0)))
            })
            .flatten();

        let Some((_, to)) = target else {
            if old.is_some() {
                if available && self.jump_in_progress(unit) {
                    return;
                }
                self.eco_release_commander(id);
            }
            return;
        };

        // A travel jump already ordered must land before work is stopped
        // and the commander is taken for local defense.
        if self.jump_in_progress(unit) {
            return;
        }
        if old.is_none() {
            let w = self.eco.workers.get_mut(&id).unwrap();
            w.clear_task(&mut self.eco.tasks);
            unit.set_move_state(0, 0);
            unit.set_fire_state(2, 0);
        }
        if old.is_some_and(|d| distance(d.order, to) < 64.0 && self.frame - d.at < REORDER)
            && unit.num_current_commands() > 0
        {
            return;
        }
        if !self.jump_travel(unit, to, "commander defense") {
            unit.move_to(Vec3::new(to.x, self.cb.map().elevation_at(to.x, to.z), to.z), 0);
        }
        self.eco.workers.get_mut(&id).unwrap().raid_defense = Some(Defense { anchor, order: to, at: self.frame });
    }

    pub(crate) fn eco_release_commander_defense(&mut self) {
        for id in self.eco.commanders.clone() {
            self.eco_release_commander(id);
        }
    }

    fn eco_release_commander(&mut self, id: i32) {
        let Some(w) = self.eco.workers.get_mut(&id) else { return };
        if w.raid_defense.take().is_none() || w.is_chicken {
            return;
        }
        let unit = w.unit;
        // An operator, transport or retreat may now own the orders. A
        // jump already under way finishes before economy work resumes.
        if unit.health() > 0.0
            && unit.team() == self.team_id
            && !reserved::is_reserved(self.cb, id)
            && !self.eco_in_transit(id)
            && !self.qrf_claims(id)
            && !super::terraform::is_terraforming(unit)
            && !self.jump_in_progress(unit)
        {
            unit.stop(0);
            self.eco_assign_worker(id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use springai::{Callback, SkirmishAi, Unit, UnitDef, ffi};
    use std::cell::{Cell, RefCell};
    use std::ffi::{CStr, c_char, c_void};

    thread_local! {
        static HEALTH: Cell<f32> = const { Cell::new(3000.0) };
        static JUMP_RELOAD: Cell<f32> = const { Cell::new(1.0) };
        static CAN_JUMP: Cell<bool> = const { Cell::new(true) };
        static ORDERS: RefCell<Vec<(i32, i16)>> = const { RefCell::new(Vec::new()) };
    }

    fn commander_ai() -> Ai {
        unsafe extern "C" fn size(_: i32) -> i32 {
            512
        }
        unsafe extern "C" fn zero(_: i32, _: i32) -> i32 {
            0
        }
        unsafe extern "C" fn team(_: i32, _: i32) -> i32 {
            2
        }
        unsafe extern "C" fn no(_: i32, _: i32) -> bool {
            false
        }
        unsafe extern "C" fn yes(_: i32, _: i32) -> bool {
            true
        }
        unsafe extern "C" fn speed(_: i32, _: i32) -> f32 {
            45.0
        }
        unsafe extern "C" fn bp(_: i32, _: i32) -> f32 {
            10.0
        }
        unsafe extern "C" fn max_health(_: i32, _: i32) -> f32 {
            3000.0
        }
        unsafe extern "C" fn health(_: i32, _: i32) -> f32 {
            HEALTH.with(Cell::get)
        }
        unsafe extern "C" fn range(_: i32, _: i32) -> f32 {
            300.0
        }
        unsafe extern "C" fn depth(_: i32, _: i32) -> f32 {
            22.0
        }
        unsafe extern "C" fn cost(_: i32, _: i32, _: i32) -> f32 {
            100.0
        }
        unsafe extern "C" fn empty(_: i32, _: i32, _: *mut i32, _: i32) -> i32 {
            0
        }
        unsafe extern "C" fn pos(_: i32, id: i32, out: *mut f32) {
            let p = if id == 10 { [1000.0, 0.0, 1000.0] } else { [1600.0, 0.0, 1000.0] };
            unsafe {
                std::ptr::copy_nonoverlapping(p.as_ptr(), out, 3);
            }
        }
        unsafe extern "C" fn elevation(_: i32, _: f32, _: f32) -> f32 {
            0.0
        }
        unsafe extern "C" fn rules(_: i32, _: i32, name: *const c_char, default: f32) -> f32 {
            if unsafe { CStr::from_ptr(name) } == c"jumpReload" { JUMP_RELOAD.with(Cell::get) } else { default }
        }
        unsafe extern "C" fn params(_: i32, _: i32, keys: *mut *const c_char, vals: *mut *const c_char) -> i32 {
            if !CAN_JUMP.with(Cell::get) {
                return 0;
            }
            if !keys.is_null() && !vals.is_null() {
                unsafe {
                    *keys = c"canjump".as_ptr();
                    *vals = c"1".as_ptr();
                }
            }
            1
        }
        unsafe extern "C" fn command(_: i32, _: i32, _: i32, topic: i32, data: *mut c_void) -> i32 {
            // Every unit command begins with the same header.
            let h = unsafe { &*(data as *const ffi::SStopUnitCommand) };
            let cmd = if topic as u32 == ffi::CommandTopic::COMMAND_UNIT_CUSTOM {
                unsafe { (*(data as *const ffi::SCustomUnitCommand)).cmdId }
            } else {
                topic
            };
            ORDERS.with_borrow_mut(|o| o.push((cmd, h.options)));
            0
        }
        HEALTH.with(|h| h.set(3000.0));
        JUMP_RELOAD.with(|r| r.set(1.0));
        CAN_JUMP.with(|j| j.set(true));
        ORDERS.with_borrow_mut(Vec::clear);
        let mut ai = test_ai_with(|t| {
            t.Map_getWidth = Some(size);
            t.Map_getHeight = Some(size);
            t.Map_getElevationAt = Some(elevation);
            t.Unit_getTeam = Some(team);
            t.Unit_getHealth = Some(health);
            t.Unit_getMaxHealth = Some(max_health);
            t.Unit_getMaxRange = Some(range);
            t.Unit_getPos = Some(pos);
            t.Unit_getRulesParamFloat = Some(rules);
            t.Unit_getWeapons = Some(zero);
            t.Unit_getCurrentCommands = Some(zero);
            t.UnitDef_getWeaponMounts = Some(zero);
            t.UnitDef_getBuildSpeed = Some(bp);
            t.UnitDef_getBuildOptions = Some(empty);
            t.UnitDef_getCustomParams = Some(params);
            t.UnitDef_getSpeed = Some(speed);
            t.UnitDef_getMaxWaterDepth = Some(depth);
            t.UnitDef_getCost = Some(cost);
            t.UnitDef_isAbleToFly = Some(no);
            t.UnitDef_isAbleToAttack = Some(yes);
            t.UnitDef_isMoveDataAvailable = Some(no);
            t.Engine_handleCommand = Some(command);
        });
        let u = Unit::from_id(ai.cb, 10).unwrap();
        let mut w = super::super::Worker::new(u, ai.team_id);
        w.is_com = true;
        w.power = 155.0;
        ai.eco.insert_worker(w);
        ai.eco.commanders.push(10);
        ai.frame = 600;
        ai.qrf.on = true;
        ai.qrf.live = true;
        ai.qrf.assets.push(crate::military::qrf::Asset {
            unit: u,
            pos: Vec3::xz(1300.0, 1000.0),
            cost: 75.0,
            is_mex: true,
        });
        ai.qrf.tracks.insert(
            20,
            crate::military::qrf::Track {
                unit: Unit::from_id(ai.cb, 20).unwrap(),
                pos: Vec3::xz(1600.0, 1000.0),
                vel: Vec3::ZERO,
                first_seen: 600,
                last_seen: 600,
                top_speed: 90.0,
                def: UnitDef::from_id(ai.cb, 1),
                guess: None,
                in_los: true,
                health: 300.0,
                pressed: 0,
                pressed_on: 0,
            },
        );
        ai.qrf.groups.push(crate::military::qrf::RaidGroup {
            members: vec![20],
            center: Vec3::xz(1600.0, 1000.0),
            vel: Vec3::ZERO,
            speed: 90.0,
            power: 100.0,
            mobility: crate::military::qrf::field::Mobility::BOT,
            pressed: 0,
            pressed_on: vec![],
            prediction_only: false,
        });
        ai
    }

    fn test_ai_with(customize: impl FnOnce(&mut ffi::SSkirmishAICallback)) -> Ai {
        use std::ffi::{CStr, c_char, c_void};
        unsafe extern "C" fn size(_: i32) -> i32 {
            32
        }
        unsafe extern "C" fn team(_: i32) -> i32 {
            2
        }
        unsafe extern "C" fn zero(_: i32) -> i32 {
            0
        }
        unsafe extern "C" fn water(_: i32) -> f32 {
            0.0
        }
        unsafe extern "C" fn game_rules(_: i32, _: *const c_char, default: f32) -> f32 {
            default
        }
        unsafe extern "C" fn floats(_: i32, _: *mut f32, _: i32) -> i32 {
            0
        }
        unsafe extern "C" fn ints(_: i32, _: *mut i32, _: i32) -> i32 {
            0
        }
        unsafe extern "C" fn log(_: i32, _: *const c_char) {}
        unsafe extern "C" fn lookup(_: i32, _: *const c_char) -> i32 {
            -1
        }
        unsafe extern "C" fn resource(_: i32, _: *const c_char) -> i32 {
            0
        }
        unsafe extern "C" fn option(_: i32, _: *const c_char) -> *const c_char {
            c"off".as_ptr()
        }
        unsafe extern "C" fn defs(_: i32, _: *mut i32, _: i32) -> i32 {
            0
        }
        unsafe extern "C" fn name(_: i32, _: i32) -> *const c_char {
            c"staticcon".as_ptr()
        }
        unsafe extern "C" fn def(_: i32, _: i32) -> i32 {
            1
        }
        unsafe extern "C" fn speed(_: i32, _: i32) -> f32 {
            0.0
        }
        unsafe extern "C" fn range(_: i32, _: i32) -> f32 {
            500.0
        }
        unsafe extern "C" fn building(_: i32, _: i32) -> bool {
            false
        }
        unsafe extern "C" fn pos(_: i32, id: i32, out: *mut f32) {
            unsafe {
                std::ptr::copy_nonoverlapping([id as f32 * 100.0, 0.0, 0.0].as_ptr(), out, 3);
            }
        }
        unsafe extern "C" fn rules(_: i32, id: i32, name: *const c_char, default: f32) -> f32 {
            assert_eq!(unsafe { CStr::from_ptr(name) }, c"capture_controller");
            match id {
                10 => 0.0,
                11 => 42.0,
                12 => -1.0,
                _ => default,
            }
        }
        unsafe extern "C" fn command(_: i32, _: i32, _: i32, _: i32, _: *mut c_void) -> i32 {
            0
        }
        let mut table = ffi::SSkirmishAICallback {
            Map_getWidth: Some(size),
            Map_getHeight: Some(size),
            Map_getSlopeMap: Some(floats),
            Map_getHeightMap: Some(floats),
            Map_getLosMap: Some(ints),
            Map_getWaterDamage: Some(water),
            Mod_getLosMipLevel: Some(zero),
            Game_getMyTeam: Some(team),
            Game_getMyAllyTeam: Some(zero),
            Game_getRulesParamFloat: Some(game_rules),
            getUnitDefs: Some(defs),
            getUnitDefByName: Some(lookup),
            getResourceByName: Some(resource),
            SkirmishAI_OptionValues_getValueByKey: Some(option),
            Log_log: Some(log),
            UnitDef_getName: Some(name),
            UnitDef_getSpeed: Some(speed),
            UnitDef_getBuildDistance: Some(range),
            Unit_getDef: Some(def),
            Unit_isBeingBuilt: Some(building),
            Unit_getPos: Some(pos),
            Unit_getRulesParamFloat: Some(rules),
            Engine_handleCommand: Some(command),
            ..Default::default()
        };
        customize(&mut table);
        let cb = unsafe { Callback::from_raw(0, Box::leak(Box::new(table))) };
        Ai::new(cb)
    }

    #[test]
    fn recon_commander_defends_with_a_jump_and_economy_does_not_replace_it() {
        let mut ai = commander_ai();
        ai.eco_defend_commanders();
        assert!(ai.eco.workers[&10].raid_defense.is_some());
        assert_eq!(ai.jumps.counts.get("commander defense"), Some(&1));
        let orders = ORDERS.with_borrow(Vec::len);
        ai.eco_assign_worker(10);
        ai.eco_defend_commanders();
        ORDERS.with_borrow(|o| assert_eq!(o.len(), orders, "the queued jump must survive"));
        assert!(!ai.eco.workers[&10].is_chicken);
    }

    #[test]
    fn cooldown_walks_and_hurt_commanders_do_not_join_defense() {
        let mut ai = commander_ai();
        JUMP_RELOAD.with(|r| r.set(0.5));
        ai.eco_defend_commanders();
        assert!(ai.eco.workers[&10].raid_defense.is_some());
        assert_eq!(ai.jumps.total, 0);
        ORDERS.with_borrow(|o| assert!(o.iter().any(|&(id, _)| id as u32 == ffi::CommandTopic::COMMAND_UNIT_MOVE)));
        HEALTH.with(|h| h.set(1000.0));
        ai.eco_defend_commanders();
        assert!(ai.eco.workers[&10].raid_defense.is_none());
    }

    #[test]
    fn ordinary_commander_leaves_its_job_to_defend_then_resumes_work() {
        let mut ai = commander_ai();
        CAN_JUMP.with(|j| j.set(false));
        let task = ai.eco.tasks.insert(super::super::WorkerTask::construction(
            UnitDef::from_id(ai.cb, 1).unwrap(),
            Vec3::xz(2400.0, 1000.0),
            0,
        ));
        ai.eco.construction_tasks.push(task);
        ai.eco.workers.get_mut(&10).unwrap().set_task(&mut ai.eco.tasks, task, ai.frame);
        ai.eco_defend_commanders();
        assert!(ai.eco.workers[&10].raid_defense.is_some());
        assert!(ai.eco.workers[&10].task.is_none());
        assert!(ai.eco.tasks.get(task).unwrap().assigned_workers.is_empty());
        assert_eq!(ai.jumps.total, 0);
        // An unseen raid stops pulling the commander off its job.
        ai.frame += FRESH + 1;
        ai.eco_defend_commanders();
        assert!(ai.eco.workers[&10].raid_defense.is_none());
        assert_eq!(ai.eco.workers[&10].task, Some(task));
        assert_eq!(ai.eco.tasks.get(task).unwrap().assigned_workers, vec![10]);
        ORDERS.with_borrow(|o| assert_eq!(o.last().unwrap().0 as u32, ffi::CommandTopic::COMMAND_UNIT_BUILD));
    }

    #[test]
    fn commander_defense_yields_to_operator_ownership() {
        let mut ai = commander_ai();
        CAN_JUMP.with(|j| j.set(false));
        ai.eco_defend_commanders();
        assert!(ai.eco.workers[&10].raid_defense.is_some());
        reserved::reserve(ai.cb, 10);
        let n = ORDERS.with_borrow(Vec::len);
        ai.eco_defend_commanders();
        assert!(ai.eco.workers[&10].raid_defense.is_none());
        ORDERS.with_borrow(|o| assert_eq!(o.len(), n));
        reserved::release(ai.cb, 10);
    }

    #[test]
    fn operator_switch_defaults_on_and_releases_defense_immediately() {
        let mut ai = commander_ai();
        CAN_JUMP.with(|j| j.set(false));
        assert!(!ai.dev_off("commander"));
        ai.eco_defend_commanders();
        assert!(ai.eco.workers[&10].raid_defense.is_some());
        ai.set_feature_off("commander", true);
        assert!(ai.eco.workers[&10].raid_defense.is_none());
        ai.eco_defend_commanders();
        assert!(ai.eco.workers[&10].raid_defense.is_none());
        ai.set_feature_off("commander", false);
        ai.eco_defend_commanders();
        assert!(ai.eco.workers[&10].raid_defense.is_some());
    }

    #[test]
    fn travel_jump_preserves_the_following_work_order() {
        let mut ai = commander_ai();
        let task = ai.eco.tasks.insert(super::super::WorkerTask::construction(
            UnitDef::from_id(ai.cb, 1).unwrap(),
            Vec3::xz(2400.0, 1000.0),
            0,
        ));
        ai.eco.construction_tasks.push(task);
        ai.eco.workers.get_mut(&10).unwrap().set_task(&mut ai.eco.tasks, task, ai.frame);
        ai.eco_assign_worker(10);
        assert_eq!(ai.jumps.counts.get("commander travel"), Some(&1));
        ORDERS.with_borrow(|o| {
            assert_eq!(o[0], (crate::defs::cmd::JUMP, 0));
            assert!(o[1..].iter().all(|&(_, opts)| opts & crate::util::OPTION_SHIFT_KEY != 0));
            assert_eq!(o.last().unwrap().0 as u32, ffi::CommandTopic::COMMAND_UNIT_BUILD);
        });
        let n = ORDERS.with_borrow(Vec::len);
        ai.eco_assign_worker(10);
        ORDERS.with_borrow(|o| assert_eq!(o.len(), n));
    }

    #[test]
    fn operator_switch_disables_commander_travel_jumps() {
        let mut ai = commander_ai();
        ai.set_feature_off("commander", true);
        let task = ai.eco.tasks.insert(super::super::WorkerTask::construction(
            UnitDef::from_id(ai.cb, 1).unwrap(),
            Vec3::xz(2400.0, 1000.0),
            0,
        ));
        ai.eco.construction_tasks.push(task);
        ai.eco.workers.get_mut(&10).unwrap().set_task(&mut ai.eco.tasks, task, ai.frame);
        ai.eco_assign_worker(10);
        assert_eq!(ai.jumps.total, 0);
        ORDERS.with_borrow(|o| assert_eq!(o.last().unwrap().0 as u32, ffi::CommandTopic::COMMAND_UNIT_BUILD));
    }

    #[test]
    fn commander_closes_to_weapon_range_and_holds_there() {
        let home = Vec3::xz(1000.0, 1000.0);
        let enemy = Vec3::xz(1600.0, 1000.0);
        let to = defense_point(home, home, enemy, 250.0).unwrap();
        assert_eq!(to, Vec3::xz(1350.0, 1000.0));
        assert_eq!(defense_point(home, to, enemy, 250.0), Some(to));
    }

    #[test]
    fn pursuit_does_not_move_the_leash() {
        let home = Vec3::xz(1000.0, 1000.0);
        let forward = Vec3::xz(1350.0, 1000.0);
        assert!(defense_point(home, forward, Vec3::xz(1700.0, 1000.0), 250.0).is_none());
        assert!(defense_point(home, Vec3::xz(1700.0, 1000.0), forward, 250.0).is_none());
        // A raider on top of the commander requires no direction division.
        assert_eq!(defense_point(home, home, home, 250.0), Some(home));
    }
}
