//! A hurt Crab must survive leaving its stationary armor before we move it.
//! Pure synchronous judgement: no new cache, commitment or checkpoint state.
//! Existing retreat ownership remains in place; every refused order is retried
//! on the next retreat tick.

use springai::{Unit, Vec3};

use crate::Ai;
use crate::military::fightsim::UnitFate;

const HORIZON: i32 = 15 * 30;
// Raw probabilities retain the predictor's damage variance; the calibrated
// probabilities cannot express safe escape (see fightsim::local).
const MAX_DEATH_RISK: f32 = 0.1;

fn survives(fate: Option<&UnitFate>) -> bool {
    fate.is_some_and(|f| !f.dead && f.hp_left > 0.0 && f.p_raw <= MAX_DEATH_RISK)
}

impl Ai {
    pub(super) fn crab_retreat_safe(&mut self, unit: Unit, path: &[Vec3]) -> bool {
        if self.defs.name_of(unit) != "spidercrabe" {
            return true;
        }
        let Some(local) = self.local_retreat_fight(unit, HORIZON) else { return false };
        // An unidentified contact or an unmodelled battle is not a clearance.
        if local.unknown {
            return false;
        }
        if local.theirs.is_empty() {
            return true;
        }
        let Some(model) = self.local_model() else { return false };
        let path: Vec<_> = path.iter().map(|p| (p.x, p.z)).collect();
        let paralysis = [(0, unit.paralyze_damage() / unit.max_health().max(1.0))];
        let fates = model.retreat_fates(&local.snap, HORIZON, &path, &paralysis);
        survives(fates.first())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::military::{Coward, Enemy};

    #[test]
    fn crab_requires_a_living_low_risk_forecast() {
        let safe = UnitFate { hp_left: 100.0, p_raw: 0.05, ..Default::default() };
        assert!(survives(Some(&safe)));
        assert!(!survives(None));
        assert!(!survives(Some(&UnitFate { dead: true, ..safe })));
        assert!(!survives(Some(&UnitFate { hp_left: 0.0, ..safe })));
        assert!(!survives(Some(&UnitFate { p_raw: 0.2, ..safe })));
        assert!(!survives(Some(&UnitFate { p_raw: f32::NAN, ..safe })));
    }

    #[test]
    fn crab_waits_for_unknown_contacts_and_rechecks_when_they_clear() {
        unsafe extern "C" fn def(_: i32, id: i32) -> i32 {
            if id == 1 { 1 } else { -1 }
        }
        unsafe extern "C" fn zero(_: i32, _: i32) -> f32 {
            0.0
        }
        unsafe extern "C" fn max_health(_: i32, _: i32) -> f32 {
            4000.0
        }
        unsafe extern "C" fn no(_: i32, _: i32) -> bool {
            false
        }
        let mut ai = crate::military::roles::retirement_tests::raider_test_ai_with(c"spidercrabe", |t| {
            t.Unit_getDef = Some(def);
            t.Unit_getParalyzeDamage = Some(zero);
            t.Unit_getCaptureProgress = Some(zero);
            t.Unit_getMaxHealth = Some(max_health);
            t.UnitDef_getHealth = Some(max_health);
            t.UnitDef_getRadius = Some(zero);
            t.UnitDef_isAbleToMove = Some(no);
            t.UnitDef_isBuilder = Some(no);
        });
        let unit = Unit::from_id(ai.cb, 1).unwrap();
        let mut coward = Coward::new(unit, unit.def().unwrap(), "spidercrabe", 0.5, ai.team_id);
        assert!(coward.should_retreat());
        ai.retreat_handler.cowards.insert(1, coward);
        ai.retreat_handler.retreating_units.insert(1);
        ai.retreat_handler.initialized = true;
        let path = [Vec3::xz(50.0, 0.0)];
        assert!(ai.crab_retreat_safe(unit, &path));
        ai.war.targets.insert(2, Enemy::new(Unit::from_id(ai.cb, 2).unwrap(), 0.0));
        assert!(!ai.crab_retreat_safe(unit, &path));
        assert!(ai.retreat_handler.is_retreating(1));
        assert!(ai.retreat_handler.coward(1).unwrap().is_retreating);
        ai.war.targets.clear();
        assert!(ai.crab_retreat_safe(unit, &path), "clearance must be re-evaluated, not latched forever");
    }
}
