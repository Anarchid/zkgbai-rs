use super::*;
use crate::economy::factory::FacKind;
use crate::kgbutil::AirField;
use crate::military::Bomber;
use crate::util::{OPTION_SHIFT_KEY, distance};
use crate::{Ai, defs::cmd};
use springai::{NO_TIMEOUT, Unit, Vec3};
use std::collections::{BTreeMap, BTreeSet};

mod defense;
mod wave;
use defense::FireSource;

const PERIOD: i32 = 30;
const SORTIE_TIMEOUT: i32 = 30 * 150;
const COOLDOWN: i32 = 30 * 15;

#[derive(Default)]
struct Utilization {
    state_frames: BTreeMap<&'static str, u64>,
    launched: usize,
    payload_spent: usize,
    aborted: BTreeMap<&'static str, usize>,
}

fn readiness(reserved: bool, active: bool, empty: bool, healthy: bool, at_home: bool) -> &'static str {
    if reserved {
        "operator"
    } else if active {
        "in_progress"
    } else if empty {
        "rearming"
    } else if !healthy {
        "repairing"
    } else if !at_home {
        "returning"
    } else {
        "ready"
    }
}

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
    return_route: Vec<Vec3>,
}
impl Plan {
    fn score(&self) -> f32 {
        // Preserve primary-before-secondary dispatch, but favor value reachable
        // in a shorter flight within each tier. Interceptions remain urgent.
        if self.job.kind == MissionKind::Intercept {
            self.job.score()
        } else {
            self.job.score() / (1.0 + self.length / 1200.0)
        }
    }
}

#[derive(Clone, Copy)]
struct WaveFlight {
    id: i32,
    target: Vec3,
}

struct Sortie {
    wave: Option<WaveFlight>,
    job: Mission,
    members: Vec<i32>,
    launched: i32,
    last_order: i32,
    last_route: i32,
    route: Vec<Vec3>,
    return_route: Vec<Vec3>,
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
    last_wave_check: i32,
    wave_checks: usize,
    waves_launched: usize,
    last_wave_launch: i32,
    launched: usize,
    raven_damage: Option<f32>,
    defenses: Vec<FireSource>,
    utilization: BTreeMap<Plane, Utilization>,
}
impl BomberHandler {
    pub fn new() -> Self {
        Self::default()
    }
    fn refresh_priority_memory(&mut self, contacts: &[Contact], frame: i32) {
        // Fresh observations replace remembered priority demand, including when
        // healing demotes a target. Unseen targets retain the normal memory.
        for e in contacts.iter().filter(|e| e.known && !e.static_ && e.live && frame - e.seen <= FRESH) {
            self.recent.remove(&(MissionKind::Heavy, e.id));
        }
    }

    pub fn get_bomber_size(&self) -> usize {
        self.units.values().filter(|a| !matches!(a.kind, Plane::Sparrow | Plane::Raptor)).count()
    }
}

impl Ai {
    pub(crate) fn airstrike_manages(&self, id: i32) -> bool {
        self.bomber_handler.units.contains_key(&id)
    }

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
        plans.sort_by(|a, b| b.score().total_cmp(&a.score()));
        for plan in plans {
            if let Some(kind) = purchase(&plan.packages, &have, |k| {
                self.defs.get(k.name()).map_or(f32::INFINITY, |d| d.cost(self.metal))
            }) {
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
                    heavy: e.is_com || e.is_arty || e.is_major_cancer || (!e.is_static && e.value >= 600.0),
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

    fn airstrike_path_risk(&self, from: Vec3, route: &[Vec3], interceptors: &[(Vec3, f32, f32)]) -> Risk {
        defense::forecast(from, route, &self.bomber_handler.defenses, interceptors)
    }

    fn airstrike_risk(&self, air: &AirField, from: Vec3, pos: Vec3, interceptors: &[(Vec3, f32, f32)]) -> Risk {
        let route = air.route(pos);
        round_trip(
            self.airstrike_path_risk(from, &route, interceptors),
            self.airstrike_path_risk(pos, &return_route(from, &route), interceptors),
        )
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
        self.bomber_handler.defenses = self.airstrike_defenses();
        let interceptors = self.airstrike_interceptors();
        // Defensive interception accepts the target fighter itself; ground AA
        // still gates the mission and its route.
        let air = self.airstrike_field(home, &interceptors);
        let defense_air = self.airstrike_field(home, &[]);
        let contacts = self.airstrike_contacts();
        self.bomber_handler.refresh_priority_memory(&contacts, frame);
        let blast = self
            .defs
            .get(Plane::Likho.name())
            .into_iter()
            .flat_map(|d| d.weapon_mounts())
            .filter_map(|m| m.weapon_def())
            .filter(|w| !w.name().contains("fake"))
            .filter_map(|w| {
                w.damage_types().first().copied().map(|damage| Blast {
                    damage,
                    radius: w.area_of_effect(),
                    edge: w.edge_effectiveness(),
                })
            })
            .filter(|b| b.damage > 0.0 && b.radius > 0.0)
            .max_by(|a, b| a.damage.total_cmp(&b.damage))
            .unwrap_or_default();
        let jobs = missions(&contacts, home, frame);
        let had_jobs = !jobs.is_empty();
        let mut plans = Vec::new();
        for mut job in jobs {
            job.aim_group(blast);
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
                home,
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
            let return_route = return_route(home, &route);
            let plan = Plan { job, packages: choices, risk, length, route, return_route };
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
                    home,
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
        plans.sort_by(|a, b| {
            b.job
                .kind
                .primary()
                .cmp(&a.job.kind.primary())
                .then_with(|| b.score().total_cmp(&a.score()))
                .then_with(|| a.job.key().cmp(&b.job.key()))
        });
        self.bomber_handler.plans = plans;
        self.airstrike_fly(frame, home, &air, &defense_air, &interceptors);
        self.airstrike_wave(frame, home);
        self.airstrike_launch(frame, home);
        let busy: BTreeSet<_> = self.bomber_handler.sorties.iter().flat_map(|s| s.members.iter().copied()).collect();
        for (&id, a) in &self.bomber_handler.units {
            let state = readiness(
                springai::reserved::is_reserved(self.cb, id),
                busy.contains(&id),
                a.unit.rules_param_float("noammo", 0.0) > 0.0,
                a.unit.health() >= a.unit.max_health() * 0.95,
                distance(a.unit.pos(), home) < 450.0,
            );
            *self.bomber_handler.utilization.entry(a.kind).or_default().state_frames.entry(state).or_default() +=
                PERIOD as u64;
        }
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
            let exit = return_route(home, &field.route(pos));
            let risk = round_trip(
                self.airstrike_path_risk(from, &approach.route(pos), enemies),
                self.airstrike_path_risk(pos, &exit, enemies),
            );
            sortie.return_route = exit;
            let reroute = frame - sortie.last_route >= 150 && distance(from, pos) > 800.0;
            let route = if reroute { approach.route(pos) } else { Vec::new() };
            let still_job = self.bomber_handler.plans.iter().any(|p| p.job.key() == sortie.job.key());
            let disabled = self.dev_off("airstrikes");
            let wave_safe = sortie.wave.is_none_or(|w| {
                distance(w.target, pos) <= wave::MAX_PURSUIT
                    && wave::route_allowed(from, &approach.route(pos), |p| self.graph.is_ally_territory(p))
            });
            let done = disabled
                || !valid
                || (!still_job && sortie.wave.is_none())
                || !wave_safe
                || frame - sortie.launched > SORTIE_TIMEOUT;
            let mut keep = Vec::new();
            for id in sortie.members {
                let Some(a) = self.bomber_handler.units.get_mut(&id) else { continue };
                let u = a.unit;
                let spent = u.rules_param_float("noammo", 0.0) > 0.0;
                let safe = u.def().is_some_and(|d| risk.survives(u.health(), d.speed()));
                if springai::reserved::is_reserved(self.cb, id) {
                    *self
                        .bomber_handler
                        .utilization
                        .entry(a.kind)
                        .or_default()
                        .aborted
                        .entry("operator")
                        .or_default() += 1;
                    a.returning = true;
                    continue;
                }
                if done || spent || !safe || u.health() < u.max_health() * 0.5 {
                    let stats = self.bomber_handler.utilization.entry(a.kind).or_default();
                    if spent {
                        stats.payload_spent += 1;
                    } else {
                        let reason = if !valid {
                            "target_lost"
                        } else if !wave_safe {
                            "wave_depth"
                        } else if !still_job && sortie.wave.is_none() {
                            "mission_changed"
                        } else if !safe {
                            "unsafe_route"
                        } else if frame - sortie.launched > SORTIE_TIMEOUT {
                            "timeout"
                        } else if disabled {
                            "disabled"
                        } else {
                            "damage"
                        };
                        *stats.aborted.entry(reason).or_default() += 1;
                    }
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
            if springai::reserved::is_reserved(self.cb, id) {
                continue;
            }
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

    fn airstrike_launch(&mut self, frame: i32, home: Vec3) {
        if self.dev_off("airstrikes") {
            return;
        }
        let mut busy: BTreeSet<_> =
            self.bomber_handler.sorties.iter().flat_map(|s| s.members.iter().copied()).collect();
        let mut covered: BTreeSet<_> =
            self.bomber_handler.sorties.iter().flat_map(|s| s.job.targets.iter().copied()).collect();
        let plans = self.bomber_handler.plans.clone();
        // Plans are ordered primary first. Only aircraft actually committed
        // to a sortie are reserved: an unfillable package must not immobilize
        // every aircraft of that type while secondary opportunities expire.
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
                let mut members = Vec::new();
                for &(kind, n) in package {
                    let candidates: Vec<_> = self
                        .bomber_handler
                        .units
                        .iter()
                        .filter(|(id, a)| {
                            !busy.contains(id)
                                && !springai::reserved::is_reserved(self.cb, **id)
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
            if self.airstrike_dispatch(&plan, &members, &package, frame, None) {
                busy.extend(members);
                covered.extend(job.targets.iter().copied());
            }
        }
    }

    fn airstrike_dispatch(
        &mut self,
        plan: &Plan,
        members: &[i32],
        package: &[(Plane, usize)],
        frame: i32,
        wave: Option<WaveFlight>,
    ) -> bool {
        let job = &plan.job;
        let interceptors = if job.kind == MissionKind::Intercept { Vec::new() } else { self.airstrike_interceptors() };
        let exit = self.airstrike_path_risk(job.pos, &plan.return_route, &interceptors);
        if members.iter().any(|id| {
            let a = &self.bomber_handler.units[id];
            // A healthy aircraft may be circling near the rally rather than
            // exactly at home. Check its actual first leg, not an assumed start.
            let risk = round_trip(self.airstrike_path_risk(a.unit.pos(), &plan.route, &interceptors), exit);
            risk.intercepted || !a.unit.def().is_some_and(|d| risk.survives(a.unit.health(), d.speed()))
        }) {
            return false;
        }
        // Napalm and implosion blasts should not carpet our own troops.
        if members.iter().any(|id| matches!(self.bomber_handler.units[id].kind, Plane::Phoenix | Plane::Likho))
            && self.cb.friendly_units_in(job.pos, 220.0).iter().any(|u| u.def().is_some_and(|d| !d.is_able_to_fly()))
        {
            return false;
        }
        let route = plan.route.clone();
        if route.is_empty() {
            return false;
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
            return false;
        }
        for &id in members {
            let leader =
                self.bomber_handler.units[&id].escort.and_then(|id| self.bomber_handler.units.get(&id)).map(|a| a.unit);
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
            self.bomber_handler.utilization.entry(a.kind).or_default().launched += 1;
        }

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
            wave,
            job: job.clone(),
            members: members.to_vec(),
            launched: frame,
            last_order: frame,
            last_route: frame,
            route,
            return_route: plan.return_route.clone(),
            package: package.to_vec(),
        });
        true
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
        use serde_json::json;
        let mut status = self.bomber_handler.status(self.frame, self.airstrike_production());
        let busy: BTreeSet<_> = self.bomber_handler.sorties.iter().flat_map(|s| s.members.iter().copied()).collect();
        let home = self.graph.get_ally_center();
        status["fleet"] = json!(self.bomber_handler.units.iter().map(|(&id, a)| {
            let empty = a.unit.rules_param_float("noammo", 0.0) > 0.0;
            let health = a.unit.health() / a.unit.max_health().max(1.0);
            let away = distance(a.unit.pos(), home);
            json!({"id": id, "unit": a.kind.name(), "name": format!("{:?}", a.kind),
                "state": readiness(springai::reserved::is_reserved(self.cb, id), busy.contains(&id), empty, health >= 0.95, away < 450.0),
                "health": health, "noammo": empty, "home_distance": away})
        }).collect::<Vec<_>>());
        status
    }
}

/// Both live tools and journal snapshots use the same state projection. Active
/// sorties win over overlapping candidates, including those with a new anchor.
fn mission_view(
    job: &Mission,
    route: &[Vec3],
    return_route: &[Vec3],
    packages: &[Vec<(Plane, usize)>],
    sortie: Option<&Sortie>,
) -> serde_json::Value {
    use serde_json::json;
    json!({
        "id": format!("{:?}:{}", job.kind, job.target),
        "kind": format!("{:?}", job.kind), "label": job.kind.label(), "target": job.target, "targets": job.targets,
        "state": if sortie.is_some() { "in_progress" } else { "available" },
        "return_route": return_route.iter().map(|p| [p.x,p.z]).collect::<Vec<_>>(),
        "wave": sortie.and_then(|s| s.wave.map(|w| w.id)),
        "primary": job.kind.primary(), "deep": job.deep, "value": job.value,
        "pos": [job.pos.x, job.pos.z], "seen": job.seen,
        "route": route.iter().map(|p| [p.x, p.z]).collect::<Vec<_>>(),
        "members": sortie.map(|s| s.members.as_slice()).unwrap_or(&[]),
        "launched": sortie.map(|s| s.launched),
        "package_damage_values": (job.kind == MissionKind::Group).then(||
            packages.iter().map(|p| job.damage_value(p)).collect::<Vec<_>>()),
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
            .map(|s| mission_view(&s.job, &s.route, &s.return_route, std::slice::from_ref(&s.package), Some(s)))
            .collect();
        for p in &self.plans {
            if !p.job.kind.primary() || p.job.targets.iter().any(|id| covered.contains(id)) {
                continue;
            }
            let mut v = mission_view(&p.job, &p.route, &p.return_route, &p.packages, None);
            v["aa"] = json!(p.risk.aa);
            v["exposure"] = json!(p.risk.exposure);
            v["incoming_dps"] = json!(p.risk.aa * 500.0);
            v["ready_volley_damage"] = json!(p.risk.burst);
            missions.push(v);
        }
        json!({"frame": frame, "grounded": self.grounded, "next": next, "aircraft": self.units.len(),
            "ordered": self.orders.len(), "building": self.building.len(),
            "available": missions.len() - self.sorties.len(), "in_progress": self.sorties.len(),
            "missions": missions,
            "combined_strikes": {"checks": self.wave_checks, "last_check": self.last_wave_check,
                "launched": self.waves_launched, "last_launch": self.last_wave_launch},
            "utilization": self.utilization.iter().map(|(k, u)| json!({"unit": k.name(), "name": format!("{k:?}"),
                "aircraft_seconds": u.state_frames.iter().map(|(s, f)| (*s, *f as f64 / 30.0)).collect::<BTreeMap<_,_>>(),
                "aircraft_launched": u.launched, "payload_spent": u.payload_spent, "aborted": u.aborted})).collect::<Vec<_>>(),
            "sorties": self.sorties.iter().map(|s| json!({"kind": format!("{:?}",s.job.kind),
                "target": s.job.target, "members": s.members, "launched": s.launched
            })).collect::<Vec<_>>()})
    }
}

#[cfg(test)]
mod operator_tests {
    use super::*;
    #[test]
    fn readiness_distinguishes_payload_repair_return_and_operator_control() {
        assert_eq!(readiness(false, false, false, true, true), "ready");
        assert_eq!(readiness(false, false, true, true, true), "rearming");
        assert_eq!(readiness(false, false, false, false, true), "repairing");
        assert_eq!(readiness(false, false, false, true, false), "returning");
        assert_eq!(readiness(false, true, false, true, false), "in_progress");
        assert_eq!(readiness(true, true, false, true, false), "operator");
    }

    #[test]
    fn snapshots_show_candidate_then_active_then_available_without_duplicate_targets() {
        let job = Mission {
            kind: MissionKind::Thermite,
            group: None,
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
            return_route: return_route(Vec3::ZERO, &route),
            route: route.clone(),
            packages: vec![package.clone()],
            risk: Risk { aa: 0.0, exposure: 0.0, intercepted: false, burst: 0.0 },
            length: 1400.0,
        });
        for (id, kind) in [
            (6, MissionKind::Static),
            (7, MissionKind::Opportunistic),
            (8, MissionKind::Assassinate),
            (9, MissionKind::Shield),
        ] {
            let mut secondary = h.plans[0].clone();
            secondary.job.kind = kind;
            secondary.job.target = id;
            secondary.job.targets = vec![id];
            h.plans.push(secondary);
        }
        let candidate = h.status(30, Some("bomberassault"));
        assert_eq!(candidate["available"], 1);
        assert_eq!(candidate["missions"].as_array().unwrap().len(), 1);
        assert_eq!(h.plans.len(), 5); // filtering readouts must not remove launch opportunities
        assert_eq!(candidate["missions"][0]["state"], "available");
        assert_eq!(candidate["missions"][0]["pos"], serde_json::json!([1200.0, 300.0]));
        assert_eq!(candidate["missions"][0]["packages"][0][1]["name"], "Sparrow");
        h.sorties.push(Sortie {
            wave: None,
            job,
            return_route: return_route(Vec3::ZERO, &route),
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
        h.sorties[0].job.kind = MissionKind::Static;
        let active_secondary = h.status(100, None);
        assert_eq!(active_secondary["missions"].as_array().unwrap().len(), 1);
        assert_eq!(active_secondary["missions"][0]["primary"], false);
        assert_eq!(active_secondary["missions"][0]["state"], "in_progress");
        assert_eq!(active_secondary["in_progress"], 1);
        assert_eq!(active_secondary["available"], 0);
        h.sorties.clear();
        assert_eq!(h.status(120, None)["missions"][0]["state"], "available");
        let mut priority = h.plans[0].clone();
        priority.job.kind = MissionKind::Heavy;
        let key = priority.job.key();
        h.recent.insert(key, priority);
        let mut contact = super::super::tests::enemy(5);
        contact.live = false;
        h.refresh_priority_memory(&[contact.clone()], 300);
        assert!(h.recent.contains_key(&key));
        contact.live = true;
        h.refresh_priority_memory(&[contact.clone()], 1000);
        assert!(h.recent.contains_key(&key));
        h.refresh_priority_memory(&[contact], 300);
        assert!(!h.recent.contains_key(&key));
        h.plans.clear();
        h.grounded = true;
        assert!(h.status(150, None)["missions"].as_array().unwrap().is_empty());
    }
}
