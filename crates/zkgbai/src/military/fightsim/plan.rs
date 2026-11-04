//! Force planning: the cheapest force of the kinds we can field that takes
//! on what stands at a place, by the fight predictor.
//!
//! Units are picked one at a time, each the kind that adds most to the
//! predicted margin (their expected killed share less ours) per metal of
//! it, until the predictor gives the fight to us comfortably: P(win) of at
//! least [`Caps::win`] at an expected loss of at most [`Caps::loss`] of our
//! armed metal. Short of that within [`Caps::metal`] and [`Caps::units`],
//! the plan is the best found, marked unmet. What stands there keeps its
//! places (turrets hold, mobile defenders close in); our force starts just
//! out of everyone's reach on one side of it, so a kind that outranges the
//! defense can fight from beyond its reach as the model lets it.
//!
//! The conquest escort sizes itself so against the raid it expects (see
//! `qrf::escort`); the `force_plan` tool asks it offline ("three Lotuses on
//! a mex triplet: what of a vehicle factory's takes them?").

use std::collections::BTreeMap;
use std::sync::Arc;

use super::{FateMode, FightSnapshot, Model, SimFlags, SnapUnit, UnitSpec, WeaponSpec};

/// When a force is enough, and how much it may be.
#[derive(Clone, Copy, Debug)]
pub struct Caps {
    pub win: f32,
    pub loss: f32,
    pub metal: f32,
    pub units: usize,
}

impl Default for Caps {
    fn default() -> Self {
        Caps { win: 0.8, loss: 0.5, metal: 600.0, units: 8 }
    }
}

/// A force: indices into the options (repeats allowed), with what the
/// predictor expects of it.
#[derive(Clone, Debug, Default)]
pub struct Plan {
    pub pick: Vec<usize>,
    pub p_win: f32,
    /// Expected share of our armed metal lost, and of theirs killed.
    pub loss: f32,
    pub killed: f32,
    pub cost: f32,
    /// Whether it meets the caps' win and loss.
    pub met: bool,
    /// Seizures: seconds until the last armed defender died (0: not all).
    pub secs: f32,
}

/// What waiting costs (user, 2026-11-03: "if it takes 20 s to clear with
/// standing forces and 50 s + 50 s to produce + 20 s to walk with Slings,
/// then this adds 100 s of input from this area to the enemy, which is 600
/// metal on a typical Ravaged triplet, and the situation can move"): a
/// force is priced by the metal it is expected to lose plus what the place
/// pays its holder until it is cleared, the force built (at the lesser of
/// a factory's build power and our income), walked there at its slowest
/// unit's speed, and fought.
#[derive(Clone, Copy, Debug)]
pub struct Logistics {
    /// Metal per second going into the force.
    pub rate: f32,
    /// Ground elmos from where the force is built to the place.
    pub walk: f32,
    /// Metal per second the place pays whoever holds it.
    pub stake: f32,
}

/// A ZK factory's build power: metal per second (Shared.FACTORY_BUILDPOWER).
pub const FACTORY_BUILD_POWER: f32 = 10.0;

impl Logistics {
    /// One factory building at the lesser of its build power and `income`.
    pub fn factory(income: f32, walk: f32, stake: f32) -> Logistics {
        Logistics { rate: income.clamp(0.5, FACTORY_BUILD_POWER), walk, stake }
    }
}

/// A force's time to clear and its price (see [`Logistics`]).
#[derive(Clone, Copy, Debug, Default)]
pub struct Timing {
    pub produce: f32,
    pub walk: f32,
    pub fight: f32,
    /// Seconds from the decision to the place cleared.
    pub total: f32,
    /// Expected metal lost, the place's pay over `total`, and their sum.
    pub lost: f32,
    pub waited: f32,
    pub price: f32,
}

/// The time and price of a force of `cost` metal whose slowest unit moves
/// `speed` elmos a frame, `fight` seconds to clear (0: it does not clear,
/// counted as `horizon`), losing `loss` of its metal: `built` forces are
/// produced first and walk `lg.walk`; standing ones are where they stand.
pub fn timing(cost: f32, speed: f32, fight: f32, horizon: f32, loss: f32, built: bool, lg: &Logistics) -> Timing {
    let produce = if built { cost / lg.rate.max(0.1) } else { 0.0 };
    let walk = if built { lg.walk / (speed.max(0.1) * 30.0) } else { 0.0 };
    let fight = if fight > 0.0 { fight } else { horizon };
    let total = produce + walk + fight;
    let lost = loss * cost;
    let waited = lg.stake * total;
    Timing { produce, walk, fight, total, lost, waited, price: lost + waited }
}

/// [`timing`] of a plan built from `options`.
pub fn plan_timing(plan: &Plan, options: &[Arc<UnitSpec>], horizon: f32, lg: &Logistics) -> Timing {
    let speed = plan.pick.iter().map(|&k| options[k].speed).fold(f32::INFINITY, f32::min);
    let speed = if speed.is_finite() { speed } else { 1.0 };
    timing(plan.cost, speed, plan.secs, horizon, plan.loss, true, lg)
}

/// Elmos our force starts beyond the farthest reach in the fight.
const CLEAR: f32 = 150.0;
/// Spacing of a force's units on its line, elmos.
const SPACING: f32 = 24.0;

/// The fight of `pick` against `against` (specs at their places).
pub fn staged_fight(
    against: &[(Arc<UnitSpec>, f32, f32)],
    options: &[Arc<UnitSpec>],
    pick: &[usize],
    flags: SimFlags,
) -> FightSnapshot {
    // Beyond the reach of what fights here (the picked units and the
    // defenders), not of every kind that might have been picked.
    let reach = (against.iter().map(|a| &a.0).chain(pick.iter().map(|&k| &options[k])))
        .flat_map(|s| s.weapons.iter().map(|w| w.range))
        .fold(0.0f32, f32::max);
    let left = against.iter().map(|a| a.1).fold(f32::INFINITY, f32::min);
    let mid = against.iter().map(|a| a.2).sum::<f32>() / against.len().max(1) as f32;
    let x0 = if left.is_finite() { left - reach - CLEAR } else { 0.0 };
    let mut units = Vec::new();
    for (i, &k) in pick.iter().enumerate() {
        let s = options[k].clone();
        let z = mid + SPACING * (i as f32 - (pick.len() as f32 - 1.0) / 2.0);
        units.push(SnapUnit { side: 0, hp: s.health, x: x0, z, spec: s, tubes: Vec::new() });
    }
    for (s, x, z) in against {
        units.push(SnapUnit { side: 1, hp: s.health, x: *x, z: *z, spec: s.clone(), tubes: Vec::new() });
    }
    FightSnapshot { units, flags }
}

/// What one candidate force is predicted to do: the margin the search
/// climbs, the criterion held against [`Caps::win`], and the expected
/// loss and kill shares.
struct Judged {
    margin: f32,
    crit: f32,
    loss: f32,
    killed: f32,
    secs: f32,
}

/// The search shared by the goals: a greedy mix (each step the kind that
/// adds most margin per metal) and every single-kind force, grown until it
/// meets the caps; the cheapest that meets them, else the one closest to
/// the criterion.
fn search(
    options: &[Arc<UnitSpec>],
    caps: &Caps,
    judge: &dyn Fn(&[usize]) -> Judged,
    price: Option<&dyn Fn(&Plan) -> f32>,
) -> Plan {
    let usable: Vec<usize> =
        (0..options.len()).filter(|&k| options[k].cost > 0.0 && !options[k].weapons.is_empty()).collect();
    let grow = |kinds: &[usize]| -> Plan {
        let mut plan = Plan { loss: 1.0, ..Plan::default() };
        let mut margin = f32::NEG_INFINITY;
        while plan.pick.len() < caps.units {
            let mut best: Option<(usize, f32, Judged)> = None;
            for &k in kinds {
                if plan.cost + options[k].cost > caps.metal {
                    continue;
                }
                let mut trial = plan.pick.clone();
                trial.push(k);
                let j = judge(&trial);
                let gain = if margin.is_finite() { j.margin - margin } else { j.margin };
                let score = gain / options[k].cost;
                if best.as_ref().is_none_or(|b| score > b.1) {
                    best = Some((k, score, j));
                }
            }
            let Some((k, _, j)) = best else { break };
            plan.pick.push(k);
            plan.cost += options[k].cost;
            (plan.p_win, plan.loss, plan.killed, plan.secs, margin) = (j.crit, j.loss, j.killed, j.secs, j.margin);
            if j.crit >= caps.win - 1e-4 && j.loss <= caps.loss {
                plan.met = true;
                break;
            }
        }
        plan
    };
    let mut plans = vec![grow(&usable)];
    plans.extend(usable.iter().map(|&k| grow(&[k])));
    let key = |p: &Plan| price.map_or(p.cost, |f| f(p));
    let met = plans.iter().filter(|p| p.met).min_by(|a, b| key(a).total_cmp(&key(b))).cloned();
    met.unwrap_or_else(|| plans.into_iter().max_by(|a, b| a.p_win.total_cmp(&b.p_win)).unwrap_or_default())
}

fn nothing_armed(against: &[(Arc<UnitSpec>, f32, f32)]) -> bool {
    against.iter().all(|a| a.0.weapons.is_empty())
}

/// The cheapest force from `options` that takes on `against` in a fight
/// (see the module notes): P(win) and the expected loss of the fight
/// model, which stops when a side would break off (a raid met).
pub fn cheapest_force(
    model: &Model,
    against: &[(Arc<UnitSpec>, f32, f32)],
    options: &[Arc<UnitSpec>],
    flags: SimFlags,
    caps: &Caps,
) -> Plan {
    if nothing_armed(against) {
        // Nothing there fights back: the force is nobody (a builder takes it).
        return Plan { p_win: 1.0, loss: 0.0, met: true, ..Plan::default() };
    }
    search(
        options,
        caps,
        &|pick: &[usize]| {
            let out = model.predict(&staged_fight(against, options, pick, flags));
            Judged {
                margin: out.mean_b - out.mean_a,
                crit: out.p_a_wins,
                loss: out.mean_a,
                killed: out.mean_b,
                secs: 0.0,
            }
        },
        None,
    )
}

/// How our force moves in a seizure's fight.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Moves {
    /// Our force a-moves in: closes until in range, fires (the default:
    /// what an attack order does).
    AMove,
    /// ZK's own unit AI moves both sides (skirmishers keep their range).
    /// Against turrets its skirmishers never close, so nothing happens.
    Kiting,
}

/// The cheapest force from `options` that clears `against` (turrets hold
/// and do not break off), by the mechanics fight with no stopping rule for
/// `horizon` frames: met when at least [`Caps::win`] of the armed
/// defenders' metal is dead (1.0: all of it) and at most [`Caps::loss`] of
/// ours; the search climbs damage dealt less damage taken, in metal. The
/// fight's own outcome is used, not its calibrated death chances (those
/// fall to two values here, about 0.70 dead and 0.13 untouched).
pub fn cheapest_seizure(
    model: &Model,
    against: &[(Arc<UnitSpec>, f32, f32)],
    options: &[Arc<UnitSpec>],
    flags: SimFlags,
    caps: &Caps,
    horizon: i32,
    moves: Moves,
) -> Plan {
    seizure_search(model, against, options, flags, caps, horizon, moves, None)
}

/// [`cheapest_seizure`] by price instead of metal: of the forces that meet
/// the caps, the one whose expected loss plus the place's pay until it is
/// cleared is least (see [`Logistics`]).
#[allow(clippy::too_many_arguments)]
pub fn priced_seizure(
    model: &Model,
    against: &[(Arc<UnitSpec>, f32, f32)],
    options: &[Arc<UnitSpec>],
    flags: SimFlags,
    caps: &Caps,
    horizon: i32,
    moves: Moves,
    lg: &Logistics,
) -> Plan {
    let h = horizon as f32 / 30.0;
    seizure_search(
        model,
        against,
        options,
        flags,
        caps,
        horizon,
        moves,
        Some(&|p: &Plan| plan_timing(p, options, h, lg).price),
    )
}

#[allow(clippy::too_many_arguments)]
fn seizure_search(
    model: &Model,
    against: &[(Arc<UnitSpec>, f32, f32)],
    options: &[Arc<UnitSpec>],
    flags: SimFlags,
    caps: &Caps,
    horizon: i32,
    moves: Moves,
    price: Option<&dyn Fn(&Plan) -> f32>,
) -> Plan {
    if nothing_armed(against) {
        return Plan { p_win: 1.0, loss: 0.0, met: true, ..Plan::default() };
    }
    search(
        options,
        caps,
        &|pick: &[usize]| {
            let j = judge_seizure(model, &staged_fight(against, options, pick, flags), horizon, moves);
            Judged { margin: j.margin, crit: j.crit, loss: j.loss, killed: j.killed, secs: j.secs }
        },
        price,
    )
}

/// What the mechanics fight with no stopping rule does to a snapshot in
/// `horizon` frames (see [`cheapest_seizure`]): side 1 holds, side 0 moves
/// as `moves` says.
#[derive(Clone, Copy, Debug, Default)]
pub struct Seizure {
    /// Damage dealt less damage taken, in metal.
    pub margin: f32,
    /// Share of side 1's armed metal dead.
    pub crit: f32,
    /// Share of side 0's metal dead, and of side 1's.
    pub loss: f32,
    pub killed: f32,
    /// Seconds until side 1's last armed unit died (0: not all of them).
    pub secs: f32,
}

/// The seizure fight on any snapshot: a planned force (`staged_fight`) or
/// one recorded in a game, at its places (`force_post`).
pub fn judge_seizure(model: &Model, snap: &FightSnapshot, horizon: i32, moves: Moves) -> Seizure {
    let fates = match moves {
        Moves::Kiting => model.kiting_fates(snap, horizon),
        Moves::AMove => model.fates(snap, horizon, FateMode::AMove),
    };
    let (mut ours, mut lost, mut hurt_ours) = (0.0f32, 0.0f32, 0.0f32);
    let (mut theirs, mut armed, mut killed, mut killed_armed, mut hurt_theirs) =
        (0.0f32, 0.0f32, 0.0f32, 0.0f32, 0.0f32);
    let (mut last, mut all) = (0i32, true);
    for (u, f) in snap.units.iter().zip(&fates) {
        // Damage is counted from the health the unit came in with.
        let hurt = u.spec.cost * ((u.hp - f.hp_left.max(0.0)) / u.spec.health.max(1.0)).clamp(0.0, 1.0);
        if u.side == 0 {
            ours += u.spec.cost;
            hurt_ours += hurt;
            if f.dead {
                lost += u.spec.cost;
            }
        } else {
            theirs += u.spec.cost;
            hurt_theirs += hurt;
            let gun = !u.spec.weapons.is_empty();
            if gun {
                armed += u.spec.cost;
            }
            if f.dead {
                killed += u.spec.cost;
                if gun {
                    killed_armed += u.spec.cost;
                    last = last.max(f.death_frame.unwrap_or(horizon));
                }
            } else if gun {
                all = false;
            }
        }
    }
    Seizure {
        margin: hurt_theirs - hurt_ours,
        crit: killed_armed / armed.max(1.0),
        loss: lost / ours.max(1.0),
        killed: killed / theirs.max(1.0),
        secs: if all && armed > 0.0 { last as f32 / 30.0 } else { 0.0 },
    }
}

/// A unit def as `tools/forceplan/dump_specs.lua` writes it from ZK's unit
/// files: its fight spec, name and build options.
pub struct SpecRow {
    pub spec: Arc<UnitSpec>,
    pub human: String,
    pub builds: Vec<String>,
}

/// specs.tsv (see [`SpecRow`]), by def name.
pub fn load_specs(path: &str) -> Result<BTreeMap<String, SpecRow>, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("cannot read {path}: {e}"))?;
    let mut out = BTreeMap::new();
    for line in text.lines().skip(1) {
        let f: Vec<&str> = line.split('\t').collect();
        if f.len() < 12 {
            continue;
        }
        let num = |s: &str| s.parse::<f32>().unwrap_or(0.0);
        let weapons = (f[9].split('|').filter(|w| !w.is_empty()))
            .filter_map(|w| {
                let p: Vec<&str> = w.split(':').collect();
                (p.len() == 6).then(|| WeaponSpec {
                    name: p[0].into(),
                    weapon_type: p[1].into(),
                    range: num(p[2]),
                    reload: num(p[3]),
                    salvo: p[4].parse().unwrap_or(1),
                    damage: num(p[5]),
                })
            })
            .collect();
        let spec = UnitSpec {
            def: f[0].into(),
            cost: num(f[1]),
            health: num(f[2]),
            speed: num(f[3]),
            mobile: f[4] == "1",
            air: f[5] == "1",
            builder: f[6] == "1",
            com: f[7] == "1",
            radius: num(f[8]),
            weapons,
        };
        let builds = f[11].split(',').filter(|s| !s.is_empty()).map(String::from).collect();
        out.insert(f[0].to_string(), SpecRow { spec: Arc::new(spec), human: f[10].to_string(), builds });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::military::fightsim::WeaponSpec;

    fn spec(
        def: &str,
        cost: f32,
        health: f32,
        speed: f32,
        mobile: bool,
        w: (&str, &str, f32, f32, f32),
    ) -> Arc<UnitSpec> {
        Arc::new(UnitSpec {
            def: def.into(),
            cost,
            speed,
            mobile,
            weapons: vec![WeaponSpec {
                name: w.0.into(),
                weapon_type: w.1.into(),
                range: w.2,
                reload: w.3,
                salvo: 1,
                damage: w.4,
            }],
            health,
            air: false,
            builder: false,
            com: false,
            radius: 15.0,
        })
    }

    /// Stats from ZK's unit defs (tools/forceplan/dump_specs.lua).
    fn lotus() -> Arc<UnitSpec> {
        spec("turretlaser", 90.0, 785.0, 0.0, false, ("turretlaser_laser", "BeamLaser", 460.0, 0.1, 7.5))
    }
    fn fencer() -> Arc<UnitSpec> {
        spec(
            "vehsupport",
            145.0,
            530.0,
            2.8,
            true,
            ("vehsupport_cortruck_missile", "MissileLauncher", 600.0, 0.766, 40.01),
        )
    }
    fn scorcher() -> Arc<UnitSpec> {
        spec("vehraid", 130.0, 480.0, 3.7, true, ("vehraid_heatray", "LaserCannon", 262.0, 0.1, 30.0))
    }

    fn model() -> Option<Model> {
        Model::load().ok()
    }

    #[test]
    fn a_force_that_outranges_the_turrets_is_preferred_to_one_that_must_close() {
        let Some(m) = model() else { return };
        let triplet = [(lotus(), 0.0, 0.0), (lotus(), 150.0, -100.0), (lotus(), 150.0, 100.0)];
        let options = [scorcher(), fencer()];
        let plan = cheapest_force(
            &m,
            &triplet,
            &options,
            SimFlags::default(),
            &Caps { metal: 1500.0, units: 12, ..Caps::default() },
        );
        assert!(plan.met, "{plan:?}");
        assert!(plan.pick.iter().filter(|&&k| k == 1).count() >= 1, "{plan:?}");
    }

    #[test]
    fn two_fencers_clear_a_lotus_triplet_in_forty_seconds_where_scorchers_lose_some() {
        let Some(m) = model() else { return };
        let triplet = [(lotus(), 0.0, 0.0), (lotus(), 150.0, -100.0), (lotus(), 150.0, 100.0)];
        let caps = Caps { win: 1.0, metal: 1500.0, units: 12, ..Caps::default() };
        let fencers = cheapest_seizure(&m, &triplet, &[fencer()], SimFlags::default(), &caps, 30 * 40, Moves::AMove);
        assert!(fencers.met && fencers.loss == 0.0, "{fencers:?}");
        let both =
            cheapest_seizure(&m, &triplet, &[scorcher(), fencer()], SimFlags::default(), &caps, 30 * 40, Moves::AMove);
        assert!(both.met && both.cost <= fencers.cost, "{both:?}");
    }

    #[test]
    fn a_rich_place_pays_for_a_fast_force_over_slow_cheap_artillery() {
        let Some(m) = model() else { return };
        let triplet = [(lotus(), 0.0, 0.0), (lotus(), 150.0, -100.0), (lotus(), 150.0, 100.0)];
        let options = [fencer(), scorcher()];
        let caps = Caps { win: 1.0, metal: 1500.0, units: 12, ..Caps::default() };
        let (h, flags) = (30 * 90, SimFlags::default());
        let cheap = cheapest_seizure(&m, &triplet, &options, flags, &caps, h, Moves::AMove);
        let poor = priced_seizure(
            &m,
            &triplet,
            &options,
            flags,
            &caps,
            h,
            Moves::AMove,
            &Logistics::factory(10.0, 3000.0, 0.0),
        );
        assert_eq!(poor.pick, cheap.pick, "nothing at stake: the cheapest force");
        let t = plan_timing(&cheap, &options, 90.0, &Logistics::factory(10.0, 3000.0, 6.0));
        assert!(t.produce > 0.0 && t.walk > 0.0 && t.total > t.fight, "{t:?}");
        let rich = priced_seizure(
            &m,
            &triplet,
            &options,
            flags,
            &caps,
            h,
            Moves::AMove,
            &Logistics::factory(10.0, 3000.0, 50.0),
        );
        let tr = plan_timing(&rich, &options, 90.0, &Logistics::factory(10.0, 3000.0, 50.0));
        assert!(
            tr.price <= plan_timing(&cheap, &options, 90.0, &Logistics::factory(10.0, 3000.0, 50.0)).price,
            "{rich:?} {tr:?}"
        );
    }

    fn puppy() -> Arc<UnitSpec> {
        spec("jumpscout", 45.0, 80.0, 3.5, true, ("jumpscout_missile", "MissileLauncher", 168.0, 1.5, 410.1))
    }

    /// A tough unarmed building: only what a shot does to it counts.
    fn block() -> Arc<UnitSpec> {
        Arc::new(UnitSpec {
            def: "staticstorage".into(),
            cost: 100.0,
            health: 5000.0,
            radius: 20.0,
            ..UnitSpec::default()
        })
    }

    #[test]
    fn a_puppy_is_spent_by_its_one_missile() {
        let Some(m) = model() else { return };
        let snap = staged_fight(&[(block(), 0.0, 0.0)], &[puppy()], &[0], SimFlags::from_switches(false, true, true));
        let fates = m.fates(&snap, 30 * 20, FateMode::AMove);
        let (pup, wall) = (&fates[0], &fates[1]);
        assert!(pup.dead, "the Puppy is gone once it fires: {pup:?}");
        assert!(wall.hp_left >= 5000.0 - 420.0, "one missile at most in 20 s: {wall:?}");
        // The plain fight too: a short-ranged tough turret outlasts one
        // missile (refiring, eight would have killed it).
        let stub = spec("turretstub", 100.0, 3000.0, 0.0, false, ("turretstub_gun", "Cannon", 50.0, 1.0, 1.0));
        let out = m.predict(&staged_fight(&[(stub, 0.0, 0.0)], &[puppy()], &[0], SimFlags::default()));
        assert!(out.killed_b == 0.0, "one Puppy, one missile: {out:?}");
    }

    #[test]
    fn nothing_armed_needs_no_force() {
        let Some(m) = model() else { return };
        let mut unarmed = (*lotus()).clone();
        unarmed.weapons.clear();
        let plan =
            cheapest_force(&m, &[(Arc::new(unarmed), 0.0, 0.0)], &[fencer()], SimFlags::default(), &Caps::default());
        assert!(plan.met && plan.pick.is_empty());
    }

    #[test]
    fn the_force_stays_within_its_caps() {
        let Some(m) = model() else { return };
        let wall: Vec<_> = (0..10).map(|i| (lotus(), 0.0, 60.0 * i as f32)).collect();
        let caps = Caps::default();
        let plan = cheapest_force(&m, &wall, &[scorcher()], SimFlags::default(), &caps);
        assert!(plan.pick.len() <= caps.units && plan.cost <= caps.metal, "{plan:?}");
    }
}
