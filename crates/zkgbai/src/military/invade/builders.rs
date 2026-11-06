//! Constructors are part of an invasion's cost and arrival time. Prefer a
//! usable builder on hand; otherwise ask one existing factory for a builder
//! that can reach every spot. Orders and nanoframes close the production gap.

use springai::{UnitDef, Vec3};
use zk_terrain::{Tier, paths::Paths};

use super::{CON_BUILD_POWER, MAX_CONS, Piece};
use crate::{
    Ai,
    graph::MetalSpot,
    terrain::{pos, tier_of},
    util::distance,
};

#[derive(Clone, Copy, PartialEq)]
pub(super) struct BuilderPlan {
    pub factory: i32,
    pub def: UnitDef,
}

pub(super) struct BuilderRequest {
    pub plan: BuilderPlan,
    pub region: usize,
    spots: Vec<usize>,
}

pub(super) struct Source<'a> {
    pub plan: Option<BuilderPlan>,
    pub cost: f32,
    delay: f32,
    speed: f32,
    from: Vec3,
    paths: Option<Paths<'a>>,
}

impl Source<'_> {
    pub fn arrival(&self, spots: &[&MetalSpot]) -> Option<f32> {
        let walk = spots
            .iter()
            .map(|s| {
                self.paths.as_ref().map_or_else(
                    || distance(self.from, s.position),
                    |p| p.distance_to(pos(s.position)).unwrap_or(f32::INFINITY),
                ) / self.speed
            })
            .fold(0.0f32, f32::max);
        let eta = self.delay + walk;
        eta.is_finite().then_some(eta)
    }
}

fn constructor(def: UnitDef) -> bool {
    def.speed() > 0.0 && def.build_options().iter().any(|d| d.name() == "staticmex")
}

impl Ai {
    fn invade_builder_source(
        &self,
        def: UnitDef,
        from: Vec3,
        delay: f32,
        cost: f32,
        plan: Option<BuilderPlan>,
    ) -> Option<Source<'_>> {
        if def.speed() <= 0.0 {
            return None;
        }
        let paths = if def.is_able_to_fly() {
            None
        } else {
            Some(Paths::new(self.terrain()?.tier(tier_of(def)?)?, pos(from), |_| false))
        };
        Some(Source { plan, cost, delay, speed: def.speed(), from, paths })
    }

    pub(super) fn invade_builder_tier(&self) -> Option<Tier> {
        self.fac
            .factories
            .values()
            .filter_map(|f| f.unit.def())
            .flat_map(|d| d.build_options())
            .filter(|&d| constructor(d))
            .find_map(tier_of)
            // Air-only openings still need region candidates, including water.
            .or_else(|| self.terrain()?.tier(Tier::Hover).map(|_| Tier::Hover))
    }

    pub(super) fn invade_builder_sources(&self) -> Vec<Source<'_>> {
        let mut out: Vec<_> = self
            .invade_constructors()
            .filter_map(|w| self.invade_builder_source(w.unit.def()?, w.get_pos(), 0.0, 0.0, None))
            .take(MAX_CONS)
            .collect();
        // A constructor already rising is a committed resource, not another
        // order. Its remaining build delays arrival but is not charged twice.
        for unit in self.cb.team_units() {
            if unit.health() <= 0.0 || !unit.is_being_built() {
                continue;
            }
            let Some(def) = unit.def().filter(|&d| constructor(d)) else { continue };
            #[allow(clippy::manual_clamp, reason = "Preserve the finite fallback for NaN income")]
            let delay = def.cost(self.metal) * (1.0 - unit.build_progress()).clamp(0.0, 1.0)
                / self.eco.effective_income.max(1.0).min(CON_BUILD_POWER);
            out.extend(self.invade_builder_source(def, unit.pos(), delay, 0.0, None));
        }
        for f in self.fac.factories.values() {
            if f.unit.health() <= 0.0 || f.unit.is_being_built() {
                continue;
            }
            let Some(factory) = f.unit.def() else { continue };
            let rate = factory.build_speed().min(self.eco.effective_income).max(1.0);
            let queued = f
                .unit
                .current_commands()
                .iter()
                .filter_map(|c| (c.id() < 0).then(|| UnitDef::from_id(self.cb, -c.id())).flatten())
                .map(|d| d.cost(self.metal))
                .sum::<f32>();
            for def in factory.build_options().into_iter().filter(|&d| constructor(d)) {
                let cost = def.cost(self.metal);
                out.extend(self.invade_builder_source(
                    def,
                    f.get_pos(),
                    (queued + cost) / rate,
                    cost,
                    Some(BuilderPlan { factory: f.id, def }),
                ));
            }
        }
        out
    }

    pub(super) fn invade_request_builder(&mut self, pieces: &[Piece]) {
        let frame = self.frame;
        let factories = &self.fac.factories;
        self.invade.builder_orders.retain(|id, (def, since)| {
            factories.get(id).is_some_and(|f| {
                f.unit.health() > 0.0
                    && (frame - *since < 90 || f.unit.current_commands().iter().any(|c| c.id() == -def.id()))
            })
        });
        let next = pieces
            .iter()
            .filter(|p| {
                self.invade.cur.as_ref().is_some_and(|t| t.region == p.region)
                    || self.invade.captures.iter().any(|c| c.region == p.region)
            })
            .filter(|p| p.builder.is_some() && p.best.is_some_and(|b| b.2 > 0.0))
            .max_by(|a, b| a.best.unwrap().2.total_cmp(&b.best.unwrap().2));
        let next = next.map(|p| BuilderRequest { plan: p.builder.unwrap(), region: p.region, spots: p.spots.clone() });
        if let Some(b) = &next
            && self.invade.builder.as_ref().is_none_or(|old| old.plan != b.plan || old.region != b.region)
        {
            self.log(&format!(
                "invade: requisition {} from factory {} for piece {}",
                self.defs.name(b.plan.def),
                b.plan.factory,
                b.region
            ));
        }
        self.invade.builder = next;
    }

    /// Called only at an idle factory, before its ordinary combat pick.
    pub(crate) fn invade_constructor_order(&self, factory: i32) -> Option<UnitDef> {
        if self.dev_off("invade") {
            return None;
        }
        let request = self.invade.builder.as_ref()?;
        let plan = request.plan;
        if plan.factory != factory
            || self.invade.builder_orders.iter().any(|(id, (def, frame))| {
                self.fac.factories.get(id).is_some_and(|f| {
                    f.unit.health() > 0.0
                        && (self.frame - *frame < 90 || f.unit.current_commands().iter().any(|c| c.id() == -def.id()))
                })
            })
        {
            return None;
        }
        let spots: Vec<_> = request
            .spots
            .iter()
            .filter_map(|&i| self.graph.metal_spots.get(i))
            .filter(|s| !s.owned && !s.allied)
            .collect();
        if spots.is_empty() {
            return None;
        }
        // Recheck between the ten-second planning passes: a constructor can
        // have finished, or another factory can already have started one.
        if self.invade_builder_sources().iter().any(|s| s.plan.is_none() && s.arrival(&spots).is_some()) {
            return None;
        }
        let fac = self.fac.factories.get(&factory)?;
        (fac.unit.health() > 0.0 && fac.unit.def()?.build_options().contains(&plan.def)).then_some(plan.def)
    }

    pub(crate) fn invade_constructor_ordered(&mut self, factory: i32, def: UnitDef) {
        if self.invade.builder.as_ref().is_some_and(|b| b.plan.def == def) {
            self.invade.builder_orders.insert(factory, (def, self.frame));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use springai::{Callback, Unit};
    use std::cell::Cell;

    thread_local! {
        static RISING: Cell<bool> = const { Cell::new(false) };
        static BUSY_PARENT: Cell<bool> = const { Cell::new(false) };
    }

    unsafe fn ids(values: &[i32], out: *mut i32, max: i32) -> i32 {
        if !out.is_null() {
            unsafe { std::ptr::copy_nonoverlapping(values.as_ptr(), out, values.len().min(max.max(0) as usize)) };
        }
        values.len() as i32
    }

    fn ai() -> Ai {
        unsafe extern "C" fn defs(_: i32, out: *mut i32, max: i32) -> i32 {
            unsafe { ids(&[1, 2, 3, 4, 10, 11, 12], out, max) }
        }
        unsafe extern "C" fn options(_: i32, id: i32, out: *mut i32, max: i32) -> i32 {
            let values: &[i32] = match id {
                1..=3 => &[4],
                10 | 12 => &[2],
                11 => &[1],
                _ => &[],
            };
            unsafe { ids(values, out, max) }
        }
        unsafe extern "C" fn name(_: i32, id: i32) -> *const std::ffi::c_char {
            match id {
                1 => c"cloakcon",
                2 => c"hovercon",
                3 => c"gunshipcon",
                4 => c"staticmex",
                10 => c"factoryhover",
                11 => c"factorycloak",
                _ => c"platehover",
            }
            .as_ptr()
        }
        unsafe extern "C" fn speed(_: i32, id: i32) -> f32 {
            if id <= 3 { 60.0 } else { 0.0 }
        }
        unsafe extern "C" fn bp(_: i32, _: i32) -> f32 {
            10.0
        }
        unsafe extern "C" fn cost(_: i32, _: i32, _: i32) -> f32 {
            150.0
        }
        unsafe extern "C" fn commands(_: i32, id: i32) -> i32 {
            i32::from(id == 10 && BUSY_PARENT.with(Cell::get))
        }
        unsafe extern "C" fn command(_: i32, _: i32, _: i32) -> i32 {
            -5
        }
        unsafe extern "C" fn built(_: i32, id: i32) -> bool {
            id == 2 && RISING.with(Cell::get)
        }
        unsafe extern "C" fn progress(_: i32, _: i32) -> f32 {
            0.5
        }
        unsafe extern "C" fn units(_: i32, out: *mut i32, max: i32) -> i32 {
            unsafe { ids(if RISING.with(Cell::get) { &[2] } else { &[] }, out, max) }
        }
        RISING.with(|v| v.set(false));
        BUSY_PARENT.with(|v| v.set(false));
        let mut table = None;
        let mut ai = super::super::tests::coastal_ai_with(1, |t| table = Some(*t));
        let mut table = table.unwrap();
        table.getUnitDefs = Some(defs);
        table.UnitDef_getBuildOptions = Some(options);
        table.UnitDef_getName = Some(name);
        table.UnitDef_getSpeed = Some(speed);
        table.UnitDef_getBuildSpeed = Some(bp);
        table.UnitDef_getCost = Some(cost);
        table.Unit_getCurrentCommands = Some(commands);
        table.Unit_CurrentCommand_getId = Some(command);
        table.Unit_isBeingBuilt = Some(built);
        table.Unit_getBuildProgress = Some(progress);
        table.getTeamUnits = Some(units);
        ai.cb = unsafe { Callback::from_raw(0, Box::leak(Box::new(table))) };
        ai.defs = crate::defs::Defs::new(ai.cb);
        ai.eco.effective_income = 10.0;
        ai.set_feature_off("invade", false);
        ai.frame = super::super::FROM;
        ai.offset = 0;
        ai.graph.metal_spots.push(MetalSpot::new(0, 896.0, 0.0, 256.0, 4.0));
        for (id, name) in [(10, "factoryhover"), (11, "factorycloak"), (12, "factoryhover")] {
            ai.fac
                .factories
                .insert(id, crate::economy::Factory::new(Unit::from_id(ai.cb, id).unwrap(), name, false, 1000.0));
        }
        // Evaluate the coast from a hover army's region map.
        ai.invade.tier = Some(Tier::Hover);
        ai
    }

    #[test]
    fn unreachable_expansion_buys_one_compatible_constructor_and_prices_its_delay() {
        let mut ai = ai();
        let pieces = ai.invade_weigh(ai.terrain().unwrap().tier(Tier::Hover).unwrap());
        assert_eq!(pieces.len(), 1);
        let p = &pieces[0];
        let plan = p.builder.expect("hover builder required across the water");
        assert_eq!(plan.def.id(), 2);
        assert_eq!(plan.factory, 10, "factory and plate ties are stable");
        assert_eq!(p.weigh.build_metal, super::super::MEX_COST + 150.0);
        assert!(p.weigh.t_con >= 15.0 + 768.0 / 60.0);
        ai.invade_choose(&pieces);
        ai.invade_request_builder(&pieces);
        assert_eq!(ai.invade_constructor_order(10).unwrap().id(), 2);
        assert!(ai.invade_constructor_order(11).is_none(), "land factory cannot supply this job");
        assert!(ai.invade_constructor_order(12).is_none(), "the second queue must not duplicate it");
        ai.invade_constructor_ordered(10, plan.def);
        assert!(ai.invade_constructor_order(10).is_none(), "accepted order bridges the UnitCreated gap");
        ai.frame += 91;
        assert!(ai.invade_constructor_order(10).is_some(), "canceled orders can retry");
        RISING.with(|v| v.set(true));
        assert!(ai.invade_constructor_order(10).is_none(), "an unfinished constructor satisfies the request");
        let pieces = ai.invade_weigh(ai.terrain().unwrap().tier(Tier::Hover).unwrap());
        assert!(pieces[0].builder.is_none());
        assert_eq!(pieces[0].weigh.build_metal, super::super::MEX_COST);
    }

    #[test]
    fn completed_or_canceled_jobs_stop_requesting_builders() {
        let mut ai = ai();
        ai.invade_update(ai.frame);
        assert!(ai.invade_constructor_order(10).is_some());
        ai.graph.metal_spots[0].owned = true;
        assert!(ai.invade_constructor_order(10).is_none());
        ai.frame += super::super::PERIOD;
        ai.invade_update(ai.frame);
        assert!(ai.invade.builder.is_none());
        ai.graph.metal_spots[0].owned = false;
        ai.frame += super::super::PERIOD;
        ai.invade_update(ai.frame);
        assert!(ai.invade.builder.is_some());
        ai.set_feature_off("invade", true);
        assert!(ai.invade_constructor_order(10).is_none());
    }

    #[test]
    fn an_idle_plate_takes_the_constructor_while_the_parent_keeps_producing() {
        let mut ai = ai();
        BUSY_PARENT.with(|b| b.set(true));
        ai.invade_update(ai.frame);
        assert_eq!(ai.invade.builder.as_ref().unwrap().plan.factory, 12);
        assert!(ai.invade_constructor_order(10).is_none());
        assert_eq!(ai.invade_constructor_order(12).unwrap().id(), 2);
    }

    #[test]
    fn a_constructor_on_hand_and_no_capable_factory_both_avoid_new_orders() {
        let mut ai = ai();
        ai.eco.workers.insert(2, crate::economy::Worker::new(Unit::from_id(ai.cb, 2).unwrap(), ai.team_id));
        ai.invade_update(ai.frame);
        assert!(ai.invade.builder.is_none());
        assert_eq!(ai.invade.captures.len(), 1);
        ai.eco.workers.remove(&2);
        ai.fac.factories.retain(|&id, _| id == 11);
        let pieces = ai.invade_weigh(ai.terrain().unwrap().tier(Tier::Hover).unwrap());
        assert!(pieces.is_empty(), "no promise of a builder that available factories cannot supply");
    }
}
