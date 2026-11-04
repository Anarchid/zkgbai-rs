//! Invasion targets: which enemy holding to take next, and with what (user,
//! 2026-11-03: "this sort of thing will require more than the centerline
//! objective generators; what would be a good invasion-target selector?
//! Clusterized mexes, regions?" -> region pieces cut around metal-spot
//! clusters, priced by time). Run on situations taken from recorded games.
//!
//! usage: invasion_post <specs.tsv> < situations.jsonl > ranked.jsonl
//!
//! A situation is one moment of one game as our journal knew it:
//!   {"id", "zktamap": path, "spots": [[x, z, value]...], "tier": "bot",
//!    "ours": [[def, x, z, health share]...]   (our armed mobile units),
//!    "our_mexes": [[x, z]...], "factories": [[def, x, z]...], "income",
//!    "enemy": [[def, x, z, frames since seen]...], "enemy_base": [x, z]}
//!
//! The candidates are the terrain's region pieces for our tier (regions cut
//! around clusters of metal spots, `Terrain::build_with_spots`) with a spot
//! that is not ours, of three kinds (user, 2026-11-03: seize what they hold,
//! what they hold but have not colonized, "and regions they do not hold and
//! have not colonized ... but which we forgot to build up ourselves"):
//! - held: an enemy mex we know of stands on one of its spots;
//! - contested: free spots, and enemy armed units seen there in the last
//!   30 s or, where none is seen, what killed our units there in the last
//!   5 minutes (each killer once): the raiders that keep the ground;
//! - open: free spots and nobody about.
//! For each:
//! - stake: what its enemy-held spots pay the enemy, metal/s; gain: that
//!   and all its spots' income, ours once taken;
//! - defenders: the known armed enemy buildings covering its spots (reach
//!   + 50) or standing in it, and the armed units seen there (10 s on held
//!   ground, 30 s on empty) or the killers above; their armed metal `D`;
//! - the build after the clear: a mex on every spot (85 metal each) and,
//!   unless open, a Lotus (90), at a constructor's 5 metal/s, by our nearest
//!   constructor or a new one from a factory; the income starts halfway
//!   through the build, and the build's metal is spent;
//! - each source of force: a constructor alone where nobody is about;
//!   every group of our armed units (600 elmos apart at most) as it stands,
//!   or a detachment of it; and every factory that makes armed ground units
//!   with the force planner's plan (by price for buildings, the fight test
//!   against units only: a riot for raiders) scaled to the metal that nets
//!   most; time = build (factories) + ground walk at the slowest unit's
//!   speed + fight (the seizure fight's seconds);
//! - the chance to clear and the share lost from the metal ratio, as 782
//!   recorded assaults put them (zkgbai-runs/forceplan/post: P = logistic
//!   of -2.123 + 1.653 ln ratio, lost = 0.482 - 0.152 ln ratio);
//! - holding it: every known enemy army away from the piece that can walk
//!   there before our turrets would stand (our time + 60 s) retakes it by
//!   the same odds, its metal against what we keep there; the enemy base
//!   counts as an army of what a factory builds (10 metal/s) in that time
//!   less its walk at Glaive speed. The hold factor is the chance none of
//!   them retakes it;
//! - a group much bigger than the job sends a detachment of 2, 3, 5 or 8
//!   times the defenders' metal (or all of itself);
//! - reclaim (user, 2026-11-03): ZK wrecks keep 40% of a unit's cost, so
//!   holding the piece also gives 40% of what died there in the last 5
//!   minutes (as the journal saw it: an upper bound, some may be reclaimed
//!   already), of the defenders killed and of our own losses there;
//! - net = P x hold x (gain x max(0, 480 - income start) + reclaim) - lost
//!   share x metal - metal x force time / 600 (the force's time, worth its
//!   metal over ten minutes) - the build's metal.
//! The pieces come out ranked by their best source's net.
use std::collections::HashMap;
use std::io::{BufRead, Write};
use std::path::Path;
use std::sync::Arc;

use serde_json::{Value, json};
use zk_terrain::paths::Paths;
use zk_terrain::{Pos, SpringMapData, Terrain, Tier, TierMap};
use zkgbai::military::fightsim::plan::{
    Caps, FACTORY_BUILD_POWER, Logistics, Moves, SpecRow, cheapest_force, judge_seizure, load_specs, plan_timing, priced_seizure,
    staged_fight,
};
use zkgbai::military::fightsim::{Model, SimFlags, UnitSpec};

/// Seconds over which a holding's income is counted: how long our mexes
/// built after 5:00 lasted in the cloak battery and the map sweep (survival
/// median 494 s over 5608 mexes, those standing at the end counted).
const HORIZON: f32 = 480.0;
/// Our units this near each other are one group.
const GROUP_LINK: f32 = 600.0;
/// Enemy units seen this recently (frames) count as standing where seen.
const SEEN: f64 = 300.0;
/// ...and enemy armies seen this recently can come back.
const ARMY_SEEN: f64 = 900.0;
/// Metal a wreck keeps of its unit's cost (ZK featuredefs_post: wreck 0.4,
/// heap 0.2).
const WRECK: f32 = 0.4;
/// Enemy units seen this recently keep empty ground contested.
const PRESENCE_SEEN: f64 = 900.0;
/// The build after a clear (ZK unit files): a mex, a Lotus, a constructor's
/// build power (metal per second).
const MEX_COST: f32 = 85.0;
const TURRET_COST: f32 = 90.0;
const CON_BUILD_POWER: f32 = 5.0;
/// Spot to region snapping, elmos.
const SNAP: f32 = 120.0;
/// What an army's time is worth: its own metal over this many seconds (a
/// force sent here is not taking or holding anything else meanwhile; without
/// it, whole 7000-metal armies went for single Lotuses).
const TIE_UP_SECS: f32 = 600.0;
/// A Glaive's speed, elmos a frame: the enemy's base army when none is seen.
const BASE_SPEED: f32 = 2.5;

fn p_clear(ratio: f32) -> f32 {
    if !ratio.is_finite() {
        return 1.0;
    }
    1.0 / (1.0 + (-(-2.123 + 1.653 * ratio.max(0.05).ln())).exp())
}

fn lost_share(ratio: f32) -> f32 {
    if !ratio.is_finite() {
        return 0.0;
    }
    (0.482 - 0.152 * ratio.max(0.05).ln()).clamp(0.0, 1.0)
}

/// Units whose guns only reach aircraft (ZK names them all with "aa":
/// Razor, Hacksaw, Chainsaw, Gnat-swatting bots), not ground defenders.
fn anti_air(def: &str) -> bool {
    def.contains("aa")
}

fn r2(v: f32) -> f64 {
    (v as f64 * 100.0).round() / 100.0
}

fn tier_named(n: &str) -> Option<Tier> {
    [Tier::Spider, Tier::Bot, Tier::Vehicle, Tier::Hover, Tier::Ship, Tier::Amphib].into_iter().find(|t| t.name() == n)
}

struct Unit {
    def: String,
    pos: Pos,
    spec: Arc<UnitSpec>,
    hp: f32,
}

/// Single-linkage groups of `units` within `link` elmos.
fn groups(units: &[Unit], link: f32) -> Vec<Vec<usize>> {
    let mut seen = vec![false; units.len()];
    let mut out = Vec::new();
    for i in 0..units.len() {
        if seen[i] {
            continue;
        }
        seen[i] = true;
        let (mut g, mut k) = (vec![i], 0);
        while k < g.len() {
            let a = g[k];
            for j in 0..units.len() {
                if !seen[j] && units[a].pos.distance(units[j].pos) <= link {
                    seen[j] = true;
                    g.push(j);
                }
            }
            k += 1;
        }
        out.push(g);
    }
    out
}

fn centroid(units: &[&Unit]) -> Pos {
    let n = units.len().max(1) as f32;
    Pos::new(units.iter().map(|u| u.pos.x).sum::<f32>() / n, units.iter().map(|u| u.pos.z).sum::<f32>() / n)
}

/// Ground elmos from `from` into region `r` (`None`: no way).
fn walk(map: &TierMap, from: Pos, r: usize) -> Option<f32> {
    let d = Paths::new(map, from, |_| false).to_region(r);
    d.is_finite().then_some(d)
}

struct Source {
    label: String,
    metal: f32,
    pick: Vec<(String, usize)>,
    produce: f32,
    walk: f32,
    fight: f32,
    judged_clear: bool,
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let Some(path) = args.get(1) else {
        eprintln!("usage: invasion_post <specs.tsv> < situations.jsonl");
        std::process::exit(2);
    };
    let db: std::collections::BTreeMap<String, SpecRow> = load_specs(path).unwrap_or_else(|e| panic!("{e}"));
    let model = Model::load().expect("predictor tables");
    let flags = SimFlags::from_switches(false, true, true);
    let caps = Caps { win: 1.0, loss: 0.5, metal: 1500.0, units: 12 };
    let horizon = 60 * 30;
    let mut terrains: HashMap<(String, String), Option<Terrain>> = HashMap::new();
    let stdin = std::io::stdin();
    let mut out = std::io::stdout().lock();
    for line in stdin.lock().lines() {
        let line = line.expect("stdin");
        if line.trim().is_empty() {
            continue;
        }
        let s: Value = serde_json::from_str(&line).unwrap_or_else(|e| panic!("bad situation: {e}"));
        let id = s["id"].clone();
        let tier_name = s["tier"].as_str().unwrap_or("bot").to_owned();
        let Some(tier) = tier_named(&tier_name) else { continue };
        let zkta = s["zktamap"].as_str().unwrap_or_default().to_owned();
        let spots: Vec<(Pos, f32)> = (s["spots"].as_array().into_iter().flatten())
            .filter_map(|v| Some((Pos::new(v[0].as_f64()? as f32, v[1].as_f64()? as f32), v[2].as_f64()? as f32)))
            .collect();
        let terrain = terrains.entry((zkta.clone(), tier_name.clone())).or_insert_with(|| {
            let data = SpringMapData::load(Path::new(&zkta)).ok()?;
            let cut: Vec<Pos> = spots.iter().map(|s| s.0).collect();
            Some(Terrain::build_with_spots(&data, &[(tier, tier.zero_k_class())], &cut))
        });
        let Some(map) = terrain.as_ref().and_then(|t| t.tier(tier)) else {
            writeln!(out, "{}", json!({"id": id, "error": "no terrain"})).ok();
            continue;
        };
        let unit = |v: &Value| -> Option<Unit> {
            let def = v[0].as_str()?.to_owned();
            let spec = db.get(&def)?.spec.clone();
            Some(Unit { def, pos: Pos::new(v[1].as_f64()? as f32, v[2].as_f64()? as f32), spec, hp: v[3].as_f64().unwrap_or(1.0) as f32 })
        };
        let ours: Vec<Unit> = (s["ours"].as_array().into_iter().flatten()).filter_map(unit).collect();
        let enemy: Vec<(Unit, f64)> = (s["enemy"].as_array().into_iter().flatten())
            .filter_map(|v| Some((unit(v)?, v[3].as_f64().unwrap_or(0.0))))
            .collect();
        let our_mexes: Vec<Pos> = (s["our_mexes"].as_array().into_iter().flatten())
            .filter_map(|v| Some(Pos::new(v[0].as_f64()? as f32, v[1].as_f64()? as f32)))
            .collect();
        let income = s["income"].as_f64().unwrap_or(10.0) as f32;
        let cons: Vec<(Pos, f32)> = (s["cons"].as_array().into_iter().flatten())
            .filter_map(|v| Some((Pos::new(v[1].as_f64()? as f32, v[2].as_f64()? as f32), db.get(v[0].as_str()?)?.spec.speed)))
            .collect();
        let enemy_base = s["enemy_base"].as_array().map(|v| Pos::new(v[0].as_f64().unwrap_or(0.0) as f32, v[1].as_f64().unwrap_or(0.0) as f32));
        // Who holds each spot: ours, the enemy's (a known mex within 60), or free.
        let enemy_mexes: Vec<Pos> = enemy.iter().filter(|(u, _)| u.def == "staticmex").map(|(u, _)| u.pos).collect();
        let held = |p: Pos, by: &[Pos]| by.iter().any(|m| m.distance(p) <= 60.0);
        let mut pieces: HashMap<usize, Vec<usize>> = HashMap::new();
        for (k, (p, _)) in spots.iter().enumerate() {
            if let Some(r) = map.region_at(*p).or_else(|| map.region_near(*p, SNAP)) {
                pieces.entry(r).or_default().push(k);
            }
        }
        let our_regions: std::collections::HashSet<usize> = (our_mexes.iter().chain(ours.iter().map(|u| &u.pos)))
            .filter_map(|p| map.region_near(*p, SNAP))
            .collect();
        // Sources: our standing groups, and our factories' priced plans.
        let armed_ours: Vec<&Unit> = ours.iter().filter(|u| !u.spec.weapons.is_empty() && u.spec.mobile && !u.spec.builder).collect();
        let owned: Vec<Unit> = armed_ours.iter().map(|u| Unit { def: u.def.clone(), pos: u.pos, spec: u.spec.clone(), hp: u.hp }).collect();
        let our_groups: Vec<Vec<&Unit>> = groups(&owned, GROUP_LINK).into_iter().map(|g| g.into_iter().map(|i| &owned[i]).collect()).collect();
        let factories: Vec<(String, Pos, Vec<String>)> = (s["factories"].as_array().into_iter().flatten())
            .filter_map(|v| {
                let def = v[0].as_str()?.to_owned();
                let builds: Vec<String> = db.get(&def)?.builds.iter()
                    .filter(|b| db.get(*b).is_some_and(|r| r.spec.mobile && !r.spec.air && !r.spec.builder && !r.spec.weapons.is_empty()))
                    .cloned().collect();
                let at = Pos::new(v[1].as_f64()? as f32, v[2].as_f64()? as f32);
                (!builds.is_empty()).then_some((def, at, builds))
            })
            .collect();
        // The enemy's armies seen lately, for its time to come back.
        let movers: Vec<Unit> = (enemy.iter())
            .filter(|(u, age)| *age <= ARMY_SEEN && u.spec.mobile && !u.spec.weapons.is_empty() && !u.spec.air && !u.spec.builder)
            .map(|(u, _)| Unit { def: u.def.clone(), pos: u.pos, spec: u.spec.clone(), hp: u.hp })
            .collect();
        let armies: Vec<(Pos, f32, f32)> = groups(&movers, GROUP_LINK).into_iter()
            .map(|g| {
                let us: Vec<&Unit> = g.iter().map(|&i| &movers[i]).collect();
                let metal: f32 = us.iter().map(|u| u.spec.cost).sum();
                let speed = us.iter().map(|u| u.spec.speed).fold(f32::INFINITY, f32::min);
                (centroid(&us), metal, speed)
            })
            .filter(|a| a.1 >= 150.0)
            .collect();
        let mut ranked = Vec::new();
        for (&r, ks) in &pieces {
            let theirs: Vec<usize> = ks.iter().copied().filter(|&k| held(spots[k].0, &enemy_mexes)).collect();
            let free: Vec<usize> = ks.iter().copied().filter(|&k| !held(spots[k].0, &enemy_mexes) && !held(spots[k].0, &our_mexes)).collect();
            if theirs.is_empty() && free.is_empty() {
                continue;
            }
            // The spots we would build on once the piece is ours.
            let targets: Vec<usize> = theirs.iter().chain(&free).copied().collect();
            let stake: f32 = theirs.iter().map(|&k| spots[k].1).sum();
            let ours_after: f32 = targets.iter().map(|&k| spots[k].1).sum();
            let gain = stake + ours_after;
            let in_piece = |p: Pos| map.region_near(p, SNAP) == Some(r);
            let near = |p: Pos| in_piece(p) || targets.iter().any(|&k| p.distance(spots[k].0) <= 600.0);
            let covers = |u: &Unit| targets.iter().any(|&k| u.pos.distance(spots[k].0) <= u.spec.weapons.iter().map(|w| w.range).fold(0.0, f32::max) + 50.0);
            // Held ground counts units seen there in the last 10 s; empty
            // ground the enemy only patrols, the last 30 s.
            let seen = if theirs.is_empty() { PRESENCE_SEEN } else { SEEN };
            let mut defenders: Vec<&Unit> = (enemy.iter())
                .filter(|(u, age)| {
                    !u.spec.weapons.is_empty() && !u.spec.air && !u.spec.builder && !anti_air(&u.def)
                        && if u.spec.mobile { *age <= seen && near(u.pos) } else { covers(u) || in_piece(u.pos) }
                })
                .map(|(u, _)| u)
                .collect();
            // What killed our units there in the last 5 minutes, where no
            // enemy unit is seen now: the raiders that keep the ground.
            let mut killer_ids = std::collections::HashSet::new();
            let killers: Vec<Unit> = (s["deaths"].as_array().into_iter().flatten())
                .filter_map(|v| {
                    if v[4].as_str()? != "ours" {
                        return None;
                    }
                    let p = Pos::new(v[1].as_f64()? as f32, v[2].as_f64()? as f32);
                    let by = v[5].as_str()?;
                    let spec = db.get(by)?.spec.clone();
                    (near(p) && spec.mobile && !spec.weapons.is_empty() && !anti_air(by) && killer_ids.insert(v[6].as_i64().unwrap_or(-1)))
                        .then(|| Unit { def: by.to_owned(), pos: p, spec, hp: 1.0 })
                })
                .collect();
            if defenders.iter().all(|u| !u.spec.mobile) {
                defenders.extend(killers.iter());
            }
            let kind = if !theirs.is_empty() { "held" } else if !defenders.is_empty() { "contested" } else { "open" };
            let d_metal: f32 = defenders.iter().map(|u| u.spec.cost).sum();
            let wrecks: f32 = WRECK * (s["deaths"].as_array().into_iter().flatten())
                .filter_map(|v| {
                    let p = Pos::new(v[1].as_f64()? as f32, v[2].as_f64()? as f32);
                    near(p).then(|| db.get(v[0].as_str()?).map(|r| r.spec.cost))?
                })
                .sum::<f32>();
            let against: Vec<(Arc<UnitSpec>, f32, f32)> = defenders.iter().map(|u| (u.spec.clone(), u.pos.x, u.pos.z)).collect();
            let only_mobiles = !against.is_empty() && against.iter().all(|a| a.0.mobile);
            let center = map.regions[r].center;
            // The build once it is clear: a mex on every target spot, and a
            // turret where anyone contests it, by our nearest constructor or
            // a new one from a factory.
            let turret = if kind == "open" { 0.0 } else { TURRET_COST };
            let build_metal = MEX_COST * targets.len() as f32 + turret;
            let build_secs = build_metal / CON_BUILD_POWER;
            let con_from_field = (cons.iter())
                .filter_map(|(p, speed)| walk(map, *p, r).map(|d| d / (speed.max(0.1) * 30.0)))
                .fold(f32::INFINITY, f32::min);
            let con_from_factory = (factories.iter())
                .filter_map(|(fdef, fpos, _)| {
                    let con = db.get(fdef)?.builds.iter().find(|b| db.get(*b).is_some_and(|r| r.spec.builder && r.spec.mobile))?;
                    let spec = &db.get(con)?.spec;
                    walk(map, *fpos, r).map(|d| spec.cost / income.clamp(0.5, FACTORY_BUILD_POWER) + d / (spec.speed.max(0.1) * 30.0))
                })
                .fold(f32::INFINITY, f32::min);
            let t_con = con_from_field.min(con_from_factory);
            if !t_con.is_finite() {
                continue;
            }
            // Who can come back: the known armies away from the piece (its
            // defenders are counted above), and the enemy base's factory.
            let threats: Vec<(f32, f32)> = (armies.iter())
                .filter(|(p, _, _)| !near(*p))
                .filter_map(|(p, m, speed)| walk(map, *p, r).map(|d| (d / (speed.max(0.1) * 30.0), *m)))
                .collect();
            let base_walk = enemy_base.and_then(|b| walk(map, b, r)).map(|d| d / (BASE_SPEED * 30.0));
            let eta = threats.iter().map(|t| t.0).chain(base_walk).fold(f32::INFINITY, f32::min);
            // P(clear), metal lost, P(hold), benefit, net, and when the income
            // starts, of `metal` arriving and clearing in `t_force` seconds and
            // the build after it.
            let evaluate = |metal: f32, t_force: f32| -> (f32, f32, f32, f32, f32, f32) {
                let ratio = if d_metal > 0.0 { metal / d_metal } else { f32::INFINITY };
                let (p, share) = (p_clear(ratio), lost_share(ratio));
                let start = t_force.max(t_con);
                let income_from = start + build_secs / 2.0;
                let done = start + build_secs;
                let keep = (metal * (1.0 - share) + turret).max(1.0);
                let window = done + 60.0;
                let mut hold = 1.0f32;
                for &(e, m) in &threats {
                    if e <= window {
                        hold *= 1.0 - p_clear(m / keep);
                    }
                }
                if let Some(bw) = base_walk {
                    let built = FACTORY_BUILD_POWER * (window - bw).max(0.0);
                    if built > 0.0 {
                        hold *= 1.0 - p_clear(built / keep);
                    }
                }
                let reclaim = wrecks + WRECK * (d_metal + share * metal);
                let benefit = (gain * (HORIZON - income_from).max(0.0) + reclaim) * hold;
                let tied = metal * t_force / TIE_UP_SECS;
                (p, share * metal, hold, benefit, p * benefit - share * metal - tied - build_metal, income_from)
            };
            let mut sources: Vec<Source> = Vec::new();
            if against.is_empty() {
                sources.push(Source { label: "constructor".into(), metal: 0.0, pick: vec![], produce: 0.0, walk: 0.0, fight: 0.0, judged_clear: true });
            }
            for (gi, g) in our_groups.iter().enumerate() {
                let metal_g: f32 = g.iter().map(|u| u.spec.cost * u.hp.clamp(0.05, 1.0)).sum();
                if metal_g < 100.0 {
                    continue;
                }
                let from = centroid(g);
                let Some(w) = walk(map, from, r) else { continue };
                let speed = g.iter().map(|u| u.spec.speed).fold(f32::INFINITY, f32::min).max(0.1);
                let (fight0, clear) = if against.is_empty() {
                    (0.0, true)
                } else {
                    let specs: Vec<Arc<UnitSpec>> = g.iter().map(|u| u.spec.clone()).collect();
                    let pick: Vec<usize> = (0..specs.len()).collect();
                    let j = judge_seizure(&model, &staged_fight(&against, &specs, &pick, flags), horizon, Moves::AMove);
                    (if j.secs > 0.0 { j.secs } else { 30.0 }, j.crit >= 0.999 && j.loss <= 0.5)
                };
                // The whole group, or a detachment sized to the job (an
                // escort, where nobody holds the ground).
                let mut sizes: Vec<f32> = [2.0f32, 3.0, 5.0, 8.0].iter().map(|k| k * d_metal).filter(|m| *m >= 100.0 && *m < metal_g).collect();
                if d_metal <= 0.0 {
                    sizes.push(metal_g.min(300.0));
                }
                sizes.push(metal_g);
                let mut best: Option<(f32, f32, f32)> = None;
                for m in sizes {
                    let fight = if fight0 > 0.0 { (fight0 * metal_g / m).max(3.0) } else { 0.0 };
                    let net = evaluate(m, w / (speed * 30.0) + fight).4;
                    if best.is_none_or(|b| net > b.0) {
                        best = Some((net, m, fight));
                    }
                }
                let Some((_, m, fight)) = best else { continue };
                let mut count: HashMap<String, usize> = HashMap::new();
                for u in g {
                    *count.entry(u.def.clone()).or_default() += 1;
                }
                let label = if m < metal_g { format!("group {gi} (part)") } else { format!("group {gi}") };
                sources.push(Source { label, metal: m, pick: count.into_iter().collect(), produce: 0.0, walk: w / (speed * 30.0), fight,
                                      judged_clear: clear });
            }
            for (fdef, fpos, builds) in &factories {
                if against.is_empty() {
                    break;
                }
                let Some(w) = walk(map, *fpos, r) else { continue };
                let options: Vec<Arc<UnitSpec>> = builds.iter().map(|b| db[b].spec.clone()).collect();
                let lg = Logistics::factory(income, w, stake.max(ours_after));
                // Only units there: the fight test (a riot for raiders);
                // buildings too: the seizure test.
                let base = if only_mobiles {
                    cheapest_force(&model, &against, &options, flags, &Caps { metal: 1500.0, units: 12, ..Caps::default() })
                } else {
                    priced_seizure(&model, &against, &options, flags, &caps, horizon, Moves::AMove, &lg)
                };
                // The plan's mix, scaled to the metal that nets most (the
                // planner's own size is often too small by the recorded
                // assaults' odds).
                let (mix, unit_metal, speed, fight0, cost0) = if !base.pick.is_empty() {
                    let t = plan_timing(&base, &options, 60.0, &lg);
                    let speed = base.pick.iter().map(|&k| options[k].speed).fold(f32::INFINITY, f32::min);
                    (base.pick.clone(), base.cost, speed, if only_mobiles { 15.0 } else { t.fight }, base.cost)
                } else {
                    let k = (0..options.len()).min_by(|&a, &b| options[a].cost.total_cmp(&options[b].cost)).unwrap_or(0);
                    (vec![k], options[k].cost, options[k].speed, 5.0, options[k].cost)
                };
                let mut best: Option<(f32, Source)> = None;
                for scale in [1.0f32, 1.5, 2.0, 3.0, 4.0, 6.0] {
                    let metal = unit_metal * scale;
                    if metal > 3000.0 {
                        break;
                    }
                    let t_produce = metal / lg.rate;
                    let t_walk = w / (speed.max(0.1) * 30.0);
                    let t_fight = (fight0 * cost0 / metal).max(3.0);
                    let net = evaluate(metal, t_produce + t_walk + t_fight).4;
                    if best.as_ref().is_none_or(|b| net > b.0) {
                        let mut count: HashMap<String, usize> = HashMap::new();
                        for &k in &mix {
                            *count.entry(builds[k].clone()).or_default() += scale.round() as usize;
                        }
                        best = Some((net, Source { label: format!("factory {fdef}"), metal, pick: count.into_iter().collect(),
                                                   produce: t_produce, walk: t_walk, fight: t_fight, judged_clear: base.met }));
                    }
                }
                if let Some((_, src)) = best {
                    sources.push(src);
                }
            }
            let frontier = our_regions.contains(&r) || map.neighbours(r).any(|(_, n)| our_regions.contains(&n));
            let mut options: Vec<Value> = Vec::new();
            for src in &sources {
                let t_force = src.produce + src.walk + src.fight;
                let ratio = if d_metal > 0.0 { src.metal / d_metal } else { f32::INFINITY };
                let (p, lost, hold, benefit, net, income_from) = evaluate(src.metal, t_force);
                options.push(json!({"source": src.label, "metal": r2(src.metal), "pick": src.pick, "ratio": if ratio.is_finite() { r2(ratio) } else { -1.0 },
                                    "p_clear": r2(p), "judged_clear": src.judged_clear, "produce": r2(src.produce), "walk": r2(src.walk),
                                    "fight": r2(src.fight), "total": r2(t_force.max(t_con) + build_secs), "income_from": r2(income_from),
                                    "lost": r2(lost), "hold": r2(hold), "benefit": r2(benefit), "net": r2(net)}));
            }
            options.sort_by(|a, b| b["net"].as_f64().unwrap_or(f64::MIN).total_cmp(&a["net"].as_f64().unwrap_or(f64::MIN)));
            let best_net = options.first().and_then(|o| o["net"].as_f64()).unwrap_or(f64::MIN);
            ranked.push((best_net, json!({
                "region": r, "kind": kind, "center": [center.x.round(), center.z.round()], "spots": ks.len(), "theirs": theirs.len(), "free": free.len(),
                "stake": r2(stake), "gain": r2(gain), "def_metal": r2(d_metal), "wrecks": r2(wrecks),
                "turrets": defenders.iter().filter(|u| !u.spec.mobile).count(), "mobiles": defenders.iter().filter(|u| u.spec.mobile).count(),
                "defenders": defenders.iter().map(|u| u.def.clone()).collect::<Vec<_>>(), "killers": killers.len(),
                "spot_xy": theirs.iter().map(|&k| [spots[k].0.x.round(), spots[k].0.z.round()]).collect::<Vec<_>>(),
                "free_xy": free.iter().map(|&k| [spots[k].0.x.round(), spots[k].0.z.round()]).collect::<Vec<_>>(),
                "build_metal": r2(build_metal), "build_secs": r2(build_secs), "con_secs": r2(t_con),
                "frontier": frontier, "enemy_eta": if eta.is_finite() { r2(eta) } else { -1.0 },
                "best": options.first().cloned(), "options": options,
            })));
        }
        ranked.sort_by(|a, b| b.0.total_cmp(&a.0));
        let pieces: Vec<Value> = ranked.into_iter().map(|(_, v)| v).collect();
        writeln!(out, "{}", json!({"id": id, "pieces": pieces, "groups": our_groups.len(), "armies": armies.len()})).ok();
    }
}
