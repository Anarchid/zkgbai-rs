//! Gate priorities along ground-warped direct and equidistant lines.
//! These are opportunities, not claims that a constructor can safely take
//! them. The economy must check support and travel before committing.

use crate::paths::Paths;
use crate::strategy::{Analysis, Asset, MAX_HOLD_WIDTH, Side};
use crate::{Pos, TierMap};

const DENSITY_RADIUS: f32 = 900.0;
const TRAVEL_SCALE: f32 = 1400.0;

#[derive(Clone, Debug)]
pub struct GateValue {
    pub choke: usize,
    pub home: usize,
    pub pos: Pos,
    pub width: f32,
    pub score: f32,
}

/// Nearby routes and resources reinforce each other, but a bounded density
/// bonus cannot buy an arbitrarily long trip. Distances include climbs.
pub fn gates(map: &TierMap, analysis: &Analysis, ally: Pos, enemy: Pos, assets: &[Asset]) -> Vec<GateValue> {
    let ours = Paths::new(map, ally, |_| false);
    let theirs = Paths::new(map, enemy, |_| false);
    let Some(span) = ours.distance_to(enemy) else { return Vec::new() };
    let Some(reverse_span) = theirs.distance_to(ally) else { return Vec::new() };
    let typical = assets.iter().map(|a| a.value.max(0.0)).sum::<f32>() / assets.len().max(1) as f32;
    let mut out = Vec::new();
    for c in &map.chokes {
        let da = ours.to_choke(c.id);
        let de = theirs.to_choke(c.id);
        // Include neutral gates exactly on the ground-distance bisector;
        // exclude enemy territory, disconnected gates and rear pockets.
        if !da.is_finite() || !de.is_finite() || da > de || de >= reverse_span {
            continue;
        }
        let Some(home) = c.regions.iter().copied().min_by(|&a, &b| ours.to_region(a).total_cmp(&ours.to_region(b)))
        else {
            continue;
        };
        let width = c.hold_width(home);
        if width > MAX_HOLD_WIDTH || c.regions.len() < 2 {
            continue;
        }
        let near = Paths::new(map, map.setback(c.id, home, 32.0), |_| false);
        let resources: f32 = assets
            .iter()
            .filter(|a| a.side != Side::Enemy)
            .map(|a| {
                let d = near.distance_to(a.pos).unwrap_or(f32::INFINITY);
                let weight = if a.side == Side::Ally { 1.0 } else { 0.5 };
                weight * a.value.max(0.0) / typical.max(1.0) * (1.0 - d / DENSITY_RADIUS).max(0.0)
            })
            .sum();
        let density: f32 = map
            .chokes
            .iter()
            .filter(|n| n.id != c.id)
            .map(|n| {
                let narrow = (1.0
                    - n.regions.iter().map(|&r| n.hold_width(r)).fold(f32::INFINITY, f32::min) / MAX_HOLD_WIDTH)
                    .max(0.0);
                narrow * (1.0 - near.to_choke(n.id) / DENSITY_RADIUS).max(0.0)
            })
            .sum();
        // Keep the previous protected-value cut as a bonus, not an
        // eligibility test: parallel gates can be good shared fire lanes.
        let protects = if analysis.hold.contains(&c.id) { 1.5 } else { 1.0 };
        let score = priority(da, de, span.max(reverse_span), width, resources, density) * protects;
        out.push(GateValue { choke: c.id, home, pos: map.setback(c.id, home, 120.0), width, score });
    }
    out.sort_by(|a, b| b.score.total_cmp(&a.score).then(a.choke.cmp(&b.choke)));
    out
}

fn priority(ally: f32, enemy: f32, span: f32, width: f32, resources: f32, density: f32) -> f32 {
    let scale = (span * 0.2).max(300.0);
    let direct = 1.0 / (1.0 + ((ally + enemy - span).max(0.0) / scale).powi(2));
    let equidistant = 1.0 / (1.0 + ((ally - enemy) / scale).powi(2));
    let openness = 1.0 / (1.0 + width / 200.0);
    let concentration = 1.0 + 2.0 * resources.min(4.0) + 1.5 * density.min(3.0);
    (1.0 + 2.0 * direct + equidistant) * openness * concentration / (1.0 + ally / TRAVEL_SCALE).powi(2)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::strategy::Scenario;
    use crate::testmaps::{at, rooms};

    #[test]
    fn prefers_direct_routes_density_and_narrow_fronts_but_discounts_travel() {
        let score = |a, e, w, r, d| priority(a, e, 2000.0, w, r, d);
        assert!(score(800.0, 1200.0, 100.0, 0.0, 0.0) > score(1600.0, 2000.0, 100.0, 0.0, 0.0));
        assert!(score(800.0, 1200.0, 100.0, 2.0, 2.0) > 4.0 * score(800.0, 1200.0, 100.0, 0.0, 0.0));
        assert!(score(800.0, 1200.0, 100.0, 2.0, 2.0) > score(800.0, 1200.0, 600.0, 2.0, 2.0));
        assert!(
            priority(500.0, 500.0, 1000.0, 100.0, 0.0, 0.0) > 5.0 * priority(4000.0, 4000.0, 8000.0, 100.0, 0.0, 0.0)
        );
    }

    #[test]
    fn considers_gates_without_a_value_cut_but_not_enemy_or_rear_gates() {
        let map = rooms();
        let (ally, enemy) = (at(2.0, 1.0), at(22.0, 1.0));
        let a = Analysis::run(&Scenario { map: &map, ally_base: ally, enemy_base: enemy, assets: &[] });
        assert!(a.hold.is_empty());
        let g = gates(&map, &a, ally, enemy, &[]);
        assert_eq!(g.iter().map(|g| g.choke).collect::<Vec<_>>(), vec![0]);
        assert!(g[0].score.is_finite());
    }

    #[test]
    fn unreachable_enemy_produces_no_fortification_rush() {
        let mut map = rooms();
        map.regions[1].chokes.clear();
        map.regions[2].chokes.retain(|&c| c != 1);
        let (ally, enemy) = (at(2.0, 1.0), at(22.0, 1.0));
        let a = Analysis::default();
        assert!(gates(&map, &a, ally, enemy, &[]).is_empty());
    }
}
