//! The metal-spot graph: Delaunay links between metal spots, ownership and
//! fog-of-war inference over them, territory maps and the overdrive grid.

mod ascii;
mod link;
mod metal_spot;
mod pylon;
#[cfg(test)]
mod tests;
mod triangulation;

pub use link::Link;
pub use metal_spot::MetalSpot;
pub use pylon::Pylon;

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use springai::{Callback, Unit, UnitDef, Vec3};

use crate::Ai;
use crate::kgbutil::ByteArrayGraphics;
use crate::los::LosManager;
use crate::util::{NULLPOS, distance, get_directional_point};

use ascii::Canvas;
use triangulation::delaunay_edges;

/// Written by the `kgbgraph` debug command, relative to the engine's working
/// directory.
const ASCII_GRAPH_FILE: &str = "zkgbai_graph.txt";

/// Sentinel "infinite" distance of the original's nearest-pylon searches.
const FAR: f32 = 65535.0;

/// One metal spot as announced by the game.
#[derive(Clone, Copy, Debug, PartialEq)]
struct SpotData {
    x: f32,
    y: f32,
    z: f32,
    metal: f32,
}

/// Territory grid cell (64 elmos square) of a position.
fn grid_coords(pos: Vec3) -> (i32, i32) {
    ((pos.x / 64.0).round() as i32, (pos.z / 64.0).round() as i32)
}

/// `x/z` formatted like C++ `std::to_string(float)` (`%f`), which is what the
/// start box gadget's AI call-ins were fed by the original.
fn lua_pos(pos: Vec3) -> String {
    format!("{:.6}/{:.6}", pos.x, pos.z)
}

/// Parse a `METAL_SPOTS:` payload: a JSON array of objects with numeric `x`,
/// `y`, `z` and `metal`. Entries lacking any of these are skipped. The error
/// is the complete log message.
fn parse_metal_spots_json(json: &str) -> Result<Vec<SpotData>, String> {
    let val: serde_json::Value =
        serde_json::from_str(json).map_err(|e| format!("Failed parsing JSON metalmap: {e}"))?;
    let Some(entries) = val.as_array() else {
        return Err("Failed parsing JSON metalmap (invalid result type)".to_owned());
    };
    let mut data = Vec::new();
    for entry in entries {
        let Some(obj) = entry.as_object() else { continue };
        let num = |k: &str| obj.get(k).and_then(serde_json::Value::as_f64).map(|v| v as f32);
        if let (Some(x), Some(y), Some(z), Some(metal)) = (num("x"), num("y"), num("z"), num("metal")) {
            data.push(SpotData { x, y, z, metal });
        }
    }
    Ok(data)
}

/// `distance / bp`, treating no build power as infinitely far.
fn per_bp(distance: f32, bp: f32) -> f32 {
    if bp > 0.0 { distance / bp } else { f32::INFINITY }
}

pub struct GraphManager {
    cb: Callback,
    pub my_team_id: i32,
    pub territory_fraction: f32,
    pub eminent_territory: bool,
    pub is_water_map: bool,
    /// Pylon-ish unit-def name -> grid link radius.
    pub pylon_defs: BTreeMap<String, i32>,
    /// All metal spots; a spot's `index` is its position in this vector.
    pub metal_spots: Vec<MetalSpot>,
    /// All graph edges; a link's `index` is its position in this vector.
    pub links: Vec<Link>,
    /// Per link: whether ground forces travel directly between its spots
    /// (no cliff or water in between; see `Ai::terrain_classify_links`).
    /// Territory shadows spread along these links only. Empty until the
    /// terrain model is ready, when every link counts.
    pub ground_links: Vec<bool>,
    /// Energy-grid buildings by unit id.
    pub pylons: BTreeMap<i32, Pylon>,
    /// Width + height above 1536 heightmap squares.
    pub big_map: bool,
    mex_def_id: i32,
    /// `factorygunship`: centers and havens are snapped to its build sites.
    center_def: Option<UnitDef>,
    front_line_graphics: ByteArrayGraphics,
    ally_territory_graphics: ByteArrayGraphics,
    enemy_territory_graphics: ByteArrayGraphics,
    graph_initialized: bool,
    ally_center: Vec3,
    enemy_center: Vec3,
    map_center: Vec3,
    ally_base: Vec3,
    start_pos: Vec3,
}

impl GraphManager {
    pub fn new(cb: Callback) -> Self {
        let map = cb.map();
        let mut gm = GraphManager::with_map_size(cb, map.width(), map.height());
        gm.mex_def_id = cb.unit_def_by_name("staticmex").map_or(-1, |d| d.id());
        gm.center_def = cb.unit_def_by_name("factorygunship");

        for ud in cb.unit_defs() {
            if ud.id() == gm.mex_def_id {
                continue;
            }
            if let Some(range) = ud.custom_param("pylonrange") {
                let name = ud.name();
                match range.trim().parse::<f32>() {
                    Ok(r) => {
                        gm.pylon_defs.insert(name, r as i32);
                    }
                    Err(_) => cb.log(&format!("GraphManager: ignoring bad pylonrange '{range}' of {name}")),
                }
            }
        }

        cb.log("GraphManager: Parsing metal spots...");
        let grp_mexes = gm.parse_metal_spots_grp();
        if !grp_mexes.is_empty() {
            gm.initialize_graph(&grp_mexes);
        }
        gm
    }

    /// An empty graph for a map of `w` x `h` heightmap squares; makes no
    /// engine calls.
    fn with_map_size(cb: Callback, w: i32, h: i32) -> Self {
        GraphManager {
            cb,
            my_team_id: -1,
            territory_fraction: 0.0,
            eminent_territory: false,
            is_water_map: false,
            pylon_defs: BTreeMap::new(),
            metal_spots: Vec::new(),
            links: Vec::new(),
            ground_links: Vec::new(),
            pylons: BTreeMap::new(),
            big_map: w + h > 1536,
            mex_def_id: -1,
            center_def: None,
            front_line_graphics: ByteArrayGraphics::new(w / 8, h / 8),
            ally_territory_graphics: ByteArrayGraphics::new(w / 8, h / 8),
            enemy_territory_graphics: ByteArrayGraphics::new(w / 8, h / 8),
            graph_initialized: false,
            ally_center: NULLPOS,
            enemy_center: NULLPOS,
            map_center: Vec3::new((w * 4) as f32, 0.0, (h * 4) as f32),
            ally_base: NULLPOS,
            start_pos: NULLPOS,
        }
    }

    /// Metal spots published by the game as rules params (`mex_count`,
    /// `mex_x<i>`, ...).
    fn parse_metal_spots_grp(&self) -> Vec<SpotData> {
        let g = self.cb.game();
        let num_spots = g.rules_param_float("mex_count", 0.0) as i32;
        if num_spots == 0 {
            return Vec::new();
        }
        self.cb.log(&format!("GraphManager: Detected {num_spots} metal spots in GRP"));
        if num_spots < 0 {
            return Vec::new();
        }
        (1..=num_spots)
            .map(|i| SpotData {
                x: g.rules_param_float(&format!("mex_x{i}"), 0.0),
                y: g.rules_param_float(&format!("mex_y{i}"), 0.0),
                z: g.rules_param_float(&format!("mex_z{i}"), 0.0),
                metal: g.rules_param_float(&format!("mex_metal{i}"), 0.0),
            })
            .collect()
    }

    /// Lua message: `METAL_SPOTS:<json>` builds the graph unless the rules
    /// params already did.
    fn lua_message(&mut self, in_data: &str) {
        let Some(json) = in_data.strip_prefix("METAL_SPOTS:") else { return };
        self.cb.log("Trying to parse JSON metalmap");
        let data = match parse_metal_spots_json(json) {
            Ok(data) => data,
            Err(msg) => {
                self.cb.log(&msg);
                return;
            }
        };
        self.cb.log(&format!("Parsed JSON metalmap with {} spots", data.len()));
        if !data.is_empty() && !self.graph_initialized {
            self.initialize_graph(&data);
        }
    }

    fn initialize_graph(&mut self, data: &[SpotData]) {
        let cb = self.cb;
        cb.log("GraphManager: Initializing graph");
        self.build_graph(data, &mut |s| cb.log(s));
        self.do_initial_inference();
        self.graph_initialized = true;
    }

    /// Add `data` as metal spots and link them by Delaunay triangulation.
    /// Engine-free apart from `log`.
    fn build_graph(&mut self, data: &[SpotData], log: &mut dyn FnMut(&str)) {
        // Like the original, the triangulation works on truncated integer
        // (x, z) keys: a later spot with the same key replaces an earlier one
        // there, which then stays unlinked.
        let mut mexes: BTreeMap<(i32, i32), usize> = BTreeMap::new();
        for s in data {
            let index = self.metal_spots.len();
            self.metal_spots.push(MetalSpot::new(index, s.x, s.y, s.z, s.metal));
            mexes.insert((s.x as i32, s.z as i32), index);
        }

        log("GraphManager: Constructing edges");
        let points: Vec<(f64, f64)> = mexes.keys().map(|&(x, z)| (x as f64, z as f64)).collect();
        let spot_of: Vec<usize> = mexes.values().copied().collect();
        let edges = delaunay_edges(&points);

        log("GraphManager: Storing recorded graph");
        let mut linked = vec![false; self.metal_spots.len()];
        for (a, b) in edges {
            let (m0, m1) = (spot_of[a], spot_of[b]);
            linked[m0] = true;
            linked[m1] = true;
            let index = self.links.len();
            self.links.push(Link::new(index, m0, m1, self.metal_spots[m0].position, self.metal_spots[m1].position));
            self.metal_spots[m0].links.push(index);
            self.metal_spots[m1].links.push(index);
        }
        for (&(x, z), &spot) in &mexes {
            if !linked[spot] {
                log(&format!("GraphManager: Failed to link mex @ {x},{z}"));
            }
        }
    }

    /// Initial guesses from the start boxes, and water map detection.
    fn do_initial_inference(&mut self) {
        let map = self.cb.map();
        for i in 0..self.metal_spots.len() {
            let pos = self.metal_spots[i].position;
            if !self.is_water_map && map.elevation_at(pos.x, pos.z) < 10.0 {
                self.is_water_map = true;
            }

            let checkpos = format!("ai_is_valid_startpos:{}", lua_pos(pos));
            if self.cb.call_lua_rules(&checkpos).as_deref() == Some("1") {
                self.metal_spots[i].ally_shadowed = true;
            }

            let checkpos = format!("ai_is_valid_enemy_startpos:{}", lua_pos(pos));
            if self.cb.call_lua_rules(&checkpos).as_deref() == Some("1") {
                self.set_hostile(i);
                self.set_neutral(i);
            }
        }
        self.calc_ally_base();
    }

    /// The center of the starting territory, for placing superweapons and
    /// things.
    fn calc_ally_base(&mut self) {
        let ally_spots: Vec<Vec3> = self.get_ally_territory().iter().map(|ms| ms.position).collect();
        if !ally_spots.is_empty() {
            for pos in &ally_spots {
                self.ally_base.x += pos.x;
                self.ally_base.z += pos.z;
            }
            self.ally_base.x /= ally_spots.len() as f32;
            self.ally_base.z /= ally_spots.len() as f32;
        }
    }

    /// Module event: refresh spot states from LOS every 60 frames.
    fn update(&mut self, frame: i32, los: &LosManager) {
        if frame % 60 > 0 {
            return;
        }

        for i in 0..self.metal_spots.len() {
            let pos = self.metal_spots[i].position;
            if los.is_in_los(pos) {
                self.metal_spots[i].last_seen = frame;
                self.metal_spots[i].visible = true;

                let mut has_mex = false;
                if self.cb.friendly_units_in(pos, 50.0).into_iter().any(|u| self.is_mex(u)) {
                    self.metal_spots[i].hostile = false;
                    has_mex = true;
                }
                if !has_mex && self.cb.enemy_units_in(pos, 50.0).into_iter().any(|u| self.is_mex(u)) {
                    self.set_hostile(i);
                    has_mex = true;
                }
                if !has_mex {
                    self.set_neutral(i);
                }
                self.clean_shadows(i);
            } else {
                let ms = &mut self.metal_spots[i];
                ms.visible = false;
                ms.owned = false;
                if frame - ms.last_seen > 3600 {
                    ms.last_seen = frame - 1800;
                }
            }
        }

        // mark front line territory
        self.front_line_graphics.clear();
        for i in 0..self.metal_spots.len() {
            if self.spot_is_front_line(&self.metal_spots[i]) {
                let (x, y) = grid_coords(self.metal_spots[i].position);
                self.front_line_graphics.paint_circle(x, y, 10, 1);
            }
        }

        self.calc_centers();

        let spots = self.metal_spots.len() as f32;
        // port: the original divides by zero (NaN) on maps without metal spots.
        if spots > 0.0 {
            self.territory_fraction = self.get_owned_spots().len() as f32 / spots;
            self.eminent_territory =
                self.get_touched_spots().len() as f32 / spots > 0.85 || self.territory_fraction > 0.4;
        } else {
            self.territory_fraction = 0.0;
            self.eminent_territory = false;
        }
    }

    fn is_mex(&self, u: Unit) -> bool {
        u.def().is_some_and(|d| d.id() == self.mex_def_id)
    }

    /// Whether ground forces travel a link directly (and territory shadows
    /// spread along it; see `ground_links`).
    pub fn is_ground_link(&self, link: usize) -> bool {
        self.ground_links.get(link).copied().unwrap_or(true)
    }

    /// Mark a spot as enemy-held and its ground neighbours as
    /// enemy-shadowed.
    fn set_hostile(&mut self, spot: usize) {
        let ms = &mut self.metal_spots[spot];
        ms.hostile = true;
        ms.owned = false;
        ms.touched = true;
        ms.enemy_shadowed = false;

        // paint territory circles
        let (x, y) = grid_coords(ms.position);
        self.enemy_territory_graphics.paint_circle(x, y, 13, 1);

        // set adjacent spots as enemyShadowed if they aren't already hostile
        for k in 0..self.metal_spots[spot].links.len() {
            let li = self.metal_spots[spot].links[k];
            if !self.is_ground_link(li) {
                continue;
            }
            for v in [self.links[li].v0, self.links[li].v1] {
                if !self.metal_spots[v].hostile {
                    self.metal_spots[v].enemy_shadowed = true;
                }
            }
        }
    }

    /// Mark a spot as ours and its ground neighbours as ally-shadowed.
    fn set_owned(&mut self, spot: usize) {
        let ms = &mut self.metal_spots[spot];
        ms.hostile = false;
        ms.owned = true;
        ms.touched = true;
        ms.ally_shadowed = false;

        // paint territory circles
        let (x, y) = grid_coords(ms.position);
        self.ally_territory_graphics.paint_circle(x, y, 25, 1);

        // set adjacent spots as allyShadowed if they aren't already owned
        for k in 0..self.metal_spots[spot].links.len() {
            let li = self.metal_spots[spot].links[k];
            if !self.is_ground_link(li) {
                continue;
            }
            for v in [self.links[li].v0, self.links[li].v1] {
                if !self.metal_spots[v].owned {
                    self.metal_spots[v].ally_shadowed = true;
                }
            }
        }
    }

    /// A visible spot without a mex: whoever held it is remembered as a
    /// shadow.
    fn set_neutral(&mut self, spot: usize) {
        let ms = &mut self.metal_spots[spot];
        let (x, y) = grid_coords(ms.position);
        if ms.owned {
            ms.ally_shadowed = true;
            ms.extractor = None;
            // unpaint territory circles
            self.ally_territory_graphics.unpaint_circle(x, y, 25, 1);
        }
        if ms.hostile {
            ms.enemy_shadowed = true;
            // unpaint territory circles
            self.enemy_territory_graphics.unpaint_circle(x, y, 13, 1);
        }
        ms.hostile = false;
        ms.owned = false;
    }

    /// Drop shadows that no adjacent spot supports any more.
    fn clean_shadows(&mut self, spot: usize) {
        let mut has_adjacent_owned = false;
        let mut has_adjacent_hostile = false;
        for &li in self.metal_spots[spot].links.iter().filter(|&&li| self.is_ground_link(li)) {
            let (a, b) = (&self.metal_spots[self.links[li].v0], &self.metal_spots[self.links[li].v1]);
            if a.hostile || b.hostile {
                has_adjacent_hostile = true;
            }
            if a.owned || b.owned {
                has_adjacent_owned = true;
            }
        }

        let ms = &mut self.metal_spots[spot];
        if !has_adjacent_hostile && (has_adjacent_owned || ms.owned) {
            ms.enemy_shadowed = false;
        }
        if !has_adjacent_owned {
            ms.ally_shadowed = false;
        }
    }

    fn calc_centers(&mut self) {
        self.ally_center = self.spots_center(&self.get_owned_spots());
        self.enemy_center = self.spots_center(&self.get_enemy_spots());
    }

    /// Mean position of `spots` snapped to a build site; `NULLPOS` if none.
    fn spots_center(&self, spots: &[&MetalSpot]) -> Vec3 {
        let mut position = NULLPOS;
        if !spots.is_empty() {
            let count = spots.len() as f32;
            let (mut x, mut z) = (0.0f32, 0.0f32);
            for ms in spots {
                x += ms.position.x / count;
                z += ms.position.z / count;
            }
            position.x = x;
            position.z = z;
            if let Some(def) = self.center_def {
                position = self.cb.map().find_closest_build_site(def, position, 600.0, 3, 0);
            }
        }
        position
    }

    /// Register a new grid building and update link connectivity.
    fn add_pylon(&mut self, unit: Unit, radius: i32, position: Vec3) {
        let id = unit.id();
        // Unit ids are recycled: drop whatever stale pylon still holds this id.
        self.remove_pylon(id);

        let mut p = Pylon::new(unit, radius, position);
        for (i, m) in self.metal_spots.iter_mut().enumerate() {
            if distance(p.position, m.position) < (p.radius + 50) as f32 {
                m.pylons.push(id);
                p.spots.push(i);
            }
        }
        self.pylons.insert(id, p);

        for li in 0..self.links.len() {
            let l = &self.links[li];
            if distance(position, l.center_pos) < l.length / 2.0 + radius as f32 {
                let others: Vec<i32> = l.pylons.iter().copied().filter(|&o| o != id).collect();
                for other in others {
                    let Some(lp) = self.pylons.get_mut(&other) else { continue };
                    if distance(position, lp.position) < (radius + lp.radius) as f32 {
                        lp.add_neighbour(id);
                        if let Some(p) = self.pylons.get_mut(&id) {
                            p.add_neighbour(other);
                        }
                    }
                }

                self.links[li].pylons.insert(id);
                if let Some(p) = self.pylons.get_mut(&id) {
                    p.links.push(li);
                }
                self.check_link_connected(li);
            }
        }
    }

    /// Forget a grid building (if `id` is one) and update link connectivity.
    // port: the original found the dead pylon through the links in its range,
    // so one in range of spots but of no link stayed in those spots' lists.
    fn remove_pylon(&mut self, id: i32) {
        let Some(dead) = self.pylons.remove(&id) else { return };
        for n in &dead.neighbours {
            if let Some(p) = self.pylons.get_mut(n) {
                p.remove_neighbour(id);
            }
        }
        for &s in &dead.spots {
            let pylons = &mut self.metal_spots[s].pylons;
            if let Some(i) = pylons.iter().position(|&p| p == id) {
                pylons.remove(i);
            }
        }
        for &l in &dead.links {
            self.links[l].pylons.remove(&id);
            self.check_link_connected(l);
        }
    }

    /// Recompute whether a chain of pylons leads from `v1` of a link to `v0`.
    fn check_link_connected(&mut self, link: usize) {
        let connected = self.link_has_pylon_chain(link);
        self.links[link].connected = connected;
    }

    fn link_has_pylon_chain(&self, link: usize) -> bool {
        let l = &self.links[link];
        let target = l.v0;
        let target_pos = self.metal_spots[target].position;
        let mut visited = BTreeSet::new();
        let mut queue: VecDeque<i32> = self.metal_spots[l.v1].pylons.iter().copied().collect();
        let mut min_dist = FAR;
        let mut i = 0;

        while let Some(qid) = queue.pop_front() {
            if i > 50 {
                return false; // infinite loop guard
            }
            i += 1;
            let Some(q) = self.pylons.get(&qid) else { continue };
            visited.insert(qid);

            // iterate through each pylon starting at v1 and
            // progressively moving towards v0 until v0 is reached
            let dist = distance(q.position, target_pos);
            if dist < min_dist {
                min_dist = dist;
                queue.extend(q.neighbours.iter().filter(|c| !visited.contains(*c)));
            }
            if q.spots.contains(&target) {
                return true;
            }
        }
        false
    }

    /// The pylon of an unconnected link, reachable from its end closest to
    /// `position`, that gets nearest to the other end: where to extend the
    /// grid from. `None` if the link is connected or that end has no pylons.
    pub fn get_link_connection_head(&self, link: usize, position: Vec3) -> Option<i32> {
        let l = self.links.get(link)?;
        // start from the closest end
        let (p0, p1) = (self.metal_spots[l.v0].position, self.metal_spots[l.v1].position);
        let (start, end_pos) = if distance(position, p0) < distance(position, p1) { (l.v0, p1) } else { (l.v1, p0) };
        let start_pylons = &self.metal_spots[start].pylons;
        if l.connected || start_pylons.is_empty() {
            return None;
        }

        let mut winner = None;
        let mut min_dist = FAR;
        let mut i = 0;
        let mut visited = BTreeSet::new();
        let mut queue: VecDeque<i32> = start_pylons.iter().copied().collect();
        while let Some(qid) = queue.pop_front() {
            if i > 50 {
                return None; // infinite loop guard
            }
            i += 1;
            let Some(q) = self.pylons.get(&qid) else { continue };
            visited.insert(qid);
            let dist = distance(q.position, end_pos);
            if dist < min_dist {
                min_dist = dist;
                winner = Some(qid);
                queue.extend(q.neighbours.iter().filter(|c| !visited.contains(*c)));
            }
        }
        winner
    }

    pub fn set_start_pos(&mut self, pos: Vec3) {
        self.start_pos = pos;
    }

    pub fn get_start_pos(&self) -> Vec3 {
        self.start_pos
    }

    pub fn get_links(&self) -> &[Link] {
        &self.links
    }

    /// Both endpoints owned (the original `Link::isOwned`).
    pub fn is_link_owned(&self, link: &Link) -> bool {
        let (v0, v1) = (&self.metal_spots[link.v0], &self.metal_spots[link.v1]);
        (v0.owned && v1.owned)
            || ((v0.owned || v1.owned) && !v0.hostile && !v0.enemy_shadowed && !v1.hostile && !v1.enemy_shadowed)
    }

    /// An end is ally-shadowed and neither is hostile (`Link::isAllyShadowed`).
    pub fn is_link_ally_shadowed(&self, link: &Link) -> bool {
        let (v0, v1) = (&self.metal_spots[link.v0], &self.metal_spots[link.v1]);
        (v0.ally_shadowed || v1.ally_shadowed) && !v0.hostile && !v1.hostile
    }

    /// An end is hostile or enemy-shadowed (`Link::isHostile`).
    pub fn is_link_hostile(&self, link: &Link) -> bool {
        let (v0, v1) = (&self.metal_spots[link.v0], &self.metal_spots[link.v1]);
        v0.hostile || v0.enemy_shadowed || v1.hostile || v1.enemy_shadowed
    }

    /// Whether both ends have live extractors that the game reports on
    /// different overdrive grids. False if either end has no extractor.
    fn link_grids_differ(&self, link: &Link) -> bool {
        match (self.metal_spots[link.v0].extractor, self.metal_spots[link.v1].extractor) {
            (Some(a), Some(b)) => {
                a.health() > 0.0
                    && b.health() > 0.0
                    && a.rules_param_float("gridNumber", 0.0) != b.rules_param_float("gridNumber", 0.0)
            }
            _ => false,
        }
    }

    fn spot_is_front_line(&self, ms: &MetalSpot) -> bool {
        if !ms.owned && !ms.ally_shadowed {
            return false;
        }
        if ms.enemy_shadowed {
            return true;
        }
        ms.links.iter().any(|&l| self.is_link_hostile(&self.links[l]))
    }

    /// Ours (or ally-shadowed) and bordering enemy territory
    /// (`MetalSpot::isFrontLine`).
    pub fn is_spot_front_line(&self, spot: usize) -> bool {
        self.metal_spots.get(spot).is_some_and(|ms| self.spot_is_front_line(ms))
    }

    /// Whether a spot's extractor is hooked up to the overdrive grid
    /// (`MetalSpot::isConnected`).
    pub fn is_spot_connected(&self, spot: usize) -> bool {
        let Some(ms) = self.metal_spots.get(spot) else { return false };
        match ms.extractor {
            Some(e) if !e.is_being_built() => {
                let mut num_connected = 0;
                for &li in &ms.links {
                    let l = &self.links[li];
                    if !self.is_link_owned(l) {
                        continue;
                    }
                    let finished = |s: usize| self.metal_spots[s].extractor.filter(|u| !u.is_being_built());
                    if finished(l.v0).is_none() || finished(l.v1).is_none() {
                        if l.connected {
                            num_connected += 1;
                        }
                        continue;
                    }
                    if !l.connected && self.link_grids_differ(l) {
                        return false;
                    }
                    num_connected += 1;
                }
                num_connected > 0
            }
            // don't allow more than one connected link to uncapped or partially finished mexes where we can't check grid number.
            _ => ms.links.iter().any(|&l| self.links[l].connected),
        }
    }

    pub fn is_ally_territory(&self, position: Vec3) -> bool {
        let (x, y) = grid_coords(position);
        self.ally_territory_graphics.get_value(x, y) > 0.0
    }

    pub fn is_enemy_territory(&self, position: Vec3) -> bool {
        let (x, y) = grid_coords(position);
        self.enemy_territory_graphics.get_value(x, y) > 0.0
    }

    pub fn is_front_line(&self, position: Vec3) -> bool {
        let (x, y) = grid_coords(position);
        self.front_line_graphics.get_value(x, y) > 0.0
    }

    pub fn get_metal_spots(&self) -> Vec<&MetalSpot> {
        self.metal_spots.iter().collect()
    }

    pub fn get_enemy_spots(&self) -> Vec<&MetalSpot> {
        self.metal_spots.iter().filter(|ms| ms.hostile).collect()
    }

    pub fn get_enemy_territory(&self) -> Vec<&MetalSpot> {
        self.metal_spots.iter().filter(|ms| (ms.hostile || ms.enemy_shadowed) && !ms.owned).collect()
    }

    pub fn get_ally_territory(&self) -> Vec<&MetalSpot> {
        self.metal_spots.iter().filter(|ms| (ms.owned || ms.ally_shadowed) && !ms.hostile).collect()
    }

    pub fn get_owned_spots(&self) -> Vec<&MetalSpot> {
        self.metal_spots.iter().filter(|ms| ms.owned).collect()
    }

    /// All metal spots not owned by allies.
    pub fn get_unowned_spots(&self) -> Vec<&MetalSpot> {
        self.metal_spots.iter().filter(|ms| !ms.owned).collect()
    }

    pub fn get_neutral_spots(&self) -> Vec<&MetalSpot> {
        self.metal_spots.iter().filter(|ms| !ms.owned && !ms.hostile).collect()
    }

    pub fn get_touched_spots(&self) -> Vec<&MetalSpot> {
        self.metal_spots.iter().filter(|ms| ms.touched).collect()
    }

    pub fn get_front_line_spots(&self) -> Vec<&MetalSpot> {
        self.metal_spots.iter().filter(|ms| self.spot_is_front_line(ms)).collect()
    }

    /// Debug dump of the graph to a text file.
    pub fn paint_ascii_graph(&self) {
        if let Err(e) = std::fs::write(ASCII_GRAPH_FILE, self.render_ascii_graph()) {
            self.cb.log(&format!("GraphManager: could not write {ASCII_GRAPH_FILE}: {e}"));
        }
    }

    /// The graph as text, one character per 64 elmos: `o`/`e`/`u` for
    /// owned/hostile/other spots, outlined `o`/`e`/`c` for ally-, enemy- or
    /// both-shadowed ones, `=`/`-` (`|`, `+`) for connected/other links.
    fn render_ascii_graph(&self) -> String {
        let mut canvas = Canvas::default();
        let res = 8;
        let cell = |p: Vec3| ((p.x / 8.0) as i32, (p.z / 8.0) as i32);

        for l in &self.links {
            let c = if l.connected { b'=' } else { b'-' };
            let (x1, y1) = cell(self.metal_spots[l.v0].position);
            let (x2, y2) = cell(self.metal_spots[l.v1].position);
            canvas.draw_line(x1, y1, x2, y2, c, res);
        }

        for ms in &self.metal_spots {
            let (x, y) = cell(ms.position);
            // draw solid dots for owned/hostile mexes
            if ms.owned {
                canvas.paint_circle(x, y, 6, b'o', res, false);
            } else if ms.hostile {
                canvas.paint_circle(x, y, 6, b'e', res, false);
            } else {
                canvas.paint_circle(x, y, 4, b'u', res, false);
            }
            // draw hollow circles for shadow captured mexes
            match (ms.ally_shadowed, ms.enemy_shadowed) {
                (true, false) => canvas.paint_circle(x, y, 8, b'o', res, true),
                (false, true) => canvas.paint_circle(x, y, 8, b'e', res, true),
                (true, true) => canvas.paint_circle(x, y, 8, b'c', res, true),
                (false, false) => {}
            }
        }
        canvas.render()
    }

    pub fn get_ally_center(&self) -> Vec3 {
        self.ally_center
    }

    pub fn get_ally_base(&self) -> Vec3 {
        self.ally_base
    }

    pub fn get_map_center(&self) -> Vec3 {
        self.map_center
    }

    pub fn get_enemy_center(&self) -> Vec3 {
        self.enemy_center
    }

    /// Center of the closest link between owned spots on the front line;
    /// `NULLPOS` if none.
    pub fn get_closest_front_line_link(&self, position: Vec3) -> Vec3 {
        let mut bestpos = NULLPOS;
        let mut bestdist = FAR;
        for l in &self.links {
            if self.metal_spots[l.v0].owned && self.metal_spots[l.v1].owned && self.is_front_line(l.center_pos) {
                let dist = distance(position, l.center_pos);
                if dist < bestdist {
                    bestdist = dist;
                    bestpos = l.center_pos;
                }
            }
        }
        bestpos
    }

    /// The closest spot outside our territory; failing that the enemy
    /// center, or the map center if that is unknown too.
    pub fn get_closest_battle_front(&self, position: Vec3) -> Vec3 {
        let mut pos = None;
        let mut mindist = 0.0;
        for ms in &self.metal_spots {
            if !ms.owned && !self.is_ally_territory(ms.position) {
                let dist = distance(ms.position, position);
                if pos.is_none() || dist < mindist {
                    mindist = dist;
                    pos = Some(ms.position);
                }
            }
        }
        match pos {
            Some(p) => p,
            None if self.enemy_center != NULLPOS => self.enemy_center,
            None => self.map_center,
        }
    }

    fn closest_spot_where(&self, position: Vec3, pred: impl Fn(&MetalSpot) -> bool) -> Option<usize> {
        let mut best = None;
        let mut min_range = 0.0;
        for (i, ms) in self.metal_spots.iter().enumerate() {
            if !pred(ms) {
                continue;
            }
            let dist = distance(position, ms.position);
            if best.is_none() || dist < min_range {
                best = Some(i);
                min_range = dist;
            }
        }
        best
    }

    /// Index of the metal spot closest to `position`; `None` without spots.
    pub fn get_closest_spot_index(&self, position: Vec3) -> Option<usize> {
        self.closest_spot_where(position, |_| true)
    }

    pub fn get_closest_neutral_spot(&self, position: Vec3) -> Option<&MetalSpot> {
        self.closest_spot_where(position, |ms| !ms.owned && !ms.hostile).map(|i| &self.metal_spots[i])
    }

    pub fn get_closest_enemy_spot(&self, position: Vec3) -> Option<&MetalSpot> {
        self.closest_spot_where(position, |ms| ms.hostile).map(|i| &self.metal_spots[i])
    }

    pub fn get_closest_spot(&self, position: Vec3) -> Option<&MetalSpot> {
        self.get_closest_spot_index(position).map(|i| &self.metal_spots[i])
    }

    /// The closest owned front-line spot.
    pub fn get_closest_front_line_spot(&self, position: Vec3) -> Option<&MetalSpot> {
        self.closest_spot_where(position, |ms| ms.owned && self.spot_is_front_line(ms)).map(|i| &self.metal_spots[i])
    }

    /// The nearest spot not on the grid, if any.
    fn closest_unconnected_spot(&self, position: Vec3) -> Option<usize> {
        let mut best = None;
        let mut dist = 0.0;
        for (i, ms) in self.metal_spots.iter().enumerate() {
            if !self.is_spot_connected(i) {
                let d = distance(position, ms.position);
                if best.is_none() || d < dist {
                    dist = d;
                    best = Some(i);
                }
            }
        }
        best
    }

    /// The shortest of a spot's links satisfying `pred`.
    fn shortest_link_where(&self, spot: usize, pred: impl Fn(&Link) -> bool) -> Option<usize> {
        let mut link = None;
        let mut dist = 0.0;
        for &li in &self.metal_spots[spot].links {
            let l = &self.links[li];
            if pred(l) && (link.is_none() || l.length < dist) {
                dist = l.length;
                link = Some(li);
            }
        }
        link
    }

    /// Center of the shortest unconnected owned link (between finished
    /// extractors on different grids) of the nearest unconnected spot;
    /// `NULLPOS` if none.
    pub fn get_closest_pylon_spot(&self, position: Vec3) -> Vec3 {
        // First find the nearest unconnected mex
        let Some(best) = self.closest_unconnected_spot(position) else { return NULLPOS };

        // then find the shortest unconnected link
        // port: the original dereferenced missing extractors here; such links are skipped.
        self.shortest_link_where(best, |l| !l.connected && self.is_link_owned(l) && self.link_grids_differ(l))
            .map_or(NULLPOS, |li| self.links[li].center_pos)
    }

    /// Best place for a grid building of type `pylon_name` near `position`
    /// to connect overdrive; `NULLPOS` if none.
    pub fn get_overdrive_sweet_spot(&self, position: Vec3, pylon_name: &str) -> Vec3 {
        let Some(&pylon_range) = self.pylon_defs.get(pylon_name) else { return NULLPOS };
        let mut radius = 0.9 * pylon_range as f32;

        // First find the nearest unconnected mex (all connected: the nearest;
        // port: the original took the one nearest the map's corner).
        let best = self.closest_unconnected_spot(position).or_else(|| self.get_closest_spot_index(position));
        let Some(best) = best else { return NULLPOS };

        // then find the shortest unconnected link
        let link = self.shortest_link_where(best, |l| {
            !l.connected
                && self.is_link_owned(l)
                && (self.metal_spots[l.v0].extractor.is_none()
                    || self.metal_spots[l.v1].extractor.is_none()
                    || self.link_grids_differ(l))
        });

        let Some(li) = link else { return self.metal_spots[best].position };
        let l = &self.links[li];
        let (p0, p1) = (self.metal_spots[l.v0].position, self.metal_spots[l.v1].position);
        let head = self.get_link_connection_head(li, position).and_then(|id| self.pylons.get(&id));
        match head {
            Some(p) => {
                radius += 0.9 * p.radius as f32;
                if distance(position, p0) < distance(position, p1) {
                    get_directional_point(p.position, p1, radius)
                } else {
                    get_directional_point(p.position, p0, radius)
                }
            }
            // if no pylons are present, start a new chain.
            None => {
                if distance(position, p0) < distance(position, p1) {
                    get_directional_point(p0, l.center_pos, radius)
                } else {
                    get_directional_point(p1, l.center_pos, radius)
                }
            }
        }
    }
}

/// Cross-module graph logic and module events.
impl Ai {
    /// Module init: nothing to do (the original only cached manager pointers).
    pub fn graph_init(&mut self) {}

    pub fn graph_lua_message(&mut self, data: &str) {
        self.graph.lua_message(data);
    }

    pub fn graph_unit_created(&mut self, unit: Unit, _builder: Option<Unit>) {
        let Some(def) = unit.def() else { return };
        if def.id() == self.graph.mex_def_id {
            let pos = unit.pos();
            if let Some(spot) = self.graph.get_closest_spot_index(pos) {
                if self.war.get_total_friendly_threat(pos) < 0.5 || !self.eco.defended_fac {
                    self.graph.set_owned(spot);
                }
                self.graph.metal_spots[spot].extractor = Some(unit);
            }
        }

        if let Some(&radius) = self.graph.pylon_defs.get(self.defs.name(def)) {
            self.graph.add_pylon(unit, radius, unit.pos());
        }
    }

    pub fn graph_unit_finished(&mut self, unit: Unit) {
        let Some(def) = unit.def() else { return };
        if def.id() == self.graph.mex_def_id
            && let Some(spot) = self.graph.get_closest_spot_index(unit.pos())
        {
            self.graph.set_owned(spot);
            self.graph.metal_spots[spot].extractor = Some(unit);
        }
    }

    pub fn graph_unit_destroyed(&mut self, unit: Unit, _attacker: Option<Unit>) {
        self.graph.remove_pylon(unit.id());
    }

    /// When enemy porc is seen, infer that the nearest metal spot is hostile.
    pub fn graph_enemy_enter_los(&mut self, enemy: Unit) {
        let Some(def) = enemy.def() else { return };
        if enemy.max_speed() == 0.0
            && def.is_able_to_attack()
            && let Some(spot) = self.graph.get_closest_spot_index(enemy.pos())
        {
            // unless it's already owned by allies
            let ms = &self.graph.metal_spots[spot];
            if !ms.hostile && !ms.owned {
                self.graph.set_hostile(spot);
            }
        }
    }

    pub fn graph_update(&mut self, frame: i32) {
        self.graph.update(frame, &self.los);
    }

    /// 100 elmos from the closest (by distance per build power) safe worker
    /// in our territory, towards the ally center; `None` if no worker is safe.
    fn graph_closest_worker_haven(&self, position: Vec3, need_no_aa: bool) -> Option<Vec3> {
        let ally_center = self.graph.ally_center;
        let mut closest = None;
        let mut dist = 0.0;
        for w in self.eco.workers.values() {
            let wpos = w.get_pos();
            if !self.graph.is_ally_territory(wpos)
                || self.war.get_threat(wpos) > 0.0
                || (need_no_aa && self.war.get_aa_threat(wpos) > 0.0)
                || wpos == position
            {
                continue;
            }
            let d = per_bp(distance(position, wpos), self.war.get_bp(wpos));
            if closest.is_none() || d < dist {
                dist = d;
                closest = Some(get_directional_point(wpos, ally_center, 100.0));
            }
        }
        closest
    }

    /// Snap to a `factorygunship` build site, as the original did.
    fn graph_build_site(&self, pos: Vec3) -> Vec3 {
        match self.graph.center_def {
            Some(def) => self.cb.map().find_closest_build_site(def, pos, 600.0, 3, 0),
            None => pos,
        }
    }

    pub fn graph_get_closest_haven(&self, position: Vec3) -> Vec3 {
        self.graph_closest_worker_haven(position, false).unwrap_or(self.graph.ally_center)
    }

    /// The closest metal spot without enemy threat; the map center if none.
    pub fn graph_get_closest_raider_haven(&self, position: Vec3) -> Vec3 {
        let mut closest = None;
        let mut dist = 0.0;
        for ms in &self.graph.metal_spots {
            if self.war.get_threat(ms.position) == 0.0 {
                let tempdist = distance(position, ms.position);
                if closest.is_none() || tempdist < dist {
                    dist = tempdist;
                    closest = Some(ms.position);
                }
            }
        }
        closest.unwrap_or(self.graph.map_center)
    }

    pub fn graph_get_closest_air_haven(&self, position: Vec3) -> Vec3 {
        match self.graph_closest_worker_haven(position, true) {
            Some(closest) => self.graph_build_site(closest),
            None => self.graph.ally_center,
        }
    }

    /// The closest ally-shadowed link whose enemy threat we can take on from
    /// `position`; `NULLPOS` if none.
    pub fn graph_get_closest_leading_link(&self, position: Vec3) -> Vec3 {
        let fthreat = self.war.get_friendly_threat(position);
        self.graph_closest_leading_link(position, |p| self.war.get_effective_threat(p) <= fthreat)
    }

    /// The closest ally-shadowed link without AA threat; `NULLPOS` if none.
    pub fn graph_get_closest_air_leading_link(&self, position: Vec3) -> Vec3 {
        self.graph_closest_leading_link(position, |p| self.war.get_aa_threat(p) == 0.0)
    }

    fn graph_closest_leading_link(&self, position: Vec3, safe: impl Fn(Vec3) -> bool) -> Vec3 {
        let mut closest = None;
        let mut dist = 0.0;
        for l in &self.graph.links {
            if self.graph.is_link_ally_shadowed(l) && safe(l.center_pos) {
                let d = distance(position, l.center_pos);
                if closest.is_none() || d < dist {
                    dist = d;
                    closest = Some(l.center_pos);
                }
            }
        }
        closest.map_or(NULLPOS, |c| self.graph_build_site(c))
    }
}
