//! Reactor placement: choose depth inside friendly territory before grid
//! convenience. Pylons can connect a rear reactor; they cannot move one.

use springai::{UnitDef, Vec3};

use crate::Ai;
use crate::util::{NULLPOS, distance};

/// Minimum distance from frontline and hostile metal spots, in elmos.
/// These exceed the graph's 640-elmo frontline paint radius.
fn setback(name: &str) -> f32 {
    if name == "energysingu" { 1800.0 } else { 1200.0 }
}

fn clearance(pos: Vec3, danger: &[Vec3]) -> f32 {
    danger.iter().map(|&p| distance(pos, p)).fold(f32::INFINITY, f32::min)
}

/// Resolve buildability first: the engine can move a seed by 600 elmos.
/// Rank the resulting sites by their nearest danger, with home distance
/// breaking ties. Never fall back to an exposed site when all are rejected.
fn choose_site(
    seeds: impl Iterator<Item = Vec3>,
    danger: &[Vec3],
    minimum: f32,
    home: Vec3,
    mut resolve: impl FnMut(Vec3) -> Option<Vec3>,
) -> Option<Vec3> {
    seeds
        .filter_map(&mut resolve)
        .filter(|p| p.x.is_finite() && p.z.is_finite() && p.x >= 0.0 && p.z >= 0.0)
        .map(|p| (p, clearance(p, danger)))
        .filter(|(_, depth)| *depth >= minimum)
        .max_by(|(a, ad), (b, bd)| ad.total_cmp(bd).then_with(|| distance(*b, home).total_cmp(&distance(*a, home))))
        .map(|(p, _)| p)
}

impl Ai {
    fn eco_reactor_danger(&self) -> Vec<Vec3> {
        self.graph
            .metal_spots
            .iter()
            .filter(|s| s.hostile || s.enemy_shadowed || self.graph.is_spot_front_line(s.index))
            .map(|s| s.position)
            .collect()
    }

    fn eco_reactor_territory_safe(&self, p: Vec3) -> bool {
        p.x.is_finite()
            && p.z.is_finite()
            && p.x >= 0.0
            && p.z >= 0.0
            && self.graph.is_ally_territory(p)
            && !self.graph.is_enemy_territory(p)
            && !self.graph.is_front_line(p)
            && self.war.get_threat(p) <= 0.0
            && self.war.get_porc_threat(p) <= 0.0
    }

    /// Recheck queued reactors if the front moves before construction starts.
    pub(super) fn eco_reactor_site_safe(&self, def: UnitDef, p: Vec3) -> bool {
        self.eco_reactor_territory_safe(p) && clearance(p, &self.eco_reactor_danger()) >= setback(self.defs.name(def))
    }

    pub(super) fn eco_reactor_site(&self, def: UnitDef) -> Option<Vec3> {
        let home = self.graph.get_ally_base();
        let danger = self.eco_reactor_danger();
        // Sample around each friendly anchor too, so an occupied mex/base
        // need not force the engine's nearest result toward the front.
        let anchors = std::iter::once(home).chain(
            self.graph.metal_spots.iter().filter(|s| (s.owned || s.ally_shadowed) && !s.hostile).map(|s| s.position),
        );
        let seeds = anchors.filter(|&p| p != NULLPOS).flat_map(|p| {
            std::iter::once(p).chain((0..8).map(move |i| {
                let angle = i as f32 * std::f32::consts::TAU / 8.0;
                Vec3::xz(p.x + 400.0 * angle.cos(), p.z + 400.0 * angle.sin())
            }))
        });
        let map = self.cb.map();
        let (width, height) = (map.width() as f32 * 8.0, map.height() as f32 * 8.0);
        let minimum = setback(self.defs.name(def));
        choose_site(seeds, &danger, minimum, home, |seed| {
            if seed.x < 0.0
                || seed.z < 0.0
                || seed.x >= width
                || seed.z >= height
                || clearance(seed, &danger) + 600.0 < minimum
            {
                return None;
            }
            let p = self.eco_build_site(def, seed, 0);
            (self.eco_reactor_territory_safe(p)
                && clearance(p, &danger) >= minimum
                && self.eco_build_check(def, p, 0)
                && map.is_possible_to_build_at(def, p, 0))
            .then_some(p)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefers_depth_over_a_nearer_grid_site() {
        let rear = Vec3::xz(800.0, 1000.0);
        let grid = Vec3::xz(1800.0, 1000.0);
        let danger = [Vec3::xz(4000.0, 1000.0)];
        assert_eq!(choose_site([grid, rear].into_iter(), &danger, setback("energysingu"), grid, Some), Some(rear));
    }

    #[test]
    fn checks_the_engine_result_not_the_safe_seed() {
        let seed = Vec3::xz(1000.0, 1000.0);
        let shifted = Vec3::xz(1600.0, 1000.0);
        let danger = [Vec3::xz(3000.0, 1000.0)];
        assert!(clearance(seed, &danger) > setback("energysingu"));
        assert!(choose_site([seed].into_iter(), &danger, setback("energysingu"), seed, |_| Some(shifted)).is_none());
        assert_eq!(
            choose_site([seed].into_iter(), &danger, setback("energyfusion"), seed, |_| Some(shifted)),
            Some(shifted)
        );
    }

    #[test]
    fn nearest_flank_matters_even_far_from_enemy_center() {
        let exposed = Vec3::xz(800.0, 1000.0);
        let sheltered = Vec3::xz(1500.0, 3000.0);
        let danger = [Vec3::xz(5000.0, 1000.0), Vec3::xz(800.0, 200.0)];
        assert_eq!(choose_site([exposed, sheltered].into_iter(), &danger, 1800.0, exposed, Some), Some(sheltered));
    }

    #[test]
    fn no_safe_site_means_wait_instead_of_a_frontline_fallback() {
        let site = Vec3::xz(1500.0, 1000.0);
        let danger = [Vec3::xz(2500.0, 1000.0)];
        assert!(choose_site([site].into_iter(), &danger, 1800.0, site, Some).is_none());
        assert!(choose_site([site].into_iter(), &[], 1800.0, site, |_| None).is_none());
        assert!(choose_site([site].into_iter(), &[], 1800.0, site, |_| Some(Vec3::xz(-1.0, 0.0))).is_none());
    }

    #[test]
    fn without_a_known_front_prefers_home_and_still_checks_buildability() {
        let home = Vec3::xz(800.0, 1000.0);
        let far = Vec3::xz(1800.0, 1000.0);
        assert_eq!(choose_site([far, home].into_iter(), &[], 1800.0, home, Some), Some(home));
        assert_eq!(choose_site([far, home].into_iter(), &[], 1800.0, home, |p| (p != home).then_some(p)), Some(far));
    }
}
