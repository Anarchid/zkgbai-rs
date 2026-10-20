//! Building a [`TierMap`] from raw map data.

use std::collections::{HashMap, VecDeque};

use zkta::analyzer::{NoopRenderer, analyze_bitmap};
use zkta::geometry::Point;

use crate::{CELL, Choke, ChokeKind, MoveClass, NONE, Pos, Region, SpringMapData, Tier, TierMap, borders};

/// Passable components smaller than this (cells) get no region of their own:
/// specks between steep slopes that no unit meaningfully occupies.
const MIN_POCKET_CELLS: usize = 32;

/// Open ground fenced off by slopes, without a region from zkta, becomes a
/// region of its own from this size (cells, about 180 x 180 elmos).
const MIN_TERRACE_CELLS: usize = 128;

/// How far (cells) a gate line is extended past its crossing points to reach
/// real obstacle cells: zkta's crossing points lie on simplified contours,
/// which can sit a few cells off the raster obstacle boundary.
const GATE_EXTENSION: i32 = 12;

pub(crate) fn tier_map(data: &SpringMapData, tier: Tier, class: MoveClass) -> TierMap {
    // Hard obstacles: where the engine stops units (connectivity). Soft
    // obstacles additionally include slopes units only crawl up (see
    // `MoveClass::soft`); the region decomposition works on those.
    let ground = Ground::new(data, class);
    let obstacles = ground.obstacles(class);
    let soft = ground.obstacles(class.soft());
    let (width, height) = (ground.width, ground.height);
    let (w, h) = (width as u32, height as u32);
    let component = label_components(&obstacles, width, height);
    let passable = component.iter().filter(|&&c| c != NONE).count();

    let mut map = TierMap {
        tier,
        width,
        height,
        regions: Vec::new(),
        chokes: Vec::new(),
        region: vec![NONE; width * height],
        passable_fraction: passable as f32 / (width * height).max(1) as f32,
        component,
    };
    if passable == 0 {
        return map;
    }

    let analysis = analyze_bitmap(&soft, w, h, &NoopRenderer);

    // zkta ids -> our dense indices, in zkta's (deterministic) order.
    let region_index: HashMap<usize, usize> = analysis.regions.iter().enumerate().map(|(i, r)| (r.id, i)).collect();
    map.regions = analysis
        .regions
        .iter()
        .enumerate()
        .map(|(i, r)| Region { id: i, center: cell_point(r.center), cells: 0, component: NONE, chokes: Vec::new() })
        .collect();
    map.chokes = analysis
        .chokepoints
        .iter()
        .enumerate()
        .map(|(i, c)| {
            let mut regions: Vec<usize> = c.region_ids.iter().filter_map(|id| region_index.get(id).copied()).collect();
            regions.sort_unstable();
            let width = (2.0 * c.radius as f32 * CELL).max(CELL);
            let sides = regions.len();
            Choke {
                id: i,
                kind: ChokeKind::Gate,
                pos: cell_point(c.position),
                width,
                gate: c.crossing_points.map(|[a, b]| [cell_point(a), cell_point(b)]),
                regions,
                climb: vec![0.0; sides],
                bypass: vec![f32::INFINITY; sides],
                wall: width,
            }
        })
        .collect();
    for c in &map.chokes {
        for &r in &c.regions {
            map.regions[r].chokes.push(c.id);
        }
    }

    let walls = gate_walls(&analysis.chokepoints, &soft, width, height);
    let seeds: Vec<Option<usize>> =
        analysis.regions.iter().map(|r| seed_cell(r.center, r.radius, &soft, &walls, width, height)).collect();
    assign_regions(&mut map, &soft, &obstacles, &walls, &seeds);
    borders::link(&mut map, &ground, &soft, &walls);
    map
}

fn map_cell_center(cell: usize, width: usize) -> Pos {
    Pos::new(((cell % width) as f32 + 0.5) * CELL, ((cell / width) as f32 + 0.5) * CELL)
}

/// Center of a model cell in world coordinates.
fn cell_point(p: Point) -> Pos {
    Pos::new((p.x as f32 + 0.5) * CELL, (p.y as f32 + 0.5) * CELL)
}

/// The ground at slope-map resolution, for one movement class: what
/// stops it, and how long it takes to cross.
pub(crate) struct Ground {
    pub width: usize,
    pub height: usize,
    class: MoveClass,
    /// Slope-map value per cell; `NAN` off the slope map's data.
    slope: Vec<f32>,
    /// Mean height of the cell's 2x2 heightmap squares.
    elevation: Vec<f32>,
    /// Water depth, likewise.
    depth: Vec<f32>,
}

impl Ground {
    fn new(data: &SpringMapData, class: MoveClass) -> Self {
        let (width, height) = (data.slope_width as usize, data.slope_height as usize);
        let hm_w = data.map_width as usize;
        let hm = |x: usize, z: usize| data.height_map.get(z * hm_w + x).copied().unwrap_or(0.0);
        let mut g = Ground {
            width,
            height,
            class,
            slope: vec![f32::NAN; width * height],
            elevation: vec![0.0; width * height],
            depth: vec![0.0; width * height],
        };
        for sz in 0..height {
            for sx in 0..width {
                let i = sz * width + sx;
                let (hx, hz) = (sx * 2, sz * 2);
                let corners = [hm(hx, hz), hm(hx + 1, hz), hm(hx, hz + 1), hm(hx + 1, hz + 1)];
                g.elevation[i] = corners.iter().sum::<f32>() * 0.25;
                g.depth[i] = corners.iter().map(|&y| (-y).max(0.0)).sum::<f32>() * 0.25;
                if let Some(&slope) = data.slope_map.get(i) {
                    g.slope[i] = slope;
                }
            }
        }
        g
    }

    /// Obstacle bitmap for a movement class (cells without slope data
    /// block).
    fn obstacles(&self, class: MoveClass) -> Vec<bool> {
        (0..self.slope.len()).map(|i| self.slope[i].is_nan() || class.blocks(self.slope[i], self.depth[i])).collect()
    }

    /// Time to step from cell `a` to its neighbour `b`, in cells of flat
    /// ground. Climbing slows units by `1 + slope * slope_mod` (see
    /// [`MoveClass::uphill_speed`]), scaled by how directly the step climbs
    /// (the engine's `dirSlopeMod`): its rise against the rise straight up
    /// the slope. Downhill, across slopes and on water for hovercraft, a
    /// step takes as long as on flat ground.
    pub fn step_time(&self, a: usize, b: usize) -> f32 {
        let rise = self.elevation[b] - self.elevation[a];
        let on_water = self.depth[a] > 0.0 || self.depth[b] > 0.0;
        if rise <= 0.0 || self.class.min_water_depth > 0.0 || (self.class.hover && on_water) {
            return 1.0;
        }
        let slope = 0.5 * (self.slope[a] + self.slope[b]);
        let cos = (1.0 - slope).max(0.05);
        let tan = (1.0 - cos * cos).max(0.0).sqrt() / cos;
        if tan.is_nan() || tan <= 1e-4 {
            return 1.0;
        }
        let directly = (rise / (CELL * tan)).min(1.0);
        1.0 + slope * self.class.slope_mod * directly
    }
}

/// 4-connected components of passable cells; `NONE` for obstacles.
fn label_components(obstacles: &[bool], width: usize, height: usize) -> Vec<u32> {
    let mut label = vec![NONE; obstacles.len()];
    let mut next = 0u32;
    let mut stack = Vec::new();
    for start in 0..obstacles.len() {
        if obstacles[start] || label[start] != NONE {
            continue;
        }
        label[start] = next;
        stack.push(start);
        while let Some(c) = stack.pop() {
            for n in neighbours4(c, width, height) {
                if !obstacles[n] && label[n] == NONE {
                    label[n] = next;
                    stack.push(n);
                }
            }
        }
        next += 1;
    }
    label
}

fn neighbours4(c: usize, width: usize, height: usize) -> impl Iterator<Item = usize> {
    let (x, z) = (c % width, c / width);
    [
        (x > 0).then(|| c - 1),
        (x + 1 < width).then(|| c + 1),
        (z > 0).then(|| c - width),
        (z + 1 < height).then(|| c + width),
    ]
    .into_iter()
    .flatten()
}

/// Rasterize every chokepoint's gate line (extended to the obstacles on
/// both sides) as walls: per cell, the index of the chokepoint whose wall
/// it is, `NONE` elsewhere. Lines are 8-connected, which seals them against
/// the 4-connected region flood.
fn gate_walls(chokes: &[zkta::region::Chokepoint], obstacles: &[bool], width: usize, height: usize) -> Vec<u32> {
    let mut walls = vec![NONE; obstacles.len()];
    let in_grid = |x: i32, z: i32| x >= 0 && z >= 0 && (x as usize) < width && (z as usize) < height;
    let blocked = |(x, z): (i32, i32)| !in_grid(x, z) || obstacles[z as usize * width + x as usize];
    // The obstacle cell nearest `p` within the extension, if any.
    let nearest_obstacle = |(px, pz): (i32, i32)| -> Option<(i32, i32)> {
        (1..=GATE_EXTENSION).find_map(|ring| {
            let mut ring_cells = Vec::new();
            for d in -ring..=ring {
                ring_cells.extend([(px + d, pz - ring), (px + d, pz + ring), (px - ring, pz + d), (px + ring, pz + d)]);
            }
            ring_cells.into_iter().filter(|&q| blocked(q)).min_by_key(|&(x, z)| (x - px).pow(2) + (z - pz).pow(2))
        })
    };
    for (i, c) in chokes.iter().enumerate() {
        let Some([a, b]) = c.crossing_points else { continue };
        let (dx, dz) = ((b.x - a.x) as f32, (b.y - a.y) as f32);
        let len = (dx * dx + dz * dz).sqrt();
        if len < 0.5 {
            continue;
        }
        let (ux, uz) = (dx / len, dz / len);
        // March outward from each end until the line meets an obstacle;
        // not at all from an end on one already, or past a thin obstacle
        // the line would cut into the open ground beyond it. An end the
        // line does not seal that way is joined to the nearest obstacle.
        let extend = |p: Point, sx: f32, sz: f32| -> ((i32, i32), Option<(i32, i32)>) {
            let p = (p.x, p.y);
            if blocked(p) {
                return (p, None);
            }
            let at =
                |i: i32| ((p.0 as f32 + sx * i as f32).round() as i32, (p.1 as f32 + sz * i as f32).round() as i32);
            if let Some(q) = (1..=GATE_EXTENSION).map(at).find(|&q| blocked(q)) {
                return (q, None);
            }
            match nearest_obstacle(p) {
                Some(q) => (p, Some(q)),
                None => (at(GATE_EXTENSION), None),
            }
        };
        let (start, start_spur) = extend(a, -ux, -uz);
        let (end, end_spur) = extend(b, ux, uz);
        let mut lines = vec![(start, end)];
        lines.extend(start_spur.map(|q| (start, q)));
        lines.extend(end_spur.map(|q| (end, q)));
        for (from, to) in lines {
            for (x, z) in line_cells(from, to) {
                if in_grid(x, z) && walls[z as usize * width + x as usize] == NONE {
                    walls[z as usize * width + x as usize] = i as u32;
                }
            }
        }
    }
    walls
}

/// Bresenham line, 8-connected, inclusive of both ends.
fn line_cells((mut x0, mut z0): (i32, i32), (x1, z1): (i32, i32)) -> Vec<(i32, i32)> {
    let (dx, dz) = ((x1 - x0).abs(), -(z1 - z0).abs());
    let (sx, sz) = (if x0 < x1 { 1 } else { -1 }, if z0 < z1 { 1 } else { -1 });
    let mut err = dx + dz;
    let mut cells = Vec::with_capacity((dx - dz) as usize + 1);
    loop {
        cells.push((x0, z0));
        if x0 == x1 && z0 == z1 {
            return cells;
        }
        let e2 = 2 * err;
        if e2 >= dz {
            err += dz;
            x0 += sx;
        }
        if e2 <= dx {
            err += dx;
            z0 += sz;
        }
    }
}

/// The region center's cell, or the nearest open (passable, non-wall) cell
/// within the region's radius.
fn seed_cell(
    center: Point,
    radius: f64,
    obstacles: &[bool],
    walls: &[u32],
    width: usize,
    height: usize,
) -> Option<usize> {
    let open = |x: i32, z: i32| -> Option<usize> {
        if x < 0 || z < 0 || x as usize >= width || z as usize >= height {
            return None;
        }
        let c = z as usize * width + x as usize;
        (!obstacles[c] && walls[c] == NONE).then_some(c)
    };
    if let Some(c) = open(center.x, center.y) {
        return Some(c);
    }
    let max_ring = (radius.ceil() as i32).max(2);
    for ring in 1..=max_ring {
        for dz in -ring..=ring {
            for dx in -ring..=ring {
                if (dx.abs() == ring || dz.abs() == ring)
                    && let Some(c) = open(center.x + dx, center.y + dz)
                {
                    return Some(c);
                }
            }
        }
    }
    None
}

/// Flood regions from their seeds over soft-passable ground within the gate
/// walls, then give the leftovers (wall cells, steep but passable slopes,
/// pockets without a seed) to the nearest region over hard-passable ground.
fn assign_regions(map: &mut TierMap, soft: &[bool], obstacles: &[bool], walls: &[u32], seeds: &[Option<usize>]) {
    let (width, height) = (map.width, map.height);
    let mut queue = VecDeque::new();
    for (r, seed) in seeds.iter().enumerate() {
        if let Some(c) = *seed
            && map.region[c] == NONE
        {
            map.region[c] = r as u32;
            queue.push_back(c);
        }
    }
    let mut frontier = Vec::new();
    while let Some(c) = queue.pop_front() {
        frontier.push(c);
        for n in neighbours4(c, width, height) {
            if !soft[n] && walls[n] == NONE && map.region[n] == NONE {
                map.region[n] = map.region[c];
                queue.push_back(n);
            }
        }
    }
    // Open ground fenced off by slopes that zkta found no region in (a
    // terrace, a hilltop) is a region of its own: the slopes around it are
    // borders, not part of a neighbour.
    frontier.extend(new_regions(map, |c| !soft[c] && walls[c] == NONE, MIN_TERRACE_CELLS));
    // Second pass: through walls, steep slopes and into unseeded pockets.
    queue.extend(frontier);
    while let Some(c) = queue.pop_front() {
        for n in neighbours4(c, width, height) {
            if !obstacles[n] && map.region[n] == NONE {
                map.region[n] = map.region[c];
                queue.push_back(n);
            }
        }
    }
    // Passable components zkta found no region in (islands, sealed
    // plateaus) become regions of their own, without chokepoints; tiny
    // specks stay region-less.
    new_regions(map, |c| !obstacles[c], MIN_POCKET_CELLS);

    for (cell, &r) in map.region.iter().enumerate() {
        if r != NONE {
            let reg = &mut map.regions[r as usize];
            reg.cells += 1;
            reg.component = map.component[cell];
        }
    }
}

/// Turn every connected stretch of `open` cells without a region into a
/// region of its own, if it has at least `min_cells` cells. Returns the
/// cells of the new regions.
fn new_regions(map: &mut TierMap, open: impl Fn(usize) -> bool, min_cells: usize) -> Vec<usize> {
    const POCKET: u32 = NONE - 1;
    let (width, height) = (map.width, map.height);
    let (mut claimed, mut small) = (Vec::new(), Vec::new());
    for start in 0..map.region.len() {
        if !open(start) || map.region[start] != NONE {
            continue;
        }
        map.region[start] = POCKET;
        let mut cells = vec![start];
        let mut i = 0;
        while i < cells.len() {
            for n in neighbours4(cells[i], width, height) {
                if open(n) && map.region[n] == NONE {
                    map.region[n] = POCKET;
                    cells.push(n);
                }
            }
            i += 1;
        }
        if cells.len() < min_cells {
            small.extend(cells);
            continue;
        }
        let id = map.regions.len() as u32;
        for &c in &cells {
            map.region[c] = id;
        }
        let (sx, sz) = cells.iter().fold((0.0, 0.0), |(x, z), &c| (x + (c % width) as f32, z + (c / width) as f32));
        let centroid = Pos::new((sx / cells.len() as f32 + 0.5) * CELL, (sz / cells.len() as f32 + 0.5) * CELL);
        // The member cell closest to the centroid (the centroid itself may
        // lie outside a concave pocket).
        let center = cells
            .iter()
            .map(|&c| map_cell_center(c, width))
            .min_by(|a, b| a.distance(centroid).total_cmp(&b.distance(centroid)))
            .unwrap_or(centroid);
        map.regions.push(Region { id: id as usize, center, cells: 0, component: NONE, chokes: Vec::new() });
        claimed.extend(cells);
    }
    for c in small {
        map.region[c] = NONE;
    }
    claimed
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A map of two open rooms joined by one narrow corridor, plus an island
    /// unreachable from both.
    fn two_rooms() -> SpringMapData {
        let (sw, sh) = (96u32, 48u32);
        let mut slope = vec![0.0f32; (sw * sh) as usize];
        let wall = 1.0;
        for z in 0..sh {
            for x in 0..sw {
                let i = (z * sw + x) as usize;
                // Dividing wall at x in 44..52, with a corridor at z in 20..26.
                if (44..52).contains(&x) && !(20..26).contains(&z) {
                    slope[i] = wall;
                }
                // A walled-off island in the right room.
                let ring = (80..92).contains(&x) && (4..16).contains(&z);
                let inside = (83..89).contains(&x) && (7..13).contains(&z);
                if ring && !inside {
                    slope[i] = wall;
                }
            }
        }
        SpringMapData {
            map_width: sw * 2,
            map_height: sh * 2,
            slope_width: sw,
            slope_height: sh,
            slope_map: slope,
            height_map: vec![50.0; (sw * sh * 4) as usize],
        }
    }

    #[test]
    fn rooms_and_corridor() {
        let t = tier_map(&two_rooms(), Tier::Vehicle, Tier::Vehicle.zero_k_class());
        let left = Pos::new(10.0 * CELL, 24.0 * CELL);
        let right = Pos::new(70.0 * CELL, 40.0 * CELL);
        let island = Pos::new(86.0 * CELL, 10.0 * CELL);

        assert!(t.connected(left, right, 0.0));
        assert!(!t.connected(left, island, 0.0));
        assert!(!t.is_passable(Pos::new(48.0 * CELL, 5.0 * CELL)));

        let (rl, rr) = (t.region_at(left).unwrap(), t.region_at(right).unwrap());
        assert_ne!(rl, rr, "rooms must be separate regions");
        // The rooms are linked through a chokepoint in the corridor.
        let joined = t.neighbours(rl).any(|(c, r)| r == rr && (40.0..56.0).contains(&(t.chokes[c].pos.x / CELL)));
        assert!(joined, "no corridor chokepoint between the rooms: {:?}", t.chokes);
        // Every passable cell has a region (the island is big enough).
        for cell in 0..t.width * t.height {
            assert_eq!(t.component[cell] != NONE, t.region_of_cell(cell).is_some(), "cell {cell}");
        }
        assert!(t.region_at(island).is_some());
    }

    #[test]
    fn deterministic() {
        let a = tier_map(&two_rooms(), Tier::Bot, Tier::Bot.zero_k_class());
        let b = tier_map(&two_rooms(), Tier::Bot, Tier::Bot.zero_k_class());
        assert_eq!(a.region, b.region);
        assert_eq!(a.chokes.len(), b.chokes.len());
    }

    #[test]
    fn bresenham_is_connected() {
        let cells = line_cells((0, 0), (7, -3));
        assert_eq!(cells.first(), Some(&(0, 0)));
        assert_eq!(cells.last(), Some(&(7, -3)));
        for w in cells.windows(2) {
            assert!((w[0].0 - w[1].0).abs() <= 1 && (w[0].1 - w[1].1).abs() <= 1);
        }
    }
}
