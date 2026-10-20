//! Shortest ground routes over a tier's region graph: straight lines within
//! a region, chokepoint to chokepoint between regions, and crossing a
//! chokepoint into a region costs its climb (see [`Choke::climb`]): routes
//! are directional, uphill is longer than down.
//!
//! [`Choke::climb`]: crate::Choke::climb

use std::collections::BinaryHeap;

use crate::graph::HeapItem;
use crate::{Pos, TierMap};

/// Snap radius for placing positions onto passable ground.
pub const SNAP: f32 = 96.0;

/// Shortest ground routes from one position to every chokepoint, and so to
/// any position on the map.
///
/// The search runs over *sides* of chokepoints: side `k` of chokepoint `c`
/// is standing at it in its region `regions[k]`, numbered `first[c] + k`.
pub struct Paths<'a> {
    map: &'a TierMap,
    origin: Pos,
    /// Region of the origin; `None` off passable ground.
    region: Option<usize>,
    /// Number of each chokepoint's first side.
    first: Vec<usize>,
    /// The chokepoint of each side.
    choke_of: Vec<usize>,
    /// Route length to each side, `INFINITY` if unreachable.
    dist: Vec<f32>,
    /// The side each side is reached from (`None`: straight from the
    /// origin).
    from: Vec<Option<usize>>,
}

impl<'a> Paths<'a> {
    /// Routes from `origin` that never enter a chokepoint for which `closed`
    /// is true (held by the other side, say).
    pub fn new(map: &'a TierMap, origin: Pos, closed: impl Fn(usize) -> bool) -> Self {
        Self::search(map, origin, |c, _| !closed(c))
    }

    /// Routes from `origin` entering chokepoint `c` after `d` elmos only
    /// where `admit(c, d)`, which must refuse every later entry once it
    /// refuses one (a deadline per chokepoint, say).
    pub(crate) fn search(map: &'a TierMap, origin: Pos, admit: impl Fn(usize, f32) -> bool) -> Self {
        let mut first = Vec::with_capacity(map.chokes.len());
        let mut choke_of = Vec::new();
        for c in &map.chokes {
            first.push(choke_of.len());
            choke_of.extend(std::iter::repeat_n(c.id, c.regions.len()));
        }
        let n = choke_of.len();
        let mut dist = vec![f32::INFINITY; n];
        let mut from = vec![None; n];
        let region = map.region_near(origin, SNAP);
        let side = |c: usize, r: usize| map.chokes[c].side(r).map(|k| first[c] + k);
        let mut heap = BinaryHeap::new();
        if let Some(r) = region {
            for &c in &map.regions[r].chokes {
                let d = origin.distance(map.chokes[c].pos);
                if let Some(s) = side(c, r)
                    && admit(c, d)
                    && d < dist[s]
                {
                    dist[s] = d;
                    heap.push(HeapItem(d, s));
                }
            }
        }
        while let Some(HeapItem(d, s)) = heap.pop() {
            if d > dist[s] {
                continue;
            }
            let c = choke_of[s];
            let choke = &map.chokes[c];
            let mut relax = |next: usize, nd: f32, heap: &mut BinaryHeap<HeapItem>| {
                if nd < dist[next] {
                    dist[next] = nd;
                    from[next] = Some(s);
                    heap.push(HeapItem(nd, next));
                }
            };
            // Cross into its other regions.
            for (k, &climb) in choke.climb.iter().enumerate() {
                if first[c] + k != s {
                    relax(first[c] + k, d + climb, &mut heap);
                }
            }
            // On through this side's region to its other chokepoints.
            let r = choke.regions[s - first[c]];
            for &next in map.regions[r].chokes.iter().filter(|&&x| x != c) {
                let nd = d + choke.pos.distance(map.chokes[next].pos);
                if let Some(ns) = side(next, r)
                    && admit(next, nd)
                {
                    relax(ns, nd, &mut heap);
                }
            }
        }
        Paths { map, origin, region, first, choke_of, dist, from }
    }

    /// Region of the origin; `None` off passable ground.
    pub fn origin_region(&self) -> Option<usize> {
        self.region
    }

    /// The side of chokepoint `c` in region `r`.
    fn side(&self, c: usize, r: usize) -> Option<usize> {
        self.map.chokes[c].side(r).map(|k| self.first[c] + k)
    }

    /// Route length to a chokepoint (from either side), `INFINITY` if
    /// unreachable.
    pub fn to_choke(&self, choke: usize) -> f32 {
        let sides = self.first[choke]..self.first[choke] + self.map.chokes[choke].regions.len();
        sides.map(|s| self.dist[s]).fold(f32::INFINITY, f32::min)
    }

    /// Route length to chokepoint `c` standing in region `r` beside it
    /// (having crossed it, if need be), `INFINITY` if unreachable.
    pub fn into_region(&self, c: usize, r: usize) -> f32 {
        self.side(c, r).map_or(f32::INFINITY, |s| self.dist[s])
    }

    /// Route length to the edge of a region (zero for the origin's region),
    /// `INFINITY` if unreachable.
    pub fn to_region(&self, region: usize) -> f32 {
        if Some(region) == self.region {
            return 0.0;
        }
        self.map.regions[region].chokes.iter().map(|&c| self.into_region(c, region)).fold(f32::INFINITY, f32::min)
    }

    /// The side the route to `region` enters it by; `None` for the origin's
    /// region and unreachable ones.
    fn entry_side(&self, region: usize) -> Option<usize> {
        if Some(region) == self.region {
            return None;
        }
        let sides = self.map.regions[region].chokes.iter().filter_map(|&c| self.side(c, region));
        sides.filter(|&s| self.dist[s].is_finite()).min_by(|&a, &b| self.dist[a].total_cmp(&self.dist[b]))
    }

    /// The chokepoint the route to `region` enters it by; `None` for the
    /// origin's region and unreachable ones.
    pub(crate) fn entry(&self, region: usize) -> Option<usize> {
        self.entry_side(region).map(|s| self.choke_of[s])
    }

    /// Which chokepoints the routes to `regions` pass.
    pub(crate) fn chokes_towards(&self, regions: impl IntoIterator<Item = usize>) -> Vec<bool> {
        let mut seen = vec![false; self.dist.len()];
        let mut on = vec![false; self.first.len()];
        for r in regions {
            let mut at = self.entry_side(r);
            while let Some(s) = at.filter(|&s| !seen[s]) {
                seen[s] = true;
                on[self.choke_of[s]] = true;
                at = self.from[s];
            }
        }
        on
    }

    /// Route length to `p`, `None` if unreachable.
    pub fn distance_to(&self, p: Pos) -> Option<f32> {
        self.last_side(p).map(|(_, d)| d)
    }

    /// The chokepoints passed on the way to `p`, in order (empty within the
    /// origin's region or if unreachable).
    pub fn route(&self, p: Pos) -> Vec<usize> {
        let mut route = Vec::new();
        let mut at = self.last_side(p).and_then(|(s, _)| s);
        while let Some(s) = at {
            let prev = self.from[s];
            // A side reached from another side of the same chokepoint
            // crossed it.
            if prev.is_some_and(|q| self.choke_of[q] == self.choke_of[s]) {
                route.push(self.choke_of[s]);
            }
            at = prev;
        }
        route.reverse();
        route
    }

    /// The last side on the way to `p` (`None` within the origin's region)
    /// and the route length.
    fn last_side(&self, p: Pos) -> Option<(Option<usize>, f32)> {
        let start = self.region?;
        let r = self.map.region_near(p, SNAP)?;
        if r == start {
            return Some((None, self.origin.distance(p)));
        }
        self.map.regions[r]
            .chokes
            .iter()
            .filter_map(|&c| Some((self.side(c, r)?, c)))
            .map(|(s, c)| (Some(s), self.dist[s] + self.map.chokes[c].pos.distance(p)))
            .filter(|(_, d)| d.is_finite())
            .min_by(|a, b| a.1.total_cmp(&b.1))
    }

    /// Route length to every side of every chokepoint, by
    /// `first[choke] + k` for its region `regions[k]`; with `first`.
    pub(crate) fn sides(&self) -> (&[usize], &[f32]) {
        (&self.first, &self.dist)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testmaps::{at, rooms};

    #[test]
    fn routes_follow_chokepoints() {
        let map = rooms();
        let (ab, bc, ad, ce) = (0, 1, 2, 3);
        let from_d = Paths::new(&map, at(5.0, 6.0), |_| false);
        assert_eq!(from_d.route(at(20.0, 6.0)), vec![ad, ab, bc, ce]);
        assert_eq!(from_d.route(at(6.0, 5.0)), Vec::<usize>::new());
        assert_eq!(from_d.to_region(3), 0.0);

        // Close bc: room c and strip e are cut off.
        let closed = Paths::new(&map, at(5.0, 6.0), |c| c == bc);
        assert!(closed.distance_to(at(22.0, 1.0)).is_none());
        assert!(closed.to_region(4).is_infinite());
        assert!(closed.distance_to(at(12.0, 1.0)).is_some());
    }

    #[test]
    fn climbing_lengthens_routes_one_way() {
        let mut map = rooms();
        let (ab, a, b) = (0, 0, 1);
        let k = map.chokes[ab].side(b).unwrap();
        map.chokes[ab].climb[k] = 500.0;
        let (pa, pb) = (at(2.0, 1.0), at(12.0, 1.0));
        let up = Paths::new(&map, pa, |_| false);
        let down = Paths::new(&map, pb, |_| false);
        let (d_up, d_down) = (up.distance_to(pb).unwrap(), down.distance_to(pa).unwrap());
        assert!((d_up - d_down - 500.0).abs() < 1.0, "{d_up} {d_down}");
        assert_eq!(up.route(pb), vec![ab]);
        assert_eq!(down.route(pa), vec![ab]);
        // To the edge of b: to the gate, and up into b.
        assert!((up.to_region(b) - pa.distance(map.chokes[ab].pos) - 500.0).abs() < 1.0);
        assert_eq!(up.into_region(ab, a), pa.distance(map.chokes[ab].pos));
        assert_eq!(up.to_choke(ab), pa.distance(map.chokes[ab].pos));
    }
}
