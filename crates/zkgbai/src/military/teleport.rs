//! Teleporting with a Djinn (amphibious teleport bridge): units that reach
//! its beacon are teleported to wherever the Djinn stands deployed. The
//! Djinn puts its beacon down from afar, but must itself get to the exit,
//! so it works two ways:
//!
//! - As a gate (by default): the beacon by our factory, the Djinn at the
//!   front where our squads gather, on our side of it. Squads forming and
//!   rallying from home go through when that beats walking; the Djinn
//!   follows the front.
//! - For an insertion: the Djinn deployed at an exit behind the enemy's
//!   lines (where a teleport saves our forces the most travel to the
//!   enemy's value, see [`Analysis::teleports`](zk_terrain::strategy::Analysis)),
//!   the beacon where our squads gather. It cannot walk there through the
//!   enemy, so it goes only when a Charon can fly it in on a way clear of
//!   anti-air (see `drops`).
//!
//! Squads near the beacon go through when the best target from the exit,
//! all told, beats their own. A Djinn in danger falls back to a refuge and
//! makes a new plan once safe.

use std::collections::BTreeMap;

use springai::{NO_TIMEOUT, Unit, Vec3};
use zk_terrain::strategy::ground_distance;

use crate::Ai;
use crate::kgbutil::{PathCost, kgb_move_to};
use crate::military::SquadId;
use crate::military::drops::Ferried;
use crate::nav;
use crate::terrain::{pos, tier_of};
use crate::util::{NULLPOS, OPTION_SHIFT_KEY, distance, get_directional_point};

/// Custom commands of Zero-K's teleporter gadget.
const CMD_PLACE_BEACON: i32 = 35170;
const CMD_WAIT_AT_BEACON: i32 = 35171;

/// Frames between updates.
const PERIOD: i32 = 30;

/// A Djinn is worth building from this income and army (fighter value) on.
const MIN_INCOME: f32 = 25.0;
const MIN_ARMY: f32 = 2000.0;

/// An insertion exit must save at least this share of the travel to the
/// enemy's value, all told.
const MIN_SHARE: f32 = 0.25;

/// A gate is worth it where the front is this far from home, elmos; a
/// squad goes through it if that takes at most this share of the walk.
const GATE_MIN: f32 = 2500.0;
const GATE_SHARE: f32 = 0.6;

/// Frames before a lost Djinn is replaced, and before an unfulfilled
/// production order is forgotten.
const REBUILD_DELAY: i32 = 30 * 180;
const ORDER_TIMEOUT: i32 = 30 * 120;

/// Frames to wait for the beacon to appear (it takes 8 s), and to sit in a
/// refuge before making a new plan.
const BEACON_TIMEOUT: i32 = 30 * 20;
const REFUGE_WAIT: i32 = 30 * 10;

/// A beacon this close to where it should be will do, elmos.
const BEACON_SLACK: f32 = 600.0;

/// Squads this far from the beacon (by ground, elmos) may go through it.
const LAUNCH_RANGE: f32 = 1500.0;

/// Mass the Djinn teleports per second (its `teleporter_throughput`).
const THROUGHPUT: f32 = 150.0;

/// Teleported units land this close to the Djinn.
const LANDED: f32 = 500.0;

/// How close to its exit the Djinn stops, elmos.
const ARRIVED: f32 = 150.0;

/// Frames an exit the Djinn was driven from is avoided.
const EXIT_BAN: i32 = 30 * 180;

/// A gate follows the front once it is this far from the Djinn (elmos), a
/// beacon follows the squads' gathering as far, at most every
/// [`RESTAGE_PERIOD`] frames; squads gather this far behind the front.
const RESTAGE_DISTANCE: f32 = 1500.0;
const RESTAGE_PERIOD: i32 = 30 * 60;
const STAGING_SETBACK: f32 = 300.0;

/// The beacon stands this far from the factory, towards the front.
const BEACON_OFFSET: f32 = 250.0;

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    /// Beacon at home, Djinn at the front.
    Gate,
    /// Beacon where squads gather, Djinn flown behind the enemy's lines.
    Insertion,
}

#[derive(Clone, Copy)]
enum Phase {
    /// Making its beacon for `mode`.
    Beacon { since: i32, mode: Mode },
    /// Walking to the front.
    Travel { exit: Vec3, since: i32 },
    /// Being flown to an exit.
    Airlift { exit: Vec3 },
    /// Standing at its exit, deployed.
    Deployed { since: i32 },
    /// Standing at its exit, moving its beacon.
    Restage { since: i32 },
    /// Fallen back from danger, or waiting for something to do.
    Refuge { since: i32 },
}

struct Djinn {
    unit: Unit,
    beacon: Option<Unit>,
    phase: Phase,
    mode: Mode,
}

/// The teleport bridge, if we have one, and the squads going through it.
#[derive(Default)]
pub struct Teleports {
    djinn: Option<Djinn>,
    /// When a Djinn was queued at a factory.
    ordered: Option<i32>,
    rebuild_after: i32,
    /// Squads on their way through the beacon, and since when.
    squads: BTreeMap<SquadId, i32>,
    /// Exits the Djinn was driven from, and until when to avoid them.
    banned: Vec<(Vec3, i32)>,
}

impl Ai {
    /// Whether an amphibious factory should build a Djinn now.
    pub fn teleport_wants_djinn(&self) -> bool {
        let t = &self.teleports;
        !self.dev_off("teleport")
            && t.djinn.is_none()
            && t.ordered.is_none_or(|f| self.frame - f > ORDER_TIMEOUT)
            && self.frame >= t.rebuild_after
            && self.eco.adjusted_income >= MIN_INCOME
            && self.fac.fighter_value >= MIN_ARMY
            && (self.teleport_gate_exit().is_some() || self.teleport_best_exits().next().is_some())
    }

    /// A factory has been told to build a Djinn.
    pub fn teleport_ordered(&mut self) {
        self.teleports.ordered = Some(self.frame);
    }

    /// A Djinn is ready: make a plan.
    pub fn teleport_add_djinn(&mut self, unit: Unit) {
        if self.teleports.djinn.is_some() {
            return;
        }
        self.teleports.ordered = None;
        let phase = Phase::Refuge { since: self.frame - REFUGE_WAIT };
        self.teleports.djinn = Some(Djinn { unit, beacon: None, phase, mode: Mode::Gate });
    }

    /// Where our squads gather: behind the front-line metal spot nearest
    /// our base (at our base while there is no front).
    fn teleport_staging(&self) -> Option<Vec3> {
        let home = self.graph.get_ally_center();
        if home == NULLPOS {
            return None;
        }
        Some(match self.graph.get_closest_front_line_spot(home) {
            Some(s) if distance(s.position, home) > STAGING_SETBACK => {
                get_directional_point(s.position, home, STAGING_SETBACK)
            }
            _ => home,
        })
    }

    /// Home, where new units come from: by the factory nearest our base,
    /// towards the front.
    fn teleport_home(&self) -> Option<Vec3> {
        let base = self.graph.get_ally_center();
        let staging = self.teleport_staging()?;
        let factory = (self.fac.factories.values().map(|f| f.get_pos()))
            .min_by(|a, b| distance(*a, base).total_cmp(&distance(*b, base)))
            .unwrap_or(base);
        Some(get_directional_point(factory, staging, BEACON_OFFSET))
    }

    fn teleport_banned(&self, p: Vec3) -> bool {
        self.teleports.banned.iter().any(|&(b, until)| until > self.frame && distance(b, p) < 500.0)
    }

    /// The gate's exit: the front where our squads gather, when it is far
    /// from home and ours.
    fn teleport_gate_exit(&self) -> Option<Vec3> {
        let (home, staging) = (self.teleport_home()?, self.teleport_staging()?);
        let safe = self.war.get_effective_threat(staging) <= 0.0
            && !self.incursion_imminent(staging, 0.0)
            && !self.teleport_banned(staging);
        (safe && distance(home, staging) >= GATE_MIN).then_some(staging)
    }

    /// Insertion exits worth it, best first, that the enemy is not
    /// threatening or about to overrun.
    fn teleport_best_exits(&self) -> impl Iterator<Item = Vec3> + '_ {
        let s = self.strategy.as_ref();
        let map = s.and_then(|s| self.terrain()?.tier(s.tier));
        let total: f32 = s.map_or(0.0, |s| s.analysis.values.iter().map(|v| v.enemy).sum());
        let elevation = self.cb.map();
        (s.into_iter().flat_map(|s| s.analysis.teleports.iter()))
            .take_while(move |t| t.gain >= MIN_SHARE * total)
            .filter_map(move |t| {
                let c = map?.regions[t.region].center;
                Some(Vec3::new(c.x, elevation.elevation_at(c.x, c.z), c.z))
            })
            .filter(|&exit| {
                self.war.get_threat(exit) <= 0.0 && !self.incursion_imminent(exit, 0.0) && !self.teleport_banned(exit)
            })
    }

    /// An insertion exit a Charon can fly the Djinn at `from` to now.
    fn teleport_insertion_exit(&self, from: Vec3) -> Option<Vec3> {
        self.drops_free_charon()?;
        self.teleport_best_exits().find(|&exit| self.drops_clear_sky(from, exit))
    }

    /// Order the beacon down at a free spot by `at`.
    fn teleport_place_beacon(&mut self, unit: Unit, at: Vec3) {
        let Some(def) = self.defs.get("tele_beacon") else { return };
        let site = self.cb.map().find_closest_build_site(def, at, 600.0, 2, 0);
        if site.x >= 0.0 {
            unit.execute_custom_command(CMD_PLACE_BEACON, &[site.x, site.y, site.z], 0, NO_TIMEOUT);
            self.log(&format!("teleport: Djinn placing its beacon at ({:.0}, {:.0})", site.x, site.z));
        }
    }

    /// Where the beacon should be for `mode`.
    fn teleport_beacon_site(&self, mode: Mode) -> Option<Vec3> {
        match mode {
            Mode::Gate => self.teleport_home(),
            Mode::Insertion => self.teleport_staging(),
        }
    }

    /// Decide what to do: fly in behind the enemy's lines if a Charon can
    /// take the Djinn, else be a gate if the front is far, else wait. The
    /// beacon goes where the plan wants it first.
    fn teleport_plan(&mut self, unit: Unit) {
        let frame = self.frame;
        let mode = if self.teleport_insertion_exit(unit.pos()).is_some() { Mode::Insertion } else { Mode::Gate };
        if mode == Mode::Gate && self.teleport_gate_exit().is_none() {
            self.teleport_set_phase(Phase::Refuge { since: frame });
            return;
        }
        let Some(site) = self.teleport_beacon_site(mode) else { return };
        let beacon = self.teleports.djinn.as_ref().and_then(|d| d.beacon).map(|b| b.pos());
        if beacon.is_some_and(|b| distance(b, site) <= BEACON_SLACK) {
            self.teleport_go(unit, mode);
        } else {
            self.teleport_set_phase(Phase::Beacon { since: frame, mode });
            self.teleport_place_beacon(unit, site);
        }
    }

    /// With the beacon in place, set off in `mode`.
    fn teleport_go(&mut self, unit: Unit, mode: Mode) {
        let frame = self.frame;
        if let Some(d) = self.teleports.djinn.as_mut() {
            d.mode = mode;
        }
        match mode {
            Mode::Insertion => match self.teleport_insertion_exit(unit.pos()) {
                Some(exit) if self.drops_ferry(unit, exit) => {
                    self.teleport_set_phase(Phase::Airlift { exit });
                    self.log(&format!("teleport: Djinn flown to an exit at ({:.0}, {:.0})", exit.x, exit.z));
                }
                _ => self.teleport_set_phase(Phase::Refuge { since: frame }),
            },
            Mode::Gate => match self.teleport_gate_exit() {
                Some(exit) => {
                    self.teleport_set_phase(Phase::Travel { exit, since: frame });
                    self.log(&format!("teleport: Djinn heading for the front at ({:.0}, {:.0})", exit.x, exit.z));
                    self.teleport_move(unit, exit);
                }
                None => self.teleport_set_phase(Phase::Refuge { since: frame }),
            },
        }
    }

    pub fn teleport_update(&mut self, frame: i32) {
        if (frame + self.offset + 17) % PERIOD != 0 {
            return;
        }
        let Some(d) = &self.teleports.djinn else { return };
        let unit = d.unit;
        if unit.health() <= 0.0 || unit.team() != self.team_id {
            self.log("teleport: Djinn lost");
            self.teleports = Teleports { rebuild_after: frame + REBUILD_DELAY, ..Teleports::default() };
            return;
        }

        // The beacon: ours is the one linked to this Djinn.
        if d.beacon.is_none_or(|b| b.health() <= 0.0) {
            let id = unit.id() as f32;
            let beacon = self.cb.team_units().into_iter().find(|b| {
                b.def().is_some_and(|def| self.defs.name(def) == "tele_beacon")
                    && b.rules_param_float("connectto", -1.0) == id
            });
            if let Some(d) = self.teleports.djinn.as_mut() {
                d.beacon = beacon;
            }
        }

        let here = unit.pos();
        let threatened = self.war.get_threat(here) > 0.0;
        let Some((phase, mode, beacon)) =
            self.teleports.djinn.as_ref().map(|d| (d.phase, d.mode, d.beacon.map(|b| b.pos())))
        else {
            return;
        };
        match phase {
            Phase::Refuge { since } if threatened || frame - since < REFUGE_WAIT => {}
            Phase::Refuge { .. } => self.teleport_plan(unit),
            Phase::Beacon { since, mode } => {
                let site = self.teleport_beacon_site(mode);
                let placed = beacon.zip(site).is_some_and(|(b, s)| distance(b, s) <= BEACON_SLACK);
                if placed {
                    self.teleport_go(unit, mode);
                } else if frame - since > BEACON_TIMEOUT {
                    self.teleport_plan(unit);
                }
            }
            Phase::Travel { exit, since } => {
                if threatened || self.war.get_threat(exit) > 0.0 {
                    self.teleports.banned.push((exit, frame + EXIT_BAN));
                    self.teleport_fall_back(unit);
                } else if distance(here, exit) <= ARRIVED {
                    self.teleport_deploy(unit);
                } else if frame - since >= 30 * 5 {
                    self.teleport_set_phase(Phase::Travel { exit, since: frame });
                    self.teleport_move(unit, exit);
                }
            }
            Phase::Airlift { exit } => match self.drops_ferried(unit) {
                Some(Ferried::Underway) => {}
                Some(Ferried::Landed(_)) => self.teleport_deploy(unit),
                _ => {
                    self.teleports.banned.push((exit, frame + EXIT_BAN));
                    self.teleport_fall_back(unit);
                }
            },
            Phase::Deployed { .. } | Phase::Restage { .. } if threatened => {
                self.teleports.banned.push((here, frame + EXIT_BAN));
                self.teleport_fall_back(unit);
            }
            Phase::Deployed { since } if frame - since >= RESTAGE_PERIOD && self.teleports.squads.is_empty() => {
                let staging = self.teleport_staging();
                match mode {
                    // Follow the front, or fly in if a Charon can now.
                    Mode::Gate => {
                        let moved = staging.is_some_and(|s| distance(s, here) > RESTAGE_DISTANCE);
                        if moved || beacon.is_none() || self.teleport_insertion_exit(here).is_some() {
                            self.teleport_plan(unit);
                        }
                    }
                    // Keep the beacon where the squads gather.
                    Mode::Insertion => {
                        if let Some(staging) = staging
                            && beacon.is_none_or(|b| distance(b, staging) > RESTAGE_DISTANCE)
                        {
                            self.teleport_set_phase(Phase::Restage { since: frame });
                            self.teleport_place_beacon(unit, staging);
                        }
                    }
                }
            }
            Phase::Deployed { .. } => {}
            Phase::Restage { since } if frame - since > BEACON_TIMEOUT => {
                self.teleport_set_phase(Phase::Deployed { since: frame });
            }
            Phase::Restage { .. } => {}
        }
    }

    fn teleport_set_phase(&mut self, phase: Phase) {
        if let Some(d) = self.teleports.djinn.as_mut() {
            d.phase = phase;
        }
    }

    /// Standing still deploys it.
    fn teleport_deploy(&mut self, unit: Unit) {
        unit.stop(0);
        self.teleport_set_phase(Phase::Deployed { since: self.frame });
        let here = unit.pos();
        let how = match self.teleports.djinn.as_ref().map(|d| d.mode) {
            Some(Mode::Insertion) => "behind the enemy's lines",
            _ => "as a gate at the front",
        };
        self.log(&format!("teleport: Djinn deployed {how} at ({:.0}, {:.0})", here.x, here.z));
    }

    /// Fall back from danger: to a refuge if there is one, else home.
    fn teleport_fall_back(&mut self, unit: Unit) {
        let to = self.incursion_refuge(unit).unwrap_or_else(|| self.graph_get_closest_haven(unit.pos()));
        self.teleport_set_phase(Phase::Refuge { since: self.frame });
        self.log("teleport: Djinn falling back");
        self.teleport_move(unit, to);
    }

    /// Walk the Djinn along a path that avoids the enemy.
    fn teleport_move(&mut self, unit: Unit, to: Vec3) {
        let frame = self.frame;
        let nav = nav!(self);
        let mut path = nav.pathfinder.find_path(nav.war, unit, to, PathCost::AvoidEnemies);
        let Some(first) = path.pop_front() else { return };
        kgb_move_to(unit, first, 0, frame);
        // Every other waypoint is close enough.
        while let Some(p) = path.pop_front() {
            if path.len().is_multiple_of(2) {
                kgb_move_to(unit, p, OPTION_SHIFT_KEY, frame);
            }
        }
    }

    /// The deployed Djinn, its beacon and its mode, if any.
    fn teleport_bridge(&self) -> Option<(Unit, Unit, Mode)> {
        let d = self.teleports.djinn.as_ref().filter(|d| matches!(d.phase, Phase::Deployed { .. }))?;
        Some((d.unit, d.beacon?, d.mode))
    }

    /// Whether squad `sid` is on its way through the beacon (its members
    /// are not to be given other orders meanwhile).
    pub fn teleport_busy(&mut self, sid: SquadId) -> bool {
        let Some(&since) = self.teleports.squads.get(&sid) else { return false };
        let djinn = self.teleports.djinn.as_ref().filter(|d| matches!(d.phase, Phase::Deployed { .. }));
        let at = djinn.map(|d| d.unit.pos());
        let members: Vec<Unit> = self
            .squads
            .get(sid)
            .map(|s| s.fighters.iter().filter_map(|id| self.fighters.get(id)).map(|f| f.unit).collect())
            .unwrap_or_default();
        let mass: f32 = members.iter().filter_map(|u| u.def()).map(|d| d.mass()).sum();
        let timeout = 30.0 * (30.0 + 2.0 * mass / THROUGHPUT);
        let through = at.is_some_and(|at| members.iter().all(|u| distance(u.pos(), at) < LANDED));
        if at.is_none() || through || (self.frame - since) as f32 > timeout {
            self.teleports.squads.remove(&sid);
            return false;
        }
        true
    }

    /// Squad `sid`'s members, their ground tier's map, the slowest speed
    /// and the time they take to go through.
    fn teleport_squad(&self, sid: SquadId) -> Option<(Vec<Unit>, &zk_terrain::TierMap, f32, f32)> {
        let s = self.squads.get(sid).filter(|s| !s.is_air_squad)?;
        let members: Vec<Unit> = s.fighters.iter().filter_map(|id| self.fighters.get(id)).map(|f| f.unit).collect();
        let defs: Vec<_> = members.iter().filter_map(|u| u.def()).collect();
        let map = self.terrain()?.tier(tier_of(*defs.first()?)?)?;
        let speed = defs.iter().map(|d| d.speed()).fold(f32::INFINITY, f32::min);
        let wait = defs.iter().map(|d| d.mass()).sum::<f32>() / THROUGHPUT;
        Some((members, map, speed, wait))
    }

    fn teleport_send(&mut self, sid: SquadId, members: &[Unit], beacon: Unit) {
        for u in members {
            u.execute_custom_command(CMD_WAIT_AT_BEACON, &[beacon.id() as f32], 0, NO_TIMEOUT);
        }
        self.teleports.squads.insert(sid, self.frame);
    }

    /// Send squad `sid` at `from`, on its way to `rally`, through a gate if
    /// that takes much less than walking; whether it was.
    pub fn teleport_gate(&mut self, sid: SquadId, from: Vec3, rally: Vec3) -> bool {
        let Some((djinn, beacon, Mode::Gate)) = self.teleport_bridge() else { return false };
        if rally == NULLPOS {
            return false;
        }
        let Some((members, map, speed, wait)) = self.teleport_squad(sid) else { return false };
        let (exit, b) = (pos(djinn.pos()), pos(beacon.pos()));
        let (Some(walk), Some(to_beacon)) =
            (ground_distance(map, pos(from), pos(rally)), ground_distance(map, pos(from), b))
        else {
            return false;
        };
        let Some(beyond) = ground_distance(map, exit, pos(rally)) else { return false };
        let through = to_beacon + wait * speed + beyond;
        if to_beacon > LAUNCH_RANGE || through > GATE_SHARE * walk {
            return false;
        }
        self.teleport_send(sid, &members, beacon);
        self.log(&format!(
            "teleport: squad {sid} ({} units) through the gate: {through:.0} against a walk of {walk:.0}",
            members.len()
        ));
        true
    }

    /// Send squad `sid` at `from` through the beacon if the best target
    /// from the Djinn's exit, counting the way to the beacon and the wait
    /// there, beats its own best target (at `cost`, as
    /// [`war_best_target`](Ai::war_best_target) scores them); whether it was.
    pub fn teleport_shortcut(&mut self, sid: SquadId, from: Vec3, cost: f32) -> bool {
        let Some((djinn, beacon, _)) = self.teleport_bridge() else { return false };
        let exit = djinn.pos();
        let Some(leader) = self.squads.get(sid).and_then(|s| s.get_leader(&self.fighters)) else { return false };
        let Some((members, map, speed, wait)) = self.teleport_squad(sid) else { return false };
        let Some(to_beacon) = ground_distance(map, pos(from), pos(beacon.pos())).filter(|&d| d <= LAUNCH_RANGE) else {
            return false;
        };
        let (target, exit_cost) = self.war_best_target(leader, exit, true);
        // Target costs count distance at a quarter for the most wanted
        // targets.
        let overhead = (to_beacon + wait * speed) / 4.0;
        if target == NULLPOS || exit_cost + overhead >= cost {
            return false;
        }
        self.teleport_send(sid, &members, beacon);
        self.log(&format!(
            "teleport: squad {sid} ({} units) through the beacon for ({:.0}, {:.0}): cost {:.0} against {cost:.0}",
            members.len(),
            target.x,
            target.z,
            exit_cost + overhead
        ));
        true
    }
}
