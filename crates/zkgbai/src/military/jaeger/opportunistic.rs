//! Short supported bites for Widows whose primary job is scouting or spotting.
//! Ownership and the original job stay with their controller throughout.

use std::collections::BTreeMap;

use springai::{NO_TIMEOUT, Unit};

use super::{cost, is_heavy};
use crate::Ai;
use crate::defs::cmd;
use crate::military::scouts::WIDOW;
use crate::util::{NULLPOS, distance};

const APPROACH: f32 = 100.0;
const TIMEOUT: i32 = 30 * 5;
const RETRY: i32 = 30 * 5;
const STUN: f32 = 8000.1;

#[derive(Clone, Copy)]
struct Bite {
    target: Option<i32>,
    until: i32,
}

#[derive(Default)]
pub(super) struct Bites {
    units: BTreeMap<i32, Bite>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Order {
    Idle,
    Attack(i32),
    Keep,
    Resume,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum BiteAction {
    Idle,
    Active,
    Resume,
}

struct Target {
    id: i32,
    value: f32,
    stun_left: f32,
}

fn supported_nearby(threat: f32, distance: f32, range: f32) -> bool {
    threat.is_finite() && threat > 0.0 && distance <= range + APPROACH
}

impl Bites {
    pub(super) fn forget(&mut self, id: i32) {
        self.units.remove(&id);
    }

    fn step(&mut self, id: i32, frame: i32, ready: bool, targets: &[Target]) -> Order {
        // Keep this unit's expired attack until its controller can stop it.
        self.units.retain(|&other, b| other == id || b.target.is_some() || frame < b.until);
        if let Some(b) = self.units.get(&id) {
            if let Some(target) = b.target {
                if ready && frame < b.until && targets.iter().any(|t| t.id == target) {
                    return Order::Keep;
                }
                self.units.insert(id, Bite { target: None, until: frame + RETRY });
                return Order::Resume;
            }
            if frame < b.until {
                return Order::Idle;
            }
        }
        if !ready {
            return Order::Idle;
        }
        let target = targets
            .iter()
            .filter(|t| {
                let reserved = self.units.values().filter(|b| b.target == Some(t.id) && frame < b.until).count();
                reserved as f32 * STUN < t.stun_left
            })
            .max_by(|a, b| a.value.total_cmp(&b.value));
        let Some(t) = target else { return Order::Idle };
        self.units.insert(id, Bite { target: Some(t.id), until: frame + TIMEOUT });
        Order::Attack(t.id)
    }
}

impl Ai {
    /// The owner is issuing another command or handing the unit off.
    pub(crate) fn widow_forget_bite(&mut self, id: i32) {
        self.jaegers.opportunistic.forget(id);
    }

    /// Returns Active while an attack owns this unit's commands. On Resume,
    /// the caller must invalidate cached movement before resuming its old job.
    pub(crate) fn widow_opportunistic_bite(&mut self, unit: Unit, allowed: bool) -> BiteAction {
        if self.defs.name_of(unit) != WIDOW {
            return BiteAction::Idle;
        }
        let weapon = unit.weapon(0);
        let ready = allowed
            && unit.health() > 0.0
            && unit.team() == self.team_id
            && !self.retreat_handler.is_retreating(unit.id())
            && !unit.is_paralyzed()
            && unit.rules_param_float("disarmed", 0.0) == 0.0
            && weapon.is_some_and(|w| w.reload_frame() <= self.frame);
        let targets: Vec<_> = if ready {
            let range = weapon.map_or(120.0, |w| w.range());
            self.war
                .targets
                .values()
                .filter(|e| !e.is_dead && e.visible && e.position != NULLPOS && is_heavy(e, self.metal))
                .filter(|e| !e.unit.is_paralyzed())
                .filter(|e| {
                    supported_nearby(
                        self.war.get_total_friendly_threat(e.position),
                        distance(unit.pos(), e.position),
                        range,
                    )
                })
                .filter(|e| self.war.can_hit(self.war.reach(unit.def()), e))
                .filter(|e| unit.def().is_some_and(|d| self.terrain_reaches(d, unit.pos(), e.position, range)))
                .map(|e| Target {
                    id: e.unit_id,
                    value: cost(e, self.metal),
                    stun_left: (e.def_health() - e.unit.paralyze_damage()).max(0.0),
                })
                .collect()
        } else {
            Vec::new()
        };
        match self.jaegers.opportunistic.step(unit.id(), self.frame, ready, &targets) {
            Order::Attack(id) => {
                let target = self.war.targets[&id].unit;
                unit.execute_custom_command(cmd::UNIT_CANCEL_TARGET, &[], 0, NO_TIMEOUT);
                unit.set_fire_state(0, 0);
                unit.set_move_state(0, 0);
                unit.attack(target, 0);
                self.log(&format!("widow: unit {} takes a supported bite at heavy {id}", unit.id()));
                BiteAction::Active
            }
            Order::Keep => BiteAction::Active,
            Order::Resume => {
                // Reload proves the shot fired. Also end the detour on lost
                // vision/support, repairs, target death or the chase timeout.
                unit.stop(0);
                unit.set_fire_state(0, 0);
                BiteAction::Resume
            }
            Order::Idle => BiteAction::Idle,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn heavy(id: i32, stun_left: f32) -> Target {
        Target { id, value: 1000.0, stun_left }
    }

    #[test]
    fn only_nearby_friendly_threat_opens_a_bite() {
        assert!(!supported_nearby(0.0, 100.0, 120.0));
        assert!(!supported_nearby(f32::NAN, 100.0, 120.0));
        assert!(supported_nearby(0.01, 220.0, 120.0));
        assert!(!supported_nearby(10.0, 221.0, 120.0));
    }

    #[test]
    fn scouting_and_spotting_reserve_only_the_stuns_needed() {
        let mut b = Bites::default();
        let targets = [heavy(42, 12000.0)];
        assert_eq!(b.step(1, 0, true, &targets), Order::Attack(42));
        assert_eq!(b.step(2, 0, true, &targets), Order::Attack(42));
        assert_eq!(b.step(3, 0, true, &targets), Order::Idle);
        assert_eq!(b.step(1, 5, true, &targets), Order::Keep);
    }

    #[test]
    fn reload_or_loss_of_support_returns_control_and_blocks_immediate_retry() {
        for ready in [true, false] {
            let mut b = Bites::default();
            let targets = [heavy(42, 5000.0)];
            assert_eq!(b.step(1, 0, true, &targets), Order::Attack(42));
            let current = if ready { &[][..] } else { &targets[..] };
            assert_eq!(b.step(1, 5, ready, current), Order::Resume);
            assert_eq!(b.step(1, 10, true, &targets), Order::Idle);
            assert_eq!(b.step(1, 5 + RETRY, true, &targets), Order::Attack(42));
        }
    }

    #[test]
    fn timeout_and_removal_release_the_reservation() {
        let mut b = Bites::default();
        let targets = [heavy(42, 5000.0)];
        assert_eq!(b.step(1, 0, true, &targets), Order::Attack(42));
        // Another controller ticking first must not erase the expired
        // attack before its owner gets a chance to cancel the command.
        assert_eq!(b.step(2, TIMEOUT, true, &targets), Order::Attack(42));
        assert_eq!(b.step(1, TIMEOUT, true, &targets), Order::Resume);
        b.forget(2);
        assert_eq!(b.step(3, TIMEOUT, true, &targets), Order::Attack(42));
    }

    #[test]
    fn repairs_and_reserved_jobs_cannot_start_a_bite() {
        let mut b = Bites::default();
        assert_eq!(b.step(1, 0, false, &[heavy(42, 5000.0)]), Order::Idle);
    }
}
