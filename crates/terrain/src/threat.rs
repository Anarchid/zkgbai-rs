//! Threat over time on a tier's region graph: where an enemy force can get
//! to and how soon, which chokepoints held against it stop it, and where
//! our forces can get in its way first.

use crate::graph::FlowNet;
use crate::paths::{Paths, SNAP};
use crate::strategy::{Asset, MAX_HOLD_WIDTH, Side};
use crate::{Pos, TierMap};

/// A force on the move: an enemy raid, one of our squads.
#[derive(Clone, Copy, Debug)]
pub struct Force {
    pub pos: Pos,
    /// Elmos per second.
    pub speed: f32,
    /// Fighting strength, in whatever unit the caller compares forces by.
    pub power: f32,
}

impl Force {
    fn seconds(&self, distance: f32) -> f32 {
        distance / self.speed.max(1.0)
    }
}

/// How soon `force` can reach each region (seconds to its edge; zero for
/// its own), never passing a chokepoint `held` against it; `INFINITY` where
/// it cannot go.
pub fn arrival_times(map: &TierMap, force: &Force, held: impl Fn(usize) -> bool) -> Vec<f32> {
    let paths = Paths::new(map, force.pos, held);
    (0..map.regions.len()).map(|r| force.seconds(paths.to_region(r))).collect()
}

/// A defender sent to stand in a chokepoint.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Block {
    pub choke: usize,
    /// Index into the defenders.
    pub defender: usize,
    /// Seconds the defender arrives before the force.
    pub slack: f32,
}

/// How to keep a force away from our value.
#[derive(Clone, Debug, Default)]
pub struct Interception {
    pub blocks: Vec<Block>,
    /// Allied value the blocks keep the force from (value behind held
    /// chokepoints is safe anyway and not counted).
    pub protected: f32,
    /// Allied value the force can still reach.
    pub exposed: f32,
}

/// Seconds a defender must arrive ahead of the force to block it.
pub const MIN_SLACK: f32 = 2.0;

/// Cost of sending a defender to block a chokepoint, in average asset
/// values (plus as much again for the widest holdable gate): blocking must
/// protect more than this.
pub const BLOCK_COST: f32 = 0.25;

/// The chokepoints where `defenders` can stand before `force` gets there
/// to keep it from as much allied value as possible (a minimum cut between
/// the force and our assets, through chokepoints a defender at least as
/// strong as the force reaches first). Each defender blocks at most one
/// chokepoint; chokepoints `held` against the force are impassable to it.
pub fn intercept(
    map: &TierMap,
    force: &Force,
    held: impl Fn(usize) -> bool,
    defenders: &[Force],
    assets: &[Asset],
) -> Interception {
    let enemy = Paths::new(map, force.pos, &held);
    let Some(source) = enemy.origin_region() else { return Interception::default() };
    let n = map.regions.len();

    // Allied value the force can reach, by region.
    let mut value = vec![0.0f32; n];
    let mut count = 0;
    for a in assets.iter().filter(|a| a.side == Side::Ally) {
        if let Some(r) = map.region_near(a.pos, SNAP) {
            count += 1;
            if enemy.to_region(r).is_finite() {
                value[r] += a.value;
            }
        }
    }
    let total: f32 = value.iter().sum();
    if total <= 0.0 {
        return Interception::default();
    }
    let mean_asset = assets.iter().filter(|a| a.side == Side::Ally).map(|a| a.value).sum::<f32>() / count as f32;

    // Who gets to each chokepoint ahead of the force: (defender, slack).
    let mut first: Vec<Vec<(usize, f32)>> = vec![Vec::new(); map.chokes.len()];
    for (i, d) in defenders.iter().enumerate().filter(|(_, d)| d.power >= force.power) {
        let ours = Paths::new(map, d.pos, |_| false);
        for (c, arrivals) in first.iter_mut().enumerate() {
            let (theirs, mine) = (enemy.to_choke(c), ours.to_choke(c));
            if !theirs.is_finite() || !mine.is_finite() {
                continue;
            }
            let slack = force.seconds(theirs) - d.seconds(mine);
            if slack >= MIN_SLACK {
                arrivals.push((i, slack));
            }
        }
    }

    // Cut, assign defenders, and cut again without the chokepoints left
    // unmanned until every block has a defender.
    let mut unmanned = vec![false; map.chokes.len()];
    loop {
        let sink = n;
        let mut flow = FlowNet::new(n + 1);
        for (c, choke) in map.chokes.iter().enumerate().filter(|&(c, _)| !held(c)) {
            let cap = if first[c].is_empty() || unmanned[c] || choke.width > MAX_HOLD_WIDTH {
                f64::INFINITY
            } else {
                (mean_asset * BLOCK_COST * (1.0 + choke.width / MAX_HOLD_WIDTH)) as f64
            };
            for (i, &a) in choke.regions.iter().enumerate() {
                for &b in &choke.regions[i + 1..] {
                    flow.add_edge(a, b, cap, Some(c));
                }
            }
        }
        for (r, &v) in value.iter().enumerate().filter(|&(r, &v)| r != source && v > 0.0) {
            flow.add_edge(r, sink, v as f64, None);
        }
        let Some(cut) = flow.min_cut(source, sink) else { return Interception::default() };

        // Chokepoints with the fewest candidates first, each taking its
        // defender with the most slack.
        let mut order = cut.chokes.clone();
        order.sort_by_key(|&c| first[c].len());
        let mut used = vec![false; defenders.len()];
        let mut blocks = Vec::new();
        let mut short = false;
        for c in order {
            let best = first[c].iter().filter(|(d, _)| !used[*d]).max_by(|a, b| a.1.total_cmp(&b.1));
            match best {
                Some(&(defender, slack)) => {
                    used[defender] = true;
                    blocks.push(Block { choke: c, defender, slack });
                }
                None => {
                    unmanned[c] = true;
                    short = true;
                }
            }
        }
        if short {
            continue;
        }
        let exposed: f32 = (0..n).filter(|&r| cut.source_side[r]).map(|r| value[r]).sum();
        blocks.sort_by_key(|b| b.choke);
        return Interception { blocks, protected: total - exposed, exposed };
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testmaps::{at, rooms};

    const AB: usize = 0;
    const BC: usize = 1;

    /// A raid in room c, and our value in a, b and d.
    fn raid() -> (Force, [Asset; 3]) {
        let raid = Force { pos: at(22.0, 1.0), speed: 100.0, power: 10.0 };
        let assets = [
            Asset { pos: at(4.0, 1.0), value: 10.0, side: Side::Ally },
            Asset { pos: at(12.0, 1.0), value: 5.0, side: Side::Ally },
            Asset { pos: at(5.0, 6.0), value: 10.0, side: Side::Ally },
        ];
        (raid, assets)
    }

    #[test]
    fn held_chokepoints_stop_a_raid() {
        let map = rooms();
        let (raid, _) = raid();
        let t = arrival_times(&map, &raid, |_| false);
        // c is its own; b is behind one door, a behind two, d behind three.
        assert_eq!(t[2], 0.0);
        assert!(t[1] < t[0] && t[0] < t[3], "{t:?}");
        // With bc held, everything but strip e is out of reach.
        let t = arrival_times(&map, &raid, |c| c == BC);
        assert!(t[0].is_infinite() && t[1].is_infinite() && t[3].is_infinite() && t[4].is_finite());
    }

    #[test]
    fn defenders_block_the_door_they_reach_first() {
        let map = rooms();
        let (raid, assets) = raid();

        // A defender home in room a cannot beat the raid to bc, but holds
        // ab: a and d are safe, b is lost.
        let home = Force { pos: at(2.0, 1.0), speed: 100.0, power: 20.0 };
        let i = intercept(&map, &raid, |_| false, &[home], &assets);
        assert_eq!(i.blocks.iter().map(|b| (b.choke, b.defender)).collect::<Vec<_>>(), vec![(AB, 0)]);
        assert_eq!((i.protected, i.exposed), (20.0, 5.0));

        // A second defender near bc saves b too: one block at bc.
        let near = Force { pos: at(15.0, 1.0), speed: 100.0, power: 20.0 };
        let i = intercept(&map, &raid, |_| false, &[home, near], &assets);
        assert_eq!(i.blocks.iter().map(|b| (b.choke, b.defender)).collect::<Vec<_>>(), vec![(BC, 1)]);
        assert_eq!((i.protected, i.exposed), (25.0, 0.0));

        // Too weak a defender is no block.
        let weak = Force { power: 5.0, ..near };
        let i = intercept(&map, &raid, |_| false, &[weak], &assets);
        assert!(i.blocks.is_empty());
        assert_eq!(i.exposed, 25.0);

        // Nothing to do when bc is held already.
        let i = intercept(&map, &raid, |c| c == BC, &[home], &assets);
        assert!(i.blocks.is_empty());
        assert_eq!((i.protected, i.exposed), (0.0, 0.0));
    }
}
