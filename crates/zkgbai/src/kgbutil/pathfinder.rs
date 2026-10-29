//! Threat-aware A* pathfinder over a downsampled slope map.

mod astar;

use std::collections::{BTreeMap, VecDeque};

use springai::{Callback, Map, MoveData, Unit, UnitDef, Vec3};

use self::astar::{AStar, Best, IMPASSABLE, Links, Reach, SearchOpts, thin_waypoints};
use super::HeightMap;
use crate::defs::Defs;
use crate::military::MilitaryManager;
use crate::military::jumps::{JumpDef, arc_clear};
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

/// A jump link's cost beyond its length, in steps: the stop to take off
/// and the landing.
const JUMP_STEPS: f32 = 3.0;

/// Jump links reach this much short of the jump: units take off from
/// near a link's start, not on it.
const TAKEOFF_SLACK: f32 = 48.0;

/// A squad member still on the near side follows its leader's jump for
/// this long after the leader took it, frames.
const FOLLOW_JUMP: i32 = 30 * 15;

/// What jump links depend on: the movement's slope and depths, the jump's
/// reach and height (as bits).
type JumpKey = [u32; 5];

/// The jump on a unit's way. The way the unit was last given ends where it
/// takes off; the jump module jumps it from there to the landing once it
/// can (see `military::jumps`).
#[derive(Clone, Copy, Debug)]
pub struct JumpPlan {
    pub unit: Unit,
    /// Take-off and landing.
    pub from: Vec3,
    pub to: Vec3,
    /// On along the way from the landing (the landing, if the way jumps
    /// again from there).
    pub then: Vec3,
    /// When planned (or taken).
    pub frame: i32,
    /// Another unit's way: its squad leader's.
    pub shared: bool,
    key: JumpKey,
    cells: (usize, usize),
}

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
    /// Circles (centre, radius) every path keeps out of for now, ending at
    /// their rim where it would end inside (see [`Pathfinder::no_go_exit`]):
    /// the blasts of missiles of ours and our allies' in flight (see
    /// `military::blasts`).
    pub no_go: Vec<(Vec3, f32)>,
    /// Whether jumpers' ways may jump, and the frame (the jump module sets
    /// both).
    pub jumps_on: bool,
    pub now: i32,
    /// The next query walks: the way of a squad with members that cannot
    /// jump.
    pub walk_only: bool,
    /// The ground, for jump arcs.
    heights: HeightMap,
    /// Jump links by movement and jumps, built when first wanted after the
    /// ground last changed.
    jump_links: Vec<(JumpKey, Links)>,
    jumpers: BTreeMap<i32, Option<JumpDef>>,
    /// The jump on each unit's last way, and the last one each took.
    pub jump_plans: BTreeMap<i32, JumpPlan>,
    jumped: BTreeMap<i32, JumpPlan>,
    /// Jump links built since the jump module last looked: def, links, ms.
    pub jump_link_builds: Vec<(String, usize, f32)>,
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
            no_go: Vec::new(),
            jumps_on: true,
            now: 0,
            walk_only: false,
            heights: HeightMap::new(cb),
            jump_links: Vec::new(),
            jumpers: BTreeMap::new(),
            jump_plans: BTreeMap::new(),
            jumped: BTreeMap::new(),
            jump_link_builds: Vec::new(),
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
        let heights = HeightMap::new(self.cb);
        if heights != self.heights {
            self.heights = heights;
            self.jump_links.clear();
        }
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
    /// (circles of centre and radius, elmos) and to [`no_go`](Self::no_go):
    /// a cell whose corner lies in one costs [`KEEPOUT_COST`] more, so the
    /// path keeps out of them where any way round exists, and leaves them
    /// by the shortest way where it starts in one. A target in a no-go
    /// circle gives way to its rim, on the unit's side.
    pub fn find_path_keepout(
        &mut self,
        war: &MilitaryManager,
        u: Unit,
        target: Vec3,
        cost: PathCost,
        keepout: &[(Vec3, f32)],
    ) -> VecDeque<Vec3> {
        self.find_path_inner(war, u, target, cost, keepout, None)
    }

    /// The first jump on a squad's route with optional, currently ready
    /// equipment. The caller drives the throw; no intrinsic jump order is
    /// left on the passenger, which cannot jump by itself.
    pub(crate) fn equipped_jump(
        &mut self,
        war: &MilitaryManager,
        u: Unit,
        target: Vec3,
        cost: PathCost,
        jump: JumpDef,
    ) -> Option<JumpPlan> {
        self.find_path_inner(war, u, target, cost, &[], Some(jump));
        self.jump_plans.remove(&u.id())
    }

    fn find_path_inner(
        &mut self,
        war: &MilitaryManager,
        u: Unit,
        target: Vec3,
        cost: PathCost,
        keepout: &[(Vec3, f32)],
        equipment: Option<JumpDef>,
    ) -> VecDeque<Vec3> {
        let start = u.pos();
        let mut result = VecDeque::new();
        let walk_only = std::mem::take(&mut self.walk_only);
        let jump_plan = self.jump_plans.remove(&u.id());

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
        // Keep a squad leader's jump while the unit still follows it.
        if let Some(p) = jump_plan.filter(|p| p.shared) {
            self.jump_plans.insert(u.id(), p);
        }
        let target = self.no_go_exit(target, start).unwrap_or(target);
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

        let circles: Vec<(Vec3, f32)>;
        let keepout = if self.no_go.is_empty() {
            keepout
        } else {
            circles = keepout.iter().chain(&self.no_go).copied().collect();
            &circles
        };
        paint_keepout(self.terrain.smwidth, self.terrain.slope_map.len(), keepout, &mut self.keepout);
        // Jumpers' ways jump what they cannot walk (see `jump_links`), the
        // dearer the longer their jump has yet to reload.
        let jumps = if flyer || walk_only || !self.jumps_on { None } else { equipment.or_else(|| self.jumper(def)) };
        let links = jumps.map(|jd| (self.jump_links_for(jd, def), jump_wait(u, jd, def)));
        let (terrain, extra) = (&self.terrain, &self.keepout);
        let links = links.map(|(i, wait)| (&self.jump_links[i].1, wait));
        let mut cell_cost = |c: usize| match terrain.cost(cost, war, c) {
            IMPASSABLE => IMPASSABLE,
            base => base + extra.get(c).copied().unwrap_or(0.0),
        };
        match self.astar.find_path(start_pos, target_pos, &mut cell_cost, links) {
            // Nothing closer to the target is reachable: head straight for it.
            None => result.push_back(target),
            Some(end) => {
                self.astar.trace(start_pos, end, &mut self.path_buf);
                // A jump on the way: the way ends where it takes off, so the
                // unit is never sent past it to walk round (the engine's
                // own pathing knows no jumps).
                let path = &self.path_buf;
                let w = terrain.smwidth.max(1) as usize;
                let step = |a: usize, b: usize| (a % w).abs_diff(b % w) <= 1 && (a / w).abs_diff(b / w) <= 1;
                let jump_at = (0..path.len()).find(|&i| !step(if i == 0 { start_pos } else { path[i - 1] }, path[i]));
                for cell in thin_waypoints(&path[..jump_at.unwrap_or(path.len())]) {
                    result.push_back(if flyer { terrain.to_pos_raised(cell, flyheight) } else { terrain.to_pos(cell) });
                }
                match (jump_at, jumps) {
                    (Some(i), Some(jd)) => {
                        let (a, b) = (if i == 0 { start_pos } else { path[i - 1] }, path[i]);
                        let (from, to) = (terrain.centre(a), terrain.centre(b));
                        // Walking on from the landing, as far as two steps.
                        let mut then = b;
                        for &c in path[i + 1..].iter().take(2) {
                            if !step(then, c) {
                                break;
                            }
                            then = c;
                        }
                        let then = if then == b { to } else { terrain.to_pos(then) };
                        let key = jump_key(terrain, jd);
                        let frame = self.now;
                        let plan = JumpPlan { unit: u, from, to, then, frame, shared: false, key, cells: (a, b) };
                        self.jump_plans.insert(u.id(), plan);
                        result.push_back(from);
                    }
                    _ => result.push_back(target),
                }
            }
        }
        result
    }

    /// The jumps of def `ud`, if it jumps.
    fn jumper(&mut self, ud: UnitDef) -> Option<JumpDef> {
        *self.jumpers.entry(ud.id()).or_insert_with(|| JumpDef::of(ud))
    }

    /// Whether `u`'s ways may jump.
    pub fn can_jump(&mut self, u: Unit) -> bool {
        self.jumps_on && u.def().is_some_and(|d| d.move_data().is_some() && self.jumper(d).is_some())
    }

    /// The jump links of the movement set (see `Terrain::set_movement`)
    /// and of `jd`, built if not yet: their index.
    fn jump_links_for(&mut self, jd: JumpDef, def: UnitDef) -> usize {
        let t = &self.terrain;
        let key = jump_key(t, jd);
        if let Some(i) = self.jump_links.iter().position(|l| l.0 == key) {
            return i;
        }
        let begun = std::time::Instant::now();
        let passable: Vec<bool> = (0..t.slope_map.len())
            .map(|c| {
                let elev = t.elev_map.get(c).copied().unwrap_or(0.0);
                t.slope_map[c] <= t.max_slope && elev >= t.max_depth && elev <= t.min_depth
            })
            .collect();
        let hm = &self.heights;
        let reach = jd.reach() - TAKEOFF_SLACK;
        let links = jump_links(t.smwidth.max(0) as usize, &passable, |x, z| hm.height_at(x, z), reach, jd.height);
        self.jump_link_builds.push((def.name(), links.len(), begun.elapsed().as_secs_f32() * 1000.0));
        self.jump_links.push((key, links));
        self.jump_links.len() - 1
    }

    /// Give squad member `member` its leader's jump: the one on the
    /// leader's way, or the one the leader just took while the member is
    /// still on its near side; none if the member cannot jump.
    pub fn share_jump(&mut self, leader: Unit, member: Unit) {
        if member.id() == leader.id() {
            return;
        }
        let pos = member.pos();
        let took = (self.jumped.get(&leader.id()))
            .filter(|p| self.now - p.frame <= FOLLOW_JUMP && distance(pos, p.from) < distance(pos, p.to));
        let plan = self.jump_plans.get(&leader.id()).filter(|p| !p.shared).or(took).copied();
        match plan.filter(|_| self.can_jump(member)) {
            Some(p) => {
                self.jump_plans.insert(member.id(), JumpPlan { unit: member, frame: self.now, shared: true, ..p });
            }
            None => {
                if self.jump_plans.get(&member.id()).is_some_and(|p| p.shared) {
                    self.jump_plans.remove(&member.id());
                }
            }
        }
    }

    /// `plan`'s jump was ordered: off the plans, kept for the unit's squad.
    pub fn jump_taken(&mut self, plan: &JumpPlan) {
        let id = plan.unit.id();
        self.jump_plans.remove(&id);
        self.jumped.insert(id, JumpPlan { frame: self.now, ..*plan });
        let now = self.now;
        self.jumped.retain(|_, p| now - p.frame <= FOLLOW_JUMP);
    }

    /// `plan`'s jump does not go from its take-off: no way jumps it again
    /// (until the ground changes). Its link's cells.
    pub fn drop_jump(&mut self, plan: &JumpPlan) -> (Vec3, Vec3) {
        for (_, links) in self.jump_links.iter_mut().filter(|l| l.0 == plan.key) {
            links.drop_edge(plan.cells.0, plan.cells.1);
        }
        self.jump_plans.remove(&plan.unit.id());
        (plan.from, plan.to)
    }

    /// The no-go circle `p` is in, if any.
    pub fn no_go_at(&self, p: Vec3) -> Option<(Vec3, f32)> {
        self.no_go.iter().copied().find(|&(c, r)| distance(c, p) <= r)
    }

    /// Whether `p` is in a no-go circle or within `margin` of one.
    pub fn no_go_near(&self, p: Vec3, margin: f32) -> bool {
        self.no_go.iter().any(|&(c, r)| distance(c, p) <= r + margin)
    }

    /// Where to stand clear of the no-go circle `p` is in (`None` when in
    /// none): just past its rim on `side`'s side of its centre, or as near
    /// that as the map and the other circles allow.
    pub fn no_go_exit(&self, p: Vec3, side: Vec3) -> Option<Vec3> {
        let (c, r) = self.no_go_at(p)?;
        let (w, h) = (self.terrain.mwidth, self.terrain.mheight);
        let on_map = |q: Vec3| {
            q.x >= MAP_RES as f32 && q.z >= MAP_RES as f32 && q.x <= w - MAP_RES as f32 && q.z <= h - MAP_RES as f32
        };
        let out = rim_points(c, r + MAP_RES as f32, side);
        let q = (out.iter().copied()).find(|&q| on_map(q) && self.no_go_at(q).is_none()).unwrap_or(out[0]);
        Some(Vec3::new(q.x, self.terrain.map.elevation_at(q.x, q.z), q.z))
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

    /// The middle of `cell`, on the ground.
    fn centre(&self, cell: usize) -> Vec3 {
        let (x, z) = cell_corner(self.smwidth, cell);
        let (x, z) = (x + MAP_RES as f32 / 2.0, z + MAP_RES as f32 / 2.0);
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

/// What `jd`'s links depend on with the movement set in `t`.
fn jump_key(t: &Terrain, jd: JumpDef) -> JumpKey {
    [t.max_slope, t.max_depth, t.min_depth, jd.reach(), jd.height].map(f32::to_bits)
}

/// Steps of walking `u` would do while its jump reloads.
fn jump_wait(u: Unit, jd: JumpDef, def: UnitDef) -> f32 {
    let ready = if u.rules_param_float("is_jumping", 0.0) == 1.0 {
        0.0
    } else {
        u.rules_param_float("jumpReload", 1.0).clamp(0.0, 1.0)
    };
    (1.0 - ready) * jd.reload as f32 / 30.0 * def.speed() / MAP_RES as f32
}

/// Jump links over ground a unit cannot walk, for a grid of `passable`
/// cells in rows of `width`: from each passable cell beside an impassable
/// one to each other such cell within `reach` that the straight way to
/// crosses impassable cells, where a jump `height` high between their
/// middles clears the `ground`; each costs its length in steps and
/// [`JUMP_STEPS`].
fn jump_links(width: usize, passable: &[bool], ground: impl Fn(f32, f32) -> f32, reach: f32, height: f32) -> Links {
    let len = passable.len();
    let rows = len / width.max(1);
    let res = MAP_RES as f32;
    let blocked = |x: isize, z: isize| {
        x >= 0 && z >= 0 && (x as usize) < width && (z as usize) < rows && !passable[z as usize * width + x as usize]
    };
    let rim: Vec<bool> = (0..rows * width)
        .map(|c| {
            let (x, z) = ((c % width) as isize, (c / width) as isize);
            passable[c] && (-1..=1).any(|dz| (-1..=1).any(|dx| blocked(x + dx, z + dz)))
        })
        .collect();
    let middle = |x: isize, z: isize| {
        let (x, z) = ((x as f32 + 0.5) * res, (z as f32 + 0.5) * res);
        Vec3::new(x, ground(x, z), z)
    };
    let r = (reach / res).floor() as isize;
    let mut list = Vec::new();
    for a in (0..rim.len()).filter(|&c| rim[c]) {
        let (ax, az) = ((a % width) as isize, (a / width) as isize);
        let from = middle(ax, az);
        for bz in (az - r).max(0)..=(az + r).min(rows as isize - 1) {
            for bx in (ax - r).max(0)..=(ax + r).min(width as isize - 1) {
                let b = bz as usize * width + bx as usize;
                let (dx, dz) = ((bx - ax) as f32, (bz - az) as f32);
                let steps = dx.hypot(dz);
                if !rim[b] || steps < 1.5 || steps * res > reach {
                    continue;
                }
                let n = (steps * 4.0).ceil() as i32;
                let over = (1..n).any(|k| {
                    let t = k as f32 / n as f32;
                    blocked((ax as f32 + dx * t).round() as isize, (az as f32 + dz * t).round() as isize)
                });
                if over && arc_clear(&ground, height, from, middle(bx, bz)) {
                    list.push((a, b, steps + JUMP_STEPS));
                }
            }
        }
    }
    Links::new(len, list)
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

/// Points `r` from `c` on the ground plane: first toward `side`, then
/// turning from it either way by sixteenths of a turn.
fn rim_points(c: Vec3, r: f32, side: Vec3) -> [Vec3; 16] {
    let toward = (side.z - c.z).atan2(side.x - c.x);
    std::array::from_fn(|k| {
        let turn = (k as i32 + 1) / 2 * if k % 2 == 1 { 1 } else { -1 };
        let a = toward + turn as f32 * std::f32::consts::PI / 8.0;
        Vec3::xz(c.x + r * a.cos(), c.z + r * a.sin())
    })
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

    #[test]
    fn rim_points_turn_from_the_side() {
        let c = Vec3::xz(1000.0, 1000.0);
        let near = |a: Vec3, b: Vec3| distance(a, b) < 0.5;
        let at = |a: f32| Vec3::xz(c.x + 100.0 * a.cos(), c.z + 100.0 * a.sin());
        let p = rim_points(c, 100.0, Vec3::xz(1500.0, 1000.0));
        assert!(p.iter().all(|&q| (distance(q, c) - 100.0).abs() < 0.5));
        // Toward the side first, then either way of it, ending opposite.
        assert!(near(p[0], Vec3::xz(1100.0, 1000.0)));
        assert!(near(p[1], at(std::f32::consts::PI / 8.0)));
        assert!(near(p[2], at(-std::f32::consts::PI / 8.0)));
        assert!(near(p[15], Vec3::xz(900.0, 1000.0)));
        // A side on the centre itself still gives points.
        assert!(rim_points(c, 100.0, c).iter().all(|q| q.x.is_finite() && q.z.is_finite()));
    }

    /// A 20 x 10 cell map (1280 x 640 elmos): low ground west, a plateau
    /// `top` high east of a cliff at cells x = 10 and 11, walkable only on
    /// the ramp at z = 0.
    fn cliff(top: f32) -> (Vec<bool>, impl Fn(f32, f32) -> f32) {
        let passable = (0..200).map(|c| !(10..=11).contains(&(c % 20)) || c / 20 == 0).collect();
        let ground = move |x: f32, _z: f32| top * ((x - 640.0) / 128.0).clamp(0.0, 1.0);
        (passable, ground)
    }

    #[test]
    fn jump_links_cross_cliffs() {
        let at = |x: usize, z: usize| z * 20 + x;
        let (passable, ground) = cliff(150.0);
        let links = jump_links(20, &passable, ground, 320.0, 200.0);
        // Up and down the cliff, straight across and slanting.
        for (a, b) in [(at(9, 5), at(12, 5)), (at(12, 5), at(9, 5)), (at(9, 3), at(12, 6))] {
            let l = links.of(a).iter().find(|e| e.0 as usize == b);
            assert!(l.is_some_and(|e| (e.1 - (distance_steps(a, b) + JUMP_STEPS)).abs() < 1e-4), "{a} -> {b}");
        }
        // Not along the cliff (no impassable cell on the way), not beyond
        // reach, not from off the rim.
        for (a, b) in [(at(9, 5), at(9, 8)), (at(9, 5), at(14, 5)), (at(8, 5), at(12, 5))] {
            assert!(links.of(a).iter().all(|e| e.0 as usize != b), "{a} -> {b}");
        }

        // A ridge too high to clear: no links.
        let passable: Vec<bool> = (0..200).map(|c| !(10..=11).contains(&(c % 20))).collect();
        let ridge = |x: f32, _z: f32| if (640.0..768.0).contains(&x) { 600.0 } else { 0.0 };
        assert_eq!(jump_links(20, &passable, ridge, 320.0, 200.0).len(), 0);
    }

    fn distance_steps(a: usize, b: usize) -> f32 {
        ((a % 20) as f32 - (b % 20) as f32).hypot((a / 20) as f32 - (b / 20) as f32)
    }

    #[test]
    fn ways_jump_cliffs_instead_of_walking_round() {
        let (passable, ground) = cliff(150.0);
        let links = jump_links(20, &passable, ground, 320.0, 200.0);
        let mut cost = |c: usize| if passable[c] { 1.0 } else { IMPASSABLE };
        let (start, target) = (5 * 20 + 2, 5 * 20 + 17);
        let mut a = AStar::new(20, 200);
        let mut way = Vec::new();
        let step = |a: usize, b: usize| (a % 20).abs_diff(b % 20) <= 1 && (a / 20).abs_diff(b / 20) <= 1;
        // Walking, round by the ramp at z = 0.
        assert_eq!(a.find_path(start, target, &mut cost, None), Some(target));
        a.trace(start, target, &mut way);
        assert!(way.iter().any(|&c| c / 20 == 0) && way.windows(2).all(|w| step(w[0], w[1])), "{way:?}");
        // Jumping: one leap over the cliff, no ramp.
        assert_eq!(a.find_path(start, target, &mut cost, Some((&links, 0.0))), Some(target));
        a.trace(start, target, &mut way);
        assert_eq!(way.windows(2).filter(|w| !step(w[0], w[1])).count(), 1, "{way:?}");
        assert!(way.iter().all(|&c| c / 20 > 1), "{way:?}");
        // With the jump reloading the whole way round: walking again.
        assert_eq!(a.find_path(start, target, &mut cost, Some((&links, 40.0))), Some(target));
        a.trace(start, target, &mut way);
        assert!(way.windows(2).all(|w| step(w[0], w[1])), "{way:?}");
    }
}
