//! Automatic stakeout reinforcements borrow authority, not the scout's role.

use springai::{Unit, reserved};

use super::{Member, field::Mobility};
use crate::Ai;
use crate::military::{Fighter, roles::Role};

impl Ai {
    pub(crate) fn qrf_can_borrow_stakeout(&self, id: i32) -> bool {
        self.role_of(id) == Some(Role::Stakeout)
            && self.stakeouts.contains(id)
            && !self.qrf.members.contains_key(&id)
            && !reserved::is_reserved(self.cb, id)
            && !self.retreat_handler.is_retreating(id)
            && !self.coalesce_owns(id)
            && !self.recovery_owns(id)
            && reserved::owner(self.cb, id).is_some_and(|owner| owner.module == "stakeouts")
    }

    /// Keep each picket's post/look assignment and budget membership until return.
    pub(crate) fn qrf_borrow_stakeouts(&mut self, units: &[Unit]) -> Option<usize> {
        if !self.qrf.on || !self.qrf.live || self.dev_off("qrf") {
            return None;
        }
        let team = self.qrf.next_team;
        let mut any = false;
        for &unit in units {
            let id = unit.id();
            if !self.qrf_can_borrow_stakeout(id) || unit.team() != self.team_id || unit.health() <= 0.0 {
                continue;
            }
            let Some(def) = unit.def() else { continue };
            let Some(mobility) = Mobility::of(def) else { continue };
            let cost = def.cost(self.metal);
            let threat = Fighter::new(unit, cost, self.team_id).power;
            let lease = reserved::lend(
                self.cb,
                id,
                reserved::Owner { module: "qrf", helpers: &["retreat", "teleport", "water"] },
            );
            self.qrf.stakeout_loans.insert(id, lease);
            self.qrf.members.insert(
                id,
                Member {
                    unit,
                    def,
                    team,
                    cost,
                    speed: def.speed(),
                    mobility,
                    range: def.max_weapon_range(),
                    squad: None,
                    threat,
                    order: None,
                },
            );
            reserved::as_module("qrf.borrow", || {
                unit.set_move_state(1, 0);
                unit.set_fire_state(2, 0);
            });
            any = true;
        }
        if any {
            self.qrf.next_team += 1;
            Some(team)
        } else {
            None
        }
    }

    /// A newer role/lease wins. Do not reroute it or cancel its orders.
    pub(super) fn qrf_prune_stakeout_loans(&mut self) {
        let stale: Vec<_> = self
            .qrf
            .stakeout_loans
            .iter()
            .filter_map(|(&id, lease)| {
                (!lease.active()
                    || !self.stakeouts.contains(id)
                    || self
                        .qrf
                        .members
                        .get(&id)
                        .is_none_or(|m| m.unit.health() <= 0.0 || m.unit.team() != self.team_id))
                .then_some(id)
            })
            .collect();
        for id in stale {
            self.qrf_remove_unit(id);
        }
    }

    /// Returns true even for an invalidated loan: generic recruitment must not
    /// overwrite the newer controller. Retreat/operator orders remain untouched.
    pub(super) fn qrf_return_stakeout(&mut self, unit: Unit) -> bool {
        let id = unit.id();
        let Some(lease) = self.qrf.stakeout_loans.remove(&id) else { return false };
        let resume = lease.active()
            && unit.health() > 0.0
            && unit.team() == self.team_id
            && self.role_of(id) == Some(Role::Stakeout)
            && self.stakeouts.contains(id);
        if resume && !self.retreat_handler.is_retreating(id) {
            reserved::as_module("qrf.release", || {
                unit.stop(0);
            });
        }
        lease.release();
        if resume {
            reserved::as_module("stakeouts.return", || {
                unit.set_move_state(0, 0);
            });
            self.stakeouts_release(id);
        }
        true
    }
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use springai::{Vec3, ffi};
    use std::cell::{Cell, RefCell};
    use std::ffi::c_void;

    thread_local! {
        static HEALTH: Cell<f32> = const { Cell::new(660.0) };
        static TEAM: Cell<i32> = const { Cell::new(2) };
        static ORDERS: RefCell<Vec<i32>> = const { RefCell::new(Vec::new()) };
    }

    pub(crate) fn loan_test_ai() -> Ai {
        HEALTH.set(660.0);
        TEAM.set(2);
        ORDERS.with_borrow_mut(Vec::clear);
        unsafe extern "C" fn yes(_: i32, _: i32) -> bool {
            true
        }
        unsafe extern "C" fn speed(_: i32, _: i32) -> f32 {
            106.0
        }
        unsafe extern "C" fn slope(_: i32, _: i32) -> f32 {
            0.4
        }
        unsafe extern "C" fn depth(_: i32, _: i32) -> f32 {
            22.0
        }
        unsafe extern "C" fn health(_: i32, _: i32) -> f32 {
            HEALTH.get()
        }
        unsafe extern "C" fn team(_: i32, _: i32) -> i32 {
            TEAM.get()
        }
        unsafe extern "C" fn command(_: i32, _: i32, _: i32, topic: i32, _: *mut c_void) -> i32 {
            ORDERS.with_borrow_mut(|o| o.push(topic));
            0
        }
        let mut ai = crate::military::roles::retirement_tests::raider_test_ai_with(c"tankraid", |t| {
            t.UnitDef_getSpeed = Some(speed);
            t.UnitDef_isMoveDataAvailable = Some(yes);
            t.UnitDef_MoveData_getMaxSlope = Some(slope);
            t.UnitDef_getMaxWaterDepth = Some(depth);
            t.Unit_getHealth = Some(health);
            t.Unit_getTeam = Some(team);
            t.Engine_handleCommand = Some(command);
        });
        reserved::clear(ai.cb);
        ai.qrf.on = true;
        ai.qrf.live = true;
        ai.qrf.next_team = 7;
        ai.frame = 4570;
        for id in [1, 2] {
            ai.assign_role(Unit::from_id(ai.cb, id).unwrap(), Role::Stakeout);
        }
        ai
    }

    #[test]
    fn released_kodachis_keep_their_role_and_regain_command_authority() {
        let mut ai = loan_test_ai();
        let units: Vec<_> = [1, 2].map(|id| Unit::from_id(ai.cb, id).unwrap()).into();
        let team = ai.qrf_borrow_stakeouts(&units).unwrap();
        assert_eq!(team, 7);
        assert_eq!(ai.qrf.next_team, 8);
        assert_eq!(ai.stakeouts.count(), 2, "borrowing must not trigger replacement production");
        assert!(ai.stakeouts_borrowable().is_empty(), "coalesce must not take QRF's scouts");
        assert_eq!(ai.qrf_borrow_stakeouts(&units), None, "repeated damage must not create another team");
        for &unit in &units {
            assert_eq!(ai.role_of(unit.id()), Some(Role::Stakeout));
            assert_eq!(crate::operator::views::role(&ai, unit), "qrf");
            assert!(!reserved::as_module("stakeouts", || unit.move_to(Vec3::ZERO, 0)));
            assert!(reserved::as_module("qrf.rescue", || unit.move_to(Vec3::ZERO, 0)));
        }
        ORDERS.with_borrow_mut(Vec::clear);
        assert_eq!(ai.qrf_release_team(team, "over budget"), 2);
        ORDERS.with_borrow(|o| {
            assert_eq!(o.iter().filter(|&&t| t == ffi::CommandTopic::COMMAND_UNIT_STOP as i32).count(), 2)
        });
        for &unit in &units {
            assert_eq!(crate::operator::views::role(&ai, unit), "stakeout");
            assert!(reserved::as_module("stakeouts", || unit.move_to(Vec3::ZERO, 0)));
            assert!(!reserved::as_module("qrf", || unit.move_to(Vec3::ZERO, 0)));
            assert!(!ai.war.new_units.contains(&unit.id()));
        }
        assert!(ai.qrf.members.is_empty());
        assert!(ai.qrf.stakeout_loans.is_empty());
        assert_eq!(ai.stakeouts.count(), 2);
        reserved::clear(ai.cb);
    }

    #[test]
    fn qrf_shutdown_returns_even_reserved_and_retreating_loans_without_stopping_them() {
        let mut ai = loan_test_ai();
        let units = [1, 2].map(|id| Unit::from_id(ai.cb, id).unwrap());
        ai.qrf_borrow_stakeouts(&units).unwrap();
        reserved::reserve(ai.cb, 1);
        ai.retreat_send(units[1]);
        assert!(ai.qrf_teams().is_empty());
        ORDERS.with_borrow_mut(Vec::clear);
        ai.qrf_live_stop();
        assert!(!ai.qrf.on);
        assert!(ai.qrf.stakeout_loans.is_empty());
        assert!(ai.qrf.members.is_empty());
        assert!(reserved::is_reserved(ai.cb, 1));
        assert!(ai.retreat_handler.is_retreating(2));
        ORDERS.with_borrow(|o| assert!(!o.contains(&(ffi::CommandTopic::COMMAND_UNIT_STOP as i32))));
        for id in [1, 2] {
            assert_eq!(reserved::owner(ai.cb, id).unwrap().module, "stakeouts");
        }
        reserved::clear(ai.cb);
    }

    #[test]
    fn newer_role_or_lease_is_never_overwritten_by_qrf_release() {
        for new_role in [false, true] {
            let mut ai = loan_test_ai();
            let unit = Unit::from_id(ai.cb, 1).unwrap();
            let team = ai.qrf_borrow_stakeouts(&[unit]).unwrap();
            let newer = if new_role {
                ai.assign_role(unit, Role::SoloRaider);
                None
            } else {
                Some(reserved::lend(ai.cb, 1, reserved::Owner { module: "recovery", helpers: &[] }))
            };
            ORDERS.with_borrow_mut(Vec::clear);
            ai.qrf_release_team(team, "over budget");
            ORDERS.with_borrow(|o| assert!(o.is_empty()));
            assert_eq!(reserved::owner(ai.cb, 1).unwrap().module, if new_role { "raider" } else { "recovery" });
            assert!(!ai.war.new_units.contains(&1));
            if let Some(lease) = newer {
                assert!(lease.active());
                lease.release();
            }
            reserved::clear(ai.cb);
        }
    }

    #[test]
    fn invalidated_dead_and_transferred_loans_leave_the_roster() {
        for reason in 0..3 {
            let mut ai = loan_test_ai();
            let unit = Unit::from_id(ai.cb, 1).unwrap();
            ai.qrf_borrow_stakeouts(&[unit]).unwrap();
            match reason {
                0 => ai.assign_role(unit, Role::SoloRaider),
                1 => HEALTH.set(0.0),
                _ => TEAM.set(3),
            }
            ORDERS.with_borrow_mut(Vec::clear);
            ai.qrf_prune_stakeout_loans();
            assert!(ai.qrf.members.is_empty());
            assert!(ai.qrf.stakeout_loans.is_empty());
            ORDERS.with_borrow(|o| assert!(o.is_empty()));
            reserved::clear(ai.cb);
        }
    }

    #[test]
    fn borrowed_command_authority_still_respects_reservations_and_mutes() {
        let mut ai = loan_test_ai();
        let unit = Unit::from_id(ai.cb, 1).unwrap();
        reserved::reserve(ai.cb, 1);
        assert_eq!(ai.qrf_borrow_stakeouts(&[unit]), None);
        reserved::release(ai.cb, 1);
        ai.qrf_borrow_stakeouts(&[unit]).unwrap();
        reserved::mute(ai.cb, "qrf");
        assert!(!reserved::as_module("qrf.rescue", || unit.move_to(Vec3::ZERO, 0)));
        reserved::unmute(ai.cb, "qrf");
        reserved::reserve(ai.cb, 1);
        assert!(!reserved::as_module("qrf.rescue", || unit.move_to(Vec3::ZERO, 0)));
        reserved::clear(ai.cb);
    }
}
