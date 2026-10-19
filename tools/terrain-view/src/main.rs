//! Render the terrain model of a map to PNG, one image per movement tier.
//!
//! usage: terrain-view <map.zktamap> <out-dir> [tier,tier,...] [--scenario <file.json>]
//!
//! Colours: regions get distinct hues, obstacles are dark grey; gate lines
//! are white, chokepoints red squares (size ~ passage width), region
//! centers black dots.
//!
//! With a scenario (dumped by the AI with `ZKGBAI_DUMP_TERRAIN`), regions are
//! muted and the strategic analysis is overlaid: assets as dots (blue ally,
//! red enemy, white neutral), bases as large squares, the best walls in
//! yellow, the cheapest seal in magenta, bridges in cyan, drop targets as
//! orange rings and teleporter exits as green rings.

use std::path::{Path, PathBuf};
use std::time::Instant;

use image::{Rgb, RgbImage};
use zk_terrain::strategy::{Analysis, Asset, Scenario, Side};
use zk_terrain::{CELL, Pos, SpringMapData, Terrain, Tier, TierMap};

const SCALE: u32 = 2;

fn main() {
    let mut args: Vec<String> = std::env::args().collect();
    let scenario = args.iter().position(|a| a == "--scenario").map(|i| {
        let file = args.get(i + 1).cloned().expect("--scenario needs a file");
        args.drain(i..i + 2);
        load_scenario(Path::new(&file))
    });
    if args.len() < 3 {
        eprintln!("usage: terrain-view <map.zktamap> <out-dir> [tier,tier,...] [--scenario <file.json>]");
        std::process::exit(2);
    }
    let data = SpringMapData::load(Path::new(&args[1])).expect("cannot read map data");
    let out = PathBuf::from(&args[2]);
    std::fs::create_dir_all(&out).expect("cannot create output dir");
    let tiers: Vec<Tier> = match args.get(3) {
        Some(list) => list
            .split(',')
            .map(|n| *Tier::ALL.iter().find(|t| t.name() == n).unwrap_or_else(|| panic!("unknown tier {n}")))
            .collect(),
        None => Tier::ALL.to_vec(),
    };

    let start = Instant::now();
    let terrain = Terrain::build(&data, &tiers);
    eprintln!("analysed {} tiers in {:.2}s", tiers.len(), start.elapsed().as_secs_f32());
    for map in terrain.tiers() {
        let path = out.join(format!("{}.png", map.tier.name()));
        let mut img = render(map, scenario.is_some());
        if let Some(sc) = &scenario {
            let s = Scenario { map, ally_base: sc.ally_base, enemy_base: sc.enemy_base, assets: &sc.assets };
            let analysis = Analysis::run(&s);
            overlay(&mut img, map, sc, &analysis);
            report(map, &analysis);
        }
        img.save(&path).expect("cannot write image");
        let linked = map.chokes.iter().filter(|c| c.regions.len() >= 2).count();
        println!(
            "{:<8} passable {:>5.1}%  regions {:>3}  chokepoints {:>3} ({} linking)  -> {}",
            map.tier.name(),
            map.passable_fraction * 100.0,
            map.regions.len(),
            map.chokes.len(),
            linked,
            path.display()
        );
    }
}

struct LoadedScenario {
    ally_base: Pos,
    enemy_base: Pos,
    assets: Vec<Asset>,
}

fn load_scenario(path: &Path) -> LoadedScenario {
    let text = std::fs::read_to_string(path).expect("cannot read scenario");
    let v: serde_json::Value = serde_json::from_str(&text).expect("bad scenario json");
    let pos =
        |v: &serde_json::Value| Pos::new(v[0].as_f64().unwrap_or(0.0) as f32, v[1].as_f64().unwrap_or(0.0) as f32);
    let assets = v["assets"]
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
    LoadedScenario { ally_base: pos(&v["ally_base"]), enemy_base: pos(&v["enemy_base"]), assets }
}

fn report(map: &TierMap, a: &Analysis) {
    let r = |r: Option<usize>| r.map_or("-".to_string(), |r| format!("R{r}"));
    println!("  bases: ally {} enemy {}", r(a.ally_base), r(a.enemy_base));
    for w in a.walls.iter().take(5) {
        println!(
            "  wall    C{:<3} width {:>4.0}  protects {:>5.1} in {} regions",
            w.choke,
            w.width,
            w.protects,
            w.regions.len()
        );
    }
    println!("  seal    {:?} (total width {:.0})", a.seal, a.seal.iter().map(|&c| map.chokes[c].width).sum::<f32>());
    for b in a.bridges.iter().take(5) {
        println!(
            "  bridge  R{}->R{} span {:>4.0}  exposes {:>5.1}  bypasses {} guards  score {:.1}",
            b.from, b.to, b.span, b.enemy_exposed, b.guards_bypassed, b.score
        );
    }
    for d in a.drops.iter().take(3) {
        println!(
            "  drop    R{:<3} enemy value {:>5.1}  ground {}  guards {}",
            d.region,
            d.enemy_value,
            d.ground_distance.map_or("unreachable".into(), |g| format!("{g:.0}")),
            d.guards
        );
    }
    for t in a.teleports.iter().take(3) {
        println!("  teleport R{:<3} gain {:.1}", t.region, t.gain);
    }
}

fn overlay(img: &mut RgbImage, map: &TierMap, sc: &LoadedScenario, a: &Analysis) {
    let px = |p: Pos| ((p.x / CELL * SCALE as f32) as i32, (p.z / CELL * SCALE as f32) as i32);
    for w in a.walls.iter().take(5) {
        let c = &map.chokes[w.choke];
        match c.gate {
            Some([g0, g1]) => thick_line(img, px(g0), px(g1), 2, Rgb([255, 220, 0])),
            None => square(img, px(c.pos), 5, Rgb([255, 220, 0])),
        }
    }
    for &c in &a.seal {
        let p = px(map.chokes[c].pos);
        ring(img, p, 9, Rgb([255, 0, 255]));
        ring(img, p, 10, Rgb([255, 0, 255]));
    }
    for b in a.bridges.iter().take(5) {
        thick_line(img, px(b.ends[0]), px(b.ends[1]), 2, Rgb([0, 255, 255]));
    }
    for d in a.drops.iter().take(3) {
        let p = px(map.regions[d.region].center);
        for r in 12..15 {
            ring(img, p, r, Rgb([255, 140, 0]));
        }
    }
    for t in a.teleports.iter().take(3) {
        let p = px(map.regions[t.region].center);
        for r in 16..19 {
            ring(img, p, r, Rgb([0, 230, 0]));
        }
    }
    for asset in &sc.assets {
        let colour = match asset.side {
            Side::Ally => Rgb([30, 80, 255]),
            Side::Enemy => Rgb([230, 20, 20]),
            Side::Neutral => Rgb([255, 255, 255]),
        };
        square(img, px(asset.pos), 2, colour);
    }
    square(img, px(sc.ally_base), 6, Rgb([0, 40, 200]));
    square(img, px(sc.enemy_base), 6, Rgb([180, 0, 0]));
}

fn ring(img: &mut RgbImage, (x, z): (i32, i32), r: i32, c: Rgb<u8>) {
    for i in 0..(r * 8).max(16) {
        let a = i as f32 / (r * 8).max(16) as f32 * std::f32::consts::TAU;
        put(img, x + (a.cos() * r as f32) as i32, z + (a.sin() * r as f32) as i32, c);
    }
}

fn thick_line(img: &mut RgbImage, a: (i32, i32), b: (i32, i32), t: i32, c: Rgb<u8>) {
    for dz in -t / 2..=t / 2 {
        for dx in -t / 2..=t / 2 {
            line(img, (a.0 + dx, a.1 + dz), (b.0 + dx, b.1 + dz), c);
        }
    }
}

fn region_colour(r: usize) -> Rgb<u8> {
    // Golden-ratio hue walk for well-separated colours.
    let h = (r as f32 * 0.618_034) % 1.0;
    let (s, v) = (0.55, 0.9);
    let i = (h * 6.0) as i32;
    let f = h * 6.0 - i as f32;
    let (p, q, t) = (v * (1.0 - s), v * (1.0 - f * s), v * (1.0 - (1.0 - f) * s));
    let (r, g, b) = match i % 6 {
        0 => (v, t, p),
        1 => (q, v, p),
        2 => (p, v, t),
        3 => (p, q, v),
        4 => (t, p, v),
        _ => (v, p, q),
    };
    Rgb([(r * 255.0) as u8, (g * 255.0) as u8, (b * 255.0) as u8])
}

fn mute(c: Rgb<u8>) -> Rgb<u8> {
    Rgb(c.0.map(|v| (v as u16 * 2 / 5 + 120) as u8))
}

fn render(map: &TierMap, muted: bool) -> RgbImage {
    let (w, h) = (map.width as u32 * SCALE, map.height as u32 * SCALE);
    let mut img = RgbImage::from_pixel(w, h, Rgb([40, 40, 40]));
    for cell in 0..map.width * map.height {
        if let Some(r) = map.region_of_cell(cell) {
            let (cx, cz) = ((cell % map.width) as u32 * SCALE, (cell / map.width) as u32 * SCALE);
            for dz in 0..SCALE {
                for dx in 0..SCALE {
                    let colour = if muted { mute(region_colour(r)) } else { region_colour(r) };
                    img.put_pixel(cx + dx, cz + dz, colour);
                }
            }
        }
    }
    let px = |p: Pos| ((p.x / CELL * SCALE as f32) as i32, (p.z / CELL * SCALE as f32) as i32);
    for c in &map.chokes {
        if let Some([a, b]) = c.gate {
            line(&mut img, px(a), px(b), Rgb([255, 255, 255]));
        }
        let half = ((c.width / CELL * SCALE as f32) / 4.0).clamp(2.0, 10.0) as i32;
        square(&mut img, px(c.pos), half, if c.regions.len() >= 2 { Rgb([220, 0, 0]) } else { Rgb([255, 160, 0]) });
    }
    for r in &map.regions {
        square(&mut img, px(r.center), 2, Rgb([0, 0, 0]));
    }
    img
}

fn put(img: &mut RgbImage, x: i32, z: i32, c: Rgb<u8>) {
    if x >= 0 && z >= 0 && (x as u32) < img.width() && (z as u32) < img.height() {
        img.put_pixel(x as u32, z as u32, c);
    }
}

fn square(img: &mut RgbImage, (x, z): (i32, i32), half: i32, c: Rgb<u8>) {
    for dz in -half..=half {
        for dx in -half..=half {
            put(img, x + dx, z + dz, c);
        }
    }
}

fn line(img: &mut RgbImage, (x0, z0): (i32, i32), (x1, z1): (i32, i32), c: Rgb<u8>) {
    let n = (x1 - x0).abs().max((z1 - z0).abs()).max(1);
    for i in 0..=n {
        let t = i as f32 / n as f32;
        put(img, (x0 as f32 + (x1 - x0) as f32 * t) as i32, (z0 as f32 + (z1 - z0) as f32 * t) as i32, c);
    }
}
