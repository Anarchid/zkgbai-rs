//! Shortest ground routes over a tier's region graph: straight lines within
//! a region, chokepoint to chokepoint between regions.

use std::collections::BinaryHeap;

use crate::graph::HeapItem;
use crate::{Pos, TierMap};

/// Snap radius for placing positions onto passable ground.
pub(crate) const SNAP: f32 = 96.0;

/// Shortest ground routes from one position to every chokepoint, and so to
/// any position on the map.
pub struct Paths<'a> {
    map: &'a TierMap,
    origin: Pos,
    /// Region of the origin; `None` off passable ground.
    region: Option<usize>,
    /// Route length to each chokepoint, `INFINITY` if unreachable.
    to_choke: Vec<f32>,
    /// The chokepoint each chokepoint is reached from (`None`: straight
    /// from the origin).
    from: Vec<Option<usize>>,
}

impl<'a> Paths<'a> {
    /// Routes from `origin` that never enter a chokepoint for which `closed`
    /// is true (held by the other side, say).
    pub fn new(map: &'a TierMap, origin: Pos, closed: impl Fn(usize) -> bool) -> Self {
        let n = map.chokes.len();
        let mut to_choke = vec![f32::INFINITY; n];
        let mut from = vec![None; n];
        let region = map.region_near(origin, SNAP);
        let mut heap = BinaryHeap::new();
        for &c in region.map_or(&[][..], |r| &map.regions[r].chokes) {
            if !closed(c) {
                to_choke[c] = origin.distance(map.chokes[c].pos);
                heap.push(HeapItem(to_choke[c], c));
            }
        }
        while let Some(HeapItem(d, c)) = heap.pop() {
            if d > to_choke[c] {
                continue;
            }
            let choke = &map.chokes[c];
            for &r in &choke.regions {
                for &next in &map.regions[r].chokes {
                    let nd = d + choke.pos.distance(map.chokes[next].pos);
                    if nd < to_choke[next] && !closed(next) {
                        to_choke[next] = nd;
                        from[next] = Some(c);
                        heap.push(HeapItem(nd, next));
                    }
                }
            }
        }
        Paths { map, origin, region, to_choke, from }
    }

    /// Region of the origin; `None` off passable ground.
    pub fn origin_region(&self) -> Option<usize> {
        self.region
    }

    /// Route length to a chokepoint, `INFINITY` if unreachable.
    pub fn to_choke(&self, choke: usize) -> f32 {
        self.to_choke[choke]
    }

    /// Route length to the edge of a region (zero for the origin's region),
    /// `INFINITY` if unreachable.
    pub fn to_region(&self, region: usize) -> f32 {
        if Some(region) == self.region {
            return 0.0;
        }
        self.map.regions[region].chokes.iter().map(|&c| self.to_choke[c]).fold(f32::INFINITY, f32::min)
    }

    /// Route length to `p`, `None` if unreachable.
    pub fn distance_to(&self, p: Pos) -> Option<f32> {
        self.last_choke(p).map(|(_, d)| d)
    }

    /// The chokepoints passed on the way to `p`, in order (empty within the
    /// origin's region or if unreachable).
    pub fn route(&self, p: Pos) -> Vec<usize> {
        let mut route = Vec::new();
        let mut at = self.last_choke(p).and_then(|(c, _)| c);
        while let Some(c) = at {
            route.push(c);
            at = self.from[c];
        }
        route.reverse();
        route
    }

    /// The last chokepoint on the way to `p` (`None` within the origin's
    /// region) and the route length.
    fn last_choke(&self, p: Pos) -> Option<(Option<usize>, f32)> {
        let start = self.region?;
        let r = self.map.region_near(p, SNAP)?;
        if r == start {
            return Some((None, self.origin.distance(p)));
        }
        self.map.regions[r]
            .chokes
            .iter()
            .map(|&c| (Some(c), self.to_choke[c] + self.map.chokes[c].pos.distance(p)))
            .filter(|(_, d)| d.is_finite())
            .min_by(|a, b| a.1.total_cmp(&b.1))
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
}
