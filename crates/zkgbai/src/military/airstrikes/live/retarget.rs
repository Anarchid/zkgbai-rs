//! One local fallback pass before an armed strike gives up its target area.
use super::*;

const VICINITY: f32 = 800.0;
const MAX_LEG: f32 = 1200.0;

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

fn fits(package: &[(Plane, usize)], have: &BTreeMap<Plane, usize>) -> bool {
    // Do not give an incompatible payload the new job, or count spent aircraft
    // toward a reliable strike. Mixed Raven/Likho waves retain mixed packages.
    package.iter().all(|(kind, n)| have.get(kind).copied().unwrap_or(0) >= *n)
        && have.keys().all(|kind| package.iter().any(|(k, _)| k == kind))
}

// A leftover Raven does not need another spare Raven to finish a nearby
// target. Keep the damage margin, but use the ammunition already on station.
fn precision_fits(job: &Mission, risk: Risk, have: &BTreeMap<Plane, usize>, raven_damage: f32) -> bool {
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
        && have.keys().all(|k| matches!(k, Plane::Raven | Plane::Likho))
        && (!have.contains_key(&Plane::Raven) || risk.aa <= 0.15)
        && have
            .iter()
            .map(|(k, n)| *n as f64 * if *k == Plane::Raven { f64::from(raven_damage) } else { 2000.0 })
            .sum::<f64>()
            >= f64::from(job.hp) * 1.2
}

fn opportunities(contacts: &[Contact], home: Vec3, frame: i32) -> Vec<Mission> {
    let mut jobs = missions(contacts, home, frame);
    for e in contacts.iter().filter(|e| e.known && e.live && !e.air && !e.static_ && frame - e.seen <= FRESH) {
        // A terminal opportunity need not meet the primary 600-metal filter,
        // or belong to an artillery/raider class. Keep it secondary.
        if jobs.iter().any(|j| j.target == e.id && matches!(j.kind, MissionKind::Heavy | MissionKind::Opportunistic)) {
            continue;
        }
        jobs.push(Mission {
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
        });
    }
    jobs
}

impl Ai {
    pub(super) fn airstrike_retarget(
        &self,
        sortie: &mut Sortie,
        home: Vec3,
        air: &AirField,
        interceptors: &[(Vec3, f32, f32)],
        occupied: &BTreeSet<i32>,
    ) -> bool {
        if sortie.retargeted
            || self.dev_off("airstrikes")
            || self.frame - sortie.launched > SORTIE_TIMEOUT
            || matches!(sortie.job.kind, MissionKind::Intercept | MissionKind::Shield)
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
        if range > VICINITY {
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
                        !sortie.job.flight_safe(a.kind, old_risk, a.unit.health(), a.unit.max_health(), d.speed())
                    })
                }));
        if !fallback_due(
            sortie.job.kind == MissionKind::Speculative,
            observed,
            sortie.memory.destroyed,
            self.frame - sortie.memory.last_contact,
            range,
            unsafe_route,
        ) {
            return false;
        }
        let mut have = BTreeMap::new();
        for a in &loaded {
            *have.entry(a.kind).or_default() += 1;
        }
        let actual: Vec<_> = have.iter().map(|(&k, &n)| (k, n)).collect();
        let contacts = self.airstrike_contacts();
        let blast = self.airstrike_blast();
        let mut best: Option<(f32, Plan)> = None;
        for mut job in opportunities(&contacts, home, self.frame) {
            if matches!(job.kind, MissionKind::Intercept | MissionKind::Shield)
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
            job.aim_group(blast);
            let length = field.length(job.pos).unwrap_or(f32::INFINITY);
            if !local(anchor, from, job.pos, length)
                || loaded.iter().any(|a| distance(a.unit.pos(), job.pos) > VICINITY)
            {
                continue;
            }
            let route = field.route(job.pos);
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
            if have.keys().any(|k| matches!(k, Plane::Phoenix | Plane::Likho))
                && self
                    .cb
                    .friendly_units_in(job.pos, 220.0)
                    .iter()
                    .any(|u| u.def().is_some_and(|d| !d.is_able_to_fly()))
            {
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
            "airstrikes: local retarget {:?}:{} -> {:?}:{} value={:.0} distance={:.0}",
            sortie.job.kind, sortie.job.target, plan.job.kind, plan.job.target, plan.job.value, plan.length
        ));
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
