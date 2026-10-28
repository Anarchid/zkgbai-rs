//! Mechanics on the fight predictor: a port of the frozen mechanics model
//! (tools/predictor/mech.py with the round-1 six and the round-3 heatray,
//! zkgbai-runs/predictor/mech1..mech3) onto the same one-dimensional fight:
//!
//! - air: a weapon skips targets it cannot shoot (aircraft, or ground);
//! - burn: fire weapons leave their fitted damage over time on what they hit;
//! - death: units explode as they die, and kamikazes (Imp, Snitch, Skuttle,
//!   ...) walk into the enemy and blow up within 20 elmos;
//! - emp: paralysis builds up to a cap and wears off over 40 s; a unit
//!   paralysed past its health neither moves nor fires; pure EMP weapons
//!   fire too (no health damage);
//! - slow: slow damage cuts speed and rate of fire by up to half, wearing
//!   off half a second after the last slowing hit;
//! - engage: each unit fires at all with a fitted probability (its nearest
//!   enemy over its range, short range, static, commander, builder, own
//!   units in its line of fire, crowding, enemies), a weight on every shot;
//! - heatray: weapons whose damage falls off with distance land their
//!   fitted share of it by range band.
//!
//! The parameters and a calibration refitted on these outputs are compiled
//! in from `data/fightsim_mech.tsv`, written by
//! `tools/predictor/export_mech.py` (its first line says from what).

use std::collections::HashMap;

use super::{
    DT, FateMode, FightSnapshot, HORIZON, Hazard, HitTables, MechOutcome, Model, RBANDS, SimUnit, UnitFate, ZoiBeta,
    data_lines, expit, logit, parse_f64, project, speed_band, take,
};

const MECH_TSV: &str = include_str!("../../../data/fightsim_mech.tsv");

/// Unit defs that attack by dying next to the enemy.
const KAMIKAZE: [&str; 6] = ["cloakbomb", "shieldbomb", "jumpbomb", "amphbomb", "gunshipbomb", "subscout"];
/// A kamikaze within this of its nearest enemy blows up.
const KAMIKAZE_REACH: f64 = 20.0;
/// Seconds for full paralysis to wear off (unit_paralysis_damage.lua).
const STUN_DECAY_S: f64 = 40.0;
/// Most a unit is slowed (timeslow_defs.lua).
const SLOW_MAX: f64 = 0.5;
/// Slow damage wearing off per second, as a share of health.
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

/// The fitted mechanics and their calibration.
pub struct MechTables {
    weapons: HashMap<String, WeaponMech>,
    death: HashMap<String, Death>,
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
    /// The data file's fitting header.
    pub fitted: String,
}

impl MechTables {
    pub fn load() -> Result<MechTables, String> {
        MechTables::parse(MECH_TSV)
    }

    fn parse(tsv: &str) -> Result<MechTables, String> {
        let mut weapons = HashMap::new();
        let mut death = HashMap::new();
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
        Ok(MechTables {
            weapons,
            death,
            engage: w,
            stun_cap_s: stun_cap_s.ok_or("mech: no stun_cap_s")?,
            win: take(&cal, "win.")?,
            cal_a: ZoiBeta { theta: take(&cal, "zoibA.")? },
            cal_b: ZoiBeta { theta: take(&cal, "zoibB.")? },
            fate: take(&cal, "fate.")?,
            fitted: tsv.lines().next().unwrap_or_default().trim_start_matches('#').trim().to_owned(),
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
    death: Option<Death>,
    /// P(the unit fires at all in this fight).
    engage: f64,
    para: f64,
    slowdmg: f64,
    slow_last: i32,
    /// (last frame, damage per frame).
    burns: Vec<(i32, f64)>,
    /// Lateral coordinate: 0 except in a fates query's pass mode.
    lat: f64,
}

impl MechUnit {
    fn stunned(&self) -> bool {
        self.para > self.maxhp
    }

    fn slowed(&self) -> f64 {
        if self.slowdmg > 0.0 { SLOW_MAX.min(self.slowdmg / self.u.hp.max(1.0)) } else { 0.0 }
    }
}

/// mech.py `apart`: distance along the axis, and across it when the two
/// stand on different lines (pass mode).
fn apart(a: &MechUnit, b: &MechUnit) -> f64 {
    let dy = a.lat - b.lat;
    if dy == 0.0 { (a.u.x - b.u.x).abs() } else { (a.u.x - b.u.x).hypot(dy) }
}

/// What a run of the mechanics fight answers.
struct Run<'a> {
    hazard: Option<&'a Hazard>,
    withdraw: bool,
    /// A's mobile units drive through B (fates' pass mode).
    pass: bool,
    horizon: i32,
    /// Per-unit bookkeeping for a fates query; the fight then goes on to
    /// the horizon while one side has an armed unit and the other anything.
    track: Option<Track>,
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
        let (t, s) = (&self.mech, self.mech.stun_cap_s);
        let (ka, kb) = simulate_mech(&units, &self.hits, s, Some(&self.hazard), false);
        let (wa, wb) = simulate_mech(&units, &self.hits, s, Some(&self.hazard), true);
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
        let mut units = self.mech_units(snap);
        let pass = match mode {
            FateMode::AMove => false,
            FateMode::Pass { offset } => {
                for m in &mut units {
                    m.lat = if m.u.side == 1 && !m.u.mobile { 0.0 } else { offset as f64 };
                }
                true
            }
        };
        let n = units.len();
        let track = Track { var: vec![0.0; n], death_f: vec![None; n] };
        let mut run = Run { hazard: None, withdraw: false, pass, horizon, track: Some(track) };
        let (_, end) = run_mech(&units, &self.hits, self.mech.stun_cap_s, &mut run);
        let Some(tr) = run.track else { return Vec::new() };
        let c = &self.mech.fate;
        let ln_h = (horizon.max(1) as f64 / 150.0).ln();
        units
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
            .collect()
    }

    /// The snapshot's units for the mechanics fight. Pure EMP weapons go
    /// last with no health damage (mech.py adds them back after the base
    /// weapons); weapons doing neither are dropped.
    pub(super) fn mech_units(&self, snap: &FightSnapshot) -> Vec<MechUnit> {
        let t = &self.mech;
        let xs = project(snap);
        let mut us: Vec<MechUnit> = snap
            .units
            .iter()
            .zip(xs)
            .map(|(su, x)| {
                let ws: Vec<(&super::WeaponSpec, WeaponMech)> =
                    su.spec.weapons.iter().map(|w| (w, t.weapon(&w.name))).collect();
                let ordered = ws
                    .iter()
                    .filter(|(w, m)| !m.paralyzer && w.damage > 0.0)
                    .chain(ws.iter().filter(|(_, m)| m.paralyzer));
                let (mut weapons, mut mech) = (Vec::new(), Vec::new());
                for (w, m) in ordered {
                    let mut sw = self.sim_weapon(w);
                    if m.paralyzer {
                        sw.nominal = 0.0;
                    }
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
                    death: t.death.get(&su.spec.def).copied(),
                    engage: 1.0,
                    para: 0.0,
                    slowdmg: 0.0,
                    slow_last: -1_000_000_000,
                    burns: Vec::new(),
                    lat: 0.0,
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
fn hurt(us: &mut [MechUnit], killed: &mut [f64; 2], t: usize, dmg: f64) {
    if !us[t].u.alive {
        return;
    }
    us[t].u.hp -= dmg;
    if us[t].u.hp <= 0.0 {
        kill(us, killed, t);
    }
}

/// mech.py `kill`: no check that `t` lives (a kamikaze caught in a blast
/// still blows up when it reaches the enemy, as in the Python model).
fn kill(us: &mut [MechUnit], killed: &mut [f64; 2], t: usize) {
    us[t].u.alive = false;
    if us[t].u.armed {
        killed[us[t].u.side as usize] += us[t].u.cost;
    }
    let Some(d) = us[t].death else { return };
    for o in 0..us.len() {
        if !us[o].u.alive {
            continue;
        }
        let dd = apart(&us[o], &us[t]);
        if dd < d.aoe {
            let v = d.damage * (1.0 - d.edge * dd / d.aoe);
            if d.paralyzer {
                us[o].para += v;
            } else {
                hurt(us, killed, o, v);
            }
        }
    }
}

/// mech.py `simulate_mech` with air, burn, death, emp, slow, engage and
/// heatray on: expected killed share of each side's armed cost, weighted
/// over the stopping time when a hazard is given. `withdraw`: every mobile
/// A unit turns away from B at its (slowed) speed instead of fighting.
pub(super) fn simulate_mech(
    template: &[MechUnit],
    hits: &HitTables,
    stun_cap_s: f64,
    hazard: Option<&Hazard>,
    withdraw: bool,
) -> (f64, f64) {
    let mut run = Run { hazard, withdraw, pass: false, horizon: HORIZON, track: None };
    run_mech(template, hits, stun_cap_s, &mut run).0
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
        alive.extend((0..n).filter(|&i| us[i].u.alive));
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
            break;
        }
        // Status: burns tick, paralysis and slow wear off (units killed by a
        // burn earlier in order still wear off, as in the Python model).
        for &i in &alive {
            if !us[i].burns.is_empty() {
                let b: f64 = us[i].burns.iter().filter(|(end, _)| *end > f).map(|(_, r)| r).sum();
                us[i].burns.retain(|(end, _)| *end > f + DT);
                if b > 0.0 {
                    hurt(&mut us, &mut killed, i, b * dt);
                }
            }
            let m = &mut us[i];
            if m.para > 0.0 {
                m.para = (m.para - m.maxhp / STUN_DECAY_S * dt / 30.0).max(0.0);
            }
            if m.slowdmg > 0.0 && f - m.slow_last > SLOW_GRACE {
                m.slowdmg = (m.slowdmg - SLOW_DECAY * m.u.hp.max(1.0) * dt / 30.0).max(0.0);
            }
        }
        alive.retain(|&i| us[i].u.alive);
        // Movement towards the nearest enemy alive at the top of the step
        // (mech.py reads the step's opening sides here).
        let mut moved = false;
        for &i in &alive {
            us[i].u.moving = false;
            if !us[i].u.mobile || us[i].u.speed <= 0.0 || us[i].stunned() {
                continue;
            }
            let side = us[i].u.side as usize;
            let mut nearest: Option<(f64, f64)> = None;
            for &j in &sides[1 - side] {
                let d = apart(&us[j], &us[i]);
                if nearest.is_none_or(|n| d < n.0) {
                    nearest = Some((d, us[j].u.x));
                }
            }
            let Some((dist, nx)) = nearest else { continue };
            let speed = us[i].u.speed * (1.0 - us[i].slowed());
            if run.pass && side == 0 {
                if us[i].kamikaze && dist <= KAMIKAZE_REACH {
                    kill(&mut us, &mut killed, i);
                    continue;
                }
                us[i].u.x += speed * dt;
                us[i].u.moving = true;
                moved = true;
                continue;
            }
            if withdraw && side == 0 {
                let u = &mut us[i].u;
                u.x -= if nx > u.x { speed * dt } else { -speed * dt };
                u.moving = true;
                moved = true;
                continue;
            }
            if us[i].kamikaze {
                if dist <= KAMIKAZE_REACH {
                    kill(&mut us, &mut killed, i);
                    continue;
                }
                let u = &mut us[i].u;
                let step = (speed * dt).min(dist);
                u.x += if nx > u.x { step } else { -step };
                u.moving = true;
                moved = true;
                continue;
            }
            let u = &mut us[i].u;
            if !u.armed {
                continue;
            }
            if dist > u.max_range * 0.95 {
                let step = (speed * dt).min(dist - u.max_range * 0.9);
                u.x += if nx > u.x { step } else { -step };
                u.moving = true;
                moved = true;
            }
        }
        alive.retain(|&i| us[i].u.alive);
        // Fire: every reloaded weapon at its nearest living target in range
        // that it can shoot, its expected damage times the shooter's chance
        // of engaging at all, with burn, paralysis and slow riding along.
        let mut shot_fired = false;
        for &i in &alive {
            if !us[i].u.armed || !us[i].u.alive || us[i].stunned() {
                continue;
            }
            let side = us[i].u.side as usize;
            foes.clear();
            foes.extend(sides[1 - side].iter().copied().filter(|&j| us[j].u.alive));
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
                    if e.u.alive
                        && d <= range
                        && (if e.air { m.can_air } else { m.can_ground })
                        && target.is_none_or(|t| d < t.0)
                    {
                        target = Some((d, j));
                    }
                }
                let Some((dist, j)) = target else { continue };
                us[i].u.weapons[wi].next = f + if sf > 0.0 { (reload as f64 / (1.0 - sf)) as i32 } else { reload };
                shot_fired = true;
                let rb = if range > 0.0 { ((RBANDS as f64 * dist / range) as i32).min(RBANDS) } else { RBANDS };
                let sb = speed_band(if us[j].u.moving { us[j].u.speed } else { 0.0 });
                let hit = hits.lookup(key, us[j].u.target, rb as u8, sb);
                let frac = m.dyn_frac.and_then(|d| d[rb as usize]).unwrap_or(hit.frac);
                let shots = if hitscan { 1.0 } else { salvo as f64 };
                let dmg = hit.p_tgt * frac * nominal * shots * eff;
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
                    t.slow_last = f;
                }
                if dmg > 0.0 {
                    if let Some(tr) = run.track.as_mut() {
                        let (p, d1) = (hit.p_tgt * eff, frac * nominal);
                        tr.var[j] += shots * p * (1.0 - p) * d1 * d1;
                    }
                    hurt(&mut us, &mut killed, j, dmg);
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
    /// Absolute tolerance on killed shares against the Python model.
    const TOL: f64 = 1e-3;

    fn specs() -> HashMap<String, Arc<UnitSpec>> {
        data_lines(UNITS)
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
        FightSnapshot { units }
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
    fn tables_load_with_fitting_header() {
        let t = MechTables::load().unwrap();
        assert!(t.fitted.contains("fitted 20"), "{}", t.fitted);
        assert!(t.weapons.len() > 100 && t.death.len() > 50);
        // Kodachi burns, Bolas slows, Faraday is pure EMP, Scorcher's heat
        // falls off with range, Angler cannot shoot the ground.
        assert!(t.weapon("tankraid_napalm_bomblet").burn > 0.0);
        assert!(t.weapon("hoverheavyraid_disruptor").slow > 0.0);
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
        let mut snap =
            FightSnapshot { units: vec![unit(0, "cloakskirm", 0.0, 0.0), unit(1, "shieldraid", 400.0, 0.0)] };
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
}
