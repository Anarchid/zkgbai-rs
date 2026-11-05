//! `coalesce` (off by default, `enable=coalesce`): raiders standing near
//! one another, whoever drives them, take the local fight together when
//! the fight predictor says it pays (user, 2026-10-31: a stakeout, a solo
//! raider and two survivors of a raider squad seeing a Lotus could become
//! one strike-squad-like formation for that fight).
//!
//! Philosoraider moves raiders by the threat map: each one alone, or one
//! squad, weighs painted enemy threat against our painted raider threat.
//! Raiders that happen to stand together but belong to different
//! controllers never add up, and the painted discs leave value on the
//! table or make raiders oscillate at their rims.
//!
//! Twice a second the raiders free to be borrowed (solo raiders, Kodachis,
//! raider-squad members, armed stakeout pickets; not retreating, not on a
//! strike, a precision mission, an operator order or a QRF draft, not in
//! the air or under water, no cloaked Scythes) are clustered by [`LINK`]
//! (single linkage; a raider squad's members always share a cluster). A
//! cluster of [`MIN_MEMBERS`] or more with an armed enemy in its local fight
//! (`fightsim::local`), or a lone raider beside an enemy whose tubes are
//! all empty (`ammo`, [`LONE_OPEN`]), is judged two ways:
//!
//! - **clean**: all of it a-moving in. Every armed enemy likely dead (each
//!   P(dead) >= [`CLEAN_MIN_P`], [`CLEAN_SHARE`] of their armed metal), at
//!   most [`CLEAN_LOSS`] of our metal lost, and the kills worth
//!   [`CLEAN_RETURN`] times our loss;
//! - **prize**: all of it diving one prize (commander, factory, heavy, big
//!   economy: `local::prize`), dead with P >= [`PRIZE_P`], everything killed
//!   worth [`PRIZE_RETURN`] times our loss: a heavy for half its cost in
//!   raiders.
//!
//! The better plan that passes forms a gang: its members are borrowed (the
//! raider handler and stakeouts skip them, see [`Ai::coalesce_owns`]) and
//! do not retreat to repair (committed). The gang is judged again twice a
//! second by the same plan with looser thresholds, so a turn in the fight
//! breaks it and a wobble does not; it breaks up when its prize dies, no
//! armed enemy is left, the forecast turns, or after [`MAX_LIFE`]. Members
//! go back to whoever drove them, and a new gang may form at once (the
//! Lotus is dead: the mexes behind it are the next fight).
//!
//! `coalsize` (default on): a gang is sized to its fight, as the QRF's
//! mark sizes a team. The cluster's blocks (a raider squad is one block:
//! its handler skips the whole squad while any member is borrowed) are
//! taken nearest the focus first until they are worth [`JUST_ENOUGH`]
//! times the fight's armed enemy metal, and that prefix is judged; if it
//! fails (the clean plan's loss is a share of the gang's metal, so a
//! small gang fails where a big one passes) the prefix is doubled, up to
//! [`SIZE_STEPS`] times, and only then the whole cluster (the lobby, 11-04:
//! six raiders chasing one Dagger, seven one Ravager, whole clusters on a
//! lone Dart). A raider squad is split too: the members taken are lent to
//! this module (see `springai::reserved::lend`), so the squad's own orders no
//! longer reach them, and its handler drives the rest on (it skips a squad
//! only when its leader, or all of it, is borrowed; the leader is never
//! taken into a prefix, so the squad keeps its place). A clean gang
//! that is not closing on its focus (out of its guns' reach and the gap
//! not shrinking by [`CHASE_CLOSE`] a recheck, [`CHASE_TICKS`] rechecks
//! running: the focus kites or flees at our speed) breaks off (`chase`),
//! and that enemy is left alone for [`SHY`] frames, so the same Dart does
//! not form a new gang every few seconds.
//!
//! `coalesceshadow` (with `coalesce` off) judges the same clusters and
//! journals what would have formed, with the same windows, for the off arm
//! of an A/B.
//!
//! Journal `k=coalesce`: `form` / `would` (members and where they came
//! from, plan, prize, the fates' sums), `end` (reason, life), `window`
//! [`WINDOW`] after a form or would (members' metal lost, enemies' value
//! seen dying, whether the prize died), `cost` per minute.

use std::collections::BTreeMap;

use serde_json::{Value, json};
use springai::{Unit, Vec3, reserved};

use crate::Ai;
use crate::military::SquadId;
use crate::military::fightsim::local::{Budget, CONTACT, Local, Tally, Theirs, kills, tally, their_index};
use crate::military::fightsim::{FateMode, UnitFate};
use crate::military::wrappers::fighter::fight_clear;
use crate::nav;
use crate::util::distance;

pub const FEATURE: &str = "coalesce";
pub const SHADOW: &str = "coalesceshadow";
pub const SIZE: &str = "coalsize";

/// Frames between scans (2 Hz).
const PERIOD: i32 = 15;
/// Raiders this close are one cluster (single linkage).
const LINK: f32 = 450.0;
/// Legacy minimum. `raidcommit` also judges singletons.
const MIN_MEMBERS: usize = 2;
/// ...unless an enemy in its fight has fired every tube, for at least this
/// many more frames (`ammo`): a Kodachi that has taken a Picket's three
/// missiles, a Glaive whose pair-mate did.
const LONE_OPEN: i32 = 60;
/// Synchronous query time per scan (microseconds); clusters left over wait.
const BUDGET_US: u128 = 4000;
/// Prizes tried per cluster, most valuable first.
const PRIZES: usize = 2;

/// Clean plan: forming, then holding (the sim's own P(dead), see
/// `fightsim::local`).
const CLEAN_MIN_P: f32 = 0.8;
const CLEAN_SHARE: f32 = 0.8;
const CLEAN_LOSS: f32 = 0.25;
const CLEAN_RETURN: f32 = 1.5;
const CLEAN_KEEP_SHARE: f32 = 0.5;
const CLEAN_KEEP_LOSS: f32 = 0.5;
/// The kills must be worth at least this much metal.
const MIN_PRIZE_METAL: f32 = 50.0;

/// Prize plan: forming, then holding.
const PRIZE_P: f32 = 0.8;
const PRIZE_RETURN: f32 = 2.0;
const PRIZE_KEEP_P: f32 = 0.4;
const PRIZE_KEEP_RETURN: f32 = 1.0;

/// Failed judgements in a row that break a gang.
const FAILS: u8 = 3;
/// A gang breaks up after this long (frames).
const MAX_LIFE: i32 = 30 * 45;
/// Orders are repeated at most this often, unless the focus changes.
const REORDER: i32 = 60;
/// A sized gang is worth this many times the fight's armed enemy metal.
const JUST_ENOUGH: f32 = 2.0;
/// A failing prefix is doubled this many times before the whole cluster.
const SIZE_STEPS: u32 = 2;
/// A clean gang out of reach of its focus is chasing when the gap does
/// not shrink by this much (elmos) per recheck, this many rechecks
/// running (a closing gang shrinks it by 50 or more at raider speed).
const CHASE_CLOSE: f32 = 20.0;
const CHASE_TICKS: u8 = 4;
/// An enemy that outran a gang is left alone this long (frames).
const SHY: i32 = 30 * 15;
/// Frames after a form (or would) over which the outcome is read.
const WINDOW: i32 = 30 * 30;
/// A unit in a `would` record is not recorded again for this long, so the
/// off arm logs one situation once, as the on arm forms once.
const QUIET: i32 = 30 * 15;
/// Frames between cost lines.
const REPORT_PERIOD: i32 = 1800;

/// Where a member came from (and goes back to).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Origin {
    Solo,
    Kodachi,
    Squad(SquadId),
    /// A squad raider between squads.
    Loose,
    Stakeout,
}

impl Origin {
    pub(crate) fn name(self) -> &'static str {
        match self {
            Origin::Solo => "solo",
            Origin::Kodachi => "kodachi",
            Origin::Squad(_) => "squad",
            Origin::Loose => "loose",
            Origin::Stakeout => "stakeout",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Plan {
    Clean,
    Prize(i32),
}

impl Plan {
    fn name(self) -> &'static str {
        match self {
            Plan::Clean => "clean",
            Plan::Prize(_) => "prize",
        }
    }
}

struct Gang {
    id: u32,
    members: Vec<(i32, Origin, Unit)>,
    plan: Plan,
    since: i32,
    last_order: i32,
    focus: Option<i32>,
    /// Failed judgements in a row, and the last judgement's sums.
    fails: u8,
    last: Option<(Tally, f32)>,
    /// The focus and our gap to it at the last recheck, and how many
    /// rechecks running the gap has grown (`coalsize`).
    gap: Option<(i32, f32)>,
    chasing: u8,
}

/// A gang's judgement.
enum Judged {
    Pass(Verdict),
    /// No plan passes: the last plan tried's sums and P(prize).
    Fail(Tally, f32),
    /// No armed enemy in the fight (or the prize left it): done.
    NoFight,
    /// An unidentified contact is in the fight.
    Unknown,
}

/// A form (or would) whose outcome window is open.
struct Window {
    gid: u32,
    frame: i32,
    shadow: bool,
    plan: Plan,
    members: Vec<(Unit, f32)>,
    /// Their units: id, worth, seen dying since.
    enemies: Vec<(i32, f32, bool)>,
    predicted: Tally,
}

/// A judged cluster's verdict.
struct Verdict {
    plan: Plan,
    tally: Tally,
    /// P(dead) of the prize.
    p_prize: f32,
    /// Where to send a clean gang: their armed unit nearest our middle.
    focus: Option<i32>,
    record: Value,
}

#[derive(Default)]
pub struct Coalesce {
    gangs: Vec<Gang>,
    /// Unit id -> gang id.
    owner: BTreeMap<i32, u32>,
    leases: BTreeMap<i32, reserved::CommandLease>,
    next_id: u32,
    windows: Vec<Window>,
    /// Shadow records: unit id -> frame until which it is not recorded.
    quiet: BTreeMap<i32, i32>,
    /// Rejected encounters assemble here until the next scan; old handlers
    /// may not turn a rejection back into an attack.
    holds: BTreeMap<i32, (i32, Vec3)>,
    /// Bound assembly; repeated rejected scans must not pin a scout forever.
    assembly_retry: BTreeMap<i32, i32>,
    /// Round-robin start among clusters when the budget runs out.
    rotate: usize,
    micros: u64,
    queries: u64,
    scans: u64,
    over_budget: u64,
    forms: u64,
    /// Lone raiders judged beside an emptied tube weapon (`ammo`), and
    /// the gangs of one formed (or would be).
    lone_judged: u64,
    lone_formed: u64,
    /// Gangs formed as a sized prefix of their cluster, and clean gangs
    /// broken off a chase (`coalsize`).
    sized: u64,
    sized_failed: u64,
    chases: u64,
    /// Enemies that outran a gang: unit id -> frame until which no gang
    /// forms on them.
    shy: BTreeMap<i32, i32>,
    last_report: i32,
}

/// Blocks (a raider squad is one block) `(distance to the focus, metal,
/// member indices)`, nearest first, until they are worth `need`: the
/// indices taken. At least `min_units` units when the blocks have them.
fn take_blocks(blocks: &mut [(f32, f32, Vec<usize>)], need: f32, min_units: usize) -> Vec<usize> {
    blocks.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut taken = Vec::new();
    let mut have = 0.0;
    for (_, cost, idx) in blocks.iter() {
        if have >= need && taken.len() >= min_units {
            break;
        }
        taken.extend(idx.iter().copied());
        have += cost;
    }
    taken
}

fn passes(plan: Plan, t: &Tally, p_prize: f32, holding: bool, minimum_value: f32) -> bool {
    if t.kill_value < minimum_value {
        return false;
    }
    match plan {
        Plan::Clean if holding => {
            t.armed_share >= CLEAN_KEEP_SHARE && t.own_loss <= CLEAN_KEEP_LOSS * t.own_cost && t.net() >= 0.0
        }
        Plan::Clean => {
            t.min_armed_p >= CLEAN_MIN_P
                && t.armed_share >= CLEAN_SHARE
                && t.own_loss <= CLEAN_LOSS * t.own_cost
                && t.kill_value >= CLEAN_RETURN * t.own_loss
        }
        Plan::Prize(_) if holding => p_prize >= PRIZE_KEEP_P && t.kill_value >= PRIZE_KEEP_RETURN * t.own_loss,
        Plan::Prize(_) => p_prize >= PRIZE_P && t.kill_value >= PRIZE_RETURN * t.own_loss,
    }
}

/// Single-linkage clusters of `points` (indices), where `same` also links.
fn clusters(points: &[Vec3], same: impl Fn(usize, usize) -> bool) -> Vec<Vec<usize>> {
    let n = points.len();
    let mut parent: Vec<usize> = (0..n).collect();
    fn find(p: &mut [usize], mut i: usize) -> usize {
        while p[i] != i {
            p[i] = p[p[i]];
            i = p[i];
        }
        i
    }
    for i in 0..n {
        for j in i + 1..n {
            if distance(points[i], points[j]) <= LINK || same(i, j) {
                let (a, b) = (find(&mut parent, i), find(&mut parent, j));
                if a != b {
                    parent[a] = b;
                }
            }
        }
    }
    let mut groups: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    for i in 0..n {
        let r = find(&mut parent, i);
        groups.entry(r).or_default().push(i);
    }
    groups.into_values().collect()
}

fn tally_json(t: &Tally) -> Value {
    json!({
        "own_cost": t.own_cost, "own_loss": t.own_loss, "kill_value": t.kill_value,
        "armed_cost": t.armed_cost, "armed_share": t.armed_share, "min_armed_p": t.min_armed_p,
    })
}

impl Ai {
    pub fn coalesce_on(&self) -> bool {
        !self.dev_off(FEATURE) || self.raidcommit_on()
    }

    fn coalesce_runs(&self) -> bool {
        self.coalesce_on() || !self.dev_off(SHADOW)
    }

    /// Whether unit `id` is borrowed by a gang (its controller skips it).
    pub fn coalesce_owns(&self, id: i32) -> bool {
        self.coalesce.owner.contains_key(&id)
    }

    /// Whether raider squad `sid` is a gang's: its leader borrowed, or all
    /// of it (with `coalsize` a squad lends some members and goes on).
    pub fn coalesce_owns_squad(&self, sid: SquadId) -> bool {
        !self.coalesce.owner.is_empty()
            && self.squads.get(sid).is_some_and(|s| {
                let owned = |id: &i32| self.coalesce.owner.contains_key(id);
                if self.dev_off(SIZE) {
                    return s.fighters.iter().chain(s.leader.iter()).any(owned);
                }
                s.leader.as_ref().is_some_and(owned) || !s.fighters.is_empty() && s.fighters.iter().all(owned)
            })
    }

    /// An enemy died: open windows count it.
    pub fn coalesce_enemy_destroyed(&mut self, id: i32) {
        for w in &mut self.coalesce.windows {
            for e in w.enemies.iter_mut().filter(|e| e.0 == id) {
                e.2 = true;
            }
        }
    }

    pub fn coalesce_update(&mut self, frame: i32) {
        if frame % PERIOD != (self.offset + 9) % PERIOD {
            return;
        }
        self.coalesce.holds.retain(|_, (until, _)| *until > frame);
        self.coalesce.assembly_retry.retain(|_, until| *until > frame);
        if !self.raidcommit_on() {
            self.coalesce.holds.clear();
            self.coalesce.assembly_retry.clear();
        }
        self.coalesce_close_windows(frame);
        if frame - self.coalesce.last_report >= REPORT_PERIOD {
            self.coalesce_report(frame);
        }
        if !self.coalesce_on() && !self.coalesce.gangs.is_empty() {
            // Switched off in play: everyone goes home.
            let ids: Vec<u32> = self.coalesce.gangs.iter().map(|g| g.id).collect();
            for gid in ids {
                self.coalesce_end(gid, "off");
            }
        }
        if !self.coalesce_runs() {
            return;
        }
        let mut budget = Budget::new(BUDGET_US);
        self.coalesce_recheck(&mut budget);
        self.coalesce_scan(&mut budget);
        let c = &mut self.coalesce;
        c.scans += 1;
        c.micros += budget.micros();
        c.queries += budget.queries as u64;
    }

    /// Changing policy cancels the old automatic orders before handing units
    /// back. Reservations and stronger controllers retain their authority.
    pub(crate) fn raidcommit_switch_changed(&mut self) {
        let units: Vec<_> = self.coalesce.gangs.iter().flat_map(|g| g.members.iter().map(|m| m.2)).collect();
        let gangs: Vec<_> = self.coalesce.gangs.iter().map(|g| g.id).collect();
        for gid in gangs {
            self.coalesce_end(gid, "policy_changed");
        }
        self.coalesce.holds.clear();
        self.coalesce.assembly_retry.clear();
        for u in units {
            if u.health() > 0.0
                && u.team() == self.team_id
                && !springai::reserved::is_reserved(self.cb, u.id())
                && !self.recovery_owns(u.id())
                && !self.retreat_is_retreating(u)
                && !self.qrf.members.contains_key(&u.id())
                && !self.squad_strike_owns(u.id())
            {
                u.stop(0);
            }
        }
        self.qrf_reset_commit_orders();
    }

    /// A denial remains binding while the regular controller chooses its next job.
    pub(crate) fn raidcommit_wait(&mut self, units: &[Unit]) -> bool {
        if !self.raidcommit_on() {
            return false;
        }
        let holds: Vec<_> = units
            .iter()
            .filter_map(|u| {
                self.coalesce.holds.get(&u.id()).filter(|(until, _)| *until > self.frame).map(|(_, p)| (*u, *p))
            })
            .collect();
        if holds.is_empty() {
            return false;
        }
        // A squad may have lost a borrowed member; don't resume its attack
        // around that member while the rejected encounter is regrouping.
        let meeting = holds[0].1;
        for &u in units {
            self.raidcommit_move(u, meeting);
        }
        true
    }

    fn raidcommit_hold(&mut self, units: &[Unit], meeting: Vec3) {
        let fresh: Vec<_> = units
            .iter()
            .copied()
            .filter(|u| self.coalesce.assembly_retry.get(&u.id()).is_none_or(|until| *until <= self.frame))
            .collect();
        if !fresh.is_empty() {
            self.op_emit(
                "raidcommit",
                json!({"event":"assemble",
                "units":fresh.iter().map(|u|u.id()).collect::<Vec<_>>(), "at":[meeting.x, meeting.z]}),
            );
        }
        for unit in fresh {
            self.coalesce.holds.insert(unit.id(), (self.frame + 90, meeting));
            self.coalesce.assembly_retry.insert(unit.id(), self.frame + 300);
            self.raidcommit_move(unit, meeting);
        }
    }

    /// QRF retains ownership but uses exactly the clean-gang admission policy.
    /// No forecast, unknown contacts, or an unprofitable fight grant no attack.
    pub(crate) fn raidcommit_defense(&mut self, units: &[Unit]) -> Option<Vec3> {
        let mut budget = Budget::new(BUDGET_US);
        let verdict = self.coalesce_judge(units, None, true, &mut budget);
        match verdict {
            Judged::Pass(v) => {
                self.op_emit(
                    "raidcommit",
                    json!({"event":"defense_commit",
                    "units":units.iter().map(|u|u.id()).collect::<Vec<_>>(), "tally":tally_json(&v.tally)}),
                );
                v.focus.and_then(|id| self.war.targets.get(&id).map(|e| e.position))
            }
            _ => None,
        }
    }

    /// The raiders free to join a gang, and where each comes from.
    pub(crate) fn coalesce_candidates(&self) -> Vec<(i32, Origin, Unit)> {
        let frame = self.frame;
        let h = &self.raider_handler;
        let mut out: Vec<(i32, Origin, Unit)> = Vec::new();
        let fighter = |id: &i32| self.fighters.get(id).map(|f| f.unit);
        for id in &h.solo_raiders {
            if !self.precision.missions.contains_key(id)
                && let Some(u) = fighter(id)
            {
                out.push((*id, Origin::Solo, u));
            }
        }
        for id in &h.kodachis {
            if !self.precision.missions.contains_key(id)
                && let Some(u) = fighter(id)
            {
                out.push((*id, Origin::Kodachi, u));
            }
        }
        for id in h.small_raiders.iter().chain(h.medium_raiders.iter()) {
            let Some(f) = self.fighters.get(id) else { continue };
            let origin = match f.squad.filter(|s| self.squads.contains(*s)) {
                Some(sid) => {
                    if self.qrf_drafted(sid) || self.op_squad_target(sid).is_some() || self.squad_strike_active(sid) {
                        continue;
                    }
                    Origin::Squad(sid)
                }
                None => Origin::Loose,
            };
            out.push((*id, origin, f.unit));
        }
        for (id, u) in self.stakeouts_borrowable(frame) {
            out.push((id, Origin::Stakeout, u));
        }
        out.retain(|&(id, _, u)| {
            let Some(def) = u.def() else { return false };
            if self.raidcommit_on() && self.qrf.members.contains_key(&id) {
                return false;
            }
            u.health() > 0.0
                && u.team() == self.team_id
                && !springai::reserved::is_reserved(self.cb, id)
                && !self.recovery_owns(id)
                && !u.is_being_built()
                && !def.is_able_to_fly()
                && def.max_weapon_range() > 0.0
                && u.pos().y >= -5.0
                && !(u.is_cloaked() && self.defs.name(def) == "cloakheavyraid")
                && !self.retreat_is_retreating(u)
                && !self.squad_strike_owns(id)
                && !self.fire_controls(id)
                && !self.limpet_controls(id)
                && !self.blasts_waiting(u.pos())
                && !self.stayveto_borrowed(id)
        });
        out
    }

    /// Judge `units` (with their origins) as one gang: the best plan that
    /// passes, holding `hold` (the plan to keep) with the looser thresholds.
    fn coalesce_judge(&mut self, units: &[Unit], hold: Option<Plan>, clean_only: bool, budget: &mut Budget) -> Judged {
        if self.raidcommit_on() && budget.spent() {
            return Judged::Unknown;
        }
        let Some(mut local) = self.local_fight(units) else {
            return if self.raidcommit_on() { Judged::Unknown } else { Judged::NoFight };
        };
        if self.raidcommit_on() && (local.unknown || local.ours.len() != units.len()) {
            return Judged::Unknown;
        }
        local.snap.flags.ordered = !self.dev_off(crate::military::fightsim::ORDERED);
        if !local.armed_theirs() {
            return Judged::NoFight;
        }
        if local.unknown {
            return Judged::Unknown;
        }
        // An enemy that outran a gang is left alone a while (`coalsize`).
        let sized = !self.dev_off(SIZE);
        let shy = |id: i32| sized && self.coalesce.shy.contains_key(&id);
        if local.theirs.iter().filter(|t| t.armed).all(|t| shy(t.id)) {
            return Judged::NoFight;
        }
        let mid = {
            let n = units.len().max(1) as f32;
            Vec3::xz(units.iter().map(|u| u.pos().x).sum::<f32>() / n, units.iter().map(|u| u.pos().z).sum::<f32>() / n)
        };
        let focus = (local.theirs.iter())
            .filter(|t| t.armed && !shy(t.id))
            .min_by(|a, b| distance(a.pos, mid).total_cmp(&distance(b.pos, mid)))
            .map(|t| t.id);
        let mut plans: Vec<Plan> = match hold {
            Some(p) => vec![p],
            None if clean_only => vec![Plan::Clean],
            None => {
                let mut prizes: Vec<_> = local.theirs.iter().filter(|t| t.prize).collect();
                prizes.sort_by(|a, b| b.value.total_cmp(&a.value));
                std::iter::once(Plan::Clean).chain(prizes.iter().take(PRIZES).map(|t| Plan::Prize(t.id))).collect()
            }
        };
        // A held prize that left the fight (or died) ends the gang.
        if let Some(Plan::Prize(id)) = hold
            && !local.theirs.iter().any(|t| t.id == id)
        {
            return Judged::NoFight;
        }
        let mut best: Option<Verdict> = None;
        let mut last = None;
        for plan in plans.drain(..) {
            if budget.spent() && hold.is_none() && best.is_some() {
                break;
            }
            let (fates, prize_index) = match plan {
                // Clean gangs receive Fight orders with UnitAI still active.
                Plan::Clean => (self.local_kiting_fates(&local, budget), None),
                Plan::Prize(id) => {
                    let Some(i) = local.theirs.iter().position(|t| t.id == id) else { continue };
                    let mode = FateMode::Assassinate { target: their_index(&local, i) };
                    (self.local_fates(&local, mode, budget), Some(i))
                }
            };
            let Some(fates) = fates else {
                return if self.raidcommit_on() { Judged::Unknown } else { Judged::NoFight };
            };
            let t = tally(&local, &fates, true);
            let p_prize = prize_index.map_or(0.0, |i| fates[their_index(&local, i)].p_raw);
            last = Some((t, p_prize));
            if !passes(plan, &t, p_prize, hold.is_some(), if self.raidcommit_on() { 0.0 } else { MIN_PRIZE_METAL })
                || best.as_ref().is_some_and(|b| b.tally.net() >= t.net())
            {
                continue;
            }
            let record = coalesce_record(&local, &fates, plan, &t, p_prize);
            best = Some(Verdict { plan, tally: t, p_prize, focus, record });
        }
        match (best, last) {
            (Some(v), _) => Judged::Pass(v),
            (None, Some((t, p))) => Judged::Fail(t, p),
            (None, None) => Judged::NoFight,
        }
    }

    /// Judge each gang again; break those whose fight turned or ended.
    fn coalesce_recheck(&mut self, budget: &mut Budget) {
        let frame = self.frame;
        let ids: Vec<u32> = self.coalesce.gangs.iter().map(|g| g.id).collect();
        for gid in ids {
            // Members dead, given away, or claimed by a stronger controller.
            let Some(g) = self.coalesce.gangs.iter().find(|g| g.id == gid) else { continue };
            let (plan, since) = (g.plan, g.since);
            let members: Vec<(i32, Origin, Unit)> = g.members.clone();
            let kept: Vec<(i32, Origin, Unit)> = members
                .iter()
                .copied()
                .filter(|&(id, origin, u)| {
                    u.health() > 0.0
                        && u.team() == self.team_id
                        && !reserved::is_reserved(self.cb, id)
                        && self.coalesce.leases.get(&id).is_some_and(|l| l.active())
                        && !self.recovery_owns(id)
                        && !self.retreat_handler.is_retreating(id)
                        && !self.squad_strike_owns(id)
                        && !self.fire_controls(id)
                        && match origin {
                            Origin::Squad(sid) => {
                                self.squads.contains(sid)
                                    && !self.qrf_drafted(sid)
                                    && self.op_squad_target(sid).is_none()
                                    && !self.squad_strike_active(sid)
                            }
                            Origin::Stakeout => self.stakeouts.contains(id),
                            _ => self.fighters.contains_key(&id),
                        }
                })
                .collect();
            for &(id, ..) in members.iter().filter(|m| !kept.iter().any(|k| k.0 == m.0)) {
                self.coalesce.owner.remove(&id);
                if let Some(lease) = self.coalesce.leases.remove(&id) {
                    lease.release();
                }
            }
            if let Some(g) = self.coalesce.gangs.iter_mut().find(|g| g.id == gid) {
                g.members = kept.clone();
            }
            if kept.is_empty() {
                self.coalesce_end(gid, "wiped");
                continue;
            }
            if frame - since >= MAX_LIFE {
                self.coalesce_end(gid, "timeout");
                continue;
            }
            if let Plan::Prize(pid) = plan
                && self.war.targets.get(&pid).is_none_or(|e| e.is_dead)
            {
                self.coalesce_end(gid, "prize_dead");
                continue;
            }
            let units: Vec<Unit> = kept.iter().map(|m| m.2).collect();
            // A failed judgement ends the gang only when repeated: the sim's
            // answer jumps with small moves (who is in range of whom).
            let fail = |ai: &mut Ai, reason: &str, last: Option<(Tally, f32)>| {
                let fails = ai.coalesce.gangs.iter_mut().find(|g| g.id == gid).map_or(FAILS, |g| {
                    g.fails += 1;
                    g.last = last.or(g.last);
                    g.fails
                });
                if ai.raidcommit_on() || fails >= FAILS {
                    ai.coalesce_end(gid, reason);
                }
            };
            match self.coalesce_judge(&units, Some(plan), false, budget) {
                Judged::Pass(v) => {
                    if let Some(g) = self.coalesce.gangs.iter_mut().find(|g| g.id == gid) {
                        g.fails = 0;
                        g.last = Some((v.tally, v.p_prize));
                    }
                    self.coalesce_order(gid, v.focus);
                    if plan == Plan::Clean
                        && !self.dev_off(SIZE)
                        && let Some(fid) = v.focus
                    {
                        self.coalesce_chase(gid, fid, &units);
                    }
                }
                Judged::NoFight => self.coalesce_end(gid, "clear"),
                Judged::Unknown => fail(self, "unknown", None),
                Judged::Fail(t, p) => fail(self, "forecast", Some((t, p))),
            }
        }
    }

    /// Cluster the free raiders and form gangs where a plan passes.
    fn coalesce_scan(&mut self, budget: &mut Budget) {
        let frame = self.frame;
        let shadow = !self.coalesce_on();
        self.coalesce.quiet.retain(|_, until| *until > frame);
        self.coalesce.shy.retain(|_, until| *until > frame);
        let free: Vec<(i32, Origin, Unit)> =
            self.coalesce_candidates().into_iter().filter(|c| !self.coalesce.owner.contains_key(&c.0)).collect();
        if free.is_empty() {
            return;
        }
        let points: Vec<Vec3> = free.iter().map(|c| c.2.pos()).collect();
        let groups: Vec<Vec<usize>> = clusters(&points, |i, j| match (free[i].1, free[j].1) {
            (Origin::Squad(a), Origin::Squad(b)) => a == b,
            _ => false,
        })
        .into_iter()
        .filter(|g| {
            self.raidcommit_on() || g.len() >= MIN_MEMBERS || self.ammo_open_near(points[g[0]], CONTACT, LONE_OPEN)
        })
        .filter(|g| !shadow || g.iter().all(|&i| !self.coalesce.quiet.contains_key(&free[i].0)))
        .collect();
        if groups.is_empty() {
            return;
        }
        let n = groups.len();
        let start = self.coalesce.rotate % n;
        for k in 0..n {
            if budget.spent() {
                self.coalesce.over_budget += 1;
                self.coalesce.rotate = start + k;
                return;
            }
            let group = &groups[(start + k) % n];
            let members: Vec<(i32, Origin, Unit)> = group.iter().map(|&i| free[i]).collect();
            let cluster = members.len();
            // Just enough of the cluster first, then twice that, then
            // four times (`coalsize`).
            let mut sized = false;
            if !self.dev_off(SIZE) {
                let mut last = 0;
                for step in 0..=SIZE_STEPS {
                    let ratio = JUST_ENOUGH * (1 << step) as f32;
                    let Some(prefix) = self.coalesce_prefix(&members, ratio) else { break };
                    if prefix.len() == last {
                        continue;
                    }
                    last = prefix.len();
                    let units: Vec<Unit> = prefix.iter().map(|m| m.2).collect();
                    let judged = self.coalesce_judge(&units, None, false, budget);
                    if let Judged::Pass(v) = judged {
                        self.coalesce.sized += 1;
                        self.coalesce_form(prefix, v, shadow, cluster);
                        sized = true;
                        break;
                    }
                    self.coalesce.sized_failed += 1;
                    // Why the prefix failed (for the read).
                    let why = match &judged {
                        Judged::Fail(t, p) => json!({"tally": tally_json(t), "p_prize": p}),
                        Judged::NoFight => json!("nofight"),
                        Judged::Unknown => json!("unknown"),
                        Judged::Pass(_) => json!("pass"),
                    };
                    self.op_emit(
                        "coalesce",
                        json!({"event": "sized_no", "t": frame / 30, "ratio": ratio, "prefix": prefix.len(),
                               "cluster": cluster, "why": why}),
                    );
                    if budget.spent() {
                        sized = true;
                        break;
                    }
                }
            }
            if sized {
                continue;
            }
            let units: Vec<Unit> = members.iter().map(|m| m.2).collect();
            let lone = members.len() == 1;
            self.coalesce.lone_judged += u64::from(lone);
            match self.coalesce_judge(&units, None, false, budget) {
                Judged::Pass(v) => {
                    for u in &units {
                        self.coalesce.holds.remove(&u.id());
                        self.coalesce.assembly_retry.remove(&u.id());
                    }
                    self.coalesce.lone_formed += u64::from(lone);
                    self.coalesce_form(members, v, shadow, cluster);
                }
                Judged::Fail(..) | Judged::Unknown if self.raidcommit_on() => {
                    // An explicit rendezvous, outside weapon coverage after routing.
                    // A singleton can meet a nearby candidate not yet within LINK.
                    let mut points: Vec<_> = units.iter().map(|u| u.pos()).collect();
                    if units.len() == 1
                        && let Some(other) = free
                            .iter()
                            .filter(|m| m.0 != units[0].id() && !self.coalesce_owns(m.0))
                            .filter(|m| distance(m.2.pos(), points[0]) <= 2.0 * LINK)
                            .min_by(|a, b| distance(a.2.pos(), points[0]).total_cmp(&distance(b.2.pos(), points[0])))
                    {
                        points.push(other.2.pos());
                    }
                    let meeting = Vec3::xz(
                        points.iter().map(|p| p.x).sum::<f32>() / points.len() as f32,
                        points.iter().map(|p| p.z).sum::<f32>() / points.len() as f32,
                    );
                    self.raidcommit_hold(&units, meeting);
                }
                _ => {}
            }
        }
        self.coalesce.rotate = start + 1;
    }

    /// The cluster's blocks nearest the fight's focus until they are worth
    /// [`JUST_ENOUGH`] times its armed enemy metal (`coalsize`); `None`
    /// when that is the whole cluster, or there is no fight to size to.
    fn coalesce_prefix(&mut self, members: &[(i32, Origin, Unit)], ratio: f32) -> Option<Vec<(i32, Origin, Unit)>> {
        if members.len() <= MIN_MEMBERS {
            return None;
        }
        let units: Vec<Unit> = members.iter().map(|m| m.2).collect();
        let local = self.local_fight(&units)?;
        let armed: Vec<&Theirs> =
            (local.theirs.iter()).filter(|t| t.armed && !self.coalesce.shy.contains_key(&t.id)).collect();
        if armed.is_empty() {
            return None;
        }
        let n = units.len() as f32;
        let mid = Vec3::xz(
            units.iter().map(|u| u.pos().x).sum::<f32>() / n,
            units.iter().map(|u| u.pos().z).sum::<f32>() / n,
        );
        let focus = (armed.iter()).min_by(|a, b| distance(a.pos, mid).total_cmp(&distance(b.pos, mid)))?.pos;
        let need = ratio * armed.iter().map(|t| t.cost).sum::<f32>();
        let metal = self.metal;
        // Every raider its own block; a squad's leader stays with its squad.
        let mut blocks: Vec<(f32, f32, Vec<usize>)> = Vec::new();
        for (i, m) in members.iter().enumerate() {
            if let Origin::Squad(sid) = m.1
                && self.squads.get(sid).is_some_and(|s| s.leader == Some(m.0))
            {
                continue;
            }
            let d = distance(m.2.pos(), focus);
            let cost = m.2.def().map_or(0.0, |def| def.cost(metal));
            blocks.push((d, cost, vec![i]));
        }
        if blocks.len() < MIN_MEMBERS {
            return None;
        }
        let taken = take_blocks(&mut blocks, need, MIN_MEMBERS);
        if taken.len() >= members.len() {
            return None;
        }
        Some(taken.iter().map(|&i| members[i]).collect())
    }

    /// A clean gang that is not closing on its focus breaks off, and that
    /// enemy is left alone a while (`coalsize`).
    fn coalesce_chase(&mut self, gid: u32, fid: i32, units: &[Unit]) {
        let frame = self.frame;
        let Some(e) = self.war.targets.get(&fid) else { return };
        let Some(ed) = e.ud else { return };
        if e.is_static || e.is_dead {
            return;
        }
        let reach = units.iter().filter_map(|u| u.def()).map(|d| d.max_weapon_range()).fold(0.0, f32::max);
        let n = units.len().max(1) as f32;
        let mid = Vec3::xz(
            units.iter().map(|u| u.pos().x).sum::<f32>() / n,
            units.iter().map(|u| u.pos().z).sum::<f32>() / n,
        );
        let gap = distance(mid, e.position);
        let Some(g) = self.coalesce.gangs.iter_mut().find(|g| g.id == gid) else { return };
        let stalled = g.gap.is_some_and(|(f, last)| f == fid && gap > last - CHASE_CLOSE);
        g.gap = Some((fid, gap));
        g.chasing = if stalled && gap > reach + 50.0 && ed.speed() > 0.0 { g.chasing + 1 } else { 0 };
        if g.chasing >= CHASE_TICKS {
            self.coalesce.shy.insert(fid, frame + SHY);
            self.coalesce.chases += 1;
            self.coalesce_end(gid, "chase");
        }
    }

    fn coalesce_form(&mut self, members: Vec<(i32, Origin, Unit)>, v: Verdict, shadow: bool, cluster: usize) {
        let frame = self.frame;
        for (id, _, _) in &members {
            self.coalesce.holds.remove(id);
            self.coalesce.assembly_retry.remove(id);
        }
        let c = &mut self.coalesce;
        let gid = c.next_id;
        c.next_id += 1;
        let enemies: Vec<(i32, f32, bool)> = (v.record["theirs"].as_array().into_iter().flatten())
            .filter_map(|t| Some((t["id"].as_i64()? as i32, t["value"].as_f64()? as f32, false)))
            .collect();
        let metal: Vec<(Unit, f32)> =
            (members.iter()).map(|m| (m.2, m.2.def().map_or(0.0, |d| d.cost(self.metal)))).collect();
        c.windows.push(Window { gid, frame, shadow, plan: v.plan, members: metal, enemies, predicted: v.tally });
        let origins: Vec<Value> = members.iter().map(|m| json!([m.0, m.1.name()])).collect();
        let mut rec = v.record;
        rec["event"] = json!(if shadow { "would" } else { "form" });
        rec["gang"] = json!(gid);
        rec["t"] = json!(frame / 30);
        rec["members"] = json!(origins);
        rec["lone"] = json!(members.len() == 1);
        rec["cluster"] = json!(cluster);
        rec["spare"] = json!(cluster.saturating_sub(members.len()));
        rec["p_prize"] = json!(v.p_prize);
        if shadow {
            for m in &members {
                c.quiet.insert(m.0, frame + QUIET);
            }
            self.op_emit("coalesce", rec);
            return;
        }
        c.forms += 1;
        for m in &members {
            c.owner.insert(m.0, gid);
            c.leases.insert(
                m.0,
                reserved::lend(
                    self.cb,
                    m.0,
                    reserved::Owner {
                        module: "coalesce",
                        helpers: &[
                            "fac.unitCreated",
                            "war.unitFinished",
                            "war.unitIdle",
                            "retreat",
                            "teleport",
                            "water",
                            "facframe",
                        ],
                    },
                ),
            );
        }
        c.gangs.push(Gang {
            id: gid,
            members: members.clone(),
            plan: v.plan,
            since: frame,
            last_order: -REORDER,
            focus: None,
            fails: 0,
            last: Some((v.tally, v.p_prize)),
            gap: None,
            chasing: 0,
        });
        // Solo raiders leave their scout task (they had stopped for it).
        for m in &members {
            if matches!(m.1, Origin::Solo | Origin::Kodachi)
                && let Some(f) = self.fighters.get_mut(&m.0)
            {
                f.task = None;
            }
        }
        self.op_emit("coalesce", rec);
        self.coalesce_order(gid, v.focus);
    }

    /// Send a gang at its prize, or a clean gang at `focus`.
    fn coalesce_order(&mut self, gid: u32, focus: Option<i32>) {
        let frame = self.frame;
        let Some(g) = self.coalesce.gangs.iter().find(|g| g.id == gid) else { return };
        let target = match g.plan {
            Plan::Prize(id) => Some(id),
            Plan::Clean => focus,
        };
        if g.focus == target && frame - g.last_order < REORDER {
            return;
        }
        let Some(e) = target.and_then(|id| self.war.targets.get(&id)) else { return };
        let (eunit, epos, fresh) = (e.unit, e.position, frame - e.last_seen <= 30);
        let prize = matches!(g.plan, Plan::Prize(_));
        let units: Vec<Unit> = g.members.iter().map(|m| m.2).collect();
        let nav = nav!(self);
        for u in units {
            if prize && fresh {
                u.attack(eunit, 0);
            } else {
                fight_clear(u, epos, 0, &nav);
            }
        }
        if let Some(g) = self.coalesce.gangs.iter_mut().find(|g| g.id == gid) {
            g.focus = target;
            g.last_order = frame;
        }
    }

    fn coalesce_end(&mut self, gid: u32, reason: &str) {
        let frame = self.frame;
        let Some(i) = self.coalesce.gangs.iter().position(|g| g.id == gid) else { return };
        let g = self.coalesce.gangs.remove(i);
        for (id, origin, _) in &g.members {
            self.coalesce.owner.remove(id);
            if let Some(lease) = self.coalesce.leases.remove(id) {
                lease.release();
            }
            match origin {
                Origin::Squad(_) | Origin::Loose => {}
                Origin::Stakeout => self.stakeouts_release(*id),
                Origin::Solo | Origin::Kodachi => {
                    if let Some(f) = self.fighters.get_mut(id) {
                        f.last_task_frame = 0;
                    }
                }
            }
        }
        if self.raidcommit_on() {
            let units: Vec<_> = g
                .members
                .iter()
                .map(|m| m.2)
                .filter(|u| self.raidcommit_available(*u) && !self.qrf.members.contains_key(&u.id()))
                .collect();
            for unit in &units {
                let haven = self.graph_get_closest_raider_haven(unit.pos());
                self.raidcommit_move(*unit, haven);
                self.raidcommit_hold(&[*unit], haven);
            }
        }
        let alive = g.members.iter().filter(|m| m.2.health() > 0.0 && m.2.team() == self.team_id).count();
        self.op_emit(
            "coalesce",
            json!({
                "event": "end", "gang": gid, "t": frame / 30, "reason": reason, "plan": g.plan.name(),
                "life": (frame - g.since) / 30, "members": g.members.len(), "alive": alive,
                "last": g.last.map(|(t, p)| json!({"tally": tally_json(&t), "p_prize": p})),
            }),
        );
    }

    fn coalesce_close_windows(&mut self, frame: i32) {
        let ws = std::mem::take(&mut self.coalesce.windows);
        let (due, open): (Vec<Window>, Vec<Window>) = ws.into_iter().partition(|w| frame - w.frame >= WINDOW);
        self.coalesce.windows = open;
        for w in due {
            let lost: f32 =
                w.members.iter().filter(|(u, _)| u.health() <= 0.0 || u.team() != self.team_id).map(|m| m.1).sum();
            let dead = w.members.iter().filter(|(u, _)| u.health() <= 0.0 || u.team() != self.team_id).count();
            let killed: f32 = w.enemies.iter().filter(|e| e.2).map(|e| e.1).sum();
            let prize_dead = match w.plan {
                Plan::Prize(id) => Some(w.enemies.iter().any(|e| e.0 == id && e.2)),
                Plan::Clean => None,
            };
            self.op_emit(
                "coalesce",
                json!({
                    "event": "window", "gang": w.gid, "t": frame / 30, "form_t": w.frame / 30, "shadow": w.shadow,
                    "plan": w.plan.name(), "members": w.members.len(), "dead": dead, "lost": lost,
                    "metal": w.members.iter().map(|m| m.1).sum::<f32>(),
                    "enemies": w.enemies.len(), "enemy_dead": w.enemies.iter().filter(|e| e.2).count(),
                    "killed_value": killed, "prize_dead": prize_dead,
                    "predicted_loss": w.predicted.own_loss, "predicted_kill": w.predicted.kill_value,
                }),
            );
        }
    }

    fn coalesce_report(&mut self, frame: i32) {
        let mode = if self.coalesce_on() { "on" } else { "shadow" };
        let c = &mut self.coalesce;
        let minutes = ((frame - c.last_report) as f64 / 1800.0).max(1e-6);
        c.last_report = frame;
        if c.scans == 0 {
            return;
        }
        let data = json!({
            "event": "cost", "t": frame / 30, "mode": mode,
            "scans_per_min": c.scans as f64 / minutes, "queries": c.queries,
            "mean_us": c.micros as f64 / c.scans as f64, "over_budget": c.over_budget, "forms": c.forms,
            "gangs": c.gangs.len(), "lone_judged": c.lone_judged, "lone_formed": c.lone_formed,
            "sized": c.sized, "sized_failed": c.sized_failed, "chases": c.chases,
        });
        c.scans = 0;
        c.micros = 0;
        c.queries = 0;
        c.over_budget = 0;
        self.op_emit("coalesce", data);
    }
}

/// The judgement as the journal keeps it: plan, sums, and each unit's
/// fate (enemies with their worth, for the windows).
fn coalesce_record(local: &Local, fates: &[UnitFate], plan: Plan, t: &Tally, p_prize: f32) -> Value {
    let n = local.ours.len();
    let ours: Vec<Value> = (local.ours.iter().zip(fates))
        .map(|(o, f)| json!({"id": o.id, "cost": o.cost, "p": f.p_raw, "pc": f.p_dead}))
        .collect();
    let k = kills(local, fates);
    let theirs: Vec<Value> = (local.theirs.iter().zip(&fates[n.min(fates.len())..]).zip(k).enumerate())
        .map(|(i, ((e, f), kv))| {
            let tubes = local.snap.units.get(n + i).map_or(&[][..], |u| &u.tubes[..]);
            json!({"id": e.id, "def": e.def, "cost": e.cost, "value": e.value, "armed": e.armed,
                   "prize": e.prize, "p": f.p_raw, "pc": f.p_dead, "kill": kv, "tubes": tubes})
        })
        .collect();
    json!({
        "plan": plan.name(), "prize": match plan { Plan::Prize(id) => json!(id), Plan::Clean => Value::Null },
        "tally": tally_json(t), "tally_cal": tally_json(&tally(local, fates, false)), "p_prize": p_prize,
        "ours": ours, "theirs": theirs,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_gang_takes_the_nearest_blocks_until_just_enough() {
        // Four solos at 100/200/300/400 elmos worth 70 each vs need 150: the two nearest.
        let mut b =
            vec![(300.0, 70.0, vec![2]), (100.0, 70.0, vec![0]), (400.0, 70.0, vec![3]), (200.0, 70.0, vec![1])];
        assert_eq!(take_blocks(&mut b, 150.0, 2), vec![0, 1, 2]);
        assert_eq!(take_blocks(&mut b, 100.0, 2), vec![0, 1]);
        // A squad is one block: taken whole, and enough on its own.
        let mut b = vec![(250.0, 210.0, vec![1, 2, 3]), (100.0, 70.0, vec![0])];
        assert_eq!(take_blocks(&mut b, 150.0, 2), vec![0, 1, 2, 3]);
        let mut b = vec![(50.0, 210.0, vec![1, 2, 3]), (100.0, 70.0, vec![0])];
        assert_eq!(take_blocks(&mut b, 150.0, 2), vec![1, 2, 3]);
        // Never fewer than the minimum when the cluster has them.
        let mut b = vec![(50.0, 500.0, vec![0]), (100.0, 70.0, vec![1])];
        assert_eq!(take_blocks(&mut b, 150.0, 2), vec![0, 1]);
    }

    fn t(own_cost: f32, own_loss: f32, kill_value: f32, armed_share: f32, min_armed_p: f32) -> Tally {
        Tally { own_cost, own_loss, kill_value, armed_cost: 100.0, armed_share, min_armed_p }
    }

    #[test]
    fn clean_needs_every_armed_enemy_likely_dead_for_little() {
        assert!(passes(Plan::Clean, &t(400.0, 60.0, 300.0, 0.9, 0.9), 0.0, false, MIN_PRIZE_METAL));
        // One armed enemy likely to live.
        assert!(!passes(Plan::Clean, &t(400.0, 60.0, 300.0, 0.9, 0.4), 0.0, false, MIN_PRIZE_METAL));
        // Too dear.
        assert!(!passes(Plan::Clean, &t(400.0, 150.0, 300.0, 0.9, 0.7), 0.0, false, MIN_PRIZE_METAL));
        // Not worth it.
        assert!(!passes(Plan::Clean, &t(400.0, 30.0, 40.0, 0.9, 0.7), 0.0, false, MIN_PRIZE_METAL));
        // Holding is looser.
        assert!(passes(Plan::Clean, &t(400.0, 150.0, 300.0, 0.6, 0.2), 0.0, true, MIN_PRIZE_METAL));
        assert!(!passes(Plan::Clean, &t(400.0, 250.0, 300.0, 0.6, 0.2), 0.0, true, MIN_PRIZE_METAL));
    }

    #[test]
    fn a_prize_is_worth_half_its_value_in_raiders() {
        let heavy = t(600.0, 400.0, 900.0, 0.3, 0.1);
        assert!(passes(Plan::Prize(1), &heavy, 0.8, false, MIN_PRIZE_METAL));
        assert!(!passes(Plan::Prize(1), &heavy, 0.6, false, MIN_PRIZE_METAL));
        assert!(!passes(Plan::Prize(1), &t(600.0, 500.0, 900.0, 0.3, 0.1), 0.8, false, MIN_PRIZE_METAL));
        assert!(passes(Plan::Prize(1), &t(600.0, 500.0, 900.0, 0.3, 0.1), 0.5, true, MIN_PRIZE_METAL));
    }

    fn fixture_spec(name: &str) -> std::sync::Arc<crate::military::fightsim::UnitSpec> {
        use crate::military::fightsim::{UnitSpec, WeaponSpec};
        let line = include_str!("fightsim/testdata/mech_parity_units.tsv")
            .lines()
            .chain(include_str!("fightsim/testdata/mech_tacai_units.tsv").lines())
            .find(|l| l.split('\t').next() == Some(name))
            .unwrap();
        let f: Vec<_> = line.split('\t').collect();
        std::sync::Arc::new(UnitSpec {
            def: name.into(),
            cost: f[1].parse().unwrap(),
            health: f[2].parse().unwrap(),
            speed: f[3].parse().unwrap(),
            mobile: f[4] == "1",
            air: f[5] == "1",
            builder: f[6] == "1",
            com: f[7] == "1",
            radius: f[8].parse().unwrap(),
            weapons: f[9]
                .split('|')
                .filter(|w| !w.is_empty())
                .map(|w| {
                    let p: Vec<_> = w.split(':').collect();
                    WeaponSpec {
                        name: p[0].into(),
                        weapon_type: p[1].into(),
                        leading: None,
                        range: p[2].parse().unwrap(),
                        reload: p[3].parse().unwrap(),
                        salvo: p[4].parse().unwrap(),
                        damage: p[5].parse().unwrap(),
                    }
                })
                .collect(),
        })
    }

    fn glaive_encounter(positions: &[(f32, f32)], enemy: &str) -> Tally {
        use crate::military::fightsim::local::{HORIZON, Ours, Theirs};
        use crate::military::fightsim::{FightSnapshot, Model, SimFlags, SnapUnit};
        let glaive = fixture_spec("cloakraid");
        let foe = fixture_spec(enemy);
        let mut local = Local {
            snap: FightSnapshot { flags: SimFlags::from_switches(false, true, true), ..Default::default() },
            ours: Vec::new(),
            theirs: Vec::new(),
            unknown: false,
        };
        for (id, &(x, z)) in positions.iter().enumerate() {
            local.ours.push(Ours { id: id as i32, cost: glaive.cost });
            local.snap.units.push(SnapUnit {
                side: 0,
                spec: glaive.clone(),
                hp: glaive.health,
                x,
                z,
                reloads: Vec::new(),
                tubes: Vec::new(),
            });
        }
        local.theirs.push(Theirs {
            id: 100,
            def: enemy.into(),
            cost: foe.cost,
            value: foe.cost,
            armed: true,
            prize: false,
            pos: Vec3::xz(0.0, 0.0),
        });
        local.snap.units.push(SnapUnit {
            side: 1,
            hp: foe.health,
            spec: foe,
            x: 0.0,
            z: 0.0,
            reloads: Vec::new(),
            tubes: Vec::new(),
        });
        local.snap.flags.ordered = true;
        let fates = Model::load().unwrap().kiting_fates(&local.snap, HORIZON);
        tally(&local, &fates, true)
    }

    #[test]
    fn raidcommit_rejects_lone_glaive_against_blitz_and_kodachi() {
        for foe in ["tankheavyraid", "tankraid"] {
            let t = glaive_encounter(&[(350.0, 0.0)], foe);
            assert!(!passes(Plan::Clean, &t, 0.0, false, 0.0), "{foe}: {t:?}");
        }
    }

    #[test]
    fn raidcommit_distant_support_cannot_license_the_first_glaive() {
        let t = glaive_encounter(&[(300.0, 0.0), (1200.0, 0.0)], "tankheavyraid");
        assert!(!passes(Plan::Clean, &t, 0.0, false, 0.0), "{t:?}");
    }

    #[test]
    fn raidcommit_can_commit_a_real_pack_and_a_winning_singleton() {
        let positions: Vec<_> = (0..12).map(|i| (300.0 + (i / 4) as f32 * 24.0, (i % 4) as f32 * 24.0)).collect();
        let t = glaive_encounter(&positions, "tankheavyraid");
        assert!(passes(Plan::Clean, &t, 0.0, false, 0.0), "pack: {t:?}");
        let t = glaive_encounter(&[(160.0, 0.0)], "spiderscout");
        assert!(passes(Plan::Clean, &t, 0.0, false, 0.0), "singleton: {t:?}");
    }

    #[test]
    fn clusters_link_by_distance_and_by_squad() {
        let p = [Vec3::xz(0.0, 0.0), Vec3::xz(400.0, 0.0), Vec3::xz(800.0, 0.0), Vec3::xz(3000.0, 0.0)];
        let g = clusters(&p, |_, _| false);
        assert_eq!(g.len(), 2);
        assert!(g.iter().any(|c| c.len() == 3));
        let g = clusters(&p, |i, j| (i, j) == (0, 3));
        assert_eq!(g.len(), 1);
    }
}
