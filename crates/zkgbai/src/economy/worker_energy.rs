//! Choose energy builders by the expansion they would give up, and build
//! locally when energy is critically short.

use super::List;
use super::{Ai, TaskId, Worker, WorkerTask};
use crate::util::{NULLPOS, distance};
use springai::Vec3;

/// Beyond half a minute, an expansion opportunity is no longer immediate.
const EXPANSION_SECONDS: f32 = 30.0;

fn build_reach(w: &Worker) -> f32 {
    w.unit.def().map_or(0.0, |d| d.build_distance())
}

fn energy_bid(energy_seconds: f32, mex_seconds: f32) -> f32 {
    energy_seconds - mex_seconds.min(EXPANSION_SECONDS)
}

impl Ai {
    pub(super) fn eco_worker_route(&self, w: &Worker, target: Vec3) -> f32 {
        let from = w.get_pos();
        if !self.eco_path_policy() {
            return distance(from, target);
        }
        w.routes
            .as_ref()
            .filter(|r| self.frame - r.frame <= 30 && distance(r.origin, from) < 64.0)
            .and_then(|r| r.distance(target, build_reach(w)))
            .unwrap_or_else(|| distance(from, target))
    }

    /// Available builders and their nearest safe, unclaimed mex, in seconds.
    /// Existing reservations count: an opening already taken by another
    /// builder is not a reason to keep this one away from energy work.
    pub(super) fn eco_energy_bidders(&self) -> Vec<(i32, f32)> {
        if !self.eco_path_policy()
            || self.eco.hard_estalled
            || (self.eco.solar_tasks.is_empty() && self.eco.wind_tasks.is_empty())
        {
            return Vec::new();
        }
        let spots = self.graph.get_neutral_spots();
        self.eco
            .workers
            .values()
            .filter(|w| {
                !(w.is_chicken
                    || w.assigned_plop && w.has_plop()
                    || self.eco_worker_held(w.id)
                    || springai::reserved::is_reserved(self.cb, w.id))
                    && w.unit.health() > 0.0
                    && w.unit.team() == self.team_id
                    && !w.task.is_some_and(|id| self.op_priority(id) || self.conquest_bonus(w.id, id).is_some())
            })
            .map(|w| {
                let def = w.unit.def();
                let speed = def.map_or(1.0, |d| d.speed()).max(1.0);
                let view = self.war.view(def);
                let mex = spots
                    .iter()
                    .map(|s| s.get_pos())
                    .filter(|&p| {
                        view.get_effective_threat(p) <= view.get_total_friendly_threat(p)
                            && self.war.get_porc_threat(p) <= 0.0
                            && def.is_none_or(|d| self.terrain_reaches(d, w.get_pos(), p, build_reach(w) + 32.0))
                            && !self.eco.construction_tasks.iter().filter_map(|&id| self.eco.tasks.get(id)).any(|t| {
                                t.as_construction().is_some_and(|c| c.build_type.id() == self.eco.build_ids.mex_id)
                                    && distance(t.get_pos(), p) < 64.0
                                    && t.assigned_workers.iter().any(|&id| id != w.id)
                            })
                    })
                    .map(|p| (self.eco_worker_route(w, p) - build_reach(w)).max(0.0) / speed)
                    .fold(f32::INFINITY, f32::min);
                (w.id, mex)
            })
            .collect()
    }

    /// Lost expansion opportunity compared with the best available builder
    /// for this particular energy site. Distance to energy still matters:
    /// a rear builder across the map must not reserve everyone else's jobs.
    pub(super) fn eco_energy_opportunity(&self, worker: &Worker, task: &WorkerTask, bidders: &[(i32, f32)]) -> f32 {
        if !self.eco_path_policy()
            || self.eco.hard_estalled
            || worker.is_industrious
            || !task.as_construction().is_some_and(|c| {
                c.build_type.id() == self.eco.build_ids.solar_id || c.build_type.id() == self.eco.build_ids.wind_id
            })
        {
            return 0.0;
        }
        let Some(&(_, mex)) = bidders.iter().find(|b| b.0 == worker.id) else { return 0.0 };
        // Keep finishing energy already under this builder's hands.
        if worker.task == Some(task.id) && distance(worker.get_pos(), task.get_pos()) <= build_reach(worker) {
            return 0.0;
        }
        let bid = |w: &Worker, mex| {
            let speed = w.unit.def().map_or(1.0, |d| d.speed()).max(1.0);
            energy_bid((self.eco_worker_route(w, task.get_pos()) - build_reach(w)).max(0.0) / speed, mex)
        };
        let own = bid(worker, mex);
        let best = bidders
            .iter()
            .filter_map(|&(id, mex)| {
                let w = self.eco.workers.get(&id)?;
                if w.unit
                    .def()
                    .is_some_and(|d| !self.terrain_reaches(d, w.get_pos(), task.get_pos(), build_reach(w) + 32.0))
                {
                    return None;
                }
                Some(bid(w, mex))
            })
            .fold(own, f32::min);
        (own - best).max(0.0) * worker.unit.def().map_or(1.0, |d| d.speed()).max(1.0) / 4.0
    }

    /// No network detour during a hard stall: finish nearby energy or place
    /// a small generator inside the builder's current range. The normal
    /// build checks still protect factory exits and reserved footprints.
    pub(super) fn eco_local_energy_task(&mut self, wid: i32) -> Option<TaskId> {
        if !self.eco_path_policy() || !self.eco.hard_estalled {
            return None;
        }
        let w = self.eco.workers.get(&wid)?;
        // assigned_plop records the opening assignment even after the free
        // factory is finished. Only an unspent plop excludes local energy.
        if w.is_chicken || (w.assigned_plop && w.has_plop()) || w.unit.is_being_built() {
            return None;
        }
        let from = w.get_pos();
        let range = build_reach(w);
        if from == NULLPOS || range <= 0.0 {
            return None;
        }
        let energy = |id: TaskId| {
            self.eco.tasks.get(id).filter(|t| {
                distance(from, t.get_pos()) <= range * 0.95
                    && t.as_construction().is_some_and(|c| {
                        (c.build_type.id() == self.eco.build_ids.solar_id
                            || c.build_type.id() == self.eco.build_ids.wind_id)
                            && c.target
                                .is_none_or(|u| u.is_being_built() && u.health() > 0.0 && u.team() == self.team_id)
                    })
            })
        };
        if let Some(id) = w.task.filter(|&id| energy(id).is_some()) {
            return Some(id);
        }
        if let Some(id) = self
            .eco
            .solar_tasks
            .iter()
            .chain(&self.eco.wind_tasks)
            .copied()
            .filter(|&id| energy(id).is_some())
            .min_by(|&a, &b| distance(from, self.eco.task_pos(a)).total_cmp(&distance(from, self.eco.task_pos(b))))
        {
            return Some(id);
        }
        let names =
            if self.eco_wind_beats_solar(from) { ["energywind", "energysolar"] } else { ["energysolar", "energywind"] };
        for name in names {
            let Some(def) = self.defs.get(name) else { continue };
            for i in 0..9 {
                let angle = i as f32 * std::f32::consts::TAU / 8.0;
                let anchor = if i == 0 {
                    from
                } else {
                    Vec3::xz(from.x + range * 0.55 * angle.cos(), from.z + range * 0.55 * angle.sin())
                };
                let site = self.cb.map().find_closest_build_site(def, anchor, range * 0.45, 0, 0);
                if site.x < 0.0 || distance(from, site) > range * 0.95 {
                    continue;
                }
                let list = if name == "energywind" { List::Wind } else { List::Solar };
                if let Some(id) =
                    self.eco_add_construction(WorkerTask::construction(def, site, 0), Some(list), Some(list))
                {
                    self.op_emit(
                        "localenergy",
                        serde_json::json!({"worker":wid,"task":id,"def":name,
                        "from":[from.x,from.z],"site":[site.x,site.z],"distance":distance(from,site),"range":range}),
                    );
                    return Some(id);
                }
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hard_stall_uses_energy_in_actual_build_range_without_a_grid_walk() {
        use springai::{Unit, UnitDef};
        thread_local! {
            static PLOP: std::cell::Cell<f32> = const { std::cell::Cell::new(1.0) };
        }
        unsafe extern "C" fn rules(_: i32, _: i32, name: *const std::ffi::c_char, default: f32) -> f32 {
            if unsafe { std::ffi::CStr::from_ptr(name) } == c"facplop" { PLOP.get() } else { default }
        }
        unsafe extern "C" fn pos(_: i32, _: i32, p: *mut f32) {
            unsafe {
                std::ptr::copy_nonoverlapping([100.0, 0.0, 100.0].as_ptr(), p, 3);
            }
        }
        unsafe extern "C" fn reach(_: i32, _: i32) -> f32 {
            128.0
        }
        unsafe extern "C" fn scalar(_: i32, _: i32) -> f32 {
            100.0
        }
        unsafe extern "C" fn zero(_: i32, _: i32) -> i32 {
            0
        }
        unsafe extern "C" fn no(_: i32, _: i32) -> bool {
            false
        }
        unsafe extern "C" fn params(
            _: i32,
            _: i32,
            _: *mut *const std::ffi::c_char,
            _: *mut *const std::ffi::c_char,
        ) -> i32 {
            0
        }
        let mut ai = super::super::commander::tests::test_ai_with(|t| {
            t.Unit_getPos = Some(pos);
            t.Unit_getRulesParamFloat = Some(rules);
            t.UnitDef_getBuildDistance = Some(reach);
            t.Unit_getWeapons = Some(zero);
            t.UnitDef_getWeaponMounts = Some(zero);
            t.UnitDef_getBuildSpeed = Some(scalar);
            t.UnitDef_getCustomParams = Some(params);
            t.UnitDef_isAbleToAttack = Some(no);
            t.Unit_isBeingBuilt = Some(no);
        });
        let mut w = Worker::new(Unit::from_id(ai.cb, 10).unwrap(), ai.team_id);
        w.is_greedy = true;
        ai.eco.workers.insert(10, w);
        ai.eco.build_ids.solar_id = 2;
        let solar = UnitDef::from_id(ai.cb, 2).unwrap();
        let far = ai.eco.tasks.insert(WorkerTask::construction(solar, Vec3::xz(1100.0, 100.0), 0));
        let near = ai.eco.tasks.insert(WorkerTask::construction(solar, Vec3::xz(180.0, 100.0), 0));
        ai.eco.solar_tasks.extend([far, near]);
        assert_eq!(ai.eco_local_energy_task(10), None);
        ai.eco.hard_estalled = true;
        assert_eq!(ai.eco_local_energy_task(10), Some(near));
        ai.eco.solar_tasks.retain(|&id| id != near);
        assert_eq!(ai.eco_local_energy_task(10), None, "no distant fallback to the planned grid");
        ai.eco.solar_tasks.push(near);
        ai.eco.workers.get_mut(&10).unwrap().assigned_plop = true;
        assert_eq!(ai.eco_local_energy_task(10), None, "do not interrupt the free initial factory plop");
        PLOP.set(0.0);
        assert_eq!(
            ai.eco_local_energy_task(10),
            Some(near),
            "a spent plop must not send the commander away for energy"
        );
    }

    #[test]
    fn energy_goes_to_the_builder_with_less_to_lose_in_expansion() {
        // Both are ten seconds from the energy site, but the front builder
        // has a mex two seconds away; the rear builder's next mex is far.
        assert!(energy_bid(10.0, 30.0) < energy_bid(10.0, 2.0));
        // A constructor on the other side of the map is not a substitute.
        assert!(energy_bid(90.0, f32::INFINITY) > energy_bid(10.0, 2.0));
        // No expansion opportunities: choose whoever can arrive first.
        assert!(energy_bid(2.0, f32::INFINITY) < energy_bid(10.0, f32::INFINITY));
    }
}
