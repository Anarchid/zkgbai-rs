//! Engine-independent A* over the pathfinder's downsampled grid.
//!
//! All scratch state is allocated once per grid size and reused by every
//! query. Per-cell stamps record which search (or query, for the cost cache)
//! a value belongs to, so starting a search is O(1) instead of clearing whole
//! arrays. The open set is a [`BinaryHeap`] with lazy decrease-key: an
//! improved node is pushed again and the superseded entry is skipped when it
//! surfaces, which visits nodes in the same order as the original's
//! remove-and-reinsert.

use std::cmp::Ordering;
use std::collections::BinaryHeap;

/// Cost-function result marking an impassable cell.
pub(super) const IMPASSABLE: f32 = -1.0;

/// Neighbour steps `(dx, dz, cost multiplier)`, in the original's offset order
/// `{-1, 1, w, -w, w+1, w-1, -w+1, -w-1}` (the order matters for tie-breaking).
const NEIGHBOURS: [(isize, isize, f32); 8] =
    [(-1, 0, 1.0), (1, 0, 1.0), (0, 1, 1.0), (0, -1, 1.0), (1, 1, 1.42), (-1, 1, 1.42), (1, -1, 1.42), (-1, -1, 1.42)];

/// An open-set entry (the original `PathNode`).
#[derive(Clone, Copy, Debug)]
struct OpenNode {
    total_cost: f32,
    path_length: f32,
    pos: u32,
    /// Insertion counter, used for tie breaking.
    index: u32,
}

impl Ord for OpenNode {
    fn cmp(&self, other: &Self) -> Ordering {
        // `BinaryHeap` pops the greatest element: lowest total cost first and,
        // on equal cost, the most recently pushed node (LIFO).
        other.total_cost.total_cmp(&self.total_cost).then_with(|| self.index.cmp(&other.index))
    }
}

impl PartialOrd for OpenNode {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl PartialEq for OpenNode {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}

impl Eq for OpenNode {}

#[derive(Clone, Copy, Default)]
struct CellState {
    min_cost: f32,
    cost: f32,
    path_to: u32,
    /// `min_cost` (and `path_to`) belong to the current search iff equal to
    /// `AStar::search_id`.
    seen: u32,
    /// Closed in the current search iff equal to `AStar::search_id`.
    closed: u32,
    /// `cost` is cached for the current query iff equal to `AStar::query_id`.
    costed: u32,
}

/// Variations between the original's four copies of the search loop.
#[derive(Clone, Copy, Debug)]
pub(super) struct SearchOpts {
    /// Treat an impassable target cell as passable (cost 1): due to the low
    /// slope map resolution the unit's own cell may look unpathable, which
    /// would prevent completing a path.
    pub pass_target: bool,
    /// Track the cell closest (by heuristic) to the target in a [`Best`].
    pub track_best: bool,
    /// Record predecessors for [`AStar::trace`].
    pub record_path: bool,
}

/// Reachable cell closest to the target seen so far (the original's
/// `closest`/`bestPos` and `bestDist`).
#[derive(Clone, Copy, Debug)]
pub(super) struct Best {
    pub pos: usize,
    pub dist: f32,
}

impl Best {
    pub fn new(pos: usize) -> Self {
        Best { pos, dist: f32::MAX }
    }
}

/// Reusable A* state for a row-major grid of `width` columns.
pub(super) struct AStar {
    width: usize,
    cells: Vec<CellState>,
    heap: BinaryHeap<OpenNode>,
    search_id: u32,
    query_id: u32,
    pq_index: u32,
}

impl AStar {
    /// Scratch space for a grid of `len` cells in rows of `width`.
    pub fn new(width: usize, len: usize) -> Self {
        let len = if width == 0 { 0 } else { len };
        AStar {
            width,
            cells: vec![CellState::default(); len],
            heap: BinaryHeap::with_capacity(512),
            search_id: 0,
            query_id: 0,
            pq_index: 0,
        }
    }

    pub fn width(&self) -> usize {
        self.width
    }

    pub fn len(&self) -> usize {
        self.cells.len()
    }

    /// Start a new query: invalidates the cost cache. Searches within one
    /// query (the original's restart towards the closest reachable cell)
    /// share cached costs.
    pub fn new_query(&mut self) {
        self.query_id = self.query_id.wrapping_add(1);
        if self.query_id == 0 {
            self.reset_stamps();
            self.query_id = 1;
        }
        self.pq_index = 0;
    }

    fn next_search(&mut self) {
        self.search_id = self.search_id.wrapping_add(1);
        if self.search_id == 0 {
            self.reset_stamps();
            self.search_id = 1;
        }
        self.heap.clear();
    }

    fn reset_stamps(&mut self) {
        self.cells.fill(CellState::default());
    }

    /// The original's heuristic: grid distance where the "row" term is the
    /// fractional `index / width` (so it also carries a small column term).
    #[inline]
    fn heuristic(width: usize, start: usize, target: usize) -> f32 {
        let dx = (start % width) as i32 - (target % width) as i32;
        let w = width as f32;
        let dz = start as f32 / w - target as f32 / w;
        ((dx * dx) as f32 + dz * dz).sqrt()
    }

    /// Cost of the cheapest known path to `cell` in the last search, or
    /// `f32::MAX` if it was never reached.
    pub fn min_cost(&self, cell: usize) -> f32 {
        match self.cells.get(cell) {
            Some(c) if c.seen == self.search_id => c.min_cost,
            _ => f32::MAX,
        }
    }

    /// One A* search from `start` to `target`. `cost(cell)` returns the cell's
    /// cost multiplier or [`IMPASSABLE`]; results are cached per query.
    /// Returns whether `target` was reached.
    pub fn search<F: FnMut(usize) -> f32>(
        &mut self,
        start: usize,
        target: usize,
        opts: SearchOpts,
        best: &mut Best,
        cost: &mut F,
    ) -> bool {
        let len = self.cells.len();
        if start >= len || target >= len {
            return false;
        }
        self.next_search();
        let (sid, qid, width) = (self.search_id, self.query_id, self.width);

        let s = &mut self.cells[start];
        s.min_cost = 0.0;
        s.seen = sid;
        self.heap.push(OpenNode {
            total_cost: Self::heuristic(width, start, target),
            path_length: 0.0,
            pos: start as u32,
            index: self.pq_index,
        });

        while let Some(current) = self.heap.pop() {
            let pos = current.pos as usize;
            // A superseded entry for a node already expanded at lower cost.
            if self.cells[pos].closed == sid {
                continue;
            }
            self.cells[pos].closed = sid;

            // if we reach target pos, we have a complete path.
            if pos == target {
                return true;
            }

            let col = pos % width;
            for &(dx, dz, step_cost) in &NEIGHBOURS {
                // left/right map-edge checks; the index range check covers top/bottom.
                if (dx < 0 && col == 0) || (dx > 0 && col + 1 == width) {
                    continue;
                }
                let n = pos as isize + dz * width as isize + dx;
                if n < 0 || n as usize >= len {
                    continue;
                }
                let neighbor = n as usize;
                let cell = &mut self.cells[neighbor];

                // don't explore the same node twice
                if cell.closed == sid {
                    continue;
                }

                let mut cost_mod = if cell.costed == qid {
                    cell.cost
                } else {
                    let c = cost(neighbor);
                    cell.cost = c;
                    cell.costed = qid;
                    c
                };

                // don't explore unpathable nodes, as they do not form valid paths
                if cost_mod == IMPASSABLE {
                    if opts.pass_target && neighbor == target {
                        cost_mod = 1.0;
                    } else {
                        continue;
                    }
                }

                let templength = current.path_length + step_cost * cost_mod;
                let dist = if opts.track_best {
                    let d = Self::heuristic(width, neighbor, target);
                    if d < best.dist {
                        best.pos = neighbor;
                        best.dist = d;
                    }
                    d
                } else {
                    f32::NAN
                };

                let known = if cell.seen == sid { cell.min_cost } else { f32::MAX };
                if templength < known {
                    if opts.record_path {
                        cell.path_to = pos as u32;
                    }
                    cell.min_cost = templength;
                    cell.seen = sid;
                    self.pq_index = self.pq_index.wrapping_add(1);
                    let dist = if opts.track_best { dist } else { Self::heuristic(width, neighbor, target) };
                    self.heap.push(OpenNode {
                        total_cost: templength + dist,
                        path_length: templength,
                        pos: neighbor as u32,
                        index: self.pq_index,
                    });
                }
            }
        }
        // the open set is empty and we haven't reached the target: unreachable.
        false
    }

    /// The original `findPath` search: A* to `target`, and if that is
    /// unreachable, a second search to the reachable cell closest to it.
    /// Returns the cell the path ends at, or `None` if no cell closer than
    /// `start` is reachable (the caller then heads straight for the target).
    pub fn find_path<F: FnMut(usize) -> f32>(&mut self, start: usize, target: usize, cost: &mut F) -> Option<usize> {
        let opts = SearchOpts { pass_target: true, track_best: true, record_path: true };
        self.new_query();
        let mut best = Best::new(start);
        if self.search(start, target, opts, &mut best, cost) {
            return Some(target);
        }
        if best.pos == start {
            return None;
        }
        // port: the original restarts in a loop; the closest cell is always
        // reachable, so one restart suffices. Bail out instead of looping if
        // that fails anyway (e.g. NaN costs).
        let closest = best.pos;
        self.search(start, closest, opts, &mut best, cost).then_some(closest)
    }

    /// Reachability of every cell towards `origin`, for many targets at once.
    ///
    /// Equivalent to running `search(target, origin)` with `pass_target` for
    /// each target and the same cost function: such a path may enter any
    /// passable cell, and the origin itself regardless of its cost. So after
    /// flooding from `origin` over passable cells into `reached`, a target is
    /// reachable iff it is `origin` or one of its neighbours was reached
    /// (see [`Reach::reaches`]). One flood replaces a full search per target.
    pub fn flood<F: FnMut(usize) -> f32>(&mut self, origin: usize, cost: &mut F, reached: &mut Vec<bool>) {
        let len = self.cells.len();
        reached.clear();
        reached.resize(len, false);
        if origin >= len {
            return;
        }
        reached[origin] = true;
        let width = self.width;
        let mut stack = vec![origin];
        while let Some(pos) = stack.pop() {
            for n in neighbours(width, len, pos) {
                if reached[n] {
                    continue;
                }
                let c = cost(n);
                // Exactly the cells A* would push: not impassable, and a
                // cost that compares (NaN never improves a path length).
                if c != IMPASSABLE && c.partial_cmp(&0.0).is_some() {
                    reached[n] = true;
                    stack.push(n);
                }
            }
        }
    }

    /// Cells of the last recorded path from `start` (exclusive) to `end`
    /// (inclusive), in travel order.
    pub fn trace(&self, start: usize, end: usize, out: &mut Vec<usize>) {
        out.clear();
        let mut pos = end;
        while pos != start {
            // A predecessor chain can't be longer than the grid; bail out
            // rather than loop on corrupt state.
            if out.len() >= self.cells.len() {
                out.clear();
                return;
            }
            out.push(pos);
            pos = self.cells[pos].path_to as usize;
        }
        out.reverse();
    }
}

/// In-grid neighbours of `pos`, in [`NEIGHBOURS`] order.
fn neighbours(width: usize, len: usize, pos: usize) -> impl Iterator<Item = usize> {
    let col = pos % width.max(1);
    NEIGHBOURS.iter().filter_map(move |&(dx, dz, _)| {
        if (dx < 0 && col == 0) || (dx > 0 && col + 1 == width) {
            return None;
        }
        let n = pos as isize + dz * width as isize + dx;
        (n >= 0 && (n as usize) < len).then_some(n as usize)
    })
}

/// Result of [`AStar::flood`]: answers reachability queries towards the
/// flood origin.
pub(super) struct Reach {
    pub width: usize,
    pub origin: usize,
    pub reached: Vec<bool>,
}

impl Reach {
    /// Whether a search from `target` with `pass_target` reaches the origin.
    pub fn reaches(&self, target: usize) -> bool {
        let len = self.reached.len();
        target < len && (target == self.origin || neighbours(self.width, len, target).any(|n| self.reached[n]))
    }
}

/// The original's waypoint thinning of a traced path: drop the first step,
/// then keep every other cell. The final cell is left out; callers append the
/// exact target position instead.
pub(super) fn thin_waypoints(path: &[usize]) -> impl Iterator<Item = usize> + '_ {
    let rest = if path.len() > 1 { &path[1..] } else { path };
    rest[..rest.len().saturating_sub(1)].iter().copied().step_by(2)
}

#[cfg(test)]
mod tests {
    use super::*;

    const W: usize = 10;

    fn cell(x: usize, z: usize) -> usize {
        z * W + x
    }

    fn xz(c: usize) -> (usize, usize) {
        (c % W, c / W)
    }

    /// Flat grid cost: 1 + slope (like the slope-aware cost functions), with
    /// impassable cells where `blocked` says so.
    fn grid_cost(blocked: impl Fn(usize, usize) -> bool) -> impl FnMut(usize) -> f32 {
        move |c| {
            let (x, z) = xz(c);
            if blocked(x, z) { IMPASSABLE } else { 1.0 }
        }
    }

    fn path(
        a: &mut AStar,
        start: usize,
        target: usize,
        cost: &mut impl FnMut(usize) -> f32,
    ) -> (Option<usize>, Vec<usize>) {
        let end = a.find_path(start, target, cost);
        let mut out = Vec::new();
        if let Some(e) = end {
            a.trace(start, e, &mut out);
        }
        (end, out)
    }

    fn assert_connected(start: usize, cells: &[usize]) {
        let mut prev = start;
        for &c in cells {
            let (px, pz) = xz(prev);
            let (x, z) = xz(c);
            assert!(px.abs_diff(x) <= 1 && pz.abs_diff(z) <= 1, "{prev} -> {c} is not a grid step");
            prev = c;
        }
    }

    #[test]
    fn straight_line() {
        let mut a = AStar::new(W, W * W);
        let (start, target) = (cell(0, 5), cell(9, 5));
        let (end, cells) = path(&mut a, start, target, &mut grid_cost(|_, _| false));
        assert_eq!(end, Some(target));
        assert_eq!(cells.len(), 9);
        assert!(cells.iter().all(|&c| xz(c).1 == 5), "{cells:?}");
        assert_eq!(*cells.last().unwrap(), target);
        assert!((a.min_cost(target) - 9.0).abs() < 1e-5);
    }

    #[test]
    fn diagonal_uses_diagonal_steps() {
        let mut a = AStar::new(W, W * W);
        let (end, cells) = path(&mut a, cell(0, 0), cell(6, 6), &mut grid_cost(|_, _| false));
        assert_eq!(end, Some(cell(6, 6)));
        assert_eq!(cells.len(), 6);
        assert!((a.min_cost(cell(6, 6)) - 6.0 * 1.42).abs() < 1e-4);
    }

    #[test]
    fn obstacle_detour() {
        // Wall at x = 5 with a single gap at z = 9.
        let mut a = AStar::new(W, W * W);
        let mut cost = grid_cost(|x, z| x == 5 && z != 9);
        let (start, target) = (cell(0, 0), cell(9, 0));
        let (end, cells) = path(&mut a, start, target, &mut cost);
        assert_eq!(end, Some(target));
        assert!(cells.contains(&cell(5, 9)), "{cells:?}");
        assert!(cells.iter().all(|&c| {
            let (x, z) = xz(c);
            x != 5 || z == 9
        }));
        assert_connected(start, &cells);
    }

    #[test]
    fn unreachable_target_paths_to_nearest_reachable_cell() {
        // Target (8, 5) is boxed in by impassable cells (the map edge closes it
        // on the right).
        let mut a = AStar::new(W, W * W);
        let ring = |x: usize, z: usize| (7..=9).contains(&x) && (2..=8).contains(&z) && (x, z) != (8, 5);
        let start = cell(0, 5);
        let (end, cells) = path(&mut a, start, cell(8, 5), &mut grid_cost(ring));
        // Nearest reachable cell by the heuristic is directly left of the ring.
        assert_eq!(end, Some(cell(6, 5)));
        assert_eq!(cells.last(), Some(&cell(6, 5)));
        assert_connected(start, &cells);
    }

    #[test]
    fn impassable_target_cell_is_still_entered() {
        // Only the target itself is "unpathable" (low-res slope map artefact).
        let mut a = AStar::new(W, W * W);
        let target = cell(7, 7);
        let mut cost = move |c| if c == target { IMPASSABLE } else { 1.0 };
        let (end, _) = path(&mut a, cell(1, 1), target, &mut cost);
        assert_eq!(end, Some(target));

        // Without the exception (isWorkerReachable), it is not.
        a.new_query();
        let opts = SearchOpts { pass_target: false, track_best: true, record_path: false };
        let mut best = Best::new(0);
        assert!(!a.search(cell(1, 1), target, opts, &mut best, &mut cost));
        assert!([cell(7, 6), cell(7, 8)].contains(&best.pos), "{best:?}");
    }

    #[test]
    fn enclosed_start_gives_up() {
        let mut a = AStar::new(W, W * W);
        let start = cell(5, 5);
        let mut cost = grid_cost(|x, z| x.abs_diff(5) <= 1 && z.abs_diff(5) <= 1 && (x, z) != (5, 5));
        assert_eq!(a.find_path(start, cell(0, 0), &mut cost), None);
    }

    #[test]
    fn no_wrap_around_map_edges() {
        // Cells (9, 0) and (0, 1) are adjacent in memory but not on the map.
        let mut a = AStar::new(W, W * W);
        let (start, target) = (cell(9, 0), cell(0, 1));
        let (end, cells) = path(&mut a, start, target, &mut grid_cost(|_, _| false));
        assert_eq!(end, Some(target));
        assert_eq!(cells.len(), 9);
        assert_connected(start, &cells);
    }

    #[test]
    fn grid_corners_and_out_of_range_cells() {
        let mut a = AStar::new(W, W * W);
        let mut cost = grid_cost(|_, _| false);
        let (end, cells) = path(&mut a, cell(9, 9), cell(0, 0), &mut cost);
        assert_eq!(end, Some(cell(0, 0)));
        assert_eq!(cells.len(), 9);
        assert_connected(cell(9, 9), &cells);
        // Indices outside the grid are rejected, not panicked on.
        a.new_query();
        let opts = SearchOpts { pass_target: true, track_best: false, record_path: false };
        assert!(!a.search(0, W * W, opts, &mut Best::new(0), &mut cost));
        // A degenerate grid never finds anything.
        let mut empty = AStar::new(0, 5);
        assert_eq!(empty.len(), 0);
        assert_eq!(empty.find_path(0, 0, &mut cost), None);
    }

    #[test]
    fn same_cell_is_trivially_reached() {
        let mut a = AStar::new(W, W * W);
        let (end, cells) = path(&mut a, cell(3, 3), cell(3, 3), &mut grid_cost(|_, _| false));
        assert_eq!(end, Some(cell(3, 3)));
        assert!(cells.is_empty());
    }

    #[test]
    fn cost_cache_is_per_query() {
        let mut a = AStar::new(W, W * W);
        let calls = std::cell::Cell::new(0);
        let mut cost = |_| {
            calls.set(calls.get() + 1);
            1.0
        };
        a.find_path(cell(0, 0), cell(9, 9), &mut cost);
        let first = calls.get();
        assert!(first > 0 && first <= W * W);
        // A new query must re-evaluate costs (threat maps change between queries).
        a.find_path(cell(0, 0), cell(9, 9), &mut cost);
        assert_eq!(calls.get(), 2 * first);
    }

    #[test]
    fn cheaper_detour_beats_expensive_straight_line() {
        // Row z = 5 is very expensive between x = 2..=7; going around is cheaper.
        let mut a = AStar::new(W, W * W);
        let mut cost = |c| {
            let (x, z) = xz(c);
            if z == 5 && (2..=7).contains(&x) { 50.0 } else { 1.0 }
        };
        let (end, cells) = path(&mut a, cell(0, 5), cell(9, 5), &mut cost);
        assert_eq!(end, Some(cell(9, 5)));
        assert!(
            cells.iter().all(|&c| {
                let (x, z) = xz(c);
                z != 5 || !(2..=7).contains(&x)
            }),
            "{cells:?}"
        );
    }

    #[test]
    fn open_set_order() {
        let mut h = BinaryHeap::new();
        let node = |total_cost, index| OpenNode { total_cost, path_length: 0.0, pos: 0, index };
        h.push(node(3.0, 1));
        h.push(node(1.0, 2));
        h.push(node(1.0, 5));
        h.push(node(2.0, 9));
        let order: Vec<u32> = std::iter::from_fn(|| h.pop()).map(|n| n.index).collect();
        assert_eq!(order, [5, 2, 9, 1]);
    }

    #[test]
    fn flood_matches_per_target_search() {
        // Pseudo-random obstacle grids: every target's reachability from the
        // flood must equal a dedicated pass_target search.
        let mut seed = 0x1234_5678u32;
        let mut rnd = move || {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            seed
        };
        for _ in 0..40 {
            let blocked: Vec<bool> = (0..W * W).map(|_| rnd() % 100 < 38).collect();
            let origin = rnd() as usize % (W * W);
            let mut cost = |c: usize| if blocked[c] { IMPASSABLE } else { 1.0 };
            let mut a = AStar::new(W, W * W);
            let mut reached = Vec::new();
            a.flood(origin, &mut cost, &mut reached);
            let reach = Reach { width: W, origin, reached };
            for target in 0..W * W {
                let opts = SearchOpts { pass_target: true, track_best: false, record_path: false };
                a.new_query();
                let expected = a.search(target, origin, opts, &mut Best::new(target), &mut cost);
                assert_eq!(reach.reaches(target), expected, "target {target} origin {origin}");
            }
        }
    }

    #[test]
    fn waypoint_thinning() {
        let thin = |p: &[usize]| thin_waypoints(p).collect::<Vec<_>>();
        assert_eq!(thin(&[]), Vec::<usize>::new());
        assert_eq!(thin(&[1]), Vec::<usize>::new());
        assert_eq!(thin(&[1, 2]), Vec::<usize>::new());
        assert_eq!(thin(&[1, 2, 3]), [2]);
        assert_eq!(thin(&[1, 2, 3, 4, 5, 6]), [2, 4]);
        assert_eq!(thin(&[1, 2, 3, 4, 5, 6, 7]), [2, 4, 6]);
    }

    #[test]
    fn stamps_survive_wraparound() {
        let mut a = AStar::new(W, W * W);
        a.search_id = u32::MAX - 1;
        a.query_id = u32::MAX - 1;
        let mut cost = grid_cost(|_, _| false);
        for _ in 0..4 {
            let (end, cells) = path(&mut a, cell(0, 0), cell(9, 0), &mut cost);
            assert_eq!(end, Some(cell(9, 0)));
            assert_eq!(cells.len(), 9);
        }
    }
}
