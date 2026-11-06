use super::*;
use crate::military::Fighter;
use crate::military::taskforce::{Classes, seeds};
use springai::UnitDef;
use std::cell::RefCell;
use std::ffi::{CStr, c_char};
use std::sync::Arc;

// id -> (definition, capture owner, position, alive)
thread_local! {
    static UNITS: RefCell<BTreeMap<i32, (i32, i32, Vec3, bool)>> = const { RefCell::new(BTreeMap::new()) };
    static ORDERS: RefCell<Vec<(i32, i32)>> = const { RefCell::new(Vec::new()) };
}
const NAMES: [&CStr; 10] = [
    c"vehcapture",
    c"vehriot",
    c"shieldshield",
    c"dyncomm",
    c"staticjammer",
    c"tankheavyassault",
    c"cloakjammer",
    c"shieldfelon",
    c"turretantiheavy",
    c"turretaaheavy",
];

pub(crate) fn fixture() -> Ai {
    UNITS.with_borrow_mut(BTreeMap::clear);
    ORDERS.with_borrow_mut(Vec::clear);
    unsafe extern "C" fn zero(_: i32, _: i32) -> i32 {
        0
    }
    unsafe extern "C" fn zero_float(_: i32, _: i32) -> f32 {
        0.0
    }
    unsafe extern "C" fn no(_: i32, _: i32) -> bool {
        false
    }
    unsafe extern "C" fn yes(_: i32, _: i32) -> bool {
        true
    }
    unsafe extern "C" fn elevation(_: i32, _: f32, _: f32) -> f32 {
        0.0
    }
    unsafe extern "C" fn params(_: i32, _: i32, _: *mut *const c_char, _: *mut *const c_char) -> i32 {
        0
    }
    unsafe extern "C" fn options(_: i32, _: i32, _: *mut i32, _: i32) -> i32 {
        0
    }
    unsafe extern "C" fn map_size(_: i32) -> i32 {
        128
    }
    unsafe extern "C" fn defs(_: i32, out: *mut i32, max: i32) -> i32 {
        if !out.is_null() {
            for i in 0..max.min(NAMES.len() as i32) {
                unsafe {
                    *out.add(i as usize) = i;
                }
            }
        }
        NAMES.len() as i32
    }
    unsafe extern "C" fn name(_: i32, id: i32) -> *const c_char {
        NAMES[id as usize].as_ptr()
    }
    unsafe extern "C" fn def(_: i32, id: i32) -> i32 {
        UNITS.with_borrow(|u| u.get(&id).map_or(-1, |u| u.0))
    }
    unsafe extern "C" fn team(_: i32, _: i32) -> i32 {
        2
    }
    unsafe extern "C" fn health(_: i32, id: i32) -> f32 {
        UNITS.with_borrow(|u| u.get(&id).map_or(0.0, |u| if u.3 { 1000.0 } else { 0.0 }))
    }
    unsafe extern "C" fn max_health(_: i32, _: i32) -> f32 {
        1000.0
    }
    unsafe extern "C" fn speed(_: i32, _: i32) -> f32 {
        58.5
    }
    unsafe extern "C" fn def_speed(_: i32, id: i32) -> f32 {
        if matches!(id, 4 | 8 | 9) { 0.0 } else { 58.5 }
    }
    unsafe extern "C" fn range(_: i32, _: i32) -> f32 {
        450.0
    }
    unsafe extern "C" fn cost(_: i32, id: i32, _: i32) -> f32 {
        if id == 5 { 3000.0 } else { 420.0 }
    }
    unsafe extern "C" fn rules(_: i32, id: i32, name: *const c_char, default: f32) -> f32 {
        if unsafe { CStr::from_ptr(name) } == c"capture_controller" {
            UNITS.with_borrow(|u| u.get(&id).map_or(default, |u| u.1 as f32))
        } else {
            default
        }
    }
    unsafe extern "C" fn pos(_: i32, id: i32, out: *mut f32) {
        let p = UNITS.with_borrow(|u| u.get(&id).map_or(Vec3::ZERO, |u| u.2));
        unsafe {
            std::ptr::copy_nonoverlapping([p.x, p.y, p.z].as_ptr(), out, 3);
        }
    }
    unsafe extern "C" fn commands(_: i32, _: i32, _: i32, topic: i32, data: *mut std::ffi::c_void) -> i32 {
        let id = unsafe { *(data as *const i32) };
        ORDERS.with_borrow_mut(|o| o.push((id, topic)));
        0
    }
    unsafe extern "C" fn units(_: i32, out: *mut i32, max: i32) -> i32 {
        UNITS.with_borrow(|u| {
            let ids: Vec<_> = u.iter().filter(|(_, u)| u.3).map(|(&id, _)| id).collect();
            if !out.is_null() {
                for (i, id) in ids.iter().take(max as usize).enumerate() {
                    unsafe {
                        *out.add(i) = *id;
                    }
                }
            }
            ids.len() as i32
        })
    }
    let mut ai = crate::economy::test_ai_with(|t| {
        t.UnitDef_getPower = Some(zero_float);
        t.UnitDef_getCustomParams = Some(params);
        t.UnitDef_getBuildOptions = Some(options);
        t.UnitDef_getWeaponMounts = Some(zero);
        t.UnitDef_getBuildSpeed = Some(zero_float);
        t.UnitDef_getBuildTime = Some(max_health);
        t.UnitDef_isAbleToFly = Some(no);
        t.UnitDef_isAbleToAttack = Some(yes);
        t.UnitDef_isMoveDataAvailable = Some(no);
        t.UnitDef_isFloater = Some(no);
        t.UnitDef_getMinWaterDepth = Some(zero_float);
        t.UnitDef_getMaxWaterDepth = Some(zero_float);
        t.UnitDef_getHeight = Some(zero_float);
        t.UnitDef_getHealth = Some(max_health);
        t.Unit_getWeapons = Some(zero);
        t.Unit_getCurrentCommands = Some(zero);
        t.Unit_getAllyTeam = Some(zero);
        t.Unit_getPower = Some(zero_float);
        t.Unit_getParalyzeDamage = Some(zero_float);
        t.Unit_getCaptureProgress = Some(zero_float);
        t.Unit_getSpeed = Some(speed);
        t.Unit_isCloaked = Some(no);
        t.Unit_isParalyzed = Some(no);
        t.Map_getElevationAt = Some(elevation);
        t.Map_getWidth = Some(map_size);
        t.Map_getHeight = Some(map_size);
        t.getUnitDefs = Some(defs);
        t.UnitDef_getName = Some(name);
        t.UnitDef_getTooltip = Some(name);
        t.UnitDef_getCost = Some(cost);
        t.UnitDef_getSpeed = Some(def_speed);
        t.UnitDef_getMaxWeaponRange = Some(range);
        t.Unit_getDef = Some(def);
        t.Unit_getTeam = Some(team);
        t.Unit_getHealth = Some(health);
        t.Unit_getMaxHealth = Some(max_health);
        t.Unit_getMaxSpeed = Some(speed);
        t.Unit_getRulesParamFloat = Some(rules);
        t.Unit_getPos = Some(pos);
        t.Engine_handleCommand = Some(commands);
        t.getTeamUnits = Some(units);
        t.getFriendlyUnits = Some(units);
    });
    reserved::clear(ai.cb);
    ai.graph.set_start_pos(Vec3::xz(100.0, 100.0));
    ai.taskforces.classes = Classes::default();
    ai.taskforces.doctrines =
        seeds().into_iter().filter(|d| matches!(d.name.as_str(), "dominatrix" | "felon")).collect();
    for d in &ai.taskforces.doctrines {
        ai.taskforces.shapes.insert(d.name.clone(), Arc::new(d.shape(&ai.defs, &ai.taskforces.classes)));
    }
    ai
}

pub(crate) fn spawn(ai: &Ai, id: i32, def: i32, captor: i32) -> Unit {
    UNITS.with_borrow_mut(|u| {
        u.insert(id, (def, captor, Vec3::xz(100.0, 100.0), true));
    });
    Unit::from_id(ai.cb, id).unwrap()
}
pub(crate) fn add(ai: &mut Ai, id: i32, def: i32) {
    let u = spawn(ai, id, def, -1);
    let mut f = Fighter::new(u, 420.0, ai.team_id);
    f.shield_mob = true;
    ai.squad_add_taskforce_member(f);
}
fn domi_teams(ai: &Ai) -> Vec<(u32, usize)> {
    ai.squad_handler
        .taskforces
        .iter()
        .filter_map(|&sid| ai.squads.get(sid))
        .filter(|s| s.shape.doctrine == "dominatrix")
        .map(|s| (s.id, s.cores(&ai.fighters)))
        .collect()
}

#[test]
fn three_cores_form_without_escorts_then_accrete_to_ten_and_leave_a_pair_alone() {
    let mut ai = fixture();
    add(&mut ai, 1, 0);
    add(&mut ai, 2, 0);
    assert!(domi_teams(&ai).is_empty());
    assert!(ai.fighters[&1].squad.is_some()); // Existing assault behavior.
    assert!(ai.fighters[&2].squad.is_some());
    add(&mut ai, 3, 0);
    let team = domi_teams(&ai);
    assert_eq!(team.len(), 1);
    assert_eq!(team[0].1, 3);
    assert_eq!(ai.squads.get(team[0].0).unwrap().everyone().count(), 3);
    for id in 4..=12 {
        add(&mut ai, id, 0);
    }
    assert_eq!(domi_teams(&ai), [(team[0].0, 10)]);
    add(&mut ai, 13, 0);
    assert_eq!(domi_teams(&ai).iter().map(|t| t.1).collect::<Vec<_>>(), [10, 3]);
    let all: Vec<_> = ai.squads.ids().into_iter().filter_map(|sid| ai.squads.get(sid)).flat_map(|s| s.everyone()).collect();
    for id in 1..=13 {
        assert_eq!(all.iter().filter(|&&i| i == id).count(), 1, "duplicated {id}");
    }
}

#[test]
fn rippers_join_later_and_caps_shrink_after_a_captor_retires() {
    let mut ai = fixture();
    for id in 1..=6 {
        add(&mut ai, id, 0);
    }
    let sid = domi_teams(&ai)[0].0;
    add(&mut ai, 20, 1);
    add(&mut ai, 21, 1);
    add(&mut ai, 22, 1);
    let ripper_count = |ai: &Ai| {
        ai.squads.get(sid).unwrap().everyone().filter(|id| ai.defs.name_of(ai.fighters[id].unit) == "vehriot").count()
    };
    assert_eq!(ripper_count(&ai), 2);
    assert_ne!(ai.fighters[&22].squad, Some(sid));
    ai.dominatrix_retire(ai.fighters[&1].unit, false);
    ai.dominatrix_release_excess(sid);
    assert_eq!(ripper_count(&ai), 1);
    assert!(!ai.fighters.contains_key(&1));
}

#[test]
fn shields_wait_for_density_but_never_gate_formation() {
    let mut ai = fixture();
    for id in 1..=3 {
        add(&mut ai, id, 0);
    }
    let sid = domi_teams(&ai)[0].0;
    let role = ai.taskforces.doctrine("dominatrix").unwrap().role_of("shieldshield", &ai.taskforces.classes).unwrap();
    assert!(!ai.squad_short_of(sid, role));
    add(&mut ai, 20, 2);
    assert_ne!(ai.fighters[&20].squad, Some(sid));
    assert_eq!(ai.squads.get(sid).unwrap().everyone().count(), 3);
    ai.frame = 30 * 60 * 10;
    assert!(ai.projected_metal_density() > 2000.0);
    for id in 21..=25 {
        add(&mut ai, id, 2);
    }
    let shields = ai
        .squads
        .get(sid)
        .unwrap()
        .everyone()
        .filter(|id| ai.defs.name_of(ai.fighters[id].unit) == "shieldshield")
        .count();
    assert_eq!(shields, 3);
    assert!(!ai.squad_short_of(sid, role));
}

#[test]
fn commander_capture_retires_both_and_cannot_reenter_worker_or_combat_pools() {
    let mut ai = fixture();
    add(&mut ai, 0, 0); // Zero is a valid controller id.
    let commander = spawn(&ai, 10, 3, 0);
    let mut worker = crate::economy::Worker::new(commander, ai.team_id);
    worker.is_com = true;
    ai.eco.workers.insert(10, worker);
    ai.eco.commanders.push(10);
    ai.dominatrix_observe_capture(commander);
    assert!(ai.dominatrix_vip(0) && ai.dominatrix_vip(10));
    assert!(!ai.fighters.contains_key(&0) && !ai.eco.workers.contains_key(&10));
    assert!(!ai.eco.commanders.contains(&10));
    ai.eco_unit_given(commander, 3, ai.team_id);
    ai.war_unit_finished(commander);
    assert!(!ai.eco.workers.contains_key(&10) && !ai.war.new_units.contains(&10));
    for id in [0, 10] {
        assert_eq!(reserved::owner(ai.cb, id).unwrap().module, "dominatrix");
    }
    ORDERS.with_borrow_mut(Vec::clear);
    reserved::as_module("squad.update", || commander.fight(Vec3::xz(200.0, 200.0), 0));
    ORDERS.with_borrow(|o| assert!(o.is_empty()));
    reserved::clear(ai.cb);
}

#[test]
fn expensive_captures_share_a_cloak_and_retirement_survives_checkpoint_and_policy_change() {
    let mut ai = fixture();
    for id in 1..=3 {
        add(&mut ai, id, 0);
    }
    for (id, captor) in [(10, 1), (11, 2)] {
        let captive = spawn(&ai, id, 5, captor);
        ai.dominatrix_observe_capture(captive);
    }
    for vip in ai.taskforces.dominatrices.retired.values_mut() {
        vip.home = Vec3::xz(100.0, 100.0);
    }
    assert_eq!(ai.dominatrix_cloaks_required(), 1);
    ai.dominatrix_validate_checkpoint().unwrap();
    let bytes = checkpoint::encode(&ai.taskforces.dominatrices).unwrap();
    let restored: Dominatrices =
        springai::snapshot::with_callback(ai.cb, || checkpoint::rmp_serde::from_slice(&bytes)).unwrap();
    assert_eq!(checkpoint::encode(&restored).unwrap(), bytes);
    ai.taskforces.dominatrices = restored;
    ai.dominatrix_switch(true);
    assert!(domi_teams(&ai).is_empty());
    assert_eq!(ai.dominatrix_cloaks_required(), 1);
    // Losing the prize does not revoke an earned retirement or entitlement.
    UNITS.with_borrow_mut(|u| {
        u.get_mut(&10).unwrap().3 = false;
        u.get_mut(&11).unwrap().3 = false;
    });
    assert!(ai.dominatrix_vip(1) && ai.dominatrix_vip(2));
    assert_eq!(ai.dominatrix_cloaks_required(), 1);
    assert_eq!(checkpoint::encode(&ai.taskforces.dominatrices).unwrap(), bytes);
    // Rebound handles still refer to the live engine's unit identities.
    assert_eq!(ai.taskforces.dominatrices.retired[&1].unit.def().unwrap(), UnitDef::from_id(ai.cb, 0).unwrap());
    let mut missing = serde_json::to_value(&ai.taskforces.dominatrices).unwrap();
    missing.as_object_mut().unwrap().remove("retired");
    assert!(serde_json::from_value::<Dominatrices>(missing).is_err());
    reserved::clear(ai.cb);
}

#[test]
fn fire_control_runs_without_optional_support_and_keeps_leader_membership_consistent() {
    let mut ai = fixture();
    for id in 1..=3 {
        add(&mut ai, id, 0);
    }
    let sid = domi_teams(&ai)[0].0;
    ai.set_feature_off("firesquads", true);
    assert!(ai.fire_assign(sid));
    assert_eq!(ai.squads.get(sid).unwrap().fire.cores.len(), 3);
    for _ in 0..3 {
        ai.frame += 30;
        assert!(ai.fire_assign(sid));
        let s = ai.squads.get(sid).unwrap();
        assert_eq!(s.everyone().count(), 3);
        assert!(!s.fighters.contains(&s.leader.unwrap()));
        assert!(s.everyone().all(|id| ai.fighters[&id].squad == Some(sid)));
    }
    ai.dominatrix_switch(true);
    assert!(ai.fighters.values().all(|f| f.squad.is_some()));
}

#[test]
fn retirement_chooses_rearmost_ground_fort_including_anni_but_not_aa() {
    let mut ai = fixture();
    ai.graph.set_start_pos(Vec3::xz(50.0, 100.0));
    let mut front = crate::graph::MetalSpot::new(0, 900.0, 0.0, 100.0, 2.0);
    front.owned = true;
    front.enemy_shadowed = true;
    ai.graph.metal_spots.push(front);
    let domi = spawn(&ai, 1, 0, -1);
    let near = spawn(&ai, 10, 8, -1);
    let rear = spawn(&ai, 11, 8, -1);
    let aa = spawn(&ai, 12, 9, -1);
    UNITS.with_borrow_mut(|u| {
        u.get_mut(&1).unwrap().2 = Vec3::xz(850.0, 100.0);
        u.get_mut(&10).unwrap().2 = Vec3::xz(800.0, 100.0);
        u.get_mut(&11).unwrap().2 = Vec3::xz(300.0, 100.0);
        u.get_mut(&12).unwrap().2 = Vec3::xz(100.0, 900.0);
    });
    assert_eq!(ai.dominatrix_home(domi, &[near, rear, aa]), Vec3::xz(200.0, 100.0));
    UNITS.with_borrow_mut(|u| u.get_mut(&11).unwrap().3 = false);
    assert_eq!(ai.dominatrix_home(domi, &[near, rear, aa]), Vec3::xz(700.0, 100.0));
}
