//! Strike destinations must be occupiable, not merely near reachable ground.
use super::*;
use crate::terrain::{move_class, pos, tier_of};
use springai::UnitDef;
use zk_terrain::{TierMap, paths::SNAP};

fn connected_point(map: &TierMap, from: Vec3, to: Vec3) -> bool {
    map.is_passable(pos(to)) && map.reaches_within(pos(from), pos(to), 0.0, SNAP)
}

/// Preserve a legal destination; otherwise find a connected firing position.
/// The radius is weapon reach, never the terrain model's snapping tolerance.
fn approach(from: Vec3, target: Vec3, range: f32, mut legal: impl FnMut(Vec3) -> bool) -> Option<Vec3> {
    if legal(target) {
        return Some(target);
    }
    let bearing = (from.z - target.z).atan2(from.x - target.x);
    let mut best = None;
    for radius in [range * 0.85, range * 0.5, range * 0.2] {
        for i in 0..16 {
            let angle = bearing + i as f32 * std::f32::consts::TAU / 16.0;
            let p = Vec3::xz(target.x + angle.cos() * radius, target.z + angle.sin() * radius);
            if legal(p) && best.is_none_or(|old| distance(from, p) < distance(from, old)) {
                best = Some(p);
            }
        }
    }
    best
}

impl Ai {
    fn strike_walk_test(&self, def: UnitDef, from: Vec3) -> impl Fn(Vec3) -> bool + '_ {
        let engine_map = self.cb.map();
        let (width, height) = ((engine_map.width() * 8) as f32, (engine_map.height() * 8) as f32);
        let class = move_class(def);
        let terrain = tier_of(def).and_then(|tier| self.reach_terrain()?.tier(tier));
        move |to| {
            if !to.x.is_finite() || !to.z.is_finite() || to.x < 0.0 || to.z < 0.0 || to.x >= width || to.z >= height {
                return false;
            }
            // Actual depth remains authoritative while terrain is pending or
            // disabled; use the unit's limits, not its tier's constructor.
            !class.is_some_and(|c| c.blocks(0.0, -engine_map.elevation_at(to.x, to.z)))
                && terrain.is_none_or(|map| connected_point(map, from, to))
        }
    }

    pub(super) fn strike_point_reachable(&self, def: UnitDef, from: Vec3, to: Vec3) -> bool {
        self.strike_walk_test(def, from)(to)
    }

    pub(super) fn strike_target_point(&self, def: UnitDef, from: Vec3, target: Vec3) -> Option<Vec3> {
        approach(from, target, def.max_weapon_range().max(0.0), self.strike_walk_test(def, from)).map(|mut p| {
            p.y = self.cb.map().elevation_at(p.x, p.z);
            p
        })
    }

    pub(super) fn strike_tactic_reaches(&self, tactic: Tactic, from: Vec3, target: Vec3) -> bool {
        self.defs.get(tactic.name()).is_some_and(|d| self.strike_target_point(d, from, target).is_some())
    }

    pub(super) fn strike_members_reach(&self, members: &[i32], target: Vec3) -> bool {
        !members.is_empty()
            && members.iter().all(|id| {
                self.fighters.get(id).is_some_and(|f| {
                    f.is_dead()
                        || f.unit.def().is_some_and(|d| self.strike_target_point(d, f.get_pos(), target).is_some())
                })
            })
    }

    /// Clamp a spread/search position to legal ground near its mission anchor.
    pub(super) fn strike_move_point(&self, u: Unit, desired: Vec3, anchor: Vec3, range: f32) -> Option<Vec3> {
        let def = u.def()?;
        let from = u.pos();
        let legal = self.strike_walk_test(def, from);
        let point = if legal(desired) { Some(desired) } else { approach(from, anchor, range, legal) };
        point.map(|mut p| {
            p.y = self.cb.map().elevation_at(p.x, p.z);
            p
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zk_terrain::{SpringMapData, Terrain, Tier};

    fn coast() -> Terrain {
        let data = SpringMapData {
            map_width: 128,
            map_height: 64,
            slope_width: 64,
            slope_height: 32,
            slope_map: vec![0.0; 64 * 32],
            height_map: (0..128 * 64).map(|i| if (32..96).contains(&(i % 128)) { -100.0 } else { 20.0 }).collect(),
        };
        Terrain::build_with(
            &data,
            &[(Tier::Spider, Tier::Spider.zero_k_class()), (Tier::Hover, Tier::Hover.zero_k_class())],
        )
    }

    #[test]
    fn shoreline_approach_stays_on_connected_dry_ground() {
        let terrain = coast();
        let map = terrain.tier(Tier::Spider).unwrap();
        let from = Vec3::xz(128.0, 256.0);
        let target = Vec3::xz(288.0, 256.0);
        // The old check accepts nearby land but used to command the wet point.
        assert!(map.reaches_within(pos(from), pos(target), 96.0, SNAP));
        assert!(!connected_point(map, from, target));
        let p = approach(from, target, 150.0, |p| connected_point(map, from, p)).unwrap();
        assert!(p.x < 256.0 && distance(p, target) < 150.0);
    }

    #[test]
    fn moving_lance_leaves_flea_reach_but_not_hover_reach() {
        let terrain = coast();
        let from = Vec3::xz(128.0, 256.0);
        for tier in [Tier::Spider, Tier::Hover] {
            let map = terrain.tier(tier).unwrap();
            let reach = |target| approach(from, target, 150.0, |p| connected_point(map, from, p));
            assert!(reach(Vec3::xz(224.0, 256.0)).is_some());
            assert_eq!(reach(Vec3::xz(512.0, 256.0)).is_some(), tier == Tier::Hover);
            // Dry land on the other side is also disconnected for Fleas.
            assert_eq!(reach(Vec3::xz(896.0, 256.0)).is_some(), tier == Tier::Hover);
        }
    }
}
