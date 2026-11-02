//! Keeping the model steady across re-analyses.
//!
//! The AI analyses the map again now and then to see terraforming. But the
//! engine's heightmap changes all game long without anyone terraforming on
//! purpose (structures level their footprints, ditches, craters), and zkta
//! numbers regions in an order that a change anywhere can shuffle: on
//! Akilon Wastelands (2026-11-02) a ditch that moved 116 cells of the
//! vehicle tier into other regions renumbered 29 of its 30 regions, so
//! whatever the AI keeps by region or chokepoint id pointed elsewhere. So:
//!
//! - a tier whose input (what stops and what slows its units, its movement
//!   class, the metal spots regions are cut around) has not changed is not
//!   analysed again (see [`Prior`] and [`Terrain::rebuild`]); slopes that
//!   changed without changing what stops or slows units are left
//!   unmeasured (climbs, bypasses, walls, where a border is easiest to
//!   cross);
//! - a tier analysed again takes over the ids of the old model's regions
//!   and chokepoints that are still there ([`TierMap::renumber_after`]).
//!
//! [`Terrain::rebuild`]: crate::Terrain::rebuild

use std::collections::BTreeMap;
use std::hash::{DefaultHasher, Hash, Hasher};

use crate::split::Cap;
use crate::{ChokeKind, MoveClass, NONE, Pos, Tier, TierMap};

/// What a tier's model was built from.
#[derive(Clone, Debug, Default)]
pub(crate) struct Built {
    /// Fingerprint of its input (see [`fingerprint`]).
    pub input: u64,
    /// The piece caps [`split`](crate::split) used.
    pub caps: Vec<Cap>,
}

/// What a re-analysis needs to know of the model it replaces (see
/// [`Terrain::rebuild`](crate::Terrain::rebuild)); small enough to send to
/// the analysis thread. The default knows nothing: every tier is built.
#[derive(Clone, Debug, Default)]
pub struct Prior {
    pub(crate) tiers: Vec<(Tier, Built)>,
}

impl Prior {
    pub(crate) fn tier(&self, tier: Tier) -> Option<&Built> {
        self.tiers.iter().find(|(t, _)| *t == tier).map(|(_, b)| b)
    }
}

/// Fingerprint of everything a tier's regions and chokepoints follow from:
/// the cells that stop its units (`hard`) and that slow them (`soft`), its
/// movement class and the metal spots (in any order).
pub(crate) fn fingerprint(class: MoveClass, hard: &[bool], soft: &[bool], spots: &[Pos]) -> u64 {
    let mut h = DefaultHasher::new();
    [class.max_slope, class.slope_mod, class.max_water_depth, class.min_water_depth].map(f32::to_bits).hash(&mut h);
    class.hover.hash(&mut h);
    hard.hash(&mut h);
    soft.hash(&mut h);
    let mut at: Vec<(u32, u32)> = spots.iter().map(|p| (p.x.to_bits(), p.z.to_bits())).collect();
    at.sort_unstable();
    at.hash(&mut h);
    h.finish()
}

/// How far (elmos) a chokepoint may have moved and still be the one it
/// was: half a border piece.
const CHOKE_MOVE: f32 = 320.0;

/// How a re-analysed tier was numbered after the model it replaced.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Renumbered {
    pub tier: Tier,
    pub regions: usize,
    /// Regions that took over the id of an old region.
    pub regions_kept: usize,
    pub chokes: usize,
    /// Chokepoints that took over the id of an old chokepoint.
    pub chokes_kept: usize,
}

impl TierMap {
    /// Number this model's regions and chokepoints after `old`, a model of
    /// the same tier from before the map changed. A region takes the id of
    /// the old region it shares most ground with, if that is most of its own
    /// ground or most of the old region's (so of two pieces of an old region
    /// only the bigger one keeps its id); a chokepoint takes the id of the
    /// nearest old one of its kind between the same regions, within
    /// [`CHOKE_MOVE`]. The rest take the ids left over, in their order. Ids
    /// stay dense (`0..n`): with fewer regions than before, one numbered past
    /// the new end moves into a gap.
    pub fn renumber_after(&mut self, old: &TierMap) -> Renumbered {
        let (n, m) = (self.regions.len(), old.regions.len());
        let mut done =
            Renumbered { tier: self.tier, regions: n, regions_kept: 0, chokes: self.chokes.len(), chokes_kept: 0 };
        if (self.width, self.height) != (old.width, old.height) {
            return done;
        }

        // Regions, by shared ground.
        let mut shared = vec![0u32; n * m];
        let (mut size, mut old_size) = (vec![0u32; n], vec![0u32; m]);
        for (&a, &b) in self.region.iter().zip(&old.region) {
            let (a, b) = (a as usize, b as usize);
            if a < n {
                size[a] += 1;
            }
            if b < m {
                old_size[b] += 1;
            }
            if a < n && b < m {
                shared[a * m + b] += 1;
            }
        }
        let mut pairs: Vec<(u32, usize, usize)> = (0..n * m)
            .map(|i| (shared[i], i / m, i % m))
            .filter(|&(s, a, b)| s > 0 && (2 * s > size[a] || 2 * s > old_size[b]))
            .collect();
        pairs.sort_unstable_by_key(|&(s, a, b)| (std::cmp::Reverse(s), a, b));
        let matched = assign(n, pairs.into_iter().map(|(_, a, b)| (a, b)));
        done.regions_kept = matched.kept;
        let region_id = matched.ids;

        let mut regions = std::mem::take(&mut self.regions);
        for r in &mut regions {
            r.id = region_id[r.id];
            r.whole = region_id[r.whole];
        }
        regions.sort_unstable_by_key(|r| r.id);
        self.regions = regions;
        for r in self.region.iter_mut().filter(|r| **r != NONE) {
            *r = region_id[*r as usize] as u32;
        }
        for c in &mut self.chokes {
            let mut sides: Vec<(usize, f32, f32)> =
                (0..c.regions.len()).map(|i| (region_id[c.regions[i]], c.climb[i], c.bypass[i])).collect();
            sides.sort_unstable_by_key(|s| s.0);
            c.regions = sides.iter().map(|s| s.0).collect();
            c.climb = sides.iter().map(|s| s.1).collect();
            c.bypass = sides.iter().map(|s| s.2).collect();
        }

        // Chokepoints, by kind, regions and place.
        let kind = |k: ChokeKind| k as u8;
        let mut old_by: BTreeMap<(u8, &[usize]), Vec<usize>> = BTreeMap::new();
        for (j, c) in old.chokes.iter().enumerate() {
            old_by.entry((kind(c.kind), &c.regions)).or_default().push(j);
        }
        let mut pairs: Vec<(f32, usize, usize)> = Vec::new();
        for (i, c) in self.chokes.iter().enumerate() {
            let Some(olds) = old_by.get(&(kind(c.kind), c.regions.as_slice())) else { continue };
            for &j in olds {
                let d = c.pos.distance(old.chokes[j].pos);
                if d <= CHOKE_MOVE {
                    pairs.push((d, i, j));
                }
            }
        }
        pairs.sort_unstable_by(|x, y| x.0.total_cmp(&y.0).then((x.1, x.2).cmp(&(y.1, y.2))));
        let matched = assign(self.chokes.len(), pairs.into_iter().map(|(_, i, j)| (i, j)));
        done.chokes_kept = matched.kept;
        let choke_id = matched.ids;

        let mut chokes = std::mem::take(&mut self.chokes);
        for c in &mut chokes {
            c.id = choke_id[c.id];
        }
        chokes.sort_unstable_by_key(|c| c.id);
        self.chokes = chokes;
        for r in &mut self.regions {
            r.chokes.iter_mut().for_each(|c| *c = choke_id[*c]);
            r.chokes.sort_unstable();
        }
        done
    }
}

/// New ids for `n` items from candidate `(item, old id)` pairs, best
/// first: each item takes the first free old id offered to it below `n`,
/// the rest the ids left over in item order.
struct Assigned {
    ids: Vec<usize>,
    kept: usize,
}

fn assign(n: usize, pairs: impl Iterator<Item = (usize, usize)>) -> Assigned {
    let mut ids = vec![usize::MAX; n];
    let mut taken = vec![false; n];
    let mut kept = 0;
    for (item, old) in pairs {
        if old < n && ids[item] == usize::MAX && !taken[old] {
            ids[item] = old;
            taken[old] = true;
            kept += 1;
        }
    }
    let mut free = (0..n).filter(|&k| !taken[k]);
    for id in ids.iter_mut().filter(|id| **id == usize::MAX) {
        *id = free.next().expect("as many ids as items");
    }
    Assigned { ids, kept }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CELL, SpringMapData, Terrain};

    /// A model cell's world position.
    fn cell_pos(x: usize, z: usize) -> Pos {
        Pos::new((x as f32 + 0.5) * CELL, (z as f32 + 0.5) * CELL)
    }

    /// A flat map `sw` x `sh` slope cells, with `wall` cells impassable.
    fn map(sw: u32, sh: u32, wall: impl Fn(u32, u32) -> bool) -> SpringMapData {
        let mut slope = vec![0.0f32; (sw * sh) as usize];
        for z in 0..sh {
            for x in 0..sw {
                if wall(x, z) {
                    slope[(z * sw + x) as usize] = 1.0;
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

    /// Four rooms in a row, joined by doors in the walls between them;
    /// `speck` adds a pillar in the last room.
    fn rooms(speck: bool) -> SpringMapData {
        map(256, 64, move |x, z| {
            let wall = [60, 124, 188].iter().any(|&w| (w..w + 6).contains(&x)) && !(28..36).contains(&z);
            wall || (speck && (220..226).contains(&x) && (10..16).contains(&z))
        })
    }

    fn check(t: &TierMap) {
        for (i, r) in t.regions.iter().enumerate() {
            assert_eq!(r.id, i);
            assert!(r.whole < t.regions.len());
            for &c in &r.chokes {
                assert!(t.chokes[c].regions.contains(&i), "region {i} lists choke {c}");
            }
        }
        for (i, c) in t.chokes.iter().enumerate() {
            assert_eq!(c.id, i);
            assert!(c.regions.windows(2).all(|w| w[0] < w[1]));
            assert_eq!((c.climb.len(), c.bypass.len()), (c.regions.len(), c.regions.len()));
            for &r in &c.regions {
                assert!(t.regions[r].chokes.contains(&i));
            }
        }
        assert!(t.region.iter().all(|&r| r == NONE || (r as usize) < t.regions.len()));
    }

    #[test]
    fn an_unchanged_tier_is_not_analysed_again() {
        let classes = [(Tier::Bot, Tier::Bot.zero_k_class()), (Tier::Vehicle, Tier::Vehicle.zero_k_class())];
        let t = Terrain::build_with(&rooms(false), &classes);
        assert!(Terrain::rebuild(&rooms(false), &classes, &[], &t.prior()).tiers().is_empty());
        // Spots in another order are the same input; other spots are not.
        let spots = [Pos::new(100.0, 100.0), Pos::new(900.0, 300.0)];
        let t = Terrain::build_with_spots(&rooms(false), &classes, &spots);
        let back = [spots[1], spots[0]];
        assert!(Terrain::rebuild(&rooms(false), &classes, &back, &t.prior()).tiers().is_empty());
        assert_eq!(Terrain::rebuild(&rooms(false), &classes, &spots[..1], &t.prior()).tiers().len(), 2);
        assert_eq!(Terrain::rebuild(&rooms(true), &classes, &spots, &t.prior()).tiers().len(), 2);
    }

    #[test]
    fn a_rebuilt_tier_keeps_the_ids_of_what_is_still_there() {
        let classes = [(Tier::Bot, Tier::Bot.zero_k_class())];
        let mut t = Terrain::build_with(&rooms(false), &classes);
        let before = t.tier(Tier::Bot).unwrap().clone();
        let fresh = Terrain::rebuild(&rooms(true), &classes, &[], &t.prior());
        assert_eq!(fresh.tiers().len(), 1);
        let done = t.absorb(fresh);
        let after = t.tier(Tier::Bot).unwrap();
        check(after);
        assert_eq!(done[0].regions_kept, before.regions.len().min(after.regions.len()), "{done:?}");
        // Every room is where it was, under the same id.
        for x in [20, 90, 150, 240] {
            let p = cell_pos(x, 50);
            assert_eq!(after.region_at(p), before.region_at(p), "room at cell {x}");
        }
        for c in &before.chokes {
            let same = after
                .chokes
                .iter()
                .find(|d| d.kind == c.kind && d.regions == c.regions && d.pos.distance(c.pos) <= CHOKE_MOVE);
            assert_eq!(same.map(|d| d.id), Some(c.id), "{c:?}");
        }
    }

    #[test]
    fn renumbering_a_shuffled_model_restores_it() {
        let classes = [(Tier::Bot, Tier::Bot.zero_k_class())];
        let t = Terrain::build_with(&rooms(false), &classes);
        let old = t.tier(Tier::Bot).unwrap();
        // The same model with its regions and chokepoints in reverse order.
        let mut shuffled = old.clone();
        let (n, k) = (old.regions.len(), old.chokes.len());
        shuffled.regions.reverse();
        shuffled.chokes.reverse();
        for r in &mut shuffled.regions {
            r.id = n - 1 - r.id;
            r.whole = n - 1 - r.whole;
            r.chokes.iter_mut().for_each(|c| *c = k - 1 - *c);
        }
        for c in &mut shuffled.chokes {
            c.id = k - 1 - c.id;
            c.regions.iter_mut().for_each(|r| *r = n - 1 - *r);
            c.regions.reverse();
            c.climb.reverse();
            c.bypass.reverse();
        }
        for r in shuffled.region.iter_mut().filter(|r| **r != NONE) {
            *r = (n - 1) as u32 - *r;
        }
        check(&shuffled);
        let done = shuffled.renumber_after(old);
        check(&shuffled);
        assert_eq!((done.regions_kept, done.chokes_kept), (n, k));
        assert_eq!(shuffled.region, old.region);
        for (a, b) in shuffled.chokes.iter().zip(&old.chokes) {
            assert_eq!((a.id, &a.regions, a.pos, &a.climb), (b.id, &b.regions, b.pos, &b.climb));
        }
        for (a, b) in shuffled.regions.iter().zip(&old.regions) {
            assert_eq!((a.id, a.whole, &a.chokes, a.cells), (b.id, b.whole, &b.chokes, b.cells));
        }
    }

    #[test]
    fn leftover_ids_fill_the_gaps() {
        let ids = assign(3, [(0, 4), (1, 0), (2, 0)].into_iter());
        assert_eq!((ids.ids, ids.kept), (vec![1, 0, 2], 1));
    }
}
