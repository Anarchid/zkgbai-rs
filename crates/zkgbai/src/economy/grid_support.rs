//! Persistent priority and protected placement for useful grid connections.
use std::collections::BTreeMap;

use springai::{Unit, UnitDef, Vec3};

use super::{List, TaskId};
use crate::{
    Ai,
    util::{NULLPOS, distance},
};

const SUPPORT_LIMIT: usize = 6;
const LINK_MARGIN: f32 = 0.95;

#[derive(Clone)]
pub(super) struct PowerNeed {
    pub pos: Vec3,
    pub value: f32,
    pub threat: f32,
    pub what: String,
}

#[derive(Clone)]
pub(super) struct Connection {
    pub assets: Vec<Vec3>,
    pub overdrive: Option<(Vec3, Vec3)>,
    pub value: f32,
    pub threat: f32,
    pub rate: f32,
    pub why: String,
}

#[derive(Default)]
pub(crate) struct Support {
    pub(super) needs: Vec<PowerNeed>,
    pub(super) jobs: BTreeMap<TaskId, Connection>,
    burials: Vec<Burial>,
}

struct Burial {
    at: Vec3,
    corners: [Vec3; 4],
    level: f32,
    worker: Option<Unit>,
    issued: i32,
    retry: i32,
    done: bool,
}

/// A live piece's capital remains useful indefinitely; nearby hostile value
/// raises the price of leaving it switched off, without an unbounded threat bid.
fn restoration_value(piece: f32, threat: f32) -> f32 {
    piece + threat.min(4.0 * piece) * 0.5
}

/// Validate the actual snapped site, not just its seed. The last relay must
/// reach both ends; earlier steps must make substantial progress from power.
fn link_step(site: Vec3, goal: Vec3, gr: f32, source: Vec3, sr: f32, pr: f32) -> bool {
    let reach = LINK_MARGIN * (sr + pr);
    let last = LINK_MARGIN * (gr + pr);
    let gap = distance(goal, source);
    distance(site, source) <= reach
        && if gap <= reach + last { distance(site, goal) <= last } else { distance(site, goal) <= gap - reach * 0.5 }
}

fn link_seeds(goal: Vec3, gr: f32, source: Vec3, sr: f32, pr: f32) -> Vec<Vec3> {
    let mut out = Vec::new();
    for (at, range) in [(goal, gr + pr), (source, sr + pr)] {
        out.push(at);
        for fraction in [0.25, 0.5, 0.75, 1.0] {
            for i in 0..64 {
                let angle = std::f32::consts::TAU * i as f32 / 64.0;
                let radius = range * LINK_MARGIN * fraction;
                let site = Vec3::xz(at.x + angle.cos() * radius, at.z + angle.sin() * radius);
                if link_step(site, goal, gr, source, sr, pr) {
                    out.push(site);
                }
            }
        }
    }
    out
}

/// Absolute level: retries finish the same hole instead of digging it deeper.
fn burial_level(ground: f32, height: f32) -> Option<f32> {
    if !ground.is_finite() || !height.is_finite() || height <= 0.0 {
        return None;
    }
    let level = (ground - height).max(4.0);
    (ground - level >= 8.0).then_some(level)
}

impl Ai {
    pub(super) fn eco_power_need(&self, def: UnitDef, at: Vec3) -> PowerNeed {
        let reach = def.max_weapon_range();
        let threat = self
            .war
            .targets
            .values()
            .filter(|e| {
                !e.is_dead && e.power > 0.0 && (e.visible || e.is_radar_visible || self.frame - e.last_seen <= 30 * 30)
            })
            .filter(|e| distance(at, e.position) <= reach + 600.0)
            .map(|e| e.value.max(0.0))
            .sum::<f32>();
        PowerNeed {
            pos: at,
            value: restoration_value(def.cost(self.metal), threat),
            threat,
            what: format!("{} at ({:.0}, {:.0})", self.defs.name(def), at.x, at.z),
        }
    }

    pub(super) fn eco_refresh_grid_support(&mut self, needs: Vec<PowerNeed>, powered: &[(Vec3, f32)]) {
        self.eco.grid_support.needs = needs;
        let pr = self.eco.grid_defs.pylon_range;
        let support = &mut self.eco.grid_support;
        support.jobs.retain(|id, job| {
            let Some(task) = self.eco.tasks.get(*id) else { return false };
            if !powered.iter().any(|&(at, range)| distance(at, task.position) < range + pr) {
                return false;
            }
            if job.overdrive.is_some() {
                return true;
            }
            let needs: Vec<_> =
                support.needs.iter().filter(|n| job.assets.iter().any(|&a| distance(a, n.pos) < 16.0)).collect();
            job.value = needs.iter().map(|n| n.value).sum();
            job.threat = needs.iter().map(|n| n.threat).sum();
            job.rate = job.value / if job.threat > 0.0 { 60.0 } else { 600.0 };
            !needs.is_empty()
        });
    }

    pub(crate) fn eco_grid_issue(&self, task: TaskId) -> Option<(f32, f32, f32, String)> {
        let j = self.eco.grid_support.jobs.get(&task)?;
        Some((j.value, j.rate, j.threat, j.why.clone()))
    }

    pub(super) fn eco_power_connection(&self, assets: Vec<Vec3>) -> Connection {
        let needs: Vec<_> =
            self.eco.grid_support.needs.iter().filter(|n| assets.iter().any(|&a| distance(a, n.pos) < 16.0)).collect();
        let value: f32 = needs.iter().map(|n| n.value).sum();
        let threat: f32 = needs.iter().map(|n| n.threat).sum();
        Connection {
            assets,
            overdrive: None,
            value,
            threat,
            rate: value / if threat > 0.0 { 60.0 } else { 600.0 },
            why: format!("restore power to {}", needs.iter().map(|n| n.what.as_str()).collect::<Vec<_>>().join(", ")),
        }
    }

    /// One step from the powered grid. Ordinary overdrive tasks cannot fill
    /// its reserved slots; a suitable existing task is promoted instead.
    pub(super) fn eco_reconnect_power(&mut self, goal: Vec3, gr: f32, source: Vec3, sr: f32, job: Connection) {
        let Some(pylon) = self.defs.get("energypylon") else { return };
        let pr = self.eco.grid_defs.pylon_range;
        if self.eco.grid_support.jobs.values().any(|old| match (old.overdrive, job.overdrive) {
            (Some((a, b)), Some((c, d))) => distance(a, c) < 16.0 && distance(b, d) < 16.0,
            (None, None) => old.assets.iter().any(|a| job.assets.iter().any(|b| distance(*a, *b) < 16.0)),
            _ => false,
        }) {
            return;
        }
        if let Some(id) = self
            .eco
            .pylon_tasks
            .iter()
            .copied()
            .find(|&id| self.eco.tasks.get(id).is_some_and(|t| link_step(t.position, goal, gr, source, sr, pr)))
        {
            let at = self.eco.task_pos(id);
            self.eco
                .grid_support
                .jobs
                .entry(id)
                .and_modify(|old| {
                    if job.value > old.value {
                        *old = job.clone();
                    }
                })
                .or_insert(job);
            self.eco_plan_pylon_burial(pylon, at);
            return;
        }
        let replace = if self.eco.grid_support.jobs.len() >= SUPPORT_LIMIT {
            let replace = self
                .eco
                .grid_support
                .jobs
                .iter()
                .filter(|(id, old)| {
                    old.value < job.value
                        && self
                            .eco
                            .tasks
                            .get(**id)
                            .and_then(|t| t.as_construction())
                            .is_some_and(|c| c.target.is_none())
                })
                .min_by(|a, b| a.1.value.total_cmp(&b.1.value))
                .map(|(&id, _)| id);
            let Some(id) = replace else { return };
            Some(id)
        } else {
            None
        };
        let enemies: Vec<_> = self
            .war
            .targets
            .values()
            .filter(|e| {
                !e.is_dead
                    && e.power > 0.0
                    && (e.visible || e.is_radar_visible || (e.is_static && self.frame - e.last_seen < 30 * 60))
            })
            .map(|e| e.position)
            .collect();
        let enemy = self.graph.get_enemy_base().unwrap_or(self.graph.get_enemy_center());
        let safety = |at: Vec3| {
            if !enemies.is_empty() {
                enemies.iter().map(|&e| distance(at, e)).fold(f32::INFINITY, f32::min)
            } else if enemy != NULLPOS {
                distance(at, enemy)
            } else {
                -distance(at, self.graph.get_ally_center())
            }
        };
        let mut seeds = link_seeds(goal, gr, source, sr, pr);
        seeds.sort_by(|&a, &b| safety(b).total_cmp(&safety(a)));
        let builders: Vec<_> = self
            .eco
            .workers
            .values()
            .filter(|w| !w.is_chicken)
            .filter_map(|w| {
                let def = w.unit.def()?;
                def.build_options().iter().any(|d| d.id() == pylon.id()).then_some((
                    def,
                    w.get_pos(),
                    def.build_distance(),
                ))
            })
            .collect();
        let mut best: Option<(bool, f32, Vec3)> = None;
        let map = self.cb.map();
        for seed in seeds {
            let at = map.find_closest_build_site(pylon, seed, 48.0, 3, 0);
            if at.x < 0.0
                || !link_step(at, goal, gr, source, sr, pr)
                || !map.is_possible_to_build_at(pylon, at, 0)
                || !self.eco_build_check(pylon, at, 0)
                || self.war.get_porc_threat(at) > 0.0
                || self.war.get_effective_threat(at) > self.war.get_total_friendly_threat(at)
                || !builders.iter().any(|&(def, from, range)| {
                    self.terrain_reaches(def, from, at, range)
                        && self.war.view(Some(def)).get_effective_threat(at)
                            <= self.war.view(Some(def)).get_total_friendly_threat(at)
                })
            {
                continue;
            }
            let score = safety(at);
            let clear = self.eco_pylon_hole_clear(pylon, at);
            if best.is_none_or(|b| (clear && !b.0) || (clear == b.0 && score > b.1)) {
                best = Some((clear, score, at));
            }
        }
        let Some((_, _, at)) = best else { return };
        if let Some(id) = replace {
            self.eco.grid_support.jobs.remove(&id);
            self.eco.drop_construction_task(id);
        }
        let Some(id) = self.eco_add_simple(pylon, at, List::Pylon, Some(List::Pylon)) else { return };
        self.log(&format!("grid: priority pylon at ({:.0}, {:.0}), value {:.0}: {}", at.x, at.z, job.value, job.why));
        self.eco.grid_support.jobs.insert(id, job);
        self.eco_plan_pylon_burial(pylon, at);
    }

    /// Leave room for the crater's sloping sides. Never lower a neighbour's
    /// foundation or a factory's exit to hide the pylon.
    fn eco_pylon_hole_clear(&self, def: UnitDef, at: Vec3) -> bool {
        let margin = def.x_size().max(def.z_size()) as f32 * 4.0 + def.height() + 32.0;
        if self.fac.factories.values().any(|f| distance(f.get_pos(), at) < margin + 400.0) {
            return false;
        }
        if self.eco.construction_tasks.iter().filter_map(|&id| self.eco.tasks.get(id)).any(|t| {
            t.as_construction().is_some_and(|c| {
                !(c.build_type.id() == def.id() && distance(t.position, at) < 25.0)
                    && distance(t.position, at)
                        < margin
                            + if super::planning::keeps_exit(self.defs.name(c.build_type)) {
                                400.0
                            } else {
                                c.build_type.radius()
                            }
            })
        }) {
            return false;
        }
        !self.cb.friendly_units_in_cylinder(at, margin + 500.0).iter().any(|u| {
            u.health() > 0.0
                && u.def().is_some_and(|d| {
                    d.speed() <= 0.0
                        && !(d.id() == def.id() && distance(u.pos(), at) < 25.0)
                        && distance(u.pos(), at)
                            < margin + if super::planning::keeps_exit(self.defs.name(d)) { 400.0 } else { d.radius() }
                })
        })
    }

    fn eco_plan_pylon_burial(&mut self, def: UnitDef, at: Vec3) {
        if self.eco.grid_support.burials.iter().any(|b| distance(b.at, at) < 25.0) {
            return;
        }
        let (hx, hz) = (def.x_size() as f32 * 4.0 + 8.0, def.z_size() as f32 * 4.0 + 8.0);
        let corners = [(-hx, -hz), (hx, -hz), (hx, hz), (-hx, hz)].map(|(x, z)| Vec3::xz(at.x + x, at.z + z));
        let ground = corners
            .iter()
            .chain(std::iter::once(&at))
            .map(|p| self.cb.map().elevation_at(p.x, p.z))
            .fold(f32::INFINITY, f32::min);
        let Some(level) = burial_level(ground, def.height()) else { return };
        self.eco.grid_support.burials.push(Burial {
            at,
            corners,
            level,
            worker: None,
            issued: 0,
            retry: 0,
            done: false,
        });
    }

    /// Connect first, then lower the live pylon. Retry interrupted work to its
    /// original absolute level, and release builders promptly on danger/loss.
    pub(super) fn eco_bury_support_pylons(&mut self, frame: i32) {
        let Some(def) = self.defs.get("energypylon") else { return };
        let mut burials = std::mem::take(&mut self.eco.grid_support.burials);
        for b in &mut burials {
            if b.done {
                continue;
            }
            let unit = self.eco.pylons.iter().copied().find(|u| u.health() > 0.0 && distance(u.pos(), b.at) < 25.0);
            let ready = unit.is_some_and(|u| !u.is_being_built());
            let safe = ready
                && !self.eco.hard_estalled
                && !self.dev_off("grid")
                && self.eco_pylon_hole_clear(def, b.at)
                && self.war.get_effective_threat(b.at) <= self.war.get_total_friendly_threat(b.at);
            let remaining = b
                .corners
                .iter()
                .chain(std::iter::once(&b.at))
                .map(|p| self.cb.map().elevation_at(p.x, p.z) - b.level)
                .fold(0.0, f32::max);
            if remaining <= 4.0 {
                b.done = true;
            }
            if let Some(worker) = b.worker {
                let ours = worker.health() > 0.0 && worker.team() == self.team_id;
                let fighting =
                    self.eco.workers.get(&worker.id()).is_none_or(|w| w.is_chicken || w.raid_defense.is_some());
                let owned = ours
                    && !fighting
                    && !springai::reserved::is_reserved(self.cb, worker.id())
                    && !self.recovery_owns(worker.id());
                if b.done || !safe || !owned || frame - b.issued > 30 * 120 {
                    if owned && super::terraform::is_terraforming(worker) {
                        worker.stop(0);
                    }
                    self.eco_release_terraform(&[worker]);
                    b.worker = None;
                    b.retry = frame + 30 * 10;
                } else if super::terraform::is_terraforming(worker) || frame - b.issued < 60 {
                    self.eco_hold_terraform(&[worker], 60);
                    continue;
                } else {
                    self.eco_release_terraform(&[worker]);
                    b.worker = None;
                    b.retry = frame + 30 * 10;
                }
            }
            if b.done || !safe || frame < b.retry || self.eco.hard_estalled {
                continue;
            }
            let radius = def.x_size().max(def.z_size()) as f32 * 4.0 + 24.0;
            let worker = self
                .eco
                .workers
                .values()
                .filter(|w| {
                    !w.is_chicken
                        && !self.eco_worker_held(w.id)
                        && !springai::reserved::is_reserved(self.cb, w.id)
                        && !self.recovery_owns(w.id)
                        && self.logistics_assignment(w.id).is_none_or(|id| self.eco.tasks.get(id).is_none())
                        && !w.task.is_some_and(|id| self.op_priority(id) || self.conquest_bonus(w.id, id).is_some())
                        && !self.jump_in_progress(w.unit)
                })
                .filter(|w| {
                    w.unit.def().is_some_and(|d| {
                        w.unit.health() > 0.0
                            && w.unit.team() == self.team_id
                            && self.war.view(Some(d)).get_effective_threat(b.at)
                                <= self.war.view(Some(d)).get_total_friendly_threat(b.at)
                            && d.terraform_speed() > 0.0
                            && distance(w.get_pos(), b.at) > radius
                            && distance(w.get_pos(), b.at) <= d.build_distance()
                    })
                })
                .min_by(|a, c| distance(a.get_pos(), b.at).total_cmp(&distance(c.get_pos(), b.at)))
                .map(|w| w.id);
            let Some(wid) = worker else { continue };
            self.eco_suspend_assignment(wid, "grid terraform");
            let w = self.eco.workers.get_mut(&wid).unwrap();
            w.clear_task(&mut self.eco.tasks);
            let worker = w.unit;
            self.terraform_level(&[worker], &b.corners, b.level, false);
            b.worker = Some(worker);
            b.issued = frame;
        }
        self.eco.grid_support.burials = burials;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_existing_weapon_keeps_its_connection_priority_without_pending_construction() {
        let mut ai = crate::economy::test_ai_with(|_| {});
        let def = UnitDef::from_id(ai.cb, 1).unwrap();
        let at = Vec3::xz(1000.0, 1000.0);
        let pylon = Vec3::xz(800.0, 1000.0);
        let id = ai.eco.tasks.insert(super::super::WorkerTask::construction(def, pylon, 0));
        let ordinary = ai.eco.tasks.insert(super::super::WorkerTask::construction(def, Vec3::xz(5000.0, 5000.0), 0));
        let need = PowerNeed { pos: at, value: 4000.0, threat: 4000.0, what: "Desolator".into() };
        ai.eco.grid_defs.pylon_range = 500.0;
        let source = [(Vec3::xz(300.0, 1000.0), 500.0)];
        ai.eco_refresh_grid_support(vec![need.clone()], &source);
        let job = ai.eco_power_connection(vec![at]);
        ai.eco.grid_support.jobs.insert(id, job);
        assert!(ai.eco.pending_power.is_none());
        assert_eq!(ai.eco_grid_issue(id).unwrap().0, 4000.0);
        assert!(ai.eco_grid_issue(ordinary).is_none());
        ai.frame = 30 * 3600;
        ai.eco_refresh_grid_support(vec![need.clone()], &source);
        assert_eq!(ai.eco_grid_issue(id).unwrap().0, 4000.0);
        // Restored power retires the priority; another outage can open it again.
        ai.eco_refresh_grid_support(Vec::new(), &source);
        assert!(ai.eco_grid_issue(id).is_none());
        ai.eco_refresh_grid_support(vec![need], &source);
        let job = ai.eco_power_connection(vec![at]);
        ai.eco.grid_support.jobs.insert(id, job);
        assert!(ai.eco_grid_issue(id).is_some());
        // A destroyed source cannot leave a disconnected pylon with a stale bid.
        ai.eco_refresh_grid_support(ai.eco.grid_support.needs.clone(), &[]);
        assert!(ai.eco_grid_issue(id).is_none());
    }
    #[test]
    fn back_edge_of_a_one_pylon_bridge_still_reaches_both_ends() {
        let goal = Vec3::xz(1000.0, 1000.0);
        let source = Vec3::xz(100.0, 1000.0);
        let enemy = Vec3::xz(2000.0, 1000.0);
        let sites = link_seeds(goal, 100.0, source, 500.0, 500.0);
        let best = sites
            .into_iter()
            .filter(|&p| link_step(p, goal, 100.0, source, 500.0, 500.0))
            .max_by(|&a, &b| distance(a, enemy).total_cmp(&distance(b, enemy)))
            .unwrap();
        assert!(best.x < 500.0, "{best:?}");
        assert!(distance(best, goal) <= 570.0);
        assert!(distance(best, source) <= 950.0);
        assert!(!link_step(Vec3::xz(1700.0, 1000.0), goal, 100.0, source, 500.0, 500.0));
    }
    #[test]
    fn long_connections_must_make_progress_and_not_walk_backwards() {
        let goal = Vec3::xz(4000.0, 0.0);
        let source = Vec3::xz(0.0, 0.0);
        assert!(link_step(Vec3::xz(900.0, 0.0), goal, 100.0, source, 500.0, 500.0));
        assert!(!link_step(Vec3::xz(-900.0, 0.0), goal, 100.0, source, 500.0, 500.0));
        assert!(!link_step(Vec3::xz(1600.0, 0.0), goal, 100.0, source, 500.0, 500.0));
    }
    #[test]
    fn threatened_capital_is_more_valuable_and_does_not_expire() {
        assert_eq!(restoration_value(2000.0, 0.0), 2000.0);
        assert_eq!(restoration_value(2000.0, 4000.0), 4000.0);
        assert_eq!(restoration_value(2000.0, 100000.0), 6000.0);
    }
    #[test]
    fn burial_uses_model_height_without_drowning_the_pylon() {
        assert_eq!(burial_level(200.0, 80.0), Some(120.0));
        assert_eq!(burial_level(50.0, 80.0), Some(4.0));
        assert_eq!(burial_level(5.0, 80.0), None);
        assert_eq!(burial_level(200.0, 0.0), None);
    }
}

#[cfg(test)]
#[path = "grid_support_tests.rs"]
mod execution_tests;
