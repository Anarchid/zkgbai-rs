//! Enemy shields in the way of our bombs.
//!
//! Every Zero-K shield (`shieldInterceptType` 3) stops every bomb our planes
//! drop: Likho implosion bombs, Raven guided bombs, Phoenix napalm and Odin
//! thermite. Under the default `shield_merge` mode a shield takes a bomb only
//! while its charge exceeds the bomb's shield damage, and then loses that much;
//! a weaker shield lets the bomb through untouched and keeps its charge
//! (`weapon_shield_merge.lua`, Recoil `CPlasmaRepulser::IncomingProjectile`).
//! So an Aspis (3600) takes one Likho bomb (2000), four Raven bombs (800), 90
//! napalm bombs (40) and practically all thermite (2), while a Thug (1200)
//! takes no Likho bomb at all. Shield links trade charge too slowly to matter
//! within one pass and are ignored.
//!
//! A stopped bomb bursts on the bubble, at most its radius from the ground
//! under the carrier. A bubble narrower than the blast (a Thug's 90 against
//! napalm's 160) leaves the units beneath inside the burst, so only bubbles at
//! least as wide as a weapon's blast protect against it.
use super::Contact;
use crate::util::distance;
use springai::Vec3;

/// Thermite's shield damage (bomberassault `Thermite Bomb`, damage 2): any
/// charged bubble eats an Odin's whole payload.
pub(super) const THERMITE_DAMAGE: f32 = 2.0;
pub(super) const THERMITE_RADIUS: f32 = 20.0;
/// Raven's guided bomb: 800 damage, area of effect 32.
pub(super) const RAVEN_RADIUS: f32 = 16.0;
/// Bombs absorbed beyond this are as good as all of them.
const MAX_SOAK: usize = 64;

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Shield {
    pub pos: Vec3,
    pub radius: f32,
    pub charge: f32,
}

impl Shield {
    pub(super) fn covers(&self, p: Vec3) -> bool {
        distance(self.pos, p) < self.radius
    }
}

/// The charged shields of known enemies.
pub(super) fn carriers(contacts: &[Contact]) -> Vec<Shield> {
    contacts
        .iter()
        .filter(|c| c.known && !c.air)
        .filter_map(|c| c.shield.map(|(radius, charge)| Shield { pos: c.pos, radius, charge }))
        .filter(|s| s.charge > 0.0)
        .collect()
}

/// Of `carriers`, those whose bubbles cover any of `points`.
pub(super) fn over(carriers: &[Shield], points: &[Vec3]) -> Vec<Shield> {
    carriers.iter().filter(|s| points.iter().any(|p| s.covers(*p))).copied().collect()
}

/// The bubble that takes a bomb of this shield damage and blast radius aimed
/// at `aim`: the strongest one over it that still can.
pub(super) fn guard(charge: &[f32], shields: &[Shield], aim: Vec3, damage: f32, radius: f32) -> Option<usize> {
    shields
        .iter()
        .enumerate()
        .filter(|(i, s)| s.radius >= radius && charge[*i] > damage && s.covers(aim))
        .max_by(|a, b| charge[a.0].total_cmp(&charge[b.0]))
        .map(|(i, _)| i)
}

/// One bomb at `aim`: true when a shield takes it (and loses the charge).
pub(super) fn stop(charge: &mut [f32], shields: &[Shield], aim: Vec3, damage: f32, radius: f32) -> bool {
    let Some(i) = guard(charge, shields, aim, damage, radius) else { return false };
    charge[i] -= damage;
    true
}

/// Bombs the shields over `aim` take before one gets through.
pub(super) fn soak(shields: &[Shield], aim: Vec3, damage: f32, radius: f32) -> usize {
    let mut charge: Vec<f32> = shields.iter().map(|s| s.charge).collect();
    let mut n = 0;
    while n < MAX_SOAK && stop(&mut charge, shields, aim, damage, radius) {
        n += 1;
    }
    n
}

#[cfg(test)]
mod tests {
    use super::*;

    fn aspis() -> Shield {
        Shield { pos: Vec3::ZERO, radius: 350.0, charge: 3600.0 }
    }

    #[test]
    fn aspis_takes_one_likho_four_ravens_and_all_thermite() {
        let s = [aspis()];
        assert_eq!(soak(&s, Vec3::xz(100.0, 0.0), 2000.0, 96.0), 1);
        assert_eq!(soak(&s, Vec3::xz(100.0, 0.0), 800.0, RAVEN_RADIUS), 4);
        assert_eq!(soak(&s, Vec3::xz(100.0, 0.0), THERMITE_DAMAGE, THERMITE_RADIUS), MAX_SOAK);
        // Outside the bubble nothing is stopped.
        assert_eq!(soak(&s, Vec3::xz(400.0, 0.0), 2000.0, 96.0), 0);
    }

    #[test]
    fn weak_or_narrow_bubbles_let_bombs_through() {
        let thug = [Shield { pos: Vec3::ZERO, radius: 90.0, charge: 1200.0 }];
        // Too weak for an implosion bomb, too narrow for napalm.
        assert_eq!(soak(&thug, Vec3::ZERO, 2000.0, 96.0), 0);
        assert_eq!(soak(&thug, Vec3::ZERO, 40.0, 160.0), 0);
        // A Raven bomb is narrow and weaker than its charge.
        assert_eq!(soak(&thug, Vec3::ZERO, 800.0, RAVEN_RADIUS), 1);
    }

    #[test]
    fn overlapping_bubbles_each_take_their_share() {
        let s = [aspis(), Shield { pos: Vec3::xz(200.0, 0.0), ..aspis() }];
        assert_eq!(soak(&s, Vec3::xz(100.0, 0.0), 2000.0, 96.0), 2);
    }
}
