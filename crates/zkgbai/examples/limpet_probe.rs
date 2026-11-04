//! Offline marginal Limpet commitment probe. Input: tools/predictor/limpet_probe.py.
//! cargo run -p zkgbai --example limpet_probe -- INPUT.json > results.jsonl
use serde_json::{Value, json};
use std::{collections::HashMap, sync::Arc};
use zkgbai::military::fightsim::{FateMode, FightSnapshot, Model, SimFlags, SnapUnit, UnitSpec, WeaponSpec};

fn num(v: &Value, k: &str) -> f32 {
    v[k].as_f64().unwrap() as f32
}

fn main() {
    let path = std::env::args().nth(1).expect("INPUT.json");
    let data: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    let specs: HashMap<_, _> = data["specs"]
        .as_object()
        .unwrap()
        .iter()
        .map(|(name, v)| {
            let spec = UnitSpec {
                def: name.clone(),
                cost: num(v, "cost"),
                health: num(v, "health"),
                speed: num(v, "speed"),
                radius: num(v, "radius"),
                mobile: v["mobile"].as_bool().unwrap(),
                air: v["air"].as_bool().unwrap(),
                builder: v["builder"].as_bool().unwrap(),
                weapons: v["weapons"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|w| WeaponSpec {
                        name: w["name"].as_str().unwrap().into(),
                        weapon_type: w["weapon_type"].as_str().unwrap().into(),
                        range: num(w, "range"),
                        reload: num(w, "reload"),
                        salvo: w["salvo"].as_i64().unwrap() as i32,
                        damage: num(w, "damage"),
                    })
                    .collect(),
                ..Default::default()
            };
            (name.clone(), Arc::new(spec))
        })
        .collect();
    let model = Model::load().unwrap();
    for c in data["cases"].as_array().unwrap() {
        let units: Vec<_> = c["units"]
            .as_array()
            .unwrap()
            .iter()
            .map(|u| SnapUnit {
                reloads: Vec::new(),
                side: u["side"].as_u64().unwrap() as u8,
                spec: specs[u["def"].as_str().unwrap()].clone(),
                hp: num(u, "hp"),
                x: num(u, "x"),
                z: num(u, "z"),
                ..Default::default()
            })
            .collect();
        let bomb = c["bomb"].as_u64().unwrap() as usize;
        for twod in [false, true] {
            for horizon in [300, 600, 900] {
                let snap = FightSnapshot {
                    units: units.clone(),
                    flags: SimFlags { bombs: true, ordered: true, ..SimFlags::from_switches(twod, true, true) },
                };
                // Segmented logs do not carry reliable current reload phases.
                // Busy is deliberately empty: loaded weapons, no invented evidence.
                let v = model.bomb_charge(&snap, bomb, &[], horizon);
                let mut sit = snap.clone();
                sit.units.remove(bomb);
                let fs = model.fates(&sit, horizon, FateMode::AMove);
                let mut loss = [0.0f32; 2];
                for (u, f) in sit.units.iter().zip(&fs) {
                    loss[u.side as usize] += u.spec.cost * f.p_raw;
                }
                let fa = model.fates(&snap, horizon, FateMode::AMove);
                println!(
                    "{}",
                    json!({"id":c["id"], "kind":c["kind"], "twod":twod,
                        "horizon":horizon,"limpet":true, "gain":v.gain,"saved":v.saved,"spent":v.spent,
                        "net":v.net,"p_through":v.p_through,"went_off":v.went_off,
                        "sit_enemy_loss":loss[1],"sit_ally_loss":loss[0],
                        "charge_trade":loss[1]-loss[0]+v.net,
                        "p_raw":fa.iter().map(|f|f.p_raw).collect::<Vec<_>>(),
                        "hp_left":fa.iter().map(|f|f.hp_left).collect::<Vec<_>>(),
                        "death_frame":fa.iter().map(|f|f.death_frame).collect::<Vec<_>>() })
                );
            }
        }
    }
}
