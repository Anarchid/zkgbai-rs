//! Periodic legacy-style flock launch, sharing the normal sortie controller.
use super::*;

pub(super) const PERIOD: i32 = 30 * 30;
pub(super) const MAX_PURSUIT: f32 = 600.0;
const MIN_FLOCK: usize = 8;
const MAX_DEPTH: f32 = 1800.0;
const TARGET_SPREAD: f32 = 1800.0;

type Package = Vec<(Plane, usize)>;

fn bomber(kind: Plane) -> bool {
    matches!(kind, Plane::Raven | Plane::Phoenix | Plane::Likho | Plane::Odin)
}

fn flock_ready(ready: usize, recovering: usize) -> bool {
    ready >= MIN_FLOCK && recovering == 0
}

/// A cheap fallback is not permission for a cross-map incursion. Sample every
/// segment, including the terminal leg; long friendly flights are still fine.
pub(super) fn route_allowed(from: Vec3, route: &[Vec3], friendly: impl Fn(Vec3) -> bool) -> bool {
    let Some(&target) = route.last() else { return false };
    let mut previous = from;
    let mut depth = 0.0;
    let mut length = 0.0;
    for &next in route {
        let leg = distance(previous, next);
        let steps = (leg / 64.0).ceil().max(1.0) as usize;
        for step in 1..=steps {
            let p = previous + (next - previous) * (step as f32 / steps as f32);
            depth = if friendly(p) { 0.0 } else { depth + leg / steps as f32 };
            if depth > MAX_DEPTH {
                return false;
            }
        }
        length += leg;
        previous = next;
    }
    length.is_finite() && length <= distance(from, target) * 1.5 + 384.0
}

fn preference(job: &Mission, package: &[(Plane, usize)]) -> f32 {
    let favorite = match job.kind {
        MissionKind::Group | MissionKind::Wind | MissionKind::Interdict => Plane::Phoenix,
        MissionKind::Thermite => Plane::Odin,
        _ => Plane::Raven,
    };
    if package.iter().all(|(kind, _)| *kind == favorite || *kind == Plane::Sparrow)
        || (job.kind == MissionKind::Group && package.iter().any(|(kind, _)| *kind == Plane::Likho))
    {
        0.0
    } else {
        400.0
    }
}

pub(super) fn choices(plan: &Plan, have: &BTreeMap<Plane, usize>, raven_damage: f32) -> Vec<Package> {
    let mut choices = plan.packages.clone();
    // Precision bombers can help with statics instead of waiting for a primary
    // job of their own. Keep the same AA gates and reliable kill margin.
    if matches!(
        plan.job.kind,
        MissionKind::Heavy | MissionKind::Opportunistic | MissionKind::Static | MissionKind::Thermite
    ) {
        let mut precision = plan.job.clone();
        precision.kind = MissionKind::Heavy;
        choices.extend(packages(&precision, plan.risk, raven_damage));
        // Combine partial Raven/Likho wings when neither alone can kill it.
        if plan.risk.aa <= 0.15 && !plan.risk.intercepted {
            let required = f64::from(plan.job.hp) * 1.2;
            for likhos in 1..=have.get(&Plane::Likho).copied().unwrap_or(0).min(MAX_WING) {
                let remaining = required - likhos as f64 * 2000.0;
                if remaining <= 0.0 {
                    break;
                }
                let ravens = (remaining / f64::from(raven_damage.max(1.0))).ceil() as usize + 1;
                if ravens <= have.get(&Plane::Raven).copied().unwrap_or(0) && likhos + ravens <= MAX_WING {
                    choices.push(vec![(Plane::Likho, likhos), (Plane::Raven, ravens)]);
                }
            }
        }
    }
    choices.retain(|p| {
        p.iter().all(|(k, n)| (bomber(*k) || *k == Plane::Sparrow) && *n <= have.get(k).copied().unwrap_or(0))
    });
    choices.sort();
    choices.dedup();
    choices
}

/// Pick across the entire fleet, retaining legacy distance/AA/value ranking
/// rather than mission tier. Reserve victims as well as aircraft, so overlapping
/// group and single-unit plans cannot spend two salvos on the same target.
fn allocate(plans: &[Plan], have: &BTreeMap<Plane, usize>, available: &[Vec<Package>]) -> Vec<(usize, Package)> {
    let mut have = have.clone();
    let mut covered = BTreeSet::new();
    let mut anchor = None;
    let mut assigned = Vec::new();
    loop {
        let mut options = Vec::new();
        for (i, plan) in plans.iter().enumerate() {
            if plan.job.targets.iter().any(|id| covered.contains(id))
                || anchor.is_some_and(|p| distance(p, plan.job.pos) > TARGET_SPREAD)
            {
                continue;
            }
            for package in &available[i] {
                if package.iter().any(|(k, n)| *n > have.get(k).copied().unwrap_or(0)) {
                    continue;
                }
                let threat = 1.0 + plan.risk.aa;
                let score =
                    plan.length * threat - plan.job.damage_value(package) / threat + preference(&plan.job, package);
                options.push((score, package.iter().map(|(_, n)| n).sum::<usize>(), i, package));
            }
        }
        options.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)).then(a.2.cmp(&b.2)).then(a.3.cmp(b.3)));
        let Some((_, _, i, package)) = options.first() else { break };
        for (k, n) in *package {
            *have.get_mut(k).unwrap() -= n;
        }
        let job = &plans[*i].job;
        anchor.get_or_insert(job.pos);
        covered.extend(job.targets.iter().copied());
        assigned.push((*i, (*package).clone()));
    }
    assigned
}

impl Ai {
    pub(super) fn airstrike_wave(&mut self, frame: i32, home: Vec3) {
        if self.dev_off("airstrikes") || frame - self.bomber_handler.last_wave_check < PERIOD {
            return;
        }
        self.bomber_handler.last_wave_check = frame;
        self.bomber_handler.wave_checks += 1;
        let busy: BTreeSet<_> = self.bomber_handler.sorties.iter().flat_map(|s| s.members.iter().copied()).collect();
        let covered: BTreeSet<_> =
            self.bomber_handler.sorties.iter().flat_map(|s| s.job.targets.iter().copied()).collect();
        let mut ready = BTreeMap::<Plane, Vec<i32>>::new();
        let mut recovering = 0;
        for (&id, a) in &self.bomber_handler.units {
            if busy.contains(&id)
                || springai::reserved::is_reserved(self.cb, id)
                || (!bomber(a.kind) && a.kind != Plane::Sparrow)
            {
                continue;
            }
            let healthy = a.unit.health() >= a.unit.max_health() * 0.95;
            let armed = a.unit.rules_param_float("noammo", 0.0) == 0.0;
            if bomber(a.kind) && (!healthy || !armed) {
                recovering += 1;
            }
            let away = distance(a.unit.pos(), home);
            if healthy && armed && away < 1200.0 && (away < 450.0 || self.graph.is_ally_territory(a.unit.pos())) {
                ready.entry(a.kind).or_default().push(id);
            }
        }
        let count = ready.iter().filter(|(k, _)| bomber(**k)).map(|(_, ids)| ids.len()).sum();
        if !flock_ready(count, recovering) {
            return;
        }
        let have: BTreeMap<_, _> = ready.iter().map(|(&k, ids)| (k, ids.len())).collect();
        let plans: Vec<_> = self
            .bomber_handler
            .plans
            .iter()
            .filter(|p| {
                !matches!(
                    p.job.kind,
                    MissionKind::Intercept | MissionKind::Shield | MissionKind::Assassinate | MissionKind::Speculative
                ) && !p.job.targets.iter().any(|id| covered.contains(id))
                    && !self.bomber_handler.cooldown.contains_key(&p.job.target)
                    && route_allowed(home, &p.route, |pos| self.graph.is_ally_territory(pos))
            })
            .cloned()
            .collect();
        let available: Vec<_> = plans
            .iter()
            .map(|p| {
                let mut options = choices(p, &have, self.bomber_handler.raven_damage.unwrap_or(800.0));
                let friendlies = self
                    .cb
                    .friendly_units_in(p.job.pos, 220.0)
                    .iter()
                    .any(|u| u.def().is_some_and(|d| !d.is_able_to_fly()));
                options.retain(|package| {
                    package.iter().all(|(kind, _)| {
                        !(friendlies && matches!(kind, Plane::Phoenix | Plane::Likho))
                            && ready[kind].iter().all(|id| {
                                let u = self.bomber_handler.units[id].unit;
                                u.def().is_some_and(|d| kind.survives(p.risk, u.health(), d.speed()))
                            })
                    })
                });
                options
            })
            .collect();
        let assignments = allocate(&plans, &have, &available);
        let assigned_bombers: usize =
            assignments.iter().flat_map(|(_, p)| p).filter(|(kind, _)| bomber(*kind)).map(|(_, n)| n).sum();
        if assigned_bombers < MIN_FLOCK {
            return;
        }
        let mut sent = 0;
        for (i, package) in assignments {
            let members: Vec<_> = package.iter().flat_map(|(k, n)| ready[k].iter().take(*n).copied()).collect();
            if self.airstrike_dispatch(
                &plans[i],
                &members,
                &package,
                frame,
                Some(WaveFlight { id: frame, target: plans[i].job.pos }),
            ) {
                for (k, n) in package {
                    ready.get_mut(&k).unwrap().drain(..n);
                }
                sent += members.len();
            }
        }
        if sent > 0 {
            self.bomber_handler.waves_launched += 1;
            self.bomber_handler.last_wave_launch = frame;
            self.log(&format!("airstrikes: combined wave={frame} aircraft={sent} ready={count}"));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan(kind: MissionKind, id: i32, hp: f32, value: f32) -> Plan {
        let job = Mission {
            kind,
            inference: None,
            group: (kind == MissionKind::Group).then(|| {
                let mut c = super::super::super::tests::enemy(id);
                c.pos = Vec3::xz(1000.0, id as f32 * 100.0);
                c.hp = hp;
                c.value = value;
                GroupStrike::new(&[&c])
            }),
            target: id,
            targets: vec![id],
            pos: Vec3::xz(1000.0, id as f32 * 100.0),
            hp,
            value,
            seen: 0,
            deep: false,
        };
        let risk = Risk { aa: 0.0, exposure: 0.0, intercepted: false, burst: 0.0 };
        Plan {
            packages: packages(&job, risk, 800.0),
            route: vec![job.pos],
            return_route: vec![Vec3::ZERO],
            job,
            risk,
            length: 1100.0,
        }
    }

    #[test]
    fn legacy_gate_waits_for_eight_and_recovery() {
        assert!(!flock_ready(7, 0));
        assert!(flock_ready(8, 0));
        assert!(!flock_ready(20, 1));
        assert!(!bomber(Plane::Sparrow));
        assert!(!bomber(Plane::Raptor));
        assert!(!bomber(Plane::Magpie));
    }

    #[test]
    fn whole_fleet_matches_roles_without_duplicate_victims() {
        let plans = vec![
            plan(MissionKind::Heavy, 1, 1000.0, 1100.0),
            plan(MissionKind::Group, 2, 1500.0, 1000.0),
            plan(MissionKind::Heavy, 3, 3000.0, 1600.0),
            plan(MissionKind::Thermite, 4, 14000.0, 5000.0),
            plan(MissionKind::Static, 5, 400.0, 300.0),
            plan(MissionKind::Static, 4, 14000.0, 5000.0),
        ];
        let have = BTreeMap::from([(Plane::Raven, 5), (Plane::Phoenix, 1), (Plane::Likho, 2), (Plane::Odin, 1)]);
        let available: Vec<_> = plans.iter().map(|p| choices(p, &have, 800.0)).collect();
        let assigned = allocate(&plans, &have, &available);
        assert_eq!(assigned.iter().flat_map(|(_, p)| p).map(|(_, n)| n).sum::<usize>(), 9);
        assert_eq!(assigned.len(), 5);
        for (i, kind) in
            [(0, Plane::Raven), (1, Plane::Phoenix), (2, Plane::Likho), (3, Plane::Odin), (4, Plane::Raven)]
        {
            assert!(assigned.iter().any(|(index, p)| *index == i && p.iter().any(|(k, _)| *k == kind)));
        }
    }

    #[test]
    fn partial_precision_wings_combine_but_do_not_ignore_aa() {
        let mut p = plan(MissionKind::Heavy, 1, 5000.0, 3000.0);
        let have = BTreeMap::from([(Plane::Raven, 4), (Plane::Likho, 2)]);
        assert_eq!(choices(&p, &have, 800.0), vec![vec![(Plane::Likho, 2), (Plane::Raven, 4)]]);
        p.risk.aa = 0.3;
        p.packages = packages(&p.job, p.risk, 800.0);
        assert!(choices(&p, &have, 800.0).is_empty());
    }

    #[test]
    fn near_value_beats_low_value_and_far_pursuit() {
        let cheap = plan(MissionKind::Static, 1, 400.0, 100.0);
        let valuable = plan(MissionKind::Heavy, 2, 1000.0, 1100.0);
        let mut distant = valuable.clone();
        distant.job.target = 3;
        distant.job.targets = vec![3];
        distant.job.pos = Vec3::xz(8000.0, 0.0);
        distant.length = 8000.0;
        assert!(valuable.score() > distant.score());
        let have = BTreeMap::from([(Plane::Raven, 3)]);
        let plans = vec![cheap, distant, valuable];
        let available: Vec<_> = plans.iter().map(|p| choices(p, &have, 800.0)).collect();
        assert_eq!(allocate(&plans, &have, &available)[0].0, 2);
        assert!(route_allowed(Vec3::ZERO, &[Vec3::xz(1500.0, 0.0)], |_| false));
        assert!(!route_allowed(Vec3::ZERO, &[Vec3::xz(2500.0, 0.0)], |_| false));
        assert!(route_allowed(Vec3::ZERO, &[Vec3::xz(8000.0, 0.0)], |_| true));
        assert!(!route_allowed(Vec3::ZERO, &[Vec3::xz(0.0, 5000.0), Vec3::xz(1000.0, 0.0)], |_| true));
    }
}
