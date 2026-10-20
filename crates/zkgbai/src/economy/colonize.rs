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
//! enough.

use springai::Vec3;
use zk_terrain::occupancy::Occupancy;
use zk_terrain::paths::SNAP;

use crate::Ai;
use crate::terrain::{pos, tier_of};
use crate::util::distance;

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

/// A colony being taken.
pub struct Colony {
    center: Vec3,
    spots: Vec<Vec3>,
    /// The colonist (a worker id), and whether a Charon flies it there.
    colonist: i32,
    ferried: bool,
    since: i32,
}

impl Ai {
    /// Whether the colonist's job at `p` is a colony's.
    pub(super) fn eco_colony_bonus(&self, wid: i32, p: Vec3, mex: bool) -> f32 {
        match &self.eco.colony {
            Some(c) if mex && c.colonist == wid && distance(p, c.center) <= CLUSTER => COLONY_BONUS,
            _ => 0.0,
        }
    }

    /// See the colony under way to its end; or found one.
    pub(super) fn eco_plan_colony(&mut self) {
        if self.dev_off("colonize") {
            return;
        }
        let frame = self.frame;
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
        // The richest cluster.
        let Some(spots) = (free.iter())
            .map(|&(p, _)| free.iter().filter(|q| distance(q.0, p) <= CLUSTER).copied().collect::<Vec<_>>())
            .filter(|c| c.len() >= MIN_SPOTS)
            .max_by(|a, b| a.iter().map(|s| s.1).sum::<f32>().total_cmp(&b.iter().map(|s| s.1).sum::<f32>()))
        else {
            self.eco.colony_after = frame + RETRY;
            return;
        };
        let n = spots.len() as f32;
        let center = spots.iter().fold(Vec3::ZERO, |c, s| Vec3::xz(c.x + s.0.x / n, c.z + s.0.z / n));
        let spots: Vec<Vec3> = spots.into_iter().map(|s| s.0).collect();
        // The nearest constructor that flies, else the nearest that walks.
        let pick = |fly: bool| {
            (self.eco.workers.values())
                .filter(|w| !w.is_com && !w.is_chicken && !w.assigned_plop && !w.unit.is_being_built())
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
        self.eco.colony = Some(Colony { center, spots, colonist: wid, ferried, since: frame });
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
