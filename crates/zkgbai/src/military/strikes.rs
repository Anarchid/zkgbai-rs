//! Dedicated assassination waves. Halberds, Dirtbags, Fleas, Darts and Scythes qualify.
//! Halberds are borrowed from normal assault squads and returned when jobs end.
//! Kill probability, not winning the escort fight, is the launch criterion.

use crate::Ai;
use crate::defs::cmd;
use crate::economy::factory::FacKind;
use crate::kgbutil::kgb_move_to;
use crate::military::Fighter;
use crate::military::sweeps::{PHANTOM_FIX_RADIUS, SNIPER_MEMORY, counterbattery::FRESH as FIRE_MEMORY};
use crate::util::{NULLPOS, distance};
use springai::{NO_TIMEOUT, Unit, Vec3};
use std::collections::{BTreeMap, BTreeSet};

mod evasion;
mod fragile;
mod lobsters;
mod navigation;
mod opportunities;
mod prediction;
mod scenario;
mod squads;
mod staging;

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
    Scythe,
    Dirtbag,
    Flea,
    Dart,
}
impl Tactic {
    fn name(self) -> &'static str {
        match self {
            Self::Halberd => "hoverassault",
            Self::Scythe => "cloakheavyraid",
            Self::Dirtbag => "shieldscout",
            Self::Flea => "spiderscout",
            Self::Dart => "vehscout",
        }
    }
    fn light_swarm(self) -> bool {
        matches!(self, Self::Flea | Self::Dart)
    }
    fn fire_state(self) -> i32 {
        if matches!(self, Self::Halberd | Self::Scythe) { 0 } else { 2 }
    }
    fn factory(self) -> FacKind {
        match self {
            Self::Halberd => FacKind::Hover,
            Self::Scythe => FacKind::Cloak,
            Self::Dirtbag => FacKind::Shield,
            Self::Flea => FacKind::Spider,
            Self::Dart => FacKind::Veh,
        }
    }
    fn size(self, targets: usize, value: f32, cost: f32) -> usize {
        match self {
            Self::Halberd => (5 * targets).clamp(5, 10),
            Self::Scythe => 3 * targets,
            Self::Dirtbag => (value / cost.max(1.0)).ceil() as usize,
            Self::Flea => (15 * targets).max(20),
            Self::Dart => 20 * targets.max(1),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Prey {
    GlassCannon,
    Fragile,
    Defenseless,
    Commander,
    Heavy,
}
impl Prey {
    fn needs_stun(self, tactic: Tactic) -> bool {
        tactic.light_swarm() && matches!(self, Self::Commander | Self::Heavy)
    }
    fn can_requisition(self, tactic: Tactic) -> bool {
        self != Self::Commander && self.allows(tactic, true) && !self.needs_stun(tactic)
    }
    fn allows(self, tactic: Tactic, stunned: bool) -> bool {
        match self {
            Self::GlassCannon | Self::Fragile => true,
            Self::Defenseless => tactic.light_swarm(),
            Self::Commander => matches!(tactic, Tactic::Halberd | Tactic::Dirtbag) || (tactic.light_swarm() && stunned),
            Self::Heavy => tactic.light_swarm() && stunned,
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
    prey: Prey,
}
impl Job {
    fn size(&self, tactic: Tactic, cost: f32) -> usize {
        tactic.size(if self.phantom() { self.count } else { 1 }, self.value, cost)
    }
    fn production_pays(&self, tactic: Tactic, cost: f32) -> bool {
        self.prey.can_requisition(tactic)
            && (self.prey != Prey::Defenseless || self.value >= self.size(tactic, cost) as f32 * cost)
    }
    fn matches_death(&self, id: i32, name: &str, pos: Vec3) -> bool {
        self.id == Some(id)
            || (self.phantom()
                && name == "cloaksnipe"
                && (self.id.is_none() || self.count > 1)
                && distance(self.pos, pos) < PHANTOM_FIX_RADIUS)
    }
    fn phantom(&self) -> bool {
        self.name == "cloaksnipe"
    }
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Phase {
    Screening,
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
    lobsters: lobsters::LobsterStrikes,
    members: BTreeSet<i32>,
    /// Factory id -> (type, issue frame, nanoframe id).
    orders: BTreeMap<i32, (&'static str, i32, Option<i32>)>,
    cooldowns: Vec<(Vec3, i32)>,
    predictor: Option<prediction::Predictor>,
    model_failed: bool,
    missions: BTreeMap<crate::military::wrappers::SquadId, squads::Mission>,
    squad_retry: BTreeMap<crate::military::wrappers::SquadId, i32>,
    squad_predictor: Option<prediction::Predictor>,
    killed: BTreeSet<i32>,
    serial: u64,
    last_halberd_order: Option<i32>,
    /// Firewalker forced ground salvos: position, end, next permitted burst.
    firewalkers: BTreeMap<i32, (Vec3, i32, i32)>,
    badgers: BTreeSet<i32>,
    hits: Vec<(Vec3, i32)>,
    evasion: BTreeMap<i32, (Vec3, i32)>,
}

fn glass_cannon(name: &str) -> bool {
    matches!(name, "cloaksnipe" | "hoverarty" | "jumparty" | "striderarty" | "tankheavyarty" | "tankarty")
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
                    "flea_requires_stun": j.prey.needs_stun(Tactic::Flea),
                    "fragile": j.prey == Prey::Fragile,
                    "stun_frames": self.strike_stun(j).map(|(_, frames)| frames),
                })
            })
            .collect();
        let wave = self.strikes.wave.as_ref().map(|w| {
            serde_json::json!({
                "tactic": w.tactic.name(), "phase": format!("{:?}", w.phase).to_lowercase(),
                "members": w.members, "needed": w.needed, "target": w.job.id,
                "staging": [w.home.x, w.home.z], "job_pos": [w.job.pos.x, w.job.pos.z],
                "job_age_frames": self.frame - w.job.last,
                "gathered": w.members.iter().filter(|id| self.fighters.get(id).is_some_and(|f|
                    distance(f.get_pos(), w.home) < staging::muster_radius(w.tactic, w.members.len()))).count(),
                "prediction_pending": w.query.is_some(),
                "evading": w.members.iter().filter(|id| self.strike_evasion_goal(**id).is_some()).count(),
            })
        });
        serde_json::json!({ "enabled": !self.dev_off("strikes"), "fragile_enabled": !self.dev_off("strikefragile"), "jobs": jobs, "wave": wave, "squad_missions": self.squad_strikes_status(), "raider_missions": self.squad_strikes_status(), "lobster_missions": self.lobster_strikes_status(), "counterbattery": self.counterbattery_status() })
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
                prey: Prey::GlassCannon,
            })
            .collect();
        for e in self.war.targets.values().filter(|e| {
            e.identified && !e.is_dead && !e.can_fly() && self.frame - e.last_seen < age && e.position != NULLPOS
        }) {
            let prey = if e.is_com {
                Prey::Commander
            } else if glass_cannon(&e.def_name) {
                Prey::GlassCannon
            } else if self.strike_fragile(e) {
                Prey::Fragile
            } else if e.ud.is_some_and(|d| self.strike_defenseless(d)) {
                Prey::Defenseless
            } else if super::jaeger::is_heavy(e, self.metal)
                && ((e.visible && e.unit.is_paralyzed()) || self.jaeger_stun_expected(e.unit_id))
            {
                Prey::Heavy
            } else {
                continue;
            };
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
                prey,
            });
        }
        // Weapon identity can reveal artillery without a LOS contact. Keep
        // very broad single-bearing estimates for scouting/counterbattery only.
        for fix in self.counterbattery_fixes(age.min(FIRE_MEMORY)) {
            let Some(name) = fix.def.as_deref().filter(|name| glass_cannon(name) && *name != "cloaksnipe") else {
                continue;
            };
            if fix.uncertainty > PHANTOM_FIX_RADIUS
                || jobs.iter().any(|j| j.name == name && distance(j.pos, fix.pos) < PHANTOM_FIX_RADIUS)
            {
                continue;
            }
            let Some(def) = self.defs.get(name) else { continue };
            jobs.push(Job {
                id: fix.source.filter(|id| self.war.targets.contains_key(id)),
                pos: fix.pos,
                last: fix.last,
                count: 1,
                value: def.cost(self.metal),
                name: name.into(),
                prey: Prey::GlassCannon,
            });
        }
        // "Defenseless" includes the surroundings, not just the victim's
        // weapons. Defended economy cannot fund opportunistic procurement.
        let threats = self.strike_opportunity_threats();
        jobs.retain(|j| j.prey != Prey::Defenseless || opportunities::clear(j.pos, j.pos, &threats));
        // Value AA formations and other defenseless clusters together; do not
        // build a whole wave for a lone cheap mex. Existing waves can still raid it.
        let soft: Vec<_> = jobs.iter().filter(|j| j.prey == Prey::Defenseless).map(|j| (j.pos, j.value)).collect();
        for j in jobs.iter_mut().filter(|j| j.prey == Prey::Defenseless) {
            let nearby: Vec<_> = soft
                .iter()
                .filter(|(p, _)| distance(*p, j.pos) < 600.0 && opportunities::clear(j.pos, *p, &threats))
                .collect();
            j.count = nearby.len();
            j.value = nearby.iter().map(|(_, value)| value).sum();
        }
        jobs.sort_by(|a, b| {
            let pounce = |j: &Job| j.prey.needs_stun(Tactic::Flea) && self.strike_stun(j).is_some();
            (self.frame - b.last < SNIPER_MEMORY)
                .cmp(&(self.frame - a.last < SNIPER_MEMORY))
                .then_with(|| pounce(b).cmp(&pounce(a)))
                .then_with(|| b.value.total_cmp(&a.value))
        });
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
        if let Some(w) = self.strikes.wave.as_ref().filter(|w| {
            matches!(w.phase, Phase::Forming | Phase::Screening)
                && w.tactic.factory() == kind
                && self.strike_preparable(w.tactic, &w.job)
                && self.strike_tactic_reaches(w.tactic, w.home, w.job.pos)
                && (w.job.prey != Prey::Defenseless || self.strike_opportunity_clear(w.home, w.job.pos))
                && w.job.production_pays(
                    w.tactic,
                    self.defs.get(w.tactic.name()).map_or(f32::INFINITY, |d| d.cost(self.metal)),
                )
        }) {
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
        let order =
            self.strikes.orders.iter().find(|(_, (_, _, id))| *id == Some(unit.id())).map(|(&factory, _)| factory);
        if name == Tactic::Scythe.name()
            && order.is_none()
            && self.scouts_adopt_scythe()
            && !self.raider_handler.medium_raiders.contains(&unit.id())
        {
            // Preserve the pending dedicated scout; ordinary raider squads
            // and units explicitly produced for this strike remain eligible.
            return false;
        }
        if let Some(factory) = order {
            self.strikes.orders.remove(&factory);
        }
        let Some(w) = self.strikes.wave.as_ref() else { return false };
        if self.dev_off("strikes")
            || !matches!(w.phase, Phase::Forming | Phase::Screening)
            || name != w.tactic.name()
            || w.members.len() >= w.needed
            || !self.strike_preparable(w.tactic, &w.job)
            || (w.job.prey == Prey::Defenseless && !self.strike_opportunity_clear(w.home, w.job.pos))
        {
            return false;
        }
        let home = w.home;
        if !unit.def().is_some_and(|d| self.strike_point_reachable(d, unit.pos(), home)) {
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
        if let Some(e) = self.war.targets.get(&id)
            && let Some(w) = self.strikes.wave.as_mut()
            && w.job.matches_death(id, &e.def_name, e.position)
        {
            // An inferred fix may be revealed and killed by an ally between
            // strike ticks, before the wave ever acquires its unit id.
            w.job.id.get_or_insert(id);
            self.strikes.killed.insert(id);
        }
    }

    fn strikes_adopt(&mut self, unit: Unit) {
        self.raider_remove_unit(unit);
        self.squad_remove_unit(unit);
        self.retreat_remove_unit(unit);
        self.war.new_units.remove(&unit.id());
        unit.execute_custom_command(cmd::UNIT_CANCEL_TARGET, &[], 0, NO_TIMEOUT);
        unit.set_fire_state(self.strikes.wave.as_ref().map_or(2, |w| w.tactic.fire_state()), 0);
        unit.set_move_state(0, 0);
        if self.strikes.wave.as_ref().is_some_and(|w| w.tactic == Tactic::Scythe) {
            unit.cloak(true, 0);
        }
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
        self.strikes.evasion.remove(&unit.id());
    }

    fn strikes_release(&mut self, w: Wave) {
        self.strikes.killed.clear();
        self.log(&format!("strikes: {} survivors of {} return to normal handlers", w.members.len(), w.tactic.name()));
        self.strikes_release_members(w.members);
    }

    fn strikes_release_members(&mut self, members: Vec<i32>) {
        for id in members {
            self.strikes.members.remove(&id);
            self.strikes.evasion.remove(&id);
            if let Some(f) = self.fighters.remove(&id) {
                let u = f.unit;
                if u.health() <= 0.0 || u.team() != self.team_id {
                    continue;
                }
                u.stop(0);
                u.execute_custom_command(cmd::UNIT_CANCEL_TARGET, &[], 0, NO_TIMEOUT);
                u.set_fire_state(2, 0);
                u.set_move_state(1, 0);
                if !self.unit_classes.no_retreat.contains(self.defs.name_of(u)) {
                    self.retreat_add_coward(u);
                }
                if matches!(self.defs.name_of(u), "spiderscout" | "shieldscout" | "vehscout") {
                    self.assign_role(u, super::roles::Role::SoloRaider);
                } else {
                    self.war.new_units.insert(id);
                    self.war_unit_idle(u);
                }
            }
        }
    }

    /// Mount target masks distinguish dedicated AA from flex-AA and exclude
    /// fake/shield-only weapons. Unknown masks/definitions fail closed.
    fn strike_defenseless(&self, def: springai::UnitDef) -> bool {
        // Opportunities are shared by Fleas and Darts. A weapon capable of
        // hitting either scout is a threat, even if their target masks differ.
        let mut category = 0;
        for name in [Tactic::Flea.name(), Tactic::Dart.name()] {
            let Some(scout) = self.defs.get(name) else { return false };
            let mask = scout.category();
            if mask == 0 {
                return false;
            }
            category |= mask;
        }
        for mount in def.weapon_mounts() {
            let Some(w) = mount.weapon_def() else { return false };
            if w.is_shield() || super::water::is_fake(&w.name()) {
                continue;
            }
            if mount.only_target_category() & category != 0 && w.damage_types().iter().any(|d| *d > 0.0) {
                return false;
            }
        }
        true
    }

    /// Only current LOS exposes a trustworthy paralysis amount. The ratio
    /// uses live max health, including commander upgrades.
    fn strike_stun(&self, j: &Job) -> Option<(f32, i32)> {
        let e = j.id.and_then(|id| self.war.targets.get(&id))?;
        if e.is_dead || !e.visible || !e.unit.is_paralyzed() {
            return None;
        }
        let maxhp = e.unit.max_health();
        if !maxhp.is_finite() || maxhp <= 0.0 {
            return None;
        }
        let ratio = e.unit.paralyze_damage() / maxhp;
        let frames = super::fightsim::mech::paralysis_frames(ratio);
        (frames > 0).then_some((ratio, frames))
    }

    fn strike_target_allowed(&self, tactic: Tactic, j: &Job) -> bool {
        j.prey.allows(tactic, self.strike_stun(j).is_some())
            && (j.prey != Prey::Fragile
                || j.id.and_then(|id| self.war.targets.get(&id)).is_some_and(|e| self.strike_fragile(e)))
    }

    fn strike_preparable(&self, tactic: Tactic, j: &Job) -> bool {
        self.strike_target_allowed(tactic, j)
            || (j.prey.needs_stun(tactic) && j.id.is_some_and(|id| self.jaeger_stun_expected(id)))
    }

    fn strike_suitable(&self, tactic: Tactic, j: &Job, from: Vec3) -> bool {
        if !self.strike_preparable(tactic, j) {
            return false;
        }
        let Some(def) = self.defs.get(tactic.name()) else { return false };
        if self.missiles_ground_burning(j.pos) || self.strike_target_point(def, from, j.pos).is_none() {
            return false;
        }
        if j.prey == Prey::Defenseless {
            return self.strike_opportunity_clear(from, j.pos);
        }
        if !tactic.light_swarm() {
            return true;
        }
        // A lone riot is sporadic cover; overlapping riots or unknown screens
        // are not a reason to feed another light swarm every update.
        self.war
            .targets
            .values()
            .filter(|e| {
                !e.is_dead
                    && self.frame - e.last_seen < JOB_MEMORY
                    && Some(e.unit_id) != j.id
                    && !(e.visible && e.unit.is_paralyzed())
                    && (e.is_riot || !e.identified)
                    && distance(e.position, j.pos) < 600.0
            })
            .count()
            <= 1
    }

    fn strike_can_recruit(&self, f: &Fighter, name: &str) -> bool {
        self.defs.name_of(f.unit) == name
            && !self.squad_strike_owns(f.id)
            && !springai::reserved::is_reserved(self.cb, f.id)
            && !self.retreat_handler.is_retreating(f.id)
            && !self.qrf_claims(f.id)
            && !f
                .squad
                .is_some_and(|sid| self.op.over.squad_intents.contains_key(&sid) || self.op_squad_target(sid).is_some())
            && (self.squad_handler.fighters.contains(&f.id)
                || self.raider_handler.solo_raiders.contains(&f.id)
                || self.raider_handler.small_raiders.contains(&f.id)
                || (name == Tactic::Scythe.name() && self.raider_handler.medium_raiders.contains(&f.id)))
    }

    fn strike_cooling(&self, pos: Vec3) -> bool {
        self.strikes.cooldowns.iter().any(|(p, f)| self.frame - *f < COOLDOWN && distance(*p, pos) < 500.0)
    }

    pub fn strikes_update(&mut self, frame: i32) {
        if (frame + self.offset) % PERIOD != 0 {
            return;
        }
        self.strikes.evasion.retain(|id, (_, until)| frame < *until && self.fighters.contains_key(id));
        self.lobster_strikes_update();
        self.squad_strikes_maintain();
        // Offer ready Halberd squads first, before the dedicated wave borrows
        // individual members or an unrelated wave monopolizes recruitment.
        let squads: Vec<_> = self.squad_handler.squads.iter().copied().collect();
        for sid in squads {
            if self.squad_strike_halberds(sid) {
                self.squad_strike(sid);
            }
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
                if self.strike_cooling(j.pos) || self.squad_strike_job_taken(&j) {
                    continue;
                }
                for tactic in [Tactic::Scythe, Tactic::Halberd, Tactic::Dirtbag, Tactic::Flea, Tactic::Dart] {
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
                    let needed = j.size(tactic, cost);
                    if needed == 0 || needed > MAX_WAVE {
                        continue;
                    }
                    let mut existing: Vec<Vec3> = self
                        .fighters
                        .values()
                        .filter(|f| self.strike_can_recruit(f, tactic.name()))
                        .map(|f| f.get_pos())
                        .collect();
                    // A partial borrowed force without a factory must not
                    // monopolize selection and starve another viable tactic.
                    if existing.len() < needed
                        && (!j.production_pays(tactic, cost)
                            || !self.fac.factories.values().any(|f| f.kind() == tactic.factory()))
                    {
                        continue;
                    }
                    existing.sort_by(|a, b| distance(*a, j.pos).total_cmp(&distance(*b, j.pos)));
                    let home = existing
                        .first()
                        .copied()
                        .filter(|p| self.war.get_effective_threat(*p) <= 0.0)
                        .unwrap_or_else(|| self.graph_get_closest_haven(base));
                    let home = if j.prey.needs_stun(tactic) {
                        // Pounce from the existing force, not a distant factory.
                        let n = needed as f32;
                        Vec3::xz(
                            existing.iter().take(needed).map(|p| p.x).sum::<f32>() / n,
                            existing.iter().take(needed).map(|p| p.z).sum::<f32>() / n,
                        )
                    } else if home == NULLPOS {
                        base
                    } else {
                        home
                    };
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
        // A waiting/returning light swarm may exploit a freshly stunned heavy.
        // No new procurement for this short window; keep the full-wave rule.
        if let Some(mut w) = self.strikes.wave.take() {
            if matches!(w.phase, Phase::Forming | Phase::Screening) {
                if w.job.id.is_some_and(|id| self.strikes.killed.contains(&id)) {
                    self.strikes_release(w);
                    return;
                }
                // Surviving estimates are refreshed below from the kill-adjusted
                // evidence; do not subtract these deaths again after launch.
                self.strikes.killed.clear();
            }
            if matches!(w.phase, Phase::Forming | Phase::Screening) && frame - w.job.last >= SNIPER_MEMORY {
                let cost = self.defs.get(w.tactic.name()).map_or(1.0, |d| d.cost(self.metal));
                if let Some(j) = self.strike_jobs(SNIPER_MEMORY).into_iter().find(|j| {
                    !self.strike_cooling(j.pos)
                        && self.strike_suitable(w.tactic, j, w.home)
                        && (j.production_pays(w.tactic, cost) || w.members.len() >= j.size(w.tactic, cost))
                }) {
                    self.log(&format!(
                        "strikes: waiting {} wave switches to fresh {} activity",
                        w.tactic.name(),
                        j.name
                    ));
                    w.job = j;
                    w.query = None;
                }
            }
            if w.tactic.light_swarm() && w.phase != Phase::Diving {
                let positions: Vec<_> = w
                    .members
                    .iter()
                    .filter_map(|id| self.fighters.get(id))
                    .filter(|f| f.unit.health() > 0.0)
                    .map(|f| f.get_pos())
                    .collect();
                let n = positions.len().max(1) as f32;
                let from = Vec3::xz(
                    positions.iter().map(|p| p.x).sum::<f32>() / n,
                    positions.iter().map(|p| p.z).sum::<f32>() / n,
                );
                if let Some(j) = self.strike_jobs(SNIPER_MEMORY).into_iter().find(|j| {
                    j.prey.needs_stun(w.tactic)
                        && j.id != w.job.id
                        && !self.strike_cooling(j.pos)
                        && positions.len() >= j.size(w.tactic, 1.0)
                        && self.strike_suitable(w.tactic, j, from)
                }) {
                    self.log(&format!("strikes: {} wave pounces on stunned {}", w.tactic.name(), j.name));
                    w.needed = j.size(w.tactic, 1.0);
                    w.job = j;
                    w.home = from;
                    w.phase = Phase::Forming;
                    w.since = frame;
                    w.query = None;
                }
            }
            if matches!(w.phase, Phase::Forming | Phase::Screening) && !self.strike_preparable(w.tactic, &w.job) {
                self.strikes_release(w);
                return;
            }
            self.strikes.wave = Some(w);
        }
        // Borrow only the named tactic, from ordinary handlers. Never steal QRF,
        // operator-controlled units, scouts or other special forces.
        if let Some(w) = self.strikes.wave.as_ref().filter(|w| matches!(w.phase, Phase::Forming | Phase::Screening)) {
            let name = w.tactic.name();
            let need = w.needed.saturating_sub(w.members.len());
            let home = w.home;
            let mut recruits: Vec<Unit> =
                self.fighters.values().filter(|f| self.strike_can_recruit(f, name)).map(|f| f.unit).collect();
            recruits.sort_by(|a, b| distance(a.pos(), home).total_cmp(&distance(b.pos(), home)));
            for u in recruits.into_iter().take(need) {
                self.strikes_claims(u);
            }
        }
        let Some(mut w) = self.strikes.wave.take() else { return };
        w.members
            .retain(|id| self.fighters.get(id).is_some_and(|f| f.unit.health() > 0.0 && f.unit.team() == self.team_id));
        if matches!(w.phase, Phase::Forming | Phase::Screening) {
            let fresh = self.strike_jobs(JOB_MEMORY).into_iter().find(|j| {
                if w.job.id.is_some() {
                    j.id == w.job.id
                } else {
                    j.name == w.job.name && distance(j.pos, w.job.pos) < 500.0
                }
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
            if !self.strike_tactic_reaches(w.tactic, w.home, w.job.pos)
                || (!w.members.is_empty() && !self.strike_members_reach(&w.members, w.job.pos))
            {
                self.strikes_release(w);
                return;
            }
            let cost = self.defs.get(w.tactic.name()).map_or(1.0, |d| d.cost(self.metal));
            w.needed = w.job.size(w.tactic, cost);
            if w.members.len() > w.needed {
                self.strikes_release_members(w.members.split_off(w.needed));
            }
            if w.needed > MAX_WAVE
                || (w.members.len() < w.needed
                    && (!w.job.production_pays(w.tactic, cost)
                        || !self.fac.factories.values().any(|f| f.kind() == w.tactic.factory())))
            {
                self.strikes_release(w);
                return;
            }
            // Follow the front while waiting, and gather before a likely call.
            if !(w.job.prey.needs_stun(w.tactic) && self.strike_stun(&w.job).is_some()) {
                if let Some(stage) = self.strike_stage(&w) {
                    if distance(stage, w.home) > 100.0 {
                        w.home = stage;
                        w.query = None;
                    }
                }
            }
            if w.job.prey == Prey::Defenseless && !self.strike_opportunity_clear(w.home, w.job.pos) {
                self.log("strikes: release opportunistic wave; approach is defended");
                self.strikes_release(w);
                return;
            }
            let expected = w.job.id.is_some_and(|id| self.jaeger_stun_expected(id));
            w.phase = staging::waiting_phase(w.tactic, frame - w.job.last, expected);
            if w.phase == Phase::Screening || self.squad_strike_job_taken(&w.job) {
                w.query = None;
            }
            let radius = staging::muster_radius(w.tactic, w.members.len());
            let gathered = self.strike_waiting_risk(w.home, radius) <= 0.0
                && w.members.iter().all(|id| self.strike_evasion_goal(*id).is_none())
                && w.members.len() >= w.needed
                && w.members
                    .iter()
                    .all(|id| self.fighters.get(id).is_some_and(|f| distance(f.get_pos(), w.home) < radius));
            if w.phase == Phase::Forming
                && !self.squad_strike_job_taken(&w.job)
                && gathered
                && frame - w.job.last < SNIPER_MEMORY
                && self.strike_target_allowed(w.tactic, &w.job)
                && self.strike_suitable(w.tactic, &w.job, w.home)
                && (w.job.prey != Prey::Defenseless || self.strike_opportunity_wave_clear(&w, w.job.pos))
            {
                self.strike_predict(&mut w);
            }
        } else if w.phase == Phase::Diving {
            let target = w.job.id.and_then(|id| self.war.targets.get(&id));
            let mut killed = std::mem::take(&mut self.strikes.killed);
            if let Some(e) = target.filter(|e| e.is_dead) {
                killed.insert(e.unit_id);
            }
            let dead = w.job.id.is_some_and(|id| killed.contains(&id));
            if w.job.phantom() {
                w.job.count = w.job.count.saturating_sub(killed.len());
            }
            let end = target.filter(|e| e.visible || e.is_radar_visible).map_or(w.job.pos, |e| e.position);
            let unreachable = !self.strike_members_reach(&w.members, end);
            let defended = w.job.prey == Prey::Defenseless && !self.strike_opportunity_wave_clear(&w, end);
            let next_soft = if dead && !defended && w.job.prey == Prey::Defenseless {
                self.strike_jobs(SNIPER_MEMORY).into_iter().find(|j| {
                    j.prey == Prey::Defenseless
                        && j.id != w.job.id
                        && distance(j.pos, w.job.pos) < 600.0
                        && !self.strike_cooling(j.pos)
                        && self.strike_suitable(w.tactic, j, w.job.pos)
                        && self.strike_opportunity_wave_clear(&w, j.pos)
                })
            } else {
                None
            };
            if let Some(next) = next_soft {
                self.log(&format!(
                    "strikes: {} continue through defenseless formation onto {}",
                    w.tactic.name(),
                    next.name
                ));
                w.job = next;
                w.since = frame;
            } else if dead && w.job.phantom() && w.job.count > 0 {
                w.job.id = None; // Sweep for the next Phantom with the surviving wave.
            } else if dead
                || unreachable
                || defended
                || !self.strike_target_allowed(w.tactic, &w.job)
                || frame - w.since > DIVE_TIMEOUT
                || w.members.is_empty()
            {
                self.log(&format!(
                    "strikes: {} ended ({})",
                    w.tactic.name(),
                    if dead {
                        "target killed"
                    } else if unreachable {
                        "target left reachable terrain"
                    } else if defended {
                        "opportunity now defended"
                    } else if w.job.prey == Prey::Fragile && !self.strike_target_allowed(w.tactic, &w.job) {
                        "fragile target no longer eligible"
                    } else if !self.strike_target_allowed(w.tactic, &w.job) {
                        "stun window closed"
                    } else {
                        "search expired or wave lost"
                    }
                ));
                w.phase = Phase::Returning;
                w.since = frame;
                self.strikes.cooldowns.push((w.job.pos, frame));
            } else if let Some(e) = target.filter(|e| e.visible || e.is_radar_visible) {
                w.job.pos = e.position;
                w.job.last = frame;
            } else if w.job.phantom() {
                if let Some(e) = self.war.targets.values().find(|e| {
                    !e.is_dead
                        && e.visible
                        && e.def_name == "cloaksnipe"
                        && distance(e.position, w.job.pos) < PHANTOM_FIX_RADIUS
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
            let cost = self.defs.get(w.tactic.name()).map_or(1.0, |d| d.cost(self.metal));
            if let Some(j) = self.strike_jobs(SNIPER_MEMORY).into_iter().find(|j| {
                !self.strike_cooling(j.pos)
                    && self.strike_suitable(w.tactic, j, w.home)
                    && (j.production_pays(w.tactic, cost) || w.members.len() >= j.size(w.tactic, cost))
            }) {
                let needed = j.size(w.tactic, cost);
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
            if matches!(w.phase, Phase::Forming | Phase::Screening)
                && let Some(goal) = self.strike_evasion_goal(*id)
            {
                kgb_move_to(u, goal, 0, self.frame);
                continue;
            }
            if w.phase == Phase::Diving
                && let Some(t) = visible
                && distance(u.pos(), t.pos()) <= u.max_range().max(80.0) * 0.9
            {
                u.set_fire_state(w.tactic.fire_state(), 0); // Armored/stealth approaches focus fire; chaff shoots freely.
                u.attack(t, 0);
                continue;
            }
            u.set_fire_state(w.tactic.fire_state(), 0);
            let goal = if w.tactic == Tactic::Halberd && w.phase == Phase::Diving && !close {
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
            // Three cloaked hunters cover a wider circuit than a chaff comb;
            // the Phantom's larger decloak radius does the final locating.
            let sweep_radius = if w.tactic == Tactic::Scythe { 400.0 } else { 200.0 };
            let sweep = if searching { (angle.cos() * sweep_radius, angle.sin() * sweep_radius) } else { (0.0, 0.0) };
            // Once it is revealed, every member must enter firing range; a
            // permanent wide grid would leave the outside swarm members idle.
            let spacing = if w.tactic != Tactic::Halberd && matches!(w.phase, Phase::Forming | Phase::Returning) {
                staging::MUSTER_SPACING
            } else if w.phase == Phase::Diving && visible.is_some() {
                SPREAD.min(u.max_range().max(40.0) / width.max(2) as f32)
            } else {
                SPREAD
            };
            let mut p = Vec3::xz(
                goal.x + (i % width) as f32 * spacing - (width - 1) as f32 * spacing / 2.0 + sweep.0,
                goal.z + (i / width) as f32 * spacing - (width - 1) as f32 * spacing / 2.0 + sweep.1,
            );
            if w.tactic != Tactic::Halberd && (w.phase == Phase::Screening || w.phase == Phase::Diving) {
                let spacing = if visible.is_some() && w.phase == Phase::Diving {
                    u.max_range().max(40.0) / w.members.len().max(2) as f32
                } else {
                    SPREAD
                };
                p = staging::line_point(goal, w.job.pos, w.home, i, w.members.len(), spacing);
                if w.phase == Phase::Screening {
                    // Contract unsafe/unreachable ends instead of stretching
                    // the screen across cliffs or into known enemy fire.
                    for _ in 0..5 {
                        if self.strike_stage_clear(u, w.home, p) {
                            break;
                        }
                        p = Vec3::xz((p.x + w.home.x) * 0.5, (p.z + w.home.z) * 0.5);
                    }
                    if !self.strike_stage_clear(u, w.home, p) {
                        p = w.home;
                    }
                }
            }
            let map = self.cb.map();
            p.x = p.x.clamp(1.0, (map.width() * 8 - 1).max(1) as f32);
            p.z = p.z.clamp(1.0, (map.height() * 8 - 1).max(1) as f32);
            p.y = map.elevation_at(p.x, p.z);
            let range = if w.phase == Phase::Diving { u.max_range().max(0.0) } else { 100.0 };
            let Some(p) = self.strike_move_point(u, p, center, range) else {
                u.stop(0); // Cancel an older chase; never reuse an illegal endpoint.
                continue;
            };
            // RAW_MOVE uses the engine's terrain pathing, with no random
            // dispersion or enemy-avoidance detours that would undo the dive.
            kgb_move_to(u, p, 0, self.frame);
        }
    }

    fn strikes_support_fire(&mut self) {
        let mut fixes = if self.dev_off("strikes") { Vec::new() } else { self.strikes_phantom_targets() };
        if !self.dev_off("strikes") {
            fixes.extend(
                self.counterbattery_fixes(FIRE_MEMORY)
                    .filter(|f| f.ground && f.uncertainty <= 400.0)
                    .map(|f| (f.pos, 1)),
            );
        }
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
    fn defenseless_formations_are_flea_jobs_with_a_production_budget() {
        let mut j = Job {
            id: Some(1),
            pos: Vec3::ZERO,
            last: 0,
            count: 4,
            value: 400.0,
            name: "vehaa".into(),
            prey: Prey::Defenseless,
        };
        assert!(j.prey.allows(Tactic::Flea, false));
        assert!(!j.prey.allows(Tactic::Halberd, false));
        assert!(!j.prey.needs_stun(Tactic::Flea));
        assert_eq!(j.size(Tactic::Flea, 25.0), 20);
        assert!(!j.production_pays(Tactic::Flea, 25.0));
        j.value = 800.0;
        assert!(j.production_pays(Tactic::Flea, 25.0));
    }

    #[test]
    fn commanders_never_requisition_any_strike_tactic() {
        let commander = Job {
            id: Some(1),
            pos: Vec3::ZERO,
            last: 0,
            count: 1,
            value: 1200.0,
            name: "dyntrainer_recon_base".into(),
            prey: Prey::Commander,
        };
        for tactic in [Tactic::Halberd, Tactic::Dirtbag, Tactic::Flea, Tactic::Dart, Tactic::Scythe] {
            assert!(!Prey::Commander.can_requisition(tactic));
            assert!(!commander.production_pays(tactic, 30.0));
            assert!(Prey::GlassCannon.can_requisition(tactic));
        }
        assert!(!Prey::Heavy.can_requisition(Tactic::Flea));
        // Only procurement is forbidden: an existing Dirtbag/Halberd force
        // can still commit, and existing Fleas still need paralysis.
        assert!(Prey::Commander.allows(Tactic::Dirtbag, false));
        assert!(Prey::Commander.allows(Tactic::Halberd, false));
        assert!(!Prey::Commander.allows(Tactic::Flea, false));
        assert!(Prey::Commander.allows(Tactic::Flea, true));
    }

    #[test]
    fn fleas_need_stunned_commanders_and_heavies_but_not_glass_cannons() {
        assert!(!Prey::Commander.allows(Tactic::Flea, false));
        assert!(Prey::Commander.allows(Tactic::Flea, true));
        assert!(Prey::Commander.allows(Tactic::Halberd, false));
        assert!(Prey::Commander.allows(Tactic::Dirtbag, false));
        assert!(!Prey::Heavy.allows(Tactic::Flea, false));
        assert!(Prey::Heavy.allows(Tactic::Flea, true));
        assert!(!Prey::Heavy.allows(Tactic::Dirtbag, true));
        for name in ["cloaksnipe", "hoverarty", "jumparty", "striderarty", "tankheavyarty"] {
            assert!(glass_cannon(name));
        }
        assert!(Prey::GlassCannon.allows(Tactic::Flea, false));
    }

    #[test]
    fn darts_can_spend_more_than_a_phantom_but_not_procure_for_stuns_or_cheap_raids() {
        let mut job = Job {
            id: None,
            pos: Vec3::ZERO,
            last: 0,
            count: 1,
            value: 750.0,
            name: "cloaksnipe".into(),
            prey: Prey::GlassCannon,
        };
        assert_eq!(Tactic::Dart.factory(), FacKind::Veh);
        assert_eq!(job.size(Tactic::Dart, 40.0), 20);
        assert!(job.production_pays(Tactic::Dart, 40.0)); // 800 metal is acceptable.
        job.count = 2;
        assert_eq!(job.size(Tactic::Dart, 40.0), 40);
        for prey in [Prey::Commander, Prey::Heavy] {
            assert!(!prey.allows(Tactic::Dart, false));
            assert!(prey.allows(Tactic::Dart, true));
            assert!(prey.needs_stun(Tactic::Dart));
            assert!(!prey.can_requisition(Tactic::Dart));
        }
        job.name = "vehaa".into();
        job.prey = Prey::Defenseless;
        assert!(job.prey.allows(Tactic::Dart, false));
        assert!(!job.production_pays(Tactic::Dart, 40.0));
        job.value = 800.0;
        assert!(job.production_pays(Tactic::Dart, 40.0));
    }

    #[test]
    fn inferred_phantom_death_is_recognized_before_acquisition() {
        let mut job = Job {
            id: None,
            pos: Vec3::xz(1000.0, 1000.0),
            last: 0,
            count: 1,
            value: 750.0,
            name: "cloaksnipe".into(),
            prey: Prey::GlassCannon,
        };
        assert!(job.matches_death(42, "cloaksnipe", Vec3::xz(1100.0, 1000.0)));
        assert!(!job.matches_death(42, "cloaksnipe", Vec3::xz(2000.0, 1000.0)));
        assert!(!job.matches_death(42, "jumparty", job.pos));
        job.id = Some(43);
        assert!(!job.matches_death(42, "cloaksnipe", job.pos));
        assert!(job.matches_death(43, "cloaksnipe", job.pos));
        job.count = 2;
        assert!(job.matches_death(42, "cloaksnipe", job.pos));
    }

    #[test]
    fn armored_and_cloaked_approaches_hold_fire_but_chaff_shoots() {
        assert_eq!(Tactic::Halberd.fire_state(), 0);
        assert_eq!(Tactic::Scythe.fire_state(), 0);
        assert_eq!(Tactic::Flea.fire_state(), 2);
        assert_eq!(Tactic::Dart.fire_state(), 2);
        assert_eq!(Tactic::Dirtbag.fire_state(), 2);
    }

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
        assert_eq!(Tactic::Scythe.size(1, 750.0, 250.0), 3);
        assert_eq!(Tactic::Scythe.size(4, 3000.0, 250.0), 12);
        assert_eq!(Tactic::Scythe.size(1, 1000.0, 250.0), 3);
        assert!(Prey::GlassCannon.can_requisition(Tactic::Scythe));
        assert!(!Prey::Commander.can_requisition(Tactic::Scythe));
        assert!(!Prey::Heavy.allows(Tactic::Scythe, true));
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
