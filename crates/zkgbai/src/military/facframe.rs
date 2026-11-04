//! Units fighting at an enemy factory shoot the factory, not its frames
//! (switch `facframe`).
//!
//! An attack-move takes the enemy nearest the unit's line (Recoil
//! `CMobileCAI::ExecuteFight` -> `GetClosestValidTarget`: 2-D distance to
//! the target's middle, nothing else weighed) and holds it as a temporary
//! attack until it dies. A factory's frame stands on its build pad, nearer
//! than the factory's middle from the yard side, so the fight takes the
//! frame; it dies in a shot or two, the factory starts the next one, and
//! the fight takes that: the factory (4000 hp in ZK) never dies. A killed
//! factory kills its frame with it (`CFactory::KillUnit`).
//!
//! Every [`PERIOD`] frames, a unit of ours whose first order is such an
//! engine-made attack (internal, or ahead of the fight it came from) on a
//! frame under [`KEEP_FRAME`] built inside a visible enemy factory turns to
//! the nearest armed enemy within its reach that it can hit and that can
//! hit it, or, with none, to the factory: an attack inserted at the front of
//! its queue, the frame's attack removed. A unit so turned to a factory
//! turns again to an armed enemy that comes within its reach. When its
//! attack ends (the factory dead, the unit leashed back), the fight it came
//! from goes on. Off (`disable=facframe`) it only counts.
//!
//! A turned unit is this module's (`springai::reserved::own`) while its
//! attack lasts, at most [`HOLD_MAX`]: handlers re-issue their fights on a
//! clock (loners every 3 s), each new fight takes the frame again, and a
//! slow gun fired its one shot at the frame in the moment before the next
//! turn (Adamantine Mountain 11-04: a Cyclops cannon killed Venom frames
//! for minutes while its beam held the factory). The retreat handler and
//! the helpers every ground unit keeps still order it; its owner before is
//! restored when the hold ends.

use std::collections::{BTreeMap, BTreeSet};

use springai::reserved::{self, Owner};
use springai::{Unit, UnitDef, Vec3, opt};

use crate::Ai;
use crate::military::water::is_fake;
use crate::util::distance;

const CMD_INSERT: i32 = 1;
const CMD_REMOVE: i32 = 2;
const CMD_FIGHT: i32 = 16;
const CMD_ATTACK: i32 = 20;

const PERIOD: i32 = 10;
/// Frames this far built are left to the fight: about to come out, armed,
/// and most of their cost is sunk in them.
const KEEP_FRAME: f32 = 0.85;
/// A unit's reach: its range and the engine's fight leash at manoeuvre
/// (100 * move state squared).
const REACH_SLACK: f32 = 100.0;
/// Our units this far from a factory with a frame are looked at (the
/// longest ranges plus their leash).
const SCAN: f32 = 2000.0;
/// A frame stands inside its factory's footprint: within half its longer
/// side (8 elmos a square) and this slack of the factory's middle.
const PAD_SLACK: f32 = 16.0;
/// Each factory's turns are logged at most this often (frames).
const LOG_EVERY: i32 = 600;
/// A turned unit is held at most this long (frames); then its handlers
/// order it again, and a fight that takes a frame is turned anew.
const HOLD_MAX: i32 = 900;
/// A unit lent away when its hold ended is waited for this long (frames).
const LENT_MAX: i32 = 5400;
/// The owner of a turned unit: this module, the retreat handler, those that
/// move every ground unit (jumps, teleport, water, nuke blasts) and the
/// Cyclops drive into cannon reach.
const OWNER: Owner = Owner {
    module: "facframe",
    helpers: &["retreat", "war.unitDamaged", "jumps", "teleport", "water", "blasts", "cyclops"],
};

/// A unit turned: what it attacks, the factory if that is one, its owner
/// before, and since when.
#[derive(Clone, Copy)]
struct Hold {
    target: i32,
    factory: Option<i32>,
    prev: Option<Owner>,
    since: i32,
}

/// A frame inside an enemy factory.
#[derive(Clone, Copy)]
struct Frame {
    factory: i32,
    progress: f32,
}

/// A unit's first order, as far as this module cares.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Front {
    /// The attacked unit, if the first order is an attack on a unit.
    target: Option<i32>,
    /// Whether the engine made it: internal, or ahead of a fight.
    engine_made: bool,
}

/// What a unit does about its attack.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Turn {
    Keep,
    ToFactory(i32),
    ToFighter(i32),
}

/// The turn for a unit whose first order is `front`: `frame` is what it
/// attacks if that is a frame in a factory, `turned` the factory we turned
/// it to before, `fighter` the nearest armed enemy in its reach, and
/// `hits_factory` whether its weapons can hit the frame's factory.
fn turn(front: Front, frame: Option<Frame>, turned: Option<i32>, fighter: Option<i32>, hits_factory: bool) -> Turn {
    let Some(target) = front.target else { return Turn::Keep };
    if let Some(f) = frame.filter(|f| front.engine_made && f.progress < KEEP_FRAME) {
        return match fighter {
            Some(e) => Turn::ToFighter(e),
            None if hits_factory => Turn::ToFactory(f.factory),
            None => Turn::Keep,
        };
    }
    match (turned, fighter) {
        (Some(fac), Some(e)) if fac == target => Turn::ToFighter(e),
        _ => Turn::Keep,
    }
}

/// Whether a frame at `at` stands inside a factory of footprint
/// `x_size` by `z_size` squares at `fac`.
fn on_pad(at: Vec3, fac: Vec3, x_size: i32, z_size: i32) -> bool {
    distance(at, fac) <= x_size.max(z_size) as f32 * 4.0 + PAD_SLACK
}

#[derive(Default)]
pub struct FacFrames {
    /// Our units turned, while their attack lasts.
    held: BTreeMap<i32, Hold>,
    /// Units whose hold ended while another module had borrowed them (a
    /// stayveto stand): the owner they go back to if returned to us, and
    /// since when.
    lent: BTreeMap<i32, (Option<Owner>, i32)>,
    /// Whether a shooter def can hurt a target def.
    hits: BTreeMap<(i32, i32), bool>,
    /// Whether a def is a factory: a static builder with build options.
    factory_defs: BTreeMap<i32, bool>,
    /// When each factory's turns were last logged.
    logged: BTreeMap<i32, i32>,
    /// Since the summary: the (unit, frame) attacks counted, so that one
    /// held over several checks counts once (with the switch off too).
    counted: BTreeSet<(i32, i32)>,
    /// Since the summary: engine attacks on frames found, turns to the
    /// factory, turns to an armed enemy (from a frame or from the
    /// factory), and turned-to factories that died.
    seen: u32,
    to_factory: u32,
    to_fighter: u32,
    from_factory: u32,
    killed: u32,
    /// Turns whose order was refused.
    refused: u32,
    /// Holds that ran out ([`HOLD_MAX`]).
    expired: u32,
}

impl Ai {
    /// Hold `id`, turned to `target` (a factory if `factory`).
    fn facframe_hold(&mut self, id: i32, target: i32, factory: Option<i32>, frame: i32) {
        let prev = match self.facframes.held.get(&id) {
            Some(h) => h.prev,
            None => reserved::owner(self.cb, id).filter(|&o| o != OWNER),
        };
        reserved::own(self.cb, id, OWNER);
        self.facframes.held.insert(id, Hold { target, factory, prev, since: frame });
    }

    /// End `id`'s hold: its owner before gets it back; if another module
    /// borrowed it meanwhile, once that module gives it back to us.
    fn facframe_release(&mut self, id: i32) {
        let Some(h) = self.facframes.held.remove(&id) else { return };
        if reserved::owner(self.cb, id) == Some(OWNER) {
            self.facframe_restore(id, h.prev);
        } else {
            self.facframes.lent.insert(id, (h.prev, self.frame));
        }
    }

    /// A unit of ours died: no owner comes back to its id.
    pub fn facframe_unit_destroyed(&mut self, id: i32) {
        self.facframes.lent.remove(&id);
        if self.facframes.held.remove(&id).is_some() && reserved::owner(self.cb, id) == Some(OWNER) {
            reserved::disown(self.cb, id);
        }
    }

    fn facframe_restore(&mut self, id: i32, prev: Option<Owner>) {
        match prev {
            Some(o) => {
                reserved::own(self.cb, id, o);
            }
            None => {
                reserved::disown(self.cb, id);
            }
        }
    }

    /// Units lent away when their hold ended: returned to us, they go to
    /// their owner before; dead, or someone else's a long while, forgotten.
    fn facframe_returns(&mut self, frame: i32) {
        let lent: Vec<(i32, Option<Owner>, i32)> =
            self.facframes.lent.iter().map(|(&id, &(prev, since))| (id, prev, since)).collect();
        for (id, prev, since) in lent {
            let alive = Unit::from_id(self.cb, id).is_some_and(|u| u.health() > 0.0);
            let ours = reserved::owner(self.cb, id) == Some(OWNER);
            if ours && !self.facframes.held.contains_key(&id) {
                self.facframe_restore(id, prev);
            }
            if ours || !alive || frame - since > LENT_MAX {
                self.facframes.lent.remove(&id);
            }
        }
    }

    /// Whether `shooter` has a weapon that damages `target`.
    fn facframe_hits(&mut self, shooter: UnitDef, target: UnitDef) -> bool {
        let key = (shooter.id(), target.id());
        if let Some(&h) = self.facframes.hits.get(&key) {
            return h;
        }
        let category = target.category();
        let h = category != 0
            && shooter.weapon_mounts().iter().any(|m| {
                m.only_target_category() & category != 0
                    && m.weapon_def().is_some_and(|w| {
                        !w.is_shield()
                            && !w.is_manual_fire()
                            && !is_fake(&w.name())
                            && w.damage_types().iter().any(|d| *d > 0.0)
                    })
            });
        self.facframes.hits.insert(key, h);
        h
    }

    /// The nearest enemy within `unit`'s reach that it can hit and that can
    /// hit it, frames aside.
    fn facframe_fighter(&mut self, unit: Unit, def: UnitDef) -> Option<i32> {
        let at = unit.pos();
        let reach = unit.max_range() + REACH_SLACK;
        let mut best: Option<(f32, i32)> = None;
        for e in self.cb.enemy_units_in_cylinder(at, reach) {
            if e.health() <= 0.0 || e.is_being_built() {
                continue;
            }
            let Some(edef) = e.def() else { continue };
            let d = distance(at, e.pos());
            if best.is_some_and(|(bd, _)| bd <= d) {
                continue;
            }
            if self.facframe_hits(def, edef) && self.facframe_hits(edef, def) {
                best = Some((d, e.id()));
            }
        }
        best.map(|(_, id)| id)
    }

    pub fn facframe_update(&mut self, frame: i32) {
        if frame % 1800 == (self.offset + 17) % 1800 && self.facframes.seen + self.facframes.from_factory > 0 {
            let f = &self.facframes;
            self.log(&format!(
                "facframe: in the last minute {} engine attacks on factory frames, turned {} to the factory and {} \
                 to an armed enemy, {} more from the factory to an armed enemy; {} turned-to factories died; {} \
                 turns refused; {} holds ran out{}",
                f.seen,
                f.to_factory,
                f.to_fighter,
                f.from_factory,
                f.killed,
                f.refused,
                f.expired,
                if self.dev_off("facframe") { " (facframe off)" } else { "" }
            ));
            let f = &mut self.facframes;
            (f.seen, f.to_factory, f.to_fighter, f.from_factory, f.killed, f.refused, f.expired) =
                (0, 0, 0, 0, 0, 0, 0);
            f.counted.clear();
        }
        if frame % PERIOD != (self.offset + 4) % PERIOD {
            return;
        }
        let off = self.dev_off("facframe");
        if !self.facframes.lent.is_empty() {
            self.facframe_returns(frame);
        }

        // Turned-to targets dead (the unit's own attack ends then too), and
        // holds run out.
        let gone: Vec<(i32, Option<i32>, bool)> = (self.facframes.held.iter())
            .filter_map(|(&u, h)| {
                let dead = self.war.targets.get(&h.target).is_none_or(|e| e.is_dead);
                (dead || frame - h.since > HOLD_MAX).then_some((u, h.factory.filter(|_| dead), dead))
            })
            .collect();
        for (u, fac, dead) in gone {
            self.facframe_release(u);
            if !dead {
                self.facframes.expired += 1;
            }
            if let Some(fac) = fac
                && !self.facframes.held.values().any(|h| h.factory == Some(fac))
            {
                self.facframes.killed += 1;
            }
        }

        // Visible enemy factories, and the frames in them.
        let seen: Vec<(i32, Unit, UnitDef)> = (self.war.targets.values())
            .filter(|e| e.visible && !e.is_dead && e.is_static)
            .filter_map(|e| Some((e.unit_id, e.unit, e.ud?)))
            .collect();
        let mut factories: Vec<(i32, Unit, Vec3, UnitDef)> = Vec::new();
        for (id, u, def) in seen {
            let is_factory = *(self.facframes.factory_defs.entry(def.id()))
                .or_insert_with(|| def.is_builder() && def.speed() <= 0.0 && !def.build_options().is_empty());
            if is_factory && u.health() > 0.0 && !u.is_being_built() {
                factories.push((id, u, u.pos(), def));
            }
        }
        let mut frames: BTreeMap<i32, Frame> = BTreeMap::new();
        let mut busy: Vec<(i32, Vec3)> = Vec::new();
        for &(fid, _, at, def) in &factories {
            let r = def.x_size().max(def.z_size()) as f32 * 4.0 + PAD_SLACK;
            let mut any = false;
            for u in self.cb.enemy_units_in_cylinder(at, r) {
                if !u.is_being_built() || u.health() <= 0.0 {
                    continue;
                }
                let Some(d) = u.def() else { continue };
                if d.speed() <= 0.0 || !on_pad(u.pos(), at, def.x_size(), def.z_size()) {
                    continue;
                }
                frames.insert(u.id(), Frame { factory: fid, progress: u.build_progress() });
                any = true;
            }
            if any {
                busy.push((fid, at));
            }
        }
        if busy.is_empty() && self.facframes.held.is_empty() {
            return;
        }

        // Our units near a busy factory, and those turned to one.
        let team = self.team_id;
        let mut units: BTreeMap<i32, Unit> = BTreeMap::new();
        for &(_, at) in &busy {
            for u in self.cb.friendly_units_in_cylinder(at, SCAN) {
                if u.team() == team {
                    units.insert(u.id(), u);
                }
            }
        }
        for &id in self.facframes.held.keys() {
            if let Some(u) = Unit::from_id(self.cb, id) {
                units.insert(id, u);
            }
        }

        let fac_defs: BTreeMap<i32, (UnitDef, Vec3)> =
            factories.iter().map(|&(id, _, at, def)| (id, (def, at))).collect();
        let mut turns: BTreeMap<i32, (u32, u32, f32, String)> = BTreeMap::new();
        for (id, unit) in units {
            if unit.health() <= 0.0 || unit.is_being_built() {
                self.facframe_release(id);
                continue;
            }
            let Some(def) = unit.def() else { continue };
            let queue = unit.current_commands();
            let Some(first) = queue.first() else {
                self.facframe_release(id);
                continue;
            };
            let params = first.params();
            let target = (first.id() == CMD_ATTACK && params.len() == 1).then(|| params[0] as i32);
            let engine_made =
                first.options() & opt::INTERNAL_ORDER != 0 || queue.get(1).is_some_and(|c| c.id() == CMD_FIGHT);
            let front = Front { target, engine_made };
            let held = self.facframes.held.get(&id).copied();
            if held.is_some_and(|h| Some(h.target) != target) {
                // A helper gave it other orders (a retreat, say), or the
                // attack ended.
                self.facframe_release(id);
            }
            // The factory it was turned to, if it still attacks it.
            let turned = held.filter(|h| Some(h.target) == target).and_then(|h| h.factory);
            let on_frame = target.and_then(|t| frames.get(&t)).copied();
            let young = on_frame.filter(|f| engine_made && f.progress < KEEP_FRAME);
            if young.is_none() && turned.is_none_or(|f| Some(f) != target) {
                continue;
            }
            // Whether this attack counts in the summary (first seen).
            let new = young.is_none() || self.facframes.counted.insert((id, target.unwrap_or(-1)));
            if young.is_some() && new {
                self.facframes.seen += 1;
            }
            let fighter = self.facframe_fighter(unit, def);
            let hits_factory =
                on_frame.and_then(|f| fac_defs.get(&f.factory)).is_some_and(|&(fdef, _)| self.facframe_hits(def, fdef));
            let t = turn(front, on_frame, turned, fighter, hits_factory);
            let to = match t {
                Turn::Keep => continue,
                Turn::ToFactory(f) | Turn::ToFighter(f) => f,
            };
            // The front attack goes, and frame attacks queued right behind
            // it (a turn that raced a new frame's attack).
            let mut drop = vec![first.tag() as f32];
            for c in queue.iter().skip(1) {
                let p = c.params();
                if c.id() != CMD_ATTACK || p.len() != 1 || !frames.contains_key(&(p[0] as i32)) {
                    break;
                }
                drop.push(c.tag() as f32);
            }
            // Refused: the unit is another module's alone (see
            // `roles`, which names this module a helper of each role), or
            // held by the operator.
            if !off
                && !unit.execute_custom_command(
                    CMD_INSERT,
                    &[0.0, CMD_ATTACK as f32, 0.0, to as f32],
                    opt::ALT_KEY,
                    springai::NO_TIMEOUT,
                )
            {
                self.facframes.refused += u32::from(new);
                continue;
            }
            if !off {
                unit.execute_custom_command(CMD_REMOVE, &drop, 0, springai::NO_TIMEOUT);
            }
            let fac = on_frame.map(|f| f.factory).or(turned).unwrap_or(-1);
            let entry = turns.entry(fac).or_insert((0, 0, on_frame.map_or(-1.0, |f| f.progress), String::new()));
            if entry.3.is_empty() {
                entry.3 = self.defs.name(def).to_owned();
            }
            match t {
                Turn::ToFactory(f) => {
                    self.facframes.to_factory += u32::from(new);
                    entry.0 += 1;
                    if !off {
                        self.facframe_hold(id, f, Some(f), frame);
                    }
                }
                Turn::ToFighter(e) => {
                    if on_frame.is_some() {
                        self.facframes.to_fighter += u32::from(new);
                    } else {
                        self.facframes.from_factory += 1;
                    }
                    entry.1 += 1;
                    if !off {
                        self.facframe_hold(id, e, None, frame);
                    }
                }
                Turn::Keep => {}
            }
        }
        for (fac, (n_fac, n_fighter, progress, shooter)) in turns {
            if self.facframes.logged.get(&fac).is_some_and(|&t| frame - t < LOG_EVERY) {
                continue;
            }
            self.facframes.logged.insert(fac, frame);
            let (name, at) = fac_defs
                .get(&fac)
                .map_or(("?".to_owned(), Vec3::xz(0.0, 0.0)), |&(d, at)| (self.defs.name(d).to_owned(), at));
            let frame_note =
                if progress >= 0.0 { format!(" (a frame {:.0}% built)", progress * 100.0) } else { String::new() };
            self.log(&format!(
                "facframe: {name} {fac} at ({:.0}, {:.0}){frame_note}: {n_fac} turned to the factory, {n_fighter} to an \
                 armed enemy (first a {shooter}){}",
                at.x,
                at.z,
                if off { " (facframe off: not ordered)" } else { "" }
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FRAME: Option<Frame> = Some(Frame { factory: 7, progress: 0.3 });

    fn front(target: i32, engine_made: bool) -> Front {
        Front { target: Some(target), engine_made }
    }

    #[test]
    fn an_engine_attack_on_a_young_frame_turns_to_the_factory() {
        assert_eq!(turn(front(50, true), FRAME, None, None, true), Turn::ToFactory(7));
    }

    #[test]
    fn an_armed_enemy_in_reach_comes_before_the_factory() {
        assert_eq!(turn(front(50, true), FRAME, None, Some(9), true), Turn::ToFighter(9));
    }

    #[test]
    fn a_frame_about_to_come_out_is_left_to_the_fight() {
        let old = Some(Frame { factory: 7, progress: 0.9 });
        assert_eq!(turn(front(50, true), old, None, None, true), Turn::Keep);
    }

    #[test]
    fn our_own_attack_orders_on_frames_stand() {
        assert_eq!(turn(front(50, false), FRAME, None, None, true), Turn::Keep);
    }

    #[test]
    fn a_unit_that_cannot_hit_the_factory_keeps_its_frame() {
        assert_eq!(turn(front(50, true), FRAME, None, None, false), Turn::Keep);
    }

    #[test]
    fn a_unit_turned_to_a_factory_turns_to_an_armed_enemy_in_reach() {
        assert_eq!(turn(front(7, false), None, Some(7), Some(9), true), Turn::ToFighter(9));
        assert_eq!(turn(front(7, false), None, Some(7), None, true), Turn::Keep);
    }

    #[test]
    fn a_factory_attack_we_did_not_give_stands() {
        assert_eq!(turn(front(7, true), None, None, Some(9), true), Turn::Keep);
    }

    #[test]
    fn frames_count_inside_the_footprint_only() {
        let fac = Vec3::xz(1000.0, 1000.0);
        // factorycloak: 7 by 10 squares, half the longer side 40 elmos.
        assert!(on_pad(Vec3::xz(1000.0, 1030.0), fac, 7, 10));
        assert!(!on_pad(Vec3::xz(1000.0, 1080.0), fac, 7, 10));
    }
}
