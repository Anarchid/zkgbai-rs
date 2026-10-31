//! Volley spotters: a member of a doctrine's role with `spot`, who holds
//! the core's mark in sight from
//! the mark to the volley. A Lance or a Phantom firing at a radar dot hits
//! about half as often as at a target in sight (0.49 against 0.90 for the
//! Lance over 472 games, predictor/LOS-READ.md), because dots wobble.
//!
//! The errand is timed to the gun that fires next: early enough before
//! its reload to get there (its way at its speed, and 2 s; 12 s at most),
//! the spotter goes within sight of that gun's mark, or of the target it
//! would mark were it in sight (one shot kills it, worth its floor: radar
//! dots of a type we have seen count), and holds there until the gun has
//! fired; then it comes back to the force. Spotters hold fire by their
//! role's `fire_state`. How it stands, by the role's `spot` (`any`: by the
//! unit's type):
//!
//! - `tank`: an armored scout (a Halberd, armored while closed) drives to
//!   within 0.6 of its sight of the target; hurt, it goes for repairs (the
//!   retreat handler has it) and, healed, is assigned again;
//! - `cloak`: a cloaked unit (a Widow, Scythe or Gremlin) stands off within
//!   sight, past its decloak distance;
//! - `air`: a plane (a Sparrow) overflies the target at the volley and
//!   waits over the force; fired on, it flees home.
//! - `watch`: a standing ground spotter per gun for Phantoms and Emissaries,
//!   or one shared by a battery of up to four Lances. It continuously holds the nearest known enemy
//!   just inside its own sight, on the gun's side. Cloakers keep clear of
//!   enemies along the path and at the destination; Halberds hold fire
//!   throughout, retaining their armour, and repair below 60% health.
//!
//! A force short of its spotter calls one from our standing units, in the
//! order of the role's `units` (a plane only while the enemy shows no
//! anti-air, seen or felt), from wherever it serves (assault squads,
//! raiders, scouts, anti-heavy hunters, anti-air); none is built for it,
//! but for one Sparrow a game from a standing plane factory (see
//! `handlers::squad::squad_produce`). One just finished is claimed too,
//! before the handlers that would take it; a spotter no force wants goes
//! back to them. Widow spotters may take a short bite at a nearby visible
//! heavy under friendly threat, keeping their place in the force and returning
//! to spotting as soon as the shot fires or the opportunity ends.
//!
//! Any doctrine of the squad handler may have the role (a siege doctrine as
//! a lance one): the squad handler calls [`Ai::taskforce_spot`] after each
//! force's conduct, [`Ai::taskforce_spotter_claims`] as units are
//! finished and [`Ai::taskforce_spotter_lost`] as they die.
//!
//! Log lines (`taskforce:`): `a X is called to spot for the D force (squad
//! N, state), M from its leader (unit U)`; `a X spots for the D force
//! (squad N, state), M from its leader (unit U)`; `a X spotting for the D
//! force (squad N) goes to see a Y (W metal, in sight|on radar) for a G's
//! volley in T s, R away`; `... sights|identifies a Y (M metal), R from the
//! guns`; `a X spotter of the D force (squad N, state) goes for repairs at
//! H% health|flees anti-air fire, R from the force`; `a X spotter of the D
//! force (squad N, state) is lost JOB to a K, R from the force` (JOB:
//! escorting, scouting, loitering, overflight, retreating).

use std::collections::{BTreeMap, BTreeSet};

use springai::{NO_TIMEOUT, Unit, UnitDef, Vec3};

use super::marksman::Fix;
use crate::Ai;
use crate::defs::cmd;
use crate::kgbutil::PathCost;
use crate::kgbutil::kgb_move_to;
use crate::military::nukes::segment_distance;
use crate::military::{Fighter, SquadId};
use crate::nav;
use crate::util::{NULLPOS, OPTION_SHIFT_KEY, distance, get_directional_point};

/// How a spotter stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Spot {
    Tank,
    Cloak,
    Air,
    /// By the unit's type: a plane, a cloaked unit, or else a tank.
    Any,
    /// Hold the nearest known enemy at the ground spotter's sight edge.
    Watch,
}

const SPOTS: [Spot; 5] = [Spot::Tank, Spot::Cloak, Spot::Air, Spot::Any, Spot::Watch];

impl Spot {
    pub fn name(self) -> &'static str {
        match self {
            Spot::Tank => "tank",
            Spot::Cloak => "cloak",
            Spot::Air => "air",
            Spot::Any => "any",
            Spot::Watch => "watch",
        }
    }

    pub fn parse(s: &str) -> Result<Spot, String> {
        SPOTS.into_iter().find(|a| a.name() == s).ok_or_else(|| {
            let names: Vec<&str> = SPOTS.iter().map(|a| a.name()).collect();
            format!("spot '{s}': one of {}", names.join(", "))
        })
    }

    /// Whether unit type `d` spots this way: planes fly, loiterers cloak.
    pub fn fits(self, d: UnitDef) -> Result<(), String> {
        match self {
            Spot::Air if !d.is_able_to_fly() => Err("cannot fly (spot air)".into()),
            Spot::Cloak if !d.is_able_to_cloak() => Err("cannot cloak (spot cloak)".into()),
            Spot::Tank | Spot::Cloak | Spot::Watch if d.is_able_to_fly() => Err("flies (spot air)".into()),
            _ => Ok(()),
        }
    }

    /// How a unit of type `d` spots, for a role spotting this way.
    pub fn way(self, d: Option<UnitDef>) -> Spot {
        match (self, d) {
            (Spot::Any, Some(d)) if d.is_able_to_fly() => Spot::Air,
            (Spot::Any | Spot::Watch, Some(d)) if d.is_able_to_cloak() => Spot::Cloak,
            (Spot::Any | Spot::Watch, _) => Spot::Tank,
            (s, _) => s,
        }
    }

    /// Health share below which it goes for repairs (a plane flees when
    /// fired on instead).
    fn repair(self) -> f32 {
        match self {
            Spot::Tank | Spot::Any | Spot::Watch => 0.6,
            Spot::Cloak => 0.5,
            Spot::Air => 0.0,
        }
    }

    /// What it does on an errand, for the log.
    fn spotting(self) -> &'static str {
        match self {
            Spot::Tank | Spot::Any | Spot::Watch => "scouting",
            Spot::Cloak => "loitering",
            Spot::Air => "overflight",
        }
    }

    /// Seconds to spare on arriving before the volley.
    fn lead(self) -> f32 {
        if self == Spot::Air { 1.0 } else { 2.0 }
    }
}

/// A tank stops this share of its sight short of the target; a loiterer
/// this share, and its decloak distance plus `CLOAK_MARGIN` at least.
const TANK_SIGHT: f32 = 0.6;
const CLOAK_SIGHT: f32 = 0.8;
const CLOAK_MARGIN: f32 = 120.0;

/// An errand starts at most this long before the volley.
const MAX_LEAD: i32 = 12 * 30;

/// An errand whose volley has not come this long after it began is given
/// up.
const ERRAND_FRAMES: i32 = 25 * 30;

/// Reach beyond the gun's range for a target to see.
const REACH: f32 = 20.0;

/// Orders are given again when the point moves this far, or this often.
const RESEND_MOVED: f32 = 120.0;
const RESEND_FRAMES: i32 = 90;

/// An escorting spotter this far from the leader is sent back to it.
const STRAYED: f32 = 600.0;

/// What a spotter is doing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Job {
    /// With the force, orbiting the leader (a plane: over it).
    Escorting,
    /// Holding `target` in sight for `gun`'s volley: done once the gun's
    /// reload frame is no longer `reload`.
    Spotting { gun: i32, target: i32, reload: i32 },
    /// Gone for repairs, or fled (the retreat handler has it).
    Repairing,
}

#[derive(Clone, Debug)]
pub struct Spotter {
    pub unit: i32,
    pub sid: SquadId,
    /// How it stands (the role's `spot`, resolved for its type).
    pub spot: Spot,
    pub job: Job,
    pub watch: bool,
    /// The gun whose standing spotter this is, including while repairing.
    gun: Option<i32>,
    since: i32,
    sent: Vec3,
    sent_at: i32,
    /// Escorting: told to orbit the leader since its last errand.
    orbiting: bool,
    /// Spotting: the target was never seen when the errand began, and
    /// whether it has come into sight since.
    unknown: bool,
    sighted: bool,
}

impl Spotter {
    /// The job's name in the log.
    pub fn doing(&self) -> &'static str {
        match self.job {
            Job::Escorting => "escorting",
            Job::Spotting { .. } => self.spot.spotting(),
            Job::Repairing => "retreating",
        }
    }

    /// The target it holds in sight for `gun`, if on such an errand.
    pub fn errand_for(&self, gun: i32) -> Option<i32> {
        match self.job {
            Job::Spotting { gun: g, target, .. } if g == gun => Some(target),
            _ => None,
        }
    }

    /// Whether it holds `target` in sight for `gun`.
    pub fn serves(&self, gun: i32, target: i32) -> bool {
        self.errand_for(gun) == Some(target)
    }
}

/// Spotters by unit id, those let go (never claimed again as they are
/// finished), and how many were built for forces this game.
#[derive(Default)]
pub struct Spotters {
    pub by_unit: BTreeMap<i32, Spotter>,
    released: BTreeSet<i32>,
    pub ordered: u32,
}

fn health_share(u: Unit) -> f32 {
    let max = u.max_health();
    if max > 0.0 { u.health() / max } else { 0.0 }
}

/// Where a spotter goes to see a target at `at` for a gun at `gun`.
fn spot_point(spot: Spot, def: Option<UnitDef>, at: Vec3, gun: Vec3) -> Vec3 {
    let sight = def.map_or(400.0, |d| d.los_radius());
    let off = match spot {
        Spot::Air => return at,
        Spot::Tank | Spot::Any | Spot::Watch => TANK_SIGHT * sight,
        Spot::Cloak => (CLOAK_SIGHT * sight).max(def.map_or(0.0, |d| d.decloak_distance()) + CLOAK_MARGIN),
    };
    if distance(at, gun) <= off {
        return gun;
    }
    let p = get_directional_point(at, gun, off);
    Vec3::new(p.x, at.y, p.z)
}

/// A core gun of a force, for the errands.
struct Gun {
    id: i32,
    unit: Unit,
    /// When it fires next (its reload frame, or now if loaded).
    next: i32,
    reload: i32,
}

/// Keep existing assignments, reserving a distinct gun for each spotter.
/// Repairing spotters participate so their place is kept until they return.
fn pair_guns(watchers: impl IntoIterator<Item = (i32, Option<i32>)>, guns: &[i32]) -> BTreeMap<i32, i32> {
    let watchers: Vec<_> = watchers.into_iter().collect();
    let mut pairs = BTreeMap::new();
    let mut taken = BTreeSet::new();
    for &(id, gun) in &watchers {
        if let Some(gun) = gun.filter(|g| guns.contains(g) && !taken.contains(g)) {
            pairs.insert(id, gun);
            taken.insert(gun);
        }
    }
    for (id, _) in watchers {
        if !pairs.contains_key(&id)
            && let Some(&gun) = guns.iter().find(|g| !taken.contains(g))
        {
            pairs.insert(id, gun);
            taken.insert(gun);
        }
    }
    pairs
}

const WATCH_ARRIVED: f32 = 24.0;
const WATCH_MARGIN: f32 = WATCH_ARRIVED + 8.0;
const WATCH_REPATH: i32 = 15;
const WATCH_LEG: f32 = 300.0;

/// Alternatives stay at the spotter's sight edge, on the gun's side of
/// the enemy. Arrival tolerance must not leave the enemy outside sight.
fn watch_points(gun: Vec3, enemy: Vec3, sight: f32) -> Vec<Vec3> {
    let angle = (gun.z - enemy.z).atan2(gun.x - enemy.x);
    let reach = (sight - WATCH_MARGIN).max(0.0);
    [0.0_f32, 0.25, -0.25, 0.5, -0.5, 0.75, -0.75, 1.0, -1.0]
        .into_iter()
        .map(|turn| {
            let a = angle + turn * std::f32::consts::FRAC_PI_2;
            Vec3::xz(enemy.x + reach * a.cos(), enemy.z + reach * a.sin())
        })
        .collect()
}

fn watch_clear(p: Vec3, circles: &[(Vec3, f32)]) -> bool {
    circles.iter().all(|&(c, r)| distance(p, c) >= r)
}

/// Escape a circle already entered, without crossing another or taking
/// a shortcut back through the enemy on the way out.
fn watch_safe_leg(a: Vec3, b: Vec3, circles: &[(Vec3, f32)]) -> bool {
    circles.iter().all(|&(c, r)| {
        let away = (a.x - c.x) * (b.x - a.x) + (a.z - c.z) * (b.z - a.z);
        segment_distance(c, a, b) >= r || (distance(a, c) < r && away >= 0.0 && distance(b, c) > distance(a, c))
    })
}

impl Ai {
    /// A repaired watcher returns to its reserved place in the same force.
    /// Once its gun or doctrine is gone, let its ordinary handler have it.
    pub(crate) fn taskforce_watch_rejoin(&mut self, id: i32) -> bool {
        let Some(sp) = self.taskforces.spotters.by_unit.get(&id).filter(|sp| sp.watch && sp.job == Job::Repairing)
        else {
            return false;
        };
        if self.retreat_handler.is_retreating(id) {
            return true;
        }
        let sid = sp.sid;
        let Some(unit) = self.fighters.get(&id).map(|f| f.unit) else { return false };
        let role = self.squads.get(sid).and_then(|s| s.shape.by_def.get(&unit.def()?.id()).copied());
        let Some(mut sp) = self.taskforces.spotters.by_unit.remove(&id) else { return false };
        if role.is_some_and(|r| self.squad_short_of(sid, r)) {
            self.retreat_remove_unit(unit);
            let mut nav = nav!(self);
            if let Some(s) = self.squads.get_mut(sid) {
                s.add_unit_nav(id, &mut self.fighters, &mut nav);
            }
            sp.job = Job::Escorting;
            sp.sent_at = i32::MIN / 2;
            self.taskforces.spotters.by_unit.insert(id, sp);
        } else {
            self.taskforces.spotters.by_unit.insert(id, sp);
            self.taskforce_spotter_release(id);
        }
        true
    }

    pub(crate) fn taskforce_spotter_upkeep(&self) -> f32 {
        self.taskforces
            .spotters
            .by_unit
            .values()
            .filter(|s| s.watch)
            .filter_map(|s| self.fighters.get(&s.unit))
            .filter(|f| !f.is_dead())
            .filter_map(|f| f.unit.def())
            .map(|d| d.cloak_cost_moving())
            .sum()
    }

    fn taskforce_watch_circles(&self, unit: Unit) -> Vec<(Vec3, f32)> {
        if !unit.def().is_some_and(|d| d.is_able_to_cloak()) {
            return Vec::new();
        }
        let decloak = unit.def().map_or(125.0, |d| d.decloak_distance()) + 64.0;
        self.war
            .targets
            .values()
            .filter(|e| !e.is_dead && e.position != NULLPOS)
            .map(|e| {
                let size = e.ud.map_or(32.0, |d| d.x_size().max(d.z_size()) as f32 * 4.0);
                (e.position, decloak + size)
            })
            .collect()
    }

    fn taskforce_watch(&mut self, id: i32, unit: Unit, gun: Unit) {
        let frame = self.frame;
        if self.taskforces.spotters.by_unit.get(&id).is_some_and(|sp| frame - sp.sent_at < WATCH_REPATH) {
            return;
        }
        let here = unit.pos();
        let nearest = self
            .war
            .targets
            .values()
            .filter(|e| !e.is_dead && e.position != NULLPOS)
            .min_by(|a, b| distance(a.position, here).total_cmp(&distance(b.position, here)))
            .map(|e| (e.unit_id, e.position));
        let circles = self.taskforce_watch_circles(unit);
        let points = match nearest {
            Some((_, enemy)) if !self.retreat_handler.is_retreating(gun.id()) => {
                watch_points(gun.pos(), enemy, unit.def().map_or(0.0, |d| d.los_radius()))
            }
            _ => vec![gun.pos()],
        };
        let map = self.cb.map();
        let target = points
            .into_iter()
            .find(|p| {
                p.x >= 0.0
                    && p.z >= 0.0
                    && p.x < (map.width() * 8) as f32
                    && p.z < (map.height() * 8) as f32
                    && watch_clear(*p, &circles)
                    && self.pathfinder.is_reachable(&self.war, unit, *p, PathCost::AvoidEnemies)
            })
            .unwrap_or_else(|| self.graph_get_closest_haven(here));
        if let Some(sp) = self.taskforces.spotters.by_unit.get_mut(&id) {
            sp.gun = Some(gun.id());
            sp.sent = target;
            sp.sent_at = frame;
            sp.orbiting = false;
            sp.job = nearest.map_or(Job::Escorting, |(target, _)| Job::Spotting { gun: gun.id(), target, reload: 0 });
        }
        self.taskforce_watch_move(unit, target, &circles);
    }

    /// Repairs use the same enemy clearance as the outward journey.
    pub(crate) fn taskforce_watch_retreat(&mut self, id: i32, target: Vec3) -> bool {
        if !self.taskforces.spotters.by_unit.get(&id).is_some_and(|sp| sp.watch) {
            return false;
        }
        let Some(unit) = self.fighters.get(&id).map(|f| f.unit) else { return false };
        let circles = self.taskforce_watch_circles(unit);
        self.taskforce_watch_move(unit, target, &circles);
        true
    }

    fn taskforce_watch_move(&mut self, unit: Unit, mut target: Vec3, circles: &[(Vec3, f32)]) {
        let here = unit.pos();
        let cloaker = unit.def().is_some_and(|d| d.is_able_to_cloak());
        unit.set_fire_state(0, 0);
        unit.set_move_state(0, 0);
        unit.execute_custom_command(cmd::UNIT_CANCEL_TARGET, &[], 0, NO_TIMEOUT);
        unit.execute_custom_command(cmd::UNIT_AI, &[0.0], 0, NO_TIMEOUT);
        if cloaker {
            unit.cloak(true, 0);
            if watch_clear(here, circles)
                && self.cb.economy().current(self.energy) < 2.0 * unit.def().map_or(0.0, |d| d.cloak_cost_moving())
            {
                unit.stop(0);
                return;
            }
        }
        target.y = self.cb.map().elevation_at(target.x, target.z);
        if distance(here, target) < WATCH_ARRIVED && watch_clear(here, circles) {
            unit.stop(0);
            return;
        }
        let cost = if cloaker { PathCost::ScythePath } else { PathCost::AvoidEnemies };
        let path = self.pathfinder.find_path_keepout(&self.war, unit, target, cost, circles);
        let (mut from, mut length, mut ordered) = (here, 0.0, false);
        for p in path {
            if distance(from, p) < 1.0 {
                continue;
            }
            if !watch_safe_leg(from, p, circles) {
                break;
            }
            kgb_move_to(unit, p, if ordered { OPTION_SHIFT_KEY } else { 0 }, self.frame);
            ordered = true;
            length += distance(from, p);
            from = p;
            if length >= WATCH_LEG {
                break;
            }
        }
        if !ordered {
            unit.stop(0);
        }
    }

    /// Whether the enemy shows anti-air: seen, or felt (hits on our
    /// aircraft, see `air_intel`).
    pub fn taskforce_enemy_aa(&self) -> bool {
        self.war.targets.values().any(|e| !e.is_dead && (e.is_aa || e.is_flex_aa))
            || !self.air_intel_threat().is_empty()
    }

    /// Whether a unit of type `d` may spot now (a plane not against
    /// anti-air).
    fn taskforce_spotter_may(&self, spot: Spot, d: Option<UnitDef>) -> bool {
        spot.way(d) != Spot::Air || !self.taskforce_enemy_aa()
    }

    /// A unit just finished that some force short of a spotter takes:
    /// it joins (see the module notes). False: it goes its usual way.
    pub fn taskforce_spotter_claims(&mut self, unit: Unit, name: &str, cost: f32) -> bool {
        self.taskforce_load();
        if self.taskforces.spotters.released.contains(&unit.id()) {
            return false;
        }
        let tf = &self.taskforces;
        let wanted: Vec<(String, usize, Spot)> = (tf.doctrines.iter())
            .filter(|d| d.enabled && super::squad_aim(d.aim))
            .filter_map(|d| {
                let r = d.role_of(name, &tf.classes)?;
                d.roles[r].spot.map(|s| (d.name.clone(), r, s))
            })
            .collect();
        let short = wanted.iter().any(|(d, r, s)| {
            self.taskforce_spotter_may(*s, unit.def())
                && (self.squad_handler.taskforces.iter())
                    .filter(|&&sid| self.squads.get(sid).is_some_and(|q| &q.shape.doctrine == d))
                    .any(|&sid| self.squad_short_of(sid, *r))
        });
        if !short {
            return false;
        }
        if unit.def().is_some_and(|d| d.is_able_to_fly()) {
            unit.set_idle_mode(0, 0);
        }
        self.jaegers.order_filled(name, self.frame);
        self.squad_add_taskforce_member(Fighter::new(unit, cost, self.team_id));
        true
    }

    /// A spotter no force wants goes back to its usual handler, for good.
    pub(crate) fn taskforce_spotter_release(&mut self, id: i32) {
        self.widow_forget_bite(id);
        let Some(unit) = self.fighters.get(&id).map(|f| f.unit) else { return };
        let watching = self.taskforces.spotters.by_unit.remove(&id).is_some_and(|sp| sp.watch);
        self.taskforces.spotters.released.insert(id);
        self.squad_forget_fighter(id);
        unit.stop(0);
        if watching {
            self.retreat_remove_unit(unit);
            unit.set_fire_state(2, 0);
            unit.set_move_state(1, 0);
            unit.execute_custom_command(cmd::UNIT_AI, &[1.0], 0, NO_TIMEOUT);
            unit.execute_custom_command(cmd::DONT_FIRE_AT_RADAR, &[0.0], 0, NO_TIMEOUT);
        }
        self.war_unit_finished(unit);
        self.war_unit_idle(unit);
    }

    /// A unit for spotting built at a force's call (see `squad_produce`).
    pub(crate) fn taskforce_note_produced(&mut self, doctrine: &str, role: &str) {
        let spots = self.taskforces.doctrine(doctrine).and_then(|d| d.role(role)).is_some_and(|r| r.spot.is_some());
        if spots {
            self.taskforces.spotters.ordered += 1;
        }
    }

    /// Take `unit` from whatever handler has it, for a force: false if it is
    /// busy (retreating, home defense, another force).
    fn taskforce_detach(&mut self, unit: Unit) -> bool {
        let id = unit.id();
        if self.retreat_handler.is_retreating(id) || self.qrf_claims(id) || unit.is_being_built() {
            return false;
        }
        let in_force = self.fighters.get(&id).and_then(|f| f.squad).and_then(|s| self.squads.get(s));
        if in_force.is_some_and(|q| q.is_taskforce()) || self.taskforces.spotters.by_unit.contains_key(&id) {
            return false;
        }
        if self.squad_handler.fighters.contains(&id) {
            self.squad_forget_fighter(id);
        }
        self.raider_remove_unit(unit);
        self.jaeger_remove_unit(unit);
        self.scouts_release(id);
        self.war.aas.remove(&id);
        self.war.new_units.remove(&id);
        self.fighters.remove(&id);
        unit.stop(0);
        true
    }

    /// Force `sid`, short of its spotter, calls one from our standing units
    /// (see the module notes).
    fn taskforce_spotter_call(&mut self, sid: SquadId, role: usize) {
        let Some(s) = self.squads.get(sid) else { return };
        let Some(leader) = s.get_leader(&self.fighters) else { return };
        let Some(d) = self.taskforces.doctrine(&s.shape.doctrine) else { return };
        let r = &d.roles[role];
        let (spot, units) = (r.spot.unwrap_or(Spot::Any), r.units.clone());
        let (doctrine, state) = (d.name.clone(), force_state(s.engaging, s.engaged));
        let lpos = leader.pos();
        let standing = self.cb.team_units();
        for name in &units {
            let Some(def) = self.defs.get(name) else { continue };
            if !self.taskforce_spotter_may(spot, Some(def)) {
                continue;
            }
            let mut ours: Vec<Unit> =
                standing.iter().copied().filter(|u| u.def().is_some_and(|d| d.id() == def.id())).collect();
            ours.sort_by(|a, b| distance(a.pos(), lpos).total_cmp(&distance(b.pos(), lpos)));
            for unit in ours {
                if health_share(unit) <= spot.way(Some(def)).repair() || !self.taskforce_detach(unit) {
                    continue;
                }
                let id = unit.id();
                let mut f = Fighter::new(unit, def.cost(self.metal), self.team_id);
                f.shield_mob = true;
                self.fighters.insert(id, f);
                self.squad_handler.fighters.insert(id);
                self.retreat_remove_unit(unit);
                if def.is_able_to_fly() {
                    unit.set_idle_mode(0, 0);
                }
                let mut nav = nav!(self);
                if let Some(s) = self.squads.get_mut(sid) {
                    s.add_unit_nav(id, &mut self.fighters, &mut nav);
                }
                self.log(&format!(
                    "taskforce: a {name} is called to spot for the {doctrine} force (squad {sid}, {state}), {:.0} from \
                     its leader (unit {id})",
                    distance(unit.pos(), lpos)
                ));
                return;
            }
        }
    }

    /// The target a spotter should hold in sight for `gun`: its mark, or
    /// what it would mark were it in sight (see the module notes).
    fn taskforce_spot_target(&self, gun: &Gun, share: f32, shields: &[(Vec3, f32)]) -> Option<(i32, f32)> {
        let w = gun.unit.weapon(0)?;
        let damage = w.def().and_then(|d| d.damage_types().first().copied()).unwrap_or(0.0);
        let (pos, reach) = (gun.unit.pos(), w.range() + REACH);
        let marked = self.taskforces.marks.get(&gun.id).map(|m| m.target);
        if let Some(e) = marked.and_then(|t| self.war.targets.get(&t))
            && let Ok((worth, _)) = self.marksman_shot(e, damage, shields, Fix::Radar)
        {
            return Some((e.unit_id, worth));
        }
        let floor = self.marksman_floor(gun.unit, share);
        let taken: BTreeSet<i32> = self.taskforces.marks.values().map(|m| m.target).collect();
        (self.war.targets.values())
            .filter(|e| !e.can_fly() && e.def_name != "wolverine_mine" && !taken.contains(&e.unit_id))
            .filter(|e| distance(pos, e.position) <= reach)
            .filter_map(|e| self.marksman_shot(e, damage, shields, Fix::Radar).ok().map(|(w, _)| (e.unit_id, w)))
            .filter(|&(_, w)| w >= floor)
            .max_by(|a, b| a.1.total_cmp(&b.1))
    }

    /// Steer the spotters of force `sid` (after its conduct).
    pub(crate) fn taskforce_spot(&mut self, sid: SquadId) {
        let frame = self.frame;
        let Some(s) = self.squads.get(sid) else { return };
        let shape = s.shape.clone();
        let members: Vec<(i32, Unit, Spot, bool)> = (s.everyone().filter_map(|id| self.fighters.get(&id)))
            .filter_map(|f| {
                shape
                    .spot(f.unit.def().map(|d| d.id()))
                    .map(|sp| (f.id, f.unit, sp.way(f.unit.def()), sp == Spot::Watch))
            })
            .collect();
        // Spotters that left the force (other than for repairs) are done.
        let here: BTreeSet<i32> = members.iter().map(|m| m.0).collect();
        (self.taskforces.spotters.by_unit)
            .retain(|id, sp| sp.sid != sid || sp.job == Job::Repairing || here.contains(id));
        let Some(leader) = s.get_leader(&self.fighters) else { return };
        let lpos = leader.pos();
        let state = force_state(s.engaging, s.engaged);
        let guns: Vec<Gun> = (s.everyone().filter_map(|id| self.fighters.get(&id)))
            .filter(|f| shape.is_core(f.unit.def().map(|d| d.id())))
            .filter_map(|f| {
                let reload = f.unit.weapon(0)?.reload_frame();
                Some(Gun { id: f.id, unit: f.unit, next: reload.max(frame), reload })
            })
            .collect();
        if guns.is_empty() {
            for (id, _, _, watch) in members {
                if watch {
                    self.taskforce_spotter_release(id);
                }
            }
            return;
        }
        // A role may want several spotters. Repairing members reserve their
        // places too, so a new recruit cannot displace them.
        let role = shape.roles.iter().position(|r| r.spot.is_some());
        if let Some(role) = role
            && self.squad_short_of(sid, role)
        {
            self.taskforce_spotter_call(sid, role);
        }
        let watchers = members
            .iter()
            .filter(|m| m.3)
            .map(|m| m.0)
            .chain(
                self.taskforces
                    .spotters
                    .by_unit
                    .values()
                    .filter(|sp| sp.sid == sid && sp.watch && sp.job == Job::Repairing)
                    .map(|sp| sp.unit),
            )
            .map(|id| (id, self.taskforces.spotters.by_unit.get(&id).and_then(|sp| sp.gun)));
        let pairs = pair_guns(watchers, &guns.iter().map(|g| g.id).collect::<Vec<_>>());
        let shields = self.impalers_shields();
        let share = shape.mark_share;
        let orbit = |unit: Unit| shape.orbit(unit.def().map(|d| d.id()));
        let mut lines = Vec::new();
        for (id, unit, spot, watch) in members {
            let name = self.defs.name_of(unit).to_owned();
            let entry = self.taskforces.spotters.by_unit.get(&id);
            if entry.is_none_or(|sp| sp.sid != sid || sp.job == Job::Repairing) {
                lines.push(format!(
                    "taskforce: a {name} spots for the {} force (squad {sid}, {state}), {:.0} from its leader (unit {id})",
                    shape.doctrine,
                    distance(unit.pos(), lpos)
                ));
                let sp = Spotter {
                    unit: id,
                    sid,
                    spot,
                    job: Job::Escorting,
                    watch,
                    gun: pairs.get(&id).copied(),
                    since: frame,
                    sent: NULLPOS,
                    sent_at: i32::MIN / 2,
                    orbiting: true,
                    unknown: false,
                    sighted: false,
                };
                self.taskforces.spotters.by_unit.insert(id, sp);
            }
            match self.widow_opportunistic_bite(unit, health_share(unit) >= spot.repair()) {
                crate::military::jaeger::BiteAction::Active => continue,
                crate::military::jaeger::BiteAction::Resume => {
                    if let Some(sp) = self.taskforces.spotters.by_unit.get_mut(&id) {
                        sp.job = Job::Escorting;
                        sp.sent_at = i32::MIN / 2;
                        sp.sent = NULLPOS;
                        sp.orbiting = false;
                    }
                }
                crate::military::jaeger::BiteAction::Idle => {}
            }
            if health_share(unit) < spot.repair() {
                lines.push(format!(
                    "taskforce: a {name} spotter of the {} force (squad {sid}, {state}) goes for repairs at {:.0}% \
                     health, {:.0} from the force",
                    shape.doctrine,
                    100.0 * health_share(unit),
                    distance(unit.pos(), lpos)
                ));
                self.taskforce_spotter_away(id, unit);
                continue;
            }
            if watch {
                let Some(gun) = pairs.get(&id).and_then(|g| guns.iter().find(|x| x.id == *g)) else {
                    self.taskforce_spotter_release(id);
                    continue;
                };
                self.taskforce_watch(id, unit, gun.unit);
                continue;
            }
            // The gun that fires next, and what to see for it.
            let Some(gun) = guns.iter().min_by_key(|g| g.next) else { continue };
            let target = self.taskforce_spot_target(gun, share, &shields);
            let Some(sp) = self.taskforces.spotters.by_unit.get_mut(&id) else { continue };
            if let Job::Spotting { gun: g, target: t, reload } = sp.job {
                let fired = guns.iter().find(|x| x.id == g).is_none_or(|x| x.reload != reload && x.reload > frame);
                let gone = self.war.targets.get(&t).is_none_or(|e| e.is_dead);
                if fired || gone || frame - sp.since > ERRAND_FRAMES {
                    sp.job = Job::Escorting;
                    sp.since = frame;
                    sp.orbiting = false;
                } else if let Some(e) = self.war.targets.get(&t).filter(|e| e.visible && !sp.sighted) {
                    sp.sighted = true;
                    lines.push(format!(
                        "taskforce: a {name} spotting for the {} force (squad {sid}) {} a {} ({:.0} metal), {:.0} from \
                         the guns",
                        shape.doctrine,
                        if sp.unknown { "identifies" } else { "sights" },
                        e.def_name,
                        e.ud.map_or(e.value, |d| d.cost(self.metal)),
                        distance(e.position, gun.unit.pos())
                    ));
                }
            }
            if sp.job == Job::Escorting
                && let Some((t, worth)) = target
                && let Some(e) = self.war.targets.get(&t)
            {
                let to = spot_point(spot, unit.def(), e.position, gun.unit.pos());
                let speed = unit.def().map_or(60.0, |d| d.speed()).max(1.0);
                let need = (30.0 * (distance(unit.pos(), to) / speed + spot.lead())) as i32;
                if gun.next - frame <= need.min(MAX_LEAD) {
                    sp.job = Job::Spotting { gun: gun.id, target: t, reload: gun.reload };
                    sp.since = frame;
                    sp.sent = NULLPOS;
                    sp.unknown = !e.identified;
                    sp.sighted = e.visible;
                    lines.push(format!(
                        "taskforce: a {name} spotting for the {} force (squad {sid}) goes to see a {} ({worth:.0} metal, \
                         {}) for a {}'s volley in {:.0} s, {:.0} away",
                        shape.doctrine,
                        e.def_name,
                        if e.visible { "in sight" } else { "on radar" },
                        self.defs.name_of(gun.unit),
                        (gun.next - frame) as f32 / 30.0,
                        distance(unit.pos(), to)
                    ));
                }
            }
            match sp.job {
                Job::Spotting { gun: g, target: t, .. } => {
                    let Some(e) = self.war.targets.get(&t) else { continue };
                    let gpos = guns.iter().find(|x| x.id == g).map_or(lpos, |x| x.unit.pos());
                    let to = spot_point(spot, unit.def(), e.position, gpos);
                    if distance(to, sp.sent) > RESEND_MOVED || frame - sp.sent_at > RESEND_FRAMES {
                        sp.sent = to;
                        sp.sent_at = frame;
                        sp.orbiting = false;
                        if spot == Spot::Air {
                            unit.move_to(to, 0);
                        } else {
                            kgb_move_to(unit, to, 0, frame);
                        }
                    }
                }
                Job::Escorting => {
                    let back = !sp.orbiting || distance(unit.pos(), lpos) > STRAYED;
                    if back && frame - sp.sent_at > RESEND_FRAMES {
                        sp.orbiting = true;
                        sp.sent_at = frame;
                        if spot == Spot::Air {
                            unit.move_to(lpos, 0);
                        } else {
                            let lid = leader.id() as f32;
                            unit.execute_custom_command(cmd::ORBIT, &[lid, orbit(unit)], 0, NO_TIMEOUT);
                            unit.execute_custom_command(cmd::ORBIT_DRAW, &[lid], OPTION_SHIFT_KEY, NO_TIMEOUT);
                        }
                    }
                }
                Job::Repairing => {}
            }
        }
        for line in lines {
            self.log(&line);
        }
    }

    /// Spotter `id` goes for repairs, or flees: out of its force for good.
    fn taskforce_spotter_away(&mut self, id: i32, unit: Unit) {
        self.widow_forget_bite(id);
        if let Some(sp) = self.taskforces.spotters.by_unit.get_mut(&id) {
            sp.job = Job::Repairing;
            sp.since = self.frame;
        }
        self.retreat_send(unit);
    }

    /// A spotter plane fired on flees home (see the module notes).
    pub fn taskforce_spotter_damaged(&mut self, unit: Unit) {
        let id = unit.id();
        let Some(sp) = self.taskforces.spotters.by_unit.get(&id) else { return };
        if sp.spot != Spot::Air || sp.job == Job::Repairing {
            return;
        }
        let sid = sp.sid;
        let force = self.squads.get(sid).filter(|s| s.is_taskforce());
        let (doctrine, state, far) = match force {
            Some(s) => (
                s.shape.doctrine.clone(),
                force_state(s.engaging, s.engaged),
                s.get_leader(&self.fighters).map_or(0.0, |l| distance(l.pos(), unit.pos())),
            ),
            None => ("gone".to_owned(), "gone", 0.0),
        };
        self.log(&format!(
            "taskforce: a {} spotter of the {doctrine} force (squad {sid}, {state}) flees anti-air fire, {far:.0} from \
             the force",
            self.defs.name_of(unit)
        ));
        self.taskforce_spotter_away(id, unit);
    }

    /// A spotter's death, in its force or gone for repairs.
    pub fn taskforce_spotter_lost(&mut self, unit: Unit, attacker: Option<Unit>) {
        let Some(sp) = self.taskforces.spotters.by_unit.remove(&unit.id()) else { return };
        let by = attacker.and_then(|a| a.def()).map_or("?".to_owned(), |d| self.defs.name(d).to_owned());
        let force = self.squads.get(sp.sid).filter(|s| s.is_taskforce());
        let (doctrine, state, far) = match force {
            Some(s) => (
                s.shape.doctrine.clone(),
                force_state(s.engaging, s.engaged),
                s.get_leader(&self.fighters)
                    .map_or("-".to_owned(), |l| format!("{:.0}", distance(l.pos(), unit.pos()))),
            ),
            None => ("gone".to_owned(), "gone", "-".to_owned()),
        };
        self.log(&format!(
            "taskforce: a {} spotter of the {doctrine} force (squad {}, {state}) is lost {} to a {by}, {far} from the \
             force",
            self.defs.name_of(unit),
            sp.sid,
            sp.doing()
        ));
    }

    /// What spotter `id` is doing, for the death line (`None`: no spotter).
    pub fn taskforce_spotter_doing(&self, id: i32) -> Option<&'static str> {
        self.taskforces.spotters.by_unit.get(&id).map(Spotter::doing)
    }
}

/// A force's state for the log: forming (never engaged), engaged, rallying.
pub fn force_state(engaging: bool, engaged: i32) -> &'static str {
    match (engaging, engaged) {
        (true, _) => "engaged",
        (false, 0) => "forming",
        _ => "rallying",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn watchers_keep_distinct_guns_and_repair_reservations() {
        let pairs = pair_guns([(1, Some(20)), (2, None), (3, Some(10))], &[10, 20]);
        assert_eq!(pairs, BTreeMap::from([(1, 20), (3, 10)]));
        // A dead gun releases its assignment; survivors keep theirs.
        let pairs = pair_guns([(1, Some(20)), (2, Some(99)), (3, None)], &[10, 20, 30]);
        assert_eq!(pairs, BTreeMap::from([(1, 20), (2, 10), (3, 30)]));
        assert!(pair_guns([(1, Some(20))], &[]).is_empty());
    }

    #[test]
    fn watchers_use_their_own_sight_edge_even_when_enemy_is_close() {
        let (gun, enemy) = (Vec3::xz(1000.0, 1000.0), Vec3::xz(3000.0, 1000.0));
        for sight in [425.0, 550.0, 660.0, 760.0] {
            let points = watch_points(gun, enemy, sight);
            assert!((points[0].x - (enemy.x - sight + WATCH_MARGIN)).abs() < 0.01);
            for p in points {
                assert!((distance(p, enemy) - (sight - WATCH_MARGIN)).abs() < 0.01);
                assert!(distance(p, enemy) + WATCH_ARRIVED < sight);
                assert!(p.x <= enemy.x + 0.01);
            }
        }
        let edge = watch_points(gun, enemy, 425.0)[0];
        let moved_gun = watch_points(Vec3::xz(2000.0, 1000.0), enemy, 425.0)[0];
        assert!(distance(edge, moved_gun) < 0.01);
        let close = Vec3::xz(1000.0, 1100.0);
        let edge = watch_points(gun, close, 660.0)[0];
        assert!(edge.z < gun.z);
        assert!((distance(edge, close) - (660.0 - WATCH_MARGIN)).abs() < 0.01);
    }

    #[test]
    fn watchers_avoid_decloak_circles_and_only_escape_outwards() {
        let points = watch_points(Vec3::xz(1000.0, 1000.0), Vec3::xz(2000.0, 1000.0), 660.0);
        let circles = [(points[0], 200.0)];
        assert!(!watch_clear(points[0], &circles));
        assert!(points.iter().any(|&p| watch_clear(p, &circles)));
        let circles = [(Vec3::xz(500.0, 500.0), 125.0)];
        assert!(!watch_safe_leg(Vec3::xz(0.0, 500.0), Vec3::xz(1000.0, 500.0), &circles));
        assert!(watch_safe_leg(Vec3::xz(0.0, 250.0), Vec3::xz(1000.0, 250.0), &circles));
        assert!(watch_safe_leg(Vec3::xz(550.0, 500.0), Vec3::xz(700.0, 500.0), &circles));
        assert!(!watch_safe_leg(Vec3::xz(550.0, 500.0), Vec3::xz(510.0, 500.0), &circles));
        assert!(!watch_safe_leg(Vec3::xz(550.0, 500.0), Vec3::xz(300.0, 500.0), &circles));
    }

    #[test]
    fn spots_parse() {
        assert_eq!(Spot::parse("cloak"), Ok(Spot::Cloak));
        assert_eq!(Spot::parse("any"), Ok(Spot::Any));
        assert_eq!(Spot::parse("watch"), Ok(Spot::Watch));
        assert!(Spot::parse("boat").unwrap_err().contains("tank, cloak, air, any, watch"));
    }

    #[test]
    fn spot_points_stand_off_toward_the_gun() {
        let (at, gun) = (Vec3::xz(1000.0, 0.0), Vec3::xz(0.0, 0.0));
        let p = spot_point(Spot::Tank, None, at, gun);
        assert!((p.x - 760.0).abs() < 1.0 && p.z.abs() < 1.0, "{p:?}");
        assert_eq!(spot_point(Spot::Air, None, at, gun), at);
        // Closer than the stand-off: stay with the gun.
        assert_eq!(spot_point(Spot::Tank, None, Vec3::xz(100.0, 0.0), gun), gun);
    }
}
