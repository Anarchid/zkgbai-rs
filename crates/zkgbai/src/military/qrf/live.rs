//! The QRF in normal play.
//!
//! The force has two parts:
//! - *dedicated* teams: anti-raiders (light raiders where no factory of
//!   ours makes those, see [`ANTI_RAIDERS`]) taken as they leave the
//!   factory while the defense budget wants more, and given back to the
//!   usual handlers when it wants less. Short of the budget for a while,
//!   factories build them in place of plain fighters (`qrf_production`).
//!   Hurt members go home to be repaired like any unit (the retreat
//!   handler) and rejoin when healed.
//! - *moonlighting* squads: raider squads, and assault squads fast enough,
//!   still forming (waiting at their rally point for the rest of their
//!   members) answer raids near home in the meantime. A squad sent at a
//!   raid (*drafted*) is the QRF's to move until the raid is dealt with; it
//!   keeps taking new members, and leaves on its own errand only once it
//!   is let go. The incursion planner uses squads past forming, so the two
//!   never contend for a squad.
//!
//! The budget (AI options `qrf_defense`, `qrf_cap`, `qrf_gain`; the
//! operator's `qrf_set`): on autopilot, nothing until something is lost
//! to a raid; then enough power for as many teams as
//! raids came at once, each `gain` times a typical raid lately, less
//! half of what moonlights, at most `cap` of our mobile army.
//!
//! Raids are the enemy groups near our value and not in their territory,
//! and only those our force could take on (no stronger than twice it): a
//! main army is the army's and the incursion planner's. Teams stand off
//! enemy defenses. The algorithm on autopilot is `mark` (see [`super::mark`]);
//! the operator can give a team a role (a zone, a post, a place to hold)
//! and a tactic (mark, shadow, hunt), and the roles are kept until cleared.

use std::collections::{BTreeMap, VecDeque};

use serde_json::json;
use springai::{Unit, Vec3};

use super::field::{Grid, Mobility};
use super::{Member, PERIOD, RaidGroup};
use crate::Ai;
use crate::military::{Fighter, SquadId, SquadKind};
use crate::util::distance;

/// Team ids of moonlighting squads: this plus the squad id.
pub const MOON: usize = 1 << 20;

/// Normal play starts the QRF this late (frames), once the map is known.
const START: i32 = 30 * 10;

/// Raids are enemy groups this close to our value (elmos).
const NEAR: f32 = 2200.0;

/// An enemy group in their territory still counts this close to our value.
const NEAR_IN_THEIRS: f32 = 800.0;

/// Groups stronger than this many times our force are an army, not a raid.
const ARMY_RATIO: f32 = 2.0;

/// The budget counts raid-like groups: their slowest at least this fast
/// (elmos per second; Bandits 93, Scythes 90, Panthers 87, not Scalpels or
/// assaults), or this weak; and never stronger than this, or this share
/// of our army.
const RAID_SPEED: f32 = 80.0;
pub(super) const RAID_WEAK: f32 = 400.0;
const RAID_FLOOR: f32 = 800.0;
const RAID_SHARE: f32 = 0.5;

/// Raids are remembered this long for the budget (frames).
const RECALL: i32 = 30 * 180;

/// The dedicated teams never take more than this share of the recruit-class
/// units the factories built in the last [`RECALL`]: a factory whose riots
/// all vanish at the door refills them and never builds its assaults (Hide
/// and Seek, Venoms taken 8 to 1 against Hermits 1 to 9).
const RECRUIT_SHARE: f32 = 0.5;

/// Losses to raids are remembered this long for the budget (frames).
const LOSS_RECALL: i32 = 30 * 300;

/// How the budget sizes a team against the raids seen (AI option
/// `qrf_size`, the operator's `qrf_set size`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Sizing {
    /// A typical raid: the 75th percentile of raid power over [`RECALL`].
    Typical,
    /// The largest raid-like group seen over [`RECALL`].
    Largest,
    /// The raids that cost us: the 75th percentile of raid power weighted
    /// by the metal lost to each over [`LOSS_RECALL`], never under typical;
    /// and the budget wants nothing until something was lost to a raid
    /// (the default: ZKGBAI's own raiders leave when a fighter shows and a
    /// dedicated force buys nothing in mirrors, the baselines' Daggers stay
    /// and kill).
    Lossy,
}

impl Sizing {
    pub fn name(self) -> &'static str {
        match self {
            Sizing::Typical => "typical",
            Sizing::Largest => "largest",
            Sizing::Lossy => "lossy",
        }
    }

    pub fn parse(s: &str) -> Option<Sizing> {
        match s.trim() {
            "typical" => Some(Sizing::Typical),
            "largest" => Some(Sizing::Largest),
            "lossy" => Some(Sizing::Lossy),
            _ => None,
        }
    }
}

/// Defaults: dedicated power per unit of a typical raid's; the
/// ceiling, as a share of our mobile army.
pub const GAIN: f32 = 1.5;
pub const CAP: f32 = 0.3;

/// Teams are let go when the force is over budget by this share (plus a
/// raider) for this long.
const SLACK: f32 = 0.3;
const OVER_FOR: i32 = 30 * 30;

/// A drafted squad is let go this long after it was last sent (frames).
/// A draft lapses this long (frames) after its last match or press, and
/// a drafted squad unmatched this long (its raid turned, or flickers out
/// of radar) is let go. In seconds: `PERIOD` is half a second, and with
/// the hold and the release at two periods a drafted squad was let go
/// one second after its raid blinked and taken again the next period,
/// 18-24 redrafts a game (Oct 23-24).
const DRAFT_HOLD: i32 = 30 * 20;
const DRAFT_IDLE: i32 = 30 * 20;
/// A released squad is not drafted again this soon (frames), and a drafted
/// one is not released while the raid it was sent at is still pressing
/// (within `PRESSED_RECALL`).
pub(super) const DRAFT_COOLDOWN: i32 = 30 * 60;
pub(super) const PRESSED_RECALL: i32 = 30 * 60;

/// Factories build raiders for the force once it has been short of the
/// budget (by more than a raider) this long; an order is counted as on
/// its way for this long (frames).
const SHORT_FOR: i32 = 30 * 20;
const ORDER_PENDING: i32 = 30 * 60;

/// What the dedicated teams are made of. Early on, light raiders (cheap,
/// and as good for offense when let go: the flexible pick); later (see
/// [`LATE_INCOME`]), anti-raider raiders and light riots (Blitz, Bolas,
/// Pyro, Venom, Archer: 180-300 metal, 700-1300 hp, ranges of 225-255 over
/// the light raiders' 185-210, fast enough to get there: they hold a mex
/// against raids), with light raiders then only where no factory of ours
/// makes the former. A factory short of the budget builds the first of
/// the phase's picks it can.
// Venom is back (user, 2026-10-26: the riot-raider is the pick for holding
// a mex against raids). It was left out from live15 on because a Venom
// ordered is a Redback or Recluse not built (mirrors on Hide and Seek 0-6-2).
pub const ANTI_RAIDERS: [&str; 5] = ["tankheavyraid", "hoverheavyraid", "jumpraid", "amphimpulse", "spideremp"];
pub const RAIDERS: [&str; 6] = ["shieldraid", "vehraid", "cloakraid", "tankraid", "hoverraid", "amphraid"];

/// The dedicated picks are made from this income (metal per second), or
/// this late (frames), on.
pub const LATE_INCOME: f32 = 20.0;
pub const LATE_FRAME: i32 = 30 * 60 * 6;

/// Whether the dedicated teams take this kind of unit at all.
pub fn is_recruit(name: &str) -> bool {
    ANTI_RAIDERS.contains(&name) || RAIDERS.contains(&name)
}

/// Assault squads moonlight only if their slowest goes this fast (Archers
/// 70, Venoms 84; not Reavers, Outlaws, Maces at 52-61).
const MOON_SPEED: f32 = 65.0;

/// The budget's mode.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Mode {
    /// No dedicated force: moonlighting only, until an operator sets one.
    /// (Tried as the default, Oct 23: it gave up the Agalta gain, 15 wins
    /// and 5.0 mexes lost to 12 and 7.2, and gained nothing at 15:00 in
    /// self-play or against Circuit over the capped auto force.)
    Off,
    /// From the raids seen and the losses (see the module doc).
    Auto,
    /// This much power (metal).
    Metal(f32),
    /// This share of our mobile army.
    Share(f32),
}

pub struct Budget {
    pub mode: Mode,
    pub cap: f32,
    pub gain: f32,
    /// Dedicated power wanted, and had (metal, by health).
    pub wanted: f32,
    pub have: f32,
    /// Our mobile army (metal).
    pub army: f32,
    /// Moonlighting squads' power.
    pub moonlight: f32,
    pub reason: String,
    pub size: Sizing,
    /// Raids near our value: frame, each raid's power.
    sightings: VecDeque<(i32, Vec<f32>)>,
    /// Our assets lost to raids: frame, the raid's power, the metal lost.
    loss_raids: VecDeque<(i32, f32, f32)>,
    over_since: Option<i32>,
    short_since: Option<i32>,
    /// Raiders ordered from factories for the force: frame, metal.
    pub pending: Vec<(i32, f32)>,
    /// Recruit-class units that left a factory lately: frame, taken.
    door: VecDeque<(i32, bool)>,
    /// Factories may build raiders for the force (`qrf_production`).
    pub production: bool,
    /// Some factory of ours makes anti-raiders (light raiders are then
    /// left to raid).
    pub anti_raiders: bool,
}

impl Default for Budget {
    fn default() -> Self {
        Budget {
            mode: Mode::Auto,
            cap: CAP,
            gain: GAIN,
            wanted: 0.0,
            have: 0.0,
            army: 0.0,
            moonlight: 0.0,
            reason: "quiet".into(),
            size: Sizing::Lossy,
            sightings: VecDeque::new(),
            loss_raids: VecDeque::new(),
            over_since: None,
            short_since: None,
            pending: Vec::new(),
            door: VecDeque::new(),
            production: false,
            anti_raiders: false,
        }
    }
}

/// The 75th percentile.
fn p75(mut xs: Vec<f32>) -> f32 {
    if xs.is_empty() {
        return 0.0;
    }
    xs.sort_by(f32::total_cmp);
    xs[((xs.len() - 1) as f32 * 0.75).round() as usize]
}

/// The 75th percentile of `xs` by weight: the least value at or above
/// which three quarters of the weight lies.
fn weighted_p75(mut xs: Vec<(f32, f32)>) -> f32 {
    xs.retain(|x| x.1 > 0.0);
    if xs.is_empty() {
        return 0.0;
    }
    xs.sort_by(|a, b| a.0.total_cmp(&b.0));
    let total: f32 = xs.iter().map(|x| x.1).sum();
    let mut acc = 0.0;
    for &(v, w) in &xs {
        acc += w;
        if acc >= 0.75 * total {
            return v;
        }
    }
    xs.last().map_or(0.0, |x| x.0)
}

impl Budget {
    /// Raids at once, and a raid's power, lately (75th percentiles over the
    /// times raids were near).
    pub fn recent(&self) -> (usize, f32) {
        let n = p75(self.sightings.iter().map(|s| s.1.len() as f32).collect()).round() as usize;
        let typical = p75(self.sightings.iter().flat_map(|s| s.1.iter().copied()).collect());
        (n, typical)
    }

    /// The raid power a team is sized against, by [`Sizing`].
    pub fn strongest(&self) -> f32 {
        let typical = self.recent().1;
        match self.size {
            Sizing::Typical => typical,
            Sizing::Largest => (self.sightings.iter().flat_map(|s| s.1.iter().copied())).fold(typical, f32::max),
            Sizing::Lossy => weighted_p75(self.loss_raids.iter().map(|l| (l.1, l.2)).collect()).max(typical),
        }
    }

    /// Whether the budget is armed: raids seen, or (lossy) something lost
    /// to one lately.
    pub fn armed(&self) -> bool {
        match self.size {
            Sizing::Lossy => !self.loss_raids.is_empty(),
            _ => !self.sightings.is_empty(),
        }
    }

    /// A recruit-class unit left a factory; whether the cap lets the teams
    /// take it (counting it).
    pub fn may_recruit(&mut self, frame: i32, take: bool) -> bool {
        while self.door.front().is_some_and(|d| frame - d.0 > RECALL) {
            self.door.pop_front();
        }
        let built = self.door.len() as f32 + 1.0;
        let taken = self.door.iter().filter(|d| d.1).count() as f32;
        let ok = take && taken + 1.0 <= RECRUIT_SHARE * built;
        self.door.push_back((frame, ok));
        ok
    }

    /// An asset of ours lost to a raid of this power.
    pub fn lost_to(&mut self, frame: i32, raid_power: f32, cost: f32) {
        self.loss_raids.push_back((frame, raid_power, cost));
        while self.loss_raids.front().is_some_and(|l| frame - l.0 > LOSS_RECALL) {
            self.loss_raids.pop_front();
        }
    }
}

/// The autopilot's want: `gain` times a typical recent raid, for as
/// many teams as raids came at once (up to three), more for losses, less
/// half of what moonlights and the metal of the turrets the posture keeps
/// for the same job (`turrets`, see `posture`: on a lane map the raids
/// the force answers are partly answered already, and a force sized as if
/// they were not keeps the army home), at most `cap` of the army.
#[allow(clippy::too_many_arguments)]
pub fn auto_wanted(
    raids: usize,
    strongest: f32,
    losses: f32,
    moonlight: f32,
    turrets: f32,
    army: f32,
    gain: f32,
    cap: f32,
) -> (f32, String) {
    if raids == 0 && losses <= 0.0 {
        return (0.0, "quiet".into());
    }
    let teams = raids.clamp(1, 3) as f32;
    let per_team = gain * strongest.max(150.0);
    let need = per_team * teams * (1.0 + 0.25 * losses.min(4.0));
    let ceiling = cap * army;
    let want = (need - 0.5 * moonlight - turrets).max(0.0);
    if want > ceiling {
        (ceiling, format!("capped: {teams:.0} x {per_team:.0} (losses {losses:.1}) over {cap:.2} of {army:.0}"))
    } else {
        (
            want,
            format!(
                "{teams:.0} x {per_team:.0} (losses {losses:.1}) less half of {moonlight:.0} moonlighting and {turrets:.0} in posture turrets"
            ),
        )
    }
}

/// What the operator wants of a team.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum RoleKind {
    /// The algorithm's choice.
    Auto,
    /// Only raids on our value within `radius` of `pos`; waits there.
    Zone,
    /// Waits at `pos` (answers raids anywhere).
    Post,
    /// Stands at `pos`, always.
    Hold,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Tactic {
    /// Mark and strike when clearly stronger (the algorithm's own).
    Mark,
    /// Mark, never strike: keep the force.
    Shadow,
    /// Strike when as strong, and meet raids on their way.
    Hunt,
}

#[derive(Clone, Copy, Debug)]
pub struct Role {
    pub kind: RoleKind,
    pub pos: Vec3,
    pub radius: f32,
    pub tactic: Tactic,
}

impl RoleKind {
    pub fn name(self) -> &'static str {
        match self {
            RoleKind::Auto => "auto",
            RoleKind::Zone => "zone",
            RoleKind::Post => "post",
            RoleKind::Hold => "hold",
        }
    }
}

impl Tactic {
    pub fn name(self) -> &'static str {
        match self {
            Tactic::Mark => "mark",
            Tactic::Shadow => "shadow",
            Tactic::Hunt => "hunt",
        }
    }
}

/// What a team did last: `mark`, `goal`, `strike`, `screen`, `post`,
/// `hold`, or `free` (moonlighters not sent).
#[derive(Clone, Copy, Debug)]
pub struct Doing {
    pub what: &'static str,
    pub at: Vec3,
    /// The power of the raid it marks (0: none).
    pub raid: f32,
    pub frame: i32,
}

#[derive(Default)]
pub struct Stats {
    /// Raiders ordered from factories for the force.
    pub ordered: u32,
    pub recruited: u32,
    pub released: u32,
    pub drafts: u32,
    /// Squads drafted again after a release (thrash, if many).
    pub redrafts: u32,
    /// Our assets lost, and of those while some team marked the raid.
    pub losses: u32,
    pub losses_marked: u32,
    pub losses_metal: f32,
}

impl Ai {
    /// Whether to start the QRF in normal play now.
    pub(super) fn qrf_live_wanted(&self, frame: i32) -> bool {
        frame >= START && !self.raid_testing() && !self.dev_off("qrf") && self.qrf.grid.is_none()
    }

    pub(super) fn qrf_live_start(&mut self) {
        let opt = |k: &str| self.cb.option(k).filter(|v| !v.trim().is_empty());
        let num = |k: &str| opt(k).and_then(|v| v.trim().parse::<f32>().ok());
        let q = &mut self.qrf;
        q.on = true;
        q.live = true;
        q.algo = opt("qrf_algo").unwrap_or_else(|| "mark".into());
        q.team_size = opt("qrf_team").and_then(|v| v.parse().ok()).unwrap_or(3).max(1);
        q.trace = opt("qrf_trace").and_then(|v| v.parse().ok()).unwrap_or(0);
        q.no_moonlight = opt("qrf_moonlight").is_some_and(|v| matches!(v.as_str(), "off" | "0" | "false"));
        q.budget.mode = match opt("qrf_defense").as_deref() {
            None | Some("") | Some("auto") => Mode::Auto,
            Some("off") => Mode::Off,
            Some(v) => match v.strip_suffix('%') {
                Some(p) => Mode::Share(p.parse::<f32>().unwrap_or(30.0).clamp(0.0, 100.0) / 100.0),
                None => Mode::Metal(v.parse::<f32>().unwrap_or(0.0).max(0.0)),
            },
        };
        q.budget.cap = num("qrf_cap").unwrap_or(CAP).clamp(0.0, 1.0);
        q.budget.gain = num("qrf_gain").unwrap_or(GAIN).max(0.0);
        q.budget.size = opt("qrf_size").and_then(|v| Sizing::parse(&v)).unwrap_or(Sizing::Lossy);
        q.budget.production = opt("qrf_production").is_some_and(|v| matches!(v.as_str(), "on" | "1" | "true"));
        q.grid = Some(Grid::new(self.cb));
        q.candidates = super::candidates(self.cb);
        let line = format!(
            "qrf: live algo={} team={} moonlight={} defense={:?} cap={} gain={} size={}",
            q.algo,
            q.team_size,
            !q.no_moonlight,
            q.budget.mode,
            q.budget.cap,
            q.budget.gain,
            q.budget.size.name()
        );
        self.log(&line);
    }

    /// Whether the dedicated picks are made now (past the early game).
    pub fn qrf_late(&self) -> bool {
        self.eco.adjusted_income >= LATE_INCOME || self.frame >= LATE_FRAME
    }

    /// An asset of ours (a static or a worker) took damage: the attacker's
    /// track has pressed, which is what lets a moonlighting squad be sent
    /// at its raid (a sighting alone drafted 30 squads a game against
    /// Circuit for 3-4 raids that ever touched a mex).
    pub fn qrf_asset_pressed(&mut self, unit: Unit, attacker: Option<Unit>) {
        if !self.qrf.on || !self.qrf.live {
            return;
        }
        let Some(a) = attacker else { return };
        let Some(def) = unit.def() else { return };
        let worker = def.speed() > 0.0 && !def.build_options().is_empty();
        if def.speed() > 0.0 && !worker || self.defs.name(def) == "terraunit" {
            return;
        }
        let frame = self.frame;
        let (id, on) = (a.id(), unit.id());
        if let Some(t) = self.qrf.tracks.get_mut(&id) {
            t.pressed = frame;
            t.pressed_on = on;
        }
        for g in &mut self.qrf.groups {
            if g.members.contains(&id) {
                g.pressed = frame;
                if !g.pressed_on.contains(&on) {
                    g.pressed_on.push(on);
                }
            }
        }
    }

    /// Whether `sid` is a moonlighting squad the QRF sent at a raid (its
    /// handler then leaves it be).
    pub fn qrf_drafted(&self, sid: SquadId) -> bool {
        self.qrf.live && self.qrf.drafted.contains_key(&sid)
    }

    /// Moonlighting squads: forming raider squads at home (and those
    /// drafted, until let go), their members on the roster as teams of
    /// their own.
    pub(super) fn qrf_live_roster(&mut self, frame: i32) {
        let moonlight = !self.qrf.no_moonlight && self.qrf.algo == "mark";
        let mut eligible: BTreeMap<SquadId, Vec<i32>> = BTreeMap::new();
        if moonlight {
            // Raider squads, and assault squads fast enough (light riots,
            // skirmishers); not shield balls, which move with their leader.
            let squads = self.raider_handler.raider_squads.iter().chain(&self.squad_handler.squads);
            for &sid in squads {
                let Some(sq) = self.squads.get(sid) else { continue };
                let drafted = self.qrf.drafted.contains_key(&sid);
                let slowest = (sq.fighters.iter())
                    .filter_map(|id| self.fighters.get(id)?.unit.def())
                    .map(|d| d.speed())
                    .fold(f32::INFINITY, f32::min);
                if !matches!(sq.kind, SquadKind::Raider | SquadKind::Assault | SquadKind::Hover)
                    || sq.kind != SquadKind::Raider && slowest < MOON_SPEED
                    || sq.is_air_squad
                    || sq.is_dead(&self.fighters)
                    || !(sq.status == 'f' || drafted && sq.status == 'r')
                    || self.op.over.squad_intents.contains_key(&sid)
                    || self.op.over.squad_targets.contains_key(&sid)
                    || self.incursions.orders.contains_key(&sid)
                {
                    continue;
                }
                let pos = sq.get_pos(&self.fighters);
                if !drafted && self.graph.is_enemy_territory(pos) {
                    continue;
                }
                let ids: Vec<i32> = (sq.fighters.iter().copied())
                    .filter(|id| self.fighters.get(id).is_some_and(|f| f.unit.health() > 0.0))
                    .collect();
                if !ids.is_empty() {
                    eligible.insert(sid, ids);
                }
            }
        }
        let mut left: Vec<(SquadId, String)> = Vec::new();
        self.qrf.drafted.retain(|sid, at| {
            let keep = eligible.contains_key(sid) && frame - *at <= DRAFT_HOLD;
            if !keep {
                let why = if !eligible.contains_key(sid) {
                    let sq = self.squads.get(*sid);
                    format!(
                        "ineligible: status {} kind {:?} dead {} intent {} incursion {}",
                        sq.map_or('?', |s| s.status),
                        sq.map(|s| s.kind),
                        sq.is_none_or(|s| s.is_dead(&self.fighters)),
                        self.op.over.squad_intents.contains_key(sid) || self.op.over.squad_targets.contains_key(sid),
                        self.incursions.orders.contains_key(sid)
                    )
                } else {
                    format!("hold expired ({} s since refresh)", (frame - *at) / 30)
                };
                left.push((*sid, why));
            }
            keep
        });
        for (sid, why) in left {
            self.qrf.draft_left.insert(sid, (frame, why));
            // Dropped is released: the cooldown applies, or the thrash
            // moves here.
            self.qrf.draft_released.insert(sid, frame);
            self.qrf.draft_unmatched.remove(&sid);
            self.qrf.draft_raid.remove(&sid);
        }
        self.qrf.members.retain(|id, m| m.squad.is_none_or(|s| eligible.get(&s).is_some_and(|ids| ids.contains(id))));
        let metal = self.metal;
        for (sid, ids) in eligible {
            for id in ids {
                if self.qrf.members.contains_key(&id) {
                    continue;
                }
                let Some(f) = self.fighters.get(&id) else { continue };
                let Some(def) = f.unit.def() else { continue };
                let Some(mobility) = Mobility::of(def) else { continue };
                let member = Member {
                    unit: f.unit,
                    def,
                    team: MOON + sid as usize,
                    cost: def.cost(metal),
                    speed: def.speed(),
                    mobility,
                    range: def.max_weapon_range(),
                    squad: Some(sid),
                    threat: f.power,
                    order: None,
                };
                self.qrf.members.insert(id, member);
            }
        }
    }

    /// Raids: enemy groups near our value, not deep in their territory,
    /// and no stronger than our force can take on. Also what the budget
    /// remembers of them.
    pub(super) fn qrf_live_filter(&mut self, frame: i32) {
        let assets: Vec<Vec3> = self.qrf.assets.iter().map(|a| a.pos).collect();
        let near = |c: Vec3| assets.iter().map(|&a| distance(a, c)).fold(f32::INFINITY, f32::min);
        let graph = &self.graph;
        self.qrf.groups.retain(|g| {
            let d = near(g.center);
            d < NEAR && (d < NEAR_IN_THEIRS || !graph.is_enemy_territory(g.center))
        });
        let biggest = RAID_FLOOR.max(RAID_SHARE * self.qrf.budget.army);
        let raids: Vec<f32> = (self.qrf.groups.iter())
            .filter(|g| (g.speed >= RAID_SPEED || g.power <= RAID_WEAK) && g.power <= biggest)
            .map(|g| g.power)
            .collect();
        if !raids.is_empty() {
            self.qrf.budget.sightings.push_back((frame, raids));
        }
        while self.qrf.budget.sightings.front().is_some_and(|s| frame - s.0 > RECALL) {
            self.qrf.budget.sightings.pop_front();
        }
        let teams = self.qrf_teams();
        let force: f32 = teams.values().map(|ids| self.qrf_team_power(ids)).sum();
        let army = ARMY_RATIO * force.max(150.0);
        // Raids too strong for the force are no targets, but they still
        // raid: kept aside so a loss to them counts. (With no force the cap
        // is 300; a human raiding in fours, or a Dagger beside a Dante,
        // was deleted here before the loss was attributed, the budget
        // stayed "quiet" through 12 losses, and the force never formed:
        // Theta Crystals, Oct 23.)
        let (groups, heavies): (Vec<RaidGroup>, Vec<RaidGroup>) =
            std::mem::take(&mut self.qrf.groups).into_iter().partition(|g| g.power <= army);
        self.qrf.groups = groups;
        self.qrf.heavies = heavies;
    }

    /// The raiding part of a group's power: its members that go raider
    /// speed or are light. A loss beside a strider is sized to the raiders
    /// with it, not to the strider; a lone strider's kill is no raid.
    fn qrf_raiders_power(&self, g: &RaidGroup) -> f32 {
        let metal = self.metal;
        (g.members.iter().filter_map(|id| self.qrf.tracks.get(id)))
            .map(|t| (t.max_speed(), t.power(metal)))
            .filter(|&(speed, power)| speed >= RAID_SPEED || power <= RAID_WEAK)
            .map(|(_, power)| power)
            .sum()
    }

    /// Our mobile army, the force, the want; teams let go when over.
    pub(super) fn qrf_live_budget(&mut self, frame: i32) {
        let metal = self.metal;
        let army: f32 = (self.cb.team_units().into_iter())
            .filter_map(|u| u.def())
            .filter(|d| d.speed() > 0.0 && d.build_options().is_empty() && d.num_weapon_mounts() > 0)
            .map(|d| d.cost(metal))
            .sum();
        let (mut have, mut moon) = (0.0, 0.0);
        for m in self.qrf.members.values() {
            if m.squad.is_some() {
                moon += m.power();
            } else {
                have += m.power();
            }
        }
        let losses = self.firefighters_pressure().0.max(0.0);
        // The posture's turrets answer raids too (see `auto_wanted`).
        let turrets = if self.posture_on() && !self.dev_off("turretcredit") { self.posture.defense } else { 0.0 };
        let anti_raiders = (self.fac.factories.values())
            .filter_map(|f| f.unit.def())
            .any(|d| d.build_options().iter().any(|o| ANTI_RAIDERS.contains(&self.defs.name(*o))));
        self.qrf.budget.anti_raiders = anti_raiders;
        let b = &mut self.qrf.budget;
        b.army = army;
        b.have = have;
        b.moonlight = moon;
        let raids = b.recent().0;
        let strongest = b.strongest();
        let (wanted, reason) = match b.mode {
            Mode::Off => (0.0, "off: moonlighting only (qrf_set defense sets a force)".into()),
            Mode::Auto if !b.armed() => (0.0, format!("quiet [{}: nothing lost to a raid lately]", b.size.name())),
            Mode::Auto => {
                let (w, r) = auto_wanted(raids, strongest, losses, moon, turrets, army, b.gain, b.cap);
                (w, format!("{r} [{} raid {strongest:.0}]", b.size.name()))
            }
            Mode::Metal(v) => (v, "set by the operator".into()),
            Mode::Share(s) => (s * army, format!("{:.0}% of the army, set by the operator", s * 100.0)),
        };
        b.wanted = wanted;
        b.reason = reason;
        b.pending.retain(|p| frame - p.0 < ORDER_PENDING);
        if wanted - have > 150.0 {
            b.short_since.get_or_insert(frame);
        } else {
            b.short_since = None;
        }
        // Over budget for a while: let the weakest free team go.
        let over = have > wanted * (1.0 + SLACK) + 150.0;
        if !over {
            b.over_since = None;
            return;
        }
        let since = *b.over_since.get_or_insert(frame);
        if frame - since < OVER_FOR {
            return;
        }
        let teams = self.qrf_teams();
        let busy = |t: usize| {
            self.qrf.doing.get(&t).is_some_and(|d| matches!(d.what, "mark" | "goal" | "strike") && frame - d.frame < 60)
        };
        let weakest = (teams.iter())
            .filter(|(t, _)| **t < MOON && !self.qrf.roles.contains_key(t) && !busy(**t))
            .min_by(|a, b| self.qrf_team_power(a.1).total_cmp(&self.qrf_team_power(b.1)))
            .map(|(&t, _)| t);
        if let Some(t) = weakest {
            self.qrf_release_team(t, "over budget");
            self.qrf.budget.over_since = Some(frame);
        }
    }

    /// Take a raider leaving the factory into a dedicated team, if the
    /// budget wants more.
    pub fn qrf_recruit(&mut self, unit: Unit) -> bool {
        let q = &self.qrf;
        if !q.on || !q.live || q.releasing {
            return false;
        }
        let Some(def) = unit.def() else { return false };
        let name = self.defs.name(def);
        // Late, light raiders only where no factory makes anti-raiders.
        let wanted =
            ANTI_RAIDERS.contains(&name) || RAIDERS.contains(&name) && (!self.qrf_late() || !q.budget.anti_raiders);
        if !wanted {
            return false;
        }
        let cost = def.cost(self.metal);
        let short = q.budget.have + 0.5 * cost <= q.budget.wanted;
        let frame = self.frame;
        // A unit the force ordered is its own; the door share only governs
        // what the factory built for itself. Otherwise the cap refuses the
        // ordered units, the force stays short, and the factory keeps
        // substituting raiders for its own picks.
        let ordered = q.budget.pending.iter().position(|p| (p.1 - cost).abs() < 1.0);
        if let Some(i) = ordered {
            if !short {
                return false;
            }
            self.qrf.budget.pending.remove(i);
        } else if !self.qrf.budget.may_recruit(frame, short) {
            return false;
        }
        let Some(mobility) = Mobility::of(def) else { return false };
        let team = self.qrf_open_team(unit.pos());
        let threat = Fighter::new(unit, cost, self.team_id).power;
        self.qrf.members.insert(
            unit.id(),
            Member {
                unit,
                def,
                team,
                cost,
                speed: def.speed(),
                mobility,
                range: def.max_weapon_range(),
                squad: None,
                threat,
                order: None,
            },
        );
        self.qrf.budget.have += cost;
        self.qrf.live_stats.recruited += 1;
        unit.set_move_state(1, 0);
        unit.set_fire_state(2, 0);
        true
    }

    /// A raider for the force from a factory about to build `choice`, if
    /// the force has been short of its budget a while (and the orders on
    /// their way do not make it up). Never in place of anti-air or a
    /// special unit (`choice` is a plain fighter).
    pub fn qrf_production(&mut self, factory: Unit, choice: &str) -> Option<&'static str> {
        let b = &self.qrf.budget;
        if !self.qrf.on || !self.qrf.live || !b.production {
            return None;
        }
        let frame = self.frame;
        if b.short_since.is_none_or(|s| frame - s < SHORT_FOR) || is_recruit(choice) {
            return None;
        }
        let def = self.defs.get(choice)?;
        if self.unit_classes.aas.contains(choice) || def.is_able_to_fly() || def.speed() <= 0.0 {
            return None;
        }
        let options = factory.def()?.build_options();
        let picks: Vec<&'static str> = if self.qrf_late() {
            ANTI_RAIDERS.iter().chain(&RAIDERS).copied().collect()
        } else {
            RAIDERS.iter().chain(&ANTI_RAIDERS).copied().collect()
        };
        let name = picks.into_iter().find(|n| options.iter().any(|d| self.defs.name(*d) == *n))?;
        let cost = self.defs.get(name)?.cost(self.metal);
        let pending: f32 = b.pending.iter().map(|p| p.1).sum();
        if b.wanted - b.have - pending < 0.5 * cost {
            return None;
        }
        self.qrf.budget.pending.push((frame, cost));
        self.qrf.live_stats.ordered += 1;
        Some(name)
    }

    /// Take units into a new team of the operator's (whatever handler had
    /// them lets go).
    pub fn qrf_draft_units(&mut self, units: &[Unit]) -> Option<usize> {
        let team = self.qrf.next_team;
        let mut any = false;
        for &u in units {
            let Some(def) = u.def() else { continue };
            let Some(mobility) = Mobility::of(def) else { continue };
            if self.qrf.members.get(&u.id()).is_some_and(|m| m.squad.is_none()) {
                if let Some(m) = self.qrf.members.get_mut(&u.id()) {
                    m.team = team;
                }
                any = true;
                continue;
            }
            springai::reserved::release(self.cb, u.id());
            self.raider_remove_unit(u);
            self.squad_remove_unit(u);
            self.misc_remove_unit(u);
            self.jaeger_remove_unit(u);
            self.fighters.remove(&u.id());
            self.war.new_units.remove(&u.id());
            let cost = def.cost(self.metal);
            let threat = Fighter::new(u, cost, self.team_id).power;
            self.qrf.members.insert(
                u.id(),
                Member {
                    unit: u,
                    def,
                    team,
                    cost,
                    speed: def.speed(),
                    mobility,
                    range: def.max_weapon_range(),
                    squad: None,
                    threat,
                    order: None,
                },
            );
            if !self.retreat_handler.is_coward(u.id()) {
                self.retreat_add_coward(u);
            }
            u.set_move_state(1, 0);
            u.set_fire_state(2, 0);
            any = true;
        }
        if !any {
            return None;
        }
        self.qrf.next_team += 1;
        Some(team)
    }

    /// The dedicated team a recruit joins: the nearest with room (and no
    /// role of the operator's), else a new one.
    fn qrf_open_team(&mut self, pos: Vec3) -> usize {
        let teams = self.qrf_teams();
        let size = self.qrf.team_size;
        let mut counts: BTreeMap<usize, usize> = BTreeMap::new();
        for m in self.qrf.members.values().filter(|m| m.squad.is_none()) {
            *counts.entry(m.team).or_default() += 1;
        }
        let open = (counts.iter())
            .filter(|(t, n)| **n < size && !self.qrf.roles.contains_key(t))
            .min_by(|a, b| {
                let p = |t: usize| teams.get(&t).map_or(pos, |ids| self.qrf_team_pos(ids).0);
                distance(p(*a.0), pos).total_cmp(&distance(p(*b.0), pos))
            })
            .map(|(&t, _)| t);
        open.unwrap_or_else(|| {
            let t = self.qrf.next_team;
            self.qrf.next_team += 1;
            t
        })
    }

    /// Give a dedicated team back to the usual handlers.
    pub fn qrf_release_team(&mut self, team: usize, why: &str) -> usize {
        let ids: Vec<i32> =
            (self.qrf.members.iter()).filter(|(_, m)| m.team == team && m.squad.is_none()).map(|(&id, _)| id).collect();
        self.qrf.releasing = true;
        for &id in &ids {
            let Some(m) = self.qrf.members.remove(&id) else { continue };
            if m.unit.health() <= 0.0 {
                continue;
            }
            // Picked up as a unit fresh from the factory.
            self.war.new_units.insert(id);
            self.war_unit_idle(m.unit);
        }
        self.qrf.releasing = false;
        self.qrf.roles.remove(&team);
        self.qrf.doing.remove(&team);
        self.qrf.live_stats.released += ids.len() as u32;
        let line = format!("qrf: team {team} let go ({} units): {why}", ids.len());
        self.log(&line);
        ids.len()
    }

    /// After the algorithm: moonlighting squads sent at a raid are drafted
    /// (their handler leaves them be), the others let go.
    pub(super) fn qrf_live_after(&mut self, frame: i32) {
        let sent: Vec<(SquadId, bool)> = (self.qrf.doing.iter())
            .filter(|(t, _)| **t >= MOON)
            .map(|(&t, d)| ((t - MOON) as SquadId, d.frame == frame && matches!(d.what, "mark" | "goal" | "strike")))
            .collect();
        for (sid, now) in sent {
            if now {
                self.qrf.draft_unmatched.remove(&sid);
                let raid = self.qrf.mark.marks.get(&(MOON + sid as usize)).cloned().unwrap_or_default();
                if self.qrf.drafted.insert(sid, frame).is_none() {
                    if self.qrf.drafted_ever.insert(sid) {
                        self.qrf.live_stats.drafts += 1;
                    } else {
                        self.qrf.live_stats.redrafts += 1;
                        let (at, why) = self.qrf.draft_left.get(&sid).cloned().unwrap_or((frame, "never left?".into()));
                        let line = format!("qrf: redraft squad {sid} after {} s, left: {why}", (frame - at) / 30);
                        self.log(&line);
                    }
                }
                if !raid.is_empty() {
                    self.qrf.draft_raid.insert(sid, raid);
                }
            } else if self.qrf.drafted.contains_key(&sid) {
                // Still pressing: the squad stays with it (a mark comes
                // and goes with radar).
                let pressing = self.qrf.draft_raid.get(&sid).is_some_and(|ids| {
                    (self.qrf.groups.iter())
                        .any(|g| frame - g.pressed <= PRESSED_RECALL && g.members.iter().any(|m| ids.contains(m)))
                });
                if pressing {
                    // (And the hold runs from now.)
                    self.qrf.draft_unmatched.remove(&sid);
                    self.qrf.drafted.insert(sid, frame);
                    continue;
                }
                // Unmatched `DRAFT_IDLE` running (the raid turned, or stopped
                // threatening anything): let the squad go, and not take it
                // again for a while.
                let since = *self.qrf.draft_unmatched.entry(sid).or_insert(frame);
                if frame - since >= DRAFT_IDLE {
                    self.qrf.drafted.remove(&sid);
                    self.qrf.draft_unmatched.remove(&sid);
                    self.qrf.draft_raid.remove(&sid);
                    self.qrf.draft_released.insert(sid, frame);
                    self.qrf
                        .draft_left
                        .insert(sid, (frame, format!("released (unmatched {} s)", (frame - since) / 30)));
                }
            }
        }
        self.qrf.draft_released.retain(|_, f| frame - *f < DRAFT_COOLDOWN);
        // Moonlighters gone: forget what they did.
        let members = &self.qrf.members;
        self.qrf.doing.retain(|t, _| members.values().any(|m| m.team == *t));
        if frame % (30 * 30) < PERIOD {
            self.qrf_live_journal(frame);
        }
    }

    /// The QRF's state as a journal record (every 30 s).
    fn qrf_live_journal(&mut self, frame: i32) {
        let b = &self.qrf.budget;
        let s = &self.qrf.live_stats;
        let teams = self.qrf_teams();
        let dedicated = teams.keys().filter(|t| **t < MOON).count();
        let moon = teams.keys().filter(|t| **t >= MOON).count();
        let line = format!(
            "qrflive: t={} wanted={:.0} have={:.0} army={:.0} moonlight={:.0} teams={dedicated} moonlighters={moon} \
             drafted={} raids={} ordered={} recruited={} released={} drafts={} losses={} marked={} lost_metal={:.0} reason={}",
            frame / 30,
            b.wanted,
            b.have,
            b.army,
            b.moonlight,
            self.qrf.drafted.len(),
            self.qrf.groups.len(),
            s.ordered,
            s.recruited,
            s.released,
            s.drafts,
            s.losses,
            s.losses_marked,
            s.losses_metal,
            b.reason.replace(' ', "_"),
        );
        self.log(&line);
        let v = json!({
            "wanted": b.wanted.round(), "have": b.have.round(), "army": b.army.round(), "moonlight": b.moonlight.round(),
            "mode": format!("{:?}", b.mode), "reason": b.reason, "teams": dedicated, "moonlighters": moon,
            "drafted": self.qrf.drafted.len(), "raids": self.qrf.groups.len(),
            "ordered": s.ordered, "recruited": s.recruited, "released": s.released, "drafts": s.drafts, "redrafts": s.redrafts,
            "losses": s.losses, "losses_marked": s.losses_marked, "losses_metal": s.losses_metal.round(),
        });
        self.op_emit("qrf", v);
    }

    /// One of our assets died: who was on the raid (for the analysts).
    pub fn qrf_asset_lost(&mut self, unit: Unit, attacker: Option<Unit>) {
        if !self.qrf.on || !self.qrf.live {
            return;
        }
        let Some(def) = unit.def() else { return };
        let worker = def.speed() > 0.0 && !def.build_options().is_empty();
        // Terraform markers die on completion; they are not assets.
        if def.speed() > 0.0 && !worker || self.defs.name(def) == "terraunit" {
            return;
        }
        let pos = unit.pos();
        let cost = def.cost(self.metal);
        let frame = self.frame;
        // The attacker's raid group (the heavies too); the nearest group
        // only when the attacker is unknown (a static, say, or one not
        // seen).
        let all: Vec<&RaidGroup> = self.qrf.groups.iter().chain(self.qrf.heavies.iter()).collect();
        let by_attacker = attacker.and_then(|a| {
            let id = a.id();
            (all.iter().enumerate()).find(|(_, g)| g.members.contains(&id)).map(|(i, g)| (i, distance(g.center, pos)))
        });
        let raid = by_attacker.or_else(|| {
            (all.iter().enumerate())
                .map(|(i, g)| (i, distance(g.center, pos)))
                .filter(|&(_, d)| d < 700.0)
                .min_by(|a, b| a.1.total_cmp(&b.1))
        });
        // What the budget hears: the raiders in the group.
        let raid =
            raid.map(|(i, _)| (all[i].center, all[i].members.len(), all[i].power, self.qrf_raiders_power(all[i])));
        drop(all);
        let teams = self.qrf_teams();
        let nearest = (teams.iter())
            .map(|(&t, ids)| (t, distance(self.qrf_team_pos(ids).0, pos)))
            .min_by(|a, b| a.1.total_cmp(&b.1));
        let marked = raid.is_some_and(|(c, ..)| {
            (self.qrf.doing.values()).any(|d| d.raid > 0.0 && frame - d.frame < 60 && distance(d.at, c) < 1200.0)
        });
        let s = &mut self.qrf.live_stats;
        s.losses += 1;
        s.losses_marked += marked as u32;
        s.losses_metal += cost;
        if let Some((_, _, _, raiders)) = raid
            && raiders > 0.0
        {
            self.qrf.budget.lost_to(frame, raiders, cost);
        }
        let name = self.defs.name(def).to_owned();
        let raid_s =
            raid.map_or("none".to_owned(), |(_, n, power, raiders)| format!("{n}x{power:.0} raiders={raiders:.0}"));
        let near_s = nearest.map_or("none".to_owned(), |(t, d)| {
            format!("{}{}@{:.0}", if t >= MOON { "m" } else { "t" }, if t >= MOON { t - MOON } else { t }, d)
        });
        let line = format!(
            "qrfloss: t={} {name} at {:.0},{:.0} raid={raid_s} marked={marked} nearest={near_s}",
            frame / 30,
            pos.x,
            pos.z
        );
        self.log(&line);
        // A pilot hears of the economy raided unanswered (once a minute).
        if raid.is_some() && !marked && (name == "staticmex" || name.starts_with("energy")) {
            let b = &self.qrf.budget;
            let text = format!(
                "RAIDED: {name} lost at ({:.0}, {:.0}) to a raid of {raid_s} (units x metal) no QRF team was on; \
                 home defense {:.0} of {:.0} wanted ({}). qrf shows the teams; qrf_set defense raises the budget.",
                pos.x, pos.z, b.have, b.wanted, b.reason
            );
            self.op_alert("qrf-unanswered", 1800, text);
        }
        self.op_emit(
            "qrf-loss",
            json!({"unit": name, "pos": [pos.x.round(), pos.z.round()], "raid": raid_s, "marked": marked, "nearest": near_s}),
        );
    }

    /// Stop in normal play (the feature switched off): teams given back.
    pub(super) fn qrf_live_stop(&mut self) {
        let teams: Vec<usize> = self.qrf_teams().keys().copied().filter(|t| *t < MOON).collect();
        for t in teams {
            self.qrf_release_team(t, "qrf switched off");
        }
        self.qrf.members.clear();
        self.qrf.drafted.clear();
        self.qrf.doing.clear();
        self.qrf.on = false;
        self.qrf.grid = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizing_picks_the_raids_that_matter() {
        let mut b = Budget { size: Sizing::Typical, ..Budget::default() };
        // Nine single Daggers and one group of 600 seen; one mex lost to
        // the group.
        b.sightings.push_back((0, vec![80.0; 9]));
        b.sightings.push_back((30, vec![600.0]));
        b.lost_to(60, 600.0, 75.0);
        assert!((b.strongest() - 80.0).abs() < 1.0);
        b.size = Sizing::Largest;
        assert!((b.strongest() - 600.0).abs() < 1.0);
        b.size = Sizing::Lossy;
        assert!((b.strongest() - 600.0).abs() < 1.0);
        // Losses mostly to light raids: the weighted p75 is light, never
        // under typical.
        b.loss_raids.clear();
        for _ in 0..4 {
            b.lost_to(60, 80.0, 75.0);
        }
        b.lost_to(60, 600.0, 75.0);
        assert!((b.strongest() - 80.0).abs() < 1.0);
        // Old losses are forgotten.
        b.lost_to(60 + LOSS_RECALL + 1, 300.0, 75.0);
        assert_eq!(b.loss_raids.len(), 1);
        assert_eq!(weighted_p75(vec![(1.0, 1.0), (2.0, 1.0), (3.0, 1.0), (4.0, 1.0)]), 3.0);
        assert_eq!(weighted_p75(Vec::new()), 0.0);
    }

    #[test]
    fn recruit_cap_leaves_the_factory_half() {
        let mut b = Budget::default();
        // Of four units at the door, every second one may be taken (the
        // first is always the factory's); after the window the count is
        // fresh.
        let takes: Vec<bool> = (0..4).map(|i| b.may_recruit(i * 30, true)).collect();
        assert_eq!(takes, vec![false, true, false, true]);
        assert!(!b.may_recruit(120, false));
        assert!(!b.may_recruit(RECALL + 200, true));
        assert_eq!(b.door.len(), 1);
        assert!(b.may_recruit(RECALL + 230, true));
    }

    #[test]
    fn lossy_arms_on_losses_only() {
        let mut b = Budget::default();
        b.sightings.push_back((0, vec![80.0]));
        assert!(!b.armed());
        b.lost_to(10, 80.0, 75.0);
        assert!(b.armed());
        b.size = Sizing::Typical;
        b.loss_raids.clear();
        assert!(b.armed());
    }

    #[test]
    fn quiet_wants_nothing() {
        assert_eq!(auto_wanted(0, 0.0, 0.0, 0.0, 0.0, 5000.0, GAIN, CAP).0, 0.0);
    }

    #[test]
    fn raids_want_teams_up_to_the_cap() {
        // Two raids of 300 at once: two teams of 450.
        let (w, _) = auto_wanted(2, 300.0, 0.0, 0.0, 0.0, 10000.0, GAIN, CAP);
        assert!((w - 900.0).abs() < 1.0);
        // Moonlighters count half.
        let (w, _) = auto_wanted(2, 300.0, 0.0, 400.0, 0.0, 10000.0, GAIN, CAP);
        assert!((w - 700.0).abs() < 1.0);
        // The posture's turrets count in full.
        let (w, _) = auto_wanted(2, 300.0, 0.0, 0.0, 600.0, 10000.0, GAIN, CAP);
        assert!((w - 300.0).abs() < 1.0);
        assert_eq!(auto_wanted(2, 300.0, 0.0, 0.0, 2000.0, 10000.0, GAIN, CAP).0, 0.0);
        // Capped by the army.
        let (w, r) = auto_wanted(3, 1000.0, 2.0, 0.0, 0.0, 2000.0, GAIN, CAP);
        assert!((w - 600.0).abs() < 1.0 && r.starts_with("capped"));
        // Losses alone call for a team.
        assert!(auto_wanted(0, 0.0, 1.0, 0.0, 0.0, 10000.0, GAIN, CAP).0 > 0.0);
    }
}
