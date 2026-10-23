//! Jumpjets: the units that jump (ZK `canjump`: the jump factory's but
//! Moderators, Placeholders, Firewalkers and Puppies; recon commanders).
//!
//! ZK's unit_jumpjets: `CMD_JUMP [x, y, z]` walks the unit into range,
//! waits for the reload (10 s; Jugglenaut 15, commanders 20), then flies a
//! parabola `jump_height` high. Ground rising within 30 of the arc cancels
//! the jump, as does a landing too steep to walk; a unit waiting for its
//! reload stands still, so a jump is only ordered when it can go at once.
//!
//! Uses: escapes (a retreating unit or a fleeing constructor jumps the
//! first stretch of its way home: over the chasers, often over a cliff),
//! Jacks jumping onto a target worth it, Pyros into a clump of light
//! units, Jugglenauts into a crowd (the landing hits 1000 in 340), and
//! Skuttles onto their heavy (see `jaeger`). Jugglenauts pull with their
//! gravity guns and push while they retreat, throwing chasers off.

use std::collections::BTreeMap;

use springai::{NO_TIMEOUT, Unit, UnitDef, Vec3};

use crate::Ai;
use crate::defs::cmd;
use crate::kgbutil::{PathCost, kgb_move_to};
use crate::util::{NULLPOS, OPTION_SHIFT_KEY, distance};

/// Frames between engagement checks.
const PERIOD: i32 = 10;

/// Jumps reach this share of their range (the gadget measures in 2D from
/// the unit, which drifts while the order goes out).
const REACH: f32 = 0.92;

/// The ground this much above the arc cancels a jump (the gadget's 30, and
/// some for the start-of-game height map in between).
const ARC_CLEARANCE: f32 = 36.0;

/// An escape jump goes at least this far along the way home.
const MIN_ESCAPE: f32 = 120.0;

/// Not worth a jump: a target this close is walked to.
const MIN_ENGAGE: f32 = 140.0;

/// Steepest landing (rise over run) a bot walks off.
const MAX_LANDING_SLOPE: f32 = 0.7;

/// A target last seen longer ago than this is not jumped at.
const FRESH: i32 = 45;

/// Jugglenaut landings (1000 damage within 170, falling off fast from
/// the middle, ours hit too): worth one from this much enemy metal within
/// this, and none with more than this much of ours there.
const SUMO_CROWD: f32 = 700.0;
const SUMO_SPLASH: f32 = 150.0;
const SUMO_FRIENDS: f32 = 300.0;

/// Pyros: at least this many light enemies within this of the landing.
/// The jump's 10 s reload strands a Pyro that jumps in where it loses, so
/// only where we are already the stronger (they jump out more than in).
const PYRO_CLUMP: usize = 2;
const PYRO_SPLASH: f32 = 160.0;
const PYRO_EDGE: f32 = 0.8;

/// A jump order still first in the queue after this many frames failed
/// (the gadget leaves a jump it cannot make queued, and the unit stands).
const STALL: i32 = 45;
/// Light: up to this much health.
const LIGHT_HEALTH: f32 = 1300.0;

/// What a def's jumps are like (from its custom params).
#[derive(Clone, Copy, Debug)]
pub struct JumpDef {
    pub range: f32,
    pub height: f32,
    pub speed: f32,
    pub reload: i32,
    /// Deepest water it may land in.
    max_depth: f32,
}

/// A jump order not yet seen taking off.
struct Pending {
    unit: Unit,
    /// Frame of the order.
    at: i32,
    to: Vec3,
    /// The order was seen first in the unit's queue.
    queued: bool,
}

#[derive(Default)]
pub struct Jumps {
    defs: BTreeMap<i32, Option<JumpDef>>,
    /// Frame of each unit's last jump order.
    last: BTreeMap<i32, i32>,
    /// Jump orders not yet seen taking off.
    pending: Vec<Pending>,
    /// Since the summary: jumps seen in the air, orders gone without a jump
    /// (replaced by another order), orders never seen queued (refused, or
    /// replaced at once), jumps that stalled.
    pub flew: u32,
    pub replaced: u32,
    pub refused: u32,
    pub stalled: u32,
    /// Jugglenauts we drive, and whether each pushes now.
    sumos: BTreeMap<i32, bool>,
    /// Jump orders by use, since the last summary.
    pub counts: BTreeMap<String, u32>,
    pub total: u32,
}

fn param(ud: UnitDef, key: &str) -> Option<f32> {
    ud.custom_param(key).and_then(|v| v.trim().parse::<f32>().ok())
}

/// Ground position of `p` on the live map.
fn ground(ai: &Ai, p: Vec3) -> Vec3 {
    Vec3::new(p.x, ai.cb.map().elevation_at(p.x, p.z), p.z)
}

impl Ai {
    /// The jumps of def `ud`, if it jumps (and the gadget drives them).
    pub fn jump_def(&mut self, ud: UnitDef) -> Option<JumpDef> {
        if let Some(d) = self.jumps.defs.get(&ud.id()) {
            return *d;
        }
        let d = (ud.custom_param("canjump").is_some_and(|v| v.trim() == "1")
            && param(ud, "no_jump_handling").is_none_or(|v| v != 1.0))
        .then(|| JumpDef {
            range: param(ud, "jump_range").unwrap_or(400.0),
            height: param(ud, "jump_height").unwrap_or(200.0),
            speed: param(ud, "jump_speed").unwrap_or(6.0).max(1.0),
            reload: (param(ud, "jump_reload").unwrap_or(10.0) * 30.0) as i32,
            max_depth: ud.max_water_depth(),
        });
        self.jumps.defs.insert(ud.id(), d);
        d
    }

    /// The jumps of `unit` if it can jump right now.
    pub fn jump_ready(&mut self, unit: Unit) -> Option<JumpDef> {
        if self.dev_off("jumps") || unit.is_being_built() || unit.health() <= 0.0 {
            return None;
        }
        let jd = self.jump_def(unit.def()?)?;
        let last = self.jumps.last.get(&unit.id()).copied();
        if last.is_some_and(|f| self.frame - f < jd.reload + 15) {
            return None;
        }
        // The gadget's own reload and flight (readable by the owner; when
        // not, the frame count above stands).
        if unit.rules_param_float("is_jumping", 0.0) == 1.0
            || unit.rules_param_float("jumpReload", 1.0) < 1.0
            || unit.rules_param_float("disarmed", 0.0) == 1.0
        {
            return None;
        }
        Some(jd)
    }

    /// Whether a jump from `from` to `to` clears the ground and lands
    /// where the unit can stand.
    pub fn jump_clear(&self, jd: JumpDef, from: Vec3, to: Vec3) -> bool {
        let map = self.cb.map();
        let (w, h) = (map.width() as f32 * 8.0, map.height() as f32 * 8.0);
        if to.x < 16.0 || to.z < 16.0 || to.x > w - 16.0 || to.z > h - 16.0 {
            return false;
        }
        let start = map.elevation_at(from.x, from.z).max(from.y.min(map.elevation_at(from.x, from.z) + 40.0));
        let end = map.elevation_at(to.x, to.z);
        if end < -jd.max_depth {
            return false;
        }
        // Landing slope.
        let d = 12.0;
        let dx = (map.elevation_at(to.x + d, to.z) - map.elevation_at(to.x - d, to.z)) / (2.0 * d);
        let dz = (map.elevation_at(to.x, to.z + d) - map.elevation_at(to.x, to.z - d)) / (2.0 * d);
        if (dx * dx + dz * dz).sqrt() > MAX_LANDING_SLOPE {
            return false;
        }
        // The arc, as the gadget samples it.
        let (vx, vy, vz) = (to.x - from.x, end - start, to.z - from.z);
        let steps = 40;
        for k in 1..steps {
            let i = k as f32 / steps as f32;
            let arc = start + vy * i + (1.0 - (2.0 * i - 1.0).powi(2)) * jd.height;
            if map.elevation_at(from.x + vx * i, from.z + vz * i) - ARC_CLEARANCE > arc {
                return false;
            }
        }
        true
    }

    /// Order `unit` to jump to `to` now (replacing its orders); `why` is
    /// counted in the summary.
    fn jump_order(&mut self, unit: Unit, to: Vec3, why: &str) {
        let to = ground(self, to);
        unit.execute_custom_command(cmd::JUMP, &[to.x, to.y, to.z], 0, NO_TIMEOUT);
        self.jumps.last.insert(unit.id(), self.frame);
        self.jumps.pending.push(Pending { unit, at: self.frame, to, queued: false });
        *self.jumps.counts.entry(why.to_owned()).or_default() += 1;
        self.jumps.total += 1;
    }

    /// Flight time of a jump of `jd` over `dist`, frames.
    fn jump_flight(jd: JumpDef, dist: f32) -> f32 {
        let half = (dist * dist / 4.0 + jd.height * jd.height).sqrt();
        2.0 * half / jd.speed
    }

    /// Jump `unit` onto `target` (leading it; at `known`, its last known
    /// position, when it is out of sight) and attack it on landing, if it
    /// can now and it is in reach; `why` is counted.
    pub fn jump_strike(&mut self, unit: Unit, target: Unit, known: Vec3, why: &str) -> bool {
        let Some(jd) = self.jump_ready(unit) else { return false };
        let seen = target.pos();
        let at = if seen == NULLPOS || (seen.x == 0.0 && seen.z == 0.0) { known } else { seen };
        let from = unit.pos();
        if at == NULLPOS {
            return false;
        }
        let lead = Self::jump_flight(jd, distance(from, at)) * 0.5;
        let v = target.vel();
        let to = Vec3::new(at.x + (v.x * lead).clamp(-150.0, 150.0), at.y, at.z + (v.z * lead).clamp(-150.0, 150.0));
        let d = distance(from, to);
        if !(MIN_ENGAGE * 0.5..=jd.range * REACH).contains(&d) || !self.jump_clear(jd, from, to) {
            return false;
        }
        self.jump_order(unit, to, why);
        unit.attack(target, OPTION_SHIFT_KEY);
        true
    }

    /// Jump `unit` to `to` if it can now, `to` is in reach and the arc is
    /// clear; `why` is counted.
    pub fn jump_to(&mut self, unit: Unit, to: Vec3, why: &str) -> bool {
        let Some(jd) = self.jump_ready(unit) else { return false };
        let from = unit.pos();
        let d = distance(from, to);
        if !(MIN_ENGAGE * 0.5..=jd.range * REACH).contains(&d) || !self.jump_clear(jd, from, to) {
            return false;
        }
        self.jump_order(unit, to, why);
        true
    }

    /// A unit on its way to `home` jumps the first stretch of the way, if
    /// it can and enemies reach it here. (The way is found anew: the retreat
    /// orders just given reach the engine a frame later, so the unit's queue
    /// still holds what it did before, a raider's way to its target, say.)
    pub fn jump_escape(&mut self, unit: Unit, home: Vec3) {
        let Some(def) = unit.def() else { return };
        if home == NULLPOS || self.jump_def(def).is_none() {
            return;
        }
        let pos = unit.pos();
        if self.war.view(Some(def)).get_threat(pos) <= 0.0 {
            return;
        }
        let Some(jd) = self.jump_ready(unit) else { return };
        let way: Vec<Vec3> =
            self.pathfinder.find_path(&self.war, unit, home, PathCost::AvoidEnemies).into_iter().collect();
        if way.is_empty() {
            return;
        }
        // The farthest point along it within reach that a jump gets to.
        let reach = jd.range * REACH;
        let mut best: Option<(Vec3, usize)> = None;
        let mut prev = pos;
        let mut along = 0.0;
        'way: for (i, &wp) in way.iter().enumerate() {
            let seg = distance(prev, wp);
            let n = (seg / 32.0).ceil().max(1.0) as i32;
            for k in 1..=n {
                let t = k as f32 / n as f32;
                let p = Vec3::new(prev.x + (wp.x - prev.x) * t, 0.0, prev.z + (wp.z - prev.z) * t);
                if distance(pos, p) > reach {
                    break 'way;
                }
                if along + seg * t >= MIN_ESCAPE && self.jump_clear(jd, pos, p) {
                    best = Some((p, i));
                }
            }
            along += seg;
            prev = wp;
        }
        let Some((to, i)) = best else { return };
        let why = format!("escape {}", self.defs.name(def));
        self.jump_order(unit, to, &why);
        for &wp in &way[i..] {
            if distance(to, wp) > 16.0 {
                kgb_move_to(unit, wp, OPTION_SHIFT_KEY, self.frame);
            }
        }
    }

    /// Follow jump orders: seen in the air, or still queued and not taking
    /// off (walk there instead), or gone without a jump. AI orders reach the
    /// engine a frame or more after they are given, so the queue is judged
    /// only once STALL frames have passed.
    fn jumps_watch(&mut self, frame: i32) {
        let pending = std::mem::take(&mut self.jumps.pending);
        let trace = std::env::var_os("ZKGBAI_JUMP_TRACE").is_some();
        for mut j in pending {
            let unit = j.unit;
            if unit.health() <= 0.0 || unit.team() != self.team_id {
                continue;
            }
            let p = unit.pos();
            let up = p.y - self.cb.map().elevation_at(p.x, p.z);
            let first = unit.current_commands().first().map(|c| c.id());
            if trace {
                let cmds: Vec<String> = (unit.current_commands().iter())
                    .map(|c| format!("{}{:?}", c.id(), c.params().iter().map(|v| *v as i32).collect::<Vec<_>>()))
                    .collect();
                self.log(&format!(
                    "jumps: trace {} {} +{} at ({:.0}, {:.0}) up {:.0} to ({:.0}, {:.0}) is_jumping {} reload {} queue {}",
                    unit.id(),
                    self.defs.name_of(unit),
                    frame - j.at,
                    p.x,
                    p.z,
                    up,
                    j.to.x,
                    j.to.z,
                    unit.rules_param_float("is_jumping", -1.0),
                    unit.rules_param_float("jumpReload", -1.0),
                    cmds.join(" ")
                ));
            }
            if unit.rules_param_float("is_jumping", 0.0) == 1.0 || up > 60.0 {
                self.jumps.flew += 1;
                continue;
            }
            j.queued |= first == Some(cmd::JUMP);
            if frame - j.at < STALL {
                self.jumps.pending.push(j);
                continue;
            }
            if first == Some(cmd::JUMP) {
                self.jumps.stalled += 1;
                kgb_move_to(unit, j.to, 0, frame);
            } else if j.queued {
                self.jumps.replaced += 1;
            } else {
                self.jumps.refused += 1;
            }
        }
    }

    pub fn jumps_remove_unit(&mut self, unit: Unit) {
        self.jumps.last.remove(&unit.id());
        self.jumps.sumos.remove(&unit.id());
    }

    pub fn jumps_update(&mut self, frame: i32) {
        if frame % 1800 == (self.offset + 17) % 1800 && self.jumps.total > 0 {
            let counts: Vec<String> = self.jumps.counts.iter().map(|(k, n)| format!("{k} {n}")).collect();
            self.log(&format!(
                "jumps: {} in the last minute ({} flew, {} replaced, {} never queued, {} stalled): {}",
                self.jumps.total,
                self.jumps.flew,
                self.jumps.replaced,
                self.jumps.refused,
                self.jumps.stalled,
                counts.join(", ")
            ));
            self.jumps.counts.clear();
            self.jumps.total = 0;
            self.jumps.flew = 0;
            self.jumps.replaced = 0;
            self.jumps.refused = 0;
            self.jumps.stalled = 0;
        }
        self.jumps_watch(frame);
        if frame % PERIOD != (self.offset + 5) % PERIOD || self.dev_off("jumps") {
            return;
        }
        self.jumps_sumo_gravity();
        let ids: Vec<(i32, Unit)> = self.fighters.iter().map(|(&id, f)| (id, f.unit)).collect();
        for (id, unit) in ids {
            if self.retreat_handler.is_retreating(id) {
                continue;
            }
            match self.defs.name_of(unit) {
                "jumpassault" => {
                    self.jumps_jack(unit);
                }
                "jumpraid" => {
                    self.jumps_pyro(unit);
                }
                "jumpsumo" => {
                    self.jumps_sumo(unit);
                }
                _ => {}
            }
        }
    }

    /// Enemies (ground, armed or worth killing) seen lately within `r` of
    /// `p`: (id, position, metal, health).
    fn jumps_enemies_near(&self, p: Vec3, r: f32) -> Vec<(i32, Vec3, f32, f32)> {
        let frame = self.frame;
        self.war
            .targets
            .values()
            .filter(|e| !e.is_dead && e.identified && !e.can_fly() && !e.is_static)
            .filter(|e| frame - e.last_seen <= FRESH && distance(e.position, p) <= r)
            .map(|e| (e.unit_id, e.position, e.ud.map_or(e.value, |d| d.cost(self.metal)), e.get_health()))
            .collect()
    }

    /// Metal of our own units within `r` of `p` (jumpers excepted).
    fn jumps_friends_near(&self, p: Vec3, r: f32) -> f32 {
        (self.cb.friendly_units_in(p, r).iter())
            .filter(|u| u.team() == self.team_id && u.def().is_some_and(|d| d.speed() > 0.0))
            .filter(|u| self.defs.name_of(**u) != "jumpsumo")
            .filter_map(|u| u.def().map(|d| d.cost(self.metal)))
            .sum()
    }

    /// A Jack jumps onto the best target in reach that its side can take:
    /// commanders, defenses (it breaks defense lines: 6000 health) and what
    /// outranges it first.
    fn jumps_jack(&mut self, unit: Unit) {
        let Some(jd) = self.jump_ready(unit) else { return };
        let pos = unit.pos();
        let mine = self.war.get_total_friendly_power(pos) + (unit.power() + unit.health()) / 10.0;
        let mut best: Option<(f32, Unit, Vec3)> = None;
        for e in self.war.targets.values() {
            let fresh = self.frame - e.last_seen <= FRESH || (e.is_static && e.is_porc);
            if e.is_dead || !e.identified || e.can_fly() || (e.is_static && !e.is_porc) || !fresh {
                continue;
            }
            let d = distance(pos, e.position);
            if !(MIN_ENGAGE..=jd.range * REACH + 60.0).contains(&d) {
                continue;
            }
            // Not into more than we have about (defenses included).
            if self.war.get_enemy_power(e.position) > 1.5 * mine {
                continue;
            }
            let cost = e.ud.map_or(e.value, |d| d.cost(self.metal));
            let worth =
                cost * if e.is_com {
                    3.0
                } else if e.is_porc {
                    1.3
                } else if e.is_arty || e.def_max_weapon_range() > 300.0 {
                    1.5
                } else if e.is_worker {
                    1.2
                } else {
                    1.0
                } / (1.0 + d / 300.0);
            if best.is_none_or(|b| worth > b.0) {
                best = Some((worth, e.unit, e.position));
            }
        }
        if let Some((worth, target, at)) = best
            && worth >= 100.0
        {
            self.jump_strike(unit, target, at, "jack");
        }
    }

    /// A Pyro jumps into a clump of light enemies (its flames go through
    /// them all), away from defenses.
    fn jumps_pyro(&mut self, unit: Unit) {
        let Some(jd) = self.jump_ready(unit) else { return };
        let pos = unit.pos();
        let light: Vec<(i32, Vec3, f32, f32)> = self
            .jumps_enemies_near(pos, jd.range * REACH + PYRO_SPLASH)
            .into_iter()
            .filter(|e| e.3 <= LIGHT_HEALTH)
            .collect();
        if light.len() < PYRO_CLUMP {
            return;
        }
        let mine = self.war.get_total_friendly_power(pos) + (unit.power() + unit.health()) / 10.0;
        let mut best: Option<(usize, f32, Vec3)> = None;
        for &(_, p, _, _) in &light {
            let d = distance(pos, p);
            if !(MIN_ENGAGE..=jd.range * REACH).contains(&d) {
                continue;
            }
            let near: Vec<&(i32, Vec3, f32, f32)> = light.iter().filter(|e| distance(e.1, p) <= PYRO_SPLASH).collect();
            if near.len() < PYRO_CLUMP {
                continue;
            }
            let n = near.len() as f32;
            let c = Vec3::new(
                near.iter().map(|e| e.1.x).sum::<f32>() / n,
                0.0,
                near.iter().map(|e| e.1.z).sum::<f32>() / n,
            );
            let metal: f32 = near.iter().map(|e| e.2).sum();
            if distance(pos, c) > jd.range * REACH
                || self.war.get_porc_threat(c) > 0.0
                || self.war.get_enemy_power(c) > PYRO_EDGE * mine
            {
                continue;
            }
            if best.is_none_or(|b| (near.len(), metal) > (b.0, b.1)) {
                best = Some((near.len(), metal, c));
            }
        }
        if let Some((_, _, to)) = best
            && self.jump_clear(jd, pos, to)
        {
            self.jump_order(unit, to, "pyro");
            unit.fight(to, OPTION_SHIFT_KEY);
        }
    }

    /// A Jugglenaut jumps into the thick of a crowd (its landing hits all
    /// within 340).
    fn jumps_sumo(&mut self, unit: Unit) {
        let Some(jd) = self.jump_ready(unit) else { return };
        let pos = unit.pos();
        let near = self.jumps_enemies_near(pos, jd.range * REACH + SUMO_SPLASH);
        let mut best: Option<(f32, Vec3)> = None;
        for &(_, p, _, _) in &near {
            let d = distance(pos, p);
            if !(MIN_ENGAGE..=jd.range * REACH).contains(&d) || self.war.get_porc_threat(p) > 0.0 {
                continue;
            }
            let metal: f32 = near.iter().filter(|e| distance(e.1, p) <= SUMO_SPLASH).map(|e| e.2).sum();
            if metal >= SUMO_CROWD
                && best.is_none_or(|b| metal > b.0)
                && self.jumps_friends_near(p, 170.0) <= SUMO_FRIENDS
            {
                best = Some((metal, p));
            }
        }
        if let Some((_, to)) = best
            && self.jump_clear(jd, pos, to)
        {
            self.jump_order(unit, to, "jugglenaut");
            unit.fight(to, OPTION_SHIFT_KEY);
        }
    }

    /// Jugglenauts pull, and push while they retreat.
    fn jumps_sumo_gravity(&mut self) {
        let sumos: Vec<(i32, Unit)> = self
            .fighters
            .iter()
            .filter(|(_, f)| self.defs.name_of(f.unit) == "jumpsumo")
            .map(|(&id, f)| (id, f.unit))
            .collect();
        for (id, unit) in sumos {
            let push = self.retreat_handler.is_retreating(id);
            if self.jumps.sumos.get(&id) != Some(&push) {
                unit.execute_custom_command(cmd::PUSH_PULL, &[if push { 1.0 } else { 0.0 }], 0, NO_TIMEOUT);
                self.jumps.sumos.insert(id, push);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flights_take_longer_the_farther_and_higher() {
        let jd = JumpDef { range: 400.0, height: 200.0, speed: 6.0, reload: 300, max_depth: 22.0 };
        let near = Ai::jump_flight(jd, 100.0);
        let far = Ai::jump_flight(jd, 400.0);
        assert!(near < far);
        // A full Pyro-like jump takes a couple of seconds.
        assert!((60.0..120.0).contains(&far), "{far}");
    }
}
