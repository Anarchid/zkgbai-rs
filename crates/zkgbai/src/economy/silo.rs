//! A tactical silo's final footprint is also its firing position. Reserve
//! it while preparing a threatened site; only completed defenses unlock it.
//!
//! A nanoframe already begun is never given up for want of room for its
//! guards: it waits for those that fit, and from [`FINISH_FIRST`] built it
//! is finished before them (2026-11-04, Adamantine Mountain 3v3: a silo
//! stood 99.5% built for 13 minutes beside a nuke being built, its job
//! dropped and re-adopted every poll because the full layout never fit).

use springai::{Callback, NO_TIMEOUT, Unit, UnitDef, Vec3};
use zk_terrain::occupancy::Occupancy;

use super::tasks::{BuildPrerequisite, ConstructionTask};
use super::{EconomyManager, List, SITE_SLACK, TaskId, TaskKind, WorkerTask};
use crate::Ai;
use crate::defs::cmd;
use crate::military::missiles;
use crate::terrain::{HELD_POWER, pos};
use crate::util::{NULLPOS, distance, get_directional_point};

const SILO_REPLAN: i32 = 30 * 120;
const GUARD_AHEAD: f32 = 220.0;
/// The side Lotuses stand this far from the Stinger between them: clear
/// of its 130-elmo death blast, which `porcmix` checks in `eco_build_check`
/// (at 110 it refused them both).
const GUARD_SPREAD: f32 = 140.0;
const GUARD_SEARCH: f32 = 96.0;
const CARETAKERS: usize = 2;
/// A nanoframe this far built is finished before its guards unless the
/// site is under fire: the rest of it costs less than any one guard, and
/// the guards stay queued (`eco_fortify_silo` reuses them).
const FINISH_FIRST: f32 = 0.8;
/// Look beyond the site's current weapon-range threat: forces in the
/// same region or across nearby entrances can arrive during construction.
const NEAR_REGION: f32 = 1200.0;
/// Danger per million square elmos, with small regions capped at their
/// actual strength so a lone picket in a tiny region cannot trigger it.
const DENSITY_AREA: f32 = 1_000_000.0;
const PREPARE_POWER: f32 = HELD_POWER;

fn region_density(power: f32, cells: usize) -> f32 {
    power * DENSITY_AREA / (cells as f32 * zk_terrain::CELL.powi(2)).max(DENSITY_AREA)
}

/// Completion is evidence about live buildings, independent of whether
/// their old construction jobs were completed, canceled, or deleted.
fn prerequisite_unit(cb: Callback, team: i32, required: &BuildPrerequisite) -> Option<Unit> {
    cb.friendly_units_in_cylinder(required.pos, SITE_SLACK).into_iter().find(|u| {
        u.health() > 0.0
            && distance(u.pos(), required.pos) < SITE_SLACK
            && u.def().is_some_and(|d| d.id() == required.def.id())
            && (!u.is_being_built() || u.team() == team)
    })
}

fn prerequisites_ready(cb: Callback, team: i32, requirements: &[BuildPrerequisite], threatened: bool) -> bool {
    (!threatened || !requirements.is_empty())
        && requirements.iter().all(|p| prerequisite_unit(cb, team, p).is_some_and(|u| !u.is_being_built()))
}

/// Whether a silo nanoframe `progress` built, at a site with `threat`, is
/// finished before its guards.
fn finish_first(progress: f32, threat: f32) -> bool {
    progress >= FINISH_FIRST && threat <= 0.0
}

/// Prefer the furthest-along live nanoframe, breaking ties deterministically.
fn silo_to_finish(units: impl Iterator<Item = Unit>) -> Option<Unit> {
    units
        .filter(|u| u.is_being_built())
        .max_by(|a, b| a.build_progress().total_cmp(&b.build_progress()).then_with(|| b.id().cmp(&a.id())))
}

impl EconomyManager {
    /// A dropped job does not destroy its nanoframe. Reattach to the live
    /// building, preserving its guard prerequisites, and stop competing jobs.
    fn reconcile_silo(&mut self, def: UnitDef, finish: Option<Unit>) {
        let jobs: Vec<_> = self
            .construction_tasks
            .iter()
            .copied()
            .filter(|&id| self.tasks.get(id).and_then(|t| t.as_construction()).is_some_and(|c| c.build_type == def))
            .collect();
        let keep = finish.and_then(|u| {
            jobs.iter()
                .copied()
                .find(|&id| self.tasks.get(id).is_some_and(|t| distance(t.position, u.pos()) < SITE_SLACK))
        });
        for id in jobs {
            if Some(id) != keep {
                self.drop_construction_task(id);
            }
        }
        if let Some(unit) = finish {
            let id = keep.unwrap_or_else(|| {
                // This is already a live building: a fresh build-site check
                // would reject its occupied footprint.
                let task = WorkerTask::construction(def, unit.pos(), unit.building_facing());
                let id = self.tasks.insert(task);
                self.construction_tasks.push(id);
                id
            });
            self.tasks.get_mut(id).unwrap().as_construction_mut().unwrap().target = Some(unit);
        }
    }

    /// Nanoframes must use the single construction job, including its guard
    /// gate and rush priority. Generic repairs would split the crew again.
    pub(super) fn silo_work_allowed(&self, task: &WorkerTask) -> bool {
        !matches!(&task.kind, TaskKind::Repair(r)
            if r.target.is_being_built()
                && r.target.def().is_some_and(|d| d.id() == self.build_ids.missile_silo_id))
    }
}

impl Ai {
    fn eco_silo_local_danger(&self, at: Vec3) -> bool {
        // Even a small force already firing here warrants protection;
        // the density threshold is for forces that have yet to arrive.
        self.war.get_threat(at) > 0.0 || self.war.get_enemy_power(at).max(self.eco_raiders_near(at)) >= PREPARE_POWER
    }

    fn eco_silo_needs_guards(&self, at: Vec3) -> bool {
        if self.eco_silo_local_danger(at) {
            return true;
        }
        let Some(strategy) = &self.strategy else { return false };
        let Some(map) = self.terrain().and_then(|t| t.tier(strategy.tier)) else { return false };
        let Some(here) = map.region_at(pos(at)) else { return false };
        // Recompute presence: the strategic snapshot can be thirty seconds old.
        let occupancy = Occupancy::new(map, &self.region_presence());
        let mut regions = vec![here];
        for (gate, region) in map.neighbours(here) {
            if map.chokes[gate].pos.distance(pos(at)) <= NEAR_REGION && !regions.contains(&region) {
                regions.push(region);
            }
        }
        regions.iter().map(|&r| region_density(occupancy.regions[r].enemy_power, map.regions[r].cells)).sum::<f32>()
            >= PREPARE_POWER
    }

    fn eco_prerequisite_unit(&self, required: &BuildPrerequisite) -> Option<Unit> {
        prerequisite_unit(self.cb, self.team_id, required)
    }

    /// Whether this silo job's nanoframe is far enough along to finish
    /// before its guards (see [`FINISH_FIRST`]).
    fn eco_silo_finishing(&self, c: &ConstructionTask, at: Vec3) -> bool {
        c.target.is_some_and(|u| {
            u.health() > 0.0 && u.is_being_built() && finish_first(u.build_progress(), self.war.get_threat(at))
        })
    }

    pub(super) fn eco_prerequisites_ready(&self, task: &WorkerTask) -> bool {
        task.as_construction().is_none_or(|c| {
            let silo = self.defs.name(c.build_type) == missiles::SILO;
            if silo && self.missiles_shuns_site(task.position) {
                return false;
            }
            if silo && self.eco_silo_finishing(c, task.position) {
                return true;
            }
            // Close the gate immediately if danger arrives between
            // preparation polls. The poll will queue the missing guards.
            let threatened = silo && c.prerequisites.is_empty() && self.eco_silo_local_danger(task.position);
            prerequisites_ready(self.cb, self.team_id, &c.prerequisites, threatened)
        })
    }

    fn eco_prerequisite_task(&self, required: &BuildPrerequisite) -> Option<TaskId> {
        self.eco.construction_tasks.iter().copied().find(|&id| {
            self.eco.tasks.get(id).is_some_and(|t| {
                distance(t.position, required.pos) < SITE_SLACK
                    && t.as_construction().is_some_and(|c| c.build_type.id() == required.def.id())
            })
        })
    }

    /// Reuse a live nanoframe or queued job; recreate a canceled/destroyed
    /// guard at its reserved position. Failure never unlocks the silo.
    fn eco_ensure_silo_guard(&mut self, required: &BuildPrerequisite) -> Option<Option<TaskId>> {
        let unit = self.eco_prerequisite_unit(required);
        if unit.is_some_and(|u| !u.is_being_built()) || self.eco_prerequisite_task(required).is_some() {
            return Some(None);
        }
        if unit.is_none() && !self.cb.map().is_possible_to_build_at(required.def, required.pos, 0) {
            return None;
        }
        let mut task = WorkerTask::construction(required.def, required.pos, 0);
        task.as_construction_mut().unwrap().target = unit;
        let list = match required.def.name().as_str() {
            "staticshield" => List::Construction,
            "turretaaflak" => List::Aa,
            _ => List::Porc,
        };
        // Placed on purpose, like conquest's guards: the Lotus-overlap rule
        // (no Lotus within twice its reach of another) refused the second
        // of the pair 2 x GUARD_SPREAD apart, so the full layout never fit.
        self.eco_add_construction_uncrowded(task, Some(list), (list != List::Construction).then_some(list)).map(Some)
    }

    fn eco_silo_guard_layout(&self, at: Vec3) -> [(&'static str, Vec3); 5] {
        let enemy = self.graph.get_enemy_center();
        let toward = if enemy == NULLPOS { self.graph.get_map_center() } else { enemy };
        let ahead = get_directional_point(at, toward, GUARD_AHEAD);
        let (dx, dz) = ((ahead.x - at.x) / GUARD_AHEAD, (ahead.z - at.z) / GUARD_AHEAD);
        let aside = |d: f32| Vec3::xz(ahead.x - dz * d, ahead.z + dx * d);
        [
            ("turretheavylaser", aside(0.0)),
            ("turretlaser", aside(GUARD_SPREAD)),
            ("turretlaser", aside(-GUARD_SPREAD)),
            ("turretaaflak", get_directional_point(at, toward, GUARD_AHEAD / 2.0)),
            ("staticshield", get_directional_point(at, toward, 110.0)),
        ]
    }

    /// Keep guards close to the chosen footprint. A generic 600-elmo
    /// relocation can put a shield or laser beyond the silo it guards.
    fn eco_silo_guard_site(&self, def: UnitDef, seed: Vec3) -> Option<Vec3> {
        let unit = self.cb.friendly_units_in_cylinder(seed, GUARD_SEARCH).into_iter().find(|u| {
            u.health() > 0.0
                && u.def().is_some_and(|d| d.id() == def.id())
                && (!u.is_being_built() || u.team() == self.team_id)
        });
        if let Some(unit) = unit {
            return Some(unit.pos());
        }
        if let Some(task) = self.eco.construction_tasks.iter().filter_map(|&id| self.eco.tasks.get(id)).find(|t| {
            distance(t.position, seed) <= GUARD_SEARCH
                && t.as_construction().is_some_and(|c| c.build_type.id() == def.id())
        }) {
            return Some(task.position);
        }
        // The engine does not know our queued footprints. Try around them
        // as well, without allowing the defense to drift away from the silo.
        [(0.0, 0.0), (64.0, 0.0), (-64.0, 0.0), (0.0, 64.0), (0.0, -64.0)].into_iter().find_map(|(x, z)| {
            let p = self.cb.map().find_closest_build_site(def, Vec3::xz(seed.x + x, seed.z + z), GUARD_SEARCH, 3, 0);
            (p.x >= 0.0 && distance(p, seed) <= GUARD_SEARCH && self.eco_build_check(def, p, 0)).then_some(p)
        })
    }

    fn eco_silo_plan(&self) -> Option<TaskId> {
        let silo = self.defs.get(missiles::SILO)?;
        self.eco.construction_tasks.iter().copied().find(|&id| {
            self.eco.tasks.get(id).and_then(|t| t.as_construction()).is_some_and(|c| c.build_type.id() == silo.id())
        })
    }

    /// Stop the original job as well as remembering the loss. Clearing only
    /// its target makes workers immediately build another nanoframe there.
    pub(super) fn eco_silo_lost(&mut self, unit: Unit) {
        let at = unit.pos();
        self.missiles_note_loss(at);
        let jobs: Vec<_> = self
            .eco
            .construction_tasks
            .iter()
            .copied()
            .filter(|&id| {
                self.eco.tasks.get(id).is_some_and(|t| {
                    t.as_construction().is_some_and(|c| {
                        self.defs.name(c.build_type) == missiles::SILO
                            && (c.target == Some(unit) || self.missiles_shuns_site(t.position))
                    })
                })
            })
            .collect();
        for id in jobs {
            self.eco.drop_construction_task(id);
        }
    }

    /// Reserve the guards before workers may resume this silo. A plan not
    /// yet begun needs the full layout, else it is dropped (a later plan
    /// may find a better site). A begun nanoframe keeps the guards that
    /// fit: dropping its job only re-adopted it next poll, so the metal in
    /// it never paid. From [`FINISH_FIRST`] built its workers stay on it.
    fn eco_prepare_silo(&mut self, id: TaskId) -> bool {
        let site = self.eco.task_pos(id);
        let Some(c) = self.eco.tasks.get(id).and_then(|t| t.as_construction()) else { return false };
        let frame = c.target.filter(|u| u.health() > 0.0 && u.is_being_built());
        let finishing = self.eco_silo_finishing(c, site);
        let mut guards = Vec::new();
        let mut created = Vec::new();
        let mut missing = Vec::new();
        for (name, seed) in self.eco_silo_guard_layout(site) {
            let required = self
                .defs
                .get(name)
                .and_then(|def| self.eco_silo_guard_site(def, seed).map(|pos| BuildPrerequisite { def, pos }));
            let queued = required.and_then(|r| self.eco_ensure_silo_guard(&r).map(|id| (r, id)));
            let Some((required, task)) = queued else {
                if frame.is_some() {
                    missing.push(name);
                    continue;
                }
                for task in created {
                    self.eco.drop_construction_task(task);
                }
                self.eco.drop_construction_task(id);
                self.log("missiles: no room for the full defenses; deferring the silo");
                return false;
            };
            guards.push(required);
            created.extend(task);
        }
        if let Some(unit) = frame
            && !missing.is_empty()
            && self.missiles.guards_noted != Some(unit.id())
        {
            self.missiles.guards_noted = Some(unit.id());
            self.log(&format!(
                "missiles: no room for {} by the silo at ({:.0}, {:.0}), {:.0}% built; {}",
                missing.join(", "),
                site.x,
                site.z,
                100.0 * unit.build_progress(),
                if finishing {
                    "finishing it first".to_owned()
                } else {
                    format!("waiting for the {} guards that fit", guards.len())
                }
            ));
        }
        // With no guard to wait for, the gate closes only while danger is
        // at the site itself (`eco_prerequisites_ready`).
        if !guards.is_empty() && !finishing {
            let idle = self.eco.stop_workers(id);
            self.eco.idle_workers(&idle);
        }
        self.eco.tasks.get_mut(id).unwrap().as_construction_mut().unwrap().prerequisites = guards;
        true
    }

    /// A safe site can become a front while its constructors are travelling.
    /// Recheck pending and started jobs, independently of factory requisition.
    pub(super) fn eco_update_silo_preparation(&mut self) {
        let Some(id) = self.eco_silo_plan() else { return };
        let task = self.eco.tasks.get(id).unwrap();
        let site = task.position;
        let c = task.as_construction().unwrap();
        let guards = c.prerequisites.clone();
        let begun = c.target.is_some_and(|u| u.health() > 0.0 && u.is_being_built());
        let finishing = self.eco_silo_finishing(c, site);
        if self.missiles_shuns_site(site) {
            self.eco.drop_construction_task(id);
            return;
        }
        if guards.is_empty() && self.eco_silo_needs_guards(site) {
            if self.eco_prepare_silo(id) {
                let has_guards = self
                    .eco
                    .tasks
                    .get(id)
                    .and_then(|t| t.as_construction())
                    .is_some_and(|c| !c.prerequisites.is_empty());
                if has_guards {
                    self.log(&format!(
                        "missiles: danger reached the silo site at ({:.0}, {:.0}); {}",
                        site.x,
                        site.z,
                        if finishing {
                            "finishing the nanoframe first, guards queued"
                        } else {
                            "pausing for completed lasers, flak and shield"
                        }
                    ));
                }
            }
            return;
        }
        if !finishing && !prerequisites_ready(self.cb, self.team_id, &guards, false) {
            let idle = self.eco.stop_workers(id);
            self.eco.idle_workers(&idle);
        }
        let mut blocked = Vec::new();
        for (i, guard) in guards.iter().enumerate() {
            if self.eco_ensure_silo_guard(guard).is_none() {
                if !begun {
                    self.eco.drop_construction_task(id);
                    self.log("missiles: abandoning silo preparation: a required defense site is blocked");
                    return;
                }
                blocked.push(i);
            }
        }
        // A begun nanoframe stops waiting for a guard whose site is gone.
        if !blocked.is_empty()
            && let Some(c) = self.eco.tasks.get_mut(id).and_then(|t| t.as_construction_mut())
        {
            c.prerequisites =
                guards.iter().enumerate().filter(|(i, _)| !blocked.contains(i)).map(|(_, &g)| g).collect();
            let left = c.prerequisites.len();
            self.log(&format!(
                "missiles: {} guard site(s) of the begun silo at ({:.0}, {:.0}) blocked; waiting for the other {left}",
                blocked.len(),
                site.x,
                site.z
            ));
        }
    }

    pub(super) fn eco_plan_missile_silo(&mut self) {
        if self.dev_off("requisition") || self.op_requisition().is_some() {
            self.missiles.site.cancel();
            return;
        }
        let Some(silo) = self.defs.get(missiles::SILO) else { return };
        // Live units are authoritative, even after threat cleanup dropped
        // their jobs or before unitCreated attached them to a task.
        let existing: Vec<_> = self
            .cb
            .team_units()
            .into_iter()
            .filter(|u| u.health() > 0.0 && u.team() == self.team_id && u.def() == Some(silo))
            .collect();
        if !existing.is_empty() {
            self.missiles.site.cancel();
            let completed = existing.iter().any(|u| !u.is_being_built());
            let finish = (!completed)
                .then(|| {
                    silo_to_finish(
                        existing
                            .iter()
                            .copied()
                            .filter(|u| self.war.get_porc_threat(u.pos()) <= 0.0 && !self.missiles_shuns_site(u.pos())),
                    )
                })
                .flatten();
            self.eco.reconcile_silo(silo, finish);
            self.eco_update_silo_preparation();
            // Wait for an unsafe surviving silo to become usable, rather
            // than spending another 1200 metal at a replacement site.
            // Preparation may drop the recovered job too. The live
            // nanoframe still prevents commissioning a replacement.
            return;
        }
        // Keep the selected project (ours or an ally's). Pending own jobs
        // can still be adopted by eco_watch_allied_projects; do not open a
        // second assist each time another allied frame overtakes this one.
        if self.eco_silo_plan().is_some() {
            self.missiles.site.cancel();
            return;
        }
        if !self.dev_off("missiles")
            && !self.eco.hard_estalled
            && self.eco.adjusted_income >= missiles::SILO_INCOME
            && self.eco_assist_ally(silo)
        {
            return;
        }
        if self.dev_off("missiles")
            || self.missiles_silo().is_some()
            || self.missiles.planned.is_some_and(|f| self.frame - f < SILO_REPLAN)
        {
            self.missiles.site.cancel();
            return;
        }
        // Affordability pauses admission, not the pending search. Growth/overdrive
        // demand is not a construction stall. Completed work stays in the worker
        // until affordable, then is checked against current demand and terrain.
        if self.eco.adjusted_income < missiles::SILO_INCOME || self.eco.hard_estalled {
            return;
        }
        let Some(choice) = self.missiles_site() else { return };
        let site = choice.pos;
        // Reserve the exact scored footprint first, so guard placement
        // cannot occupy it. No worker assignment runs during this planning.
        let Some(id) = self.eco_add_simple(silo, site, List::Construction, None) else { return };
        self.missiles.planned = Some(self.frame);
        let threatened = self.eco_silo_needs_guards(site);
        if threatened && !self.eco_prepare_silo(id) {
            return;
        }
        self.log(&format!(
            "missiles: silo at ({:.0}, {:.0}), {:.0} Eos/Shockley payoff ({} targets); \
             {:.0} static metal in reach ({:.1}%), {:.1}% held enemy territory; battlefield density {:.0}, \
             80% static coverage needs >{:.0}; other coverage needs >{:.0} metal/Melmo² (laneness {:.2}); {}",
            site.x,
            site.z,
            choice.payoff,
            choice.targets,
            choice.coverage.value,
            100.0 * choice.coverage.share,
            100.0 * choice.territory_share,
            choice.siege.density,
            choice.siege.cook_threshold(),
            choice.siege.threshold,
            choice.siege.laneness,
            if threatened { "waiting for completed lasers, flak and shield" } else { "summoning constructors" }
        ));
    }

    /// Completed silos get missile priority and caretakers. Reuse prepared
    /// guards; quiet sites may add their defenses now without delaying the silo.
    pub fn eco_fortify_silo(&mut self, silo: Unit) {
        silo.execute_custom_command(cmd::PRIORITY, &[2.0], 0, NO_TIMEOUT);
        let at = silo.pos();
        for (name, seed) in self.eco_silo_guard_layout(at) {
            let Some(def) = self.defs.get(name) else { continue };
            let Some(pos) = self.eco_silo_guard_site(def, seed) else { continue };
            self.eco_ensure_silo_guard(&BuildPrerequisite { def, pos });
        }
        let wid = self
            .eco
            .workers
            .values()
            .min_by(|a, b| distance(a.get_pos(), at).total_cmp(&distance(b.get_pos(), at)))
            .map(|w| w.id);
        if let Some(wid) = wid {
            for _ in 0..CARETAKERS {
                self.eco_create_nano_turret_task(wid, silo);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use springai::Callback;

    #[test]
    fn growth_demand_allows_search_and_temporary_unaffordability_preserves_it() {
        use std::cell::Cell;
        use std::ffi::c_char;
        thread_local! {
            static DEFINITIONS: Cell<bool> = const { Cell::new(false) };
            static QUERIES: Cell<usize> = const { Cell::new(0) };
        }
        unsafe extern "C" fn defs(_: i32, out: *mut i32, max: i32) -> i32 {
            if !DEFINITIONS.get() {
                return 0;
            }
            if max > 0 && !out.is_null() {
                unsafe {
                    *out = 1;
                }
            }
            1
        }
        unsafe extern "C" fn name(_: i32, _: i32) -> *const c_char {
            c"staticmissilesilo".as_ptr()
        }
        unsafe extern "C" fn units(_: i32, _: *mut i32, _: i32) -> i32 {
            0
        }
        unsafe extern "C" fn site(_: i32, _: i32, _: *mut f32, _: f32, _: i32, _: i32, out: *mut f32) {
            QUERIES.set(QUERIES.get() + 1);
            unsafe {
                std::ptr::copy_nonoverlapping([-1.0, 0.0, 0.0].as_ptr(), out, 3);
            }
        }
        DEFINITIONS.set(false);
        QUERIES.set(0);
        let mut ai = super::super::test_ai_with(|t| {
            t.getUnitDefs = Some(defs);
            t.UnitDef_getName = Some(name);
            t.getFriendlyUnits = Some(units);
            t.getTeamUnits = Some(units);
            t.Map_findClosestBuildSite = Some(site);
        });
        DEFINITIONS.set(true);
        ai.defs = crate::defs::Defs::new(ai.cb);
        ai.frame = 600;
        ai.eco.adjusted_income = missiles::SILO_INCOME;
        ai.eco.estalled = true;
        ai.eco_plan_missile_silo();
        assert_eq!(QUERIES.get(), 2, "growth demand must not block candidate collection");
        assert!(ai.missiles.site.active());
        ai.frame += 1;
        ai.eco.hard_estalled = true;
        ai.eco_plan_missile_silo();
        assert!(ai.missiles.site.active(), "a hard stall pauses, rather than erases, the search");
        assert_eq!(QUERIES.get(), 2);
        ai.frame += 1;
        ai.eco.hard_estalled = false;
        ai.eco.adjusted_income = missiles::SILO_INCOME - 1.0;
        ai.eco_plan_missile_silo();
        assert!(ai.missiles.site.active());
        assert_eq!(QUERIES.get(), 2);
        // A real policy override still cancels the outstanding search.
        ai.set_feature_off("missiles", true);
        ai.eco_plan_missile_silo();
        assert!(!ai.missiles.site.active());
    }

    #[test]
    fn nearly_built_silos_finish_before_their_guards_unless_under_fire() {
        // The Adamantine silo: 99.5% built, danger in the region, none firing.
        assert!(finish_first(0.995, 0.0));
        assert!(finish_first(FINISH_FIRST, 0.0));
        assert!(!finish_first(0.36, 0.0));
        assert!(!finish_first(0.995, 0.5));
    }

    #[test]
    fn guard_lotuses_stand_clear_of_the_stinger_s_death_blast() {
        // porcmix: death blasts of 130 (Stinger) and 90 (Lotus) elmos.
        use crate::economy::porcmix::blast_room;
        assert!(blast_room(GUARD_SPREAD, 90.0, 130.0) >= 1.0);
        assert!(blast_room(2.0 * GUARD_SPREAD, 90.0, 90.0) >= 1.0);
    }

    #[test]
    fn nearby_region_density_distinguishes_an_army_from_a_sparse_region() {
        assert!(region_density(600.0, 4000) >= PREPARE_POWER);
        assert!(region_density(600.0, 40000) < PREPARE_POWER);
        assert!(region_density(50.0, 1) < PREPARE_POWER);
        assert!(region_density(0.0, 4000) < PREPARE_POWER);
        // Forces split across two entrances still pose one combined threat.
        assert!(2.0 * region_density(100.0, 3000) >= PREPARE_POWER);
    }

    #[derive(Clone)]
    struct Guard {
        def: i32,
        pos: Vec3,
        health: f32,
        building: bool,
        progress: f32,
    }

    thread_local! {
        static GUARDS: std::cell::RefCell<Vec<Guard>> = const { std::cell::RefCell::new(Vec::new()) };
    }

    fn callback() -> Callback {
        unsafe extern "C" fn friendly(_: i32, _: *mut f32, _: f32, spherical: bool, out: *mut i32, max: i32) -> i32 {
            // Build positions are ground positions; tall turrets must count.
            assert!(!spherical);
            GUARDS.with(|g| {
                let n = g.borrow().len();
                for i in 0..if out.is_null() { 0 } else { n.min(max.max(0) as usize) } {
                    unsafe {
                        *out.add(i) = i as i32;
                    }
                }
                n as i32
            })
        }
        unsafe extern "C" fn def(_: i32, id: i32) -> i32 {
            GUARDS.with(|g| g.borrow()[id as usize].def)
        }
        unsafe extern "C" fn health(_: i32, id: i32) -> f32 {
            GUARDS.with(|g| g.borrow()[id as usize].health)
        }
        unsafe extern "C" fn building(_: i32, id: i32) -> bool {
            GUARDS.with(|g| g.borrow()[id as usize].building)
        }
        unsafe extern "C" fn team(_: i32, _: i32) -> i32 {
            1
        }
        unsafe extern "C" fn position(_: i32, id: i32, out: *mut f32) {
            let p = GUARDS.with(|g| g.borrow()[id as usize].pos);
            unsafe {
                std::ptr::copy_nonoverlapping([p.x, p.y, p.z].as_ptr(), out, 3);
            }
        }
        unsafe extern "C" fn progress(_: i32, id: i32) -> f32 {
            GUARDS.with(|g| g.borrow()[id as usize].progress)
        }
        unsafe extern "C" fn facing(_: i32, _: i32) -> i32 {
            0
        }
        unsafe extern "C" fn size(_: i32) -> i32 {
            1
        }
        unsafe extern "C" fn water(_: i32) -> f32 {
            0.0
        }
        unsafe extern "C" fn heights(_: i32, _: *mut f32, _: i32) -> i32 {
            0
        }
        let table = springai::ffi::SSkirmishAICallback {
            Unit_getBuildProgress: Some(progress),
            Unit_getBuildingFacing: Some(facing),
            Map_getWidth: Some(size),
            Map_getHeight: Some(size),
            Map_getWaterDamage: Some(water),
            Map_getHeightMap: Some(heights),
            getFriendlyUnitsIn: Some(friendly),
            Unit_getDef: Some(def),
            Unit_getHealth: Some(health),
            Unit_isBeingBuilt: Some(building),
            Unit_getTeam: Some(team),
            Unit_getPos: Some(position),
            ..Default::default()
        };
        unsafe { Callback::from_raw(0, Box::leak(Box::new(table))) }
    }

    #[test]
    fn orphaned_silos_resume_one_nanoframe_and_reuse_its_guard_gate() {
        let cb = callback();
        let def = UnitDef::from_id(cb, 10).unwrap();
        GUARDS.with(|g| {
            *g.borrow_mut() = [0.2, 0.6, 0.2]
                .into_iter()
                .enumerate()
                .map(|(i, progress)| Guard {
                    def: 10,
                    pos: Vec3::xz(1000.0 * (i + 1) as f32, 1000.0),
                    health: 100.0,
                    building: true,
                    progress,
                })
                .collect()
        });
        let units: Vec<_> = (0..3).map(|id| Unit::from_id(cb, id).unwrap()).collect();
        let mut eco = EconomyManager::new(cb);
        eco.build_ids.missile_silo_id = def.id();
        // The most advanced nanoframe has lost its construction job; a
        // replacement is already queued somewhere else.
        let pending = eco.tasks.insert(WorkerTask::construction(def, Vec3::xz(5000.0, 1000.0), 0));
        eco.construction_tasks.push(pending);
        let finish = silo_to_finish(units.iter().copied()).unwrap();
        assert_eq!(finish, units[1]);
        eco.reconcile_silo(def, Some(finish));
        assert!(eco.tasks.get(pending).is_none());
        assert_eq!(eco.construction_tasks.len(), 1);
        let id = eco.construction_tasks[0];
        let task = eco.tasks.get_mut(id).unwrap();
        assert_eq!(task.as_construction().unwrap().target, Some(finish));
        assert_eq!(task.position, finish.pos());
        task.as_construction_mut().unwrap().prerequisites.push(BuildPrerequisite { def, pos: Vec3::ZERO });
        // Repeated planning preserves the same job and its defenses.
        eco.reconcile_silo(def, Some(finish));
        assert_eq!(eco.construction_tasks, vec![id]);
        assert_eq!(eco.tasks.get(id).unwrap().as_construction().unwrap().prerequisites.len(), 1);
        assert!(eco.silo_work_allowed(eco.tasks.get(id).unwrap()));
        for &target in &units {
            let repair = WorkerTask {
                id: 0,
                position: target.pos(),
                assigned_workers: Vec::new(),
                kind: TaskKind::Repair(super::super::tasks::RepairTask {
                    target,
                    is_shield_mob: false,
                    is_tank: false,
                }),
            };
            assert!(!eco.silo_work_allowed(&repair));
        }
        // Completion permits normal damage repair and ends competing builds.
        GUARDS.with(|g| g.borrow_mut()[1].building = false);
        eco.reconcile_silo(def, None);
        assert!(eco.construction_tasks.is_empty());
        let repair = WorkerTask {
            id: 0,
            position: finish.pos(),
            assigned_workers: Vec::new(),
            kind: TaskKind::Repair(super::super::tasks::RepairTask {
                target: finish,
                is_shield_mob: false,
                is_tank: false,
            }),
        };
        assert!(eco.silo_work_allowed(&repair));
        assert_eq!(silo_to_finish(units.iter().copied()), Some(units[0]));
        assert_eq!(silo_to_finish(units.iter().rev().copied()), Some(units[0]));
    }

    #[test]
    fn silo_waits_for_every_live_completed_guard() {
        let cb = callback();
        let guards: Vec<_> = (1..=5)
            .map(|id| BuildPrerequisite {
                def: UnitDef::from_id(cb, id).unwrap(),
                pos: Vec3::xz(id as f32 * 100.0, 100.0),
            })
            .collect();
        assert!(!prerequisites_ready(cb, 1, &guards, true)); // Jobs alone do not count.
        GUARDS.with(|g| {
            *g.borrow_mut() = guards
                .iter()
                .map(|p| Guard {
                    def: p.def.id(),
                    pos: Vec3::new(p.pos.x, 200.0, p.pos.z),
                    health: 100.0,
                    building: true,
                    progress: 0.2,
                })
                .collect()
        });
        assert!(!prerequisites_ready(cb, 1, &guards, true)); // All nanoframes.
        for i in 0..5 {
            assert!(!prerequisites_ready(cb, 1, &guards, true));
            GUARDS.with(|g| g.borrow_mut()[i].building = false);
        }
        assert!(prerequisites_ready(cb, 1, &guards, true));
        GUARDS.with(|g| g.borrow_mut()[2].health = 0.0);
        assert!(!prerequisites_ready(cb, 1, &guards, true)); // A destroyed guard closes the gate again.
        GUARDS.with(|g| g.borrow_mut()[2].health = 100.0);
        assert!(prerequisites_ready(cb, 1, &guards, true));
        GUARDS.with(|g| g.borrow_mut()[2].pos.x += 100.0);
        assert!(!prerequisites_ready(cb, 1, &guards, true)); // Same type elsewhere is not protection here.
        assert!(prerequisites_ready(cb, 1, &[], false)); // Quiet sites have no preparation gate.
        assert!(!prerequisites_ready(cb, 1, &[], true)); // A newly threatened site must acquire guards first.
        GUARDS.with(|g| g.borrow_mut().clear());
        assert!(!prerequisites_ready(cb, 1, &guards, true)); // Deleted tasks/buildings cannot unlock it.
    }
}
