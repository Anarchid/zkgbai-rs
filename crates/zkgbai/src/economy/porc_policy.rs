//! One admission policy for economy-planned ground turrets. Asset guards,
//! pooled upgrades and conquest must not each invent their own porc budget.

use springai::{UnitDef, Vec3};

use super::{List, WorkerTask};
use crate::{
    Ai,
    util::{NULLPOS, distance},
};

const MIDLINE_BAND: f32 = 160.0;
const CHOKE_RADIUS: f32 = 300.0;
const FACTORY_RADIUS: f32 = 400.0;
const EMERGENCY_HORIZON: f32 = 60.0;
const ROUTINE_CAP: f32 = 1200.0;
const EMERGENCY_RESERVE: f32 = 600.0;
const LOCAL_CAP: f32 = 600.0;

fn ground_turret(name: &str) -> bool {
    name.starts_with("turret") && !name.starts_with("turretaa")
}

/// Signed perpendicular distance from the start-position bisector, positive
/// on the enemy side. Comparing raw distances would widen the band at map edges.
fn midline_depth(at: Vec3, home: Vec3, enemy: Vec3) -> Option<(f32, f32, f32)> {
    if home == NULLPOS || enemy == NULLPOS {
        return None;
    }
    let (dx, dz) = (enemy.x - home.x, enemy.z - home.z);
    let length = dx.hypot(dz);
    if !length.is_finite() || length < 1.0 {
        return None;
    }
    let (nx, nz) = (dx / length, dz / length);
    let depth = (at.x - (home.x + enemy.x) * 0.5) * nx + (at.z - (home.z + enemy.z) * 0.5) * nz;
    depth.is_finite().then_some((depth, nx, nz))
}

fn budget_allows(income: f32, committed: f32, local: f32, cost: f32, emergency: bool) -> bool {
    let cap = if emergency { ROUTINE_CAP + EMERGENCY_RESERVE } else { (20.0 * income).clamp(300.0, ROUTINE_CAP) };
    cost > 0.0 && committed + cost <= cap && local + cost <= LOCAL_CAP
}

impl Ai {
    /// Only a current threat to an actual factory can unlock emergency porc.
    /// A raider once seen near a mex is not a permanent emergency.
    pub(crate) fn eco_factory_emergency_at(&self, site: Vec3) -> bool {
        self.fac.factories.values().any(|f| {
            let at = f.get_pos();
            f.unit.health() > 0.0
                && distance(site, at) <= FACTORY_RADIUS
                && (self.war.get_effective_threat(at) > self.war.get_total_friendly_threat(at)
                    || self.incursions.predicted.iter().any(|p| {
                        p.seconds >= 0.0
                            && p.seconds <= EMERGENCY_HORIZON
                            && distance(p.pos, at) <= FACTORY_RADIUS
                            && p.power > self.war.get_total_friendly_power(at)
                    }))
        })
    }

    pub(crate) fn eco_porc_site_allowed(&self, def: UnitDef, site: Vec3, emergency: bool) -> bool {
        if !ground_turret(&def.name()) {
            return true;
        }
        if site.x < 0.0 || site.z < 0.0 || !site.x.is_finite() || !site.z.is_finite() {
            return false;
        }
        if emergency {
            return self.eco_factory_emergency_at(site);
        }
        if self
            .strategy
            .as_ref()
            .is_some_and(|s| s.fortify.iter().any(|g| !g.blocked && distance(site, g.pos) <= CHOKE_RADIUS))
        {
            return true;
        }
        self.graph
            .get_enemy_base()
            .and_then(|enemy| midline_depth(site, self.graph.get_start_pos(), enemy))
            .is_some_and(|(depth, _, _)| depth.abs() <= MIDLINE_BAND && def.max_weapon_range() + depth >= 160.0)
    }

    /// Count live turrets and outstanding orders once, including heavy and sea
    /// turrets that are absent from the historical `eco.porcs` list.
    fn eco_porc_commitments(&self) -> Vec<(Vec3, f32)> {
        let live: Vec<_> = self
            .cb
            .team_units()
            .into_iter()
            .filter(|u| u.health() > 0.0)
            .filter_map(|u| {
                let def = u.def()?;
                ground_turret(&def.name()).then_some((u.id(), u.pos(), def.cost(self.metal)))
            })
            .collect();
        let mut committed: Vec<_> = live.iter().map(|&(_, p, cost)| (p, cost)).collect();
        for task in self.eco.construction_tasks.iter().filter_map(|&id| self.eco.tasks.get(id)) {
            let Some(c) = task.as_construction() else { continue };
            if !ground_turret(&c.build_type.name()) || c.target.is_some_and(|u| live.iter().any(|l| l.0 == u.id())) {
                continue;
            }
            // unitCreated may not have attached the nanoframe yet.
            if live.iter().any(|&(_, p, _)| distance(p, task.position) < super::SITE_SLACK) {
                continue;
            }
            committed.push((task.position, c.build_type.cost(self.metal)));
        }
        committed
    }

    pub(crate) fn eco_porc_admissible(&self, def: UnitDef, site: Vec3, emergency: bool) -> bool {
        if !ground_turret(&def.name()) {
            return true;
        }
        if !self.eco_porc_site_allowed(def, site, emergency) {
            return false;
        }
        let committed = self.eco_porc_commitments();
        budget_allows(
            self.eco.adjusted_income,
            committed.iter().map(|p| p.1).sum(),
            committed.iter().filter(|p| distance(p.0, site) <= 2.0 * FACTORY_RADIUS).map(|p| p.1).sum(),
            def.cost(self.metal),
            emergency,
        )
    }

    /// Seed sparse Lotus coverage on the fixed midpoint line near a worker.
    /// Safe travel and nearby support must exist before we reserve any metal.
    pub(super) fn eco_defend_midline(&mut self, from: Vec3) {
        if self.eco.adjusted_income < 10.0 || self.eco.estalled {
            return;
        }
        let Some(enemy) = self.graph.get_enemy_base() else { return };
        let Some((depth, nx, nz)) = midline_depth(from, self.graph.get_start_pos(), enemy) else { return };
        if depth.abs() > 700.0 {
            return;
        }
        let Some(def) = self.defs.get("turretlaser") else { return };
        let seed = Vec3::xz(from.x - (depth + 64.0) * nx, from.z - (depth + 64.0) * nz);
        let site = self.cb.map().find_closest_build_site(def, seed, 96.0, 3, 0);
        if !self.eco_porc_admissible(def, site, false)
            || self.eco_fire_positions().iter().any(|(_, p, _)| distance(*p, site) < 700.0)
            || self.eco_shared_site_travel(def, site).is_none()
        {
            return;
        }
        let mut task = WorkerTask::construction(def, site, 0);
        let c = task.as_construction_mut().unwrap();
        c.fortification = true;
        c.frame_issued = self.frame;
        if let Some(id) = self.eco_add_construction(task, Some(List::Porc), Some(List::Porc)) {
            self.eco.fortify_tasks.insert(id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ai() -> Ai {
        use std::ffi::c_char;
        unsafe extern "C" fn name(_: i32, id: i32) -> *const c_char {
            match id {
                1 => c"turretlaser".as_ptr(),
                2 => c"turrettorp".as_ptr(),
                3 => c"turretaalaser".as_ptr(),
                _ => c"factorycloak".as_ptr(),
            }
        }
        unsafe extern "C" fn cost(_: i32, id: i32, _: i32) -> f32 {
            match id {
                1 => 90.0,
                2 => 280.0,
                _ => 1000.0,
            }
        }
        unsafe extern "C" fn units(_: i32, out: *mut i32, max: i32) -> i32 {
            if !out.is_null() && max >= 3 {
                unsafe {
                    std::ptr::copy_nonoverlapping([10, 11, 12].as_ptr(), out, 3);
                }
            }
            3
        }
        unsafe extern "C" fn def(_: i32, id: i32) -> i32 {
            match id {
                10 => 1,
                11 => 2,
                12 => 3,
                _ => 4,
            }
        }
        unsafe extern "C" fn health(_: i32, _: i32) -> f32 {
            100.0
        }
        unsafe extern "C" fn pos(_: i32, id: i32, out: *mut f32) {
            let x = if id == 20 { 1000.0 } else { id as f32 * 1000.0 };
            unsafe {
                std::ptr::copy_nonoverlapping([x, 0.0, 1000.0].as_ptr(), out, 3);
            }
        }
        unsafe extern "C" fn elevation(_: i32, _: f32, _: f32) -> f32 {
            100.0
        }
        unsafe extern "C" fn size(_: i32, _: i32) -> i32 {
            4
        }
        unsafe extern "C" fn facing(_: i32, _: i32) -> i32 {
            0
        }
        let mut ai = super::super::test_ai_with(|cb| {
            cb.UnitDef_getName = Some(name);
            cb.UnitDef_getCost = Some(cost);
            cb.getTeamUnits = Some(units);
            cb.Unit_getDef = Some(def);
            cb.Unit_getHealth = Some(health);
            cb.Unit_getPos = Some(pos);
            cb.Map_getElevationAt = Some(elevation);
            cb.UnitDef_getXSize = Some(size);
            cb.UnitDef_getZSize = Some(size);
            cb.Unit_getBuildingFacing = Some(facing);
        });
        ai.eco.adjusted_income = 100.0;
        ai.fac.factories.insert(
            20,
            super::super::Factory::new(springai::Unit::from_id(ai.cb, 20).unwrap(), "factorycloak", false, 10000.0),
        );
        ai
    }

    fn threaten_factory(ai: &mut Ai) {
        ai.incursions.predicted.push(crate::military::incursions::Predicted {
            pos: Vec3::xz(1000.0, 1000.0),
            from: Vec3::xz(2000.0, 1000.0),
            power: 500.0,
            seconds: 30.0,
            tier: zk_terrain::Tier::Bot,
        });
    }

    #[test]
    fn emergency_requires_a_current_threat_and_the_final_site_near_a_factory() {
        let mut ai = ai();
        let def = UnitDef::from_id(ai.cb, 1).unwrap();
        let near = Vec3::xz(1200.0, 1000.0);
        assert!(!ai.eco_porc_admissible(def, near, true));
        threaten_factory(&mut ai);
        assert!(ai.eco_porc_admissible(def, near, true));
        assert!(!ai.eco_porc_admissible(def, near, false));
        // A 600-elmo build-site search must not relocate the emergency away.
        assert!(!ai.eco_porc_admissible(def, Vec3::xz(1600.0, 1000.0), true));
        ai.incursions.predicted[0].seconds = 61.0;
        assert!(!ai.eco_porc_admissible(def, near, true));
        ai.incursions.predicted[0].seconds = 30.0;
        ai.incursions.predicted[0].pos = Vec3::xz(3000.0, 1000.0);
        assert!(!ai.eco_porc_admissible(def, near, true));
    }

    #[test]
    fn live_sea_turrets_and_queued_orders_count_but_nanoframes_count_once() {
        let mut ai = ai();
        let lotus = UnitDef::from_id(ai.cb, 1).unwrap();
        let sum = |ai: &Ai| ai.eco_porc_commitments().iter().map(|p| p.1).sum::<f32>();
        assert_eq!(sum(&ai), 370.0); // Lotus + Urchin, excluding AA.
        let mut task = WorkerTask::construction(lotus, Vec3::xz(10000.0, 1000.0), 0);
        task.as_construction_mut().unwrap().target = springai::Unit::from_id(ai.cb, 10);
        let id = ai.eco.tasks.insert(task);
        ai.eco.construction_tasks.push(id);
        assert_eq!(sum(&ai), 370.0);
        ai.eco.tasks.get_mut(id).unwrap().as_construction_mut().unwrap().target = None;
        assert_eq!(sum(&ai), 370.0); // Before unitCreated attaches the frame.
        ai.eco.tasks.get_mut(id).unwrap().position = Vec3::xz(5000.0, 1000.0);
        assert_eq!(sum(&ai), 460.0);
    }

    #[test]
    fn shared_admission_cannot_be_bypassed_by_uncrowded_or_fortification_flags() {
        let mut ai = ai();
        let def = UnitDef::from_id(ai.cb, 1).unwrap();
        let site = Vec3::xz(1200.0, 1000.0);
        let mut task = WorkerTask::construction(def, site, 0);
        task.as_construction_mut().unwrap().fortification = true;
        assert!(ai.eco_add_construction_uncrowded(task, Some(List::Porc), Some(List::Porc)).is_none());
        assert!(ai.eco_add_simple(def, site, List::Porc, Some(List::Porc)).is_none());
        threaten_factory(&mut ai);
        let mut task = WorkerTask::construction(def, site, 0);
        task.as_construction_mut().unwrap().emergency_porc = true;
        assert!(ai.eco_add_construction(task, Some(List::Porc), Some(List::Porc)).is_some());
    }

    #[test]
    fn bisector_is_fixed_width_even_far_from_the_start_axis() {
        let a = Vec3::xz(1000.0, 1000.0);
        let b = Vec3::xz(5000.0, 1000.0);
        for z in [0.0, 1000.0, 16000.0] {
            assert_eq!(midline_depth(Vec3::xz(3000.0, z), a, b).unwrap().0, 0.0);
            assert_eq!(midline_depth(Vec3::xz(3200.0, z), a, b).unwrap().0, 200.0);
        }
        assert!(midline_depth(a, a, a).is_none());
        assert!(midline_depth(a, NULLPOS, b).is_none());
        let diagonal = midline_depth(Vec3::xz(2000.0, 2000.0), a, Vec3::xz(3000.0, 3000.0)).unwrap();
        assert_eq!(diagonal.0, 0.0);
    }

    #[test]
    fn all_planners_share_a_hard_cap_with_a_small_emergency_reserve() {
        assert!(budget_allows(100.0, 1100.0, 0.0, 100.0, false));
        assert!(!budget_allows(1000.0, 1200.0, 0.0, 90.0, false));
        assert!(budget_allows(100.0, 1200.0, 0.0, 600.0, true));
        assert!(!budget_allows(1000.0, 1800.0, 0.0, 90.0, true));
        assert!(!budget_allows(10.0, 300.0, 0.0, 90.0, false));
        assert!(!budget_allows(100.0, 600.0, 600.0, 90.0, true));
    }

    #[test]
    fn aa_and_strategic_weapons_do_not_consume_ground_porc_budget() {
        for name in ["turretlaser", "turretgauss", "turrettorp", "turretheavy", "turretantiheavy"] {
            assert!(ground_turret(name));
        }
        for name in ["turretaalaser", "turretaaflak", "staticnuke", "staticheavyarty", "staticcon"] {
            assert!(!ground_turret(name));
        }
    }
}
