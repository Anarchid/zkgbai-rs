//! Persistent expansion waypoints and energy partners. Job completion does
//! not erase an expander's direction; emergencies can still interrupt it.

use super::{Ai, TaskId, TaskKind, Worker, WorkerTask};
use crate::util::{NULLPOS, distance, get_directional_point};
use springai::{UnitDef, Vec3};

const HORIZON: f32 = 1400.0;
const MAX_ROUTE: f32 = 2600.0;
const GOAL_REACHED: f32 = 256.0;
const STALLED: i32 = 120 * 30;

#[derive(Clone, Copy, Debug)]
pub(super) struct ExpansionGoal {
    pub target: Vec3,
    best: f32,
    gained: i32,
}

impl ExpansionGoal {
    fn keep(&mut self, from: Vec3, reach: f32, frame: i32, useful: bool) -> bool {
        let remaining = distance(from, self.target);
        if remaining < self.best - 64.0 {
            self.best = remaining;
            self.gained = frame;
        }
        useful && remaining > reach.max(GOAL_REACHED) && frame - self.gained < STALLED
    }
}

fn direction_cost(from: Vec3, site: Vec3, goal: Vec3, speed: f32) -> f32 {
    // Prefer progress, but retain some travel cost even straight ahead.
    // A backward job pays for moving away from the next expansion waypoint.
    let progress = distance(site, goal) - distance(from, goal);
    (0.6 * progress).clamp(-speed * 8.0, speed * 16.0) / 4.0
}

fn heading_cost(from: Vec3, site: Vec3, enemy: Vec3) -> f32 {
    let a = distance(from, site);
    let b = distance(from, enemy);
    if a < 1.0 || b < 1.0 {
        return 0.0;
    }
    let cos = ((site.x - from.x) * (enemy.x - from.x) + (site.z - from.z) * (enemy.z - from.z)) / (a * b);
    (1.0 - cos.clamp(-1.0, 1.0)) * 700.0
}

fn route_anchor(from: Vec3, toward: Vec3) -> Vec3 {
    let d = distance(from, toward);
    if d <= 1.0 { from } else { get_directional_point(from, toward, d.min(320.0)) }
}

fn project_on_link(p: Vec3, a: Vec3, b: Vec3) -> Vec3 {
    let (dx, dz) = (b.x - a.x, b.z - a.z);
    let len2 = dx * dx + dz * dz;
    if len2 < 1.0 {
        return a;
    }
    let t = (((p.x - a.x) * dx + (p.z - a.z) * dz) / len2).clamp(0.0, 1.0);
    Vec3::xz(a.x + t * dx, a.z + t * dz)
}

fn partner_shortfall(greedy: usize, industrious: usize, pending: usize) -> bool {
    greedy > industrious + pending
}

impl Ai {
    pub(super) fn eco_directed_job(&self, w: &Worker, id: TaskId) -> bool {
        self.op_priority(id)
            || self.conquest_bonus(w.id, id).is_some()
            || self.logistics_bonus(w.id, id).is_some()
            || self.eco.tasks.get(id).and_then(|t| t.as_construction()).is_some_and(|c| c.fac_plop)
    }

    pub(super) fn eco_energy_job(&self, t: &WorkerTask) -> bool {
        match &t.kind {
            TaskKind::Construction(c) => {
                super::is_energy_plant(self.defs.name(c.build_type)) && c.target.is_none_or(|u| u.is_being_built())
            }
            TaskKind::Repair(r) => r.target.is_being_built() && super::is_energy_plant(self.defs.name_of(r.target)),
            _ => false,
        }
    }

    pub(super) fn eco_needs_industrious(&self) -> bool {
        if !self.eco_path_policy() {
            return false;
        }
        let greedy = self.eco.workers.values().filter(|w| w.is_greedy).count();
        let industrious = self.eco.workers.values().filter(|w| w.is_industrious).count();
        let pending = self.eco.new_workers.iter().filter(|id| !self.eco.workers.contains_key(id)).count();
        partner_shortfall(greedy, industrious, pending)
    }

    pub(super) fn eco_assign_worker_role(&self, w: &mut Worker) {
        let workers: Vec<_> = self.eco.workers.values().filter(|other| other.id != w.id).collect();
        let greedy = workers.iter().filter(|w| w.is_greedy).count();
        let industrious = workers.iter().filter(|w| w.is_industrious).count();
        if self.eco_path_policy() && partner_shortfall(greedy, industrious, 0) {
            w.is_industrious = true;
        } else {
            // Preserve the old expansion/generalist ratio among builders
            // other than the dedicated energy partners.
            let n = workers.iter().filter(|w| !w.is_industrious).count() + 1;
            let non_coms = n.saturating_sub(self.eco.commanders.len());
            w.is_greedy = n as i32 > 1 + self.merged_allies
                && (greedy < 2 || (greedy as f32) < non_coms as f32 * (1.0 - self.graph.territory_fraction).min(1.0));
        }
        self.op_emit(
            "workerrole",
            serde_json::json!({"worker":w.id,
            "role":if w.is_industrious {"industrious"} else if w.is_greedy {"expansion"} else {"general"}}),
        );
    }

    pub(super) fn eco_refresh_expansion_goal(&mut self, wid: i32) {
        if !self.eco_lanes() {
            if let Some(w) = self.eco.workers.get_mut(&wid) {
                w.expansion = None;
            }
            return;
        }
        let Some(w) = self.eco.workers.get(&wid) else { return };
        // No expansion trip while recovering energy or replacing the factory.
        if self.eco.hard_estalled || self.fac.factories.is_empty() {
            return;
        }
        if w.is_industrious
            || !(w.is_com || w.is_greedy)
            || w.is_chicken
            || self.eco_worker_held(wid)
            || self.recovery_owns(wid)
            || w.task.is_some_and(|id| self.eco_directed_job(w, id))
        {
            self.eco.workers.get_mut(&wid).unwrap().expansion = None;
            return;
        }
        let Some(def) = w.unit.def() else { return };
        let from = w.get_pos();
        let reach = def.build_distance();
        let view = self.war.view(Some(def));
        let safe = |p: Vec3| {
            self.war.get_porc_threat(p) <= 0.0
                && view.get_effective_threat(p) <= view.get_total_friendly_threat(p)
                && !w.shunned.iter().any(|&(s, until)| until > self.frame && distance(s, p) < 256.0)
                && !(w.is_com && self.comguard_avoids(wid, p))
                && self.terrain_reaches(def, from, p, reach + 32.0)
        };
        if let Some(mut goal) = w.expansion {
            let useful = safe(goal.target)
                && self.graph.get_neutral_spots().iter().any(|s| distance(s.get_pos(), goal.target) < 512.0);
            if goal.keep(from, reach, self.frame, useful) {
                self.eco.workers.get_mut(&wid).unwrap().expansion = Some(goal);
                return;
            }
        }
        let enemy = self.graph.get_enemy_base().unwrap_or_else(|| {
            let p = self.graph.get_enemy_center();
            if p == NULLPOS { self.graph.get_map_center() } else { p }
        });
        let lanes = self.eco_expansion_lanes(w);
        let target = self
            .eco
            .construction_tasks
            .iter()
            .filter_map(|&id| self.eco.tasks.get(id))
            .filter(|t| self.eco_lane_mex(w, t) && safe(t.get_pos()))
            .filter(|t| t.as_construction().is_some_and(|c| c.target.is_none()))
            .filter_map(|t| {
                let p = t.get_pos();
                let route = w.routes.as_ref()?.distance(p, reach)?;
                if route > MAX_ROUTE
                    || distance(from, p) <= reach.max(GOAL_REACHED)
                    || w.expansion.is_some_and(|g| self.frame - g.gained >= STALLED && distance(g.target, p) < 512.0)
                {
                    return None;
                }
                let heading = if w.is_com { heading_cost(from, p, enemy) } else { 0.0 };
                let occupied = self.eco_expansion_lane_cost(w, t, &lanes) * 3.0;
                let cost = route / 4.0 + (route - HORIZON).abs() / 2.0 + heading + occupied;
                Some((p, cost))
            })
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(p, _)| p);
        let old = w.expansion.map(|g| g.target);
        self.eco.workers.get_mut(&wid).unwrap().expansion =
            target.map(|target| ExpansionGoal { target, best: distance(from, target), gained: self.frame });
        if old != target {
            self.op_emit(
                "expansiongoal",
                serde_json::json!({"worker":wid,"from":[from.x,from.z],
                "goal":target.map(|p| [p.x,p.z]),"reason":"reached, unavailable, stalled, or first assignment"}),
            );
        }
    }

    pub(super) fn eco_expansion_goal_cost(&self, w: &Worker, t: &WorkerTask) -> f32 {
        if self.eco_unified() {
            return self.eco_unified_job_cost(w, t);
        }
        if !self.eco_lanes() {
            return 0.0;
        }
        let Some(goal) = w.expansion else { return 0.0 };
        // Emergency energy is selected locally before this auction. Do not
        // erase the direction of unrelated jobs when wind output fluctuates:
        // that sent commanders to rear mexes, then back when energy recovered.
        if self.eco_directed_job(w, t.id) {
            return 0.0;
        }
        let Some(def) = w.unit.def() else { return 0.0 };
        // Leave ongoing work under the worker's hands alone. This bias is
        // about where to walk for the next economic job.
        if distance(w.get_pos(), t.get_pos()) <= def.build_distance() {
            return 0.0;
        }
        direction_cost(w.get_pos(), t.get_pos(), goal.target, def.speed())
    }

    /// A waypoint owns the next trip while useful work along it exists.
    /// Allow local work and a little sideways placement slack; emergencies
    /// and explicit missions may still pull the worker away.
    pub(super) fn eco_expansion_backtracks(&self, w: &Worker, t: &WorkerTask) -> bool {
        if !self.eco_lanes() {
            return false;
        }
        let Some(goal) = w.expansion else { return false };
        let Some(def) = w.unit.def() else { return false };
        if self.eco_directed_job(w, t.id)
            || distance(w.get_pos(), t.get_pos()) <= def.build_distance()
            || (self.eco.hard_estalled && self.eco_energy_job(t))
            || t.as_construction().is_some_and(|c| {
                c.fac_plop
                    || self.eco_factory_defense_hand(w, t)
                    || (self.fac.factories.is_empty() && self.eco.build_ids.fac_ids.contains(&c.build_type.id()))
            })
        {
            return false;
        }
        distance(t.get_pos(), goal.target) > distance(w.get_pos(), goal.target) + 64.0
    }

    pub(super) fn eco_expansion_onward(&self, w: &Worker, t: &WorkerTask, cost: f32) -> bool {
        // Do not keep a worker in a lane whose only jobs are far away or
        // already overstaffed. Fall back to the ordinary auction instead.
        self.eco_lanes()
            && w.expansion.is_some()
            && cost < MAX_ROUTE / 4.0
            && self.eco_worker_route(w, t.get_pos()) <= MAX_ROUTE
            && !self.eco_expansion_backtracks(w, t)
    }

    pub(super) fn eco_careful_energy(&self, wid: i32) -> bool {
        !self.eco_path_policy()
            || self.eco.workers.get(&wid).is_some_and(|w| w.is_industrious)
            || self.eco_overdrive_phase()
    }

    /// Early expansion energy can land anywhere along a nearby link. Do
    /// not march to the connection head just to make the grid look complete.
    pub(super) fn eco_energy_lane_anchor(&self, wid: i32, from: Vec3) -> Vec3 {
        let Some(w) = self.eco.workers.get(&wid) else { return from };
        let goal = if self.eco_unified() { self.eco_unified_waypoint(wid) } else { w.expansion.map(|g| g.target) };
        let ahead = goal
            .and_then(|target| {
                let path = w.routes.as_ref()?.path(target, w.unit.def()?.build_distance())?;
                let p = path.into_iter().find(|&p| distance(from, p) >= 256.0).unwrap_or(target);
                Some(route_anchor(from, p))
            })
            .unwrap_or(from);
        self.graph
            .get_links()
            .iter()
            .filter(|l| self.graph.is_link_owned(l))
            .map(|l| {
                project_on_link(ahead, self.graph.metal_spots[l.v0].get_pos(), self.graph.metal_spots[l.v1].get_pos())
            })
            .filter(|&p| distance(from, p) <= 448.0)
            .min_by(|&a, &b| distance(a, ahead).total_cmp(&distance(b, ahead)))
            .unwrap_or(ahead)
    }

    pub(super) fn eco_small_energy_site(&self, wid: i32, def: UnitDef, from: Vec3) -> Vec3 {
        if self.eco_careful_energy(wid) {
            let anchor = self.graph.get_overdrive_sweet_spot(from, self.defs.name(def));
            return if anchor == NULLPOS { NULLPOS } else { self.eco_build_site(def, anchor, 0) };
        }
        let Some(w) = self.eco.workers.get(&wid) else { return NULLPOS };
        let Some(worker_def) = w.unit.def() else { return NULLPOS };
        let anchor = self.eco_energy_lane_anchor(wid, from);
        let view = self.war.view(Some(worker_def));
        // Try the nearby link first, then local ground if its footprint is
        // blocked. Both choices have bounded travel and normal safety checks.
        for anchor in [anchor, from] {
            let site = self.cb.map().find_closest_build_site(def, anchor, 128.0, 3, 0);
            if site.x < 0.0
                || distance(from, site) > 576.0
                || !self.eco_build_check(def, site, 0)
                || !self.terrain_reaches(worker_def, from, site, worker_def.build_distance())
                || view.get_effective_threat(site) > view.get_total_friendly_threat(site)
                || self.war.get_porc_threat(site) > 0.0
            {
                continue;
            }
            return site;
        }
        NULLPOS
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn greedy_workers_get_energy_partners_and_losses_request_replacements() {
        use springai::Unit;
        unsafe extern "C" fn scalar(_: i32, _: i32) -> f32 {
            100.0
        }
        unsafe extern "C" fn zero(_: i32, _: i32) -> i32 {
            0
        }
        let mut ai = super::super::commander::tests::test_ai_with(|t| {
            t.Unit_getWeapons = Some(zero);
            t.UnitDef_getWeaponMounts = Some(zero);
            t.UnitDef_getBuildSpeed = Some(scalar);
        });
        ai.set_feature_off("lanes", false);
        let mut commander = Worker::new(Unit::from_id(ai.cb, 1).unwrap(), ai.team_id);
        commander.is_com = true;
        ai.eco.commanders.push(1);
        ai.eco.workers.insert(1, commander);
        for (id, greedy, industrious) in [(10, true, false), (11, false, true), (12, true, false), (13, false, true)] {
            let mut w = Worker::new(Unit::from_id(ai.cb, id).unwrap(), ai.team_id);
            ai.eco_assign_worker_role(&mut w);
            assert_eq!((w.is_greedy, w.is_industrious), (greedy, industrious));
            ai.eco.workers.insert(id, w);
            assert_eq!(ai.eco_needs_industrious(), greedy);
        }
        ai.eco.workers.remove(&11);
        assert!(ai.eco_needs_industrious());
        ai.eco.new_workers.insert(14);
        assert!(!ai.eco_needs_industrious(), "a constructor being built already covers the loss");
        let mut replacement = Worker::new(Unit::from_id(ai.cb, 14).unwrap(), ai.team_id);
        ai.eco_assign_worker_role(&mut replacement);
        assert!(replacement.is_industrious);
        assert!(!replacement.is_greedy);
    }

    #[test]
    fn early_energy_uses_the_nearby_part_of_a_link_instead_of_its_far_head() {
        let a = Vec3::xz(0.0, 0.0);
        let b = Vec3::xz(1800.0, 0.0);
        assert_eq!(project_on_link(Vec3::xz(300.0, 100.0), a, b), Vec3::xz(300.0, 0.0));
        assert_eq!(project_on_link(Vec3::xz(2200.0, 100.0), a, b), b);
        assert_eq!(project_on_link(Vec3::xz(300.0, 100.0), a, a), a);
        assert_eq!(route_anchor(a, a), a, "a reached waypoint must not produce a NaN site");
        assert_eq!(route_anchor(a, b), Vec3::xz(320.0, 0.0));
    }

    #[test]
    fn vector_survives_intermediate_jobs_but_releases_at_goal_or_danger() {
        let mut goal = ExpansionGoal { target: Vec3::xz(1400.0, 0.0), best: 1400.0, gained: 0 };
        for frame in [30, 300, 900] {
            assert!(goal.keep(Vec3::xz(400.0, 0.0), 128.0, frame, true));
            assert_eq!(goal.target, Vec3::xz(1400.0, 0.0));
        }
        assert!(!goal.keep(Vec3::xz(1200.0, 0.0), 128.0, 1000, true));
        assert!(!goal.keep(Vec3::xz(400.0, 0.0), 128.0, 1000, false));
        assert!(!goal.keep(Vec3::xz(400.0, 0.0), 128.0, 5000, true));
    }

    #[test]
    fn forward_energy_beats_equally_distant_backward_energy_without_free_travel() {
        let from = Vec3::xz(1000.0, 1000.0);
        let goal = Vec3::xz(2400.0, 1000.0);
        let ahead = Vec3::xz(1300.0, 1000.0);
        let back = Vec3::xz(700.0, 1000.0);
        let cost = |p| distance(from, p) / 4.0 + direction_cost(from, p, goal, 60.0);
        assert!(cost(ahead) > 0.0);
        assert!(cost(ahead) < cost(back));
        assert!(cost(Vec3::xz(2200.0, 1000.0)) > cost(ahead));
    }

    #[test]
    fn energy_shortfall_does_not_reverse_expansion_job_preferences() {
        use springai::{Unit, UnitDef};
        unsafe extern "C" fn scalar(_: i32, _: i32) -> f32 {
            60.0
        }
        unsafe extern "C" fn zero(_: i32, _: i32) -> i32 {
            0
        }
        let mut ai = super::super::commander::tests::test_ai_with(|t| {
            t.Unit_getWeapons = Some(zero);
            t.UnitDef_getWeaponMounts = Some(zero);
            t.UnitDef_getBuildSpeed = Some(scalar);
            t.UnitDef_getSpeed = Some(scalar);
            t.UnitDef_getBuildDistance = Some(scalar);
        });
        ai.set_feature_off("unifiedconquest", true);
        ai.set_feature_off("lanes", false);
        let mut w = Worker::new(Unit::from_id(ai.cb, 10).unwrap(), ai.team_id);
        w.is_com = true;
        w.expansion = Some(ExpansionGoal { target: Vec3::xz(2400.0, 0.0), best: 1400.0, gained: 0 });
        let mex = UnitDef::from_id(ai.cb, 2).unwrap();
        let ahead = WorkerTask::construction(mex, Vec3::xz(1300.0, 0.0), 0);
        let behind = WorkerTask::construction(mex, Vec3::xz(700.0, 0.0), 0);
        let local = WorkerTask::construction(mex, Vec3::xz(980.0, 0.0), 0);
        assert!(!ai.eco_expansion_onward(&w, &behind, -1000.0), "a discounted rear job still leaves the lane");
        assert!(ai.eco_expansion_onward(&w, &ahead, 100.0));
        assert!(ai.eco_expansion_onward(&w, &local, 100.0));
        assert!(!ai.eco_expansion_onward(&w, &ahead, 1000.0), "overstaffed work is not a reason to stay");
        let far = WorkerTask::construction(mex, Vec3::xz(3700.0, 0.0), 0);
        assert!(!ai.eco_expansion_onward(&w, &far, -1000.0), "a cheap distant job is not a useful local alternative");
        let normal = ai.eco_expansion_goal_cost(&w, &behind);
        assert!(normal > 0.0);
        ai.eco.hard_estalled = true;
        assert_eq!(ai.eco_expansion_goal_cost(&w, &behind), normal);
        assert!(ai.eco_expansion_backtracks(&w, &behind));
        let mut emergency = WorkerTask::construction(mex, Vec3::xz(700.0, 0.0), 0);
        emergency.as_construction_mut().unwrap().fac_plop = true;
        assert!(!ai.eco_expansion_backtracks(&w, &emergency));
        assert!(ai.eco_expansion_goal_cost(&w, &ahead) < 0.0);
        assert_eq!(ai.eco_expansion_goal_cost(&w, &local), 0.0, "work in reach needs no detour");
    }

    #[test]
    fn commander_faces_enemy_and_partners_count_pending_constructors() {
        let from = Vec3::xz(1000.0, 1000.0);
        let enemy = Vec3::xz(5000.0, 1000.0);
        assert_eq!(heading_cost(from, Vec3::xz(2000.0, 1000.0), enemy), 0.0);
        assert!(heading_cost(from, Vec3::xz(0.0, 1000.0), enemy) > 1000.0);
        assert!(partner_shortfall(1, 0, 0));
        assert!(!partner_shortfall(1, 0, 1));
        assert!(!partner_shortfall(2, 2, 0));
        assert!(partner_shortfall(2, 1, 0));
    }
}
