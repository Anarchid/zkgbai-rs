//! Who holds the regions of a tier's map: the enemy's fighting strength,
//! anti-air and value in each, and ours, from what the caller knows of the
//! units and buildings on the map; and the ground ways a force has, if it
//! must fight its way through what holds them.

use std::collections::BinaryHeap;

use crate::graph::HeapItem;
use crate::paths::SNAP;
use crate::strategy::Side;
use crate::{Pos, TierMap};

/// A unit or building on the map, as the caller knows it.
#[derive(Clone, Copy, Debug)]
pub struct Presence {
    pub pos: Pos,
    pub side: Side,
    /// Strength against ground forces, in whatever unit the caller
    /// compares forces by.
    pub power: f32,
    /// Strength against aircraft, likewise.
    pub aa: f32,
    pub value: f32,
}

/// What stands in a region.
#[derive(Clone, Copy, Debug, Default)]
pub struct Hold {
    pub enemy_power: f32,
    pub enemy_aa: f32,
    pub enemy_value: f32,
    pub ally_power: f32,
    pub ally_value: f32,
}

impl Hold {
    /// Whose the region is: the side whose strength there is greater, or
    /// whose value only stands there; `Neutral` with neither.
    pub fn holder(&self) -> Side {
        let (e, a) = (self.enemy_power, self.ally_power);
        if e > a || (e == a && self.enemy_value > self.ally_value) {
            Side::Enemy
        } else if a > e || self.ally_value > self.enemy_value {
            Side::Ally
        } else {
            Side::Neutral
        }
    }
}

/// Elmos a force's way grows by for every region it must fight through, at
/// an enemy there as strong as itself (in proportion for weaker ones).
pub const FIGHT_TOLL: f32 = 2000.0;

/// The regions of one tier's map and what holds them.
#[derive(Clone, Debug, Default)]
pub struct Occupancy {
    pub regions: Vec<Hold>,
}

impl Occupancy {
    /// Sort `presence` into the regions of `map` (a unit off its ground,
    /// a ship on a bot's map say, holds none of it).
    pub fn new(map: &TierMap, presence: &[Presence]) -> Self {
        let mut regions = vec![Hold::default(); map.regions.len()];
        for p in presence {
            let Some(r) = map.region_near(p.pos, SNAP) else { continue };
            let h = &mut regions[r];
            match p.side {
                Side::Enemy => {
                    h.enemy_power += p.power;
                    h.enemy_aa += p.aa;
                    h.enemy_value += p.value;
                }
                Side::Ally => {
                    h.ally_power += p.power;
                    h.ally_value += p.value;
                }
                Side::Neutral => {}
            }
        }
        Occupancy { regions }
    }

    /// The length of a ground way from `from` to the middle of each region
    /// for a force `power` strong that fights through what holds the regions
    /// it passes: each costs [`FIGHT_TOLL`] elmos more for an enemy as strong
    /// as the force, and one where the enemy is stronger closes the way
    /// (the force does not get through). `INFINITY` where no way is open;
    /// `INFINITY` for `power` leaves every way open, at no toll.
    pub fn fighting_distances(&self, map: &TierMap, from: Pos, power: f32) -> Vec<f32> {
        let n = map.regions.len();
        let mut dist = vec![f32::INFINITY; n];
        let Some(start) = map.region_near(from, SNAP) else { return dist };
        let toll = |r: usize| {
            let e = self.regions.get(r).map_or(0.0, |h| h.enemy_power);
            if power.is_infinite() || e <= 0.0 {
                Some(0.0)
            } else if e > power {
                None
            } else {
                Some(FIGHT_TOLL * e / power.max(1.0))
            }
        };
        let mut heap = BinaryHeap::new();
        // Where the force stands it is already.
        dist[start] = from.distance(map.regions[start].center);
        heap.push(HeapItem(dist[start], start));
        while let Some(HeapItem(d, r)) = heap.pop() {
            if d > dist[r] {
                continue;
            }
            let here = map.regions[r].center;
            for (c, next) in map.neighbours(r) {
                let Some(t) = toll(next) else { continue };
                let gate = map.chokes[c].pos;
                let nd = d + here.distance(gate) + gate.distance(map.regions[next].center) + t;
                if nd < dist[next] {
                    dist[next] = nd;
                    heap.push(HeapItem(nd, next));
                }
            }
        }
        dist
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testmaps::{at, rooms};

    fn enemy(pos: Pos, power: f32) -> Presence {
        Presence { pos, side: Side::Enemy, power, aa: 0.0, value: 100.0 }
    }

    #[test]
    fn strength_in_the_way_lengthens_or_closes_it() {
        // Rooms a-b-c in a row, strips d (under a) and e (under c).
        let map = rooms();
        let (b, c, e) = (1, 2, 4);
        let home = at(2.0, 1.0);
        let open = Occupancy::new(&map, &[]).fighting_distances(&map, home, 100.0);
        let free = Occupancy::new(&map, &[]).fighting_distances(&map, home, f32::INFINITY);
        assert_eq!(open, free);
        assert!(free[c].is_finite() && free[e] > free[c]);

        // A weaker enemy in room b: the way through it is longer by a share
        // of the toll, and so is all beyond it.
        let weak = Occupancy::new(&map, &[enemy(at(12.0, 1.0), 50.0)]);
        assert_eq!(weak.regions[b].holder(), Side::Enemy);
        let d = weak.fighting_distances(&map, home, 100.0);
        assert!((d[c] - free[c] - FIGHT_TOLL / 2.0).abs() < 1.0, "{} vs {}", d[c], free[c]);

        // A stronger one closes the only way to c and e; a force stronger
        // still gets through, and no force at all is turned back by it.
        let strong = Occupancy::new(&map, &[enemy(at(12.0, 1.0), 300.0)]);
        let d = strong.fighting_distances(&map, home, 100.0);
        assert!(d[b].is_infinite() && d[c].is_infinite() && d[e].is_infinite());
        assert!(d[3].is_finite());
        assert!(strong.fighting_distances(&map, home, 400.0)[e].is_finite());
        assert_eq!(strong.fighting_distances(&map, home, f32::INFINITY), free);
    }
}
