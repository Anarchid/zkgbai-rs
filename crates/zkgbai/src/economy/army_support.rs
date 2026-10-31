//! Above 2000 metal per square kiloelmo, buy one Aspis OR Iris per 2000 metal
//! of completed ground army. Factories and dedicated static morph jobs
//! share the allowance, including production already in flight. Full Lance
//! batteries require one Iris each even below the general density threshold.

use std::collections::BTreeMap;

use springai::{NO_TIMEOUT, Unit, UnitDef, Vec3};

use super::{Factory, List, TaskId};
use crate::Ai;
use crate::defs::cmd;
use crate::util::distance;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Support {
    Shield,
    Cloak,
}

impl Support {
    fn mobile(self) -> &'static str {
        match self {
            Self::Shield => "shieldshield",
            Self::Cloak => "cloakjammer",
        }
    }

    fn building(self) -> &'static str {
        match self {
            Self::Shield => "staticshield",
            Self::Cloak => "staticjammer",
        }
    }

    fn from_mobile(name: &str) -> Option<Self> {
        match name {
            "shieldshield" => Some(Self::Shield),
            "cloakjammer" => Some(Self::Cloak),
            _ => None,
        }
    }

    fn index(self) -> usize {
        match self {
            Self::Shield => 0,
            Self::Cloak => 1,
        }
    }
}

#[derive(Default)]
pub(super) struct ArmySupport {
    tasks: BTreeMap<TaskId, Support>,
    /// Only statics built for this purpose; ordinary defenses never morph.
    morphs: BTreeMap<i32, (Unit, Support)>,
    /// Bridge command acceptance to UnitCreated, before a nanoframe counts.
    orders: BTreeMap<i32, (Support, i32)>,
}

impl ArmySupport {
    pub(super) fn forget_task(&mut self, id: TaskId) {
        self.tasks.remove(&id);
    }

    fn completed(&mut self, task: TaskId, unit: Unit, name: &str) -> bool {
        if let Some(&support) = self.tasks.get(&task)
            && name == support.building()
        {
            self.tasks.remove(&task);
            self.morphs.insert(unit.id(), (unit, support));
            return true;
        }
        false
    }

    fn created(&mut self, name: &str, builder: Option<i32>) {
        if let Some(support) = Support::from_mobile(name)
            && let Some(builder) = builder
            && self.orders.get(&builder).is_some_and(|&(pending, _)| pending == support)
        {
            self.orders.remove(&builder);
        }
    }

    fn pending(&self) -> [usize; 2] {
        let mut count = [0; 2];
        for support in self
            .tasks
            .values()
            .copied()
            .chain(self.morphs.values().map(|&(_, s)| s))
            .chain(self.orders.values().map(|&(s, _)| s))
        {
            count[support.index()] += 1;
        }
        count
    }
}

fn allowance(density: f32, army: f32) -> usize {
    if !density.is_finite() || density <= 2000.0 || !army.is_finite() || army <= 0.0 {
        return 0;
    }
    (army / 2000.0).floor() as usize
}

fn preferred(count: [usize; 2]) -> [Support; 2] {
    if count[0] <= count[1] { [Support::Shield, Support::Cloak] } else { [Support::Cloak, Support::Shield] }
}

/// Battery cloaks take precedence and cannot be substituted with shields.
fn demand(density: f32, army: f32, count: [usize; 2], cloaks_required: usize, cloak_limit: usize) -> Vec<Support> {
    if count[Support::Cloak.index()] < cloaks_required {
        vec![Support::Cloak]
    } else if count.iter().sum::<usize>() < allowance(density, army) {
        preferred(count)
            .into_iter()
            .filter(|s| *s != Support::Cloak || count[Support::Cloak.index()] < cloak_limit.max(cloaks_required))
            .collect()
    } else {
        Vec::new()
    }
}

fn request_morph(unit: Unit, mobile: UnitDef) {
    if !unit.is_being_built() && unit.rules_param_float("morphing", 0.0) == 0.0 {
        unit.execute_custom_command(cmd::MORPH, &[mobile.id() as f32], 0, NO_TIMEOUT);
    }
}

impl Ai {
    /// Completed combat metal excludes the support itself, constructors,
    /// aircraft and buildings: buying support cannot unlock more support.
    fn army_support_demand(&self) -> Vec<Support> {
        let density = self.projected_metal_density();
        let mut army = 0.0;
        let mut other_cloaks = 0;
        let mut count = self.eco.army_support.pending();
        for unit in self.cb.team_units() {
            if unit.health() <= 0.0 || unit.team() != self.team_id {
                continue;
            }
            let Some(def) = unit.def() else { continue };
            if let Some(support) = Support::from_mobile(self.defs.name(def)) {
                count[support.index()] += 1;
                // A cloaker committed to another taskforce, or a battery
                // reduced below four guns, cannot cover a new full battery.
                // Ordinary support followers can be recalled, so exclude them.
                if support == Support::Cloak
                    && !self.misc_handler.supports.contains(&unit.id())
                    && self
                        .fighters
                        .get(&unit.id())
                        .and_then(|f| f.squad)
                        .and_then(|sid| self.squads.get(sid))
                        .is_some_and(|s| {
                            s.is_taskforce() && (s.shape.doctrine != "lance" || s.cores(&self.fighters) < 4)
                        })
                {
                    other_cloaks += 1;
                }
            } else if !unit.is_being_built()
                && def.speed() > 0.0
                && def.build_speed() <= 0.0
                && !def.is_able_to_fly()
                && def.is_able_to_attack()
            {
                army += def.cost(self.metal);
            }
        }
        let batteries = self
            .squad_handler
            .taskforces
            .iter()
            .filter_map(|&sid| self.squads.get(sid))
            .filter(|s| s.shape.doctrine == "lance" && s.cores(&self.fighters) >= 4)
            .count();
        let cloak_limit = self.support_ground_squads().len().max(batteries + other_cloaks);
        demand(density, army, count, batteries + other_cloaks, cloak_limit)
    }

    fn army_support_can_build(&self, factory: &Factory, support: Support) -> bool {
        factory.unit.health() > 0.0
            && factory.unit.team() == self.team_id
            && !factory.unit.is_being_built()
            && self
                .defs
                .get(support.mobile())
                .is_some_and(|def| factory.unit.def().is_some_and(|f| f.build_options().contains(&def)))
    }

    pub(super) fn army_support_factory(&self, factory: &Factory) -> Option<&'static str> {
        self.army_support_demand().into_iter().find(|&s| self.army_support_can_build(factory, s)).map(Support::mobile)
    }

    pub(super) fn army_support_ordered(&mut self, factory: i32, def: UnitDef) {
        if let Some(support) = Support::from_mobile(self.defs.name(def)) {
            self.eco.army_support.orders.insert(factory, (support, self.frame));
        }
    }

    pub(super) fn army_support_created(&mut self, unit: Unit, builder: Option<Unit>) {
        self.eco.army_support.created(self.defs.name_of(unit), builder.map(|u| u.id()));
    }

    pub(super) fn army_support_finished(&mut self, task: TaskId, unit: Unit) {
        if self.eco.army_support.completed(task, unit, self.defs.name_of(unit)) {
            self.army_support_morphs();
        }
    }

    fn army_support_morphs(&mut self) {
        self.eco.army_support.morphs.retain(|_, (unit, support)| {
            unit.health() > 0.0 && unit.team() == self.team_id && self.defs.name_of(*unit) == support.building()
        });
        for &(unit, support) in self.eco.army_support.morphs.values() {
            if let Some(def) = self.defs.get(support.mobile()) {
                // Rejected or canceled morphs retry without opening another
                // allowance slot. Already morphing statics retain their slot.
                request_morph(unit, def);
            }
        }
    }

    pub(super) fn eco_plan_army_support(&mut self) {
        if self.test.is_some() {
            return;
        }
        self.army_support_morphs();
        self.eco.army_support.orders.retain(|id, (support, frame)| {
            self.fac.factories.get(id).is_some_and(|f| {
                f.unit.health() > 0.0
                    && f.unit.team() == self.team_id
                    && (self.frame - *frame < 90
                        || self
                            .defs
                            .get(support.mobile())
                            .is_some_and(|d| f.unit.current_commands().iter().any(|c| c.id() == -d.id())))
            })
        });
        let wanted = self.army_support_demand();
        if wanted.is_empty() {
            return;
        }
        // Prefer existing production capacity. No new factory is needed
        // just to gain access to an Aspis or Iris.
        if self.fac.factories.values().any(|f| wanted.iter().any(|&s| self.army_support_can_build(f, s)))
            || !self.eco.army_support.tasks.is_empty()
        {
            return;
        }
        for support in wanted {
            let (Some(building), Some(_)) = (self.defs.get(support.building()), self.defs.get(support.mobile())) else {
                continue;
            };
            let workers: Vec<_> = self
                .eco
                .workers
                .values()
                .filter(|w| w.unit.health() > 0.0 && w.unit.team() == self.team_id)
                .filter_map(|w| {
                    let def = w.unit.def()?;
                    def.build_options().contains(&building).then(|| (def, w.get_pos()))
                })
                .collect();
            let anchors: Vec<_> = self
                .fac
                .factories
                .values()
                .filter(|f| f.unit.health() > 0.0 && f.unit.team() == self.team_id)
                .map(Factory::get_pos)
                .collect();
            for anchor in anchors {
                for (x, z) in [(1.0, 0.0), (-1.0, 0.0), (0.0, 1.0), (0.0, -1.0)] {
                    let at = Vec3::xz(anchor.x + x * 250.0, anchor.z + z * 250.0);
                    let site = self.cb.map().find_closest_build_site(building, at, 200.0, 3, 0);
                    if site.x < 0.0
                        || distance(site, anchor) > 600.0
                        || !self.eco_build_check(building, site, 0)
                        || self.war.get_porc_threat(site) > 0.0
                        || !workers.iter().any(|&(def, pos)| self.terrain_reaches(def, pos, site, def.build_distance()))
                    {
                        continue;
                    }
                    if let Some(task) = self.eco_add_simple(building, site, List::Construction, None) {
                        self.eco.army_support.tasks.insert(task, support);
                        self.log(&format!("army support: {} to morph into {}", support.building(), support.mobile()));
                        return;
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use springai::{Callback, ffi};

    fn unit(id: i32) -> Unit {
        let cb = unsafe { Callback::from_raw(0, Box::leak(Box::new(ffi::SSkirmishAICallback::default()))) };
        Unit::from_id(cb, id).unwrap()
    }

    #[test]
    fn morph_command_waits_for_completion_and_retries_after_cancellation() {
        use std::cell::{Cell, RefCell};
        thread_local! {
            static BUILDING: Cell<bool> = const { Cell::new(true) };
            static MORPHING: Cell<f32> = const { Cell::new(0.0) };
            static ORDERS: RefCell<Vec<(i32, i32, Vec<f32>)>> = const { RefCell::new(Vec::new()) };
        }
        unsafe extern "C" fn building(_: i32, _: i32) -> bool {
            BUILDING.with(Cell::get)
        }
        unsafe extern "C" fn rules(_: i32, _: i32, _: *const std::ffi::c_char, _: f32) -> f32 {
            MORPHING.with(Cell::get)
        }
        unsafe extern "C" fn command(_: i32, _: i32, _: i32, topic: i32, data: *mut std::ffi::c_void) -> i32 {
            assert_eq!(topic as u32, ffi::CommandTopic::COMMAND_UNIT_CUSTOM);
            let c = unsafe { &*(data as *const ffi::SCustomUnitCommand) };
            let params = unsafe { std::slice::from_raw_parts(c.params, c.params_size as usize) }.to_vec();
            ORDERS.with(|o| o.borrow_mut().push((c.unitId, c.cmdId, params)));
            0
        }
        let table = ffi::SSkirmishAICallback {
            Unit_isBeingBuilt: Some(building),
            Unit_getRulesParamFloat: Some(rules),
            Engine_handleCommand: Some(command),
            ..Default::default()
        };
        let cb = unsafe { Callback::from_raw(0, Box::leak(Box::new(table))) };
        let unit = Unit::from_id(cb, 20).unwrap();
        let mobile = UnitDef::from_id(cb, 7).unwrap();
        request_morph(unit, mobile);
        ORDERS.with(|o| assert!(o.borrow().is_empty()));
        BUILDING.with(|b| b.set(false));
        request_morph(unit, mobile);
        ORDERS.with(|o| assert_eq!(*o.borrow(), [(20, cmd::MORPH, vec![7.0])]));
        MORPHING.with(|m| m.set(1.0));
        request_morph(unit, mobile);
        ORDERS.with(|o| assert_eq!(o.borrow().len(), 1));
        MORPHING.with(|m| m.set(0.0));
        request_morph(unit, mobile);
        ORDERS.with(|o| assert_eq!(o.borrow().len(), 2));
    }

    #[test]
    fn batteries_require_cloak_even_below_general_support_density() {
        assert_eq!(demand(1000.0, 4000.0, [0, 0], 0, 2), []);
        assert_eq!(demand(1000.0, 4000.0, [0, 0], 1, 2), [Support::Cloak]);
        assert_eq!(demand(3000.0, 8000.0, [4, 0], 1, 2), [Support::Cloak]);
        // Pending orders and morphs count just like existing cloakers.
        assert_eq!(demand(1000.0, 8000.0, [0, 1], 1, 2), []);
        assert_eq!(demand(1000.0, 8000.0, [0, 1], 2, 2), [Support::Cloak]);
        assert_eq!(demand(3000.0, 8000.0, [0, 1], 1, 2), [Support::Shield, Support::Cloak]);
    }

    #[test]
    fn cloaks_stop_at_squad_count_but_shields_keep_stacking() {
        assert_eq!(demand(3000.0, 20000.0, [3, 2], 0, 2), [Support::Shield]);
        assert_eq!(demand(3000.0, 20000.0, [3, 2], 0, 3), [Support::Cloak, Support::Shield]);
        assert_eq!(demand(3000.0, 20000.0, [0, 0], 0, 0), [Support::Shield]);
        // Explicit battery requirements survive a temporarily empty squad census.
        assert_eq!(demand(1000.0, 2000.0, [0, 0], 1, 0), [Support::Cloak]);
    }

    #[test]
    fn queued_and_morphing_cloakers_fill_the_cap_before_they_finish() {
        let mut plan = ArmySupport::default();
        plan.tasks.insert(1, Support::Cloak);
        plan.orders.insert(10, (Support::Cloak, 0));
        assert_eq!(demand(3000.0, 20000.0, plan.pending(), 0, 2), [Support::Shield]);
        plan.completed(1, unit(20), "staticjammer");
        assert_eq!(demand(3000.0, 20000.0, plan.pending(), 0, 2), [Support::Shield]);
        plan.orders.remove(&10);
        assert!(demand(3000.0, 20000.0, plan.pending(), 0, 2).contains(&Support::Cloak));
    }

    #[test]
    fn strict_density_and_whole_army_allowances() {
        assert_eq!(allowance(1999.0, 8000.0), 0);
        assert_eq!(allowance(2000.0, 8000.0), 0);
        assert_eq!(allowance(2000.1, 1999.0), 0);
        assert_eq!(allowance(2000.1, 2000.0), 1);
        assert_eq!(allowance(2000.1, 3999.0), 1);
        assert_eq!(allowance(2000.1, 4000.0), 2);
        assert_eq!(allowance(f32::NAN, 8000.0), 0);
        assert_eq!(allowance(3000.0, f32::INFINITY), 0);
    }

    #[test]
    fn both_routes_share_slots_and_cancellation_releases_them() {
        let mut plan = ArmySupport::default();
        plan.tasks.insert(1, Support::Shield);
        plan.orders.insert(10, (Support::Cloak, 0));
        assert_eq!(plan.pending(), [1, 1]);
        // Unrelated constructors and the wrong factory cannot clear an order.
        plan.created("cloakcon", Some(10));
        plan.created("cloakjammer", Some(11));
        assert_eq!(plan.pending(), [1, 1]);
        plan.created("cloakjammer", Some(10));
        assert_eq!(plan.pending(), [1, 0]); // Live nanoframe now supplies the other slot.
        plan.forget_task(1);
        assert_eq!(plan.pending(), [0, 0]);
    }

    #[test]
    fn only_dedicated_statics_morph_and_completion_keeps_one_slot() {
        let mut plan = ArmySupport::default();
        let static_unit = unit(20);
        plan.tasks.insert(1, Support::Shield);
        plan.completed(2, static_unit, "staticshield");
        plan.completed(1, static_unit, "staticjammer");
        assert!(plan.morphs.is_empty());
        plan.completed(1, static_unit, "staticshield");
        plan.forget_task(1); // Normal completed-job cleanup must not release the morph slot.
        plan.completed(1, static_unit, "staticshield");
        assert_eq!(plan.pending(), [1, 0]);
        assert_eq!(plan.morphs.len(), 1);
        assert_eq!(preferred(plan.pending())[0], Support::Cloak);
    }
}
