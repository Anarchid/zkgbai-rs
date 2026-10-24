//! Marksmen: the cores of a doctrine with `marksman` (Lances, Phantoms).
//! They keep firing at will, at radar dots too (chaff shot at now and then
//! kills well enough), and each is set (ZK's Set Target, which holds at
//! fire at will and leaves the gun's orders alone) on the most valuable
//! target in its reach that one shot kills, among those whose place we know
//! exactly: in sight, or buildings, whose radar ghosts never lie. Two guns
//! never mark the same target.
//!
//! With the doctrine's `mark_hold`, a gun picks its mark only as it
//! reloads (from `PREAIM` before) and keeps it until it has fired, unless
//! the mark dies, is lost from sight, shielded, or drifts `KEEP` beyond
//! its range (or out of reach once the gun is loaded): so a volley goes at
//! the mark it was readied for. A mark is worth setting from `mark_share`
//! of what a volley of the gun's type has killed in this game (a fifth of
//! the gun's cost, for the first volleys): chaff below that the engine
//! picks as well.
//!
//! Each pass logs what the force's guns reach: `taskforce: the D force
//! (squad N, state) reaches K known, I identified, V visible` (known: with
//! a position, radar dots too; only when it is some).

use std::collections::{BTreeMap, BTreeSet};

use springai::{NO_TIMEOUT, Unit};

use crate::Ai;
use crate::defs::cmd;
use crate::military::{Enemy, SquadId};
use crate::util::{NULLPOS, distance};

/// Marks are worth setting from this share of the gun's own cost (below,
/// the engine's own pick is as good), without `mark_share`.
const MARK_WORTH: f32 = 0.1;

/// Until a gun type has killed, its volley is taken to kill this share of
/// its cost, weighing as so many volleys.
const PRIOR_KILL: f32 = 0.2;
const PRIOR_VOLLEYS: f32 = 5.0;

/// Kills by one gun this many frames apart are one volley.
const VOLLEY_FRAMES: i32 = 15;

/// Reach beyond the weapon's range, so the gun turns to a mark coming in.
const REACH: f32 = 20.0;

/// A held mark is kept this far beyond the weapon's range while the gun
/// reloads.
const KEEP: f32 = 150.0;

/// A holding gun picks its mark from this many frames before it reloads.
const PREAIM: i32 = 90;

/// A loaded gun that has not fired at its held mark in this many frames
/// (no line of fire, say) picks again.
const HOLD_LOADED: i32 = 240;

/// A gun's mark: the target, the frame it was set, and the gun's reload
/// frame then (another one means it has fired since).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Mark {
    pub target: i32,
    at: i32,
    reload: i32,
}

impl Mark {
    /// A mark set by a gun that is no marksman (a siege gun's target): the
    /// hold rules never read it.
    pub(crate) fn plain(target: i32, at: i32) -> Mark {
        Mark { target, at, reload: 0 }
    }
}

/// What volleys have killed, by gun type (def id): metal and volleys; and
/// each gun's last kill (frame).
#[derive(Default)]
pub struct Volleys {
    by_def: BTreeMap<i32, (f32, f32)>,
    last: BTreeMap<i32, i32>,
}

impl Volleys {
    /// What a volley of type `def` (costing `cost`) kills, so far.
    pub fn kill(&self, def: i32, cost: f32) -> f32 {
        let (metal, n) = self.by_def.get(&def).copied().unwrap_or_default();
        (metal + PRIOR_VOLLEYS * PRIOR_KILL * cost) / (n + PRIOR_VOLLEYS)
    }

    /// Gun `gun` of type `def` killed `metal` at `frame`.
    pub fn add(&mut self, gun: i32, def: i32, metal: f32, frame: i32) {
        let e = self.by_def.entry(def).or_default();
        e.0 += metal;
        if self.last.insert(gun, frame).is_none_or(|f| frame - f > VOLLEY_FRAMES) {
            e.1 += 1.0;
        }
    }
}

impl Ai {
    /// Set each core gun of force `sid` on its best mark, or let it be.
    pub(crate) fn taskforce_mark(&mut self, sid: SquadId) {
        let Some(s) = self.squads.get(sid) else { return };
        let (hold, share) = (s.shape.mark_hold, s.shape.mark_share);
        let guns: Vec<(i32, Unit)> = (s.everyone().filter_map(|id| self.fighters.get(&id)))
            .filter(|f| s.shape.is_core(f.unit.def().map(|d| d.id())))
            .map(|f| (f.id, f.unit))
            .collect();
        if guns.is_empty() {
            return;
        }
        let state = super::spotter::force_state(s.engaging, s.engaged);
        let doctrine = s.shape.doctrine.clone();
        let frame = self.frame;
        let fighters = &self.fighters;
        self.taskforces.marks.retain(|gun, _| fighters.contains_key(gun));
        let shields = self.impalers_shields();
        self.taskforce_log_reach(sid, &doctrine, state, &guns);
        let mut lines = Vec::new();
        for (id, unit) in guns {
            let Some(w) = unit.weapon(0) else { continue };
            let damage = w.def().and_then(|d| d.damage_types().first().copied()).unwrap_or(0.0);
            let range = w.range();
            let reload = w.reload_frame();
            let pos = unit.pos();
            let Some(ud) = unit.def() else { continue };
            let cost = ud.cost(self.metal);
            let floor =
                if share > 0.0 { share * self.taskforces.volleys.kill(ud.id(), cost) } else { MARK_WORTH * cost };
            let held = self.taskforces.marks.get(&id).copied();
            let taken: BTreeSet<i32> =
                (self.taskforces.marks.iter()).filter(|(g, _)| **g != id).map(|(_, m)| m.target).collect();
            // What one shot does to `e`: its worth and health if it kills
            // it and we know where it is exactly; else why not.
            let shot = |e: &Enemy| -> Result<(f32, f32), &'static str> {
                if e.is_dead || !e.identified || e.position == NULLPOS {
                    return Err("lost");
                }
                if !(e.visible || e.is_static) {
                    return Err("out of sight");
                }
                if shields.iter().any(|&(p, r)| distance(p, e.position) <= r + 10.0) {
                    return Err("shielded");
                }
                let health =
                    if e.visible && e.unit.health() > 0.0 { e.unit.health() } else { e.get_health() * e.def_health() };
                // Armored when closed or still: count it so.
                let per_shot = damage * e.ud.map_or(1.0, |d| d.armored_multiple()).min(1.0);
                if health <= 0.0 || health > per_shot {
                    return Err("no longer one shot");
                }
                Ok((e.ud.map_or(e.value, |d| d.cost(self.metal)), health))
            };
            if hold && let Some(m) = held {
                let fired = reload != m.reload;
                let e = self.war.targets.get(&m.target);
                let off = e.map_or(f32::INFINITY, |e| distance(pos, e.position));
                let why = match e.map(&shot) {
                    None | Some(Err("lost")) => Some(""),
                    Some(Err(why)) => Some(why),
                    Some(Ok(_)) if off > range + KEEP => Some("out of reach"),
                    Some(Ok(_)) => None,
                };
                if let Some(why) = why {
                    unit.execute_custom_command(cmd::UNIT_CANCEL_TARGET, &[], 0, NO_TIMEOUT);
                    self.taskforces.marks.remove(&id);
                    if let (false, Some(e)) = (why.is_empty(), e) {
                        let gun = self.defs.name_of(unit);
                        lines
                            .push(format!("taskforce: a {gun} drops its mark, a {} ({why}, {off:.0} off)", e.def_name));
                    }
                } else if !fired
                    && (reload > frame || (off <= range + REACH && frame - reload.max(m.at) <= HOLD_LOADED))
                {
                    // Held through the reload; a loaded gun lets go of a mark
                    // it cannot reach, or has not fired at for a while.
                    continue;
                }
            }
            // A holding gun picks only as it reloads.
            if hold && reload - frame > PREAIM {
                continue;
            }
            let mine = self.taskforces.marks.get(&id).map(|m| m.target);
            let reach = range + REACH;
            let best = (self.war.targets.values())
                .filter(|e| !e.can_fly() && e.def_name != "wolverine_mine")
                .filter(|e| !taken.contains(&e.unit_id) && distance(pos, e.position) <= reach)
                .filter_map(|e| shot(e).ok().filter(|&(worth, _)| worth >= floor).map(|(w, h)| (e.unit_id, w, h)))
                // Most worth; then the mark it has (no turning between equals);
                // then the one nearest dead.
                .max_by(|a, b| {
                    (a.1.total_cmp(&b.1))
                        .then_with(|| (Some(a.0) == mine).cmp(&(Some(b.0) == mine)))
                        .then_with(|| b.2.total_cmp(&a.2))
                });
            match best {
                Some((target, worth, health)) if Some(target) != mine => {
                    unit.execute_custom_command(cmd::UNIT_SET_TARGET, &[target as f32], 0, NO_TIMEOUT);
                    self.taskforces.marks.insert(id, Mark { target, at: frame, reload });
                    let e = &self.war.targets[&target];
                    lines.push(format!(
                        "taskforce: a {} marks a {} ({worth:.0} metal, {health:.0} hp, {:.0} off){}",
                        self.defs.name_of(unit),
                        e.def_name,
                        distance(pos, e.position),
                        if e.visible { "" } else { ", out of sight" }
                    ));
                }
                Some(_) => {
                    // The same mark again: held anew from this reload.
                    if let Some(m) = self.taskforces.marks.get_mut(&id) {
                        (m.at, m.reload) = (frame, reload);
                    }
                }
                None => {
                    if self.taskforces.marks.remove(&id).is_some() {
                        unit.execute_custom_command(cmd::UNIT_CANCEL_TARGET, &[], 0, NO_TIMEOUT);
                    }
                }
            }
        }
        for line in lines {
            self.log(&line);
        }
    }

    /// Log what the guns of force `sid` reach: enemies with a position,
    /// identified, visible (aircraft aside).
    fn taskforce_log_reach(&mut self, sid: SquadId, doctrine: &str, state: &str, guns: &[(i32, Unit)]) {
        let reach: Vec<(springai::Vec3, f32)> =
            guns.iter().filter_map(|(_, u)| u.weapon(0).map(|w| (u.pos(), w.range() + REACH))).collect();
        let (mut known, mut identified, mut visible) = (0, 0, 0);
        for e in self.war.targets.values().filter(|e| !e.is_dead && e.position != NULLPOS && !e.can_fly()) {
            if !reach.iter().any(|&(p, r)| distance(p, e.position) <= r) {
                continue;
            }
            known += 1;
            identified += usize::from(e.identified);
            visible += usize::from(e.visible);
        }
        if known > 0 {
            self.log(&format!(
                "taskforce: the {doctrine} force (squad {sid}, {state}) reaches {known} known, {identified} identified, \
                 {visible} visible"
            ));
        }
    }

    /// Log a kill by a gun some marksman or siege doctrine takes for a core
    /// (on or off, in a force or not, so the arms of an A/B count alike):
    /// whether it was its mark (a siege gun's set target).
    pub fn taskforce_enemy_destroyed(&mut self, enemy: Unit, attacker: Option<Unit>) {
        let Some(a) = attacker.filter(|a| a.team() == self.team_id) else { return };
        let name = self.defs.name_of(a).to_owned();
        let tf = &self.taskforces;
        let gun = |d: &super::Doctrine| (d.marksman || d.aim == super::Aim::Siege) && d.is_core(&name, &tf.classes);
        if !tf.doctrines.iter().any(gun) {
            return;
        }
        let marked = tf.marks.get(&a.id()).is_some_and(|m| m.target == enemy.id());
        let in_force = self.fighters.get(&a.id()).and_then(|f| f.squad).is_some_and(|s| {
            self.squads
                .get(s)
                .is_some_and(|q| q.is_taskforce() && (q.shape.marksman || q.shape.aim == super::Aim::Siege))
        });
        let e = self.war.targets.get(&enemy.id());
        let cost = e.and_then(|e| e.ud).map_or(0.0, |d| d.cost(self.metal));
        let victim = e.map_or("?", |e| e.def_name.as_str()).to_owned();
        if let Some(def) = a.def() {
            let frame = self.frame;
            self.taskforces.volleys.add(a.id(), def.id(), cost, frame);
        }
        self.log(&format!(
            "taskforce: a {name} kills a {victim} ({cost:.0} metal), {}{}",
            if marked { "its mark" } else { "unmarked" },
            if in_force { "" } else { ", alone" }
        ));
    }
}
