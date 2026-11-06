//! Durable special-project execution. Strategic choice lives in `chose_sup_wep`
//! and `s_wep`; TaskStore, pending power and logistics own the actual work.
//! This service retries placement independently of worker job requests and
//! reports the boundary at which an accepted strategic choice is waiting.
use super::TaskId;
use crate::{
    Ai,
    logistics::{Kind, PAYBACK, Worth},
    util::distance,
};
use serde_json::{Value, json};
use springai::{UnitDef, Vec3};

const RETRY: i32 = 30 * 10;

#[derive(Default, Debug, PartialEq)]
pub(super) struct Status {
    next_attempt: i32,
    what: String,
    phase: String,
    reason: String,
    task: Option<TaskId>,
    since: i32,
}

/// Include the rear even when the expanding economy's centroid moves forward.
/// No RNG: a failed candidate does not change every unrelated policy's stream.
fn seeds(home: Vec3, base: Vec3, center: Vec3) -> Vec<Vec3> {
    let mut out = vec![base, home, Vec3::xz(base.x * 0.7 + center.x * 0.3, base.z * 0.7 + center.z * 0.3)];
    for radius in [400.0, 800.0, 1200.0, 1800.0, 2600.0] {
        for direction in 0..16 {
            let angle = std::f32::consts::TAU * direction as f32 / 16.0;
            out.push(Vec3::xz(home.x + radius * angle.cos(), home.z + radius * angle.sin()));
        }
    }
    out.retain(|p| p.x.is_finite() && p.z.is_finite() && p.x >= 0.0 && p.z >= 0.0);
    out
}

impl Ai {
    pub(crate) fn eco_special_json(&self) -> Value {
        let s = &self.eco.special;
        json!({"what":s.what,"phase":s.phase,"reason":s.reason,"task":s.task,
            "since":s.since,"next_attempt":s.next_attempt})
    }

    pub(super) fn eco_special_report(&mut self, what: &str, phase: &str, reason: &str, task: Option<TaskId>) {
        let s = &mut self.eco.special;
        if s.what == what && s.phase == phase && s.reason == reason && s.task == task {
            return;
        }
        s.what = what.to_owned();
        s.phase = phase.to_owned();
        s.reason = reason.to_owned();
        s.task = task;
        s.since = self.frame;
        self.op_emit("special", self.eco_special_json());
        self.log(&format!("special: {what} {phase}: {reason} (task {task:?})"));
    }

    pub(super) fn eco_track_special(&mut self, id: TaskId) {
        if !self.eco.super_wep_tasks.contains(&id) {
            return;
        }
        let Some(c) = self.eco.tasks.get(id).and_then(|t| t.as_construction()) else { return };
        if self.logistics.issues.get(&id).is_some_and(|i| i.kind == Kind::Operator) {
            return;
        }
        let value = 2.0 * c.build_type.cost(self.metal);
        // This is the current value of the retained strategic request. Refresh
        // its forecast rather than letting an old valuation expire unnoticed.
        // Deadlines, grid restoration and energy emergencies still precede it.
        self.logistics_open(
            id,
            Kind::Special,
            Worth::Stream { value, from: self.frame as f32 / 30.0, rate: value / PAYBACK },
            "committed special project",
            0.0,
        );
    }

    fn eco_special_active(&mut self) -> bool {
        let tasks: Vec<_> =
            self.eco.super_wep_tasks.iter().copied().filter(|&id| self.eco.tasks.get(id).is_some()).collect();
        for &id in &tasks {
            self.eco_track_special(id);
        }
        let Some(&id) = tasks.first() else { return false };
        let c = self.eco.tasks.get(id).unwrap().as_construction().unwrap();
        let what = c.build_type.name();
        let started = c.target.is_some();
        let (phase, reason) = self.logistics.issues.get(&id).map_or(("accepted", "ordinary_assignment"), |i| {
            if i.state == "executing" && started { ("building", "crew") } else { (i.state, i.reason) }
        });
        self.eco_special_report(&what, phase, reason, Some(id));
        true
    }

    pub(super) fn eco_special_update(&mut self) {
        // Existing commitments remain authoritative if selection changes.
        if self.eco_special_active() {
            return;
        }
        if let Some((def, _)) = self.eco_awaiting() {
            self.eco_special_report(&def.name(), "waiting", "power_connection", None);
            return;
        }
        if !self.eco.chose_sup_wep {
            return;
        }
        if self.frame < self.eco.special.next_attempt {
            return;
        }
        self.eco.special.next_attempt = self.frame + RETRY;
        if self.eco.s_wep == 0 || self.eco.has_super_wep {
            self.eco_create_bertha_task();
        } else {
            self.eco_create_super_wep_task();
        }
        // Includes a task adopted from an ally, as well as new construction.
        self.eco_special_active();
    }

    fn eco_special_travel(&self, build: UnitDef, site: Vec3) -> Option<f32> {
        self.eco
            .workers
            .values()
            .filter_map(|w| {
                let def = w.unit.def()?;
                if w.is_chicken
                    || w.unit.health() <= 0.0
                    || w.unit.is_being_built()
                    || !def.build_options().iter().any(|d| d.id() == build.id())
                    || !self.terrain_reaches(def, w.get_pos(), site, def.build_distance() + 32.0)
                {
                    return None;
                }
                let view = self.war.view(Some(def));
                if view.get_effective_threat(site) / self.op_greed() > view.get_total_friendly_threat(site) {
                    return None;
                }
                let travel = (self.eco_worker_route(w, site) - def.build_distance()).max(0.0) / def.speed().max(1.0);
                travel.is_finite().then_some(travel)
            })
            .min_by(f32::total_cmp)
    }

    pub(super) fn eco_special_site(&self, def: UnitDef) -> Option<Vec3> {
        let home = self.graph.get_start_pos();
        let map = self.cb.map();
        let mut candidates = seeds(home, self.graph.get_ally_base(), self.graph.get_ally_center());
        candidates.extend(self.fac.factories.values().map(|f| f.get_pos()));
        let mut seen = std::collections::BTreeSet::new();
        let mut best: Option<(f32, Vec3)> = None;
        for seed in candidates {
            if seed.x >= map.width() as f32 * 8.0 || seed.z >= map.height() as f32 * 8.0 {
                continue;
            }
            // A smaller search radius keeps the rear survey from repeatedly
            // returning the same distant build site.
            let site = map.find_closest_build_site(def, seed, 192.0, 3, 0);
            if !site.x.is_finite()
                || !site.z.is_finite()
                || site.z < 0.0
                || !seen.insert((site.x as i32, site.z as i32))
                || self.eco_bad_super_wep_site(site)
                || !self.eco_build_check(def, site, 0)
                || self.eco_big_gun_near(site, 350.0)
            {
                continue;
            }
            let Some(travel) = self.eco_special_travel(def, site) else { continue };
            let score = travel + distance(home, site) / 100.0;
            if best.is_none_or(|(old, _)| score < old) {
                best = Some((score, site));
            }
        }
        best.map(|(_, site)| site)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn project_ai() -> (Ai, TaskId) {
        use super::super::{Worker, WorkerTask};
        use springai::Unit;
        use std::ffi::c_char;
        unsafe extern "C" fn scalar(_: i32, _: i32) -> f32 {
            100.0
        }
        unsafe extern "C" fn zero(_: i32, _: i32) -> i32 {
            0
        }
        unsafe extern "C" fn team(_: i32, _: i32) -> i32 {
            2
        }
        unsafe extern "C" fn cost(_: i32, id: i32, _: i32) -> f32 {
            if id == 2 { 10000.0 } else { 100.0 }
        }
        unsafe extern "C" fn build_time(_: i32, id: i32) -> f32 {
            if id == 2 { 10000.0 } else { 100.0 }
        }
        unsafe extern "C" fn no(_: i32, _: i32) -> bool {
            false
        }
        unsafe extern "C" fn name(_: i32, id: i32) -> *const c_char {
            if id == 2 { c"staticnuke".as_ptr() } else { c"cloakcon".as_ptr() }
        }
        unsafe extern "C" fn params(_: i32, _: i32, _: *mut *const c_char, _: *mut *const c_char) -> i32 {
            0
        }
        unsafe extern "C" fn rules(_: i32, _: i32, _: *const c_char, default: f32) -> f32 {
            default
        }
        unsafe extern "C" fn possible(_: i32, _: i32, _: *mut f32, _: i32) -> bool {
            true
        }
        unsafe extern "C" fn size(_: i32, _: i32) -> i32 {
            4
        }
        unsafe extern "C" fn elevation(_: i32, _: f32, _: f32) -> f32 {
            100.0
        }
        let mut ai = super::super::test_ai_with(|t| {
            t.Unit_getTeam = Some(team);
            t.Unit_getHealth = Some(scalar);
            t.Unit_getWeapons = Some(zero);
            t.Unit_getCurrentCommands = Some(zero);
            t.Unit_getRulesParamFloat = Some(rules);
            t.UnitDef_getWeaponMounts = Some(zero);
            t.UnitDef_getBuildSpeed = Some(scalar);
            t.UnitDef_getBuildTime = Some(build_time);
            t.UnitDef_getName = Some(name);
            t.UnitDef_getCost = Some(cost);
            t.UnitDef_getSpeed = Some(scalar);
            t.UnitDef_getCustomParams = Some(params);
            t.UnitDef_isAbleToAttack = Some(no);
            t.UnitDef_isMoveDataAvailable = Some(no);
            t.UnitDef_isAbleToFly = Some(no);
            t.UnitDef_getMaxWaterDepth = Some(scalar);
            t.Map_isPossibleToBuildAt = Some(possible);
            t.UnitDef_getXSize = Some(size);
            t.UnitDef_getZSize = Some(size);
            t.Map_getElevationAt = Some(elevation);
        });
        ai.frame = 1817;
        ai.offset = 0;
        ai.eco.metal = 10000.0;
        ai.eco.energy = 10000.0;
        ai.eco.effective_income = 200.0;
        let w = Worker::new(Unit::from_id(ai.cb, 10).unwrap(), ai.team_id);
        ai.eco.workers.insert(10, w);
        let id = ai.eco.tasks.insert(WorkerTask::construction(
            UnitDef::from_id(ai.cb, 2).unwrap(),
            Vec3::xz(1100.0, 100.0),
            0,
        ));
        ai.eco.construction_tasks.push(id);
        ai.eco.super_wep_tasks.push(id);
        (ai, id)
    }

    #[test]
    fn aged_unstarted_project_survives_cleanup_and_gets_a_real_crew() {
        let (mut ai, id) = project_ai();
        ai.eco_clean_orders(); // 60 seconds old: the former 30-second expiry killed it here.
        assert!(ai.eco.tasks.get(id).is_some());
        ai.eco_special_update();
        assert_eq!(ai.logistics.issues[&id].kind, Kind::Special);
        ai.logistics_update(ai.frame);
        assert_eq!(ai.logistics.issues[&id].crew, vec![10]);
        ai.eco_assign_worker(10);
        assert_eq!(ai.eco.workers[&10].task, Some(id));
        // Energy emergencies pause staffing, without discarding the project.
        ai.eco.hard_estalled = true;
        ai.frame += 30;
        ai.logistics_update(ai.frame);
        assert!(ai.logistics.issues[&id].crew.is_empty());
        assert_eq!(ai.logistics.issues[&id].reason, "energy");
        assert!(ai.eco.tasks.get(id).is_some());
        ai.eco.hard_estalled = false;
        ai.frame += 30;
        ai.eco_special_update();
        ai.logistics_update(ai.frame);
        assert_eq!(ai.logistics.issues[&id].crew, vec![10]);
    }

    #[test]
    fn allied_retarget_keeps_project_identity_but_refreshes_crew_geometry_and_cost() {
        let (mut ai, id) = project_ai();
        ai.eco_special_update();
        ai.logistics_update(ai.frame);
        let issue = ai.logistics.issues[&id].id;
        let task = ai.eco.tasks.get_mut(id).unwrap();
        task.position = Vec3::xz(1600.0, 400.0);
        task.as_construction_mut().unwrap().build_type = UnitDef::from_id(ai.cb, 1).unwrap();
        // This is the in-place mutation performed when a queued project adopts an allied frame.
        ai.eco_special_update();
        let changed = &ai.logistics.issues[&id];
        assert_eq!(changed.id, issue);
        assert_eq!(changed.pos, Vec3::xz(1600.0, 400.0));
        assert_eq!(changed.cost, 100.0);
        assert_eq!(changed.build_time, 100.0);
        assert!(changed.crew.is_empty(), "old travel evidence must not follow a moved frame");
        assert_eq!(ai.eco.super_wep_tasks, vec![id]);
    }

    #[test]
    fn survey_keeps_rear_candidates_when_centers_coincide_or_advance() {
        let home = Vec3::xz(1000.0, 1000.0);
        let same = seeds(home, home, home);
        assert!(same.iter().all(|p| p.x.is_finite() && p.z.is_finite()));
        let forward = seeds(home, home, Vec3::xz(5000.0, 5000.0));
        assert!(forward.iter().any(|p| p.x < home.x && p.z < home.z));
        assert!(forward.contains(&home));
        assert!(forward.len() > 30);
    }
}
