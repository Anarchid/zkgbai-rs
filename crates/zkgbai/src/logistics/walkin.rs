//! The walk-in guard (`walkin`, on by default; `disable=walkin`).
//!
//! The rule (user, 2026-11-03): is there an enemy force worth 1500 metal or
//! more, known where it stands, that no force of ours would intercept
//! before it gets to something we value? Then fortify that thing.
//! Recalibrated 2026-11-04 (user: "it almost let a ravager ball into its
//! factory, and the factory was never defended"): on Comet Catcher 4
//! Ravagers and a Leveler (1000 metal seen at 8:20, 1240 at 8:25) drove
//! into our factory, under the 1500 bar; a forming constructor 450 away
//! and 3 caretakers stood by. The bar is [`MIN_VALUE`] = 800 now (raider
//! packs of 300-700 stay under it), the dearest places are fortified
//! first, and a place worth [`BUMP_RATIO`] times an unstarted turret's
//! place takes its slot under [`MAX_OPEN`].
//!
//! - *A force*: identified enemy ground fighters (commanders too, not
//!   constructors) seen within the last [`KNOWN`] frames, linked into
//!   groups [`LINK`] elmos apart, worth [`MIN_VALUE`] metal or more. It
//!   moves at the speed at which [`MIN_VALUE`] of it arrives (its fastest
//!   members first), on the movement tier of most of its value, never
//!   through a chokepoint our turrets hold against it.
//! - *Something we value*: places round our dearest buildings, one per
//!   [`PLACE_LINK`]; a place counts when our buildings and constructors
//!   within [`VAR_RADIUS`] are worth [`PLACE_VALUE`] or more. The places
//!   the group can get within its weapons' reach of within [`HORIZON`]
//!   seconds, soonest first (factories and the base count, not only the
//!   metal spots the Defense client watches).
//! - *Intercept*: what fights the group by the time it gets there: our
//!   turrets covering the place, our fighters (and armed constructors) at
//!   the place ([`PRESENT`]), our fighters on the group's route there
//!   ([`IN_WAY`]), and those free to answer (not attacking, not raiding,
//!   not retreating: forming and rallying squads, QRF teams and guards)
//!   that get there first along the ground. The place is defended if all
//!   that is as strong as the group.
//! - *Fortify*: at the first place where nothing intercepts, a turret on
//!   the group's side of it: the cheapest that makes the place as strong
//!   as the group, else the strongest; one that reaches as far as the
//!   group's main weapons and that the nearest crew (with the caretakers
//!   in reach) can have standing before the group arrives, or before as
//!   long again as it has already stood within reach of the place (a force
//!   that stands is not walking in; [`DWELL_GAP`]). The turret goes at
//!   the place itself when the enemy already covers the side toward the
//!   group. It is a logistics issue (kind `walkin`): the crew gets a
//!   share of the place's value, other construction waits for it
//!   (`pause`), and it is dropped if no force has had its place within
//!   reach (met or not: a force's strength flickers with what we see of
//!   it) for [`STAND_DOWN`] before it is started. At most [`MAX_OPEN`]
//!   under way, [`PLACE_CAP`] per place per [`CAP_WINDOW`] (dropped ones
//!   not counted).
//!
//! Journal: `k=walkin` per decision (`event`: open, defended, pending,
//! capped, outranged, late, nosite, stood_down), at most one per place and
//! event every [`NOTE_FRAMES`].

use std::collections::BTreeMap;

use serde_json::{Value, json};
use springai::{Unit, UnitDef, Vec3};
use zk_terrain::Tier;
use zk_terrain::paths::Paths;

use super::{Hand, IN_TIME, Kind, MARGIN, UNIT_COST_CAP, Worth, best_crew_stretched, porc_power};
use crate::Ai;
use crate::economy::TaskId;
use crate::terrain::{pos, tier_of};
use crate::util::{distance, get_directional_point};

/// The switch.
pub const FEATURE: &str = "walkin";

/// A force worth this much metal or more counts (the user's 1500 until
/// 2026-11-04, see the module notes).
pub const MIN_VALUE: f32 = 800.0;

/// Enemies seen within this many frames are known where they stand.
const KNOWN: i32 = 300;

/// Enemies this close to one another are one force, elmos.
const LINK: f32 = 450.0;

/// Places a force gets to within this many seconds are at stake.
const HORIZON: f32 = 60.0;

/// The soonest this many places a force gets to are looked at.
const MAX_PLACES: usize = 8;

/// One place per this much ground round our dearest buildings, elmos.
const PLACE_LINK: f32 = 350.0;

/// What a place holds: our buildings and constructors this close, elmos.
const VAR_RADIUS: f32 = 600.0;

/// A place is something we value from this much metal on.
const PLACE_VALUE: f32 = 600.0;

/// Our units this close to a place fight there, elmos.
const PRESENT: f32 = 500.0;

/// Our fighters this close to a force's route meet it on the way, elmos.
const IN_WAY: f32 = 300.0;

/// A force starts hurting a place at its weapons' reach, at least this.
const TOUCH_MIN: f32 = 200.0;

/// A force's reach is the longest weapon range that at least this share
/// of its power has.
const REACH_SHARE: f32 = 0.2;

/// A turret may reach this much less than the force and still count.
const RANGE_SLACK: f32 = 20.0;

/// Forecast build times are stretched by this (home crews built in
/// 0.6-1.2 times the forecast in the `homeguard` smokes; long walks take
/// more, see [`super::PESSIMISM`]).
pub(super) const STRETCH: f32 = 1.2;

/// The turret stands this far from the place toward the force, elmos.
const OFFSET: f32 = 160.0;

/// The force's approach to a place is read this far back along its route.
const LOOKBACK: f32 = 400.0;

/// A place a force has stood within reach of for some seconds gets as long
/// again for its turret (counted up to [`HORIZON`]). Its clock starts over
/// once no force has had it within reach for this many frames; places
/// within [`DWELL_LINK`] elmos share a clock.
const DWELL_GAP: i32 = 450;
const DWELL_LINK: f32 = 300.0;

/// Walk-in turrets under way at once, at most.
const MAX_OPEN: usize = 2;

/// With [`MAX_OPEN`] under way, a place worth this many times the place of
/// an unstarted walk-in turret takes its slot (the cheapest such goes).
const BUMP_RATIO: f32 = 1.5;

/// Walk-in turrets per place (within [`CAP_RADIUS`]) per [`CAP_WINDOW`].
const PLACE_CAP: usize = 3;
const CAP_RADIUS: f32 = 500.0;
const CAP_WINDOW: i32 = 9000;

/// An unstarted walk-in turret no force has called for in this many
/// frames is dropped (as long as a place's dwell clock keeps,
/// [`DWELL_GAP`]).
const STAND_DOWN: i32 = DWELL_GAP;

/// Frames between journal notes for the same place and event.
const NOTE_FRAMES: i32 = 300;

/// Candidate turrets: Stardust, Faraday, Gauss, Stinger, Lotus.
const TURRETS: [&str; 5] = ["turretriot", "turretemp", "turretgauss", "turretheavylaser", "turretlaser"];

/// An enemy force.
#[derive(Clone, Debug)]
struct Group {
    value: f32,
    power: f32,
    units: usize,
    /// The member nearest its middle.
    pos: Vec3,
    /// Speed at which [`MIN_VALUE`] of it arrives, elmos/s.
    speed: f32,
    /// How far its main weapons reach, elmos.
    reach: f32,
    tier: Tier,
}

impl Group {
    fn json(&self) -> Value {
        json!({
            "value": self.value.round(), "power": self.power.round(), "units": self.units,
            "pos": [self.pos.x.round(), self.pos.z.round()], "speed": (self.speed * 10.0).round() / 10.0,
            "reach": self.reach.round(), "tier": self.tier.name(),
        })
    }
}

/// Something of ours worth defending.
#[derive(Clone, Copy, Debug)]
struct Place {
    /// Its dearest building.
    anchor: Vec3,
    /// Our buildings and constructors within [`VAR_RADIUS`], metal.
    value: f32,
}

/// One of our armed units.
struct Ours {
    pos: Vec3,
    /// Elmos/s; 0 for constructors (they never go to meet a force).
    speed: f32,
    power: f32,
    /// `None` for aircraft.
    tier: Option<Tier>,
    /// Free to answer: not attacking, raiding or building.
    free: bool,
}

/// What meets a force at a place by the time it gets there, by kind.
#[derive(Clone, Copy, Debug, Default)]
struct Interceptors {
    porc: f32,
    present: f32,
    in_way: f32,
    coming: f32,
}

impl Interceptors {
    fn total(&self) -> f32 {
        self.porc + self.present + self.in_way + self.coming
    }

    fn json(&self) -> Value {
        json!({"porc": self.porc.round(), "present": self.present.round(), "in_way": self.in_way.round(),
               "coming": self.coming.round()})
    }
}

/// A force and the first place where nothing meets it (`breach`), or the
/// soonest place it reaches, met.
struct Assessment {
    group: Group,
    place: Place,
    seconds: f32,
    ours: Interceptors,
    /// Where it comes from into the place (its route's last waypoint).
    approach: Vec3,
    breach: bool,
    /// Seconds a force has stood within reach of the place (see
    /// [`DWELL_GAP`]).
    dwell: f32,
}

/// Running totals, logged with the logistics summary.
#[derive(Default, Clone, Copy)]
pub struct Stats {
    /// Forces worth [`MIN_VALUE`] (per assessment).
    pub groups: u32,
    /// Of those, with a place of ours within the horizon.
    pub threats: u32,
    /// Of those, met at every place.
    pub defended: u32,
    /// Places reached with nothing to meet the force.
    pub breaches: u32,
    pub opened: u32,
    /// A turret for the place is already under way.
    pub pending: u32,
    pub capped: u32,
    pub outranged: u32,
    /// No turret stands in time.
    pub late: u32,
    pub nosite: u32,
    pub stood_down: u32,
    pub bumped: u32,
}

impl Stats {
    pub fn json(&self) -> Value {
        json!({
            "groups": self.groups, "threats": self.threats, "defended": self.defended, "breaches": self.breaches,
            "opened": self.opened, "pending": self.pending, "capped": self.capped, "outranged": self.outranged,
            "late": self.late, "nosite": self.nosite, "stood_down": self.stood_down, "bumped": self.bumped,
        })
    }
}

#[derive(Default)]
pub struct State {
    pub stats: Stats,
    /// Walk-in turrets opened (stood down ones forgotten): the place, the
    /// frame, the task.
    opened: Vec<(Vec3, i32, TaskId)>,
    /// Frame each open walk-in issue was last called for: a force had its
    /// place within reach (breach or met: a force's strength flickers with
    /// what of it we see).
    called: BTreeMap<TaskId, i32>,
    /// Caretakers off their factory to help build: the nanoframe each
    /// repairs.
    helping: BTreeMap<i32, Unit>,
    /// Last journal note per place (on a 256-elmo grid) and event.
    notes: BTreeMap<(i32, i32, &'static str), i32>,
    /// Places a force has within reach: anchor, first and last frame.
    standing: Vec<(Vec3, i32, i32)>,
}

/// Seconds a force has had the place at `anchor` within reach, this pass
/// included: its clock in `standing` (stale clocks pruned by the caller,
/// see [`DWELL_GAP`]), started if it has none.
fn stand(standing: &mut Vec<(Vec3, i32, i32)>, anchor: Vec3, frame: i32) -> f32 {
    match standing.iter_mut().find(|s| distance(s.0, anchor) <= DWELL_LINK) {
        Some(s) => {
            s.0 = anchor;
            s.2 = frame;
            ((frame - s.1) as f32 / 30.0).min(HORIZON)
        }
        None => {
            standing.push((anchor, frame, frame));
            0.0
        }
    }
}

/// The open walk-in turret that gives way to a place worth `value`: of
/// those not started, the one guarding the least, if `value` is
/// [`BUMP_RATIO`] times that or more. `open`: (task, its place's worth,
/// started).
fn bump<T: Copy>(open: &[(T, f32, bool)], value: f32) -> Option<T> {
    (open.iter()).filter(|o| !o.2 && o.1 * BUMP_RATIO <= value).min_by(|a, b| a.1.total_cmp(&b.1)).map(|o| o.0)
}

/// What an issue's worth stands for, undiscounted (walk-in turrets: their
/// place's value).
fn worth_value(w: &Worth) -> f32 {
    match *w {
        Worth::Flat(value) | Worth::Deadline { value, .. } | Worth::Stream { value, .. } => value,
    }
}

/// Distance from `p` to the polyline `line` (on the ground plane).
fn to_line(line: &[Vec3], p: Vec3) -> f32 {
    let mut best = f32::INFINITY;
    for w in line.windows(2) {
        let (a, b) = (w[0], w[1]);
        let (dx, dz) = (b.x - a.x, b.z - a.z);
        let len2 = dx * dx + dz * dz;
        let t = if len2 > 0.0 { (((p.x - a.x) * dx + (p.z - a.z) * dz) / len2).clamp(0.0, 1.0) } else { 0.0 };
        best = best.min(distance(p, Vec3::xz(a.x + t * dx, a.z + t * dz)));
    }
    if line.len() == 1 { distance(p, line[0]) } else { best }
}

/// The speed at which `need` metal of a force arrives: members as (speed,
/// value), fastest first.
fn arrival_speed(members: &mut [(f32, f32)], need: f32) -> f32 {
    members.sort_by(|a, b| b.0.total_cmp(&a.0));
    let mut sum = 0.0;
    for &(speed, value) in members.iter() {
        sum += value;
        if sum >= need {
            return speed;
        }
    }
    members.last().map_or(0.0, |m| m.0)
}

/// The longest weapon range that at least `share` of a force's power has:
/// members as (range, power).
fn reach_of(members: &mut [(f32, f32)], share: f32) -> f32 {
    let total: f32 = members.iter().map(|m| m.1).sum();
    members.sort_by(|a, b| b.0.total_cmp(&a.0));
    let mut sum = 0.0;
    for &(range, power) in members.iter() {
        sum += power;
        if sum >= share * total {
            return range;
        }
    }
    0.0
}

/// Which turret, of candidates (power, cost, in time): in time, the
/// cheapest that makes `ours` as strong as `enemy`, else the strongest.
fn pick_turret(candidates: &[(f32, f32, bool)], ours: f32, enemy: f32) -> Option<usize> {
    let ok: Vec<usize> = (0..candidates.len()).filter(|&i| candidates[i].2).collect();
    let enough = ok.iter().copied().filter(|&i| ours + candidates[i].0 >= enemy);
    if let Some(i) = enough.min_by(|&a, &b| candidates[a].1.total_cmp(&candidates[b].1)) {
        return Some(i);
    }
    ok.into_iter().max_by(|&a, &b| candidates[a].0.total_cmp(&candidates[b].0))
}

impl Ai {
    /// The walk-in guard's assessment (every logistics period).
    pub(super) fn logistics_walkins(&mut self, now: f32) {
        let frame = self.frame;
        self.walkin_stand_down(frame);
        self.walkin_caretakers();
        let groups = self.walkin_groups(frame);
        self.logistics.walkin.stats.groups += groups.len() as u32;
        if groups.is_empty() {
            return;
        }
        let places = self.walkin_places();
        if places.is_empty() {
            return;
        }
        let ours = self.walkin_ours();
        let standing = &mut self.logistics.walkin.standing;
        standing.retain(|&(_, _, last)| frame - last <= DWELL_GAP);
        let mut assessed = self.walkin_assess(&groups, &places, &ours);
        for a in &mut assessed {
            a.dwell = stand(&mut self.logistics.walkin.standing, a.place.anchor, frame);
            let anchor = a.place.anchor;
            for i in self.logistics.issues.values() {
                if i.kind == Kind::Walkin && distance(i.pos, anchor) <= VAR_RADIUS {
                    self.logistics.walkin.called.insert(i.task, frame);
                }
            }
        }
        // The dearest places first: they get the turrets under the caps.
        assessed.sort_by(|a, b| b.place.value.total_cmp(&a.place.value));
        for a in assessed {
            if a.breach {
                self.logistics.walkin.stats.breaches += 1;
                self.walkin_fortify(&a, now);
            } else {
                self.logistics.walkin.stats.defended += 1;
                self.walkin_note("defended", &a, json!({}));
            }
        }
    }

    /// Known enemy forces worth [`MIN_VALUE`] or more, dearest first.
    fn walkin_groups(&self, frame: i32) -> Vec<Group> {
        struct M {
            pos: Vec3,
            value: f32,
            power: f32,
            speed: f32,
            reach: f32,
            tier: Tier,
        }
        let mut ms: Vec<M> = Vec::new();
        for e in self.war.targets.values() {
            if !e.identified
                || e.is_dead
                || e.is_static
                || e.can_fly()
                || (e.is_worker && !e.is_com)
                || frame - e.last_seen > KNOWN
            {
                continue;
            }
            let power = e.surface_danger(e.is_submerged());
            let Some(tier) = e.ud.and_then(tier_of) else { continue };
            if power <= 0.0 {
                continue;
            }
            let reach = e.def_max_weapon_range();
            ms.push(M { pos: e.position, value: e.value, power, speed: e.def_speed(), reach, tier });
        }
        // Single linkage.
        let n = ms.len();
        let mut parent: Vec<usize> = (0..n).collect();
        fn root(p: &mut [usize], mut i: usize) -> usize {
            while p[i] != i {
                p[i] = p[p[i]];
                i = p[i];
            }
            i
        }
        for i in 0..n {
            for j in i + 1..n {
                if distance(ms[i].pos, ms[j].pos) <= LINK {
                    let (a, b) = (root(&mut parent, i), root(&mut parent, j));
                    if a != b {
                        parent[a] = b;
                    }
                }
            }
        }
        let mut sets: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
        for i in 0..n {
            let r = root(&mut parent, i);
            sets.entry(r).or_default().push(i);
        }
        let mut groups = Vec::new();
        for idx in sets.values() {
            let value: f32 = idx.iter().map(|&i| ms[i].value).sum();
            if value < MIN_VALUE {
                continue;
            }
            let power: f32 = idx.iter().map(|&i| ms[i].power).sum();
            let k = idx.len() as f32;
            let (x, z) = idx.iter().fold((0.0, 0.0), |(x, z), &i| (x + ms[i].pos.x / k, z + ms[i].pos.z / k));
            let middle = Vec3::xz(x, z);
            let at = idx
                .iter()
                .map(|&i| ms[i].pos)
                .min_by(|a, b| distance(*a, middle).total_cmp(&distance(*b, middle)))
                .unwrap_or(middle);
            let mut speeds: Vec<(f32, f32)> = idx.iter().map(|&i| (ms[i].speed, ms[i].value)).collect();
            let speed = arrival_speed(&mut speeds, MIN_VALUE);
            let mut ranges: Vec<(f32, f32)> = idx.iter().map(|&i| (ms[i].reach, ms[i].power)).collect();
            let reach = reach_of(&mut ranges, REACH_SHARE);
            let mut by_tier: BTreeMap<Tier, f32> = BTreeMap::new();
            for &i in idx {
                *by_tier.entry(ms[i].tier).or_default() += ms[i].value;
            }
            let Some(tier) = by_tier.into_iter().max_by(|a, b| a.1.total_cmp(&b.1)).map(|t| t.0) else { continue };
            groups.push(Group { value, power, units: idx.len(), pos: at, speed, reach, tier });
        }
        groups.sort_by(|a, b| b.value.total_cmp(&a.value));
        groups
    }

    /// Our places worth [`PLACE_VALUE`] or more.
    fn walkin_places(&self) -> Vec<Place> {
        let team = self.team_id;
        let mut buildings: Vec<(Vec3, f32)> = Vec::new();
        for u in self.cb.team_units() {
            if u.team() != team || u.health() <= 0.0 {
                continue;
            }
            let Some(d) = u.def() else { continue };
            if d.speed() > 0.0 {
                continue;
            }
            let cost = d.cost(self.metal);
            // Terraform placeholders cost 100000 on paper.
            if cost >= UNIT_COST_CAP {
                continue;
            }
            let cost = if u.is_being_built() { cost * u.build_progress().clamp(0.0, 1.0) } else { cost };
            if cost > 0.0 {
                buildings.push((u.pos(), cost));
            }
        }
        let workers: Vec<(Vec3, f32)> = (self.eco.workers.values())
            .filter(|w| w.unit.health() > 0.0 && w.unit.team() == team)
            .filter_map(|w| Some((w.get_pos(), w.unit.def()?.cost(self.metal))))
            .collect();
        buildings.sort_by(|a, b| b.1.total_cmp(&a.1));
        let mut anchors: Vec<Vec3> = Vec::new();
        for &(p, _) in &buildings {
            if anchors.iter().all(|&a| distance(a, p) > PLACE_LINK) {
                anchors.push(p);
            }
        }
        (anchors.into_iter())
            .filter_map(|anchor| {
                let value: f32 = (buildings.iter().chain(&workers))
                    .filter(|(p, _)| distance(*p, anchor) <= VAR_RADIUS)
                    .map(|x| x.1)
                    .sum();
                (value >= PLACE_VALUE).then_some(Place { anchor, value })
            })
            .collect()
    }

    /// Our armed units, and whether each is free to answer a call.
    fn walkin_ours(&self) -> Vec<Ours> {
        let team = self.team_id;
        let mut ours = Vec::new();
        for f in self.fighters.values() {
            let u = f.unit;
            if f.power <= 0.0 || u.health() <= 0.0 || u.team() != team || self.retreat_handler.is_retreating(f.id) {
                continue;
            }
            let Some(d) = u.def() else { continue };
            let committed = self.raider_handler.solo_raiders.contains(&f.id)
                || f.squad.and_then(|s| self.squads.get(s)).is_some_and(|s| s.status == 'a' || s.engaging);
            let tier = if d.is_able_to_fly() { None } else { tier_of(d) };
            let power = (f.power + u.max_health()) / 10.0;
            ours.push(Ours { pos: f.get_pos(), speed: d.speed(), power, tier, free: !committed });
        }
        // The QRF's own teams and guards (moonlighters are in squads).
        for m in self.qrf.members.values().filter(|m| m.squad.is_none()) {
            if m.unit.health() <= 0.0 || self.retreat_handler.is_retreating(m.unit.id()) {
                continue;
            }
            let power = (m.threat + m.unit.max_health()) / 10.0;
            ours.push(Ours { pos: m.pos(), speed: m.speed, power, tier: tier_of(m.def), free: true });
        }
        // Commanders and armed constructors fight where they stand (as the
        // threat map counts them).
        for w in self.eco.workers.values() {
            if w.unit.health() <= 0.0 || !w.unit.def().is_some_and(|d| d.is_able_to_attack()) {
                continue;
            }
            let power = (w.power + w.unit.max_health()) / 14.0;
            ours.push(Ours { pos: w.get_pos(), speed: 0.0, power, tier: None, free: false });
        }
        ours
    }

    /// For each force, the first place it gets to that nothing of ours
    /// meets it at (or the soonest, met). At most one force per place.
    fn walkin_assess(&mut self, groups: &[Group], places: &[Place], ours: &[Ours]) -> Vec<Assessment> {
        let Some(terrain) = self.terrain.get() else { return Vec::new() };
        let war = &self.war;
        let mut threats = 0;
        let mut out: Vec<Assessment> = Vec::new();
        for g in groups {
            let Some(map) = terrain.tier(g.tier) else { continue };
            let held = |c: usize| {
                let p = map.chokes[c].pos;
                war.get_friendly_porc_threat(Vec3::xz(p.x, p.z)) >= g.power
            };
            let paths = Paths::new(map, pos(g.pos), held);
            if paths.origin_region().is_none() {
                continue;
            }
            let touch = g.reach.max(TOUCH_MIN);
            let mut reached: Vec<(f32, &Place)> = (places.iter())
                .filter_map(|pl| {
                    let d = paths.distance_to(pos(pl.anchor))?;
                    let t = (d - touch).max(0.0) / g.speed.max(1.0);
                    (t <= HORIZON).then_some((t, pl))
                })
                .collect();
            if reached.is_empty() {
                continue;
            }
            threats += 1;
            reached.sort_by(|a, b| a.0.total_cmp(&b.0));
            reached.truncate(MAX_PLACES);
            let mut first: Option<Assessment> = None;
            for (t, pl) in reached {
                // Already there: too late to plan for this place.
                if t <= MARGIN {
                    continue;
                }
                let mut line = vec![g.pos];
                line.extend(paths.route(pos(pl.anchor)).into_iter().map(|c| {
                    let p = map.chokes[c].pos;
                    Vec3::xz(p.x, p.z)
                }));
                line.push(pl.anchor);
                let mut i = Interceptors { porc: war.get_friendly_porc_threat(pl.anchor), ..Default::default() };
                let mut back: Vec<(Tier, Paths)> = Vec::new();
                for o in ours {
                    if distance(o.pos, pl.anchor) <= PRESENT {
                        i.present += o.power;
                        continue;
                    }
                    if o.speed > 0.0 && to_line(&line, o.pos) <= IN_WAY {
                        i.in_way += o.power;
                        continue;
                    }
                    if !o.free || o.speed <= 0.0 {
                        continue;
                    }
                    let straight = distance(o.pos, pl.anchor);
                    let d = match o.tier {
                        None => straight,
                        Some(tier) => {
                            let k = match back.iter().position(|b| b.0 == tier) {
                                Some(k) => k,
                                None => {
                                    let Some(m) = terrain.tier(tier) else { continue };
                                    back.push((tier, Paths::new(m, pos(pl.anchor), |_| false)));
                                    back.len() - 1
                                }
                            };
                            match back[k].1.distance_to(pos(o.pos)) {
                                Some(d) => d.max(straight),
                                None => continue,
                            }
                        }
                    };
                    if d / o.speed <= t {
                        i.coming += o.power;
                    }
                }
                let approach = paths.approach(pos(pl.anchor), LOOKBACK).map_or(g.pos, |p| Vec3::xz(p.x, p.z));
                let breach = i.total() < g.power;
                let a = Assessment { group: g.clone(), place: *pl, seconds: t, ours: i, approach, breach, dwell: 0.0 };
                if breach {
                    first = Some(a);
                    break;
                }
                first.get_or_insert(a);
            }
            let Some(a) = first else { continue };
            if a.breach && out.iter().any(|b| b.breach && distance(b.place.anchor, a.place.anchor) < CAP_RADIUS) {
                continue;
            }
            out.push(a);
        }
        self.logistics.walkin.stats.threats += threats;
        out
    }

    /// Fortify the place of breach `a`: a turret toward the force, if one
    /// stands in time (see the module notes).
    fn walkin_fortify(&mut self, a: &Assessment, now: f32) {
        let frame = self.frame;
        let by = now + a.seconds.max(a.dwell) - MARGIN;
        let worth = Worth::Deadline { value: a.place.value, by };
        // A turret for this place under way (anywhere it covers: a base has
        // several places): keep it called for, its deadline current.
        let open: Vec<(TaskId, Kind)> = (self.logistics.issues.values())
            .filter(|i| matches!(i.kind, Kind::Walkin | Kind::Defense) && distance(i.pos, a.place.anchor) <= VAR_RADIUS)
            .map(|i| (i.task, i.kind))
            .collect();
        if !open.is_empty() {
            for (task, kind) in open {
                if kind != Kind::Walkin {
                    continue;
                }
                self.logistics.walkin.called.insert(task, frame);
                if let Some(i) = self.logistics.issues.get_mut(&task) {
                    i.worth = worth;
                    i.against = a.group.power;
                }
            }
            self.logistics.walkin.stats.pending += 1;
            self.walkin_note("pending", a, json!({}));
            return;
        }
        let under_way = self.logistics.issues.values().filter(|i| i.kind == Kind::Walkin).count();
        self.logistics.walkin.opened.retain(|&(_, f, _)| frame - f < CAP_WINDOW);
        let here = (self.logistics.walkin.opened.iter())
            .filter(|(p, _, _)| distance(*p, a.place.anchor) <= CAP_RADIUS)
            .count();
        // The slot this place would take (see `bump`); given up only once
        // this place's turret stands planned, else the cheaper place would
        // reopen its own in the same pass.
        let bumped = if under_way >= MAX_OPEN && here < PLACE_CAP {
            let open: Vec<(TaskId, f32, bool)> = (self.logistics.issues.values())
                .filter(|i| i.kind == Kind::Walkin)
                .map(|i| (i.task, worth_value(&i.worth), i.started))
                .collect();
            bump(&open, a.place.value)
        } else {
            None
        };
        if bumped.is_none() && (under_way >= MAX_OPEN || here >= PLACE_CAP) {
            self.logistics.walkin.stats.capped += 1;
            self.walkin_note("capped", a, json!({"under_way": under_way, "here": here}));
            return;
        }
        // Toward the force; else (the enemy covers that side already, or it
        // is blocked) at the place itself.
        let mut sites = vec![a.place.anchor];
        if distance(a.place.anchor, a.approach) > 1.0 {
            sites.insert(0, get_directional_point(a.place.anchor, a.approach, OFFSET));
        }
        let site = sites[0];
        let hands = self.walkin_hands(site, &[], None, false);
        let (metal, income) = (self.eco.metal, self.eco.effective_income);
        let ours = a.ours.total();
        let mut defs: Vec<(UnitDef, f32)> = Vec::new();
        let mut candidates: Vec<(f32, f32, bool)> = Vec::new();
        let mut outranged = 0;
        for name in TURRETS {
            let Some(def) = self.defs.get(name) else { continue };
            if def.max_weapon_range() + RANGE_SLACK < a.group.reach {
                outranged += 1;
                continue;
            }
            let cost = def.cost(self.metal);
            let (_, eta, value) =
                best_crew_stretched(&hands, def.build_time(), cost, metal, income, &worth, now, STRETCH);
            defs.push((def, eta));
            candidates.push((porc_power(def), cost, value >= IN_TIME * a.place.value));
        }
        let Some(k) = pick_turret(&candidates, ours, a.group.power) else {
            let (event, n) = if defs.is_empty() { ("outranged", outranged) } else { ("late", defs.len()) };
            if defs.is_empty() {
                self.logistics.walkin.stats.outranged += 1;
            } else {
                self.logistics.walkin.stats.late += 1;
            }
            let etas: Vec<Value> =
                (defs.iter()).map(|(d, eta)| json!([self.defs.name(*d), (eta * 10.0).round() / 10.0])).collect();
            self.walkin_note(event, a, json!({"turrets": n, "etas": etas, "hands": hands.len()}));
            return;
        };
        let (def, eta) = defs[k];
        let Some((task, site)) = sites.iter().find_map(|&s| self.eco_logistics_porc(def, s, 0.0).map(|(t, _)| (t, s)))
        else {
            self.logistics.walkin.stats.nosite += 1;
            self.walkin_note("nosite", a, json!({"turret": self.defs.name(def)}));
            return;
        };
        if let Some(old) = bumped {
            self.logistics.walkin.opened.retain(|o| o.2 != old);
            self.logistics.walkin.called.remove(&old);
            self.logistics.walkin.stats.bumped += 1;
            self.logistics_close(old, "bumped");
            self.eco.eco_logistics_drop(old);
        }
        // Unstamped: the economy drops stamped porc nobody took up within
        // 20 s; this one stays while a force calls for it (`STAND_DOWN`).
        if let Some(c) = self.eco.tasks.get_mut(task).and_then(|t| t.as_construction_mut()) {
            c.frame_issued = 0;
        }
        let what = self.defs.name(def).to_owned();
        let why = format!(
            "walk-in: {} units worth {:.0} (power {:.0}, reach {:.0}) {:.0}s from {:.0} of ours (standing {:.0}s); met by {:.0} \
             (porc {:.0}, present {:.0}, in the way {:.0}, coming {:.0}); {what} +{:.0}, eta {eta:.0}s, {} hands",
            a.group.units,
            a.group.value,
            a.group.power,
            a.group.reach,
            a.seconds,
            a.place.value,
            a.dwell,
            ours,
            a.ours.porc,
            a.ours.present,
            a.ours.in_way,
            a.ours.coming,
            candidates[k].0,
            hands.len(),
        );
        if let Some(id) = self.logistics_open(task, Kind::Walkin, worth, &why, a.group.power) {
            self.logistics.walkin.opened.push((a.place.anchor, frame, task));
            self.logistics.walkin.called.insert(task, frame);
            self.logistics.walkin.stats.opened += 1;
            let extra = json!({"issue": id, "turret": what, "turret_power": candidates[k].0.round(),
                               "eta": (eta * 10.0).round() / 10.0, "site": [site.x.round(), site.z.round()],
                               "hands": hands.len()});
            self.walkin_note("open", a, extra);
        }
    }

    /// Journal decision `event` on assessment `a` (rate limited per place).
    fn walkin_note(&mut self, event: &'static str, a: &Assessment, extra: Value) {
        let key = ((a.place.anchor.x / 256.0) as i32, (a.place.anchor.z / 256.0) as i32, event);
        let frame = self.frame;
        if event != "open" && self.logistics.walkin.notes.get(&key).is_some_and(|&f| frame - f < NOTE_FRAMES) {
            return;
        }
        self.logistics.walkin.notes.insert(key, frame);
        let mut note = json!({
            "event": event, "group": a.group.json(),
            "place": {"pos": [a.place.anchor.x.round(), a.place.anchor.z.round()], "value": a.place.value.round()},
            "seconds": (a.seconds * 10.0).round() / 10.0, "dwell": (a.dwell * 10.0).round() / 10.0,
            "ours": a.ours.json(), "met": a.ours.total().round(),
        });
        if let (Some(n), Value::Object(e)) = (note.as_object_mut(), extra) {
            n.extend(e);
        }
        if event == "open" {
            self.log(&format!(
                "walkin: fortify ({:.0}, {:.0}) worth {:.0} against {} units worth {:.0} {:.0}s out, met by {:.0} of {:.0}",
                a.place.anchor.x,
                a.place.anchor.z,
                a.place.value,
                a.group.units,
                a.group.value,
                a.seconds,
                a.ours.total(),
                a.group.power
            ));
        }
        self.op_emit("walkin", note);
    }

    /// Drop unstarted walk-in turrets no force has called for lately.
    fn walkin_stand_down(&mut self, frame: i32) {
        let issues = &self.logistics.issues;
        self.logistics.walkin.called.retain(|t, _| issues.contains_key(t));
        let stale: Vec<TaskId> = (self.logistics.issues.values())
            .filter(|i| i.kind == Kind::Walkin && !i.started)
            .filter(|i| self.logistics.walkin.called.get(&i.task).is_none_or(|&f| frame - f > STAND_DOWN))
            .map(|i| i.task)
            .collect();
        for task in stale {
            self.logistics.walkin.opened.retain(|o| o.2 != task);
            self.logistics.walkin.stats.stood_down += 1;
            self.logistics_close(task, "stood down");
            self.eco.eco_logistics_drop(task);
        }
    }

    /// Hands for a walk-in turret at `pos`: the workers that could crew it
    /// (see [`Ai::logistics_hands`]) and the caretakers in reach of it, who
    /// help once a worker has started it (`started`: at once).
    pub(super) fn walkin_hands(&self, pos: Vec3, crew: &[i32], task: Option<TaskId>, started: bool) -> Vec<Hand> {
        let mut hands = self.logistics_hands(pos, crew, task);
        let first = if started { Some(0.0) } else { hands.first().map(|h| h.1) };
        let Some(first) = first else { return hands };
        let team = self.team_id;
        for n in self.eco.nanos.values() {
            let u = n.unit;
            if u.health() <= 0.0 || u.team() != team || u.is_being_built() {
                continue;
            }
            let Some(d) = u.def() else { continue };
            if distance(u.pos(), pos) <= d.build_distance() + 24.0 {
                hands.push((u.id(), first, d.build_speed()));
            }
        }
        hands.sort_by(|a, b| a.1.total_cmp(&b.1));
        hands
    }

    /// Whether `id` is a caretaker (it takes no share of a crew's bonus:
    /// it goes where it is told).
    pub(super) fn walkin_is_caretaker(&self, id: i32) -> bool {
        self.eco.nanos.contains_key(&id)
    }

    /// Caretakers in reach of a walk-in turret under construction leave
    /// their factory to help build it, and go back once it stands or is
    /// gone.
    fn walkin_caretakers(&mut self) {
        let team = self.team_id;
        let frames: Vec<Unit> = (self.logistics.issues.values())
            .filter(|i| i.kind == Kind::Walkin)
            .filter_map(|i| self.eco.tasks.get(i.task)?.as_construction()?.target)
            .filter(|u| u.health() > 0.0 && u.is_being_built())
            .collect();
        let done: Vec<i32> =
            (self.logistics.walkin.helping.iter()).filter(|(_, f)| !frames.contains(f)).map(|(&id, _)| id).collect();
        for id in done {
            self.logistics.walkin.helping.remove(&id);
            if let Some(n) = self.eco.nanos.get(&id)
                && n.unit.health() > 0.0
                && n.target.health() > 0.0
            {
                n.unit.guard(n.target, 0);
            }
        }
        for f in frames {
            let at = f.pos();
            let near: Vec<Unit> = (self.eco.nanos.values())
                .filter(|n| n.unit.health() > 0.0 && n.unit.team() == team && !n.unit.is_being_built())
                .filter(|n| !self.logistics.walkin.helping.contains_key(&n.unit.id()))
                .filter(|n| n.unit.def().is_some_and(|d| distance(n.unit.pos(), at) <= d.build_distance() + 24.0))
                .map(|n| n.unit)
                .collect();
            if near.is_empty() {
                continue;
            }
            self.log(&format!(
                "walkin: {} caretakers help build {} at ({:.0}, {:.0})",
                near.len(),
                self.defs.name_of(f),
                at.x,
                at.z
            ));
            for u in near {
                u.repair(f, 0);
                self.logistics.walkin.helping.insert(u.id(), f);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_force_arrives_when_enough_of_it_has() {
        // 3 fast raiders worth 600 lead 3 slow assaults worth 1200: 1500
        // has arrived when the first assault has.
        let mut m = vec![(60.0, 400.0), (110.0, 200.0), (60.0, 400.0), (110.0, 200.0), (110.0, 200.0), (60.0, 400.0)];
        assert_eq!(arrival_speed(&mut m, 1500.0), 60.0);
        // Raiders alone worth 1600: their speed.
        let mut m = vec![(115.0, 400.0), (110.0, 400.0), (120.0, 400.0), (112.0, 400.0)];
        assert_eq!(arrival_speed(&mut m, 1500.0), 110.0);
        // Never enough: the slowest.
        let mut m = vec![(80.0, 100.0), (50.0, 100.0)];
        assert_eq!(arrival_speed(&mut m, 1500.0), 50.0);
    }

    #[test]
    fn reach_is_what_a_fifth_of_the_power_has() {
        // Ten Glaives (range 185) and two Fencers (700) with 9% each of
        // the power: together less than a fifth; three are more.
        let mut m: Vec<(f32, f32)> =
            std::iter::repeat_n((185.0, 8.0), 10).chain([(700.0, 9.0), (700.0, 9.0)]).collect();
        assert_eq!(reach_of(&mut m, 0.2), 185.0);
        m.push((700.0, 9.0));
        assert_eq!(reach_of(&mut m, 0.2), 700.0);
        // A dozen equal raiders: their range (no member is a fifth alone).
        let mut m = vec![(185.0, 10.0); 12];
        assert_eq!(reach_of(&mut m, 0.2), 185.0);
    }

    #[test]
    fn the_cheapest_turret_that_suffices_else_the_strongest() {
        // (power, cost, in time)
        let c = [(150.0, 220.0, true), (300.0, 450.0, true), (400.0, 400.0, false), (80.0, 90.0, true)];
        // 200 ours vs 330: the 220 one makes it.
        assert_eq!(pick_turret(&c, 200.0, 330.0), Some(0));
        // 200 ours vs 450: only the 450 one makes it (the 400 is late).
        assert_eq!(pick_turret(&c, 200.0, 450.0), Some(1));
        // Nothing suffices: the strongest in time.
        assert_eq!(pick_turret(&c, 0.0, 2000.0), Some(1));
        // Nothing in time.
        assert_eq!(pick_turret(&[(1.0, 1.0, false)], 0.0, 10.0), None);
    }

    #[test]
    fn a_force_that_stands_gets_as_long_again() {
        let mut standing = Vec::new();
        let base = Vec3::xz(1000.0, 1000.0);
        assert_eq!(stand(&mut standing, base, 0), 0.0);
        assert_eq!(stand(&mut standing, Vec3::xz(1200.0, 1000.0), 300), 10.0);
        assert_eq!(stand(&mut standing, base, 600), 20.0);
        // Elsewhere: its own clock.
        assert_eq!(stand(&mut standing, Vec3::xz(2000.0, 1000.0), 600), 0.0);
        // Capped at the horizon.
        assert_eq!(stand(&mut standing, base, 600 + 30 * 100), HORIZON);
        // Left alone past the gap: starts over.
        let frame = 600 + 30 * 100 + DWELL_GAP + 1;
        standing.retain(|&(_, _, last)| frame - last <= DWELL_GAP);
        assert_eq!(stand(&mut standing, base, frame), 0.0);
    }

    #[test]
    fn a_dearer_place_takes_the_slot_of_the_cheapest_unstarted_turret() {
        let open = [(1, 700.0, false), (2, 600.0, false), (3, 500.0, true)];
        // The factory (2000): the cheapest unstarted goes, not the started one.
        assert_eq!(bump(&open, 2000.0), Some(2));
        // Not dear enough against either.
        assert_eq!(bump(&open, 850.0), None);
        assert_eq!(bump(&open, 900.0), Some(2));
        // Everything started: no slot.
        assert_eq!(bump(&[(1, 600.0, true)], 5000.0), None);
    }

    #[test]
    fn distance_to_a_route() {
        let line = [Vec3::xz(0.0, 0.0), Vec3::xz(1000.0, 0.0), Vec3::xz(1000.0, 1000.0)];
        assert!((to_line(&line, Vec3::xz(500.0, 200.0)) - 200.0).abs() < 1e-3);
        assert!((to_line(&line, Vec3::xz(1300.0, 500.0)) - 300.0).abs() < 1e-3);
        assert!((to_line(&line, Vec3::xz(-300.0, -400.0)) - 500.0).abs() < 1e-3);
        assert!((to_line(&[Vec3::xz(0.0, 0.0)], Vec3::xz(3.0, 4.0)) - 5.0).abs() < 1e-3);
    }
}
