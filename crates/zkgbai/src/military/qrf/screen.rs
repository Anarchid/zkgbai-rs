//! Direct control for QRF positioning: shoot without pursuit and yield to approaching weapons.
use crate::Ai;
use crate::kgbutil::pathfinder::PathCost;
use crate::military::wrappers::fighter::next_waypoint;
use crate::util::NULLPOS;
use springai::Vec3;
use std::collections::BTreeMap;

/// Authoritative objectives; routes are engine-owned and rechecked from current observations.
#[derive(Default)]
#[checkpoint::state]
pub(super) struct Screening {
    pub(super) goals: BTreeMap<i32, Vec3>,
}

impl Ai {
    pub(super) fn qrf_withdraw(&mut self, ids: &[i32]) {
        for &id in ids {
            let Some(unit) = self.qrf.members.get(&id).map(|m| m.unit) else { continue };
            let haven = self.graph_get_closest_haven(unit.pos());
            self.qrf_reposition(&[id], if haven == NULLPOS { unit.pos() } else { haven });
        }
    }

    /// The planner selects the objective; the control tick owns the actual
    /// route. Keeping the objective lets a pending planner still yield to an
    /// advancing enemy instead of preserving yesterday's safe MOVE queue.
    pub(super) fn qrf_reposition(&mut self, ids: &[i32], target: Vec3) {
        for &id in ids {
            let Some(unit) = self.qrf.members.get(&id).map(|m| m.unit) else { continue };
            if unit.pos().y < -5.0 || unit.def().is_none_or(|d| d.is_able_to_fly()) {
                let mut path = self.pathfinder.find_path(&self.war, unit, target, PathCost::AvoidEnemies);
                let waypoint = next_waypoint(&mut path, 1).unwrap_or_else(|| unit.pos());
                self.qrf_order(id, waypoint, false);
                continue;
            }
            self.qrf.screen.goals.insert(id, target);
        }
    }

    pub(super) fn qrf_clear_screen(&mut self, id: i32) {
        let yielding = self.retreat_handler.is_retreating(id)
            || self.recovery_owns(id)
            || self.coalesce_owns(id)
            || self.harassment_owns(id);
        if self.qrf.screen.goals.remove(&id).is_some()
            && let Some(m) = self.qrf.members.get_mut(&id)
        {
            // The next QRF order must replace any raw-move queue. Do not
            // change a newer owner's command state after a lease is revoked.
            m.order = None;
            if !yielding
                && !springai::reserved::is_reserved(self.cb, id)
                && self.qrf.stakeout_loans.get(&id).is_none_or(|l| l.active())
                && m.unit.health() > 0.0
                && m.unit.team() == self.team_id
            {
                m.unit.set_move_state(1, 0);
            }
        }
    }

    pub(super) fn qrf_screen_tick(&mut self) {
        let goals: Vec<_> = self.qrf.screen.goals.iter().map(|(&id, &goal)| (id, goal)).collect();
        for (id, goal) in goals {
            let available = self.qrf.members.get(&id).is_some_and(|m| {
                m.unit.health() > 0.0
                    && m.unit.team() == self.team_id
                    && m.squad.is_none_or(|sid| self.qrf_drafted(sid))
                    && m.unit.pos().y >= -5.0
            }) && !springai::reserved::is_reserved(self.cb, id)
                && !self.retreat_handler.is_retreating(id)
                && !self.recovery_owns(id)
                && !self.coalesce_owns(id)
                && !self.harassment_owns(id)
                && self.qrf.stakeout_loans.get(&id).is_none_or(|l| l.active());
            if !available || self.raidcommit_on() {
                self.qrf_clear_screen(id);
                continue;
            }
            let unit = self.qrf.members[&id].unit;
            // Fire at will, but never pursue when the raw-move queue ends.
            unit.set_move_state(0, 0);
            self.move_outside_weapons(unit, goal, super::PERIOD);
        }
    }
}
#[cfg(test)]
mod screen_tests {
    use super::*;
    use crate::military::{
        Enemy,
        qrf::{Member, field::Mobility},
    };
    use springai::{Unit, UnitDef, ffi, reserved};
    use std::{cell::RefCell, ffi::c_void};

    thread_local! {
        static MOVES: RefCell<Vec<(i32, Vec<f32>)>> = const { RefCell::new(Vec::new()) };
        static MODES: RefCell<Vec<i32>> = const { RefCell::new(Vec::new()) };
    }

    fn fixture() -> Ai {
        unsafe extern "C" fn def(_: i32, id: i32) -> i32 {
            if id == 77 { 1 } else { -1 }
        }
        unsafe extern "C" fn pos(_: i32, _: i32, out: *mut f32) {
            unsafe {
                std::ptr::copy_nonoverlapping([1000.0, 0.0, 1000.0].as_ptr(), out, 3);
            }
        }
        unsafe extern "C" fn speed(_: i32, _: i32) -> f32 {
            106.0
        }
        unsafe extern "C" fn no(_: i32, _: i32) -> bool {
            false
        }
        unsafe extern "C" fn commands(_: i32, _: i32) -> i32 {
            0
        }
        unsafe extern "C" fn health(_: i32, _: i32) -> f32 {
            300.0
        }
        unsafe extern "C" fn team(_: i32, _: i32) -> i32 {
            2
        }
        unsafe extern "C" fn elevation(_: i32, _: f32, _: f32) -> f32 {
            0.0
        }
        unsafe extern "C" fn command(_: i32, _: i32, _: i32, topic: i32, data: *mut c_void) -> i32 {
            if topic == ffi::CommandTopic::COMMAND_UNIT_CUSTOM as i32 {
                let c = unsafe { &*data.cast::<ffi::SCustomUnitCommand>() };
                let params = unsafe { std::slice::from_raw_parts(c.params, c.params_size as usize) }.to_vec();
                MOVES.with_borrow_mut(|v| v.push((c.cmdId, params)));
            } else if topic == ffi::CommandTopic::COMMAND_UNIT_SET_MOVE_STATE as i32 {
                let c = unsafe { &*data.cast::<ffi::SSetMoveStateUnitCommand>() };
                MODES.with_borrow_mut(|v| v.push(c.moveState));
            } else if topic == ffi::CommandTopic::COMMAND_UNIT_FIGHT as i32 {
                MOVES.with_borrow_mut(|v| v.push((20, Vec::new())));
            }
            0
        }
        let mut ai = crate::economy::test_ai_with(|t| {
            t.Unit_getDef = Some(def);
            t.Unit_getPos = Some(pos);
            t.Unit_getHealth = Some(health);
            t.Unit_getTeam = Some(team);
            t.UnitDef_getSpeed = Some(speed);
            t.UnitDef_isAbleToFly = Some(no);
            t.Unit_getCurrentCommands = Some(commands);
            t.Map_getElevationAt = Some(elevation);
            t.Engine_handleCommand = Some(command);
        });
        reserved::clear(ai.cb);
        ai.set_feature_off("raidcommit", true);
        ai.qrf.live = true;
        ai.qrf.on = true;
        ai.frame = 150;
        ai.qrf.members.insert(
            77,
            Member {
                unit: Unit::from_id(ai.cb, 77).unwrap(),
                def: UnitDef::from_id(ai.cb, 1).unwrap(),
                team: 0,
                cost: 80.0,
                speed: 106.0,
                mobility: Mobility::BOT,
                range: 240.0,
                squad: None,
                threat: 80.0,
                order: None,
            },
        );
        let mut enemy = Enemy::new(Unit::from_id(ai.cb, 88).unwrap(), 80.0);
        enemy.position = Vec3::xz(3300.0, 1000.0);
        enemy.last_seen = ai.frame;
        ai.war.targets.insert(88, enemy);
        MOVES.with_borrow_mut(Vec::clear);
        MODES.with_borrow_mut(Vec::clear);
        ai
    }

    #[test]
    fn pending_assignment_still_backs_screen_away_from_advancing_contact() {
        let mut ai = fixture();
        let goal = Vec3::xz(2500.0, 1000.0);
        ai.qrf_reposition(&[77], goal);
        ai.qrf_screen_tick();
        MOVES.with_borrow(|v| {
            assert_eq!(v.last().unwrap().0, crate::defs::cmd::RAW_MOVE);
            assert_eq!(v.last().unwrap().1[0], goal.x);
        });
        assert_eq!(MODES.with_borrow(|v| *v.last().unwrap()), 0);
        ai.qrf.plan_pending = true;
        ai.frame += super::super::PERIOD;
        ai.war.targets.get_mut(&88).unwrap().position = Vec3::xz(1300.0, 1000.0);
        ai.qrf_screen_tick();
        MOVES.with_borrow(|v| assert!(v.last().unwrap().1[0] < 1000.0, "screen must back away"));
        assert_eq!(ai.qrf.screen.goals[&77], goal, "keep the objective while yielding ground");
    }

    #[test]
    fn screening_checkpoint_preserves_objective_and_next_movement() {
        let mut ai = fixture();
        ai.qrf_reposition(&[77], Vec3::xz(2500.0, 1000.0));
        ai.qrf_screen_tick();
        let bytes = checkpoint::encode(&ai.qrf.screen).unwrap();
        let restored: Screening =
            springai::snapshot::with_callback(ai.cb, || checkpoint::rmp_serde::from_slice(&bytes)).unwrap();
        assert_eq!(checkpoint::encode(&restored).unwrap(), bytes);
        let mut old = serde_json::to_value(&ai.qrf.screen).unwrap();
        old.as_object_mut().unwrap().remove("goals");
        let missing = springai::snapshot::with_callback(ai.cb, || serde_json::from_value::<Screening>(old));
        assert!(missing.err().unwrap().to_string().contains("goals"));
        ai.war.targets.get_mut(&88).unwrap().position = Vec3::xz(1300.0, 1000.0);
        MOVES.with_borrow_mut(Vec::clear);
        ai.qrf_screen_tick();
        let expected = MOVES.with_borrow(Clone::clone);
        let expected_state = checkpoint::encode(&ai.qrf.screen).unwrap();
        ai.qrf.screen = restored;
        MOVES.with_borrow_mut(Vec::clear);
        ai.qrf_screen_tick();
        assert_eq!(MOVES.with_borrow(Clone::clone), expected);
        assert_eq!(checkpoint::encode(&ai.qrf.screen).unwrap(), expected_state);
    }

    #[test]
    fn explicit_order_and_ownership_transfer_cancel_screen_objective() {
        let mut ai = fixture();
        let goal = Vec3::xz(2500.0, 1000.0);
        ai.qrf_reposition(&[77], goal);
        // Explicit combat supersedes screening; the next control tick must
        // not replace that commitment with an old movement objective.
        ai.qrf_order(77, Vec3::xz(800.0, 1000.0), true);
        assert!(ai.qrf.screen.goals.is_empty());
        assert_eq!(MODES.with_borrow(|v| *v.last().unwrap()), 1);
        ai.qrf_reposition(&[77], goal);
        ai.set_feature_off("raidcommit", false);
        assert!(ai.qrf.screen.goals.is_empty(), "policy change hands off movement");
        ai.set_feature_off("raidcommit", true);
        ai.qrf_reposition(&[77], goal);
        reserved::reserve(ai.cb, 77);
        MOVES.with_borrow_mut(Vec::clear);
        MODES.with_borrow_mut(Vec::clear);
        ai.qrf_screen_tick();
        assert!(ai.qrf.screen.goals.is_empty());
        assert!(MOVES.with_borrow(|v| v.is_empty()));
        assert!(MODES.with_borrow(|v| v.is_empty()));
        reserved::clear(ai.cb);
    }
}
