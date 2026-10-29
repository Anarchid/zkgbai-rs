//! The `mark` QRF algorithm: man-marking, the way a good player screens
//! with raiders. Each raid near our value gets a team (matched to raids by
//! how late each team would be at each raid's threatened assets, a
//! reach-avoid matching) that stands between it and what it can reach
//! soon: of the places around the raid, the one from which the team gets
//! to every threatened asset before the raid does (or late by the least
//! value-weighted time), close to the raid and soon reached. If the team
//! gets to the asset the raid heads for (by heading and how soon) first,
//! it stands on that asset instead: raiders must come into our guns to
//! shoot it. Presence alone does not keep the AI's raiders off (they go
//! for the mexes anyway), so the team strikes, when clearly stronger, a
//! raid that comes close or breaches (reaches an asset soon, before the
//! team could), and never chases it out of its mark. Raids beyond the
//! teams are marked in pairs (one team between two raids). Teams with no
//! raid stand a forward screen: where the raids seen so far came in, just
//! ahead of the first asset each would reach. Tuning for experiments: AI
//! options `mark_<field of Tune>`.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use springai::Vec3;

use super::field::{Field, NEVER};
use super::live::{DRAFT_COOLDOWN, Doing, MOON, PRESSED_RECALL, RoleKind, Tactic};
use crate::Ai;
use crate::military::SquadId;
use crate::military::qrf::station::k_means;
use crate::util::distance;

/// A raid threatens the assets it gets to within this many seconds.
const HORIZON: f32 = 45.0;

/// Seconds a team should be at an asset ahead of the raid.
const MARGIN: f32 = 2.0;
/// A moonlighting squad is sent only at a raid that damaged an asset of
/// ours within `PRESSED_RECALL` (heavy raids, `RAID_WEAK` and up, need
/// not have), and only where it gets to a threatened asset this many
/// seconds before the raid does (an asset under fire: before it dies).
const SLACK: f32 = 10.0;

/// Ground is this much longer than straight, for a team from a candidate
/// place (the raid's times are true travel times).
const DETOUR: f32 = 1.2;

/// Candidate places are sampled this far apart, within this far of the
/// raid.
const STEP: f32 = 96.0;
const REACH: f32 = 1800.0;

/// A team stands about this far from the raid (beyond raider weapons),
/// and a candidate costs this many seconds per elmo away from that.
const STANDOFF: f32 = 300.0;
const STANDOFF_COST: f32 = 0.012;

/// A candidate costs this share of the seconds the team needs to get there.
const TRANSIT_COST: f32 = 0.3;

/// A team strikes a raid this close when this much stronger; it breaks off
/// once the raid is this far from its mark.
const STRIKE_RANGE: f32 = 550.0;
const STRIKE_RATIO: f32 = 1.5;
const LEASH: f32 = 700.0;

/// Assets a raid reaches within this many seconds, ahead of its team, are
/// breached: the team goes at the raid (if strong enough).
const BREACH: f32 = 12.0;

/// A raid this much stronger than its team gets a second team.
const DOUBLE_RATIO: f32 = 0.8;
/// A team sent at a raid keeps this many times the raid's power, the rest
/// split off (see `qrf_mark_split`).
const JUST_ENOUGH: f32 = 2.0;

/// Entry points remembered.
const ENTRIES: usize = 48;

/// A screen post stands this far from its gate asset toward the entry.
const POST_AHEAD: f32 = 450.0;

/// Frames between re-placing the screen.
const RESITE: i32 = 30 * 10;

/// Tuning, from AI options `mark_<name>` (for experiments).
#[derive(Clone, Copy)]
struct Tune {
    /// Strike when this much stronger (0: never).
    strike: f32,
    /// Strike only a raid within this far of an asset (0: anywhere).
    commit: f32,
    /// Mark with a fight order (else a move: shooting on the way only).
    fight: bool,
    standoff: f32,
    /// Screen posts this far ahead of their gates.
    ahead: f32,
    /// Weight of lateness against transit and standoff in the spot.
    latew: f32,
    /// Strike breaches.
    breach: bool,
    /// Seconds per elmo off the standoff, in the spot.
    standw: f32,
    /// Meet raids on their way when stronger.
    intercept: bool,
    /// Stand on the asset a raid heads for when there first.
    goal: bool,
    /// Screen posts over our value where raids reach it (clusters of our
    /// assets by cost and exposure), not only at the gates raids used.
    value: bool,
}

impl Default for Tune {
    fn default() -> Self {
        Tune {
            strike: STRIKE_RATIO,
            commit: 0.0,
            fight: true,
            standoff: STANDOFF,
            ahead: POST_AHEAD,
            latew: 1.0,
            breach: true,
            standw: STANDOFF_COST,
            intercept: false,
            goal: true,
            value: false,
        }
    }
}

#[derive(Default)]
pub struct Mark {
    tune: Option<Tune>,
    /// Team -> track ids of what it marked last.
    pub(super) marks: BTreeMap<usize, BTreeSet<i32>>,
    /// Where raids were first seen.
    entries: Vec<Vec3>,
    seen: BTreeSet<i32>,
    posts: Vec<Vec3>,
    sited: i32,
    stats: Stats,
}

#[derive(Default)]
struct Stats {
    marks: u32,
    strikes: u32,
    /// Members split off a team sent at a raid weaker than it needs.
    splits: u32,
    screens: u32,
}

/// One or more raids a team marks.
struct Target {
    /// Indices into `qrf.groups`.
    groups: Vec<usize>,
    center: Vec3,
    power: f32,
    /// Threatened assets: index into `qrf.assets`, the raid's seconds.
    threatened: Vec<(usize, f32)>,
    urgency: f32,
}

/// The assignment of rows (teams) to distinct columns (targets), one per
/// column as long as rows last, of least total cost (exhaustive: a few of
/// each).
fn best_match(cost: &[Vec<f32>]) -> Vec<(usize, usize)> {
    let rows = cost.len();
    let cols = cost.first().map_or(0, Vec::len);
    let mut best: (f32, Vec<(usize, usize)>) = (f32::INFINITY, Vec::new());
    fn go(
        c: usize,
        cols: usize,
        cost: &[Vec<f32>],
        used: &mut Vec<bool>,
        acc: f32,
        cur: &mut Vec<(usize, usize)>,
        best: &mut (f32, Vec<(usize, usize)>),
    ) {
        if c == cols || cur.len() == used.len() {
            if acc < best.0 {
                *best = (acc, cur.clone());
            }
            return;
        }
        for r in 0..used.len() {
            if used[r] {
                continue;
            }
            used[r] = true;
            cur.push((r, c));
            go(c + 1, cols, cost, used, acc + cost[r][c], cur, best);
            cur.pop();
            used[r] = false;
        }
    }
    if rows == 0 || cols == 0 || rows > 7 {
        return (0..rows.min(cols)).map(|i| (i, i)).collect();
    }
    let mut used = vec![false; rows];
    go(0, cols, cost, &mut used, 0.0, &mut Vec::new(), &mut best);
    best.1
}

impl Ai {
    fn qrf_mark_tune(&mut self) -> Tune {
        if let Some(t) = self.qrf.mark.tune {
            return t;
        }
        let f = |k: &str, d: f32| self.cb.option(k).and_then(|v| v.parse::<f32>().ok()).unwrap_or(d);
        let d = Tune::default();
        let t = Tune {
            strike: f("mark_strike", d.strike),
            commit: f("mark_commit", d.commit),
            fight: f("mark_fight", 1.0) > 0.0,
            standoff: f("mark_standoff", d.standoff),
            ahead: f("mark_ahead", d.ahead),
            latew: f("mark_latew", d.latew),
            breach: f("mark_breach", 1.0) > 0.0,
            standw: f("mark_standw", d.standw),
            intercept: f("mark_intercept", 0.0) > 0.0,
            goal: f("mark_goal", 1.0) > 0.0,
            value: f("mark_value", 0.0) > 0.0,
        };
        self.log(&format!(
            "qrfmark: strike={} commit={} fight={} standoff={} ahead={} latew={} breach={} standw={} intercept={} goal={} value={}",
            t.strike, t.commit, t.fight, t.standoff, t.ahead, t.latew, t.breach, t.standw, t.intercept, t.goal, t.value
        ));
        self.qrf.mark.tune = Some(t);
        t
    }

    /// A team's tuning: the operator's tactic over the algorithm's own.
    fn qrf_mark_team_tune(&self, team: usize, tune: Tune) -> Tune {
        match self.qrf.roles.get(&team).map(|r| r.tactic) {
            Some(Tactic::Shadow) => Tune { strike: 0.0, breach: false, ..tune },
            Some(Tactic::Hunt) => Tune { strike: 1.0, intercept: true, ..tune },
            _ => tune,
        }
    }

    /// Whether a team may take a target: a zone team only raids on our
    /// value within its zone; a holding team none.
    fn qrf_mark_allows(&self, team: usize, t: &Target) -> bool {
        match self.qrf.roles.get(&team) {
            Some(r) if r.kind == RoleKind::Hold => false,
            Some(r) if r.kind == RoleKind::Zone => {
                t.threatened.iter().any(|&(i, _)| distance(self.qrf.assets[i].pos, r.pos) <= r.radius)
            }
            _ => true,
        }
    }

    fn qrf_mark_doing(&mut self, team: usize, what: &'static str, at: Vec3, raid: f32) {
        let frame = self.frame;
        self.qrf.doing.insert(team, Doing { what, at, raid, frame });
    }

    pub(super) fn qrf_mark_update(&mut self, frame: i32) {
        let tune = self.qrf_mark_tune();
        self.qrf_mark_merge_teams();
        let mut all_teams = self.qrf_teams();
        if all_teams.is_empty() {
            return;
        }
        self.qrf_mark_entries();
        // Teams holding a place are not matched to raids.
        let mut teams: BTreeMap<usize, Vec<i32>> = (all_teams.iter())
            .filter(|(t, _)| self.qrf.roles.get(t).is_none_or(|r| r.kind != RoleKind::Hold))
            .map(|(&t, ids)| (t, ids.clone()))
            .collect();

        // The raids that threaten anything, with their travel times.
        let mut targets: Vec<Target> = Vec::new();
        // In normal play the travel times go only as far as they are used
        // (the staged game keeps the whole map, as it was tuned on).
        let (raid_max, team_max) = if self.qrf.live { (HORIZON + 15.0, HORIZON * 2.0) } else { (NEVER, NEVER) };
        if self.qrf.live && !self.qrf_prepare_fields(&teams, raid_max, team_max) {
            self.qrf.plan_pending = true;
            return;
        }
        // A team's travel times: kept while it stands, in normal play.
        let team_field = |ai: &Ai, team: usize, ids: &[i32]| -> Option<Arc<Field>> {
            if ai.qrf.live {
                ai.qrf.async_fields.team(team)
            } else {
                ai.qrf_team_field_within(ids, team_max).map(Arc::new)
            }
        };
        let group_fields: Vec<Option<Arc<Field>>> = self
            .qrf
            .groups
            .iter()
            .map(|g| {
                if self.qrf.live {
                    self.qrf.async_fields.group(&g.members)
                } else {
                    self.qrf_group_field_within(g, raid_max).map(Arc::new)
                }
            })
            .collect();
        for (gi, f) in group_fields.iter().enumerate() {
            let Some(f) = f.as_ref() else { continue };
            let g = &self.qrf.groups[gi];
            let threatened: Vec<(usize, f32)> = (self.qrf.assets.iter().enumerate())
                .map(|(i, a)| (i, f.at(a.pos)))
                .filter(|&(_, t)| t < HORIZON)
                .collect();
            if threatened.is_empty() {
                continue;
            }
            let urgency = threatened.iter().map(|&(i, t)| self.qrf.assets[i].cost / (t + 5.0)).sum();
            targets.push(Target { groups: vec![gi], center: g.center, power: g.power, threatened, urgency });
        }
        // More raids than teams: pair the closest raids up.
        while targets.len() > teams.len() && targets.len() >= 2 {
            let mut best = (f32::INFINITY, 0, 1);
            for i in 0..targets.len() {
                for j in i + 1..targets.len() {
                    let d = distance(targets[i].center, targets[j].center);
                    if d < best.0 {
                        best = (d, i, j);
                    }
                }
            }
            let b = targets.remove(best.2);
            let a = &mut targets[best.1];
            let (wa, wb) = (a.power.max(1.0), b.power.max(1.0));
            a.center = (a.center * wa + b.center * wb) * (1.0 / (wa + wb));
            a.power += b.power;
            a.urgency += b.urgency;
            a.groups.extend(b.groups);
            for (i, t) in b.threatened {
                match a.threatened.iter_mut().find(|(j, _)| *j == i) {
                    Some(e) => e.1 = e.1.min(t),
                    None => a.threatened.push((i, t)),
                }
            }
        }
        targets.sort_by(|a, b| b.urgency.total_cmp(&a.urgency));

        // Pairwise outcomes (a reach-avoid matching): how late, in value
        // times seconds, each team gets to each target's threatened assets
        // from where it is now; the teams go where the sum is least, a
        // team that marked a raid keeping it unless another does much
        // better. Strong raids get a second team if one is left.
        let tracks_of = |t: &Target, ai: &Ai| -> BTreeSet<i32> {
            t.groups.iter().flat_map(|&g| ai.qrf.groups[g].members.iter().copied()).collect()
        };
        // The exhaustive matching takes seven teams at most: those nearest
        // a target.
        let mut team_ids: Vec<usize> = teams.keys().copied().collect();
        if team_ids.len() > 7 && !targets.is_empty() {
            let gap = |t: &usize| {
                let p = self.qrf_team_pos(&teams[t]).0;
                targets.iter().map(|x| distance(x.center, p)).fold(f32::INFINITY, f32::min)
            };
            team_ids.sort_by(|a, b| gap(a).total_cmp(&gap(b)));
            team_ids.truncate(7);
        }
        // (No raid, no travel times: they are the costliest part.)
        let fields: Vec<Option<Arc<Field>>> = (team_ids.iter())
            .map(|&t| if targets.is_empty() { None } else { team_field(self, t, &teams[&t]) })
            .collect();
        let mut cost = vec![vec![0.0f32; targets.len()]; team_ids.len()];
        // Of that, what is late among the assets the raid reaches soon: a
        // breach (the raid is inside our value), where guarding has failed.
        let mut breach = vec![vec![0.0f32; targets.len()]; team_ids.len()];
        // A moonlighting squad goes only at a raid that has pressed, and
        // only where it arrives with slack (its path time, not the crow's).
        // An asset the raid is shooting: its deadline is its time to die
        // (health over a tenth of the raid's power a second), not the
        // raid's path time, which is nought.
        let deadline = |t: &Target, i: usize, tr: f32, ai: &Ai| -> f32 {
            let a = &ai.qrf.assets[i];
            let id = a.unit.id();
            let under_fire = (t.groups.iter()).any(|&g| {
                let g = &ai.qrf.groups[g];
                frame - g.pressed <= PRESSED_RECALL && g.pressed_on.contains(&id)
            });
            if under_fire { a.unit.health() / (t.power / 10.0).max(1.0) } else { tr }
        };
        let mut allowed = vec![vec![true; targets.len()]; team_ids.len()];
        for (a, &team) in team_ids.iter().enumerate() {
            let (pos, _) = self.qrf_team_pos(&teams[&team]);
            for (b, t) in targets.iter().enumerate() {
                let value: f32 = t.threatened.iter().map(|&(i, _)| self.qrf.assets[i].cost).sum();
                let mut late = 0.0;
                let mut slack = false;
                for &(i, tr) in &t.threatened {
                    let at = self.qrf.assets[i].pos;
                    let mine = fields[a].as_ref().map_or(NEVER, |f| f.at(at)).min(HORIZON * 2.0);
                    let l = self.qrf.assets[i].cost * (mine + MARGIN - tr).max(0.0);
                    late += l;
                    if tr < BREACH {
                        breach[a][b] += l;
                    }
                    if mine + SLACK <= deadline(t, i, tr, self) {
                        slack = true;
                    }
                }
                let ids = tracks_of(t, self);
                let kept = self.qrf.mark.marks.get(&team).is_some_and(|m| !m.is_disjoint(&ids));
                cost[a][b] =
                    late + 0.01 * value * distance(pos, t.center) / 100.0 - if kept { 3.0 * value } else { 0.0 };
                let cooling = team >= MOON
                    && (self.qrf.draft_released.get(&((team - MOON) as SquadId)))
                        .is_some_and(|f| frame - f < DRAFT_COOLDOWN);
                // (Pressed is no longer required: on Agalta the drafts it
                // refused were answering raids before they pressed, and
                // mexes lost by 10:00 went 5.5 to 8.7; the slack test and
                // the hold in seconds are what the rule keeps.)
                allowed[a][b] = self.qrf_mark_allows(team, t) && (team < MOON || slack && !cooling);
                if !allowed[a][b] {
                    cost[a][b] += 1e9;
                }
            }
        }
        let mut plan: Vec<(usize, usize)> =
            (best_match(&cost).into_iter()).filter(|&(a, b)| allowed[a][b]).map(|(a, b)| (team_ids[a], b)).collect();
        let mut free: BTreeSet<usize> = all_teams.keys().copied().collect();
        for (team, _) in &plan {
            free.remove(team);
        }
        for (ti, t) in targets.iter().enumerate() {
            let mine: f32 = plan.iter().filter(|p| p.1 == ti).map(|p| self.qrf_team_power(&teams[&p.0])).sum();
            if free.is_empty() || t.power < DOUBLE_RATIO * mine {
                continue;
            }
            let cooling = |f: usize, ai: &Ai| {
                f >= MOON
                    && (ai.qrf.draft_released.get(&((f - MOON) as SquadId))).is_some_and(|r| frame - r < DRAFT_COOLDOWN)
            };
            let Some(team) = (free.iter().copied())
                .filter(|&f| teams.contains_key(&f) && self.qrf_mark_allows(f, t) && (f < MOON || !cooling(f, self)))
                .min_by(|&a, &b| {
                    let da = distance(self.qrf_team_pos(&teams[&a]).0, t.center);
                    let db = distance(self.qrf_team_pos(&teams[&b]).0, t.center);
                    da.total_cmp(&db)
                })
            else {
                break;
            };
            free.remove(&team);
            plan.push((team, ti));
        }

        // Just enough for each raid: a team keeps its members nearest the
        // raid up to [`JUST_ENOUGH`] times its power (one at least); the
        // rest split off, free for the next raid or the screen (user,
        // 2026-10-26: four Pyros chased one Flea, and the next Flea found
        // the whole force deciding where to go).
        if !self.dev_off("justenough") {
            for &(team, ti) in &plan {
                if team >= MOON || self.qrf.roles.contains_key(&team) {
                    continue;
                }
                let Some(ids) = teams.get(&team).cloned() else { continue };
                let (keep, rest) = self.qrf_mark_split(&ids, targets[ti].center, targets[ti].power);
                if rest.is_empty() {
                    continue;
                }
                let new = self.qrf.next_team;
                self.qrf.next_team += 1;
                for id in &rest {
                    if let Some(m) = self.qrf.members.get_mut(id) {
                        m.team = new;
                    }
                }
                self.qrf.mark.stats.splits += rest.len() as u32;
                teams.insert(team, keep.clone());
                all_teams.insert(team, keep);
                all_teams.insert(new, rest);
                free.insert(new);
            }
        }

        let mut marks = BTreeMap::new();
        for &(team, ti) in &plan {
            let tune = self.qrf_mark_team_tune(team, tune);
            let t = &targets[ti];
            // A moonlighter's assignment, with how late it is: the asset it
            // reaches with the most slack, its path time and the deadline
            // (for the read; no behaviour hangs on it).
            if team >= MOON {
                let tf = team_ids.iter().position(|&x| x == team).and_then(|i| fields[i].clone());
                let late = tf.as_ref().and_then(|tf| {
                    (t.threatened.iter())
                        .map(|&(i, tr)| (tf.at(self.qrf.assets[i].pos), deadline(t, i, tr, self)))
                        .map(|(path, dl)| (dl - path, path, dl))
                        .max_by(|a, b| a.0.total_cmp(&b.0))
                });
                let sid = (team - MOON) as SquadId;
                let line = match late {
                    Some((margin, path, dl)) => format!(
                        "qrf: draft squad {sid} at raid {:.0}: path {path:.0} s, deadline {dl:.0} s, margin {margin:+.0} s{}",
                        t.power,
                        if self.qrf.drafted.contains_key(&sid) { "" } else { " (new)" }
                    ),
                    None => format!("qrf: draft squad {sid} at raid {:.0}: no field", t.power),
                };
                self.log(&line);
            }
            marks.insert(team, tracks_of(t, self));
            let ids = &teams[&team];
            let (pos, _) = self.qrf_team_pos(ids);
            let power: f32 = plan.iter().filter(|p| p.1 == ti).map(|p| self.qrf_team_power(&teams[&p.0])).sum();
            let tf = match team_ids.iter().position(|&x| x == team) {
                Some(i) => fields[i].clone(),
                None => team_field(self, team, ids),
            };
            let mut spot = self.qrf_mark_spot(ids, t, tune, tf.as_deref());
            // Goalkeeping: stand on the asset the raid heads for, a little
            // toward it, if we get there first (our guns outrange raiders'
            // on a mex).
            let mut goal_kept = false;
            if tune.goal
                && let Some(tf) = team_ids.iter().position(|&x| x == team).and_then(|i| fields[i].as_ref())
                && let Some(g) = (t.groups.iter().map(|&g| &self.qrf.groups[g]))
                    .min_by(|a, b| distance(a.center, pos).total_cmp(&distance(b.center, pos)))
                && let Some((tr, goal)) = self.qrf_mark_goal(g, &t.threatened)
                && tf.at(goal) + MARGIN <= tr
            {
                let d = distance(goal, g.center).max(1.0);
                spot = goal + (g.center - goal) * (80.0f32.min(d) / d);
                goal_kept = true;
            }
            // Strike what comes close, if clearly stronger, and while it is
            // still near the mark (and near our value, if so tuned).
            let assets = &self.qrf.assets;
            let near = (t.groups.iter().map(|&g| &self.qrf.groups[g]))
                .filter(|g| distance(g.center, pos) < STRIKE_RANGE && distance(g.center, spot) < LEASH)
                .filter(|g| tune.commit <= 0.0 || assets.iter().any(|a| distance(a.pos, g.center) < tune.commit))
                .min_by(|a, b| distance(a.center, pos).total_cmp(&distance(b.center, pos)))
                .map(|g| g.center + g.vel * 1.0);
            let ti_team = team_ids.iter().position(|&x| x == team);
            let breached = ti_team.is_some_and(|i| breach[i][ti] > 0.0) && tune.breach;
            let near = near.or_else(|| {
                // A breach: go at the raid that is closest to our value.
                (t.groups.iter().map(|&g| &self.qrf.groups[g]))
                    .filter(|_| breached)
                    .min_by(|a, b| distance(a.center, pos).total_cmp(&distance(b.center, pos)))
                    .map(|g| g.center + g.vel * 1.5)
            });
            let strong = tune.strike > 0.0 && power >= tune.strike * t.power;
            // Stronger, and ahead of the raid on its way: meet it there.
            let near = near.or_else(|| {
                if !tune.intercept || !strong {
                    return None;
                }
                let tf = fields[ti_team?].as_ref()?;
                let g = (t.groups.iter().copied()).min_by(|&a, &b| {
                    let (a, b) = (&self.qrf.groups[a], &self.qrf.groups[b]);
                    distance(a.center, pos).total_cmp(&distance(b.center, pos))
                })?;
                let gf = group_fields[g].as_ref()?;
                self.qrf_mark_intercept(&self.qrf.groups[g], gf, tf, &t.threatened)
            });
            let raid_power = t.power;
            match near {
                Some(aim) if strong => {
                    self.qrf.mark.stats.strikes += 1;
                    self.qrf_order_team(ids, aim, true);
                    self.qrf_mark_doing(team, "strike", aim, raid_power);
                }
                _ => {
                    self.qrf.mark.stats.marks += 1;
                    self.qrf_order_team(ids, spot, tune.fight);
                    self.qrf_mark_doing(team, if goal_kept { "goal" } else { "mark" }, spot, raid_power);
                }
            }
        }
        self.qrf.mark.marks = marks;

        // Teams with a place of the operator's go there; moonlighters not
        // sent are their squads' again.
        let mut screens = BTreeSet::new();
        for team in free {
            let ids = &all_teams[&team];
            let (pos, _) = self.qrf_team_pos(ids);
            match self.qrf.roles.get(&team).copied() {
                Some(r) if r.kind != RoleKind::Auto => {
                    let what = match r.kind {
                        RoleKind::Hold => "hold",
                        _ => "post",
                    };
                    if distance(pos, r.pos) > 120.0 {
                        self.qrf_order_team(ids, r.pos, true);
                    }
                    self.qrf_mark_doing(team, what, r.pos, 0.0);
                }
                _ if team >= MOON => self.qrf_mark_doing(team, "free", pos, 0.0),
                _ => {
                    screens.insert(team);
                }
            }
        }
        let free = screens;

        // The rest screen.
        if frame - self.qrf.mark.sited > RESITE || self.qrf.mark.posts.len() != free.len() {
            self.qrf.mark.posts = if tune.value {
                self.qrf_mark_value_posts(free.len(), tune.ahead)
            } else {
                self.qrf_mark_posts(free.len(), tune.ahead)
            };
            self.qrf.mark.sited = frame;
        }
        let mut posts = self.qrf.mark.posts.clone();
        for team in free {
            let ids = &all_teams[&team];
            let (pos, _) = self.qrf_team_pos(ids);
            let Some(k) = (0..posts.len()).min_by(|&a, &b| distance(posts[a], pos).total_cmp(&distance(posts[b], pos)))
            else {
                break;
            };
            let p = posts.remove(k);
            self.qrf.mark.stats.screens += 1;
            if distance(pos, p) > 150.0 {
                self.qrf_order_team(ids, p, true);
            }
            self.qrf_mark_doing(team, "screen", p, 0.0);
        }
        if frame % (30 * 30) < super::PERIOD {
            let s = &self.qrf.mark.stats;
            let line = format!(
                "qrfmark: t={} marks={} strikes={} splits={} screens={} entries={} posts={}",
                frame / 30,
                s.marks,
                s.strikes,
                s.splits,
                s.screens,
                self.qrf.mark.entries.len(),
                self.qrf.mark.posts.iter().map(|p| format!("{:.0},{:.0}", p.x, p.z)).collect::<Vec<_>>().join(" ")
            );
            self.log(&line);
        }
    }

    /// A team's members split for a raid of `power` at `at`: those to keep
    /// (nearest first, until [`JUST_ENOUGH`] times the raid's power, one at
    /// least) and the rest.
    fn qrf_mark_split(&self, ids: &[i32], at: Vec3, power: f32) -> (Vec<i32>, Vec<i32>) {
        let mut by: Vec<(f32, f32, i32)> = (ids.iter())
            .filter_map(|id| self.qrf.members.get(id).map(|m| (distance(m.pos(), at), m.power(), *id)))
            .collect();
        by.sort_by(|a, b| a.0.total_cmp(&b.0));
        let need = JUST_ENOUGH * power;
        let (mut keep, mut rest, mut have) = (Vec::new(), Vec::new(), 0.0);
        for (_, p, id) in by {
            if keep.is_empty() || have < need {
                keep.push(id);
                have += p;
            } else {
                rest.push(id);
            }
        }
        (keep, rest)
    }

    /// Where a team marks a target from: see the module doc.
    fn qrf_mark_spot(&self, ids: &[i32], t: &Target, tune: Tune, field: Option<&Field>) -> Vec3 {
        let (standoff, latew) = (tune.standoff, tune.latew);
        let live = self.qrf.live;
        let (pos, speed) = self.qrf_team_pos(ids);
        let Some(grid) = self.qrf.grid.as_ref() else { return pos };
        let Some(mobility) = ids.iter().find_map(|id| self.qrf.members.get(id)).map(|m| m.mobility) else {
            return pos;
        };
        let speed = speed.max(1.0);
        let value: f32 = t.threatened.iter().map(|&(i, _)| self.qrf.assets[i].cost).sum::<f32>().max(1.0);
        let raids: Vec<Vec3> = t.groups.iter().map(|&g| self.qrf.groups[g].center).collect();
        let mut best = (f32::INFINITY, pos);
        let n = (REACH / STEP) as i32;
        for dz in -n..=n {
            for dx in -n..=n {
                let x = Vec3::xz(t.center.x + dx as f32 * STEP, t.center.z + dz as f32 * STEP);
                if distance(x, t.center) > REACH || x.x < 0.0 || x.z < 0.0 || !grid.passable(x, mobility) {
                    continue;
                }
                // In normal play, out of reach of enemy defenses.
                if live && self.war.get_porc_threat(x) > 0.0 {
                    continue;
                }
                let transit = field.map_or(distance(pos, x) / speed, |f| f.at(x));
                if transit == NEVER {
                    continue;
                }
                let late: f32 = (t.threatened.iter())
                    .map(|&(i, tr)| {
                        let a = &self.qrf.assets[i];
                        let mine = distance(x, a.pos) * DETOUR / speed + MARGIN;
                        a.cost * (mine - tr).max(0.0)
                    })
                    .sum::<f32>()
                    / value;
                let gap = raids.iter().map(|&r| distance(r, x)).fold(f32::INFINITY, f32::min);
                let score = latew * late + TRANSIT_COST * transit + tune.standw * (gap - standoff).abs();
                if score < best.0 {
                    best = (score, x);
                }
            }
        }
        best.1
    }

    /// The asset a raid seems to head for, by its heading and how soon it
    /// gets there: its raid time and place.
    fn qrf_mark_goal(&self, g: &super::RaidGroup, threatened: &[(usize, f32)]) -> Option<(f32, Vec3)> {
        let speed = g.vel.length_2d();
        (threatened.iter())
            .map(|&(i, tr)| {
                let a = self.qrf.assets[i].pos;
                let d = distance(a, g.center).max(1.0);
                let cos = if speed > 20.0 {
                    ((a.x - g.center.x) * g.vel.x + (a.z - g.center.z) * g.vel.z) / (d * speed)
                } else {
                    1.0
                };
                (tr * (1.6 - cos), tr, a)
            })
            .min_by(|a, b| a.0.total_cmp(&b.0))
            .map(|(_, tr, a)| (tr, a))
    }

    /// Where a team gets ahead of a raid on its way to the asset it seems
    /// to head for (by its heading and how soon it gets there): the first
    /// place on that way, from the raid, that the team reaches first.
    fn qrf_mark_intercept(
        &self,
        g: &super::RaidGroup,
        gf: &Field,
        tf: &Field,
        threatened: &[(usize, f32)],
    ) -> Option<Vec3> {
        let grid = self.qrf.grid.as_ref()?;
        let goal = self.qrf_mark_goal(g, threatened)?.1;
        let path = gf.descent(goal, 400);
        path.iter().rev().find(|&&c| tf.cell(c) + MARGIN <= gf.cell(c)).map(|&c| grid.centre(c))
    }

    /// Remember where raids show up first.
    fn qrf_mark_entries(&mut self) {
        let fresh: Vec<(i32, Vec3)> = (self.qrf.tracks.iter())
            .filter(|(id, _)| !self.qrf.mark.seen.contains(id))
            .map(|(&id, t)| (id, t.pos))
            .collect();
        for (id, p) in fresh {
            self.qrf.mark.seen.insert(id);
            // In normal play, only where raids come into our value (not
            // where scouts saw the enemy at home).
            if self.qrf.live
                && (self.graph.is_enemy_territory(p) || self.qrf.assets.iter().all(|a| distance(a.pos, p) > 2500.0))
            {
                continue;
            }
            // One entry per raid: not next to one just recorded.
            if self.qrf.mark.entries.iter().rev().take(4).any(|&e| distance(e, p) < 500.0) {
                continue;
            }
            self.qrf.mark.entries.push(p);
        }
        let n = self.qrf.mark.entries.len();
        if n > ENTRIES {
            self.qrf.mark.entries.drain(..n - ENTRIES);
        }
    }

    /// Screen posts for `k` teams: ahead of the first asset raids from each
    /// entry would reach; before any raid, toward the enemy's side.
    fn qrf_mark_posts(&self, k: usize, ahead: f32) -> Vec<Vec3> {
        if k == 0 || self.qrf.assets.is_empty() {
            return Vec::new();
        }
        let mut entries: Vec<(Vec3, f32)> =
            self.qrf.mark.entries.iter().enumerate().map(|(i, &e)| (e, 1.0 + i as f32 / ENTRIES as f32)).collect();
        if entries.len() < 4 {
            // Prior: the assets nearest the enemy, as if raids came
            // straight from there.
            let map = self.cb.map();
            let (w, h) = (map.width() as f32 * 8.0, map.height() as f32 * 8.0);
            let n = self.qrf.assets.len() as f32;
            let c = self.qrf.assets.iter().fold(Vec3::ZERO, |s, a| s + a.pos * (1.0 / n));
            let enemy = match self.graph.get_enemy_center() {
                e if e != crate::util::NULLPOS && distance(e, c) > 1500.0 => e,
                _ => Vec3::xz(w - c.x, h - c.z),
            };
            let mut near: Vec<Vec3> = self.qrf.assets.iter().map(|a| a.pos).collect();
            near.sort_by(|a, b| distance(*a, enemy).total_cmp(&distance(*b, enemy)));
            for a in near.iter().take((near.len() / 3).max(k)) {
                let d = distance(*a, enemy).max(1.0);
                let e = *a + (enemy - *a) * (1500.0 / d);
                entries.push((e, 0.3));
            }
        }
        let gates: Vec<(Vec3, f32)> = (entries.iter())
            .filter_map(|&(e, w)| {
                let gate = self.qrf.assets.iter().min_by(|a, b| distance(a.pos, e).total_cmp(&distance(b.pos, e)))?;
                let d = distance(gate.pos, e).max(1.0);
                Some((gate.pos + (e - gate.pos) * (ahead.min(d * 0.5) / d), w))
            })
            .filter(|(p, _)| !self.qrf.live || self.war.get_porc_threat(*p) <= 0.0)
            .collect();
        k_means(&gates, k)
    }

    /// Where raids come from: the entries seen, else the enemy's side.
    pub(super) fn qrf_mark_sources(&self) -> Vec<Vec3> {
        if self.qrf.mark.entries.len() >= 4 {
            return self.qrf.mark.entries.clone();
        }
        let map = self.cb.map();
        let (w, h) = (map.width() as f32 * 8.0, map.height() as f32 * 8.0);
        let n = self.qrf.assets.len().max(1) as f32;
        let c = self.qrf.assets.iter().fold(Vec3::ZERO, |s, a| s + a.pos * (1.0 / n));
        let enemy = match self.graph.get_enemy_center() {
            e if e != crate::util::NULLPOS && distance(e, c) > 1500.0 => e,
            _ => Vec3::xz(w - c.x, h - c.z),
        };
        let mut v = self.qrf.mark.entries.clone();
        v.push(enemy);
        v
    }

    /// Screen posts for `k` teams over our value: our assets clustered by
    /// cost and exposure (how near the raids' entries, or the enemy's side,
    /// they are), each cluster's post a little ahead toward the nearest
    /// entry. Covers the mexes where they are, off the road raids take,
    /// instead of the gates alone.
    fn qrf_mark_value_posts(&self, k: usize, ahead: f32) -> Vec<Vec3> {
        if k == 0 || self.qrf.assets.is_empty() {
            return Vec::new();
        }
        let sources = self.qrf_mark_sources();
        let nearest_source =
            |p: Vec3| (sources.iter().copied()).min_by(|a, b| distance(*a, p).total_cmp(&distance(*b, p))).unwrap_or(p);
        let points: Vec<(Vec3, f32)> = (self.qrf.assets.iter())
            .filter(|a| !self.qrf.live || self.war.get_porc_threat(a.pos) <= 0.0)
            .map(|a| {
                let d = distance(nearest_source(a.pos), a.pos);
                (a.pos, a.cost / (1.0 + d / 1500.0))
            })
            .collect();
        (k_means(&points, k).into_iter())
            .map(|c| {
                let e = nearest_source(c);
                let d = distance(e, c).max(1.0);
                c + (e - c) * (ahead.min(d * 0.5) / d)
            })
            .collect()
    }

    /// A team down to one member joins the nearest other team.
    fn qrf_mark_merge_teams(&mut self) {
        let teams = self.qrf_teams();
        if teams.len() < 2 {
            return;
        }
        // Not while raids are about: a team split down to just enough for a
        // raid (see `qrf_mark_split`) would merge straight back.
        if !self.dev_off("justenough") && !self.qrf.groups.is_empty() {
            return;
        }
        // In normal play only into a team with room: recruits come one by
        // one to make up new teams.
        let own = |t: usize| t < MOON && !self.qrf.roles.contains_key(&t);
        let room = |n: usize| !self.qrf.live || n < self.qrf.team_size;
        for (&t, ids) in &teams {
            if ids.len() != 1 || !own(t) {
                continue;
            }
            let (p, _) = self.qrf_team_pos(ids);
            let other = (teams.iter())
                .filter(|(o, v)| **o != t && v.len() > 1 && own(**o) && room(v.len()))
                .min_by(|a, b| distance(self.qrf_team_pos(a.1).0, p).total_cmp(&distance(self.qrf_team_pos(b.1).0, p)))
                .map(|(&o, _)| o);
            if let Some(o) = other
                && let Some(m) = self.qrf.members.get_mut(&ids[0])
            {
                m.team = o;
                return;
            }
        }
    }
}
