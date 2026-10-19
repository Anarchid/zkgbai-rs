//! Teleport assaults with a Djinn (amphibious teleport bridge): units that
//! reach its beacon are teleported to wherever the Djinn stands deployed.
//!
//! With an amphibious factory, an army and a good exit on the map (where
//! a teleport saves our forces the most travel to the enemy's value, see
//! [`Analysis::teleports`](zk_terrain::strategy::Analysis)), the AI builds
//! one Djinn. It puts its beacon down where our squads gather (behind our
//! front line), then wades to the best exit it can reach that the enemy is
//! not threatening or about to overrun, and stops there, which deploys it.
//! Squads near the beacon go through it when the best target from the
//! exit, all told, beats their own. The beacon follows the front; a Djinn in danger falls back
//! to a refuge and picks a new exit once safe.

use std::collections::BTreeMap;

use springai::{NO_TIMEOUT, Unit, Vec3};
use zk_terrain::Tier;
use zk_terrain::paths::SNAP;
use zk_terrain::strategy::ground_distance;

use crate::Ai;
use crate::kgbutil::{PathCost, kgb_move_to};
use crate::military::SquadId;
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

/// An exit must save at least this share of the travel to the enemy's
/// value, all told.
const MIN_SHARE: f32 = 0.25;

/// Frames before a lost Djinn is replaced, and before an unfulfilled
/// production order is forgotten.
const REBUILD_DELAY: i32 = 30 * 180;
const ORDER_TIMEOUT: i32 = 30 * 120;

/// Frames to wait for the beacon to appear, and to sit in a refuge before
/// heading out again.
const BEACON_TIMEOUT: i32 = 30 * 20;
const REFUGE_WAIT: i32 = 30 * 10;

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

/// The beacon moves once the front is this far from it (elmos), at most
/// every [`RESTAGE_PERIOD`] frames; it stands this far behind the front.
const RESTAGE_DISTANCE: f32 = 1500.0;
const RESTAGE_PERIOD: i32 = 30 * 60;
const STAGING_SETBACK: f32 = 300.0;

#[derive(Clone, Copy)]
enum Phase {
    /// Making its beacon (at home).
    Beacon { since: i32 },
    /// On its way to an exit.
    Travel { exit: Vec3, since: i32 },
    /// Standing at its exit, deployed.
    Deployed { since: i32 },
    /// Standing at its exit, moving its beacon.
    Restage { since: i32 },
    /// Fallen back from danger.
    Refuge { since: i32 },
}

struct Djinn {
    unit: Unit,
    beacon: Option<Unit>,
    phase: Phase,
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
            && self.strategy.as_ref().is_some_and(|s| {
                let total: f32 = s.analysis.values.iter().map(|v| v.enemy).sum();
                s.analysis.teleports.first().is_some_and(|t| t.gain >= MIN_SHARE * total)
            })
    }

    /// A factory has been told to build a Djinn.
    pub fn teleport_ordered(&mut self) {
        self.teleports.ordered = Some(self.frame);
    }

    /// A Djinn is ready: put its beacon down at home first.
    pub fn teleport_add_djinn(&mut self, unit: Unit) {
        if self.teleports.djinn.is_some() {
            return;
        }
        self.teleports.ordered = None;
        self.teleports.djinn = Some(Djinn { unit, beacon: None, phase: Phase::Beacon { since: self.frame } });
        self.teleport_place_beacon(unit);
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

    /// Order the beacon down at a free spot where our squads gather.
    fn teleport_place_beacon(&mut self, unit: Unit) {
        let (Some(def), Some(staging)) = (self.defs.get("tele_beacon"), self.teleport_staging()) else { return };
        let site = self.cb.map().find_closest_build_site(def, staging, 600.0, 2, 0);
        if site.x >= 0.0 {
            unit.execute_custom_command(CMD_PLACE_BEACON, &[site.x, site.y, site.z], 0, NO_TIMEOUT);
            self.log(&format!("teleport: Djinn placing its beacon at ({:.0}, {:.0})", site.x, site.z));
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
            let since = match d.phase {
                Phase::Beacon { since } => Some(since),
                _ => None,
            };
            let Some(d) = self.teleports.djinn.as_mut() else { return };
            d.beacon = beacon;
            if beacon.is_none() {
                // Lost, or never made: (re)make it, when not on the move.
                if since.is_none_or(|s| frame - s > BEACON_TIMEOUT) && !matches!(d.phase, Phase::Travel { .. }) {
                    d.phase = Phase::Beacon { since: frame };
                    self.teleport_place_beacon(unit);
                }
                if since.is_some() {
                    return;
                }
            }
        }

        let here = unit.pos();
        let threatened = self.war.get_threat(here) > 0.0;
        let Some(phase) = self.teleports.djinn.as_ref().map(|d| d.phase) else { return };
        match phase {
            Phase::Refuge { since } if threatened || frame - since < REFUGE_WAIT => {}
            Phase::Beacon { .. } | Phase::Refuge { .. } => match self.teleport_exit(unit) {
                Some(exit) => {
                    self.teleport_set_phase(Phase::Travel { exit, since: frame });
                    self.log(&format!("teleport: Djinn heading for an exit at ({:.0}, {:.0})", exit.x, exit.z));
                    self.teleport_move(unit, exit);
                }
                None => self.teleport_set_phase(Phase::Refuge { since: frame }),
            },
            Phase::Travel { exit, since } => {
                if threatened || self.war.get_threat(exit) > 0.0 {
                    self.teleports.banned.push((exit, frame + EXIT_BAN));
                    self.teleport_fall_back(unit);
                } else if distance(here, exit) <= ARRIVED {
                    // Standing still deploys it.
                    unit.stop(0);
                    self.teleport_set_phase(Phase::Deployed { since: frame });
                    self.log(&format!("teleport: Djinn deployed at ({:.0}, {:.0})", here.x, here.z));
                } else if frame - since >= 30 * 5 {
                    self.teleport_set_phase(Phase::Travel { exit, since: frame });
                    self.teleport_move(unit, exit);
                }
            }
            Phase::Deployed { .. } | Phase::Restage { .. } if threatened => {
                self.teleports.banned.push((here, frame + EXIT_BAN));
                self.teleport_fall_back(unit);
            }
            Phase::Deployed { since } => {
                // Keep the beacon where the squads are.
                let beacon = self.teleports.djinn.as_ref().and_then(|d| d.beacon).map(|b| b.pos());
                let staging = self.teleport_staging();
                if let (Some(beacon), Some(staging)) = (beacon, staging)
                    && frame - since >= RESTAGE_PERIOD
                    && self.teleports.squads.is_empty()
                    && distance(beacon, staging) > RESTAGE_DISTANCE
                {
                    self.teleport_set_phase(Phase::Restage { since: frame });
                    self.teleport_place_beacon(unit);
                }
            }
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

    /// The best exit the Djinn can reach that the enemy is not threatening
    /// or about to overrun.
    fn teleport_exit(&self, unit: Unit) -> Option<Vec3> {
        let s = self.strategy.as_ref()?;
        let terrain = self.terrain()?;
        let (map, amph) = (terrain.tier(s.tier)?, terrain.tier(Tier::Amphib)?);
        let total: f32 = s.analysis.values.iter().map(|v| v.enemy).sum();
        let elevation = self.cb.map();
        let banned =
            |p: Vec3| self.teleports.banned.iter().any(|&(b, until)| until > self.frame && distance(b, p) < 500.0);
        s.analysis.teleports.iter().take_while(|t| t.gain >= MIN_SHARE * total).find_map(|t| {
            let c = map.regions[t.region].center;
            let exit = Vec3::new(c.x, elevation.elevation_at(c.x, c.z), c.z);
            let safe = self.war.get_threat(exit) <= 0.0 && !self.incursion_imminent(exit, 0.0) && !banned(exit);
            (safe && amph.connected(pos(unit.pos()), c, SNAP)).then_some(exit)
        })
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

    /// Send squad `sid` at `from` through the beacon if the best target
    /// from the Djinn's exit, counting the way to the beacon and the wait
    /// there, beats its own best target (at `cost`, as
    /// [`war_best_target`](Ai::war_best_target) scores them); whether it was.
    pub fn teleport_shortcut(&mut self, sid: SquadId, from: Vec3, cost: f32) -> bool {
        let Some(d) = self.teleports.djinn.as_ref().filter(|d| matches!(d.phase, Phase::Deployed { .. })) else {
            return false;
        };
        let (Some(beacon), exit) = (d.beacon, d.unit.pos()) else { return false };
        let Some(s) = self.squads.get(sid).filter(|s| !s.is_air_squad) else { return false };
        let Some(leader) = s.get_leader(&self.fighters) else { return false };
        let members: Vec<Unit> = s.fighters.iter().filter_map(|id| self.fighters.get(id)).map(|f| f.unit).collect();
        let defs: Vec<_> = members.iter().filter_map(|u| u.def()).collect();
        let (Some(tier), Some(terrain)) = (defs.first().and_then(|&d| tier_of(d)), self.terrain()) else {
            return false;
        };
        let Some(map) = terrain.tier(tier) else { return false };
        let Some(to_beacon) = ground_distance(map, pos(from), pos(beacon.pos())).filter(|&d| d <= LAUNCH_RANGE) else {
            return false;
        };
        let speed = defs.iter().map(|d| d.speed()).fold(f32::INFINITY, f32::min);
        let wait = defs.iter().map(|d| d.mass()).sum::<f32>() / THROUGHPUT;
        let (target, exit_cost) = self.war_best_target(leader, exit, true);
        // Target costs count distance at a quarter for the most wanted
        // targets.
        let overhead = (to_beacon + wait * speed) / 4.0;
        if target == NULLPOS || exit_cost + overhead >= cost {
            return false;
        }
        for u in &members {
            u.execute_custom_command(CMD_WAIT_AT_BEACON, &[beacon.id() as f32], 0, NO_TIMEOUT);
        }
        self.teleports.squads.insert(sid, self.frame);
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
