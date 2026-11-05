//! Shared resource allocation and execution evidence for tracked projects.
use super::{Hand, Worth, best_crew};
use crate::economy::TaskId;
use std::collections::{BTreeMap, BTreeSet};

pub(super) struct Request {
    pub task: TaskId,
    pub worth: Worth,
    pub work: f32,
    pub cost: f32,
    pub incumbent: bool,
    pub urgent: bool,
    pub hands: Vec<Hand>,
}

pub(super) struct Allocation {
    pub crew: Vec<i32>,
    pub eta: f32,
    pub value: f32,
}

/// Deadlines preempt capital, then valued grid repairs precede ordinary work.
/// Among grid repairs restore the most value first; otherwise retain crews
/// and use task ids for a stable oldest-first order.
/// A worker, stored metal, and each share of income are spent only once.
pub(super) fn allocate(
    mut requests: Vec<Request>,
    mut metal: f32,
    mut income: f32,
    now: f32,
) -> BTreeMap<TaskId, Allocation> {
    requests.sort_by(|a, b| {
        a.worth
            .deadline()
            .unwrap_or(f32::INFINITY)
            .total_cmp(&b.worth.deadline().unwrap_or(f32::INFINITY))
            .then_with(|| b.urgent.cmp(&a.urgent))
            .then_with(|| {
                if a.urgent && b.urgent {
                    b.worth.at(now).total_cmp(&a.worth.at(now))
                } else {
                    std::cmp::Ordering::Equal
                }
            })
            .then_with(|| b.incumbent.cmp(&a.incumbent))
            .then_with(|| a.task.cmp(&b.task))
    });
    metal = metal.max(0.0);
    income = income.max(0.0);
    let mut used = BTreeSet::new();
    let mut out = BTreeMap::new();
    for mut r in requests {
        r.hands.retain(|h| !used.contains(&h.0));
        let (k, eta, value) = best_crew(&r.hands, r.work, r.cost, metal, income, &r.worth, now);
        let crew: Vec<_> =
            if eta.is_finite() && value > 0.0 { r.hands.iter().take(k).map(|h| h.0).collect() } else { Vec::new() };
        if !crew.is_empty() {
            used.extend(crew.iter().copied());
            let stored = metal.min(r.cost);
            metal -= stored;
            // Reserve the rate the forecast needs before its pessimism
            // allowance, rather than allocating that allowance again.
            let rate = (r.cost - stored).max(0.0) / (eta / super::PESSIMISM).max(0.001);
            income = (income - rate).max(0.0);
        }
        out.insert(r.task, Allocation { value: if crew.is_empty() { 0.0 } else { value }, crew, eta });
    }
    out
}

#[derive(Default)]
pub(super) struct Progress {
    pub last: i32,
    built: f32,
    distances: BTreeMap<i32, f32>,
}

impl Progress {
    pub fn new(frame: i32) -> Self {
        Self { last: frame, ..Default::default() }
    }
    pub fn start(&mut self, frame: i32) {
        self.last = frame;
        self.distances.clear();
    }
    /// Use observed construction and cumulative approach, never order resends,
    /// as evidence. New crews get time to begin; an unchanged stalled crew does not.
    pub fn observe(&mut self, frame: i32, built: f32, distances: BTreeMap<i32, f32>) {
        let changed = self.distances.keys().ne(distances.keys());
        let advanced = built > self.built + 0.0001
            || built < self.built
            || distances.iter().any(|(id, d)| self.distances.get(id).is_some_and(|old| *d + 32.0 < *old));
        if changed || advanced {
            self.last = frame;
            self.built = built;
            self.distances = distances;
        }
    }

    pub fn stalled(&self, frame: i32) -> bool {
        frame - self.last >= 30 * 30
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn job(task: TaskId, incumbent: bool, hands: Vec<Hand>) -> Request {
        Request { task, incumbent, urgent: false, hands, work: 100.0, cost: 0.0, worth: Worth::Flat(1000.0) }
    }

    #[test]
    fn restoring_high_voltage_grid_income_preempts_ordinary_capital() {
        let hands = vec![(1, 0.0, 10.0)];
        let mut grid = job(3, false, hands.clone());
        grid.urgent = true;
        grid.worth = Worth::Flat(60_000.0); // 100 metal/s over ten minutes.
        let mut turret = job(2, true, hands.clone());
        turret.urgent = true;
        turret.worth = Worth::Flat(4000.0);
        let plan = allocate(vec![job(1, true, hands), turret, grid], 0.0, 10.0, 10000.0);
        assert_eq!(plan[&3].crew, vec![1]);
        assert!(plan[&1].crew.is_empty());
        assert!(plan[&2].crew.is_empty());
    }

    #[test]
    fn crews_are_disjoint_and_deadlines_preempt_existing_capital() {
        let hands = vec![(1, 0.0, 10.0), (2, 2.0, 10.0)];
        let mut urgent = job(3, false, hands.clone());
        urgent.worth = Worth::Deadline { value: 1000.0, by: 9.0 };
        let plan = allocate(vec![job(1, true, hands), urgent], 0.0, 10.0, 0.0);
        assert_eq!(plan[&3].crew, vec![1, 2]);
        assert!(plan[&1].crew.is_empty());
    }

    #[test]
    fn an_existing_crew_finishes_before_a_new_capital_project() {
        let hands = vec![(1, 0.0, 10.0), (2, 2.0, 10.0)];
        let plan = allocate(vec![job(1, false, hands.clone()), job(2, true, hands)], 0.0, 10.0, 0.0);
        assert_eq!(plan[&2].crew, vec![1]);
        assert_eq!(plan[&1].crew, vec![2]);
    }

    #[test]
    fn forecasts_cannot_spend_the_same_bank_or_income_twice() {
        let mut a = job(1, true, vec![(1, 0.0, 10.0)]);
        let mut b = job(2, false, vec![(2, 0.0, 10.0)]);
        a.cost = 200.0;
        b.cost = 200.0;
        let plan = allocate(vec![a, b], 100.0, 10.0, 0.0);
        assert_eq!(plan[&1].crew, vec![1]);
        assert!(plan[&2].crew.is_empty());
        assert!(plan[&2].eta.is_infinite());
    }

    #[test]
    fn headway_accumulates_but_reissuing_without_progress_does_not_renew_a_crew() {
        let mut p = Progress::default();
        p.observe(0, 0.0, BTreeMap::from([(1, 500.0)]));
        p.observe(30, 0.0, BTreeMap::from([(1, 480.0)]));
        p.observe(60, 0.0, BTreeMap::from([(1, 460.0)]));
        assert_eq!(p.last, 60);
        p.observe(960, 0.0, BTreeMap::from([(1, 460.0)]));
        assert!(p.stalled(960));
        p.observe(990, 0.01, BTreeMap::from([(1, 460.0)]));
        assert!(!p.stalled(990));
    }

    #[test]
    fn a_crew_that_never_accepts_its_orders_also_expires() {
        let mut p = Progress::new(100);
        p.observe(500, 0.0, BTreeMap::new());
        assert!(p.stalled(1000));
        p.start(1000);
        assert!(!p.stalled(1000));
    }
}
