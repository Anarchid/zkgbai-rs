//! Reuse an assembled force where its previous dive ended, including on the
//! same target. Procurement sizes and the return trip do not apply to survivors.
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

impl Ai {
    pub(super) fn strike_members_center(&self, members: &[i32]) -> Option<Vec3> {
        let positions: Vec<_> = members.iter().filter_map(|id| self.fighters.get(id)).map(Fighter::get_pos).collect();
        let n = positions.len() as f32;
        (!positions.is_empty()).then(|| {
            Vec3::xz(positions.iter().map(|p| p.x).sum::<f32>() / n, positions.iter().map(|p| p.z).sum::<f32>() / n)
        })
    }

    fn strike_chain_clear(&self, w: &Wave, job: &Job, from: Vec3) -> bool {
        !self.strike_cooling(job.pos)
            && !self.squad_strike_job_taken(job)
            && self.strike_target_allowed(w.tactic, job)
            && self.strike_suitable(w.tactic, job, from)
            && self.strike_members_reach(&w.members, job.pos)
            && (job.prey != Prey::Defenseless || self.strike_opportunity_wave_clear(w, job.pos))
    }

    pub(super) fn strike_chain(&mut self, w: &mut Wave) -> bool {
        let Some(from) = self.strike_members_center(&w.members) else { return false };
        let Some(job) = nearby_jobs(self.strike_jobs(SNIPER_MEMORY), from)
            .into_iter()
            .find(|job| self.strike_chain_clear(w, job, from))
        else {
            return false;
        };
        self.log(&format!(
            "strikes: {} survivors of {} reassess {} {} for chain strike",
            w.members.len(),
            w.tactic.name(),
            job.name,
            job.id.unwrap_or(-1)
        ));
        w.job = job;
        w.needed = w.members.len();
        w.phase = Phase::Chaining;
        w.since = self.frame;
        w.query = None;
        true
    }

    pub(super) fn strike_chain_update(&mut self, w: &mut Wave) {
        let job = self.strike_jobs(SNIPER_MEMORY).into_iter().find(|j| squads::same_job(j, &w.job));
        let from = self.strike_members_center(&w.members);
        if self.frame - w.since >= 90
            || from.is_none()
            || job.as_ref().is_none_or(|j| !self.strike_chain_clear(w, j, from.unwrap_or(w.home)))
        {
            w.phase = Phase::Returning;
            w.since = self.frame;
            w.query = None;
            self.strikes.cooldowns.push((w.job.pos, self.frame));
            return;
        }
        w.job = job.unwrap();
        self.strike_predict(w);
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

    fn phantom(id: i32, x: f32) -> Job {
        Job {
            id: Some(id),
            pos: Vec3::xz(x, 0.0),
            last: DIVE_TIMEOUT + 1,
            count: 1,
            value: 750.0,
            name: "cloaksnipe".into(),
            prey: Prey::GlassCannon,
        }
    }

    #[test]
    fn chain_search_uses_current_position_and_can_find_the_next_phantom() {
        let jobs = nearby_jobs(vec![phantom(2, 5100.0), phantom(3, 6000.0), phantom(4, 100.0)], Vec3::xz(5000.0, 0.0));
        assert_eq!(jobs.iter().map(|j| j.id).collect::<Vec<_>>(), vec![Some(2), Some(3)]);
        assert!(nearby_jobs(vec![phantom(2, 5100.0)], Vec3::xz(100.0, 0.0)).is_empty());
    }
}
