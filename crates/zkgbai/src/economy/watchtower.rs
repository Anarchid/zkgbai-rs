//! Watchtowers: an advanced radar (`staticheavyradar`, 5600 elmos of
//! radar, 450 metal) on a spike of raised ground, to see past the hills in
//! between, where it has radar on the most of the enemy's ground that ours
//! does not: its metal spots and the buildings we know of, and most of all
//! what our silos strike or would (the engine tells of enemy deaths in our
//! radar as in our sight, so a struck building in radar is known dead or
//! seen standing, not presumed; see `missiles`).
//!
//! From some income on, the nearest constructors raise a spike on safe
//! ground of ours whose radar would see enough of that (of the places that
//! bring in nearly the most, the farthest back; there, on the highest
//! ground about it that sees nearly as much), and the radar goes on top
//! (or on the ground, if the spike does not come up, or if the hill alone
//! sees all a spike would). The spike is the radar's footprint wide. Radar
//! sees by sight lines over the terrain, as the engine's does: what a hill
//! between hides, a site does not count, unless the spike lifts the radar
//! over it.

use springai::{Unit, Vec3};

use crate::Ai;
use crate::economy::List;
use crate::economy::terraform::is_terraforming;
use crate::util::{NULLPOS, distance, get_directional_point};

const RADAR: &str = "staticheavyradar";

/// How far its radar reaches, less a margin, elmos.
const REACH: f32 = 5000.0;

/// The spike: a flat top this much higher than the ground under it, elmos,
/// just the radar's footprint wide (see `eco_raise_spike`).
const SPIKE: f32 = 64.0;

/// Natural high ground is looked for this far about the chosen place, on
/// a grid this fine, elmos; the highest this many points are weighed.
const HILL_SEARCH: f32 = 480.0;
const HILL_STEP: f32 = 32.0;
const HILLS: usize = 24;
/// Not on top of the metal spot itself.
const HILL_OFF_SPOT: f32 = 64.0;

/// The radar's height above its footing (`radarEmitHeight`), and how high
/// above the ground what it watches stands, elmos.
const EMIT_HEIGHT: f32 = 32.0;
const TARGET_HEIGHT: f32 = 10.0;

/// Frames to give the spike (and constructors to send, one after another,
/// as each is called away), and the radar on it; before trying again after
/// a tower.
const RAISE_FRAMES: i32 = 30 * 90;
const RAISE_TRIES: u32 = 3;
const BUILD_FRAMES: i32 = 30 * 480;
const RETRY: i32 = 30 * 180;

/// A tower is worth it from this income on, a second from twice that.
const MIN_INCOME: f32 = 30.0;

/// A tower must bring this much into radar: a point of the enemy's ground
/// counts one, a strike target three.
const MIN_WATCH: usize = 6;
const STRIKE_WEIGHT: usize = 3;

/// Constructors this far from the site are not sent, elmos; this many go.
const WORKER_RANGE: f32 = 4000.0;
const RAISERS: usize = 2;

/// Spacing from other buildings for the site, in build squares (16 elmos):
/// room for the top and its sloped skirts.
const CLEARANCE: i32 = 6;

/// A watchtower going up.
pub struct Watchtower {
    site: Vec3,
    /// Ground height there before the spike.
    ground: f32,
    /// The constructor raising the spike (none when the ground is high
    /// enough as it is).
    worker: Option<Unit>,
    since: i32,
    /// Constructors sent to raise the spike.
    tries: u32,
    /// Whether the radar on top is planned.
    planned: bool,
}

impl Ai {
    /// Plan a watchtower if our strikes want one; see the one going up.
    pub(super) fn eco_plan_watchtower(&mut self) {
        let Some(def) = self.defs.get(RADAR) else { return };
        let frame = self.frame;
        if self.dev_off("radar") {
            return;
        }
        let towers: Vec<Unit> = (self.cb.team_units().into_iter()).filter(|u| self.defs.name_of(*u) == RADAR).collect();
        if let Some(t) = &self.eco.watchtower {
            let (site, ground, worker, since, planned, tries) =
                (t.site, t.ground, t.worker, t.since, t.planned, t.tries);
            let raised = self.cb.map().elevation_at(site.x, site.z) - ground;
            let called_away = !planned && raised < SPIKE - 8.0 && worker.is_some_and(|w| !is_terraforming(w));
            if towers.iter().any(|r| distance(r.pos(), site) < 300.0) {
                self.log(&format!("radar: watchtower up at ({:.0}, {:.0}), {raised:.0} elmos up", site.x, site.z));
                (self.eco.watchtower, self.eco.watchtower_after) = (None, frame + RETRY);
            } else if called_away && tries < RAISE_TRIES && frame - since <= RAISE_FRAMES {
                // Another constructor, when one is free.
                let orders: Vec<i32> =
                    worker.map(|w| w.current_commands().iter().map(|c| c.id()).collect()).unwrap_or_default();
                if let Some(unit) = self.eco_raise_spike(site) {
                    let tries = tries + 1;
                    if let Some(t) = self.eco.watchtower.as_mut() {
                        (t.worker, t.tries) = (Some(unit), tries);
                    }
                    self.log(&format!(
                        "radar: the constructor raising the spike at ({:.0}, {:.0}) was called away (orders \
                         {orders:?}); another sent ({tries})",
                        site.x, site.z
                    ));
                }
            } else if !planned && (raised >= SPIKE - 8.0 || frame - since > RAISE_FRAMES || called_away) {
                // On the spike: where it is, or close by.
                let map = self.cb.map();
                let at = if map.is_possible_to_build_at(def, site, 0) {
                    site
                } else {
                    map.find_closest_build_site(def, site, 40.0, 0, 0)
                };
                let ok = at.x >= 0.0 && self.eco_add_simple(def, at, List::Construction, None).is_some();
                if let Some(t) = self.eco.watchtower.as_mut() {
                    t.planned = true;
                    t.since = frame;
                }
                self.log(&format!(
                    "radar: spike at ({:.0}, {:.0}) {raised:.0} elmos up; {}",
                    site.x,
                    site.z,
                    if ok { "the advanced radar goes on top" } else { "no place for the radar" }
                ));
                if !ok {
                    (self.eco.watchtower, self.eco.watchtower_after) = (None, frame + RETRY);
                }
            } else if planned && frame - since > BUILD_FRAMES {
                self.log("radar: the watchtower did not go up");
                (self.eco.watchtower, self.eco.watchtower_after) = (None, frame + RETRY);
            }
            return;
        }
        let towers_wanted = if self.eco.adjusted_income >= 2.0 * MIN_INCOME { 2 } else { 1 };
        if frame < self.eco.watchtower_after
            || towers.len() >= towers_wanted
            || self.eco.adjusted_income < MIN_INCOME
            || self.eco.estalled
        {
            return;
        }
        // What we strike or would, and the enemy's ground, out of our radar.
        let mut watch: Vec<(Vec3, usize)> = (self.missiles_watch_points().into_iter())
            .chain(self.nukes_watch_points())
            .map(|p| (p, STRIKE_WEIGHT))
            .collect();
        watch.extend(self.graph.get_enemy_territory().iter().map(|m| (m.get_pos(), 1)));
        let buildings = self.war.targets.values().filter(|e| e.identified && e.is_static && !e.is_dead);
        watch.extend(buildings.map(|e| (e.position, 1)));
        let covered = self.los.in_radar(&watch.iter().map(|w| w.0).collect::<Vec<_>>());
        let unseen: Vec<(Vec3, usize)> = watch.into_iter().zip(covered).filter(|&(_, c)| !c).map(|(w, _)| w).collect();
        // Safe ground of ours within reach of the most of it, the nearest.
        let mut sites: Vec<Vec3> = self.graph.get_owned_spots().iter().map(|m| m.get_pos()).collect();
        sites.push(self.graph.get_ally_center());
        let hm = &self.eco.height_map;
        let reached_from = |p: Vec3, eye: f32| {
            (unseen.iter())
                .filter(|u| distance(u.0, p) <= REACH && hm.sees(p, eye, u.0, TARGET_HEIGHT))
                .map(|u| u.1)
                .sum::<usize>()
        };
        let reached = |p: Vec3| reached_from(p, SPIKE + EMIT_HEIGHT);
        let near = |p: Vec3| unseen.iter().map(|u| distance(u.0, p)).fold(f32::MAX, f32::min);
        let scored: Vec<(Vec3, usize)> = (sites.into_iter())
            .filter(|&p| p != NULLPOS && self.graph.is_ally_territory(p) && self.war.get_threat(p) <= 0.0)
            .map(|p| (p, reached(p)))
            .filter(|&(_, r)| r >= MIN_WATCH)
            .collect();
        // Of those that bring in nearly the most, the farthest back: the
        // spike goes up where no enemy stops the terraforming or chases off
        // the constructors, and the radar lives.
        let best = scored.iter().map(|s| s.1).max().unwrap_or(0);
        let Some(spot) = (scored.into_iter())
            .filter(|&(_, r)| 5 * r >= 4 * best)
            .map(|(p, _)| p)
            .max_by(|&a, &b| near(a).total_cmp(&near(b)))
        else {
            return;
        };
        // Beside the metal spot, away from the enemy, unless a hill about it
        // sees as much: of the safe ground of ours there whose spike would
        // see nearly the most, the highest.
        let base = self.graph.get_ally_center();
        let beside = get_directional_point(spot, base, 150.0);
        let n = (HILL_SEARCH / HILL_STEP) as i32;
        let (w, h) = (self.cb.map().width() as f32 * 8.0, self.cb.map().height() as f32 * 8.0);
        let mut hills: Vec<(f32, Vec3)> = (-n..=n)
            .flat_map(|i| (-n..=n).map(move |j| Vec3::xz(spot.x + i as f32 * HILL_STEP, spot.z + j as f32 * HILL_STEP)))
            .filter(|p| p.x > 32.0 && p.z > 32.0 && p.x < w - 32.0 && p.z < h - 32.0)
            .filter(|&p| (HILL_OFF_SPOT..=HILL_SEARCH).contains(&distance(p, spot)))
            .filter(|&p| self.graph.is_ally_territory(p) && self.war.get_threat(p) <= 0.0)
            .map(|p| (hm.height_at(p.x, p.z), p))
            .collect();
        hills.sort_by(|a, b| b.0.total_cmp(&a.0));
        hills.truncate(HILLS);
        hills.push((hm.height_at(beside.x, beside.z), beside));
        let hills: Vec<(usize, f32, Vec3)> = hills.into_iter().map(|(y, p)| (reached(p), y, p)).collect();
        let best = hills.iter().map(|c| c.0).max().unwrap_or(0);
        let mut hills: Vec<(f32, Vec3)> =
            (hills.into_iter()).filter(|c| 5 * c.0 >= 4 * best).map(|c| (c.1, c.2)).collect();
        hills.sort_by(|a, b| b.0.total_cmp(&a.0));
        // Clear of other buildings by the spike's skirts (terraforming stops
        // at buildings): on the hill, or the nearest place to it; else
        // beside the spot as before.
        let map = self.cb.map();
        let site = (hills.iter().map(|&(_, p)| map.find_closest_build_site(def, p, 2.0 * HILL_STEP, CLEARANCE, 0)))
            .find(|s| s.x >= 0.0)
            .unwrap_or_else(|| map.find_closest_build_site(def, beside, 600.0, CLEARANCE, 0));
        if site.x < 0.0 {
            return;
        }
        let ground = map.elevation_at(site.x, site.z);
        let (worth, flat) = (reached(site), reached_from(site, EMIT_HEIGHT));
        // How much higher than beside the spot (+ 0.0: no "-0").
        let hill = (ground - hm.height_at(beside.x, beside.z)).round() + 0.0;
        if flat >= worth {
            // High enough as it is: the radar alone.
            if self.eco_add_simple(def, site, List::Construction, None).is_none() {
                return;
            }
            self.eco.watchtower =
                Some(Watchtower { site, ground, worker: None, since: frame, tries: 0, planned: true });
            self.log(&format!(
                "radar: a watchtower at ({:.0}, {:.0}) on the ground, {hill} higher than beside the spot: a spike \
                 would see no more (worth {flat})",
                site.x, site.z
            ));
            return;
        }
        let Some(unit) = self.eco_raise_spike(site) else { return };
        self.eco.watchtower =
            Some(Watchtower { site, ground, worker: Some(unit), since: frame, tries: 1, planned: false });
        self.log(&format!(
            "radar: raising a spike at ({:.0}, {:.0}), {hill} higher than beside the spot, for a watchtower over enemy \
             ground out of radar (worth {worth} in sight from the spike, {flat} from the ground)",
            site.x, site.z
        ));
    }

    /// Have the nearest free constructors raise a spike at `site`; one of
    /// them.
    fn eco_raise_spike(&mut self, site: Vec3) -> Option<Unit> {
        let mut near: Vec<(f32, i32)> = (self.eco.workers.values())
            .filter(|w| !w.is_chicken && !w.assigned_plop && !w.unit.is_being_built())
            .map(|w| (distance(w.get_pos(), site), w.id))
            .filter(|&(d, _)| d <= WORKER_RANGE)
            .collect();
        near.sort_by(|a, b| a.0.total_cmp(&b.0));
        let mut units = Vec::new();
        for (_, wid) in near.into_iter().take(RAISERS) {
            if let Some(w) = self.eco.workers.get_mut(&wid) {
                w.clear_task(&mut self.eco.tasks);
                units.push(w.unit);
            }
        }
        let unit = *units.first()?;
        // The gadget levels whole 16-elmo cells, from those the polygon's
        // corners floor to (`floor(x / 16) * 16`) through the next row and
        // column: corners just inside the footprint level it and no more.
        let (fx, fz) =
            self.defs.get(RADAR).map_or((16.0, 16.0), |d| (d.x_size() as f32 * 8.0, d.z_size() as f32 * 8.0));
        let (hx, hz) = (fx / 2.0 - 4.0, fz / 2.0 - 4.0);
        let square = [(-hx, -hz), (hx, -hz), (hx, hz), (-hx, hz)].map(|(dx, dz)| Vec3::xz(site.x + dx, site.z + dz));
        let map = self.cb.map();
        let top = square.iter().map(|p| map.elevation_at(p.x, p.z)).fold(map.elevation_at(site.x, site.z), f32::max);
        self.terraform_level(&units, &square, top + SPIKE, false);
        Some(unit)
    }
}
