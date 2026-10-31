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
//! Skuttles onto their heavy (see `jaeger`). A Skuttle shown against its
//! will (an enemy near, or hit) jumps at once: onto the best blast in
//! reach, or else away from what shows it. Skuttles never walk up to
//! their mark: the blast (90) is short of the decloak (180), so one
//! walking in is seen and shot for nothing. Jugglenauts
//! pull with their gravity guns and push while they retreat, throwing
//! chasers off.

use std::collections::BTreeMap;

use springai::{NO_TIMEOUT, Unit, UnitDef, Vec3};

use super::water::is_fake;
use crate::Ai;
use crate::defs::cmd;
use crate::kgbutil::pathfinder::JumpPlan;
use crate::kgbutil::{PathCost, kgb_move_to};
use crate::util::{NULLPOS, OPTION_SHIFT_KEY, distance};

pub(crate) mod lobster;

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

/// Frames from a jump order to take-off: the order reaches the engine a
/// frame later, and the gadget's jump starts the frame after.
const TAKEOFF_FRAMES: f32 = 2.0;

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
/// Brawlers about the landing (shorter-ranged than the Pyro, together more
/// damage per second than its own: two Glaives do 210 to its 69) only
/// where we are four times the stronger: on foot the Pyro outranges and
/// kites them, and a jump into their midst gives that away.
const PYRO_EDGE_BRAWL: f32 = 0.25;

/// A jump order still first in the queue after this many frames failed
/// (the gadget leaves a jump it cannot make queued, and the unit stands).
const STALL: i32 = 45;
/// Light: up to this much health.
const LIGHT_HEALTH: f32 = 1300.0;

/// A jump on a unit's way (see the pathfinder's jump links) planned this
/// long ago is stale: the unit was sent elsewhere since, frames.
const WAY_STALE: i32 = 30 * 20;
/// A unit this near its take-off, ready, whose jump does not go this many
/// checks running: the jump is dropped from all ways.
const TAKEOFF: f32 = 40.0;
const TAKEOFF_TRIES: u32 = 3;
/// A squad member jumps its leader's jump in parallel from as far as this
/// off the leader's take-off.
const ALONGSIDE: f32 = 160.0;

/// Engine move and attack-move orders (ZK's raw move is `cmd::RAW_MOVE`).
const CMD_MOVE: i32 = 10;
const CMD_FIGHT: i32 = 16;
const CMD_ATTACK: i32 = 20;

/// A Skuttle's death blast (`jumpbomb_DEATH`): 8002 within 90 (half the
/// def's areaOfEffect) of what it hits, 40% of it at the rim
/// (edgeEffectiveness).
const BLAST: f32 = 8002.4;
const BLAST_RADIUS: f32 = 90.0;
const BLAST_EDGE: f32 = 0.4;

/// Anything this close shows a Skuttle (`minCloakDistance` 180, from mid
/// positions in 3D, and some for our stale view of the enemy).
const SHOWN_NEAR: f32 = 220.0;

/// Energy in store below which a Skuttle's cloak (15 a second moving)
/// may have dropped for want of upkeep rather than for an enemy.
const UPKEEP_DRY: f32 = 150.0;

/// A shown Skuttle (550 metal) jumps onto a blast that kills this much
/// more enemy metal than ours; this much once it is half dead.
const SKUTTLE_WORTH: f32 = 300.0;
const SKUTTLE_WORTH_HURT: f32 = 100.0;

/// A Skuttle that cannot jump onto its mark yet waits this far from it:
/// in jump reach (368), beyond the mark's decloak (180) and some.
const SKUTTLE_STANDOFF: f32 = 280.0;

/// A Skuttle already shown this close to its mark goes on at it on foot
/// when it cannot jump (a jump onto it lands 60-75 short).
pub const SKUTTLE_PRESS: f32 = 120.0;

/// A shown Skuttle jumping away lands as far as this from the enemies it
/// knows of, at best (farther is no better).
const SKUTTLE_CLEAR: f32 = 600.0;

/// A Skuttle that jumped on its own stays out of its squad this long: the
/// flight (about 2.5 s), the cloak's 1.5 s linger after the last enemy,
/// and some.
const SKUTTLE_HOLD: i32 = 30 * 6;

/// What a def's jumps are like (from its custom params).
#[derive(Clone, Copy, Debug)]
pub struct JumpDef {
    pub range: f32,
    pub height: f32,
    pub speed: f32,
    pub reload: i32,
    /// Frames the gadget waits before the unit takes off.
    pub delay: i32,
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
    lobsters: lobster::Lobsters,
    defs: BTreeMap<i32, Option<JumpDef>>,
    /// Each def's guns: damage per second and longest reach.
    guns: BTreeMap<i32, (f32, f32)>,
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
    /// Units at the take-off of a jump on their way that does not go: the
    /// take-off, and checks so far.
    takeoff: BTreeMap<i32, (Vec3, u32)>,
    /// Jumps on ways dropped since the summary, and how far each one
    /// taken rose (or fell, below 0).
    pub dropped: u32,
    way_rises: Vec<f32>,
    /// Our Skuttles: whether each was cloaked at the last check, its health
    /// then, and whether it has ever been cloaked (one fresh from the
    /// factory is not yet).
    skuttles: BTreeMap<i32, (bool, f32, bool)>,
    /// Skuttles seen losing their cloak since the summary.
    pub shown: u32,
}

fn param(ud: UnitDef, key: &str) -> Option<f32> {
    ud.custom_param(key).and_then(|v| v.trim().parse::<f32>().ok())
}

impl JumpDef {
    /// The jumps of def `ud`, if it jumps (and the gadget drives them).
    pub fn of(ud: UnitDef) -> Option<JumpDef> {
        (ud.custom_param("canjump").is_some_and(|v| v.trim() == "1")
            && param(ud, "no_jump_handling").is_none_or(|v| v != 1.0))
        .then(|| JumpDef {
            range: param(ud, "jump_range").unwrap_or(400.0),
            height: param(ud, "jump_height").unwrap_or(200.0),
            speed: param(ud, "jump_speed").unwrap_or(6.0).max(1.0),
            reload: (param(ud, "jump_reload").unwrap_or(10.0) * 30.0) as i32,
            delay: param(ud, "jump_delay").unwrap_or(0.0) as i32,
            max_depth: ud.max_water_depth(),
        })
    }

    /// How far a jump is ordered.
    pub fn reach(&self) -> f32 {
        self.range * REACH
    }
}

/// Whether a jump `height` high from `from` to `to` (each at the height it
/// leaves or lands at) clears the ground (`ground(x, z)`), as the gadget
/// samples its arc, and lands where a unit can stand.
pub fn arc_clear(ground: impl Fn(f32, f32) -> f32, height: f32, from: Vec3, to: Vec3) -> bool {
    // Landing slope.
    let d = 12.0;
    let dx = (ground(to.x + d, to.z) - ground(to.x - d, to.z)) / (2.0 * d);
    let dz = (ground(to.x, to.z + d) - ground(to.x, to.z - d)) / (2.0 * d);
    if (dx * dx + dz * dz).sqrt() > MAX_LANDING_SLOPE {
        return false;
    }
    let (vx, vy, vz) = (to.x - from.x, to.y - from.y, to.z - from.z);
    let steps = 40;
    (1..steps).all(|k| {
        let i = k as f32 / steps as f32;
        let arc = from.y + vy * i + (1.0 - (2.0 * i - 1.0).powi(2)) * height;
        ground(from.x + vx * i, from.z + vz * i) - ARC_CLEARANCE <= arc
    })
}

/// Ground position of `p` on the live map.
fn ground(ai: &Ai, p: Vec3) -> Vec3 {
    Vec3::new(p.x, ai.cb.map().elevation_at(p.x, p.z), p.z)
}

/// Damage of a Skuttle's blast to a unit of radius `r` whose middle is
/// `d` from it (the engine measures to the unit's surface).
fn blast_damage(d: f32, r: f32) -> f32 {
    let d = (d - r).max(0.0);
    if d >= BLAST_RADIUS {
        return 0.0;
    }
    BLAST * (BLAST_RADIUS - d) / (BLAST_RADIUS - d * BLAST_EDGE)
}

impl Ai {
    /// Orders must survive the delay before the engine starts a jump,
    /// and the flight itself.
    pub fn jump_in_progress(&self, unit: Unit) -> bool {
        self.jumps.pending.iter().any(|j| j.unit.id() == unit.id()) || unit.rules_param_float("is_jumping", 0.0) == 1.0
    }

    /// Shortcut a safe stretch of the walking route and queue the rest.
    /// Callers may append their work order after this returns true.
    pub fn jump_travel(&mut self, unit: Unit, goal: Vec3, why: &str) -> bool {
        if goal == NULLPOS || self.jump_in_progress(unit) {
            return false;
        }
        let Some(jd) = self.jump_ready(unit) else { return false };
        let pos = unit.pos();
        let def = unit.def();
        let Some((to, way)) = self.jump_path(unit, goal, jd.range * REACH, |ai, p| {
            let view = ai.war.view(def);
            ai.jump_clear(jd, pos, p)
                && view.get_porc_threat(p) <= 0.0
                && view.get_effective_threat(p) <= 0.0
                && distance(pos, p) >= MIN_ESCAPE
                && Self::jump_flight(jd, distance(pos, p))
                    < distance(pos, p) / def.map_or(1.0, |d| d.speed()).max(1.0) * 30.0
        }) else {
            return false;
        };
        self.jump_order(unit, to, why);
        for wp in way {
            if distance(to, wp) > 16.0 {
                kgb_move_to(unit, wp, OPTION_SHIFT_KEY, self.frame);
            }
        }
        true
    }

    /// The jumps of def `ud`, if it jumps (and the gadget drives them).
    pub fn jump_def(&mut self, ud: UnitDef) -> Option<JumpDef> {
        if let Some(d) = self.jumps.defs.get(&ud.id()) {
            return *d;
        }
        let d = JumpDef::of(ud);
        self.jumps.defs.insert(ud.id(), d);
        d
    }

    /// What `ud`'s guns do to units on the ground, close up: damage per
    /// second (bare, without the threat map's bonuses for fire, area or
    /// slow) and their longest reach.
    fn jumps_gun(&mut self, ud: UnitDef) -> (f32, f32) {
        if let Some(&g) = self.jumps.guns.get(&ud.id()) {
            return g;
        }
        let dps = (ud.weapon_mounts().iter())
            .filter_map(|m| m.weapon_def())
            .filter(|wd| !is_fake(&wd.name().to_lowercase()))
            .filter(|wd| wd.weapon_type() != "TorpedoLauncher" || wd.is_sub_missile())
            .map(|wd| {
                let shot = wd.damage_types().get(1).copied().unwrap_or(0.0)
                    * wd.projectiles_per_shot() as f32
                    * wd.salvo_size() as f32;
                if wd.reload() > 0.0 { shot / wd.reload() } else { shot }
            })
            .sum();
        let g = (dps, ud.max_weapon_range());
        self.jumps.guns.insert(ud.id(), g);
        g
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
        let (from, to) = (Vec3::new(from.x, start, from.z), Vec3::new(to.x, end, to.z));
        arc_clear(|x, z| map.elevation_at(x, z), jd.height, from, to)
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

    /// Where a jump of `jd` from `from` meets a target at `at` going `v`
    /// (elmos a frame): where the target is when the jump lands there.
    pub fn jump_lead_point(jd: JumpDef, from: Vec3, at: Vec3, v: Vec3) -> Vec3 {
        let mut to = at;
        for _ in 0..4 {
            let t = Self::jump_time(jd, distance(from, to));
            to = Vec3::new(at.x + v.x * t, at.y, at.z + v.z * t);
        }
        to
    }

    /// Frames from a jump order of `jd` over `dist` to the landing.
    pub fn jump_time(jd: JumpDef, dist: f32) -> f32 {
        TAKEOFF_FRAMES + jd.delay as f32 + Self::jump_flight(jd, dist)
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
        let Some(to) = self.jump_strike_landing(unit, target, known) else { return false };
        self.jump_order(unit, to, why);
        unit.attack(target, OPTION_SHIFT_KEY);
        true
    }

    /// Where `unit` would land jumping onto `target` (see `jump_strike`),
    /// if it can now and that is in reach.
    fn jump_strike_landing(&mut self, unit: Unit, target: Unit, known: Vec3) -> Option<Vec3> {
        let jd = self.jump_ready(unit)?;
        let seen = target.pos();
        let at = if seen == NULLPOS || (seen.x == 0.0 && seen.z == 0.0) { known } else { seen };
        let from = unit.pos();
        if at == NULLPOS {
            return None;
        }
        let lead = Self::jump_flight(jd, distance(from, at)) * 0.5;
        let v = target.vel();
        let to = Vec3::new(at.x + (v.x * lead).clamp(-150.0, 150.0), at.y, at.z + (v.z * lead).clamp(-150.0, 150.0));
        let d = distance(from, to);
        ((MIN_ENGAGE * 0.5..=jd.range * REACH).contains(&d) && self.jump_clear(jd, from, to)).then_some(to)
    }

    /// Where a Skuttle of jumps `jd` lands jumping onto `target` (last seen
    /// at `known`), if in reach and clear: where it will be (`lead`), or by
    /// the old half-flight lead.
    fn skuttle_landing(&mut self, unit: Unit, jd: JumpDef, target: Unit, known: Vec3) -> Option<Vec3> {
        if self.dev_off("lead") {
            return self.jump_strike_landing(unit, target, known);
        }
        let seen = target.pos();
        let unseen = seen == NULLPOS || (seen.x == 0.0 && seen.z == 0.0);
        let (at, v) = if unseen { (known, Vec3::default()) } else { (seen, target.vel()) };
        let from = unit.pos();
        let to = Self::jump_lead_point(jd, from, at, v);
        let d = distance(from, to);
        ((MIN_ENGAGE * 0.5..=jd.range * REACH).contains(&d) && self.jump_clear(jd, from, to)).then_some(to)
    }

    /// Whether a Skuttle that cannot jump goes on at a mark `d` off on foot:
    /// one on top of it, or one already shown that close (it landed short):
    /// walking back is no safer.
    pub fn skuttle_presses_on(unit: Unit, d: f32) -> bool {
        d < MIN_ENGAGE * 0.5 || (!unit.is_cloaked() && d <= SKUTTLE_PRESS)
    }

    /// A Skuttle that cannot jump onto `target` now: one that presses on
    /// (see [`Ai::skuttle_presses_on`]) attacks it, and goes off within 50
    /// (true); the rest wait in jump reach and out of the mark's sight
    /// (walking up, one would be seen and shot short of its blast).
    pub fn skuttle_wait(&mut self, unit: Unit, target: Unit) -> bool {
        let (pos, at) = (unit.pos(), target.pos());
        if at == NULLPOS || (at.x == 0.0 && at.z == 0.0) {
            return false;
        }
        let d = distance(pos, at);
        if Self::skuttle_presses_on(unit, d) {
            if unit.current_commands().first().map(|c| c.id()) != Some(CMD_ATTACK) {
                unit.attack(target, 0);
            }
            return true;
        }
        let k = SKUTTLE_STANDOFF / d.max(1.0);
        let stand = Vec3::new(at.x + (pos.x - at.x) * k, 0.0, at.z + (pos.z - at.z) * k);
        if distance(pos, stand) > 40.0 {
            kgb_move_to(unit, stand, 0, self.frame);
        }
        false
    }

    /// [`Ai::jump_strike`], landing where `target` will be: it keeps its
    /// velocity until then (heavies are slow to speed up or stop, though
    /// they may turn). Lands within reach or not at all.
    pub fn jump_lead(&mut self, unit: Unit, target: Unit, known: Vec3, why: &str) -> bool {
        let Some(jd) = self.jump_ready(unit) else { return false };
        let seen = target.pos();
        let unseen = seen == NULLPOS || (seen.x == 0.0 && seen.z == 0.0);
        let (at, v) = if unseen { (known, Vec3::default()) } else { (seen, target.vel()) };
        let from = unit.pos();
        if at == NULLPOS {
            return false;
        }
        let to = Self::jump_lead_point(jd, from, at, v);
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
        let Some((to, way)) = self.jump_path(unit, home, jd.range * REACH, |ai, p| ai.jump_clear(jd, pos, p)) else {
            return;
        };
        let why = format!("escape {}", self.defs.name(def));
        self.jump_order(unit, to, &why);
        for wp in way {
            if distance(to, wp) > 16.0 {
                kgb_move_to(unit, wp, OPTION_SHIFT_KEY, self.frame);
            }
        }
    }

    /// Farthest safe landing along the walking route. Lobsters use the
    /// same shortcut selection, checking every passenger's landing.
    fn jump_path(
        &mut self,
        unit: Unit,
        home: Vec3,
        reach: f32,
        clear: impl Fn(&Self, Vec3) -> bool,
    ) -> Option<(Vec3, Vec<Vec3>)> {
        let pos = unit.pos();
        let way: Vec<Vec3> =
            self.pathfinder.find_path(&self.war, unit, home, PathCost::AvoidEnemies).into_iter().collect();
        if way.is_empty() {
            return None;
        }
        // The farthest point along it within reach that a jump gets to.
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
                if along + seg * t >= MIN_ESCAPE && clear(self, p) {
                    best = Some((p, i));
                }
            }
            along += seg;
            prev = wp;
        }
        best.map(|(to, i)| (to, way[i..].to_vec()))
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
        self.lobster_remove_unit(unit.id());
        self.jumps.last.remove(&unit.id());
        self.jumps.sumos.remove(&unit.id());
        self.jumps.takeoff.remove(&unit.id());
        self.jumps.skuttles.remove(&unit.id());
        self.pathfinder.jump_plans.remove(&unit.id());
    }

    /// Jumps on our units' ways (see the pathfinder's jump links): each
    /// goes once its unit is in reach of the landing and can (a squad
    /// member's alongside its leader's). One that does not go from its
    /// take-off is dropped.
    fn jumps_follow_ways(&mut self, frame: i32) {
        let plans: Vec<JumpPlan> = self.pathfinder.jump_plans.values().copied().collect();
        for p in plans {
            let (unit, id) = (p.unit, p.unit.id());
            if unit.health() <= 0.0 || unit.team() != self.team_id || frame - p.frame > WAY_STALE {
                self.pathfinder.jump_plans.remove(&id);
                self.jumps.takeoff.remove(&id);
                continue;
            }
            // Only on its way: not from a build or an attack it was given since.
            let first = unit.current_commands().first().map(|c| c.id());
            if first.is_some_and(|c| ![CMD_MOVE, CMD_FIGHT, cmd::RAW_MOVE].contains(&c)) {
                continue;
            }
            let pos = unit.pos();
            let off = Vec3::new(pos.x - p.from.x, 0.0, pos.z - p.from.z);
            let alongside = Vec3::new(p.to.x + off.x, p.to.y, p.to.z + off.z);
            let beside = p.shared && distance(pos, p.from) > TAKEOFF && distance(pos, p.from) <= ALONGSIDE;
            let landing = if beside && self.jump_to(unit, alongside, "way") {
                Some(alongside)
            } else if self.jump_to(unit, p.to, "way") {
                Some(p.to)
            } else {
                None
            };
            if let Some(to) = landing {
                if distance(p.then, p.to) > 16.0 {
                    kgb_move_to(unit, p.then, OPTION_SHIFT_KEY, frame);
                }
                let map = self.cb.map();
                self.jumps.way_rises.push(map.elevation_at(to.x, to.z) - map.elevation_at(pos.x, pos.z));
                self.pathfinder.jump_taken(&p);
                self.jumps.takeoff.remove(&id);
                continue;
            }
            if distance(pos, p.from) > TAKEOFF || self.jump_ready(unit).is_none() {
                continue;
            }
            let tries = match self.jumps.takeoff.get(&id) {
                Some(&(at, n)) if at == p.from => n + 1,
                _ => 1,
            };
            self.jumps.takeoff.insert(id, (p.from, tries));
            if tries < TAKEOFF_TRIES {
                continue;
            }
            self.jumps.takeoff.remove(&id);
            if p.shared {
                // The leader's jump, not this unit's: its own way next.
                self.pathfinder.jump_plans.remove(&id);
                continue;
            }
            let (from, to) = self.pathfinder.drop_jump(&p);
            self.jumps.dropped += 1;
            self.log(&format!(
                "jumps: {} {} stands at its take-off ({:.0}, {:.0}) and cannot jump to ({:.0}, {:.0}): no way \
                 jumps there now",
                self.defs.name_of(unit),
                id,
                from.x,
                from.z,
                to.x,
                to.z
            ));
        }
    }

    pub fn jumps_update(&mut self, frame: i32) {
        self.pathfinder.jumps_on = !self.dev_off("jumps") && !self.dev_off("jumppath");
        for (def, n, ms) in std::mem::take(&mut self.pathfinder.jump_link_builds) {
            self.log(&format!("jumps: {n} jump links for the ways of {def} ({ms:.0} ms)"));
        }
        if frame % 1800 == (self.offset + 17) % 1800 && (self.jumps.total > 0 || self.jumps.shown > 0) {
            let counts: Vec<String> = self.jumps.counts.iter().map(|(k, n)| format!("{k} {n}")).collect();
            let rises: Vec<String> = self.jumps.way_rises.iter().map(|r| format!("{r:+.0}")).collect();
            self.log(&format!(
                "jumps: {} in the last minute ({} flew, {} replaced, {} never queued, {} stalled, {} dropped from \
                 ways): {}{}{}",
                self.jumps.total,
                self.jumps.flew,
                self.jumps.replaced,
                self.jumps.refused,
                self.jumps.stalled,
                self.jumps.dropped,
                counts.join(", "),
                if rises.is_empty() { String::new() } else { format!("; ways rose {}", rises.join(" ")) },
                if self.jumps.shown == 0 { String::new() } else { format!("; Skuttles shown {}", self.jumps.shown) }
            ));
            self.jumps.way_rises.clear();
            self.jumps.counts.clear();
            self.jumps.total = 0;
            self.jumps.flew = 0;
            self.jumps.replaced = 0;
            self.jumps.refused = 0;
            self.jumps.stalled = 0;
            self.jumps.dropped = 0;
            self.jumps.shown = 0;
        }
        self.jumps_watch(frame);
        self.lobster_update();
        if frame % PERIOD != (self.offset + 5) % PERIOD || self.dev_off("jumps") {
            return;
        }
        self.jumps_follow_ways(frame);
        self.jumps_sumo_gravity();
        // Skuttles: jaegers, and the odd one left to the sappers;
        // retreating too.
        let skuttles: Vec<Unit> = (self.fighters.values().map(|f| f.unit))
            .chain(self.misc_handler.sapper_units())
            .filter(|&u| self.defs.name_of(u) == "jumpbomb")
            .collect();
        for unit in skuttles {
            self.jumps_skuttle(unit);
        }
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
            .map(|e| (e.unit_id, e.position, e.ud.map_or(e.value, |d| d.cost(self.metal)), e.current_health()))
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
    /// them all), away from defenses, and not into brawlers it would
    /// rather kite.
    fn jumps_pyro(&mut self, unit: Unit) {
        let Some(jd) = self.jump_ready(unit) else { return };
        let Some(def) = unit.def() else { return };
        let (own_dps, own_range) = self.jumps_gun(def);
        let pos = unit.pos();
        let mine = self.war.get_total_friendly_power(pos) + (unit.power() + unit.health()) / 10.0;
        if let Some(to) = self.jumps_pyro_target(pos, jd.range, mine, own_dps, own_range)
            && self.jump_clear(jd, pos, to)
        {
            self.jump_order(unit, to, "pyro");
            unit.fight(to, OPTION_SHIFT_KEY);
        }
    }

    /// Pyros and equipped squads share target selection and brawler safety.
    fn jumps_pyro_target(&mut self, pos: Vec3, range: f32, mine: f32, own_dps: f32, own_range: f32) -> Option<Vec3> {
        let light: Vec<(i32, Vec3, f32, f32)> = self
            .jumps_enemies_near(pos, range * REACH + PYRO_SPLASH)
            .into_iter()
            .filter(|e| e.3 <= LIGHT_HEALTH)
            .collect();
        if light.len() < PYRO_CLUMP {
            return None;
        }
        let mut best: Option<(usize, f32, Vec3)> = None;
        for &(_, p, _, _) in &light {
            let d = distance(pos, p);
            if !(MIN_ENGAGE..=range * REACH).contains(&d) {
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
            if distance(pos, c) > range * REACH || self.war.get_porc_threat(c) > 0.0 {
                continue;
            }
            // Those that the Pyro outranges, within its reach of the landing.
            let brawlers: Vec<UnitDef> = (self.war.targets.values())
                .filter(|e| !e.is_dead && e.identified && !e.can_fly() && self.frame - e.last_seen <= FRESH)
                .filter(|e| e.def_max_weapon_range() < own_range && distance(e.position, c) <= own_range)
                .filter_map(|e| e.ud)
                .collect();
            let brawl: f32 = brawlers.into_iter().map(|d| self.jumps_gun(d).0).sum();
            let edge = if brawl > own_dps { PYRO_EDGE_BRAWL } else { PYRO_EDGE };
            if self.war.get_enemy_power(c) > edge * mine {
                continue;
            }
            if best.is_none_or(|b| (near.len(), metal) > (b.0, b.1)) {
                best = Some((near.len(), metal, c));
            }
        }
        best.map(|(_, _, to)| to)
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
    /// A Skuttle shown against its will jumps at once if it can (jumps are
    /// silent and keep the cloak): onto the best blast in reach if that is
    /// worth it, else away from what shows it. Shown with nothing near and
    /// no hit, it walks on if the energy store is near empty (its upkeep
    /// ran dry) or it has not cloaked yet (fresh from the factory).
    fn jumps_skuttle(&mut self, unit: Unit) {
        let id = unit.id();
        if unit.is_being_built() || unit.health() <= 0.0 {
            return;
        }
        let (cloaked, health) = (unit.is_cloaked(), unit.health());
        let (was_cloaked, was_health, was_ever) =
            self.jumps.skuttles.get(&id).copied().unwrap_or((false, health, false));
        self.jumps.skuttles.insert(id, (cloaked, health, was_ever || cloaked));
        // (A jump ordered just now has not taken off yet.)
        let ordered = self.jumps.last.get(&id).is_some_and(|&f| self.frame - f < 15);
        if cloaked || ordered || unit.rules_param_float("is_jumping", 0.0) == 1.0 {
            return;
        }
        if was_cloaked {
            self.jumps.shown += 1;
        }
        let frame = self.frame;
        let hit = health < was_health - 1.0;
        let pos = unit.pos();
        // What showed it: an enemy close enough to (the Skuttle sees 330,
        // so whatever is within its decloak is in its own sight), a hit,
        // or else something it cannot see (a cloaked enemy) unless its
        // upkeep ran dry. (Guns that reach it from farther are not known:
        // it is myopic, and spotters are seldom about.)
        let near = (self.war.targets.values())
            .filter(|e| !e.is_dead && frame - e.last_seen <= FRESH && distance(e.position, pos) <= SHOWN_NEAR)
            .map(|e| (distance(e.position, pos), e))
            .min_by(|a, b| a.0.total_cmp(&b.0))
            .map(|(d, e)| (d, e.ud.map_or("radar dot", |u| self.defs.name(u))));
        let dry = self.eco.energy + self.eco.energy_reserve < UPKEEP_DRY;
        if near.is_none() && !hit && (dry || !was_ever) {
            return;
        }
        let shown_by = match near {
            Some((d, name)) => format!("{name} at {d:.0}{}", if hit { ", hit" } else { "" }),
            None if hit => "a hit".to_owned(),
            None => "nothing in sight".to_owned(),
        };
        let Some(jd) = self.jump_ready(unit) else {
            if was_cloaked {
                self.log(&format!(
                    "jumps: Skuttle {id} shown at ({:.0}, {:.0}) by {shown_by}, its jump not ready",
                    pos.x, pos.z
                ));
            }
            return;
        };
        let reach = jd.range * REACH;

        // Onto the best blast in reach (a moving target is led, so a bit
        // beyond reach may land in it).
        let mut best: Option<(f32, Unit, Vec3, String)> = None;
        for e in self.war.targets.values() {
            let fresh = frame - e.last_seen <= FRESH || e.is_static;
            if e.is_dead || !e.identified || e.can_fly() || !fresh || distance(pos, e.position) > reach + 40.0 {
                continue;
            }
            let worth = self.skuttle_blast_worth(e.position, id);
            if best.as_ref().is_none_or(|b| worth > b.0) {
                best = Some((worth, e.unit, e.position, e.ud.map_or(String::new(), |u| self.defs.name(u).to_owned())));
            }
        }
        let need = if health < 0.5 * unit.max_health() { SKUTTLE_WORTH_HURT } else { SKUTTLE_WORTH };
        if let Some((worth, target, at, name)) = best.clone().filter(|b| b.0 >= need) {
            let close = distance(pos, at) < MIN_ENGAGE * 0.5;
            let landing = if close { None } else { self.skuttle_landing(unit, jd, target, at) };
            if close || landing.is_some() {
                // Out of its squad first (a leader leaving is stopped), under
                // way onto it as a pounce.
                self.jaeger_pounced(id, target.id());
                match landing {
                    Some(to) => {
                        self.jump_order(unit, to, "skuttle shown: onto");
                        unit.attack(target, OPTION_SHIFT_KEY);
                    }
                    None => {
                        if unit.current_commands().first().map(|c| c.id()) != Some(CMD_ATTACK) {
                            unit.attack(target, 0);
                        }
                    }
                }
                self.log(&format!(
                    "jumps: Skuttle {id} shown at ({:.0}, {:.0}) by {shown_by} {} {name} at ({:.0}, {:.0}): {worth:.0} \
                     metal more theirs than ours in the blast",
                    pos.x,
                    pos.z,
                    if close { "goes off on" } else { "jumps onto" },
                    at.x,
                    at.z
                ));
                return;
            }
        }

        // Away: the landing farthest from the enemies we know of, out of
        // their threat, toward home.
        let enemies: Vec<Vec3> = (self.war.targets.values())
            .filter(|e| !e.is_dead && (frame - e.last_seen <= FRESH || e.is_static))
            .filter(|e| distance(e.position, pos) <= reach + SKUTTLE_CLEAR)
            .map(|e| e.position)
            .collect();
        let haven = self.graph_get_closest_haven(pos);
        let def = unit.def();
        let mut away: Option<(f32, Vec3, f32)> = None;
        for k in 0..16 {
            let a = k as f32 * std::f32::consts::TAU / 16.0;
            for frac in [1.0, 0.75, 0.5] {
                let to = ground(self, Vec3::new(pos.x + a.cos() * reach * frac, 0.0, pos.z + a.sin() * reach * frac));
                if !self.jump_clear(jd, pos, to) {
                    continue;
                }
                let clear = enemies.iter().map(|&p| distance(p, to)).fold(SKUTTLE_CLEAR, f32::min);
                let threat = if self.war.view(def).get_threat(to) > 0.0 { 300.0 } else { 0.0 };
                let home = if haven == NULLPOS { 0.0 } else { distance(pos, haven) - distance(to, haven) };
                let score = clear - threat + 0.25 * home;
                if away.is_none_or(|b| score > b.0) {
                    away = Some((score, to, clear));
                }
            }
        }
        match away {
            Some((_, to, clear)) => {
                self.jaeger_hold(id, SKUTTLE_HOLD);
                self.jump_order(unit, to, "skuttle shown: away");
                self.log(&format!(
                    "jumps: Skuttle {id} shown at ({:.0}, {:.0}) by {shown_by} jumps away to ({:.0}, {:.0}), {clear:.0} \
                     from the nearest enemy{}",
                    pos.x,
                    pos.z,
                    to.x,
                    to.z,
                    best.as_ref().map_or(String::new(), |b| format!(" (best blast {:.0})", b.0))
                ));
            }
            None if was_cloaked => {
                self.log(&format!(
                    "jumps: Skuttle {id} shown at ({:.0}, {:.0}) by {shown_by} has nowhere to jump",
                    pos.x, pos.z
                ));
            }
            None => {}
        }
    }

    /// Enemy metal a Skuttle's blast at `c` kills, less ours and our
    /// allies' (a Skuttle of ours there goes off too) but Skuttle `me`.
    fn skuttle_blast_worth(&self, c: Vec3, me: i32) -> f32 {
        let r = BLAST_RADIUS + 60.0;
        let frame = self.frame;
        let theirs: f32 = (self.war.targets.values())
            .filter(|e| !e.is_dead && e.identified && !e.can_fly() && (frame - e.last_seen <= FRESH || e.is_static))
            .filter(|e| distance(e.position, c) <= r)
            .filter_map(|e| {
                let ud = e.ud?;
                let dmg = blast_damage(distance(e.position, c), ud.radius());
                Some(ud.cost(self.metal) * (dmg / e.current_health().max(1.0)).min(1.0))
            })
            .sum();
        let ours: f32 = (self.cb.friendly_units_in(c, r).iter())
            .filter(|u| u.id() != me && u.health() > 0.0)
            .filter_map(|u| {
                let ud = u.def()?;
                let dmg = blast_damage(distance(u.pos(), c), ud.radius());
                Some(ud.cost(self.metal) * (dmg / u.health()).min(1.0))
            })
            .sum();
        theirs - ours
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flights_take_longer_the_farther_and_higher() {
        let jd = JumpDef { range: 400.0, height: 200.0, speed: 6.0, reload: 300, delay: 0, max_depth: 22.0 };
        let near = Ai::jump_flight(jd, 100.0);
        let far = Ai::jump_flight(jd, 400.0);
        assert!(near < far);
        // A full Pyro-like jump takes a couple of seconds.
        assert!((60.0..120.0).contains(&far), "{far}");
    }

    #[test]
    fn a_lead_jump_lands_where_its_target_will_be() {
        // A Skuttle's jumps, at a Cyclops walking 1.9 elmos a frame.
        let jd = JumpDef { range: 400.0, height: 120.0, speed: 6.0, reload: 300, delay: 0, max_depth: 15.0 };
        let (from, at) = (Vec3::xz(0.0, 0.0), Vec3::xz(300.0, 0.0));
        assert_eq!(Ai::jump_lead_point(jd, from, at, Vec3::default()), at);
        for v in [Vec3::xz(-1.9, 0.0), Vec3::xz(1.9, 0.0), Vec3::xz(0.0, 1.9)] {
            let to = Ai::jump_lead_point(jd, from, at, v);
            let t = Ai::jump_time(jd, distance(from, to));
            let there = Vec3::xz(at.x + v.x * t, at.z + v.z * t);
            assert!(distance(to, there) < 2.0, "{to:?} vs {there:?}");
        }
    }

    #[test]
    fn skuttle_blasts_fall_off_to_the_rim() {
        // On top of a Grizzly-sized unit: all of it; at its surface's rim:
        // nothing; halfway: most (edgeEffectiveness 0.4).
        assert_eq!(blast_damage(10.0, 30.0), BLAST);
        assert_eq!(blast_damage(30.0 + BLAST_RADIUS, 30.0), 0.0);
        let half = blast_damage(30.0 + BLAST_RADIUS / 2.0, 30.0);
        assert!((0.6 * BLAST..0.65 * BLAST).contains(&half), "{half}");
    }
}
