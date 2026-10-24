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

use crate::Ai;
use crate::defs::cmd;
use crate::kgbutil::kgb_move_to_exact;
use crate::military::taskforce::{Aim, doctrine::ORBIT, marksman::Mark};
use crate::military::{Enemy, SquadId};
use crate::terrain::reaches;
use crate::util::{NULLPOS, distance, get_directional_point};

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
}

#[derive(Default)]
struct SiegeForce {
    station: Option<Station>,
    /// Guns given spots about the station: when each was last told to go.
    sent: BTreeMap<i32, i32>,
    last_report: i32,
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

#[derive(Default)]
pub struct Siege {
    forces: BTreeMap<SquadId, SiegeForce>,
    guns: BTreeMap<i32, Gun>,
}

/// A known enemy static: where, its ground-firing weapon range (0 for one
/// unarmed, or anti-air only: neither fences the ground), its worth to the
/// guns, and whether it is in sight.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Static {
    pub pos: Vec3,
    pub range: f32,
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
    let mut by_worth: Vec<&Static> = statics.iter().collect();
    by_worth.sort_by(|a, b| b.worth.total_cmp(&a.worth));
    let mut spots: Vec<(f32, f32)> = vec![(from.x, from.z)];
    for s in by_worth.iter().take(STATION_TARGETS) {
        for share in STATION_SHARES {
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

impl Ai {
    /// The enemy statics we know of (identified, and where they were seen:
    /// a building's radar ghost stands where it is).
    fn siege_statics(&self) -> Vec<Static> {
        (self.war.targets.values())
            .filter(|e| e.identified && e.is_static && !e.is_dead && e.position != NULLPOS)
            .map(|e| Static {
                pos: e.position,
                range: if e.can_attack() && !e.is_aa { e.def_max_weapon_range() } else { 0.0 },
                worth: worth(e, e.ud.map_or(e.value, |d| d.cost(self.metal))),
                visible: e.visible,
            })
            .collect()
    }

    /// The guns' reach: the leader's weapon range less a margin.
    fn siege_reach(leader: Unit) -> f32 {
        leader.def().map_or(1150.0, |d| d.max_weapon_range()) - RANGE_MARGIN
    }

    /// Whether a station is still safe to stand at: beyond every known
    /// static's reach, and under no defense on the threat map.
    fn siege_safe(&self, p: Vec3, leader: Unit, statics: &[Static]) -> bool {
        stands_off(p, statics, STANDOFF) && self.war.view(leader.def()).get_porc_threat(p) <= 0.0
    }

    /// Whether the guns can stand at `p`: safe, reachable from where the
    /// leader is, and the straight way there under no known defense.
    fn siege_fits(&self, p: Vec3, leader: Unit, statics: &[Static]) -> bool {
        self.siege_safe(p, leader, statics)
            && leader.def().is_none_or(|d| reaches(self.reach_terrain(), d, leader.pos(), p, AT_STATION))
            && self.siege_way_clear(leader.pos(), p, leader)
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
        let statics = self.siege_statics();
        let reach = Self::siege_reach(leader);
        // A station is kept while it is safe and reaches something: the
        // guns stay until they have shot what they reach (every move costs
        // them their aim).
        let kept = (self.taskforces.siege.forces.get(&sid).and_then(|f| f.station.as_ref()))
            .filter(|s| self.siege_safe(s.pos, leader, &statics))
            .filter(|s| in_reach(s.pos, reach, &statics).0 > 0)
            .map(|s| (s.pos, s.deployed));
        let station = match kept {
            Some((s, deployed)) => return (s, distance(pos, s) > if deployed { PARK_LEAVE } else { PARK }),
            None => {
                let (s, k, v, how) = self.siege_pick(sid, leader, reach, &statics);
                let old = self.taskforces.siege.forces.get(&sid).and_then(|f| f.station.as_ref()).map(|s| s.pos);
                if old.is_none_or(|o| distance(o, s) > AT_STATION) {
                    let near = nearest(s, &statics);
                    let margin = near.map_or(0.0, |(d, n)| d - n.range);
                    let doctrine = self.squads.get(sid).map_or(String::new(), |q| q.shape.doctrine.clone());
                    self.log(&format!(
                        "taskforce: the {doctrine} force (squad {sid}) stations at ({:.0}, {:.0}), {:.0} off, in reach of \
                         {k} statics worth {v:.0}; {how}; the nearest armed static {}",
                        s.x,
                        s.z,
                        distance(pos, s),
                        match near {
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
                    f.station = Some(Station { pos: s, reach: (k, v), deployed: false });
                } else if let Some(st) = f.station.as_mut() {
                    st.reach = (k, v);
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
        statics: &[Static],
    ) -> (Vec3, usize, f32, &'static str) {
        let from = leader.pos();
        let map = self.cb.map();
        let (w, h) = (map.width() as f32 * 8.0, map.height() as f32 * 8.0);
        let on_map = |x: f32, z: f32| x >= 50.0 && z >= 50.0 && x <= w - 50.0 && z <= h - 50.0;
        if !statics.is_empty() {
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
        let statics = self.siege_statics();
        let reach = leader.map_or(1100.0, Self::siege_reach);
        // A gun fires out to its range; the reach (less a margin) is for
        // choosing stations.
        let range = reach + RANGE_MARGIN;
        let shields = self.impalers_shields();
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
                    self.siege_aim(id, unit, range, &shields);
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
            if let Some(st) = f.station.as_mut() {
                st.deployed = false;
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

    /// Where force `sid` would stand: its station as last picked (for the
    /// `wait_army` dial).
    pub(crate) fn siege_station(&self, sid: SquadId) -> Option<Vec3> {
        self.taskforces.siege.forces.get(&sid).and_then(|f| f.station.as_ref()).map(|s| s.pos)
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
        Static { pos: Vec3::xz(x, z), range, worth, visible: true }
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
