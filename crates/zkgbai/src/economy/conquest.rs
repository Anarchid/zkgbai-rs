//! Conquest: walk the commander to forward ground a few turrets can hold,
//! take its metal, and fortify it when what the turrets deny pays for them
//! (feature `conquest`, off by default; user, 2026-10-31).
//!
//! The objectives come from `zk_terrain::conquest`, once per game on a
//! background thread: turret sites (Stingers by default) and the neutral
//! spots near them, each valued per enemy and movement tier as the income
//! the turrets put out of a raider's reach or behind a longer way round.
//! What the enemy fields is a belief per enemy team: Zero-K's ground
//! factories as the prior (three bot factories, two vehicle, one each
//! hover, spider, amphibious), replaced by the metal of what we have seen
//! of each tier (a factory counts as much as an army). The walk and the
//! mexes hold whatever the enemy fields and go first; the turrets are the
//! commitment, decided when the commander gets there and then again every
//! second until they pay:
//!
//! `worth = HORIZON x RAID_LOSS x E[denial]`, against the turrets' metal at
//! its price now (2x up to 3:00 falling to 1x at 5:00: metal early grows
//! into more), times the gain dial (`conquest_gain`, `always` = build).
//!
//! While the commander is away, the constructor nearest home builds the
//! energy (`energy` records): the commander is the opening's energy
//! builder, greedy constructors never build any, and without a stand-in the
//! first battery stalled (energy bank 19-45 against 137-331 at 2-4 min).
//!
//! The commander's station is the objective, from setting out until the
//! game ends, unless the objective fails or the commander dies. With
//! `conquestleash` (off by default; user, 2026-11-02) its jobs cost
//! `LEASH_COST` per elmo beyond `LEASH_RADIUS` of the station: in b2 it
//! spent 12-21% of the time after a hold over 1500 elmos away, on fusions,
//! repairs and far mexes the constructors could have built.
//!
//! Two switches of 2026-11-02 (user: "let's try both"), off by default:
//! `conquestsym` measures both sides' walks from the same kind of place, the
//! center of the metal spots in each side's start box: before, ours came
//! from our start and theirs from their presumed territory, some 750 elmos
//! nearer the middle on Wanderlust, so every middle objective looked like a
//! race we lose. (The presumed territories are no fix: the enemy's is what
//! is hostile or shadowed, ours what we own or shadow, 16 spots against 9
//! at 1:00 there.) `conquestseq` sends the commander on to the next open
//! objective once one is held, until `SEQ_UNTIL`.
//!
//! Guards and attacks (user, 2026-11-03: "always build at least a Lotus
//! before leaving main and before leaving the objective; nodal zones should
//! request defenses earlier; if they get attacked, the turrets get to shoot,
//! so more turrets each time"). Before it sets out, the commander puts a
//! Lotus by our home mex nearest the enemy (the opening raid comes for the
//! base mexes while it is away), unless one stands there (`prepare`); on
//! arrival it puts one by the objective's spots, and the objective is not
//! done until it stands. Turrets stand on the enemy's side of what they
//! hold: the objective's guard `GUARD_FORWARD` toward the enemy from its
//! spots' middle, the turret sites `SITE_FORWARD` toward them. The price clock runs early: 2x until 3:00, 1x from
//! 5:00 (it was 4x to 4:00, 1x at 7:00: the first loss at an objective came
//! a median 2:45 after setting out, the first turret 6:00; 2x to 2:00 and 1x
//! at 4:00 committed two Stingers at 2:59 on 12 metal/s on Ravaged, user
//! 2026-11-03: "the Stinger could wait a minute"). The turrets come within
//! our means: one under construction at a time, the objective's own spots
//! first (its turrets wait up to `OWN_SPOTS_WAIT` for them), each only while
//! it costs at most `AFFORD_SECS` of our metal income (else the objective
//! watches on), and builders near them are drawn in only while metal is
//! banked (`BANK_EMPTY`): on Ravaged two to three Stingers crawled at once
//! with the bank at 0 for four minutes and the mexes neglected. An attack on
//! our units within `ATTACK_RADIUS` of the objective, or of one held,
//! commits the turrets at once and orders one more there (a Lotus when a
//! Stinger is beyond our means), at most one per `ATTACK_GAP` and
//! `MAX_ADDED` per objective.
//!
//! Conquered ground (user, 2026-11-03: "seizing critical junctions denies
//! them to the enemy, and the ground behind them is ours to take; nothing
//! capitalizes on it"). Every objective's control is read each second:
//! ours while our turrets cover it, or while our force there outweighs the
//! enemy's `FORCE_RATIO` times for `FORCE_FOR` seconds (lapsing
//! `FORCE_LAPSE` after it last did), theirs when theirs does for
//! `ENEMY_TICKS` or their turrets stand on a site. While one is ours, the
//! spots it shields (each by how much longer the enemy's way to it gets,
//! weighted by the belief) are conquered ground: a mex there is cheaper for
//! our constructors by `GROUND_BONUS` times the shield, raid memory is
//! waived there while no enemy stands near, colonists may go there though
//! the spot graph presumes it the enemy's, and the chokepoints within the
//! objective's reach count as held for the terrain's shelter. A commander
//! hurt at an objective our force still holds leaves its turrets queued for
//! the usual builders.
//!
//! On the way and lean (user, 2026-11-03: "move towards these while
//! capturing mexes on site; and still fewer turrets"): walking out, the
//! commander takes the free spots near its way first, nearest first, a spot
//! off the straight line costing `ENROUTE_DETOUR_COST` per elmo it adds (up
//! to `ENROUTE_DETOUR`), so it creeps toward the objective instead of
//! walking past metal; an objective is one turret site (the search plans
//! single sites), and attacks add Lotuses only.
//!
//! Allies (user, 2026-11-03: "they build independent turrets instead of
//! assisting each other; assisting ally turrets in our objective region when
//! we want to build turrets might be good even if they're humans or
//! Circuits"): before a conquest turret is ordered, an ally's turret of its
//! kind standing at the site counts as ours, and an ally's turret frame
//! within `ALLY_TURRET_REACH` is helped instead (any ally). With allied
//! ZKGBAIs, an instance whose objectives the lower-numbered ones would all
//! take goes as support: it walks out taking mexes as before, orders no
//! turrets of its own and helps the allied turrets rising within
//! `SUPPORT_RADIUS` of the objective, for `SUPPORT_FOR` after it arrives.
//!
//! Conquest's turrets are placed on purpose and stand past the economy's
//! Lotus crowding rule (it pools Lotuses whose reach overlaps); the guards
//! count a Lotus as cover only when one stands within its reach of the site.
//!
//! Lines and the front (feature `conquestlines`, default on in wip; user
//! 2026-11-03 on the offline comparison "Where conquest would stand": "this
//! is much better; can you swap it in for wip"). The search also closes
//! doors a turret reaches end to end (`DOOR_SLACK` past its reach) and looks
//! for lines: the narrowest cut across the contested band, at most three
//! turrets, kept when it is worth 1.5x its turrets apart (Mercurial's two
//! middle gaps seal our half, ~32 metal/s against bots, for two Stingers).
//! Its spots are ours or theirs only where a mex stands, the rest neutral,
//! sides then read from the walks (the spot graph's shadows had already
//! given Fairyland's centerward tuplets to one side or the other), and
//! `PER_TIER` objectives per design tier reach the pick. The pick gives no
//! bonus for safety (race factor at most 1) and favours ground the enemy
//! reaches soon: x `1 + (URGENT_AT - enemy walk) / URGENT_SCALE`, 0.5 to 1.5
//! (else the safe naturals and the remote corners won). A line is held from
//! its door nearest home: the commander walks there first, forces and
//! control are read there, attacks count near any of its doors, and each
//! door has its turret (one at a time, within our means).
//!
//! Every decision goes to the journal (`conquest` records).

use std::sync::mpsc;

use serde_json::json;
use springai::{UnitDef, Vec3};
use zk_terrain::conquest::{Objective, Params, objectives};
use zk_terrain::strategy::{Asset, Side};
use zk_terrain::{Pos, Tier, TierMap};

use super::{List, TaskId, WorkerTask};
use crate::Ai;
use crate::terrain::{pos, tier_of};
use crate::util::distance;

/// Frames between updates.
const PERIOD: i32 = 30;
/// Tiers objectives are valued on (ships keep to the water).
const TIERS: [Tier; 5] = [Tier::Bot, Tier::Vehicle, Tier::Hover, Tier::Spider, Tier::Amphib];
/// What an enemy fields before we see any of it: Zero-K's ground
/// factories by tier.
const PRIOR: [(Tier, f32); 5] =
    [(Tier::Bot, 3.0), (Tier::Vehicle, 2.0), (Tier::Hover, 1.0), (Tier::Spider, 1.0), (Tier::Amphib, 1.0)];
/// A seen factory counts as this much metal of its tier's units.
const FACTORY_EVIDENCE: f32 = 1500.0;
/// The commander sets out once we hold this many mexes (its first ones)...
const OPENING_MEXES: usize = 3;
/// ... or from this frame on.
const OPENING_LATEST: i32 = 30 * 100;
/// Objectives farther than this from our start (walking) are left alone.
const MAX_WALK: f32 = 4500.0;
/// Walk lead (enemy's walk minus ours, elmos) worth doubling an
/// objective's pick weight: ground we reach first is ours to take.
const RACE_SCALE: f32 = 2000.0;
/// Job-cost bonus of the objective's jobs for the commander, and of its
/// turrets for builders near them.
const BONUS: f32 = 2500.0;
const HELP_BONUS: f32 = 900.0;
const HELP_RADIUS: f32 = 900.0;
/// The turrets are decided when the commander is this near the objective,
/// or this long after it set out.
const ARRIVE: f32 = 900.0;
const WALK_TIMEOUT: i32 = 30 * 300;
/// Spots on the way (see the module notes).
const ENROUTE_DETOUR: f32 = 700.0;
const ENROUTE_DETOUR_COST: f32 = 0.5;
/// Turret sites an objective has (the search's `max_sites`).
const SITES: usize = 1;
/// Lines and the front pick (see the module notes).
const DOOR_SLACK: f32 = 100.0;
const PER_TIER: usize = 3;
const URGENT_AT: f32 = 3500.0;
const URGENT_SCALE: f32 = 2000.0;
/// Allies' turrets (see the module notes).
const ALLY_TURRET_REACH: f32 = 400.0;
const SUPPORT_RADIUS: f32 = 800.0;
const SUPPORT_FOR: i32 = 30 * 180;
/// A line's guard stands among the spots this near its first door.
const GUARD_SPOTS: f32 = 700.0;
/// Undecided objectives are given up at this game time.
const WATCH_UNTIL: i32 = 30 * 60 * 12;
/// Seconds the denial counts for, and the share of income at stake that
/// raids take on contested ground over it.
const HORIZON: f32 = 600.0;
const RAID_LOSS: f32 = 0.35;
/// Updates in a row with the enemy holding the objective before the
/// commander is called back.
const DANGER_TICKS: u32 = 3;
/// The commander is called back under this share of its health...
const CREW_HEALTH: f32 = 0.5;
/// ... and after this long fortifying (the turrets' jobs stay for others).
const FORTIFY_TIMEOUT: i32 = 30 * 240;
/// Times a turret's job is put back after it vanished unbuilt.
const MAX_RETRIES: u8 = 2;
/// A built turret of ours this near its site is the site's.
const BUILT_RADIUS: f32 = 150.0;
const DEFAULT_TURRET: &str = "turretheavylaser";
/// Job-cost bonus of energy jobs for the energy worker (above the
/// caretakers' 1000, below a factory's plop).
const ENERGY_BONUS: f32 = 1500.0;
/// The energy worker hands energy back once a commander is this near home.
const HOME_RADIUS: f32 = 1500.0;
/// With `conquestleash`, the commander's jobs farther than this from its
/// station cost this much more per elmo beyond (ordinary jobs weigh a
/// walk at a quarter to a half per elmo).
const LEASH_RADIUS: f32 = 1200.0;
const LEASH_COST: f32 = 1.0;
/// With `conquestseq`, no objective is set out for after this frame.
const SEQ_UNTIL: i32 = 30 * 60 * 18;

/// The guard: the cheap turret the commander puts up before it leaves home
/// and by the objective's spots on arrival.
const GUARD_TURRET: &str = "turretlaser";
/// The guard's job comes before everything: this on top of the crew's.
const GUARD_BONUS: f32 = 4000.0;
/// A guard not up this long after it was asked for is given up (frames).
const GUARD_TIMEOUT: i32 = 30 * 45;
/// The home guard stands this far out from its mex, toward the enemy.
const GUARD_OUT: f32 = 120.0;
/// Turrets stand on the enemy's side of what they hold ("this side toward
/// enemy", user 2026-11-03): the objective's guard this far from its spots'
/// middle toward the enemy (inside a Lotus's 460 of them), the turret sites
/// this far toward the enemy from where the search put them.
const GUARD_FORWARD: f32 = 150.0;
const SITE_FORWARD: f32 = 100.0;
/// Attacks within this of an objective's center add turrets there (elmos),
/// at most one per `ATTACK_GAP` frames and `MAX_ADDED` per objective.
const ATTACK_RADIUS: f32 = 900.0;
const ATTACK_GAP: i32 = 30 * 20;
const MAX_ADDED: u32 = 2;
/// Within our means (see the module notes).
const AFFORD_SECS: f32 = 30.0;
const BANK_EMPTY: f32 = 30.0;
const OWN_SPOTS_WAIT: i32 = 30 * 60;
/// The crew's own spots at the objective come before its turrets.
const MEX_FIRST: f32 = 1000.0;

/// Conquered ground (see the module notes).
const FORCE_RATIO: f32 = 1.5;
const FORCE_MIN: f32 = crate::terrain::HELD_POWER;
const FORCE_FOR: u32 = 30;
const FORCE_LAPSE: i32 = 30 * 60;
const ENEMY_TICKS: u32 = 3;
pub(crate) const GROUND_BONUS: f32 = 1500.0;
pub(crate) const GROUND_SAFE: f32 = 0.5;
/// Conquered ground is reweighed this often (frames), and on any change.
const GROUND_EVERY: i32 = 30 * 10;

/// Mines at a held objective (user, 2026-11-03: "if we're a mine haver
/// factory we should do the mines for these"): a field this far on its
/// enemy side, mines wanted per held objective, and at most this many in
/// all.
const MINE_FORWARD: f32 = 250.0;
const MINES_PER_HELD: usize = 3;
const MINES_MAX: usize = 6;
/// A mine site this near a held objective's field middle belongs to it.
const MINE_FIELD_REACH: f32 = 700.0;

#[derive(Default)]
pub struct Conquest {
    job: Option<mpsc::Receiver<Vec<Objective>>>,
    asked: bool,
    pub objectives: Vec<Objective>,
    pub assets: Vec<Asset>,
    /// Metal of what we have seen of each tier, per enemy team.
    pub seen: Vec<(i32, Vec<(Tier, f32)>)>,
    pub op: Option<Op>,
    pub done: bool,
    /// The constructor building energy at home while the commander is away.
    pub energy: Option<EnergyWorker>,
    /// Where the commander belongs: its objective (see `conquest_leash`).
    pub station: Option<Station>,
    /// Objectives held, in order (with `conquestseq`, the next is sought).
    pub taken: Vec<usize>,
    /// Held objectives, watched for attacks (turrets are added there).
    pub held: Vec<Held>,
    /// Our units hit near an objective lately: (frame, where).
    pub attacks: Vec<(i32, Vec3)>,
    /// Who holds each objective (see `conquest_control`).
    pub control: Vec<Control>,
    /// Per metal spot (graph index): how much the objectives we hold shield
    /// it (see `conquest_ground`), and when that was weighed.
    pub ground: Vec<f32>,
    pub ground_at: i32,
}

/// Who holds an objective, as our AI reads it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Hold {
    #[default]
    Unknown,
    /// Ours, by turrets or by force.
    Ours(&'static str),
    Contested,
    Theirs,
}

impl Hold {
    fn name(self) -> &'static str {
        match self {
            Hold::Unknown => "unknown",
            Hold::Ours(by) => by,
            Hold::Contested => "contested",
            Hold::Theirs => "theirs",
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Control {
    pub hold: Hold,
    /// Seconds in a row our force (their force) was the stronger there.
    pub force: u32,
    pub enemy: u32,
    pub last_force: i32,
    pub since: i32,
}

pub struct Held {
    pub idx: usize,
    pub center: Vec3,
    pub added: u32,
    pub last_attack: i32,
}

pub struct Station {
    pub crew: i32,
    pub center: Vec3,
    pub since: i32,
}

pub struct EnergyWorker {
    pub id: i32,
    /// Greedy before (made greedy again when the commander is home).
    pub was_greedy: bool,
    pub since: i32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stage {
    /// The commander puts up the home guard before it sets out.
    Prepare,
    /// The commander goes there and takes the spots.
    Walk,
    /// Arrived, the turrets did not pay yet: asked again every update.
    Watch,
    /// Turrets under way.
    Fortify,
}

impl Stage {
    fn name(self) -> &'static str {
        match self {
            Stage::Prepare => "prepare",
            Stage::Walk => "walk",
            Stage::Watch => "watch",
            Stage::Fortify => "fortify",
        }
    }
}

pub struct Op {
    pub idx: usize,
    pub crew: i32,
    pub stage: Stage,
    /// Jobs of the objective's spots not yet ours.
    pub mex_tasks: Vec<TaskId>,
    pub turrets: Vec<Turret>,
    pub started: i32,
    /// When the turrets were committed.
    pub fortify_from: i32,
    danger: u32,
    /// The guards: at home before setting out, by the spots on arrival
    /// (with when each was asked for).
    pub home_guard: Option<(Turret, i32)>,
    pub guard: Option<(Turret, i32)>,
    /// Support: the allies' objective, no turrets of our own (see the module
    /// notes); our jobs helping allied turrets there.
    pub support: bool,
    pub help: Vec<TaskId>,
    /// Turrets added for attacks (after the objective's own, the first
    /// `sites`), and when the last attack counted.
    pub sites: usize,
    pub added: u32,
    pub last_attack: i32,
}

#[derive(Clone, Copy, Debug)]
pub struct Turret {
    /// A Lotus (an attack's addition when a Stinger is beyond our means),
    /// not the conquest turret.
    pub light: bool,
    pub site: Vec3,
    pub task: Option<TaskId>,
    pub retries: u8,
    pub built: bool,
}

/// Metal now against metal later: 2x until 3:00, 1x from 5:00 (see the
/// module notes).
fn metal_price(frame: i32) -> f32 {
    let minutes = frame as f32 / 1800.0;
    1.0 + ((5.0 - minutes) / 2.0).clamp(0.0, 1.0)
}

fn prior() -> Vec<(Tier, f32)> {
    let total: f32 = PRIOR.iter().map(|p| p.1).sum();
    PRIOR.iter().map(|&(t, w)| (t, w / total)).collect()
}

impl Ai {
    pub(super) fn eco_conquest_update(&mut self, frame: i32) {
        if frame % PERIOD != (self.offset + 7) % PERIOD {
            return;
        }
        if self.dev_off("conquest") {
            if self.eco.conquest.op.is_some() {
                self.conquest_end("switched off", false);
            }
            self.conquest_energy_release("switched off", true);
            return;
        }
        self.conquest_plan(frame);
        self.conquest_observe();
        self.conquest_attacks(frame);
        self.conquest_control(frame);
        if self.eco.conquest.op.is_some() {
            self.conquest_step(frame);
        } else if !self.eco.conquest.done {
            self.conquest_start(frame);
        }
        self.conquest_energy(frame);
        if self.eco.conquest.station.as_ref().is_some_and(|s| !self.eco.workers.contains_key(&s.crew)) {
            self.conquest_station_clear("commander lost");
        }
    }

    fn conquest_station_clear(&mut self, why: &str) {
        let Some(s) = self.eco.conquest.station.take() else { return };
        self.log(&format!("conquest: commander {} leaves its station ({why})", s.crew));
        self.op_emit(
            "conquest",
            json!({"event": "station", "t": self.frame / 30, "what": "clear", "why": why, "crew": s.crew,
                   "secs": (self.frame - s.since) / 30}),
        );
    }

    /// Extra job cost for worker `wid` working at `p` (see
    /// `eco_cost_of_job`): with `conquestleash`, the commander's jobs far
    /// from its station, unless it is hurt.
    pub(crate) fn conquest_leash(&self, wid: i32, p: Vec3) -> f32 {
        let Some(s) = self.eco.conquest.station.as_ref().filter(|s| s.crew == wid) else { return 0.0 };
        if self.dev_off("conquestleash") {
            return 0.0;
        }
        let hurt =
            self.eco.workers.get(&wid).is_none_or(|w| w.unit.health() < CREW_HEALTH * w.unit.max_health().max(1.0));
        if hurt {
            return 0.0;
        }
        LEASH_COST * (distance(p, s.center) - LEASH_RADIUS).max(0.0)
    }

    /// Where the energy worker stays: our first factory, else our start.
    fn conquest_home(&self) -> Vec3 {
        self.fac.factories.values().next().map_or_else(|| self.graph.get_start_pos(), |f| f.get_pos())
    }

    /// Whether worker `wid` is the energy worker and `task` an energy job
    /// (see `eco_cost_of_job_alone`).
    fn conquest_energy_bonus(&self, wid: i32, task: TaskId) -> Option<f32> {
        let e = self.eco.conquest.energy.as_ref()?;
        if e.id != wid {
            return None;
        }
        let ids = &self.eco.build_ids;
        let t = self.eco.tasks.get(task)?;
        let id = t.as_construction()?.build_type.id();
        // Energy at home: not the commander's out at the objective.
        ((id == ids.solar_id || id == ids.wind_id) && distance(t.get_pos(), self.conquest_home()) <= HOME_RADIUS)
            .then_some(ENERGY_BONUS)
    }

    /// While the commander is away, the constructor nearest home becomes
    /// non-greedy (greedy ones never build energy) and puts energy first;
    /// once a commander is home again it goes back to what it was. If the
    /// commanders are all gone, it stays on (non-greedy).
    fn conquest_energy(&mut self, frame: i32) {
        if self.eco.conquest.energy.as_ref().is_some_and(|e| !self.eco.workers.contains_key(&e.id)) {
            let id = self.eco.conquest.energy.take().map_or(0, |e| e.id);
            self.op_emit("conquest", json!({"event": "energy", "t": frame / 30, "what": "lost", "worker": id}));
        }
        let home = self.conquest_home();
        if self.eco.conquest.op.is_some() {
            if self.eco.conquest.energy.is_none() {
                self.conquest_energy_pick(frame, home);
            }
            return;
        }
        if self.eco.conquest.energy.is_none() {
            return;
        }
        let commanders: Vec<Vec3> =
            self.eco.commanders.iter().filter_map(|id| self.eco.workers.get(id)).map(|w| w.get_pos()).collect();
        if commanders.is_empty() {
            self.conquest_energy_release("no commander", false);
        } else if commanders.iter().any(|&p| distance(p, home) <= HOME_RADIUS) {
            self.conquest_energy_release("commander home", true);
        }
    }

    fn conquest_energy_pick(&mut self, frame: i32, home: Vec3) {
        let team = self.team_id;
        let pick = (self.eco.workers.values())
            .filter(|w| !w.is_com && w.unit.health() > 0.0 && w.unit.team() == team)
            .map(|w| (w.id, w.is_greedy, distance(w.get_pos(), home)))
            .min_by(|a, b| a.2.total_cmp(&b.2));
        let Some((id, was_greedy, dist)) = pick else { return };
        if let Some(w) = self.eco.workers.get_mut(&id)
            && w.is_greedy
        {
            w.is_greedy = false;
            self.eco.greed -= 1.0;
        }
        self.eco.conquest.energy = Some(EnergyWorker { id, was_greedy, since: frame });
        self.log(&format!(
            "conquest: worker {id} builds energy at home while the commander is away ({dist:.0} from home, was greedy {was_greedy})"
        ));
        self.op_emit(
            "conquest",
            json!({"event": "energy", "t": frame / 30, "what": "pick", "worker": id, "was_greedy": was_greedy, "dist": dist.round()}),
        );
    }

    /// Drop the energy worker's role; `restore`: greedy again if it was.
    fn conquest_energy_release(&mut self, why: &str, restore: bool) {
        let Some(e) = self.eco.conquest.energy.take() else { return };
        if restore
            && e.was_greedy
            && let Some(w) = self.eco.workers.get_mut(&e.id)
            && !w.is_greedy
        {
            w.is_greedy = true;
            self.eco.greed += 1.0;
        }
        self.log(&format!(
            "conquest: worker {} hands energy back ({why}) after {} s",
            e.id,
            (self.frame - e.since) / 30
        ));
        self.op_emit(
            "conquest",
            json!({"event": "energy", "t": self.frame / 30, "what": "release", "why": why, "worker": e.id,
                   "secs": (self.frame - e.since) / 30}),
        );
    }

    fn conquest_turret(&self) -> Option<UnitDef> {
        let name = std::env::var("ZKGBAI_CONQUEST_TURRET")
            .ok()
            .or_else(|| self.cb.option("conquest_turret"))
            .filter(|v| !v.trim().is_empty())
            .unwrap_or_else(|| DEFAULT_TURRET.to_owned());
        self.defs.get(name.trim()).or_else(|| self.defs.get(DEFAULT_TURRET))
    }

    /// The gain dial: the turrets go up when worth / priced cost reaches it
    /// (`always`: 0).
    fn conquest_gain(&self) -> f32 {
        let v = self
            .cb
            .option("conquest_gain")
            .filter(|v| !v.trim().is_empty())
            .or_else(|| std::env::var("ZKGBAI_CONQUEST_GAIN").ok())
            .unwrap_or_default();
        let v = v.trim().to_ascii_lowercase();
        if v == "always" { 0.0 } else { v.parse::<f32>().ok().filter(|g| *g >= 0.0).unwrap_or(1.0) }
    }

    /// Start the objectives' search once the terrain and both bases are
    /// known; collect its result.
    fn conquest_plan(&mut self, frame: i32) {
        if let Some(rx) = &self.eco.conquest.job
            && let Ok(found) = rx.try_recv()
        {
            self.eco.conquest.job = None;
            self.eco.conquest.objectives = found;
            self.eco.conquest.control = vec![Control::default(); self.eco.conquest.objectives.len()];
            self.conquest_report_atlas();
        }
        if self.eco.conquest.asked || frame < 90 {
            return;
        }
        let Some(terrain) = self.terrain.get() else { return };
        let Some((mut ally, mut enemy)) = self.strategy_bases() else { return };
        let box_ally = Some(self.graph.get_ally_base()).filter(|b| !b.is_zero());
        if !self.dev_off("conquestsym")
            && let (Some(a), Some(e)) = (box_ally, self.graph.get_enemy_base())
        {
            // Both walks from the same kind of place (see the module notes).
            (ally, enemy) = (a, e);
        }
        let maps: Vec<TierMap> = TIERS.iter().filter_map(|&t| terrain.tier(t).cloned()).collect();
        let Some(walk) = terrain.tier(Tier::Bot).cloned() else { return };
        let lines = !self.dev_off("conquestlines");
        let assets = if lines { self.conquest_assets() } else { self.strategy_assets() };
        let range = self.conquest_turret().map(|d| d.max_weapon_range()).filter(|r| *r > 100.0).unwrap_or(620.0);
        let params = if lines {
            Params {
                range,
                max_sites: SITES,
                door_slack: DOOR_SLACK,
                lines: true,
                per_tier: PER_TIER,
                ..Params::default()
            }
        } else {
            Params { range, max_sites: SITES, ..Params::default() }
        };
        let (tx, rx) = mpsc::channel();
        let (a, e, spots) = (pos(ally), pos(enemy), assets.clone());
        let spawned = std::thread::Builder::new().name("zkgbai-conquest".into()).spawn(move || {
            let refs: Vec<&TierMap> = maps.iter().collect();
            let _ = tx.send(objectives(&refs, &walk, a, &[e], &spots, &params));
        });
        self.eco.conquest.asked = true;
        match spawned {
            Ok(_) => {
                self.eco.conquest.job = Some(rx);
                self.eco.conquest.assets = assets;
                self.log(&format!(
                    "conquest: looking for objectives from ({:.0},{:.0}) against ({:.0},{:.0}), reach {range:.0}",
                    ally.x, ally.z, enemy.x, enemy.z
                ));
            }
            Err(e) => self.log(&format!("conquest: cannot start the search ({e})")),
        }
    }

    fn conquest_report_atlas(&mut self) {
        let c = &self.eco.conquest;
        let mut lines = Vec::new();
        let list: Vec<serde_json::Value> = c
            .objectives
            .iter()
            .enumerate()
            .map(|(i, o)| {
                let tiers: Vec<String> =
                    o.denial.iter().map(|d| format!("{} {:.1} ({}/{})", d.tier.name(), d.worth, d.sealed, o.spots.len())).collect();
                lines.push(format!(
                    "conquest: objective {i} ({}{} design) sites {} spots {} income {:.1} walk {:.0} (enemy {:.0}); denial {}",
                    if o.line { "line, " } else { "" },
                    o.design.name(),
                    o.sites.iter().map(|s| format!("({:.0},{:.0})", s.x, s.z)).collect::<Vec<_>>().join(" "),
                    o.spots.len(),
                    o.income,
                    o.walk,
                    o.enemy_walk,
                    tiers.join(", ")
                ));
                json!({
                    "design": o.design.name(), "line": o.line, "income": o.income, "walk": o.walk.round(), "enemy_walk": o.enemy_walk.round(),
                    "sites": o.sites.iter().map(|s| [s.x.round(), s.z.round()]).collect::<Vec<_>>(),
                    "spots": o.spots.iter().filter_map(|&k| c.assets.get(k)).map(|a| [a.pos.x.round(), a.pos.z.round(), a.value]).collect::<Vec<_>>(),
                    "denial": o.denial.iter().map(|d| json!({"tier": d.tier.name(), "worth": d.worth, "own": d.own, "sealed": d.sealed})).collect::<Vec<_>>(),
                })
            })
            .collect();
        if lines.is_empty() {
            lines.push("conquest: no objectives on this map".into());
        }
        for l in &lines {
            self.log(l);
        }
        self.op_emit("conquest", json!({"event": "atlas", "t": self.frame / 30, "objectives": list}));
    }

    /// What we have seen of each enemy team's tiers: the metal of its units
    /// we know of now, a factory counting as `FACTORY_EVIDENCE`.
    fn conquest_observe(&mut self) {
        let teams: Vec<i32> = self.get_enemy_team_ids().iter().copied().collect();
        let mut seen: Vec<(i32, Vec<(Tier, f32)>)> = teams.iter().map(|&t| (t, Vec::new())).collect();
        for e in self.war.targets.values().filter(|e| !e.is_dead) {
            let Some(ud) = e.ud else { continue };
            let team = e.unit.team();
            let Some((_, tiers)) = seen.iter_mut().find(|(t, _)| *t == team) else { continue };
            let (tier, w) = if e.is_static {
                match super::planning::factory_tier(ud.name().as_str()) {
                    Some(t) => (t, FACTORY_EVIDENCE),
                    None => continue,
                }
            } else {
                match tier_of(ud) {
                    Some(t) => (t, ud.cost(self.metal).max(1.0)),
                    None => continue,
                }
            };
            match tiers.iter_mut().find(|(t, _)| *t == tier) {
                Some((_, c)) => *c += w,
                None => tiers.push((tier, w)),
            }
        }
        self.eco.conquest.seen = seen;
    }

    /// Each enemy team's tier as a distribution: what we saw, or the prior.
    fn conquest_belief(&self) -> Vec<(i32, Vec<(Tier, f32)>)> {
        let c = &self.eco.conquest;
        if c.seen.is_empty() {
            return vec![(-1, prior())];
        }
        c.seen
            .iter()
            .map(|(team, tiers)| {
                let ground: Vec<(Tier, f32)> = tiers.iter().copied().filter(|(t, _)| TIERS.contains(t)).collect();
                let total: f32 = ground.iter().map(|p| p.1).sum();
                if total > 0.0 {
                    (*team, ground.iter().map(|&(t, w)| (t, w / total)).collect())
                } else {
                    (*team, prior())
                }
            })
            .collect()
    }

    /// Denial an objective is expected to give: per enemy team the tiers'
    /// numbers weighted by the belief, averaged over the teams (the
    /// objectives are found against one enemy origin: our base's foe).
    fn conquest_expected(&self, o: &Objective) -> f32 {
        let belief = self.conquest_belief();
        let n = belief.len().max(1) as f32;
        belief.iter().map(|(_, b)| b.iter().map(|&(t, p)| p * o.denial_for(t, 0)).sum::<f32>()).sum::<f32>() / n
    }

    /// Whether a turret of `def` is within our means now: its metal at most
    /// `AFFORD_SECS` of our metal income.
    fn conquest_affordable(&self, def: UnitDef) -> bool {
        def.cost(self.metal) <= AFFORD_SECS * self.eco.effective_income.max(0.0)
    }

    /// Walking out, a free spot near the commander's way: as dear as the
    /// objective's own, less the detour it adds (see the module notes).
    fn conquest_enroute_bonus(&self, op: &Op, wid: i32, task: TaskId) -> Option<f32> {
        if wid != op.crew || op.stage != Stage::Walk {
            return None;
        }
        let t = self.eco.tasks.get(task)?;
        if t.as_construction()?.build_type.id() != self.eco.build_ids.mex_id {
            return None;
        }
        let from = self.eco.workers.get(&wid)?.get_pos();
        let to = self.conquest_v3(self.conquest_anchor(self.eco.conquest.objectives.get(op.idx)?));
        let at = t.get_pos();
        let detour = distance(from, at) + distance(at, to) - distance(from, to);
        (detour <= ENROUTE_DETOUR).then(|| BONUS + MEX_FIRST - ENROUTE_DETOUR_COST * detour.max(0.0))
    }

    /// The turret an attack adds: a Lotus, if within our means
    /// (`Some((def, true))`), else none.
    fn conquest_attack_turret(&self) -> Option<(UnitDef, bool)> {
        let light = self.defs.get(GUARD_TURRET)?;
        self.conquest_affordable(light).then_some((light, true))
    }

    /// (worth, priced cost, ratio) of an objective's turrets now.
    fn conquest_value(&self, o: &Objective) -> (f32, f32, f32) {
        let worth = HORIZON * RAID_LOSS * self.conquest_expected(o);
        let each = self.conquest_turret().map_or(450.0, |d| d.cost(self.metal));
        let cost = each * o.sites.len() as f32 * metal_price(self.frame);
        (worth, cost, if cost > 0.0 { worth / cost } else { 0.0 })
    }

    fn conquest_spot_hostile(&self, p: Pos) -> bool {
        self.graph
            .metal_spots
            .iter()
            .any(|s| s.hostile && distance(s.position, Vec3::new(p.x, s.position.y, p.z)) < 50.0)
    }

    fn conquest_spot_owned(&self, p: Pos) -> bool {
        self.graph.metal_spots.iter().any(|s| s.owned && distance(s.position, Vec3::new(p.x, s.position.y, p.z)) < 50.0)
    }

    fn conquest_v3(&self, p: Pos) -> Vec3 {
        Vec3::new(p.x, self.cb.map().elevation_at(p.x, p.z), p.z)
    }

    /// The metal spots as conquest sees them: ours or theirs only where a
    /// mex stands (owned, or an enemy's seen), the rest neutral (see the
    /// module notes).
    fn conquest_assets(&self) -> Vec<Asset> {
        (self.graph.metal_spots.iter())
            .map(|s| {
                let side = if s.owned {
                    Side::Ally
                } else if s.hostile {
                    Side::Enemy
                } else {
                    Side::Neutral
                };
                Asset { pos: pos(s.position), value: s.value, side }
            })
            .collect()
    }

    /// Where an objective is held from: its middle; a line's door nearest
    /// home (see the module notes).
    fn conquest_anchor(&self, o: &Objective) -> Pos {
        if !o.line {
            return o.center();
        }
        let home = pos(self.conquest_home());
        (o.sites.iter().copied())
            .min_by(|a, b| a.distance(home).total_cmp(&b.distance(home)))
            .unwrap_or_else(|| o.center())
    }

    /// The objective's turret site nearest `p` (a line's door; else its one).
    fn conquest_near_site(&self, o: &Objective, p: Vec3) -> Vec3 {
        let q = pos(p);
        let s = (o.sites.iter().copied())
            .min_by(|a, b| a.distance(q).total_cmp(&b.distance(q)))
            .unwrap_or_else(|| o.center());
        self.conquest_v3(s)
    }

    fn conquest_start(&mut self, frame: i32) {
        let c = &self.eco.conquest;
        if c.objectives.is_empty() {
            return;
        }
        if self.eco.mexes.len() < OPENING_MEXES && frame < OPENING_LATEST {
            return;
        }
        if !c.taken.is_empty() && frame > SEQ_UNTIL {
            self.eco.conquest.done = true;
            self.log("conquest: too late for another objective");
            return;
        }
        // A later objective waits for a healthy commander.
        let need = if c.taken.is_empty() { 0.0 } else { CREW_HEALTH };
        let Some(crew) = self.eco.commanders.iter().copied().find(|id| {
            self.eco.workers.get(id).is_some_and(|w| {
                !w.has_plop()
                    && w.unit.health() > 0.0
                    && w.unit.health() >= need * w.unit.max_health()
                    && w.unit.team() == self.team_id
            })
        }) else {
            return;
        };
        let best: Vec<(usize, f32)> = (0..c.objectives.len())
            .filter(|&i| {
                let o = &c.objectives[i];
                !c.taken.contains(&i)
                    && o.walk <= MAX_WALK
                    && !o.spots.iter().filter_map(|&k| c.assets.get(k)).all(|a| self.conquest_spot_hostile(a.pos))
                    && o.sites.iter().all(|&s| self.war.get_porc_threat(self.conquest_v3(s)) <= 0.0)
            })
            .map(|i| (i, self.conquest_expected(&c.objectives[i])))
            .filter(|(_, v)| *v > 0.0)
            .collect::<Vec<_>>();
        let front = !self.dev_off("conquestlines");
        let race = |i: usize| {
            let o = &c.objectives[i];
            let r = 1.0 + (o.enemy_walk - o.walk) / RACE_SCALE;
            if front {
                r.clamp(0.3, 1.0) * (1.0 + (URGENT_AT - o.enemy_walk) / URGENT_SCALE).clamp(0.5, 1.5)
            } else {
                r.clamp(0.3, 1.5)
            }
        };
        let mut ranked = best;
        ranked.sort_by(|a, b| (b.1 * race(b.0)).total_cmp(&(a.1 * race(a.0))));
        let best = if self.iq.peers.is_empty() {
            ranked.first().map(|&(i, e)| (i, e, false))
        } else {
            self.conquest_team_pick(crew, &ranked)
        };
        let Some((idx, expected, support)) = best else {
            self.eco.conquest.done = true;
            self.log("conquest: no objective is open");
            self.op_emit("conquest", json!({"event": "none", "t": frame / 30}));
            return;
        };
        let sites: Vec<Vec3> = (self.eco.conquest.objectives[idx].sites.iter())
            .map(|&s| self.conquest_forward(self.conquest_v3(s), SITE_FORWARD))
            .collect();
        let n_sites = sites.len();
        self.eco.conquest.op = Some(Op {
            idx,
            crew,
            stage: Stage::Prepare,
            mex_tasks: Vec::new(),
            turrets: sites
                .into_iter()
                .map(|site| Turret { light: false, site, task: None, retries: 0, built: false })
                .collect(),
            started: frame,
            fortify_from: 0,
            danger: 0,
            home_guard: None,
            guard: None,
            support,
            help: Vec::new(),
            sites: n_sites,
            added: 0,
            last_attack: 0,
        });
        self.conquest_refresh_mexes();
        let o = &self.eco.conquest.objectives[idx];
        let (worth, cost, ratio) = self.conquest_value(o);
        let center = self.conquest_anchor(o);
        self.eco.conquest.station = Some(Station { crew, center: self.conquest_v3(center), since: frame });
        self.log(&format!(
            "conquest: commander {crew} sets out for objective {idx} at ({:.0},{:.0}): {} spots, expected denial {expected:.1}/s, turrets now {ratio:.2} (worth {worth:.0} / cost {cost:.0})",
            center.x,
            center.z,
            o.spots.len()
        ));
        let belief = self.conquest_belief_json();
        self.op_emit(
            "conquest",
            json!({"event": "go", "t": frame / 30, "objective": idx, "crew": crew, "expected": expected,
                   "worth": worth, "cost": cost, "ratio": ratio, "belief": belief, "mexes": self.eco.mexes.len(),
                   "taken": self.eco.conquest.taken.len()}),
        );
    }

    /// With allied ZKGBAIs playing on their own (see `iq`): the eligible
    /// objective nearest our commander that those of a lower team number
    /// would not take, judging each of them, lowest first, to take the one
    /// nearest its commander (user, 2026-11-03: "we typically spread around,
    /// so the closest to us is the pick; we can probe what they would pick,
    /// since we know where their commander is").
    fn conquest_team_pick(&self, crew: i32, eligible: &[(usize, f32)]) -> Option<(usize, f32, bool)> {
        let c = &self.eco.conquest;
        let center = |i: usize| self.conquest_v3(self.conquest_anchor(&c.objectives[i]));
        let mut before: Vec<(i32, Vec3)> = (self.cb.friendly_units().into_iter())
            .filter(|u| self.iq.peers.contains(&u.team()) && u.team() < self.team_id && u.health() > 0.0)
            .filter(|u| u.def().is_some_and(|d| self.defs.is_commander(d)))
            .map(|u| (u.team(), u.pos()))
            .collect();
        before.sort_by_key(|b| b.0);
        before.dedup_by_key(|b| b.0);
        let mut claimed: Vec<usize> = Vec::new();
        for &(_, at) in &before {
            if let Some(&(i, _)) = (eligible.iter())
                .filter(|(i, _)| !claimed.contains(i))
                .min_by(|a, b| distance(center(a.0), at).total_cmp(&distance(center(b.0), at)))
            {
                claimed.push(i);
            }
        }
        let ours = self.eco.workers.get(&crew).map(|w| w.get_pos())?;
        let nearest = |pool: &mut dyn Iterator<Item = &(usize, f32)>| {
            pool.min_by(|a, b| distance(center(a.0), ours).total_cmp(&distance(center(b.0), ours))).copied()
        };
        // None left: support the allies at the nearest of theirs.
        let pick = (nearest(&mut eligible.iter().filter(|(i, _)| !claimed.contains(i))).map(|(i, e)| (i, e, false)))
            .or_else(|| nearest(&mut eligible.iter()).map(|(i, e)| (i, e, true)));
        self.log(&format!(
            "conquest: allied ZKGBAIs on teams {:?} before us would take objectives {claimed:?}; ours: {:?}{}",
            before.iter().map(|b| b.0).collect::<Vec<_>>(),
            pick.map(|p| p.0),
            if pick.is_some_and(|p| p.2) { " (support)" } else { "" }
        ));
        pick
    }

    fn conquest_belief_json(&self) -> serde_json::Value {
        json!(
            self.conquest_belief()
                .iter()
                .map(|(team, b)| json!({"team": team, "p": b.iter().map(|(t, p)| (t.name(), (p * 100.0).round() / 100.0)).collect::<std::collections::BTreeMap<_, _>>()}))
                .collect::<Vec<_>>()
        )
    }

    /// The jobs of the objective's spots not yet ours (the economy makes
    /// them for every neutral spot; see `eco_capture_mexes`).
    fn conquest_refresh_mexes(&mut self) {
        let Some(op) = &self.eco.conquest.op else { return };
        let mex = self.eco.build_ids.mex_id;
        let spots: Vec<Pos> = self.eco.conquest.objectives[op.idx]
            .spots
            .iter()
            .filter_map(|&k| self.eco.conquest.assets.get(k))
            .map(|a| a.pos)
            .collect();
        let tasks: Vec<TaskId> = spots
            .iter()
            .filter(|&&p| !self.conquest_spot_owned(p))
            .filter_map(|&p| {
                self.eco.construction_tasks.iter().copied().find(|&id| {
                    self.eco.tasks.get(id).is_some_and(|t| {
                        t.as_construction().is_some_and(|c| c.build_type.id() == mex)
                            && distance(t.position, Vec3::new(p.x, t.position.y, p.z)) < 50.0
                    })
                })
            })
            .collect();
        if let Some(op) = &mut self.eco.conquest.op {
            op.mex_tasks = tasks;
        }
    }

    /// Job-cost bonus of task `task` for worker `wid` (see
    /// `eco_cost_of_job_alone`): the objective's jobs for its commander,
    /// its turrets for builders near them.
    pub(crate) fn conquest_bonus(&self, wid: i32, task: TaskId) -> Option<f32> {
        if let Some(bonus) = self.conquest_energy_bonus(wid, task) {
            return Some(bonus);
        }
        let op = self.eco.conquest.op.as_ref()?;
        // Before setting out the commander stays home on the home guard: the
        // objective's jobs and those on the way wait for it (it had walked
        // off to the objective's mexes while others built the Lotus).
        if op.stage == Stage::Prepare {
            let (t, _) = op.home_guard.filter(|(t, _)| t.task == Some(task))?;
            if wid == op.crew {
                return Some(BONUS + GUARD_BONUS);
            }
            let w = self.eco.workers.get(&wid)?;
            return (self.eco.metal >= BANK_EMPTY && distance(w.get_pos(), t.site) <= HELP_RADIUS)
                .then_some(HELP_BONUS);
        }
        if op.mex_tasks.contains(&task) {
            return (wid == op.crew).then_some(BONUS + MEX_FIRST);
        }
        if let Some(bonus) = self.conquest_enroute_bonus(op, wid, task) {
            return Some(bonus);
        }
        if op.help.contains(&task) {
            return (wid == op.crew).then_some(BONUS);
        }
        let banked = self.eco.metal >= BANK_EMPTY;
        let guard = [op.home_guard, op.guard].into_iter().flatten().find(|(t, _)| t.task == Some(task));
        if let Some((t, _)) = guard {
            if wid == op.crew {
                return Some(BONUS + GUARD_BONUS);
            }
            let w = self.eco.workers.get(&wid)?;
            return (banked && distance(w.get_pos(), t.site) <= HELP_RADIUS).then_some(HELP_BONUS);
        }
        let t = op.turrets.iter().find(|t| t.task == Some(task))?;
        if wid == op.crew {
            return Some(BONUS);
        }
        let w = self.eco.workers.get(&wid)?;
        (banked && distance(w.get_pos(), t.site) <= HELP_RADIUS).then_some(HELP_BONUS)
    }

    fn conquest_step(&mut self, frame: i32) {
        let Some(op) = &self.eco.conquest.op else { return };
        let (idx, crew, stage, started) = (op.idx, op.crew, op.stage, op.started);
        if !self.eco.workers.contains_key(&crew) {
            self.conquest_end("commander lost", false);
            return;
        }
        let o = self.eco.conquest.objectives[idx].clone();
        let center = self.conquest_v3(self.conquest_anchor(&o));
        let spots: Vec<Pos> = o.spots.iter().filter_map(|&k| self.eco.conquest.assets.get(k)).map(|a| a.pos).collect();
        if spots.iter().all(|&p| self.conquest_spot_hostile(p)) {
            self.conquest_end("the enemy took the spots", false);
            return;
        }
        if o.sites.iter().any(|&s| self.war.get_porc_threat(self.conquest_v3(s)) > 0.0) {
            self.conquest_end("enemy defenses on a site", false);
            return;
        }
        let held = self.war.get_effective_threat(center) > 1.5 * self.war.get_total_friendly_threat(center).max(0.1);
        let danger = if held { op.danger + 1 } else { 0 };
        if let Some(op) = &mut self.eco.conquest.op {
            op.danger = danger;
        }
        if stage != Stage::Watch && danger >= DANGER_TICKS {
            self.conquest_end("the enemy holds the objective", false);
            return;
        }
        let hurt =
            self.eco.workers.get(&crew).is_some_and(|w| w.unit.health() < CREW_HEALTH * w.unit.max_health().max(1.0));
        if hurt && stage != Stage::Watch {
            self.conquest_end("commander hurt", false);
            return;
        }
        self.conquest_refresh_mexes();
        match stage {
            Stage::Prepare => {
                if self.conquest_guard_step(frame, true)
                    && let Some(op) = &mut self.eco.conquest.op
                {
                    op.stage = Stage::Walk;
                    op.started = frame;
                }
            }
            Stage::Walk => {
                let near = self.eco.workers.get(&crew).is_some_and(|w| distance(w.get_pos(), center) <= ARRIVE);
                if near || frame - started > WALK_TIMEOUT {
                    if self.eco.conquest.op.as_ref().is_some_and(|op| op.support) {
                        if let Some(op) = &mut self.eco.conquest.op {
                            op.stage = Stage::Watch;
                            op.fortify_from = frame;
                        }
                        self.log(&format!("conquest: arrived to support the allies at objective {idx}"));
                    } else {
                        self.conquest_decide(frame, if near { "arrived" } else { "walk timed out" });
                    }
                }
            }
            Stage::Watch if self.eco.conquest.op.as_ref().is_some_and(|op| op.support) => {
                self.conquest_support_step(frame, center);
            }
            Stage::Watch => {
                let guarded = self.conquest_guard_step(frame, false);
                if frame > WATCH_UNTIL && guarded {
                    self.conquest_end("turrets never paid", true);
                } else if frame <= WATCH_UNTIL {
                    self.conquest_decide(frame, "watch");
                }
            }
            Stage::Fortify => {
                self.conquest_guard_step(frame, false);
                self.conquest_fortify(frame);
            }
        }
    }

    /// The guard of the op (`home`: the one before setting out): asked for
    /// once, ordered (again) until it stands; true once it stands or was
    /// given up (`GUARD_TIMEOUT`, or no site).
    fn conquest_guard_step(&mut self, frame: i32, home: bool) -> bool {
        let Some(def) = self.defs.get(GUARD_TURRET) else { return true };
        let Some(op) = &self.eco.conquest.op else { return true };
        let slot = if home { op.home_guard } else { op.guard };
        let (t, asked) = match slot {
            Some(g) => g,
            None => {
                let Some(site) =
                    (if home { self.conquest_home_guard_site() } else { self.conquest_guard_site(op.idx) })
                else {
                    return true;
                };
                (Turret { light: false, site, task: None, retries: 0, built: false }, frame)
            }
        };
        let fresh = slot.is_none();
        // Covered: a Lotus standing within its reach of the site. A queued
        // one there (or its frame) becomes the guard, seen up before the
        // commander leaves. Else our own, past the economy's crowding rule
        // (user 2026-11-03, "the lotus-before-leaving base seems to be
        // forgotten": any Lotus within twice the reach counted, a queued
        // task included, so the commander left at 1:23 with none standing).
        let reach = def.max_weapon_range();
        let covered = !t.built
            && t.task.is_none()
            && self.eco.porcs.iter().any(|u| {
                u.health() > 0.0
                    && !u.is_being_built()
                    && u.def().is_some_and(|d| d.id() == def.id())
                    && distance(u.pos(), t.site) <= reach
            });
        let adopted =
            if t.built || covered || t.task.is_some() { None } else { self.conquest_queued_lotus(t.site, reach) };
        let t = if covered {
            Turret { built: true, ..t }
        } else if let Some((id, at)) = adopted {
            Turret { task: Some(id), site: at, ..t }
        } else {
            self.conquest_turret_order(t, def)
        };
        let done = t.built || frame - asked > GUARD_TIMEOUT || (t.task.is_none() && t.retries >= MAX_RETRIES);
        if fresh
            || (done && !slot.is_some_and(|(o, _)| o.built == t.built && o.retries == t.retries && o.task == t.task))
        {
            let what = if covered {
                "covered by a Lotus standing near"
            } else if adopted.is_some() {
                "a queued Lotus near taken as the guard"
            } else if t.built {
                "up"
            } else if done {
                "given up"
            } else if t.task.is_some() {
                "ordered"
            } else {
                "asked"
            };
            self.log(&format!(
                "conquest: {} guard ({GUARD_TURRET}) at ({:.0},{:.0}): {what}",
                if home { "home" } else { "objective" },
                t.site.x,
                t.site.z
            ));
            self.op_emit(
                "conquest",
                json!({"event": "guard", "t": frame / 30, "where": if home { "home" } else { "objective" }, "what": what,
                       "x": t.site.x.round(), "z": t.site.z.round()}),
            );
        }
        if let Some(op) = &mut self.eco.conquest.op {
            if home {
                op.home_guard = Some((t, asked));
            } else {
                op.guard = Some((t, asked));
            }
        }
        done
    }

    /// A Lotus queued (or rising) within `reach` of `site`: its task and
    /// place.
    fn conquest_queued_lotus(&self, site: Vec3, reach: f32) -> Option<(TaskId, Vec3)> {
        (self.eco.porc_tasks.iter()).find_map(|&id| {
            let task = self.eco.tasks.get(id)?;
            let c = task.as_construction()?;
            (c.build_type.name() == GUARD_TURRET && distance(task.position, site) <= reach)
                .then_some((id, task.position))
        })
    }

    /// Where the home guard goes: by our home mex nearest the enemy, out
    /// toward them; none if one of its kind stands there already.
    fn conquest_home_guard_site(&self) -> Option<Vec3> {
        let home = self.conquest_home();
        let (_, enemy) = self.strategy_bases()?;
        let mex = (self.eco.mexes.iter())
            .map(|m| m.pos())
            .filter(|&p| distance(p, home) <= HOME_RADIUS)
            .min_by(|a, b| distance(*a, enemy).total_cmp(&distance(*b, enemy)))
            .unwrap_or(home);
        let d = distance(mex, enemy).max(1.0);
        let site =
            Vec3::new(mex.x + (enemy.x - mex.x) * GUARD_OUT / d, mex.y, mex.z + (enemy.z - mex.z) * GUARD_OUT / d);
        let guarded = (self.eco.porcs.iter())
            .any(|u| distance(u.pos(), site) <= 300.0 && u.def().is_some_and(|d| self.defs.name(d) == GUARD_TURRET));
        (!guarded).then_some(site)
    }

    /// Where the objective's guard goes: among its spots (else its center).
    fn conquest_guard_site(&self, idx: usize) -> Option<Vec3> {
        let o = self.eco.conquest.objectives.get(idx)?;
        // A line's: the spots by its door nearest home.
        let anchor = self.conquest_anchor(o);
        let spots: Vec<Pos> = (o.spots.iter().filter_map(|&k| self.eco.conquest.assets.get(k)))
            .map(|a| a.pos)
            .filter(|p| !o.line || p.distance(anchor) <= GUARD_SPOTS)
            .collect();
        let p = if spots.is_empty() {
            anchor
        } else {
            let n = spots.len() as f32;
            Pos::new(spots.iter().map(|p| p.x).sum::<f32>() / n, spots.iter().map(|p| p.z).sum::<f32>() / n)
        };
        Some(self.conquest_forward(self.conquest_v3(p), GUARD_FORWARD))
    }

    /// `p` moved `by` elmos toward the enemy (the middle of the metal in
    /// their start box, else their presumed center).
    fn conquest_forward(&self, p: Vec3, by: f32) -> Vec3 {
        let Some(enemy) = self.graph.get_enemy_base().or_else(|| self.strategy_bases().map(|b| b.1)) else { return p };
        let d = distance(p, enemy);
        if d < 1.0 {
            return p;
        }
        let k = by.min(d) / d;
        self.conquest_v3(Pos::new(p.x + (enemy.x - p.x) * k, p.z + (enemy.z - p.z) * k))
    }

    /// One turret: whether one of `def` stands within `BUILT_RADIUS` of its
    /// site, else its construction ordered (again, up to `MAX_RETRIES`).
    fn conquest_turret_order(&mut self, mut t: Turret, def: UnitDef) -> Turret {
        if t.built {
            return t;
        }
        let standing = self.eco.porcs.iter().any(|u| {
            distance(u.pos(), t.site) <= BUILT_RADIUS
                && u.def().is_some_and(|d| d.id() == def.id())
                && !u.is_being_built()
        });
        if standing {
            t.built = true;
            t.task = None;
            return t;
        }
        if let Some(a) = self.conquest_turret_ally(t, def) {
            return a;
        }
        if t.task.is_some_and(|id| self.eco.tasks.get(id).is_some()) || (t.task.is_some() && t.retries >= MAX_RETRIES) {
            return t;
        }
        let at = self.cb.map().find_closest_build_site(def, t.site, 250.0, 3, 0);
        let id = (at.x >= 0.0)
            .then(|| {
                self.eco_add_construction_uncrowded(
                    WorkerTask::construction(def, at, 0),
                    Some(List::Porc),
                    Some(List::Porc),
                )
            })
            .flatten();
        if t.task.is_some() || id.is_none() {
            t.retries += 1;
        }
        t.task = id;
        if let Some(id) = id {
            self.log(&format!("conquest: {} ordered at ({:.0},{:.0}) (task {id})", def.name(), at.x, at.z));
        }
        t
    }

    /// Our force there and theirs, by power (the scale of `get_danger`).
    fn conquest_forces_at(&self, idx: usize) -> (f32, f32) {
        let Some(o) = self.eco.conquest.objectives.get(idx) else { return (0.0, 0.0) };
        let c = self.conquest_v3(self.conquest_anchor(o));
        (self.war.get_total_friendly_power(c), self.war.get_enemy_power(c))
    }

    /// Whether our force outweighs the enemy's at objective `idx` now.
    fn conquest_force_holds(&self, idx: usize) -> bool {
        let (ours, theirs) = self.conquest_forces_at(idx);
        ours >= FORCE_MIN && ours >= FORCE_RATIO * theirs
    }

    /// Read who holds each objective; reweigh the conquered ground when
    /// that changes (and every `GROUND_EVERY`).
    fn conquest_control(&mut self, frame: i32) {
        let n = self.eco.conquest.objectives.len();
        if self.eco.conquest.control.len() != n {
            self.eco.conquest.control = vec![Control::default(); n];
        }
        let mut changed = false;
        for i in 0..n {
            let o = &self.eco.conquest.objectives[i];
            let center = self.conquest_v3(self.conquest_anchor(o));
            let turrets = self.war.get_friendly_porc_threat(center) >= crate::terrain::HELD_POWER;
            let their_porc = o.sites.iter().any(|&s| self.war.get_porc_threat(self.conquest_v3(s)) > 0.0);
            let (ours, theirs) = self.conquest_forces_at(i);
            let force = ours >= FORCE_MIN && ours >= FORCE_RATIO * theirs;
            let enemy = their_porc || (theirs >= FORCE_MIN && theirs > FORCE_RATIO * ours);
            let mut c = self.eco.conquest.control[i];
            c.force = if force { c.force + 1 } else { 0 };
            c.enemy = if enemy { c.enemy + 1 } else { 0 };
            if force {
                c.last_force = frame;
            }
            let hold = if turrets && !enemy {
                Hold::Ours("turrets")
            } else if c.enemy >= ENEMY_TICKS {
                Hold::Theirs
            } else if c.force >= FORCE_FOR || (c.hold == Hold::Ours("force") && frame - c.last_force < FORCE_LAPSE) {
                Hold::Ours("force")
            } else if matches!(c.hold, Hold::Ours(_)) {
                Hold::Contested
            } else {
                c.hold
            };
            if hold != c.hold {
                self.log(&format!(
                    "conquest: objective {i} is {} (was {}; our power {ours:.0}, theirs {theirs:.0})",
                    hold.name(),
                    c.hold.name()
                ));
                self.op_emit(
                    "conquest",
                    json!({"event": "control", "t": frame / 30, "objective": i, "hold": hold.name(),
                           "was": c.hold.name(), "ours": ours.round(), "theirs": theirs.round()}),
                );
                c.hold = hold;
                c.since = frame;
                changed = true;
            }
            self.eco.conquest.control[i] = c;
        }
        if changed || frame - self.eco.conquest.ground_at >= GROUND_EVERY {
            self.conquest_ground_weigh(frame, changed);
        }
    }

    /// The spots the objectives we hold shield, by how much (the most any
    /// held objective gives; each objective's tiers weighted by the belief).
    fn conquest_ground_weigh(&mut self, frame: i32, log: bool) {
        let c = &self.eco.conquest;
        let mut g = vec![0.0f32; c.assets.len()];
        let belief = self.conquest_belief();
        let teams = belief.len().max(1) as f32;
        for (i, o) in c.objectives.iter().enumerate() {
            if !matches!(c.control.get(i).map(|c| c.hold), Some(Hold::Ours(_))) {
                continue;
            }
            let mut acc = vec![0.0f32; g.len()];
            for (_, b) in &belief {
                for &(t, p) in b {
                    if let Some(d) = o.denial.iter().find(|d| d.tier == t && d.enemy == 0) {
                        for &(k, w) in &d.shield {
                            if let Some(a) = acc.get_mut(k) {
                                *a += p * w / teams;
                            }
                        }
                    }
                }
            }
            for (gk, ak) in g.iter_mut().zip(acc) {
                *gk = gk.max(ak);
            }
        }
        if log {
            let shielded = g.iter().filter(|&&w| w >= GROUND_SAFE).count();
            let free = (g.iter().enumerate())
                .filter(|&(k, &w)| {
                    w >= GROUND_SAFE && self.graph.metal_spots.get(k).is_some_and(|s| !s.owned && !s.hostile)
                })
                .count();
            self.log(&format!(
                "conquest: conquered ground shields {shielded} spots ({free} free, shield >= {GROUND_SAFE})"
            ));
        }
        self.eco.conquest.ground = g;
        self.eco.conquest.ground_at = frame;
    }

    /// How much the objectives we hold shield the metal spot under `p`
    /// (0: none, or not a spot).
    pub(crate) fn conquest_ground(&self, p: Vec3) -> f32 {
        let g = &self.eco.conquest.ground;
        if g.is_empty() || self.dev_off("conquest") {
            return 0.0;
        }
        self.graph.spot_under(p).and_then(|i| g.get(i)).copied().unwrap_or(0.0)
    }

    /// A mex job's bonus on conquered ground (see `eco_cost_of_job_alone`).
    pub(crate) fn conquest_ground_bonus(&self, p: Vec3) -> f32 {
        GROUND_BONUS * self.conquest_ground(p)
    }

    /// Whether `p` is conquered ground no enemy stands near now: raid
    /// memory is waived there (see `assign`).
    pub(crate) fn conquest_ground_safe(&self, p: Vec3) -> bool {
        self.conquest_ground(p) >= GROUND_SAFE && self.war.get_enemy_power(p) <= 0.0
    }

    /// Sites of the objectives we hold: the chokepoints within their reach
    /// count as held for the terrain's shelter.
    pub(crate) fn conquest_held_sites(&self) -> Vec<Vec3> {
        let c = &self.eco.conquest;
        (c.objectives.iter().enumerate())
            .filter(|(i, _)| matches!(c.control.get(*i).map(|c| c.hold), Some(Hold::Ours(_))))
            .flat_map(|(_, o)| o.sites.iter().map(|&s| self.conquest_v3(s)))
            .collect()
    }

    /// Minefields for the objectives we hold: (middle, value), on each
    /// one's enemy side (see `sappers`).
    pub(crate) fn conquest_mine_fields(&self) -> Vec<(Vec3, f32)> {
        if self.dev_off("conquest") {
            return Vec::new();
        }
        let c = &self.eco.conquest;
        (c.objectives.iter().enumerate())
            .filter(|(i, _)| matches!(c.control.get(*i).map(|c| c.hold), Some(Hold::Ours(_))))
            .map(|(_, o)| (self.conquest_forward(self.conquest_v3(self.conquest_anchor(o)), MINE_FORWARD), 2.0))
            .collect()
    }

    /// Whether `p` lies in a held objective's minefield: a mine site there
    /// counts as front for the sappers though it is past our front line.
    pub(crate) fn conquest_mine_ground(&self, p: Vec3) -> bool {
        self.conquest_mine_fields().iter().any(|(c, _)| distance(*c, p) <= MINE_FIELD_REACH)
    }

    /// Whether a factory that makes mines should make one now for the
    /// objectives we hold (`have`: our mines).
    pub(crate) fn conquest_wants_mines(&self, have: usize) -> bool {
        let held = self.conquest_mine_fields().len();
        held > 0 && have < (MINES_PER_HELD * held).min(MINES_MAX)
    }

    /// The conquest turret's reach (Stinger 620 by default).
    pub(crate) fn conquest_reach(&self) -> f32 {
        self.conquest_turret().map(|d| d.max_weapon_range()).filter(|r| *r > 100.0).unwrap_or(620.0)
    }

    /// Allies' turrets for one of ours (see the module notes): `Some` built,
    /// when an ally's of its kind stands at its site; `Some` with a job on
    /// it, when an ally's turret frame rises near; else `None` (ours).
    fn conquest_turret_ally(&mut self, t: Turret, def: UnitDef) -> Option<Turret> {
        if self.allies.is_empty() {
            return None;
        }
        let near = self.cb.friendly_units_in_cylinder(t.site, ALLY_TURRET_REACH);
        let allied =
            |u: &springai::Unit| u.team() != self.team_id && self.allies.contains(&u.team()) && u.health() > 0.0;
        if near.iter().any(|u| {
            allied(u)
                && !u.is_being_built()
                && distance(u.pos(), t.site) <= BUILT_RADIUS
                && u.def().is_some_and(|d| d.id() == def.id())
        }) {
            return Some(Turret { built: true, task: None, ..t });
        }
        if t.task.is_some_and(|id| self.eco.tasks.get(id).is_some()) {
            return None;
        }
        let frame = near.into_iter().find(|u| {
            allied(u) && u.is_being_built() && u.def().is_some_and(|d| self.defs.name(d).starts_with("turret"))
        })?;
        let id = self.eco_task_on(frame).or_else(|| self.eco_help_frame(frame, List::Porc))?;
        Some(Turret { site: frame.pos(), task: Some(id), ..t })
    }

    /// Support (see the module notes): help the allied turrets rising near
    /// the objective; done after `SUPPORT_FOR` with none left to help.
    fn conquest_support_step(&mut self, frame: i32, center: Vec3) {
        let frames: Vec<springai::Unit> = (self.cb.friendly_units_in_cylinder(center, SUPPORT_RADIUS).into_iter())
            .filter(|u| {
                u.team() != self.team_id
                    && self.allies.contains(&u.team())
                    && u.health() > 0.0
                    && u.is_being_built()
                    && u.def().is_some_and(|d| self.defs.name(d).starts_with("turret"))
            })
            .collect();
        let mut help = Vec::new();
        for u in &frames {
            if let Some(id) = self.eco_task_on(*u).or_else(|| self.eco_help_frame(*u, List::Porc)) {
                help.push(id);
            }
        }
        let since = self.eco.conquest.op.as_ref().map_or(frame, |op| op.fortify_from);
        if let Some(op) = &mut self.eco.conquest.op {
            op.help = help;
        }
        if frames.is_empty() && frame - since > SUPPORT_FOR {
            self.conquest_end("support done", true);
        }
    }

    /// Our unit was hit: if near an objective (ours now, or held), note it
    /// (see `conquest_attacks`).
    pub(crate) fn conquest_attacked(&mut self, unit: springai::Unit, attacker: Option<springai::Unit>) {
        let Some(a) = attacker else { return };
        if !self.war.targets.contains_key(&a.id()) {
            return;
        }
        let c = &self.eco.conquest;
        if c.op.is_none() && c.held.is_empty() {
            return;
        }
        let p = unit.pos();
        let op_center = c.op.as_ref().and_then(|op| c.objectives.get(op.idx)).map(|o| self.conquest_near_site(o, p));
        let near = op_center.is_some_and(|q| distance(q, p) <= ATTACK_RADIUS)
            || c.held.iter().any(|h| distance(h.center, p) <= ATTACK_RADIUS);
        if near && self.eco.conquest.attacks.len() < 32 {
            self.eco.conquest.attacks.push((self.frame, p));
        }
    }

    /// Attacks at an objective: the turrets go up now (they will shoot) and
    /// one more is ordered by where it came, at most one per `ATTACK_GAP`
    /// and `MAX_ADDED` per objective; held objectives get theirs from the
    /// usual builders.
    fn conquest_attacks(&mut self, frame: i32) {
        let attacks = std::mem::take(&mut self.eco.conquest.attacks);
        if attacks.is_empty() {
            return;
        }
        // The turret an attack adds: none when even a Lotus is beyond us.
        let Some((def, light)) = self.conquest_attack_turret() else { return };
        let toward = |p: Vec3, c: Vec3| Vec3::new(p.x + (c.x - p.x) * 0.4, p.y, p.z + (c.z - p.z) * 0.4);
        if let Some(op) = &self.eco.conquest.op
            && matches!(op.stage, Stage::Watch | Stage::Fortify)
            && op.added < MAX_ADDED
            && frame - op.last_attack >= ATTACK_GAP
        {
            let o = &self.eco.conquest.objectives[op.idx];
            let hit =
                (attacks.iter().rev()).find(|(_, p)| distance(*p, self.conquest_near_site(o, *p)) <= ATTACK_RADIUS);
            if let Some(&(_, p)) = hit {
                let center = self.conquest_near_site(o, p);
                let watching = op.stage == Stage::Watch && !light;
                // A Lotus while the Stingers still wait is ordered now (the
                // op's list is ordered only once it fortifies).
                let direct = op.stage == Stage::Watch && light;
                let site = toward(p, center);
                if direct {
                    self.conquest_turret_order(Turret { light, site, task: None, retries: 0, built: false }, def);
                }
                if let Some(op) = &mut self.eco.conquest.op {
                    op.added += 1;
                    op.last_attack = frame;
                    if !direct {
                        op.turrets.push(Turret { light, site, task: None, retries: 0, built: false });
                    }
                    if watching {
                        op.stage = Stage::Fortify;
                        op.fortify_from = frame;
                    }
                }
                let idx = self.eco.conquest.op.as_ref().map_or(0, |op| op.idx);
                self.log(&format!(
                    "conquest: objective {idx} attacked at ({:.0},{:.0}): {}one more {} ordered",
                    p.x,
                    p.z,
                    if watching { "turrets go up, " } else { "" },
                    def.name()
                ));
                self.op_emit(
                    "conquest",
                    json!({"event": "attack", "t": frame / 30, "objective": idx, "x": p.x.round(), "z": p.z.round(),
                           "commit": watching, "held": false}),
                );
                if watching {
                    self.conquest_fortify(frame);
                }
            }
        }
        for i in 0..self.eco.conquest.held.len() {
            let h = &self.eco.conquest.held[i];
            if h.added >= MAX_ADDED || frame - h.last_attack < ATTACK_GAP {
                continue;
            }
            let Some(&(_, p)) = attacks.iter().rev().find(|(_, p)| distance(*p, h.center) <= ATTACK_RADIUS) else {
                continue;
            };
            let (idx, center) = (h.idx, h.center);
            let t = Turret { light, site: toward(p, center), task: None, retries: 0, built: false };
            let t = self.conquest_turret_order(t, def);
            let h = &mut self.eco.conquest.held[i];
            h.added += 1;
            h.last_attack = frame;
            self.log(&format!(
                "conquest: held objective {idx} attacked at ({:.0},{:.0}): one more {} ordered",
                p.x,
                p.z,
                def.name()
            ));
            self.op_emit(
                "conquest",
                json!({"event": "attack", "t": frame / 30, "objective": idx, "x": p.x.round(), "z": p.z.round(),
                       "held": true, "ordered": t.task.is_some()}),
            );
        }
    }

    /// Whether the turrets pay now: build them, or watch.
    fn conquest_decide(&mut self, frame: i32, why: &str) {
        let Some(op) = &self.eco.conquest.op else { return };
        let (idx, stage) = (op.idx, op.stage);
        let o = &self.eco.conquest.objectives[idx];
        let (worth, cost, ratio) = self.conquest_value(o);
        let gain = self.conquest_gain();
        let means = self.conquest_turret().is_some_and(|d| self.conquest_affordable(d));
        let go = ratio >= gain && means;
        // Watching, record only the change (and the first look).
        if stage != Stage::Watch || go {
            let expected = self.conquest_expected(o);
            self.log(&format!(
                "conquest: {why}: turrets {} at ratio {ratio:.2} (worth {worth:.0}, cost {cost:.0} at x{:.1}, gain {gain:.2}, expected denial {expected:.1}/s, income {:.1}{})",
                if go { "go up" } else { "wait" },
                metal_price(frame),
                self.eco.effective_income,
                if means { "" } else { ": beyond our means" }
            ));
            let belief = self.conquest_belief_json();
            self.op_emit(
                "conquest",
                json!({"event": if go { "commit" } else { "wait" }, "t": frame / 30, "why": why, "worth": worth, "cost": cost,
                       "ratio": ratio, "gain": gain, "price": metal_price(frame), "expected": expected, "belief": belief}),
            );
        }
        if let Some(op) = &mut self.eco.conquest.op {
            op.stage = if go { Stage::Fortify } else { Stage::Watch };
            if go {
                op.fortify_from = frame;
            }
        }
        // On arrival: the objective's guard first.
        if stage == Stage::Walk {
            self.conquest_guard_step(frame, false);
        }
        if go {
            self.conquest_fortify(frame);
        }
    }

    fn conquest_fortify(&mut self, frame: i32) {
        let Some(heavy) = self.conquest_turret() else {
            self.conquest_end("no turret", false);
            return;
        };
        let lotus = self.defs.get(GUARD_TURRET).unwrap_or(heavy);
        let n = self.eco.conquest.op.as_ref().map_or(0, |op| op.turrets.len());
        for i in 0..n {
            let Some(t) = self.eco.conquest.op.as_ref().map(|op| {
                let t = op.turrets[i];
                (t.site, t.task, t.retries, t.built, t.light)
            }) else {
                return;
            };
            let (site, task, retries, built, light) = t;
            let def = if light { lotus } else { heavy };
            if built {
                continue;
            }
            let standing = self.eco.porcs.iter().any(|u| {
                distance(u.pos(), site) <= BUILT_RADIUS
                    && u.def().is_some_and(|d| d.id() == def.id())
                    && !u.is_being_built()
            });
            if standing {
                if let Some(op) = &mut self.eco.conquest.op {
                    op.turrets[i].built = true;
                    op.turrets[i].task = None;
                }
                continue;
            }
            if task.is_some_and(|id| self.eco.tasks.get(id).is_some()) {
                continue;
            }
            if task.is_some() && retries >= MAX_RETRIES {
                continue;
            }
            // An ally's turret here stands, or rises near: theirs or help.
            let current = self.eco.conquest.op.as_ref().map(|op| op.turrets[i]);
            if let Some(a) = current.and_then(|c| self.conquest_turret_ally(c, def)) {
                if let Some(op) = &mut self.eco.conquest.op {
                    op.turrets[i] = a;
                }
                continue;
            }
            // Within our means: one at a time, the objective's own spots
            // first (for a while), and only what our income bears.
            let (busy, spots_pending) = self.eco.conquest.op.as_ref().map_or((true, false), |op| {
                let busy = (op.turrets.iter().enumerate())
                    .any(|(j, o)| j != i && !o.built && o.task.is_some_and(|id| self.eco.tasks.get(id).is_some()));
                (busy, !op.mex_tasks.is_empty() && frame - op.fortify_from < OWN_SPOTS_WAIT)
            });
            if busy || spots_pending || !self.conquest_affordable(def) {
                continue;
            }
            let at = self.cb.map().find_closest_build_site(def, site, 250.0, 3, 0);
            let id = (at.x >= 0.0)
                .then(|| {
                    self.eco_add_construction_uncrowded(
                        WorkerTask::construction(def, at, 0),
                        Some(List::Porc),
                        Some(List::Porc),
                    )
                })
                .flatten();
            if let Some(op) = &mut self.eco.conquest.op {
                op.turrets[i].task = id;
                if task.is_some() || id.is_none() {
                    op.turrets[i].retries += 1;
                }
            }
            if let Some(id) = id {
                self.log(&format!("conquest: {} ordered at ({:.0},{:.0}) (task {id})", def.name(), at.x, at.z));
            }
        }
        let (all_built, stuck) = self.eco.conquest.op.as_ref().map_or((false, true), |op| {
            let own = &op.turrets[..op.sites.min(op.turrets.len())];
            (own.iter().all(|t| t.built), own.iter().all(|t| t.built || (t.task.is_none() && t.retries >= MAX_RETRIES)))
        });
        let since = self.eco.conquest.op.as_ref().map_or(0, |op| frame - op.fortify_from);
        let guarded = self.eco.conquest.op.as_ref().is_none_or(|op| {
            op.guard.is_none_or(|(t, asked)| {
                t.built || frame - asked > GUARD_TIMEOUT || (t.task.is_none() && t.retries >= MAX_RETRIES)
            })
        });
        if all_built && guarded {
            self.conquest_end("held", true);
        } else if stuck && frame > 0 {
            self.conquest_end("turret sites lost", false);
        } else if since > FORTIFY_TIMEOUT {
            // Some of its own turrets standing, it is held in part: kept
            // among the held (attacks still add turrets there).
            let some = self
                .eco
                .conquest
                .op
                .as_ref()
                .is_some_and(|op| op.turrets[..op.sites.min(op.turrets.len())].iter().any(|t| t.built));
            if some {
                self.conquest_end("held in part", true);
            } else {
                self.conquest_end("fortify timed out", false);
            }
        }
    }

    /// Release the commander; drop turret jobs not yet started.
    fn conquest_end(&mut self, why: &str, success: bool) {
        let Some(op) = self.eco.conquest.op.take() else { return };
        if success {
            self.eco.conquest.taken.push(op.idx);
            if let Some(o) = self.eco.conquest.objectives.get(op.idx) {
                let center = self.conquest_v3(self.conquest_anchor(o));
                self.eco.conquest.held.push(Held { idx: op.idx, center, added: op.added, last_attack: op.last_attack });
            }
        }
        // With `conquestseq` a held objective leads on to the next.
        self.eco.conquest.done = !success || self.dev_off("conquestseq");
        let mut dropped = 0;
        let guards = [op.home_guard, op.guard].into_iter().flatten().map(|(t, _)| t);
        // A held objective keeps what is still to come (turrets added for
        // attacks); a lost one drops what was not started.
        // So does one our force still holds when only the commander was hurt.
        let keep = success || (why == "commander hurt" && self.conquest_force_holds(op.idx));
        let pending: Vec<Turret> = if keep { Vec::new() } else { op.turrets.iter().copied().chain(guards).collect() };
        for t in pending.iter() {
            if let Some(id) = t.task
                && self.eco.tasks.get(id).is_some_and(|task| task.as_construction().is_some_and(|c| c.target.is_none()))
            {
                self.eco.drop_construction_task(id);
                dropped += 1;
            }
        }
        let built = op.turrets.iter().filter(|t| t.built).count();
        if !success {
            self.conquest_station_clear(why);
        }
        self.log(&format!(
            "conquest: objective {} ends ({why}) after {} s in {}: {built} turrets standing, {dropped} unstarted dropped",
            op.idx,
            (self.frame - op.started) / 30,
            op.stage.name()
        ));
        self.op_emit(
            "conquest",
            json!({"event": "end", "t": self.frame / 30, "objective": op.idx, "why": why, "success": success,
                   "stage": op.stage.name(), "built": built, "dropped": dropped, "secs": (self.frame - op.started) / 30}),
        );
    }

    /// Operator / journal view.
    pub(crate) fn conquest_json(&self) -> serde_json::Value {
        let c = &self.eco.conquest;
        json!({
            "objectives": c.objectives.len(),
            "done": c.done,
            "taken": c.taken,
            "belief": self.conquest_belief_json(),
            "energy_worker": c.energy.as_ref().map(|e| json!({"worker": e.id, "was_greedy": e.was_greedy, "since": e.since / 30})),
            "station": c.station.as_ref().map(|s| json!({"crew": s.crew, "x": s.center.x.round(), "z": s.center.z.round(), "since": s.since / 30, "leash": !self.dev_off("conquestleash")})),
            "op": c.op.as_ref().map(|op| {
                let guards: Vec<serde_json::Value> = (vec![("home", op.home_guard), ("objective", op.guard)].into_iter())
                    .filter_map(|(w, g)| g.map(|(t, _)| json!({"where": w, "built": t.built, "task": t.task})))
                    .collect();
                json!({
                "objective": op.idx, "crew": op.crew, "stage": op.stage.name(), "since": op.started / 30,
                "mex_jobs": op.mex_tasks.len(),
                "turrets": op.turrets.iter().map(|t| json!({"x": t.site.x.round(), "z": t.site.z.round(), "task": t.task, "built": t.built})).collect::<Vec<_>>(),
                "guards": guards,
                "added": op.added,
            })}),
            "held": c.held.iter().map(|h| json!({"objective": h.idx, "added": h.added})).collect::<Vec<_>>(),
            "control": c.control.iter().map(|k| k.hold.name()).collect::<Vec<_>>(),
            "ground": c.ground.iter().filter(|&&w| w >= GROUND_SAFE).count(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use springai::Unit;
    use std::cell::Cell;

    thread_local! {
        static COMMANDER_HOME: Cell<bool> = const { Cell::new(false) };
    }

    /// Commander 10 (out, or home), constructors 11 (far from home) and
    /// 12 (near), both greedy; home is our start at (1000, 1000).
    fn energy_ai() -> Ai {
        unsafe extern "C" fn team(_: i32, _: i32) -> i32 {
            2
        }
        unsafe extern "C" fn health(_: i32, _: i32) -> f32 {
            1000.0
        }
        unsafe extern "C" fn bp(_: i32, _: i32) -> f32 {
            5.0
        }
        unsafe extern "C" fn zero(_: i32, _: i32) -> i32 {
            0
        }
        unsafe extern "C" fn pos(_: i32, id: i32, out: *mut f32) {
            let p = match id {
                10 if COMMANDER_HOME.with(Cell::get) => [1200.0, 0.0, 1000.0],
                10 => [4000.0, 0.0, 4000.0],
                11 => [2400.0, 0.0, 1000.0],
                _ => [1300.0, 0.0, 1100.0],
            };
            unsafe { std::ptr::copy_nonoverlapping(p.as_ptr(), out, 3) };
        }
        COMMANDER_HOME.with(|h| h.set(false));
        let mut ai = super::super::commander::tests::test_ai_with(|t| {
            t.Unit_getTeam = Some(team);
            t.Unit_getHealth = Some(health);
            t.Unit_getPos = Some(pos);
            t.UnitDef_getBuildSpeed = Some(bp);
            t.Unit_getMaxHealth = Some(health);
            t.UnitDef_getWeaponMounts = Some(zero);
            t.Unit_getWeapons = Some(zero);
        });
        ai.graph.set_start_pos(Vec3::new(1000.0, 0.0, 1000.0));
        for (id, com, greedy) in [(10, true, false), (11, false, true), (12, false, true)] {
            let mut w = super::super::Worker::new(Unit::from_id(ai.cb, id).unwrap(), ai.team_id);
            w.is_com = com;
            w.is_greedy = greedy;
            ai.eco.insert_worker(w);
        }
        ai.eco.commanders.push(10);
        ai.eco.greed = 2.0;
        ai
    }

    fn walking() -> Op {
        Op {
            idx: 0,
            crew: 10,
            stage: Stage::Walk,
            mex_tasks: vec![],
            turrets: vec![],
            started: 0,
            fortify_from: 0,
            danger: 0,
            home_guard: None,
            guard: None,
            support: false,
            help: Vec::new(),
            sites: 0,
            added: 0,
            last_attack: 0,
        }
    }

    #[test]
    fn the_price_clock_runs_from_two_to_one_between_three_and_five_minutes() {
        assert_eq!(metal_price(0), 2.0);
        assert_eq!(metal_price(30 * 180), 2.0);
        assert!((metal_price(30 * 240) - 1.5).abs() < 1e-4);
        assert_eq!(metal_price(30 * 300), 1.0);
        assert_eq!(metal_price(30 * 600), 1.0);
    }

    fn greedy(ai: &Ai, id: i32) -> bool {
        ai.eco.workers.get(&id).unwrap().is_greedy
    }

    #[test]
    fn the_constructor_nearest_home_builds_energy_until_the_commander_is_back() {
        let mut ai = energy_ai();
        ai.eco.conquest.op = Some(walking());
        ai.conquest_energy(600);
        let e = ai.eco.conquest.energy.as_ref().expect("an energy worker");
        assert_eq!((e.id, e.was_greedy), (12, true));
        assert!(!greedy(&ai, 12) && greedy(&ai, 11));
        assert_eq!(ai.eco.greed, 1.0);
        // The objective over, the commander still out: it stays on.
        ai.eco.conquest.op = None;
        ai.conquest_energy(900);
        assert!(ai.eco.conquest.energy.is_some());
        // The commander home: greedy again.
        COMMANDER_HOME.with(|h| h.set(true));
        ai.conquest_energy(1200);
        assert!(ai.eco.conquest.energy.is_none());
        assert!(greedy(&ai, 12));
        assert_eq!(ai.eco.greed, 2.0);
    }

    #[test]
    fn without_a_commander_the_energy_worker_stays_non_greedy() {
        let mut ai = energy_ai();
        ai.eco.conquest.op = Some(walking());
        ai.conquest_energy(600);
        ai.eco.conquest.op = None;
        ai.eco.commanders.clear();
        ai.conquest_energy(900);
        assert!(ai.eco.conquest.energy.is_none());
        assert!(!greedy(&ai, 12));
        assert_eq!(ai.eco.greed, 1.0);
    }

    #[test]
    fn the_leash_prices_the_commanders_far_jobs() {
        let mut ai = energy_ai();
        assert!(ai.dev_off("conquestleash"), "off by default");
        ai.set_feature_off("conquestleash", false);
        let center = Vec3::new(4000.0, 0.0, 4000.0);
        ai.eco.conquest.station = Some(Station { crew: 10, center, since: 0 });
        // Near the station, and anyone else's job: nothing.
        assert_eq!(ai.conquest_leash(10, Vec3::new(4000.0, 0.0, 5000.0)), 0.0);
        assert_eq!(ai.conquest_leash(12, Vec3::new(1000.0, 0.0, 1000.0)), 0.0);
        // Home, 4243 elmos away: the walk past the radius.
        let home = ai.conquest_leash(10, Vec3::new(1000.0, 0.0, 1000.0));
        assert!((home - LEASH_COST * (3000.0 * 2f32.sqrt() - LEASH_RADIUS)).abs() < 1.0, "{home}");
        // Switched off: nothing.
        ai.set_feature_off("conquestleash", true);
        assert_eq!(ai.conquest_leash(10, Vec3::new(1000.0, 0.0, 1000.0)), 0.0);
    }

    #[test]
    fn a_failed_objective_frees_the_commander_a_held_one_keeps_it() {
        let mut ai = energy_ai();
        for (why, success, kept) in [("held", true, true), ("commander hurt", false, false)] {
            ai.eco.conquest.op = Some(walking());
            ai.eco.conquest.station = Some(Station { crew: 10, center: Vec3::new(4000.0, 0.0, 4000.0), since: 0 });
            ai.conquest_end(why, success);
            assert_eq!(ai.eco.conquest.station.is_some(), kept, "{why}");
        }
    }

    #[test]
    fn with_conquestseq_a_held_objective_leads_on_and_a_failed_one_stops() {
        let mut ai = energy_ai();
        let mut op = walking();
        op.idx = 2;
        ai.eco.conquest.op = Some(op);
        ai.conquest_end("held", true);
        assert_eq!((ai.eco.conquest.done, ai.eco.conquest.taken.clone()), (true, vec![2]), "off by default: one");
        ai.set_feature_off("conquestseq", false);
        ai.eco.conquest.done = false;
        ai.eco.conquest.op = Some(walking());
        ai.conquest_end("held", true);
        assert_eq!((ai.eco.conquest.done, ai.eco.conquest.taken.clone()), (false, vec![2, 0]));
        ai.eco.conquest.op = Some(walking());
        ai.conquest_end("commander hurt", false);
        assert_eq!((ai.eco.conquest.done, ai.eco.conquest.taken.len()), (true, 2));
    }

    #[test]
    fn the_prior_is_a_distribution_over_ground_tiers() {
        let p = prior();
        assert!((p.iter().map(|x| x.1).sum::<f32>() - 1.0).abs() < 1e-5);
        assert!(p.iter().all(|(t, _)| TIERS.contains(t)));
    }
}
