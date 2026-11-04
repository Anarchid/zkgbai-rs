//! Arrowheads (see [`Arrowhead`]): a force facing skirmishers of about its
//! leader's reach puts its `arrow` roles in a wedge behind the leader,
//! pointed at them, instead of their orbits (user, 2026-11-04).
//!
//! Every [`PERIOD`] frames each force whose doctrine has one reads the
//! enemies about its leader: the armed mobile ground units seen within
//! `watch` beyond their reach of it, and of those the skirmishers, whose
//! reach is within the doctrine's band of the leader's. Worth `min_value`
//! and `share` of what is about, they raise the wedge, or keep it `hold`
//! seconds more. It points at them as their metal weighs, by nearness,
//! their shield-piercing weapons `pierce` times, and turns at most
//! [`TURN`] a step. Members go to their places by raw moves, a little
//! ahead of where the leader goes, while within [`FAR`] of them (farther
//! ones come back by their keep orders); when the wedge drops they keep
//! as before.
//!
//! The geometry: a Felon tip, then rows of 2, 3, 4... close packed, a
//! row `step` × √3/2 behind the last; Aspis first, then Thugs, Outlaws,
//! Racketeers (the doctrine's role order). Every place lies behind the
//! line through the tip across the wedge, and within 30° of its axis, so
//! from anything within 60° of where it points the leader is the nearest
//! of the force.

use std::collections::BTreeMap;

use springai::{UnitDef, Vec3};

use super::Arrowhead;
use crate::Ai;
use crate::military::wrappers::squad::SquadId;
use crate::util::distance;

/// Frames between arrowhead steps.
pub const PERIOD: i32 = 6;
/// The most the wedge turns a step (radians): 90° a second.
pub const TURN: f32 = std::f32::consts::FRAC_PI_2 / (30 / PERIOD) as f32;
/// Members farther than this from their place are left to their keep
/// orders (the force's stragglers beyond 600 are brought back by path).
pub const FAR: f32 = 600.0;
/// Frames of the leader's motion members' goals are put ahead.
pub const LEAD: f32 = 20.0;
/// A member's goal is given again when it moved this far, or after
/// [`REGIVE`] frames (another order may have replaced it).
pub const REGOAL: f32 = 12.0;
pub const REGIVE: i32 = 30;
/// Enemies last seen this many frames ago or less count.
const SEEN: i32 = 90;

/// Place `k` of a wedge `step` apart: (depth behind its tip, offset to the
/// side). Rows of 2, 3, 4... places, close packed, the middle of a row
/// first, then its left before its right.
pub fn slot(k: usize, step: f32) -> (f32, f32) {
    let (mut row, mut first) = (1usize, 0usize);
    while k > first + row {
        first += row + 1;
        row += 1;
    }
    let mut sides: Vec<f32> = (0..=row).map(|j| (j as f32 - row as f32 / 2.0) * step).collect();
    sides.sort_by(|a, b| a.abs().total_cmp(&b.abs()).then(a.total_cmp(b)));
    (row as f32 * step * 0.75_f32.sqrt(), sides[k - first])
}

/// Places for a wedge's members (id, rank, where it stands in the wedge's
/// frame, see [`local`]): ranks in order take the places nearest the tip,
/// and within a rank's block the nearest member and place pair first.
pub fn assign(members: &[(i32, usize, (f32, f32))], step: f32) -> BTreeMap<i32, (f32, f32)> {
    let mut sorted: Vec<&(i32, usize, (f32, f32))> = members.iter().collect();
    sorted.sort_by_key(|m| (m.1, m.0));
    let mut out = BTreeMap::new();
    let mut k = 0;
    for chunk in sorted.chunk_by(|a, b| a.1 == b.1) {
        let mut left: Vec<&(i32, usize, (f32, f32))> = chunk.to_vec();
        let mut free: Vec<(f32, f32)> = (k..k + chunk.len()).map(|j| slot(j, step)).collect();
        k += chunk.len();
        while !left.is_empty() {
            let gap = |m: &(i32, usize, (f32, f32)), p: (f32, f32)| (m.2.0 - p.0).hypot(m.2.1 - p.1);
            let (mi, pi) = (0..left.len())
                .flat_map(|mi| (0..free.len()).map(move |pi| (mi, pi)))
                .min_by(|&(a, b), &(c, d)| gap(left[a], free[b]).total_cmp(&gap(left[c], free[d])))
                .expect("as many places as members");
            out.insert(left[mi].0, free[pi]);
            left.swap_remove(mi);
            free.swap_remove(pi);
        }
    }
    out
}

/// Where a place (depth, side) lies, the tip at `lead` and the wedge
/// pointing along `axis` (a unit vector on the map's x, z).
pub fn world(lead: Vec3, axis: (f32, f32), place: (f32, f32)) -> Vec3 {
    let ((ux, uz), (depth, side)) = (axis, place);
    Vec3::xz(lead.x - ux * depth - uz * side, lead.z - uz * depth + ux * side)
}

/// Where `pos` lies in the wedge's frame: (depth behind the tip, side).
pub fn local(lead: Vec3, axis: (f32, f32), pos: Vec3) -> (f32, f32) {
    let ((dx, dz), (ux, uz)) = ((pos.x - lead.x, pos.z - lead.z), axis);
    (-(dx * ux + dz * uz), dz * ux - dx * uz)
}

/// `axis` turned toward `want` by at most `max` radians.
pub fn turn(axis: (f32, f32), want: (f32, f32), max: f32) -> (f32, f32) {
    use std::f32::consts::{PI, TAU};
    let a = axis.1.atan2(axis.0);
    let d = (want.1.atan2(want.0) - a + PI).rem_euclid(TAU) - PI;
    let b = a + d.clamp(-max, max);
    (b.cos(), b.sin())
}

/// An armed enemy ground unit, for an arrowhead's read.
#[derive(Clone, Debug, PartialEq)]
pub struct Foe {
    pub pos: Vec3,
    pub value: f32,
    pub reach: f32,
    /// It has a weapon of about its full reach that shields do not stop.
    pub pierce: bool,
    pub name: String,
}

/// What a leader's arrowhead read sees.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Read {
    /// Metal of the skirmishers about, and of those that pierce shields.
    pub band: f32,
    pub pierce: f32,
    /// Metal of every armed foe about.
    pub about: f32,
    /// Where they are, weighed (none: nowhere in particular).
    pub toward: Option<(f32, f32)>,
    /// The skirmishers by unit type.
    pub names: BTreeMap<String, u32>,
}

impl Read {
    pub fn raises(&self, a: &Arrowhead) -> bool {
        self.band > 0.0
            && self.band >= a.min_value
            && self.band >= a.share * self.about
            && (a.max_pierce <= 0.0 || self.pierce <= a.max_pierce)
    }

    /// "2 vehcapture, 1 cloakskirm".
    pub fn against(&self) -> String {
        let names: Vec<String> = self.names.iter().map(|(n, k)| format!("{k} {n}")).collect();
        names.join(", ")
    }
}

/// The read for a leader at `lead` whose reach is `reach`.
pub fn read<'a>(lead: Vec3, reach: f32, a: &Arrowhead, foes: impl IntoIterator<Item = &'a Foe>) -> Read {
    let mut r = Read::default();
    let (mut sx, mut sz) = (0.0f32, 0.0f32);
    for f in foes {
        let d = distance(lead, f.pos);
        if d > f.reach + a.watch {
            continue;
        }
        r.about += f.value;
        if f.reach < reach - a.below || f.reach > reach + a.above {
            continue;
        }
        r.band += f.value;
        if f.pierce {
            r.pierce += f.value;
        }
        *r.names.entry(f.name.clone()).or_default() += 1;
        if d > 1.0 {
            let w = f.value * if f.pierce { a.pierce } else { 1.0 } / d.max(100.0);
            sx += w * (f.pos.x - lead.x) / d;
            sz += w * (f.pos.z - lead.z) / d;
        }
    }
    let len = sx.hypot(sz);
    r.toward = (len > 1e-6).then(|| (sx / len, sz / len));
    r
}

/// Whether a unit type has a weapon of about its full reach that shields do
/// not stop (the Dominatrix's capture ray, a support commander's gauss).
pub fn pierces(ud: UnitDef) -> bool {
    let reach = ud.max_weapon_range();
    (ud.weapon_mounts().iter().filter_map(|m| m.weapon_def()))
        .any(|w| !w.is_shield() && w.intercepted_by_shield_type() == 0 && w.range() >= 0.8 * reach)
}

impl Ai {
    /// Every [`PERIOD`] frames: raise, turn, keep or drop each force's
    /// arrowhead, and send its members to their places.
    pub(crate) fn taskforce_arrowheads(&mut self, frame: i32) {
        let off = self.dev_off("arrowhead");
        let sids: Vec<SquadId> = (self.squads.ids().into_iter())
            .filter(|&sid| {
                self.squads
                    .get(sid)
                    .is_some_and(|s| s.is_taskforce() && (s.shape.arrowhead.is_some() || s.arrow.is_some()))
            })
            .collect();
        if sids.is_empty() {
            return;
        }
        let foes = if off { Vec::new() } else { self.arrow_foes(frame) };
        for sid in sids {
            self.taskforce_arrowhead(sid, &foes, off, frame);
        }
    }

    /// The armed mobile ground enemies seen lately.
    fn arrow_foes(&mut self, frame: i32) -> Vec<Foe> {
        let seen: Vec<(Vec3, f32, f32, UnitDef)> = (self.war.targets.values())
            .filter(|e| e.identified && !e.is_dead && !e.is_static && !e.can_fly() && e.can_attack())
            .filter(|e| e.visible || frame - e.last_seen <= SEEN)
            .filter(|e| e.def_max_weapon_range() > 0.0)
            .filter_map(|e| e.ud.map(|ud| (e.position, e.value, e.def_max_weapon_range(), ud)))
            .collect();
        let cache = &mut self.squad_handler.arrow_pierce;
        (seen.into_iter())
            .map(|(pos, value, reach, ud)| Foe {
                pos,
                value,
                reach,
                pierce: *cache.entry(ud.id()).or_insert_with(|| pierces(ud)),
                name: self.defs.name(ud).to_owned(),
            })
            .collect()
    }

    fn taskforce_arrowhead(&mut self, sid: SquadId, foes: &[Foe], off: bool, frame: i32) {
        let Some(s) = self.squads.get(sid) else { return };
        let doctrine = s.shape.doctrine.clone();
        let lead = s.get_leader(&self.fighters).filter(|l| l.health() > 0.0).map(|l| (l, l.pos()));
        let (Some(a), Some((leader, lpos)), false) = (s.shape.arrowhead.clone(), lead, off) else {
            if s.arrow.is_some() {
                let held = s.arrow_held(frame);
                if let Some(s) = self.squads.get_mut(sid) {
                    s.arrow_release(&self.fighters);
                }
                self.log(&format!(
                    "taskforce: the {doctrine} force (squad {sid}) drops its arrowhead after {held:.0} s"
                ));
            }
            return;
        };
        let reach = leader.def().map_or(0.0, |d| d.max_weapon_range());
        let r = read(lpos, reach, &a, foes);
        let raises = r.raises(&a);
        let vel = leader.vel();
        let Some(s) = self.squads.get_mut(sid) else { return };
        match (&mut s.arrow, raises) {
            (None, false) => return,
            (None, true) => {
                // Pointing at them, else where the force goes.
                let go = (s.target.x - lpos.x, s.target.z - lpos.z);
                let len = go.0.hypot(go.1);
                let axis = r.toward.unwrap_or(if len > 1.0 { (go.0 / len, go.1 / len) } else { (1.0, 0.0) });
                s.arrow_raise(axis, frame);
                let members = s.arrow_count(&self.fighters);
                self.log(&format!(
                    "taskforce: the {doctrine} force (squad {sid}) forms an arrowhead ({members} members) against {} \
                     ({:.0} metal, {:.0} of it piercing shields; {:.0} armed about; reach {reach:.0}), pointing ({:.2}, {:.2})",
                    r.against(),
                    r.band,
                    r.pierce,
                    r.about,
                    axis.0,
                    axis.1
                ));
            }
            (Some(arrow), true) => {
                arrow.last = frame;
                if let Some(t) = r.toward {
                    arrow.axis = turn(arrow.axis, t, TURN);
                }
            }
            (Some(arrow), false) => {
                if (frame - arrow.last) as f32 > a.hold * 30.0 {
                    let held = s.arrow_held(frame);
                    s.arrow_release(&self.fighters);
                    self.log(&format!(
                        "taskforce: the {doctrine} force (squad {sid}) drops its arrowhead after {held:.0} s (no skirmishers for {:.0} s)",
                        a.hold
                    ));
                    return;
                }
            }
        }
        let Some(s) = self.squads.get_mut(sid) else { return };
        s.arrow_drive(&self.fighters, lpos, vel, a.step, frame);
    }

    /// Whether member `id` stands in its force's arrowhead (it may stand
    /// still there on purpose).
    pub(crate) fn squad_in_arrowhead(&self, id: i32) -> bool {
        let sid = self.fighters.get(&id).and_then(|f| f.squad);
        sid.and_then(|sid| self.squads.get(sid)).is_some_and(|s| s.arrow_places(id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: (f32, f32), b: (f32, f32)) -> bool {
        (a.0 - b.0).abs() < 1e-3 && (a.1 - b.1).abs() < 1e-3
    }

    #[test]
    fn wedge_rows_fill_middle_out_behind_the_tip() {
        let h = 36.0 * 0.75_f32.sqrt();
        assert!(close(slot(0, 36.0), (h, -18.0)));
        assert!(close(slot(1, 36.0), (h, 18.0)));
        assert!(close(slot(2, 36.0), (2.0 * h, 0.0)));
        assert!(close(slot(3, 36.0), (2.0 * h, -36.0)));
        assert!(close(slot(4, 36.0), (2.0 * h, 36.0)));
        assert!(close(slot(5, 36.0), (3.0 * h, -18.0)));
        assert!(close(slot(9, 36.0), (4.0 * h, 0.0)));
        // Every place is behind the tip, within 30° of the axis: so from
        // anything within 60° of where it points, the tip is nearest.
        for k in 0..30 {
            let (depth, side) = slot(k, 36.0);
            assert!(depth > 0.0 && side.abs() <= depth * (30f32.to_radians().tan()) + 1e-3, "{k}");
            let lead = Vec3::xz(0.0, 0.0);
            for deg in [-60.0f32, -30.0, 0.0, 30.0, 60.0] {
                let axis = (deg.to_radians().cos(), deg.to_radians().sin());
                let foe = Vec3::xz(450.0, 0.0);
                let p = world(lead, axis, (depth, side));
                assert!(distance(p, foe) >= distance(lead, foe) - 1e-2, "{k} at {deg}");
            }
        }
    }

    #[test]
    fn world_and_local_are_inverse() {
        let lead = Vec3::xz(1000.0, 2000.0);
        let axis = (0.6, -0.8);
        for place in [(30.0, 0.0), (60.0, -36.0), (90.0, 18.0)] {
            assert!(close(local(lead, axis, world(lead, axis, place)), place));
        }
        // Pointing east, depth is west of the tip.
        let p = world(lead, (1.0, 0.0), (50.0, 0.0));
        assert!((p.x - 950.0).abs() < 1e-3 && (p.z - 2000.0).abs() < 1e-3);
    }

    #[test]
    fn ranks_take_the_places_nearest_the_tip_in_order() {
        // An Aspis (rank 0), two Thugs (rank 1) and a Racketeer (rank 3),
        // the Racketeer standing nearest the tip: the Aspis takes place 0,
        // the Thugs places 1 and 2 (the one standing nearer place 1 takes
        // it), the Racketeer place 3 all the same.
        let members = [(10, 3, (5.0, 0.0)), (11, 1, (30.0, 40.0)), (12, 0, (30.0, 30.0)), (13, 1, (200.0, 0.0))];
        let p = assign(&members, 36.0);
        let h = 36.0 * 0.75_f32.sqrt();
        assert!(close(p[&12], (h, -18.0)));
        assert!(close(p[&11], (h, 18.0)));
        assert!(close(p[&13], (2.0 * h, 0.0)));
        assert!(close(p[&10], (2.0 * h, -36.0)));
    }

    #[test]
    fn turning_is_limited_and_takes_the_short_way() {
        let a = turn((1.0, 0.0), (0.0, 1.0), 0.1);
        assert!(close(a, (0.1f32.cos(), 0.1f32.sin())));
        let b = turn((1.0, 0.0), (-1.0, -0.01), 0.1);
        assert!(b.1 < 0.0, "the short way round is clockwise");
        assert!(close(turn((1.0, 0.0), (0.0, 1.0), 4.0), (0.0, 1.0)));
    }

    fn foe(x: f32, z: f32, value: f32, reach: f32, pierce: bool, name: &str) -> Foe {
        Foe { pos: Vec3::xz(x, z), value, reach, pierce, name: name.into() }
    }

    #[test]
    fn dominatrices_raise_it_rogues_and_raiders_do_not() {
        let a = Arrowhead::default();
        let lead = Vec3::xz(0.0, 0.0);
        // A Dominatrix 700 away (reach 450, watch 400): about, in the band.
        let dom = [foe(700.0, 0.0, 420.0, 450.0, true, "vehcapture")];
        let r = read(lead, 435.0, &a, &dom);
        assert!(r.raises(&a) && r.pierce == 420.0 && close(r.toward.unwrap(), (1.0, 0.0)));
        assert_eq!(r.against(), "1 vehcapture");
        // Out of watch: not about.
        assert!(!read(lead, 435.0, &a, &[foe(900.0, 0.0, 420.0, 450.0, true, "vehcapture")]).raises(&a));
        // Rogues outrange the Felon by far: not skirmishers of its reach.
        let rogues = [foe(600.0, 0.0, 600.0, 530.0, false, "shieldskirm")];
        assert!(!read(lead, 435.0, &a, &rogues).raises(&a));
        // A lone cheap Ronin is worth too little; a Dominatrix in a crowd of
        // brawlers is under the share.
        assert!(!read(lead, 435.0, &a, &[foe(500.0, 0.0, 90.0, 455.0, false, "cloakskirm")]).raises(&a));
        let crowd = [
            foe(500.0, 0.0, 420.0, 450.0, true, "vehcapture"),
            foe(300.0, 0.0, 1200.0, 280.0, false, "shieldassault"),
            foe(300.0, 50.0, 1200.0, 280.0, false, "shieldassault"),
        ];
        assert!(!read(lead, 435.0, &a, &crowd).raises(&a));
        // Capped, three Dominatrices are too many for a wedge, two are not.
        let capped = Arrowhead { max_pierce: 900.0, ..Arrowhead::default() };
        let doms = |n: usize| {
            (0..n).map(|i| foe(600.0, 90.0 * i as f32, 420.0, 450.0, true, "vehcapture")).collect::<Vec<_>>()
        };
        assert!(read(lead, 435.0, &capped, &doms(2)).raises(&capped));
        assert!(!read(lead, 435.0, &capped, &doms(3)).raises(&capped));
        assert!(read(lead, 435.0, &a, &doms(3)).raises(&a));
    }

    #[test]
    fn piercing_weapons_pull_the_point() {
        let a = Arrowhead::default();
        // A Dominatrix north, two Ronins east, all at 600: the Dominatrix
        // weighs 420 x 3 against 180.
        let foes = [
            foe(0.0, 600.0, 420.0, 450.0, true, "vehcapture"),
            foe(600.0, 0.0, 90.0, 455.0, false, "cloakskirm"),
            foe(600.0, 10.0, 90.0, 455.0, false, "cloakskirm"),
        ];
        let t = read(Vec3::xz(0.0, 0.0), 435.0, &a, &foes).toward.unwrap();
        assert!(t.1 > 0.95 && t.0 > 0.0, "{t:?}");
    }
}
