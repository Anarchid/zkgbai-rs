//! Wind turbines by height (Zero-K's `LuaRules/Gadgets/
//! unit_windmill_control.lua`). The map's wind strength s is drawn evenly
//! between `WindMin` and `WindMax` (0 and 2.5 energy) every 32 s; a turbine
//! makes (max - s)·share + s, where `share`, what it keeps of the maximum
//! however calm the wind, rises with the ground it stands on:
//! (height - `WindGroundMin`)·`WindSlope`, capped at `WindMinBound` (0.4).
//! Low ground thus makes 0 to 2.5 (1.25 on average), the highest 1 to 2.5
//! (1.75). Below `tidalHeight` (-10) the gadget makes it a tidal
//! generator, a steady 1.2. The parameters are public game rules params.
//!
//! The economy values a turbine at what it is sure to make plus
//! [`RELIABILITY`] of the rest: on low ground what a solar makes for the
//! same metal, at the cap 60% more. Where the ground by the grid is high
//! enough, turbines go up there instead of solars. Where a hill of ours
//! rises higher, a constructor walks up and puts down a batch of them, if
//! the better yield pays for the walk (both ways) and the pylons linking
//! the batch to the grid (else its energy could not overdrive).

use springai::{Callback, Unit, UnitDef, Vec3};

use super::List;
use crate::Ai;
use crate::util::distance;

/// Share of the fluctuating part of a turbine's output the economy counts
/// on: stalls come in the lulls. On low ground a turbine is then worth
/// what a solar makes for its metal.
const RELIABILITY: f32 = 0.8;

/// Beyond the solars the economy keeps anyway (they armor up under fire,
/// turbines have 150 health), turbines replace solars by the grid where
/// they are worth this much more per metal (below, the old mix: mostly
/// solars).
const LOCAL_EDGE: f32 = 1.15;

/// Hill sites are sampled every so many elmos (one turbine each, packed
/// close enough to link to each other: pylon range 63 each).
const CELL: f32 = 64.0;

/// Hill sites worth less than this (energy per turbine) are not looked at.
const HILL_MIN_WORTH: f32 = 1.15;

/// A batch on a hill: this many turbines, from sites this near the first
/// and at most this much worse.
const BATCH: usize = 8;
const BATCH_REACH: f32 = 256.0;
const BATCH_SLACK: f32 = 0.1;

/// A hill must beat building by the grid, per metal, by this much, after
/// the walk and the link.
const HILL_EDGE: f32 = 1.1;

/// How far a constructor is sent for a hill, elmos; how many of the
/// nearest-best sites are checked for reach and link.
const HILL_REACH: f32 = 3000.0;
const HILL_SHORTLIST: usize = 12;

/// What a building makes in energy: Zero-K moves `energyMake` into the
/// custom param `income_energy` (`gamedata/unitdefs_post.lua`) and zeroes
/// the engine's.
pub fn energy_make(def: UnitDef) -> f32 {
    def.custom_param("income_energy").and_then(|v| v.trim().parse().ok()).unwrap_or(0.0)
}

/// The wind as the gadget makes it.
#[derive(Clone, Copy, Debug)]
pub struct WindModel {
    min: f32,
    max: f32,
    ground_min: f32,
    slope: f32,
    bound: f32,
    tidal_height: f32,
    /// What a tidal generator makes (the def's `energyMake`).
    tidal: f32,
}

impl WindModel {
    pub fn read(cb: Callback, tidal: f32) -> Self {
        let g = cb.game();
        WindModel {
            min: g.rules_param_float("WindMin", 0.0),
            max: g.rules_param_float("WindMax", 2.5),
            ground_min: g.rules_param_float("WindGroundMin", 0.0),
            slope: g.rules_param_float("WindSlope", 0.001),
            bound: g.rules_param_float("WindMinBound", 0.4),
            tidal_height: g.rules_param_float("tidalHeight", -10.0),
            tidal,
        }
    }

    fn tidal_at(&self, h: f32) -> bool {
        h <= self.tidal_height
    }

    /// What a turbine on ground `h` high keeps of the maximum in a calm
    /// (a floater on shallow water stands at sea level).
    pub fn share(&self, h: f32) -> f32 {
        ((h.max(0.0) - self.ground_min) * self.slope).clamp(0.0, self.bound)
    }

    /// The least a turbine there makes.
    pub fn floor(&self, h: f32) -> f32 {
        if self.tidal_at(h) {
            return self.tidal;
        }
        (self.max - self.min) * self.share(h) + self.min
    }

    /// What it makes on average.
    pub fn mean(&self, h: f32) -> f32 {
        if self.tidal_at(h) {
            return self.tidal;
        }
        let s = (self.min + self.max) / 2.0;
        (self.max - s) * self.share(h) + s
    }

    /// What the economy counts it for.
    pub fn worth(&self, h: f32) -> f32 {
        let floor = self.floor(h);
        floor + RELIABILITY * (self.mean(h) - floor)
    }

    /// The best a turbine anywhere is worth.
    pub fn best_worth(&self) -> f32 {
        let floor = (self.max - self.min) * self.bound + self.min;
        let s = (self.min + self.max) / 2.0;
        floor + RELIABILITY * ((self.max - s) * self.bound + s - floor)
    }
}

/// Wind turbines: the model and the hill sites.
#[derive(Default)]
pub struct Wind {
    model: Option<WindModel>,
    /// Ground worth looking at for hill batches, best first: (site, worth).
    hills: Vec<(Vec3, f32)>,
    /// Turbines put on hills, for the summary.
    pub hill_turbines: usize,
    /// Batches queued.
    pub batches: usize,
}

/// A batch of turbines for a hill.
struct Hill {
    sites: Vec<Vec3>,
    worth: f32,
    /// Seconds walked there and back.
    walk: f32,
    link: Option<Link>,
    /// A turbine's pylon range.
    wind_range: f32,
}

/// How a site links to the grid: the nearest member of a grid with metal
/// spots, its pylon range, and the pylons between.
struct Link {
    to: Vec3,
    range: f32,
    pylons: usize,
}

impl Ai {
    /// The wind model, read once the game runs.
    pub(super) fn eco_wind_model(&mut self) -> Option<WindModel> {
        if self.eco.wind.model.is_none() {
            let tidal = energy_make(self.defs.get("energywind")?);
            let model = WindModel::read(self.cb, tidal);
            // The ground worth a hill batch: sampled every CELL elmos.
            let map = self.cb.map();
            let (w, h) = (map.width() as f32 * 8.0, map.height() as f32 * 8.0);
            let mut hills = Vec::new();
            let mut z = CELL / 2.0;
            while z < h {
                let mut x = CELL / 2.0;
                while x < w {
                    let g = self.eco.height_map.height_at(x, z);
                    let worth = model.worth(g);
                    if g > 0.0 && worth >= HILL_MIN_WORTH {
                        hills.push((Vec3::new(x, g, z), worth));
                    }
                    x += CELL;
                }
                z += CELL;
            }
            hills.sort_by(|a, b| b.1.total_cmp(&a.1));
            let best = hills.first().map_or(0.0, |h| h.1);
            let home = self.graph.get_start_pos();
            self.log(&format!(
                "wind: {:.1}-{:.1} energy, calm share {:.4}/elmo above {:.0} up to {:.2} (a turbine is worth {:.2} at \
                 best, {:.2} at our start); {} hill sites worth >= {HILL_MIN_WORTH}, best {best:.2}",
                model.min,
                model.max,
                model.slope,
                model.ground_min,
                model.bound,
                model.best_worth(),
                model.worth(self.eco.height_map.height_at(home.x, home.z)),
                hills.len(),
            ));
            self.eco.wind.hills = hills;
            self.eco.wind.model = Some(model);
        }
        self.eco.wind.model
    }

    /// What a turbine at `pos` is worth (energy); 0 without a model.
    pub(super) fn eco_wind_worth(&mut self, pos: Vec3) -> f32 {
        let h = self.cb.map().elevation_at(pos.x, pos.z);
        self.eco_wind_model().map_or(0.0, |m| m.worth(h))
    }

    /// Whether a turbine at `pos` beats a solar per metal by
    /// [`LOCAL_EDGE`] (then turbines go there instead of solars).
    pub(super) fn eco_wind_beats_solar(&mut self, pos: Vec3) -> bool {
        if self.dev_off("wind") {
            return false;
        }
        let (Some(solar), Some(wind)) = (self.defs.get("energysolar"), self.defs.get("energywind")) else {
            return false;
        };
        let worth = self.eco_wind_worth(pos);
        let per_metal = |d: UnitDef, e: f32| e / d.cost(self.metal).max(1.0);
        per_metal(wind, worth) >= LOCAL_EDGE * per_metal(solar, energy_make(solar))
    }

    /// A batch of turbines on a hill of ours for the worker `wid`, if the
    /// better wind there beats building by the grid at `local` (a turbine
    /// there, or a solar if better), per metal, after the walk and the
    /// link. Returns whether one was queued.
    pub(super) fn eco_wind_hill(&mut self, wid: i32, local: Vec3) -> bool {
        if self.dev_off("wind") || self.eco_wind_model().is_none() || self.eco.wind.hills.is_empty() {
            return false;
        }
        let (Some(wind), Some(solar), Some(pylon)) =
            (self.defs.get("energywind"), self.defs.get("energysolar"), self.defs.get("energypylon"))
        else {
            return false;
        };
        let Some(w) = self.eco.workers.get(&wid) else { return false };
        let (wpos, bp, unit) = (w.get_pos(), w.bp, w.unit);
        let Some(wdef) = unit.def() else { return false };
        let speed = wdef.speed();
        if speed <= 0.0 || bp <= 0.0 {
            return false;
        }
        let wind_cost = wind.cost(self.metal).max(1.0);
        let local_worth = self.eco_wind_worth(local);
        let solar_rate = energy_make(solar) / solar.cost(self.metal).max(1.0);
        let local_rate = (local_worth / wind_cost).max(solar_rate);
        let here = distance(wpos, local);

        // Free safe sites in reach that could win, by energy per metal with
        // the walk (there and back, shared by the batch).
        let walk = |p: Vec3| 2.0 * (distance(wpos, p) - here).max(0.0) / speed;
        let taken = self.eco_wind_taken();
        let free = |p: Vec3| !taken.iter().any(|&t| distance(t, p) < CELL * 0.75);
        let mut shortlist: Vec<(Vec3, f32, f32)> = (self.eco.wind.hills.iter())
            .filter(|&&(p, worth)| worth / wind_cost >= HILL_EDGE * local_rate && distance(wpos, p) <= HILL_REACH)
            .filter(|&&(p, _)| free(p) && self.eco_safe_ground(p))
            .map(|&(p, worth)| (p, worth, worth / (wind_cost + walk(p) * bp / BATCH as f32)))
            .collect();
        shortlist.sort_by(|a, b| b.2.total_cmp(&a.2));
        shortlist.truncate(HILL_SHORTLIST);

        // Of those the worker reaches, the best after the link to the grid.
        let range = wdef.build_distance();
        let pylon_cost = pylon.cost(self.metal);
        let wind_range = self.eco.grid_defs.range(wind.id()).unwrap_or(0.0);
        let grid = self.eco_mex_grid_members();
        let mut best: Option<(Vec3, f32, f32, Option<Link>)> = None;
        for (p, worth, _) in shortlist {
            if !self.terrain_reaches(wdef, wpos, p, range + 32.0) {
                continue;
            }
            let link = self.eco_link_to(&grid, p, wind_range);
            let pylons = link.as_ref().map_or(0, |l| l.pylons);
            let overhead = (walk(p) * bp + pylons as f32 * pylon_cost) / BATCH as f32;
            let rate = worth / (wind_cost + overhead);
            if best.as_ref().is_none_or(|b| rate > b.2) {
                best = Some((p, worth, rate, link));
            }
        }
        let Some((first, worth, rate, link)) = best else { return false };
        if rate < HILL_EDGE * local_rate {
            return false;
        }

        // The batch: the first site and the nearest good free ones by it.
        let mut near: Vec<Vec3> = (self.eco.wind.hills.iter())
            .filter(|&&(p, w)| p != first && w >= worth - BATCH_SLACK && distance(p, first) <= BATCH_REACH)
            .filter(|&&(p, _)| free(p) && self.eco_safe_ground(p))
            .map(|&(p, _)| p)
            .collect();
        near.sort_by(|a, b| distance(*a, first).total_cmp(&distance(*b, first)));
        let mut sites = vec![first];
        sites.extend(near.into_iter().take(BATCH - 1));
        let hill = Hill { sites, worth, walk: walk(first), link, wind_range };
        self.eco_queue_hill(wind, hill, local_worth, rate / local_rate)
    }

    fn eco_queue_hill(&mut self, wind: UnitDef, hill: Hill, local_worth: f32, edge: f32) -> bool {
        let map = self.cb.map();
        let mut queued = 0;
        for &p in &hill.sites {
            let site = map.find_closest_build_site(wind, p, CELL / 2.0, 0, 0);
            if site.x < 0.0 {
                continue;
            }
            if self.eco_add_simple(wind, site, List::Wind, Some(List::Wind)).is_some() {
                queued += 1;
            }
        }
        if queued == 0 {
            return false;
        }
        let first = hill.sites[0];
        if let Some(l) = hill.link.as_ref().filter(|l| l.pylons > 0) {
            self.eco_lay_pylons(first, hill.wind_range, l.to, l.range, "a turbine batch on a hill");
        }
        self.eco.wind.batches += 1;
        self.eco.wind.hill_turbines += queued;
        let model = self.eco.wind.model;
        let h = map.elevation_at(first.x, first.z);
        self.log(&format!(
            "wind: a batch of {queued} turbines on the hill at ({:.0}, {:.0}), {h:.0} high: worth {:.2} each (floor \
             {:.2}; {local_worth:.2} by the grid), {:.0} s walk there and back, {} pylons to link; {edge:.2}x the \
             energy per metal",
            first.x,
            first.z,
            hill.worth,
            model.map_or(0.0, |m| m.floor(h)),
            hill.walk,
            hill.link.as_ref().map_or(0, |l| l.pylons),
        ));
        true
    }

    /// Where turbines stand or are queued.
    fn eco_wind_taken(&self) -> Vec<Vec3> {
        let eco = &self.eco;
        (eco.windgens.iter().map(|u| u.pos())).chain(eco.wind_tasks.iter().map(|&t| eco.task_pos(t))).collect()
    }

    /// Ground of ours, off the front, no enemy in sight of it: where
    /// energy goes up.
    pub(super) fn eco_safe_ground(&self, p: Vec3) -> bool {
        self.graph.is_ally_territory(p)
            && !self.graph.is_front_line(p)
            && !self.graph.is_enemy_territory(p)
            && self.war.get_threat(p) <= 0.0
            && self.war.get_porc_threat(p) <= 0.0
    }

    /// How `p` (a building of pylon range `r`) links to `grid` (see
    /// [`Link`]); none without a grid.
    fn eco_link_to(&self, grid: &[(Vec3, f32)], p: Vec3, r: f32) -> Option<Link> {
        let gap = |m: &(Vec3, f32)| distance(m.0, p) - m.1;
        let &(to, range) = grid.iter().min_by(|a, b| gap(a).total_cmp(&gap(b)))?;
        let pylons = if distance(to, p) < r + range {
            0
        } else {
            super::grid::chain(p, r, to, range, self.eco.grid_defs.pylon_range).len()
        };
        Some(Link { to, range, pylons })
    }

    /// One line on the energy we make, by source.
    pub(super) fn eco_energy_summary(&mut self) {
        let model = self.eco_wind_model();
        let map = self.cb.map();
        let eco = &self.eco;
        let make =
            |units: &[Unit]| -> f32 { units.iter().map(|u| u.rules_param_float("current_energyIncome", 0.0)).sum() };
        let floor: f32 = (eco.windgens.iter())
            .map(|u| {
                let p = u.pos();
                model.map_or(0.0, |m| m.floor(map.elevation_at(p.x, p.z)))
            })
            .sum();
        let (geos, adv): (Vec<Unit>, Vec<Unit>) = eco.geos.iter().partition(|u| self.defs.name_of(**u) == "energygeo");
        self.log(&format!(
            "energy: solars {} ({:.1}), turbines {} ({:.1}, floor {floor:.1}; {} queued on hills in {} batches), geos {} \
             ({:.1}), advanced {} ({:.1}), fusions {} ({:.1}); metal income {:.1}",
            eco.solars.len(),
            make(&eco.solars),
            eco.windgens.len(),
            make(&eco.windgens),
            eco.wind.hill_turbines,
            eco.wind.batches,
            geos.len(),
            make(&geos),
            adv.len(),
            make(&adv),
            eco.fusions.len(),
            make(&eco.fusions),
            eco.effective_income_metal,
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn zk() -> WindModel {
        WindModel { min: 0.0, max: 2.5, ground_min: 100.0, slope: 0.001, bound: 0.4, tidal_height: -10.0, tidal: 1.2 }
    }

    #[test]
    fn turbines_by_height() {
        let m = zk();
        // Low ground: 0 to 2.5, worth what a solar makes per metal (1 per 35).
        assert_eq!(m.floor(50.0), 0.0);
        assert!((m.mean(50.0) - 1.25).abs() < 1e-5);
        assert!((m.worth(50.0) - 1.0).abs() < 1e-5);
        // 400 above the ground minimum: at the cap, 1 to 2.5.
        assert!((m.floor(500.0) - 1.0).abs() < 1e-5);
        assert!((m.mean(500.0) - 1.75).abs() < 1e-5);
        assert!((m.worth(700.0) - m.best_worth()).abs() < 1e-5);
        assert!((m.best_worth() - 1.6).abs() < 1e-5);
        // Halfway up.
        assert!((m.floor(300.0) - 0.5).abs() < 1e-5);
        // Under water: tidal.
        assert_eq!(m.worth(-20.0), 1.2);
        // Shallow water: a turbine at sea level.
        assert_eq!(m.worth(-5.0), 1.0);
    }
}
