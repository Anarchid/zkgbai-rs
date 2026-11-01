//! Allied projects: what an ally builds counts before we build our own.
//!
//! Allied ZKGBAIs play their own teams unless merged (see
//! `check_teams_merging`), and humans and other AIs build beside us. Each
//! instance used to plan as if alone: allied ZKGBAIs put their nukes on the
//! same spot, one after the other (each picks the alliance's base), and a
//! job whose site an ally's frame took was dropped, never helped.
//!
//! - An ally's frame is helped as ours would be. A job whose site an allied
//!   frame of its type took builds that frame (see `eco_clean_orders`); a
//!   project of [`ASSISTED`] we are about to start, or queued and have not
//!   started, goes to an ally's frame of its type near us instead (the one
//!   furthest along); for a superweapon, an ally's of any of the three, so
//!   the alliance raises one at a time. A build order on an allied frame
//!   assists it, and our team pays for what we put in.
//! - The engine tells us nothing of allied units' lives, so jobs on an
//!   ally's frame are looked at every second: done once it stands, dropped
//!   if it dies or leaves the alliance.
//! - Superweapons and anti-nukes go by turns between allied ZKGBAIs. AIs
//!   cannot read each other's command queues, so a plan shows only once its
//!   frame stands, and instances that decide at once would start at once.
//!   The k-th of a kind the alliance has (anyone's, frames too) is the turn
//!   of the (k mod n)-th of the n ZKGBAIs in team order. The next in that
//!   order waits [`LAPSE`] for it, the one after twice that, and so on, so
//!   an owner that does not want one holds the rest up a minute per step.
//! - What allies have counts by its effect where we can see it: an allied
//!   anti-nuke near where ours would stand covers us, and superweapon sites
//!   keep clear of allied superweapons as of ours.

use std::collections::BTreeMap;

use springai::{Unit, UnitDef, Vec3};

use super::{List, TaskId, WorkerTask};
use crate::Ai;
use crate::util::distance;

/// Projects an ally's frame of the same type is helped with before we
/// start our own.
pub(super) const ASSISTED: [&str; 7] =
    ["staticnuke", "zenith", "raveparty", "staticheavyarty", "energysingu", "energyfusion", "staticantinuke"];
/// One superweapon at a time per instance (see `eco_generate_tasks`), so
/// one turn order for the three.
pub(super) const SUPERWEAPONS: [&str; 3] = ["staticnuke", "zenith", "raveparty"];
/// Superweapon sites keep clear of these, anyone's.
pub(super) const BIG_GUNS: [&str; 4] = ["staticheavyarty", "staticnuke", "zenith", "raveparty"];
pub(super) const ANTI: &str = "staticantinuke";
/// The wait per step of the turn order from the owner of a turn, frames
/// (1:00).
pub(super) const LAPSE: i32 = 1800;
/// A turn not asked for in this long is a new wish: its wait starts over.
const FORGET: i32 = 900;
/// An allied frame farther than this from our base and factories is left
/// to its owner, elmos.
const ASSIST_REACH: f32 = 3000.0;
/// An allied anti-nuke this close to where ours would stand covers us:
/// half of `nuke_coverage` (2500).
const ANTI_SHARED: f32 = 1250.0;
/// The same note goes to the log at most this often, frames.
const SAY_EVERY: i32 = 30 * 60;

/// Turns asked for, notes logged, and the alliance's projects as last
/// counted.
#[derive(Default)]
pub struct AlliedProjects {
    turns: BTreeMap<&'static str, Turn>,
    said: BTreeMap<String, i32>,
    /// Every living unit of [`ASSISTED`] types in the alliance, ours too,
    /// with its type: counted every second (see
    /// `eco_watch_allied_projects`), so the planners that ask each pass
    /// need not go over every allied unit.
    census: Vec<(Unit, &'static str)>,
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

impl AlliedProjects {
    /// Whether `team` may start the next of `kind`, the alliance having
    /// `count` of it: on its own turn at once, else after [`LAPSE`] per
    /// step from the owner, counted from the first ask at this count.
    fn ask(&mut self, kind: &'static str, order: &[i32], team: i32, count: usize, frame: i32) -> Wait {
        let n = order.len().max(1);
        let owner = order.get(count % n).copied().unwrap_or(team);
        let steps = steps(order, count, team);
        let reset = self.turns.get(kind).is_none_or(|t| t.count != count || frame - t.last > FORGET);
        if reset {
            self.turns.insert(kind, Turn { count, since: frame, last: frame });
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
    pub(super) fn eco_ally_turn(&mut self, kind: &'static str, names: &[&str]) -> bool {
        let order = self.eco_turn_order();
        if order.len() < 2 {
            return true;
        }
        let count = self.eco.allied.count(names);
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

    /// Where our assets are: the alliance's base and our factories.
    fn eco_homes(&self) -> Vec<Vec3> {
        std::iter::once(self.graph.get_ally_center()).chain(self.fac.factories.values().map(|f| f.unit.pos())).collect()
    }

    /// Whether our workers may go and help at `p`.
    fn eco_ally_site_safe(&self, p: Vec3) -> bool {
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
                homes.iter().any(|&h| distance(h, p) <= ASSIST_REACH) && self.eco_ally_site_safe(p)
            })
            .max_by(|a, b| a.build_progress().total_cmp(&b.build_progress()))
    }

    /// Our job building `u`, if one does.
    fn eco_task_on(&self, u: Unit) -> Option<TaskId> {
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
    fn eco_help_frame(&mut self, u: Unit, list: List) -> Option<TaskId> {
        let def = u.def()?;
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
        if self.eco_task_on(u).is_none() {
            let list = self.eco_project_list(def);
            self.eco_help_frame(u, list);
        }
        true
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
            if a.is_being_built() && self.eco_task_on(a).is_none() && self.eco_ally_site_safe(a.pos()) {
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

    /// Every second: end jobs on allied frames that stand, died or left
    /// the alliance; move projects not yet started (and a superweapon
    /// waiting for power) to an ally's frame of their type.
    pub(super) fn eco_watch_allied_projects(&mut self) {
        if self.allies.is_empty() {
            return;
        }
        self.eco.allied.census = (self.cb.friendly_units().into_iter())
            .filter(|&u| u.health() > 0.0)
            .filter_map(|u| {
                let name = self.defs.name_of(u);
                ASSISTED.iter().find(|&&n| n == name).map(|&n| (u, n))
            })
            .collect();
        let mut ended = Vec::new();
        let mut pending = Vec::new();
        for &id in &self.eco.construction_tasks {
            let Some(t) = self.eco.tasks.get(id) else { continue };
            let Some(c) = t.as_construction() else { continue };
            match c.target {
                Some(u) if u.team() != self.team_id => {
                    let gone = u.health() <= 0.0 || !self.allies.contains(&u.team());
                    if gone || !u.is_being_built() {
                        ended.push((id, gone, self.defs.name(c.build_type), t.position));
                    }
                }
                None if !c.fac_plop && ASSISTED.contains(&self.defs.name(c.build_type)) => {
                    pending.push((id, c.build_type, t.position));
                }
                _ => {}
            }
        }
        for (id, gone, name, p) in ended {
            self.log(&format!(
                "economy: the allied {name} we helped at ({:.0}, {:.0}) {}",
                p.x,
                p.z,
                if gone { "is gone" } else { "stands" }
            ));
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
                    self.eco.drop_construction_task(id);
                    self.eco.anti_nuke = false;
                    continue;
                }
                a.filter(|a| self.eco_ally_site_safe(a.pos()))
            } else {
                self.eco_ally_frame(def)
            };
            let Some(u) = u else { continue };
            if anti {
                self.eco.anti_nuke = false;
            }
            if self.eco_task_on(u).is_some() {
                // Another job of ours already helps it.
                self.eco.drop_construction_task(id);
                continue;
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
                }
            }
            self.eco.idle_workers(&idle);
        }
        if let Some((def, _)) = self.eco_awaiting()
            && ASSISTED.contains(&self.defs.name(def))
            && self.eco_ally_frame(def).is_some()
        {
            self.eco.pending_power = None;
            self.eco_assist_ally(def);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn notes_are_said_once_a_minute() {
        let mut p = AlliedProjects::default();
        assert!(p.say("a".into(), 0));
        assert!(!p.say("a".into(), SAY_EVERY - 1));
        assert!(p.say("b".into(), 10));
        assert!(p.say("a".into(), SAY_EVERY));
    }
}
