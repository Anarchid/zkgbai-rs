//! Exercise factory planning, failed placement retries, and rush production holds.
use super::*;
use crate::economy::{Factory, Worker};
use springai::ffi;
use std::cell::{Cell, RefCell};
use std::ffi::{c_char, c_void};

thread_local! {
    static DEFS_READY: Cell<bool> = const { Cell::new(false) };
    static BUILD_ORDERS: RefCell<Vec<(i32, i32)>> = const { RefCell::new(Vec::new()) };
    static SITE_AVAILABLE: Cell<bool> = const { Cell::new(true) };
}

fn expansion_ai() -> Ai {
    unsafe extern "C" fn defs(_: i32, out: *mut i32, max: i32) -> i32 {
        if !DEFS_READY.get() {
            return 0;
        }
        if !out.is_null() && max >= 9 {
            unsafe { std::ptr::copy_nonoverlapping([1, 2, 3, 4, 5, 6, 7, 8, 9].as_ptr(), out, 9) };
        }
        9
    }
    unsafe extern "C" fn name(_: i32, id: i32) -> *const c_char {
        match id {
            1 => c"factorycloak".as_ptr(),
            2 => c"factoryplane".as_ptr(),
            3 => c"platecloak".as_ptr(),
            4 => c"aircon".as_ptr(),
            5 => c"amphassault".as_ptr(),
            6 => c"factoryamph".as_ptr(),
            7 => c"cloakcon".as_ptr(),
            8 => c"hovercon".as_ptr(),
            _ => c"factoryhover".as_ptr(),
        }
    }
    unsafe extern "C" fn def(_: i32, id: i32) -> i32 {
        match id {
            10..=12 => 1,
            40 => 9,
            20 => 4,
            100.. => 2,
            _ => 3,
        }
    }
    unsafe extern "C" fn pos(_: i32, id: i32, out: *mut f32) {
        let p = if id == 20 { [1000.0, 20.0, 1000.0] } else { [0.0, 20.0, 0.0] };
        unsafe { std::ptr::copy_nonoverlapping(p.as_ptr(), out, 3) };
    }
    unsafe extern "C" fn elevation(_: i32, _: f32, _: f32) -> f32 {
        if SITE_AVAILABLE.get() { 20.0 } else { 0.0 }
    }
    unsafe extern "C" fn size(_: i32) -> i32 {
        512
    }
    unsafe extern "C" fn site(_: i32, _: i32, _: *mut f32, _: f32, _: i32, _: i32, out: *mut f32) {
        let p = if SITE_AVAILABLE.get() { [1000.0, 20.0, 1000.0] } else { [-1.0, 0.0, -1.0] };
        unsafe { std::ptr::copy_nonoverlapping(p.as_ptr(), out, 3) };
    }
    unsafe extern "C" fn possible(_: i32, _: i32, _: *mut f32, _: i32) -> bool {
        SITE_AVAILABLE.get()
    }
    unsafe extern "C" fn scalar(_: i32, _: i32) -> f32 {
        100.0
    }
    unsafe extern "C" fn zero(_: i32, _: i32) -> i32 {
        0
    }
    unsafe extern "C" fn team(_: i32, _: i32) -> i32 {
        2
    }
    unsafe extern "C" fn cost(_: i32, _: i32, _: i32) -> f32 {
        700.0
    }
    unsafe extern "C" fn no(_: i32, _: i32) -> bool {
        false
    }
    unsafe extern "C" fn params(_: i32, _: i32, _: *mut *const c_char, _: *mut *const c_char) -> i32 {
        0
    }
    unsafe extern "C" fn rules(_: i32, _: i32, _: *const c_char, default: f32) -> f32 {
        default
    }
    unsafe extern "C" fn friends(_: i32, _: *mut f32, _: f32, _: bool, _: *mut i32, _: i32) -> i32 {
        0
    }
    unsafe extern "C" fn commands(_: i32, id: i32) -> i32 {
        if id == 11 { 1 } else { 0 }
    }
    unsafe extern "C" fn building(_: i32, id: i32) -> bool {
        id == 12
    }
    unsafe extern "C" fn units(_: i32, _: *mut i32, _: i32) -> i32 {
        0
    }
    unsafe extern "C" fn speed(_: i32, id: i32) -> f32 {
        if matches!(id, 4 | 7 | 8) { 60.0 } else { 0.0 }
    }
    unsafe extern "C" fn options(_: i32, id: i32, out: *mut i32, max: i32) -> i32 {
        let option = match id {
            1 => 7,
            9 => 8,
            7 | 8 => 1,
            _ => return 0,
        };
        if !out.is_null() && max > 0 {
            unsafe { out.write(option) };
        }
        1
    }
    unsafe extern "C" fn command(_: i32, _: i32, _: i32, topic: i32, data: *mut c_void) -> i32 {
        if topic == ffi::CommandTopic::COMMAND_UNIT_BUILD as i32 {
            let c = unsafe { &*data.cast::<ffi::SBuildUnitCommand>() };
            BUILD_ORDERS.with_borrow_mut(|o| o.push((c.unitId, c.toBuildUnitDefId)));
        }
        0
    }
    DEFS_READY.set(false);
    SITE_AVAILABLE.set(true);
    BUILD_ORDERS.with_borrow_mut(Vec::clear);
    let mut ai = crate::economy::test_ai_with(|t| {
        t.Unit_getCurrentCommands = Some(commands);
        t.Unit_isBeingBuilt = Some(building);
        t.getFriendlyUnits = Some(units);
        t.getTeamUnits = Some(units);
        t.UnitDef_getSpeed = Some(speed);
        t.UnitDef_getBuildOptions = Some(options);
        t.Engine_handleCommand = Some(command);
        t.Unit_getTeam = Some(team);
        t.Unit_getHealth = Some(scalar);
        t.Unit_getWeapons = Some(zero);
        t.Unit_getBuildingFacing = Some(zero);
        t.Unit_getRulesParamFloat = Some(rules);
        t.UnitDef_getWeaponMounts = Some(zero);
        t.UnitDef_getBuildSpeed = Some(scalar);
        t.UnitDef_getBuildTime = Some(scalar);
        t.UnitDef_getCost = Some(cost);
        t.UnitDef_getCustomParams = Some(params);
        t.UnitDef_isAbleToAttack = Some(no);
        t.UnitDef_isMoveDataAvailable = Some(no);
        t.UnitDef_isAbleToFly = Some(no);
        t.UnitDef_getMaxWaterDepth = Some(scalar);
        t.UnitDef_getXSize = Some(zero);
        t.UnitDef_getZSize = Some(zero);
        t.getFriendlyUnitsIn = Some(friends);
        t.getFeaturesIn = Some(friends);
        t.getFeatures = Some(units);
        t.getUnitDefs = Some(defs);
        t.UnitDef_getName = Some(name);
        t.Unit_getDef = Some(def);
        t.Unit_getPos = Some(pos);
        t.Map_getWidth = Some(size);
        t.Map_getHeight = Some(size);
        t.Map_getElevationAt = Some(elevation);
        t.Map_findClosestBuildSite = Some(site);
        t.Map_isPossibleToBuildAt = Some(possible);
    });
    DEFS_READY.set(true);
    ai.defs = crate::defs::Defs::new(ai.cb);
    ai.test = None; // The generic fixture returns "off" even for the staged-test option.
    ai.frame = 18000;
    ai.offset = 0;
    ai.set_feature_off("densgate", true);
    ai.eco.static_income = 60.0;
    ai.eco.new_fac_at = Some(ai.frame);
    ai.eco.initial_fac_list = BTreeSet::from(["factorycloak".into()]);
    let unit = Unit::from_id(ai.cb, 10).unwrap();
    ai.fac.factories.insert(10, Factory::new(unit, "factorycloak", true, 1000.0));
    let worker = Unit::from_id(ai.cb, 20).unwrap();
    ai.eco.workers.insert(20, Worker::new(worker, ai.team_id));
    ai
}

#[test]
fn factory_expansion_plates_provide_production_without_spending_tech_slots() {
    let mut ai = expansion_ai();
    for id in 30..35 {
        let u = Unit::from_id(ai.cb, id).unwrap();
        ai.eco.plates.push(u);
        ai.fac.factories.insert(id, Factory::new(u, "factorycloak", false, 1000.0));
    }
    assert_eq!(ai.fac.factories.len(), 6);
    assert_eq!(ai.eco_tech_factory_count(), 1);
    assert!(ai.eco_has_factory("factorycloak"));
    ai.eco_update_new_factory(ai.frame);
    assert!(ai.eco.new_fac_due, "five plates must not prevent a new tech at +60 income");

    // Ordinary factories still exhaust the same income budget.
    ai.eco.new_fac_due = false;
    ai.eco.new_fac_at = Some(ai.frame);
    for id in 100..103 {
        let u = Unit::from_id(ai.cb, id).unwrap();
        ai.fac.factories.insert(id, Factory::new(u, "factorycloak", false, 1000.0));
    }
    assert_eq!(ai.eco_tech_factory_count(), 4);
    ai.eco_update_new_factory(ai.frame);
    assert!(!ai.eco.new_fac_due);
    assert_eq!(ai.eco.new_fac_at, Some(ai.frame), "a blocked timer remains ready to retry");
}

#[test]
fn factory_expansion_failed_site_retries_without_another_five_minute_wait() {
    let mut ai = expansion_ai();
    ai.eco_update_new_factory(ai.frame);
    assert!(ai.eco.new_fac_due);
    let next = ai.eco.new_fac_at;
    SITE_AVAILABLE.set(false);
    ai.eco_create_factory_task(20, false);
    assert!(ai.eco.factory_tasks.is_empty());
    assert!(ai.eco.new_fac_due, "a type choice is not a scheduled factory");
    assert_eq!(ai.eco.new_fac_at, next);
    SITE_AVAILABLE.set(true);
    ai.eco_create_factory_task(20, false);
    assert_eq!(ai.eco.factory_tasks.len(), 1);
    assert!(!ai.eco.new_fac_due, "consume the request only once its job exists");
    let task = ai.eco.tasks.get(ai.eco.factory_tasks[0]).unwrap().as_construction().unwrap();
    assert_eq!(ai.defs.name(task.build_type), "factoryplane");
}

#[test]
fn factory_expansion_energy_rejection_keeps_the_request_pending() {
    let mut ai = expansion_ai();
    ai.eco_update_new_factory(ai.frame);
    ai.set_feature_off("stallgate", false);
    ai.eco.estalled = true;
    ai.eco.metal = 500.0;
    ai.eco_create_factory_task(20, false);
    assert!(ai.eco.factory_tasks.is_empty());
    assert!(ai.eco.new_fac_due);
    ai.eco.estalled = false;
    ai.eco_create_factory_task(20, false);
    assert_eq!(ai.eco.factory_tasks.len(), 1);
    assert!(!ai.eco.new_fac_due);
}

#[test]
fn agalta_opening_survives_failed_placement_until_a_task_exists() {
    let mut ai = expansion_ai();
    ai.fac.factories.clear();
    ai.eco.opening = Some("factoryamph".into());
    ai.eco.potential_fac_list = BTreeSet::from(["factoryamph".into(), "factoryplane".into()]);
    let def = ai.defs.get("factoryamph").unwrap();
    let site = (def, Vec3::new(1000.0, 20.0, 1000.0), 0);
    ai.eco.opening_site = Some(site);
    SITE_AVAILABLE.set(false);
    ai.eco_create_factory_task(20, true);
    assert!(ai.eco.factory_tasks.is_empty());
    assert_eq!(ai.eco.opening.as_deref(), Some("factoryamph"));
    assert_eq!(ai.eco.opening_site, Some(site));
    SITE_AVAILABLE.set(true);
    ai.eco_create_factory_task(20, true);
    assert_eq!(ai.eco.factory_tasks.len(), 1);
    let task = ai.eco.tasks.get(ai.eco.factory_tasks[0]).unwrap().as_construction().unwrap();
    assert_eq!(task.build_type, def);
    assert!(ai.eco.opening.is_none());
    assert!(ai.eco.opening_site.is_none());
}

#[test]
fn agalta_rush_factory_is_binding_even_when_the_old_opening_was_consumed() {
    let mut ai = expansion_ai();
    ai.fac.factories.clear();
    crate::economy::rush::tests::planned_rush(&mut ai);
    ai.eco.opening = None;
    ai.eco.potential_fac_list = BTreeSet::from(["factoryplane".into()]);
    SITE_AVAILABLE.set(false);
    ai.eco_create_factory_task(20, true);
    assert!(ai.eco.factory_tasks.is_empty());
    SITE_AVAILABLE.set(true);
    ai.eco_create_factory_task(20, true);
    assert_eq!(ai.eco.factory_tasks.len(), 1);
    let task = ai.eco.tasks.get(ai.eco.factory_tasks[0]).unwrap().as_construction().unwrap();
    assert_eq!(ai.defs.name(task.build_type), "factoryamph");
}

#[test]
fn agalta_rush_timeout_restarts_idle_ground_factories_once() {
    let mut ai = expansion_ai();
    crate::economy::rush::tests::planned_rush(&mut ai);
    for (id, tech) in [(11, "factorycloak"), (12, "factorycloak"), (40, "factoryhover")] {
        let u = Unit::from_id(ai.cb, id).unwrap();
        ai.fac.factories.insert(id, Factory::new(u, tech, true, 1000.0));
    }
    for feature in ["reach", "expansioncons", "invade", "stakeouts", "scouts", "armysupport"] {
        ai.set_feature_off(feature, true);
    }
    for id in [10, 40] {
        ai.fac_unit_idle(Unit::from_id(ai.cb, id).unwrap());
    }
    BUILD_ORDERS.with_borrow(|o| assert!(o.is_empty(), "planned rush holds normal production"));
    ai.eco.last_rc_value_frame.set(1350);
    ai.frame = 1350;
    ai.rush_update(ai.frame);
    assert!(matches!(ai.eco.rush.state, crate::economy::rush::State::Planned { .. }));
    BUILD_ORDERS.with_borrow(|o| assert!(o.is_empty()));
    ai.frame = 1365;
    ai.eco.last_rc_value_frame.set(ai.frame);
    ai.rush_update(ai.frame);
    assert!(matches!(ai.eco.rush.state, crate::economy::rush::State::Over));
    BUILD_ORDERS.with_borrow(|o| assert_eq!(o.as_slice(), &[(10, 7), (40, 8)]));
    ai.frame = 1380;
    ai.rush_update(ai.frame);
    BUILD_ORDERS
        .with_borrow(|o| assert_eq!(o.len(), 2, "busy and unfinished factories stay untouched; no duplicate orders"));
}
