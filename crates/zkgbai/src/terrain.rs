//! The terrain model: regions, chokepoints and connectivity for every
//! movement tier (see the `zk-terrain` crate). It is built on a background
//! thread at startup; until it is ready, `Ai::terrain` yields nothing and
//! callers fall back to the metal-spot graph.
//!
//! Once it is ready, a strategic analysis of the game (which chokepoints
//! guard our territory) is refreshed periodically, and the metal-spot
//! graph learns which of its links ground forces travel directly. The
//! terrain itself is re-analysed every [`REANALYSIS_PERIOD`] frames, so
//! terraforming (ours or the enemy's: walls, ditches, ramps) shows up.

use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::time::{Duration, Instant};

use springai::{Callback, Unit, UnitDef, Vec3};
use zk_terrain::occupancy::{Occupancy, Presence};
use zk_terrain::paths::{Paths, SNAP};
use zk_terrain::strategy::{Analysis, Asset, Scenario, Side, ground_distance};
use zk_terrain::{ChokeKind, MoveClass, Pos, SpringMapData, Terrain, Tier, TierMap};

use crate::Ai;
use crate::defs::Defs;

/// A model and its compute time, from the analysis thread.
type Analysed = Receiver<(Terrain, Duration)>;

pub enum TerrainModel {
    /// First analysis running.
    Pending {
        rx: Analysed,
    },
    Ready {
        terrain: Terrain,
        /// A re-analysis under way.
        rebuild: Option<Analysed>,
    },
    Unavailable,
}

/// Frames between re-analyses of the terrain.
const REANALYSIS_PERIOD: i32 = 30 * 60;

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
        let classes: Vec<String> = tier_classes(defs)
            .iter()
            .map(|(t, c)| {
                format!(
                    "{} {:.3} (crawl {:.3}, slopeMod {:.1})",
                    t.name(),
                    c.max_slope,
                    c.soft().max_slope,
                    c.slope_mod
                )
            })
            .collect();
        cb.log(&format!("terrain: slope limits {}", classes.join(", ")));
        match analyse(cb, defs) {
            Ok(rx) => TerrainModel::Pending { rx },
            Err(e) => {
                cb.log(&format!("terrain: {e}; terrain analysis disabled"));
                TerrainModel::Unavailable
            }
        }
    }

    /// The model, once the analysis has finished.
    pub fn get(&self) -> Option<&Terrain> {
        match self {
            TerrainModel::Ready { terrain, .. } => Some(terrain),
            _ => None,
        }
    }
}

/// The map as it is now.
fn map_data(cb: Callback) -> Result<SpringMapData, String> {
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
        return Err("unexpected map data sizes".into());
    }
    Ok(data)
}

/// Each tier's movement class, from the game's constructor of that tier.
fn tier_classes(defs: &Defs) -> Vec<(Tier, MoveClass)> {
    TIER_UNITS
        .iter()
        .map(|&(tier, unit)| (tier, defs.get(unit).and_then(move_class).unwrap_or_else(|| tier.zero_k_class())))
        .collect()
}

/// Analyse the map as it is now on a background thread.
fn analyse(cb: Callback, defs: &Defs) -> Result<Analysed, String> {
    let data = map_data(cb)?;
    let classes = tier_classes(defs);
    let (tx, rx) = mpsc::channel();
    // Pure computation: the thread must never call into the engine.
    std::thread::Builder::new()
        .name("zkgbai-terrain".into())
        .spawn(move || {
            let start = Instant::now();
            let terrain = Terrain::build_with(&data, &classes);
            let _ = tx.send((terrain, start.elapsed()));
        })
        .map_err(|e| format!("cannot start analysis thread ({e})"))?;
    Ok(rx)
}

/// The terrain limits of a ground unit type; `None` for aircraft and
/// buildings.
pub fn move_class(def: UnitDef) -> Option<MoveClass> {
    let md = def.move_data()?;
    let slope_mod = md.slope_mod();
    Some(MoveClass {
        max_slope: md.max_slope(),
        slope_mod: if slope_mod > 0.0 { slope_mod } else { zk_terrain::default_slope_mod(md.max_slope()) },
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

/// Whether a ground unit of type `def` at `from` can get within `range`
/// elmos of `to` (a worker within build range of a site, say); true for
/// aircraft and wherever the terrain model cannot tell.
pub fn reaches(terrain: Option<&Terrain>, def: UnitDef, from: Vec3, to: Vec3, range: f32) -> bool {
    let Some(map) = tier_of(def).and_then(|t| terrain?.tier(t)) else { return true };
    map.region_near(pos(from), SNAP).is_none() || map.reaches_within(pos(from), pos(to), range.max(SNAP), SNAP)
}

/// Whether unit `u` (from where it is) can get within weapon range of a
/// target, by [`reaches`].
pub fn engage_test(terrain: Option<&Terrain>, u: Unit) -> impl Fn(Vec3) -> bool + '_ {
    engage_test_from(terrain, u.def(), u.pos())
}

/// [`engage_test`] for a unit of type `def` at `from`.
pub fn engage_test_from(terrain: Option<&Terrain>, def: Option<UnitDef>, from: Vec3) -> impl Fn(Vec3) -> bool + '_ {
    let range = def.map_or(0.0, |d| d.max_weapon_range()).clamp(SNAP, MAX_ENGAGE_RANGE);
    move |target| def.is_none_or(|d| reaches(terrain, d, from, target, range))
}

impl Ai {
    /// The terrain model, if its analysis has finished.
    pub fn terrain(&self) -> Option<&Terrain> {
        self.terrain.get()
    }

    /// Whether the first terrain analysis is still running.
    pub fn terrain_pending(&self) -> bool {
        matches!(self.terrain, TerrainModel::Pending { .. })
    }

    /// See [`reaches`].
    pub fn terrain_reaches(&self, def: UnitDef, from: Vec3, to: Vec3, range: f32) -> bool {
        reaches(self.reach_terrain(), def, from, to, range)
    }

    /// The terrain model for reachability checks (none when they are off).
    pub fn reach_terrain(&self) -> Option<&Terrain> {
        if self.dev_off("reach") { None } else { self.terrain.get() }
    }

    /// Pick up the finished analysis; keep the link classification current;
    /// re-analyse now and then.
    pub fn terrain_update(&mut self, frame: i32) {
        if self.terrain().is_some() && self.graph.ground_links.len() != self.graph.links.len() {
            self.terrain_classify_links();
        }
        let reanalyse = !self.dev_off("reanalysis");
        if let TerrainModel::Ready { rebuild, .. } = &mut self.terrain {
            match rebuild.as_ref().map(|rx| rx.try_recv()) {
                None if reanalyse && (frame + self.offset) % REANALYSIS_PERIOD == 0 => {
                    match analyse(self.cb, &self.defs) {
                        Ok(rx) => *rebuild = Some(rx),
                        Err(e) => self.cb.log(&format!("terrain: re-analysis failed: {e}")),
                    }
                }
                Some(Ok((terrain, _))) => self.terrain_replace(terrain),
                Some(Err(TryRecvError::Disconnected)) => *rebuild = None,
                _ => {}
            }
            return;
        }
        let TerrainModel::Pending { rx } = &self.terrain else { return };
        match rx.try_recv() {
            Ok((terrain, compute)) => {
                self.terrain = TerrainModel::Ready { terrain, rebuild: None };
                self.log(&format!("terrain: analysis took {:.0} ms", compute.as_secs_f64() * 1e3));
                for t in self.terrain().map(|t| t.tiers()).unwrap_or_default() {
                    let gates = t.chokes.iter().filter(|c| c.kind == ChokeKind::Gate).count();
                    self.log(&format!(
                        "terrain: {:<8} {:>5.1}% passable, {} regions, {gates} gates, {} borders",
                        t.tier.name(),
                        t.passable_fraction * 100.0,
                        t.regions.len(),
                        t.chokes.len() - gates
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
    /// (`kgbterrain <tier>` chat command), with `strategy` the chokepoints
    /// of the latest strategic analysis, with `threat` the enemy's
    /// incursions and our blocks, or with `occupancy` who holds each region.
    pub fn terrain_draw(&self, tier_name: &str) {
        let Some(terrain) = self.terrain() else {
            self.say("Terrain analysis is not ready.");
            return;
        };
        if tier_name == "strategy" {
            self.strategy_draw();
            return;
        }
        if tier_name == "threat" {
            self.incursions_draw();
            return;
        }
        if tier_name == "occupancy" {
            self.occupancy_draw();
            return;
        }
        let Some(tier) = Tier::ALL.into_iter().find(|t| t.name() == tier_name) else {
            self.say("Usage: kgbterrain spider|bot|vehicle|hover|ship|amphib|strategy|threat|occupancy");
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
            // Climbs into each side, where crossing is slower than flat ground.
            let climbs: Vec<String> = (c.regions.iter().zip(&c.climb))
                .filter(|&(_, &x)| x >= 16.0)
                .map(|(r, x)| format!(" up into R{r} +{x:.0}"))
                .collect();
            let label = match c.kind {
                ChokeKind::Gate => {
                    let round = c.bypass.iter().copied().fold(f32::INFINITY, f32::min);
                    let sides = if round.is_finite() { format!(", round it +{round:.0}") } else { String::new() };
                    format!("C{} gate {:.0} (wall {:.0}{sides})", c.id, c.width, c.wall)
                }
                ChokeKind::Border => format!("C{} border {:.0}", c.id, c.width),
            };
            map_draw.add_point(at(c.pos), &format!("{label}{}", climbs.concat()));
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
            let saved = map_data(self.cb).and_then(|data| data.save(&map_file).map_err(|e| e.to_string()));
            if let Err(e) = saved {
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

impl Ai {
    /// Swap in a re-analysed model: report what terraforming changed, and
    /// redo what refers to the old model's regions and chokepoints.
    fn terrain_replace(&mut self, terrain: Terrain) {
        let changes = self.terrain_changes(&terrain);
        self.terrain = TerrainModel::Ready { terrain, rebuild: None };
        if let Some(changes) = changes {
            self.log(&format!("terrain: re-analysis: {changes}"));
        }
        self.graph.ground_links.clear();
        self.incursions.forget_terrain();
        self.strategy = None;
        self.strategy_refresh(self.frame);
    }

    /// How ground routes from our base to the metal spots changed between
    /// the model in use and `new`, by tier; `None` if they did not.
    fn terrain_changes(&self, new: &Terrain) -> Option<String> {
        let old = self.terrain()?;
        let (home, _) = self.strategy_bases()?;
        let spots: Vec<Pos> = self.graph.metal_spots.iter().map(|m| pos(m.position)).collect();
        let mut report = Vec::new();
        for tier in GROUND_TIERS {
            let (Some(a), Some(b)) = (old.tier(tier), new.tier(tier)) else { continue };
            let (before, after) = (Paths::new(a, pos(home), |_| false), Paths::new(b, pos(home), |_| false));
            let (mut longer, mut cut, mut shorter) = (0, 0, 0);
            for &p in &spots {
                match (before.distance_to(p), after.distance_to(p)) {
                    (Some(_), None) => cut += 1,
                    (None, Some(_)) => shorter += 1,
                    (Some(x), Some(y)) if y > x * CHANGED + DETOUR_SLACK => longer += 1,
                    (Some(x), Some(y)) if x > y * CHANGED + DETOUR_SLACK => shorter += 1,
                    _ => {}
                }
            }
            if longer + cut + shorter > 0 {
                report.push(format!(
                    "{}: {longer} metal spots further from our base, {cut} cut off, {shorter} nearer \
                     ({} -> {} regions)",
                    tier.name(),
                    a.regions.len(),
                    b.regions.len()
                ));
            }
        }
        (!report.is_empty()).then(|| report.join("; "))
    }
}

/// A route changed if it is this many times longer or shorter (plus
/// [`DETOUR_SLACK`] elmos).
const CHANGED: f32 = 1.3;

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

/// Enemy units seen within this many frames hold the region they were in.
pub const PRESENCE_RECENT: i32 = 30 * 30;

/// How far behind a chokepoint (on our side) its defenses stand, elmos.
const FORTIFY_SETBACK: f32 = 120.0;

/// A chokepoint is held when our defenses covering it are this strong
/// (about two light laser turrets, on the scale of enemy danger), and a
/// region when they cover its middle this strongly (four).
pub const HELD_POWER: f32 = 150.0;
const HELD_REGION_POWER: f32 = 400.0;

/// Enemy ground forces threaten what is within this many elmos of ground
/// they can stand on (a typical weapon range).
const STRIKE_RANGE: f32 = 400.0;

/// Weapon ranges beyond this count as this much when judging whether a
/// unit can engage a target (it bounds the search for ground in range).
const MAX_ENGAGE_RANGE: f32 = 600.0;

/// The latest strategic analysis of the game, on the movement tier of the
/// enemy's ground army.
pub struct Strategy {
    pub frame: i32,
    pub tier: Tier,
    pub analysis: Analysis,
    /// Chokepoints by strategic value (legacy: nearest the enemy first).
    pub fortify: Vec<Fortification>,
    /// Regions (of the tier's map) the enemy's ground army can only reach
    /// through our base or past chokepoints our defenses hold, or not at
    /// all: ground defenses there are wasted.
    pub sheltered: Vec<bool>,
    /// What holds each region (see [`Ai::region_presence`]).
    pub occupancy: Occupancy,
    /// The enemy's base, and the other movement tiers its ground forces
    /// have shown.
    pub(crate) enemy_base: Pos,
    /// Initial enemy base estimate, stable as their territory expands.
    leyline_enemy: Pos,
    other_tiers: Vec<Tier>,
}

/// A chokepoint guarding our territory from enemy ground forces.
#[derive(Clone)]
pub struct Fortification {
    pub choke: usize,
    pub home: usize,
    pub score: f32,
    /// Placement seed; shared defenses may stand elsewhere.
    pub pos: Vec3,
    /// Width of the front its defenses cover, elmos (see
    /// `Choke::hold_width`).
    pub width: f32,
    /// No build site near `pos` (until the next analysis).
    pub blocked: bool,
}

impl Ai {
    /// Our base and the enemy's for strategic purposes: the enemy's is the
    /// center of their seen mexes, or of their presumed territory before
    /// any are seen. `None` while the enemy's whereabouts are unknown.
    pub(crate) fn strategy_bases(&self) -> Option<(Vec3, Vec3)> {
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

    /// The movement tiers of the enemy's seen ground army, with what it
    /// cost, most first; bots until we know better.
    pub(crate) fn enemy_ground_tiers(&self) -> Vec<Tier> {
        let mut cost: Vec<(Tier, f32)> = Vec::new();
        for e in self.war.targets.values().filter(|e| !e.is_static) {
            let Some((ud, tier)) = e.ud.and_then(|ud| Some((ud, tier_of(ud)?))) else { continue };
            match cost.iter_mut().find(|(t, _)| *t == tier) {
                Some((_, c)) => *c += ud.cost(self.metal),
                None => cost.push((tier, ud.cost(self.metal))),
            }
        }
        cost.sort_by(|a, b| b.1.total_cmp(&a.1));
        let tiers: Vec<Tier> = cost.into_iter().map(|(t, _)| t).collect();
        if tiers.is_empty() { vec![Tier::Bot] } else { tiers }
    }

    /// Metal spots as assets of the side that holds (or probably holds)
    /// them.
    pub(crate) fn strategy_assets(&self) -> Vec<Asset> {
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

    /// What is known to stand on the map, for who holds which region: the
    /// enemy's buildings, and its ground units seen in the last
    /// [`PRESENCE_RECENT`] frames, with their danger to ground forces or to
    /// aircraft and their value, and the anti-air known only by its fire
    /// (see `air_intel`); our fighters, and our metal spots' value.
    pub fn region_presence(&self) -> Vec<Presence> {
        let frame = self.frame;
        let mut presence: Vec<Presence> = (self.war.targets.values())
            .filter(|e| !e.is_dead && !e.can_fly() && (e.is_static || frame - e.last_seen <= PRESENCE_RECENT))
            .map(|e| {
                let danger = e.get_danger();
                let aa = e.is_aa || e.is_flex_aa;
                Presence {
                    pos: pos(e.position),
                    side: Side::Enemy,
                    power: if e.is_aa && !e.is_flex_aa { 0.0 } else { danger },
                    aa: if aa { danger } else { 0.0 },
                    value: e.value,
                    seen: e.last_seen,
                }
            })
            .collect();
        // Anti-air known only by its fire.
        presence.extend(self.air_intel_presence().into_iter().map(|(p, aa, seen)| Presence {
            pos: pos(p),
            side: Side::Enemy,
            power: 0.0,
            aa,
            value: 0.0,
            seen,
        }));
        presence.extend(self.fighters.values().map(|f| Presence {
            pos: pos(f.get_pos()),
            side: Side::Ally,
            power: (f.power + f.unit.max_health()) / 10.0,
            aa: 0.0,
            value: f.metal_value,
            seen: frame,
        }));
        presence.extend(self.graph.get_owned_spots().iter().map(|m| Presence {
            pos: pos(m.get_pos()),
            side: Side::Ally,
            power: 0.0,
            aa: 0.0,
            value: m.value,
            seen: frame,
        }));
        presence
    }

    /// Label each region of the strategy's tier with who holds it.
    fn occupancy_draw(&self) {
        let (Some(strategy), Some(terrain)) = (&self.strategy, self.terrain()) else {
            self.say("No strategic analysis yet.");
            return;
        };
        let Some(map) = terrain.tier(strategy.tier) else { return };
        let draw = self.cb.map();
        let mut held = [0; 3];
        for (r, h) in strategy.occupancy.regions.iter().enumerate() {
            let side = h.holder();
            held[side as usize] += 1;
            if side == Side::Neutral {
                continue;
            }
            let c = map.regions[r].center;
            draw.add_point(
                Vec3::new(c.x, draw.elevation_at(c.x, c.z), c.z),
                &format!(
                    "R{r} enemy {:.0} (aa {:.0}{}, {:.0}m), ours {:.0} ({:.0}m)",
                    h.enemy_power,
                    h.enemy_aa,
                    h.aa_seen.map_or(String::new(), |s| format!(" {} s old", (self.frame - s) / 30)),
                    h.enemy_value,
                    h.ally_power,
                    h.ally_value
                ),
            );
        }
        self.say(&format!(
            "Frame {} ({} tier): {} regions ours, {} the enemy's, {} no one's.",
            strategy.frame,
            strategy.tier.name(),
            held[Side::Ally as usize],
            held[Side::Enemy as usize],
            held[Side::Neutral as usize]
        ));
    }

    /// Re-run the strategic analysis every [`STRATEGY_PERIOD`] frames.
    pub fn strategy_update(&mut self, frame: i32) {
        if (frame + self.offset) % STRATEGY_PERIOD == 0 {
            self.strategy_refresh(frame);
        }
    }

    /// Run the strategic analysis now.
    pub(crate) fn strategy_refresh(&mut self, frame: i32) {
        let Some((ally, enemy)) = self.strategy_bases() else { return };
        let tiers = self.enemy_ground_tiers();
        let tier = tiers[0];
        let Some(map) = self.terrain().and_then(|t| t.tier(tier)) else { return };
        let all_assets = self.strategy_assets();
        let mut assets = all_assets.clone();
        // With `detour`, chokepoints are held (and walled) for our own metal
        // only, not half of every free spot behind them: on Otago three
        // mexes of ours had gates fortified at 1:29.
        if !self.dev_off("detour") {
            assets.retain(|a| a.side != Side::Neutral);
        }
        let analysis = Analysis::run(&Scenario { map, ally_base: pos(ally), enemy_base: pos(enemy), assets: &assets });
        let elevation = self.cb.map();
        let leyline_enemy = self.strategy.as_ref().map_or(pos(enemy), |s| s.leyline_enemy);
        // The default-on fortify policy uses shared coverage and leylines.
        // Disabling it restores the old value-cut gates and their ordering.
        let gates = if tier == Tier::Spider {
            Vec::new()
        } else if self.dev_off("fortify") {
            fortifications(map, &analysis)
                .into_iter()
                .filter_map(|(choke, pos, width)| {
                    Some(zk_terrain::fortify::GateValue {
                        choke,
                        home: analysis.home_side(map, choke)?,
                        pos,
                        width,
                        score: 1.0,
                    })
                })
                .collect()
        } else {
            zk_terrain::fortify::gates(map, &analysis, pos(ally), leyline_enemy, &all_assets)
        };
        let fortify = gates
            .into_iter()
            .map(|g| Fortification {
                choke: g.choke,
                home: g.home,
                score: g.score,
                pos: Vec3::new(g.pos.x, elevation.elevation_at(g.pos.x, g.pos.z), g.pos.z),
                width: g.width,
                blocked: false,
            })
            .collect();
        let held = |c: usize| {
            let p = map.chokes[c].pos;
            self.war.get_friendly_porc_threat(Vec3::xz(p.x, p.z)) >= HELD_POWER
        };
        // A region our defenses cover in strength holds like a chokepoint.
        let held_region = |r: usize| {
            let c = map.regions[r].center;
            self.war.get_friendly_porc_threat(Vec3::xz(c.x, c.z)) >= HELD_REGION_POWER
        };
        let sheltered = analysis.sheltered(map, pos(enemy), held, held_region);
        let other_tiers = tiers[1..].to_vec();
        let occupancy = Occupancy::new(map, &self.region_presence());
        let strategy = Strategy {
            frame,
            tier,
            analysis,
            fortify,
            sheltered,
            occupancy,
            enemy_base: pos(enemy),
            leyline_enemy,
            other_tiers,
        };
        // The first analysis, and whenever what is sheltered changes much.
        let before = self.strategy.as_ref().map_or(0, |s| s.sheltered.iter().filter(|&&s| s).count());
        let now = strategy.sheltered.iter().filter(|&&s| s).count();
        if before.abs_diff(now) > 1 || self.strategy.is_none() {
            let spots =
                self.graph.get_owned_spots().iter().filter(|m| self.strategy_shelters(&strategy, m.get_pos())).count();
            self.log(&format!(
                "terrain: {now} of {} {} regions sheltered from enemy ground forces ({spots} of our metal spots)",
                strategy.sheltered.len(),
                tier.name()
            ));
        }
        self.strategy = Some(strategy);
    }

    /// Whether enemy ground forces can only reach `p` through our base or
    /// past chokepoints we hold, or not at all (see [`Strategy::sheltered`]);
    /// false before the first strategic analysis.
    pub fn strategy_sheltered(&self, p: Vec3) -> bool {
        !self.dev_off("porc") && self.strategy.as_ref().is_some_and(|s| self.strategy_shelters(s, p))
    }

    fn strategy_shelters(&self, s: &Strategy, p: Vec3) -> bool {
        let Some(terrain) = self.terrain() else { return false };
        let Some(map) = terrain.tier(s.tier) else { return false };
        // Somewhere the enemy can stand within striking range (none: out of
        // its reach, at sea say).
        let main = map.region_near(pos(p), STRIKE_RANGE).is_none_or(|r| s.sheltered[r]);
        // The enemy's other ground forces must not get there at all.
        main && !s
            .other_tiers
            .iter()
            .any(|&t| terrain.tier(t).is_some_and(|m| m.connected(s.enemy_base, pos(p), STRIKE_RANGE)))
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
/// our base and theirs: (choke, defense position, front width), nearest the
/// enemy first.
fn fortifications(map: &TierMap, a: &Analysis) -> Vec<(usize, Pos, f32)> {
    let Some(base) = a.ally_base else { return Vec::new() };
    let mut sites: Vec<_> = a
        .hold
        .iter()
        .filter_map(|&c| {
            // Not in a pocket behind our base, which the enemy can only
            // reach through it anyway.
            if a.choke_distance(map, c, false) >= a.dist_from_enemy[base] {
                return None;
            }
            let home = a.home_side(map, c)?;
            let p = map.setback(c, home, FORTIFY_SETBACK);
            Some((a.choke_distance(map, c, false), (c, p, map.chokes[c].hold_width(home))))
        })
        .collect();
    sites.sort_by(|x, y| x.0.total_cmp(&y.0));
    sites.into_iter().map(|(_, site)| site).collect()
}
