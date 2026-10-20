//! Threat over time on a tier's region graph: where an enemy force can get
//! to and how soon, which chokepoints held against it stop it, and where
//! our forces can get in its way first.

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
    /// The region the defender keeps the force out of: where it stands.
    pub behind: usize,
    /// Index into the defenders.
    pub defender: usize,
    /// Seconds the defender arrives before the force.
    pub slack: f32,
}

/// How to keep a force away from our value.
///
/// Value counts by urgency: in full where the force already is, less the
/// longer it takes to get there, nothing from the horizon on.
#[derive(Clone, Debug, Default)]
pub struct Interception {
    pub blocks: Vec<Block>,
    /// Urgency-weighted allied value the blocks take away from the force
    /// (by cutting it off or making it go around).
    pub protected: f32,
    /// Urgency-weighted allied value still at stake.
    pub exposed: f32,
}

/// Seconds a defender must arrive ahead of the force to block it.
pub const MIN_SLACK: f32 = 2.0;

/// Cost of sending a defender to block a chokepoint, in average asset
/// values (plus as much again for the widest holdable gate): a block must
/// protect more than this.
pub const BLOCK_COST: f32 = 0.25;

/// How much of a value counts that a force reaches in `t` seconds.
fn urgency(t: f32, horizon: f32) -> f32 {
    if t.is_finite() { (1.0 - t / horizon).max(0.0) } else { 0.0 }
}

/// Where `defenders` can stand before `force` gets there to keep it from as
/// much allied value as they can within `horizon` seconds of it. Blocks are
/// chosen greedily, the one that takes most value at stake away first, at
/// chokepoints no wider than [`MAX_HOLD_WIDTH`] that a defender at least as
/// strong as the force reaches first; each defender blocks at most one, the
/// weakest that will do. Chokepoints `held` against the force are
/// impassable to it.
pub fn intercept(
    map: &TierMap,
    force: &Force,
    held: impl Fn(usize) -> bool,
    defenders: &[Force],
    assets: &[Asset],
    horizon: f32,
) -> Interception {
    let enemy = Paths::new(map, force.pos, &held);
    if enemy.origin_region().is_none() {
        return Interception::default();
    }

    // Allied value by region.
    let mut value = vec![0.0f32; map.regions.len()];
    let mut count = 0;
    for a in assets.iter().filter(|a| a.side == Side::Ally) {
        if let Some(r) = map.region_near(a.pos, SNAP) {
            count += 1;
            value[r] += a.value;
        }
    }
    let at_stake = |paths: &Paths| -> f32 {
        value.iter().enumerate().map(|(r, &v)| v * urgency(force.seconds(paths.to_region(r)), horizon)).sum()
    };
    let before = at_stake(&enemy);
    if before <= 0.0 {
        return Interception::default();
    }
    let mean_asset = value.iter().sum::<f32>() / count as f32;
    let valued: Vec<usize> = (0..value.len()).filter(|&r| value[r] > 0.0).collect();

    // The front a block must cover: on the side away from the force.
    let front = |c: usize| {
        let choke = &map.chokes[c];
        let behind = choke.regions.iter().copied().max_by(|&a, &b| enemy.to_region(a).total_cmp(&enemy.to_region(b)));
        behind.map_or(choke.width, |r| choke.hold_width(r))
    };
    let fronts: Vec<f32> = (0..map.chokes.len()).map(front).collect();
    let front = |c: usize| fronts[c];

    // Who gets to each chokepoint ahead of the force: (defender, slack).
    let mut first: Vec<Vec<(usize, f32)>> = vec![Vec::new(); map.chokes.len()];
    for (i, d) in defenders.iter().enumerate().filter(|(_, d)| d.power >= force.power) {
        let ours = Paths::new(map, d.pos, |_| false);
        for (c, arrivals) in first.iter_mut().enumerate().filter(|&(c, _)| front(c) <= MAX_HOLD_WIDTH) {
            let (theirs, mine) = (enemy.to_choke(c), ours.to_choke(c));
            if !theirs.is_finite() || !mine.is_finite() || force.seconds(theirs) > horizon {
                continue;
            }
            let slack = force.seconds(theirs) - d.seconds(mine);
            if slack >= MIN_SLACK {
                arrivals.push((i, slack));
            }
        }
    }

    let mut used = vec![false; defenders.len()];
    let mut blocks: Vec<Block> = Vec::new();
    let mut paths = enemy;
    let mut exposed = before;
    loop {
        // Only closing a chokepoint on the force's way to our value can
        // change how soon it gets there.
        let on_route = paths.chokes_towards(valued.iter().copied());
        let mut best: Option<(f32, usize, Paths, f32)> = None;
        for c in (0..map.chokes.len()).filter(|&c| on_route[c] && first[c].iter().any(|&(d, _)| !used[d])) {
            let closed = |x: usize| held(x) || x == c || blocks.iter().any(|b| b.choke == x);
            let after = Paths::new(map, force.pos, closed);
            let left = at_stake(&after);
            let cost = mean_asset * BLOCK_COST * (1.0 + front(c) / MAX_HOLD_WIDTH);
            let net = exposed - left - cost;
            if net > 0.0 && best.as_ref().is_none_or(|b| net > b.0) {
                best = Some((net, c, after, left));
            }
        }
        let Some((_, c, after, left)) = best else { break };
        let candidates = first[c].iter().filter(|&&(d, _)| !used[d]);
        let by_power = |a: &&(usize, f32), b: &&(usize, f32)| {
            defenders[a.0].power.total_cmp(&defenders[b.0].power).then(b.1.total_cmp(&a.1))
        };
        let Some(&(defender, slack)) = candidates.min_by(by_power) else { break };
        used[defender] = true;
        // The defender stands on the side the force would come out on.
        let regions = &map.chokes[c].regions;
        let enemy_side = |r: usize| paths.to_region(r);
        let behind = regions.iter().copied().max_by(|&a, &b| enemy_side(a).total_cmp(&enemy_side(b)));
        blocks.push(Block { choke: c, behind: behind.unwrap_or(regions[0]), defender, slack });
        paths = after;
        exposed = left;
    }
    blocks.sort_by_key(|b| b.choke);
    Interception { blocks, protected: before - exposed, exposed }
}

/// Where one force can get to, its routes closed where held against it.
#[derive(Clone, Debug)]
struct Reach {
    force: Force,
    region: Option<usize>,
    /// Number of each chokepoint's first side (see [`Paths`]).
    first: Vec<usize>,
    /// Seconds to each side of each chokepoint: to stand beside it in its
    /// region `regions[k]` at `first[c] + k`.
    to_side: Vec<f32>,
}

impl Reach {
    /// Seconds to `p` in region `r`, `INFINITY` if unreachable.
    fn to(&self, map: &TierMap, r: usize, p: Pos) -> f32 {
        if Some(r) == self.region {
            return self.force.seconds(self.force.pos.distance(p));
        }
        let via = |c: usize| {
            let side = map.chokes[c].side(r).map_or(f32::INFINITY, |k| self.to_side[self.first[c] + k]);
            side + self.force.seconds(map.chokes[c].pos.distance(p))
        };
        map.regions[r].chokes.iter().map(|&c| via(c)).fold(f32::INFINITY, f32::min)
    }
}

/// The enemy forces about, combined: how soon the first of them can get to
/// each region and chokepoint, and how strong it is.
#[derive(Clone, Debug, Default)]
pub struct Danger {
    reaches: Vec<Reach>,
    /// Seconds until the first force reaches each region's edge (zero
    /// where one is already); `INFINITY` where none can.
    pub regions: Vec<f32>,
    /// Power of the first force to reach each region.
    pub first: Vec<f32>,
    /// Seconds until the first force reaches each chokepoint.
    pub chokes: Vec<f32>,
}

impl Danger {
    pub fn new(map: &TierMap) -> Self {
        Danger {
            reaches: Vec::new(),
            regions: vec![f32::INFINITY; map.regions.len()],
            first: vec![0.0; map.regions.len()],
            chokes: vec![f32::INFINITY; map.chokes.len()],
        }
    }

    /// Add a force that never passes chokepoints `held` against it.
    pub fn add(&mut self, map: &TierMap, force: Force, held: impl Fn(usize) -> bool) {
        let paths = Paths::new(map, force.pos, held);
        let to_choke: Vec<f32> = (0..map.chokes.len()).map(|c| force.seconds(paths.to_choke(c))).collect();
        for (soonest, &t) in self.chokes.iter_mut().zip(&to_choke) {
            *soonest = soonest.min(t);
        }
        for r in 0..map.regions.len() {
            let t = force.seconds(paths.to_region(r));
            if t < self.regions[r] {
                self.regions[r] = t;
                self.first[r] = force.power;
            }
        }
        let (first, sides) = paths.sides();
        let to_side = sides.iter().map(|&d| force.seconds(d)).collect();
        self.reaches.push(Reach { force, region: paths.origin_region(), first: first.to_vec(), to_side });
    }

    /// How soon the first force can get to `p`, and how strong it is;
    /// `None` if none can.
    pub fn at(&self, map: &TierMap, p: Pos) -> Option<(f32, f32)> {
        let region = map.region_near(p, SNAP)?;
        self.reaches
            .iter()
            .map(|r| (r.to(map, region, p), r.force.power))
            .filter(|(t, _)| t.is_finite())
            .min_by(|a, b| a.0.total_cmp(&b.0))
    }
}

/// Where to flee to.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Refuge {
    pub region: usize,
    /// Just inside the chokepoint the flight enters the region by.
    pub pos: Pos,
    /// Seconds to get there.
    pub eta: f32,
    /// Seconds until the first enemy gets there (`INFINITY`: none can).
    pub enemy: f32,
}

/// How far inside a refuge's entrance a fleeing unit stops, elmos.
pub const REFUGE_SETBACK: f32 = 300.0;

/// Where `unit` should flee from `danger`: the region it can get to without
/// passing a chokepoint less than `margin` seconds ahead of every enemy,
/// with the most time to spare once there (an enemy arrival `cap` seconds
/// or more away counts as never, so then the nearest such region); `None`
/// if staying put is as good.
pub fn refuge(map: &TierMap, unit: &Force, danger: &Danger, margin: f32, cap: f32) -> Option<Refuge> {
    let paths = Paths::search(map, unit.pos, |c, d| unit.seconds(d) + margin <= danger.chokes[c]);
    let here = paths.origin_region()?;
    let spare = |r: usize, eta: f32| danger.regions[r].min(cap) - eta;
    let (region, eta) = (0..map.regions.len())
        .map(|r| (r, unit.seconds(paths.to_region(r))))
        .filter(|&(_, eta)| eta.is_finite())
        .max_by(|&(a, ta), &(b, tb)| spare(a, ta).total_cmp(&spare(b, tb)))?;
    if region == here || spare(region, eta) <= spare(here, 0.0) {
        return None;
    }
    let entry = paths.entry(region)?;
    Some(Refuge { region, pos: map.setback(entry, region, REFUGE_SETBACK), eta, enemy: danger.regions[region] })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testmaps::{at, ring, rooms};

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
        let i = intercept(&map, &raid, |_| false, &[home], &assets, f32::INFINITY);
        assert_eq!(i.blocks.iter().map(|b| (b.choke, b.defender)).collect::<Vec<_>>(), vec![(AB, 0)]);
        assert_eq!((i.protected, i.exposed), (20.0, 5.0));

        // A second defender near bc saves b too: one block at bc.
        let near = Force { pos: at(15.0, 1.0), speed: 100.0, power: 20.0 };
        let i = intercept(&map, &raid, |_| false, &[home, near], &assets, f32::INFINITY);
        assert_eq!(i.blocks.iter().map(|b| (b.choke, b.defender)).collect::<Vec<_>>(), vec![(BC, 1)]);
        assert_eq!((i.protected, i.exposed), (25.0, 0.0));

        // Of two that get there in time, the weaker goes.
        let strong = Force { power: 50.0, ..home };
        let i = intercept(&map, &raid, |_| false, &[strong, home], &assets, f32::INFINITY);
        assert_eq!(i.blocks.iter().map(|b| (b.choke, b.defender)).collect::<Vec<_>>(), vec![(AB, 1)]);

        // Too weak a defender is no block.
        let weak = Force { power: 5.0, ..near };
        let i = intercept(&map, &raid, |_| false, &[weak], &assets, f32::INFINITY);
        assert!(i.blocks.is_empty());
        assert_eq!(i.exposed, 25.0);

        // Each stands on the side it defends.
        let i = intercept(&map, &raid, |_| false, &[home, near], &assets, f32::INFINITY);
        assert_eq!(i.blocks[0].behind, 1);
        let i = intercept(&map, &raid, |_| false, &[home], &assets, f32::INFINITY);
        assert_eq!(i.blocks[0].behind, 0);

        // Within a short horizon b (3.5 s away) is at stake and a (9.3 s)
        // barely is; d (14 s) is not: holding ab is not worth it, holding
        // bc is.
        let i = intercept(&map, &raid, |_| false, &[home], &assets, 10.0);
        assert!(i.blocks.is_empty());
        assert!(i.protected == 0.0 && (3.0..4.5).contains(&i.exposed), "{i:?}");
        let at_stake = i.exposed;
        let i = intercept(&map, &raid, |_| false, &[home, near], &assets, 10.0);
        assert_eq!(i.blocks.iter().map(|b| b.choke).collect::<Vec<_>>(), vec![BC]);
        assert_eq!((i.protected, i.exposed), (at_stake, 0.0));

        // Nothing to do when bc is held already.
        let i = intercept(&map, &raid, |c| c == BC, &[home], &assets, f32::INFINITY);
        assert!(i.blocks.is_empty());
        assert_eq!((i.protected, i.exposed), (0.0, 0.0));
    }

    #[test]
    fn a_block_that_only_forces_a_detour_still_pays() {
        let map = ring();
        let door = 0;
        // A raid in b, our value in a: through the door it is 10 s away,
        // round by the corridor 36 s.
        let raid = Force { pos: at(20.0, 1.0), speed: 50.0, power: 10.0 };
        let assets = [Asset { pos: at(10.0, 1.0), value: 10.0, side: Side::Ally }];
        let guard = Force { pos: at(6.0, 1.0), speed: 100.0, power: 20.0 };
        let i = intercept(&map, &raid, |_| false, &[guard], &assets, 60.0);
        assert_eq!(i.blocks.iter().map(|b| (b.choke, b.behind)).collect::<Vec<_>>(), vec![(door, 0)]);
        // Less at stake, but a is still reachable the long way.
        assert!(i.protected > 3.0 && i.exposed > 3.0, "{i:?}");
    }

    #[test]
    fn danger_is_the_first_enemy_anywhere() {
        let map = rooms();
        let (raid, _) = raid();
        let mut danger = Danger::new(&map);
        danger.add(&map, raid, |_| false);
        let slow = Force { pos: at(2.0, 1.0), speed: 10.0, power: 99.0 };
        danger.add(&map, slow, |_| false);
        // b is the raid's (3.5 s to its edge, a little more inside); a is
        // the slow force's own room.
        let (t, power) = danger.at(&map, at(12.0, 1.0)).unwrap();
        assert!((6.0..7.0).contains(&t) && power == 10.0, "{t} {power}");
        assert_eq!((danger.regions[0], danger.first[0]), (0.0, 99.0));
        assert!((danger.regions[1] - 3.5).abs() < 0.2, "{:?}", danger.regions);
    }

    #[test]
    fn workers_flee_behind_held_chokepoints() {
        let map = rooms();
        let (raid, _) = raid();
        let worker = Force { pos: at(12.0, 1.0), speed: 50.0, power: 0.0 };

        // Nothing held: a is barely safer than b, d out of reach in time.
        let mut danger = Danger::new(&map);
        danger.add(&map, raid, |_| false);
        let r = refuge(&map, &worker, &danger, 2.0, 30.0).unwrap();
        assert_eq!(r.region, 0);
        assert!(r.enemy - r.eta < 5.0, "{r:?}");

        // With ab held (or screened) against the raid, behind it is safe
        // for good: the worker goes there, just inside the door.
        let mut danger = Danger::new(&map);
        danger.add(&map, raid, |c| c == AB);
        let r = refuge(&map, &worker, &danger, 2.0, 30.0).unwrap();
        assert_eq!(r.region, 0);
        assert!(r.enemy.is_infinite());
        assert!(r.pos.distance(map.chokes[AB].pos) < REFUGE_SETBACK + 1.0, "{r:?}");

        // Held at bc, b itself is safe: stay.
        let mut danger = Danger::new(&map);
        danger.add(&map, raid, |c| c == BC);
        assert_eq!(refuge(&map, &worker, &danger, 2.0, 30.0), None);

        // Far away in d with nothing held, staying beats running at it.
        let mut danger = Danger::new(&map);
        danger.add(&map, raid, |_| false);
        let far = Force { pos: at(5.0, 6.0), ..worker };
        assert_eq!(refuge(&map, &far, &danger, 2.0, 30.0), None);
    }
}
