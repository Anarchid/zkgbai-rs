//! Stakeouts: scout-class units (Flea `spiderscout`, Dart `vehscout`,
//! Dirtbag `shieldscout`) as pickets on the lanes to our economy.
//!
//! On a map of radar-shadowing canyons a raid walks unseen until it is on
//! a mex, and the home defense force (see `qrf`) cannot be dispatched in
//! time. A picket at the gate the raid must pass sees it minutes earlier:
//! the enemy is then in our sight, so every module that reads enemy tracks
//! (the QRF, the incursion planner, the threat map) knows of it, with no
//! message from here. The Flea is the natural picket: 25 metal, sight 620,
//! and it cloaks for free while it stands still (`idle_cloak`), decloaking
//! only within 130 of an enemy. Its gun reaches 150, so anything it could
//! shoot has already decloaked it: pickets fire at will.
//!
//! A stakeout's life:
//!
//! - **Look.** The first scouts go and look: at the enemy's start first
//!   (what factory, which way they expand), then wherever enemy ground has
//!   gone unseen longest. Later, while every post is manned, a spare scout
//!   keeps looking.
//! - **Post.** Once the terrain's strategic analysis knows which
//!   chokepoints every enemy ground route to our metal spots passes (a set
//!   cover of our spots by gates no wider than static defense could hold),
//!   a scout walks to each post and stands: beside the gate on our side,
//!   farther off the raid's path than its decloak radius, holding
//!   position. On open maps with no such gates there are no posts, and the
//!   scouts only look.
//! - **Shadow.** Any armed enemy that comes close is kept at a distance:
//!   outside its weapon range, inside our sight. A picket at least as fast
//!   as the enemy follows it when it moves off, and so keeps it in sight for
//!   the defenders; a slower one only backs away and returns to its post
//!   when the enemy is gone. The retreat point is a place, never the
//!   nearest "haven": the raider handler's haven rule made solo raiders
//!   flip-flop at the rim of a threat disc.
//! - **Sortie.** A defenceless constructor or mex in sight with no enemy
//!   fire about is killed, then the picket goes back to its post.
//!
//! The factory keeps a scout short of a post or a looker (see
//! `fac_assign_factory_task`), unless pickets have been dying fast lately: a
//! lane that eats pickets is not a lane to picket. `disable=stakeouts`
//! leaves the scouts to the factory's old scout budget (which never gave
//! them a handler: they stood idle at the factory).

use std::collections::{BTreeMap, VecDeque};

use serde_json::json;
use springai::{Unit, UnitDef, Vec3};
use zk_terrain::paths::SNAP;
use zk_terrain::strategy::MAX_HOLD_WIDTH;
use zk_terrain::{Pos, TierMap};

use crate::Ai;
use crate::economy::factory::FacKind;
use crate::military::{Enemy, Fighter};
use crate::nav;
use crate::terrain::pos;
use crate::util::{NULLPOS, distance, get_directional_point};

/// The units taken as stakeouts.
pub const SCOUTS: [&str; 3] = ["spiderscout", "vehscout", "shieldscout"];

/// Frames between a stakeout's ticks.
const PERIOD: i32 = 15;

/// Lookers wanted before this frame (the opening).
const LOOK_UNTIL: i32 = 30 * 60 * 8;
/// Lookers wanted in the opening, and after it.
const LOOKERS_EARLY: usize = 2;
const LOOKERS_LATE: usize = 1;

/// Posts manned at most.
const MAX_POSTS: usize = 4;

/// A post stands this far behind its gate toward home...
const POST_SETBACK: f32 = 150.0;
/// ...and this far off the raid's path beyond the gate's half-width (the
/// Flea decloaks within 130).
const POST_OFFSET: f32 = 160.0;
/// Off the path at most this far (sight must still cover the gate).
const MAX_OFFSET: f32 = 460.0;

/// A stakeout has arrived within this of where it was going.
const ARRIVE: f32 = 80.0;

/// A picket that cannot cloak keeps this much more than an enemy's weapon
/// range, and at least `KEEP_MIN`...
const RANGE_MARGIN: f32 = 150.0;
const KEEP_MIN: f32 = 300.0;
/// ...one that cloaks when still cannot be seen standing, so it moves only
/// when an enemy would come this much closer than its decloak radius.
const DECLOAK_MARGIN: f32 = 100.0;
/// A static weapon (a turret) shoots a moving picket within its range plus
/// this, cloak or not: moving decloaks.
const TURRET_MARGIN: f32 = 100.0;
/// Backing off, a picket goes to this much inside its own sight of the
/// enemy: one decisive step, then it stands (and cloaks).
const BACK_TO: f32 = 60.0;
/// Following, it stops this much inside its own sight.
const FOLLOW_TO: f32 = 100.0;
/// Frames between follow moves (a step, then a look).
const FOLLOW_EVERY: i32 = 45;
/// A spot a looker fled from is left alone this long (frames).
const SHUN: i32 = 30 * 120;

/// Enemies seen within this many frames are where we saw them.
const RECENT: i32 = 45;
/// Soft targets seen within this many frames are worth a sortie.
const SORTIE_RECENT: i32 = 90;

/// A stakeout follows an enemy at least this share of its speed.
const SPEED_PARITY: f32 = 0.95;

/// A looked-at spot counts as fresh this long (frames).
const FRESH: i32 = 30 * 60;
/// A minute unseen is worth this many seconds of walking.
const STALE_WORTH: f32 = 0.25;
/// The enemy's start area (near the centroid of their presumed territory)
/// is worth this many seconds of walking while unseen in the opening.
const START_WORTH: f32 = 20.0;
const START_RADIUS: f32 = 900.0;

/// With this many stakeouts lost within [`LOSS_RECALL`] frames, no more
/// are asked for.
const LOSS_CAP: usize = 4;
const LOSS_RECALL: i32 = 30 * 60 * 5;

/// Frames between journal summaries (the analysts read the posts' manning
/// and the lookers' targets from them).
const SUMMARY_PERIOD: i32 = 30 * 30;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Duty {
    Look,
    Post,
    Shadow,
}

impl Duty {
    fn name(self) -> &'static str {
        match self {
            Duty::Look => "look",
            Duty::Post => "post",
            Duty::Shadow => "shadow",
        }
    }
}

struct Stakeout {
    f: Fighter,
    def: UnitDef,
    speed: f32,
    sight: f32,
    duty: Duty,
    /// Index into `posts`.
    post: Option<usize>,
    /// The enemy shadowed, and whether we are standing (orders issued
    /// only when it moves).
    bogey: Option<i32>,
    standing: bool,
    /// Metal spot being looked at.
    look: Option<usize>,
    /// A soft target under attack.
    sortie: Option<i32>,
    last_tick: i32,
    /// Frame of the last move order.
    moved: i32,
    /// Cloaks for free when it stands still (the Flea).
    cloaks: bool,
}

/// A gate to picket.
#[derive(Clone, Debug)]
pub struct Post {
    pub choke: usize,
    pub pos: Vec3,
    /// Our metal spots whose every enemy route passes the gate.
    pub covers: usize,
    pub width: f32,
    pub manned: Option<i32>,
}

#[derive(Default)]
struct Stats {
    claimed: u32,
    lost: u32,
    sightings: u32,
    flights: u32,
    follows: u32,
    sorties: u32,
    kills: u32,
}

#[derive(Default)]
pub struct Stakeouts {
    members: BTreeMap<i32, Stakeout>,
    pub posts: Vec<Post>,
    /// Strategy frame the posts were planned from.
    posts_frame: i32,
    losses: VecDeque<i32>,
    /// Metal spots lookers fled from, until a frame.
    shunned: BTreeMap<usize, i32>,
    stats: Stats,
}

impl Stakeouts {
    pub fn count(&self) -> usize {
        self.members.len()
    }

    pub fn contains(&self, id: i32) -> bool {
        self.members.contains_key(&id)
    }

    fn lookers(&self) -> usize {
        self.members.values().filter(|m| m.duty == Duty::Look).count()
    }

    fn lookers_wanted(frame: i32) -> usize {
        if frame < LOOK_UNTIL { LOOKERS_EARLY } else { LOOKERS_LATE }
    }

    /// Stakeouts wanted in all: a picket per post and the lookers.
    fn wanted(&self, frame: i32) -> usize {
        self.posts.len().min(MAX_POSTS) + Self::lookers_wanted(frame)
    }
}

/// Whether an enemy would shoot a stakeout.
fn armed(e: &Enemy) -> bool {
    !e.is_dead && (!e.identified || e.can_attack())
}

/// Whether an enemy is on the ground and moving about: one to shadow.
fn mobile_armed(e: &Enemy) -> bool {
    armed(e) && !e.is_static && !e.can_fly()
}

/// Whether an enemy is worth a sortie: it cannot shoot back.
fn soft(e: &Enemy) -> bool {
    !e.is_dead && e.identified && !e.can_attack() && (e.is_mex || e.is_worker || !e.is_static)
}

/// How close an enemy may come to a picket: a turret shoots a moving
/// picket within its range; a mobile enemy cannot see a cloaked one
/// standing, so only its coming within decloak distance matters; a picket
/// that cannot cloak stays out of weapon range.
fn keep_from(e: &Enemy, cloaks: bool) -> f32 {
    if e.is_static {
        e.threat_radius + TURRET_MARGIN
    } else if cloaks {
        e.ud.map_or(130.0, |d| d.decloak_distance().max(1.0)).max(130.0) + DECLOAK_MARGIN
    } else {
        (e.threat_radius + RANGE_MARGIN).max(KEEP_MIN)
    }
}

/// Where to stand beside a gate: `POST_SETBACK` toward home and off the
/// path by the gate's half-width plus `POST_OFFSET`, on passable ground of
/// the home region; the setback itself if neither side is.
fn post_position(map: &TierMap, choke: usize, home: usize) -> Pos {
    let c = &map.chokes[choke];
    let back = map.setback(choke, home, POST_SETBACK);
    let (dx, dz) = (back.x - c.pos.x, back.z - c.pos.z);
    let len = (dx * dx + dz * dz).sqrt();
    if len <= 0.0 {
        return back;
    }
    let (px, pz) = (-dz / len, dx / len);
    let off = (c.width / 2.0 + POST_OFFSET).min(MAX_OFFSET);
    for sign in [1.0, -1.0] {
        let p = Pos::new(back.x + px * off * sign, back.z + pz * off * sign);
        if map.is_passable(p) && map.region_at(p) == Some(home) {
            return p;
        }
    }
    back
}

impl Ai {
    /// Whether a new unit is a stakeout's.
    pub fn stakeouts_claims(&self, name: &str) -> bool {
        !self.dev_off("stakeouts") && SCOUTS.contains(&name)
    }

    pub fn stakeouts_add(&mut self, unit: Unit) {
        let Some(def) = unit.def() else { return };
        let cost = def.cost(self.metal);
        let f = Fighter::new_raider(unit, cost, self.team_id);
        // Hold position: a picket stands where it is put (and cloaks);
        // fire at will: anything in range has decloaked it already.
        unit.set_move_state(0, 0);
        unit.set_fire_state(2, 0);
        let frame = self.frame;
        self.stakeouts.members.insert(
            unit.id(),
            Stakeout {
                f,
                def,
                speed: def.speed(),
                sight: def.los_radius(),
                duty: Duty::Look,
                post: None,
                bogey: None,
                standing: false,
                look: None,
                sortie: None,
                last_tick: frame - PERIOD,
                moved: 0,
                cloaks: def.is_able_to_cloak() && def.cloak_cost() <= 0.0,
            },
        );
        self.stakeouts.stats.claimed += 1;
    }

    pub fn stakeouts_remove_unit(&mut self, unit: Unit, attacker: Option<Unit>) {
        let id = unit.id();
        let Some(m) = self.stakeouts.members.remove(&id) else { return };
        if let Some(p) = m.post.and_then(|i| self.stakeouts.posts.get_mut(i)) {
            p.manned = None;
        }
        let frame = self.frame;
        let s = &mut self.stakeouts;
        s.stats.lost += 1;
        s.losses.push_back(frame);
        while s.losses.front().is_some_and(|f| frame - f > LOSS_RECALL) {
            s.losses.pop_front();
        }
        let recent = s.losses.len();
        let pos = unit.pos();
        let attacker = attacker.map(|a| {
            let ap = a.pos();
            json!({"unit": a.id(), "def": self.defs.name_of(a), "x": ap.x.round(), "z": ap.z.round()})
        });
        self.op_emit(
            "stakeout",
            json!({"event": "lost", "t": frame / 30, "unit": id, "def": self.defs.name(m.def), "duty": m.duty.name(),
                   "post": m.post, "x": pos.x.round(), "z": pos.z.round(), "attacker": attacker, "recent_losses": recent}),
        );
    }

    /// Whether the stakeouts decide how many scouts a factory of this kind
    /// builds (its scout is one of theirs).
    pub(crate) fn stakeouts_own(&self, kind: FacKind) -> bool {
        !self.dev_off("stakeouts") && matches!(kind, FacKind::Spider | FacKind::Veh)
    }

    /// How many scouts a factory of this kind should build for the
    /// stakeouts: what they are short of, if its scout is one of theirs.
    pub(crate) fn stakeouts_short(&self, kind: FacKind) -> i32 {
        if !self.stakeouts_own(kind) {
            return 0;
        }
        let s = &self.stakeouts;
        if s.losses.len() >= LOSS_CAP {
            return 0;
        }
        (s.wanted(self.frame) as i32 - s.count() as i32).max(0)
    }

    pub fn stakeouts_update(&mut self, frame: i32) {
        if self.dev_off("stakeouts") {
            return;
        }
        // Posts are planned before any scout exists: the factory builds
        // for them.
        if frame % 30 == (self.offset + 11) % 30 {
            self.stakeouts_plan_posts(frame);
        }
        if self.stakeouts.members.is_empty() {
            return;
        }
        // One stakeout per frame, each every PERIOD frames.
        let due = (self.stakeouts.members.iter())
            .filter(|(_, m)| frame - m.last_tick >= PERIOD)
            .min_by_key(|(_, m)| m.last_tick)
            .map(|(&id, _)| id);
        if let Some(id) = due {
            if let Some(m) = self.stakeouts.members.get_mut(&id) {
                m.last_tick = frame;
            }
            self.stakeouts_tick(id, frame);
        }
        if frame % SUMMARY_PERIOD == (self.offset + 17) % SUMMARY_PERIOD {
            self.stakeouts_summary(frame);
        }
    }

    /// Plan the posts from the strategic analysis: for each region holding
    /// our metal spots, the gate every enemy ground route into it passes
    /// (the first on our side of the map, else the one the enemy enters
    /// by), then the gates covering most spots first.
    fn stakeouts_plan_posts(&mut self, frame: i32) {
        let planned = {
            let Some(strategy) = self.strategy.as_ref() else { return };
            if strategy.frame == self.stakeouts.posts_frame {
                return;
            }
            let Some(map) = self.terrain().and_then(|t| t.tier(strategy.tier)) else { return };
            let a = &strategy.analysis;
            let (Some(_), Some(theirs)) = (a.ally_base, a.enemy_base) else { return };

            // Our spots per region.
            let mut spots: BTreeMap<usize, usize> = BTreeMap::new();
            for s in &self.graph.metal_spots {
                if (s.owned || (s.ally_shadowed && !s.hostile))
                    && let Some(r) = map.region_near(pos(s.position), SNAP)
                {
                    *spots.entry(r).or_default() += 1;
                }
            }
            let gate = |c: usize| {
                let ch = &map.chokes[c];
                ch.width <= MAX_HOLD_WIDTH && ch.regions.len() == 2
            };
            let from_enemy = |x: &usize, y: &usize| {
                a.choke_distance(map, *x, false).total_cmp(&a.choke_distance(map, *y, false))
            };
            let mut covers: BTreeMap<usize, usize> = BTreeMap::new();
            for (&r, &n) in &spots {
                if r == theirs {
                    continue;
                }
                // The first gate on our side of the map that every enemy
                // route passes: the earliest warning on our own ground.
                let ours = (a.guards_vs_enemy[r].iter().copied())
                    .filter(|&c| gate(c) && a.choke_on_our_side(map, c))
                    .min_by(from_enemy);
                // Else the gate the enemy enters the region by.
                let entry = || (map.regions[r].chokes.iter().copied()).filter(|&c| gate(c)).min_by(from_enemy);
                let Some(c) = ours.or_else(entry) else { continue };
                *covers.entry(c).or_default() += n;
            }
            // Posts already manned stay while their gate still covers
            // something: a picket is not walked from gate to gate as the
            // analysis shifts. The rest by spots covered.
            let kept: Vec<usize> = (self.stakeouts.posts.iter()).map(|p| p.choke).filter(|c| covers.contains_key(c)).collect();
            let mut chosen: Vec<(usize, usize)> = kept.iter().map(|&c| (c, covers[&c])).collect();
            let mut rest: Vec<(usize, usize)> = covers.iter().filter(|(c, _)| !kept.contains(c)).map(|(&c, &n)| (c, n)).collect();
            rest.sort_by(|x, y| y.1.cmp(&x.1).then(x.0.cmp(&y.0)));
            chosen.extend(rest);
            chosen.truncate(MAX_POSTS);

            let elevation = self.cb.map();
            let posts: Vec<(usize, usize, Vec3, f32)> = (chosen.into_iter())
                .filter_map(|(c, n)| {
                    let home = a.home_side(map, c)?;
                    let p = post_position(map, c, home);
                    Some((c, n, Vec3::new(p.x, elevation.elevation_at(p.x, p.z), p.z), map.chokes[c].width))
                })
                .collect();
            (strategy.frame, posts)
        };
        let (strategy_frame, planned) = planned;

        let old = std::mem::take(&mut self.stakeouts.posts);
        let posts: Vec<Post> = (planned.into_iter())
            .map(|(c, n, p, width)| Post {
                choke: c,
                pos: p,
                covers: n,
                width,
                manned: old.iter().find(|o| o.choke == c).and_then(|o| o.manned),
            })
            .collect();
        // Pickets whose post is gone go back to looking.
        for m in self.stakeouts.members.values_mut() {
            m.post = m.post.and_then(|i| {
                let c = old.get(i)?.choke;
                posts.iter().position(|p| p.choke == c)
            });
            if m.post.is_none() && m.duty == Duty::Post {
                m.duty = Duty::Look;
                m.standing = false;
            }
        }
        let changed = posts.len() != old.len() || posts.iter().zip(&old).any(|(p, o)| p.choke != o.choke);
        self.stakeouts.posts = posts;
        self.stakeouts.posts_frame = strategy_frame;
        if changed {
            let list: Vec<String> = (self.stakeouts.posts.iter())
                .map(|p| {
                    format!("gate {} ({:.0} wide, {} spots) at {:.0},{:.0}", p.choke, p.width, p.covers, p.pos.x, p.pos.z)
                })
                .collect();
            self.log(&format!("stakeouts: {} posts: {}", list.len(), list.join("; ")));
            let v: Vec<_> = (self.stakeouts.posts.iter())
                .map(|p| {
                    json!({"choke": p.choke, "x": p.pos.x.round(), "z": p.pos.z.round(), "covers": p.covers, "width": p.width.round()})
                })
                .collect();
            self.op_emit("stakeout", json!({"event": "posts", "t": frame / 30, "posts": v}));
        }
    }

    fn stakeouts_tick(&mut self, id: i32, frame: i32) {
        let Some(m) = self.stakeouts.members.get(&id) else { return };
        if m.f.is_dead() {
            self.stakeouts_remove_unit(m.f.unit, None);
            return;
        }
        let (unit, p, sight, speed, def, duty) = (m.f.unit, m.f.get_pos(), m.sight, m.speed, m.def, m.duty);
        let (cloaks, look) = (m.cloaks, m.look);

        // The nearest armed enemy that could shoot us, else the nearest
        // mobile one in or near our sight.
        let mut danger: Option<(i32, f32)> = None;
        let mut near: Option<(i32, f32)> = None;
        for e in self.war.targets.values() {
            if !armed(e) || frame - e.last_seen > RECENT {
                continue;
            }
            let d = distance(p, e.position);
            let keep = keep_from(e, cloaks);
            if d < keep && danger.is_none_or(|(_, b)| d - keep < b) {
                danger = Some((e.unit_id, d - keep));
            }
            if mobile_armed(e) && d < sight + 300.0 && near.is_none_or(|(_, b)| d < b) {
                near = Some((e.unit_id, d));
            }
        }
        if let Some((eid, _)) = danger.or(near) {
            // A looker that fled leaves that spot alone for a while.
            if danger.is_some()
                && duty == Duty::Look
                && let Some(spot) = look
            {
                self.stakeouts.shunned.insert(spot, frame + SHUN);
                if let Some(m) = self.stakeouts.members.get_mut(&id) {
                    m.look = None;
                }
            }
            self.stakeouts_shadow(id, eid, frame);
            return;
        }

        // Nothing about: back from shadowing to the duty we had.
        if duty == Duty::Shadow && let Some(m) = self.stakeouts.members.get_mut(&id) {
            m.duty = if m.post.is_some() { Duty::Post } else { Duty::Look };
            m.bogey = None;
            m.standing = false;
        }

        // A sortie against something defenceless in sight.
        let sortie = (self.war.targets.values())
            .filter(|e| soft(e) && frame - e.last_seen <= SORTIE_RECENT)
            .filter(|e| self.war.get_threat(e.position) <= 0.0 && self.war.get_porc_threat(e.position) <= 0.0)
            .map(|e| (e.unit_id, distance(p, e.position)))
            .filter(|&(_, d)| d < sight)
            .min_by(|a, b| a.1.total_cmp(&b.1));
        if let Some((eid, _)) = sortie {
            let target = self.war.targets[&eid].position;
            let new = self.stakeouts.members.get(&id).is_some_and(|m| m.sortie != Some(eid));
            if new {
                self.stakeouts.stats.sorties += 1;
            }
            let mut nav = nav!(self);
            if let Some(m) = self.stakeouts.members.get_mut(&id) {
                m.sortie = Some(eid);
                m.standing = false;
                m.f.raid(target, &mut nav);
            }
            return;
        }
        if let Some(m) = self.stakeouts.members.get_mut(&id)
            && m.sortie.take().is_some()
        {
            self.stakeouts.stats.kills += 1;
        }

        self.stakeouts_assign(id, frame);
        let Some(m) = self.stakeouts.members.get(&id) else { return };
        let (duty, standing, post) = (m.duty, m.standing, m.post.and_then(|i| self.stakeouts.posts.get(i)).map(|p| p.pos));
        match duty {
            Duty::Post => {
                let Some(target) = post else { return };
                if distance(p, target) <= ARRIVE {
                    if !standing {
                        unit.stop(0);
                        if let Some(m) = self.stakeouts.members.get_mut(&id) {
                            m.standing = true;
                        }
                    }
                } else {
                    let mut nav = nav!(self);
                    if let Some(m) = self.stakeouts.members.get_mut(&id) {
                        m.standing = false;
                        m.moved = frame;
                        m.f.sneak(target, &mut nav);
                    }
                }
            }
            Duty::Look => {
                let Some(spot) = self.stakeouts_look_target(id, p, speed, def, frame) else { return };
                let target = self.graph.metal_spots[spot].position;
                let mut nav = nav!(self);
                if let Some(m) = self.stakeouts.members.get_mut(&id) {
                    m.look = Some(spot);
                    m.standing = false;
                    m.moved = frame;
                    m.f.sneak(target, &mut nav);
                }
            }
            Duty::Shadow => {}
        }
    }

    /// Give a stakeout its duty: a post while one is unmanned and enough
    /// others look, else looking.
    fn stakeouts_assign(&mut self, id: i32, frame: i32) {
        let Some(m) = self.stakeouts.members.get(&id) else { return };
        if m.duty == Duty::Post && m.post.is_some_and(|i| self.stakeouts.posts.get(i).is_some_and(|p| p.manned == Some(id))) {
            return;
        }
        let lookers = self.stakeouts.lookers() - usize::from(m.duty == Duty::Look);
        let (p, def) = (m.f.get_pos(), m.def);
        let want_post = lookers >= Stakeouts::lookers_wanted(frame);
        let post = if want_post {
            (self.stakeouts.posts.iter().enumerate())
                .filter(|(_, post)| post.manned.is_none_or(|u| !self.stakeouts.members.contains_key(&u)))
                .filter(|(_, post)| self.terrain_reaches(def, p, post.pos, SNAP))
                .min_by(|a, b| distance(p, a.1.pos).total_cmp(&distance(p, b.1.pos)))
                .map(|(i, _)| i)
        } else {
            None
        };
        let m = self.stakeouts.members.get_mut(&id).unwrap();
        let before = m.duty;
        match post {
            Some(i) => {
                if let Some(old) = m.post.filter(|&o| o != i).and_then(|o| self.stakeouts.posts.get_mut(o)) {
                    old.manned = None;
                }
                m.post = Some(i);
                m.duty = Duty::Post;
                m.standing = false;
                self.stakeouts.posts[i].manned = Some(id);
            }
            None => {
                if let Some(old) = m.post.take().and_then(|o| self.stakeouts.posts.get_mut(o)) {
                    old.manned = None;
                }
                m.duty = Duty::Look;
            }
        }
        let (duty, post) = (m.duty, m.post);
        if duty != before {
            self.op_emit("stakeout", json!({"event": "duty", "t": frame / 30, "unit": id, "duty": duty.name(), "post": post}));
        }
    }

    /// Keep an enemy at a distance: outside its reach, inside our sight;
    /// follow it if we are as fast, else let it go.
    fn stakeouts_shadow(&mut self, id: i32, eid: i32, frame: i32) {
        let Some(e) = self.war.targets.get(&eid) else { return };
        let (epos, espeed, ename) = (e.position, e.speed(), e.def_name.clone());
        let Some(m) = self.stakeouts.members.get(&id) else { return };
        let (p, sight, speed, def, cloaks, moved) = (m.f.get_pos(), m.sight, m.speed, m.def, m.cloaks, m.moved);
        let d = distance(p, epos);
        let keep = keep_from(e, cloaks);
        // Back off to the edge of our sight, or farther if its reach leaves
        // no safe band inside it (then we lose it rather than die to it).
        let back_to = (sight - BACK_TO).max(keep + 150.0);
        let follow_to = sight - FOLLOW_TO;
        let follows = speed >= SPEED_PARITY * espeed && !e.is_static && keep + 150.0 <= follow_to;
        let new = m.bogey != Some(eid);
        if new {
            self.stakeouts.stats.sightings += 1;
            self.op_emit(
                "stakeout",
                json!({"event": "sighting", "t": frame / 30, "unit": id, "enemy": eid, "def": ename,
                       "x": epos.x.round(), "z": epos.z.round(), "dist": d.round(), "follows": follows}),
            );
        }
        let start = self.graph.get_start_pos();
        let target = if d < keep {
            // One decisive step back along the line away from it; toward
            // home if that ground is not ours to walk.
            let away = get_directional_point(epos, p, back_to);
            let away = if self.terrain_reaches(def, p, away, SNAP) || start == NULLPOS {
                away
            } else {
                get_directional_point(p, start, back_to - d)
            };
            Some((away, true))
        } else if d > sight - BACK_TO + 40.0 && follows && frame - moved >= FOLLOW_EVERY {
            Some((get_directional_point(p, epos, d - follow_to), false))
        } else {
            None
        };
        if let Some(m) = self.stakeouts.members.get_mut(&id) {
            if m.duty != Duty::Shadow {
                m.duty = Duty::Shadow;
                m.sortie = None;
            }
            m.bogey = Some(eid);
        }
        match target {
            Some((t, flight)) => {
                let s = &mut self.stakeouts.stats;
                if flight {
                    s.flights += 1;
                } else {
                    s.follows += 1;
                }
                let mut nav = nav!(self);
                if let Some(m) = self.stakeouts.members.get_mut(&id) {
                    m.standing = false;
                    m.moved = frame;
                    m.f.sneak(t, &mut nav);
                }
            }
            None => {
                if let Some(m) = self.stakeouts.members.get_mut(&id)
                    && !m.standing
                {
                    m.f.unit.stop(0);
                    m.standing = true;
                }
            }
        }
    }

    /// Where to look next: enemy ground unseen longest, nearest first; the
    /// enemy's start area while it is unseen in the opening.
    fn stakeouts_look_target(&self, id: i32, p: Vec3, speed: f32, def: UnitDef, frame: i32) -> Option<usize> {
        let spots = &self.graph.metal_spots;
        let enemy: Vec<usize> =
            (spots.iter().enumerate()).filter(|(_, s)| s.hostile || s.enemy_shadowed).map(|(i, _)| i).collect();
        let pool: Vec<usize> = if enemy.is_empty() {
            (spots.iter().enumerate()).filter(|(_, s)| !s.owned && !s.ally_shadowed).map(|(i, _)| i).collect()
        } else {
            enemy
        };
        if pool.is_empty() {
            return None;
        }
        let n = pool.len() as f32;
        let centre = pool.iter().fold(Vec3::ZERO, |c, &i| {
            Vec3::new(c.x + spots[i].position.x / n, 0.0, c.z + spots[i].position.z / n)
        });
        // Keep the spot we are going to until it is fresh.
        let current = self.stakeouts.members.get(&id).and_then(|m| m.look);
        let shunned_now = |i: usize| self.stakeouts.shunned.get(&i).is_some_and(|&until| until > frame);
        if let Some(i) = current.filter(|&i| pool.contains(&i) && frame - spots[i].last_seen > FRESH && !shunned_now(i)) {
            return Some(i);
        }
        let taken: Vec<usize> =
            (self.stakeouts.members.iter()).filter(|(o, _)| **o != id).filter_map(|(_, m)| m.look).collect();
        let shunned = |i: usize| self.stakeouts.shunned.get(&i).is_some_and(|&until| until > frame);
        let speed = speed.max(1.0);
        let mut best: Option<(usize, f32)> = None;
        for &i in &pool {
            let s = &spots[i];
            if frame - s.last_seen <= FRESH || taken.contains(&i) || shunned(i) {
                continue;
            }
            if self.war.get_porc_threat(s.position) > 0.0 || !self.terrain_reaches(def, p, s.position, SNAP) {
                continue;
            }
            let stale = ((frame - s.last_seen) as f32 / 30.0).min(120.0);
            let mut cost = distance(p, s.position) / speed - stale * STALE_WORTH;
            if frame < LOOK_UNTIL && distance(s.position, centre) < START_RADIUS {
                cost -= START_WORTH;
            }
            if best.is_none_or(|(_, b)| cost < b) {
                best = Some((i, cost));
            }
        }
        best.map(|(i, _)| i).or(current)
    }

    fn stakeouts_summary(&mut self, frame: i32) {
        self.stakeouts.shunned.retain(|_, until| *until > frame);
        let s = &self.stakeouts;
        let (mut look, mut post, mut shadow) = (0, 0, 0);
        for m in s.members.values() {
            match m.duty {
                Duty::Look => look += 1,
                Duty::Post => post += 1,
                Duty::Shadow => shadow += 1,
            }
        }
        let manned = s.posts.iter().filter(|p| p.manned.is_some()).count();
        let st = &s.stats;
        self.log(&format!(
            "stakeouts: t={} members={} (look {look}, post {post}, shadow {shadow}) posts={} manned={manned} wanted={} \
             claimed={} lost={} sightings={} flights={} follows={} sorties={} kills={}",
            frame / 30,
            s.count(),
            s.posts.len(),
            s.wanted(frame),
            st.claimed,
            st.lost,
            st.sightings,
            st.flights,
            st.follows,
            st.sorties,
            st.kills
        ));
        let posts: Vec<_> = (s.posts.iter())
            .map(|p| {
                json!({"choke": p.choke, "x": p.pos.x.round(), "z": p.pos.z.round(), "covers": p.covers,
                       "width": p.width.round(), "manned": p.manned})
            })
            .collect();
        let members: Vec<_> = (s.members.iter())
            .map(|(&id, m)| {
                let p = m.f.get_pos();
                let look = m.look.and_then(|i| self.graph.metal_spots.get(i)).map(|sp| {
                    json!({"spot": m.look, "x": sp.position.x.round(), "z": sp.position.z.round()})
                });
                json!({"unit": id, "def": self.defs.name(m.def), "duty": m.duty.name(), "post": m.post, "look": look,
                       "bogey": m.bogey, "sortie": m.sortie, "standing": m.standing, "cloaked": m.f.unit.is_cloaked(),
                       "x": p.x.round(), "z": p.z.round()})
            })
            .collect();
        self.op_emit(
            "stakeout",
            json!({"event": "summary", "t": frame / 30, "members": s.count(), "look": look, "post": post, "shadow": shadow,
                   "posts": posts, "manned": manned, "wanted": s.wanted(frame), "pickets": members,
                   "claimed": st.claimed, "lost": st.lost, "sightings": st.sightings, "flights": st.flights,
                   "follows": st.follows, "sorties": st.sorties, "kills": st.kills}),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wanted_counts_posts_and_lookers() {
        let mut s = Stakeouts::default();
        assert_eq!(s.wanted(0), LOOKERS_EARLY);
        assert_eq!(s.wanted(LOOK_UNTIL), LOOKERS_LATE);
        for c in 0..6 {
            s.posts.push(Post { choke: c, pos: Vec3::ZERO, covers: 1, width: 300.0, manned: None });
        }
        assert_eq!(s.wanted(0), MAX_POSTS + LOOKERS_EARLY);
    }

    #[test]
    fn duty_names() {
        assert_eq!(Duty::Look.name(), "look");
        assert_eq!(Duty::Post.name(), "post");
        assert_eq!(Duty::Shadow.name(), "shadow");
    }
}
