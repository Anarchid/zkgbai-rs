//! Counting cards: how much metal the enemy could have that we know
//! nothing of — a hidden nuke, say.
//!
//! The enemy's production is estimated symmetrically: what we make, scaled
//! by the metal-spot value it holds against ours. Spots seen to be its
//! count in full; spots only inferred to be (see the territory shadows,
//! which take in much neutral ground) at a tenth; both sides count a base
//! as well, so that early the estimate is near our own income. Against
//! that stand the enemy units we have seen and not seen die, and what we
//! have destroyed. What is left is unaccounted for — mostly economy
//! buildings we never get to see, and the enemy should have as much of
//! those, for what it makes, as we do. What it cannot account for beyond
//! that is suspicious; when that is as much as a nuke silo costs, and more
//! than the estimate could be off by late on, and we can spare an
//! anti-nuke, it is anti time.
//!
//! Fitted to the arena's numbers (310 estimates over 8 self-play games on
//! 7 maps), the income estimate is within 0.76-1.32 of the truth (10th to
//! 90th percentile) at every stage of the game. The earlier one, counting
//! inferred spots in full and scaling by 1/2.16, ran 1.46 times high early
//! and 0.72 late, and built anti-nukes at ten minutes against no nuke.
//! Replayed with the fit, games without nukes peaked at 3.8k suspicious
//! metal; a silo planned at 10 minutes showed as 9.5k by 17.

use std::collections::BTreeMap;

use crate::Ai;

/// Frames between estimates.
const PERIOD: i32 = 30;

/// Metal a team starts with besides its free commander and factory.
const START_METAL: f32 = 1000.0;

/// Spot value each side counts besides its spots, the weight of spots only
/// inferred to be the enemy's, and what the enemy really makes for what
/// the estimate says (all fitted, see above).
const BASE_VALUE: f32 = 24.0;
pub(super) const INFERRED_WEIGHT: f32 = 0.1;
const CALIBRATION: f32 = 1.03;

/// Suspicious metal that makes a hidden nuke a worry (a silo's cost, or
/// more than the estimate's error: a share of all the enemy made), and the
/// income from which we can spare an anti-nuke.
pub const ANTI_SUSPICION: f32 = 8000.0;
const ANTI_SHARE: f32 = 0.15;
const ANTI_INCOME: f32 = 35.0;

/// Frames between summaries in the log.
const LOG_PERIOD: i32 = 1800;

#[derive(Default)]
pub struct Intel {
    /// Enemy units and buildings identified and not known dead: cost.
    known: BTreeMap<i32, f32>,
    /// Cost of the enemy units we have seen die.
    pub killed: f32,
    /// The enemy's estimated production so far, and its rate (metal/s).
    pub produced: f32,
    pub income: f32,
    /// Our own production so far, and the share of it in our economy's
    /// buildings.
    ours: f32,
    infrastructure: f32,
    last_frame: i32,
    /// Spot values for the log: ours, the enemy's seen, inferred.
    spots: (f32, f32, f32),
}

impl Intel {
    /// What the enemy has produced that we cannot account for.
    pub fn unaccounted(&self) -> f32 {
        self.produced - self.killed - self.known.values().sum::<f32>()
    }

    /// What is unaccounted for beyond economy buildings in the share we
    /// have ours in.
    pub fn suspicious(&self) -> f32 {
        self.unaccounted() - self.infrastructure * self.produced
    }
}

impl Ai {
    pub fn intel_update(&mut self, frame: i32) {
        if (frame + self.offset + 7) % PERIOD != 0 {
            return;
        }
        let metal = self.metal;
        // Everything identified, but commanders (they come free).
        for (&id, e) in &self.war.targets {
            if let Some(ud) = e.ud.filter(|_| e.identified && !e.is_com) {
                self.intel.known.entry(id).or_insert_with(|| ud.cost(metal));
            }
        }

        // Their production, by ours and the spots each side holds.
        let spots = &self.graph.metal_spots;
        let ours: f32 = spots.iter().filter(|s| s.owned).map(|s| s.value).sum();
        let theirs: f32 = spots.iter().filter(|s| !s.owned && (s.hostile || s.enemy_shadowed)).map(|s| s.value).sum();
        let seen: f32 = spots.iter().filter(|s| !s.owned && s.hostile).map(|s| s.value).sum();
        let held = seen + INFERRED_WEIGHT * (theirs - seen);
        let income = CALIBRATION * self.eco.static_income * (held + BASE_VALUE) / (ours + BASE_VALUE);
        let infrastructure = self.eco_infrastructure_value();
        let intel = &mut self.intel;
        if intel.last_frame == 0 {
            intel.produced = START_METAL;
            intel.ours = START_METAL;
        }
        let dt = (frame - intel.last_frame) as f32 / 30.0;
        intel.produced += income * dt;
        intel.ours += self.eco.static_income * dt;
        intel.infrastructure = (infrastructure / intel.ours).clamp(0.0, 0.9);
        intel.income = income;
        intel.last_frame = frame;
        intel.spots = (ours, seen, theirs - seen);

        if frame / LOG_PERIOD != (frame - PERIOD) / LOG_PERIOD {
            let i = &self.intel;
            self.log(&format!(
                "intel: enemy made ~{:.0} metal (~{:.1}/s); seen alive {:.0}, destroyed {:.0}, unaccounted {:.0}, \
                 of which suspicious {:.0} (we keep {:.0}% in buildings); spots ours {:.1}, theirs seen {:.1}, \
                 inferred {:.1}; our income {:.1}",
                i.produced,
                i.income,
                i.known.values().sum::<f32>(),
                i.killed,
                i.unaccounted(),
                i.suspicious(),
                100.0 * i.infrastructure,
                i.spots.0,
                i.spots.1,
                i.spots.2,
                self.eco.static_income
            ));
        }
    }

    /// An enemy unit died.
    pub fn intel_enemy_destroyed(&mut self, id: i32, cost: Option<f32>) {
        match self.intel.known.remove(&id) {
            Some(c) => self.intel.killed += c,
            None => self.intel.killed += cost.unwrap_or(0.0),
        }
    }

    /// Whether the enemy could be hiding a nuke and we can afford to guard
    /// against it.
    pub fn intel_wants_anti(&self) -> bool {
        !self.dev_off("intel")
            && self.intel.suspicious() >= ANTI_SUSPICION.max(ANTI_SHARE * self.intel.produced)
            && self.eco.adjusted_income >= ANTI_INCOME
            && self.eco.static_income >= 0.8 * self.intel.income
    }
}
