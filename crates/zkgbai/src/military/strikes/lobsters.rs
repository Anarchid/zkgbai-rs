//! Existing Buoy assault squads use their attached Lobster for artillery kills.
//! The throw controller owns gathering and flight; this module owns the decision
//! to launch and the focus fire/search afterward. There is no procurement here.
use super::{
    prediction::{Answer, Predictor},
    squads::same_job,
    *,
};
use crate::military::jumps::lobster::{FLIGHT, STRIKE_AIM_FRAMES, StrikeThrow};
use crate::military::{SquadId, SquadKind};

#[derive(Default)]
pub(super) struct LobsterStrikes {
    missions: BTreeMap<SquadId, Mission>,
    retry: BTreeMap<SquadId, i32>,
    predictor: Option<Predictor>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum State {
    Gathering,
    Assessing,
    Launching,
    Flying,
    Attacking,
}

struct Query {
    key: u64,
    at: i32,
    plan: StrikeThrow,
    pos: Vec3,
    members: Vec<i32>,
}
struct Mission {
    job: Job,
    members: Vec<i32>,
    launcher: i32,
    state: State,
    since: i32,
    query: Option<Query>,
    answer: Option<Answer>,
    retreat: Vec<i32>,
    returning: bool,
    commanded: i32,
}

impl Ai {
    pub(crate) fn lobster_strike_owns(&self, id: i32) -> bool {
        self.strikes.lobsters.missions.values().any(|m| m.launcher == id || m.members.contains(&id))
    }
    /// Damage callbacks run before the strike callback. Keep a fully gathered
    /// payload intact long enough to re-predict with its actual damaged health.
    /// An unfinished gather remains subject to ordinary individual retreat.
    pub(crate) fn lobster_strike_assessing(&self, id: i32) -> bool {
        self.strikes.lobsters.missions.iter().any(|(&sid, m)| {
            matches!(m.state, State::Gathering | State::Assessing)
                && (m.launcher == id || m.members.contains(&id))
                && self.lobster_strike_throw(sid).is_some_and(|t| t.ready)
        })
    }
    pub(super) fn lobster_strike_active(&self, sid: SquadId) -> bool {
        self.strikes.lobsters.missions.contains_key(&sid)
    }
    pub(super) fn lobster_strike_job_taken(&self, job: &Job) -> bool {
        self.strikes.lobsters.missions.values().any(|m| same_job(&m.job, job))
    }
    fn lobster_strike_squad(&self, sid: SquadId) -> bool {
        self.squads.get(sid).is_some_and(|s| {
            s.kind == SquadKind::Assault
                && s.status == 'a'
                && s.fighters.len() <= MAX_WAVE
                && s.fighters
                    .iter()
                    .filter_map(|id| self.fighters.get(id))
                    .any(|f| !f.is_dead() && self.defs.name_of(f.unit) == "amphfloater")
                && s.fighters
                    .iter()
                    .filter_map(|id| self.fighters.get(id))
                    .all(|f| f.is_dead() || self.defs.name_of(f.unit).starts_with("amph"))
        })
    }
    fn lobster_strike_available(&self, sid: SquadId) -> bool {
        !self.dev_off("strikes")
            && !self.dev_off("lobsters")
            && !self.dev_off("jumps")
            && !self.qrf_drafted(sid)
            && self.op_squad_target(sid).is_none()
            && !self.op.over.squad_intents.contains_key(&sid)
            && self.lobster_strike_squad(sid)
            && self.squads.get(sid).is_some_and(|s| {
                s.fighters.iter().filter_map(|id| self.fighters.get(id)).filter(|f| !f.is_dead()).all(|f| {
                    !springai::reserved::is_reserved(self.cb, f.id)
                        && !self.retreat_handler.is_retreating(f.id)
                        && !self.qrf_claims(f.id)
                        && !self.strikes.members.contains(&f.id)
                })
            })
    }
    fn lobster_strike_refresh(&self, job: &Job) -> Option<Job> {
        if job
            .id
            .is_some_and(|id| self.strikes.killed.contains(&id) || self.war.targets.get(&id).is_some_and(|e| e.is_dead))
        {
            return None;
        }
        self.strike_jobs(SNIPER_MEMORY).into_iter().find(|j| {
            same_job(j, job)
                && !self.missiles_ground_burning(j.pos)
                && matches!(j.prey, Prey::GlassCannon | Prey::Fragile)
        })
    }
    fn lobster_strike_release_unit(&mut self, sid: SquadId, id: i32, launcher: i32, retreat: bool) {
        let Some(f) = self.fighters.get(&id).filter(|f| !f.is_dead()) else { return };
        let u = f.unit;
        let still_member = f.squad == Some(sid);
        let takeover = self.qrf_drafted(sid)
            || self.op_squad_target(sid).is_some()
            || self.op.over.squad_intents.contains_key(&sid);
        // A wounded unit may already have left the squad in the damage
        // callback. Restore its controls too, without erasing its repair order
        // or overriding a new tactical owner.
        if !springai::reserved::is_reserved(self.cb, id) && !self.squad_strike_owns(id) && !self.fire_controls(id) {
            if still_member && !takeover {
                u.stop(0);
            }
            if id == launcher {
                u.set_fire_state(0, 0);
            } else {
                u.set_fire_state(2, 0);
                crate::military::fire_squad::release_unit(u);
            }
        }
        if retreat && !self.retreat_handler.is_coward(id) {
            self.retreat_add_coward(u);
        }
    }
    fn lobster_strike_release(&mut self, sid: SquadId, m: Mission) {
        self.lobster_strike_abort(sid);
        for &id in m.members.iter().chain(std::iter::once(&m.launcher)) {
            self.lobster_strike_release_unit(sid, id, m.launcher, m.retreat.contains(&id));
        }
        self.strikes.lobsters.retry.insert(sid, self.frame + 300);
        self.log(&format!("strikes: Lobster/Buoy squad {sid} released from {}", m.job.name));
    }
    pub(super) fn lobster_strike_cancel(&mut self, sid: SquadId) {
        if let Some(m) = self.strikes.lobsters.missions.remove(&sid) {
            self.lobster_strike_release(sid, m);
        }
    }
    pub(super) fn lobster_strike_damaged(&mut self, id: i32) {
        let sid = self
            .strikes
            .lobsters
            .missions
            .iter()
            .find(|(_, m)| {
                matches!(m.state, State::Gathering | State::Assessing) && (m.launcher == id || m.members.contains(&id))
            })
            .map(|(&sid, _)| sid);
        if let Some(sid) = sid {
            if self.lobster_strike_throw(sid).is_some_and(|t| t.ready) {
                // Already gathered: resubmit with the damaged unit's real HP.
                // A stale approval must not launch, but taking one sniper hit
                // is not a veto on an otherwise profitable, immediate throw.
                let m = self.strikes.lobsters.missions.get_mut(&sid).unwrap();
                m.state = State::Gathering;
                m.query = None;
                m.answer = None;
            } else {
                // A spread squad gathering under fire can resume its normal
                // evasion instead of standing at an unfinished launch point.
                self.lobster_strike_cancel(sid);
            }
        }
    }
    pub(super) fn lobster_strikes_update(&mut self) {
        let mut ids: BTreeSet<_> = self.strikes.lobsters.missions.keys().copied().collect();
        ids.extend(self.squad_handler.squads.iter().copied().filter(|sid| self.lobster_strike_squad(*sid)));
        for sid in ids {
            self.lobster_strike(sid);
        }
        self.strikes.lobsters.retry.retain(|sid, at| *at > self.frame && self.squads.get(*sid).is_some());
    }
    pub(super) fn lobster_strike(&mut self, sid: SquadId) -> Option<Vec3> {
        if let Some(p) = self.strikes.lobsters.predictor.as_ref() {
            while let Ok(a) = p.rx.try_recv() {
                if let Some(m) = self
                    .strikes
                    .lobsters
                    .missions
                    .values_mut()
                    .find(|m| m.query.as_ref().is_some_and(|q| q.key == a.key))
                {
                    m.answer = Some(a);
                }
            }
        }
        if let Some(mut m) = self.strikes.lobsters.missions.remove(&sid) {
            let old_members = m.members.clone();
            m.members.retain(|id| self.fighters.get(id).is_some_and(|f| !f.is_dead() && f.squad == Some(sid)));
            for id in old_members.iter().filter(|id| !m.members.contains(id)) {
                self.lobster_strike_release_unit(sid, *id, m.launcher, m.retreat.contains(id));
            }
            let before_flight = matches!(m.state, State::Gathering | State::Assessing | State::Launching);
            let launcher_ok = self.fighters.get(&m.launcher).is_some_and(|f| {
                !f.is_dead()
                    && f.squad == Some(sid)
                    && !springai::reserved::is_reserved(self.cb, f.id)
                    && !self.qrf_claims(f.id)
                    && !self.retreat_handler.is_retreating(f.id)
            });
            if !self.lobster_strike_available(sid)
                || m.members.is_empty()
                || self.frame - m.since >= DIVE_TIMEOUT
                || (before_flight && (!launcher_ok || m.members != old_members))
            {
                self.lobster_strike_release(sid, m);
                return None;
            }
            let job = self.lobster_strike_refresh(&m.job);
            if let Some(j) = job {
                m.job = j;
            } else {
                m.returning = true;
            }
            let throw = self.lobster_strike_throw(sid);
            // Keep an already launched squad's flight undisturbed after a kill.
            if m.returning && !throw.is_some_and(|t| t.fired) {
                self.lobster_strike_release(sid, m);
                return None;
            }
            match m.state {
                State::Gathering => {
                    let Some(plan) = throw else {
                        self.lobster_strike_release(sid, m);
                        return None;
                    };
                    if plan.ready {
                        let clear = m.members.iter().all(|id| {
                            self.fighters.get(id).is_some_and(|f| {
                                let p = f.get_pos();
                                let end = Vec3::xz(p.x + plan.to.x - plan.from.x, p.z + plan.to.z - plan.from.z);
                                f.unit.def().is_some_and(|d| self.strike_target_point(d, end, m.job.pos).is_some())
                            })
                        });
                        if !clear || self.missiles_ground_burning(plan.to) {
                            self.lobster_strike_release(sid, m);
                            return None;
                        }
                        if self.strikes.lobsters.predictor.is_none() {
                            match Predictor::start() {
                                Ok(p) => self.strikes.lobsters.predictor = Some(p),
                                Err(e) => {
                                    self.log(&format!("strikes: Lobster predictor unavailable: {e}"));
                                    self.strikes.model_failed = true;
                                    self.lobster_strike_release(sid, m);
                                    return None;
                                }
                            }
                        }
                        self.strikes.serial += 1;
                        let key = self.strikes.serial;
                        if let Some(r) = self.strike_prediction_request(
                            key,
                            &m.job,
                            &m.members,
                            plan.from,
                            self.defs.get("amphfloater"),
                            false,
                        ) {
                            let r = r.with_throw(
                                plan.to.x - plan.from.x,
                                plan.to.z - plan.from.z,
                                STRIKE_AIM_FRAMES,
                                FLIGHT,
                            );
                            if self.strikes.lobsters.predictor.as_ref().unwrap().tx.try_send(r).is_ok() {
                                m.query = Some(Query {
                                    key,
                                    at: self.frame,
                                    plan,
                                    pos: m.job.pos,
                                    members: m.members.clone(),
                                });
                                m.state = State::Assessing;
                            }
                        }
                    }
                }
                State::Assessing => {
                    let q = m.query.as_ref().unwrap();
                    if throw.is_none() || self.frame - q.at >= 90 {
                        self.lobster_strike_release(sid, m);
                        return None;
                    }
                    if let Some(a) = m.answer.take() {
                        if a.key != q.key
                            || a.members != m.members
                            || q.members != m.members
                            || distance(q.pos, m.job.pos) > 80.0
                            || !launchable(a.probability)
                            || !self.lobster_strike_launch(sid, q.plan)
                        {
                            self.lobster_strike_release(sid, m);
                            return None;
                        }
                        m.state = State::Launching;
                        m.since = self.frame;
                        if self.strikes.wave.as_ref().is_some_and(|w| {
                            matches!(w.phase, Phase::Forming | Phase::Screening) && same_job(&w.job, &m.job)
                        }) {
                            let w = self.strikes.wave.take().unwrap();
                            self.strikes_release(w);
                        }
                        for id in m.members.iter().chain(std::iter::once(&m.launcher)) {
                            if self.retreat_handler.is_coward(*id) {
                                m.retreat.push(*id);
                                if let Some(f) = self.fighters.get(id) {
                                    self.retreat_remove_unit(f.unit);
                                }
                            }
                        }
                        self.log(&format!(
                            "strikes: Lobster {} throws squad {sid} ({} units) at {} (target death {:.0}%)",
                            m.launcher,
                            m.members.len(),
                            m.job.name,
                            a.probability * 100.0
                        ));
                    }
                }
                State::Launching => {
                    if throw.is_some_and(|t| t.fired) {
                        m.state = State::Flying;
                    } else if throw.is_none() {
                        self.lobster_strike_release(sid, m);
                        return None;
                    }
                }
                State::Flying => {
                    if throw.is_none() {
                        m.state = State::Attacking;
                    }
                }
                State::Attacking => {
                    if !self.strike_members_reach(&m.members, m.job.pos) {
                        self.lobster_strike_release(sid, m);
                        return None;
                    }
                    if self.frame - m.commanded >= 15 {
                        let visible = m
                            .job
                            .id
                            .and_then(|id| self.war.targets.get(&id))
                            .filter(|e| e.visible && !e.is_dead)
                            .map(|e| e.unit);
                        for (i, id) in m.members.iter().enumerate() {
                            let Some(f) = self.fighters.get(id) else { continue };
                            let u = f.unit;
                            u.set_fire_state(2, 0);
                            self.fighters.get_mut(id).unwrap().target = m.job.pos;
                            if let Some(t) = visible {
                                if distance(u.pos(), t.pos()) <= u.max_range().max(80.0) * 0.95 {
                                    u.attack(t, 0);
                                } else {
                                    let desired =
                                        crate::util::get_directional_point(t.pos(), u.pos(), u.max_range() * 0.8);
                                    if let Some(p) = self.strike_move_point(u, desired, t.pos(), u.max_range()) {
                                        kgb_move_to(u, p, 0, self.frame);
                                    } else {
                                        u.stop(0);
                                    }
                                }
                            } else {
                                let angle = (self.frame - m.since) as f32 / 90.0
                                    + i as f32 * std::f32::consts::TAU / m.members.len() as f32;
                                let desired =
                                    Vec3::xz(m.job.pos.x + angle.cos() * 160.0, m.job.pos.z + angle.sin() * 160.0);
                                if let Some(p) = self.strike_move_point(u, desired, m.job.pos, u.max_range()) {
                                    kgb_move_to(u, p, 0, self.frame);
                                } else {
                                    u.stop(0);
                                }
                            }
                        }
                        m.commanded = self.frame;
                    }
                }
            }
            let goal = m.job.pos;
            self.strikes.lobsters.missions.insert(sid, m);
            return Some(goal);
        }
        if !self.lobster_strike_available(sid)
            || self.strikes.model_failed
            || self.strikes.lobsters.retry.get(&sid).is_some_and(|at| *at > self.frame)
            || self.strikes.lobsters.missions.values().any(|m| matches!(m.state, State::Gathering | State::Assessing))
        {
            return None;
        }
        self.strikes.lobsters.retry.insert(sid, self.frame + 30);
        let members: Vec<_> = self
            .squads
            .get(sid)?
            .fighters
            .iter()
            .copied()
            .filter(|id| self.fighters.get(id).is_some_and(|f| !f.is_dead()))
            .collect();
        let jobs: Vec<_> = self
            .strike_jobs(SNIPER_MEMORY)
            .into_iter()
            .filter(|j| {
                matches!(j.prey, Prey::GlassCannon | Prey::Fragile)
                    && !self.strike_cooling(j.pos)
                    && !self.squad_strike_job_taken(j)
                    && !self.strikes.wave.as_ref().is_some_and(|w| w.phase == Phase::Diving && same_job(&w.job, j))
            })
            .collect();
        for job in jobs {
            if self.missiles_ground_burning(job.pos) || !self.lobster_strike_begin(sid, job.pos) {
                continue;
            }
            self.fire_release(sid);
            let launcher = self.lobster_strike_throw(sid)?.launcher;
            for id in &members {
                if let Some(f) = self.fighters.get(id) {
                    f.unit.set_fire_state(2, 0);
                    f.unit.set_move_state(0, 0);
                    f.unit.execute_custom_command(cmd::UNIT_AI, &[0.0], 0, NO_TIMEOUT);
                }
            }
            let goal = job.pos;
            self.strikes.lobsters.missions.insert(
                sid,
                Mission {
                    job,
                    members,
                    launcher,
                    state: State::Gathering,
                    since: self.frame,
                    query: None,
                    answer: None,
                    retreat: Vec::new(),
                    returning: false,
                    commanded: -30,
                },
            );
            return Some(goal);
        }
        None
    }
    pub(super) fn lobster_strikes_status(&self) -> serde_json::Value {
        serde_json::json!(self.strikes.lobsters.missions.iter().map(|(sid,m)|serde_json::json!({"squad":sid,"launcher":m.launcher,"target":m.job.id,"def":m.job.name,"members":m.members,"phase":format!("{:?}",m.state).to_lowercase(),"returning":m.returning})).collect::<Vec<_>>())
    }
}
