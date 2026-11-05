//! Exploit exposed enemy Lances with enough bombers to keep striking while
//! another wing rearms: four Ravens OR one Likho per Lance, without a cap.

use super::*;

#[derive(Default)]
pub(super) struct LanceReserve {
    targets: BTreeSet<i32>,
    raven_only: usize,
    likho_only: usize,
    either: usize,
}

impl LanceReserve {
    /// Packages have already passed the aircraft's round-trip survival check.
    pub(super) fn observe(&mut self, target: i32, risk: Risk, packages: &[Vec<(Plane, usize)>]) {
        if risk.aa > 0.15 || risk.intercepted || !risk.aa.is_finite() || !self.targets.insert(target) {
            return;
        }
        let allows = |kind| packages.iter().any(|p| p.iter().any(|(k, _)| *k == kind));
        match (allows(Plane::Raven), allows(Plane::Likho)) {
            (true, true) => self.either += 1,
            (true, false) => self.raven_only += 1,
            (false, true) => self.likho_only += 1,
            (false, false) => {}
        }
    }

    fn purchase(&self, have: &BTreeMap<Plane, usize>, cost: impl Fn(Plane) -> f32) -> Option<Plane> {
        // Each Lance contributes one alternative, and mixed fleets can answer
        // different Lances. Reserve aircraft for targets only their type can
        // safely reach before spending the rest on flexible targets.
        let alternatives: Vec<_> = (0..=self.either)
            .map(|ravens| {
                vec![
                    (Plane::Raven, 4 * (self.raven_only + ravens)),
                    (Plane::Likho, self.likho_only + self.either - ravens),
                ]
            })
            .collect();
        purchase(&alternatives, have, cost)
    }
}

impl BomberHandler {
    pub(super) fn inventory(&self, frame: i32) -> BTreeMap<Plane, usize> {
        let mut have = BTreeMap::new();
        for kind in self
            .units
            .values()
            .map(|a| a.kind)
            .chain(self.building.values().copied())
            .chain(self.orders.values().filter(|o| frame - o.1 < 30 * 30).map(|o| o.0))
        {
            *have.entry(kind).or_default() += 1;
        }
        have
    }
}

impl Ai {
    /// Urgent bomber procurement, ahead of routine Owl scouting at the factory.
    pub(crate) fn airstrike_lance_production(&self) -> Option<&'static str> {
        self.airstrike_lance_priority(&self.bomber_handler.inventory(self.frame)).map(Plane::name)
    }

    pub(super) fn airstrike_lance_priority(&self, have: &BTreeMap<Plane, usize>) -> Option<Plane> {
        let h = &self.bomber_handler;
        if h.grounded || self.dev_off("airstrikes") {
            return None;
        }
        // Refreshed with current route risk during the planner's regular pass,
        // including remembered Lances; production does not solve the map again.
        let next = h
            .lance_reserve
            .purchase(have, |k| self.defs.get(k.name()).map_or(f32::INFINITY, |d| d.cost(self.metal)))?;
        // Enemy aircraft bearing down on us still take precedence over this
        // opportunity. Other primary missions wait for the expanded reserve.
        h.recent
            .values()
            .filter(|p| p.job.kind == MissionKind::Intercept && self.frame - p.job.seen <= MEMORY)
            .filter_map(|p| {
                purchase(&p.packages, have, |k| self.defs.get(k.name()).map_or(f32::INFINITY, |d| d.cost(self.metal)))
                    .map(|kind| (p.score(), kind))
            })
            .max_by(|a, b| a.0.total_cmp(&b.0))
            .map(|(_, kind)| kind)
            .or(Some(next))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clear() -> Risk {
        Risk { aa: 0.0, exposure: 0.0, intercepted: false, burst: 0.0 }
    }

    fn choices() -> Vec<Vec<(Plane, usize)>> {
        vec![vec![(Plane::Raven, 2)], vec![(Plane::Likho, 1)]]
    }

    fn cost(kind: Plane) -> f32 {
        if kind == Plane::Raven { 300.0 } else { 2000.0 }
    }

    #[test]
    fn one_lance_wants_four_ravens_or_one_likho() {
        let mut reserve = LanceReserve::default();
        reserve.observe(1, clear(), &choices());
        let mut have = BTreeMap::new();
        for _ in 0..4 {
            assert_eq!(reserve.purchase(&have, cost), Some(Plane::Raven));
            *have.entry(Plane::Raven).or_default() += 1;
        }
        assert_eq!(reserve.purchase(&have, cost), None);
        assert_eq!(reserve.purchase(&BTreeMap::from([(Plane::Likho, 1)]), cost), None);
        assert_eq!(reserve.purchase(&BTreeMap::new(), |_| 1000.0), Some(Plane::Likho));
    }

    #[test]
    fn demand_scales_with_distinct_lances_without_a_cap() {
        let mut reserve = LanceReserve::default();
        for id in 1..=100 {
            reserve.observe(id, clear(), &choices());
            reserve.observe(id, clear(), &choices());
        }
        assert_eq!(reserve.either, 100);
        let mut have = BTreeMap::from([(Plane::Raven, 200), (Plane::Likho, 49)]);
        assert_eq!(reserve.purchase(&have, cost), Some(Plane::Raven));
        have.insert(Plane::Likho, 50);
        assert_eq!(reserve.purchase(&have, cost), None);
        assert_eq!(reserve.purchase(&BTreeMap::from([(Plane::Raven, 400)]), cost), None);
        assert_eq!(reserve.purchase(&BTreeMap::from([(Plane::Likho, 100)]), cost), None);
    }

    #[test]
    fn mixed_fleets_and_partial_wings_reduce_the_remaining_purchase_cost() {
        let mut reserve = LanceReserve::default();
        reserve.observe(1, clear(), &choices());
        reserve.observe(2, clear(), &choices());
        let have = BTreeMap::from([(Plane::Raven, 3), (Plane::Likho, 1)]);
        assert_eq!(reserve.purchase(&have, cost), Some(Plane::Raven));
        assert_eq!(reserve.purchase(&BTreeMap::from([(Plane::Raven, 4), (Plane::Likho, 1)]), cost), None);
        // At these prices, complete the partial Raven wing, then buy a Likho:
        // 2600 metal beats either five more Ravens (3000) or two Likhos (4000).
        let cost = |kind| if kind == Plane::Raven { 600.0 } else { 2000.0 };
        assert_eq!(reserve.purchase(&BTreeMap::from([(Plane::Raven, 3)]), cost), Some(Plane::Raven));
        assert_eq!(reserve.purchase(&BTreeMap::from([(Plane::Raven, 4)]), cost), Some(Plane::Likho));
    }

    #[test]
    fn weak_aa_and_survivable_packages_are_required() {
        for risk in [Risk { aa: 0.151, ..clear() }, Risk { intercepted: true, ..clear() }] {
            let mut reserve = LanceReserve::default();
            reserve.observe(1, risk, &choices());
            assert_eq!(reserve.purchase(&BTreeMap::new(), cost), None);
        }
        let mut reserve = LanceReserve::default();
        reserve.observe(1, clear(), &[]);
        assert_eq!(reserve.purchase(&BTreeMap::new(), cost), None);
        reserve.observe(2, Risk { aa: 0.15, ..clear() }, &[vec![(Plane::Likho, 1)]]);
        assert_eq!(reserve.purchase(&BTreeMap::from([(Plane::Raven, 100)]), cost), Some(Plane::Likho));
        reserve.observe(3, clear(), &choices());
        assert_eq!(reserve.purchase(&BTreeMap::from([(Plane::Likho, 1)]), cost), Some(Plane::Raven));
        reserve.observe(4, clear(), &[vec![(Plane::Raven, 2)]]);
        assert_eq!(reserve.purchase(&BTreeMap::from([(Plane::Likho, 100)]), cost), Some(Plane::Raven));
    }

    #[test]
    fn unfinished_and_unstarted_orders_fill_the_reserve_without_double_purchasing() {
        let mut h = BomberHandler::default();
        for id in 1..=4 {
            h.building.insert(id, Plane::Raven);
        }
        h.orders.insert(10, (Plane::Likho, 100));
        let mut reserve = LanceReserve::default();
        reserve.observe(1, clear(), &choices());
        reserve.observe(2, clear(), &choices());
        assert_eq!(reserve.purchase(&h.inventory(999), cost), None);
        assert_eq!(reserve.purchase(&h.inventory(1000), cost), Some(Plane::Raven));
        h.orders.clear();
        h.building.insert(5, Plane::Likho);
        assert_eq!(reserve.purchase(&h.inventory(1000), cost), None);
    }
}
