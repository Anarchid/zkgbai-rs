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

/// Seconds over which a tube weapon's rate of fire is counted, from loaded
/// (the fight predictor's live horizon).
const TUBE_WINDOW: f32 = 10.0;

/// Shots per second over a weapon's first [`TUBE_WINDOW`] seconds from
/// loaded: 1 / reload for most weapons. Tube weapons (ZK scriptReload.lua:
/// customParams `script_burst` tubes, each reloading `script_reload` s after
/// it fires; the engine reload only spaces the tubes) fire far less: a
/// Picket 3 in 10 s, not 9; a Hacksaw 2, not 50.
pub fn shot_rate(wd: WeaponDef) -> f32 {
    rate_from(wd.reload(), &wd.custom_params())
}

fn rate_from(reload: f32, params: &[(String, String)]) -> f32 {
    let base = if reload > 0.0 { 1.0 / reload } else { 1.0 };
    let param = |key: &str| params.iter().find(|(k, _)| k == key).and_then(|(_, v)| v.parse::<f32>().ok());
    let Some(tube_reload) = param("script_reload").filter(|r| *r > 0.0) else { return base };
    let tubes = param("script_burst").unwrap_or(1.0).max(1.0);
    base.min(tubes * (TUBE_WINDOW / tube_reload).ceil() / TUBE_WINDOW)
}

/// Sustained damage estimate of one weapon: burst damage (against armor
/// class 1) adjusted for Zero-K special effects, times its [`shot_rate`].
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
    let rate = rate_from(wd.reload(), &params);
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
    shot * rate
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
    fn tube_weapons_fire_at_their_tubes_rate() {
        use super::rate_from;
        let tubes = |reload: &str, burst: Option<&str>| {
            let mut p = vec![("script_reload".to_owned(), reload.to_owned())];
            if let Some(b) = burst {
                p.push(("script_burst".to_owned(), b.to_owned()));
            }
            p
        };
        let near = |a: f32, b: f32| (a - b).abs() < 1e-4;
        // Picket: 3 tubes of 12.5 s, 1.2 s apart -> 3 shots in 10 s, not 9.
        assert!(near(rate_from(1.2, &tubes("12.5", Some("3"))), 0.3));
        // Ogre: 2 tubes of 2.3 s -> 10 shots in 10 s, not 34.
        assert!(near(rate_from(0.3, &tubes("2.3", Some("2"))), 1.0));
        // Hacksaw: 2 tubes of 15 s -> 2 shots, not 50.
        assert!(near(rate_from(0.2, &tubes("15", Some("2"))), 0.2));
        // Disco Rave Party guns: no burst given = one tube.
        assert!(near(rate_from(1.0, &tubes("6", None)), 0.2));
        // Everything else: the engine reload.
        assert!(near(rate_from(1.5, &[]), 1.0 / 1.5));
        assert!(near(rate_from(0.0, &[]), 1.0));
    }

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
