//! Pyros can burn remembered buildings through terrain. A coordinate target
//! bypasses the unit target's visibility requirement without changing movement.

use std::collections::BTreeMap;

use springai::{NO_TIMEOUT, Unit, Vec3};

use crate::Ai;
use crate::defs::cmd;
use crate::military::handlers::precision::Phase;
use crate::util::distance;

const UPDATE_FRAMES: i32 = 15;
const CONTACT_MEMORY: i32 = 30 * 20;
const CLEARANCE: f32 = 150.0;
const RANGE_SLACK: f32 = 16.0;

#[derive(Default)]
#[checkpoint::state]
pub(super) struct BlindFire {
    active: BTreeMap<i32, Aim>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
#[checkpoint::state]
struct Aim {
    target: i32,
    pos: Vec3,
}

struct Contact {
    id: i32,
    pos: Vec3,
    identified: bool,
    stationary: bool,
    visible: bool,
    radar: bool,
    dead: bool,
    last_seen: i32,
}

/// No speculative mobile targets. Even unidentified radar contacts take
/// priority over shooting blindly; old mobile memories eventually expire.
fn choose(pos: Vec3, range: f32, frame: i32, contacts: &[Contact], mission: Option<i32>) -> Option<Aim> {
    contacts
        .iter()
        .filter(|e| e.identified && e.stationary && !e.visible && !e.dead)
        .filter(|e| mission.is_none_or(|target| target == e.id))
        // Ground Settarget snaps Y to terrain. Include the uphill range cost
        // (Pyro heightMod = 1) rather than using only horizontal distance.
        .filter(|e| {
            let horizontal = distance(pos, e.pos);
            horizontal.hypot(e.pos.y - pos.y) <= (range - RANGE_SLACK).max(0.0)
        })
        .filter(|e| {
            !contacts.iter().any(|other| {
                other.id != e.id
                    && !other.dead
                    && (other.stationary || other.visible || other.radar || frame - other.last_seen < CONTACT_MEMORY)
                    && distance(pos, other.pos) < range + CLEARANCE
            })
        })
        .min_by(|a, b| distance(pos, a.pos).total_cmp(&distance(pos, b.pos)).then(a.id.cmp(&b.id)))
        .map(|e| Aim { target: e.id, pos: e.pos })
}

/// The gadget publishes its current target. Never cancel another controller's
/// replacement ground target, or overwrite one to start this fallback.
fn owns_ground(unit: Unit, aim: Aim) -> bool {
    same_ground(
        aim,
        unit.rules_param_float("target_type", 0.0),
        unit.rules_param_float("target_x", f32::NAN),
        unit.rules_param_float("target_z", f32::NAN),
    )
}

fn same_ground(aim: Aim, kind: f32, x: f32, z: f32) -> bool {
    kind == 1.0 && (x - aim.pos.x).abs() < 0.5 && (z - aim.pos.z).abs() < 0.5
}

impl Ai {
    pub(super) fn pyro_blind_fire_update(&mut self, frame: i32) {
        if frame % UPDATE_FRAMES != (self.offset + 11) % UPDATE_FRAMES {
            return;
        }
        let enabled = !self.dev_off("precision");
        let mut ids: Vec<_> = self
            .fighters
            .values()
            .filter(|f| self.defs.name_of(f.unit) == "jumpraid")
            .map(|f| f.id)
            .chain(self.precision.blind_fire.active.keys().copied())
            .collect();
        ids.sort_unstable();
        ids.dedup();
        if ids.is_empty() {
            return;
        }
        let contacts: Vec<_> = self
            .war
            .targets
            .values()
            .map(|e| Contact {
                id: e.unit_id,
                pos: if e.is_static {
                    Vec3 { y: self.cb.map().elevation_at(e.position.x, e.position.z), ..e.position }
                } else {
                    e.position
                },
                identified: e.identified,
                stationary: e.is_static,
                visible: e.visible,
                radar: e.is_radar_visible,
                dead: e.is_dead,
                last_seen: e.last_seen,
            })
            .collect();
        for id in ids {
            let previous = self.precision.blind_fire.active.get(&id).copied();
            let fighter = self.fighters.get(&id);
            let Some(unit) = fighter.map(|f| f.unit).or_else(|| Unit::from_id(self.cb, id)) else {
                self.precision.blind_fire.active.remove(&id);
                continue;
            };
            if unit.health() <= 0.0 || unit.team() != self.team_id {
                self.precision.blind_fire.active.remove(&id);
                continue;
            }
            if springai::reserved::is_reserved(self.cb, id) {
                // Keep ownership bookkeeping until commands are permitted
                // again; the operator may meanwhile replace the target.
                continue;
            }
            let mission = self.precision.missions.get(&id);
            let allowed = enabled
                && fighter.is_some()
                && self.defs.name_of(unit) == "jumpraid"
                && !self.retreat_handler.is_retreating(id)
                && mission.is_none_or(|m| m.phase == Phase::Fire);
            let next = if allowed {
                unit.weapon(0).and_then(|w| choose(unit.pos(), w.range(), frame, &contacts, mission.map(|m| m.target)))
            } else {
                None
            };
            let target_type = unit.rules_param_float("target_type", 0.0);
            let own = previous.is_some_and(|aim| owns_ground(unit, aim));
            let next = next.filter(|aim| {
                target_type == 0.0
                    || own
                    || (target_type == 2.0 && unit.rules_param_float("target_id", -1.0) == aim.target as f32)
            });
            if let Some(aim) = next {
                if own && previous == Some(aim) {
                    continue;
                }
                // Restore a dropped/replaced unit target after the normal
                // controllers run, without replacing their movement queue.
                if unit.execute_custom_command(cmd::UNIT_SET_TARGET, &[aim.pos.x, aim.pos.y, aim.pos.z], 0, NO_TIMEOUT)
                {
                    self.precision.blind_fire.active.insert(id, aim);
                    if previous != Some(aim) {
                        self.log(&format!("precision: Pyro {id} ground-targets unseen building {}", aim.target));
                    }
                }
            } else {
                if own && !unit.execute_custom_command(cmd::UNIT_CANCEL_TARGET, &[], 0, NO_TIMEOUT) {
                    // A mute or command lease may temporarily prevent cleanup.
                    continue;
                }
                self.precision.blind_fire.active.remove(&id);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lotus() -> Contact {
        Contact {
            id: 42,
            pos: Vec3 { x: 180.0, y: 60.0, z: 0.0 },
            identified: true,
            stationary: true,
            visible: false,
            radar: false,
            dead: false,
            last_seen: 10,
        }
    }

    #[test]
    fn remembered_uphill_static_needs_neither_los_nor_radar() {
        let e = lotus();
        assert_eq!(choose(Vec3::default(), 240.0, 1000, &[e], None).unwrap().target, 42);
    }

    #[test]
    fn visibility_identity_mobility_death_range_and_mission_gate_blind_fire() {
        for change in 0..6 {
            let mut e = lotus();
            match change {
                0 => e.visible = true,
                1 => e.identified = false,
                2 => e.stationary = false,
                3 => e.dead = true,
                4 => e.pos.x = 230.0,
                _ => e.pos.y = 200.0,
            }
            assert!(choose(Vec3::default(), 240.0, 1000, &[e], None).is_none());
        }
        assert!(choose(Vec3::default(), 240.0, 1000, &[lotus()], Some(43)).is_none());
    }

    #[test]
    fn nearby_contacts_yield_priority_even_when_unidentified() {
        let mut other = lotus();
        other.id = 43;
        other.stationary = false;
        other.identified = false;
        other.radar = true;
        other.last_seen = 0;
        let mut contacts = [lotus(), other];
        assert!(choose(Vec3::default(), 240.0, 1000, &contacts, None).is_none());
        contacts[1].radar = false;
        contacts[1].last_seen = 999;
        assert!(choose(Vec3::default(), 240.0, 1000, &contacts, None).is_none());
        contacts[1].last_seen = 0;
        assert!(choose(Vec3::default(), 240.0, 1000, &contacts, None).is_some());
        contacts[1].stationary = true;
        assert!(choose(Vec3::default(), 240.0, 1000, &contacts, None).is_none());
        contacts[1].dead = true;
        assert!(choose(Vec3::default(), 240.0, 1000, &contacts, None).is_some());
    }

    #[test]
    fn active_coordinate_ownership_survives_checkpoint_and_next_selection() {
        let aim = choose(Vec3::default(), 240.0, 1000, &[lotus()], None).unwrap();
        let state = BlindFire { active: BTreeMap::from([(7, aim)]) };
        let bytes = checkpoint::encode(&state).unwrap();
        let restored: BlindFire = checkpoint::rmp_serde::from_slice(&bytes).unwrap();
        assert_eq!(checkpoint::encode(&restored).unwrap(), bytes);
        assert_eq!(restored.active[&7], aim);
        // After loading, cancellation still recognizes only our coordinate
        // target, never a replacement unit target or another ground position.
        assert!(same_ground(restored.active[&7], 1.0, aim.pos.x, aim.pos.z));
        assert!(!same_ground(restored.active[&7], 2.0, aim.pos.x, aim.pos.z));
        assert!(!same_ground(restored.active[&7], 1.0, aim.pos.x + 10.0, aim.pos.z));
        assert!(!same_ground(restored.active[&7], 1.0, f32::NAN, f32::NAN));
        assert_eq!(choose(Vec3::default(), 240.0, 1015, &[lotus()], Some(restored.active[&7].target)), Some(aim));
        // A missing ownership field must not silently forget an engine order.
        let missing = checkpoint::encode(&BTreeMap::<String, u8>::new()).unwrap();
        assert!(checkpoint::rmp_serde::from_slice::<BlindFire>(&missing).is_err());
    }
}
