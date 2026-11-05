//! Explicit combat permission (`raidcommit`): threat maps choose a route;
//! only coalesce's judged, named participants may deliberately enter combat.
//! Ordinary travel uses short MOVE orders and stays outside observed weapons.

use springai::{Unit, Vec3, reserved};

use crate::Ai;
use crate::kgbutil::pathfinder::PathCost;
use crate::military::wrappers::fighter::next_waypoint;
use crate::util::{NULLPOS, distance};

pub const FEATURE: &str = "raidcommit";
const STEP: f32 = 192.0;
const MARGIN: f32 = 96.0;
const SAMPLE: f32 = 16.0;

#[derive(Clone, Copy)]
struct Gun {
    pos: Vec3,
    radius: f32,
}

fn clearance(pos: Vec3, guns: &[Gun]) -> f32 {
    guns.iter().map(|g| distance(pos, g.pos) - g.radius).fold(f32::INFINITY, f32::min)
}

/// Every sample must remain outside coverage, or improve the worst overlap
/// when already caught. This also catches a route crossing a gun's disc
/// whose endpoint happens to be safe on the other side.
fn clip(pos: Vec3, target: Vec3, guns: &[Gun]) -> Vec3 {
    let delta = target - pos;
    let len = delta.length_2d().min(STEP);
    if len < 1.0 {
        return pos;
    }
    let step = delta * (1.0 / delta.length_2d());
    let mut last = pos;
    let mut floor = clearance(pos, guns).min(0.0);
    let steps = (len / SAMPLE).ceil() as usize;
    for i in 1..=steps {
        let p = pos + step * (len * i as f32 / steps as f32);
        let c = clearance(p, guns);
        if c < floor {
            break;
        }
        floor = c.min(0.0);
        last = p;
    }
    last
}

/// Choose a common rendezvous direction while safe; escape the most exposed
/// position first when a mobile enemy has closed the distance.
fn steer(pos: Vec3, goal: Vec3, guns: &[Gun]) -> Vec3 {
    let direct = clip(pos, goal, guns);
    if clearance(pos, guns) >= 0.0 {
        return direct;
    }
    let mut best = direct;
    let mut score = clearance(best, guns);
    for i in 0..16 {
        let angle = i as f32 * std::f32::consts::TAU / 16.0;
        let candidate = clip(pos, pos + Vec3::xz(angle.cos() * STEP, angle.sin() * STEP), guns);
        let c = clearance(candidate, guns);
        if c > score || (c == score && distance(candidate, goal) < distance(best, goal)) {
            best = candidate;
            score = c;
        }
    }
    best
}

impl Ai {
    pub(crate) fn raidcommit_on(&self) -> bool {
        !self.dev_off(FEATURE)
    }

    /// Preserve specialized air, submerged and cloaked-raider controllers.
    pub(crate) fn raidcommit_ground(&self, unit: Unit) -> bool {
        self.raidcommit_on()
            && unit.pos().y >= -5.0
            && !(unit.is_cloaked() && self.defs.name_of(unit) == "cloakheavyraid")
            && unit.def().is_some_and(|d| !d.is_able_to_fly())
    }

    pub(crate) fn raidcommit_available(&self, unit: Unit) -> bool {
        self.raidcommit_ground(unit)
            && unit.health() > 0.0
            && unit.team() == self.team_id
            && !unit.is_being_built()
            && !reserved::is_reserved(self.cb, unit.id())
            && !self.coalesce_owns(unit.id())
            && !self.recovery_owns(unit.id())
            && !self.retreat_is_retreating(unit)
            && !self.precision.missions.contains_key(&unit.id())
            && !self.squad_strike_owns(unit.id())
            && !self.fire_controls(unit.id())
            && !self.limpet_controls(unit.id())
    }

    pub(crate) fn raidcommit_move(&mut self, unit: Unit, goal: Vec3) {
        if !self.raidcommit_available(unit) {
            return;
        }
        let pos = unit.pos();
        let goal = if goal == NULLPOS { pos } else { goal };
        let guns: Vec<_> = self
            .war
            .targets
            .values()
            .filter(|e| !e.is_dead && (e.is_static || self.frame - e.last_seen <= 300))
            .filter(|e| !e.identified || e.surface_danger(e.is_submerged()) > 0.0)
            .map(|e| Gun {
                pos: e.position,
                radius: if e.identified { e.def_max_weapon_range() + MARGIN } else { 400.0 },
            })
            .collect();
        let mut path = self.pathfinder.find_path(&self.war, unit, goal, PathCost::AvoidEnemies);
        let waypoint = next_waypoint(&mut path, 1).unwrap_or(pos);
        let mut to = steer(pos, waypoint, &guns);
        // The escape direction must also be traversable. A blocked escape
        // cancels pursuit in place rather than restoring the rejected FIGHT.
        if clearance(pos, &guns) < 0.0 && distance(to, waypoint) > SAMPLE {
            let mut escape = self.pathfinder.find_path(&self.war, unit, to, PathCost::AvoidEnemies);
            to = clip(pos, next_waypoint(&mut escape, 0).unwrap_or(pos), &guns);
        }
        if let Some(rim) = self.pathfinder.no_go_exit(to, pos) {
            to = rim;
        }
        to.y = self.cb.map().elevation_at(to.x, to.z);
        // No SHIFT and no kgb_move_to jump/dash: the checked short segment is
        // the entire movement order. Even a failed route removes old attacks.
        if self.qrf.members.contains_key(&unit.id()) {
            self.qrf_order(unit.id(), to, false);
        } else {
            unit.move_to(to, 0);
        }
        if let Some(f) = self.fighters.get_mut(&unit.id()) {
            f.target = goal;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn travel_cannot_cross_coverage_to_a_safe_endpoint() {
        let start = Vec3::xz(100.0, 100.0);
        let goal = Vec3::xz(280.0, 100.0);
        let guns = [Gun { pos: Vec3::xz(190.0, 100.0), radius: 40.0 }];
        let to = clip(start, goal, &guns);
        assert!(to.x < 150.0);
        assert!(clearance(to, &guns) >= 0.0);
    }

    #[test]
    fn exposed_raider_escapes_instead_of_finishing_its_attack() {
        let pos = Vec3::xz(400.0, 400.0);
        let gun = Gun { pos: Vec3::xz(500.0, 400.0), radius: 245.0 + MARGIN };
        let to = steer(pos, gun.pos, &[gun]);
        assert!(to.x < pos.x);
        assert!(clearance(to, &[gun]) > clearance(pos, &[gun]));
    }

    #[test]
    fn rendezvous_stops_outside_all_covering_guns() {
        let pos = Vec3::xz(100.0, 100.0);
        let guns = [
            Gun { pos: Vec3::xz(500.0, 100.0), radius: 245.0 + MARGIN },
            Gun { pos: Vec3::xz(100.0, 500.0), radius: 210.0 + MARGIN },
        ];
        let to = steer(pos, Vec3::xz(400.0, 400.0), &guns);
        assert!(clearance(to, &guns) >= 0.0);
    }

    #[test]
    fn clear_travel_is_bounded_and_zero_length_stops() {
        let pos = Vec3::xz(100.0, 100.0);
        assert_eq!(clip(pos, pos, &[]), pos);
        assert!((distance(clip(pos, Vec3::xz(1000.0, 100.0), &[]), pos) - STEP).abs() < 0.01);
    }
}
