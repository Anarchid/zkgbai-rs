//! Invasion targets, live (`invade`, on in the wip line; `disable=invade`): which enemy holding,
//! raider ground or forgotten ground to take next, with the force on hand,
//! and the build that makes it ours.
//!
//! Why (zkgbai-runs/forceplan, 2026-11-03/04): over 326,755 pieces of 932
//! recorded games, our assaults barely followed what a piece was worth
//! (attack AUC 0.555), and after a clear our constructors almost never built
//! on the freed spots (1% within 3 minutes); paying empty ground was left
//! unbuilt 76-98% of the time with constructors out. Standing forces that
//! could clear did so faster and cheaper than any force built for the job
//! (73%), so this sends the units on hand.
//!
//! Every [`PERIOD`] from [`FROM`], each region of our ground tier's terrain
//! map holding metal spots not ours (regions are cut around spot clusters,
//! `regionsplit`) is a piece, of three kinds (the user's): held (an enemy
//! mex on a spot), contested (free spots, enemy armed units seen there
//! lately), open (free spots, nobody about). For each:
//! - stake: what its enemy mexes pay; gain: stake plus all its spots ours;
//! - defenders: armed enemy buildings covering its spots or standing in it,
//!   armed units seen there lately ([`SEEN_HELD`] on held ground,
//!   [`SEEN_EMPTY`] on empty), their metal `D` (by health left);
//! - reclaim: the wrecks lying in it now (the features' metal);
//! - the build after the clear: a mex per spot ([`MEX_COST`]) and, unless
//!   open, a Lotus ([`TURRET_COST`]), at [`CON_BUILD_POWER`], by our
//!   constructor nearest by ground; income from halfway through the build;
//! - sources: each assault squad of ours on its ground route there (and a
//!   lone constructor on open ground); P(clear) and the share lost
//!   from the metal ratio as 782 recorded assaults put them;
//! - hold: every enemy army seen lately away from the piece that can walk
//!   there before the build stands (+60 s) retakes it by the same odds, and
//!   the enemy base by what a factory makes meanwhile;
//! - net = P x hold x (gain x (HORIZON - income start) + reclaim) - lost -
//!   the squad's time (its metal over [`TIE_UP_SECS`]) - the build.
//!
//! Free and defended ground compete on net, including free captures still
//! awaiting construction. An attack starts only if it beats the best free
//! expansion; an existing attack yields after its minimum commitment when
//! free expansion pays `SWITCH` times more. Constructor travel uses the
//! constructor's own movement tier (or flight), independently of the army.
//! If no available constructor reaches the spots, existing factories and
//! plates offer a new one; its cost and queued production time enter the
//! forecast. A chosen job requests that constructor at the factory's next
//! idle decision, with accepted orders and unfinished builders counted once.
//! The defended piece with the best net > 0 is the target: its squad is
//! sent at the defenders (in place of the squad's own target, as an
//! operator would send it), kept on it at least [`MIN_COMMIT`] and until
//! another piece nets [`SWITCH`] times more. Once no defenders or enemy
//! mexes remain it becomes a capture and the squad goes back to its own business.
//! Captures (at most [`MAX_CAPTURES`]; also the best empty piece that pays)
//! put their spots and wrecks first for our constructors ([`SPOT_BONUS`],
//! [`RECLAIM_BONUS`]) until the spots are all ours, defenders show again,
//! or [`BUILD_LIMIT`] passes. A target ends when its net stays negative two
//! passes, its squad is lost, or it is not clear in [`SEND_LIMIT`].

use std::collections::BTreeMap;

use serde_json::{Value, json};
use springai::Vec3;
use zk_terrain::paths::Paths;
use zk_terrain::{Tier, TierMap};

use crate::Ai;
use crate::military::wrappers::{SquadId, SquadKind};
use crate::terrain::{pos, tier_of};
use crate::util::{NULLPOS, distance};

mod builders;
use builders::{BuilderPlan, BuilderRequest};

/// How often the pieces are weighed (frames), and from when.
const PERIOD: i32 = 30 * 10;
const FROM: i32 = 30 * 120;
/// Seconds over which a holding's income counts (our mexes built after 5:00
/// lasted a median 494 s in the recorded batteries).
const HORIZON: f32 = 480.0;
/// Enemy units this near each other are one army.
const GROUP_LINK: f32 = 600.0;
/// Enemy armed units seen this recently (frames) stand where seen: on held
/// ground, and on empty ground (which raiders patrol).
const SEEN_HELD: i32 = 30 * 10;
const SEEN_EMPTY: i32 = 30 * 30;
/// Enemy armies seen this recently can come back.
const ARMY_SEEN: i32 = 30 * 30;
/// The build after a clear: a mex, a Lotus, a constructor's metal a second.
const MEX_COST: f32 = 85.0;
const TURRET_COST: f32 = 90.0;
const CON_BUILD_POWER: f32 = 5.0;
/// Spot to region snapping, elmos.
const SNAP: f32 = 120.0;
/// An army's time is worth its metal over this many seconds.
const TIE_UP_SECS: f32 = 600.0;
/// Glaive speed (elmos a second): the enemy base's army.
const BASE_SPEED: f32 = 75.0;
/// What an enemy factory turns out (metal a second).
const FACTORY_RATE: f32 = 10.0;
/// Mex job bonus on the target's spots once it is clear (see `assign`), and
/// reclaim job bonus in its zone.
pub const SPOT_BONUS: f32 = 1000.0;
pub const RECLAIM_BONUS: f32 = 400.0;
/// A new pick must net this much more than the current target, which keeps
/// its squad at least `MIN_COMMIT` (frames) in any case.
const SWITCH: f32 = 1.5;
const MIN_COMMIT: i32 = 30 * 60;
/// Captures open at once.
const MAX_CAPTURES: usize = 3;
/// A target given up keeps off the list this long (frames).
const COOLDOWN: i32 = 30 * 90;
/// Clearing may take this long, and the build after it this long (frames).
const SEND_LIMIT: i32 = 30 * 180;
const BUILD_LIMIT: i32 = 30 * 240;
/// Paths computed for constructors and enemy armies, at most.
const MAX_CONS: usize = 10;
const MAX_ARMIES: usize = 6;

fn p_clear(ratio: f32) -> f32 {
    if !ratio.is_finite() {
        return 1.0;
    }
    1.0 / (1.0 + (-(-2.123 + 1.653 * ratio.max(0.05).ln())).exp())
}

fn lost_share(ratio: f32) -> f32 {
    if !ratio.is_finite() {
        return 0.0;
    }
    (0.482 - 0.152 * ratio.max(0.05).ln()).clamp(0.0, 1.0)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Held,
    Contested,
    Open,
}
impl Kind {
    fn name(self) -> &'static str {
        match self {
            Self::Held => "held",
            Self::Contested => "contested",
            Self::Open => "open",
        }
    }
}

/// The inputs of a piece's net, apart from the source.
#[derive(Clone, Debug)]
struct Weigh {
    gain: f32,
    d_metal: f32,
    reclaim: f32,
    build_metal: f32,
    build_secs: f32,
    t_con: f32,
    turret: f32,
    /// (seconds to get there, metal) of each army that can come back, and
    /// the enemy base's walk (seconds).
    threats: Vec<(f32, f32)>,
    base_walk: Option<f32>,
}

/// (P(clear), metal lost, P(hold), net) of `metal` clearing in `t_force` s.
fn evaluate(w: &Weigh, metal: f32, t_force: f32) -> (f32, f32, f32, f32) {
    let ratio = if w.d_metal > 0.0 { metal / w.d_metal } else { f32::INFINITY };
    let (p, share) = (p_clear(ratio), lost_share(ratio));
    let start = t_force.max(w.t_con);
    let income_from = start + w.build_secs / 2.0;
    let window = start + w.build_secs + 60.0;
    let keep = (metal * (1.0 - share) + w.turret).max(1.0);
    let mut hold = 1.0f32;
    for &(eta, m) in &w.threats {
        if eta <= window {
            hold *= 1.0 - p_clear(m / keep);
        }
    }
    if let Some(bw) = w.base_walk {
        let built = FACTORY_RATE * (window - bw).max(0.0);
        if built > 0.0 {
            hold *= 1.0 - p_clear(built / keep);
        }
    }
    let reclaim = w.reclaim + 0.4 * (w.d_metal + share * metal);
    let benefit = (w.gain * (HORIZON - income_from).max(0.0) + reclaim) * hold;
    let tied = metal * t_force / TIE_UP_SECS;
    (p, share * metal, hold, p * benefit - share * metal - tied - w.build_metal)
}

pub struct Target {
    pub region: usize,
    pub kind: Kind,
    pub center: Vec3,
    pub spots: Vec<usize>,
    pub radius: f32,
    pub squad: SquadId,
    /// Where the squad goes: the defenders' middle.
    pub aim: Vec3,
    pub since: i32,
    pub net: f32,
    pub p: f32,
    pub d_metal: f32,
    /// Passes in a row its net was below zero.
    bad: u8,
}

/// Ground with no defenders or enemy mexes, first for our constructors: a
/// target the squad cleared, or empty ground that pays.
pub struct Capture {
    pub region: usize,
    pub kind: Kind,
    pub center: Vec3,
    /// Metal spot indices to build on (theirs and free).
    pub spots: Vec<usize>,
    /// Reclaim zone radius around `center`.
    pub radius: f32,
    pub since: i32,
}

#[derive(Default)]
pub struct Invade {
    /// The defended target a squad is sent at.
    pub cur: Option<Target>,
    pub captures: Vec<Capture>,
    tier: Option<Tier>,
    cooldown: Vec<(usize, i32)>,
    /// The last pass's best pieces, for the operator.
    pub top: Vec<Value>,
    pub picks: u32,
    pub clears: u32,
    pub captured: u32,
    builder: Option<BuilderRequest>,
    builder_orders: BTreeMap<i32, (springai::UnitDef, i32)>,
}

/// One piece as weighed this pass.
struct Piece {
    region: usize,
    kind: Kind,
    center: Vec3,
    radius: f32,
    spots: Vec<usize>,
    aim: Vec3,
    weigh: Weigh,
    /// Best source: squad (or none: a constructor alone), metal, net, P.
    best: Option<(Option<SquadId>, f32, f32, f32)>,
    builder: Option<BuilderPlan>,
}

impl Ai {
    /// Our ground tier: the biggest assault squad's, or a constructor's
    /// before an army exists. Free expansion must not wait for an army.
    fn invade_tier(&self) -> Option<Tier> {
        self.invade_squads()
            .into_iter()
            .max_by(|a, b| a.1.total_cmp(&b.1))
            .and_then(|(sid, _, _, _)| self.squads.get(sid)?.get_leader(&self.fighters)?.def())
            .and_then(tier_of)
            .or(self.invade.tier)
            .or_else(|| self.invade_constructors().find_map(|w| w.unit.def().and_then(tier_of)))
            .or_else(|| self.invade_builder_tier())
    }

    fn invade_constructors(&self) -> impl Iterator<Item = &crate::economy::Worker> {
        self.eco.workers.values().filter(|w| {
            !w.is_com
                && !w.is_chicken
                && !w.assigned_plop
                && !w.unit.is_being_built()
                && !self.eco_worker_held(w.id)
                && w.unit.health() > 0.0
                && w.unit.def().is_some_and(|d| d.speed() > 0.0)
        })
    }

    /// A squad's metal by health, position and speed (elmos a second).
    fn invade_squad_state(&self, sid: SquadId) -> Option<(f32, Vec3, f32)> {
        let s = self.squads.get(sid)?;
        let (mut metal, mut speed) = (0.0f32, f32::INFINITY);
        for f in s.fighters.iter().filter_map(|id| self.fighters.get(id)) {
            let hp = f.unit.health() / f.unit.max_health().max(1.0);
            metal += f.metal_value * hp.clamp(0.05, 1.0);
            speed = speed.min(f.unit.def().map_or(60.0, |d| d.speed()));
        }
        let at = s.get_pos(&self.fighters);
        (metal > 0.0 && at != NULLPOS && speed.is_finite()).then_some((metal, at, speed.max(10.0)))
    }

    /// The assault squads that may be sent: (id, metal by health, position,
    /// speed in elmos a second).
    fn invade_squads(&self) -> Vec<(SquadId, f32, Vec3, f32)> {
        let mut out = Vec::new();
        for &sid in &self.squad_handler.squads {
            let Some(s) = self.squads.get(sid) else { continue };
            if !matches!(s.kind, SquadKind::Assault | SquadKind::Hover)
                || !matches!(s.status, 'a' | 'r')
                || self.qrf_drafted(sid)
                || self.op_squad_target(sid).is_some()
                || self.op.over.squad_intents.contains_key(&sid)
            {
                continue;
            }
            if let Some((metal, at, speed)) = self.invade_squad_state(sid).filter(|s| s.0 >= 100.0) {
                out.push((sid, metal, at, speed));
            }
        }
        out
    }

    /// The squad's target while it is sent at an invasion target.
    pub(crate) fn invade_squad_target(&self, sid: SquadId) -> Option<Vec3> {
        let t = self.invade.cur.as_ref()?;
        (t.squad == sid).then_some(t.aim)
    }

    /// A mex job's bonus on a capture's spots (see `assign`).
    pub(crate) fn invade_spot_bonus(&self, p: Vec3) -> f32 {
        let near = |c: &Capture| {
            c.spots.iter().any(|&i| self.graph.metal_spots.get(i).is_some_and(|s| distance(s.position, p) < 60.0))
        };
        if self.invade.captures.iter().any(near) { SPOT_BONUS } else { 0.0 }
    }

    /// A reclaim job's bonus in a capture's zone (see `assign`).
    pub(crate) fn invade_reclaim_bonus(&self, p: Vec3) -> f32 {
        if self.invade.captures.iter().any(|c| distance(c.center, p) <= c.radius) { RECLAIM_BONUS } else { 0.0 }
    }

    fn invade_end(&mut self, why: &str, cool: bool) {
        let Some(t) = self.invade.cur.take() else { return };
        if cool {
            self.invade.cooldown.push((t.region, self.frame));
        }
        self.log(&format!(
            "invade: {} piece {} at ({:.0},{:.0}) ends: {why} after {} s",
            t.kind.name(),
            t.region,
            t.center.x,
            t.center.z,
            (self.frame - t.since) / 30
        ));
        self.op_emit(
            "invade",
            json!({"event": "end", "t": self.frame / 30, "region": t.region, "kind": t.kind.name(), "why": why,
                   "secs": (self.frame - t.since) / 30}),
        );
    }

    fn invade_capture(&mut self, p: &Piece, why: &str) {
        if self.invade.captures.iter().any(|c| c.region == p.region) {
            return;
        }
        self.log(&format!(
            "invade: capture {} piece {} at ({:.0},{:.0}) ({why}): {} spots, reclaim {:.0}",
            p.kind.name(),
            p.region,
            p.center.x,
            p.center.z,
            p.spots.len(),
            p.weigh.reclaim
        ));
        self.op_emit(
            "invade",
            json!({"event": "capture", "t": self.frame / 30, "region": p.region, "kind": p.kind.name(), "why": why,
                   "x": p.center.x.round(), "z": p.center.z.round(), "spots": p.spots.len(), "reclaim": p.weigh.reclaim.round()}),
        );
        self.invade.captures.push(Capture {
            region: p.region,
            kind: p.kind,
            center: p.center,
            spots: p.spots.clone(),
            radius: p.radius,
            since: self.frame,
        });
    }

    pub fn invade_update(&mut self, frame: i32) {
        if (frame + self.offset) % PERIOD != 0 {
            return;
        }
        if self.dev_off("invade") {
            self.invade_end("switched off", false);
            self.invade.captures.clear();
            self.invade.builder = None;
            return;
        }
        if frame < FROM {
            return;
        }
        self.invade.cooldown.retain(|(_, f)| frame - *f < COOLDOWN);
        let Some(tier) = self.invade_tier() else { return };
        let pieces = {
            let Some(map) = self.terrain().and_then(|t| t.tier(tier)) else { return };
            self.invade_weigh(map)
        };
        if self.invade.tier.is_some_and(|old| old != tier) {
            // The first army can use a different tier from the builders.
            // Region ids belong to one tier and cannot carry over to another.
            self.invade_end("movement tier changed", false);
            self.invade.captures.clear();
            self.invade.cooldown.clear();
        }
        self.invade.tier = Some(tier);
        let piece = |r: usize| pieces.iter().find(|p| p.region == r);
        let ours = |ai: &Ai, spots: &[usize]| {
            spots.iter().all(|&i| ai.graph.metal_spots.get(i).is_none_or(|s| s.owned || s.allied))
        };
        // Captures: built, too slow, or defended again.
        let mut keep = Vec::new();
        for c in std::mem::take(&mut self.invade.captures) {
            let why = if ours(self, &c.spots) {
                self.invade.captured += 1;
                Some(("built", false))
            } else if frame - c.since > BUILD_LIMIT {
                Some(("not built in time", true))
            } else if piece(c.region).is_some_and(|p| p.kind != Kind::Open) {
                Some(("defended again", false))
            } else {
                None
            };
            match why {
                Some((why, cool)) => {
                    if cool {
                        self.invade.cooldown.push((c.region, frame));
                    }
                    self.log(&format!(
                        "invade: capture of {} piece {} ends: {why} after {} s",
                        c.kind.name(),
                        c.region,
                        (frame - c.since) / 30
                    ));
                    self.op_emit(
                        "invade",
                        json!({"event": "capture end", "t": frame / 30, "region": c.region, "why": why,
                                                   "secs": (frame - c.since) / 30}),
                    );
                }
                None => keep.push(c),
            }
        }
        self.invade.captures = keep;
        // The squad's target as it stands now.
        if let Some(t) = self.invade.cur.as_ref() {
            let (region, sid, since) = (t.region, t.squad, t.since);
            let now = piece(region);
            let state = self.invade_squad_state(sid);
            if now.is_none() || ours(self, &t.spots) {
                self.invade_end("piece ours or gone", false);
            } else if let Some(p) = now.filter(|p| p.kind == Kind::Open) {
                self.invade.clears += 1;
                self.invade_end("cleared", false);
                self.invade_capture(p, "cleared");
            } else if state.is_none() {
                self.invade_end("squad lost", true);
            } else if frame - since > SEND_LIMIT {
                self.invade_end("too slow to clear", true);
            } else if let (Some(p), Some((m, at, sp))) = (now, state) {
                let walk = self.invade_walk(tier, at, p.aim).map_or(f32::INFINITY, |d| d / sp);
                let net = evaluate(&p.weigh, m, walk).3;
                let t = self.invade.cur.as_mut().expect("checked");
                t.bad = if net < 0.0 { t.bad + 1 } else { 0 };
                (t.aim, t.d_metal, t.net) = (p.aim, p.weigh.d_metal, net);
                if t.bad >= 2 {
                    self.invade_end("no longer pays", true);
                }
            }
        }
        self.invade_choose(&pieces);
        self.invade_request_builder(&pieces);
    }

    /// Rank expansion against conquest, also while an earlier capture is
    /// being built. Removing captures from this comparison made a free
    /// piece stop competing as soon as it was queued.
    fn invade_choose(&mut self, pieces: &[Piece]) {
        let frame = self.frame;
        let free_net = pieces
            .iter()
            .filter(|p| p.kind == Kind::Open)
            .filter(|p| !self.invade.cooldown.iter().any(|c| c.0 == p.region))
            .filter_map(|p| p.best.map(|b| b.2))
            .filter(|net| net.is_finite())
            .fold(0.0f32, f32::max);
        if self.invade.cur.as_ref().is_some_and(|t| frame - t.since >= MIN_COMMIT && free_net > SWITCH * t.net.max(1.0))
        {
            self.invade_end("better free expansion", false);
        }
        let busy: Vec<usize> =
            self.invade.cooldown.iter().map(|c| c.0).chain(self.invade.captures.iter().map(|c| c.region)).collect();
        let open = |p: &&Piece| !busy.contains(&p.region) && p.best.is_some_and(|b| b.2.is_finite() && b.2 > 0.0);
        // Empty ground that pays: first for the constructors.
        if self.invade.captures.len() < MAX_CAPTURES
            && let Some(p) = (pieces.iter())
                .filter(open)
                .filter(|p| p.kind == Kind::Open)
                .max_by(|a, b| a.best.map_or(f32::MIN, |x| x.2).total_cmp(&b.best.map_or(f32::MIN, |x| x.2)))
        {
            self.invade_capture(p, "empty ground");
        }
        // Defended ground: a squad, unless the one sent is still committed.
        let best = (pieces.iter())
            .filter(open)
            .filter(|p| p.kind != Kind::Open)
            .filter_map(|p| match p.best {
                Some((Some(sid), m, net, pc)) => Some((p, sid, m, net, pc)),
                _ => None,
            })
            .max_by(|a, b| a.3.total_cmp(&b.3));
        self.invade.top = {
            let mut v: Vec<&Piece> = pieces.iter().filter(|p| p.best.is_some()).collect();
            v.sort_by(|a, b| b.best.map_or(f32::MIN, |x| x.2).total_cmp(&a.best.map_or(f32::MIN, |x| x.2)));
            v.into_iter()
                .take(5)
                .map(|p| {
                    let b = p.best.expect("filtered");
                    json!({"region": p.region, "kind": p.kind.name(), "x": p.center.x.round(), "z": p.center.z.round(),
                           "spots": p.spots.len(), "def_metal": p.weigh.d_metal.round(), "reclaim": p.weigh.reclaim.round(),
                           "squad": b.0.map(|s| format!("{s:?}")), "metal": b.1.round(), "net": b.2.round(), "p": (b.3 * 100.0).round() / 100.0})
                })
                .collect()
        };
        let Some((p, squad, metal, net, pc)) = best else { return };
        if net <= free_net {
            return;
        }
        if let Some(t) = self.invade.cur.as_ref() {
            if t.region == p.region || frame - t.since < MIN_COMMIT || net < SWITCH * t.net.max(1.0) {
                return;
            }
            self.invade_end("a better piece", false);
        }
        let (kind, region) = (p.kind, p.region);
        self.invade.picks += 1;
        self.log(&format!(
            "invade: target {} piece {region} at ({:.0},{:.0}), {} spots, defenders {:.0} metal, reclaim {:.0}; squad {squad:?} ({metal:.0} metal), P(clear) {pc:.2}, net {net:.0}",
            kind.name(),
            p.center.x,
            p.center.z,
            p.spots.len(),
            p.weigh.d_metal,
            p.weigh.reclaim,
        ));
        self.op_emit(
            "invade",
            json!({"event": "target", "t": frame / 30, "region": region, "kind": kind.name(),
                   "x": p.center.x.round(), "z": p.center.z.round(), "spots": p.spots.len(),
                   "def_metal": p.weigh.d_metal.round(), "reclaim": p.weigh.reclaim.round(),
                   "metal": metal.round(), "p": pc, "net": net.round()}),
        );
        self.invade.cur = Some(Target {
            region,
            kind,
            center: p.center,
            spots: p.spots.clone(),
            radius: p.radius,
            squad,
            aim: p.aim,
            since: frame,
            net,
            p: pc,
            d_metal: p.weigh.d_metal,
            bad: 0,
        });
    }

    /// Ground elmos to the target, including travel inside its region.
    fn invade_walk(&self, tier: Tier, from: Vec3, to: Vec3) -> Option<f32> {
        let map = self.terrain()?.tier(tier)?;
        Paths::new(map, pos(from), |_| false).distance_to(pos(to))
    }

    fn invade_weigh(&self, map: &TierMap) -> Vec<Piece> {
        let frame = self.frame;
        // Pieces: regions holding spots that are not ours.
        let mut regions: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
        for s in &self.graph.metal_spots {
            if s.owned || s.allied {
                continue;
            }
            if let Some(r) = map.region_at(pos(s.position)).or_else(|| map.region_near(pos(s.position), SNAP)) {
                regions.entry(r).or_default().push(s.index);
            }
        }
        if regions.is_empty() {
            return Vec::new();
        }
        // The enemy: armed buildings, armed units seen lately (no aircraft,
        // no anti-air only), with their metal by health left.
        struct Foe {
            pos: Vec3,
            metal: f32,
            reach: f32,
            mobile: bool,
            age: i32,
            speed: f32,
        }
        let foes: Vec<Foe> = (self.war.targets.values())
            .filter(|e| {
                !e.is_dead && e.identified && e.position != NULLPOS && e.can_attack() && !e.is_aa && !e.can_fly()
            })
            .filter(|e| e.is_static || frame - e.last_seen <= ARMY_SEEN)
            .filter_map(|e| {
                let d = e.ud?;
                Some(Foe {
                    pos: e.position,
                    metal: d.cost(self.metal) * e.get_health().clamp(0.05, 1.0),
                    reach: e.def_max_weapon_range(),
                    mobile: !e.is_static,
                    age: frame - e.last_seen,
                    speed: d.speed(),
                })
            })
            .collect();
        // The enemy's armies seen lately, biggest first.
        let movers: Vec<&Foe> = foes.iter().filter(|f| f.mobile).collect();
        let mut armies: Vec<(Vec3, f32, f32)> = Vec::new();
        let mut seen = vec![false; movers.len()];
        for i in 0..movers.len() {
            if seen[i] {
                continue;
            }
            seen[i] = true;
            let (mut g, mut k) = (vec![i], 0);
            while k < g.len() {
                let a = g[k];
                for j in 0..movers.len() {
                    if !seen[j] && distance(movers[a].pos, movers[j].pos) <= GROUP_LINK {
                        seen[j] = true;
                        g.push(j);
                    }
                }
                k += 1;
            }
            let n = g.len() as f32;
            let at = Vec3::xz(
                g.iter().map(|&i| movers[i].pos.x).sum::<f32>() / n,
                g.iter().map(|&i| movers[i].pos.z).sum::<f32>() / n,
            );
            let metal: f32 = g.iter().map(|&i| movers[i].metal).sum();
            let speed = g.iter().map(|&i| movers[i].speed).fold(f32::INFINITY, f32::min).max(10.0);
            if metal >= 150.0 {
                armies.push((at, metal, speed));
            }
        }
        armies.sort_by(|a, b| b.1.total_cmp(&a.1));
        armies.truncate(MAX_ARMIES);
        let army_paths: Vec<(Vec3, f32, f32, Paths)> =
            armies.into_iter().map(|(at, m, sp)| (at, m, sp, Paths::new(map, pos(at), |_| false))).collect();
        let base_paths = self.graph.get_enemy_base().map(|b| Paths::new(map, pos(b), |_| false));
        // Our sources: assault squads on this tier, and constructors.
        let squads: Vec<(SquadId, f32, Vec3, f32, Paths)> = (self.invade_squads().into_iter())
            .filter(|(sid, _, _, _)| {
                self.squads.get(*sid).and_then(|s| s.get_leader(&self.fighters)).and_then(|l| l.def()).and_then(tier_of)
                    == Some(map.tier)
            })
            .map(|(sid, m, at, sp)| (sid, m, at, sp, Paths::new(map, pos(at), |_| false)))
            .collect();
        // Constructors do not inherit the army's movement capabilities:
        // a bot constructor cannot follow a hover squad across a lake.
        // Aircraft use direct flight; ground builders use their own map.
        let cons = self.invade_builder_sources();
        let mut out = Vec::new();
        for (r, ks) in regions {
            let spots: Vec<&crate::graph::MetalSpot> =
                ks.iter().filter_map(|&k| self.graph.metal_spots.get(k)).collect();
            let theirs = spots.iter().filter(|s| s.hostile).count();
            let stake: f32 = spots.iter().filter(|s| s.hostile).map(|s| s.value).sum();
            let gain = stake + spots.iter().map(|s| s.value).sum::<f32>();
            let center = {
                let c = map.regions[r].center;
                Vec3::xz(c.x, c.z)
            };
            let radius = spots.iter().map(|s| distance(s.position, center)).fold(0.0, f32::max) + 300.0;
            let in_piece = |p: Vec3| map.region_near(pos(p), SNAP) == Some(r);
            let near = |p: Vec3| in_piece(p) || spots.iter().any(|s| distance(p, s.position) <= 600.0);
            let seen_for = if theirs > 0 { SEEN_HELD } else { SEEN_EMPTY };
            let defenders: Vec<&Foe> = foes
                .iter()
                .filter(|f| {
                    if f.mobile {
                        f.age <= seen_for && near(f.pos)
                    } else {
                        in_piece(f.pos) || spots.iter().any(|s| distance(f.pos, s.position) <= f.reach + 50.0)
                    }
                })
                .collect();
            let kind = if theirs > 0 {
                Kind::Held
            } else if !defenders.is_empty() {
                Kind::Contested
            } else {
                Kind::Open
            };
            let d_metal: f32 = defenders.iter().map(|f| f.metal).sum();
            let aim = if defenders.is_empty() {
                // An unguarded enemy mex still needs clearing before our
                // constructor can build. Aim at an actual occupied spot.
                spots.iter().find(|s| s.hostile).map_or(center, |s| s.position)
            } else {
                // The buildings first: they hold the ground.
                let pick: Vec<&&Foe> = if defenders.iter().any(|f| !f.mobile) {
                    defenders.iter().filter(|f| !f.mobile).collect()
                } else {
                    defenders.iter().collect()
                };
                let n = pick.len() as f32;
                Vec3::xz(pick.iter().map(|f| f.pos.x).sum::<f32>() / n, pick.iter().map(|f| f.pos.z).sum::<f32>() / n)
            };
            let reclaim: f32 = (self.cb.features_in(center, radius).into_iter())
                .filter_map(|f| f.def())
                .filter(|fd| fd.is_reclaimable())
                .map(|fd| fd.contained_resource(self.metal))
                .sum();
            let turret = if kind == Kind::Open { 0.0 } else { TURRET_COST };
            let structures = MEX_COST * spots.len() as f32 + turret;
            let Some((con, t_con)) =
                cons.iter().filter_map(|con| con.arrival(&spots).map(|eta| (con, eta))).min_by(|(a, ta), (b, tb)| {
                    a.plan
                        .is_some()
                        .cmp(&b.plan.is_some())
                        .then_with(|| (a.cost + gain * ta).total_cmp(&(b.cost + gain * tb)))
                })
            else {
                continue;
            };
            let build_metal = structures + con.cost;
            let threats: Vec<(f32, f32)> = (army_paths.iter())
                .filter(|(at, _, _, _)| !near(*at))
                .filter_map(|(_, m, sp, paths)| {
                    let d = paths.to_region(r);
                    d.is_finite().then(|| (d / sp, *m))
                })
                .collect();
            let base_walk =
                base_paths.as_ref().map(|p| p.to_region(r)).filter(|d| d.is_finite()).map(|d| d / BASE_SPEED);
            let weigh = Weigh {
                gain,
                d_metal,
                reclaim,
                build_metal,
                build_secs: structures / CON_BUILD_POWER,
                t_con,
                turret,
                threats,
                base_walk,
            };
            let mut best: Option<(Option<SquadId>, f32, f32, f32)> = None;
            if kind == Kind::Open {
                let (pc, _, _, net) = evaluate(&weigh, 0.0, 0.0);
                best = Some((None, 0.0, net, pc));
            }
            for (sid, m, _, sp, paths) in &squads {
                if kind == Kind::Open {
                    continue;
                }
                let Some(d) = paths.distance_to(pos(aim)) else { continue };
                let (pc, _, _, net) = evaluate(&weigh, *m, d / sp);
                if best.is_none_or(|b| net > b.2) {
                    best = Some((Some(*sid), *m, net, pc));
                }
            }
            out.push(Piece { region: r, kind, center, radius, spots: ks.clone(), aim, weigh, best, builder: con.plan });
        }
        out
    }

    pub(crate) fn invade_json(&self) -> Value {
        let v = &self.invade;
        let cur = v.cur.as_ref().map(|t| {
            json!({"region": t.region, "kind": t.kind.name(), "x": t.center.x.round(), "z": t.center.z.round(),
                   "spots": t.spots.len(), "squad": format!("{:?}", t.squad), "aim": [t.aim.x.round(), t.aim.z.round()],
                   "since": t.since / 30, "net": t.net.round(), "p": t.p, "def_metal": t.d_metal.round()})
        });
        let captures: Vec<Value> = (v.captures.iter())
            .map(|c| {
                json!({"region": c.region, "kind": c.kind.name(), "x": c.center.x.round(), "z": c.center.z.round(),
                            "spots": c.spots.len(), "since": c.since / 30})
            })
            .collect();
        json!({"enabled": !self.dev_off("invade"), "cur": cur, "captures": captures, "top": v.top, "picks": v.picks,
               "clears": v.clears, "captured": v.captured,
               "builder": v.builder.as_ref().map(|b| json!({"region": b.region, "factory": b.plan.factory,
                   "unit": self.defs.name(b.plan.def)}))})
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn weigh(d_metal: f32, threats: Vec<(f32, f32)>) -> Weigh {
        Weigh {
            gain: 4.0,
            d_metal,
            reclaim: 0.0,
            build_metal: 2.0 * MEX_COST + TURRET_COST,
            build_secs: (2.0 * MEX_COST + TURRET_COST) / CON_BUILD_POWER,
            t_con: 30.0,
            turret: TURRET_COST,
            threats,
            base_walk: None,
        }
    }

    #[test]
    fn the_metal_ratio_sets_the_odds_as_the_recorded_assaults_did() {
        // An even chance near 3.6x the defenders' metal (782 assaults).
        assert!((p_clear(3.6) - 0.5).abs() < 0.03);
        assert!(p_clear(8.0) > 0.75 && p_clear(1.0) < 0.15);
        assert!(lost_share(1.0) > lost_share(5.0));
        assert_eq!(p_clear(f32::INFINITY), 1.0);
    }

    #[test]
    fn a_bigger_squad_nets_more_until_its_time_costs_more() {
        let w = weigh(200.0, vec![]);
        let small = evaluate(&w, 300.0, 60.0).3;
        let right = evaluate(&w, 1200.0, 60.0).3;
        assert!(right > small, "{small} {right}");
        // The same squad an hour's walk away is not worth sending.
        assert!(evaluate(&w, 1200.0, 3600.0).3 < 0.0);
    }

    #[test]
    fn an_army_that_gets_there_first_spoils_the_hold() {
        let quiet = evaluate(&weigh(200.0, vec![]), 1200.0, 60.0);
        let raided = evaluate(&weigh(200.0, vec![(30.0, 10000.0)]), 1200.0, 60.0);
        let late = evaluate(&weigh(200.0, vec![(3000.0, 10000.0)]), 1200.0, 60.0);
        assert!(raided.2 < 0.2 && quiet.2 == 1.0 && late.2 == 1.0);
        assert!(raided.3 < quiet.3);
    }

    #[test]
    fn open_ground_needs_only_a_constructor() {
        let w = Weigh { d_metal: 0.0, turret: 0.0, ..weigh(0.0, vec![]) };
        let (p, lost, hold, net) = evaluate(&w, 0.0, 0.0);
        assert_eq!((p, lost, hold), (1.0, 0.0, 1.0));
        assert!(net > 0.0);
    }

    fn piece(region: usize, defended: bool, net: f32) -> Piece {
        Piece {
            region,
            kind: if defended { Kind::Held } else { Kind::Open },
            center: Vec3::xz(1000.0 * region as f32, 1000.0),
            radius: 300.0,
            spots: vec![region],
            aim: Vec3::xz(1000.0 * region as f32, 1000.0),
            weigh: weigh(if defended { 200.0 } else { 0.0 }, vec![]),
            best: Some((defended.then_some(7), if defended { 1200.0 } else { 0.0 }, net, 1.0)),
            builder: None,
        }
    }

    #[test]
    fn nearby_free_mexes_beat_a_profitable_remote_attack_on_every_pass() {
        let mut ai = crate::economy::test_ai_with(|_| {});
        let free = Weigh {
            build_metal: 2.0 * MEX_COST,
            build_secs: 2.0 * MEX_COST / CON_BUILD_POWER,
            turret: 0.0,
            t_con: 10.0,
            ..weigh(0.0, vec![])
        };
        let remote = Weigh { gain: 8.0, t_con: 150.0, ..weigh(200.0, vec![]) };
        let free_net = evaluate(&free, 0.0, 0.0).3;
        let attack_net = evaluate(&remote, 1200.0, 150.0).3;
        assert!(free_net > attack_net && attack_net > 0.0);
        let pieces = [piece(1, true, attack_net), piece(2, false, free_net)];
        for _ in 0..4 {
            ai.invade_choose(&pieces);
            assert!(ai.invade.cur.is_none(), "queuing the free ground must not remove it from the comparison");
            assert_eq!(ai.invade.captures.len(), 1);
            assert_eq!(ai.invade.captures[0].region, 2);
        }
        // Once the free mexes are taken, the profitable attack can proceed.
        ai.invade.captures.clear();
        ai.invade_choose(&pieces[..1]);
        assert_eq!(ai.invade.cur.as_ref().unwrap().region, 1);
    }

    #[test]
    fn a_more_valuable_attack_is_still_allowed_but_equal_value_prefers_free_ground() {
        for (free, attack) in [(100.0, true), (200.0, false), (-100.0, true)] {
            let mut ai = crate::economy::test_ai_with(|_| {});
            ai.invade_choose(&[piece(1, true, 200.0), piece(2, false, free)]);
            assert_eq!(ai.invade.cur.is_some(), attack);
        }
    }

    #[test]
    fn stalled_expansion_on_cooldown_does_not_block_conquest() {
        let mut ai = crate::economy::test_ai_with(|_| {});
        ai.invade.cooldown.push((2, ai.frame));
        ai.invade_choose(&[piece(1, true, 100.0), piece(2, false, 1000.0)]);
        assert_eq!(ai.invade.cur.as_ref().unwrap().region, 1);
        assert!(ai.invade.captures.is_empty());
    }

    #[test]
    fn an_existing_attack_yields_to_better_expansion_after_its_commitment() {
        let mut ai = crate::economy::test_ai_with(|_| {});
        let attack = [piece(1, true, 100.0)];
        ai.invade_choose(&attack);
        let pieces = [piece(1, true, 100.0), piece(2, false, 200.0)];
        ai.frame += MIN_COMMIT - 1;
        ai.invade_choose(&pieces);
        assert!(ai.invade.cur.is_some());
        ai.frame += 1;
        ai.invade_choose(&pieces);
        assert!(ai.invade.cur.is_none());
        assert!(ai.invade.cooldown.is_empty(), "deferral is not a failed attack");
    }

    #[test]
    fn full_capture_slots_do_not_make_free_ground_disappear_from_ranking() {
        let mut ai = crate::economy::test_ai_with(|_| {});
        let free: Vec<_> = (2..2 + MAX_CAPTURES).map(|r| piece(r, false, 200.0)).collect();
        for p in &free {
            ai.invade_capture(p, "empty ground");
        }
        let pieces = [piece(1, true, 100.0), piece(2, false, 200.0), piece(5, false, 300.0)];
        ai.invade_choose(&pieces);
        assert!(ai.invade.cur.is_none());
        assert_eq!(ai.invade.captures.len(), MAX_CAPTURES);
    }

    /// Bot (1), hover (2), or flying (3) constructor on the west shore.
    pub(super) fn coastal_ai(builder: i32) -> Ai {
        coastal_ai_with(builder, |_| {})
    }

    pub(super) fn coastal_ai_with(builder: i32, customize: impl FnOnce(&mut springai::ffi::SSkirmishAICallback)) -> Ai {
        unsafe extern "C" fn def(_: i32, id: i32) -> i32 {
            id
        }
        unsafe extern "C" fn health(_: i32, _: i32) -> f32 {
            1000.0
        }
        unsafe extern "C" fn speed(_: i32, _: i32) -> f32 {
            60.0
        }
        unsafe extern "C" fn bp(_: i32, _: i32) -> f32 {
            5.0
        }
        unsafe extern "C" fn zero(_: i32, _: i32) -> i32 {
            0
        }
        unsafe extern "C" fn depth(_: i32, _: i32) -> f32 {
            0.0
        }
        unsafe extern "C" fn slope(_: i32, _: i32) -> f32 {
            0.2
        }
        unsafe extern "C" fn class(_: i32, id: i32) -> i32 {
            if id == 2 { 2 } else { 1 }
        }
        unsafe extern "C" fn flies(_: i32, id: i32) -> bool {
            id == 3
        }
        unsafe extern "C" fn moves(_: i32, id: i32) -> bool {
            id != 3
        }
        unsafe extern "C" fn pos(_: i32, _: i32, out: *mut f32) {
            unsafe { std::ptr::copy_nonoverlapping([128.0, 0.0, 256.0].as_ptr(), out, 3) };
        }
        unsafe extern "C" fn features(_: i32, _: *mut f32, _: f32, _: bool, _: *mut i32, _: i32) -> i32 {
            0
        }
        unsafe extern "C" fn units(_: i32, _: *mut i32, _: i32) -> i32 {
            0
        }
        let mut ai = crate::economy::test_ai_with(|t| {
            t.Unit_getDef = Some(def);
            t.Unit_getHealth = Some(health);
            t.Unit_getPos = Some(pos);
            t.UnitDef_getSpeed = Some(speed);
            t.UnitDef_getBuildSpeed = Some(bp);
            t.UnitDef_getWeaponMounts = Some(zero);
            t.Unit_getWeapons = Some(zero);
            t.Unit_getCurrentCommands = Some(zero);
            t.UnitDef_getMinWaterDepth = Some(depth);
            t.UnitDef_getMaxWaterDepth = Some(depth);
            t.UnitDef_MoveData_getMaxSlope = Some(slope);
            t.UnitDef_MoveData_getSpeedModClass = Some(class);
            t.UnitDef_isAbleToFly = Some(flies);
            t.UnitDef_isMoveDataAvailable = Some(moves);
            t.getFeaturesIn = Some(features);
            t.getTeamUnits = Some(units);
            customize(t);
        });
        let data = zk_terrain::SpringMapData {
            map_width: 128,
            map_height: 64,
            slope_width: 64,
            slope_height: 32,
            slope_map: vec![0.0; 64 * 32],
            height_map: (0..128 * 64).map(|i| if (32..96).contains(&(i % 128)) { -100.0 } else { 20.0 }).collect(),
        };
        let terrain = zk_terrain::Terrain::build_with(
            &data,
            &[(Tier::Bot, Tier::Bot.zero_k_class()), (Tier::Hover, Tier::Hover.zero_k_class())],
        );
        ai.terrain = crate::terrain::TerrainModel::Ready { terrain, rebuild: None, split: 0 };
        ai.eco
            .workers
            .insert(builder, crate::economy::Worker::new(springai::Unit::from_id(ai.cb, builder).unwrap(), ai.team_id));
        ai
    }

    #[test]
    fn constructor_access_uses_its_own_tier_even_when_the_army_can_cross_water() {
        for builder in [1, 2, 3] {
            let mut ai = coastal_ai(builder);
            ai.graph.metal_spots.push(crate::graph::MetalSpot::new(0, 896.0, 0.0, 256.0, 2.0));
            let army_map = ai.terrain().unwrap().tier(Tier::Hover).unwrap();
            let pieces = ai.invade_weigh(army_map);
            if builder == 1 {
                assert!(pieces.is_empty(), "a bot constructor cannot cross the lake with the hover army");
            } else {
                assert_eq!(pieces.len(), 1, "hover and air constructors can reach the other shore");
                assert!(pieces[0].weigh.t_con >= 768.0 / 60.0, "crossing a single region still takes time");
            }
        }
    }

    #[test]
    fn free_expansion_starts_without_an_assault_squad() {
        let mut ai = coastal_ai(1);
        ai.graph.metal_spots.push(crate::graph::MetalSpot::new(0, 160.0, 0.0, 256.0, 2.0));
        assert!(ai.invade_squads().is_empty());
        assert_eq!(ai.invade_tier(), Some(Tier::Bot));
        ai.frame = FROM;
        ai.offset = 0;
        ai.set_feature_off("invade", false);
        ai.invade_update(FROM);
        assert!(ai.invade.cur.is_none());
        assert_eq!(ai.invade.captures.len(), 1);
        assert_eq!(ai.invade.captures[0].spots, vec![0]);
    }

    #[test]
    fn an_unguarded_enemy_mex_is_not_a_constructor_only_expansion() {
        let mut ai = coastal_ai(1);
        let mut spot = crate::graph::MetalSpot::new(0, 160.0, 0.0, 256.0, 2.0);
        spot.hostile = true;
        ai.graph.metal_spots.push(spot);
        let pieces = ai.invade_weigh(ai.terrain().unwrap().tier(Tier::Bot).unwrap());
        assert_eq!(pieces.len(), 1);
        assert_eq!(pieces[0].kind, Kind::Held);
        assert_eq!(pieces[0].weigh.d_metal, 0.0);
        assert_eq!(pieces[0].aim, Vec3::xz(160.0, 256.0));
        assert!(pieces[0].best.is_none(), "a constructor alone cannot replace an enemy mex");
        ai.invade_choose(&pieces);
        assert!(ai.invade.captures.is_empty());

        let mut enemy = piece(1, true, 200.0);
        enemy.weigh.d_metal = 0.0;
        ai.invade_choose(&[enemy, piece(2, false, 100.0)]);
        assert_eq!(ai.invade.cur.as_ref().unwrap().region, 1, "a squad must clear the enemy mex");
        assert_eq!(ai.invade.captures.len(), 1);
        assert_eq!(ai.invade.captures[0].region, 2);
    }
}
