//! Optional jammers accompany deep thermite strikes without gating launch.
use super::*;
use std::collections::BTreeMap;

impl Mission {
    pub(super) fn optional_escort(&self) -> bool {
        self.kind == MissionKind::Thermite && self.deep
    }
    pub(super) fn flight_safe(&self, plane: Plane, risk: Risk, hp: f32, max_hp: f32, speed: f32) -> bool {
        (self.optional_escort() && plane == Plane::Sparrow) || (risk.survives(hp, speed) && hp >= max_hp * 0.5)
    }
}

/// Keep at most one spare jammer for the fleet, only after every offensive
/// deficit is filled. It never appears in a mission's required package.
pub(super) fn want_spare(deep_mission: bool, have: &BTreeMap<Plane, usize>) -> bool {
    deep_mission
        && have.get(&Plane::Odin).copied().unwrap_or(0) > 0
        && have.get(&Plane::Sparrow).copied().unwrap_or(0) == 0
}

/// Ready, unassigned escorts only. Never wait for a rendezvous; each jammer
/// must already cover its Odin with some room for formation drift.
pub(super) fn pairs(odins: &[(i32, Vec3)], escorts: &[(i32, Vec3, f32)]) -> Vec<(i32, i32)> {
    let mut paired = Vec::new();
    for &(odin, pos) in odins {
        if let Some(&(escort, _, _)) = escorts
            .iter()
            .filter(|(id, at, radius)| {
                !paired.iter().any(|(used, _)| used == id) && *radius > 0.0 && distance(pos, *at) <= radius * 0.75
            })
            .min_by(|a, b| distance(pos, a.1).total_cmp(&distance(pos, b.1)).then(a.0.cmp(&b.0)))
        {
            paired.push((escort, odin));
        }
    }
    paired
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn escorts_are_unique_and_no_rendezvous_is_required() {
        let odins = [(1, Vec3::ZERO), (2, Vec3::xz(100.0, 0.0))];
        assert!(pairs(&odins, &[]).is_empty());
        assert!(pairs(&odins, &[(3, Vec3::xz(1000.0, 0.0), 500.0)]).is_empty());
        assert_eq!(pairs(&odins, &[(3, Vec3::ZERO, 500.0)]), vec![(3, 1)]);
        assert_eq!(pairs(&odins, &[(3, Vec3::ZERO, 500.0), (4, Vec3::xz(100.0, 0.0), 500.0)]), vec![(3, 1), (4, 2)]);
    }
    #[test]
    fn optional_stock_is_bounded_and_counts_orders() {
        let mut have = BTreeMap::new();
        assert!(!want_spare(true, &have));
        have.insert(Plane::Odin, 3);
        assert!(want_spare(true, &have));
        assert!(!want_spare(false, &have));
        have.insert(Plane::Sparrow, 1); // includes queued/building/active escorts
        assert!(!want_spare(true, &have));
    }
}
