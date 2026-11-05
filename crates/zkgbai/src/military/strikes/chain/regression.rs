//! Drive the real controller across expiry and an asynchronous chain answer.
use super::*;
use springai::{SkirmishAi, ffi};
use std::{
    cell::Cell,
    ffi::{c_char, c_void},
    sync::mpsc,
};

thread_local! {
    static DEFS: Cell<bool> = const { Cell::new(false) };
    static ENEMY_NEW: Cell<bool> = const { Cell::new(false) };
    static ATTACKS: Cell<usize> = const { Cell::new(0) };
}

fn fixture() -> Ai {
    unsafe extern "C" fn defs(_: i32, out: *mut i32, max: i32) -> i32 {
        if !DEFS.get() {
            return 0;
        }
        if !out.is_null() {
            for i in 0..max.min(2) {
                unsafe {
                    out.add(i as usize).write(i + 1);
                }
            }
        }
        2
    }
    unsafe extern "C" fn name(_: i32, id: i32) -> *const c_char {
        if id == 1 { c"spiderscout".as_ptr() } else { c"hoverarty".as_ptr() }
    }
    unsafe extern "C" fn def(_: i32, id: i32) -> i32 {
        if ENEMY_NEW.get() {
            -1
        } else if id < 100 {
            1
        } else {
            2
        }
    }
    unsafe extern "C" fn pos(_: i32, id: i32, out: *mut f32) {
        let x = if id < 100 { 4900.0 } else { 5000.0 };
        unsafe {
            std::ptr::copy_nonoverlapping([x, 0.0, 1000.0].as_ptr(), out, 3);
        }
    }
    unsafe extern "C" fn hp(_: i32, _: i32) -> f32 {
        100.0
    }
    unsafe extern "C" fn range(_: i32, _: i32) -> f32 {
        200.0
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
    unsafe extern "C" fn floats(_: i32, _: *mut f32, _: i32) -> i32 {
        0
    }
    unsafe extern "C" fn ground(_: i32, _: f32, _: f32) -> f32 {
        0.0
    }
    unsafe extern "C" fn size(_: i32) -> i32 {
        1024
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
    unsafe extern "C" fn unit_rules(_: i32, _: i32, _: *const c_char, default: f32) -> f32 {
        default
    }
    unsafe extern "C" fn cost(_: i32, _: i32, _: i32) -> f32 {
        1000.0
    }
    unsafe extern "C" fn command(_: i32, _: i32, _: i32, topic: i32, _: *mut c_void) -> i32 {
        if topic as u32 == ffi::CommandTopic::COMMAND_UNIT_ATTACK {
            ATTACKS.set(ATTACKS.get() + 1);
        }
        0
    }
    DEFS.set(false);
    ENEMY_NEW.set(false);
    ATTACKS.set(0);
    let table = ffi::SSkirmishAICallback {
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
        getUnitDefs: Some(defs),
        UnitDef_getName: Some(name),
        UnitDef_getHumanName: Some(name),
        UnitDef_getWeaponMounts: Some(zero),
        UnitDef_getCost: Some(cost),
        UnitDef_getHealth: Some(hp),
        UnitDef_isAbleToFly: Some(no),
        UnitDef_isAbleToMove: Some(no),
        UnitDef_isBuilder: Some(no),
        UnitDef_getSpeed: Some(hp),
        UnitDef_getRadius: Some(hp),
        UnitDef_getMaxWeaponRange: Some(range),
        UnitDef_getCategory: Some(zero),
        Unit_getDef: Some(def),
        Unit_getPos: Some(pos),
        Unit_getHealth: Some(hp),
        Unit_getMaxHealth: Some(hp),
        Unit_getTeam: Some(zero),
        Unit_getRulesParamFloat: Some(unit_rules),
        UnitDef_isMoveDataAvailable: Some(no),
        Unit_getMaxRange: Some(range),
        Unit_getWeapons: Some(zero),
        Unit_isBeingBuilt: Some(no),
        Unit_isParalyzed: Some(no),
        getFriendlyUnits: Some(no_units),
        Map_getElevationAt: Some(ground),
        Map_getWidth: Some(size),
        Map_getHeight: Some(size),
        Engine_handleCommand: Some(command),
        ..Default::default()
    };
    // SAFETY: all used callbacks are supplied and the table lives for the test process.
    let cb = unsafe { springai::Callback::from_raw(0, Box::leak(Box::new(table))) };
    let mut ai = Ai::new(cb);
    DEFS.set(true);
    ai.defs = crate::defs::Defs::new(cb);
    ai.frame = DIVE_TIMEOUT + PERIOD;
    ai.offset = 0;
    ENEMY_NEW.set(true);
    let mut enemy = crate::military::enemy::Enemy::new(Unit::from_id(cb, 100).unwrap(), 1000.0);
    ENEMY_NEW.set(false);
    enemy.ud = enemy.unit.def();
    enemy.def_name = "hoverarty".into();
    enemy.visible = true;
    enemy.identified = true;
    enemy.last_seen = ai.frame;
    ai.war.targets.insert(100, enemy);
    for id in [1, 2, 3] {
        ai.fighters.insert(id, Fighter::new(Unit::from_id(cb, id).unwrap(), 25.0, 0));
        ai.strikes.members.insert(id);
    }
    ai.strikes.wave = Some(Wave {
        tactic: Tactic::Flea,
        job: ai.strike_jobs(SNIPER_MEMORY).remove(0),
        needed: 20,
        members: vec![1, 2, 3],
        chain: Chain::default(),
        phase: Phase::Diving,
        since: 0,
        home: Vec3::xz(100.0, 1000.0),
        query: None,
    });
    ai
}

#[test]
fn timeout_reassesses_same_live_target_without_cooldown_or_return_orders() {
    let mut ai = fixture();
    let (tx, requests) = mpsc::sync_channel(1);
    let (answers, rx) = mpsc::sync_channel(1);
    ai.strikes.predictor = Some(prediction::Predictor { tx, rx });
    ai.strikes_update(ai.frame);
    let wave = ai.strikes.wave.as_ref().unwrap();
    assert_eq!(wave.phase, Phase::Chaining);
    assert_eq!(wave.job.id, Some(100));
    assert_eq!(wave.needed, 3);
    assert!(ai.strikes.cooldowns.is_empty());
    assert_eq!(ATTACKS.get(), 3, "the survivors keep firing while reassessed");
    let request = requests.try_recv().expect("timeout must submit a fresh prediction immediately");
    assert_eq!(request.members, vec![1, 2, 3]);
    answers
        .send(prediction::Answer {
            key: request.key,
            reload_units: request.reload_units,
            reload_stamp: request.reload_stamp,
            kill_frames: Some(100),
            probability: 1.0,
            pos: request.pos,
            members: request.members,
        })
        .unwrap();
    ai.frame += PERIOD;
    ai.strikes_update(ai.frame);
    let wave = ai.strikes.wave.as_ref().unwrap();
    assert_eq!(wave.phase, Phase::Diving);
    assert_eq!(wave.job.id, Some(100));
    assert_eq!(wave.since, ai.frame);
    assert_eq!(ATTACKS.get(), 6);
    assert!(ai.strikes.cooldowns.is_empty());
}

#[test]
fn squad_maintenance_chains_expired_mission_instead_of_releasing_it() {
    let mut ai = fixture();
    ai.strikes.wave = None;
    ai.strikes.members.clear();
    let sid = ai.squads.create(crate::military::SquadKind::Raider, 0);
    let squad = ai.squads.get_mut(sid).unwrap();
    squad.status = 'a';
    squad.fighters = vec![1, 2, 3];
    squad.leader = Some(1);
    for fighter in ai.fighters.values_mut() {
        fighter.squad = Some(sid);
    }
    let (tx, requests) = mpsc::sync_channel(1);
    let (answers, rx) = mpsc::sync_channel(1);
    ai.strikes.squad_predictor = Some(prediction::Predictor { tx, rx });
    assert!(ai.squad_strike(sid).is_some());
    let request = requests.try_recv().unwrap();
    answers
        .send(prediction::Answer {
            key: request.key,
            reload_units: request.reload_units,
            reload_stamp: request.reload_stamp,
            kill_frames: Some(100),
            probability: 1.0,
            pos: request.pos,
            members: request.members,
        })
        .unwrap();
    ai.frame += PERIOD;
    assert!(ai.squad_strike(sid).is_some());
    assert_eq!(ai.squad_strikes_status()[0]["phase"], "diving");
    ai.frame += DIVE_TIMEOUT;
    ai.war.targets.get_mut(&100).unwrap().last_seen = ai.frame;
    ai.squad_strikes_maintain();
    assert_eq!(ai.squad_strikes_status()[0]["phase"], "chaining");
    assert_eq!(ai.squad_strikes_status()[0]["target"], 100);
    assert!(!ai.strikes.squad_retry.contains_key(&sid), "no ten-second release cooldown");
    let request = requests.try_recv().expect("maintenance must reassess immediately");
    assert_eq!(request.members, vec![1, 2, 3]);
    assert_eq!(ATTACKS.get(), 6, "expiry must not interrupt in-range fire");
}
