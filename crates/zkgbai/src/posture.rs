//! Posture: how much of our standing metal goes to fixed defenses, and
//! where they stand.
//!
//! Two things raise the share (the user's design, 2026-10-25):
//!
//! - **Low openness.** A map whose terrain lets narrow fronts close off our
//!   metal (see `zk_terrain::lanes`: Hide and Seek, Eye of Horus) rewards
//!   turrets at the fronts; an open map (Comet Catcher, Agalta) rewards
//!   raiding and naked expansion. The openness that counts is the enemy
//!   ground army's tier's. It is tempered by the map's own metal density
//!   (spot metal per usable area): a lane map with little metal to hold
//!   (Otago) earns less.
//! - **High density.** Standing metal per usable area (both sides', the
//!   enemy's as known: the F12 regime variable `D_known`, square
//!   kilo-elmos): as the map fills up, raids stop paying and attrition does.
//!
//! `share = floor + lane x laneness x richness + density x min(D / ref, 1)`,
//! at most `cap`, all five levers (`defense_*`, `density_ref`). The economy
//! keeps the posture's own turrets at `share` of our standing metal, on top
//! of what the rest of the AI builds (see `eco_defend_posture`): each turret
//! where it takes the most raid exposure off our metal per metal, a front
//! site for the spots whose raid routes cross it, a point site for a mex
//! and its neighbours. Off by default since 2026-10-27 (`enable=posture`,
//! see `ai::OFF_BY_DEFAULT`); off, the economy keeps the terrain analysis's
//! chokepoint fortification and no budget.

use std::sync::mpsc;

use serde_json::json;
use zk_terrain::Tier;
use zk_terrain::lanes::{self, LaneParams, Lanes};

use crate::Ai;
use crate::terrain::pos;

/// Frames between assessments.
const PERIOD: i32 = 30;
/// Frames between journal records.
const REPORT: i32 = 30 * 60;
/// Tiers whose lanes are measured (spiders climb round everything).
const TIERS: [Tier; 4] = [Tier::Bot, Tier::Vehicle, Tier::Hover, Tier::Amphib];

/// Openness at or below which a map counts fully as lanes, and at or above
/// which not at all, per tier (`zkgbai-runs/terrain/openness/NOTE.md`).
/// Bots: Hide and Seek 0.38, Eye of Horus 0.43; Otago 0.67; Comet Catcher
/// 0.82, Agalta 0.84. Vehicles and hovers find every map lane-ier (slopes
/// stop them): Hide and Seek 0.28, Eye of Horus 0.37, Otago 0.36; Comet 0.59,
/// Agalta 0.84.
pub fn thresholds(tier: Tier) -> (f32, f32) {
    match tier {
        Tier::Vehicle | Tier::Hover => (0.30, 0.65),
        _ => (0.35, 0.85),
    }
}
/// The map's own metal density (metal per second per square kilo-elmo)
/// that counts as ordinary: Otago 1.5, Hide and Seek 2.6, Comet 3.7.
const RICH: f32 = 3.0;

/// Lever defaults.
/// Scantily clad by default (user, 2026-10-26: the posture is there so we
/// are not naked; we would rather be attacking), and the turrets' own
/// spacing (`MAX_OVERLAP` in `eco_defend_posture`) usually binds first.
pub const FLOOR: f32 = 0.0;
pub const LANE_WEIGHT: f32 = 0.15;
pub const DENSITY_WEIGHT: f32 = 0.10;
pub const CAP: f32 = 0.25;
/// Standing metal per square kilo-elmo at which density counts fully (the
/// F12 regime label).
pub const DENSITY_REF: f32 = 1500.0;

/// Enemy mobiles count towards density while seen this recently (F12).
const MOBILE_RECENT: i32 = 30 * 60;
/// Unit costs from here up are placeholders (terraform units), not metal.
const NOMINAL_COST: f32 = 20000.0;
/// An order nobody started is dropped after this long (frames), and its
/// site avoided for [`SHUN`] within [`SHUN_RADIUS`].
const STALE: i32 = 30 * 90;
const SHUN: i32 = 30 * 180;
const SHUN_RADIUS: f32 = 250.0;

#[derive(Default)]
pub struct Posture {
    job: Option<mpsc::Receiver<Vec<Lanes>>>,
    started: bool,
    /// Lanes per tier, once measured.
    pub lanes: Vec<Lanes>,
    /// Index in `lanes` of the tier that decides (the enemy ground army's).
    pub tier: Option<usize>,
    /// Standing metal per square kilo-elmo, ours and the enemy's as known.
    pub density: f32,
    /// The map's own metal density (spot metal per usable area).
    pub richness: f32,
    /// Share of our standing metal fixed defenses should hold.
    pub share: f32,
    /// Our standing metal, and what of it is the posture's own defense
    /// (built, under way, or ordered lately): the turrets the rest of the AI
    /// builds for its own reasons (incursions, factories, fusions) do not
    /// count against the share.
    pub standing: f32,
    pub defense: f32,
    /// All fixed defenses' metal, the rest of the AI's included (reported).
    pub all_defense: f32,
    /// The turrets the posture ordered.
    pub orders: Vec<PostureOrder>,
    /// Sites of orders nobody took up, avoided until a frame.
    pub shunned: Vec<(springai::Vec3, i32)>,
    pub stats: PostureStats,
}

/// A turret the posture ordered.
pub struct PostureOrder {
    pub task: crate::economy::TaskId,
    pub site: springai::Vec3,
    pub cost: f32,
    pub issued: i32,
    /// The turret, once its nanoframe stands.
    pub unit: Option<springai::Unit>,
}

#[derive(Default)]
pub struct PostureStats {
    pub front_orders: u32,
    pub point_orders: u32,
    /// Orders dropped because nobody started them in time.
    pub stale: u32,
}

impl Posture {
    /// The lanes of the deciding tier.
    pub fn decisive(&self) -> Option<&Lanes> {
        self.tier.and_then(|i| self.lanes.get(i))
    }

    /// How much the map counts as lanes, 0 to 1.
    pub fn laneness(&self) -> f32 {
        self.decisive().map_or(0.0, |l| laneness(l.tier, l.openness))
    }
}

/// How much `openness` counts as lanes for `tier` (see [`thresholds`]).
pub fn laneness(tier: Tier, openness: f32) -> f32 {
    let (lane, open) = thresholds(tier);
    ((open - openness) / (open - lane)).clamp(0.0, 1.0)
}

/// The map's own metal density as a multiplier: half on a barren map, up to
/// 1.25 on a rich one.
fn richness_factor(richness: f32) -> f32 {
    (0.5 + 0.5 * richness / RICH).clamp(0.5, 1.25)
}

/// The share formula (see the module docs).
pub fn share(laneness: f32, richness: f32, density: f32, levers: &Levers) -> f32 {
    let dense = if levers.density_ref > 0.0 { (density / levers.density_ref).min(1.0) } else { 0.0 };
    (levers.floor + levers.lane * laneness * richness_factor(richness) + levers.density * dense).clamp(0.0, levers.cap)
}

/// The budget's levers, with the operator's overrides applied.
pub struct Levers {
    pub floor: f32,
    pub lane: f32,
    pub density: f32,
    pub cap: f32,
    pub density_ref: f32,
}

impl Ai {
    pub(crate) fn posture_levers(&self) -> Levers {
        let o = &self.op.over;
        Levers {
            floor: o.defense_floor.unwrap_or(FLOOR),
            lane: o.defense_lane.unwrap_or(LANE_WEIGHT),
            density: o.defense_density.unwrap_or(DENSITY_WEIGHT),
            cap: o.defense_cap.unwrap_or(CAP),
            density_ref: o.density_ref.unwrap_or(DENSITY_REF),
        }
    }

    /// Whether the posture decides fixed defenses.
    pub fn posture_on(&self) -> bool {
        !self.dev_off("posture")
    }

    pub fn posture_update(&mut self, frame: i32) {
        if frame % PERIOD != (self.offset + 13) % PERIOD {
            return;
        }
        // Density is read beyond the posture (the spider factory's raider
        // phase, the energy ladder), so it is measured with the posture off.
        if !self.posture_on() {
            self.posture_measure();
            return;
        }
        self.posture_start();
        if let Some(rx) = &self.posture.job
            && let Ok(lanes) = rx.try_recv()
        {
            self.posture.job = None;
            for l in &lanes {
                self.log(&format!(
                    "posture: {} lanes: openness {:.2} (perimeter {:.2}, closure {:.2}), {} fronts, {} sites, keeps {} of our {} spots",
                    l.tier.name(),
                    l.openness,
                    l.perimeter.openness,
                    l.closure,
                    l.perimeter.fronts.len(),
                    l.perimeter.fronts.iter().map(|f| f.sites.len()).sum::<usize>(),
                    l.perimeter.kept.iter().filter(|&&k| k).count(),
                    l.ours.iter().filter(|&&o| o).count(),
                ));
            }
            self.posture.lanes = lanes;
            self.posture_emit("lanes");
        }
        self.posture_assess();
        if frame % REPORT == (self.offset + 13) % REPORT {
            self.posture_emit("assess");
        }
    }

    /// Measure the lanes once, on a thread, when the terrain model and both
    /// starts are known.
    fn posture_start(&mut self) {
        if self.posture.started {
            return;
        }
        let Some((ally, enemy)) = self.strategy_bases() else { return };
        let Some(terrain) = self.terrain() else { return };
        let maps: Vec<_> = TIERS.iter().filter_map(|&t| terrain.tier(t).cloned()).collect();
        let spots: Vec<zk_terrain::Pos> = self.graph.metal_spots.iter().map(|s| pos(s.position)).collect();
        let (ours, theirs) = (pos(ally), pos(enemy));
        let (tx, rx) = mpsc::channel();
        // Pure computation: the thread must never call into the engine.
        let spawned = std::thread::Builder::new().name("zkgbai-lanes".into()).spawn(move || {
            let lanes: Vec<Lanes> = (maps.iter())
                .filter_map(|m| lanes::analyse(m, ours, theirs, &spots, &LaneParams::for_tier(m.tier)))
                .collect();
            let _ = tx.send(lanes);
        });
        self.posture.started = true;
        match spawned {
            Ok(_) => self.posture.job = Some(rx),
            Err(e) => self.log(&format!("posture: cannot start the lanes analysis ({e})")),
        }
    }

    /// Standing metal, density and the share, now.
    fn posture_assess(&mut self) {
        self.posture_measure();
        self.posture.defense = self.posture_ledger();
        self.posture.all_defense = self.eco_defense_value();
        let levers = self.posture_levers();
        self.posture.share = share(self.posture.laneness(), self.posture.richness, self.posture.density, &levers);
    }

    /// Standing metal, the map's richness and density, now.
    fn posture_measure(&mut self) {
        let frame = self.frame;
        // Every unit at full cost, nanoframes too (F12), but not terraform
        // units: their nominal cost is a placeholder in the tens of thousands.
        let standing: f32 = (self.cb.team_units().iter())
            .filter_map(|u| u.def())
            .map(|d| d.cost(self.metal))
            .filter(|&c| c < NOMINAL_COST)
            .sum();
        let enemy: f32 = (self.war.targets.values())
            .filter(|e| !e.is_dead && (e.is_static || frame - e.last_seen <= MOBILE_RECENT))
            .map(|e| e.value)
            .filter(|&v| v < NOMINAL_COST)
            .sum();
        // The deciding tier: the enemy ground army's, or bots before it is
        // seen.
        let tiers = self.enemy_ground_tiers();
        let tier = (tiers.iter()).find_map(|t| self.posture.lanes.iter().position(|l| l.tier == *t));
        self.posture.tier = tier.or_else(|| self.posture.lanes.iter().position(|l| l.tier == Tier::Bot));
        // Usable ground: the deciding tier's, measured with its lanes, or
        // the bots' terrain model's before they are.
        let usable = self.posture.decisive().map(|l| l.usable_km2).or_else(|| {
            let m = self.terrain()?.tier(Tier::Bot)?;
            Some(m.passable_fraction * (m.width * m.height) as f32 * zk_terrain::CELL * zk_terrain::CELL / 1e6)
        });
        let usable = usable.unwrap_or(0.0);
        let metal: f32 = self.graph.metal_spots.iter().map(|s| s.value).sum();
        self.posture.richness = if usable > 0.0 { metal / usable } else { RICH };
        self.posture.density = if usable > 0.0 { (standing + enemy) / usable } else { 0.0 };
        self.posture.standing = standing;
    }

    /// Metal the posture's turrets should hold now, beyond what they do.
    pub(crate) fn posture_deficit(&self) -> f32 {
        self.posture.share * self.posture.standing - self.posture.defense
    }

    /// The posture's own turrets' metal: standing ones, nanoframes, and
    /// orders not yet started. An order nobody started within [`STALE`] is
    /// dropped and its site shunned for [`SHUN`] (a crew cannot get there,
    /// or will not), so the budget goes elsewhere. Forgets the dead.
    fn posture_ledger(&mut self) -> f32 {
        let frame = self.frame;
        let team = self.team_id;
        let tasks = &self.eco.tasks;
        let mut value = 0.0;
        let mut stale = Vec::new();
        self.posture.orders.retain_mut(|o| {
            if o.unit.is_none() {
                o.unit = tasks.get(o.task).and_then(|t| t.as_construction()).and_then(|c| c.target);
            }
            match o.unit {
                Some(u) if u.health() > 0.0 && u.team() == team => {
                    value += o.cost;
                    true
                }
                Some(_) => false,
                None if tasks.get(o.task).is_some() => {
                    if frame - o.issued < STALE {
                        value += o.cost;
                        true
                    } else {
                        stale.push((o.task, o.site));
                        false
                    }
                }
                None => false,
            }
        });
        for (task, site) in stale {
            self.logistics_close(task, "stale");
            self.eco.eco_logistics_drop(task);
            self.posture.shunned.push((site, frame + SHUN));
            self.posture.stats.stale += 1;
        }
        self.posture.shunned.retain(|&(_, until)| until > frame);
        value
    }

    /// Whether a site is near one shunned.
    pub(crate) fn posture_shuns(&self, p: springai::Vec3) -> bool {
        self.posture.shunned.iter().any(|&(s, _)| crate::util::distance(s, p) < SHUN_RADIUS)
    }

    /// The posture's orders nobody has started yet.
    pub(crate) fn posture_waiting(&self) -> usize {
        (self.posture.orders.iter())
            .filter(|o| o.unit.is_none())
            .filter(|o| {
                self.eco.tasks.get(o.task).and_then(|t| t.as_construction()).is_some_and(|c| c.target.is_none())
            })
            .count()
    }

    /// Record a turret the posture ordered, and give it to the project
    /// tracker (see `logistics`) so a crew goes for it: far sites lose to
    /// nearer jobs otherwise. Worth `value` metal, less a payback share of
    /// it for every second it is late.
    pub(crate) fn posture_ordered(
        &mut self,
        task: crate::economy::TaskId,
        site: springai::Vec3,
        cost: f32,
        value: f32,
        why: &str,
    ) {
        self.posture.orders.push(PostureOrder { task, site, cost, issued: self.frame, unit: None });
        if !self.dev_off("logistics") && !self.dev_off("clients") {
            let now = self.frame as f32 / 30.0;
            let worth = crate::logistics::Worth::Stream { value, from: now, rate: value / crate::logistics::PAYBACK };
            self.logistics_open(task, crate::logistics::Kind::Posture, worth, why, 0.0);
        }
    }

    fn posture_emit(&self, event: &str) {
        let p = &self.posture;
        let lanes: Vec<serde_json::Value> = (p.lanes.iter())
            .map(|l| {
                json!({
                    "tier": l.tier.name(), "openness": l.openness, "perimeter": l.perimeter.openness,
                    "closure": l.closure, "kept": l.perimeter.kept.iter().filter(|&&k| k).count(),
                    "ours": l.ours.iter().filter(|&&o| o).count(),
                    "fronts": l.perimeter.fronts.iter().map(|f| json!({
                        "x": f.center.x.round(), "z": f.center.z.round(), "span": f.span.round(),
                        "sites": f.sites.iter().map(|s| [s.x.round(), s.z.round()]).collect::<Vec<_>>(),
                    })).collect::<Vec<_>>(),
                })
            })
            .collect();
        self.op_emit(
            "posture",
            json!({
                "event": event, "t": self.frame / 30,
                "tier": p.decisive().map(|l| l.tier.name()), "laneness": p.laneness(),
                "richness": p.richness, "density": p.density.round(), "share": p.share,
                "standing": p.standing.round(), "defense": p.defense.round(), "all_defense": p.all_defense.round(),
                "orders_held": p.orders.len(),
                "front_orders": p.stats.front_orders, "point_orders": p.stats.point_orders, "stale": p.stats.stale,
                "lanes": if event == "lanes" { json!(lanes) } else { json!(null) },
            }),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn defaults() -> Levers {
        Levers { floor: FLOOR, lane: LANE_WEIGHT, density: DENSITY_WEIGHT, cap: CAP, density_ref: DENSITY_REF }
    }

    #[test]
    fn lanes_and_density_raise_the_share() {
        let l = defaults();
        // Bots' openness from the calibration, early (no density yet).
        let bot = |o| laneness(Tier::Bot, o);
        let veh = |o| laneness(Tier::Vehicle, o);
        let hide = share(bot(0.38), 2.6, 0.0, &l);
        let horus = share(bot(0.43), 3.1, 0.0, &l);
        let otago = share(bot(0.67), 1.5, 0.0, &l);
        let comet = share(bot(0.82), 3.7, 0.0, &l);
        let agalta = share(bot(0.84), 3.1, 0.0, &l);
        assert!(hide > 0.1 && horus > 0.1, "lane maps hold a tenth or more: {hide} {horus}");
        assert!(otago > 0.02 && otago < hide / 2.0, "Otago some, tempered by its thin metal: {otago}");
        assert!(comet < 0.03 && agalta < 0.01, "open maps next to none: {comet} {agalta}");
        // Against vehicles: Hide and Seek fully lanes, Comet mostly open,
        // Otago more than against bots.
        assert!(share(veh(0.28), 2.6, 0.0, &l) > 0.12);
        assert!(share(veh(0.59), 3.7, 0.0, &l) < 0.04);
        assert!(share(veh(0.36), 1.5, 0.0, &l) > otago);
        // Density raises every map's share, up to the cap.
        assert!(share(bot(0.84), 3.1, 1500.0, &l) >= 0.1);
        assert!(share(bot(0.38), 2.6, 3000.0, &l) <= CAP);
    }
}
