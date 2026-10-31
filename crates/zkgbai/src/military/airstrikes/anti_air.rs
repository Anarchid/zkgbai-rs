//! Likho's guided implosion bomb can catch these large, slow aircraft.
use super::*;

pub(super) fn heavy_air_target(name: &str) -> bool {
    matches!(name, "gunshipheavyskirm" | "gunshipkrow" | "bomberassault")
}

/// Heavy aircraft this close to a HeavyAir quarry fly with it (a Nimbus
/// covers 600 elmos): part of the target, not an escort that vetoes it.
pub(super) const FLOCK: f32 = 600.0;

pub(super) fn flock_mate(quarry: Vec3, pos: Vec3, name: &str) -> bool {
    heavy_air_target(name) && crate::util::distance(quarry, pos) < FLOCK
}

impl Mission {
    pub(super) fn heavy_air_packages(&self) -> Vec<Vec<(Plane, usize)>> {
        let mut choices = Vec::new();
        let mut previous = 0.0;
        for n in 1..=MAX_WING {
            let package = vec![(Plane::Likho, n)];
            let value = self.damage_value(&package);
            // A Krow need not be killed in one enormous salvo. Launch useful
            // partial strikes, preferring more ready aircraft when they help.
            if value - previous < 250.0 {
                break;
            }
            previous = value;
            choices.push(package);
        }
        choices.reverse();
        choices
    }
}

#[cfg(test)]
mod flock_tests {
    use super::*;

    #[test]
    fn heavy_aircraft_near_the_quarry_are_its_flock_fighters_are_not() {
        let q = Vec3::xz(1000.0, 1000.0);
        assert!(flock_mate(q, Vec3::xz(1100.0, 1050.0), "gunshipheavyskirm"));
        assert!(flock_mate(q, Vec3::xz(1400.0, 1300.0), "gunshipkrow"));
        assert!(!flock_mate(q, Vec3::xz(1700.0, 1000.0), "gunshipheavyskirm"));
        assert!(!flock_mate(q, Vec3::xz(1100.0, 1000.0), "planeheavyfighter"));
        assert!(!flock_mate(q, Vec3::xz(1100.0, 1000.0), "gunshipaa"));
    }
}
