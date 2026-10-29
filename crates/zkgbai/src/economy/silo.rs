//! A tactical silo's final footprint is also its firing position. Reserve
//! it while preparing a threatened site; only completed defenses unlock it.

use springai::{Callback, NO_TIMEOUT, Unit, UnitDef, Vec3};
use zk_terrain::occupancy::Occupancy;

use super::tasks::BuildPrerequisite;
use super::{List, SITE_SLACK, TaskId, WorkerTask};
use crate::Ai;
use crate::defs::cmd;
use crate::military::missiles;
use crate::terrain::{HELD_POWER, pos};
use crate::util::{NULLPOS, distance, get_directional_point};

const SILO_REPLAN: i32 = 30 * 120;
const GUARD_AHEAD: f32 = 220.0;
const GUARD_SPREAD: f32 = 110.0;
const GUARD_SEARCH: f32 = 96.0;
const CARETAKERS: usize = 2;
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

fn prerequisites_ready(cb: Callback, team: i32, requirements: &[BuildPrerequisite]) -> bool {
    requirements.iter().all(|p| prerequisite_unit(cb, team, p).is_some_and(|u| !u.is_being_built()))
}

impl Ai {
    fn eco_silo_needs_guards(&self, at: Vec3) -> bool {
        let local = self.war.get_enemy_power(at).max(self.eco_raiders_near(at));
        if local >= PREPARE_POWER {
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

    pub(super) fn eco_prerequisites_ready(&self, task: &WorkerTask) -> bool {
        task.as_construction().is_none_or(|c| prerequisites_ready(self.cb, self.team_id, &c.prerequisites))
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
        self.eco_add_construction(task, Some(list), (list != List::Construction).then_some(list)).map(Some)
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

    pub(super) fn eco_plan_missile_silo(&mut self) {
        let Some(silo) = self.defs.get(missiles::SILO) else { return };
        let planned = self.eco.construction_tasks.iter().copied().find(|&id| {
            self.eco.tasks.get(id).and_then(|t| t.as_construction()).is_some_and(|c| c.build_type.id() == silo.id())
        });
        if let Some(id) = planned {
            let c = self.eco.tasks.get(id).unwrap().as_construction().unwrap();
            let started = c.target.is_some();
            let guards = c.prerequisites.clone();
            for guard in guards {
                if self.eco_ensure_silo_guard(&guard).is_none() && !started {
                    self.eco.drop_construction_task(id);
                    self.log("missiles: abandoning silo preparation: a required defense site is blocked");
                    break;
                }
            }
            return;
        }
        if self.dev_off("missiles")
            || self.missiles_silo().is_some()
            || self.eco.adjusted_income < missiles::SILO_INCOME
            || self.eco.estalled
            || self.missiles.planned.is_some_and(|f| self.frame - f < SILO_REPLAN)
        {
            return;
        }
        let Some(choice) = self.missiles_site() else {
            self.missiles_report_no_site();
            return;
        };
        let site = choice.pos;
        // Reserve the exact scored footprint first, so guard placement
        // cannot occupy it. No worker assignment runs during this planning.
        let Some(id) = self.eco_add_simple(silo, site, List::Construction, None) else { return };
        self.missiles.planned = Some(self.frame);
        let threatened = self.eco_silo_needs_guards(site);
        if threatened {
            let mut guards = Vec::new();
            let mut created = Vec::new();
            for (name, seed) in self.eco_silo_guard_layout(site) {
                let required = self
                    .defs
                    .get(name)
                    .and_then(|def| self.eco_silo_guard_site(def, seed).map(|pos| BuildPrerequisite { def, pos }));
                let queued = required.and_then(|r| self.eco_ensure_silo_guard(&r).map(|id| (r, id)));
                let Some((required, task)) = queued else {
                    for task in created {
                        self.eco.drop_construction_task(task);
                    }
                    self.eco.drop_construction_task(id);
                    self.log("missiles: no room for the full defenses; deferring the silo");
                    return;
                };
                guards.push(required);
                created.extend(task);
            }
            self.eco.tasks.get_mut(id).unwrap().as_construction_mut().unwrap().prerequisites = guards;
        }
        self.log(&format!(
            "missiles: silo at ({:.0}, {:.0}), {:.0} Eos/Shockley payoff ({} targets); \
             {:.0} static metal in reach ({:.1}%, density {:.0} metal/Melmo²); {}",
            site.x,
            site.z,
            choice.payoff,
            choice.targets,
            choice.coverage.value,
            100.0 * choice.coverage.share,
            choice.coverage.density,
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
        let table = springai::ffi::SSkirmishAICallback {
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
    fn silo_waits_for_every_live_completed_guard() {
        let cb = callback();
        let guards: Vec<_> = (1..=5)
            .map(|id| BuildPrerequisite {
                def: UnitDef::from_id(cb, id).unwrap(),
                pos: Vec3::xz(id as f32 * 100.0, 100.0),
            })
            .collect();
        assert!(!prerequisites_ready(cb, 1, &guards)); // Jobs alone do not count.
        GUARDS.with(|g| {
            *g.borrow_mut() = guards
                .iter()
                .map(|p| Guard {
                    def: p.def.id(),
                    pos: Vec3::new(p.pos.x, 200.0, p.pos.z),
                    health: 100.0,
                    building: true,
                })
                .collect()
        });
        assert!(!prerequisites_ready(cb, 1, &guards)); // All nanoframes.
        for i in 0..5 {
            assert!(!prerequisites_ready(cb, 1, &guards));
            GUARDS.with(|g| g.borrow_mut()[i].building = false);
        }
        assert!(prerequisites_ready(cb, 1, &guards));
        GUARDS.with(|g| g.borrow_mut()[2].health = 0.0);
        assert!(!prerequisites_ready(cb, 1, &guards)); // A destroyed guard closes the gate again.
        GUARDS.with(|g| g.borrow_mut()[2].health = 100.0);
        assert!(prerequisites_ready(cb, 1, &guards));
        GUARDS.with(|g| g.borrow_mut()[2].pos.x += 100.0);
        assert!(!prerequisites_ready(cb, 1, &guards)); // Same type elsewhere is not protection here.
        assert!(prerequisites_ready(cb, 1, &[])); // Quiet sites have no preparation gate.
        GUARDS.with(|g| g.borrow_mut().clear());
        assert!(!prerequisites_ready(cb, 1, &guards)); // Deleted tasks/buildings cannot unlock it.
    }
}
