//! The fight predictor in play: snapshots from the AI's own state, the
//! crushing-defeat veto on ordinary squads, and fire-squad commitments.
//!
//! `fightsim`, on by default, vetoes only crushing defeats: at least 75%
//! of our armed metal lost, at most 5% of theirs, and a return no greater
//! than one tenth of our losses, over [`VETO_HORIZON`] (15 s) of the
//! mechanics fight with no stopping rule and ZK's tactical unit AI moving
//! the units it governs ([`Model::kiting_fates`]: a Scalpel keeps its
//! range from a Knight). Losses are the units' raw death probabilities
//! weighted by metal. The fight is judged on the game thread at every
//! check, so a moving squad always has an answer. The veto never overrides
//! an existing threat retreat or requires an otherwise acceptable attack
//! to predict a win.
//!
//! A veto latches: the squad keeps retreating for at least [`LATCH_MIN`],
//! then while the fight against the enemies that set it (remembered for
//! [`LATCH_MEMORY`] after last seen, wherever they are), judged as if the
//! squad alone stood again where the veto stopped it, is still crushing,
//! for at most [`LATCH_MAX`]: the squad growing or the enemy leaving ends
//! it; the retreat's own distance and what stands by it at the haven do not. `fightshadow` keeps judging and logging when
//! the veto is disabled. Ordinary assault/raider attacks use this veto,
//! including on home ground. Strike missions and other special controllers
//! keep their own conduct. Side A includes nearby friendly support; side B
//! is fresh identified opposition on the approach. Fire squads separately
//! use committed per-unit fates from the worker thread; their policy is
//! unaffected, and `firesquads` can keep the worker running independently.
//!
//! Journal (`k=fightsim`): `event: cost` every minute; `event: verdict`
//! whenever the veto and the flat rule disagree, with the judged losses
//! and the fight as it was seen (`agree: false`), and for one agreement in
//! [`AGREE_SAMPLE`] the same record with `agree: true`; `event: release`
//! when a latch ends; `event: window` 30 s after each verdict record with
//! the squad's metal lost meanwhile and what the enemies in the snapshot
//! lost (seen dying), the mechanism the arm reads.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, OnceLock};
use std::time::Instant;

use serde_json::json;
use springai::{Unit, UnitDef, Vec3};

use super::worker::{FightAnswer, FightRequest, FightWorker};
use super::{FightOutcome, FightSnapshot, Model, SnapUnit, UnitFate, UnitSpec, WeaponSpec};
use crate::Ai;
use crate::military::{SquadId, SquadKind};
use crate::util::{NULLPOS, distance};

/// Default-on veto (`disable=fightsim` restores ordinary threat policy).
pub const FEATURE: &str = "fightsim";
/// The ordinary shadow switch; fire squads also need the worker.
pub const SHADOW: &str = "fightshadow";
/// Commanders fight with the weapons they carry (`disable=comspec`: every
/// weapon their def mounts; see [`Ai::fight_spec_of`]).
pub const COMSPEC: &str = "comspec";
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
/// Frames of fight the veto judges (15 s): long enough for an outranged
/// squad to be shot to pieces, short enough to leave a slow fight alone.
pub const VETO_HORIZON: i32 = 450;
/// A vetoed squad keeps retreating at least this long (frames, 10 s) ...
pub const LATCH_MIN: i32 = 300;
/// ... and is judged afresh, as on first contact, after at most this long.
pub const LATCH_MAX: i32 = 1800;
/// A latch remembers the enemies that set it this long after they were
/// last seen (frames, 20 s).
pub const LATCH_MEMORY: i32 = 600;

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

/// The veto's model, shared by every AI in the process and loaded once.
fn veto_model() -> Result<&'static Model, &'static str> {
    static MODEL: OnceLock<Result<Model, String>> = OnceLock::new();
    MODEL.get_or_init(Model::load).as_ref().map_err(String::as_str)
}

/// Killed share of each side's armed metal by the horizon, from the units'
/// raw death probabilities (the calibrated ones are squeezed towards the
/// middle), and each side's armed metal.
fn kiting_losses(snap: &FightSnapshot, fates: &[UnitFate]) -> ([f32; 2], [f32; 2]) {
    let (mut lost, mut metal) = ([0.0f32; 2], [0.0f32; 2]);
    for (u, f) in snap.units.iter().zip(fates) {
        if !u.spec.weapons.is_empty() {
            metal[u.side as usize] += u.spec.cost;
            lost[u.side as usize] += u.spec.cost * f.p_raw;
        }
    }
    ([0, 1].map(|s| if metal[s] > 0.0 { lost[s] / metal[s] } else { 0.0 }), metal)
}

/// The veto's question: does side A a-moving into `snap`, the tactical AI
/// moving what it governs, lose crushingly within [`VETO_HORIZON`]? The
/// killed shares and the answer.
pub(crate) fn judge(model: &Model, snap: &FightSnapshot) -> ([f32; 2], bool) {
    let (loss, metal) = kiting_losses(snap, &model.kiting_fates(snap, VETO_HORIZON));
    (loss, crushing_defeat(loss[0], loss[1], metal))
}

/// Whether a latched retreat `age` frames old goes on: always before
/// [`LATCH_MIN`], then while the remembered fight is still crushing, never
/// past [`LATCH_MAX`]. `crushing` is asked only when it matters.
fn latch_holds(age: i32, crushing: impl FnOnce() -> bool) -> bool {
    age < LATCH_MIN || (age < LATCH_MAX && crushing())
}

/// A vetoed squad's retreat.
struct Latch {
    since: i32,
    /// The enemies of the vetoed fight.
    enemies: Vec<i32>,
    /// Where the squad was when vetoed.
    at: Vec3,
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

/// A dynamic commander's weapons in its two slots (weapon def id, manual
/// fire), read off the unit (`None` when unreadable: out of LOS).
pub(crate) fn read_loadout(unit: Option<Unit>) -> Option<Vec<(i32, bool)>> {
    let u = unit?;
    let l: Vec<(i32, bool)> = (1..=2)
        .filter_map(|slot| {
            let wid = u.rules_param_float(&format!("comm_weapon_id_{slot}"), 0.0) as i32;
            let manual = u.rules_param_float(&format!("comm_weapon_manual_{slot}"), 0.0) > 0.0;
            (wid > 0).then_some((wid, manual))
        })
        .collect();
    (!l.is_empty()).then_some(l)
}

/// Of a commander def's weapons `all` (one per weapon def, as built from its
/// mounts), those it fires: the weapon defs `fired`, or with the loadout
/// unknown (`None`) one beam laser (else the first).
pub(crate) fn commander_weapons(all: &[WeaponSpec], def: UnitDef, fired: Option<&[i32]>) -> Vec<WeaponSpec> {
    let names: Option<Vec<String>> = fired.map(|fired| {
        (def.weapon_mounts().iter())
            .filter_map(|m| m.weapon_def())
            .filter(|wd| fired.contains(&wd.id()))
            .map(|wd| wd.name())
            .collect()
    });
    pick_weapons(all, names.as_deref())
}

/// [`commander_weapons`] by the names of the weapon defs fired.
fn pick_weapons(all: &[WeaponSpec], fired: Option<&[String]>) -> Vec<WeaponSpec> {
    let Some(fired) = fired else {
        let beam = all.iter().find(|w| w.name.ends_with("commweapon_beamlaser"));
        return beam.or(all.first()).cloned().into_iter().collect();
    };
    all.iter().filter(|w| fired.contains(&w.name)).cloned().collect()
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
    /// Dynamic commanders' weapons (weapon def id, manual fire), as last
    /// read off the unit, by unit id; and their specs by def and the weapon
    /// def ids it fires (see [`Ai::fight_spec_of`]).
    loadouts: HashMap<i32, Vec<(i32, bool)>>,
    com_specs: HashMap<(i32, Vec<i32>), Arc<UnitSpec>>,
    commits: BTreeMap<SquadId, FightAnswer>,
    /// Squads held in retreat by a veto.
    latches: BTreeMap<SquadId, Latch>,
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
    /// Checks a latch answered, and latches ended.
    pub latch_holds: u64,
    pub latch_releases: u64,
    /// Fights judged on the game thread, their total and worst microseconds.
    judged: u64,
    judge_us: u64,
    judge_max_us: u64,
    last_report: i32,
    reported: (u64, u64, u64),
    /// The model for synchronous local judgements (see `local`), loaded
    /// once on first use.
    pub(crate) local_model: Option<Option<Arc<Model>>>,
}

/// Side A of a snapshot as the window needs it, and the sides' metal.
struct SnapshotNotes {
    members: Vec<(i32, f32)>,
    /// Side B's unit ids and metal.
    enemies: Vec<(i32, f32)>,
    metal: [f32; 2],
    armed_metal: [f32; 2],
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
                    // The veto judges on this thread: load its model now, not at first contact.
                    if let Err(e) = veto_model() {
                        self.log(&format!("fightsim: the veto's model failed ({e}); squads keep the flat rule"));
                    }
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
            if a.commit && fs.commits.get(&a.key).is_none_or(|old| a.frame >= old.frame) {
                fs.commits.insert(a.key, a);
            }
        }
        fs.commits.retain(|_, a| frame - a.frame <= STALE);
        // Squads gone: a living squad's latch ends at its next check, logged.
        fs.latches.retain(|_, l| frame - l.since <= 2 * LATCH_MAX);
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

    /// The cost line: the worker's queries per minute (fire squads) with
    /// their mean and worst microseconds, the veto's judgements on this
    /// thread with theirs, refusals, fallbacks, disagreements with the flat
    /// rule and latches.
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
        let judge_mean_us = if fs.judged > 0 { fs.judge_us as f64 / fs.judged as f64 } else { 0.0 };
        let data = json!({
            "event": "cost", "t": frame / 30, "mode": if drives { "veto" } else { "shadow" },
            "queries_per_min": dq as f64 / minutes, "mean_us": mean_us, "max_us": s.max_micros,
            "queue_full": s.full, "verdicts": fs.verdicts, "no_contact": fs.no_contact, "fallbacks": fs.fallbacks,
            "sim_retreats": fs.sim_retreats, "flat_retreats": fs.flat_retreats, "disagreements": fs.disagreements,
            "agreements": fs.agreements, "judged": fs.judged, "judge_mean_us": judge_mean_us,
            "judge_max_us": fs.judge_max_us, "latch_holds": fs.latch_holds, "latch_releases": fs.latch_releases,
        });
        let line = format!(
            "fightsim: {} fights judged, {judge_mean_us:.0} us mean ({} max); {} verdicts, {} without contact, {} fallbacks; retreats sim {} flat {}, {} disagreements; latches held {} checks, {} released; worker {:.0} queries/min, {mean_us:.0} us mean",
            fs.judged,
            fs.judge_max_us,
            fs.verdicts,
            fs.no_contact,
            fs.fallbacks,
            fs.sim_retreats,
            fs.flat_retreats,
            fs.disagreements,
            fs.latch_holds,
            fs.latch_releases,
            dq as f64 / minutes,
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
                // Shots per reload as the hit tables count them: projectiles per
                // shot times the burst (a Scalpel fires two missiles a volley; the
                // engine's salvo size alone, its burst, is one).
                salvo: wd.salvo_size().max(1) * wd.projectiles_per_shot().max(1),
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

    /// Unit `id` (ours or an enemy's) as the simulation sees it: its def's
    /// spec, except for a dynamic commander.
    ///
    /// A dynamic commander's def mounts every weapon its chassis can take
    /// (ZK's dyncomm_chassis_generator: 26 mounts on the support chassis,
    /// about 2000 dps of weapon kinds, multistunner and disruptor bomb
    /// included), and [`Ai::fight_spec`] armed it with all of them. The
    /// commander fires the one or two its modules gave it, which ZK's
    /// commander script names in rules params (`comm_weapon_id_1/2`, and
    /// `comm_weapon_manual_1/2` for manual fire; in LOS for enemies). A base
    /// support trainer fires one light particle beam (about 190 dps). With
    /// every mount the sim said our commander survives 2 Maces or 8 Glaives,
    /// which kill it with its real gun (2026-11-03).
    ///
    /// The loadout last read off the unit stands in while it cannot be read;
    /// a commander never read fires one beam laser, as the fitted model
    /// counted commanders. Our commanders' manual weapons are left out: this
    /// AI never orders them fired. `disable=comspec` restores the def's spec.
    pub(crate) fn fight_spec_of(&mut self, id: i32, def: UnitDef) -> Arc<UnitSpec> {
        if !self.defs.is_commander(def) || self.dev_off(COMSPEC) {
            return self.fight_spec(def);
        }
        let read = read_loadout(Unit::from_id(self.cb, id));
        let fs = &mut self.war.fightsim;
        let changed = read.as_ref().is_some_and(|l| fs.loadouts.get(&id) != Some(l));
        if let Some(l) = &read {
            fs.loadouts.insert(id, l.clone());
        }
        let fired = self.commander_fired(id, read);
        let key = (def.id(), fired.clone().unwrap_or_else(|| vec![-1]));
        let base = self.fight_spec(def);
        let spec = match self.war.fightsim.com_specs.get(&key) {
            Some(s) => s.clone(),
            None => {
                let weapons = commander_weapons(&base.weapons, def, fired.as_deref());
                let spec = Arc::new(UnitSpec { weapons, ..(*base).clone() });
                self.war.fightsim.com_specs.insert(key, spec.clone());
                spec
            }
        };
        if changed {
            let names: Vec<&str> = spec.weapons.iter().map(|w| w.name.as_str()).collect();
            self.log(&format!(
                "fightsim: commander {id} ({}) fires {names:?} (its def mounts {})",
                spec.def,
                base.weapons.len()
            ));
        }
        spec
    }

    /// The weapon def ids commander `id` fires, sorted (`None`: never read),
    /// from `read` (just read off the unit) or as last read; without the
    /// manual weapons of ours.
    pub(crate) fn commander_fired(&self, id: i32, read: Option<Vec<(i32, bool)>>) -> Option<Vec<i32>> {
        let ours = Unit::from_id(self.cb, id).is_some_and(|u| u.team() == self.team_id);
        let loadout = read.or_else(|| self.war.fightsim.loadouts.get(&id).cloned())?;
        let mut fired: Vec<i32> = loadout.iter().filter(|&&(_, manual)| !(ours && manual)).map(|w| w.0).collect();
        fired.sort_unstable();
        fired.dedup();
        Some(fired)
    }

    /// The fight a squad at `pos` would be in now: the squad and our other
    /// fighters beside it against every identified enemy within reach as
    /// last seen, or (`only`) the squad alone against those enemies wherever
    /// they were last seen within [`LATCH_MEMORY`]. `None` without an armed
    /// enemy (nothing to predict).
    fn fight_snapshot(
        &mut self,
        sid: SquadId,
        pos: Vec3,
        target: Vec3,
        only: Option<&[i32]>,
    ) -> Option<(FightSnapshot, SnapshotNotes)> {
        let frame = self.frame;
        let mut ours: Vec<(i32, UnitDef, f32, Vec3)> = Vec::new();
        let squad = self.squads.get(sid)?;
        if !ordinary_veto_kind(squad.kind, squad.is_air_squad, self.squad_strike_active(sid)) || target == NULLPOS {
            return None;
        }
        let members: Vec<i32> = squad.fighters.iter().copied().chain(squad.leader).collect();
        for (id, f) in &self.fighters {
            // A latched fight (`only`) is the squad's own: what stands by
            // it at the haven does not walk back in with it.
            if !members.contains(id)
                && (only.is_some()
                    || distance(f.get_pos(), pos) > FRIEND_REACH
                    || self.retreat_handler.is_retreating(*id))
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
            let wanted = match only {
                Some(ids) => ids.contains(&eid) && frame - e.last_seen <= LATCH_MEMORY,
                None => {
                    frame - e.last_seen <= SEEN_WITHIN
                        && on_approach(e.position, pos, target, e.def_max_weapon_range().max(350.0) + 200.0)
                }
            };
            if e.is_dead || !e.identified || !wanted {
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
        let mut snap = FightSnapshot { flags: self.sim_flags(), ..Default::default() };
        let mut notes = SnapshotNotes {
            members: Vec::new(),
            enemies: Vec::new(),
            metal: [0.0; 2],
            armed_metal: [0.0; 2],
            air_share: 0.0,
            static_share: 0.0,
            n: [0; 2],
        };
        for (id, def, hp, p) in ours {
            let spec = self.fight_spec_of(id, def);
            if !spec.weapons.is_empty() {
                notes.armed_metal[0] += spec.cost;
            }
            notes.members.push((id, spec.cost));
            notes.metal[0] += spec.cost;
            notes.n[0] += 1;
            let tubes = self.snap_tubes(id);
            snap.units.push(SnapUnit { side: 0, spec, hp, x: p.x, z: p.z, tubes });
        }
        let (mut air, mut statics) = (0.0, 0.0);
        for (eid, def, hp, p, flies, is_static) in theirs {
            let spec = self.fight_spec_of(eid, def);
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
            let tubes = self.snap_tubes(eid);
            snap.units.push(SnapUnit { side: 1, spec, hp, x: p.x, z: p.z, tubes });
        }
        if notes.metal[1] > 0.0 {
            notes.air_share = air / notes.metal[1];
            notes.static_share = statics / notes.metal[1];
        }
        (snap.armed(0) > 0 && snap.armed(1) > 0).then_some((snap, notes))
    }

    /// Our units `ours` against enemies `theirs` as they stand now, judged
    /// as the veto judges (kiting losses over [`VETO_HORIZON`]): each
    /// side's killed share of its armed metal. None without the predictor,
    /// without an armed unit on each side, or with what the sim does not
    /// do (shields, submerged fights, too many units).
    pub(crate) fn fight_judge_units(&mut self, ours: &[Unit], theirs: &[i32]) -> Option<[f32; 2]> {
        let model = veto_model().ok()?;
        let snap = self.fight_snapshot_units(ours, theirs)?;
        Some(self.fightsim_judge(model, &snap).0)
    }

    /// The same fight run as the a-move fight the model was fitted on
    /// (calibrated win probability and killed shares), for the record.
    pub(crate) fn fight_predict_units(&mut self, ours: &[Unit], theirs: &[i32]) -> Option<FightOutcome> {
        let model = veto_model().ok()?;
        let snap = self.fight_snapshot_units(ours, theirs)?;
        Some(model.predict(&snap))
    }

    /// The snapshot behind [`Ai::fight_judge_units`].
    fn fight_snapshot_units(&mut self, ours: &[Unit], theirs: &[i32]) -> Option<FightSnapshot> {
        if !self.fightsim_runs() {
            return None;
        }
        let frame = self.frame;
        let mut snap = FightSnapshot { flags: self.sim_flags(), ..Default::default() };
        for u in ours {
            let (Some(def), hp, p) = (u.def(), u.health(), u.pos()) else { continue };
            if hp <= 0.0 {
                continue;
            }
            if p.y < -5.0 || def.weapon_mounts().iter().filter_map(|m| m.weapon_def()).any(|w| w.is_shield()) {
                return None;
            }
            let spec = self.fight_spec_of(u.id(), def);
            let tubes = self.snap_tubes(u.id());
            snap.units.push(SnapUnit { side: 0, spec, hp, x: p.x, z: p.z, tubes });
        }
        let mut foes: Vec<(i32, UnitDef, f32, Vec3)> = Vec::new();
        for eid in theirs {
            let Some(e) = self.war.targets.get(eid) else { continue };
            if e.is_dead || !e.identified || frame - e.last_seen > LATCH_MEMORY {
                continue;
            }
            let Some(def) = e.ud else { continue };
            if e.position.y < -5.0 {
                return None;
            }
            foes.push((*eid, def, e.get_health() * e.def_health(), e.position));
        }
        for (eid, def, hp, p) in foes {
            let spec = self.fight_spec_of(eid, def);
            let tubes = self.snap_tubes(eid);
            snap.units.push(SnapUnit { side: 1, spec, hp, x: p.x, z: p.z, tubes });
        }
        if snap.units.len() > MAX_UNITS || snap.armed(0) == 0 || snap.armed(1) == 0 {
            return None;
        }
        Some(snap)
    }

    /// Judge `snap` on this thread, counting the cost.
    fn fightsim_judge(&mut self, model: &Model, snap: &FightSnapshot) -> ([f32; 2], bool) {
        let t0 = Instant::now();
        let out = judge(model, snap);
        let us = t0.elapsed().as_micros() as u64;
        let fs = &mut self.war.fightsim;
        fs.judged += 1;
        fs.judge_us += us;
        fs.judge_max_us = fs.judge_max_us.max(us);
        out
    }

    /// Add a veto for crushing defeats to the complete ordinary threat rule.
    /// An uncertain or merely losing fight never adds a refusal. `flat` must
    /// already include the caller's territorial exceptions, so home ground
    /// cannot exempt an otherwise suicidal engagement from the veto. A veto
    /// latches (see the module notes): the squad is told to retreat at every
    /// check until the latch ends.
    pub fn fight_verdict(&mut self, sid: SquadId, pos: Vec3, target: Vec3, courage: f32, flat: bool) -> bool {
        if !self.fightsim_runs() {
            return flat;
        }
        let drives = self.fightsim_drives();
        let frame = self.frame;
        self.war.fightsim.verdicts += 1;
        if flat {
            self.war.fightsim.flat_retreats += 1;
        }
        let Ok(model) = veto_model() else {
            self.war.fightsim.fallbacks += 1;
            return flat;
        };
        if let Some((since, enemies, at)) =
            self.war.fightsim.latches.get(&sid).map(|l| (l.since, l.enemies.clone(), l.at))
        {
            let age = frame - since;
            let mut judged = None;
            let hold = latch_holds(age, || {
                let r = self.fight_snapshot(sid, pos, target, Some(&enemies)).map(|(mut snap, _)| {
                    // As if back where the veto stopped us: the retreat's
                    // distance alone must not make the fight look winnable.
                    let (dx, dz) = (at.x - pos.x, at.z - pos.z);
                    for u in snap.units.iter_mut().filter(|u| u.side == 0) {
                        u.x += dx;
                        u.z += dz;
                    }
                    self.fightsim_judge(model, &snap)
                });
                judged = Some(r);
                r.is_some_and(|(_, crushing)| crushing)
            });
            if hold && drives {
                self.war.fightsim.latch_holds += 1;
                return true;
            }
            self.war.fightsim.latches.remove(&sid);
            self.war.fightsim.latch_releases += 1;
            let loss = judged.flatten().map(|(loss, _)| loss);
            self.op_emit(
                "fightsim",
                json!({
                    "event": "release", "t": frame / 30, "squad": sid, "held_s": age / 30,
                    "why": if age >= LATCH_MAX { "time" } else if loss.is_none() { "gone" } else { "not crushing" },
                    "killed": loss,
                }),
            );
        }
        let Some((snapshot, notes)) = self.fight_snapshot(sid, pos, target, None) else {
            self.war.fightsim.no_contact += 1;
            return flat;
        };
        let armed = (snapshot.armed(0), snapshot.armed(1));
        let (loss, retreat) = self.fightsim_judge(model, &snapshot);
        let fs = &mut self.war.fightsim;
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
            let enemies = notes.enemies.iter().map(|&(id, metal)| (id, metal, false)).collect();
            fs.windows.push(Window {
                vid,
                squad: sid,
                frame,
                members: notes.members.clone(),
                metal: notes.metal[0],
                enemies,
                agree,
            });
            let rec = json!({
                "event": "verdict", "vid": vid, "t": frame / 30, "squad": sid, "agree": agree,
                "mode": if drives { "veto" } else { "shadow" }, "horizon_s": VETO_HORIZON / 30,
                "n": notes.n, "armed": [armed.0, armed.1], "metal": notes.metal,
                "b_air_share": notes.air_share, "b_static_share": notes.static_share,
                "killed": loss, "sim": retreat, "flat": flat, "courage": courage,
                "armed_metal": notes.armed_metal, "veto": drives && retreat && !flat,
            });
            self.op_emit("fightsim", rec);
        }
        if drives && retreat {
            let enemies = notes.enemies.iter().map(|e| e.0).collect();
            self.war.fightsim.latches.insert(sid, Latch { since: frame, enemies, at: pos });
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
        let mut snapshot = FightSnapshot { flags: self.sim_flags(), ..Default::default() };
        let mut metal = [0.0; 2];
        for (side, id, d, hp, p) in raw {
            // A casualty, reinforcement, substantial damage, or movement
            // invalidates the old result even inside the freshness window.
            // So does a tube weapon firing or loading.
            let tubes = self.snap_tubes(id);
            (side, id, d.id(), (hp / d.health().max(1.0) * 5.0) as i32, (p.x / 256.0) as i32, (p.z / 256.0) as i32)
                .hash(&mut hash);
            tubes.len().hash(&mut hash);
            let spec = self.fight_spec_of(id, d);
            metal[side as usize] += spec.cost;
            snapshot.units.push(SnapUnit { side, spec, hp, x: p.x, z: p.z, tubes });
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
                units.push(SnapUnit {
                    side,
                    hp: spec.health,
                    spec: spec.clone(),
                    x,
                    z: i as f32 * 40.0,
                    tubes: Vec::new(),
                });
            }
        }
        FightSnapshot { flags: Default::default(), units }
    }

    /// The veto's purpose: an outranged, outrun squad walking into a kiting
    /// group it cannot touch. Slow deaths (one Knight to two Scalpels takes
    /// ~25 s) and even fights stay allowed, as does a crushing superiority.
    #[test]
    fn hopeless_knights_are_vetoed_against_kiting_scalpels() {
        let model = Model::load().unwrap();
        for (ours, theirs, veto) in
            [(1, 5, true), (1, 3, true), (3, 20, true), (1, 2, false), (3, 3, false), (10, 3, false)]
        {
            let (loss, crushing) = judge(&model, &encounter(ours, theirs));
            assert_eq!(crushing, veto, "{ours} Knights vs {theirs} Scalpels: killed {loss:?}");
            if veto {
                assert!(loss[1] < 0.01, "{ours} Knights vs {theirs} Scalpels touch them: {loss:?}");
            }
        }
    }

    #[test]
    fn a_latch_holds_its_minimum_then_while_crushing_never_past_its_maximum() {
        let never = || -> bool { panic!("judged inside the minimum or past the maximum") };
        assert!(latch_holds(0, never));
        assert!(latch_holds(LATCH_MIN - 1, never));
        assert!(latch_holds(LATCH_MIN, || true));
        assert!(!latch_holds(LATCH_MIN, || false));
        assert!(latch_holds(LATCH_MAX - 1, || true));
        assert!(!latch_holds(LATCH_MAX, never));
    }

    #[test]
    fn losses_are_armed_metal_weighted_death_chances() {
        let snap = encounter(2, 1);
        let fate =
            |p| UnitFate { dead: false, death_frame: None, hp_left: 0.0, p_dead: 0.5, p_raw: p, captured: false };
        let (loss, metal) = kiting_losses(&snap, &[fate(1.0), fate(0.0), fate(0.5)]);
        assert_eq!(metal, [700.0, 220.0]);
        assert_eq!(loss, [0.5, 0.5]);
    }
}

#[cfg(test)]
mod comspec_tests {
    use super::*;

    fn w(name: &str, dps: f32) -> WeaponSpec {
        WeaponSpec {
            name: name.into(),
            weapon_type: "BeamLaser".into(),
            range: 300.0,
            reload: 1.0,
            salvo: 1,
            damage: dps,
        }
    }

    #[test]
    fn a_commander_fires_its_loadout_not_every_mount() {
        // dynsupport1's mounts (the fight spec keeps one per weapon def).
        let all = vec![
            w("dyntrainer_support_base_commweapon_peashooter", 120.0),
            w("dyntrainer_support_base_commweapon_beamlaser", 144.0),
            w("dyntrainer_support_base_commweapon_lparticlebeam", 192.0),
            w("dyntrainer_support_base_commweapon_hparticlebeam", 258.0),
            w("dyntrainer_support_base_commweapon_multistunner", 352.0),
        ];
        let fired = ["dyntrainer_support_base_commweapon_lparticlebeam".to_owned()];
        let got = pick_weapons(&all, Some(&fired));
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].name, fired[0]);
        // Never read: one beam laser, as the fitted model counted them.
        let got = pick_weapons(&all, None);
        assert_eq!(got.len(), 1);
        assert!(got[0].name.ends_with("commweapon_beamlaser"));
        // A loadout of manual weapons only (ours, left out): unarmed.
        assert!(pick_weapons(&all, Some(&[])).is_empty());
    }
}
