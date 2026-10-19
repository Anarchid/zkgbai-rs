//! The terrain model: regions, chokepoints and connectivity for every
//! movement tier (see the `zk-terrain` crate). It is built on a background
//! thread at startup; until it is ready, `Ai::terrain` yields nothing and
//! callers fall back to the metal-spot graph.
//!
//! Once it is ready, a strategic analysis of the game (which chokepoints
//! guard our territory) is refreshed periodically, and the metal-spot
//! graph learns which of its links ground forces travel directly.

use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::time::{Duration, Instant};

use springai::{Callback, UnitDef, Vec3};
use zk_terrain::strategy::{Analysis, Asset, Scenario, Side, ground_distance};
use zk_terrain::{MoveClass, Pos, SpringMapData, Terrain, Tier, TierMap};

use crate::Ai;
use crate::defs::Defs;

pub enum TerrainModel {
    /// Analysis running; the thread sends the model and its compute time.
    Pending {
        rx: Receiver<(Terrain, Duration)>,
    },
    Ready(Terrain),
    Unavailable,
}

/// Constructor whose movement class stands for each tier.
const TIER_UNITS: [(Tier, &str); 6] = [
    (Tier::Spider, "spidercon"),
    (Tier::Bot, "cloakcon"),
    (Tier::Vehicle, "vehcon"),
    (Tier::Hover, "hovercon"),
    (Tier::Ship, "shipcon"),
    (Tier::Amphib, "amphcon"),
];

impl TerrainModel {
    /// Fetch the map data and start the analysis thread.
    pub fn start(cb: Callback, defs: &Defs) -> Self {
        let map = cb.map();
        let (w, h) = (map.width().max(0) as u32, map.height().max(0) as u32);
        let data = SpringMapData {
            map_width: w,
            map_height: h,
            slope_width: w / 2,
            slope_height: h / 2,
            slope_map: map.slope_map(),
            height_map: map.height_map(),
        };
        if data.slope_map.len() != (data.slope_width * data.slope_height) as usize
            || data.height_map.len() != (w * h) as usize
            || data.slope_map.is_empty()
        {
            cb.log("terrain: unexpected map data sizes; terrain analysis disabled");
            return TerrainModel::Unavailable;
        }
        let classes: Vec<(Tier, MoveClass)> = TIER_UNITS
            .iter()
            .map(|&(tier, unit)| (tier, defs.get(unit).and_then(move_class).unwrap_or_else(|| tier.zero_k_class())))
            .collect();

        let (tx, rx) = mpsc::channel();
        // Pure computation: the thread must never call into the engine.
        let spawned = std::thread::Builder::new().name("zkgbai-terrain".into()).spawn(move || {
            let start = Instant::now();
            let terrain = Terrain::build_with(&data, &classes);
            let _ = tx.send((terrain, start.elapsed()));
        });
        match spawned {
            Ok(_) => TerrainModel::Pending { rx },
            Err(e) => {
                cb.log(&format!("terrain: cannot start analysis thread ({e}); terrain analysis disabled"));
                TerrainModel::Unavailable
            }
        }
    }

    /// The model, once the analysis has finished.
    pub fn get(&self) -> Option<&Terrain> {
        match self {
            TerrainModel::Ready(t) => Some(t),
            _ => None,
        }
    }
}

/// The terrain limits of a ground unit type; `None` for aircraft and
/// buildings.
pub fn move_class(def: UnitDef) -> Option<MoveClass> {
    let md = def.move_data()?;
    Some(MoveClass {
        max_slope: md.max_slope(),
        max_water_depth: if md.speed_mod_class() == 2 { f32::INFINITY } else { md.depth() },
        min_water_depth: def.min_water_depth().max(0.0),
        hover: md.speed_mod_class() == 2,
    })
}

/// Movement tier of a ground unit type; `None` for aircraft and buildings.
pub fn tier_of(def: UnitDef) -> Option<Tier> {
    let md = def.move_data()?;
    Some(Tier::classify(md.max_slope(), def.min_water_depth(), def.max_water_depth(), md.speed_mod_class()))
}

/// Engine position to terrain-model position.
pub fn pos(v: Vec3) -> Pos {
    Pos::new(v.x, v.z)
}

impl Ai {
    /// The terrain model, if its analysis has finished.
    pub fn terrain(&self) -> Option<&Terrain> {
        self.terrain.get()
    }

    /// Pick up the finished analysis; keep the link classification current.
    pub fn terrain_update(&mut self) {
        if self.terrain().is_some() && self.graph.ground_links.len() != self.graph.links.len() {
            self.terrain_classify_links();
        }
        let TerrainModel::Pending { rx } = &self.terrain else { return };
        match rx.try_recv() {
            Ok((terrain, compute)) => {
                self.terrain = TerrainModel::Ready(terrain);
                self.log(&format!("terrain: analysis took {:.0} ms", compute.as_secs_f64() * 1e3));
                for t in self.terrain().map(|t| t.tiers()).unwrap_or_default() {
                    self.log(&format!(
                        "terrain: {:<8} {:>5.1}% passable, {} regions, {} chokepoints",
                        t.tier.name(),
                        t.passable_fraction * 100.0,
                        t.regions.len(),
                        t.chokes.len()
                    ));
                }
            }
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => {
                self.log("terrain: analysis failed; continuing without terrain model");
                self.terrain = TerrainModel::Unavailable;
            }
        }
    }

    /// Draw a tier's chokepoints, gates and regions as map markers
    /// (`kgbterrain <tier>` chat command), or with `strategy` the
    /// chokepoints of the latest strategic analysis.
    pub fn terrain_draw(&self, tier_name: &str) {
        let Some(terrain) = self.terrain() else {
            self.say("Terrain analysis is not ready.");
            return;
        };
        if tier_name == "strategy" {
            self.strategy_draw();
            return;
        }
        let Some(tier) = Tier::ALL.into_iter().find(|t| t.name() == tier_name) else {
            self.say("Usage: kgbterrain spider|bot|vehicle|hover|ship|amphib|strategy");
            return;
        };
        let Some(map) = terrain.tier(tier) else { return };
        let map_draw = self.cb.map();
        let at = |p: Pos| {
            let y = map_draw.elevation_at(p.x, p.z);
            Vec3::new(p.x, y, p.z)
        };
        for c in &map.chokes {
            if let Some([a, b]) = c.gate {
                map_draw.add_line(at(a), at(b));
            }
            map_draw.add_point(at(c.pos), &format!("C{} {:.0}", c.id, c.width));
        }
        for r in &map.regions {
            map_draw.add_point(at(r.center), &format!("R{}", r.id));
        }
        self.say(&format!("Drew {} regions, {} chokepoints for {}.", map.regions.len(), map.chokes.len(), tier.name()));
    }
}

/// Frames at which the scenario is dumped when `ZKGBAI_DUMP_TERRAIN` is set.
const DUMP_FRAMES: [i32; 3] = [1800, 9000, 18000];

impl Ai {
    /// Development aid: with `ZKGBAI_DUMP_TERRAIN=<dir>` in the environment,
    /// write the map data (`<map>.zktamap`) and the AI's view of the game
    /// (`<map>-f<frame>-team<n>.json`: bases and metal spots with sides) for
    /// offline analysis with `tools/terrain-view`.
    pub fn terrain_dump(&self, frame: i32) {
        if !DUMP_FRAMES.contains(&frame) {
            return;
        }
        let Some(dir) = std::env::var_os("ZKGBAI_DUMP_TERRAIN") else { return };
        let dir = std::path::PathBuf::from(dir);
        let map = self.cb.map();
        let name: String =
            map.name().chars().map(|c| if c.is_ascii_alphanumeric() || c == '.' { c } else { '_' }).collect();
        if let Err(e) = std::fs::create_dir_all(&dir) {
            self.log(&format!("terrain: cannot create dump dir: {e}"));
            return;
        }
        let map_file = dir.join(format!("{name}.zktamap"));
        if !map_file.exists() {
            let (w, h) = (map.width().max(0) as u32, map.height().max(0) as u32);
            let data = SpringMapData {
                map_width: w,
                map_height: h,
                slope_width: w / 2,
                slope_height: h / 2,
                slope_map: map.slope_map(),
                height_map: map.height_map(),
            };
            if let Err(e) = data.save(&map_file) {
                self.log(&format!("terrain: cannot write {}: {e}", map_file.display()));
                return;
            }
        }
        let spots: Vec<serde_json::Value> = self
            .strategy_assets()
            .iter()
            .map(|a| {
                let side = match a.side {
                    Side::Ally => "ally",
                    Side::Enemy => "enemy",
                    Side::Neutral => "neutral",
                };
                serde_json::json!({ "x": a.pos.x, "z": a.pos.z, "value": a.value, "side": side })
            })
            .collect();
        let (ally, enemy) = self.strategy_bases().unwrap_or_default();
        let base = |v: Vec3| serde_json::json!([v.x, v.z]);
        let scenario = serde_json::json!({
            "map": map.name(),
            "frame": frame,
            "team": self.team_id,
            "zktamap": map_file.file_name().map(|f| f.to_string_lossy().into_owned()),
            "ally_base": base(ally),
            "enemy_base": base(enemy),
            "assets": spots,
        });
        let file = dir.join(format!("{name}-f{frame}-team{}.json", self.team_id));
        match std::fs::write(&file, serde_json::to_string_pretty(&scenario).unwrap_or_default()) {
            Ok(()) => self.log(&format!("terrain: dumped scenario to {}", file.display())),
            Err(e) => self.log(&format!("terrain: cannot write {}: {e}", file.display())),
        }
    }
}

/// Tiers whose routes make two metal spots neighbours for territory
/// inference: whoever holds one can walk, drive or hover to the other.
const GROUND_TIERS: [Tier; 4] = [Tier::Bot, Tier::Vehicle, Tier::Hover, Tier::Amphib];

/// A link is direct if ground travel along it is at most this many times
/// its length (plus [`DETOUR_SLACK`] elmos).
const MAX_DETOUR: f32 = 2.0;
const DETOUR_SLACK: f32 = 300.0;

impl Ai {
    /// Mark the metal-spot links that ground forces travel directly, so
    /// territory shadows do not spread across cliffs and water.
    fn terrain_classify_links(&mut self) {
        let Some(terrain) = self.terrain.get() else { return };
        let maps: Vec<&TierMap> = GROUND_TIERS.iter().filter_map(|&t| terrain.tier(t)).collect();
        let spots = &self.graph.metal_spots;
        let ground: Vec<bool> = self
            .graph
            .links
            .iter()
            .map(|l| {
                let (a, b) = (pos(spots[l.v0].position), pos(spots[l.v1].position));
                let limit = l.length * MAX_DETOUR + DETOUR_SLACK;
                maps.is_empty() || maps.iter().any(|m| ground_distance(m, a, b).is_some_and(|d| d <= limit))
            })
            .collect();
        let indirect = ground.iter().filter(|&&g| !g).count();
        self.graph.ground_links = ground;
        self.log(&format!(
            "terrain: {indirect} of {} metal-spot links cross impassable terrain",
            self.graph.links.len()
        ));
    }
}

/// Frames between strategic re-analyses.
const STRATEGY_PERIOD: i32 = 900;

/// How far behind a chokepoint (on our side) its defenses stand, elmos.
const FORTIFY_SETBACK: f32 = 120.0;

/// The latest strategic analysis of the game, on the movement tier of the
/// enemy's ground army.
pub struct Strategy {
    pub frame: i32,
    pub tier: Tier,
    pub analysis: Analysis,
    /// Chokepoints worth fortifying, nearest the enemy first.
    pub fortify: Vec<Fortification>,
}

/// A chokepoint guarding our territory from enemy ground forces.
pub struct Fortification {
    pub choke: usize,
    /// Where its defenses go: just behind the gate on our side.
    pub pos: Vec3,
    /// Passage width, elmos.
    pub width: f32,
    /// No build site near `pos` (until the next analysis).
    pub blocked: bool,
}

impl Ai {
    /// Our base and the enemy's for strategic purposes: the enemy's is the
    /// center of their seen mexes, or of their presumed territory before
    /// any are seen. `None` while the enemy's whereabouts are unknown.
    fn strategy_bases(&self) -> Option<(Vec3, Vec3)> {
        let ally =
            if self.graph.get_start_pos().is_zero() { self.graph.get_ally_base() } else { self.graph.get_start_pos() };
        let mut enemy = self.graph.get_enemy_center();
        if enemy.is_zero() {
            let territory = self.graph.get_enemy_territory();
            let n = territory.len() as f32;
            enemy = territory
                .iter()
                .fold(Vec3::ZERO, |c, s| Vec3::new(c.x + s.position.x / n, 0.0, c.z + s.position.z / n));
        }
        (!ally.is_zero() && !enemy.is_zero()).then_some((ally, enemy))
    }

    /// The movement tier most of the enemy's seen ground army (by cost)
    /// belongs to; bots until we know better.
    fn enemy_ground_tier(&self) -> Tier {
        let mut cost: Vec<(Tier, f32)> = Vec::new();
        for e in self.war.targets.values().filter(|e| !e.is_static) {
            let Some((ud, tier)) = e.ud.and_then(|ud| Some((ud, tier_of(ud)?))) else { continue };
            match cost.iter_mut().find(|(t, _)| *t == tier) {
                Some((_, c)) => *c += ud.cost(self.metal),
                None => cost.push((tier, ud.cost(self.metal))),
            }
        }
        cost.into_iter().max_by(|a, b| a.1.total_cmp(&b.1)).map_or(Tier::Bot, |(t, _)| t)
    }

    /// Metal spots as assets of the side that holds (or probably holds)
    /// them.
    fn strategy_assets(&self) -> Vec<Asset> {
        self.graph
            .metal_spots
            .iter()
            .map(|s| {
                let side = if s.owned || (s.ally_shadowed && !s.hostile) {
                    Side::Ally
                } else if s.hostile || s.enemy_shadowed {
                    Side::Enemy
                } else {
                    Side::Neutral
                };
                Asset { pos: pos(s.position), value: s.value, side }
            })
            .collect()
    }

    /// Re-run the strategic analysis every [`STRATEGY_PERIOD`] frames.
    pub fn strategy_update(&mut self, frame: i32) {
        if (frame + self.offset) % STRATEGY_PERIOD != 0 {
            return;
        }
        let Some((ally, enemy)) = self.strategy_bases() else { return };
        let tier = self.enemy_ground_tier();
        let Some(map) = self.terrain().and_then(|t| t.tier(tier)) else { return };
        let assets = self.strategy_assets();
        let analysis = Analysis::run(&Scenario { map, ally_base: pos(ally), enemy_base: pos(enemy), assets: &assets });
        let elevation = self.cb.map();
        // Spiders climb around any chokepoint.
        let fortify = if tier == Tier::Spider { Vec::new() } else { fortifications(map, &analysis) };
        let fortify = fortify
            .into_iter()
            .map(|(choke, p, width)| Fortification {
                choke,
                pos: Vec3::new(p.x, elevation.elevation_at(p.x, p.z), p.z),
                width,
                blocked: false,
            })
            .collect();
        self.strategy = Some(Strategy { frame, tier, analysis, fortify });
    }

    /// Mark the held chokepoints and the fortification sites.
    fn strategy_draw(&self) {
        let (Some(strategy), Some(terrain)) = (&self.strategy, self.terrain()) else {
            self.say("No strategic analysis yet.");
            return;
        };
        let Some(map) = terrain.tier(strategy.tier) else { return };
        let draw = self.cb.map();
        for &c in &strategy.analysis.hold {
            let p = map.chokes[c].pos;
            draw.add_point(Vec3::new(p.x, draw.elevation_at(p.x, p.z), p.z), &format!("hold C{c}"));
        }
        for f in &strategy.fortify {
            draw.add_point(f.pos, &format!("fortify C{} ({:.0} wide)", f.choke, f.width));
        }
        self.say(&format!(
            "Frame {} ({} tier): hold {} chokepoints, fortifying {}.",
            strategy.frame,
            strategy.tier.name(),
            strategy.analysis.hold.len(),
            strategy.fortify.len()
        ));
    }
}

/// The chokepoints worth holding (see [`Analysis::hold`]) that lie between
/// our base and theirs: (choke, defense position, width), nearest the enemy
/// first.
fn fortifications(map: &TierMap, a: &Analysis) -> Vec<(usize, Pos, f32)> {
    let Some(base) = a.ally_base else { return Vec::new() };
    let mut sites: Vec<_> = a
        .hold
        .iter()
        .filter_map(|&c| {
            let choke = &map.chokes[c];
            // Not in a pocket behind our base, which the enemy can only
            // reach through it anyway.
            if a.choke_distance(map, c, false) >= a.dist_from_enemy[base] {
                return None;
            }
            let home = map.regions[a.home_side(map, c)?].center;
            let d = choke.pos.distance(home);
            let t = if d > 0.0 { (FORTIFY_SETBACK / d).min(0.5) } else { 0.0 };
            let p = Pos::new(choke.pos.x + (home.x - choke.pos.x) * t, choke.pos.z + (home.z - choke.pos.z) * t);
            Some((a.choke_distance(map, c, false), (c, p, choke.width)))
        })
        .collect();
    sites.sort_by(|x, y| x.0.total_cmp(&y.0));
    sites.into_iter().map(|(_, site)| site).collect()
}
