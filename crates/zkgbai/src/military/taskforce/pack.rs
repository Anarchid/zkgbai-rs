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
//! starts with an arc. With `attack` charge (or mixed, for what moves) it
//! runs at the target head-on instead, aimed anew each conduct until
//! [`CHARGE_LOCK`] off, then straight through: ZK units push mobiles out of
//! their way, and what dodges a tangent cannot sidestep a ram. With
//! `commit`, a run begun is finished through its target before threat,
//! raiders or its health are judged again (runs are for doing damage; they
//! expect losses). Enemy porc is weighed against the pack: it takes on
//! targets, run lines and ground under porc threat up to `porc_share` of its
//! own (`shun_porc` off: porc is not looked at). Repositioning (neither
//! taking nor dealing damage for `quiet` seconds) with a way home clear of
//! threat, its members below `repair_below` health go for repairs alone, to
//! rejoin when healed. With `judge`
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
//! them; the screen holds meanwhile); below `retreat_health`, to a haven,
//! where it parks for repairs until its cores average `heal_to`. A screen
//! member below `retreat_health` goes for repairs alone. Packs neither out
//! nor parked merge, the smaller into the larger, are led from where most
//! of their cores stand, and gather at the haven nearest the rally point,
//! not at it.
//!
//! Log lines `taskforce: the ravager pack (squad N) ...`: dives (target),
//! passes or charges (the how manyth, guns within reach of how many), falls
//! back to its screen, parks / is repaired, takes in (merges), is led from
//! its bulk; engages and break-offs as other forces'.

use std::sync::Arc;

use springai::{Unit, UnitDef, Vec3};

use super::{Attack, Judge};
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
const PASS_OUT: f32 = 300.0;
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
/// A turn back of more than a right angle starts this far along an arc.
const TURN: f32 = 250.0;
/// A screen member's place must move this far before it is sent anew.
const SCREEN_REPATH: f32 = 250.0;
/// Outweighed, a pack keeps going home this many frames (10 s) before it
/// judges again, so it does not turn on the threat's edge.
const BACK_FOR: i32 = 300;

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
    pub passes: u32,
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
    /// When a member last took damage, and last dealt it (frames).
    pub last_hurt: i32,
    pub last_hit: i32,
}

impl Default for Pack {
    fn default() -> Self {
        Pack {
            target: NULLPOS,
            since: 0,
            pass: None,
            passes: 0,
            mobile: false,
            ram: None,
            shun: Vec::new(),
            parked: false,
            back_until: 0,
            to_screen: false,
            last_hurt: 0,
            last_hit: 0,
        }
    }
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
    let (dx, dz) = (target.x - pos.x, target.z - pos.z);
    let d = (dx * dx + dz * dz).sqrt();
    let way = if d > 1.0 { (dx / d, dz / d) } else { heading };
    let left = (-way.1, way.0);
    // Where the heading's line runs by the target, across the way.
    let ahead = (dx * heading.0 + dz * heading.1).max(0.0);
    let natural = (pos.x + heading.0 * ahead - target.x) * left.0 + (pos.z + heading.1 * ahead - target.z) * left.1;
    let across: Vec<(f32, f32)> = guns.iter().map(|&((ox, oz), reach)| (ox * left.0 + oz * left.1, reach)).collect();
    let span = across.iter().map(|&(p, reach)| p.abs() + reach).fold(0.0, f32::max);
    let mut best: Option<(usize, f32, Pass)> = None;
    let mut side = -span;
    while side <= span {
        let pass = Pass { way, side };
        let n = across.iter().filter(|&&(p, reach)| (side + p).abs() <= reach).count();
        let off = (side - natural).abs();
        if best.is_none_or(|(m, o, _)| n > m || (n == m && off < o)) && clear(&pass) {
            best = Some((n, off, pass));
        }
        side += PASS_STEP;
    }
    best.map(|(n, _, pass)| (pass, n))
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

/// For a way to `dest` that lies more than a right angle off `heading` at
/// `pos`: a first waypoint [`TURN`] along, turned a right angle toward its
/// side, so that the force swings round instead of reversing.
pub fn turn_point(pos: Vec3, heading: (f32, f32), dest: Vec3) -> Option<Vec3> {
    let (tx, tz) = (dest.x - pos.x, dest.z - pos.z);
    if heading.0 * tx + heading.1 * tz >= 0.0 || tx * tx + tz * tz < TURN * TURN {
        return None;
    }
    let side = if heading.0 * tz - heading.1 * tx >= 0.0 { 1.0 } else { -1.0 };
    // The heading turned a right angle: (-hz, hx) to the left.
    let (ax, az) = (-heading.1 * side, heading.0 * side);
    Some(Vec3::xz(pos.x + ax * TURN, pos.z + az * TURN))
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
        let Some(s) = self.squads.get(sid) else { return (None, "gone".into()) };
        let shape = s.shape.clone();
        let Some(leader) = s.get_leader(&self.fighters) else { return (None, "no leader".into()) };
        let (engaging, courage) = (s.engaging, s.courage);
        let pos = leader.pos();
        let cores: Vec<Unit> = (s.everyone().filter_map(|id| self.fighters.get(&id)))
            .map(|f| f.unit)
            .filter(|u| shape.is_core(u.def().map(|d| d.id())))
            .collect();
        let health = cores.iter().map(|&u| health_fraction(u)).sum::<f32>() / cores.len().max(1) as f32;
        let rallied = s.rally_fraction(&self.fighters).is_none_or(|r| r > shape.rallied);
        let trailers: Vec<Unit> =
            (s.trailers(&self.fighters).iter().filter_map(|id| self.fighters.get(id))).map(|f| f.unit).collect();
        let mut st = self.taskforces.packs.get(&sid).cloned().unwrap_or_default();
        let doctrine = shape.doctrine.clone();

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
        // Committed, a run begun is finished through its target first.
        let running =
            shape.commit && st.target != NULLPOS && st.pass.is_some_and(|p| p.along(pos, st.target) <= PASS_OUT);

        // Parked for repairs until healed.
        let healed = shape.heal_to.max(shape.retreat_health);
        if st.parked && health >= healed {
            st.parked = false;
            self.log(&format!(
                "taskforce: the {doctrine} pack (squad {sid}) is repaired ({:.0}%, {} cores)",
                100.0 * health,
                cores.len()
            ));
        } else if !st.parked && health < shape.retreat_health && !running {
            st.parked = true;
            st.target = NULLPOS;
            self.log(&format!(
                "taskforce: the {doctrine} pack (squad {sid}) parks for repairs ({:.0}%, {} cores)",
                100.0 * health,
                cores.len()
            ));
        }
        if st.parked {
            self.pack_go(sid, pos, head, home, false);
            self.pack_screen(sid, home, false);
            self.taskforces.packs.insert(sid, st);
            return (None, "parked for repairs".into());
        }

        // Repositioning (neither hurt nor hitting for `quiet` s) with the way
        // home clear, the hurt go for repairs alone.
        let quiet = (shape.quiet * 30.0) as i32;
        if shape.repair_below > 0.0 && !running && self.frame - st.last_hurt > quiet && self.frame - st.last_hit > quiet
        {
            let hurt: Vec<Unit> = cores.iter().copied().filter(|&u| health_fraction(u) < shape.repair_below).collect();
            if !hurt.is_empty() && self.pack_way_clear(leader, home) {
                let healths: Vec<String> =
                    hurt.iter().map(|&u| format!("{:.0}%", 100.0 * health_fraction(u))).collect();
                // (A pack formed of members hurt in another has no fight of
                // its own yet.)
                let last = st.last_hurt.max(st.last_hit);
                let quiet = if last == 0 {
                    "it has not fought".to_owned()
                } else {
                    format!("quiet {:.0} s", (self.frame - last) as f32 / 30.0)
                };
                self.log(&format!(
                    "taskforce: the {doctrine} pack (squad {sid}) sends {} of {} cores for repairs ({}): {quiet}, the \
                     way home clear",
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
                ai.pack_fates(sid, &pack_route(pos, t), st.ram.unwrap_or(false))
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
            // Judged by fates: the best run of a few targets, each rammed or
            // passed, by what it should kill less what it should lose.
            if fates {
                let mut best: Option<(Vec3, bool, Judgement, Vec<Value>)> = None;
                let mut tried = Vec::new();
                for _ in 0..FATE_CANDIDATES {
                    let (t, _) = self.war_best_target_shunning(leader, ahead, false, porc_cap, &shun);
                    if t == NULLPOS {
                        break;
                    }
                    shun.push(t);
                    tried.push(t);
                    for ram in [true, false] {
                        let Some((j, detail)) = self.pack_fates(sid, &pack_route(pos, t), ram) else { continue };
                        if best.as_ref().is_none_or(|b| j.worth() > b.2.worth()) {
                            best = Some((t, ram, j, detail));
                        }
                    }
                }
                match best {
                    Some((t, ram, j, detail)) if j.worth() >= shape.judge_margin => {
                        let verdict = if ram { "rams" } else { "passes" };
                        self.pack_log_judgement(sid, t, &j, verdict);
                        self.pack_emit_judgement(sid, t, &j, detail, verdict);
                        st.ram = Some(ram);
                        st.target = self.war_mark_target(t);
                        st.since = self.frame;
                        st.mobile = self.pack_target_left(t).is_some_and(|l| l.1);
                    }
                    best => {
                        if let Some((t, _, j, detail)) = best {
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
            let (t, _) = if fates {
                (st.target, 0.0)
            } else {
                self.war_best_target_shunning(leader, ahead, false, porc_cap, &shun)
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
            self.log(&format!(
                "taskforce: the {doctrine} pack (squad {sid}) dives ({:.0}, {:.0}), {:.0} away, {} cores at {:.0}%",
                st.target.x,
                st.target.z,
                distance(pos, st.target),
                cores.len(),
                100.0 * health
            ));
        }
        let target = st.target;
        // A pass is over once it is well beyond its pass point.
        if st.pass.is_some_and(|p| p.along(pos, target) > PASS_OUT) {
            st.pass = None;
            st.passes += 1;
        }
        let charge = match (st.ram.filter(|_| fates), shape.attack) {
            (Some(ram), _) => ram,
            (None, Attack::Pass) => false,
            (None, Attack::Charge) => true,
            (None, Attack::Mixed) => st.mobile,
        };
        // A charge is aimed anew until it is close.
        let aim = charge && st.pass.is_some_and(|p| p.along(pos, target) < -CHARGE_LOCK);
        if st.pass.is_none() && distance(pos, target) > PLAN_AT {
            let way = self.pack_way(sid, pos, head, target, true);
            self.pack_drive(sid, &way);
        } else {
            let pass = match st.pass.filter(|_| !aim) {
                Some(p) => p,
                None => {
                    let guns = self.pack_guns(sid, shape.pass_reach);
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
                    let planned = plan_pass(pos, head, target, &guns, clear);
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
            st.pass = Some(pass);
            let [a, b] = pass_way(pos, target, &pass);
            // Swinging round to come back over it.
            let way: Vec<Vec3> = turn_point(pos, head, a).into_iter().chain([a, b]).collect();
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

    /// A unit of ours took damage (`hurt`) or dealt it: if it is in a pack,
    /// that pack is fighting.
    pub fn pack_touched(&mut self, unit: Unit, hurt: bool) {
        if self.taskforces.packs.is_empty() {
            return;
        }
        let Some(sid) = self.fighters.get(&unit.id()).and_then(|f| f.squad) else { return };
        let frame = self.frame;
        if let Some(p) = self.taskforces.packs.get_mut(&sid) {
            if hurt {
                p.last_hurt = frame;
            } else {
                p.last_hit = frame;
            }
        }
    }

    /// Whether the way from the pack's leader to `home` (its path avoiding
    /// enemies) crosses no enemy threat or porc.
    fn pack_way_clear(&mut self, leader: Unit, home: Vec3) -> bool {
        let path = self.pathfinder.find_path(&self.war, leader, home, PathCost::AvoidEnemies);
        let view = self.war.view(leader.def());
        path.iter().all(|&p| view.get_threat(p) <= 0.0 && view.get_porc_threat(p) <= 0.0)
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

    /// Send pack `sid` from `pos` to `dest` unless it stands there already
    /// (`assault`: by the attack path, else away from enemies).
    fn pack_go(&mut self, sid: SquadId, pos: Vec3, head: (f32, f32), dest: Vec3, assault: bool) {
        if distance(pos, dest) <= STAND_NEAR {
            return;
        }
        let way = self.pack_way(sid, pos, head, dest, assault);
        self.pack_drive(sid, &way);
    }

    /// Waypoints for pack `sid`'s leader from `pos` to `dest`: an arc
    /// first if `dest` lies more than a right angle off its heading, then
    /// its path, points [`SPACING`] apart, ending at `dest` when it is near.
    fn pack_way(&mut self, sid: SquadId, pos: Vec3, head: (f32, f32), dest: Vec3, assault: bool) -> Vec<Vec3> {
        let mut way: Vec<Vec3> = turn_point(pos, head, dest).into_iter().collect();
        let from = way.last().copied().unwrap_or(pos);
        let Some(leader) = self.squads.get(sid).and_then(|s| s.get_leader(&self.fighters)) else { return way };
        let cost = if assault { PathCost::AssaultPath } else { PathCost::AvoidEnemies };
        let path = self.pathfinder.find_path(&self.war, leader, dest, cost);
        way.extend(spaced(path.into_iter().filter(|p| distance(*p, pos) > SPACING / 2.0), from, 2, SPACING));
        if way.last().is_none_or(|&l| distance(l, dest) < 2.0 * SPACING && distance(l, dest) > 1.0) {
            way.push(dest);
        }
        way
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

#[cfg(test)]
mod tests {
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
        assert!(turn_point(o, (1.0, 0.0), Vec3::xz(500.0, 400.0)).is_none());
        assert!(turn_point(o, (1.0, 0.0), Vec3::xz(-100.0, 0.0)).is_none());
        // Behind and to the left: a right angle to the left first.
        let p = turn_point(o, (1.0, 0.0), Vec3::xz(-800.0, 100.0)).unwrap();
        assert!(close(p, Vec3::xz(0.0, TURN)));
        let p = turn_point(o, (1.0, 0.0), Vec3::xz(-800.0, -100.0)).unwrap();
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
