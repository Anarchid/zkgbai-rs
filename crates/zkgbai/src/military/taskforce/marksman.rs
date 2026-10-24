//! Marksmen: the cores of a doctrine with `marksman` (Lances, Phantoms).
//! They keep firing at will, at radar dots too (chaff shot at now and then
//! kills well enough), and each is set (ZK's Set Target, which holds at
//! fire at will and leaves the gun's orders alone) on the most valuable
//! target in its reach that one shot kills, among those whose place we know
//! exactly: in sight, or buildings, whose radar ghosts never lie. Two guns
//! never mark the same target.

use std::collections::BTreeSet;

use springai::{NO_TIMEOUT, Unit};

use crate::Ai;
use crate::defs::cmd;
use crate::military::SquadId;
use crate::util::{NULLPOS, distance};

/// Marks are worth setting from this share of the gun's own cost (below,
/// the engine's own pick is as good).
const MARK_WORTH: f32 = 0.1;

/// Reach beyond the weapon's range, so the gun turns to a mark coming in.
const REACH: f32 = 20.0;

impl Ai {
    /// Set each core gun of force `sid` on its best mark, or let it be.
    pub(crate) fn taskforce_mark(&mut self, sid: SquadId) {
        let Some(s) = self.squads.get(sid) else { return };
        let guns: Vec<(i32, Unit)> = (s.everyone().filter_map(|id| self.fighters.get(&id)))
            .filter(|f| s.shape.is_core(f.unit.def().map(|d| d.id())))
            .map(|f| (f.id, f.unit))
            .collect();
        if guns.is_empty() {
            return;
        }
        let fighters = &self.fighters;
        self.taskforces.marks.retain(|gun, _| fighters.contains_key(gun));
        let shields = self.impalers_shields();
        for (id, unit) in guns {
            let Some(w) = unit.weapon(0) else { continue };
            let damage = w.def().and_then(|d| d.damage_types().first().copied()).unwrap_or(0.0);
            let reach = w.range() + REACH;
            let floor = MARK_WORTH * unit.def().map_or(0.0, |d| d.cost(self.metal));
            let pos = unit.pos();
            let mine = self.taskforces.marks.get(&id).copied();
            let taken: BTreeSet<i32> =
                (self.taskforces.marks.iter()).filter(|(g, _)| **g != id).map(|(_, t)| *t).collect();
            let best = (self.war.targets.values())
                .filter(|e| e.identified && !e.is_dead && !e.can_fly() && e.position != NULLPOS)
                .filter(|e| (e.visible || e.is_static) && e.def_name != "wolverine_mine")
                .filter(|e| !taken.contains(&e.unit_id) && distance(pos, e.position) <= reach)
                .filter(|e| !shields.iter().any(|&(p, r)| distance(p, e.position) <= r + 10.0))
                .filter_map(|e| {
                    let health =
                        if e.visible && e.unit.health() > 0.0 { e.unit.health() } else { e.get_health() * e.def_health() };
                    // Armored when closed or still: count it so.
                    let per_shot = damage * e.ud.map_or(1.0, |d| d.armored_multiple()).min(1.0);
                    let worth = e.ud.map_or(e.value, |d| d.cost(self.metal));
                    (health > 0.0 && health <= per_shot && worth >= floor).then_some((e.unit_id, worth, health))
                })
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
                    self.taskforces.marks.insert(id, target);
                    let e = &self.war.targets[&target];
                    let line = format!(
                        "taskforce: a {} marks a {} ({worth:.0} metal, {health:.0} hp, {:.0} off){}",
                        self.defs.name_of(unit),
                        e.def_name,
                        distance(pos, e.position),
                        if e.visible { "" } else { ", out of sight" }
                    );
                    self.log(&line);
                }
                Some(_) => {}
                None => {
                    if self.taskforces.marks.remove(&id).is_some() {
                        unit.execute_custom_command(cmd::UNIT_CANCEL_TARGET, &[], 0, NO_TIMEOUT);
                    }
                }
            }
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
        let marked = tf.marks.get(&a.id()) == Some(&enemy.id());
        let in_force = self.fighters.get(&a.id()).and_then(|f| f.squad).is_some_and(|s| {
            self.squads
                .get(s)
                .is_some_and(|q| q.is_taskforce() && (q.shape.marksman || q.shape.aim == super::Aim::Siege))
        });
        let e = self.war.targets.get(&enemy.id());
        let cost = e.and_then(|e| e.ud).map_or(0.0, |d| d.cost(self.metal));
        let victim = e.map_or("?", |e| e.def_name.as_str()).to_owned();
        self.log(&format!(
            "taskforce: a {name} kills a {victim} ({cost:.0} metal), {}{}",
            if marked { "its mark" } else { "unmarked" },
            if in_force { "" } else { ", alone" }
        ));
    }
}
