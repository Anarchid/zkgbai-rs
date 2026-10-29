//! Live Mark's travel fields. Only owned numbers and an immutable terrain
//! grid cross the thread boundary; callbacks and issuing orders stay here.
use std::collections::BTreeMap;
use std::sync::Arc;

use super::{
    PERIOD,
    field::{CELL, Field, Grid, Mobility},
};
use crate::{Ai, kgbutil::worker::Worker, util::distance};
use springai::Vec3;

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
enum Key {
    Group(Vec<i32>),
    Team(usize),
}

#[derive(Clone)]
struct Request {
    key: Key,
    frame: i32,
    mobility: Mobility,
    speed: f32,
    sources: Vec<(i32, Vec3)>,
    max: f32,
}

impl Request {
    fn reusable(&self, now: &Self) -> bool {
        self.key == now.key
            && now.frame - self.frame <= 3 * PERIOD
            && self.mobility == now.mobility
            && self.speed == now.speed
            && self.max == now.max
            && self.sources.len() == now.sources.len()
            && self.sources.iter().zip(&now.sources).all(|(a, b)| {
                // Permit one update of movement plus a cell's sampling error.
                // Exact membership and movement class still have to match.
                a.0 == b.0 && distance(a.1, b.1) <= CELL * 0.75 + self.speed * 0.75
            })
    }
}

type Result = (Arc<Grid>, Vec<(Request, Arc<Field>)>);
type Job = (Arc<Grid>, Vec<Request>);
fn compute((grid, requests): Job) -> Result {
    let fields = requests
        .into_iter()
        .map(|r| {
            let sources: Vec<_> = r.sources.iter().map(|(_, p)| (*p, 0.0)).collect();
            let field = Arc::new(grid.field_within(r.mobility, r.speed, &sources, r.max));
            (r, field)
        })
        .collect();
    (grid, fields)
}

#[derive(Default)]
pub(super) struct Fields {
    worker: Option<Worker<Job, Result>>,
    cache: BTreeMap<Key, (Request, Arc<Field>)>,
    grid: Option<Arc<Grid>>,
}

impl Fields {
    pub fn stats(&self) -> Option<crate::kgbutil::worker::Stats> {
        self.worker.as_ref().map(Worker::stats)
    }
    pub fn team(&self, id: usize) -> Option<Arc<Field>> {
        self.cache.get(&Key::Team(id)).map(|(_, f)| f.clone())
    }
    pub fn group(&self, ids: &[i32]) -> Option<Arc<Field>> {
        let mut ids = ids.to_vec();
        ids.sort_unstable();
        self.cache.get(&Key::Group(ids)).map(|(_, f)| f.clone())
    }
}

impl Ai {
    /// Keep the last orders while a fresh batch is in flight. Nothing queues:
    /// when it completes, the next job snapshots the newest positions.
    pub(super) fn qrf_prepare_fields(
        &mut self,
        teams: &BTreeMap<usize, Vec<i32>>,
        raid_max: f32,
        team_max: f32,
    ) -> bool {
        let Some(grid) = self.qrf.grid.clone() else { return false };
        let mut requests = Vec::new();
        for g in &self.qrf.groups {
            let mut ids = g.members.clone();
            ids.sort_unstable();
            requests.push(Request {
                key: Key::Group(ids),
                frame: self.frame,
                mobility: g.mobility,
                speed: g.speed,
                sources: vec![(0, g.center)],
                max: raid_max,
            });
        }
        if !requests.is_empty() {
            for (&team, ids) in teams {
                let ms: Vec<_> = ids.iter().filter_map(|id| self.qrf.members.get(id)).collect();
                let Some(first) = ms.first() else { continue };
                requests.push(Request {
                    key: Key::Team(team),
                    frame: self.frame,
                    mobility: first.mobility,
                    speed: ms.iter().map(|m| m.speed).fold(f32::INFINITY, f32::min),
                    sources: ms.iter().map(|m| (m.unit.id(), m.pos())).collect(),
                    max: team_max,
                });
            }
        }
        let requests: BTreeMap<_, _> = requests.into_iter().map(|r| (r.key.clone(), r)).collect();
        let fields = &mut self.qrf.async_fields;
        if fields.grid.as_ref().is_none_or(|old| !Arc::ptr_eq(old, &grid)) {
            fields.cache.clear();
            fields.grid = Some(grid.clone());
        }
        let worker = fields.worker.get_or_insert_with(|| Worker::new("qrf-fields", compute));
        if let Some((Some((source_grid, result)), _elapsed)) = worker.poll()
            && Arc::ptr_eq(&source_grid, &grid)
        {
            for (r, f) in result {
                fields.cache.insert(r.key.clone(), (r, f));
            }
        }
        fields.cache.retain(|key, (old, _)| requests.get(key).is_some_and(|r| old.reusable(r)));
        let ready = requests.keys().all(|key| fields.cache.contains_key(key));
        if !worker.busy() {
            // Refresh before expiry, so standing teams do not lose an update
            // periodically. Missing/new membership is computed in this batch too.
            let refresh: Vec<_> = requests
                .into_values()
                .filter(|r| {
                    fields.cache.get(&r.key).is_none_or(|(old, _)| {
                        r.frame - old.frame >= 2 * PERIOD
                            || old.sources.iter().zip(&r.sources).any(|(a, b)| distance(a.1, b.1) >= CELL * 0.75)
                    })
                })
                .collect();
            if !refresh.is_empty() {
                worker.submit((grid, refresh));
            }
        }
        ready
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn invalidates_changed_members_mobility_and_expired_results() {
        let a = Request {
            key: Key::Team(1),
            frame: 30,
            mobility: Mobility::BOT,
            speed: 100.0,
            sources: vec![(5, Vec3::xz(100.0, 100.0))],
            max: 90.0,
        };
        let mut b = a.clone();
        b.frame += PERIOD;
        b.sources[0].1.x += 50.0;
        assert!(a.reusable(&b));
        b.sources[0].0 = 6;
        assert!(!a.reusable(&b));
        b = a.clone();
        b.mobility.max_slope += 0.1;
        assert!(!a.reusable(&b));
        b = a.clone();
        b.frame += 3 * PERIOD + 1;
        assert!(!a.reusable(&b));
        b = a.clone();
        b.sources[0].1.x += 1000.0;
        assert!(!a.reusable(&b));
    }
}
