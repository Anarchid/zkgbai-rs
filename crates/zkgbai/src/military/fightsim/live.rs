//! The fight predictor in play: snapshots from the AI's own state, the
//! worker's answers, ordinary danger checks and fire-squad commitments.
//!
//! `fightsim`, on by default, vetoes only crushing defeats: at least 75%
//! of our armed metal lost, at most 5% of theirs, and a return no greater
//! than one tenth of our losses. It never overrides an existing threat
//! retreat or requires an otherwise acceptable attack to predict a win.
//! `fightshadow` keeps logging when the veto is disabled; `fightmech`
//! selects the mechanics answer when available (the base model otherwise).
//! Ordinary assault/raider attacks use this veto, including on home ground.
//! Strike missions and other special controllers keep their own conduct.
//! Side A includes nearby friendly support; side B is fresh identified
//! opposition on the approach. Stale or changed encounters cannot veto.
//! Fire squads separately use committed per-unit fates; their policy is
//! unaffected, and `firesquads` can keep the worker running independently.
//!
//! Journal (`k=fightsim`): `event: cost` every minute; `event: verdict`
//! whenever the two rules disagree, with both answers and the fight as the
//! predictor saw it (`agree: false`), and for one agreement in
//! [`AGREE_SAMPLE`] the same record with `agree: true`; `event: window`
//! 30 s after each verdict record with the squad's metal lost meanwhile
//! and what the enemies in the snapshot lost (seen dying), the mechanism
//! the arm reads.

use std::collections::{BTreeMap, HashMap};
use std::hash::{Hash, Hasher};
use std::sync::Arc;

use serde_json::json;
use springai::{UnitDef, Vec3};

use super::worker::{FightAnswer, FightRequest, FightWorker};
use super::{FightSnapshot, Model, SnapUnit, UnitSpec, WeaponSpec};
use crate::Ai;
use crate::military::{SquadId, SquadKind};
use crate::util::{NULLPOS, distance};

/// Default-on veto (`disable=fightsim` restores ordinary threat policy).
pub const FEATURE: &str = "fightsim";
/// The ordinary shadow switch; fire squads also need the worker.
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
const SEEN_WITHIN: i32 = 90;
/// Never truncate a large battle into a misleading smaller one.
const MAX_UNITS: usize = 96;
/// Frames between cost lines in the journal.
const REPORT_PERIOD: i32 = 1800;
/// Below this expected own loss share a predicted defeat is no reason to leave.
const MIN_LOSS: f32 = 0.75;
/// A veto requires negligible enemy losses, both fractionally and in metal.
const MAX_ENEMY_LOSS: f32 = 0.05;
const MAX_RETURN: f32 = 0.10;
/// Frames after a verdict over which the squad's losses are read.
const WINDOW: i32 = 900;
/// One agreement with the flat rule in this many is logged, with a window.
const AGREE_SAMPLE: u64 = 5;

fn crushing_defeat(ours: f32, theirs: f32, metal: [f32; 2]) -> bool {
    [ours, theirs].iter().all(|v| v.is_finite() && (0.0..=1.0).contains(v))
        && metal.iter().all(|v| v.is_finite() && *v > 0.0)
        && ours >= MIN_LOSS
        && theirs <= MAX_ENEMY_LOSS
        && theirs * metal[1] <= MAX_RETURN * ours * metal[0]
}

fn apply_veto(flat: bool, enabled: bool, crushing: bool) -> bool {
    flat || (enabled && crushing)
}

fn ordinary_veto_kind(kind: SquadKind, air: bool, strike: bool) -> bool {
    !air && !strike && matches!(kind, SquadKind::Assault | SquadKind::Hover | SquadKind::Raider)
}

fn ordinary_answer_usable(a: &FightAnswer, frame: i32, stamp: u64) -> bool {
    !a.commit && (0..=STALE).contains(&(frame - a.frame)) && a.stamp == stamp
}

fn hash_unit(h: &mut impl Hasher, side: u8, id: i32, def: i32, hp: f32, max_hp: f32, p: Vec3) {
    (side, id, def, (hp / max_hp.max(1.0) * 10.0) as i32, (p.x / 256.0) as i32, (p.z / 256.0) as i32).hash(h);
}

/// Only the next stretch of an attack matters, including enemy firing reach.
fn on_approach(p: Vec3, origin: Vec3, target: Vec3, radius: f32) -> bool {
    let dx = target.x - origin.x;
    let dz = target.z - origin.z;
    let len = dx.hypot(dz);
    let t = (((p.x - origin.x) * dx + (p.z - origin.z) * dz) / (len * len).max(1.0))
        .clamp(0.0, (ENEMY_REACH / len.max(1.0)).min(1.0));
    distance(p, Vec3::xz(origin.x + t * dx, origin.z + t * dz)) <= radius
}

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
    commits: BTreeMap<SquadId, FightAnswer>,
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
    armed_metal: [f32; 2],
    stamp: u64,
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
        self.fightsim_drives() || !self.dev_off(SHADOW) || !self.dev_off("firesquads")
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
                    let mode =
                        if self.fightsim_drives() { "crushing-defeat veto" } else { "ordinary squad checks in shadow" };
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
            let answers = if a.commit { &mut fs.commits } else { &mut fs.answers };
            if answers.get(&a.key).is_none_or(|old| a.frame >= old.frame) {
                answers.insert(a.key, a);
            }
        }
        fs.answers.retain(|_, a| frame - a.frame <= 10 * STALE);
        fs.commits.retain(|_, a| frame - a.frame <= STALE);
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
            "event": "cost", "t": frame / 30, "mode": if drives { "veto" } else { "shadow" },
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
    pub(crate) fn fight_spec(&mut self, def: UnitDef) -> Arc<UnitSpec> {
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
    fn fight_snapshot(&mut self, sid: SquadId, pos: Vec3, target: Vec3) -> Option<(FightSnapshot, SnapshotNotes)> {
        let frame = self.frame;
        let mut ours: Vec<(i32, UnitDef, f32, Vec3)> = Vec::new();
        let squad = self.squads.get(sid)?;
        if !ordinary_veto_kind(squad.kind, squad.is_air_squad, self.squad_strike_active(sid)) || target == NULLPOS {
            return None;
        }
        let members: Vec<i32> = squad.fighters.iter().copied().chain(squad.leader).collect();
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
            // Shields, cloaked ambushes and submerged fights are not simulated.
            // Dedicated suicide controllers do not call the veto; bombs in a
            // mixed ordinary squad must not cause it to reject their tactic.
            if members.contains(id)
                && (f.unit.is_cloaked()
                    || f.get_pos().y < -5.0
                    || matches!(
                        self.defs.name(def),
                        "cloakbomb" | "shieldbomb" | "jumpbomb" | "amphbomb" | "gunshipbomb" | "subscout"
                    ))
            {
                return None;
            }
            if def.weapon_mounts().iter().filter_map(|m| m.weapon_def()).any(|w| w.is_shield()) {
                return None;
            }
            ours.push((*id, def, hp, f.unit.pos()));
        }
        let mut theirs: Vec<(i32, UnitDef, f32, Vec3, bool, bool)> = Vec::new();
        for (&eid, e) in &self.war.targets {
            if e.is_dead
                || !e.identified
                || frame - e.last_seen > SEEN_WITHIN
                || !on_approach(e.position, pos, target, e.def_max_weapon_range().max(350.0) + 200.0)
            {
                continue;
            }
            let Some(def) = e.ud else { continue };
            if e.position.y < -5.0 {
                return None;
            }
            theirs.push((eid, def, e.get_health() * e.def_health(), e.position, e.can_fly(), e.is_static));
        }
        if ours.len() + theirs.len() > MAX_UNITS {
            return None;
        }
        let mut hash = std::collections::hash_map::DefaultHasher::new();
        ((target.x / 256.0) as i32, (target.z / 256.0) as i32).hash(&mut hash);
        let mut snap = FightSnapshot::default();
        let mut notes = SnapshotNotes {
            members: Vec::new(),
            enemies: Vec::new(),
            metal: [0.0; 2],
            armed_metal: [0.0; 2],
            stamp: 0,
            air_share: 0.0,
            static_share: 0.0,
            n: [0; 2],
        };
        for (id, def, hp, p) in ours {
            let spec = self.fight_spec(def);
            hash_unit(&mut hash, 0, id, def.id(), hp, spec.health, p);
            if !spec.weapons.is_empty() {
                notes.armed_metal[0] += spec.cost;
            }
            notes.members.push((id, spec.cost));
            notes.metal[0] += spec.cost;
            notes.n[0] += 1;
            snap.units.push(SnapUnit { side: 0, spec, hp, x: p.x, z: p.z });
        }
        let (mut air, mut statics) = (0.0, 0.0);
        for (eid, def, hp, p, flies, is_static) in theirs {
            let spec = self.fight_spec(def);
            hash_unit(&mut hash, 1, eid, def.id(), hp, spec.health, p);
            if !spec.weapons.is_empty() {
                notes.armed_metal[1] += spec.cost;
            }
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
        notes.stamp = hash.finish();
        (snap.armed(0) > 0 && snap.armed(1) > 0).then_some((snap, notes))
    }

    /// Add a veto for crushing defeats to the complete ordinary threat rule.
    /// An uncertain or merely losing fight never adds a refusal. `flat` must
    /// already include the caller's territorial exceptions, so home ground
    /// cannot exempt an otherwise suicidal engagement from the veto.
    pub fn fight_verdict(&mut self, sid: SquadId, pos: Vec3, target: Vec3, courage: f32, flat: bool) -> bool {
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
        let Some((snapshot, notes)) = self.fight_snapshot(sid, pos, target) else {
            self.war.fightsim.no_contact += 1;
            return flat;
        };
        let armed = (snapshot.armed(0), snapshot.armed(1));
        let fs = &mut self.war.fightsim;
        if let Some(w) = fs.worker.as_mut() {
            w.request(FightRequest { key: sid, frame, snapshot, mech, commit: false, stamp: notes.stamp });
        }
        let Some(a) = fs.answers.get(&sid).filter(|a| ordinary_answer_usable(a, frame, notes.stamp)) else {
            fs.fallbacks += 1;
            return flat;
        };
        let o = a.outcome;
        let (loss_a, loss_b) = if mech {
            o.mech.map_or((o.killed_a, o.killed_b), |m| (m.killed_a, m.killed_b))
        } else {
            (o.killed_a, o.killed_b)
        };
        let retreat = crushing_defeat(loss_a, loss_b, notes.armed_metal);
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
                "mode": if drives { "veto" } else { "shadow" },
                "age": age, "n": notes.n, "armed": [armed.0, armed.1], "metal": notes.metal,
                "b_air_share": notes.air_share, "b_static_share": notes.static_share,
                "p_a_wins": o.p_a_wins, "killed": [o.killed_a, o.killed_b], "mean": [o.mean_a, o.mean_b],
                "withdraw": [o.withdraw_a, o.withdraw_b], "sim": retreat, "flat": flat, "courage": courage,
                "armed_metal": notes.armed_metal, "veto": drives && retreat && !flat,
            });
            if let Some(m) = o.mech {
                rec["mech_p_a_wins"] = json!(m.p_a_wins);
                rec["mech_killed"] = json!([m.killed_a, m.killed_b]);
                rec["mech_mean"] = json!([m.mean_a, m.mean_b]);
                rec["mech_withdraw"] = json!([m.withdraw_a, m.withdraw_b]);
                rec["mech_sim"] = json!(crushing_defeat(m.killed_a, m.killed_b, notes.armed_metal));
            }
            self.op_emit("fightsim", rec);
        }
        apply_veto(flat, drives, retreat)
    }
}

impl Ai {
    /// This squad alone, against the actual local contacts. Never borrow
    /// the ordinary query (which includes allies and early disengagement).
    /// Missing, stale, or materially changed snapshots cannot approve a charge.
    pub(crate) fn fire_commit_favorable(&mut self, sid: SquadId, enemies: &[i32]) -> bool {
        use std::hash::{Hash, Hasher};
        if self.dev_off("firesquads") || self.war.fightsim.worker.is_none() {
            return false;
        }
        let Some(s) = self.squads.get(sid) else {
            return false;
        };
        let mut raw = Vec::new();
        for id in &s.fighters {
            let Some(f) = self.fighters.get(id).filter(|f| !f.is_dead()) else {
                continue;
            };
            if self.retreat_handler.is_retreating(*id) {
                continue;
            }
            if let Some(d) = f.unit.def() {
                raw.push((
                    0u8,
                    *id,
                    d,
                    (f.unit.health() - f.unit.paralyze_damage()).max(0.0) * (1.0 - f.unit.capture_progress()),
                    f.unit.pos(),
                ));
            }
        }
        for id in enemies {
            let Some(e) = self.war.targets.get(id) else {
                continue;
            };
            if let Some(d) = e.ud {
                raw.push((1u8, *id, d, e.get_health() * e.def_health(), e.position));
            }
        }
        let mut hash = std::collections::hash_map::DefaultHasher::new();
        let mut snapshot = FightSnapshot::default();
        let mut metal = [0.0; 2];
        for (side, id, d, hp, p) in raw {
            // A casualty, reinforcement, substantial damage, or movement
            // invalidates the old result even inside the freshness window.
            (side, id, d.id(), (hp / d.health().max(1.0) * 5.0) as i32, (p.x / 256.0) as i32, (p.z / 256.0) as i32)
                .hash(&mut hash);
            let spec = self.fight_spec(d);
            metal[side as usize] += spec.cost;
            snapshot.units.push(SnapUnit { side, spec, hp, x: p.x, z: p.z });
        }
        if snapshot.armed(0) == 0 || snapshot.armed(1) == 0 {
            return false;
        }
        let stamp = hash.finish();
        let fs = &mut self.war.fightsim;
        let favorable = fs
            .commits
            .get(&sid)
            .filter(|a| commit_answer_usable(a, self.frame, stamp))
            .is_some_and(|a| favorable_commit(a.outcome, metal));
        if let Some(w) = fs.worker.as_mut() {
            w.request(FightRequest { key: sid, frame: self.frame, commit: true, stamp, snapshot, mech: true });
        }
        favorable
    }
}

fn favorable_commit(o: super::FightOutcome, metal: [f32; 2]) -> bool {
    o.killed_b >= 0.10 && o.killed_b * metal[1] > 1.15 * o.killed_a * metal[0]
}

#[cfg(test)]
mod fire_tests {
    use super::*;
    #[test]
    fn commit_requires_damage_and_a_favorable_metal_exchange() {
        let o = |a, b| super::super::FightOutcome { killed_a: a, killed_b: b, ..Default::default() };
        assert!(!favorable_commit(o(0.0, 0.0), [1000.0, 1000.0]));
        assert!(favorable_commit(o(0.2, 0.8), [1000.0, 1000.0]));
        assert!(!favorable_commit(o(0.2, 0.8), [5000.0, 1000.0]));
        assert!(favorable_commit(o(0.9, 1.0), [1000.0, 2000.0]));
    }
}

fn commit_answer_usable(a: &FightAnswer, frame: i32, stamp: u64) -> bool {
    a.commit && (0..=STALE).contains(&(frame - a.frame)) && a.stamp == stamp
}

#[cfg(test)]
mod freshness_tests {
    use super::*;
    #[test]
    fn old_rosters_and_ordinary_answers_cannot_authorize_commitment() {
        let mut a = FightAnswer { key: 1, frame: 100, commit: true, stamp: 42, outcome: Default::default(), micros: 0 };
        assert!(commit_answer_usable(&a, 130, 42));
        assert!(!commit_answer_usable(&a, 130, 43));
        assert!(!commit_answer_usable(&a, 191, 42));
        assert!(!commit_answer_usable(&a, 99, 42));
        a.commit = false;
        assert!(!commit_answer_usable(&a, 130, 42));
    }
}

#[cfg(test)]
mod veto_tests {
    use super::*;

    #[test]
    fn strike_and_special_tactics_are_exempt() {
        for kind in [SquadKind::Assault, SquadKind::Hover, SquadKind::Raider] {
            assert!(ordinary_veto_kind(kind, false, false));
            assert!(!ordinary_veto_kind(kind, false, true));
            assert!(!ordinary_veto_kind(kind, true, false));
        }
        assert!(!ordinary_veto_kind(SquadKind::Jaeger, false, false));
        assert!(!ordinary_veto_kind(SquadKind::Taskforce, false, false));
    }

    #[test]
    fn veto_is_default_on_and_never_replaces_the_normal_retreat_rule() {
        assert!(!crate::ai::OFF_BY_DEFAULT.contains(&FEATURE));
        for flat in [false, true] {
            assert_eq!(apply_veto(flat, false, true), flat);
            assert_eq!(apply_veto(flat, true, false), flat);
            assert!(apply_veto(flat, true, true));
        }
    }

    #[test]
    fn only_crushing_losses_with_negligible_return_are_vetoed() {
        assert!(crushing_defeat(0.95, 0.0, [1500.0, 5000.0]));
        assert!(crushing_defeat(0.75, 0.05, [1500.0, 2000.0]));
        assert!(!crushing_defeat(0.74, 0.0, [1500.0, 5000.0]));
        assert!(!crushing_defeat(0.95, 0.06, [1500.0, 1500.0]));
        assert!(!crushing_defeat(0.95, 0.8, [1500.0, 1500.0]));
        assert!(!crushing_defeat(0.1, 0.0, [1500.0, 5000.0]));
        assert!(!crushing_defeat(0.95, 0.05, [1500.0, 100000.0]));
        assert!(!crushing_defeat(f32::NAN, 0.0, [1500.0, 5000.0]));
        assert!(!crushing_defeat(1.0, -0.1, [1500.0, 5000.0]));
        assert!(!crushing_defeat(1.0, 0.0, [0.0, 5000.0]));
    }

    #[test]
    fn stale_changed_and_commit_answers_cannot_veto() {
        let mut a =
            FightAnswer { key: 1, frame: 100, commit: false, stamp: 42, outcome: Default::default(), micros: 0 };
        assert!(ordinary_answer_usable(&a, 130, 42));
        assert!(!ordinary_answer_usable(&a, 130, 43));
        assert!(!ordinary_answer_usable(&a, 191, 42));
        assert!(!ordinary_answer_usable(&a, 99, 42));
        a.commit = true;
        assert!(!ordinary_answer_usable(&a, 130, 42));
    }

    #[test]
    fn casualties_reinforcements_damage_and_movement_change_the_stamp() {
        let stamp = |id, hp, x| {
            let mut h = std::collections::hash_map::DefaultHasher::new();
            hash_unit(&mut h, 0, id, 1, hp, 100.0, Vec3::xz(x, 0.0));
            h.finish()
        };
        let original = stamp(1, 100.0, 0.0);
        assert_ne!(original, stamp(2, 100.0, 0.0));
        assert_ne!(original, stamp(1, 70.0, 0.0));
        assert_ne!(original, stamp(1, 100.0, 300.0));
    }

    #[test]
    fn approach_is_local_and_directional() {
        let o = Vec3::xz(0.0, 0.0);
        let t = Vec3::xz(5000.0, 0.0);
        assert!(on_approach(Vec3::xz(900.0, 100.0), o, t, 600.0));
        assert!(!on_approach(t, o, t, 600.0));
        assert!(!on_approach(Vec3::xz(-1000.0, 0.0), o, t, 600.0));
        assert!(!on_approach(Vec3::xz(500.0, 700.0), o, t, 600.0));
        assert!(on_approach(o, o, o, 600.0));
    }

    fn fixture(name: &str) -> Arc<UnitSpec> {
        let row = include_str!("testdata/mech_parity_units.tsv")
            .lines()
            .find(|l| l.starts_with(&format!("{name}\t")))
            .unwrap();
        let f: Vec<_> = row.split('\t').collect();
        Arc::new(UnitSpec {
            def: name.into(),
            cost: f[1].parse().unwrap(),
            health: f[2].parse().unwrap(),
            speed: f[3].parse().unwrap(),
            mobile: f[4] == "1",
            radius: f[8].parse().unwrap(),
            weapons: f[9]
                .split('|')
                .map(|w| {
                    let p: Vec<_> = w.split(':').collect();
                    WeaponSpec {
                        name: p[0].into(),
                        weapon_type: p[1].into(),
                        range: p[2].parse().unwrap(),
                        reload: p[3].parse().unwrap(),
                        salvo: p[4].parse().unwrap(),
                        damage: p[5].parse().unwrap(),
                    }
                })
                .collect(),
            ..Default::default()
        })
    }

    fn encounter(ours: usize, theirs: usize) -> FightSnapshot {
        let mut units = Vec::new();
        for (side, n, name, x) in [(0, ours, "cloakassault", 0.0), (1, theirs, "hoverskirm", 600.0)] {
            let spec = fixture(name);
            for i in 0..n {
                units.push(SnapUnit { side, hp: spec.health, spec: spec.clone(), x, z: i as f32 * 40.0 });
            }
        }
        FightSnapshot { units }
    }

    #[test]
    fn scalpel_pack_prediction() {
        let model = Model::load().unwrap();
        // A feeding case, an unfavorable but useful fight, and reinforcements
        // turning the matchup around. Neither of the last two should veto.
        for (ours, theirs, veto) in [(3, 20, true), (3, 10, false), (10, 3, false)] {
            let snapshot = encounter(ours, theirs);
            let out = model.predict_with(&snapshot, true).mech.unwrap();
            let metal = [ours as f32 * 350.0, theirs as f32 * 220.0];
            assert_eq!(
                crushing_defeat(out.killed_a, out.killed_b, metal),
                veto,
                "{ours} Knights vs {theirs} Scalpels: {out:?}"
            );
        }
    }
}
