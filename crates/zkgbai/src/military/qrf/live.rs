//! The QRF in normal play.
//!
//! The force has two parts:
//! - *dedicated* teams: raiders taken as they leave the factory while the
//!   defense budget wants more (and given back to the raider handler when
//!   it wants less). Hurt members go home to be repaired like any unit
//!   (the retreat handler) and rejoin when healed.
//! - *moonlighting* squads: raider squads still forming (waiting at their
//!   rally point for the rest of their members) answer raids near home in
//!   the meantime. A squad sent at a raid (*drafted*) is the QRF's to move
//!   until the raid is dealt with; it keeps taking new members, and leaves
//!   on its own raid only once it is let go. The incursion planner uses
//!   squads past forming, so the two never contend for a squad.
//!
//! The budget (AI options `qrf_defense`, `qrf_cap`, `qrf_gain`; the
//! operator's `qrf_set`): on autopilot, nothing until raids come into our
//! territory or kill our economy; then enough power for as many teams as
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
use super::{Member, PERIOD};
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
const RAID_WEAK: f32 = 400.0;
const RAID_FLOOR: f32 = 800.0;
const RAID_SHARE: f32 = 0.5;

/// Raids are remembered this long for the budget (frames).
const RECALL: i32 = 30 * 180;

/// Defaults: dedicated power per unit of a typical raid's; the
/// ceiling, as a share of our mobile army.
const GAIN: f32 = 1.5;
const CAP: f32 = 0.3;

/// Teams are let go when the force is over budget by this share (plus a
/// raider) for this long.
const SLACK: f32 = 0.3;
const OVER_FOR: i32 = 30 * 30;

/// A drafted squad is let go this long after it was last sent (frames).
const DRAFT_HOLD: i32 = PERIOD * 2;

/// Units the dedicated teams take: raiders that run on the ground.
pub const RECRUITS: [&str; 8] =
    ["cloakraid", "shieldraid", "hoverraid", "vehraid", "tankraid", "tankheavyraid", "jumpraid", "amphraid"];

/// The budget's mode.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Mode {
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
    /// Raids near our value: frame, each raid's power.
    sightings: VecDeque<(i32, Vec<f32>)>,
    over_since: Option<i32>,
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
            sightings: VecDeque::new(),
            over_since: None,
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

impl Budget {
    /// Raids at once, and a raid's power, lately (75th percentiles over the
    /// times raids were near).
    pub fn recent(&self) -> (usize, f32) {
        let n = p75(self.sightings.iter().map(|s| s.1.len() as f32).collect()).round() as usize;
        let typical = p75(self.sightings.iter().flat_map(|s| s.1.iter().copied()).collect());
        (n, typical)
    }
}

/// The autopilot's want: `gain` times a typical recent raid, for as
/// many teams as raids came at once (up to three), more for losses, less
/// half of what moonlights, at most `cap` of the army.
pub fn auto_wanted(
    raids: usize,
    strongest: f32,
    losses: f32,
    moonlight: f32,
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
    let want = (need - 0.5 * moonlight).max(0.0);
    if want > ceiling {
        (ceiling, format!("capped: {teams:.0} x {per_team:.0} (losses {losses:.1}) over {cap:.2} of {army:.0}"))
    } else {
        (want, format!("{teams:.0} x {per_team:.0} (losses {losses:.1}) less half of {moonlight:.0} moonlighting"))
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
    pub recruited: u32,
    pub released: u32,
    pub drafts: u32,
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
            None | Some("auto") => Mode::Auto,
            Some(v) => match v.strip_suffix('%') {
                Some(p) => Mode::Share(p.parse::<f32>().unwrap_or(30.0).clamp(0.0, 100.0) / 100.0),
                None => Mode::Metal(v.parse::<f32>().unwrap_or(0.0).max(0.0)),
            },
        };
        q.budget.cap = num("qrf_cap").unwrap_or(CAP).clamp(0.0, 1.0);
        q.budget.gain = num("qrf_gain").unwrap_or(GAIN).max(0.0);
        q.grid = Some(Grid::new(self.cb));
        q.candidates = super::candidates(self.cb);
        let line = format!(
            "qrf: live algo={} team={} moonlight={} defense={:?} cap={} gain={}",
            q.algo, q.team_size, !q.no_moonlight, q.budget.mode, q.budget.cap, q.budget.gain
        );
        self.log(&line);
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
            for &sid in &self.raider_handler.raider_squads {
                let Some(sq) = self.squads.get(sid) else { continue };
                let drafted = self.qrf.drafted.contains_key(&sid);
                if sq.kind != SquadKind::Raider
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
        self.qrf.drafted.retain(|sid, at| eligible.contains_key(sid) && frame - *at <= DRAFT_HOLD);
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
        self.qrf.groups.retain(|g| g.power <= army);
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
        let b = &mut self.qrf.budget;
        b.army = army;
        b.have = have;
        b.moonlight = moon;
        let (raids, strongest) = b.recent();
        let (wanted, reason) = match b.mode {
            Mode::Auto => auto_wanted(raids, strongest, losses, moon, army, b.gain, b.cap),
            Mode::Metal(v) => (v, "set by the operator".into()),
            Mode::Share(s) => (s * army, format!("{:.0}% of the army, set by the operator", s * 100.0)),
        };
        b.wanted = wanted;
        b.reason = reason;
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
        if !RECRUITS.contains(&self.defs.name(def)) {
            return false;
        }
        let cost = def.cost(self.metal);
        if q.budget.have + 0.5 * cost > q.budget.wanted {
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
            if now && self.qrf.drafted.insert(sid, frame).is_none() {
                self.qrf.live_stats.drafts += 1;
            }
        }
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
             drafted={} raids={} recruited={} released={} drafts={} losses={} marked={} lost_metal={:.0} reason={}",
            frame / 30,
            b.wanted,
            b.have,
            b.army,
            b.moonlight,
            self.qrf.drafted.len(),
            self.qrf.groups.len(),
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
            "recruited": s.recruited, "released": s.released, "drafts": s.drafts,
            "losses": s.losses, "losses_marked": s.losses_marked, "losses_metal": s.losses_metal.round(),
        });
        self.op_emit("qrf", v);
    }

    /// One of our assets died: who was on the raid (for the analysts).
    pub fn qrf_asset_lost(&mut self, unit: Unit) {
        if !self.qrf.on || !self.qrf.live {
            return;
        }
        let Some(def) = unit.def() else { return };
        let worker = def.speed() > 0.0 && !def.build_options().is_empty();
        if def.speed() > 0.0 && !worker {
            return;
        }
        let pos = unit.pos();
        let cost = def.cost(self.metal);
        let frame = self.frame;
        let raid = (self.qrf.groups.iter().enumerate())
            .map(|(i, g)| (i, distance(g.center, pos)))
            .filter(|&(_, d)| d < 700.0)
            .min_by(|a, b| a.1.total_cmp(&b.1));
        let teams = self.qrf_teams();
        let nearest = (teams.iter())
            .map(|(&t, ids)| (t, distance(self.qrf_team_pos(ids).0, pos)))
            .min_by(|a, b| a.1.total_cmp(&b.1));
        let marked = raid.is_some_and(|(i, _)| {
            let c = self.qrf.groups[i].center;
            (self.qrf.doing.values()).any(|d| d.raid > 0.0 && frame - d.frame < 60 && distance(d.at, c) < 1200.0)
        });
        let s = &mut self.qrf.live_stats;
        s.losses += 1;
        s.losses_marked += marked as u32;
        s.losses_metal += cost;
        let name = self.defs.name(def).to_owned();
        let raid_s = raid.map_or("none".to_owned(), |(i, _)| {
            let g = &self.qrf.groups[i];
            format!("{}x{:.0}", g.members.len(), g.power)
        });
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
    fn quiet_wants_nothing() {
        assert_eq!(auto_wanted(0, 0.0, 0.0, 0.0, 5000.0, GAIN, CAP).0, 0.0);
    }

    #[test]
    fn raids_want_teams_up_to_the_cap() {
        // Two raids of 300 at once: two teams of 450.
        let (w, _) = auto_wanted(2, 300.0, 0.0, 0.0, 10000.0, GAIN, CAP);
        assert!((w - 900.0).abs() < 1.0);
        // Moonlighters count half.
        let (w, _) = auto_wanted(2, 300.0, 0.0, 400.0, 10000.0, GAIN, CAP);
        assert!((w - 700.0).abs() < 1.0);
        // Capped by the army.
        let (w, r) = auto_wanted(3, 1000.0, 2.0, 0.0, 2000.0, GAIN, CAP);
        assert!((w - 600.0).abs() < 1.0 && r.starts_with("capped"));
        // Losses alone call for a team.
        assert!(auto_wanted(0, 0.0, 1.0, 0.0, 10000.0, GAIN, CAP).0 > 0.0);
    }
}
