//! Postdiction: the force planner's judgements on fights recorded in games.
//!
//! usage: force_post <specs.tsv> < episodes.jsonl > judged.jsonl
//!
//! Each input line is one episode: our armed units and what they met, as
//! our journal knew them at its first snapshot of the fight.
//!   {"id": "...", "horizon": 60,
//!    "ours": [["cloakraid", x, z, health share], ...],
//!    "against": [["turretlaser", x, z, health share], ...],
//!    "options": ["cloakraid", "cloakskirm", ...]}   (what our factories make)
//! Each output line has, for that episode:
//!   fight: the fight model at the recorded places (`Model::predict`, as the
//!     AI asks it: P(win) and the calibrated killed shares, ours `mean_a`);
//!   seize: the mechanics fight with no stopping rule over the horizon at
//!     the recorded places, our side a-moving (`plan::judge_seizure`): the
//!     share of their armed metal dead, ours lost, the seconds to clear;
//!   staged: the same with our force set out as the planner sets its own
//!     (beyond everyone's reach), so composition is judged apart from place;
//!   plan: the cheapest seizure force of the options (`cheapest_seizure`:
//!     every armed defender dead in the horizon, at most half lost);
//!   plan_fight: the cheapest force by the fight model (`cheapest_force`).
//! With "income" (metal/s), "walk" (ground elmos from our factory) and
//! "stake" (metal/s the place pays its holder) in the episode, each plan
//! gets its time to clear and price (`plan::Logistics`), `priced` is the
//! best plan by price, and `standing` prices our recorded force as it
//! stands (nothing to build or walk; its fight at the recorded places).
//! Defs the specs lack are listed in `missing` and left out of the fight.
use std::collections::BTreeMap;
use std::io::{BufRead, Write};
use std::sync::Arc;

use serde_json::{Value, json};
use zkgbai::military::fightsim::plan::{
    Caps, Logistics, Moves, Plan, Seizure, Timing, cheapest_force, cheapest_seizure, judge_seizure, load_specs,
    plan_timing, priced_seizure, staged_fight, timing,
};
use zkgbai::military::fightsim::{FightSnapshot, Model, SimFlags, SnapUnit, UnitSpec};

fn r3(v: f32) -> f64 {
    (v as f64 * 1000.0).round() / 1000.0
}

fn seizure(j: &Seizure) -> Value {
    json!({"crit": r3(j.crit), "loss": r3(j.loss), "killed": r3(j.killed), "secs": r3(j.secs), "margin": r3(j.margin)})
}

fn plan_json(plan: &Plan, names: &[String]) -> Value {
    let mut pick: BTreeMap<&str, usize> = BTreeMap::new();
    for &k in &plan.pick {
        *pick.entry(names[k].as_str()).or_default() += 1;
    }
    json!({"pick": pick, "cost": r3(plan.cost), "crit": r3(plan.p_win), "loss": r3(plan.loss),
           "killed": r3(plan.killed), "secs": r3(plan.secs), "met": plan.met})
}

fn timing_json(t: &Timing) -> Value {
    json!({"produce": r3(t.produce), "walk": r3(t.walk), "fight": r3(t.fight), "total": r3(t.total),
           "lost": r3(t.lost), "waited": r3(t.waited), "price": r3(t.price)})
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let Some(path) = args.get(1) else {
        eprintln!("usage: force_post <specs.tsv> < episodes.jsonl");
        std::process::exit(2);
    };
    let db = load_specs(path).unwrap_or_else(|e| panic!("{e}"));
    let model = Model::load().expect("predictor tables");
    // The AI's switches: fight2d off, fightfocus and sreload on.
    let flags = SimFlags::from_switches(false, true, true);
    let caps_seize = Caps { win: 1.0, loss: 0.5, metal: 1500.0, units: 12 };
    let caps_fight = Caps { metal: 1500.0, units: 12, ..Caps::default() };
    let stdin = std::io::stdin();
    let mut out = std::io::stdout().lock();
    for line in stdin.lock().lines() {
        let line = line.expect("stdin");
        if line.trim().is_empty() {
            continue;
        }
        let ep: Value = serde_json::from_str(&line).unwrap_or_else(|e| panic!("bad episode: {e}: {line}"));
        let horizon = (ep["horizon"].as_f64().unwrap_or(60.0) * 30.0) as i32;
        let mut missing: Vec<String> = Vec::new();
        let mut side = |key: &str, s: u8| -> Vec<SnapUnit> {
            (ep[key].as_array().into_iter().flatten())
                .filter_map(|u| {
                    let def = u[0].as_str()?;
                    let Some(row) = db.get(def) else {
                        missing.push(def.to_owned());
                        return None;
                    };
                    let share = u[3].as_f64().unwrap_or(1.0).clamp(0.01, 1.0) as f32;
                    Some(SnapUnit {
                        side: s,
                        spec: row.spec.clone(),
                        hp: row.spec.health * share,
                        x: u[1].as_f64()? as f32,
                        z: u[2].as_f64()? as f32,
                        tubes: Vec::new(),
                    })
                })
                .collect()
        };
        let ours = side("ours", 0);
        let theirs = side("against", 1);
        let mut res = json!({"id": ep["id"], "missing": missing});
        if !ours.is_empty() && theirs.iter().any(|u| !u.spec.weapons.is_empty()) {
            let snap = FightSnapshot { units: ours.iter().chain(&theirs).cloned().collect(), flags };
            let f = model.predict(&snap);
            res["fight"] = json!({"p_win": r3(f.p_a_wins), "mean_a": r3(f.mean_a), "mean_b": r3(f.mean_b),
                                  "killed_a": r3(f.killed_a), "killed_b": r3(f.killed_b)});
            let at_places = judge_seizure(&model, &snap, horizon, Moves::AMove);
            res["seize"] = seizure(&at_places);
            let lg = ep["stake"].as_f64().map(|st| {
                Logistics::factory(ep["income"].as_f64().unwrap_or(15.0) as f32, ep["walk"].as_f64().unwrap_or(3000.0) as f32, st as f32)
            });
            let h = horizon as f32 / 30.0;
            if let Some(lg) = &lg {
                let cost: f32 = ours.iter().map(|u| u.spec.cost).sum();
                let t = timing(cost, 1.0, at_places.secs, h, at_places.loss, false, lg);
                res["standing"] = timing_json(&t);
            }
            // Our recorded force set out as the planner sets its own: the
            // defenders at their places (relative), ours at the start line.
            let against: Vec<(Arc<UnitSpec>, f32, f32)> = theirs.iter().map(|u| (u.spec.clone(), u.x, u.z)).collect();
            let specs: Vec<Arc<UnitSpec>> = ours.iter().map(|u| u.spec.clone()).collect();
            let pick: Vec<usize> = (0..specs.len()).collect();
            let mut staged = staged_fight(&against, &specs, &pick, flags);
            for (u, o) in staged.units.iter_mut().filter(|u| u.side == 0).zip(&ours) {
                u.hp = o.hp;
            }
            res["staged"] = seizure(&judge_seizure(&model, &staged, horizon, Moves::AMove));
            let names: Vec<String> = (ep["options"].as_array().into_iter().flatten())
                .filter_map(|o| o.as_str())
                .filter(|n| db.get(*n).is_some_and(|r| r.spec.mobile && !r.spec.air && !r.spec.builder && !r.spec.weapons.is_empty()))
                .map(String::from)
                .collect();
            if !names.is_empty() {
                let options: Vec<Arc<UnitSpec>> = names.iter().map(|n| db[n].spec.clone()).collect();
                let p = cheapest_seizure(&model, &against, &options, flags, &caps_seize, horizon, Moves::AMove);
                res["plan"] = plan_json(&p, &names);
                if let Some(lg) = &lg {
                    res["plan"]["timing"] = timing_json(&plan_timing(&p, &options, h, lg));
                    let q = priced_seizure(&model, &against, &options, flags, &caps_seize, horizon, Moves::AMove, lg);
                    res["priced"] = plan_json(&q, &names);
                    res["priced"]["timing"] = timing_json(&plan_timing(&q, &options, h, lg));
                }
                let p = cheapest_force(&model, &against, &options, flags, &caps_fight);
                res["plan_fight"] = plan_json(&p, &names);
            }
        }
        writeln!(out, "{res}").expect("stdout");
    }
}
