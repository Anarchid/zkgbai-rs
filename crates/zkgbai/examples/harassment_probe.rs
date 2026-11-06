use serde_json::{Value, json};
use std::{collections::HashMap, sync::Arc};
use zkgbai::military::fightsim::{
    ExitTrigger, FightSnapshot, HarassOrder, Model, ShotLead, SimFlags, SnapUnit, UnitSpec, WeaponSpec,
};

fn f(v: &Value, k: &str) -> f32 {
    v[k].as_f64().unwrap() as f32
}
fn main() {
    let args: Vec<_> = std::env::args().collect();
    let specs: Vec<Value> = serde_json::from_str(&std::fs::read_to_string(&args[1]).unwrap()).unwrap();
    let manifest: Value = serde_json::from_str(&std::fs::read_to_string(&args[2]).unwrap()).unwrap();
    let defs: HashMap<String, Arc<UnitSpec>> = specs
        .iter()
        .map(|s| {
            let spec = UnitSpec {
                def: s["def"].as_str().unwrap().into(),
                cost: f(s, "cost"),
                health: f(s, "health"),
                speed: f(s, "speed"),
                radius: f(s, "radius"),
                mobile: true,
                weapons: s["weapons"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|w| WeaponSpec {
                        name: w["name"].as_str().unwrap().into(),
                        weapon_type: w["weapon_type"].as_str().unwrap().into(),
                        range: f(w, "range"),
                        reload: f(w, "reload"),
                        salvo: f(w, "salvo") as i32,
                        damage: f(w, "damage"),
                        leading: ShotLead::new(
                            w["weapon_type"].as_str().unwrap(),
                            f(w, "projectile_speed"),
                            f(w, "lead_limit"),
                        ),
                    })
                    .collect(),
                ..Default::default()
            };
            (spec.def.clone(), Arc::new(spec))
        })
        .collect();
    let model = Model::load().unwrap();
    for c in manifest["cases"].as_array().unwrap() {
        let mut snap = FightSnapshot { flags: SimFlags::from_switches(true, true, true), ..Default::default() };
        snap.flags.ordered = true;
        for (side, key) in ["A", "B"].iter().enumerate() {
            for u in c[key].as_array().unwrap() {
                let spec = defs[u["name"].as_str().unwrap()].clone();
                snap.units.push(SnapUnit {
                    hp: spec.health * f(u, "hp"),
                    side: side as u8,
                    x: f(u, "x"),
                    z: f(u, "z"),
                    reloads: spec.weapons.iter().map(|w| (w.name.clone(), 0)).collect(),
                    spec,
                    ..Default::default()
                });
            }
        }
        for enemy_kiting in [true, false] {
            for (name, own_kiting, trigger) in [
                ("fight", true, ExitTrigger::Never),
                ("attack", false, ExitTrigger::Never),
                ("immediate", true, ExitTrigger::AtFrame(0)),
                ("fight_shot", true, ExitTrigger::AfterFirstShot(0)),
                ("attack_shot", false, ExitTrigger::AfterFirstShot(0)),
                ("attack_shot60", false, ExitTrigger::AfterFirstShot(60)),
            ] {
                let now = std::time::Instant::now();
                let out = model.harassment(
                    &snap,
                    600,
                    HarassOrder {
                        trigger,
                        own_kiting: true,
                        enemy_kiting,
                        target: (!own_kiting).then_some(1),
                        exit: (c["exit"][0].as_f64().unwrap() as f32, c["exit"][1].as_f64().unwrap() as f32),
                    },
                );
                let micros = now.elapsed().as_micros();
                let mut loss = 0.0;
                let mut kill = 0.0;
                let mut damage_credit = 0.0;
                for (u, r) in snap.units.iter().zip(&out.units) {
                    if u.side == 0 {
                        loss += u.spec.cost * r.p_dead;
                    } else {
                        kill += u.spec.cost * r.p_dead;
                        // Diagnostic partial-damage credit only, not destroyed metal.
                        damage_credit += 0.1 * u.spec.cost * (1.0 - r.p_dead) * (u.hp - r.hp).max(0.0) / u.spec.health;
                    }
                }
                println!(
                    "{}",
                    json!({"case":c["case"],"foe":c["foe"],"count":c["count"],"distance":c["distance"],
                    "policy":name,"enemy_kiting":enemy_kiting,"enemy_fire":if enemy_kiting {"prior"} else {"certain"},"micros":micros,"loss":loss,"kill":kill,
                    "damage_credit":damage_credit,"first_shot":out.first_shot,"exit_frame":out.exit_frame,
                    "shots":out.shots,"reached":out.reached,"clearance":out.clearance,
                    "units":out.units.iter().map(|u| json!({"hp":u.hp,"p_dead":u.p_dead,"death_frame":u.death_frame,
                        "position":u.position,"slow":u.slow,"reloads":u.reloads})).collect::<Vec<_>>() })
                );
            }
        }
    }
}
