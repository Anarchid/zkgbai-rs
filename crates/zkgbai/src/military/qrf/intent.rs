//! The `intent` QRF algorithm: guess where each raid is going, and be there
//! first.
//!
//! Each raid (contacts grouped by `qrf`, followed across updates by their
//! members) carries a belief over destinations: every metal spot (taken or
//! not: raiders scout empty ones too) and our buildings away from spots. The
//! prior is a conditional logit over features of each destination as the
//! raid sees it (how far, how much nearer than the nearest, ours, guarded by
//! a team no weaker than the raid, whose surroundings enemies have seen,
//! visited by enemies lately), in the form of Synnaeve and Bessière's
//! tactical attack prediction; its weights can be fitted from the
//! `qrfchoice:` lines the algorithm logs at every destination a raid
//! reaches. The evidence is the raid's progress: a raid going somewhere
//! gets nearer to it, in travel time over the ground, as fast as it moves
//! (its velocity is exact even on radar); one going elsewhere does not.
//! Evidence fades, so a raid that turns away (it does, when it sees a team)
//! soon has a new belief.
//!
//! Each raid in turn, the most value at stake first, gets the team and the
//! place that cover the most of what it may go for: a team covers an asset
//! from a place if it gets there and then to the asset before the raid is
//! done killing it, or if the place is on the raid's way to the asset and
//! the team is there first (an interception). Places tried: the likeliest
//! destinations, the points between them (a hedge when the belief is
//! split), and where the team can first cut each likely way. A team fights a
//! raid that comes near it, near our things and the place it holds, and not
//! running off, if it is clearly stronger: nothing mends the force, so it
//! never chases, regroups before it fights, and a lone survivor joins
//! another team.
//!
//! Teams with no raid wait where they answer raids soonest: stations chosen
//! so that from where raids show up (at first a ring at radar's edge, then
//! as seen) the team reaches the first asset the raid would hit with as
//! little delay past the raid as can be.

use std::collections::{BTreeMap, BTreeSet};

use springai::Vec3;

use super::field::Field;
use crate::Ai;
use crate::util::distance;

/// Our buildings this close to a destination are its value.
const CLUSTER: f32 = 220.0;

/// Features of a destination, for the prior: distance (per 1000 elmos),
/// how much farther than the nearest (per 1000), ours, guarded, ours and
/// seen by enemies, visited by enemies lately.
const FEATURES: usize = 6;
/// Fitted (conditional logit, `fitprior.py`) from 1393 choices of the
/// usual raider AI in the staged raid game: distance is nearly all of it.
pub const PRIOR_WEIGHTS: [f32; FEATURES] = [-2.08, -2.16, 0.02, -0.07, 0.38, 0.09];

/// What a radar is worth to the planner: what it lets us see, not its
/// metal. Raiders kill radars first, and then we are blind there.
const RADAR_WORTH: f32 = 400.0;

/// Our buildings this close to an asset a raid reaches are hit with it.
const NEIGHBOURS: f32 = 600.0;

/// A team no weaker than the raid this close to a destination guards it.
const GUARDED: f32 = 600.0;

/// Enemies this close to a destination have seen it; this close, lately,
/// visited it.
const SEEN: f32 = 700.0;
const VISITED: f32 = 400.0;
const LATELY: i32 = 30 * 30;

/// Evidence: each update keeps this much of the old belief; progress at
/// full speed toward a destination is worth `PROGRESS` logits per update.
const KEEP: f32 = 0.8;
const PROGRESS: f32 = 1.0;

/// A raid not seen for this many frames is over; one seen again this soon
/// this close to where it was is the same (radar flickers, contacts drop
/// out of sight behind hills).
const RAID_GONE: i32 = 30 * 10;
const RAID_AGAIN: f32 = 700.0;

/// Seconds a raid takes to kill what it reaches; a team there by then
/// still saves most of it.
const KILL: f32 = 4.0;

/// Seconds a team must be at an interception ahead of the raid.
const MARGIN: f32 = 2.0;

/// Travel over the ground is this much longer than in a straight line.
const DETOUR: f32 = 1.15;

/// A team takes on a raid within this distance of it, when the raid is
/// within `LEASH` of the place the team holds and the team has at least
/// `ODDS` of the raid's power.
const ENGAGE: f32 = 500.0;
const LEASH: f32 = 900.0;
const ODDS: f32 = 1.5;

/// The raid must be this close to our things to be fought, and moving away
/// from the team no faster than this (elmos per second).
const IN_REACH: f32 = 600.0;
const FLEEING: f32 = 30.0;

/// A team whose farthest member is this far from its centre regroups
/// before it fights.
const SPREAD: f32 = 250.0;

/// A team holds a place for a raid only if it covers this much expected
/// metal there.
const MIN_COVER: f32 = 5.0;

/// A place this close to a raid's way to a destination is on it.
const ON_WAY: f32 = 160.0;

/// A raid whose centre comes this close to a destination went there.
const APPROACH: f32 = 300.0;

/// When a raid's guess is scored, frames after it was first seen.
const SCORE_AT: i32 = 30 * 5;

/// Destinations in a `qrfchoice` line: the nearest so many.
const CHOICES: usize = 20;

/// Frames between re-placing the waiting stations.
const RESITE: i32 = 30 * 20;

/// Where raids are first expected to show up: this far from our nearest
/// asset (radar's edge), sampled this far apart; seen ones weigh more.
const RING: (f32, f32) = (1200.0, 1800.0);
const RING_SAMPLE: f32 = 256.0;
const SEEN_ENTRY: f32 = 3.0;

/// A raid's speed when nothing better is known, elmos per second.
const RAID_SPEED: f32 = 115.0;

/// A team as the plan sees it.
struct TeamInfo {
    team: usize,
    ids: Vec<i32>,
    pos: Vec3,
    /// Its slowest member's, elmos per second.
    speed: f32,
    power: f32,
    field: Option<Field>,
}

/// A destination: a metal spot, or a building of ours away from spots.
#[derive(Clone, Copy)]
struct Dest {
    key: i64,
    pos: Vec3,
    /// Metal of ours there (0: an empty spot).
    value: f32,
}

/// What we believe of one raid.
struct Belief {
    members: BTreeSet<i32>,
    born: i32,
    last_seen: i32,
    entry: Vec3,
    pos: Vec3,
    power: f32,
    /// Seconds to each destination at the last update.
    eta: BTreeMap<i64, f32>,
    logit: BTreeMap<i64, f32>,
    /// The first guess scored: the likeliest and second likeliest
    /// destination, the first's probability and seconds away.
    guess: Option<(i64, Option<i64>, f32, f32)>,
    /// The first destination it went to, and whether it was ours.
    went: Option<(i64, bool)>,
    /// Destinations and their features when this leg began.
    leg: Vec<(i64, [f32; FEATURES])>,
    legs: u32,
    last_went: Option<i64>,
}

#[derive(Default)]
struct Score {
    raids: u32,
    guessed: u32,
    top1: u32,
    top2: u32,
    top1_ours: u32,
    went_ours: u32,
    unresolved: u32,
}

#[derive(Default)]
pub struct Intent {
    beliefs: BTreeMap<u32, Belief>,
    next: u32,
    /// Where raids show up: sampled at first, then as seen.
    entries: Vec<(Vec3, f32)>,
    sampled: bool,
    stations: Vec<Vec3>,
    sited: i32,
    /// Destinations enemies have seen, and when they visited them last.
    seen: BTreeSet<i64>,
    visited: BTreeMap<i64, i32>,
    /// What each team holds: the raid and the place.
    holds: BTreeMap<usize, (u32, Vec3)>,
    weights: Option<[f32; FEATURES]>,
    /// Free teams join others' raids (AI option `qrf_concentrate`, on).
    concentrate: bool,
    /// No chasing, regrouping, lone members merged (`qrf_chase=on` off).
    no_chase: bool,
    /// Power a team needs over a raid to take it on (`qrf_odds`).
    odds: f32,
    /// Deterrence only (`qrf_deter=on`): teams never go at raids and hold
    /// position where they stand, firing at what comes in range.
    deter: bool,
    held: BTreeSet<i32>,
    score: Score,
}

/// Softmax of logits.
fn probabilities(logit: &BTreeMap<i64, f32>) -> BTreeMap<i64, f32> {
    let max = logit.values().copied().fold(f32::NEG_INFINITY, f32::max);
    let sum: f32 = logit.values().map(|l| (l - max).exp()).sum();
    logit.iter().map(|(&a, l)| (a, (l - max).exp() / sum.max(f32::MIN_POSITIVE))).collect()
}

fn dot(w: &[f32; FEATURES], f: &[f32; FEATURES]) -> f32 {
    w.iter().zip(f).map(|(a, b)| a * b).sum()
}

impl Ai {
    /// Destinations, with our metal at each.
    fn qrf_intent_dests(&self) -> Vec<Dest> {
        let assets: Vec<(Vec3, f32)> = (self.qrf.assets.iter())
            .map(|a| (a.pos, if self.defs.name_of(a.unit) == "staticradar" { RADAR_WORTH } else { a.cost }))
            .collect();
        let mut dests: Vec<Dest> = (self.graph.metal_spots.iter().enumerate())
            .map(|(i, s)| {
                let p = s.get_pos();
                let value = assets.iter().filter(|a| distance(a.0, p) < CLUSTER).map(|a| a.1).sum();
                Dest { key: i as i64, pos: p, value }
            })
            .collect();
        for (a, &(pos, _)) in self.qrf.assets.iter().zip(&assets) {
            if dests.iter().any(|d| distance(d.pos, pos) < CLUSTER) {
                continue;
            }
            let value = assets.iter().filter(|b| distance(pos, b.0) < CLUSTER).map(|b| b.1).sum();
            dests.push(Dest { key: 1_000_000 + i64::from(a.unit.id()), pos, value });
        }
        dests
    }

    pub(super) fn qrf_intent_update(&mut self, frame: i32) {
        let dt = super::PERIOD as f32 / 30.0;
        if self.qrf.intent.weights.is_none() {
            let w = self.cb.option("qrf_prior").and_then(|v| {
                let w: Vec<f32> = v.split(':').filter_map(|x| x.parse().ok()).collect();
                <[f32; FEATURES]>::try_from(w).ok()
            });
            self.qrf.intent.weights = Some(w.unwrap_or(PRIOR_WEIGHTS));
            self.qrf.intent.concentrate = self.cb.option("qrf_concentrate").is_none_or(|v| v != "off");
            self.qrf.intent.no_chase = self.cb.option("qrf_chase").is_none_or(|v| v != "on");
            self.qrf.intent.odds = self.cb.option("qrf_odds").and_then(|v| v.parse().ok()).unwrap_or(ODDS);
            self.qrf.intent.deter = self.cb.option("qrf_deter").is_some_and(|v| v == "on");
            if self.qrf.intent.deter {
                self.qrf.intent.odds = f32::INFINITY;
            }
        }
        let weights = self.qrf.intent.weights.unwrap_or(PRIOR_WEIGHTS);
        let dests = self.qrf_intent_dests();
        if !dests.iter().any(|d| d.value > 0.0) {
            return;
        }
        if !self.qrf.intent.sampled {
            self.qrf_intent_sample_entries(&dests);
        }
        // What enemies have seen and visited.
        let contacts: Vec<Vec3> = self.qrf.tracks.values().filter(|t| t.last_seen == frame).map(|t| t.pos).collect();
        for d in &dests {
            if contacts.iter().any(|&c| distance(c, d.pos) < SEEN) {
                self.qrf.intent.seen.insert(d.key);
            }
            if contacts.iter().any(|&c| distance(c, d.pos) < VISITED) {
                self.qrf.intent.visited.insert(d.key, frame);
            }
        }

        // Raids followed by their members.
        let groups: Vec<(BTreeSet<i32>, Vec3, f32, usize)> = (self.qrf.groups.iter().enumerate())
            .map(|(i, g)| (g.members.iter().copied().collect(), g.center, g.power, i))
            .collect();
        let mut matched: BTreeMap<u32, usize> = BTreeMap::new();
        let mut taken = BTreeSet::new();
        for (&bid, b) in &self.qrf.intent.beliefs {
            let best = (groups.iter())
                .filter(|g| !taken.contains(&g.3))
                .map(|g| (g.0.intersection(&b.members).count(), g.3))
                .filter(|&(n, _)| n > 0)
                .max();
            if let Some((_, gi)) = best {
                matched.insert(bid, gi);
                taken.insert(gi);
            }
        }
        for (members, center, power, gi) in &groups {
            if taken.contains(gi) {
                continue;
            }
            // A raid lost a while and seen again near where it was is the
            // same raid.
            let lost = (self.qrf.intent.beliefs.iter())
                .filter(|(bid, b)| !matched.contains_key(bid) && distance(b.pos, *center) < RAID_AGAIN)
                .min_by(|a, b| distance(a.1.pos, *center).total_cmp(&distance(b.1.pos, *center)))
                .map(|(&bid, _)| bid);
            if let Some(bid) = lost {
                if let Some(b) = self.qrf.intent.beliefs.get_mut(&bid) {
                    b.members = members.clone();
                }
                matched.insert(bid, *gi);
                continue;
            }
            let bid = self.qrf.intent.next;
            self.qrf.intent.next += 1;
            // A raid split off another keeps what we believed of it.
            let parent = (self.qrf.intent.beliefs.values()).find(|b| b.members.intersection(members).next().is_some());
            let (born, entry, eta, logit) = match parent {
                Some(b) => (b.born, b.entry, b.eta.clone(), b.logit.clone()),
                None => (frame, *center, BTreeMap::new(), BTreeMap::new()),
            };
            if parent.is_none() {
                self.qrf.intent.entries.push((*center, SEEN_ENTRY));
            }
            self.qrf.intent.beliefs.insert(
                bid,
                Belief {
                    members: members.clone(),
                    born,
                    last_seen: frame,
                    entry,
                    pos: *center,
                    power: *power,
                    eta,
                    logit,
                    guess: None,
                    went: None,
                    leg: Vec::new(),
                    legs: 0,
                    last_went: None,
                },
            );
            matched.insert(bid, *gi);
        }

        // Teams and their travel times.
        let teams = self.qrf_teams();
        let team_info: Vec<TeamInfo> = (teams.iter())
            .map(|(&t, ids)| {
                let (pos, speed) = self.qrf_team_pos(ids);
                TeamInfo {
                    team: t,
                    ids: ids.clone(),
                    pos,
                    speed,
                    power: self.qrf_team_power(ids),
                    field: self.qrf_team_field(ids),
                }
            })
            .collect();

        // Beliefs: the prior from features, evidence from progress.
        let mut fields: BTreeMap<u32, Field> = BTreeMap::new();
        let bids: Vec<u32> = matched.keys().copied().collect();
        for bid in bids {
            let gi = matched[&bid];
            let g = &self.qrf.groups[gi];
            let (center, power, vel_len, speed) = (g.center, g.power, g.vel.length_2d(), g.speed);
            let Some(field) = self.qrf_group_field(g) else { continue };
            let members: BTreeSet<i32> = g.members.iter().copied().collect();
            let feats = self.qrf_intent_features(&dests, center, power, &team_info, frame);
            let Some(b) = self.qrf.intent.beliefs.get_mut(&bid) else { continue };
            b.members = members;
            b.last_seen = frame;
            b.pos = center;
            b.power = power;
            if b.leg.is_empty() {
                b.leg = feats.iter().map(|&(i, f)| (dests[i].key, f)).collect();
                b.leg.sort_by(|a, b| a.1[0].total_cmp(&b.1[0]));
                b.leg.truncate(CHOICES);
            }
            let mut eta = BTreeMap::new();
            for &(i, f) in &feats {
                let d = dests[i];
                let t = field.at(d.pos);
                let t = if t.is_finite() { t } else { distance(d.pos, center) * DETOUR / speed.max(1.0) };
                eta.insert(d.key, t);
                let prior = dot(&weights, &f);
                let evidence = match b.eta.get(&d.key) {
                    Some(&prev) if vel_len > 10.0 => PROGRESS * (((prev - t) / dt).clamp(-1.5, 1.5) - 1.0),
                    _ => 0.0,
                };
                let old = b.logit.get(&d.key).copied().unwrap_or(prior);
                b.logit.insert(d.key, KEEP * old + (1.0 - KEEP) * prior + evidence);
            }
            b.logit.retain(|k, _| eta.contains_key(k));
            b.eta = eta;
            if b.guess.is_none() && frame - b.born >= SCORE_AT {
                let mut ranked: Vec<(i64, f32)> = probabilities(&b.logit).into_iter().collect();
                ranked.sort_by(|a, b| b.1.total_cmp(&a.1));
                if let Some(&(first, pf)) = ranked.first() {
                    let t = b.eta.get(&first).copied().unwrap_or(0.0);
                    b.guess = Some((first, ranked.get(1).map(|r| r.0), pf, t));
                }
            }
            fields.insert(bid, field);
        }
        // Where raids went: a choice made, a new leg.
        let mut choices = Vec::new();
        for (&bid, b) in self.qrf.intent.beliefs.iter_mut().filter(|(_, b)| b.last_seen == frame) {
            let Some(d) = dests.iter().find(|d| distance(d.pos, b.pos) < APPROACH && Some(d.key) != b.last_went) else {
                continue;
            };
            if b.went.is_none() {
                b.went = Some((d.key, d.value > 0.0));
            }
            if let Some(k) = b.leg.iter().position(|l| l.0 == d.key) {
                choices.push((bid, b.legs, k, std::mem::take(&mut b.leg)));
            }
            b.leg.clear();
            b.legs += 1;
            b.last_went = Some(d.key);
        }
        for (bid, leg, k, list) in choices {
            let fs: Vec<String> =
                (list.iter()).map(|(_, f)| f.iter().map(|x| format!("{x:.2}")).collect::<Vec<_>>().join(",")).collect();
            self.log(&format!("qrfchoice: raid={bid} leg={leg} chosen={k} feats={}", fs.join(";")));
        }
        // Raids over: scored.
        let over: Vec<u32> = (self.qrf.intent.beliefs.iter())
            .filter(|(_, b)| frame - b.last_seen > RAID_GONE)
            .map(|(&id, _)| id)
            .collect();
        for bid in over {
            if let Some(b) = self.qrf.intent.beliefs.remove(&bid) {
                self.qrf_intent_score(bid, &b, &dests);
            }
        }

        // Plans: raids by value at stake, each the best free team and place.
        let mut plans: Vec<(f32, u32, BTreeMap<i64, f32>)> = (self.qrf.intent.beliefs.iter())
            .filter(|(bid, b)| b.last_seen == frame && fields.contains_key(bid))
            .map(|(&bid, b)| {
                let p = probabilities(&b.logit);
                let stake: f32 = (dests.iter())
                    .map(|d| {
                        let t = b.eta.get(&d.key).copied().unwrap_or(f32::INFINITY);
                        p.get(&d.key).copied().unwrap_or(0.0) * d.value / (t + 10.0)
                    })
                    .sum();
                (stake, bid, p)
            })
            .collect();
        plans.sort_by(|a, b| b.0.total_cmp(&a.0));
        let mut free: BTreeSet<usize> = team_info.iter().map(|t| t.team).collect();
        let mut holds = BTreeMap::new();
        for (_, bid, p) in &plans {
            let b = &self.qrf.intent.beliefs[bid];
            let rf = &fields[bid];
            let mut likely: Vec<(i64, f32)> = p.iter().map(|(&a, &q)| (a, q)).collect();
            likely.sort_by(|a, b| b.1.total_cmp(&a.1));
            likely.truncate(4);
            let dest_of = |k: i64| dests.iter().find(|d| d.key == k).copied();
            // The raid's way to each likely destination, as places.
            let ways: Vec<(i64, Vec<Vec3>)> = (likely.iter())
                .filter_map(|&(k, _)| dest_of(k))
                .map(|d| {
                    let grid = self.qrf.grid.as_ref();
                    let cells = rf.descent(d.pos, 400);
                    let pts = cells.iter().filter_map(|&c| grid.map(|g| g.centre(c))).collect();
                    (d.key, pts)
                })
                .collect();
            let mut best: Option<(f32, f32, usize, Vec3)> = None;
            for ti in team_info.iter().filter(|t| free.contains(&t.team)) {
                let Some(tf) = &ti.field else { continue };
                let mut cands: Vec<Vec3> =
                    likely.iter().filter_map(|&(k, _)| dest_of(k)).filter(|d| d.value > 0.0).map(|d| d.pos).collect();
                let n = cands.len();
                for i in 0..n {
                    for j in i + 1..n {
                        cands.push((cands[i] + cands[j]) * 0.5);
                    }
                }
                // The first place on each way the team is at before the raid.
                for (_, pts) in &ways {
                    // `descent` runs from the destination back to the raid.
                    if let Some(&x) = pts.iter().rev().find(|&&x| tf.at(x) + MARGIN <= rf.at(x)) {
                        cands.push(x);
                    }
                }
                // Destinations whose way the team cuts from each place.
                let cut_ways: Vec<Vec<i64>> = (cands.iter())
                    .map(|&x| {
                        if tf.at(x) + MARGIN > rf.at(x) {
                            return Vec::new();
                        }
                        (ways.iter())
                            .filter(|(_, pts)| pts.iter().any(|&q| distance(q, x) < ON_WAY))
                            .map(|(k, _)| *k)
                            .collect()
                    })
                    .collect();
                for (&x, cut) in cands.iter().zip(&cut_ways) {
                    let tx = tf.at(x);
                    if !tx.is_finite() {
                        continue;
                    }
                    let cut_at: Vec<Vec3> = cut.iter().filter_map(|&k| dest_of(k)).map(|d| d.pos).collect();
                    let covered: f32 = (dests.iter())
                        .filter(|d| d.value > 0.0)
                        .map(|d| {
                            let raid_t = b.eta.get(&d.key).copied().unwrap_or(f32::INFINITY);
                            let t = tx + distance(x, d.pos) * DETOUR / ti.speed.max(1.0);
                            // A cut way saves where it led and what is by it.
                            let share =
                                if cut_at.iter().any(|&c| distance(c, d.pos) < NEIGHBOURS) || t + MARGIN <= raid_t {
                                    1.0
                                } else if t <= raid_t + KILL {
                                    0.5
                                } else {
                                    0.0
                                };
                            share * p.get(&d.key).copied().unwrap_or(0.0) * d.value
                        })
                        .sum();
                    // More covered, then nearer the raid (met sooner).
                    let ahead = distance(x, b.pos);
                    let better = match best {
                        None => true,
                        Some((c, a, _, _)) => covered > c * 1.05 + 1.0 || (covered > c * 0.95 - 1.0 && ahead < a),
                    };
                    if better {
                        best = Some((covered, ahead, ti.team, x));
                    }
                }
            }
            // A raid that puts nothing of ours at stake is not worth a team
            // off its station.
            if let Some((covered, _, t, x)) = best
                && covered >= MIN_COVER
            {
                free.remove(&t);
                holds.insert(t, (*bid, x));
            }
        }

        // Free teams join a held raid they get to in time: two teams kill a
        // raid faster, and lose less doing it, than one (Lanchester), and
        // nothing mends the force.
        if self.qrf.intent.concentrate {
            for ti in team_info.iter().filter(|t| free.contains(&t.team)) {
                let Some(tf) = &ti.field else { continue };
                let join = (holds.iter())
                    .filter_map(|(_, &(bid, x))| {
                        let rf = fields.get(&bid)?;
                        (tf.at(x) <= rf.at(x) + KILL).then_some((bid, x, tf.at(x)))
                    })
                    .min_by(|a, b| a.2.total_cmp(&b.2));
                if let Some((bid, x, _)) = join {
                    holds.insert(ti.team, (bid, x));
                }
            }
        }

        // Orders: hold the place, take on a raid that comes near.
        let ours: Vec<Vec3> = dests.iter().filter(|d| d.value > 0.0).map(|d| d.pos).collect();
        for ti in &team_info {
            let (t, ids, tpos, tpower) = (ti.team, &ti.ids, ti.pos, ti.power);
            let place = holds.get(&t).map(|h| h.1);
            // A raid near the team, near our things and its place, and not
            // running off: nothing mends the force, so it does not chase.
            let near = (self.qrf.groups.iter())
                .filter(|g| distance(g.center, tpos) < ENGAGE && tpower >= self.qrf.intent.odds * g.power)
                .filter(|g| place.is_none_or(|x| distance(g.center, x) < LEASH))
                .filter(|g| !self.qrf.intent.no_chase || ours.iter().any(|&a| distance(a, g.center) < IN_REACH))
                .filter(|g| {
                    let away = Vec3::xz(g.center.x - tpos.x, g.center.z - tpos.z);
                    let n = away.length_2d().max(1.0);
                    !self.qrf.intent.no_chase || (g.vel.x * away.x + g.vel.z * away.z) / n < FLEEING
                })
                .min_by(|a, b| distance(a.center, tpos).total_cmp(&distance(b.center, tpos)))
                .map(|g| g.center + g.vel * 1.0);
            // Strung out: together first.
            let spread = (ids.iter())
                .filter_map(|id| self.qrf.members.get(id))
                .map(|m| distance(m.pos(), tpos))
                .fold(0.0f32, f32::max);
            if let Some(aim) = near {
                if spread > SPREAD && self.qrf.intent.no_chase {
                    self.qrf_order_team(ids, tpos, true);
                } else {
                    self.qrf_order_team(ids, aim, true);
                }
            } else if let Some(x) = place {
                self.qrf_order_team(ids, x, true);
            }
        }
        // Deterring teams hold position: no chasing what they shoot at.
        if self.qrf.intent.deter {
            for (&id, m) in &self.qrf.members {
                if self.qrf.intent.held.insert(id) {
                    m.unit.set_move_state(0, 0);
                }
            }
        }
        // A team down to one joins the nearest other.
        if self.qrf.intent.no_chase && team_info.len() > 1 {
            for ti in team_info.iter().filter(|t| t.ids.len() == 1) {
                let other = (team_info.iter())
                    .filter(|o| o.team != ti.team && o.ids.len() > 1)
                    .min_by(|a, b| distance(a.pos, ti.pos).total_cmp(&distance(b.pos, ti.pos)));
                if let Some(o) = other
                    && let Some(m) = self.qrf.members.get_mut(&ti.ids[0])
                {
                    m.team = o.team;
                }
            }
        }
        self.qrf.intent.holds = holds;
        self.qrf_intent_wait(
            frame,
            &dests,
            &team_info.iter().map(|t| (t.team, t.ids.clone(), t.speed)).collect::<Vec<_>>(),
        );
        if frame % (30 * 30) < super::PERIOD {
            self.qrf_intent_log(frame);
        }
    }

    /// Features of every destination for a raid at `at`.
    fn qrf_intent_features(
        &self,
        dests: &[Dest],
        at: Vec3,
        power: f32,
        teams: &[TeamInfo],
        frame: i32,
    ) -> Vec<(usize, [f32; FEATURES])> {
        let near = dests.iter().map(|d| distance(d.pos, at)).fold(f32::INFINITY, f32::min);
        (dests.iter().enumerate())
            .map(|(i, d)| {
                let dist = distance(d.pos, at);
                let ours = d.value > 0.0;
                let guarded = teams.iter().any(|t| distance(t.pos, d.pos) < GUARDED && t.power >= power);
                let seen = ours && self.qrf.intent.seen.contains(&d.key);
                let lately = self.qrf.intent.visited.get(&d.key).is_some_and(|&f| frame - f < LATELY);
                let f = [
                    dist / 1000.0,
                    (dist - near) / 1000.0,
                    f32::from(u8::from(ours)),
                    f32::from(u8::from(guarded)),
                    f32::from(u8::from(seen)),
                    f32::from(u8::from(lately)),
                ];
                (i, f)
            })
            .collect()
    }

    /// The first guess of where raids show up: a ring at radar's edge
    /// around our assets, on the enemy's side of them (each place more
    /// toward the enemy's start than its nearest asset).
    fn qrf_intent_sample_entries(&mut self, dests: &[Dest]) {
        self.qrf.intent.sampled = true;
        let ours: Vec<Vec3> = dests.iter().filter(|d| d.value > 0.0).map(|d| d.pos).collect();
        let map = self.cb.map();
        let (w, h) = (map.width() as f32 * 8.0, map.height() as f32 * 8.0);
        // Where the enemy is: its metal, or else across the map from ours.
        let enemy = match self.graph.get_enemy_center() {
            e if e != crate::util::NULLPOS => e,
            _ => {
                let n = ours.len().max(1) as f32;
                let c = ours.iter().fold(Vec3::ZERO, |a, &p| a + p * (1.0 / n));
                Vec3::xz(w - c.x, h - c.z)
            }
        };
        let facing = |p: Vec3, a: Vec3| distance(p, enemy) < distance(a, enemy);
        let mut z = RING_SAMPLE / 2.0;
        while z < h {
            let mut x = RING_SAMPLE / 2.0;
            while x < w {
                let p = Vec3::xz(x, z);
                let nearest = ours.iter().copied().min_by(|a, b| distance(*a, p).total_cmp(&distance(*b, p)));
                let d = nearest.map_or(f32::INFINITY, |a| distance(a, p));
                if (RING.0..RING.1).contains(&d) && nearest.is_some_and(|a| facing(p, a)) {
                    self.qrf.intent.entries.push((p, 1.0));
                }
                x += RING_SAMPLE;
            }
            z += RING_SAMPLE;
        }
    }

    /// Teams with no raid wait where they answer raids soonest.
    fn qrf_intent_wait(&mut self, frame: i32, dests: &[Dest], teams: &[(usize, Vec<i32>, f32)]) {
        let k = teams.len();
        if self.qrf.intent.stations.len() != k || frame - self.qrf.intent.sited > RESITE {
            let ours: Vec<Vec3> = dests.iter().filter(|d| d.value > 0.0).map(|d| d.pos).collect();
            let team_speed = teams.iter().map(|t| t.2).fold(f32::INFINITY, f32::min).min(200.0);
            // Each entry: its first asset and the raid's time there.
            let demand: Vec<(Vec3, f32, f32)> = (self.qrf.intent.entries.iter())
                .filter_map(|&(e, w)| {
                    let a = ours.iter().copied().min_by(|a, b| distance(*a, e).total_cmp(&distance(*b, e)))?;
                    Some((a, w, distance(a, e) * DETOUR / RAID_SPEED))
                })
                .collect();
            let late = |set: &[Vec3]| -> f32 {
                (demand.iter())
                    .map(|&(a, w, t)| {
                        let ours_t =
                            set.iter().map(|&s| distance(s, a) * DETOUR / team_speed).fold(f32::INFINITY, f32::min);
                        w * (ours_t - t + MARGIN).max(0.0)
                    })
                    .sum()
            };
            let mut set: Vec<Vec3> = Vec::new();
            while set.len() < k.min(ours.len()) {
                let best = (ours.iter())
                    .filter(|c| !set.contains(c))
                    .min_by(|a, b| {
                        let mut sa = set.clone();
                        sa.push(**a);
                        let mut sb = set.clone();
                        sb.push(**b);
                        late(&sa).total_cmp(&late(&sb))
                    })
                    .copied();
                match best {
                    Some(b) => set.push(b),
                    None => break,
                }
            }
            for _ in 0..3 {
                let mut improved = false;
                for i in 0..set.len() {
                    for &c in &ours {
                        if set.contains(&c) {
                            continue;
                        }
                        let mut s = set.clone();
                        s[i] = c;
                        if late(&s) < late(&set) - 0.5 {
                            set = s;
                            improved = true;
                        }
                    }
                }
                if !improved {
                    break;
                }
            }
            self.qrf.intent.stations = set;
            self.qrf.intent.sited = frame;
        }
        let mut used: BTreeSet<usize> = BTreeSet::new();
        let stations = self.qrf.intent.stations.clone();
        for (t, ids, _) in teams {
            if self.qrf.intent.holds.contains_key(t) {
                continue;
            }
            let (p, _) = self.qrf_team_pos(ids);
            let pick = (0..stations.len())
                .filter(|i| !used.contains(i))
                .min_by(|&a, &b| distance(stations[a], p).total_cmp(&distance(stations[b], p)));
            if let Some(i) = pick {
                used.insert(i);
                if distance(p, stations[i]) > 150.0 {
                    self.qrf_order_team(ids, stations[i], true);
                }
            }
        }
    }

    fn qrf_intent_score(&mut self, bid: u32, b: &Belief, dests: &[Dest]) {
        let s = &mut self.qrf.intent.score;
        s.raids += 1;
        let Some((first, second, p, eta)) = b.guess else { return };
        s.guessed += 1;
        let Some((went, ours)) = b.went else {
            s.unresolved += 1;
            return;
        };
        let top1 = went == first;
        let top2 = top1 || second == Some(went);
        s.top1 += u32::from(top1);
        s.top2 += u32::from(top2);
        s.went_ours += u32::from(ours);
        s.top1_ours += u32::from(ours && top1);
        let pos = |k: i64| dests.iter().find(|d| d.key == k).map_or((0.0, 0.0), |d| (d.pos.x, d.pos.z));
        let (g, w) = (pos(first), pos(went));
        let line = format!(
            "qrfintent: raid {bid} entry {:.0},{:.0} guess {:.0},{:.0} p={p:.2} eta={eta:.0} went {:.0},{:.0} ours={ours} top1={top1} top2={top2} legs={}",
            b.entry.x, b.entry.z, g.0, g.1, w.0, w.1, b.legs
        );
        self.log(&line);
    }

    fn qrf_intent_log(&mut self, frame: i32) {
        let s = &self.qrf.intent.score;
        let holds: Vec<String> =
            (self.qrf.intent.holds.iter()).map(|(t, (bid, x))| format!("{t}->{bid}@{:.0},{:.0}", x.x, x.z)).collect();
        let st: Vec<String> = self.qrf.intent.stations.iter().map(|s| format!("{:.0},{:.0}", s.x, s.z)).collect();
        let line = format!(
            "qrfintent: t={} raids={} guessed={} top1={} top2={} went_ours={} top1_ours={} unresolved={} holds=[{}] stations=[{}]",
            frame / 30,
            s.raids,
            s.guessed,
            s.top1,
            s.top2,
            s.went_ours,
            s.top1_ours,
            s.unresolved,
            holds.join(" "),
            st.join(" ")
        );
        self.log(&line);
    }
}
