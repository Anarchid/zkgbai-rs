//! Ask whether staying to finish a turret beats this builder withdrawing.
//! Unlike the static threat map, the rollout includes the vulnerable frame,
//! observed construction throughput, and its weapons becoming operational.
use std::collections::BTreeMap;
use std::time::Instant;

use serde_json::json;
use springai::{Unit, reserved};

use super::{TaskId, TaskKind, WorkerTask};
use crate::Ai;
use crate::military::fightsim::UnitFate;
use crate::military::fightsim::local::{Local, SUPPORT, tally};
use crate::military::fightsim::mech::{BuildHand, Construction};
use crate::util::distance;

const PERIOD: i32 = 15;
const FRESH: i32 = 30;
const HORIZON: i32 = 450;
const MARGIN: f32 = 20.0;

#[derive(Default)]
pub(super) struct BuildGuard {
    verdicts: BTreeMap<i32, Verdict>,
    hits: BTreeMap<i32, (i32, Option<i32>)>,
    emitted: BTreeMap<i32, i32>,
    checked: BTreeMap<i32, (i32, TaskId)>,
}

struct Verdict {
    frame: i32,
    task: TaskId,
    target: Unit,
    hp: f32,
    target_hp: f32,
    enemies: Vec<i32>,
}

fn build_target(task: &WorkerTask) -> Option<Unit> {
    match &task.kind {
        TaskKind::Construction(c) => c.target,
        TaskKind::Repair(r) => Some(r.target),
        _ => None,
    }
}

/// Operational value is credited only to a finished, surviving turret;
/// additional construction is paid for in both branches. Its already paid
/// metal remains at risk even if the builder leaves it unfinished.
fn score(local: &Local, fates: &[UnitFate], plan: &Construction, progress: f32) -> f32 {
    let mut tally = tally(local, fates, true);
    let cost = local.ours[plan.target].cost;
    // The ordinary tally prices a nanoframe as a whole finished unit.
    tally.own_loss -= cost * (1.0 - plan.progress) * fates[plan.target].p_raw;
    let operational = if progress >= 1.0 { cost * (1.0 - fates[plan.target].p_raw) } else { 0.0 };
    tally.net() + operational - cost * (progress - plan.progress).max(0.0)
}

fn worthwhile(stay: &UnitFate, commander: bool, max_hp: f32, gain: f32, finished: bool) -> bool {
    finished && gain > MARGIN && (!commander || (stay.p_raw < 0.3 && stay.hp_left >= 0.1 * max_hp))
}

impl Ai {
    pub(super) fn eco_build_guard_hit(&mut self, unit: Unit, attacker: Option<Unit>) {
        if self.eco.workers.contains_key(&unit.id()) && attacker.is_none_or(|a| a.ally_team() != self.ally_team_id) {
            self.eco.build_guard.hits.insert(unit.id(), (self.frame, attacker.map(|a| a.id())));
        }
    }

    /// A fresh answer belongs to this exact worker, task and nanoframe.
    /// New/unseen damage and changing ownership invalidate it immediately.
    pub(super) fn eco_build_guard_task(&self, wid: i32) -> Option<TaskId> {
        if self.dev_off("buildguard") || self.eco.hard_estalled {
            return None;
        }
        let v = self.eco.build_guard.verdicts.get(&wid)?;
        let w = self.eco.workers.get(&wid)?;
        let target = build_target(self.eco.tasks.get(v.task)?)?;
        let hit_covered = self
            .eco
            .build_guard
            .hits
            .get(&wid)
            .is_none_or(|&(at, shooter)| self.frame - at > 90 || shooter.is_some_and(|id| v.enemies.contains(&id)));
        (self.frame - v.frame <= FRESH
            && w.task == Some(v.task)
            && target == v.target
            && !w.is_chicken
            && w.unit.team() == self.team_id
            && w.unit.health() >= 0.9 * v.hp
            && v.target.health() >= 0.9 * v.target_hp
            && v.target.is_being_built()
            && (v.target.team() == self.team_id || self.allies.contains(&v.target.team()))
            && hit_covered
            && !reserved::is_reserved(self.cb, wid)
            && distance(w.get_pos(), v.target.pos()) <= w.unit.def()?.build_distance()
            && w.unit.resource_use(self.metal) > 0.0)
            .then_some(v.task)
    }

    pub(super) fn eco_guard_construction(&mut self, frame: i32) {
        if frame % PERIOD != self.offset % PERIOD {
            return;
        }
        self.eco.build_guard.hits.retain(|id, (at, _)| self.eco.workers.contains_key(id) && frame - *at <= 90);
        if self.dev_off("buildguard") || self.eco.hard_estalled {
            self.eco.build_guard.verdicts.clear();
            return;
        }
        let mut ids: Vec<_> = self.eco.workers.keys().copied().collect();
        if !ids.is_empty() {
            let offset = (frame / PERIOD) as usize % ids.len();
            ids.rotate_left(offset);
        }
        self.eco.build_guard.emitted.retain(|id, _| self.eco.workers.contains_key(id));
        self.eco.build_guard.checked.retain(|id, _| self.eco.workers.contains_key(id));
        let start = Instant::now();
        for id in ids {
            // Bound synchronous work; skipped answers expire rather than
            // granting an unexamined construction a safety exemption.
            if start.elapsed().as_micros() > 5000 {
                break;
            }
            self.eco_refresh_build_guard(id);
        }
        self.eco.build_guard.verdicts.retain(|id, v| self.eco.workers.contains_key(id) && frame - v.frame <= FRESH);
    }

    /// Also called at the actual retreat decision: a newly arrived enemy
    /// must not make the worker flee before the next periodic prediction.
    pub(super) fn eco_refresh_build_guard(&mut self, id: i32) -> bool {
        if self.dev_off("buildguard") || self.eco.hard_estalled {
            return false;
        }
        let task = self.eco.workers.get(&id).and_then(|w| w.task);
        if self.eco.build_guard.checked.get(&id).is_some_and(|&(at, old)| Some(old) == task && self.frame - at < PERIOD)
        {
            return self.eco_build_guard_task(id).is_some();
        }
        let was = self.eco.build_guard.verdicts.remove(&id);
        let emit = self.eco.build_guard.emitted.get(&id).is_none_or(|at| self.frame - at >= 90);
        if let Some(v) = self.eco_judge_construction(id, emit) {
            self.eco.build_guard.verdicts.insert(id, v);
        } else if was.is_some() {
            self.op_emit("buildguard", json!({"event":"release", "id":id, "f":self.frame}));
        }
        self.eco_build_guard_task(id).is_some()
    }

    fn eco_judge_construction(&mut self, wid: i32, emit: bool) -> Option<Verdict> {
        let w = self.eco.workers.get(&wid)?;
        let task = w.task?;
        let t = self.eco.tasks.get(task)?;
        let target = build_target(t)?;
        let worker = w.unit;
        let def = target.def()?;
        let built = target.build_progress();
        if w.is_chicken
            || w.raid_defense.is_some()
            || worker.health() <= 0.0
            || !target.is_being_built()
            || target.health() <= 0.0
            || !(0.0..1.0).contains(&built)
            || def.speed() > 0.0
            || !def.is_able_to_attack()
            || def.weapon_mounts().is_empty()
            || (target.team() != self.team_id && !self.allies.contains(&target.team()))
            || reserved::is_reserved(self.cb, wid)
            || self.eco_in_transit(wid)
            || !self.eco_worker_can_take(w, t)
        {
            return None;
        }
        let pos = target.pos();
        if !self.war.targets.values().any(|e| {
            !e.is_dead
                && e.can_attack()
                && (e.is_static || self.frame - e.last_seen <= 90)
                && distance(e.position, pos) <= SUPPORT
        }) {
            return None;
        }
        let cost = def.cost(self.metal).max(1.0);
        let work = def.build_time().max(1.0);
        let rate = |u: Unit| -> Option<(f32, f32)> {
            let d = u.def()?;
            let range = d.build_distance();
            let rate = (d.build_speed() / work).min(u.resource_use(self.metal) / cost);
            (u.health() > 0.0
                && !u.is_being_built()
                && !u.is_paralyzed()
                && distance(u.pos(), pos) <= range
                && rate.is_finite()
                && rate > 0.0)
                .then_some((rate, range))
        };
        let (our_rate, our_range) = rate(worker)?;
        let mut units = vec![worker, target];
        let mut hands = vec![BuildHand { index: 0, rate: our_rate, range: our_range }];
        let mut friends = self.cb.friendly_units_in(pos, 800.0);
        friends.sort_by(|a, b| distance(a.pos(), pos).total_cmp(&distance(b.pos(), pos)));
        for u in friends {
            if units.iter().any(|x| x.id() == u.id()) || u.health() <= 0.0 || u.is_being_built() {
                continue;
            }
            let assigned = self.eco.workers.get(&u.id()).is_some_and(|w| {
                !w.is_chicken && w.task.and_then(|id| self.eco.tasks.get(id)).and_then(build_target) == Some(target)
            });
            let assisting = assigned
                || u.current_commands().first().is_some_and(|cmd| {
                    cmd.id() == 40 && cmd.params().first().is_some_and(|p| *p as i32 == target.id())
                });
            if assisting && let Some((rate, range)) = rate(u) {
                hands.push(BuildHand { index: units.len(), rate, range });
                units.push(u);
            } else if !self.retreat_handler.is_retreating(u.id())
                && u.def().is_some_and(|d| d.is_able_to_attack() && !d.weapon_mounts().is_empty())
            {
                units.push(u);
            }
            if units.len() >= 40 {
                break;
            }
        }
        let total: f32 = hands.iter().map(|h| h.rate).sum();
        if (1.0 - built) / total > HORIZON as f32 / 30.0 {
            return None;
        }
        let mut local = self.local_fight(&units)?;
        // local_fight omits dead/unreadable units. Do not apply indices to a
        // different census, and do not override retreat around unknown fire.
        if local.unknown
            || !local.armed_theirs()
            || local.ours.len() != units.len()
            || !local.ours.iter().zip(&units).all(|(o, u)| o.id == u.id())
            || local.snap.units[1].spec.weapons.is_empty()
        {
            return None;
        }
        // This rollout seeds EMP explicitly, rather than treating it as HP
        // damage as the generic local census does.
        for (snap, u) in local.snap.units.iter_mut().zip(&units) {
            snap.hp = u.health() * (1.0 - u.capture_progress());
        }
        let enemies: Vec<_> = local.theirs.iter().map(|e| e.id).collect();
        if self
            .eco
            .build_guard
            .hits
            .get(&wid)
            .is_some_and(|&(at, shooter)| self.frame - at <= 90 && shooter.is_none_or(|id| !enemies.contains(&id)))
        {
            return None;
        }
        let paralysis: Vec<_> =
            units.iter().enumerate().map(|(i, u)| (i, u.paralyze_damage() / u.max_health().max(1.0))).collect();
        let plan = Construction { target: 1, progress: built, builders: hands, retreat: None, release: Some(0) };
        self.eco.build_guard.checked.insert(wid, (self.frame, task));
        let model = self.local_model()?;
        let (stay, done) = model.construction_fates(&local.snap, HORIZON, &plan, &paralysis);
        let leave_plan = Construction { retreat: Some(0), ..plan.clone() };
        let (leave, left) = model.construction_fates(&local.snap, HORIZON, &leave_plan, &paralysis);
        let gain = score(&local, &stay, &plan, done) - score(&local, &leave, &plan, left);
        let commander = local.snap.units[0].spec.com;
        let hold = worthwhile(
            &stay[0],
            commander,
            worker.max_health(),
            gain,
            done >= 1.0 && !stay[1].dead && stay[1].p_raw < 0.5,
        );
        if emit {
            self.eco.build_guard.emitted.insert(wid, self.frame);
            self.op_emit(
                "buildguard",
                json!({"event":if hold {"stay"} else {"leave"}, "id":wid,
                "target":target.id(), "progress":built, "seconds":(1.0-built)/total,
                "gain":gain, "stay_dead":stay[0].p_raw, "leave_dead":leave[0].p_raw,
                "stay_finished":done >= 1.0, "leave_finished":left >= 1.0,
                "turret_dead":stay[1].p_raw, "crew":plan.builders.len()}),
            );
        }
        hold.then_some(Verdict {
            frame: self.frame,
            task,
            target,
            hp: worker.health(),
            target_hp: target.health(),
            enemies,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::super::{Worker, WorkerTask};
    use super::*;
    use springai::UnitDef;

    #[test]
    fn prediction_keeps_its_exact_job_and_yields_to_uncovered_fire_or_stalls() {
        unsafe extern "C" fn spending(_: i32, _: i32, _: i32) -> f32 {
            10.0
        }
        unsafe extern "C" fn building(_: i32, id: i32) -> bool {
            id == 12
        }
        unsafe extern "C" fn progress(_: i32, _: i32) -> f32 {
            0.5
        }
        let mut ai = super::super::assign::tests::assignment_ai(|t| {
            t.Unit_getResourceUse = Some(spending);
            t.Unit_isBeingBuilt = Some(building);
            t.Unit_getBuildProgress = Some(progress);
        });
        let worker = Unit::from_id(ai.cb, 10).unwrap();
        let target = Unit::from_id(ai.cb, 12).unwrap();
        let def = UnitDef::from_id(ai.cb, 1).unwrap();
        let mut job = WorkerTask::construction(def, target.pos(), 0);
        job.as_construction_mut().unwrap().target = Some(target);
        let task = ai.eco.tasks.insert(job);
        let other = ai.eco.tasks.insert(WorkerTask::construction(def, worker.pos(), 0));
        ai.eco.construction_tasks.extend([task, other]);
        let mut w = Worker::new(worker, ai.team_id);
        w.set_task(&mut ai.eco.tasks, task, 0);
        ai.eco.workers.insert(w.id, w);
        ai.eco.build_guard.verdicts.insert(
            10,
            Verdict {
                frame: ai.frame,
                task,
                target,
                hp: worker.health(),
                target_hp: target.health(),
                enemies: vec![42],
            },
        );
        assert_eq!(ai.eco_build_guard_task(10), Some(task));
        assert!(!ai.eco_project_hand(10, other));
        ai.eco_assign_worker(10);
        assert_eq!(ai.eco.workers[&10].task, Some(task));
        ai.eco.build_guard.hits.insert(10, (ai.frame, Some(42)));
        assert_eq!(ai.eco_build_guard_task(10), Some(task), "judged enemy fire is covered");
        ai.eco.build_guard.hits.insert(10, (ai.frame, None));
        assert_eq!(ai.eco_build_guard_task(10), None, "unseen fire invalidates a prediction");
        ai.eco.build_guard.hits.clear();
        ai.eco.hard_estalled = true;
        assert_eq!(ai.eco_build_guard_task(10), None);
        ai.eco.hard_estalled = false;
        ai.eco.workers.get_mut(&10).unwrap().task = Some(other);
        assert_eq!(ai.eco_build_guard_task(10), None, "the answer cannot follow a different project");
        ai.eco.workers.get_mut(&10).unwrap().task = Some(task);
        ai.frame += FRESH + 1;
        assert_eq!(ai.eco_build_guard_task(10), None, "stale answers do not suppress retreat");
    }
    #[test]
    fn completion_can_pay_for_a_constructor_but_not_a_doomed_commander() {
        let endangered = UnitFate { p_raw: 0.7, hp_left: 0.0, ..Default::default() };
        assert!(worthwhile(&endangered, false, 200.0, 100.0, true));
        assert!(!worthwhile(&endangered, true, 3000.0, 1000.0, true));
        assert!(!worthwhile(&endangered, false, 200.0, 100.0, false));
        assert!(!worthwhile(&endangered, false, 200.0, 10.0, true));
    }
}
