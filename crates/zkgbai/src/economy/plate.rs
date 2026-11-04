//! Factory plates on allied factories. A plate (150 metal, against 700 for a
//! factory, with a factory's build power) builds what a factory of its type
//! within [`PLATE_RANGE`] builds, and an ally's factory will do: Zero-K's
//! `unit_factory_plate.lua` compares ally teams, not teams. So when we want
//! a factory of a type an ally already has standing, we plate the ally's
//! instead and build those units ourselves, the tech for a fifth of the
//! price. Not for the opening (the commander's plop is free) and not for
//! strider hubs (no plates).
//!
//! An ally's factory still rising will do too: the plate starts work when
//! the factory is done. Allied ZKGBAIs often want a type at once (all three
//! asked for an air plant within 20 frames on Calamity, 2026-11-02, from
//! the same enemy-AA read, and each built one before any stood), so with
//! no allied factory of the type to plate, the factory goes by turns (see
//! `allied`): the one whose turn it is builds it, the others plate it.
//!
//! A plate whose factory is gone builds nothing (the gadget's `nofactory`
//! rules param): it leaves our factories until a factory of its type stands
//! in range again, which the gadget finds by itself. With no factory of its
//! type standing or rising in range for [`ORPHANED`], it is blown up, so
//! the type can be had again (a plate of ours counts as having it).

use springai::{Unit, UnitDef, Vec3};

use super::TaskId;
use super::WorkerTask;
use super::allied::ASSIST_REACH;
use super::planning::{AIR_STALL_WHY, exit_lane, keeps_exit};
use crate::Ai;
use crate::military::airstall::AIR_FACTORIES;
use crate::util::distance;

/// How far from its factory a plate works (`FACTORY_PLATE_RANGE`), elmos.
const PLATE_RANGE: f32 = 420.0;
/// Plate sites are sought this far from the factory's centre, elmos, and
/// kept this far inside the range.
const PLATE_RINGS: [f32; 2] = [220.0, 320.0];
const PLATE_SLACK: f32 = 40.0;
/// A plate with no factory of its type in range this long goes, frames.
const ORPHANED: i32 = 900;

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

    /// The allied factory of type `name` we would plate: standing (else
    /// rising), safe, near our base; the one nearest our factories.
    fn eco_plate_parent(&self, name: &str) -> Option<Unit> {
        let homes = self.eco_homes();
        (self.eco.allied.factories.iter().copied())
            .filter(|u| u.team() != self.team_id && u.health() > 0.0 && self.defs.name_of(*u) == name)
            .filter(|u| self.eco_ally_site_safe(u.pos()))
            .map(|u| (u, homes.iter().map(|&h| distance(h, u.pos())).fold(f32::MAX, f32::min)))
            .filter(|&(_, d)| d <= ASSIST_REACH)
            .min_by(|a, b| (a.0.is_being_built(), a.1).partial_cmp(&(b.0.is_being_built(), b.1)).unwrap())
            .map(|(u, _)| u)
    }

    /// Whether we may build a factory of type `name` now, with no ally's
    /// to plate: always for types without plates, else in our turn (see
    /// `allied`), counting the alliance's factories of the type.
    pub(super) fn eco_factory_turn(&mut self, name: &str) -> bool {
        if self.allies.is_empty() || self.dev_off("plates") || self.eco_plate_of(name).is_none() {
            return true;
        }
        let count =
            (self.eco.allied.factories.iter()).filter(|u| u.health() > 0.0 && self.defs.name_of(**u) == name).count();
        self.eco_ally_turn_at(name, count)
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

    /// `airstall`: for an AI with nothing but air factories whose air does
    /// not pay, a requisition for the type of the nearest allied ground
    /// factory we would plate (worth the window's losses).
    pub(super) fn eco_air_stall_requisition(&self) -> Option<(String, String, f32)> {
        if !self.air_stalled() || !self.air_only() || self.allies.is_empty() || self.dev_off("plates") {
            return None;
        }
        let homes = self.eco_homes();
        let near = |u: Unit| homes.iter().map(|&h| distance(h, u.pos())).fold(f32::MAX, f32::min);
        let mut types: Vec<String> = (self.eco.allied.factories.iter())
            .filter(|u| u.team() != self.team_id && u.health() > 0.0)
            .map(|&u| self.defs.name_of(u).to_owned())
            .filter(|n| !AIR_FACTORIES.contains(&n.as_str()) && self.eco_plate_of(n).is_some())
            .collect();
        types.sort();
        types.dedup();
        let (name, parent) = (types.into_iter())
            .filter_map(|n| self.eco_plate_parent(&n).map(|p| (n, p)))
            .min_by(|a, b| near(a.1).total_cmp(&near(b.1)))?;
        let (killed, lost) = self.war.air.trade();
        let why = format!(
            "{AIR_STALL_WHY}: our aircraft killed {killed:.0} metal for {lost:.0} lost in 5 minutes; a plate on team \
             {}'s {name}",
            parent.team()
        );
        Some((name, why, lost))
    }

    /// Why no plate of factory type `name` went up for worker `wid` (for
    /// the air stall's log).
    pub(super) fn eco_plate_blocked(&mut self, wid: i32, name: &str) -> &'static str {
        let Some(plate) = self.eco_plate_of(name) else { return "the type has no plate" };
        let Some(parent) = self.eco_plate_parent(name) else { return "no allied one safe and near our base now" };
        let Some((pos, facing)) = self.eco_plate_site(plate, parent) else { return "no site by the ally's" };
        if self.eco.tasks.contains(&self.eco.factory_tasks, &WorkerTask::construction(plate, pos, facing)) {
            return "one planned already";
        }
        if !self.eco_can_reach(wid, pos) { "out of the worker's reach" } else { "no reason found" }
    }

    /// `airstall`: nothing but air factories, air stalled, and no allied
    /// ground factory to plate: the metal goes to a superweapon.
    pub(super) fn eco_air_stall_super(&self) -> bool {
        self.air_stalled() && self.air_only() && self.eco_air_stall_requisition().is_none()
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
        let plates: Vec<i32> = self.eco.plates.iter().map(|u| u.id()).collect();
        self.eco.plate_orphaned.retain(|id, _| plates.contains(id));
        for plate in self.eco.plates.clone() {
            let Some(def) = plate.def() else { continue };
            let tech = self.fac_tech(def);
            let near = (self.cb.friendly_units_in_cylinder(plate.pos(), PLATE_RANGE).into_iter())
                .any(|u| u.health() > 0.0 && self.defs.name_of(u) == tech);
            if near {
                self.eco.plate_orphaned.remove(&plate.id());
            } else {
                let since = *self.eco.plate_orphaned.entry(plate.id()).or_insert(self.frame);
                if self.frame - since > ORPHANED {
                    let p = plate.pos();
                    self.log(&format!(
                        "economy: no {tech} in range of the {} at ({:.0}, {:.0}) for {} s; blowing it up",
                        self.defs.name(def),
                        p.x,
                        p.z,
                        ORPHANED / 30
                    ));
                    self.fac.factories.remove(&plate.id());
                    plate.self_destruct(0);
                    self.eco.plate_orphaned.remove(&plate.id());
                    continue;
                }
            }
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
