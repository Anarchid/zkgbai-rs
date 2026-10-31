//! Conquest: walk the commander to forward ground a few turrets can hold,
//! take its metal, and fortify it when what the turrets deny pays for them
//! (feature `conquest`, off by default; user, 2026-10-31).
//!
//! The objectives come from `zk_terrain::conquest`, once per game on a
//! background thread: turret sites (Stingers by default) and the neutral
//! spots near them, each valued per enemy and movement tier as the income
//! the turrets put out of a raider's reach or behind a longer way round.
//! What the enemy fields is a belief per enemy team: Zero-K's ground
//! factories as the prior (three bot factories, two vehicle, one each
//! hover, spider, amphibious), replaced by the metal of what we have seen
//! of each tier (a factory counts as much as an army). The walk and the
//! mexes hold whatever the enemy fields and go first; the turrets are the
//! commitment, decided when the commander gets there and then again every
//! second until they pay:
//!
//! `worth = HORIZON x RAID_LOSS x E[denial]`, against the turrets' metal at
//! its price now (4x up to 4:00 falling to 1x at 7:00: metal early grows
//! into more), times the gain dial (`conquest_gain`, `always` = build).
//!
//! Every decision goes to the journal (`conquest` records).

use std::sync::mpsc;

use serde_json::json;
use springai::{UnitDef, Vec3};
use zk_terrain::conquest::{Objective, Params, objectives};
use zk_terrain::strategy::Asset;
use zk_terrain::{Pos, Tier, TierMap};

use super::{List, TaskId, WorkerTask};
use crate::Ai;
use crate::terrain::{pos, tier_of};
use crate::util::distance;

/// Frames between updates.
const PERIOD: i32 = 30;
/// Tiers objectives are valued on (ships keep to the water).
const TIERS: [Tier; 5] = [Tier::Bot, Tier::Vehicle, Tier::Hover, Tier::Spider, Tier::Amphib];
/// What an enemy fields before we see any of it: Zero-K's ground
/// factories by tier.
const PRIOR: [(Tier, f32); 5] =
    [(Tier::Bot, 3.0), (Tier::Vehicle, 2.0), (Tier::Hover, 1.0), (Tier::Spider, 1.0), (Tier::Amphib, 1.0)];
/// A seen factory counts as this much metal of its tier's units.
const FACTORY_EVIDENCE: f32 = 1500.0;
/// The commander sets out once we hold this many mexes (its first ones)...
const OPENING_MEXES: usize = 3;
/// ... or from this frame on.
const OPENING_LATEST: i32 = 30 * 100;
/// Objectives farther than this from our start (walking) are left alone.
const MAX_WALK: f32 = 4500.0;
/// Walk lead (enemy's walk minus ours, elmos) worth doubling an
/// objective's pick weight: ground we reach first is ours to take.
const RACE_SCALE: f32 = 2000.0;
/// Job-cost bonus of the objective's jobs for the commander, and of its
/// turrets for builders near them.
const BONUS: f32 = 2500.0;
const HELP_BONUS: f32 = 900.0;
const HELP_RADIUS: f32 = 900.0;
/// The turrets are decided when the commander is this near the objective,
/// or this long after it set out.
const ARRIVE: f32 = 900.0;
const WALK_TIMEOUT: i32 = 30 * 150;
/// Undecided objectives are given up at this game time.
const WATCH_UNTIL: i32 = 30 * 60 * 12;
/// Seconds the denial counts for, and the share of income at stake that
/// raids take on contested ground over it.
const HORIZON: f32 = 600.0;
const RAID_LOSS: f32 = 0.35;
/// Updates in a row with the enemy holding the objective before the
/// commander is called back.
const DANGER_TICKS: u32 = 3;
/// The commander is called back under this share of its health...
const CREW_HEALTH: f32 = 0.5;
/// ... and after this long fortifying (the turrets' jobs stay for others).
const FORTIFY_TIMEOUT: i32 = 30 * 240;
/// Times a turret's job is put back after it vanished unbuilt.
const MAX_RETRIES: u8 = 2;
/// A built turret of ours this near its site is the site's.
const BUILT_RADIUS: f32 = 150.0;
const DEFAULT_TURRET: &str = "turretheavylaser";

#[derive(Default)]
pub struct Conquest {
    job: Option<mpsc::Receiver<Vec<Objective>>>,
    asked: bool,
    pub objectives: Vec<Objective>,
    pub assets: Vec<Asset>,
    /// Metal of what we have seen of each tier, per enemy team.
    pub seen: Vec<(i32, Vec<(Tier, f32)>)>,
    pub op: Option<Op>,
    pub done: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stage {
    /// The commander goes there and takes the spots.
    Walk,
    /// Arrived, the turrets did not pay yet: asked again every update.
    Watch,
    /// Turrets under way.
    Fortify,
}

impl Stage {
    fn name(self) -> &'static str {
        match self {
            Stage::Walk => "walk",
            Stage::Watch => "watch",
            Stage::Fortify => "fortify",
        }
    }
}

pub struct Op {
    pub idx: usize,
    pub crew: i32,
    pub stage: Stage,
    /// Jobs of the objective's spots not yet ours.
    pub mex_tasks: Vec<TaskId>,
    pub turrets: Vec<Turret>,
    pub started: i32,
    /// When the turrets were committed.
    pub fortify_from: i32,
    danger: u32,
}

pub struct Turret {
    pub site: Vec3,
    pub task: Option<TaskId>,
    pub retries: u8,
    pub built: bool,
}

/// Metal now against metal later: 4x until 4:00, 1x from 7:00.
fn metal_price(frame: i32) -> f32 {
    let minutes = frame as f32 / 1800.0;
    1.0 + 3.0 * ((7.0 - minutes) / 3.0).clamp(0.0, 1.0)
}

fn prior() -> Vec<(Tier, f32)> {
    let total: f32 = PRIOR.iter().map(|p| p.1).sum();
    PRIOR.iter().map(|&(t, w)| (t, w / total)).collect()
}

impl Ai {
    pub(super) fn eco_conquest_update(&mut self, frame: i32) {
        if frame % PERIOD != (self.offset + 7) % PERIOD {
            return;
        }
        if self.dev_off("conquest") {
            if self.eco.conquest.op.is_some() {
                self.conquest_end("switched off", false);
            }
            return;
        }
        self.conquest_plan(frame);
        self.conquest_observe();
        if self.eco.conquest.op.is_some() {
            self.conquest_step(frame);
        } else if !self.eco.conquest.done {
            self.conquest_start(frame);
        }
    }

    fn conquest_turret(&self) -> Option<UnitDef> {
        let name = std::env::var("ZKGBAI_CONQUEST_TURRET")
            .ok()
            .or_else(|| self.cb.option("conquest_turret"))
            .filter(|v| !v.trim().is_empty())
            .unwrap_or_else(|| DEFAULT_TURRET.to_owned());
        self.defs.get(name.trim()).or_else(|| self.defs.get(DEFAULT_TURRET))
    }

    /// The gain dial: the turrets go up when worth / priced cost reaches it
    /// (`always`: 0).
    fn conquest_gain(&self) -> f32 {
        let v = self
            .cb
            .option("conquest_gain")
            .filter(|v| !v.trim().is_empty())
            .or_else(|| std::env::var("ZKGBAI_CONQUEST_GAIN").ok())
            .unwrap_or_default();
        let v = v.trim().to_ascii_lowercase();
        if v == "always" { 0.0 } else { v.parse::<f32>().ok().filter(|g| *g >= 0.0).unwrap_or(1.0) }
    }

    /// Start the objectives' search once the terrain and both bases are
    /// known; collect its result.
    fn conquest_plan(&mut self, frame: i32) {
        if let Some(rx) = &self.eco.conquest.job
            && let Ok(found) = rx.try_recv()
        {
            self.eco.conquest.job = None;
            self.eco.conquest.objectives = found;
            self.conquest_report_atlas();
        }
        if self.eco.conquest.asked || frame < 90 {
            return;
        }
        let Some(terrain) = self.terrain.get() else { return };
        let Some((ally, enemy)) = self.strategy_bases() else { return };
        let maps: Vec<TierMap> = TIERS.iter().filter_map(|&t| terrain.tier(t).cloned()).collect();
        let Some(walk) = terrain.tier(Tier::Bot).cloned() else { return };
        let assets = self.strategy_assets();
        let range = self.conquest_turret().map(|d| d.max_weapon_range()).filter(|r| *r > 100.0).unwrap_or(620.0);
        let params = Params { range, ..Params::default() };
        let (tx, rx) = mpsc::channel();
        let (a, e, spots) = (pos(ally), pos(enemy), assets.clone());
        let spawned = std::thread::Builder::new().name("zkgbai-conquest".into()).spawn(move || {
            let refs: Vec<&TierMap> = maps.iter().collect();
            let _ = tx.send(objectives(&refs, &walk, a, &[e], &spots, &params));
        });
        self.eco.conquest.asked = true;
        match spawned {
            Ok(_) => {
                self.eco.conquest.job = Some(rx);
                self.eco.conquest.assets = assets;
                self.log(&format!(
                    "conquest: looking for objectives from ({:.0},{:.0}) against ({:.0},{:.0}), reach {range:.0}",
                    ally.x, ally.z, enemy.x, enemy.z
                ));
            }
            Err(e) => self.log(&format!("conquest: cannot start the search ({e})")),
        }
    }

    fn conquest_report_atlas(&mut self) {
        let c = &self.eco.conquest;
        let mut lines = Vec::new();
        let list: Vec<serde_json::Value> = c
            .objectives
            .iter()
            .enumerate()
            .map(|(i, o)| {
                let tiers: Vec<String> =
                    o.denial.iter().map(|d| format!("{} {:.1} ({}/{})", d.tier.name(), d.worth, d.sealed, o.spots.len())).collect();
                lines.push(format!(
                    "conquest: objective {i} ({} design) sites {} spots {} income {:.1} walk {:.0} (enemy {:.0}); denial {}",
                    o.design.name(),
                    o.sites.iter().map(|s| format!("({:.0},{:.0})", s.x, s.z)).collect::<Vec<_>>().join(" "),
                    o.spots.len(),
                    o.income,
                    o.walk,
                    o.enemy_walk,
                    tiers.join(", ")
                ));
                json!({
                    "design": o.design.name(), "income": o.income, "walk": o.walk.round(), "enemy_walk": o.enemy_walk.round(),
                    "sites": o.sites.iter().map(|s| [s.x.round(), s.z.round()]).collect::<Vec<_>>(),
                    "spots": o.spots.iter().filter_map(|&k| c.assets.get(k)).map(|a| [a.pos.x.round(), a.pos.z.round(), a.value]).collect::<Vec<_>>(),
                    "denial": o.denial.iter().map(|d| json!({"tier": d.tier.name(), "worth": d.worth, "own": d.own, "sealed": d.sealed})).collect::<Vec<_>>(),
                })
            })
            .collect();
        if lines.is_empty() {
            lines.push("conquest: no objectives on this map".into());
        }
        for l in &lines {
            self.log(l);
        }
        self.op_emit("conquest", json!({"event": "atlas", "t": self.frame / 30, "objectives": list}));
    }

    /// What we have seen of each enemy team's tiers: the metal of its units
    /// we know of now, a factory counting as `FACTORY_EVIDENCE`.
    fn conquest_observe(&mut self) {
        let teams: Vec<i32> = self.get_enemy_team_ids().iter().copied().collect();
        let mut seen: Vec<(i32, Vec<(Tier, f32)>)> = teams.iter().map(|&t| (t, Vec::new())).collect();
        for e in self.war.targets.values().filter(|e| !e.is_dead) {
            let Some(ud) = e.ud else { continue };
            let team = e.unit.team();
            let Some((_, tiers)) = seen.iter_mut().find(|(t, _)| *t == team) else { continue };
            let (tier, w) = if e.is_static {
                match super::planning::factory_tier(ud.name().as_str()) {
                    Some(t) => (t, FACTORY_EVIDENCE),
                    None => continue,
                }
            } else {
                match tier_of(ud) {
                    Some(t) => (t, ud.cost(self.metal).max(1.0)),
                    None => continue,
                }
            };
            match tiers.iter_mut().find(|(t, _)| *t == tier) {
                Some((_, c)) => *c += w,
                None => tiers.push((tier, w)),
            }
        }
        self.eco.conquest.seen = seen;
    }

    /// Each enemy team's tier as a distribution: what we saw, or the prior.
    fn conquest_belief(&self) -> Vec<(i32, Vec<(Tier, f32)>)> {
        let c = &self.eco.conquest;
        if c.seen.is_empty() {
            return vec![(-1, prior())];
        }
        c.seen
            .iter()
            .map(|(team, tiers)| {
                let ground: Vec<(Tier, f32)> = tiers.iter().copied().filter(|(t, _)| TIERS.contains(t)).collect();
                let total: f32 = ground.iter().map(|p| p.1).sum();
                if total > 0.0 {
                    (*team, ground.iter().map(|&(t, w)| (t, w / total)).collect())
                } else {
                    (*team, prior())
                }
            })
            .collect()
    }

    /// Denial an objective is expected to give: per enemy team the tiers'
    /// numbers weighted by the belief, averaged over the teams (the
    /// objectives are found against one enemy origin: our base's foe).
    fn conquest_expected(&self, o: &Objective) -> f32 {
        let belief = self.conquest_belief();
        let n = belief.len().max(1) as f32;
        belief.iter().map(|(_, b)| b.iter().map(|&(t, p)| p * o.denial_for(t, 0)).sum::<f32>()).sum::<f32>() / n
    }

    /// (worth, priced cost, ratio) of an objective's turrets now.
    fn conquest_value(&self, o: &Objective) -> (f32, f32, f32) {
        let worth = HORIZON * RAID_LOSS * self.conquest_expected(o);
        let each = self.conquest_turret().map_or(450.0, |d| d.cost(self.metal));
        let cost = each * o.sites.len() as f32 * metal_price(self.frame);
        (worth, cost, if cost > 0.0 { worth / cost } else { 0.0 })
    }

    fn conquest_spot_hostile(&self, p: Pos) -> bool {
        self.graph
            .metal_spots
            .iter()
            .any(|s| s.hostile && distance(s.position, Vec3::new(p.x, s.position.y, p.z)) < 50.0)
    }

    fn conquest_spot_owned(&self, p: Pos) -> bool {
        self.graph.metal_spots.iter().any(|s| s.owned && distance(s.position, Vec3::new(p.x, s.position.y, p.z)) < 50.0)
    }

    fn conquest_v3(&self, p: Pos) -> Vec3 {
        Vec3::new(p.x, self.cb.map().elevation_at(p.x, p.z), p.z)
    }

    fn conquest_start(&mut self, frame: i32) {
        let c = &self.eco.conquest;
        if c.objectives.is_empty() {
            return;
        }
        if self.eco.mexes.len() < OPENING_MEXES && frame < OPENING_LATEST {
            return;
        }
        let Some(crew) = self.eco.commanders.iter().copied().find(|id| {
            self.eco
                .workers
                .get(id)
                .is_some_and(|w| !w.has_plop() && w.unit.health() > 0.0 && w.unit.team() == self.team_id)
        }) else {
            return;
        };
        let best = (0..c.objectives.len())
            .filter(|&i| {
                let o = &c.objectives[i];
                o.walk <= MAX_WALK
                    && !o.spots.iter().filter_map(|&k| c.assets.get(k)).all(|a| self.conquest_spot_hostile(a.pos))
                    && o.sites.iter().all(|&s| self.war.get_porc_threat(self.conquest_v3(s)) <= 0.0)
            })
            .map(|i| (i, self.conquest_expected(&c.objectives[i])))
            .filter(|(_, v)| *v > 0.0)
            .max_by(|a, b| {
                let race = |i: usize| {
                    let o = &c.objectives[i];
                    (1.0 + (o.enemy_walk - o.walk) / RACE_SCALE).clamp(0.3, 1.5)
                };
                (a.1 * race(a.0)).total_cmp(&(b.1 * race(b.0)))
            });
        let Some((idx, expected)) = best else {
            self.eco.conquest.done = true;
            self.log("conquest: no objective is open");
            self.op_emit("conquest", json!({"event": "none", "t": frame / 30}));
            return;
        };
        let sites: Vec<Vec3> = self.eco.conquest.objectives[idx].sites.iter().map(|&s| self.conquest_v3(s)).collect();
        self.eco.conquest.op = Some(Op {
            idx,
            crew,
            stage: Stage::Walk,
            mex_tasks: Vec::new(),
            turrets: sites.into_iter().map(|site| Turret { site, task: None, retries: 0, built: false }).collect(),
            started: frame,
            fortify_from: 0,
            danger: 0,
        });
        self.conquest_refresh_mexes();
        let o = &self.eco.conquest.objectives[idx];
        let (worth, cost, ratio) = self.conquest_value(o);
        let center = o.center();
        self.log(&format!(
            "conquest: commander {crew} sets out for objective {idx} at ({:.0},{:.0}): {} spots, expected denial {expected:.1}/s, turrets now {ratio:.2} (worth {worth:.0} / cost {cost:.0})",
            center.x,
            center.z,
            o.spots.len()
        ));
        let belief = self.conquest_belief_json();
        self.op_emit(
            "conquest",
            json!({"event": "go", "t": frame / 30, "objective": idx, "crew": crew, "expected": expected,
                   "worth": worth, "cost": cost, "ratio": ratio, "belief": belief, "mexes": self.eco.mexes.len()}),
        );
    }

    fn conquest_belief_json(&self) -> serde_json::Value {
        json!(
            self.conquest_belief()
                .iter()
                .map(|(team, b)| json!({"team": team, "p": b.iter().map(|(t, p)| (t.name(), (p * 100.0).round() / 100.0)).collect::<std::collections::BTreeMap<_, _>>()}))
                .collect::<Vec<_>>()
        )
    }

    /// The jobs of the objective's spots not yet ours (the economy makes
    /// them for every neutral spot; see `eco_capture_mexes`).
    fn conquest_refresh_mexes(&mut self) {
        let Some(op) = &self.eco.conquest.op else { return };
        let mex = self.eco.build_ids.mex_id;
        let spots: Vec<Pos> = self.eco.conquest.objectives[op.idx]
            .spots
            .iter()
            .filter_map(|&k| self.eco.conquest.assets.get(k))
            .map(|a| a.pos)
            .collect();
        let tasks: Vec<TaskId> = spots
            .iter()
            .filter(|&&p| !self.conquest_spot_owned(p))
            .filter_map(|&p| {
                self.eco.construction_tasks.iter().copied().find(|&id| {
                    self.eco.tasks.get(id).is_some_and(|t| {
                        t.as_construction().is_some_and(|c| c.build_type.id() == mex)
                            && distance(t.position, Vec3::new(p.x, t.position.y, p.z)) < 50.0
                    })
                })
            })
            .collect();
        if let Some(op) = &mut self.eco.conquest.op {
            op.mex_tasks = tasks;
        }
    }

    /// Job-cost bonus of task `task` for worker `wid` (see
    /// `eco_cost_of_job_alone`): the objective's jobs for its commander,
    /// its turrets for builders near them.
    pub(crate) fn conquest_bonus(&self, wid: i32, task: TaskId) -> Option<f32> {
        let op = self.eco.conquest.op.as_ref()?;
        if op.mex_tasks.contains(&task) {
            return (wid == op.crew).then_some(BONUS);
        }
        let t = op.turrets.iter().find(|t| t.task == Some(task))?;
        if wid == op.crew {
            return Some(BONUS);
        }
        let w = self.eco.workers.get(&wid)?;
        (distance(w.get_pos(), t.site) <= HELP_RADIUS).then_some(HELP_BONUS)
    }

    fn conquest_step(&mut self, frame: i32) {
        let Some(op) = &self.eco.conquest.op else { return };
        let (idx, crew, stage, started) = (op.idx, op.crew, op.stage, op.started);
        if !self.eco.workers.contains_key(&crew) {
            self.conquest_end("commander lost", false);
            return;
        }
        let o = self.eco.conquest.objectives[idx].clone();
        let center = self.conquest_v3(o.center());
        let spots: Vec<Pos> = o.spots.iter().filter_map(|&k| self.eco.conquest.assets.get(k)).map(|a| a.pos).collect();
        if spots.iter().all(|&p| self.conquest_spot_hostile(p)) {
            self.conquest_end("the enemy took the spots", false);
            return;
        }
        if o.sites.iter().any(|&s| self.war.get_porc_threat(self.conquest_v3(s)) > 0.0) {
            self.conquest_end("enemy defenses on a site", false);
            return;
        }
        let held = self.war.get_effective_threat(center) > 1.5 * self.war.get_total_friendly_threat(center).max(0.1);
        let danger = if held { op.danger + 1 } else { 0 };
        if let Some(op) = &mut self.eco.conquest.op {
            op.danger = danger;
        }
        if stage != Stage::Watch && danger >= DANGER_TICKS {
            self.conquest_end("the enemy holds the objective", false);
            return;
        }
        let hurt =
            self.eco.workers.get(&crew).is_some_and(|w| w.unit.health() < CREW_HEALTH * w.unit.max_health().max(1.0));
        if hurt && stage != Stage::Watch {
            self.conquest_end("commander hurt", false);
            return;
        }
        self.conquest_refresh_mexes();
        match stage {
            Stage::Walk => {
                let near = self.eco.workers.get(&crew).is_some_and(|w| distance(w.get_pos(), center) <= ARRIVE);
                if near || frame - started > WALK_TIMEOUT {
                    self.conquest_decide(frame, if near { "arrived" } else { "walk timed out" });
                }
            }
            Stage::Watch => {
                if frame > WATCH_UNTIL {
                    self.conquest_end("turrets never paid", true);
                } else {
                    self.conquest_decide(frame, "watch");
                }
            }
            Stage::Fortify => self.conquest_fortify(frame),
        }
    }

    /// Whether the turrets pay now: build them, or watch.
    fn conquest_decide(&mut self, frame: i32, why: &str) {
        let Some(op) = &self.eco.conquest.op else { return };
        let (idx, stage) = (op.idx, op.stage);
        let o = &self.eco.conquest.objectives[idx];
        let (worth, cost, ratio) = self.conquest_value(o);
        let gain = self.conquest_gain();
        let go = ratio >= gain;
        // Watching, record only the change (and the first look).
        if stage != Stage::Watch || go {
            let expected = self.conquest_expected(o);
            self.log(&format!(
                "conquest: {why}: turrets {} at ratio {ratio:.2} (worth {worth:.0}, cost {cost:.0} at x{:.1}, gain {gain:.2}, expected denial {expected:.1}/s)",
                if go { "go up" } else { "wait" },
                metal_price(frame)
            ));
            let belief = self.conquest_belief_json();
            self.op_emit(
                "conquest",
                json!({"event": if go { "commit" } else { "wait" }, "t": frame / 30, "why": why, "worth": worth, "cost": cost,
                       "ratio": ratio, "gain": gain, "price": metal_price(frame), "expected": expected, "belief": belief}),
            );
        }
        if let Some(op) = &mut self.eco.conquest.op {
            op.stage = if go { Stage::Fortify } else { Stage::Watch };
            if go {
                op.fortify_from = frame;
            }
        }
        if go {
            self.conquest_fortify(frame);
        }
    }

    fn conquest_fortify(&mut self, frame: i32) {
        let Some(def) = self.conquest_turret() else {
            self.conquest_end("no turret", false);
            return;
        };
        let n = self.eco.conquest.op.as_ref().map_or(0, |op| op.turrets.len());
        for i in 0..n {
            let Some(t) = self
                .eco
                .conquest
                .op
                .as_ref()
                .map(|op| (op.turrets[i].site, op.turrets[i].task, op.turrets[i].retries, op.turrets[i].built))
            else {
                return;
            };
            let (site, task, retries, built) = t;
            if built {
                continue;
            }
            let standing = self.eco.porcs.iter().any(|u| {
                distance(u.pos(), site) <= BUILT_RADIUS
                    && u.def().is_some_and(|d| d.id() == def.id())
                    && !u.is_being_built()
            });
            if standing {
                if let Some(op) = &mut self.eco.conquest.op {
                    op.turrets[i].built = true;
                    op.turrets[i].task = None;
                }
                continue;
            }
            if task.is_some_and(|id| self.eco.tasks.get(id).is_some()) {
                continue;
            }
            if task.is_some() && retries >= MAX_RETRIES {
                continue;
            }
            let at = self.cb.map().find_closest_build_site(def, site, 250.0, 3, 0);
            let id = (at.x >= 0.0)
                .then(|| {
                    self.eco_add_construction(WorkerTask::construction(def, at, 0), Some(List::Porc), Some(List::Porc))
                })
                .flatten();
            if let Some(op) = &mut self.eco.conquest.op {
                op.turrets[i].task = id;
                if task.is_some() || id.is_none() {
                    op.turrets[i].retries += 1;
                }
            }
            if let Some(id) = id {
                self.log(&format!("conquest: {} ordered at ({:.0},{:.0}) (task {id})", def.name(), at.x, at.z));
            }
        }
        let (all_built, stuck) = self.eco.conquest.op.as_ref().map_or((false, true), |op| {
            (
                op.turrets.iter().all(|t| t.built),
                op.turrets.iter().all(|t| t.built || (t.task.is_none() && t.retries >= MAX_RETRIES)),
            )
        });
        let since = self.eco.conquest.op.as_ref().map_or(0, |op| frame - op.fortify_from);
        if all_built {
            self.conquest_end("held", true);
        } else if stuck && frame > 0 {
            self.conquest_end("turret sites lost", false);
        } else if since > FORTIFY_TIMEOUT {
            self.conquest_end("fortify timed out", false);
        }
    }

    /// Release the commander; drop turret jobs not yet started.
    fn conquest_end(&mut self, why: &str, success: bool) {
        let Some(op) = self.eco.conquest.op.take() else { return };
        self.eco.conquest.done = true;
        let mut dropped = 0;
        for t in &op.turrets {
            if let Some(id) = t.task
                && self.eco.tasks.get(id).is_some_and(|task| task.as_construction().is_some_and(|c| c.target.is_none()))
            {
                self.eco.drop_construction_task(id);
                dropped += 1;
            }
        }
        let built = op.turrets.iter().filter(|t| t.built).count();
        self.log(&format!(
            "conquest: objective {} ends ({why}) after {} s in {}: {built} turrets standing, {dropped} unstarted dropped",
            op.idx,
            (self.frame - op.started) / 30,
            op.stage.name()
        ));
        self.op_emit(
            "conquest",
            json!({"event": "end", "t": self.frame / 30, "objective": op.idx, "why": why, "success": success,
                   "stage": op.stage.name(), "built": built, "dropped": dropped, "secs": (self.frame - op.started) / 30}),
        );
    }

    /// Operator / journal view.
    pub(crate) fn conquest_json(&self) -> serde_json::Value {
        let c = &self.eco.conquest;
        json!({
            "objectives": c.objectives.len(),
            "done": c.done,
            "belief": self.conquest_belief_json(),
            "op": c.op.as_ref().map(|op| json!({
                "objective": op.idx, "crew": op.crew, "stage": op.stage.name(), "since": op.started / 30,
                "mex_jobs": op.mex_tasks.len(),
                "turrets": op.turrets.iter().map(|t| json!({"x": t.site.x.round(), "z": t.site.z.round(), "task": t.task, "built": t.built})).collect::<Vec<_>>(),
            })),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metal_is_dear_early_and_at_par_from_seven_minutes() {
        assert_eq!(metal_price(0), 4.0);
        assert_eq!(metal_price(1800 * 4), 4.0);
        assert!((metal_price(1800 * 5) - 3.0).abs() < 1e-4);
        assert_eq!(metal_price(1800 * 7), 1.0);
        assert_eq!(metal_price(1800 * 20), 1.0);
    }

    #[test]
    fn the_prior_is_a_distribution_over_ground_tiers() {
        let p = prior();
        assert!((p.iter().map(|x| x.1).sum::<f32>() - 1.0).abs() < 1e-5);
        assert!(p.iter().all(|(t, _)| TIERS.contains(t)));
    }
}
