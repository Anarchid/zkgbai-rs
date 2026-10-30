//! Sparrow morphs get their own radar beside the factory buildpower.

use springai::{NO_TIMEOUT, Unit, UnitDef, Vec3};

use super::{EconomyManager, List, TaskId, erase_one};
use crate::Ai;
use crate::defs::cmd;
use crate::util::distance;

struct BuildPower {
    pos: Vec3,
    range: f32,
    power: f32,
}

/// Prefer the most caretaker buildpower in range, then proximity to the
/// factory. Without caretakers this falls back to a site by a factory.
fn best_site(candidates: impl Iterator<Item = (Vec3, Vec3)>, builders: &[BuildPower]) -> Option<Vec3> {
    candidates
        .map(|(site, factory)| {
            let power: f32 = builders.iter().filter(|b| distance(b.pos, site) <= b.range).map(|b| b.power).sum();
            (site, power, distance(site, factory))
        })
        .max_by(|a, b| a.1.total_cmp(&b.1).then_with(|| b.2.total_cmp(&a.2)))
        .map(|c| c.0)
}

impl Ai {
    pub(super) fn eco_plan_sparrow(&mut self) {
        if self.eco.sparrow_radar_task.is_some()
            || self.eco.sparrows.iter().any(|u| self.defs.name_of(*u) == "staticradar")
        {
            return;
        }
        let (Some(radar), Some(_)) = (self.defs.get("staticradar"), self.defs.get("planelightscout")) else {
            return;
        };
        let factories: Vec<_> = self
            .fac
            .factories
            .values()
            .filter(|f| f.unit.health() > 0.0 && f.unit.team() == self.team_id)
            .map(|f| (f.id, f.get_pos()))
            .collect();
        let builders: Vec<_> = self
            .eco
            .nanos
            .values()
            .filter(|n| {
                factories.iter().any(|f| f.0 == n.target.id())
                    && n.unit.health() > 0.0
                    && n.unit.team() == self.team_id
                    && !n.unit.is_being_built()
            })
            .filter_map(|n| {
                let def = n.unit.def()?;
                Some(BuildPower { pos: n.unit.pos(), range: def.build_distance(), power: def.build_speed() })
            })
            .collect();
        let workers: Vec<_> = self
            .eco
            .workers
            .values()
            .filter(|w| w.unit.health() > 0.0 && w.unit.team() == self.team_id)
            .filter_map(|w| {
                let def = w.unit.def()?;
                def.build_options().contains(&radar).then(|| (def, w.get_pos()))
            })
            .collect();

        // Sample both sides of each factory: caretakers need not be
        // distributed evenly around it. Keep the whole search near base.
        let mut candidates = Vec::new();
        for &(_, factory) in &factories {
            for x in -3..=3 {
                for z in -3..=3 {
                    let at = Vec3::xz(factory.x + x as f32 * 128.0, factory.z + z as f32 * 128.0);
                    let site = self.cb.map().find_closest_build_site(radar, at, 96.0, 3, 0);
                    if site.x < 0.0
                        || distance(site, factory) > 600.0
                        || !self.eco_build_check(radar, site, 0)
                        || self.war.get_porc_threat(site) > 0.0
                        || !workers.iter().any(|&(def, pos)| self.terrain_reaches(def, pos, site, def.build_distance()))
                    {
                        continue;
                    }
                    candidates.push((site, factory));
                }
            }
        }
        let Some(site) = best_site(candidates.into_iter(), &builders) else { return };
        // Deliberately outside radar_tasks: this temporary radar must not
        // suppress construction of radars that provide lasting coverage.
        self.eco.sparrow_radar_task = self.eco_add_simple(radar, site, List::Construction, None);
    }
}

impl EconomyManager {
    /// Only the completed radar belonging to our dedicated job may morph.
    pub(super) fn morph_sparrow_radar(&mut self, task: TaskId, radar: Unit, scout: UnitDef) {
        if self.sparrows.contains(&radar)
            || self.sparrow_radar_task != Some(task)
            || !self.tasks.get(task).and_then(|t| t.as_construction()).is_some_and(|c| c.target == Some(radar))
        {
            return;
        }
        if radar.execute_custom_command(cmd::MORPH, &[scout.id() as f32], 0, NO_TIMEOUT) {
            self.sparrows.push(radar);
            erase_one(&mut self.radars, &radar);
            radar.execute_custom_command(cmd::MISC_PRIORITY, &[3.0], 0, NO_TIMEOUT);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::economy::WorkerTask;
    use springai::{Callback, ffi};

    thread_local! {
        static ORDERS: std::cell::RefCell<Vec<(i32, i32)>> = const { std::cell::RefCell::new(Vec::new()) };
    }

    fn economy() -> (EconomyManager, Callback) {
        unsafe extern "C" fn size(_: i32) -> i32 {
            1
        }
        unsafe extern "C" fn water(_: i32) -> f32 {
            0.0
        }
        unsafe extern "C" fn heights(_: i32, _: *mut f32, _: i32) -> i32 {
            0
        }
        unsafe extern "C" fn command(_: i32, _: i32, _: i32, topic: i32, data: *mut std::ffi::c_void) -> i32 {
            assert_eq!(topic as u32, ffi::CommandTopic::COMMAND_UNIT_CUSTOM);
            let c = unsafe { &*(data as *const ffi::SCustomUnitCommand) };
            ORDERS.with(|o| o.borrow_mut().push((c.unitId, c.cmdId)));
            0
        }
        let table = ffi::SSkirmishAICallback {
            Map_getWidth: Some(size),
            Map_getHeight: Some(size),
            Map_getWaterDamage: Some(water),
            Map_getHeightMap: Some(heights),
            Engine_handleCommand: Some(command),
            ..Default::default()
        };
        let cb = unsafe { Callback::from_raw(0, Box::leak(Box::new(table))) };
        ORDERS.with(|o| o.borrow_mut().clear());
        (EconomyManager::new(cb), cb)
    }

    fn radar_task(eco: &mut EconomyManager, cb: Callback, unit: Unit) -> TaskId {
        let mut task = WorkerTask::construction(UnitDef::from_id(cb, 1).unwrap(), Vec3::xz(100.0, 100.0), 0);
        task.as_construction_mut().unwrap().target = Some(unit);
        let id = eco.tasks.insert(task);
        eco.construction_tasks.push(id);
        id
    }

    #[test]
    fn only_the_dedicated_radars_completion_can_start_a_morph() {
        let (mut eco, cb) = economy();
        let coverage = Unit::from_id(cb, 10).unwrap();
        let dedicated = Unit::from_id(cb, 11).unwrap();
        let scout = UnitDef::from_id(cb, 2).unwrap();
        let coverage_job = radar_task(&mut eco, cb, coverage);
        let dedicated_job = radar_task(&mut eco, cb, dedicated);
        eco.sparrow_radar_task = Some(dedicated_job);
        eco.radars = vec![coverage, dedicated];

        eco.morph_sparrow_radar(coverage_job, coverage, scout);
        eco.morph_sparrow_radar(dedicated_job, coverage, scout);
        assert!(eco.sparrows.is_empty());
        ORDERS.with(|o| assert!(o.borrow().is_empty()));
        eco.delete_task(coverage_job);
        assert_eq!(eco.sparrow_radar_task, Some(dedicated_job));

        eco.morph_sparrow_radar(dedicated_job, dedicated, scout);
        assert_eq!(eco.sparrows, vec![dedicated]);
        assert_eq!(eco.radars, vec![coverage]);
        ORDERS.with(|o| assert_eq!(*o.borrow(), [(11, cmd::MORPH), (11, cmd::MISC_PRIORITY)]));
        // Repeated completion cannot issue another morph.
        eco.morph_sparrow_radar(dedicated_job, dedicated, scout);
        ORDERS.with(|o| assert_eq!(o.borrow().len(), 2));
        eco.delete_task(dedicated_job);
        assert!(eco.sparrow_radar_task.is_none());
        assert_eq!(eco.sparrows, vec![dedicated]);
    }

    #[test]
    fn additional_sparrows_can_morph_without_replacing_existing_scouts() {
        let (mut eco, cb) = economy();
        let first = Unit::from_id(cb, 10).unwrap();
        let radar = Unit::from_id(cb, 11).unwrap();
        let scout = UnitDef::from_id(cb, 2).unwrap();
        let job = radar_task(&mut eco, cb, radar);
        eco.sparrows.push(first);
        eco.sparrow_radar_task = Some(job);
        eco.morph_sparrow_radar(job, radar, scout);
        assert_eq!(eco.sparrows, vec![first, radar]);
        eco.morph_sparrow_radar(job, radar, scout);
        ORDERS.with(|o| assert_eq!(o.borrow().len(), 2));
    }

    #[test]
    fn canceling_the_dedicated_job_allows_a_replacement() {
        let (mut eco, cb) = economy();
        let radar = Unit::from_id(cb, 10).unwrap();
        let job = radar_task(&mut eco, cb, radar);
        eco.sparrow_radar_task = Some(job);
        eco.drop_construction_task(job);
        assert!(eco.sparrow_radar_task.is_none());
        assert!(eco.sparrows.is_empty());
        assert!(eco.tasks.get(job).is_none());
        assert!(eco.radar_tasks.is_empty());
    }

    #[test]
    fn sparrow_prefers_the_factory_with_concentrated_buildpower() {
        let first = Vec3::xz(1000.0, 1000.0);
        let main = Vec3::xz(3000.0, 1000.0);
        let close = Vec3::xz(1100.0, 1000.0);
        let supported = Vec3::xz(3200.0, 1000.0);
        let unsupported = Vec3::xz(2800.0, 1000.0);
        let builders = [
            BuildPower { pos: close, range: 150.0, power: 10.0 },
            BuildPower { pos: Vec3::xz(3300.0, 900.0), range: 250.0, power: 10.0 },
            BuildPower { pos: Vec3::xz(3300.0, 1100.0), range: 250.0, power: 10.0 },
        ];
        let sites = [(close, first), (unsupported, main), (supported, main)];
        assert_eq!(best_site(sites.into_iter(), &builders), Some(supported));
    }

    #[test]
    fn sparrow_uses_a_nearby_site_without_caretakers_and_waits_without_a_legal_site() {
        let factory = Vec3::xz(1000.0, 1000.0);
        let near = Vec3::xz(1100.0, 1000.0);
        let far = Vec3::xz(1400.0, 1000.0);
        assert_eq!(best_site([(far, factory), (near, factory)].into_iter(), &[]), Some(near));
        assert_eq!(best_site(std::iter::empty(), &[]), None);
    }
}
