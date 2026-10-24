//! The fight predictor in play: snapshots from the AI's own state, the
//! worker's answers, and the one decision it drives.
//!
//! Behind the `fightsim` switch (off by default, see `ai.rs`). Its one
//! consumer is the attacking squad's danger check in `handlers/squad.rs`:
//! where the flat threat map says "the enemy threat here, over our courage,
//! exceeds what we tolerate", the predictor says "the fight we are walking
//! into is one we are expected to lose". Side A is the squad and our other
//! fighters beside it; side B is every identified enemy within reach, as
//! last seen. Answers arrive a few frames late and are used as long as they
//! are younger than [`STALE`]; with no fresh answer, or no contact, the flat
//! rule decides as before. A cost line goes to the journal every minute.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use serde_json::json;
use springai::{UnitDef, Vec3};

use super::worker::{FightAnswer, FightRequest, FightWorker};
use super::{FightSnapshot, Model, SnapUnit, UnitSpec, WeaponSpec};
use crate::Ai;
use crate::military::SquadId;
use crate::util::distance;

/// The switch (`enable=fightsim`).
pub const FEATURE: &str = "fightsim";
/// Frames an answer stays usable (3 s).
pub const STALE: i32 = 90;
/// Enemies within this of the squad are in the fight.
const ENEMY_REACH: f32 = 1200.0;
/// Our other fighters within this of the squad are on its side.
const FRIEND_REACH: f32 = 500.0;
/// Enemies unseen for longer (frames) are not counted.
const SEEN_WITHIN: i32 = 900;
/// Frames between cost lines in the journal.
const REPORT_PERIOD: i32 = 1800;
/// Below this expected own loss share a predicted defeat is no reason to leave.
const MIN_LOSS: f32 = 0.1;

/// The predictor's state in the military manager.
#[derive(Default)]
pub struct FightSim {
    worker: Option<FightWorker>,
    /// The model failed to load or start: stay on the flat rule.
    failed: bool,
    specs: HashMap<i32, Arc<UnitSpec>>,
    /// The latest answer per squad.
    answers: BTreeMap<SquadId, FightAnswer>,
    /// Decisions asked of the predictor and how they were made.
    pub verdicts: u64,
    pub no_contact: u64,
    pub fallbacks: u64,
    pub sim_retreats: u64,
    pub flat_retreats: u64,
    pub disagreements: u64,
    last_report: i32,
    reported: (u64, u64, u64),
}

impl Ai {
    /// Start or stop the worker with the switch, pick up its answers.
    pub fn fightsim_update(&mut self, frame: i32) {
        if self.dev_off(FEATURE) {
            if self.war.fightsim.worker.take().is_some() {
                self.log("fightsim: off; the predictor thread stopped");
            }
            return;
        }
        if self.war.fightsim.worker.is_none() && !self.war.fightsim.failed {
            match Model::load().and_then(FightWorker::start) {
                Ok(w) => {
                    self.war.fightsim.worker = Some(w);
                    self.war.fightsim.last_report = frame;
                    self.log("fightsim: on; the predictor thread started");
                }
                Err(e) => {
                    self.war.fightsim.failed = true;
                    self.log(&format!("fightsim: {e}; squads keep the flat threat rule"));
                    return;
                }
            }
        }
        let fs = &mut self.war.fightsim;
        let Some(w) = fs.worker.as_mut() else { return };
        for a in w.drain() {
            let newer = fs.answers.get(&a.key).is_none_or(|old| a.frame >= old.frame);
            if newer {
                fs.answers.insert(a.key, a);
            }
        }
        fs.answers.retain(|_, a| frame - a.frame <= 10 * STALE);
        if frame - fs.last_report >= REPORT_PERIOD {
            self.fightsim_report(frame);
        }
    }

    /// The cost line: queries per minute, mean and worst microseconds per
    /// query on the worker, refusals, fallbacks and disagreements with the
    /// flat rule, since the last line.
    fn fightsim_report(&mut self, frame: i32) {
        let fs = &mut self.war.fightsim;
        let Some(w) = fs.worker.as_ref() else { return };
        let s = w.stats();
        let minutes = ((frame - fs.last_report) as f64 / 1800.0).max(1e-6);
        let (q0, m0, a0) = fs.reported;
        let (dq, dm, da) = (s.queries - q0, s.micros - m0, s.answers - a0);
        fs.reported = (s.queries, s.micros, s.answers);
        fs.last_report = frame;
        if dq == 0 && fs.verdicts == 0 {
            return;
        }
        let mean_us = if da > 0 { dm as f64 / da as f64 } else { 0.0 };
        let data = json!({
            "event": "cost", "t": frame / 30,
            "queries_per_min": dq as f64 / minutes, "mean_us": mean_us, "max_us": s.max_micros,
            "queue_full": s.full, "verdicts": fs.verdicts, "no_contact": fs.no_contact, "fallbacks": fs.fallbacks,
            "sim_retreats": fs.sim_retreats, "flat_retreats": fs.flat_retreats, "disagreements": fs.disagreements,
        });
        let line = format!(
            "fightsim: {:.0} queries/min, {mean_us:.0} us mean ({} max); {} verdicts, {} without contact, {} fallbacks, {} queue-full; retreats sim {} flat {}, {} disagreements",
            dq as f64 / minutes,
            s.max_micros,
            fs.verdicts,
            fs.no_contact,
            fs.fallbacks,
            s.full,
            fs.sim_retreats,
            fs.flat_retreats,
            fs.disagreements
        );
        self.log(&line);
        self.op_emit("fightsim", data);
    }

    /// Whether the predictor is deciding for squads now.
    pub fn fightsim_on(&self) -> bool {
        self.war.fightsim.worker.is_some() && !self.dev_off(FEATURE)
    }

    /// The simulation's view of a unit type, cached per def.
    fn fight_spec(&mut self, def: UnitDef) -> Arc<UnitSpec> {
        if let Some(s) = self.war.fightsim.specs.get(&def.id()) {
            return s.clone();
        }
        let speed = def.speed() / 30.0;
        let mut weapons: Vec<WeaponSpec> = Vec::new();
        for wd in def.weapon_mounts().iter().filter_map(|m| m.weapon_def()) {
            let name = wd.name();
            // One entry per weapon def, as the fitted model counted them.
            if weapons.iter().any(|w| w.name == name)
                || wd.is_shield()
                || name.contains("fake")
                || name.contains("noweapon")
            {
                continue;
            }
            let damage = wd.damage_types().first().copied().unwrap_or(0.0);
            let range = wd.range();
            if damage <= 0.0 || range <= 0.0 {
                continue;
            }
            weapons.push(WeaponSpec {
                name,
                range,
                reload: wd.reload(),
                salvo: wd.salvo_size(),
                damage,
                weapon_type: wd.weapon_type(),
            });
        }
        let spec = Arc::new(UnitSpec {
            def: self.defs.name(def).to_owned(),
            cost: def.cost(self.metal),
            speed,
            mobile: def.is_able_to_move() && speed > 0.0,
            weapons,
        });
        self.war.fightsim.specs.insert(def.id(), spec.clone());
        spec
    }

    /// The fight a squad at `pos` would be in now: the squad and our other
    /// fighters beside it against every identified enemy within reach as
    /// last seen. `None` without an armed enemy in reach (nothing to
    /// predict).
    fn fight_snapshot(&mut self, sid: SquadId, pos: Vec3) -> Option<FightSnapshot> {
        let frame = self.frame;
        let mut ours: Vec<(UnitDef, f32, Vec3)> = Vec::new();
        let members: Vec<i32> = self.squads.get(sid).map(|s| s.fighters.clone()).unwrap_or_default();
        for (id, f) in &self.fighters {
            if !members.contains(id)
                && (distance(f.get_pos(), pos) > FRIEND_REACH || self.retreat_handler.is_retreating(*id))
            {
                continue;
            }
            let (Some(def), hp) = (f.unit.def(), f.unit.health()) else { continue };
            if hp <= 0.0 {
                continue;
            }
            ours.push((def, hp, f.unit.pos()));
        }
        let mut theirs: Vec<(UnitDef, f32, Vec3)> = Vec::new();
        for e in self.war.targets.values() {
            if e.is_dead
                || !e.identified
                || frame - e.last_seen > SEEN_WITHIN
                || distance(e.position, pos) > ENEMY_REACH
            {
                continue;
            }
            let Some(def) = e.ud else { continue };
            theirs.push((def, e.get_health() * e.def_health(), e.position));
        }
        let mut snap = FightSnapshot::default();
        for (side, list) in [(0u8, ours), (1u8, theirs)] {
            for (def, hp, p) in list {
                let spec = self.fight_spec(def);
                snap.units.push(SnapUnit { side, spec, hp, x: p.x, z: p.z });
            }
        }
        (snap.armed(1) > 0).then_some(snap)
    }

    /// Whether an attacking squad at `pos` should fall back, by the
    /// predictor: it expects to lose the fight it is in (calibrated P(A
    /// wins) below 1/2 over the squad's courage) and to lose at least
    /// [`MIN_LOSS`] of its own cost. `flat` is the threat map's verdict,
    /// which stands wherever the predictor has nothing fresh to say.
    pub fn fight_verdict(&mut self, sid: SquadId, pos: Vec3, courage: f32, flat: bool) -> bool {
        if !self.fightsim_on() {
            return flat;
        }
        let frame = self.frame;
        self.war.fightsim.verdicts += 1;
        if flat {
            self.war.fightsim.flat_retreats += 1;
        }
        let Some(snapshot) = self.fight_snapshot(sid, pos) else {
            self.war.fightsim.no_contact += 1;
            return flat;
        };
        let armed = (snapshot.armed(0), snapshot.armed(1));
        let fs = &mut self.war.fightsim;
        if let Some(w) = fs.worker.as_mut() {
            w.request(FightRequest { key: sid, frame, snapshot });
        }
        let Some(a) = fs.answers.get(&sid).filter(|a| frame - a.frame <= STALE) else {
            fs.fallbacks += 1;
            return flat;
        };
        let o = a.outcome;
        let retreat = o.p_a_wins < 0.5 / courage.max(0.1) && o.mean_a >= MIN_LOSS;
        if retreat {
            fs.sim_retreats += 1;
        }
        if retreat != flat {
            fs.disagreements += 1;
            let age = frame - a.frame;
            self.op_emit(
                "fightsim",
                json!({
                    "event": "verdict", "t": frame / 30, "squad": sid, "age": age, "armed": [armed.0, armed.1],
                    "p_a_wins": o.p_a_wins, "killed": [o.killed_a, o.killed_b], "mean": [o.mean_a, o.mean_b],
                    "withdraw": [o.withdraw_a, o.withdraw_b], "sim": retreat, "flat": flat, "courage": courage,
                }),
            );
        }
        retreat
    }
}
