//! Expensive targets whose current HP makes an assassination economical.
use super::*;
use crate::military::enemy::Enemy;

fn qualifies(hp: f32, cost: f32, riot: bool, lance_hp: f32, lance_cost: f32) -> bool {
    !riot
        && [hp, cost, lance_hp, lance_cost].into_iter().all(|x| x.is_finite() && x > 0.0)
        && cost >= 600.0
        && hp / cost <= 0.5 * lance_hp / lance_cost
}

impl Ai {
    pub(super) fn strike_fragile(&self, enemy: &Enemy) -> bool {
        // Wounds must still be observable: hidden units may have repaired.
        // Commanders retain their dedicated opportunity/procurement policy.
        if self.dev_off("strikefragile") || enemy.is_dead || !enemy.visible || enemy.is_com || enemy.can_fly() {
            return false;
        }
        let (Some(def), Some(lance)) = (enemy.ud, self.defs.get("hoverarty")) else { return false };
        qualifies(enemy.unit.health(), def.cost(self.metal), enemy.is_riot, lance.health(), lance.cost(self.metal))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expensive_non_riots_qualify_at_half_a_healthy_lances_hp_per_metal() {
        // Test reference: 400 HP / 1000 metal. The live policy reads the Lance definition.
        assert!(qualifies(120.0, 600.0, false, 400.0, 1000.0));
        assert!(!qualifies(120.1, 600.0, false, 400.0, 1000.0));
        assert!(!qualifies(1.0, 599.0, false, 400.0, 1000.0));
        assert!(!qualifies(1.0, 2000.0, true, 400.0, 1000.0));
        assert!(qualifies(400.0, 2000.0, false, 400.0, 1000.0));
        assert!(!qualifies(6000.0, 2000.0, false, 400.0, 1000.0)); // repaired heavy
        assert!(!qualifies(0.0, 2000.0, false, 400.0, 1000.0));
        assert!(!qualifies(f32::NAN, 2000.0, false, 400.0, 1000.0));
        assert!(!qualifies(1.0, f32::INFINITY, false, 400.0, 1000.0));
        assert!(!qualifies(1.0, 2000.0, false, 400.0, 0.0));
        assert!(qualifies(200.0, 2000.0, false, 200.0, 1000.0));
        assert!(!qualifies(201.0, 2000.0, false, 200.0, 1000.0));
    }

    #[test]
    fn fragile_targets_enable_unstunned_assassinations_by_default() {
        assert!(!crate::ai::OFF_BY_DEFAULT.contains(&"strikefragile"));
        for tactic in [Tactic::Halberd, Tactic::Dirtbag, Tactic::Scythe, Tactic::Flea, Tactic::Dart] {
            assert!(Prey::Fragile.allows(tactic, false));
            assert!(!Prey::Fragile.needs_stun(tactic));
            assert!(Prey::Fragile.can_requisition(tactic));
        }
    }
}
