//! Task generation: factories, energy and grid, caretakers, storage, radar,
//! static defenses, superweapons, reclaim, morphs.

use std::collections::BTreeSet;

use springai::{NO_TIMEOUT, Unit, UnitDef, Vec3, WeaponDef};

use super::{List, Nanotower, TaskId, WorkerTask, erase_one};
use crate::Ai;
use crate::defs::cmd;
use crate::military::{missiles, nukes};
use crate::terrain::pos as terrain_pos;
use crate::util::{NULLPOS, distance, get_angular_point, get_directional_point, get_radial_point, kgb_rand};
use zk_terrain::Tier;

/// Turrets at metal spots and links stand this many times further apart
/// where enemy ground forces cannot get (about half as many).
const SHELTERED_SPACING: f32 = 2.0;

/// A factory's units reach a metal spot if they can get this close to it
/// (about a constructor's build range).
const FACTORY_REACH: f32 = 150.0;

/// Frames before a missile silo planned is planned again (if the job was
/// dropped, or the silo lost).
const SILO_REPLAN: i32 = 30 * 120;

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

/// Past this income a factory of a type we do not have yet is started
/// every five minutes or so (a minute either way), unless something
/// presses (out of energy, or attacked lately); one factory for every
/// `NEW_FAC_PER` of income at most.
pub(super) const NEW_FAC_INCOME: f32 = 50.0;
const NEW_FAC_EVERY: i32 = 30 * 300;
const NEW_FAC_JITTER: i32 = 30 * 60;
const NEW_FAC_PER: f32 = 25.0;

/// A gunship factory is asked for, from income `DROP_INCOME` on, when this
/// share of the enemy's value is out of ground reach or behind guarded
/// chokepoints the long way round (ground route this many times the
/// straight line).
const DROP_SHARE: f32 = 0.25;
const DROP_DETOUR: f32 = 1.5;
const DROP_INCOME: f32 = 30.0;

/// Or when this much of the enemy's buildings is only for drops to get to
/// (see `drops`).
const DROP_PROSPECT: f32 = 5000.0;

/// Air tempo: as a game runs on, and more so on larger maps, the speed of
/// aircraft converts more of the enemy's value (raids across the map,
/// strikes on whatever moves out). A dose (see [`air_tempo_dose`]) is
/// zero before `AIR_TEMPO_FROM_MIN` minutes, rises to one by
/// `AIR_TEMPO_FULL_MIN`, and scales with the map's diagonal (heightmap
/// squares) against `AIR_TEMPO_DIAG` (a 14x14 map), between
/// `AIR_TEMPO_DIAG_MIN` and `AIR_TEMPO_DIAG_MAX`. Two uses: the ordinary
/// factory switch (the random pick of the next factory, and the roll of
/// a new type) weighs the air factory it prefers 1 + `AIR_TEMPO_PICK` x
/// dose against 1 for every other candidate; and when nothing else asks
/// for a factory, an air factory is asked for once per game, if none
/// stands or is going up, worth `AIR_TEMPO_SHARE` of the enemy's standing
/// value times the dose, once that is worth the factory's cost. Planes
/// rather than gunships where static anti-air worth `AIR_TEMPO_AA_PLANES`
/// or an enemy air force worth `AIR_TEMPO_AIR_PLANES` was seen.
/// Off by default (unverified): `enable=airtempo` turns both on.
const AIR_TEMPO_SHARE: f32 = 0.15;
const AIR_TEMPO_PICK: f32 = 3.0;
const AIR_TEMPO_FROM_MIN: f32 = 10.0;
const AIR_TEMPO_FULL_MIN: f32 = 30.0;
const AIR_TEMPO_DIAG: f32 = 1270.0;
const AIR_TEMPO_DIAG_MIN: f32 = 0.5;
const AIR_TEMPO_DIAG_MAX: f32 = 1.5;
const AIR_TEMPO_AA_PLANES: f32 = 800.0;
const AIR_TEMPO_AIR_PLANES: f32 = 1000.0;
/// How an air tempo requisition's reason starts (the consumer knows it
/// by this).
pub(super) const AIR_TEMPO_WHY: &str = "air tempo";

/// Air fire: raids that cost us mexes and power where no home defense
/// team was near (see `firefighters::UNANSWERED_DIST`) are what aircraft
/// answer, and the firefighters only wait for an air factory. With none
/// owned or going up, once the unanswered losses of the last five
/// minutes weigh `firefighters::WANTED_AT`, an air factory is asked for,
/// ahead of the Widow scout, worth the metal of those losses (a mex's
/// income over the memory counted) continued over the payback. Once a
/// game. `disable=airfire`.
pub(super) const AIR_FIRE_WHY: &str = "air fire";

/// Whether a requisition's reason is one of the air ones (fire or tempo).
pub(super) fn is_air_requisition(why: &str) -> bool {
    why.starts_with(AIR_FIRE_WHY) || why.starts_with(AIR_TEMPO_WHY)
}

/// The air tempo dose `minutes` into the game on a map of diagonal
/// `map_diag` heightmap squares: zero before `AIR_TEMPO_FROM_MIN`, then
/// growing with time and with the map's size (see `AIR_TEMPO_SHARE`).
pub(super) fn air_tempo_dose(minutes: f32, map_diag: f32) -> f32 {
    let time = ((minutes - AIR_TEMPO_FROM_MIN) / (AIR_TEMPO_FULL_MIN - AIR_TEMPO_FROM_MIN)).clamp(0.0, 1.0);
    let size = (map_diag / AIR_TEMPO_DIAG).clamp(AIR_TEMPO_DIAG_MIN, AIR_TEMPO_DIAG_MAX);
    time * size
}

/// One of `list` for the roll `roll` in [0, 1), each name's odds in
/// proportion to `weight(name)` (uniform when all weigh the same).
fn weighted_pick(list: &BTreeSet<String>, weight: impl Fn(&str) -> f64, roll: f64) -> Option<String> {
    let total: f64 = list.iter().map(|f| weight(f).max(0.0)).sum();
    let mut at = roll.clamp(0.0, 1.0) * total;
    for f in list {
        at -= weight(f).max(0.0);
        if at < 0.0 {
            return Some(f.clone());
        }
    }
    list.iter().next_back().cloned()
}

/// Bertha sites are tried in this many headings across the half-circle
/// facing the map's middle, at these shares of the way there, on the
/// highest ground within this of each (elmos); an enemy metal spot counts
/// as this much metal, and shells are aimed this high above the ground.
const BERTHA_HEADINGS: usize = 9;
const BERTHA_SHARES: [f32; 4] = [0.4, 0.55, 0.7, 0.85];
const BERTHA_HIGH_GROUND: f32 = 500.0;
const BERTHA_SPOT_WORTH: f32 = 300.0;
const BERTHA_TARGET_HEIGHT: f32 = 20.0;

/// A missile silo's guard turrets stand this far towards the enemy, this
/// far apart, elmos; caretakers by it.
const SILO_GUARD: f32 = 220.0;
const SILO_GUARD_SPREAD: f32 = 110.0;
const SILO_CARETAKERS: usize = 2;

/// From this income on, a factory asked for is built (the usual second
/// factory waits for 50 and a few caretakers); and how often the asking is
/// re-assessed, in frames.
const REQUISITION_INCOME: f32 = 20.0;

/// A jump factory is asked for (for Firewalkers) from this much enemy light
/// skirmisher metal seen, when it is this share of the enemy's army.
const FIREWALKER_REQUISITION: f32 = 1800.0;
const FIREWALKER_SHARE: f32 = 0.3;
const REQUISITION_PERIOD: i32 = 30 * 20;

/// Ground and sea factories, by the tier of their units (not the jump
/// factory: its units go where the other bots go).
const GROUND_FACTORIES: [&str; 8] = [
    "factorycloak",
    "factoryshield",
    "factoryveh",
    "factorytank",
    "factoryhover",
    "factoryamph",
    "factoryspider",
    "factoryship",
];

/// The `k`-th factory of `list` for the `factory_pick` option, in name
/// order, the jump factory left out: it came later, and series mirrored
/// against older builds keep their matchups (ZKGBAI_FACTORY=factoryjump
/// opens with it).
pub(crate) fn picked_factory(list: &BTreeSet<String>, k: usize) -> String {
    let picks: Vec<&String> = list.iter().filter(|f| *f != "factoryjump").collect();
    if picks.is_empty() {
        return list.iter().next().cloned().unwrap_or_default();
    }
    picks[k % picks.len()].clone()
}

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

/// Metal income from which fusions may be built, and energy income from
/// which singularities may (also with two fusions standing); see
/// `eco_create_fusion_task`.
const FUSION_INCOME: f32 = 30.0;
const SINGU_ENERGY: f32 = 150.0;

/// A turret this near a mex covers it (point defense), elmos.
const POINT_COVER: f32 = 350.0;
/// Distance from the enemy at which a spot counts as plainly exposed: nearer
/// counts more (up to 4x), farther less.
const EXPOSURE_REF: f32 = 2000.0;
/// A spot on our side not yet taken, against an owned mex.
const FUTURE_WORTH: f32 = 0.25;
/// What a posture turret is worth to the project tracker, in multiples of
/// its cost.
const POSTURE_WORTH: f32 = 2.0;
/// Turret kinds a posture zone mixes, in order, with the income (metal/s)
/// each wants: Lotus, Picket, Stardust, Stinger, Faraday, Gauss, Desolator.
/// At most [`SAME_KIND`] of a kind per zone (user, 2026-10-26: no more
/// stacks of Lotuses).
const ZONE_MIX: [(&str, f32); 7] = [
    ("turretlaser", 0.0),
    ("turretmissile", 0.0),
    ("turretriot", 10.0),
    ("turretheavylaser", 15.0),
    ("turretemp", 25.0),
    ("turretgauss", 40.0),
    ("turretheavy", 80.0),
];
const SAME_KIND: usize = 2;
/// Posture turrets keep this far from every turret of ours (splash), elmos.
const TURRET_SPACING: f32 = 150.0;
/// A posture turret goes only where fewer than this many turrets of ours
/// reach within a Lotus's range: nowhere under more than two Lotus circles
/// (the posture keeps us from being naked, no more; Circuit overporcs, we
/// should not).
const MAX_OVERLAP: usize = 2;
const LOTUS_REACH: f32 = 460.0;
/// A turret must take at least this much spot-exposure off (a well covered
/// mex far from the enemy is not worth another).
const MIN_GAIN: f32 = 0.06;
/// Point defense stands this far from its mex towards the enemy, elmos,
/// and at most [`POINT_SHIFT`] from there.
const POINT_FORWARD: f32 = 110.0;
const POINT_SHIFT: f32 = 200.0;
/// Candidates tried per call, best first, before giving up for now.
const PLACE_TRIES: usize = 4;

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
    pub(super) fn eco_add_construction(
        &mut self,
        task: WorkerTask,
        dedup: Option<List>,
        extra: Option<List>,
    ) -> Option<TaskId> {
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

    /// A construction task the operator asked for: `def` at the legal site
    /// closest to `pos`. Returns the task and its site.
    pub(crate) fn eco_operator_build(
        &mut self,
        def: UnitDef,
        pos: Vec3,
        facing: i32,
    ) -> Result<(TaskId, Vec3), String> {
        let site = self.cb.map().find_closest_build_site(def, pos, 600.0, 3, facing);
        if site.x < 0.0 {
            return Err("no build site within 600 elmos".into());
        }
        let task = WorkerTask::construction(def, site, facing);
        if let Some(&id) =
            self.eco.construction_tasks.iter().find(|&&id| self.eco.tasks.get(id).is_some_and(|t| t.same_as(&task)))
        {
            return Ok((id, site));
        }
        let armed = !def.weapon_mounts().is_empty();
        let id = self
            .eco_add_construction(task, Some(List::Construction), armed.then_some(List::Porc))
            .ok_or("the economy refused the site (too close to a factory's yard or a planned building)")?;
        Ok((id, site))
    }

    /// A defense the project tracker asked for (see `logistics`): `def` at
    /// the legal site closest to `pos`, unless porc is planned within
    /// `near` of it, which is adopted instead. Returns the task and whether
    /// it was adopted.
    pub(crate) fn eco_logistics_porc(&mut self, def: UnitDef, pos: Vec3, near: f32) -> Option<(TaskId, bool)> {
        let site = self.eco_build_site(def, pos, 0);
        // What `eco_clean_orders` would drop at once: a blocked site, or one
        // enemy defenses cover; and where a worker would not go.
        if site.x < 0.0
            || !self.cb.map().is_possible_to_build_at(def, site, 0)
            || self.war.get_porc_threat(site) > 0.0
            || self.war.get_effective_threat(site) > self.war.get_total_friendly_threat(site)
        {
            return None;
        }
        let planned = (self.eco.porc_tasks.iter().copied())
            .filter(|&id| self.eco.tasks.get(id).is_some_and(|t| t.as_construction().is_some()))
            .min_by(|&a, &b| distance(self.eco.task_pos(a), site).total_cmp(&distance(self.eco.task_pos(b), site)))
            .filter(|&id| distance(self.eco.task_pos(id), site) < near);
        if let Some(id) = planned {
            return Some((id, true));
        }
        let mut ct = WorkerTask::construction(def, site, 0);
        if let Some(c) = ct.as_construction_mut() {
            c.frame_issued = self.frame;
        }
        self.eco_add_construction(ct, Some(List::Porc), Some(List::Porc)).map(|id| (id, false))
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
                // a factory a module asked for (that we have none of: it may be
                // stale, asked for before one went up another way)
                // (an air factory for tempo past the usual cap, with the room
                // a factory of a new type gets)
                || (eco.requisition.as_ref().is_some_and(|(f, _)| !self.eco_has_factory(f))
                    && eco.static_income > REQUISITION_INCOME
                    && nfac >= 1
                    && (nfac < 3 + merged
                        || (eco.requisition.as_ref().is_some_and(|(_, why)| is_air_requisition(why))
                            && (nfac as f32) < 1.0 + eco.static_income / NEW_FAC_PER))
                    && no_fac_tasks)
                || (eco.static_income > 50.0 && nfac == 1 && nnanos > 3 && no_fac_tasks)
                // a factory of a new type, now and then
                || (eco.new_fac_due && nfac >= 1 && no_fac_tasks)
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
                    && (!self.misc_handler.striders.is_empty() || !self.squad_handler.taskforces.is_empty())
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
            self.eco_create_energy_task(wid, position);
        }
        self.eco_create_fusion_task();

        // do we need caretakers? The AI's own rule: one per 10 static income,
        // factories and caretakers counted together. It is also the floor
        // the factory side needs: four caretakers at 54 income were the
        // difference between an army of 6030 and 1150 at 10:00 (Oct 23).
        // A clock rule from the second constructor on took the opening's
        // mexes, and sinks triggered by free spots or by the metal float
        // never fired in time; by 15:00 this rule plus the second factory
        // already spend what buildpower 1.6 brings in.
        let built =
            self.eco.nanos.len() + self.eco.nano_tasks.len() + self.fac.factories.len() + self.eco.factory_tasks.len();
        let want = (self.eco.static_income / 10.0).floor();
        if !self.fac.factories.is_empty()
            && (built as f32) < want
            && let Some(target) = self.eco_get_caretaker_target()
        {
            self.eco_create_nano_turret_task(wid, target);
        }

        //do we need storages?
        if frame > 600
            && !self.fac.factories.is_empty()
            && (!self.eco_energy_starved() || self.dev_off("storagegate"))
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

        // The posture's budget and sites (see `posture`): its own queue, not
        // the rest's (its orders would crowd out theirs, and theirs its).
        if self.posture_on() && self.eco.defended_fac {
            self.eco_defend_posture();
        }
        // porc push against nearby enemy porc and defend the front line
        if self.eco.defended_fac && (self.eco.porc_tasks.len() as f32) < self.fac.num_workers as f32 * 1.5 {
            // Without the posture, the terrain analysis's chokepoints.
            if !self.posture_on() {
                self.eco_defend_chokes();
            }
            self.eco_porc_push(position);
            if self.graph.eminent_territory {
                self.eco_defend_front(position);
            }
        }

        // do we need pylons? (in the overdrive phase, see
        // `eco_overdrive_phase`; it was two fusions). With `gridmex` the
        // grid planner links the mexes instead (see `grid`).
        if self.eco_overdrive_phase() && self.dev_off("gridmex") {
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

        // do we need superweapons? (not while energy is what we lack)
        if !self.eco.chose_sup_wep
            && !self.eco_energy_starved()
            && ((!self.allies.is_empty() && self.eco.has_striders && self.fac.num_striders > 1)
                || self.allies.is_empty())
            && self.eco.fusions.len() > 4
        {
            self.eco_choose_super_weapon();
        }

        // A nuke, when one strike would pay for it.
        if !self.eco.has_super_wep && self.eco.super_wep_tasks.is_empty() && self.eco.s_wep != 1 && self.nukes_wanted()
        {
            if let Some((p, v)) = self.nukes_build_target() {
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
                if let Some(id) = self.eco_add_simple(protector, pos, List::Construction, None) {
                    self.logistics_antinuke(id, self.war.enemy_has_nuke || nuked);
                }
            }
        }
    }

    /// The facing towards the map's middle, along the axis `pos` is farther
    /// off it.
    fn eco_factory_facing(&self, pos: Vec3) -> i32 {
        let map_width = self.cb.map().width() * 8;
        let map_height = self.cb.map().height() * 8;
        if (map_width as f32 - 2.0 * pos.x).abs() > (map_height as f32 - 2.0 * pos.z).abs() {
            if 2.0 * pos.x > map_width as f32 {
                3 // facing="west"
            } else {
                1 // facing="east"
            }
        } else if 2.0 * pos.z > map_height as f32 {
            2 // facing="north"
        } else {
            0 // facing="south"
        }
    }

    /// Where the first factory goes for a commander at `from`, and its
    /// facing: towards the map's middle, clear of units, features and metal
    /// spots. `None` when nothing fits. Also run from `init` for the pregame
    /// announcement, before any unit exists.
    pub(crate) fn eco_first_factory_site(&self, factory: UnitDef, from: Vec3) -> Option<(Vec3, i32)> {
        let facing = self.eco_factory_facing(from);
        let map_center = self.graph.get_map_center();
        let closest_spot = |p: Vec3| self.graph.get_closest_spot(p).map(|s| s.get_pos());
        let clearance = factory.x_size().max(factory.z_size()) as f32 * 4.0;
        for i in 1..=11 {
            let mut position = get_directional_point(from, map_center, clearance + 20.0);
            position = self.eco_build_site(factory, position, facing);

            // don't let factories get blocked by random crap, nor build them on top of mexes
            let mut j = 0;
            let mut closest = closest_spot(position);
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
                    get_angular_point(from, map_center, clearance + 20.0)
                } else {
                    get_radial_point(from, 125.0)
                };
                position = self.eco_build_site(factory, position, facing);
                closest = closest_spot(position);
            }
            // only for preventing stupid placement
            if self.eco_build_check(factory, position, facing) {
                return Some((position, facing));
            }
        }
        None
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
        let asked = if nfac > 0 { self.eco.requisition.take() } else { None };
        let asked_value = self.eco.requisition_value;
        let reach_fac = asked.as_ref().filter(|(f, _)| !self.eco_has_factory(f)).map(|(f, _)| f.clone());
        let new_type = if nfac > 0 && !is_plop && self.eco.new_fac_due { self.eco_new_factory_type() } else { None };

        if self.eco.potential_fac_list.is_empty() && reach_fac.is_none() && new_type.is_none() {
            self.eco.added_facs = false;
            return;
        }

        let mut fac_name = String::new();
        let factory: Option<UnitDef>;
        // The opening rolled at the start (with the odds below) is binding, as
        // allies were told of it, unless the reach check above ruled it out.
        let opening_due =
            nfac == 0 && self.eco.opening.as_ref().is_some_and(|f| self.eco.potential_fac_list.contains(f));

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
            fac_name = picked_factory(&self.eco.potential_fac_list, k);
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
        } else if let Some(new_type) = new_type {
            self.log(&format!("economy: a factory of a new type: {new_type}"));
            factory = self.defs.get(&new_type);
            self.eco.potential_fac_list.remove(&new_type);
            self.eco.new_fac_due = false;
            fac_name = new_type;
        } else if !opening_due && self.eco.potential_fac_list.contains("factoryship") && kgb_rand() < 0.5 {
            fac_name = "factoryship".to_owned();
            factory = self.defs.get(&fac_name);
            self.eco.potential_fac_list.remove(&fac_name);
        } else if !opening_due && is_plop && self.eco.potential_fac_list.contains("factorygunship") {
            fac_name = "factorygunship".to_owned();
            factory = self.defs.get(&fac_name);
            self.eco.potential_fac_list.remove(&fac_name);
        } else if (nfac as i32) < 1 + merged.max(1) || has_striders || is_plop || self.eco.pylons.is_empty() {
            // (The air factory for tempo weighs more as the game runs on.)
            let (gunship, plane) = self.eco_air_tempo_weights();
            let weight = |f: &str| match f {
                "factorygunship" => gunship,
                "factoryplane" => plane,
                _ => 1.0,
            };
            fac_name = weighted_pick(&self.eco.potential_fac_list, weight, kgb_rand()).unwrap_or_default();
            self.eco_air_tempo_rolled(&fac_name, weight(&fac_name));
            // The opening rolled at the start (the commander suits it).
            if let Some(opening) =
                self.eco.opening.take().filter(|f| nfac == 0 && self.eco.potential_fac_list.contains(f))
            {
                fac_name = opening;
            }
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
        let mut position: Vec3;
        let mut facing = self.eco_factory_facing(wpos);

        let closest_spot = |ai: &Ai, p: Vec3| ai.graph.get_closest_spot(p).map(|s| s.get_pos());
        let mut i = 0;
        if self.fac.factories.is_empty() || is_plop {
            // Where the pregame announced it to allies, if it still fits.
            let announced = (self.eco.opening_site.take())
                .filter(|&(def, pos, face)| def.id() == factory.id() && self.eco_build_check(factory, pos, face))
                .map(|(_, pos, face)| (pos, face));
            let Some(site) = announced.or_else(|| self.eco_first_factory_site(factory, wpos)) else {
                restore(self, &fac_name);
                return;
            };
            (position, facing) = site;
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
        // The free first factory always; another not while energy is what we lack.
        if self.eco_build_check(factory, position, facing)
            && !self.eco.tasks.contains(&self.eco.factory_tasks, &ct)
            && self.eco_can_reach(wid, position)
            && (is_plop || !self.eco_energy_starved())
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
            // A requisition is an issue worth what it answers.
            if let Some((f, why)) = &asked
                && *f == fac_name
            {
                self.logistics_factory(id, asked_value, why);
                if is_air_requisition(why) {
                    self.eco.air_asked = true;
                }
            }
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
        // New Fusions say what they make a moment after they are done.
        if needs > 0.0 && energy <= 0.0 {
            return;
        }
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
        if self.fac.factories.is_empty() || self.eco.radars.is_empty() || self.eco_awaiting_power() {
            return; // Eh, no.
        }
        let Some(bertha) = self.defs.get("staticheavyarty") else {
            return;
        };
        let Some(position) = self.eco_bertha_site(bertha) else { return };
        self.eco_add_powered(bertha, position, List::SuperWep);
    }

    /// Where a Bertha shells the most: of sites on high ground between our
    /// base and the map's middle (not in enemy land or at the front, clear
    /// of factories and other superweapons), the one whose plasma, on its
    /// low arc, clears the terrain to the most enemy value in range: the
    /// buildings we know of, and the enemy's metal spots at what a spot is
    /// worth. Shells that would hit a hill on the way do not count.
    fn eco_bertha_site(&mut self, bertha: UnitDef) -> Option<Vec3> {
        let wd = bertha.weapon_mounts().into_iter().find_map(|m| m.weapon_def())?;
        let range = wd.range();
        // Elmos per frame, as the engine keeps it.
        let speed = if wd.projectile_speed() < 100.0 { wd.projectile_speed() * 30.0 } else { wd.projectile_speed() };
        let gravity = self.cb.map().gravity();
        let muzzle = 0.8 * bertha.height();
        let mut targets: Vec<(Vec3, f32)> = (self.war.targets.values())
            .filter(|e| e.identified && e.is_static && !e.is_dead)
            .map(|e| (e.position, e.value))
            .collect();
        targets.extend(self.graph.get_enemy_territory().iter().map(|m| (m.position, BERTHA_SPOT_WORTH)));
        let ally_base = self.graph.get_ally_base();
        let map_center = self.graph.get_map_center();
        let span = distance(ally_base, map_center);
        let (dx, dz) = (map_center.x - ally_base.x, map_center.z - ally_base.z);
        let heading = dz.atan2(dx);
        let mut best: Option<(Vec3, f32, f32)> = None;
        for k in 0..BERTHA_HEADINGS {
            let a = heading + std::f32::consts::PI * (k as f32 / (BERTHA_HEADINGS - 1) as f32 - 0.5);
            for share in BERTHA_SHARES {
                let p = Vec3::xz(ally_base.x + span * share * a.cos(), ally_base.z + span * share * a.sin());
                let high = self.eco.height_map.get_highest_point_in_radius(p, BERTHA_HIGH_GROUND);
                let site = self.eco_build_site(bertha, high, 0);
                if site.x < 0.0
                    || self.eco_bad_super_wep_site(site)
                    || self.eco.super_weps.iter().any(|b| distance(b.pos(), site) < 350.0)
                {
                    continue;
                }
                let hm = &self.eco.height_map;
                let (mut clear, mut in_range) = (0.0, 0.0);
                for &(t, v) in targets.iter().filter(|t| distance(t.0, site) <= range) {
                    in_range += v;
                    if hm.shell_clears(site, muzzle, t, BERTHA_TARGET_HEIGHT, speed, gravity) {
                        clear += v;
                    }
                }
                if best.is_none_or(|b| clear > b.1) {
                    best = Some((site, clear, in_range));
                }
            }
        }
        let Some((site, clear, in_range)) = best else {
            if self.frame - self.eco.bertha_report >= 30 * 300 {
                self.eco.bertha_report = self.frame;
                self.log(
                    "economy: no Bertha site: high ground towards the middle is all front line, enemy land or taken",
                );
            }
            return None;
        };
        self.log(&format!(
            "economy: Bertha site at ({:.0}, {:.0}): its shells clear the terrain to {clear:.0} of {in_range:.0} \
             metal of the enemy's in range",
            site.x, site.z
        ));
        Some(site)
    }

    fn eco_create_super_wep_task(&mut self) {
        if self.fac.factories.is_empty() || self.eco.radars.is_empty() || self.eco_awaiting_power() {
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

        self.eco_add_powered(death, position, List::SuperWep);
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
    /// Whether we have a factory named `name`, standing or going up.
    pub(super) fn eco_has_factory(&self, name: &str) -> bool {
        self.fac.factories.values().any(|f| self.defs.name_of(f.unit) == name)
            || (self.eco.factory_tasks.iter().filter_map(|&id| self.eco.tasks.get(id)))
                .filter_map(|t| t.as_construction())
                .any(|c| self.defs.name(c.build_type) == name)
    }

    /// Now and then past some income, have a factory of a new type built
    /// (see [`NEW_FAC_INCOME`]).
    pub(super) fn eco_update_new_factory(&mut self, frame: i32) {
        if (frame + self.offset) % 30 != 0 || self.fac.factories.is_empty() || self.eco.new_fac_due {
            return;
        }
        let income = self.eco.static_income;
        let next = || frame + NEW_FAC_EVERY + ((kgb_rand() * 2.0 - 1.0) * f64::from(NEW_FAC_JITTER)) as i32;
        let Some(at) = self.eco.new_fac_at else {
            if income > NEW_FAC_INCOME {
                self.eco.new_fac_at = Some(next());
            }
            return;
        };
        let room = (self.fac.factories.len() as f32) < 1.0 + income / NEW_FAC_PER;
        let attacked = self.war.defense_targets().any(|(_, predicted)| !predicted);
        if frame < at || income <= NEW_FAC_INCOME || !room || self.eco.estalled || attacked {
            return;
        }
        if self.eco_new_factory_type().is_some() {
            self.eco.new_fac_due = true;
        }
        self.eco.new_fac_at = Some(next());
    }

    /// A type of factory we have none of yet, at random: of those whose
    /// units get about this map, the aircraft factories, and a strider hub
    /// once we have pylons (not the jump factory: nothing here tells it
    /// what to build).
    fn eco_new_factory_type(&self) -> Option<String> {
        let mut types: Vec<String> = self.eco.initial_fac_list.iter().cloned().collect();
        types.extend(["factorygunship", "factoryplane"].map(String::from));
        if !self.eco.pylons.is_empty() {
            types.push("striderhub".to_owned());
        }
        types.sort();
        types.dedup();
        types.retain(|f| self.defs.get(f).is_some() && !self.eco_has_factory(f));
        if types.is_empty() {
            return None;
        }
        // (The air factory for tempo weighs more as the game runs on.)
        let (gunship, plane) = self.eco_air_tempo_weights();
        let weight = |f: &str| match f {
            "factorygunship" => gunship,
            "factoryplane" => plane,
            _ => 1.0,
        };
        weighted_pick(&types.into_iter().collect(), weight, kgb_rand())
    }

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
    /// units get to metal ours cannot, a spider factory for Widows to open
    /// the way for our nukes, or a gunship factory to drop our assaults
    /// past the chokepoints guarding much of the enemy's value.
    /// The factory we open with, decided at the start so the commander suits
    /// it and allies can be told: the one set, the one picked, else a roll
    /// with the first factory's odds (see `eco_create_factory_task`), kept
    /// for it. Only the reach check can still rule it out.
    pub(crate) fn eco_roll_opening(&mut self) -> Option<String> {
        if !self.eco.user_fac.is_empty() {
            return Some(self.eco.user_fac.clone());
        }
        let list = &self.eco.potential_fac_list;
        if list.is_empty() {
            return None;
        }
        if let Some(k) = self.eco.dev_pick {
            return Some(picked_factory(list, k));
        }
        if self.merged_allies > 0 {
            return None;
        }
        // A shipyard half the time, a gunship plop in big teams, else any.
        let roll = if list.contains("factoryship") && kgb_rand() < 0.5 {
            Some("factoryship".to_owned())
        } else if list.contains("factorygunship") {
            Some("factorygunship".to_owned())
        } else {
            let n = ((kgb_rand() * list.len() as f64) as usize).min(list.len() - 1);
            list.iter().nth(n).cloned()
        };
        self.eco.opening.clone_from(&roll);
        roll
    }

    /// The factory a module wants, why, and what that answers in metal
    /// (for `logistics`).
    fn eco_find_requisition(&self) -> Option<(String, String, f32)> {
        if let Some((f, gap, fills)) = self.eco_reach_gap() {
            // The metal out of reach, as income over a payback period.
            let value = gap * self.eco.static_income * crate::logistics::PAYBACK;
            return Some((
                f.to_owned(),
                format!(
                    "{:.0}% of the metal we do not hold is out of our units' reach; it gets to {:.0}%",
                    100.0 * gap,
                    100.0 * fills
                ),
                value,
            ));
        }
        if let Some((f, gap, seen, closes)) = self.arsenal_gap() {
            return Some((
                f.to_owned(),
                format!(
                    "{gap:.0} of {seen:.0} metal of the enemy seen lately is out of our units' reach or under water; \
                     its units get to {closes:.0}"
                ),
                gap,
            ));
        }
        let owned = |name: &str| self.fac.factories.values().any(|f| self.defs.name_of(f.unit) == name);
        if !owned("factoryspider")
            && let Some(value) = self.scouts_want_factory().filter(|&v| v >= nukes::STRIKE_VALUE)
        {
            return Some((
                "factoryspider".to_owned(),
                format!("{value:.0} metal is behind anti-nukes: Widows to find and stun them"),
                value,
            ));
        }
        // (Ahead of the Widow scout, which otherwise takes the second
        // factory in nearly every game.)
        if let Some(r) = self.eco_air_fire() {
            return Some(r);
        }
        // (A cloak factory builds Scythes for it instead.)
        if !owned("factoryspider") && !owned("factorycloak") && self.scouts_deep_wanted() && !self.scouts_have_scout() {
            let (income, spare) = self.scouts_energy();
            return Some((
                "factoryspider".to_owned(),
                format!(
                    "{income:.0} energy income, {spare:.0} past spending all metal{}: a Widow to scout the enemy's \
                     land deep",
                    if self.misc_has_nuke() || self.missiles_silo().is_some() { ", a silo" } else { "" }
                ),
                self.intel.unaccounted().max(0.0),
            ));
        }
        // Firewalkers burn masses of light skirmishers.
        let skirms = self.war.enemy_light_skirm_value;
        if !owned("factoryjump")
            && self.eco.bots_viable
            && self.defs.get("factoryjump").is_some()
            && skirms >= FIREWALKER_REQUISITION
            && skirms >= FIREWALKER_SHARE * self.war.enemy_fighter_value
        {
            return Some((
                "factoryjump".to_owned(),
                format!(
                    "{skirms:.0} of the enemy's {:.0} metal of army seen is light skirmishers: Firewalkers",
                    self.war.enemy_fighter_value
                ),
                skirms,
            ));
        }
        let Some(s) = self.strategy.as_ref() else {
            return self.eco_air_tempo();
        };
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
                    guarded,
                ));
            }
            let prospect = self.drops_prospect_value();
            if prospect >= DROP_PROSPECT {
                return Some((
                    "factorygunship".to_owned(),
                    format!(
                        "{prospect:.0} metal of enemy buildings is much further on foot, past what holds the way, \
                         than by air: air drops"
                    ),
                    prospect,
                ));
            }
        }
        self.eco_air_tempo()
    }

    /// The air tempo dose now (see [`AIR_TEMPO_SHARE`]); zero when off.
    fn eco_air_tempo_dose(&self) -> f32 {
        if self.dev_off("airtempo") {
            return 0.0;
        }
        air_tempo_dose(self.frame as f32 / 1800.0, self.map_diag)
    }

    /// The enemy's standing value (the production estimate less what
    /// died, or at least what we have seen and not seen die), the static
    /// anti-air seen, and whether that or the enemy's air force calls for
    /// planes rather than gunships.
    fn eco_air_tempo_enemy(&self) -> (f32, f32, bool) {
        let (mut seen, mut static_aa) = (0.0f32, 0.0f32);
        for e in self.war.targets.values().filter(|e| e.identified && !e.is_dead) {
            seen += e.value;
            if e.is_static && e.is_aa {
                static_aa += e.value;
            }
        }
        let enemy = (self.intel.produced - self.intel.killed).max(seen);
        let planes = static_aa >= AIR_TEMPO_AA_PLANES || self.war.max_enemy_air_value >= AIR_TEMPO_AIR_PLANES;
        (enemy, static_aa, planes)
    }

    /// The weights of the gunship and the plane factory in the ordinary
    /// switch's roll (every other candidate weighs 1): the one air tempo
    /// prefers at 1 + `AIR_TEMPO_PICK` x dose.
    fn eco_air_tempo_weights(&self) -> (f64, f64) {
        let dose = self.eco_air_tempo_dose();
        if dose <= 0.0 {
            return (1.0, 1.0);
        }
        let w = 1.0 + f64::from(AIR_TEMPO_PICK * dose);
        if self.eco_air_tempo_enemy().2 { (1.0, w) } else { (w, 1.0) }
    }

    /// Log an air factory the ordinary switch rolled with air tempo's
    /// weight on it.
    fn eco_air_tempo_rolled(&self, name: &str, weight: f64) {
        if weight > 1.0 && matches!(name, "factorygunship" | "factoryplane") {
            self.log(&format!(
                "economy: {AIR_TEMPO_WHY} {:.2} weighed the roll: {name} at {weight:.1} to 1 ({:.0} min, map diagonal {:.0})",
                self.eco_air_tempo_dose(),
                self.frame as f32 / 1800.0,
                self.map_diag
            ));
        }
    }

    /// Whether an air factory may be asked for: none stands or is going
    /// up, and none was asked for this way before.
    fn eco_air_open(&self) -> bool {
        !self.eco.air_asked && !self.eco_has_factory("factorygunship") && !self.eco_has_factory("factoryplane")
    }

    /// An air factory for the firefighters (see [`AIR_FIRE_WHY`]): which,
    /// why, and what it is worth in metal.
    fn eco_air_fire(&self) -> Option<(String, String, f32)> {
        use crate::military::firefighters::{MEMORY, UNANSWERED_DIST, WANTED_AT};
        if self.dev_off("airfire") || self.dev_off("firefighters") || !self.eco_air_open() {
            return None;
        }
        let (weight, metal) = self.firefighters_unanswered();
        if weight < WANTED_AT {
            return None;
        }
        let value = metal * crate::logistics::PAYBACK / (MEMORY as f32 / 30.0);
        let (_, static_aa, planes) = self.eco_air_tempo_enemy();
        let name = if planes { "factoryplane" } else { "factorygunship" };
        self.defs.get(name)?;
        let kind = if planes {
            format!(
                "planes, past {static_aa:.0} metal of anti-air and {:.0} of aircraft seen",
                self.war.max_enemy_air_value
            )
        } else {
            "gunships".to_owned()
        };
        Some((
            name.to_owned(),
            format!(
                "{AIR_FIRE_WHY} {weight:.1}: {metal:.0} metal of mexes and power lost to raids in the last {:.0} min with no \
                 home defense team within {UNANSWERED_DIST:.0}; firefighters, {kind}",
                MEMORY as f32 / 1800.0
            ),
            value,
        ))
    }

    /// An air factory for tempo (see [`AIR_TEMPO_SHARE`]): which, why, and
    /// what it is worth in metal; none while one stands or is going up,
    /// after one was taken this way, or before it pays for itself.
    fn eco_air_tempo(&self) -> Option<(String, String, f32)> {
        if !self.eco_air_open() {
            return None;
        }
        let dose = self.eco_air_tempo_dose();
        if dose <= 0.0 {
            return None;
        }
        let (enemy, static_aa, planes) = self.eco_air_tempo_enemy();
        let value = AIR_TEMPO_SHARE * enemy * dose;
        let name = if planes { "factoryplane" } else { "factorygunship" };
        let cost = self.defs.get(name)?.cost(self.metal);
        if value < cost {
            return None;
        }
        let kind = if planes {
            format!(
                "planes, past {static_aa:.0} metal of anti-air and {:.0} of aircraft seen",
                self.war.max_enemy_air_value
            )
        } else {
            "gunships".to_owned()
        };
        Some((
            name.to_owned(),
            format!(
                "{AIR_TEMPO_WHY} {dose:.2} ({:.0} min, map diagonal {:.0}): {:.0}% of the enemy's {enemy:.0} metal \
                 standing is what aircraft would convert; {kind}",
                self.frame as f32 / 1800.0,
                self.map_diag,
                100.0 * AIR_TEMPO_SHARE
            ),
            value,
        ))
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
        if planned || self.missiles.planned.is_some_and(|f| self.frame - f < SILO_REPLAN) {
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
            self.missiles.planned = Some(self.frame);
            self.log(&format!(
                "missiles: silo at ({:.0}, {:.0}), {value:.0} metal of targets ({n}) in reach; summoning constructors",
                pos.x, pos.z
            ));
        }
    }

    /// Make a firebase of a missile silo just up (it stands forward, within
    /// reach of the enemy's buildings): its missiles first for resources, a
    /// heavy and a light laser and a flak turret between it and the enemy,
    /// an area shield over it against artillery, and caretakers that speed
    /// its missiles and repair it.
    pub fn eco_fortify_silo(&mut self, silo: Unit) {
        silo.execute_custom_command(cmd::PRIORITY, &[2.0], 0, NO_TIMEOUT);
        let at = silo.pos();
        let enemy = self.graph.get_enemy_center();
        let toward = if enemy == NULLPOS { self.graph.get_map_center() } else { enemy };
        let ahead = get_directional_point(at, toward, SILO_GUARD);
        let (dx, dz) = ((ahead.x - at.x) / SILO_GUARD, (ahead.z - at.z) / SILO_GUARD);
        let aside = |d: f32| Vec3::xz(ahead.x - dz * d, ahead.z + dx * d);
        let mut planned = Vec::new();
        for (name, p, list) in [
            ("turretheavylaser", aside(0.0), List::Porc),
            ("turretlaser", aside(SILO_GUARD_SPREAD), List::Porc),
            ("turretlaser", aside(-SILO_GUARD_SPREAD), List::Porc),
            ("turretaaflak", get_directional_point(at, toward, SILO_GUARD / 2.0), List::Aa),
            ("staticshield", get_directional_point(at, toward, 110.0), List::Construction),
        ] {
            let Some(def) = self.defs.get(name) else { continue };
            let site = self.eco_build_site(def, p, 0);
            let extra = (list != List::Construction).then_some(list);
            if site.x >= 0.0 && self.eco_add_simple(def, site, list, extra).is_some() {
                planned.push(name);
            }
        }
        // The caretakers, by the nearest constructor's reach.
        let wid = (self.eco.workers.values())
            .min_by(|a, b| distance(a.get_pos(), at).total_cmp(&distance(b.get_pos(), at)))
            .map(|w| w.id);
        let mut caretakers = 0;
        if let Some(wid) = wid {
            for _ in 0..SILO_CARETAKERS {
                let before = self.eco.nano_tasks.len();
                self.eco_create_nano_turret_task(wid, silo);
                caretakers += self.eco.nano_tasks.len() - before;
            }
        }
        self.log(&format!(
            "missiles: fortifying the silo at ({:.0}, {:.0}): {}, {caretakers} caretakers",
            at.x,
            at.z,
            if planned.is_empty() { "no room for defenses".to_owned() } else { planned.join(", ") }
        ));
    }

    /// Re-assess what factory the modules would have, now and then.
    pub(super) fn eco_update_requisition(&mut self, frame: i32) {
        if (frame + self.offset) % REQUISITION_PERIOD != 0
            || self.fac.factories.is_empty()
            || self.dev_off("requisition")
        {
            return;
        }
        if let Some(r) = self.op_requisition() {
            self.eco.requisition = Some(r);
            self.eco.requisition_value = 0.0;
            return;
        }
        self.eco_plan_sub_defense();
        self.eco_plan_missile_silo();
        self.eco_plan_watchtower();
        self.eco_plan_colony();
        let r = self.eco_find_requisition();
        if let Some((f, why, value)) = &r
            && self.eco.requisition_logged.as_ref() != Some(f)
        {
            self.log(&format!("economy: requisitioning a {f}: {why} ({value:.0} metal)"));
            self.eco.requisition_logged = Some(f.clone());
        }
        self.eco.requisition_value = r.as_ref().map_or(0.0, |r| r.2);
        self.eco.requisition = r.map(|(f, why, _)| (f, why));
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
        if !self.op_porc_allowed(position) {
            return;
        }
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
        if !self.op_porc_allowed(position) {
            return;
        }
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
        if !self.op_porc_allowed(position) {
            return;
        }
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

    /// Metal in our fixed defenses: built, and under way or planned.
    pub(crate) fn eco_defense_value(&self) -> f32 {
        let built: f32 = self.eco.porcs.iter().filter_map(|u| u.def()).map(|d| d.cost(self.metal)).sum();
        let planned: f32 = (self.eco.porc_tasks.iter())
            .filter_map(|&id| self.eco.tasks.get(id).and_then(|t| t.as_construction()))
            .filter(|c| c.target.is_none_or(|u| !self.eco.porcs.iter().any(|p| p.id() == u.id())))
            .map(|c| c.build_type.cost(self.metal))
            .sum();
        built + planned
    }

    /// Spend the posture's budget (see `posture`): one turret per call while
    /// the posture's own turrets hold less than their share, where it takes
    /// the most raid exposure off our metal per metal.
    ///
    /// A spot's exposure is its worth (an owned mex 1, a spot on our side
    /// not yet taken a quarter) by how near the enemy it is, halved by every
    /// turret covering it (any turret, the rest of the AI's too): one within
    /// [`POINT_COVER`] of it, or one at the front site its raid route crosses
    /// (see `zk_terrain::lanes`). A front site takes half the exposure of
    /// every spot routed through it, a point site half of its mex's and its
    /// neighbours': chokepoints win where many routes cross, point defense
    /// for a mex no front guards.
    fn eco_defend_posture(&mut self) {
        if self.eco.adjusted_income < 6.0 || self.posture_deficit() <= 0.0 {
            return;
        }
        // At most so many orders waiting for a crew.
        if self.posture_waiting() >= (self.fac.num_workers / 2).max(2) as usize {
            return;
        }
        let enemy = self.graph.get_enemy_center();
        if enemy.is_zero() {
            return;
        }
        let spots: Vec<(Vec3, bool)> = self.graph.metal_spots.iter().map(|s| (s.position, s.owned)).collect();
        let metal: Vec<f32> = self.graph.metal_spots.iter().map(|s| s.value).collect();
        let mean_metal = (metal.iter().sum::<f32>() / metal.len().max(1) as f32).max(0.01);
        let decisive = self.posture.decisive();
        let lanes = decisive.filter(|_| self.posture.laneness() > 0.0);
        let ours = |i: usize| lanes.is_some_and(|l| l.ours.get(i).copied().unwrap_or(false));
        // How far a raid has to come: by ground from the enemy's start where
        // the lanes measured it (a maze corner is far by any route), else
        // as the crow flies from their center.
        let reach = |i: usize| {
            decisive
                .and_then(|l| l.enemy_ground.get(i).copied())
                .filter(|d| d.is_finite())
                .unwrap_or_else(|| distance(spots[i].0, enemy))
        };
        let worth: Vec<f32> = (0..spots.len())
            .map(|i| {
                let near = (EXPOSURE_REF / reach(i).max(EXPOSURE_REF * 0.25)).min(4.0);
                let w = if spots[i].1 {
                    1.0
                } else if ours(i) {
                    FUTURE_WORTH
                } else {
                    0.0
                };
                w * near * metal[i] / mean_metal
            })
            .collect();
        // Front sites: (site, width per site, spots routed through it).
        let fronts: Vec<(Vec3, f32, Vec<usize>)> = lanes
            .map(|l| {
                (l.perimeter.fronts.iter())
                    .flat_map(|f| {
                        let width = f.span / f.sites.len().max(1) as f32;
                        (f.sites.iter().zip(&f.routes)).map(move |(s, r)| (Vec3::xz(s.x, s.z), width, r.clone()))
                    })
                    .collect()
            })
            .unwrap_or_default();
        // Turrets covering each spot.
        let mut cover: Vec<u32> = spots.iter().map(|&(p, _)| self.eco_porc_count(p, POINT_COVER) as u32).collect();
        for (site, _, routed) in &fronts {
            let n = self.eco_porc_count(*site, CHOKE_PORC_RADIUS) as u32;
            for &i in routed {
                if let Some(c) = cover.get_mut(i) {
                    *c += n;
                }
            }
        }
        let left = |i: usize| worth[i] * 0.5f32.powi(cover[i].min(16) as i32);
        // Not naked, not much more (user, 2026-10-26): no site where two of
        // our turrets' Lotus circles already meet.
        let safe = |ai: &Ai, p: Vec3| {
            ai.war.get_porc_threat(p) <= 0.0
                && ai.war.get_effective_threat(p) <= ai.war.get_total_friendly_threat(p)
                && !ai.posture_shuns(p)
                && ai.eco_porc_count(p, LOTUS_REACH) < MAX_OVERLAP
        };
        let budget = self.posture_deficit();
        // Candidates: (gain per metal, gain, site, zone radius, front width
        // or none for a point, how far the turret may stand from the site).
        let mut candidates: Vec<(f32, f32, Vec3, f32, Option<f32>, f32)> = Vec::new();
        for (site, width, routed) in &fronts {
            let gain: f32 = routed.iter().map(|&i| 0.5 * left(i)).sum();
            if gain >= MIN_GAIN && safe(self, *site) {
                candidates.push((0.0, gain, *site, CHOKE_PORC_RADIUS, Some(*width), CHOKE_PORC_RADIUS * 0.6));
            }
        }
        for (i, &(p, owned)) in spots.iter().enumerate() {
            if !owned || worth[i] <= 0.0 {
                continue;
            }
            let gain: f32 = (0..spots.len())
                .filter(|&j| spots[j].1 && distance(spots[j].0, p) < POINT_COVER)
                .map(|j| 0.5 * left(j))
                .sum();
            // From the value towards the enemy: between the mex and where
            // a raid on it comes from.
            let site = get_directional_point(p, enemy, POINT_FORWARD);
            if gain >= MIN_GAIN && safe(self, site) {
                // Within [`POINT_SHIFT`] of the site, so well inside the
                // cover that counts it (a turret pushed out of it was never
                // counted, and the spot drew turret after turret: fourteen
                // Lotuses in a Hide and Seek corner).
                candidates.push((0.0, gain, site, POINT_COVER, None, POINT_SHIFT));
            }
        }
        // Each candidate's turret: the zone's next kind (see `ZONE_MIX`).
        let mut priced: Vec<(f32, f32, Vec3, UnitDef, bool, f32)> = (candidates.into_iter())
            .filter_map(|(_, gain, site, radius, width, shift)| {
                let turret = self.eco_zone_turret(site, radius, width, budget)?;
                Some((gain / turret.cost(self.metal).max(1.0), gain, site, turret, width.is_some(), shift))
            })
            .collect();
        priced.sort_by(|a, b| b.0.total_cmp(&a.0));
        for (_, gain, site, turret, front, shift) in priced.into_iter().take(PLACE_TRIES) {
            // Spread out: no turret of ours within [`TURRET_SPACING`].
            let Some(at) = self.eco_spaced_site(turret, site, shift) else { continue };
            // Unstamped (`frame_issued` 0): stamped porc nobody took up is
            // dropped after 20 s (`eco_clean_orders`), before a crew from
            // far away can get there; the posture's ledger keeps its own
            // time.
            let Some(task) = self.eco_add_simple(turret, at, List::Porc, Some(List::Porc)) else { continue };
            let cost = turret.cost(self.metal);
            if front {
                self.posture.stats.front_orders += 1;
            } else {
                self.posture.stats.point_orders += 1;
            }
            let why = format!(
                "posture: {} {} takes {:.2} spot-exposure off ({:.0}% share)",
                if front { "front" } else { "point" },
                turret.name(),
                gain,
                self.posture.share * 100.0
            );
            self.posture_ordered(task, at, cost, cost * POSTURE_WORTH, &why);
            return;
        }
    }

    /// The next turret for a posture zone (within `radius` of `at`): the
    /// first kind of [`ZONE_MIX`] the income allows, the zone has fewer
    /// than [`SAME_KIND`] of (built or planned), and `budget` covers (a
    /// Lotus always fits). A narrow front (`width` under 250) starts with
    /// the riot turret.
    fn eco_zone_turret(&self, at: Vec3, radius: f32, width: Option<f32>, budget: f32) -> Option<UnitDef> {
        let income = self.eco.adjusted_income;
        let mut kinds: Vec<&(&str, f32)> = ZONE_MIX.iter().collect();
        if width.is_some_and(|w| w < 250.0) {
            kinds.sort_by_key(|(name, _)| *name != "turretriot");
        }
        let count = |id: i32| {
            let built = (self.eco.porcs.iter())
                .filter(|u| distance(u.pos(), at) < radius && u.def().is_some_and(|d| d.id() == id))
                .count();
            let planned = (self.eco.porc_tasks.iter())
                .filter(|&&t| distance(self.eco.task_pos(t), at) < radius)
                .filter(|&&t| {
                    self.eco.tasks.get(t).and_then(|t| t.as_construction()).is_some_and(|c| c.build_type.id() == id)
                })
                .count();
            built + planned
        };
        let lotus = self.defs.get("turretlaser").map_or(90.0, |d| d.cost(self.metal));
        (kinds.into_iter())
            .filter(|(_, wants)| income >= *wants)
            .filter_map(|(name, _)| self.defs.get(name))
            .find(|d| count(d.id()) < SAME_KIND && d.cost(self.metal) <= budget.max(lotus))
    }

    /// A legal site for `def` within `shift` of `near` with no turret of
    /// ours (built or planned) within [`TURRET_SPACING`]: splash then hits
    /// one at a time. Tries rings round `near`, nearest first.
    fn eco_spaced_site(&self, def: UnitDef, near: Vec3, shift: f32) -> Option<Vec3> {
        let others: Vec<Vec3> = (self.eco.porcs.iter().map(|u| u.pos()))
            .chain(self.eco.porc_tasks.iter().map(|&t| self.eco.task_pos(t)))
            .collect();
        let map = self.cb.map();
        let mut r = 0.0;
        while r <= shift {
            let steps = if r == 0.0 { 1 } else { 8 };
            for k in 0..steps {
                let a = k as f32 * std::f32::consts::TAU / steps as f32;
                let p = Vec3::new(near.x + r * a.cos(), near.y, near.z + r * a.sin());
                let site = map.find_closest_build_site(def, p, 48.0, 3, 0);
                if site.x < 0.0 || distance(site, near) > shift {
                    continue;
                }
                let overlap = others.iter().filter(|&&o| distance(o, site) < LOTUS_REACH).count();
                if overlap < MAX_OVERLAP && others.iter().all(|&o| distance(o, site) >= TURRET_SPACING) {
                    return Some(site);
                }
            }
            r += 70.0;
        }
        None
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
            // A spot an ally queued a mex at in the pregame is the ally's
            // (see `iq`): no task for it, and one made before the queue
            // arrived is dropped, until the claim lapses.
            if let Some(team) = self.iq_spot_claimed(position) {
                if self.iq_note_skipped(position) {
                    self.log(&format!(
                        "economy: spot at ({:.0},{:.0}) left to ally team {team}'s queue",
                        position.x, position.z
                    ));
                }
                let claimed: Vec<TaskId> = (self.eco.construction_tasks.iter().copied())
                    .filter(|&id| {
                        self.eco.tasks.get(id).is_some_and(|t| {
                            t.as_construction().is_some_and(|c| {
                                c.build_type.id() == mex.id()
                                    && c.target.is_none()
                                    && distance(t.position, position) < 1.0
                            })
                        })
                    })
                    .collect();
                for id in claimed {
                    self.eco.drop_construction_task(id);
                }
                continue;
            }
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

    fn eco_create_energy_task(&mut self, wid: i32, worker_pos: Vec3) {
        let mut position = worker_pos;
        if position == NULLPOS {
            return;
        }

        // A geothermal plant on a free vent of ours beats anything else
        // (see `geo`).
        if self.eco_create_geo_task(wid) {
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

        // Turbines: a batch up a hill of ours, if the better wind pays for
        // the walk; by the grid instead of solars, where the ground is high
        // enough (see `wind`).
        let wind_spot = self.graph.get_overdrive_sweet_spot(position, "energywind");
        if wind_spot != NULLPOS && self.eco_wind_hill(wid, wind_spot) {
            return;
        }
        let windy = wind_spot != NULLPOS && self.eco_wind_beats_solar(wind_spot);

        // for solars (a core of them anyway: they armor up under fire)
        let core =
            ((self.eco.solars.len() + self.eco.solar_tasks.len()) as f32) < (self.eco.raw_mex_income / 2.0).round();
        if (core || (!windy && kgb_rand() > 0.25)) && self.cb.map().elevation_at(position.x, position.z) > 0.0 {
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
        // Which plants the economy may build. The old ladder wanted eminent
        // territory (40% of the spots owned, rare on a contested map) and 35
        // energy for a fusion, three fusions for a singularity; geothermal
        // and wind counted for nothing. The plants are for overdrive (a
        // surplus), so they open with the economy instead (user,
        // 2026-10-26): fusions from [`FUSION_INCOME`], singularities from
        // [`SINGU_ENERGY`] energy income or two fusions, or either from the
        // map's density. `disable=ecoladder` keeps the old ladder.
        // Density opens them too, on any income (a 16v16 on a small map):
        // standing metal per usable area (see `posture`), one fusion per
        // density reference, a singularity from twice it.
        let dense = self.posture.density / self.posture_levers().density_ref.max(1.0);
        let (fusion_open, singu_open, allowed) = if self.dev_off("ecoladder") {
            let eminent = self.graph.eminent_territory;
            (eco.effective_income > 35.0 && eminent, nfus > 2 && eminent, (eco.static_income / 27.5).floor())
        } else {
            let fusions = (eco.fusions.iter()).filter(|u| self.defs.name_of(**u) == "energyfusion").count();
            (
                eco.static_income > FUSION_INCOME || dense >= 1.0,
                eco.effective_income_energy >= SINGU_ENERGY || fusions >= 2 || dense >= 2.0,
                (eco.static_income / 27.5).floor().max(dense.floor()),
            )
        };

        let name = if fusion_open
            && allowed > nfus as f32
            && !self.fac.factories.is_empty()
            && nft + nfus - started < 3
            && nft < 1 + self.merged_allies
        {
            // for fusions
            "energyfusion"
        } else if singu_open
            && nfus < 5 + i32::from(self.merged_allies > 1)
            && !self.fac.factories.is_empty()
            && eco.fusion_tasks.is_empty()
        {
            // for singus; a geothermal plant is upgraded first
            if self.eco_upgrade_geo(self.frame) {
                return;
            }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn list(names: &[&str]) -> BTreeSet<String> {
        names.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn factory_pick_leaves_the_jump_factory_out() {
        let with = list(&["factorycloak", "factoryjump", "factoryshield", "factoryveh"]);
        let without = list(&["factorycloak", "factoryshield", "factoryveh"]);
        for k in 0..7 {
            assert_eq!(picked_factory(&with, k), picked_factory(&without, k));
        }
        assert_eq!(picked_factory(&list(&["factoryjump"]), 3), "factoryjump");
    }

    #[test]
    fn air_tempo_dose_grows_with_time_and_map_size() {
        // Nothing before the threshold, on any map.
        for diag in [724.0, 1086.0, 1448.0, 2172.0] {
            assert_eq!(air_tempo_dose(0.0, diag), 0.0);
            assert_eq!(air_tempo_dose(AIR_TEMPO_FROM_MIN - 0.1, diag), 0.0);
            assert!(air_tempo_dose(AIR_TEMPO_FROM_MIN + 1.0, diag) > 0.0);
        }
        // Monotone in time (non-decreasing, strictly while ramping).
        let mut last = 0.0;
        for m in 0..60 {
            let d = air_tempo_dose(m as f32, 1448.0);
            assert!(d >= last, "minute {m}: {d} < {last}");
            if (m as f32) > AIR_TEMPO_FROM_MIN && (m as f32) <= AIR_TEMPO_FULL_MIN {
                assert!(d > last, "minute {m} should still ramp");
            }
            last = d;
        }
        assert_eq!(air_tempo_dose(AIR_TEMPO_FULL_MIN, AIR_TEMPO_DIAG), 1.0);
        assert_eq!(air_tempo_dose(90.0, AIR_TEMPO_DIAG), 1.0);
        // Monotone in the map's diagonal, bounded both ways.
        let mut last = 0.0;
        for diag in (400..3000).step_by(100) {
            let d = air_tempo_dose(AIR_TEMPO_FULL_MIN, diag as f32);
            assert!(d >= last, "diagonal {diag}: {d} < {last}");
            assert!((AIR_TEMPO_DIAG_MIN..=AIR_TEMPO_DIAG_MAX).contains(&d));
            last = d;
        }
        assert!(air_tempo_dose(20.0, 2172.0) > air_tempo_dose(20.0, 724.0));
    }

    #[test]
    fn weighted_pick_follows_the_weights() {
        let l = list(&["factorycloak", "factorygunship", "factoryplane", "factoryveh"]);
        let weight = |f: &str| if f == "factorygunship" { 3.0 } else { 1.0 };
        let mut counts = std::collections::BTreeMap::new();
        for i in 0..600 {
            let pick = weighted_pick(&l, weight, i as f64 / 600.0).unwrap();
            *counts.entry(pick).or_insert(0) += 1;
        }
        assert_eq!(counts["factorygunship"], 300);
        assert_eq!(counts["factorycloak"], 100);
        assert_eq!(counts["factoryplane"], 100);
        assert_eq!(counts["factoryveh"], 100);
        // Uniform when every weight is one; never past the end.
        assert_eq!(weighted_pick(&l, |_| 1.0, 0.0).as_deref(), Some("factorycloak"));
        assert_eq!(weighted_pick(&l, |_| 1.0, 0.999).as_deref(), Some("factoryveh"));
        assert_eq!(weighted_pick(&l, |_| 1.0, 1.0).as_deref(), Some("factoryveh"));
        assert_eq!(weighted_pick(&list(&[]), |_| 1.0, 0.5), None);
    }
}
