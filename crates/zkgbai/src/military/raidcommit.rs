//! Explicit combat permission (`raidcommit`): threat maps choose a route;
//! only coalesce's judged, named participants may deliberately enter combat.
//! Ordinary travel queues a continuous safe route outside observed weapons.

use springai::{Unit, Vec3, reserved};

use crate::Ai;
use crate::kgbutil::kgb_move_to;
use crate::kgbutil::pathfinder::PathCost;
use crate::military::wrappers::fighter::next_waypoint;
use crate::util::{NULLPOS, OPTION_SHIFT_KEY, distance};

pub const FEATURE: &str = "raidcommit";
const ESCAPE_STEP: f32 = 192.0;
const MARGIN: f32 = 32.0;
const SAMPLE: f32 = 16.0;
const CMD_MOVE: i32 = 10;

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
    let len = delta.length_2d();
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
        let candidate = clip(pos, pos + Vec3::xz(angle.cos() * ESCAPE_STEP, angle.sin() * ESCAPE_STEP), guns);
        let c = clearance(candidate, guns);
        if c > score || (c == score && distance(candidate, goal) < distance(best, goal)) {
            best = candidate;
            score = c;
        }
    }
    best
}

/// Keep the path's bends, stopping at the first unsafe segment. The queue
/// reaches the objective in open ground instead of stopping every 192 elmos.
fn checked_route(pos: Vec3, path: impl IntoIterator<Item = Vec3>, guns: &[Gun]) -> Vec<Vec3> {
    let mut route = Vec::new();
    let mut at = pos;
    for waypoint in path {
        let to = clip(at, waypoint, guns);
        if distance(at, to) >= 1.0 {
            route.push(to);
        }
        if distance(to, waypoint) >= 1.0 {
            break;
        }
        at = to;
    }
    if route.is_empty() {
        route.push(pos);
    }
    route
}

fn route_still_safe(pos: Vec3, queued: &[Vec3], desired: &[Vec3], guns: &[Gun]) -> bool {
    if queued.is_empty() || desired.is_empty() || distance(*queued.last().unwrap(), *desired.last().unwrap()) > 32.0 {
        return false;
    }
    let mut at = pos;
    for &to in queued {
        if distance(clip(at, to, guns), to) >= 1.0 {
            return false;
        }
        at = to;
    }
    true
}

/// ZK's raw move owns movement around units and structures. Queuing engine
/// MOVE commands makes its gadget repeatedly translate/restart the queue.
fn send_route(unit: Unit, route: &[Vec3], frame: i32) {
    for (i, to) in route.iter().enumerate() {
        kgb_move_to(unit, *to, if i == 0 { 0 } else { OPTION_SHIFT_KEY }, frame);
    }
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
            && !self.harassment_owns(unit.id())
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
        let keepout: Vec<_> = guns.iter().map(|g| (g.pos, g.radius)).collect();
        self.pathfinder.walk_only = true;
        let path = self.pathfinder.find_path_keepout(&self.war, unit, goal, PathCost::AvoidEnemies, &keepout);
        let mut route = checked_route(pos, path, &guns);
        if clearance(pos, &guns) < 0.0 {
            // An enemy closing on us still calls for a short, checked escape.
            let escape_goal = steer(pos, route[0], &guns);
            self.pathfinder.walk_only = true;
            let mut escape = self.pathfinder.find_path(&self.war, unit, escape_goal, PathCost::AvoidEnemies);
            route = vec![clip(pos, next_waypoint(&mut escape, 0).unwrap_or(pos), &guns)];
        }
        let mut checked = Vec::new();
        let mut at = pos;
        for mut to in route {
            let rim = self.pathfinder.no_go_exit(to, at);
            if let Some(rim) = rim {
                to = rim;
            }
            to.y = self.cb.map().elevation_at(to.x, to.z);
            checked.push(to);
            at = to;
            if rim.is_some() {
                break;
            }
        }
        // Replacing the same MOVE queue twice a second resets engine pathing
        // and braking. Keep it while its destination and every segment remain
        // safe; new threats and old FIGHT orders invalidate it immediately.
        let queued: Option<Vec<Vec3>> = unit
            .current_commands()
            .iter()
            .map(|c| {
                let p = c.params();
                (matches!(c.id(), CMD_MOVE | crate::defs::cmd::RAW_MOVE) && p.len() >= 3)
                    .then(|| Vec3::new(p[0], p[1], p[2]))
            })
            .collect();
        let keep = queued.as_ref().is_some_and(|q| {
            route_still_safe(pos, q, &checked, &guns) && q.iter().all(|p| self.pathfinder.no_go_at(*p).is_none())
        });
        if !keep {
            send_route(unit, &checked, self.frame);
            self.qrf_commit_moved(unit.id(), *checked.last().unwrap());
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
    fn routes_issue_raw_move_and_replace_then_queue() {
        use springai::ffi;
        use std::{cell::RefCell, ffi::c_void};
        thread_local! {
            static SENT: RefCell<Vec<(i32, i32, i16, Vec<f32>)>> = const { RefCell::new(Vec::new()) };
        }
        unsafe extern "C" fn command(_: i32, _: i32, _: i32, topic: i32, data: *mut c_void) -> i32 {
            if topic == ffi::CommandTopic::COMMAND_UNIT_CUSTOM as i32 {
                let c = unsafe { &*data.cast::<ffi::SCustomUnitCommand>() };
                let params = unsafe { std::slice::from_raw_parts(c.params, c.params_size as usize) }.to_vec();
                SENT.with_borrow_mut(|s| s.push((topic, c.cmdId, c.options, params)));
            } else {
                SENT.with_borrow_mut(|s| s.push((topic, -1, 0, Vec::new())));
            }
            0
        }
        let ai = crate::economy::test_ai_with(|t| t.Engine_handleCommand = Some(command));
        let unit = Unit::from_id(ai.cb, 42).unwrap();
        let route = [Vec3::xz(100.0, 200.0), Vec3::xz(900.0, 200.0)];
        SENT.with_borrow_mut(Vec::clear);
        send_route(unit, &route, 300);
        let sent = SENT.with_borrow(Clone::clone);
        assert_eq!(sent.len(), 2);
        for (i, (_, cmd, opts, params)) in sent.iter().enumerate() {
            assert_eq!(*cmd, crate::defs::cmd::RAW_MOVE);
            assert_ne!(*opts & springai::opt::RIGHT_MOUSE_KEY, 0);
            assert_eq!(*opts & OPTION_SHIFT_KEY != 0, i > 0);
            assert_eq!(params, &[route[i].x, route[i].y, route[i].z]);
        }
    }

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
    fn clear_travel_reaches_the_objective_without_short_stops() {
        let pos = Vec3::xz(100.0, 100.0);
        let goal = Vec3::xz(2000.0, 100.0);
        assert_eq!(clip(pos, pos, &[]), pos);
        let route = checked_route(pos, [goal], &[]);
        assert_eq!(route.len(), 1);
        assert!(distance(route[0], goal) < 0.01);
    }

    #[test]
    fn continuous_route_keeps_bends_and_clips_only_at_danger() {
        let pos = Vec3::xz(100.0, 100.0);
        let bend = Vec3::xz(100.0, 800.0);
        let goal = Vec3::xz(2000.0, 800.0);
        let gun = Gun { pos: Vec3::xz(1600.0, 800.0), radius: 300.0 };
        let route = checked_route(pos, [bend, goal], &[gun]);
        assert_eq!(route[0], bend);
        assert!(route[1].x > 1200.0 && route[1].x <= 1300.0);
        assert!(clearance(route[1], &[gun]) >= 0.0);
        assert!(route_still_safe(pos, &route, &route, &[gun]));
        let moved = Gun { pos: Vec3::xz(1000.0, 800.0), radius: 300.0 };
        assert!(!route_still_safe(pos, &route, &route, &[moved]));
        assert!(!route_still_safe(pos, &route, &[Vec3::xz(500.0, 100.0)], &[gun]));
    }
}
