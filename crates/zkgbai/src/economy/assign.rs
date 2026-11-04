//! Worker assignment: job costs, the round-robin assignment loop, task
//! validation and cleanup.

use springai::{UnitDef, Vec3};

use super::planning::{FACTORY_CLEARANCE, exit_lane, keeps_exit};
use super::worker::Approach;
use super::{NEED_PULL, SITE_SLACK, TaskId, TaskKind, Worker, WorkerTask, erase_one, erase_one_dq};
use crate::Ai;
use crate::military::DefenseTarget;
use crate::terrain::pos as terrain_pos;
use crate::util::{NULLPOS, OPTION_SHIFT_KEY, distance};
use zk_terrain::Tier;
use zk_terrain::paths::SNAP;

/// Cost of a job a worker must never take.
// port: the C++ used 65535/65536 here together with "first task always
// wins" (`task == nullptr || tmpcost < cost`), which could hand a worker
// another commander's factory plop; the Java's Float.MAX_VALUE with strict
// comparisons is restored.
const NEVER: f32 = f32::MAX;
/// Finish the last few seconds of active construction before rebidding its
/// builder elsewhere. Use actual spending, so a stalled repair cannot pin it.
const FINISH_SECONDS: f32 = 10.0;
const FINISH_PROGRESS: f32 = 0.80;

/// Constables pair up on builds: a Constable within PAIR_RANGE already on a
/// job makes it PAIR_BONUS cheaper to join (and is no crowd).
const PAIRED_CON: &str = "jumpcon";
const PAIR_RANGE: f32 = 500.0;
const PAIR_BONUS: f32 = 150.0;

/// Raid memory: enemy armed ground units seen within RAID_MEMORY frames and
/// RAID_RADIUS of a job make it cost RAID_COST more (plus their danger),
/// unless our power there is more than theirs / RAID_HOLD. Against
/// jumpjets most of our constructors lost before minute 15 died building
/// turrets where raiders had just been. Off with `disable=raidmem`.
const RAID_MEMORY: i32 = 30 * 20;
const RAID_RADIUS: f32 = 800.0;
const RAID_HOLD: f32 = 0.8;
const RAID_COST: f32 = 1500.0;

/// Metal spots only one type of our constructors gets to (out at sea, say,
/// for aircraft): one of those for every so many spots, up to a few, and a
/// discount that puts those ahead of their other jobs (the factory built
/// them for it), unless it is dangerous out there (threat costs 500 a
/// point).
const STRANDED_PER_CON: usize = 3;
const STRANDED_CONS: usize = 3;
const STRANDED_BONUS: f32 = 3000.0;

/// A missile silo is rushed: the constructors near it are summoned, about
/// this many before more of them cost extra.
const SQUAD: f32 = 4.0;
const SQUAD_PULL: f32 = 3000.0;

/// A worker on its way to its own task out of build range was re-sent its
/// move + build every time it was serviced (every 10 frames or so), and a
/// fresh raw move never reaches ZK's own stuck handling (cmd_raw_move steps
/// a stuck unit aside after 6-10 s), nor does the unstick test below fire on
/// a unit re-ordered that often: on Horus a recon commander (build distance
/// 144) stood 2-7 minutes behind our own solar, ordered 658 times, while
/// metal floated. With `approach` a worker that has not got APPROACH_GAIN
/// nearer, nor travelled APPROACH_TRAVEL (a detour), in APPROACH_SLOW is
/// left to its order up to APPROACH_RESEND (one making headway is re-sent
/// it every time, as before: holding every order 10 s cost income on every
/// factory in horus/fix ab1), and one that has not in APPROACH_STALL drops
/// the task and shuns the site for APPROACH_SHUN (within
/// APPROACH_SHUN_RADIUS). On by default since Oct 27 (`disable=approach`).
const APPROACH_SLOW: i32 = 4 * 30;
const APPROACH_RESEND: i32 = 10 * 30;
const APPROACH_GAIN: f32 = 32.0;
const APPROACH_TRAVEL: f32 = 128.0;
const APPROACH_STALL: i32 = 20 * 30;
const APPROACH_SHUN: i32 = 60 * 30;
const APPROACH_SHUN_RADIUS: f32 = 64.0;
/// A combat unit's repair pulls a worker by the metal its missing health is
/// worth (`lightrepair`): in full from REPAIR_FULL_METAL up, less below. The
/// flat pull (250 + health / 10) outbid a mex beside the worker (100 + up to
/// 100 for free spots near) for a Dart at half health, 60 HP or 20 metal,
/// which Zero-K heals for free once it idles (5 HP/s after 60 s; Glaives 20
/// HP/s after 5 s). Off with `disable=lightrepair`.
const REPAIR_FULL_METAL: f32 = 150.0;

/// How much of a combat unit's repair pull it gets (0..=1): the metal worth
/// of its missing health over REPAIR_FULL_METAL.
fn repair_worth(cost: f32, health: f32, max_health: f32) -> f32 {
    if max_health <= 0.0 {
        return 1.0;
    }
    let missing = cost * ((max_health - health) / max_health).clamp(0.0, 1.0);
    (missing / REPAIR_FULL_METAL).min(1.0)
}

/// A lone mex's pull on a worker beside it, no free spots near.
const LONE_MEX_PULL: f32 = 100.0;

/// Whether a lone worker beside a mobile combat unit would rather build a
/// lone mex there than repair it (`lightrepair`). A retreated unit like that
/// does not wait at home for a repair (see `retreat`).
pub(crate) fn repair_loses_to_mex(cost: f32, health: f32, max_health: f32) -> bool {
    repair_worth(cost, health, max_health) * (250.0 + (max_health + health) / 10.0) < LONE_MEX_PULL
}

/// Spring's repair and reclaim command ids (a build's is minus the def id).
const CMD_REPAIR: i32 = 40;
const CMD_RECLAIM: i32 = 90;

/// A metal spot with free ones near it is worth a little more than a lone
/// one: whoever takes it goes straight on to the next. Every free spot
/// within the radius adds up to the bonus, the nearer the more, up to the
/// cap (on the scale of job costs: a quarter of the elmos away, so the cap
/// is worth 400 elmos of walking).
const CLUSTER_RADIUS: f32 = 600.0;
const CLUSTER_BONUS: f32 = 40.0;
const CLUSTER_CAP: f32 = 100.0;

/// How much the free spots `free` near a spot at `p` make it worth.
fn cluster_bonus(p: Vec3, free: impl Iterator<Item = Vec3>) -> f32 {
    let near = |q: Vec3| {
        let d = distance(p, q);
        if d < 1.0 { 0.0 } else { (1.0 - d / CLUSTER_RADIUS).max(0.0) }
    };
    (CLUSTER_BONUS * free.map(near).sum::<f32>()).min(CLUSTER_CAP)
}

/// Mexes before a new factory (`remexfirst`, default on): a worker whose
/// cheapest job is a factory, strider hub or plate (not a commander's plop,
/// nor a factory while we have none) builds a free metal spot within
/// REMEX_RADIUS of it first, if that mex, its crowd and danger counted,
/// costs under REMEX_CEILING (an unshared, unraided mex out to the radius
/// does; a third worker on one, or raiders still near it, do not). A
/// factory pulls by its metal cost (~750 against a mex's 100, plus up to
/// 100 for free spots near), and raids that cost us mexes ask for an air
/// factory (`airfire`) whose crew bonus comes on top: the constructors
/// built the air factory in a base whose mexes had just died (user, 11-04).
/// Zero-K has no construction decay, so a frame left for a mex waits.
/// A worker's own mex is priced REMEX_KEEP lower: a commander joining it
/// (+600 to the others' price) flipped a constructor between the mex and
/// the factory 1360 elmos away every second while the commander's raid
/// defense took it off and gave it back (Altair smoke, 11-04); raiders near
/// it (RAID_COST) still send it off.
const REMEX_RADIUS: f32 = 1200.0;
const REMEX_CEILING: f32 = 300.0;
const REMEX_KEEP: f32 = 700.0;
/// The missile silo is rushed (SQUAD_PULL), and half the time it is for
/// offense, so only a mex the worker stands at, within its build range or
/// REMEX_STEP past it, goes before it (user, 11-04: "if the constructor is
/// literally sitting on the mex, it just is worth it to build").
const REMEX_STEP: f32 = 64.0;

/// An order for a worker, decided while the task store is borrowed.
enum Order {
    Build(UnitDef, Vec3, i32),
    Repair(springai::Unit),
    ReclaimFeature(springai::Feature),
    ReclaimUnit(springai::Unit),
}

impl Ai {
    pub(crate) fn eco_worker_held(&self, id: i32) -> bool {
        self.eco.workers.get(&id).is_some_and(|w| {
            w.raid_defense.is_some()
                || self.crab_escort(id)
                || self.rush_push_escort(id)
                || self.rush_holds(id)
                || self.logistics_holds(id)
                || self.eco_in_transit(id)
                || self.eco.terraform_hold.get(&id).is_some_and(|until| self.frame < *until)
                || super::terraform::is_terraforming(w.unit)
        })
    }

    pub(super) fn eco_assign_workers(&mut self) {
        let frame = self.frame;
        if frame < 8 {
            return;
        }

        // Generate tasks and assign workers to tasks on alternating frames.
        // Also refresh retreat paths for chickening units and process them when they're done chickening.
        if frame % 2 == 0 {
            if let Some(wid) = self.eco.idlers.pop_front() {
                self.eco_service_worker(wid);
            } else if let Some(wid) = self.eco.assigned.pop_front() {
                self.eco_service_worker(wid);
            }
        } else if let Some(wid) = self.eco.seeders.pop_front() {
            let Some(unit) = self.eco.workers.get(&wid).map(|w| w.unit) else {
                return;
            };
            if unit.health() <= 0.0 || unit.team() != self.team_id {
                self.eco.remove_worker(wid);
            } else {
                self.eco_generate_tasks(wid);
                self.eco.seeders.push_back(wid);
            }
        }
    }

    /// One step of the idlers/assigned round robin: drop dead workers,
    /// handle chickening (retreating from danger), otherwise (re)assign.
    fn eco_service_worker(&mut self, wid: i32) {
        let frame = self.frame;
        let Some(w) = self.eco.workers.get(&wid) else {
            return;
        };
        let unit = w.unit;
        if unit.health() <= 0.0 || unit.team() != self.team_id {
            self.eco.remove_worker(wid);
            return;
        }

        if self.recovery_owns(wid) {
            self.eco.assigned.push_back(wid);
            return;
        }
        let pos = unit.pos();
        // The threat maps as the worker sees them, under water too (see `water`).
        let view = self.war.view(unit.def());
        // (A worker on the crew of an issue worth more to it than its
        // risk stays: see `logistics`.)
        // The operator's greed dial scales the threat a worker perceives.
        let greed = self.op_greed();
        // The predictor speaks for a commander in a fight it judged (see
        // `comguard`): no flinch from the threat paint.
        let answered = w.is_com && self.comguard_answers(wid) || self.eco_build_guard_task(wid).is_some();
        let should_flee = !w.is_chicken
            && !answered
            && !self.logistics_holds(wid)
            && (view.get_effective_threat(pos) / greed > view.get_total_friendly_threat(pos)
                || ((unit.health() < unit.max_health() && w.raid_defense.is_none() || w.get_shields() < 0.5)
                    && view.get_threat(pos) / greed > 0.0
                    && !w.is_working_on_porc(&self.eco.tasks)
                    && !self.crab_escort(wid)
                    && !self.rush_push_escort(wid)));
        if should_flee && !self.eco_refresh_build_guard(wid) {
            if let Some(w) = self.eco.workers.get_mut(&wid) {
                w.is_chicken = true;
                w.chicken_frame = frame;
            }
            self.war.add_defense_target(DefenseTarget::new(pos, unit.max_health(), frame));
            erase_one_dq(&mut self.eco.seeders, wid);
        }

        let Some(w) = self.eco.workers.get(&wid) else {
            return;
        };
        // Busy terraforming (or just ordered to), or flown to a colony:
        // leave it to it.
        let held = self.eco.terraform_hold.get(&wid).is_some_and(|&until| frame < until)
            || self.crab_escort(wid)
            || self.rush_push_escort(wid)
            || self.rush_holds(wid);
        if !w.is_chicken && (held || super::terraform::is_terraforming(unit) || self.eco_in_transit(wid)) {
            self.eco.assigned.push_back(wid);
            return;
        }
        let (is_chicken, chicken_frame, last_retreat_frame) = (w.is_chicken, w.chicken_frame, w.last_retreat_frame);
        if is_chicken {
            let view = self.war.view(unit.def());
            // A commander the predictor holds out of a fight stays out (see
            // `comguard`), at its own haven.
            let guarded = self.comguard_haven(wid);
            if guarded.is_none()
                && (answered
                    || (view.get_threat(pos) == 0.0
                        && ((view.get_total_friendly_threat(pos) > 0.0 && frame - chicken_frame > 90)
                            || frame - chicken_frame > 150)))
            {
                self.eco_stop_chickening(wid);
            } else if let Some(haven) = guarded {
                if frame - last_retreat_frame > 15 && distance(pos, haven) > 25.0 {
                    self.comguard_retreat(wid, haven);
                }
            } else if frame - last_retreat_frame > 15 {
                // Ahead of the enemy to where it gets last, if it pays.
                let haven = self.incursion_refuge(unit).unwrap_or_else(|| self.graph_get_closest_haven(pos));
                if distance(pos, haven) > 25.0 {
                    let mut nav = crate::nav!(self);
                    if let Some(w) = self.eco.workers.get_mut(&wid) {
                        w.retreat_to(haven, &mut nav);
                        if self.war.get_threat(w.get_pos()) > 0.0 {
                            w.chicken_frame = frame;
                        }
                    }
                    // Constables and recon commanders jump away.
                    self.jump_escape(unit, haven);
                } else {
                    self.eco_stop_chickening(wid);
                }
            }
        } else if self.eco.workers.get(&wid).is_some_and(|w| w.raid_defense.is_some())
            || self.jump_in_progress(unit)
            || self.recovery_owns(wid)
        {
            // Local defense and jumps own the orders until they finish.
        } else {
            // The approach rule handles reassignment; shared recovery owns
            // persistent movement failures instead of resetting them every 4 s.
            self.eco_assign_worker(wid);
        }
        self.eco.assigned.push_back(wid);
    }

    /// Journal worker `wid` leaving the job `old` (a tracked issue's) for
    /// `new`: each job's price as the ladder has it, the crew bonus, the
    /// threat paint at each.
    fn eco_journal_leave(&self, wid: i32, old: TaskId, new: TaskId) {
        let Some(w) = self.eco.workers.get(&wid) else { return };
        let wpos = w.get_pos();
        let view = self.war.view(w.unit.def());
        let one = |id: TaskId| {
            let Some(t) = self.eco.tasks.get(id) else { return serde_json::Value::Null };
            let what = t.as_construction().map_or("other", |c| self.defs.name(c.build_type));
            serde_json::json!({"what": what, "dist": distance(wpos, t.get_pos()).round(),
                               "cost": self.eco_cost_of_job(w, wpos, t).round(),
                               "bonus": self.logistics_bonus(wid, id).map(f32::round),
                               "threat": (view.get_effective_threat(t.get_pos()) * 100.0).round() / 100.0,
                               "raiders": self.eco_raiders_near(t.get_pos()).round()})
        };
        self.op_emit(
            "leave",
            serde_json::json!({"worker": wid, "def": self.defs.name_of(w.unit), "greedy": w.is_greedy,
                               "chicken": w.is_chicken, "from": one(old), "to": one(new)}),
        );
    }

    fn eco_stop_chickening(&mut self, wid: i32) {
        if let Some(w) = self.eco.workers.get_mut(&wid) {
            w.is_chicken = false;
        }
        self.eco_assign_worker(wid);
        self.eco.seeders.push_back(wid);
    }

    pub(super) fn eco_assign_worker(&mut self, wid: i32) {
        let frame = self.frame;
        if springai::reserved::is_reserved(self.cb, wid) || self.recovery_owns(wid) {
            return;
        }
        // Idle, completion, move-failed and retreat-finished callbacks also
        // enter here directly, bypassing the round-robin reservation check.
        if self.crab_escort(wid)
            || self.rush_push_escort(wid)
            || self.rush_holds(wid)
            || self.eco.terraform_hold.get(&wid).is_some_and(|&until| until > frame)
        {
            return;
        }
        if self.eco.workers.get(&wid).is_some_and(|w| w.raid_defense.is_some() || self.jump_in_progress(w.unit)) {
            return;
        }
        // Cancel the old engine order even if no safe replacement exists.
        let blocked_construction = self.eco.workers.get(&wid).is_some_and(|w| {
            w.task.and_then(|id| self.eco.tasks.get(id)).is_some_and(|t| {
                !self.eco.silo_work_allowed(t)
                    || !self.eco_prerequisites_ready(t)
                    || (!self.op_priority(t.id) && !self.logistics_allows(wid, t.id))
                    || (t.as_construction().is_some_and(|c| c.fortification)
                        && self.eco_fortify_travel(w, t.get_pos()).is_none())
            })
        });
        if blocked_construction && let Some(w) = self.eco.workers.get_mut(&wid) {
            w.clear_task(&mut self.eco.tasks);
        }
        let Some(task_id) = self.eco_get_cheapest_job(wid) else {
            return;
        };
        let Some(w) = self.eco.workers.get(&wid) else {
            return;
        };
        let unit = w.unit;
        let is_com = w.is_com;
        let wtask = w.task;
        let wpos = unit.pos();
        let build_dist = unit.def().map_or(0.0, |d| d.build_distance());
        let task_pos = self.eco.task_pos(task_id);

        if wtask == Some(task_id) && distance(wpos, task_pos) <= build_dist * 1.5 {
            // (In range is headway: see `eco_approach_holds`.)
            if let Some(a) = self.eco.workers.get_mut(&wid).and_then(|w| w.approach.as_mut()) {
                (a.best, a.gained, a.anchor) = (distance(wpos, task_pos), frame, wpos);
            }
            return;
        }
        let approach = !self.dev_off("approach");
        if approach && wtask == Some(task_id) && self.eco_approach_holds(wid, task_id, task_pos) {
            return;
        }

        // A worker leaving a tracked issue's job: journaled with both
        // prices, to see what pulls workers off a geothermal plant.
        if let Some(old) = wtask.filter(|&t| t != task_id && self.logistics_tracks(t)) {
            self.eco_journal_leave(wid, old, task_id);
        }
        // remove it from its old assignment if it had one
        if wtask != Some(task_id)
            && let Some(w) = self.eco.workers.get_mut(&wid)
        {
            w.clear_task(&mut self.eco.tasks);
            w.set_task(&mut self.eco.tasks, task_id, frame);
        }

        let out_of_range = distance(wpos, task_pos) > build_dist * 1.5;
        let mut stale = None;
        let Some(task) = self.eco.tasks.get_mut(task_id) else {
            return;
        };
        let order = match &mut task.kind {
            TaskKind::Construction(c) => {
                // (A finished building elsewhere is not this job's: build it.)
                let elsewhere = |t: springai::Unit| !t.is_being_built() && distance(t.pos(), task_pos) >= SITE_SLACK;
                stale = c.target.filter(|&t| t.health() > 0.0 && elsewhere(t)).map(|t| t.pos());
                if c.target.is_some_and(|t| t.health() <= 0.0 || elsewhere(t)) {
                    c.target = None;
                }
                match c.target {
                    None => Order::Build(c.build_type, task_pos, c.facing),
                    Some(t) => Order::Repair(t),
                }
            }
            TaskKind::Reclaim(r) => Order::ReclaimFeature(r.target),
            TaskKind::CombatReclaim(crt) => {
                let target = crt.target;
                // prevent workers from being unassigned to dangerous/invalid combat reclaim jobs
                if (!target.is_being_built() && target.team() != self.team_id)
                    || self.war.get_threat(task_pos) > 0.0
                    || target.health() <= 0.0
                {
                    let idle = self.eco.stop_workers(task_id);
                    for wo in idle {
                        if wo == wid {
                            continue;
                        }
                        self.eco.idlers.push_back(wo);
                        erase_one_dq(&mut self.eco.assigned, wo);
                    }
                    erase_one(&mut self.eco.combat_reclaim_tasks, &task_id);
                    self.eco.delete_task(task_id);
                    return;
                }
                Order::ReclaimUnit(target)
            }
            TaskKind::Repair(r) => Order::Repair(r.target),
        };
        if let Some(p) = stale {
            self.log(&format!(
                "economy: the job at ({:.0}, {:.0}) had a finished building at ({:.0}, {:.0}) for its own",
                task_pos.x, task_pos.z, p.x, p.z
            ));
        }

        let opts = if out_of_range {
            let jump_goal = crate::util::get_directional_point(task_pos, wpos, build_dist * 0.95);
            let jumped = is_com && !self.dev_off("commander") && self.jump_travel(unit, jump_goal, "commander travel");
            if !jumped && let Some(w) = self.eco.workers.get(&wid) {
                let mut nav = crate::nav!(self);
                w.move_to(task_pos, &mut nav);
            }
            if approach && let Some(w) = self.eco.workers.get_mut(&wid) {
                let cmd = match order {
                    Order::Build(def, ..) => -def.id(),
                    Order::Repair(_) => CMD_REPAIR,
                    Order::ReclaimFeature(_) | Order::ReclaimUnit(_) => CMD_RECLAIM,
                };
                let dist = distance(wpos, task_pos);
                // Progress counts across re-sends of the same task's order.
                let (best, gained, anchor) = match w.approach {
                    Some(a) if a.task == task_id => (a.best, a.gained, a.anchor),
                    _ => (dist, frame, wpos),
                };
                w.approach = Some(Approach { task: task_id, cmd, sent: frame, best, gained, anchor });
            }
            OPTION_SHIFT_KEY
        } else {
            0
        };
        match order {
            Order::Build(def, pos, facing) => unit.build(def, pos, facing, opts),
            Order::Repair(t) => unit.repair(t, opts),
            Order::ReclaimFeature(f) => unit.reclaim_feature(f, opts),
            Order::ReclaimUnit(t) => unit.reclaim_unit(t, opts),
        };
    }

    /// `approach` (see [`APPROACH_RESEND`]): whether worker `wid`, out of
    /// build range of its own task `task_id` at `task_pos`, is left to the
    /// order it has, or has stalled and just dropped the task.
    fn eco_approach_holds(&mut self, wid: i32, task_id: TaskId, task_pos: Vec3) -> bool {
        let frame = self.frame;
        let Some(w) = self.eco.workers.get_mut(&wid) else {
            return false;
        };
        let Some(mut a) = w.approach.filter(|a| a.task == task_id) else {
            return false;
        };
        let pos = w.get_pos();
        let dist = distance(pos, task_pos);
        // A retreat since the order (the worker ran from danger) restarts
        // the clock, as headway does.
        if dist < a.best - APPROACH_GAIN || distance(pos, a.anchor) >= APPROACH_TRAVEL || w.last_retreat_frame >= a.sent
        {
            a.best = a.best.min(dist);
            a.gained = frame;
            a.anchor = pos;
        }
        w.approach = Some(a);
        if frame - a.gained >= APPROACH_STALL {
            w.shunned.retain(|&(_, until)| until > frame);
            w.shunned.push((task_pos, frame + APPROACH_SHUN));
            w.approach = None;
            w.clear_task(&mut self.eco.tasks);
            let name = self.defs.name_of(w.unit);
            self.log(&format!(
                "economy: approach: {} #{} got no nearer than {:.0} to its job at ({:.0}, {:.0}) in {} s (at ({:.0}, \
                 {:.0})): dropped, site shunned",
                name,
                wid,
                a.best,
                task_pos.x,
                task_pos.z,
                (frame - a.gained) / 30,
                pos.x,
                pos.z
            ));
            return true;
        }
        // Slowed, and still on the order sent (not retreated since, its queue
        // ending in a command of that order's kind: a build of the same
        // unit, a repair, a reclaim; the target is not compared): leave it
        // be a while, for ZK's stuck handling to step it aside.
        let queued = w.unit.current_commands().last().map(|c| c.id());
        frame - a.gained >= APPROACH_SLOW
            && queued == Some(a.cmd)
            && w.last_retreat_frame < a.sent
            && frame - a.sent < APPROACH_RESEND
    }

    /// Whether worker `w` shuns a site at `p` (see [`APPROACH_SHUN`]).
    fn eco_worker_shuns(&self, w: &Worker, p: Vec3) -> bool {
        let frame = self.frame;
        w.shunned.iter().any(|&(s, until)| until > frame && distance(s, p) < APPROACH_SHUN_RADIUS)
    }

    /// Whether a worker on the job of repairing `unit` is about at it (in
    /// build range, give or take a step).
    pub(crate) fn eco_repair_crew_near(&self, unit: springai::Unit) -> bool {
        let eco = &self.eco;
        let p = unit.pos();
        eco.repair_tasks
            .iter()
            .filter_map(|&id| eco.tasks.get(id))
            .filter(|t| matches!(&t.kind, TaskKind::Repair(r) if r.target == unit))
            .flat_map(|t| t.assigned_workers.iter().filter_map(|id| eco.workers.get(id)))
            .any(|w| distance(w.get_pos(), p) <= w.unit.def().map_or(0.0, |d| d.build_distance()) + 100.0)
    }

    /// Shared admission rule for the project planner and the executor.
    pub(super) fn eco_worker_can_take(&self, w: &Worker, t: &WorkerTask) -> bool {
        let def = w.unit.def();
        let range = def.map_or(0.0, |d| d.build_distance()) + 32.0;
        let jumps_cliffs = w.is_com
            && !self.dev_off("comreach")
            && def.is_some_and(|d| crate::military::jumps::JumpDef::of(d).is_some());
        let spider_reach = jumps_cliffs
            && self
                .reach_terrain()
                .and_then(|t| t.tier(Tier::Spider))
                .is_some_and(|m| m.reaches_within(terrain_pos(w.get_pos()), terrain_pos(t.get_pos()), range, SNAP));
        !(t.as_construction().is_some_and(|c| c.fortification) && self.eco_fortify_travel(w, t.get_pos()).is_none())
            && self.eco.silo_work_allowed(t)
            && self.eco_prerequisites_ready(t)
            && def.is_none_or(|d| self.terrain_reaches(d, w.get_pos(), t.get_pos(), range) || spider_reach)
            && !self.eco_worker_shuns(w, t.get_pos())
            && !(w.is_com && self.comguard_avoids(w.id, t.get_pos()))
    }

    fn eco_finishing_task(&self, w: &Worker) -> Option<TaskId> {
        if let Some(task) = self.eco_build_guard_task(w.id) {
            return Some(task);
        }
        let t = self.eco.tasks.get(w.task?)?;
        let c = t.as_construction()?;
        let u = c.target?;
        let built = u.build_progress();
        let range = w.unit.def()?.build_distance();
        if !u.is_being_built()
            || u.health() <= 0.0
            || (u.team() != self.team_id && !self.allies.contains(&u.team()))
            || !(FINISH_PROGRESS..1.0).contains(&built)
            || w.bp <= 0.0
            || w.is_chicken
            || self.eco.hard_estalled
            || distance(w.get_pos(), u.pos()) > range
            || !self.eco_worker_can_take(w, t)
            || !self.logistics_allows(w.id, t.id)
        {
            return None;
        }
        let spending = w.unit.resource_use(self.metal);
        if spending <= 0.0 {
            return None;
        }
        let left = 1.0 - built;
        let seconds = (c.build_type.build_time() * left / w.bp).max(c.build_type.cost(self.metal) * left / spending);
        let view = self.war.view(w.unit.def());
        let p = u.pos();
        let safe = view.get_effective_threat(p) <= view.get_total_friendly_threat(p)
            && (self.eco_raiders_near(p) <= RAID_HOLD * self.war.get_total_friendly_power(p)
                || self.conquest_ground_safe(p));
        (seconds <= FINISH_SECONDS && safe).then_some(t.id)
    }

    /// A project may lease only a usable, safe worker, never a commander
    /// already carrying an explicit mission or a worker held by another controller.
    pub(crate) fn eco_project_hand(&self, wid: i32, task: TaskId) -> bool {
        let Some(w) = self.eco.workers.get(&wid) else { return false };
        let Some(t) = self.eco.tasks.get(task) else { return false };
        // The finishing check validates its current crew through this method.
        // Only ask it when proposing a different job, avoiding that cycle.
        let finishing =
            w.task != Some(task) && !self.op_priority(task) && self.eco_finishing_task(w).is_some_and(|id| id != task);
        let dedicated = finishing
            || w.task.is_some_and(|id| {
                id != task
                    && (self.op_priority(id)
                        || self.conquest_bonus(wid, id).is_some()
                        || self.eco.tasks.get(id).and_then(|t| t.as_construction()).is_some_and(|c| c.fac_plop))
            });
        let pos = t.get_pos();
        let raiders = self.eco_raiders_near(pos);
        let recent_danger = raiders > 0.0
            && raiders > RAID_HOLD * self.war.get_total_friendly_power(pos)
            && !self.conquest_ground_safe(pos);
        let view = self.war.view(w.unit.def());
        let predicted = self.eco_build_guard_task(wid) == Some(task);
        // Planning must not commit a worker to a site the executor flees.
        // A construction rollout can answer the same question more precisely.
        let safe = predicted
            || !recent_danger
                && view.get_effective_threat(pos) / self.op_greed() <= view.get_total_friendly_threat(pos);
        !dedicated
            && !w.is_chicken
            && w.raid_defense.is_none()
            && w.unit.health() > 0.0
            && w.unit.team() == self.team_id
            && !springai::reserved::is_reserved(self.cb, wid)
            && !self.eco_worker_held(wid)
            && !self.jump_in_progress(w.unit)
            && !super::terraform::is_terraforming(w.unit)
            && self.eco_worker_can_take(w, t)
            && safe
    }

    fn eco_get_cheapest_job(&self, wid: i32) -> Option<TaskId> {
        let eco = &self.eco;
        let worker = eco.workers.get(&wid)?;
        let wpos = worker.get_pos();
        let view = self.war.view(worker.unit.def());
        // Raiders seen lately near a job count as well as those seen there
        // now: jumpjets come out of the fog in seconds (see RAID_MEMORY).
        let raiders = self.eco_recent_raiders();
        let currency = !self.dev_off("currency");
        let greed = self.op_greed();
        let threat_cost = |t: &WorkerTask| {
            let p = t.get_pos();
            let near: f32 = raiders.iter().filter(|r| distance(r.0, p) <= RAID_RADIUS).map(|r| r.1).sum();
            // On the crew of an issue, the danger is in the bonus's
            // currency: the expected loss of the worker (see `logistics`).
            if currency && self.logistics_bonus(wid, t.id).is_some() {
                return self.logistics_risk(worker, p, near);
            }
            let mut cost = 500.0 * view.get_effective_threat(p).max(0.0);
            // Not on conquered ground with no enemy near now (see `conquest`).
            if near > 0.0 && near > RAID_HOLD * self.war.get_total_friendly_power(p) && !self.conquest_ground_safe(p) {
                cost += RAID_COST + near;
            }
            // The operator's greed dial: far, dangerous jobs priced cheaper.
            cost / greed
        };
        // Only jobs the worker can get to (bots cannot build out at sea).
        let def = worker.unit.def();
        let range = def.map_or(0.0, |d| d.build_distance()) + 32.0;
        let reaches = |t: &WorkerTask| {
            self.eco_worker_can_take(worker, t) && (self.op_priority(t.id) || self.logistics_allows(wid, t.id))
        };
        // Metal only this type of constructor gets to goes first.
        let others = match def {
            Some(d) if !worker.is_com => self.eco_other_builders(d),
            _ => Vec::new(),
        };
        // So does metal of a colony it is the colonist of.
        let stranded = |t: &WorkerTask| {
            let mex = t.as_construction().is_some_and(|c| c.build_type.id() == eco.build_ids.mex_id);
            let bonus = if mex && self.eco_out_of_reach(&others, t.get_pos()) { STRANDED_BONUS } else { 0.0 };
            bonus + self.eco_colony_bonus(wid, t.get_pos(), mex)
        };

        let mut task: Option<TaskId> = None;
        let mut cost = NEVER;

        if let Some(t) = worker.task.and_then(|id| eco.tasks.get(id)).filter(|&t| reaches(t)) {
            task = Some(t.id);
            cost = self.eco_cost_of_job(worker, wpos, t) - stranded(t);
            cost += threat_cost(t);
        }

        let mut consider = |ids: &[TaskId], with_threat: bool, skip: &dyn Fn(&WorkerTask) -> bool| {
            for t in ids.iter().filter_map(|&id| eco.tasks.get(id)) {
                if skip(t) || !reaches(t) {
                    continue;
                }
                let mut tmpcost = self.eco_cost_of_job(worker, wpos, t) - stranded(t);
                if with_threat {
                    tmpcost += threat_cost(t);
                }
                if tmpcost < cost {
                    cost = tmpcost;
                    task = Some(t.id);
                }
            }
        };
        let none = |_: &WorkerTask| false;

        consider(&eco.construction_tasks, true, &none);

        if eco.max_storage > 1.0
            && (eco.metal / eco.max_storage < 0.9 || !eco.hard_estalled)
            && (!eco.energy_reclaim || eco.energy_reclaim_tasks.is_empty())
        {
            consider(&eco.reclaim_tasks, true, &none);
        }

        if eco.energy_reclaim {
            consider(&eco.energy_reclaim_tasks, true, &none);
        }

        consider(&eco.combat_reclaim_tasks, false, &none);

        // don't assign workers to repair themselves
        let own = |t: &WorkerTask| matches!(&t.kind, TaskKind::Repair(r) if r.target.id() == wid);
        consider(&eco.repair_tasks, true, &own);

        // A new project must not leave an ordinary turret at 99% while a
        // replacement builder walks across the map. Explicit missions still
        // win; actual spending and remaining work bound this commitment.
        let preempt = task.is_some_and(|id| {
            self.op_priority(id)
                || self.conquest_bonus(wid, id).is_some()
                || eco.tasks.get(id).and_then(|t| t.as_construction()).is_some_and(|c| c.fac_plop)
        });
        if !preempt && let Some(finishing) = self.eco_finishing_task(worker) {
            return Some(finishing);
        }

        // A free mex near the worker before a new factory, or one it stands
        // at before the missile silo (`remexfirst`).
        if let Some((fac, radius)) =
            task.and_then(|id| eco.tasks.get(id)).and_then(|t| Some((t, self.eco_remex_radius(t, range)?)))
            && !self.dev_off("remexfirst")
        {
            let mex = eco.build_ids.mex_id;
            let best = (eco.construction_tasks.iter().filter_map(|&id| eco.tasks.get(id)))
                .filter(|t| t.as_construction().is_some_and(|c| c.build_type.id() == mex))
                .filter(|t| distance(wpos, t.get_pos()) <= radius && reaches(t))
                .map(|t| {
                    let keep = if worker.task == Some(t.id) { REMEX_KEEP } else { 0.0 };
                    (t, self.eco_cost_of_job(worker, wpos, t) - stranded(t) + threat_cost(t) - keep)
                })
                .min_by(|a, b| a.1.total_cmp(&b.1))
                .filter(|&(_, c)| c < REMEX_CEILING);
            if let Some((m, c)) = best {
                if worker.task != Some(m.id) {
                    let (p, f) = (m.get_pos(), fac.get_pos());
                    let name = fac.as_construction().map_or("", |c| self.defs.name(c.build_type));
                    self.log(&format!(
                        "economy: remexfirst: worker {wid} takes the mex at ({:.0}, {:.0}) (cost {c:.0}) before the {name} at ({:.0}, {:.0})",
                        p.x, p.z, f.x, f.z
                    ));
                }
                return Some(m.id);
            }
        }

        // Explicit missions and the energy emergency still preempt a lease.
        // Otherwise execute the project crew we actually forecast, including
        // its travel, instead of silently bidding the same worker elsewhere.
        if !preempt
            && let Some(project) = self.logistics_assignment(wid)
            && self.eco_project_hand(wid, project)
        {
            return Some(project);
        }
        task
    }

    /// How far a free mex may be from a worker whose cheapest job is `t`
    /// to go first (`remexfirst`), the worker's reach being `range`:
    /// [`REMEX_RADIUS`] for a new factory, strider hub or plate (not a
    /// commander's plop, nor a factory while we have none); a step past its
    /// reach for the missile silo (see [`REMEX_STEP`]); none for anything
    /// else, or a job the operator asked for.
    fn eco_remex_radius(&self, t: &WorkerTask, range: f32) -> Option<f32> {
        let c = t.as_construction()?;
        if self.op_priority(t.id) {
            return None;
        }
        let id = c.build_type.id();
        if id == self.eco.build_ids.missile_silo_id {
            return Some(range + REMEX_STEP);
        }
        let plate = self.defs.name(c.build_type).starts_with("plate");
        // (`airstall`: metal piles up while air stalls, so its plate waits for
        // no remex; on 2026-11-04 four constructors took a remex first 19
        // times over 8 minutes, mostly the same mex, and the plate never rose.)
        if plate && self.air_stalled() {
            return None;
        }
        let factory = self.eco.build_ids.fac_ids.contains(&id) || plate;
        (factory && !c.fac_plop && !self.fac.factories.is_empty()).then_some(REMEX_RADIUS)
    }

    /// The types of our constructors other than `def` (commanders aside:
    /// they have better things to do), once each.
    fn eco_other_builders(&self, def: UnitDef) -> Vec<UnitDef> {
        let mut defs: Vec<UnitDef> = Vec::new();
        for d in self.eco.workers.values().filter(|w| !w.is_com).filter_map(|w| w.unit.def()) {
            if d.id() != def.id() && !defs.iter().any(|o| o.id() == d.id()) {
                defs.push(d);
            }
        }
        defs
    }

    /// Whether some types of constructor, `others`, and none of them get to
    /// build at `p` from our base.
    fn eco_out_of_reach(&self, others: &[UnitDef], p: Vec3) -> bool {
        let from = self.graph.get_ally_center();
        !others.is_empty() && others.iter().all(|&d| !self.terrain_reaches(d, from, p, d.build_distance() + 32.0))
    }

    /// The free metal spots no enemy defenses cover that constructors of
    /// type `def` get to from `from` and none of our other ones do.
    pub(super) fn eco_stranded_spots(&self, def: UnitDef, from: Vec3) -> usize {
        let others = self.eco_other_builders(def);
        let range = def.build_distance() + 32.0;
        (self.graph.get_neutral_spots().iter().map(|s| s.get_pos()))
            .filter(|&p| self.war.get_porc_threat(p) == 0.0 && self.terrain_reaches(def, from, p, range))
            .filter(|&p| self.eco_out_of_reach(&others, p))
            .count()
    }

    /// Whether factory-built constructors of type `def` (from `from`) are
    /// wanted for metal only they get to: how many spots, if so.
    pub(super) fn eco_wants_stranded_con(&self, def: UnitDef, from: Vec3) -> Option<usize> {
        let have = self.eco.workers.values().filter(|w| w.unit.def().is_some_and(|d| d.id() == def.id())).count();
        if have >= STRANDED_CONS {
            return None;
        }
        let spots = self.eco_stranded_spots(def, from);
        (have < spots.div_ceil(STRANDED_PER_CON)).then_some(spots)
    }

    /// What the free metal near a spot at `p` makes taking it worth (see
    /// [`CLUSTER_BONUS`]); spots enemy defenses cover are not free.
    fn eco_mex_cluster_bonus(&self, p: Vec3) -> f32 {
        let free = (self.graph.get_neutral_spots().into_iter().map(|s| s.get_pos()))
            .filter(|&q| self.war.get_porc_threat(q) == 0.0);
        cluster_bonus(p, free)
    }

    /// What worker `worker` at `wpos` doing `task` costs (lower is better),
    /// less PAIR_BONUS for a Constable joining its partner (see below).
    /// The danger of the enemy armed ground units seen lately within
    /// RAID_RADIUS of `p` (see below).
    pub(crate) fn eco_raiders_near(&self, p: Vec3) -> f32 {
        self.eco_recent_raiders().iter().filter(|r| distance(r.0, p) <= RAID_RADIUS).map(|r| r.1).sum()
    }

    /// Enemy armed ground units seen within RAID_MEMORY frames: where last
    /// seen and their danger (empty with `disable=raidmem`).
    pub(super) fn eco_recent_raiders(&self) -> Vec<(Vec3, f32)> {
        if self.dev_off("raidmem") {
            return Vec::new();
        }
        let frame = self.frame;
        (self.war.targets.values())
            .filter(|e| !e.is_dead && !e.is_static && !e.can_fly() && e.can_attack() && !e.is_worker)
            .filter(|e| frame - e.last_seen <= RAID_MEMORY && e.position != NULLPOS)
            .map(|e| (e.position, e.get_danger()))
            .collect()
    }

    fn eco_cost_of_job(&self, worker: &Worker, wpos: Vec3, task: &WorkerTask) -> f32 {
        let cost = self.eco_cost_of_job_alone(worker, wpos, task);
        if cost >= NEVER {
            return cost;
        }
        // A commander holding conquered ground stays near it (`conquestleash`).
        let progress =
            task.as_construction().and_then(|c| c.target).map_or(0.0, |u| u.build_progress().clamp(0.0, 1.0));
        let cost = cost + self.conquest_leash(worker.id, task.get_pos()) - 1000.0 * progress * progress;
        if self.dev_off("pairs") || self.defs.name_of(worker.unit) != PAIRED_CON {
            return cost;
        }
        let partner = (task.assigned_workers.iter())
            .filter(|&&id| id != worker.id)
            .filter_map(|id| self.eco.workers.get(id))
            .any(|w| !w.is_com && distance(w.get_pos(), wpos) <= PAIR_RANGE && self.defs.name_of(w.unit) == PAIRED_CON);
        if partner { cost - PAIR_BONUS } else { cost }
    }

    /// What worker `w`'s present job costs it on the ladder (lower is
    /// better; `NEVER` without one), as `eco_assign_worker` weighs keeping
    /// it: for jobs handed out otherwise (a ditch, see `terraform`).
    pub(super) fn eco_current_job_cost(&self, w: &Worker) -> f32 {
        let Some(t) = w.task.and_then(|id| self.eco.tasks.get(id)) else { return NEVER };
        let mex = t.as_construction().is_some_and(|c| c.build_type.id() == self.eco.build_ids.mex_id);
        self.eco_cost_of_job(w, w.get_pos(), t) - self.eco_colony_bonus(w.id, t.get_pos(), mex)
    }

    fn eco_cost_of_job_alone(&self, worker: &Worker, wpos: Vec3, task: &WorkerTask) -> f32 {
        let eco = &self.eco;
        let mut cost_mod = 1.0f32;
        let task_pos = task.get_pos();
        let dist = distance(wpos, task_pos);

        // Constables (5 build power for 135 metal, 550 health) go in pairs
        // on one build, as good players pair them: the first other Constable
        // on the job, if near, is a partner, not a crowd.
        let pairs = !self.dev_off("pairs") && self.defs.name_of(worker.unit) == PAIRED_CON;
        let mut partner = false;
        for w in task.assigned_workers.iter().filter_map(|id| eco.workers.get(id)) {
            // increment cost mod for every other worker unassigned to the given task that isn't the worker we're assigning
            // as long as they're closer or equaldist to the target.
            let idist = distance(w.get_pos(), task_pos);
            let rdist = idist.max(200.0);
            let deltadist = (idist - dist).abs();
            if w.id != worker.id {
                if pairs
                    && !partner
                    && !w.is_com
                    && distance(w.get_pos(), wpos) <= PAIR_RANGE
                    && self.defs.name_of(w.unit) == PAIRED_CON
                {
                    partner = true;
                } else if w.is_com {
                    cost_mod += 2.0;
                } else if rdist < dist || deltadist < 100.0 {
                    cost_mod += 1.0;
                }
            }
        }

        match &task.kind {
            TaskKind::Construction(ctask) => {
                if ctask.fac_plop {
                    // only assign facplop tasks to the commander who has the plop!
                    return if worker.id != ctask.assigned_plop { NEVER } else { -1000.0 };
                }

                // Asked for by the operator: before anything else, spread
                // over workers like other jobs.
                if self.op_priority(task.id) {
                    return (dist / 4.0) - 3000.0 + (400.0 * (cost_mod - 1.0)).max(0.0);
                }

                // A conquest objective's job: its commander's first, and
                // its turrets' for builders near them; energy at home for
                // the energy worker while the commander is away (see
                // `conquest`).
                if let Some(bonus) = self.conquest_bonus(worker.id, task.id) {
                    return (dist / 4.0) - bonus + (400.0 * (cost_mod - 1.0)).max(0.0);
                }

                // On the crew of a tracked issue: its share of the value
                // (see `logistics`).
                if let Some(share) = self.logistics_bonus(worker.id, task.id) {
                    return (dist / 4.0) - share;
                }

                let ids = &eco.build_ids;
                let def_id = ctask.build_type.id();
                if def_id == ids.solar_id || def_id == ids.wind_id {
                    // (Greedy workers build it while metal floats for want
                    // of it: `energycrew`.)
                    let greedy = worker.is_greedy && !self.eco_floating_on_energy();
                    // With `compush` the commander leaves the energy behind
                    // it to the energy worker.
                    let back = self.compush_back_cost(worker, wpos, task_pos);
                    // With `energyneed` the pull follows the shortfall
                    // against construction, a walk costing as for a mex.
                    if !self.dev_off("energyneed") {
                        if greedy {
                            return dist + (10000.0 * cost_mod); // greedy workers shouldn't build energy at all.
                        }
                        let crowd = (1000.0 * (cost_mod - if worker.is_com { 2.0 } else { 1.0 })).max(0.0);
                        return (dist / 4.0) - NEED_PULL * self.eco_energy_shortfall() + crowd + back;
                    }
                    // for small energy
                    if eco.estalled || eco.effective_income_metal > eco.effective_income_energy - 2.5 {
                        if greedy {
                            return dist + (10000.0 * cost_mod); // greedy workers shouldn't build energy at all.
                        }
                        if worker.is_com {
                            if eco.hard_estalled {
                                return ((dist / 2.0) - 300.0) + (1000.0 * (cost_mod - 2.0)).max(0.0) + back;
                            }
                            return ((dist / 3.0) - 200.0) + (1000.0 * (cost_mod - 2.0)).max(0.0) + back;
                        }
                        return ((dist / 3.0) - 200.0) + (1000.0 * (cost_mod - 1.0)).max(0.0);
                    } else {
                        if greedy {
                            return dist + (10000.0 * cost_mod); // greedy workers shouldn't build energy at all.
                        }
                        if worker.is_com {
                            return (dist / 4.0) + (1000.0 * (cost_mod - 2.0)).max(0.0) + back;
                        }
                        return (dist / 4.0) + (1000.0 * (cost_mod - 1.0)).max(0.0);
                    }
                }

                if def_id == ids.mex_id {
                    // for mexes; conquered ground's first (see `conquest`)
                    // and with `compush` the commander's toward the enemy.
                    let cluster = self.eco_mex_cluster_bonus(task_pos)
                        + self.conquest_ground_bonus(task_pos)
                        + self.invade_spot_bonus(task_pos)
                        + self.compush_face_bonus(worker, wpos, task_pos)
                        + self.conquest_route_pull(worker.id, task.id, dist);
                    if worker.is_greedy {
                        return ((dist / 4.0) - 100.0 - cluster) + (600.0 * (cost_mod - 1.0)).max(0.0);
                    }
                    return ((dist / 4.0) - 100.0 - cluster) + (600.0 * (cost_mod - 2.0)).max(0.0);
                }

                let cost = ctask.build_type.cost(self.metal);
                if worker.is_greedy
                    && !self.fac.factories.is_empty()
                    && ((ctask.fac_def && eco.effective_income < 15.0) || (cost > 300.0 && def_id != ids.airpad_id))
                {
                    return dist + (10000.0 * cost_mod);
                }

                if def_id == ids.missile_silo_id {
                    return (dist / 4.0) - SQUAD_PULL + (400.0 * (cost_mod - SQUAD)).max(0.0);
                }

                if ids.fac_ids.contains(&def_id) {
                    // emergency facs get maximum priority
                    if self.fac.factories.is_empty() {
                        return -1000.0;
                    }
                    return ((dist / 2.0) - cost) + (500.0 * (cost_mod - 1.0));
                }

                if ctask.fac_def
                    && worker.is_com
                    && self.eco_get_nearest_fac(task_pos).is_some_and(|f| f.unit.health() < f.unit.max_health())
                {
                    return -1000.0;
                }

                // A chokepoint's turret, with `detour`, is priced as light
                // porc: its metal cost is no pull of its own (a Stinger
                // outbid a mex at the same distance out to 1400 elmos).
                let gate_turret = eco.fortify_tasks.contains(&task.id) && !self.dev_off("detour");
                if ids.expensive_ids.contains(&def_id) && !gate_turret {
                    // for expensive porc.
                    if eco.hard_estalled {
                        return dist + (250.0 * cost_mod);
                    }
                    if cost_mod < 4.0 {
                        return (dist / 2.0) - cost.min(3500.0);
                    }
                    return ((dist / 2.0) - cost.min(3500.0)) + (500.0 * cost_mod);
                }

                if ctask.build_type.is_able_to_attack() {
                    // for cheap porc
                    if !eco.defended_fac || self.war.slasher_spam as f32 * 140.0 > self.war.enemy_porc_value {
                        return dist + 300.0;
                    }
                    if worker.is_com && !eco.estalled {
                        return ((dist / 4.0) - 100.0) + (600.0 * (cost_mod - 2.0)).max(0.0);
                    }
                    if worker.is_greedy {
                        return ((dist / 3.0) - 200.0) + (600.0 * (cost_mod - 1.0)).max(0.0);
                    }
                    return ((dist / 2.0) - 200.0) + (1500.0 * (cost_mod - 2.0)).max(0.0);
                }

                if def_id == ids.airpad_id {
                    // for airpads
                    return (dist / 3.0) - 350.0 + (500.0 * (cost_mod - 2.0)).max(0.0);
                }

                if def_id == ids.nano_id || def_id == ids.storage_id {
                    // for nanotowers and storages
                    if worker.is_com || (eco.hard_estalled && !worker.is_greedy) {
                        return dist;
                    }
                    return dist - 1000.0 + (500.0 * (cost_mod - 2.0));
                }

                if def_id == ids.pylon_id {
                    // for pylons: first of all while a superweapon waits
                    // for its site to have power (see `grid`)
                    let gridmex = !self.dev_off("gridmex");
                    let nfus = eco.fusions.len();
                    // With `gridmex`, pylons count once they stand: frames
                    // counted as pylons dropped their own job down the
                    // ladder, and one sat at 1% for 16 minutes (Folsom,
                    // 2026-10-27).
                    let npyl = if gridmex {
                        eco.pylons.iter().filter(|u| !u.is_being_built()).count()
                    } else {
                        eco.pylons.len()
                    };
                    if eco.pending_power.is_some() {
                        return (dist / 4.0) - 3000.0 + (500.0 * (cost_mod - 1.0));
                    }
                    // A frame already up is finished first.
                    if gridmex && ctask.target.is_some() {
                        return (dist / 4.0) - 1000.0 + (500.0 * (cost_mod - 1.0));
                    }
                    if worker.is_greedy {
                        if nfus > npyl {
                            return (dist / 4.0) - 3000.0 + (500.0 * (cost_mod - 1.0));
                        }
                        // The grid planner queues only pylons that pay
                        // (`gridmex`): linking a mex is a mex's worth.
                        if gridmex {
                            return (dist / 4.0) - 100.0 + (600.0 * (cost_mod - 1.0)).max(0.0);
                        }
                        return dist + (10000.0 * cost_mod);
                    }
                    if nfus > npyl {
                        return (dist / 4.0) - 1000.0 + (500.0 * (cost_mod - 1.0));
                    }
                    if nfus > 3 {
                        return (dist / 4.0) - 600.0 + (500.0 * (cost_mod - 1.0));
                    }
                    if nfus > 2 {
                        return (dist / 4.0) - 300.0 + (500.0 * (cost_mod - 1.0));
                    }
                    return (dist / 4.0) - 100.0 + (500.0 * (cost_mod - 1.0));
                }

                if def_id == ids.radar_id {
                    // for radar
                    if eco.hard_estalled && !worker.is_greedy {
                        return dist + 300.0;
                    }
                    return (dist / 4.0) - 100.0 + (1500.0 * (cost_mod - 1.0));
                }

                if cost > 300.0 {
                    // allow at least 3 builders for each expensive task, more if the cost is high.
                    return ((dist / 2.0) - cost.min(3000.0)) + (500.0 * (cost_mod - 3.0)).max(0.0);
                }

                (dist - 250.0) + (100.0 * (cost_mod - 1.0))
            }

            TaskKind::Reclaim(rtask) => {
                if rtask.target.def().is_none() {
                    return NEVER;
                }

                if self.war.get_porc_threat(task_pos) > 0.0 {
                    // don't reclaim under enemy porc
                    return (dist * dist) + (1000.0 * cost_mod);
                }

                // A cleared invasion target's wrecks first (see `invade`).
                let mut rec_mod = 100.0 + self.invade_reclaim_bonus(task_pos);
                if self.graph.is_ally_territory(task_pos) {
                    let tf = self.graph.territory_fraction;
                    rec_mod += 200.0 * tf * tf;
                }

                if rtask.metal_value > 50.0 {
                    return ((dist / 4.0) - rec_mod) + (250.0 * (cost_mod - 3.0)).max(0.0);
                }
                ((dist / 4.0) - rec_mod) + (700.0 * (cost_mod - 1.0))
            }

            // favor combat reclaim when it's possible
            TaskKind::CombatReclaim(_) => dist - 600.0 + (600.0 * (cost_mod - 2.0)).max(0.0),

            TaskKind::Repair(rptask) => {
                let target = rptask.target;
                let health = target.health();
                let Some(td) = target.def().filter(|_| health > 0.0) else {
                    return NEVER;
                };
                let max_health = target.max_health();
                if !worker.is_greedy && td.speed() > 0.0 && td.is_able_to_attack() && !td.is_builder() {
                    // for mobile combat units
                    if self.war.get_porc_threat(task_pos) > 0.0 {
                        return NEVER;
                    }

                    if eco.hard_estalled {
                        return dist;
                    }

                    let worth = if self.dev_off("lightrepair") {
                        1.0
                    } else {
                        repair_worth(td.cost(self.metal), health, max_health)
                    };

                    if rptask.is_shield_mob && worker.has_shields {
                        return dist + (100.0 * (cost_mod - 1.0)) - worth * (250.0 + max_health / (5.0 * cost_mod));
                    }

                    if !self.graph.is_ally_territory(task_pos) {
                        return dist + 500.0;
                    }

                    dist + (100.0 * (cost_mod - 1.0)) - worth * (250.0 + (max_health + health) / (10.0 * cost_mod))
                } else if !worker.is_greedy
                    && td.is_able_to_attack()
                    && !td.is_builder()
                    && self.defs.name(td) != "turretaalaser"
                {
                    // for static defenses
                    dist + (100.0 * (cost_mod - 1.0)) - (max_health * 2.0) / cost_mod
                } else {
                    // for everything else
                    dist - 250.0 + (600.0 * (cost_mod - 1.0))
                }
            }
        }
    }

    /// Whether a building of type `def` can be queued at `pos`: not
    /// underwater where that is bad, not overlapping queued jobs or
    /// factories, nor (but mexes) their exit lanes.
    pub(crate) fn eco_build_check(&self, def: UnitDef, pos: Vec3, facing: i32) -> bool {
        // stop things from being built underwater if the map water does damage or if water pathing is bad on the map
        if (self.eco.water_damage || !self.graph.is_water_map) && self.cb.map().elevation_at(pos.x, pos.z) < 10.0 {
            return false;
        }

        // get a building's footprint half-extents based on facing
        let extents = |def: UnitDef, facing: i32, pad: f32| {
            let (x, z) = ((def.x_size() as f32 + pad) * 4.0, (def.z_size() as f32 + pad) * 4.0);
            if facing == 0 || facing == 2 { (x, z) } else { (z, x) }
        };
        let (xsize, zsize) = extents(def, facing, 0.0);
        let overlaps = |cpos: Vec3, cxsize: f32, czsize: f32| {
            // too close in the x dimension, and too close in the z dimension
            (cpos.x - pos.x).abs() < xsize + cxsize && (cpos.z - pos.z).abs() < zsize + czsize
        };

        //check for overlap with existing queued jobs
        for t in self.eco.construction_tasks.iter().filter_map(|&id| self.eco.tasks.get(id)) {
            if let Some(c) = t.as_construction() {
                let pad = if keeps_exit(self.defs.name(c.build_type)) || keeps_exit(self.defs.name(def)) {
                    FACTORY_CLEARANCE / 4.0
                } else {
                    0.0
                };
                let (cxsize, czsize) = extents(c.build_type, c.facing, pad);
                if overlaps(t.position, cxsize, czsize) {
                    return false;
                }
            }
        }

        for f in self.fac.factories.values() {
            // check fac overlap
            let fac = f.unit;
            let Some(fdef) = fac.def() else {
                continue;
            };
            let (cxsize, czsize) = extents(fdef, fac.building_facing(), FACTORY_CLEARANCE / 4.0);
            if overlaps(fac.pos(), cxsize, czsize) {
                return false;
            }
        }

        // New factories need the same clearance from existing static units.
        if keeps_exit(self.defs.name(def)) {
            let reach = xsize.max(zsize) + FACTORY_CLEARANCE + 400.0;
            for u in self.cb.friendly_units_in_cylinder(pos, reach) {
                if let Some(d) = u.def().filter(|d| d.speed() <= 0.0) {
                    let (hx, hz) = extents(d, u.building_facing(), FACTORY_CLEARANCE / 4.0);
                    if overlaps(u.pos(), hx, hz) {
                        return false;
                    }
                }
            }
        }

        // nor in front of a factory's exit, built or queued (mexes go where the metal is)
        if self.defs.name(def) != "staticmex" {
            let in_lane = |fdef: UnitDef, fpos: Vec3, ffacing: i32| {
                let lane = exit_lane(fdef, fpos, ffacing);
                keeps_exit(self.defs.name(fdef))
                    && pos.x + xsize > lane[0]
                    && pos.x - xsize < lane[2]
                    && pos.z + zsize > lane[1]
                    && pos.z - zsize < lane[3]
            };
            let built = (self.fac.factories.values())
                .any(|f| f.unit.def().is_some_and(|d| in_lane(d, f.unit.pos(), f.unit.building_facing())));
            let queued = (self.eco.factory_tasks.iter().filter_map(|&id| self.eco.tasks.get(id)))
                .any(|t| t.as_construction().is_some_and(|c| in_lane(c.build_type, t.position, c.facing)));
            if built || queued {
                return false;
            }
        }
        // No ground turret among too many of its kind, or in a death blast
        // (`porcmix`).
        self.eco_mix_allows(def, pos)
    }

    /// Remove finished or invalid tasks, sending their workers to be reassigned.
    pub(super) fn eco_clean_orders(&mut self) {
        let frame = self.frame;
        //remove invalid jobs from the queue
        let mut invalid: Vec<TaskId> = Vec::new();
        let mut adopted = Vec::new();
        let map = self.cb.map();

        for id in self.eco.construction_tasks.clone() {
            let Some(t) = self.eco.tasks.get(id) else {
                continue;
            };
            let Some(c) = t.as_construction() else {
                continue;
            };
            let (bt, pos, facing) = (c.build_type, t.position, c.facing);
            let mut target = c.target;
            // Do not start an old plan after enemy territory has advanced
            // into missile range. Existing frames retain the threat policy.
            if target.is_none() && self.eco_super_project_exposed(bt, pos) {
                invalid.push(id);
                continue;
            }
            if target.is_none() && !map.is_possible_to_build_at(bt, pos, facing) {
                // Our own nanoframe (update is called before unitCreated) or
                // an ally's of the same type with enough left to build: build
                // it (see `allied`).
                let radius = bt.x_size().min(bt.z_size()) as f32 * 4.0;
                let nano = self.cb.friendly_units_in_cylinder(pos, radius).into_iter().find(|&u| {
                    u.is_being_built()
                        && u.def().is_some_and(|d| d.id() == bt.id())
                        && u.health() > 0.0
                        && (u.team() == self.team_id || self.eco_worth_helping(u))
                });
                match nano {
                    Some(u) => {
                        if u.team() != self.team_id {
                            adopted.push(id);
                        }
                        target = Some(u);
                        if let Some(c) = self.eco.tasks.get_mut(id).and_then(|t| t.as_construction_mut()) {
                            c.target = Some(u);
                        }
                    }
                    // if a construction job is blocked and not by a frame of its type, remove it
                    None => {
                        if !invalid.contains(&id) {
                            invalid.push(id);
                        }
                    }
                }
            }

            // A queued reactor must still have its setback when the front
            // moves. Keep started nanoframes under the existing threat policy.
            if target.is_none()
                && self.eco.fusion_tasks.contains(&id)
                && !self.eco_reactor_site_safe(bt, pos)
                && !invalid.contains(&id)
            {
                invalid.push(id);
            }

            if self.war.get_porc_threat(pos) > 0.0
                && (target.is_none() || !bt.is_able_to_attack())
                && !invalid.contains(&id)
            {
                invalid.push(id);
            }
        }

        for &id in &self.eco.super_wep_tasks {
            if invalid.contains(&id) {
                continue;
            }
            let stale = self
                .eco
                .tasks
                .get(id)
                .and_then(|t| t.as_construction())
                .is_some_and(|c| frame - c.frame_issued > 900 && c.target.is_none());
            if stale {
                invalid.push(id);
            }
        }
        // remove old porc push tasks, so as not to waste metal on unneeded porc.
        for &id in &self.eco.porc_tasks {
            if invalid.contains(&id) {
                continue;
            }
            let stale = self.eco.tasks.get(id).is_some_and(|t| {
                t.assigned_workers.is_empty()
                    && t.as_construction()
                        .is_some_and(|c| c.frame_issued > 0 && frame - c.frame_issued > 600 && c.target.is_none())
            });
            if stale {
                invalid.push(id);
            }
        }

        for id in invalid {
            self.eco.drop_construction_task(id);
        }
        for id in adopted {
            self.logistics_assist(id);
        }

        let invalid: Vec<TaskId> = self
            .eco
            .reclaim_tasks
            .iter()
            .copied()
            .filter(|&id| matches!(self.eco.tasks.get(id).map(|t| &t.kind), Some(TaskKind::Reclaim(r)) if r.target.def().is_none()))
            .collect();
        for id in invalid {
            self.eco_cancel_task(id);
        }

        let invalid: Vec<TaskId> = self
            .eco
            .combat_reclaim_tasks
            .iter()
            .copied()
            .filter(|&id| match self.eco.tasks.get(id) {
                Some(t @ WorkerTask { kind: TaskKind::CombatReclaim(crt), .. }) => {
                    let target = crt.target;
                    (!target.is_being_built() && target.team() != self.team_id)
                        || self.war.get_porc_threat(t.get_pos()) > 0.0
                        || target.health() <= 0.0
                }
                _ => false,
            })
            .collect();
        for id in invalid {
            self.eco_cancel_task(id);
        }

        let invalid: Vec<TaskId> = self
            .eco
            .repair_tasks
            .iter()
            .copied()
            .filter(|&id| match self.eco.tasks.get(id).map(|t| &t.kind) {
                Some(TaskKind::Repair(r)) => {
                    let health = r.target.health();
                    health <= 0.0 || health == r.target.max_health()
                }
                _ => false,
            })
            .collect();
        for id in invalid {
            self.eco_cancel_task(id);
        }

        if self.eco.chose_sup_wep && self.eco.s_wep == 1 && self.war.enemy_has_anti_nuke {
            self.eco.s_wep = 0;
        }
    }

    /// Stop a task, send its workers to be reassigned and delete it.
    fn eco_cancel_task(&mut self, id: TaskId) {
        let idle = self.eco.stop_workers(id);
        self.eco.idle_workers(&idle);
        self.eco.delete_task(id);
    }
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;

    pub(in crate::economy) fn assignment_ai(customize: impl FnOnce(&mut springai::ffi::SSkirmishAICallback)) -> Ai {
        use std::ffi::{c_char, c_void};
        unsafe extern "C" fn scalar(_: i32, _: i32) -> f32 {
            100.0
        }
        unsafe extern "C" fn zero(_: i32, _: i32) -> i32 {
            0
        }
        unsafe extern "C" fn team(_: i32, _: i32) -> i32 {
            2
        }
        unsafe extern "C" fn cost(_: i32, _: i32, _: i32) -> f32 {
            100.0
        }
        unsafe extern "C" fn no(_: i32, _: i32) -> bool {
            false
        }
        unsafe extern "C" fn params(_: i32, _: i32, _: *mut *const c_char, _: *mut *const c_char) -> i32 {
            0
        }
        unsafe extern "C" fn rules(_: i32, _: i32, _: *const c_char, default: f32) -> f32 {
            default
        }
        unsafe extern "C" fn command(_: i32, _: i32, _: i32, _: i32, _: *mut c_void) -> i32 {
            0
        }
        super::super::commander::tests::test_ai_with(|t| {
            t.Unit_getTeam = Some(team);
            t.Unit_getHealth = Some(scalar);
            t.Unit_getWeapons = Some(zero);
            t.Unit_getCurrentCommands = Some(zero);
            t.Unit_getRulesParamFloat = Some(rules);
            t.UnitDef_getWeaponMounts = Some(zero);
            t.UnitDef_getBuildSpeed = Some(scalar);
            t.UnitDef_getBuildTime = Some(scalar);
            t.UnitDef_getCost = Some(cost);
            t.UnitDef_getSpeed = Some(scalar);
            t.UnitDef_getCustomParams = Some(params);
            t.UnitDef_isAbleToAttack = Some(no);
            t.UnitDef_isMoveDataAvailable = Some(no);
            t.UnitDef_isAbleToFly = Some(no);
            t.UnitDef_getMaxWaterDepth = Some(scalar);
            t.Engine_handleCommand = Some(command);
            customize(t);
        })
    }

    #[test]
    fn project_crews_reach_the_scheduler_and_yield_to_new_safety_evidence() {
        use crate::logistics::{Kind, Worth};
        use springai::{Unit, UnitDef};
        let mut ai = assignment_ai(|_| {});
        ai.frame = 17;
        ai.offset = 0;
        ai.eco.metal = 1000.0;
        ai.eco.energy = 1000.0;
        ai.eco.effective_income = 10.0;
        for id in [10, 11] {
            let w = Worker::new(Unit::from_id(ai.cb, id).unwrap(), ai.team_id);
            ai.eco.workers.insert(id, w);
        }
        let mut jobs = Vec::new();
        for x in [1000.0, 1400.0] {
            let task =
                ai.eco.tasks.insert(WorkerTask::construction(UnitDef::from_id(ai.cb, 1).unwrap(), Vec3::xz(x, 0.0), 0));
            ai.eco.construction_tasks.push(task);
            ai.logistics_open(task, Kind::Energy, Worth::Flat(1000.0), "test", 0.0);
            jobs.push(task);
        }
        ai.logistics_update(17);
        assert_eq!(ai.logistics.issues[&jobs[0]].crew, vec![10]);
        assert_eq!(ai.logistics.issues[&jobs[1]].crew, vec![11]);
        ai.eco_assign_worker(10);
        ai.eco_assign_worker(11);
        assert_eq!(ai.eco.workers[&10].task, Some(jobs[0]));
        assert_eq!(ai.eco.workers[&11].task, Some(jobs[1]));
        // A failed approach immediately cancels the lease's permission,
        // without waiting for the next planner tick or finishing a timer.
        ai.eco.workers.get_mut(&10).unwrap().shunned.push((Vec3::xz(1000.0, 0.0), 1000));
        ai.eco_assign_worker(10);
        assert_eq!(ai.eco.workers[&10].task, None);
        assert_eq!(ai.eco.workers[&11].task, Some(jobs[1]));
        springai::reserved::reserve(ai.cb, 11);
        assert!(!ai.eco_project_hand(11, jobs[1]));
        springai::reserved::release(ai.cb, 11);
        ai.logistics_open(jobs[1], Kind::Energy, Worth::Flat(0.0), "obsolete valuation", 0.0);
        ai.frame = 47;
        ai.logistics_update(47);
        assert!(!ai.logistics.issues.contains_key(&jobs[1]));
        assert!(ai.eco.tasks.get(jobs[1]).is_some(), "return the task for fresh valuation, do not strand it");
    }

    #[test]
    fn active_last_seconds_keep_the_builder_from_routine_jobs_and_new_project_crews() {
        use crate::logistics::{Kind, Worth};
        use springai::Unit;
        use std::cell::Cell;
        thread_local! {
            static BUILT: Cell<f32> = const { Cell::new(0.99) };
            static SPENDING: Cell<f32> = const { Cell::new(1.0) };
        }
        unsafe extern "C" fn progress(_: i32, _: i32) -> f32 {
            BUILT.get()
        }
        unsafe extern "C" fn spending(_: i32, _: i32, _: i32) -> f32 {
            SPENDING.get()
        }
        unsafe extern "C" fn building(_: i32, id: i32) -> bool {
            id == 12 && BUILT.get() < 1.0
        }
        let mut ai = assignment_ai(|t| {
            t.Unit_getBuildProgress = Some(progress);
            t.Unit_getResourceUse = Some(spending);
            t.Unit_isBeingBuilt = Some(building);
        });
        let worker = Unit::from_id(ai.cb, 10).unwrap();
        let target = Unit::from_id(ai.cb, 12).unwrap();
        let def = UnitDef::from_id(ai.cb, 1).unwrap();
        let mut t = WorkerTask::construction(def, target.pos(), 0);
        t.as_construction_mut().unwrap().target = Some(target);
        let finish = ai.eco.tasks.insert(t);
        let next = ai.eco.tasks.insert(WorkerTask::construction(def, worker.pos(), 0));
        ai.eco.construction_tasks.extend([finish, next]);
        let mut w = Worker::new(worker, ai.team_id);
        w.set_task(&mut ai.eco.tasks, finish, 0);
        ai.eco.workers.insert(w.id, w);
        ai.logistics_open(next, Kind::Energy, Worth::Flat(5000.0), "new project", 0.0);
        assert_eq!(ai.eco_finishing_task(&ai.eco.workers[&10]), Some(finish));
        assert!(!ai.eco_project_hand(10, next), "do not forecast the finishing builder on another crew");
        assert_eq!(ai.eco_get_cheapest_job(10), Some(finish));
        ai.op.over.priority_tasks.insert(next);
        assert_eq!(ai.eco_get_cheapest_job(10), Some(next), "an explicit priority can preempt");
        assert!(ai.eco_project_hand(10, next));
        ai.op.over.priority_tasks.clear();
        ai.logistics_open(finish, Kind::Energy, Worth::Flat(5000.0), "current project", 0.0);
        ai.frame = 17;
        ai.offset = 0;
        ai.eco.metal = 1000.0;
        ai.eco.energy = 1000.0;
        ai.eco.effective_income = 10.0;
        ai.logistics_update(17);
        assert_eq!(ai.logistics.issues[&finish].crew, vec![10]);
        assert_eq!(ai.eco_finishing_task(&ai.eco.workers[&10]), Some(finish));
        assert!(ai.eco_project_hand(10, finish), "a finishing builder remains eligible for its own crew");
        assert!(!ai.eco_project_hand(10, next), "current crew validation must not recurse");
        SPENDING.set(0.0);
        assert_eq!(ai.eco_finishing_task(&ai.eco.workers[&10]), None, "a stalled build does not pin a worker");
        SPENDING.set(0.01);
        assert_eq!(ai.eco_finishing_task(&ai.eco.workers[&10]), None, "resource-limited work must fit the time budget");
        SPENDING.set(5.0);
        BUILT.set(0.80);
        assert_eq!(
            ai.eco_finishing_task(&ai.eco.workers[&10]),
            Some(finish),
            "80% paid work can be cheap to finish too"
        );
        BUILT.set(0.79);
        assert_eq!(ai.eco_finishing_task(&ai.eco.workers[&10]), None);
        BUILT.set(1.0);
        assert_eq!(ai.eco_finishing_task(&ai.eco.workers[&10]), None);
        BUILT.set(0.99);
        ai.eco.hard_estalled = true;
        assert_eq!(ai.eco_finishing_task(&ai.eco.workers[&10]), None);
        ai.eco.hard_estalled = false;
        ai.eco.workers.get_mut(&10).unwrap().shunned.push((target.pos(), 1000));
        assert_eq!(
            ai.eco_finishing_task(&ai.eco.workers[&10]),
            None,
            "unsafe or unreachable sites still lose their crew"
        );
    }

    #[test]
    fn clustered_spot_beats_lone_one() {
        let spots = [Vec3::xz(0.0, 0.0), Vec3::xz(200.0, 0.0), Vec3::xz(0.0, 300.0), Vec3::xz(3000.0, 0.0)];
        let clustered = cluster_bonus(spots[0], spots.iter().copied());
        let lone = cluster_bonus(spots[3], spots.iter().copied());
        assert_eq!(lone, 0.0);
        assert!((clustered - CLUSTER_BONUS * (2.0 / 3.0 + 0.5)).abs() < 1e-3);
    }

    /// A lone worker's repair cost for a mobile combat unit on our ground,
    /// as `eco_cost_of_job_alone` prices it, against a lone mex there.
    fn repair_cost(dist: f32, worth: f32, health: f32, max_health: f32) -> f32 {
        dist - worth * (250.0 + (max_health + health) / 10.0)
    }

    #[test]
    fn mex_beside_worker_beats_half_dead_dart() {
        // Dart (vehscout): 40 metal, 120 HP, at half health, 50 elmos off;
        // a lone mex 50 elmos off: (dist / 4) - 100.
        let mex = 50.0 / 4.0 - 100.0;
        let dart = repair_cost(50.0, repair_worth(40.0, 60.0, 120.0), 60.0, 120.0);
        assert!(mex < dart, "mex {mex} vs dart {dart}");
        // the flat pull it had picked the Dart
        assert!(repair_cost(50.0, 1.0, 60.0, 120.0) < mex);
    }

    #[test]
    fn scouts_and_small_raiders_do_not_wait_for_repair() {
        // at half health: Dart, Flea, Glaive, Bandit, Dagger lose to a mex
        for (cost, hp) in [(40.0, 120.0), (25.0, 40.0), (65.0, 230.0), (75.0, 350.0), (80.0, 300.0)] {
            assert!(repair_loses_to_mex(cost, hp / 2.0, hp), "{cost} m, {hp} HP");
        }
        // Scorcher, Rogue, Hermit do not
        for (cost, hp) in [(130.0, 480.0), (125.0, 520.0), (145.0, 1550.0)] {
            assert!(!repair_loses_to_mex(cost, hp / 2.0, hp), "{cost} m, {hp} HP");
        }
    }

    #[test]
    fn remex_ceiling_takes_free_mexes_not_crowded_or_raided_ones() {
        // A non-greedy worker's mex price (see `eco_cost_of_job_alone`):
        // (dist / 4) - 100 - cluster + 600 (cost_mod - 2), cost_mod being 1
        // plus the workers already on it nearer than this one.
        let mex = |dist: f32, cluster: f32, on_it: f32| dist / 4.0 - 100.0 - cluster + (600.0 * (on_it - 1.0)).max(0.0);
        // A lone mex at the edge of the radius, and one shared with one other.
        assert!(mex(REMEX_RADIUS, 0.0, 0.0) < REMEX_CEILING);
        assert!(mex(REMEX_RADIUS, 0.0, 1.0) < REMEX_CEILING);
        // A third worker on it, or raiders near it (RAID_COST and up), not.
        assert!(mex(0.0, CLUSTER_CAP, 2.0) >= REMEX_CEILING);
        assert!(mex(0.0, CLUSTER_CAP, 0.0) + RAID_COST >= REMEX_CEILING);
        // Its own mex: a commander joining it (counts 2) does not send the
        // worker off; raiders near it do.
        assert!(mex(REMEX_RADIUS, 0.0, 2.0) - REMEX_KEEP < REMEX_CEILING);
        assert!(mex(0.0, CLUSTER_CAP, 0.0) + RAID_COST - REMEX_KEEP >= REMEX_CEILING);
    }

    #[test]
    fn heavy_repair_keeps_its_full_pull() {
        // 600 metal at half health: 300 metal missing
        assert_eq!(repair_worth(600.0, 1200.0, 2400.0), 1.0);
        assert_eq!(repair_worth(40.0, 120.0, 120.0), 0.0);
        assert_eq!(repair_worth(40.0, 60.0, 0.0), 1.0);
    }

    #[test]
    fn cluster_bonus_is_capped() {
        let ring = (0..12).map(|i| Vec3::xz(50.0 * (i as f32).cos(), 50.0 * (i as f32).sin()));
        assert_eq!(cluster_bonus(Vec3::xz(0.0, 0.0), ring), CLUSTER_CAP);
    }
}
