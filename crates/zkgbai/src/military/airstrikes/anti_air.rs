//! Likho's guided implosion bomb can catch these large, slow aircraft.
use super::*;

pub(super) fn heavy_air_target(name: &str) -> bool {
    matches!(name, "gunshipheavyskirm" | "gunshipkrow" | "bomberassault")
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
