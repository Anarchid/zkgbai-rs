//! Sacrificial reconnaissance. Five minutes without LOS triggers deep-economy
//! and anti-nuke dives; a missile at 90% asks for a fresh pre-launch look.
//! Straight ingress is sampled for the first AA contact. Sparrow must cover
//! the remaining depth before its fuse expires, including acceleration and a
//! flight margin. Swift can continue at cruise speed after its shorter boost.

use std::collections::BTreeMap;

use springai::{Unit, UnitDef, Vec3};

use super::burst_order;
use crate::Ai;
use crate::kgbutil::kgb_move_to;
use crate::military::scouts::SPARROW;
use crate::util::{NULLPOS, distance};

const STALE: i32 = 30 * 300;
const FRESH: i32 = 30 * 30;
const RETRY_NUKE: i32 = 30 * 90;
const TIMEOUT: i32 = 30 * 60;
const NUKE_WAIT: i32 = 30 * 45;
const SPREAD: f32 = 1000.0;
const STEP: f32 = 128.0;
const SWIFT: &str = "planefighter";

#[derive(Clone, Copy, PartialEq, Debug)]
enum Purpose {
    Economy,
    Anti,
    Nuke,
}

#[derive(Clone, Copy)]
struct Attempt {
    target: Vec3,
    started: i32,
    purpose: Purpose,
}

#[derive(Clone, Copy)]
struct Dive {
    unit: Unit,
    attempt: Attempt,
    swift: bool,
    boosted: bool,
    burst_range: f32,
}

#[derive(Default)]
pub(super) struct Recon {
    active: BTreeMap<i32, Dive>,
    attempts: Vec<Attempt>,
    swift_wanted_until: Option<i32>,
}

pub(crate) fn dive_budget(density: f32, income: f32) -> usize {
    if density <= 2000.0 { 0 } else { 1 + (income.max(0.0) / 100.0).floor().min(2.0) as usize }
}

pub(crate) fn recon_ready(stock: i32, queued: i32, progress: f32) -> bool {
    stock > 0 || (queued > 0 && progress >= 0.9)
}

/// Integrate acceleration up to the speed cap; never assume instant 5x speed.
fn burst_distance(speed: f32, multiplier: f32, acceleration: f32, seconds: f32) -> f32 {
    if speed <= 0.0 || seconds <= 0.0 {
        return 0.0;
    }
    let top = speed * multiplier.max(1.0);
    let ramp = if acceleration > 0.0 { ((top - speed) / acceleration).min(seconds) } else { seconds };
    let reached = (speed + acceleration.max(0.0) * ramp).min(top);
    (speed + reached) * 0.5 * ramp + top * (seconds - ramp)
}

fn burst_range(def: UnitDef) -> f32 {
    let param = |name: &str, default: f32| {
        def.custom_param(name).and_then(|v| v.parse::<f32>().ok()).filter(|v| v.is_finite()).unwrap_or(default)
    };
    let mult = param("boost_speed_mult", 1.0);
    // Engine acceleration is elmos/frame²; speed is elmos/second.
    let acceleration = def.max_acceleration() * 30.0 * 30.0 * param("boost_accel_mult", mult);
    // Allow for drag, steering and the half-second steering cadence.
    0.8 * burst_distance(def.speed(), mult, acceleration, param("boost_duration", 0.0) / 30.0)
}

fn continuing_range(burst: f32, boost_seconds: f32, cruise: f32, deadline: f32) -> f32 {
    burst + cruise.max(0.0) * (deadline - boost_seconds).max(0.0)
}

/// Distance from the first dangerous sample (with a one-cell lead) to the aim.
fn penetration_depth(from: Vec3, target: Vec3, dangerous: impl Fn(Vec3) -> bool) -> f32 {
    let length = distance(from, target);
    let n = (length / STEP).ceil().max(1.0) as usize;
    for i in 0..=n {
        let t = i as f32 / n as f32;
        let p = Vec3::xz(from.x + (target.x - from.x) * t, from.z + (target.z - from.z) * t);
        if dangerous(p) {
            return (length * (1.0 - t) + STEP).min(length);
        }
    }
    0.0
}

fn retry_allowed(attempts: &[Attempt], target: Vec3, purpose: Purpose, frame: i32) -> bool {
    let cooldown = if purpose == Purpose::Nuke { RETRY_NUKE } else { STALE };
    attempts.iter().all(|a| {
        (purpose == Purpose::Nuke && a.purpose != Purpose::Nuke)
            || distance(a.target, target) >= SPREAD
            || frame - a.started >= cooldown
    })
}

fn nuke_wait(attempts: &[Attempt], target: Vec3, seen: i32, frame: i32) -> bool {
    frame - seen >= FRESH
        && attempts
            .iter()
            .any(|a| distance(a.target, target) < 640.0 && frame - a.started < NUKE_WAIT && seen < a.started)
}

fn stale_economy(frame: i32, observations: impl Iterator<Item = i32>) -> bool {
    observations.max().is_some_and(|seen| frame - seen >= STALE)
}

fn burst_aligned(from: Vec3, target: Vec3, velocity: Vec3, cruise: f32) -> bool {
    let speed = velocity.x.hypot(velocity.z);
    let length = distance(from, target);
    speed >= cruise * 0.7
        && length > 0.0
        && ((target.x - from.x) * velocity.x + (target.z - from.z) * velocity.z) >= 0.9 * length * speed
}

impl Ai {
    pub(crate) fn sweeps_claims(&self, id: i32) -> bool {
        self.sweeps.detonating.contains(&id) || self.sweeps.recon.active.contains_key(&id)
    }

    pub(crate) fn sweeps_forget(&mut self, id: i32) {
        self.sweeps.detonating.remove(&id);
        self.sweeps.recon.active.remove(&id);
    }

    pub(crate) fn sweeps_wants_swift(&self) -> bool {
        !self.dev_off("scouts")
            && self.sweeps.recon.swift_wanted_until.is_some_and(|until| self.frame < until)
            && self.scouts_air_needed(SWIFT, 1)
    }

    fn sweeps_dangerous(&self, p: Vec3) -> bool {
        self.war.get_aa_threat(p) > 0.0
            || self.war.targets.values().any(|e| {
                !e.is_dead
                    && e.can_fly()
                    && (e.is_aa || e.is_flex_aa)
                    && self.frame - e.last_seen < 30 * 20
                    && distance(e.position, p) < e.threat_radius.max(600.0) + e.def_speed() * 5.0
            })
    }

    /// Actual LOS, including allied and cloaked scouts, supplies freshness.
    /// Several points prevent one visible building from clearing the whole area.
    fn sweeps_area_seen(&self, p: Vec3) -> i32 {
        let map = self.cb.map();
        [(0.0, 0.0), (400.0, 0.0), (-400.0, 0.0), (0.0, 400.0), (0.0, -400.0)]
            .into_iter()
            .map(|(x, z)| {
                Vec3::xz(
                    (p.x + x).clamp(0.0, (map.width() * 8 - 1).max(0) as f32),
                    (p.z + z).clamp(0.0, (map.height() * 8 - 1).max(0) as f32),
                )
            })
            .map(|p| self.scouts_last_seen(p))
            .min()
            .unwrap_or(-STALE)
    }

    fn sweeps_available(&self, u: Unit, purpose: Purpose) -> bool {
        let preempt = purpose == Purpose::Nuke
            && self.sweeps.recon.active.get(&u.id()).is_some_and(|d| !d.boosted && d.attempt.purpose != Purpose::Nuke);
        u.health() > 0.0
            && u.team() == self.team_id
            && !u.is_being_built()
            && (!self.sweeps_claims(u.id()) || preempt)
            && !self.retreat_is_retreating(u)
            && !springai::reserved::is_reserved(self.cb, u.id())
            && !self.airstrike_claims(u.id())
    }

    fn sweeps_request(&mut self, target: Vec3, purpose: Purpose) {
        if self.dev_off("scouts")
            || !retry_allowed(&self.sweeps.recon.attempts, target, purpose, self.frame)
            || self.sweeps.recon.active.values().any(|d| distance(d.attempt.target, target) < SPREAD)
        {
            return;
        }
        let Some(sparrow_def) = self.defs.get(SPARROW) else { return };
        let range = burst_range(sparrow_def);
        let mut sparrows: Vec<_> = self
            .eco
            .sparrows
            .iter()
            .copied()
            .filter(|&u| self.defs.name_of(u) == SPARROW && self.sweeps_available(u, purpose))
            .collect();
        sparrows.sort_by(|a, b| distance(a.pos(), target).total_cmp(&distance(b.pos(), target)));
        let mut chosen = None;
        let mut too_deep = false;
        for u in sparrows {
            let depth = penetration_depth(u.pos(), target, |p| self.sweeps_dangerous(p));
            if depth <= range {
                chosen = Some((u, false, range, depth));
                break;
            }
            too_deep = true;
        }
        if chosen.is_none() {
            // Even without a ready Sparrow, assess its reach from home before
            // requisitioning a Swift. Do not substitute Swifts for cheap dives.
            too_deep |= penetration_depth(self.graph.get_ally_center(), target, |p| self.sweeps_dangerous(p)) > range;
            if too_deep {
                self.sweeps.recon.swift_wanted_until = Some(self.frame + STALE);
                let mut swifts: Vec<_> = self
                    .misc_recon_swifts()
                    .into_iter()
                    .filter(|&u| {
                        self.sweeps_available(u, purpose) && u.rules_param_float("specialReloadRemaining", 0.0) <= 0.0
                    })
                    .collect();
                swifts.sort_by(|a, b| distance(a.pos(), target).total_cmp(&distance(b.pos(), target)));
                for u in swifts {
                    let Some(def) = u.def() else { continue };
                    let depth = penetration_depth(u.pos(), target, |p| self.sweeps_dangerous(p));
                    let burst = burst_range(def);
                    let seconds =
                        def.custom_param("boost_duration").and_then(|v| v.parse::<f32>().ok()).unwrap_or(30.0) / 30.0;
                    // Swift has no fuse: it can continue at cruise after boosting.
                    // Bound the mission so it still arrives before a nuke hold expires.
                    if distance(u.pos(), target) <= continuing_range(burst, seconds, def.speed(), 40.0) {
                        chosen = Some((u, true, burst, depth));
                        break;
                    }
                }
            }
        }
        let Some((unit, swift, burst_range, depth)) = chosen else { return };
        let attempt = Attempt { target, purpose, started: self.frame };
        self.sweeps.recon.attempts.push(attempt);
        self.sweeps.recon.active.insert(unit.id(), Dive { unit, attempt, swift, boosted: false, burst_range });
        self.retreat_remove_unit(unit);
        kgb_move_to(unit, target, 0, self.frame);
        self.log(&format!(
            "sweeps: {:?} dive by {} {}, depth {:.0}, burst reach {:.0}, target ({:.0}, {:.0})",
            purpose,
            if swift { "Swift" } else { "Sparrow" },
            unit.id(),
            depth,
            burst_range,
            target.x,
            target.z
        ));
    }

    /// A bounded hold, only for an actual reconnaissance attempt. An attempt or
    /// timeout never changes anti-nuke beliefs; normal enemy LOS does that.
    pub(crate) fn sweeps_verify_nuke(&mut self, target: Vec3) -> bool {
        if self.dev_off("scouts") || self.frame - self.sweeps_area_seen(target) < FRESH {
            return false;
        }
        self.sweeps_request(target, Purpose::Nuke);
        nuke_wait(&self.sweeps.recon.attempts, target, self.sweeps_area_seen(target), self.frame)
    }

    pub(crate) fn sweeps_recon_update(&mut self) {
        self.sweeps.recon.attempts.retain(|a| self.frame - a.started < STALE);
        self.sweeps
            .detonating
            .retain(|&id| Unit::from_id(self.cb, id).is_some_and(|u| u.health() > 0.0 && u.team() == self.team_id));
        let dives: Vec<_> = self.sweeps.recon.active.values().copied().collect();
        for d in dives {
            let unit = d.unit;
            let id = unit.id();
            if unit.health() <= 0.0 || unit.team() != self.team_id {
                self.sweeps_forget(id);
                continue;
            }
            if springai::reserved::is_reserved(self.cb, id) {
                self.sweeps_forget(id);
                continue;
            }
            if self.sweeps.detonating.contains(&id) {
                continue;
            }
            if self.dev_off("scouts")
                || self.frame - d.attempt.started >= TIMEOUT
                || self.sweeps_area_seen(d.attempt.target) >= d.attempt.started
            {
                self.sweeps.recon.active.remove(&id);
                kgb_move_to(unit, self.graph.get_ally_center(), 0, self.frame);
                self.retreat_add_coward(unit);
                continue;
            }
            if !d.boosted {
                let from = unit.pos();
                let length = distance(from, d.attempt.target);
                let ahead = if length > 0.0 {
                    let t = (STEP * 2.0 / length).min(1.0);
                    Vec3::xz(from.x + (d.attempt.target.x - from.x) * t, from.z + (d.attempt.target.z - from.z) * t)
                } else {
                    from
                };
                if (length <= d.burst_range || self.sweeps_dangerous(ahead))
                    && unit
                        .def()
                        .is_some_and(|def| burst_aligned(from, d.attempt.target, unit.vel() * 30.0, def.speed()))
                {
                    let boosted = if d.swift {
                        burst_order(unit, d.attempt.target, self.frame)
                    } else {
                        self.sweeps_start_burst(unit, d.attempt.target)
                    };
                    self.sweeps.recon.active.get_mut(&id).unwrap().boosted = boosted;
                }
            }
        }
        if self.dev_off("scouts") {
            return;
        }
        // Steering needs half-second updates; choosing new strategic jobs does not.
        if (self.frame + self.offset + 7) % 60 != 0 {
            return;
        }
        if let Some(target) = self.nukes_prelaunch_target() {
            self.sweeps_verify_nuke(target);
        }
        // A precise stale fix warrants a dive even when no missile is ready.
        for target in self.nukes_recon_fixes() {
            if self.frame - self.scouts_last_seen(target) >= STALE
                && retry_allowed(&self.sweeps.recon.attempts, target, Purpose::Anti, self.frame)
            {
                self.sweeps_request(target, Purpose::Anti);
                break;
            }
        }
        let budget = dive_budget(self.war.front.ratchet, self.eco.effective_income);
        let used = self.sweeps.recon.attempts.iter().filter(|a| a.purpose == Purpose::Economy).count();
        if used >= budget {
            return;
        }
        let base = self.graph.get_enemy_base().unwrap_or_else(|| self.graph.get_enemy_center());
        if base == NULLPOS {
            return;
        }
        let mut targets: Vec<(Vec3, f32)> = vec![(base, 4.0)];
        targets.extend(
            self.graph.metal_spots.iter().filter(|m| distance(m.position, base) < 2000.0).map(|m| (m.position, 2.0)),
        );
        targets.extend(
            self.war
                .targets
                .values()
                .filter(|e| {
                    e.identified
                        && !e.is_dead
                        && e.is_static
                        && !e.is_porc
                        && !e.is_aa
                        && !e.is_flex_aa
                        && (e.is_mex || e.value >= 500.0)
                })
                .map(|e| (e.position, 1.0 + e.value / 1000.0)),
        );
        // Ignore the front and reclaimed land: seeing an exposed mex there
        // says nothing about whether deep strategic scouting has succeeded.
        let ours = self.graph.get_ally_center();
        targets.retain(|&(p, _)| !self.graph.is_ally_territory(p) && distance(p, base) < distance(p, ours) * 0.65);
        // A successful strategic scout suppresses the fallback for five minutes.
        if !stale_economy(self.frame, targets.iter().map(|&(p, _)| self.scouts_last_seen(p))) {
            return;
        }
        targets.sort_by(|a, b| {
            let score = |&(p, value): &(Vec3, f32)| value / (1.0 + distance(p, base) / 2000.0);
            score(b).total_cmp(&score(a))
        });
        let mut considered = Vec::new();
        for (target, _) in targets {
            if self.graph.is_ally_territory(target) {
                continue;
            }
            if considered.iter().any(|&p| distance(p, target) < SPREAD) {
                continue;
            }
            considered.push(target);
            self.sweeps_request(target, Purpose::Economy);
            if self.sweeps.recon.attempts.iter().filter(|a| a.purpose == Purpose::Economy).count() >= budget
                || considered.len() >= 8
            {
                break;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dense_front_and_income_control_dive_budget() {
        assert_eq!(dive_budget(2000.0, 500.0), 0);
        assert_eq!(dive_budget(2000.1, 30.0), 1);
        assert_eq!(dive_budget(2500.0, 100.0), 2);
        assert_eq!(dive_budget(2500.0, 200.0), 3);
        assert_eq!(dive_budget(2500.0, 10000.0), 3);
    }

    #[test]
    fn nuke_readiness_requires_stock_or_a_real_almost_finished_missile() {
        assert!(!recon_ready(0, 1, 0.899));
        assert!(recon_ready(0, 1, 0.9));
        assert!(!recon_ready(0, 0, 1.0));
        assert!(recon_ready(1, 0, 0.0));
    }

    #[test]
    fn acceleration_and_fuse_bound_sparrow_penetration() {
        let range = 0.8 * burst_distance(210.0, 5.0, 1080.0, 3.0);
        assert!(range > 2000.0 && range < 2500.0);
        assert!(range < 210.0 * 5.0 * 3.0);
        assert_eq!(burst_distance(210.0, 5.0, 0.0, 3.0), 630.0);
        let target = Vec3::xz(6000.0, 0.0);
        let shallow = penetration_depth(Vec3::ZERO, target, |p| p.x >= 4500.0);
        let deep = penetration_depth(Vec3::ZERO, target, |p| p.x >= 2000.0);
        assert!(shallow < range);
        assert!(deep > range);
        let swift_burst = 0.8 * burst_distance(390.0, 5.0, 2250.0, 1.0);
        assert!(swift_burst < deep);
        assert!(continuing_range(swift_burst, 1.0, 390.0, 11.0) > deep);
        assert_eq!(continuing_range(swift_burst, 1.0, 390.0, 1.0), swift_burst);
        assert_eq!(penetration_depth(Vec3::ZERO, target, |_| false), 0.0);
    }

    #[test]
    fn failed_dive_has_cooldown_but_distant_rich_targets_can_run_together() {
        let target = Vec3::xz(5000.0, 5000.0);
        let attempts = [Attempt { target, started: 0, purpose: Purpose::Economy }];
        assert!(!retry_allowed(&attempts, target, Purpose::Economy, STALE - 1));
        assert!(retry_allowed(&attempts, target, Purpose::Economy, STALE));
        assert!(retry_allowed(&attempts, Vec3::xz(6500.0, 5000.0), Purpose::Economy, 1));
        // A failed routine dive must not suppress the higher-priority check.
        assert!(retry_allowed(&attempts, target, Purpose::Nuke, RETRY_NUKE - 1));
        let attempts = [Attempt { target, started: 0, purpose: Purpose::Nuke }];
        assert!(!retry_allowed(&attempts, target, Purpose::Nuke, RETRY_NUKE - 1));
        assert!(retry_allowed(&attempts, target, Purpose::Nuke, RETRY_NUKE));
    }

    #[test]
    fn any_successful_deep_scout_delays_economy_dives_for_five_minutes() {
        assert!(!stale_economy(STALE - 1, [0, -STALE].into_iter()));
        assert!(stale_economy(STALE, [0, -STALE].into_iter()));
        assert!(!stale_economy(STALE * 2, [0, STALE + 1].into_iter()));
        assert!(!stale_economy(STALE, std::iter::empty()));
    }

    #[test]
    fn nuke_waits_for_observation_not_dispatch_and_cannot_wait_forever() {
        let target = Vec3::xz(5000.0, 5000.0);
        let attempts = [Attempt { target, started: 10000, purpose: Purpose::Nuke }];
        assert!(nuke_wait(&attempts, target, 0, 10001));
        // Seeing the area after departure releases the hold.
        assert!(!nuke_wait(&attempts, target, 10001, 10002));
        // A dead or stalled scout cannot indefinitely hold the missile.
        assert!(!nuke_wait(&attempts, target, 0, 10000 + NUKE_WAIT));
        // No scout, or a different target, does not incur a phantom hold.
        assert!(!nuke_wait(&[], target, 0, 10001));
        assert!(!nuke_wait(&attempts, Vec3::xz(7000.0, 5000.0), 0, 10001));
    }

    #[test]
    fn a_short_fuse_must_not_start_while_turning_or_taking_off() {
        let target = Vec3::xz(3000.0, 0.0);
        assert!(burst_aligned(Vec3::ZERO, target, Vec3::xz(210.0, 0.0), 210.0));
        assert!(!burst_aligned(Vec3::ZERO, target, Vec3::xz(-210.0, 0.0), 210.0));
        assert!(!burst_aligned(Vec3::ZERO, target, Vec3::xz(0.0, 210.0), 210.0));
        assert!(!burst_aligned(Vec3::ZERO, target, Vec3::xz(30.0, 0.0), 210.0));
    }
}
