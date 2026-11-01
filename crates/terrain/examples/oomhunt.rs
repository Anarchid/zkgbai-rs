//! Run the terrain analysis on a .zktamap one tier at a time, as the AI's
//! re-analysis does (`Terrain::build_with` with Zero-K's classes), printing
//! each tier before it starts so a runaway names its tier.
//!
//! usage: oomhunt <map.zktamap> [tier,tier,...]

use std::io::Write;
use std::path::Path;
use std::time::Instant;

use zk_terrain::{SpringMapData, Terrain, Tier};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let data = SpringMapData::load(Path::new(&args[1])).expect("cannot read map data");
    let all = [Tier::Spider, Tier::Bot, Tier::Vehicle, Tier::Hover, Tier::Ship, Tier::Amphib];
    let tiers: Vec<Tier> = match args.get(2) {
        Some(list) => all.into_iter().filter(|t| list.split(',').any(|n| n == t.name())).collect(),
        None => all.to_vec(),
    };
    for tier in tiers {
        print!("{:<8} ", tier.name());
        std::io::stdout().flush().ok();
        let start = Instant::now();
        let terrain = Terrain::build(&data, &[tier]);
        let map = terrain.tier(tier).expect("tier built");
        println!(
            "{:>7.0} ms  {:>3} regions  {:>3} chokes",
            start.elapsed().as_secs_f64() * 1e3,
            map.regions.len(),
            map.chokes.len()
        );
    }
}
