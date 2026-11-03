//! Pool the loaded bombers of separate sorties onto one target none of them
//! can take alone. Lone Ravens sent at mexes fly past a Lance: one Raven bomb
//! (800) cannot kill its 1000 HP, two can. A sortie retargets only its own
//! bombers (see `retarget`), so this pass looks across sorties: the nearest
//! free ones whose bombers together fit the target give them up to one new
//! sortie when the pooled drop is worth clearly more than the drops they give
//! up. Like a retarget it happens once: pooled sorties do not switch again.
use super::*;

/// Every pooled bomber must be this close to the target.
const REACH: f32 = 800.0;
/// The pooled drop must be worth this much more than the drops given up.
const GAIN: f32 = 1.25;

/// The best pooling found: its gain, plan, donor sorties and bombers.
type Pooled = (f32, Plan, Vec<usize>, BTreeMap<Plane, usize>);

/// A sortie free to give up its target: its loaded bombers, the value of the
/// drop it is flying, and how far its farthest bomber is from the candidate.
#[derive(Clone)]
pub(super) struct Donor {
    pub index: usize,
    pub planes: BTreeMap<Plane, usize>,
    pub value: f32,
    pub reach: f32,
}

/// The fewest nearest donors (at least two) whose bombers together fit `job`,
/// and their pooled bombers, when that pays.
pub(super) fn pick(
    job: &Mission,
    risk: Risk,
    raven_damage: f32,
    donors: &[Donor],
) -> Option<(Vec<usize>, BTreeMap<Plane, usize>)> {
    let mut near: Vec<&Donor> = donors.iter().filter(|d| d.reach <= REACH).collect();
    near.sort_by(|a, b| a.reach.total_cmp(&b.reach).then_with(|| a.index.cmp(&b.index)));
    let choices = packages(job, risk, raven_damage);
    let mut have = BTreeMap::new();
    let mut used = Vec::new();
    let mut given_up = 0.0;
    for d in near {
        for (&k, &n) in &d.planes {
            *have.entry(k).or_default() += n;
        }
        used.push(d.index);
        given_up += d.value;
        if used.len() < 2 {
            continue;
        }
        let fits = choices.iter().any(|p| retarget::fits(p, &have))
            || retarget::precision_fits(job, risk, &have, raven_damage);
        if fits {
            let pooled: Vec<_> = have.iter().map(|(&k, &n)| (k, n)).collect();
            return (job.damage_value(&pooled) >= given_up * GAIN).then_some((used, have));
        }
    }
    None
}

impl Ai {
    /// One pooling per update (see the module note). Returns whether it
    /// pooled.
    pub(super) fn airstrike_pool(&mut self, home: Vec3, air: &AirField, interceptors: &[(Vec3, f32, f32)]) -> bool {
        if self.dev_off("airpool") {
            return false;
        }
        let frame = self.frame;
        // Free sorties: not switched yet, not in a wave, of strike kinds,
        // every member a loaded, healthy Raven or Likho.
        let mut donors = Vec::new();
        let mut members = BTreeMap::new();
        for (i, s) in self.bomber_handler.sorties.iter().enumerate() {
            if s.retargeted
                || s.wave.is_some()
                || frame - s.launched > SORTIE_TIMEOUT
                || !matches!(
                    s.job.kind,
                    MissionKind::Heavy
                        | MissionKind::Opportunistic
                        | MissionKind::Assassinate
                        | MissionKind::Static
                        | MissionKind::Group
                )
            {
                continue;
            }
            let loaded: Option<Vec<_>> = s
                .members
                .iter()
                .map(|id| {
                    self.bomber_handler.units.get(id).filter(|a| {
                        matches!(a.kind, Plane::Raven | Plane::Likho)
                            && a.unit.health() >= a.unit.max_health() * 0.5
                            && a.unit.rules_param_float("noammo", 0.0) == 0.0
                            && !springai::reserved::is_reserved(self.cb, a.unit.id())
                    })
                })
                .collect();
            let Some(loaded) = loaded.filter(|l| !l.is_empty()) else { continue };
            let mut planes = BTreeMap::new();
            for a in &loaded {
                *planes.entry(a.kind).or_default() += 1;
            }
            let package: Vec<_> = planes.iter().map(|(&k, &n)| (k, n)).collect();
            members.insert(i, loaded.iter().map(|a| (a.unit.id(), a.unit.pos())).collect::<Vec<_>>());
            donors.push(Donor { index: i, planes, value: s.job.damage_value(&package), reach: 0.0 });
        }
        if donors.len() < 2 {
            return false;
        }
        let donor_ids: BTreeSet<_> = donors.iter().map(|d| d.index).collect();
        let occupied: BTreeSet<_> = (self.bomber_handler.sorties.iter().enumerate())
            .filter(|(i, _)| !donor_ids.contains(i))
            .flat_map(|(_, s)| s.job.targets.iter().copied())
            .collect();
        let contacts = self.airstrike_contacts();
        let arms = self.airstrike_arms();
        let raven_damage = self.bomber_handler.raven_damage.unwrap_or(800.0);
        let mut best: Option<Pooled> = None;
        let mut fields = BTreeMap::new();
        for mut job in retarget::opportunities(&contacts, home, frame) {
            if !matches!(
                job.kind,
                MissionKind::Heavy
                    | MissionKind::Opportunistic
                    | MissionKind::Assassinate
                    | MissionKind::Static
                    | MissionKind::Group
            ) || self.airstrike_kind_off(job.kind)
                || job.targets.iter().any(|id| occupied.contains(id))
                || !self
                    .war
                    .targets
                    .get(&job.target)
                    .is_some_and(|e| !e.is_dead && e.identified && (e.visible || e.is_radar_visible || e.is_static))
            {
                continue;
            }
            job.aim_group(arms);
            let near: Vec<Donor> = donors
                .iter()
                .map(|d| Donor {
                    reach: members[&d.index].iter().map(|(_, p)| distance(*p, job.pos)).fold(0.0, f32::max),
                    ..d.clone()
                })
                .filter(|d| d.reach <= REACH)
                .collect();
            if near.len() < 2 {
                continue;
            }
            // Only then route (a full path field) from the nearest bomber,
            // one field per lead: every member must survive the route.
            let clear = Risk { aa: 0.0, exposure: 0.0, intercepted: false, burst: 0.0 };
            if pick(&job, clear, raven_damage, &near).is_none() {
                continue;
            }
            let lead_index = near.iter().min_by(|a, b| a.reach.total_cmp(&b.reach)).map(|d| d.index).unwrap();
            let lead = members[&lead_index][0].1;
            let field = fields.entry(lead_index).or_insert_with(|| self.airstrike_field(lead, interceptors));
            let route = field.route(job.pos);
            let length = field.length(job.pos).unwrap_or(f32::INFINITY);
            if route.is_empty() || !length.is_finite() {
                continue;
            }
            let exit = return_route(home, &air.route(job.pos));
            let risk = round_trip(
                self.airstrike_path_risk(lead, &route, interceptors),
                self.airstrike_path_risk(job.pos, &exit, interceptors),
            );
            let Some((used, have)) = pick(&job, risk, raven_damage, &near) else { continue };
            let safe = used.iter().flat_map(|i| members[i].iter()).all(|(id, _)| {
                let a = &self.bomber_handler.units[id];
                a.unit
                    .def()
                    .is_some_and(|d| job.flight_safe(a.kind, risk, a.unit.health(), a.unit.max_health(), d.speed()))
            });
            let pooled: Vec<_> = have.iter().map(|(&k, &n)| (k, n)).collect();
            if risk.intercepted || !safe || !self.airstrike_spares_friends(&job, &pooled, &route) {
                continue;
            }
            let given_up: f32 = near.iter().filter(|d| used.contains(&d.index)).map(|d| d.value).sum();
            let gain = job.damage_value(&pooled) - given_up;
            if best.as_ref().is_none_or(|(g, ..)| gain > *g) {
                let plan = Plan { packages: vec![pooled], risk, length, route, return_route: exit, job };
                best = Some((gain, plan, used, have));
            }
        }
        let Some((gain, plan, used, have)) = best else { return false };
        let ids: Vec<i32> = used.iter().flat_map(|i| members[i].iter().map(|(id, _)| *id)).collect();
        self.log(&format!(
            "airstrikes: pool {} sorties ({} bombers) -> {:?}:{} value={:.0} gain={gain:.0}",
            used.len(),
            ids.len(),
            plan.job.kind,
            plan.job.target,
            plan.job.damage_value(&have.iter().map(|(&k, &n)| (k, n)).collect::<Vec<_>>()),
        ));
        // The first donor becomes the pooled sortie; the others are dissolved
        // (their bombers are in it, their targets free again).
        let mut used = used;
        used.sort_unstable();
        let keep = used[0];
        for &i in used.iter().skip(1).rev() {
            self.bomber_handler.sorties.remove(i);
        }
        let s = &mut self.bomber_handler.sorties[keep];
        let previous = std::mem::take(&mut s.job.targets);
        s.job = plan.job;
        s.job.targets.extend(previous);
        s.members = ids;
        s.package = have.into_iter().collect();
        s.route = plan.route;
        s.return_route = plan.return_route;
        s.memory = TargetMemory { last_contact: frame, destroyed: false };
        s.searched_since = None;
        s.retargeted = true;
        s.release = release::State::default();
        s.last_order = -1000;
        s.last_route = -1000;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lance() -> Mission {
        Mission { value: 1100.0, ..super::super::super::tests::job(MissionKind::Heavy, 1000.0) }
    }
    fn raven(index: usize, value: f32, reach: f32) -> Donor {
        Donor { index, planes: BTreeMap::from([(Plane::Raven, 1)]), value, reach }
    }
    fn clear() -> Risk {
        Risk { aa: 0.0, exposure: 0.0, intercepted: false, burst: 0.0 }
    }

    #[test]
    fn two_lone_ravens_on_mexes_take_a_lance_together() {
        let donors = [raven(0, 340.0, 300.0), raven(1, 340.0, 500.0), raven(2, 340.0, 700.0)];
        let (used, have) = pick(&lance(), clear(), 800.0, &donors).expect("pooled");
        // The two nearest suffice (1600 of bomb for 1000 HP with the margin).
        assert_eq!(used, vec![0, 1]);
        assert_eq!(have, BTreeMap::from([(Plane::Raven, 2)]));
    }

    #[test]
    fn pooling_needs_a_partner_in_reach_and_a_real_gain() {
        // One Raven alone is a retarget's business, not a pool.
        assert!(pick(&lance(), clear(), 800.0, &[raven(0, 340.0, 300.0)]).is_none());
        // The second is too far.
        assert!(pick(&lance(), clear(), 800.0, &[raven(0, 340.0, 300.0), raven(1, 340.0, 900.0)]).is_none());
        // Two drops worth 1000 together are not given up for 1100.
        assert!(pick(&lance(), clear(), 800.0, &[raven(0, 500.0, 300.0), raven(1, 500.0, 400.0)]).is_none());
    }
}
