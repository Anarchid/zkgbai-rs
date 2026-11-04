//! A Dominatrix's bounty includes the metal held in its known captives.
use super::*;
use crate::military::enemy::Enemy;

pub(super) fn values<'a>(enemies: impl Iterator<Item = &'a Enemy>, metal: springai::Resource) -> BTreeMap<i32, f32> {
    let mut values = BTreeMap::new();
    for enemy in enemies.filter(|e| e.identified && !e.is_dead) {
        let Some(controller) = enemy.capture_controller() else { continue };
        let value = enemy.ud.map_or(enemy.value, |d| d.cost(metal));
        if value.is_finite() && value > 0.0 {
            *values.entry(controller).or_default() += value;
        }
    }
    values
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::ffi::{CStr, c_char};

    thread_local! {
        static OWNERS: RefCell<BTreeMap<i32, f32>> = RefCell::new(BTreeMap::new());
    }

    fn fixture() -> Ai {
        unsafe extern "C" fn no_def(_: i32, _: i32) -> i32 {
            -1
        }
        unsafe extern "C" fn rules(_: i32, id: i32, name: *const c_char, default: f32) -> f32 {
            assert_eq!(unsafe { CStr::from_ptr(name) }, c"capture_controller");
            OWNERS.with_borrow(|owners| owners.get(&id).copied().unwrap_or(default))
        }
        OWNERS.with_borrow_mut(|owners| *owners = BTreeMap::from([(10, 0.0), (11, 0.0), (12, 1.0)]));
        let mut ai = crate::economy::test_ai_with(|cb| {
            cb.Unit_getDef = Some(no_def);
            cb.Unit_getRulesParamFloat = Some(rules);
        });
        for (id, name, value) in [
            (0, "vehcapture", 420.0),
            (1, "vehcapture", 420.0),
            (2, "cloaksnipe", 750.0),
            (10, "cloakraid", 65.0),
            (11, "tankassault", 850.0),
            (12, "cloakraid", 65.0),
        ] {
            let mut enemy = Enemy::new(Unit::from_id(ai.cb, id).unwrap(), value);
            enemy.def_name = name.into();
            enemy.value = value;
            enemy.identified = true;
            enemy.visible = true;
            enemy.position = Vec3::xz(1000.0, 1000.0);
            ai.war.targets.insert(id, enemy);
        }
        ai
    }

    #[test]
    fn captives_raise_dominatrix_priority_without_inflating_wave_size() {
        let ai = fixture();
        let jobs = ai.strike_jobs(JOB_MEMORY);
        assert_eq!(jobs.iter().map(|j| j.id.unwrap()).collect::<Vec<_>>(), vec![0, 2, 1]);
        assert_eq!(jobs[0].value, 420.0 + 65.0 + 850.0);
        assert_eq!(jobs[2].value, 420.0 + 65.0);
        assert_eq!(jobs[0].prey, Prey::Captor);
        for tactic in [Tactic::Flea, Tactic::Dart] {
            assert!(jobs[0].prey.allows(tactic, false));
            assert!(!jobs[0].prey.needs_stun(tactic));
            assert!(jobs[0].production_pays(tactic, 40.0));
            assert_eq!(jobs[0].size(tactic, 40.0), 20);
        }
        for tactic in [Tactic::Scythe, Tactic::Halberd, Tactic::Dirtbag] {
            assert!(!Prey::Captor.can_requisition(tactic));
        }
    }

    #[test]
    fn bounty_tracks_current_known_ownership_and_forgets_dead_or_removed_captives() {
        let mut ai = fixture();
        assert_eq!(values(ai.war.targets.values(), ai.metal)[&0], 915.0);
        ai.war.targets.get_mut(&11).unwrap().visible = false;
        OWNERS.with_borrow_mut(|owners| {
            owners.insert(11, -1.0);
        });
        assert_eq!(values(ai.war.targets.values(), ai.metal)[&0], 915.0); // preserve observed link through fog
        ai.war.targets.get_mut(&11).unwrap().visible = true;
        assert_eq!(values(ai.war.targets.values(), ai.metal)[&0], 65.0); // observed release
        OWNERS.with_borrow_mut(|owners| {
            owners.insert(10, 1.0);
        });
        assert_eq!(values(ai.war.targets.values(), ai.metal), BTreeMap::from([(1, 130.0)]));
        ai.war.targets.get_mut(&10).unwrap().is_dead = true;
        assert_eq!(values(ai.war.targets.values(), ai.metal), BTreeMap::from([(1, 65.0)]));
        ai.war.targets.remove(&12);
        assert!(values(ai.war.targets.values(), ai.metal).is_empty());
        assert!(ai.strike_jobs(JOB_MEMORY).iter().filter(|j| j.prey == Prey::Captor).all(|j| j.value == 420.0));
    }
}
