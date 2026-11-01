//! Opening factory diversity without a pregame queue relay. UnitCreated
//! reaches the owning AI only, so observe allied placements through the
//! friendly-unit snapshot before issuing or refreshing our build orders.
//!
//! Allies spread their openings over movement types (see [`movetype`]):
//! a later pick prefers a type whose units move unlike any ally's, then a
//! type no ally has, then whatever plays the map. And where much of our
//! side's metal lies under water, one allied ZKGBAI takes the water side
//! (see [`Ai::eco_water_side`]): the start in our box nearest that metal,
//! and an amphibious or hover opening (user, 2026-11-02: "in a Folsom 3v3,
//! at least one gets a water-capable factory ... picking the northern
//! spot"). Every instance decides it alike in the pregame, from the map
//! and the start boxes, so no message is needed.

use std::collections::BTreeSet;

use springai::Vec3;
use zk_terrain::Tier;

use super::planning::factory_tier;
use crate::Ai;
use crate::util::{NULLPOS, distance};

/// Land constructors wade this deep, elmos (`maxWaterDepth`).
const WADE: f32 = 22.0;
/// A land constructor builds this far from where it stands, about, elmos.
const WET_REACH: f32 = 120.0;
/// The share of our side's metal under water from which one of us takes
/// the water side.
const WATER_NEED: f32 = 0.12;
/// Openings whose constructors take metal under water and on land too (a
/// shipyard's take no land).
pub(crate) const WATER_FACTORIES: [&str; 2] = ["factoryamph", "factoryhover"];

/// How a factory's units move, for spreading the alliance's openings: the
/// terrain tier, aircraft as one, else the factory itself.
fn movetype(name: &str) -> &str {
    if matches!(name, "factoryplane" | "factorygunship") {
        return "air";
    }
    match factory_tier(name) {
        Some(Tier::Bot) => "bot",
        Some(Tier::Vehicle) => "vehicle",
        Some(Tier::Hover) => "hover",
        Some(Tier::Amphib) => "amph",
        Some(Tier::Spider) => "spider",
        Some(Tier::Ship) => "ship",
        _ => name,
    }
}

/// Ten seconds for an earlier controller to declare or place its factory.
/// Stagger the fallback by rank, too: a missing AI must neither block the
/// opening forever nor release every waiting controller on the same frame.
const WAIT: i32 = 300;
const SLOT: i32 = 30;

fn waits(frame: i32, team: i32, peers: &BTreeSet<i32>, known: impl Fn(i32) -> bool) -> bool {
    let rank = peers.range(..team).count() as i32;
    frame < WAIT + SLOT * rank && peers.range(..team).any(|&t| !known(t))
}

/// Of the `viable` openings, those whose units move unlike any of the
/// `used` ones', else those not used, else all.
fn diverse(viable: &BTreeSet<String>, used: &BTreeSet<String>) -> BTreeSet<String> {
    let moved: BTreeSet<&str> = used.iter().map(|u| movetype(u)).collect();
    let fresh: BTreeSet<_> = viable.iter().filter(|v| !moved.contains(movetype(v))).cloned().collect();
    if !fresh.is_empty() {
        return fresh;
    }
    let unused: BTreeSet<_> = viable.difference(used).cloned().collect();
    // A duplicate that can play the map is better than no factory.
    if unused.is_empty() { viable.clone() } else { unused }
}

impl Ai {
    fn eco_allied_plop_types(&self) -> BTreeSet<String> {
        let mut used: BTreeSet<_> = self.iq.placed_factories.values().flatten().cloned().collect();
        // Real placements trump queues. Among plans, humans and lower AI
        // team ids win; higher ids must not make us yield back and forth.
        for (&team, plans) in &self.iq.teams {
            if self.iq.placed_factories.contains_key(&team)
                || (self.iq.ai_allies.contains(&team) && team > self.team_id)
            {
                continue;
            }
            used.extend(plans.iter().filter(|p| p.def_name.starts_with("factory")).map(|p| p.def_name.clone()));
        }
        used
    }

    pub(super) fn eco_plop_choices(&self, viable: &BTreeSet<String>) -> BTreeSet<String> {
        // The water side opens for water, if any such opening plays the map.
        if self.eco.water_side.is_some_and(|(team, _)| team == self.team_id) {
            let wet: BTreeSet<String> =
                viable.iter().filter(|f| WATER_FACTORIES.contains(&f.as_str())).cloned().collect();
            if !wet.is_empty() {
                return wet;
            }
        }
        diverse(viable, &self.eco_allied_plop_types())
    }

    /// Whether a land constructor cannot build at `p`: deeper than it
    /// wades, with no ground it wades to within its reach.
    fn eco_wet(&self, p: Vec3) -> bool {
        let map = self.cb.map();
        let deep = |x: f32, z: f32| map.elevation_at(x, z) < -WADE;
        deep(p.x, p.z)
            && (0..8).all(|k| {
                let a = k as f32 * std::f32::consts::TAU / 8.0;
                deep(p.x + WET_REACH * a.cos(), p.z + WET_REACH * a.sin())
            })
    }

    /// The alliance's water side, from what every allied instance knows
    /// alike in the pregame: with another allied ZKGBAI and at least
    /// [`WATER_NEED`] of our side's metal (spots nearer our start box than
    /// the enemy's) where land constructors cannot build, the last of us in
    /// team order takes the spot in our start box nearest that metal and
    /// opens for water. (team, start spot)
    pub(crate) fn eco_water_side(&self) -> Option<(i32, Vec3)> {
        if self.iq.peers.is_empty() || self.dev_off("waterside") {
            return None;
        }
        let (base, enemy) = (self.graph.get_ally_base(), self.graph.get_enemy_base()?);
        let starts: Vec<Vec3> = self.graph.get_ally_territory().iter().map(|s| s.position).collect();
        if base == NULLPOS || starts.len() < 2 {
            return None;
        }
        let ours = (self.graph.metal_spots.iter()).filter(|s| distance(s.position, base) < distance(s.position, enemy));
        let (mut total, mut wet, mut wx, mut wz) = (0.0, 0.0, 0.0, 0.0);
        for s in ours {
            total += s.value;
            if self.eco_wet(s.position) {
                wet += s.value;
                (wx, wz) = (wx + s.value * s.position.x, wz + s.value * s.position.z);
            }
        }
        let share = if total > 0.0 { wet / total } else { 0.0 };
        if share < WATER_NEED {
            self.log(&format!("economy: {:.0}% of our side's metal is under water: no water side", 100.0 * share));
            return None;
        }
        let at = Vec3::xz(wx / wet, wz / wet);
        let team = self.iq.peers.iter().copied().chain([self.team_id]).max()?;
        let site = starts.into_iter().min_by(|a, b| distance(*a, at).total_cmp(&distance(*b, at)))?;
        self.log(&format!(
            "economy: {:.0}% of our side's metal is under water, about ({:.0}, {:.0}): team {team} takes the water side, \
             starting at ({:.0}, {:.0})",
            100.0 * share,
            at.x,
            at.z,
            site.x,
            site.z
        ));
        Some((team, site))
    }

    pub(super) fn eco_wait_for_allied_plop(&self) -> bool {
        waits(self.frame, self.team_id, &self.iq.peers, |team| {
            self.iq.placed_factories.contains_key(&team)
                || self.iq.teams.get(&team).is_some_and(|ps| ps.iter().any(|p| p.def_name.starts_with("factory")))
        })
    }

    pub(super) fn eco_coordinate_plops(&mut self) {
        if self.allies.is_empty() || !self.eco.workers.values().any(|w| w.has_plop()) {
            return;
        }
        self.iq.placed_factories.clear();
        for u in self.cb.friendly_units() {
            let team = u.team();
            if !self.allies.contains(&team) || u.health() <= 0.0 {
                continue;
            }
            let name = self.defs.name_of(u);
            if name.starts_with("factory") {
                self.iq.placed_factories.entry(team).or_default().insert(name.to_owned());
            }
        }
        // Explicit player/developer choices remain binding.
        if !self.eco.user_fac.is_empty() || self.eco.dev_pick.is_some() {
            return;
        }
        let used = self.eco_allied_plop_types();
        let duplicates: Vec<_> = self
            .eco
            .factory_tasks
            .iter()
            .filter_map(|&id| {
                let c = self.eco.tasks.get(id)?.as_construction()?;
                let worker = self.eco.workers.get(&c.assigned_plop)?;
                let name = self.defs.name(c.build_type);
                (c.fac_plop
                    && c.target.is_none()
                    && worker.has_plop()
                    && used.contains(name)
                    && self.eco.potential_fac_list.iter().any(|f| !used.contains(f)))
                .then(|| (id, c.assigned_plop, name.to_owned()))
            })
            .collect();
        for (id, wid, name) in duplicates {
            self.log(&format!("economy: ally placed or queued {name}; replanning our unspent plop"));
            self.eco.drop_construction_task(id);
            if let Some(w) = self.eco.workers.get_mut(&wid) {
                w.assigned_plop = false;
            }
            self.eco.potential_fac_list.insert(name);
            self.eco.opening = None;
            self.eco.opening_site = None;
            self.iq.sent = false;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(ns: &[&str]) -> BTreeSet<String> {
        ns.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn no_queue_relay_sequences_three_controllers_by_actual_placements() {
        let peers = BTreeSet::from([0, 2, 5]);
        assert!(!waits(10, 0, &peers, |_| false));
        assert!(waits(10, 2, &peers, |_| false));
        assert!(waits(10, 5, &peers, |_| false));
        assert!(!waits(20, 2, &peers, |t| t == 0));
        assert!(waits(20, 5, &peers, |t| t == 0));
        assert!(!waits(30, 5, &peers, |t| t == 0 || t == 2));
        let viable = names(&["factorygunship", "factorycloak", "factoryveh"]);
        let used = names(&["factorygunship"]);
        assert!(!diverse(&viable, &used).contains("factorygunship"));
        assert_eq!(diverse(&viable, &names(&["factorygunship", "factorycloak"])), names(&["factoryveh"]));
    }

    #[test]
    fn later_picks_prefer_units_that_move_unlike_the_allies() {
        let viable = names(&["factorycloak", "factoryshield", "factoryveh", "factoryamph"]);
        // A cloak ally: no more bots.
        assert_eq!(diverse(&viable, &names(&["factorycloak"])), names(&["factoryveh", "factoryamph"]));
        // Bots and vehicles taken: the amphibians.
        assert_eq!(diverse(&viable, &names(&["factoryshield", "factorytank"])), names(&["factoryamph"]));
        // Every movement type taken: any type not taken.
        let bots = names(&["factorycloak", "factoryshield"]);
        assert_eq!(diverse(&bots, &names(&["factoryjump"])), bots);
        assert_eq!(diverse(&bots, &names(&["factorycloak"])), names(&["factoryshield"]));
        // Aircraft move as one.
        assert_eq!(movetype("factoryplane"), movetype("factorygunship"));
        assert_ne!(movetype("factoryhover"), movetype("factoryamph"));
    }

    #[test]
    fn unavailable_predecessors_have_staggered_bounded_waits() {
        let peers = BTreeSet::from([0, 2, 5]);
        assert!(waits(329, 2, &peers, |_| false));
        assert!(!waits(330, 2, &peers, |_| false));
        assert!(waits(330, 5, &peers, |_| false));
        assert!(!waits(360, 5, &peers, |_| false));
        assert!(!waits(0, 0, &BTreeSet::new(), |_| false));
    }

    #[test]
    fn diversity_never_adds_unviable_types_or_blocks_an_exhausted_pool() {
        let viable = names(&["factoryship"]);
        assert_eq!(diverse(&viable, &names(&["factoryship", "factorygunship"])), viable);
        assert_eq!(diverse(&viable, &BTreeSet::new()), viable);
        assert!(diverse(&BTreeSet::new(), &viable).is_empty());
    }

    use crate::economy::{Worker, WorkerTask};
    use springai::{Callback, SkirmishAi, Unit, Vec3, ffi};
    use std::cell::Cell;
    use std::ffi::{c_char, c_void};

    thread_local! {
        static STOPS: Cell<usize> = const { Cell::new(0) };
        static DEFS_READY: Cell<bool> = const { Cell::new(false) };
    }

    fn pending_plop() -> (Ai, super::super::TaskId) {
        unsafe extern "C" fn ids(_: i32, out: *mut i32, max: i32) -> i32 {
            if !DEFS_READY.with(Cell::get) {
                return 0;
            }
            if !out.is_null() && max >= 2 {
                unsafe {
                    out.write(1);
                    out.add(1).write(2);
                }
            }
            2
        }
        unsafe extern "C" fn name(_: i32, id: i32) -> *const c_char {
            if id == 1 { c"dyntrainer".as_ptr() } else { c"factorygunship".as_ptr() }
        }
        unsafe extern "C" fn def(_: i32, id: i32) -> i32 {
            if id == 10 { 1 } else { 2 }
        }
        unsafe extern "C" fn team(_: i32, id: i32) -> i32 {
            if id == 10 { 2 } else { 0 }
        }
        unsafe extern "C" fn health(_: i32, _: i32) -> f32 {
            1000.0
        }
        unsafe extern "C" fn zero(_: i32, _: i32) -> i32 {
            0
        }
        unsafe extern "C" fn power(_: i32, _: i32) -> f32 {
            10.0
        }
        unsafe extern "C" fn rules(_: i32, _: i32, _: *const c_char, _: f32) -> f32 {
            1.0
        }
        unsafe extern "C" fn units(_: i32, out: *mut i32, max: i32) -> i32 {
            if !out.is_null() && max > 0 {
                unsafe {
                    out.write(20);
                }
            }
            1
        }
        unsafe extern "C" fn command(_: i32, _: i32, _: i32, _: i32, _: *mut c_void) -> i32 {
            STOPS.with(|s| s.set(s.get() + 1));
            0
        }
        DEFS_READY.with(|v| v.set(false));
        let mut ai = test_ai_with(|t| {
            t.getUnitDefs = Some(ids);
            t.UnitDef_getName = Some(name);
            t.UnitDef_getBuildSpeed = Some(power);
            t.Unit_getDef = Some(def);
            t.Unit_getTeam = Some(team);
            t.Unit_getHealth = Some(health);
            t.Unit_getWeapons = Some(zero);
            t.Unit_getRulesParamFloat = Some(rules);
            t.getFriendlyUnits = Some(units);
            t.Engine_handleCommand = Some(command);
        });
        // Populate just the name cache after constructing the unrelated
        // managers; this fixture does not model their combat definitions.
        DEFS_READY.with(|v| v.set(true));
        ai.defs = crate::defs::Defs::new(ai.cb);
        ai.allies = vec![0, 4];
        ai.iq.ai_allies = BTreeSet::from([0, 4]);
        ai.iq.peers = BTreeSet::from([0, 4]);
        ai.eco.potential_fac_list = names(&["factoryveh"]);
        ai.eco.opening = Some("factorygunship".into());
        let def = ai.defs.get("factorygunship").unwrap();
        ai.eco.opening_site = Some((def, Vec3::xz(100.0, 100.0), 0));
        ai.iq.sent = true;
        let mut task = WorkerTask::construction_plop(def, Vec3::xz(100.0, 100.0), 0, true);
        task.as_construction_mut().unwrap().assigned_plop = 10;
        let id = ai.eco.tasks.insert(task);
        ai.eco.factory_tasks.push(id);
        ai.eco.construction_tasks.push(id);
        let mut worker = Worker::new(Unit::from_id(ai.cb, 10).unwrap(), ai.team_id);
        worker.assigned_plop = true;
        worker.set_task(&mut ai.eco.tasks, id, 1);
        ai.eco.workers.insert(10, worker);
        ai.eco.assigned.push_back(10);
        STOPS.with(|s| s.set(0));
        (ai, id)
    }

    #[test]
    fn allied_placement_cancels_only_the_unspent_duplicate_and_releases_its_commander() {
        let (mut ai, id) = pending_plop();
        assert!(ai.eco_wait_for_allied_plop());
        ai.eco_coordinate_plops();
        assert!(!ai.eco_wait_for_allied_plop());
        assert!(ai.eco.tasks.get(id).is_none());
        assert!(ai.eco.factory_tasks.is_empty());
        assert!(ai.eco.construction_tasks.is_empty());
        assert!(!ai.eco.workers[&10].assigned_plop);
        assert!(ai.eco.workers[&10].task.is_none());
        assert!(ai.eco.idlers.contains(&10));
        assert!(!ai.eco.assigned.contains(&10));
        assert!(ai.eco.opening.is_none());
        assert!(ai.eco.opening_site.is_none());
        assert!(!ai.iq.sent);
        assert_eq!(ai.eco_plop_choices(&ai.eco.potential_fac_list), names(&["factoryveh"]));
        STOPS.with(|s| assert_eq!(s.get(), 1));
        ai.eco_coordinate_plops();
        STOPS.with(|s| assert_eq!(s.get(), 1));
    }

    #[test]
    fn preserves_started_factories_explicit_choices_and_exhausted_diversity() {
        for case in 0..5 {
            let (mut ai, id) = pending_plop();
            match case {
                0 => ai.eco.tasks.get_mut(id).unwrap().as_construction_mut().unwrap().target = Unit::from_id(ai.cb, 30),
                1 => ai.eco.user_fac = "factorygunship".into(),
                2 => ai.eco.dev_pick = Some(1),
                3 => ai.eco.potential_fac_list.clear(),
                _ => ai.eco.tasks.get_mut(id).unwrap().as_construction_mut().unwrap().fac_plop = false,
            }
            ai.eco_coordinate_plops();
            assert!(ai.eco.tasks.get(id).is_some(), "case {case}");
            assert!(ai.eco.workers[&10].assigned_plop);
            STOPS.with(|s| assert_eq!(s.get(), 0));
        }
    }

    #[test]
    fn queue_priority_is_asymmetric_and_actual_placements_override_stale_plans() {
        let (mut ai, _) = pending_plop();
        let plan =
            |name: &str| crate::iq::Plan { def_name: name.into(), def_id: 2, pos: Vec3::ZERO, face: 0, frame: 0 };
        ai.iq.teams.insert(4, vec![plan("factoryveh")]);
        assert!(ai.eco_allied_plop_types().is_empty()); // higher AI yields to us
        ai.iq.ai_allies.remove(&4); // human plans have priority regardless of id
        assert_eq!(ai.eco_allied_plop_types(), names(&["factoryveh"]));
        ai.iq.teams.insert(0, vec![plan("factorycloak")]);
        assert!(!ai.eco_wait_for_allied_plop());
        ai.eco_coordinate_plops(); // team 0 actually placed gunships
        assert_eq!(ai.eco_allied_plop_types(), names(&["factoryveh", "factorygunship"]));
    }

    fn test_ai_with(customize: impl FnOnce(&mut ffi::SSkirmishAICallback)) -> Ai {
        use std::ffi::{CStr, c_char, c_void};
        unsafe extern "C" fn size(_: i32) -> i32 {
            32
        }
        unsafe extern "C" fn team(_: i32) -> i32 {
            2
        }
        unsafe extern "C" fn zero(_: i32) -> i32 {
            0
        }
        unsafe extern "C" fn water(_: i32) -> f32 {
            0.0
        }
        unsafe extern "C" fn game_rules(_: i32, _: *const c_char, default: f32) -> f32 {
            default
        }
        unsafe extern "C" fn floats(_: i32, _: *mut f32, _: i32) -> i32 {
            0
        }
        unsafe extern "C" fn ints(_: i32, _: *mut i32, _: i32) -> i32 {
            0
        }
        unsafe extern "C" fn log(_: i32, _: *const c_char) {}
        unsafe extern "C" fn lookup(_: i32, _: *const c_char) -> i32 {
            -1
        }
        unsafe extern "C" fn resource(_: i32, _: *const c_char) -> i32 {
            0
        }
        unsafe extern "C" fn option(_: i32, _: *const c_char) -> *const c_char {
            c"off".as_ptr()
        }
        unsafe extern "C" fn defs(_: i32, _: *mut i32, _: i32) -> i32 {
            0
        }
        unsafe extern "C" fn name(_: i32, _: i32) -> *const c_char {
            c"staticcon".as_ptr()
        }
        unsafe extern "C" fn def(_: i32, _: i32) -> i32 {
            1
        }
        unsafe extern "C" fn speed(_: i32, _: i32) -> f32 {
            0.0
        }
        unsafe extern "C" fn range(_: i32, _: i32) -> f32 {
            500.0
        }
        unsafe extern "C" fn building(_: i32, _: i32) -> bool {
            false
        }
        unsafe extern "C" fn pos(_: i32, id: i32, out: *mut f32) {
            unsafe {
                std::ptr::copy_nonoverlapping([id as f32 * 100.0, 0.0, 0.0].as_ptr(), out, 3);
            }
        }
        unsafe extern "C" fn rules(_: i32, id: i32, name: *const c_char, default: f32) -> f32 {
            assert_eq!(unsafe { CStr::from_ptr(name) }, c"capture_controller");
            match id {
                10 => 0.0,
                11 => 42.0,
                12 => -1.0,
                _ => default,
            }
        }
        unsafe extern "C" fn command(_: i32, _: i32, _: i32, _: i32, _: *mut c_void) -> i32 {
            0
        }
        let mut table = ffi::SSkirmishAICallback {
            Map_getWidth: Some(size),
            Map_getHeight: Some(size),
            Map_getSlopeMap: Some(floats),
            Map_getHeightMap: Some(floats),
            Map_getLosMap: Some(ints),
            Map_getWaterDamage: Some(water),
            Mod_getLosMipLevel: Some(zero),
            Game_getMyTeam: Some(team),
            Game_getMyAllyTeam: Some(zero),
            Game_getRulesParamFloat: Some(game_rules),
            getUnitDefs: Some(defs),
            getUnitDefByName: Some(lookup),
            getResourceByName: Some(resource),
            SkirmishAI_OptionValues_getValueByKey: Some(option),
            Log_log: Some(log),
            UnitDef_getName: Some(name),
            UnitDef_getSpeed: Some(speed),
            UnitDef_getBuildDistance: Some(range),
            Unit_getDef: Some(def),
            Unit_isBeingBuilt: Some(building),
            Unit_getPos: Some(pos),
            Unit_getRulesParamFloat: Some(rules),
            Engine_handleCommand: Some(command),
            ..Default::default()
        };
        customize(&mut table);
        let cb = unsafe { Callback::from_raw(0, Box::leak(Box::new(table))) };
        Ai::new(cb)
    }
}
