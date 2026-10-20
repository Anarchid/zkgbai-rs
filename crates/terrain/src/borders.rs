//! Every way between regions, not only zkta's gates: where two regions
//! touch without a gate (over slopes units crawl up but run down, or on
//! open ground the decomposition split without one) the region graph gets
//! a *border* chokepoint. Each chokepoint is measured on the ground: how
//! much longer than on flat ground crossing it takes either way, for gates
//! how much longer going round them over their sides takes, and how much
//! wall would close it.

use std::collections::{BTreeMap, BinaryHeap, HashMap, HashSet};

use crate::build::Ground;
use crate::graph::HeapItem;
use crate::{CELL, Choke, ChokeKind, NONE, Pos, TierMap};

/// Where a gate's regions touch within this many elmos of its gate line,
/// over slopes or open ground, belongs to the gate (its sides): defenses at
/// the gate cover it, and a wall closing the gate must close it too.
pub const FLANK_REACH: f32 = 320.0;

/// Where a gate's regions touch this close (elmos) to its gate line counts
/// as the gate itself, not a way round it: a wall or ditch across the gate
/// overhangs its ends by this much.
pub const GATE_MARGIN: f32 = 48.0;

/// Borders longer than this (elmos) are split, so that each piece has a
/// crossing of its own.
const BORDER_PIECE: f32 = 640.0;

/// How far around a chokepoint (cells) its crossings are searched for open
/// ground on either side.
const SEARCH_PAD: usize = 16;

/// Contacts this close (cells) belong to the same stretch of border: rims
/// are often broken up by specks of steeper ground.
const CONTACT_GAP: isize = 3;

/// Where two regions touch: neighbouring cells `a` < `b` of different
/// regions.
#[derive(Clone, Copy, Debug)]
struct Contact {
    a: usize,
    b: usize,
}

/// Turn the region contacts no gate accounts for into border chokepoints,
/// and measure every chokepoint's crossings.
pub(crate) fn link(map: &mut TierMap, ground: &Ground, soft: &[bool], walls: &[u32]) {
    let (w, h) = (map.width, map.height);
    let mut by_pair: BTreeMap<(usize, usize), Vec<Contact>> = BTreeMap::new();
    for a in 0..w * h {
        let Some(ra) = map.region_of_cell(a) else { continue };
        let (x, z) = (a % w, a / w);
        for b in [(x + 1 < w).then(|| a + 1), (z + 1 < h).then(|| a + w)].into_iter().flatten() {
            if let Some(rb) = map.region_of_cell(b)
                && rb != ra
            {
                by_pair.entry((ra.min(rb), ra.max(rb))).or_default().push(Contact { a, b });
            }
        }
    }

    // Unmeasured until a pair of its regions is.
    for c in &mut map.chokes {
        c.climb.iter_mut().for_each(|x| *x = f32::NAN);
    }
    let mut borders = Vec::new();
    let mut measured = Vec::new();
    for ((ra, rb), contacts) in by_pair {
        let pair = Pair::new(map, ground, soft, ra, rb, contacts);
        let gates: Vec<usize> = (map.chokes.iter())
            .filter(|c| c.kind == ChokeKind::Gate && c.side(ra).is_some() && c.side(rb).is_some())
            .map(|c| c.id)
            .collect();
        // Which gate each contact belongs to: its gate line's first, then
        // its sides, reached from the line through touching contacts.
        let mut owner: Vec<Option<usize>> = vec![None; pair.contacts.len()];
        for (i, ct) in pair.contacts.iter().enumerate() {
            owner[i] = [walls[ct.a], walls[ct.b]]
                .into_iter()
                .filter(|&g| g != NONE)
                .map(|g| g as usize)
                .find(|g| gates.contains(g));
        }
        for &g in &gates {
            let choke = &map.chokes[g];
            let near_line = |p: Pos| match choke.gate {
                Some([a, b]) => segment_distance(p, a, b),
                None => p.distance(choke.pos),
            };
            let mut line: Vec<usize> = (0..owner.len()).filter(|&i| owner[i] == Some(g)).collect();
            if line.is_empty() {
                // No gate line between them here: what touches at the gate.
                let close = (choke.width * 0.5).max(2.0 * CELL);
                line = (0..owner.len()).filter(|&i| owner[i].is_none() && near_line(pair.mid(i)) <= close).collect();
            }
            line.extend(pair.spread(&line, |i| owner[i].is_none() && near_line(pair.mid(i)) <= GATE_MARGIN));
            line.iter().for_each(|&i| owner[i] = Some(g));
            let sides = pair.spread(&line, |i| owner[i].is_none() && near_line(pair.mid(i)) <= FLANK_REACH);
            sides.iter().for_each(|&i| owner[i] = Some(g));
            if !line.is_empty() {
                measured.push(measure_gate(choke, &pair, &line, &sides));
            }
        }
        // The rest: border pieces.
        let mut free: Vec<bool> = owner.iter().map(Option::is_none).collect();
        for seed in 0..free.len() {
            if !free[seed] {
                continue;
            }
            let origin = pair.mid(seed);
            let piece = pair.spread(&[seed], |i| free[i] && pair.mid(i).distance(origin) <= BORDER_PIECE * 0.5);
            let piece: Vec<usize> = std::iter::once(seed).chain(piece).collect();
            piece.iter().for_each(|&i| free[i] = false);
            borders.push(pair.border(&piece));
        }
    }

    for m in measured {
        let c = &mut map.chokes[m.choke];
        c.wall = c.wall.max(m.wall);
        for (k, climb, bypass) in m.sides {
            c.climb[k] = if c.climb[k].is_nan() { climb } else { c.climb[k].min(climb) };
            c.bypass[k] = c.bypass[k].min(bypass);
        }
    }
    for c in &mut map.chokes {
        c.climb.iter_mut().filter(|x| x.is_nan()).for_each(|x| *x = 0.0);
    }
    for mut b in borders {
        b.id = map.chokes.len();
        for &r in &b.regions {
            map.regions[r].chokes.push(b.id);
        }
        map.chokes.push(b);
    }
}

/// The contacts between two regions, and the ground to measure them on.
struct Pair<'a> {
    map: &'a TierMap,
    ground: &'a Ground,
    soft: &'a [bool],
    regions: [usize; 2],
    contacts: Vec<Contact>,
    /// Contacts by the cells they touch.
    at_cell: HashMap<usize, Vec<usize>>,
}

/// The quickest crossing found: time and path length in cells, and the
/// contact crossed.
struct Crossing {
    time: f32,
    length: f32,
    contact: usize,
}

impl<'a> Pair<'a> {
    fn new(
        map: &'a TierMap,
        ground: &'a Ground,
        soft: &'a [bool],
        ra: usize,
        rb: usize,
        contacts: Vec<Contact>,
    ) -> Self {
        let mut at_cell: HashMap<usize, Vec<usize>> = HashMap::new();
        for (i, ct) in contacts.iter().enumerate() {
            at_cell.entry(ct.a).or_default().push(i);
            at_cell.entry(ct.b).or_default().push(i);
        }
        Pair { map, ground, soft, regions: [ra, rb], contacts, at_cell }
    }

    /// Middle of a contact.
    fn mid(&self, i: usize) -> Pos {
        let (a, b) = (self.map.cell_center(self.contacts[i].a), self.map.cell_center(self.contacts[i].b));
        Pos::new((a.x + b.x) * 0.5, (a.z + b.z) * 0.5)
    }

    /// Contacts near contact `i`: within [`CONTACT_GAP`] cells of one of
    /// its cells.
    fn touching(&self, i: usize) -> Vec<usize> {
        let (w, h) = (self.map.width as isize, self.map.height as isize);
        let ct = self.contacts[i];
        let mut near = Vec::new();
        for c in [ct.a, ct.b] {
            let (x, z) = ((c as isize) % w, (c as isize) / w);
            for nz in (z - CONTACT_GAP).max(0)..=(z + CONTACT_GAP).min(h - 1) {
                for nx in (x - CONTACT_GAP).max(0)..=(x + CONTACT_GAP).min(w - 1) {
                    near.extend(self.at_cell.get(&((nz * w + nx) as usize)).into_iter().flatten());
                }
            }
        }
        near
    }

    /// The contacts reached from `start` through touching contacts that
    /// pass `admit` (`start` itself excluded).
    fn spread(&self, start: &[usize], admit: impl Fn(usize) -> bool) -> Vec<usize> {
        let mut seen: HashSet<usize> = start.iter().copied().collect();
        let mut queue: Vec<usize> = start.to_vec();
        let mut reached = Vec::new();
        let mut k = 0;
        while k < queue.len() {
            let i = queue[k];
            k += 1;
            for n in self.touching(i) {
                if seen.insert(n) && admit(n) {
                    reached.push(n);
                    queue.push(n);
                }
            }
        }
        reached.sort_unstable();
        reached
    }

    /// The two contacts furthest apart, and their distance.
    fn extent(&self, set: &[usize]) -> (usize, usize, f32) {
        let mut best = (set[0], set[0], 0.0);
        for (k, &i) in set.iter().enumerate() {
            for &j in &set[k + 1..] {
                let d = self.mid(i).distance(self.mid(j));
                if d > best.2 {
                    best = (i, j, d);
                }
            }
        }
        best
    }

    /// The quickest way from open ground of region `from` (index into
    /// `regions`) to open ground of the other, crossing between them only
    /// at contacts `through`; searched within [`SEARCH_PAD`] cells of them.
    fn cross(&self, through: &[usize], from: usize) -> Option<Crossing> {
        let (w, h) = (self.map.width, self.map.height);
        let (rf, rt) = (self.regions[from], self.regions[1 - from]);
        let cells = through.iter().flat_map(|&i| [self.contacts[i].a, self.contacts[i].b]);
        let (mut x0, mut z0, mut x1, mut z1) = (usize::MAX, usize::MAX, 0, 0);
        for c in cells {
            (x0, z0, x1, z1) = (x0.min(c % w), z0.min(c / w), x1.max(c % w), z1.max(c / w));
        }
        let (x0, z0) = (x0.saturating_sub(SEARCH_PAD), z0.saturating_sub(SEARCH_PAD));
        let (x1, z1) = ((x1 + SEARCH_PAD).min(w - 1), (z1 + SEARCH_PAD).min(h - 1));
        let bw = x1 - x0 + 1;
        let global = |l: usize| (l / bw + z0) * w + (l % bw + x0);
        let n = bw * (z1 - z0 + 1);
        let region = |l: usize| self.map.region_of_cell(global(l));

        let gates: HashSet<(usize, usize)> =
            through.iter().map(|&i| (self.contacts[i].a, self.contacts[i].b)).collect();
        let open = |l: usize, r: usize| region(l) == Some(r) && !self.soft[global(l)];
        let any_open = |r: usize| (0..n).any(|l| open(l, r));
        let (open_from, open_to) = (any_open(rf), any_open(rt));
        let is_source = |l: usize| if open_from { open(l, rf) } else { region(l) == Some(rf) };
        let is_target = |l: usize| if open_to { open(l, rt) } else { region(l) == Some(rt) };

        let mut time = vec![f32::INFINITY; n];
        let mut length = vec![0.0f32; n];
        let mut crossed = vec![usize::MAX; n];
        let mut heap = BinaryHeap::new();
        for l in (0..n).filter(|&l| is_source(l)) {
            time[l] = 0.0;
            heap.push(HeapItem(0.0, l));
        }
        while let Some(HeapItem(t, l)) = heap.pop() {
            if t > time[l] {
                continue;
            }
            if is_target(l) {
                return Some(Crossing { time: t, length: length[l], contact: crossed[l] });
            }
            let (lx, lz) = (l % bw, l / bw);
            let here = region(l);
            let neighbours = [
                (lx > 0).then(|| l - 1),
                (lx + 1 < bw).then(|| l + 1),
                (lz > 0).then(|| l - bw),
                (l + bw < n).then(|| l + bw),
            ];
            for m in neighbours.into_iter().flatten() {
                let there = region(m);
                if there != Some(rf) && there != Some(rt) {
                    continue;
                }
                let mut via = crossed[l];
                if there != here {
                    let (a, b) = (global(l).min(global(m)), global(l).max(global(m)));
                    if !gates.contains(&(a, b)) {
                        continue;
                    }
                    via = self.at_cell[&a].iter().copied().find(|&i| self.contacts[i].b == b).unwrap_or(via);
                }
                let nt = t + self.ground.step_time(global(l), global(m));
                if nt < time[m] {
                    time[m] = nt;
                    length[m] = length[l] + 1.0;
                    crossed[m] = via;
                    heap.push(HeapItem(nt, m));
                }
            }
        }
        None
    }

    /// A border chokepoint over contacts `piece`.
    fn border(&self, piece: &[usize]) -> Choke {
        let (a, b, length) = self.extent(piece);
        let crossings = [self.cross(piece, 1), self.cross(piece, 0)];
        // climb[i]: into regions[i], so from the other one.
        let climb: Vec<f32> =
            crossings.iter().map(|c| c.as_ref().map_or(0.0, |c| (c.time - c.length) * CELL)).collect();
        let easiest = crossings
            .iter()
            .flatten()
            .min_by(|x, y| x.time.total_cmp(&y.time))
            .map_or(piece[0], |c| if c.contact == usize::MAX { piece[0] } else { c.contact });
        let width = length + CELL;
        Choke {
            id: 0,
            kind: ChokeKind::Border,
            pos: self.mid(easiest),
            width,
            gate: Some([self.mid(a), self.mid(b)]),
            regions: self.regions.to_vec(),
            climb,
            bypass: vec![f32::INFINITY; 2],
            wall: width,
        }
    }
}

/// A gate measured between two of its regions.
struct GateMeasure {
    choke: usize,
    /// Its wall: the gate line and its sides.
    wall: f32,
    /// (index into its regions, climb into it, bypass into it).
    sides: Vec<(usize, f32, f32)>,
}

/// Measure a gate between `pair`'s regions: crossing its line (contacts
/// `line`), going round over its sides (`sides`), and the wall closing both.
fn measure_gate(choke: &Choke, pair: &Pair, line: &[usize], sides: &[usize]) -> GateMeasure {
    let all: Vec<usize> = line.iter().chain(sides).copied().collect();
    let (_, _, length) = pair.extent(&all);
    let mut m = GateMeasure { choke: choke.id, wall: length + CELL, sides: Vec::new() };
    for into in 0..2 {
        let Some(k) = choke.side(pair.regions[into]) else { continue };
        let Some(through) = pair.cross(line, 1 - into) else { continue };
        let climb = (through.time - through.length) * CELL;
        let round = if sides.is_empty() { None } else { pair.cross(sides, 1 - into) };
        let bypass = round.map_or(f32::INFINITY, |r| ((r.time - through.time) * CELL).max(0.0));
        m.sides.push((k, climb, bypass));
    }
    m
}

/// Distance from `p` to the segment `a`-`b`.
fn segment_distance(p: Pos, a: Pos, b: Pos) -> f32 {
    let (dx, dz) = (b.x - a.x, b.z - a.z);
    let len2 = dx * dx + dz * dz;
    if len2 <= f32::EPSILON {
        return p.distance(a);
    }
    let t = (((p.x - a.x) * dx + (p.z - a.z) * dz) / len2).clamp(0.0, 1.0);
    p.distance(Pos::new(a.x + t * dx, a.z + t * dz))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::build::tier_map;
    use crate::strategy::ground_distance;
    use crate::{SpringMapData, Tier};

    /// A map of `sw` x `sh` slope cells, each with the slope and elevation
    /// `cell(x, z)` gives it.
    fn map_from(sw: u32, sh: u32, cell: impl Fn(u32, u32) -> (f32, f32)) -> SpringMapData {
        let mut slope_map = vec![0.0; (sw * sh) as usize];
        let mut height_map = vec![0.0; (sw * sh * 4) as usize];
        for z in 0..sh {
            for x in 0..sw {
                let (slope, y) = cell(x, z);
                slope_map[(z * sw + x) as usize] = slope;
                for (dx, dz) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                    height_map[((z * 2 + dz) * sw * 2 + x * 2 + dx) as usize] = y;
                }
            }
        }
        SpringMapData {
            map_width: sw * 2,
            map_height: sh * 2,
            slope_width: sw,
            slope_height: sh,
            slope_map,
            height_map,
        }
    }

    /// Rise over one cell straight up a slope.
    fn rise(slope: f32) -> f32 {
        let cos = 1.0 - slope;
        CELL * (1.0 - cos * cos).sqrt() / cos
    }

    fn at(x: f32, z: f32) -> Pos {
        Pos::new((x + 0.5) * CELL, (z + 0.5) * CELL)
    }

    #[test]
    fn a_slope_between_terraces_is_a_border_climbed_one_way() {
        // For bots: a low terrace (x < 28) and a high one (x >= 36), with
        // a band of slopes they crawl up (0.3) all along between them.
        let r = rise(0.3);
        let data = map_from(64, 32, |x, _| match x {
            0..28 => (0.0, 50.0),
            28..36 => (0.3, 50.0 + r * (x - 27) as f32),
            _ => (0.0, 50.0 + r * 8.0),
        });
        let t = tier_map(&data, Tier::Bot, Tier::Bot.zero_k_class());
        let (low, high) = (t.region_at(at(10.0, 16.0)).unwrap(), t.region_at(at(54.0, 16.0)).unwrap());
        assert_ne!(low, high);
        let between: Vec<&Choke> =
            t.chokes.iter().filter(|c| c.side(low).is_some() && c.side(high).is_some()).collect();
        assert!(!between.is_empty());
        for c in &between {
            assert_eq!(c.kind, ChokeKind::Border);
            // Eight cells up at 0.3 cost about three times as long as flat
            // ground; coming down is free.
            assert!((250.0..450.0).contains(&c.climb_into(high)), "{c:?}");
            assert!(c.climb_into(low) < 1.0, "{c:?}");
        }
        let (a, b) = (at(10.0, 16.0), at(54.0, 16.0));
        let (up, down) = (ground_distance(&t, a, b).unwrap(), ground_distance(&t, b, a).unwrap());
        assert!(up > down + 250.0, "up {up} down {down}");
    }

    #[test]
    fn a_gate_between_slopes_can_be_gone_round() {
        // A ridge across the map (x 44..52) with a flat corridor through it
        // (z 20..26). Bots crawl over a ridge of 0.3 slopes; one of 1.0 is a
        // cliff.
        let ridge = |slope: f32| {
            let r = if slope < 0.5 { rise(slope) } else { 0.0 };
            map_from(96, 48, move |x, z| match x {
                44..52 if !(20..26).contains(&z) => {
                    let up = if x < 48 { x - 43 } else { 52 - x };
                    (slope, 50.0 + r * up as f32)
                }
                _ => (0.0, 50.0),
            })
        };
        let (left, right) = (at(10.0, 24.0), at(80.0, 24.0));
        let gate_between = |t: &TierMap| -> Choke {
            let (l, r) = (t.region_at(left).unwrap(), t.region_at(right).unwrap());
            let gates =
                t.chokes.iter().filter(|c| c.kind == ChokeKind::Gate && c.side(l).is_some() && c.side(r).is_some());
            gates
                .min_by(|a, b| a.pos.distance(at(48.0, 23.0)).total_cmp(&b.pos.distance(at(48.0, 23.0))))
                .unwrap()
                .clone()
        };

        let cliffs = tier_map(&ridge(1.0), Tier::Bot, Tier::Bot.zero_k_class());
        let gate = gate_between(&cliffs);
        assert!(gate.bypass.iter().all(|b| b.is_infinite()), "{gate:?}");
        assert!(gate.wall < gate.width + 2.0 * GATE_MARGIN + 2.0 * CELL, "{gate:?}");
        assert!(gate.climb.iter().all(|&c| c < 1.0), "{gate:?}");

        let slopes = tier_map(&ridge(0.3), Tier::Bot, Tier::Bot.zero_k_class());
        let gate = gate_between(&slopes);
        assert!(gate.bypass.iter().all(|b| b.is_finite() && *b > 50.0), "{gate:?}");
        assert!(gate.wall > gate.width + 2.0 * FLANK_REACH - 4.0 * CELL, "{gate:?}");
        let (l, r) = (slopes.region_at(left).unwrap(), slopes.region_at(right).unwrap());
        assert!(gate.hold_width(l) > gate.width && gate.hold_width(r) > gate.width);
        // Further along the ridge the regions touch over it: borders.
        assert!(
            slopes.chokes.iter().any(|c| c.kind == ChokeKind::Border && c.side(l).is_some() && c.side(r).is_some())
        );
    }
}
