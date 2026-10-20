//! Task generation: factories, energy and grid, caretakers, storage, radar,
//! static defenses, superweapons, reclaim, morphs.

use springai::{NO_TIMEOUT, Unit, UnitDef, Vec3, WeaponDef};

use super::{List, Nanotower, TaskId, WorkerTask, erase_one};
use crate::Ai;
use crate::defs::cmd;
use crate::military::missiles;
use crate::terrain::pos as terrain_pos;
use crate::util::{NULLPOS, distance, get_angular_point, get_directional_point, get_radial_point, kgb_rand};
use zk_terrain::Tier;

/// Turrets at metal spots and links stand this many times further apart
/// where enemy ground forces cannot get (about half as many).
const SHELTERED_SPACING: f32 = 2.0;

/// A factory's units reach a metal spot if they can get this close to it
/// (about a constructor's build range).
const FACTORY_REACH: f32 = 150.0;

/// Water at least this deep takes an Urchin, elmos.
const URCHIN_DEPTH: f32 = 8.0;

/// An opening factory is ruled out when its units reach less than this
/// share of what the best one's do.
const REACH_RIVAL: f32 = 0.8;

/// The share of the metal we do not hold, out of our units' reach, for
/// which another factory is asked for.
const REACH_GAP: f32 = 0.25;

/// A ground or sea factory is asked for (rather than a gunship factory)
/// when its units get to this share of the metal out of reach.
const AIR_FALLBACK: f32 = 0.7;

/// A gunship factory is asked for, from income `DROP_INCOME` on, when this
/// share of the enemy's value is out of ground reach or behind guarded
/// chokepoints the long way round (ground route this many times the
/// straight line).
const DROP_SHARE: f32 = 0.25;
const DROP_DETOUR: f32 = 1.5;
const DROP_INCOME: f32 = 30.0;

/// From this income on, a factory asked for is built (the usual second
/// factory waits for 50 and a few caretakers); and how often the asking is
/// re-assessed, in frames.
const REQUISITION_INCOME: f32 = 20.0;
const REQUISITION_PERIOD: i32 = 30 * 20;

/// Ground and sea factories, by the tier of their units.
const GROUND_FACTORIES: [&str; 9] = [
    "factorycloak",
    "factoryshield",
    "factoryjump",
    "factoryveh",
    "factorytank",
    "factoryhover",
    "factoryamph",
    "factoryspider",
    "factoryship",
];

/// Movement tier of a factory's units; `None` for aircraft and striders.
fn factory_tier(name: &str) -> Option<Tier> {
    match name {
        "factorycloak" | "factoryshield" | "factoryjump" => Some(Tier::Bot),
        "factoryveh" | "factorytank" => Some(Tier::Vehicle),
        "factoryhover" => Some(Tier::Hover),
        "factoryamph" => Some(Tier::Amphib),
        "factoryspider" => Some(Tier::Spider),
        "factoryship" => Some(Tier::Ship),
        _ => None,
    }
}

/// Turrets within this distance of a chokepoint's defense position count as
/// its defenses.
const CHOKE_PORC_RADIUS: f32 = 300.0;

impl Ai {
    /// The common `FindClosestBuildSite(def, pos, 600, 3, facing)`.
    pub(super) fn eco_build_site(&self, def: UnitDef, pos: Vec3, facing: i32) -> Vec3 {
        self.cb.map().find_closest_build_site(def, pos, 600.0, 3, facing)
    }

    /// The common "if buildCheck(ct) && !CONTAINSP(dedup, ct): add to
    /// constructionTasks (and `extra`)" step. Returns the new task id.
    fn eco_add_construction(&mut self, task: WorkerTask, dedup: Option<List>, extra: Option<List>) -> Option<TaskId> {
        let c = task.as_construction()?;
        if !self.eco_build_check(c.build_type, task.position, c.facing) {
            return None;
        }
        if dedup.is_some_and(|l| self.eco.tasks.contains(self.eco.list(l), &task)) {
            return None;
        }
        let id = self.eco.tasks.insert(task);
        self.eco.construction_tasks.push(id);
        if let Some(l) = extra {
            self.eco.list_mut(l).push(id);
        }
        Some(id)
    }

    /// Construction task for `def` at `pos` with facing 0, deduplicated
    /// against `dedup`.
    pub(super) fn eco_add_simple(
        &mut self,
        def: UnitDef,
        pos: Vec3,
        dedup: List,
        extra: Option<List>,
    ) -> Option<TaskId> {
        self.eco_add_construction(WorkerTask::construction(def, pos, 0), Some(dedup), extra)
    }

    fn eco_can_reach(&mut self, wid: i32, pos: Vec3) -> bool {
        let Some(w) = self.eco.workers.get(&wid) else {
            return false;
        };
        let mut nav = crate::nav!(self);
        w.can_reach(pos, &mut nav)
    }

    pub(super) fn eco_generate_tasks(&mut self, wid: i32) {
        let frame = self.frame;
        let Some(worker) = self.eco.workers.get(&wid) else {
            return;
        };
        let position = worker.get_pos();
        let (is_com, is_greedy, assigned_plop) = (worker.is_com, worker.is_greedy, worker.assigned_plop);

        // do we need a factory?
        let has_plop = worker.has_plop();
        {
            let eco = &self.eco;
            let nfac = self.fac.factories.len() as i32;
            let merged = self.merged_allies;
            let tf = self.graph.territory_fraction;
            let eminent = self.graph.eminent_territory;
            let no_fac_tasks = eco.factory_tasks.is_empty();
            let nnanos = eco.nanos.len();
            if (has_plop && !assigned_plop)
                || (frame > 900 && !has_plop && nfac < 1 + merged && no_fac_tasks)
                // a factory a module asked for
                || (eco.requisition.is_some()
                    && eco.static_income > REQUISITION_INCOME
                    && nfac >= 1
                    && nfac < 3 + merged
                    && no_fac_tasks)
                || (eco.static_income > 50.0 && nfac == 1 && nnanos > 3 && no_fac_tasks)
                || (eco.static_income > 90.0 && nfac == 2 && nnanos > 6 && eminent && no_fac_tasks)
                || (merged > 0 && eco.static_income > 65.0 && !eco.has_striders && eminent && no_fac_tasks)
                || (merged > 0
                    && eco.static_income > 65.0
                    && tf > 0.35
                    && (!eco.has_planes && !eco.has_gunship)
                    && no_fac_tasks)
                || (merged > 0
                    && eco.big_map
                    && tf > 0.4
                    && (!self.misc_handler.striders.is_empty() || !self.squad_handler.shield_squads.is_empty())
                    && (!eco.has_planes || !eco.has_gunship)
                    && no_fac_tasks)
            {
                self.eco_create_factory_task(wid, has_plop);
            }
        }

        if !is_com {
            self.eco_collect_reclaimables(wid);
        }

        // do we need AA?
        if self.eco.enemy_has_air
            && self.war.max_enemy_air_value > 1000.0
            && self.graph.territory_fraction > 0.4
            && self.eco.aa_tasks.is_empty()
        {
            self.eco_create_aa_task(position);
        }

        // is there sufficient energy to cover metal income?
        if !is_greedy
            && self.eco.estalled
            && ((self.eco.solar_tasks.len() + self.eco.wind_tasks.len()) as i32) < self.fac.num_workers
        {
            self.eco_create_energy_task(position);
        }
        self.eco_create_fusion_task();

        // do we need caretakers?
        if !self.fac.factories.is_empty()
            && ((self.eco.nanos.len()
                + self.eco.nano_tasks.len()
                + self.fac.factories.len()
                + self.eco.factory_tasks.len()) as f32)
                < (self.eco.static_income / 10.0).floor()
            && let Some(target) = self.eco_get_caretaker_target()
        {
            self.eco_create_nano_turret_task(wid, target);
        }

        //do we need storages?
        if frame > 600
            && !self.fac.factories.is_empty()
            && (self.eco.metal / self.eco.max_storage > 0.8
                || ((self.eco.commanders.len() + self.eco.storages.len()) as i32) < 1 + self.merged_allies)
            && self.eco.storage_tasks.is_empty()
            && let Some(target) = self.eco_get_caretaker_target()
        {
            self.eco_create_storage_task(wid, target);
        }

        // do we need airpads for planes
        if !is_greedy
            && !self.fac.factories.is_empty()
            && self.eco.have_planes
            && self.eco.airpad_tasks.is_empty()
            && self.eco.static_income > 15.0
            && ((self.eco.airpads.len() as f32) < (self.bomber_handler.get_bomber_size() as f32 / 8.0).round()
                || self.eco.airpads.is_empty())
            && let Some(target) = self.eco_get_caretaker_target()
        {
            self.eco_create_air_pad_task(target);
        }

        // do we need radar?
        if !is_com && self.eco.adjusted_income > 20.0 && !self.eco.hard_estalled {
            self.eco_create_radar_task(wid);
        }

        // porc push against nearby enemy porc and defend the front line
        if self.eco.defended_fac && (self.eco.porc_tasks.len() as f32) < self.fac.num_workers as f32 * 1.5 {
            self.eco_defend_chokes();
            self.eco_porc_push(position);
            if self.graph.eminent_territory {
                self.eco_defend_front(position);
            }
        }

        // do we need pylons?
        if self.eco.fusions.len() > 1 {
            // (the C++ port lowered this from > 3, since grid requirements exist)
            self.eco_create_grid_task(position);
        }

        if self.eco.big_map
            && self.eco.super_weps.len() > 3
            && !self.eco.has_super_wep
            && self.eco.super_wep_tasks.is_empty()
        {
            self.eco.chose_sup_wep = false;
        }

        // do we need superweapons?
        if !self.eco.chose_sup_wep
            && ((!self.allies.is_empty() && self.eco.has_striders && self.fac.num_striders > 1)
                || self.allies.is_empty())
            && self.eco.fusions.len() > 4
        {
            self.eco_choose_super_weapon();
        }

        // A nuke, when one strike would pay for it.
        if !self.eco.has_super_wep && self.eco.super_wep_tasks.is_empty() && self.eco.s_wep != 1 && self.nukes_wanted()
        {
            if let Some((p, v)) = self.nukes_target() {
                self.log(&format!(
                    "nukes: {v:.0} metal in one blast at ({:.0}, {:.0}) unguarded; building a silo",
                    p.x, p.z
                ));
            }
            self.eco.s_wep = 1;
            self.eco.chose_sup_wep = true;
        }

        if self.eco.chose_sup_wep {
            if (self.eco.s_wep == 0 || self.eco.has_super_wep) && self.eco.super_wep_tasks.is_empty() {
                self.eco_create_bertha_task();
            } else if self.eco.s_wep > 0 && !self.eco.has_super_wep && self.eco.super_wep_tasks.is_empty() {
                self.eco_create_super_wep_task();
            }
        }

        // do we need a protector? (also when the enemy could be hiding one)
        let suspicious = self.intel_wants_anti();
        let nuked = self.eco.got_nuked || self.nukes.enemy_detonation;
        if !self.eco.anti_nuke && (self.war.enemy_has_nuke || nuked || suspicious) {
            if suspicious && !self.war.enemy_has_nuke && !nuked {
                self.log(&format!(
                    "intel: {:.0} metal of the enemy's suspiciously unaccounted for; building an anti-nuke",
                    self.intel.suspicious()
                ));
            }
            self.eco.anti_nuke = true;
            if let Some(protector) = self.defs.get("staticantinuke") {
                let pos = self.eco_build_site(protector, self.graph.get_ally_center(), 0);
                self.eco_add_simple(protector, pos, List::Construction, None);
            }
        }
    }

    fn eco_create_factory_task(&mut self, wid: i32, is_plop: bool) {
        let nfac = self.fac.factories.len();
        let merged = self.merged_allies;
        // The first factory waits a moment for the terrain model, to know
        // whose units can get to the metal.
        if nfac == 0 && self.terrain_pending() && self.frame < 150 {
            return;
        }

        if self.eco.potential_fac_list.is_empty() && (nfac as i32) < 1 + merged {
            self.eco.potential_fac_list = self.eco.initial_fac_list.clone();
            self.eco.added_facs = false;
        }

        if nfac > 0 && !is_plop && !self.eco.added_facs {
            let have_pylons = !self.eco.pylons.is_empty();
            let list = &mut self.eco.potential_fac_list;
            list.insert("factorygunship".to_owned());

            list.insert("factoryplane".to_owned());
            if have_pylons {
                list.insert("striderhub".to_owned());
            }

            if nfac > 0 && (nfac < 3 || merged > 0) {
                // only build air or striders as second and third facs.
                for f in [
                    "factoryspider",
                    "factorycloak",
                    "factoryveh",
                    "factoryshield",
                    "factoryamph",
                    "factoryhover",
                    "factoryship",
                    "factoryjump",
                    "factorytank",
                ] {
                    list.remove(f);
                }
            }
            self.eco.added_facs = true;
        }

        // port: the Java declares a local here; the C++ dropped the
        // declaration and clobbered the `hasStriders` member instead.
        let mut has_striders = false;
        for f in self.fac.factories.values() {
            let def_name = self.defs.name_of(f.unit);
            if def_name == "striderhub" {
                has_striders = true;
            }
            // don't build the same fac twice.
            self.eco.potential_fac_list.remove(def_name);
        }

        // Which factories' units can get to the metal; a factory asked for.
        let from = self.eco.workers.get(&wid).map(|w| w.get_pos());
        if let Some(from) = from.filter(|_| nfac == 0) {
            self.eco_weigh_factories_by_reach(from);
        }
        let reach_fac = if nfac > 0 { self.eco.requisition.take().map(|(f, _)| f) } else { None };

        if self.eco.potential_fac_list.is_empty() {
            self.eco.added_facs = false;
            return;
        }

        let mut fac_name = String::new();
        let factory: Option<UnitDef>;

        // Uncomment this to set the intial fac for debugging purposes.
        /*if nfac == 0 {
            fac_name = "factoryveh".to_owned();
            factory = self.defs.get(&fac_name);
            self.eco.potential_fac_list.remove(&fac_name);
        } else*/
        if !self.eco.user_fac.is_empty() && nfac == 0 {
            factory = self.defs.get(&self.eco.user_fac);
            let user_fac = self.eco.user_fac.clone();
            self.eco.potential_fac_list.remove(&user_fac);
        } else if let Some(k) = self.eco.dev_pick.filter(|_| nfac == 0) {
            // The k-th of the factories viable here (in name order).
            let list = &self.eco.potential_fac_list;
            fac_name = list.iter().nth(k % list.len()).cloned().unwrap_or_default();
            factory = self.defs.get(&fac_name);
            self.eco.potential_fac_list.remove(&fac_name);
        } else if let Some(dev_fac) = nfac.checked_sub(1).and_then(|i| self.eco.dev_facs.get(i)).cloned() {
            factory = self.defs.get(&dev_fac);
            self.eco.potential_fac_list.remove(&dev_fac);
            fac_name = dev_fac;
        } else if let Some(reach_fac) = reach_fac {
            factory = self.defs.get(&reach_fac);
            self.eco.potential_fac_list.remove(&reach_fac);
            fac_name = reach_fac;
        } else if self.eco.potential_fac_list.contains("factoryship") && kgb_rand() < 0.5 {
            fac_name = "factoryship".to_owned();
            factory = self.defs.get(&fac_name);
            self.eco.potential_fac_list.remove(&fac_name);
        } else if is_plop && self.eco.potential_fac_list.contains("factorygunship") {
            fac_name = "factorygunship".to_owned();
            factory = self.defs.get(&fac_name);
            self.eco.potential_fac_list.remove(&fac_name);
        } else if (nfac as i32) < 1 + merged.max(1) || has_striders || is_plop || self.eco.pylons.is_empty() {
            let len = self.eco.potential_fac_list.len();
            let n = ((kgb_rand() * len as f64) as usize).min(len - 1);
            fac_name = self.eco.potential_fac_list.iter().nth(n).cloned().unwrap_or_default();
            factory = self.defs.get(&fac_name);
            self.eco.potential_fac_list.remove(&fac_name);

            // don't build two airfacs in small teams or on small maps.
            if merged < 5 && !self.eco.big_map {
                if fac_name == "factoryplane" {
                    self.eco.potential_fac_list.remove("factorygunship");
                } else if fac_name == "factorygunship" {
                    self.eco.potential_fac_list.remove("factoryplane");
                }
            }
        } else {
            fac_name = "striderhub".to_owned();
            factory = self.defs.get(&fac_name);
            self.eco.potential_fac_list.remove(&fac_name);
        }

        // port: unknown factory definitions (the C++ dereferenced them unchecked).
        let Some(factory) = factory else {
            return;
        };
        // port: the C++ re-inserted an empty name on failure for the user's choice.
        let restore = |ai: &mut Ai, name: &str| {
            if !name.is_empty() {
                ai.eco.potential_fac_list.insert(name.to_owned());
            }
        };

        let Some(wpos) = self.eco.workers.get(&wid).map(|w| w.get_pos()) else {
            return;
        };
        let mut position = wpos;

        let map_width = self.cb.map().width() * 8;
        let map_height = self.cb.map().height() * 8;

        let facing = if (map_width as f32 - 2.0 * position.x).abs() > (map_height as f32 - 2.0 * position.z).abs() {
            if 2.0 * position.x > map_width as f32 {
                3 // facing="west"
            } else {
                1 // facing="east"
            }
        } else if 2.0 * position.z > map_height as f32 {
            2 // facing="north"
        } else {
            0 // facing="south"
        };

        let map_center = self.graph.get_map_center();
        let closest_spot = |ai: &Ai, p: Vec3| ai.graph.get_closest_spot(p).map(|s| s.get_pos());
        let mut i = 0;
        let clearance = factory.x_size().max(factory.z_size()) as f32 * 4.0;
        if self.fac.factories.is_empty() || is_plop {
            loop {
                if i > 10 {
                    restore(self, &fac_name);
                    return;
                }
                i += 1;
                position = get_directional_point(wpos, map_center, clearance + 20.0);
                position = self.eco_build_site(factory, position, facing);

                // don't let factories get blocked by random crap, nor build them on top of mexes
                let mut j = 0;
                let mut closest = closest_spot(self, position);
                while !self.cb.friendly_units_in(position, clearance).is_empty()
                    || !self.cb.features_in(position, clearance).is_empty()
                    || closest.is_some_and(|c| distance(c, position) < 100.0)
                    || !self.eco_build_check(factory, position, 0)
                {
                    if j > 5 {
                        break;
                    }
                    j += 1;
                    position = if i < 8 {
                        get_angular_point(wpos, map_center, clearance + 20.0)
                    } else {
                        get_radial_point(wpos, 125.0)
                    };
                    position = self.eco_build_site(factory, position, facing);
                    closest = closest_spot(self, position);
                }
                // only for preventing stupid placement
                if self.eco_build_check(factory, position, facing) {
                    break;
                }
            }
        } else {
            loop {
                if i > 5 {
                    restore(self, &fac_name);
                    return;
                }
                // don't cram factories together
                let ally_center = self.graph.get_ally_center();
                let ally_base = self.graph.get_ally_base();
                let radius = 450.0f32.max(distance(ally_base, ally_center) * kgb_rand() as f32);
                position = get_angular_point(ally_center, ally_base, radius);
                let Some(mut facpos) = self.eco_get_nearest_fac(position).map(|f| f.get_pos()) else {
                    i += 1;
                    continue;
                };

                // don't let factories get blocked by random crap, nor build them on top of mexes
                if fac_name == "striderhub" {
                    // Talas: whatever, it grids..
                    position = self.graph.get_overdrive_sweet_spot(position, &fac_name);
                } else {
                    let mut j = 0;
                    let mut closest = closest_spot(self, position);
                    while distance(facpos, position) < 400.0
                        || !self.cb.friendly_units_in(position, 150.0).is_empty()
                        || closest.is_some_and(|c| distance(c, position) < 100.0)
                        || self.graph.is_front_line(position)
                    {
                        if j > 5 {
                            break;
                        }
                        position = get_radial_point(position, 175.0);
                        if let Some(p) = self.eco_get_nearest_fac(position).map(|f| f.get_pos()) {
                            facpos = p;
                        }
                        closest = closest_spot(self, position);
                        j += 1;
                    }
                }
                position = self.eco_build_site(factory, position, facing);

                // only for preventing stupid placement
                let closest = closest_spot(self, position);
                let good = distance(facpos, position) > 400.0
                    && self.cb.friendly_units_in(position, 150.0).is_empty()
                    && closest.is_none_or(|c| distance(c, position) > 100.0)
                    && !self.graph.is_front_line(position)
                    && self.eco_build_check(factory, position, facing);
                i += 1;
                if good {
                    break;
                }
            }
        }

        let mut ct = WorkerTask::construction_plop(factory, position, facing, is_plop);
        if self.eco_build_check(factory, position, facing)
            && !self.eco.tasks.contains(&self.eco.factory_tasks, &ct)
            && self.eco_can_reach(wid, position)
        {
            if is_plop {
                if let Some(c) = ct.as_construction_mut() {
                    c.assigned_plop = wid;
                }
                if let Some(w) = self.eco.workers.get_mut(&wid) {
                    w.assigned_plop = true;
                }
            }
            let id = self.eco.tasks.insert(ct);
            self.eco.construction_tasks.push(id);
            self.eco.factory_tasks.push(id);
        } else {
            restore(self, &fac_name);
        }
    }

    fn eco_create_aa_task(&mut self, worker_pos: Vec3) {
        let mut position = self.graph.get_closest_front_line_link(worker_pos);
        if position == NULLPOS {
            return;
        }

        let name = if !self.graph.eminent_territory || self.eco.effective_income < 30.0 || kgb_rand() > 0.5 {
            "turretaalaser"
        } else {
            "turretaafar"
        };
        let Some(aa) = self.defs.get(name) else {
            return;
        };

        position = self.eco_build_site(aa, position, 0);

        // don't build AA too close together.
        if self.eco.aas.iter().any(|a| distance(a.pos(), position) < 250.0) {
            return;
        }

        self.eco_add_simple(aa, position, List::Porc, Some(List::Aa));
    }

    fn eco_choose_super_weapon(&mut self) {
        if (self.eco.super_weps.len() < 4 && kgb_rand() > 0.4) || !self.eco.big_map {
            self.eco.s_wep = 0;
        } else {
            let rnd = kgb_rand();
            if !self.war.enemy_has_anti_nuke && rnd > 0.66 {
                self.eco.s_wep = 1;
            } else if rnd > 0.33 {
                self.eco.s_wep = 2;
            } else {
                self.eco.s_wep = 3;
            }
        }
        // Only what our energy can power (the grid planner links it up): a
        // nuke needs none, a Bertha little.
        let name = ["staticheavyarty", "staticnuke", "zenith", "raveparty"][self.eco.s_wep.clamp(0, 3) as usize];
        let needs = self.defs.get(name).map_or(0.0, |d| self.eco.grid_defs.needs(d.id()));
        let energy = self.eco_energy_production();
        if needs > energy {
            let fallback = if self.war.enemy_has_anti_nuke { 0 } else { 1 };
            self.log(&format!(
                "economy: {name} needs {needs:.0} energy and we make {energy:.0}; building {} instead",
                ["staticheavyarty", "staticnuke"][fallback as usize]
            ));
            self.eco.s_wep = fallback;
        }
        self.eco.chose_sup_wep = true;
    }

    /// Whether `position` is a poor superweapon site: enemy territory,
    /// front line, or too close to a factory.
    fn eco_bad_super_wep_site(&self, position: Vec3) -> bool {
        self.graph.is_enemy_territory(position)
            || self.graph.is_front_line(position)
            || self.eco_get_nearest_fac(position).is_some_and(|f| distance(position, f.get_pos()) < 300.0)
    }

    fn eco_create_bertha_task(&mut self) {
        if self.fac.factories.is_empty() || self.eco.radars.is_empty() {
            return; // Eh, no.
        }
        let Some(bertha) = self.defs.get("staticheavyarty") else {
            return;
        };
        let mut position;

        let mut good = false;
        let mut i = 0;
        loop {
            i += 1;
            if i > 4 {
                return;
            }
            let ally_base = self.graph.get_ally_base();
            let map_center = self.graph.get_map_center();
            position = get_angular_point(ally_base, map_center, distance(ally_base, map_center) * 0.65);
            position = self.eco.height_map.get_highest_point_in_radius(position, 800.0);
            position = self.eco_build_site(bertha, position, 0);
            let mut j = 0;
            while !good {
                j += 1;
                if j > 3 {
                    break;
                }
                good = !self.eco.super_weps.iter().any(|b| distance(b.pos(), position) < 350.0);

                if self.eco_bad_super_wep_site(position) {
                    good = false;
                }

                if !good {
                    position = get_radial_point(position, 300.0);
                    position = self.eco_build_site(bertha, position, 0);
                }
            }
            if good {
                break;
            }
        }

        let mut ct = WorkerTask::construction(bertha, position, 0);
        if let Some(c) = ct.as_construction_mut() {
            c.frame_issued = self.frame;
        }
        self.eco_add_construction(ct, Some(List::SuperWep), Some(List::SuperWep));
    }

    fn eco_create_super_wep_task(&mut self) {
        if self.fac.factories.is_empty() || self.eco.radars.is_empty() {
            return; // Eh, no.
        }
        let name = match self.eco.s_wep {
            1 => "staticnuke",
            2 => "zenith",
            _ => "raveparty",
        };
        let Some(death) = self.defs.get(name) else {
            return;
        };

        let ally_base = self.graph.get_ally_base();
        let ally_center = self.graph.get_ally_center();
        let mut position = get_directional_point(ally_base, ally_center, distance(ally_base, ally_center) * 0.3);
        // port: the original guarded this with try/catch; a degenerate
        // direction (base == center) yields NaN instead.
        if !position.x.is_finite() || !position.z.is_finite() {
            return;
        }
        position = self.eco_build_site(death, position, 0);

        let mut i = 0;
        loop {
            i += 1;
            if i > 3 {
                return;
            }
            if !self.eco_bad_super_wep_site(position) {
                break;
            }
            position = get_radial_point(position, 400.0);
            position = self.eco_build_site(death, position, 0);
        }

        let mut ct = WorkerTask::construction(death, position, 0);
        if let Some(c) = ct.as_construction_mut() {
            c.frame_issued = self.frame;
        }
        self.eco_add_construction(ct, Some(List::SuperWep), Some(List::SuperWep));
    }

    /// Rule out first factories whose units get to much less of the metal
    /// (from `from`, by the terrain model's tiers; aircraft get everywhere)
    /// than the best one's do.
    fn eco_weigh_factories_by_reach(&mut self, from: Vec3) {
        let Some(terrain) = self.reach_terrain() else { return };
        let reaches = |tier: Tier, p: Vec3| {
            terrain.tier(tier).is_none_or(|m| m.connected(terrain_pos(from), terrain_pos(p), FACTORY_REACH))
        };
        let spots = &self.graph.metal_spots;
        let share = |tier: Tier| {
            let (total, reached) = spots
                .iter()
                .fold((0.0, 0.0), |(t, r), s| (t + s.value, if reaches(tier, s.position) { r + s.value } else { r }));
            if total > 0.0 { reached / total } else { 1.0 }
        };
        let candidates: Vec<(String, f32)> =
            self.eco.potential_fac_list.iter().filter_map(|f| Some((f.clone(), share(factory_tier(f)?)))).collect();
        let best = candidates.iter().map(|c| c.1).fold(0.0, f32::max);
        let out: Vec<String> = candidates.iter().filter(|c| c.1 < REACH_RIVAL * best).map(|c| c.0.clone()).collect();
        if !out.is_empty() {
            let shares: Vec<String> = candidates.iter().map(|(f, s)| format!("{f} {:.0}%", s * 100.0)).collect();
            self.log(&format!("economy: metal within reach: {}; ruling out {}", shares.join(", "), out.join(", ")));
            for f in out {
                self.eco.potential_fac_list.remove(&f);
            }
        }
    }

    /// The factory whose units get to most of the metal we do not hold and
    /// our factories' units cannot, when that is a good share of it: the
    /// factory, the share out of reach and the share it gets to. A gunship
    /// factory (its constructors fly anywhere) only when no ground or sea
    /// factory gets to most of it.
    fn eco_reach_gap(&self) -> Option<(&'static str, f32, f32)> {
        let terrain = self.reach_terrain()?;
        let from = self.graph.get_ally_center();
        if from == NULLPOS {
            return None;
        }
        let reaches = |tier: Tier, p: Vec3| {
            terrain.tier(tier).is_none_or(|m| m.connected(terrain_pos(from), terrain_pos(p), FACTORY_REACH))
        };
        let factories: Vec<&str> = self.fac.factories.values().map(|f| self.defs.name_of(f.unit)).collect();
        let ours: Vec<Tier> = factories.iter().filter_map(|f| factory_tier(f)).collect();
        // Aircraft get everywhere already.
        if ours.is_empty() || factories.iter().any(|f| factory_tier(f).is_none() && f.contains("factory")) {
            return None;
        }
        let spots = &self.graph.metal_spots;
        let total: f32 = spots.iter().filter(|s| !s.owned).map(|s| s.value).sum();
        let gap: Vec<&crate::graph::MetalSpot> =
            spots.iter().filter(|s| !s.owned && !ours.iter().any(|&t| reaches(t, s.position))).collect();
        let gap_value: f32 = gap.iter().map(|s| s.value).sum();
        if total <= 0.0 || gap_value < REACH_GAP * total {
            return None;
        }
        let fill = |f: &str| {
            factory_tier(f).map_or(0.0, |t| gap.iter().filter(|s| reaches(t, s.position)).map(|s| s.value).sum::<f32>())
        };
        let best = GROUND_FACTORIES
            .iter()
            .filter(|f| !factories.contains(f) && self.defs.get(f).is_some())
            .map(|&f| (f, fill(f)))
            .max_by(|a, b| a.1.total_cmp(&b.1))
            .filter(|&(_, v)| v >= AIR_FALLBACK * gap_value)
            .unwrap_or(("factorygunship", gap_value));
        Some((best.0, gap_value / total, best.1 / total))
    }

    /// The factory the modules would have built next, and why: one whose
    /// units get to metal ours cannot, or a gunship factory to drop our
    /// assaults past the chokepoints guarding much of the enemy's value.
    fn eco_find_requisition(&self) -> Option<(String, String)> {
        if let Some((f, gap, fills)) = self.eco_reach_gap() {
            return Some((
                f.to_owned(),
                format!(
                    "{:.0}% of the metal we do not hold is out of our units' reach; it gets to {:.0}%",
                    100.0 * gap,
                    100.0 * fills
                ),
            ));
        }
        if let Some((f, gap, seen, closes)) = self.arsenal_gap() {
            return Some((
                f.to_owned(),
                format!(
                    "{gap:.0} of {seen:.0} metal of the enemy seen lately is out of our units' reach or under water; \
                     its units get to {closes:.0}"
                ),
            ));
        }
        let owned = |name: &str| self.fac.factories.values().any(|f| self.defs.name_of(f.unit) == name);
        let s = self.strategy.as_ref()?;
        if !self.dev_off("drops")
            && !owned("factorygunship")
            && self.eco.static_income > DROP_INCOME
            && self.drops_cargo_count(self.graph.get_ally_center()) >= 4
        {
            // Value the ground forces cannot get to, or only the long way
            // round through guarded chokepoints.
            let total: f32 = s.analysis.values.iter().map(|v| v.enemy).sum();
            let guarded: f32 = s
                .analysis
                .drops
                .iter()
                .filter(|d| d.ground_distance.is_none() || (d.guards > 0 && d.detour >= DROP_DETOUR))
                .map(|d| d.enemy_value)
                .sum();
            if total > 0.0 && guarded >= DROP_SHARE * total {
                return Some((
                    "factorygunship".to_owned(),
                    format!(
                        "{:.0}% of the enemy's value is out of ground reach or far round guarded chokepoints: air drops",
                        100.0 * guarded / total
                    ),
                ));
            }
        }
        None
    }

    /// An Urchin where enemies were seen under water lately, by the nearest
    /// of our metal spots, if none covers it yet. There is no sonar
    /// building to build: sonar comes with ships and with the Urchin
    /// (torpedo turret), which also hits what it sees. It stands on water
    /// (its yardmap is all water): the first water on the way out from our
    /// metal to the sighting.
    fn eco_plan_sub_defense(&mut self) {
        let Some(sighting) = self.arsenal_submerged_sighting() else { return };
        let Some(urchin) = self.defs.get("turrettorp") else { return };
        let range = urchin.sonar_radius() as f32;
        let near = |p: Vec3| distance(p, sighting) < 2.0 * range;
        let ours = self.cb.team_units().into_iter().filter(|u| u.def().is_some_and(|d| d.id() == urchin.id()));
        let covered = ours.into_iter().any(|u| near(u.pos()))
            || self.eco.porc_tasks.iter().any(|&t| {
                let task = self.eco.tasks.get(t).and_then(|t| t.as_construction());
                task.is_some_and(|c| c.build_type.id() == urchin.id()) && near(self.eco.task_pos(t))
            });
        let Some(spot) = (self.graph.get_owned_spots().iter().map(|m| m.get_pos()))
            .min_by(|a, b| distance(*a, sighting).total_cmp(&distance(*b, sighting)))
        else {
            return;
        };
        if covered || distance(spot, sighting) > 3.0 * range {
            return;
        }
        let map = self.cb.map();
        let steps = (distance(spot, sighting) / 64.0).ceil().max(1.0) as usize;
        let water = (0..=steps)
            .map(|i| i as f32 / steps as f32)
            .map(|t| Vec3::xz(spot.x + t * (sighting.x - spot.x), spot.z + t * (sighting.z - spot.z)))
            .find(|p| map.elevation_at(p.x, p.z) < -URCHIN_DEPTH);
        let Some(water) = water else { return };
        let site = self.eco_build_site(urchin, water, 0);
        if site.x < 0.0 {
            return;
        }
        if self.eco_add_simple(urchin, site, List::Porc, Some(List::Porc)).is_some() {
            self.log(&format!("economy: urchin at ({:.0}, {:.0}) for enemies under water", site.x, site.z));
        }
    }

    /// A missile silo where it reaches enough worth Eos (see `missiles`),
    /// once we can afford one; its job summons the constructors near it.
    fn eco_plan_missile_silo(&mut self) {
        let Some(silo) = self.defs.get(missiles::SILO) else { return };
        if self.dev_off("missiles")
            || self.missiles_silo().is_some()
            || self.eco.adjusted_income < missiles::SILO_INCOME
            || self.eco.estalled
        {
            return;
        }
        let planned = self.eco.construction_tasks.iter().any(|&id| {
            self.eco.tasks.get(id).and_then(|t| t.as_construction()).is_some_and(|c| c.build_type.id() == silo.id())
        });
        if planned {
            return;
        }
        let Some((site, value, n)) = self.missiles_site() else {
            self.missiles_report_no_site();
            return;
        };
        let pos = self.eco_build_site(silo, site, 0);
        if pos.x < 0.0 {
            return;
        }
        if self.eco_add_simple(silo, pos, List::Construction, None).is_some() {
            self.log(&format!(
                "missiles: silo at ({:.0}, {:.0}), {value:.0} metal of targets ({n}) in reach; summoning constructors",
                pos.x, pos.z
            ));
        }
    }

    /// Re-assess what factory the modules would have, now and then.
    pub(super) fn eco_update_requisition(&mut self, frame: i32) {
        if (frame + self.offset) % REQUISITION_PERIOD != 0
            || self.fac.factories.is_empty()
            || self.dev_off("requisition")
        {
            return;
        }
        self.eco_plan_sub_defense();
        self.eco_plan_missile_silo();
        let r = self.eco_find_requisition();
        if let Some((f, why)) = &r
            && self.eco.requisition_logged.as_ref() != Some(f)
        {
            self.log(&format!("economy: requisitioning a {f}: {why}"));
            self.eco.requisition_logged = Some(f.clone());
        }
        self.eco.requisition = r;
    }

    /// Try up to ~100 random spots around `position` for one `def`.
    fn eco_ring_porc(&mut self, def: Option<UnitDef>, position: Vec3, radius: f32, dedup: List, extra: Option<List>) {
        let Some(def) = def else {
            return;
        };
        let mut i = 0;
        loop {
            if i > 100 {
                break;
            }
            i += 1;
            let pos = get_radial_point(position, radius);
            let pos = self.eco_build_site(def, pos, 0);
            if self.eco_add_simple(def, pos, dedup, extra).is_some() {
                break;
            }
        }
    }

    pub(super) fn eco_defend_singu(&mut self, position: Vec3) {
        let llt = self.defs.get("turretlaser");
        let defender = self.defs.get("turretmissile");
        let hlt = self.defs.get("turretheavylaser");
        let cobra = self.defs.get("turretaaflak");
        let shield = self.defs.get("staticshield");

        // build an llt
        self.eco_ring_porc(llt, position, 150.0, List::Porc, Some(List::Porc));
        // build a defender
        self.eco_ring_porc(defender, position, 150.0, List::Porc, Some(List::Porc));
        // build an hlt
        self.eco_ring_porc(hlt, position, 150.0, List::Porc, Some(List::Porc));
        // build a cobra
        self.eco_ring_porc(cobra, position, 200.0, List::Porc, Some(List::Aa));
        // build an area shield
        self.eco_ring_porc(shield, position, 75.0, List::Construction, None);
    }

    /// An LLT, or on water maps sometimes a torpedo turret.
    fn eco_light_turret(&self) -> Option<UnitDef> {
        let mut turret = self.defs.get("turretlaser");
        if self.eco.ships_viable && kgb_rand() > 0.5 {
            turret = self.defs.get("turrettorp");
        }
        turret
    }

    pub(super) fn eco_defend_mex(&mut self, position: Vec3) {
        let turret = self.eco_light_turret();
        // Where enemy ground forces cannot get, about half as many.
        let spacing = if self.strategy_sheltered(position) { SHELTERED_SPACING } else { 1.0 };

        if self.eco.porcs.iter().any(|u| distance(position, u.pos()) < 350.0 * spacing) {
            return;
        }

        if self.eco.porc_tasks.iter().any(|&c| distance(position, self.eco.task_pos(c)) < 350.0 * spacing) {
            return;
        }

        let Some(turret) = turret else {
            return;
        };
        let ally_base = self.graph.get_ally_base();
        let pos = get_directional_point(ally_base, position, distance(ally_base, position) + 100.0);
        let pos = self.eco_build_site(turret, pos, 0);

        self.eco_add_simple(turret, pos, List::Porc, Some(List::Porc));
    }

    pub(super) fn eco_fortify_mex(&mut self, position: Vec3, wd: Option<WeaponDef>) {
        let porc;

        if self.eco.adjusted_income > 30.0 {
            porc = match wd {
                None => self.defs.get("turretemp"),
                // TODO: move these ranges to config.
                Some(wd) if wd.range() > 620.0 => self.defs.get("turretgauss"),
                Some(wd) if wd.range() > 410.0 => self.defs.get("turretheavylaser"),
                Some(_) if self.eco.ships_viable => self.defs.get("turrettorp"),
                Some(_) => self.defs.get("turretriot"),
            };
        } else {
            porc = if self.eco.ships_viable && kgb_rand() > 0.5 {
                self.defs.get("turrettorp")
            } else {
                self.defs.get("turretlaser")
            };

            if self.eco.adjusted_income > 20.0 {
                let pos = get_angular_point(position, self.graph.get_enemy_center(), 150.0);
                if let Some(porc) = porc {
                    let pos = self.eco_build_site(porc, pos, 0);
                    self.eco_add_simple(porc, pos, List::Porc, Some(List::Porc));
                }
            }
        }

        let pos = get_angular_point(position, self.graph.get_enemy_center(), 150.0);
        if let Some(porc) = porc {
            let pos = self.eco_build_site(porc, pos, 0);
            self.eco_add_simple(porc, pos, List::Porc, Some(List::Porc));
        }
    }

    pub(super) fn eco_defend_nano(&mut self, n: Nanotower) {
        if n.target.health() <= 0.0 || n.target.team() != self.team_id {
            return;
        }
        let Some(turret) = self.eco_light_turret() else {
            return;
        };

        let (tpos, npos) = (n.target.pos(), n.unit.pos());
        let pos = get_directional_point(tpos, npos, distance(tpos, npos) + 100.0);
        let pos = self.eco_build_site(turret, pos, 0);

        self.eco_add_simple(turret, pos, List::Porc, Some(List::Porc));
    }

    pub(super) fn eco_defend_fusion(&mut self, position: Vec3, factory: bool) {
        let hlt = self.defs.get("turretheavylaser");
        let emp = self.defs.get("turretemp");
        // build an hlt and a faraday
        let mut pos = get_radial_point(position, if factory { 175.0 } else { 125.0 });
        if let Some(hlt) = hlt {
            pos = self.eco_build_site(hlt, pos, 0);
            self.eco_add_simple(hlt, pos, List::Porc, Some(List::Porc));
        }

        pos = get_directional_point(position, pos, distance(position, pos) + 75.0);
        if let Some(emp) = emp {
            pos = self.eco_build_site(emp, pos, 0);
            self.eco_add_simple(emp, pos, List::Porc, Some(List::Porc));
        }
    }

    pub(super) fn eco_defend_fac(&mut self, position: Vec3) {
        let Some(turret) = self.eco_light_turret() else {
            return;
        };

        if self.merged_allies == 0 {
            let mut i = 0;
            loop {
                if i > 10 {
                    break;
                }
                i += 1;
                let pos = get_angular_point(position, self.graph.get_map_center(), 250.0);
                let pos = self.eco_build_site(turret, pos, 0);
                let mut ct = WorkerTask::construction(turret, pos, 0);
                if let Some(c) = ct.as_construction_mut() {
                    c.fac_def = true;
                }
                if self.eco_add_construction(ct, Some(List::Porc), Some(List::Porc)).is_some() {
                    break;
                }
            }
        } else {
            let ally_base = self.graph.get_ally_base();
            let mut pos = get_directional_point(ally_base, position, distance(position, ally_base) + 175.0);
            pos = get_directional_point(pos, self.graph.get_map_center(), 75.0);
            pos = self.eco_build_site(turret, pos, 0);
            self.eco_add_simple(turret, pos, List::Porc, Some(List::Porc));
        }
    }

    /// Turrets at our metal spots (sparser where enemy ground forces cannot
    /// get; that metal goes into the economy instead).
    pub(super) fn eco_defend_mexes(&mut self) {
        let spots: Vec<Vec3> = self.graph.get_owned_spots().iter().map(|ms| ms.get_pos()).collect();
        for p in spots {
            self.eco_defend_mex(p);
        }
    }

    pub(super) fn eco_defend_links(&mut self) {
        // Only links ground forces travel (not across water or cliffs).
        let graph = &self.graph;
        let all = self.dev_off("porc");
        let links = graph
            .get_links()
            .iter()
            .enumerate()
            .filter(|&(i, l)| (all || graph.is_ground_link(i)) && graph.is_link_owned(l));
        let positions: Vec<Vec3> = if self.merged_allies == 0 {
            links.filter(|(_, l)| l.length <= 550.0).map(|(_, l)| l.get_pos()).collect()
        } else {
            links
                .filter(|(_, l)| l.length < 800.0 && graph.is_front_line(l.get_pos()))
                .map(|(_, l)| l.get_pos())
                .collect()
        };
        for p in positions {
            self.eco_defend_link(p);
        }
    }

    fn eco_defend_link(&mut self, position: Vec3) {
        let Some(llt) = self.defs.get("turretlaser") else {
            return;
        };

        // don't spam redundant porc (where enemy ground forces cannot get,
        // even less)
        let spacing = if self.strategy_sheltered(position) { SHELTERED_SPACING } else { 1.0 };
        let porcdist = 300.0 * spacing;
        if self.eco.porcs.iter().any(|u| distance(position, u.pos()) < porcdist) {
            return;
        }

        if self.eco.porc_tasks.iter().any(|&c| distance(position, self.eco.task_pos(c)) < porcdist) {
            return;
        }

        // build an llt
        let pos = self.eco_build_site(llt, position, 0);
        self.eco_add_simple(llt, pos, List::Porc, Some(List::Porc));
    }

    /// Fortify the chokepoints guarding our territory (see
    /// `Strategy::fortify`): at most one new turret per call, more per
    /// chokepoint as income grows.
    fn eco_defend_chokes(&mut self) {
        let income = self.eco.adjusted_income;
        if income < 10.0 {
            return;
        }
        let wanted = 1 + usize::from(income > 25.0) + usize::from(income > 45.0);
        let Some(strategy) = &self.strategy else {
            return;
        };
        let sites: Vec<(usize, Vec3, f32)> =
            strategy.fortify.iter().enumerate().filter(|(_, f)| !f.blocked).map(|(i, f)| (i, f.pos, f.width)).collect();
        for (i, pos, width) in sites {
            let have = self.eco_porc_count(pos, CHOKE_PORC_RADIUS);
            if have >= wanted {
                continue;
            }
            let Some(turret) = self.eco_choke_turret(width, have) else {
                continue;
            };
            let site = self.eco_build_site(turret, pos, 0);
            // a site that far away could be on the wrong side of the gate
            if distance(site, pos) > CHOKE_PORC_RADIUS * 0.6 {
                if let Some(f) = self.strategy.as_mut().and_then(|s| s.fortify.get_mut(i)) {
                    f.blocked = true;
                }
                continue;
            }
            if self.eco_add_simple(turret, site, List::Porc, Some(List::Porc)).is_some() {
                let choke = self.strategy.as_ref().and_then(|s| s.fortify.get(i)).map_or(0, |f| f.choke);
                self.log(&format!("terrain: fortifying chokepoint C{choke} with {}", turret.name()));
                return;
            }
        }
    }

    /// The `nth` turret for a chokepoint `width` elmos wide: a riot turret
    /// for narrow gates or an LLT first, then an HLT, then heavier porc.
    fn eco_choke_turret(&self, width: f32, nth: usize) -> Option<UnitDef> {
        let income = self.eco.adjusted_income;
        self.defs.get(match nth {
            0 if width < 250.0 && income > 20.0 => "turretriot",
            0 => "turretlaser",
            1 => "turretheavylaser",
            _ if income > 60.0 => "turretgauss",
            _ if width < 250.0 => "turretemp",
            _ => "turretlaser",
        })
    }

    /// Porc (built or queued) within `radius` of `pos`.
    fn eco_porc_count(&self, pos: Vec3, radius: f32) -> usize {
        self.eco.porcs.iter().filter(|u| distance(pos, u.pos()) < radius).count()
            + self.eco.porc_tasks.iter().filter(|&&c| distance(pos, self.eco.task_pos(c)) < radius).count()
    }

    /// Whether existing porc (built or queued) is within `porcdist` of `pos`.
    fn eco_porc_near(&self, pos: Vec3, porcdist: f32) -> bool {
        self.eco.porcs.iter().any(|u| distance(pos, u.pos()) < porcdist)
            || self.eco.porc_tasks.iter().any(|&c| distance(pos, self.eco.task_pos(c)) < porcdist)
    }

    fn eco_porc_push(&mut self, position: Vec3) {
        let tf2 = (self.graph.territory_fraction * 2.0).min(1.0);
        let mut rnd = kgb_rand();
        let mut porc = if rnd > 0.15 * tf2 as f64 {
            self.defs.get("turretmissile")
        } else {
            rnd = kgb_rand();
            if rnd > 0.35 { self.defs.get("turretheavylaser") } else { self.defs.get("turretemp") }
        };

        if self.eco.enemy_has_air && kgb_rand() > 0.95 {
            porc = self.defs.get("turretaalaser");
        }
        let Some(enemy) = self.war.get_closest_enemy_porc(position).and_then(|id| self.war.targets.get(&id)) else {
            return;
        };
        // port: an unidentified porc (no def) is skipped (the C++ dereferenced it).
        let (Some(ud), Some(porc)) = (enemy.ud, porc) else {
            return;
        };

        let pos = get_angular_point(enemy.position, position, ud.max_weapon_range() + 100.0);
        let pos = self.eco_build_site(porc, pos, 0);

        let porcdist = 150.0 + (150.0 * (1.0 - tf2));
        if self.eco_porc_near(pos, porcdist) {
            return;
        }

        let mut ct = WorkerTask::construction(porc, pos, 0);
        if let Some(c) = ct.as_construction_mut() {
            c.frame_issued = self.frame;
        }
        self.eco_add_construction(ct, Some(List::Porc), Some(List::Porc));
    }

    fn eco_defend_front(&mut self, position: Vec3) {
        let tf2 = (self.graph.territory_fraction * 2.0).min(1.0);
        let mut rnd = kgb_rand();
        let porc = if rnd > 0.6 {
            self.defs.get("turretmissile")
        } else if rnd > 0.25 * tf2 as f64 {
            self.defs.get("turretlaser")
        } else {
            rnd = kgb_rand();
            self.defs.get(if rnd > 0.75 {
                "turretheavylaser"
            } else if rnd > 0.5 {
                "turretgauss"
            } else if rnd > 0.25 {
                "turretriot"
            } else {
                "turretemp"
            })
        };

        let Some(ms_pos) = self.graph.get_closest_front_line_spot(position).map(|ms| ms.get_pos()) else {
            return;
        };

        let fpos = self.graph.get_closest_battle_front(ms_pos);

        let radius = 150.0 + (kgb_rand() as f32 * 350.0);
        let pos = get_angular_point(ms_pos, fpos, radius);
        let Some(porc) = porc else {
            return;
        };
        let pos = self.eco_build_site(porc, pos, 0);

        let porcdist = 200.0 + (150.0 * (1.0 - tf2));
        if self.eco_porc_near(pos, porcdist) {
            return;
        }

        let mut ct = WorkerTask::construction(porc, pos, 0);
        if let Some(c) = ct.as_construction_mut() {
            c.frame_issued = self.frame;
        }
        self.eco_add_construction(ct, Some(List::Porc), Some(List::Porc));
    }

    fn eco_create_radar_task(&mut self, wid: i32) {
        let Some(radar) = self.defs.get("staticradar") else {
            return;
        };
        let Some(wpos) = self.eco.workers.get(&wid).map(|w| w.get_pos()) else {
            return;
        };
        let mut position = self.eco.height_map.get_highest_point_in_radius(wpos, 800.0);
        position = self.eco_build_site(radar, position, 0);
        if !self.eco_need_radar(position) || !self.eco_can_reach(wid, position) {
            position = self.eco.height_map.get_highest_point_in_radius(wpos, 500.0);
            position = self.eco_build_site(radar, position, 0);
            if !self.eco_need_radar(position) || !self.eco_can_reach(wid, position) {
                return;
            }
        }

        self.eco_add_simple(radar, position, List::Radar, Some(List::Radar));
    }

    fn eco_need_radar(&self, position: Vec3) -> bool {
        !self.eco.radars.iter().any(|r| distance(r.pos(), position) <= 1250.0)
            && !self.eco.radar_tasks.iter().any(|&r| distance(self.eco.task_pos(r), position) <= 1250.0)
    }

    pub(super) fn eco_capture_mexes(&mut self) {
        let Some(mex) = self.defs.get("staticmex") else {
            return;
        };
        let spots: Vec<Vec3> = self.graph.get_neutral_spots().iter().map(|ms| ms.get_pos()).collect();
        let map = self.cb.map();

        for position in spots {
            if map.is_possible_to_build_at(mex, position, 0) && self.war.get_porc_threat(position) == 0.0 {
                let ct = WorkerTask::construction(mex, position, 0);
                if !self.eco.tasks.contains(&self.eco.construction_tasks, &ct) {
                    let id = self.eco.tasks.insert(ct);
                    self.eco.construction_tasks.push(id);
                }
            }
        }
    }

    /// Commander upgrades; disabled in the original (`update` has the call
    /// commented out).
    // TODO: configurable? or just find something "smart"?
    #[allow(dead_code)]
    fn eco_morph_coms(&mut self) {
        for &wid in &self.eco.commanders {
            let Some(u) = self.eco.workers.get(&wid).map(|w| w.unit) else {
                continue;
            };
            let level = u.rules_param_float("comm_level", 0.0);
            let chassis = u.rules_param_float("comm_chassis", 0.0);
            let old_mod_count = u.rules_param_float("comm_module_count", 0.0);
            let new_mod_count = 1.0f32; // TODO: moar
            let mut params = vec![level, chassis, old_mod_count, new_mod_count];

            let mut i = 1;
            while i as f32 <= old_mod_count {
                // port: the C++ built this key with pointer arithmetic
                // (`"comm_module_" + i`); the Java concatenates.
                let mod_id = u.rules_param_float(&format!("comm_module_{i}"), -1.0);
                if mod_id != -1.0 {
                    params.push(mod_id);
                }
                i += 1;
            }

            params.push(13.0); // shotgun

            u.execute_custom_command(cmd::MORPH_UPGRADE_INTERNAL, &params, 0, NO_TIMEOUT);
            u.execute_custom_command(cmd::MISC_PRIORITY, &[3.0], 0, NO_TIMEOUT);
        }
        self.eco.morphed_coms = true;
    }

    pub(super) fn eco_morph_sparrow(&mut self) {
        if self.fac.factories.is_empty() || self.eco.radars.is_empty() {
            return;
        }
        // find the most useless radar and turn it into a sparrow
        let mut best: Option<Unit> = None;
        let mut bestdist = 65535.0;
        for &r in &self.eco.radars {
            if r.health() <= 0.0 || r.team() != self.team_id {
                continue;
            }
            let rpos = r.pos();
            let Some(f) = self.eco_get_nearest_fac(rpos) else {
                continue;
            };
            let tmpdist = distance(f.get_pos(), rpos);
            if tmpdist < bestdist {
                bestdist = tmpdist;
                best = Some(r);
            }
        }
        let (Some(best), Some(scout)) = (best, self.defs.get("planelightscout")) else {
            return;
        };
        self.eco.sparrow = Some(best);
        erase_one(&mut self.eco.radars, &best);
        best.execute_custom_command(cmd::MORPH, &[scout.id() as f32], 0, NO_TIMEOUT);
        best.execute_custom_command(cmd::MISC_PRIORITY, &[3.0], 0, NO_TIMEOUT); // low prio, cause sparrows are freaking expensive.
    }

    fn eco_create_energy_task(&mut self, worker_pos: Vec3) {
        let mut position = worker_pos;
        if position == NULLPOS {
            return;
        }

        // prevent ecomanager from spamming solars/windgens that graphmanager doesn't know about yet
        let solar_dist = 400.0;
        let spammed = |ai: &Ai, position: Vec3| {
            ai.eco.solar_tasks.iter().chain(&ai.eco.wind_tasks).any(|&s| {
                ai.eco.tasks.get(s).is_some_and(|t| {
                    distance(t.get_pos(), position) < solar_dist
                        && t.as_construction().is_some_and(|c| c.target.is_none())
                })
            })
        };

        // for solars
        if (((self.eco.solars.len() + self.eco.solar_tasks.len()) as f32) < (self.eco.raw_mex_income / 2.0).round()
            || kgb_rand() > 0.25)
            && self.cb.map().elevation_at(position.x, position.z) > 0.0
        {
            let Some(solar) = self.defs.get("energysolar") else {
                return;
            };
            position = self.graph.get_overdrive_sweet_spot(position, "energysolar");
            // in case all metal spots are connected
            if position == NULLPOS {
                return;
            }
            position = self.eco_build_site(solar, position, 0);

            if spammed(self, position) {
                return;
            }

            self.eco_add_simple(solar, position, List::Solar, Some(List::Solar));
        } else {
            // for windgens
            let Some(windgen) = self.defs.get("energywind") else {
                return;
            };
            position = self.graph.get_overdrive_sweet_spot(position, "energywind");
            // in case all metal spots are connected
            if position == NULLPOS {
                return;
            }
            position = self.eco_build_site(windgen, position, 0);

            if spammed(self, position) {
                return;
            }

            self.eco_add_simple(windgen, position, List::Wind, Some(List::Wind));
        }
    }

    fn eco_create_fusion_task(&mut self) {
        if self.fac.factories.is_empty() {
            return;
        }
        let eco = &self.eco;
        let started = eco
            .fusion_tasks
            .iter()
            .filter(|&&ft| eco.tasks.get(ft).and_then(|t| t.as_construction()).is_some_and(|c| c.target.is_some()))
            .count() as i32;
        let (nft, nfus) = (eco.fusion_tasks.len() as i32, eco.fusions.len() as i32);
        let eminent = self.graph.eminent_territory;

        let name = if eco.effective_income > 35.0
            && eminent
            && (eco.static_income / 27.5).floor() > nfus as f32
            && !self.fac.factories.is_empty()
            && nft + nfus - started < 3
            && nft < 1 + self.merged_allies
        {
            // for fusions
            "energyfusion"
        } else if nfus > 2
            && nfus < 5 + i32::from(self.merged_allies > 1)
            && eminent
            && !self.fac.factories.is_empty()
            && eco.fusion_tasks.is_empty()
        {
            // for singus
            "energysingu"
        } else {
            return;
        };
        let Some(def) = self.defs.get(name) else {
            return;
        };

        let ally_center = self.graph.get_ally_center();
        let ally_base = self.graph.get_ally_base();
        let mut position = get_angular_point(ally_center, ally_base, distance(ally_center, ally_base));
        position = self.graph.get_overdrive_sweet_spot(position, name);
        if position == NULLPOS {
            return;
        }
        position = self.eco_build_site(def, position, 0);
        self.eco_add_construction(WorkerTask::construction(def, position, 0), None, Some(List::Fusion));
    }

    fn eco_create_grid_task(&mut self, position: Vec3) {
        let Some(pylon) = self.defs.get("energypylon") else {
            return;
        };

        let mut fdist = 65535.0;
        let mut best = position;
        for f in &self.eco.fusions {
            let fpos = f.pos();
            let tmpdist = distance(fpos, position);
            if tmpdist < fdist {
                fdist = tmpdist;
                best = fpos;
            }
        }

        best = self.graph.get_overdrive_sweet_spot(best, "energypylon");
        if best == NULLPOS {
            return;
        }
        best = self.eco_build_site(pylon, best, 0);
        // don't build pylons in the middle of nowhere
        if self.eco_get_fusion_dist(best) > 1500.0 {
            return;
        }

        // check the build site for existing pylons, since getOverdriveSweetSpot may cluster them.
        let gdist = self
            .eco
            .pylons
            .iter()
            .map(|u| distance(best, u.pos()))
            .chain(self.eco.pylon_tasks.iter().map(|&c| distance(best, self.eco.task_pos(c))))
            .fold(65535.0f32, f32::min);
        if gdist <= 750.0 {
            return; // Already have one
        }

        self.eco_add_simple(pylon, best, List::Pylon, Some(List::Pylon));
    }

    fn eco_create_nano_turret_task(&mut self, wid: i32, target: Unit) {
        let Some(nano) = self.defs.get("staticcon") else {
            return;
        };

        let def_name = self.defs.name_of(target);
        let (radius, mut build_dist) = if def_name == "striderhub" || def_name.contains("factory") {
            (400.0, 350.0)
        } else {
            (350.0, 300.0) // for superweapons
        };

        let mut i = 0;
        loop {
            let tpos = target.pos();
            let mut position = get_radial_point(tpos, build_dist);
            position = self.eco_build_site(nano, position, 0);

            if distance(position, tpos) < radius {
                let mut ct = WorkerTask::construction(nano, position, 0);
                if let Some(c) = ct.as_construction_mut() {
                    c.ct_target = Some(target);
                }
                if self.eco_build_check(nano, position, 0)
                    && !self.eco.tasks.contains(&self.eco.nano_tasks, &ct)
                    && self.eco_can_reach(wid, position)
                {
                    let id = self.eco.tasks.insert(ct);
                    self.eco.construction_tasks.push(id);
                    self.eco.nano_tasks.push(id);
                    return;
                }
            }
            i += 1;
            if i >= 8 {
                build_dist -= 50.0;
            }
            if i > 10 {
                return;
            }
        }
    }

    fn eco_create_storage_task(&mut self, wid: i32, target: Unit) {
        let Some(stor) = self.defs.get("staticstorage") else {
            return;
        };
        let build_dist = 400.0;

        let mut i = 0;
        let position = loop {
            let tpos = target.pos();
            let p = get_radial_point(tpos, build_dist);
            let p = self.eco_build_site(stor, p, 0);
            if distance(p, tpos) < 500.0 {
                break p;
            }
            if i > 1000 {
                return;
            }
            i += 1;
        };
        let ct = WorkerTask::construction(stor, position, 0);
        // (the dedup checks nanoTasks, as in the original)
        if self.eco_build_check(stor, position, 0)
            && !self.eco.tasks.contains(&self.eco.nano_tasks, &ct)
            && self.eco_can_reach(wid, position)
        {
            let id = self.eco.tasks.insert(ct);
            self.eco.construction_tasks.push(id);
            self.eco.storage_tasks.push(id);
        }
    }

    fn eco_create_air_pad_task(&mut self, target: Unit) {
        if self.defs.name_of(target) == "striderhub" {
            return;
        }
        let Some(airpad) = self.defs.get("staticrearm") else {
            return;
        };
        let build_dist = 400.0;

        let position = get_radial_point(target.pos(), build_dist);
        let position = self.eco_build_site(airpad, position, 0);

        self.eco_add_simple(airpad, position, List::Airpad, Some(List::Airpad));
    }

    /// An AA defense position between our center and the enemy, once we
    /// have several fusions (the "screamer" in `unitCreated`).
    pub(super) fn eco_create_screamer_task(&mut self) {
        let ally_center = self.graph.get_ally_center();
        let Some(front) = self.graph.get_closest_front_line_spot(ally_center).map(|s| s.get_pos()) else {
            return;
        };
        let Some(artemis) = self.defs.get("turretaaheavy") else {
            return;
        };
        let pos = get_directional_point(ally_center, self.graph.get_enemy_center(), 0.5 * distance(ally_center, front));
        let pos = self.eco_build_site(artemis, pos, 0);
        if self.eco_add_simple(artemis, pos, List::Construction, None).is_some() {
            self.eco.screamer = true;
        }
    }

    /// The factory (or factory under construction) with the fewest
    /// caretakers, with some per-type priority.
    fn eco_get_caretaker_target(&self) -> Option<Unit> {
        let eco = &self.eco;
        let count_cts = |id: i32| {
            let built = eco.nanos.values().filter(|n| n.target.health() > 0.0 && n.target.id() == id).count();
            let queued = eco
                .nano_tasks
                .iter()
                .filter_map(|&c| eco.tasks.get(c).and_then(|t| t.as_construction()).and_then(|c| c.ct_target))
                .filter(|t| t.health() > 0.0 && t.id() == id)
                .count();
            (built + queued) as f32
        };

        let mut target = None;
        let mut ct_count = 9001.0;
        for f in self.fac.factories.values() {
            // Spread caretakers evenly between facs, with some priority.
            let mut num_ct = count_cts(f.id);
            let merged = self.merged_allies > 0;
            match self.defs.name_of(f.unit) {
                "striderhub" => num_ct += if merged { -0.3 } else { 0.1 },
                "factoryplane" | "factorygunship" => num_ct += if merged { -0.2 } else { 0.1 },
                "factoryshield" => num_ct += if merged { -0.1 } else { 0.0 },
                _ => {}
            }
            if num_ct < ct_count {
                target = Some(f.unit);
                ct_count = num_ct;
            }
        }

        for c in eco.factory_tasks.iter().filter_map(|&c| eco.tasks.get(c).and_then(|t| t.as_construction())) {
            let Some(ctarget) = c.target.filter(|t| t.health() > 0.0) else {
                continue;
            };
            let mut num_ct = count_cts(ctarget.id());
            let def_name = self.defs.name_of(ctarget);
            if def_name == "striderhub" {
                num_ct -= 0.9;
            }
            if matches!(def_name, "factoryplane" | "factorygunship" | "factoryshield") {
                num_ct -= 0.5;
            }
            if num_ct < ct_count {
                target = Some(ctarget);
                ct_count = num_ct;
            }
        }
        target
    }

    fn eco_collect_reclaimables(&mut self, wid: i32) {
        let cap = self.fac.num_workers.max(20) as usize;
        if self.eco.reclaim_tasks.len() > cap {
            return;
        }
        let Some(wpos) = self.eco.workers.get(&wid).map(|w| w.get_pos()) else {
            return;
        };
        let mut feats = self.cb.features_in(wpos, self.map_diag * 2.5);

        // Sort by closest first.
        feats.sort_by(|a, b| distance(wpos, a.pos()).total_cmp(&distance(wpos, b.pos())));

        for f in feats {
            let Some(fd) = f.def() else {
                continue;
            };
            let fpos = f.pos();
            if !fd.is_reclaimable() || self.war.get_porc_threat(fpos) > 0.0 {
                continue;
            }
            let (metal, energy) = (fd.contained_resource(self.metal), fd.contained_resource(self.energy));
            let (list, value) = if metal > energy { (List::Reclaim, metal) } else { (List::EnergyReclaim, energy) };
            if self.eco.list(list).len() > cap {
                break;
            }
            let rt = WorkerTask::reclaim(f, value);
            if !self.eco.tasks.contains(self.eco.list(list), &rt) && self.eco_can_reach(wid, fpos) {
                let id = self.eco.tasks.insert(rt);
                self.eco.list_mut(list).push(id);
                break;
            }
        }
    }
}
