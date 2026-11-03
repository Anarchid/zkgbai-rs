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
use springai::{Unit, Vec3};

use crate::Ai;
use crate::military::SquadId;
use crate::military::fightsim::local::{Budget, CONTACT, Local, Tally, kills, tally, their_index};
use crate::military::fightsim::{FateMode, UnitFate};
use crate::military::wrappers::fighter::fight_clear;
use crate::nav;
use crate::util::distance;

pub const FEATURE: &str = "coalesce";
pub const SHADOW: &str = "coalesceshadow";

/// Frames between scans (2 Hz).
const PERIOD: i32 = 15;
/// Raiders this close are one cluster (single linkage).
const LINK: f32 = 450.0;
/// The smallest cluster judged: one raider alone is philosoraider's...
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
    next_id: u32,
    windows: Vec<Window>,
    /// Shadow records: unit id -> frame until which it is not recorded.
    quiet: BTreeMap<i32, i32>,
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
    last_report: i32,
}

fn passes(plan: Plan, t: &Tally, p_prize: f32, holding: bool) -> bool {
    if t.kill_value < MIN_PRIZE_METAL {
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
        !self.dev_off(FEATURE)
    }

    fn coalesce_runs(&self) -> bool {
        self.coalesce_on() || !self.dev_off(SHADOW)
    }

    /// Whether unit `id` is borrowed by a gang (its controller skips it).
    pub fn coalesce_owns(&self, id: i32) -> bool {
        self.coalesce.owner.contains_key(&id)
    }

    /// Whether any member of raider squad `sid` is borrowed.
    pub fn coalesce_owns_squad(&self, sid: SquadId) -> bool {
        !self.coalesce.owner.is_empty()
            && self.squads.get(sid).is_some_and(|s| {
                s.fighters.iter().chain(s.leader.iter()).any(|id| self.coalesce.owner.contains_key(id))
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
            u.health() > 0.0
                && u.team() == self.team_id
                && !u.is_being_built()
                && !def.is_able_to_fly()
                && def.max_weapon_range() > 0.0
                && u.pos().y >= -5.0
                && !(u.is_cloaked() && self.defs.name(def) == "cloakheavyraid")
                && !self.retreat_is_retreating(u)
                && !self.squad_strike_owns(id)
                && !self.fire_controls(id)
                && !self.blasts_waiting(u.pos())
                && !self.stayveto_borrowed(id)
        });
        out
    }

    /// Judge `units` (with their origins) as one gang: the best plan that
    /// passes, holding `hold` (the plan to keep) with the looser thresholds.
    fn coalesce_judge(&mut self, units: &[Unit], hold: Option<Plan>, budget: &mut Budget) -> Judged {
        let Some(mut local) = self.local_fight(units) else { return Judged::NoFight };
        // A gang is borrowed, committed and ordered: it fires for certain
        // (`ordered`, SimFlags::ordered).
        local.snap.flags.ordered = !self.dev_off(crate::military::fightsim::ORDERED);
        if !local.armed_theirs() {
            return Judged::NoFight;
        }
        if local.unknown {
            return Judged::Unknown;
        }
        let mid = {
            let n = units.len().max(1) as f32;
            Vec3::xz(units.iter().map(|u| u.pos().x).sum::<f32>() / n, units.iter().map(|u| u.pos().z).sum::<f32>() / n)
        };
        let focus = (local.theirs.iter())
            .filter(|t| t.armed)
            .min_by(|a, b| distance(a.pos, mid).total_cmp(&distance(b.pos, mid)))
            .map(|t| t.id);
        let mut plans: Vec<Plan> = match hold {
            Some(p) => vec![p],
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
            let (mode, prize_index) = match plan {
                Plan::Clean => (FateMode::AMove, None),
                Plan::Prize(id) => {
                    let Some(i) = local.theirs.iter().position(|t| t.id == id) else { continue };
                    (FateMode::Assassinate { target: their_index(&local, i) }, Some(i))
                }
            };
            let Some(fates) = self.local_fates(&local, mode, budget) else { return Judged::NoFight };
            let t = tally(&local, &fates, true);
            let p_prize = prize_index.map_or(0.0, |i| fates[their_index(&local, i)].p_raw);
            last = Some((t, p_prize));
            if !passes(plan, &t, p_prize, hold.is_some()) || best.as_ref().is_some_and(|b| b.tally.net() >= t.net()) {
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
                if fails >= FAILS {
                    ai.coalesce_end(gid, reason);
                }
            };
            match self.coalesce_judge(&units, Some(plan), budget) {
                Judged::Pass(v) => {
                    if let Some(g) = self.coalesce.gangs.iter_mut().find(|g| g.id == gid) {
                        g.fails = 0;
                        g.last = Some((v.tally, v.p_prize));
                    }
                    self.coalesce_order(gid, v.focus);
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
        .filter(|g| g.len() >= MIN_MEMBERS || self.ammo_open_near(points[g[0]], CONTACT, LONE_OPEN))
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
            let units: Vec<Unit> = members.iter().map(|m| m.2).collect();
            let lone = members.len() == 1;
            self.coalesce.lone_judged += u64::from(lone);
            let Judged::Pass(v) = self.coalesce_judge(&units, None, budget) else { continue };
            self.coalesce.lone_formed += u64::from(lone);
            self.coalesce_form(members, v, shadow);
        }
        self.coalesce.rotate = start + 1;
    }

    fn coalesce_form(&mut self, members: Vec<(i32, Origin, Unit)>, v: Verdict, shadow: bool) {
        let frame = self.frame;
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
            match origin {
                Origin::Stakeout => self.stakeouts_release(*id),
                Origin::Solo | Origin::Kodachi => {
                    if let Some(f) = self.fighters.get_mut(id) {
                        f.last_task_frame = 0;
                    }
                }
                _ => {}
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

    fn t(own_cost: f32, own_loss: f32, kill_value: f32, armed_share: f32, min_armed_p: f32) -> Tally {
        Tally { own_cost, own_loss, kill_value, armed_cost: 100.0, armed_share, min_armed_p }
    }

    #[test]
    fn clean_needs_every_armed_enemy_likely_dead_for_little() {
        assert!(passes(Plan::Clean, &t(400.0, 60.0, 300.0, 0.9, 0.9), 0.0, false));
        // One armed enemy likely to live.
        assert!(!passes(Plan::Clean, &t(400.0, 60.0, 300.0, 0.9, 0.4), 0.0, false));
        // Too dear.
        assert!(!passes(Plan::Clean, &t(400.0, 150.0, 300.0, 0.9, 0.7), 0.0, false));
        // Not worth it.
        assert!(!passes(Plan::Clean, &t(400.0, 30.0, 40.0, 0.9, 0.7), 0.0, false));
        // Holding is looser.
        assert!(passes(Plan::Clean, &t(400.0, 150.0, 300.0, 0.6, 0.2), 0.0, true));
        assert!(!passes(Plan::Clean, &t(400.0, 250.0, 300.0, 0.6, 0.2), 0.0, true));
    }

    #[test]
    fn a_prize_is_worth_half_its_value_in_raiders() {
        let heavy = t(600.0, 400.0, 900.0, 0.3, 0.1);
        assert!(passes(Plan::Prize(1), &heavy, 0.8, false));
        assert!(!passes(Plan::Prize(1), &heavy, 0.6, false));
        assert!(!passes(Plan::Prize(1), &t(600.0, 500.0, 900.0, 0.3, 0.1), 0.8, false));
        assert!(passes(Plan::Prize(1), &t(600.0, 500.0, 900.0, 0.3, 0.1), 0.5, true));
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
