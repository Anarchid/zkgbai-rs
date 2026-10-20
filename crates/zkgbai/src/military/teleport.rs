//! The Djinn (amphibious teleport bridge) as an evacuator. Units that reach
//! its beacon (a "lamp", put down anywhere from afar, in 8 s) are
//! teleported to wherever the Djinn stands deployed, taking their mass /
//! 150 seconds. So the Djinn stands at our repair base (the safe place by
//! our base with the most build power), and when a strider or other heavy
//! unit retreats from far off, it throws its lamp ahead of it on its way
//! back: the unit waits there and comes out among the constructors that
//! repair it, instead of walking home under fire.
//!
//! A Djinn in danger falls back to a refuge and goes back once safe.

use std::collections::BTreeMap;

use springai::{NO_TIMEOUT, Unit, UnitDef, Vec3};

use crate::Ai;
use crate::kgbutil::{PathCost, kgb_move_to};
use crate::nav;
use crate::util::{NULLPOS, OPTION_SHIFT_KEY, distance, get_directional_point};

/// Custom commands of Zero-K's teleporter gadget.
const CMD_PLACE_BEACON: i32 = 35170;
const CMD_WAIT_AT_BEACON: i32 = 35171;

/// Frames between updates.
const PERIOD: i32 = 30;

/// A Djinn is worth building from this income on, with a unit worth
/// evacuating in the army.
const MIN_INCOME: f32 = 25.0;

/// Ground units costing this much (striders and the like) are evacuated
/// when they retreat from this far from the Djinn, elmos.
const EVACUEE_COST: f32 = 1500.0;
const EVACUATE_FROM: f32 = 2000.0;

/// The lamp goes this many seconds of the unit's walk ahead of it (the
/// lamp takes 8 s to appear), towards home, and this far to the side of
/// its way (no lamp goes down where a unit stands).
const LEAD: f32 = 12.0;
const ASIDE: f32 = 120.0;

/// Frames before a lost Djinn is replaced, and before an unfulfilled
/// production order is forgotten.
const REBUILD_DELAY: i32 = 30 * 180;
const ORDER_TIMEOUT: i32 = 30 * 120;

/// Frames to wait for the lamp to appear, and to sit in a refuge before
/// going back.
const BEACON_TIMEOUT: i32 = 30 * 20;
const REFUGE_WAIT: i32 = 30 * 10;

/// A lamp this close to where it should be will do, elmos.
const BEACON_SLACK: f32 = 600.0;

/// Mass the Djinn teleports per second (its `teleporter_throughput`).
const THROUGHPUT: f32 = 150.0;

/// Teleported units come out this close to the Djinn.
const LANDED: f32 = 500.0;

/// How close to its post the Djinn stops, elmos; it moves when its post is
/// this far off.
const ARRIVED: f32 = 150.0;
const REPOST: f32 = 800.0;

/// The Djinn stands this far from the repair base, towards our base.
const POST_OFFSET: f32 = 150.0;

#[derive(Clone, Copy)]
enum Phase {
    /// Walking to its post.
    Travel { to: Vec3, since: i32 },
    /// Putting its lamp down at `at`, for unit `for_unit` (if any).
    Beacon { at: Vec3, for_unit: Option<i32>, since: i32 },
    /// At its post, deployed.
    Deployed,
    /// Fallen back from danger.
    Refuge { since: i32 },
}

struct Djinn {
    unit: Unit,
    beacon: Option<Unit>,
    phase: Phase,
}

/// The evacuator, if we have one, and the units on their way through.
#[derive(Default)]
pub struct Teleports {
    djinn: Option<Djinn>,
    /// When a Djinn was queued at a factory.
    ordered: Option<i32>,
    rebuild_after: i32,
    /// Units told to wait at the lamp (and their type): since when.
    evacuating: BTreeMap<i32, (Unit, Option<UnitDef>, i32)>,
}

impl Teleports {
    /// Whether unit `id` is on its way through the lamp (not to be given
    /// other orders meanwhile).
    pub fn evacuating(&self, id: i32) -> bool {
        self.evacuating.contains_key(&id)
    }
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
            && self.fighters.values().any(|f| self.teleport_evacuee(f.unit))
    }

    /// A factory has been told to build a Djinn.
    pub fn teleport_ordered(&mut self) {
        self.teleports.ordered = Some(self.frame);
    }

    /// A Djinn is ready: to its post.
    pub fn teleport_add_djinn(&mut self, unit: Unit) {
        if self.teleports.djinn.is_some() {
            return;
        }
        self.teleports.ordered = None;
        let phase = Phase::Refuge { since: self.frame - REFUGE_WAIT };
        self.teleports.djinn = Some(Djinn { unit, beacon: None, phase });
    }

    /// Whether `unit` is worth a lamp when it retreats: a heavy ground unit.
    fn teleport_evacuee(&self, unit: Unit) -> bool {
        unit.def().is_some_and(|d| d.move_data().is_some() && d.cost(self.metal) >= EVACUEE_COST)
    }

    /// The Djinn's post: by the safe place on our ground with the most
    /// build power (a factory or caretaker by our base), towards our base.
    fn teleport_post(&self) -> Vec3 {
        let base = self.graph.get_ally_center();
        let builders = self.fac.factories.values().map(|f| f.unit).chain(self.eco.nanos.values().map(|n| n.unit));
        let best = builders
            .map(|u| u.pos())
            .filter(|&p| p != NULLPOS && self.graph.is_ally_territory(p) && self.war.get_threat(p) <= 0.0)
            .max_by(|a, b| {
                (self.war.get_bp(*a).total_cmp(&self.war.get_bp(*b)))
                    .then_with(|| distance(*b, base).total_cmp(&distance(*a, base)))
            });
        match best {
            Some(p) if base != NULLPOS => get_directional_point(p, base, POST_OFFSET),
            Some(p) => p,
            None => base,
        }
    }

    /// Order the lamp down at a free spot by `at`; where, if it can be.
    fn teleport_place_beacon(&mut self, unit: Unit, at: Vec3) -> Option<Vec3> {
        let def = self.defs.get("tele_beacon")?;
        let site = self.cb.map().find_closest_build_site(def, at, 600.0, 2, 0);
        if site.x < 0.0 {
            return None;
        }
        unit.execute_custom_command(CMD_PLACE_BEACON, &[site.x, site.y, site.z], 0, NO_TIMEOUT);
        Some(site)
    }

    /// The retreating unit most worth a lamp, and where to put it: ahead of
    /// it on its way to the Djinn at `post`.
    fn teleport_next_evacuee(&self, post: Vec3) -> Option<(Unit, Vec3)> {
        let retreating = self.fighters.values().map(|f| f.unit).filter(|u| self.retreat_handler.is_retreating(u.id()));
        let (unit, _) = retreating
            .filter(|&u| u.health() > 0.0 && self.teleport_evacuee(u) && !self.teleports.evacuating(u.id()))
            .filter(|u| distance(u.pos(), post) >= EVACUATE_FROM)
            .filter_map(|u| Some((u, u.def()?.cost(self.metal))))
            .max_by(|a, b| a.1.total_cmp(&b.1))?;
        let here = unit.pos();
        let speed = unit.def().map_or(30.0, |d| d.speed());
        let lead = (speed * LEAD).min(distance(here, post) / 2.0);
        let ahead = get_directional_point(here, post, lead);
        let (dx, dz) = (ahead.x - here.x, ahead.z - here.z);
        let n = (dx * dx + dz * dz).sqrt().max(1.0);
        Some((unit, Vec3::xz(ahead.x - dz / n * ASIDE, ahead.z + dx / n * ASIDE)))
    }

    pub fn teleport_update(&mut self, frame: i32) {
        if (frame + self.offset + 17) % PERIOD != 0 {
            return;
        }
        self.teleport_update_evacuees();
        let Some(d) = &self.teleports.djinn else { return };
        let unit = d.unit;
        if unit.health() <= 0.0 || unit.team() != self.team_id {
            self.log("teleport: Djinn lost");
            let evacuating = std::mem::take(&mut self.teleports.evacuating);
            self.teleports = Teleports { rebuild_after: frame + REBUILD_DELAY, evacuating, ..Teleports::default() };
            return;
        }

        // The lamp: ours is the one linked to this Djinn.
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
        let Some((phase, beacon)) = self.teleports.djinn.as_ref().map(|d| (d.phase, d.beacon)) else { return };
        let post = self.teleport_post();
        match phase {
            Phase::Refuge { since } if threatened || frame - since < REFUGE_WAIT => {}
            Phase::Refuge { .. } => self.teleport_travel(unit, post),
            _ if threatened => self.teleport_fall_back(unit),
            Phase::Travel { to, since } => {
                if distance(here, to) <= ARRIVED {
                    self.teleport_arrive(unit, beacon);
                } else if distance(to, post) > REPOST || frame - since >= 30 * 10 {
                    self.teleport_travel(unit, post);
                }
            }
            Phase::Beacon { at, for_unit, since } => {
                let placed = beacon.filter(|b| distance(b.pos(), at) <= BEACON_SLACK);
                if let Some(b) = placed {
                    self.teleport_deploy(unit);
                    if let Some(u) = for_unit.and_then(|id| self.fighters.get(&id)).map(|f| f.unit) {
                        self.teleport_send(u, b);
                    }
                } else if frame - since > BEACON_TIMEOUT {
                    self.log(&format!("teleport: the lamp at ({:.0}, {:.0}) did not come up", at.x, at.z));
                    self.teleport_deploy(unit);
                }
            }
            Phase::Deployed if distance(here, post) > REPOST && self.teleports.evacuating.is_empty() => {
                self.teleport_travel(unit, post);
            }
            Phase::Deployed => {
                if !self.teleports.evacuating.is_empty() {
                    return;
                }
                if let Some((u, at)) = self.teleport_next_evacuee(here)
                    && let Some(site) = self.teleport_place_beacon(unit, at)
                {
                    self.teleport_set_phase(Phase::Beacon { at: site, for_unit: Some(u.id()), since: frame });
                    let (p, name) = (u.pos(), self.defs.name_of(u).to_owned());
                    self.log(&format!(
                        "teleport: lamp at ({:.0}, {:.0}) for a {name} retreating from ({:.0}, {:.0})",
                        site.x, site.z, p.x, p.z
                    ));
                }
            }
        }
    }

    /// Release evacuees that came through, died or took too long.
    fn teleport_update_evacuees(&mut self) {
        let frame = self.frame;
        let at = self.teleports.djinn.as_ref().map(|d| d.unit.pos());
        let mut done = Vec::new();
        for (&id, &(u, def, since)) in &self.teleports.evacuating {
            let wait = def.map_or(0.0, |d| d.mass()) / THROUGHPUT;
            let through = at.is_some_and(|at| distance(u.pos(), at) < LANDED);
            if u.health() <= 0.0 {
                done.push((id, "was lost on the way"));
            } else if through {
                done.push((id, "came through"));
            } else if at.is_none() || (frame - since) as f32 > 30.0 * (40.0 + 2.0 * wait) {
                done.push((id, "did not come through"));
                u.stop(0);
            }
        }
        for (id, how) in done {
            if let Some((_, def, _)) = self.teleports.evacuating.remove(&id) {
                let name = def.map_or("?", |d| self.defs.name(d)).to_owned();
                self.log(&format!("teleport: the {name} {how}"));
            }
        }
    }

    fn teleport_set_phase(&mut self, phase: Phase) {
        if let Some(d) = self.teleports.djinn.as_mut() {
            d.phase = phase;
        }
    }

    /// Tell `unit` to wait at the lamp to be teleported.
    fn teleport_send(&mut self, unit: Unit, beacon: Unit) {
        if unit.health() <= 0.0 {
            return;
        }
        unit.execute_custom_command(CMD_WAIT_AT_BEACON, &[beacon.id() as f32], 0, NO_TIMEOUT);
        self.teleports.evacuating.insert(unit.id(), (unit, unit.def(), self.frame));
    }

    /// At its post: it needs a lamp to deploy (put by the front, where
    /// units come back from, until one is wanted elsewhere).
    fn teleport_arrive(&mut self, unit: Unit, beacon: Option<Unit>) {
        let here = unit.pos();
        if beacon.is_some() {
            self.teleport_deploy(unit);
            return;
        }
        let front = self.graph.get_closest_front_line_spot(here).map_or(here, |s| s.position);
        let at = get_directional_point(front, here, 300.0);
        match self.teleport_place_beacon(unit, at) {
            Some(site) => self.teleport_set_phase(Phase::Beacon { at: site, for_unit: None, since: self.frame }),
            None => self.teleport_deploy(unit),
        }
    }

    /// Standing still deploys it.
    fn teleport_deploy(&mut self, unit: Unit) {
        unit.stop(0);
        if !matches!(self.teleports.djinn.as_ref().map(|d| d.phase), Some(Phase::Deployed)) {
            let here = unit.pos();
            self.log(&format!("teleport: Djinn deployed at the repair base by ({:.0}, {:.0})", here.x, here.z));
        }
        self.teleport_set_phase(Phase::Deployed);
    }

    fn teleport_travel(&mut self, unit: Unit, to: Vec3) {
        self.teleport_set_phase(Phase::Travel { to, since: self.frame });
        self.teleport_move(unit, to);
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
}
