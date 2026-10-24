//! Geothermal plants. A plant (`energygeo`: 500 metal, 25 energy) stands
//! on one of the map's vents (features with `geoThermal`); it morphs into
//! the advanced plant (`energyheavygeo`: 1000 metal and 1000 energy more,
//! at misc priority over 90 s, making nothing meanwhile; 100 energy, the
//! best rate in the game), which dies like a nuclear missile (1280 across,
//! 9500 at the middle).
//!
//! A free vent on safe ground of ours (our territory, off the front, no
//! threat) is the first energy built once the income can carry it. The
//! upgrade is for the overdrive phase only: it takes the place of the next
//! singularity reactor the fusion planner orders (a third of its metal for
//! a third of its energy), if a plant stands on safe ground with little of
//! ours within the blast. Started at an income of 30 (minutes 5 to 7), it
//! cost a won game in three: 1000 metal off expansion for energy nothing
//! could use yet (base 10 - 3 in series s1, Oct 23). As it starts, the
//! plant is linked to a grid with metal spots, and constructors raise a box
//! of walls round it, high enough to hide its aim point from direct fire
//! over level ground (the engine fires only on a clear ground ray to the
//! aim point, unless the ray hits the ground within the shot's splash of
//! the target).

use springai::{NO_TIMEOUT, Unit, Vec3};

use super::List;
use crate::Ai;
use crate::defs::cmd;
use crate::economy::terraform::is_terraforming;
use crate::util::distance;

const PLANT: &str = "energygeo";
const ADVANCED: &str = "energyheavygeo";

/// A plant is built once the energy we lack (against the economy's own
/// target) is this share of what it makes, or from this income on (metal/s;
/// 500 metal is then half a minute of it).
const GEO_SHORTFALL: f32 = 0.5;
const GEO_MIN_INCOME: f32 = 15.0;

/// A plant's site is this close to its vent, elmos.
const VENT_SNAP: f32 = 48.0;

/// The advanced plant's blast reaches this far, elmos; it is not upgraded
/// while more than this much metal of ours stands within it.
const BLAST: f32 = 640.0;
const VAR_MAX: f32 = 3000.0;

/// Terraform placeholders cost 100000 on paper.
const UNIT_COST_CAP: f32 = 20000.0;

/// Frames before another try after an upgrade that did not happen.
const RETRY: i32 = 30 * 60;

/// The wall box: walls this far out from the plant's middle (its
/// footprint reaches 40, the walls' skirts 24), raised this high; the aim
/// point is 30 above the ground (model middle at the ground plus
/// `aimposoffset`), a shooter's muzzle about 20.
const BOX_HALF: f32 = 72.0;
const WALL: f32 = 48.0;
const AIM: f32 = 30.0;
const MUZZLE: f32 = 20.0;

/// The box is checked from this many directions, this far out.
const COVER_DIRS: usize = 16;
const COVER_RANGE: f32 = 300.0;

/// Constructors this far from the plant are not sent, elmos; this many go;
/// how many times, and for how long.
const WORKER_RANGE: f32 = 3000.0;
const RAISERS: usize = 2;
const RAISE_TRIES: u32 = 3;
const BOX_FRAMES: i32 = 30 * 150;

/// Frames before trying again when no constructor was free.
const BOX_RETRY: i32 = 30 * 20;

/// Geothermal state.
#[derive(Default)]
pub struct Geothermal {
    /// The map's vents.
    vents: Vec<Vec3>,
    scanned: bool,
    /// The plant morphing into an advanced one: (unit, where, since).
    morph: Option<(Unit, Vec3, i32)>,
    /// No upgrade before this frame.
    upgrade_after: i32,
    /// Why the last upgrade look found none, and when that was logged
    /// (on change, at most once a minute).
    waiting: String,
    waiting_logged: i32,
    /// The wall box going up, and the plants boxed (or given up on).
    boxing: Option<GeoBox>,
    boxed: Vec<Vec3>,
    /// No box is tried before this frame (after finding no constructor).
    box_after: i32,
}

/// A wall box going up.
pub struct GeoBox {
    mid: Vec3,
    /// When it was ordered first, and last.
    started: i32,
    sides: Vec<Side>,
    worker: Unit,
    since: i32,
    tries: u32,
}

/// A wall of the box: from `a` to `b`, leveled to `level`; checked at
/// `checks` (corners are shared).
#[derive(Clone)]
struct Side {
    a: Vec3,
    b: Vec3,
    level: f32,
    checks: [Vec3; 3],
}

impl Ai {
    /// The map's vents (gaia features: always visible).
    pub(crate) fn eco_geo_vents(&mut self) -> Vec<Vec3> {
        if !self.eco.geo.scanned {
            self.eco.geo.scanned = true;
            // (Some maps put two vent features on one vent.)
            let mut vents: Vec<Vec3> = Vec::new();
            for f in self.cb.features() {
                if f.def().is_some_and(|d| d.is_geo_thermal()) && !vents.iter().any(|&v| distance(v, f.pos()) < 32.0) {
                    vents.push(f.pos());
                }
            }
            self.eco.geo.vents = vents;
            if !self.eco.geo.vents.is_empty() {
                let list: Vec<String> =
                    self.eco.geo.vents.iter().map(|v| format!("({:.0}, {:.0})", v.x, v.z)).collect();
                self.log(&format!("geo: {} vents: {}", list.len(), list.join(" ")));
            }
        }
        self.eco.geo.vents.clone()
    }

    /// Log the vents and the wind once, early in the game.
    pub(super) fn eco_energy_survey(&mut self, frame: i32) {
        if frame >= 30 && !self.eco.geo.scanned {
            self.eco_geo_vents();
            self.eco_wind_model();
        }
    }

    /// A plant on the free vent of ours nearest to the worker `wid`, if the
    /// income carries one and none is planned. Returns whether one was.
    pub(super) fn eco_create_geo_task(&mut self, wid: i32) -> bool {
        if self.dev_off("geo") {
            return false;
        }
        let vents = self.eco_geo_vents();
        let Some(plant) = self.defs.get(PLANT).filter(|_| !vents.is_empty()) else { return false };
        let short = self.eco.energy_target - self.eco.effective_income_energy;
        if short < GEO_SHORTFALL * super::wind::energy_make(plant) && self.eco.static_income < GEO_MIN_INCOME {
            return false;
        }
        let planned = self.eco.construction_tasks.iter().any(|&t| {
            self.eco.tasks.get(t).and_then(|t| t.as_construction()).is_some_and(|c| c.build_type.id() == plant.id())
        });
        let Some(w) = self.eco.workers.get(&wid) else { return false };
        let (wpos, wunit) = (w.get_pos(), w.unit);
        let Some(wdef) = wunit.def() else { return false };
        if planned {
            return false;
        }
        let map = self.cb.map();
        let range = wdef.build_distance() + 32.0;
        let site = (vents.into_iter())
            .filter(|&v| self.eco_safe_ground(v))
            .map(|v| (v, map.find_closest_build_site(plant, v, VENT_SNAP, 0, 0)))
            .filter(|&(v, s)| s.x >= 0.0 && distance(s, v) <= VENT_SNAP)
            .filter(|&(_, s)| self.terrain_reaches(wdef, wpos, s, range))
            .min_by(|a, b| distance(a.1, wpos).total_cmp(&distance(b.1, wpos)));
        let Some((vent, site)) = site else { return false };
        let Some(task) = self.eco_add_simple(plant, site, List::Construction, None) else { return false };
        // Worth what its energy would cost in solars (see `logistics`).
        let value = self.defs.get("energysolar").map_or(0.0, |s| {
            let e = super::wind::energy_make(s);
            if e > 0.0 { super::wind::energy_make(plant) * s.cost(self.metal) / e } else { 0.0 }
        });
        let why = format!("a geothermal plant on the vent at ({:.0}, {:.0})", vent.x, vent.z);
        self.logistics_energy(task, value, &why);
        self.log(&format!(
            "geo: a plant on the vent at ({:.0}, {:.0}) (income {:.1}, energy {:.1} of {:.1} wanted, {:.0} elmos from \
             the constructor)",
            vent.x,
            vent.z,
            self.eco.static_income,
            self.eco.effective_income_energy,
            self.eco.energy_target,
            distance(site, wpos)
        ));
        true
    }

    /// See the upgrade and the wall box through.
    pub(super) fn eco_plan_geo(&mut self, frame: i32) {
        if self.eco.geos.is_empty() && self.eco.geo.morph.is_none() {
            return;
        }
        self.eco_check_morph(frame);
        self.eco_check_box(frame);
        // (The staged test has no fusions; it upgrades as soon as it can.)
        if self.test.as_deref() == Some("geo") {
            self.eco_upgrade_geo(frame);
        }
        // An advanced plant without its box (one that came another way, or
        // whose box went up before the morph failed and was tried again).
        if self.eco.geo.boxing.is_none() && frame >= self.eco.geo.box_after && !self.dev_off("geobox") {
            let unboxed = (self.eco.geos.iter())
                .find(|&&g| {
                    self.defs.name_of(g) == ADVANCED && !self.eco.geo.boxed.iter().any(|&b| distance(b, g.pos()) < 50.0)
                })
                .map(|g| g.pos());
            if let Some(mid) = unboxed {
                self.eco_box_geo(mid, frame);
            }
        }
    }

    fn eco_check_morph(&mut self, frame: i32) {
        let Some((unit, pos, since)) = self.eco.geo.morph else { return };
        let up = self.eco.geos.iter().any(|&g| self.defs.name_of(g) == ADVANCED && distance(g.pos(), pos) < 50.0);
        if up {
            self.log(&format!(
                "geo: advanced plant up at ({:.0}, {:.0}) after {} s",
                pos.x,
                pos.z,
                (frame - since) / 30
            ));
            self.eco.geo.morph = None;
        } else if !self.eco.geos.contains(&unit) {
            self.log(&format!("geo: the plant upgrading at ({:.0}, {:.0}) is gone", pos.x, pos.z));
            self.eco.geo.morph = None;
            self.eco.geo.upgrade_after = frame + RETRY;
        } else if frame - since > 30 * 5 && unit.rules_param_float("morphing", 0.0) == 0.0 {
            self.log(&format!("geo: the plant at ({:.0}, {:.0}) is not upgrading; trying later", pos.x, pos.z));
            self.eco.geo.morph = None;
            self.eco.geo.upgrade_after = frame + RETRY;
        }
    }

    /// Upgrade a plant that is safe enough to stand as a bomb, in place of
    /// the singularity reactor the fusion planner is about to order. Returns
    /// whether an upgrade started.
    pub(super) fn eco_upgrade_geo(&mut self, frame: i32) -> bool {
        if self.eco.geo.morph.is_some() || frame < self.eco.geo.upgrade_after || self.dev_off("advgeo") {
            return false;
        }
        let Some(advanced) = self.defs.get(ADVANCED) else { return false };
        let plants: Vec<Unit> =
            (self.eco.geos.iter().copied()).filter(|&g| self.defs.name_of(g) == PLANT && !g.is_being_built()).collect();
        if plants.is_empty() {
            return false;
        }
        let why = if self.eco.hard_estalled { "energy stalled".to_owned() } else { String::new() };
        let mut best: Option<(Unit, f32)> = None;
        let mut reasons = Vec::new();
        if why.is_empty() {
            for g in plants {
                let pos = g.pos();
                if !self.eco_safe_ground(pos) {
                    reasons.push(format!("({:.0}, {:.0}) unsafe", pos.x, pos.z));
                    continue;
                }
                let var = self.eco_value_near(pos, BLAST, g);
                if var > VAR_MAX {
                    reasons.push(format!("({:.0}, {:.0}) {var:.0} metal of ours in the blast", pos.x, pos.z));
                    continue;
                }
                if best.is_none_or(|b| var < b.1) {
                    best = Some((g, var));
                }
            }
        }
        let Some((g, var)) = best else {
            let why = if why.is_empty() { reasons.join(", ") } else { why };
            // Logged when the reason changes kind (not on every number).
            let kind: String = why.chars().filter(|c| !c.is_ascii_digit()).collect();
            if kind != self.eco.geo.waiting && frame - self.eco.geo.waiting_logged >= 30 * 60 {
                self.log(&format!("geo: no upgrade (a singularity instead): {why}"));
                (self.eco.geo.waiting, self.eco.geo.waiting_logged) = (kind, frame);
            }
            return false;
        };
        self.eco.geo.waiting.clear();
        let pos = g.pos();
        g.execute_custom_command(cmd::MORPH, &[advanced.id() as f32], 0, NO_TIMEOUT);
        self.eco.geo.morph = Some((g, pos, frame));
        let eco = &self.eco;
        self.log(&format!(
            "geo: upgrading the plant at ({:.0}, {:.0}) before a singularity (energy {:.1}, metal {:.1}, {} fusions; \
             {var:.0} metal of ours within the blast)",
            pos.x,
            pos.z,
            eco.effective_income_energy,
            eco.effective_income_metal,
            eco.fusions.len()
        ));
        self.eco_link_geo(pos, advanced.id());
        if !self.dev_off("geobox") && self.eco.geo.boxing.is_none() && frame >= self.eco.geo.box_after {
            self.eco_box_geo(pos, frame);
        }
        true
    }

    /// Pylons from the plant at `pos` to the nearest grid with metal spots,
    /// unless it is on one: energy off such a grid overdrives nothing.
    fn eco_link_geo(&mut self, pos: Vec3, def_id: i32) {
        let Some(range) = self.eco.grid_defs.range(def_id) else { return };
        let grid = self.eco_mex_grid_members();
        if grid.iter().any(|m| distance(m.0, pos) < 1.0) {
            return;
        }
        let Some(link) = self.eco_link_to(&grid, pos, range) else { return };
        if link.pylons > 0 {
            self.eco_lay_pylons(pos, range, link.to, link.range, "the advanced geothermal plant");
        }
    }

    /// Metal in friendly units within `r` of `pos`, `except` aside.
    fn eco_value_near(&self, pos: Vec3, r: f32, except: Unit) -> f32 {
        (self.cb.friendly_units_in(pos, r).into_iter())
            .filter(|&u| u != except)
            .filter_map(|u| Some(u.def()?.cost(self.metal)))
            .filter(|&c| c < UNIT_COST_CAP)
            .fold(0.0, |a, c| a + c)
    }

    /// Send the nearest free constructors to raise the walls round the
    /// plant at `mid`: each side leveled to its highest ground plus
    /// [`WALL`] (so a retry only finishes what is missing).
    fn eco_box_geo(&mut self, mid: Vec3, frame: i32) {
        let map = self.cb.map();
        let h = BOX_HALF;
        let corners = [(-h, -h), (h, -h), (h, h), (-h, h)].map(|(dx, dz)| Vec3::xz(mid.x + dx, mid.z + dz));
        let sides: Vec<Side> = (0..4)
            .map(|i| {
                let (a, b) = (corners[i], corners[(i + 1) % 4]);
                let at = |t: f32| Vec3::xz(a.x + (b.x - a.x) * t, a.z + (b.z - a.z) * t);
                let top = [0.0, 0.25, 0.5, 0.75, 1.0].map(|t| map.elevation_at(at(t).x, at(t).z));
                let level = top.into_iter().fold(f32::MIN, f32::max) + WALL;
                Side { a, b, level, checks: [0.25, 0.5, 0.75].map(at) }
            })
            .collect();
        let Some(worker) = self.eco_send_raisers(mid, &sides) else {
            self.log(&format!("geo: no constructor to raise the walls round ({:.0}, {:.0}) yet", mid.x, mid.z));
            self.eco.geo.box_after = frame + BOX_RETRY;
            return;
        };
        self.eco.geo.boxing = Some(GeoBox { mid, started: frame, sides, worker, since: frame, tries: 1 });
        self.log(&format!("geo: raising a wall box round ({:.0}, {:.0}), {WALL:.0} high", mid.x, mid.z));
    }

    /// Have the nearest free constructors level the sides not up yet; one
    /// of them.
    fn eco_send_raisers(&mut self, mid: Vec3, sides: &[Side]) -> Option<Unit> {
        let mut near: Vec<(f32, i32)> = (self.eco.workers.values())
            // (`assigned_plop` stays set once the plop is used.)
            .filter(|w| !(w.is_chicken || w.unit.is_being_built() || (w.assigned_plop && w.has_plop())))
            .map(|w| (distance(w.get_pos(), mid), w.id))
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
        let first = *units.first()?;
        let short: Vec<&Side> = sides.iter().filter(|s| self.eco_side_short(s) > 0.0).collect();
        let mut queued = false;
        for side in short {
            self.terraform_line_level(&units, &[side.a, side.b], side.level, queued);
            queued = true;
        }
        Some(first)
    }

    /// How far below its level a side's lowest checked point still is.
    fn eco_side_short(&self, side: &Side) -> f32 {
        let map = self.cb.map();
        side.checks.iter().map(|p| side.level - map.elevation_at(p.x, p.z)).fold(0.0, f32::max)
    }

    fn eco_check_box(&mut self, frame: i32) {
        let Some(b) = &self.eco.geo.boxing else { return };
        let short: Vec<f32> = b.sides.iter().map(|s| self.eco_side_short(s)).collect();
        let (mid, worker, started, since, tries) = (b.mid, b.worker, b.started, b.since, b.tries);
        let heights = short.iter().map(|r| format!("{:.0}", WALL - r)).collect::<Vec<_>>().join("/");
        if short.iter().all(|&s| s <= 8.0) {
            let hidden = self.eco_box_cover(mid);
            self.log(&format!(
                "geo: wall box round ({:.0}, {:.0}) up (sides {heights} high); hides it from {hidden} of \
                 {COVER_DIRS} directions {COVER_RANGE:.0} out ({} s)",
                mid.x,
                mid.z,
                (frame - started) / 30
            ));
            self.eco.geo.boxing = None;
            self.eco.geo.boxed.push(mid);
        } else if frame - started > BOX_FRAMES
            || (frame - since > 30 * 10 && !is_terraforming(worker) && tries >= RAISE_TRIES)
        {
            let hidden = self.eco_box_cover(mid);
            self.log(&format!(
                "geo: wall box round ({:.0}, {:.0}) left with sides {heights} high after {tries} tries; hides it \
                 from {hidden} of {COVER_DIRS} directions",
                mid.x, mid.z
            ));
            self.eco.geo.boxing = None;
            self.eco.geo.boxed.push(mid);
        } else if frame - since > 30 * 10 && !is_terraforming(worker) {
            let sides = self.eco.geo.boxing.as_ref().map(|b| b.sides.clone()).unwrap_or_default();
            if let Some(unit) = self.eco_send_raisers(mid, &sides) {
                if let Some(b) = self.eco.geo.boxing.as_mut() {
                    (b.worker, b.tries, b.since) = (unit, tries + 1, frame);
                }
                self.log(&format!(
                    "geo: the constructor on the wall box round ({:.0}, {:.0}) was called away (sides {heights} \
                     high); another sent ({})",
                    mid.x,
                    mid.z,
                    tries + 1
                ));
            }
        }
    }

    /// From how many of [`COVER_DIRS`] directions, [`COVER_RANGE`] out on
    /// the live ground, a muzzle cannot see the plant's aim point.
    fn eco_box_cover(&self, mid: Vec3) -> usize {
        let map = self.cb.map();
        let aim = map.elevation_at(mid.x, mid.z) + AIM;
        (0..COVER_DIRS)
            .filter(|&k| {
                let a = k as f32 * std::f32::consts::TAU / COVER_DIRS as f32;
                let from = Vec3::xz(mid.x + a.cos() * COVER_RANGE, mid.z + a.sin() * COVER_RANGE);
                let muzzle = map.elevation_at(from.x, from.z).max(0.0) + MUZZLE;
                let steps = (COVER_RANGE / 8.0) as i32;
                (1..steps).any(|s| {
                    let t = s as f32 / steps as f32;
                    let (x, z) = (from.x + (mid.x - from.x) * t, from.z + (mid.z - from.z) * t);
                    map.elevation_at(x, z) > muzzle + (aim - muzzle) * t
                })
            })
            .count()
    }
}
