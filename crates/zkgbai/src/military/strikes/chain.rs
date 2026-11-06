//! `strikechain`: a dive that ends, for whatever reason, looks round the
//! wave before it goes home. Another target near it (the second of three
//! Lances), or the same still-eligible target after a timeout, is put to the predictor
//! with the wave as it stands; a yes is a new dive with its own clock.
use super::*;

#[cfg(test)]
mod regression;

pub(super) const SEARCH_RADIUS: f32 = 1800.0;

pub(super) fn nearby_jobs(jobs: Vec<Job>, from: Vec3) -> Vec<Job> {
    let mut jobs: Vec<_> = jobs.into_iter().filter(|j| distance(from, j.pos) < SEARCH_RADIUS).collect();
    jobs.sort_by(|a, b| {
        (distance(from, a.pos) / a.value.max(1.0)).total_cmp(&(distance(from, b.pos) / b.value.max(1.0)))
    });
    jobs
}

/// The same quarry: one unit, or one inferred fix by name and place.
pub(super) fn same_target(a: &Job, b: &Job) -> bool {
    match (a.id, b.id) {
        (Some(x), Some(y)) => x == y,
        (None, None) => a.name == b.name && distance(a.pos, b.pos) < 500.0,
        _ => false,
    }
}

/// Candidates best first: the most metal per hit point left to take, then
/// the nearest.
fn rank(value: f32, hp: f32, dist: f32) -> (f32, f32) {
    (value / hp.max(1.0), dist)
}

impl Ai {
    /// A wave's place: the member nearest its members' centroid (a centroid
    /// may fall off the ground they stand on, across a cliff).
    pub(super) fn strike_wave_center(&self, members: &[i32]) -> Option<Vec3> {
        let positions: Vec<Vec3> = (members.iter())
            .filter_map(|id| self.fighters.get(id))
            .filter(|f| f.unit.health() > 0.0)
            .map(|f| f.get_pos())
            .collect();
        let n = positions.len() as f32;
        let mid =
            Vec3::xz(positions.iter().map(|p| p.x).sum::<f32>() / n, positions.iter().map(|p| p.z).sum::<f32>() / n);
        positions.into_iter().min_by(|a, b| distance(*a, mid).total_cmp(&distance(*b, mid)))
    }

    /// Hit points left to a job's target (an inferred fix: its count at
    /// full health).
    fn strike_job_hp(&self, j: &Job) -> f32 {
        let seen = j.id.and_then(|id| self.war.targets.get(&id)).filter(|e| !e.is_dead);
        seen.map(|e| e.get_health() * e.def_health())
            .or_else(|| self.defs.get(&j.name).map(|d| d.health() * j.count.max(1) as f32))
            .unwrap_or(f32::INFINITY)
    }

    /// The dive on `w.job` ended (`dead`: its target killed). True if a
    /// chain target is now being asked about (phase `Chaining`); false to
    /// go home as before.
    pub(super) fn strike_chain_start(&mut self, w: &mut Wave, dead: bool, frame: i32) -> bool {
        if self.dev_off("strikechain")
            || self.strikes.model_failed
            || w.members.is_empty()
            || w.chain.links >= MAX_LINKS
        {
            return false;
        }
        let Some(from) = self.strike_wave_center(&w.members) else { return false };
        let old = w.job.clone();
        let mut asks: Vec<((f32, f32), Job)> = (self.strike_jobs(SNIPER_MEMORY).into_iter())
            .filter(|j| {
                let same = same_target(j, &old);
                distance(j.pos, from) <= CHAIN_REACH
                    && if same { !dead } else { !self.strike_cooling(j.pos) }
                    && !self.squad_strike_job_taken(j)
                    && self.strike_target_allowed(w.tactic, j)
                    && self.strike_suitable(w.tactic, j, from)
                    && self.strike_members_reach(&w.members, j.pos)
                    && (j.prey != Prey::Defenseless || self.strike_opportunity_wave_clear(w, j.pos))
            })
            .map(|j| (rank(j.value, self.strike_job_hp(&j), distance(j.pos, from)), j))
            .collect();
        if asks.is_empty() {
            return false;
        }
        asks.sort_by(|(a, _), (b, _)| b.0.total_cmp(&a.0).then_with(|| a.1.total_cmp(&b.1)));
        asks.truncate(CHAIN_ASKS);
        let names: Vec<String> = (asks.iter())
            .map(|(_, j)| {
                let hp = j.id.and_then(|id| self.war.targets.get(&id)).map_or(1.0, |e| e.get_health());
                let same = if same_target(j, &old) { " again" } else { "" };
                format!("{} {:.0}%{same}", j.name, 100.0 * hp)
            })
            .collect();
        self.log(&format!(
            "strikes: {} {} chain: asking about {} ({})",
            w.members.len(),
            w.tactic.name(),
            asks.len(),
            names.join(", ")
        ));
        let mut asks: Vec<Job> = asks.into_iter().map(|(_, j)| j).rev().collect();
        let first = asks.pop().expect("not empty");
        w.chain.asks = asks;
        w.chain.ended = Some(old);
        w.job = first;
        w.needed = w.members.len();
        w.phase = Phase::Chaining;
        w.since = frame;
        w.query = None;
        self.strike_predict(w);
        true
    }

    /// One pass of a wave asking about a chain target.
    pub(super) fn strike_chain_step(&mut self, w: &mut Wave, frame: i32) {
        let target = w.job.id.and_then(|id| self.war.targets.get(&id));
        let gone = w.job.id.is_some_and(|id| self.strikes.killed.contains(&id)) || target.is_some_and(|e| e.is_dead);
        if let Some(e) = target.filter(|e| !e.is_dead && (e.visible || e.is_radar_visible)) {
            w.job.pos = e.position;
            w.job.last = frame;
        }
        self.strikes.killed.clear();
        if w.members.is_empty() || self.dev_off("strikechain") {
            self.strike_chain_fail(w, frame);
            return;
        }
        let valid = self.strike_wave_center(&w.members).is_some_and(|from| {
            self.strike_target_allowed(w.tactic, &w.job)
                && self.strike_suitable(w.tactic, &w.job, from)
                && self.strike_members_reach(&w.members, w.job.pos)
                && !self.squad_strike_job_taken(&w.job)
                && (w.job.prey != Prey::Defenseless || self.strike_opportunity_wave_clear(w, w.job.pos))
                && frame - w.job.last < SNIPER_MEMORY
        });
        if !gone && valid && frame - w.since <= CHAIN_WAIT && !self.strikes.model_failed {
            self.strike_predict(w);
            match w.phase {
                Phase::Diving => {
                    w.chain.links += 1;
                    if let Some(old) = w.chain.ended.take().filter(|old| !same_target(old, &w.job)) {
                        w.chain.struck.push(old.pos);
                    }
                    w.chain.asks.clear();
                    return;
                }
                // Refused (the predictor put the candidate on a short cooldown).
                Phase::Returning => {}
                _ => return,
            }
        }
        // Next candidate, or home.
        let Some(next) = w.chain.asks.pop() else {
            self.strike_chain_fail(w, frame);
            return;
        };
        w.job = next;
        w.phase = Phase::Chaining;
        w.since = frame;
        w.query = None;
        self.strike_predict(w);
    }

    /// No chain target taken: home.
    fn strike_chain_fail(&mut self, w: &mut Wave, frame: i32) {
        self.log(&format!("strikes: {} chain: none taken, going home", w.tactic.name()));
        if let Some(old) = w.chain.ended.take() {
            w.job = old;
        }
        self.strike_chain_home(w, frame);
    }

    /// The wave goes home: the place of its last job and every place it
    /// struck on the way cool down.
    pub(super) fn strike_chain_home(&mut self, w: &mut Wave, frame: i32) {
        self.strikes.cooldowns.push((w.job.pos, frame));
        for p in w.chain.struck.drain(..) {
            self.strikes.cooldowns.push((p, frame));
        }
        w.chain.asks.clear();
        w.chain.ended = None;
        w.phase = Phase::Returning;
        w.since = frame;
        w.query = None;
    }
    /// Do not retreat from a target already in weapon reach while the worker
    /// reassesses it, or advance farther on the old, now cancelled mission.
    pub(super) fn strike_chain_hold(&self, members: &[i32], job: &Job) {
        let target = job.id.and_then(|id| self.war.targets.get(&id)).filter(|e| e.visible && !e.is_dead);
        for id in members {
            let Some(f) = self.fighters.get(id) else { continue };
            let u = f.unit;
            if let Some(e) = target.filter(|e| distance(u.pos(), e.position) <= u.max_range().max(80.0) * 0.9) {
                u.attack(e.unit, 0);
            } else {
                u.stop(0);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn job(id: Option<i32>, name: &str, x: f32) -> Job {
        Job { id, pos: Vec3::xz(x, 0.0), last: 0, count: 1, value: 1000.0, name: name.into(), prey: Prey::GlassCannon }
    }

    #[test]
    fn same_target_is_one_unit_or_one_nearby_fix() {
        assert!(same_target(&job(Some(7), "hoverarty", 0.0), &job(Some(7), "hoverarty", 900.0)));
        assert!(!same_target(&job(Some(7), "hoverarty", 0.0), &job(Some(8), "hoverarty", 0.0)));
        assert!(same_target(&job(None, "cloaksnipe", 0.0), &job(None, "cloaksnipe", 300.0)));
        assert!(!same_target(&job(None, "cloaksnipe", 0.0), &job(None, "cloaksnipe", 600.0)));
        assert!(!same_target(&job(Some(7), "cloaksnipe", 0.0), &job(None, "cloaksnipe", 0.0)));
    }

    #[test]
    fn a_nearly_dead_lance_ranks_before_a_healthy_one() {
        let wounded = rank(1000.0, 40.0, 600.0);
        let healthy = rank(1000.0, 1000.0, 100.0);
        assert!(wounded.0 > healthy.0);
        // Equal worth per hit point: the nearer first (ascending distance).
        assert_eq!(rank(1000.0, 1000.0, 100.0).0, rank(1000.0, 1000.0, 700.0).0);
        assert!(rank(1000.0, 0.0, 0.0).0.is_finite());
    }
}
