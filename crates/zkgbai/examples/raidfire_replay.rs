//! Replay recorded raid-defense rollouts offline under every raid-aim rule
//! (`mech::RaidAim`), with and without `fightsim::SimFlags::focus` (2026-11-02,
//! zkgbai-runs/raidfire). Each stdin line is one rollout, each stdout line
//! its fates per variant.
//!
//! usage: raidfire_replay SPECS.tsv [THREADS] < sims.jsonl > fates.jsonl
//!
//! SPECS.tsv: `def cost health speed(elmos/frame) mobile air builder com
//! radius weapons(name:type:range:reload:salvo:damage|...)` (the format of
//! zkgbai-runs/ecodefend/replay/specs.tsv).
//! In: `{"id": .., "horizon": frames, "units": [[def, side, hp, x, z,
//! arrival frames, focus index or null], ..], "assets": [index, ..],
//! "paralysis": [[index, share], ..]}` (an ecodefend judge record's `snap`,
//! cut to a prefix). Out: `{"id": .., "fates": {variant: [[p_dead, hp_left,
//! p_raw, death_frame or -1], ..]}}` in input order, or `{"id": .., "error": ..}`.

use std::collections::HashMap;
use std::io::{BufRead, Write};
use std::sync::Arc;

use serde_json::{Map, Value, json};
use zkgbai::military::fightsim::mech::{Defense, RaidAim};
use zkgbai::military::fightsim::{FightSnapshot, Model, SimFlags, SnapUnit, UnitSpec, WeaponSpec};

/// The variants: name, least-health targeting, raid aim.
const VARIANTS: [(&str, bool, RaidAim); 6] = [
    ("base", false, RaidAim::Assets),
    ("hold", false, RaidAim::Hold),
    ("free", false, RaidAim::Free),
    ("focus", true, RaidAim::Assets),
    ("focus_hold", true, RaidAim::Hold),
    ("focus_free", true, RaidAim::Free),
];

fn specs(path: &str) -> HashMap<String, Arc<UnitSpec>> {
    let text = std::fs::read_to_string(path).expect("specs");
    text.lines()
        .filter(|l| !l.starts_with('#') && !l.starts_with("def\t") && !l.trim().is_empty())
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

fn run(model: &Model, specs: &HashMap<String, Arc<UnitSpec>>, line: &str) -> Value {
    let v: Value = match serde_json::from_str(line) {
        Ok(v) => v,
        Err(e) => return json!({"error": format!("parse: {e}")}),
    };
    let id = v["id"].clone();
    let mut units = Vec::new();
    let mut arrivals = Vec::new();
    let mut focus = Vec::new();
    for u in v["units"].as_array().into_iter().flatten() {
        let def = u[0].as_str().unwrap_or("");
        let Some(spec) = specs.get(def) else { return json!({"id": id, "error": format!("unknown def {def}")}) };
        units.push(SnapUnit {
            reloads: Vec::new(),
            side: u[1].as_u64().unwrap_or(0) as u8,
            spec: spec.clone(),
            hp: u[2].as_f64().unwrap_or(0.0) as f32,
            x: u[3].as_f64().unwrap_or(0.0) as f32,
            z: u[4].as_f64().unwrap_or(0.0) as f32,
        });
        arrivals.push(u[5].as_i64().unwrap_or(0) as i32);
        focus.push(u[6].as_u64().map(|i| i as usize));
    }
    let assets: Vec<usize> =
        v["assets"].as_array().into_iter().flatten().filter_map(|a| a.as_u64()).map(|a| a as usize).collect();
    let paralysis: Vec<(usize, f32)> = (v["paralysis"].as_array().into_iter().flatten())
        .filter_map(|p| Some((p[0].as_u64()? as usize, p[1].as_f64()? as f32)))
        .collect();
    let horizon = v["horizon"].as_i64().unwrap_or(900) as i32;
    let mut out = Map::new();
    for (name, least_health, raid_aim) in VARIANTS {
        let defense = Defense { arrivals: arrivals.clone(), assets: assets.clone(), focus: focus.clone(), raid_aim };
        let flags = SimFlags::from_switches(false, least_health, false); // the frozen fights, no tubes
        let snap = FightSnapshot { units: units.clone(), flags };
        let fates = model.defense_fates(&snap, horizon, &defense, &paralysis);
        let rows: Vec<Value> =
            fates.iter().map(|f| json!([f.p_dead, f.hp_left, f.p_raw, f.death_frame.unwrap_or(-1)])).collect();
        out.insert(name.into(), Value::Array(rows));
    }
    json!({"id": id, "fates": out})
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let specs = specs(args.get(1).expect("usage: raidfire_replay SPECS.tsv [THREADS]"));
    let threads: usize = args.get(2).and_then(|t| t.parse().ok()).unwrap_or(4).max(1);
    let lines: Vec<String> =
        std::io::stdin().lock().lines().map_while(Result::ok).filter(|l| !l.trim().is_empty()).collect();
    let chunk = lines.len().div_ceil(threads).max(1);
    let outs: Vec<Vec<String>> = std::thread::scope(|s| {
        let handles: Vec<_> = lines
            .chunks(chunk)
            .map(|part| {
                let specs = &specs;
                s.spawn(move || {
                    let model = Model::load().expect("model");
                    part.iter().map(|l| run(&model, specs, l).to_string()).collect::<Vec<String>>()
                })
            })
            .collect();
        handles.into_iter().map(|h| h.join().expect("thread")).collect()
    });
    let stdout = std::io::stdout();
    let mut w = std::io::BufWriter::new(stdout.lock());
    for o in outs.into_iter().flatten() {
        writeln!(w, "{o}").unwrap();
    }
}
