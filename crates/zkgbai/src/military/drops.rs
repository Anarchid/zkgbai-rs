//! Air drops: Charons (light air transports, one unit each) land a strike
//! force where it can do damage and could not get to on foot: by enemy
//! value whose ground way, for a force that strong, is closed or long — by
//! the terrain, or by what holds the regions on it (see
//! `zk_terrain::occupancy`) — while the sky over the way there is clear.
//!
//! A sortie carries at least five true riots of one movement tier. It raids
//! reachable economic checkpoints, discounting light QRF but rejecting
//! systematically fortified pockets. Landed troops stay under drop control:
//! they spend themselves damaging local infrastructure, evade skirmishers,
//! and never inherit the ordinary army's mainland retreat orders.
//!
//! Between sorties a free Charon also ferries colony constructors.

use std::collections::BTreeMap;

use springai::{NO_TIMEOUT, Unit, UnitDef, Vec3};
use zk_terrain::occupancy::Occupancy;
use zk_terrain::paths::{Paths, SNAP};
use zk_terrain::{Tier, TierMap};

use crate::Ai;
use crate::kgbutil::{AirField, PathCost};
use crate::military::Fighter;
use crate::nav;
use crate::terrain::{pos, tier_of};
use crate::util::{OPTION_SHIFT_KEY, distance};

mod policy;
mod raid;
use policy::{Contact, RAID_REACH, cargo_allowed, pocket_value};
use raid::Raid;

/// Engine commands, and Zero-K's one-click abilities (the Charon's first is
/// Drop Cargo).
const CMD_MOVE: i32 = 10;
const CMD_ONECLICK_WEAPON: i32 = 35000;
const CMD_LOAD_UNITS: i32 = 75;
const CMD_UNLOAD_UNITS: i32 = 80;
const CMD_UNLOAD_UNIT: i32 = 81;

/// Frames between updates.
const PERIOD: i32 = 30;

/// Charons per sortie.
pub(crate) const FLIGHT: usize = 5;

/// Drops are worth making from this income on.
const MIN_INCOME: f32 = 20.0;

/// Frames between sorties, and before an unfulfilled production order is
/// forgotten.
const SORTIE_DELAY: i32 = 30 * 60;
const ORDER_TIMEOUT: i32 = 30 * 60;

/// Frames between reports of why no sortie goes.
const REPORT_PERIOD: i32 = 30 * 180;

/// Frames allowed for loading, and for the sortie beyond the flight's time.
const LOAD_TIMEOUT: i32 = 30 * 40;
const SORTIE_SLACK: i32 = 30 * 60;

/// A Charon's speed with light cargo, as a share of its own; the flight's
/// time is reckoned at least and at most this many frames.
const LADEN: f32 = 0.7;
const MIN_FLIGHT: i32 = 30 * 20;
const MAX_FLIGHT_FRAMES: i32 = 30 * 150;

/// Cargo aboard hangs this close under its Charon, elmos across (it swings
/// out as the Charon turns; falling cargo has no live Charon, and unloaded
/// cargo is on the ground).
const HANGING: f32 = 150.0;

/// Frames between reports of a sortie in the air.
const STATUS_PERIOD: i32 = 30 * 30;

/// Units this far from the Charons are not picked up.
const PICKUP_RANGE: f32 = 3500.0;

/// Minimum economic utility for transport procurement; reassessment period.
const PROSPECT: f32 = MIN_DROP_VALUE;
const PROSPECT_PERIOD: i32 = 900;

/// Unload radius around each Charon's landing spot, elmos, doubling with
/// every spot it tries: the engine refuses an unload order outright if it
/// finds no room for the cargo within it when the order is given.
const DROP_RADIUS: f32 = 128.0;

/// Landing spots keep this far from enemy buildings, and this far from
/// features (wrecks, rocks, trees), elmos.
const BUILDING_CLEARANCE: f32 = 120.0;
const FEATURE_CLEARANCE: f32 = 40.0;

/// Frames for an order to show in the command queue (Zero-K's transport
/// gadget gives it anew the next frame).
const SETTLE: i32 = 60;

/// Landing spots are this far apart, within this of the landing (or this,
/// out of the enemy's fire), elmos; this many more than the Charons are
/// kept in reserve.
const SPOT_SPACING: f32 = 96.0;
const SPOT_REACH: f32 = 400.0;
const SAFE_REACH: f32 = 700.0;
const SPARE_SPOTS: usize = 6;

/// A Charon under fire this close to its spot ejects its cargo (Zero-K's
/// Drop Cargo: the fall hurts it less than the fire would); so does one
/// this badly hurt this close to the drop.
const EJECT_RANGE: f32 = 800.0;
const EJECT_HEALTH: f32 = 0.35;
const EJECT_DROP_RANGE: f32 = 2500.0;

/// Frames between checks for a combat landing.
const EJECT_CHECK: i32 = 6;

/// A Charon this close to its spot is setting down; one that has not in
/// this many frames tries the next spot, this many times at most, then
/// anywhere it can.
const NEAR: f32 = 400.0;
const STALL: i32 = 30 * 15;
const MAX_TRIES: u32 = 4;

/// The enemy's ground strength at the landing (turrets and units, on the
/// scale of enemy danger) may be this share of the drop force's.
const GROUND_ODDS: f32 = 0.6;

/// A landing must offer this much economic utility (replacement + mex denial).
pub(crate) const MIN_DROP_VALUE: f32 = 800.0;

/// The walk there (fighting through what holds the way) must be this many
/// times the flight; a closed way counts as the most.
const MIN_GAIN: f32 = 1.5;
const MAX_GAIN: f32 = 4.0;

/// Anti-air the flight may pass (threat times cells flown under it).
const MAX_EXPOSURE: f32 = 0.25;

/// The longest flight, elmos (over ground we have not seen lately it may
/// meet anti-air we do not know of); a flight this long counts half as
/// much as a short one.
const MAX_FLIGHT: f32 = 8000.0;
const FLIGHT_SCALE: f32 = 3000.0;

/// A Charon and its cargo (`squad`: the original handler for aborted loads).
#[derive(Clone)]
struct Load {
    charon: Unit,
    cargo: Unit,
    what: String,
    squad: bool,
    landed: bool,
    /// Where the Charon was last seen alive.
    last: Vec3,
    /// Where it is to set its cargo down, and how many spots it has tried.
    spot: Vec3,
    tries: u32,
    /// When it was last told to unload, and since when it has been near its
    /// spot without setting down.
    ordered: i32,
    near: Option<i32>,
    /// The Charon's weight before loading (Zero-K's gadgets weigh every
    /// unit their own way), and its health when last seen.
    empty_mass: f32,
    health: f32,
    /// Dropped in the air (falling cargo hangs under its Charon too); not
    /// picked up, and so back with its handler.
    ejected: bool,
    returned: bool,
}

#[derive(Clone, Copy, PartialEq)]
enum Stage {
    Loading,
    Flying,
}

/// What tells whether cargo is aboard.
#[derive(Clone, Copy)]
struct Aboard {
    map: springai::Map,
    team: i32,
}

impl Aboard {
    fn alive(&self, u: Unit) -> bool {
        u.health() > 0.0 && u.team() == self.team
    }

    /// Whether `u` is off the ground (or sea): its position is its middle,
    /// up to its height above the ground when it stands.
    fn lifted(&self, u: Unit) -> bool {
        let p = u.pos();
        let height = u.def().map_or(30.0, |d| d.height());
        p.y > self.map.elevation_at(p.x, p.z).max(0.0) + height + 15.0
    }

    /// Whether `l`'s cargo is aboard its Charon: Zero-K's transport gadget
    /// weighs the Charon with its cargo (when it lets us see that), or the
    /// cargo hangs in the air under it (how far under depends on the unit).
    fn carried(&self, l: &Load) -> bool {
        let (c, u) = (l.charon.pos(), l.cargo.pos());
        !l.ejected
            && self.alive(l.charon)
            && (l.charon.rules_param_float("massOverride", 0.0) > l.empty_mass + 1.0
                || (self.lifted(l.cargo) && distance(Vec3::xz(c.x, c.z), Vec3::xz(u.x, u.z)) < HANGING))
    }
}

struct Sortie {
    loads: Vec<Load>,
    drop: Vec3,
    /// The way there, and landing spots no Charon has tried yet.
    route: Vec<Vec3>,
    spare: Vec<Vec3>,
    stage: Stage,
    since: i32,
    /// Frames the flight should take.
    flight: i32,
    aborting: bool,
    raid: Raid,
}

/// A Charon flying a constructor to a colony.
struct Ferry {
    load: Load,
    stage: Stage,
    since: i32,
    route: Vec<Vec3>,
    spare: Vec<Vec3>,
    flight: i32,
}

/// Our Charons and the sortie under way.
#[derive(Default)]
pub struct Drops {
    charons: BTreeMap<i32, Unit>,
    sortie: Option<Sortie>,
    ferry: Option<Ferry>,
    /// When a Charon was queued at a factory.
    ordered: Option<i32>,
    last_sortie: i32,
    /// When it was last told why no sortie went.
    last_report: i32,
    last_reason: &'static str,
    /// Enemy value only drops get to (see `drops_prospect`).
    prospect: f32,
    raids: Vec<Raid>,
}

/// A unit that fits a seat, and what it brings (on the scale of enemy
/// danger); `squad` remembers which handler to restore after an abort.
struct Seat {
    unit: Unit,
    def: UnitDef,
    tier: Option<Tier>,
    power: f32,
    squad: bool,
}

/// Why the enemy's buildings were passed over as landings: too well
/// defended, no ground for the cargo by them, anti-air on the way (or too
/// far to fly), walkable (the walk not long enough against the flight), too
/// little at stake.
#[derive(Default)]
struct Passed {
    defended: usize,
    no_ground: usize,
    anti_air: usize,
    walkable: usize,
    little: usize,
}

/// Where a flight is to land.
struct Landing {
    at: Vec3,
    value: f32,
    /// Elmos on foot (fighting through; `INFINITY`: no way through) and by
    /// air, and the anti-air on the way.
    walk: f32,
    flight: f32,
    exposure: f32,
    route: Vec<Vec3>,
    spots: Vec<Vec3>,
}

impl Ai {
    /// Whether a gunship factory should build a Charon now.
    pub fn drops_want_charon(&self) -> bool {
        let d = &self.drops;
        !self.dev_off("drops")
            && d.charons.len() < FLIGHT
            && d.ordered.is_none_or(|f| self.frame - f > ORDER_TIMEOUT)
            && self.eco.adjusted_income >= MIN_INCOME
            && self.drops_cargo_count(self.graph.get_ally_center()) >= FLIGHT
            // Enemy value our ground forces would have to fight their way
            // to, or go the long way round.
            && d.prospect >= PROSPECT
    }

    /// Enemy value only drops get to, as last assessed.
    pub fn drops_prospect_value(&self) -> f32 {
        self.drops.prospect
    }

    /// Economic utility of the best viable pocket for the available riots.
    /// Procurement and launch share their terrain, defense and flight gates.
    fn drops_prospect(&self) -> f32 {
        let home = self.graph.get_ally_center();
        let Some((tier, seats)) = self.drops_pick(self.drops_cargo(home)) else { return 0.0 };
        let power = seats.iter().map(|s| s.power).sum::<f32>();
        // Procurement uses the same economic, fortification and flight gates
        // as launch. Many bad pockets cannot add up to one good sortie.
        self.drops_target(home, power, tier, seats[0].def).map_or(0.0, |landing| landing.value)
    }

    /// A factory has been told to build a Charon.
    pub fn drops_ordered(&mut self) {
        self.drops.ordered = Some(self.frame);
        self.log(&format!("drops: Charon {} of {FLIGHT} ordered", self.drops.charons.len() + 1));
    }

    pub fn drops_add_charon(&mut self, unit: Unit) {
        self.drops.ordered = None;
        self.drops.charons.insert(unit.id(), unit);
        self.drops_go_home(unit);
    }

    /// Fly home, round the anti-air.
    fn drops_go_home(&mut self, charon: Unit) {
        let home = self.graph.get_ally_center();
        if home.x >= 0.0 {
            self.drops_fly_to(charon, home, 0);
        }
    }

    /// Fly to `to` by the pathfinder's way round the anti-air; the options
    /// for orders to follow.
    fn drops_fly_to(&mut self, charon: Unit, to: Vec3, opts: i16) -> i16 {
        let nav = nav!(self);
        let path = nav.pathfinder.find_path(nav.war, charon, to, PathCost::AirPath);
        self.drops_follow(charon, path.into_iter().collect(), opts)
    }

    /// Fly by `route`, every third waypoint and the last; the options for
    /// orders to follow (`opts` if none were given).
    fn drops_follow(&self, charon: Unit, route: Vec<Vec3>, mut opts: i16) -> i16 {
        let n = route.len();
        for (k, p) in route.into_iter().enumerate() {
            if k % 3 == 2 || k + 1 == n {
                charon.move_to(p, opts);
                opts = OPTION_SHIFT_KEY;
            }
        }
        opts
    }

    pub fn drops_update(&mut self, frame: i32) {
        // Staged probing Charons are the test's (see `strike_test`).
        if self.test.as_deref() == Some("aaprobe") {
            return;
        }
        if frame % EJECT_CHECK == 0 {
            self.drops_eject_check();
        }
        if (frame + self.offset + 23) % PERIOD != 0 {
            return;
        }
        if (frame + self.offset + 23) % PROSPECT_PERIOD == 0 && !self.dev_off("drops") {
            self.drops.prospect = self.drops_prospect();
        }
        let team = self.team_id;
        self.drops.charons.retain(|_, u| u.health() > 0.0 && u.team() == team);
        self.drops_ferry_update(frame);
        self.drops_raids_update();
        if self.drops.sortie.is_some() {
            self.drops_fly(frame);
        } else if self.drops.charons.len() >= FLIGHT && frame - self.drops.last_sortie >= SORTIE_DELAY {
            self.drops_launch(frame);
        }
    }

    fn drops_aboard(&self) -> Aboard {
        Aboard { map: self.cb.map(), team: self.team_id }
    }

    /// How many units near `at` would fill a flight of one movement tier.
    pub fn drops_cargo_count(&self, at: Vec3) -> usize {
        self.drops_pick(self.drops_cargo(at)).map_or(0, |(_, seats)| seats.len())
    }

    /// True riots that ride light and are gathering or safely at home,
    /// strongest first. QRF and operator-controlled units stay with their job.
    fn drops_cargo(&self, at: Vec3) -> Vec<Seat> {
        let Some(charon) = self.defs.get("gunshiptrans") else { return Vec::new() };
        let max_size = 2 * charon.transport_size();
        let light = |d: UnitDef| {
            d.custom_param("requiremediumtrans").is_none() && d.custom_param("requireheavytrans").is_none()
        };
        let fits = |f: &Fighter| {
            cargo_allowed(self.defs.name_of(f.unit))
                && f.unit.health() > 0.0
                && f.unit.team() == self.team_id
                && !self.qrf_claims(f.id)
                && !f.squad.is_some_and(|sid| {
                    self.op.over.squad_intents.contains_key(&sid) || self.op_squad_target(sid).is_some()
                })
                && f.unit.def().is_some_and(|d| light(d) && d.x_size() <= max_size && !d.is_able_to_fly())
                && !self.retreat_is_retreating(f.unit)
                && distance(f.get_pos(), at) <= PICKUP_RANGE
        };
        let gathering = |f: &Fighter| {
            f.squad.and_then(|s| self.squads.get(s)).is_none_or(|s| s.status != 'a')
                // Small ordinary squads become active before five riots
                // accumulate. They remain recruitable while safely at home.
                || (distance(f.get_pos(), self.graph.get_ally_center()) <= 1200.0
                    && self.war.get_effective_threat(f.get_pos()) <= 0.0)
        };
        let seat = |f: &Fighter, squad| {
            let def = f.unit.def()?;
            Some(Seat { unit: f.unit, def, tier: tier_of(def), power: (f.power + f.unit.max_health()) / 10.0, squad })
        };
        let h = &self.raider_handler;
        let mut squads: Vec<Seat> = (self.squad_handler.fighters.iter())
            .filter_map(|id| self.fighters.get(id))
            .filter(|f| !f.shield_mob && gathering(f) && fits(f))
            .filter_map(|f| seat(f, true))
            .collect();
        let mut raiders: Vec<Seat> = (h.small_raiders.iter().chain(&h.medium_raiders))
            .filter_map(|id| self.fighters.get(id))
            .filter(|f| gathering(f) && fits(f))
            .filter_map(|f| seat(f, false))
            .collect();
        squads.sort_by(|a, b| b.power.total_cmp(&a.power));
        raiders.sort_by(|a, b| b.power.total_cmp(&a.power));
        squads.extend(raiders);
        squads
    }

    /// The flight's worth of `seats` of one movement tier (they land on the
    /// same ground, and walk the same ways) that is strongest.
    fn drops_pick(&self, seats: Vec<Seat>) -> Option<(Tier, Vec<Seat>)> {
        let mut by_tier: BTreeMap<u8, (Tier, Vec<Seat>)> = BTreeMap::new();
        for s in seats {
            let Some(tier) = s.tier else { continue };
            let (_, group) = by_tier.entry(tier as u8).or_insert((tier, Vec::new()));
            if group.len() < FLIGHT {
                group.push(s);
            }
        }
        let power = |g: &[Seat]| g.iter().map(|s| s.power).sum::<f32>();
        (by_tier.into_values()).filter(|(_, g)| g.len() >= FLIGHT).max_by(|a, b| power(&a.1).total_cmp(&power(&b.1)))
    }

    /// Where to land a force of tier `tier`, `power` strong, flying from
    /// `from`: by the enemy's buildings, where the defenders are well short
    /// of the force and enough value is at stake, the sky on the cheapest
    /// way there is clear, and walking there instead (fighting through what
    /// holds the regions in between) would be much the longer way or no way
    /// at all; the most value at stake, times that, first.
    fn drops_target(&self, from: Vec3, power: f32, tier: Tier, def: UnitDef) -> Result<Landing, Passed> {
        let war = &self.war;
        let mut passed = Passed::default();
        // A staged drop lands anywhere (see `strike_test`).
        let staged = self.test.as_deref().is_some_and(|t| t.starts_with("drop"));
        let Some(map) = self.terrain().and_then(|t| t.tier(tier)) else { return Err(passed) };
        let occupancy = Occupancy::new(map, &self.region_presence());
        let walk = occupancy.fighting_distances(map, pos(from), power);
        let grid = self.cb.map();
        let air = AirField::new(war, grid.width() / 8, grid.height() / 8, from);
        let mut options: Vec<(Vec3, f32, f32, f32, f32, f32)> = Vec::new();
        let contacts = self.drops_contacts();
        for p in contacts.iter().filter(|c| c.value > 0.0).map(|c| c.at) {
            if policy::danger_at(&contacts, p) > GROUND_ODDS * power {
                passed.defended += 1;
                continue;
            }
            let Some(r) = map.region_near(pos(p), SPOT_REACH) else {
                passed.no_ground += 1;
                continue;
            };
            let (Some(flight), Some(exposure)) = (air.length(p), air.exposure(p)) else { continue };
            if exposure > MAX_EXPOSURE || flight > MAX_FLIGHT {
                passed.anti_air += 1;
                continue;
            }
            let gain = (walk[r] / flight.max(1.0)).min(MAX_GAIN);
            if gain < if staged { 0.0 } else { MIN_GAIN } {
                passed.walkable += 1;
                continue;
            }
            let paths = policy::combat_paths(map, &contacts, p, GROUND_ODDS * power);
            let Some(at_stake) =
                pocket_value(&contacts, p, power, |at| paths.distance_to(pos(at)).is_some_and(|d| d <= RAID_REACH))
            else {
                passed.defended += 1;
                continue;
            };
            if at_stake < if staged { 0.0 } else { MIN_DROP_VALUE } {
                passed.little += 1;
                continue;
            }
            options.push((p, at_stake, walk[r], flight, exposure, at_stake * gain / (1.0 + flight / FLIGHT_SCALE)));
        }
        options.sort_by(|a, b| b.5.total_cmp(&a.5));
        // The best with ground enough to land on.
        let n = options.len();
        let landing = options.into_iter().find_map(|(at, value, walk, flight, exposure, _)| {
            let spots = self.drops_spots(map, def, at);
            (spots.len() >= FLIGHT).then(|| Landing { at, value, walk, flight, exposure, route: air.route(at), spots })
        });
        passed.no_ground += n - usize::from(landing.is_some());
        landing.ok_or(passed)
    }

    /// Spots near `at`, spread apart, where cargo `def` can stand: passable
    /// ground of its tier that leads to `at`, clear of the enemy's buildings
    /// and of features, and that the engine finds clear (when it finds
    /// enough so); those out of the enemy's fire first (a little off the
    /// target, out of its defenders' reach), the nearest first.
    fn drops_spots(&self, map: &TierMap, def: UnitDef, at: Vec3) -> Vec<Vec3> {
        let grid = self.cb.map();
        let buildings: Vec<Vec3> = (self.war.targets.values())
            .filter(|e| e.is_static && !e.is_dead && distance(e.position, at) <= SPOT_REACH + BUILDING_CLEARANCE)
            .map(|e| e.position)
            .collect();
        let clear_of = |p: Vec3| {
            buildings.iter().all(|&b| distance(b, p) >= BUILDING_CLEARANCE)
                && self.cb.features_in_cylinder(p, FEATURE_CLEARANCE).is_empty()
        };
        let (w, h) = (grid.width() as f32 * 8.0, grid.height() as f32 * 8.0);
        let step = SPOT_SPACING / 2.0;
        let mut ground = Vec::new();
        for ring in 0..=(SAFE_REACH / step) as i32 {
            let radius = ring as f32 * step;
            let around = ((std::f32::consts::TAU * radius / step) as i32).max(1);
            for k in 0..around {
                let a = std::f32::consts::TAU * k as f32 / around as f32;
                let (x, z) = (at.x + radius * a.cos(), at.z + radius * a.sin());
                let p = Vec3::new(x, grid.elevation_at(x, z), z);
                let inside = x > 16.0 && z > 16.0 && x < w - 16.0 && z < h - 16.0;
                if inside && map.is_passable(pos(p)) && map.connected(pos(p), pos(at), SNAP) && clear_of(p) {
                    ground.push(p);
                }
            }
        }
        // Out of the enemy's fire first (the target's defenders cover it,
        // not all around it), the nearest first; under fire only within
        // the usual reach.
        let fired_on = |p: Vec3| self.war.get_threat(p) > 0.0;
        ground.retain(|&p| !fired_on(p) || distance(p, at) <= SPOT_REACH);
        ground.sort_by_key(|&p| fired_on(p));
        let spread = |list: Vec<Vec3>| {
            let mut spots: Vec<Vec3> = Vec::new();
            for p in list {
                if spots.iter().all(|&s| distance(s, p) >= SPOT_SPACING) {
                    spots.push(p);
                }
            }
            spots
        };
        let clear = spread(ground.iter().copied().filter(|&p| grid.is_possible_to_build_at(def, p, 0)).collect());
        if clear.len() >= FLIGHT { clear } else { spread(ground) }
    }

    /// Pick up a flight's worth of strike force and set off, if there is
    /// somewhere to go.
    fn drops_launch(&mut self, frame: i32) {
        let ferrying = self.drops.ferry.as_ref().map(|f| f.load.charon.id());
        let charons: Vec<Unit> = self.drops.charons.values().copied().filter(|c| Some(c.id()) != ferrying).collect();
        if charons.len() < FLIGHT || self.dev_off("drops") {
            return;
        }
        let Some(&first) = charons.first() else { return };
        let base = first.pos();
        let seats = self.drops_cargo(base);
        let mut tiers: BTreeMap<&str, usize> = BTreeMap::new();
        for s in &seats {
            *tiers.entry(s.tier.map_or("none", |t| t.name())).or_default() += 1;
        }
        let Some((tier, mut cargo)) = self.drops_pick(seats) else {
            if self.drops.last_reason != "cargo" || frame - self.drops.last_report >= REPORT_PERIOD {
                self.drops.last_report = frame;
                self.drops.last_reason = "cargo";
                let (squads, raiders) = (self.squad_handler.fighters.len(), self.raider_handler.small_raiders.len());
                self.log(&format!(
                    "drops: {} Charons home, but no {FLIGHT} of one tier to carry within {PICKUP_RANGE:.0}: {tiers:?} \
                     (of {squads} squad fighters, {raiders} small raiders)",
                    charons.len()
                ));
            }
            return;
        };
        cargo.truncate(FLIGHT);
        let power: f32 = cargo.iter().map(|s| s.power).sum();
        let landing = match self.drops_target(base, power, tier, cargo[0].def) {
            Ok(landing) => landing,
            Err(p) => {
                if self.drops.last_reason != "landing" || frame - self.drops.last_report >= REPORT_PERIOD {
                    self.drops.last_report = frame;
                    self.drops.last_reason = "landing";
                    self.log(&format!(
                        "drops: no landing for {} {} (power {power:.0}): of the enemy's buildings {} too well \
                         defended, {} without ground for them, {} past anti-air or too far, {} as near on foot, {} with \
                         too little at stake",
                        cargo.len(),
                        tier.name(),
                        p.defended,
                        p.no_ground,
                        p.anti_air,
                        p.walkable,
                        p.little
                    ));
                }
                return;
            }
        };
        self.drops.last_reason = "";
        let mut spots = landing.spots.clone();
        spots.truncate(FLIGHT + SPARE_SPOTS);
        let spare = spots.split_off(FLIGHT.min(spots.len()));
        let mut loads = Vec::new();
        for ((&charon, seat), &spot) in charons.iter().zip(&cargo).zip(&spots) {
            let u = seat.unit;
            if seat.squad {
                self.squad_remove_unit(u);
            } else {
                self.raider_remove_unit(u);
            }
            self.retreat_remove_unit(u);
            u.stop(0);
            charon.execute_custom_command(CMD_LOAD_UNITS, &[u.id() as f32], 0, NO_TIMEOUT);
            let (last, what) = (charon.pos(), self.defs.name_of(u).to_owned());
            let (squad, ordered) = (seat.squad, frame + LOAD_TIMEOUT);
            let empty_mass = charon.rules_param_float("massOverride", charon.def().map_or(0.0, |d| d.mass()));
            let (landed, health) = (false, charon.health());
            loads.push(Load {
                charon,
                cargo: u,
                what,
                squad,
                landed,
                last,
                spot,
                tries: 1,
                ordered,
                near: None,
                empty_mass,
                health,
                ejected: false,
                returned: false,
            });
        }
        let names: Vec<&str> = loads.iter().map(|l| l.what.as_str()).collect();
        let on_foot = if landing.walk.is_finite() {
            format!("{:.0} elmos on foot", landing.walk)
        } else {
            "no way on foot".into()
        };
        self.log(&format!(
            "drops: {} Charons loading {} (power {power:.0}) for ({:.0}, {:.0}), {:.0} economic utility: {on_foot}, \
             {:.0} by air past {:.2} anti-air",
            loads.len(),
            names.join(", "),
            landing.at.x,
            landing.at.z,
            landing.value,
            landing.flight,
            landing.exposure
        ));
        let speed = self.defs.get("gunshiptrans").map_or(100.0, |d| d.speed()).max(1.0);
        let flight = ((30.0 * landing.flight / (LADEN * speed)) as i32).clamp(MIN_FLIGHT, MAX_FLIGHT_FRAMES);
        let route = landing.route;
        self.drops.sortie = Some(Sortie {
            loads,
            drop: landing.at,
            route,
            spare,
            stage: Stage::Loading,
            since: frame,
            flight,
            aborting: false,
            raid: Raid::new(landing.at, tier),
        });
    }

    /// Unload where `l` is to set its cargo down, the wider the more spots
    /// it has tried.
    fn drops_unload(l: &Load, opts: i16) {
        let (s, radius) = (l.spot, (DROP_RADIUS * 2f32.powi(l.tries as i32 - 1)).min(SPOT_REACH));
        l.charon.execute_custom_command(CMD_UNLOAD_UNITS, &[s.x, s.y, s.z, radius], opts, NO_TIMEOUT);
    }

    fn drops_fly(&mut self, frame: i32) {
        let Some(s) = self.drops.sortie.as_ref() else { return };
        let (drop, stage, since, flight, aborting) = (s.drop, s.stage, s.since, s.flight, s.aborting);
        let mut raid = s.raid.clone();
        let route = s.route.clone();
        let mut spare = s.spare.clone();
        let mut loads = s.loads.clone();
        let team = self.team_id;
        let alive = |u: Unit| u.health() > 0.0 && u.team() == team;
        let aboard = self.drops_aboard();
        let lifted = |u: Unit| aboard.lifted(u);
        let carried = |l: &Load| aboard.carried(l);

        match stage {
            Stage::Loading => {
                let all = loads.iter().all(|l| !alive(l.cargo) || !alive(l.charon) || carried(l));
                if all || frame - since > LOAD_TIMEOUT {
                    let ready = loads.iter().filter(|l| alive(l.charon) && alive(l.cargo) && carried(l)).count();
                    if !policy::full_flight(ready) {
                        // A timeout is not permission to send a token raid.
                        // Keep ownership until loaded units are safely down.
                        self.log(&format!("drops: abort loading, only {ready}/{FLIGHT} aboard"));
                        let home = self.graph.get_ally_center();
                        for l in &mut loads {
                            if alive(l.charon) && alive(l.cargo) && carried(l) {
                                l.spot = home;
                                let opts = self.drops_fly_to(l.charon, home, 0);
                                Self::drops_unload(l, opts);
                                l.ordered = frame;
                            } else if alive(l.cargo) && lifted(l.cargo) {
                                // A transport loss can leave falling cargo.
                                // Do not hand it to a ground handler mid-air.
                                l.ejected = true;
                            } else {
                                l.landed = true;
                                l.returned = true;
                                self.drops_return(l.cargo, l.squad);
                                if alive(l.charon) {
                                    self.drops_go_home(l.charon);
                                }
                            }
                        }
                        if let Some(s) = self.drops.sortie.as_mut() {
                            s.loads = loads;
                            s.spare.clear();
                            s.stage = Stage::Flying;
                            s.since = frame;
                            s.aborting = true;
                        }
                        return;
                    }
                    for l in loads.iter_mut() {
                        if alive(l.charon) && alive(l.cargo) && carried(l) {
                            let opts = self.drops_follow(l.charon, route.clone(), 0);
                            Self::drops_unload(l, opts);
                            l.ordered = frame;
                        } else {
                            // Not picked up: back to what it was doing.
                            (l.landed, l.returned) = (true, true);
                            self.drops_return(l.cargo, l.squad);
                            if alive(l.charon) {
                                self.drops_go_home(l.charon);
                            }
                        }
                    }
                    if let Some(s) = self.drops.sortie.as_mut() {
                        s.stage = Stage::Flying;
                        s.since = frame;
                    }
                    let how: Vec<String> = (loads.iter())
                        .map(|l| {
                            let (c, u) = (l.charon.pos(), l.cargo.pos());
                            format!(
                                "{} {}: mass {:.0}/{:.0}, {:.0} up, {:.0} across",
                                l.what,
                                if l.landed { "left" } else { "aboard" },
                                l.charon.rules_param_float("massOverride", 0.0),
                                l.empty_mass,
                                u.y - aboard.map.elevation_at(u.x, u.z).max(0.0),
                                distance(Vec3::xz(c.x, c.z), Vec3::xz(u.x, u.z))
                            )
                        })
                        .collect();
                    self.log(&format!("drops: flight away after {} s ({})", (frame - since) / 30, how.join("; ")));
                }
            }
            Stage::Flying => {
                let late = frame - since > flight + SORTIE_SLACK;
                if (frame - since) % STATUS_PERIOD < PERIOD {
                    let status: Vec<String> = (loads.iter().filter(|l| !l.landed))
                        .map(|l| {
                            let ids: Vec<i32> = l.charon.current_commands().iter().map(|c| c.id()).collect();
                            let (c, u) = (l.charon.pos(), l.cargo.pos());
                            format!(
                                "{} {:.0} from its spot{} ({:.0} up, {:.0} across), orders {ids:?}",
                                l.what,
                                distance(c, l.spot),
                                if carried(l) { " aboard" } else { "" },
                                u.y - aboard.map.elevation_at(u.x, u.z).max(0.0),
                                distance(Vec3::xz(c.x, c.z), Vec3::xz(u.x, u.z))
                            )
                        })
                        .collect();
                    self.log(&format!("drops: in the air {} s: {}", (frame - since) / 30, status.join("; ")));
                }
                for l in loads.iter_mut().filter(|l| !l.landed) {
                    if alive(l.charon) {
                        l.last = l.charon.pos();
                    }
                    // Off the Charon but still falling: not down yet.
                    if alive(l.cargo) && !carried(l) && lifted(l.cargo) {
                        continue;
                    }
                    if !alive(l.cargo) || !carried(l) {
                        // Dropped (or shot down with its Charon).
                        l.landed = true;
                        if aborting {
                            self.drops_return(l.cargo, l.squad);
                            l.returned = true;
                        } else if alive(l.cargo) {
                            // Fight immediately; a stuck final transport must
                            // not leave the first landed riots idle for minutes.
                            let local = self.terrain().and_then(|t| t.tier(raid.tier)).is_some_and(|map| {
                                Paths::new(map, pos(drop), |_| false)
                                    .distance_to(pos(l.cargo.pos()))
                                    .is_some_and(|d| d <= RAID_REACH)
                            });
                            if local {
                                raid.members.push(l.cargo);
                            } else {
                                let mut stranded = Raid::new(l.cargo.pos(), raid.tier);
                                stranded.members.push(l.cargo);
                                self.drops.raids.push(stranded);
                            }
                        }
                        let what = &l.what;
                        let fate = if alive(l.cargo) {
                            let p = l.cargo.pos();
                            format!("landed {:.0} from the drop at ({:.0}, {:.0})", distance(p, drop), p.x, p.z)
                        } else if alive(l.charon) {
                            "died aboard".to_owned()
                        } else {
                            format!("went down with its Charon {:.0} from the drop", distance(l.last, drop))
                        };
                        self.log(&format!("drops: {what} {fate}"));
                        if alive(l.charon) {
                            self.drops_go_home(l.charon);
                        }
                        continue;
                    }
                    self.drops_watch(l, &mut spare, frame, late);
                }
            }
        }

        self.drops_raid_command(&mut raid);
        let done = loads.iter().all(|l| l.landed);
        if let Some(s) = self.drops.sortie.as_mut() {
            s.loads = loads;
            s.spare = spare;
            s.raid = raid;
        }
        if !done {
            return;
        }
        if let Some(s) = self.drops.sortie.take() {
            let n = s.raid.members.len();
            if n > 0 {
                self.drops.raids.push(s.raid);
            }
            self.log(&format!("drops: {n} landed raiders remain on local damage orders"));
        }
        self.drops.last_sortie = frame;
    }

    /// See that a Charon still carrying its cargo is on its way to set it
    /// down, and if not, have it try the next spot: when its unload order
    /// is gone (the engine refuses or drops it when it finds no room for the
    /// cargo; Zero-K's own unload beside it only flies there on land), or
    /// it hangs by its spot without setting down; when `late` or out of
    /// tries, it unloads wherever it is.
    fn drops_watch(&mut self, l: &mut Load, spare: &mut Vec<Vec3>, frame: i32, late: bool) {
        let here = l.charon.pos();
        if frame - l.ordered < SETTLE {
            return;
        }
        let ids: Vec<i32> = l.charon.current_commands().iter().map(|c| c.id()).collect();
        let unloading = ids.iter().any(|&c| c == CMD_UNLOAD_UNITS || c == CMD_UNLOAD_UNIT);
        let near = distance(here, l.spot) <= NEAR;
        if l.tries > MAX_TRIES && !late && !near {
            // Out of spots on the way: there first, then anywhere it can.
            if !ids.contains(&CMD_MOVE) {
                self.drops_fly_to(l.charon, l.spot, 0);
            }
            return;
        }
        if late || l.tries > MAX_TRIES {
            // Out where it is.
            if l.near.is_none_or(|t| frame - t > STALL) {
                (l.near, l.ordered) = (Some(frame), frame);
                self.drops_eject(l, if late { "is late" } else { "found nowhere to set down" });
            }
            return;
        }
        let why = if !unloading {
            if near { "lost its unload order" } else { "was refused its unload order" }
        } else if near && l.near.is_some_and(|t| frame - t > STALL) {
            "could not set down"
        } else {
            if near && l.near.is_none() {
                l.near = Some(frame);
            }
            return;
        };
        let from = l.spot;
        if !spare.is_empty() {
            l.spot = spare.remove(0);
        }
        (l.tries, l.near, l.ordered) = (l.tries + 1, None, frame);
        let opts = if distance(here, l.spot) > NEAR { self.drops_fly_to(l.charon, l.spot, 0) } else { 0 };
        Self::drops_unload(l, opts);
        self.log(&format!(
            "drops: the Charon with the {} {why} at ({:.0}, {:.0}), {:.0} from it (orders {ids:?}); trying \
             ({:.0}, {:.0})",
            l.what,
            from.x,
            from.z,
            distance(here, from),
            l.spot.x,
            l.spot.z
        ));
    }

    /// A combat landing: a Charon hit by its spot, or badly hurt by the
    /// drop, drops its cargo now (Zero-K's Charons also drop it as they
    /// crash, but from on high).
    fn drops_eject_check(&mut self) {
        let Some(s) = self.drops.sortie.as_ref().filter(|s| s.stage == Stage::Flying && !s.aborting) else { return };
        let (drop, mut loads) = (s.drop, s.loads.clone());
        let aboard = self.drops_aboard();
        for l in loads.iter_mut().filter(|l| !l.landed && aboard.carried(l)) {
            let (here, health, max) = (l.charon.pos(), l.charon.health(), l.charon.max_health());
            let hit = health < l.health - 1.0;
            l.health = health;
            let under_fire = hit && distance(here, l.spot) <= EJECT_RANGE;
            let going_down = health < EJECT_HEALTH * max && distance(here, drop) <= EJECT_DROP_RANGE;
            if (under_fire || going_down) && self.frame - l.ordered >= EJECT_CHECK {
                self.drops_eject(l, if under_fire { "is under fire" } else { "is going down" });
            }
        }
        if let Some(s) = self.drops.sortie.as_mut() {
            s.loads = loads;
        }
    }

    /// Have `l`'s Charon drop its cargo where it is, in the air (Zero-K's
    /// Drop Cargo).
    fn drops_eject(&mut self, l: &mut Load, why: &str) {
        (l.ordered, l.ejected) = (self.frame, true);
        l.charon.execute_custom_command(CMD_ONECLICK_WEAPON, &[1.0], 0, NO_TIMEOUT);
        let p = l.charon.pos();
        let up = p.y - self.cb.map().elevation_at(p.x, p.z).max(0.0);
        self.log(&format!(
            "drops: the Charon with the {} {why} {:.0} from its spot, {up:.0} up; ejecting it",
            l.what,
            distance(p, l.spot)
        ));
    }

    /// Have a free Charon fly constructor `cargo` to `to` (a colony's
    /// middle), by the cheapest way round the anti-air; whether one goes.
    pub fn drops_ferry(&mut self, cargo: Unit, to: Vec3) -> bool {
        if self.dev_off("drops") || self.drops.ferry.is_some() {
            return false;
        }
        let busy: Vec<i32> = self.drops.sortie.iter().flat_map(|s| s.loads.iter().map(|l| l.charon.id())).collect();
        let Some(charon) = (self.drops.charons.values().copied())
            .filter(|c| !busy.contains(&c.id()))
            .min_by(|a, b| distance(a.pos(), cargo.pos()).total_cmp(&distance(b.pos(), cargo.pos())))
        else {
            return false;
        };
        let Some(def) = cargo.def() else { return false };
        let Some(map) = tier_of(def).and_then(|t| self.terrain().and_then(|m| m.tier(t))) else { return false };
        let grid = self.cb.map();
        let air = AirField::new(&self.war, grid.width() / 8, grid.height() / 8, cargo.pos());
        let (Some(length), Some(exposure)) = (air.length(to), air.exposure(to)) else { return false };
        let mut spots = self.drops_spots(map, def, to);
        if exposure > MAX_EXPOSURE || spots.is_empty() {
            return false;
        }
        let spot = spots.remove(0);
        spots.truncate(SPARE_SPOTS);
        cargo.stop(0);
        charon.execute_custom_command(CMD_LOAD_UNITS, &[cargo.id() as f32], 0, NO_TIMEOUT);
        let (frame, what) = (self.frame, self.defs.name_of(cargo).to_owned());
        let empty_mass = charon.rules_param_float("massOverride", charon.def().map_or(0.0, |d| d.mass()));
        let load = Load {
            charon,
            cargo,
            what,
            squad: false,
            landed: false,
            last: charon.pos(),
            spot,
            tries: 1,
            ordered: frame + LOAD_TIMEOUT,
            near: None,
            empty_mass,
            health: charon.health(),
            ejected: false,
            returned: false,
        };
        let speed = self.defs.get("gunshiptrans").map_or(100.0, |d| d.speed()).max(1.0);
        let flight = ((30.0 * length / (LADEN * speed)) as i32).clamp(MIN_FLIGHT, MAX_FLIGHT_FRAMES);
        self.log(&format!(
            "drops: a Charon fetches the {} for a colony at ({:.0}, {:.0}), {length:.0} by air",
            load.what, to.x, to.z
        ));
        let route = air.route(spot);
        self.drops.ferry = Some(Ferry { load, stage: Stage::Loading, since: frame, route, spare: spots, flight });
        true
    }

    /// Whether worker `wid` is being flown to a colony.
    pub fn drops_ferrying(&self, wid: i32) -> bool {
        self.drops.ferry.as_ref().is_some_and(|f| f.load.cargo.id() == wid)
    }

    /// See the ferry under way through loading, the flight and setting down.
    fn drops_ferry_update(&mut self, frame: i32) {
        let Some(f) = self.drops.ferry.as_ref() else { return };
        let (stage, since, flight, route) = (f.stage, f.since, f.flight, f.route.clone());
        let (mut l, mut spare) = (f.load.clone(), f.spare.clone());
        let aboard = self.drops_aboard();
        let (charon_alive, cargo_alive) = (aboard.alive(l.charon), aboard.alive(l.cargo));
        let carried = aboard.carried(&l);
        let done = match stage {
            Stage::Loading if carried => {
                let opts = self.drops_follow(l.charon, route, 0);
                Self::drops_unload(&l, opts);
                l.ordered = frame;
                if let Some(f) = self.drops.ferry.as_mut() {
                    (f.stage, f.since) = (Stage::Flying, frame);
                }
                None
            }
            Stage::Loading if !charon_alive || !cargo_alive || frame - since > LOAD_TIMEOUT => {
                Some(format!("the Charon did not pick up the {}", l.what))
            }
            Stage::Loading => None,
            Stage::Flying if !cargo_alive => Some(format!("the {} was lost on the way", l.what)),
            Stage::Flying if !carried && aboard.lifted(l.cargo) => None,
            Stage::Flying if !carried => Some(format!(
                "the {} is down for its colony, {:.0} from its spot",
                l.what,
                distance(l.cargo.pos(), l.spot)
            )),
            Stage::Flying => {
                self.drops_watch(&mut l, &mut spare, frame, frame - since > flight + SORTIE_SLACK);
                None
            }
        };
        match done {
            Some(what) => {
                self.log(&format!("drops: {what}"));
                if charon_alive {
                    self.drops_go_home(l.charon);
                }
                self.drops.ferry = None;
            }
            None => {
                if let Some(f) = self.drops.ferry.as_mut() {
                    (f.load, f.spare) = (l, spare);
                }
            }
        }
    }

    /// A unit that was not picked up goes back to its handler.
    fn drops_return(&mut self, u: Unit, squad: bool) {
        if u.health() <= 0.0 || u.team() != self.team_id {
            return;
        }
        let Some(def) = u.def() else { return };
        let cost = def.cost(self.metal);
        if squad {
            self.squad_add_assault(Fighter::new(u, cost, self.team_id));
        } else {
            self.raider_add_small_raider(Fighter::new_raider(u, cost, self.team_id));
        }
        self.retreat_add_coward(u);
    }
}
