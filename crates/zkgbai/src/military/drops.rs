//! Air drops: Charons (light air transports, one unit each) land a strike
//! force where it can do damage and should not be able to get: past the
//! chokepoints guarding the enemy's value, deep in its territory.
//!
//! With a gunship factory, the AI keeps a flight of Charons. Once they are
//! home, they pick up the strongest units that ride light (Zero-K slows a
//! Charon to 0.7 with cargo up to 500 metal, to 0.4 above, and it is too
//! slow to live then) — assaults and riots of squads still gathering,
//! light raiders only to fill up — and
//! land them by the enemy's most valuable buildings that two things hold
//! for: no anti-air on the way or there (most of the flight must get
//! through), and fewer defenders on the ground, turrets and units together,
//! than the drop force takes on. Landed, the force is an attacking squad on
//! its own; the Charons fly home for the next sortie.
//!
//! Between sorties, a Charon also ferries single units where they are
//! wanted and could not walk: a Djinn to a teleport exit behind the enemy's
//! lines (see `teleport`).

use std::collections::BTreeMap;

use springai::{NO_TIMEOUT, Unit, Vec3};
use zk_terrain::paths::SNAP;

use crate::Ai;
use crate::kgbutil::{PathCost, kgb_move_to};
use crate::military::Fighter;
use crate::nav;
use crate::terrain::pos;
use crate::util::{OPTION_SHIFT_KEY, distance};

/// Engine commands.
const CMD_LOAD_UNITS: i32 = 75;
const CMD_UNLOAD_UNITS: i32 = 80;

/// Frames between updates.
const PERIOD: i32 = 30;

/// Charons per sortie.
const FLIGHT: usize = 4;

/// Drops are worth making from this income on.
const MIN_INCOME: f32 = 20.0;

/// Frames between sorties, and before an unfulfilled production order is
/// forgotten.
const SORTIE_DELAY: i32 = 30 * 60;
const ORDER_TIMEOUT: i32 = 30 * 60;

/// Frames allowed for loading, and for the whole sortie.
const LOAD_TIMEOUT: i32 = 30 * 30;
const SORTIE_TIMEOUT: i32 = 30 * 120;

/// Units this far from the Charons are not picked up.
const PICKUP_RANGE: f32 = 2000.0;

/// Unload radius around the drop point, elmos.
const DROP_RADIUS: f32 = 120.0;

/// The enemy's ground strength at the landing (turrets and units, on the
/// scale of enemy danger) may be this share of the drop force's.
const GROUND_ODDS: f32 = 0.6;

/// Enemy value within this of the landing counts as at stake, elmos.
const DAMAGE_RADIUS: f32 = 600.0;

/// A landing must put at least this much enemy value at stake.
const MIN_DROP_VALUE: f32 = 800.0;

/// The way in is checked for anti-air this often, elmos.
const ROUTE_STEP: f32 = 250.0;

/// How deep in the enemy's land a landing must be: its share of the ground
/// way from our base to theirs (where our ground forces would not get by
/// walking).
const BACKLAND: f32 = 0.6;

/// A Charon and the unit it carries (`squad`: an assault, not a raider).
#[derive(Clone, Copy)]
struct Load {
    charon: Unit,
    cargo: Unit,
    squad: bool,
    landed: bool,
    /// Where the Charon was last seen alive.
    last: Vec3,
}

#[derive(Clone, Copy, PartialEq)]
enum Stage {
    Loading,
    Flying,
}

/// A Charon ferrying one unit (a Djinn to its exit, say).
#[derive(Clone, Copy)]
struct Ferry {
    charon: Unit,
    cargo: Unit,
    to: Vec3,
    stage: Stage,
    since: i32,
}

/// How a ferry is going.
#[derive(Clone, Copy, PartialEq)]
pub enum Ferried {
    Underway,
    Landed(Vec3),
    Failed,
}

/// What tells whether cargo is aboard.
#[derive(Clone, Copy)]
struct Aboard {
    own_mass: f32,
    map: springai::Map,
    team: i32,
}

impl Aboard {
    fn alive(&self, u: Unit) -> bool {
        u.health() > 0.0 && u.team() == self.team
    }

    /// Whether `u` is off the ground (or sea).
    fn lifted(&self, u: Unit) -> bool {
        let p = u.pos();
        p.y > self.map.elevation_at(p.x, p.z).max(0.0) + 15.0
    }

    /// Whether `cargo` is aboard `charon`: Zero-K's transport gadget weighs
    /// the Charon with its cargo, or the cargo is up in the air beside it.
    fn carried(&self, charon: Unit, cargo: Unit) -> bool {
        self.alive(charon)
            && (charon.rules_param_float("massOverride", 0.0) > self.own_mass + 1.0
                || (self.lifted(cargo) && distance(cargo.pos(), charon.pos()) < 60.0))
    }
}

/// A ferry (carrying medium cargo, slowly) gives up after this long.
const FERRY_TIMEOUT: i32 = 30 * 240;

struct Sortie {
    loads: Vec<Load>,
    drop: Vec3,
    stage: Stage,
    since: i32,
}

/// Our Charons and the sortie under way.
#[derive(Default)]
pub struct Drops {
    charons: BTreeMap<i32, Unit>,
    sortie: Option<Sortie>,
    /// When a Charon was queued at a factory.
    ordered: Option<i32>,
    last_sortie: i32,
    ferry: Option<Ferry>,
    /// The cargo of the last ferry, and how it went.
    ferried: Option<(i32, Ferried)>,
}

/// A unit that fits a seat, and what it brings (on the scale of enemy
/// danger); `squad`: an assault or riot, not a raider.
struct Seat {
    unit: Unit,
    power: f32,
    squad: bool,
}

impl Ai {
    /// Whether a gunship factory should build a Charon now.
    pub fn drops_want_charon(&self) -> bool {
        let d = &self.drops;
        !self.dev_off("drops")
            && d.charons.len() < FLIGHT
            && d.ordered.is_none_or(|f| self.frame - f > ORDER_TIMEOUT)
            && self.eco.adjusted_income >= MIN_INCOME
            && self.drops_cargo(self.graph.get_ally_center()).len() >= FLIGHT
            // Somewhere guarded by chokepoints, or out of reach, by ground.
            && self.strategy.as_ref().is_some_and(|s| {
                s.analysis.drops.iter().any(|d| d.guards > 0 || d.ground_distance.is_none())
            })
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

    fn drops_go_home(&self, charon: Unit) {
        let home = self.graph.get_ally_center();
        if home.x >= 0.0 {
            charon.move_to(home, 0);
        }
    }

    pub fn drops_update(&mut self, frame: i32) {
        if (frame + self.offset + 23) % PERIOD != 0 {
            return;
        }
        let team = self.team_id;
        self.drops.charons.retain(|_, u| u.health() > 0.0 && u.team() == team);
        if self.drops.ferry.is_some() {
            self.drops_ferry_fly(frame);
        } else if self.drops.sortie.is_some() {
            self.drops_fly(frame);
        } else if self.drops.charons.len() >= FLIGHT && frame - self.drops.last_sortie >= SORTIE_DELAY {
            self.drops_launch(frame);
        }
    }

    fn drops_aboard(&self) -> Aboard {
        let own_mass = self.defs.get("gunshiptrans").map_or(0.0, |d| d.mass());
        Aboard { own_mass, map: self.cb.map(), team: self.team_id }
    }

    /// Whether no anti-air is known on the straight way from `from` to `to`.
    pub fn drops_clear_sky(&self, from: Vec3, to: Vec3) -> bool {
        let steps = (distance(from, to) / ROUTE_STEP).ceil().max(1.0) as usize;
        (0..=steps).all(|k| {
            let t = k as f32 / steps as f32;
            self.war.get_aa_threat(Vec3::xz(from.x + (to.x - from.x) * t, from.z + (to.z - from.z) * t)) <= 0.0
        })
    }

    /// A Charon free to ferry a unit (none is out on a sortie or ferry).
    pub fn drops_free_charon(&self) -> Option<Unit> {
        if self.dev_off("drops") || self.drops.sortie.is_some() || self.drops.ferry.is_some() {
            return None;
        }
        let home = self.graph.get_ally_center();
        (self.drops.charons.values().copied())
            .filter(|c| c.health() > 0.0)
            .min_by(|a, b| distance(a.pos(), home).total_cmp(&distance(b.pos(), home)))
    }

    /// Have a free Charon carry `cargo` to `to`; whether one is on it.
    pub fn drops_ferry(&mut self, cargo: Unit, to: Vec3) -> bool {
        let Some(charon) = self.drops_free_charon() else { return false };
        cargo.stop(0);
        charon.execute_custom_command(CMD_LOAD_UNITS, &[cargo.id() as f32], 0, NO_TIMEOUT);
        let since = self.frame;
        self.drops.ferry = Some(Ferry { charon, cargo, to, stage: Stage::Loading, since });
        self.drops.ferried = Some((cargo.id(), Ferried::Underway));
        let what = self.defs.name_of(cargo).to_owned();
        self.log(&format!("drops: Charon ferrying a {what} to ({:.0}, {:.0})", to.x, to.z));
        true
    }

    /// How the ferry of `cargo` went, if it had one.
    pub fn drops_ferried(&self, cargo: Unit) -> Option<Ferried> {
        self.drops.ferried.filter(|f| f.0 == cargo.id()).map(|f| f.1)
    }

    fn drops_ferry_fly(&mut self, frame: i32) {
        let Some(f) = self.drops.ferry else { return };
        let aboard = self.drops_aboard();
        let alive = |u: Unit| aboard.alive(u);
        let carried = aboard.carried(f.charon, f.cargo);
        let outcome = match f.stage {
            Stage::Loading if carried => {
                self.drops_route(f.charon, f.to);
                self.drops.ferry = Some(Ferry { stage: Stage::Flying, since: frame, ..f });
                None
            }
            Stage::Loading if frame - f.since > LOAD_TIMEOUT || !alive(f.charon) || !alive(f.cargo) => {
                Some(Ferried::Failed)
            }
            Stage::Loading => None,
            Stage::Flying if !alive(f.cargo) => Some(Ferried::Failed),
            Stage::Flying if !carried && !aboard.lifted(f.cargo) => Some(Ferried::Landed(f.cargo.pos())),
            Stage::Flying => {
                if carried && (frame - f.since > FERRY_TIMEOUT || distance(f.charon.pos(), f.to) < DROP_RADIUS) {
                    let p = f.charon.pos();
                    f.charon.execute_custom_command(CMD_UNLOAD_UNITS, &[p.x, p.y, p.z, DROP_RADIUS], 0, NO_TIMEOUT);
                }
                None
            }
        };
        let Some(outcome) = outcome else { return };
        self.drops.ferry = None;
        self.drops.ferried = Some((f.cargo.id(), outcome));
        if alive(f.charon) {
            self.drops_go_home(f.charon);
        }
        let how = match outcome {
            Ferried::Landed(p) => format!("landed {:.0} from its goal", distance(p, f.to)),
            _ => "failed".to_owned(),
        };
        self.log(&format!("drops: ferry {how}"));
    }

    /// How many units near `at` would fit a Charon's seat.
    pub fn drops_cargo_count(&self, at: Vec3) -> usize {
        self.drops_cargo(at).len()
    }

    /// Units near `at` that ride a Charon light, the most useful first:
    /// assaults and riots of squads still gathering, then light raiders.
    fn drops_cargo(&self, at: Vec3) -> Vec<Seat> {
        let Some(charon) = self.defs.get("gunshiptrans") else { return Vec::new() };
        let max_size = 2 * charon.transport_size();
        let light = |d: springai::UnitDef| {
            d.custom_param("requiremediumtrans").is_none() && d.custom_param("requireheavytrans").is_none()
        };
        let fits = |f: &Fighter| {
            f.unit.def().is_some_and(|d| light(d) && d.x_size() <= max_size && !d.is_able_to_fly())
                && !self.retreat_is_retreating(f.unit)
                && distance(f.get_pos(), at) <= PICKUP_RANGE
        };
        let gathering = |f: &Fighter| f.squad.and_then(|s| self.squads.get(s)).is_none_or(|s| s.status != 'a');
        let seat = |f: &Fighter, squad| Seat { unit: f.unit, power: (f.power + f.unit.max_health()) / 10.0, squad };
        let h = &self.raider_handler;
        let mut squads: Vec<Seat> = (self.squad_handler.fighters.iter())
            .filter_map(|id| self.fighters.get(id))
            .filter(|f| !f.shield_mob && gathering(f) && fits(f))
            .map(|f| seat(f, true))
            .collect();
        let mut raiders: Vec<Seat> = (h.small_raiders.iter().chain(&h.medium_raiders))
            .filter_map(|id| self.fighters.get(id))
            .filter(|f| gathering(f) && fits(f))
            .map(|f| seat(f, false))
            .collect();
        squads.sort_by(|a, b| b.power.total_cmp(&a.power));
        raiders.sort_by(|a, b| b.power.total_cmp(&a.power));
        squads.extend(raiders);
        squads
    }

    /// Where to land a force `power` strong flying from `from`, and the
    /// enemy value at stake there: by the enemy's buildings deep in its
    /// land, where no anti-air covers the way in or the landing, the
    /// defenders are well short of the force, and the most value is at
    /// stake — the more so the deeper, and behind chokepoints that guard it
    /// from the ground.
    fn drops_target(&self, from: Vec3, power: f32) -> Option<(Vec3, f32)> {
        let war = &self.war;
        let s = self.strategy.as_ref()?;
        let map = self.terrain().and_then(|t| t.tier(s.tier))?;
        let a = &s.analysis;
        // How far into the enemy's land (by ground, from our base to
        // theirs), and how well guarded: (depth, guard bonus).
        let backland = |p: Vec3| -> Option<(f32, f32)> {
            let r = map.region_near(pos(p), SNAP)?;
            let (ours, theirs) = (a.dist_from_ally[r], a.dist_from_enemy[r]);
            let depth = match (ours.is_finite(), theirs.is_finite()) {
                (false, _) => 1.0,
                (true, false) => return None,
                _ => ours / (ours + theirs).max(1.0),
            };
            let d = a.drops.iter().find(|d| d.region == r);
            let guards =
                d.map_or(1.0, |d| if d.ground_distance.is_none() { 3.0 } else { 1.0 + (d.guards as f32).min(2.0) });
            (depth >= BACKLAND).then_some((depth, guards))
        };
        let clear_sky = |to: Vec3| self.drops_clear_sky(from, to);
        war.targets
            .values()
            .filter(|e| e.identified && e.is_static && !e.is_dead && !self.graph.is_ally_territory(e.position))
            .map(|e| e.position)
            .filter(|&p| war.get_enemy_power(p) <= GROUND_ODDS * power)
            .filter_map(|p| {
                let at_stake: f32 = war
                    .targets
                    .values()
                    .filter(|e| e.identified && !e.is_dead && !e.can_fly() && distance(e.position, p) <= DAMAGE_RADIUS)
                    .map(|e| e.value)
                    .sum();
                (at_stake >= MIN_DROP_VALUE).then_some((p, at_stake))
            })
            .filter_map(|(p, v)| backland(p).map(|(depth, guards)| (p, v, v * guards * depth * depth)))
            .filter(|&(p, ..)| clear_sky(p))
            .max_by(|a, b| a.2.total_cmp(&b.2))
            .map(|(p, v, _)| (p, v))
    }

    /// Pick up a flight's worth of strike force and set off, if there is
    /// somewhere to go.
    fn drops_launch(&mut self, frame: i32) {
        let charons: Vec<Unit> = self.drops.charons.values().copied().collect();
        let Some(&first) = charons.first() else { return };
        let base = first.pos();
        let mut cargo = self.drops_cargo(base);
        if cargo.len() < FLIGHT {
            return;
        }
        cargo.truncate(FLIGHT);
        let power: f32 = cargo.iter().map(|s| s.power).sum();
        let Some((drop, value)) = self.drops_target(base, power) else { return };
        let mut loads = Vec::new();
        for (&charon, seat) in charons.iter().zip(&cargo) {
            let u = seat.unit;
            if seat.squad {
                self.squad_remove_unit(u);
            } else {
                self.raider_remove_unit(u);
            }
            self.retreat_remove_unit(u);
            u.stop(0);
            charon.execute_custom_command(CMD_LOAD_UNITS, &[u.id() as f32], 0, NO_TIMEOUT);
            loads.push(Load { charon, cargo: u, squad: seat.squad, landed: false, last: charon.pos() });
        }
        let names: Vec<&str> = loads.iter().map(|l| self.defs.name_of(l.cargo)).collect();
        self.log(&format!(
            "drops: {} Charons loading {} (power {power:.0}) for ({:.0}, {:.0}), {value:.0} metal at stake",
            loads.len(),
            names.join(", "),
            drop.x,
            drop.z
        ));
        self.drops.sortie = Some(Sortie { loads, drop, stage: Stage::Loading, since: frame });
    }

    fn drops_fly(&mut self, frame: i32) {
        let Some(s) = self.drops.sortie.as_ref() else { return };
        let (drop, stage, since) = (s.drop, s.stage, s.since);
        let mut loads = s.loads.clone();
        let team = self.team_id;
        let alive = |u: Unit| u.health() > 0.0 && u.team() == team;
        let aboard = self.drops_aboard();
        let lifted = |u: Unit| aboard.lifted(u);
        let carried = |l: &Load| aboard.carried(l.charon, l.cargo);

        match stage {
            Stage::Loading => {
                let all = loads.iter().all(|l| !alive(l.cargo) || !alive(l.charon) || carried(l));
                if all || frame - since > LOAD_TIMEOUT {
                    for l in loads.iter_mut() {
                        if alive(l.charon) && alive(l.cargo) && carried(l) {
                            self.drops_route(l.charon, drop);
                        } else {
                            // Not picked up: back to what it was doing.
                            l.landed = true;
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
                    self.log("drops: flight away");
                }
            }
            Stage::Flying => {
                let late = frame - since > SORTIE_TIMEOUT;
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
                        let what = self.defs.name_of(l.cargo);
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
                    } else if late || !alive(l.charon) {
                        let p = l.charon.pos();
                        l.charon.execute_custom_command(CMD_UNLOAD_UNITS, &[p.x, p.y, p.z, DROP_RADIUS], 0, NO_TIMEOUT);
                    }
                }
            }
        }

        let done = loads.iter().all(|l| l.landed);
        if let Some(s) = self.drops.sortie.as_mut() {
            s.loads = loads;
        }
        if !done {
            return;
        }
        // Those on the ground by the drop fight on together.
        let landed: Vec<Unit> = (self.drops.sortie.take().map(|s| s.loads).unwrap_or_default().iter())
            .filter(|l| l.cargo.health() > 0.0 && distance(l.cargo.pos(), drop) < 2.0 * DAMAGE_RADIUS)
            .map(|l| l.cargo)
            .collect();
        let squad = self.squad_adopt(&landed);
        for &u in &landed {
            self.retreat_add_coward(u);
        }
        self.log(&format!("drops: {} landed (squad {squad:?})", landed.len()));
        self.drops.last_sortie = frame;
    }

    /// Fly a loaded Charon to the drop along a path that avoids anti-air,
    /// and unload there.
    fn drops_route(&mut self, charon: Unit, drop: Vec3) {
        let frame = self.frame;
        let nav = nav!(self);
        let mut path = nav.pathfinder.find_path(nav.war, charon, drop, PathCost::AirPath);
        path.pop_back();
        let mut opts = 0;
        while let Some(p) = path.pop_front() {
            if path.len().is_multiple_of(3) {
                kgb_move_to(charon, p, opts, frame);
                opts = OPTION_SHIFT_KEY;
            }
        }
        charon.execute_custom_command(CMD_UNLOAD_UNITS, &[drop.x, drop.y, drop.z, DROP_RADIUS], opts, NO_TIMEOUT);
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
