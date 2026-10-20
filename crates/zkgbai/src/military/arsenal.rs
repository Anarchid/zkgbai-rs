//! What our forces can engage. Ground units only get where their movement
//! tier does (in weapon range), and most weapons cannot hit anything under
//! water: only water weapons — torpedoes, depth charges — do (see `water`).
//!
//! Against the enemy seen lately, the factories we have are weighed by
//! their units: what none of them can engage is the gap, and where it is a
//! good share of what we see, the factory whose units close most of it is
//! asked for (see the economy's requisitions). Enemies seen under water
//! also call for an Urchin (torpedo turret: sonar to see them by, and
//! torpedoes); there is no sonar building.

use springai::{UnitDef, Vec3};
use zk_terrain::Tier;

use crate::Ai;
use crate::military::Enemy;
use crate::terrain::{reaches, tier_of};

/// Enemies seen within this many frames count.
const RECENT: i32 = 30 * 180;

/// A gap of this much enemy value, and this share of what we see, calls
/// for a factory.
const GAP_VALUE: f32 = 1500.0;
const GAP_SHARE: f32 = 0.2;

/// A factory is asked for if its units engage this share of the gap.
const GAP_CLOSED: f32 = 0.5;

/// Weapon ranges count up to this for reaching targets, elmos.
const MAX_RANGE: f32 = 600.0;

/// Factories that can be asked for to close a gap (not the jump factory:
/// nothing here tells it what to build).
const CANDIDATES: [&str; 10] = [
    "factorycloak",
    "factoryshield",
    "factoryveh",
    "factorytank",
    "factoryhover",
    "factoryamph",
    "factoryspider",
    "factoryship",
    "factorygunship",
    "factoryplane",
];

/// What a kind of our units can engage: its movement tier (`None`: it
/// flies), whether it hits things under water, its longest reach, and a
/// unit type standing for it.
#[derive(Clone, Copy)]
struct Kind {
    tier: Option<Tier>,
    under_water: bool,
    range: f32,
    def: UnitDef,
}

impl Ai {
    /// The kinds of combat unit the factories `factories` build.
    fn arsenal_kinds(&self, factories: &[UnitDef]) -> Vec<Kind> {
        let mut kinds: Vec<Kind> = Vec::new();
        for def in factories.iter().flat_map(|f| f.build_options()) {
            if def.weapon_mounts().is_empty() || !def.build_options().is_empty() {
                continue;
            }
            let (tier, under_water) = (tier_of(def), self.war.reach(Some(def)).under_water);
            if tier.is_none() && !def.is_able_to_fly() {
                continue;
            }
            let range = def.max_weapon_range().min(MAX_RANGE);
            match kinds.iter_mut().find(|k| k.tier == tier && k.under_water == under_water) {
                Some(k) if range > k.range => {
                    k.range = range;
                    k.def = def;
                }
                Some(_) => {}
                None => kinds.push(Kind { tier, under_water, range, def }),
            }
        }
        kinds
    }

    /// Whether some kind of `kinds`, from `base`, engages enemy `e`.
    fn arsenal_engages(&self, kinds: &[Kind], base: Vec3, e: &Enemy) -> bool {
        let submerged = e.is_submerged();
        let terrain = self.reach_terrain();
        kinds.iter().any(|k| {
            (!submerged || k.under_water)
                && (k.tier.is_none() || reaches(terrain, k.def, base, e.position, k.range.max(1.0)))
        })
    }

    /// The enemy value seen lately that none of our factories' units can
    /// engage, when it is a good share of what we see: the factory whose
    /// units engage the most of it, the value out of reach, all seen, and
    /// what that factory would engage.
    pub fn arsenal_gap(&self) -> Option<(&'static str, f32, f32, f32)> {
        let base = self.graph.get_ally_center();
        let owned: Vec<UnitDef> = self.fac.factories.values().filter_map(|f| f.unit.def()).collect();
        if owned.is_empty() || base.x < 0.0 {
            return None;
        }
        let ours = self.arsenal_kinds(&owned);
        let frame = self.frame;
        let seen: Vec<&Enemy> = (self.war.targets.values())
            .filter(|e| e.identified && !e.is_dead && !e.can_fly() && frame - e.last_seen < RECENT)
            .collect();
        let total: f32 = seen.iter().map(|e| e.value).sum();
        let gap: Vec<&Enemy> = seen.iter().copied().filter(|e| !self.arsenal_engages(&ours, base, e)).collect();
        let gap_value: f32 = gap.iter().map(|e| e.value).sum();
        if gap_value < GAP_VALUE || gap_value < GAP_SHARE * total {
            return None;
        }
        let names: Vec<&str> = owned.iter().map(|d| self.defs.name(*d)).collect();
        CANDIDATES
            .iter()
            .filter(|f| !names.contains(f))
            .filter_map(|&f| {
                let kinds = self.arsenal_kinds(&[self.defs.get(f)?]);
                let closed: f32 = gap.iter().filter(|e| self.arsenal_engages(&kinds, base, e)).map(|e| e.value).sum();
                Some((f, closed))
            })
            .max_by(|a, b| a.1.total_cmp(&b.1))
            .filter(|&(_, closed)| closed >= GAP_CLOSED * gap_value)
            .map(|(f, closed)| (f, gap_value, total, closed))
    }

    /// Where an enemy was seen under water lately, if anywhere.
    pub fn arsenal_submerged_sighting(&self) -> Option<Vec3> {
        let frame = self.frame;
        (self.war.targets.values())
            .filter(|e| !e.is_dead && frame - e.last_seen < RECENT && e.is_submerged())
            .max_by_key(|e| e.last_seen)
            .map(|e| e.position)
    }
}
