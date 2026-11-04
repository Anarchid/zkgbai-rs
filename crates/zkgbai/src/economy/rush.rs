//! Rush (user, 2026-11-04): in a 3v3 or bigger on a small map (Alien
//! Desert, Fairyland), the alliance's ZKGBAIs now and then rush a
//! heavy (Cyclops, Crab, Grizzly, or a Krow at high density). The odds rise with how crowded the
//! front is: its width per player a side. On RUSH, one of us opens with the
//! heavy's factory, the recruited crew starts together around it (user: "when
//! planning to do this the guys should start together"), its first unit is
//! the heavy, and the recruited commanders guard that factory before doing
//! anything else: the engine turns a guard on a factory into assisting what
//! it builds, and each helper's team pays for its own build power. The
//! recruited teams' other factories wait; nonparticipants open normally.
//! When the heavy walks out, everyone goes back to work. The heavy keeps a
//! coordinated assault mission (see military::rush_push).
//!
//! The plan comes before the starts are chosen, so it is made while the AIs
//! initialize, before any unit exists and with no shared random seed in
//! sight. The alliance's instances run in one engine process when one player
//! hosts them all (a local skirmish, an arena match) and share this library:
//! the first to initialize rolls the plan, rusher and starts included, and
//! leaves it in a process-wide table for the others, keyed by game. Allied
//! ZKGBAIs hosted by different players cannot share it, so there is no rush
//! then.
//!
//! The same games roll a second dice of the same odds, on its own throw, for
//! conquest (user: "let's also add a similar probability term for
//! conquer=on in those games"): on a hit, every ZKGBAI of ours plays with the
//! `conquest` switch on (the commander's trip to forward ground a few turrets
//! hold; off by default), unless its options set the switch either way. It
//! needs no helpers, so a lone ZKGBAI among humans rolls it too. With a rush
//! as well, the trip waits for the heavy: conquest pulls the commander only
//! through job prices, and the rush holds it off the jobs meanwhile.
//!
//! `disable=rush` and `disable=conquestroll` turn either off; the
//! (undeclared) options `rush=force` and `conquestroll=force` take every
//! eligible roll, for staged games.

use std::collections::BTreeMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde_json::json;
use springai::{Unit, UnitDef, Vec3};
use zk_terrain::lanes::LaneGrid;
use zk_terrain::{Pos, Tier, TierMap};

use crate::Ai;
use crate::terrain::pos;
use crate::util::{distance, kgb_rand};

/// The engine's CMD_GUARD.
const GUARD: i32 = 25;
/// Maps up to this diagonal (heightmap squares) are small: Alien Desert
/// (12x8) is 923, Fairyland (10x10) 905, a 12x10 map 1000; 12x12 is 1086.
const SMALL_DIAG: f32 = 1000.0;
/// Players a side, at least (3v3+).
const MIN_SIDE: usize = 3;
/// The odds of a rush fall linearly with the front's width per player a
/// side, from [`MAX_ODDS`] to none at this width, elmos. Measured fronts:
/// Fairyland 6332, Alien Desert 6748, so a 3v3 there rolls RUSH ~45% of
/// the time, a 4v4 on Fairyland 60%.
const FRONT_ZERO: f32 = 4000.0;
const MAX_ODDS: f32 = 0.75;
/// Depths (shares of the way from our start box to theirs, by ground) whose
/// passable widths average into the front, and the half-width of each band.
const DEPTHS: [f32; 3] = [0.4, 0.5, 0.6];
const BAND: f32 = 0.05;
/// How long the first instance waits for the terrain model to measure the
/// front (the first analysis takes ~0.1 s).
const TERRAIN_WAIT: Duration = Duration::from_secs(5);
/// A plan in the table is this game's if it is younger than this (the
/// alliance's instances initialize within seconds of each other; a lobby
/// may run its next game in the same process).
const PLAN_FRESH: Duration = Duration::from_secs(120);
/// Together: starts at least this far apart (the commanders' own room) and
/// at most this far from the rusher's.
const START_GAP: f32 = 120.0;
const START_NEAR: f32 = 600.0;
/// The rusher's factory must stand by this frame, or the rush is off.
const FACTORY_BY: i32 = 30 * 45;
/// A rush whose heavy is not out this long after its factory stood is
/// given up.
const GIVE_UP: i32 = 30 * 60 * 4;
/// Frames between checks.
const PERIOD: i32 = 15;
/// The heavy's nanoframe stands this near its factory, elmos.
const NEAR_FACTORY: f32 = 300.0;
/// An order reaches the factory's queue through the server, frames later:
/// the heavy is ordered again only if it does not stand this long after.
const REORDER: i32 = 150;

/// The factories with a rush heavy: (factory, heavy, its name).
pub(crate) const HEAVIES: [(&str, &str, &str); 4] = [
    ("factorytank", "tankheavyassault", "Cyclops"),
    ("factoryspider", "spidercrabe", "Crab"),
    ("factoryamph", "amphassault", "Grizzly"),
    ("factorygunship", "gunshipkrow", "Krow"),
];

/// One game's plan, as the first of the alliance's instances rolled it.
#[derive(Clone, Debug)]
struct Pact {
    /// The team whose instance rolled it.
    by: i32,
    /// The rusher's team and the heavy's factory; `None`: no rush.
    rush: Option<(i32, &'static str)>,
    /// The start of each of us.
    starts: BTreeMap<i32, Vec3>,
    /// The roll, or why there was none.
    why: String,
    /// Conquest on, and its roll (or why there was none).
    conquest: bool,
    conquest_why: String,
}

/// This process's plans: (game key, when, plan).
static PACTS: Mutex<Vec<(String, Instant, Pact)>> = Mutex::new(Vec::new());

/// The rush's state (in the economy).
#[derive(Default)]
pub struct Rush {
    pub state: State,
    /// One commander per recruited team; the others open normally.
    participating: bool,
    commander: Option<i32>,
    /// Every allied instance observes the heavy, including non-builders.
    pub(crate) push: Option<crate::military::rush_push::Push>,
    /// Our start in the plan.
    start: Option<Vec3>,
}

#[derive(Default)]
pub enum State {
    #[default]
    Undecided,
    /// No rush this game.
    Off,
    /// Planned: waiting for the rusher's factory.
    Planned {
        rusher: i32,
        factory: &'static str,
        heavy: UnitDef,
        called: &'static str,
    },
    On(Plan),
    /// The rush is over.
    Over,
}

pub struct Plan {
    /// The team whose factory builds the heavy.
    pub rusher: i32,
    pub factory: Unit,
    pub heavy: UnitDef,
    /// When its factory stood.
    pub since: i32,
    /// The heavy's nanoframe, once it stands.
    pub nano: Option<Unit>,
    /// When our factory was last ordered to build it.
    pub ordered: Option<i32>,
}

/// What a factory of ours does in a rush.
pub(crate) enum RushOrder {
    /// Build the heavy.
    Heavy(UnitDef),
    /// Nothing: the metal goes to the heavy.
    Hold,
}

/// The odds of a rush with a front `front` elmos wide and `side` players a
/// side.
pub(crate) fn odds(front: f32, side: f32) -> f32 {
    if front <= 0.0 || side <= 0.0 {
        return 0.0;
    }
    (1.0 - front / side / FRONT_ZERO).clamp(0.0, MAX_ODDS)
}

/// Early demi-striders pay with three commanders; a Krow has room for seven.
/// This is a total crew cap, including the factory owner's commander.
fn commander_cap(factory: &str) -> usize {
    if factory == "factorygunship" { 7 } else { 3 }
}

/// Density expressed as players on a typical 6400-elmo front. Never plan
/// more build power than the AI participants actually provide. At Krow
/// density choose the larger tier; terrain-filtered demi-striders remain
/// the fallback if gunships cannot open this map.
fn rush_choices<'a>(factories: &[&'a str], front: f32, side: f32, available: usize) -> Vec<&'a str> {
    let density = if front > 0.0 { side * 6400.0 / front } else { 0.0 };
    let krow = available >= 7 && density >= 7.0 && factories.contains(&"factorygunship");
    factories
        .iter()
        .copied()
        .filter(|&f| {
            commander_cap(f) <= available && (if krow { f == "factorygunship" } else { f != "factorygunship" })
        })
        .collect()
}

/// The front: the mean passable width of `map` at [`DEPTHS`] between the
/// two starts (see `zk_terrain::lanes`, the closure profile's `available`).
pub(crate) fn front_width(map: &TierMap, ours: Pos, theirs: Pos) -> Option<f32> {
    let g = LaneGrid::new(map, 2);
    let (oc, ec) = (g.cell_near(ours)?, g.cell_near(theirs)?);
    let (d_o, d_e) = (g.distances(oc), g.distances(ec));
    let widths: Vec<f32> =
        g.profile(ec, oc, &d_o, &d_e, &DEPTHS, BAND).iter().map(|c| c.available).filter(|&w| w > 0.0).collect();
    (!widths.is_empty()).then(|| widths.iter().sum::<f32>() / widths.len() as f32)
}

/// The host player of each AI team in the start script.
pub(crate) fn ai_hosts(script: &str) -> BTreeMap<i32, i32> {
    let mut hosts = BTreeMap::new();
    let (mut depth, mut section) = (0, None::<i32>);
    let (mut team, mut host) = (None, None);
    for raw in script.lines() {
        let line: String = raw.trim().to_ascii_lowercase().chars().filter(|c| !c.is_whitespace()).collect();
        if line.starts_with("[ai") {
            section = Some(depth);
            (team, host) = (None, None);
        }
        // The section's own keys, not those of its [Options].
        if section.is_some_and(|d| depth == d + 1) {
            for kv in line.split([';', '{', '}']) {
                if let Some(v) = kv.strip_prefix("team=").and_then(|t| t.parse().ok()) {
                    team = Some(v);
                } else if let Some(v) = kv.strip_prefix("host=").and_then(|t| t.parse().ok()) {
                    host = Some(v);
                }
            }
        }
        depth += line.matches('{').count() as i32;
        depth -= line.matches('}').count() as i32;
        if let Some(d) = section
            && line.contains('}')
            && depth <= d
        {
            if let (Some(t), Some(h)) = (team, host) {
                hosts.insert(t, h);
            }
            section = None;
        }
    }
    hosts
}

/// Starts for `n` of us together: `anchor` first, then the metal `spots`
/// nearest it, each at least [`START_GAP`] from the others and at most
/// [`START_NEAR`] from it. Fewer than `n` if the spots run out.
pub(crate) fn together(anchor: Vec3, spots: &[Vec3], n: usize) -> Vec<Vec3> {
    let mut near: Vec<Vec3> = spots.iter().copied().filter(|&s| distance(s, anchor) <= START_NEAR).collect();
    near.sort_by(|a, b| distance(*a, anchor).total_cmp(&distance(*b, anchor)));
    let mut starts = vec![anchor];
    for s in near {
        if starts.len() >= n {
            break;
        }
        if starts.iter().all(|&t| distance(t, s) >= START_GAP) {
            starts.push(s);
        }
    }
    starts
}

/// Whether `unit` has a guard on `target` in its queue (the engine pushes
/// the assist in front of the guard).
fn guards(unit: Unit, target: Unit) -> bool {
    unit.current_commands()
        .iter()
        .any(|c| c.id() == GUARD && c.params().first().is_some_and(|&p| p as i32 == target.id()))
}

impl Ai {
    /// Plan the rush at init, before the opening and the starts: take this
    /// game's plan from the table, or roll it (see the module docs).
    pub(crate) fn rush_plan(&mut self) {
        if !matches!(self.eco.rush.state, State::Undecided) {
            return;
        }
        if let Some(why) = self.plan_ineligible() {
            return self.rush_off(&format!("{why} (no conquest roll either)"));
        }
        let script = self.cb.game().setup_script();
        let mut h = DefaultHasher::new();
        script.hash(&mut h);
        let key = format!("{}:{:016x}", self.ally_team_id, h.finish());
        let found = {
            let mut table = PACTS.lock().unwrap_or_else(|e| e.into_inner());
            table.retain(|(_, at, _)| at.elapsed() < PLAN_FRESH);
            table.iter().find(|(k, _, _)| *k == key).map(|(_, _, p)| p.clone())
        };
        // (Instances initialize one after another on the engine's thread:
        // no other rolls meanwhile.)
        let pact = found.unwrap_or_else(|| {
            let p = self.rush_roll();
            PACTS.lock().unwrap_or_else(|e| e.into_inner()).push((key, Instant::now(), p.clone()));
            p
        });
        let whose = if pact.by == self.team_id { "rolled".to_owned() } else { format!("team {}'s roll", pact.by) };
        self.conquest_by_roll(&pact, &whose);
        let Some((rusher, factory)) = pact.rush else {
            return self.rush_off(&format!("{whose}: {}", pact.why));
        };
        let Some(&(_, heavy, called)) = HEAVIES.iter().find(|h| h.0 == factory) else { return };
        let Some(heavy) = self.defs.get(heavy) else {
            return self.rush_off(&format!("no {heavy} unit def"));
        };
        self.eco.rush.start = pact.starts.get(&self.team_id).copied();
        self.eco.rush.participating = self.eco.rush.start.is_some();
        self.log(&format!(
            "rush: RUSH! ({whose}) {}: team {rusher} opens with {factory} for a {called}, we start together{}",
            pact.why,
            match self.eco.rush.start {
                Some(s) => format!(", ours at ({:.0}, {:.0})", s.x, s.z),
                None => String::new(),
            }
        ));
        self.op_emit(
            "rush",
            json!({"rush": true, "by": pact.by, "rusher": rusher, "factory": factory, "why": pact.why, "participants": pact.starts.keys().collect::<Vec<_>>() }),
        );
        self.eco.rush.state = State::Planned { rusher, factory, heavy, called };
    }

    /// Why this game gets no team roll at all (rush nor conquest).
    fn plan_ineligible(&self) -> Option<String> {
        let (side, enemies) = (self.allies.len() + 1, self.enemy_teams.len());
        if self.is_ffa || side.min(enemies) < MIN_SIDE {
            return Some(format!("{side} v {enemies}, not 3v3 or bigger"));
        }
        if self.map_diag > SMALL_DIAG {
            return Some(format!("the map's diagonal is {:.0}, over {SMALL_DIAG:.0}", self.map_diag));
        }
        let hosts = ai_hosts(&self.cb.game().setup_script());
        let ours = hosts.get(&self.team_id);
        if ours.is_none() || self.iq.peers.iter().any(|t| hosts.get(t) != ours) {
            return Some("allied ZKGBAIs on other hosts cannot share a plan".into());
        }
        None
    }

    /// Whether the `disable` option (or ZKGBAI_DISABLE) names `feature`
    /// (and not just its being off by default).
    fn listed_off(&self, feature: &str) -> bool {
        let list = self.cb.option("disable").filter(|v| !v.trim().is_empty());
        let list = list.or_else(|| std::env::var("ZKGBAI_DISABLE").ok()).unwrap_or_default();
        list.split(',').any(|f| f.trim() == feature)
    }

    /// Whether conquest is ours to roll: off only by default, and the roll
    /// not switched off.
    fn conquest_rollable(&self) -> bool {
        self.dev_off("conquest") && !self.listed_off("conquest") && !self.dev_off("conquestroll")
    }

    /// Switch conquest on if the team's roll says so (see the module docs).
    fn conquest_by_roll(&mut self, pact: &Pact, whose: &str) {
        if !self.conquest_rollable() {
            return;
        }
        if pact.conquest {
            self.dev_enable("conquest");
            self.log(&format!("conquest: switched on by the team's roll ({whose}) {}", pact.conquest_why));
        } else {
            self.log(&format!("conquest: stays off by the team's roll ({whose}) {}", pact.conquest_why));
        }
        self.op_emit("conquestroll", json!({"on": pact.conquest, "by": pact.by, "why": pact.conquest_why}));
    }

    /// Roll this game's plan, for the alliance's instances in this process.
    fn rush_roll(&mut self) -> Pact {
        let mut pact = Pact {
            by: self.team_id,
            rush: None,
            starts: BTreeMap::new(),
            why: String::new(),
            conquest: false,
            conquest_why: String::new(),
        };
        let (ally, enemy) = (self.graph.get_ally_base(), self.graph.get_enemy_base());
        let ready = self.terrain_wait(TERRAIN_WAIT);
        let map = ready.then(|| self.terrain().and_then(|t| t.tier(Tier::Bot))).flatten();
        let front = map.zip(enemy).and_then(|(m, e)| front_width(m, pos(ally), pos(e)));
        let Some(front) = front else {
            pact.why = "the front could not be measured".into();
            pact.conquest_why.clone_from(&pact.why);
            return pact;
        };
        let per_side = (self.allies.len() + 1 + self.enemy_teams.len()) as f32 / 2.0;
        let natural = odds(front, per_side);
        let crowd = format!(
            "front {front:.0} elmos for {per_side:.1} players a side ({:.0} each), odds {natural:.2}",
            front / per_side
        );
        // Each dice: (hit, what it rolled).
        let dice = |key: &str| {
            let force = self.cb.option(key).is_some_and(|v| v.trim().eq_ignore_ascii_case("force"));
            let throw = kgb_rand();
            let hit = throw < f64::from(if force { 1.0 } else { natural });
            (hit, format!("{crowd}{}: rolled {throw:.3}", if force { " (forced)" } else { "" }))
        };
        // Conquest's first (it needs no helpers).
        if self.conquest_rollable() {
            (pact.conquest, pact.conquest_why) = dice("conquestroll");
        } else {
            pact.conquest_why = "not rolled: conquest is set by the options".into();
        }
        let factories: Vec<&'static str> =
            HEAVIES.iter().map(|h| h.0).filter(|f| self.eco.potential_fac_list.contains(*f)).collect();
        let no_rush = if self.dev_off("rush") {
            Some("switched off")
        } else if self.iq.peers.len() + 1 < MIN_SIDE {
            Some("fewer than three ZKGBAIs on our side to assist")
        } else if self.eco.water_side.is_some() {
            Some("one of us takes the water side")
        } else if factories.is_empty() {
            Some("no demi-strider factory plays the map")
        } else {
            None
        };
        if let Some(why) = no_rush {
            pact.why = why.into();
            return pact;
        }
        let hit;
        (hit, pact.why) = dice("rush");
        if !hit {
            return pact;
        }
        // The rusher, its factory, and our starts around its.
        let mut us: Vec<i32> = self.iq.peers.iter().copied().chain([self.team_id]).collect();
        us.sort_unstable();
        let pick = |n: usize| ((kgb_rand() * n as f64) as usize).min(n - 1);
        let rusher = us.remove(pick(us.len()));
        let available = us.len() + 1;
        let candidates = rush_choices(&factories, front, per_side, available);
        if candidates.is_empty() {
            pact.why.push_str(": no heavy fits the available crew");
            return pact;
        }
        let factory = candidates[pick(candidates.len())];
        let count = commander_cap(factory);
        us.truncate(count - 1);
        let starts = self.rush_starts(count);
        if starts.len() < count {
            pact.why.push_str(": not enough room for the rush crew");
            return pact;
        }
        pact.why.push_str(&format!(", {count} commanders recruited of {available}"));
        for (team, start) in [rusher].into_iter().chain(us).zip(starts) {
            pact.starts.insert(team, start);
        }
        pact.rush = Some((rusher, factory));
        pact
    }

    /// Starts for `n` of us together: the box's spot nearest the map center
    /// first, then the spots nearest it, then points around it where the
    /// box has too few (see [`together`]).
    fn rush_starts(&self, n: usize) -> Vec<Vec3> {
        let spots: Vec<Vec3> = self.graph.get_ally_territory().iter().map(|s| s.get_pos()).collect();
        let center = self.graph.get_map_center();
        let Some(&anchor) = spots.iter().min_by(|a, b| distance(**a, center).total_cmp(&distance(**b, center))) else {
            return Vec::new();
        };
        let mut starts = together(anchor, &spots, n);
        'more: for radius in [1.5 * START_GAP, 2.5 * START_GAP] {
            for k in 0..16 {
                if starts.len() >= n {
                    break 'more;
                }
                let a = k as f32 * std::f32::consts::TAU / 16.0;
                let p = Vec3::xz(anchor.x + radius * a.cos(), anchor.z + radius * a.sin());
                let check = format!("ai_is_valid_startpos:{:.6}/{:.6}", p.x, p.z);
                if starts.iter().all(|&t| distance(t, p) >= START_GAP)
                    && self.cb.call_lua_rules(&check).as_deref() == Some("1")
                {
                    starts.push(p);
                }
            }
        }
        starts
    }

    /// Our start in the rush plan, if any.
    pub(crate) fn rush_start(&self) -> Option<Vec3> {
        matches!(self.eco.rush.state, State::Planned { .. }).then_some(self.eco.rush.start).flatten()
    }

    /// The opening the plan gives us: the heavy's factory, for the rusher.
    pub(crate) fn rush_opening(&self) -> Option<&'static str> {
        match self.eco.rush.state {
            State::Planned { rusher, factory, .. } if rusher == self.team_id => Some(factory),
            _ => None,
        }
    }

    /// The heavy's factory in a planned rush (the others leave it to the
    /// rusher).
    pub(crate) fn rush_planned_factory(&self) -> Option<&'static str> {
        match self.eco.rush.state {
            State::Planned { factory, .. } if self.eco.rush.participating => Some(factory),
            _ => None,
        }
    }

    /// Whether our opening is the plan's and stays (no diversity replans).
    pub(crate) fn rush_binds_opening(&self) -> bool {
        self.rush_opening().is_some()
    }

    fn rush_off(&mut self, why: &str) {
        self.log(&format!("rush: no rush: {why}"));
        self.op_emit("rush", json!({"rush": false, "why": why}));
        self.eco.rush.state = State::Off;
    }

    /// Wait for the rusher's factory, then drive the rush.
    pub(crate) fn rush_update(&mut self, frame: i32) {
        if frame % PERIOD != 0 {
            return;
        }
        self.rush_push_update(frame);
        match self.eco.rush.state {
            State::Planned { rusher, factory, heavy, called } => self.rush_await(frame, rusher, factory, heavy, called),
            State::On(_) => self.rush_drive(frame),
            State::Undecided | State::Off | State::Over => {}
        }
    }

    fn rush_await(&mut self, frame: i32, rusher: i32, factory: &str, heavy: UnitDef, called: &str) {
        let mut units = self.cb.friendly_units();
        units.extend(self.cb.team_units());
        let found =
            units.into_iter().find(|u| u.team() == rusher && u.health() > 0.0 && self.defs.name_of(*u) == factory);
        let Some(unit) = found else {
            if frame > FACTORY_BY {
                self.log(&format!("rush: over: team {rusher}'s {factory} never stood"));
                self.op_emit("rush", json!({"over": "no factory"}));
                self.eco.rush.state = State::Over;
            }
            return;
        };
        self.log(&format!("rush: team {rusher}'s {factory} stands (unit {}): recruited commanders to it", unit.id()));
        if self.eco.rush.participating {
            self.eco.rush.commander = self
                .eco
                .commanders
                .iter()
                .copied()
                .find(|id| self.eco.workers.get(id).is_some_and(|w| w.unit.health() > 0.0));
        }
        if rusher == self.team_id {
            self.banter_mark("rush", unit.pos(), crate::banter::fields([("heavy", called.into())]));
        }
        self.eco.rush.state = State::On(Plan { rusher, factory: unit, heavy, since: frame, nano: None, ordered: None });
    }

    fn rush_drive(&mut self, frame: i32) {
        let State::On(plan) = &self.eco.rush.state else { return };
        let (rusher, factory, heavy, since) = (plan.rusher, plan.factory, plan.heavy, plan.since);
        let nano = plan.nano.or_else(|| {
            self.cb
                .friendly_units_in_cylinder(factory.pos(), NEAR_FACTORY)
                .into_iter()
                .find(|u| u.team() == rusher && u.health() > 0.0 && u.def().is_some_and(|d| d == heavy))
        });
        let end = if factory.health() <= 0.0 && nano.is_none() {
            Some("its factory is gone")
        } else if nano.is_some_and(|n| n.health() <= 0.0 || n.team() != rusher) {
            Some("the heavy died unfinished")
        } else if nano.is_some_and(|n| !n.is_being_built()) {
            Some("the heavy is out")
        } else if frame - since > GIVE_UP {
            Some("the heavy is not out in time")
        } else {
            None
        };
        if let Some(why) = end {
            if let Some(unit) = nano.filter(|n| n.health() > 0.0 && n.team() == rusher && !n.is_being_built()) {
                self.eco.rush.push = Some(crate::military::rush_push::Push::new(unit, rusher));
                self.op_emit("rush-push", json!({"event": "launched", "unit": unit.id(), "rusher": rusher}));
            }
            return self.rush_end(why);
        }
        if let State::On(plan) = &mut self.eco.rush.state
            && plan.nano.is_none()
            && let Some(n) = nano
        {
            plan.nano = Some(n);
            self.log(&format!("rush: the {} stands (unit {})", self.defs.name(heavy), n.id()));
        }
        // Our factory builds the heavy as soon as it is free.
        if rusher == self.team_id
            && nano.is_none()
            && self.rush_may_order()
            && !factory.is_being_built()
            && factory.num_current_commands() == 0
            && self.fac.factories.contains_key(&factory.id())
        {
            self.fac_assign_factory_task(factory.id());
        }
        // Our commanders guard the factory (and so assist the heavy).
        for wid in self.eco.rush.commander.into_iter() {
            let Some(w) = self.eco.workers.get_mut(&wid) else { continue };
            if w.is_chicken || w.raid_defense.is_some() || w.has_plop() {
                continue;
            }
            if w.task.is_some() {
                w.clear_task(&mut self.eco.tasks);
            }
            let unit = w.unit;
            if !guards(unit, factory) {
                unit.guard(factory, 0);
            }
        }
    }

    fn rush_end(&mut self, why: &str) {
        let State::On(plan) = std::mem::replace(&mut self.eco.rush.state, State::Over) else { return };
        let minutes = (self.frame - plan.since) as f32 / 1800.0;
        self.log(&format!("rush: over after {minutes:.1} min: {why}"));
        self.op_emit("rush", json!({"over": why, "minutes": minutes}));
        // The commanders stop guarding (the factory builds on), and go back
        // to work.
        for wid in self.eco.rush.commander.into_iter() {
            let Some(w) = self.eco.workers.get(&wid) else { continue };
            if guards(w.unit, plan.factory) {
                w.unit.stop(0);
            }
            super::erase_one_dq(&mut self.eco.assigned, wid);
            if !self.eco.idlers.contains(&wid) {
                self.eco.idlers.push_back(wid);
            }
        }
        // The factories held go back to work.
        let idle: Vec<i32> = (self.fac.factories.iter())
            .filter(|(_, f)| !f.unit.is_being_built() && f.unit.num_current_commands() == 0)
            .map(|(&id, _)| id)
            .collect();
        for id in idle {
            self.fac_assign_factory_task(id);
        }
    }

    /// Whether worker `wid` is held for the rush: a commander of ours, its
    /// plop spent, while a rush is on.
    pub(crate) fn rush_holds(&self, wid: i32) -> bool {
        matches!(self.eco.rush.state, State::On(_))
            && self.eco.rush.commander == Some(wid)
            && self.eco.workers.get(&wid).is_some_and(|w| !w.has_plop())
    }

    /// Whether the heavy may be ordered (again): not ordered lately.
    fn rush_may_order(&self) -> bool {
        let State::On(plan) = &self.eco.rush.state else { return false };
        plan.ordered.is_none_or(|f| self.frame - f >= REORDER)
    }

    /// What our factory `fac_id` builds while a rush is on: the rusher's
    /// factory the heavy until it stands (once per [`REORDER`]: the order
    /// is on its way meanwhile); every other factory nothing.
    pub(crate) fn rush_factory(&self, fac_id: i32) -> Option<RushOrder> {
        if !self.eco.rush.participating {
            return None;
        }
        let State::On(plan) = &self.eco.rush.state else {
            // Factory-finished can run before the next rush tick sees the
            // factory. Do not slip a normal opening unit ahead of the heavy.
            return matches!(self.eco.rush.state, State::Planned { .. }).then_some(RushOrder::Hold);
        };
        if plan.rusher == self.team_id && plan.factory.id() == fac_id {
            if plan.nano.is_some() {
                return None;
            }
            return Some(if self.rush_may_order() { RushOrder::Heavy(plan.heavy) } else { RushOrder::Hold });
        }
        Some(RushOrder::Hold)
    }

    /// Our factory was ordered to build the heavy.
    pub(crate) fn rush_ordered(&mut self) {
        let frame = self.frame;
        if let State::On(plan) = &mut self.eco.rush.state {
            plan.ordered = Some(frame);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn odds_rise_as_the_front_crowds() {
        // Fairyland: a 6332-elmo front.
        let three = odds(6332.0, 3.0);
        let four = odds(6332.0, 4.0);
        assert!((0.4..0.5).contains(&three) && four > three, "{three} {four}");
        assert_eq!(odds(6332.0, 1.0), 0.0);
        assert_eq!(odds(100.0, 5.0), MAX_ODDS);
        assert_eq!(odds(0.0, 3.0), 0.0);
        assert_eq!(odds(4000.0, 0.0), 0.0);
    }

    #[test]
    fn hosts_come_from_each_ai_section_not_its_options() {
        let script = "[GAME]\n{\n  [AI0]\n  {\n    Name=ZKGBAI_0;\n    Team=0;\n    Host=0;\n    [Options]\n    {\n      \
                      team=7;\n      host=9;\n    }\n  }\n  [ai1]\n  {\n    team=2;\n    host=3;\n  }\n  [PLAYER0]\n  {\n    \
                      Team=5;\n  }\n}\n";
        assert_eq!(ai_hosts(script), BTreeMap::from([(0, 0), (2, 3)]));
    }

    #[test]
    fn together_takes_the_nearest_spots_with_room_and_no_farther() {
        let anchor = Vec3::xz(1000.0, 1000.0);
        let spots = [
            Vec3::xz(1000.0, 1000.0), // the anchor itself
            Vec3::xz(1050.0, 1000.0), // too close to it
            Vec3::xz(1300.0, 1000.0),
            Vec3::xz(1000.0, 1250.0),
            Vec3::xz(2000.0, 1000.0), // too far
        ];
        let starts = together(anchor, &spots, 3);
        assert_eq!(starts, vec![anchor, Vec3::xz(1000.0, 1250.0), Vec3::xz(1300.0, 1000.0)]);
        assert_eq!(together(anchor, &spots, 9).len(), 3);
    }

    #[test]
    fn density_and_available_commanders_choose_the_tier() {
        let all: Vec<_> = HEAVIES.iter().map(|h| h.0).collect();
        assert_eq!(rush_choices(&all, 6400.0, 3.0, 3), all[..3]);
        assert_eq!(rush_choices(&all, 6400.0, 10.0, 10), vec!["factorygunship"]);
        assert_eq!(rush_choices(&all, 6400.0, 10.0, 3), all[..3]);
        assert_eq!(rush_choices(&all, 12000.0, 10.0, 10), all[..3]);
        assert!(rush_choices(&all, 6400.0, 10.0, 2).is_empty());
        assert_eq!(commander_cap("factoryamph"), 3);
        assert_eq!(commander_cap("factorygunship"), 7);
    }

    #[test]
    fn every_heavy_factory_has_its_heavy() {
        let factories: Vec<&str> = HEAVIES.iter().map(|h| h.0).collect();
        assert_eq!(factories, ["factorytank", "factoryspider", "factoryamph", "factorygunship"]);
    }
}
