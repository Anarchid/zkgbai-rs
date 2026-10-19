//! Strategic analysis of one tier's region graph: which chokepoints guard
//! which regions, and which terrain interventions would hurt the enemy most
//! (or protect us best).
//!
//! Everything is a small graph computation over regions (nodes) and
//! chokepoints (edges, weighted by travel distance through the choke);
//! only bridge discovery looks at the cell grid.

use std::cmp::Ordering;
use std::collections::{BTreeMap, BinaryHeap, VecDeque};

use crate::{CELL, NONE, Pos, TierMap};

/// Whose an asset is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    Ally,
    Enemy,
    Neutral,
}

/// Something worth holding or taking: a metal spot, a building...
#[derive(Clone, Copy, Debug)]
pub struct Asset {
    pub pos: Pos,
    pub value: f32,
    pub side: Side,
}

/// What the analysis needs to know about the game.
pub struct Scenario<'a> {
    pub map: &'a TierMap,
    pub ally_base: Pos,
    pub enemy_base: Pos,
    pub assets: &'a [Asset],
}

/// Value of the assets in a region, by side.
#[derive(Clone, Copy, Debug, Default)]
pub struct RegionValue {
    pub ally: f32,
    pub enemy: f32,
    pub neutral: f32,
}

/// Walling off a chokepoint.
#[derive(Clone, Debug)]
pub struct WallOption {
    pub choke: usize,
    /// Wall length needed (the passage width), elmos.
    pub width: f32,
    /// Allied value no longer reachable by enemy ground forces.
    pub protects: f32,
    /// Regions it seals from the enemy base.
    pub regions: Vec<usize>,
}

/// A terraformed ramp or land bridge between two regions.
#[derive(Clone, Debug)]
pub struct BridgeOption {
    pub from: usize,
    pub to: usize,
    /// The two passable cells the bridge would join.
    pub ends: [Pos; 2],
    /// Length of the impassable stretch to fill, elmos.
    pub span: f32,
    /// Enemy value weighted by how much closer it gets to our base
    /// (newly reachable value counts in full).
    pub enemy_exposed: f32,
    /// Enemy guards (chokepoints protecting their value from us) bypassed.
    pub guards_bypassed: usize,
    pub score: f32,
}

/// Landing ground forces by air.
#[derive(Clone, Debug)]
pub struct DropOption {
    pub region: usize,
    pub enemy_value: f32,
    /// Ground travel distance from our base, `None` if unreachable.
    pub ground_distance: Option<f32>,
    /// Chokepoints our ground forces would have to pass.
    pub guards: usize,
    pub score: f32,
}

/// Placing a teleporter exit (units travel from home to `region` at once).
#[derive(Clone, Debug)]
pub struct TeleportOption {
    pub region: usize,
    /// Enemy value weighted by the travel distance saved.
    pub gain: f32,
}

#[derive(Clone, Debug, Default)]
pub struct Analysis {
    pub ally_base: Option<usize>,
    pub enemy_base: Option<usize>,
    pub values: Vec<RegionValue>,
    /// Ground travel distance from each base, `f32::INFINITY` if none.
    pub dist_from_ally: Vec<f32>,
    pub dist_from_enemy: Vec<f32>,
    /// Chokepoints every enemy ground route from their base to the region
    /// passes (empty: exposed via independent routes, or unreachable).
    pub guards_vs_enemy: Vec<Vec<usize>>,
    /// Chokepoints every ground route from our base to the region passes.
    pub guards_vs_ally: Vec<Vec<usize>>,
    /// Walls by allied value protected per elmo of wall, best first.
    pub walls: Vec<WallOption>,
    /// The walls (on our side of the map) that best trade wall length for
    /// allied value cut off from the enemy base; empty if no wall is worth
    /// building.
    pub seal: Vec<usize>,
    pub bridges: Vec<BridgeOption>,
    pub drops: Vec<DropOption>,
    pub teleports: Vec<TeleportOption>,
}

/// Longest obstacle stretch considered bridgeable, elmos.
pub const MAX_BRIDGE_SPAN: f32 = 320.0;

/// A bridge must save at least this much ground travel between its ends
/// (elmos) to be worth building.
pub const MIN_BRIDGE_SAVING: f32 = 800.0;

/// Wall length (elmos) worth one average asset when choosing a seal.
pub const WALL_ELMOS_PER_ASSET: f32 = 300.0;

/// Snap radius for placing assets and bases onto passable ground.
const SNAP: f32 = 96.0;

impl Analysis {
    pub fn run(s: &Scenario) -> Analysis {
        let map = s.map;
        let n = map.regions.len();
        if n == 0 {
            return Analysis::default();
        }
        let ally_base = map.region_near(s.ally_base, SNAP);
        let enemy_base = map.region_near(s.enemy_base, SNAP);
        let graph = RegionGraph::new(map);

        let mut values = vec![RegionValue::default(); n];
        for a in s.assets {
            if let Some(r) = map.region_near(a.pos, SNAP) {
                let v = &mut values[r];
                match a.side {
                    Side::Ally => v.ally += a.value,
                    Side::Enemy => v.enemy += a.value,
                    Side::Neutral => v.neutral += a.value,
                }
            }
        }

        let dist_from_ally = ally_base.map_or_else(|| vec![f32::INFINITY; n], |b| graph.distances(b, None));
        let dist_from_enemy = enemy_base.map_or_else(|| vec![f32::INFINITY; n], |b| graph.distances(b, None));
        let guards_vs_enemy = enemy_base.map_or_else(|| vec![Vec::new(); n], |b| graph.guards(b));
        let guards_vs_ally = ally_base.map_or_else(|| vec![Vec::new(); n], |b| graph.guards(b));

        let mut a = Analysis {
            ally_base,
            enemy_base,
            values,
            dist_from_ally,
            dist_from_enemy,
            guards_vs_enemy,
            guards_vs_ally,
            ..Analysis::default()
        };
        a.walls = a.wall_options(map);
        a.seal = a.seal_walls(map);
        a.bridges = a.bridge_options(map, &graph);
        a.drops = a.drop_options(map);
        a.teleports = a.teleport_options(&graph);
        a
    }

    fn wall_options(&self, map: &TierMap) -> Vec<WallOption> {
        let mut by_choke: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
        for (r, guards) in self.guards_vs_enemy.iter().enumerate() {
            for &c in guards {
                by_choke.entry(c).or_default().push(r);
            }
        }
        let mut walls: Vec<WallOption> = by_choke
            .into_iter()
            .map(|(c, regions)| WallOption {
                choke: c,
                width: map.chokes[c].width,
                protects: regions.iter().map(|&r| self.values[r].ally + 0.5 * self.values[r].neutral).sum(),
                regions,
            })
            .filter(|w| w.protects > 0.0)
            .collect();
        walls.sort_by(|a, b| (b.protects / b.width).total_cmp(&(a.protects / a.width)));
        walls
    }

    /// The best walls to build, as a minimum cut (project selection): from
    /// the enemy base, every allied-valued region drains its value to the
    /// sink (the price of leaving it exposed) and every chokepoint costs its
    /// wall length, priced at [`WALL_ELMOS_PER_ASSET`] per average asset.
    /// The cheapest cut is the most valuable set of walls. Only chokepoints
    /// on our side (nearer our base than theirs) can be walled: walls at the
    /// enemy's doorstep would not survive, so the cut must not simply box
    /// them in.
    fn seal_walls(&self, map: &TierMap) -> Vec<usize> {
        let Some(source) = self.enemy_base else { return Vec::new() };
        let value = |r: usize| self.values[r].ally + 0.5 * self.values[r].neutral;
        let targets: Vec<usize> = (0..map.regions.len())
            .filter(|&r| r != source && value(r) > 0.0 && self.dist_from_enemy[r].is_finite())
            .collect();
        if targets.is_empty() {
            return Vec::new();
        }
        let typical = targets.iter().map(|&r| value(r)).sum::<f32>() / targets.len() as f32;
        let per_elmo = typical / WALL_ELMOS_PER_ASSET;
        let sink = map.regions.len();
        let mut flow = FlowNet::new(sink + 1);
        for (c, choke) in map.chokes.iter().enumerate() {
            // Travel distance from a base to the chokepoint itself.
            let side_dist = |d: &[f32]| {
                choke
                    .regions
                    .iter()
                    .map(|&r| d[r] + map.regions[r].center.distance(choke.pos))
                    .fold(f32::INFINITY, f32::min)
            };
            let ours = side_dist(&self.dist_from_ally) < side_dist(&self.dist_from_enemy);
            let cap = if ours { (choke.width * per_elmo) as f64 } else { f64::INFINITY };
            for (i, &a) in choke.regions.iter().enumerate() {
                for &b in &choke.regions[i + 1..] {
                    flow.add_edge(a, b, cap, Some(c));
                }
            }
        }
        for &t in &targets {
            flow.add_edge(t, sink, value(t) as f64, None);
        }
        flow.min_cut(source, sink)
    }

    fn bridge_options(&self, map: &TierMap, graph: &RegionGraph) -> Vec<BridgeOption> {
        let Some(home) = self.ally_base else { return Vec::new() };
        let enemy_regions: Vec<usize> = (0..map.regions.len()).filter(|&r| self.values[r].enemy > 0.0).collect();
        if enemy_regions.is_empty() {
            return Vec::new();
        }
        let scale = ((map.width * map.height) as f32).sqrt() * CELL;
        let mut options: Vec<BridgeOption> = bridge_candidates(map)
            .into_iter()
            .filter_map(|cand| {
                // Distances from the far end, to see what the bridge brings closer.
                let (near, far) = if self.dist_from_ally[cand.from] <= self.dist_from_ally[cand.to] {
                    (cand.from, cand.to)
                } else {
                    (cand.to, cand.from)
                };
                if !self.dist_from_ally[near].is_finite() {
                    return None;
                }
                let from_far = graph.distances(far, None);
                // Only real shortcuts: the ends must be far apart by ground.
                if from_far[near] < cand.span + MIN_BRIDGE_SAVING {
                    return None;
                }
                let via = self.dist_from_ally[near] + cand.span;
                let mut exposed = 0.0;
                let mut bypassed = 0;
                for &r in &enemy_regions {
                    let before = self.dist_from_ally[r];
                    let after = before.min(via + from_far[r]);
                    if after < before {
                        exposed += self.values[r].enemy
                            * if before.is_finite() { ((before - after) / scale).min(1.0) } else { 1.0 };
                    }
                }
                // Guards of enemy value the new edge goes around.
                if far != home {
                    let guards_after = graph.guards_with_extra_edge(home, near, far);
                    for &r in &enemy_regions {
                        bypassed += self.guards_vs_ally[r].len().saturating_sub(guards_after[r].len());
                    }
                }
                let score = (exposed + 0.1 * bypassed as f32 * self.values_scale()) / (1.0 + cand.span / 160.0);
                (exposed > 0.0 || bypassed > 0).then_some(BridgeOption {
                    from: near,
                    to: far,
                    ends: cand.ends,
                    span: cand.span,
                    enemy_exposed: exposed,
                    guards_bypassed: bypassed,
                    score,
                })
            })
            .collect();
        options.sort_by(|a, b| b.score.total_cmp(&a.score));
        options
    }

    /// Typical asset value, so bypassed guards weigh like a share of it.
    fn values_scale(&self) -> f32 {
        let (sum, count) =
            self.values.iter().filter(|v| v.enemy > 0.0).fold((0.0, 0), |(s, c), v| (s + v.enemy, c + 1));
        if count == 0 { 1.0 } else { sum / count as f32 }
    }

    fn drop_options(&self, map: &TierMap) -> Vec<DropOption> {
        let Some(home) = self.ally_base else { return Vec::new() };
        let home_pos = map.regions[home].center;
        let mut drops: Vec<DropOption> = (0..map.regions.len())
            .filter(|&r| self.values[r].enemy > 0.0 && Some(r) != self.enemy_base)
            .map(|r| {
                let ground = self.dist_from_ally[r];
                let straight = home_pos.distance(map.regions[r].center).max(CELL);
                let guards = self.guards_vs_ally[r].len();
                let detour = if ground.is_finite() { (ground / straight).max(1.0) } else { 4.0 };
                DropOption {
                    region: r,
                    enemy_value: self.values[r].enemy,
                    ground_distance: ground.is_finite().then_some(ground),
                    guards,
                    score: self.values[r].enemy * detour * (1.0 + guards as f32),
                }
            })
            .collect();
        drops.sort_by(|a, b| b.score.total_cmp(&a.score));
        drops
    }

    fn teleport_options(&self, graph: &RegionGraph) -> Vec<TeleportOption> {
        let enemy_regions: Vec<usize> = (0..self.values.len()).filter(|&r| self.values[r].enemy > 0.0).collect();
        let mut options: Vec<TeleportOption> = (0..self.values.len())
            .filter(|&x| Some(x) != self.enemy_base && Some(x) != self.ally_base)
            .map(|x| {
                let from_exit = graph.distances(x, None);
                let gain = enemy_regions
                    .iter()
                    .map(|&r| {
                        let before = self.dist_from_ally[r];
                        let after = from_exit[r];
                        if !after.is_finite() {
                            0.0
                        } else if !before.is_finite() {
                            self.values[r].enemy
                        } else {
                            self.values[r].enemy * ((before - after).max(0.0) / before.max(1.0))
                        }
                    })
                    .sum();
                TeleportOption { region: x, gain }
            })
            .filter(|t| t.gain > 0.0)
            .collect();
        options.sort_by(|a, b| b.gain.total_cmp(&a.gain));
        options
    }
}

/// Regions as nodes, chokepoints as weighted edges.
struct RegionGraph {
    /// Per region: (neighbour, choke, travel distance through the choke).
    adj: Vec<Vec<(usize, usize, f32)>>,
}

impl RegionGraph {
    fn new(map: &TierMap) -> Self {
        let mut adj = vec![Vec::new(); map.regions.len()];
        for c in &map.chokes {
            for (i, &a) in c.regions.iter().enumerate() {
                for &b in &c.regions[i + 1..] {
                    let w = map.regions[a].center.distance(c.pos) + c.pos.distance(map.regions[b].center);
                    adj[a].push((b, c.id, w));
                    adj[b].push((a, c.id, w));
                }
            }
        }
        RegionGraph { adj }
    }

    /// Dijkstra from `src`, optionally ignoring one chokepoint.
    fn distances(&self, src: usize, without: Option<usize>) -> Vec<f32> {
        let mut dist = vec![f32::INFINITY; self.adj.len()];
        let mut heap = BinaryHeap::new();
        dist[src] = 0.0;
        heap.push(HeapItem(0.0, src));
        while let Some(HeapItem(d, r)) = heap.pop() {
            if d > dist[r] {
                continue;
            }
            for &(n, c, w) in &self.adj[r] {
                if Some(c) == without {
                    continue;
                }
                let nd = d + w;
                if nd < dist[n] {
                    dist[n] = nd;
                    heap.push(HeapItem(nd, n));
                }
            }
        }
        dist
    }

    fn reachable(&self, src: usize, without: Option<usize>, extra: Option<(usize, usize)>) -> Vec<bool> {
        let mut seen = vec![false; self.adj.len()];
        let mut queue = VecDeque::from([src]);
        seen[src] = true;
        while let Some(r) = queue.pop_front() {
            let extra_n = extra.and_then(|(a, b)| {
                if r == a {
                    Some(b)
                } else if r == b {
                    Some(a)
                } else {
                    None
                }
            });
            let ns = self.adj[r].iter().filter(|&&(_, c, _)| Some(c) != without).map(|&(n, _, _)| n);
            for n in ns.chain(extra_n) {
                if !seen[n] {
                    seen[n] = true;
                    queue.push_back(n);
                }
            }
        }
        seen
    }

    /// For every region, the chokepoints all routes from `src` pass.
    fn guards(&self, src: usize) -> Vec<Vec<usize>> {
        self.guards_impl(src, None)
    }

    /// Like [`guards`](Self::guards), with an extra edge between two regions.
    fn guards_with_extra_edge(&self, src: usize, a: usize, b: usize) -> Vec<Vec<usize>> {
        self.guards_impl(src, Some((a, b)))
    }

    fn guards_impl(&self, src: usize, extra: Option<(usize, usize)>) -> Vec<Vec<usize>> {
        let base = self.reachable(src, None, extra);
        let mut guards = vec![Vec::new(); self.adj.len()];
        let mut chokes: Vec<usize> = self.adj.iter().flatten().map(|&(_, c, _)| c).collect();
        chokes.sort_unstable();
        chokes.dedup();
        for c in chokes {
            let cut = self.reachable(src, Some(c), extra);
            for r in 0..self.adj.len() {
                if base[r] && !cut[r] {
                    guards[r].push(c);
                }
            }
        }
        guards
    }
}

#[derive(PartialEq)]
struct HeapItem(f32, usize);

impl Eq for HeapItem {}

impl Ord for HeapItem {
    fn cmp(&self, o: &Self) -> Ordering {
        o.0.total_cmp(&self.0).then_with(|| self.1.cmp(&o.1))
    }
}

impl PartialOrd for HeapItem {
    fn partial_cmp(&self, o: &Self) -> Option<Ordering> {
        Some(self.cmp(o))
    }
}

/// A pair of regions separated by a short impassable stretch.
struct BridgeCandidate {
    from: usize,
    to: usize,
    ends: [Pos; 2],
    span: f32,
}

/// Shortest obstacle crossings between distinct regions: a bounded
/// multi-source BFS from every region's boundary cells through obstacle
/// cells; the first passable cell of another region it meets gives a
/// candidate (shortest per region pair).
fn bridge_candidates(map: &TierMap) -> Vec<BridgeCandidate> {
    let (w, h) = (map.width, map.height);
    let max_steps = (MAX_BRIDGE_SPAN / CELL) as u32;
    // Per obstacle cell: (steps, origin passable cell).
    let mut origin = vec![(u32::MAX, usize::MAX); w * h];
    let mut queue = VecDeque::new();
    let passable = |c: usize| map.component[c] != NONE;
    let neighbours = |c: usize| {
        let (x, z) = (c % w, c / w);
        [(x > 0).then(|| c - 1), (x + 1 < w).then(|| c + 1), (z > 0).then(|| c - w), (z + 1 < h).then(|| c + w)]
            .into_iter()
            .flatten()
    };
    for c in 0..w * h {
        if passable(c) && map.region[c] != NONE && neighbours(c).any(|n| !passable(n)) {
            for n in neighbours(c).filter(|&n| !passable(n)) {
                if origin[n].0 == u32::MAX {
                    origin[n] = (1, c);
                    queue.push_back(n);
                }
            }
        }
    }
    let mut best: BTreeMap<(usize, usize), BridgeCandidate> = BTreeMap::new();
    let mut consider = |a_cell: usize, b_cell: usize, steps: u32| {
        let (ra, rb) = (map.region[a_cell], map.region[b_cell]);
        if ra == NONE || rb == NONE || ra == rb {
            return;
        }
        let (ra, rb) = (ra as usize, rb as usize);
        // Regions already joined by a chokepoint need no bridge.
        if map.neighbours(ra).any(|(_, r)| r == rb) {
            return;
        }
        let (pa, pb) = (map.cell_center(a_cell), map.cell_center(b_cell));
        let span = pa.distance(pb).max(steps as f32 * CELL);
        let key = (ra.min(rb), ra.max(rb));
        let better = best.get(&key).is_none_or(|b| span < b.span);
        if better {
            best.insert(key, BridgeCandidate { from: ra, to: rb, ends: [pa, pb], span });
        }
    };
    // Expand through obstacles; where two fronts from different regions
    // meet, or a front reaches another region's ground, record a crossing.
    while let Some(c) = queue.pop_front() {
        let (steps, from) = origin[c];
        for n in neighbours(c) {
            if passable(n) {
                if n != from {
                    consider(from, n, steps);
                }
            } else if origin[n].0 == u32::MAX {
                if steps < max_steps {
                    origin[n] = (steps + 1, from);
                    queue.push_back(n);
                }
            } else if origin[n].1 != from {
                consider(from, origin[n].1, steps + origin[n].0);
            }
        }
    }
    best.into_values().filter(|b| b.span <= MAX_BRIDGE_SPAN).collect()
}

/// Max-flow network (Edmonds-Karp) for the wall seal computation.
struct FlowNet {
    edges: Vec<Vec<FlowEdge>>,
}

#[derive(Clone, Copy)]
struct FlowEdge {
    to: usize,
    cap: f64,
    /// Index of the reverse edge in `edges[to]`.
    rev: usize,
    choke: Option<usize>,
}

impl FlowNet {
    fn new(n: usize) -> Self {
        FlowNet { edges: vec![Vec::new(); n] }
    }

    /// Undirected edge with capacity `cap` both ways.
    fn add_edge(&mut self, a: usize, b: usize, cap: f64, choke: Option<usize>) {
        let (ia, ib) = (self.edges[a].len(), self.edges[b].len());
        self.edges[a].push(FlowEdge { to: b, cap, rev: ib, choke });
        self.edges[b].push(FlowEdge { to: a, cap, rev: ia, choke });
    }

    /// Chokepoints on a minimum cut between `s` and `t`; empty if the
    /// cut is infinite.
    fn min_cut(&mut self, s: usize, t: usize) -> Vec<usize> {
        loop {
            // BFS for an augmenting path.
            let mut prev: Vec<Option<(usize, usize)>> = vec![None; self.edges.len()];
            let mut queue = VecDeque::from([s]);
            let mut seen = vec![false; self.edges.len()];
            seen[s] = true;
            while let Some(u) = queue.pop_front() {
                for (i, e) in self.edges[u].iter().enumerate() {
                    if e.cap > 1e-9 && !seen[e.to] {
                        seen[e.to] = true;
                        prev[e.to] = Some((u, i));
                        queue.push_back(e.to);
                    }
                }
            }
            if !seen[t] {
                // Cut: edges from the source side to the sink side.
                let mut cut: Vec<usize> = Vec::new();
                for u in (0..self.edges.len()).filter(|&u| seen[u]) {
                    for e in &self.edges[u] {
                        if !seen[e.to]
                            && let Some(c) = e.choke
                        {
                            cut.push(c);
                        }
                    }
                }
                cut.sort_unstable();
                cut.dedup();
                return cut;
            }
            // Bottleneck.
            let mut bottleneck = f64::INFINITY;
            let mut v = t;
            while let Some((u, i)) = prev[v] {
                bottleneck = bottleneck.min(self.edges[u][i].cap);
                v = u;
            }
            if !bottleneck.is_finite() {
                return Vec::new();
            }
            let mut v = t;
            while let Some((u, i)) = prev[v] {
                self.edges[u][i].cap -= bottleneck;
                let rev = self.edges[u][i].rev;
                self.edges[v][rev].cap += bottleneck;
                v = u;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Choke, Region, Tier};

    /// A hand-built map: `#` obstacle, letters regions; chokes are given as
    /// (region, region, cell x, cell z, width in cells).
    fn grid_map(rows: &[&str], chokes: &[(char, char, f32, f32, f32)]) -> TierMap {
        let (w, h) = (rows[0].len(), rows.len());
        let mut region = vec![NONE; w * h];
        for (z, row) in rows.iter().enumerate() {
            for (x, ch) in row.chars().enumerate() {
                if ch != '#' {
                    region[z * w + x] = (ch as u8 - b'a') as u32;
                }
            }
        }
        let n = region.iter().filter(|&&r| r != NONE).max().unwrap() + 1;
        // Components: 4-connected passable cells.
        let mut component = vec![NONE; w * h];
        let mut next = 0;
        for start in 0..w * h {
            if region[start] == NONE || component[start] != NONE {
                continue;
            }
            let mut stack = vec![start];
            component[start] = next;
            while let Some(c) = stack.pop() {
                let (x, z) = (c % w, c / w);
                for (dx, dz) in [(-1i32, 0i32), (1, 0), (0, -1), (0, 1)] {
                    let (nx, nz) = (x as i32 + dx, z as i32 + dz);
                    if nx < 0 || nz < 0 || nx >= w as i32 || nz >= h as i32 {
                        continue;
                    }
                    let nc = nz as usize * w + nx as usize;
                    if region[nc] != NONE && component[nc] == NONE {
                        component[nc] = next;
                        stack.push(nc);
                    }
                }
            }
            next += 1;
        }
        let mut regions: Vec<Region> = (0..n as usize)
            .map(|r| {
                let cells: Vec<usize> = (0..w * h).filter(|&c| region[c] == r as u32).collect();
                let (sx, sz) = cells.iter().fold((0.0, 0.0), |(a, b), &c| (a + (c % w) as f32, b + (c / w) as f32));
                let k = cells.len() as f32;
                Region {
                    id: r,
                    center: Pos::new((sx / k + 0.5) * CELL, (sz / k + 0.5) * CELL),
                    cells: cells.len(),
                    component: component[cells[0]],
                    chokes: Vec::new(),
                }
            })
            .collect();
        let chokes: Vec<Choke> = chokes
            .iter()
            .enumerate()
            .map(|(i, &(a, b, x, z, width))| {
                let (ra, rb) = ((a as u8 - b'a') as usize, (b as u8 - b'a') as usize);
                regions[ra].chokes.push(i);
                regions[rb].chokes.push(i);
                Choke {
                    id: i,
                    pos: Pos::new((x + 0.5) * CELL, (z + 0.5) * CELL),
                    width: width * CELL,
                    gate: None,
                    regions: vec![ra.min(rb), ra.max(rb)],
                }
            })
            .collect();
        TierMap { tier: Tier::Vehicle, width: w, height: h, regions, chokes, component, region, passable_fraction: 1.0 }
    }

    /// Scale a layout up: every character becomes a `k` x `k` block.
    fn scaled(rows: &[&str], k: usize) -> Vec<String> {
        rows.iter()
            .flat_map(|r| {
                let wide: String = r.chars().flat_map(|c| std::iter::repeat_n(c, k)).collect();
                std::iter::repeat_n(wide, k)
            })
            .collect()
    }

    /// Three rooms a-b-c in a row, and below them two strips d (under a)
    /// and e (under c) split by a thin wall; laid out at 4x scale so travel
    /// distances are realistic (~1700 x 450 elmos).
    fn rooms() -> TierMap {
        let rows = scaled(
            &[
                "aaaaaaaa#bbbbbbbb#cccccccc",
                "aaaaaaaa#bbbbbbbb#cccccccc",
                "aaaaaaaaabbbbbbbbbcccccccc",
                "aaaaaaaa#bbbbbbbb#cccccccc",
                "#dd#####################ee",
                "dddddddddddd##eeeeeeeeeeee",
                "dddddddddddd##eeeeeeeeeeee",
            ],
            4,
        );
        let rows: Vec<&str> = rows.iter().map(String::as_str).collect();
        let k = 4.0;
        grid_map(
            &rows,
            &[
                ('a', 'b', 8.0 * k, 2.0 * k, k),
                ('b', 'c', 17.0 * k, 2.0 * k, k),
                ('a', 'd', 1.0 * k, 4.0 * k, 2.0 * k),
                ('c', 'e', 24.0 * k, 4.0 * k, 2.0 * k),
            ],
        )
    }

    /// Center of a layout position (in unscaled layout cells).
    fn at(x: f32, z: f32) -> Pos {
        Pos::new((x * 4.0 + 2.0) * CELL, (z * 4.0 + 2.0) * CELL)
    }

    #[test]
    fn guards_walls_seal_bridges_drops() {
        let map = rooms();
        let assets = [
            Asset { pos: at(12.0, 1.0), value: 10.0, side: Side::Ally },
            Asset { pos: at(22.0, 1.0), value: 10.0, side: Side::Enemy },
            Asset { pos: at(20.0, 6.0), value: 6.0, side: Side::Enemy },
        ];
        let s = Scenario { map: &map, ally_base: at(2.0, 1.0), enemy_base: at(22.0, 0.0), assets: &assets };
        let a = Analysis::run(&s);
        let (ra, rb, rc, rd, re) = (0, 1, 2, 3, 4);
        let (ab, bc, ad, ce) = (0, 1, 2, 3);
        assert_eq!((a.ally_base, a.enemy_base), (Some(ra), Some(rc)));

        // Enemy routes from c: b only via bc; a via bc+ab; d via bc+ab+ad.
        assert_eq!(a.guards_vs_enemy[rb], vec![bc]);
        assert_eq!(a.guards_vs_enemy[ra], vec![ab, bc]);
        assert_eq!(a.guards_vs_enemy[rd], vec![ab, bc, ad]);
        assert_eq!(a.guards_vs_enemy[re], vec![ce]);

        // Walling bc protects our room b. It is not a seal option, though:
        // bc is on the enemy's doorstep.
        assert_eq!(a.walls[0].choke, bc);
        assert!(a.walls[0].regions.contains(&rb));
        assert!(a.seal.is_empty(), "seal: {:?}", a.seal);

        // With our value in a and d only, walling ab protects it all.
        let home_assets = [
            Asset { pos: at(4.0, 1.0), value: 10.0, side: Side::Ally },
            Asset { pos: at(5.0, 6.0), value: 10.0, side: Side::Ally },
            Asset { pos: at(22.0, 1.0), value: 10.0, side: Side::Enemy },
        ];
        let home = Analysis::run(&Scenario { assets: &home_assets, ..s });
        assert_eq!(home.seal, vec![ab]);

        // A bridge through the thin wall between d and e opens a second
        // route: it bypasses every guard on e (ab, bc, ce) and on their base
        // c (ab, bc), and shortens the way to e.
        let bridge = a.bridges.iter().find(|b| (b.from, b.to) == (rd, re)).expect("no d-e bridge");
        assert!(bridge.span <= 9.0 * CELL, "span {}", bridge.span);
        assert_eq!(bridge.guards_bypassed, 5);
        assert!(bridge.enemy_exposed > 0.0);

        // e is the only enemy-valued region besides their base: drop there.
        assert_eq!(a.drops.iter().map(|d| d.region).collect::<Vec<_>>(), vec![re]);
        // A teleporter exit next to their value beats walking there.
        assert!(a.teleports.first().is_some_and(|t| t.region == re || t.region == rb));
    }

    #[test]
    fn min_cut_prefers_narrow_chokes() {
        let mut f = FlowNet::new(4);
        // Two routes 0 -> 3; the cheapest cut takes the narrow choke on
        // each (7 and 8), not the wide entries (1 and 2).
        f.add_edge(0, 1, 100.0, Some(1));
        f.add_edge(1, 3, 5.0, Some(7));
        f.add_edge(0, 2, 100.0, Some(2));
        f.add_edge(2, 3, 50.0, Some(8));
        assert_eq!(f.min_cut(0, 3), vec![7, 8]);

        let mut g = FlowNet::new(3);
        g.add_edge(0, 1, 3.0, Some(4));
        g.add_edge(1, 2, f64::INFINITY, None);
        assert_eq!(g.min_cut(0, 2), vec![4]);
    }
}
