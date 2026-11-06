//! Cyclopes (`tankheavyassault`) drive into their cannon's reach
//! (switch `cyclops`).
//!
//! A Cyclops has two guns: the Tankbuster cannon (450, a `Cannon`: ZK's
//! cannon gravity, `heightMod` 0.8, so its ballistic reach falls off
//! against anything above it, see `Gun::reach_2d`) and the slow beam (440,
//! a `BeamLaser`: `heightMod` 0.2, height barely counts). An attacking
//! unit stops as soon as any one of its weapons can fire (the engine's
//! `CMobileCAI::ExecuteObjectAttack`), so against a target that stands
//! higher than its gun (a tall turret, one on a rise) a Cyclops halts in
//! the beam's reach and only slows the target, cannon silent: 40 above, the
//! cannon reaches 425; 80 above, 396.
//!
//! The engine measures that from the gun's aim point (on the turret) to the
//! target's aim point (its middle), so a tall target such as a Desolator
//! (100 high) stands some 30 higher than the ground says; aim points are
//! taken as [`AIM_SHARE`] of the model's height.
//!
//! Every [`PERIOD`] frames a Cyclops that stands, its beam firing lately
//! and its cannon ready and quiet for a while, drives in to
//! [`CLOSE`] of the cannon's reach of its nearest ground enemy that the
//! beam reaches and the cannon does not, then attack-moves there again;
//! its handler leaves it be meanwhile (see `misc_assign_loners`). When it
//! stays so stalled [`NUDGE_AFTER`] checks running and no enemy is out of
//! the cannon's reach by that estimate (the guess was short, or terrain or
//! friends block the shot), it steps [`NUDGE`] nearer its nearest ground
//! enemy, but never inside [`NUDGE_FLOOR`] of the cannon's range.
//!
//! A cannon is not quiet when it fires at whatever it finds itself: a weapon
//! that cannot hit the unit's attack target picks its own targets (Recoil
//! `CWeapon::AllowWeaponAutoTarget`). Wires' base above it on Adamantine
//! Mountain (11-04), a Cyclops told to attack a spider factory (`facframe`)
//! or a Lotus stood in its beam's reach for minutes while its cannon killed
//! the frames on the pad, nearer and lower. So (switch `cyclopsorder`) a
//! standing Cyclops whose first order is an attack on a ground unit its
//! beam reaches drives in to [`CLOSE`] of the cannon's reach of that unit
//! when the cannon falls short of it by the estimate, and steps [`NUDGE`]
//! nearer it once the cannon has fired [`ELSEWHERE_SHOTS`] times while it
//! lost no health, cannon quiet or not.

use std::collections::BTreeMap;

use springai::{Unit, UnitDef, Vec3};

use crate::Ai;
use crate::kgbutil::kgb_move_to;
use crate::kgbutil::line_of_fire::{Flight, Gun, ZK_CANNON_GRAVITY};
use crate::util::{OPTION_SHIFT_KEY, distance};

const CMD_ATTACK: i32 = 20;

pub const CYCLOPS: &str = "tankheavyassault";

const PERIOD: i32 = 15;
/// The beam fired within this long (frames) past its own reload.
const BEAM_LIVE: i32 = 20;
/// The cannon ready and silent at least this long (frames).
const CANNON_IDLE: i32 = 45;
/// Standing: slower than this share of its top speed.
const STANDING: f32 = 0.25;
/// Its beam's target is looked for this far beyond the beam's range.
const BEAM_SLACK: f32 = 40.0;
/// Drives in to this share of the cannon's reach of the target...
const CLOSE: f32 = 0.85;
/// ...and at least this much nearer than it stands.
const MIN_STEP: f32 = 40.0;
/// Its handler leaves it be for the drive, and this long more (frames).
const HOLD: i32 = 45;
/// Enemies seen this long ago (frames) still count where they were
/// (buildings always do).
const FRESH: i32 = 45;
/// Aim points stand this share of the model's height above the ground.
const AIM_SHARE: f32 = 0.5;
/// Stalled this many checks running, it steps nearer anyway...
const NUDGE_AFTER: u32 = 4;
/// ...by this much...
const NUDGE: f32 = 80.0;
/// ...but not nearer than this share of the cannon's range.
const NUDGE_FLOOR: f32 = 0.6;
/// Cannon shots while the attack target lost no health that tell the
/// cannon fires at something else.
const ELSEWHERE_SHOTS: u32 = 2;

/// The guns of a Cyclops def: weapon indices and the cannon as a gun at the
/// origin (its reach by height).
#[derive(Clone, Copy)]
struct Guns {
    cannon: i32,
    beam: i32,
    beam_range: f32,
    gun: Gun,
}

/// A Cyclops's attack target and its cannon: the target's health when it
/// last lost some, and the cannon's shots since (reload frames seen).
#[derive(Clone, Copy, Debug, PartialEq)]
struct Watch {
    target: i32,
    hp: f32,
    shots: u32,
    reload: i32,
}

/// The watch after a check finds attack target `target` at `hp` and the
/// cannon's reload frame at `reload`.
fn watch_step(prev: Option<Watch>, target: i32, hp: f32, reload: i32) -> Watch {
    match prev {
        Some(w) if w.target == target && hp >= w.hp - 1.0 => {
            Watch { shots: w.shots + u32::from(reload != w.reload), reload, ..w }
        }
        _ => Watch { target, hp, shots: 0, reload },
    }
}

/// A drive in: how far the Cyclops stands from the enemy, how near it
/// goes, the enemy's aim above its own, the enemy, where, and its def.
type Drive = (f32, f32, f32, i32, Vec3, String);

/// A Cyclops driving in.
struct Closing {
    /// Its handler leaves it be until then.
    until: i32,
    /// The cannon's reload frame when it was sent.
    cannon_was: i32,
}

#[derive(Default)]
pub struct Cyclopes {
    guns: BTreeMap<i32, Option<Guns>>,
    closing: BTreeMap<i32, Closing>,
    /// Checks running that found each Cyclops stalled.
    streak: BTreeMap<i32, u32>,
    /// Each standing Cyclops's attack target and cannon (`cyclopsorder`).
    watch: BTreeMap<i32, Watch>,
    /// Since the summary: checks that found a Cyclops stalled (beam on,
    /// cannon quiet, standing), drives ordered, and drives after which the
    /// cannon fired before the hold ran out.
    stalls: u32,
    drives: u32,
    nudges: u32,
    fired: u32,
    /// Drives toward the attack target the cannon could not hit.
    ordered: u32,
}

impl Cyclopes {
    /// Whether `id` is driving in, so its handler should leave it be.
    pub fn holds(&self, id: i32, frame: i32) -> bool {
        self.closing.get(&id).is_some_and(|c| c.until > frame)
    }
}

/// Where a unit standing at `from` drives to put `target` `want` away: on
/// the line between them.
fn stand_off(target: Vec3, from: Vec3, want: f32) -> Vec3 {
    let d = distance(from, target).max(1.0);
    let k = want / d;
    Vec3::xz(target.x + (from.x - target.x) * k, target.z + (from.z - target.z) * k)
}

/// How near a Cyclops whose cannon is `gun` drives to a target `dh` above
/// it, `d` away: [`CLOSE`] of the cannon's reach, at least [`MIN_STEP`]
/// nearer; none if the cannon already reaches it.
fn drive_in(gun: &Gun, d: f32, dh: f32) -> Option<f32> {
    let reach = gun.reach_2d(dh);
    (d > reach).then(|| (reach * CLOSE).min(d - MIN_STEP).max(0.0))
}

/// How near a Cyclops stalled `d` from its nearest enemy steps: [`NUDGE`]
/// nearer, none if that is inside [`NUDGE_FLOOR`] of its cannon's range.
fn nudge(gun: &Gun, d: f32) -> Option<f32> {
    let want = d - NUDGE;
    (want >= gun.range * NUDGE_FLOOR).then_some(want)
}

/// How high a unit of `def` aims from or is aimed at, above its ground.
fn aim_height(def: Option<UnitDef>) -> f32 {
    def.map_or(0.0, |d| d.height() * AIM_SHARE)
}

impl Ai {
    fn cyclops_guns(&mut self, def: UnitDef) -> Option<Guns> {
        if let Some(g) = self.cyclopes.guns.get(&def.id()) {
            return *g;
        }
        let mounts: Vec<_> =
            def.weapon_mounts().into_iter().filter_map(|m| Some((m.index(), m.weapon_def()?))).collect();
        let find = |kind: &str| mounts.iter().find(|(_, w)| w.weapon_type() == kind);
        let g = match (find("Cannon"), find("BeamLaser")) {
            (Some((ci, cw)), Some((bi, bw))) => {
                let speed = cw.projectile_speed();
                Some(Guns {
                    cannon: *ci,
                    beam: *bi,
                    beam_range: bw.range(),
                    gun: Gun {
                        x: 0.0,
                        z: 0.0,
                        muzzle: 0.0,
                        range: cw.range(),
                        height_mod: cw.height_mod(),
                        aoe: cw.area_of_effect(),
                        body: 0.0,
                        flight: Flight::Arc {
                            speed: if speed < 100.0 { speed * 30.0 } else { speed },
                            gravity: ZK_CANNON_GRAVITY,
                        },
                        avoids_friends: false,
                        through_ground: false,
                    },
                })
            }
            _ => None,
        };
        self.cyclopes.guns.insert(def.id(), g);
        g
    }

    pub fn cyclops_update(&mut self, frame: i32) {
        if frame % 1800 == (self.offset + 23) % 1800 && self.cyclopes.stalls + self.cyclopes.drives > 0 {
            let c = &self.cyclopes;
            self.log(&format!(
                "cyclops: in the last minute {} checks found a Cyclops stalled (beam on, cannon quiet or not on its \
                 target), {} drives in ({} nudges, {} toward the target it was ordered to attack), the cannon fired \
                 after {}{}",
                c.stalls,
                c.drives,
                c.nudges,
                c.ordered,
                c.fired,
                if self.dev_off("cyclops") { " (cyclops off)" } else { "" }
            ));
            self.cyclopes.stalls = 0;
            self.cyclopes.drives = 0;
            self.cyclopes.nudges = 0;
            self.cyclopes.fired = 0;
            self.cyclopes.ordered = 0;
        }
        if frame % PERIOD != (self.offset + 8) % PERIOD {
            return;
        }
        // Drives that ran out: did the cannon fire?
        let done: Vec<i32> =
            (self.cyclopes.closing.iter()).filter(|(_, c)| c.until <= frame).map(|(&id, _)| id).collect();
        for id in done {
            let Some(c) = self.cyclopes.closing.remove(&id) else { continue };
            let fired = Unit::from_id(self.cb, id).and_then(|u| Some((u, self.cyclops_guns(u.def()?)?))).is_some_and(
                |(u, g)| u.weapons().get(g.cannon as usize).is_some_and(|w| w.reload_frame() > c.cannon_was),
            );
            if fired {
                self.cyclopes.fired += 1;
            }
        }
        let ids: Vec<(i32, Unit)> = (self.fighters.iter())
            .filter(|(_, f)| self.defs.name_of(f.unit) == CYCLOPS)
            .map(|(&id, f)| (id, f.unit))
            .collect();
        for (id, unit) in ids {
            if self.cyclopes.holds(id, frame) || self.retreat_handler.is_retreating(id) {
                continue;
            }
            self.cyclops_check(unit, frame);
        }
    }

    fn cyclops_check(&mut self, unit: Unit, frame: i32) {
        if !self.cyclops_stalled(unit, frame) {
            self.cyclopes.streak.remove(&unit.id());
        }
    }

    /// The ground unit a standing Cyclops at `pos` was ordered to attack
    /// (its first order), if the beam reaches it and the cannon cannot hit
    /// it: out of the cannon's reach by the estimate (a drive in), or fired
    /// [`ELSEWHERE_SHOTS`] times while it lost no health (a nudge); with
    /// whether it is the latter. Keeps the watch on the target either way.
    fn cyclops_ordered(&mut self, unit: Unit, g: &Guns, pos: Vec3, own_aim: f32, reload: i32) -> Option<(Drive, bool)> {
        let first = unit.current_commands().into_iter().next();
        let target = first.map(|c| (c.id(), c.params())).filter(|(id, p)| *id == CMD_ATTACK && p.len() == 1);
        let Some(e) = target.and_then(|(_, p)| self.war.targets.get(&(p[0] as i32))) else {
            self.cyclopes.watch.remove(&unit.id());
            return None;
        };
        if e.is_dead || !e.visible || e.can_fly() {
            self.cyclopes.watch.remove(&unit.id());
            return None;
        }
        let (tid, at, hp) = (e.unit_id, e.unit.pos(), e.unit.health());
        let d = distance(pos, at);
        let dh = at.y + aim_height(e.ud) - (pos.y + own_aim);
        let name = e.def_name.clone();
        let w = watch_step(self.cyclopes.watch.get(&unit.id()).copied(), tid, hp, reload);
        self.cyclopes.watch.insert(unit.id(), w);
        if d > g.beam_range + BEAM_SLACK {
            return None;
        }
        if let Some(want) = drive_in(&g.gun, d, dh) {
            return Some(((d, want, dh, tid, at, name), false));
        }
        if w.shots >= ELSEWHERE_SHOTS {
            return nudge(&g.gun, d).map(|want| ((d, want, dh, tid, at, name), true));
        }
        None
    }

    /// Checks whether `unit` stands stalled (beam on, cannon quiet or not
    /// on its attack target) and if so drives it in; whether it was stalled.
    fn cyclops_stalled(&mut self, unit: Unit, frame: i32) -> bool {
        if unit.is_being_built() || unit.health() <= 0.0 || unit.is_paralyzed() {
            return false;
        }
        let Some(def) = unit.def() else { return false };
        let Some(g) = self.cyclops_guns(def) else { return false };
        let weapons = unit.weapons();
        let (Some(cannon), Some(beam)) = (weapons.get(g.cannon as usize), weapons.get(g.beam as usize)) else {
            return false;
        };
        // The beam fired lately (its reload frame is when it fired plus its
        // reload); the cannon has been ready a while and has not, or it
        // cannot hit the unit's attack target.
        let cannon_was = cannon.reload_frame();
        if beam.reload_frame() < frame - BEAM_LIVE {
            return false;
        }
        let quiet = frame - cannon_was >= CANNON_IDLE;
        let v = unit.vel();
        if v.x.hypot(v.z) * 30.0 > STANDING * def.speed() {
            return false;
        }
        if unit.rules_param_float("disarmed", 0.0) > 0.0 {
            return false;
        }
        let pos = unit.pos();
        let own_aim = aim_height(Some(def));
        let ordered =
            if self.dev_off("cyclopsorder") { None } else { self.cyclops_ordered(unit, &g, pos, own_aim, cannon_was) };
        if !quiet && ordered.is_none() {
            return false;
        }
        let streak = {
            let s = self.cyclopes.streak.entry(unit.id()).or_insert(0);
            *s += 1;
            *s
        };
        self.cyclopes.stalls += 1;
        let toward_order = ordered.is_some();
        let (choice, how) = match ordered {
            Some((drive, elsewhere)) => {
                let how = if elsewhere {
                    format!(
                        "its cannon fired {ELSEWHERE_SHOTS} shots and the target it attacks lost no health, nudges in"
                    )
                } else {
                    "the target it attacks is out of the cannon's reach, drives in".to_string()
                };
                (Some(drive), how)
            }
            None => {
                // Its nearest ground enemy that the beam reaches and the
                // cannon not; failing one, after a streak, its nearest
                // ground enemy.
                let mut best: Option<Drive> = None;
                let mut near: Option<Drive> = None;
                for e in self.war.targets.values() {
                    if e.is_dead || e.can_fly() || (!e.is_static && frame - e.last_seen > FRESH) {
                        continue;
                    }
                    let live = e.unit.pos();
                    let at = if e.visible && (live.x != 0.0 || live.z != 0.0) { live } else { e.position };
                    let d = distance(pos, at);
                    if d > g.beam_range + BEAM_SLACK {
                        continue;
                    }
                    let dh = at.y + aim_height(e.ud) - (pos.y + own_aim);
                    if best.as_ref().is_none_or(|b| d < b.0)
                        && let Some(want) = drive_in(&g.gun, d, dh)
                    {
                        best = Some((d, want, dh, e.unit_id, at, e.def_name.clone()));
                    }
                    if streak >= NUDGE_AFTER
                        && d <= g.beam_range
                        && near.as_ref().is_none_or(|b| d < b.0)
                        && let Some(want) = nudge(&g.gun, d)
                    {
                        near = Some((d, want, dh, e.unit_id, at, e.def_name.clone()));
                    }
                }
                let how = if best.is_none() && near.is_some() {
                    format!("stalled {streak} checks, nudges in")
                } else {
                    "drives in".to_string()
                };
                (best.or(near), how)
            }
        };
        let Some((d, want, dh, tid, at, name)) = choice else { return true };
        if self.dev_off("cyclops") {
            return true;
        }
        let to = stand_off(at, pos, want);
        kgb_move_to(unit, to, 0, frame);
        unit.fight(at, OPTION_SHIFT_KEY);
        let drive = (distance(pos, to) / (def.speed() / 30.0).max(0.5)) as i32;
        self.cyclopes.closing.insert(unit.id(), Closing { until: frame + drive + HOLD, cannon_was });
        self.cyclopes.drives += 1;
        self.cyclopes.streak.remove(&unit.id());
        self.cyclopes.watch.remove(&unit.id());
        if toward_order {
            self.cyclopes.ordered += 1;
        }
        if how.contains("nudges") {
            self.cyclopes.nudges += 1;
        }
        self.log(&format!(
            "cyclops: {} stands {d:.0} from {name} {tid} ({dh:+.0} aim above it: the cannon reaches {:.0}, the beam \
             {:.0}); {how} to {want:.0}",
            unit.id(),
            g.gun.reach_2d(dh),
            g.beam_range,
        ));
        true
    }

    pub fn cyclops_remove_unit(&mut self, id: i32) {
        self.cyclopes.closing.remove(&id);
        self.cyclopes.streak.remove(&id);
        self.cyclopes.watch.remove(&id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cannon() -> Gun {
        Gun {
            x: 0.0,
            z: 0.0,
            muzzle: 0.0,
            range: 450.0,
            height_mod: 0.8,
            aoe: 32.0,
            body: 0.0,
            flight: Flight::Arc { speed: 270.0, gravity: ZK_CANNON_GRAVITY },
            avoids_friends: false,
            through_ground: false,
        }
    }

    #[test]
    fn the_cannon_falls_short_of_the_beam_against_higher_targets() {
        let g = cannon();
        assert!((g.reach_2d(0.0) - 450.0).abs() < 1.0, "{}", g.reach_2d(0.0));
        let up40 = g.reach_2d(40.0);
        assert!((415.0..435.0).contains(&up40), "{up40}");
        let up80 = g.reach_2d(80.0);
        assert!((385.0..405.0).contains(&up80), "{up80}");
        // Downhill it reaches past the beam.
        assert!(g.reach_2d(-60.0) > 450.0);
    }

    #[test]
    fn drives_in_only_when_the_cannon_falls_short() {
        let g = cannon();
        assert_eq!(drive_in(&g, 430.0, 0.0), None);
        let want = drive_in(&g, 435.0, 80.0).unwrap();
        assert!((330.0..345.0).contains(&want), "{want}");
        // Never less than a step nearer, never past the target.
        let want = drive_in(&g, 400.0, 80.0).unwrap();
        assert!(want <= 360.0, "{want}");
        let p = stand_off(Vec3::xz(0.0, 0.0), Vec3::xz(400.0, 0.0), 340.0);
        assert!((p.x - 340.0).abs() < 0.01 && p.z.abs() < 0.01);
    }

    #[test]
    fn the_watch_counts_cannon_shots_while_the_target_loses_no_health() {
        let w = watch_step(None, 7, 4000.0, 100);
        assert_eq!(w.shots, 0);
        // Same reload frame: no shot. A new one: a shot. Healing is no hit.
        let w = watch_step(Some(w), 7, 4000.0, 100);
        assert_eq!(w.shots, 0);
        let w = watch_step(Some(w), 7, 4000.0, 220);
        let w = watch_step(Some(w), 7, 4025.0, 340);
        assert_eq!(w.shots, 2);
        // The target lost health: shots count from there.
        let hit = watch_step(Some(w), 7, 3000.0, 460);
        assert_eq!((hit.shots, hit.hp), (0, 3000.0));
        // Another target: a new watch.
        let other = watch_step(Some(w), 8, 785.0, 460);
        assert_eq!((other.target, other.shots), (8, 0));
    }

    #[test]
    fn nudges_stop_at_the_floor() {
        let g = cannon();
        assert_eq!(nudge(&g, 400.0), Some(320.0));
        assert_eq!(nudge(&g, 350.0), Some(270.0));
        assert_eq!(nudge(&g, 340.0), None);
    }
}
