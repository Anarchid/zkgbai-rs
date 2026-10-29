use super::*;
fn enemy(id: i32) -> Contact {
    Contact {
        id,
        pos: Vec3::xz(2000.0, 2000.0),
        vel: Vec3::ZERO,
        value: 300.0,
        hp: 1000.0,
        max_hp: 1000.0,
        seen: 300,
        known: true,
        live: true,
        air: false,
        static_: false,
        heavy: false,
        raider: false,
        wind: false,
        singu: false,
        home: false,
        fighting: false,
    }
}
fn clear() -> Risk {
    Risk { aa: 0.0, exposure: 0.0, intercepted: false }
}
fn job(kind: MissionKind, hp: f32) -> Mission {
    Mission {
        kind,
        target: 1,
        pos: Vec3::xz(2000.0, 2000.0),
        hp,
        value: 1000.0,
        targets: vec![1],
        seen: 300,
        deep: false,
    }
}
#[test]
fn ravens_have_twenty_percent_margin_and_one_spare() {
    for (hp, n) in [(800.0, 3), (2000.0, 4), (4000.0, 7)] {
        assert_eq!(packages(&job(MissionKind::Heavy, hp), clear(), 800.0), vec![vec![(Plane::Raven, n)]]);
    }
}
#[test]
fn group_and_heavy_switch_to_likho_with_aa() {
    let r = Risk { aa: 0.3, exposure: 0.5, intercepted: false };
    assert_eq!(packages(&job(MissionKind::Group, 2500.0), clear(), 800.0)[0][0].0, Plane::Phoenix);
    for kind in [MissionKind::Group, MissionKind::Heavy] {
        assert_eq!(packages(&job(kind, 2500.0), r, 800.0)[0][0].0, Plane::Likho);
    }
    assert!(packages(&job(MissionKind::Wind, 500.0), r, 800.0).is_empty());
}
#[test]
fn interception_needs_magpie_and_raptors() {
    assert_eq!(
        packages(&job(MissionKind::Intercept, 1200.0), clear(), 800.0),
        vec![vec![(Plane::Magpie, 1), (Plane::Raptor, 2)]]
    );
    assert!(inbound(Vec3::xz(3000.0, 0.0), Vec3::xz(-200.0, 0.0), Vec3::ZERO, false));
    assert!(!inbound(Vec3::xz(3000.0, 0.0), Vec3::xz(200.0, 0.0), Vec3::ZERO, false));
    assert!(!inbound(Vec3::xz(3000.0, 0.0), Vec3::xz(0.0, 200.0), Vec3::ZERO, false));
}
#[test]
fn wind_clump_and_medium_group_are_distinct_jobs() {
    let mut es: Vec<_> = (0..6).map(enemy).collect();
    for (i, e) in es.iter_mut().enumerate() {
        e.pos.x += i as f32 * 40.0;
        e.static_ = true;
        e.wind = true;
        e.value = 35.0;
    }
    let js = missions(&es, Vec3::ZERO, 300);
    assert_eq!(js.iter().filter(|j| j.kind == MissionKind::Wind).count(), 1);
    assert!(!js.iter().any(|j| j.kind == MissionKind::Group));
    for e in &mut es {
        e.static_ = false;
        e.wind = false;
        e.value = 300.0;
    }
    assert!(missions(&es, Vec3::ZERO, 300).iter().any(|j| j.kind == MissionKind::Group));
}
#[test]
fn stale_mobiles_cannot_launch_but_static_memory_survives() {
    let mut e = enemy(1);
    e.heavy = true;
    e.live = false;
    assert!(missions(&[e.clone()], Vec3::ZERO, 1000).is_empty());
    e.static_ = true;
    e.value = 1000.0;
    assert!(missions(&[e], Vec3::ZERO, 1000).iter().any(|j| j.kind == MissionKind::Thermite));
}
#[test]
fn secondary_jobs_never_become_primary() {
    for k in [MissionKind::Assassinate, MissionKind::Static, MissionKind::Shield] {
        assert!(!k.primary());
    }
    let mut e = enemy(1);
    e.hp = 200.0;
    e.value = 900.0;
    e.fighting = true;
    let js = missions(&[e], Vec3::ZERO, 300);
    assert!(js.iter().any(|j| j.kind == MissionKind::Assassinate));
    assert!(js.iter().any(|j| j.kind == MissionKind::Shield));
    assert!(js.iter().all(|j| !j.kind.primary()));
}
#[test]
fn exposure_includes_return_and_rejects_lethal_or_missing_routes() {
    assert!(!Risk { aa: 0.0, exposure: f32::INFINITY, intercepted: false }.survives(5200.0, 185.0));
    assert!(!Risk { aa: 0.0, exposure: 20.0, intercepted: false }.survives(5200.0, 185.0));
    assert!(Risk { aa: 0.1, exposure: 1.0, intercepted: false }.survives(5200.0, 185.0));
    assert!(packages(&job(MissionKind::Wind, 100.0), Risk { intercepted: true, ..clear() }, 800.0).is_empty());
}
#[test]
fn impossible_waves_are_not_clamped_into_suicide_attacks() {
    assert!(packages(&job(MissionKind::Heavy, 100000.0), clear(), 800.0).is_empty());
}
#[test]
fn expensive_statics_and_home_raiders_create_primary_jobs() {
    let mut singu = enemy(1);
    singu.static_ = true;
    singu.value = 4000.0;
    singu.singu = true;
    let mut raider = enemy(2);
    raider.raider = true;
    raider.home = true;
    let js = missions(&[singu, raider], Vec3::ZERO, 300);
    assert!(js.iter().any(|j| j.kind == MissionKind::Thermite));
    assert!(js.iter().any(|j| j.kind == MissionKind::Interdict));
}

#[test]
fn deep_odin_strikes_require_a_jammer_per_bomber() {
    let mut j = job(MissionKind::Thermite, 20000.0);
    assert_eq!(packages(&j, clear(), 800.0), vec![vec![(Plane::Odin, 2)]]);
    j.deep = true;
    assert_eq!(packages(&j, clear(), 800.0), vec![vec![(Plane::Odin, 2), (Plane::Sparrow, 2)]]);
}

#[test]
fn reservations_prevent_duplicate_purchases_and_complete_alternatives_win() {
    use std::collections::BTreeMap;
    let packages = vec![vec![(Plane::Phoenix, 1)], vec![(Plane::Magpie, 3)]];
    let mut have = BTreeMap::new();
    assert_eq!(purchase(&packages, &have), Some(Plane::Phoenix));
    have.insert(Plane::Phoenix, 1); // queued by the first factory
    assert_eq!(purchase(&packages, &have), None); // second factory pauses
    have.clear();
    have.insert(Plane::Magpie, 3);
    assert_eq!(purchase(&packages, &have), None);
    let deep = vec![vec![(Plane::Odin, 1), (Plane::Sparrow, 1)]];
    have.insert(Plane::Odin, 1);
    assert_eq!(purchase(&deep, &have), Some(Plane::Sparrow));
    have.insert(Plane::Sparrow, 1);
    assert_eq!(purchase(&deep, &have), None);
}

#[test]
fn assassination_deadline_accounts_for_a_fleeing_target() {
    let target = Vec3::xz(2000.0, 0.0);
    let toward = Vec3::xz(-100.0, 0.0);
    let away = Vec3::xz(100.0, 0.0);
    assert!(intercept_time(Vec3::ZERO, target, toward, 250.0).unwrap() < 12.0);
    assert!(intercept_time(Vec3::ZERO, target, away, 250.0).unwrap() > 12.0);
    assert!(intercept_time(Vec3::ZERO, target, Vec3::xz(300.0, 0.0), 250.0).is_none());
}
