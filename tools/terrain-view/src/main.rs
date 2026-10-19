//! Render the terrain model of a map to PNG, one image per movement tier.
//!
//! usage: terrain-view <map.zktamap> <out-dir> [tier,tier,...]
//!
//! Colours: regions get distinct hues, obstacles are dark grey; gate lines
//! are white, chokepoints red squares (size ~ passage width), region
//! centers black dots.

use std::path::{Path, PathBuf};
use std::time::Instant;

use image::{Rgb, RgbImage};
use zk_terrain::{CELL, Pos, SpringMapData, Terrain, Tier, TierMap};

const SCALE: u32 = 2;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        eprintln!("usage: terrain-view <map.zktamap> <out-dir> [tier,tier,...]");
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
        render(map).save(&path).expect("cannot write image");
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

fn render(map: &TierMap) -> RgbImage {
    let (w, h) = (map.width as u32 * SCALE, map.height as u32 * SCALE);
    let mut img = RgbImage::from_pixel(w, h, Rgb([40, 40, 40]));
    for cell in 0..map.width * map.height {
        if let Some(r) = map.region_of_cell(cell) {
            let (cx, cz) = ((cell % map.width) as u32 * SCALE, (cell / map.width) as u32 * SCALE);
            for dz in 0..SCALE {
                for dx in 0..SCALE {
                    img.put_pixel(cx + dx, cz + dz, region_colour(r));
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
