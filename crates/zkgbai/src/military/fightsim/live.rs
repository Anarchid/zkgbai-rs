//! The fight predictor in play: snapshots from the AI's own state, the
//! worker's answers, and the one decision it drives.
//!
//! Two switches (see `ai.rs`): `fightshadow`, on by default, runs the
//! predictor and logs what it would decide against the flat rule without
//! acting; `fightsim`, off by default, lets it drive. A third, `fightmech`
//! (on by default), runs the mechanics model (`mech.rs`) beside the base
//! one on every query and logs its answer; it decides nothing. The
//! predictor's one consumer is the attacking squad's danger check in
//! `handlers/squad.rs`: where the flat threat map says "the enemy threat
//! here, over our courage, exceeds what we tolerate", the predictor says
//! "the fight we are walking into is one we are expected to lose". Side A is the squad and our other fighters
//! beside it; side B is every identified enemy within reach, as last seen.
//! Answers arrive a few frames late and are used as long as they are
//! younger than [`STALE`]; with no fresh answer, or no contact, the flat
//! rule decides as before.
//!
//! Journal (`k=fightsim`): `event: cost` every minute; `event: verdict`
//! whenever the two rules disagree, with both answers and the fight as the
//! predictor saw it (`agree: false`), and for one agreement in
//! [`AGREE_SAMPLE`] the same record with `agree: true`; `event: window`
//! 30 s after each verdict record with the squad's metal lost meanwhile
//! and what the enemies in the snapshot lost (seen dying), the mechanism
//! the arm reads.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use serde_json::json;
use springai::{UnitDef, Vec3};

use super::worker::{FightAnswer, FightRequest, FightWorker};
use super::{FightSnapshot, Model, SnapUnit, UnitSpec, WeaponSpec};
use crate::Ai;
use crate::military::SquadId;
use crate::util::distance;

/// The driving switch (`enable=fightsim`).
pub const FEATURE: &str = "fightsim";
/// The shadow switch (`disable=fightshadow` stops the predictor entirely).
pub const SHADOW: &str = "fightshadow";
/// The mechanics model beside the base one (`disable=fightmech`).
pub const MECH: &str = "fightmech";
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
/// Frames after a verdict over which the squad's losses are read.
const WINDOW: i32 = 900;
/// One agreement with the flat rule in this many is logged, with a window.
const AGREE_SAMPLE: u64 = 5;

/// A verdict whose loss window is still open.
struct Window {
    vid: u64,
    squad: SquadId,
    frame: i32,
    /// Fighter-store ids of side A and their metal at the verdict.
    members: Vec<(i32, f32)>,
    metal: f32,
    /// Side B's unit ids, metal, and whether we saw it die since.
    enemies: Vec<(i32, f32, bool)>,
    agree: bool,
}

/// The predictor's state in the military manager.
#[derive(Default)]
pub struct FightSim {
    worker: Option<FightWorker>,
    /// The model failed to load or start: stay on the flat rule.
    failed: bool,
    specs: HashMap<i32, Arc<UnitSpec>>,
    /// The latest answer per squad.
    answers: BTreeMap<SquadId, FightAnswer>,
    windows: Vec<Window>,
    next_vid: u64,
    /// Decisions asked of the predictor and how they were made.
    pub verdicts: u64,
    pub no_contact: u64,
    pub fallbacks: u64,
    pub sim_retreats: u64,
    pub flat_retreats: u64,
    pub disagreements: u64,
    pub agreements: u64,
    last_report: i32,
    reported: (u64, u64, u64),
}

/// Side A of a snapshot as the window needs it, and the sides' metal.
struct SnapshotNotes {
    members: Vec<(i32, f32)>,
    /// Side B's unit ids and metal.
    enemies: Vec<(i32, f32)>,
    metal: [f32; 2],
    /// Share of B's metal in aircraft and in statics.
    air_share: f32,
    static_share: f32,
    n: [usize; 2],
}

impl Ai {
    /// Whether the predictor drives squads (not just shadows).
    pub fn fightsim_drives(&self) -> bool {
        !self.dev_off(FEATURE)
    }

    /// Whether the predictor runs at all (driving or in shadow).
    pub fn fightsim_runs(&self) -> bool {
        self.fightsim_drives() || !self.dev_off(SHADOW)
    }

    /// An enemy died: the open windows that had it in their fight count it.
    pub fn fightsim_enemy_destroyed(&mut self, id: i32) {
        for w in &mut self.war.fightsim.windows {
            for e in w.enemies.iter_mut().filter(|e| e.0 == id) {
                e.2 = true;
            }
        }
    }

    /// Start or stop the worker with the switches, pick up its answers,
    /// close loss windows.
    pub fn fightsim_update(&mut self, frame: i32) {
        if !self.fightsim_runs() {
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
                    let mode = if self.fightsim_drives() { "driving squads" } else { "in shadow" };
                    self.log(&format!("fightsim: on, {mode}; the predictor thread started"));
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
        self.fightsim_close_windows(frame);
        if frame - self.war.fightsim.last_report >= REPORT_PERIOD {
            self.fightsim_report(frame);
        }
    }

    /// Verdicts whose 30 s are up: the squad's metal lost since, by its
    /// members then that are dead now.
    fn fightsim_close_windows(&mut self, frame: i32) {
        let due: Vec<Window> = {
            let ws = &mut self.war.fightsim.windows;
            let (due, open): (Vec<Window>, Vec<Window>) =
                std::mem::take(ws).into_iter().partition(|w| frame - w.frame >= WINDOW);
            *ws = open;
            due
        };
        for w in due {
            let mut lost = 0.0;
            let mut dead = 0;
            for (id, metal) in &w.members {
                let alive = self.fighters.get(id).is_some_and(|f| f.unit.health() > 0.0);
                if !alive {
                    lost += metal;
                    dead += 1;
                }
            }
            let gone = w.enemies.iter().filter(|e| e.2);
            let (enemy_dead, enemy_lost) = (gone.clone().count(), gone.map(|e| e.1).sum::<f32>());
            self.op_emit(
                "fightsim",
                json!({
                    "event": "window", "vid": w.vid, "squad": w.squad, "t": frame / 30, "verdict_t": w.frame / 30,
                    "agree": w.agree, "members": w.members.len(), "metal": w.metal, "dead": dead, "lost": lost,
                    "enemies": w.enemies.len(), "enemy_metal": w.enemies.iter().map(|e| e.1).sum::<f32>(),
                    "enemy_dead": enemy_dead, "enemy_lost": enemy_lost,
                }),
            );
        }
    }

    /// The cost line: queries per minute, mean and worst microseconds per
    /// query on the worker, refusals, fallbacks and disagreements with the
    /// flat rule, since the last line.
    fn fightsim_report(&mut self, frame: i32) {
        let drives = self.fightsim_drives();
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
            "event": "cost", "t": frame / 30, "mode": if drives { "drive" } else { "shadow" },
            "queries_per_min": dq as f64 / minutes, "mean_us": mean_us, "max_us": s.max_micros,
            "queue_full": s.full, "verdicts": fs.verdicts, "no_contact": fs.no_contact, "fallbacks": fs.fallbacks,
            "sim_retreats": fs.sim_retreats, "flat_retreats": fs.flat_retreats, "disagreements": fs.disagreements,
            "agreements": fs.agreements,
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
            health: def.health(),
            air: def.is_able_to_fly(),
            builder: def.is_builder(),
            com: self.defs.is_commander(def),
            radius: def.radius(),
        });
        self.war.fightsim.specs.insert(def.id(), spec.clone());
        spec
    }

    /// The fight a squad at `pos` would be in now: the squad and our other
    /// fighters beside it against every identified enemy within reach as
    /// last seen. `None` without an armed enemy in reach (nothing to
    /// predict).
    fn fight_snapshot(&mut self, sid: SquadId, pos: Vec3) -> Option<(FightSnapshot, SnapshotNotes)> {
        let frame = self.frame;
        let mut ours: Vec<(i32, UnitDef, f32, Vec3)> = Vec::new();
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
            ours.push((*id, def, hp, f.unit.pos()));
        }
        let mut theirs: Vec<(i32, UnitDef, f32, Vec3, bool, bool)> = Vec::new();
        for (&eid, e) in &self.war.targets {
            if e.is_dead
                || !e.identified
                || frame - e.last_seen > SEEN_WITHIN
                || distance(e.position, pos) > ENEMY_REACH
            {
                continue;
            }
            let Some(def) = e.ud else { continue };
            theirs.push((eid, def, e.get_health() * e.def_health(), e.position, e.can_fly(), e.is_static));
        }
        let mut snap = FightSnapshot::default();
        let mut notes = SnapshotNotes {
            members: Vec::new(),
            enemies: Vec::new(),
            metal: [0.0; 2],
            air_share: 0.0,
            static_share: 0.0,
            n: [0; 2],
        };
        for (id, def, hp, p) in ours {
            let spec = self.fight_spec(def);
            notes.members.push((id, spec.cost));
            notes.metal[0] += spec.cost;
            notes.n[0] += 1;
            snap.units.push(SnapUnit { side: 0, spec, hp, x: p.x, z: p.z });
        }
        let (mut air, mut statics) = (0.0, 0.0);
        for (eid, def, hp, p, flies, is_static) in theirs {
            let spec = self.fight_spec(def);
            notes.enemies.push((eid, spec.cost));
            notes.metal[1] += spec.cost;
            notes.n[1] += 1;
            if flies {
                air += spec.cost;
            }
            if is_static {
                statics += spec.cost;
            }
            snap.units.push(SnapUnit { side: 1, spec, hp, x: p.x, z: p.z });
        }
        if notes.metal[1] > 0.0 {
            notes.air_share = air / notes.metal[1];
            notes.static_share = statics / notes.metal[1];
        }
        (snap.armed(1) > 0).then_some((snap, notes))
    }

    /// Whether an attacking squad at `pos` should fall back. The predictor
    /// says yes when it expects the squad to lose the fight it is in
    /// (calibrated P(A wins) below 1/2 over the squad's courage) and to lose
    /// at least [`MIN_LOSS`] of its own cost. `flat` is the threat map's
    /// verdict, which stands wherever the predictor has nothing fresh to
    /// say, and everywhere while it only shadows. The mechanics model's
    /// answer, when on, is logged beside it with its own verdict by the
    /// same rule.
    pub fn fight_verdict(&mut self, sid: SquadId, pos: Vec3, courage: f32, flat: bool) -> bool {
        if self.war.fightsim.worker.is_none() {
            return flat;
        }
        let drives = self.fightsim_drives();
        let mech = !self.dev_off(MECH);
        let frame = self.frame;
        self.war.fightsim.verdicts += 1;
        if flat {
            self.war.fightsim.flat_retreats += 1;
        }
        let Some((snapshot, notes)) = self.fight_snapshot(sid, pos) else {
            self.war.fightsim.no_contact += 1;
            return flat;
        };
        let armed = (snapshot.armed(0), snapshot.armed(1));
        let fs = &mut self.war.fightsim;
        if let Some(w) = fs.worker.as_mut() {
            w.request(FightRequest { key: sid, frame, snapshot, mech });
        }
        let Some(a) = fs.answers.get(&sid).filter(|a| frame - a.frame <= STALE) else {
            fs.fallbacks += 1;
            return flat;
        };
        let o = a.outcome;
        let falls_back = |p_a_wins: f32, mean_a: f32| p_a_wins < 0.5 / courage.max(0.1) && mean_a >= MIN_LOSS;
        let retreat = falls_back(o.p_a_wins, o.mean_a);
        if retreat {
            fs.sim_retreats += 1;
        }
        let agree = retreat == flat;
        if agree {
            fs.agreements += 1;
        } else {
            fs.disagreements += 1;
        }
        if !agree || fs.agreements.is_multiple_of(AGREE_SAMPLE) {
            let vid = fs.next_vid;
            fs.next_vid += 1;
            let age = frame - a.frame;
            let enemies = notes.enemies.iter().map(|&(id, metal)| (id, metal, false)).collect();
            fs.windows.push(Window {
                vid,
                squad: sid,
                frame,
                members: notes.members,
                metal: notes.metal[0],
                enemies,
                agree,
            });
            let mut rec = json!({
                "event": "verdict", "vid": vid, "t": frame / 30, "squad": sid, "agree": agree,
                "mode": if drives { "drive" } else { "shadow" },
                "age": age, "n": notes.n, "armed": [armed.0, armed.1], "metal": notes.metal,
                "b_air_share": notes.air_share, "b_static_share": notes.static_share,
                "p_a_wins": o.p_a_wins, "killed": [o.killed_a, o.killed_b], "mean": [o.mean_a, o.mean_b],
                "withdraw": [o.withdraw_a, o.withdraw_b], "sim": retreat, "flat": flat, "courage": courage,
            });
            if let Some(m) = o.mech {
                rec["mech_p_a_wins"] = json!(m.p_a_wins);
                rec["mech_killed"] = json!([m.killed_a, m.killed_b]);
                rec["mech_mean"] = json!([m.mean_a, m.mean_b]);
                rec["mech_withdraw"] = json!([m.withdraw_a, m.withdraw_b]);
                rec["mech_sim"] = json!(falls_back(m.p_a_wins, m.mean_a));
            }
            self.op_emit("fightsim", rec);
        }
        if drives { retreat } else { flat }
    }
}
