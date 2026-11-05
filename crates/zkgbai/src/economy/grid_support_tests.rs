use super::*;
use springai::ffi;
use std::{
    cell::{Cell, RefCell},
    ffi::{c_char, c_void},
};

thread_local! {
    static READY: Cell<bool> = const { Cell::new(false) };
    static BUILT: Cell<bool> = const { Cell::new(false) };
    static GROUND: Cell<f32> = const { Cell::new(200.0) };
    static ORDERS: RefCell<Vec<(i32, Vec<f32>)>> = const { RefCell::new(Vec::new()) };
}

fn ai() -> Ai {
    unsafe extern "C" fn defs(_: i32, out: *mut i32, max: i32) -> i32 {
        if !READY.get() {
            return 0;
        }
        if !out.is_null() && max > 0 {
            unsafe {
                *out = 1;
            }
        }
        1
    }
    unsafe extern "C" fn name(_: i32, id: i32) -> *const c_char {
        if id == 1 { c"energypylon".as_ptr() } else { c"cloakcon".as_ptr() }
    }
    unsafe extern "C" fn unit_def(_: i32, id: i32) -> i32 {
        if id == 10 { 1 } else { 2 }
    }
    unsafe extern "C" fn scalar(_: i32, _: i32) -> f32 {
        80.0
    }
    unsafe extern "C" fn range(_: i32, _: i32) -> f32 {
        500.0
    }
    unsafe extern "C" fn size(_: i32, _: i32) -> i32 {
        4
    }
    unsafe extern "C" fn zero(_: i32, _: i32) -> i32 {
        0
    }
    unsafe extern "C" fn team(_: i32, _: i32) -> i32 {
        2
    }
    unsafe extern "C" fn building(_: i32, id: i32) -> bool {
        id == 10 && !BUILT.get()
    }
    unsafe extern "C" fn elevation(_: i32, _: f32, _: f32) -> f32 {
        GROUND.get()
    }
    unsafe extern "C" fn near(_: i32, _: *mut f32, _: f32, _: bool, _: *mut i32, _: i32) -> i32 {
        0
    }
    unsafe extern "C" fn rules(_: i32, _: i32, _: *const c_char, default: f32) -> f32 {
        default
    }
    unsafe extern "C" fn pos(_: i32, id: i32, out: *mut f32) {
        let x = if id == 10 { 100.0 } else { 220.0 };
        unsafe {
            std::ptr::copy_nonoverlapping([x, GROUND.get(), 100.0].as_ptr(), out, 3);
        }
    }
    unsafe extern "C" fn command(_: i32, _: i32, _: i32, topic: i32, data: *mut c_void) -> i32 {
        if topic == ffi::CommandTopic::COMMAND_UNIT_CUSTOM as i32 {
            let c = unsafe { &*(data as *const ffi::SCustomUnitCommand) };
            let params = unsafe { std::slice::from_raw_parts(c.params, c.params_size as usize) }.to_vec();
            ORDERS.with(|o| o.borrow_mut().push((c.cmdId, params)));
        }
        0
    }
    READY.set(false);
    BUILT.set(false);
    GROUND.set(200.0);
    ORDERS.with(|o| o.borrow_mut().clear());
    let mut ai = super::super::test_ai_with(|cb| {
        cb.getUnitDefs = Some(defs);
        cb.UnitDef_getName = Some(name);
        cb.Unit_getDef = Some(unit_def);
        cb.Unit_getHealth = Some(scalar);
        cb.Unit_getTeam = Some(team);
        cb.Unit_getPos = Some(pos);
        cb.Unit_isBeingBuilt = Some(building);
        cb.Unit_getWeapons = Some(zero);
        cb.Unit_getCurrentCommands = Some(zero);
        cb.Unit_getRulesParamFloat = Some(rules);
        cb.UnitDef_getWeaponMounts = Some(zero);
        cb.UnitDef_getBuildSpeed = Some(scalar);
        cb.UnitDef_getBuildDistance = Some(range);
        cb.UnitDef_getTerraformSpeed = Some(scalar);
        cb.UnitDef_getHeight = Some(scalar);
        cb.UnitDef_getXSize = Some(size);
        cb.UnitDef_getZSize = Some(size);
        cb.Map_getElevationAt = Some(elevation);
        cb.getFriendlyUnitsIn = Some(near);
        cb.Engine_handleCommand = Some(command);
    });
    READY.set(true);
    ai.defs = crate::defs::Defs::new(ai.cb);
    ai.set_feature_off("grid", false);
    ai.eco.pylons.push(Unit::from_id(ai.cb, 10).unwrap());
    let worker = super::super::Worker::new(Unit::from_id(ai.cb, 20).unwrap(), ai.team_id);
    ai.eco.workers.insert(worker.id, worker);
    ai
}

#[test]
fn connect_before_burial_and_retry_the_same_absolute_depth() {
    let mut ai = ai();
    let pylon = ai.defs.get("energypylon").unwrap();
    ai.eco_plan_pylon_burial(pylon, Vec3::xz(100.0, 100.0));
    ai.eco_bury_support_pylons(0);
    assert!(ORDERS.with(|o| o.borrow().is_empty()), "finish power before digging");
    BUILT.set(true);
    ai.frame = 30;
    ai.eco_bury_support_pylons(30);
    assert!(ai.eco.terraform_hold.contains_key(&20));
    // The engine dropped an order halfway through; a retry must not lower
    // another full model-height from the partially lowered ground.
    GROUND.set(170.0);
    ai.frame = 120;
    ai.eco_bury_support_pylons(120);
    ai.frame = 450;
    ai.eco_bury_support_pylons(450);
    let levels: Vec<_> =
        ORDERS.with(|o| o.borrow().iter().filter(|c| c.0 == 39801).map(|c| (c.1[0], c.1[6])).collect());
    assert_eq!(levels, vec![(1.0, 120.0), (1.0, 120.0)]);
    GROUND.set(120.0);
    ai.frame = 480;
    ai.eco_bury_support_pylons(480);
    assert!(ai.eco.grid_support.burials[0].done);
    assert!(!ai.eco.terraform_hold.contains_key(&20));
}
