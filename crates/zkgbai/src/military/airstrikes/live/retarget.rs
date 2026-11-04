//! One local target change: salvage a failed attack, or take a better drop:
//! an earlier one, or a later one worth the wait (Lances just beyond the mex
//! a lone Raven was sent at).
use super::*;

const VICINITY: f32 = 800.0;
const MAX_LEG: f32 = 1200.0;

// Leave room for the engine's terminal turn/release inside the eight-second
// commitment. Require a real gain rather than reacting to small route jitter.
const MAX_OPPORTUNITY_ETA: f32 = 6.0;
const TIME_GAIN: f32 = 1.0;
const VALUE_GAIN: f32 = 1.1;
/// What a sortie has left after its drop: the flight home and the rearm.
/// A later drop pays when its value per second over the whole remaining
/// sortie beats the current one's by `LATER_RATE_GAIN`.
const CYCLE: f32 = 30.0;
const LATER_RATE_GAIN: f32 = 1.5;

fn better_drop(old_value: f32, new_value: f32, old_eta: f32, new_eta: f32) -> bool {
    old_value.is_finite()
        && new_value.is_finite()
        && new_value > old_value * VALUE_GAIN
        && old_eta.is_finite()
        && new_eta.is_finite()
        && new_eta <= MAX_OPPORTUNITY_ETA
        && (new_eta + TIME_GAIN <= old_eta
            || new_value / (new_eta + CYCLE) >= old_value / (old_eta.max(0.0) + CYCLE) * LATER_RATE_GAIN)
}

fn arrival_time(from: Vec3, route: &[Vec3], velocity: Vec3, speed: f32) -> f32 {
    let Some(&target) = route.last() else { return f32::INFINITY };
    if speed <= 0.0 {
        return f32::INFINITY;
    }
    let mut previous = from;
    let length: f32 = route
        .iter()
        .map(|&p| {
            let leg = distance(previous, p);
            previous = p;
            leg
        })
        .sum();
    // A close contact moving away may take longer to catch than a farther
    // stationary quarry. Routing detours are charged on top of interception.
    intercept_time(from, target, velocity, speed).unwrap_or(f32::INFINITY)
        + (length - distance(from, target)).max(0.0) / speed
}

fn local(anchor: Vec3, from: Vec3, target: Vec3, length: f32) -> bool {
    distance(anchor, target) <= VICINITY
        && distance(from, target) <= VICINITY
        && length.is_finite()
        && length <= MAX_LEG
}

fn fallback_due(hunting: bool, observed: bool, destroyed: bool, gap: i32, range: f32, unsafe_route: bool) -> bool {
    range <= VICINITY
        && (destroyed
            || unsafe_route
            || (!observed
                && (if hunting { range <= 250.0 } else { gap > 30 * 20 || (gap >= 30 * 5 && range <= 400.0) })))
}

fn static_raven_limit(job: &Mission, have: &BTreeMap<Plane, usize>, damage: f32) -> Option<usize> {
    (job.kind == MissionKind::Static && have.len() == 1 && have.contains_key(&Plane::Raven))
        .then(|| job.ravens(damage).min(have[&Plane::Raven]))
}

pub(super) fn fits(package: &[(Plane, usize)], have: &BTreeMap<Plane, usize>) -> bool {
    // Do not give an incompatible payload the new job, or count spent aircraft
    // toward a reliable strike. Mixed Raven/Likho waves retain mixed packages.
    package.iter().all(|(kind, n)| have.get(kind).copied().unwrap_or(0) >= *n)
        && have.keys().all(|kind| package.iter().any(|(k, _)| k == kind))
}

// A leftover Raven does not need another spare Raven to finish a nearby
// target. Keep the damage margin, but use the ammunition already on station.
pub(super) fn precision_fits(job: &Mission, risk: Risk, have: &BTreeMap<Plane, usize>, raven_damage: f32) -> bool {
    !risk.intercepted
        && matches!(
            job.kind,
            MissionKind::Heavy
                | MissionKind::Opportunistic
                | MissionKind::Assassinate
                | MissionKind::Static
                | MissionKind::Thermite
        )
        && !have.is_empty()
        && have.keys().all(|k| {
            matches!(k, Plane::Raven | Plane::Likho)
                && can_hit_submerged(*k, job.kind, job.submerged)
                && !(job.fast && *k == Plane::Raven)
        })
        && (!have.contains_key(&Plane::Raven) || risk.aa <= 0.15)
        && job.value >= LIKHO_MIN_VALUE * have.get(&Plane::Likho).copied().unwrap_or(0) as f32
        && have
            .iter()
            .map(|(k, n)| *n as f64 * if *k == Plane::Raven { f64::from(raven_damage) } else { 2000.0 })
            .sum::<f64>()
            >= f64::from(job.hp) * 1.2
}

pub(super) fn opportunities(contacts: &[Contact], home: Vec3, frame: i32) -> Vec<Mission> {
    let mut jobs = missions(contacts, home, frame);
    let carriers = shields::carriers(contacts);
    for e in contacts.iter().filter(|e| e.known && e.live && !e.air && !e.static_ && frame - e.seen <= FRESH) {
        // A terminal opportunity need not meet the primary 600-metal filter,
        // or belong to an artillery/raider class. Keep it secondary.
        if jobs.iter().any(|j| j.target == e.id && matches!(j.kind, MissionKind::Heavy | MissionKind::Opportunistic)) {
            continue;
        }
        jobs.push(Mission {
            submerged: e.submerged,
            kind: MissionKind::Opportunistic,
            target: e.id,
            pos: e.pos,
            hp: e.hp,
            value: e.value,
            group: None,
            inference: None,
            targets: vec![e.id],
            seen: frame,
            deep: false,
            shields: shields::over(&carriers, &[e.pos]),
            // Any raider is one of these; a Raven's bomb cannot catch it.
            fast: e.speed >= RAVEN_MAX_TARGET_SPEED,
            wearable: false,
        });
    }
    jobs
}

impl Ai {
    pub(super) fn airstrike_retarget(
        &mut self,
        sortie: &mut Sortie,
        home: Vec3,
        air: &AirField,
        interceptors: &[(Vec3, f32, f32)],
        occupied: &BTreeSet<i32>,
    ) -> bool {
        if sortie.retargeted
            || self.dev_off("airstrikes")
            || self.frame - sortie.launched > SORTIE_TIMEOUT
            || matches!(
                sortie.job.kind,
                MissionKind::Intercept | MissionKind::HeavyAir | MissionKind::Shield | MissionKind::Carpet
            )
            || (sortie.job.kind == MissionKind::Speculative && self.dev_off("airspeculate"))
        {
            return false;
        }
        let loaded: Vec<_> = sortie
            .members
            .iter()
            .filter_map(|id| self.bomber_handler.units.get(id))
            .filter(|a| {
                a.kind != Plane::Sparrow
                    && a.unit.health() >= a.unit.max_health() * 0.5
                    && a.unit.rules_param_float("noammo", 0.0) == 0.0
                    && !springai::reserved::is_reserved(self.cb, a.unit.id())
            })
            .collect();
        let Some(first) = loaded.first() else { return false };
        let from = first.unit.pos();
        let old = self.war.targets.get(&sortie.job.target);
        let observed = old.is_some_and(|e| !e.is_dead && (e.visible || e.is_radar_visible || e.is_static));
        let anchor = old.filter(|_| observed).map_or(sortie.job.pos, |e| e.position);
        let range = distance(from, anchor);
        let contacts = self.airstrike_contacts();
        // Most travelling sorties have no local alternative. Avoid another
        // full path field until something known is actually near the aircraft.
        if !contacts.iter().any(|e| e.known && e.live && !e.air && distance(from, e.pos) <= VICINITY) {
            return false;
        }
        let field = self.airstrike_field(from, interceptors);
        let old_route = field.route(anchor);
        let old_exit = return_route(home, &air.route(anchor));
        let old_risk = round_trip(
            self.airstrike_path_risk(from, &old_route, interceptors),
            self.airstrike_path_risk(anchor, &old_exit, interceptors),
        );
        let committed = loaded.iter().any(|a| sortie.release.active(a.unit.id(), self.frame));
        let unsafe_route = !committed
            && (old_risk.intercepted
                || loaded.iter().any(|a| {
                    a.unit.def().is_none_or(|d| {
                        !can_hit_submerged(a.kind, sortie.job.kind, old.is_some_and(|e| e.is_submerged()))
                            || !sortie.job.flight_safe(
                                a.kind,
                                old_risk,
                                a.unit.health(),
                                a.unit.max_health(),
                                d.speed(),
                            )
                    })
                }));
        let fallback = fallback_due(
            sortie.job.kind == MissionKind::Speculative,
            observed,
            sortie.memory.destroyed,
            self.frame - sortie.memory.last_contact,
            range,
            unsafe_route,
        );
        let mut have = BTreeMap::new();
        for a in &loaded {
            *have.entry(a.kind).or_default() += 1;
        }
        let mut actual: Vec<_> = have.iter().map(|(&k, &n)| (k, n)).collect();
        let arms = self.airstrike_arms();
        let mut old_job = sortie.job.clone();
        old_job.pos = anchor;
        if let Some(e) = contacts.iter().find(|e| e.id == old_job.target && e.live) {
            old_job.hp = e.hp;
            old_job.value = e.value;
        }
        if old_job.group.is_some() {
            let victims: Vec<_> = contacts.iter().filter(|e| e.live && old_job.targets.contains(&e.id)).collect();
            let points: Vec<_> = victims.iter().map(|e| e.pos).collect();
            old_job.shields = shields::over(&shields::carriers(&contacts), &points);
            old_job.group = Some(if old_job.kind == MissionKind::Outpost {
                GroupStrike::outpost(&victims, old_job.shields.clone(), home)
            } else {
                GroupStrike::new(&victims, old_job.shields.clone())
            });
            old_job.aim_group(arms);
        }
        let old_value = old_job.damage_value(&actual);
        let old_velocity = contacts.iter().find(|e| e.id == sortie.job.target && e.live).map_or(Vec3::ZERO, |e| e.vel);
        let mut best: Option<(f32, Plan)> = None;
        for mut job in opportunities(&contacts, home, self.frame) {
            if matches!(job.kind, MissionKind::Intercept | MissionKind::HeavyAir | MissionKind::Shield)
                || self.airstrike_kind_off(job.kind)
                || job.target == sortie.job.target
                || job.targets.iter().any(|id| occupied.contains(id) && !sortie.job.targets.contains(id))
                || !self
                    .war
                    .targets
                    .get(&job.target)
                    .is_some_and(|e| !e.is_dead && e.identified && (e.visible || e.is_radar_visible))
            {
                continue;
            }
            job.aim_group(arms);
            let length = field.length(job.pos).unwrap_or(f32::INFINITY);
            if !(if fallback {
                local(anchor, from, job.pos, length)
            } else {
                distance(from, job.pos) <= VICINITY && length.is_finite() && length <= MAX_LEG
            }) || loaded.iter().any(|a| distance(a.unit.pos(), job.pos) > VICINITY)
            {
                continue;
            }
            let route = field.route(job.pos);
            if !fallback {
                let value = job.damage_value(&actual);
                let velocity = contacts.iter().find(|e| e.id == job.target).map_or(Vec3::ZERO, |e| e.vel);
                // Every loaded member must get an earlier drop. The leader's
                // shorter route alone must not drag a trailing bomber backwards.
                if loaded.iter().any(|a| {
                    a.unit.def().is_none_or(|d| {
                        !better_drop(
                            old_value,
                            value,
                            arrival_time(a.unit.pos(), &old_route, old_velocity, d.speed()),
                            arrival_time(a.unit.pos(), &route, velocity, d.speed()),
                        )
                    })
                }) {
                    continue;
                }
            }
            if route.is_empty()
                || sortie.wave.is_some_and(|w| {
                    distance(w.target, job.pos) > wave::MAX_PURSUIT
                        || !wave::route_allowed(from, &route, |p| self.graph.is_ally_territory(p))
                })
            {
                continue;
            }
            let exit = return_route(home, &air.route(job.pos));
            let return_risk = self.airstrike_path_risk(job.pos, &exit, interceptors);
            let risk = round_trip(self.airstrike_path_risk(from, &route, interceptors), return_risk);
            if risk.intercepted
                || loaded.iter().any(|a| {
                    let risk = round_trip(self.airstrike_path_risk(a.unit.pos(), &route, interceptors), return_risk);
                    risk.intercepted
                        || a.unit.def().is_none_or(|d| {
                            !job.flight_safe(a.kind, risk, a.unit.health(), a.unit.max_health(), d.speed())
                        })
                })
            {
                continue;
            }
            if !self.airstrike_spares_friends(&job, &actual, &route) {
                continue;
            }
            let choices = packages(&job, risk, self.bomber_handler.raven_damage.unwrap_or(800.0));
            let plan = Plan { job, packages: choices, risk, length, route, return_route: exit };
            if !wave::choices(&plan, &have, self.bomber_handler.raven_damage.unwrap_or(800.0))
                .iter()
                .any(|p| fits(p, &have))
                && !precision_fits(&plan.job, risk, &have, self.bomber_handler.raven_damage.unwrap_or(800.0))
            {
                continue;
            }
            let score = plan.job.damage_value(&actual) / (1.0 + length / 400.0);
            if best.as_ref().is_none_or(|(s, _)| score > *s) {
                best = Some((score, plan));
            }
        }
        let Some((_, plan)) = best else { return false };
        self.log(&format!(
            "airstrikes: local retarget {:?}:{} -> {:?}:{} value={:.0} distance={:.0} reason={}",
            sortie.job.kind,
            sortie.job.target,
            plan.job.kind,
            plan.job.target,
            plan.job.value,
            plan.length,
            if fallback { "fallback" } else { "better_drop" }
        ));
        if let Some(n) = static_raven_limit(&plan.job, &have, self.bomber_handler.raven_damage.unwrap_or(800.0)) {
            // A fallback to a mex must not send the entire old priority wing.
            // Cancel surplus queued attacks before handing them to recovery.
            let surplus: BTreeSet<_> = loaded
                .iter()
                .skip(n)
                .map(|a| {
                    a.unit.stop(0);
                    a.unit.id()
                })
                .collect();
            for id in &surplus {
                let a = self.bomber_handler.units.get_mut(id).unwrap();
                a.returning = true;
                a.last_order = -1000;
            }
            sortie.members.retain(|id| !surplus.contains(id));
            actual = vec![(Plane::Raven, n)];
        }
        let previous = sortie.job.targets.clone();
        sortie.job = plan.job;
        sortie.job.targets.extend(previous);
        sortie.route = plan.route;
        sortie.return_route = plan.return_route;
        sortie.package = actual;
        sortie.memory = TargetMemory { last_contact: self.frame, destroyed: false };
        sortie.searched_since = None;
        sortie.retargeted = true;
        sortie.release = release::State::default();
        sortie.last_order = -1000;
        sortie.last_route = -1000;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn a_loaded_likho_can_take_a_nearer_wounded_merlin_without_waiting_for_failure() {
        let from = Vec3::ZERO;
        let old = super::super::super::tests::job(MissionKind::Group, 2500.0);
        let merlin = Mission { value: 3500.0, ..super::super::super::tests::job(MissionKind::Heavy, 1500.0) };
        let wing = [(Plane::Likho, 1)];
        let clear = Risk { aa: 0.0, exposure: 0.0, intercepted: false, burst: 0.0 };
        assert!(!fallback_due(false, true, false, 0, 1400.0, false));
        assert!(precision_fits(&merlin, clear, &BTreeMap::from(wing), 800.0));
        assert!(better_drop(
            old.damage_value(&wing),
            merlin.damage_value(&wing),
            arrival_time(from, &[Vec3::xz(1400.0, 0.0)], Vec3::ZERO, 270.0),
            arrival_time(from, &[Vec3::xz(430.0, 0.0)], Vec3::ZERO, 270.0)
        ));
        // The opportunity does not waive the payload requirement for a healthy Merlin.
        assert!(!precision_fits(&Mission { hp: 3000.0, ..merlin }, clear, &BTreeMap::from(wing), 800.0));
    }

    #[test]
    fn a_later_drop_must_pay_for_its_wait() {
        // A Lance (1100) a second past the mex (340) a Raven was sent at.
        assert!(better_drop(340.0, 1100.0, 2.0, 3.0));
        // A little more for five more seconds: no.
        assert!(!better_drop(340.0, 400.0, 1.0, 6.0));
        // Still within the terminal window only.
        assert!(!better_drop(340.0, 1100.0, 2.0, 7.0));
    }

    #[test]
    fn an_upgrade_must_pay_more_and_finish_sooner_within_the_terminal_window() {
        assert!(better_drop(600.0, 3500.0, 5.0, 2.0));
        // Later, but six times the value: worth the wait.
        assert!(better_drop(600.0, 3500.0, 2.0, 5.0));
        assert!(!better_drop(600.0, 800.0, 2.0, 5.0));
        assert!(!better_drop(600.0, 650.0, 5.0, 2.0));
        // Barely sooner, but far more: the rate rule takes it.
        assert!(better_drop(600.0, 3500.0, 5.0, 4.5));
        assert!(!better_drop(600.0, 3500.0, 15.0, 7.0));
        assert!(!better_drop(600.0, 3500.0, f32::INFINITY, 2.0));
        assert!(!better_drop(600.0, 3500.0, 5.0, f32::NAN));
    }

    #[test]
    fn a_short_straight_line_does_not_hide_a_detour_or_an_escaping_target() {
        let from = Vec3::ZERO;
        let old = arrival_time(from, &[Vec3::xz(1000.0, 0.0)], Vec3::ZERO, 270.0);
        // A modest upgrade qualifies only by arriving sooner, which neither
        // the detour nor the escaping target does once they are counted.
        let detour = arrival_time(from, &[Vec3::xz(0.0, 600.0), Vec3::xz(400.0, 0.0)], Vec3::ZERO, 270.0);
        assert!(detour > old);
        assert!(!better_drop(600.0, 700.0, old, detour));
        let escaping = arrival_time(from, &[Vec3::xz(400.0, 0.0)], Vec3::xz(200.0, 0.0), 270.0);
        assert!(!better_drop(600.0, 700.0, old, escaping));
        assert!(!arrival_time(from, &[], Vec3::ZERO, 270.0).is_finite());
    }

    #[test]
    fn a_static_fallback_does_not_spend_the_old_priority_wing() {
        let job = super::super::super::tests::job(MissionKind::Static, 560.0);
        assert_eq!(static_raven_limit(&job, &BTreeMap::from([(Plane::Raven, 4)]), 800.0), Some(1));
        assert_eq!(static_raven_limit(&job, &BTreeMap::from([(Plane::Likho, 2)]), 800.0), None);
    }

    #[test]
    fn local_opportunities_include_light_units_but_not_unknown_or_stale_contacts() {
        let mut c = super::super::super::tests::enemy(7);
        c.value = 250.0;
        c.heavy = false;
        c.raider = false;
        let jobs = opportunities(&[c.clone()], Vec3::ZERO, 300);
        assert!(jobs.iter().any(|j| j.target == 7 && j.kind == MissionKind::Opportunistic && !j.kind.primary()));
        c.live = false;
        assert!(opportunities(&[c.clone()], Vec3::ZERO, 300).is_empty());
        c.live = true;
        c.known = false;
        assert!(opportunities(&[c], Vec3::ZERO, 300).is_empty());
    }

    #[test]
    fn a_spare_raven_can_finish_a_local_target_but_not_ignore_aa_or_hp() {
        let job = super::super::super::tests::job(MissionKind::Static, 500.0);
        let have = BTreeMap::from([(Plane::Raven, 1)]);
        let clear = Risk { aa: 0.0, exposure: 0.0, intercepted: false, burst: 0.0 };
        assert!(precision_fits(&job, clear, &have, 800.0));
        assert!(!precision_fits(&Mission { hp: 1200.0, ..job.clone() }, clear, &have, 800.0));
        assert!(!precision_fits(&job, Risk { aa: 0.16, ..clear }, &have, 800.0));
        assert!(!precision_fits(&job, Risk { intercepted: true, ..clear }, &have, 800.0));
    }

    #[test]
    fn fallback_is_local_and_does_not_abandon_a_live_safe_target() {
        assert!(!fallback_due(false, true, false, 900, 200.0, false));
        assert!(!fallback_due(false, false, false, 90, 200.0, false));
        assert!(fallback_due(false, false, false, 150, 200.0, false));
        assert!(fallback_due(true, false, false, 90, 200.0, false));
        assert!(!fallback_due(true, false, false, 90, 700.0, false));
        assert!(fallback_due(false, true, false, 0, 700.0, true));
        assert!(!fallback_due(false, false, true, 900, 1000.0, true));
        assert!(local(Vec3::ZERO, Vec3::xz(100.0, 0.0), Vec3::xz(500.0, 0.0), 600.0));
        assert!(!local(Vec3::ZERO, Vec3::xz(700.0, 0.0), Vec3::xz(1400.0, 0.0), 700.0));
        assert!(!local(Vec3::ZERO, Vec3::ZERO, Vec3::xz(500.0, 0.0), 1500.0));
    }
    #[test]
    fn fallback_requires_a_compatible_loaded_package() {
        assert!(fits(&[(Plane::Likho, 1)], &BTreeMap::from([(Plane::Likho, 1)])));
        assert!(!fits(&[(Plane::Raven, 3)], &BTreeMap::from([(Plane::Raven, 2)])));
        assert!(!fits(&[(Plane::Raven, 3)], &BTreeMap::from([(Plane::Raven, 3), (Plane::Odin, 1)])));
    }
}
