//! Structural openness of maps, per movement tier: how much of a side's
//! metal a narrow front can close off from the enemy's ground forces.
//!
//! usage: openness <manifest.json> [tier,tier,...]
//!
//! The manifest lists maps: `[{"name", "zktamap", "starts": [[x, z], [x, z]],
//! "spots": [[x, z], ...]}]`. For each map, tier and side (each start in turn
//! is ours), the spots are split by ground distance: ours if at most 0.9 of
//! the way to the enemy's start, theirs past the mirror line, neutral
//! between. Every spot is worth 1, so maps compare by shape, not richness.
//!
//! Printed per map, tier and side, and written as JSON lines on stdout
//! (the table goes to stderr):
//!
//! - `hold`: the chokepoints the AI's strategic analysis would hold
//!   (`Analysis::hold`), and the fortification sites it would build at (the
//!   same filter as the AI's `fortifications`);
//! - `cut@λ`: the cheapest way to keep the enemy's ground forces from our
//!   spots when a front costs its hold width in elmos and giving up a spot
//!   costs λ elmos: the share of our spots kept, the front's total width and
//!   chokepoint count, with the front on our side of the map only (as the AI
//!   holds) and anywhere;
//! - `guards`: the stakeouts' read: the first gate (at most
//!   `MAX_HOLD_WIDTH` wide, two regions) on our side that every enemy route
//!   to a spot's region passes; the share of our spots behind any such gate,
//!   and behind the best four;
//! - `detour`: ground distance from the enemy's start to each of our spots
//!   over the straight line, median.

use std::collections::BTreeMap;
use std::path::Path;


use serde_json::{Value, json};
use zk_terrain::lanes;
use zk_terrain::paths::Paths;
use zk_terrain::strategy::{Analysis, Asset, MAX_HOLD_WIDTH, Scenario, Side};
use zk_terrain::{ChokeKind, Pos, SpringMapData, Terrain, Tier, TierMap};

/// Prices of a spot given up, elmos of front (region-graph cuts).
const LAMBDAS: [f64; 3] = [300.0, 600.0, 1200.0];
/// Snap radius for spots and starts on coarse-grid obstacles, elmos.
const SNAP: f32 = 96.0;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        eprintln!("usage: openness <manifest.json> [tier,tier,...]");
        std::process::exit(2);
    }
    let manifest: Value = serde_json::from_str(&std::fs::read_to_string(&args[1]).expect("cannot read manifest"))
        .expect("bad manifest json");
    let tiers: Vec<Tier> = match args.get(2) {
        Some(list) => list
            .split(',')
            .map(|n| *Tier::ALL.iter().find(|t| t.name() == n).unwrap_or_else(|| panic!("unknown tier {n}")))
            .collect(),
        None => vec![Tier::Bot, Tier::Vehicle, Tier::Hover, Tier::Amphib, Tier::Spider],
    };
    let p = |v: &Value| Pos::new(v[0].as_f64().unwrap_or(0.0) as f32, v[1].as_f64().unwrap_or(0.0) as f32);
    eprintln!(
        "{:<26} {:<7} {:>4} {:>4} {:>4} {:>4} | {:>2} {:>15} {:>3} | {:>19} {:>19} {:>19} | {:>9} | {:>6}",
        "map",
        "tier",
        "side",
        "ours",
        "gate",
        "bord",
        "AI",
        "hold front/kept",
        "for",
        "cut@300 kept/front",
        "cut@600 kept/front",
        "cut@1200 kept/front",
        "guard any/4",
        "detour"
    );
    for m in manifest.as_array().expect("manifest must be a list") {
        let name = m["name"].as_str().unwrap_or("?");
        let data = SpringMapData::load(Path::new(m["zktamap"].as_str().expect("zktamap"))).expect("cannot read map");
        let starts: Vec<Pos> = m["starts"].as_array().expect("starts").iter().map(p).collect();
        let spots: Vec<Pos> = m["spots"].as_array().expect("spots").iter().map(p).collect();
        // Metal per second of all spots (1 each where the manifest has none).
        let metal: f64 = (m["spots"].as_array().expect("spots").iter()).map(|s| s[2].as_f64().unwrap_or(1.0)).sum();
        let terrain = Terrain::build(&data, &tiers);
        for map in terrain.tiers() {
            for side in 0..2 {
                let (ours, theirs) = (starts[side], starts[1 - side]);
                let row = analyse(map, ours, theirs, &spots);
                print_row(name, map, side, &row);
                let mut out = row;
                // Usable area in square kilo-elmos, for the map's own metal
                // density (spot metal per usable area, before anything stands).
                let usable = f64::from(map.passable_fraction) * (map.width * map.height) as f64 * 256.0 / 1e6;
                out["metal"] = json!(metal);
                out["usable_km2"] = json!(usable);
                out["map"] = json!(name);
                out["tier"] = json!(map.tier.name());
                out["side"] = json!(side);
                println!("{out}");
            }
        }
    }
}

fn analyse(map: &TierMap, ours: Pos, theirs: Pos, spots: &[Pos]) -> Value {
    let gates = map.chokes.iter().filter(|c| c.kind == ChokeKind::Gate).count();
    let borders = map.chokes.len() - gates;
    let from_ours = Paths::new(map, snap(map, ours), |_| false);
    let from_theirs = Paths::new(map, snap(map, theirs), |_| false);
    let mut assets = Vec::new();
    let mut ally_spots = Vec::new();
    let mut detours = Vec::new();
    for &s in spots {
        let s = snap(map, s);
        let (Some(da), Some(de)) = (from_ours.distance_to(s), from_theirs.distance_to(s)) else { continue };
        let side = if da <= 0.9 * de {
            Side::Ally
        } else if de <= 0.9 * da {
            Side::Enemy
        } else {
            Side::Neutral
        };
        if side == Side::Ally {
            ally_spots.push(s);
            let crow = s.distance(theirs).max(1.0);
            detours.push(de / crow);
        }
        assets.push(Asset { pos: s, value: 1.0, side });
    }
    detours.sort_by(f32::total_cmp);
    let detour = detours.get(detours.len() / 2).copied().unwrap_or(f32::NAN);

    let scenario = Scenario { map, ally_base: ours, enemy_base: theirs, assets: &assets };
    let a = Analysis::run(&scenario);
    let (Some(base), Some(enemy)) = (a.ally_base, a.enemy_base) else {
        return json!({"ok": false, "why": "a base off the tier's ground", "ours": ally_spots.len()});
    };

    // Our spots per region.
    let mut per_region: BTreeMap<usize, f64> = BTreeMap::new();
    for &s in &ally_spots {
        if let Some(r) = map.region_near(s, SNAP) {
            *per_region.entry(r).or_default() += 1.0;
        }
    }
    let total = ally_spots.len() as f64;

    // The AI's hold and fortification sites (as `fortifications` filters).
    let hold_front: f32 = (a.hold.iter())
        .map(|&c| a.home_side(map, c).map_or(map.chokes[c].width, |h| map.chokes[c].hold_width(h)))
        .sum();
    let fortify: Vec<usize> = (a.hold.iter().copied())
        .filter(|&c| a.choke_distance(map, c, false) < a.dist_from_enemy[base] && a.home_side(map, c).is_some())
        .collect();
    let hold_kept = kept_behind(map, enemy, &a.hold, &per_region) / total.max(1.0);

    let cuts: Vec<Value> = LAMBDAS
        .iter()
        .map(|&l| {
            let mine = front_cut(map, &a, enemy, &per_region, l, true);
            let any = front_cut(map, &a, enemy, &per_region, l, false);
            json!({"lambda": l, "ours": mine, "anywhere": any})
        })
        .collect();

    // The stakeouts' guards.
    let is_gate = |c: usize| map.chokes[c].width <= MAX_HOLD_WIDTH && map.chokes[c].regions.len() == 2;
    let mut covers: BTreeMap<usize, f64> = BTreeMap::new();
    let mut guarded = 0.0;
    for (&r, &n) in &per_region {
        if r == enemy {
            continue;
        }
        let first = (a.guards_vs_enemy[r].iter().copied())
            .filter(|&c| is_gate(c) && a.choke_on_our_side(map, c))
            .min_by(|&x, &y| a.choke_distance(map, x, false).total_cmp(&a.choke_distance(map, y, false)));
        if let Some(c) = first {
            guarded += n;
            *covers.entry(c).or_default() += n;
        }
    }
    let mut best: Vec<(usize, f64)> = covers.into_iter().collect();
    best.sort_by(|x, y| y.1.total_cmp(&x.1));
    let top4: f64 = best.iter().take(4).map(|x| x.1).sum();
    let gates_top: Vec<Value> = best
        .iter()
        .take(4)
        .map(|&(c, n)| json!({"choke": c, "width": map.chokes[c].width.round(), "spots": n}))
        .collect();

    // Lanes on the grid itself (see `zk_terrain::lanes`).
    let lanes_json = match lanes::analyse(map, ours, theirs, spots, &lanes::LaneParams::for_tier(map.tier)) {
        Some(l) => json!({
            "openness": l.openness,
            "perimeter": l.perimeter.openness,
            "closure": l.closure,
            "kept": l.perimeter.kept.iter().filter(|&&k| k).count(),
            "ours": l.ours.iter().filter(|&&o| o).count(),
            "usable_km2": l.usable_km2,
            "profile": l.profile.iter().map(|c| json!({"depth": c.depth, "width": c.width, "available": c.available.round()})).collect::<Vec<_>>(),
            "fronts": l.perimeter.fronts.iter().map(|f| json!({
                "x": f.center.x.round(), "z": f.center.z.round(), "width": f.width.round(), "span": f.span.round(),
                "sites": f.sites.iter().map(|s| [s.x.round(), s.z.round()]).collect::<Vec<_>>(),
            })).collect::<Vec<_>>(),
        }),
        None => json!(null),
    };

    json!({
        "ok": true,
        "passable": map.passable_fraction,
        "regions": map.regions.len(),
        "gates": gates,
        "borders": borders,
        "ours": ally_spots.len(),
        "hold": a.hold,
        "hold_front": hold_front.round(),
        "hold_kept": hold_kept,
        "fortify": fortify,
        "cuts": cuts,
        "guarded": guarded / total.max(1.0),
        "guarded_top4": top4 / total.max(1.0),
        "guard_gates": gates_top,
        "detour": detour,
        "lanes": lanes_json,
    })
}

/// Nearest passable ground within `SNAP`, else `p`.
fn snap(map: &TierMap, p: Pos) -> Pos {
    if map.is_passable(p) {
        return p;
    }
    let mut best = p;
    let mut bd = f32::INFINITY;
    let step = 16.0;
    let n = (SNAP / step) as i32;
    for dx in -n..=n {
        for dz in -n..=n {
            let q = Pos::new(p.x + dx as f32 * step, p.z + dz as f32 * step);
            let d = q.distance(p);
            if d < bd && d <= SNAP && map.is_passable(q) {
                bd = d;
                best = q;
            }
        }
    }
    best
}

/// Spots in regions the enemy cannot reach from `enemy` with `closed` shut.
fn kept_behind(map: &TierMap, enemy: usize, closed: &[usize], per_region: &BTreeMap<usize, f64>) -> f64 {
    let reach = reachable(map, enemy, |c| closed.contains(&c));
    per_region.iter().filter(|(r, _)| !reach[**r]).map(|(_, n)| n).sum()
}

fn reachable(map: &TierMap, from: usize, closed: impl Fn(usize) -> bool) -> Vec<bool> {
    let mut seen = vec![false; map.regions.len()];
    let mut stack = vec![from];
    seen[from] = true;
    while let Some(r) = stack.pop() {
        for &c in &map.regions[r].chokes {
            if closed(c) {
                continue;
            }
            for &o in &map.chokes[c].regions {
                if !seen[o] {
                    seen[o] = true;
                    stack.push(o);
                }
            }
        }
    }
    seen
}

/// The cheapest front keeping the enemy (at region `enemy`) from our spots:
/// a chokepoint costs its hold width against crossings into the region
/// behind it, a spot given up costs `lambda`. With `ours_only`, only
/// chokepoints on our side of the map may be held.
fn front_cut(
    map: &TierMap,
    a: &Analysis,
    enemy: usize,
    per_region: &BTreeMap<usize, f64>,
    lambda: f64,
    ours_only: bool,
) -> Value {
    let n = map.regions.len();
    let sink = n;
    let mut net = Flow::new(n + 1);
    for (c, choke) in map.chokes.iter().enumerate() {
        let allowed = !ours_only || a.choke_on_our_side(map, c);
        let cap = |into: usize| if allowed { f64::from(choke.hold_width(into)) } else { f64::INFINITY };
        for (i, &x) in choke.regions.iter().enumerate() {
            for &y in &choke.regions[i + 1..] {
                net.add(x, y, cap(y), Some(c));
                net.add(y, x, cap(x), Some(c));
            }
        }
    }
    let total: f64 = per_region.values().sum();
    for (&r, &v) in per_region {
        if r != enemy {
            net.add(r, sink, lambda * v, None);
        }
    }
    let (flow, cut) = net.min_cut(enemy, sink);
    let chokes: Vec<usize> = {
        let mut v: Vec<usize> = cut.iter().filter_map(|&e| net.edges[e].choke).collect();
        v.sort_unstable();
        v.dedup();
        v
    };
    let front: f64 = cut.iter().filter(|&&e| net.edges[e].choke.is_some()).map(|&e| net.edges[e].cap).sum();
    let given_up: f64 = cut.iter().filter(|&&e| net.edges[e].choke.is_none()).map(|&e| net.edges[e].cap).sum();
    let kept = if total > 0.0 { 1.0 - given_up / lambda / total } else { 0.0 };
    json!({"kept": kept, "front": front.round(), "chokes": chokes, "cost": flow.round()})
}

struct Edge {
    to: usize,
    cap: f64,
    flow: f64,
    choke: Option<usize>,
}

/// Edmonds-Karp max flow on a small graph.
struct Flow {
    adj: Vec<Vec<usize>>,
    edges: Vec<Edge>,
}

impl Flow {
    fn new(n: usize) -> Self {
        Flow { adj: vec![Vec::new(); n], edges: Vec::new() }
    }

    fn add(&mut self, a: usize, b: usize, cap: f64, choke: Option<usize>) {
        self.adj[a].push(self.edges.len());
        self.edges.push(Edge { to: b, cap, flow: 0.0, choke });
        self.adj[b].push(self.edges.len());
        self.edges.push(Edge { to: a, cap: 0.0, flow: 0.0, choke: None });
    }

    fn residual(&self, e: usize) -> f64 {
        self.edges[e].cap - self.edges[e].flow
    }

    /// Max flow value and the saturated edges leaving the source side.
    fn min_cut(&mut self, s: usize, t: usize) -> (f64, Vec<usize>) {
        let mut total = 0.0;
        loop {
            let mut prev: Vec<Option<usize>> = vec![None; self.adj.len()];
            let mut seen = vec![false; self.adj.len()];
            seen[s] = true;
            let mut queue = std::collections::VecDeque::from([s]);
            while let Some(u) = queue.pop_front() {
                for &e in &self.adj[u] {
                    let v = self.edges[e].to;
                    if !seen[v] && self.residual(e) > 1e-9 {
                        seen[v] = true;
                        prev[v] = Some(e);
                        queue.push_back(v);
                    }
                }
            }
            if !seen[t] {
                let cut = (0..self.edges.len())
                    .step_by(2)
                    .filter(|&e| seen[self.edges[e ^ 1].to] && !seen[self.edges[e].to])
                    .collect();
                return (total, cut);
            }
            let mut push = f64::INFINITY;
            let mut v = t;
            while let Some(e) = prev[v] {
                push = push.min(self.residual(e));
                v = self.edges[e ^ 1].to;
            }
            if !push.is_finite() {
                return (f64::INFINITY, Vec::new());
            }
            let mut v = t;
            while let Some(e) = prev[v] {
                self.edges[e].flow += push;
                self.edges[e ^ 1].flow -= push;
                v = self.edges[e ^ 1].to;
            }
            total += push;
        }
    }
}

fn print_row(name: &str, map: &TierMap, side: usize, r: &Value) {
    let short: String = name.chars().take(26).collect();
    if !r["ok"].as_bool().unwrap_or(false) {
        eprintln!("{short:<26} {:<7} {side:>4}  -- {}", map.tier.name(), r["why"].as_str().unwrap_or(""));
        return;
    }
    let cut = |i: usize| {
        let c = &r["cuts"][i]["ours"];
        format!("{:>3.0}% {:>5.0}w {:>2}c", c["kept"].as_f64().unwrap_or(0.0) * 100.0, c["front"].as_f64().unwrap_or(0.0), c["chokes"].as_array().map_or(0, Vec::len))
    };
    eprintln!(
        "{short:<26} {:<7} {side:>4} {:>4} {:>4} {:>4} | {:>2} {:>6.0}w {:>5.0}% {:>3} | {:>19} {:>19} {:>19} | {:>3.0}% {:>3.0}% | {:>6.2}",
        map.tier.name(),
        r["ours"],
        r["gates"],
        r["borders"],
        r["hold"].as_array().map_or(0, Vec::len),
        r["hold_front"].as_f64().unwrap_or(0.0),
        r["hold_kept"].as_f64().unwrap_or(0.0) * 100.0,
        r["fortify"].as_array().map_or(0, Vec::len),
        cut(0),
        cut(1),
        cut(2),
        r["guarded"].as_f64().unwrap_or(0.0) * 100.0,
        r["guarded_top4"].as_f64().unwrap_or(0.0) * 100.0,
        r["detour"].as_f64().unwrap_or(f64::NAN),
    );
}
