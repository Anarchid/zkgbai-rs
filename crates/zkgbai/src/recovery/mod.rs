//! Persistent movement failures, factory exit jams, and bounded rescue missions.
//! `disable=recovery` disables interventions. No fighter is scrapped for being slow.

mod evacuate;
mod local;
mod probe;

use std::collections::{BTreeMap, BTreeSet};

use springai::{
    Unit, Vec3,
    reserved::{self, CommandLease, Owner},
};

use crate::{
    Ai,
    economy::planning::{exit_lane, keeps_exit},
    terrain::move_class,
    util::distance,
};
use local::{CELL, Local, Obstacle, RADIUS};

const PERIOD: i32 = 30;
const STALL: i32 = 30 * 20;
const RETRY: i32 = 30 * 30;
const OWNER: Owner = Owner { module: "recovery", helpers: &[] };

#[derive(Default)]
#[checkpoint::state]
pub struct Recovery {
    probe: probe::Probe,
    watches: BTreeMap<i32, Watch>,
    births: BTreeMap<i32, (Unit, i32)>,
    failures: BTreeMap<i32, i32>,
    resolved: BTreeMap<i32, &'static str>,
    mission: Option<Mission>,
    demand: Option<(&'static str, i32)>,
    ordered: Option<i32>,
}

#[checkpoint::state]
struct Watch {
    unit: Unit,
    progress: Progress,
    goal: Vec3,
    retry: i32,
    attempted: u8,
}

#[derive(Clone, Copy)]
#[checkpoint::state]
struct Progress {
    low: Vec3,
    high: Vec3,
    goal: Vec3,
    best: f32,
    since: i32,
    last: i32,
}

impl Progress {
    fn new(at: Vec3, goal: Vec3, frame: i32) -> Self {
        Self { low: at, high: at, goal, best: distance(at, goal), since: frame, last: frame }
    }

    /// Time is eligible movement time, not time spent stunned, held or idle.
    /// Equivalent reissued commands do not reset the clock. Changing waypoints
    /// resets the distance baseline, but cannot disguise standing in one place.
    fn observe(&mut self, at: Vec3, goal: Vec3, frame: i32) -> bool {
        if frame - self.last > 2 * PERIOD {
            *self = Self::new(at, goal, frame);
            return false;
        }
        self.last = frame;
        if distance(goal, self.goal) > 64.0 {
            self.goal = goal;
            self.best = distance(at, goal);
        }
        let d = distance(at, goal);
        let explored = at.x < self.low.x - 96.0
            || at.z < self.low.z - 96.0
            || at.x > self.high.x + 96.0
            || at.z > self.high.z + 96.0;
        if explored || d + 32.0 < self.best {
            self.low.x = self.low.x.min(at.x);
            self.low.z = self.low.z.min(at.z);
            self.high.x = self.high.x.max(at.x);
            self.high.z = self.high.z.max(at.z);
            self.best = self.best.min(d);
            self.since = frame;
            return true;
        }
        false
    }
}

#[checkpoint::state]
struct Loan {
    unit: Unit,
    lease: CommandLease,
}

#[checkpoint::state]
struct Mission {
    method: &'static str,
    cargo: Unit,
    from: Vec3,
    goal: Vec3,
    to: Vec3,
    since: i32,
    deadline: i32,
    loans: Vec<Loan>,
    phase: Phase,
    // Ordered remaining removals; save/load must retain an active corridor.
    clearance: Vec<Unit>,
}

#[derive(Clone, Copy, Debug)]
#[checkpoint::state]
enum Phase {
    Walk,
    Clear { blocker: Unit, worker: Unit },
    Traffic,
    Lobster { launcher: Unit },
    Flight,
    Beacon { djinn: Unit, site: Vec3 },
    Teleport { djinn: Unit },
    Load { transport: Unit, empty_mass: f32 },
    Carry { transport: Unit, empty_mass: f32 },
    Unload { transport: Unit, empty_mass: f32 },
    Verify { anchor: Vec3 },
}

impl Ai {
    pub(crate) fn recovery_owns(&self, id: i32) -> bool {
        reserved::owner(self.cb, id).is_some_and(|o| o.module == "recovery")
    }

    pub(crate) fn recovery_created(&mut self, unit: Unit, builder: Option<Unit>) {
        if let Some(factory) =
            builder.filter(|b| b.team() == self.team_id && b.def().is_some_and(|d| keeps_exit(self.defs.name(d))))
        {
            self.recovery.births.insert(unit.id(), (factory, self.frame));
        }
    }

    pub(crate) fn recovery_move_failed(&mut self, unit: Unit) {
        if unit.team() == self.team_id {
            self.recovery.failures.insert(unit.id(), self.frame);
        }
    }

    pub(crate) fn recovery_transferred(&mut self, unit: Unit, new_team: i32) {
        if new_team == self.team_id {
            return;
        }
        let affected = self.recovery.mission.as_ref().is_some_and(|m| {
            m.loans.iter().any(|l| l.unit.id() == unit.id())
                || m.clearance.iter().any(|u| u.id() == unit.id())
                || matches!(m.phase, Phase::Clear { blocker, .. } if blocker.id() == unit.id())
        });
        if affected && let Some(m) = self.recovery.mission.take() {
            // Cancel reclaim immediately when ownership changes, rather than
            // letting a constructor work on a newly allied building until polling.
            self.recovery_finish(m, false);
        }
    }

    fn recovery_free(&self, unit: Unit) -> bool {
        let id = unit.id();
        unit.health() > 0.0
            && unit.team() == self.team_id
            && !unit.is_being_built()
            && !reserved::is_reserved(self.cb, id)
            && !self.dominatrix_vip(id)
            && !self.recovery_owns(id)
            && !self.eco_worker_held(id)
            && !reserved::owner(self.cb, id).is_some_and(|o| matches!(o.module, "coalesce" | "strikes" | "airstrikes"))
            && !self.coalesce_owns(id)
            && !self.squad_strike_owns(id)
            && !self.squad_in_arrowhead(id)
            && !self.limpet_controls(id)
            && !self.fire_controls(id)
            && !self.qrf_claims(id)
            && !self.teleports.evacuating(id)
            && !self.drops_busy(id)
            && !self.lobster_busy(id)
            && !self.jump_in_progress(unit)
            && !self.blasts_waiting(unit.pos())
            && !self.fighters.get(&id).is_some_and(|f| {
                f.squad
                    .is_some_and(|s| self.op.over.squad_intents.contains_key(&s) || self.op_squad_target(s).is_some())
            })
    }

    fn recovery_grounded(&self, unit: Unit) -> bool {
        let p = unit.pos();
        unit.def().is_some_and(|d| p.y <= self.cb.map().elevation_at(p.x, p.z).max(0.0) + d.height() + 15.0)
    }

    fn recovery_intent(&self, unit: Unit) -> Option<Vec3> {
        if !self.recovery_free(unit) || unit.is_paralyzed() || !self.recovery_grounded(unit) {
            return None;
        }
        let def = unit.def()?;
        if def.is_able_to_fly() || def.speed() <= 0.0 || def.move_data().is_none() {
            return None;
        }
        if unit.rules_param_float("slowState", 0.0) > 0.5 {
            return None;
        }
        let command = unit.current_commands().first().copied();
        // Explicit WAIT, GUARD, ATTACK, manual abilities and transport commands
        // are not a request to make ground progress.
        if command.is_some_and(|c| matches!(c.id(), 5 | 20 | 25 | 75 | 76 | 80 | 81 | 35171)) {
            return None;
        }
        let here = unit.pos();
        if let Some((factory, _)) = self.recovery.births.get(&unit.id())
            && let Some(d) = factory.def()
        {
            let lane = exit_lane(d, factory.pos(), factory.building_facing());
            let mut goal = Vec3::xz((lane[0] + lane[2]) * 0.5, (lane[1] + lane[3]) * 0.5);
            match factory.building_facing() {
                0 => goal.z = lane[3] + 96.0,
                1 => goal.x = lane[2] + 96.0,
                2 => goal.z = lane[1] - 96.0,
                _ => goal.x = lane[0] - 96.0,
            }
            if distance(here, factory.pos()) < 500.0 && distance(here, goal) > 96.0 {
                return Some(goal);
            }
        }
        if let Some(w) = self.eco.workers.get(&unit.id())
            && let Some(task) = w.task.and_then(|id| self.eco.tasks.get(id))
        {
            let goal = task.get_pos();
            if distance(here, goal) > def.build_distance() + 48.0 {
                return Some(goal);
            }
            return None;
        }
        if command.is_none()
            && let Some(w) = self.recovery.watches.get(&unit.id())
            && self.frame - w.progress.since < 180 * 30
            && (w.attempted != 0 || self.recovery.failures.contains_key(&unit.id()))
        {
            // Raw-move may drop a failed order. A confirmed enclosure still
            // needs relief after that happens; intentional WAIT/GUARD above
            // and operator reservations still cancel this remembered intent.
            return Some(w.goal);
        }
        let c = command?;
        if !matches!(c.id(), 10 | 16 | 31109) {
            return None;
        }
        // Fight orders legitimately stop to fire. Retreat MOVE orders do not.
        if c.id() == 16 && unit.weapons().iter().any(|w| w.reload_frame() >= self.frame - 30) {
            return None;
        }
        let p = c.params();
        if p.len() < 3 {
            return None;
        }
        let goal = Vec3::new(p[0], p[1], p[2]);
        (goal.x.is_finite() && goal.z.is_finite() && distance(here, goal) > 96.0).then_some(goal)
    }

    pub(crate) fn recovery_update(&mut self, frame: i32) {
        if (frame + self.offset) % PERIOD != 0 {
            return;
        }
        self.recovery_probe();
        if let Some(mission) = self.recovery.mission.take() {
            self.recovery_tick(mission);
        }
        if self.dev_off("recovery") || reserved::is_muted(self.cb, "recovery.update") {
            return;
        }
        let units = self.cb.team_units();
        let alive: BTreeSet<_> = units.iter().map(Unit::id).collect();
        self.recovery.births.retain(|id, (f, _born)| {
            alive.contains(id)
                && f.health() > 0.0
                && units.iter().find(|u| u.id() == *id).is_some_and(|u| distance(u.pos(), f.pos()) < 500.0)
        });
        self.recovery.resolved.retain(|id, _| alive.contains(id));
        self.recovery.failures.retain(|id, at| alive.contains(id) && frame - *at < 30 * 60);
        for unit in units {
            let Some(goal) = self.recovery_intent(unit) else {
                continue;
            };
            let watch = self.recovery.watches.entry(unit.id()).or_insert_with(|| Watch {
                unit,
                progress: Progress::new(unit.pos(), goal, frame),
                goal,
                retry: 0,
                attempted: 0,
            });
            if watch.progress.observe(unit.pos(), goal, frame) {
                watch.attempted = 0;
            }
            if frame - watch.progress.since > 180 * 30 {
                watch.attempted = 0;
                watch.progress.since = frame - STALL;
            }
            watch.goal = goal;
        }
        self.recovery.watches.retain(|id, w| {
            alive.contains(id) && frame - w.progress.last <= 2 * PERIOD
                || self.recovery.mission.as_ref().is_some_and(|m| m.cargo.id() == *id)
        });
        if self.recovery.mission.is_some() {
            return;
        }
        // One expensive local diagnosis per second, oldest first.
        let suspect = self
            .recovery
            .watches
            .values()
            .filter(|w| {
                let window = if self.eco.workers.contains_key(&w.unit.id())
                    || self.recovery.failures.get(&w.unit.id()).is_some_and(|at| frame - *at < 15 * 30)
                {
                    12 * 30
                } else {
                    STALL
                };
                frame - w.progress.since >= window && w.progress.last == frame && frame >= w.retry
            })
            .min_by_key(|w| w.progress.since)
            .map(|w| (w.unit, w.goal, w.attempted));
        if let Some((unit, goal, attempted)) = suspect {
            self.recovery.watches.get_mut(&unit.id()).unwrap().retry = frame + RETRY;
            self.recovery_diagnose(unit, goal, attempted);
        }
    }

    fn recovery_local(&self, unit: Unit) -> Option<Local> {
        self.recovery_local_at(unit, unit.pos(), true)
    }

    fn recovery_local_at(&self, unit: Unit, from: Vec3, leaving_factory: bool) -> Option<Local> {
        let def = unit.def()?;
        let class = move_class(def)?;
        let map = self.cb.map();
        let slopes = map.slope_map();
        let sw = map.width() as usize / 2;
        let (width, height) = (map.width() as f32 * 8.0, map.height() as f32 * 8.0);
        let pad = def.x_size().max(def.z_size()) as f32 * 4.0 + 4.0;
        let parent_unit = self.recovery.births.get(&unit.id()).filter(|_| leaving_factory).map(|(f, _)| *f);
        let parent = parent_unit.map(|f| f.id());
        let mut obstacles: Vec<_> = self
            .cb
            .friendly_units_in_cylinder(from, RADIUS * 1.5 + 200.0)
            .into_iter()
            .chain(self.cb.enemy_units_in_cylinder(from, RADIUS * 1.5 + 200.0))
            .filter(|u| u.id() != unit.id() && Some(u.id()) != parent)
            .filter_map(|u| {
                let d = u.def()?;
                if d.is_able_to_fly() || !self.recovery_grounded(u) {
                    return None;
                }
                let (x, z) = (d.x_size() as f32 * 4.0 + pad, d.z_size() as f32 * 4.0 + pad);
                let (hx, hz) = if u.building_facing() % 2 == 0 { (x, z) } else { (z, x) };
                Some(Obstacle { id: u.id(), at: u.pos(), hx, hz, mobile: d.speed() > 0.0 })
            })
            .chain(self.cb.features_in_cylinder(from, RADIUS * 1.5 + 200.0).into_iter().filter_map(|f| {
                let d = f.def()?;
                d.is_blocking().then_some(Obstacle {
                    id: -f.id() - 1,
                    at: f.pos(),
                    hx: d.x_size() as f32 * 4.0 + pad,
                    hz: d.z_size() as f32 * 4.0 + pad,
                    mobile: false,
                })
            }))
            .collect();
        // Factory yard maps are open at the production mouth. Model the
        // other three sides explicitly: ignoring the entire parent factory
        // would invent escape routes through its rear wall.
        if let Some(factory) = parent_unit
            && let Some(d) = factory.def()
        {
            let fp = factory.pos();
            let facing = factory.building_facing();
            let (x, z) = (d.x_size() as f32 * 4.0, d.z_size() as f32 * 4.0);
            let (hx, hz) = if facing % 2 == 0 { (x, z) } else { (z, x) };
            let sides = [
                (Vec3::xz(fp.x, fp.z + hz), hx + pad, pad),
                (Vec3::xz(fp.x + hx, fp.z), pad, hz + pad),
                (Vec3::xz(fp.x, fp.z - hz), hx + pad, pad),
                (Vec3::xz(fp.x - hx, fp.z), pad, hz + pad),
            ];
            for (side, (at, hx, hz)) in sides.into_iter().enumerate() {
                if side != facing as usize {
                    obstacles.push(Obstacle { id: factory.id(), at, hx, hz, mobile: false });
                }
            }
        }
        Some(Local::new(
            from,
            |p| {
                [-pad, 0.0, pad].into_iter().all(|dx| {
                    [-pad, 0.0, pad].into_iter().all(|dz| {
                        let (x, z) = (p.x + dx, p.z + dz);
                        x >= 0.0
                            && z >= 0.0
                            && x < width
                            && z < height
                            && slopes
                                .get((z / CELL) as usize * sw + (x / CELL) as usize)
                                .is_some_and(|&slope| !class.blocks(slope, -map.elevation_at(x, z)))
                    })
                })
            },
            obstacles,
        ))
    }

    fn recovery_diagnose(&mut self, unit: Unit, goal: Vec3, attempted: u8) {
        let Some(local) = self.recovery_local(unit) else {
            return;
        };
        let path = local.escape(goal, None, true);
        let static_path = local.escape(goal, None, false);
        self.log(&format!("recovery: {} #{} stalled at ({:.0},{:.0}); factory={}, moveFailed={}, static_exit={}, traffic_exit={}, attempts={attempted}",
            self.defs.name_of(unit), unit.id(), unit.pos().x, unit.pos().z, self.recovery.births.contains_key(&unit.id()),
            self.recovery.failures.contains_key(&unit.id()), static_path.is_some(), path.is_some()));
        if attempted & 1 == 0 {
            if let Some(path) = path.or(static_path) {
                let to = *path.last().unwrap();
                let mission = self.recovery_begin(unit, goal, to, Phase::Walk, 15 * 30, &[]);
                for (n, p) in path.iter().step_by(3).chain(path.last()).enumerate() {
                    unit.move_to(self.recovery_ground(*p), if n == 0 { 0 } else { springai::opt::SHIFT_KEY });
                }
                self.recovery.watches.get_mut(&unit.id()).unwrap().attempted |= 1;
                self.recovery.mission = Some(mission);
                return;
            }
            // No walk was attempted. A neighbour's clearance can open a
            // route before this unit moves; reconsider it on the next probe.
        }
        if attempted & 2 == 0 && local.escape(goal, None, true).is_none() {
            let blockers = self.cb.friendly_units_in_cylinder(unit.pos(), RADIUS);
            for blocker in blockers {
                if !self.recovery_free(blocker) {
                    continue;
                }
                let Some(d) = blocker.def() else {
                    continue;
                };
                if d.speed() <= 0.0 || d.is_able_to_fly() {
                    continue;
                }
                if local.escape(goal, Some(blocker.id()), true).is_none() {
                    continue;
                }
                if let Some(to) = self.recovery_landing(blocker, goal, 160.0)
                    && self.terrain_reaches(d, blocker.pos(), to, 32.0)
                {
                    let m = self.recovery_begin(unit, goal, unit.pos(), Phase::Traffic, 12 * 30, &[blocker]);
                    blocker.move_to(to, 0);
                    self.recovery.watches.get_mut(&unit.id()).unwrap().attempted |= 2;
                    self.recovery.mission = Some(m);
                    return;
                }
            }
            let candidates: BTreeMap<_, _> = self
                .cb
                .friendly_units_in_cylinder(unit.pos(), RADIUS * 1.5)
                .into_iter()
                .filter(|&u| self.recovery_free(u) && self.recovery_expendable(u))
                .map(|u| (u.id(), u))
                .collect();
            let prices =
                candidates.iter().map(|(&id, u)| (id, u.def().unwrap().cost(self.metal).ceil() as u32)).collect();
            if let Some(ids) = local.clearance(goal, &prices) {
                let blockers: Vec<_> = ids.iter().map(|id| candidates[id]).collect();
                if self.recovery_clearance_safe(&blockers) {
                    let blocker = blockers[0];
                    let worker = self
                        .eco
                        .workers
                        .values()
                        .map(|w| w.unit)
                        .find(|&w| self.recovery_free(w) && self.recovery_reclaim_path(w, blocker).is_some());
                    if let Some(worker) = worker {
                        let mut m = self.recovery_begin(
                            unit,
                            goal,
                            unit.pos(),
                            Phase::Clear { blocker, worker },
                            30 * 30,
                            &[worker],
                        );
                        m.clearance = blockers[1..].to_vec();
                        if blockers.iter().any(|u| self.defs.name_of(*u) == "energywind") {
                            m.method = "wind";
                        }
                        if self.recovery_order_clear(worker, blocker) {
                            self.log(&format!("recovery: clear corridor {:?} for #{}", ids, unit.id()));
                            self.recovery.watches.get_mut(&unit.id()).unwrap().attempted |= 2;
                            self.recovery.mission = Some(m);
                        } else {
                            self.recovery_finish(m, false);
                        }
                        return;
                    }
                }
            }
        }
        self.recovery_evacuate(unit, goal, attempted);
    }

    fn recovery_ground(&self, p: Vec3) -> Vec3 {
        Vec3::new(p.x, self.cb.map().elevation_at(p.x, p.z), p.z)
    }

    fn recovery_expendable(&self, unit: Unit) -> bool {
        unit.team() == self.team_id
            && matches!(self.defs.name_of(unit), "energysolar" | "energywind")
            // Initial candidates pass recovery_free (finished units only).
            // Reclaim itself lowers build progress; keep that active target
            // eligible until it disappears instead of aborting a partial cut.
            && unit.def().is_some_and(|d| d.is_reclaimable() && d.cost(self.metal) <= 100.0)
    }

    fn recovery_clearance_safe(&self, blockers: &[Unit]) -> bool {
        if self.eco.hard_estalled || blockers.iter().any(|&u| !self.recovery_expendable(u)) {
            return false;
        }
        let loss: f32 = blockers
            .iter()
            .map(|u| {
                // Wind output varies. Budget at least its current output and a
                // conservative 2.5 E floor, rather than assuming Solar's fixed 2 E.
                let floor = if self.defs.name_of(*u) == "energywind" { 2.5 } else { 2.0 };
                u.rules_param_float("current_energyIncome", floor).max(floor)
            })
            .sum();
        if self.eco.effective_income_energy - loss < self.eco.effective_expenditure {
            return false;
        }
        let nodes: Vec<_> = self
            .cb
            .friendly_units()
            .into_iter()
            .filter_map(|u| {
                let r = u.def()?.custom_param("pylonrange")?.parse::<f32>().ok()?;
                (r > 0.0 && !u.is_being_built()).then_some((u.id(), u.pos(), r))
            })
            .collect();
        preserves_grid(&nodes, &blockers.iter().map(Unit::id).collect::<Vec<_>>())
    }

    fn recovery_reclaim_path(&self, worker: Unit, blocker: Unit) -> Option<Vec<Vec3>> {
        let d = worker.def()?;
        if !d.is_able_to_reclaim() || self.war.get_effective_threat(worker.pos()) > 0.0 {
            return None;
        }
        let range = d.build_distance() * 0.85;
        if distance(worker.pos(), blocker.pos()) < range {
            return Some(Vec::new());
        }
        if distance(worker.pos(), blocker.pos()) > RADIUS + range {
            return None;
        }
        let path = self.recovery_local(worker)?.within(blocker.pos(), range)?;
        path.iter().all(|&p| self.war.get_effective_threat(p) <= 0.0).then_some(path)
    }

    fn recovery_order_clear(&self, worker: Unit, blocker: Unit) -> bool {
        let Some(path) = self.recovery_reclaim_path(worker, blocker) else {
            return false;
        };
        for (n, p) in path.iter().step_by(3).chain(path.last()).enumerate() {
            worker.move_to(self.recovery_ground(*p), if n == 0 { 0 } else { springai::opt::SHIFT_KEY });
        }
        worker.reclaim_unit(blocker, if path.is_empty() { 0 } else { springai::opt::SHIFT_KEY })
    }

    fn recovery_begin(
        &self,
        cargo: Unit,
        goal: Vec3,
        to: Vec3,
        phase: Phase,
        duration: i32,
        helpers: &[Unit],
    ) -> Mission {
        let method = match phase {
            Phase::Walk => "walk",
            Phase::Clear { blocker, .. } if self.defs.name_of(blocker) == "energywind" => "wind",
            Phase::Clear { .. } => "solar",
            Phase::Traffic => "traffic",
            Phase::Lobster { .. } => "lobster",
            Phase::Beacon { .. } => "djinn",
            Phase::Load { transport, .. } if self.defs.name_of(transport) == "gunshipheavytrans" => "hercules",
            Phase::Load { .. } => "charon",
            _ => "verify",
        };
        self.log(&format!("recovery: {method} rescue starts for #{}", cargo.id()));
        let mut ids = BTreeSet::new();
        let loans = std::iter::once(cargo)
            .chain(helpers.iter().copied())
            .filter(|u| ids.insert(u.id()))
            .map(|unit| Loan { unit, lease: reserved::lend(self.cb, unit.id(), OWNER) })
            .collect();
        Mission {
            method,
            cargo,
            from: cargo.pos(),
            goal,
            to,
            since: self.frame,
            deadline: self.frame + duration,
            loans,
            phase,
            clearance: Vec::new(),
        }
    }

    fn recovery_finish(&mut self, mission: Mission, success: bool) {
        self.log(&format!(
            "recovery: #{} {} after {:.0}s",
            mission.cargo.id(),
            if success { "moving again" } else { "attempt ended; reassess" },
            (self.frame - mission.since) as f32 / 30.0
        ));
        let unloading = match mission.phase {
            Phase::Load { transport, .. } | Phase::Carry { transport, .. } | Phase::Unload { transport, .. }
                if transport
                    .def()
                    .is_some_and(|d| transport.rules_param_float("massOverride", d.mass()) > d.mass() + 1.0) =>
            {
                Some(transport.id())
            }
            _ => None,
        };
        for loan in mission.loans {
            if Some(loan.unit.id()) != unloading
                && loan.lease.active()
                && loan.unit.health() > 0.0
                && loan.unit.team() == self.team_id
            {
                if success && loan.unit.id() == mission.cargo.id() {
                    loan.unit.move_to(self.recovery_ground(mission.goal), 0);
                } else {
                    loan.unit.stop(0);
                }
            }
            loan.lease.release();
        }
        if let Some(w) = self.recovery.watches.get_mut(&mission.cargo.id()) {
            w.progress = Progress::new(mission.cargo.pos(), mission.goal, self.frame);
            w.retry = self.frame + if success { RETRY } else { 5 * 30 };
            if success {
                w.attempted = 0;
            }
        }
        if success {
            self.recovery.demand = None;
            // Preserve the substantive relief method if a later local walk
            // finishes clearing the same enclosure.
            if mission.method != "walk" || !self.recovery.resolved.contains_key(&mission.cargo.id()) {
                self.recovery.resolved.insert(mission.cargo.id(), mission.method);
            }
            self.recovery.failures.remove(&mission.cargo.id());
        }
    }
}

/// Removing a corridor must not split any previously connected surviving grid
/// nodes. Redundant chains count: requiring every neighbour to link directly
/// incorrectly rejects ordinary wind farms with alternate paths.
fn preserves_grid(nodes: &[(i32, Vec3, f32)], removed: &[i32]) -> bool {
    let components = |exclude: bool| {
        let mut group = vec![usize::MAX; nodes.len()];
        for start in 0..nodes.len() {
            if group[start] != usize::MAX || (exclude && removed.contains(&nodes[start].0)) {
                continue;
            }
            group[start] = start;
            let mut pending = vec![start];
            while let Some(i) = pending.pop() {
                for j in 0..nodes.len() {
                    if group[j] == usize::MAX
                        && !(exclude && removed.contains(&nodes[j].0))
                        && distance(nodes[i].1, nodes[j].1) <= nodes[i].2 + nodes[j].2
                    {
                        group[j] = start;
                        pending.push(j);
                    }
                }
            }
        }
        group
    };
    let (before, after) = (components(false), components(true));
    let mut mapping = BTreeMap::new();
    for i in 0..nodes.len() {
        if removed.contains(&nodes[i].0) {
            continue;
        }
        if let Some(old) = mapping.insert(before[i], after[i])
            && old != after[i]
        {
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn grid_allows_redundant_wind_chain_but_preserves_bridges() {
        let nodes = [
            (1, Vec3::xz(0.0, 0.0), 63.0),
            (2, Vec3::xz(100.0, 0.0), 63.0),
            (3, Vec3::xz(200.0, 0.0), 63.0),
            (4, Vec3::xz(0.0, 100.0), 63.0),
            (5, Vec3::xz(100.0, 100.0), 63.0),
            (6, Vec3::xz(200.0, 100.0), 63.0),
        ];
        assert!(preserves_grid(&nodes, &[2]));
        assert!(preserves_grid(&nodes, &[1, 2]));
        assert!(!preserves_grid(&nodes, &[2, 5]));
        assert!(!preserves_grid(&nodes[..3], &[2]));
    }

    #[test]
    fn active_clearance_roundtrip_preserves_order_deadlines_and_leases() {
        let raw = unsafe { std::mem::zeroed() };
        let cb = unsafe { springai::Callback::from_raw(19, &raw) };
        let unit = |id| Unit::from_id(cb, id).unwrap();
        let m = Mission {
            method: "wind",
            cargo: unit(1),
            from: Vec3::xz(12.0, 34.0),
            goal: Vec3::xz(500.0, 100.0),
            to: Vec3::xz(12.0, 34.0),
            since: 1500,
            deadline: 2400,
            loans: vec![Loan { unit: unit(2), lease: reserved::lend(cb, 2, OWNER) }],
            phase: Phase::Clear { blocker: unit(3), worker: unit(2) },
            clearance: vec![unit(5), unit(4)],
        };
        let bytes = checkpoint::encode(&m).unwrap();
        let restored: Mission = springai::snapshot::with_callback(cb, || {
            checkpoint::with_literals(&["wind"], || checkpoint::rmp_serde::from_slice(&bytes))
        })
        .unwrap();
        assert_eq!(checkpoint::encode(&restored).unwrap(), bytes);
        assert_eq!(restored.clearance.iter().map(Unit::id).collect::<Vec<_>>(), vec![5, 4]);
        assert_eq!(restored.deadline, 2400);
        assert!(restored.loans[0].lease.active());
        for loan in restored.loans {
            loan.lease.release();
        }
    }

    #[test]
    fn reorders_and_small_oscillation_cannot_hide_a_stall() {
        let mut p = Progress::new(Vec3::xz(0.0, 0.0), Vec3::xz(500.0, 0.0), 0);
        for frame in (30..=900).step_by(30) {
            let x = if frame % 60 == 0 { 8.0 } else { -8.0 };
            p.observe(Vec3::xz(x, 0.0), Vec3::xz(500.0 + x, 0.0), frame);
        }
        assert_eq!(p.since, 0);
    }
    #[test]
    fn gradual_progress_accumulates_and_command_churn_does_not() {
        let goal = Vec3::xz(500.0, 0.0);
        let mut walking = Progress::new(Vec3::xz(0.0, 0.0), goal, 0);
        for step in 1..=20 {
            walking.observe(Vec3::xz(step as f32 * 8.0, 0.0), goal, step * 30);
        }
        assert!(walking.since >= 450);
        let mut stuck = Progress::new(Vec3::xz(0.0, 0.0), goal, 0);
        for step in 1..=30 {
            stuck.observe(Vec3::xz(0.0, 0.0), Vec3::xz(500.0, if step % 2 == 0 { 300.0 } else { -300.0 }), step * 30);
        }
        assert_eq!(stuck.since, 0);
    }

    #[test]
    fn revisiting_a_bounded_pen_is_not_new_progress() {
        let goal = Vec3::xz(1000.0, 0.0);
        let mut p = Progress::new(Vec3::xz(0.0, 0.0), goal, 0);
        for step in 1..=60 {
            let at = if step % 2 == 0 { Vec3::xz(0.0, 120.0) } else { Vec3::xz(0.0, -120.0) };
            p.observe(at, goal, step * 30);
        }
        assert!(p.since <= 60, "repeated oscillation must eventually count as stalled");
    }

    #[test]
    fn detours_and_ineligible_time_reset_stall_evidence() {
        let goal = Vec3::xz(500.0, 0.0);
        let mut p = Progress::new(Vec3::xz(0.0, 0.0), goal, 0);
        assert!(p.observe(Vec3::xz(0.0, 100.0), goal, 30));
        assert_eq!(p.since, 30);
        assert!(!p.observe(Vec3::xz(0.0, 100.0), goal, 600));
        assert_eq!(p.since, 600);
    }
}
