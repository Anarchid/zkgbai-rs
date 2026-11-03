//! `comguard` (on; `disable=comguard`): the commander is a combat pusher.
//! Shot at, it fights on unless the fight predictor, counting everything
//! about, says it dies; then it leaves and keeps out of that fight.
//!
//! The worker retreat (`assign`) reads the threat paint: a commander fled
//! when the enemy paint was twice ours (its own paint included) or when
//! hurt in enemy paint, unless building a turret, and came back once its
//! spot had no enemy paint for 3 s. In the lobby games of Nov 2 the
//! commander died in 10 of 40, each time to an army seen closing for
//! 20-30 s (5 Reapers, 4 Ravagers and a Mace, Halberds, 3 Buoys outranging
//! it); it stepped back at the first hits and went back to work under fire
//! (user, 2026-11-03: "steps back when fired at, then goes and builds
//! something while ignoring continued incoming fire").
//!
//! User, 2026-11-03, after a first version fled ahead of contact from any
//! fight costing it a fifth of its health (and so from 4 Scorchers beside
//! its own garrison, QRF and Lotus, which it did not count): "We want the
//! commander to be able to be a combat pusher, so generally maybe a
//! trigger can be actually taking damage first, and then retreating if the
//! local situation *in its entirety* proclaims commander death."
//!
//! Every [`PERIOD`] frames, a commander with an armed enemy within
//! [`SUPPORT`] asks the predictor about its local fight
//! (`fightsim::local`: every identified enemy about; on our side every
//! armed friendly unit, ours or an ally's, turrets included, within
//! [`FRIENDS`], the nearest [`MAX_FRIENDS`]), ZK's tactical AI moving the
//! units it governs (`kiting_fates`: Buoys keep their range), over
//! [`GUARD_HORIZON`] (15 s, the squad veto's). Enemy turrets it is out of
//! range of stay out (the a-move would walk it in); unidentified contacts
//! are left out.
//!
//! Hit by an enemy within [`HIT_WINDOW`], it leaves when the sim's own
//! P(dead) reaches [`DEATH`] or its expected health at the horizon is
//! under [`NEAR_DEAD`] of its max; not hit, it never leaves on the guard's
//! word. While a verdict is fresh ([`FRESH`]) and covers what last hit it
//! (the shooter is in the judged fight, or it was not hit lately), the
//! threat-paint retreat does not apply to it and a retreat that rule began
//! ends: the predictor speaks for the commander. A hit from outside the
//! judged fight (unseen or unidentified) leaves it to the threat paint.
//!
//! Leaving, it drops local defense, flees (the worker retreat, jumps
//! included) to the first of the incursion refuge, the closest haven and
//! the allies' centre that the enemies in the fight do not reach (re-picked
//! when they come on), and asks for defense. It is held [`HOLD`] past the
//! last death verdict; for [`AVOID`] after, it takes no job those enemies
//! reach.
//!
//! Journal (`k=comguard`): `flee` (the fight and its fate), `release`,
//! `hit` (a hit commander judged to live, at most every [`HIT_EMIT`]),
//! `cost` every minute.

use std::collections::BTreeMap;
use std::time::Instant;

use serde_json::json;
use springai::{Unit, Vec3, reserved};

use super::erase_one_dq;
use crate::Ai;
use crate::military::DefenseTarget;
use crate::military::fightsim::local::SUPPORT;
use crate::util::distance;

pub const FEATURE: &str = "comguard";

/// Frames between judgements.
const PERIOD: i32 = 10;
/// Armed friendly units (ours and allies') this close fight beside it...
const FRIENDS: f32 = 800.0;
/// ...the nearest this many (the local fight is judged up to 64 units).
const MAX_FRIENDS: usize = 40;
/// Frames of fight judged (15 s).
const GUARD_HORIZON: i32 = 450;
/// The sim's own P(dead) at which a hit commander leaves...
const DEATH: f32 = 0.3;
/// ...or the share of its health it is expected to end the fight under:
/// P(dead) is the spread around the expected fight, and a commander
/// expected out of it at 201 of 3250 had P(dead) 0.24 (smoke3 g2).
const NEAR_DEAD: f32 = 0.1;
/// A hit this recent (frames) lets a death verdict send it off.
const HIT_WINDOW: i32 = 90;
/// A verdict this recent (frames) speaks for the commander.
const FRESH: i32 = 30;
/// It is held this long past the last death verdict (frames).
const HOLD: i32 = 150;
/// Jobs the enemies of a death verdict reach are off this long after
/// (frames)...
const AVOID: i32 = 900;
/// ...within their range and this margin.
const AVOID_MARGIN: f32 = 350.0;
/// Mobile enemies unseen for longer do not mark jobs off (frames).
const AVOID_SEEN: i32 = 600;
/// Enemy turrets farther than their range and this margin are left out.
const STATIC_MARGIN: f32 = 100.0;
/// A haven within an enemy's range and this margin is no haven.
const HAVEN_MARGIN: f32 = 400.0;
/// A hit commander judged to live is journaled at most this often (frames).
const HIT_EMIT: i32 = 90;
const REPORT_PERIOD: i32 = 1800;

/// A commander held out of a fight, or lately.
struct Danger {
    since: i32,
    last: i32,
    /// The enemies of the fight that set it.
    enemies: Vec<i32>,
    haven: Vec3,
    /// Released (no longer holding the commander), still marking jobs off.
    released: bool,
}

/// What a commander's guard knows between judgements.
#[derive(Clone, Copy, Default)]
struct Watch {
    /// The last enemy hit: frame and shooter (`None`: unseen).
    hit: Option<(i32, Option<i32>)>,
    /// The last verdict's frame and whether it covered the last hit.
    verdict: Option<(i32, bool)>,
    last_emit: i32,
}

#[derive(Default)]
pub struct ComGuard {
    dangers: BTreeMap<i32, Danger>,
    watch: BTreeMap<i32, Watch>,
    judged: u64,
    hit_judged: u64,
    flees: u64,
    micros: u64,
    max_micros: u64,
    last_report: i32,
}

/// One judgement of a commander's fight.
struct Verdict {
    dies: bool,
    p_raw: f32,
    p_dead: f32,
    hp: f32,
    hp_left: f32,
    max: f32,
    ours: usize,
    /// An unidentified contact was left out of the fight.
    unknown: bool,
    /// Their units (id, def, distance, cost).
    theirs: Vec<(i32, String, f32, f32)>,
}

/// Whether the fight proclaims the commander's death: the sim's own
/// P(dead), or its expected health at the horizon out of `max`.
fn dies(p_raw: f32, hp_left: f32, max: f32) -> bool {
    p_raw >= DEATH || hp_left < NEAR_DEAD * max.max(1.0)
}

/// Whether a verdict judged on `theirs` covers the last hit: none lately,
/// or its shooter is in the fight.
fn covers(hit: Option<(i32, Option<i32>)>, frame: i32, theirs: &[i32]) -> bool {
    match hit {
        Some((at, shooter)) if frame - at <= HIT_WINDOW => shooter.is_some_and(|s| theirs.contains(&s)),
        _ => true,
    }
}

impl Ai {
    pub(super) fn eco_guard_commanders(&mut self, frame: i32) {
        if frame % PERIOD != self.offset % PERIOD {
            return;
        }
        self.comguard_report(frame);
        if self.dev_off(FEATURE) {
            self.eco.guard.dangers.clear();
            self.eco.guard.watch.clear();
            return;
        }
        let commanders = self.eco.commanders.clone();
        self.eco.guard.dangers.retain(|id, d| commanders.contains(id) && frame - d.last <= HOLD + AVOID);
        self.eco.guard.watch.retain(|id, _| commanders.contains(id));
        for id in commanders {
            self.comguard_check(id, frame);
        }
    }

    /// Whether commander `id` is held in its retreat.
    pub(super) fn comguard_holds(&self, id: i32) -> bool {
        self.eco.guard.dangers.get(&id).is_some_and(|d| !d.released)
    }

    /// Where commander `id`, held, retreats to.
    pub(super) fn comguard_haven(&self, id: i32) -> Option<Vec3> {
        self.eco.guard.dangers.get(&id).filter(|d| !d.released).map(|d| d.haven)
    }

    /// Whether commander `id` keeps away from a job at `p`: the enemies of
    /// its last danger reach it (see [`AVOID`]).
    pub(super) fn comguard_avoids(&self, id: i32, p: Vec3) -> bool {
        let frame = self.frame;
        let Some(d) = self.eco.guard.dangers.get(&id).filter(|d| frame - d.last <= HOLD + AVOID) else {
            return false;
        };
        d.enemies.iter().filter_map(|e| self.war.targets.get(e)).any(|e| {
            !e.is_dead
                && (e.is_static || frame - e.last_seen <= AVOID_SEEN)
                && distance(e.position, p) <= e.def_max_weapon_range() + AVOID_MARGIN
        })
    }

    /// Commander `unit` was damaged by `attacker` (`None`: unseen).
    pub(super) fn comguard_hit(&mut self, unit: Unit, attacker: Option<Unit>) {
        let id = unit.id();
        if !self.eco.commanders.contains(&id) || attacker.is_some_and(|a| a.ally_team() == self.ally_team_id) {
            return;
        }
        let shooter = attacker.filter(|a| a.def().is_some()).map(|a| a.id());
        self.eco.guard.watch.entry(id).or_default().hit = Some((self.frame, shooter));
    }

    /// Whether the predictor speaks for commander `id` now (see the module
    /// notes): the threat-paint retreat does not apply to it.
    pub(super) fn comguard_answers(&self, id: i32) -> bool {
        !self.dev_off(FEATURE)
            && (self.eco.guard.watch.get(&id))
                .and_then(|w| w.verdict)
                .is_some_and(|(at, covered)| covered && self.frame - at <= FRESH)
    }

    fn comguard_check(&mut self, id: i32, frame: i32) {
        let Some(unit) = self.eco.workers.get(&id).map(|w| w.unit) else { return };
        if unit.health() <= 0.0 || unit.team() != self.team_id || unit.is_being_built() {
            return;
        }
        // The operator or a transport owns it.
        if reserved::is_reserved(self.cb, id) || self.eco_in_transit(id) {
            return;
        }
        let verdict = self.comguard_judge(unit);
        let watch = self.eco.guard.watch.get(&id).copied().unwrap_or_default();
        let hit = watch.hit.is_some_and(|(at, _)| frame - at <= HIT_WINDOW);
        if let Some(v) = &verdict {
            let theirs: Vec<i32> = v.theirs.iter().map(|t| t.0).collect();
            let covered = covers(watch.hit, frame, &theirs);
            self.eco.guard.watch.entry(id).or_default().verdict = Some((frame, covered));
        }
        let held = self.comguard_holds(id);
        match verdict {
            Some(v) if v.dies && (held || hit) => {
                let enemies: Vec<i32> = v.theirs.iter().map(|t| t.0).collect();
                if held {
                    let d = self.eco.guard.dangers.get_mut(&id).unwrap();
                    d.last = frame;
                    for e in enemies {
                        if !d.enemies.contains(&e) {
                            d.enemies.push(e);
                        }
                    }
                    // The enemies came on to the haven: another.
                    let (haven, enemies) = (d.haven, d.enemies.clone());
                    if self.comguard_haven_margin(haven, &enemies) <= 0.0 {
                        let haven = self.comguard_pick_haven(unit, &enemies);
                        self.eco.guard.dangers.get_mut(&id).unwrap().haven = haven;
                        self.comguard_retreat(id, haven);
                    }
                    return;
                }
                self.comguard_flee(unit, frame, enemies, &v);
            }
            v => {
                if hit && let Some(v) = &v {
                    self.eco.guard.hit_judged += 1;
                    if frame - watch.last_emit >= HIT_EMIT {
                        self.eco.guard.watch.entry(id).or_default().last_emit = frame;
                        self.comguard_emit("hit", unit, v, None);
                    }
                }
                let ended = self.eco.guard.dangers.get(&id).is_some_and(|d| !d.released && frame - d.last > HOLD);
                if ended {
                    let d = self.eco.guard.dangers.get_mut(&id).unwrap();
                    d.released = true;
                    let held_s = (frame - d.since) / 30;
                    self.op_emit(
                        "comguard",
                        json!({"event": "release", "t": frame / 30, "id": id, "held_s": held_s,
                        "hp": unit.health().round(), "why": if v.is_none() { "no fight" } else { "lives" }}),
                    );
                }
            }
        }
    }

    /// The commander's fate in its local fight; `None` without an armed
    /// enemy about, or when the fight cannot be judged.
    fn comguard_judge(&mut self, com: Unit) -> Option<Verdict> {
        let frame = self.frame;
        let pos = com.pos();
        let armed_near = self.war.targets.values().any(|e| {
            !e.is_dead
                && e.can_attack()
                && !e.is_aa
                && (e.is_static || frame - e.last_seen <= 90)
                && distance(e.position, pos) <= SUPPORT
        });
        if !armed_near {
            return None;
        }
        // Every armed friendly unit about, whoever drives it (squads, QRF,
        // garrisons, raiders, turrets, allies), the nearest first.
        let mut friends: Vec<(f32, Unit)> = (self.cb.friendly_units_in(pos, FRIENDS).into_iter())
            .filter(|u| {
                u.id() != com.id()
                    && u.health() > 0.0
                    && !u.is_being_built()
                    && !self.retreat_handler.is_retreating(u.id())
                    && u.def().is_some_and(|d| d.is_able_to_attack() && !d.weapon_mounts().is_empty())
            })
            .map(|u| (distance(u.pos(), pos), u))
            .collect();
        friends.sort_by(|a, b| a.0.total_cmp(&b.0));
        let units: Vec<Unit> =
            std::iter::once(com).chain(friends.into_iter().take(MAX_FRIENDS).map(|(_, u)| u)).collect();
        let mut local = self.local_fight(&units)?;
        // Turrets it is not in range of: the a-move would walk it in, the
        // commander building beside them does not.
        let n = local.ours.len();
        let keep: Vec<bool> = (local.theirs.iter().zip(&local.snap.units[n..]))
            .map(|(t, u)| {
                let reach = u.spec.weapons.iter().map(|w| w.range).fold(0.0, f32::max);
                u.spec.mobile || distance(t.pos, pos) <= reach + STATIC_MARGIN
            })
            .collect();
        if keep.contains(&false) {
            let mut k = keep.iter();
            local.theirs.retain(|_| *k.next().unwrap());
            let mut i = 0;
            local.snap.units.retain(|_| {
                i += 1;
                i <= n || keep[i - 1 - n]
            });
        }
        if !local.armed_theirs() {
            return None;
        }
        let model = self.local_model()?;
        let t0 = Instant::now();
        let fates = model.kiting_fates(&local.snap, GUARD_HORIZON);
        let us = t0.elapsed().as_micros() as u64;
        let g = &mut self.eco.guard;
        g.judged += 1;
        g.micros += us;
        g.max_micros = g.max_micros.max(us);
        let i = local.ours.iter().position(|o| o.id == com.id())?;
        let f = fates.get(i)?;
        let hp = local.snap.units[i].hp;
        let max = com.max_health();
        let theirs = local.theirs.iter().map(|t| (t.id, t.def.clone(), distance(t.pos, pos), t.cost)).collect();
        Some(Verdict {
            dies: dies(f.p_raw, f.hp_left, max),
            p_raw: f.p_raw,
            p_dead: f.p_dead,
            hp,
            hp_left: f.hp_left,
            max,
            ours: local.ours.len(),
            theirs,
            unknown: local.unknown,
        })
    }

    fn comguard_flee(&mut self, unit: Unit, frame: i32, enemies: Vec<i32>, v: &Verdict) {
        let id = unit.id();
        let pos = unit.pos();
        if let Some(w) = self.eco.workers.get_mut(&id) {
            // Local defense ends here: `eco_release_commander` leaves a
            // fleeing commander to its retreat.
            w.raid_defense = None;
            w.is_chicken = true;
            w.chicken_frame = frame;
        }
        erase_one_dq(&mut self.eco.seeders, id);
        self.war.add_defense_target(DefenseTarget::new(pos, unit.max_health(), frame));
        let haven = self.comguard_pick_haven(unit, &enemies);
        self.eco.guard.flees += 1;
        self.eco.guard.dangers.insert(id, Danger { since: frame, last: frame, enemies, haven, released: false });
        self.comguard_retreat(id, haven);
        self.comguard_emit("flee", unit, v, Some(haven));
    }

    /// Send commander `id` on to `haven` (the worker retreat), jumping if
    /// it can and enemies reach it.
    pub(super) fn comguard_retreat(&mut self, id: i32, haven: Vec3) {
        let Some(unit) = self.eco.workers.get(&id).map(|w| w.unit) else { return };
        let mut nav = crate::nav!(self);
        if let Some(w) = self.eco.workers.get_mut(&id) {
            w.retreat_to(haven, &mut nav);
        }
        self.jump_escape(unit, haven);
    }

    /// The first of the incursion refuge, the closest haven and the allies'
    /// centre that no enemy of the fight reaches; else the one farthest
    /// from them.
    fn comguard_pick_haven(&mut self, unit: Unit, enemies: &[i32]) -> Vec3 {
        let pos = unit.pos();
        let candidates: Vec<Vec3> =
            [self.incursion_refuge(unit), Some(self.graph_get_closest_haven(pos)), Some(self.graph.get_ally_center())]
                .into_iter()
                .flatten()
                .collect();
        let margin = |p: Vec3| self.comguard_haven_margin(p, enemies);
        (candidates.iter().copied())
            .find(|&p| margin(p) > 0.0)
            .or_else(|| candidates.iter().copied().max_by(|&a, &b| margin(a).total_cmp(&margin(b))))
            .unwrap_or(pos)
    }

    /// How far `p` is outside the reach of the armed `enemies` (their range
    /// and [`HAVEN_MARGIN`]); negative inside it.
    fn comguard_haven_margin(&self, p: Vec3, enemies: &[i32]) -> f32 {
        (enemies.iter())
            .filter_map(|e| self.war.targets.get(e))
            .filter(|e| !e.is_dead && e.can_attack())
            .map(|e| distance(p, e.position) - e.def_max_weapon_range() - HAVEN_MARGIN)
            .fold(f32::INFINITY, f32::min)
    }

    fn comguard_emit(&self, event: &str, unit: Unit, v: &Verdict, haven: Option<Vec3>) {
        let theirs: Vec<_> = v.theirs.iter().map(|t| json!([t.1, t.2.round(), t.3.round()])).collect();
        let pos = unit.pos();
        self.op_emit(
            "comguard",
            json!({
                "event": event, "t": self.frame / 30, "id": unit.id(), "def": self.defs.name_of(unit),
                "pos": [pos.x.round(), pos.z.round()], "hp": v.hp.round(), "max": v.max.round(),
                "p_raw": (v.p_raw * 100.0).round() / 100.0, "p_dead": (v.p_dead * 100.0).round() / 100.0,
                "hp_left": v.hp_left.round(), "ours": v.ours, "theirs": theirs, "unknown": v.unknown,
                "haven": haven.map(|h| [h.x.round(), h.z.round()]),
            }),
        );
    }

    fn comguard_report(&mut self, frame: i32) {
        let g = &self.eco.guard;
        if frame - g.last_report < REPORT_PERIOD {
            return;
        }
        let (judged, hit, flees, micros, max) = (g.judged, g.hit_judged, g.flees, g.micros, g.max_micros);
        self.eco.guard.last_report = frame;
        if judged == 0 {
            return;
        }
        let mean = micros / judged.max(1);
        self.log(&format!(
            "comguard: {judged} judged ({mean} us mean, {max} max), {hit} hit and living, {flees} flees"
        ));
        self.op_emit(
            "comguard",
            json!({"event": "cost", "t": frame / 30, "judged": judged, "hit_living": hit, "flees": flees,
                   "us_mean": mean, "us_max": max}),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn death_is_p_dead_or_an_expected_end_near_zero() {
        // smoke3 g2: 2 Thugs at 270 (P(dead) 1.0), a fight it ends at 6%.
        assert!(dies(1.0, 0.0, 3250.0));
        assert!(dies(0.24, 201.0, 3250.0));
        // 4 Glaives on it beside 5 of ours: it ends at 85%.
        assert!(!dies(0.0, 2773.0, 3250.0));
        // A Warrior and Rockos took it to 44% expected: it fights on.
        assert!(!dies(0.0, 1431.0, 3250.0));
    }

    #[test]
    fn a_verdict_covers_a_hit_only_with_its_shooter_in_the_fight() {
        // Not hit lately: the verdict speaks.
        assert!(covers(None, 1000, &[]));
        assert!(covers(Some((800, Some(7))), 1000, &[]));
        // Hit by a unit in the judged fight.
        assert!(covers(Some((950, Some(7))), 1000, &[3, 7]));
        // Hit by one outside it, or unseen: the threat paint decides.
        assert!(!covers(Some((950, Some(9))), 1000, &[3, 7]));
        assert!(!covers(Some((950, None)), 1000, &[3, 7]));
    }
}
