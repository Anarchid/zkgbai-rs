//! Building a [`TierMap`] from raw map data.

use std::collections::{HashMap, VecDeque};

use zkta::analyzer::{NoopRenderer, analyze_bitmap};
use zkta::geometry::Point;

use crate::{CELL, Choke, MoveClass, NONE, Pos, Region, SpringMapData, Tier, TierMap};

/// Slope limits for the region decomposition, relative to the engine's hard
/// limit (see [`MoveClass::softened`]). Connectivity uses the hard limit.
const SOFT_SLOPE_FACTOR: f32 = 0.5;

/// Passable components smaller than this (cells) get no region of their own:
/// specks between steep slopes that no unit meaningfully occupies.
const MIN_POCKET_CELLS: usize = 32;

/// How far (cells) a gate line is extended past its crossing points to reach
/// real obstacle cells: zkta's crossing points lie on simplified contours,
/// which can sit a few cells off the raster obstacle boundary.
const GATE_EXTENSION: i32 = 12;

pub(crate) fn tier_map(data: &SpringMapData, tier: Tier, class: MoveClass) -> TierMap {
    // Hard obstacles: where the engine stops units (connectivity). Soft
    // obstacles additionally include slopes units only crawl over; the
    // region decomposition works on those.
    let (obstacles, w, h) = obstacle_map(data, class);
    let (soft, _, _) = obstacle_map(data, class.softened(SOFT_SLOPE_FACTOR));
    let (width, height) = (w as usize, h as usize);
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
            Choke {
                id: i,
                pos: cell_point(c.position),
                width: (2.0 * c.radius as f32 * CELL).max(CELL),
                gate: c.crossing_points.map(|[a, b]| [cell_point(a), cell_point(b)]),
                regions,
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
    map
}

fn map_cell_center(cell: usize, width: usize) -> Pos {
    Pos::new(((cell % width) as f32 + 0.5) * CELL, ((cell / width) as f32 + 0.5) * CELL)
}

/// Center of a model cell in world coordinates.
fn cell_point(p: Point) -> Pos {
    Pos::new((p.x as f32 + 0.5) * CELL, (p.y as f32 + 0.5) * CELL)
}

/// Obstacle bitmap at slope-map resolution for a movement class. Water
/// depth per cell is the mean over its 2x2 heightmap squares.
fn obstacle_map(data: &SpringMapData, class: MoveClass) -> (Vec<bool>, u32, u32) {
    let (w, h) = (data.slope_width, data.slope_height);
    let hm_w = data.map_width as usize;
    let depth_at = |x: usize, z: usize| data.height_map.get(z * hm_w + x).map_or(0.0, |&v| (-v).max(0.0));
    let mut obstacles = vec![true; (w * h) as usize];
    for sz in 0..h as usize {
        for sx in 0..w as usize {
            let i = sz * w as usize + sx;
            let Some(&slope) = data.slope_map.get(i) else { continue };
            let (hx, hz) = (sx * 2, sz * 2);
            let depth =
                (depth_at(hx, hz) + depth_at(hx + 1, hz) + depth_at(hx, hz + 1) + depth_at(hx + 1, hz + 1)) * 0.25;
            obstacles[i] = class.blocks(slope, depth);
        }
    }
    (obstacles, w, h)
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
/// both sides) as walls. Lines are 8-connected, which seals them against the
/// 4-connected region flood.
fn gate_walls(chokes: &[zkta::region::Chokepoint], obstacles: &[bool], width: usize, height: usize) -> Vec<bool> {
    let mut walls = vec![false; obstacles.len()];
    let in_grid = |x: i32, z: i32| x >= 0 && z >= 0 && (x as usize) < width && (z as usize) < height;
    let blocked = |x: i32, z: i32| !in_grid(x, z) || obstacles[z as usize * width + x as usize];
    for c in chokes {
        let Some([a, b]) = c.crossing_points else { continue };
        let (dx, dz) = ((b.x - a.x) as f32, (b.y - a.y) as f32);
        let len = (dx * dx + dz * dz).sqrt();
        if len < 0.5 {
            continue;
        }
        let (ux, uz) = (dx / len, dz / len);
        // March outward from each end until the line meets an obstacle.
        let extend = |p: Point, sx: f32, sz: f32| -> (i32, i32) {
            let mut end = (p.x, p.y);
            for i in 1..=GATE_EXTENSION {
                let q = ((p.x as f32 + sx * i as f32).round() as i32, (p.y as f32 + sz * i as f32).round() as i32);
                end = q;
                if blocked(q.0, q.1) {
                    break;
                }
            }
            end
        };
        let start = extend(a, -ux, -uz);
        let end = extend(b, ux, uz);
        for (x, z) in line_cells(start, end) {
            if in_grid(x, z) {
                walls[z as usize * width + x as usize] = true;
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
    walls: &[bool],
    width: usize,
    height: usize,
) -> Option<usize> {
    let open = |x: i32, z: i32| -> Option<usize> {
        if x < 0 || z < 0 || x as usize >= width || z as usize >= height {
            return None;
        }
        let c = z as usize * width + x as usize;
        (!obstacles[c] && !walls[c]).then_some(c)
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
fn assign_regions(map: &mut TierMap, soft: &[bool], obstacles: &[bool], walls: &[bool], seeds: &[Option<usize>]) {
    let (width, height) = (map.width, map.height);
    let region = &mut map.region;
    let mut queue = VecDeque::new();
    for (r, seed) in seeds.iter().enumerate() {
        if let Some(c) = *seed
            && region[c] == NONE
        {
            region[c] = r as u32;
            queue.push_back(c);
        }
    }
    let mut frontier = Vec::new();
    while let Some(c) = queue.pop_front() {
        frontier.push(c);
        for n in neighbours4(c, width, height) {
            if !soft[n] && !walls[n] && region[n] == NONE {
                region[n] = region[c];
                queue.push_back(n);
            }
        }
    }
    // Second pass: through walls, steep slopes and into unseeded pockets.
    queue.extend(frontier);
    while let Some(c) = queue.pop_front() {
        for n in neighbours4(c, width, height) {
            if !obstacles[n] && region[n] == NONE {
                region[n] = region[c];
                queue.push_back(n);
            }
        }
    }

    // Passable components zkta found no region in (islands, sealed
    // plateaus) become regions of their own, without chokepoints; tiny
    // specks stay region-less.
    const POCKET: u32 = NONE - 1;
    for start in 0..region.len() {
        if obstacles[start] || region[start] != NONE {
            continue;
        }
        region[start] = POCKET;
        let mut cells = vec![start];
        let mut i = 0;
        while i < cells.len() {
            for n in neighbours4(cells[i], width, height) {
                if !obstacles[n] && region[n] == NONE {
                    region[n] = POCKET;
                    cells.push(n);
                }
            }
            i += 1;
        }
        if cells.len() < MIN_POCKET_CELLS {
            continue;
        }
        let id = map.regions.len() as u32;
        for &c in &cells {
            region[c] = id;
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
    }

    for r in region.iter_mut().filter(|r| **r == POCKET) {
        *r = NONE;
    }
    for (cell, &r) in region.iter().enumerate() {
        if r != NONE {
            let reg = &mut map.regions[r as usize];
            reg.cells += 1;
            reg.component = map.component[cell];
        }
    }
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
