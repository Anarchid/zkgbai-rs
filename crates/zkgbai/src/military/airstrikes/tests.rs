use super::*;
pub(super) fn enemy(id: i32) -> Contact {
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
    Risk { aa: 0.0, exposure: 0.0, intercepted: false, burst: 0.0 }
}
fn job(kind: MissionKind, hp: f32) -> Mission {
    Mission {
        kind,
        group: (kind == MissionKind::Group).then(|| {
            let mut c = enemy(1);
            c.hp = hp;
            c.value = 1000.0;
            GroupStrike::new(&[&c])
        }),
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
        assert_eq!(packages(&job(MissionKind::Heavy, hp), clear(), 800.0)[0], vec![(Plane::Raven, n)]);
    }
}
#[test]
fn group_and_heavy_switch_to_likho_with_aa() {
    let r = Risk { aa: 0.3, exposure: 0.5, intercepted: false, burst: 0.0 };
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
    assert!(!Risk { aa: 0.0, exposure: f32::INFINITY, intercepted: false, burst: 0.0 }.survives(5200.0, 185.0));
    assert!(!Risk { aa: 0.0, exposure: 20.0, intercepted: false, burst: 0.0 }.survives(5200.0, 185.0));
    assert!(Risk { aa: 0.1, exposure: 1.0, intercepted: false, burst: 0.0 }.survives(5200.0, 185.0));
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
    assert_eq!(purchase(&packages, &have, |_| 1.0), Some(Plane::Phoenix));
    have.insert(Plane::Phoenix, 1); // queued by the first factory
    assert_eq!(purchase(&packages, &have, |_| 1.0), None); // second factory pauses
    have.clear();
    have.insert(Plane::Magpie, 3);
    assert_eq!(purchase(&packages, &have, |_| 1.0), None);
    let deep = vec![vec![(Plane::Odin, 1), (Plane::Sparrow, 1)]];
    have.insert(Plane::Odin, 1);
    assert_eq!(purchase(&deep, &have, |_| 1.0), Some(Plane::Sparrow));
    have.insert(Plane::Sparrow, 1);
    assert_eq!(purchase(&deep, &have, |_| 1.0), None);
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

#[test]
fn cheap_artillery_is_secondary_but_groups_of_cheap_units_remain_primary() {
    let mut fencer = enemy(1);
    fencer.heavy = true;
    fencer.value = 300.0;
    let js = missions(&[fencer.clone()], Vec3::ZERO, 300);
    assert!(!js.iter().any(|j| j.kind.primary()));
    assert!(js.iter().any(|j| j.kind == MissionKind::Opportunistic));
    for (cost, primary) in [(599.0, false), (600.0, true), (1000.0, true)] {
        fencer.value = cost;
        assert_eq!(missions(&[fencer.clone()], Vec3::ZERO, 300).iter().any(|j| j.kind == MissionKind::Heavy), primary);
    }
    fencer.value = 300.0;
    fencer.hp = 1.0;
    assert!(!missions(&[fencer], Vec3::ZERO, 300).iter().any(|j| j.kind.primary()));
    let group: Vec<_> = (0..4).map(enemy).collect();
    assert!(missions(&group, Vec3::ZERO, 300).iter().any(|j| j.kind == MissionKind::Group && j.kind.primary()));
}

#[test]
fn existing_likhos_can_alpha_priority_targets_and_groups_in_clear_air() {
    use std::collections::BTreeMap;
    for kind in [MissionKind::Heavy, MissionKind::Group, MissionKind::Opportunistic] {
        let choices = packages(&job(kind, 3000.0), clear(), 800.0);
        assert!(choices.iter().any(|p| p.iter().any(|(k, _)| *k == Plane::Likho)));
        let have = BTreeMap::from([(Plane::Likho, 2)]);
        assert_eq!(purchase(&choices, &have, |_| 1000.0), None);
    }
    assert!(!MissionKind::Opportunistic.primary());
}

#[test]
fn procurement_compares_metal_not_airframe_count() {
    use std::collections::BTreeMap;
    let choices = vec![vec![(Plane::Raven, 3)], vec![(Plane::Likho, 1)]];
    let cost = |k| if k == Plane::Likho { 2000.0 } else { 300.0 };
    assert_eq!(purchase(&choices, &BTreeMap::new(), cost), Some(Plane::Raven));
    assert_eq!(purchase(&choices, &BTreeMap::from([(Plane::Likho, 1)]), cost), None);
}

#[test]
fn priority_strikes_follow_current_health_and_tough_units_use_existing_fleets() {
    assert!(priority_target(1100.0, 1000.0)); // Lance
    assert!(priority_target(900.0, 1250.0)); // Firewalker
    assert!(priority_target(1600.0, 2045.0)); // Tremor
    assert!(!priority_target(2200.0, 12000.0)); // Cyclops
    assert!(priority_target(600.0, 1800.0));
    assert!(!priority_target(600.0, 1801.0));
    let mut cyclops = enemy(1);
    cyclops.heavy = true;
    cyclops.value = 2200.0;
    cyclops.hp = 12000.0;
    cyclops.max_hp = 12000.0;
    let js = missions(&[cyclops.clone()], Vec3::ZERO, 300);
    assert!(js.iter().all(|j| !j.kind.primary()));
    let choice = packages(&js[0], clear(), 800.0);
    assert!(choice.contains(&vec![(Plane::Raven, 19)]));
    for (hp, primary) in [(6601.0, false), (6600.0, true), (2000.0, true), (12000.0, false)] {
        cyclops.hp = hp;
        let js = missions(&[cyclops.clone()], Vec3::ZERO, 300);
        assert_eq!(js.iter().any(|j| j.kind == MissionKind::Heavy), primary);
        assert_eq!(js.iter().any(|j| j.kind == MissionKind::Opportunistic), !primary);
    }
    cyclops.hp = 2000.0;
    let damaged = missions(&[cyclops], Vec3::ZERO, 300);
    assert_eq!(damaged[0].kind, MissionKind::Heavy);
    assert!(packages(&damaged[0], clear(), 800.0).contains(&vec![(Plane::Raven, 4)]));
    assert!(packages(&damaged[0], clear(), 800.0).contains(&vec![(Plane::Likho, 2)]));
    assert!(damaged.iter().any(|j| j.kind == MissionKind::Assassinate));
    let lance = job(MissionKind::Heavy, 1000.0);
    let mut tougher = lance.clone();
    tougher.hp = 3000.0;
    assert!(lance.score() > tougher.score());
}

#[test]
fn explicit_egress_preserves_bends_and_missing_or_dangerous_return_blocks_launch() {
    let home = Vec3::xz(10.0, 10.0);
    let outbound = vec![Vec3::xz(10.0, 1000.0), Vec3::xz(1000.0, 1000.0)];
    assert_eq!(return_route(home, &outbound), vec![outbound[0], home]);
    let missing = route_risk(outbound[1], &[], |_| 0.0, |_| false);
    assert!(!round_trip(clear(), missing).survives(3000.0, 200.0));
    let unsafe_exit = route_risk(outbound[1], &[home], |_| 10.0, |_| false);
    assert!(!round_trip(clear(), unsafe_exit).survives(3000.0, 200.0));
    let safe_exit = route_risk(outbound[1], &return_route(home, &outbound), |_| 0.0, |_| false);
    assert!(round_trip(clear(), safe_exit).survives(3000.0, 200.0));
}

#[test]
fn route_sampling_checks_between_waypoints_not_just_the_endpoints() {
    let risk = route_risk(
        Vec3::ZERO,
        &[Vec3::xz(384.0, 0.0)],
        |p| if (100.0..160.0).contains(&p.x) { 2.0 } else { 0.0 },
        |p| (100.0..160.0).contains(&p.x),
    );
    assert!(risk.exposure > 0.0);
    assert!(risk.intercepted);
    assert_eq!(risk.aa, 0.0);
}

fn ravager_group(spacing: f32, hp: f32) -> Mission {
    let contacts: Vec<_> = (0..3)
        .map(|id| {
            let mut c = enemy(id);
            c.pos.x += (id - 1) as f32 * spacing;
            c.hp = hp;
            c.max_hp = 2200.0;
            c.value = 250.0;
            c
        })
        .collect();
    missions(&contacts, Vec3::ZERO, 300).into_iter().find(|j| j.kind == MissionKind::Group).unwrap()
}

#[test]
fn one_likho_can_damage_a_ravager_pack_without_killing_it() {
    let job = ravager_group(40.0, 2200.0);
    assert_eq!(job.target, 1); // aim at the middle, not the first contact
    let value = job.damage_value(&[(Plane::Likho, 1)]);
    assert!((440.0..460.0).contains(&value));
    let options = packages(&job, clear(), 800.0);
    assert!(options.contains(&vec![(Plane::Likho, 1)]));
    assert!(options.contains(&vec![(Plane::Phoenix, 1)]));
    assert_eq!(purchase(&options, &std::collections::BTreeMap::from([(Plane::Likho, 1)]), |_| 2000.0), None);
}

#[test]
fn group_packages_require_useful_marginal_damage_and_cap_overkill() {
    let job = ravager_group(40.0, 2200.0);
    let options = job.group_packages(&[Plane::Likho]);
    assert_eq!(options, vec![vec![(Plane::Likho, 2)], vec![(Plane::Likho, 1)]]);
    assert_eq!(job.damage_value(&[(Plane::Likho, 24)]), 750.0);
    let wounded = ravager_group(40.0, 400.0);
    assert_eq!(wounded.damage_value(&[(Plane::Likho, 1)]), 750.0);
    assert_eq!(wounded.group_packages(&[Plane::Likho]), vec![vec![(Plane::Likho, 1)]]);
}

#[test]
fn splash_geometry_and_live_weapon_profile_limit_group_value() {
    let mut job = ravager_group(160.0, 2200.0);
    assert!(job.damage_value(&[(Plane::Likho, 1)]) < 200.0);
    assert!(job.group_packages(&[Plane::Likho]).is_empty());
    job.aim_group(Blast { damage: 4000.0, radius: 400.0, edge: 0.5 });
    assert_eq!(job.target, 1);
    assert!(job.damage_value(&[(Plane::Likho, 1)]) > 700.0);
    assert_eq!(job.group_packages(&[Plane::Likho]), vec![vec![(Plane::Likho, 1)]]);
}

#[test]
fn worthwhile_group_damage_still_respects_air_cover() {
    let job = ravager_group(40.0, 2200.0);
    let light = packages(&job, Risk { aa: 0.3, ..clear() }, 800.0);
    assert!(light.contains(&vec![(Plane::Likho, 1)]));
    assert!(light.iter().flatten().all(|(k, _)| *k == Plane::Likho));
    let intense = Risk { aa: 1.3, ..clear() };
    // Structural candidates are filtered by the airframe's survival budget.
    assert!(!packages(&job, intense, 800.0).is_empty());
    assert!(!intense.survives(2360.0, 270.0));
    assert!(packages(&job, Risk { intercepted: true, ..clear() }, 800.0).is_empty());
}
