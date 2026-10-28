//! Stakeouts: scout-class units (Flea `spiderscout`, Dart `vehscout`,
//! Dirtbag `shieldscout`), and other factories' cheapest fast units (see
//! [`stakeout_unit`]), as pickets on the lanes to our economy.
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
//! Only a picket that cloaks standing still (the Flea) shadows. One that
//! cannot runs like any coward when an enemy comes within its reach: to
//! the nearest haven, without turning back for a while; and the retreat
//! handler pulls it home at the first scratch.
//!
//! The factory keeps a scout short of a post or a looker (see
//! `fac_assign_factory_task`), unless pickets have been dying fast lately: a
//! lane that eats pickets is not a lane to picket. Spider and vehicle
//! factories build scouts only for the stakeouts; others build for them
//! while they are short (feature `anyscout`), the opening scout included,
//! which so looks for the enemy rather than waiting at home. An opening
//! jump, tank or ship factory of a 1v1 builds a wave of them first while
//! the map is sparse, then a wave more per so many raiders (feature
//! `openscouts`, see `fac_opening_scout`). Either way the unit comes from
//! the factory's [`scout_roster`]. A unit built for the stakeouts is theirs alone (see
//! `roles`). `disable=stakeouts` leaves the scouts to the factory's old
//! scout budget.

use std::collections::{BTreeMap, VecDeque};

use serde_json::json;
use springai::{Unit, UnitDef, Vec3};
use zk_terrain::paths::SNAP;
use zk_terrain::strategy::MAX_HOLD_WIDTH;
use zk_terrain::{Pos, TierMap};

use crate::Ai;
use crate::economy::factory::FacKind;
use crate::military::puppies;
use crate::military::roles::Role;
use crate::military::{Enemy, Fighter};
use crate::nav;
use crate::terrain::pos;
use crate::util::{NULLPOS, distance, get_directional_point, kgb_rand};

/// The units taken as stakeouts.
pub const SCOUTS: [&str; 3] = ["spiderscout", "vehscout", "shieldscout"];

/// The scout roster: what a factory of `kind` builds for the stakeouts,
/// its specialist cheap scout where it has one, else its cheapest raider
/// (the Kodachi, cheap or not); two where either will do. None for the
/// air factories (we do not open with them) and the strider hub.
pub(crate) fn scout_roster(kind: FacKind) -> &'static [&'static str] {
    match kind {
        FacKind::Cloak => &["cloakraid"],
        FacKind::Shield => &["shieldscout", "shieldraid"],
        FacKind::Veh => &["vehscout"],
        FacKind::Hover => &["hoverraid"],
        FacKind::Tank => &["tankraid"],
        FacKind::Amph => &["amphraid"],
        FacKind::Jump => &[puppies::PUPPY],
        FacKind::Spider => &["spiderscout"],
        FacKind::Ship => &["shipscout", "shiptorpraider"],
        _ => &[],
    }
}

/// Whether a factory of `kind` opens with a wave of scouts for the
/// stakeouts (see `fac_opening_scout`): the jumpjets, tanks and ships,
/// whose scouts the stakeouts did not take before the roster. The others
/// keep the stakeouts' own numbers.
pub(crate) fn opens_with_scouts(kind: FacKind) -> bool {
    matches!(kind, FacKind::Jump | FacKind::Tank | FacKind::Ship)
}

/// The unit a factory of `kind` builds for the stakeouts: from its scout
/// roster, rolled where it has two.
pub(crate) fn stakeout_unit(kind: FacKind) -> Option<&'static str> {
    let roster = scout_roster(kind);
    let i = (kgb_rand() * roster.len() as f64) as usize;
    roster.get(i.min(roster.len().saturating_sub(1))).copied()
}

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

/// Enemies seen within this many frames are where we saw them...
const RECENT: i32 = 45;
/// ...and the enemy a picket shadows is followed for this long after it
/// left our sight (toward where it was last seen).
const BOGEY_RECENT: i32 = 120;
/// Soft targets seen within this many frames are worth a sortie.
const SORTIE_RECENT: i32 = 90;

/// A cloaking stakeout follows an enemy with this much speed in hand
/// (standing again, it vanishes; a chase at equal speed is a death); one
/// that cannot cloak does not shadow at all (it runs, see `stakeouts_flee`).
const SPEED_PARITY: f32 = 1.05;

/// A looked-at spot counts as fresh this long (frames).
const FRESH: i32 = 30 * 60;
/// A minute unseen is worth this many seconds of walking.
const STALE_WORTH: f32 = 0.25;
/// The enemy's start area (near the centroid of their presumed territory)
/// is worth this many seconds of walking while unseen in the opening.
const START_WORTH: f32 = 20.0;
const START_RADIUS: f32 = 900.0;

/// With this many stakeouts lost within [`LOSS_RECALL`] frames, no more
/// are asked for...
const LOSS_CAP: usize = 4;
const LOSS_RECALL: i32 = 30 * 60 * 5;
/// ...and none within this many frames of the last loss: the lane that
/// killed the picket is still hot.
const REPLACE_DELAY: i32 = 30 * 90;
/// A commander's drones reach this far beyond its own weapons.
const DRONE_REACH: f32 = 400.0;

/// Frames between journal summaries (the analysts read the posts' manning
/// and the lookers' targets from them).
const SUMMARY_PERIOD: i32 = 30 * 30;

/// A picket that cannot cloak runs from an enemy for this long (frames)
/// before it looks again...
const FLEE_FOR: i32 = 30 * 8;
/// ...and its courage outside a squad: with the retreat level of a light
/// unit (0.5), it runs home for repairs below 95% health.
const PICKET_COURAGE: f32 = 0.5;

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
    /// Cloaks for free when it stands still (the Flea: ZK's idle cloak,
    /// a gadget, not the engine's `canCloak`).
    cloaks: bool,
    /// Our own decloak radius: an enemy this close uncloaks us.
    decloak: f32,
    /// Has a weapon (sorties); the Dirtbag has none.
    armed: bool,
    /// Running from an enemy until this frame (pickets that cannot cloak).
    fleeing_until: i32,
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

    /// Whether [`LOSS_CAP`] pickets died within [`LOSS_RECALL`]. (The
    /// losses are pruned only at the next loss, which a cap that stops
    /// building pickets would never bring.)
    fn capped(&self, frame: i32) -> bool {
        self.losses.iter().filter(|&&l| frame - l <= LOSS_RECALL).count() >= LOSS_CAP
    }

    /// Whether the loss cap holds, or a picket died within [`REPLACE_DELAY`].
    fn hot(&self, frame: i32) -> bool {
        self.capped(frame) || self.losses.back().is_some_and(|&l| frame - l < REPLACE_DELAY)
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
/// standing, so only its coming within our decloak radius (`cloaks`)
/// matters; a picket that cannot cloak stays out of weapon range.
fn keep_from(e: &Enemy, cloaks: Option<f32>) -> f32 {
    if e.is_static {
        e.threat_radius + TURRET_MARGIN
    } else if e.is_com {
        e.threat_radius + DRONE_REACH
    } else if let Some(decloak) = cloaks {
        decloak + DECLOAK_MARGIN
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
                cloaks: def.custom_param("idle_cloak").is_some() || (def.is_able_to_cloak() && def.cloak_cost() <= 0.0),
                decloak: Some(def.decloak_distance()).filter(|&d| d > 0.0).unwrap_or(130.0),
                armed: !def.weapon_mounts().is_empty(),
                fleeing_until: 0,
            },
        );
        // One that cannot cloak is a coward that runs at the first scratch.
        if !self.stakeouts.members[&unit.id()].cloaks {
            self.retreat_add_coward(unit);
            self.retreat_set_base_courage(unit.id(), PICKET_COURAGE);
        }
        self.stakeouts.stats.claimed += 1;
        if self.stakeouts.stats.claimed == 1 {
            self.log(&format!(
                "stakeouts: first picket {} (sight {:.0}, speed {:.0}, cloaks when still: {}, decloak {:.0})",
                self.defs.name(def),
                def.los_radius(),
                def.speed(),
                self.stakeouts.members[&unit.id()].cloaks,
                self.stakeouts.members[&unit.id()].decloak
            ));
        }
    }

    pub fn stakeouts_remove_unit(&mut self, unit: Unit, attacker: Option<Unit>) {
        let id = unit.id();
        self.role_left_stakeouts(id);
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

    /// Let a stakeout go to another role (see `roles`): no loss.
    pub(crate) fn stakeouts_forget(&mut self, unit: Unit) {
        let Some(m) = self.stakeouts.members.remove(&unit.id()) else { return };
        if let Some(p) = m.post.and_then(|i| self.stakeouts.posts.get_mut(i)) {
            p.manned = None;
        }
        if !m.cloaks {
            self.retreat_remove_unit(unit);
        }
    }

    /// Whether the stakeouts decide how many scouts a factory of this kind
    /// builds (its scout is one of theirs).
    pub(crate) fn stakeouts_own(&self, kind: FacKind) -> bool {
        !self.dev_off("stakeouts") && matches!(kind, FacKind::Spider | FacKind::Veh)
    }

    /// How many units a factory of this kind should build for the
    /// stakeouts: what they are short of (less those being built), if its
    /// scout is one of theirs or, with `anyscout`, it has a unit for them.
    pub(crate) fn stakeouts_short(&self, kind: FacKind) -> i32 {
        let builds = self.stakeouts_own(kind)
            || (!self.dev_off("stakeouts") && !self.dev_off("anyscout") && !scout_roster(kind).is_empty());
        if !builds || self.stakeouts_hot() {
            return 0;
        }
        let s = &self.stakeouts;
        let coming = self.roles.declared_count(Role::Stakeout);
        (s.wanted(self.frame) as i32 - (s.count() + coming) as i32).max(0)
    }

    /// Whether pickets have been dying fast lately: none is to be built.
    pub(crate) fn stakeouts_hot(&self) -> bool {
        self.stakeouts.hot(self.frame)
    }

    /// Whether too many pickets died lately (the cap alone, without the
    /// delay after each loss).
    pub(crate) fn stakeouts_capped(&self) -> bool {
        self.stakeouts.capped(self.frame)
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
            let from_enemy =
                |x: &usize, y: &usize| a.choke_distance(map, *x, false).total_cmp(&a.choke_distance(map, *y, false));
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
            let kept: Vec<usize> =
                (self.stakeouts.posts.iter()).map(|p| p.choke).filter(|c| covers.contains_key(c)).collect();
            let mut chosen: Vec<(usize, usize)> = kept.iter().map(|&c| (c, covers[&c])).collect();
            let mut rest: Vec<(usize, usize)> =
                covers.iter().filter(|(c, _)| !kept.contains(c)).map(|(&c, &n)| (c, n)).collect();
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
                    format!(
                        "gate {} ({:.0} wide, {} spots) at {:.0},{:.0}",
                        p.choke, p.width, p.covers, p.pos.x, p.pos.z
                    )
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
        let (cloaks, look, bogey, armed_self) = (m.cloaks.then_some(m.decloak), m.look, m.bogey, m.armed);
        // Running, or pulled home for repairs (see `retreat`).
        if frame < m.fleeing_until || self.retreat_is_retreating(unit) {
            return;
        }

        // The nearest armed enemy that could shoot us, else the nearest
        // mobile one in or near our sight (the one we shadow a while
        // longer, to follow it where it went).
        let mut danger: Option<(i32, f32)> = None;
        let mut near: Option<(i32, f32)> = None;
        for e in self.war.targets.values() {
            let recent = if bogey == Some(e.unit_id) { BOGEY_RECENT } else { RECENT };
            if !armed(e) || frame - e.last_seen > recent {
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
            if cloaks.is_some() {
                self.stakeouts_shadow(id, eid, frame);
                return;
            }
            // One that cannot cloak runs from what could shoot it, and
            // otherwise only notes what it sees and keeps to its duty.
            if danger.is_some() {
                self.stakeouts_flee(id, eid, frame);
                return;
            }
            self.stakeouts_sighted(id, eid, frame, false);
        }

        // Nothing about: back from shadowing to the duty we had.
        if duty == Duty::Shadow
            && let Some(m) = self.stakeouts.members.get_mut(&id)
        {
            m.duty = if m.post.is_some() { Duty::Post } else { Duty::Look };
            m.bogey = None;
            m.standing = false;
        }

        // A sortie against something defenceless in sight.
        let sortie = (self.war.targets.values())
            .filter(|_| armed_self)
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
        let (duty, standing, post) =
            (m.duty, m.standing, m.post.and_then(|i| self.stakeouts.posts.get(i)).map(|p| p.pos));
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
        if m.duty == Duty::Post
            && m.post.is_some_and(|i| self.stakeouts.posts.get(i).is_some_and(|p| p.manned == Some(id)))
        {
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
            self.op_emit(
                "stakeout",
                json!({"event": "duty", "t": frame / 30, "unit": id, "duty": duty.name(), "post": post}),
            );
        }
    }

    /// Note an enemy a stakeout sees (once per enemy in a row).
    fn stakeouts_sighted(&mut self, id: i32, eid: i32, frame: i32, follows: bool) {
        let Some(e) = self.war.targets.get(&eid) else { return };
        let (epos, ename) = (e.position, e.def_name.clone());
        let Some(m) = self.stakeouts.members.get_mut(&id) else { return };
        if m.bogey == Some(eid) {
            return;
        }
        m.bogey = Some(eid);
        let d = distance(m.f.get_pos(), epos);
        self.stakeouts.stats.sightings += 1;
        self.op_emit(
            "stakeout",
            json!({"event": "sighting", "t": frame / 30, "unit": id, "enemy": eid, "def": ename,
                   "x": epos.x.round(), "z": epos.z.round(), "dist": d.round(), "follows": follows}),
        );
    }

    /// A picket that cannot cloak runs from an enemy like any coward: to
    /// the nearest haven, and on for a while (no turning back at the rim of
    /// the enemy's reach).
    fn stakeouts_flee(&mut self, id: i32, eid: i32, frame: i32) {
        self.stakeouts_sighted(id, eid, frame, false);
        let Some(m) = self.stakeouts.members.get(&id) else { return };
        let haven = self.graph_get_closest_haven(m.f.get_pos());
        self.stakeouts.stats.flights += 1;
        let mut nav = nav!(self);
        if let Some(m) = self.stakeouts.members.get_mut(&id) {
            m.fleeing_until = frame + FLEE_FOR;
            m.standing = false;
            m.moved = frame;
            m.sortie = None;
            m.f.sneak(haven, &mut nav);
        }
    }

    /// Keep an enemy at a distance: outside its reach, inside our sight;
    /// follow it if we are as fast, else let it go (pickets that cloak).
    fn stakeouts_shadow(&mut self, id: i32, eid: i32, frame: i32) {
        let Some(e) = self.war.targets.get(&eid) else { return };
        // (`Enemy::speed` is elmos per frame; ours per second.)
        let (epos, espeed) = (e.position, e.speed() * 30.0);
        let Some(m) = self.stakeouts.members.get(&id) else { return };
        let (p, sight, speed, def, moved) = (m.f.get_pos(), m.sight, m.speed, m.def, m.moved);
        let cloaks = m.cloaks.then_some(m.decloak);
        let d = distance(p, epos);
        let keep = keep_from(e, cloaks);
        // Back off to the edge of our sight, or farther if its reach leaves
        // no safe band inside it (then we lose it rather than die to it).
        let back_to = (sight - BACK_TO).max(keep + 150.0);
        let follow_to = sight - FOLLOW_TO;
        let follows = speed >= SPEED_PARITY * espeed && !e.is_static && keep + 150.0 <= follow_to;
        self.stakeouts_sighted(id, eid, frame, follows);
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
        } else if d > follow_to + 60.0 && follows && frame - moved >= FOLLOW_EVERY {
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
        let centre = pool
            .iter()
            .fold(Vec3::ZERO, |c, &i| Vec3::new(c.x + spots[i].position.x / n, 0.0, c.z + spots[i].position.z / n));
        // Frames since a spot was seen; one never seen (`last_seen` 0) is as
        // stale as can be, so the opening lookers go at once.
        let age = |i: usize| if spots[i].last_seen <= 0 { i32::MAX / 2 } else { frame - spots[i].last_seen };
        // Keep the spot we are going to until it is fresh.
        let current = self.stakeouts.members.get(&id).and_then(|m| m.look);
        let shunned_now = |i: usize| self.stakeouts.shunned.get(&i).is_some_and(|&until| until > frame);
        if let Some(i) = current.filter(|&i| pool.contains(&i) && age(i) > FRESH && !shunned_now(i)) {
            return Some(i);
        }
        let taken: Vec<usize> =
            (self.stakeouts.members.iter()).filter(|(o, _)| **o != id).filter_map(|(_, m)| m.look).collect();
        let shunned = |i: usize| self.stakeouts.shunned.get(&i).is_some_and(|&until| until > frame);
        let speed = speed.max(1.0);
        let mut best: Option<(usize, f32)> = None;
        for &i in &pool {
            let s = &spots[i];
            if age(i) <= FRESH || taken.contains(&i) || shunned(i) {
                continue;
            }
            if self.war.get_porc_threat(s.position) > 0.0 || !self.terrain_reaches(def, p, s.position, SNAP) {
                continue;
            }
            let stale = (age(i) as f32 / 30.0).min(120.0);
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
                let look = m
                    .look
                    .and_then(|i| self.graph.metal_spots.get(i))
                    .map(|sp| json!({"spot": m.look, "x": sp.position.x.round(), "z": sp.position.z.round()}));
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
    fn every_opening_factory_has_a_scout() {
        use FacKind::*;
        for kind in [Cloak, Shield, Veh, Hover, Tank, Amph, Jump, Spider, Ship] {
            let scout = stakeout_unit(kind).unwrap();
            assert!(scout_roster(kind).contains(&scout));
        }
        for kind in [Gunship, Plane, StriderHub, Other] {
            assert!(stakeout_unit(kind).is_none());
        }
    }

    #[test]
    fn losses_cool_off() {
        let mut s = Stakeouts::default();
        s.losses.extend([0, 30, 60, 90]);
        assert!(s.hot(90 + REPLACE_DELAY));
        assert!(s.hot(LOSS_RECALL));
        assert!(!s.hot(LOSS_RECALL + 1));
        s.losses.clear();
        s.losses.push_back(0);
        assert!(s.hot(1) && !s.capped(1));
    }

    #[test]
    fn duty_names() {
        assert_eq!(Duty::Look.name(), "look");
        assert_eq!(Duty::Post.name(), "post");
        assert_eq!(Duty::Shadow.name(), "shadow");
    }
}
