//! Compush and the conquest route (user, 2026-11-03: "a frequent
//! inefficiency: taking its first three mexes, the third is sometimes
//! significantly behind the second ... two experimental alleviations").
//!
//! `compush` (default on in wip) drops the early trip: the commander
//! expands toward the enemy, everything else as usual, and any objective
//! whose spots all become ours, by whoever took them and whenever, gets its
//! turrets as a trip's would: its guard Lotus at once, its turrets when they
//! pay (`conquest_value` against the gain dial) and while within our means,
//! one at a time, built by those near them (the commander first). A turret
//! up, the objective is held: attacks there add turrets, and its shield
//! counts as conquered ground. Toward the enemy: the commander's mex jobs
//! cost `FACE_COST` less per elmo a spot is nearer the enemy's start-box
//! metal than the commander stands (half an elmo walked per elmo forward),
//! on our side of the middle only (to `FACE_SHARE` of the way). The sweep of 2026-11-03 had the trip cost a
//! fifth of the home energy in the first five minutes, the commander away
//! 55% of the time against 26%.
//!
//! `conquestroute` (off by default; with `compush` off) plans the trip with
//! the start: in a one-on-one, the objectives are searched before the start
//! position goes out (the terrain takes ~0.1 s), the trip's objective is
//! picked as it would be, and the start is the spot of our box whose ground
//! walk through three mexes (`ROUTE_MEXES`, spots within `ROUTE_RADIUS` of
//! it) to the objective is shortest, the mexes in that walk's order. Until
//! the commander sets out, the next of them not yet ours is its job first.

use serde_json::json;
use springai::Vec3;
use zk_terrain::Pos;
use zk_terrain::paths::Paths;

use super::super::{List, TaskId, Worker, WorkerTask};
use super::{
    BANK_EMPTY, BONUS, BUILT_RADIUS, FORTIFY_TIMEOUT, GUARD_TURRET, HELP_BONUS, HELP_RADIUS, Held, MAX_RETRIES,
    MEX_FIRST, SITE_FORWARD, Turret,
};
use crate::Ai;
use crate::terrain::pos;
use crate::util::distance;

/// Commander mex jobs: cost off per elmo nearer the enemy (jobs cost about
/// a quarter per elmo walked: one elmo forward pays for half an elmo
/// walked; at one for one the first smoke walked 2500 elmos to the middle
/// of Mercurial past every spot on the way, building nothing for 80 s).
const FACE_COST: f32 = 0.125;
/// Toward the enemy only on our side: spots at most this share of the way.
const FACE_SHARE: f32 = 0.55;
/// A failed seizure (lost spots, no turret up in time) waits this long.
const SEIZE_COOLDOWN: i32 = 30 * 120;
/// The opening route: this many mexes, among the spots this near the start.
const ROUTE_MEXES: usize = 3;
const ROUTE_RADIUS: f32 = 1600.0;
/// A mex job this near a route spot is its.
const ROUTE_JOB: f32 = 100.0;
/// How long the start position waits for the terrain.
const TERRAIN_WAIT: std::time::Duration = std::time::Duration::from_secs(5);

/// An objective whose spots became ours (see the module notes).
#[derive(Clone, Debug)]
pub struct Seized {
    pub idx: usize,
    /// The turrets pay: ordered as our means allow.
    pub committed: bool,
    /// The guard Lotus (light), then a turret per site.
    pub turrets: Vec<Turret>,
    pub since: i32,
    pub fortify_from: i32,
}

/// The best opening walk: from one of `starts` (indices into `points`)
/// through `ROUTE_MEXES` distinct `points` within `ROUTE_RADIUS` of it to
/// `goal` (an index into `points`), by `dist` (ground, `None` when there is
/// no way). Returns (start, mexes in order, length).
pub(crate) fn best_route(
    points: &[Pos],
    starts: &[usize],
    goal: usize,
    dist: impl Fn(usize, usize) -> Option<f32>,
) -> Option<(usize, Vec<usize>, f32)> {
    let mut best: Option<(usize, Vec<usize>, f32)> = None;
    for &s in starts {
        let near: Vec<usize> =
            (0..points.len()).filter(|&k| k != goal && points[k].distance(points[s]) <= ROUTE_RADIUS).collect();
        if near.len() < ROUTE_MEXES {
            continue;
        }
        // Short lists: every ordered choice.
        let mut stack: Vec<(Vec<usize>, f32)> = vec![(Vec::new(), 0.0)];
        while let Some((path, len)) = stack.pop() {
            if best.as_ref().is_some_and(|b| len >= b.2) {
                continue;
            }
            let at = path.last().copied().unwrap_or(s);
            if path.len() == ROUTE_MEXES {
                let Some(last) = dist(at, goal) else { continue };
                let total = len + last;
                if best.as_ref().is_none_or(|b| total < b.2) {
                    best = Some((s, path, total));
                }
                continue;
            }
            for &k in near.iter().filter(|k| !path.contains(k)) {
                let Some(d) = (if k == at { Some(0.0) } else { dist(at, k) }) else { continue };
                let mut next = path.clone();
                next.push(k);
                stack.push((next, len + d));
            }
        }
    }
    best
}

/// A spot at `p`'s cost off for a commander at `wpos`: `FACE_COST` per elmo
/// it is nearer `enemy`, on our side (`FACE_SHARE` of the way from `home`).
fn face(wpos: Vec3, p: Vec3, home: Vec3, enemy: Vec3) -> f32 {
    let (dh, de) = (distance(p, home), distance(p, enemy));
    if dh > FACE_SHARE * (dh + de) {
        return 0.0;
    }
    FACE_COST * (distance(wpos, enemy) - de).max(0.0)
}

impl Ai {
    /// With `compush`, the commander's mex job at `p` costs this much less
    /// for being nearer the enemy than it stands (see the module notes).
    pub(crate) fn compush_face_bonus(&self, worker: &Worker, wpos: Vec3, p: Vec3) -> f32 {
        if !worker.is_com || self.dev_off("conquest") || self.dev_off("compush") {
            return 0.0;
        }
        let Some(enemy) = self.graph.get_enemy_base() else { return 0.0 };
        let home = self.graph.get_ally_base();
        if home.is_zero() {
            return 0.0;
        }
        face(wpos, p, home, enemy)
    }

    /// Objectives whose spots became ours are seized; the seized get their
    /// turrets, and end held or not (see the module notes).
    pub(super) fn conquest_seized_step(&mut self, frame: i32) {
        self.conquest_seize_new(frame);
        let mut i = 0;
        while i < self.eco.conquest.seized.len() {
            match self.conquest_seized_one(i, frame) {
                Some((why, success)) => self.conquest_seized_end(i, why, success, frame),
                None => i += 1,
            }
        }
    }

    fn conquest_objective_spots(&self, idx: usize) -> Vec<Pos> {
        let c = &self.eco.conquest;
        c.objectives
            .get(idx)
            .map(|o| o.spots.iter().filter_map(|&k| c.assets.get(k)).map(|a| a.pos).collect())
            .unwrap_or_default()
    }

    fn conquest_seize_new(&mut self, frame: i32) {
        self.eco.conquest.seize_cool.retain(|&(_, until)| until > frame);
        for idx in 0..self.eco.conquest.objectives.len() {
            let c = &self.eco.conquest;
            if c.taken.contains(&idx)
                || c.seized.iter().any(|s| s.idx == idx)
                || c.seize_cool.iter().any(|&(i, _)| i == idx)
            {
                continue;
            }
            let spots = self.conquest_objective_spots(idx);
            if spots.is_empty() || !spots.iter().all(|&p| self.conquest_spot_owned(p)) {
                continue;
            }
            let o = &self.eco.conquest.objectives[idx];
            if o.sites.iter().any(|&s| self.war.get_porc_threat(self.conquest_v3(s)) > 0.0) {
                continue;
            }
            let fresh = |site: Vec3, light: bool| Turret { light, site, task: None, retries: 0, built: false };
            let mut turrets: Vec<Turret> = self.conquest_guard_site(idx).map(|g| fresh(g, true)).into_iter().collect();
            turrets.extend(
                o.sites.iter().map(|&s| fresh(self.conquest_forward(self.conquest_v3(s), SITE_FORWARD), false)),
            );
            let (worth, cost, ratio) = self.conquest_value(o);
            let anchor = self.conquest_anchor(o);
            self.log(&format!(
                "conquest: objective {idx} at ({:.0},{:.0}) seized: its {} spots are ours; turrets at ratio {ratio:.2} (worth {worth:.0} / cost {cost:.0})",
                anchor.x,
                anchor.z,
                spots.len()
            ));
            self.op_emit(
                "conquest",
                json!({"event": "seized", "t": frame / 30, "objective": idx, "spots": spots.len(), "line": o.line,
                       "worth": worth, "cost": cost, "ratio": ratio, "x": anchor.x.round(), "z": anchor.z.round()}),
            );
            self.eco.conquest.seized.push(Seized { idx, committed: false, turrets, since: frame, fortify_from: 0 });
        }
    }

    /// One seized objective's turn: `Some((why, held))` when it ends.
    fn conquest_seized_one(&mut self, i: usize, frame: i32) -> Option<(&'static str, bool)> {
        let s = self.eco.conquest.seized[i].clone();
        let o = self.eco.conquest.objectives.get(s.idx)?.clone();
        let spots = self.conquest_objective_spots(s.idx);
        if !spots.iter().any(|&p| self.conquest_spot_owned(p)) && !s.turrets.iter().any(|t| t.built && !t.light) {
            return Some(("the spots are lost", false));
        }
        if o.sites.iter().any(|&p| self.war.get_porc_threat(self.conquest_v3(p)) > 0.0) {
            return Some(("enemy defenses on a site", false));
        }
        let mut committed = s.committed;
        if !committed {
            let (worth, cost, ratio) = self.conquest_value(&o);
            let gain = self.conquest_gain();
            let means = self.conquest_turret().is_some_and(|d| self.conquest_affordable(d));
            if ratio >= gain && means {
                committed = true;
                self.conquest_seized_commit(
                    i,
                    frame,
                    &format!("pays: ratio {ratio:.2} (worth {worth:.0}, cost {cost:.0})"),
                );
            }
        }
        let mut turrets = std::mem::take(&mut self.eco.conquest.seized[i].turrets);
        self.conquest_order_turrets(&mut turrets, committed);
        self.eco.conquest.seized[i].turrets = turrets.clone();
        let heavy: Vec<&Turret> = turrets.iter().filter(|t| !t.light).collect();
        let gone = |t: &Turret| t.built || (t.task.is_none() && t.retries >= MAX_RETRIES);
        let guard_done = turrets.iter().filter(|t| t.light).all(gone);
        let fortify_from = self.eco.conquest.seized[i].fortify_from;
        if committed && heavy.iter().all(|t| t.built) && guard_done {
            return Some(("held", true));
        }
        if committed && !heavy.is_empty() && heavy.iter().all(|t| gone(t)) {
            return Some(if heavy.iter().any(|t| t.built) {
                ("held in part", true)
            } else {
                ("turret sites lost", false)
            });
        }
        if committed && frame - fortify_from > FORTIFY_TIMEOUT {
            return Some(if heavy.iter().any(|t| t.built) {
                ("held in part", true)
            } else {
                ("fortify timed out", false)
            });
        }
        None
    }

    fn conquest_seized_commit(&mut self, i: usize, frame: i32, why: &str) {
        let Some(s) = self.eco.conquest.seized.get_mut(i) else { return };
        if s.committed {
            return;
        }
        s.committed = true;
        s.fortify_from = frame;
        let idx = s.idx;
        self.log(&format!("conquest: seized objective {idx}: turrets go up ({why})"));
        self.op_emit("conquest", json!({"event": "seized commit", "t": frame / 30, "objective": idx, "why": why}));
    }

    /// Attacks by a seized objective still watching: its turrets go up now.
    pub(super) fn conquest_seized_attacked(&mut self, frame: i32, attacks: &[(i32, Vec3)]) {
        for i in 0..self.eco.conquest.seized.len() {
            let s = &self.eco.conquest.seized[i];
            if s.committed {
                continue;
            }
            let Some(o) = self.eco.conquest.objectives.get(s.idx) else { continue };
            let hit = attacks.iter().any(|&(_, p)| distance(self.conquest_near_site(o, p), p) <= super::ATTACK_RADIUS);
            if hit && self.conquest_turret().is_some_and(|d| self.conquest_affordable(d)) {
                self.conquest_seized_commit(i, frame, "attacked");
            }
        }
    }

    /// The next of `turrets` ordered: Lotuses (light) always, the conquest
    /// turret when `heavy_ok`; one under construction at a time, each only
    /// within our means (see `conquest_fortify`).
    fn conquest_order_turrets(&mut self, turrets: &mut [Turret], heavy_ok: bool) {
        let Some(heavy) = self.conquest_turret() else { return };
        let lotus = self.defs.get(GUARD_TURRET).unwrap_or(heavy);
        for i in 0..turrets.len() {
            let t = turrets[i];
            if t.built || (!t.light && !heavy_ok) {
                continue;
            }
            let def = if t.light { lotus } else { heavy };
            let standing = self.eco.porcs.iter().any(|u| {
                distance(u.pos(), t.site) <= BUILT_RADIUS
                    && u.def().is_some_and(|d| d.id() == def.id())
                    && !u.is_being_built()
            });
            // A guard Lotus standing within its reach covers the site.
            let covered = t.light
                && self.eco.porcs.iter().any(|u| {
                    u.health() > 0.0
                        && !u.is_being_built()
                        && u.def().is_some_and(|d| d.id() == lotus.id())
                        && distance(u.pos(), t.site) <= lotus.max_weapon_range()
                });
            if standing || (covered && t.task.is_none()) {
                turrets[i].built = true;
                turrets[i].task = None;
                continue;
            }
            if t.task.is_some_and(|id| self.eco.tasks.get(id).is_some())
                || (t.task.is_some() && t.retries >= MAX_RETRIES)
            {
                continue;
            }
            if let Some(a) = self.conquest_turret_ally(t, def) {
                turrets[i] = a;
                continue;
            }
            let busy = (turrets.iter().enumerate())
                .any(|(j, o)| j != i && !o.built && o.task.is_some_and(|id| self.eco.tasks.get(id).is_some()));
            if busy || !self.conquest_affordable(def) {
                continue;
            }
            let at = self.cb.map().find_closest_build_site(def, t.site, 250.0, 3, 0);
            let id = (at.x >= 0.0)
                .then(|| {
                    self.eco_add_construction_uncrowded(
                        WorkerTask::construction(def, at, 0),
                        Some(List::Porc),
                        Some(List::Porc),
                    )
                })
                .flatten();
            if t.task.is_some() || id.is_none() {
                turrets[i].retries += 1;
            }
            turrets[i].task = id;
            if let Some(id) = id {
                self.log(&format!("conquest: {} ordered at ({:.0},{:.0}) (task {id}, seized)", def.name(), at.x, at.z));
            }
        }
    }

    fn conquest_seized_end(&mut self, i: usize, why: &str, success: bool, frame: i32) {
        let s = self.eco.conquest.seized.remove(i);
        let mut dropped = 0;
        if success {
            self.eco.conquest.taken.push(s.idx);
            if let Some(o) = self.eco.conquest.objectives.get(s.idx) {
                let center = self.conquest_v3(self.conquest_anchor(o));
                self.eco.conquest.held.push(Held { idx: s.idx, center, added: 0, last_attack: 0 });
            }
        } else {
            for t in &s.turrets {
                if let Some(id) = t.task
                    && self
                        .eco
                        .tasks
                        .get(id)
                        .is_some_and(|task| task.as_construction().is_some_and(|c| c.target.is_none()))
                {
                    self.eco.drop_construction_task(id);
                    dropped += 1;
                }
            }
            self.eco.conquest.seize_cool.push((s.idx, frame + SEIZE_COOLDOWN));
        }
        let built = s.turrets.iter().filter(|t| t.built && !t.light).count();
        self.log(&format!(
            "conquest: seized objective {} ends ({why}) after {} s: {built} turrets standing, {dropped} unstarted dropped",
            s.idx,
            (frame - s.since) / 30
        ));
        self.op_emit(
            "conquest",
            json!({"event": "seized end", "t": frame / 30, "objective": s.idx, "why": why, "success": success,
                   "built": built, "dropped": dropped, "secs": (frame - s.since) / 30, "committed": s.committed}),
        );
    }

    /// A seized objective's turret job: the commander's first when near,
    /// else for builders near while metal is banked.
    pub(super) fn conquest_seized_bonus(&self, wid: i32, task: TaskId) -> Option<f32> {
        let t = (self.eco.conquest.seized.iter()).flat_map(|s| s.turrets.iter()).find(|t| t.task == Some(task))?;
        let w = self.eco.workers.get(&wid)?;
        if distance(w.get_pos(), t.site) > HELP_RADIUS {
            return None;
        }
        if w.is_com {
            return Some(BONUS);
        }
        (self.eco.metal >= BANK_EMPTY).then_some(HELP_BONUS)
    }

    /// With `conquestroute`: the route's next mex and its job, the
    /// commander's first once its factory stands (it had walked the route
    /// with the plop in hand: the factory went up at 2:02 on Wanderlust).
    /// Spots with our mex standing are passed; so is one hostile or with
    /// no job (a factory's yard).
    pub(super) fn conquest_route_step(&mut self) {
        let c = &self.eco.conquest;
        if c.route.is_empty() {
            return;
        }
        let ready = !self.fac.factories.is_empty()
            && (self.eco.commanders.iter().filter_map(|id| self.eco.workers.get(id))).any(|w| !w.has_plop());
        let mex = self.eco.build_ids.mex_id;
        let mut passed = Vec::new();
        let mut next = None;
        if c.op.is_none() && !c.done && ready {
            for &p in &c.route {
                // Done when a mex of ours stands finished there: a frame
                // started counts the spot ours, and the route had moved on,
                // leaving the start's mex half built until 1:38 on Agalta.
                let standing = self.eco.mexes.iter().any(|m| {
                    !m.is_being_built() && m.health() > 0.0 && distance(m.pos(), Vec3::new(p.x, m.pos().y, p.z)) < 50.0
                });
                if standing {
                    continue;
                }
                if self.conquest_spot_hostile(pos(p)) {
                    passed.push(format!("({:.0},{:.0}) hostile", p.x, p.z));
                    continue;
                }
                let job = self.eco.construction_tasks.iter().copied().find(|&id| {
                    self.eco.tasks.get(id).is_some_and(|t| {
                        t.as_construction().is_some_and(|k| k.build_type.id() == mex)
                            && distance(t.position, Vec3::new(p.x, t.position.y, p.z)) < ROUTE_JOB
                    })
                });
                match job {
                    Some(id) => {
                        next = Some((p, id));
                        break;
                    }
                    None => passed.push(format!("({:.0},{:.0}) has no job", p.x, p.z)),
                }
            }
        }
        let was = self.eco.conquest.route_next.map(|n| n.0);
        if next.map(|n| n.0) != was {
            let what = match next {
                Some((p, _)) => format!("next mex ({:.0},{:.0})", p.x, p.z),
                None if !ready => "waits for the factory".to_owned(),
                None => "done".to_owned(),
            };
            let note = if passed.is_empty() { String::new() } else { format!(" (passed {})", passed.join(", ")) };
            self.log(&format!("conquest route: {what}{note}"));
        }
        self.eco.conquest.route_next = next;
    }

    /// With `conquestroute`, until the commander sets out: the route's next
    /// mex is a commander's job first (see `conquest_route_step`).
    pub(super) fn conquest_route_bonus(&self, wid: i32, task: TaskId) -> Option<f32> {
        let c = &self.eco.conquest;
        let (_, id) = c.route_next.filter(|_| c.op.is_none() && !c.done)?;
        (id == task && self.eco.commanders.contains(&wid)).then_some(BONUS + MEX_FIRST)
    }

    /// With `conquestroute` in a one-on-one (see the module notes): the
    /// objectives searched now, the start chosen for the walk to the one the
    /// trip would pick, and the walk's mexes kept. `None`: start as usual.
    pub(crate) fn conquest_route_start(&mut self) -> Option<Vec3> {
        if self.dev_off("conquest") || !self.dev_off("compush") || self.dev_off("conquestroute") {
            return None;
        }
        if !self.terrain_wait(TERRAIN_WAIT) {
            self.log("conquest route: no terrain before the start; starting as usual");
            return None;
        }
        let input = self.conquest_search_input()?;
        let walk = input.walk().clone();
        let assets = input.assets.clone();
        let found = input.run();
        let n = found.len();
        self.eco.conquest.objectives = found;
        self.eco.conquest.control = vec![super::Control::default(); n];
        self.eco.conquest.assets = assets;
        self.eco.conquest.asked = true;
        let ranked = self.conquest_ranked();
        let Some(&(idx, _)) = ranked.first() else {
            self.log("conquest route: no objective open; starting as usual");
            return None;
        };
        // A line's door nearest our box (the factory is not up yet).
        let o = &self.eco.conquest.objectives[idx];
        let home = pos(self.graph.get_ally_base());
        let goal = if o.line {
            (o.sites.iter().copied()).min_by(|a, b| a.distance(home).total_cmp(&b.distance(home))).unwrap_or(o.center())
        } else {
            o.center()
        };
        let box_spots: Vec<Pos> = self.graph.get_ally_territory().iter().map(|s| pos(s.get_pos())).collect();
        if box_spots.is_empty() {
            return None;
        }
        let mut points: Vec<Pos> = (self.graph.get_metal_spots().iter().map(|s| pos(s.get_pos())))
            .filter(|p| box_spots.iter().any(|b| b.distance(*p) <= ROUTE_RADIUS))
            .collect();
        let starts: Vec<usize> =
            (0..points.len()).filter(|&k| box_spots.iter().any(|b| b.distance(points[k]) < 1.0)).collect();
        points.push(goal);
        let g = points.len() - 1;
        let fields: Vec<Paths> = points.iter().map(|&p| Paths::new(&walk, p, |_| false)).collect();
        let dist = |a: usize, b: usize| fields[a].distance_to(points[b]);
        let (s, mexes, len) = best_route(&points, &starts, g, dist)?;
        let route: Vec<Vec3> = mexes.iter().map(|&k| self.conquest_v3(points[k])).collect();
        let start = self.conquest_v3(points[s]);
        self.log(&format!(
            "conquest route: start at ({:.0},{:.0}) for objective {idx} at ({:.0},{:.0}), mexes {}, ground walk {len:.0} (of {} start spots)",
            start.x,
            start.z,
            goal.x,
            goal.z,
            route.iter().map(|p| format!("({:.0},{:.0})", p.x, p.z)).collect::<Vec<_>>().join(" "),
            starts.len()
        ));
        self.eco.conquest.route_note = Some(json!({
            "event": "route", "t": 0, "objective": idx, "start": [start.x.round(), start.z.round()],
            "goal": [goal.x.round(), goal.z.round()], "length": len.round(),
            "mexes": route.iter().map(|p| [p.x.round(), p.z.round()]).collect::<Vec<_>>(),
        }));
        self.eco.conquest.route = route;
        Some(start)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn straight(points: &[Pos]) -> impl Fn(usize, usize) -> Option<f32> + '_ {
        |a, b| Some(points[a].distance(points[b]))
    }

    #[test]
    fn the_commander_leans_toward_the_enemy_on_our_side_only() {
        let v = |x: f32| Vec3::new(x, 0.0, 1000.0);
        let (home, enemy, at) = (v(1000.0), v(9000.0), v(2000.0));
        // 800 elmos nearer the enemy than the commander: 100 off.
        assert!((face(at, v(2800.0), home, enemy) - 100.0).abs() < 1e-3);
        // Behind it: nothing; past the middle's share: nothing.
        assert_eq!(face(at, v(1500.0), home, enemy), 0.0);
        assert_eq!(face(at, v(6000.0), home, enemy), 0.0);
        // At the share's edge (55% of 8000 from home): still on.
        assert!(face(at, v(5300.0), home, enemy) > 0.0);
    }

    #[test]
    fn the_route_takes_its_mexes_in_order_toward_the_goal() {
        // Start box around x 1000; the goal far east. Mexes at 1000 (the
        // start), 1300 east, 1600 east, and one 600 west of the start (the
        // detour behind).
        let points = vec![
            Pos::new(1000.0, 1000.0),
            Pos::new(1300.0, 1000.0),
            Pos::new(1600.0, 1000.0),
            Pos::new(400.0, 1000.0),
            Pos::new(6000.0, 1000.0),
        ];
        let (s, mexes, len) = best_route(&points, &[0, 3], 4, straight(&points)).expect("a route");
        assert_eq!(s, 0);
        assert_eq!(mexes, vec![0, 1, 2]);
        assert!((len - 5000.0).abs() < 1.0, "{len}");
    }

    #[test]
    fn the_start_is_the_box_spot_nearest_the_goal_with_mexes_about() {
        // Two candidate starts, each with mexes around: the eastern one
        // walks less to the goal.
        let points = vec![
            Pos::new(1000.0, 1000.0),
            Pos::new(1100.0, 1200.0),
            Pos::new(900.0, 1300.0),
            Pos::new(2000.0, 1000.0),
            Pos::new(2100.0, 1200.0),
            Pos::new(1900.0, 1300.0),
            Pos::new(6000.0, 1000.0),
        ];
        let (s, mexes, _) = best_route(&points, &[0, 3], 6, straight(&points)).expect("a route");
        assert_eq!(s, 3);
        assert_eq!(mexes.len(), ROUTE_MEXES);
        assert!(mexes.iter().all(|&k| (3..6).contains(&k)), "{mexes:?}");
    }

    #[test]
    fn unreachable_legs_are_skipped_and_too_few_mexes_give_none() {
        let points = vec![Pos::new(0.0, 0.0), Pos::new(100.0, 0.0), Pos::new(5000.0, 0.0)];
        assert!(best_route(&points, &[0], 2, straight(&points)).is_none(), "two spots only");
        let points = vec![Pos::new(0.0, 0.0), Pos::new(100.0, 0.0), Pos::new(200.0, 0.0), Pos::new(5000.0, 0.0)];
        let none = |_: usize, _: usize| None;
        assert!(best_route(&points, &[0], 3, none).is_none());
    }
}
