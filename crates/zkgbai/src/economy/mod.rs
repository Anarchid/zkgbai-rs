//! Economy: workers and construction/reclaim/repair tasks, energy and
//! overdrive grid, static defenses, superweapons.

mod assign;
pub mod build_ids;
mod colonize;
pub mod factory;
pub mod factory_manager;
mod fortify;
mod geo;
pub mod grid;
mod lotus;
mod planning;
mod silo;
pub mod tasks;
pub mod terraform;
mod watchtower;
mod wind;
pub mod worker;

pub use build_ids::BuildIds;
pub use factory::Factory;
pub use factory_manager::FactoryManager;
pub use tasks::{Nanotower, TaskId, TaskKind, TaskStore, WorkerTask};
pub use worker::Worker;

use std::cell::Cell;
use std::collections::{BTreeMap, BTreeSet, VecDeque};

use springai::{Callback, NO_TIMEOUT, Unit, UnitDef, Vec3, WeaponDef};

use crate::Ai;
use crate::defs::cmd;
use crate::kgbutil::HeightMap;
use crate::military::Porc;
use crate::util::distance;

/// A building within this of a construction job's site is that job's
/// (build sites snap to the build grid), elmos.
pub(super) const SITE_SLACK: f32 = 25.0;

pub struct EconomyManager {
    cb: Callback,
    pub sparrow: Option<Unit>,
    pub effective_income_metal: f32,
    pub static_income: f32,
    pub reclaim_income: f32,
    pub max_storage: f32,
    pub base_income: f32,
    pub effective_income: f32,
    pub metal: f32,
    /// Energy stored, less the reserve the game holds back for cloaks
    /// (`energy_reserve`; see the scouts).
    pub energy: f32,
    pub energy_reserve: f32,
    pub adjusted_income: f32,
    pub greed: f32,
    pub defended_fac: bool,
    pub spiders_viable: bool,
    pub bots_viable: bool,
    pub ships_viable: bool,
    /// Builders (including commanders) by unit id.
    pub workers: BTreeMap<i32, Worker>,
    /// Friendly static defenses.
    pub porcs: Vec<Unit>,
    /// Friendly static defenses with defense-power bookkeeping, by unit id.
    pub dporcs: BTreeMap<i32, Porc>,
    /// Caretakers by unit id.
    pub nanos: BTreeMap<i32, Nanotower>,
    pub fusions: Vec<Unit>,
    /// When the overdrive phase began (see `eco_overdrive_phase`).
    pub od_phase: Option<i32>,
    pub mexes: Vec<Unit>,

    /// All worker tasks; the lists below hold ids into it.
    pub tasks: TaskStore,
    /// Worker ids of commanders.
    pub commanders: Vec<i32>,
    /// Workers waiting for a new assignment (worker ids).
    pub idlers: VecDeque<i32>,
    /// Workers with an assignment, re-evaluated round-robin (worker ids).
    pub assigned: VecDeque<i32>,
    /// Workers that generate new tasks, round-robin (worker ids).
    pub seeders: VecDeque<i32>,
    pub factory_tasks: Vec<TaskId>,
    pub construction_tasks: Vec<TaskId>,
    pub reclaim_tasks: Vec<TaskId>,
    pub energy_reclaim_tasks: Vec<TaskId>,
    pub porc_tasks: Vec<TaskId>,
    pub combat_reclaim_tasks: Vec<TaskId>,
    pub repair_tasks: Vec<TaskId>,
    super_wep_tasks: Vec<TaskId>,
    radar_tasks: Vec<TaskId>,
    solar_tasks: Vec<TaskId>,
    wind_tasks: Vec<TaskId>,
    fusion_tasks: Vec<TaskId>,
    pylon_tasks: Vec<TaskId>,
    nano_tasks: Vec<TaskId>,
    storage_tasks: Vec<TaskId>,
    aa_tasks: Vec<TaskId>,
    airpad_tasks: Vec<TaskId>,

    radars: Vec<Unit>,
    airpads: Vec<Unit>,
    solars: Vec<Unit>,
    windgens: Vec<Unit>,
    /// Geothermal plants, plain and advanced (finished).
    geos: Vec<Unit>,
    pylons: Vec<Unit>,
    aas: Vec<Unit>,
    screamers: Vec<Unit>,
    pub(crate) super_weps: Vec<Unit>,
    storages: Vec<Unit>,
    initial_fac_list: BTreeSet<String>,
    potential_fac_list: BTreeSet<String>,
    new_workers: BTreeSet<i32>,
    new_mexes: Vec<Unit>,
    new_energy: Vec<Unit>,
    reclaim_record: VecDeque<f32>,

    pub effective_income_energy: f32,
    raw_mex_income: f32,
    /// The energy income the economy wants (stalled below it).
    energy_target: f32,
    /// Workers just ordered to terraform, until when they are left to it
    /// (the order reaches the engine a frame later; meanwhile they look
    /// free).
    pub(super) terraform_hold: BTreeMap<i32, i32>,
    /// min(metal, energy) usage, refreshed every 15 frames.
    pub effective_expenditure: f32,
    reclaim_value: Cell<f32>,
    last_rc_value_frame: Cell<i32>,
    energy_reclaim: bool,
    /// Actual energy shortage, independent of the desired overdrive income.
    pub(crate) hard_estalled: bool,
    pub estalled: bool,
    big_map: bool,
    water_damage: bool,
    added_facs: bool,
    have_planes: bool,
    anti_nuke: bool,
    screamer: bool,
    has_striders: bool,
    has_planes: bool,
    has_gunship: bool,
    has_super_wep: bool,
    enemy_has_air: bool,
    morphed_coms: bool,
    got_nuked: bool,
    s_wep: i32,
    chose_sup_wep: bool,
    user_fac: String,
    /// Development aid: the factories to build after the first.
    dev_facs: Vec<String>,
    /// Development aid: open with the n-th viable factory.
    dev_pick: Option<usize>,
    /// The first factory, rolled at the start (see `eco_roll_opening`).
    opening: Option<String>,
    /// Where the opening factory goes and its facing, as announced to
    /// allies in the pregame (see `crate::iq`).
    pub(crate) opening_site: Option<(UnitDef, Vec3, i32)>,
    /// A factory a module asked for, and why; and the last one logged.
    pub requisition: Option<(String, String)>,
    /// What the requisition answers, in metal (0 when the operator asked).
    pub requisition_value: f32,
    pub requisition_logged: Option<String>,
    /// When a factory of a new type is next due, and whether one is due now
    /// (see `planning::NEW_FAC_INCOME`).
    pub new_fac_at: Option<i32>,
    pub new_fac_due: bool,
    /// Whether an air factory was taken for firefighters or for tempo (see
    /// `planning::is_air_requisition`): once a game.
    pub air_asked: bool,
    /// Terraform commands issued (for their tags).
    pub terraform_tags: i32,
    /// Development aid: the terraform test, when on.
    pub terraform_test: Option<terraform::TerraformTest>,
    /// The ditch being dug, ditches dug (their middles), and ditches tried
    /// lately (middle, frame).
    pub ditch: Option<terraform::Ditch>,
    pub last_fortify_plan: i32,
    lotus_demand: Vec<lotus::Demand>,
    pub ditched: Vec<Vec3>,
    pub ditch_tries: Vec<(Vec3, i32)>,
    /// When a ditch none was worth digging was last journaled (`detour`).
    pub ditch_noted: i32,
    /// Turrets ordered for the chokepoints we fortify (see `detour`).
    pub fortify_tasks: BTreeSet<TaskId>,
    /// A watchtower for our strikes going up, and when to plan the next.
    watchtower: Option<watchtower::Watchtower>,
    /// Geothermal plants: vents, the upgrade, the wall box.
    geo: geo::Geothermal,
    /// Wind turbines: the model, hill sites.
    wind: wind::Wind,
    watchtower_after: i32,
    /// A colony being taken, and when to look for the next.
    colony: Option<colonize::Colony>,
    colony_after: i32,
    /// When a colony last wanted an air constructor.
    colony_wants_air: Option<i32>,
    /// Grid ranges and power needs by unit type.
    pub grid_defs: grid::GridDefs,
    /// A building that needs power, held back until its site has it.
    pub(super) pending_power: Option<grid::PendingPower>,
    /// When it was last said that no Bertha site was found.
    bertha_report: i32,
    last_repaired_unit: i32,
    nuke_wep_id: i32,
    wolv_mine_id: i32,

    build_ids: BuildIds,
    height_map: HeightMap,
}

/// The task category lists, for list-generic helpers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum List {
    Construction,
    Factory,
    SuperWep,
    Radar,
    Porc,
    Solar,
    Wind,
    Fusion,
    Pylon,
    Nano,
    Storage,
    Aa,
    Airpad,
    Reclaim,
    EnergyReclaim,
    CombatReclaim,
    Repair,
}

impl List {
    const ALL: [List; 17] = [
        List::Construction,
        List::Factory,
        List::SuperWep,
        List::Radar,
        List::Porc,
        List::Solar,
        List::Wind,
        List::Fusion,
        List::Pylon,
        List::Nano,
        List::Storage,
        List::Aa,
        List::Airpad,
        List::Reclaim,
        List::EnergyReclaim,
        List::CombatReclaim,
        List::Repair,
    ];
}

/// Remove the first occurrence of `x` (the original `ERASEONE`).
fn erase_one<T: PartialEq>(v: &mut Vec<T>, x: &T) {
    if let Some(i) = v.iter().position(|e| e == x) {
        v.remove(i);
    }
}

fn erase_one_dq(v: &mut VecDeque<i32>, x: i32) {
    if let Some(i) = v.iter().position(|&e| e == x) {
        v.remove(i);
    }
}

impl EconomyManager {
    pub fn new(cb: Callback) -> Self {
        let map = cb.map();
        let (w, h) = (map.width() as f32, map.height() as f32);
        let map_diag = (w * w + h * h).sqrt();
        EconomyManager {
            cb,
            sparrow: None,
            effective_income_metal: 0.0,
            static_income: 0.0,
            reclaim_income: 0.0,
            max_storage: 0.0,
            base_income: 0.0,
            effective_income: 0.0,
            metal: 0.0,
            energy: 0.0,
            energy_reserve: 0.0,
            adjusted_income: 0.0,
            greed: 0.0,
            defended_fac: false,
            spiders_viable: false,
            bots_viable: false,
            ships_viable: false,
            workers: BTreeMap::new(),
            porcs: Vec::new(),
            dporcs: BTreeMap::new(),
            nanos: BTreeMap::new(),
            fusions: Vec::new(),
            od_phase: None,
            mexes: Vec::new(),
            tasks: TaskStore::default(),
            commanders: Vec::new(),
            idlers: VecDeque::new(),
            assigned: VecDeque::new(),
            seeders: VecDeque::new(),
            factory_tasks: Vec::new(),
            construction_tasks: Vec::new(),
            reclaim_tasks: Vec::new(),
            energy_reclaim_tasks: Vec::new(),
            porc_tasks: Vec::new(),
            combat_reclaim_tasks: Vec::new(),
            repair_tasks: Vec::new(),
            super_wep_tasks: Vec::new(),
            radar_tasks: Vec::new(),
            solar_tasks: Vec::new(),
            wind_tasks: Vec::new(),
            fusion_tasks: Vec::new(),
            pylon_tasks: Vec::new(),
            nano_tasks: Vec::new(),
            storage_tasks: Vec::new(),
            aa_tasks: Vec::new(),
            airpad_tasks: Vec::new(),
            radars: Vec::new(),
            airpads: Vec::new(),
            solars: Vec::new(),
            windgens: Vec::new(),
            geos: Vec::new(),
            pylons: Vec::new(),
            aas: Vec::new(),
            screamers: Vec::new(),
            super_weps: Vec::new(),
            storages: Vec::new(),
            initial_fac_list: BTreeSet::new(),
            potential_fac_list: BTreeSet::new(),
            new_workers: BTreeSet::new(),
            new_mexes: Vec::new(),
            new_energy: Vec::new(),
            reclaim_record: VecDeque::new(),
            effective_income_energy: 0.0,
            raw_mex_income: 0.0,
            energy_target: 0.0,
            terraform_hold: BTreeMap::new(),
            effective_expenditure: 0.0,
            reclaim_value: Cell::new(0.0),
            last_rc_value_frame: Cell::new(0),
            energy_reclaim: false,
            hard_estalled: false,
            estalled: false,
            big_map: map_diag > 1270.0,
            water_damage: map.water_damage() > 0.0,
            added_facs: false,
            have_planes: false,
            anti_nuke: false,
            screamer: false,
            has_striders: false,
            has_planes: false,
            has_gunship: false,
            has_super_wep: false,
            enemy_has_air: false,
            morphed_coms: false,
            got_nuked: false,
            s_wep: 0,
            chose_sup_wep: false,
            user_fac: String::new(),
            dev_facs: Vec::new(),
            dev_pick: None,
            opening: None,
            opening_site: None,
            requisition: None,
            requisition_value: 0.0,
            requisition_logged: None,
            new_fac_at: None,
            new_fac_due: false,
            air_asked: false,
            terraform_tags: 0,
            terraform_test: None,
            ditch: None,
            last_fortify_plan: -300,
            lotus_demand: Vec::new(),
            ditched: Vec::new(),
            ditch_tries: Vec::new(),
            ditch_noted: 0,
            fortify_tasks: BTreeSet::new(),
            watchtower: None,
            watchtower_after: 0,
            geo: geo::Geothermal::default(),
            wind: wind::Wind::default(),
            colony: None,
            colony_after: 0,
            colony_wants_air: None,
            grid_defs: grid::GridDefs::default(),
            pending_power: None,
            bertha_report: i32::MIN / 2,
            last_repaired_unit: 0,
            nuke_wep_id: -1,
            wolv_mine_id: -1,
            build_ids: BuildIds::default(),
            height_map: HeightMap::new(cb),
        }
    }

    /// The pylon jobs (for `logistics`).
    pub(crate) fn pylon_tasks(&self) -> &[TaskId] {
        &self.pylon_tasks
    }

    fn list(&self, l: List) -> &Vec<TaskId> {
        match l {
            List::Construction => &self.construction_tasks,
            List::Factory => &self.factory_tasks,
            List::SuperWep => &self.super_wep_tasks,
            List::Radar => &self.radar_tasks,
            List::Porc => &self.porc_tasks,
            List::Solar => &self.solar_tasks,
            List::Wind => &self.wind_tasks,
            List::Fusion => &self.fusion_tasks,
            List::Pylon => &self.pylon_tasks,
            List::Nano => &self.nano_tasks,
            List::Storage => &self.storage_tasks,
            List::Aa => &self.aa_tasks,
            List::Airpad => &self.airpad_tasks,
            List::Reclaim => &self.reclaim_tasks,
            List::EnergyReclaim => &self.energy_reclaim_tasks,
            List::CombatReclaim => &self.combat_reclaim_tasks,
            List::Repair => &self.repair_tasks,
        }
    }

    fn list_mut(&mut self, l: List) -> &mut Vec<TaskId> {
        match l {
            List::Construction => &mut self.construction_tasks,
            List::Factory => &mut self.factory_tasks,
            List::SuperWep => &mut self.super_wep_tasks,
            List::Radar => &mut self.radar_tasks,
            List::Porc => &mut self.porc_tasks,
            List::Solar => &mut self.solar_tasks,
            List::Wind => &mut self.wind_tasks,
            List::Fusion => &mut self.fusion_tasks,
            List::Pylon => &mut self.pylon_tasks,
            List::Nano => &mut self.nano_tasks,
            List::Storage => &mut self.storage_tasks,
            List::Aa => &mut self.aa_tasks,
            List::Airpad => &mut self.airpad_tasks,
            List::Reclaim => &mut self.reclaim_tasks,
            List::EnergyReclaim => &mut self.energy_reclaim_tasks,
            List::CombatReclaim => &mut self.combat_reclaim_tasks,
            List::Repair => &mut self.repair_tasks,
        }
    }

    fn task_pos(&self, id: TaskId) -> Vec3 {
        self.tasks.get(id).map_or(Vec3::ZERO, |t| t.get_pos())
    }

    /// Whether a repair task for `target` exists.
    fn has_repair_task(&self, target: Unit) -> bool {
        self.repair_tasks
            .iter()
            .any(|&id| matches!(self.tasks.get(id).map(|t| &t.kind), Some(TaskKind::Repair(r)) if r.target == target))
    }

    /// End the task for all its workers and return them (the original
    /// `WorkerTask::stopWorkers`).
    fn stop_workers(&mut self, id: TaskId) -> Vec<i32> {
        let Some(task) = self.tasks.get_mut(id) else {
            return Vec::new();
        };
        let ws = std::mem::take(&mut task.assigned_workers);
        for w in &ws {
            if let Some(worker) = self.workers.get_mut(w) {
                worker.end_task();
            }
        }
        ws
    }

    /// Queue workers for reassignment.
    fn idle_workers(&mut self, ws: &[i32]) {
        for &w in ws {
            self.idlers.push_back(w);
            erase_one_dq(&mut self.assigned, w);
        }
    }

    /// Destroy a task: remaining workers end it, and it leaves every list.
    fn delete_task(&mut self, id: TaskId) {
        if let Some(task) = self.tasks.remove(id) {
            for w in &task.assigned_workers {
                if let Some(worker) = self.workers.get_mut(w) {
                    worker.end_task();
                }
            }
        }
        for l in List::ALL {
            erase_one(self.list_mut(l), &id);
        }
    }

    /// Drop a construction job the project tracker gave up on (see
    /// `logistics`).
    pub(crate) fn eco_logistics_drop(&mut self, id: TaskId) {
        if self.tasks.get(id).is_some_and(|t| t.as_construction().is_some()) {
            self.drop_construction_task(id);
        }
    }

    /// Stop a construction task, send its workers to be reassigned and
    /// remove it everywhere.
    fn drop_construction_task(&mut self, id: TaskId) {
        let idle = self.stop_workers(id);
        self.idle_workers(&idle);
        self.delete_task(id);
    }

    /// Forget a worker (dead, captured, ...): leave its task and every list.
    fn remove_worker(&mut self, wid: i32) {
        if let Some(mut w) = self.workers.remove(&wid) {
            w.clear_task(&mut self.tasks);
            if w.is_greedy {
                self.greed -= 1.0;
            }
        }
        erase_one(&mut self.commanders, &wid);
        erase_one_dq(&mut self.seeders, wid);
        erase_one_dq(&mut self.idlers, wid);
        erase_one_dq(&mut self.assigned, wid);
    }

    /// Register a new worker, replacing (and detaching) any stale one with
    /// the same id.
    fn insert_worker(&mut self, w: Worker) {
        if self.workers.contains_key(&w.id) {
            self.remove_worker(w.id);
        }
        self.workers.insert(w.id, w);
    }
}

/// Energy for overdrive (see `eco_update_income`): energy is wanted up to
/// this many times the mexes' base income, and (from this frame on) while
/// energy storage is less than this full.
const OD_ENERGY_RATIO: f32 = 2.6;
const OD_STORAGE_FROM: i32 = 30 * 60 * 5;
const OD_STORAGE_FILL: f32 = 0.5;

/// The overdrive phase (pylons, joined grids, energy wanted as with two
/// fusions) starts at two fusions, or at this much energy income from any
/// source (geothermal and wind plants made two fusions rare: user,
/// 2026-10-26), or once no more than [`OD_FREE_SHARE`] of the metal spots
/// are known to be unclaimed and uncontested (expansion is done) at an
/// income of [`OD_FREE_INCOME`] or more.
const OD_PHASE_ENERGY: f32 = 100.0;
const OD_FREE_SHARE: f32 = 0.15;
const OD_FREE_INCOME: f32 = 20.0;

/// Cross-module economy logic and module events.
impl Ai {
    /// `energycrew` (`disable=energycrew` stops it): metal floats for want of
    /// energy, so greedy constructors build energy too. Energy jobs are made only by
    /// non-greedy workers and greedy ones pay 10000 for them; early on
    /// nearly every constructor is greedy, so the commander alone builds
    /// energy, and on Horus a commander on a Fusion at 17 energy (horus/fix
    /// ab1, jump m1: metal at its 510 storage 3-7 min, 81% of 2-10 min
    /// floating) stalled the whole economy.
    pub(crate) fn eco_floating_on_energy(&self) -> bool {
        let eco = &self.eco;
        !self.dev_off("energycrew") && eco.estalled && eco.max_storage > 1.0 && eco.metal >= 0.8 * eco.max_storage
    }

    /// Whether the overdrive phase has begun (see [`OD_PHASE_ENERGY`]); once
    /// begun it stays. `disable=odphase` keeps the old rule: two fusions.
    pub(crate) fn eco_overdrive_phase(&self) -> bool {
        if self.dev_off("odphase") {
            return self.eco.fusions.len() >= 2;
        }
        self.eco.od_phase.is_some()
    }

    /// Start the overdrive phase when its time has come, saying why.
    fn eco_check_overdrive_phase(&mut self) {
        if self.eco.od_phase.is_some() {
            return;
        }
        let spots = &self.graph.metal_spots;
        let free = (spots.iter()).filter(|s| !s.owned && !s.hostile && self.war.get_threat(s.position) <= 0.0).count();
        let free_share = free as f32 / spots.len().max(1) as f32;
        let why = if self.eco.fusions.len() >= 2 {
            Some(format!("{} fusions", self.eco.fusions.len()))
        } else if self.eco.effective_income_energy >= OD_PHASE_ENERGY {
            Some(format!("energy income {:.0}", self.eco.effective_income_energy))
        } else if free_share <= OD_FREE_SHARE && self.eco.static_income >= OD_FREE_INCOME {
            Some(format!("{free} of {} metal spots free and uncontested", spots.len()))
        } else {
            None
        };
        if let Some(why) = why {
            self.eco.od_phase = Some(self.frame);
            self.log(&format!("economy: overdrive phase from now ({why})"));
            self.op_emit("economy", serde_json::json!({"event": "overdrive phase", "why": why, "t": self.frame / 30}));
        }
    }

    pub fn eco_init(&mut self) {
        let terrain = crate::kgbutil::TerrainAnalyzer::new(self);
        self.eco.initial_fac_list = terrain.get_initial_fac_list().clone();
        self.eco.potential_fac_list = self.eco.initial_fac_list.clone();
        self.eco.spiders_viable = terrain.are_spiders_viable();
        self.eco.bots_viable = terrain.are_bots_viable();
        self.eco.ships_viable = terrain.are_ships_viable();
        self.eco.build_ids = BuildIds::new(&self.defs);
        self.eco.grid_defs = grid::GridDefs::new(self.cb);

        self.eco.nuke_wep_id = self.cb.weapon_def_by_name("staticnuke_crblmssl").map_or(-1, |w| w.id());
        self.eco.wolv_mine_id = self.defs.id("wolverine_mine");

        // Development aid: the factories to build, first (as `kgbfac` sets
        // it) to last, comma-separated.
        // (The AI option `factory` for one AI only.)
        if let Some(facs) =
            self.cb.option("factory").filter(|f| !f.trim().is_empty()).or_else(|| std::env::var("ZKGBAI_FACTORY").ok())
        {
            let mut facs = facs.split(',').map(str::trim).filter(|f| self.defs.get(f).is_some()).map(str::to_owned);
            self.eco.user_fac = facs.next().unwrap_or_default();
            self.eco.dev_facs = facs.collect();
        }
        self.eco.dev_pick = (self.cb.option("factory_pick"))
            .or_else(|| std::env::var("ZKGBAI_FACTORY_PICK").ok())
            .and_then(|n| n.trim().parse::<f32>().ok())
            .filter(|&n| n >= 0.0)
            .map(|n| n as usize);
        if let Some(n) = self.eco.dev_pick {
            self.log(&format!("dev: opening with viable factory #{n}"));
        }
        if std::env::var_os("ZKGBAI_TERRAFORM_TEST").is_some() {
            self.eco.terraform_test = Some(terraform::TerraformTest::default());
        }

        self.eco_capture_mexes();
    }

    /// Chat message from `player`.
    /// The terrain heights as at the start of the game.
    pub fn eco_height_map(&self) -> &HeightMap {
        &self.eco.height_map
    }

    pub fn eco_message(&mut self, player: i32, message: &str) {
        let _ = player;
        if self.merged_allies == 0 && message.starts_with("kgbfac") {
            // port: the C++ tested `find("veh") == 6` (only "kgbfacveh");
            // the Java original accepted any suffix ("kgbfac veh").
            const CHOICES: [(&str, &str); 8] = [
                ("veh", "factoryveh"),
                ("hover", "factoryhover"),
                ("tank", "factorytank"),
                ("cloak", "factorycloak"),
                ("shield", "factoryshield"),
                ("amph", "factoryamph"),
                ("spider", "factoryspider"),
                ("jump", "factoryjump"),
            ];
            match CHOICES.iter().find(|(suffix, _)| message.ends_with(suffix)) {
                Some((_, fac)) => self.eco.user_fac = (*fac).to_owned(),
                None => {
                    self.say("Invalid factory name!");
                    self.say("Valid choices: veh, hover, tank, cloak, shield, amph, spider, jump");
                }
            }

            if !self.eco.user_fac.is_empty() {
                let name =
                    self.defs.get(&self.eco.user_fac).map_or_else(|| self.eco.user_fac.clone(), |d| d.human_name());
                self.say(&format!("Set first factory: {name}"));
            }
        } else if self.merged_allies > 0 && message.starts_with("kgbfac") {
            self.say("Teams mode, cannot set first factory!");
        }
    }

    /// Metal in our economy's buildings and factories (what an enemy
    /// rarely sees of ours, and we of its).
    pub fn eco_infrastructure_value(&self) -> f32 {
        let eco = &self.eco;
        let cost = |u: &Unit| if u.health() > 0.0 { u.def().map_or(0.0, |d| d.cost(self.metal)) } else { 0.0 };
        let lists = [
            &eco.mexes,
            &eco.solars,
            &eco.windgens,
            &eco.fusions,
            &eco.pylons,
            &eco.storages,
            &eco.radars,
            &eco.airpads,
        ];
        lists.iter().flat_map(|l| l.iter()).map(cost).sum::<f32>()
            + eco.nanos.values().map(|n| cost(&n.unit)).sum::<f32>()
            + self.fac.factories.values().map(|f| cost(&f.unit)).sum::<f32>()
    }

    /// Energy-stalled with metal to spare: a storage, a factory or a
    /// superweapon now would feed the stall that starves them (metal above
    /// 80% of storage is what a stall produces, and it used to call for a
    /// storage). Energy comes first.
    pub fn eco_energy_starved(&self) -> bool {
        !self.dev_off("stallgate") && self.eco.estalled && self.eco.metal > 200.0
    }

    pub fn eco_update(&mut self, frame: i32) {
        if frame % 300 == self.offset % 300 {
            self.eco_check_nanos();
        }

        if frame % 15 == self.offset % 15 {
            self.eco_update_income(frame);
        }
        self.eco_energy_survey(frame);
        if frame % 1800 == self.offset % 1800 && frame > 0 {
            self.eco_energy_summary();
        }

        self.eco_plan_lotus_upgrades(frame);
        self.eco_dig_ditches(frame);
        self.eco_plan_grid(frame);
        self.eco_update_requisition(frame);
        self.eco_update_new_factory(frame);

        if frame % 30 == self.offset % 30 {
            self.eco_update_silo_preparation();
            self.eco_capture_mexes();
            self.eco_defend_links();
            self.eco_defend_mexes();

            // remove finished or invalidated tasks
            self.eco_clean_orders();

            for u in &self.eco.screamers {
                if u.stockpile_queued() < 2 {
                    u.queue_stockpile(0);
                }
            }

            // Two missiles in store; against an anti-nuke one: for what it
            // does not cover, or to fire past it stunned (see `nukes`).
            for &u in &self.eco.super_weps {
                let wanted = if self.war.enemy_has_anti_nuke {
                    u.stockpile() + u.stockpile_queued() < 1
                } else {
                    u.stockpile() + u.stockpile_queued() < 2
                };
                if self.defs.name_of(u) == "staticnuke" && !u.is_being_built() && wanted {
                    u.queue_stockpile(0);
                }
            }

            // if greedy workers make up too much of the workforce, convert them.
            let n = self.eco.workers.len() as f32;
            let tf = self.graph.territory_fraction;
            for w in self.eco.workers.values_mut() {
                let greed = self.eco.greed;
                if n > greed && (greed <= 2.0 || greed <= n * (1.0 - tf * tf).min(1.0)) {
                    break;
                }
                if w.is_greedy {
                    w.is_greedy = false;
                    self.eco.greed -= 1.0;
                }
            }

            self.eco_plan_geo(frame);
            self.eco.terraform_hold.retain(|_, &mut until| until > frame);

            if self.eco.effective_income > 30.0 && self.eco.sparrow.is_none() && !self.eco.has_planes {
                self.eco_morph_sparrow();
            }

            /*if !self.eco.morphed_coms && self.eco.adjusted_income > 20.0 {
                self.eco_morph_coms();
            }*/
        }

        self.eco_assign_workers();
        self.iq_tick();
    }

    /// The 15-frame income/stall bookkeeping block of `update`.
    fn eco_update_income(&mut self, frame: i32) {
        self.eco_check_overdrive_phase();
        let merged = self.merged_allies as f32;
        let odreserve = !self.dev_off("odreserve");
        let odphase_off = self.dev_off("odphase");
        let (m, e) = (self.metal, self.energy);
        let team = self.team_id;
        let eco = &mut self.eco;
        eco.raw_mex_income = 0.0;
        eco.effective_income_metal = 2.0 * (1.0 + merged);
        eco.effective_income_energy = 2.0 * (1.0 + merged);
        eco.max_storage = (500.0 * (eco.commanders.len() + eco.storages.len()) as f32).max(0.5);

        for ms in &eco.mexes {
            eco.raw_mex_income += ms.rules_param_float("mexIncome", 0.0);
            eco.effective_income_metal += ms.rules_param_float("current_metalIncome", 0.0);
        }
        for w in &eco.commanders {
            if let Some(w) = eco.workers.get(w) {
                eco.effective_income_metal += w.unit.rules_param_float("current_metalIncome", 0.0);
                eco.effective_income_energy += w.unit.rules_param_float("current_energyIncome", 0.0);
            }
        }
        for u in eco.solars.iter().chain(&eco.windgens).chain(&eco.fusions).chain(&eco.geos) {
            eco.effective_income_energy += u.rules_param_float("current_energyIncome", 0.0);
        }

        eco.base_income = eco.effective_income_metal.min(eco.effective_income_energy);

        let economy = eco.cb.economy();
        eco.metal = economy.current(m);
        // (A game that takes no reserve from AIs leaves it at 0.)
        eco.energy_reserve = eco.cb.game().team_rules_param_float(team, "energyReserve", 0.0).min(eco.max_storage);
        eco.energy = (economy.current(e) - eco.energy_reserve).max(0.0);

        let expend_metal = economy.usage(m);
        let expend_energy = economy.usage(e);

        eco.effective_income = eco.effective_income_metal.min(eco.effective_income_energy);
        eco.effective_expenditure = expend_metal.min(expend_energy);

        eco.static_income = eco.effective_income_metal;

        let current_reclaim: f32 = eco.workers.values().map(|w| w.unit.resource_make(m)).sum();
        eco.reclaim_record.push_back(current_reclaim);

        // Take a 5 second poll of reclaim income, add half to static income to turn reclaim into units.
        if eco.reclaim_record.len() > 10 {
            eco.reclaim_record.pop_front();
            eco.reclaim_income = eco.reclaim_record.iter().sum::<f32>() / 10.0;

            if eco.static_income > 20.0 || self.graph.eminent_territory {
                eco.static_income += eco.reclaim_income;
            } else {
                eco.static_income += eco.reclaim_income / 2.0;
            }
        }

        eco.adjusted_income = (eco.static_income / (1.0 + merged)) + ((1.0 + merged) * (frame as f32 / 1800.0));

        if eco.fusions.len() > 3 {
            let prio = if !eco.fusion_tasks.is_empty() || eco.energy <= 100.0 { 1.0 } else { 2.0 };
            for u in &eco.super_weps {
                u.execute_custom_command(cmd::PRIORITY, &[prio], 0, NO_TIMEOUT);
            }
        }

        // note: cloaky uniquely requires running energy rich.
        if self.merged_allies < 2 {
            for f in self.fac.factories.values() {
                match self.defs.name_of(f.unit) {
                    "striderhub" => eco.has_striders = true,
                    "factoryplane" => eco.has_planes = true,
                    "factorygunship" => eco.has_gunship = true,
                    _ => {}
                }
            }
        }
        // Energy for overdrive (`odreserve`): ZK spends on overdrive the
        // spare energy times how full energy storage is, so energy only
        // when stalled leaves almost none (CircuitAI: 37 E/s on overdrive at
        // minute 10 against our 13; energy 1.9x mex income at minute 5 to
        // our 1.3, 2.9x to 2.1 at minute 15).
        let fusions = eco.fusions.len().max(if eco.od_phase.is_some() && !odphase_off { 2 } else { 0 });
        let eratio = if odreserve { OD_ENERGY_RATIO } else { 2.0 } + fusions as f32;
        eco.energy_target = eco.raw_mex_income * eratio;
        //float eratio = (eFac ? 1.5f : 1.f) + (graphManager.territoryFraction + (graphManager.territoryFraction * graphManager.territoryFraction)) / 2.f;
        let tf = self.graph.territory_fraction;
        eco.hard_estalled = (eco.max_storage > 1.0
            && eco.energy < (eco.effective_income_energy * (1.0 + tf)).min(eco.max_storage / 2.0))
            || eco.static_income > eco.effective_income_energy;
        eco.estalled = eco.hard_estalled
            || eco.raw_mex_income * eratio > eco.effective_income_energy
            || (odreserve
                && frame > OD_STORAGE_FROM
                && eco.max_storage > 1.0
                && eco.energy + eco.energy_reserve < OD_STORAGE_FILL * eco.max_storage);
        if eco.max_storage > 1.0 && eco.hard_estalled && eco.metal / eco.max_storage > 0.5 {
            eco.energy_reclaim = true;
        }
        if eco.max_storage > 1.0 && eco.energy / eco.max_storage > 0.75 {
            eco.energy_reclaim = false;
        }

        self.eco_set_eco_priorities();
    }

    pub fn eco_unit_created(&mut self, unit: Unit, builder: Option<Unit>) {
        let Some(def) = unit.def() else {
            return;
        };
        let def_name = self.defs.name(def).to_owned();
        let def_name = def_name.as_str();

        if def_name == "planescout" {
            self.eco.sparrow = Some(unit);
        }

        if def.speed() > 0.0 && !def.build_options().is_empty() && !self.defs.is_commander(def) {
            unit.set_move_state(0, 0);
            self.eco.new_workers.insert(unit.id());
        }

        // Track info for construction tasks
        if let Some(builder) = builder {
            if unit.is_being_built() {
                if def.speed() == 0.0 {
                    // The builder's job, if this stands at its site: a builder
                    // given a new job may yet start what it was building, and
                    // that must not become the new job's (it would send all
                    // who take the job to repair it, and never build there).
                    let upos = unit.pos();
                    let job = self.eco.workers.get(&builder.id()).and_then(|w| w.task);
                    if let Some(t) = job.and_then(|t| self.eco.tasks.get(t)).filter(|t| {
                        distance(t.get_pos(), upos) >= SITE_SLACK
                            && t.as_construction().is_some_and(|c| c.build_type == def)
                    }) {
                        let p = t.get_pos();
                        self.log(&format!(
                            "economy: a {} started at ({:.0}, {:.0}) by a builder whose job is at ({:.0}, {:.0})",
                            self.defs.name(def),
                            upos.x,
                            upos.z,
                            p.x,
                            p.z
                        ));
                    }
                    let own_task = job.filter(|&t| {
                        self.eco.tasks.get(t).is_some_and(|t| {
                            distance(t.get_pos(), upos) < SITE_SLACK
                                && t.as_construction().is_some_and(|c| c.build_type == def)
                        })
                    });
                    if let Some(c) =
                        own_task.and_then(|t| self.eco.tasks.get_mut(t)).and_then(|t| t.as_construction_mut())
                    {
                        c.target = Some(unit);
                    } else {
                        for &id in &self.eco.construction_tasks {
                            if let Some(t) = self.eco.tasks.get_mut(id) {
                                let dist = distance(t.get_pos(), upos);
                                if let Some(c) =
                                    t.as_construction_mut().filter(|c| dist < SITE_SLACK && c.build_type == def)
                                {
                                    c.target = Some(unit);
                                }
                            }
                        }
                    }
                }
            } else {
                // instant factory plops only call unitcreated, not unitfinished.
                if self.eco.commanders.contains(&builder.id()) {
                    let task = self.eco.workers.get(&builder.id()).and_then(|w| w.task);
                    if let Some(w) = self.eco.workers.get_mut(&builder.id()) {
                        w.clear_task(&mut self.eco.tasks);
                    }
                    if let Some(task) = task {
                        self.eco.delete_task(task);
                    }
                }
            }
        }

        if def_name == "staticmex" {
            self.eco.new_mexes.push(unit);
        }

        if def_name == "energysolar" || def_name == "energywind" {
            self.eco.new_energy.push(unit);
        }

        // An advanced plant from a morph comes finished.
        if def_name == "energyheavygeo" && !unit.is_being_built() && !self.eco.geos.contains(&unit) {
            self.eco.geos.push(unit);
        }

        if matches!(def_name, "factoryplane" | "striderhub" | "factorygunship") && self.eco.defended_fac {
            self.eco_defend_fusion(unit.pos(), true);
        }

        if def_name == "energypylon" {
            self.eco_defend_mex(unit.pos());
            self.eco.pylons.push(unit);
        }

        if matches!(
            def_name,
            "turretmissile"
                | "turretlaser"
                | "turretheavylaser"
                | "turretgauss"
                | "turretemp"
                | "turretriot"
                | "turretaalaser"
        ) {
            self.eco.porcs.push(unit);
            if def_name != "turretaalaser" {
                self.eco.dporcs.insert(unit.id(), Porc::new(unit, self.team_id));
            }
        }

        if matches!(def_name, "turretaalaser" | "turretaaflak" | "turretaafar") {
            self.eco.aas.push(unit);
        }

        if def_name == "staticrearm" {
            self.eco.airpads.push(unit);
        }

        if matches!(def_name, "energyfusion" | "energysingu" | "staticheavyarty" | "turretaaheavy" | "turretaafar") {
            self.eco_defend_fusion(unit.pos(), false);
        }
        // A geothermal plant is a fusion-lite: a Lotus guards it.
        if def_name == "energygeo" && !self.dev_off("geo") {
            self.eco_defend_geo(unit.pos());
        }

        if def_name == "energyfusion" || def_name == "energysingu" {
            self.eco.fusions.push(unit);
            if !self.eco.screamer && self.eco.fusions.len() > 4 {
                self.eco_create_screamer_task();
            }
        }

        if self.merged_allies > 1
            && (def_name == "striderdante" || def_name == "striderscorpion")
            && self.misc_handler.striders.is_empty()
            && !self.eco.has_repair_task(unit)
        {
            let t = WorkerTask::repair(unit, def_name, &self.unit_classes);
            let id = self.eco.tasks.insert(t);
            self.eco.repair_tasks.push(id);
        }

        if def_name == "striderfunnelweb" && self.fac.num_funnels == 0 && !self.eco.has_repair_task(unit) {
            let t = WorkerTask::repair(unit, def_name, &self.unit_classes);
            let id = self.eco.tasks.insert(t);
            self.eco.repair_tasks.push(id);
        }

        if def_name == "factoryplane" {
            self.eco.have_planes = true;
        }

        if def_name == "turretaaheavy" {
            self.eco.screamers.push(unit);
        }

        if def_name == "staticantinuke" {
            unit.execute_custom_command(cmd::PRIORITY, &[2.0], 0, NO_TIMEOUT);
        }

        // Area cloakers pay for their field at misc priority: a field that
        // fails shows all it hides.
        if matches!(def_name, "cloakjammer" | "staticjammer") {
            unit.execute_custom_command(cmd::MISC_PRIORITY, &[2.0], 0, NO_TIMEOUT);
        }

        if matches!(def_name, "staticheavyarty" | "staticnuke" | "zenith" | "raveparty") {
            self.eco.super_weps.push(unit);
            if def_name == "staticnuke" {
                unit.execute_custom_command(cmd::MISC_PRIORITY, &[2.0], 0, NO_TIMEOUT);
            }
        }
    }

    pub fn eco_unit_finished(&mut self, unit: Unit) {
        let Some(def) = unit.def() else {
            return;
        };
        let def_name = self.defs.name(def).to_owned();
        let def_name = def_name.as_str();

        if def_name == "staticmex" {
            self.eco.mexes.push(unit);
            erase_one(&mut self.eco.new_mexes, &unit);
            // Mexes float: the ground under it tells.
            let p = unit.pos();
            if self.cb.map().elevation_at(p.x, p.z) < 0.0 {
                self.log(&format!("economy: mex at sea at ({:.0}, {:.0})", p.x, p.z));
            }
            unit.execute_custom_command(cmd::PRIORITY, &[1.0], 0, NO_TIMEOUT);

            if !self.eco.defended_fac
                && (self.eco.mexes.len() as i32 > 1 + (self.merged_allies * 2) || self.graph.territory_fraction > 0.2)
            {
                let facs: Vec<Vec3> = self.fac.factories.values().map(|f| f.get_pos()).collect();
                for p in facs {
                    self.eco_defend_fac(p);
                }
                self.eco.defended_fac = true;
            }
        }

        if def_name == "energysolar" {
            self.eco.solars.push(unit);
        }

        if def_name == "energywind" {
            self.eco.windgens.push(unit);
        }

        if matches!(def_name, "energygeo" | "energyheavygeo") && !self.eco.geos.contains(&unit) {
            self.eco.geos.push(unit);
        }

        if def_name == "energysolar" || def_name == "energywind" {
            erase_one(&mut self.eco.new_energy, &unit);
            unit.execute_custom_command(cmd::PRIORITY, &[1.0], 0, NO_TIMEOUT);
        }

        if def_name == "staticradar" {
            self.eco.radars.push(unit);
        }

        if def_name == "planelightscout" {
            self.eco.sparrow = Some(unit);
        }

        if def_name == "staticstorage" {
            self.eco.storages.push(unit);
        }

        if matches!(def_name, "energysingu" | "staticnuke" | "zenith" | "raveparty") {
            self.eco_defend_singu(unit.pos());
            for n in self.eco.nanos.values() {
                if n.target == unit {
                    n.unit.self_destruct(0);
                }
            }
            let unneeded: Vec<TaskId> = self
                .eco
                .nano_tasks
                .iter()
                .copied()
                .filter(|&id| {
                    self.eco.tasks.get(id).and_then(|t| t.as_construction()).is_some_and(|c| c.ct_target == Some(unit))
                })
                .collect();
            for id in unneeded {
                let idle = self.eco.stop_workers(id);
                self.eco.idle_workers(&idle);
                self.eco.delete_task(id);
            }
        }

        if matches!(def_name, "staticnuke" | "zenith" | "raveparty") {
            self.eco.has_super_wep = true;
        }

        if def.speed() > 0.0 && !def.build_options().is_empty() && self.defs.is_commander(def) {
            let mut w = Worker::new(unit, self.team_id);
            w.is_com = true;
            let id = w.id;
            self.eco.insert_worker(w);
            self.eco.new_workers.remove(&id);
            self.eco.commanders.push(id);
            self.eco_generate_tasks(id);
            self.eco_assign_worker(id);
            self.eco.assigned.push_back(id);
            self.eco.seeders.push_back(id);
            unit.set_move_state(0, 0);
        }

        // The job at its site is done; any other that took it for its
        // building builds its own again.
        let upos = unit.pos();
        let targeting: Vec<TaskId> = (self.eco.construction_tasks.iter().copied())
            .filter(|&id| {
                self.eco.tasks.get(id).and_then(|t| t.as_construction()).is_some_and(|c| c.target == Some(unit))
            })
            .collect();
        let mut finished = None;
        for id in targeting {
            let Some(t) = self.eco.tasks.get_mut(id) else { continue };
            let p = t.get_pos();
            if finished.is_none() && distance(p, upos) < SITE_SLACK {
                finished = Some(id);
            } else if let Some(c) = t.as_construction_mut() {
                c.target = None;
                self.log(&format!(
                    "economy: the {def_name} job at ({:.0}, {:.0}) had the one finished at ({:.0}, {:.0}) for its own",
                    p.x, p.z, upos.x, upos.z
                ));
            }
        }
        if let Some(id) = finished {
            if def_name == "staticcon" {
                let ct_target = self.eco.tasks.get(id).and_then(|t| t.as_construction()).and_then(|c| c.ct_target);
                match ct_target {
                    Some(target) => {
                        let nt = Nanotower { unit, target };
                        self.eco.nanos.insert(unit.id(), nt);
                        unit.set_repeat(true, 0);
                        unit.guard(nt.target, 0);
                        self.eco_defend_nano(nt);
                    }
                    // port: a caretaker without a target would be self-destructed
                    // by checkNanos anyway (the C++ stored a null target).
                    None => {
                        unit.self_destruct(0);
                    }
                }
            }

            self.eco.drop_construction_task(id);
        }
    }

    pub fn eco_unit_damaged(
        &mut self,
        unit: Unit,
        attacker: Option<Unit>,
        damage: f32,
        dir: Vec3,
        weapon: Option<WeaponDef>,
        paralyzer: bool,
    ) {
        let _ = (damage, dir, paralyzer);
        let h = unit;
        let Some(hdef) = h.def() else {
            return;
        };
        if hdef.id() == self.eco.wolv_mine_id {
            return;
        }

        // add repair tasks for damaged units
        // An unfinished silo is repaired through its construction job so
        // a generic repair job cannot bypass its fortification prerequisites.
        let preparing_silo = self.defs.name(hdef) == crate::military::missiles::SILO && h.is_being_built();
        if h.id() != self.eco.last_repaired_unit
            && h.health() > 0.0
            && !self.defs.name(hdef).contains("bomber")
            && !preparing_silo
        {
            self.eco.last_repaired_unit = h.id();
            if !self.eco.has_repair_task(h) {
                let t = WorkerTask::repair(h, self.defs.name(hdef), &self.unit_classes);
                let id = self.eco.tasks.insert(t);
                self.eco.repair_tasks.push(id);
            }
        }

        let hostile = attacker.is_none_or(|a| a.ally_team() != self.ally_team_id);
        if weapon.is_some_and(|w| w.id() == self.eco.nuke_wep_id) && hostile {
            self.eco.got_nuked = true;
        }

        // fortify mexes if they die
        if hdef.id() == self.eco.build_ids.mex_id && h.health() <= 0.0 && !h.is_being_built() {
            let pos = h.pos();
            if self.graph.get_closest_spot(pos).is_some_and(|s| s.owned) {
                // only fortify mexes that had already been completed
                self.eco_fortify_mex(pos, weapon);
            }
        }

        // If it was a building under construction, reset the builder's target
        if hdef.speed() == 0.0
            && (h.health() <= 0.0 || !hdef.is_able_to_attack())
            && hostile
            && weapon.is_some_and(|w| self.unit_classes.porc_weps.contains(&w.id()))
        {
            let found = self.eco.construction_tasks.iter().copied().find(|&id| {
                self.eco.tasks.get(id).and_then(|t| t.as_construction()).is_some_and(|c| c.target == Some(h))
            });
            if let Some(id) = found {
                // if a building was killed by enemy porc, cancel it.
                self.eco.drop_construction_task(id);
            }
        }
    }

    pub fn eco_unit_destroyed(&mut self, unit: Unit, attacker: Option<Unit>) {
        let _ = attacker;
        let def = unit.def();
        let uid = unit.id();
        if def.is_some_and(|d| self.defs.name(d) == crate::military::missiles::SILO) {
            self.eco_silo_lost(unit);
        }
        if def.is_some_and(|d| d.speed() == 0.0) {
            let eco = &mut self.eco;
            for list in [
                &mut eco.radars,
                &mut eco.porcs,
                &mut eco.airpads,
                &mut eco.fusions,
                &mut eco.mexes,
                &mut eco.solars,
                &mut eco.windgens,
                &mut eco.geos,
                &mut eco.pylons,
                &mut eco.aas,
                &mut eco.super_weps,
                &mut eco.storages,
                &mut eco.screamers,
                &mut eco.new_mexes,
                &mut eco.new_energy,
            ] {
                erase_one(list, &unit);
            }
            eco.dporcs.remove(&uid);
            eco.nanos.remove(&uid);

            for &id in &eco.construction_tasks {
                if let Some(c) = eco.tasks.get_mut(id).and_then(|t| t.as_construction_mut())
                    && c.target == Some(unit)
                {
                    // if a building was killed while under construction, reset its target.
                    c.target = None;
                    break;
                }
            }
        } else if self.eco.sparrow == Some(unit) && unit.rules_param_float("wasMorphedTo", 0.0) == 0.0 {
            self.eco.sparrow = None;
        }

        if matches!(self.defs.name_of(unit), "staticnuke" | "zenith" | "raveparty") {
            self.eco.has_super_wep = false;
        }

        // if the unit had a repair task targeting it, remove it
        let repair = self.eco.repair_tasks.iter().copied().find(
            |&id| matches!(self.eco.tasks.get(id).map(|t| &t.kind), Some(TaskKind::Repair(r)) if r.target == unit),
        );
        if let Some(id) = repair {
            let idle = self.eco.stop_workers(id);
            self.eco.idle_workers(&idle);
            self.eco.delete_task(id);
        }

        // port: with an unknown def, still drop any worker record for the unit.
        if def.is_none_or(|d| d.speed() > 0.0 && d.build_speed() > 0.0) {
            self.eco.new_workers.remove(&uid);
            if self.eco.workers.contains_key(&uid) {
                self.eco.remove_worker(uid);
            }
        }
    }

    pub fn eco_unit_captured(&mut self, unit: Unit, old_team: i32, new_team: i32) {
        let _ = new_team;
        if old_team == self.team_id {
            self.eco_unit_destroyed(unit, None);
        }
    }

    pub fn eco_unit_given(&mut self, unit: Unit, old_team: i32, new_team: i32) {
        let _ = old_team;
        if new_team != self.team_id {
            return;
        }
        let Some(def) = unit.def() else {
            return;
        };
        if def.speed() > 0.0 && !def.build_options().is_empty() {
            let mut w = Worker::new(unit, self.team_id);
            w.is_com = self.defs.is_commander(def);
            let id = w.id;
            self.eco.insert_worker(w);
            self.eco.new_workers.remove(&id);
            self.eco_generate_tasks(id);
            self.eco_assign_worker(id);
            self.eco.assigned.push_back(id);
            self.eco.seeders.push_back(id);
            if self.defs.is_commander(def) {
                self.eco.commanders.push(id);
            }
        }

        let def_name = self.defs.name(def).to_owned();
        let def_name = def_name.as_str();

        if unit.is_being_built() && !self.eco.has_repair_task(unit) {
            let t = WorkerTask::repair(unit, def_name, &self.unit_classes);
            let id = self.eco.tasks.insert(t);
            self.eco.repair_tasks.push(id);
        }

        if matches!(
            def_name,
            "turretmissile"
                | "turretlaser"
                | "turretheavylaser"
                | "turretemp"
                | "turretgauss"
                | "turretriot"
                | "turretaalaser"
        ) {
            self.eco.porcs.push(unit);
        }

        if matches!(def_name, "turretaalaser" | "turretaafar" | "turretaaflak") {
            self.eco.aas.push(unit);
        }

        match def_name {
            "staticcon" => {
                unit.self_destruct(0);
            }
            "staticmex" => self.eco.mexes.push(unit),
            "energysolar" => self.eco.solars.push(unit),
            "energywind" => self.eco.windgens.push(unit),
            "energygeo" | "energyheavygeo" if !unit.is_being_built() => self.eco.geos.push(unit),
            "energypylon" => self.eco.pylons.push(unit),
            "energyfusion" | "energysingu" => self.eco.fusions.push(unit),
            _ => {}
        }
    }

    pub fn eco_unit_idle(&mut self, unit: Unit) {
        let uid = unit.id();
        if self.eco.new_workers.contains(&uid) && unit.num_current_commands() == 0 {
            let mut w = Worker::new(unit, self.team_id);
            let n = self.eco.workers.len() + usize::from(!self.eco.workers.contains_key(&uid));
            let greed = self.eco.greed;
            let tf = self.graph.territory_fraction;
            let non_coms = n as f32 - self.eco.commanders.len() as f32;
            if n as i32 > 1 + self.merged_allies && (greed < 2.0 || greed < non_coms * (1.0 - tf).min(1.0)) {
                w.is_greedy = true;
            }
            let greedy = w.is_greedy;
            self.eco.insert_worker(w);
            if greedy {
                self.eco.greed += 1.0;
            }

            self.eco_generate_tasks(uid);
            self.eco_assign_worker(uid);
            self.eco.new_workers.remove(&uid);

            self.eco.assigned.push_back(uid);
            self.eco.seeders.push_back(uid);
        }
    }

    pub fn eco_enemy_enter_los(&mut self, enemy: Unit) {
        let Some(def) = enemy.def() else {
            return;
        };
        let pos = enemy.pos();
        // capture combat reclaim for enemy building nanoframes that enter los
        if enemy.is_being_built() && def.speed() == 0.0 && self.war.get_porc_threat(pos) == 0.0 {
            self.eco.add_combat_reclaim_if_new(enemy);
        }

        // porc over enemy mexes
        if self.defs.name(def) == "staticmex" && self.graph.is_ally_territory(pos) {
            self.eco_defend_mex(pos);
        }

        // check for enemy air
        if def.is_able_to_fly() && !matches!(self.defs.name(def), "dronelight" | "droneheavyslow" | "dronecarry") {
            self.eco.enemy_has_air = true;
        }
    }

    pub fn eco_enemy_created(&mut self, enemy: Unit) {
        let Some(def) = enemy.def() else {
            return;
        };
        let pos = enemy.pos();
        // capture combat reclaim for undefended enemy buildings that are started within los
        if def.speed() == 0.0 && self.war.get_threat(pos) == 0.0 {
            self.eco.add_combat_reclaim_if_new(enemy);
        }

        // porc over enemy mexes
        if self.defs.name(def) == "staticmex" && self.graph.is_ally_territory(pos) {
            self.eco_defend_mex(pos);
        }
    }

    /// Have a worker reclaim a friendly unit (e.g. an abandoned or stuck one).
    pub fn eco_add_combat_reclaim_task(&mut self, target: Unit) {
        let id = self.eco.tasks.insert(WorkerTask::combat_reclaim(target));
        self.eco.combat_reclaim_tasks.push(id);
    }

    /// Total metal value of reclaimable features worth collecting.
    pub fn eco_get_reclaim_value(&self) -> f32 {
        let eco = &self.eco;
        let last = eco.last_rc_value_frame.get();
        if last == 0 || self.frame - last > 300 {
            eco.last_rc_value_frame.set(self.frame);
            let total: f32 =
                self.cb.features().iter().filter_map(|f| f.def()).map(|d| d.contained_resource(self.metal)).sum();
            eco.reclaim_value.set(total);
        }
        // The cached value is rescaled on every call, as in the original.
        let v = eco.reclaim_value.get() * (1.0 + self.merged_allies as f32) / (1.0 + self.allies.len() as f32);
        eco.reclaim_value.set(v);
        v
    }

    pub fn eco_get_nearest_fac(&self, position: Vec3) -> Option<&Factory> {
        let mut nearest: Option<&Factory> = None;
        let mut dist = 0.0;
        for f in self.fac.factories.values() {
            let tdist = distance(position, f.get_pos());
            if nearest.is_none() || tdist < dist {
                dist = tdist;
                nearest = Some(f);
            }
        }
        nearest
    }

    pub fn eco_get_farthest_fac(&self, position: Vec3) -> Option<&Factory> {
        let mut farthest: Option<&Factory> = None;
        let mut dist = 0.0;
        for f in self.fac.factories.values() {
            let tdist = distance(position, f.get_pos());
            if farthest.is_none() || tdist > dist {
                dist = tdist;
                farthest = Some(f);
            }
        }
        farthest
    }

    pub fn eco_get_fusion_dist(&self, position: Vec3) -> f32 {
        let mut dist = 65535.0;
        for f in &self.eco.fusions {
            let tdist = distance(position, f.pos());
            if tdist < dist {
                dist = tdist;
            }
        }
        dist
    }

    fn eco_check_nanos(&mut self) {
        for n in self.eco.nanos.values() {
            if n.target.health() <= 0.0 {
                n.unit.self_destruct(0);
            }
        }
    }

    fn eco_set_eco_priorities(&mut self) {
        let eco = &self.eco;
        let team = self.team_id;
        let building = |u: &Unit| u.health() > 0.0 && u.team() == team && u.is_being_built();

        let prio =
            if !eco.hard_estalled && !self.graph.eminent_territory && eco.adjusted_income < 20.0 { 2.0 } else { 1.0 };
        for u in eco.new_mexes.iter().filter(|u| building(u)) {
            u.execute_custom_command(cmd::PRIORITY, &[prio], 0, NO_TIMEOUT);
        }

        let prio = if eco.hard_estalled { 2.0 } else { 1.0 };
        for u in eco.new_energy.iter().chain(&eco.fusions).filter(|u| building(u)) {
            u.execute_custom_command(cmd::PRIORITY, &[prio], 0, NO_TIMEOUT);
        }
    }
}

impl EconomyManager {
    /// Add a combat reclaim task for `target` unless one exists.
    fn add_combat_reclaim_if_new(&mut self, target: Unit) {
        let t = WorkerTask::combat_reclaim(target);
        if !self.tasks.contains(&self.combat_reclaim_tasks, &t) {
            let id = self.tasks.insert(t);
            self.combat_reclaim_tasks.push(id);
        }
    }
}
