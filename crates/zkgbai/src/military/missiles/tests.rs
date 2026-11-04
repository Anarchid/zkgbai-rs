use super::*;
use springai::{SkirmishAi, ffi};
use std::cell::{Cell, RefCell};
use std::ffi::{c_char, c_void};

thread_local! {
    static DEFS: Cell<bool> = const { Cell::new(false) };
    static ENEMY_NEW: Cell<bool> = const { Cell::new(false) };
    static EMP: Cell<f32> = const { Cell::new(0.0) };
    static RIDGE: Cell<bool> = const { Cell::new(false) };
    static ORDERS: RefCell<Vec<(i32, Vec3)>> = const { RefCell::new(Vec::new()) };
}

fn fixture() -> Ai {
    unsafe extern "C" fn defs(_: i32, out: *mut i32, max: i32) -> i32 {
        if !DEFS.get() {
            return 0;
        }
        if !out.is_null() {
            for i in 0..max.min(6) {
                unsafe {
                    out.add(i as usize).write(i + 1);
                }
            }
        }
        6
    }
    unsafe extern "C" fn name(_: i32, id: i32) -> *const c_char {
        match id {
            1 => c"staticmissilesilo",
            2 => c"tacnuke",
            3 => c"empmissile",
            4 => c"seismic",
            5 => c"shieldshield",
            _ => c"striderdante",
        }
        .as_ptr()
    }
    unsafe extern "C" fn def(_: i32, id: i32) -> i32 {
        if ENEMY_NEW.get() {
            return -1;
        }
        match id {
            101 => 1,
            102 | 103 => 2,
            104 | 105 => 3,
            106 => 4,
            201 | 202 => 5,
            _ => 6,
        }
    }
    unsafe extern "C" fn pos(_: i32, id: i32, out: *mut f32) {
        let x = match id {
            201 => 3000.0,
            202 => 3020.0,
            203 => 3040.0,
            _ => 1000.0,
        };
        unsafe {
            std::ptr::copy_nonoverlapping([x, 30.0, 1000.0].as_ptr(), out, 3);
        }
    }
    unsafe extern "C" fn vel(_: i32, _: i32, out: *mut f32) {
        unsafe {
            std::ptr::copy_nonoverlapping([1.0, 0.0, 0.0].as_ptr(), out, 3);
        }
    }
    unsafe extern "C" fn health(_: i32, _: i32) -> f32 {
        3000.0
    }
    unsafe extern "C" fn emp(_: i32, _: i32) -> f32 {
        EMP.get()
    }
    unsafe extern "C" fn paralyzed(_: i32, _: i32) -> bool {
        EMP.get() > 3000.0
    }
    unsafe extern "C" fn no(_: i32, _: i32) -> bool {
        false
    }
    unsafe extern "C" fn zero(_: i32, _: i32) -> i32 {
        0
    }
    unsafe extern "C" fn no_units(_: i32, _: *mut i32, _: i32) -> i32 {
        0
    }
    unsafe extern "C" fn no_friends(_: i32, _: *mut f32, _: f32, _: bool, _: *mut i32, _: i32) -> i32 {
        0
    }
    unsafe extern "C" fn ground(_: i32, x: f32, _: f32) -> f32 {
        if RIDGE.get() && (2600.0..2620.0).contains(&x) { 600.0 } else { 0.0 }
    }
    unsafe extern "C" fn mounts(_: i32, id: i32) -> i32 {
        i32::from((2..=4).contains(&id))
    }
    unsafe extern "C" fn weapon(_: i32, id: i32, _: i32) -> i32 {
        id
    }
    unsafe extern "C" fn range(_: i32, id: i32) -> f32 {
        if id == 4 { 6000.0 } else { 3500.0 }
    }
    unsafe extern "C" fn uptime(_: i32, id: i32) -> f32 {
        if id == 3 { 5.0 } else { 3.0 }
    }
    unsafe extern "C" fn accel(_: i32, _: i32) -> f32 {
        0.2
    }
    unsafe extern "C" fn start(_: i32, _: i32) -> f32 {
        0.01 / 30.0
    }
    unsafe extern "C" fn speed(_: i32, _: i32) -> f32 {
        40.0
    }
    unsafe extern "C" fn turn(_: i32, id: i32) -> f32 {
        (if id == 3 { 12000.0 } else { 18000.0 }) * std::f32::consts::TAU / 65536.0 / 30.0
    }
    unsafe extern "C" fn cost(_: i32, id: i32, _: i32) -> f32 {
        if id == 5 { 750.0 } else { 6000.0 }
    }
    unsafe extern "C" fn shield(_: i32, _: i32, _: i32) -> f32 {
        3600.0
    }
    unsafe extern "C" fn size(_: i32) -> i32 {
        1024
    }
    unsafe extern "C" fn command(_: i32, _: i32, _: i32, topic: i32, data: *mut c_void) -> i32 {
        if topic as u32 == ffi::CommandTopic::COMMAND_UNIT_CUSTOM {
            let c = unsafe { &*data.cast::<ffi::SCustomUnitCommand>() };
            if c.cmdId == CMD_ATTACK {
                let p = unsafe { std::slice::from_raw_parts(c.params, c.params_size as usize) };
                ORDERS.with_borrow_mut(|o| o.push((c.unitId, Vec3::new(p[0], p[1], p[2]))));
            }
        }
        0
    }
    DEFS.set(false);
    ENEMY_NEW.set(false);
    EMP.set(0.0);
    RIDGE.set(false);
    ORDERS.with_borrow_mut(Vec::clear);
    unsafe extern "C" fn floats(_: i32, _: *mut f32, _: i32) -> i32 {
        0
    }
    unsafe extern "C" fn zero_ai(_: i32) -> i32 {
        0
    }
    unsafe extern "C" fn water(_: i32) -> f32 {
        0.0
    }
    unsafe extern "C" fn log(_: i32, _: *const c_char) {}
    unsafe extern "C" fn lookup(_: i32, _: *const c_char) -> i32 {
        -1
    }
    unsafe extern "C" fn resource(_: i32, _: *const c_char) -> i32 {
        0
    }
    unsafe extern "C" fn option(_: i32, _: *const c_char) -> *const c_char {
        c"off".as_ptr()
    }
    unsafe extern "C" fn rules(_: i32, _: *const c_char, default: f32) -> f32 {
        default
    }
    let mut t = ffi::SSkirmishAICallback {
        Map_getSlopeMap: Some(floats),
        Map_getHeightMap: Some(floats),
        Map_getLosMap: Some(no_units),
        Map_getWaterDamage: Some(water),
        Mod_getLosMipLevel: Some(zero_ai),
        Game_getMyTeam: Some(zero_ai),
        Game_getMyAllyTeam: Some(zero_ai),
        Game_getRulesParamFloat: Some(rules),
        getUnitDefByName: Some(lookup),
        getResourceByName: Some(resource),
        SkirmishAI_OptionValues_getValueByKey: Some(option),
        Log_log: Some(log),
        Unit_isBeingBuilt: Some(no),
        ..Default::default()
    };
    {
        t.getUnitDefs = Some(defs);
        t.UnitDef_getName = Some(name);
        t.UnitDef_getHumanName = Some(name);
        t.UnitDef_getWeaponMounts = Some(mounts);
        t.UnitDef_WeaponMount_getWeaponDef = Some(weapon);
        t.UnitDef_getCost = Some(cost);
        t.UnitDef_getHealth = Some(health);
        t.UnitDef_isAbleToFly = Some(no);
        t.Unit_getDef = Some(def);
        t.Unit_getPos = Some(pos);
        t.Unit_getVel = Some(vel);
        t.Unit_getHealth = Some(health);
        t.Unit_getMaxHealth = Some(health);
        t.Unit_getParalyzeDamage = Some(emp);
        t.Unit_isParalyzed = Some(paralyzed);
        t.Unit_getCurrentCommands = Some(zero);
        t.Unit_Weapon_getShieldPower = Some(shield);
        t.getFriendlyUnits = Some(no_units);
        t.getFriendlyUnitsIn = Some(no_friends);
        t.Map_getElevationAt = Some(ground);
        t.Map_getWidth = Some(size);
        t.Map_getHeight = Some(size);
        t.WeaponDef_getRange = Some(range);
        t.WeaponDef_getUpTime = Some(uptime);
        t.WeaponDef_getWeaponAcceleration = Some(accel);
        t.WeaponDef_getStartVelocity = Some(start);
        t.WeaponDef_getProjectileSpeed = Some(speed);
        t.WeaponDef_getTurnRate = Some(turn);
        t.Engine_handleCommand = Some(command);
    }
    let cb = unsafe { springai::Callback::from_raw(0, Box::leak(Box::new(t))) };
    let mut ai = Ai::new(cb);
    DEFS.set(true);
    ai.defs = crate::defs::Defs::new(ai.cb);
    ai.frame = 3000;
    ai.missiles.silo = Unit::from_id(ai.cb, 101);
    for id in [201, 202, 203] {
        ENEMY_NEW.set(true);
        let mut e = crate::military::enemy::Enemy::new(Unit::from_id(ai.cb, id).unwrap(), 0.0);
        ENEMY_NEW.set(false);
        e.ud = e.unit.def();
        e.position = e.unit.pos();
        e.def_name = e.ud.unwrap().name();
        e.value = e.ud.unwrap().cost(ai.metal);
        e.visible = true;
        e.identified = true;
        e.build_progress = 1.0;
        e.last_seen = ai.frame;
        ai.war.targets.insert(id, e);
    }
    ai
}

#[test]
fn observed_stun_opens_eos_targets_but_short_stun_fog_and_terrain_do_not() {
    let mut ai = fixture();
    // Remove shields to isolate the mobile target's own stun qualification.
    ai.war.targets.remove(&201);
    ai.war.targets.remove(&202);
    assert!(ai.missiles_eos_targets().is_empty());
    EMP.set(3100.0); // 1.33 seconds, not enough for flight.
    assert!(ai.missiles_eos_targets().is_empty());
    EMP.set(6000.0); // 40 seconds.
    assert_eq!(ai.missiles_eos_targets().len(), 1);
    ai.missiles.ready = vec![Unit::from_id(ai.cb, 102).unwrap()];
    RIDGE.set(true);
    ai.missiles_fire_eos();
    assert!(ORDERS.with_borrow(Vec::is_empty));
    RIDGE.set(false);
    ai.war.targets.get_mut(&203).unwrap().visible = false;
    ai.missiles_fire_eos();
    assert!(ORDERS.with_borrow(Vec::is_empty));
    ai.war.targets.get_mut(&203).unwrap().visible = true;
    ai.missiles_fire_eos();
    assert_eq!(ORDERS.with_borrow(Vec::len), 1);
    assert!(ai.missiles.ready.is_empty());
}

#[test]
fn shieldball_leads_the_pack_and_requires_a_complete_salvo_from_existing_silo() {
    let mut ai = fixture();
    let ball = ai.missiles_shieldball(ai.missiles.silo.unwrap().pos()).unwrap();
    assert_eq!(ball.needed, 2);
    assert_eq!(ball.members.len(), 3);
    assert!(ball.at.x > 3200.0, "moving pack must be led by the Shockley flight time");
    ai.missiles.ready = vec![Unit::from_id(ai.cb, 104).unwrap()];
    ai.missiles_fire_shieldball();
    assert!(ORDERS.with_borrow(Vec::is_empty));
    ai.missiles.ready.push(Unit::from_id(ai.cb, 105).unwrap());
    ai.missiles_fire_shieldball();
    assert_eq!(ORDERS.with_borrow(Vec::len), 2);
    assert!(ai.missiles_shieldball(ai.missiles.silo.unwrap().pos()).is_none(), "do not repeat the same strike");
    ai.missiles.shocked.clear();
    ai.missiles.silo = None;
    assert!(ai.missiles_shieldball(Vec3::xz(1000.0, 1000.0)).is_none());
}

#[test]
fn stunned_cluster_counts_combined_value_and_salvos_do_not_overlap() {
    let ai = fixture();
    EMP.set(4500.0); // Twenty seconds: enough for flight, even without a fresh 45s Shockley stun.
    let targets = ai.missiles_stunned_clusters();
    assert!(!targets.is_empty());
    assert!(targets.iter().all(|t| t.value == 7500.0 && t.members.len() == 3));
    let pads = [Vec3::xz(1000.0, 1000.0); 4];
    let orders = eos_salvos_checked(targets, &pads, 0, |_, _| true);
    assert_eq!(orders.len(), 1, "overlapping cluster candidates must not double-spend ammunition");
}

#[test]
fn quake_reserves_eos_and_only_real_post_impact_terrain_unlocks_followthrough() {
    for opens in [false, true] {
        let mut ai = fixture();
        ai.war.targets.remove(&201);
        ai.war.targets.remove(&202);
        ai.war.targets.get_mut(&203).unwrap().is_static = true;
        ai.eco.adjusted_income = SILO_INCOME;
        RIDGE.set(true);
        ai.missiles.ready = vec![Unit::from_id(ai.cb, 102).unwrap(), Unit::from_id(ai.cb, 106).unwrap()];
        let silo = ai.missiles.silo.unwrap();
        ai.missiles_refresh_seismic(silo);
        assert_eq!(ai.missiles.seismic.eos_demand(), 1);
        assert!(ai.missiles_quake_demand().is_some());
        ai.missiles_fire_quake();
        assert_eq!(ORDERS.with_borrow(Vec::len), 1);
        assert_eq!(ORDERS.with_borrow(|o| o[0].0), 106);
        RIDGE.set(!opens);
        ai.missiles_fire_eos();
        assert_eq!(ORDERS.with_borrow(Vec::len), 1, "Eos remains reserved until impact verification");
        ai.frame += 600;
        ai.missiles_refresh_seismic(silo);
        ai.missiles_fire_eos();
        assert_eq!(ORDERS.with_borrow(Vec::len), if opens { 2 } else { 1 });
        assert!(ai.missiles_quake_demand().is_none(), "cooldown prevents repeated terraforming");
    }
}
