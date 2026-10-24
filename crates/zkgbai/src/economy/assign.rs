//! Worker assignment: job costs, the round-robin assignment loop, task
//! validation and cleanup.

use springai::{UnitDef, Vec3};

use super::{SITE_SLACK, TaskId, TaskKind, Worker, WorkerTask, erase_one, erase_one_dq};
use crate::Ai;
use crate::military::DefenseTarget;
use crate::util::{NULLPOS, OPTION_SHIFT_KEY, distance};

/// Cost of a job a worker must never take.
// port: the C++ used 65535/65536 here together with "first task always
// wins" (`task == nullptr || tmpcost < cost`), which could hand a worker
// another commander's factory plop; the Java's Float.MAX_VALUE with strict
// comparisons is restored.
const NEVER: f32 = f32::MAX;

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

/// An order for a worker, decided while the task store is borrowed.
enum Order {
    Build(UnitDef, Vec3, i32),
    Repair(springai::Unit),
    ReclaimFeature(springai::Feature),
    ReclaimUnit(springai::Unit),
}

impl Ai {
    pub(super) fn eco_assign_workers(&mut self) {
        let frame = self.frame;
        if frame < 8 {
            return;
        }

        // Generate tasks and assign workers to tasks on alternating frames.
        // Also refresh retreat paths for chickening units and process them when they're done chickening.
        if frame % 2 == 0 {
            if let Some(wid) = self.eco.idlers.pop_front() {
                self.eco_service_worker(wid, false);
            } else if let Some(wid) = self.eco.assigned.pop_front() {
                self.eco_service_worker(wid, true);
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
    fn eco_service_worker(&mut self, wid: i32, check_stuck: bool) {
        let frame = self.frame;
        let Some(w) = self.eco.workers.get(&wid) else {
            return;
        };
        let unit = w.unit;
        if unit.health() <= 0.0 || unit.team() != self.team_id {
            self.eco.remove_worker(wid);
            return;
        }

        let pos = unit.pos();
        // The threat maps as the worker sees them, under water too (see `water`).
        let view = self.war.view(unit.def());
        // (A worker on the crew of an issue worth more to it than its
        // risk stays: see `logistics`.)
        // The operator's greed dial scales the threat a worker perceives.
        let greed = self.op_greed();
        if !w.is_chicken
            && !self.logistics_holds(wid)
            && (view.get_effective_threat(pos) / greed > view.get_total_friendly_threat(pos)
                || ((unit.health() < unit.max_health() || w.get_shields() < 0.5)
                    && view.get_threat(pos) / greed > 0.0
                    && !w.is_working_on_porc(&self.eco.tasks)))
        {
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
        let held = self.eco.terraform_hold.get(&wid).is_some_and(|&until| frame < until);
        if !w.is_chicken && (held || super::terraform::is_terraforming(unit) || self.eco_in_transit(wid)) {
            self.eco.assigned.push_back(wid);
            return;
        }
        let (is_chicken, chicken_frame, last_retreat_frame) = (w.is_chicken, w.chicken_frame, w.last_retreat_frame);
        if is_chicken {
            let view = self.war.view(unit.def());
            if view.get_threat(pos) == 0.0
                && ((view.get_total_friendly_threat(pos) > 0.0 && frame - chicken_frame > 90)
                    || frame - chicken_frame > 150)
            {
                self.eco_stop_chickening(wid);
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
        } else if !check_stuck {
            self.eco_assign_worker(wid);
        } else {
            let unstuck = match self.eco.workers.get_mut(&wid) {
                Some(w) => w.unstick(frame, &mut self.eco.tasks),
                None => true,
            };
            if !unstuck {
                self.eco_assign_worker(wid);
            }
        }
        self.eco.assigned.push_back(wid);
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
        let Some(task_id) = self.eco_get_cheapest_job(wid) else {
            return;
        };
        let Some(w) = self.eco.workers.get(&wid) else {
            return;
        };
        let unit = w.unit;
        let wtask = w.task;
        let wpos = unit.pos();
        let build_dist = unit.def().map_or(0.0, |d| d.build_distance());
        let task_pos = self.eco.task_pos(task_id);

        if wtask == Some(task_id) && distance(wpos, task_pos) <= build_dist * 1.5 {
            return;
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
            if let Some(w) = self.eco.workers.get(&wid) {
                let mut nav = crate::nav!(self);
                w.move_to(task_pos, &mut nav);
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
            if near > 0.0 && near > RAID_HOLD * self.war.get_total_friendly_power(p) {
                cost += RAID_COST + near;
            }
            // The operator's greed dial: far, dangerous jobs priced cheaper.
            cost / greed
        };
        // Only jobs the worker can get to (bots cannot build out at sea).
        let def = worker.unit.def();
        let range = def.map_or(0.0, |d| d.build_distance()) + 32.0;
        let reaches = |t: &WorkerTask| def.is_none_or(|d| self.terrain_reaches(d, wpos, t.get_pos(), range));
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
            if let Some(target) = t.as_construction().and_then(|c| c.target) {
                let p = target.build_progress();
                cost -= 1000.0 * (p * p);
            }
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

        task
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
    fn eco_recent_raiders(&self) -> Vec<(Vec3, f32)> {
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
        if cost >= NEVER || self.dev_off("pairs") || self.defs.name_of(worker.unit) != PAIRED_CON {
            return cost;
        }
        let partner = (task.assigned_workers.iter())
            .filter(|&&id| id != worker.id)
            .filter_map(|id| self.eco.workers.get(id))
            .any(|w| !w.is_com && distance(w.get_pos(), wpos) <= PAIR_RANGE && self.defs.name_of(w.unit) == PAIRED_CON);
        if partner { cost - PAIR_BONUS } else { cost }
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

                // On the crew of a tracked issue: its share of the value
                // (see `logistics`).
                if let Some(share) = self.logistics_bonus(worker.id, task.id) {
                    return (dist / 4.0) - share;
                }

                let ids = &eco.build_ids;
                let def_id = ctask.build_type.id();
                if def_id == ids.solar_id || def_id == ids.wind_id {
                    // for small energy
                    if eco.estalled || eco.effective_income_metal > eco.effective_income_energy - 2.5 {
                        if worker.is_greedy {
                            return dist + (10000.0 * cost_mod); // greedy workers shouldn't build energy at all.
                        }
                        if worker.is_com {
                            if eco.hard_estalled {
                                return ((dist / 2.0) - 300.0) + (1000.0 * (cost_mod - 2.0)).max(0.0);
                            }
                            return ((dist / 3.0) - 200.0) + (1000.0 * (cost_mod - 2.0)).max(0.0);
                        }
                        return ((dist / 3.0) - 200.0) + (1000.0 * (cost_mod - 1.0)).max(0.0);
                    } else {
                        if worker.is_greedy {
                            return dist + (10000.0 * cost_mod); // greedy workers shouldn't build energy at all.
                        }
                        if worker.is_com {
                            return (dist / 4.0) + (1000.0 * (cost_mod - 2.0)).max(0.0);
                        }
                        return (dist / 4.0) + (1000.0 * (cost_mod - 1.0)).max(0.0);
                    }
                }

                if def_id == ids.mex_id {
                    // for mexes
                    let cluster = self.eco_mex_cluster_bonus(task_pos);
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

                if ids.expensive_ids.contains(&def_id) {
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
                    let (nfus, npyl) = (eco.fusions.len(), eco.pylons.len());
                    if eco.pending_power.is_some() {
                        return (dist / 4.0) - 3000.0 + (500.0 * (cost_mod - 1.0));
                    }
                    if worker.is_greedy {
                        if nfus > npyl {
                            return (dist / 4.0) - 3000.0 + (500.0 * (cost_mod - 1.0));
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

                let mut rec_mod = 100.0;
                if self.graph.is_ally_territory(task_pos) {
                    let tf = self.graph.territory_fraction;
                    rec_mod = 100.0 + (200.0 * tf * tf);
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

                    if rptask.is_shield_mob && worker.has_shields {
                        return dist + (100.0 * (cost_mod - 1.0)) - 250.0 - max_health / (5.0 * cost_mod);
                    }

                    if !self.graph.is_ally_territory(task_pos) {
                        return dist + 500.0;
                    }

                    dist + (100.0 * (cost_mod - 1.0)) - 250.0 - (max_health + health) / (10.0 * cost_mod)
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
    /// factories.
    pub(super) fn eco_build_check(&self, def: UnitDef, pos: Vec3, facing: i32) -> bool {
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
                let (cxsize, czsize) = extents(c.build_type, c.facing, 0.0);
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
            let (cxsize, czsize) = extents(fdef, fac.building_facing(), 1.0);
            if overlaps(fac.pos(), cxsize, czsize) {
                return false;
            }
        }
        true
    }

    /// Remove finished or invalid tasks, sending their workers to be reassigned.
    pub(super) fn eco_clean_orders(&mut self) {
        let frame = self.frame;
        //remove invalid jobs from the queue
        let mut invalid: Vec<TaskId> = Vec::new();
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
            if target.is_none() && !map.is_possible_to_build_at(bt, pos, facing) {
                //check to make sure it isn't our own nanoframe, since update is called before unitCreated
                let radius = bt.x_size().min(bt.z_size()) as f32 * 4.0;
                let nano = self.cb.friendly_units_in(pos, radius).into_iter().find(|u| {
                    u.is_being_built() && u.def().is_some_and(|d| d.id() == bt.id()) && u.team() == self.team_id
                });
                match nano {
                    Some(u) => {
                        target = Some(u);
                        if let Some(c) = self.eco.tasks.get_mut(id).and_then(|t| t.as_construction_mut()) {
                            c.target = Some(u);
                        }
                    }
                    // if a construction job is blocked and it isn't our own nanoframe, remove it
                    None => {
                        if !invalid.contains(&id) {
                            invalid.push(id);
                        }
                    }
                }
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
mod tests {
    use super::*;

    #[test]
    fn clustered_spot_beats_lone_one() {
        let spots = [Vec3::xz(0.0, 0.0), Vec3::xz(200.0, 0.0), Vec3::xz(0.0, 300.0), Vec3::xz(3000.0, 0.0)];
        let clustered = cluster_bonus(spots[0], spots.iter().copied());
        let lone = cluster_bonus(spots[3], spots.iter().copied());
        assert_eq!(lone, 0.0);
        assert!((clustered - CLUSTER_BONUS * (2.0 / 3.0 + 0.5)).abs() < 1e-3);
    }

    #[test]
    fn cluster_bonus_is_capped() {
        let ring = (0..12).map(|i| Vec3::xz(50.0 * (i as f32).cos(), 50.0 * (i as f32).sin()));
        assert_eq!(cluster_bonus(Vec3::xz(0.0, 0.0), ring), CLUSTER_CAP);
    }
}
