//! Per-unit and per-group controllers.

pub mod bomber;
pub mod coward;
pub mod fighter;
pub mod porc;
pub mod squad;

pub use bomber::Bomber;
pub use coward::Coward;
pub use fighter::{Fighter, FighterKind, FighterStore};
pub use porc::Porc;
pub use squad::{Squad, SquadId, SquadKind, Squads};

use springai::WeaponDef;

/// Sustained damage estimate of one weapon: burst damage (against armor
/// class 1) adjusted for Zero-K special effects, divided by reload time.
///
/// `paralyzer_bonus` doubles paralyzer weapons (static defenses count EMP
/// higher). Used by fighters, porc and enemy power estimates.
pub fn weapon_power(wd: WeaponDef, paralyzer_bonus: bool) -> f32 {
    let base = wd.damage_types().get(1).copied().unwrap_or(0.0);
    let mut shot = base * wd.projectiles_per_shot() as f32 * wd.salvo_size() as f32;
    // The original iterated a sorted std::map; the effects compound, so keep
    // that order.
    let mut params = wd.custom_params();
    params.sort();
    for (k, v) in params {
        let v: f32 = v.parse().unwrap_or(0.0);
        match k.as_str() {
            "extra_damage" => shot += v,                            // emp
            "impulse" => shot += v / 10.0,                          // impulse
            "timeslow_damagefactor" => shot *= 1.0 + slow_bonus(v), // slow
            "disarmDamageOnly" => shot /= 10.0,
            "setunitsonfire" => shot *= 2.0,
            _ => {}
        }
    }
    if paralyzer_bonus && wd.is_paralyzer() {
        shot *= 2.0;
    }
    shot *= 1.0 + wd.area_of_effect() / 100.0;
    let reload = wd.reload();
    if reload > 0.0 { shot / reload } else { shot }
}

/// Worth of slow damage against real damage: a slowed unit moves and
/// fires slower, down to half speed, so slow counts but saturates.
const SLOW_WORTH: f32 = 0.6;
/// Slow at most doubles a weapon's power (the target's slow is capped).
const SLOW_CAP: f32 = 1.0;

/// What a disruptor's slow (`timeslow_damagefactor`: slow damage per point
/// of real damage) adds to its power, as a share of its real damage. It
/// used to be capped at a half, so a Bolas (1.667) looked no more
/// dangerous than a weapon with a third of its slow.
fn slow_bonus(factor: f32) -> f32 {
    (SLOW_WORTH * factor.max(0.0)).min(SLOW_CAP)
}

#[cfg(test)]
mod tests {
    use super::slow_bonus;

    #[test]
    fn slow_counts_more_but_saturates() {
        // Bolas: 1.667 slow per damage, now doubled (it was x1.5).
        assert!((slow_bonus(1.667) - 1.0).abs() < 0.01);
        // Outlaw, Moderator, Limpet: capped at double.
        assert_eq!(slow_bonus(2.5), 1.0);
        assert_eq!(slow_bonus(10.0), 1.0);
        // A weak slow still counts in proportion.
        assert!((slow_bonus(0.5) - 0.3).abs() < 1e-6);
        assert_eq!(slow_bonus(0.0), 0.0);
    }
}
