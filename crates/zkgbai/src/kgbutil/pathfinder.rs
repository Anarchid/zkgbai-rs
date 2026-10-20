//! Threat-aware A* pathfinder over a downsampled slope map.

mod astar;

use std::collections::VecDeque;

use springai::{Callback, Map, MoveData, Unit, UnitDef, Vec3};

use self::astar::{AStar, Best, IMPASSABLE, Reach, SearchOpts, thin_waypoints};
use crate::defs::Defs;
use crate::military::MilitaryManager;
use crate::military::water::Dive;
use crate::util::{NULLPOS, distance};

/// Engine slope-map cells merged per pathfinder cell, per axis.
const MAP_COMPRESSION: i32 = 4;
/// Elmos per engine slope-map cell.
const ORIGINAL_MAP_RES: i32 = 16;
/// Elmos per pathfinder cell.
const MAP_RES: i32 = MAP_COMPRESSION * ORIGINAL_MAP_RES;

/// `minDepth` for units without a minimum water depth.
const NO_MIN_DEPTH: f32 = 65535.0;

/// Extra cost of a cell inside a keep-out circle (see
/// [`Pathfinder::find_path_keepout`]): above any threat cost.
const KEEPOUT_COST: f32 = 200.0;

/// Path cost model (the original's `costSupplier` integer constants, in order).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PathCost {
    /// Fastest path to target (not shortest).
    TestPath = 0,
    /// Fastest path to target while avoiding AA.
    AirPath = 1,
    /// Fastest path to target while avoiding AA and riots.
    AirRaiderPath = 2,
    /// Best path to target while preferring steep slopes.
    SpiderRaiderPath = 3,
    /// Best path to target while preferring steep slopes.
    WidowPath = 4,
    /// Best path to target while preferring steep slopes.
    SpiderPath = 5,
    /// Fastest path to target while avoiding riot units.
    RaiderPath = 6,
    /// Fastest path to target while avoiding decloaking.
    ScythePath = 7,
    /// Fastest path to target while not suiciding too badly.
    AssaultPath = 8,
    /// Fastest path to target while preventing getting stuck on buildings.
    StriderPath = 9,
    /// Fastest path to target while avoiding enemies that are able to attack.
    AvoidEnemies = 10,
    /// Can a raider (group) reach a destination in one piece?
    RaiderCheck = 11,
    /// Can a scythe (group) reach a destination without decloaking?
    ScytheCheck = 12,
    /// Can a squad reach a destination in one piece?
    AssaultCheck = 13,
    /// Can a flyer reach a destination in one piece?
    AirCheck = 14,
    /// Can a flying raider (group) reach a destination in one piece?
    AirRaiderCheck = 15,
}

pub struct Pathfinder {
    cb: Callback,
    terrain: Terrain,
    astar: AStar,
    /// Scratch buffer for traced paths.
    path_buf: Vec<usize>,
    /// Extra cost per cell of the current query's keep-out circles (empty
    /// without any).
    keepout: Vec<f32>,
    scythe_id: i32,
    /// Spider movement class max slope; units sharing it path like spiders.
    spider_max_slope: Option<f32>,
    spider_scout_id: i32,
    spider_antiheavy_id: i32,
    gunship_bomb_id: i32,
    gunship_raid_id: i32,
}

/// Grid geometry, the slope map and the per-query "uniforms" read by the
/// cost functions.
struct Terrain {
    map: Map,
    mwidth: f32,
    mheight: f32,
    smwidth: i32,
    slope_map: Vec<f32>,
    /// Ground elevation at each cell's corner, refreshed with the slope map.
    /// Cost functions read this instead of querying the engine per cell.
    elev_map: Vec<f32>,
    /// Footprint probe for strider pathing.
    blocker: Option<UnitDef>,

    max_threat: f32,
    min_depth: f32,
    max_depth: f32,
    max_slope: f32,
    floater: bool,
    amph: bool,
    /// From what depth the unit is all under water, where only what hits
    /// under water threatens it (see [`crate::military::water`]).
    dive: Dive,
}

impl Pathfinder {
    pub fn new(cb: Callback, defs: &Defs) -> Self {
        let map = cb.map();
        let mwidth = map.width() as f32 * 8.0;
        let mheight = map.height() as f32 * 8.0;
        let mut pf = Pathfinder {
            cb,
            terrain: Terrain {
                map,
                mwidth,
                mheight,
                smwidth: (mwidth / MAP_RES as f32) as i32,
                slope_map: Vec::new(),
                elev_map: Vec::new(),
                blocker: defs.get("striderdetriment"),
                max_threat: 0.0,
                min_depth: 0.0,
                max_depth: 0.0,
                max_slope: 0.0,
                floater: false,
                amph: false,
                dive: Dive::SURFACE,
            },
            astar: AStar::new(0, 0),
            path_buf: Vec::with_capacity(256),
            keepout: Vec::new(),
            scythe_id: defs.id("cloakheavyraid"),
            spider_max_slope: defs.get("spidercon").and_then(|d| d.move_data()).map(|md| md.max_slope()),
            spider_scout_id: defs.id("spiderscout"),
            spider_antiheavy_id: defs.id("spiderantiheavy"),
            gunship_bomb_id: defs.id("gunshipbomb"),
            gunship_raid_id: defs.id("gunshipraid"),
        };
        pf.update_slope_map();
        pf
    }

    /// Refresh the slope map (terrain can be deformed during the game).
    pub fn update_slope_map(&mut self) {
        let map = self.cb.map().slope_map();
        let t = &mut self.terrain;
        let sm_len = map.len() / MAP_COMPRESSION as usize / MAP_COMPRESSION as usize;
        // port: start from zeros like the Java (fresh array); the C++ resize()
        // kept the previous maxima, so slopes could never decrease.
        t.slope_map.clear();
        t.slope_map.resize(sm_len, 0.0);
        let src_width = t.smwidth * MAP_COMPRESSION;
        let (xs, ys) = (t.mwidth / ORIGINAL_MAP_RES as f32, t.mheight / ORIGINAL_MAP_RES as f32);
        let mut y = 0;
        while (y as f32) < ys {
            let mut x = 0;
            while (x as f32) < xs {
                let (cx, cy) = (x / MAP_COMPRESSION, y / MAP_COMPRESSION);
                let src = map.get((y * src_width + x) as usize);
                let dst = t.slope_map.get_mut((cy * t.smwidth + cx) as usize);
                if let (Some(&s), Some(d)) = (src, dst) {
                    *d = d.max(s);
                }
                x += 1;
            }
            y += 1;
        }
        // port: the original queried the engine's elevation for every cell a
        // search visited; cache it per refresh (it goes stale under
        // terraforming exactly like the slope map does).
        t.elev_map.clear();
        t.elev_map.extend((0..sm_len).map(|cell| {
            let (x, z) = cell_corner(t.smwidth, cell);
            t.map.elevation_at(x, z)
        }));
        let width = t.smwidth.max(0) as usize;
        if self.astar.width() != width || self.astar.len() != sm_len {
            self.astar = AStar::new(width, sm_len);
        }
    }

    /// Waypoints from the unit's position to `target`, ending with `target`.
    ///
    /// If the target is unreachable the path leads to the reachable point
    /// closest to it (then to the target); if no such point is closer than
    /// the start, the path is just `[target]`. Out-of-map positions also give
    /// `[target]`. Empty only if the unit has no position or definition.
    /// The cost model is overridden for spiders and aircraft.
    pub fn find_path(&mut self, war: &MilitaryManager, u: Unit, target: Vec3, cost: PathCost) -> VecDeque<Vec3> {
        self.find_path_keepout(war, u, target, cost, &[])
    }

    /// [`find_path`](Self::find_path) giving a wide berth to `keepout`
    /// (circles of centre and radius, elmos): a cell whose corner lies in
    /// one costs [`KEEPOUT_COST`] more, so the path keeps out of them where
    /// any way round exists, and leaves them by the shortest way where it
    /// starts in one.
    pub fn find_path_keepout(
        &mut self,
        war: &MilitaryManager,
        u: Unit,
        target: Vec3,
        cost: PathCost,
        keepout: &[(Vec3, f32)],
    ) -> VecDeque<Vec3> {
        let start = u.pos();
        let mut result = VecDeque::new();

        if start == NULLPOS {
            return result;
        }

        // bounds check, needed because unit positions can sometimes be nonsensical for various reasons.
        if !self.terrain.in_map(start) || !self.terrain.in_map(target) {
            result.push_back(target);
            return result;
        }

        let Some(def) = u.def() else {
            return result;
        };
        let mut cost = cost;
        let mut flyer = true;
        let mut flyheight = 0.0;
        self.terrain.max_slope = 1.0;
        self.terrain.dive = war.dive(Some(def));
        if let Some(md) = def.move_data() {
            self.terrain.set_movement(def, md);
            flyer = false;
            if cost != PathCost::AvoidEnemies
                && cost != PathCost::StriderPath
                && Some(self.terrain.max_slope) == self.spider_max_slope
            {
                // use special pathing for spiders, since they favor hills.
                let id = def.id();
                cost = if id == self.spider_scout_id {
                    PathCost::SpiderRaiderPath
                } else if id == self.spider_antiheavy_id {
                    PathCost::WidowPath
                } else {
                    PathCost::SpiderPath
                };
            }
        } else {
            let id = def.id();
            cost = if cost == PathCost::AvoidEnemies || id == self.gunship_bomb_id || id == self.gunship_raid_id {
                PathCost::AirRaiderPath
            } else {
                PathCost::AirPath
            };
            flyheight = start.y - self.terrain.elevation(start);
        }

        let (Some(start_pos), Some(target_pos)) = (self.terrain.cell_of(start), self.terrain.cell_of(target)) else {
            result.push_back(target);
            return result;
        };

        paint_keepout(self.terrain.smwidth, self.terrain.slope_map.len(), keepout, &mut self.keepout);
        let (terrain, extra) = (&self.terrain, &self.keepout);
        let mut cell_cost = |c: usize| match terrain.cost(cost, war, c) {
            IMPASSABLE => IMPASSABLE,
            base => base + extra.get(c).copied().unwrap_or(0.0),
        };
        match self.astar.find_path(start_pos, target_pos, &mut cell_cost) {
            // Nothing closer to the target is reachable: head straight for it.
            None => result.push_back(target),
            Some(end) => {
                self.astar.trace(start_pos, end, &mut self.path_buf);
                for cell in thin_waypoints(&self.path_buf) {
                    result.push_back(if flyer { terrain.to_pos_raised(cell, flyheight) } else { terrain.to_pos(cell) });
                }
                result.push_back(target);
            }
        }
        result
    }

    pub fn is_assault_reachable(&mut self, war: &MilitaryManager, u: Unit, target: Vec3, max_threat: f32) -> bool {
        let Some(def) = u.def() else {
            return false;
        };
        self.terrain.max_threat = max_threat;
        let cost = if def.move_data().is_none() { PathCost::AirCheck } else { PathCost::AssaultCheck };
        self.is_reachable(war, u, target, cost)
    }

    pub fn is_raider_reachable(&mut self, war: &MilitaryManager, u: Unit, target: Vec3, max_threat: f32) -> bool {
        let Some(def) = u.def() else {
            return false;
        };
        self.terrain.max_threat = max_threat;
        let cost = if def.move_data().is_none() {
            PathCost::AirRaiderCheck
        } else if def.id() == self.scythe_id {
            PathCost::ScytheCheck
        } else {
            PathCost::RaiderCheck
        };
        self.is_reachable(war, u, target, cost)
    }

    /// Batch form of [`is_raider_reachable`](Self::is_raider_reachable) for
    /// one unit and threat limit over many targets: a single flood fill
    /// instead of a search per target, with identical answers (until the
    /// threat maps are next repainted).
    pub fn raider_reachability(&mut self, war: &MilitaryManager, u: Unit, max_threat: f32) -> Reachability {
        let Some(def) = u.def() else {
            return Reachability::none();
        };
        self.terrain.max_threat = max_threat;
        let cost = if def.move_data().is_none() {
            PathCost::AirRaiderCheck
        } else if def.id() == self.scythe_id {
            PathCost::ScytheCheck
        } else {
            PathCost::RaiderCheck
        };
        self.reachability(war, u, cost)
    }

    /// Batch form of [`is_assault_reachable`](Self::is_assault_reachable);
    /// see [`raider_reachability`](Self::raider_reachability).
    pub fn assault_reachability(&mut self, war: &MilitaryManager, u: Unit, max_threat: f32) -> Reachability {
        let Some(def) = u.def() else {
            return Reachability::none();
        };
        self.terrain.max_threat = max_threat;
        let cost = if def.move_data().is_none() { PathCost::AirCheck } else { PathCost::AssaultCheck };
        self.reachability(war, u, cost)
    }

    /// Batch form of [`is_reachable`](Self::is_reachable); see
    /// [`raider_reachability`](Self::raider_reachability).
    pub fn reachability(&mut self, war: &MilitaryManager, u: Unit, cost: PathCost) -> Reachability {
        let start = u.pos();
        if start == NULLPOS || !self.terrain.in_map(start) {
            return Reachability::none();
        }
        let Some(def) = u.def() else {
            return Reachability::none();
        };
        self.terrain.max_slope = 1.0;
        self.terrain.dive = war.dive(Some(def));
        if let Some(md) = def.move_data() {
            self.terrain.set_movement(def, md);
        }
        let Some(origin) = self.terrain.cell_of(start) else {
            return Reachability::none();
        };
        let terrain = &self.terrain;
        let mut reached = Vec::new();
        self.astar.flood(origin, &mut |c| terrain.cost(cost, war, c), &mut reached);
        Reachability {
            reach: Some(Reach { width: self.astar.width(), origin, reached }),
            mwidth: self.terrain.mwidth,
            mheight: self.terrain.mheight,
            smwidth: self.terrain.smwidth,
        }
    }

    pub fn is_reachable(&mut self, war: &MilitaryManager, u: Unit, target: Vec3, cost: PathCost) -> bool {
        let start = u.pos();
        if start == NULLPOS {
            return false;
        }

        // bounds check, needed because unit positions can sometimes be nonsensical for various reasons.
        if !self.terrain.in_map(start) || !self.terrain.in_map(target) {
            return false;
        }

        let Some(def) = u.def() else {
            return false;
        };
        self.terrain.max_slope = 1.0;
        self.terrain.dive = war.dive(Some(def));
        if let Some(md) = def.move_data() {
            self.terrain.set_movement(def, md);
        }

        // reversed, to return in right order when traversing backwards
        let (Some(start_pos), Some(target_pos)) = (self.terrain.cell_of(target), self.terrain.cell_of(start)) else {
            return false;
        };
        let opts = SearchOpts { pass_target: true, track_best: false, record_path: false };
        let terrain = &self.terrain;
        self.astar.new_query();
        self.astar.search(start_pos, target_pos, opts, &mut Best::new(start_pos), &mut |c| terrain.cost(cost, war, c))
    }

    pub fn is_worker_reachable(&mut self, war: &MilitaryManager, u: Unit, target: Vec3) -> bool {
        let start = u.pos();
        if start == NULLPOS {
            return false;
        }

        // bounds check, needed because unit positions can sometimes be nonsensical for various reasons.
        if !self.terrain.in_map(start) || !self.terrain.in_map(target) {
            return false;
        }

        let Some(def) = u.def() else {
            return false;
        };
        self.terrain.max_slope = 1.0;
        self.terrain.dive = war.dive(Some(def));
        let Some(md) = def.move_data() else {
            return true;
        };
        self.terrain.set_movement(def, md);

        // DON'T reverse start and target since we're getting the closest reachable point.
        let (Some(start_pos), Some(target_pos)) = (self.terrain.cell_of(start), self.terrain.cell_of(target)) else {
            return false;
        };
        let opts = SearchOpts { pass_target: false, track_best: true, record_path: false };
        let mut best = Best::new(0);
        let terrain = &self.terrain;
        self.astar.new_query();
        if self.astar.search(start_pos, target_pos, opts, &mut best, &mut |c| terrain.cost(PathCost::TestPath, war, c))
        {
            return true;
        }
        // take the closest reachable point and see if it's within build range.
        distance(terrain.to_pos(best.pos), target) < def.build_distance()
    }

    /// Terrain-only path cost between two points for a movement type.
    pub fn get_path_cost(&mut self, war: &MilitaryManager, start: Vec3, target: Vec3, ud: UnitDef) -> f32 {
        // bounds check, needed because unit positions can sometimes be nonsensical for various reasons.
        if !self.terrain.in_map(start) || !self.terrain.in_map(target) {
            return -1.0;
        }

        self.terrain.max_slope = 1.0;
        self.terrain.dive = war.dive(Some(ud));
        if let Some(md) = ud.move_data() {
            self.terrain.set_movement(ud, md);
            self.terrain.floater = ud.is_floater();
            self.terrain.amph = self.terrain.max_depth < -1000.0;
        }

        // reversed, to return in right order when traversing backwards
        let (Some(start_pos), Some(target_pos)) = (self.terrain.cell_of(target), self.terrain.cell_of(start)) else {
            return -1.0;
        };
        let opts = SearchOpts { pass_target: true, track_best: true, record_path: false };
        let mut best = Best::new(0);
        let terrain = &self.terrain;
        self.astar.new_query();
        if self.astar.search(start_pos, target_pos, opts, &mut best, &mut |c| terrain.cost(PathCost::TestPath, war, c))
        {
            return self.astar.min_cost(target_pos) * MAP_RES as f32;
        }
        let build_distance = ud.build_distance();
        if distance(terrain.to_pos(best.pos), target) < build_distance {
            return self.astar.min_cost(best.pos) * MAP_RES as f32 + build_distance;
        }
        -1.0
    }
}

impl Terrain {
    /// Movement uniforms shared by every query for ground/sea units.
    fn set_movement(&mut self, def: UnitDef, md: MoveData) {
        self.max_slope = md.max_slope();
        self.max_depth = -def.max_water_depth();
        let min_water_depth = def.min_water_depth();
        self.min_depth = if min_water_depth > 0.0 { -min_water_depth } else { NO_MIN_DEPTH };
    }

    fn in_map(&self, p: Vec3) -> bool {
        !(p.x < 0.0 || p.x > self.mwidth || p.z < 0.0 || p.z > self.mheight)
    }

    /// Grid cell containing `p`, clamped to the grid; `None` for an empty grid.
    fn cell_of(&self, p: Vec3) -> Option<usize> {
        cell_index(self.smwidth, self.slope_map.len(), p)
    }

    fn elevation(&self, p: Vec3) -> f32 {
        self.map.elevation_at(p.x, p.z)
    }

    fn to_pos(&self, cell: usize) -> Vec3 {
        let (x, z) = cell_corner(self.smwidth, cell);
        Vec3::new(x, self.map.elevation_at(x, z), z)
    }

    fn to_pos_raised(&self, cell: usize, height: f32) -> Vec3 {
        let (x, z) = cell_corner(self.smwidth, cell);
        Vec3::new(x, self.map.elevation_at(x, z) + height, z)
    }

    /// Cost multiplier of entering `cell`, or [`IMPASSABLE`].
    fn cost(&self, kind: PathCost, war: &MilitaryManager, cell: usize) -> f32 {
        let slope = self.slope_map[cell];
        let (x, z) = cell_corner(self.smwidth, cell);
        // `getElev(pos)` in the original, from the per-refresh cache.
        let elev = self.elev_map.get(cell).copied().unwrap_or(0.0);
        let pos = Vec3::new(x, elev, z);
        let bad_depth = elev < self.max_depth || elev > self.min_depth;
        let max_slope = self.max_slope;
        // The threat maps as this unit sees them, under water too.
        let war = war.view_dive(self.dive);
        match kind {
            PathCost::TestPath => {
                if (slope > max_slope && (!self.floater || elev > 0.0)) || bad_depth {
                    return IMPASSABLE;
                }
                if self.floater && elev < 0.0 {
                    return 1.0;
                }
                let water_penalty = if self.amph || elev > 0.0 { 0.0 } else { 5.0 };
                1.0 + water_penalty + (slope / max_slope)
            }
            PathCost::AirPath => 1.0 + (30.0 * war.get_aa_threat(pos)),
            PathCost::AirRaiderPath => {
                1.0 + (40.0 * war.get_threat(pos).max(war.get_aa_threat(pos)).max(war.get_riot_threat(pos)))
            }
            // use inverse slope-cost relation for spiders, because they actually benefit from hills!
            PathCost::SpiderRaiderPath => {
                if slope > max_slope || bad_depth {
                    return IMPASSABLE;
                }
                1.0 + (1.0 - slope) + (40.0 * war.get_threat(pos).max(war.get_riot_threat(pos)))
            }
            PathCost::WidowPath => {
                if slope > max_slope || bad_depth {
                    return IMPASSABLE;
                }
                // Cloaked, but found by what bumps into it and hurt by
                // what is fired nearby: wide of enemies and fighting.
                1.0 + (1.0 - slope) + (20.0 * war.get_scythe_threat(pos)) + (20.0 * war.get_threat(pos))
            }
            PathCost::SpiderPath => {
                if slope > max_slope || bad_depth {
                    return IMPASSABLE;
                }
                1.0 + (1.0 - slope) + (10.0 * war.get_threat(pos))
            }
            PathCost::RaiderPath => {
                if slope > max_slope || bad_depth {
                    return IMPASSABLE;
                }
                1.0 + (slope / max_slope) + 40.0 * war.get_threat(pos).max(war.get_riot_threat(pos))
            }
            PathCost::ScythePath => {
                if slope > max_slope || bad_depth {
                    return IMPASSABLE;
                }
                1.0 + (slope / max_slope) + 40.0 * war.get_scythe_threat(pos).max(war.get_riot_threat(pos))
            }
            PathCost::AssaultPath => {
                if slope > max_slope || bad_depth {
                    return IMPASSABLE;
                }
                1.0 + (slope / max_slope) + 10.0 * war.get_threat(pos)
            }
            PathCost::StriderPath => {
                // port: without the probe def, skip the buildability test
                // (the C++ passed a null def to the engine).
                if slope > max_slope
                    || bad_depth
                    || self.blocker.is_some_and(|b| !self.map.is_possible_to_build_at(b, pos, 0))
                {
                    return IMPASSABLE;
                }
                1.0 + (slope / max_slope) + 10.0 * war.get_threat(pos)
            }
            PathCost::AvoidEnemies => {
                if slope > max_slope || bad_depth {
                    return IMPASSABLE;
                }
                1.0 + (slope / max_slope) + 40.0 * war.get_threat(pos)
            }
            PathCost::RaiderCheck => {
                if slope > max_slope || bad_depth {
                    return IMPASSABLE;
                }
                let threat = war.get_threat(pos) * (1.0 + war.get_riot_threat(pos));
                if threat > self.max_threat {
                    return IMPASSABLE;
                }
                1.0 + (slope / max_slope) + 20.0 * threat
            }
            PathCost::ScytheCheck => {
                if slope > max_slope
                    || bad_depth
                    || war.get_riot_threat(pos) > 0.0
                    || (war.get_scythe_threat(pos) > 0.0 && war.get_threat(pos) > self.max_threat)
                {
                    return IMPASSABLE;
                }
                1.0 + (slope / max_slope)
            }
            PathCost::AssaultCheck => {
                if slope > max_slope || bad_depth {
                    return IMPASSABLE;
                }
                let threat = war.get_threat(pos);
                if threat > self.max_threat {
                    return IMPASSABLE;
                }
                1.0 + (slope / max_slope) + 5.0 * threat
            }
            PathCost::AirCheck => {
                let aa_threat = war.get_aa_threat(pos);
                if aa_threat > 1.0 {
                    return IMPASSABLE;
                }
                1.0 + 10.0 * (aa_threat + war.get_threat(pos))
            }
            PathCost::AirRaiderCheck => {
                if war.get_aa_threat(pos) > 0.0 || war.get_riot_threat(pos) > 0.0 {
                    return IMPASSABLE;
                }
                let threat = war.get_threat(pos);
                if threat > self.max_threat {
                    return IMPASSABLE;
                }
                1.0 + 10.0 * threat
            }
        }
    }
}

/// Precomputed answers to reachability checks from one unit's position;
/// see [`Pathfinder::reachability`].
pub struct Reachability {
    reach: Option<Reach>,
    mwidth: f32,
    mheight: f32,
    smwidth: i32,
}

impl Reachability {
    fn none() -> Self {
        Reachability { reach: None, mwidth: 0.0, mheight: 0.0, smwidth: 0 }
    }

    /// Same answer as the corresponding single-target check for `target`.
    pub fn reaches(&self, target: Vec3) -> bool {
        let Some(r) = &self.reach else {
            return false;
        };
        if target.x < 0.0 || target.x > self.mwidth || target.z < 0.0 || target.z > self.mheight {
            return false;
        }
        cell_index(self.smwidth, r.reached.len(), target).is_some_and(|t| r.reaches(t))
    }
}

/// Grid cell of a world position for a grid of `len` cells in rows of
/// `width`. `None` for an empty grid.
fn cell_index(width: i32, len: usize, p: Vec3) -> Option<usize> {
    let rows = len as i64 / width.max(1) as i64;
    if width <= 0 || rows <= 0 {
        return None;
    }
    // port: clamp to the grid. Positions exactly on the far map edge pass the
    // bounds checks but mapped one cell past it in the original (out of
    // bounds or wrapped to the next row).
    let col = ((p.x / MAP_RES as f32) as i64).clamp(0, width as i64 - 1);
    let row = ((p.z / MAP_RES as f32) as i64).clamp(0, rows - 1);
    Some((row * width as i64 + col) as usize)
}

/// Fill `out` (for a grid of `len` cells in rows of `width`) with
/// [`KEEPOUT_COST`] for the cells whose corner lies in a circle of
/// `keepout`, zero elsewhere; empty without circles.
fn paint_keepout(width: i32, len: usize, keepout: &[(Vec3, f32)], out: &mut Vec<f32>) {
    out.clear();
    if keepout.is_empty() {
        return;
    }
    out.resize(len, 0.0);
    let width = width.max(1);
    let rows = len as i32 / width;
    let res = MAP_RES as f32;
    for &(c, r) in keepout {
        let (x0, x1) = (((c.x - r) / res).ceil() as i32, ((c.x + r) / res).floor() as i32);
        let (z0, z1) = (((c.z - r) / res).ceil() as i32, ((c.z + r) / res).floor() as i32);
        for z in z0.max(0)..=z1.min(rows - 1) {
            for x in x0.max(0)..=x1.min(width - 1) {
                let (dx, dz) = (x as f32 * res - c.x, z as f32 * res - c.z);
                if dx * dx + dz * dz <= r * r {
                    out[(z * width + x) as usize] = KEEPOUT_COST;
                }
            }
        }
    }
}

/// World x/z of a cell's top-left corner.
fn cell_corner(width: i32, cell: usize) -> (f32, f32) {
    let w = width.max(1) as usize;
    ((MAP_RES as usize * (cell % w)) as f32, (MAP_RES as usize * (cell / w)) as f32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cell_mapping() {
        // 4 x 3 grid (256 x 192 elmos).
        assert_eq!(cell_index(4, 12, Vec3::new(0.0, 0.0, 0.0)), Some(0));
        assert_eq!(cell_index(4, 12, Vec3::new(130.0, 0.0, 70.0)), Some(4 + 2));
        // Far edges clamp instead of overflowing into the next row / past the end.
        assert_eq!(cell_index(4, 12, Vec3::new(256.0, 0.0, 0.0)), Some(3));
        assert_eq!(cell_index(4, 12, Vec3::new(256.0, 0.0, 192.0)), Some(11));
        assert_eq!(cell_index(0, 12, Vec3::ZERO), None);
        assert_eq!(cell_index(4, 3, Vec3::ZERO), None);
        assert_eq!(cell_corner(4, 6), (128.0, 64.0));
    }

    #[test]
    fn keepout_cells() {
        // 4 x 3 grid (256 x 192 elmos): corners within 64 of (64, 64).
        let mut out = Vec::new();
        paint_keepout(4, 12, &[(Vec3::xz(64.0, 64.0), 64.0)], &mut out);
        let marked: Vec<usize> = (0..12).filter(|&c| out[c] > 0.0).collect();
        assert_eq!(marked, vec![1, 4, 5, 6, 9]);
        // Circles past the edges clamp; none leave the buffer empty.
        paint_keepout(4, 12, &[(Vec3::xz(-50.0, 150.0), 60.0)], &mut out);
        assert_eq!((0..12).filter(|&c| out[c] > 0.0).collect::<Vec<_>>(), vec![8]);
        paint_keepout(4, 12, &[], &mut out);
        assert!(out.is_empty());
    }
}
