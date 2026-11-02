//! Conquest objectives of a map (see `zk_terrain::conquest`), from a terrain
//! dump: the sites, the spots they take, and the denial per enemy tier.
//!
//! usage: conquest_atlas <map.zktamap> <scenario.json> [range=620] [sites=2] [--png out.png]
//!        [--eval "x,z x,z"]   (value these sites instead of searching)
//!        [--lattice elmos] [--per-tier n] [--shortlist n] [--band lo,hi]   (search variants)
//!        [--json out.json]    (objectives, shortlists, band shares, spots, bases)
//!
//! The scenario is the AI's dump (`ZKGBAI_DUMP_TERRAIN`); its spot sides are
//! as the AI saw them at the dump's frame, so dump early (frame 1800).
use std::path::Path;

use image::{Rgb, RgbImage};
use zk_terrain::conquest::{Params, covered, evaluate, objectives, shares, shortlists};
use zk_terrain::strategy::{Asset, Side};
use zk_terrain::{CELL, Pos, SpringMapData, Terrain, Tier};

fn main() {
    let mut args: Vec<String> = std::env::args().collect();
    let mut flag = |name: &str| {
        args.iter().position(|a| a == name).map(|i| {
            let f = args[i + 1].clone();
            args.drain(i..i + 2);
            f
        })
    };
    let png = flag("--png");
    let json_out = flag("--json");
    let lattice: f32 = flag("--lattice").and_then(|s| s.parse().ok()).unwrap_or(0.0);
    let per_tier: usize = flag("--per-tier").and_then(|s| s.parse().ok()).unwrap_or(1);
    let short: Option<usize> = flag("--shortlist").and_then(|s| s.parse().ok());
    let band: Option<[f32; 2]> = flag("--band").and_then(|s| {
        let v: Vec<f32> = s.split(',').filter_map(|x| x.parse().ok()).collect();
        (v.len() == 2).then(|| [v[0], v[1]])
    });
    let eval: Option<Vec<Pos>> = flag("--eval").map(|s| {
        s.split_whitespace()
            .map(|xz| {
                let mut it = xz.split(',').map(|v| v.parse::<f32>().expect("x,z"));
                Pos::new(it.next().unwrap(), it.next().unwrap())
            })
            .collect()
    });
    if args.len() < 3 {
        eprintln!("usage: conquest_atlas <map.zktamap> <scenario.json> [range] [sites] [--png out.png]");
        std::process::exit(2);
    }
    let data = SpringMapData::load(Path::new(&args[1])).expect("cannot read map data");
    let v: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&args[2]).expect("scenario")).expect("bad scenario json");
    let range: f32 = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(620.0);
    let max_sites: usize = args.get(4).and_then(|s| s.parse().ok()).unwrap_or(2);
    let p2 = |v: &serde_json::Value| Pos::new(v[0].as_f64().unwrap_or(0.0) as f32, v[1].as_f64().unwrap_or(0.0) as f32);
    let (ally, enemy) = (p2(&v["ally_base"]), p2(&v["enemy_base"]));
    let assets: Vec<Asset> = v["assets"]
        .as_array()
        .map(|a| {
            a.iter()
                .map(|s| Asset {
                    pos: Pos::new(s["x"].as_f64().unwrap_or(0.0) as f32, s["z"].as_f64().unwrap_or(0.0) as f32),
                    value: s["value"].as_f64().unwrap_or(1.0) as f32,
                    side: match s["side"].as_str() {
                        Some("ally") => Side::Ally,
                        Some("enemy") => Side::Enemy,
                        _ => Side::Neutral,
                    },
                })
                .collect()
        })
        .unwrap_or_default();
    let tiers = [Tier::Bot, Tier::Vehicle, Tier::Hover, Tier::Spider, Tier::Amphib];
    let t0 = std::time::Instant::now();
    let terrain = Terrain::build(&data, &tiers);
    let built = t0.elapsed();
    let maps: Vec<_> = tiers.iter().filter_map(|&t| terrain.tier(t)).collect();
    let walk = terrain.tier(Tier::Bot).expect("bot tier");
    let mut p = Params { range, max_sites, lattice, per_tier, ..Params::default() };
    if let Some(n) = short {
        p.shortlist = n;
    }
    if let Some(b) = band {
        p.band = b;
    }
    let t1 = std::time::Instant::now();
    let found = match &eval {
        Some(sites) => vec![evaluate(&maps, walk, ally, &[enemy], &assets, sites, &p)],
        None => objectives(&maps, walk, ally, &[enemy], &assets, &p),
    };
    println!(
        "{}: terrain {:.0} ms, objectives {:.0} ms, {} found (range {range}, sites <= {max_sites})",
        v["map"].as_str().unwrap_or("?"),
        built.as_secs_f64() * 1e3,
        t1.elapsed().as_secs_f64() * 1e3,
        found.len()
    );
    for (i, o) in found.iter().enumerate() {
        let sites: Vec<String> = o.sites.iter().map(|s| format!("({:.0},{:.0})", s.x, s.z)).collect();
        let spots: Vec<String> = o
            .spots
            .iter()
            .map(|&k| format!("({:.0},{:.0}) {:.2}", assets[k].pos.x, assets[k].pos.z, assets[k].value))
            .collect();
        println!(
            "#{i} design {} score {:.1} income {:.2} walk {:.0} enemy walk {:.0}\n   sites {}\n   spots {}",
            o.design.name(),
            o.score,
            o.income,
            o.walk,
            o.enemy_walk,
            sites.join(" "),
            spots.join(", ")
        );
        for d in &o.denial {
            let closed = maps.iter().find(|m| m.tier == d.tier).map(|m| covered(m, &o.sites, range).len()).unwrap_or(0);
            println!(
                "   denial {:<7} {:>6.2} metal/s (own spots {:>5.2}, sealed {}/{})  gates covered {closed}",
                d.tier.name(),
                d.worth,
                d.own,
                d.sealed,
                o.spots.len()
            );
        }
    }
    if let Some(out) = &json_out {
        let pj = |q: Pos| serde_json::json!([q.x.round(), q.z.round()]);
        let objs: Vec<serde_json::Value> = found
            .iter()
            .map(|o| {
                serde_json::json!({
                    "design": o.design.name(), "score": o.score, "income": o.income,
                    "walk": o.walk, "enemy_walk": o.enemy_walk,
                    "sites": o.sites.iter().map(|&q| pj(q)).collect::<Vec<_>>(),
                    "spots": o.spots,
                    "denial": o.denial.iter().map(|d| serde_json::json!({
                        "tier": d.tier.name(), "worth": d.worth, "own": d.own, "sealed": d.sealed})).collect::<Vec<_>>(),
                })
            })
            .collect();
        let lists: serde_json::Map<String, serde_json::Value> = shortlists(&maps, walk, ally, &[enemy], &assets, &p)
            .into_iter()
            .map(|(t, l)| {
                (
                    t.name().to_string(),
                    serde_json::json!(l.iter().map(|(v, q)| [q.x.round(), q.z.round(), *v]).collect::<Vec<_>>()),
                )
            })
            .collect();
        let step = 64.0;
        let (nx, nz, sh) = shares(walk, ally, &[enemy], step);
        let out_v = serde_json::json!({
            "map": v["map"], "size": [data.map_width * 8, data.map_height * 8],
            "ally": pj(ally), "enemy": pj(enemy), "range": range,
            "params": {"lattice": p.lattice, "per_tier": p.per_tier, "shortlist": p.shortlist, "band": p.band},
            "assets": assets.iter().map(|a| serde_json::json!({"x": a.pos.x, "z": a.pos.z, "value": a.value,
                "side": match a.side { Side::Ally => "ally", Side::Enemy => "enemy", Side::Neutral => "neutral" }})).collect::<Vec<_>>(),
            "objectives": objs, "shortlists": lists,
            "shares": {"step": step, "nx": nx, "nz": nz,
                       "v": sh.iter().map(|s| s.map(|x| (x * 1000.0).round() / 1000.0)).collect::<Vec<_>>()},
            "ms": t1.elapsed().as_secs_f64() * 1e3,
        });
        std::fs::write(out, serde_json::to_string(&out_v).expect("json")).expect("cannot write json");
        println!("wrote {out}");
    }
    if let Some(out) = png {
        let (w, h) = (data.map_width * 8 / 16, data.map_height * 8 / 16);
        let mut img = RgbImage::new(w, h);
        let px = |p: Pos| ((p.x / CELL) as i64, (p.z / CELL) as i64);
        for z in 0..h {
            for x in 0..w {
                let p = Pos::new((x as f32 + 0.5) * CELL, (z as f32 + 0.5) * CELL);
                let c = match (walk.is_passable(p), terrain.tier(Tier::Vehicle).is_some_and(|m| m.is_passable(p))) {
                    (true, true) => Rgb([200, 200, 190]),
                    (true, false) => Rgb([150, 140, 120]),
                    _ => Rgb([50, 50, 50]),
                };
                img.put_pixel(x, z, c);
            }
        }
        let disc = |img: &mut RgbImage, c: Pos, r: f32, col: Rgb<u8>, filled: bool| {
            let (cx, cz) = px(c);
            let rr = (r / CELL) as i64;
            for dz in -rr..=rr {
                for dx in -rr..=rr {
                    let d2 = dx * dx + dz * dz;
                    let on = if filled { d2 <= rr * rr } else { d2 <= rr * rr && d2 >= (rr - 1) * (rr - 1) };
                    let (x, z) = (cx + dx, cz + dz);
                    if on && x >= 0 && z >= 0 && (x as u32) < w && (z as u32) < h {
                        img.put_pixel(x as u32, z as u32, col);
                    }
                }
            }
        };
        if let Some(vm) = terrain.tier(Tier::Vehicle) {
            for c in &vm.chokes {
                let col = Rgb([255, 120, 0]);
                if let Some([a, b]) = c.gate {
                    for k in 0..=20 {
                        let t = k as f32 / 20.0;
                        disc(&mut img, Pos::new(a.x + (b.x - a.x) * t, a.z + (b.z - a.z) * t), 16.0, col, true);
                    }
                }
            }
        }
        for a in &assets {
            let col = match a.side {
                Side::Ally => Rgb([40, 80, 255]),
                Side::Enemy => Rgb([230, 30, 30]),
                Side::Neutral => Rgb([255, 255, 255]),
            };
            disc(&mut img, a.pos, 24.0 + 12.0 * a.value, col, true);
        }
        disc(&mut img, ally, 80.0, Rgb([0, 0, 200]), true);
        disc(&mut img, enemy, 80.0, Rgb([200, 0, 0]), true);
        for (i, o) in found.iter().enumerate().take(3) {
            let col = [Rgb([0, 200, 0]), Rgb([0, 200, 200]), Rgb([200, 0, 200])][i];
            for &s in &o.sites {
                disc(&mut img, s, range, col, false);
                disc(&mut img, s, 40.0, col, true);
            }
        }
        img.save(&out).expect("cannot write png");
        println!("wrote {out}");
    }
}
