//! Existing raider and pure Halberd squads may spend themselves on artillery. This is
//! target selection, not procurement: their normal membership stays intact.
use super::{
    prediction::{Answer, Predictor},
    *,
};
use crate::military::wrappers::{SquadId, SquadKind};

const SIGHT_MEMORY: i32 = 30 * 10;
const SEARCH_RADIUS: f32 = 1800.0;

pub(super) struct Mission {
    job: Job,
    members: Vec<i32>,
    key: u64,
    since: i32,
    diving: bool,
    answer: Option<Answer>,
    retreat: Vec<i32>,
    commanded: i32,
    halberds: bool,
    home: Vec3,
}

/// A riot beside the target or anywhere on the approach vetoes the mission;
/// friendly strength must not cancel this gate as it can on a net threat map.
fn covers_approach(from: Vec3, to: Vec3, escort: Vec3, range: f32) -> bool {
    let dx = to.x - from.x;
    let dz = to.z - from.z;
    let t = (((escort.x - from.x) * dx + (escort.z - from.z) * dz) / (dx * dx + dz * dz).max(1.0)).clamp(0.0, 1.0);
    distance(escort, Vec3::xz(from.x + t * dx, from.z + t * dz)) < range.max(450.0) + 150.0
}

fn answer_current(a: &Answer, m: &Mission, frame: i32) -> bool {
    a.key == m.key && frame - m.since < 90 && a.members == m.members && distance(a.pos, m.job.pos) < 150.0
}

fn eligible_kind(kind: SquadKind, all_halberds: bool) -> bool {
    kind == SquadKind::Raider || (matches!(kind, SquadKind::Assault | SquadKind::Hover) && all_halberds)
}

pub(super) fn same_job(a: &Job, b: &Job) -> bool {
    match (a.id, b.id) {
        (Some(a), Some(b)) => a == b,
        _ => a.name == b.name && distance(a.pos, b.pos) < 500.0,
    }
}

impl Ai {
    pub(super) fn squad_strike_job_taken(&self, job: &Job) -> bool {
        self.lobster_strike_job_taken(job) || self.strikes.missions.values().any(|m| same_job(&m.job, job))
    }

    pub(super) fn squad_strike_waiting_fire(&self, id: i32) -> Option<i32> {
        self.strikes
            .missions
            .values()
            .find(|m| m.halberds && !m.diving && m.members.contains(&id))
            .map(|_| Tactic::Halberd.fire_state())
    }

    pub(super) fn squad_strike_evade(&mut self, id: i32, goal: Vec3) {
        if let Some(m) = self.strikes.missions.values_mut().find(|m| m.halberds && !m.diving && m.members.contains(&id))
        {
            m.home = goal;
            m.key = 0;
            m.answer = None;
        }
    }

    pub(crate) fn squad_strike_active(&self, sid: SquadId) -> bool {
        self.lobster_strike_active(sid) || self.strikes.missions.contains_key(&sid)
    }

    pub(crate) fn squad_strike_halberds(&self, sid: SquadId) -> bool {
        self.squads.get(sid).is_some_and(|s| {
            matches!(s.kind, SquadKind::Hover | SquadKind::Assault)
                && !s.fighters.is_empty()
                && s.fighters.iter().all(|id| {
                    self.fighters
                        .get(id)
                        .is_none_or(|f| f.is_dead() || self.defs.name_of(f.unit) == Tactic::Halberd.name())
                })
        })
    }

    fn squad_strike_refresh(&self, m: &Mission) -> Option<Job> {
        if !m.halberds {
            return m.job.id.and_then(|id| self.squad_strike_job(id));
        }
        // A confirmed kill ends this mission. Do not turn its old position
        // back into an anonymous search; new firing evidence can offer a job.
        if m.job
            .id
            .is_some_and(|id| self.strikes.killed.contains(&id) || self.war.targets.get(&id).is_some_and(|e| e.is_dead))
        {
            return None;
        }
        self.strike_jobs(SNIPER_MEMORY)
            .into_iter()
            .find(|j| same_job(j, &m.job) && self.strike_target_allowed(Tactic::Halberd, j))
    }

    fn squad_strike_clear_for(&self, job: &Job, members: &[i32], halberds: bool) -> bool {
        if !halberds {
            return self.squad_strike_clear(job, members);
        }
        // Assaults may accept escorts: the assassination model prices them
        // into target death, instead of applying the raiders' hard riot veto.
        members
            .iter()
            .filter(|id| self.fighters.get(id).is_some_and(|f| !f.is_dead()))
            .all(|id| self.fighters.get(id).is_some_and(|f| self.strike_suitable(Tactic::Halberd, job, f.get_pos())))
    }
    pub(crate) fn squad_strike_owns(&self, id: i32) -> bool {
        self.lobster_strike_owns(id)
            || self
                .fighters
                .get(&id)
                .and_then(|f| f.squad)
                .and_then(|sid| self.strikes.missions.get(&sid))
                .is_some_and(|m| m.members.contains(&id))
    }

    fn squad_strike_job(&self, id: i32) -> Option<Job> {
        let e = self.war.targets.get(&id)?;
        if e.is_dead
            || !e.identified
            || e.is_com
            || e.can_fly()
            || e.is_riot
            || self.frame - e.last_seen >= SIGHT_MEMORY
            || e.position == NULLPOS
        {
            return None;
        }
        let prey = if glass_cannon(&e.def_name) {
            Prey::GlassCannon
        } else if self.strike_fragile(e) {
            Prey::Fragile
        } else {
            return None;
        };
        Some(Job {
            id: Some(id),
            pos: e.position,
            last: e.last_seen,
            count: 1,
            value: e.ud?.cost(self.metal),
            name: e.def_name.clone(),
            prey,
        })
    }

    fn squad_strike_clear(&self, job: &Job, members: &[i32]) -> bool {
        !self.missiles_ground_burning(job.pos)
            && members.iter().filter(|id| self.fighters.get(id).is_some_and(|f| !f.is_dead())).all(|id| {
                let Some(f) = self.fighters.get(id) else { return false };
                let Some(def) = f.unit.def() else { return false };
                self.strike_target_point(def, f.get_pos(), job.pos).is_some()
                    && !self.war.targets.values().any(|e| {
                        !e.is_dead
                            && Some(e.unit_id) != job.id
                            && (e.is_static || self.frame - e.last_seen < JOB_MEMORY)
                            && (e.is_riot || !e.identified)
                            && covers_approach(f.get_pos(), job.pos, e.position, e.def_max_weapon_range())
                    })
            })
    }

    fn squad_strike_available(&self, sid: SquadId) -> bool {
        !self.dev_off("strikes")
            && !self.qrf_drafted(sid)
            && !self.op.over.squad_intents.contains_key(&sid)
            && self.op_squad_target(sid).is_none()
            && self.squads.get(sid).is_some_and(|s| {
                eligible_kind(s.kind, self.squad_strike_halberds(sid))
                    && s.status == 'a'
                    && s.fighters.iter().any(|id| self.fighters.get(id).is_some_and(|f| !f.is_dead()))
                    && s.fighters.len() <= MAX_WAVE
                    && s.fighters.iter().filter(|id| self.fighters.get(id).is_some_and(|f| !f.is_dead())).all(|id| {
                        self.fighters.get(id).is_some_and(|f| {
                            !f.is_dead()
                                && f.squad == Some(sid)
                                && !self.strikes.members.contains(id)
                                && !springai::reserved::is_reserved(self.cb, *id)
                                && !self.qrf_claims(*id)
                                && !self.retreat_handler.is_retreating(*id)
                                && f.unit.def().is_some_and(|d| !d.is_able_to_fly())
                        })
                    })
            })
    }

    pub(crate) fn squad_strike_cancel(&mut self, sid: SquadId) {
        self.lobster_strike_cancel(sid);
        let Some(m) = self.strikes.missions.remove(&sid) else { return };
        self.squad_strike_release(sid, m);
    }

    fn squad_strike_release(&mut self, sid: SquadId, m: Mission) {
        let takeover = self.qrf_drafted(sid)
            || self.op.over.squad_intents.contains_key(&sid)
            || self.op_squad_target(sid).is_some();
        for id in &m.members {
            let Some(f) = self.fighters.get(id).filter(|f| !f.is_dead() && f.squad == Some(sid)) else { continue };
            let u = f.unit;
            if springai::reserved::is_reserved(self.cb, *id) || self.strikes.members.contains(id) {
                continue;
            }
            if !takeover {
                u.stop(0);
                u.set_fire_state(2, 0);
            }
            if m.retreat.contains(id) {
                self.retreat_add_coward(u);
            }
        }
        self.strikes.squad_retry.insert(sid, self.frame + 30 * 10);
        self.log(&format!("strikes: squad {sid} released from {} {}", m.job.name, m.job.id.unwrap_or(-1)));
    }

    /// Also cancel orphaned or pre-empted missions when their squad's handler
    /// no longer runs (deleted squads, operator orders, QRF, strikes disabled).
    pub(super) fn squad_strikes_maintain(&mut self) {
        let cancel: Vec<_> = self
            .strikes
            .missions
            .iter()
            .filter_map(|(&sid, m)| {
                let invalid = !self.squad_strike_available(sid)
                    || self.frame - m.since >= DIVE_TIMEOUT
                    || self
                        .squad_strike_refresh(m)
                        .is_none_or(|job| !self.squad_strike_clear_for(&job, &m.members, m.halberds));
                invalid.then_some(sid)
            })
            .collect();
        for sid in cancel {
            self.squad_strike_cancel(sid);
        }
        self.strikes.squad_retry.retain(|sid, at| *at > self.frame && self.squads.get(*sid).is_some());
    }

    /// Runs before normal danger/mex scoring. A pending answer briefly holds
    /// the squad; a committed answer owns its orders until the job ends.
    pub(crate) fn squad_strike(&mut self, sid: SquadId) -> Option<Vec3> {
        if let Some(goal) = self.lobster_strike(sid) {
            return Some(goal);
        }
        if !self.squad_strike_available(sid) {
            self.squad_strike_cancel(sid);
            return None;
        }
        if let Some(p) = self.strikes.squad_predictor.as_ref() {
            while let Ok(a) = p.rx.try_recv() {
                if let Some(m) = self.strikes.missions.values_mut().find(|m| m.key == a.key) {
                    m.answer = Some(a);
                }
            }
        }
        if let Some(mut m) = self.strikes.missions.remove(&sid) {
            m.members.retain(|id| self.fighters.get(id).is_some_and(|f| !f.is_dead() && f.squad == Some(sid)));
            let job = self.squad_strike_refresh(&m);
            if m.members.is_empty()
                || self.frame - m.since >= DIVE_TIMEOUT
                || job.as_ref().is_none_or(|j| !self.squad_strike_clear_for(j, &m.members, m.halberds))
            {
                self.squad_strike_release(sid, m);
                return None;
            }
            m.job = job.unwrap();
            if m.key == 0 {
                // An existing assault squad can be stretched along its old
                // route. Gather the whole force before evaluating the dive.
                self.squad_strike_command(sid, &m);
                let ready = m.members.iter().all(|id| {
                    self.fighters.get(id).is_some_and(|f| {
                        distance(f.get_pos(), m.home) <= formation_radius(m.members.len())
                            && self.strike_evasion_goal(*id).is_none()
                    })
                });
                if ready {
                    if let Some(key) = self.squad_strike_predict(&m.job, &m.members, m.home, m.halberds) {
                        m.key = key;
                        m.since = self.frame;
                    }
                }
                let goal = m.home;
                self.strikes.missions.insert(sid, m);
                return Some(goal);
            }
            if !m.diving {
                if let Some(a) = m.answer.take() {
                    if !answer_current(&a, &m, self.frame) || !launchable(a.probability) {
                        self.squad_strike_release(sid, m);
                        return None;
                    }
                    m.diving = true;
                    m.since = self.frame;
                    if self.strikes.wave.as_ref().is_some_and(|w| {
                        matches!(w.phase, Phase::Forming | Phase::Screening) && same_job(&w.job, &m.job)
                    }) {
                        let w = self.strikes.wave.take().unwrap();
                        self.strikes_release(w);
                    }
                    for id in &m.members {
                        if self.retreat_handler.is_coward(*id) {
                            m.retreat.push(*id);
                            if let Some(f) = self.fighters.get(id) {
                                self.retreat_remove_unit(f.unit);
                            }
                        }
                    }
                    self.log(&format!(
                        "strikes: squad {sid} ({} units) dive {} {} (target death {:.0}%)",
                        m.members.len(),
                        m.job.name,
                        m.job.id.unwrap_or(-1),
                        a.probability * 100.0
                    ));
                } else if self.frame - m.since >= 90 {
                    self.squad_strike_release(sid, m);
                    return None;
                }
            }
            if m.diving && self.frame - m.commanded >= 15 {
                self.squad_strike_command(sid, &m);
                m.commanded = self.frame;
            }
            let goal = m.job.pos;
            self.strikes.missions.insert(sid, m);
            return Some(goal);
        }
        if self.strikes.model_failed
            || self.strikes.squad_retry.get(&sid).is_some_and(|at| *at > self.frame)
            || self.strikes.missions.values().any(|m| !m.diving)
        {
            return None;
        }
        self.strikes.squad_retry.insert(sid, self.frame + 30);
        let s = self.squads.get(sid)?;
        let from = s.get_pos(&self.fighters);
        let members: Vec<_> =
            s.fighters.iter().copied().filter(|id| self.fighters.get(id).is_some_and(|f| !f.is_dead())).collect();
        let halberds = self.squad_strike_halberds(sid);
        let candidates = if halberds {
            self.strike_jobs(SNIPER_MEMORY)
                .into_iter()
                .filter(|j| self.strike_target_allowed(Tactic::Halberd, j))
                .collect()
        } else {
            self.war
                .targets
                .values()
                .filter(|e| e.visible)
                .filter_map(|e| self.squad_strike_job(e.unit_id))
                .collect::<Vec<_>>()
        };
        let mut jobs: Vec<_> = candidates
            .into_iter()
            .filter(|j| {
                distance(from, j.pos) < SEARCH_RADIUS
                    && !self.strike_cooling(j.pos)
                    && !self.squad_strike_job_taken(j)
                    && !self.strikes.wave.as_ref().is_some_and(|w| w.phase == Phase::Diving && same_job(&w.job, j))
            })
            .collect();
        jobs.sort_by(|a, b| {
            (distance(from, a.pos) / a.value.max(1.0)).total_cmp(&(distance(from, b.pos) / b.value.max(1.0)))
        });
        let job = jobs.into_iter().find(|j| self.squad_strike_clear_for(j, &members, halberds))?;
        let key = if halberds { 0 } else { self.squad_strike_predict(&job, &members, from, false)? };
        for id in &members {
            if let Some(f) = self.fighters.get_mut(id) {
                f.clear_task(&mut self.raider_handler.scout_tasks);
                f.unit.stop(0);
                if halberds {
                    f.unit.set_fire_state(0, 0);
                }
            }
        }
        let goal = job.pos;
        self.strikes.missions.insert(
            sid,
            Mission {
                job,
                members,
                key,
                since: self.frame,
                diving: false,
                answer: None,
                retreat: Vec::new(),
                commanded: -30,
                halberds,
                home: from,
            },
        );
        Some(goal)
    }

    fn squad_strike_predict(&mut self, job: &Job, members: &[i32], from: Vec3, halberds: bool) -> Option<u64> {
        if self.strikes.squad_predictor.is_none() {
            match Predictor::start() {
                Ok(p) => self.strikes.squad_predictor = Some(p),
                Err(e) => {
                    self.log(&format!("strikes: squad model unavailable: {e}"));
                    self.strikes.model_failed = true;
                    return None;
                }
            }
        }
        self.strikes.serial += 1;
        let key = self.strikes.serial;
        let ground = if halberds { self.defs.get(Tactic::Halberd.name()) } else { None };
        let request = self.strike_prediction_request(key, job, members, from, ground, false)?;
        self.strikes.squad_predictor.as_ref()?.tx.try_send(request).ok()?;
        Some(key)
    }

    fn squad_strike_command(&mut self, sid: SquadId, m: &Mission) {
        if m.halberds {
            self.strike_command(&Wave {
                tactic: Tactic::Halberd,
                job: m.job.clone(),
                needed: m.members.len(),
                members: m.members.clone(),
                phase: if m.diving { Phase::Diving } else { Phase::Forming },
                since: m.since,
                home: m.home,
                query: None,
            });
            let target = if m.diving { m.job.pos } else { m.home };
            if let Some(s) = self.squads.get_mut(sid) {
                s.target = target;
            }
            for id in &m.members {
                if let Some(f) = self.fighters.get_mut(id) {
                    f.target = target;
                }
            }
            return;
        }
        let visible = m.job.id.and_then(|id| self.war.targets.get(&id)).filter(|e| e.visible).map(|e| e.unit);
        for id in &m.members {
            let Some(f) = self.fighters.get(id) else { continue };
            let u = f.unit;
            let point = u.def().and_then(|d| self.strike_target_point(d, u.pos(), m.job.pos));
            self.fighters.get_mut(id).unwrap().target = m.job.pos;
            u.set_fire_state(if matches!(self.defs.name_of(u), "hoverassault" | "cloakheavyraid") { 0 } else { 2 }, 0);
            if let Some(t) = visible.filter(|t| distance(u.pos(), t.pos()) < u.max_range().max(80.0) * 0.9) {
                u.attack(t, 0);
            } else if let Some(p) = point {
                kgb_move_to(u, p, 0, self.frame);
            } else {
                u.stop(0);
            }
        }
        if let Some(s) = self.squads.get_mut(sid) {
            s.target = m.job.pos;
            s.last_assignment_frame = self.frame;
        }
    }

    pub(super) fn squad_strikes_status(&self) -> serde_json::Value {
        serde_json::json!(
            self.strikes
                .missions
                .iter()
                .map(|(sid, m)| serde_json::json!({
                    "squad": sid, "target": m.job.id, "def": m.job.name, "members": m.members,
                    "phase": if m.diving { "diving" } else if m.key == 0 { "gathering" } else { "assessing" },
                    "tactic": if m.halberds { "hoverassault" } else { "raiders" },
                }))
                .collect::<Vec<_>>()
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_raiders_or_pure_halberd_assaults_can_take_squad_jobs() {
        assert!(eligible_kind(SquadKind::Raider, false));
        assert!(eligible_kind(SquadKind::Hover, true));
        assert!(eligible_kind(SquadKind::Assault, true));
        assert!(!eligible_kind(SquadKind::Hover, false));
        assert!(!eligible_kind(SquadKind::Assault, false));
        assert!(!eligible_kind(SquadKind::Taskforce, true));
    }

    #[test]
    fn riot_cover_includes_target_and_route_but_not_distant_escorts() {
        let from = Vec3::xz(0.0, 0.0);
        let to = Vec3::xz(1600.0, 0.0);
        assert!(covers_approach(from, to, Vec3::xz(1600.0, 500.0), 250.0));
        assert!(covers_approach(from, to, Vec3::xz(800.0, 500.0), 250.0));
        assert!(!covers_approach(from, to, Vec3::xz(800.0, 900.0), 250.0));
        assert!(covers_approach(from, to, Vec3::xz(800.0, 900.0), 800.0));
        assert!(covers_approach(to, to, to, 250.0));
    }
    #[test]
    fn late_or_changed_squad_predictions_cannot_launch() {
        let mut m = Mission {
            job: Job {
                id: Some(1),
                pos: Vec3::xz(0.0, 0.0),
                last: 0,
                count: 1,
                value: 1000.0,
                name: "hoverarty".into(),
                prey: Prey::GlassCannon,
            },
            members: vec![2, 3],
            key: 7,
            since: 100,
            diving: false,
            answer: None,
            retreat: Vec::new(),
            commanded: 0,
            halberds: false,
            home: Vec3::xz(0.0, 0.0),
        };
        let a = Answer { key: 7, kill_frames: Some(100), probability: 1.0, pos: m.job.pos, members: vec![2, 3] };
        assert!(answer_current(&a, &m, 101));
        assert!(!answer_current(&a, &m, 190));
        m.members.pop();
        assert!(!answer_current(&a, &m, 101));
        m.members.push(3);
        m.job.pos.x = 200.0;
        assert!(!answer_current(&a, &m, 101));
    }
}
