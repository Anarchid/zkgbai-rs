//! Dedicated assassination waves. Only Halberds, Dirtbags and Fleas qualify.
//! Halberds are borrowed from normal assault squads and returned when jobs end.
//! Kill probability, not winning the escort fight, is the launch criterion.

use crate::Ai;
use crate::defs::cmd;
use crate::economy::factory::FacKind;
use crate::kgbutil::kgb_move_to;
use crate::military::sweeps::SNIPER_MEMORY;
use crate::military::{Enemy, Fighter};
use crate::terrain::reaches;
use crate::util::{NULLPOS, distance};
use springai::{NO_TIMEOUT, Unit, Vec3};
use std::collections::{BTreeMap, BTreeSet};

mod prediction;
mod scenario;

const PERIOD: i32 = 30;
const JOB_MEMORY: i32 = 30 * 180;
const COOLDOWN: i32 = 30 * 90;
const HALBERD_REQUISITION_PERIOD: i32 = 30 * 30;
const DIVE_TIMEOUT: i32 = 30 * 45;
const MAX_WAVE: usize = 128;
const SPREAD: f32 = 105.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Tactic {
    Halberd,
    Dirtbag,
    Flea,
}
impl Tactic {
    fn name(self) -> &'static str {
        match self {
            Self::Halberd => "hoverassault",
            Self::Dirtbag => "shieldscout",
            Self::Flea => "spiderscout",
        }
    }
    fn factory(self) -> FacKind {
        match self {
            Self::Halberd => FacKind::Hover,
            Self::Dirtbag => FacKind::Shield,
            Self::Flea => FacKind::Spider,
        }
    }
    fn size(self, targets: usize, value: f32, cost: f32) -> usize {
        match self {
            Self::Halberd => (5 * targets).clamp(5, 10),
            Self::Dirtbag => (value / cost.max(1.0)).ceil() as usize,
            Self::Flea => (15 * targets).max(20),
        }
    }
}

#[derive(Clone, Debug)]
struct Job {
    id: Option<i32>,
    pos: Vec3,
    last: i32,
    count: usize,
    value: f32,
    name: String,
}
impl Job {
    fn phantom(&self) -> bool {
        self.name == "cloaksnipe"
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Phase {
    Forming,
    Diving,
    Returning,
}
struct Wave {
    tactic: Tactic,
    job: Job,
    needed: usize,
    members: Vec<i32>,
    phase: Phase,
    since: i32,
    home: Vec3,
    query: Option<(u64, i32)>,
}

#[derive(Default)]
pub struct Strikes {
    wave: Option<Wave>,
    members: BTreeSet<i32>,
    /// Factory id -> (type, issue frame, nanoframe id).
    orders: BTreeMap<i32, (&'static str, i32, Option<i32>)>,
    cooldowns: Vec<(Vec3, i32)>,
    predictor: Option<prediction::Predictor>,
    model_failed: bool,
    killed: BTreeSet<i32>,
    serial: u64,
    last_halberd_order: Option<i32>,
    /// Firewalker forced ground salvos: position, end, next permitted burst.
    firewalkers: BTreeMap<i32, (Vec3, i32, i32)>,
    badgers: BTreeSet<i32>,
}

fn valuable(e: &Enemy) -> bool {
    e.identified
        && !e.is_dead
        && !e.can_fly()
        && !e.is_static
        && (e.is_com
            || matches!(e.def_name.as_str(), "cloaksnipe" | "hoverarty" | "jumparty" | "striderarty" | "tankheavyarty"))
}

/// Keep a modest assault force available while Phantoms remain implied,
/// independently of whether there is a precise, launchable job right now.
fn halberd_requisition_due(frame: i32, last: Option<i32>, phantoms: usize, have: usize) -> bool {
    phantoms > 0
        && have < phantoms.saturating_mul(5).clamp(5, 10)
        && last.is_none_or(|at| frame - at >= HALBERD_REQUISITION_PERIOD)
}

fn formation_radius(n: usize) -> f32 {
    (((n as f32).sqrt().ceil() - 1.0) * SPREAD * std::f32::consts::FRAC_1_SQRT_2 + 150.0).max(450.0)
}

fn launchable(probability: f32) -> bool {
    // Spending the whole wave is explicitly acceptable. Do not require a
    // favorable whole-army outcome, or charge the survivors' value twice.
    probability.is_finite() && probability >= 0.55
}

impl Ai {
    pub(crate) fn strikes_status(&self) -> serde_json::Value {
        let jobs: Vec<_> = self
            .strike_jobs(JOB_MEMORY)
            .iter()
            .map(|j| {
                serde_json::json!({
                    "target": j.id, "def": j.name, "pos": [j.pos.x, j.pos.z], "count": j.count,
                    "value": j.value, "age_frames": self.frame - j.last, "cooldown": self.strike_cooling(j.pos),
                    "fresh": self.frame - j.last < SNIPER_MEMORY,
                })
            })
            .collect();
        let wave = self.strikes.wave.as_ref().map(|w| {
            serde_json::json!({
                "tactic": w.tactic.name(), "phase": format!("{:?}", w.phase).to_lowercase(),
                "members": w.members, "needed": w.needed, "target": w.job.id,
            })
        });
        serde_json::json!({ "enabled": !self.dev_off("strikes"), "jobs": jobs, "wave": wave })
    }

    /// Merge seen Phantoms with impact estimates by max, not addition: a
    /// firing Phantom may be observed and inferred at the same time.
    fn strike_jobs(&self, age: i32) -> Vec<Job> {
        let phantom_cost = self.defs.get("cloaksnipe").map_or(750.0, |d| d.cost(self.metal));
        let mut jobs: Vec<Job> = self
            .phantom_fixes(age)
            .into_iter()
            .map(|f| Job {
                id: None,
                pos: f.pos,
                last: f.last,
                count: f.count,
                value: f.count as f32 * phantom_cost,
                name: "cloaksnipe".into(),
            })
            .collect();
        for e in
            self.war.targets.values().filter(|e| valuable(e) && self.frame - e.last_seen < age && e.position != NULLPOS)
        {
            if e.def_name == "cloaksnipe" {
                if let Some(j) = jobs.iter_mut().find(|j| j.phantom() && distance(j.pos, e.position) < 400.0) {
                    if e.visible {
                        j.id = Some(e.unit_id);
                        j.pos = e.position;
                    }
                    j.last = j.last.max(e.last_seen);
                    let seen = self
                        .war
                        .targets
                        .values()
                        .filter(|other| {
                            !other.is_dead
                                && other.def_name == "cloaksnipe"
                                && self.frame - other.last_seen < age
                                && distance(other.position, j.pos) < 400.0
                        })
                        .count();
                    j.count = j.count.max(seen);
                    j.value = j.count as f32 * phantom_cost;
                    continue;
                }
            }
            jobs.push(Job {
                id: Some(e.unit_id),
                pos: e.position,
                last: e.last_seen,
                count: 1,
                value: e.ud.map_or(e.value, |d| d.cost(self.metal)),
                name: e.def_name.clone(),
            });
        }
        jobs.sort_by(|a, b| b.value.total_cmp(&a.value));
        jobs
    }

    pub(crate) fn strikes_phantom_targets(&self) -> Vec<(Vec3, usize)> {
        self.strike_jobs(SNIPER_MEMORY).into_iter().filter(|j| j.phantom()).map(|j| (j.pos, j.count)).collect()
    }

    pub(crate) fn strikes_order(&self, kind: FacKind, factory: i32) -> Option<&'static str> {
        if self.dev_off("strikes") {
            return None;
        }
        if self.strikes.orders.get(&factory).is_some_and(|(_, f, _)| self.frame - *f < JOB_MEMORY) {
            return None;
        }
        if let Some(w) = self.strikes.wave.as_ref().filter(|w| w.phase == Phase::Forming && w.tactic.factory() == kind)
        {
            let outstanding = self
                .strikes
                .orders
                .values()
                .filter(|(n, f, _)| *n == w.tactic.name() && self.frame - *f < JOB_MEMORY)
                .count();
            if w.members.len() + outstanding < w.needed {
                return Some(w.tactic.name());
            }
        }
        if kind != FacKind::Hover {
            return None;
        }
        let phantoms = self.strike_jobs(JOB_MEMORY).iter().filter(|j| j.phantom()).map(|j| j.count).sum();
        let def = self.defs.get("hoverassault")?;
        // Count ordinary squads, strike members and nanoframes together. Only
        // commands not yet attached to a nanoframe need a separate reservation.
        let have = self
            .cb
            .friendly_units()
            .iter()
            .filter(|u| u.team() == self.team_id && u.health() > 0.0 && u.def().is_some_and(|d| d.id() == def.id()))
            .count()
            + self
                .strikes
                .orders
                .values()
                .filter(|(n, f, id)| *n == "hoverassault" && self.frame - *f < JOB_MEMORY && id.is_none())
                .count();
        halberd_requisition_due(self.frame, self.strikes.last_halberd_order, phantoms, have).then_some("hoverassault")
    }

    pub(crate) fn strikes_ordered(&mut self, factory: i32, name: &'static str) {
        self.strikes.orders.insert(factory, (name, self.frame, None));
        if name == "hoverassault" {
            self.strikes.last_halberd_order = Some(self.frame);
        }
    }

    pub(crate) fn strikes_claims(&mut self, unit: Unit) -> bool {
        if self.strikes.members.contains(&unit.id()) {
            return true;
        }
        if springai::reserved::is_reserved(self.cb, unit.id()) {
            return false;
        }
        let name = self.defs.name_of(unit);
        if let Some((&factory, _)) = self.strikes.orders.iter().find(|(_, (_, _, id))| *id == Some(unit.id())) {
            self.strikes.orders.remove(&factory);
        }
        let Some(w) = self.strikes.wave.as_ref() else { return false };
        if self.dev_off("strikes")
            || w.phase != Phase::Forming
            || name != w.tactic.name()
            || w.members.len() >= w.needed
        {
            return false;
        }
        let home = w.home;
        if !unit.def().is_some_and(|d| reaches(self.reach_terrain(), d, unit.pos(), home, 100.0)) {
            return false;
        }
        self.strikes_adopt(unit);
        true
    }

    pub(crate) fn strikes_created(&mut self, unit: Unit, builder: Option<Unit>) {
        if let Some(order) = builder.and_then(|b| self.strikes.orders.get_mut(&b.id()))
            && self.defs.name_of(unit) == order.0
        {
            order.2 = Some(unit.id());
        }
    }

    pub(crate) fn strikes_enemy_destroyed(&mut self, id: i32) {
        if let Some(e) = self.war.targets.get(&id).filter(|e| e.def_name == "cloaksnipe") {
            self.sweeps_phantom_destroyed(e.position);
        }
        if self.strikes.wave.as_ref().is_some_and(|w| w.job.id == Some(id)) {
            self.strikes.killed.insert(id);
        }
    }

    fn strikes_adopt(&mut self, unit: Unit) {
        self.raider_remove_unit(unit);
        self.squad_remove_unit(unit);
        self.retreat_remove_unit(unit);
        self.war.new_units.remove(&unit.id());
        unit.execute_custom_command(cmd::UNIT_CANCEL_TARGET, &[], 0, NO_TIMEOUT);
        unit.set_fire_state(0, 0);
        unit.set_move_state(0, 0);
        let cost = unit.def().map_or(0.0, |d| d.cost(self.metal));
        self.fighters.insert(unit.id(), Fighter::new(unit, cost, self.team_id));
        self.strikes.members.insert(unit.id());
        if let Some(w) = self.strikes.wave.as_mut() {
            w.members.push(unit.id());
        }
    }

    pub(crate) fn strikes_remove(&mut self, unit: Unit) {
        if self.strikes.members.remove(&unit.id()) {
            self.fighters.remove(&unit.id());
        }
        if let Some(w) = self.strikes.wave.as_mut() {
            w.members.retain(|id| *id != unit.id());
        }
        self.strikes.orders.retain(|factory, (_, _, built)| *factory != unit.id() && *built != Some(unit.id()));
        self.strikes.firewalkers.remove(&unit.id());
        self.strikes.badgers.remove(&unit.id());
    }

    fn strikes_release(&mut self, w: Wave) {
        self.log(&format!("strikes: {} survivors of {} return to normal handlers", w.members.len(), w.tactic.name()));
        self.strikes_release_members(w.members);
    }

    fn strikes_release_members(&mut self, members: Vec<i32>) {
        for id in members {
            self.strikes.members.remove(&id);
            if let Some(f) = self.fighters.remove(&id) {
                let u = f.unit;
                if u.health() <= 0.0 || u.team() != self.team_id {
                    continue;
                }
                u.execute_custom_command(cmd::UNIT_CANCEL_TARGET, &[], 0, NO_TIMEOUT);
                u.set_fire_state(2, 0);
                u.set_move_state(1, 0);
                if !self.unit_classes.no_retreat.contains(self.defs.name_of(u)) {
                    self.retreat_add_coward(u);
                }
                self.war.new_units.insert(id);
                self.war_unit_idle(u);
            }
        }
    }

    fn strike_suitable(&self, tactic: Tactic, j: &Job, from: Vec3) -> bool {
        let Some(def) = self.defs.get(tactic.name()) else { return false };
        if self.missiles_ground_burning(j.pos) || !reaches(self.reach_terrain(), def, from, j.pos, 80.0) {
            return false;
        }
        if tactic != Tactic::Flea {
            return true;
        }
        // A lone riot is sporadic cover; overlapping riots or unknown screens
        // are not a reason to feed another twenty Fleas every update.
        self.war
            .targets
            .values()
            .filter(|e| {
                !e.is_dead
                    && self.frame - e.last_seen < JOB_MEMORY
                    && (e.is_riot || !e.identified)
                    && distance(e.position, j.pos) < 600.0
            })
            .count()
            <= 1
    }

    fn strike_cooling(&self, pos: Vec3) -> bool {
        self.strikes.cooldowns.iter().any(|(p, f)| self.frame - *f < COOLDOWN && distance(*p, pos) < 500.0)
    }

    pub fn strikes_update(&mut self, frame: i32) {
        if (frame + self.offset) % PERIOD != 0 {
            return;
        }
        self.strikes_support_fire();
        self.strikes.orders.retain(|_, (_, f, _)| frame - *f < JOB_MEMORY);
        self.strikes.cooldowns.retain(|(_, f)| frame - *f < COOLDOWN);
        if self.dev_off("strikes") {
            if let Some(w) = self.strikes.wave.take() {
                self.strikes_release(w);
            }
            return;
        }
        if self.strikes.wave.is_none() {
            'jobs: for j in self.strike_jobs(JOB_MEMORY) {
                if self.strike_cooling(j.pos) {
                    continue;
                }
                for tactic in [Tactic::Halberd, Tactic::Dirtbag, Tactic::Flea] {
                    let base = self
                        .fac
                        .factories
                        .values()
                        .find(|f| f.kind() == tactic.factory())
                        .map(|f| f.get_pos())
                        .or_else(|| {
                            self.fighters
                                .values()
                                .find(|f| self.defs.name_of(f.unit) == tactic.name())
                                .map(|f| f.get_pos())
                        });
                    let Some(base) = base else { continue };
                    if !self.strike_suitable(tactic, &j, base) {
                        continue;
                    }
                    let cost = self.defs.get(tactic.name()).map_or(1.0, |d| d.cost(self.metal));
                    let needed = tactic.size(j.count, j.value, cost);
                    if needed == 0 || needed > MAX_WAVE {
                        continue;
                    }
                    let mut existing: Vec<Vec3> = self
                        .fighters
                        .values()
                        .filter(|f| {
                            self.defs.name_of(f.unit) == tactic.name()
                                && self.squad_handler.fighters.contains(&f.id)
                                && !self.qrf_claims(f.id)
                                && !self.retreat_handler.is_retreating(f.id)
                        })
                        .map(|f| f.get_pos())
                        .collect();
                    existing.sort_by(|a, b| distance(*a, j.pos).total_cmp(&distance(*b, j.pos)));
                    let home = existing
                        .first()
                        .copied()
                        .filter(|p| self.war.get_effective_threat(*p) <= 0.0)
                        .unwrap_or_else(|| self.graph_get_closest_haven(base));
                    let home = if home == NULLPOS { base } else { home };
                    self.log(&format!(
                        "strikes: assemble {needed} {} for {} (~{} targets)",
                        tactic.name(),
                        j.name,
                        j.count
                    ));
                    self.strikes.wave = Some(Wave {
                        tactic,
                        job: j,
                        needed,
                        members: Vec::new(),
                        phase: Phase::Forming,
                        since: frame,
                        home,
                        query: None,
                    });
                    break 'jobs;
                }
            }
        }
        // Borrow only the named tactic, from ordinary handlers. Never steal QRF,
        // operator-controlled units, scouts or other special forces.
        if let Some(w) = self.strikes.wave.as_ref().filter(|w| w.phase == Phase::Forming) {
            let name = w.tactic.name();
            let need = w.needed.saturating_sub(w.members.len());
            let home = w.home;
            let mut recruits: Vec<Unit> = self
                .fighters
                .values()
                .filter(|f| {
                    self.defs.name_of(f.unit) == name
                        && !self.retreat_handler.is_retreating(f.id)
                        && !self.qrf_claims(f.id)
                        && !f.squad.is_some_and(|sid| {
                            self.op.over.squad_intents.contains_key(&sid) || self.op_squad_target(sid).is_some()
                        })
                        && (self.squad_handler.fighters.contains(&f.id)
                            || self.raider_handler.solo_raiders.contains(&f.id)
                            || self.raider_handler.small_raiders.contains(&f.id))
                })
                .map(|f| f.unit)
                .collect();
            recruits.sort_by(|a, b| distance(a.pos(), home).total_cmp(&distance(b.pos(), home)));
            for u in recruits.into_iter().take(need) {
                self.strikes_claims(u);
            }
        }
        let Some(mut w) = self.strikes.wave.take() else { return };
        w.members
            .retain(|id| self.fighters.get(id).is_some_and(|f| f.unit.health() > 0.0 && f.unit.team() == self.team_id));
        if w.phase == Phase::Forming {
            let fresh = self.strike_jobs(JOB_MEMORY).into_iter().find(|j| {
                (w.job.id.is_some() && j.id == w.job.id) || (j.name == w.job.name && distance(j.pos, w.job.pos) < 500.0)
            });
            if let Some(j) = fresh {
                w.job = j;
            } else {
                self.strikes_release(w);
                return;
            }
            if w.job.id.is_some_and(|id| self.strikes.killed.remove(&id))
                || frame - w.job.last >= JOB_MEMORY
                || frame - w.since > JOB_MEMORY * 2
            {
                self.strikes_release(w);
                return;
            }
            let cost = self.defs.get(w.tactic.name()).map_or(1.0, |d| d.cost(self.metal));
            w.needed = w.tactic.size(w.job.count, w.job.value, cost);
            if w.members.len() > w.needed {
                self.strikes_release_members(w.members.split_off(w.needed));
            }
            if w.needed > MAX_WAVE
                || (w.members.len() < w.needed && !self.fac.factories.values().any(|f| f.kind() == w.tactic.factory()))
            {
                self.strikes_release(w);
                return;
            }
            let radius = formation_radius(w.members.len());
            let gathered = w.members.len() >= w.needed
                && w.members
                    .iter()
                    .all(|id| self.fighters.get(id).is_some_and(|f| distance(f.get_pos(), w.home) < radius));
            if gathered && frame - w.job.last < SNIPER_MEMORY && self.strike_suitable(w.tactic, &w.job, w.home) {
                self.strike_predict(&mut w);
            }
        } else if w.phase == Phase::Diving {
            let target = w.job.id.and_then(|id| self.war.targets.get(&id));
            let dead = w.job.id.is_some_and(|id| self.strikes.killed.remove(&id));
            if dead && w.job.phantom() && w.job.count > 1 {
                w.job.count -= 1;
                w.job.id = None; // Sweep for the next Phantom with the surviving wave.
            } else if dead || frame - w.since > DIVE_TIMEOUT || w.members.is_empty() {
                self.log(&format!(
                    "strikes: {} ended ({})",
                    w.tactic.name(),
                    if dead { "target killed" } else { "search expired or wave lost" }
                ));
                w.phase = Phase::Returning;
                w.since = frame;
                self.strikes.cooldowns.push((w.job.pos, frame));
            } else if let Some(e) = target.filter(|e| e.visible || e.is_radar_visible) {
                w.job.pos = e.position;
                w.job.last = frame;
            } else if w.job.phantom() {
                if let Some(e) = self.war.targets.values().find(|e| {
                    !e.is_dead && e.visible && e.def_name == "cloaksnipe" && distance(e.position, w.job.pos) < 750.0
                }) {
                    w.job.id = Some(e.unit_id);
                    w.job.pos = e.position;
                    w.job.last = frame;
                }
            }
        }
        if w.phase == Phase::Returning
            && (w.members.iter().all(|id| self.fighters.get(id).is_none_or(|f| distance(f.get_pos(), w.home) < 450.0))
                || frame - w.since > JOB_MEMORY)
        {
            if let Some(j) = self
                .strike_jobs(SNIPER_MEMORY)
                .into_iter()
                .find(|j| !self.strike_cooling(j.pos) && self.strike_suitable(w.tactic, j, w.home))
            {
                let cost = self.defs.get(w.tactic.name()).map_or(1.0, |d| d.cost(self.metal));
                let needed = w.tactic.size(j.count, j.value, cost);
                if needed <= MAX_WAVE {
                    w.job = j;
                    w.needed = needed;
                    w.phase = Phase::Forming;
                    w.since = frame;
                    w.query = None;
                } else {
                    self.strikes_release(w);
                    return;
                }
            } else {
                self.strikes_release(w);
                return;
            }
        }
        self.strike_command(&w);
        self.strikes.wave = Some(w);
    }

    fn strike_command(&mut self, w: &Wave) {
        let visible =
            w.job.id.and_then(|id| self.war.targets.get(&id)).filter(|e| e.visible && !e.is_dead).map(|e| e.unit);
        let center = if w.phase == Phase::Diving { w.job.pos } else { w.home };
        // Advance together toward the fix. The slowest laggards keep the pack
        // from becoming a trickle, but fallen units never block the survivors.
        let positions: Vec<Vec3> =
            w.members.iter().filter_map(|id| self.fighters.get(id).map(|f| f.get_pos())).collect();
        let n = positions.len().max(1) as f32;
        let mid =
            Vec3::xz(positions.iter().map(|p| p.x).sum::<f32>() / n, positions.iter().map(|p| p.z).sum::<f32>() / n);
        let lagging = positions.iter().any(|p| distance(*p, mid) > formation_radius(w.members.len()));
        let close = distance(mid, center) < 450.0;
        for (i, id) in w.members.iter().enumerate() {
            let Some(f) = self.fighters.get(id) else { continue };
            let u = f.unit;
            if w.phase == Phase::Diving
                && let Some(t) = visible
                && distance(u.pos(), t.pos()) <= u.max_range().max(80.0) * 0.9
            {
                u.set_fire_state(0, 0); // Explicit attack fires only at the assassination target.
                u.attack(t, 0);
                continue;
            }
            u.set_fire_state(0, 0);
            let goal = if w.phase == Phase::Diving && !close {
                if lagging {
                    mid
                } else {
                    crate::util::get_directional_point(mid, center, distance(mid, center).min(350.0))
                }
            } else {
                center
            };
            // 105-elmo spacing, a moving comb across the uncertain fix. This
            // stays inside Phantom decloak distance while avoiding tight clumps.
            let width = (w.members.len() as f32).sqrt().ceil().max(1.0) as usize;
            let angle = ((self.frame - w.since) / 90 % 8) as f32 * std::f32::consts::FRAC_PI_4;
            let searching = w.phase == Phase::Diving && close && visible.is_none();
            let sweep = if searching { (angle.cos() * 200.0, angle.sin() * 200.0) } else { (0.0, 0.0) };
            // Once it is revealed, every member must enter firing range; a
            // permanent wide grid would leave the outside Fleas/Dirtbags idle.
            let spacing = if w.phase == Phase::Diving && visible.is_some() {
                SPREAD.min(u.max_range().max(40.0) / width.max(2) as f32)
            } else {
                SPREAD
            };
            let mut p = Vec3::xz(
                goal.x + (i % width) as f32 * spacing - (width - 1) as f32 * spacing / 2.0 + sweep.0,
                goal.z + (i / width) as f32 * spacing - (width - 1) as f32 * spacing / 2.0 + sweep.1,
            );
            let map = self.cb.map();
            p.x = p.x.clamp(1.0, (map.width() * 8 - 1).max(1) as f32);
            p.z = p.z.clamp(1.0, (map.height() * 8 - 1).max(1) as f32);
            p.y = map.elevation_at(p.x, p.z);
            // RAW_MOVE uses the engine's terrain pathing, with no random
            // dispersion or enemy-avoidance detours that would undo the dive.
            kgb_move_to(u, p, 0, self.frame);
        }
    }

    fn strikes_support_fire(&mut self) {
        let fixes = if self.dev_off("strikes") { Vec::new() } else { self.strikes_phantom_targets() };
        let implied = !self.dev_off("strikes") && self.strike_jobs(JOB_MEMORY).iter().any(Job::phantom);
        let units: Vec<Unit> = self.fighters.values().map(|f| f.unit).collect();
        for u in units {
            match self.defs.name_of(u) {
                "veharty" if implied => {
                    if self.strikes.badgers.insert(u.id()) {
                        self.log(&format!("strikes: Badger {} enables fire towards enemies", u.id()));
                        u.execute_custom_command(cmd::FIRE_TOWARDS_ENEMY, &[1.0], 0, NO_TIMEOUT);
                    }
                }
                "jumparty" => {
                    if let Some(&(p, until, again)) = self.strikes.firewalkers.get(&u.id()) {
                        let fresh = fixes.iter().any(|(fix, _)| distance(*fix, p) < 400.0);
                        let clear = self
                            .cb
                            .friendly_units_in(p, 250.0)
                            .iter()
                            .all(|a| a.def().is_some_and(|d| d.is_able_to_fly()));
                        if self.frame < until && fresh && clear && distance(u.pos(), p) <= u.max_range() {
                            u.execute_custom_command(cmd::UNIT_SET_TARGET, &[p.x, p.y, p.z], 0, until);
                            continue;
                        }
                        u.execute_custom_command(cmd::UNIT_CANCEL_TARGET, &[], 0, NO_TIMEOUT);
                        if self.frame < again {
                            continue;
                        }
                    }
                    if let Some(&(mut p, _)) = fixes
                        .iter()
                        .filter(|(p, _)| distance(u.pos(), *p) <= u.max_range())
                        .filter(|(p, _)| {
                            self.cb
                                .friendly_units_in(*p, 250.0)
                                .iter()
                                .all(|a| a.def().is_some_and(|d| d.is_able_to_fly()))
                        })
                        .min_by(|a, b| distance(u.pos(), a.0).total_cmp(&distance(u.pos(), b.0)))
                    {
                        p.y = self.cb.map().elevation_at(p.x, p.z);
                        let reload = u
                            .def()
                            .into_iter()
                            .flat_map(|d| d.weapon_mounts())
                            .filter_map(|m| m.weapon_def())
                            .map(|w| w.reload())
                            .fold(1.0_f32, f32::max);
                        let until = self.frame + (reload * 30.0 * 3.0) as i32;
                        self.strikes.firewalkers.insert(u.id(), (p, until, until + 30 * 30));
                        self.log(&format!("strikes: Firewalker {} ground salvos at {:.0},{:.0}", u.id(), p.x, p.z));
                        u.execute_custom_command(cmd::UNIT_SET_TARGET, &[p.x, p.y, p.z], 0, until);
                    }
                }
                _ => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn persistent_phantoms_requisition_halberds_periodically_without_a_wave() {
        assert!(halberd_requisition_due(100, None, 1, 0));
        assert!(!halberd_requisition_due(999, Some(100), 1, 1));
        assert!(halberd_requisition_due(1000, Some(100), 1, 1));
        assert!(!halberd_requisition_due(1000, Some(100), 1, 5));
        assert!(halberd_requisition_due(1000, Some(100), 2, 5));
        assert!(!halberd_requisition_due(1000, Some(100), 20, 10));
        assert!(!halberd_requisition_due(1000, Some(100), 0, 0));
        // Losses reopen procurement after the interval; a full surviving
        // force (including construction reservations) prevents endless spam.
        assert!(halberd_requisition_due(2000, Some(1000), 1, 4));
    }

    #[test]
    fn waves_follow_requested_costs_and_counts() {
        assert_eq!(Tactic::Halberd.size(1, 750.0, 210.0), 5);
        assert_eq!(Tactic::Halberd.size(3, 2250.0, 210.0), 10);
        assert_eq!(Tactic::Dirtbag.size(1, 750.0, 30.0), 25);
        assert_eq!(Tactic::Dirtbag.size(2, 1500.0, 30.0), 50);
        assert_eq!(Tactic::Flea.size(1, 750.0, 25.0), 20);
        assert_eq!(Tactic::Flea.size(2, 1500.0, 25.0), 30);
        assert_eq!(Tactic::Flea.size(3, 2250.0, 25.0), 45);
    }
    #[test]
    fn launch_gate_is_target_death_not_wave_survival() {
        assert!(launchable(0.8));
        assert!(!launchable(0.4));
        assert!(!launchable(f32::NAN));
    }
}
