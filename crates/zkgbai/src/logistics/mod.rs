//! Logistics: the AI's project tracker.
//!
//! Construction jobs become *issues* with a value in metal that depends on
//! when they finish: a defense is worth what it saves only if it stands
//! before the raid arrives. Every second the tracker forecasts each
//! issue's completion from the workers that could crew it (travel, build
//! power as each arrives, metal), picks the smallest crew that gets the
//! value, and hands each crew member's share of it to the assignment loop
//! as a bonus ([`Ai::logistics_bonus`]). While an urgent issue is under way
//! it can also hold back the metal of other construction and of the
//! factories (Zero-K build priority; the `pause` feature).
//!
//! The first client is defense against incursions the terrain model
//! forecasts (`incursions`): at the first of our assets an enemy group can
//! reach, a turret sized to the group, if one can stand in time and the
//! value at risk pays for it. The others (feature `clients`) are capital
//! that waits: pylons while a building waits for its site to have power
//! (`grid`), a factory a module requisitioned, an anti-nuke; each worth
//! what it unlocks or saves, less a share of it for every second it is
//! late. The operator can open issues of its own (tool `issue`).
//!
//! The crew's bonus is in metal, so the danger of the job can be too
//! (feature `currency`, off by default: it lost its mirrors): what sending
//! the worker risks is its cost times the chance of losing it there, and a
//! worker whose share is more than that stays on the job where it would
//! otherwise run.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{Value, json};
use springai::{NO_TIMEOUT, Unit, UnitDef, Vec3};

use crate::Ai;
use crate::defs::cmd;
use crate::economy::{TaskId, Worker};
use crate::military::wrappers::weapon_power;
use crate::util::{distance, get_directional_point};

/// Frames between assessments.
const PERIOD: i32 = 30;

/// Our value within this of a defended point is at risk, elmos.
const VAR_RADIUS: f32 = 600.0;

/// Value kept from being lost counts this much of value gained.
const VAR_UTILITY: f32 = 0.5;

/// A defense is opened when it pays at least this much over its cost.
const MIN_NET: f32 = 200.0;

/// Seconds past its deadline over which an issue's value falls to nothing.
const DECAY: f32 = 20.0;

/// A defense is due this many seconds before the enemy arrives.
const MARGIN: f32 = 5.0;

/// Workers an issue gets at most.
const MAX_CREW: usize = 6;

/// Forecast completion times are stretched by this: workers path around
/// things, get shot at and stall for metal (measured: jobs forecast at 14 s
/// took 40 s when late).
const PESSIMISM: f32 = 1.5;

/// A defense opens only if the forecast gets this much of what it is worth
/// (it stands in time, more or less); late turrets came out late or not at
/// all.
const IN_TIME: f32 = 0.8;

/// A crew member's bonus in the assignment loop, at most.
const BONUS_CAP: f32 = 3500.0;

/// Seconds a current crew member counts nearer than it is, so crews do
/// not churn.
const HYSTERESIS: f32 = 4.0;

/// Porc already planned this close to a defense's site is that defense.
const ADOPT_RADIUS: f32 = 250.0;

/// One defense issue per this much ground.
const SAME_RADIUS: f32 = 300.0;

/// The turret stands this far from the asset towards the enemy, elmos.
const OFFSET: f32 = 120.0;

/// Units dearer than this are not real (terraform placeholders).
const UNIT_COST_CAP: f32 = 50000.0;

/// Construction cheaper than this is not paused (mexes, solars).
const PAUSE_MIN_COST: f32 = 150.0;

/// Frames between summaries in the log.
const STATS_PERIOD: i32 = 1800;

/// Capital pays back over this long (seconds): what a building that
/// unlocks another (a pylon for a Bertha), or a factory that answers a
/// threat, loses for every second it is late.
pub const PAYBACK: f32 = 600.0;

/// Odds a nuke is coming when the intel only suspects one.
const SUSPICION_ODDS: f32 = 0.3;

/// Candidate turrets for an emergency defense.
const TURRETS: [&str; 6] =
    ["turretlaser", "turretmissile", "turretriot", "turretheavylaser", "turretemp", "turretgauss"];

/// What an issue is worth, as a function of when it is done (game
/// seconds).
#[derive(Clone, Copy, Debug)]
pub enum Worth {
    /// The same whenever.
    Flat(f32),
    /// All of it by `by`, falling to nothing over [`DECAY`] seconds after.
    Deadline { value: f32, by: f32 },
    /// All of it at `from`, less `rate` for every second after.
    Stream { value: f32, from: f32, rate: f32 },
}

impl Worth {
    pub fn at(&self, done: f32) -> f32 {
        match *self {
            Worth::Flat(v) => v,
            Worth::Deadline { value, by } => {
                if done <= by {
                    value
                } else {
                    value * (1.0 - (done - by) / DECAY).max(0.0)
                }
            }
            Worth::Stream { value, from, rate } => (value - rate * (done - from).max(0.0)).max(0.0),
        }
    }

    pub fn most(&self) -> f32 {
        match *self {
            Worth::Flat(v) | Worth::Deadline { value: v, .. } | Worth::Stream { value: v, .. } => v,
        }
    }

    pub fn deadline(&self) -> Option<f32> {
        match *self {
            Worth::Flat(_) | Worth::Stream { .. } => None,
            Worth::Deadline { by, .. } => Some(by),
        }
    }

    fn value(&self) -> Value {
        match *self {
            Worth::Flat(v) => json!({"flat": v}),
            Worth::Deadline { value, by } => json!({"value": value, "by": by}),
            Worth::Stream { value, from, rate } => json!({"value": value, "from": from, "rate": rate}),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Defense,
    Grid,
    Factory,
    Antinuke,
    Operator,
}

impl Kind {
    fn name(self) -> &'static str {
        match self {
            Kind::Defense => "defense",
            Kind::Grid => "grid",
            Kind::Factory => "factory",
            Kind::Antinuke => "antinuke",
            Kind::Operator => "operator",
        }
    }
}

/// A construction job the tracker values.
pub struct Issue {
    pub id: u32,
    pub task: TaskId,
    pub kind: Kind,
    pub what: String,
    pub pos: Vec3,
    pub cost: f32,
    pub build_time: f32,
    pub worth: Worth,
    pub opened: i32,
    pub why: String,
    /// The enemy power it is against (defenses).
    pub against: f32,
    /// The latest forecast: the crew, seconds until done with it, what it
    /// is worth then, and each crew member's bonus.
    pub crew: Vec<i32>,
    pub eta: f32,
    pub value: f32,
    pub share: f32,
    pub started: bool,
}

impl Issue {
    fn json(&self) -> Value {
        json!({
            "id": self.id,
            "task": self.task,
            "kind": self.kind.name(),
            "what": self.what,
            "pos": [self.pos.x.round(), self.pos.z.round()],
            "cost": self.cost,
            "worth": self.worth.value(),
            "why": self.why,
            "against": self.against,
            "crew": self.crew,
            "eta": (self.eta * 10.0).round() / 10.0,
            "value": self.value.round(),
            "share": self.share.round(),
            "started": self.started,
            "opened": self.opened,
        })
    }
}

/// Running totals, logged every [`STATS_PERIOD`] frames and at the end.
#[derive(Default, Clone, Copy)]
pub struct Stats {
    pub opened: u32,
    pub adopted: u32,
    /// Closed because the building stands: in time, or late.
    pub met: u32,
    pub late: u32,
    /// Closed because the tracker dropped a hopeless job.
    pub abandoned: u32,
    /// Closed because the job went away otherwise.
    pub gone: u32,
    /// Times the other construction was paused for an urgent issue.
    pub pauses: u32,
    /// Forecast arrivals looked at, and why no defense was opened: our
    /// defenses there suffice, too little at risk, no turret pays for
    /// itself in time, no site.
    pub looked: u32,
    pub defended: u32,
    pub cheap: u32,
    pub unpaid: u32,
    pub nosite: u32,
}

impl Stats {
    pub fn json(&self) -> Value {
        json!({
            "opened": self.opened, "adopted": self.adopted, "met": self.met, "late": self.late,
            "abandoned": self.abandoned, "gone": self.gone, "pauses": self.pauses,
            "looked": self.looked, "defended": self.defended, "cheap": self.cheap, "unpaid": self.unpaid,
            "nosite": self.nosite,
        })
    }
}

#[derive(Default)]
pub struct Logistics {
    pub issues: BTreeMap<TaskId, Issue>,
    next_id: u32,
    pub stats: Stats,
    /// Construction and factories set to low priority for an urgent issue.
    paused: Vec<Unit>,
    frame: i32,
}

/// Defense power of a turret of type `def`, on the scale of the enemy
/// danger (see `Porc::get_power`).
fn porc_power(def: UnitDef) -> f32 {
    let weapons: f32 =
        (def.weapon_mounts().iter().filter_map(|m| m.weapon_def()).filter(|wd| !wd.name().contains("fake")))
            .map(|wd| weapon_power(wd, true))
            .sum();
    (weapons + def.health()) / 10.0
}

/// The chance that an enemy force of `enemy` power overruns `ours`.
pub(crate) fn loss_chance(enemy: f32, ours: f32) -> f32 {
    if enemy <= 0.0 {
        return 0.0;
    }
    let r = (ours / enemy).max(0.0);
    (1.0 - r * r).clamp(0.0, 1.0)
}

/// A worker that could crew a job: id, seconds until it is there, build
/// power.
type Hand = (i32, f32, f32);

/// Seconds until a crew of `hands` (arriving as they say) finishes `work`
/// build points that still cost `cost` metal, with `metal` in store and
/// `income` a second.
fn finish_time(hands: &[Hand], work: f32, cost: f32, metal: f32, income: f32) -> f32 {
    let mut left = work;
    let mut t = 0.0f32;
    let mut bp = 0.0f32;
    for &(_, arrive, b) in hands {
        if arrive > t {
            if bp > 0.0 {
                let done = bp * (arrive - t);
                if done >= left {
                    return (t + left / bp).max(metal_time(cost, metal, income));
                }
                left -= done;
            }
            t = arrive;
        }
        bp += b;
    }
    if bp <= 0.0 {
        return f32::INFINITY;
    }
    (t + left / bp).max(metal_time(cost, metal, income))
}

fn metal_time(cost: f32, metal: f32, income: f32) -> f32 {
    (cost - metal).max(0.0) / income.max(1.0)
}

/// The smallest crew (nearest hands first) that gets nearly all the value
/// any crew would: how many, seconds until done, the value then.
fn best_crew(
    hands: &[Hand],
    work: f32,
    cost: f32,
    metal: f32,
    income: f32,
    worth: &Worth,
    now: f32,
) -> (usize, f32, f32) {
    let mut plans = Vec::with_capacity(MAX_CREW);
    for k in 1..=hands.len().min(MAX_CREW) {
        let eta = finish_time(&hands[..k], work, cost, metal, income) * PESSIMISM;
        plans.push((k, eta, worth.at(now + eta)));
    }
    let Some(top) = plans.iter().map(|p| p.2).fold(None, |m: Option<f32>, v| Some(m.map_or(v, |m| m.max(v)))) else {
        return (0, f32::INFINITY, 0.0);
    };
    if top <= 0.0 {
        // Hopeless for now: the nearest one, so the forecast says when.
        return plans.first().copied().unwrap_or((0, f32::INFINITY, 0.0));
    }
    plans.into_iter().find(|p| p.2 >= 0.95 * top).unwrap_or((0, f32::INFINITY, 0.0))
}

impl Ai {
    /// Re-assess the issues every [`PERIOD`] frames (after `incursions`).
    pub fn logistics_update(&mut self, frame: i32) {
        if self.dev_off("logistics") {
            return;
        }
        if (frame + self.offset + 13) % PERIOD != 0 {
            return;
        }
        let now = frame as f32 / 30.0;
        self.logistics_close_gone(now);
        self.logistics_defenses(now);
        self.logistics_grid(now);
        self.logistics_forecast(now);
        if !self.dev_off("pause") {
            self.logistics_pause(now);
        }
        if frame / STATS_PERIOD != self.logistics.frame / STATS_PERIOD {
            self.logistics_summary();
        }
        self.logistics.frame = frame;
    }

    pub fn logistics_summary(&self) {
        let s = self.logistics.stats;
        self.log(&format!(
            "logistics: {} issues opened ({} adopted), {} met, {} late, {} abandoned, {} gone; {} open; {} pauses; \
             of {} arrivals forecast: {} defended, {} too little at risk, {} no turret pays, {} no site",
            s.opened,
            s.adopted,
            s.met,
            s.late,
            s.abandoned,
            s.gone,
            self.logistics.issues.len(),
            s.pauses,
            s.looked,
            s.defended,
            s.cheap,
            s.unpaid,
            s.nosite
        ));
    }

    /// The bonus worker `wid` gets in the assignment loop for task `task`:
    /// its share of the issue's value, if it is on the crew.
    pub fn logistics_bonus(&self, wid: i32, task: TaskId) -> Option<f32> {
        let issue = self.logistics.issues.get(&task)?;
        (issue.share > 0.0 && issue.crew.contains(&wid)).then_some(issue.share)
    }

    /// Whether `task` is an open issue.
    pub fn logistics_tracks(&self, task: TaskId) -> bool {
        self.logistics.issues.contains_key(&task)
    }

    /// What sending worker `w` to `p` risks, in metal: its cost times the
    /// chance of losing it to the enemy power there (or seen near there
    /// lately, `raiders_near`: the raid memory) against ours.
    pub fn logistics_risk(&self, w: &Worker, p: Vec3, raiders_near: f32) -> f32 {
        let Some(cost) = w.unit.def().map(|d| d.cost(self.metal)) else { return 0.0 };
        let enemy = self.war.get_enemy_power(p).max(raiders_near);
        cost * loss_chance(enemy, self.war.get_total_friendly_power(p))
    }

    /// Whether worker `wid` stays on its job in danger: it is on the crew
    /// of the issue its job is, and its share of the value is more than
    /// what it risks. (Commanders never: they are worth more than they
    /// cost.)
    pub fn logistics_holds(&self, wid: i32) -> bool {
        if self.dev_off("currency") {
            return false;
        }
        let Some(w) = self.eco.workers.get(&wid) else { return false };
        if w.is_com {
            return false;
        }
        let Some(share) = w.task.and_then(|t| self.logistics_bonus(wid, t)) else { return false };
        let p = w.get_pos();
        share > self.logistics_risk(w, p, self.eco_raiders_near(p))
    }

    /// Open an issue on construction task `task` (or re-value one).
    pub fn logistics_open(&mut self, task: TaskId, kind: Kind, worth: Worth, why: &str, against: f32) -> Option<u32> {
        let t = self.eco.tasks.get(task)?;
        let c = t.as_construction()?;
        let what = self.defs.name(c.build_type).to_owned();
        let pos = t.get_pos();
        let (cost, build_time) = (c.build_type.cost(self.metal), c.build_type.build_time());
        if let Some(i) = self.logistics.issues.get_mut(&task) {
            i.worth = worth;
            i.why = why.to_owned();
            i.against = against;
            i.kind = kind;
            return Some(i.id);
        }
        self.logistics.next_id += 1;
        let id = self.logistics.next_id;
        let issue = Issue {
            id,
            task,
            kind,
            what: what.clone(),
            pos,
            cost,
            build_time,
            worth,
            opened: self.frame,
            why: why.to_owned(),
            against,
            crew: Vec::new(),
            eta: f32::INFINITY,
            value: 0.0,
            share: 0.0,
            started: c.target.is_some(),
        };
        self.log(&format!(
            "logistics: issue #{id} ({}): {what} at ({:.0}, {:.0}) worth {} ({why})",
            kind.name(),
            pos.x,
            pos.z,
            match worth {
                Worth::Flat(v) => format!("{v:.0}"),
                Worth::Deadline { value, by } => format!("{value:.0} by {:.0}s", by - self.frame as f32 / 30.0),
                Worth::Stream { value, rate, .. } => format!("{value:.0} less {rate:.1}/s late"),
            }
        ));
        self.op_emit("issue", json!({"event": "open", "issue": issue.json()}));
        self.logistics.issues.insert(task, issue);
        self.logistics.stats.opened += 1;
        Some(id)
    }

    /// Close the issue on `task`, saying how it ended.
    pub fn logistics_close(&mut self, task: TaskId, outcome: &str) -> Option<Issue> {
        let issue = self.logistics.issues.remove(&task)?;
        self.log(&format!(
            "logistics: issue #{} {}: {} at ({:.0}, {:.0}) after {:.0}s",
            issue.id,
            outcome,
            issue.what,
            issue.pos.x,
            issue.pos.z,
            (self.frame - issue.opened) as f32 / 30.0
        ));
        self.op_emit("issue", json!({"event": "close", "outcome": outcome, "issue": issue.json()}));
        Some(issue)
    }

    /// Our value (unit costs) within [`VAR_RADIUS`] of `p`.
    fn logistics_value_at_risk(&self, p: Vec3) -> f32 {
        let team = self.team_id;
        (self.cb.friendly_units_in(p, VAR_RADIUS).into_iter())
            .filter(|u| u.team() == team)
            .filter_map(|u| Some(u.def()?.cost(self.metal)))
            // Terraform placeholders cost 100000 on paper.
            .filter(|&c| c < UNIT_COST_CAP)
            .sum()
    }

    /// Workers that could crew a job at `pos`, nearest first (the current
    /// `crew` a little nearer than it is).
    fn logistics_hands(&self, pos: Vec3, crew: &[i32]) -> Vec<Hand> {
        let mut hands: Vec<Hand> = (self.eco.workers.values())
            .filter(|w| !w.is_chicken && w.bp > 0.0)
            .filter_map(|w| {
                let d = w.unit.def()?;
                let speed = d.speed();
                if speed <= 0.0 {
                    return None;
                }
                let wpos = w.get_pos();
                let range = d.build_distance();
                if !self.terrain_reaches(d, wpos, pos, range + 32.0) {
                    return None;
                }
                let mut travel = (distance(wpos, pos) - range).max(0.0) / speed;
                if crew.contains(&w.id) {
                    travel = (travel - HYSTERESIS).max(0.0);
                }
                Some((w.id, travel, w.bp))
            })
            .collect();
        hands.sort_by(|a, b| a.1.total_cmp(&b.1));
        hands
    }

    /// Issues whose job is gone: closed as met, late or gone.
    fn logistics_close_gone(&mut self, now: f32) {
        let gone: Vec<TaskId> =
            self.logistics.issues.keys().copied().filter(|&t| self.eco.tasks.get(t).is_none()).collect();
        for task in gone {
            let Some(issue) = self.logistics.issues.get(&task) else { continue };
            let about = self.cb.friendly_units_in(issue.pos, 48.0);
            let stands = (self.eco.porcs.iter().chain(about.iter()))
                .any(|u| distance(u.pos(), issue.pos) < 48.0 && self.defs.name_of(*u) == issue.what);
            let outcome = if !stands {
                self.logistics.stats.gone += 1;
                "gone"
            } else if issue.worth.deadline().is_none_or(|by| now <= by + MARGIN) {
                self.logistics.stats.met += 1;
                "met"
            } else {
                self.logistics.stats.late += 1;
                "late"
            };
            self.logistics_close(task, outcome);
        }
    }

    /// Defenses against the incursions forecast: where an enemy group gets
    /// to our value first, a turret that pays, if one can stand in time.
    fn logistics_defenses(&mut self, now: f32) {
        if self.dev_off("porc") {
            return;
        }
        let predicted = self.incursions.predicted.clone();
        let metal = self.eco.metal;
        let income = self.eco.effective_income;
        for p in predicted {
            // Already there: too late to plan for.
            if p.seconds <= MARGIN {
                continue;
            }
            let same = |i: &Issue| i.kind == Kind::Defense && distance(i.pos, p.pos) < SAME_RADIUS;
            if self.logistics.issues.values().any(same) {
                continue;
            }
            self.logistics.stats.looked += 1;
            let var = self.logistics_value_at_risk(p.pos);
            let ours = self.war.get_total_friendly_power(p.pos);
            let before = loss_chance(p.power, ours);
            if before <= 0.0 {
                self.logistics.stats.defended += 1;
                continue;
            }
            if var * before * VAR_UTILITY < MIN_NET {
                self.logistics.stats.cheap += 1;
                continue;
            }
            let by = now + p.seconds - MARGIN;
            let hands = self.logistics_hands(p.pos, &[]);
            if hands.is_empty() {
                continue;
            }
            let mut best: Option<(UnitDef, f32, f32, f32, f32)> = None;
            for name in TURRETS {
                let Some(def) = self.defs.get(name) else { continue };
                let cost = def.cost(self.metal);
                let after = loss_chance(p.power, ours + porc_power(def));
                let saved = var * (before - after) * VAR_UTILITY;
                let worth = Worth::Deadline { value: saved, by };
                let (_, eta, value) = best_crew(&hands, def.build_time(), cost, metal, income, &worth, now);
                let net = value - cost;
                if value >= IN_TIME * saved && net >= MIN_NET && best.as_ref().is_none_or(|b| net > b.1) {
                    best = Some((def, net, saved, eta, after));
                }
            }
            let Some((def, net, saved, eta, after)) = best else {
                self.logistics.stats.unpaid += 1;
                continue;
            };
            let site = if distance(p.pos, p.from) > 1.0 { get_directional_point(p.pos, p.from, OFFSET) } else { p.pos };
            let Some((task, adopted)) = self.eco_logistics_porc(def, site, ADOPT_RADIUS) else {
                self.logistics.stats.nosite += 1;
                continue;
            };
            if self.logistics.issues.contains_key(&task) {
                continue;
            }
            let why = format!(
                "{:.0} enemy power {:.0}s away; {:.0} at risk, loss {:.0}% -> {:.0}%, eta {eta:.0}s, net {net:.0}, \
                 threat at the site {:.1} vs ours {:.1}, {} hands",
                p.power,
                p.seconds,
                var,
                before * 100.0,
                after * 100.0,
                self.war.get_effective_threat(site),
                self.war.get_total_friendly_threat(site),
                hands.len()
            );
            if self.logistics_open(task, Kind::Defense, Worth::Deadline { value: saved, by }, &why, p.power).is_some()
                && adopted
            {
                self.logistics.stats.adopted += 1;
            }
        }
    }

    /// Pylons while a building waits for its site to have power (`grid`):
    /// what waits is capital idle, worth its cost less a [`PAYBACK`] share
    /// of it for every second late.
    fn logistics_grid(&mut self, now: f32) {
        if self.dev_off("clients") {
            return;
        }
        let Some((def, pos)) = self.eco_awaiting() else {
            let powered: Vec<TaskId> =
                self.logistics.issues.values().filter(|i| i.kind == Kind::Grid).map(|i| i.task).collect();
            for task in powered {
                self.logistics_close(task, "powered");
            }
            return;
        };
        let value = def.cost(self.metal);
        let why = format!("a {} at ({:.0}, {:.0}) waits for power", self.defs.name(def), pos.x, pos.z);
        let worth = Worth::Stream { value, from: now, rate: value / PAYBACK };
        for task in self.eco.pylon_tasks().to_vec() {
            if !self.logistics.issues.contains_key(&task) {
                self.logistics_open(task, Kind::Grid, worth, &why, 0.0);
            }
        }
    }

    /// A factory a module requisitioned (task `task`): worth the usual
    /// share of the `value` it answers, less a [`PAYBACK`] share of that
    /// for every second it is not there.
    pub fn logistics_factory(&mut self, task: TaskId, value: f32, why: &str) {
        if self.dev_off("logistics") || self.dev_off("clients") || value <= 0.0 {
            return;
        }
        let value = value * VAR_UTILITY;
        let now = self.frame as f32 / 30.0;
        self.logistics_open(task, Kind::Factory, Worth::Stream { value, from: now, rate: value / PAYBACK }, why, 0.0);
    }

    /// An anti-nuke (task `task`): worth what the worst blast on us would
    /// take, by the odds a nuke is coming (`sure`, or the intel's
    /// suspicion), less that again for every missile's worth of the
    /// enemy's income late.
    pub fn logistics_antinuke(&mut self, task: TaskId, sure: bool) {
        if self.dev_off("logistics") || self.dev_off("clients") {
            return;
        }
        let blast = self.nukes_worst_blast();
        let odds = if sure { 1.0 } else { SUSPICION_ODDS };
        let value = blast * odds;
        let missile =
            (self.defs.get("staticnuke").and_then(|d| d.stockpile_def())).map_or(2000.0, |w| w.cost(self.metal));
        let cycle = (missile / self.intel.income.max(1.0)).max(30.0);
        let now = self.frame as f32 / 30.0;
        let why = format!("{blast:.0} metal in the worst blast, odds {odds:.1}, a missile every {cycle:.0}s");
        self.logistics_open(task, Kind::Antinuke, Worth::Stream { value, from: now, rate: value / cycle }, &why, 0.0);
    }

    /// Forecast every open issue: its crew, when it is done, what it is
    /// worth then; drop jobs past saving.
    fn logistics_forecast(&mut self, now: f32) {
        let metal = self.eco.metal;
        let income = self.eco.effective_income;
        let tasks: Vec<TaskId> = self.logistics.issues.keys().copied().collect();
        let mut abandon = Vec::new();
        for task in tasks {
            let Some(t) = self.eco.tasks.get(task) else { continue };
            let Some(c) = t.as_construction() else { continue };
            let progress = c.target.map_or(0.0, |u| u.build_progress().clamp(0.0, 1.0));
            let started = c.target.is_some();
            let Some(issue) = self.logistics.issues.get(&task) else { continue };
            let work = issue.build_time * (1.0 - progress);
            let cost = issue.cost * (1.0 - progress);
            let hands = self.logistics_hands(issue.pos, &issue.crew);
            let (k, eta, value) = best_crew(&hands, work, cost, metal, income, &issue.worth, now);
            let hopeless = value <= 0.0 && !started && issue.worth.deadline().is_some_and(|by| now > by + DECAY);
            let Some(issue) = self.logistics.issues.get_mut(&task) else { continue };
            issue.started = started;
            issue.eta = eta;
            issue.value = value;
            issue.crew = hands.iter().take(k).map(|h| h.0).collect();
            issue.share = if k > 0 && value > 0.0 { (value / k as f32).min(BONUS_CAP) } else { 0.0 };
            if hopeless {
                abandon.push(task);
            }
        }
        for task in abandon {
            self.logistics.stats.abandoned += 1;
            self.logistics_close(task, "abandoned");
            self.eco.eco_logistics_drop(task);
        }
    }

    /// Zero-K build priority: an urgent defense under way gets high, the
    /// other construction and the factories low, until it is done.
    fn logistics_pause(&mut self, now: f32) {
        let urgent: Vec<Unit> = (self.logistics.issues.values())
            .filter(|i| i.kind == Kind::Defense && i.started && i.value >= MIN_NET)
            .filter(|i| i.worth.deadline().is_none_or(|by| now <= by + DECAY))
            .filter_map(|i| self.eco.tasks.get(i.task)?.as_construction()?.target)
            .collect();
        if urgent.is_empty() {
            if !self.logistics.paused.is_empty() {
                for u in self.logistics.paused.drain(..) {
                    if u.health() > 0.0 && u.team() == self.team_id {
                        u.execute_custom_command(cmd::PRIORITY, &[1.0], 0, NO_TIMEOUT);
                    }
                }
            }
            return;
        }
        let keep: BTreeSet<i32> = (self.logistics.issues.values())
            .filter_map(|i| self.eco.tasks.get(i.task)?.as_construction()?.target)
            .map(|u| u.id())
            .collect();
        for u in &urgent {
            u.execute_custom_command(cmd::PRIORITY, &[2.0], 0, NO_TIMEOUT);
        }
        let team = self.team_id;
        let mut pause: Vec<Unit> = (self.cb.team_units().into_iter())
            .filter(|u| u.team() == team && u.is_being_built() && !keep.contains(&u.id()))
            .filter(|u| u.def().is_some_and(|d| d.speed() <= 0.0 && d.cost(self.metal) >= PAUSE_MIN_COST))
            .collect();
        pause.extend(self.fac.factories.values().map(|f| f.unit));
        let fresh = self.logistics.paused.is_empty();
        for u in pause {
            if !self.logistics.paused.contains(&u) {
                u.execute_custom_command(cmd::PRIORITY, &[0.0], 0, NO_TIMEOUT);
                self.logistics.paused.push(u);
            }
        }
        if fresh && !self.logistics.paused.is_empty() {
            self.logistics.stats.pauses += 1;
            self.log(&format!(
                "logistics: {} jobs and factories paused for {} urgent defense(s)",
                self.logistics.paused.len(),
                urgent.len()
            ));
        }
    }

    /// The open issues and the totals, for the operator.
    pub fn logistics_json(&self) -> Value {
        let issues: Vec<Value> = self.logistics.issues.values().map(Issue::json).collect();
        json!({"issues": issues, "stats": self.logistics.stats.json(), "paused": self.logistics.paused.len()})
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crew_joins_as_it_arrives() {
        // One hand on site with 10 bp: 100 work takes 10 s.
        assert_eq!(finish_time(&[(1, 0.0, 10.0)], 100.0, 0.0, 0.0, 10.0), 10.0);
        // A second arriving at 5 s halves the rest: 5 s + 50 / 20 = 7.5 s.
        assert_eq!(finish_time(&[(1, 0.0, 10.0), (2, 5.0, 10.0)], 100.0, 0.0, 0.0, 10.0), 7.5);
        // Done before the second arrives.
        assert_eq!(finish_time(&[(1, 0.0, 10.0), (2, 50.0, 10.0)], 100.0, 0.0, 0.0, 10.0), 10.0);
        // Metal binds: 200 to pay, none stored, 10 a second.
        assert_eq!(finish_time(&[(1, 0.0, 10.0)], 100.0, 200.0, 0.0, 10.0), 20.0);
        assert!(finish_time(&[], 100.0, 0.0, 0.0, 10.0).is_infinite());
    }

    #[test]
    fn smallest_crew_that_meets_the_deadline() {
        let hands = [(1, 0.0, 10.0), (2, 2.0, 10.0), (3, 4.0, 10.0), (4, 30.0, 10.0)];
        let worth = Worth::Deadline { value: 1000.0, by: 9.0 };
        // Alone: 10 s, 15 with pessimism (value 1000 * (1 - 6/20) = 700);
        // two: 2 + 80/20 = 6 s, 9 with pessimism: in time.
        let (k, eta, v) = best_crew(&hands, 100.0, 0.0, 0.0, 10.0, &worth, 0.0);
        assert_eq!(k, 2);
        assert!((eta - 9.0).abs() < 1e-4 && (v - 1000.0).abs() < 1e-3, "{eta} {v}");
        // Past saving: the nearest one, no value.
        let worth = Worth::Deadline { value: 1000.0, by: -30.0 };
        let (k, _, v) = best_crew(&hands, 100.0, 0.0, 0.0, 10.0, &worth, 0.0);
        assert_eq!((k, v), (1, 0.0));
    }

    #[test]
    fn worth_decays_past_the_deadline() {
        let w = Worth::Deadline { value: 100.0, by: 10.0 };
        assert_eq!(w.at(10.0), 100.0);
        assert_eq!(w.at(20.0), 50.0);
        assert_eq!(w.at(40.0), 0.0);
        assert_eq!(Worth::Flat(7.0).at(1e9), 7.0);
    }

    #[test]
    fn a_stream_is_worth_less_the_longer_it_waits() {
        let w = Worth::Stream { value: 3000.0, from: 100.0, rate: 5.0 };
        assert_eq!(w.at(100.0), 3000.0);
        assert_eq!(w.at(50.0), 3000.0);
        assert_eq!(w.at(160.0), 2700.0);
        assert_eq!(w.at(1000.0), 0.0);
        assert_eq!(w.deadline(), None);
    }

    #[test]
    fn stronger_defense_lowers_the_loss_chance() {
        assert_eq!(loss_chance(100.0, 0.0), 1.0);
        assert_eq!(loss_chance(100.0, 100.0), 0.0);
        assert!((loss_chance(100.0, 50.0) - 0.75).abs() < 1e-6);
        assert_eq!(loss_chance(0.0, 0.0), 0.0);
    }
}
