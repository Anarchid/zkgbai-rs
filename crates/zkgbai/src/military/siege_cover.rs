//! Siege guns advance with a screen. Target worth must not buy permission
//! to leave it, but a gun assembling behind the front may join better cover.

use springai::Vec3;

use crate::{
    Ai,
    util::{NULLPOS, distance},
};

/// Threat-map power, not metal. Nearby enemy concentrations and measured
/// fight density both raise the screen needed to leave a stronger position.
fn required_cover(enemy: f32, density: f32) -> f32 {
    400.0 * density + enemy * (0.25 + 0.25 * density)
}

fn supported(from: f32, to: f32, enemy: f32, density: f32) -> bool {
    to >= required_cover(enemy, density) || to > from + 1.0
}

fn screen_power(p: Vec3, friendly: f32, own: Option<(Vec3, f32)>) -> f32 {
    let own = own.map_or(0.0, |(center, power)| {
        let (x, z) = super::cell(p);
        let (cx, cz) = super::cell(center);
        let (dx, dz) = (x - cx, z - cz);
        // Match the radius-10 squad circle painted by war_update.
        if dx.abs() < 10 && dz.abs() < 10 && dx * dx + dz * dz <= 100 { power } else { 0.0 }
    });
    (friendly - own).max(0.0)
}

fn supported_route(from: Vec3, to: Vec3, enemy: f32, density: f32, cover: impl Fn(Vec3) -> f32) -> bool {
    let (start, end) = (cover(from), cover(to));
    if !supported(start, end, enemy, density) {
        return false;
    }
    // Joining a stronger screen must work from the rear, even across a
    // quiet gap between the base's defenses and the army at the front.
    if end > start + 1.0 {
        return true;
    }
    let floor = 0.5 * start.min(end);
    let steps = (distance(from, to) / 100.0).ceil().max(1.0) as usize;
    (1..steps).all(|i| {
        let f = i as f32 / steps as f32;
        cover(Vec3::xz(from.x + f * (to.x - from.x), from.z + f * (to.z - from.z))) >= floor
    })
}

/// A modest production increase at a dense front; zero settings remain off.
pub(crate) fn production_scale(density: f32) -> f32 {
    1.0 + 0.5 * density.clamp(0.0, 1.0)
}

/// Raise both the chance to order a gun and the standing artillery budget.
pub(crate) fn production_chance(base: f32, have: f32, targets: f32, density: f32) -> f32 {
    if targets <= 0.0 {
        return 0.0;
    }
    let scale = production_scale(density);
    (base * scale * (1.0 - have / (targets * scale)).max(0.0)).clamp(0.0, 1.0)
}

impl Ai {
    /// `own` is the artillery core's contribution to the friendly map,
    /// centred where its squad is painted. Guns cannot screen themselves;
    /// escorts and other friendly units still count.
    pub(crate) fn siege_advance_supported(&self, from: Vec3, to: Vec3, own: Option<(Vec3, f32)>) -> bool {
        if from == NULLPOS || to == NULLPOS {
            return false;
        }
        let cover = |p| screen_power(p, self.war.get_total_friendly_power(p), own);
        // Sample around the firing position as well as on it: artillery
        // stands outside weapon reach, where direct enemy threat is often 0.
        let enemy = [0.0, 600.0, 1200.0]
            .into_iter()
            .flat_map(|r| {
                (0..8).map(move |i| {
                    let a = i as f32 * std::f32::consts::FRAC_PI_4;
                    let p = Vec3::xz(to.x + r * a.cos(), to.z + r * a.sin());
                    let (x, z) = super::cell(p);
                    let grid = &self.war.threat_graphics;
                    if x >= 0 && z >= 0 && x < grid.width && z < grid.height {
                        self.war.get_enemy_power(p)
                    } else {
                        0.0
                    }
                })
            })
            .fold(0.0, f32::max);
        supported_route(from, to, enemy, self.war.front.escalation(), cover)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn leaving_the_screen_requires_cover_proportional_to_the_front() {
        assert!(!supported(1000.0, 0.0, 800.0, 0.0));
        assert!(supported(1000.0, 300.0, 800.0, 0.0));
        assert!(!supported(1000.0, 300.0, 800.0, 1.0));
        assert!(supported(1200.0, 900.0, 800.0, 1.0));
        assert!(!supported(1200.0, 900.0, 1600.0, 1.0));
    }

    #[test]
    fn guns_can_join_a_stronger_screen_but_not_march_between_empty_zones() {
        assert!(supported(0.0, 300.0, 2000.0, 1.0));
        assert!(supported(300.0, 600.0, 2000.0, 1.0));
        assert!(!supported(0.0, 0.0, 2000.0, 1.0));
        assert!(!supported(300.0, 300.0, 2000.0, 1.0));
        assert!(supported(0.0, 0.0, 0.0, 0.0));
    }

    #[test]
    fn guns_do_not_screen_themselves_but_escorts_and_distant_allies_do() {
        let p = Vec3::xz(1000.0, 1000.0);
        let own = Some((p, 600.0));
        assert_eq!(screen_power(p, 600.0, own), 0.0);
        assert_eq!(screen_power(p, 900.0, own), 300.0);
        let away = Vec3::xz(2000.0, 1000.0);
        assert_eq!(screen_power(away, 900.0, own), 900.0);
        assert_eq!(screen_power(p, 100.0, own), 0.0);
    }

    #[test]
    fn siege_production_grows_gradually_and_stays_bounded() {
        assert_eq!(production_scale(0.0), 1.0);
        assert_eq!(production_scale(0.5), 1.25);
        assert_eq!(production_scale(1.0), 1.5);
        assert_eq!(production_scale(2.0), 1.5);
        assert_eq!(production_chance(0.1, 0.0, 0.0, 1.0), 0.0);
        assert_eq!(production_chance(0.1, 2000.0, 2000.0, 0.0), 0.0);
        assert!(production_chance(0.1, 2000.0, 2000.0, 1.0) > 0.0);
        assert_eq!(production_chance(0.1, 3000.0, 2000.0, 1.0), 0.0);
        assert_eq!(production_chance(0.0, 0.0, 2000.0, 1.0), 0.0);
        assert_eq!(production_chance(1.0, 0.0, 2000.0, 1.0), 1.0);
    }

    #[test]
    fn joining_the_front_can_cross_a_gap_but_lateral_moves_keep_the_screen() {
        let (from, to) = (Vec3::xz(100.0, 100.0), Vec3::xz(1100.0, 100.0));
        let cover = |p: Vec3| if p.x < 300.0 || p.x > 900.0 { 1000.0 } else { 0.0 };
        assert!(!supported_route(from, to, 800.0, 1.0, cover));
        assert!(supported_route(from, to, 800.0, 1.0, |_| 1000.0));
        let joining = |p: Vec3| {
            if p.x < 300.0 {
                200.0
            } else if p.x > 900.0 {
                1000.0
            } else {
                0.0
            }
        };
        assert!(supported_route(from, to, 800.0, 1.0, joining));
    }
}
