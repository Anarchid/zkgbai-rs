//! `comguard` (on; `disable=comguard`): a commander leaves a fight the
//! fight predictor says it loses, before the first shots land, and keeps
//! out of it while it lasts.
//!
//! The worker retreat (`assign`) asks the threat paint: a commander flees
//! when the enemy paint is twice ours (its own paint included, and a
//! commander paints a lot) or when hurt in enemy paint, unless building a
//! turret; it comes back once its spot has had no enemy paint for 3 s.
//! In the lobby games of Nov 2 the commander died in 10 of 40, each time
//! to an army seen closing for 20-30 s (5 Reapers, 4 Ravagers and a Mace,
//! Halberds, 3 Buoys outranging it) while it built, repaired or stood at
//! full health; it turned only when hurt, and once went back to a mex job
//! inside the Buoys' range (user, 2026-11-03: "steps back when fired at,
//! then goes and builds something while ignoring continued incoming
//! fire").
//!
//! Every [`PERIOD`] frames, each commander with an armed enemy within
//! [`SUPPORT`] asks the predictor about its local fight
//! (`fightsim::local`: every identified enemy about, the commander and our
//! armed units and turrets within [`FRIENDS`]), ZK's tactical AI moving
//! the units it governs (`kiting_fates`: Buoys keep their range), over
//! [`HORIZON`] (10 s). The commander is in danger when the sim's own
//! P(dead) reaches [`DEATH`], or when it comes out under [`CRIPPLED`] of
//! its health having lost [`LOSS`] of it (3 Buoys at 420: 57% gone, none
//! of them hurt). Enemy turrets it is out of range of stay out of the
//! fight (the a-move would walk it in); unidentified contacts are left out
//! (the threat rules still see them).
//!
//! In danger it drops local defense, flees (the worker retreat, jumps
//! included) to the first of the incursion refuge, the closest haven and
//! the allies' centre that the enemies in the fight do not reach, and asks
//! for defense. The danger holds [`HOLD`] past its last verdict: the
//! retreat does not end nor local defense take it meanwhile. For
//! [`AVOID`] after, it takes no job the enemies of that fight reach.
//!
//! Journal (`k=comguard`): `flee` (the fight and its fate), `release`,
//! one `safe` judgement in [`SAMPLE`], and `cost` every minute.

use std::collections::BTreeMap;
use std::time::Instant;

use serde_json::json;
use springai::{Unit, Vec3, reserved};

use super::erase_one_dq;
use crate::Ai;
use crate::military::DefenseTarget;
use crate::military::fightsim::local::{HORIZON, SUPPORT};
use crate::util::distance;

pub const FEATURE: &str = "comguard";

/// Frames between judgements.
const PERIOD: i32 = 10;
/// Our armed units and turrets this close to the commander fight beside it.
const FRIENDS: f32 = 600.0;
/// The sim's own P(dead) at which the commander leaves...
const DEATH: f32 = 0.5;
/// ...or the health share it must not end below, having lost this share.
const CRIPPLED: f32 = 0.5;
const LOSS: f32 = 0.15;
/// The danger holds this long past its last verdict (frames).
const HOLD: i32 = 150;
/// Jobs the danger's enemies reach are off this long after (frames)...
const AVOID: i32 = 900;
/// ...within their range and this margin.
const AVOID_MARGIN: f32 = 350.0;
/// Mobile enemies unseen for longer do not mark jobs off (frames).
const AVOID_SEEN: i32 = 600;
/// Enemy turrets farther than their range and this margin are left out.
const STATIC_MARGIN: f32 = 100.0;
/// A haven within an enemy's range and this margin is no haven.
const HAVEN_MARGIN: f32 = 400.0;
/// One safe judgement in this many is journaled.
const SAMPLE: u64 = 20;
const REPORT_PERIOD: i32 = 1800;

/// A commander in danger, or lately.
struct Danger {
    since: i32,
    last: i32,
    /// The enemies of the fight that set it.
    enemies: Vec<i32>,
    haven: Vec3,
    /// Released (no longer holding the commander), still marking jobs off.
    released: bool,
}

#[derive(Default)]
pub struct ComGuard {
    dangers: BTreeMap<i32, Danger>,
    judged: u64,
    safe: u64,
    flees: u64,
    micros: u64,
    max_micros: u64,
    last_report: i32,
}

/// One judgement of a commander's fight.
struct Verdict {
    danger: bool,
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

/// Whether a commander with `hp` of `max` health and this fate is in
/// danger.
fn in_danger(p_raw: f32, hp: f32, hp_left: f32, max: f32) -> bool {
    let max = max.max(1.0);
    p_raw >= DEATH || (hp_left < CRIPPLED * max && hp - hp_left >= LOSS * max)
}

impl Ai {
    pub(super) fn eco_guard_commanders(&mut self, frame: i32) {
        if frame % PERIOD != self.offset % PERIOD {
            return;
        }
        self.comguard_report(frame);
        if self.dev_off(FEATURE) {
            self.eco.guard.dangers.clear();
            return;
        }
        let commanders = self.eco.commanders.clone();
        self.eco.guard.dangers.retain(|id, d| commanders.contains(id) && frame - d.last <= HOLD + AVOID);
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
        let held = self.comguard_holds(id);
        match verdict {
            Some(v) if v.danger => {
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
                if v.as_ref().is_some_and(|v| !v.danger) {
                    self.eco.guard.safe += 1;
                    if self.eco.guard.safe.is_multiple_of(SAMPLE) {
                        let v = v.as_ref().unwrap();
                        self.comguard_emit("safe", unit, v, None);
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
                        "hp": unit.health().round(), "why": if v.is_none() { "no fight" } else { "safe" }}),
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
        let mut units = vec![com];
        units.extend(
            (self.fighters.values())
                .filter(|f| {
                    f.unit.health() > 0.0
                        && !self.retreat_handler.is_retreating(f.unit.id())
                        && distance(f.get_pos(), pos) <= FRIENDS
                })
                .map(|f| f.unit),
        );
        units.extend(
            (self.eco.porcs.iter())
                .filter(|p| !p.is_being_built() && p.health() > 0.0 && distance(p.pos(), pos) <= FRIENDS)
                .copied(),
        );
        units.extend(
            (self.eco.commanders.iter())
                .filter(|&&c| c != com.id())
                .filter_map(|c| self.eco.workers.get(c))
                .filter(|w| w.unit.health() > 0.0 && distance(w.get_pos(), pos) <= FRIENDS)
                .map(|w| w.unit),
        );
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
        let fates = model.kiting_fates(&local.snap, HORIZON);
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
            danger: in_danger(f.p_raw, hp, f.hp_left, max),
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
        let (judged, safe, flees, micros, max) = (g.judged, g.safe, g.flees, g.micros, g.max_micros);
        self.eco.guard.last_report = frame;
        if judged == 0 {
            return;
        }
        let mean = micros / judged.max(1);
        self.log(&format!("comguard: {judged} judged ({mean} us mean, {max} max), {flees} flees, {safe} safe"));
        self.op_emit(
            "comguard",
            json!({"event": "cost", "t": frame / 30, "judged": judged, "safe": safe, "flees": flees,
                   "us_mean": mean, "us_max": max}),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn danger_is_death_or_a_crippling_loss() {
        // Dies.
        assert!(in_danger(0.6, 3800.0, 0.0, 3800.0));
        // 3 Buoys at 420: 57% of its health gone, none of them hurt.
        assert!(in_danger(0.0, 3800.0, 1623.0, 3800.0));
        // 3 Glaives: it kills them and keeps 58%.
        assert!(!in_danger(0.0, 3800.0, 2213.0, 3800.0));
        // Already hurt, a lone raider scratches it: no reason to leave.
        assert!(!in_danger(0.0, 1500.0, 1400.0, 3800.0));
        // Already hurt and losing a sixth more: leave.
        assert!(in_danger(0.0, 1500.0, 900.0, 3800.0));
    }
}
