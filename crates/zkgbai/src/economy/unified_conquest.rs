//! Objective-led economic expansion (`unifiedconquest`). Conquest's atlas
//! supplies strategic destinations; cleared invasion captures and free metal
//! supply productive steps. This policy never starts a fortification trip.
//! Each builder owns a small catchment until it is built, unsafe or stalled.
//! Explicit conquest, operator, project and recovery controllers retain priority.

use std::collections::{BTreeMap, BTreeSet};

use springai::Vec3;

use super::{Ai, Worker, WorkerTask};
use crate::util::{NULLPOS, distance};

const PERIOD: i32 = 150;
const STALLED: i32 = 120 * 30;
const CLUSTER: f32 = 700.0;
const HORIZON: f32 = 480.0;

#[derive(Default)]
pub(super) struct Unified {
    mode: Option<u8>,
    missions: BTreeMap<i32, Mission>,
    destinations: BTreeMap<i32, Vec3>,
    cooldown: BTreeMap<(i32, usize), i32>,
    pub want_air: Option<i32>,
    builder: Option<Builder>,
    ordered: Option<(springai::UnitDef, i32)>,
}

struct Builder {
    factory: i32,
    def: springai::UnitDef,
    spots: Vec<usize>,
}

#[derive(Clone)]
struct Mission {
    spots: Vec<usize>,
    waypoint: Vec3,
    best: f32,
    gained: i32,
}

#[derive(Clone)]
struct Objective {
    spots: Vec<usize>,
    center: Vec3,
    income: f32,
}

/// Spend only on income that starts inside the horizon; walking ties up
/// build power too. Compare payback rates, so a distant rich region does
/// not steal the opening from mexes under our hands.
fn value(income: f32, spots: usize, travel: f32, bp: f32, extra: f32) -> f32 {
    let build = 85.0 * spots as f32;
    let seconds = build / bp.max(1.0);
    let net = income * (HORIZON - travel - seconds / 2.0).max(0.0) - build - bp * travel - extra;
    net / (30.0 + travel + seconds)
}

/// A commander may economically advance through our half, but ordinary
/// expansion beyond it requires an owned foothold and supporting force. Explicit conquest
/// deliberately has its own commitment and safety policy.
fn our_half(p: Vec3, home: Vec3, enemy: Vec3) -> bool {
    distance(p, home) <= 0.9 * distance(p, enemy)
}

impl Ai {
    pub(crate) fn eco_unified(&self) -> bool {
        !self.dev_off("unifiedconquest")
    }
    pub(super) fn eco_lanes(&self) -> bool {
        !self.eco_unified() && !self.dev_off("lanes")
    }
    pub(super) fn eco_path_policy(&self) -> bool {
        self.eco_unified() || self.eco_lanes()
    }

    pub(super) fn eco_legacy_energy(&self) -> bool {
        self.eco.estalled && self.eco.max_storage > 1.0 && self.eco.metal >= 0.8 * self.eco.max_storage
    }

    fn unified_worker(&self, w: &Worker) -> bool {
        (!w.is_industrious || !self.eco_energy_goal_unmet())
            && !w.is_chicken
            && w.raid_defense.is_none()
            && w.unit.health() > 0.0
            && w.unit.team() == self.team_id
            && !w.unit.is_being_built()
            && !(w.assigned_plop && w.has_plop())
            && !self.eco_worker_held(w.id)
            && !self.recovery_owns(w.id)
            && !springai::reserved::is_reserved(self.cb, w.id)
            && !(w.is_com && self.conquest_active())
            && !w.task.is_some_and(|id| self.eco.tasks.get(id).is_some_and(|t| self.eco_directed_job(w, t.id)))
    }

    fn unified_safe(&self, w: &Worker, p: Vec3) -> bool {
        let view = self.war.view(w.unit.def());
        if self.war.get_porc_threat(p) > 0.0
            || view.get_effective_threat(p) > view.get_total_friendly_threat(p)
            || w.shunned.iter().any(|&(s, until)| until > self.frame && distance(s, p) < 256.0)
        {
            return false;
        }
        if self.eco_raiders_near(p) > self.war.get_total_friendly_power(p) && !self.conquest_ground_safe(p) {
            return false;
        }
        if !w.is_com {
            return true;
        }
        if self.comguard_avoids(w.id, p) {
            return false;
        }
        // Wounded commanders can work locally or move home, never commit a
        // fresh outward trip. No destination is worth a commander gamble.
        let home = self.graph.get_start_pos();
        if home == NULLPOS {
            return true;
        }
        if distance(w.get_pos(), p) <= w.unit.def().map_or(0.0, |d| d.build_distance()) {
            return true;
        }
        if w.unit.health() < 0.75 * w.unit.max_health().max(1.0) && distance(p, home) > distance(w.get_pos(), home) {
            return false;
        }
        let enemy = self.graph.get_enemy_base().unwrap_or_else(|| self.graph.get_enemy_center());
        if enemy == NULLPOS || distance(home, enemy) < 1.0 {
            return distance(p, home) <= 1200.0;
        }
        let support = (self.war.get_total_friendly_power(p) - w.power).max(0.0);
        let anchored = self.graph.metal_spots.iter().any(|s| (s.owned || s.allied) && distance(s.position, p) <= 600.0);
        our_half(p, home, enemy)
            || (anchored && support >= crate::terrain::HELD_POWER && support >= 1.5 * self.war.get_enemy_power(p))
    }

    pub(super) fn eco_unified_allows(&self, w: &Worker, t: &WorkerTask) -> bool {
        if !self.eco_unified()
            || !w.is_com
            || self.conquest_active()
            || self.eco_directed_job(w, t.id)
            || self.eco_factory_defense_hand(w, t)
            || (self.eco.hard_estalled && self.eco_energy_job(t))
        {
            return true;
        }
        self.unified_safe(w, t.get_pos())
    }

    fn unified_reaches(&self, w: &Worker, p: Vec3) -> bool {
        let Some(d) = w.unit.def() else { return false };
        self.unified_safe(w, p)
            && self.terrain_reaches(d, w.get_pos(), p, d.build_distance() + 32.0)
            && (d.is_able_to_fly()
                || w.routes
                    .as_ref()
                    .and_then(|r| r.path(p, d.build_distance()))
                    .map(|path| {
                        path.iter().all(|&step| {
                            self.unified_safe(w, step)
                                && self.war.get_enemy_power(step) <= self.war.get_total_friendly_power(step)
                        })
                    })
                    .unwrap_or_else(|| self.eco_walk_is_clear(w.unit, p)))
    }

    /// A bounded preference for productive steps toward a conquest site.
    /// We do not credit turret denial before fortifications actually exist.
    fn unified_direction(&self, w: &Worker, p: Vec3) -> f32 {
        if !w.is_com {
            return 1.0;
        }
        let Some(&site) = self.eco.unified.destinations.get(&w.id) else {
            return 1.0;
        };
        let progress = distance(w.get_pos(), site) - distance(p, site);
        1.0 + 0.25 * (progress / 700.0).clamp(-1.0, 1.0)
    }

    fn unified_objectives(&self) -> Vec<Objective> {
        let mut free: BTreeSet<usize> = self
            .graph
            .metal_spots
            .iter()
            .filter(|s| {
                !s.owned
                    && !s.allied
                    && !s.hostile
                    && (!s.enemy_shadowed
                        || self.conquest_ground_safe(s.position)
                        || self.unified_capture_weight(&[s.index]) > 1.0)
            })
            .map(|s| s.index)
            .collect();
        let mut result = Vec::new();
        while let Some(&first) = free.first() {
            let center = self.graph.metal_spots[first].position;
            let spots: Vec<_> = free
                .iter()
                .copied()
                .filter(|&k| distance(center, self.graph.metal_spots[k].position) <= CLUSTER)
                .collect();
            let income = spots.iter().map(|&k| self.graph.metal_spots[k].value).sum();
            for k in &spots {
                free.remove(k);
            }
            result.push(Objective { spots, center, income });
        }
        result
    }

    /// Update independent crews, retaining a useful assignment instead of
    /// auctioning their directions again after every completed building.
    pub(super) fn eco_unified_update(&mut self, frame: i32) {
        if frame % PERIOD != self.offset % PERIOD {
            return;
        }
        let mode = if self.eco_unified() {
            2
        } else if self.eco_lanes() {
            1
        } else {
            0
        };
        if self.eco.unified.mode != Some(mode) {
            self.eco.unified.missions.clear();
            self.eco.unified.destinations.clear();
            self.eco.unified.want_air = None;
            self.eco.unified.builder = None;
            self.eco.unified.cooldown.clear();
            self.eco.colony = None;
            self.eco.colony_wants_air = None;
            for w in self.eco.workers.values_mut() {
                w.expansion = None;
                if mode == 0 {
                    w.is_industrious = false;
                }
            }
            self.eco.unified.mode = Some(mode);
            self.op_emit("expansionpolicy", serde_json::json!({"mode":mode,"unifiedconquest":mode==2,"lanes":mode==1}));
        }
        if mode != 2 {
            return;
        }
        self.eco.unified.cooldown.retain(|_, until| *until > frame);
        if self.eco.hard_estalled || self.fac.factories.is_empty() {
            return;
        }
        // A strategic objective persists independently of the intermediate
        // mex catchments. Do not turn toward a different site after each mex.
        let commanders = self.eco.commanders.clone();
        for wid in commanders {
            let unfinished = |site: Vec3| {
                self.graph
                    .metal_spots
                    .iter()
                    .any(|s| !s.owned && !s.allied && !s.hostile && distance(s.position, site) <= CLUSTER)
            };
            if self.eco.unified.destinations.get(&wid).is_some_and(|&site| {
                !unfinished(site) || self.eco.workers.get(&wid).is_none_or(|w| distance(w.get_pos(), site) <= 512.0)
            }) {
                self.eco.unified.destinations.remove(&wid);
            }
            if self.eco.unified.destinations.contains_key(&wid) {
                continue;
            }
            let Some(w) = self.eco.workers.get(&wid) else {
                continue;
            };
            if let Some(site) = self
                .eco
                .conquest
                .objectives
                .iter()
                .filter_map(|o| {
                    let p = o.sites.first()?;
                    let p = Vec3::xz(p.x, p.z);
                    if !unfinished(p) || distance(w.get_pos(), p) <= 512.0 {
                        return None;
                    }
                    Some((p, (o.income + o.score.max(0.0) * 0.1) / (500.0 + distance(w.get_pos(), p))))
                })
                .max_by(|a, b| a.1.total_cmp(&b.1))
                .map(|x| x.0)
            {
                self.eco.unified.destinations.insert(wid, site);
            }
        }
        let mut kept = BTreeMap::new();
        for (wid, mut m) in std::mem::take(&mut self.eco.unified.missions) {
            let Some(w) = self.eco.workers.get(&wid) else { continue };
            let previous = m.spots.len();
            m.spots.retain(|&k| self.graph.metal_spots.get(k).is_some_and(|s| !s.owned && !s.allied && !s.hostile));
            let ferrying = self.drops_ferrying(wid);
            let next = m
                .spots
                .iter()
                .map(|&k| self.graph.metal_spots[k].position)
                .filter(|&p| ferrying || self.unified_reaches(w, p))
                .min_by(|&a, &b| self.eco_worker_route(w, a).total_cmp(&self.eco_worker_route(w, b)));
            let Some(next) = next else { continue };
            let d = distance(w.get_pos(), next);
            if m.spots.len() < previous || d < m.best - 64.0 || ferrying {
                m.best = d;
                m.gained = frame;
            }
            m.waypoint = next;
            if frame - m.gained >= STALLED {
                for &k in &m.spots {
                    self.eco.unified.cooldown.insert((wid, k), frame + 90 * 30);
                }
            } else if self.unified_worker(w) || ferrying {
                kept.insert(wid, m);
            }
        }
        self.eco.unified.missions = kept;
        self.eco.unified.builder = None;
        self.eco.unified.want_air = None;
        let objectives = self.unified_objectives();
        let mut claimed: BTreeSet<usize> =
            self.eco.unified.missions.values().flat_map(|m| m.spots.iter().copied()).collect();
        let mut workers: Vec<_> =
            self.eco.workers.values().filter(|w| self.unified_worker(w)).map(|w| (w.id, w.is_com)).collect();
        workers.sort_by_key(|&(id, com)| (!com, id));
        for (wid, _) in workers {
            if self.eco.unified.missions.contains_key(&wid) {
                continue;
            }
            let w = &self.eco.workers[&wid];
            let Some(def) = w.unit.def() else { continue };
            let mut best: Option<(Objective, Vec3, f32)> = None;
            for objective in &objectives {
                let spots: Vec<_> = objective
                    .spots
                    .iter()
                    .copied()
                    .filter(|k| !claimed.contains(k) && !self.eco.unified.cooldown.contains_key(&(wid, *k)))
                    .filter(|&k| self.unified_reaches(w, self.graph.metal_spots[k].position))
                    .collect();
                let Some(waypoint) = spots
                    .iter()
                    .map(|&k| self.graph.metal_spots[k].position)
                    .min_by(|&a, &b| self.eco_worker_route(w, a).total_cmp(&self.eco_worker_route(w, b)))
                else {
                    continue;
                };
                let income = spots.iter().map(|&k| self.graph.metal_spots[k].value).sum();
                let walk = (self.eco_worker_route(w, waypoint) - def.build_distance()).max(0.0);
                // Ordinary commander expansion is built a step at a time.
                if w.is_com && walk > 1400.0 {
                    continue;
                }
                let score = value(income, spots.len(), walk / def.speed().max(1.0), w.bp, 0.0)
                    * self.unified_direction(w, waypoint)
                    * self.unified_capture_weight(&spots);
                if score > 0.0 && best.as_ref().is_none_or(|b| score > b.2) {
                    best = Some((Objective { spots, center: objective.center, income }, waypoint, score));
                }
            }
            if let Some((objective, waypoint, score)) = best {
                for k in &objective.spots {
                    claimed.insert(*k);
                }
                self.unified_assign(wid, objective, waypoint, score);
            }
        }
        // Remote free ground still supports the old transport capability,
        // but selection and worker ownership belong to this planner.
        if self.eco.effective_income >= 15.0 {
            let remote = objectives
                .iter()
                .filter(|o| o.spots.len() >= 3 && o.spots.iter().all(|k| !claimed.contains(k)))
                .filter(|o| {
                    o.spots.iter().all(|&k| {
                        let p = self.graph.metal_spots[k].position;
                        self.war.get_porc_threat(p) <= 0.0
                            && self.war.get_enemy_power(p) <= 0.0
                            && self.eco_raiders_near(p) <= self.war.get_total_friendly_power(p)
                    })
                })
                .max_by(|a, b| a.income.total_cmp(&b.income))
                .cloned();
            if let Some(o) = remote {
                if self.unified_request_builder(&o) {
                    return;
                }
                let worker = self
                    .eco
                    .workers
                    .values()
                    .filter(|w| !w.is_com && self.unified_worker(w) && !self.eco.unified.missions.contains_key(&w.id))
                    .min_by(|a, b| distance(a.get_pos(), o.center).total_cmp(&distance(b.get_pos(), o.center)))
                    .map(|w| (w.id, w.unit, w.bp));
                if let Some((wid, unit, bp)) = worker {
                    let score = value(o.income, o.spots.len(), 60.0, bp, 0.0);
                    if score > 0.0 && self.drops_ferry(unit, o.center) {
                        let center = o.center;
                        self.unified_assign(wid, o, center, score);
                        return;
                    }
                }
                self.eco.unified.want_air = Some(frame);
            }
        }
    }

    fn unified_assign(&mut self, wid: i32, o: Objective, waypoint: Vec3, score: f32) {
        let best = distance(self.eco.workers[&wid].get_pos(), waypoint);
        self.op_emit(
            "expansionmission",
            serde_json::json!({"worker":wid,"spots":o.spots,"waypoint":[waypoint.x,waypoint.z],"score":score}),
        );
        self.eco.unified.missions.insert(wid, Mission { spots: o.spots, waypoint, best, gained: self.frame });
    }

    pub(super) fn eco_unified_waypoint(&self, wid: i32) -> Option<Vec3> {
        self.eco.unified.missions.get(&wid).map(|m| m.waypoint)
    }

    pub(super) fn eco_unified_job_cost(&self, w: &Worker, t: &WorkerTask) -> f32 {
        if !self.eco_unified() || self.eco_directed_job(w, t.id) {
            return 0.0;
        }
        let Some(m) = self.eco.unified.missions.get(&w.id) else { return 0.0 };
        if !self.unified_worker(w) {
            return 0.0;
        }
        let Some(def) = w.unit.def() else { return 0.0 };
        let p = t.get_pos();
        if !self.unified_safe(w, p) {
            return 0.0;
        }
        if t.as_construction().is_some_and(|c| c.build_type.id() == self.eco.build_ids.mex_id)
            && m.spots.iter().any(|&k| distance(p, self.graph.metal_spots[k].position) < 64.0)
        {
            // A constant catchment bonus preserves distance ordering inside
            // it; canceling each job's walk would prefer arbitrary far mexes.
            return -(self.eco_worker_route(w, m.waypoint) / 4.0).min(2000.0) - 250.0;
        }
        if distance(w.get_pos(), p) <= def.build_distance() {
            return 0.0;
        }
        // Only a soft detour price; energy/emergencies and useful work behind
        // us remain available. Never the lane policy's hard onward override.
        ((distance(p, m.waypoint) - distance(w.get_pos(), m.waypoint)) * 0.15)
            .clamp(-def.speed() * 2.0, def.speed() * 4.0)
    }

    /// Access is a capability requirement, independent of the army's tier.
    /// Buy a compatible constructor from an existing factory only if its
    /// delayed income pays for it. New factories/combat forces remain the
    /// strategic production planner's responsibility.
    fn unified_request_builder(&mut self, o: &Objective) -> bool {
        let reaches = |d: springai::UnitDef, from: Vec3| {
            o.spots
                .iter()
                .all(|&k| self.terrain_reaches(d, from, self.graph.metal_spots[k].position, d.build_distance() + 32.0))
        };
        if self
            .eco
            .workers
            .values()
            .any(|w| !w.is_com && w.unit.health() > 0.0 && w.unit.def().is_some_and(|d| reaches(d, w.get_pos())))
        {
            return false;
        }
        // A rising constructor is already paid for, not a reason to order
        // another one from each factory on successive planner passes.
        if self.cb.team_units().iter().any(|u| {
            u.health() > 0.0
                && u.is_being_built()
                && u.def().is_some_and(|d| {
                    d.speed() > 0.0 && d.build_options().iter().any(|d| d.name() == "staticmex") && reaches(d, u.pos())
                })
        }) {
            return true;
        }
        let mut best = None;
        for (&factory, f) in &self.fac.factories {
            if f.unit.health() <= 0.0 || f.unit.is_being_built() {
                continue;
            }
            let Some(fd) = f.unit.def() else {
                continue;
            };
            let rate = fd.build_speed().min(self.eco.effective_income).max(1.0);
            let queued: f32 = f
                .unit
                .current_commands()
                .iter()
                .filter_map(|c| (c.id() < 0).then(|| springai::UnitDef::from_id(self.cb, -c.id())).flatten())
                .map(|d| d.cost(self.metal))
                .sum();
            for def in fd
                .build_options()
                .into_iter()
                .filter(|d| d.speed() > 0.0 && d.build_options().iter().any(|d| d.name() == "staticmex"))
            {
                if !reaches(def, f.get_pos()) {
                    continue;
                }
                let cost = def.cost(self.metal);
                let travel = distance(f.get_pos(), o.center) / def.speed().max(1.0) + (queued + cost) / rate;
                let score = value(o.income, o.spots.len(), travel, def.build_speed(), cost);
                if score > 0.0 && best.as_ref().is_none_or(|(_, _, v)| score > *v) {
                    best = Some((factory, def, score));
                }
            }
        }
        let Some((factory, def, _)) = best else {
            return false;
        };
        self.eco.unified.builder = Some(Builder { factory, def, spots: o.spots.clone() });
        true
    }

    pub(super) fn eco_expansion_constructor_order(&self, factory: i32) -> Option<springai::UnitDef> {
        if !self.eco_unified() || self.eco.hard_estalled {
            return None;
        }
        let b = self.eco.unified.builder.as_ref()?;
        if factory != b.factory || self.eco.unified.ordered.is_some_and(|(_, f)| self.frame - f < 90) {
            return None;
        }
        let spots: Vec<_> = b
            .spots
            .iter()
            .filter_map(|&k| self.graph.metal_spots.get(k))
            .filter(|s| !s.owned && !s.allied && !s.hostile)
            .collect();
        if spots.is_empty() {
            return None;
        }
        if self.cb.team_units().iter().any(|u| {
            u.health() > 0.0
                && u.def().is_some_and(|d| {
                    d.speed() > 0.0
                        && !self.defs.is_commander(d)
                        && d.build_options().iter().any(|d| d.name() == "staticmex")
                        && spots.iter().all(|s| self.terrain_reaches(d, u.pos(), s.position, d.build_distance() + 32.0))
                })
        }) {
            return None;
        }
        let f = self.fac.factories.get(&factory)?;
        (f.unit.health() > 0.0 && f.unit.def()?.build_options().contains(&b.def)).then_some(b.def)
    }

    pub(super) fn eco_expansion_constructor_ordered(&mut self, def: springai::UnitDef) {
        if self.eco.unified.builder.as_ref().is_some_and(|b| b.def == def) {
            self.eco.unified.ordered = Some((def, self.frame));
        }
    }

    fn unified_capture_weight(&self, _spots: &[usize]) -> f32 {
        1.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn nearby_income_beats_a_long_opening_walk_and_expired_income_never_pays() {
        assert!(value(4.0, 2, 2.0, 5.0, 0.0) > value(6.0, 3, 100.0, 5.0, 0.0));
        assert!(value(6.0, 3, 500.0, 5.0, 0.0) < 0.0);
        assert!(value(6.0, 3, 30.0, 5.0, 150.0) < value(6.0, 3, 30.0, 5.0, 0.0));
    }
    #[test]
    fn ordinary_commander_expansion_stops_before_the_middle() {
        let (home, enemy) = (Vec3::xz(100.0, 100.0), Vec3::xz(8100.0, 100.0));
        assert!(our_half(Vec3::xz(3500.0, 100.0), home, enemy));
        assert!(!our_half(Vec3::xz(4100.0, 100.0), home, enemy));
        assert!(!our_half(Vec3::xz(6000.0, 100.0), home, enemy));
    }
    #[test]
    fn switch_precedence_and_rollback_remove_all_direction_bias() {
        let mut ai = super::super::commander::tests::test_ai_with(|_| {});
        ai.set_feature_off("unifiedconquest", false);
        ai.set_feature_off("lanes", false);
        assert!(ai.eco_unified());
        assert!(!ai.eco_lanes());
        ai.set_feature_off("unifiedconquest", true);
        assert!(ai.eco_lanes());
        ai.set_feature_off("lanes", true);
        assert!(!ai.eco_path_policy());
        assert!(!ai.eco_needs_industrious());
        assert!(ai.eco_careful_energy(99));
    }
    fn mission_ai() -> Ai {
        mission_ai_with(|_| {})
    }

    fn mission_ai_with(customize: impl FnOnce(&mut springai::ffi::SSkirmishAICallback)) -> Ai {
        unsafe extern "C" fn scalar(_: i32, _: i32) -> f32 {
            1000.0
        }
        unsafe extern "C" fn speed(_: i32, _: i32) -> f32 {
            60.0
        }
        unsafe extern "C" fn reach(_: i32, _: i32) -> f32 {
            128.0
        }
        unsafe extern "C" fn bp(_: i32, _: i32) -> f32 {
            5.0
        }
        unsafe extern "C" fn zero(_: i32, _: i32) -> i32 {
            0
        }
        unsafe extern "C" fn team(_: i32, _: i32) -> i32 {
            2
        }
        unsafe extern "C" fn fly(_: i32, _: i32) -> bool {
            true
        }
        unsafe extern "C" fn moves(_: i32, _: i32) -> bool {
            false
        }
        unsafe extern "C" fn rules(_: i32, _: i32, _: *const std::ffi::c_char, default: f32) -> f32 {
            default
        }
        let mut ai = super::super::commander::tests::test_ai_with(|t| {
            t.Unit_getHealth = Some(scalar);
            t.Unit_getMaxHealth = Some(scalar);
            t.Unit_getTeam = Some(team);
            t.Unit_getWeapons = Some(zero);
            t.UnitDef_getWeaponMounts = Some(zero);
            t.UnitDef_getSpeed = Some(speed);
            t.UnitDef_getBuildDistance = Some(reach);
            t.UnitDef_getBuildSpeed = Some(bp);
            t.UnitDef_isAbleToFly = Some(fly);
            t.UnitDef_isMoveDataAvailable = Some(moves);
            t.Unit_getRulesParamFloat = Some(rules);
            t.Unit_getCurrentCommands = Some(zero);
            customize(t);
        });
        ai.set_feature_off("unifiedconquest", false);
        ai.set_feature_off("conquest", true);
        ai.set_feature_off("compush", true);
        ai.offset = 0;
        ai.graph.set_start_pos(Vec3::xz(1000.0, 0.0));
        for id in [10, 11] {
            let mut w = Worker::new(springai::Unit::from_id(ai.cb, id).unwrap(), ai.team_id);
            w.is_greedy = true;
            ai.eco.workers.insert(id, w);
        }
        ai.fac.factories.insert(
            50,
            super::super::Factory::new(springai::Unit::from_id(ai.cb, 50).unwrap(), "factorycloak", false, 1000.0),
        );
        for (i, x) in [(0, 1150.0), (1, 2100.0)] {
            ai.graph.metal_spots.push(crate::graph::MetalSpot::new(i, x, 0.0, 0.0, 2.0));
        }
        ai
    }

    #[test]
    fn builders_keep_distinct_catchments_and_release_completed_ones() {
        let mut ai = mission_ai();
        ai.eco_unified_update(0);
        assert_eq!(ai.eco.unified.missions.len(), 2);
        let first = ai.eco.unified.missions[&10].spots.clone();
        let second = ai.eco.unified.missions[&11].spots.clone();
        assert!(first.iter().all(|k| !second.contains(k)));
        ai.frame = PERIOD;
        ai.eco_unified_update(PERIOD);
        assert_eq!(ai.eco.unified.missions[&10].spots, first);
        for k in first {
            ai.graph.metal_spots[k].owned = true;
        }
        ai.frame += PERIOD;
        ai.eco_unified_update(ai.frame);
        assert!(!ai.eco.unified.missions.contains_key(&10));
        assert_eq!(ai.eco.unified.missions[&11].spots, second);
    }

    #[test]
    fn team_conquest_keeps_commander_ownership_and_switches_clear_old_missions() {
        let mut ai = mission_ai();
        ai.eco.workers.get_mut(&10).unwrap().is_com = true;
        ai.eco_unified_update(0);
        assert!(ai.eco.unified.missions.contains_key(&10));
        ai.set_feature_off("conquest", false);
        ai.frame = PERIOD;
        ai.eco_unified_update(ai.frame);
        assert!(!ai.eco.unified.missions.contains_key(&10));
        ai.eco.workers.get_mut(&11).unwrap().is_industrious = true;
        ai.set_feature_off("unifiedconquest", true);
        ai.set_feature_off("lanes", true);
        ai.frame += PERIOD;
        ai.eco_unified_update(ai.frame);
        assert!(ai.eco.unified.missions.is_empty());
        assert!(!ai.eco.workers[&11].is_industrious);
        assert!(!ai.eco_path_policy());
    }

    #[test]
    fn legacy_colonization_requests_resume_only_when_unified_is_off() {
        let mut ai = mission_ai();
        ai.eco.workers.clear();
        ai.eco.colony_wants_air = Some(0);
        assert!(!ai.eco_wants_air_colonist());
        ai.eco.unified.want_air = Some(0);
        assert!(ai.eco_wants_air_colonist());
        ai.set_feature_off("unifiedconquest", true);
        assert!(ai.eco_wants_air_colonist());
    }
    #[test]
    fn unreachable_coast_orders_one_compatible_builder_and_cancels_when_claimed() {
        unsafe extern "C" fn options(_: i32, id: i32, out: *mut i32, max: i32) -> i32 {
            let values: &[i32] = match id {
                50 => &[2],
                1 | 2 => &[3],
                _ => &[],
            };
            if !out.is_null() {
                unsafe { std::ptr::copy_nonoverlapping(values.as_ptr(), out, values.len().min(max.max(0) as usize)) };
            }
            values.len() as i32
        }
        unsafe extern "C" fn name(_: i32, id: i32) -> *const std::ffi::c_char {
            match id {
                2 => c"hovercon",
                3 => c"staticmex",
                50 => c"factoryhover",
                _ => c"cloakcon",
            }
            .as_ptr()
        }
        unsafe extern "C" fn def(_: i32, id: i32) -> i32 {
            if id == 50 { 50 } else { 1 }
        }
        unsafe extern "C" fn flies(_: i32, _: i32) -> bool {
            false
        }
        unsafe extern "C" fn moves(_: i32, _: i32) -> bool {
            true
        }
        unsafe extern "C" fn depth(_: i32, _: i32) -> f32 {
            0.0
        }
        unsafe extern "C" fn slope(_: i32, _: i32) -> f32 {
            0.2
        }
        unsafe extern "C" fn class(_: i32, id: i32) -> i32 {
            if id == 2 { 2 } else { 1 }
        }
        unsafe extern "C" fn cost(_: i32, _: i32, _: i32) -> f32 {
            150.0
        }
        unsafe extern "C" fn units(_: i32, _: *mut i32, _: i32) -> i32 {
            0
        }
        unsafe extern "C" fn pos(_: i32, _: i32, out: *mut f32) {
            unsafe { std::ptr::copy_nonoverlapping([128.0, 0.0, 256.0].as_ptr(), out, 3) };
        }
        let mut ai = mission_ai_with(|t| {
            t.UnitDef_getBuildOptions = Some(options);
            t.UnitDef_getName = Some(name);
            t.Unit_getDef = Some(def);
            t.Unit_getPos = Some(pos);
            t.UnitDef_isAbleToFly = Some(flies);
            t.UnitDef_isMoveDataAvailable = Some(moves);
            t.UnitDef_getMinWaterDepth = Some(depth);
            t.UnitDef_getMaxWaterDepth = Some(depth);
            t.UnitDef_MoveData_getMaxSlope = Some(slope);
            t.UnitDef_MoveData_getSpeedModClass = Some(class);
            t.UnitDef_getCost = Some(cost);
            t.getTeamUnits = Some(units);
        });
        let data = zk_terrain::SpringMapData {
            map_width: 128,
            map_height: 64,
            slope_width: 64,
            slope_height: 32,
            slope_map: vec![0.0; 64 * 32],
            height_map: (0..128 * 64).map(|i| if (32..96).contains(&(i % 128)) { -100.0 } else { 20.0 }).collect(),
        };
        use zk_terrain::Tier;
        let terrain = zk_terrain::Terrain::build_with(
            &data,
            &[(Tier::Bot, Tier::Bot.zero_k_class()), (Tier::Hover, Tier::Hover.zero_k_class())],
        );
        ai.terrain = crate::terrain::TerrainModel::Ready { terrain, rebuild: None, split: 0 };
        ai.graph.metal_spots.clear();
        for i in 0..3 {
            ai.graph.metal_spots.push(crate::graph::MetalSpot::new(i, 896.0, 0.0, 128.0 + i as f32 * 64.0, 2.0));
        }
        ai.eco.effective_income = 20.0;
        let o = Objective { spots: vec![0, 1, 2], center: Vec3::xz(896.0, 256.0), income: 6.0 };
        assert!(ai.unified_request_builder(&o));
        let def = ai.eco_expansion_constructor_order(50).expect("the land builders cannot reach this coast");
        assert_eq!(def.id(), 2);
        assert!(ai.eco_expansion_constructor_order(99).is_none());
        ai.eco_expansion_constructor_ordered(def);
        assert!(ai.eco_expansion_constructor_order(50).is_none(), "accepted order bridges the nanoframe callback gap");
        ai.frame = 91;
        assert!(ai.eco_expansion_constructor_order(50).is_some(), "a canceled order may retry");
        for s in &mut ai.graph.metal_spots {
            s.owned = true;
        }
        assert!(ai.eco_expansion_constructor_order(50).is_none());
    }
    #[test]
    fn catchment_bonus_preserves_nearest_mex_order() {
        let mut ai = mission_ai();
        ai.graph.metal_spots[1].position = Vec3::xz(1400.0, 0.0);
        ai.eco.build_ids.mex_id = 1;
        ai.eco_unified_update(0);
        let w = &ai.eco.workers[&10];
        let def = springai::UnitDef::from_id(ai.cb, 1).unwrap();
        let near = WorkerTask::construction(def, Vec3::xz(1150.0, 0.0), 0);
        let far = WorkerTask::construction(def, Vec3::xz(1400.0, 0.0), 0);
        let price = |t: &WorkerTask| distance(w.get_pos(), t.get_pos()) / 4.0 + ai.eco_unified_job_cost(w, t);
        assert!(price(&near) < price(&far), "reserving a catchment must not erase travel between its mexes");
    }
    #[test]
    fn finished_strategic_objectives_do_not_pull_the_commander_back() {
        let mut ai = mission_ai();
        ai.eco.workers.get_mut(&10).unwrap().is_com = true;
        ai.eco.commanders.push(10);
        ai.eco_unified_update(0);
        ai.eco.unified.destinations.insert(10, Vec3::xz(2200.0, 0.0));
        ai.frame = PERIOD;
        ai.eco_unified_update(ai.frame);
        assert!(ai.eco.unified.destinations.contains_key(&10));
        ai.graph.metal_spots[1].owned = true;
        ai.frame += PERIOD;
        ai.eco_unified_update(ai.frame);
        assert!(!ai.eco.unified.destinations.contains_key(&10));
    }
}
