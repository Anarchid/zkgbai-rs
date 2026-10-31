//! Optional squad launchers. The passengers jump; the Lobster walks back
//! to them. Neither squad formation nor ordinary movement waits for one.

use std::collections::BTreeMap;

use springai::{Unit, Vec3};

use super::{JumpDef, REACH, STALL, ground, param};
use crate::Ai;
use crate::kgbutil::{PathCost, kgb_move_to, kgb_move_to_exact};
use crate::military::{SquadId, SquadKind};
use crate::nav;
use crate::util::{NULLPOS, OPTION_SHIFT_KEY, distance};

const LOBSTER: &str = "amphlaunch";
// unit_thrower accelerates vertically for 22 frames, then sideways, with
// a nominal 125-frame flight and 15 extra frames of collision protection.
const FLIGHT: i32 = 155;
const GATHER_TIME: i32 = 90;
const GATHER_MARGIN: f32 = 24.0;
// A short throw can be the only way across a cliff. Tactical target
// selection has its own minimum distance; route execution must not use it.
const MIN_THROW: f32 = 32.0;
const TAKEOFF: f32 = 40.0;
const APPROACH_TIME: i32 = 30 * 20;

#[derive(Default)]
pub(super) struct Lobsters {
    throws: BTreeMap<i32, Throw>,
    /// Brief backoff after a refused/timed-out order. Actual reload is
    /// always read from the weapon, including slow/disarm effects.
    retry_after: BTreeMap<i32, i32>,
}

struct Throw {
    squad: SquadId,
    members: Vec<i32>,
    to: Vec3,
    /// Routed throws stage the launcher and passengers at the same take-off.
    takeoff: Option<Vec3>,
    resume: Vec3,
    escape: bool,
    phase: Phase,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    Approach(i32),
    Gather(i32),
    Aim(i32),
    Flight(i32),
}

impl Phase {
    fn start(frame: i32, launcher: Vec3, takeoff: Option<Vec3>) -> Self {
        if takeoff.is_some_and(|at| distance(launcher, at) > TAKEOFF) {
            Self::Approach(frame)
        } else {
            Self::Gather(frame)
        }
    }

    fn expired(self, frame: i32) -> bool {
        match self {
            Self::Approach(at) => frame - at >= APPROACH_TIME,
            Self::Gather(at) => frame - at >= GATHER_TIME,
            Self::Aim(at) => frame - at >= STALL,
            Self::Flight(at) => frame - at >= FLIGHT,
        }
    }
}

/// The gadget adds one common horizontal displacement to every passenger;
/// it preserves the formation's offsets, rather than stacking at the aim.
fn landing(from: Vec3, launcher: Vec3, aim: Vec3) -> Vec3 {
    Vec3::new(from.x + aim.x - launcher.x, aim.y, from.z + aim.z - launcher.z)
}

fn gathered(launcher: Vec3, passengers: &[Vec3], radius: f32) -> bool {
    !passengers.is_empty() && passengers.iter().all(|&p| distance(launcher, p) <= radius - GATHER_MARGIN)
}

fn grounded(middle: f32, ground: f32, height: f32) -> bool {
    middle - ground.max(-5.0) <= height + 12.0
}

fn throw_in_range(from: Vec3, to: Vec3, reach: f32) -> bool {
    (MIN_THROW..=reach).contains(&distance(from, to))
}

/// A path link describes one unit's landing at a cliff rim. Try nearby
/// aims so the whole formation can land on the far side. The caller also
/// verifies that every passenger can walk back to the planned landing.
fn landing_aim(planned: Vec3, from: Vec3, mut clear: impl FnMut(Vec3) -> bool) -> Option<Vec3> {
    if clear(planned) {
        return Some(planned);
    }
    let forward = (planned.z - from.z).atan2(planned.x - from.x);
    for radius in [32.0, 64.0, 96.0, 128.0, 160.0] {
        for k in 0..8 {
            let turn = (k + 1) / 2 * if k % 2 == 1 { 1 } else { -1 };
            let angle = forward + turn as f32 * std::f32::consts::FRAC_PI_4;
            let p = Vec3::new(planned.x + radius * angle.cos(), planned.y, planned.z + radius * angle.sin());
            if clear(p) {
                return Some(p);
            }
        }
    }
    None
}

impl Ai {
    fn lobster_members(&self, sid: SquadId) -> Option<Vec<Unit>> {
        let s = self.squads.get(sid)?;
        if s.kind != SquadKind::Assault || s.is_air_squad || s.fighters.is_empty() {
            return None;
        }
        s.fighters
            .iter()
            .map(|id| {
                let u = self.fighters.get(id)?.unit;
                (u.health() > 0.0
                    && u.team() == self.team_id
                    && !u.is_being_built()
                    && !self.retreat_handler.is_retreating(*id)
                    && !u.def()?.is_able_to_fly())
                .then_some(u)
            })
            .collect()
    }

    fn lobster_ready(&self, u: Unit) -> Option<(JumpDef, f32)> {
        if self.dev_off("lobsters")
            || self.dev_off("jumps")
            || u.health() <= 0.0
            || u.team() != self.team_id
            || u.is_being_built()
            || u.is_paralyzed()
            || self.retreat_handler.is_retreating(u.id())
            || u.rules_param_float("disarmed", 0.0) > 0.0
            || self.jumps.lobsters.retry_after.get(&u.id()).is_some_and(|&at| self.frame < at)
        {
            return None;
        }
        let d = u.def()?;
        let mount = param(d, "manualfire_num")? as i32 - 1;
        let weapon = u.weapon(mount)?;
        // The two launcher weapons share a reload in the unit script.
        if u.weapons().iter().any(|w| w.reload_frame() > self.frame) {
            return None;
        }
        let p = u.pos();
        // The C AI API reports the model's middle, whereas the unit script
        // tests its base against the ground. Allow the model's height here.
        if !grounded(p.y, self.cb.map().elevation_at(p.x, p.z), d.height()) {
            return None;
        }
        let range = weapon.range();
        let gather = param(d, "thrower_gather")?;
        if range <= 0.0 || gather <= GATHER_MARGIN {
            return None;
        }
        Some((
            JumpDef {
                range,
                // Conservative arc during the gadget's vertical acceleration.
                height: 0.8 * self.cb.map().gravity() * 125.0_f32.powi(2) / 7200.0,
                speed: 1.0,
                reload: 0,
                delay: 0,
                max_depth: d.max_water_depth(),
            },
            gather,
        ))
    }

    fn lobster_clear(&self, jd: JumpDef, lobster: Unit, passengers: &[Unit], to: Vec3) -> bool {
        let from = lobster.pos();
        if !throw_in_range(from, to, jd.reach()) {
            return false;
        }
        passengers.iter().all(|u| {
            let Some(def) = u.def() else { return false };
            let pos = u.pos();
            let end = landing(pos, from, to);
            // A common aim is only useful if every passenger can walk off
            // its own landing (including water restrictions).
            self.jump_clear(JumpDef { max_depth: def.max_water_depth(), ..jd }, pos, end)
                && self.cb.map().is_possible_to_build_at(def, ground(self, end), 0)
        })
    }

    fn lobster_safe(&self, to: Vec3, escape: bool, mine: f32) -> bool {
        escape || (self.war.get_porc_threat(to) <= 0.0 && self.war.get_enemy_power(to) <= super::PYRO_EDGE * mine)
    }

    fn lobster_route_landing(
        &mut self,
        jd: JumpDef,
        lobster: Unit,
        passengers: &[Unit],
        planned: Vec3,
        escape: bool,
        mine: f32,
    ) -> Option<Vec3> {
        let from = lobster.pos();
        landing_aim(planned, from, |to| {
            self.lobster_safe(to, escape, mine)
                && self.lobster_clear(jd, lobster, passengers, to)
                && passengers.iter().all(|u| {
                    let Some(def) = u.def() else { return false };
                    // Do not "fix" a cliff landing by putting a passenger
                    // back on the near side, or on an isolated ledge.
                    self.pathfinder.get_path_cost(&self.war, landing(u.pos(), from, to), planned, def) >= 0.0
                })
        })
    }

    /// Iris-style attachment, with one Lobster per amphib squad. It stays
    /// outside the member list, squad value, leader choice and rally checks.
    pub(crate) fn lobster_support(&mut self, id: i32) -> bool {
        let Some(f) = self.fighters.get(&id) else { return false };
        if self.defs.name_of(f.unit) != LOBSTER {
            return false;
        }
        if self.retreat_handler.is_retreating(id)
            || self.jumps.lobsters.throws.get(&id).is_some_and(|t| !matches!(t.phase, Phase::Flight(_)))
        {
            return true;
        }
        let pos = f.unit.pos();
        let ready = self.lobster_ready(f.unit).is_some();
        let gather = f.unit.def().and_then(|d| param(d, "thrower_gather")).unwrap_or(160.0);
        let eligible = |sid: SquadId| {
            self.squads.get(sid).is_some_and(|s| {
                s.kind == SquadKind::Assault
                    && !s.is_dead(&self.fighters)
                    && s.fighters
                        .iter()
                        .all(|m| self.fighters.get(m).is_some_and(|m| self.defs.name_of(m.unit).starts_with("amph")))
                    && !self.misc_handler.supports.iter().any(|&other| {
                        other != id
                            && self.fighters.get(&other).is_some_and(|f| {
                                if f.squad != Some(sid) || self.defs.name_of(f.unit) != LOBSTER {
                                    return false;
                                }
                                if self
                                    .jumps
                                    .lobsters
                                    .throws
                                    .get(&other)
                                    .is_some_and(|t| !matches!(t.phase, Phase::Flight(_)))
                                {
                                    return true;
                                }
                                let spos = s.get_pos(&self.fighters);
                                // A fresh launcher arriving with the squad
                                // can replace one stranded by its last shot.
                                let closer = distance(pos, spos) + gather < distance(f.unit.pos(), spos);
                                let rearmed = ready
                                    && distance(pos, spos) < gather - GATHER_MARGIN
                                    && self.lobster_ready(f.unit).is_none();
                                !closer && !rearmed
                            })
                    })
            })
        };
        let squad = f.squad.filter(|&sid| eligible(sid)).or_else(|| {
            self.squad_handler.squads.iter().copied().filter(|&sid| eligible(sid)).max_by(|a, b| {
                let score = |sid| {
                    self.squads
                        .get(sid)
                        .map_or(0.0, |s| s.metal_value / (1.0 + distance(pos, s.get_pos(&self.fighters)) / 1000.0))
                };
                score(*a).total_cmp(&score(*b))
            })
        });
        let target = squad
            .and_then(|sid| self.squads.get(sid))
            .map(|s| s.get_pos(&self.fighters))
            .unwrap_or_else(|| self.graph_get_closest_haven(pos));
        if let Some(sid) = squad {
            for &other in &self.misc_handler.supports {
                if other != id
                    && let Some(f) = self.fighters.get_mut(&other)
                    && f.squad == Some(sid)
                    && self.defs.name_of(f.unit) == LOBSTER
                {
                    f.squad = None;
                }
            }
        }
        let mut nav = nav!(self);
        if let Some(f) = self.fighters.get_mut(&id) {
            f.squad = squad;
            f.move_to(target, &mut nav);
        }
        true
    }

    /// Substitute a squad jump only while optional equipment is nearby and
    /// ready. A missing, reloading or distant Lobster leaves orders alone.
    pub(crate) fn lobster_squad_move(&mut self, sid: SquadId, target: Vec3, escape: bool) -> bool {
        if let Some((&id, t)) = self.jumps.lobsters.throws.iter().find(|(_, t)| t.squad == sid) {
            // Defense may interrupt gathering/aiming an offensive throw.
            if escape && !t.escape && !matches!(t.phase, Phase::Flight(_)) {
                self.lobster_cancel(id);
                return false;
            }
            if let Some(t) = self.jumps.lobsters.throws.get_mut(&id) {
                if escape || !t.escape {
                    t.resume = target;
                }
                t.escape |= escape;
            }
            return true;
        }
        if target == NULLPOS || self.dev_off("lobsters") || self.dev_off("jumps") || self.qrf_drafted(sid) {
            return false;
        }
        let Some(s) = self.squads.get(sid) else { return false };
        if !escape && s.status != 'a' {
            return false;
        }
        let Some(leader) = s.get_leader(&self.fighters) else { return false };
        let mine = self.war.get_total_friendly_power(s.get_pos(&self.fighters)) + s.get_threat(&self.fighters) as f32;
        let Some(passengers) = self.lobster_members(sid) else { return false };
        let lobster = self
            .misc_handler
            .supports
            .iter()
            .filter_map(|id| self.fighters.get(id))
            .find(|f| f.squad == Some(sid) && self.defs.name_of(f.unit) == LOBSTER)
            .map(|f| f.unit);
        let Some(lobster) = lobster else { return false };
        let Some((jd, radius)) = self.lobster_ready(lobster) else { return false };
        let from = lobster.pos();
        // Do not stop a squad to wait for a straggling launcher, nor try to
        // gather units still in flight from a previous/replacement Lobster.
        if passengers.iter().any(|u| {
            distance(from, u.pos()) > radius + 80.0
                || u.pos().y - self.cb.map().elevation_at(u.pos().x, u.pos().z).max(0.0) > 60.0
        }) {
            return false;
        }
        if escape && self.war.view(leader.def()).get_threat(leader.pos()) <= 0.0 {
            return false;
        }
        let (mut own_dps, mut own_range) = (0.0, 0.0_f32);
        for def in passengers.iter().filter_map(Unit::def) {
            let (dps, range) = self.jumps_gun(def);
            own_dps += dps;
            own_range = own_range.max(range);
        }
        let strike = (!escape)
            .then(|| self.jumps_pyro_target(from, jd.range, mine, own_dps, own_range))
            .flatten()
            .filter(|&to| self.lobster_clear(jd, lobster, &passengers, to));
        let route = if strike.is_none() && !self.dev_off("jumppath") {
            self.pathfinder.equipped_jump(&self.war, leader, target, PathCost::AvoidEnemies, jd)
        } else {
            None
        };
        // Keep a routed throw even when the current spread cannot land:
        // first move to its take-off and gather, then validate the actual
        // formation. Falling back to a walking order loses the jump edge.
        let routed = route.filter(|p| self.lobster_safe(p.to, escape, mine));
        let takeoff = routed.map(|p| p.from);
        let to = strike.or(routed.map(|p| p.to)).or_else(|| {
            self.jump_path(leader, target, jd.range * REACH, |ai, p| {
                ai.lobster_clear(jd, lobster, &passengers, p) && ai.lobster_safe(p, escape, mine)
            })
            .map(|(to, _)| to)
        });
        let Some(to) = to else { return false };
        let phase = Phase::start(self.frame, from, takeoff);
        if std::env::var_os("ZKGBAI_JUMP_TRACE").is_some() {
            self.log(&format!(
                "lobster: plan {} squad {sid} phase {phase:?} escape {escape} to ({:.0}, {:.0})",
                lobster.id(),
                to.x,
                to.z
            ));
        }
        let t = Throw {
            squad: sid,
            members: passengers.iter().map(|u| u.id()).collect(),
            to,
            takeoff,
            resume: target,
            escape,
            phase,
        };
        if let Some(s) = self.squads.get_mut(sid) {
            s.target = target;
        }
        self.jumps.lobsters.throws.insert(lobster.id(), t);
        if let Some(at) = takeoff.filter(|_| matches!(phase, Phase::Approach(_))) {
            // Fighter::move_to randomises the endpoint by 100 elmos
            // and queues only two waypoints. This leg must actually reach
            // the take-off while ordinary squad assignments are suspended.
            for u in passengers {
                self.lobster_approach(u, at, radius * 0.6);
            }
            self.lobster_approach(lobster, at, TAKEOFF * 0.5);
        } else {
            lobster.stop(0);
        }
        self.lobster_tick(lobster.id());
        true
    }

    fn lobster_approach(&mut self, u: Unit, at: Vec3, stop: f32) {
        let path = self.pathfinder.find_path(&self.war, u, at, PathCost::AvoidEnemies);
        let last = path.len().saturating_sub(1);
        for (i, wp) in path.into_iter().enumerate() {
            let opts = if i == 0 { 0 } else { OPTION_SHIFT_KEY };
            kgb_move_to_exact(u, wp, if i == last { stop } else { 16.0 }, opts, self.frame);
        }
    }

    fn lobster_resume(&mut self, t: Throw) {
        let mut nav = nav!(self);
        if let Some(s) = self.squads.get_mut(t.squad) {
            if t.escape {
                s.retreat_to(t.resume, &mut self.fighters, &mut nav);
            } else {
                s.set_target(t.resume, &mut self.fighters, &mut nav);
            }
        }
    }

    fn lobster_cancel(&mut self, id: i32) {
        if let Some(t) = self.jumps.lobsters.throws.remove(&id) {
            if std::env::var_os("ZKGBAI_JUMP_TRACE").is_some() {
                self.log(&format!("lobster: release {id} squad {} phase {:?}", t.squad, t.phase));
            }
            if !matches!(t.phase, Phase::Flight(_)) {
                if let Some(f) =
                    self.fighters.get(&id).filter(|f| f.unit.health() > 0.0 && f.unit.team() == self.team_id)
                {
                    f.unit.stop(0);
                }
                self.jumps.lobsters.retry_after.insert(id, self.frame + GATHER_TIME);
            }
            self.lobster_resume(t);
        }
    }

    fn lobster_tick(&mut self, id: i32) {
        let Some(t) = self.jumps.lobsters.throws.get(&id) else { return };
        if let Phase::Flight(_) = t.phase {
            // A dead launcher cannot cancel passengers already in flight.
            if t.phase.expired(self.frame) {
                self.lobster_cancel(id);
            }
            return;
        }
        let Some(lobster) =
            self.fighters.get(&id).map(|f| f.unit).filter(|u| u.health() > 0.0 && u.team() == self.team_id)
        else {
            self.lobster_cancel(id);
            return;
        };
        if matches!(t.phase, Phase::Aim(_)) && lobster.weapons().iter().any(|w| w.reload_frame() > self.frame) {
            let to = t.to;
            let sid = t.squad;
            self.jumps.lobsters.throws.get_mut(&id).unwrap().phase = Phase::Flight(self.frame);
            *self.jumps.counts.entry("lobster".to_owned()).or_default() += 1;
            self.jumps.total += 1;
            self.jumps.flew += 1;
            // The launcher stays behind, so start catching up immediately.
            let mut nav = nav!(self);
            if let Some(f) = self.fighters.get_mut(&id) {
                f.move_to(to, &mut nav);
            }
            if std::env::var_os("ZKGBAI_JUMP_TRACE").is_some() {
                self.log(&format!("lobster: fired {id} squad {sid}"));
            }
            return;
        }
        if t.phase.expired(self.frame) || self.lobster_ready(lobster).is_none() || self.qrf_drafted(t.squad) {
            self.lobster_cancel(id);
            return;
        }
        let Some(passengers) = self
            .lobster_members(t.squad)
            .filter(|units| units.len() == t.members.len() && units.iter().all(|u| t.members.contains(&u.id())))
        else {
            self.lobster_cancel(id);
            return;
        };
        let (jd, radius) = self.lobster_ready(lobster).unwrap();
        let mine = self.war.get_total_friendly_power(lobster.pos())
            + self.squads.get(t.squad).map_or(0.0, |s| s.get_threat(&self.fighters) as f32);
        let (to, escape, phase, takeoff) = (t.to, t.escape, t.phase, t.takeoff);
        if !self.lobster_safe(to, escape, mine) {
            self.lobster_cancel(id);
            return;
        }
        let pos = lobster.pos();
        if matches!(phase, Phase::Approach(_)) {
            let at = takeoff.expect("routed approach has a take-off");
            // The launcher can outrun its passengers. Keep their queued
            // approach alive until they arrive, rather than spending the
            // three-second gather budget while they are still walking.
            if distance(pos, at) > TAKEOFF || passengers.iter().any(|u| distance(pos, u.pos()) > radius) {
                return;
            }
            lobster.stop(0);
            self.jumps.lobsters.throws.get_mut(&id).unwrap().phase = Phase::Gather(self.frame);
        }
        if matches!(phase, Phase::Aim(_)) {
            if !self.lobster_clear(jd, lobster, &passengers, to) {
                self.lobster_cancel(id);
            }
            return;
        }
        // Whole-formation landing searches include walking checks; do
        // them at the gather-order cadence, not on every engine frame.
        if self.frame % 10 != (self.offset + 5) % 10 {
            return;
        }
        let positions: Vec<Vec3> = passengers.iter().map(Unit::pos).collect();
        // The current spread may straddle the cliff even though the
        // gathered formation fits. Keep gathering until a whole-squad
        // landing is clear, with the normal bounded gather timeout.
        let aim = if gathered(pos, &positions, radius) {
            if takeoff.is_some() {
                self.lobster_route_landing(jd, lobster, &passengers, to, escape, mine)
            } else {
                self.lobster_clear(jd, lobster, &passengers, to).then_some(to)
            }
        } else {
            None
        };
        if let Some(aim) = aim {
            // Stop passengers during the brief aim so they cannot walk out
            // of the cylinder between the readiness check and the shot.
            for u in passengers {
                u.stop(0);
            }
            if lobster.dgun_pos(ground(self, aim), 0) {
                let t = self.jumps.lobsters.throws.get_mut(&id).unwrap();
                t.to = aim;
                t.phase = Phase::Aim(self.frame);
            } else {
                self.lobster_cancel(id);
            }
        } else {
            let n = passengers.len() as f32;
            for (i, u) in passengers.iter().enumerate() {
                let angle = std::f32::consts::TAU * i as f32 / n;
                let p = Vec3::xz(pos.x + angle.cos() * radius * 0.4, pos.z + angle.sin() * radius * 0.4);
                // At a cliff rim, part of the gathering circle can lie
                // on the slope. Do not send a passenger there indefinitely.
                let p = if u.def().is_some_and(|d| self.cb.map().is_possible_to_build_at(d, ground(self, p), 0)) {
                    p
                } else {
                    pos
                };
                kgb_move_to(*u, ground(self, p), 0, self.frame);
            }
        }
    }

    pub(super) fn lobster_update(&mut self) {
        let ids: Vec<i32> = self.jumps.lobsters.throws.keys().copied().collect();
        for id in ids {
            self.lobster_tick(id);
        }
        self.jumps.lobsters.retry_after.retain(|_, at| *at > self.frame);
    }

    pub(super) fn lobster_remove_unit(&mut self, id: i32) {
        self.jumps.lobsters.retry_after.remove(&id);
        // Pending throws are unwound by lobster_update, which can restore
        // the survivors' normal orders after a launcher/member disappears.
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn routed_throw_approaches_even_when_landing_is_already_in_range() {
        let launcher = Vec3::xz(64.0, 256.0);
        let takeoff = Vec3::xz(256.0, 256.0);
        let to = Vec3::xz(384.0, 256.0);
        assert!(throw_in_range(launcher, to, 570.0));
        assert_eq!(Phase::start(100, launcher, Some(takeoff)), Phase::Approach(100));
        assert_eq!(Phase::start(200, takeoff, Some(takeoff)), Phase::Gather(200));
        assert_eq!(Phase::start(100, launcher, None), Phase::Gather(100));
    }

    #[test]
    fn routed_short_cliff_crossing_is_not_a_tactical_distance_decision() {
        let from = Vec3::xz(64.0, 256.0);
        let to = Vec3::xz(192.0, 256.0);
        // The planner admits links spanning two 64-elmo grid cells.
        assert!(distance(from, to) < super::super::MIN_ENGAGE);
        assert!(throw_in_range(from, to, 570.0));
        assert!(!throw_in_range(from, from, 570.0));
        assert!(!throw_in_range(from, Vec3::xz(640.0, 256.0), 570.0));
    }

    #[test]
    fn cliff_rim_landing_moves_inland_until_the_whole_squad_fits() {
        let launcher = Vec3::xz(64.0, 256.0);
        let planned = Vec3::xz(192.0, 256.0);
        let passengers = [Vec3::xz(4.0, 256.0), Vec3::xz(124.0, 256.0)];
        // The leader-sized link lands on the plateau at x >= 176, but
        // preserving the squad's offsets would leave one on the cliff.
        let clear = |aim| passengers.iter().all(|&p| landing(p, launcher, aim).x >= 176.0);
        assert!(!clear(planned));
        let aim = landing_aim(planned, launcher, clear).unwrap();
        assert_eq!(aim, Vec3::xz(256.0, 256.0));
        assert!(passengers.iter().all(|&p| landing(p, launcher, aim).x >= 176.0));
    }

    #[test]
    fn landing_adjustment_can_go_sideways_and_never_accepts_an_unsafe_candidate() {
        let launcher = Vec3::xz(100.0, 300.0);
        let planned = Vec3::xz(400.0, 300.0);
        // A building blocks the landing and the plateau ends ahead.
        let aim = landing_aim(planned, launcher, |p| p.z >= 364.0 && (p.x - 400.0).abs() < 1.0).unwrap();
        assert!((aim.x - 400.0).abs() < 1.0 && aim.z >= 364.0);
        assert_eq!(landing_aim(planned, launcher, |_| false), None);
        assert_eq!(landing_aim(planned, launcher, |_| true), Some(planned));
    }

    #[test]
    fn launch_requires_the_whole_squad_inside_the_gather_radius() {
        let p = Vec3::xz(500.0, 500.0);
        assert!(!gathered(p, &[], 160.0));
        assert!(gathered(p, &[p, Vec3::xz(630.0, 500.0)], 160.0));
        assert!(!gathered(p, &[p, Vec3::xz(650.0, 500.0)], 160.0));
        // Reload alone is insufficient after throwing: the launcher must
        // walk back to its passengers (or a replacement must reach them).
        let thrown = [Vec3::xz(1000.0, 500.0), Vec3::xz(1010.0, 500.0)];
        assert!(!gathered(p, &thrown, 160.0));
        assert!(gathered(Vec3::xz(990.0, 500.0), &thrown, 160.0));
    }

    #[test]
    fn every_passenger_keeps_its_offset_from_the_launcher() {
        let launcher = Vec3::xz(500.0, 500.0);
        let target = Vec3::xz(1000.0, 700.0);
        assert_eq!(landing(Vec3::xz(450.0, 530.0), launcher, target), Vec3::xz(950.0, 730.0));
        assert_eq!(landing(Vec3::xz(550.0, 470.0), launcher, target), Vec3::xz(1050.0, 670.0));
    }

    #[test]
    fn launcher_ground_check_allows_the_model_middle_above_its_base() {
        assert!(grounded(169.0, 155.0, 28.0));
        assert!(grounded(-20.0, -100.0, 28.0));
        assert!(!grounded(255.0, 155.0, 28.0));
        assert!(!grounded(100.0, -100.0, 28.0));
    }

    #[test]
    fn approach_gather_aim_and_flight_have_separate_bounded_lifetimes() {
        for (phase, duration) in [
            (Phase::Approach(100), APPROACH_TIME),
            (Phase::Gather(100), GATHER_TIME),
            (Phase::Aim(100), STALL),
            (Phase::Flight(100), FLIGHT),
        ] {
            assert!(!phase.expired(100 + duration - 1));
            assert!(phase.expired(100 + duration));
        }
    }
}
