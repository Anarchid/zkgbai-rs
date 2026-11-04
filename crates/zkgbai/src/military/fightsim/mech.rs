//! Mechanics on the fight predictor: a port of the frozen mechanics model
//! (tools/predictor/mech.py with the round-1 six, the round-3 heatray and
//! the round-6 slow2d, zkgbai-runs/predictor/mech1..mech6) onto the same
//! one-dimensional fight:
//!
//! - air: a weapon skips targets it cannot shoot (aircraft, or ground);
//! - burn: fire weapons leave their fitted damage over time on what they hit;
//! - death: units explode as they die, and kamikazes (Imp, Snitch, Skuttle,
//!   ...) walk into the enemy and blow up within 20 elmos;
//! - emp: paralysis builds up to a cap and wears off over 40 s; a unit
//!   paralysed past its health neither moves nor fires; pure EMP weapons
//!   fire too (no health damage);
//! - slow: slow damage cuts speed and rate of fire by up to half, wearing
//!   off half a second after the last slowing hit; it scales with the damage
//!   landed and is capped at half the target's health plus the weapon's
//!   overslow (unit_timeslow.lua); slow-only weapons (Cyclops slowbeam) do
//!   not fire;
//! - engage: each unit fires at all with a fitted probability (its nearest
//!   enemy over its range, short range, static, commander, builder, own
//!   units in its line of fire, crowding, enemies), a weight on every shot;
//! - heatray: weapons whose damage falls off with distance land their
//!   fitted share of it by range band;
//! - tacai (on request, [`Model::kiting_fates`]): ZK's tactical unit AI
//!   (mech2's `tacai`): a unit whose def skirms its closest enemy holds at
//!   its skirm range, farther while reloading; one that flees it keeps
//!   out of its range. Not in the fitted answer: mech2 found it moved
//!   skirmisher losses the right way without changing who wins overall.
//!
//! The parameters and a calibration refitted on these outputs are compiled
//! in from `data/fightsim_mech.tsv`, written by
//! `tools/predictor/export_mech.py` (its first line says from what).

use std::collections::{HashMap, HashSet};

use super::{
    DT, FateMode, FightSnapshot, HORIZON, Hazard, HitTables, MechOutcome, Model, RBANDS, SimFlags, SimUnit, UnitFate,
    ZoiBeta, data_lines, expit, logit, parse_f64, plane, project, speed_band, take,
};

const MECH_TSV: &str = include_str!("../../../data/fightsim_mech.tsv");
/// ZK's tactical unit AI, written by `tools/predictor/export_mech.py ... tacai`.
const TACAI_TSV: &str = include_str!("../../../data/fightsim_tacai.tsv");

/// Unit defs that attack by dying next to the enemy.
const KAMIKAZE: [&str; 6] = ["cloakbomb", "shieldbomb", "jumpbomb", "amphbomb", "gunshipbomb", "subscout"];
/// A kamikaze within this of its nearest enemy blows up...
const KAMIKAZE_REACH: f64 = 20.0;
/// ...or with [`SimFlags::bombs`] within its def's `kamikazeDistance`,
/// centre to centre (Unit.cpp SlowUpdateKamikaze; ZK has no
/// AllowUnitKamikaze), as mech.py `KAMIKAZE_DIST`.
const KAMIKAZE_DIST: [(&str, f64); 6] = [
    ("cloakbomb", 80.0),
    ("shieldbomb", 80.0),
    ("jumpbomb", 25.0),
    ("amphbomb", 120.0),
    ("gunshipbomb", 60.0),
    ("subscout", 80.0),
];

/// mech.py `goes_off`: kamikaze `k` at `dist` from its nearest enemy `n`.
fn goes_off(k: &MechUnit, n: &MechUnit, dist: f64) -> bool {
    dist <= k.kreach || (k.kcontact && dist <= k.rad + n.rad + 1.0)
}

fn kamikaze_reach(def: &str, bombs: bool) -> f64 {
    (KAMIKAZE_DIST.iter().find(|(d, _)| bombs && *d == def)).map_or(KAMIKAZE_REACH, |&(_, r)| r)
}
/// Seconds for full paralysis to wear off (unit_paralysis_damage.lua).
const STUN_DECAY_S: f64 = 40.0;

/// Remaining stun from paralysis damage divided by current max health.
pub(crate) fn paralysis_frames(ratio: f32) -> i32 {
    if !ratio.is_finite() || ratio <= 1.0 {
        return 0;
    }
    ((ratio - 1.0) * STUN_DECAY_S as f32 * 30.0).floor() as i32
}

/// Most a unit is slowed (timeslow_defs.lua).
const SLOW_MAX: f64 = 0.5;
/// Slow damage wearing off per second, as a share of health (overslow
/// allowance wears off at the same rate).
const SLOW_DECAY: f64 = 0.04;
/// Frames after a slowing hit before the slow starts wearing off.
const SLOW_GRACE: i32 = 15;
/// Steps of a unit's distance to its nearest enemy over its range.
const ENGAGE_BANDS: [f64; 5] = [0.5, 1.0, 1.5, 2.5, 5.0];
/// The engagement features, in the order of their weights.
const ENGAGE_NAMES: [&str; 13] = [
    "const",
    "ratio>0.5",
    "ratio>1",
    "ratio>1.5",
    "ratio>2.5",
    "ratio>5",
    "short",
    "static",
    "com",
    "builder",
    "blockers",
    "log1p(crowd)",
    "log1p(foes)",
];

/// What a weapon does besides its health damage.
#[derive(Clone, Copy, Debug, PartialEq)]
struct WeaponMech {
    can_air: bool,
    can_ground: bool,
    /// Burn damage per hit, spread over `burn_frames`.
    burn: f64,
    burn_frames: i32,
    /// Paralysis damage per shot (a paralyzer's damage or an EMP extra).
    emp: f64,
    /// Slow damage per point of health damage.
    slow: f64,
    /// How far past the slow cap a hit may push (share of health).
    overslow: f64,
    /// Pure EMP: no health damage.
    paralyzer: bool,
    /// Damage landed over nominal by range band (heatrays).
    dyn_frac: Option<[Option<f64>; RBANDS as usize + 1]>,
}

const PLAIN: WeaponMech = WeaponMech {
    can_air: true,
    can_ground: true,
    burn: 0.0,
    burn_frames: 0,
    emp: 0.0,
    slow: 0.0,
    overslow: 0.0,
    paralyzer: false,
    dyn_frac: None,
};

/// A unit def's death explosion.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Death {
    damage: f64,
    aoe: f64,
    /// Damage falloff at the edge of the blast (0 = none, 1 = all).
    edge: f64,
    paralyzer: bool,
}

/// A splash weapon in the plane fight (mech.py `_aoe_info`, aoe_fit.py).
#[derive(Clone, Copy, Debug, PartialEq)]
struct Splash {
    /// Blast radius (elmos) and the engine's damage falloff at its edge.
    radius: f64,
    edge: f64,
    /// Share of the engine falloff damage landed on units of the target's
    /// side and of the shooter's side (fitted by weapon class).
    kappa_target: f64,
    kappa_shooter: f64,
    /// Splash per shot its training shots landed on others, as a share of
    /// nominal: the hit tables' damage per hit includes it, so the target
    /// is charged that much less.
    geo: f64,
}

/// One fight's calibration: P(A wins), the killed shares' means, and the
/// per-unit fates' recalibration.
#[derive(Clone, Debug)]
pub struct Calibration {
    /// P(A wins) = expit(w0 + w1 (killed_b - killed_a)) on the mechanics outputs.
    pub win: [f64; 2],
    pub cal_a: ZoiBeta,
    pub cal_b: ZoiBeta,
    /// A unit's P(dead by the horizon) recalibrated: expit(t0 + t1 logit(raw)
    /// + t2 static + t3 ln(horizon / 150)) (tools/predictor/score_fates.py).
    pub fate: [f64; 4],
}

impl Calibration {
    fn take(cal: &[(String, f64)], prefix: &str) -> Result<Calibration, String> {
        Ok(Calibration {
            win: take(cal, &format!("{prefix}win."))?,
            cal_a: ZoiBeta { theta: take(cal, &format!("{prefix}zoibA."))? },
            cal_b: ZoiBeta { theta: take(cal, &format!("{prefix}zoibB."))? },
            fate: take(cal, &format!("{prefix}fate."))?,
        })
    }
}

/// One unit def's tactical AI (LuaRules/Configs/tactical_ai_defs.lua as
/// tools/predictor/tacai_dump.py resolves it, with mech.py's defaults).
#[derive(Clone, Debug, Default, PartialEq)]
struct TacDef {
    /// Its weapon's range; 0 = it never skirms.
    skirm_range: f64,
    skirm_leeway: f64,
    /// Extra distance per frame until its weapon is ready.
    reload_leeway: f64,
    /// Kept beyond a fled enemy's range.
    flee_leeway: f64,
    skirm_all: bool,
    flee_all: bool,
    skirms: HashSet<String>,
    flees: HashSet<String>,
}

fn parse_tacai(tsv: &str) -> Result<HashMap<String, TacDef>, String> {
    let mut out = HashMap::new();
    for line in data_lines(tsv) {
        let f: Vec<&str> = line.split('\t').collect();
        if f.len() != 9 {
            return Err(format!("tacai: {} fields in {line:?}", f.len()));
        }
        let set = |s: &str| s.split(',').filter(|n| !n.is_empty()).map(str::to_owned).collect::<HashSet<_>>();
        out.insert(
            f[0].to_owned(),
            TacDef {
                skirm_range: parse_f64(f[1])?,
                skirm_leeway: parse_f64(f[2])?,
                reload_leeway: parse_f64(f[3])?,
                flee_leeway: parse_f64(f[4])?,
                skirm_all: f[5] == "1",
                flee_all: f[6] == "1",
                skirms: set(f[7]),
                flees: set(f[8]),
            },
        );
    }
    Ok(out)
}

/// A fight's tactical AI: per unit its def's rule, and whom it skirms and
/// flees among the units of the other side.
pub(super) struct Kiting {
    n: usize,
    /// (skirm range less leeway, reload leeway, flee leeway, the weapon
    /// whose reload counts: its longest-ranged, the first of equals).
    rules: Vec<Option<(f64, f64, f64, usize)>>,
    skirms: Vec<bool>,
    flees: Vec<bool>,
}

impl Kiting {
    /// mech.py `tacai`: the distance unit `i` keeps from its closest enemy
    /// `j`, if its tactical AI has a say.
    fn want(&self, us: &[MechUnit], i: usize, j: usize, dist: f64, f: i32) -> Option<f64> {
        let (skirm, reload, flee, w) = self.rules[i]?;
        if self.skirms[i * self.n + j] {
            let next = us[i].u.weapons.get(w).map_or(0, |w| w.next);
            return Some(skirm + reload * (next - f).max(0) as f64);
        }
        self.flees[i * self.n + j].then(|| dist.max(us[j].u.max_range + flee))
    }
}

/// The fitted mechanics and their calibration.
pub struct MechTables {
    weapons: HashMap<String, WeaponMech>,
    death: HashMap<String, Death>,
    /// Splash weapons, for the plane fight.
    splash: HashMap<String, Splash>,
    engage: [f64; 13],
    /// Seconds of stun past full paralysis the build-up is capped at.
    stun_cap_s: f64,
    /// P(A wins) = expit(w0 + w1 (killed_b - killed_a)) on the mechanics outputs.
    pub win: [f64; 2],
    pub cal_a: ZoiBeta,
    pub cal_b: ZoiBeta,
    /// A unit's P(dead by the horizon) recalibrated: expit(t0 + t1 logit(raw)
    /// + t2 static + t3 ln(horizon / 150)) (tools/predictor/score_fates.py).
    pub fate: [f64; 4],
    /// The `fightfocus` and `fight2d` fights' own calibrations (rows
    /// `cal focus.*` and `cal 2d.*`).
    pub focus_cal: Option<Calibration>,
    pub twod_cal: Option<Calibration>,
    /// The plain fight with tube weapons (`sreload` without `fightfocus`).
    pub sr_cal: Option<Calibration>,
    /// The data file's fitting header.
    pub fitted: String,
    /// ZK's tactical unit AI by unit def.
    tacai: HashMap<String, TacDef>,
}

impl MechTables {
    pub fn load() -> Result<MechTables, String> {
        let mut t = MechTables::parse(MECH_TSV)?;
        t.tacai = parse_tacai(TACAI_TSV)?;
        Ok(t)
    }

    /// The calibration of the fight `flags` asks for; the base one when the
    /// variant has none in the tables.
    pub fn calibration(&self, flags: SimFlags) -> Calibration {
        let base =
            || Calibration { win: self.win, cal_a: self.cal_a.clone(), cal_b: self.cal_b.clone(), fate: self.fate };
        let variant = if flags.twod {
            self.twod_cal.as_ref()
        } else if flags.focus {
            self.focus_cal.as_ref()
        } else if flags.sreload {
            self.sr_cal.as_ref()
        } else {
            None
        };
        variant.cloned().unwrap_or_else(base)
    }

    fn parse(tsv: &str) -> Result<MechTables, String> {
        let mut weapons = HashMap::new();
        let mut death = HashMap::new();
        let mut splash = HashMap::new();
        let mut dyn_frac: HashMap<String, [Option<f64>; RBANDS as usize + 1]> = HashMap::new();
        let mut engage = Vec::new();
        let mut cal = Vec::new();
        let mut stun_cap_s = None;
        for line in data_lines(tsv) {
            let f: Vec<&str> = line.split('\t').collect();
            let num =
                |i: usize| f.get(i).ok_or_else(|| format!("mech: short line {line:?}")).and_then(|s| parse_f64(s));
            let name = f.get(1).copied().unwrap_or_default().to_owned();
            match f[0] {
                "weapon" => {
                    let w = WeaponMech {
                        can_air: num(2)? != 0.0,
                        can_ground: num(3)? != 0.0,
                        burn: num(4)?,
                        burn_frames: num(5)? as i32,
                        emp: num(6)?,
                        slow: num(7)?,
                        paralyzer: num(8)? != 0.0,
                        overslow: if f.len() > 9 { num(9)? } else { 0.0 },
                        dyn_frac: None,
                    };
                    weapons.insert(name, w);
                }
                "death" => {
                    let d = Death { damage: num(2)?, aoe: num(3)?, edge: num(4)?, paralyzer: num(5)? != 0.0 };
                    death.insert(name, d);
                }
                "dyn" => {
                    let rb = num(2)? as usize;
                    if rb > RBANDS as usize {
                        return Err(format!("mech: range band {rb} in {line:?}"));
                    }
                    dyn_frac.entry(name).or_default()[rb] = Some(num(3)?);
                }
                "aoe" => {
                    let s = Splash {
                        radius: num(2)?,
                        edge: num(3)?,
                        kappa_target: num(4)?,
                        kappa_shooter: num(5)?,
                        geo: num(6)?,
                    };
                    splash.insert(name, s);
                }
                "engage" => engage.push((name, num(2)?)),
                "const" if name == "stun_cap_s" => stun_cap_s = Some(num(2)?),
                "cal" => cal.push((name, num(2)?)),
                k => return Err(format!("mech: unknown row {k:?}")),
            }
        }
        for (name, bands) in dyn_frac {
            weapons.entry(name).or_insert(PLAIN).dyn_frac = Some(bands);
        }
        if engage.iter().map(|e| e.0.as_str()).ne(ENGAGE_NAMES) {
            return Err(format!("mech: engagement features {:?}, not {ENGAGE_NAMES:?}", engage));
        }
        let mut w = [0.0; 13];
        for (w, (_, v)) in w.iter_mut().zip(&engage) {
            *w = *v;
        }
        let base = Calibration::take(&cal, "")?;
        let variant = |v: &str| {
            let p = format!("{v}.");
            cal.iter().any(|(k, _)| k.starts_with(&p)).then(|| Calibration::take(&cal, &p)).transpose()
        };
        Ok(MechTables {
            weapons,
            death,
            splash,
            engage: w,
            stun_cap_s: stun_cap_s.ok_or("mech: no stun_cap_s")?,
            win: base.win,
            cal_a: base.cal_a,
            cal_b: base.cal_b,
            fate: base.fate,
            focus_cal: variant("focus")?,
            twod_cal: variant("2d")?,
            sr_cal: variant("sr")?,
            fitted: tsv.lines().next().unwrap_or_default().trim_start_matches('#').trim().to_owned(),
            tacai: HashMap::new(),
        })
    }

    fn weapon(&self, name: &str) -> WeaponMech {
        self.weapons.get(name).copied().unwrap_or(PLAIN)
    }
}

/// A unit in the mechanics fight: the base unit and its status.
#[derive(Clone, Debug)]
pub(super) struct MechUnit {
    u: SimUnit,
    /// Per weapon, in `u.weapons` order.
    mech: Vec<WeaponMech>,
    maxhp: f64,
    air: bool,
    kamikaze: bool,
    /// How near its nearest enemy a kamikaze goes off...
    kreach: f64,
    /// ...or, with [`SimFlags::bombs`], once touching it: the engine
    /// pushes units apart by their move footprints, smaller than the radii
    /// the plane fight keeps apart (mech.py `goes_off`).
    kcontact: bool,
    death: Option<Death>,
    /// amphbomb.lua death-blast customparams (all mechanics formulations).
    limpet: bool,
    /// P(the unit fires at all in this fight).
    engage: f64,
    para: f64,
    slowdmg: f64,
    slow_last: i32,
    /// Overslow allowance: slow damage may reach (SLOW_MAX + this) x health.
    ovs: f64,
    /// (last frame, damage per frame).
    burns: Vec<(i32, f64)>,
    /// Lateral coordinate: 0 except in a fates query's pass mode, a defense
    /// rollout and the plane fight.
    lat: f64,
    /// Unit radius (elmos): the plane fight keeps units this far apart and
    /// measures splash from their edge.
    rad: f64,
    /// Per weapon, in `u.weapons` order: its splash in the plane fight.
    splash: Vec<Option<Splash>>,
}

impl MechUnit {
    fn stunned(&self) -> bool {
        self.para > self.maxhp
    }

    fn slowed(&self) -> f64 {
        if self.slowdmg > 0.0 { SLOW_MAX.min(self.slowdmg / self.u.hp.max(1.0)) } else { 0.0 }
    }

    /// unit_timeslow.lua: slow damage never exceeds (0.5 + overslow) x
    /// current health.
    fn cap_slow(&mut self) {
        self.slowdmg = self.slowdmg.min((SLOW_MAX + self.ovs) * self.u.hp.max(1.0));
    }
}

/// mech.py `apart`: distance along the axis, and across it when the two
/// stand on different lines (pass mode).
fn apart(a: &MechUnit, b: &MechUnit) -> f64 {
    let dy = a.lat - b.lat;
    if dy == 0.0 { (a.u.x - b.u.x).abs() } else { (a.u.x - b.u.x).hypot(dy) }
}

/// A Lobster carries the first `count` snapshot units by one displacement.
/// They remain exposed to enemy fire, and cannot walk or shoot until landing.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Insertion {
    pub count: usize,
    pub dx: f32,
    pub dz: f32,
    pub wait: i32,
    pub flight: i32,
}

#[derive(Clone, Copy)]
struct Flight {
    count: usize,
    displacement: f64,
    wait: i32,
    end: i32,
}

/// What a run of the mechanics fight answers.
/// A local raid-defense rollout. Arrival delays represent path detours beyond
/// straight-line travel; absent units cannot fire, block, or draw enemy fire.
/// Raiders keep shooting their observed asset, then the nearest surviving
/// protected asset. Unlike ordinary fights, positions remain two-dimensional.
#[derive(Clone, Debug, Default)]
pub struct Defense {
    pub arrivals: Vec<i32>,
    pub assets: Vec<usize>,
    pub focus: Vec<Option<usize>>,
    /// Whom the raiders shoot and walk to.
    pub raid_aim: RaidAim,
}

/// Whom the raiders of a raid-defense rollout shoot and walk to.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RaidAim {
    /// The asset each was seen shooting, then the nearest protected asset
    /// left: raiders never turn on a defender while an asset of the rollout
    /// lives (the frozen rollout). A lone Dagger then plinks a Mace for free.
    #[default]
    Assets,
    /// The asset each was seen shooting, until it dies; then whatever the
    /// fight's targeting picks (the nearest, or the least health left),
    /// defenders included: an attack-moving raider holds its target, then
    /// takes the engine's closest valid one (the AI's `raidfire`).
    Hold,
    /// No held asset: from the first frame like any unit.
    Free,
}

struct Run<'a> {
    defense: Option<&'a Defense>,
    defense_engagement: Option<(&'a FightSnapshot, &'a [f64; 13])>,
    hazard: Option<&'a Hazard>,
    withdraw: bool,
    /// A's mobile units drive through B (fates' pass mode).
    pass: bool,
    focus: Option<usize>,
    insertion: Option<Flight>,
    /// ZK's tactical unit AI moves the units it governs.
    kiting: Option<&'a Kiting>,
    horizon: i32,
    /// Per-unit bookkeeping for a fates query; the fight then goes on to
    /// the horizon while one side has an armed unit and the other anything.
    track: Option<Track>,
    /// The plane fight: moves in the plane, no overlaps, splash.
    twod: bool,
    /// Shots go to the least-health target in range.
    least_health: bool,
    /// A bomb's charge, for [`Model::bomb_charge`] (Rust only).
    bomb: Option<BombRun>,
    /// Log of the chance the bomb has survived the one-hit kills aimed at
    /// it so far ([`BombMode::Charge`]).
    bomb_ln_survive: f64,
}

/// One of side A's kamikazes in a [`Model::bomb_charge`] branch.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct BombRun {
    pub index: usize,
    pub mode: BombMode,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum BombMode {
    /// It runs at the enemy. The expected-damage fight would kill a 50-hp
    /// bomb with any shot whose hit chance times damage passes 50; here a
    /// shot that kills with one hit misses it, and its chance of hitting
    /// goes into [`Run::bomb_ln_survive`] (this branch is the bomb getting
    /// through). Hits too weak to kill alone land as expected damage, so a
    /// beam's stream still burns it down.
    Charge,
    /// It is shot down where it stands at the start, and blows up there.
    Down,
}

/// mech.py's fates bookkeeping: the Bernoulli variance of the hits each
/// unit took and the frame it died at.
struct Track {
    var: Vec<f64>,
    death_f: Vec<Option<i32>>,
}

/// mech.py `_pdead`: P(dead by the horizon), the expected damage taken by
/// then against the starting health, spread by the hits' variance
/// (normal). A unit dead earlier has its damage and variance carried on at
/// their rate to the horizon; one killed without spread is dead.
fn p_dead(hp0: f64, hp: f64, death_f: Option<i32>, var: f64, horizon: i32) -> f64 {
    let (mut taken, mut v) = (hp0 - hp, var);
    if let Some(df) = death_f {
        let scale = horizon as f64 / (df + DT).max(DT) as f64;
        taken *= scale;
        v *= scale;
    }
    if v <= 0.0 {
        return if death_f.is_some() || taken >= hp0 { 1.0 } else { 0.0 };
    }
    0.5 * (1.0 + erf((taken - hp0) / (2.0 * v).sqrt()))
}

/// Abramowitz and Stegun 7.1.26 (absolute error below 1.5e-7).
fn erf(x: f64) -> f64 {
    let t = 1.0 / (1.0 + 0.3275911 * x.abs());
    let y = 1.0
        - (((((1.061405429 * t - 1.453152027) * t) + 1.421413741) * t - 0.284496736) * t + 0.254829592)
            * t
            * (-x * x).exp();
    y.copysign(x)
}

impl Model {
    /// The mechanics fight's a-move and withdrawal under the hazard, and
    /// the a-move calibrated.
    pub(super) fn mech_outcome(&self, snap: &FightSnapshot) -> MechOutcome {
        let units = self.mech_units(snap);
        let (s, f) = (self.mech.stun_cap_s, snap.flags);
        let t = self.mech.calibration(f);
        let (ka, kb) = simulate_mech_flags(&units, &self.hits, s, Some(&self.hazard), false, f);
        let (wa, wb) = simulate_mech_flags(&units, &self.hits, s, Some(&self.hazard), true, f);
        MechOutcome {
            killed_a: ka as f32,
            killed_b: kb as f32,
            withdraw_a: wa as f32,
            withdraw_b: wb as f32,
            mean_a: t.cal_a.mean(ka) as f32,
            mean_b: t.cal_b.mean(kb) as f32,
            p_a_wins: expit(t.win[0] + t.win[1] * (kb - ka)) as f32,
        }
    }

    /// Each snapshot unit's fate, in snapshot order, after `horizon` frames
    /// of the mechanics fight with no stopping rule and A moving as `mode`
    /// says (mech.py `simulate_mech(fates=...)`). The fight goes on while one
    /// side has an armed unit and the other anything alive, unarmed eco too.
    pub fn fates(&self, snap: &FightSnapshot, horizon: i32, mode: FateMode) -> Vec<UnitFate> {
        self.fates_with_paralysis(snap, horizon, mode, &[])
    }

    /// Seed observed EMP as (snapshot index, paralysis/max-health ratio).
    /// It decays normally: a disabled target can recover and fight back.
    pub fn fates_with_paralysis(
        &self,
        snap: &FightSnapshot,
        horizon: i32,
        mode: FateMode,
        paralysis: &[(usize, f32)],
    ) -> Vec<UnitFate> {
        self.fates_with_insertion(snap, horizon, mode, paralysis, None)
    }

    pub(crate) fn fates_with_insertion(
        &self,
        snap: &FightSnapshot,
        horizon: i32,
        mode: FateMode,
        paralysis: &[(usize, f32)],
        insertion: Option<Insertion>,
    ) -> Vec<UnitFate> {
        self.fates_config(snap, horizon, mode, paralysis, insertion, None, false)
    }

    /// [`Model::fates`] for an a-move with ZK's tactical unit AI moving the
    /// units it governs: skirmishers keep their range from what they skirm
    /// (a Scalpel from a Knight), scouts and AA flee what they flee.
    pub fn kiting_fates(&self, snap: &FightSnapshot, horizon: i32) -> Vec<UnitFate> {
        self.fates_config(snap, horizon, FateMode::AMove, &[], None, None, true)
    }

    pub fn defense_fates(
        &self,
        snap: &FightSnapshot,
        horizon: i32,
        defense: &Defense,
        paralysis: &[(usize, f32)],
    ) -> Vec<UnitFate> {
        self.fates_config(snap, horizon, FateMode::AMove, paralysis, None, Some(defense), false)
    }

    #[allow(clippy::too_many_arguments)]
    fn fates_config(
        &self,
        snap: &FightSnapshot,
        horizon: i32,
        mode: FateMode,
        paralysis: &[(usize, f32)],
        insertion: Option<Insertion>,
        defense: Option<&Defense>,
        kiting: bool,
    ) -> Vec<UnitFate> {
        self.fates_run(snap, horizon, mode, paralysis, insertion, defense, kiting, &[], None).0
    }

    /// An a-move's fates for [`Model::bomb_charge`]: each unit's weapons
    /// first fire `phase[i]` of their reload in (busy elsewhere: they have
    /// just fired), and the bomb runs or is shot down as `bomb` says. Also
    /// the log of the bomb's chance to survive its run's one-hit kills.
    pub(super) fn bomb_fates(
        &self,
        snap: &FightSnapshot,
        horizon: i32,
        phase: &[f64],
        bomb: Option<BombRun>,
    ) -> (Vec<UnitFate>, f64) {
        self.fates_run(snap, horizon, FateMode::AMove, &[], None, None, false, phase, bomb)
    }

    #[allow(clippy::too_many_arguments)]
    fn fates_run(
        &self,
        snap: &FightSnapshot,
        horizon: i32,
        mode: FateMode,
        paralysis: &[(usize, f32)],
        insertion: Option<Insertion>,
        defense: Option<&Defense>,
        kiting: bool,
        phase: &[f64],
        bomb: Option<BombRun>,
    ) -> (Vec<UnitFate>, f64) {
        let mut units = self.mech_units(snap);
        for (m, &ph) in units.iter_mut().zip(phase) {
            if ph > 0.0 {
                for w in &mut m.u.weapons {
                    w.next = (ph * w.reload as f64).round() as i32;
                }
            }
        }
        let kiting = kiting.then(|| self.kiting(snap, &units));
        if defense.is_some() {
            for (m, u) in units.iter_mut().zip(&snap.units) {
                m.u.x = u.x as f64;
                m.lat = u.z as f64;
            }
        }
        // Project the displacement on the same original fight axis. Reprojecting
        // a second snapshot would rotate/recenter the battle halfway through it.
        let center = |side| {
            let us: Vec<_> = snap.units.iter().filter(|u| u.side == side).collect();
            let n = us.len().max(1) as f64;
            (us.iter().map(|u| u.x as f64).sum::<f64>() / n, us.iter().map(|u| u.z as f64).sum::<f64>() / n)
        };
        let (a, b) = (center(0), center(1));
        let len = (b.0 - a.0).hypot(b.1 - a.1).max(1.0);
        let insertion = insertion.map(|p| Flight {
            count: p.count.min(units.len()),
            displacement: (p.dx as f64 * (b.0 - a.0) + p.dz as f64 * (b.1 - a.1)) / len,
            wait: p.wait.max(0),
            end: p.wait.max(0) + p.flight.max(1),
        });
        for &(index, ratio) in paralysis {
            if ratio.is_finite()
                && ratio >= 0.0
                && let Some(u) = units.get_mut(index)
            {
                u.para = ratio as f64 * u.maxhp;
            }
        }
        let pass = match mode {
            FateMode::AMove | FateMode::Assassinate { .. } => false,
            FateMode::Pass { offset } => {
                for m in &mut units {
                    m.lat = if m.u.side == 1 && !m.u.mobile { 0.0 } else { offset as f64 };
                }
                true
            }
        };
        let n = units.len();
        let track = Track { var: vec![0.0; n], death_f: vec![None; n] };
        let focus = match mode {
            FateMode::Assassinate { target } => Some(target),
            _ => None,
        };
        // Pass mode stays on its lines (mech.py has no plane pass); it keeps
        // the targeting.
        let flags =
            if pass { SimFlags { twod: false, focus: snap.flags.least_health(), ..snap.flags } } else { snap.flags };
        let mut run = Run {
            defense,
            defense_engagement: defense.map(|_| (snap, &self.mech.engage)),
            hazard: None,
            withdraw: false,
            pass,
            focus,
            insertion,
            kiting: kiting.as_ref(),
            horizon,
            track: Some(track),
            twod: flags.twod,
            least_health: flags.least_health(),
            bomb,
            bomb_ln_survive: 0.0,
        };
        let (_, end) = run_mech(&units, &self.hits, self.mech.stun_cap_s, &mut run);
        let ln_survive = run.bomb_ln_survive;
        let Some(tr) = run.track else { return (Vec::new(), ln_survive) };
        let cal = self.mech.calibration(flags);
        let c = &cal.fate;
        let ln_h = (horizon.max(1) as f64 / 150.0).ln();
        let fates = units
            .iter()
            .zip(&end)
            .enumerate()
            .map(|(i, (u0, u))| {
                let raw = p_dead(u0.u.hp, u.u.hp, tr.death_f[i], tr.var[i], horizon);
                let z = c[1] * logit(raw.clamp(0.005, 0.995)) + c[2] * if u0.u.mobile { 0.0 } else { 1.0 };
                UnitFate {
                    dead: !u.u.alive,
                    death_frame: tr.death_f[i],
                    hp_left: if u.u.alive { u.u.hp.max(0.0) as f32 } else { 0.0 },
                    p_dead: expit(c[0] + z + c[3] * ln_h) as f32,
                    p_raw: raw as f32,
                }
            })
            .collect();
        (fates, ln_survive)
    }

    /// The tactical AI of the snapshot's units, `units` being its
    /// [`Model::mech_units`].
    pub(super) fn kiting(&self, snap: &FightSnapshot, units: &[MechUnit]) -> Kiting {
        let n = units.len();
        let (mut skirms, mut flees) = (vec![false; n * n], vec![false; n * n]);
        let mut rules = Vec::with_capacity(n);
        for (i, (su, m)) in snap.units.iter().zip(units).enumerate() {
            let Some(t) = self.mech.tacai.get(&su.spec.def) else {
                rules.push(None);
                continue;
            };
            let mut w = 0;
            for (k, wk) in m.u.weapons.iter().enumerate() {
                if wk.range > m.u.weapons[w].range {
                    w = k;
                }
            }
            rules.push(Some((t.skirm_range - t.skirm_leeway, t.reload_leeway, t.flee_leeway, w)));
            for (j, other) in snap.units.iter().enumerate() {
                if other.side != su.side {
                    let def = other.spec.def.as_str();
                    skirms[i * n + j] = t.skirm_range != 0.0 && (t.skirm_all || t.skirms.contains(def));
                    flees[i * n + j] = t.flee_all || t.flees.contains(def);
                }
            }
        }
        Kiting { n, rules, skirms, flees }
    }

    /// The snapshot's units for the mechanics fight. Pure EMP weapons go
    /// last with no health damage (mech.py adds them back after the base
    /// weapons); weapons doing neither are dropped.
    pub(super) fn mech_units(&self, snap: &FightSnapshot) -> Vec<MechUnit> {
        let t = &self.mech;
        let twod = snap.flags.twod;
        let pos: Vec<(f64, f64)> =
            if twod { plane(snap) } else { project(snap).into_iter().map(|x| (x, 0.0)).collect() };
        let mut us: Vec<MechUnit> = snap
            .units
            .iter()
            .zip(pos)
            .map(|(su, (x, lat))| {
                let ws: Vec<(&super::WeaponSpec, WeaponMech)> =
                    su.spec.weapons.iter().map(|w| (w, t.weapon(&w.name))).collect();
                let ordered = ws
                    .iter()
                    .filter(|(w, m)| !m.paralyzer && w.damage > 0.0)
                    .chain(ws.iter().filter(|(_, m)| m.paralyzer));
                let (mut weapons, mut mech, mut splash) = (Vec::new(), Vec::new(), Vec::new());
                for (w, m) in ordered {
                    let mut sw = self.sim_weapon(w, snap.flags);
                    if m.paralyzer {
                        sw.nominal = 0.0;
                    }
                    splash.push(if twod && !sw.hitscan { t.splash.get(&w.name).copied() } else { None });
                    weapons.push(sw);
                    mech.push(*m);
                }
                let hp = su.hp as f64;
                MechUnit {
                    u: self.sim_unit(su, x, weapons),
                    mech,
                    maxhp: if su.spec.health > 0.0 { su.spec.health as f64 } else { hp.max(1.0) },
                    air: su.spec.air,
                    kamikaze: KAMIKAZE.contains(&su.spec.def.as_str()),
                    kreach: kamikaze_reach(&su.spec.def, snap.flags.bombs),
                    kcontact: snap.flags.bombs,
                    death: t.death.get(&su.spec.def).copied(),
                    limpet: su.spec.def == "amphbomb",
                    engage: 1.0,
                    para: 0.0,
                    slowdmg: 0.0,
                    slow_last: -1_000_000_000,
                    ovs: 0.0,
                    burns: Vec::new(),
                    lat,
                    rad: if su.spec.radius > 0.0 { su.spec.radius as f64 } else { 12.0 },
                    splash,
                }
            })
            .collect();
        for (i, m) in us.iter_mut().enumerate() {
            if m.u.armed && m.u.max_range > 0.0 {
                m.engage =
                    expit(engage_features(snap, i, m.u.max_range).iter().zip(&t.engage).map(|(a, b)| a * b).sum());
            }
        }
        us
    }
}

/// mech.py `engage_features`: unit `i`'s features for firing at all, with
/// its own units in the line to its nearest enemy (2-D, within its own
/// radius plus 8 of the line, between 5% and 95% of the way) and within
/// 100 elmos of it.
fn engage_features(snap: &FightSnapshot, i: usize, max_range: f64) -> [f64; 13] {
    let me = &snap.units[i];
    let (mx, mz) = (me.x as f64, me.z as f64);
    let foes = snap.units.iter().filter(|u| u.side != me.side);
    let mut nearest: Option<(f64, f64, f64)> = None;
    let mut nfoes = 0;
    for e in foes {
        nfoes += 1;
        let (ex, ez) = (e.x as f64 - mx, e.z as f64 - mz);
        let d = ex.hypot(ez);
        if nearest.is_none_or(|n| d < n.0) {
            nearest = Some((d, ex, ez));
        }
    }
    let (mut blockers, mut crowd, mut dist) = (0, 0, f64::INFINITY);
    if let Some((d, ex, ez)) = nearest {
        dist = d;
        let l2 = ex * ex + ez * ez;
        for (j, o) in snap.units.iter().enumerate() {
            if j == i || o.side != me.side {
                continue;
            }
            let (ox, oz) = (o.x as f64 - mx, o.z as f64 - mz);
            if ox * ox + oz * oz < 100.0 * 100.0 {
                crowd += 1;
            }
            if l2 > 0.0 {
                let t = (ox * ex + oz * ez) / l2;
                if 0.05 < t && t < 0.95 && (ox * ez - oz * ex).abs() / l2.sqrt() < o.spec.radius as f64 + 8.0 {
                    blockers += 1;
                }
            }
        }
    }
    let ratio = if max_range > 0.0 { dist / max_range } else { 10.0 };
    let flag = |b: bool| if b { 1.0 } else { 0.0 };
    let b = ENGAGE_BANDS.map(|e| flag(ratio > e));
    [
        1.0,
        b[0],
        b[1],
        b[2],
        b[3],
        b[4],
        flag(max_range < 300.0),
        flag(!me.spec.mobile),
        flag(me.spec.com),
        flag(me.spec.builder),
        blockers.min(3) as f64,
        (crowd as f64).ln_1p(),
        (nfoes as f64).ln_1p(),
    ]
}

/// Damage `t`; a kill counts its cost if armed and sets off its death.
fn hurt(us: &mut [MechUnit], killed: &mut [f64; 2], t: usize, dmg: f64, f: i32) {
    if !us[t].u.alive {
        return;
    }
    us[t].u.hp -= dmg;
    if us[t].u.hp <= 0.0 {
        kill(us, killed, t, f);
    }
}

/// mech.py `kill`: no check that `t` lives (a kamikaze caught in a blast
/// still blows up when it reaches the enemy, as in the Python model).
fn kill(us: &mut [MechUnit], killed: &mut [f64; 2], t: usize, f: i32) {
    us[t].u.alive = false;
    if us[t].u.armed {
        killed[us[t].u.side as usize] += us[t].u.cost;
    }
    let Some(d) = us[t].death else { return };
    for o in 0..us.len() {
        if !us[o].u.alive || (us[t].limpet && us[o].u.side == us[t].u.side) {
            continue;
        }
        let dd = apart(&us[o], &us[t]);
        if dd < d.aoe {
            let v = d.damage * (1.0 - d.edge * dd / d.aoe);
            if d.paralyzer {
                us[o].para += v;
            } else {
                if us[t].limpet {
                    // unit_timeslow.lua scales by actual damage, with 60 frames
                    // of overslow: 60 * 0.04 / 30 = 0.08 of current health.
                    us[o].slowdmg += 10.0 * v;
                    us[o].slow_last = f;
                    us[o].ovs = us[o].ovs.max(0.08);
                }
                hurt(us, killed, o, v, f);
                if us[t].limpet {
                    us[o].cap_slow();
                }
            }
        }
    }
}

/// mech.py `simulate_mech` with air, burn, death, emp, slow, engage and
/// heatray on: expected killed share of each side's armed cost, weighted
/// over the stopping time when a hazard is given. `withdraw`: every mobile
/// A unit turns away from B at its (slowed) speed instead of fighting.
#[cfg(test)]
pub(super) fn simulate_mech(
    template: &[MechUnit],
    hits: &HitTables,
    stun_cap_s: f64,
    hazard: Option<&Hazard>,
    withdraw: bool,
) -> (f64, f64) {
    simulate_mech_flags(template, hits, stun_cap_s, hazard, withdraw, SimFlags::default())
}

/// [`simulate_mech`] in the fight `flags` asks for (the units laid out by
/// [`Model::mech_units`] with the same flags).
pub(super) fn simulate_mech_flags(
    template: &[MechUnit],
    hits: &HitTables,
    stun_cap_s: f64,
    hazard: Option<&Hazard>,
    withdraw: bool,
    flags: SimFlags,
) -> (f64, f64) {
    simulate_mech_kiting(template, hits, stun_cap_s, hazard, withdraw, flags, None)
}

/// [`simulate_mech_flags`] with ZK's tactical unit AI moving the units it
/// governs.
pub(super) fn simulate_mech_kiting(
    template: &[MechUnit],
    hits: &HitTables,
    stun_cap_s: f64,
    hazard: Option<&Hazard>,
    withdraw: bool,
    flags: SimFlags,
    kiting: Option<&Kiting>,
) -> (f64, f64) {
    let mut run = Run {
        defense: None,
        defense_engagement: None,
        hazard,
        withdraw,
        pass: false,
        focus: None,
        insertion: None,
        kiting,
        horizon: HORIZON,
        track: None,
        twod: flags.twod,
        least_health: flags.least_health(),
        bomb: None,
        bomb_ln_survive: 0.0,
    };
    run_mech(template, hits, stun_cap_s, &mut run).0
}

/// mech.py `toward` / `away`: move unit `i` `step` towards (`dir` 1) or away
/// from (`dir` -1) the point (`nx`, `nz`): along the axis, or in the plane.
fn step_to(m: &mut MechUnit, nx: f64, nz: f64, step: f64, dir: f64, twod: bool) {
    if !twod {
        m.u.x += dir * if nx > m.u.x { step } else { -step };
        return;
    }
    let (dx, dy) = (nx - m.u.x, nz - m.lat);
    let d = dx.hypot(dy);
    if d <= 1e-9 {
        m.u.x += dir * if m.u.side == 0 { step } else { -step };
        return;
    }
    m.u.x += dir * step * dx / d;
    m.lat += dir * step * dy / d;
}

/// mech.py `_separate`: ground units do not overlap. Every pair of them
/// closer than the sum of their radii is pushed apart along the line
/// between them (coincident ones across the axis, by index order), the
/// overlap shared between mobile units, none for a static; one pass, from
/// the positions at its start.
fn separate(us: &mut [MechUnit], alive: &[usize]) {
    let gs: Vec<usize> = alive.iter().copied().filter(|&i| !us[i].air).collect();
    let n = gs.len();
    if n < 2 {
        return;
    }
    let mut moves = vec![(0.0f64, 0.0f64); n];
    let mut any = false;
    for (a, &i) in gs.iter().enumerate() {
        for (b, &j) in gs.iter().enumerate() {
            if a == b {
                continue;
            }
            let (dx, dy) = (us[i].u.x - us[j].u.x, us[i].lat - us[j].lat);
            let dist = (dx * dx + dy * dy).sqrt();
            let over = us[i].rad + us[j].rad - dist;
            if over <= 0.0 {
                continue;
            }
            any = true;
            let share = match (us[i].u.mobile, us[j].u.mobile) {
                (false, _) => 0.0,
                (true, true) => 0.5,
                (true, false) => 1.0,
            };
            let (ux, uy) = if dist > 1e-6 { (dx / dist, dy / dist) } else { (0.0, if a > b { 1.0 } else { -1.0 }) };
            moves[a].0 += over * share * ux;
            moves[a].1 += over * share * uy;
        }
    }
    if !any {
        return;
    }
    for (a, &i) in gs.iter().enumerate() {
        if us[i].u.mobile && (moves[a].0 != 0.0 || moves[a].1 != 0.0) {
            us[i].u.x += moves[a].0;
            us[i].lat += moves[a].1;
        }
    }
}

/// The mechanics fight as `run` asks: the killed shares, and the units at
/// its end.
fn run_mech(template: &[MechUnit], hits: &HitTables, stun_cap_s: f64, run: &mut Run) -> ((f64, f64), Vec<MechUnit>) {
    let (hazard, withdraw) = (run.hazard, run.withdraw);
    let mut us: Vec<MechUnit> = template.to_vec();
    let n = us.len();
    let dt = DT as f64;
    let cost0 = [0u8, 1].map(|s| us.iter().filter(|m| m.u.side == s && m.u.armed).map(|m| m.u.cost).sum::<f64>());
    let mut killed = [0.0f64; 2];
    let kfrac = |killed: &[f64; 2]| [0, 1].map(|s| if cost0[s] > 0.0 { killed[s] / cost0[s] } else { 0.0 });
    let lr = (cost0[0].max(1.0) / cost0[1].max(1.0)).ln();
    let mut surv = 1.0;
    let mut exp_k = [0.0f64; 2];
    let mut quiet = 0;
    let mut f = 0;
    let mut alive: Vec<usize> = Vec::with_capacity(n);
    let mut sides: [Vec<usize>; 2] = [Vec::with_capacity(n), Vec::with_capacity(n)];
    let mut foes: Vec<usize> = Vec::with_capacity(n);
    let mut raid_focus = vec![None; n];
    let mut engagement_members = Vec::new();
    if let Some(b) = run.bomb
        && b.mode == BombMode::Down
        && us.get(b.index).is_some_and(|u| u.u.alive)
    {
        kill(&mut us, &mut killed, b.index, 0);
    }
    while f < run.horizon {
        if let Some(hz) = hazard
            && f > 0
            && f % 30 == 0
        {
            let k = kfrac(&killed);
            let h = hz.h((f / 30 - 1) as f64, k[0], k[1], lr);
            exp_k[0] += surv * h * k[0];
            exp_k[1] += surv * h * k[1];
            surv *= 1.0 - h;
        }
        alive.clear();
        alive
            .extend((0..n).filter(|&i| {
                us[i].u.alive && run.defense.is_none_or(|d| d.arrivals.get(i).copied().unwrap_or(0) <= f)
            }));
        // A delayed defender must not change the fitted firing probability
        // before it arrives. Refresh on arrivals/deaths, using only active
        // units at their current positions. Ordinary fights retain parity.
        if let Some((snap, weights)) = run.defense_engagement
            && engagement_members != alive
        {
            let active = FightSnapshot {
                flags: Default::default(),
                units: alive
                    .iter()
                    .map(|&i| {
                        let mut u = snap.units[i].clone();
                        u.x = us[i].u.x as f32;
                        u.z = us[i].lat as f32;
                        u.hp = us[i].u.hp as f32;
                        u
                    })
                    .collect(),
            };
            for (index, &i) in alive.iter().enumerate() {
                if us[i].u.armed && us[i].u.max_range > 0.0 {
                    us[i].engage = expit(
                        engage_features(&active, index, us[i].u.max_range)
                            .iter()
                            .zip(weights)
                            .map(|(a, b)| a * b)
                            .sum(),
                    );
                }
            }
            engagement_members.clone_from(&alive);
        }
        for (s, side) in sides.iter_mut().enumerate() {
            side.clear();
            side.extend(alive.iter().copied().filter(|&i| us[i].u.side as usize == s));
        }
        let armed = |s: usize| sides[s].iter().any(|&i| us[i].u.armed || us[i].kamikaze);
        let goes_on = if run.track.is_some() {
            (0..2).any(|s| armed(s) && !sides[1 - s].is_empty())
        } else {
            armed(0) && armed(1)
        };
        if !goes_on {
            if run.defense.is_some_and(|d| {
                d.arrivals
                    .iter()
                    .enumerate()
                    .any(|(i, &at)| at > f && at < run.horizon && us.get(i).is_some_and(|u| u.u.alive))
            }) {
                f += DT;
                continue;
            }
            break;
        }
        // Raiders target the economy they came for, rather than abandoning
        // a mex to chase a defender that is still on the other side of the map
        // (`RaidAim::Assets`). With `Hold` they keep only the asset they were
        // seen shooting, then pick like any unit (a defender that has not
        // arrived is not on the field, so cannot draw fire); `Free` holds none.
        if let Some(d) = run.defense {
            raid_focus.fill(None);
            for &i in &sides[1] {
                let held = d.focus.get(i).copied().flatten().filter(|j| d.assets.contains(j) && sides[0].contains(j));
                raid_focus[i] = match d.raid_aim {
                    RaidAim::Assets => held.or_else(|| {
                        d.assets
                            .iter()
                            .copied()
                            .filter(|j| sides[0].contains(j))
                            .min_by(|&a, &b| apart(&us[a], &us[i]).total_cmp(&apart(&us[b], &us[i])))
                    }),
                    RaidAim::Hold => held,
                    RaidAim::Free => None,
                };
            }
        }
        // Status: burns tick, paralysis and slow wear off (units killed by a
        // burn earlier in order still wear off, as in the Python model).
        for &i in &alive {
            if !us[i].burns.is_empty() {
                let b: f64 = us[i].burns.iter().filter(|(end, _)| *end > f).map(|(_, r)| r).sum();
                us[i].burns.retain(|(end, _)| *end > f + DT);
                if b > 0.0 {
                    hurt(&mut us, &mut killed, i, b * dt, f);
                }
            }
            let m = &mut us[i];
            if m.para > 0.0 {
                m.para = (m.para - m.maxhp / STUN_DECAY_S * dt / 30.0).max(0.0);
            }
            if m.slowdmg > 0.0 && f - m.slow_last > SLOW_GRACE {
                m.slowdmg = (m.slowdmg - SLOW_DECAY * m.u.hp.max(1.0) * dt / 30.0).max(0.0);
            }
            if m.ovs > 0.0 {
                m.ovs = (m.ovs - SLOW_DECAY * dt / 30.0).max(0.0);
            }
            if m.slowdmg > 0.0 {
                m.cap_slow();
            }
        }
        alive.retain(|&i| us[i].u.alive);
        // Movement towards the nearest enemy alive at the top of the step
        // (mech.py reads the step's opening sides here).
        let mut moved = false;
        for &i in &alive {
            us[i].u.moving = false;
            if let Some(p) = run.insertion.filter(|p| i < p.count && f < p.end + DT) {
                let t = ((f - p.wait) as f64 / (p.end - p.wait) as f64).clamp(0.0, 1.0);
                us[i].u.x = template[i].u.x + p.displacement * t;
                if f < p.end {
                    us[i].u.moving = f >= p.wait;
                    moved |= us[i].u.moving;
                    continue;
                }
            }
            if !us[i].u.mobile || us[i].u.speed <= 0.0 || us[i].stunned() {
                continue;
            }
            let side = us[i].u.side as usize;
            let mut nearest: Option<(f64, f64, f64, usize)> = None;
            for &j in &sides[1 - side] {
                if side == 0 && run.focus.is_some_and(|target| target != j)
                    || side == 1 && raid_focus[i].is_some_and(|target| target != j)
                {
                    continue;
                }
                let d = apart(&us[j], &us[i]);
                if nearest.is_none_or(|n| d < n.0) {
                    nearest = Some((d, us[j].u.x, us[j].lat, j));
                }
            }
            let Some((dist, nx, nz, nj)) = nearest else { continue };
            let speed = us[i].u.speed * (1.0 - us[i].slowed());
            if run.pass && side == 0 {
                if us[i].kamikaze && goes_off(&us[i], &us[nj], dist) {
                    kill(&mut us, &mut killed, i, f);
                    continue;
                }
                us[i].u.x += speed * dt;
                us[i].u.moving = true;
                moved = true;
                continue;
            }
            if withdraw && side == 0 {
                step_to(&mut us[i], nx, nz, speed * dt, -1.0, run.twod);
                us[i].u.moving = true;
                moved = true;
                continue;
            }
            if run.defense.is_some() {
                if us[i].kamikaze && goes_off(&us[i], &us[nj], dist) {
                    kill(&mut us, &mut killed, i, f);
                    continue;
                }
                if us[i].u.armed && dist > us[i].u.max_range * 0.95 {
                    let step = (speed * dt).min((dist - us[i].u.max_range * 0.9).max(0.0));
                    let fraction = step / dist.max(1.0);
                    us[i].u.x += (nx - us[i].u.x) * fraction;
                    us[i].lat += (nz - us[i].lat) * fraction;
                    us[i].u.moving = step > 0.0;
                    moved |= step > 0.0;
                }
                continue;
            }
            if us[i].kamikaze {
                if goes_off(&us[i], &us[nj], dist) {
                    kill(&mut us, &mut killed, i, f);
                    continue;
                }
                let step = (speed * dt).min(dist);
                step_to(&mut us[i], nx, nz, step, 1.0, run.twod);
                us[i].u.moving = true;
                moved = true;
                continue;
            }
            if !us[i].u.armed {
                continue;
            }
            // ZK's tactical AI: only the closest enemy decides; skirm it at
            // range (more while reloading) or flee it beyond its range.
            if let Some(want) = run.kiting.and_then(|k| k.want(&us, i, nj, dist, f)) {
                if dist < want - 1.0 {
                    let step = (speed * dt).min(want - dist);
                    step_to(&mut us[i], nx, nz, step, -1.0, run.twod);
                    us[i].u.moving = true;
                    moved = true;
                } else if dist > want + 1.0 {
                    let step = (speed * dt).min(dist - want);
                    step_to(&mut us[i], nx, nz, step, 1.0, run.twod);
                    us[i].u.moving = true;
                    moved = true;
                }
                continue;
            }
            if dist > us[i].u.max_range * 0.95 {
                let step = (speed * dt).min(dist - us[i].u.max_range * 0.9);
                step_to(&mut us[i], nx, nz, step, 1.0, run.twod);
                us[i].u.moving = true;
                moved = true;
            }
        }
        // mech.py runs the overlap pass on the step's opening list (a
        // kamikaze that blew up in the moves still pushes).
        if run.twod {
            separate(&mut us, &alive);
        }
        alive.retain(|&i| us[i].u.alive);
        // Fire: every reloaded weapon at its nearest living target in range
        // that it can shoot, its expected damage times the shooter's chance
        // of engaging at all, with burn, paralysis and slow riding along.
        let mut shot_fired = false;
        for &i in &alive {
            if !us[i].u.armed
                || !us[i].u.alive
                || us[i].stunned()
                || run.insertion.is_some_and(|p| i < p.count && f < p.end)
            {
                continue;
            }
            let side = us[i].u.side as usize;
            foes.clear();
            foes.extend(sides[1 - side].iter().copied().filter(|&j| {
                us[j].u.alive
                    && (side != 0 || run.focus.is_none_or(|target| target == j))
                    && (side != 1 || raid_focus[i].is_none_or(|target| target == j))
            }));
            if foes.is_empty() {
                continue;
            }
            let sf = us[i].slowed();
            let eff = us[i].engage;
            for wi in 0..us[i].u.weapons.len() {
                let (key, range, reload, hitscan, salvo, nominal, next) = {
                    let w = &us[i].u.weapons[wi];
                    (w.key, w.range, w.reload, w.hitscan, w.salvo, w.nominal, w.next)
                };
                if f < next {
                    continue;
                }
                let m = us[i].mech[wi];
                let mut target: Option<(f64, usize)> = None;
                for &j in &foes {
                    let e = &us[j];
                    let d = apart(e, &us[i]);
                    // focus: the least health left first, then the nearest (mech.py `focus`)
                    let better = |t: (f64, usize)| {
                        if run.least_health {
                            e.u.hp < us[t.1].u.hp || (e.u.hp == us[t.1].u.hp && d < t.0)
                        } else {
                            d < t.0
                        }
                    };
                    if e.u.alive
                        && d <= range
                        && (if e.air { m.can_air } else { m.can_ground })
                        && target.is_none_or(better)
                    {
                        target = Some((d, j));
                    }
                }
                let Some((dist, j)) = target else { continue };
                // Tube weapons (`sreload`) fire only from a loaded tube.
                if !us[i].u.weapons[wi].take_tube(f, sf) {
                    continue;
                }
                us[i].u.weapons[wi].next = f + if sf > 0.0 { (reload as f64 / (1.0 - sf)) as i32 } else { reload };
                shot_fired = true;
                let rb = if range > 0.0 { ((RBANDS as f64 * dist / range) as i32).min(RBANDS) } else { RBANDS };
                let sb = speed_band(if us[j].u.moving { us[j].u.speed } else { 0.0 });
                let hit = hits.lookup(key, us[j].u.target, rb as u8, sb);
                let frac = m.dyn_frac.and_then(|d| d[rb as usize]).unwrap_or(hit.frac);
                let shots = if hitscan { 1.0 } else { salvo as f64 };
                // The plane fight lands splash on the units around the target
                // below; the hit tables' damage per hit already holds it, so
                // the target is charged that much less (mech.py `aoe`).
                let splash = if run.twod { us[i].splash[wi] } else { None };
                let frac_t = splash.map_or(frac, |sp| (frac - sp.geo / hit.p_any.max(0.05)).max(0.0));
                let dmg = hit.p_tgt * frac_t * nominal * shots * eff;
                if let Some(b) = run.bomb
                    && b.mode == BombMode::Charge
                    && j == b.index
                    && frac_t * nominal >= us[j].u.hp
                {
                    // One hit kills it: in this branch it got through, by
                    // the chance that all of this volley missed.
                    run.bomb_ln_survive += shots * (1.0 - (hit.p_tgt * eff).clamp(0.0, 1.0)).max(1e-9).ln();
                    continue;
                }
                let t = &mut us[j];
                if m.burn > 0.0 && m.burn_frames > 0 {
                    t.burns.push((f + m.burn_frames, m.burn * hit.p_tgt * shots * eff / m.burn_frames as f64));
                }
                if m.emp > 0.0 {
                    t.para += m.emp * hit.p_tgt * shots * eff * (t.maxhp / t.u.hp.max(1.0));
                    t.para = t.para.min(t.maxhp * (1.0 + stun_cap_s / STUN_DECAY_S));
                }
                if m.slow > 0.0 {
                    t.slowdmg += m.slow * nominal * frac * hit.p_tgt * shots * eff;
                    t.ovs = t.ovs.max(m.overslow);
                    t.cap_slow();
                    t.slow_last = f;
                }
                if let Some(sp) = splash {
                    // engine falloff from each unit's edge, kappa by side; the
                    // target's domain only, never the shooter
                    let t_air = us[j].air;
                    for o in 0..n {
                        if o == j || o == i || !us[o].u.alive || us[o].air != t_air {
                            continue;
                        }
                        let dd = apart(&us[o], &us[j]) - us[o].rad;
                        if dd >= sp.radius {
                            continue;
                        }
                        let dd = dd.max(0.0);
                        let den = sp.radius - dd * sp.edge;
                        let falloff = if den > 1e-6 { (sp.radius - dd) / den } else { 1.0 };
                        let kappa = if us[o].u.side == us[j].u.side { sp.kappa_target } else { sp.kappa_shooter };
                        let v = kappa * falloff * nominal * shots * eff;
                        if v > 0.0 {
                            hurt(&mut us, &mut killed, o, v, f);
                        }
                    }
                }
                if dmg > 0.0 {
                    if let Some(tr) = run.track.as_mut() {
                        let (p, d1) = (hit.p_tgt * eff, frac_t * nominal);
                        tr.var[j] += shots * p * (1.0 - p) * d1 * d1;
                    }
                    hurt(&mut us, &mut killed, j, dmg, f);
                }
            }
        }
        if let Some(tr) = run.track.as_mut() {
            for (d, m) in tr.death_f.iter_mut().zip(&us) {
                if !m.u.alive && d.is_none() {
                    *d = Some(f);
                }
            }
            f += DT;
            continue;
        }
        quiet = if shot_fired || (moved && !withdraw) { 0 } else { quiet + DT };
        if quiet > if withdraw { 150 } else { 300 } {
            break;
        }
        f += DT;
    }
    let k = kfrac(&killed);
    let shares = if hazard.is_some() { (exp_k[0] + surv * k[0], exp_k[1] + surv * k[1]) } else { (k[0], k[1]) };
    (shares, us)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::military::fightsim::{FightOutcome, SnapUnit, UnitSpec, WeaponSpec};

    const UNITS: &str = include_str!("testdata/mech_parity_units.tsv");
    const CASES: &str = include_str!("testdata/mech_parity_cases.tsv");
    const FATES: &str = include_str!("testdata/mech_fates_cases.tsv");
    const TACAI_UNITS: &str = include_str!("testdata/mech_tacai_units.tsv");
    const TACAI_CASES: &str = include_str!("testdata/mech_tacai_cases.tsv");
    const VARIANT_UNITS: &str = include_str!("testdata/mech_variants_units.tsv");
    const VARIANT_CASES: &str = include_str!("testdata/mech_variants_cases.tsv");
    const VARIANT_FATES: &str = include_str!("testdata/mech_variants_fates.tsv");
    const SRELOAD_UNITS: &str = include_str!("testdata/mech_sreload_units.tsv");
    const SRELOAD_CASES: &str = include_str!("testdata/mech_sreload_cases.tsv");
    const SRELOAD_FATES: &str = include_str!("testdata/mech_sreload_fates.tsv");
    const BOMBS_UNITS: &str = include_str!("testdata/mech_bombs_units.tsv");
    const BOMBS_CASES: &str = include_str!("testdata/mech_bombs_cases.tsv");
    const BOMBS_FATES: &str = include_str!("testdata/mech_bombs_fates.tsv");

    /// export_mech.VARIANTS by name (both with `sreload` since 2026-11-03).
    fn variant(name: &str) -> SimFlags {
        match name {
            "base" => SimFlags::default(),
            "focus" => SimFlags::from_switches(false, true, true),
            "2d" => SimFlags::from_switches(true, false, true),
            "sr" => SimFlags::from_switches(false, false, true),
            v => panic!("variant {v:?}"),
        }
    }
    /// Absolute tolerance on killed shares against the Python model.
    const TOL: f64 = 1e-3;

    fn specs() -> HashMap<String, Arc<UnitSpec>> {
        specs_from(UNITS)
    }

    fn specs_from(units: &str) -> HashMap<String, Arc<UnitSpec>> {
        data_lines(units)
            .map(|l| {
                let f: Vec<&str> = l.split('\t').collect();
                let weapons = f[9]
                    .split('|')
                    .filter(|w| !w.is_empty())
                    .map(|w| {
                        let p: Vec<&str> = w.split(':').collect();
                        WeaponSpec {
                            name: p[0].into(),
                            weapon_type: p[1].into(),
                            range: p[2].parse().unwrap(),
                            reload: p[3].parse().unwrap(),
                            salvo: p[4].parse().unwrap(),
                            damage: p[5].parse().unwrap(),
                        }
                    })
                    .collect();
                let spec = UnitSpec {
                    def: f[0].into(),
                    cost: f[1].parse().unwrap(),
                    health: f[2].parse().unwrap(),
                    speed: f[3].parse().unwrap(),
                    mobile: f[4] == "1",
                    air: f[5] == "1",
                    builder: f[6] == "1",
                    com: f[7] == "1",
                    radius: f[8].parse().unwrap(),
                    weapons,
                };
                (f[0].to_owned(), Arc::new(spec))
            })
            .collect()
    }

    /// A case's units, `side:def:hp:x:z` separated by `;`.
    fn snapshot(units: &str, specs: &HashMap<String, Arc<UnitSpec>>) -> FightSnapshot {
        let units = units
            .split(';')
            .map(|u| {
                let p: Vec<&str> = u.split(':').collect();
                SnapUnit {
                    side: p[0].parse().unwrap(),
                    spec: specs[p[1]].clone(),
                    hp: p[2].parse().unwrap(),
                    x: p[3].parse().unwrap(),
                    z: p[4].parse().unwrap(),
                }
            })
            .collect();
        FightSnapshot { flags: Default::default(), units }
    }

    fn cases() -> Vec<(String, FightSnapshot, [f64; 6])> {
        let specs = specs();
        data_lines(CASES)
            .map(|l| {
                let f: Vec<&str> = l.split('\t').collect();
                let mut want = [0.0; 6];
                for (w, s) in want.iter_mut().zip(&f[2..8]) {
                    *w = s.parse().unwrap();
                }
                (f[0].to_owned(), snapshot(f[1], &specs), want)
            })
            .collect()
    }

    #[test]
    fn insertion_cannot_fire_early_and_keeps_incoming_damage() {
        let model = Model::load().unwrap();
        let defs = specs();
        let mut snap = FightSnapshot {
            flags: Default::default(),
            units: vec![
                SnapUnit { side: 0, spec: defs["hoverassault"].clone(), hp: 1450.0, x: 0.0, z: 0.0 },
                SnapUnit { side: 1, spec: defs["cloakriot"].clone(), hp: 820.0, x: 100.0, z: 0.0 },
            ],
        };
        // Only the passenger has a weapon in the first comparison.
        Arc::make_mut(&mut snap.units[1].spec).weapons.clear();
        let mode = FateMode::Assassinate { target: 1 };
        let insertion = Insertion { count: 1, dx: 50.0, dz: 0.0, wait: 45, flight: 155 };
        let early = model.fates_with_insertion(&snap, 198, mode, &[], Some(insertion));
        assert_eq!(early[1].hp_left, 820.0);
        let landed = model.fates_with_insertion(&snap, 600, mode, &[], Some(insertion));
        assert!(landed[1].hp_left < 820.0);
        assert!(model.fates(&snap, 198, mode)[1].hp_left < 820.0);
        snap.units[1].spec = defs["cloakriot"].clone();
        let exposed = model.fates_with_insertion(&snap, 198, mode, &[], Some(insertion));
        assert!(exposed[0].hp_left < 1450.0, "transport must not grant invulnerability");
        assert_eq!(exposed[1].hp_left, 820.0, "no passenger damage before landing");
    }

    #[test]
    fn insertion_crosses_a_gap_that_walking_cannot_cross_in_time() {
        let model = Model::load().unwrap();
        let defs = specs();
        let snap = FightSnapshot {
            flags: Default::default(),
            units: vec![
                SnapUnit { side: 0, spec: defs["hoverassault"].clone(), hp: 1450.0, x: 0.0, z: 0.0 },
                SnapUnit { side: 1, spec: defs["cloakcon"].clone(), hp: 600.0, x: 1000.0, z: 0.0 },
            ],
        };
        let mode = FateMode::Assassinate { target: 1 };
        let walk = model.fates(&snap, 210, mode);
        let fly = model.fates_with_insertion(
            &snap,
            210,
            mode,
            &[],
            Some(Insertion { count: 1, dx: 850.0, dz: 0.0, wait: 0, flight: 100 }),
        );
        assert_eq!(walk[1].hp_left, 600.0);
        assert!(fly[1].hp_left < walk[1].hp_left);
    }

    #[test]
    fn buoy_throw_can_kill_lance_but_a_ruined_payload_is_refused() {
        let model = Model::load().unwrap();
        // Live unit/weapon values; the opening gap exceeds Lance range.
        let buoy = Arc::new(UnitSpec {
            def: "amphfloater".into(),
            cost: 280.0,
            health: 1250.0,
            speed: 51.0 / 30.0,
            mobile: true,
            air: false,
            builder: false,
            com: false,
            radius: 20.0,
            weapons: vec![WeaponSpec {
                name: "amphfloater_cannon".into(),
                range: 450.0,
                reload: 1.8,
                salvo: 1,
                damage: 150.1,
                weapon_type: "Cannon".into(),
            }],
        });
        let lance = Arc::new(UnitSpec {
            def: "hoverarty".into(),
            cost: 1100.0,
            health: 1000.0,
            speed: 49.5 / 30.0,
            mobile: true,
            air: false,
            builder: false,
            com: false,
            radius: 24.0,
            weapons: vec![WeaponSpec {
                name: "hoverarty_ata".into(),
                range: 980.0,
                reload: 23.0,
                salvo: 1,
                damage: 3000.1,
                weapon_type: "BeamLaser".into(),
            }],
        });
        let mut snap = FightSnapshot {
            flags: Default::default(),
            units: (0..4)
                .map(|i| SnapUnit { side: 0, spec: buoy.clone(), hp: 1250.0, x: i as f32 * 30.0 - 45.0, z: 0.0 })
                .collect(),
        };
        snap.units.push(SnapUnit { side: 1, spec: lance, hp: 1000.0, x: 0.0, z: 1000.0 });
        let mode = FateMode::Assassinate { target: 4 };
        let flight = Insertion { count: 4, dx: 0.0, dz: 570.4, wait: 45, flight: 155 };
        let healthy = model.fates_with_insertion(&snap, 1350, mode, &[], Some(flight));
        assert!(healthy[4].p_dead >= 0.55, "healthy Buoys: {:?}", healthy[4]);
        for u in &mut snap.units[..4] {
            u.hp = 1.0;
        }
        let damaged = model.fates_with_insertion(&snap, 1350, mode, &[], Some(flight));
        assert!(damaged[4].p_dead < 0.55, "nearly dead Buoys: {:?}", damaged[4]);
    }

    #[test]
    fn tables_load_with_fitting_header() {
        let t = MechTables::load().unwrap();
        assert!(t.fitted.contains("fitted 20"), "{}", t.fitted);
        assert!(t.weapons.len() > 100 && t.death.len() > 50);
        // Kodachi burns, Bolas slows, Faraday is pure EMP, Scorcher's heat
        // falls off with range, Angler cannot shoot the ground.
        assert!(t.weapon("tankraid_napalm_bomblet").burn > 0.0);
        assert!(t.weapon("hoverheavyraid_disruptor").slow > 0.0);
        // Slow from the fixed def dump (round 6): Buoy and Moderator slow,
        // Moderator pushes past the cap; slow-only beams are left out.
        assert!(t.weapon("amphfloater_cannon").slow > 0.0);
        let moderator = t.weapon("jumpskirm_disruptor_beam");
        assert!(moderator.slow > 0.0 && moderator.overslow > 0.0);
        assert_eq!(t.weapon("tankheavyassault_slowbeam").slow, 0.0);
        assert!(t.weapon("turretemp_arm_det_weapon").paralyzer);
        let heat = t.weapon("vehraid_heatray").dyn_frac.unwrap();
        assert!(heat[0].unwrap() > heat[4].unwrap());
        let aa = t.weapon("amphaa_missile");
        assert!(aa.can_air && !aa.can_ground);
        assert!(t.death["cloakbomb"].paralyzer);
        assert!(t.stun_cap_s > 0.0);
    }

    /// The hand-built fights against mech.simulate_mech's numbers
    /// (testdata/mech_parity_cases.tsv, written by export_mech.py): one
    /// case per mechanism.
    #[test]
    fn parity_with_python_mechanics() {
        let m = Model::load().unwrap();
        let cases = cases();
        assert!(cases.len() >= 12);
        let mut worst_all = 0.0f64;
        for (name, snap, want) in &cases {
            let units = m.mech_units(snap);
            let s = m.mech.stun_cap_s;
            let dps = simulate_mech(&units, &m.hits, s, None, false);
            let stop = simulate_mech(&units, &m.hits, s, Some(&m.hazard), false);
            let wd = simulate_mech(&units, &m.hits, s, Some(&m.hazard), true);
            let got = [dps.0, dps.1, stop.0, stop.1, wd.0, wd.1];
            let worst = got.iter().zip(want).map(|(g, w)| (g - w).abs()).fold(0.0, f64::max);
            eprintln!("mech parity {name:16} worst |rust - python| = {worst:.1e}");
            assert!(worst <= TOL, "{name}: got {got:?}, python {want:?}");
            worst_all = worst_all.max(worst);
            let out = m.predict_with(snap, true).mech.unwrap();
            assert!((out.killed_a as f64 - stop.0).abs() < 1e-6 && (out.withdraw_b as f64 - wd.1).abs() < 1e-6);
            assert!((0.0..=1.0).contains(&out.p_a_wins) && (0.0..=1.0).contains(&out.mean_a));
        }
        eprintln!("mech parity: {} cases, worst |rust - python| = {worst_all:.2e}", cases.len());
    }

    /// The tactical AI against mech.py with `tacai` (testdata/mech_tacai_cases.tsv,
    /// written by `export_mech.py ... tacai`): the fight with and without the
    /// hazard, A's withdrawal, and per-unit fates at the case's horizon.
    #[test]
    fn tacai_parity_with_python() {
        let m = Model::load().unwrap();
        let specs = specs_from(TACAI_UNITS);
        let s = m.mech.stun_cap_s;
        let mut n = 0;
        for l in data_lines(TACAI_CASES) {
            let f: Vec<&str> = l.split('\t').collect();
            let snap = snapshot(f[1], &specs);
            let units = m.mech_units(&snap);
            let k = m.kiting(&snap, &units);
            let want: Vec<f64> = f[2..8].iter().map(|v| v.parse().unwrap()).collect();
            let dps = simulate_mech_kiting(&units, &m.hits, s, None, false, SimFlags::default(), Some(&k));
            let stop = simulate_mech_kiting(&units, &m.hits, s, Some(&m.hazard), false, SimFlags::default(), Some(&k));
            let wd = simulate_mech_kiting(&units, &m.hits, s, Some(&m.hazard), true, SimFlags::default(), Some(&k));
            let got = [dps.0, dps.1, stop.0, stop.1, wd.0, wd.1];
            for (g, w) in got.iter().zip(&want) {
                assert!((g - w).abs() <= TOL, "{}: got {got:?}, python {want:?}", f[0]);
            }
            let fates = m.kiting_fates(&snap, f[8].parse().unwrap());
            let want: Vec<Vec<&str>> = f[9].split(';').map(|u| u.split(':').collect()).collect();
            assert_eq!(fates.len(), want.len());
            for (i, (g, w)) in fates.iter().zip(&want).enumerate() {
                let death: i32 = w[1].parse().unwrap();
                let (hp, pd): (f64, f64) = (w[2].parse().unwrap(), w[3].parse().unwrap());
                assert_eq!(g.dead, w[0] == "1", "{} unit {i}: {g:?} vs {w:?}", f[0]);
                assert_eq!(g.death_frame, (death >= 0).then_some(death), "{} unit {i}", f[0]);
                assert!((g.hp_left as f64 - hp).abs() <= 0.5 + 1e-3 * hp, "{} unit {i}: {g:?} vs {w:?}", f[0]);
                assert!((g.p_raw as f64 - pd).abs() <= TOL, "{} unit {i}: {g:?} vs {w:?}", f[0]);
            }
            n += 1;
        }
        assert!(n >= 10);
    }

    /// The case the veto exists for: a lone Knight walking into five
    /// Scalpels dies without hurting them once they kite, and the plain
    /// a-move (no tactical AI) has it kill some first.
    #[test]
    fn scalpels_kite_a_knight_to_death() {
        let m = Model::load().unwrap();
        let specs = specs_from(TACAI_UNITS);
        let mut units = vec![SnapUnit { side: 0, spec: specs["cloakassault"].clone(), hp: 2500.0, x: 0.0, z: 0.0 }];
        for i in 0..5 {
            units.push(SnapUnit {
                side: 1,
                spec: specs["hoverskirm"].clone(),
                hp: 660.0,
                x: 600.0,
                z: 25.0 * i as f32,
            });
        }
        let snap = FightSnapshot { flags: Default::default(), units };
        let kiting = m.kiting_fates(&snap, 450);
        assert!(kiting[0].dead && kiting[0].p_raw > 0.99, "{:?}", kiting[0]);
        assert!(kiting[1..].iter().all(|f| !f.dead && f.p_raw < 0.01 && f.hp_left >= 659.0), "{kiting:?}");
        let plain = m.fates(&snap, 450, FateMode::AMove);
        assert!(plain[1..].iter().any(|f| f.hp_left < 659.0), "{plain:?}");
    }

    #[test]
    fn observed_paralysis_expires_and_the_defender_fires_again() {
        let model = Model::load().unwrap();
        let defs = specs();
        let snap = FightSnapshot {
            flags: Default::default(),
            units: vec![
                SnapUnit { side: 0, spec: defs["hoverassault"].clone(), hp: 30.0, x: 0.0, z: 0.0 },
                SnapUnit { side: 1, spec: defs["cloakriot"].clone(), hp: 100000.0, x: 100.0, z: 0.0 },
            ],
        };
        let mode = FateMode::Assassinate { target: 1 };
        let long = model.fates_with_paralysis(&snap, 300, mode, &[(1, 2.0)]);
        let short = model.fates_with_paralysis(&snap, 300, mode, &[(1, 1.05)]);
        let awake = model.fates(&snap, 300, mode);
        assert_eq!(long[0].hp_left, 30.0);
        assert!(short[0].dead && awake[0].dead);
        assert_eq!(paralysis_frames(1.5), 600);
        assert_eq!(paralysis_frames(1.0), 0);
        assert_eq!(paralysis_frames(f32::NAN), 0);
    }

    #[test]
    fn assassination_ignores_a_nearer_decoy_and_stops_after_the_kill() {
        let model = Model::load().unwrap();
        let defs = specs();
        let ours = defs["hoverassault"].clone();
        let mut decoy = (*defs["cloakriot"]).clone();
        decoy.weapons.clear();
        decoy.mobile = false;
        decoy.health = 1.0;
        let decoy = Arc::new(decoy);
        let snap = FightSnapshot {
            flags: Default::default(),
            units: vec![
                SnapUnit { side: 0, spec: ours, hp: 1450.0, x: 0.0, z: 0.0 },
                SnapUnit { side: 1, spec: decoy.clone(), hp: 1.0, x: 50.0, z: 0.0 },
                SnapUnit { side: 1, spec: decoy, hp: 1.0, x: 150.0, z: 0.0 },
            ],
        };
        let focused = model.fates(&snap, 300, FateMode::Assassinate { target: 2 });
        let normal = model.fates(&snap, 300, FateMode::AMove);
        assert!(focused[2].dead);
        assert!(!focused[1].dead);
        assert_eq!(focused[1].hp_left, 1.0);
        assert!(normal[1].dead && normal[2].dead);
    }

    /// Fates against mech.simulate_mech(fates=...) (testdata/mech_fates_cases.tsv,
    /// written by export_mech.py): the a-move, eco targets, imps, and passes
    /// at offsets 0, 120 and 220.
    #[test]
    fn fates_parity_with_python() {
        let m = Model::load().unwrap();
        let specs = specs();
        let mut n = 0;
        for l in data_lines(FATES) {
            let f: Vec<&str> = l.split('\t').collect();
            let snap = snapshot(f[1], &specs);
            let mode = if f[2].is_empty() { FateMode::AMove } else { FateMode::Pass { offset: f[2].parse().unwrap() } };
            let got = m.fates(&snap, f[3].parse().unwrap(), mode);
            let want: Vec<Vec<&str>> = f[4].split(';').map(|u| u.split(':').collect()).collect();
            assert_eq!(got.len(), snap.units.len());
            assert_eq!(got.len(), want.len());
            for (i, (g, w)) in got.iter().zip(&want).enumerate() {
                let death: i32 = w[1].parse().unwrap();
                let (hp, pd): (f64, f64) = (w[2].parse().unwrap(), w[3].parse().unwrap());
                assert_eq!(g.dead, w[0] == "1", "{} unit {i}: {g:?} vs {w:?}", f[0]);
                assert_eq!(g.death_frame, (death >= 0).then_some(death), "{} unit {i}", f[0]);
                assert!((g.hp_left as f64 - hp).abs() <= 0.5 + 1e-3 * hp, "{} unit {i}: {g:?} vs {w:?}", f[0]);
                assert!((g.p_raw as f64 - pd).abs() <= TOL, "{} unit {i}: {g:?} vs {w:?}", f[0]);
                assert!(g.p_dead > 0.0 && g.p_dead < 1.0);
            }
            n += 1;
        }
        assert!(n >= 7);
    }

    /// The `fightfocus` / `fight2d` fights against mech.simulate_mech with
    /// their opts (testdata/mech_variants_cases.tsv, `export_mech.py ...
    /// variants`): the parity crowds and line, rings round a Kodachi, spread
    /// Scorchers and a splash blob, without and with the hazard and with A
    /// withdrawing.
    #[test]
    fn variants_parity_with_python() {
        let m = Model::load().unwrap();
        let specs = specs_from(VARIANT_UNITS);
        let s = m.mech.stun_cap_s;
        let (mut n, mut worst_all) = (0, 0.0f64);
        for l in data_lines(VARIANT_CASES) {
            let f: Vec<&str> = l.split('\t').collect();
            let flags = variant(f[1]);
            let mut snap = snapshot(f[2], &specs);
            snap.flags = flags;
            let units = m.mech_units(&snap);
            let want: Vec<f64> = f[3..9].iter().map(|v| v.parse().unwrap()).collect();
            let dps = simulate_mech_flags(&units, &m.hits, s, None, false, flags);
            let stop = simulate_mech_flags(&units, &m.hits, s, Some(&m.hazard), false, flags);
            let wd = simulate_mech_flags(&units, &m.hits, s, Some(&m.hazard), true, flags);
            let got = [dps.0, dps.1, stop.0, stop.1, wd.0, wd.1];
            let worst = got.iter().zip(&want).map(|(g, w)| (g - w).abs()).fold(0.0, f64::max);
            assert!(worst <= TOL, "{} {}: got {got:?}, python {want:?}", f[0], f[1]);
            worst_all = worst_all.max(worst);
            let out = m.predict_with(&snap, true).mech.unwrap();
            assert!((out.killed_a as f64 - stop.0).abs() < 1e-6 && (out.withdraw_b as f64 - wd.1).abs() < 1e-6);
            n += 1;
        }
        eprintln!("variant parity: {n} cases, worst |rust - python| = {worst_all:.2e}");
        assert!(n >= 40);
    }

    /// The variants' fates against mech.simulate_mech(fates=...) with their
    /// opts (testdata/mech_variants_fates.tsv).
    #[test]
    fn variant_fates_parity_with_python() {
        let m = Model::load().unwrap();
        let specs = specs_from(VARIANT_UNITS);
        let mut n = 0;
        for l in data_lines(VARIANT_FATES) {
            let f: Vec<&str> = l.split('\t').collect();
            let mut snap = snapshot(f[2], &specs);
            snap.flags = variant(f[1]);
            let got = m.fates(&snap, f[3].parse().unwrap(), FateMode::AMove);
            let want: Vec<Vec<&str>> = f[4].split(';').map(|u| u.split(':').collect()).collect();
            assert_eq!(got.len(), want.len());
            for (i, (g, w)) in got.iter().zip(&want).enumerate() {
                let death: i32 = w[1].parse().unwrap();
                let (hp, pd): (f64, f64) = (w[2].parse().unwrap(), w[3].parse().unwrap());
                assert_eq!(g.dead, w[0] == "1", "{} {} unit {i}: {g:?} vs {w:?}", f[0], f[1]);
                assert_eq!(g.death_frame, (death >= 0).then_some(death), "{} {} unit {i}", f[0], f[1]);
                assert!((g.hp_left as f64 - hp).abs() <= 0.5 + 1e-3 * hp, "{} {} unit {i}: {g:?} vs {w:?}", f[0], f[1]);
                assert!((g.p_raw as f64 - pd).abs() <= TOL, "{} {} unit {i}: {g:?} vs {w:?}", f[0], f[1]);
            }
            n += 1;
        }
        assert!(n >= 12);
    }

    /// `sreload` (tube weapons fire from tubes that each reload on their own
    /// clock) against mech.simulate_mech with each variant's opts + sreload
    /// (testdata/mech_sreload_cases.tsv, `export_sreload.py`): Pickets and
    /// Ogres against raiders and assaults, crowded and ringed, ours and theirs.
    /// Each case also carries Python's answer with the switch off.
    #[test]
    fn sreload_parity_with_python() {
        let m = Model::load().unwrap();
        let specs = specs_from(SRELOAD_UNITS);
        let s = m.mech.stun_cap_s;
        let (mut n, mut changed, mut worst_all) = (0, 0, 0.0f64);
        for l in data_lines(SRELOAD_CASES) {
            let f: Vec<&str> = l.split('\t').collect();
            let flags = SimFlags { sreload: true, ..variant(f[1]) };
            let mut snap = snapshot(f[2], &specs);
            snap.flags = flags;
            let units = m.mech_units(&snap);
            let want: Vec<f64> = f[3..11].iter().map(|v| v.parse().unwrap()).collect();
            let dps = simulate_mech_flags(&units, &m.hits, s, None, false, flags);
            let stop = simulate_mech_flags(&units, &m.hits, s, Some(&m.hazard), false, flags);
            let wd = simulate_mech_flags(&units, &m.hits, s, Some(&m.hazard), true, flags);
            let mut off = snap.clone();
            off.flags.sreload = false;
            let o = simulate_mech_flags(&m.mech_units(&off), &m.hits, s, None, false, off.flags);
            let got = [dps.0, dps.1, stop.0, stop.1, wd.0, wd.1, o.0, o.1];
            let worst = got.iter().zip(&want).map(|(g, w)| (g - w).abs()).fold(0.0, f64::max);
            assert!(worst <= TOL, "{} {}: got {got:?}, python {want:?}", f[0], f[1]);
            worst_all = worst_all.max(worst);
            changed += usize::from((o.0 - dps.0).abs() + (o.1 - dps.1).abs() > 1e-6);
            n += 1;
        }
        eprintln!(
            "sreload parity: {n} cases ({changed} changed by the switch), worst |rust - python| = {worst_all:.2e}"
        );
        assert!(n >= 12 && changed >= 4);
    }

    /// The `sreload` fates against mech.simulate_mech(fates=...) with each
    /// variant's opts + sreload (testdata/mech_sreload_fates.tsv).
    #[test]
    fn sreload_fates_parity_with_python() {
        let m = Model::load().unwrap();
        let specs = specs_from(SRELOAD_UNITS);
        let mut n = 0;
        for l in data_lines(SRELOAD_FATES) {
            let f: Vec<&str> = l.split('\t').collect();
            let mut snap = snapshot(f[2], &specs);
            snap.flags = SimFlags { sreload: true, ..variant(f[1]) };
            let got = m.fates(&snap, f[3].parse().unwrap(), FateMode::AMove);
            let want: Vec<Vec<&str>> = f[4].split(';').map(|u| u.split(':').collect()).collect();
            assert_eq!(got.len(), want.len());
            for (i, (g, w)) in got.iter().zip(&want).enumerate() {
                let death: i32 = w[1].parse().unwrap();
                let (hp, pd): (f64, f64) = (w[2].parse().unwrap(), w[3].parse().unwrap());
                assert_eq!(g.dead, w[0] == "1", "{} {} unit {i}: {g:?} vs {w:?}", f[0], f[1]);
                assert_eq!(g.death_frame, (death >= 0).then_some(death), "{} {} unit {i}", f[0], f[1]);
                assert!((g.hp_left as f64 - hp).abs() <= 0.5 + 1e-3 * hp, "{} {} unit {i}: {g:?} vs {w:?}", f[0], f[1]);
                assert!((g.p_raw as f64 - pd).abs() <= TOL, "{} {} unit {i}: {g:?} vs {w:?}", f[0], f[1]);
            }
            n += 1;
        }
        assert!(n >= 10);
    }

    /// `bombs` (kamikazeDistance per def, or touching) against
    /// mech.simulate_mech with each variant's opts + bombs
    /// (testdata/mech_bombs_cases.tsv, export_bombs.py).
    #[test]
    fn bombs_parity_with_python() {
        let m = Model::load().unwrap();
        let specs = specs_from(BOMBS_UNITS);
        let s = m.mech.stun_cap_s;
        let (mut n, mut worst_all) = (0, 0.0f64);
        for l in data_lines(BOMBS_CASES) {
            let f: Vec<&str> = l.split('\t').collect();
            let flags = SimFlags { bombs: true, ..variant(f[1]) };
            let mut snap = snapshot(f[2], &specs);
            snap.flags = flags;
            let units = m.mech_units(&snap);
            let want: Vec<f64> = f[3..11].iter().map(|v| v.parse().unwrap()).collect();
            let dps = simulate_mech_flags(&units, &m.hits, s, None, false, flags);
            let stop = simulate_mech_flags(&units, &m.hits, s, Some(&m.hazard), false, flags);
            let wd = simulate_mech_flags(&units, &m.hits, s, Some(&m.hazard), true, flags);
            let mut off = snap.clone();
            off.flags.bombs = false;
            let o = simulate_mech_flags(&m.mech_units(&off), &m.hits, s, None, false, off.flags);
            let got = [dps.0, dps.1, stop.0, stop.1, wd.0, wd.1, o.0, o.1];
            let worst = got.iter().zip(&want).map(|(g, w)| (g - w).abs()).fold(0.0, f64::max);
            assert!(worst <= TOL, "{} {}: got {got:?}, python {want:?}", f[0], f[1]);
            worst_all = worst_all.max(worst);
            n += 1;
        }
        eprintln!("bombs parity: {n} cases, worst |rust - python| = {worst_all:.2e}");
        assert!(n >= 18);
    }

    /// The `bombs` fates against mech.simulate_mech(fates=...), and the
    /// flag moves when the bombs go off (in the plane, at all).
    #[test]
    fn bombs_fates_parity_with_python() {
        let m = Model::load().unwrap();
        let specs = specs_from(BOMBS_UNITS);
        let (mut n, mut moved) = (0, 0);
        for l in data_lines(BOMBS_FATES) {
            let f: Vec<&str> = l.split('\t').collect();
            let mut snap = snapshot(f[2], &specs);
            snap.flags = SimFlags { bombs: true, ..variant(f[1]) };
            let horizon = f[3].parse().unwrap();
            let got = m.fates(&snap, horizon, FateMode::AMove);
            let want: Vec<Vec<&str>> = f[4].split(';').map(|u| u.split(':').collect()).collect();
            assert_eq!(got.len(), want.len());
            for (i, (g, w)) in got.iter().zip(&want).enumerate() {
                let death: i32 = w[1].parse().unwrap();
                let (hp, pd): (f64, f64) = (w[2].parse().unwrap(), w[3].parse().unwrap());
                assert_eq!(g.dead, w[0] == "1", "{} {} unit {i}: {g:?} vs {w:?}", f[0], f[1]);
                assert_eq!(g.death_frame, (death >= 0).then_some(death), "{} {} unit {i}", f[0], f[1]);
                assert!((g.hp_left as f64 - hp).abs() <= 0.5 + 1e-3 * hp, "{} {} unit {i}: {g:?} vs {w:?}", f[0], f[1]);
                assert!((g.p_raw as f64 - pd).abs() <= TOL, "{} {} unit {i}: {g:?} vs {w:?}", f[0], f[1]);
            }
            snap.flags.bombs = false;
            let off = m.fates(&snap, horizon, FateMode::AMove);
            moved += usize::from(off.iter().zip(&got).any(|(a, b)| a.death_frame != b.death_frame));
            n += 1;
        }
        eprintln!("bombs fates parity: {n} cases, {moved} moved by the flag");
        assert!(n >= 18 && moved >= 6);
    }

    /// Off, both switches leave every answer as it was: the base parity
    /// cases' fight and fates are bit-identical with default flags.
    #[test]
    fn switches_off_are_the_frozen_fight() {
        let m = Model::load().unwrap();
        let t = MechTables::load().unwrap();
        assert!(
            t.focus_cal.is_some() && t.twod_cal.is_some() && t.sr_cal.is_some(),
            "variant calibrations in the tables"
        );
        assert!(t.splash.contains_key("tankraid_napalm_bomblet"));
        assert_eq!(t.calibration(SimFlags::default()).win, t.win);
        for (name, snap, _) in cases() {
            assert_eq!(snap.flags, SimFlags::default());
            let a = m.predict_with(&snap, true);
            let b = m.predict_with(&FightSnapshot { flags: SimFlags::default(), units: snap.units.clone() }, true);
            assert_eq!(a, b, "{name}");
        }
    }

    #[test]
    fn limpet_blast_slows_enemies_and_spares_escorts() {
        let m = Model::load().unwrap();
        let s = specs_from(BOMBS_UNITS);
        let mut snap = snapshot("0:amphbomb:400:0:0;0:amphraid:360:0:0;1:amphraid:360:0:0", &s);
        for flags in [SimFlags::default(), variant("sr"), variant("focus"), variant("2d")] {
            for bombs in [false, true] {
                snap.flags = SimFlags { bombs, ..flags };
                let mut us = m.mech_units(&snap);
                kill(&mut us, &mut [0.0; 2], 0, 123);
                assert_eq!(us[1].u.hp, 360.0);
                assert_eq!(us[1].slowdmg, 0.0);
                assert!((us[2].u.hp - 239.9).abs() < 1e-6);
                assert!((us[2].slowdmg - 0.58 * 239.9).abs() < 1e-6);
                assert_eq!(us[2].slowed(), 0.5);
                assert_eq!(us[2].slow_last, 123);
                assert_eq!(us[2].ovs, 0.08);
            }
        }
    }

    #[test]
    fn limpet_fates_match_python() {
        let m = Model::load().unwrap();
        let s = specs_from(BOMBS_UNITS);
        for line in data_lines(include_str!("testdata/mech_limpet_fates.tsv")) {
            let f: Vec<_> = line.split('\t').collect();
            let mut snap = snapshot(f[3], &s);
            snap.flags = SimFlags { bombs: f[2] == "1", ..variant(f[1]) };
            let got = m.fates(&snap, f[4].parse().unwrap(), FateMode::AMove);
            for (i, (g, want)) in got.iter().zip(f[5].split(';')).enumerate() {
                let w: Vec<_> = want.split(':').collect();
                assert_eq!(g.dead, w[0] == "1", "{} {} unit {i}", f[0], f[1]);
                assert_eq!(g.death_frame.unwrap_or(-1), w[1].parse::<i32>().unwrap(), "{} {} unit {i}", f[0], f[1]);
                assert!(
                    (g.hp_left as f64 - w[2].parse::<f64>().unwrap()).abs() < 0.01,
                    "{} {} unit {i}: {g:?} vs {want}",
                    f[0],
                    f[1]
                );
                assert!(
                    (g.p_raw as f64 - w[3].parse::<f64>().unwrap()).abs() < 1e-3,
                    "{} {} unit {i}: {g:?} vs {want}",
                    f[0],
                    f[1]
                );
            }
        }
    }

    #[test]
    fn limpet_cost_is_not_justified_by_targets_the_escorts_kill_anyway() {
        let m = Model::load().unwrap();
        let s = specs_from(BOMBS_UNITS);
        let mut snap =
            snapshot("0:amphbomb:400:80:0;0:amphraid:360:0:0;0:amphraid:360:-20:40;1:amphcon:1000:320.3:0", &s);
        snap.flags = SimFlags::from_switches(true, true, true);
        let v = m.bomb_charge(&snap, 0, &[], 900);
        assert!(v.spent > 0.99 && v.net < -150.0, "{v:?}");
    }

    /// A ring of Glaives round a Kodachi: the plane puts them round it, the
    /// axis on two points (a centred ring collapses onto one).
    #[test]
    fn a_ring_keeps_its_shape_in_the_plane() {
        let specs = specs_from(VARIANT_UNITS);
        let (g, k) = (specs["cloakraid"].clone(), specs["tankraid"].clone());
        let mut units: Vec<SnapUnit> = (0..4)
            .map(|i| {
                let a = std::f32::consts::FRAC_PI_2 * i as f32;
                SnapUnit { side: 0, spec: g.clone(), hp: 230.0, x: 300.0 * a.cos(), z: 300.0 * a.sin() }
            })
            .collect();
        units.push(SnapUnit { side: 1, spec: k, hp: 660.0, x: 0.0, z: 0.0 });
        let snap = FightSnapshot { flags: SimFlags::from_switches(true, false, false), units };
        let p = plane(&snap);
        for (x, z) in &p[..4] {
            assert!(((x * x + z * z).sqrt() - 300.0).abs() < 1e-3, "{p:?}");
        }
        assert!(project(&snap).iter().all(|x| x.abs() < 1e-3));
    }

    #[test]
    fn erf_matches_known_values() {
        for (x, want) in [(0.0, 0.0), (0.5, 0.5204998778), (1.0, 0.8427007929), (-2.0, -0.9953222650)] {
            assert!((erf(x) - want).abs() < 2e-7, "erf({x}) = {}", erf(x));
        }
    }

    /// Without the switch the base answer is the same as `predict`'s.
    #[test]
    fn mechanics_leave_the_base_answer_alone() {
        let m = Model::load().unwrap();
        for (_, snap, _) in cases() {
            let base = m.predict(&snap);
            let with = m.predict_with(&snap, true);
            assert!(base.mech.is_none() && with.mech.is_some());
            assert_eq!(FightOutcome { mech: None, ..with }, base);
        }
    }

    #[test]
    fn engagement_reads_blockers_and_crowding() {
        let specs = specs();
        let unit = |side, def: &str, x, z| SnapUnit { side, spec: specs[def].clone(), hp: 100.0, x, z };
        // A Rogue behind a file of Glaives, facing one Bandit 400 out.
        let mut snap = FightSnapshot {
            flags: Default::default(),
            units: vec![unit(0, "cloakskirm", 0.0, 0.0), unit(1, "shieldraid", 400.0, 0.0)],
        };
        let alone = engage_features(&snap, 0, 455.0);
        for k in 1..4 {
            snap.units.push(unit(0, "cloakraid", 30.0 * k as f32, 2.0));
        }
        let behind = engage_features(&snap, 0, 455.0);
        assert_eq!((alone[10], alone[11]), (0.0, 0.0));
        assert_eq!(behind[10], 3.0);
        assert!((behind[11] - 4f64.ln()).abs() < 1e-12);
        assert_eq!(behind[2], 0.0);
        assert_eq!(behind[1], 1.0);
        assert!((behind[12] - 2f64.ln()).abs() < 1e-12);
    }

    /// Microseconds per mechanics query (a-move + withdraw) on the largest
    /// case, beside the base model's.
    #[test]
    fn cost_per_query() {
        let m = Model::load().unwrap();
        let cases = cases();
        let (_, big, _) = cases.iter().find(|c| c.0 == "big_mixed").unwrap();
        let n = 200;
        let start = std::time::Instant::now();
        for _ in 0..n {
            let _ = m.predict(big);
        }
        let base = start.elapsed().as_secs_f64() * 1e6 / n as f64;
        let start = std::time::Instant::now();
        for _ in 0..n {
            let _ = m.predict_with(big, true);
        }
        let with = start.elapsed().as_secs_f64() * 1e6 / n as f64;
        let start = std::time::Instant::now();
        for _ in 0..n {
            let _ = m.fates(big, 150, FateMode::Pass { offset: 200.0 });
        }
        let fates = start.elapsed().as_secs_f64() * 1e6 / n as f64;
        eprintln!(
            "cost: {} units, {base:.0} us base, {with:.0} us with mechanics (a-move + withdraw each), {fates:.0} us fates (5 s pass)",
            big.units.len()
        );
        assert!(with < 50_000.0 && fates < 50_000.0);
    }

    /// What the switches cost per fates query (10 s a-move) and per
    /// `predict_with` (a-move and withdrawal under the hazard), by size:
    /// `cargo test --release variant_cost -- --ignored --nocapture`.
    #[test]
    #[ignore = "timing; prints"]
    fn variant_cost() {
        let m = Model::load().unwrap();
        let specs = specs_from(VARIANT_UNITS);
        for per_side in [5usize, 10, 30] {
            let mut units = Vec::new();
            for i in 0..per_side {
                let (row, col) = ((i / 5) as f32, (i % 5) as f32);
                units.push(SnapUnit {
                    side: 0,
                    spec: specs["cloakraid"].clone(),
                    hp: 230.0,
                    x: -40.0 * row,
                    z: 30.0 * col,
                });
                let def = if i % 3 == 0 { "tankraid" } else { "shieldraid" };
                let hp = specs[def].health;
                units.push(SnapUnit { side: 1, spec: specs[def].clone(), hp, x: 350.0 + 40.0 * row, z: 30.0 * col });
            }
            let mut line = format!("{per_side:2} a side:");
            for (name, flags) in [
                ("base", SimFlags::default()),
                ("focus", SimFlags::from_switches(false, true, false)),
                ("2d", SimFlags::from_switches(true, false, false)),
            ] {
                let snap = FightSnapshot { flags, units: units.clone() };
                let n = if per_side >= 30 { 20 } else { 100 };
                let t0 = std::time::Instant::now();
                for _ in 0..n {
                    let _ = m.fates(&snap, 300, FateMode::AMove);
                }
                let fates = t0.elapsed().as_secs_f64() * 1e6 / n as f64;
                let t0 = std::time::Instant::now();
                for _ in 0..n {
                    let _ = m.predict_with(&snap, true);
                }
                let with = t0.elapsed().as_secs_f64() * 1e6 / n as f64;
                line += &format!("  {name} fates {fates:.0} us, predict_with {with:.0} us;");
            }
            eprintln!("{line}");
        }
    }
}
