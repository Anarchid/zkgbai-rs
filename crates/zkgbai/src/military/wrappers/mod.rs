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
            "extra_damage" => shot += v,                                   // emp
            "impulse" => shot += v / 10.0,                                 // impulse
            "timeslow_damagefactor" => shot += (shot * v).min(shot / 2.0), // slow
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
