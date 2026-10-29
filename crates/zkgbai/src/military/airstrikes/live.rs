use super::*;
use crate::economy::factory::FacKind;
use crate::kgbutil::AirField;
use crate::military::Bomber;
use crate::util::{OPTION_SHIFT_KEY, distance};
use crate::{Ai, defs::cmd};
use springai::{NO_TIMEOUT, Unit, Vec3};
use std::collections::{BTreeMap, BTreeSet};

const PERIOD: i32 = 30;
const SORTIE_TIMEOUT: i32 = 30 * 150;
const COOLDOWN: i32 = 30 * 15;

struct Aircraft {
    unit: Unit,
    kind: Plane,
    returning: bool,
    last_order: i32,
    escort: Option<i32>,
}
#[derive(Clone)]
struct Plan {
    job: Mission,
    packages: Vec<Vec<(Plane, usize)>>,
    risk: Risk,
    length: f32,
    route: Vec<Vec3>,
}
struct Sortie {
    job: Mission,
    members: Vec<i32>,
    launched: i32,
    last_order: i32,
    last_route: i32,
    route: Vec<Vec3>,
    package: Vec<(Plane, usize)>,
}

#[derive(Default)]
pub struct BomberHandler {
    units: BTreeMap<i32, Aircraft>,
    recent: BTreeMap<(MissionKind, i32), Plan>,
    plans: Vec<Plan>,
    sorties: Vec<Sortie>,
    cooldown: BTreeMap<i32, i32>,
    /// Orders not yet represented by a nanoframe; one per factory.
    orders: BTreeMap<i32, (Plane, i32)>,
    building: BTreeMap<i32, Plane>,
    pub grounded: bool,
    last_report: i32,
    launched: usize,
    raven_damage: Option<f32>,
}
impl BomberHandler {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn get_bomber_size(&self) -> usize {
        self.units.values().filter(|a| !matches!(a.kind, Plane::Sparrow | Plane::Raptor)).count()
    }
}

impl Ai {
    pub(crate) fn airstrike_claims(&self, id: i32) -> bool {
        self.bomber_handler.building.contains_key(&id) || self.bomber_handler.units.contains_key(&id)
    }

    pub fn bomber_add_bomber(&mut self, b: Bomber) {
        let Some(kind) = Plane::of(self.defs.name_of(b.unit)) else { return };
        self.bomber_handler.building.remove(&b.id);
        self.bomber_handler.units.entry(b.id).or_insert(Aircraft {
            unit: b.unit,
            kind,
            returning: true,
            last_order: -1000,
            escort: None,
        });
    }

    pub fn bomber_remove_unit(&mut self, u: Unit) {
        self.bomber_handler.units.remove(&u.id());
        self.bomber_handler.building.remove(&u.id());
        self.bomber_handler.orders.remove(&u.id());
    }

    pub fn bomber_remove_target(&mut self, enemy: i32) {
        self.bomber_handler.recent.retain(|_, p| !p.job.targets.contains(&enemy));
    }

    pub(crate) fn airstrike_created(&mut self, u: Unit, builder: Option<Unit>) {
        if let Some((kind, _)) = builder.and_then(|b| self.bomber_handler.orders.remove(&b.id()))
            && Plane::of(self.defs.name_of(u)) == Some(kind)
        {
            self.bomber_handler.building.insert(u.id(), kind);
        }
    }

    pub(crate) fn airstrike_ordered(&mut self, factory: i32, name: &str) {
        if let Some(p) = Plane::of(name) {
            self.bomber_handler.orders.insert(factory, (p, self.frame));
        }
    }

    /// A deficit for a recent primary mission. Existing, repairing, committed,
    /// unfinished and ordered aircraft all count. Secondary jobs never buy units.
    pub(crate) fn airstrike_production(&self) -> Option<&'static str> {
        let h = &self.bomber_handler;
        if h.grounded || self.dev_off("airstrikes") {
            return None;
        }
        let mut have = BTreeMap::<Plane, usize>::new();
        for a in h.units.values() {
            *have.entry(a.kind).or_default() += 1;
        }
        for &p in h.building.values() {
            *have.entry(p).or_default() += 1;
        }
        for &(p, _) in h.orders.values().filter(|o| self.frame - o.1 < 30 * 30) {
            *have.entry(p).or_default() += 1;
        }
        let mut plans: Vec<_> = h.recent.values().filter(|p| self.frame - p.job.seen <= MEMORY).collect();
        plans.sort_by(|a, b| b.job.score().total_cmp(&a.job.score()));
        for plan in plans {
            if let Some(kind) = purchase(&plan.packages, &have) {
                return Some(kind.name());
            }
        }
        None
    }

    fn airstrike_contacts(&self) -> Vec<Contact> {
        let ground = self.eco_height_map();
        self.war
            .targets
            .values()
            .filter(|e| !e.is_dead && !e.is_nano_spam)
            .map(|e| {
                let live = e.visible || e.is_radar_visible;
                let v = if live { e.unit.vel() * 30.0 } else { Vec3::ZERO };
                // An unidentified radar return well above terrain can still be
                // intercepted; never use a hidden unit definition to identify it.
                let air = e.can_fly()
                    || (!e.identified
                        && live
                        && e.position.y > ground.height_at(e.position.x, e.position.z).max(0.0) + 100.0);
                Contact {
                    id: e.unit_id,
                    pos: e.position,
                    vel: v,
                    value: e.value,
                    hp: e.get_health().max(0.0) * e.def_health(),
                    max_hp: e.def_health(),
                    seen: e.last_seen,
                    known: e.identified,
                    live,
                    air,
                    static_: e.is_static,
                    heavy: e.is_com || e.is_arty || e.is_major_cancer || (!e.is_static && e.value >= 1000.0),
                    raider: e.is_raider
                        || self.unit_classes.small_raiders.contains(e.def_name.as_str())
                        || self.unit_classes.medium_raiders.contains(e.def_name.as_str()),
                    wind: e.def_name == "energywind",
                    singu: e.def_name == "energysingu",
                    home: self.graph.is_ally_territory(e.position),
                    fighting: !e.is_static
                        && e.can_attack()
                        && self.war.get_friendly_threat(e.position) > 0.5
                        && self.war.get_threat(e.position) > 0.3,
                }
            })
            .collect()
    }

    fn airstrike_interceptors(&self) -> Vec<(Vec3, f32, f32)> {
        self.war
            .targets
            .values()
            .filter(|e| {
                !e.is_dead
                    && e.can_fly()
                    && (e.is_aa
                        || e.is_flex_aa
                        || matches!(e.def_name.as_str(), "planefighter" | "planeheavyfighter" | "planesupport"))
                    && self.frame - e.last_seen < 30 * 20
            })
            .map(|e| (e.position, e.threat_radius.max(600.0) + e.def_speed() * 5.0, e.get_danger().max(250.0) / 500.0))
            .collect()
    }

    fn airstrike_field(&self, from: Vec3, interceptors: &[(Vec3, f32, f32)]) -> AirField {
        let map = self.cb.map();
        AirField::with_interceptors(&self.war, map.width() / 8, map.height() / 8, from, |p| {
            interceptors.iter().filter(|(at, r, _)| distance(*at, p) < *r).map(|(_, _, t)| t).sum()
        })
    }

    fn airstrike_risk(&self, air: &AirField, pos: Vec3, interceptors: &[(Vec3, f32, f32)]) -> Risk {
        let intercepted = air.route(pos).iter().any(|p| interceptors.iter().any(|(at, r, _)| distance(*p, *at) < *r));
        Risk { aa: self.war.get_aa_threat(pos), exposure: air.exposure(pos).unwrap_or(f32::INFINITY), intercepted }
    }

    pub fn bomber_update(&mut self, frame: i32) {
        if frame % PERIOD != self.offset % PERIOD {
            return;
        }
        let has_factory = self.fac.factories.values().any(|f| f.kind() == FacKind::Plane);
        if !has_factory && self.bomber_handler.units.is_empty() {
            return;
        }
        let home = self.graph.get_ally_center();
        let team = self.team_id;
        self.bomber_handler.units.retain(|_, a| a.unit.health() > 0.0 && a.unit.team() == team);
        let alive: BTreeSet<_> = self.cb.team_units().into_iter().map(|u| u.id()).collect();
        self.bomber_handler.building.retain(|id, _| alive.contains(id));
        self.bomber_handler.orders.retain(|id, (_, f)| alive.contains(id) && frame - *f < 30 * 30);
        self.bomber_handler.cooldown.retain(|_, f| frame - *f < COOLDOWN);
        if self.bomber_handler.raven_damage.is_none() {
            let damage = self
                .defs
                .get(Plane::Raven.name())
                .into_iter()
                .flat_map(|d| d.weapon_mounts())
                .filter_map(|m| m.weapon_def())
                .filter(|w| !w.name().contains("fake") && !w.name().contains("torpedo"))
                .filter_map(|w| w.damage_types().first().copied())
                .fold(0.0, f32::max);
            self.bomber_handler.raven_damage = Some(if damage > 0.0 { damage } else { 800.0 });
        }
        let interceptors = self.airstrike_interceptors();
        // Defensive interception accepts the target fighter itself; ground AA
        // still gates the mission and its route.
        let air = self.airstrike_field(home, &interceptors);
        let defense_air = self.airstrike_field(home, &[]);
        let contacts = self.airstrike_contacts();
        let jobs = missions(&contacts, home, frame);
        let had_jobs = !jobs.is_empty();
        let mut plans = Vec::new();
        for mut job in jobs {
            if job.kind == MissionKind::Thermite && air.length(job.pos).unwrap_or(f32::INFINITY) >= 3000.0 {
                job.deep = true;
            }
            if job.kind == MissionKind::Shield {
                let ally =
                    self.cb.friendly_units_in(job.pos, 600.0).into_iter().find(|u| {
                        u.def().is_some_and(|d| !d.is_able_to_fly() && d.speed() > 0.0 && d.is_able_to_attack())
                    });
                let Some(ally) = ally else { continue };
                job.pos = ally.pos();
            }
            let field = if job.kind == MissionKind::Intercept { &defense_air } else { &air };
            let risk = self.airstrike_risk(
                field,
                job.pos,
                if job.kind == MissionKind::Intercept { &[] } else { &interceptors },
            );
            let choices: Vec<_> = packages(&job, risk, self.bomber_handler.raven_damage.unwrap_or(800.0))
                .into_iter()
                .filter(|p| {
                    p.iter().all(|(k, _)| self.defs.get(k.name()).is_some_and(|d| risk.survives(d.health(), d.speed())))
                })
                .collect();
            // Seeing a still-existing job become unsafe invalidates its memory
            // immediately; a mere loss of radar contact may retain procurement.
            self.bomber_handler.recent.remove(&job.key());
            if choices.is_empty() {
                continue;
            }
            let length = field.length(job.pos).unwrap_or(f32::INFINITY);
            let route = field.route(job.pos);
            let plan = Plan { job, packages: choices, risk, length, route };
            if plan.job.kind.primary() {
                self.bomber_handler.recent.insert(plan.job.key(), plan.clone());
            }
            plans.push(plan);
        }
        let known: BTreeSet<_> = self.war.targets.keys().copied().collect();
        self.bomber_handler.recent.retain(|_, p| frame - p.job.seen <= MEMORY && known.contains(&p.job.target));
        // Recheck remembered routes too: an AA sighting elsewhere can close a
        // route without refreshing the target's radar contact.
        let unsafe_keys: Vec<_> = self
            .bomber_handler
            .recent
            .iter()
            .filter(|(_, p)| {
                let defensive = p.job.kind == MissionKind::Intercept;
                let r = self.airstrike_risk(
                    if defensive { &defense_air } else { &air },
                    p.job.pos,
                    if defensive { &[] } else { &interceptors },
                );
                let allowed = packages(&p.job, r, self.bomber_handler.raven_damage.unwrap());
                !p.packages.iter().any(|pkg| {
                    allowed.contains(pkg)
                        && pkg
                            .iter()
                            .all(|(k, _)| self.defs.get(k.name()).is_some_and(|d| r.survives(d.health(), d.speed())))
                })
            })
            .map(|(k, _)| *k)
            .collect();
        for key in unsafe_keys {
            self.bomber_handler.recent.remove(&key);
        }
        self.bomber_handler.grounded = self.dev_off("airstrikes")
            || (had_jobs && plans.is_empty() && self.bomber_handler.recent.is_empty())
            || self.war.get_aa_threat(home) > 1.25;
        self.bomber_handler.plans = plans;
        self.airstrike_fly(frame, home, &air, &defense_air, &interceptors);
        self.airstrike_launch(frame, home, &air, &defense_air);
        if frame - self.bomber_handler.last_report >= 30 * 30 {
            self.bomber_handler.last_report = frame;
            self.log(&format!(
                "airstrikes: {} jobs, {} recent primary, {} planes, {} sorties; grounded={} next={:?}; launched={}",
                self.bomber_handler.plans.len(),
                self.bomber_handler.recent.len(),
                self.bomber_handler.units.len(),
                self.bomber_handler.sorties.len(),
                self.bomber_handler.grounded,
                self.airstrike_production(),
                self.bomber_handler.launched
            ));
        }
    }

    fn airstrike_fly(
        &mut self,
        frame: i32,
        home: Vec3,
        air: &AirField,
        defense_air: &AirField,
        interceptors: &[(Vec3, f32, f32)],
    ) {
        let sorties = std::mem::take(&mut self.bomber_handler.sorties);
        let mut active = BTreeSet::new();
        let aircraft: BTreeMap<_, _> = self.bomber_handler.units.iter().map(|(&id, a)| (id, a.unit)).collect();
        for mut sortie in sorties {
            let target = self.war.targets.get(&sortie.job.target);
            let valid = target.is_some_and(|e| {
                !e.is_dead && (e.is_static || ((e.visible || e.is_radar_visible) && frame - e.last_seen <= FRESH))
            });
            let pos = if sortie.job.kind == MissionKind::Shield {
                sortie.job.pos
            } else {
                target.map_or(sortie.job.pos, |e| e.position)
            };
            let field = if sortie.job.kind == MissionKind::Intercept { defense_air } else { air };
            let enemies = if sortie.job.kind == MissionKind::Intercept { &[][..] } else { interceptors };
            let from = sortie
                .members
                .iter()
                .filter_map(|id| self.bomber_handler.units.get(id))
                .find(|a| a.kind != Plane::Sparrow)
                .map(|a| a.unit.pos())
                .unwrap_or(home);
            let approach = self.airstrike_field(from, enemies);
            let mut risk = self.airstrike_risk(&approach, pos, enemies);
            // Remaining ingress plus the planned egress to home, not twice the
            // now-shorter ingress. Replan live approaches as new AA is seen.
            risk.exposure = (risk.exposure + field.exposure(pos).unwrap_or(f32::INFINITY)) * 0.5;
            let reroute = frame - sortie.last_route >= 150 && distance(from, pos) > 800.0;
            let route = if reroute { approach.route(pos) } else { Vec::new() };
            let still_job = self.bomber_handler.plans.iter().any(|p| p.job.key() == sortie.job.key());
            let done = self.dev_off("airstrikes") || !valid || !still_job || frame - sortie.launched > SORTIE_TIMEOUT;
            let mut keep = Vec::new();
            for id in sortie.members {
                let Some(a) = self.bomber_handler.units.get_mut(&id) else { continue };
                let u = a.unit;
                let spent = u.rules_param_float("noammo", 0.0) > 0.0;
                let safe = u.def().is_some_and(|d| risk.survives(u.health(), d.speed()));
                if done || spent || !safe || u.health() < u.max_health() * 0.5 {
                    // Cancel queued attacks even if already over the rally
                    // point (the recovery loop may have no move to issue).
                    u.stop(0);
                    a.returning = true;
                    a.last_order = -1000;
                    self.bomber_handler.cooldown.insert(sortie.job.target, frame);
                    continue;
                }
                if a.kind == Plane::Sparrow {
                    // Guard follows the Odin through its route, instead of
                    // sending the faster jammer ahead to the release point.
                    if let Some(leader) = a.escort.and_then(|id| aircraft.get(&id).copied())
                        && leader.health() > 0.0
                        && leader.rules_param_float("noammo", 0.0) == 0.0
                    {
                        if frame - a.last_order >= 90 {
                            u.guard(leader, 0);
                            a.last_order = frame;
                        }
                        active.insert(id);
                        keep.push(id);
                        continue;
                    }
                    u.stop(0);
                    a.returning = true;
                    a.last_order = -1000;
                    continue;
                }
                if reroute && !route.is_empty() {
                    Self::airstrike_route(u, &route, true);
                    Self::airstrike_attack(u, &sortie.job, target.map(|e| e.unit), pos, OPTION_SHIFT_KEY);
                }
                // Keep the queued safe approach intact. Retarget a moving
                // contact only in the terminal area, at most every three seconds.
                if frame - sortie.last_order >= 90 && distance(u.pos(), pos) < 650.0 {
                    Self::airstrike_attack(u, &sortie.job, target.map(|e| e.unit), pos, 0);
                }
                active.insert(id);
                keep.push(id);
            }
            if !keep.is_empty() {
                sortie.members = keep;
                sortie.job.pos = pos;
                if reroute {
                    sortie.last_route = frame;
                    sortie.route = route;
                }
                if frame - sortie.last_order >= 90 {
                    sortie.last_order = frame;
                }
                self.bomber_handler.sorties.push(sortie);
            }
        }
        // Return along a fresh threat-aware route. In enemy territory a direct
        // FIND_PAD order would throw away the safe egress route.
        let ids: Vec<_> = self.bomber_handler.units.keys().copied().filter(|id| !active.contains(id)).collect();
        for id in ids {
            let a = &self.bomber_handler.units[&id];
            let u = a.unit;
            let hurt = u.health() < u.max_health() * 0.95;
            let empty = u.rules_param_float("noammo", 0.0) > 0.0;
            let at_home = distance(u.pos(), home) < 450.0;
            if at_home && !hurt && !empty {
                self.bomber_handler.units.get_mut(&id).unwrap().returning = false;
                continue;
            }
            if frame - a.last_order < 30 * 10 {
                continue;
            }
            if a.kind == Plane::Sparrow
                && !hurt
                && let Some(leader) = a.escort.and_then(|id| self.bomber_handler.units.get(&id)).map(|a| a.unit)
                && distance(leader.pos(), home) > 450.0
            {
                u.guard(leader, 0);
                self.bomber_handler.units.get_mut(&id).unwrap().last_order = frame;
                continue;
            }
            let needs_pad = hurt || empty;
            let route = if !(at_home || needs_pad && self.graph.is_ally_territory(u.pos())) {
                self.airstrike_field(u.pos(), interceptors).route(home)
            } else {
                vec![]
            };
            let a = self.bomber_handler.units.get_mut(&id).unwrap();
            a.returning = true;
            a.last_order = frame;
            if !route.is_empty() {
                Self::airstrike_route(u, &route, false);
            }
            if hurt || empty {
                u.execute_custom_command(
                    cmd::FIND_PAD,
                    &[],
                    if route.is_empty() { 0 } else { OPTION_SHIFT_KEY },
                    NO_TIMEOUT,
                );
            }
        }
    }

    fn airstrike_launch(&mut self, frame: i32, home: Vec3, air: &AirField, defense_air: &AirField) {
        if self.dev_off("airstrikes") {
            return;
        }
        let mut busy: BTreeSet<_> =
            self.bomber_handler.sorties.iter().flat_map(|s| s.members.iter().copied()).collect();
        let mut covered: BTreeSet<_> =
            self.bomber_handler.sorties.iter().flat_map(|s| s.job.targets.iter().copied()).collect();
        let plans = self.bomber_handler.plans.clone();
        let primary_kinds: BTreeSet<_> = plans
            .iter()
            .filter(|p| p.job.kind.primary())
            .flat_map(|p| p.packages.iter().flatten().map(|(k, _)| *k))
            .collect();
        for plan in plans {
            if self.bomber_handler.sorties.len() >= 4 {
                break;
            }
            let job = &plan.job;
            if job.targets.iter().any(|id| covered.contains(id))
                || self.bomber_handler.cooldown.contains_key(&job.target)
            {
                continue;
            }
            let mut selected = None;
            for package in &plan.packages {
                if !job.kind.primary() && package.iter().any(|(k, _)| primary_kinds.contains(k)) {
                    continue;
                }
                let mut members = Vec::new();
                for &(kind, n) in package {
                    let candidates: Vec<_> = self
                        .bomber_handler
                        .units
                        .iter()
                        .filter(|(id, a)| {
                            !busy.contains(id)
                                && a.kind == kind
                                && !a.returning
                                && a.unit.rules_param_float("noammo", 0.0) == 0.0
                                && a.unit.health() >= a.unit.max_health() * 0.95
                                && distance(a.unit.pos(), home) < 450.0
                        })
                        .take(n)
                        .map(|(id, _)| *id)
                        .collect();
                    if candidates.len() != n {
                        members.clear();
                        break;
                    }
                    members.extend(candidates);
                }
                if !members.is_empty() {
                    selected = Some((members, package.clone()));
                    break;
                }
            }
            let Some((members, package)) = selected else { continue };
            // An opportunistic assassination must arrive while the observed
            // window still exists; no procurement or long cross-map pursuit.
            if job.kind == MissionKind::Assassinate {
                let speed = members
                    .iter()
                    .filter_map(|id| self.bomber_handler.units[id].unit.def())
                    .map(|d| d.speed())
                    .fold(f32::INFINITY, f32::min);
                let velocity = self.war.targets.get(&job.target).map_or(Vec3::ZERO, |e| e.unit.vel() * 30.0);
                if plan.length / speed.max(1.0) > 12.0
                    || intercept_time(home, job.pos, velocity, speed).is_none_or(|t| t > 12.0)
                {
                    continue;
                }
            }
            // Napalm and implosion blasts should not carpet our own troops.
            if matches!(job.kind, MissionKind::Group | MissionKind::Wind | MissionKind::Heavy | MissionKind::Interdict)
                && members.iter().any(|id| matches!(self.bomber_handler.units[id].kind, Plane::Phoenix | Plane::Likho))
                && self
                    .cb
                    .friendly_units_in(job.pos, 220.0)
                    .iter()
                    .any(|u| u.def().is_some_and(|d| !d.is_able_to_fly()))
            {
                continue;
            }
            let route = if job.kind == MissionKind::Intercept { defense_air } else { air }.route(job.pos);
            if route.is_empty() {
                continue;
            }
            let odins: Vec<_> =
                members.iter().copied().filter(|id| self.bomber_handler.units[id].kind == Plane::Odin).collect();
            let escorts: Vec<_> =
                members.iter().copied().filter(|id| self.bomber_handler.units[id].kind == Plane::Sparrow).collect();
            let mut formed = true;
            for (&escort, &odin) in escorts.iter().zip(&odins) {
                let leader = self.bomber_handler.units[&odin].unit;
                let a = self.bomber_handler.units.get_mut(&escort).unwrap();
                a.escort = Some(odin);
                let radius = a.unit.def().map_or(540.0, |d| d.jammer_radius() as f32);
                if distance(a.unit.pos(), leader.pos()) > radius * 0.75 {
                    a.unit.guard(leader, 0);
                    a.last_order = frame;
                    formed = false;
                }
            }
            if !formed {
                continue;
            }
            for &id in &members {
                let leader = self.bomber_handler.units[&id]
                    .escort
                    .and_then(|id| self.bomber_handler.units.get(&id))
                    .map(|a| a.unit);
                let a = self.bomber_handler.units.get_mut(&id).unwrap();
                a.returning = false;
                a.last_order = frame;
                if a.kind == Plane::Sparrow {
                    if let Some(leader) = leader {
                        a.unit.guard(leader, 0);
                    }
                } else {
                    Self::airstrike_route(a.unit, &route, true);
                    Self::airstrike_attack(
                        a.unit,
                        job,
                        self.war.targets.get(&job.target).map(|e| e.unit),
                        job.pos,
                        OPTION_SHIFT_KEY,
                    );
                }
                busy.insert(id);
            }
            covered.extend(job.targets.iter().copied());
            self.log(&format!(
                "airstrikes: launch {:?} target={} package={} value={:.0} AA={:.2} exposure={:.2}",
                job.kind,
                job.target,
                members.len(),
                job.value,
                plan.risk.aa,
                plan.risk.exposure
            ));
            self.bomber_handler.launched += 1;
            self.bomber_handler.sorties.push(Sortie {
                job: job.clone(),
                members,
                launched: frame,
                last_order: frame,
                last_route: frame,
                route,
                package,
            });
        }
    }

    fn airstrike_route(u: Unit, route: &[Vec3], attacking: bool) {
        // Stop the move queue one cell before impact and let the attack command
        // do the terminal approach. Never cut across bends in the planned route.
        let n = if attacking { route.len().saturating_sub(1) } else { route.len() };
        for (i, p) in route.iter().take(n).enumerate() {
            u.move_to(*p, if i == 0 { 0 } else { OPTION_SHIFT_KEY });
        }
        if n == 0 {
            u.stop(0);
        }
    }

    fn airstrike_attack(u: Unit, job: &Mission, target: Option<Unit>, pos: Vec3, options: i16) {
        match job.kind {
            MissionKind::Shield => {
                u.execute_custom_command(cmd::AIR_MANUALFIRE, &[pos.x, pos.y, pos.z], options, NO_TIMEOUT);
            }
            MissionKind::Wind | MissionKind::Thermite | MissionKind::Static => {
                u.attack_area(pos, 0.0, options);
            }
            _ => {
                if let Some(t) = target {
                    u.attack(t, options);
                } else {
                    u.attack_area(pos, 0.0, options);
                }
            }
        }
    }

    pub fn airstrikes_status(&self) -> serde_json::Value {
        self.bomber_handler.status(self.frame, self.airstrike_production())
    }
}

/// Both live tools and journal snapshots use the same state projection. Active
/// sorties win over overlapping candidates, including those with a new anchor.
fn mission_view(
    job: &Mission,
    route: &[Vec3],
    packages: &[Vec<(Plane, usize)>],
    sortie: Option<&Sortie>,
) -> serde_json::Value {
    use serde_json::json;
    json!({
        "id": format!("{:?}:{}", job.kind, job.target),
        "kind": format!("{:?}", job.kind), "target": job.target, "targets": job.targets,
        "state": if sortie.is_some() { "in_progress" } else { "available" },
        "primary": job.kind.primary(), "deep": job.deep, "value": job.value,
        "pos": [job.pos.x, job.pos.z], "seen": job.seen,
        "route": route.iter().map(|p| [p.x, p.z]).collect::<Vec<_>>(),
        "members": sortie.map(|s| s.members.as_slice()).unwrap_or(&[]),
        "launched": sortie.map(|s| s.launched),
        "packages": packages.iter().map(|pkg| pkg.iter().map(|(k,n)| json!({
            "unit": k.name(), "name": format!("{k:?}"), "count": n
        })).collect::<Vec<_>>()).collect::<Vec<_>>()
    })
}

impl BomberHandler {
    fn status(&self, frame: i32, next: Option<&str>) -> serde_json::Value {
        use serde_json::json;
        let covered: BTreeSet<_> = self.sorties.iter().flat_map(|s| s.job.targets.iter().copied()).collect();
        let mut missions: Vec<_> = self
            .sorties
            .iter()
            .map(|s| mission_view(&s.job, &s.route, std::slice::from_ref(&s.package), Some(s)))
            .collect();
        for p in &self.plans {
            if p.job.targets.iter().any(|id| covered.contains(id)) {
                continue;
            }
            let mut v = mission_view(&p.job, &p.route, &p.packages, None);
            v["aa"] = json!(p.risk.aa);
            v["exposure"] = json!(p.risk.exposure);
            missions.push(v);
        }
        json!({"frame": frame, "grounded": self.grounded, "next": next, "aircraft": self.units.len(),
            "ordered": self.orders.len(), "building": self.building.len(),
            "available": missions.len() - self.sorties.len(), "in_progress": self.sorties.len(),
            "missions": missions,
            "sorties": self.sorties.iter().map(|s| json!({"kind": format!("{:?}",s.job.kind),
                "target": s.job.target, "members": s.members, "launched": s.launched
            })).collect::<Vec<_>>()})
    }
}

#[cfg(test)]
mod operator_tests {
    use super::*;
    #[test]
    fn snapshots_show_candidate_then_active_then_available_without_duplicate_targets() {
        let job = Mission {
            kind: MissionKind::Thermite,
            target: 5,
            targets: vec![5],
            pos: Vec3::xz(1200.0, 300.0),
            hp: 4000.0,
            value: 4000.0,
            seen: 30,
            deep: true,
        };
        let route = vec![Vec3::xz(500.0, 200.0), job.pos];
        let package = vec![(Plane::Odin, 1), (Plane::Sparrow, 1)];
        let mut h = BomberHandler::default();
        h.plans.push(Plan {
            job: job.clone(),
            route: route.clone(),
            packages: vec![package.clone()],
            risk: Risk { aa: 0.0, exposure: 0.0, intercepted: false },
            length: 1400.0,
        });
        let candidate = h.status(30, Some("bomberassault"));
        assert_eq!(candidate["missions"][0]["state"], "available");
        assert_eq!(candidate["missions"][0]["pos"], serde_json::json!([1200.0, 300.0]));
        assert_eq!(candidate["missions"][0]["packages"][0][1]["name"], "Sparrow");
        h.sorties.push(Sortie {
            job,
            route,
            package,
            members: vec![11, 12],
            launched: 60,
            last_order: 60,
            last_route: 60,
        });
        let active = h.status(90, None);
        assert_eq!(active["missions"].as_array().unwrap().len(), 1);
        assert_eq!(active["available"], 0);
        assert_eq!(active["in_progress"], 1);
        assert_eq!(active["missions"][0]["state"], "in_progress");
        assert_eq!(active["missions"][0]["members"], serde_json::json!([11, 12]));
        assert_eq!(active["missions"][0]["id"], candidate["missions"][0]["id"]);
        h.sorties.clear();
        assert_eq!(h.status(120, None)["missions"][0]["state"], "available");
        h.plans.clear();
        h.grounded = true;
        assert!(h.status(150, None)["missions"].as_array().unwrap().is_empty());
    }
}
