//! Colonies: free metal far from where our constructors work, which their
//! choice of jobs (the nearer the cheaper) leaves alone for long — whole
//! swathes of a big map. Now and then the economy looks for the richest
//! cluster of free metal spots that no enemy covers, away from all our
//! constructors, and sends a colonist there: an air constructor if we have
//! one (it flies); else a ground constructor, flown there by a Charon if one
//! is free (see `drops`), or walking if its way there passes no region the
//! enemy is known to stand in (see `zk_terrain::occupancy`; walking that
//! far through the enemy's land, they die). With none of these, our gunship
//! or plane factories build an air constructor. The colonist's jobs at the colony's spots count as
//! near ones until they are taken, it is lost, or it has been at it long
//! enough. By default cluster value is discounted toward the enemy centroid,
//! and the job bonus is multiplied by that discount and global standing-metal
//! density / 400 (capped at one). `colonize_policy` retains either adjustment
//! alone or the original baseline for comparisons.

use springai::Vec3;
use zk_terrain::occupancy::Occupancy;
use zk_terrain::paths::SNAP;

use crate::Ai;
use crate::terrain::{pos, tier_of};
use crate::util::{NULLPOS, distance};

/// Free spots this far from all our constructors are left alone, elmos;
/// a colony is those within this of one another, this many at least.
const FAR: f32 = 2500.0;
const CLUSTER: f32 = 1200.0;
const MIN_SPOTS: usize = 3;

/// What the colonist's jobs at the colony's spots are made cheaper by (on
/// the scale of job costs: a quarter of the elmos away).
pub(super) const COLONY_BONUS: f32 = 3000.0;

/// Frames a colony lasts at most, and before the next is looked for.
const COLONY_FRAMES: i32 = 30 * 300;
const RETRY: i32 = 30 * 90;

/// Colonies are worth it from this income on.
const MIN_INCOME: f32 = 15.0;

/// A walking colonist's strength: any enemy in a region closes it.
const UNARMED: f32 = 1.0;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
enum Policy {
    Baseline,
    EnemyDistance,
    Density,
    #[default]
    Combined,
}

impl Policy {
    fn parse(value: Option<&str>) -> Self {
        match value {
            Some("enemy_distance") => Self::EnemyDistance,
            Some("density") => Self::Density,
            Some("baseline") => Self::Baseline,
            _ => Self::Combined,
        }
    }

    fn uses_density(self) -> bool {
        matches!(self, Self::Density | Self::Combined)
    }

    fn uses_enemy_distance(self) -> bool {
        matches!(self, Self::EnemyDistance | Self::Combined)
    }

    fn utility_scale(self, distance: f32, density: f32) -> f32 {
        let distance = if self.uses_enemy_distance() { distance } else { 1.0 };
        let density = if self.uses_density() { density_scale(density) } else { 1.0 };
        distance * density
    }
}

fn density_scale(density: f32) -> f32 {
    (density / 400.0).clamp(0.0, 1.0)
}

/// Full value at least as far from the enemy as home; linear discount
/// toward the enemy. Missing/degenerate centroids preserve the baseline.
fn enemy_distance_scale(center: Vec3, enemy: Vec3, home: Vec3) -> f32 {
    if enemy == NULLPOS || home == NULLPOS || distance(home, enemy) < 1.0 {
        return 1.0;
    }
    (distance(center, enemy) / distance(home, enemy)).clamp(0.0, 1.0)
}

fn cluster_center(spots: &[(Vec3, f32)]) -> Vec3 {
    let n = spots.len() as f32;
    spots.iter().fold(Vec3::ZERO, |c, s| Vec3::xz(c.x + s.0.x / n, c.z + s.0.z / n))
}

/// A colony being taken.
pub struct Colony {
    center: Vec3,
    spots: Vec<Vec3>,
    /// The colonist (a worker id), and whether a Charon flies it there.
    colonist: i32,
    ferried: bool,
    since: i32,
    policy: Policy,
    scale: f32,
}

impl Ai {
    /// Whether the colonist's job at `p` is a colony's.
    pub(super) fn eco_colony_bonus(&self, wid: i32, p: Vec3, mex: bool) -> f32 {
        match &self.eco.colony {
            Some(c) if mex && c.colonist == wid && distance(p, c.center) <= CLUSTER => COLONY_BONUS * c.scale,
            _ => 0.0,
        }
    }

    /// See the colony under way to its end; or found one.
    pub(super) fn eco_plan_colony(&mut self) {
        if self.dev_off("colonize") {
            return;
        }
        let frame = self.frame;
        // Refresh active utilities as the global estimate or enemy centroid
        // changes. The baseline does not initialize or consult the estimator.
        if let Some(c) = &self.eco.colony {
            let scale = c.policy.utility_scale(
                enemy_distance_scale(c.center, self.graph.get_enemy_center(), self.graph.get_start_pos()),
                self.posture.density,
            );
            self.eco.colony.as_mut().unwrap().scale = scale;
        }
        if let Some(c) = &self.eco.colony {
            let free = self.graph.get_neutral_spots();
            let left = c.spots.iter().filter(|&&p| free.iter().any(|s| distance(s.get_pos(), p) < 50.0)).count();
            let unit = self.eco.workers.get(&c.colonist).map(|w| w.unit);
            let alive = unit.is_some_and(|u| u.health() > 0.0) || self.drops_ferrying(c.colonist);
            if left == 0 || !alive || frame - c.since > COLONY_FRAMES {
                let how = if !alive {
                    "the colonist was lost"
                } else if left == 0 {
                    "all taken"
                } else {
                    "out of time"
                };
                self.log(&format!(
                    "economy: colony at ({:.0}, {:.0}): {} of {} spots taken, {how}",
                    c.center.x,
                    c.center.z,
                    c.spots.len() - left,
                    c.spots.len()
                ));
                (self.eco.colony, self.eco.colony_after) = (None, frame + RETRY);
            }
            return;
        }
        if frame < self.eco.colony_after || self.eco.adjusted_income < MIN_INCOME || self.eco.estalled {
            return;
        }
        let policy = Policy::parse(self.cb.option("colonize_policy").as_deref());
        // Use the same global standing-metal / usable-area estimate as
        // missile-silo eligibility, refreshed even when posture is disabled.
        let density = policy.uses_density().then_some(self.posture.density);
        let distance_at =
            |center| enemy_distance_scale(center, self.graph.get_enemy_center(), self.graph.get_start_pos());
        let scale_at = |center| policy.utility_scale(distance_at(center), self.posture.density);
        let workers: Vec<Vec3> = self.eco.workers.values().map(|w| w.get_pos()).collect();
        let near = |p: Vec3| workers.iter().map(|&w| distance(w, p)).fold(f32::INFINITY, f32::min);
        let free: Vec<(Vec3, f32)> = (self.graph.get_neutral_spots().iter())
            .filter(|s| !s.enemy_shadowed)
            .map(|s| (s.get_pos(), s.value))
            .filter(|&(p, _)| {
                self.war.get_porc_threat(p) == 0.0
                    && self.war.get_effective_threat(p) <= 0.0
                    && !self.graph.is_enemy_territory(p)
                    && near(p) >= FAR
            })
            .collect();
        // Density affects job utility only; enemy distance also reranks sites.
        let score = |c: &Vec<(Vec3, f32)>| {
            let value = c.iter().map(|s| s.1).sum::<f32>();
            if policy.uses_enemy_distance() { value * distance_at(cluster_center(c)) } else { value }
        };
        let Some(spots) = (free.iter())
            .map(|&(p, _)| free.iter().filter(|q| distance(q.0, p) <= CLUSTER).copied().collect::<Vec<_>>())
            .filter(|c| c.len() >= MIN_SPOTS)
            .max_by(|a, b| score(a).total_cmp(&score(b)))
        else {
            self.eco.colony_after = frame + RETRY;
            return;
        };
        let center = cluster_center(&spots);
        let scale = scale_at(center);
        let spots: Vec<Vec3> = spots.into_iter().map(|s| s.0).collect();
        // The nearest constructor that flies, else the nearest that walks.
        let pick = |fly: bool| {
            (self.eco.workers.values())
                .filter(|w| !w.is_com && !w.is_chicken && !w.assigned_plop && !w.unit.is_being_built())
                .filter(|w| !self.eco_worker_held(w.id))
                .filter(|w| w.unit.def().is_some_and(|d| d.is_able_to_fly() == fly))
                .min_by(|a, b| distance(a.get_pos(), center).total_cmp(&distance(b.get_pos(), center)))
                .map(|w| (w.id, w.unit))
        };
        let flyer = pick(true);
        let walker = if flyer.is_none() { pick(false) } else { None };
        let flown = walker.filter(|&(_, unit)| self.drops_ferry(unit, center));
        let walks = walker.filter(|&(_, unit)| flown.is_none() && self.eco_walk_is_clear(unit, center));
        let Some((wid, unit, how)) = (flyer.map(|(w, u)| (w, u, "flies there")))
            .or_else(|| flown.map(|(w, u)| (w, u, "goes by Charon")))
            .or_else(|| walks.map(|(w, u)| (w, u, "walks there, by land clear of the enemy")))
        else {
            if self.eco.colony_wants_air.is_none_or(|f| frame - f > RETRY) {
                self.log(&format!(
                    "economy: {} free metal spots at ({:.0}, {:.0}), {:.0} from our nearest constructor, want a \
                     colonist: no air constructor or free Charon",
                    spots.len(),
                    center.x,
                    center.z,
                    near(center)
                ));
            }
            (self.eco.colony_wants_air, self.eco.colony_after) = (Some(frame), frame + RETRY);
            return;
        };
        let ferried = flown.is_some();
        if let Some(w) = self.eco.workers.get_mut(&wid) {
            w.clear_task(&mut self.eco.tasks);
        }
        self.log(&format!(
            "economy: colonizing {} free metal spots at ({:.0}, {:.0}), {:.0} from our nearest constructor: the {} {how}",
            spots.len(),
            center.x,
            center.z,
            near(center),
            self.defs.name_of(unit)
        ));
        self.log(&format!(
            "economy: colony policy={policy:?} utility_scale={scale:.4} density={} enemy_distance={:.0}",
            density.map_or_else(|| "unused".to_owned(), |d| format!("{d:.1}")),
            distance(center, self.graph.get_enemy_center()),
        ));
        self.eco.colony = Some(Colony { center, spots, colonist: wid, ferried, since: frame, policy, scale });
    }

    /// Whether `unit` can walk to `to` through regions of its movement tier
    /// that no enemy is known to stand in.
    fn eco_walk_is_clear(&self, unit: springai::Unit, to: Vec3) -> bool {
        let Some(map) = unit.def().and_then(tier_of).and_then(|t| self.terrain().and_then(|m| m.tier(t))) else {
            return false;
        };
        let Some(r) = map.region_near(pos(to), SNAP) else { return false };
        let occupancy = Occupancy::new(map, &self.region_presence());
        occupancy.fighting_distances(map, pos(unit.pos()), UNARMED)[r].is_finite()
    }

    /// Whether an air factory should build a constructor to colonize with.
    pub(super) fn eco_wants_air_colonist(&self) -> bool {
        let flyers = self.eco.workers.values().filter(|w| w.unit.def().is_some_and(|d| d.is_able_to_fly())).count();
        flyers == 0 && self.eco.colony_wants_air.is_some_and(|f| self.frame - f <= 2 * RETRY)
    }

    /// Whether worker `wid` is being flown to its colony (and so is not to
    /// be given jobs).
    pub(super) fn eco_in_transit(&self, wid: i32) -> bool {
        self.eco.colony.as_ref().is_some_and(|c| c.ferried && c.colonist == wid) && self.drops_ferrying(wid)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn distance_discount_prefers_flanks_to_forward_colonies() {
        let home = Vec3::xz(1000.0, 1000.0);
        let enemy = Vec3::xz(9000.0, 1000.0);
        let forward = enemy_distance_scale(Vec3::xz(5000.0, 1000.0), enemy, home);
        let flank = enemy_distance_scale(Vec3::xz(1000.0, 5000.0), enemy, home);
        assert_eq!(forward, 0.5);
        assert_eq!(flank, 1.0);
        assert_eq!(enemy_distance_scale(enemy, enemy, home), 0.0);
        assert_eq!(enemy_distance_scale(home, NULLPOS, home), 1.0);
        assert_eq!(enemy_distance_scale(home, home, home), 1.0);
    }

    #[test]
    fn both_discounts_apply_by_default_and_each_can_be_disabled() {
        assert_eq!(Policy::default().utility_scale(0.5, 200.0), 0.25);
        assert_eq!(Policy::default().utility_scale(0.5, 400.0), 0.5);
        assert_eq!(Policy::default().utility_scale(1.0, 800.0), 1.0);
        assert_eq!(Policy::default().utility_scale(1.0, 0.0), 0.0);
        assert_eq!(Policy::parse(Some("baseline")).utility_scale(0.5, 200.0), 1.0);
        assert_eq!(Policy::parse(Some("enemy_distance")).utility_scale(0.5, 200.0), 0.5);
        assert_eq!(Policy::parse(Some("density")).utility_scale(0.5, 200.0), 0.5);
    }

    #[test]
    fn density_reaches_current_utility_at_400_and_never_boosts_it() {
        for (density, expected) in [(-1.0, 0.0), (0.0, 0.0), (200.0, 0.5), (400.0, 1.0), (800.0, 1.0)] {
            assert_eq!(density_scale(density), expected);
        }
        for value in [None, Some(""), Some("invalid"), Some("combined")] {
            assert_eq!(Policy::parse(value), Policy::Combined);
        }
        assert_eq!(Policy::parse(Some("baseline")), Policy::Baseline);
    }
}
