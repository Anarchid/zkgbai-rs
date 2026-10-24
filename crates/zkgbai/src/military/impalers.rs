//! Impaler batteries: precision artillery for things that stand still.
//!
//! An Impaler (`vehheavyarty`: 700 metal, 1100 health) fires a kinetic
//! missile every 10 s out to 1500: 800 damage to what it strikes and none
//! around it (`impactOnly`). The missile rises for 2.1 s, then flies to
//! where its target was at launch, about 5 s in all, and does not home. Fired
//! at will (as other artillery is) it spends most shots on mobiles that
//! walk on and on radar dots that are not where they show. So Impalers hold
//! their fire and form batteries, of one Impaler and up, that choose their
//! targets as the missile silo does (see `missiles`):
//!
//! - buildings, where they were seen: the engine gives a building it once
//!   showed us its true place on radar too (ghosts have no radar error),
//!   and we remember it out of radar as well;
//! - mexes never seen but on radar: a contact standing still on a metal
//!   spot is taken for a mex there, and mexes sit on their spots (radar
//!   blurs where a contact is, not how fast it goes);
//! - mobiles in sight that have stood still a while (a constructor at work,
//!   anything left idle), and only when a volley kills them outright.
//!
//! A volley at what it kills outright is worth most, and only guns that
//! reach a target from where they stand fire at it. As many missiles go at
//! a target as kill it, the rest of the volley at the next (as Ravens split
//! their targets), and missiles already flying count, so nothing is
//! overkilled. Nothing under a known shield is a target (shields stop the
//! missile). With nothing in reach, a battery goes to a station within
//! range of the most it would kill, out of the enemy's reach.
//!
//! Batteries take escorts by the `impaler` doctrine (see
//! `military::taskforce`): new Levelers screen them against the raiders
//! that kill guns left alone, and Crashers against air, orbiting a gun.
//! While a battery is short of its screen, Rover factories build Levelers
//! more often. Escorts whose battery is gone join the next one short of
//! them, or the army. `disable=escorts` keeps batteries alone.
//!
//! A gun's reload tells each shot. The engine reports our hits on what our
//! radar or sight covers, so a missile due at a covered target that did
//! not hit missed. One due at a building out of our sight is taken to have
//! hit; once those add up to its health it is left alone until seen again.

use std::collections::{BTreeMap, BTreeSet};

use springai::{NO_TIMEOUT, Unit, UnitDef, Vec3, WeaponDef};

use crate::Ai;
use crate::defs::cmd;
use crate::military::taskforce::Doctrine;
use crate::military::{Enemy, Fighter};
use crate::nav;
use crate::terrain::reaches;
use crate::util::{NULLPOS, OPTION_SHIFT_KEY, distance, get_directional_point};

pub const IMPALER: &str = "vehheavyarty";

/// Frames between updates.
const PERIOD: i32 = 15;

/// A missile's damage, and the guns' range (the 2D range grows by this
/// share of how far the gun stands above its target), less a margin.
const DAMAGE: f32 = 800.0;
const RANGE: f32 = 1500.0;
const HEIGHT_MOD: f32 = 0.2;
const RANGE_MARGIN: f32 = 40.0;

/// A missile's flight: it rises for `UPTIME` frames, gaining `ACCEL`
/// elmos/frame each frame, then flies at its target gaining speed the same
/// way (its top speed is never reached at these ranges); and `TURN` more,
/// measured (it turns from going up, and leaves the launcher late).
const UPTIME: f32 = 63.0;
const ACCEL: f32 = 280.0 / 900.0;
const TURN: f32 = 32.0;

/// After a missile is due, how long its hit may take to be heard of.
const HIT_GRACE: i32 = 45;

/// An order to fire not carried out in this long is dropped.
const ORDER_TIMEOUT: i32 = 30 * 8;

/// Guns reloaded within this long join a volley at a building (they fire
/// as they reload; a building does not walk out of the way).
const SYNC: i32 = 30 * 3;

/// A mobile in sight that has stood within `STILL_SLOP` of one place this
/// long is a target, while it stands (`STILL_SPEED` elmos a frame), if it
/// is worth `STILL_WORTH` a missile and is not a raider: against CircuitAI,
/// raiders that stood 6 s walked off from 12 of 27 missiles, at 65-90
/// metal each.
const STILL_FOR: i32 = 30 * 10;
const STILL_SLOP: f32 = 8.0;
const STILL_SPEED: f32 = 0.05;
const STILL_WORTH: f32 = 120.0;

/// How much more a volley is worth that kills outright.
const KILL_BONUS: f32 = 4.0;

/// Worth beyond metal: a mex's or an energy building's (its income goes
/// with it), a defense's, a radar's or jammer's (the enemy's eyes), and a
/// constructor's (its build power).
const ECONOMY: f32 = 2.5;
const PORC: f32 = 1.5;
const EYES: f32 = 3.0;
const BUILDER: f32 = 2.0;

/// A battery has at most this many guns.
const BATTERY_MAX: usize = 6;

/// Stations stand this far from the targets they are for, in this many
/// directions; and must be clear of the enemy's reach there and this far
/// around.
const STATION_RADII: [f32; 2] = [1350.0, 1150.0];
const STATION_DIRS: usize = 16;
const SAFE_RING: f32 = 150.0;

/// Stations trade worth for exposure on their Pareto front: of the
/// stations worth at least `1 - EPS` of the most a safe one is worth, the
/// battery takes the one that leaves it out of our forces' cover least
/// over the next `HORIZON` seconds, walking there and standing there (the
/// `impaler_eps` option sets it). Guns die to raiders that find them
/// alone, on the way as much as at their stations: standing off farthest
/// from the enemy's known reach instead lost more and killed less (eps
/// 0.25/0.5/0.75 lost 2.3/2.5/3.0 guns per 10 gun-minutes against 1.9 at
/// 0, and killed 174/136/137 metal a gun-minute against 186, vs CircuitAI).
/// The way is looked at every `EXPOSURE_STEP` elmos.
const EPS: f32 = 0.25;
const HORIZON: f32 = 120.0;
const EXPOSURE_STEP: f32 = 100.0;

/// Places this far behind our squads (toward our base) are stations too.
const BEHIND_SQUAD: f32 = 200.0;

/// A battery leaves its station once our cover there is under this share
/// of what it was when picked.
const COVER_KEPT: f32 = 0.5;

/// Stations are looked for among the targets worth most per shot, this
/// many; a battery keeps its station this long at least, unless it is
/// threatened or has nothing in reach.
const STATION_TARGETS: usize = 16;
const RESTATION: i32 = 30 * 10;

/// Guns farther than this from their station are walked to it, with fresh
/// orders this often.
const AT_STATION: f32 = 120.0;
const MOVE_EVERY: i32 = 30 * 3;

/// Damage done to an enemy out of our sight is forgotten after this long
/// (it is repaired); a building taken as killed unseen is left alone at
/// most this long.
const DEALT_MEMORY: i32 = 30 * 90;
const GONE_MEMORY: i32 = 30 * 300;

/// A hit this recent tells what the next missile does to its target.
const HIT_TELLS: i32 = 30 * 10;

/// A radar contact never seen is taken for a mex on a metal spot once it
/// has stood still this long with where radar shows it, on average, this
/// near the spot, and nearer by `SPOT_CLEAR` than to any other spot (radar
/// shows a contact up to ~135 elmos off, wandering).
const SPOT_FOR: i32 = 30 * 20;
const SPOT_SNAP: f32 = 60.0;
const SPOT_CLEAR: f32 = 60.0;
const MEX: &str = "staticmex";

/// Mobile shields seen this lately still cover where they were seen.
const SHIELD_SEEN: i32 = 30 * 20;

/// Frames between reports.
const REPORT_FRAMES: i32 = 30 * 60;

/// A lost gun's log tells the damage it took this long before.
const HURT_MEMORY: i32 = 30 * 20;

/// CMD_ATTACK: on a place, a gun fires at that place.
const CMD_ATTACK: i32 = 20;

/// An order to fire: at which enemy, where, since when, and the gun's
/// reload frame when it was given (it changes when the gun fires).
struct Order {
    target: i32,
    at: Vec3,
    since: i32,
    reload: i32,
}

struct Gun {
    battery: u32,
    order: Option<Order>,
    last_move: i32,
    /// When it was made and last fired, the damage it took lately (when,
    /// how much, by what weapon) and what it was doing when that began.
    born: i32,
    fired: Option<i32>,
    hurt: Vec<(i32, f32, String)>,
    hurt_while: String,
}

/// An escort: its battery, its role in the doctrine, and the gun it was
/// told to orbit (none since it retreated, or not told yet).
struct Escort {
    battery: u32,
    role: usize,
    orbits: Option<i32>,
}

struct Battery {
    guns: Vec<i32>,
    /// Where it stands to fire, since when, and our cover there then.
    station: Option<Vec3>,
    since: i32,
    cover: f32,
}

/// A missile in flight: from which gun, at which enemy and where, when
/// fired and due, and whether it hit.
struct Shot {
    gun: i32,
    target: i32,
    at: Vec3,
    fired: i32,
    due: i32,
    hit: bool,
}

#[derive(Default)]
struct Stats {
    shots: u32,
    hits: u32,
    misses: u32,
    unseen: u32,
    /// Missiles due at what died before they came.
    late: u32,
    strays: u32,
    kills: u32,
    killed_metal: f32,
    /// Measured flight, and measured minus predicted, frames, summed over
    /// the hits on what they were fired at.
    flight: i64,
    flight_error: i64,
}

/// A station picked: where, its worth, exposure and our cover there; the
/// most worth a station had and the exposure there; and how many were
/// worth enough.
struct Station {
    pos: Vec3,
    value: f32,
    exposure: f32,
    cover: f32,
    best: (f32, f32),
    front: usize,
}

/// A target: an enemy, where, whether it moves, its health left once the
/// missiles flying at it and ordered land, what a missile does to it, and
/// its worth.
#[derive(Clone)]
struct Target {
    id: i32,
    pos: Vec3,
    mobile: bool,
    left: f32,
    per_shot: f32,
    worth: f32,
}

impl Target {
    fn shots(&self) -> usize {
        (self.left / self.per_shot).ceil().max(1.0) as usize
    }

    /// Worth per missile, more if `guns` kill it outright.
    fn score(&self, guns: usize) -> f32 {
        let k = self.shots();
        self.worth / k as f32 * if guns >= k { KILL_BONUS } else { 1.0 }
    }
}

#[derive(Default)]
pub struct Impalers {
    guns: BTreeMap<i32, Gun>,
    batteries: BTreeMap<u32, Battery>,
    next_battery: u32,
    shots: Vec<Shot>,
    /// Damage our missiles did to enemies (or are taken to have done) out
    /// of our sight: when, how much.
    dealt: BTreeMap<i32, Vec<(i32, f32)>>,
    /// Last frame each enemy was in our sight.
    sighted: BTreeMap<i32, i32>,
    /// Mobiles in sight: where each has stood, since when.
    still: BTreeMap<i32, (Vec3, i32)>,
    /// Buildings taken as killed out of our sight, since when.
    gone: BTreeMap<i32, i32>,
    /// Radar contacts never seen, standing still: the sum of where radar
    /// showed them, how many times, since when.
    contacts: BTreeMap<i32, (f32, f32, u32, i32)>,
    /// Contacts shot at as a mex on a spot (by spot index) and missed.
    not_mexes: BTreeSet<(i32, usize)>,
    /// The last hit on each enemy: when, how much.
    last_hit: BTreeMap<i32, (i32, f32)>,
    /// Shield radius by enemy unit def id (0 without one).
    shield_radius: BTreeMap<i32, f32>,
    /// Worth given up for safety (see [`EPS`]).
    eps: Option<f32>,
    /// The escorts batteries have (see [`Ai::impalers_doctrine`]).
    escorts: BTreeMap<i32, Escort>,
    stats: Stats,
    last_report: i32,
}

/// Frames a missile takes to a target `d` away (2D), the gun standing `dy`
/// above it.
fn flight_frames(d: f32, dy: f32) -> i32 {
    let (v0, rise) = (ACCEL * UPTIME, 0.5 * ACCEL * UPTIME * UPTIME);
    let s = d.hypot(rise + dy);
    let t = (-v0 + (v0 * v0 + 2.0 * ACCEL * s).sqrt()) / ACCEL;
    (UPTIME + t + TURN).ceil() as i32
}

/// Whether a gun at `from` reaches a target at `to`.
fn in_range(from: Vec3, to: Vec3) -> bool {
    distance(from, to) <= RANGE + HEIGHT_MOD * (from.y - to.y) - RANGE_MARGIN
}

/// What killing an enemy is worth, in metal.
fn worth(e: &Enemy, cost: f32) -> f32 {
    let built = if e.visible && e.unit.is_being_built() { e.unit.build_progress().max(0.1) } else { 1.0 };
    let name = e.def_name.as_str();
    let kind = if e.is_mex || name.starts_with("energy") {
        ECONOMY
    } else if matches!(name, "staticradar" | "staticheavyradar" | "staticjammer") {
        EYES
    } else if e.is_porc {
        PORC
    } else if e.is_worker && !e.is_com {
        BUILDER
    } else {
        1.0
    };
    cost * built * kind
}

impl Ai {
    /// A new Impaler: ours to command, unless `disable=impalers` leaves it
    /// to the artillery handler (it is counted either way).
    pub fn impalers_claims(&mut self, unit: Unit, cost: f32) -> bool {
        let p = unit.pos();
        self.log(&format!("impalers: an Impaler is built at ({:.0}, {:.0})", p.x, p.z));
        if self.dev_off("impalers") {
            return false;
        }
        if self.impalers.eps.is_none() {
            let eps = self.cb.option("impaler_eps").and_then(|v| v.parse::<f32>().ok()).unwrap_or(EPS);
            self.impalers.eps = Some(eps.clamp(0.0, 1.0));
        }
        self.taskforce_load();
        self.impalers_add(unit, cost);
        true
    }

    /// A new Impaler: it holds its fire and its ground, and joins the
    /// nearest battery with room, or makes one.
    fn impalers_add(&mut self, unit: Unit, cost: f32) {
        unit.set_fire_state(0, 0);
        unit.set_move_state(0, 0);
        self.retreat_add_coward(unit);
        let f = Fighter::new(unit, cost, self.team_id);
        self.fighters.insert(f.id, f);
        let pos = unit.pos();
        let near = (self.impalers.batteries.iter())
            .filter(|(_, b)| b.guns.len() < BATTERY_MAX)
            .map(|(&bid, b)| (bid, distance(self.impalers_battery_pos(b), pos)))
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(bid, _)| bid);
        let h = &mut self.impalers;
        let bid = near.unwrap_or_else(|| {
            h.next_battery += 1;
            h.batteries.insert(h.next_battery, Battery { guns: Vec::new(), station: None, since: 0, cover: 0.0 });
            h.next_battery
        });
        if let Some(b) = h.batteries.get_mut(&bid) {
            b.guns.push(unit.id());
        }
        let born = self.frame;
        let g = Gun {
            battery: bid,
            order: None,
            last_move: 0,
            born,
            fired: None,
            hurt: Vec::new(),
            hurt_while: String::new(),
        };
        h.guns.insert(unit.id(), g);
    }

    /// One of our guns took damage: remember by what, for the log if it
    /// dies.
    pub fn impalers_unit_damaged(
        &mut self,
        unit: Unit,
        attacker: Option<Unit>,
        damage: f32,
        weapon: Option<WeaponDef>,
    ) {
        let frame = self.frame;
        let Some(state) = self.impalers_gun_state(unit) else { return };
        let Some(g) = self.impalers.guns.get_mut(&unit.id()) else { return };
        let by = weapon
            .map(|w| w.name())
            .or_else(|| attacker.and_then(|a| a.def()).map(|d| d.name()))
            .unwrap_or_else(|| "?".to_owned());
        g.hurt.retain(|x| frame - x.0 < HURT_MEMORY);
        if g.hurt.is_empty() {
            g.hurt_while = state;
        }
        g.hurt.push((frame, damage, by));
    }

    /// What one of our guns is doing: retreating, firing, walking to its
    /// station or at it (and our cover where it is).
    fn impalers_gun_state(&self, unit: Unit) -> Option<String> {
        let id = unit.id();
        let g = self.impalers.guns.get(&id)?;
        let p = unit.pos();
        let off = self.impalers.batteries.get(&g.battery).and_then(|b| b.station).map(|s| distance(s, p));
        let state = if self.retreat_handler.is_retreating(id) {
            "retreating".to_owned()
        } else if g.order.is_some() {
            "firing".to_owned()
        } else {
            match off {
                Some(d) if d <= AT_STATION => "at its station".to_owned(),
                Some(d) => format!("walking {d:.0} from its station"),
                None => "without a station".to_owned(),
            }
        };
        Some(format!("{state} (cover {:.0})", self.war.get_total_friendly_power(p)))
    }

    /// Before the retreat handler forgets it: a lost gun's log tells what
    /// it was doing (retreating, walking to its station, at it), how long
    /// it lived and since it fired, and what hit it lately.
    pub fn impalers_remove_unit(&mut self, unit: Unit) {
        let id = unit.id();
        if self.defs.name_of(unit) == IMPALER {
            let (p, frame) = (unit.pos(), self.frame);
            let state = self.impalers_gun_state(unit);
            let line = match (self.impalers.guns.get(&id), state) {
                (Some(g), Some(state)) => {
                    let b = self.impalers.batteries.get(&g.battery);
                    let mut by: BTreeMap<&str, f32> = BTreeMap::new();
                    for (f, d, w) in &g.hurt {
                        if frame - f < HURT_MEMORY {
                            *by.entry(w.as_str()).or_default() += d;
                        }
                    }
                    let mut by: Vec<(&str, f32)> = by.into_iter().collect();
                    by.sort_by(|a, b| b.1.total_cmp(&a.1));
                    let by: Vec<String> = by.iter().map(|(w, d)| format!("{w} {d:.0}")).collect();
                    let first = g.hurt.first().map_or(0, |x| frame - x.0) / 30;
                    format!(
                        ": {state}; first hit {first} s ago while {}; cover {:.0} when stationed; {} s old, fired {}; \
                         hurt by {}",
                        if g.hurt.is_empty() { "unhurt" } else { g.hurt_while.as_str() },
                        b.map_or(0.0, |b| b.cover),
                        (frame - g.born) / 30,
                        g.fired.map_or("never".to_owned(), |f| format!("{} s ago", (frame - f) / 30)),
                        if by.is_empty() { "nothing seen".to_owned() } else { by.join(", ") }
                    )
                }
                _ => String::new(),
            };
            self.log(&format!("impalers: lost an Impaler at ({:.0}, {:.0}){line}", p.x, p.z));
        }
        if self.impalers.escorts.remove(&id).is_some() {
            let cost = self.fighters.remove(&id).map_or(0.0, |f| f.metal_value);
            let name = self.defs.name_of(unit).to_owned();
            self.log(&format!("impalers: lost an escort, a {name} ({cost:.0} metal)"));
            return;
        }
        let h = &mut self.impalers;
        let Some(gun) = h.guns.remove(&id) else { return };
        if let Some(b) = h.batteries.get_mut(&gun.battery) {
            b.guns.retain(|&g| g != id);
            if b.guns.is_empty() {
                h.batteries.remove(&gun.battery);
            }
        }
        self.fighters.remove(&id);
    }

    /// Our missile hit `enemy`.
    pub fn impalers_enemy_damaged(&mut self, enemy: Unit, attacker: Option<Unit>, damage: f32) {
        let Some(gun) = attacker.map(|a| a.id()).filter(|a| self.impalers.guns.contains_key(a)) else { return };
        let frame = self.frame;
        let h = &mut self.impalers;
        if let Some(s) = h.shots.iter_mut().find(|s| s.gun == gun && !s.hit) {
            s.hit = true;
            h.stats.hits += 1;
            if s.target == enemy.id() {
                h.stats.flight += i64::from(frame - s.fired);
                h.stats.flight_error += i64::from(frame - s.due);
            } else {
                h.stats.strays += 1;
            }
        }
        h.dealt.entry(enemy.id()).or_default().push((frame, damage));
        h.last_hit.insert(enemy.id(), (frame, damage));
    }

    pub fn impalers_enemy_destroyed(&mut self, enemy: Unit, attacker: Option<Unit>) {
        let id = enemy.id();
        self.impalers.dealt.remove(&id);
        self.impalers.last_hit.remove(&id);
        self.impalers.still.remove(&id);
        self.impalers.gone.remove(&id);
        if attacker.is_some_and(|a| self.impalers.escorts.contains_key(&a.id())) {
            let e = self.war.targets.get(&id);
            let cost = e.and_then(|e| e.ud).map_or(0.0, |d| d.cost(self.metal));
            let name = e.map_or("?", |e| e.def_name.as_str()).to_owned();
            self.log(&format!("impalers: an escort killed a {name} ({cost:.0} metal)"));
        }
        // (Any Impaler of ours, in a battery or not: see `disable=impalers`.)
        if attacker.is_some_and(|a| a.team() == self.team_id && self.defs.name_of(a) == IMPALER) {
            let e = self.war.targets.get(&id);
            let cost = e.and_then(|e| e.ud).map_or(0.0, |d| d.cost(self.metal));
            let name = e.map_or("?", |e| e.def_name.as_str()).to_owned();
            self.impalers.stats.kills += 1;
            self.impalers.stats.killed_metal += cost;
            let p = enemy.pos();
            self.log(&format!("impalers: killed a {name} ({cost:.0} metal) at ({:.0}, {:.0})", p.x, p.z));
        }
    }

    pub fn impalers_update(&mut self, frame: i32) {
        if (frame + self.offset + 7) % PERIOD != 0 {
            return;
        }
        if self.impalers.guns.is_empty() && self.impalers.shots.is_empty() && self.impalers.escorts.is_empty() {
            return;
        }
        self.impalers_watch(frame);
        self.impalers_settle_shots(frame);
        self.impalers_follow_orders(frame);
        let mut targets = self.impalers_targets();
        let bids: Vec<u32> = self.impalers.batteries.keys().copied().collect();
        for bid in bids {
            self.impalers_volley(bid, &mut targets);
            self.impalers_station(bid, &targets, frame);
        }
        self.impalers_escort();
        self.impalers_report(frame, &targets);
    }

    /// The doctrine batteries take escorts by: `impaler`, unless switched
    /// off (or `disable=escorts`).
    fn impalers_doctrine(&self) -> Option<&Doctrine> {
        if self.dev_off("escorts") { None } else { self.taskforces.doctrine("impaler") }
    }

    /// How many escorts of role `role` battery `bid` lacks, by the role's
    /// want of what the battery has.
    fn impalers_short(&self, bid: u32, role: usize) -> usize {
        let h = &self.impalers;
        let Some(d) = self.impalers_doctrine() else { return 0 };
        let Some((b, r)) = h.batteries.get(&bid).zip(d.roles.get(role)) else { return 0 };
        let mine = h.escorts.iter().filter(|(_, e)| e.battery == bid).map(|(id, _)| id);
        let units = b.guns.iter().chain(mine).filter_map(|id| self.fighters.get(id));
        let force = d.force(units.map(|f| (self.defs.name_of(f.unit), f.metal_value)), &self.taskforces.classes);
        let have = force.roles.get(role).map_or(0, |x| x.1);
        r.want(&force).map_or(1, |w| w.saturating_sub(have))
    }

    /// The battery most short of role `role`, the nearest to `p` of those.
    fn impalers_short_battery(&self, role: usize, p: Vec3) -> Option<u32> {
        (self.impalers.batteries.iter())
            .map(|(&bid, b)| (bid, self.impalers_short(bid, role), distance(self.impalers_battery_pos(b), p)))
            .filter(|x| x.1 > 0)
            .max_by(|a, b| a.1.cmp(&b.1).then(b.2.total_cmp(&a.2)))
            .map(|x| x.0)
    }

    /// A new unit: an escort of the battery most short of its role, if any
    /// is. It retreats when hurt, as the army does.
    pub fn impalers_claims_escort(&mut self, unit: Unit, name: &str, cost: f32) -> bool {
        let classes = &self.taskforces.classes;
        let Some((role, r)) = self.impalers_doctrine().and_then(|d| d.role_of(name, classes).map(|i| (i, &d.roles[i])))
        else {
            return false;
        };
        let (fire_state, radar) = (r.fire_state, r.fire_at_radar);
        let Some(bid) = self.impalers_short_battery(role, unit.pos()) else { return false };
        unit.set_move_state(1, 0);
        if let Some(s) = fire_state {
            unit.set_fire_state(s, 0);
        }
        if let Some(r) = radar {
            unit.execute_custom_command(cmd::DONT_FIRE_AT_RADAR, &[if r { 0.0 } else { 1.0 }], 0, NO_TIMEOUT);
        }
        let f = Fighter::new(unit, cost, self.team_id);
        self.fighters.insert(f.id, f);
        self.impalers.escorts.insert(unit.id(), Escort { battery: bid, role, orbits: None });
        let n = self.impalers.batteries.get(&bid).map_or(0, |b| b.guns.len());
        let r = self.impalers_doctrine().and_then(|d| d.roles.get(role)).map_or("?", |r| r.name.as_str());
        let short = self.impalers_short(bid, role);
        self.log(&format!("impalers: a {name} joins a battery of {n} as its {r} ({short} short now)"));
        true
    }

    /// The chance a factory pick that could build `name` builds it: a
    /// role's `produce` while some battery is short of it.
    pub fn impalers_escort_produce(&self, name: &str) -> f32 {
        let Some(d) = self.impalers_doctrine() else { return 0.0 };
        let Some(role) = d.role_of(name, &self.taskforces.classes) else { return 0.0 };
        let short = self.impalers.batteries.keys().any(|&bid| self.impalers_short(bid, role) > 0);
        if short { d.roles[role].produce } else { 0.0 }
    }

    /// Keep the escorts orbiting a gun of their battery (one not
    /// retreating, if any is), unless they retreat; those whose battery is
    /// gone join the next short of them, or the army.
    fn impalers_escort(&mut self) {
        let ids: Vec<i32> = self.impalers.escorts.keys().copied().collect();
        for id in ids {
            let Some(unit) = self.fighters.get(&id).map(|f| f.unit) else {
                self.impalers.escorts.remove(&id);
                continue;
            };
            if self.retreat_handler.is_retreating(id) {
                if let Some(e) = self.impalers.escorts.get_mut(&id) {
                    e.orbits = None;
                }
                continue;
            }
            let Some(&Escort { battery, role, orbits }) = self.impalers.escorts.get(&id) else { continue };
            if !self.impalers.batteries.contains_key(&battery) {
                match self.impalers_short_battery(role, unit.pos()) {
                    Some(bid) => {
                        if let Some(e) = self.impalers.escorts.get_mut(&id) {
                            (e.battery, e.orbits) = (bid, None);
                        }
                    }
                    None => {
                        self.impalers_release_escort(id, unit);
                        continue;
                    }
                }
            }
            let Some(b) = self.impalers.escorts.get(&id).and_then(|e| self.impalers.batteries.get(&e.battery)) else {
                continue;
            };
            let calm = |g: &i32| !self.retreat_handler.is_retreating(*g);
            let leader = match orbits {
                Some(g) if b.guns.contains(&g) && calm(&g) => g,
                _ => match b.guns.iter().find(|g| calm(g)).or(b.guns.first()) {
                    Some(&g) => g,
                    None => continue,
                },
            };
            if orbits == Some(leader) {
                continue;
            }
            let radius = self.impalers_doctrine().and_then(|d| d.roles.get(role)).map_or(120.0, |r| r.orbit);
            unit.execute_custom_command(cmd::ORBIT, &[leader as f32, radius], 0, NO_TIMEOUT);
            unit.execute_custom_command(cmd::ORBIT_DRAW, &[leader as f32], OPTION_SHIFT_KEY, NO_TIMEOUT);
            if let Some(e) = self.impalers.escorts.get_mut(&id) {
                e.orbits = Some(leader);
            }
        }
    }

    /// An escort no battery wants goes to the army, as a new unit does.
    fn impalers_release_escort(&mut self, id: i32, unit: Unit) {
        self.impalers.escorts.remove(&id);
        self.fighters.remove(&id);
        unit.stop(0);
        let name = self.defs.name_of(unit).to_owned();
        self.log(&format!("impalers: a {name} escort goes back to the army"));
        self.war.new_units.insert(id);
        self.war_unit_idle(unit);
    }

    /// Mean position of a battery's guns.
    fn impalers_battery_pos(&self, b: &Battery) -> Vec3 {
        let ps: Vec<Vec3> = b.guns.iter().filter_map(|id| self.fighters.get(id)).map(|f| f.get_pos()).collect();
        if ps.is_empty() {
            return NULLPOS;
        }
        let n = ps.len() as f32;
        let (x, z) = ps.iter().fold((0.0, 0.0), |(x, z), p| (x + p.x / n, z + p.z / n));
        Vec3::new(x, self.cb.map().elevation_at(x, z), z)
    }

    /// Keep track of what is in sight, and how long mobiles in sight have
    /// stood where they are.
    fn impalers_watch(&mut self, frame: i32) {
        let h = &mut self.impalers;
        for (&id, e) in &self.war.targets {
            if !e.identified && !e.is_dead && e.is_radar_visible {
                let v = e.unit.vel();
                if v.x.hypot(v.z) < STILL_SPEED {
                    let c = h.contacts.entry(id).or_insert((0.0, 0.0, 0, frame));
                    *c = (c.0 + e.position.x, c.1 + e.position.z, c.2 + 1, c.3);
                } else {
                    h.contacts.remove(&id);
                }
            }
            if !e.visible || e.is_dead {
                h.still.remove(&id);
                continue;
            }
            h.contacts.remove(&id);
            // (Reading its health keeps it for when it is out of sight.)
            e.get_health();
            h.sighted.insert(id, frame);
            h.dealt.remove(&id);
            h.gone.remove(&id);
            if !e.is_static {
                let s = h.still.entry(id).or_insert((e.position, frame));
                if distance(s.0, e.position) > STILL_SLOP {
                    *s = (e.position, frame);
                }
            }
        }
        let targets = &self.war.targets;
        h.sighted.retain(|id, _| targets.contains_key(id));
        h.still.retain(|id, _| targets.contains_key(id));
        h.contacts.retain(|id, _| targets.get(id).is_some_and(|e| !e.identified && !e.is_dead));
        h.not_mexes.retain(|(id, _)| targets.contains_key(id));
        h.gone.retain(|id, t| targets.contains_key(id) && frame - *t < GONE_MEMORY);
        h.last_hit.retain(|_, x| frame - x.0 < HIT_TELLS);
        h.dealt.retain(|id, d| {
            d.retain(|x| frame - x.0 < DEALT_MEMORY);
            targets.contains_key(id) && !d.is_empty()
        });
    }

    /// Settle the missiles due: those that hit are done; one at a covered
    /// target that did not hit missed; one at a target out of our sight
    /// and radar is taken to have hit, and a building it would have killed
    /// counts as gone until seen again.
    fn impalers_settle_shots(&mut self, frame: i32) {
        let (due, flying): (Vec<Shot>, Vec<Shot>) =
            std::mem::take(&mut self.impalers.shots).into_iter().partition(|s| frame > s.due + HIT_GRACE);
        self.impalers.shots = flying;
        for s in due.into_iter().filter(|s| !s.hit) {
            let Some(e) = self.war.targets.get(&s.target).filter(|e| !e.is_dead) else {
                self.impalers.stats.late += 1;
                continue;
            };
            if e.visible || e.is_radar_visible {
                self.impalers.stats.misses += 1;
                if !e.identified
                    && let Some(spot) = self.graph.get_closest_spot_index(s.at)
                {
                    self.impalers.not_mexes.insert((s.target, spot));
                    let p = s.at;
                    self.log(&format!("impalers: no mex at ({:.0}, {:.0}) after all", p.x, p.z));
                }
                continue;
            }
            self.impalers.stats.unseen += 1;
            let done = self.impalers_per_shot(e);
            self.impalers.dealt.entry(s.target).or_default().push((s.due, done));
            if e.is_static && self.impalers_health(e) <= 0.0 && !self.impalers.gone.contains_key(&s.target) {
                let (name, p) = (e.def_name.clone(), e.position);
                self.impalers.gone.insert(s.target, frame);
                self.log(&format!("impalers: the {name} at ({:.0}, {:.0}) counts as killed, unseen", p.x, p.z));
            }
        }
    }

    /// Health an enemy has: as seen, or as last seen less our missiles'
    /// damage since.
    fn impalers_health(&self, e: &Enemy) -> f32 {
        let health = if e.visible && e.unit.health() > 0.0 { e.unit.health() } else { e.get_health() * e.def_health() };
        if e.visible {
            return health;
        }
        let since = self.impalers.sighted.get(&e.unit_id).copied().unwrap_or(0);
        let dealt: f32 =
            self.impalers.dealt.get(&e.unit_id).map_or(0.0, |d| d.iter().filter(|x| x.0 >= since).map(|x| x.1).sum());
        health - dealt
    }

    /// What a missile does to `e`: what the last hit on it did, if lately;
    /// else a third (its `damageModifier`) to what armors itself closed or
    /// idle: a turret none of ours is within its range of (ZK's Gauss,
    /// Faraday, Desolator, Razor, Lucifer open to aim, and close a while
    /// after), a Halberd, Crab or amphibious constructor standing still, and
    /// a Solar once it is hurt (it closes when hit).
    fn impalers_per_shot(&self, e: &Enemy) -> f32 {
        if let Some(&(_, d)) = self.impalers.last_hit.get(&e.unit_id) {
            return d;
        }
        let m = e.ud.map_or(1.0, |d| d.armored_multiple());
        let open = if m >= 1.0 {
            true
        } else if e.def_name == "energysolar" {
            e.get_health() >= 1.0
        } else if e.is_static {
            let near = self.cb.friendly_units_in(e.position, e.def_max_weapon_range() + 50.0);
            near.iter().any(|u| u.def().is_some_and(|d| !d.is_able_to_fly() || e.is_aa))
        } else {
            false
        };
        if open { DAMAGE } else { DAMAGE * m }
    }

    /// Watch the orders given: a gun whose reload restarted fired (its
    /// missile is on the way); orders that are stale or at a target no
    /// longer one are dropped.
    fn impalers_follow_orders(&mut self, frame: i32) {
        let ids: Vec<i32> = self.impalers.guns.keys().copied().collect();
        for id in ids {
            let Some(unit) = self.fighters.get(&id).map(|f| f.unit) else { continue };
            let Some(order) = self.impalers.guns.get(&id).and_then(|g| g.order.as_ref()) else { continue };
            let (target, at, since, reload) = (order.target, order.at, order.since, order.reload);
            let weapon = unit.weapon(0);
            let rf = weapon.as_ref().map_or(0, |w| w.reload_frame());
            let retreating = self.retreat_handler.is_retreating(id);
            if rf != reload && rf > frame {
                let fired = (rf - weapon.as_ref().map_or(0, |w| w.reload_time())).min(frame);
                let p = unit.pos();
                let due = fired + flight_frames(distance(p, at), p.y - at.y);
                self.impalers.shots.push(Shot { gun: id, target, at, fired, due, hit: false });
                self.impalers.stats.shots += 1;
                if let Some(g) = self.impalers.guns.get_mut(&id) {
                    g.fired = Some(fired);
                }
            } else {
                let e = self.war.targets.get(&target).filter(|e| !e.is_dead);
                let still = e.is_some_and(|e| {
                    e.is_static || self.impalers.contacts.contains_key(&e.unit_id) || self.impalers_still(e, frame)
                });
                if frame - since <= ORDER_TIMEOUT && still && !retreating {
                    continue;
                }
            }
            // (A place to fire at stays an order until stopped; a retreat's
            // orders have replaced it already.)
            if !retreating {
                unit.stop(0);
            }
            if let Some(g) = self.impalers.guns.get_mut(&id) {
                g.order = None;
                g.last_move = 0;
            }
        }
    }

    /// Whether a mobile in sight has stood still long enough to be shot.
    fn impalers_still(&self, e: &Enemy, frame: i32) -> bool {
        let v = e.unit.vel();
        e.visible
            && v.x.hypot(v.z) < STILL_SPEED
            && self.impalers.still.get(&e.unit_id).is_some_and(|s| frame - s.1 >= STILL_FOR)
    }

    /// Known shields: where, how far they reach.
    fn impalers_shields(&mut self) -> Vec<(Vec3, f32)> {
        self.impalers_shield_sources().into_iter().map(|s| (s.0, s.1)).collect()
    }

    /// Known shields: where, how far they reach, whose.
    fn impalers_shield_sources(&mut self) -> Vec<(Vec3, f32, String)> {
        let frame = self.frame;
        let mut shields = Vec::new();
        for e in self.war.targets.values() {
            let Some(ud) = e.ud.filter(|_| !e.is_dead && e.position != NULLPOS) else { continue };
            if !e.is_static && frame - e.last_seen > SHIELD_SEEN {
                continue;
            }
            let r = *self.impalers.shield_radius.entry(ud.id()).or_insert_with(|| shield_radius(ud));
            if r > 0.0 {
                shields.push((e.position, r, e.def_name.clone()));
            }
        }
        shields
    }

    /// What our guns may fire at, worth most per missile first: buildings,
    /// and mobiles in sight standing still; out of shields, and alive by
    /// what we know once the missiles flying and ordered land.
    fn impalers_targets(&mut self) -> Vec<Target> {
        let frame = self.frame;
        let shields = self.impalers_shields();
        let Some(def) = self.defs.get(IMPALER) else { return Vec::new() };
        let reach = self.war.reach(Some(def));
        let h = &self.impalers;
        // Missiles flying and ordered, by target.
        let mut pending: BTreeMap<i32, f32> = BTreeMap::new();
        for s in h.shots.iter().filter(|s| !s.hit) {
            *pending.entry(s.target).or_default() += 1.0;
        }
        for o in h.guns.values().filter_map(|g| g.order.as_ref()) {
            *pending.entry(o.target).or_default() += 1.0;
        }
        let mut targets: Vec<Target> = (self.war.targets.values())
            .filter(|e| e.identified && !e.is_dead && !e.can_fly() && e.position != NULLPOS && e.def_health() > 0.0)
            .filter(|e| e.def_name != "wolverine_mine" && self.war.can_hit(reach, e))
            .filter(|e| {
                if e.is_static {
                    !h.gone.contains_key(&e.unit_id)
                } else {
                    !e.is_raider && self.impalers_still(e, frame)
                }
            })
            .filter(|e| !shields.iter().any(|&(p, r)| distance(p, e.position) <= r + 10.0))
            .filter_map(|e| {
                let per_shot = self.impalers_per_shot(e);
                let left = self.impalers_health(e) - per_shot * pending.get(&e.unit_id).copied().unwrap_or(0.0);
                let cost = e.ud.map_or(e.value, |d| d.cost(self.metal));
                let t = Target {
                    id: e.unit_id,
                    pos: e.position,
                    mobile: !e.is_static,
                    left,
                    per_shot,
                    worth: worth(e, cost),
                };
                (left > 0.0 && (!t.mobile || t.worth / t.shots() as f32 >= STILL_WORTH)).then_some(t)
            })
            .collect();
        targets.extend(self.impalers_radar_mexes(&pending, &shields));
        targets.sort_by(|a, b| b.score(0).total_cmp(&a.score(0)));
        targets
    }

    /// Radar contacts never seen taken for mexes on metal spots (see
    /// [`SPOT_FOR`]), at the spot: but on our spots, where a known building
    /// stands, under a shield, or where we missed one already.
    fn impalers_radar_mexes(&self, pending: &BTreeMap<i32, f32>, shields: &[(Vec3, f32)]) -> Vec<Target> {
        let Some(mex) = self.defs.get(MEX) else { return Vec::new() };
        let frame = self.frame;
        let spots = &self.graph.metal_spots;
        let known: Vec<Vec3> = (self.war.targets.values())
            .filter(|e| e.identified && e.is_static && !e.is_dead)
            .map(|e| e.position)
            .collect();
        let mut taken: BTreeSet<usize> = BTreeSet::new();
        let mut mexes = Vec::new();
        for (&id, &(sx, sz, n, since)) in &self.impalers.contacts {
            if frame - since < SPOT_FOR || n == 0 {
                continue;
            }
            let mean = Vec3::xz(sx / n as f32, sz / n as f32);
            let mut by: Vec<(usize, f32)> = spots.iter().map(|m| (m.index, distance(m.position, mean))).collect();
            by.sort_by(|a, b| a.1.total_cmp(&b.1));
            let Some(&(spot, d)) = by.first() else { continue };
            if d > SPOT_SNAP || by.get(1).is_some_and(|o| o.1 - d < SPOT_CLEAR) {
                continue;
            }
            let p = spots[spot].position;
            if spots[spot].owned
                || self.impalers.not_mexes.contains(&(id, spot))
                || !taken.insert(spot)
                || known.iter().any(|&k| distance(k, p) < 50.0)
                || shields.iter().any(|&(s, r)| distance(s, p) <= r + 10.0)
            {
                continue;
            }
            let dealt: f32 = self.impalers.dealt.get(&id).map_or(0.0, |d| d.iter().map(|x| x.1).sum());
            let left = mex.health() - dealt - DAMAGE * pending.get(&id).copied().unwrap_or(0.0);
            if left > 0.0 {
                let pos = Vec3::new(p.x, self.cb.map().elevation_at(p.x, p.z).max(0.0), p.z);
                let worth = mex.cost(self.metal) * ECONOMY;
                mexes.push(Target { id, pos, mobile: false, left, per_shot: DAMAGE, worth });
            }
        }
        mexes
    }

    /// Fire what a battery's guns reach: the targets worth most per missile
    /// (those a volley kills outright first), as many missiles at each as
    /// kill it. Guns reloading for a little longer join volleys at
    /// buildings; mobiles take guns ready now only.
    fn impalers_volley(&mut self, bid: u32, targets: &mut [Target]) {
        let frame = self.frame;
        let Some(b) = self.impalers.batteries.get(&bid) else { return };
        // Guns free to fire: where, and frames until reloaded.
        let mut free: Vec<(i32, Vec3, i32)> = (b.guns.iter())
            .filter(|id| self.impalers.guns.get(id).is_some_and(|g| g.order.is_none()))
            .filter(|&&id| !self.retreat_handler.is_retreating(id))
            .filter_map(|id| self.fighters.get(id))
            .filter(|f| !f.unit.is_paralyzed() && f.unit.rules_param_float("disarmed", 0.0) <= 0.0)
            .map(|f| (f.id, f.get_pos(), f.unit.weapon(0).map_or(0, |w| w.reload_frame()) - frame))
            .collect();
        if free.iter().all(|g| g.2 > 0) {
            return;
        }
        loop {
            // Each target with the guns that may fire at it now.
            let options = targets.iter().enumerate().filter(|(_, t)| t.left > 0.0).filter_map(|(i, t)| {
                let wait = if t.mobile { 0 } else { SYNC };
                let guns: Vec<usize> =
                    (0..free.len()).filter(|&g| free[g].2 <= wait && in_range(free[g].1, t.pos)).collect();
                let ready = guns.iter().any(|&g| free[g].2 <= 0);
                (ready && (!t.mobile || guns.len() >= t.shots())).then(|| (i, t.score(guns.len()), guns))
            });
            let Some((i, _, mut guns)) = options.max_by(|a, b| a.1.total_cmp(&b.1)) else { break };
            // Those ready now first.
            guns.sort_by_key(|&g| free[g].2);
            let t = targets[i].clone();
            let shots = t.shots();
            guns.truncate(shots);
            let ids: Vec<i32> = guns.iter().map(|&g| free[g].0).collect();
            let off = distance(free[guns[0]].1, t.pos);
            guns.sort_unstable_by(|a, b| b.cmp(a));
            for g in guns {
                free.remove(g);
            }
            targets[i].left -= t.per_shot * ids.len() as f32;
            for &id in &ids {
                self.impalers_fire(id, &t);
            }
            let name = match self.war.targets.get(&t.id).map(|e| e.def_name.as_str()) {
                Some("") => "mex on radar",
                name => name.unwrap_or("?"),
            };
            let n = ids.len();
            let volley = if n >= shots { format!("{n} to kill") } else { format!("{n} of the {shots} it takes") };
            self.log(&format!(
                "impalers: {volley} at a {name}{} ({:.0} worth, {:.0} health) at ({:.0}, {:.0}), {off:.0} off",
                if t.mobile { " standing still" } else { "" },
                t.worth,
                t.left,
                t.pos.x,
                t.pos.z,
            ));
            if free.iter().all(|g| g.2 > 0) {
                break;
            }
        }
    }

    /// Order gun `id` to fire at `t`: a mobile (in sight) by itself, a
    /// building at its place.
    fn impalers_fire(&mut self, id: i32, t: &Target) {
        let Some(unit) = self.fighters.get(&id).map(|f| f.unit) else { return };
        let reload = unit.weapon(0).map_or(0, |w| w.reload_frame());
        if t.mobile
            && let Some(e) = self.war.targets.get(&t.id)
        {
            unit.attack(e.unit, 0);
        } else {
            unit.execute_custom_command(CMD_ATTACK, &[t.pos.x, t.pos.y, t.pos.z], 0, NO_TIMEOUT);
        }
        let frame = self.frame;
        if let Some(g) = self.impalers.guns.get_mut(&id) {
            g.order = Some(Order { target: t.id, at: t.pos, since: frame, reload });
        }
    }

    /// Keep a battery where it can fire: at its station while that is
    /// clear of the enemy, covered by our forces about as when picked, and
    /// has targets in reach; else at a new one (or where it stands), on the
    /// front of worth and exposure (see [`EPS`]); else by the haven nearest
    /// to what it would shoot. Guns that are threatened fall back to a
    /// haven.
    fn impalers_station(&mut self, bid: u32, targets: &[Target], frame: i32) {
        let Some(b) = self.impalers.batteries.get(&bid) else { return };
        let pos = self.impalers_battery_pos(b);
        if pos == NULLPOS {
            return;
        }
        let n = b.guns.len();
        let (station, since, cover) = (b.station, b.since, b.cover);
        let buildings: Vec<&Target> = targets.iter().filter(|t| !t.mobile && t.left > 0.0).collect();
        let keep = station.filter(|&s| {
            self.impalers_safe(s)
                && (frame - since < RESTATION
                    || (buildings.iter().any(|t| in_range(s, t.pos))
                        && self.war.get_total_friendly_power(s) >= COVER_KEPT * cover))
        });
        let station = match keep {
            Some(s) => s,
            None => {
                let new = self.impalers_pick_station(pos, n, &buildings);
                let s = new.as_ref().map_or_else(
                    || {
                        let toward = buildings.first().map_or(self.graph.get_enemy_center(), |t| t.pos);
                        self.graph_get_closest_haven(toward)
                    },
                    |c| c.pos,
                );
                if s == NULLPOS {
                    return;
                }
                let cover = new.as_ref().map_or_else(|| self.war.get_total_friendly_power(s), |c| c.cover);
                if let Some(c) = &new
                    && station.is_none_or(|o| distance(o, s) > AT_STATION)
                {
                    let (k, v) = buildings
                        .iter()
                        .filter(|t| in_range(s, t.pos))
                        .fold((0, 0.0), |(k, v), t| (k + 1, v + t.worth));
                    self.log(&format!(
                        "impalers: a battery of {n} goes to ({:.0}, {:.0}), {:.0} off, in range of {k} buildings worth \
                         {v:.0}; exposed {:.0} s (cover {:.0} there), {:.0}% of the best worth (exposed {:.0} s there), \
                         least exposed of {} worth enough",
                        s.x,
                        s.z,
                        distance(pos, s),
                        c.exposure,
                        c.cover,
                        100.0 * c.value / c.best.0.max(1e-3),
                        c.best.1,
                        c.front
                    ));
                }
                if let Some(b) = self.impalers.batteries.get_mut(&bid) {
                    b.station = Some(s);
                    b.since = frame;
                    b.cover = cover;
                }
                s
            }
        };
        let guns: Vec<i32> = self.impalers.batteries.get(&bid).map_or(Vec::new(), |b| b.guns.clone());
        for (k, id) in guns.into_iter().enumerate() {
            let Some(p) = self.fighters.get(&id).map(|f| f.get_pos()) else { continue };
            let Some(g) = self.impalers.guns.get(&id) else { continue };
            if self.retreat_handler.is_retreating(id) || g.order.is_some() {
                continue;
            }
            let threatened = self.war.view(self.defs.get(IMPALER)).get_effective_threat(p) > 0.0;
            // Spread about the station.
            let a = k as f32 * 2.4;
            let spot = if threatened {
                self.graph_get_closest_haven(p)
            } else {
                Vec3::xz(station.x + 60.0 * a.cos(), station.z + 60.0 * a.sin())
            };
            if distance(p, spot) <= AT_STATION || frame - g.last_move < MOVE_EVERY {
                continue;
            }
            let mut nav = nav!(self);
            if let Some(f) = self.fighters.get_mut(&id) {
                f.move_to(spot, &mut nav);
            }
            if let Some(g) = self.impalers.guns.get_mut(&id) {
                g.last_move = frame;
            }
        }
    }

    /// Whether a battery may stand at `p`: no enemy reaches it, nor this
    /// far around.
    fn impalers_safe(&self, p: Vec3) -> bool {
        let view = self.war.view(self.defs.get(IMPALER));
        let ring = (0..4).map(|k| {
            let a = k as f32 * std::f32::consts::FRAC_PI_2;
            Vec3::xz(p.x + SAFE_RING * a.cos(), p.z + SAFE_RING * a.sin())
        });
        std::iter::once(p).chain(ring).all(|q| view.get_porc_threat(q) <= 0.0 && view.get_effective_threat(q) <= 0.0)
    }

    /// Seconds a battery at `from` going to `to` would spend out of our
    /// forces' cover over the next [`HORIZON`]: on the way (straight, at the
    /// guns' `speed`) and then there.
    fn impalers_exposure(&self, from: Vec3, to: Vec3, speed: f32) -> f32 {
        let d = distance(from, to);
        let steps = (d / EXPOSURE_STEP).ceil().max(1.0);
        let uncovered = |q: Vec3| self.war.get_total_friendly_power(q) <= 0.0;
        let out = (1..=steps as usize)
            .map(|k| k as f32 / steps)
            .filter(|f| uncovered(Vec3::xz(from.x + f * (to.x - from.x), from.z + f * (to.z - from.z))))
            .count() as f32;
        let walk = d / speed.max(1.0);
        let stay = (HORIZON - walk).max(0.0);
        (walk * out / steps).min(HORIZON) + if uncovered(to) { stay } else { 0.0 }
    }

    /// A station for a battery of `n` guns at `from`, among where it stands,
    /// the places at range from the buildings worth most per missile and
    /// those behind our squads, that are on the map, safe and reachable.
    /// Each is worth what it reaches (outright kills at their bonus) for the
    /// way there, and leaves the guns out of our cover so long (see
    /// [`Ai::impalers_exposure`]): of those worth at least `1 - eps` of the
    /// most, the one least exposed (and of those, the one worth most), so
    /// that no other is both worth more and less exposed.
    fn impalers_pick_station(&self, from: Vec3, n: usize, buildings: &[&Target]) -> Option<Station> {
        let def = self.defs.get(IMPALER)?;
        let map = self.cb.map();
        let (w, h) = (map.width() as f32 * 8.0, map.height() as f32 * 8.0);
        let mut spots: Vec<(f32, f32)> = vec![(from.x, from.z)];
        for t in buildings.iter().take(STATION_TARGETS) {
            for r in STATION_RADII {
                for d in 0..STATION_DIRS {
                    let a = d as f32 * std::f32::consts::TAU / STATION_DIRS as f32;
                    spots.push((t.pos.x + r * a.cos(), t.pos.z + r * a.sin()));
                }
            }
        }
        let home = self.graph.get_ally_center();
        let squads = self.squad_handler.squads.iter().chain(&self.squad_handler.taskforces);
        for s in squads.filter_map(|&id| self.squads.get(id)).filter(|s| !s.is_dead(&self.fighters)) {
            let at = s.get_pos(&self.fighters);
            if at != NULLPOS && distance(at, home) > BEHIND_SQUAD {
                let p = get_directional_point(at, home, BEHIND_SQUAD);
                spots.push((p.x, p.z));
            }
        }
        let mut places: Vec<(Vec3, f32)> = Vec::new();
        for (x, z) in spots {
            if x < 50.0 || z < 50.0 || x > w - 50.0 || z > h - 50.0 {
                continue;
            }
            let p = Vec3::new(x, map.elevation_at(x, z), z);
            let value: f32 = buildings.iter().filter(|o| in_range(p, o.pos)).map(|o| o.score(n)).sum();
            if value > 0.0 {
                places.push((p, value / (1.0 + distance(from, p) / 1000.0)));
            }
        }
        places.sort_by(|a, b| b.1.total_cmp(&a.1));
        let fits = |p: Vec3| self.impalers_safe(p) && reaches(self.reach_terrain(), def, from, p, AT_STATION);
        let &(top, most) = places.iter().find(|c| fits(c.0))?;
        let floor = (1.0 - self.impalers.eps.unwrap_or(EPS)) * most;
        let exposure = |p: Vec3| self.impalers_exposure(from, p, def.speed());
        let mut front: Vec<(Vec3, f32, f32)> =
            (places.iter()).take_while(|c| c.1 >= floor).map(|&(p, v)| (p, v, exposure(p))).collect();
        front.sort_by(|a, b| a.2.total_cmp(&b.2).then(b.1.total_cmp(&a.1)));
        let k = front.len();
        let best = (most, exposure(top));
        let (pos, value, exposure) = front.into_iter().find(|c| fits(c.0))?;
        let cover = self.war.get_total_friendly_power(pos);
        Some(Station { pos, value, exposure, cover, best, front: k })
    }

    /// Say now and then what the guns did.
    fn impalers_report(&mut self, frame: i32, targets: &[Target]) {
        if frame - self.impalers.last_report < REPORT_FRAMES {
            return;
        }
        self.impalers.last_report = frame;
        // What there is to shoot, and what keeps buildings from being targets.
        let shields = self.impalers_shield_sources();
        let mut whose: BTreeMap<String, (usize, f32)> = BTreeMap::new();
        for (_, r, name) in &shields {
            let w = whose.entry(name.clone()).or_default();
            *w = (w.0 + 1, r.max(w.1));
        }
        let buildings: Vec<&Enemy> =
            self.war.targets.values().filter(|e| e.identified && e.is_static && !e.is_dead).collect();
        let shielded =
            buildings.iter().filter(|e| shields.iter().any(|s| distance(s.0, e.position) <= s.1 + 10.0)).count();
        let gone = buildings.iter().filter(|e| self.impalers.gone.contains_key(&e.unit_id)).count();
        let whose: Vec<String> = whose.iter().map(|(n, (k, r))| format!("{k} {n} ({r:.0})")).collect();
        let frame_now = self.frame;
        let standing = self.impalers.contacts.values().filter(|c| frame_now - c.3 >= SPOT_FOR).count();
        self.log(&format!(
            "impalers: {} buildings known, {shielded} under shields, {gone} taken as killed; {standing} radar \
             contacts standing, {} not mexes; shields: {}",
            buildings.len(),
            self.impalers.not_mexes.len(),
            if whose.is_empty() { "none".to_owned() } else { whose.join(", ") }
        ));
        let h = &self.impalers;
        let s = &h.stats;
        let positions: Vec<Vec3> = h.guns.keys().filter_map(|id| self.fighters.get(id)).map(|f| f.get_pos()).collect();
        let reach: BTreeSet<i32> =
            targets.iter().filter(|t| positions.iter().any(|&p| in_range(p, t.pos))).map(|t| t.id).collect();
        let aimed = (s.hits - s.strays).max(1) as f32;
        let (flight, off) = (s.flight as f32 / aimed, s.flight_error as f32 / aimed);
        let line = format!(
            "impalers: {} guns in {} batteries; {} shots: {} hits ({} strays), {} misses, {} unseen, {} late; {} kills \
             ({:.0} metal); {} targets, {} in reach; flight {flight:.0} frames ({off:+.0} on the model)",
            h.guns.len(),
            h.batteries.len(),
            s.shots,
            s.hits,
            s.strays,
            s.misses,
            s.unseen,
            s.late,
            s.kills,
            s.killed_metal,
            targets.len(),
            reach.len()
        );
        self.log(&line);
    }
}

/// How far a unit of def `ud` shields (0 if it does not).
fn shield_radius(ud: UnitDef) -> f32 {
    (ud.weapon_mounts().iter())
        .filter_map(|m| m.weapon_def())
        .filter(|w| w.is_shield())
        .map(|w| w.shield_radius())
        .fold(0.0, f32::max)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missiles_take_four_to_five_seconds() {
        // Rises ~620 elmos in 2.1 s, then on (148 frames measured at ~1400).
        assert!((120..140).contains(&flight_frames(500.0, 0.0)));
        assert!((140..165).contains(&flight_frames(1400.0, 0.0)));
    }

    #[test]
    fn as_many_missiles_as_kill() {
        let t = |left: f32, per_shot: f32| Target { id: 0, pos: NULLPOS, mobile: false, left, per_shot, worth: 100.0 };
        // Mex, Stardust, Fusion; a closed Gauss.
        assert_eq!(t(560.0, DAMAGE).shots(), 1);
        assert_eq!(t(1500.0, DAMAGE).shots(), 2);
        assert_eq!(t(2200.0, DAMAGE).shots(), 3);
        assert_eq!(t(3000.0, DAMAGE * 0.333).shots(), 12);
        assert!(t(2200.0, DAMAGE).score(3) > 3.0 * t(2200.0, DAMAGE).score(2));
    }
}
