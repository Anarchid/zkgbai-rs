//! Packs: forces that fight on the move (the `pack` aim; the `ravager`
//! doctrine, off by default: `enable=ravager`).
//!
//! A pack fights as a player drives Ravagers. Its cores move in formation
//! with the leader (`Keep::Formation`: its moves shifted by each one's
//! place, at its pace), so the pack slides as one body and none circles;
//! its leader only ever has move orders, two or three waypoints deep, so
//! none of it brakes at the end of an order or stops to engage (a fight
//! order halts a unit at range), while its turrets shoot on the move. Once
//! `cores_to_go` cores are rallied it dives the best target the threat maps
//! say it survives (`war_best_target`, looked for from a point ahead of it,
//! so what lies ahead wins), following the unit nearest where it was. Near
//! it, the pack passes over it on the line that brings the most of its guns
//! within `pass_reach` of their reach (the formation slides unturned, so a
//! gun passes as far off as its place lies across the way: the plan reads
//! the pack's shape and thickness; Ravagers shoot over each other, so it
//! need not mind who is in front), runs on, and swings round for another,
//! until nothing is left there (or for 45 s); then the next, staying out
//! while it has `cores_to_stay`. A turn back of more than a right angle
//! starts with a waypoint sized from its cores' speed and turn rate when
//! attacking; retreats may reverse directly. Explicit `attack=pass` keeps
//! the formation tangential, outside the target's body, even under `fates`.
//! With `attack` charge (or mixed, for what moves) it
//! runs at the target head-on instead, aimed anew each conduct until
//! [`CHARGE_LOCK`] off, then straight through: ZK units push mobiles out of
//! their way, and what dodges a tangent cannot sidestep a ram. With
//! `commit`, a run begun is finished through its target before threat or
//! raiders are judged again. Health retreats always interrupt immediately,
//! regardless of run progress or heading. Enemy porc is weighed against the
//! pack: it takes on targets, run lines and ground under porc threat up to `porc_share` of its
//! own (`shun_porc` off: porc is not looked at). After `quiet` seconds
//! without firing (five by default), members below `repair_below` health
//! leave along their own clear routes home, regardless of incoming hits.
//! A weak pack near a haven may also rest as one when the estimated value
//! of healing per second exceeds that of its next run; a better raid can
//! end this discretionary rest before it is fully healed. With `judge`
//! predictor, whether a run is worth it is the fight predictor's call
//! instead of the threat maps': a run is a sequence of fights, one per
//! group of enemies along its way, each asked of the predictor with the
//! pack as the fights before should leave it; it goes if the expected kills
//! and the economy at the target beat the expected losses by
//! `judge_margin`, and gives up when the rest of a run stops being worth it.
//!
//! It falls back as one: from threat that outweighs it where it is, home
//! (10 s before it judges again); from raiders worth `rip_value` near it,
//! all the way to its screen (roles that `Keep::Trail`, Rippers, following
//! it home-side at their own pace, so that what chases the pack runs into
//! them; the screen holds meanwhile); below `retreat_health` of its original
//! core strength (losses and damage both count), to a haven, where it parks
//! for repairs until its surviving cores average `heal_to`. Recovery starts
//! a fresh strength baseline; each pass ends 150 beyond its fixed pass point.
//! A screen member below `retreat_health` goes for repairs alone. Packs neither out
//! nor parked merge, the smaller into the larger, are led from where most
//! of their cores stand, and gather at the haven nearest the rally point,
//! not at it.
//!
//! Log lines `taskforce: the ravager pack (squad N) ...`: dives (target),
//! passes or charges (the how manyth, guns within reach of how many), falls
//! back to its screen, parks / is repaired, takes in (merges), is led from
//! its bulk; engages and break-offs as other forces'.

use std::collections::BTreeMap;
use std::sync::Arc;

use springai::{Unit, UnitDef, Vec3};

use super::{Attack, Judge, Shape};
use crate::Ai;
use crate::kgbutil::PathCost;
use crate::military::SquadId;
use crate::military::fightsim::{FateMode, FightSnapshot, Model, SnapUnit, UnitSpec};
use crate::military::wrappers::squad::health_fraction;
use crate::nav;
use crate::util::{NULLPOS, distance, get_directional_point};
use serde_json::{Value, json};

/// Within this of its target a pack plans a pass; farther, it paths there.
const PLAN_AT: f32 = 900.0;
/// A pass's waypoints lie this far ahead of the leader along it, at least...
const PASS_LEAD: f32 = 150.0;
/// ...and the second this far beyond the first.
const PASS_ON: f32 = 400.0;
/// A pass is over this far beyond its pass point; the next is planned from
/// there.
const PASS_OUT: f32 = 150.0;
/// Pass lines are tried this far apart across the way.
const PASS_STEP: f32 = 10.0;
/// A pass line must be clear of enemy porc from this far before its pass
/// point to [`PASS_ON`] beyond it.
const PASS_IN: f32 = 250.0;
/// A target no pass line can clear of porc is shunned this many frames
/// (60 s).
const SHUN_FOR: i32 = 1800;
/// A charge is aimed anew each conduct until this close to its target,
/// then runs straight through.
const CHARGE_LOCK: f32 = 200.0;
/// The predictor judges a run on enemies seen within this many frames...
const JUDGE_SEEN: i32 = 900;
/// ...within their reach of the run's way, mobiles this much more (they
/// close in)...
const JUDGE_CHASE: f32 = 300.0;
/// ...as engagements apart where a gap this long along the way parts them;
/// the pack meets each this far short of it.
const JUDGE_GAP: f32 = 600.0;
const JUDGE_MEET: f32 = 500.0;
/// A run the predictor judges not worth it shuns its target this many
/// frames (30 s).
const JUDGE_SHUN: i32 = 900;
/// Judged by fates, a new target is the best of this many candidates, each
/// run ramming it or passing it at `pass_reach` of the leader's reach.
const FATE_CANDIDATES: usize = 3;
/// A pack is led from where most of its cores stand within this of one
/// another...
const BULK: f32 = 400.0;
/// ...once that core has this many more of them near it than the leader.
const BULK_MORE: usize = 2;
/// Waypoints along a path lie at least this far apart.
const SPACING: f32 = 250.0;
/// The next target is looked for from this far ahead of the leader.
const LOOK_AHEAD: f32 = 250.0;
/// Enemy value within this radius contributes to a sweep candidate.
const SWEEP_CLUSTER: f32 = 250.0;
/// Prevent a nearly coincident candidate from winning on distance alone.
const SWEEP_PAD: f32 = 300.0;
/// A target is kept while an enemy remains within this of it...
const TARGET_LEFT: f32 = 300.0;
/// ...for up to this many frames (45 s).
const TARGET_FOR: i32 = 1350;
/// Raiders within this of the leader count toward `rip_value`...
const RIP_REACH: f32 = 600.0;
/// ...if seen within this many frames.
const RIP_SEEN: i32 = 150;
/// Closer than this to its screen, a pack fights raiders where it is.
const SCREEN_NEAR: f32 = 400.0;
/// Within this of where it gathers or parks, a pack stands.
const STAND_NEAR: f32 = 300.0;
/// Fallback lateral waypoint for cores whose turning data is unavailable.
const TURN: f32 = 250.0;
/// Keep enough distance for raw-move waypoints to be passed without braking.
const TURN_MIN: f32 = 150.0;
/// A screen member's place must move this far before it is sent anew.
const SCREEN_REPATH: f32 = 250.0;
/// Outweighed, a pack keeps going home this many frames (10 s) before it
/// judges again, so it does not turn on the threat's edge.
const BACK_FOR: i32 = 300;
/// A weak pack may take a short detour to a nearby repair haven.
const REST_NEAR: f32 = 450.0;
/// Require a useful advantage before changing between resting and raiding.
const REST_MARGIN: f32 = 1.25;

/// A pass over a target: the way it goes (a unit vector on the map's
/// axes), and how far to the left of the target the leader's line runs
/// (negative: to the right).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Pass {
    pub way: (f32, f32),
    pub side: f32,
}

impl Pass {
    fn left(&self) -> (f32, f32) {
        (-self.way.1, self.way.0)
    }

    /// Where the leader goes by `target`.
    fn point(&self, target: Vec3) -> Vec3 {
        let l = self.left();
        Vec3::xz(target.x + self.side * l.0, target.z + self.side * l.1)
    }

    /// How far `pos` is along the pass beyond its pass point (negative
    /// before it).
    fn along(&self, pos: Vec3, target: Vec3) -> f32 {
        let q = self.point(target);
        (pos.x - q.x) * self.way.0 + (pos.z - q.z) * self.way.1
    }
}

/// A pack's state between conducts.
#[derive(Clone, Debug)]
pub struct Pack {
    /// What it dives (`NULLPOS`: nothing), since which frame, the pass it
    /// is on, and how many passes it made over it.
    pub target: Vec3,
    pub since: i32,
    pub pass: Option<Pass>,
    /// The pass point's target, fixed once a charge locks: a moving enemy
    /// must not drag the finish line along with it.
    pass_target: Vec3,
    pub passes: u32,
    /// Full-health core equivalents at the start of an outing, increased
    /// by reinforcements but not reduced by casualties or repair departures.
    original_cores: usize,
    /// Whether what it dives moves (`mixed` charges it).
    pub mobile: bool,
    /// Judged by fates: ram the target (true) or pass it (false), as the
    /// plan found worth more.
    pub ram: Option<bool>,
    /// Targets it shuns, until when: no pass over them clear of porc.
    pub shun: Vec<(Vec3, i32)>,
    /// At a haven for repairs.
    pub parked: bool,
    /// Falling back from threat until this frame.
    pub back_until: i32,
    /// Falling back to its screen (from raiders) until it gets there.
    pub to_screen: bool,
    /// A discretionary rest may end early when a better raid appears.
    pub opportunistic_rest: bool,
    /// Shots, including misses; incoming damage does not postpone repairs.
    pub last_fired: Option<i32>,
    reload_frames: BTreeMap<(i32, usize), i32>,
}

impl Default for Pack {
    fn default() -> Self {
        Pack {
            target: NULLPOS,
            since: 0,
            pass: None,
            pass_target: NULLPOS,
            passes: 0,
            original_cores: 0,
            mobile: false,
            ram: None,
            shun: Vec::new(),
            parked: false,
            back_until: 0,
            to_screen: false,
            opportunistic_rest: false,
            last_fired: None,
            reload_frames: BTreeMap::new(),
        }
    }
}

impl Pack {
    /// Losses stay in the denominator until the survivors recover at home.
    fn strength(&mut self, cores: usize, health_sum: f32) -> f32 {
        self.original_cores = self.original_cores.max(cores);
        health_sum / self.original_cores.max(1) as f32
    }

    /// Health always wins over run commitment and discretionary resting.
    fn retreat_for_health(&mut self, strength: f32, threshold: f32) -> bool {
        if strength >= threshold || (self.parked && !self.opportunistic_rest) {
            return false;
        }
        self.parked = true;
        self.opportunistic_rest = false;
        self.target = NULLPOS;
        self.pass = None;
        self.to_screen = false;
        self.back_until = 0;
        true
    }

    fn recover(&mut self, cores: usize, health: f32, healed: f32, at_haven: bool) -> bool {
        if !self.parked || health < healed || !at_haven {
            return false;
        }
        self.parked = false;
        self.opportunistic_rest = false;
        self.original_cores = cores;
        true
    }

    /// Resolve a completed or abandoned pass before judging health, threat
    /// or another target, so commitment cannot chain past those checks.
    fn finish_pass(&mut self, pos: Vec3, target_left: bool) {
        if !target_left {
            self.target = NULLPOS;
            self.pass = None;
        } else if self.pass.is_some_and(|p| p.along(pos, self.pass_target) >= PASS_OUT) {
            self.pass = None;
            self.passes += 1;
        }
    }

    fn fired(&mut self, frame: i32) {
        self.last_fired = Some(self.last_fired.map_or(frame, |old| old.max(frame)));
    }

    /// Polling also catches shots where the engine omits WeaponFired.
    fn observe_reload(&mut self, key: (i32, usize), reload: i32, frame: i32) {
        let previous = self.reload_frames.insert(key, reload);
        if previous.map_or(reload > frame, |old| reload > old) {
            self.fired(frame);
        }
    }

    fn quiet(&self, frame: i32, seconds: f32) -> bool {
        self.last_fired.is_none_or(|last| frame - last >= (seconds * 30.0) as i32)
    }

    fn absorb_firing(&mut self, other: Pack) {
        if let Some(last) = other.last_fired {
            self.fired(last);
        }
        self.reload_frames.extend(other.reload_frames);
    }
}

/// Recoverable health valued as metal, per second spent on the detour and
/// repairs. Build work is an estimate; no build power means no useful rest.
fn repair_rate(value: f32, work: f32, bp: f32, distance: f32, speed: f32) -> f32 {
    if value <= 0.0 || bp <= 0.0 {
        return 0.0;
    }
    value / (2.0 * distance / speed.max(1.0) + work / bp).max(1.0)
}

fn rest_worthwhile(repair: f32, carnage: f32, already_resting: bool) -> bool {
    repair > 0.0 && if already_resting { repair * REST_MARGIN >= carnage } else { repair > carnage * REST_MARGIN }
}

/// A unit's heading on the ground: where it is going, else toward `toward`
/// (else along the x axis).
pub fn heading(vel: Vec3, pos: Vec3, toward: Vec3) -> (f32, f32) {
    let unit = |x: f32, z: f32| {
        let l = (x * x + z * z).sqrt();
        (l > 1e-3).then(|| (x / l, z / l))
    };
    (unit(vel.x, vel.z))
        .or_else(|| (toward != NULLPOS).then(|| unit(toward.x - pos.x, toward.z - pos.z)).flatten())
        .unwrap_or((1.0, 0.0))
}

/// The pass over `target` from `pos` (heading `heading`) that brings the
/// most guns within reach of it, and how many: `guns` are the formation's
/// places round the leader (the leader's (0, 0)) and each gun's reach. The
/// pass runs from `pos` toward the target, the formation sliding along it
/// unturned, so a gun passes the target as far off as its place lies
/// across the way plus the leader's line; of the lines `clear` lets it take
/// that bring the most guns in, it takes the one nearest where the heading
/// already runs. None if `clear` lets it take no line at all.
pub fn plan_pass(
    pos: Vec3,
    heading: (f32, f32),
    target: Vec3,
    guns: &[((f32, f32), f32)],
    clear: impl Fn(&Pass) -> bool,
) -> Option<(Pass, usize)> {
    plan_pass_inner(pos, heading, target, guns, None, clear)
}

/// A forced tangential pass keeps the entire formation to one side of
/// the target's collision margin. Of equally armed passes choose the one
/// with the most standoff, then the least change from the current heading.
fn plan_tangent_pass(
    pos: Vec3,
    heading: (f32, f32),
    target: Vec3,
    guns: &[((f32, f32), f32)],
    clearance: f32,
    clear: impl Fn(&Pass) -> bool,
) -> Option<(Pass, usize)> {
    plan_pass_inner(pos, heading, target, guns, Some(clearance), clear)
}

fn plan_pass_inner(
    pos: Vec3,
    heading: (f32, f32),
    target: Vec3,
    guns: &[((f32, f32), f32)],
    tangent: Option<f32>,
    clear: impl Fn(&Pass) -> bool,
) -> Option<(Pass, usize)> {
    let (dx, dz) = (target.x - pos.x, target.z - pos.z);
    let d = (dx * dx + dz * dz).sqrt();
    let way = if d > 1.0 { (dx / d, dz / d) } else { heading };
    let left = (-way.1, way.0);
    // Where the heading's line runs by the target, across the way.
    let ahead = (dx * heading.0 + dz * heading.1).max(0.0);
    let natural = (pos.x + heading.0 * ahead - target.x) * left.0 + (pos.z + heading.1 * ahead - target.z) * left.1;
    let across: Vec<(f32, f32)> = guns.iter().map(|&((ox, oz), reach)| (ox * left.0 + oz * left.1, reach)).collect();
    let span = across.iter().map(|&(p, reach)| p.abs() + reach).fold(0.0, f32::max);
    let mut best: Option<(usize, f32, f32, Pass)> = None;
    let mut side = -span;
    while side <= span {
        let pass = Pass { way, side };
        let n = across.iter().filter(|&&(p, reach)| (side + p).abs() <= reach).count();
        let off = (side - natural).abs();
        let gap = tangent.map_or(0.0, |_| across.iter().map(|&(p, _)| (side + p).abs()).fold(f32::INFINITY, f32::min));
        let outside = tangent.is_none_or(|margin| {
            across.iter().all(|&(p, _)| side + p >= margin) || across.iter().all(|&(p, _)| side + p <= -margin)
        });
        if n > 0
            && outside
            && best.is_none_or(|(m, g, o, _)| n > m || (n == m && (gap > g || (gap == g && off < o))))
            && clear(&pass)
        {
            best = Some((n, gap, off, pass));
        }
        side += PASS_STEP;
    }
    best.map(|(n, _, _, pass)| (pass, n))
}

/// Explicit pass/charge doctrines constrain prediction as well as driving.
fn attack_modes(attack: Attack) -> &'static [bool] {
    match attack {
        Attack::Pass => &[false],
        Attack::Charge => &[true],
        Attack::Mixed => &[true, false],
    }
}

fn charges(attack: Attack, mobile: bool, predicted: Option<bool>) -> bool {
    match attack {
        Attack::Pass => false,
        Attack::Charge => true,
        Attack::Mixed => predicted.unwrap_or(mobile),
    }
}

/// Two waypoints along `pass` over `target` for a leader at `pos`: the pass
/// point while it is ahead, else points on along the line, so that the
/// leader never brakes.
pub fn pass_way(pos: Vec3, target: Vec3, pass: &Pass) -> [Vec3; 2] {
    let q = pass.point(target);
    let a = (pass.along(pos, target) + PASS_LEAD).max(0.0);
    let on = |x: f32| Vec3::xz(q.x + pass.way.0 * x, q.z + pass.way.1 * x);
    [on(a), on(a + PASS_ON)]
}

/// The way a run judges: from `pos` by the target `t` and on past it.
fn pack_route(pos: Vec3, t: Vec3) -> [Vec3; 3] {
    let d = distance(pos, t).max(1.0);
    [pos, t, Vec3::xz(t.x + (t.x - pos.x) / d * PASS_ON, t.z + (t.z - pos.z) / d * PASS_ON)]
}

/// A lateral turning waypoint from cruise speed and angular speed. The
/// engine slows down near raw-move goals, so the ideal radius alone is too
/// small. Formation probes retain speed with five radii for cores allowed
/// to turn at full speed, and 5.5 for those the engine may brake. Round up and retain the old
/// distance when tighter steering has no margin.
fn turn_distance(speed: f32, turn_rate: f32, turn_speed_limit: f32) -> f32 {
    if !speed.is_finite() || speed <= 0.0 || !turn_rate.is_finite() || turn_rate <= 0.0 {
        return TURN;
    }
    let omega = turn_rate * std::f32::consts::TAU * 30.0 / 65536.0;
    let margin = if turn_speed_limit.is_finite() && turn_speed_limit >= speed { 5.0 } else { 5.5 };
    ((margin * speed / omega / 10.0).ceil() * 10.0).clamp(TURN_MIN, TURN)
}

/// For a destination behind us, start with a lateral waypoint `turn` away
/// so the force swings round without reversing its drive direction.
pub fn turn_point(pos: Vec3, heading: (f32, f32), dest: Vec3, turn: f32) -> Option<Vec3> {
    let (tx, tz) = (dest.x - pos.x, dest.z - pos.z);
    if heading.0 * tx + heading.1 * tz >= 0.0 || tx * tx + tz * tz < turn * turn {
        return None;
    }
    let side = if heading.0 * tz - heading.1 * tx >= 0.0 { 1.0 } else { -1.0 };
    // The heading turned a right angle: (-hz, hx) to the left.
    let (ax, az) = (-heading.1 * side, heading.0 * side);
    Some(Vec3::xz(pos.x + ax * turn, pos.z + az * turn))
}

/// Up to `n` points of `path`, each at least `spacing` from the one before
/// (the first from `from`); the path's end if none is that far.
pub fn spaced(path: impl IntoIterator<Item = Vec3>, from: Vec3, n: usize, spacing: f32) -> Vec<Vec3> {
    let mut out = Vec::new();
    let (mut last, mut end) = (from, None);
    for p in path {
        end = Some(p);
        if out.len() < n && distance(last, p) >= spacing {
            out.push(p);
            last = p;
        }
    }
    if out.is_empty() {
        out.extend(end);
    }
    out
}

/// Movement toward a destination. Attacks may start with a turning arc;
/// retreats follow the terrain path immediately, even directly behind us.
/// Always replace old orders near the destination too.
fn approach_way(
    pos: Vec3,
    head: (f32, f32),
    dest: Vec3,
    assault: bool,
    turn: f32,
    path: impl IntoIterator<Item = Vec3>,
) -> Vec<Vec3> {
    if distance(pos, dest) <= STAND_NEAR {
        return vec![dest];
    }
    let mut way: Vec<Vec3> = if assault { turn_point(pos, head, dest, turn).into_iter().collect() } else { Vec::new() };
    let from = way.last().copied().unwrap_or(pos);
    way.extend(spaced(path.into_iter().filter(|p| distance(*p, pos) > SPACING / 2.0), from, 2, SPACING));
    if way.last().is_none_or(|&l| distance(l, dest) < 2.0 * SPACING && distance(l, dest) > 1.0) {
        way.push(dest);
    }
    way
}

/// What the fight predictor expects of a run (see `pack_judge`): its
/// engagements, the enemy metal they kill, the economy at the target taken
/// if any of the pack gets there, the pack's metal lost, and how many of its
/// cores go in and are left.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Judgement {
    pub engagements: usize,
    pub gain: f32,
    pub prize: f32,
    pub loss: f32,
    pub cores: usize,
    pub left: usize,
}

impl Judgement {
    pub fn worth(&self) -> f32 {
        self.gain + self.prize - self.loss
    }
}

/// How far along `route` the point of it nearest `p` lies, and how far off
/// `p` is.
pub fn route_project(route: &[Vec3], p: Vec3) -> (f32, f32) {
    let (mut best, mut along) = ((f32::INFINITY, 0.0), 0.0);
    for w in route.windows(2) {
        let (a, b) = (w[0], w[1]);
        let (dx, dz) = (b.x - a.x, b.z - a.z);
        let len2 = dx * dx + dz * dz;
        let t = if len2 > 0.0 { (((p.x - a.x) * dx + (p.z - a.z) * dz) / len2).clamp(0.0, 1.0) } else { 0.0 };
        let q = Vec3::xz(a.x + t * dx, a.z + t * dz);
        let off = distance(p, q);
        if off < best.0 {
            best = (off, along + t * len2.sqrt());
        }
        along += len2.sqrt();
    }
    (best.1, best.0)
}

/// The point `s` along `route` (its last point beyond its end).
pub fn route_point(route: &[Vec3], mut s: f32) -> Vec3 {
    for w in route.windows(2) {
        let len = distance(w[0], w[1]);
        if s <= len && len > 0.0 {
            let t = s / len;
            return Vec3::xz(w[0].x + t * (w[1].x - w[0].x), w[0].z + t * (w[1].z - w[0].z));
        }
        s -= len;
    }
    route.last().copied().unwrap_or(NULLPOS)
}

/// Engagements along a way: the indices of `along` (distances along it) in
/// order, grouped where a gap longer than `gap` parts them.
pub fn engagements(along: &[f32], gap: f32) -> Vec<Vec<usize>> {
    let mut order: Vec<usize> = (0..along.len()).collect();
    order.sort_by(|&a, &b| along[a].total_cmp(&along[b]));
    let mut groups: Vec<Vec<usize>> = Vec::new();
    for i in order {
        match groups.last_mut() {
            Some(g) if along[i] - along[*g.last().unwrap()] <= gap => g.push(i),
            _ => groups.push(vec![i]),
        }
    }
    groups
}

fn ordinal(n: u32) -> String {
    match n {
        1 => "a first".into(),
        2 => "a second".into(),
        3 => "a third".into(),
        _ => format!("a {n}th"),
    }
}

impl Ai {
    /// A pack's conduct (see the module): where it dives now, if it does;
    /// else why not.
    pub(crate) fn taskforce_pack(&mut self, sid: SquadId) -> (Option<Vec3>, String) {
        self.pack_merge(sid);
        self.pack_lead(sid);
        let turn = self.pack_turn_distance(sid);
        let Some(s) = self.squads.get(sid) else { return (None, "gone".into()) };
        let shape = s.shape.clone();
        let Some(leader) = s.get_leader(&self.fighters) else { return (None, "no leader".into()) };
        let (engaging, courage) = (s.engaging, s.courage);
        let pos = leader.pos();
        let cores: Vec<Unit> = (s.everyone().filter_map(|id| self.fighters.get(&id)))
            .map(|f| f.unit)
            .filter(|u| shape.is_core(u.def().map(|d| d.id())))
            .collect();
        let health_sum = cores.iter().map(|&u| health_fraction(u)).sum::<f32>();
        let health = health_sum / cores.len().max(1) as f32;
        let rallied = s.rally_fraction(&self.fighters).is_none_or(|r| r > shape.rallied);
        let trailers: Vec<Unit> =
            (s.trailers(&self.fighters).iter().filter_map(|id| self.fighters.get(id))).map(|f| f.unit).collect();
        let mut st = self.taskforces.packs.get(&sid).cloned().unwrap_or_default();
        let strength = st.strength(cores.len(), health_sum);
        let doctrine = shape.doctrine.clone();
        // A new or reloaded pack has no five-second firing history yet.
        if st.last_fired.is_none() && st.reload_frames.is_empty() {
            st.fired(self.frame);
        }
        let members: Vec<Unit> = s.everyone().filter_map(|id| self.fighters.get(&id)).map(|f| f.unit).collect();
        for unit in members {
            for (index, weapon) in unit.weapons().into_iter().enumerate() {
                if weapon.def().is_some_and(|d| d.range() > 0.0 && !d.name().contains("fake")) {
                    st.observe_reload((unit.id(), index), weapon.reload_frame(), self.frame);
                }
            }
        }
        st.reload_frames.retain(|(id, _), _| self.fighters.get(id).is_some_and(|f| f.squad == Some(sid)));

        // A screen member that is hurt goes for repairs alone.
        let mut screen = Vec::new();
        for u in trailers {
            if health_fraction(u) < shape.retreat_health {
                self.retreat_send(u);
            } else {
                screen.push(u.pos());
            }
        }
        let screen_at = (!screen.is_empty()).then(|| {
            let n = screen.len() as f32;
            Vec3::xz(screen.iter().map(|p| p.x).sum::<f32>() / n, screen.iter().map(|p| p.z).sum::<f32>() / n)
        });
        let head = heading(leader.vel(), pos, st.target);
        let home = self.graph_get_closest_haven(pos);
        let target_left =
            st.target != NULLPOS && self.frame - st.since < TARGET_FOR && self.pack_target_left(st.target).is_some();
        st.finish_pass(pos, target_left);
        // Commitment only defers tactical reconsideration. Health below
        // the retreat threshold cancels the run at any point.
        let running = shape.commit && st.target != NULLPOS && st.pass.is_some();

        // Forced retreats keep the old recovery target. Discretionary rests
        // can end earlier when the next run offers more value per second.
        let healed = shape.heal_to.max(shape.retreat_health);
        if st.recover(cores.len(), health, healed, distance(pos, home) <= STAND_NEAR) {
            self.log(&format!(
                "taskforce: the {doctrine} pack (squad {sid}) is repaired ({:.0}%, {} cores)",
                100.0 * health,
                cores.len()
            ));
        } else if st.retreat_for_health(strength, shape.retreat_health) {
            self.log(&format!(
                "taskforce: the {doctrine} pack (squad {sid}) parks for repairs ({:.0}% strength, \
                 {:.0}% health, {} of {} original cores)",
                100.0 * strength,
                100.0 * health,
                cores.len(),
                st.original_cores
            ));
        }
        if !running && ((!st.parked && health < shape.repair_below) || st.opportunistic_rest) {
            let was_resting = st.opportunistic_rest;
            let mut rest = 0.0;
            let mut carnage = 0.0;
            if distance(pos, home) <= REST_NEAR && self.pack_way_clear(leader, home) {
                let mut value = 0.0;
                let mut work = 0.0;
                for &unit in &cores {
                    if let Some(def) = unit.def() {
                        let missing = (healed - health_fraction(unit)).max(0.0);
                        value += missing * def.cost(self.metal);
                        work += missing * def.build_time();
                    }
                }
                rest = repair_rate(value, work, self.war.get_bp(home), distance(pos, home), shape.pace);
                if rest > 0.0 {
                    carnage = self.pack_carnage_rate(sid, leader, pos, &shape, &st);
                }
            }
            let take_rest = rest_worthwhile(rest, carnage, was_resting)
                && cores.iter().all(|&u| u.id() == leader.id() || self.pack_way_clear(u, home));
            if take_rest && !was_resting {
                st.parked = true;
                st.opportunistic_rest = true;
                st.target = NULLPOS;
                st.pass = None;
                self.log(&format!(
                    "taskforce: the {doctrine} pack (squad {sid}) rests near its haven ({:.0}%, {} cores): \
                     repair {rest:.1} metal/s, further carnage {carnage:.1} metal/s",
                    100.0 * health,
                    cores.len()
                ));
            } else if !take_rest && was_resting {
                st.parked = false;
                st.opportunistic_rest = false;
                self.log(&format!(
                    "taskforce: the {doctrine} pack (squad {sid}) ends its rest ({:.0}%): \
                     repair {rest:.1} metal/s, further carnage {carnage:.1} metal/s",
                    100.0 * health
                ));
            }
        }
        if st.parked {
            self.pack_go(sid, pos, head, home, false);
            self.pack_screen(sid, home, false);
            let why = if st.opportunistic_rest { "opportunistic rest" } else { "parked for repairs" };
            self.taskforces.packs.insert(sid, st);
            return (None, why.into());
        }

        // Five seconds without firing is enough: incoming hits and a
        // committed but quiet run do not veto an otherwise safe departure.
        if shape.repair_below > 0.0 && st.quiet(self.frame, shape.quiet) {
            let mut hurt = Vec::new();
            for &unit in &cores {
                if health_fraction(unit) < shape.repair_below {
                    let haven = self.graph_get_closest_haven(unit.pos());
                    if self.pack_way_clear(unit, haven) {
                        hurt.push(unit);
                    }
                }
            }
            if !hurt.is_empty() {
                let healths: Vec<String> =
                    hurt.iter().map(|&u| format!("{:.0}%", 100.0 * health_fraction(u))).collect();
                let quiet = st.last_fired.map_or("it has not fired".to_owned(), |last| {
                    format!("no shots for {:.0} s", (self.frame - last) as f32 / 30.0)
                });
                self.log(&format!(
                    "taskforce: the {doctrine} pack (squad {sid}) sends {} of {} cores for repairs ({}): \
                     {quiet}, each has a clear way home",
                    hurt.len(),
                    cores.len(),
                    healths.join(" ")
                ));
                self.taskforces.packs.insert(sid, st);
                for u in hurt {
                    self.retreat_send(u);
                }
                return (None, "sends its hurt for repairs".into());
            }
        }

        // Enough cores, and together before it goes.
        let need = if engaging { shape.cores_to_stay } else { shape.cores_to_go } as usize;
        if cores.len() < need || (!engaging && !rallied) {
            st.target = NULLPOS;
            st.pass = None;
            self.taskforces.packs.insert(sid, st);
            let at = self.pack_gather(pos);
            self.pack_go(sid, pos, head, at, false);
            self.pack_screen(sid, at, false);
            let why = if cores.len() < need { format!("{} cores of {need}", cores.len()) } else { "scattered".into() };
            return (None, why);
        }

        // Outweighed where it is: home.
        // (Porc counts whatever the pack weighs: it outranges every gun.)
        // (Judged by the predictor, a run's fights are its business: porc only.)
        // Porc counts beyond `porc_share` of the pack's own threat.
        let predictor = matches!(shape.judge, Judge::Predictor | Judge::Mech | Judge::Fates);
        let mech = shape.judge == Judge::Mech;
        let fates = shape.judge == Judge::Fates;
        let (outweighed, fthreat, porc_cap) = {
            let view = self.war.view(leader.def());
            let fthreat = view.get_friendly_threat(pos);
            let porc_cap = shape.shun_porc.then_some(shape.porc_share * fthreat);
            let tolerance = (courage - 1.0).max(0.0) * view.get_total_friendly_threat(pos);
            let porc = porc_cap.map_or(0.0, |c| (view.get_porc_threat(pos) - c).max(0.0));
            let threat = if predictor { porc } else { view.get_effective_threat(pos).max(porc) };
            (threat / courage > tolerance, fthreat, porc_cap)
        };
        let porc_ok = |ai: &Ai, p: Vec3| porc_cap.is_none_or(|c| ai.war.view(leader.def()).get_porc_threat(p) <= c);
        if shape.flee_threat && outweighed && !running {
            st.back_until = self.frame + BACK_FOR;
        }
        if self.frame < st.back_until {
            st.target = NULLPOS;
            st.pass = None;
            st.to_screen = false;
            self.taskforces.packs.insert(sid, st);
            self.pack_go(sid, pos, head, home, false);
            self.pack_screen(sid, home, false);
            return (None, "threat".into());
        }

        // Raiders on it: back to the screen, through it, the screen holding,
        // until it is there.
        let far_screen = screen_at.filter(|&at| distance(pos, at) > SCREEN_NEAR);
        if far_screen.is_none() {
            st.to_screen = false;
        }
        if let Some(at) = far_screen.filter(|_| shape.rip_value > 0.0 && !running) {
            let raiders = self.pack_raiders(pos);
            if st.to_screen || raiders >= shape.rip_value {
                if !st.to_screen {
                    self.log(&format!(
                        "taskforce: the {doctrine} pack (squad {sid}) falls back to its screen {:.0} away from \
                         raiders worth {raiders:.0}",
                        distance(pos, at)
                    ));
                }
                st.to_screen = true;
                st.target = NULLPOS;
                st.pass = None;
                self.taskforces.packs.insert(sid, st);
                let past = if distance(at, home) > 1.0 { get_directional_point(at, home, 150.0) } else { at };
                self.pack_go(sid, pos, head, past, false);
                self.pack_screen(sid, home, true);
                return (None, "raiders, to its screen".into());
            }
        }

        // The target: kept, following what is there, while something is left
        // that it survives.
        let left = (st.target != NULLPOS && self.frame - st.since < TARGET_FOR)
            .then(|| self.pack_target_left(st.target))
            .flatten();
        // (By the rule it was chosen by, the threat there less ours: a
        // stricter one dropped it again a conduct later.)
        let mut keep = left.filter(|&(t, _)| {
            let view = self.war.view(leader.def());
            running || (porc_ok(self, t) && (predictor || view.get_effective_threat(t) <= fthreat))
        });
        st.shun.retain(|s| s.1 > self.frame);
        // Judged by the predictor, the rest of the run must stay worth it.
        let rejudge = |ai: &mut Ai, t: Vec3| {
            if fates {
                ai.pack_fates(sid, &pack_route(pos, t), charges(shape.attack, st.mobile, st.ram))
            } else {
                ai.pack_judge(sid, &pack_route(pos, t), mech)
            }
        };
        if let Some((t, _)) = keep.filter(|_| predictor && !running)
            && let Some((j, detail)) = rejudge(self, t)
            && j.worth() < shape.judge_margin
        {
            self.pack_log_judgement(sid, t, &j, "gives up on");
            self.pack_emit_judgement(sid, t, &j, detail, "gives up on");
            st.shun.push((t, self.frame + JUDGE_SHUN));
            keep = None;
        }
        if let Some((t, mobile)) = keep {
            st.target = t;
            st.mobile = mobile;
        } else {
            st.pass = None;
            st.passes = 0;
            let ahead = Vec3::xz(pos.x + head.0 * LOOK_AHEAD, pos.z + head.1 * LOOK_AHEAD);
            let mut shun: Vec<Vec3> = st.shun.iter().map(|s| s.0).collect();
            let pick = |ai: &mut Ai, shun: &[Vec3]| {
                ai.pack_sweep(leader, pos, porc_cap, fthreat, shun, &shape, predictor)
                    .map(|(t, value)| (t, Some(value)))
                    .unwrap_or_else(|| (ai.war_best_target_shunning(leader, ahead, false, porc_cap, shun).0, None))
            };
            let mut near_value = None;
            // Judged by fates: the best run of a few targets, each rammed or
            // passed, by what it should kill less what it should lose.
            if fates {
                struct JudgedRun {
                    target: Vec3,
                    ram: bool,
                    judgement: Judgement,
                    detail: Vec<Value>,
                    near_value: Option<f32>,
                }
                let mut best: Option<JudgedRun> = None;
                let mut tried = Vec::new();
                for _ in 0..FATE_CANDIDATES {
                    let (t, value) = pick(self, &shun);
                    if t == NULLPOS {
                        break;
                    }
                    shun.push(t);
                    tried.push(t);
                    for &ram in attack_modes(shape.attack) {
                        let Some((j, detail)) = self.pack_fates(sid, &pack_route(pos, t), ram) else { continue };
                        if best.as_ref().is_none_or(|b| j.worth() > b.judgement.worth()) {
                            best = Some(JudgedRun { target: t, ram, judgement: j, detail, near_value: value });
                        }
                    }
                }
                match best {
                    Some(JudgedRun { target: t, ram, judgement: j, detail, near_value: value })
                        if j.worth() >= shape.judge_margin =>
                    {
                        near_value = value;
                        let verdict = if ram { "rams" } else { "passes" };
                        self.pack_log_judgement(sid, t, &j, verdict);
                        self.pack_emit_judgement(sid, t, &j, detail, verdict);
                        st.ram = Some(ram);
                        st.target = self.war_mark_target(t);
                        st.since = self.frame;
                        st.mobile = self.pack_target_left(t).is_some_and(|l| l.1);
                    }
                    best => {
                        if let Some(JudgedRun { target: t, judgement: j, detail, .. }) = best {
                            self.pack_log_judgement(sid, t, &j, "passes up");
                            self.pack_emit_judgement(sid, t, &j, detail, "passes up");
                        }
                        for t in tried {
                            st.shun.push((t, self.frame + JUDGE_SHUN));
                        }
                        st.target = NULLPOS;
                        self.taskforces.packs.insert(sid, st);
                        let at = self.pack_gather(pos);
                        self.pack_go(sid, pos, head, at, false);
                        self.pack_screen(sid, at, false);
                        return (None, "no run worth it".into());
                    }
                }
            }
            let t = if fates {
                st.target
            } else {
                let (target, value) = pick(self, &shun);
                near_value = value;
                target
            };
            if t == NULLPOS {
                st.target = NULLPOS;
                self.taskforces.packs.insert(sid, st);
                let at = self.pack_gather(pos);
                self.pack_go(sid, pos, head, at, false);
                self.pack_screen(sid, at, false);
                return (None, "no target".into());
            }
            // Judged by the predictor, a run not worth it is passed up.
            // (Every arm asks, for the journal: what the predictor expected
            // of the runs the threat maps took is the unselected sample.)
            let judged = if fates { None } else { self.pack_judge(sid, &pack_route(pos, t), mech) };
            if let Some((j, detail)) = judged.clone().filter(|_| !predictor) {
                self.pack_emit_judgement(sid, t, &j, detail, "map takes on");
            }
            if predictor && let Some((j, detail)) = judged {
                let go = j.worth() >= shape.judge_margin;
                let verdict = if go { "takes on" } else { "passes up" };
                self.pack_log_judgement(sid, t, &j, verdict);
                self.pack_emit_judgement(sid, t, &j, detail, verdict);
                if !go {
                    st.shun.push((t, self.frame + JUDGE_SHUN));
                    st.target = NULLPOS;
                    self.taskforces.packs.insert(sid, st);
                    let at = self.pack_gather(pos);
                    self.pack_go(sid, pos, head, at, false);
                    self.pack_screen(sid, at, false);
                    return (None, "judged not worth it".into());
                }
            }
            if !fates {
                st.target = self.war_mark_target(t);
                st.since = self.frame;
                st.mobile = self.pack_target_left(t).is_some_and(|l| l.1);
            }
            let how = near_value.map_or("by the global picker".to_owned(), |v| format!("swept, {v:.0} near it"));
            self.log(&format!(
                "taskforce: the {doctrine} pack (squad {sid}) dives ({:.0}, {:.0}), {:.0} away, {} cores at {:.0}%, {how}",
                st.target.x,
                st.target.z,
                distance(pos, st.target),
                cores.len(),
                100.0 * health
            ));
        }
        let target = st.target;
        let charge = charges(shape.attack, st.mobile, st.ram.filter(|_| fates));
        // A charge is aimed anew until it is close.
        let aim = charge && st.pass.is_some_and(|p| p.along(pos, st.pass_target) < -CHARGE_LOCK);
        if st.pass.is_none() && distance(pos, target) > PLAN_AT {
            let way = self.pack_way(sid, pos, head, target, true);
            self.pack_drive(sid, &way);
        } else {
            let pass = match st.pass.filter(|_| !aim) {
                Some(p) => p,
                None => {
                    let guns = self.pack_guns(sid, shape.pass_reach);
                    let clearance = if shape.attack == Attack::Pass {
                        let body = cores.iter().filter_map(|u| u.def()).map(|d| d.radius()).fold(0.0, f32::max);
                        let target_body = self
                            .war
                            .targets
                            .values()
                            .filter(|e| !e.is_dead && distance(e.position, target) < TARGET_LEFT)
                            .filter_map(|e| e.def_radius())
                            .fold(0.0, f32::max);
                        body + target_body + PASS_STEP
                    } else {
                        0.0
                    };
                    let view = self.war.view(leader.def());
                    let clear = |p: &Pass| {
                        let q = p.point(target);
                        let at = |x: f32| Vec3::xz(q.x + p.way.0 * x, q.z + p.way.1 * x);
                        let porc = |c: f32| {
                            [-PASS_IN, -PASS_IN / 2.0, 0.0, PASS_ON / 2.0, PASS_ON]
                                .iter()
                                .all(|&x| view.get_porc_threat(at(x)) <= c)
                        };
                        // A charge runs over it (the line nearest).
                        (!charge || p.side.abs() <= PASS_STEP / 2.0) && porc_cap.is_none_or(porc)
                    };
                    let planned = if shape.attack == Attack::Pass {
                        plan_tangent_pass(pos, head, target, &guns, clearance, clear)
                    } else {
                        plan_pass(pos, head, target, &guns, clear)
                    };
                    let Some((p, n)) = planned else {
                        // No way over it clear of porc: another target.
                        self.log(&format!(
                            "taskforce: the {doctrine} pack (squad {sid}) shuns ({:.0}, {:.0}): no pass clear of porc",
                            target.x, target.z
                        ));
                        st.shun.push((target, self.frame + SHUN_FOR));
                        st.target = NULLPOS;
                        self.taskforces.packs.insert(sid, st);
                        let at = self.pack_gather(pos);
                        self.pack_go(sid, pos, head, at, false);
                        self.pack_screen(sid, home, false);
                        return (None, "no pass clear of porc".into());
                    };
                    if !aim {
                        self.log(&format!(
                            "taskforce: the {doctrine} pack (squad {sid}) {} ({:.0}, {:.0}) {} time, {n} of {} guns \
                             within reach, its line {:.0} off",
                            if charge { "charges" } else { "passes" },
                            target.x,
                            target.z,
                            ordinal(st.passes + 1),
                            guns.len(),
                            p.side
                        ));
                    }
                    p
                }
            };
            if st.pass.is_none() || aim {
                st.pass_target = target;
            }
            st.pass = Some(pass);
            let [a, b] = pass_way(pos, st.pass_target, &pass);
            // Swinging round to come back over it.
            let way: Vec<Vec3> = turn_point(pos, head, a, turn).into_iter().chain([a, b]).collect();
            self.pack_drive(sid, &way);
        }
        self.pack_screen(sid, home, false);
        self.taskforces.packs.insert(sid, st);
        (Some(target), String::new())
    }

    /// What the fight predictor expects of pack `sid` running along `route`
    /// (from where it stands, by its target, `route[1]`, and on): the armed
    /// enemies seen lately within their reach of the way (mobiles a little
    /// more) grouped into engagements by where they lie along it, each asked
    /// in turn as a fight with the pack as it would arrive there (moved to
    /// meet it, less the cores the fights before were expected to kill, the
    /// most hurt first); the unarmed enemy economy at the target is the prize
    /// if any core gets there. None without the model.
    /// With `mech`, by the mechanics model's means; each engagement's record
    /// (both models' answers, the units of both sides) is returned beside
    /// the judgement, for the journal.
    fn pack_judge(&mut self, sid: SquadId, route: &[Vec3], mech: bool) -> Option<(Judgement, Vec<Value>)> {
        let model = self.pack_model()?;
        let target = *route.get(1)?;
        let s = self.squads.get(sid)?;
        let ours: Vec<(i32, UnitDef, f32, Vec3)> = (s.everyone().filter_map(|id| self.fighters.get(&id)))
            .filter(|f| s.shape.is_core(f.unit.def().map(|d| d.id())) && f.unit.health() > 0.0)
            .filter_map(|f| f.unit.def().map(|d| (f.unit.id(), d, f.unit.health(), f.get_pos())))
            .collect();
        let frame = self.frame;
        let seen: Vec<(i32, UnitDef, f32, Vec3, bool)> = (self.war.targets.values())
            .filter(|e| !e.is_dead && e.identified && frame - e.last_seen <= JUDGE_SEEN)
            .filter_map(|e| e.ud.map(|d| (e.unit_id, d, e.get_health() * e.def_health(), e.position, e.can_fly())))
            .collect();
        let mut pack: Vec<(Arc<UnitSpec>, f32, Vec3, i32)> =
            ours.into_iter().map(|(id, d, hp, p)| (self.fight_spec(d), hp, p, id)).collect();
        let mut j = Judgement { cores: pack.len(), ..Judgement::default() };
        let mut armed: Vec<(Arc<UnitSpec>, f32, Vec3, i32)> = Vec::new();
        let mut along = Vec::new();
        let mut detail = Vec::new();
        for (eid, def, hp, p, flies) in seen {
            let spec = self.fight_spec(def);
            if spec.weapons.is_empty() {
                if !flies && distance(p, target) < TARGET_LEFT {
                    j.prize += spec.cost;
                }
                continue;
            }
            let reach =
                spec.weapons.iter().map(|w| w.range).fold(0.0, f32::max) + if spec.mobile { JUDGE_CHASE } else { 0.0 };
            let (s, off) = route_project(route, p);
            if off <= reach {
                armed.push((spec, hp, p, eid));
                along.push(s);
            }
        }
        for group in engagements(&along, JUDGE_GAP) {
            if pack.is_empty() {
                break;
            }
            j.engagements += 1;
            let at = group.iter().map(|&i| along[i]).sum::<f32>() / group.len() as f32;
            // The pack as it meets them: moved along the way unless they are
            // upon it already.
            let n = pack.len() as f32;
            let c = Vec3::xz(pack.iter().map(|u| u.2.x).sum::<f32>() / n, pack.iter().map(|u| u.2.z).sum::<f32>() / n);
            let meet = if at > JUDGE_MEET { route_point(route, at - JUDGE_MEET) } else { c };
            let (sx, sz) = (meet.x - c.x, meet.z - c.z);
            let mut snap = FightSnapshot::default();
            for (spec, hp, p, _) in &pack {
                snap.units.push(SnapUnit { side: 0, spec: spec.clone(), hp: *hp, x: p.x + sx, z: p.z + sz });
            }
            for &i in &group {
                let (spec, hp, p, _) = &armed[i];
                snap.units.push(SnapUnit { side: 1, spec: spec.clone(), hp: *hp, x: p.x, z: p.z });
            }
            let out = model.predict_with(&snap, true);
            let cost_a: f32 = pack.iter().map(|u| u.0.cost).sum();
            let cost_b: f32 = group.iter().map(|&i| armed[i].0.cost).sum();
            let statics: f32 = group.iter().filter(|&&i| !armed[i].0.mobile).map(|&i| armed[i].0.cost).sum();
            let (mean_a, mean_b) = match out.mech.filter(|_| mech) {
                Some(m) => (m.mean_a, m.mean_b),
                None => (out.mean_a, out.mean_b),
            };
            detail.push(json!({
                "i": j.engagements, "at": at.round(), "ids_a": pack.iter().map(|u| u.3).collect::<Vec<_>>(),
                "ids_b": group.iter().map(|&i| armed[i].3).collect::<Vec<_>>(),
                "metal_a": cost_a.round(), "metal_b": cost_b.round(),
                "static_b": if cost_b > 0.0 { statics / cost_b } else { 0.0 },
                "base": {"ka": out.killed_a, "kb": out.killed_b, "ma": out.mean_a, "mb": out.mean_b,
                         "p": out.p_a_wins},
                "mech": out.mech.map(|m| json!({"ka": m.killed_a, "kb": m.killed_b, "ma": m.mean_a,
                                                "mb": m.mean_b, "p": m.p_a_wins})),
            }));
            j.loss += mean_a * cost_a;
            j.gain += mean_b * cost_b;
            for u in &mut pack {
                (u.2.x, u.2.z) = (u.2.x + sx, u.2.z + sz);
            }
            let dead = (mean_a * n).round() as usize;
            pack.sort_by(|a, b| a.1.total_cmp(&b.1));
            pack.drain(..dead.min(pack.len()));
        }
        j.left = pack.len();
        if pack.is_empty() {
            j.prize = 0.0;
        }
        Some((j, detail))
    }

    /// What the mechanics model's fates expect of pack `sid` running along
    /// `route` (from where it stands, by its target, `route[1]`, and on),
    /// ramming the target or passing it at `pass_reach` of the leader's
    /// reach: the enemies seen lately within reach of the way (armed ones
    /// within their reach, mobiles a little more; unarmed ones within the
    /// pack's) grouped into engagements along it, each a `fate_secs` pass
    /// (`FateMode::Pass`, on the line the group's statics lie off the way,
    /// the target's at the run's own) with the pack as the ones before
    /// should leave it (the dead dropped, the rest at their expected health).
    /// Worth = the expected value of the enemies killed, statics `lasting`
    /// times their metal, less our expected losses (sum of p_dead x cost on
    /// each side: units' deaths are not independent).
    fn pack_fates(&mut self, sid: SquadId, route: &[Vec3], ram: bool) -> Option<(Judgement, Vec<Value>)> {
        let model = self.pack_model()?;
        let target = *route.get(1)?;
        let s = self.squads.get(sid)?;
        let (horizon, lasting) = ((s.shape.fate_secs * 30.0) as i32, s.shape.lasting);
        let reach_share = s.shape.pass_reach;
        let ours: Vec<(i32, UnitDef, f32, Vec3)> = (s.everyone().filter_map(|id| self.fighters.get(&id)))
            .filter(|f| s.shape.is_core(f.unit.def().map(|d| d.id())) && f.unit.health() > 0.0)
            .filter_map(|f| f.unit.def().map(|d| (f.unit.id(), d, f.unit.health(), f.get_pos())))
            .collect();
        let frame = self.frame;
        let seen: Vec<(i32, UnitDef, f32, Vec3, bool)> = (self.war.targets.values())
            .filter(|e| !e.is_dead && e.identified && frame - e.last_seen <= JUDGE_SEEN)
            .filter_map(|e| e.ud.map(|d| (e.unit_id, d, e.get_health() * e.def_health(), e.position, e.can_fly())))
            .collect();
        let mut pack: Vec<(Arc<UnitSpec>, f32, Vec3, i32)> =
            ours.into_iter().map(|(id, d, hp, p)| (self.fight_spec(d), hp, p, id)).collect();
        let reach = pack.iter().flat_map(|u| u.0.weapons.iter().map(|w| w.range)).fold(0.0, f32::max);
        let pass_off = if ram { 0.0 } else { reach_share * reach };
        let mut j = Judgement { cores: pack.len(), ..Judgement::default() };
        // (id, spec, hp, where, along the way, off it)
        let mut near: Vec<(i32, Arc<UnitSpec>, f32, Vec3, f32, f32)> = Vec::new();
        for (eid, def, hp, p, flies) in seen {
            let spec = self.fight_spec(def);
            let (along, off) = route_project(route, p);
            let within = if spec.weapons.is_empty() {
                !flies && off <= reach
            } else {
                off <= spec.weapons.iter().map(|w| w.range).fold(0.0, f32::max)
                    + if spec.mobile { JUDGE_CHASE } else { 0.0 }
            };
            if within {
                near.push((eid, spec, hp, p, along, off));
            }
        }
        let along: Vec<f32> = near.iter().map(|n| n.4).collect();
        let mut detail = Vec::new();
        for group in engagements(&along, JUDGE_GAP) {
            if pack.is_empty() {
                break;
            }
            j.engagements += 1;
            let at = group.iter().map(|&i| along[i]).sum::<f32>() / group.len() as f32;
            let n = pack.len() as f32;
            let c = Vec3::xz(pack.iter().map(|u| u.2.x).sum::<f32>() / n, pack.iter().map(|u| u.2.z).sum::<f32>() / n);
            let meet = if at > JUDGE_MEET { route_point(route, at - JUDGE_MEET) } else { c };
            let (sx, sz) = (meet.x - c.x, meet.z - c.z);
            for u in &mut pack {
                (u.2.x, u.2.z) = (u.2.x + sx, u.2.z + sz);
            }
            // The run's own line at its target; elsewhere the way passes the
            // group's statics as far off as they lie.
            let at_target = group.iter().any(|&i| distance(near[i].3, target) < TARGET_LEFT);
            let statics: Vec<f32> = group.iter().filter(|&&i| !near[i].1.mobile).map(|&i| near[i].5).collect();
            let offset = if at_target {
                pass_off
            } else if statics.is_empty() {
                0.0
            } else {
                statics.iter().sum::<f32>() / statics.len() as f32
            };
            let mut snap = FightSnapshot::default();
            for (spec, hp, p, _) in &pack {
                snap.units.push(SnapUnit { side: 0, spec: spec.clone(), hp: *hp, x: p.x, z: p.z });
            }
            for &i in &group {
                let (_, spec, hp, p, _, _) = &near[i];
                snap.units.push(SnapUnit { side: 1, spec: spec.clone(), hp: *hp, x: p.x, z: p.z });
            }
            let fates = model.fates(&snap, horizon, FateMode::Pass { offset });
            let (ours_f, theirs_f) = fates.split_at(pack.len());
            let loss: f32 = pack.iter().zip(ours_f).map(|(u, f)| f.p_dead * u.0.cost).sum();
            let gain: f32 = (group.iter().zip(theirs_f))
                .map(|(&i, f)| f.p_dead * near[i].1.cost * if near[i].1.mobile { 1.0 } else { lasting })
                .sum();
            j.loss += loss;
            j.gain += gain;
            detail.push(json!({
                "i": j.engagements, "at": at.round(), "offset": offset.round(), "horizon": horizon,
                "ids_a": pack.iter().map(|u| u.3).collect::<Vec<_>>(),
                "ids_b": group.iter().map(|&i| near[i].0).collect::<Vec<_>>(),
                "p_dead_a": ours_f.iter().map(|f| (f.p_dead * 1000.0).round() / 1000.0).collect::<Vec<_>>(),
                "p_dead_b": theirs_f.iter().map(|f| (f.p_dead * 1000.0).round() / 1000.0).collect::<Vec<_>>(),
                // The sim's own death frame (from the query; -1: alive), for
                // checking its timing apart from who dies.
                "death_a": ours_f.iter().map(|f| f.death_frame.unwrap_or(-1)).collect::<Vec<_>>(),
                "death_b": theirs_f.iter().map(|f| f.death_frame.unwrap_or(-1)).collect::<Vec<_>>(),
                "metal_a": pack.iter().map(|u| u.0.cost).sum::<f32>().round(),
                "metal_b": group.iter().map(|&i| near[i].1.cost).sum::<f32>().round(),
                "loss": loss.round(), "gain": gain.round(),
            }));
            // The next engagement meets the survivors at their expected health.
            pack = (pack.into_iter().zip(ours_f))
                .filter(|(_, f)| !f.dead)
                .map(|((spec, _, p, id), f)| (spec, f.hp_left, p, id))
                .collect();
        }
        j.left = pack.len();
        Some((j, detail))
    }

    /// The fight predictor's model, loaded at the first ask (none if it
    /// would not load: packs then judge by threat).
    fn pack_model(&mut self) -> Option<Arc<Model>> {
        if self.taskforces.model.is_none() {
            let m = Model::load().map(Arc::new);
            if let Err(e) = &m {
                self.log(&format!("taskforce: packs cannot load the fight predictor ({e}); they judge by threat"));
            }
            self.taskforces.model = Some(m.ok());
        }
        self.taskforces.model.clone().flatten()
    }

    /// A judgement's record for the journal (`k=packjudge`): the verdict, the
    /// run's worth and each engagement's units and predictions (see
    /// `pack_judge`), for fitting the predictor to these decisions.
    fn pack_emit_judgement(&mut self, sid: SquadId, t: Vec3, j: &Judgement, detail: Vec<Value>, verdict: &str) {
        let (doctrine, judge) =
            self.squads.get(sid).map(|s| (s.shape.doctrine.clone(), s.shape.judge.name())).unwrap_or_default();
        self.op_emit(
            "packjudge",
            json!({
                "squad": sid, "doctrine": doctrine, "judge": judge, "verdict": verdict,
                "x": t.x.round(), "z": t.z.round(), "worth": j.worth().round(), "gain": j.gain.round(),
                "prize": j.prize.round(), "loss": j.loss.round(), "cores": j.cores, "left": j.left,
                "engagements": detail,
            }),
        );
    }

    fn pack_log_judgement(&mut self, sid: SquadId, t: Vec3, j: &Judgement, verdict: &str) {
        let doctrine = self.squads.get(sid).map(|s| s.shape.doctrine.clone()).unwrap_or_default();
        self.log(&format!(
            "taskforce: the {doctrine} pack (squad {sid}) {verdict} ({:.0}, {:.0}): worth {:.0} = kills {:.0} + \
             prize {:.0} - losses {:.0} over {} engagements, {} of {} cores left",
            t.x,
            t.z,
            j.worth(),
            j.gain,
            j.prize,
            j.loss,
            j.engagements,
            j.left,
            j.cores
        ));
    }

    /// An actual shot postpones discretionary individual repair, even if
    /// it misses. Reload polling covers engines which omit this event.
    pub fn pack_fired(&mut self, unit: Unit) {
        let Some(sid) = self.fighters.get(&unit.id()).and_then(|f| f.squad) else { return };
        if let Some(p) = self.taskforces.packs.get_mut(&sid) {
            p.fired(self.frame);
        }
    }

    /// The wounded unit's own complete route, sampled between waypoints.
    /// A pathfinder fallback towards an unreachable haven is not a safe way.
    fn pack_way_clear(&mut self, unit: Unit, home: Vec3) -> bool {
        let start = unit.pos();
        if start == NULLPOS || home == NULLPOS || self.incursion_imminent(home, (unit.power() + unit.health()) / 10.0) {
            return false;
        }
        let reachable = self.pathfinder.is_reachable(&self.war, unit, home, PathCost::AvoidEnemies);
        if !reachable {
            return false;
        }
        let path: Vec<_> =
            self.pathfinder.find_path(&self.war, unit, home, PathCost::AvoidEnemies).into_iter().collect();
        let view = self.war.view(unit.def());
        crate::military::fire_squad::repair_route_clear(start, home, &path, reachable, |p| {
            view.get_threat(p) <= 0.0 && view.get_porc_threat(p) <= 0.0
        })
    }

    /// The best nearby/current run's expected net metal per second,
    /// including travel and a pass. This is a rest opportunity cost, not
    /// a new attack gate. Without a model, use known target-cluster value.
    fn pack_carnage_rate(&mut self, sid: SquadId, leader: Unit, pos: Vec3, shape: &Shape, st: &Pack) -> f32 {
        let (threat, cap) = {
            let view = self.war.view(leader.def());
            let threat = view.get_friendly_threat(pos);
            (threat, shape.shun_porc.then_some(shape.porc_share * threat))
        };
        let predictor = shape.judge != Judge::Map;
        let shun: Vec<_> = st.shun.iter().filter(|s| s.1 > self.frame).map(|s| s.0).collect();
        let mut targets = Vec::new();
        if st.target != NULLPOS && self.pack_target_left(st.target).is_some() {
            targets.push(st.target);
        }
        if let Some((target, _)) = self.pack_sweep(leader, pos, cap, threat, &shun, shape, predictor)
            && !targets.iter().any(|&t| distance(t, target) < TARGET_LEFT)
        {
            targets.push(target);
        }
        if targets.is_empty() {
            let (target, _) = self.war_best_target_shunning(leader, pos, false, cap, &shun);
            if target != NULLPOS {
                targets.push(target);
            }
        }
        let mut best: f32 = 0.0;
        for target in targets {
            let route = pack_route(pos, target);
            let worth = if shape.judge == Judge::Fates {
                attack_modes(shape.attack)
                    .iter()
                    .copied()
                    .filter_map(|ram| self.pack_fates(sid, &route, ram))
                    .map(|(j, _)| j.worth())
                    .max_by(f32::total_cmp)
            } else {
                self.pack_judge(sid, &route, shape.judge == Judge::Mech).map(|(j, _)| j.worth())
            }
            .unwrap_or_else(|| {
                self.war
                    .targets
                    .values()
                    .filter(|e| {
                        !e.is_dead && e.identified && !e.can_fly() && distance(e.position, target) < TARGET_LEFT
                    })
                    .map(|e| e.value * if e.is_static { shape.lasting } else { 1.0 })
                    .sum()
            });
            let travel: f32 = route.windows(2).map(|p| distance(p[0], p[1])).sum();
            let seconds = travel / shape.pace.max(1.0) + shape.fate_secs;
            best = best.max(worth.max(0.0) / seconds.max(1.0));
        }
        best
    }

    /// The pack's guns for a pass: each core's place in the formation (the
    /// leader's (0, 0)) and `share` of its reach.
    fn pack_guns(&mut self, sid: SquadId, share: f32) -> Vec<((f32, f32), f32)> {
        let Some(s) = self.squads.get_mut(sid) else { return Vec::new() };
        let reach = |u: Unit| share * u.def().map_or(0.0, |d| d.max_weapon_range());
        let mut guns: Vec<_> = s.get_leader(&self.fighters).map(|l| ((0.0, 0.0), reach(l))).into_iter().collect();
        for (id, place) in s.formation(&self.fighters) {
            if let Some(f) = self.fighters.get(&id).filter(|f| s.shape.is_core(f.unit.def().map(|d| d.id()))) {
                guns.push((place, reach(f.unit)));
            }
        }
        guns
    }

    /// Where a pack gathers: the haven nearest the rally point (where
    /// forming forces were picked off), else the one nearest it.
    fn pack_gather(&mut self, pos: Vec3) -> Vec3 {
        let rally = self.war_get_rally_point(pos);
        self.graph_get_closest_haven(if rally == NULLPOS { pos } else { rally })
    }

    /// Sliding formations give every core a translated copy of the same
    /// turn. Allow for the least agile core and for members catching up.
    fn pack_turn_distance(&self, sid: SquadId) -> f32 {
        let Some(s) = self.squads.get(sid) else { return TURN };
        s.everyone()
            .filter_map(|id| self.fighters.get(&id))
            .filter(|f| s.shape.is_core(f.unit.def().map(|d| d.id())))
            .map(|f| {
                let Some(def) = f.unit.def() else { return TURN };
                if !matches!(self.defs.name(def), "vehassault" | "tankassault") {
                    return TURN;
                }
                let vel = f.unit.vel();
                let speed = s.shape.pace.max((vel.x * vel.x + vel.z * vel.z).sqrt() * 30.0);
                turn_distance(speed, def.turn_rate(), def.turn_in_place_speed_limit() * 30.0)
            })
            .reduce(f32::max)
            .unwrap_or(TURN)
    }

    /// Send pack `sid` to `dest`, replacing any previous attack orders
    /// even when already nearby (`assault`: attack path, else avoid enemies).
    fn pack_go(&mut self, sid: SquadId, pos: Vec3, head: (f32, f32), dest: Vec3, assault: bool) {
        let way = self.pack_way(sid, pos, head, dest, assault);
        self.pack_drive(sid, &way);
    }

    /// Path waypoints [`SPACING`] apart. Only attacks use turning arcs;
    /// retreating packs may reverse immediately.
    fn pack_way(&mut self, sid: SquadId, pos: Vec3, head: (f32, f32), dest: Vec3, assault: bool) -> Vec<Vec3> {
        let turn = if assault { self.pack_turn_distance(sid) } else { TURN };
        if distance(pos, dest) <= STAND_NEAR {
            return approach_way(pos, head, dest, assault, turn, []);
        }
        let Some(leader) = self.squads.get(sid).and_then(|s| s.get_leader(&self.fighters)) else { return Vec::new() };
        let cost = if assault { PathCost::AssaultPath } else { PathCost::AvoidEnemies };
        let path = self.pathfinder.find_path(&self.war, leader, dest, cost);
        approach_way(pos, head, dest, assault, turn, path)
    }

    fn pack_drive(&mut self, sid: SquadId, way: &[Vec3]) {
        let mut nav = nav!(self);
        if let Some(s) = self.squads.get_mut(sid) {
            s.drive(way, &mut self.fighters, &mut nav);
        }
    }

    /// Send pack `sid`'s screen to its place: [`Keep::Trail`]'s `orbit`
    /// behind the leader on the way to `home`, under fight orders (Rippers
    /// shoot what they meet); `hold`: where they are.
    fn pack_screen(&mut self, sid: SquadId, home: Vec3, hold: bool) {
        let Some(s) = self.squads.get(sid) else { return };
        let Some(lpos) = s.get_leader(&self.fighters).map(|l| l.pos()) else { return };
        let ids = s.trailers(&self.fighters);
        let shape = s.shape.clone();
        let mut nav = nav!(self);
        for id in ids {
            let Some(f) = self.fighters.get_mut(&id) else { continue };
            let pos = f.get_pos();
            let place = if hold {
                pos
            } else {
                let behind = shape.orbit(f.unit.def().map(|d| d.id()));
                if distance(lpos, home) > behind { get_directional_point(lpos, home, behind) } else { home }
            };
            if f.target != NULLPOS && distance(f.target, place) < SCREEN_REPATH {
                continue;
            }
            f.target = place;
            if hold {
                f.unit.fight(place, 0);
            } else {
                f.fight_to(place, &mut nav);
            }
        }
    }

    /// The metal of enemy raiders seen lately near `pos`.
    fn pack_raiders(&self, pos: Vec3) -> f32 {
        (self.war.targets.values())
            .filter(|e| e.identified && e.is_raider && !e.is_dead && !e.can_fly())
            .filter(|e| self.frame - e.last_seen <= RIP_SEEN && distance(e.position, pos) < RIP_REACH)
            .map(|e| e.value)
            .sum()
    }

    /// The pack's local sweep: of the enemy units we know and the enemy metal
    /// spots within `sweep` of it (not flying, not shunned, under no more
    /// porc than `porc_cap`, and, unless the predictor judges, under no more
    /// threat less ours than the pack's own), the one with the most enemy
    /// value standing within [`SWEEP_CLUSTER`] of it (statics `lasting`
    /// times their metal, a spot whose mex we have not seen at a mex's)
    /// per elmo of the way there (plus [`SWEEP_PAD`]); none worth
    /// `sweep_min`: none. With the value near it.
    #[allow(clippy::too_many_arguments)]
    fn pack_sweep(
        &self,
        leader: Unit,
        pos: Vec3,
        porc_cap: Option<f32>,
        fthreat: f32,
        shun: &[Vec3],
        shape: &Shape,
        predictor: bool,
    ) -> Option<(Vec3, f32)> {
        if shape.sweep <= 0.0 {
            return None;
        }
        let view = self.war.view(leader.def());
        let value = |e: &crate::military::enemy::Enemy| e.value * if e.is_static { shape.lasting } else { 1.0 };
        let known: Vec<(Vec3, f32)> = (self.war.targets.values())
            .filter(|e| !e.is_dead && e.identified && !e.can_fly())
            .filter(|e| distance(e.position, pos) < shape.sweep + SWEEP_CLUSTER)
            .map(|e| (e.position, value(e)))
            .collect();
        let mex = self.defs.get("staticmex").map_or(0.0, |d| d.cost(self.metal)) * shape.lasting;
        let spots: Vec<(Vec3, f32)> = (self.graph.get_enemy_spots().iter())
            .map(|m| m.get_pos())
            .filter(|&p| distance(p, pos) < shape.sweep && !known.iter().any(|k| distance(k.0, p) < 50.0))
            .map(|p| (p, mex))
            .collect();
        let candidates: Vec<_> = known.into_iter().chain(spots).collect();
        sweep_target(pos, shape.sweep, shape.sweep_min, &candidates, |c| {
            !shun.iter().any(|&s| distance(s, c) < 300.0)
                && porc_cap.is_none_or(|cap| view.get_porc_threat(c) <= cap)
                && (predictor || view.get_effective_threat(c) <= fthreat)
        })
    }

    /// What of the enemy's is left at `t`, and whether it moves: an enemy
    /// metal spot there, else the nearest unit we know of near it (the
    /// target moves with it).
    fn pack_target_left(&self, t: Vec3) -> Option<(Vec3, bool)> {
        if self.graph.get_enemy_spots().iter().any(|m| distance(m.get_pos(), t) < 100.0) {
            return Some((t, false));
        }
        (self.war.targets.values())
            .filter(|e| !e.is_dead && !e.can_fly())
            .map(|e| (distance(e.position, t), e.position, !e.is_static))
            .filter(|&(d, _, _)| d < TARGET_LEFT)
            .min_by(|a, b| a.0.total_cmp(&b.0))
            .map(|(_, p, mobile)| (p, mobile))
    }

    /// A pack neither out nor parked is led from where most of it stands:
    /// by the core with the most others within [`BULK`], once it has
    /// [`BULK_MORE`] more than the leader (a stray core first in line, as
    /// after a save's load, would gather the pack round itself).
    fn pack_lead(&mut self, sid: SquadId) {
        let parked = self.taskforces.packs.get(&sid).is_some_and(|p| p.parked);
        let Some(s) = self.squads.get(sid).filter(|s| !s.engaging && !parked) else { return };
        let cores: Vec<(i32, Vec3)> = (s.everyone().filter_map(|id| self.fighters.get(&id)))
            .filter(|f| s.shape.is_core(f.unit.def().map(|d| d.id())))
            .map(|f| (f.id, f.get_pos()))
            .collect();
        let near = |p: Vec3| cores.iter().filter(|c| distance(c.1, p) < BULK).count();
        let Some(&(best, at)) = cores.iter().max_by_key(|c| near(c.1)) else { return };
        let led = s.leader.and_then(|l| cores.iter().find(|c| c.0 == l)).map_or(0, |c| near(c.1));
        if near(at) < led + BULK_MORE {
            return;
        }
        let doctrine = s.shape.doctrine.clone();
        self.log(&format!(
            "taskforce: the {doctrine} pack (squad {sid}) is led from where {} of its {} cores stand ({led} near \
             the old leader)",
            near(at),
            cores.len()
        ));
        if let Some(s) = self.squads.get_mut(sid) {
            s.take_lead(best, &mut self.fighters);
        }
    }

    /// A pack neither out nor parked takes in the smaller ones of its
    /// doctrine that are not either (ties: the newer goes).
    fn pack_merge(&mut self, sid: SquadId) {
        let parked = |me: &Ai, q: SquadId| me.taskforces.packs.get(&q).is_some_and(|p| p.parked);
        let Some(s) = self.squads.get(sid) else { return };
        if s.engaging || parked(self, sid) {
            return;
        }
        let doctrine = s.shape.doctrine.clone();
        let mine = s.cores(&self.fighters);
        let smaller: Vec<SquadId> = (self.squad_handler.taskforces.iter().copied())
            .filter(|&q| q != sid && !parked(self, q))
            .filter(|&q| {
                self.squads.get(q).is_some_and(|o| {
                    let n = o.cores(&self.fighters);
                    o.shape.doctrine == doctrine && !o.engaging && (n < mine || (n == mine && q > sid))
                })
            })
            .collect();
        for q in smaller {
            let Some(o) = self.squads.get_mut(q) else { continue };
            let members = o.disband(&mut self.fighters);
            self.squad_handler.taskforces.retain(|&x| x != q);
            if let Some(other) = self.taskforces.packs.remove(&q) {
                self.taskforces.packs.entry(sid).or_default().absorb_firing(other);
            }
            self.squad_delete_squad(q);
            self.log(&format!(
                "taskforce: the {doctrine} pack (squad {sid}) takes in squad {q} ({} members)",
                members.len()
            ));
            for id in members {
                self.squad_log_join(sid, id, "merges into");
                let mut nav = nav!(self);
                if let Some(s) = self.squads.get_mut(sid) {
                    s.add_unit_nav(id, &mut self.fighters, &mut nav);
                }
            }
        }
    }
}

/// Value a destination by the whole nearby cluster, then divide by travel.
fn sweep_target(
    pos: Vec3,
    radius: f32,
    minimum: f32,
    candidates: &[(Vec3, f32)],
    allowed: impl Fn(Vec3) -> bool,
) -> Option<(Vec3, f32)> {
    candidates
        .iter()
        .map(|c| c.0)
        .filter(|&c| distance(c, pos) < radius && allowed(c))
        .map(|c| (c, candidates.iter().filter(|k| distance(k.0, c) < SWEEP_CLUSTER).map(|k| k.1).sum::<f32>()))
        .filter(|&(_, value)| value >= minimum)
        .max_by(|a, b| (a.1 / (distance(a.0, pos) + SWEEP_PAD)).total_cmp(&(b.1 / (distance(b.0, pos) + SWEEP_PAD))))
}

#[cfg(test)]
mod tests {
    #[test]
    fn cruise_turns_keep_a_margin_for_engine_braking_and_unknown_data() {
        // Formation-tested margins at each doctrine's cruise speed.
        assert_eq!(turn_distance(80.0, 688.0, 88.5), 210.0);
        assert_eq!(turn_distance(66.0, 583.0, 44.1), 220.0);
        // A faster catch-up member or terrain boost must not tighten a turn.
        assert!(turn_distance(88.5, 688.0, 88.5) >= turn_distance(80.0, 688.0, 88.5));
        assert_eq!(turn_distance(120.0, 583.0, 44.1), TURN);
        assert_eq!(turn_distance(80.0, 0.0, 0.0), TURN);
        assert_eq!(turn_distance(f32::NAN, 688.0, 88.5), TURN);
    }

    #[test]
    fn tighter_attack_turns_leave_retreats_free_to_reverse() {
        let pos = Vec3::ZERO;
        let dest = Vec3::xz(-1000.0, 100.0);
        let turn = turn_distance(80.0, 688.0, 88.5);
        let point = turn_point(pos, (1.0, 0.0), dest, turn).unwrap();
        assert_eq!(point, Vec3::xz(0.0, 210.0));
        let retreat = approach_way(pos, (1.0, 0.0), dest, false, turn, [dest]);
        assert_eq!(retreat, vec![dest]);
    }

    #[test]
    fn explicit_pass_cannot_ram_even_a_mobile_target_or_a_predictor_favorite() {
        assert_eq!(attack_modes(Attack::Pass), &[false]);
        assert_eq!(attack_modes(Attack::Charge), &[true]);
        assert_eq!(attack_modes(Attack::Mixed), &[true, false]);
        for mobile in [false, true] {
            for predicted in [None, Some(false), Some(true)] {
                assert!(!charges(Attack::Pass, mobile, predicted));
                assert!(charges(Attack::Charge, mobile, predicted));
            }
        }
        assert!(charges(Attack::Mixed, true, None));
        assert!(!charges(Attack::Mixed, false, None));
        assert!(charges(Attack::Mixed, false, Some(true)));
    }

    #[test]
    fn a_tangential_minotaur_pair_stays_outside_the_target_instead_of_ramming() {
        let pos = Vec3::ZERO;
        let target = Vec3::xz(1000.0, 0.0);
        let guns = [((0.0, 0.0), 252.0), ((0.0, 75.0), 252.0)];
        let (pass, n) = plan_tangent_pass(pos, (1.0, 0.0), target, &guns, 70.0, |_| true).unwrap();
        assert_eq!(n, 2);
        for &((_, z), reach) in &guns {
            assert!((pass.side + z).abs() >= 170.0);
            assert!((pass.side + z).abs() <= reach);
        }
        // If only a head-on line is clear, abandon it rather than ram.
        assert!(plan_tangent_pass(pos, (1.0, 0.0), target, &guns, 70.0, |p| p.side.abs() < 5.0).is_none());
        // Turn away from a blocked side, keeping both guns outside the body.
        let (other, n) = plan_tangent_pass(pos, (1.0, 0.0), target, &guns, 70.0, |p| p.side < 0.0).unwrap();
        assert_eq!(n, 2);
        assert!(other.side < -170.0);
    }

    #[test]
    fn health_retreat_interrupts_any_part_of_a_run_even_while_firing() {
        let target = Vec3::xz(1000.0, 0.0);
        for x in [100.0, 1000.0, 1100.0, 1200.0] {
            let mut pack = Pack {
                target,
                pass_target: target,
                pass: Some(Pass { way: (1.0, 0.0), side: 0.0 }),
                to_screen: true,
                back_until: 1000,
                ..Pack::default()
            };
            pack.fired(100);
            assert!(!pack.quiet(100, 5.0));
            pack.finish_pass(Vec3::xz(x, 0.0), true);
            assert!(pack.retreat_for_health(0.2, 0.5));
            assert!(pack.parked && !pack.opportunistic_rest);
            assert_eq!(pack.target, NULLPOS);
            assert!(pack.pass.is_none() && !pack.to_screen);
            assert_eq!(pack.back_until, 0);
            // Forced recovery cannot be converted back to an early-ending rest.
            assert!(!pack.retreat_for_health(0.2, 0.5));
            assert!(pack.parked && !pack.opportunistic_rest);
        }
    }

    #[test]
    fn low_health_promotes_discretionary_rest_to_forced_recovery() {
        let mut pack = Pack { parked: true, opportunistic_rest: true, ..Pack::default() };
        assert!(!pack.retreat_for_health(0.6, 0.5));
        assert!(pack.opportunistic_rest);
        assert!(pack.retreat_for_health(0.4, 0.5));
        assert!(pack.parked && !pack.opportunistic_rest);
    }

    #[test]
    fn retreat_reverses_directly_while_attack_can_still_turn_in_an_arc() {
        let pos = Vec3::ZERO;
        let home = Vec3::xz(-1000.0, 0.0);
        let path = [Vec3::ZERO, Vec3::xz(-400.0, 0.0), Vec3::xz(-800.0, 0.0), home];
        let retreat = approach_way(pos, (1.0, 0.0), home, false, TURN, path);
        assert_eq!(retreat[0], Vec3::xz(-400.0, 0.0));
        assert!(retreat.iter().all(|p| p.x < 0.0 && p.z == 0.0));
        let attack = approach_way(pos, (1.0, 0.0), home, true, TURN, path);
        assert_eq!(attack[0].x, 0.0);
        assert_eq!(attack[0].z.abs(), TURN);
    }

    #[test]
    fn retreat_near_a_haven_still_replaces_queued_attack_orders() {
        let home = Vec3::xz(-100.0, 0.0);
        assert_eq!(approach_way(Vec3::ZERO, (1.0, 0.0), home, false, TURN, []), vec![home]);
    }

    #[test]
    fn casualties_and_damage_both_reduce_original_strength() {
        let mut pack = Pack::default();
        assert_eq!(pack.strength(5, 5.0), 1.0);
        // Two healthy survivors are 40% of the original five, not 100%.
        assert_eq!(pack.strength(2, 2.0), 0.4);
        assert_eq!(pack.strength(2, 1.0), 0.2);
        // Selecting another target must not forget the casualties.
        pack.target = Vec3::xz(2000.0, 0.0);
        assert_eq!(pack.strength(2, 2.0), 0.4);
        // Replacements restore strength; a larger pack expands the baseline.
        assert_eq!(pack.strength(5, 5.0), 1.0);
        assert_eq!(pack.strength(6, 3.0), 0.5);
    }

    #[test]
    fn a_single_minotaur_keeps_its_actual_health_fraction() {
        let mut pack = Pack::default();
        assert_eq!(pack.strength(1, 1.0), 1.0);
        assert_eq!(pack.strength(1, 0.2), 0.2);
        assert_eq!(pack.strength(0, 0.0), 0.0);
    }

    #[test]
    fn recovery_allows_survivors_to_start_a_fresh_outing() {
        let mut pack = Pack::default();
        pack.strength(5, 5.0);
        pack.parked = true;
        // Healthy survivors still have to finish retreating, rather than
        // forgetting the casualties on the next conduct in enemy territory.
        assert!(!pack.recover(2, 1.0, 0.9, false));
        assert_eq!(pack.strength(2, 2.0), 0.4);
        assert!(!pack.recover(2, 0.8, 0.9, true));
        assert!(pack.parked);
        assert!(pack.recover(2, 0.9, 0.9, true));
        assert!(!pack.parked);
        assert_eq!(pack.strength(2, 2.0), 1.0);
        assert_eq!(pack.strength(1, 0.8), 0.4);
    }

    #[test]
    fn passes_end_150_beyond_the_locked_point_even_if_the_enemy_moves() {
        let target = Vec3::xz(1000.0, 0.0);
        let mut pack =
            Pack { target, pass_target: target, pass: Some(Pass { way: (1.0, 0.0), side: 0.0 }), ..Pack::default() };
        pack.finish_pass(Vec3::xz(1149.0, 0.0), true);
        assert!(pack.pass.is_some());
        // The enemy moves forward. The locked pass and its finish do not.
        pack.target = Vec3::xz(1300.0, 0.0);
        pack.finish_pass(Vec3::xz(1150.0, 0.0), true);
        assert!(pack.pass.is_none());
        assert_eq!(pack.passes, 1);
        // Further conducts cannot complete the same pass twice.
        pack.finish_pass(Vec3::xz(1151.0, 0.0), true);
        assert_eq!(pack.passes, 1);
    }

    #[test]
    fn a_lost_target_ends_commitment_before_another_run() {
        let target = Vec3::xz(1000.0, 0.0);
        let mut pack =
            Pack { target, pass_target: target, pass: Some(Pass { way: (1.0, 0.0), side: 0.0 }), ..Pack::default() };
        pack.finish_pass(Vec3::xz(500.0, 0.0), false);
        assert!(pack.pass.is_none());
        assert_eq!(pack.target, NULLPOS);
        assert_eq!(pack.passes, 0);
    }

    #[test]
    fn a_missed_shot_still_requires_five_seconds_of_quiet() {
        let mut pack = Pack::default();
        assert!(pack.quiet(100, 5.0));
        // A loaded, already reloaded gun is not a new shot.
        pack.observe_reload((1, 0), 60, 100);
        assert!(pack.quiet(100, 5.0));
        // Advancing reload catches a shot even without a damage/fired event.
        pack.observe_reload((1, 0), 180, 120);
        assert!(!pack.quiet(269, 5.0));
        assert!(pack.quiet(270, 5.0));
        // Finishing that reload does not keep resetting the clock.
        pack.observe_reload((1, 0), 180, 200);
        assert!(pack.quiet(270, 5.0));
        // An actual shot resets it even if it misses and reload is unobserved.
        pack.fired(280);
        assert!(!pack.quiet(429, 5.0));
        assert!(pack.quiet(430, 5.0));
    }

    #[test]
    fn new_and_merged_members_preserve_the_firing_cooldown() {
        let mut pack = Pack::default();
        pack.observe_reload((2, 0), 240, 200);
        assert!(!pack.quiet(349, 5.0));
        let mut merged = Pack::default();
        merged.fired(300);
        merged.observe_reload((3, 0), 330, 300);
        pack.absorb_firing(merged);
        // Both guns may be ready; the merged pack still fired recently.
        pack.observe_reload((3, 0), 330, 400);
        assert!(!pack.quiet(449, 5.0));
        assert!(pack.quiet(450, 5.0));
    }

    #[test]
    fn haven_rest_competes_with_carnage_and_can_end_early() {
        let rest = repair_rate(400.0, 400.0, 20.0, 0.0, 80.0);
        assert_eq!(rest, 20.0);
        assert!(rest_worthwhile(rest, 0.0, false));
        assert!(rest_worthwhile(rest, 10.0, false));
        assert!(!rest_worthwhile(rest, 30.0, false));
        // Small changes do not toggle resting and raiding every conduct.
        assert!(!rest_worthwhile(rest, 18.0, false));
        assert!(rest_worthwhile(rest, 18.0, true));
        assert!(!rest_worthwhile(rest, 30.0, true));
        assert!(!rest_worthwhile(0.0, 0.0, true));
        assert_eq!(repair_rate(400.0, 400.0, 0.0, 100.0, 80.0), 0.0);
        assert!(repair_rate(400.0, 400.0, 20.0, 400.0, 80.0) < rest);
    }

    #[test]
    fn sweep_cleans_nearby_clusters_before_distant_value() {
        use super::sweep_target;
        let candidates = [(Vec3::xz(300.0, 0.0), 90.0), (Vec3::xz(320.0, 0.0), 90.0), (Vec3::xz(1400.0, 0.0), 400.0)];
        let (target, value) = sweep_target(Vec3::ZERO, 1500.0, 100.0, &candidates, |_| true).unwrap();
        assert!(target.x < 400.0);
        assert_eq!(value, 180.0);
        // The nearer cluster can still be rejected by threat or shunning.
        assert_eq!(
            sweep_target(Vec3::ZERO, 1500.0, 100.0, &candidates, |p| p.x > 400.0),
            Some((Vec3::xz(1400.0, 0.0), 400.0))
        );
        assert!(sweep_target(Vec3::ZERO, 1500.0, 500.0, &candidates, |_| true).is_none());
        assert!(sweep_target(Vec3::ZERO, 0.0, 100.0, &candidates, |_| true).is_none());
    }

    use super::*;

    fn close(a: Vec3, b: Vec3) -> bool {
        distance(a, b) < 0.5
    }

    #[test]
    fn passes_bring_the_most_guns_in() {
        let o = Vec3::xz(0.0, 0.0);
        let t = Vec3::xz(1000.0, 0.0);
        // A line abreast across the way (+z is left of +x): 5 guns 60
        // apart, reach 200. The leader's line can be at most 80 right of
        // the target (-80) to have the gun at +120 in, at most 80 left to
        // have the one at -120; heading straight at it, it runs over it.
        let abreast: Vec<((f32, f32), f32)> = [-120.0, -60.0, 0.0, 60.0, 120.0].map(|z| ((0.0, z), 200.0)).to_vec();
        let (p, n) = plan_pass(o, (1.0, 0.0), t, &abreast, |_| true).unwrap();
        assert_eq!((n, p.way), (5, (1.0, 0.0)));
        assert!(p.side.abs() <= 10.0, "{}", p.side);
        // Heading so as to pass well to the left: the line nearest that
        // still bringing all five in.
        let (p, n) = plan_pass(o, (0.95, 0.312), t, &abreast, |_| true).unwrap();
        assert!(n == 5 && (p.side - 80.0).abs() <= 10.0, "{} {n}", p.side);
        // Too wide for all (480 across, a window of 400): the most it can,
        // four.
        let wide: Vec<((f32, f32), f32)> = [-240.0, -120.0, 0.0, 120.0, 240.0].map(|z| ((0.0, z), 200.0)).to_vec();
        assert_eq!(plan_pass(o, (1.0, 0.0), t, &wide, |_| true).unwrap().1, 4);
        // A column along the way passes as one gun: thickness costs none.
        let column: Vec<((f32, f32), f32)> = [0.0, 60.0, 120.0, -60.0].map(|x| ((x, 0.0), 200.0)).to_vec();
        let (p, n) = plan_pass(o, (0.0, 1.0), t, &column, |_| true).unwrap();
        assert_eq!(n, 4);
        assert!(p.side.abs() <= 200.0);
        // Porc to the left of the target: the line runs right of it, as
        // close as clear lets it; with no clear line, none.
        let (p, n) = plan_pass(o, (1.0, 0.0), t, &abreast, |p| p.side <= -40.0).unwrap();
        assert!(n == 5 && (p.side + 40.0).abs() < 1.0, "{} {n}", p.side);
        assert!(plan_pass(o, (1.0, 0.0), t, &abreast, |_| false).is_none());
    }

    #[test]
    fn a_charge_runs_over_the_target() {
        let o = Vec3::xz(0.0, 0.0);
        let t = Vec3::xz(1000.0, 300.0);
        let guns: Vec<((f32, f32), f32)> = [-60.0, 0.0, 60.0].map(|z| ((0.0, z), 200.0)).to_vec();
        // Heading well off: a pass keeps near the heading's line, a charge
        // takes the line over the target all the same.
        let (p, _) = plan_pass(o, (1.0, 0.0), t, &guns, |_| true).unwrap();
        assert!(p.side.abs() > 50.0, "{}", p.side);
        let (c, n) = plan_pass(o, (1.0, 0.0), t, &guns, |p| p.side.abs() <= PASS_STEP / 2.0).unwrap();
        assert!(c.side.abs() <= PASS_STEP / 2.0 && n == 3);
        let [a, _] = pass_way(o, t, &c);
        assert!(distance(a, t) <= PASS_STEP / 2.0);
    }

    #[test]
    fn routes_measure_along_and_off() {
        let route = [Vec3::xz(0.0, 0.0), Vec3::xz(1000.0, 0.0), Vec3::xz(1000.0, 500.0)];
        let (s, off) = route_project(&route, Vec3::xz(400.0, 100.0));
        assert!((s - 400.0).abs() < 0.5 && (off - 100.0).abs() < 0.5);
        // On the second leg: 1000 along the first, then 300 up it.
        let (s, off) = route_project(&route, Vec3::xz(1200.0, 300.0));
        assert!((s - 1300.0).abs() < 0.5 && (off - 200.0).abs() < 0.5);
        assert!(close(route_point(&route, 1250.0), Vec3::xz(1000.0, 250.0)));
        assert!(close(route_point(&route, 5000.0), Vec3::xz(1000.0, 500.0)));
    }

    #[test]
    fn engagements_split_at_gaps() {
        let along = [900.0, 100.0, 300.0, 2000.0, 1300.0];
        assert_eq!(engagements(&along, 600.0), vec![vec![1, 2, 0, 4], vec![3]]);
        assert_eq!(engagements(&along, 300.0), vec![vec![1, 2], vec![0], vec![4], vec![3]]);
        assert!(engagements(&[], 600.0).is_empty());
    }

    #[test]
    fn pass_waypoints_run_on_past_the_target() {
        let t = Vec3::xz(1000.0, 0.0);
        let pass = Pass { way: (1.0, 0.0), side: 100.0 };
        // Before the pass point: there, then on.
        let [a, b] = pass_way(Vec3::xz(0.0, 0.0), t, &pass);
        assert!(close(a, Vec3::xz(1000.0, 100.0)) && close(b, Vec3::xz(1400.0, 100.0)));
        // Past it: always ahead along the line.
        let [a, _] = pass_way(Vec3::xz(1100.0, 100.0), t, &pass);
        assert!(close(a, Vec3::xz(1250.0, 100.0)));
        assert!(pass.along(Vec3::xz(1400.0, 0.0), t) > PASS_OUT);
    }

    #[test]
    fn turning_back_starts_with_an_arc() {
        let o = Vec3::xz(0.0, 0.0);
        // Ahead, or near: no arc.
        assert!(turn_point(o, (1.0, 0.0), Vec3::xz(500.0, 400.0), TURN).is_none());
        assert!(turn_point(o, (1.0, 0.0), Vec3::xz(-100.0, 0.0), TURN).is_none());
        // Behind and to the left: a right angle to the left first.
        let p = turn_point(o, (1.0, 0.0), Vec3::xz(-800.0, 100.0), TURN).unwrap();
        assert!(close(p, Vec3::xz(0.0, TURN)));
        let p = turn_point(o, (1.0, 0.0), Vec3::xz(-800.0, -100.0), TURN).unwrap();
        assert!(close(p, Vec3::xz(0.0, -TURN)));
    }

    #[test]
    fn paths_thin_to_spaced_points() {
        let path: Vec<Vec3> = (1..=20).map(|i| Vec3::xz(i as f32 * 50.0, 0.0)).collect();
        let pts = spaced(path.clone(), Vec3::xz(0.0, 0.0), 2, 250.0);
        assert_eq!(pts.len(), 2);
        assert!(close(pts[0], Vec3::xz(250.0, 0.0)) && close(pts[1], Vec3::xz(500.0, 0.0)));
        // A short path: its end.
        let pts = spaced(path[..2].to_vec(), Vec3::xz(0.0, 0.0), 2, 250.0);
        assert!(pts.len() == 1 && close(pts[0], Vec3::xz(100.0, 0.0)));
    }

    #[test]
    fn heading_falls_back_on_the_target() {
        let h = heading(Vec3::xz(0.0, 0.0), Vec3::xz(0.0, 0.0), Vec3::xz(0.0, 10.0));
        assert!((h.0 - 0.0).abs() < 1e-6 && (h.1 - 1.0).abs() < 1e-6);
        let h = heading(Vec3::xz(3.0, 4.0), Vec3::xz(0.0, 0.0), NULLPOS);
        assert!((h.0 - 0.6).abs() < 1e-6 && (h.1 - 0.8).abs() < 1e-6);
    }
}
