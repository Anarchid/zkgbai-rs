//! What it takes to seize a place: the cheapest force of a factory's units
//! that the fight predictor says takes on what stands there, and the
//! constructor that then takes the spots (see `military::fightsim::plan`).
//!
//! usage: force_plan <specs.tsv> --against "3 turretlaser[, 1 turretmissile]"
//!          (--factory factoryveh | --with vehsupport,vehraid,...)
//!          [--spread 150] [--win 0.8] [--loss 0.5] [--metal 1500] [--units 12]
//!          [--sim plain|focus|2d] [--goal seize|fight] [--horizon 90] [--moves amove|kiting]
//!
//! goal seize (the default when everything there is a building): every armed
//! defender dies within the horizon, by the mechanics fight with ZK's unit
//! AI moving both sides; goal fight: P(win) of the fight model (a raid met).
//!
//! specs.tsv comes from tools/forceplan/dump_specs.lua (ZK's unit files).
use std::collections::BTreeMap;
use std::sync::Arc;

use zkgbai::military::fightsim::plan::{Caps, Moves, Plan, cheapest_force, cheapest_seizure};
use zkgbai::military::fightsim::{Model, SimFlags, UnitSpec, WeaponSpec};

struct Row {
    spec: Arc<UnitSpec>,
    human: String,
    builds: Vec<String>,
}

fn load(path: &str) -> BTreeMap<String, Row> {
    let text = std::fs::read_to_string(path).unwrap_or_else(|e| panic!("cannot read {path}: {e}"));
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
        out.insert(f[0].to_string(), Row { spec: Arc::new(spec), human: f[10].to_string(), builds });
    }
    out
}

fn main() {
    let mut args: Vec<String> = std::env::args().collect();
    let mut flag = |name: &str| {
        args.iter().position(|a| a == name).map(|i| {
            let v = args[i + 1].clone();
            args.drain(i..i + 2);
            v
        })
    };
    let against_s = flag("--against");
    let factory = flag("--factory");
    let with = flag("--with");
    let spread: f32 = flag("--spread").and_then(|v| v.parse().ok()).unwrap_or(150.0);
    let d = Caps::default();
    let caps = Caps {
        win: flag("--win").and_then(|v| v.parse().ok()).unwrap_or(-1.0),
        loss: flag("--loss").and_then(|v| v.parse().ok()).unwrap_or(d.loss),
        metal: flag("--metal").and_then(|v| v.parse().ok()).unwrap_or(1500.0),
        units: flag("--units").and_then(|v| v.parse().ok()).unwrap_or(12),
    };
    let goal = flag("--goal");
    let moves = match flag("--moves").as_deref() {
        Some("kiting") => Moves::Kiting,
        _ => Moves::AMove,
    };
    let horizon: f32 = flag("--horizon").and_then(|v| v.parse().ok()).unwrap_or(90.0);
    let flags = match flag("--sim").as_deref() {
        Some("plain") => SimFlags::default(),
        Some("2d") => SimFlags::from_switches(true, false),
        _ => SimFlags::from_switches(false, true),
    };
    let (Some(path), Some(against_s)) = (args.get(1).cloned(), against_s) else {
        eprintln!("usage: force_plan <specs.tsv> --against \"3 turretlaser\" (--factory factoryveh | --with a,b)");
        std::process::exit(2);
    };
    let db = load(&path);
    let get = |n: &str| db.get(n).unwrap_or_else(|| panic!("no unit def {n} in {path}"));
    // What stands there, on a ring `spread` apart.
    let mut what: Vec<Arc<UnitSpec>> = Vec::new();
    for part in against_s.split(',') {
        let mut it = part.split_whitespace();
        let (n, name) = match (it.next(), it.next()) {
            (Some(a), Some(b)) => (a.parse::<usize>().unwrap_or(1), b),
            (Some(a), None) => (1, a),
            _ => continue,
        };
        for _ in 0..n {
            what.push(get(name).spec.clone());
        }
    }
    let k = what.len().max(1) as f32;
    let r = if what.len() < 2 { 0.0 } else { spread / (2.0 * (std::f32::consts::PI / k).sin()) };
    let against: Vec<(Arc<UnitSpec>, f32, f32)> = (what.iter().enumerate())
        .map(|(i, s)| {
            let a = 2.0 * std::f32::consts::PI * i as f32 / k;
            (s.clone(), r * a.cos(), r * a.sin())
        })
        .collect();
    // What we may field: a factory's armed ground units, or a list.
    let (from, names): (String, Vec<String>) = match (&factory, &with) {
        (Some(f), _) => (get(f).human.clone(), get(f).builds.clone()),
        (None, Some(w)) => ("the list".into(), w.split(',').map(String::from).collect()),
        _ => {
            eprintln!("give --factory or --with");
            std::process::exit(2);
        }
    };
    let options: Vec<(String, Arc<UnitSpec>)> = (names.iter())
        .filter_map(|n| db.get(n).map(|r| (n.clone(), r.spec.clone())))
        .filter(|(_, s)| s.mobile && !s.air && !s.builder && !s.weapons.is_empty())
        .collect();
    let builder = names.iter().find(|n| db.get(*n).is_some_and(|r| r.spec.builder && r.spec.mobile));
    let model = Model::load().expect("predictor tables");
    let human = |n: &str| db.get(n).map_or(n.to_string(), |r| r.human.clone());
    let cost: f32 = what.iter().map(|s| s.cost).sum();
    let mut kinds: BTreeMap<String, usize> = BTreeMap::new();
    for s in &what {
        *kinds.entry(s.def.clone()).or_default() += 1;
    }
    let desc: Vec<String> = kinds.iter().map(|(n, c)| format!("{c}x {}", human(n))).collect();
    println!("Against: {} ({cost:.0} metal, {spread:.0} elmos apart)", desc.join(", "));
    println!("From {from}: {}", options.iter().map(|o| human(&o.0)).collect::<Vec<_>>().join(", "));
    println!(
        "Losing at most {:.0}% of the force, within {:.0} metal / {} units\n",
        caps.loss * 100.0,
        caps.metal,
        caps.units
    );
    let specs: Vec<Arc<UnitSpec>> = options.iter().map(|o| o.1.clone()).collect();
    let seize = match goal.as_deref() {
        Some("fight") => false,
        Some("seize") => true,
        _ => what.iter().all(|s| !s.mobile),
    };
    let frames = (horizon * 30.0) as i32;
    let caps = Caps {
        win: if caps.win >= 0.0 {
            caps.win
        } else if seize {
            1.0
        } else {
            d.win
        },
        ..caps
    };
    let plan_with = |opts: &[Arc<UnitSpec>]| -> Plan {
        if seize {
            cheapest_seizure(&model, &against, opts, flags, &caps, frames, moves)
        } else {
            cheapest_force(&model, &against, opts, flags, &caps)
        }
    };
    if seize {
        println!(
            "Goal: seize: {:.0}% of the armed defenders dead within {horizon:.0} s ({:?} moves), losing at most {:.0}% of the force",
            caps.win * 100.0,
            moves,
            caps.loss * 100.0
        );
    } else {
        println!("Goal: fight: P(win) >= {:.2} by the fight model", caps.win);
    }
    let crit = if seize { "defenders dead" } else { "P(win)" };
    let show = |label: &str, pick: &[usize], plan: &Plan, names: &[String]| {
        let mut c: BTreeMap<usize, usize> = BTreeMap::new();
        for &k in pick {
            *c.entry(k).or_default() += 1;
        }
        let force: Vec<String> = c.iter().map(|(k, n)| format!("{n}x {}", human(&names[*k]))).collect();
        println!(
            "{label:<16} {:<30} {:>5.0} metal  {crit} {:.2}{}  we lose {:>3.0}%  they lose {:>3.0}%  {}",
            if force.is_empty() { "nothing".into() } else { force.join(" + ") },
            plan.cost,
            plan.p_win,
            if plan.secs > 0.0 { format!(" in {:.0} s", plan.secs) } else { String::new() },
            plan.loss * 100.0,
            plan.killed * 100.0,
            if plan.met { "" } else { "(not enough within the caps)" }
        );
    };
    let names_v: Vec<String> = options.iter().map(|o| o.0.clone()).collect();
    let best = plan_with(&specs);
    show("Cheapest mix:", &best.pick, &best, &names_v);
    for (i, o) in options.iter().enumerate() {
        let one = plan_with(std::slice::from_ref(&specs[i]));
        let pick: Vec<usize> = one.pick.iter().map(|_| i).collect();
        show(&format!("{} only:", human(&o.0)), &pick, &one, &names_v);
    }
    if let Some(b) = builder {
        println!("\nThen a {} ({}, {:.0} metal) to take the spots.", human(b), b, get(b).spec.cost);
    }
}
