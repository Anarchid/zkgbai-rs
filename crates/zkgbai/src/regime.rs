//! The map's regime: how much it is lanes, and how full it is.
//!
//! - **Openness**, per movement tier (see `zk_terrain::lanes`): a map whose
//!   terrain lets narrow fronts close off our metal (Hide and Seek, Eye of
//!   Horus) reads as lanes; an open map (Comet Catcher, Agalta) does not.
//!   The tier that decides is the enemy ground army's. Missiles' siege
//!   readiness and the operator's `regime` view read it.
//! - **Density**: standing metal per usable area (all allies and the enemy's
//!   as known: the F12 regime variable `D_known`, square kilo-elmos). As
//!   the map fills up, raids stop paying and attrition does. Shared gate
//!   defenses, missile silos, colonists and a 1v1's opening scouts read
//!   it, against the operator's `density_ref` lever where they need a
//!   scale.
//!
//! (A fixed-defense budget built on these, `posture`, lost the 2026-10-27
//! Eye of Horus mirror and was removed.)

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
/// taken while its usable area is unknown: an ordinary map's (Otago 1.5,
/// Hide and Seek 2.6, Comet 3.7).
const RICH: f32 = 3.0;

/// Standing metal per square kilo-elmo at which density counts fully (the
/// F12 regime label).
pub const DENSITY_REF: f32 = 1500.0;

/// Enemy mobiles count towards density while seen this recently (F12).
const MOBILE_RECENT: i32 = 30 * 60;
/// Unit costs from here up are placeholders (terraform units), not metal.
const NOMINAL_COST: f32 = 20000.0;

#[derive(Default)]
pub struct Regime {
    job: Option<mpsc::Receiver<Vec<Lanes>>>,
    started: bool,
    /// Lanes per tier, once measured.
    pub lanes: Vec<Lanes>,
    /// Index in `lanes` of the tier that decides (the enemy ground army's).
    pub tier: Option<usize>,
    /// Standing metal per square kilo-elmo, all allies and known enemies.
    pub density: f32,
    /// The map's own metal density (spot metal per usable area).
    pub richness: f32,
    /// Our standing metal.
    pub standing: f32,
}

impl Regime {
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

/// Own standing metal for reporting, and the whole alliance for density.
/// Friendly-unit enumeration includes our team, so sum it only once.
fn standing_metal(units: impl Iterator<Item = (i32, f32)>, team: i32) -> (f32, f32) {
    units
        .filter(|&(_, cost)| cost < NOMINAL_COST)
        .fold((0.0, 0.0), |(own, all), (owner, cost)| (own + if owner == team { cost } else { 0.0 }, all + cost))
}

impl Ai {
    /// The density at which the map counts as dense: the operator's lever,
    /// else [`DENSITY_REF`].
    pub(crate) fn regime_density_ref(&self) -> f32 {
        self.op.over.density_ref.unwrap_or(DENSITY_REF)
    }

    pub fn regime_update(&mut self, frame: i32) {
        if frame % PERIOD != (self.offset + 13) % PERIOD {
            return;
        }
        self.regime_start();
        if let Some(rx) = &self.regime.job
            && let Ok(lanes) = rx.try_recv()
        {
            self.regime.job = None;
            for l in &lanes {
                self.log(&format!(
                    "regime: {} lanes: openness {:.2} (perimeter {:.2}, closure {:.2}), {} fronts, {} sites, keeps {} of our {} spots",
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
            self.regime.lanes = lanes;
            self.regime_emit("lanes");
        }
        self.regime_measure();
        if frame % REPORT == (self.offset + 13) % REPORT {
            self.regime_emit("assess");
        }
    }

    /// Measure the lanes once, on a thread, when the terrain model and both
    /// starts are known.
    fn regime_start(&mut self) {
        if self.regime.started {
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
        self.regime.started = true;
        match spawned {
            Ok(_) => self.regime.job = Some(rx),
            Err(e) => self.log(&format!("regime: cannot start the lanes analysis ({e})")),
        }
    }

    /// Standing metal, the map's richness and density, now.
    fn regime_measure(&mut self) {
        let frame = self.frame;
        // Every unit at full cost, nanoframes too (F12), but not terraform
        // units: their nominal cost is a placeholder in the tens of thousands.
        let (standing, friendly) = standing_metal(
            self.cb.friendly_units().iter().filter_map(|u| Some((u.team(), u.def()?.cost(self.metal)))),
            self.team_id,
        );
        let enemy: f32 = (self.war.targets.values())
            .filter(|e| !e.is_dead && (e.is_static || frame - e.last_seen <= MOBILE_RECENT))
            .map(|e| e.value)
            .filter(|&v| v < NOMINAL_COST)
            .sum();
        // The deciding tier: the enemy ground army's, or bots before it is
        // seen.
        let tiers = self.enemy_ground_tiers();
        let tier = (tiers.iter()).find_map(|t| self.regime.lanes.iter().position(|l| l.tier == *t));
        self.regime.tier = tier.or_else(|| self.regime.lanes.iter().position(|l| l.tier == Tier::Bot));
        // Usable ground: the deciding tier's, measured with its lanes, or
        // the bots' terrain model's before they are.
        let usable = self.regime.decisive().map(|l| l.usable_km2).or_else(|| {
            let m = self.terrain()?.tier(Tier::Bot)?;
            Some(m.passable_fraction * (m.width * m.height) as f32 * zk_terrain::CELL * zk_terrain::CELL / 1e6)
        });
        let usable = usable.unwrap_or(0.0);
        let metal: f32 = self.graph.metal_spots.iter().map(|s| s.value).sum();
        self.regime.richness = if usable > 0.0 { metal / usable } else { RICH };
        self.regime.density = if usable > 0.0 { (friendly + enemy) / usable } else { 0.0 };
        self.regime.standing = standing;
    }

    fn regime_emit(&self, event: &str) {
        let p = &self.regime;
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
            "regime",
            json!({
                "event": event, "t": self.frame / 30,
                "tier": p.decisive().map(|l| l.tier.name()), "laneness": p.laneness(),
                "richness": p.richness, "density": p.density.round(), "standing": p.standing.round(),
                "lanes": if event == "lanes" { json!(lanes) } else { json!(null) },
            }),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn density_counts_separate_allies_just_as_it_counts_a_merged_team() {
        let split = [(1, 3000.0), (2, 4000.0), (3, 5000.0), (2, NOMINAL_COST)];
        let merged = split.map(|(_, cost)| (1, cost));
        assert_eq!(standing_metal(split.into_iter(), 1), (3000.0, 12000.0));
        assert_eq!(standing_metal(merged.into_iter(), 1), (12000.0, 12000.0));
        assert_eq!(standing_metal([(1, 3000.0)].into_iter(), 1), (3000.0, 3000.0));
    }

    #[test]
    fn lane_maps_read_as_lanes_and_open_maps_do_not() {
        // Bots' openness from the calibration (`thresholds`).
        assert!(laneness(Tier::Bot, 0.38) > 0.9, "Hide and Seek");
        assert!(laneness(Tier::Bot, 0.43) > 0.8, "Eye of Horus");
        assert!(laneness(Tier::Bot, 0.84) < 0.05, "Agalta");
        // Vehicles find the same map lane-ier.
        assert!(laneness(Tier::Vehicle, 0.36) > laneness(Tier::Bot, 0.67), "Otago");
        assert_eq!(laneness(Tier::Bot, 0.2), 1.0);
        assert_eq!(laneness(Tier::Bot, 0.95), 0.0);
    }
}
