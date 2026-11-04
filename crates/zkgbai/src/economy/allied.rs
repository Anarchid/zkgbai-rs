//! Allied projects: what an ally builds counts before we build our own.
//!
//! Allied ZKGBAIs play their own teams unless merged (see
//! `check_teams_merging`), and humans and other AIs build beside us. Each
//! instance used to plan as if alone: allied ZKGBAIs put their nukes on the
//! same spot, one after the other (each picks the alliance's base), and a
//! job whose site an ally's frame took was dropped, never helped.
//!
//! - An ally's frame is helped as ours would be, if enough of it is left
//!   to build to be worth the walk ([`ASSIST_LEFT`]; an ally's mex is done
//!   before our constructor gets there). A job whose site such an allied
//!   frame of its type took builds that frame (see `eco_clean_orders`); a
//!   project of [`ASSISTED`] we are about to start, or queued and have not
//!   started, goes to an ally's frame of its type near us instead (the one
//!   furthest along); for a superweapon, an ally's of any of the three, so
//!   the alliance raises one at a time. A build order on an allied frame
//!   assists it, and our team pays for what we put in.
//! - The engine tells us nothing of allied units' lives, so jobs on an
//!   ally's frame are looked at every second: done once it stands, dropped
//!   if it dies or leaves the alliance.
//! - Superweapons, anti-nukes and factories that take plates (see
//!   `plate`) go by turns between allied ZKGBAIs. AIs
//!   cannot read each other's command queues, so a plan shows only once its
//!   frame stands, and instances that decide at once would start at once.
//!   The k-th of a kind the alliance has (anyone's, frames too) is the turn
//!   of the (k mod n)-th of the n ZKGBAIs in team order. The next in that
//!   order waits [`LAPSE`] for it, the one after twice that, and so on, so
//!   an owner that does not want one holds the rest up a minute per step.
//! - What allies have counts by its effect where we can see it: an allied
//!   anti-nuke near where ours would stand covers us, and superweapon sites
//!   keep clear of allied superweapons as of ours.
//! - One nuke is enough for the alliance: none of ours starts while a silo
//!   stands or rises anywhere in it, anyone's, ours too (an allied frame we
//!   would help aside, see [`Ai::eco_alliance_nuke`]). Helping an ally's
//!   silo used to leave our own wish standing, so once it stood we started
//!   the next (Agalta 3v3, 2026-11-03: three silos in a row, the second on
//!   15 metal of income). Berthas, Zeniths and Disco Rave Parties may still
//!   come after it.

use std::collections::BTreeMap;

use springai::{Unit, UnitDef, Vec3};

use super::{List, TaskId, WorkerTask};
use crate::Ai;
use crate::military::scouts::{OWL, SPARROW};
use crate::util::distance;

/// Projects an ally's frame of the same type is helped with before we
/// start our own.
pub(super) const ASSISTED: [&str; 8] = [
    "staticnuke",
    "zenith",
    "raveparty",
    "staticheavyarty",
    "energysingu",
    "energyfusion",
    "staticantinuke",
    "staticmissilesilo",
];
/// One superweapon at a time per instance (see `eco_generate_tasks`), so
/// one turn order for the three.
pub(super) const SUPERWEAPONS: [&str; 3] = ["staticnuke", "zenith", "raveparty"];
/// Superweapon sites keep clear of these, anyone's.
pub(super) const BIG_GUNS: [&str; 4] = ["staticheavyarty", "staticnuke", "zenith", "raveparty"];
pub(super) const ANTI: &str = "staticantinuke";
/// The alliance's one nuke (see the module notes).
pub(super) const NUKE: &str = "staticnuke";
/// The wait per step of the turn order from the owner of a turn, frames
/// (1:00).
pub(super) const LAPSE: i32 = 1800;
/// A turn not asked for in this long is a new wish: its wait starts over.
const FORGET: i32 = 900;
/// An allied frame farther than this from our base and factories is left
/// to its owner, elmos.
pub(super) const ASSIST_REACH: f32 = 3000.0;
/// An allied anti-nuke this close to where ours would stand covers us:
/// half of `nuke_coverage` (2500).
const ANTI_SHARED: f32 = 1250.0;
/// An allied frame with less than this metal of it left to build is left
/// to its owner. Cheaper frames went up before help arrived: in a 3v3 smoke
/// on Comet Catcher (2026-11-01), allies helped 329 of each other's mexes,
/// 136 of which died unfinished, walking past the next spot to do it.
pub(super) const ASSIST_LEFT: f32 = 300.0;
/// The same note goes to the log at most this often, frames.
const SAY_EVERY: i32 = 30 * 60;

/// Turns asked for, notes logged, and the alliance's projects as last
/// counted.
#[derive(Default)]
pub struct AlliedProjects {
    turns: BTreeMap<String, Turn>,
    said: BTreeMap<String, i32>,
    /// Every living unit of [`ASSISTED`] types in the alliance, ours too,
    /// with its type: counted every second (see
    /// `eco_watch_allied_projects`), so the planners that ask each pass
    /// need not go over every allied unit.
    census: Vec<(Unit, &'static str)>,
    /// The alliance's factories, ours too, standing or rising: counted
    /// with the census, for plates (see `plate`).
    pub(super) factories: Vec<Unit>,
    /// The alliance's plates, ours too.
    plates: Vec<Unit>,
    /// Every allied team's factories, plates and mexes, ours too: whose
    /// the enemy near them is to answer (see `military::Faced`).
    anchors: Vec<(i32, Vec3)>,
    /// The alliance's air scouts (Owls, Sparrows) by team.
    scouts: Vec<(i32, &'static str)>,
    progress: BTreeMap<i32, FrameProgress>,
}

/// An unfinished tactical silo claims coverage while construction advances.
/// A fresh sighting gets time for its builders to arrive (90 seconds).
const ALLY_STALL: i32 = 30 * 90;
/// Keep substantial paid construction eligible for rescue even without an
/// active allied crew. Negligible abandoned starts may still be replaced.
const SILO_RESCUE_PROGRESS: f32 = 0.30;

struct FrameProgress {
    built: f32,
    advanced: i32,
    team: i32,
}

impl FrameProgress {
    fn observe(&mut self, built: f32, team: i32, frame: i32) {
        if team != self.team || built > self.built + 0.0001 {
            self.built = built;
            self.advanced = frame;
            self.team = team;
        } else if built < self.built {
            // Reclaiming the frame does not renew its coverage claim.
            self.built = built;
        }
    }

    fn active(&self, frame: i32) -> bool {
        frame - self.advanced < ALLY_STALL
    }
}

/// A wish for the next of a kind: the alliance's count of it when asked,
/// the frame the wait began and the frame last asked.
#[derive(Clone, Copy, Debug)]
struct Turn {
    count: usize,
    since: i32,
    last: i32,
}

/// The answer to a wish.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Wait {
    /// Whether we may start one now.
    ok: bool,
    /// Whose turn it is.
    owner: i32,
    /// Steps of the turn order from the owner to us (0: ours).
    steps: usize,
    /// Whether the wait began with this ask.
    fresh: bool,
    /// Whether this ask ended a wait (once).
    lapsed: bool,
}

/// How many steps after the owner of the `count`-th turn `team` comes in
/// `order` (sorted team ids, `team` among them).
fn steps(order: &[i32], count: usize, team: i32) -> usize {
    let n = order.len();
    let me = order.iter().position(|&t| t == team).unwrap_or(0);
    (me + n - count % n) % n
}

/// The `me`-th of `n`'s part of `quota`: the i-th to the (i mod n)-th.
fn quota_share(quota: usize, n: usize, me: usize) -> usize {
    (0..quota).filter(|i| i % n.max(1) == me).count()
}

impl AlliedProjects {
    /// Whether `team` may start the next of `kind`, the alliance having
    /// `count` of it: on its own turn at once, else after [`LAPSE`] per
    /// step from the owner, counted from the first ask at this count.
    fn ask(&mut self, kind: &str, order: &[i32], team: i32, count: usize, frame: i32) -> Wait {
        let n = order.len().max(1);
        let owner = order.get(count % n).copied().unwrap_or(team);
        let steps = steps(order, count, team);
        let reset = self.turns.get(kind).is_none_or(|t| t.count != count || frame - t.last > FORGET);
        if reset {
            self.turns.insert(kind.to_owned(), Turn { count, since: frame, last: frame });
        }
        let turn = self.turns.get_mut(kind).expect("inserted above");
        let was = turn.last;
        turn.last = frame;
        let due = turn.since + LAPSE * steps as i32;
        let ok = steps == 0 || frame >= due;
        Wait { ok, owner, steps, fresh: reset && !ok, lapsed: steps > 0 && ok && was < due }
    }

    /// How many of the types `names` the alliance has, as last counted.
    fn count(&self, names: &[&str]) -> usize {
        self.census.iter().filter(|(_, n)| names.contains(n)).count()
    }

    /// Whether `key` is due a log line at `frame` (and mark it said).
    fn say(&mut self, key: String, frame: i32) -> bool {
        match self.said.get(&key) {
            Some(&at) if frame - at < SAY_EVERY => false,
            _ => {
                self.said.insert(key, frame);
                true
            }
        }
    }
}

impl Ai {
    /// Us and the allied ZKGBAIs that plan on their own, in team order.
    fn eco_turn_order(&self) -> Vec<i32> {
        let mut order: Vec<i32> = self.iq.peers.iter().copied().chain([self.team_id]).collect();
        order.sort_unstable();
        order.dedup();
        order
    }

    /// Whether we may start the next of `kind` (the types `names`) now:
    /// see the module notes. Alone, always.
    pub(super) fn eco_ally_turn(&mut self, kind: &str, names: &[&str]) -> bool {
        let count = self.eco.allied.count(names);
        self.eco_ally_turn_at(kind, count)
    }

    /// [`Self::eco_ally_turn`] with the alliance's count of the kind given.
    pub(super) fn eco_ally_turn_at(&mut self, kind: &str, count: usize) -> bool {
        let order = self.eco_turn_order();
        if order.len() < 2 {
            return true;
        }
        let w = self.eco.allied.ask(kind, &order, self.team_id, count, self.frame);
        if w.fresh {
            self.log(&format!(
                "economy: the alliance's {kind} #{} is team {}'s to start; waiting up to {} s for it",
                count + 1,
                w.owner,
                LAPSE as usize * w.steps / 30
            ));
        } else if w.lapsed {
            self.log(&format!("economy: team {} started no {kind} #{} in its turn; starting ours", w.owner, count + 1));
        }
        w.ok
    }

    /// The alliance's standing fusions and singularities, ours too, as of
    /// the last census.
    pub(super) fn eco_alliance_reactors(&self) -> usize {
        (self.eco.allied.census.iter())
            .filter(|&&(u, n)| matches!(n, "energyfusion" | "energysingu") && u.health() > 0.0 && !u.is_being_built())
            .count()
    }

    /// Every allied team's factories, plates and mexes (ours too), as of
    /// the last census; none alone.
    pub(crate) fn ally_anchors(&self) -> &[(i32, Vec3)] {
        &self.eco.allied.anchors
    }

    /// Who of us builds what the alliance needs one of us to (its air
    /// scouts, its deep-scouting Widows): the first allied ZKGBAI in team
    /// order with a factory or plate of type `factory`, else the first.
    pub(crate) fn ally_owner_of(&self, factory: &str) -> i32 {
        let order = self.eco_turn_order();
        let plate = factory.strip_prefix("factory").map(|t| format!("plate{t}")).unwrap_or_default();
        let a = &self.eco.allied;
        let has = |team: i32| {
            a.factories.iter().any(|&u| u.team() == team && self.defs.name_of(u) == factory)
                || a.plates.iter().any(|&u| u.team() == team && self.defs.name_of(u) == plate)
        };
        order.iter().copied().find(|&t| has(t)).unwrap_or(order[0])
    }

    /// How many `name`s (an air scout) allies other than the allied
    /// ZKGBAIs (humans, other AIs) have.
    pub(crate) fn ally_others_have(&self, name: &str) -> usize {
        let order = self.eco_turn_order();
        self.eco.allied.scouts.iter().filter(|&&(t, n)| n == name && !order.contains(&t)).count()
    }

    /// Our part of `quota` the alliance wants: less what allies other
    /// than ZKGBAIs have of `name`, the i-th of the rest to the (i mod
    /// n)-th of the n allied ZKGBAIs in team order. Alone, all of it.
    pub(crate) fn ally_quota_share(&self, quota: usize, name: &str) -> usize {
        let order = self.eco_turn_order();
        if order.len() < 2 {
            return quota;
        }
        let rest = quota.saturating_sub(self.ally_others_have(name));
        let me = order.iter().position(|&t| t == self.team_id).unwrap_or(0);
        quota_share(rest, order.len(), me)
    }

    /// Where our assets are: the alliance's base and our factories.
    pub(super) fn eco_homes(&self) -> Vec<Vec3> {
        std::iter::once(self.graph.get_ally_center()).chain(self.fac.factories.values().map(|f| f.unit.pos())).collect()
    }

    pub(crate) fn eco_ally_project_active(&self, u: Unit) -> bool {
        !u.is_being_built() || self.eco.allied.progress.get(&u.id()).is_none_or(|p| p.active(self.frame))
    }

    fn eco_ally_silo_worth_helping(&self, u: Unit) -> bool {
        // A nearly finished silo is especially cheap to rescue; the normal
        // minimum remaining-work rule must not exclude it.
        u.build_progress() >= SILO_RESCUE_PROGRESS || (self.eco_ally_project_active(u) && self.eco_worth_helping(u))
    }

    /// Whether enough of the allied frame `u` is left to build to be worth
    /// sending help (see [`ASSIST_LEFT`]).
    pub(super) fn eco_worth_helping(&self, u: Unit) -> bool {
        u.def().is_some_and(|d| d.cost(self.metal) * (1.0 - u.build_progress()) >= ASSIST_LEFT)
    }

    /// Whether our workers may go and help at `p`.
    pub(super) fn eco_ally_site_safe(&self, p: Vec3) -> bool {
        !self.graph.is_enemy_territory(p) && self.war.get_porc_threat(p) <= 0.0 && self.war.get_threat(p) <= 0.0
    }

    /// The allied frame of `def` we would help (of any superweapon, for
    /// one): near our base, safe, the furthest along.
    pub(super) fn eco_ally_frame(&self, def: UnitDef) -> Option<Unit> {
        let homes = self.eco_homes();
        let name = self.defs.name(def);
        let kind = |n: &str| n == name || (SUPERWEAPONS.contains(&name) && SUPERWEAPONS.contains(&n));
        (self.eco.allied.census.iter())
            .filter(|&&(u, n)| kind(n) && u.team() != self.team_id && u.health() > 0.0 && u.is_being_built())
            .map(|&(u, _)| u)
            .filter(|u| {
                let p = u.pos();
                homes.iter().any(|&h| distance(h, p) <= ASSIST_REACH)
                    && self.eco_ally_site_safe(p)
                    && !u.def().is_some_and(|d| self.eco_super_project_exposed(d, p))
                    && if name == crate::military::missiles::SILO {
                        self.eco_ally_silo_worth_helping(*u) && self.missiles_ally_site_wanted(p)
                    } else {
                        self.eco_worth_helping(*u)
                    }
            })
            .max_by(|a, b| a.build_progress().total_cmp(&b.build_progress()))
    }

    /// Our job building `u`, if one does.
    pub(super) fn eco_task_on(&self, u: Unit) -> Option<TaskId> {
        (self.eco.construction_tasks.iter().copied())
            .find(|&id| self.eco.tasks.get(id).and_then(|t| t.as_construction()).is_some_and(|c| c.target == Some(u)))
    }

    /// The list a project of `def` belongs to.
    fn eco_project_list(&self, def: UnitDef) -> List {
        match self.defs.name(def) {
            "energysingu" | "energyfusion" => List::Fusion,
            n if BIG_GUNS.contains(&n) => List::SuperWep,
            _ => List::Construction,
        }
    }

    /// A job helping to build the allied frame `u`.
    pub(super) fn eco_help_frame(&mut self, u: Unit, list: List) -> Option<TaskId> {
        if let Some(id) = self.eco_task_on(u) {
            self.logistics_assist(id);
            return Some(id);
        }
        let def = u.def()?;
        if self.eco_super_project_exposed(def, u.pos()) {
            return None;
        }
        let mut ct = WorkerTask::construction(def, u.pos(), u.building_facing());
        if let Some(c) = ct.as_construction_mut() {
            c.target = Some(u);
            c.frame_issued = self.frame;
        }
        // No build check: the frame's footprint is taken, by the frame.
        let id = self.eco.tasks.insert(ct);
        self.eco.construction_tasks.push(id);
        if list != List::Construction {
            self.eco.list_mut(list).push(id);
        }
        let p = u.pos();
        self.log(&format!(
            "economy: helping team {}'s {} at ({:.0}, {:.0}), {:.0}% built, instead of starting our own",
            u.team(),
            self.defs.name(def),
            p.x,
            p.z,
            100.0 * u.build_progress()
        ));
        self.logistics_assist(id);
        Some(id)
    }

    /// Before starting a project of `def`: help an ally's frame of it
    /// instead, if there is one we would (see [`Self::eco_ally_frame`]).
    /// Whether one is (now) helped.
    pub(super) fn eco_assist_ally(&mut self, def: UnitDef) -> bool {
        if self.allies.is_empty() || !ASSISTED.contains(&self.defs.name(def)) {
            return false;
        }
        let Some(u) = self.eco_ally_frame(def) else { return false };
        let pending = self.eco.construction_tasks.iter().copied().find(|&id| {
            self.eco
                .tasks
                .get(id)
                .and_then(|t| t.as_construction())
                .is_some_and(|c| c.target.is_none() && !c.fac_plop && c.build_type == def)
        });
        if let Some(id) = pending {
            self.eco_adopt_ally_frame(id, u);
            true
        } else {
            let list = self.eco_project_list(def);
            self.eco_help_frame(u, list).is_some()
        }
    }

    /// The alliance's nuke silo, if it has one, standing or rising: ours,
    /// or an ally's (as of the last census) other than the frame we would
    /// help (see [`Self::eco_ally_frame`]), which our silo job goes to
    /// instead. While there is one we start none: one is enough.
    pub(super) fn eco_alliance_nuke(&self) -> Option<Unit> {
        let ours = self.eco.super_weps.iter().copied().find(|&u| u.health() > 0.0 && self.defs.name_of(u) == NUKE);
        ours.or_else(|| {
            let helped = self.defs.get(NUKE).and_then(|d| self.eco_ally_frame(d));
            (self.eco.allied.census.iter())
                .filter(|&&(u, n)| n == NUKE && u.team() != self.team_id && u.health() > 0.0)
                .map(|&(u, _)| u)
                .find(|&u| !u.is_being_built() || Some(u) != helped)
        })
    }

    /// An allied anti-nuke (standing or rising) near where ours would
    /// stand.
    fn eco_ally_anti(&self, site: Vec3) -> Option<Unit> {
        (self.eco.allied.census.iter())
            .filter(|&&(u, n)| n == ANTI && u.team() != self.team_id && u.health() > 0.0)
            .map(|&(u, _)| u)
            .find(|u| distance(u.pos(), site) <= ANTI_SHARED)
    }

    /// Whether to start our anti-nuke at `site` now: not while an ally's
    /// covers it (helped if it is a frame), nor before our turn.
    pub(super) fn eco_anti_due(&mut self, site: Vec3) -> bool {
        if self.allies.is_empty() {
            return true;
        }
        if let Some(a) = self.eco_ally_anti(site) {
            if a.is_being_built()
                && self.eco_task_on(a).is_none()
                && self.eco_ally_site_safe(a.pos())
                && self.eco_worth_helping(a)
            {
                self.eco_help_frame(a, List::Construction);
            }
            let p = a.pos();
            if self.eco.allied.say(format!("anti{}", a.id()), self.frame) {
                self.log(&format!(
                    "economy: team {}'s anti-nuke at ({:.0}, {:.0}) covers our base; none of our own",
                    a.team(),
                    p.x,
                    p.z
                ));
            }
            return false;
        }
        self.eco_ally_turn("anti-nuke", &[ANTI])
    }

    /// Whether a superweapon or Bertha, anyone's, stands or rises within
    /// `radius` of `site`.
    pub(super) fn eco_big_gun_near(&self, site: Vec3, radius: f32) -> bool {
        (self.eco.allied.census.iter())
            .any(|&(u, n)| BIG_GUNS.contains(&n) && u.health() > 0.0 && distance(u.pos(), site) <= radius)
    }

    /// Every second: plates follow their factories (see `plate`); end jobs
    /// on allied frames that stand, died or left the alliance; move
    /// projects not yet started (and a superweapon waiting for power) to an
    /// ally's frame of their type.
    pub(super) fn eco_watch_allied_projects(&mut self) {
        if self.allies.is_empty() {
            return;
        }
        self.eco_watch_plates();
        let (mut census, mut factories, mut plates) = (Vec::new(), Vec::new(), Vec::new());
        let (mut anchors, mut scouts) = (Vec::new(), Vec::new());
        for u in self.cb.friendly_units().into_iter().filter(|u| u.health() > 0.0) {
            let name = self.defs.name_of(u);
            if let Some(&n) = ASSISTED.iter().find(|&&n| n == name) {
                census.push((u, n));
            } else if name.starts_with("factory") {
                factories.push(u);
                anchors.push((u.team(), u.pos()));
            } else if name.starts_with("plate") {
                plates.push(u);
                anchors.push((u.team(), u.pos()));
            } else if name == "staticmex" {
                anchors.push((u.team(), u.pos()));
            } else if let Some(&n) = [OWL, SPARROW].iter().find(|&&n| n == name) {
                scouts.push((u.team(), n));
            }
        }
        let a = &mut self.eco.allied;
        a.progress.retain(|id, _| census.iter().any(|(u, _)| u.id() == *id));
        for (u, _) in &census {
            let progress = a.progress.entry(u.id()).or_insert_with(|| FrameProgress {
                built: u.build_progress(),
                advanced: self.frame,
                team: u.team(),
            });
            progress.observe(u.build_progress(), u.team(), self.frame);
        }
        (a.census, a.factories, a.plates, a.anchors, a.scouts) = (census, factories, plates, anchors, scouts);
        let mut ended = Vec::new();
        let mut pending = Vec::new();
        for &id in &self.eco.construction_tasks {
            let Some(t) = self.eco.tasks.get(id) else { continue };
            let Some(c) = t.as_construction() else { continue };
            match c.target {
                Some(u) if u.team() != self.team_id => {
                    let gone = u.health() <= 0.0 || !self.allies.contains(&u.team());
                    let stalled = self.defs.name(c.build_type) == crate::military::missiles::SILO
                        && !self.eco_ally_silo_worth_helping(u)
                        && !self.op_priority(id);
                    if gone || !u.is_being_built() || stalled {
                        ended.push((
                            id,
                            if gone {
                                "is gone"
                            } else if stalled {
                                "stalled"
                            } else {
                                "stands"
                            },
                            self.defs.name(c.build_type).to_owned(),
                            t.position,
                        ));
                    }
                }
                None if !c.fac_plop && ASSISTED.contains(&self.defs.name(c.build_type)) => {
                    pending.push((id, c.build_type, t.position));
                }
                _ => {}
            }
        }
        for (id, outcome, name, p) in ended {
            self.log(&format!("economy: the allied {name} we helped at ({:.0}, {:.0}) {}", p.x, p.z, outcome));
            if outcome == "stalled" {
                self.logistics_close(id, "stalled_ally");
            }
            self.eco.drop_construction_task(id);
        }
        for (id, def, at) in pending {
            let anti = self.defs.name(def) == ANTI;
            // An anti-nuke counts where ours would stand: an ally's there
            // (helped while it rises) covers us. Ours is planned afresh if
            // it goes (see `eco_anti_due`).
            let u = if anti {
                let a = self.eco_ally_anti(at);
                if a.is_some_and(|a| !a.is_being_built()) {
                    let p = a.map_or(at, |a| a.pos());
                    self.log(&format!(
                        "economy: an allied anti-nuke at ({:.0}, {:.0}) covers ours, queued at ({:.0}, {:.0}); dropped",
                        p.x, p.z, at.x, at.z
                    ));
                    if self.logistics_close(id, "covered_by_ally").is_some() {
                        self.logistics.stats.met += 1;
                    }
                    self.eco.drop_construction_task(id);
                    self.eco.anti_nuke = false;
                    continue;
                }
                a.filter(|&a| self.eco_ally_site_safe(a.pos()) && self.eco_worth_helping(a))
            } else {
                self.eco_ally_frame(def)
            };
            let Some(u) = u else { continue };
            if anti {
                self.eco.anti_nuke = false;
            }
            self.eco_adopt_ally_frame(id, u);
        }
        if let Some((def, _)) = self.eco_awaiting()
            && ASSISTED.contains(&self.defs.name(def))
            && self.eco_ally_frame(def).is_some()
        {
            self.eco.pending_power = None;
            self.eco_assist_ally(def);
        }
    }

    pub(super) fn eco_adopt_ally_frame(&mut self, id: TaskId, u: Unit) {
        let Some(def) = self.eco.tasks.get(id).and_then(|t| t.as_construction()).map(|c| c.build_type) else { return };
        if let Some(other) = self.eco_task_on(u).filter(|&other| other != id) {
            self.logistics_handoff(id, other);
            self.eco.drop_construction_task(id);
            self.logistics_assist(other);
            return;
        }
        let p = u.pos();
        self.log(&format!(
            "economy: team {} started a {} at ({:.0}, {:.0}); our queued {} helps it instead",
            u.team(),
            self.defs.name_of(u),
            p.x,
            p.z,
            self.defs.name(def)
        ));
        let idle = self.eco.stop_workers(id);
        if let Some(t) = self.eco.tasks.get_mut(id) {
            t.position = p;
            if let Some(c) = t.as_construction_mut() {
                // (A superweapon's may be another of the three.)
                c.build_type = u.def().unwrap_or(def);
                c.target = Some(u);
                c.facing = u.building_facing();
                c.frame_issued = self.frame;
                // Prerequisites were sited for our old footprint.
                c.prerequisites.clear();
            }
        }
        self.logistics_sync_task(id);
        self.logistics_assist(id);
        self.eco.idle_workers(&idle);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    thread_local! {
        static BUILD: std::cell::Cell<f32> = const { std::cell::Cell::new(0.25) };
        static TEAM: std::cell::Cell<i32> = const { std::cell::Cell::new(3) };
    }

    fn project_ai() -> Ai {
        BUILD.set(0.25);
        TEAM.set(3);
        unsafe extern "C" fn cost(_: i32, def: i32, _: i32) -> f32 {
            def as f32 * 1000.0
        }
        unsafe extern "C" fn time(_: i32, def: i32) -> f32 {
            def as f32 * 2000.0
        }
        unsafe extern "C" fn team(_: i32, _: i32) -> i32 {
            TEAM.get()
        }
        unsafe extern "C" fn def(_: i32, _: i32) -> i32 {
            2
        }
        unsafe extern "C" fn facing(_: i32, _: i32) -> i32 {
            0
        }
        unsafe extern "C" fn progress(_: i32, _: i32) -> f32 {
            BUILD.get()
        }
        unsafe extern "C" fn building(_: i32, _: i32) -> bool {
            BUILD.get() < 1.0
        }
        unsafe extern "C" fn near(_: i32, _: *mut f32, _: f32, _: bool, _: *mut i32, _: i32) -> i32 {
            0
        }
        unsafe extern "C" fn health(_: i32, _: i32) -> f32 {
            100.0
        }
        let mut ai = super::super::commander::tests::test_ai_with(|t| {
            t.UnitDef_getCost = Some(cost);
            t.UnitDef_getBuildTime = Some(time);
            t.Unit_getDef = Some(def);
            t.Unit_getTeam = Some(team);
            t.Unit_getBuildingFacing = Some(facing);
            t.Unit_getBuildProgress = Some(progress);
            t.Unit_isBeingBuilt = Some(building);
            t.getFriendlyUnitsIn = Some(near);
            t.Unit_getHealth = Some(health);
        });
        ai.allies.push(3);
        ai.dev_enable("clients");
        ai.dev_enable("logistics");
        ai.frame = 90;
        ai.offset = 0;
        ai
    }

    #[test]
    fn allied_handoff_keeps_the_goal_and_refreshes_the_site_type_and_crew() {
        use crate::logistics::{Kind, Worth};
        let mut ai = project_ai();
        let def = UnitDef::from_id(ai.cb, 1).unwrap();
        let id = ai.eco.tasks.insert(WorkerTask::construction(def, Vec3::xz(50.0, 0.0), 0));
        ai.eco.construction_tasks.push(id);
        let project = ai
            .logistics_open(id, Kind::Operator, Worth::Deadline { value: 9000.0, by: 500.0 }, "cover targets", 0.0)
            .unwrap();
        let i = ai.logistics.issues.get_mut(&id).unwrap();
        i.crew = vec![10];
        i.share = 100.0;
        let frame = Unit::from_id(ai.cb, 20).unwrap();
        ai.eco_adopt_ally_frame(id, frame);
        let i = &ai.logistics.issues[&id];
        assert_eq!(i.id, project);
        assert_eq!(i.kind, Kind::Operator);
        assert_eq!(i.worth.at(500.0), 9000.0);
        assert_eq!(i.pos, frame.pos());
        assert_eq!((i.cost, i.build_time), (2000.0, 4000.0));
        assert!(i.crew.is_empty());
        assert!(ai.logistics_bonus(10, id).is_none());
        assert!(ai.logistics_assignment(10).is_none());
        assert_eq!(ai.eco.tasks.get(id).unwrap().as_construction().unwrap().target, Some(frame));
    }

    #[test]
    fn duplicate_assistance_jobs_keep_one_commitment_and_the_existing_frame_task() {
        use crate::logistics::{Kind, Worth};
        let mut ai = project_ai();
        let frame = Unit::from_id(ai.cb, 20).unwrap();
        let help = ai.eco_help_frame(frame, List::Construction).unwrap();
        assert!(ai.logistics_tracks(help), "an assistance decision receives a commitment");
        assert_eq!(ai.eco_help_frame(frame, List::Construction), Some(help));
        let def = UnitDef::from_id(ai.cb, 1).unwrap();
        let old = ai.eco.tasks.insert(WorkerTask::construction(def, Vec3::xz(50.0, 0.0), 0));
        ai.eco.construction_tasks.push(old);
        let project = ai.logistics_open(old, Kind::Operator, Worth::Flat(9000.0), "cover targets", 0.0).unwrap();
        ai.eco_adopt_ally_frame(old, frame);
        assert!(ai.eco.tasks.get(old).is_none());
        assert_eq!(ai.eco.construction_tasks, vec![help]);
        assert_eq!(ai.logistics.issues.len(), 1);
        let i = &ai.logistics.issues[&help];
        assert_eq!((i.id, i.task, i.kind), (project, help, Kind::Operator));
        assert_eq!(i.pos, frame.pos());
    }

    #[test]
    fn direct_task_retargeting_cannot_keep_an_old_crew_until_the_next_forecast() {
        use crate::logistics::{Kind, Worth};
        let mut ai = project_ai();
        let def = UnitDef::from_id(ai.cb, 1).unwrap();
        let id = ai.eco.tasks.insert(WorkerTask::construction(def, Vec3::xz(50.0, 0.0), 0));
        ai.logistics_open(id, Kind::Grid, Worth::Flat(5000.0), "grid", 0.0);
        let i = ai.logistics.issues.get_mut(&id).unwrap();
        i.crew = vec![10];
        i.share = 500.0;
        assert!(ai.logistics_bonus(10, id).is_some());
        ai.eco.tasks.get_mut(id).unwrap().position = Vec3::xz(2000.0, 0.0);
        assert!(ai.logistics_bonus(10, id).is_none());
        assert!(ai.logistics_assignment(10).is_none());
        assert!(ai.logistics_allows(11, id), "an old crew cannot exclude the new site's builders");
        // Revaluation is another entry point and must refresh the binding too.
        ai.logistics_open(id, Kind::Grid, Worth::Flat(6000.0), "moved grid", 0.0);
        assert_eq!(ai.logistics.issues[&id].pos, Vec3::xz(2000.0, 0.0));
        assert!(ai.logistics.issues[&id].crew.is_empty());
    }

    #[test]
    fn adopted_completion_is_detected_even_without_a_spatial_search_result() {
        let mut ai = project_ai();
        let u = Unit::from_id(ai.cb, 20).unwrap();
        let task = ai.eco_help_frame(u, List::Construction).unwrap();
        BUILD.set(1.0);
        ai.eco.drop_construction_task(task);
        ai.frame = 107;
        ai.logistics_update(107);
        assert!(!ai.logistics_tracks(task));
        assert_eq!(ai.logistics.stats.met, 1);
    }

    #[test]
    fn unfinished_or_captured_ally_frames_do_not_count_as_completed_projects() {
        for captured in [false, true] {
            let mut ai = project_ai();
            let u = Unit::from_id(ai.cb, 20).unwrap();
            let task = ai.eco_help_frame(u, List::Construction).unwrap();
            if captured {
                BUILD.set(1.0);
                TEAM.set(4);
            }
            ai.eco.drop_construction_task(task);
            ai.frame = 107;
            ai.logistics_update(107);
            assert_eq!(ai.logistics.stats.met, 0);
            assert_eq!(ai.logistics.stats.gone, 1);
        }
    }

    #[test]
    fn an_unfinished_ally_claim_needs_observed_progress_and_recovers_when_it_resumes() {
        let mut p = FrameProgress { built: 0.1, advanced: 0, team: 3 };
        p.observe(0.1, 3, ALLY_STALL - 1);
        assert!(p.active(ALLY_STALL - 1));
        p.observe(0.1, 3, ALLY_STALL);
        assert!(!p.active(ALLY_STALL));
        p.observe(0.05, 3, ALLY_STALL + 15);
        assert!(!p.active(ALLY_STALL + 15), "reclaiming is not construction progress");
        p.observe(0.11, 3, ALLY_STALL + 30);
        assert!(p.active(ALLY_STALL + 30));
        p.observe(0.11, 4, ALLY_STALL * 2);
        assert!(p.active(ALLY_STALL * 2), "a new owner receives its own grace period");
    }

    #[test]
    fn stalled_silos_retain_paid_work_without_claiming_active_coverage() {
        let mut ai = project_ai();
        let u = Unit::from_id(ai.cb, 42).unwrap();
        ai.frame = ALLY_STALL;
        ai.eco.allied.progress.insert(u.id(), FrameProgress { built: 0.30, advanced: 0, team: 3 });
        for built in [0.30, 0.60, 0.99] {
            BUILD.set(built);
            assert!(ai.eco_ally_silo_worth_helping(u), "rescue {built:.0}% paid construction");
            assert!(!ai.eco_ally_project_active(u), "paid work alone is not active target coverage");
        }
        assert!(!ai.eco_worth_helping(u), "near completion would fail the ordinary remaining-work floor");
        BUILD.set(0.29);
        assert!(!ai.eco_ally_silo_worth_helping(u), "a small stalled or reclaimed frame may be replaced");
        ai.eco.allied.progress.get_mut(&u.id()).unwrap().observe(0.31, 3, ai.frame);
        BUILD.set(0.31);
        assert!(ai.eco_ally_project_active(u), "resumed construction restores coverage");
        BUILD.set(0.10);
        assert!(ai.eco_ally_silo_worth_helping(u), "small active projects can still receive help");
    }

    /// Ask every 30 frames (as the planners do, more often) from `from`
    /// through `to`; the last answer.
    fn ask_until(
        p: &mut AlliedProjects,
        kind: &'static str,
        order: &[i32],
        team: i32,
        count: usize,
        from: i32,
        to: i32,
    ) -> Wait {
        let mut w = p.ask(kind, order, team, count, from);
        let mut f = from;
        while f < to {
            f = (f + 30).min(to);
            w = p.ask(kind, order, team, count, f);
        }
        w
    }

    #[test]
    fn turns_go_round_the_team_order_by_the_alliance_count() {
        let order = [1, 3, 6];
        assert_eq!(steps(&order, 0, 1), 0);
        assert_eq!(steps(&order, 0, 3), 1);
        assert_eq!(steps(&order, 0, 6), 2);
        // The second is team 3's, the third team 6's, the fourth team 1's.
        assert_eq!(steps(&order, 1, 3), 0);
        assert_eq!(steps(&order, 1, 1), 2);
        assert_eq!(steps(&order, 2, 6), 0);
        assert_eq!(steps(&order, 3, 1), 0);
    }

    #[test]
    fn the_owner_starts_at_once_and_the_rest_wait_a_lapse_per_step() {
        let order = [0, 2, 5];
        let mut owner = AlliedProjects::default();
        assert!(owner.ask("superweapon", &order, 0, 0, 100).ok);

        let mut next = AlliedProjects::default();
        let w = next.ask("superweapon", &order, 2, 0, 100);
        assert_eq!((w.ok, w.owner, w.steps, w.fresh), (false, 0, 1, true));
        let w = ask_until(&mut next, "superweapon", &order, 2, 0, 130, 100 + LAPSE - 1);
        assert!(!w.ok && !w.fresh);
        let w = next.ask("superweapon", &order, 2, 0, 100 + LAPSE);
        assert!(w.ok && w.lapsed);
        assert!(!next.ask("superweapon", &order, 2, 0, 100 + LAPSE + 30).lapsed);

        let mut last = AlliedProjects::default();
        assert!(!ask_until(&mut last, "superweapon", &order, 5, 0, 100, 100 + 2 * LAPSE - 1).ok);
        assert!(last.ask("superweapon", &order, 5, 0, 100 + 2 * LAPSE).ok);
    }

    #[test]
    fn a_new_count_or_a_forgotten_wish_restarts_the_wait() {
        let order = [0, 2];
        let mut p = AlliedProjects::default();
        assert!(!p.ask("anti-nuke", &order, 2, 0, 0).ok);
        // Team 0 started one: the next is ours, at once.
        assert!(p.ask("anti-nuke", &order, 2, 1, 600).ok);
        // A third would be team 0's again: the wait starts over.
        let w = p.ask("anti-nuke", &order, 2, 2, 900);
        assert!(!w.ok && w.fresh);
        // Not asked for a while: a new wish, a new wait.
        let again = 900 + FORGET + 30;
        let w = p.ask("anti-nuke", &order, 2, 2, again);
        assert!(!w.ok && w.fresh);
        assert!(!ask_until(&mut p, "anti-nuke", &order, 2, 2, again, again + LAPSE - 1).ok);
        assert!(p.ask("anti-nuke", &order, 2, 2, again + LAPSE).ok);
    }

    #[test]
    fn kinds_keep_their_own_turns() {
        let order = [0, 1];
        let mut p = AlliedProjects::default();
        assert!(!p.ask("superweapon", &order, 1, 0, 0).ok);
        assert!(p.ask("anti-nuke", &order, 1, 1, 0).ok);
        assert!(ask_until(&mut p, "superweapon", &order, 1, 0, 30, LAPSE).ok);
    }

    #[test]
    fn quotas_go_round_the_team_order_and_add_up() {
        assert_eq!([0, 1, 2].map(|me| quota_share(2, 3, me)), [1, 1, 0]);
        assert_eq!([0, 1, 2].map(|me| quota_share(4, 3, me)), [2, 1, 1]);
        assert_eq!([0, 1].map(|me| quota_share(0, 2, me)), [0, 0]);
        for (q, n) in [(1, 1), (5, 2), (7, 4)] {
            assert_eq!((0..n).map(|me| quota_share(q, n, me)).sum::<usize>(), q);
        }
    }

    #[test]
    fn notes_are_said_once_a_minute() {
        let mut p = AlliedProjects::default();
        assert!(p.say("a".into(), 0));
        assert!(!p.say("a".into(), SAY_EVERY - 1));
        assert!(p.say("b".into(), 10));
        assert!(p.say("a".into(), SAY_EVERY));
    }
}
