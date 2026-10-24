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
//! With `losfire`, a gun reserves its shot while a target worth its floor
//! that one shot kills is known within `RESERVE` beyond its range (it may
//! drift or be moved into reach over the reload): it holds fire and marks
//! only what is in sight (a ghost will not do); with only chaff about, it
//! fires at will. The changes are logged: `a G reserves: N deserving known
//! within R` and `a G fires at will: only chaff about`. A spotter's errand (see
//! `spotter`) may bring a better target into sight: a held mark then
//! gives way to it.
//!
//! With `mark_wait_ratio` k (marksman v3, from the regret pre-read
//! impalers/ab16/REGRET.md), a loaded gun weighs what it could hit over
//! the next `mark_hold_for` seconds, not only what is in reach now: a
//! markable target (in sight or a building, unshielded, one shot kills it)
//! that its track (or a gun move of up to `mark_move`, the gun leading its
//! force so the escort comes) brings into reach by then, worth k times the
//! best in reach, makes the gun hold its fire on it (Set Target) until it
//! fires or the time is up. Worth then counts what the target makes or
//! enables over `STREAM_SECONDS` (commanders, factories, constructors,
//! mexes). Logged: `a G holds for a X (W metal, k R, H s) over a Y (V
//! metal)`, `a G moves M for a X`, `a G fires at a X after S s`, `a G hold
//! abandoned (why)`.
//!
//! Every volley of a force's core is logged as it is fired (the gun's
//! reload frame moves on, polled each frame): `taskforce: a G fires at a X
//! (in sight|on radar), D off, its mark[, spotter R from it]`, or `a G
//! fires at will` (the engine's own pick, which the AI cannot see).
//!
//! Each pass logs what the force's guns reach: `taskforce: the D force
//! (squad N, state) reaches K known, I identified, V visible, M markable`
//! (known: with a position, radar dots too; markable: identified and in
//! sight, or a building; only when some is known).

use std::collections::{BTreeMap, BTreeSet};

use springai::{NO_TIMEOUT, Unit, Vec3};

use crate::Ai;
use crate::defs::cmd;
use crate::kgbutil::kgb_move_to;
use crate::military::{Enemy, SquadId};
use crate::util::{NULLPOS, distance, get_directional_point};

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

/// With `losfire`, deserving targets this far beyond a gun's range keep it
/// in reserve (a drift and a short gun move over the reload).
const RESERVE: f32 = 300.0;

/// Marksman v3: what a target makes or enables counts over this long.
const STREAM_SECONDS: f32 = 60.0;

/// Marksman v3: seconds a gun needs to aim once its target is in reach.
const AIM_SECONDS: f32 = 1.0;

/// A holding gun picks its mark from this many frames before it reloads.
const PREAIM: i32 = 90;

/// A loaded gun that has not fired at its held mark in this many frames
/// (no line of fire, say) picks again.
const HOLD_LOADED: i32 = 240;

/// How exactly a gun must know where a target is to mark it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fix {
    /// In sight, or a building (its ghost is exact).
    Exact,
    /// In sight only (`losfire`).
    Sight,
    /// Anywhere we know of, radar dots of a type we have seen too (what a
    /// spotter may bring into sight).
    Radar,
}

/// A gun's mark: the target, the frame it was set, and the gun's reload
/// frame then (another one means it has fired since).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Mark {
    pub target: i32,
    at: i32,
    reload: i32,
}

/// What volleys have killed, by gun type (def id): metal and volleys; each
/// gun's last kill (frame), and its reload frame when last polled.
#[derive(Default)]
pub struct Volleys {
    by_def: BTreeMap<i32, (f32, f32)>,
    last: BTreeMap<i32, i32>,
    reload: BTreeMap<i32, i32>,
    /// With `losfire`: each gun in reserve (holding fire) or not.
    reserve: BTreeMap<i32, bool>,
    /// Marksman v3: each gun's hold, and the reload frame of its last hold
    /// (one hold per reload).
    holds: BTreeMap<i32, Hold>,
    held_at: BTreeMap<i32, i32>,
}

/// A loaded gun holding its fire for a better target (marksman v3).
#[derive(Clone, Copy, Debug)]
struct Hold {
    target: i32,
    start: i32,
    /// It moved to get it.
    moved: bool,
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
    /// What one shot of `damage` does to `e`: its worth and health if it
    /// kills it and we know where it is as `fix` asks; else why not.
    pub(crate) fn marksman_shot(
        &self,
        e: &Enemy,
        damage: f32,
        shields: &[(Vec3, f32)],
        fix: Fix,
    ) -> Result<(f32, f32), &'static str> {
        if e.is_dead || !e.identified || e.position == NULLPOS {
            return Err("lost");
        }
        let known = match fix {
            Fix::Exact => e.visible || e.is_static,
            Fix::Sight => e.visible,
            Fix::Radar => true,
        };
        if !known {
            return Err("out of sight");
        }
        if shields.iter().any(|&(p, r)| distance(p, e.position) <= r + 10.0) {
            return Err("shielded");
        }
        let health = if e.visible && e.unit.health() > 0.0 { e.unit.health() } else { e.get_health() * e.def_health() };
        // Armored when closed or still: count it so.
        let per_shot = damage * e.ud.map_or(1.0, |d| d.armored_multiple()).min(1.0);
        if health <= 0.0 || health > per_shot {
            return Err("no longer one shot");
        }
        Ok((e.ud.map_or(e.value, |d| d.cost(self.metal)), health))
    }

    /// What `e` makes or enables, metal a second (marksman v3).
    fn marksman_stream(&self, e: &Enemy) -> f32 {
        let Some(d) = e.ud else { return 0.0 };
        if e.is_com {
            4.0
        } else if e.is_mex {
            2.0
        } else if d.speed() == 0.0 && !d.build_options().is_empty() || e.def_name == "staticcon" {
            10.0
        } else if e.is_worker {
            d.build_speed()
        } else {
            0.0
        }
    }

    /// A gun's floor: what a mark must be worth (see the module notes).
    pub(crate) fn marksman_floor(&self, gun: Unit, share: f32) -> f32 {
        let Some(ud) = gun.def() else { return f32::INFINITY };
        let cost = ud.cost(self.metal);
        if share > 0.0 { share * self.taskforces.volleys.kill(ud.id(), cost) } else { MARK_WORTH * cost }
    }

    /// Each frame: log every core volley of the marksmen's forces as it is
    /// fired (see the module notes).
    pub(crate) fn taskforce_watch_volleys(&mut self) {
        let frame = self.frame;
        let mut guns = Vec::new();
        for &sid in &self.squad_handler.taskforces {
            let Some(s) = self.squads.get(sid).filter(|s| s.shape.marksman) else { continue };
            for f in s.everyone().filter_map(|id| self.fighters.get(&id)) {
                if s.shape.is_core(f.unit.def().map(|d| d.id())) {
                    guns.push((f.id, f.unit));
                }
            }
        }
        let mut lines = Vec::new();
        for (id, unit) in guns {
            let Some(w) = unit.weapon(0) else { continue };
            let reload = w.reload_frame();
            let last = self.taskforces.volleys.reload.insert(id, reload);
            if last.is_none_or(|l| l == reload) || reload <= frame {
                continue;
            }
            let gun = self.defs.name_of(unit);
            if let Some(h) = self.taskforces.volleys.holds.remove(&id) {
                let e = self.war.targets.get(&h.target);
                lines.push(format!(
                    "taskforce: a {gun} fires at a {} after {:.1} s",
                    e.map_or("?", |e| e.def_name.as_str()),
                    (frame - h.start) as f32 / 30.0
                ));
                let reserve = self.taskforces.volleys.reserve.get(&id).copied().unwrap_or(false);
                unit.set_fire_state(if reserve { 0 } else { 2 }, 0);
            }
            let Some(e) = self.taskforces.marks.get(&id).and_then(|m| self.war.targets.get(&m.target)) else {
                lines.push(format!("taskforce: a {gun} fires at will"));
                continue;
            };
            let at = e.unit.pos();
            let at = if at == NULLPOS { e.position } else { at };
            let seen = if self.los.is_in_los(at) { "in sight" } else { "on radar" };
            let spotter = (self.taskforces.spotters.by_unit.values())
                .find(|sp| sp.serves(id, e.unit_id))
                .and_then(|sp| self.fighters.get(&sp.unit))
                .map_or(String::new(), |f| format!(", spotter {:.0} from it", distance(f.unit.pos(), at)));
            lines.push(format!(
                "taskforce: a {gun} fires at a {} ({seen}), {:.0} off, its mark{spotter}",
                e.def_name,
                distance(unit.pos(), at)
            ));
        }
        for line in lines {
            self.log(&line);
        }
    }

    /// Marksman v3 for gun `id`: keep or end its hold, or, loaded, start
    /// one (see the module notes). True: it holds (no other mark this pass).
    #[allow(clippy::too_many_arguments)]
    fn marksman_wait(
        &mut self,
        id: i32,
        unit: Unit,
        k: f32,
        hold_for: f32,
        max_move: f32,
        leads: bool,
        fix: Fix,
        floor: f32,
        lines: &mut Vec<String>,
    ) -> bool {
        let frame = self.frame;
        let Some(w) = unit.weapon(0) else { return false };
        let damage = w.def().and_then(|d| d.damage_types().first().copied()).unwrap_or(0.0);
        let (range, reload, pos) = (w.range(), w.reload_frame(), unit.pos());
        let gun = self.defs.name_of(unit).to_owned();
        let shields = self.impalers_shields();
        let worth = |ai: &Ai, e: &Enemy| -> Option<f32> {
            let (w, _) = ai.marksman_shot(e, damage, &shields, fix).ok()?;
            Some(w + STREAM_SECONDS * ai.marksman_stream(e))
        };
        if let Some(h) = self.taskforces.volleys.holds.get(&id).copied() {
            let e = self.war.targets.get(&h.target);
            let why = match e {
                None => Some("lost"),
                Some(e) if worth(self, e).is_none() => Some("not markable"),
                Some(_) if frame - h.start > (30.0 * hold_for) as i32 => Some("timeout"),
                _ => None,
            };
            if let Some(why) = why {
                self.taskforces.volleys.holds.remove(&id);
                let reserve = self.taskforces.volleys.reserve.get(&id).copied().unwrap_or(false);
                unit.set_fire_state(if reserve { 0 } else { 2 }, 0);
                lines.push(format!(
                    "taskforce: a {gun} hold abandoned ({why}) on a {}",
                    e.map_or("?", |e| e.def_name.as_str())
                ));
                return false;
            }
            if h.moved
                && let Some(e) = e
            {
                let d = distance(pos, e.position);
                if d > range {
                    kgb_move_to(
                        unit,
                        get_directional_point(pos, e.position, (d - range + 30.0).min(max_move)),
                        0,
                        frame,
                    );
                }
            }
            return true;
        }
        if reload > frame || self.taskforces.volleys.held_at.get(&id) == Some(&reload) {
            return false;
        }
        // Loaded: the best in reach now, and the best hittable within the hold.
        let reach = range + REACH;
        let taken: BTreeSet<i32> =
            (self.taskforces.marks.iter()).filter(|(g, _)| **g != id).map(|(_, m)| m.target).collect();
        let steps = hold_for.max(0.0).ceil() as i32;
        let mut now: Option<(i32, f32)> = None;
        let mut soon: Option<(i32, f32, bool)> = None;
        for e in self.war.targets.values().filter(|e| !e.can_fly() && !taken.contains(&e.unit_id)) {
            let Some(v) = worth(self, e) else { continue };
            let d = distance(pos, e.position);
            if d <= reach {
                if now.is_none_or(|n| v > n.1) {
                    now = Some((e.unit_id, v));
                }
                continue;
            }
            let vel = if e.visible && !e.is_static { e.unit.vel() } else { Vec3::xz(0.0, 0.0) };
            let at = |t: i32| Vec3::xz(e.position.x + vel.x * 30.0 * t as f32, e.position.z + vel.z * 30.0 * t as f32);
            // In reach by its own track, or after a gun move the gun has
            // the time to make and still aim before the hold runs out.
            let speed = unit.def().map_or(1.0, |d| d.speed()).max(1.0);
            let drift = (1..=steps).any(|t| distance(pos, at(t)) <= reach);
            let moved = leads
                && (1..=steps).any(|t| {
                    let need = distance(pos, at(t)) - reach;
                    need <= max_move && need / speed + AIM_SECONDS <= t as f32
                })
                && {
                    // Not into more threat than where it stands.
                    let view = self.war.view(unit.def());
                    let to = get_directional_point(pos, e.position, (d - range + 30.0).min(max_move));
                    view.get_effective_threat(to) <= 1.25 * view.get_effective_threat(pos) + 1.0
                };
            if (drift || moved) && soon.is_none_or(|s| v > s.1) {
                soon = Some((e.unit_id, v, !drift));
            }
        }
        let Some((t, v, moves)) = soon else { return false };
        // Over what it would shoot now: the best in reach, and with nothing
        // in reach at least what a mark is worth (the chaff it would fire at
        // instead is worth less).
        let over = now.map_or(0.0, |n| n.1).max(floor);
        if v < k * over.max(1.0) {
            return false;
        }
        unit.set_fire_state(0, 0);
        unit.execute_custom_command(cmd::UNIT_SET_TARGET, &[t as f32], 0, NO_TIMEOUT);
        self.taskforces.marks.insert(id, Mark { target: t, at: frame, reload });
        self.taskforces.volleys.holds.insert(id, Hold { target: t, start: frame, moved: moves });
        self.taskforces.volleys.held_at.insert(id, reload);
        let e = &self.war.targets[&t];
        let yielded = now.and_then(|n| self.war.targets.get(&n.0)).map_or("nothing".to_owned(), |y| y.def_name.clone());
        lines.push(format!(
            "taskforce: a {gun} holds for a {} ({v:.0} metal, k {:.1}, {hold_for:.0} s) over a {yielded} ({over:.0} metal)",
            e.def_name,
            v / over.max(1.0)
        ));
        if moves {
            let d = distance(pos, e.position);
            let by = (d - range + 30.0).min(max_move);
            kgb_move_to(unit, get_directional_point(pos, e.position, by), 0, frame);
            lines.push(format!("taskforce: a {gun} moves {by:.0} for a {}", e.def_name));
        }
        true
    }

    /// Set each core gun of force `sid` on its best mark, or let it be.
    pub(crate) fn taskforce_mark(&mut self, sid: SquadId) {
        let Some(s) = self.squads.get(sid) else { return };
        let (hold, share) = (s.shape.mark_hold, s.shape.mark_share);
        let losfire = s.shape.losfire;
        let fix = if losfire { Fix::Sight } else { Fix::Exact };
        let (wait, hold_for, wait_move) = (s.shape.mark_wait_ratio, s.shape.mark_hold_for, s.shape.mark_move);
        // Only an engaged force's leader moves for a hold (its escort comes
        // with it; a force falling back keeps falling back).
        let leader = s.leader.filter(|_| s.engaging);
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
            let floor = self.marksman_floor(unit, share);
            if losfire {
                let deserving = (self.war.targets.values())
                    .filter(|e| !e.can_fly() && distance(pos, e.position) <= range + RESERVE)
                    .filter(|e| self.marksman_shot(e, damage, &shields, Fix::Radar).is_ok_and(|(w, _)| w >= floor))
                    .count();
                let reserve = deserving > 0;
                if self.taskforces.volleys.reserve.insert(id, reserve) != Some(reserve) {
                    if !self.taskforces.volleys.holds.contains_key(&id) {
                        unit.set_fire_state(if reserve { 0 } else { 2 }, 0);
                    }
                    let gun = self.defs.name_of(unit);
                    lines.push(if reserve {
                        format!(
                            "taskforce: a {gun} reserves: {deserving} deserving known within {:.0}",
                            range + RESERVE
                        )
                    } else {
                        format!("taskforce: a {gun} fires at will: only chaff about")
                    });
                }
            }
            if wait > 0.0
                && self.marksman_wait(id, unit, wait, hold_for, wait_move, leader == Some(id), fix, floor, &mut lines)
            {
                continue;
            }
            let held = self.taskforces.marks.get(&id).copied();
            let taken: BTreeSet<i32> =
                (self.taskforces.marks.iter()).filter(|(g, _)| **g != id).map(|(_, m)| m.target).collect();
            // A target a spotter brought into sight for this gun, worth more
            // than its mark: the hold gives way to it.
            let spotted = (self.taskforces.spotters.by_unit.values())
                .filter_map(|sp| sp.errand_for(id))
                .filter_map(|t| self.war.targets.get(&t))
                .filter(|e| Some(e.unit_id) != held.map(|m| m.target) && distance(pos, e.position) <= range + REACH)
                .filter_map(|e| self.marksman_shot(e, damage, &shields, fix).ok().map(|(w, _)| w))
                .fold(0.0f32, f32::max);
            if hold && let Some(m) = held {
                let fired = reload != m.reload;
                let e = self.war.targets.get(&m.target);
                let off = e.map_or(f32::INFINITY, |e| distance(pos, e.position));
                let why = match e.map(|e| self.marksman_shot(e, damage, &shields, fix)) {
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
                    && e.and_then(|e| self.marksman_shot(e, damage, &shields, fix).ok())
                        .is_none_or(|(w, _)| spotted <= w)
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
                .filter_map(|e| {
                    self.marksman_shot(e, damage, &shields, fix).ok().filter(|&(worth, _)| worth >= floor).map(
                        |(w, h)| {
                            let stream = if wait > 0.0 { STREAM_SECONDS * self.marksman_stream(e) } else { 0.0 };
                            (e.unit_id, w + stream, h)
                        },
                    )
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
        let reach: Vec<(Vec3, f32)> =
            guns.iter().filter_map(|(_, u)| u.weapon(0).map(|w| (u.pos(), w.range() + REACH))).collect();
        let (mut known, mut identified, mut visible, mut markable) = (0, 0, 0, 0);
        for e in self.war.targets.values().filter(|e| !e.is_dead && e.position != NULLPOS && !e.can_fly()) {
            if !reach.iter().any(|&(p, r)| distance(p, e.position) <= r) {
                continue;
            }
            known += 1;
            identified += usize::from(e.identified);
            visible += usize::from(e.visible);
            markable += usize::from(e.identified && (e.visible || e.is_static));
        }
        if known > 0 {
            self.log(&format!(
                "taskforce: the {doctrine} force (squad {sid}, {state}) reaches {known} known, {identified} identified, \
                 {visible} visible, {markable} markable"
            ));
        }
    }

    /// Log a kill by a gun some marksman doctrine takes for a core (on or
    /// off, in a force or not, so the arms of an A/B count alike): whether
    /// it was its mark.
    pub fn taskforce_enemy_destroyed(&mut self, enemy: Unit, attacker: Option<Unit>) {
        let Some(a) = attacker.filter(|a| a.team() == self.team_id) else { return };
        let name = self.defs.name_of(a).to_owned();
        let tf = &self.taskforces;
        if !tf.doctrines.iter().any(|d| d.marksman && d.is_core(&name, &tf.classes)) {
            return;
        }
        let marked = tf.marks.get(&a.id()).is_some_and(|m| m.target == enemy.id());
        let in_force = self
            .fighters
            .get(&a.id())
            .and_then(|f| f.squad)
            .is_some_and(|s| self.squads.get(s).is_some_and(|q| q.is_taskforce() && q.shape.marksman));
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
