//! `stayveto` (on in the wip line, `disable=stayveto`): units about to run home
//! for repairs stay in their fight when the fight predictor says staying
//! wins far more than it risks.
//!
//! Circuit's retreat rules are more conservative than ours, which buys it
//! survivability against an AI that chases to 10% and lets go. A human
//! sees the other side of a retreat: the losses taken to fight the heavy
//! are paid, and leaving at the trigger throws away the gain they bought
//! (user, 2026-10-31).
//!
//! The retreat handler asks twice a second, before any retreat order, about
//! the units whose health retreat fired in the last [`FRESH`] frames (and
//! those standing). Units retreating within [`FRIENDS`] of one another are
//! judged together, as the retreat rule moves them: a hurt unit judged
//! alone never matters ("the others will finish the heavy"), while the
//! others cross the same threshold and leave too. Two fates of their local
//! fight (`fightsim::local`, the sim's own P(dead)): our healthy units
//! nearby fighting on with the group, and without it. The difference
//! prices the retreat:
//!
//! - extra gain = the enemy's worth killed with the group staying, less
//!   without it;
//! - extra loss = our metal lost with it staying (its own deaths
//!   included), less without it, at least [`LOSS_FLOOR`] of its cost.
//!
//! The group stands when the extra gain is [`RATIO`] times the extra loss
//! and at least [`MIN_GAIN`] (and half its cost): losing 100 more to kill
//! 1000 is worth it; losing 100 to kill 150 is not. Standing units are
//! still the retreat handler's (their controllers keep skipping them),
//! fight toward the enemy whose death they add most to, and are judged
//! again twice a second with looser thresholds ([`KEEP_RATIO`]); they run
//! when the forecast turns, the fight clears, or after [`MAX_STAND`].
//! Aircraft, spotters, units being captured or stunned, and shield
//! retreats are not judged.
//!
//! `stayshadow` (with `stayveto` off) judges the same retreats and
//! journals the groups it would have stopped, with the same windows.
//!
//! Journal `k=stayveto`: `stand` / `would` (the group, both fates' sums raw
//! and calibrated, the focus), `check` (one group in [`SAMPLE`] that did not
//! stand, for the threshold's calibration), `end` (per unit: reason, how
//! long), `window` [`WINDOW`] after each record (the group's survivors, our
//! metal lost, the enemies' worth seen dying, the focus dead), `cost` per
//! minute.

use std::collections::BTreeMap;

use serde_json::{Value, json};
use springai::reserved::{self, Owner};
use springai::{Unit, Vec3};

use crate::Ai;
use crate::military::fightsim::FateMode;
use crate::military::fightsim::local::{Budget, Local, Tally, kills, tally};
use crate::military::wrappers::fighter::fight_clear;
use crate::military::wrappers::squad::SquadKind;
use crate::nav;
use crate::util::distance;

pub const FEATURE: &str = "stayveto";
pub const SHADOW: &str = "stayshadow";
/// A stand is a declared fight (`staygang`, see [`Ai::stayveto_declare`]).
pub const GANG: &str = "staygang";

/// Standing: extra gain over extra loss to start, then to go on.
const RATIO: f32 = 3.0;
const KEEP_RATIO: f32 = 1.5;
/// Extra gain at least this much metal (and half the group's cost)...
const MIN_GAIN: f32 = 120.0;
const KEEP_MIN_GAIN: f32 = 60.0;
/// ...and the extra loss counted at least this share of its cost: a
/// fight the sim calls free still risks the units.
const LOSS_FLOOR: f32 = 0.2;
const KEEP_LOSS_FLOOR: f32 = 0.1;
/// Failed judgements in a row that end a stand: the sim's answer jumps
/// with small moves (who is in range of whom).
const FAILS: u8 = 3;
/// A stand lasts at most this long (frames).
const MAX_STAND: i32 = 30 * 30;
/// A retreat older than this (frames) is not turned back.
const FRESH: i32 = 30 * 6;
/// Retreating units this close are one group; our units this close to
/// the group fight beside it.
const FRIENDS: f32 = 600.0;
/// Synchronous query time per retreat tick (microseconds).
const BUDGET_US: u128 = 3000;
/// Frames after a record over which the outcome is read.
const WINDOW: i32 = 30 * 30;
/// One judged group that does not stand in this many is journaled.
const SAMPLE: u64 = 10;
/// Orders are repeated at most this often, unless the focus changes.
const REORDER: i32 = 60;
const REPORT_PERIOD: i32 = 1800;

struct Stand {
    since: i32,
    focus: i32,
    last_order: i32,
    /// Failed judgements in a row, and the last judgement's gain and loss.
    fails: u8,
    last: (f32, f32),
}

/// While a declared stand lasts its borrowed units take orders from the
/// retreat handler's module (where stayveto runs) and the helpers every
/// ground unit keeps (`facframe` turns an engine attack on a factory's frame
/// to the factory); their own handlers' orders are dropped.
const STAND_OWNER: Owner =
    Owner { module: "retreat", helpers: &["war.unitDamaged", "jumps", "teleport", "water", "facframe"] };

/// A healthy unit a declared stand borrowed, and the owner it goes back to.
struct Borrowed {
    id: i32,
    unit: Unit,
    origin: &'static str,
    prev: Option<Owner>,
}

/// A declared stand: the standing group and the healthy units fighting it
/// with them.
struct StandGang {
    group: Vec<i32>,
    borrowed: Vec<Borrowed>,
    since: i32,
    focus: i32,
    last_order: i32,
}

struct Window {
    frame: i32,
    kind: &'static str,
    group: Vec<Unit>,
    focus: i32,
    /// Our units in the fight and their metal.
    ours: Vec<(Unit, f32)>,
    /// Their units: id, worth, seen dying since.
    enemies: Vec<(i32, f32, bool)>,
    gain: f32,
    loss: f32,
}

#[derive(Default)]
pub struct StayVeto {
    stands: BTreeMap<i32, Stand>,
    gangs: BTreeMap<u32, StandGang>,
    next_gang: u32,
    /// Standing unit -> its declared stand.
    gang_of: BTreeMap<i32, u32>,
    /// Borrowed unit -> the declared stand that holds it.
    lent: BTreeMap<i32, u32>,
    /// Retreating unit -> frame its retreat began.
    began: BTreeMap<i32, i32>,
    /// Retreats settled: judged once in the shadow, or a stand that ended
    /// (not judged again until the unit stops retreating).
    settled: BTreeMap<i32, i32>,
    windows: Vec<Window>,
    judged: u64,
    stood: u64,
    micros: u64,
    queries: u64,
    over_budget: u64,
    last_report: i32,
}

/// A judgement of one retreating group.
struct Judgement {
    gain: f32,
    loss: f32,
    cost: f32,
    focus: i32,
    stay: Tally,
    leave: Tally,
    stay_cal: Tally,
    leave_cal: Tally,
    local: Local,
}

fn stands(gain: f32, loss: f32, cost: f32, holding: bool) -> bool {
    if holding {
        gain >= KEEP_RATIO * loss.max(KEEP_LOSS_FLOOR * cost) && gain >= KEEP_MIN_GAIN
    } else {
        gain >= RATIO * loss.max(LOSS_FLOOR * cost) && gain >= MIN_GAIN.max(0.5 * cost)
    }
}

fn tally_json(t: &Tally) -> Value {
    json!({"own_cost": t.own_cost, "own_loss": t.own_loss, "kill_value": t.kill_value, "armed_share": t.armed_share})
}

impl Ai {
    pub fn stayveto_on(&self) -> bool {
        !self.dev_off(FEATURE)
    }

    fn stayveto_runs(&self) -> bool {
        self.stayveto_on() || !self.dev_off(SHADOW)
    }

    pub fn stayveto_enemy_destroyed(&mut self, id: i32) {
        for w in &mut self.stayveto.windows {
            for e in w.enemies.iter_mut().filter(|e| e.0 == id) {
                e.2 = true;
            }
        }
    }

    /// Whether retreating unit `id` is one the veto may judge: a ground
    /// unit sent home by its health.
    fn stayveto_eligible(&self, id: i32) -> Option<Unit> {
        let c = self.retreat_handler.coward(id)?;
        let u = c.unit;
        (!c.is_plane
            && !c.is_flyer
            && c.shield_ok()
            && u.health() > 0.0
            && u.team() == self.team_id
            && u.capture_progress() <= 0.0
            && !u.is_paralyzed()
            && !self.taskforces.spotters.by_unit.contains_key(&id))
        .then_some(u)
    }

    /// Before the retreat handler's orders (every tick it runs): judge the
    /// fresh and standing retreats in groups. Returns the units that stand
    /// (they get no retreat order this tick).
    pub(crate) fn stayveto_tick(&mut self, retreating: &[i32]) -> Vec<i32> {
        let frame = self.frame;
        self.stayveto_close_windows(frame);
        if frame - self.stayveto.last_report >= REPORT_PERIOD {
            self.stayveto_report(frame);
        }
        let sv = &mut self.stayveto;
        sv.began.retain(|id, _| retreating.contains(id));
        sv.settled.retain(|id, _| retreating.contains(id));
        sv.stands.retain(|id, _| retreating.contains(id));
        for &id in retreating {
            sv.began.entry(id).or_insert(frame);
        }
        self.stayveto_gangs_tidy();
        if !self.stayveto_runs() {
            return Vec::new();
        }
        let on = self.stayveto_on();
        if !on && !self.stayveto.stands.is_empty() {
            let ids: Vec<i32> = self.stayveto.stands.keys().copied().collect();
            for id in ids {
                self.stayveto_end(id, "off");
            }
        }
        // Who is judged: standing units, and retreats still fresh (in the
        // shadow, each retreat once).
        let mut todo: Vec<(i32, Unit)> = Vec::new();
        for &id in retreating {
            let standing = self.stayveto.stands.get(&id).map(|s| s.since);
            let Some(u) = self.stayveto_eligible(id) else {
                if standing.is_some() {
                    self.stayveto_end(id, "ineligible");
                }
                continue;
            };
            if let Some(since) = standing {
                if frame - since >= MAX_STAND {
                    self.stayveto_end(id, "max");
                    continue;
                }
            } else {
                let began = self.stayveto.began.get(&id).copied().unwrap_or(frame);
                if frame - began > FRESH || self.stayveto.settled.contains_key(&id) {
                    continue;
                }
            }
            todo.push((id, u));
        }
        // Standing groups first, then the newest retreats.
        todo.sort_by_key(|(id, _)| {
            (!self.stayveto.stands.contains_key(id), -self.stayveto.began.get(id).copied().unwrap_or(0))
        });
        let mut budget = Budget::new(BUDGET_US);
        let mut standing = Vec::new();
        while let Some(&(_, lead)) = todo.first() {
            let at = lead.pos();
            let group: Vec<(i32, Unit)> =
                todo.iter().copied().filter(|(_, u)| distance(u.pos(), at) <= FRIENDS).collect();
            todo.retain(|t| !group.iter().any(|g| g.0 == t.0));
            if budget.spent() {
                // Standing units hold until they can be judged again.
                self.stayveto.over_budget += 1;
                standing.extend(group.iter().map(|g| g.0).filter(|id| self.stayveto.stands.contains_key(id)));
                continue;
            }
            standing.extend(self.stayveto_group(&group, &mut budget));
        }
        let sv = &mut self.stayveto;
        sv.micros += budget.micros();
        sv.queries += budget.queries as u64;
        standing
    }

    /// Judge one group of retreating units; the ones that stand.
    fn stayveto_group(&mut self, group: &[(i32, Unit)], budget: &mut Budget) -> Vec<i32> {
        let frame = self.frame;
        let on = self.stayveto_on();
        let ids: Vec<i32> = group.iter().map(|g| g.0).collect();
        let held: Vec<i32> = ids.iter().copied().filter(|id| self.stayveto.stands.contains_key(id)).collect();
        let holding = held.len() == ids.len();
        self.stayveto.judged += 1;
        let Some(j) = self.stayveto_judge(group, budget) else {
            for id in held {
                self.stayveto_end(id, "clear");
            }
            return Vec::new();
        };
        if stands(j.gain, j.loss, j.cost, holding) {
            if !on {
                for id in &ids {
                    self.stayveto.settled.insert(*id, frame);
                }
                self.stayveto_record("would", group, &j);
                return Vec::new();
            }
            if !holding {
                self.stayveto.stood += 1;
                self.stayveto_record("stand", group, &j);
                if self.stayveto_gang_on() {
                    self.stayveto_declare(group, j.focus);
                }
            }
            for &(id, u) in group {
                let s = self.stayveto.stands.entry(id).or_insert(Stand {
                    since: frame,
                    focus: -1,
                    last_order: -REORDER,
                    fails: 0,
                    last: (0.0, 0.0),
                });
                s.fails = 0;
                s.last = (j.gain, j.loss);
                self.stayveto_order(id, u, j.focus);
            }
            self.stayveto_gangs_order(&ids, j.focus);
            return ids;
        }
        // The standing units hold on the looser rule, and through a few
        // failed judgements; the newcomers did not pass the stricter one.
        let keep = stands(j.gain, j.loss, j.cost, true);
        let mut kept = Vec::new();
        for &(id, u) in group.iter().filter(|g| held.contains(&g.0)) {
            let fails = self.stayveto.stands.get_mut(&id).map_or(FAILS, |s| {
                s.fails = if keep { 0 } else { s.fails + 1 };
                s.last = (j.gain, j.loss);
                s.fails
            });
            if fails >= FAILS {
                self.stayveto_end(id, "forecast");
            } else {
                self.stayveto_order(id, u, j.focus);
                kept.push(id);
            }
        }
        self.stayveto_gangs_order(&kept, j.focus);
        // A retreat that does not stand is judged again while fresh, in the
        // shadow as in play.
        if !holding && self.stayveto.judged.is_multiple_of(SAMPLE) {
            self.stayveto_record("check", group, &j);
        }
        kept
    }

    /// The group's fight with it and without it.
    fn stayveto_judge(&mut self, group: &[(i32, Unit)], budget: &mut Budget) -> Option<Judgement> {
        let at: Vec<Vec3> = group.iter().map(|g| g.1.pos()).collect();
        let mut units: Vec<Unit> = group.iter().map(|g| g.1).collect();
        for (fid, f) in &self.fighters {
            if group.iter().any(|g| g.0 == *fid)
                || f.is_dead()
                || !at.iter().any(|&p| distance(f.get_pos(), p) <= FRIENDS)
                || (self.retreat_handler.is_retreating(*fid) && !self.stayveto.stands.contains_key(fid))
            {
                continue;
            }
            if f.unit.def().is_some_and(|d| d.max_weapon_range() > 0.0 && !d.is_able_to_fly()) {
                units.push(f.unit);
            }
        }
        let mut local = self.local_fight(&units)?;
        if local.unknown || !local.armed_theirs() {
            return None;
        }
        let ids: Vec<i32> = group.iter().map(|g| g.0).collect();
        let cost: f32 = local.ours.iter().filter(|o| ids.contains(&o.id)).map(|o| o.cost).sum();
        if cost <= 0.0 {
            return None;
        }
        let mut leave_local = local.without(&ids);
        if self.stayveto_gang_on() {
            // A declared stand: everyone in the stay fight fights under our
            // order (SimFlags::ordered); leaving, the others go on as the
            // logs say units do.
            local.snap.flags.ordered = true;
            leave_local.snap.flags.ordered = false;
        }
        let stay = self.local_fates(&local, FateMode::AMove, budget)?;
        let leave = self.local_fates(&leave_local, FateMode::AMove, budget)?;
        let (ts, tl) = (tally(&local, &stay, true), tally(&leave_local, &leave, true));
        let (ks, kl) = (kills(&local, &stay), kills(&leave_local, &leave));
        let focus = (0..local.theirs.len())
            .max_by(|&a, &b| (ks[a] - kl[a]).total_cmp(&(ks[b] - kl[b])))
            .map(|k| local.theirs[k].id)?;
        Some(Judgement {
            gain: ts.kill_value - tl.kill_value,
            loss: ts.own_loss - tl.own_loss,
            cost,
            focus,
            stay: ts,
            leave: tl,
            stay_cal: tally(&local, &stay, false),
            leave_cal: tally(&leave_local, &leave, false),
            local,
        })
    }

    /// Declared stands (`staygang`): on with stayveto unless disabled.
    pub fn stayveto_gang_on(&self) -> bool {
        self.stayveto_on() && !self.dev_off(GANG)
    }

    /// Whether a declared stand holds unit `id` (coalesce leaves it alone).
    pub fn stayveto_borrowed(&self, id: i32) -> bool {
        self.stayveto.lent.contains_key(&id)
    }

    /// Healthy units near `group` that a declared stand may borrow: what a
    /// raider gang could (raiders, armed pickets) and the members of
    /// ordinary assault and hover squads; never taskforces, jaegers, AA or
    /// artillery, nor anything on a strike, a QRF draft or an operator's
    /// order, nor what another stand or a raider gang holds.
    fn stayveto_borrowable(&self, group: &[(i32, Unit)]) -> Vec<(i32, Unit, &'static str)> {
        let at: Vec<Vec3> = group.iter().map(|g| g.1.pos()).collect();
        let near = |u: Unit| at.iter().any(|&p| distance(u.pos(), p) <= FRIENDS);
        let free = |id: i32| {
            !group.iter().any(|g| g.0 == id)
                && !self.stayveto.lent.contains_key(&id)
                && !self.stayveto.stands.contains_key(&id)
                && !self.coalesce_owns(id)
        };
        let mut out: Vec<(i32, Unit, &'static str)> = (self.coalesce_candidates().into_iter())
            .filter(|&(id, _, u)| free(id) && near(u))
            .map(|(id, o, u)| (id, u, o.name()))
            .collect();
        for sid in self.squads.ids() {
            let Some(s) = self.squads.get(sid) else { continue };
            if !matches!(s.kind, SquadKind::Assault | SquadKind::Hover)
                || self.qrf_drafted(sid)
                || self.op_squad_target(sid).is_some()
                || self.squad_strike_active(sid)
            {
                continue;
            }
            for &id in s.fighters.iter().chain(s.leader.iter()) {
                let Some(f) = self.fighters.get(&id) else { continue };
                let u = f.unit;
                if f.is_dead() || !free(id) || out.iter().any(|o| o.0 == id) || !near(u) {
                    continue;
                }
                let Some(def) = u.def() else { continue };
                if u.team() != self.team_id
                    || u.is_being_built()
                    || def.is_able_to_fly()
                    || def.max_weapon_range() <= 0.0
                    || u.pos().y < -5.0
                    || self.retreat_is_retreating(u)
                    || self.fire_controls(id)
                    || self.squad_strike_owns(id)
                {
                    continue;
                }
                out.push((id, u, "assault"));
            }
        }
        out
    }

    /// The group stands: make it a declared fight (`staygang`, user
    /// 2026-11-03: "a temporary stay in a fight, which actually is a fight
    /// because we declare it to be a fight, re-evaluated continually"). In
    /// the A/B games a stand lasted 2 s on median and the healthy units the
    /// stay fate counted on mostly did not close in (32% did in 5 s, 42%
    /// moved away): the fate priced a fight nobody ordered. Now the healthy
    /// units near the group that a stand may borrow join it, owned by the
    /// retreat module (their handlers' orders are dropped) and sent at the
    /// focus with the group until its last stand ends. A group that grew
    /// around a declared stand joins that one.
    fn stayveto_declare(&mut self, group: &[(i32, Unit)], focus: i32) {
        let frame = self.frame;
        if let Some(gid) = group.iter().find_map(|g| self.stayveto.gang_of.get(&g.0).copied()) {
            for &(id, _) in group {
                self.stayveto.gang_of.insert(id, gid);
                if let Some(g) = self.stayveto.gangs.get_mut(&gid)
                    && !g.group.contains(&id)
                {
                    g.group.push(id);
                }
            }
            return;
        }
        let gid = self.stayveto.next_gang;
        self.stayveto.next_gang += 1;
        let mut borrowed = Vec::new();
        for (id, unit, origin) in self.stayveto_borrowable(group) {
            let prev = reserved::owner(self.cb, id);
            reserved::own(self.cb, id, STAND_OWNER);
            self.stayveto.lent.insert(id, gid);
            borrowed.push(Borrowed { id, unit, origin, prev });
        }
        for &(id, _) in group {
            self.stayveto.gang_of.insert(id, gid);
        }
        let lent: Vec<Value> = (borrowed.iter())
            .map(|b| json!([b.id, b.unit.def().map(|d| self.defs.name(d).to_owned()), b.origin]))
            .collect();
        let ids: Vec<i32> = group.iter().map(|g| g.0).collect();
        self.op_emit(
            "stayveto",
            json!({"event": "gang", "t": frame / 30, "gang": gid, "group": ids, "focus": focus, "borrowed": lent}),
        );
        self.stayveto
            .gangs
            .insert(gid, StandGang { group: ids, borrowed, since: frame, focus: -1, last_order: -REORDER });
    }

    /// Send the borrowed units of the declared stands that `ids` stand in
    /// at `focus` (with the group, see `stayveto_order`).
    fn stayveto_gangs_order(&mut self, ids: &[i32], focus: i32) {
        let frame = self.frame;
        let mut gids: Vec<u32> = ids.iter().filter_map(|id| self.stayveto.gang_of.get(id).copied()).collect();
        gids.sort_unstable();
        gids.dedup();
        let Some(epos) = self.war.targets.get(&focus).map(|e| e.position) else { return };
        let team = self.team_id;
        for gid in gids {
            let Some(g) = self.stayveto.gangs.get(&gid) else { continue };
            if g.focus == focus && frame - g.last_order < REORDER {
                continue;
            }
            // A borrowed unit that its health sent home (and that does not
            // stand in turn) is the retreat handler's.
            let units: Vec<Unit> = (g.borrowed.iter())
                .filter(|b| !self.retreat_handler.is_retreating(b.id) || self.stayveto.stands.contains_key(&b.id))
                .map(|b| b.unit)
                .filter(|u| u.health() > 0.0 && u.team() == team)
                .collect();
            let nav = nav!(self);
            for u in units {
                fight_clear(u, epos, 0, &nav);
            }
            if let Some(g) = self.stayveto.gangs.get_mut(&gid) {
                g.focus = focus;
                g.last_order = frame;
            }
        }
    }

    /// A declared stand is over: its borrowed units go back to their owners
    /// (unless something else took them meanwhile) and handlers.
    fn stayveto_gang_end(&mut self, gid: u32, reason: &str) {
        let frame = self.frame;
        let Some(g) = self.stayveto.gangs.remove(&gid) else { return };
        for id in &g.group {
            if self.stayveto.gang_of.get(id) == Some(&gid) {
                self.stayveto.gang_of.remove(id);
            }
        }
        let mut alive = 0;
        for b in &g.borrowed {
            self.stayveto.lent.remove(&b.id);
            if reserved::owner(self.cb, b.id) == Some(STAND_OWNER) {
                match b.prev {
                    Some(o) => {
                        reserved::own(self.cb, b.id, o);
                    }
                    None => {
                        reserved::disown(self.cb, b.id);
                    }
                }
            }
            match b.origin {
                "stakeout" => self.stakeouts_release(b.id),
                "solo" | "kodachi" => {
                    if let Some(f) = self.fighters.get_mut(&b.id) {
                        f.last_task_frame = 0;
                    }
                }
                _ => {}
            }
            alive += usize::from(b.unit.health() > 0.0 && b.unit.team() == self.team_id);
        }
        self.op_emit(
            "stayveto",
            json!({"event": "gang_end", "t": frame / 30, "gang": gid, "reason": reason,
                   "life": (frame - g.since) / 30, "group": g.group.len(), "borrowed": g.borrowed.len(), "alive": alive}),
        );
    }

    /// End the declared stands none of whose group stands any more (all of
    /// them with the switch off).
    fn stayveto_gangs_tidy(&mut self) {
        let on = self.stayveto_gang_on();
        let over: Vec<u32> = (self.stayveto.gangs.iter())
            .filter(|(_, g)| !on || !g.group.iter().any(|id| self.stayveto.stands.contains_key(id)))
            .map(|(&gid, _)| gid)
            .collect();
        for gid in over {
            self.stayveto_gang_end(gid, if on { "over" } else { "off" });
        }
    }

    fn stayveto_order(&mut self, id: i32, unit: Unit, focus: i32) {
        let frame = self.frame;
        let Some(s) = self.stayveto.stands.get(&id) else { return };
        if s.focus == focus && frame - s.last_order < REORDER {
            return;
        }
        let Some(epos) = self.war.targets.get(&focus).map(|e| e.position) else { return };
        let nav = nav!(self);
        fight_clear(unit, epos, 0, &nav);
        if let Some(s) = self.stayveto.stands.get_mut(&id) {
            s.focus = focus;
            s.last_order = frame;
        }
    }

    /// A stand is over: the unit runs for repairs from the next tick.
    fn stayveto_end(&mut self, id: i32, reason: &str) {
        let frame = self.frame;
        let Some(s) = self.stayveto.stands.remove(&id) else { return };
        self.stayveto.settled.insert(id, frame);
        if let Some(gid) = self.stayveto.gang_of.remove(&id) {
            let standing = (self.stayveto.gangs.get(&gid))
                .is_some_and(|g| g.group.iter().any(|m| self.stayveto.stands.contains_key(m)));
            if !standing {
                self.stayveto_gang_end(gid, reason);
            }
        }
        self.retreat_handler.forget_retreat_order(id);
        let hp = self.retreat_handler.coward(id).map(|c| c.unit.health() / c.unit.max_health().max(1.0));
        self.op_emit(
            "stayveto",
            json!({"event": "end", "t": frame / 30, "unit": id, "reason": reason, "stood": (frame - s.since) / 30,
                   "hp": hp, "last_gain": s.last.0, "last_loss": s.last.1}),
        );
    }

    fn stayveto_record(&mut self, kind: &'static str, group: &[(i32, Unit)], j: &Judgement) {
        let frame = self.frame;
        let ours: Vec<(Unit, f32)> = (j.local.ours.iter())
            .filter_map(|o| {
                let u = match group.iter().find(|g| g.0 == o.id) {
                    Some(g) => g.1,
                    None => self.fighters.get(&o.id)?.unit,
                };
                Some((u, o.cost))
            })
            .collect();
        let enemies: Vec<(i32, f32, bool)> = j.local.theirs.iter().map(|t| (t.id, t.value, false)).collect();
        let focus_def = j.local.theirs.iter().find(|t| t.id == j.focus).map(|t| t.def.clone());
        let theirs: Vec<Value> = (j.local.theirs.iter())
            .map(|t| json!({"id": t.id, "def": t.def, "cost": t.cost, "value": t.value, "armed": t.armed}))
            .collect();
        let units: Vec<Value> = (group.iter())
            .map(|&(id, u)| {
                json!({"id": id, "def": u.def().map(|d| self.defs.name(d).to_owned()),
                       "hp": u.health() / u.max_health().max(1.0)})
            })
            .collect();
        let rec = json!({
            "event": kind, "t": frame / 30, "group": units, "cost": j.cost,
            "gain": j.gain, "loss": j.loss, "ratio": j.gain / j.loss.max(LOSS_FLOOR * j.cost).max(1.0),
            "focus": j.focus, "focus_def": focus_def,
            "stay": tally_json(&j.stay), "leave": tally_json(&j.leave),
            "stay_cal": tally_json(&j.stay_cal), "leave_cal": tally_json(&j.leave_cal),
            "n": [j.local.ours.len(), j.local.theirs.len()], "theirs": theirs,
        });
        self.op_emit("stayveto", rec);
        self.stayveto.windows.push(Window {
            frame,
            kind,
            group: group.iter().map(|g| g.1).collect(),
            focus: j.focus,
            ours,
            enemies,
            gain: j.gain,
            loss: j.loss,
        });
    }

    fn stayveto_close_windows(&mut self, frame: i32) {
        let ws = std::mem::take(&mut self.stayveto.windows);
        let (due, open): (Vec<Window>, Vec<Window>) = ws.into_iter().partition(|w| frame - w.frame >= WINDOW);
        self.stayveto.windows = open;
        let team = self.team_id;
        let dead = |u: Unit| u.health() <= 0.0 || u.team() != team;
        for w in due {
            let lost: f32 = w.ours.iter().filter(|(u, _)| dead(*u)).map(|o| o.1).sum();
            let group_lost: f32 = w.ours.iter().filter(|(u, _)| dead(*u) && w.group.contains(u)).map(|o| o.1).sum();
            let killed: f32 = w.enemies.iter().filter(|e| e.2).map(|e| e.1).sum();
            let focus_dead = w.enemies.iter().any(|e| e.0 == w.focus && e.2);
            self.op_emit(
                "stayveto",
                json!({
                    "event": "window", "t": frame / 30, "record_t": w.frame / 30, "kind": w.kind,
                    "group": w.group.len(), "group_alive": w.group.iter().filter(|u| !dead(**u)).count(),
                    "group_lost": group_lost, "ours": w.ours.len(), "lost": lost,
                    "metal": w.ours.iter().map(|o| o.1).sum::<f32>(),
                    "enemies": w.enemies.len(), "enemy_dead": w.enemies.iter().filter(|e| e.2).count(),
                    "killed_value": killed, "focus_dead": focus_dead, "gain": w.gain, "loss": w.loss,
                }),
            );
        }
    }

    fn stayveto_report(&mut self, frame: i32) {
        let mode = if self.stayveto_on() { "on" } else { "shadow" };
        let sv = &mut self.stayveto;
        sv.last_report = frame;
        if sv.judged == 0 {
            return;
        }
        let data = json!({
            "event": "cost", "t": frame / 30, "mode": mode, "judged": sv.judged, "stood": sv.stood,
            "queries": sv.queries, "micros": sv.micros, "over_budget": sv.over_budget, "standing": sv.stands.len(),
        });
        sv.judged = 0;
        sv.stood = 0;
        sv.queries = 0;
        sv.micros = 0;
        sv.over_budget = 0;
        self.op_emit("stayveto", data);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn outsized_gains_stand_and_ordinary_trades_run() {
        // The user's example: 100 more lost to kill 1000.
        assert!(stands(1000.0, 100.0, 150.0, false));
        // 100 more lost to kill 150: run.
        assert!(!stands(150.0, 100.0, 150.0, false));
        // The sim calls staying free: the units themselves are still at risk.
        assert!(!stands(130.0, 0.0, 300.0, false));
        assert!(stands(400.0, 0.0, 300.0, false));
        // Small change is not worth a unit's life.
        assert!(!stands(100.0, 0.0, 60.0, false));
        // Holding is looser.
        assert!(stands(200.0, 120.0, 150.0, true));
        assert!(!stands(200.0, 150.0, 150.0, true));
    }

    /// A declared stand's borrowed units take its orders (stayveto runs in
    /// the retreat handler's module) and nobody else's but the helpers every
    /// ground unit keeps.
    #[test]
    fn a_declared_stand_owns_what_it_borrows() {
        assert!(STAND_OWNER.admits("retreat.update"));
        assert!(STAND_OWNER.admits("war.unitDamaged"));
        for other in ["squad.update", "raider.update", "stakeouts.update", "coalesce.update"] {
            assert!(!STAND_OWNER.admits(other), "{other}");
        }
        assert!(!crate::ai::OFF_BY_DEFAULT.contains(&GANG));
    }

    #[test]
    fn on_by_default_in_the_wip_line() {
        // 2026-11-01: off after losing its mirror 3-15 (nothing against
        // Circuit or Wires); 2026-11-02 back on in the wip for a second look
        // with the newer predictor, beside the latched fightsim veto.
        assert!(!crate::ai::OFF_BY_DEFAULT.contains(&"stayveto"));
        assert!(!crate::ai::OFF_BY_DEFAULT.contains(&"coalesce"));
    }
}
