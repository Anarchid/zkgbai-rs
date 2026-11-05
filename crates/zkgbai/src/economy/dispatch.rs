//! Persistent execution of economic projects. Prices allocate free hands;
//! only explicit requests, invalidation, danger or observed failure release
//! an incumbent. TaskStore remains the project ledger, including nanoframes
//! and prerequisites; temporary work never deletes the interrupted project.
use std::collections::BTreeSet;

use super::{Ai, TaskId, TaskKind, Worker, WorkerTask};
use crate::util::distance;

const STALL: i32 = 30 * 20;
const SHUN: i32 = 30 * 30;
const ENERGY_WINDOW: f32 = 30.0;

#[derive(Clone, Copy, Debug)]
pub(super) struct Lease {
    task: TaskId,
    seen: i32,
    gained: i32,
    distance: f32,
    progress: f32,
}

impl Lease {
    fn observe(&mut self, frame: i32, distance: f32, progress: f32, working: bool, resource_wait: bool) -> bool {
        // A tactical controller/recovery may have owned the worker between
        // observations. Its time is not evidence of failed construction.
        if frame - self.seen > 90
            || distance < self.distance - 16.0
            || progress > self.progress + 0.001
            || working
            || resource_wait
        {
            self.gained = frame;
            self.distance = distance;
            self.progress = progress;
        }
        self.seen = frame;
        frame - self.gained < STALL
    }
}

fn progress(t: &WorkerTask) -> f32 {
    match &t.kind {
        TaskKind::Construction(c) => c.target.map_or(0.0, |u| u.build_progress()),
        TaskKind::Repair(r) => r.target.health() / r.target.max_health().max(1.0),
        TaskKind::Reclaim(r) => 1.0 - r.target.reclaim_left(),
        TaskKind::CombatReclaim(r) => 1.0 - r.target.health() / r.target.max_health().max(1.0),
    }
}

impl Ai {
    pub(super) fn eco_dispatch_on(&self) -> bool {
        !self.dev_off("economydispatch")
    }

    pub(super) fn dispatch_valid(&self, w: &Worker, t: &WorkerTask) -> bool {
        let friendly = |u: springai::Unit| u.team() == self.team_id || self.allies.contains(&u.team());
        let alive = match &t.kind {
            TaskKind::Construction(c) => c.target.is_none_or(|u| u.health() > 0.0 && u.is_being_built() && friendly(u)),
            TaskKind::Repair(r) => {
                r.target.health() > 0.0
                    && friendly(r.target)
                    && (r.target.is_being_built() || r.target.health() < r.target.max_health())
            }
            TaskKind::Reclaim(r) => r.target.def().is_some() && r.target.reclaim_left() > 0.0,
            TaskKind::CombatReclaim(r) => {
                r.target.health() > 0.0 && (r.target.is_being_built() || r.target.team() == self.team_id)
            }
        };
        alive
            && self.eco_unified_allows(w, t)
            && self.eco_worker_can_take(w, t)
            && (self.op_priority(t.id) || self.logistics_allows(w.id, t.id))
    }

    pub(super) fn dispatch_safe(&self, w: &Worker, t: &WorkerTask) -> bool {
        if self.eco_build_guard_task(w.id) == Some(t.id) || (w.task == Some(t.id) && self.logistics_holds(w.id)) {
            return true;
        }
        let v = self.war.view(w.unit.def());
        v.get_effective_threat(t.get_pos()) / self.op_greed() <= v.get_total_friendly_threat(t.get_pos())
    }

    /// Priority is an explicit requester, never an ordinary price crossing.
    fn dispatch_request(&self, w: &Worker, t: &WorkerTask) -> Option<(u8, &'static str)> {
        if self.op_priority(t.id) {
            Some((0, "operator"))
        } else if t.as_construction().is_some_and(|c| c.fac_plop && c.assigned_plop == w.id) {
            Some((1, "factory plop"))
        } else if self.eco_factory_defense_hand(w, t) {
            Some((2, "factory defense"))
        } else if self.conquest_bonus(w.id, t.id).is_some() {
            Some((3, "conquest mission"))
        } else if self.logistics_assignment(w.id) == Some(t.id) {
            Some((4, "project crew"))
        } else {
            None
        }
    }

    pub(super) fn eco_suspend_assignment(&mut self, wid: i32, why: &'static str) {
        if !self.eco_dispatch_on() {
            return;
        }
        let Some(w) = self.eco.workers.get_mut(&wid) else { return };
        let Some(task) = w.task else { return };
        if !w.suspended.contains(&task) {
            w.suspended.push(task);
        }
        w.lease = None;
        self.op_emit("dispatch", serde_json::json!({"worker":wid,"event":"suspend","task":task,"reason":why}));
    }

    fn dispatch_release(&mut self, wid: i32, why: &'static str, suspend: bool) {
        let old = self.eco.workers.get(&wid).and_then(|w| w.task);
        if suspend {
            self.eco_suspend_assignment(wid, why);
        }
        if let Some(w) = self.eco.workers.get_mut(&wid) {
            w.clear_task(&mut self.eco.tasks);
            w.approach = None;
            w.lease = None;
        }
        if let Some(task) = old {
            self.op_emit("dispatch", serde_json::json!({"worker":wid,"event":"release","task":task,"reason":why}));
        }
    }

    fn dispatch_observe(&mut self, wid: i32, task: TaskId) -> bool {
        let Some(w) = self.eco.workers.get(&wid) else { return false };
        let Some(t) = self.eco.tasks.get(task) else { return false };
        let distance = distance(w.get_pos(), t.get_pos());
        let progress = progress(t);
        let working = t.build_command_matches(w.unit) && w.unit.resource_use(self.metal) > 0.0;
        let needs_resources = matches!(t.kind, TaskKind::Construction(_) | TaskKind::Repair(_));
        let in_reach = distance <= w.unit.def().map_or(0.0, |d| d.build_distance()) + 64.0;
        let resource_wait = needs_resources
            && in_reach
            && t.build_command_matches(w.unit)
            && (self.eco.hard_estalled || self.eco.metal < 5.0);
        let frame = self.frame;
        let w = self.eco.workers.get_mut(&wid).unwrap();
        let lease = w.lease.get_or_insert(Lease { task, seen: frame, gained: frame, distance, progress });
        if lease.task != task {
            *lease = Lease { task, seen: frame, gained: frame, distance, progress };
        }
        lease.observe(frame, distance, progress, working, resource_wait)
    }

    /// An energy emergency is a capacity request, not permission for every
    /// worker to abandon work. Only near-term, staffed, available projects
    /// count; an unstaffed or distant reactor cannot mask a local shortage.
    fn dispatch_energy_hand(&self) -> Option<i32> {
        if !self.eco.hard_estalled {
            return None;
        }
        let mut projects = BTreeSet::new();
        let mut pending = 0.0;
        for w in self.eco.workers.values().filter(|w| self.dispatch_present(w)) {
            let Some(t) = w.task.and_then(|id| self.eco.tasks.get(id)) else { continue };
            if !self.eco_energy_job(t) || !self.dispatch_valid(w, t) || !self.dispatch_safe(w, t) {
                continue;
            }
            let (plant, target) = match &t.kind {
                TaskKind::Construction(c) => (c.build_type, c.target),
                TaskKind::Repair(r) => {
                    let Some(def) = r.target.def() else { continue };
                    (def, Some(r.target))
                }
                _ => continue,
            };
            let d = w.unit.def();
            let travel = (self.eco_worker_route(w, t.get_pos()) - d.map_or(0.0, |d| d.build_distance())).max(0.0)
                / d.map_or(1.0, |d| d.speed()).max(1.0);
            let left = plant.cost(self.metal) * (1.0 - target.map_or(0.0, |u| u.build_progress()));
            let rate = w.bp.min(self.eco.effective_income.max(1.0)).min(self.eco.effective_income_energy.max(1.0));
            if travel + left / rate.max(1.0) > ENERGY_WINDOW
                || !projects.insert(target.map_or((false, t.id as i32), |u| (true, u.id())))
            {
                continue;
            }
            let output = if self.defs.name(plant) == "energywind" { 1.0 } else { super::wind::energy_make(plant) };
            pending += output.max(0.0);
        }
        let deficit = (self.eco.static_income + self.eco.energy_upkeep - self.eco.effective_income_energy).max(2.0);
        if pending >= deficit {
            return None;
        }
        self.eco
            .workers
            .values()
            .filter(|w| self.dispatch_available(w))
            .filter(|w| {
                !w.task
                    .and_then(|id| self.eco.tasks.get(id))
                    .is_some_and(|t| self.eco_energy_job(t) || self.dispatch_request(w, t).is_some())
            })
            .min_by(|a, b| {
                let cost = |w: &Worker| {
                    w.task.and_then(|id| self.eco.tasks.get(id)).map_or(0.0, |t| {
                        let left =
                            t.as_construction().map_or(10.0, |c| c.build_type.cost(self.metal) * (1.0 - progress(t)));
                        // Idle hands first, then those giving up least remaining
                        // work. Dedicated energy partners break ordinary ties.
                        10.0 + left / w.bp.max(1.0)
                    }) + if w.is_industrious { 0.0 } else { 5.0 }
                };
                cost(a).total_cmp(&cost(b)).then(a.id.cmp(&b.id))
            })
            .map(|w| w.id)
    }

    fn dispatch_available(&self, w: &Worker) -> bool {
        self.dispatch_present(w) && !self.logistics_holds(w.id)
    }

    fn dispatch_present(&self, w: &Worker) -> bool {
        !w.is_chicken
            && w.unit.health() > 0.0
            && w.unit.team() == self.team_id
            && !w.unit.is_being_built()
            && !self.eco_worker_externally_held(w.id)
            && !self.recovery_owns(w.id)
            && !self.jump_in_progress(w.unit)
            && !self.eco_in_transit(w.id)
            && !springai::reserved::is_reserved(self.cb, w.id)
            && !(w.assigned_plop && w.has_plop())
    }

    pub(super) fn eco_dispatch_job(&mut self, wid: i32) -> Option<TaskId> {
        let w = self.eco.workers.get(&wid)?;
        if w.is_chicken {
            return None;
        }
        let old = w.task;
        let valid = old.and_then(|id| self.eco.tasks.get(id)).is_some_and(|t| self.dispatch_valid(w, t));
        let safe = old.and_then(|id| self.eco.tasks.get(id)).is_some_and(|t| self.dispatch_safe(w, t));
        let request = self
            .eco
            .construction_tasks
            .iter()
            .chain(&self.eco.repair_tasks)
            .filter_map(|id| self.eco.tasks.get(*id))
            .filter(|t| self.dispatch_valid(w, t) && self.dispatch_safe(w, t))
            .filter_map(|t| {
                self.dispatch_request(w, t).map(|(p, why)| (t.id, p, why, self.eco_cost_of_job(w, w.get_pos(), t)))
            })
            .min_by(|a, b| a.1.cmp(&b.1).then((old != Some(a.0)).cmp(&(old != Some(b.0)))).then(a.3.total_cmp(&b.3)));
        let energy = self.dispatch_energy_hand() == Some(wid);
        if old.is_some() && (!valid || !safe) {
            self.dispatch_release(wid, if !valid { "invalid or unavailable" } else { "danger" }, true);
        } else if let Some(current) = old {
            if !self.dispatch_observe(wid, current) {
                let pos = self.eco.task_pos(current);
                self.eco.workers.get_mut(&wid)?.shunned.push((pos, self.frame + SHUN));
                self.dispatch_release(wid, "no progress with resources available", false);
            }
        }
        let current = self.eco.workers.get(&wid)?.task;
        if let Some((next, _, why, _)) =
            request.filter(|r| self.eco.tasks.get(r.0).is_some_and(|t| self.dispatch_valid(&self.eco.workers[&wid], t)))
        {
            if current != Some(next) {
                if current.is_some() {
                    self.dispatch_release(wid, why, true);
                }
                self.dispatch_note(wid, next, why);
            }
            return Some(next);
        }
        if energy
            && let Some(next) = self.eco_local_energy_task(wid)
            && self.eco.tasks.get(next).is_some_and(|t| {
                self.dispatch_valid(&self.eco.workers[&wid], t) && self.dispatch_safe(&self.eco.workers[&wid], t)
            })
        {
            if current != Some(next) {
                if current.is_some() {
                    self.dispatch_release(wid, "energy capacity", true);
                }
                self.dispatch_note(wid, next, "energy capacity");
            }
            return Some(next);
        }
        if let Some(current) = current {
            return Some(current);
        }
        // Interrupted projects retain their identity and remaining work.
        // Failed admission does not erase them: a defense pause may end later.
        let suspended = self.eco.workers.get(&wid)?.suspended.clone();
        let w = self.eco.workers.get(&wid)?;
        let resume =
            suspended.iter().rev().copied().find(|id| {
                self.eco.tasks.get(*id).is_some_and(|t| self.dispatch_valid(w, t) && self.dispatch_safe(w, t))
            });
        self.eco.workers.get_mut(&wid)?.suspended.retain(|id| self.eco.tasks.get(*id).is_some() && Some(*id) != resume);
        if let Some(next) = resume {
            self.dispatch_note(wid, next, "resume");
            return Some(next);
        }
        let next = self.eco_get_cheapest_job(wid)?;
        let w = self.eco.workers.get(&wid)?;
        let t = self.eco.tasks.get(next)?;
        if !self.dispatch_valid(w, t) || !self.dispatch_safe(w, t) {
            return None;
        }
        self.dispatch_note(wid, next, "available worker");
        Some(next)
    }

    fn dispatch_note(&self, wid: i32, task: TaskId, why: &'static str) {
        let p = self.eco.task_pos(task);
        self.op_emit(
            "dispatch",
            serde_json::json!({"worker":wid,"event":"assign","task":task,"reason":why,"site":[p.x,p.z]}),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::super::assign::tests::{assignment_ai, build_command};
    use super::*;
    use springai::{Unit, UnitDef, Vec3};
    use std::cell::Cell;

    thread_local! {
        static SPEND: Cell<f32> = const { Cell::new(5.0) };
        static PROGRESS: Cell<f32> = const { Cell::new(0.3) };
        static DONE: Cell<bool> = const { Cell::new(false) };
        static ORDERS: Cell<usize> = const { Cell::new(0) };
        static DEFS: Cell<bool> = const { Cell::new(false) };
    }
    unsafe extern "C" fn spending(_: i32, _: i32, _: i32) -> f32 {
        SPEND.get()
    }
    unsafe extern "C" fn progress(_: i32, _: i32) -> f32 {
        PROGRESS.get()
    }
    unsafe extern "C" fn building(_: i32, id: i32) -> bool {
        id >= 90 && !DONE.get()
    }
    unsafe extern "C" fn pos(_: i32, _: i32, p: *mut f32) {
        unsafe { std::ptr::copy_nonoverlapping([100.0, 0.0, 100.0].as_ptr(), p, 3) };
    }
    unsafe extern "C" fn scalar(_: i32, _: i32) -> f32 {
        100.0
    }
    unsafe extern "C" fn defs(_: i32, p: *mut i32, n: i32) -> i32 {
        if !DEFS.get() {
            return 0;
        }
        if !p.is_null() && n >= 2 {
            unsafe {
                *p = 1;
                *p.add(1) = 2;
            }
        }
        2
    }
    unsafe extern "C" fn name(_: i32, id: i32) -> *const std::ffi::c_char {
        if id == 2 { c"energysolar".as_ptr() } else { c"staticmex".as_ptr() }
    }
    unsafe extern "C" fn params(
        _: i32,
        id: i32,
        keys: *mut *const std::ffi::c_char,
        values: *mut *const std::ffi::c_char,
    ) -> i32 {
        if id != 2 {
            return 0;
        }
        if !keys.is_null() && !values.is_null() {
            unsafe {
                *keys = c"income_energy".as_ptr();
                *values = c"2".as_ptr();
            }
        }
        1
    }
    unsafe extern "C" fn command(_: i32, _: i32, _: i32, _: i32, _: *mut std::ffi::c_void) -> i32 {
        ORDERS.set(ORDERS.get() + 1);
        0
    }
    fn setup() -> (Ai, TaskId, TaskId) {
        SPEND.set(5.0);
        PROGRESS.set(0.3);
        DONE.set(false);
        DEFS.set(false);
        let mut ai = assignment_ai(|t| {
            t.Unit_getResourceUse = Some(spending);
            t.Engine_handleCommand = Some(command);
            t.Unit_getBuildProgress = Some(progress);
            t.Unit_isBeingBuilt = Some(building);
            t.Unit_getPos = Some(pos);
            t.Unit_getMaxHealth = Some(scalar);
            t.UnitDef_getBuildDistance = Some(scalar);
            t.getUnitDefs = Some(defs);
            t.UnitDef_getName = Some(name);
            t.UnitDef_getCustomParams = Some(params);
        });
        DEFS.set(true);
        ai.defs = crate::defs::Defs::new(ai.cb);
        ai.eco.build_ids.mex_id = 1;
        ai.eco.build_ids.solar_id = 2;
        ai.eco.metal = 100.0;
        ai.eco.effective_income = 10.0;
        ai.eco.effective_income_energy = 10.0;
        let mut mex = WorkerTask::construction(UnitDef::from_id(ai.cb, 1).unwrap(), Vec3::xz(100.0, 100.0), 0);
        mex.as_construction_mut().unwrap().target = Unit::from_id(ai.cb, 90);
        let a = ai.eco.tasks.insert(mex);
        let b = ai.eco.tasks.insert(WorkerTask::construction(
            UnitDef::from_id(ai.cb, 2).unwrap(),
            Vec3::xz(140.0, 100.0),
            0,
        ));
        ai.eco.construction_tasks.extend([a, b]);
        ai.eco.solar_tasks.push(b);
        for id in [10, 11] {
            ai.eco.workers.insert(id, Worker::new(Unit::from_id(ai.cb, id).unwrap(), ai.team_id));
        }
        ai.eco.workers.get_mut(&10).unwrap().set_task(&mut ai.eco.tasks, a, 0);
        build_command(10, 40, vec![90.0]);
        (ai, a, b)
    }

    #[test]
    fn incumbent_survives_resource_fluctuations_helpers_and_repaired_orders() {
        let (mut ai, a, b) = setup();
        for frame in (0..1800).step_by(30) {
            ai.frame = frame;
            ai.eco.hard_estalled = false;
            ai.eco.metal = if frame % 60 == 0 { 0.0 } else { 100.0 };
            SPEND.set(if ai.eco.metal == 0.0 { 0.0 } else { 5.0 });
            // A helper coming and going never changes ownership.
            let w = ai.eco.workers.get_mut(&11).unwrap();
            w.clear_task(&mut ai.eco.tasks);
            if frame % 60 == 0 {
                w.set_task(&mut ai.eco.tasks, a, frame);
            }
            assert_eq!(ai.eco_dispatch_job(10), Some(a));
        }
        ai.eco.hard_estalled = false;
        build_command(10, 10, vec![150.0, 0.0, 100.0]); // a retreat/move displaced the build
        assert_eq!(ai.eco_dispatch_job(10), Some(a), "repair the order instead of auctioning the worker");
        ai.op.over.priority_tasks.insert(b);
        assert_eq!(ai.eco_dispatch_job(10), Some(b));
        assert!(ai.eco.workers[&10].suspended.contains(&a));
        assert!(ai.eco.tasks.get(a).is_some(), "interruption keeps the project");
        ai.eco.workers.get_mut(&10).unwrap().set_task(&mut ai.eco.tasks, b, ai.frame);
        ai.op.over.priority_tasks.clear();
        ai.eco.workers.get_mut(&10).unwrap().end_task();
        assert_eq!(ai.eco_dispatch_job(10), Some(a), "resume before a new auction");
    }

    #[test]
    fn retained_assignment_keeps_its_out_of_range_build_order() {
        let (mut ai, a, _) = setup();
        let t = ai.eco.tasks.get_mut(a).unwrap();
        t.position = Vec3::xz(2000.0, 100.0);
        t.as_construction_mut().unwrap().target = None;
        build_command(10, -1, vec![2000.0, 0.0, 100.0]);
        ORDERS.set(0);
        for frame in (0..300).step_by(10) {
            ai.frame = frame;
            ai.eco_assign_worker(10);
            assert_eq!(ai.eco.workers[&10].task, Some(a));
        }
        assert_eq!(ORDERS.get(), 0, "do not restart travel while the accepted build is queued");
    }

    #[test]
    fn energy_recruits_idle_hand_and_counts_its_committed_output_once() {
        let (mut ai, a, b) = setup();
        ai.eco.hard_estalled = true;
        ai.eco.static_income = 12.0;
        assert_eq!(ai.dispatch_energy_hand(), Some(11), "idle hand before interrupting a mex");
        assert_eq!(ai.eco_dispatch_job(11), Some(b));
        ai.eco.workers.get_mut(&11).unwrap().set_task(&mut ai.eco.tasks, b, 0);
        assert_eq!(ai.dispatch_energy_hand(), None, "staffed solar covers the two-energy deficit");
        assert_eq!(ai.eco_dispatch_job(10), Some(a));
        ai.eco.workers.get_mut(&11).unwrap().is_chicken = true;
        assert_eq!(ai.dispatch_energy_hand(), Some(10), "retreating crew cannot promise output");
        assert_eq!(ai.eco_dispatch_job(10), Some(b));
        ai.eco.workers.get_mut(&10).unwrap().set_task(&mut ai.eco.tasks, b, 0);
        ai.eco.hard_estalled = false;
        assert_eq!(ai.eco_dispatch_job(10), Some(b), "finish the emergency job after a transient recovery");
        ai.eco.workers.get_mut(&10).unwrap().end_task();
        assert_eq!(ai.eco_dispatch_job(10), Some(a));
    }

    #[test]
    fn project_commitment_does_not_disqualify_its_own_crew_or_hide_energy_output() {
        use crate::logistics::{Kind, Worth};
        let (mut ai, a, b) = setup();
        ai.set_feature_off("currency", false);
        ai.eco.workers.get_mut(&11).unwrap().set_task(&mut ai.eco.tasks, b, 0);
        ai.logistics_open(b, Kind::Energy, Worth::Flat(1000.0), "energy project", 0.0);
        let issue = ai.logistics.issues.get_mut(&b).unwrap();
        issue.crew = vec![11];
        issue.value = 1000.0;
        issue.share = 1000.0;
        assert!(ai.logistics_holds(11));
        assert!(ai.eco_project_hand(11, b), "a project's own hold is not a foreign controller");
        assert!(!ai.eco_project_hand(11, a), "other projects cannot take its crew");
        ai.eco.hard_estalled = true;
        ai.eco.static_income = 12.0;
        assert_eq!(ai.dispatch_energy_hand(), None, "committed solar output covers the shortage");
        ai.eco.terraform_hold.insert(11, 1000);
        assert!(!ai.eco_project_hand(11, b), "actual tactical ownership still excludes the worker");
        assert_eq!(ai.dispatch_energy_hand(), Some(10));
    }

    #[test]
    fn no_progress_releases_but_starvation_and_controller_absence_do_not() {
        let mut l = Lease { task: 1, seen: 0, gained: 0, distance: 1000.0, progress: 0.0 };
        for f in (0..1200).step_by(30) {
            assert!(l.observe(f, 1000.0, 0.0, false, true));
        }
        assert!(l.observe(1800, 1000.0, 0.0, false, false), "external controller absence restarts observation");
        for f in (1830..2400).step_by(30) {
            assert!(l.observe(f, 1000.0, 0.0, false, false));
        }
        assert!(!l.observe(2400, 1000.0, 0.0, false, false));
        assert!(l.observe(2430, 980.0, 0.0, false, false));
    }

    #[test]
    fn stalled_worker_releases_assignment_without_deleting_unfinished_frame() {
        let (mut ai, a, _) = setup();
        SPEND.set(0.0);
        assert_eq!(ai.eco_dispatch_job(10), Some(a));
        for f in (30..600).step_by(30) {
            ai.frame = f;
            assert_eq!(ai.eco_dispatch_job(10), Some(a));
        }
        ai.frame = 600;
        assert_ne!(ai.eco_dispatch_job(10), Some(a));
        assert!(ai.eco.tasks.get(a).is_some());
        assert!(!ai.eco.tasks.get(a).unwrap().assigned_workers.contains(&10));
    }
}
