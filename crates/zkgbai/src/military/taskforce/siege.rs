//! Siege: the conduct of doctrines whose aim is `siege` (Tremors,
//! Emissaries: see the `tremor` and `emissary` seeds).
//!
//! Both guns must stand to shoot. An Emissary (`tankarty`: 700 metal, 1150
//! range, a 600 shell every 7 s) turns its gun out to aim, which drops it to
//! 5% speed until it stows again after each shot; a Tremor
//! (`tankheavyarty`: 1600 metal, 1160 range, three 145 shells a second)
//! opens its bays, turret and barrels to fire, several seconds each way, and
//! closes them whenever it is moved, even a nudge of 24 elmos. Neither aims
//! while moving. So a siege force is formed, recruited and brought up by the
//! taskforce wrapper like any other (see `handlers::squad`), and once it
//! engages this module gives it a *station*: a place within the guns' reach
//! of the most enemy statics, beyond the reach of every enemy static we know
//! of (the standoff), where the force parks. The leader walks to the
//! station and stands; the other guns are each given a spot about it and
//! stand too; the escorts keep orbiting the leader, which puts them between
//! the guns and whatever comes. Parked, each gun is set (ZK's Set Target) on
//! the most valuable enemy in its reach whose place we know, and fires at
//! will at the rest.
//!
//! The force leaves its station when the wrapper calls it off (hurt, low
//! shields, threatened, worth too little), or when nothing is in reach any
//! more and a better station exists; leaving, the guns go back to orbiting
//! the leader. With no enemy static known, the station is where artillery
//! would otherwise be sent, pulled back to the guns' reach.
//!
//! Log lines (for the A/B scorers): `stations at` when a force picks a
//! station, `deploys` per gun when it parks (how far off the nearest known
//! enemy static and that static's range: the standoff), `undeploys (why)`
//! per gun when it leaves, `kills` as marksmen log them (see `marksman`), a
//! per-force report now and then, and a note on the member death line of
//! how the gun stood (deployed in range, deployed idle, moving, forming,
//! rallying) and for how long it stood each way.

use std::collections::BTreeMap;

use springai::{NO_TIMEOUT, Unit, Vec3};

use serde_json::json;

use crate::Ai;
use crate::defs::cmd;
use crate::economy::terraform::is_terraforming;
use crate::kgbutil::kgb_move_to_exact;
use crate::kgbutil::line_of_fire::{Flight, Gun as Arm, ZK_CANNON_GRAVITY};
use crate::military::taskforce::doctrine::Perch;
use crate::military::taskforce::{Aim, doctrine::ORBIT, marksman::Mark};
use crate::military::{Enemy, SquadId};
use crate::terrain::reaches;
use crate::util::{NULLPOS, distance, get_directional_point};

/// The engine's attack order; with a position alone it is attack-ground.
const CMD_ATTACK: i32 = 20;

/// A gun is at its spot within this.
pub const AT_STATION: f32 = 60.0;

/// The force is at its station once the leader is within this (the wrapper
/// sends the leader to a point 135 short of its target, see
/// `Squad::taskforce_set_target`), and parked it leaves only when the
/// leader is farther than `PARK_LEAVE` (a nudge is not a move).
const PARK: f32 = 200.0;
const PARK_LEAVE: f32 = 320.0;

/// The way to a station is looked at every this many elmos: none of it may
/// be under a known defense (a gun cannot walk through a fort to stand
/// behind it).
const WAY_STEP: f32 = 100.0;

/// A station lies this far beyond the reach of every enemy static we know
/// of (the Analyst's gate asks for 100).
const STANDOFF: f32 = 120.0;

/// Of the guns' range, so they reach without inching forward.
const RANGE_MARGIN: f32 = 60.0;

/// Stations are looked for about the statics worth most, this many, at
/// these shares of the guns' reach in this many directions (the leader
/// stops up to 135 short of its station, see `Squad::taskforce_set_target`,
/// so the rings leave that much reach to spare).
const STATION_TARGETS: usize = 16;
const STATION_SHARES: [f32; 2] = [0.80, 0.68];
const STATION_DIRS: usize = 16;

/// The guns spread this far about the station, each at its own spot.
const SPREAD: f32 = 70.0;

/// A gun not yet at its spot is told again every so often.
const RESEND: i32 = 30 * 8;

/// A force says how it stood every so often.
const REPORT: i32 = 30 * 60;

/// A perched force (see [`Perch`]) stands off the statics by this much
/// beyond their reach (the Crab's body is 60 across, and a Stinger hit a
/// Crab 632 off at its nominal 620), and looks for its site on tighter
/// rings about the targets. The reach stood off is the greater of the
/// static's reach to the raised aim point and its reach on the ground: the
/// core stands on the ground for the minute of the raise (crab1/g2 lost a
/// Crab so, before its pillar was up).
const PERCH_STANDOFF: f32 = 80.0;
const PERCH_SHARES: [f32; 2] = [0.97, 0.90];
/// Aim points stand about this far above the ground (mid + aimposoffset).
const AIM_ABOVE: f32 = 40.0;
/// A perch site's top square must be this flat (its corners' spread).
const PERCH_FLAT: f32 = 30.0;
/// The leading core stands on the site within this before the raise.
const ON_SITE: f32 = 24.0;
/// The raise counts as up this close to the height asked.
const UP_SLACK: f32 = 8.0;
/// A perched gun's own sight (the Crab's 660), beyond which known statics
/// are shelled as ground.
const SIGHT: f32 = 640.0;

/// Worth beyond metal of a defense, and of a mex or an energy building.
const PORC: f32 = 1.5;
const ECONOMY: f32 = 2.0;
/// A mobile in sight is worth this share of its cost as a gun's target.
const MOBILE: f32 = 0.7;

/// Where a force stands to fire, and whether it is parked there (its
/// leader at the station while engaging).
struct Station {
    pos: Vec3,
    /// Enemy statics in reach of it when picked, and their worth.
    reach: (usize, f32),
    deployed: bool,
    /// A perch site's natural height over the nearest defense (0 unless
    /// the perch's `natural` dial is on): the raise asks the remainder.
    natural: f32,
}

/// A perch going up under the leading core: the site, the ground there
/// before, when the raise began, the constructors borrowed, and whether
/// it is up or given up.
struct PerchState {
    site: Vec3,
    ground: f32,
    since: i32,
    raisers: Vec<Unit>,
    up: bool,
    abandoned: bool,
    /// Attack-ground target (enemy id) the core was last set on.
    ground_target: Option<i32>,
    /// Frames of the raise with an enemy at the foot (within `FOOT` of the
    /// site), the last conduct frame that counted, and when the raisers
    /// went back (none while they work).
    foot_frames: i32,
    foot_last: i32,
    returned: Option<i32>,
    /// Why the perched core would have left, said once (it cannot come down).
    stranded: Option<String>,
    /// How much the raise asked (the perch height less the natural one).
    raise: f32,
    /// Raisers sent so far (the first and each re-borrow).
    tries: u32,
    /// The top square and the height it is levelled to, for a re-borrow.
    square: [Vec3; 4],
    level: f32,
}

/// Raisers called away are replaced this many times within the borrow.
const RAISE_TRIES: u32 = 3;

/// An enemy this close to the site slows the raise 20x (the gadget's
/// nearby-enemy check: half the top, 100, and 36 more).
const FOOT: f32 = 170.0;

#[derive(Default)]
struct SiegeForce {
    station: Option<Station>,
    /// Guns given spots about the station: when each was last told to go.
    sent: BTreeMap<i32, i32>,
    last_report: i32,
    /// The leading core told to stand exactly on its perch site: when.
    on_site_sent: i32,
    perch: Option<PerchState>,
    /// When a perch site was last refused for want of one (said once a
    /// minute, not every pass).
    refused_at: i32,
    /// The natural height of the site the last pick chose (see
    /// [`Perch::natural`]); taken into the station as it is set.
    pending_natural: f32,
}

/// What a gun was doing at the last conduct, since when (the frame of the
/// last conduct), and how many frames it has spent each way.
#[derive(Default)]
struct Gun {
    state: &'static str,
    last: i32,
    parked: bool,
    /// Told to hold position at its spot (the leader is on maneuver
    /// otherwise, see `Squad::taskforce_add_unit`).
    held: bool,
    frames: BTreeMap<&'static str, i32>,
}

pub struct Siege {
    forces: BTreeMap<SquadId, SiegeForce>,
    guns: BTreeMap<i32, Gun>,
    /// Pillars raised (or begun): the site and the ground there before, so
    /// a raise taken up again at the same place does not stack another
    /// height on the first.
    pillars: Vec<(Vec3, f32)>,
    /// Where perch sites come from (see [`PerchSites`]).
    pub sites: Box<dyn PerchSites>,
}

impl Default for Siege {
    fn default() -> Self {
        Siege { forces: BTreeMap::new(), guns: BTreeMap::new(), pillars: Vec::new(), sites: Box::new(RingSites) }
    }
}

/// A source of candidate perch sites for a force at `from` whose guns reach
/// `reach` from the perch, against the known statics: the conduct scores
/// and checks whatever it is given (standoff, flatness, reach, the way
/// there). The first provider lays rings about the statics worth most
/// ([`RingSites`]); a terrain catalog of natural perches (peaks, cliff
/// tops over the lanes) may replace it without touching the conduct.
pub trait PerchSites: Send + Sync {
    fn sites(&self, from: Vec3, reach: f32, statics: &[Static], standoff: f32, map_size: (f32, f32)) -> Vec<Vec3>;
}

/// Rings about the statics worth most, at the perch's shares of the reach.
pub struct RingSites;

impl PerchSites for RingSites {
    fn sites(&self, from: Vec3, reach: f32, statics: &[Static], standoff: f32, (w, h): (f32, f32)) -> Vec<Vec3> {
        let on_map = |x: f32, z: f32| x >= 50.0 && z >= 50.0 && x <= w - 50.0 && z <= h - 50.0;
        candidates_on(from, reach, statics, standoff, &PERCH_SHARES, on_map).into_iter().map(|c| c.0).collect()
    }
}

/// A pillar begun within this of a site is the same pillar.
const SAME_PILLAR: f32 = 40.0;

/// A known enemy static: where, how far its ground-firing weapons reach
/// (`range`: the greater of the nominal range and the reach to a target at
/// the aim height asked; 0 for one unarmed, or anti-air only: neither
/// fences the ground), that reach to the raised aim point (`raised`), the
/// nominal range, its worth to the guns, and whether it is in sight.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Static {
    pub pos: Vec3,
    pub range: f32,
    pub raised: f32,
    pub nominal: f32,
    pub worth: f32,
    pub visible: bool,
}

/// Whether `spot` stands off every armed static by its range and `standoff`.
pub fn stands_off(spot: Vec3, statics: &[Static], standoff: f32) -> bool {
    statics.iter().filter(|s| s.range > 0.0).all(|s| distance(spot, s.pos) >= s.range + standoff)
}

/// The nearest armed static to `p` (one that fires at the ground): how far,
/// and which.
pub fn nearest(p: Vec3, statics: &[Static]) -> Option<(f32, Static)> {
    (statics.iter().filter(|s| s.range > 0.0)).map(|&s| (distance(p, s.pos), s)).min_by(|a, b| a.0.total_cmp(&b.0))
}

/// What statics a gun at `spot` with `reach` has: how many, their worth.
pub fn in_reach(spot: Vec3, reach: f32, statics: &[Static]) -> (usize, f32) {
    statics.iter().filter(|s| distance(spot, s.pos) <= reach).fold((0, 0.0), |(k, v), s| (k + 1, v + s.worth))
}

/// Candidate stations for guns of `reach` at `from`: where they stand, and
/// rings about the statics worth most, scored by the worth in reach less a
/// little for the way there; those that stand off every static, best first.
pub fn candidates(
    from: Vec3,
    reach: f32,
    statics: &[Static],
    standoff: f32,
    on_map: impl Fn(f32, f32) -> bool,
) -> Vec<(Vec3, f32)> {
    candidates_on(from, reach, statics, standoff, &STATION_SHARES, on_map)
}

/// [`candidates`] on rings at the given shares of the reach.
pub fn candidates_on(
    from: Vec3,
    reach: f32,
    statics: &[Static],
    standoff: f32,
    shares: &[f32],
    on_map: impl Fn(f32, f32) -> bool,
) -> Vec<(Vec3, f32)> {
    let mut by_worth: Vec<&Static> = statics.iter().collect();
    by_worth.sort_by(|a, b| b.worth.total_cmp(&a.worth));
    let mut spots: Vec<(f32, f32)> = vec![(from.x, from.z)];
    for s in by_worth.iter().take(STATION_TARGETS) {
        for &share in shares {
            for d in 0..STATION_DIRS {
                let a = d as f32 * std::f32::consts::TAU / STATION_DIRS as f32;
                spots.push((s.pos.x + share * reach * a.cos(), s.pos.z + share * reach * a.sin()));
            }
        }
    }
    let mut places: Vec<(Vec3, f32)> = Vec::new();
    for (x, z) in spots {
        if !on_map(x, z) {
            continue;
        }
        let p = Vec3::xz(x, z);
        if !stands_off(p, statics, standoff) {
            continue;
        }
        let (k, worth) = in_reach(p, reach, statics);
        if k > 0 {
            places.push((p, worth / (1.0 + distance(from, p) / 1000.0)));
        }
    }
    places.sort_by(|a, b| b.1.total_cmp(&a.1));
    places
}

/// A perched gun fires out to the reach it was given (the margin is the
/// station's business).
fn range_of(reach: f32) -> f32 {
    reach
}

/// What an enemy is worth to the guns.
fn worth(e: &Enemy, cost: f32) -> f32 {
    let kind = if e.is_mex || e.def_name.starts_with("energy") {
        ECONOMY
    } else if e.is_porc {
        PORC
    } else if e.is_static {
        1.0
    } else {
        MOBILE
    };
    cost * kind
}

/// The ground-firing weapons of a unit type as guns at the origin (for
/// their reach by height; see `Gun::reach_2d`): range, heightMod and how
/// the shot flies (a cannon's arc under ZK's cannon gravity).
pub fn arms_of(def: springai::UnitDef) -> Vec<Arm> {
    (def.weapon_mounts().into_iter().filter_map(|m| m.weapon_def()))
        .filter(|w| !w.is_shield() && w.range() > 0.0 && !w.name().contains("fake"))
        .filter(|w| w.is_able_to_attack_ground())
        .map(|w| {
            let flight = if w.weapon_type() == "Cannon" && w.high_trajectory() != 1 {
                let speed = w.projectile_speed();
                Flight::Arc { speed: if speed < 100.0 { speed * 30.0 } else { speed }, gravity: ZK_CANNON_GRAVITY }
            } else {
                Flight::Line
            };
            Arm {
                x: 0.0,
                z: 0.0,
                muzzle: 0.0,
                range: w.range(),
                height_mod: w.height_mod(),
                aoe: w.area_of_effect(),
                body: 0.0,
                flight,
                avoids_friends: false,
                through_ground: false,
            }
        })
        .collect()
}

/// How far guns reach a target standing `dh` above their muzzles: the
/// farthest of them.
pub fn reach_of(arms: &[Arm], dh: f32) -> f32 {
    arms.iter().map(|g| g.reach_2d(dh)).fold(0.0, f32::max)
}

impl Ai {
    /// The enemy statics we know of (identified, and where they were seen:
    /// a building's radar ghost stands where it is), with their reach to a
    /// target `up` elmos above their own ground.
    fn siege_statics(&self, up: f32) -> Vec<Static> {
        (self.war.targets.values())
            .filter(|e| e.identified && e.is_static && !e.is_dead && e.position != NULLPOS)
            .map(|e| {
                let armed = e.can_attack() && !e.is_aa;
                let nominal = if armed { e.def_max_weapon_range() } else { 0.0 };
                let raised = match (armed, up > 0.0, e.ud) {
                    (true, true, Some(d)) => reach_of(&arms_of(d), up),
                    _ => nominal,
                };
                Static {
                    pos: e.position,
                    range: nominal.max(raised),
                    raised,
                    nominal,
                    worth: worth(e, e.ud.map_or(e.value, |d| d.cost(self.metal))),
                    visible: e.visible,
                }
            })
            .collect()
    }

    /// The guns' reach from `up` elmos above the ground, less a margin: the
    /// leader's weapons by their flight (a cannon reaches farther from a
    /// perch).
    fn siege_reach(leader: Unit, up: f32) -> f32 {
        let Some(def) = leader.def() else { return 1150.0 - RANGE_MARGIN };
        let arms = arms_of(def);
        let reach = if arms.is_empty() { def.max_weapon_range() } else { reach_of(&arms, -up) };
        reach - RANGE_MARGIN
    }

    /// Whether force `sid` has its perch going up or up (it then stays
    /// where it is: a perched core cannot come down).
    pub(crate) fn siege_perched(&self, sid: SquadId) -> bool {
        (self.taskforces.siege.forces.get(&sid).and_then(|f| f.perch.as_ref())).is_some_and(|p| !p.abandoned)
    }

    /// How high the perch of force `sid` raises its leading core's aim
    /// point (0 without a perch, or one given up).
    fn siege_up(&self, sid: SquadId) -> f32 {
        let perch = self.squads.get(sid).and_then(|q| q.shape.perch.clone());
        let given_up = (self.taskforces.siege.forces.get(&sid).and_then(|f| f.perch.as_ref())).is_some_and(|p| p.abandoned);
        let natural = (self.taskforces.siege.forces.get(&sid).and_then(|f| f.station.as_ref())).map_or(0.0, |s| s.natural);
        match perch {
            Some(p) if !given_up => p.height.max(natural) + AIM_ABOVE,
            _ => 0.0,
        }
    }

    /// Whether a station is still safe to stand at: beyond every known
    /// static's reach, and under no defense on the threat map.
    fn siege_safe_by(&self, p: Vec3, leader: Unit, statics: &[Static], standoff: f32) -> bool {
        stands_off(p, statics, standoff) && self.war.view(leader.def()).get_porc_threat(p) <= 0.0
    }

    /// Whether the guns can stand at `p`: safe, reachable from where the
    /// leader is, and the straight way there under no known defense.
    fn siege_fits(&self, p: Vec3, leader: Unit, statics: &[Static]) -> bool {
        self.siege_fits_by(p, leader, statics, STANDOFF)
    }

    fn siege_fits_by(&self, p: Vec3, leader: Unit, statics: &[Static], standoff: f32) -> bool {
        self.siege_safe_by(p, leader, statics, standoff)
            && leader.def().is_none_or(|d| reaches(self.reach_terrain(), d, leader.pos(), p, AT_STATION))
            && self.siege_way_clear(leader.pos(), p, leader)
    }

    /// Whether a perch can go up at `p`: the top square flat enough.
    fn siege_perch_site_ok(&self, p: Vec3, perch: &Perch) -> bool {
        let map = self.cb.map();
        let h = perch.top / 2.0;
        let corners = [(-h, -h), (h, -h), (h, h), (-h, h)].map(|(dx, dz)| map.elevation_at(p.x + dx, p.z + dz));
        let (lo, hi) = corners.iter().fold((f32::INFINITY, f32::NEG_INFINITY), |(lo, hi), &c| (lo.min(c), hi.max(c)));
        hi - lo <= PERCH_FLAT && map.elevation_at(p.x, p.z) > 0.0
    }

    /// Whether the straight way from `from` to `to` keeps out of every known
    /// defense's reach.
    fn siege_way_clear(&self, from: Vec3, to: Vec3, leader: Unit) -> bool {
        let view = self.war.view(leader.def());
        let d = distance(from, to);
        let steps = (d / WAY_STEP).ceil().max(1.0) as usize;
        (1..steps).all(|k| {
            let f = k as f32 / steps as f32;
            view.get_porc_threat(Vec3::xz(from.x + f * (to.x - from.x), from.z + f * (to.z - from.z))) <= 0.0
        })
    }

    /// The station of force `sid` led by `leader`, and whether the force
    /// has yet to get there (so the wrapper moves it) or stands parked.
    pub(crate) fn siege_conduct(&mut self, sid: SquadId, leader: Unit) -> (Vec3, bool) {
        let pos = leader.pos();
        let up = self.siege_up(sid);
        let statics = self.siege_statics(up);
        let reach = Self::siege_reach(leader, up);
        let standoff = if up > 0.0 { PERCH_STANDOFF } else { STANDOFF };
        // A station is kept while it is safe and reaches something: the
        // guns stay until they have shot what they reach (every move costs
        // them their aim). A perch going up or up is kept whatever.
        let perching = (self.taskforces.siege.forces.get(&sid).and_then(|f| f.perch.as_ref())).is_some_and(|p| !p.abandoned);
        let kept = (self.taskforces.siege.forces.get(&sid).and_then(|f| f.station.as_ref()))
            .filter(|s| perching || self.siege_safe_by(s.pos, leader, &statics, standoff))
            .filter(|s| perching || in_reach(s.pos, reach, &statics).0 > 0)
            .map(|s| (s.pos, s.deployed));
        let station = match kept {
            Some((s, deployed)) => return (s, distance(pos, s) > if deployed { PARK_LEAVE } else { PARK }),
            None => {
                let (s, k, v, how, nat) = self.siege_pick(sid, leader, reach, up, &statics);
                let old = self.taskforces.siege.forces.get(&sid).and_then(|f| f.station.as_ref()).map(|s| s.pos);
                if old.is_none_or(|o| distance(o, s) > AT_STATION) {
                    let near = nearest(s, &statics);
                    let margin = near.map_or(0.0, |(d, n)| d - n.range);
                    let doctrine = self.squads.get(sid).map_or(String::new(), |q| q.shape.doctrine.clone());
                    self.log(&format!(
                        "taskforce: the {doctrine} force (squad {sid}) stations at ({:.0}, {:.0}), {:.0} off, in reach of \
                         {k} statics worth {v:.0}{}{}; {how}; the nearest armed static {}",
                        s.x,
                        s.z,
                        distance(pos, s),
                        if up > 0.0 { format!(" from {up:.0} up (reach {reach:.0})") } else { String::new() },
                        if nat > 0.0 { format!(", natural {nat:.0}") } else { String::new() },
                        match near {
                            Some((d, n)) if up > 0.0 => format!(
                                "is {d:.0} away (range {:.0}, {:.0} to {up:.0} up, standoff {margin:.0})",
                                n.nominal, n.raised
                            ),
                            Some((d, n)) => format!("is {d:.0} away (range {:.0}, standoff {margin:.0})", n.range),
                            None => "is unknown".to_owned(),
                        }
                    ));
                    if old.is_some() {
                        self.siege_release(sid, "restation");
                    }
                }
                let f = self.taskforces.siege.forces.entry(sid).or_default();
                // The same place again keeps the station as it is (parked
                // stays parked); a new one starts unparked.
                if old.is_none_or(|o| distance(o, s) > AT_STATION) {
                    f.station = Some(Station { pos: s, reach: (k, v), deployed: false, natural: nat });
                } else if let Some(st) = f.station.as_mut() {
                    st.reach = (k, v);
                    st.natural = nat;
                }
                s
            }
        };
        (station, distance(pos, station) > PARK)
    }

    /// A station for force `sid`: the best candidate the guns can reach
    /// that is safe; with no static known, where artillery would be sent,
    /// pulled back to the guns' reach (or the nearest haven to it). How
    /// many statics it reaches, their worth, and how it was found.
    fn siege_pick(
        &mut self,
        sid: SquadId,
        leader: Unit,
        reach: f32,
        up: f32,
        statics: &[Static],
    ) -> (Vec3, usize, f32, &'static str, f32) {
        if let Some(f) = self.taskforces.siege.forces.get_mut(&sid) {
            f.pending_natural = 0.0;
        }
        let (s, k, v, how) = self.siege_pick_at(sid, leader, reach, up, statics);
        let nat = self.taskforces.siege.forces.get(&sid).map_or(0.0, |f| f.pending_natural);
        (s, k, v, how, nat)
    }

    /// The natural height of a site over the nearest armed static we know
    /// (its ground to the static's ground), 0 if none is known or it lies
    /// lower.
    fn siege_natural(&self, p: Vec3, statics: &[Static]) -> f32 {
        let map = self.cb.map();
        let Some((_, n)) = nearest(p, statics) else { return 0.0 };
        (map.elevation_at(p.x, p.z) - map.elevation_at(n.pos.x, n.pos.z)).max(0.0)
    }

    fn siege_pick_at(
        &mut self,
        sid: SquadId,
        leader: Unit,
        reach: f32,
        up: f32,
        statics: &[Static],
    ) -> (Vec3, usize, f32, &'static str) {
        let from = leader.pos();
        let map = self.cb.map();
        let (w, h) = (map.width() as f32 * 8.0, map.height() as f32 * 8.0);
        let on_map = |x: f32, z: f32| x >= 50.0 && z >= 50.0 && x <= w - 50.0 && z <= h - 50.0;
        // A perch: a site on the tight rings, flat, standing off the
        // statics' reach to the raised aim point, reaching enough from up
        // there; else the force stands flat (and says so).
        let perch = self.squads.get(sid).and_then(|q| q.shape.perch.clone());
        if let (Some(perch), true) = (&perch, up > 0.0 && !statics.is_empty()) {
            // Natural perches: each candidate scored by the reach from its
            // own height (plus the raise that would complete it) over the
            // nearest defense, natural ones preferred.
            if let Some(natural) = &perch.natural {
                let flat = self.siege_statics(0.0);
                let picks = self.taskforces.siege.sites.sites(from, reach, statics, PERCH_STANDOFF, (w, h));
                let mut best: Option<(Vec3, f32, f32, usize, f32)> = None;
                let mut cache: Vec<(i32, Vec<Static>, f32)> = Vec::new();
                for &p in &picks {
                    let nat = self.siege_natural(p, &flat);
                    let total = nat.max(perch.height);
                    let key = (total / 10.0).round() as i32;
                    let (st, rch) = match cache.iter().find(|c| c.0 == key) {
                        Some(c) => (c.1.clone(), c.2),
                        None => {
                            let st = self.siege_statics(total + AIM_ABOVE);
                            let rch = Self::siege_reach(leader, total);
                            cache.push((key, st.clone(), rch));
                            (st, rch)
                        }
                    };
                    let (k, v) = in_reach(p, rch, &st);
                    if k < perch.min_reach as usize
                        || !self.siege_perch_site_ok(p, perch)
                        || !self.siege_fits_by(p, leader, &st, PERCH_STANDOFF)
                    {
                        continue;
                    }
                    let mut score = v / (1.0 + distance(from, p) / 1000.0);
                    if nat >= natural.min_height {
                        score *= natural.weight;
                    }
                    if best.is_none_or(|b| score > b.1 || (score == b.1 && nat > b.2)) {
                        best = Some((p, score, nat, k, v));
                    }
                }
                if let Some((p, _, nat, k, v)) = best {
                    let how = if nat >= natural.min_height { "the best natural perch site" } else { "the best perch site" };
                    let site = Vec3::new(p.x, map.elevation_at(p.x, p.z), p.z);
                    if let Some(f) = self.taskforces.siege.forces.get_mut(&sid) {
                        f.pending_natural = nat;
                    }
                    return (site, k, v, how);
                }
            }
            let picks = self.taskforces.siege.sites.sites(from, reach, statics, PERCH_STANDOFF, (w, h));
            let site = picks.iter().find(|&&c| {
                in_reach(c, reach, statics).0 >= perch.min_reach as usize
                    && self.siege_perch_site_ok(c, perch)
                    && self.siege_fits_by(c, leader, statics, PERCH_STANDOFF)
            });
            if let Some(&p) = site {
                let (k, v) = in_reach(p, reach, statics);
                return (Vec3::new(p.x, map.elevation_at(p.x, p.z), p.z), k, v, "the best perch site");
            }
            let near = statics.iter().filter(|s| s.range > 0.0).count();
            let frame = self.frame;
            let said = self.taskforces.siege.forces.get(&sid).is_some_and(|f| frame - f.refused_at < REPORT);
            if !said {
                self.taskforces.siege.forces.entry(sid).or_default().refused_at = frame;
                self.op_emit(
                    "siege-perch",
                    json!({"event": "refused", "squad": sid, "reason": "no site", "candidates": picks.len(), "armed_statics": near,
                           "min_reach": perch.min_reach, "reach_up": reach}),
                );
                self.log(&format!(
                    "taskforce: a {} finds no perch site: {} candidates about {near} armed statics reach fewer than {} or stand in reach; it stands flat",
                    self.defs.name_of(leader),
                    picks.len(),
                    perch.min_reach
                ));
            }
            // Flat it is, then: the flat statics and reach.
            let flat = self.siege_statics(0.0);
            let reach0 = Self::siege_reach(leader, 0.0);
            let picks = candidates(from, reach0, &flat, STANDOFF, on_map);
            if let Some(&(p, _)) = picks.iter().find(|c| self.siege_fits(c.0, leader, &flat)) {
                let (k, v) = in_reach(p, reach0, &flat);
                return (Vec3::new(p.x, map.elevation_at(p.x, p.z), p.z), k, v, "no perch site; the best flat stand");
            }
        } else if !statics.is_empty() {
            let picks = candidates(from, reach, statics, STANDOFF, on_map);
            if let Some(&(p, _)) = picks.iter().find(|c| self.siege_fits(c.0, leader, statics)) {
                let (k, v) = in_reach(p, reach, statics);
                return (Vec3::new(p.x, map.elevation_at(p.x, p.z), p.z), k, v, "the best safe candidate");
            }
        }
        // Nothing known, or nothing we can stand off: toward what artillery
        // would hit, at the guns' reach from it.
        let _ = sid;
        let at = self.war_get_arty_target(from, false);
        let at = if at == NULLPOS { self.graph.get_enemy_center() } else { at };
        let back = get_directional_point(at, from, (reach - 100.0).max(0.0));
        if on_map(back.x, back.z) && self.siege_fits(back, leader, statics) {
            let (k, v) = in_reach(back, reach, statics);
            return (back, k, v, "no static to stand off; at reach of the artillery target");
        }
        let haven = self.graph_get_closest_haven(at);
        let (k, v) = in_reach(haven, reach, statics);
        (haven, k, v, "no safe stand; the haven nearest the artillery target")
    }

    /// Each conduct of a siege force: park the guns at the station once the
    /// leader stands there, set their targets, keep the books, and release
    /// them when the force moves off (`why`: the wrapper's reason).
    pub(crate) fn siege_tick(&mut self, sid: SquadId, engaging: bool, why: &str) {
        let frame = self.frame;
        let Some(s) = self.squads.get(sid) else { return };
        let shape = s.shape.clone();
        let leader = s.get_leader(&self.fighters);
        let engaged_ever = s.engaged != 0;
        let members: Vec<(i32, Unit, bool)> = (s.everyone().filter_map(|id| self.fighters.get(&id)))
            .map(|f| (f.id, f.unit, shape.is_core(f.unit.def().map(|d| d.id()))))
            .collect();
        let station = (self.taskforces.siege.forces.get(&sid).and_then(|f| f.station.as_ref())).map(|s| s.pos);
        let was = self.taskforces.siege.forces.get(&sid).and_then(|f| f.station.as_ref()).is_some_and(|s| s.deployed);
        let parked_force = match (engaging, leader, station) {
            (true, Some(l), Some(st)) => distance(l.pos(), st) <= if was { PARK_LEAVE } else { PARK },
            _ => false,
        };
        if was && !parked_force {
            self.siege_release(sid, if engaging { "moving off" } else { why });
        }
        if let Some(st) = self.taskforces.siege.forces.get_mut(&sid).and_then(|f| f.station.as_mut()) {
            st.deployed = parked_force;
        }
        let up = self.siege_up(sid);
        let statics = self.siege_statics(up);
        let reach = leader.map_or(1100.0, |l| Self::siege_reach(l, up));
        // A gun fires out to its range; the reach (less a margin) is for
        // choosing stations.
        let range = reach + RANGE_MARGIN;
        let shields = self.impalers_shields();
        // The perch under the leading core (see `siege_perch`); its escorts
        // stand on the home side of the pillar, each at its role's orbit
        // from it (orbiting the core would carry them into the statics'
        // reach the core stands off by 40 only).
        if let (Some(perch), true, Some(l), Some(st)) = (shape.perch.clone(), parked_force, leader, station) {
            self.siege_perch(sid, &perch, l, st, frame);
            let home = self.graph.get_start_pos();
            for &(id, unit, core) in &members {
                if core || unit.id() == l.id() {
                    continue;
                }
                let orbit = shape.orbit(unit.def().map(|d| d.id()));
                let foot = get_directional_point(st, home, orbit.max(60.0));
                let sent = self.taskforces.siege.forces.get(&sid).and_then(|f| f.sent.get(&id)).copied();
                if distance(unit.pos(), foot) > AT_STATION && sent.is_none_or(|t| frame - t >= RESEND) {
                    kgb_move_to_exact(unit, foot, 20.0, 0, frame);
                    if let Some(f) = self.taskforces.siege.forces.get_mut(&sid) {
                        f.sent.insert(id, frame);
                    }
                }
            }
        } else if !parked_force && let Some(f) = self.taskforces.siege.forces.get_mut(&sid) {
            f.on_site_sent = 0;
        }
        let mut k = 0usize;
        for &(id, unit, core) in &members {
            if !core {
                continue;
            }
            let p = unit.pos();
            let mut state: &'static str = match (engaging, engaged_ever) {
                (true, _) => "moving",
                (false, false) => "forming",
                (false, true) => "rallying",
            };
            if parked_force && let Some(st) = station {
                // The leader at the station, the other guns each at a spot.
                let is_leader = leader.is_some_and(|l| l.id() == id);
                let spot = if is_leader {
                    st
                } else {
                    k += 1;
                    let a = k as f32 * 2.4;
                    Vec3::xz(st.x + SPREAD * a.cos(), st.z + SPREAD * a.sin())
                };
                let parked = distance(p, spot) <= if is_leader { PARK_LEAVE } else { AT_STATION };
                if parked {
                    let has = (self.war.targets.values()).any(|e| {
                        e.identified && !e.is_dead && (e.visible || e.is_static) && distance(p, e.position) <= range
                    });
                    state = if has { "deployed in range" } else { "deployed idle" };
                    if is_leader && let Some(ps) = self.taskforces.siege.forces.get(&sid).and_then(|f| f.perch.as_ref()) {
                        if ps.up {
                            state = "perched";
                        } else if !ps.abandoned {
                            state = "raising";
                        }
                    }
                    let first = !self.taskforces.siege.guns.get(&id).is_some_and(|g| g.parked);
                    if first {
                        let near = nearest(p, &statics);
                        let (n, v) = in_reach(p, reach, &statics);
                        self.log(&format!(
                            "taskforce: a {} deploys at ({:.0}, {:.0}), {}; {n} statics worth {v:.0} in reach (unit {id})",
                            self.defs.name_of(unit),
                            p.x,
                            p.z,
                            match near {
                                Some((d, s)) => format!(
                                    "{d:.0} off a {} (range {:.0}, {})",
                                    self.war
                                        .targets
                                        .values()
                                        .find(|e| e.position == s.pos)
                                        .map_or("?", |e| e.def_name.as_str()),
                                    s.range,
                                    if s.visible { "in sight" } else { "a ghost" }
                                ),
                                None => "no armed static known".to_owned(),
                            }
                        ));
                    }
                    let perched_up = is_leader
                        && (self.taskforces.siege.forces.get(&sid).and_then(|f| f.perch.as_ref())).is_some_and(|p| p.up);
                    if perched_up {
                        self.siege_shell(sid, unit, range, &shields);
                    } else {
                        self.siege_aim(id, unit, range, &shields);
                    }
                } else if !is_leader {
                    let sent = self.taskforces.siege.forces.get(&sid).and_then(|f| f.sent.get(&id)).copied();
                    if sent.is_none_or(|t| frame - t >= RESEND) {
                        kgb_move_to_exact(unit, spot, 20.0, 0, frame);
                        if let Some(f) = self.taskforces.siege.forces.get_mut(&sid) {
                            f.sent.insert(id, frame);
                        }
                    }
                }
                let g = self.taskforces.siege.guns.entry(id).or_default();
                if parked && !g.held {
                    // Hold position: on maneuver a gun firing at will walks
                    // after its targets and off its spot.
                    unit.set_move_state(0, 0);
                    g.held = true;
                }
                g.parked = parked;
            }
            let g = self.taskforces.siege.guns.entry(id).or_default();
            if g.last > 0 {
                *g.frames.entry(g.state).or_default() += frame - g.last;
            }
            g.last = frame;
            g.state = state;
        }
        self.siege_report(sid, frame);
    }

    /// The perch of force `sid` (the Crab spire): once the force is parked,
    /// the leading core `leader` is walked exactly onto the station `site`;
    /// standing there, constructors are borrowed from the economy (the
    /// nearest free ones within the perch's range, as many as its count
    /// says) and level the top square to its highest corner plus the
    /// height; the raise is up within `UP_SLACK` of that, and given up
    /// after `borrow_s` seconds (the core then fights flat). Journal
    /// records `siege-perch` tell the start, the raisers called away, up
    /// and given up.
    fn siege_perch(&mut self, sid: SquadId, perch: &Perch, leader: Unit, site: Vec3, frame: i32) {
        let p = leader.pos();
        let f = self.taskforces.siege.forces.entry(sid).or_default();
        match f.perch.as_mut() {
            None => {
                if distance(p, site) > ON_SITE {
                    if frame - f.on_site_sent >= RESEND {
                        kgb_move_to_exact(leader, site, 8.0, 0, frame);
                        f.on_site_sent = frame;
                    }
                    return;
                }
                // On the site: hold, borrow the raisers, raise.
                leader.set_move_state(0, 0);
                let (raisers, want) = self.siege_borrow(sid, perch, site, &[]);
                let map = self.cb.map();
                let now = map.elevation_at(site.x, site.z);
                // The ground before any pillar here (a raise taken up again
                // does not stack on the first).
                let known = self.taskforces.siege.pillars.iter().find(|(p, _)| distance(*p, site) <= SAME_PILLAR).map(|x| x.1);
                let ground = known.unwrap_or(now);
                if known.is_none() {
                    self.taskforces.siege.pillars.push((site, ground));
                }
                if now - ground >= perch.height - UP_SLACK {
                    // The pillar stands already (raised before and left).
                    for u in &raisers {
                        u.stop(0);
                    }
                    self.op_emit(
                        "siege-perch",
                        json!({"event": "up", "squad": sid, "site": [site.x, site.z], "raised": now - ground, "seconds": 0, "raisers": 0, "standing": true}),
                    );
                    self.log(&format!(
                        "taskforce: a {} is perched {:.0} up at ({:.0}, {:.0}) on a pillar that stood",
                        self.defs.name_of(leader), now - ground, site.x, site.z
                    ));
                    if let Some(f) = self.taskforces.siege.forces.get_mut(&sid) {
                        f.perch = Some(PerchState {
                            site,
                            ground,
                            since: frame,
                            raisers: Vec::new(),
                            up: true,
                            abandoned: false,
                            ground_target: None,
                            foot_frames: 0,
                            foot_last: frame,
                            returned: Some(frame),
                            stranded: None,
                            raise: perch.height,
                            tries: 0,
                            square: [site; 4],
                            level: 0.0,
                        });
                    }
                    return;
                }
                if raisers.is_empty() {
                    self.op_emit(
                        "siege-perch",
                        json!({"event": "refused", "squad": sid, "reason": "no raisers", "site": [site.x, site.z], "wanted": want}),
                    );
                    if let Some(f) = self.taskforces.siege.forces.get_mut(&sid) {
                        f.perch = Some(PerchState {
                            site,
                            ground,
                            since: frame,
                            raisers: Vec::new(),
                            up: false,
                            abandoned: true,
                            ground_target: None,
                            foot_frames: 0,
                            foot_last: frame,
                            returned: Some(frame),
                            stranded: None,
                            raise: perch.height,
                            tries: 0,
                            square: [site; 4],
                            level: 0.0,
                        });
                    }
                    self.log(&format!(
                        "taskforce: a {} refuses its perch at ({:.0}, {:.0}): no constructor to raise it; it stands flat",
                        self.defs.name_of(leader), site.x, site.z
                    ));
                    return;
                }
                let h = perch.top / 2.0;
                let square = [(-h, -h), (h, -h), (h, h), (-h, h)].map(|(dx, dz)| Vec3::xz(site.x + dx, site.z + dz));
                let top = square.iter().map(|c| map.elevation_at(c.x, c.z)).fold(ground, f32::max);
                let names: Vec<String> = raisers.iter().map(|u| self.defs.name_of(*u).to_owned()).collect();
                let hold = (perch.borrow_s * 30.0) as i32;
                // On a natural shoulder the raise asks the remainder only.
                let natural = self.taskforces.siege.forces.get(&sid).and_then(|f| f.station.as_ref()).map_or(0.0, |s| s.natural);
                let raise = (perch.height - natural).max(0.0);
                if raise < UP_SLACK {
                    for u in &raisers {
                        u.stop(0);
                    }
                    self.op_emit(
                        "siege-perch",
                        json!({"event": "up", "squad": sid, "site": [site.x, site.z], "raised": 0.0, "natural": natural, "seconds": 0, "raisers": 0, "standing": true}),
                    );
                    self.log(&format!(
                        "taskforce: a {} is perched at once at ({:.0}, {:.0}), {natural:.0} up by nature",
                        self.defs.name_of(leader), site.x, site.z
                    ));
                    if let Some(f) = self.taskforces.siege.forces.get_mut(&sid) {
                        f.perch = Some(PerchState {
                            site,
                            ground,
                            since: frame,
                            raisers: Vec::new(),
                            up: true,
                            abandoned: false,
                            ground_target: None,
                            foot_frames: 0,
                            foot_last: frame,
                            returned: Some(frame),
                            stranded: None,
                            raise: 0.0,
                            tries: 0,
                            square,
                            level: top,
                        });
                    }
                    return;
                }
                self.terraform_level(&raisers, &square, top + raise, false);
                // Held for the borrow: the economy's assignment leaves them
                // to the raise (crab save arms: 46 of 49 raises given up for
                // raisers called away).
                self.eco_hold_terraform(&raisers, hold);
                // What the raised point reaches, and the nearest armed
                // static's reach to it (the standoff the gates read).
                let up = perch.height.max(natural) + AIM_ABOVE;
                let statics = self.siege_statics(up);
                let reach = Self::siege_reach(leader, up);
                let (k, v) = in_reach(site, reach, &statics);
                let near = nearest(site, &statics);
                self.op_emit(
                    "siege-perch",
                    json!({"event": "start", "squad": sid, "site": [site.x, site.z], "ground": ground, "height": perch.height,
                           "natural": natural, "raise": raise,
                           "top": perch.top, "raisers": raisers.iter().map(|u| u.id()).collect::<Vec<_>>(), "wanted": want,
                           "reach_up": reach, "statics_in_reach": k, "worth_in_reach": v,
                           "nearest": near.map(|(d, n)| json!({"distance": d, "range": n.nominal, "reach_up": n.raised, "standoff": d - n.range}))}),
                );
                self.log(&format!(
                    "taskforce: a {} perches at ({:.0}, {:.0}): {} raise it {:.0}{} ({}); {k} statics worth {v:.0} in reach from {up:.0} up (reach {reach:.0}); the nearest armed static {}",
                    self.defs.name_of(leader),
                    site.x,
                    site.z,
                    raisers.len(),
                    raise,
                    if natural > 0.0 { format!(" on a natural {natural:.0}") } else { String::new() },
                    names.join(", "),
                    match near {
                        Some((d, n)) => format!("is {d:.0} away (range {:.0}, {:.0} to {up:.0} up, standoff {:.0})", n.nominal, n.raised, d - n.range),
                        None => "is unknown".to_owned(),
                    }
                ));
                if let Some(f) = self.taskforces.siege.forces.get_mut(&sid) {
                    f.perch = Some(PerchState {
                        site,
                        ground,
                        since: frame,
                        raisers,
                        up: false,
                        abandoned: false,
                        ground_target: None,
                        foot_frames: 0,
                        foot_last: frame,
                        returned: None,
                        stranded: None,
                        raise,
                        tries: 1,
                        square,
                        level: top + raise,
                    });
                }
            }
            Some(ps) if !ps.up && !ps.abandoned => {
                let raised = self.cb.map().elevation_at(ps.site.x, ps.site.z) - ps.ground;
                let working = ps.raisers.iter().filter(|u| u.health() > 0.0 && is_terraforming(**u)).count();
                let since = (frame - ps.since) / 30;
                // An enemy at the foot slows the gadget 20x: count the time.
                let site = ps.site;
                let foot = (self.war.targets.values()).any(|e| !e.is_dead && e.visible && distance(e.position, site) <= FOOT);
                if foot {
                    ps.foot_frames += frame - ps.foot_last;
                }
                ps.foot_last = frame;
                let foot_s = ps.foot_frames / 30;
                if raised >= ps.raise - UP_SLACK {
                    ps.up = true;
                    ps.returned = Some(frame);
                    let n = ps.raisers.len();
                    let raisers = ps.raisers.clone();
                    self.op_emit(
                        "siege-perch",
                        json!({"event": "up", "squad": sid, "site": [site.x, site.z], "raised": raised, "seconds": since, "raisers": n,
                               "foot_seconds": foot_s, "weaver_seconds": since as usize * n}),
                    );
                    self.log(&format!(
                        "taskforce: a {} is perched {raised:.0} up at ({:.0}, {:.0}) after {since} s ({n} raisers; an enemy at the foot {foot_s} s); it returns {n} constructors after {since} s each",
                        self.defs.name_of(leader), site.x, site.z
                    ));
                    for u in &raisers {
                        if u.health() > 0.0 {
                            u.stop(0);
                        }
                    }
                } else if working == 0 && since >= 20 && ps.tries < RAISE_TRIES && (since as f32) < perch.borrow_s {
                    // The raisers were called away (threatened, or taken):
                    // others are sent, up to RAISE_TRIES within the borrow.
                    let (square, level, tries) = (ps.square, ps.level, ps.tries);
                    let gone: Vec<Unit> = ps.raisers.clone();
                    let (more, _) = self.siege_borrow(sid, perch, site, &gone);
                    let Some(ps) = self.taskforces.siege.forces.get_mut(&sid).and_then(|f| f.perch.as_mut()) else { return };
                    if more.is_empty() {
                        ps.tries = RAISE_TRIES;
                    } else {
                        ps.raisers = more.clone();
                        ps.tries = tries + 1;
                        let left = ((perch.borrow_s - since as f32) * 30.0) as i32;
                        self.terraform_level(&more, &square, level, false);
                        self.eco_hold_terraform(&more, left.max(60));
                        self.op_emit(
                            "siege-perch",
                            json!({"event": "resent", "squad": sid, "site": [site.x, site.z], "raised": raised, "seconds": since, "raisers": more.len(), "try": tries + 1}),
                        );
                        self.log(&format!(
                            "taskforce: a {}'s raisers were called away at ({:.0}, {:.0}), {raised:.0} up after {since} s; {} more sent (try {})",
                            self.defs.name_of(leader), site.x, site.z, more.len(), tries + 1
                        ));
                    }
                } else if since as f32 >= perch.borrow_s || (working == 0 && since >= 20) {
                    ps.abandoned = true;
                    ps.returned = Some(frame);
                    let n = ps.raisers.len();
                    let raisers = ps.raisers.clone();
                    let why = if working == 0 { "the raisers were called away" } else { "time ran out" };
                    self.op_emit(
                        "siege-perch",
                        json!({"event": "given up", "squad": sid, "site": [site.x, site.z], "raised": raised, "seconds": since, "reason": why,
                               "foot_seconds": foot_s, "weaver_seconds": since as usize * n}),
                    );
                    self.log(&format!(
                        "taskforce: a {} gives up its perch at ({:.0}, {:.0}), {raised:.0} up after {since} s: {why} (an enemy at the foot {foot_s} s); it fights flat and returns {n} constructors after {since} s each",
                        self.defs.name_of(leader), site.x, site.z
                    ));
                    for u in &raisers {
                        if u.health() > 0.0 && is_terraforming(*u) {
                            u.stop(0);
                        }
                    }
                }
            }
            Some(_) => {}
        }
    }

    /// Borrow constructors for the perch of force `sid` at `site`: the
    /// nearest free ones of the perch's types within its range, as many as
    /// its count says, not those in `not` (sent before and called away);
    /// taken off their tasks. With how many were wanted.
    fn siege_borrow(&mut self, sid: SquadId, perch: &Perch, site: Vec3, not: &[Unit]) -> (Vec<Unit>, usize) {
        let want = {
            let Some(q) = self.squads.get(sid) else { return (Vec::new(), 0) };
            let Some(d) = self.taskforces.doctrine(&q.shape.doctrine) else { return (Vec::new(), 0) };
            let units = q.everyone().filter_map(|id| self.fighters.get(&id));
            let force = d.force(units.map(|f| (self.defs.name_of(f.unit), f.metal_value)), &self.taskforces.classes);
            perch.count.want(&force).max(1)
        };
        let mut near: Vec<(f32, i32)> = (self.eco.workers.values())
            .filter(|w| !w.is_chicken && !w.assigned_plop && !w.is_com && !w.unit.is_being_built())
            .filter(|w| perch.raisers.iter().any(|r| r == self.defs.name_of(w.unit)))
            .filter(|w| !not.iter().any(|u| u.id() == w.id))
            .map(|w| (distance(w.get_pos(), site), w.id))
            .filter(|&(d, _)| d <= perch.range)
            .collect();
        near.sort_by(|a, b| a.0.total_cmp(&b.0));
        let mut raisers = Vec::new();
        for (_, wid) in near.into_iter().take(want) {
            if let Some(w) = self.eco.workers.get_mut(&wid) {
                w.clear_task(&mut self.eco.tasks);
                raisers.push(w.unit);
            }
        }
        (raisers, want)
    }

    /// A perched core shells the most valuable known static in its reach
    /// beyond its sight as ground (an attack order at the building's place,
    /// which a cannon with area of effect needs no sight for), and takes the
    /// best thing in sight as its target otherwise.
    fn siege_shell(&mut self, sid: SquadId, unit: Unit, reach: f32, shields: &[(Vec3, f32)]) {
        let p = unit.pos();
        let beyond = (self.war.targets.values())
            .filter(|e| e.identified && e.is_static && !e.is_dead && e.position != NULLPOS)
            .filter(|e| {
                let d = distance(p, e.position);
                d > SIGHT && d <= reach
            })
            .filter(|e| !shields.iter().any(|&(s, r)| distance(s, e.position) <= r + 10.0))
            .map(|e| (e.unit_id, e.position, worth(e, e.ud.map_or(e.value, |d| d.cost(self.metal)))))
            .max_by(|a, b| a.2.total_cmp(&b.2));
        let in_sight = (self.war.targets.values())
            .any(|e| e.identified && !e.is_dead && (e.visible || e.is_static) && distance(p, e.position) <= SIGHT);
        let last = self.taskforces.siege.forces.get(&sid).and_then(|f| f.perch.as_ref()).and_then(|ps| ps.ground_target);
        match beyond {
            Some((id, at, _)) if !in_sight => {
                if last != Some(id) {
                    let y = self.cb.map().elevation_at(at.x, at.z);
                    unit.execute_custom_command(CMD_ATTACK, &[at.x, y, at.z], 0, NO_TIMEOUT);
                    if let Some(ps) = self.taskforces.siege.forces.get_mut(&sid).and_then(|f| f.perch.as_mut()) {
                        ps.ground_target = Some(id);
                    }
                    let name = self.war.targets.get(&id).map_or("?", |e| e.def_name.as_str()).to_owned();
                    self.log(&format!(
                        "taskforce: a {} shells a {name} at ({:.0}, {:.0}), {:.0} off, beyond its sight",
                        self.defs.name_of(unit), at.x, at.z, distance(p, at)
                    ));
                }
            }
            _ => {
                if last.is_some() {
                    unit.stop(0);
                    if let Some(ps) = self.taskforces.siege.forces.get_mut(&sid).and_then(|f| f.perch.as_mut()) {
                        ps.ground_target = None;
                    }
                }
                self.siege_aim(unit.id(), unit, range_of(reach), shields);
            }
        }
    }

    /// Set gun `id` on the most valuable enemy in its reach whose place we
    /// know (in sight, or a static), not under a known shield; or let it
    /// be. Marks are kept with the marksmen's, so kills log alike.
    fn siege_aim(&mut self, id: i32, unit: Unit, reach: f32, shields: &[(Vec3, f32)]) {
        let p = unit.pos();
        let mine = self.taskforces.marks.get(&id).map(|m| m.target);
        let best = (self.war.targets.values())
            .filter(|e| e.identified && !e.is_dead && !e.can_fly() && e.position != NULLPOS)
            .filter(|e| (e.visible || e.is_static) && e.def_name != "wolverine_mine")
            .filter(|e| distance(p, e.position) <= reach)
            .filter(|e| !shields.iter().any(|&(s, r)| distance(s, e.position) <= r + 10.0))
            .map(|e| (e.unit_id, worth(e, e.ud.map_or(e.value, |d| d.cost(self.metal)))))
            .max_by(|a, b| a.1.total_cmp(&b.1).then_with(|| (Some(a.0) == mine).cmp(&(Some(b.0) == mine))));
        match best {
            Some((target, _)) if Some(target) != mine => {
                unit.execute_custom_command(cmd::UNIT_SET_TARGET, &[target as f32], 0, NO_TIMEOUT);
                self.taskforces.marks.insert(id, Mark::plain(target, self.frame));
            }
            Some(_) => {}
            None => {
                if self.taskforces.marks.remove(&id).is_some() {
                    unit.execute_custom_command(cmd::UNIT_CANCEL_TARGET, &[], 0, NO_TIMEOUT);
                }
            }
        }
    }

    /// The guns of force `sid` leave their spots (`why`): each parked one
    /// logs it, and those not leading orbit the leader again.
    fn siege_release(&mut self, sid: SquadId, why: &str) {
        let Some(s) = self.squads.get(sid) else { return };
        let shape = s.shape.clone();
        let leader = s.leader;
        let cores: Vec<(i32, Unit)> = (s.everyone().filter_map(|id| self.fighters.get(&id)))
            .filter(|f| shape.is_core(f.unit.def().map(|d| d.id())))
            .map(|f| (f.id, f.unit))
            .collect();
        // Escorts that stood at a perch's foot orbit the leader again.
        if shape.perch.is_some() && let Some(l) = leader {
            let sent: Vec<i32> = self.taskforces.siege.forces.get(&sid).map_or(Vec::new(), |f| f.sent.keys().copied().collect());
            for id in sent {
                if let Some(f) = self.fighters.get(&id).filter(|f| !shape.is_core(f.unit.def().map(|d| d.id()))) {
                    let radius = shape.orbit(f.unit.def().map(|d| d.id()));
                    f.unit.execute_custom_command(cmd::ORBIT, &[l as f32, radius], 0, NO_TIMEOUT);
                }
            }
        }
        for (id, unit) in cores {
            let Some(g) = self.taskforces.siege.guns.get_mut(&id) else { continue };
            if g.held {
                g.held = false;
                unit.set_move_state(if Some(id) == leader { 1 } else { 0 }, 0);
            }
            if g.parked {
                g.parked = false;
                let stood = g.frames.get("deployed in range").copied().unwrap_or(0)
                    + g.frames.get("deployed idle").copied().unwrap_or(0);
                let p = unit.pos();
                let state = g.state;
                self.log(&format!(
                    "taskforce: a {} undeploys ({why}) at ({:.0}, {:.0}), {state} lately, deployed {} s in all (unit {id})",
                    self.defs.name_of(unit),
                    p.x,
                    p.z,
                    stood / 30
                ));
            }
            if Some(id) != leader
                && let Some(l) = leader
            {
                unit.execute_custom_command(cmd::ORBIT, &[l as f32, ORBIT], 0, NO_TIMEOUT);
                unit.execute_custom_command(cmd::ORBIT_DRAW, &[l as f32], springai::opt::SHIFT_KEY, NO_TIMEOUT);
            }
            if self.taskforces.marks.remove(&id).is_some() {
                unit.execute_custom_command(cmd::UNIT_CANCEL_TARGET, &[], 0, NO_TIMEOUT);
            }
        }
        if let Some(f) = self.taskforces.siege.forces.get_mut(&sid) {
            f.sent.clear();
            f.on_site_sent = 0;
            if let Some(st) = f.station.as_mut() {
                st.deployed = false;
            }
            // A perch left is a perch lost (the pillar stays); its raisers
            // go back to the economy.
            if let Some(p) = f.perch.take().filter(|p| !p.abandoned) {
                for u in &p.raisers {
                    if u.health() > 0.0 && is_terraforming(*u) {
                        u.stop(0);
                    }
                }
                let borrowed = (p.returned.unwrap_or(self.frame) - p.since) / 30;
                self.op_emit(
                    "siege-perch",
                    json!({"event": "left", "squad": sid, "reason": why, "site": [p.site.x, p.site.z], "was_up": p.up,
                           "perched_seconds": if p.up { (self.frame - p.returned.unwrap_or(p.since)) / 30 } else { 0 },
                           "weaver_seconds": borrowed as usize * p.raisers.len()}),
                );
                self.log(&format!(
                    "taskforce: the pillar at ({:.0}, {:.0}) is left ({why}){}; {} constructors were borrowed {borrowed} s each",
                    p.site.x,
                    p.site.z,
                    if p.up { format!(", perched {} s", (self.frame - p.returned.unwrap_or(p.since)) / 30) } else { String::new() },
                    p.raisers.len()
                ));
            }
        }
    }

    /// How the guns of force `sid` have stood, now and then.
    fn siege_report(&mut self, sid: SquadId, frame: i32) {
        let Some(f) = self.taskforces.siege.forces.get_mut(&sid) else { return };
        if frame - f.last_report < REPORT {
            return;
        }
        f.last_report = frame;
        let Some(s) = self.squads.get(sid) else { return };
        let shape = s.shape.clone();
        let guns: Vec<i32> = (s.everyone().filter_map(|id| self.fighters.get(&id)))
            .filter(|f| shape.is_core(f.unit.def().map(|d| d.id())))
            .map(|f| f.id)
            .collect();
        let mut sum: BTreeMap<&'static str, i32> = BTreeMap::new();
        for g in guns.iter().filter_map(|id| self.taskforces.siege.guns.get(id)) {
            for (k, v) in &g.frames {
                *sum.entry(k).or_default() += v;
            }
        }
        let part: Vec<String> = sum.iter().map(|(k, v)| format!("{k} {} s", v / 30)).collect();
        let station = f.station.as_ref().map(|s| (s.pos, s.reach, s.deployed));
        self.log(&format!(
            "taskforce: the {} force (squad {sid}) with {} guns has stood {}; station {}",
            shape.doctrine,
            guns.len(),
            if part.is_empty() { "nowhere yet".to_owned() } else { part.join(", ") },
            match station {
                Some((p, (k, v), d)) => format!(
                    "({:.0}, {:.0}) in reach of {k} statics worth {v:.0}, {}",
                    p.x,
                    p.z,
                    if d { "parked" } else { "not reached" }
                ),
                None => "none".to_owned(),
            }
        ));
    }

    /// For the death line of a member of a siege force: how a gun stood
    /// when it died and for how long each way; for an escort, how its force
    /// stood.
    pub(crate) fn siege_death_note(&self, sid: SquadId, id: i32) -> Option<String> {
        let s = self.squads.get(sid)?;
        if s.shape.aim != Aim::Siege {
            return None;
        }
        let f = self.taskforces.siege.forces.get(&sid);
        let deployed = f.and_then(|f| f.station.as_ref()).is_some_and(|s| s.deployed);
        match self.taskforces.siege.guns.get(&id) {
            Some(g) => {
                let part: Vec<String> = g.frames.iter().map(|(k, v)| format!("{k} {} s", v / 30)).collect();
                Some(format!(
                    "; siege: {}; stood {}",
                    if g.state.is_empty() { "unplaced" } else { g.state },
                    if part.is_empty() { "nowhere yet".to_owned() } else { part.join(", ") }
                ))
            }
            None => Some(format!("; siege: escort, force {}", if deployed { "deployed" } else { "not deployed" })),
        }
    }

    /// Whether a new unit of type `name` is claimed by its siege doctrine:
    /// not while the doctrine's `claim_with_site` holds and no perch site
    /// is known for a force led by it from where it stands (it then goes
    /// the army's way, a loner, until one is; said once per unit).
    pub(crate) fn siege_claims(&mut self, unit: Unit, name: &str) -> bool {
        let tf = &self.taskforces;
        let Some(d) = (tf.doctrines.iter())
            .find(|d| d.enabled && d.aim == Aim::Siege && d.claim_with_site && d.is_core(name, &tf.classes))
        else {
            return true;
        };
        let Some(perch) = d.perch.clone() else { return true };
        let min_reach = perch.min_reach as usize;
        let up = perch.height + AIM_ABOVE;
        let statics = self.siege_statics(up);
        let known = if statics.is_empty() {
            false
        } else {
            let reach = Self::siege_reach(unit, perch.height);
            let map = self.cb.map();
            let size = (map.width() as f32 * 8.0, map.height() as f32 * 8.0);
            let picks = self.taskforces.siege.sites.sites(unit.pos(), reach, &statics, PERCH_STANDOFF, size);
            picks.iter().any(|&c| {
                in_reach(c, reach, &statics).0 >= min_reach
                    && self.siege_perch_site_ok(c, &perch)
                    && self.siege_fits_by(c, unit, &statics, PERCH_STANDOFF)
            })
        };
        if !known {
            self.log(&format!(
                "taskforce: a {name} fights alone: no perch site known for a {} force yet (unit {})",
                d.name,
                unit.id()
            ));
        }
        known
    }

    /// Where force `sid` would stand: its station as last picked (for the
    /// `wait_army` dial).
    pub(crate) fn siege_station(&self, sid: SquadId) -> Option<Vec3> {
        self.taskforces.siege.forces.get(&sid).and_then(|f| f.station.as_ref()).map(|s| s.pos)
    }

    /// A perched force the wrapper would have called off (`why`): its core
    /// cannot come down, said once per reason.
    pub(crate) fn siege_stranded(&mut self, sid: SquadId, why: &str) {
        let frame = self.frame;
        let Some(ps) = self.taskforces.siege.forces.get_mut(&sid).and_then(|f| f.perch.as_mut()) else { return };
        if ps.stranded.as_deref() == Some(why) {
            return;
        }
        ps.stranded = Some(why.to_owned());
        let (site, up, since) = (ps.site, ps.up, ps.returned.unwrap_or(ps.since));
        let core = (self.squads.get(sid).and_then(|q| q.get_leader(&self.fighters))).map(|l| (self.defs.name_of(l).to_owned(), l.id()));
        self.op_emit(
            "siege-perch",
            json!({"event": "cannot come down", "squad": sid, "site": [site.x, site.z], "reason": why, "up": up, "perched_seconds": (frame - since) / 30}),
        );
        if let Some((name, id)) = core {
            self.log(&format!(
                "taskforce: a {name} cannot come down ({why}) after {} s {} at ({:.0}, {:.0}) (unit {id})",
                (frame - since) / 30,
                if up { "perched" } else { "raising" },
                site.x,
                site.z
            ));
        }
    }

    /// Forget a gun that is gone.
    pub(crate) fn siege_remove(&mut self, id: i32) {
        self.taskforces.siege.guns.remove(&id);
        for f in self.taskforces.siege.forces.values_mut() {
            f.sent.remove(&id);
        }
    }

    /// Forget a force that is gone.
    pub(crate) fn siege_forget(&mut self, sid: SquadId) {
        self.taskforces.siege.forces.remove(&sid);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(x: f32, z: f32, range: f32, worth: f32) -> Static {
        Static { pos: Vec3::xz(x, z), range, raised: range, nominal: range, worth, visible: true }
    }

    #[test]
    fn a_station_stands_off_every_armed_static_but_not_the_unarmed() {
        let statics = [s(1000.0, 1000.0, 300.0, 100.0), s(1300.0, 1000.0, 0.0, 200.0)];
        assert!(stands_off(Vec3::xz(1000.0, 1450.0), &statics, 120.0));
        assert!(!stands_off(Vec3::xz(1000.0, 1400.0), &statics, 120.0));
        // The mex has no range: standing next to it is fine.
        assert!(stands_off(Vec3::xz(1320.0, 1000.0), &[statics[1]], 120.0));
        assert!(stands_off(Vec3::xz(0.0, 0.0), &[], 120.0));
    }

    #[test]
    fn candidates_reach_the_most_worth_beyond_the_defenses() {
        // A turret (range 300) guarding a mex (worth 200) and a factory
        // (worth 600) 800 further on; guns of reach 1090 from (0, 0).
        let statics = [s(2000.0, 2000.0, 300.0, 150.0), s(2100.0, 2000.0, 0.0, 200.0), s(2800.0, 2000.0, 0.0, 600.0)];
        let picks = candidates(Vec3::xz(0.0, 0.0), 1090.0, &statics, 120.0, |x, z| x > 0.0 && z > 0.0);
        assert!(!picks.is_empty());
        for (p, _) in &picks {
            assert!(stands_off(*p, &statics, 120.0), "{p:?} inside the turret's reach");
            assert!(in_reach(*p, 1090.0, &statics).0 > 0);
        }
        // The best reaches everything.
        let (best, _) = picks[0];
        assert_eq!(in_reach(best, 1090.0, &statics).0, 3, "best {best:?}");
        // Our own spot, far away, reaches nothing and is no candidate.
        assert!(picks.iter().all(|(p, _)| distance(*p, Vec3::xz(0.0, 0.0)) > 500.0));
    }

    #[test]
    fn a_static_we_cannot_outrange_walls_off_its_ring() {
        // A Desolator-like static (range 1130) with guns of reach 1090: no
        // candidate about it stands off, so nothing is picked around it.
        let statics = [s(3000.0, 3000.0, 1130.0, 1000.0)];
        let picks = candidates(Vec3::xz(0.0, 0.0), 1090.0, &statics, 120.0, |_, _| true);
        assert!(picks.is_empty());
        // With a mex beyond it, the mex's far side is reachable.
        let statics = [s(3000.0, 3000.0, 1130.0, 1000.0), s(5000.0, 3000.0, 0.0, 200.0)];
        let picks = candidates(Vec3::xz(0.0, 0.0), 1090.0, &statics, 120.0, |_, _| true);
        assert!(picks.iter().all(|(p, _)| distance(*p, Vec3::xz(3000.0, 3000.0)) >= 1250.0));
        assert!(!picks.is_empty());
    }

    #[test]
    fn the_nearest_static_gives_the_standoff() {
        let statics = [s(0.0, 0.0, 500.0, 1.0), s(2000.0, 0.0, 300.0, 1.0), s(1250.0, 0.0, 0.0, 1.0)];
        // The mex next door is unarmed and does not count.
        let (d, n) = nearest(Vec3::xz(1200.0, 0.0), &statics).unwrap();
        assert_eq!((d, n.range), (800.0, 300.0));
        assert!(nearest(Vec3::xz(0.0, 0.0), &[]).is_none());
        assert!(nearest(Vec3::xz(0.0, 0.0), &[statics[2]]).is_none());
    }
}
