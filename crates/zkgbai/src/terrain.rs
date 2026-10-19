//! The terrain model: regions, chokepoints and connectivity for every
//! movement tier (see the `zk-terrain` crate). It is built on a background
//! thread at startup; until it is ready, `Ai::terrain` yields nothing and
//! callers fall back to the metal-spot graph.

use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::time::{Duration, Instant};

use springai::{Callback, UnitDef, Vec3};
use zk_terrain::{MoveClass, Pos, SpringMapData, Terrain, Tier};

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

    /// Pick up the finished analysis.
    pub fn terrain_update(&mut self) {
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
    /// (`kgbterrain <tier>` chat command).
    pub fn terrain_draw(&self, tier_name: &str) {
        let Some(terrain) = self.terrain() else {
            self.say("Terrain analysis is not ready.");
            return;
        };
        let Some(tier) = Tier::ALL.into_iter().find(|t| t.name() == tier_name) else {
            self.say("Usage: kgbterrain spider|bot|vehicle|hover|ship|amphib");
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
