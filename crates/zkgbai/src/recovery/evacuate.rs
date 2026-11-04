//! Rescue execution. Every attempt has exclusive command authority, a deadline,
//! and an observed landing/movement check. Availability never implies success.

use springai::{NO_TIMEOUT, Unit, UnitDef, Vec3, opt, reserved};

use super::{Ai, Loan, Mission, OWNER, Phase};
use crate::{
    kgbutil::PathCost,
    terrain::{move_class, pos, tier_of},
    util::{NULLPOS, distance},
};

const LOAD: i32 = 75;
const UNLOAD: i32 = 80;
const PLACE_BEACON: i32 = 35170;
const WAIT_BEACON: i32 = 35171;

fn fits(cargo: UnitDef, transport: UnitDef) -> bool {
    !cargo.is_able_to_fly()
        && !cargo.is_not_transportable()
        && cargo.speed() > 0.0
        && cargo.x_size().max(cargo.z_size()) <= 2 * transport.transport_size()
        && (transport.name() == "gunshipheavytrans" || cargo.custom_param("requireheavytrans").is_none())
}

impl Ai {
    fn recovery_site(&self, unit: Unit, at: Vec3, goal: Vec3) -> bool {
        let Some(def) = unit.def() else {
            return false;
        };
        let Some(class) = move_class(def) else {
            return false;
        };
        let map = self.cb.map();
        let pad = def.x_size().max(def.z_size()) as f32 * 4.0 + 16.0;
        let in_map = at.x >= pad
            && at.z >= pad
            && at.x + pad < map.width() as f32 * 8.0
            && at.z + pad < map.height() as f32 * 8.0;
        if !in_map
            || self.terrain().is_none()
            || self.war.get_effective_threat(at) > 0.0
            || self.missiles_ground_burning(at)
            || self.blasts_waiting(at)
            || class.blocks(0.0, -map.elevation_at(at.x, at.z))
            || !map.is_possible_to_build_at(def, self.recovery_ground(at), 0)
        {
            return false;
        }
        // A landing needs somewhere useful to walk, not just a free footprint.
        let home = self.graph.get_ally_center();
        let Some(tier) = tier_of(def).and_then(|t| self.terrain()?.tier(t)) else {
            return false;
        };
        let connected = (goal != NULLPOS && tier.reaches_within(pos(at), pos(goal), 64.0, 16.0))
            || (home != NULLPOS && tier.reaches_within(pos(at), pos(home), 128.0, 16.0));
        connected && self.recovery_local_at(unit, at, false).is_some_and(|grid| grid.escape(goal, None, true).is_some())
    }

    pub(super) fn recovery_landing(&self, unit: Unit, goal: Vec3, minimum: f32) -> Option<Vec3> {
        let from = unit.pos();
        let angle = (goal.z - from.z).atan2(goal.x - from.x);
        for radius in [minimum, minimum + 160.0, minimum + 320.0, minimum + 640.0] {
            for turn in [0, 1, -1, 2, -2, 3, -3, 4] {
                let a = angle + turn as f32 * std::f32::consts::FRAC_PI_4;
                let at = self.recovery_ground(Vec3::xz(from.x + radius * a.cos(), from.z + radius * a.sin()));
                if self.recovery_site(unit, at, goal) {
                    return Some(at);
                }
            }
        }
        None
    }

    /// Conservative air clearance along every leg, including pickup and landing.
    /// Ground superiority does not cancel enemy AA for a fragile rescue transport.
    fn recovery_air_clear(&self, from: Vec3, to: Vec3) -> bool {
        let steps = (distance(from, to) / 64.0).ceil().max(1.0) as usize;
        (0..=steps).all(|i| {
            let t = i as f32 / steps as f32;
            let p = Vec3::xz(from.x + (to.x - from.x) * t, from.z + (to.z - from.z) * t);
            self.war.get_aa_threat(p) <= 0.0 && !self.blasts_waiting(p)
        })
    }

    fn recovery_air_route(&mut self, transport: Unit, to: Vec3) -> Option<Vec<Vec3>> {
        let mut path: Vec<_> =
            self.pathfinder.find_path(&self.war, transport, to, PathCost::AirPath).into_iter().collect();
        path.push(to);
        let mut from = transport.pos();
        for &p in &path {
            if !self.recovery_air_clear(from, p) {
                return None;
            }
            from = p;
        }
        Some(path)
    }

    pub(super) fn recovery_evacuate(&mut self, cargo: Unit, goal: Vec3, attempted: u8) {
        // An already available Lobster is the cheapest local lift. Approach
        // without asking the trapped passenger to walk to the launcher.
        if attempted & 4 == 0 && !self.dev_off("lobsters") {
            let lobster = self
                .cb
                .team_units()
                .into_iter()
                .filter(|u| self.defs.name_of(*u) == "amphlaunch")
                .filter(|u| self.recovery_free(*u) && distance(u.pos(), cargo.pos()) < 1600.0)
                .min_by(|a, b| distance(a.pos(), cargo.pos()).total_cmp(&distance(b.pos(), cargo.pos())));
            if let Some(launcher) = lobster {
                let radius = launcher
                    .def()
                    .and_then(|d| d.custom_param("thrower_gather"))
                    .and_then(|s| s.parse::<f32>().ok())
                    .unwrap_or(0.0);
                let approach = if distance(launcher.pos(), cargo.pos()) < radius - 24.0 {
                    Some(launcher.pos())
                } else {
                    (0..8).find_map(|i| {
                        let a = i as f32 * std::f32::consts::FRAC_PI_4;
                        let p = self.recovery_ground(Vec3::xz(
                            cargo.pos().x + (radius - 32.0) * a.cos(),
                            cargo.pos().z + (radius - 32.0) * a.sin(),
                        ));
                        (radius > 48.0
                            && self.recovery_site(launcher, p, goal)
                            && launcher.def().is_some_and(|d| self.terrain_reaches(d, launcher.pos(), p, 32.0)))
                        .then_some(p)
                    })
                };
                if let Some(at) = approach {
                    let m = self.recovery_begin(cargo, goal, at, Phase::Lobster { launcher }, 35 * 30, &[launcher]);
                    cargo.stop(0);
                    launcher.move_to(at, 0);
                    self.recovery.watches.get_mut(&cargo.id()).unwrap().attempted |= 4;
                    self.recovery.mission = Some(m);
                    return;
                }
            }
        }

        if attempted & 8 == 0
            && !self.dev_off("teleport")
            && let Some(djinn) = self.teleport_recovery_available().filter(|u| self.recovery_free(*u))
        {
            // Test a free footprint around the exit, rather than the Djinn's
            // own occupied footprint. It must lead out of this neighbourhood.
            let exit_ok = distance(cargo.pos(), djinn.pos()) > 500.0
                && (0..8).any(|i| {
                    let a = i as f32 * std::f32::consts::FRAC_PI_4;
                    self.recovery_site(
                        cargo,
                        Vec3::xz(djinn.pos().x + 160.0 * a.cos(), djinn.pos().z + 160.0 * a.sin()),
                        goal,
                    )
                });
            if exit_ok
                && self.war.get_effective_threat(djinn.pos()) <= 0.0
                && let Some(def) = self.defs.get("tele_beacon")
            {
                // The engine build-site query permits mobile occupants, but the
                // beacon gadget rejects them. Check the snapped footprint too.
                let nearby = self
                    .cb
                    .friendly_units_in_cylinder(cargo.pos(), 256.0)
                    .into_iter()
                    .chain(self.cb.enemy_units_in_cylinder(cargo.pos(), 256.0))
                    .collect::<Vec<_>>();
                let site = [64.0, 96.0, 128.0, 160.0].into_iter().find_map(|r| {
                    (0..16).find_map(|i| {
                        let a = i as f32 * std::f32::consts::PI / 8.0;
                        let p = self.recovery_ground(Vec3::xz(
                            ((cargo.pos().x + r * a.cos()) / 16.0).round() * 16.0,
                            ((cargo.pos().z + r * a.sin()) / 16.0).round() * 16.0,
                        ));
                        (distance(p, cargo.pos()) <= 180.0
                            && self.cb.map().is_possible_to_build_at(def, p, 1)
                            && nearby.iter().all(|u| {
                                u.def().is_none_or(|d| {
                                    d.is_able_to_fly()
                                        || !self.recovery_grounded(*u)
                                        || (u.pos().x - p.x).abs()
                                            >= (d.x_size().max(d.z_size()) + def.x_size().max(def.z_size())) as f32
                                                * 4.0
                                                + 8.0
                                        || (u.pos().z - p.z).abs()
                                            >= (d.x_size().max(d.z_size()) + def.x_size().max(def.z_size())) as f32
                                                * 4.0
                                                + 8.0
                                })
                            }))
                        .then_some(p)
                    })
                });
                // Gadget radius is 200. Stay inside it even after build-grid snapping.
                if let Some(site) = site {
                    let m =
                        self.recovery_begin(cargo, goal, djinn.pos(), Phase::Beacon { djinn, site }, 25 * 30, &[djinn]);
                    cargo.stop(0);
                    djinn.stop(0);
                    djinn.execute_custom_command(PLACE_BEACON, &[site.x, site.y, site.z], 0, NO_TIMEOUT);
                    self.recovery.watches.get_mut(&cargo.id()).unwrap().attempted |= 8;
                    self.recovery.mission = Some(m);
                    return;
                }
            }
        }

        if attempted & 16 == 0 && !self.dev_off("drops") {
            let Some(to) = self.recovery_landing(cargo, goal, 400.0) else {
                return;
            };
            if !self.recovery_air_clear(cargo.pos(), to) {
                return;
            }
            let Some(def) = cargo.def().filter(|d| !d.is_not_transportable()) else {
                return;
            };
            let mut transports: Vec<_> = self
                .cb
                .team_units()
                .into_iter()
                .filter(|u| {
                    matches!(self.defs.name_of(*u), "gunshiptrans" | "gunshipheavytrans")
                        && self.recovery_free(*u)
                        && u.def().is_some_and(|t| {
                            fits(def, t) && u.rules_param_float("massOverride", t.mass()) <= t.mass() + 1.0
                        })
                })
                .collect();
            transports.sort_by(|a, b| distance(a.pos(), cargo.pos()).total_cmp(&distance(b.pos(), cargo.pos())));
            for transport in transports {
                let Some(route) = self.recovery_air_route(transport, cargo.pos()) else {
                    continue;
                };
                let empty_mass = transport.rules_param_float("massOverride", transport.def().unwrap().mass());
                let m =
                    self.recovery_begin(cargo, goal, to, Phase::Load { transport, empty_mass }, 60 * 30, &[transport]);
                cargo.stop(0);
                for (i, p) in route.iter().enumerate() {
                    transport.move_to(*p, if i == 0 { 0 } else { opt::SHIFT_KEY });
                }
                transport.execute_custom_command(LOAD, &[cargo.id() as f32], opt::SHIFT_KEY, NO_TIMEOUT);
                self.recovery.watches.get_mut(&cargo.id()).unwrap().attempted |= 16;
                self.recovery.mission = Some(m);
                return;
            }
            // Request one compatible transport from an existing gunship plant.
            // No new factory is commissioned for an isolated low-value casualty.
            let name = if def.custom_param("requireheavytrans").is_some()
                || self.defs.get("gunshiptrans").is_none_or(|t| !fits(def, t))
            {
                "gunshipheavytrans"
            } else {
                "gunshiptrans"
            };
            let trapped_value: f32 = self
                .recovery
                .watches
                .values()
                .filter(|w| {
                    self.frame - w.progress.since >= super::STALL && distance(w.unit.pos(), cargo.pos()) <= 256.0
                })
                .filter_map(|w| w.unit.def())
                .map(|d| d.cost(self.metal))
                .sum();
            if self.defs.get(name).is_some_and(|t| {
                fits(def, t) && (trapped_value >= t.cost(self.metal) || self.recovery.births.contains_key(&cargo.id()))
            }) && self
                .fac
                .factories
                .values()
                .any(|f| f.unit.def().is_some_and(|d| d.build_options().iter().any(|b| self.defs.name(*b) == name)))
            {
                self.recovery.demand = Some((name, self.frame));
            }
        }
    }

    pub(crate) fn recovery_transport_order(&self) -> Option<&'static str> {
        if self.dev_off("recovery") || self.dev_off("drops") {
            return None;
        }
        self.recovery
            .demand
            .filter(|(_, at)| self.frame - *at < 60 * 30)
            .filter(|_| self.recovery.ordered.is_none_or(|at| self.frame - at > 120 * 30))
            .map(|(name, _)| name)
    }

    pub(crate) fn recovery_transport_ordered(&mut self, name: &str) {
        if self.recovery_transport_order() == Some(name) {
            self.recovery.ordered = Some(self.frame);
        }
    }

    pub(crate) fn recovery_transport_ready(&mut self, unit: Unit) {
        if self.recovery.demand.is_some_and(|(name, _)| name == self.defs.name_of(unit)) {
            self.recovery.ordered = None;
            self.recovery.demand = None;
        }
    }

    fn recovery_carried(&self, cargo: Unit, transport: Unit, empty_mass: f32) -> bool {
        transport.health() > 0.0
            && transport.team() == self.team_id
            && distance(cargo.pos(), transport.pos()) < 100.0
            && (transport.rules_param_float("massOverride", empty_mass) > empty_mass + 1.0
                || !self.recovery_grounded(cargo))
    }

    fn recovery_verify(&self, m: &mut Mission) {
        m.phase = Phase::Verify { anchor: m.cargo.pos() };
        m.deadline = self.frame + 15 * 30;
        // Resume a useful ground move; successful teleport/load alone is not relief.
        m.cargo.move_to(self.recovery_ground(m.goal), 0);
    }

    pub(super) fn recovery_tick(&mut self, mut m: Mission) {
        let valid = m.loans.iter().all(|l| {
            l.lease.active()
                && l.unit.health() > 0.0
                && l.unit.team() == self.team_id
                && !reserved::is_reserved(self.cb, l.unit.id())
        });
        if !valid
            || self.dev_off("recovery")
            || reserved::is_muted(self.cb, "recovery.update")
            || self.frame >= m.deadline
        {
            // Do not abandon live cargo in a transport on a load/unload timeout.
            // The queued unload remains executable after command authority returns.
            if let Phase::Carry { transport, empty_mass }
            | Phase::Unload { transport, empty_mass }
            | Phase::Load { transport, empty_mass } = m.phase
                && transport.team() == self.team_id
                && transport.health() > 0.0
                && !reserved::is_reserved(self.cb, transport.id())
                && self.recovery_owns(transport.id())
                && self.recovery_carried(m.cargo, transport, empty_mass)
            {
                // Retain authority while unloading, with retries at nearby
                // verified sites instead of returning a loaded aircraft to drops.
                if let Some(to) = self.recovery_landing(m.cargo, m.goal, 160.0) {
                    m.to = to;
                }
                transport.execute_custom_command(UNLOAD, &[m.to.x, m.to.y, m.to.z, 64.0], 0, NO_TIMEOUT);
                m.phase = Phase::Unload { transport, empty_mass };
                m.deadline = self.frame + 20 * 30;
                if valid
                    && !self.dev_off("recovery")
                    && !reserved::is_muted(self.cb, "recovery.update")
                    && self.frame - m.since < 180 * 30
                {
                    self.recovery.mission = Some(m);
                    return;
                }
            }
            self.recovery_finish(m, false);
            return;
        }
        if (self.frame - m.since) % (10 * 30) == 0 {
            let helper = match m.phase {
                Phase::Load { transport, .. } | Phase::Carry { transport, .. } | Phase::Unload { transport, .. } => {
                    Some(transport)
                }
                Phase::Beacon { djinn, .. } | Phase::Teleport { djinn } => Some(djinn),
                Phase::Lobster { launcher } => Some(launcher),
                _ => None,
            };
            let help = helper.map(|u| {
                (u.id(), u.pos(), u.current_commands().iter().take(3).map(|c| (c.id(), c.params())).collect::<Vec<_>>())
            });
            self.log(&format!(
                "recovery: #{} phase={:?} at={:?} to={:?} grounded={} helper={help:?}",
                m.cargo.id(),
                m.phase,
                m.cargo.pos(),
                m.to,
                self.recovery_grounded(m.cargo)
            ));
        }
        match m.phase {
            Phase::Walk => {
                if distance(m.cargo.pos(), m.to) < 64.0 && distance(m.from, m.cargo.pos()) > 96.0 {
                    self.recovery_finish(m, true);
                    return;
                }
            }
            Phase::Traffic => {
                if self.frame - m.since >= 6 * 30 {
                    self.recovery_verify(&mut m);
                }
            }
            Phase::Clear { blocker, worker } => {
                if blocker.health() <= 0.0 {
                    self.recovery_verify(&mut m);
                } else if blocker.team() != self.team_id
                    || !self.recovery_expendable(blocker)
                    || self.war.get_effective_threat(worker.pos()) > 0.0
                {
                    self.recovery_finish(m, false);
                    return;
                }
            }
            Phase::Lobster { launcher } => {
                if let Some((jd, radius)) = self.lobster_ready(launcher)
                    && distance(launcher.pos(), m.cargo.pos()) < radius - 24.0
                {
                    let passengers: Vec<_> = self
                        .cb
                        .friendly_units_in_cylinder(launcher.pos(), radius + 16.0)
                        .into_iter()
                        .chain(self.cb.enemy_units_in_cylinder(launcher.pos(), radius + 16.0))
                        .filter(|u| {
                            u.id() != launcher.id() && u.def().is_some_and(|d| d.speed() > 0.0 && !d.is_able_to_fly())
                        })
                        .collect();
                    // Never involuntarily throw allied, operator-controlled or
                    // otherwise committed units. Recheck the entire grab at launch.
                    if passengers.iter().any(|u| u.id() == m.cargo.id())
                        && passengers
                            .iter()
                            .all(|u| m.loans.iter().any(|l| l.unit.id() == u.id()) || self.recovery_free(*u))
                    {
                        for radius in [256.0, 384.0, 512.0] {
                            let base = (m.goal.z - m.cargo.pos().z).atan2(m.goal.x - m.cargo.pos().x);
                            let aim = (0..8).find_map(|i| {
                                let a = base + i as f32 * std::f32::consts::FRAC_PI_4;
                                let to = self.recovery_ground(Vec3::xz(
                                    launcher.pos().x + radius * a.cos(),
                                    launcher.pos().z + radius * a.sin(),
                                ));
                                (self.lobster_clear(jd, launcher, &passengers, to)
                                    && passengers.iter().all(|u| {
                                        let p = u.pos();
                                        self.recovery_site(
                                            *u,
                                            Vec3::xz(p.x + to.x - launcher.pos().x, p.z + to.z - launcher.pos().z),
                                            m.goal,
                                        )
                                    }))
                                .then_some(to)
                            });
                            if let Some(to) = aim {
                                for u in &passengers {
                                    if !m.loans.iter().any(|l| l.unit.id() == u.id()) {
                                        m.loans.push(Loan { unit: *u, lease: reserved::lend(self.cb, u.id(), OWNER) });
                                    }
                                    u.stop(0);
                                }
                                launcher.stop(0);
                                if launcher.dgun_pos(to, 0) {
                                    m.to = to;
                                    m.phase = Phase::Flight;
                                    m.deadline = self.frame + 15 * 30;
                                }
                                break;
                            }
                        }
                    }
                }
            }
            Phase::Flight => {
                if self.recovery_grounded(m.cargo) && distance(m.from, m.cargo.pos()) > 128.0 {
                    self.recovery_verify(&mut m);
                }
            }
            Phase::Beacon { djinn, site } => {
                let beacon = self.cb.team_units().into_iter().find(|b| {
                    self.defs.name_of(*b) == "tele_beacon"
                        && b.rules_param_float("connectto", -1.0) == djinn.id() as f32
                        && !b.is_being_built()
                        && distance(b.pos(), site) < 32.0
                        && distance(b.pos(), m.cargo.pos()) < 190.0
                });
                if let Some(beacon) = beacon {
                    djinn.stop(0);
                    m.cargo.execute_custom_command(WAIT_BEACON, &[beacon.id() as f32], 0, NO_TIMEOUT);
                    m.phase = Phase::Teleport { djinn };
                    let mass = m.cargo.def().map_or(150.0, |d| d.mass());
                    let rate = djinn
                        .def()
                        .and_then(|d| d.custom_param("teleporter_throughput"))
                        .and_then(|s| s.parse::<f32>().ok())
                        .unwrap_or(150.0)
                        .max(1.0);
                    m.deadline = self.frame + ((40.0 + 2.0 * mass / rate) * 30.0) as i32;
                }
            }
            Phase::Teleport { djinn } => {
                if self.war.get_effective_threat(djinn.pos()) > 0.0 {
                    self.recovery_finish(m, false);
                    return;
                }
                if distance(m.cargo.pos(), djinn.pos()) < 400.0 && distance(m.from, m.cargo.pos()) > 300.0 {
                    self.recovery_verify(&mut m);
                }
            }
            Phase::Load { transport, empty_mass } => {
                if self.recovery_carried(m.cargo, transport, empty_mass) {
                    if let Some(path) = self.recovery_air_route(transport, m.to) {
                        for (i, p) in path.iter().enumerate() {
                            transport.move_to(*p, if i == 0 { 0 } else { opt::SHIFT_KEY });
                        }
                        m.phase = Phase::Carry { transport, empty_mass };
                        m.deadline = self.frame + 90 * 30;
                    } else {
                        m.phase = Phase::Unload { transport, empty_mass };
                        m.deadline = self.frame;
                    }
                }
            }
            Phase::Carry { transport, empty_mass } => {
                if !self.recovery_carried(m.cargo, transport, empty_mass) {
                    if self.recovery_grounded(m.cargo) {
                        self.recovery_verify(&mut m);
                    }
                } else if distance(transport.pos(), m.to) < 100.0 || transport.current_commands().is_empty() {
                    // Gunship MOVE can complete hundreds of elmos short of its
                    // target. UNLOAD supplies its own precise approach.
                    if !self.recovery_site(m.cargo, m.to, m.goal)
                        && let Some(to) = self.recovery_landing(m.cargo, m.goal, 160.0)
                    {
                        m.to = to;
                    }
                    transport.execute_custom_command(UNLOAD, &[m.to.x, m.to.y, m.to.z, 64.0], 0, NO_TIMEOUT);
                    m.phase = Phase::Unload { transport, empty_mass };
                    m.deadline = self.frame + 25 * 30;
                } else if self.war.get_aa_threat(transport.pos()) > 0.0
                    && let Some(to) = self.recovery_landing(m.cargo, m.goal, 160.0)
                {
                    m.to = to;
                    if let Some(path) = self.recovery_air_route(transport, to) {
                        for (i, p) in path.iter().enumerate() {
                            transport.move_to(*p, if i == 0 { 0 } else { opt::SHIFT_KEY });
                        }
                    }
                }
            }
            Phase::Unload { transport, empty_mass } => {
                if !self.recovery_carried(m.cargo, transport, empty_mass) && self.recovery_grounded(m.cargo) {
                    self.recovery_verify(&mut m);
                }
            }
            Phase::Verify { anchor } => {
                if self.recovery_grounded(m.cargo)
                    && distance(anchor, m.cargo.pos()) >= 80.0
                    && distance(m.from, m.cargo.pos()) >= 96.0
                {
                    self.recovery_finish(m, true);
                    return;
                }
            }
        }
        self.recovery.mission = Some(m);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::c_char;

    #[test]
    fn transport_selection_obeys_engine_flags_and_medium_cargo_fits_charon() {
        unsafe extern "C" fn air(_: i32, id: i32) -> bool {
            id == 99
        }
        unsafe extern "C" fn forbidden(_: i32, id: i32) -> bool {
            id == 98
        }
        unsafe extern "C" fn speed(_: i32, id: i32) -> f32 {
            if id == 97 { 0.0 } else { 60.0 }
        }
        unsafe extern "C" fn size(_: i32, id: i32) -> i32 {
            if id == 4 { 60 } else { 6 }
        }
        unsafe extern "C" fn capacity(_: i32, id: i32) -> i32 {
            if id == 100 { 4 } else { 25 }
        }
        unsafe extern "C" fn name(_: i32, id: i32) -> *const c_char {
            if id == 100 { c"gunshiptrans".as_ptr() } else { c"gunshipheavytrans".as_ptr() }
        }
        unsafe extern "C" fn params(_: i32, id: i32, keys: *mut *const c_char, values: *mut *const c_char) -> i32 {
            let key = match id {
                2 => c"requiremediumtrans",
                3 => c"requireheavytrans",
                _ => return 0,
            };
            if !keys.is_null() && !values.is_null() {
                unsafe {
                    keys.write(key.as_ptr());
                    values.write(c"1".as_ptr());
                }
            }
            1
        }
        let table = springai::ffi::SSkirmishAICallback {
            UnitDef_isAbleToFly: Some(air),
            UnitDef_isNotTransportable: Some(forbidden),
            UnitDef_getSpeed: Some(speed),
            UnitDef_getXSize: Some(size),
            UnitDef_getZSize: Some(size),
            UnitDef_getTransportSize: Some(capacity),
            UnitDef_getName: Some(name),
            UnitDef_getCustomParams: Some(params),
            ..Default::default()
        };
        let cb = unsafe { springai::Callback::from_raw(7100, &table) };
        let def = |id| UnitDef::from_id(cb, id).unwrap();
        assert!(fits(def(1), def(100)));
        assert!(fits(def(2), def(100)), "medium transport flag must not force Hercules");
        assert!(!fits(def(3), def(100)));
        assert!(fits(def(3), def(101)));
        for id in [4, 97, 98, 99] {
            assert!(!fits(def(id), def(101)), "ineligible cargo {id}");
        }
    }
}
