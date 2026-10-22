//! The `cover` QRF algorithm: worst-case time coverage, no guessing of
//! intent (the dynamic ambulance-redeployment problem).
//!
//! A raid is first seen where it comes onto our radar (the *entries*: the
//! edge of our radar coverage on the enemy's side). An asset is *covered*
//! when some team gets to it at least a margin sooner than the fastest
//! raider gets there from the nearest entry. Free teams stand at the
//! stations that cover the most value (greedy, then local search over
//! candidate cells, on travel times over the ground), which puts them
//! forward, between the radar's edge and our value. With `cover_q` > 0 the
//! coverage is *expected* coverage (Daskin): each team is busy elsewhere
//! with that chance, so value covered by two teams counts for more.
//!
//! Raids seen are answered by value saved: for every way of sending teams
//! at raids (or not), the value of the assets each raid can reach soon
//! that the teams sent get to first, weighted by how soon. One team goes
//! to a raid, more only while it is outgunned. A team sent to a raid goes
//! where it gets ahead of the raid on its way to the most urgent asset it
//! can still save (the farthest point along that way it reaches first),
//! not at the raid, which runs from it. It fights the raid only once the
//! raid commits (about to reach an asset, or on top of the team): a raid
//! kept away but alive comes back no sooner, a raid killed is replaced. It
//! goes at a raid where it is (busy killing something) only if it can get
//! ahead of it nowhere. The teams left over re-take the stations that
//! cover most with the teams they are (re-solved whenever teams are sent
//! or come back, moving only for a real gain); weak teams merge.
//!
//! Tunables as AI options (see [`Params`]), so arms of one build differ.

use std::collections::BTreeMap;

use springai::Vec3;

use super::LOG_PERIOD;
use super::field::{Field, Mobility, NEVER};
use crate::Ai;
use crate::util::{NULLPOS, distance};

/// Frames between re-computing the assets' fields and the entries.
const RESITE: i32 = 30 * 20;

/// Frames between re-solving the free teams' stations (also whenever the
/// free teams change).
const RESTATION: i32 = 60;

/// Seconds a team sent at a raid must beat it to an asset by.
const DISPATCH_MARGIN: f32 = 1.0;

/// Seconds over which coverage fades from none to full.
const SOFT: f32 = 6.0;

/// A raid threatens the assets it reaches within this many seconds; the
/// sooner, the more (an asset `DECAY` seconds off counts 1/e).
const HORIZON: f32 = 50.0;
const DECAY: f32 = 25.0;

/// A team goes at a raid this close (elmos), if strong enough: its power
/// at least this share of the raid's.
const ENGAGE: f32 = 450.0;
const POWER_RATIO: f32 = 0.8;

/// A raid this close to a team, or this many seconds from an asset, has
/// committed: it is fought.
const ON_US: f32 = 260.0;
const COMMIT: f32 = 5.0;

/// Free teams move to new stations only if the coverage gains this share.
const MOVE_GAIN: f32 = 0.03;

/// A mex is worth its cost this many times (income).
const MEX_WEIGHT: f32 = 3.0;

/// Fastest raider assumed until faster ones are seen, elmos per second.
const RAIDER_SPEED: f32 = 115.0;

/// Candidate stations every this many cells, and within this many seconds
/// of some asset.
const CAND_STEP: usize = 2;
const CAND_REACH: f32 = 40.0;

/// A team weaker than this share of the strongest it was merges into the
/// nearest other.
const MERGE_SHARE: f32 = 0.4;

/// Assignments searched exhaustively up to this many combinations.
const MAX_COMBOS: usize = 4096;

/// A team close to its station stays put, elmos.
const AT_STATION: f32 = 120.0;

/// A healing member rejoins at this share of its health.
const HEALED: f32 = 0.8;

/// Healing members keep this far from raids, elmos.
const HEAL_CLEAR: f32 = 900.0;

/// Tunables, from AI options.
#[derive(Clone, Copy)]
pub struct Params {
    /// `cover_q`: the chance a team is busy, for expected coverage (0:
    /// plain maximal coverage).
    pub q: f32,
    /// `cover_margin`: seconds a station's team must beat the fastest
    /// raider to an asset by.
    pub margin: f32,
    /// `cover_fight` (0/1): fight raids whenever close (1), or only once
    /// they commit (0, default).
    pub fight: bool,
    /// `cover_keep`: how much a team prefers the raid it is on.
    pub keep: f32,
    /// `cover_multi` (0/1): send every team that adds value (1), or more
    /// than one only while outgunned (0, default).
    pub multi: bool,
    /// `cover_stations` (0/1): free teams take stations (1, default), or
    /// wait where they are (0).
    pub stations: bool,
    /// `cover_heal`: members below this share of their health leave their
    /// team to heal (ZK units heal when unhurt for a while), back at
    /// [`HEALED`]; 0: never.
    pub heal: f32,
    /// `cover_tau`: seconds of exposure weighting (see [`covered`]; 0:
    /// every asset by its value alone).
    pub tau: f32,
    /// `cover_deficit`: weight of coming too late (see [`covered`]).
    pub deficit: f32,
}

impl Default for Params {
    fn default() -> Self {
        Params {
            q: 0.0,
            margin: 3.0,
            fight: false,
            keep: 1.5,
            multi: false,
            stations: true,
            heal: 0.0,
            tau: 0.0,
            deficit: 0.0,
        }
    }
}

struct Target {
    pos: Vec3,
    value: f32,
    /// Seconds from every cell to it, for our force.
    field: Field,
}

#[derive(Default)]
pub struct Cover {
    params: Option<Params>,
    targets: Vec<Target>,
    /// Seconds for the fastest raider from the nearest entry to every cell.
    entry: Option<Field>,
    entries: usize,
    candidates: Vec<usize>,
    stations: BTreeMap<usize, Vec3>,
    sited: i32,
    sited_assets: usize,
    stationed: i32,
    stationed_free: Vec<usize>,
    raider_speed: f32,
    /// Per team: the raid it is on (its members), to stay on it.
    on: BTreeMap<usize, Vec<i32>>,
    /// Per team: the most power it had.
    full: BTreeMap<usize, f32>,
    coverage: f32,
    /// Members out healing.
    healing: std::collections::BTreeSet<i32>,
    stats: Stats,
}

#[derive(Default)]
struct Stats {
    intercepts: u32,
    chases: u32,
    engages: u32,
    restations: u32,
    merges: u32,
    heals: u32,
}

/// A raid seen: its group's index, its field, and the assets it reaches
/// within the horizon with the seconds it needs, soonest first.
type Raid = (usize, Field, Vec<(usize, f32)>);

/// Coverage of one asset at `slack` seconds.
fn cov(slack: f32) -> f32 {
    ((slack + SOFT / 2.0) / SOFT).clamp(0.0, 1.0)
}

/// Covered value of assets from stations at `cells`: value times the
/// exposure (with `tau` > 0, an asset the raids reach `tau` seconds sooner
/// counts e times more: they hit the exposed first), less `deficit` times
/// the exposed value per [`SOFT`] seconds the nearest team comes too late.
fn covered(targets: &[Target], entry: &Field, cells: &[usize], p: Params) -> f32 {
    targets
        .iter()
        .map(|t| {
            let raid = entry.at(t.pos);
            if !raid.is_finite() {
                return 0.0;
            }
            let w = if p.tau > 0.0 { t.value * (-raid / p.tau).exp() } else { t.value };
            let b = cells.iter().map(|&s| t.field.cell(s)).fold(NEVER, f32::min);
            let late = (b + p.margin - raid).clamp(0.0, 60.0) / SOFT;
            let got = if p.q <= 0.0 {
                cov(raid - b - p.margin)
            } else {
                let open: f32 =
                    (cells.iter()).map(|&s| 1.0 - (1.0 - p.q) * cov(raid - t.field.cell(s) - p.margin)).product();
                1.0 - open
            };
            w * (got - p.deficit * late)
        })
        .sum()
}

/// The most [`covered`] can be: every exposed asset fully covered.
fn coverable(targets: &[Target], entry: &Field, p: Params) -> f32 {
    (targets.iter())
        .map(|t| {
            let raid = entry.at(t.pos);
            if !raid.is_finite() {
                0.0
            } else if p.tau > 0.0 {
                t.value * (-raid / p.tau).exp()
            } else {
                t.value
            }
        })
        .sum::<f32>()
        .max(1e-3)
}

impl Ai {
    fn qrf_cover_params(&self) -> Params {
        let d = Params::default();
        let f = |k: &str, v: f32| self.cb.option(k).and_then(|s| s.parse().ok()).unwrap_or(v);
        Params {
            q: f("cover_q", d.q),
            margin: f("cover_margin", d.margin),
            fight: f("cover_fight", 0.0) > 0.5,
            keep: f("cover_keep", d.keep),
            multi: f("cover_multi", 0.0) > 0.5,
            stations: f("cover_stations", 1.0) > 0.5,
            heal: f("cover_heal", d.heal),
            tau: f("cover_tau", d.tau),
            deficit: f("cover_deficit", d.deficit),
        }
    }

    pub(super) fn qrf_cover_update(&mut self, frame: i32) {
        if self.qrf.cover.params.is_none() {
            let p = self.qrf_cover_params();
            self.qrf.cover.params = Some(p);
            let line = format!(
                "qrf cover: q={} margin={} fight={} keep={} multi={} stations={} heal={} tau={} deficit={}",
                p.q, p.margin, p.fight, p.keep, p.multi, p.stations, p.heal, p.tau, p.deficit
            );
            self.log(&line);
        }
        let p = self.qrf.cover.params.unwrap_or_default();
        self.qrf_cover_merge();
        let mut teams = self.qrf_teams();
        if p.heal > 0.0 {
            self.qrf_cover_heal(p.heal);
            let healing = &self.qrf.cover.healing;
            for ids in teams.values_mut() {
                ids.retain(|id| !healing.contains(id));
            }
            teams.retain(|_, ids| !ids.is_empty());
        }
        if teams.is_empty() {
            return;
        }
        if self.qrf.cover.raider_speed <= 0.0 {
            self.qrf.cover.raider_speed = RAIDER_SPEED;
        }
        let seen = self.qrf.groups.iter().map(|g| g.speed).fold(0.0, f32::max).min(200.0);
        let faster = seen > self.qrf.cover.raider_speed + 5.0;
        if faster {
            self.qrf.cover.raider_speed = seen;
        }
        let c = &self.qrf.cover;
        if faster || c.targets.is_empty() || frame - c.sited > RESITE || c.sited_assets != self.qrf.assets.len() {
            self.qrf_cover_site(frame);
        }
        if self.qrf.cover.entry.is_none() {
            return;
        }

        // Teams: where they get, how strong.
        let fields: BTreeMap<usize, Field> =
            teams.iter().filter_map(|(&t, ids)| self.qrf_team_field(ids).map(|f| (t, f))).collect();
        let power: BTreeMap<usize, f32> = teams.iter().map(|(&t, ids)| (t, self.qrf_team_power(ids))).collect();
        for (&t, &pw) in &power {
            let f = self.qrf.cover.full.entry(t).or_insert(0.0);
            *f = f.max(pw);
        }

        // Raids: what each threatens, how soon.
        let mut raids: Vec<Raid> = Vec::new();
        for (gi, g) in self.qrf.groups.iter().enumerate() {
            let Some(f) = self.qrf_group_field(g) else { continue };
            let mut threatened: Vec<(usize, f32)> = (self.qrf.cover.targets.iter().enumerate())
                .map(|(i, t)| (i, f.at(t.pos)))
                .filter(|&(_, s)| s <= HORIZON)
                .collect();
            if threatened.is_empty() {
                continue;
            }
            threatened.sort_by(|a, b| a.1.total_cmp(&b.1));
            raids.push((gi, f, threatened));
        }

        // The best way of sending teams at raids.
        let team_ids: Vec<usize> = teams.keys().copied().collect();
        let k = team_ids.len();
        let r = raids.len();
        let mut best: (f32, Vec<Option<usize>>) = (0.0, vec![None; k]);
        let combos = (r + 1).checked_pow(k as u32).unwrap_or(usize::MAX);
        if r > 0 {
            let score = |choice: &[Option<usize>]| -> f32 {
                let mut total = 0.0;
                for (ri, (gi, _, threatened)) in raids.iter().enumerate() {
                    let mut sent: Vec<usize> = (0..k).filter(|&i| choice[i] == Some(ri)).collect();
                    if sent.is_empty() {
                        continue;
                    }
                    let g = &self.qrf.groups[*gi];
                    let pw: f32 = sent.iter().map(|&i| power[&team_ids[i]]).sum();
                    // More than one team only while the others are
                    // outgunned without it.
                    if !p.multi && sent.len() > 1 {
                        sent.sort_by(|&a, &b| power[&team_ids[b]].total_cmp(&power[&team_ids[a]]));
                        let without: f32 = sent[..sent.len() - 1].iter().map(|&i| power[&team_ids[i]]).sum();
                        if without >= POWER_RATIO * g.power {
                            return -1.0;
                        }
                    }
                    let strong = if pw >= POWER_RATIO * g.power { 1.0 } else { 0.3 };
                    let mut saved = 0.0;
                    for &(ai, s) in threatened {
                        let t = &self.qrf.cover.targets[ai];
                        let us = (sent.iter().filter_map(|&i| fields.get(&team_ids[i])))
                            .map(|f| f.at(t.pos))
                            .fold(NEVER, f32::min);
                        if us + DISPATCH_MARGIN <= s {
                            saved += t.value * (-s / DECAY).exp();
                        }
                    }
                    // Going at it where it is saves some of what lies
                    // beyond it.
                    if saved == 0.0 {
                        saved = (threatened.iter().skip(1))
                            .map(|&(ai, s)| self.qrf.cover.targets[ai].value * (-s / DECAY).exp())
                            .sum::<f32>()
                            * 0.3;
                    }
                    let mut v = saved * strong;
                    // Staying on a raid already taken.
                    for &i in &sent {
                        if let Some(prev) = self.qrf.cover.on.get(&team_ids[i])
                            && g.members.iter().any(|m| prev.contains(m))
                        {
                            v *= p.keep;
                        }
                    }
                    total += v / (1.0 + 0.5 * (sent.len() as f32 - 1.0));
                }
                total
            };
            if combos <= MAX_COMBOS {
                let mut choice = vec![None; k];
                for code in 0..combos {
                    let mut c = code;
                    for slot in choice.iter_mut() {
                        let d = c % (r + 1);
                        *slot = (d < r).then_some(d);
                        c /= r + 1;
                    }
                    let s = score(&choice);
                    if s > best.0 {
                        best = (s, choice.clone());
                    }
                }
            } else {
                // Greedy: each team to the raid it adds most to.
                let mut choice = vec![None; k];
                for i in 0..k {
                    let base = score(&choice);
                    let mut top = (0.0, None);
                    for ri in 0..r {
                        choice[i] = Some(ri);
                        let gain = score(&choice) - base;
                        if gain > top.0 {
                            top = (gain, Some(ri));
                        }
                    }
                    choice[i] = top.1;
                }
                best = (score(&choice), choice);
            }
        }

        // Orders to the teams sent.
        let tracing = self.qrf.trace > 0 && frame % (self.qrf.trace * 30) < super::PERIOD;
        let mut on = BTreeMap::new();
        for (i, &t) in team_ids.iter().enumerate() {
            let Some(ri) = best.1[i] else { continue };
            let (gi, gfield, threatened) = &raids[ri];
            let g = &self.qrf.groups[*gi];
            on.insert(t, g.members.clone());
            let ids = &teams[&t];
            let (pos, _) = self.qrf_team_pos(ids);
            let sent_power: f32 = (0..k).filter(|&j| best.1[j] == Some(ri)).map(|j| power[&team_ids[j]]).sum();
            let (center, vel) = (g.center, g.vel);
            let first = threatened.first().map_or(NEVER, |x| x.1);
            let d = distance(pos, center);
            let committed = p.fight || d < ON_US || first < COMMIT;
            if d < ENGAGE && committed && sent_power >= POWER_RATIO * g.power {
                self.qrf.cover.stats.engages += 1;
                if tracing {
                    self.log(&format!("qrf cover: team {t} fights raid@{:.0},{:.0}", center.x, center.z));
                }
                self.qrf_order_team(ids, center + vel * 1.0, true);
                continue;
            }
            let Some(tf) = fields.get(&t) else { continue };
            let mut aim = None;
            for &(ai, s) in threatened {
                let tp = self.qrf.cover.targets[ai].pos;
                if tf.at(tp) + DISPATCH_MARGIN > s {
                    continue;
                }
                // Along the raid's way to it, from it toward the raid: the
                // farthest point we get to first.
                let mut at = tp;
                if let Some(grid) = self.qrf.grid.as_ref() {
                    for c in gfield.descent(tp, 400) {
                        if tf.cell(c) + DISPATCH_MARGIN > gfield.cell(c) {
                            break;
                        }
                        at = grid.centre(c);
                    }
                }
                aim = Some(at);
                break;
            }
            if tracing {
                let a = aim.unwrap_or(center);
                let line = format!(
                    "qrf cover: team {t}@{:.0},{:.0} on raid@{:.0},{:.0} ({} threatened, first {:.0} s) {} {:.0},{:.0}",
                    pos.x,
                    pos.z,
                    center.x,
                    center.z,
                    threatened.len(),
                    first,
                    if aim.is_some() { "ahead at" } else { "chasing" },
                    a.x,
                    a.z
                );
                self.log(&line);
            }
            match aim {
                Some(at) => {
                    self.qrf.cover.stats.intercepts += 1;
                    // Fighting on the way drags it after the raid.
                    self.qrf_order_team(ids, at, p.fight);
                }
                None => {
                    self.qrf.cover.stats.chases += 1;
                    self.qrf_order_team(ids, center + vel * 2.0, true);
                }
            }
        }
        self.qrf.cover.on = on;

        // The rest take the stations that cover most.
        let free: Vec<usize> = (0..k).filter(|&i| best.1[i].is_none()).map(|i| team_ids[i]).collect();
        if !free.is_empty() && p.stations {
            let c = &self.qrf.cover;
            if c.stationed_free != free
                || frame - c.stationed >= RESTATION
                || free.iter().any(|t| !c.stations.contains_key(t))
            {
                self.qrf_cover_station(&teams, &free, p);
                self.qrf.cover.stationed = frame;
                self.qrf.cover.stationed_free = free.clone();
            }
            for t in &free {
                if let Some(&s) = self.qrf.cover.stations.get(t) {
                    let ids = &teams[t];
                    if distance(self.qrf_team_pos(ids).0, s) > AT_STATION {
                        self.qrf_order_team(ids, s, false);
                    }
                }
            }
        }

        if frame % LOG_PERIOD < super::PERIOD {
            let c = &self.qrf.cover;
            let st: Vec<String> = c.stations.iter().map(|(t, p)| format!("{t}@{:.0},{:.0}", p.x, p.z)).collect();
            let line = format!(
                "qrf cover: coverage={:.2} entries={} raider_speed={:.0} stations=[{}] intercepts={} chases={} engages={} restations={} merges={} heals={} healing={}",
                c.coverage,
                c.entries,
                c.raider_speed,
                st.join(" "),
                c.stats.intercepts,
                c.stats.chases,
                c.stats.engages,
                c.stats.restations,
                c.stats.merges,
                c.stats.heals,
                c.healing.len()
            );
            self.log(&line);
        }
    }

    /// Badly hurt members leave their teams to heal, away from raids, and
    /// come back healed.
    fn qrf_cover_heal(&mut self, low: f32) {
        let mut orders = Vec::new();
        let total: f32 = self.qrf.assets.iter().map(|a| a.cost).sum::<f32>().max(1.0);
        let centre = self.qrf.assets.iter().fold(Vec3::ZERO, |acc, a| acc + a.pos * (a.cost / total));
        for (&id, m) in &self.qrf.members {
            let share = m.unit.health() / m.unit.max_health().max(1.0);
            let healing = self.qrf.cover.healing.contains(&id);
            if !healing && share >= low || healing && share >= HEALED {
                if healing {
                    orders.push((id, None));
                }
                continue;
            }
            // Away from the nearest raid, toward our value.
            let pos = m.pos();
            let raid = (self.qrf.groups.iter())
                .map(|g| g.center)
                .min_by(|a, b| distance(*a, pos).total_cmp(&distance(*b, pos)));
            let to = match raid {
                Some(r) if distance(r, pos) < HEAL_CLEAR => {
                    let away = pos - r;
                    let n = away.length_2d().max(1.0);
                    let flee = pos + away * (400.0 / n);
                    if distance(flee, centre) < distance(r, centre) { flee } else { centre }
                }
                _ => centre,
            };
            orders.push((id, Some(to)));
        }
        for (id, to) in orders {
            match to {
                Some(to) => {
                    if self.qrf.cover.healing.insert(id) {
                        self.qrf.cover.stats.heals += 1;
                    }
                    self.qrf_order(id, to, false);
                }
                None => {
                    self.qrf.cover.healing.remove(&id);
                }
            }
        }
    }

    /// Weak teams join the nearest other.
    fn qrf_cover_merge(&mut self) {
        let teams = self.qrf_teams();
        if teams.len() < 2 {
            return;
        }
        for (&t, ids) in &teams {
            let full = self.qrf.cover.full.get(&t).copied().unwrap_or(0.0);
            if full <= 0.0 || self.qrf_team_power(ids) >= MERGE_SHARE * full {
                continue;
            }
            let (p, _) = self.qrf_team_pos(ids);
            let other = (teams.iter())
                .filter(|(o, _)| **o != t)
                .min_by(|a, b| distance(self.qrf_team_pos(a.1).0, p).total_cmp(&distance(self.qrf_team_pos(b.1).0, p)))
                .map(|(o, _)| *o);
            let Some(o) = other else { continue };
            for id in ids {
                if let Some(m) = self.qrf.members.get_mut(id) {
                    m.team = o;
                }
            }
            self.qrf.cover.full.remove(&t);
            self.qrf.cover.stations.remove(&t);
            self.qrf.cover.stats.merges += 1;
            self.log(&format!("qrf cover: team {t} ({} left) joins team {o}", ids.len()));
            // One merge per update: the teams changed.
            return;
        }
    }

    /// Re-compute the assets' fields, the entries and the candidates.
    fn qrf_cover_site(&mut self, frame: i32) {
        let c = &mut self.qrf.cover;
        c.sited = frame;
        c.sited_assets = self.qrf.assets.len();
        let Some(grid) = self.qrf.grid.as_ref() else { return };
        let Some(m) = self.qrf.members.values().next() else { return };
        let (mobility, speed) = (m.mobility, self.qrf.members.values().map(|m| m.speed).fold(f32::INFINITY, f32::min));
        let targets: Vec<Target> = (self.qrf.assets.iter())
            .map(|a| Target {
                pos: a.pos,
                value: a.cost * if a.is_mex { MEX_WEIGHT } else { 1.0 },
                field: grid.field(mobility, speed, &[(a.pos, 0.0)]),
            })
            .collect();
        // Our side and theirs.
        let total: f32 = targets.iter().map(|t| t.value).sum::<f32>().max(1.0);
        let base = targets.iter().fold(Vec3::ZERO, |acc, t| acc + t.pos * (t.value / total));
        let map = self.cb.map();
        let (w, h) = (map.width() as f32 * 8.0, map.height() as f32 * 8.0);
        let enemy = match self.graph.get_enemy_center() {
            e if e != NULLPOS && distance(e, base) > 1000.0 => e,
            _ => Vec3::xz(w - base.x, h - base.z),
        };
        // Entries: radar-covered cells next to uncovered ground that the
        // enemy's side reaches without coming onto our radar (raids come
        // round the edge of our coverage, not just across the middle).
        let n = grid.w * grid.h;
        let centres: Vec<Vec3> = (0..n).map(|i| grid.centre(i)).collect();
        let radar = self.los.in_radar(&centres);
        let raider = Mobility::BOT;
        let theirs = |p: Vec3| distance(p, enemy) < distance(p, base);
        let passable: Vec<bool> = centres.iter().map(|&p| grid.passable(p, raider)).collect();
        let mut outside = vec![false; n];
        let mut stack: Vec<usize> = (0..n).filter(|&i| !radar[i] && passable[i] && theirs(centres[i])).collect();
        for &i in &stack {
            outside[i] = true;
        }
        let neighbours = |i: usize| {
            let (x, z) = ((i % grid.w) as i64, (i / grid.w) as i64);
            [(1i64, 0i64), (-1, 0), (0, 1), (0, -1)].into_iter().filter_map(move |(dx, dz)| {
                let (nx, nz) = (x + dx, z + dz);
                (nx >= 0 && nz >= 0 && nx < grid.w as i64 && nz < grid.h as i64)
                    .then(|| (nz * grid.w as i64 + nx) as usize)
            })
        };
        while let Some(i) = stack.pop() {
            for j in neighbours(i) {
                if !outside[j] && !radar[j] && passable[j] {
                    outside[j] = true;
                    stack.push(j);
                }
            }
        }
        let mut entries: Vec<(Vec3, f32)> = (0..n)
            .filter(|&i| radar[i] && passable[i] && neighbours(i).any(|j| outside[j]))
            .map(|i| (centres[i], 0.0))
            .collect();
        // Little radar: a ring around our assets, their side first.
        if entries.len() < 4 {
            let ring = |side: bool| -> Vec<(Vec3, f32)> {
                (0..n)
                    .filter(|&i| {
                        let d = targets.iter().map(|t| distance(t.pos, centres[i])).fold(f32::INFINITY, f32::min);
                        (1000.0..1100.0).contains(&d) && passable[i] && (!side || theirs(centres[i]))
                    })
                    .map(|i| (centres[i], 0.0))
                    .collect()
            };
            entries = ring(true);
            if entries.len() < 4 {
                entries = ring(false);
            }
        }
        let entry = grid.field(raider, c.raider_speed.max(RAIDER_SPEED), &entries);
        let mut candidates = Vec::new();
        for z in (0..grid.h).step_by(CAND_STEP) {
            for x in (0..grid.w).step_by(CAND_STEP) {
                let i = z * grid.w + x;
                if !grid.passable(centres[i], mobility) {
                    continue;
                }
                let near = targets.iter().map(|t| t.field.cell(i)).fold(NEVER, f32::min);
                if near <= CAND_REACH && entry.cell(i).is_finite() {
                    candidates.push(i);
                }
            }
        }
        let c = &mut self.qrf.cover;
        c.entries = entries.len();
        c.entry = Some(entry);
        c.targets = targets;
        c.candidates = candidates;
    }

    /// Stations for the free teams: those that cover most (greedy, then
    /// local search), taken only if they cover enough more than the
    /// stations the teams have.
    fn qrf_cover_station(&mut self, teams: &BTreeMap<usize, Vec<i32>>, free: &[usize], p: Params) {
        let Some(grid) = self.qrf.grid.as_ref() else { return };
        let c = &self.qrf.cover;
        let Some(entry) = c.entry.as_ref() else { return };
        let targets = &c.targets;
        let k = free.len();
        let score = |cells: &[usize]| covered(targets, entry, cells, p);
        // Greedy.
        let mut chosen: Vec<usize> = Vec::new();
        for _ in 0..k {
            let mut trial = chosen.clone();
            trial.push(0);
            let last = trial.len() - 1;
            let mut top = (f32::NEG_INFINITY, None);
            for &cand in &c.candidates {
                trial[last] = cand;
                let s = score(&trial);
                if s > top.0 {
                    top = (s, Some(cand));
                }
            }
            match top.1 {
                Some(cell) => chosen.push(cell),
                None => break,
            }
        }
        // Local search: move one station at a time to its best cell.
        for _ in 0..3 {
            let mut moved = false;
            for i in 0..chosen.len() {
                let mut trial = chosen.clone();
                let now = score(&trial);
                let mut top = (now + 1e-3, None);
                for &cand in &c.candidates {
                    trial[i] = cand;
                    let s = score(&trial);
                    if s > top.0 {
                        top = (s, Some(cand));
                    }
                }
                if let Some(cell) = top.1 {
                    chosen[i] = cell;
                    moved = true;
                }
            }
            if !moved {
                break;
            }
        }
        let new_cov = score(&chosen);
        let current: Vec<usize> = (free.iter())
            .map(|t| grid.cell(c.stations.get(t).copied().unwrap_or_else(|| self.qrf_team_pos(&teams[t]).0)))
            .collect();
        let old_cov = score(&current);
        let total = coverable(targets, entry, p);
        let have_all = free.iter().all(|t| c.stations.contains_key(t));
        let mut stations: BTreeMap<usize, Vec3> =
            free.iter().filter_map(|t| c.stations.get(t).map(|p| (*t, *p))).collect();
        let mut restationed = false;
        if !have_all || new_cov > old_cov + MOVE_GAIN * total {
            // Teams to stations: the least walking.
            let places: Vec<Vec3> = chosen.iter().map(|&s| grid.centre(s)).collect();
            let from: Vec<Vec3> = free.iter().map(|t| self.qrf_team_pos(&teams[t]).0).collect();
            let order = best_matching(&from, &places);
            stations.clear();
            for (i, &t) in free.iter().enumerate() {
                if let Some(&j) = order.get(i)
                    && let Some(&p) = places.get(j)
                {
                    stations.insert(t, p);
                }
            }
            restationed = true;
        }
        let cells: Vec<usize> = stations.values().map(|&p| grid.cell(p)).collect();
        let cov_share = score(&cells) / total;
        let c = &mut self.qrf.cover;
        if restationed {
            c.stats.restations += 1;
        }
        c.coverage = cov_share;
        // Busy teams' stations are kept for when they come back.
        for (t, p) in stations {
            c.stations.insert(t, p);
        }
    }
}

/// Which place each of `from` goes to (an index into `to`), least total
/// distance: exhaustive for few, greedy for more.
fn best_matching(from: &[Vec3], to: &[Vec3]) -> Vec<usize> {
    let n = from.len().min(to.len());
    if to.len() <= 6 {
        let mut idx: Vec<usize> = (0..to.len()).collect();
        let mut best = (f32::INFINITY, idx.clone());
        permute(&mut idx, 0, &mut |p| {
            let d: f32 = (0..n).map(|i| distance(from[i], to[p[i]])).sum();
            if d < best.0 {
                best = (d, p.to_vec());
            }
        });
        best.1.truncate(from.len());
        return best.1;
    }
    let mut used = vec![false; to.len()];
    (from.iter())
        .map(|&f| {
            let j = (0..to.len())
                .filter(|&j| !used[j])
                .min_by(|&a, &b| distance(f, to[a]).total_cmp(&distance(f, to[b])))
                .unwrap_or(0);
            if j < used.len() {
                used[j] = true;
            }
            j
        })
        .collect()
}

fn permute(v: &mut Vec<usize>, k: usize, f: &mut impl FnMut(&[usize])) {
    if k == v.len() {
        f(v);
        return;
    }
    for i in k..v.len() {
        v.swap(k, i);
        permute(v, k + 1, f);
        v.swap(k, i);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matching_takes_the_short_way() {
        let from = [Vec3::xz(0.0, 0.0), Vec3::xz(1000.0, 0.0)];
        let to = [Vec3::xz(1000.0, 10.0), Vec3::xz(0.0, 10.0)];
        assert_eq!(best_matching(&from, &to), vec![1, 0]);
    }

    #[test]
    fn coverage_fades_over_the_soft_band() {
        assert_eq!(cov(-SOFT), 0.0);
        assert_eq!(cov(SOFT), 1.0);
        assert!((cov(0.0) - 0.5).abs() < 1e-6);
    }
}
