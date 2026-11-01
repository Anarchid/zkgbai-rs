//! Factory plates on allied factories. A plate (150 metal, against 700 for a
//! factory, with a factory's build power) builds what a factory of its type
//! within [`PLATE_RANGE`] builds, and an ally's factory will do: Zero-K's
//! `unit_factory_plate.lua` compares ally teams, not teams. So when we want
//! a factory of a type an ally already has standing, we plate the ally's
//! instead and build those units ourselves, the tech for a fifth of the
//! price. Not for the opening (the commander's plop is free) and not for
//! strider hubs (no plates).
//!
//! A plate whose factory is gone builds nothing (the gadget's `nofactory`
//! rules param): it leaves our factories until a factory of its type stands
//! in range again, which the gadget finds by itself.

use springai::{Unit, UnitDef, Vec3};

use super::TaskId;
use super::WorkerTask;
use super::allied::ASSIST_REACH;
use super::planning::{exit_lane, keeps_exit};
use crate::Ai;
use crate::util::distance;

/// How far from its factory a plate works (`FACTORY_PLATE_RANGE`), elmos.
const PLATE_RANGE: f32 = 420.0;
/// Plate sites are sought this far from the factory's centre, elmos, and
/// kept this far inside the range.
const PLATE_RINGS: [f32; 2] = [220.0, 320.0];
const PLATE_SLACK: f32 = 40.0;

impl Ai {
    /// The factory type whose units `def` builds: a plate's factory, else
    /// its own name.
    pub(crate) fn fac_tech(&self, def: UnitDef) -> String {
        def.custom_param("child_of_factory").unwrap_or_else(|| self.defs.name(def).to_owned())
    }

    /// The plate of factory type `name`, if it has one.
    fn eco_plate_of(&self, name: &str) -> Option<UnitDef> {
        let plate = self.defs.get(&format!("plate{}", name.strip_prefix("factory")?))?;
        (plate.custom_param("child_of_factory").as_deref() == Some(name)).then_some(plate)
    }

    /// The allied factory of type `name` we would plate: standing, safe,
    /// near our base; the one nearest our factories.
    fn eco_plate_parent(&self, name: &str) -> Option<Unit> {
        let homes = self.eco_homes();
        (self.cb.friendly_units().into_iter())
            .filter(|u| {
                u.team() != self.team_id && u.health() > 0.0 && !u.is_being_built() && self.defs.name_of(*u) == name
            })
            .filter(|u| self.eco_ally_site_safe(u.pos()))
            .map(|u| (u, homes.iter().map(|&h| distance(h, u.pos())).fold(f32::MAX, f32::min)))
            .filter(|&(_, d)| d <= ASSIST_REACH)
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(u, _)| u)
    }

    /// Where a `plate` goes by `parent`, and its facing: in range, off the
    /// factory's exit, with its own exit clearest.
    fn eco_plate_site(&self, plate: UnitDef, parent: Unit) -> Option<(Vec3, i32)> {
        let (ppos, pdef) = (parent.pos(), parent.def()?);
        let pface = parent.building_facing();
        let lane = keeps_exit(self.defs.name(pdef)).then(|| exit_lane(pdef, ppos, pface));
        let (hx, hz) = (plate.x_size() as f32 * 4.0, plate.z_size() as f32 * 4.0);
        let in_lane =
            |p: Vec3| lane.is_some_and(|l| p.x + hx > l[0] && p.x - hx < l[2] && p.z + hz > l[1] && p.z - hz < l[3]);
        let exit = self.eco_exit_check(plate);
        let facings = [pface, (pface + 1) % 4, (pface + 3) % 4, (pface + 2) % 4];
        let mut best: Option<(Vec3, i32, f32)> = None;
        for r in PLATE_RINGS {
            for k in 0..8 {
                let a = k as f32 * std::f32::consts::TAU / 8.0;
                let seed = Vec3::xz(ppos.x + r * a.cos(), ppos.z + r * a.sin());
                let Some((p, f, cost)) = self.eco_factory_exit(plate, seed, facings, exit.as_ref()) else { continue };
                if distance(p, ppos) > PLATE_RANGE - PLATE_SLACK || in_lane(p) {
                    continue;
                }
                if best.is_none_or(|b| cost < b.2) {
                    best = Some((p, f, cost));
                }
            }
        }
        best.map(|(p, f, _)| (p, f))
    }

    /// Instead of a factory of type `factory`: a plate on an ally's, if
    /// one stands that we would plate. The plate's job, if one was made.
    pub(super) fn eco_plate_instead(&mut self, wid: i32, factory: UnitDef) -> Option<TaskId> {
        if self.allies.is_empty() || self.dev_off("plates") {
            return None;
        }
        let name = self.defs.name(factory).to_owned();
        let plate = self.eco_plate_of(&name)?;
        let parent = self.eco_plate_parent(&name)?;
        let (pos, facing) = self.eco_plate_site(plate, parent)?;
        let ct = WorkerTask::construction(plate, pos, facing);
        if self.eco.tasks.contains(&self.eco.factory_tasks, &ct) || !self.eco_can_reach(wid, pos) {
            return None;
        }
        let pp = parent.pos();
        self.log(&format!(
            "economy: plating team {}'s {name} at ({:.0}, {:.0}): a {} at ({:.0}, {:.0}) facing {facing} instead of a \
             factory of our own",
            parent.team(),
            pp.x,
            pp.z,
            self.defs.name(plate),
            pos.x,
            pos.z
        ));
        let id = self.eco.tasks.insert(ct);
        self.eco.construction_tasks.push(id);
        self.eco.factory_tasks.push(id);
        Some(id)
    }

    /// Whether `plate` has a factory to work from now.
    fn eco_plate_working(plate: Unit) -> bool {
        plate.rules_param_float("nofactory", 0.0) <= 0.0
    }

    /// One of our plates stands: ours to produce from while it has a
    /// factory (see [`Self::eco_watch_plates`]).
    pub(crate) fn eco_plate_finished(&mut self, plate: Unit) {
        if !self.eco.plates.contains(&plate) {
            self.eco.plates.push(plate);
        }
        let Some(def) = plate.def() else { return };
        let tech = self.fac_tech(def);
        if Self::eco_plate_working(plate) {
            self.log(&format!("economy: {} finished; it builds {tech} units", self.defs.name(def)));
            self.fac_register(plate, &tech);
        } else {
            self.log(&format!("economy: {} finished without a {tech} in range", self.defs.name(def)));
        }
    }

    /// Every second: plates leave our factories while they have no
    /// factory, and come back once they have one.
    pub(super) fn eco_watch_plates(&mut self) {
        self.eco.plates.retain(|u| u.health() > 0.0 && u.team() == self.team_id);
        for plate in self.eco.plates.clone() {
            let Some(def) = plate.def() else { continue };
            let producing = self.fac.factories.contains_key(&plate.id());
            let working = Self::eco_plate_working(plate);
            let p = plate.pos();
            if producing && !working {
                self.log(&format!(
                    "economy: the {} at ({:.0}, {:.0}) lost its factory; it builds nothing",
                    self.defs.name(def),
                    p.x,
                    p.z
                ));
                self.fac.factories.remove(&plate.id());
            } else if !producing && working {
                let tech = self.fac_tech(def);
                self.log(&format!(
                    "economy: the {} at ({:.0}, {:.0}) has a {tech} again",
                    self.defs.name(def),
                    p.x,
                    p.z
                ));
                self.fac_register(plate, &tech);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::keeps_exit;

    #[test]
    fn plates_keep_their_exits_as_their_factories_do() {
        for (factory, plate) in
            [("factorycloak", "platecloak"), ("factoryveh", "plateveh"), ("factoryship", "plateship")]
        {
            assert!(keeps_exit(factory) && keeps_exit(plate));
        }
        for name in ["factoryplane", "plateplane", "factorygunship", "plategunship", "striderhub", "staticmex"] {
            assert!(!keeps_exit(name), "{name}");
        }
    }
}
