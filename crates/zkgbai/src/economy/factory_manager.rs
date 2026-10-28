//! Factory production choices and army composition bookkeeping.

use std::collections::{BTreeMap, BTreeSet};

use springai::{Callback, Unit, UnitDef};

use crate::Ai;
use crate::economy::Factory;
use crate::economy::factory::FacKind;
use crate::kgbutil::kgb_move_to;
use crate::military::roles::Role;
use crate::military::scouts;
use crate::military::stakeouts::stakeout_unit;
use crate::util::kgb_rand;

/// Firewalker and Jugglenaut metal, and from how much enemy light
/// skirmisher / close-range army metal seen they are wanted.
const FIREWALKER_COST: f32 = 900.0;
const FIREWALKER_SKIRMS: f32 = 1200.0;
const JUGGLENAUT_COST: f32 = 1700.0;
const JUGGLENAUT_BRAWLERS: f32 = 3000.0;

/// Early Constables (see `fac_wants_early_constable`): for this long, at
/// least this many, one per this much metal income, at most this many.
const EARLY_CONS_FRAMES: i32 = 30 * 60 * 12;
const EARLY_CONS_MIN: usize = 2;
const EARLY_CONS_INCOME: f32 = 5.5;
const EARLY_CONS_MAX: usize = 14;

/// Puppies for the stakeouts in a jump opening (see
/// `fac_wants_puppy_scout`): this many first, then one per this many Pyros.
const PUPPY_SCOUTS_FIRST: i32 = 2;
const PYROS_PER_PUPPY_SCOUT: i32 = 2;

/// From when the spider factory raids with Venoms rather than Fleas
/// (frames: minute 2).
const VENOM_RAIDERS_FROM: i32 = 30 * 120;
/// Glaive-sized raiders a Venom counts as in a factory's raider budget
/// (about its cost over a Glaive's).
const VENOM_RAIDER_SLOTS: f32 = 3.0;

pub struct FactoryManager {
    cb: Callback,
    pub factories: BTreeMap<i32, Factory>,
    pub early_worker: bool,
    pub highprio: bool,
    pub num_workers: i32,
    pub mobile_bp: f32,
    pub worker_value: f32,
    pub fighter_value: f32,
    pub army_value: f32,
    pub light_arty_value: f32,
    pub heavy_arty_value: f32,
    pub arty_value: f32,
    pub num_funnels: i32,
    pub num_domis: i32,
    pub num_venoms: i32,
    pub num_redbacks: i32,
    pub num_recluses: i32,
    pub num_hermits: i32,
    pub num_ravens: i32,
    pub num_striders: i32,
    pub num_heavy_striders: i32,
    pub strider_target: Option<Unit>,

    /// Enemy aircraft (other than drones) have been seen.
    pub enemy_has_air: bool,
    /// Enemy drones have been seen.
    pub enemy_has_drones: bool,
    /// Enemy amphibious units have been seen.
    pub enemy_has_amphs: bool,
    /// Enemy heavy static defenses have been seen.
    pub enemy_has_heavy_porc: bool,

    /// Metal value of our assault units (including unfinished ones).
    pub assault_value: f32,
    /// Metal value of our mobile anti-air (including unfinished ones).
    pub aa_value: f32,

    // Unit counts by type (including unfinished units).
    pub num_aas: i32,
    pub num_erasers: i32,
    pub num_aspis: i32,
    pub num_warriors: i32,
    pub num_zeus: i32,
    pub num_rockos: i32,
    pub num_glaives: i32,
    pub num_scythes: i32,
    pub num_maces: i32,
    pub num_scalpels: i32,
    pub num_reapers: i32,
    pub num_banishers: i32,
    pub num_levelers: i32,
    pub num_ravagers: i32,
    pub num_scouts: i32,
    pub num_kodachis: i32,
    pub num_infis: i32,
    pub num_ultis: i32,
    pub num_felons: i32,
    pub num_rogues: i32,
    pub num_thugs: i32,
    pub num_laws: i32,
    pub num_rackets: i32,
    pub num_bandits: i32,
    pub num_pyros: i32,
    pub num_mods: i32,
    pub num_placeholders: i32,
    pub num_jacks: i32,
    pub num_jugglenauts: i32,
    pub num_firewalkers: i32,
    /// Puppies the factories made (the herd's own splits are not counted).
    pub num_puppies: i32,

    /// Mobile combat units still under construction.
    new_units: BTreeSet<i32>,
    big_map: bool,
    /// Per-order state, recomputed in `fac_assign_factory_task`.
    has_fusion: bool,
    scout_budget: i32,
    /// What a scout ordered now is for: a stakeout while they are short of
    /// one this factory can build, else a solo raider.
    scout_role: Role,
    /// Unit id of our strider hub, if any.
    strider_hub: Option<i32>,
    /// Unit defs found missing (each logged once).
    missing_defs: BTreeSet<&'static str>,
}

impl FactoryManager {
    pub fn new(cb: Callback) -> Self {
        let map = cb.map();
        let (w, h) = (map.width() as f32, map.height() as f32);
        let map_diag = (w * w + h * h).sqrt();
        let big_map = map_diag > 1080.0;
        FactoryManager {
            cb,
            factories: BTreeMap::new(),
            early_worker: big_map,
            highprio: false,
            num_workers: 0,
            mobile_bp: 0.0,
            worker_value: 0.0,
            fighter_value: 0.0,
            army_value: 0.0,
            light_arty_value: 0.0,
            heavy_arty_value: 0.0,
            arty_value: 0.0,
            num_funnels: 0,
            num_domis: 0,
            num_venoms: 0,
            num_redbacks: 0,
            num_recluses: 0,
            num_hermits: 0,
            num_ravens: 0,
            num_striders: 0,
            num_heavy_striders: 0,
            strider_target: None,
            enemy_has_air: false,
            enemy_has_drones: false,
            enemy_has_amphs: false,
            enemy_has_heavy_porc: false,
            assault_value: 0.0,
            aa_value: 0.0,
            num_aas: 0,
            num_erasers: 0,
            num_aspis: 0,
            num_warriors: 0,
            num_zeus: 0,
            num_rockos: 0,
            num_glaives: 0,
            num_scythes: 0,
            num_maces: 0,
            num_scalpels: 0,
            num_reapers: 0,
            num_banishers: 0,
            num_levelers: 0,
            num_ravagers: 0,
            num_scouts: 0,
            num_kodachis: 0,
            num_infis: 0,
            num_ultis: 0,
            num_felons: 0,
            num_rogues: 0,
            num_thugs: 0,
            num_laws: 0,
            num_rackets: 0,
            num_bandits: 0,
            num_pyros: 0,
            num_mods: 0,
            num_placeholders: 0,
            num_jacks: 0,
            num_jugglenauts: 0,
            num_firewalkers: 0,
            num_puppies: 0,
            new_units: BTreeSet::new(),
            big_map,
            has_fusion: false,
            scout_budget: 0,
            scout_role: Role::SoloRaider,
            strider_hub: None,
            missing_defs: BTreeSet::new(),
        }
    }

    /// Adjust the per-type unit counters for a unit of type `name` appearing
    /// (`delta` = 1) or disappearing (`delta` = -1).
    fn count_unit(&mut self, name: &str, delta: i32) {
        let counter = match name {
            "cloakriot" => &mut self.num_warriors,
            "cloakassault" => &mut self.num_zeus,
            "cloakskirm" => &mut self.num_rockos,
            "cloakraid" => &mut self.num_glaives,
            "cloakheavyraid" => &mut self.num_scythes,
            "cloakjammer" => &mut self.num_erasers,
            "shieldraid" => &mut self.num_bandits,
            "shieldfelon" => &mut self.num_felons,
            "shieldskirm" => &mut self.num_rogues,
            "shieldassault" => &mut self.num_thugs,
            "shieldriot" => &mut self.num_laws,
            "shieldarty" => &mut self.num_rackets,
            "shieldshield" => &mut self.num_aspis,
            "jumpraid" => &mut self.num_pyros,
            "jumpskirm" => &mut self.num_mods,
            "jumpblackhole" => &mut self.num_placeholders,
            "jumpassault" => &mut self.num_jacks,
            "jumpsumo" => &mut self.num_jugglenauts,
            "jumparty" => &mut self.num_firewalkers,
            "vehcapture" => &mut self.num_domis,
            "vehassault" => &mut self.num_ravagers,
            "vehriot" => &mut self.num_levelers,
            "vehscout" | "hoverraid" => &mut self.num_scouts,
            "spideremp" => &mut self.num_venoms,
            "spiderriot" => &mut self.num_redbacks,
            "spiderskirm" => &mut self.num_recluses,
            "spiderassault" => &mut self.num_hermits,
            "spiderantiheavy" => &mut self.num_infis,
            "hoverriot" => &mut self.num_maces,
            "hoverskirm" => &mut self.num_scalpels,
            "tankassault" => &mut self.num_reapers,
            "tankriot" => &mut self.num_banishers,
            "tankraid" => &mut self.num_kodachis,
            // port: the original lumps Phoenixes in with Ravens.
            "bomberprec" | "bomberriot" => &mut self.num_ravens,
            "striderantiheavy" => &mut self.num_ultis,
            "striderbantha" | "striderdetriment" => &mut self.num_heavy_striders,
            "striderdante" | "striderscorpion" | "striderarty" => &mut self.num_striders,
            "striderfunnelweb" => {
                self.num_funnels += delta;
                &mut self.num_striders
            }
            _ => return,
        };
        *counter += delta;
    }

    /// Adjust the arty and assault metal values for a unit of type `name`
    /// costing `cost` appearing (`sign` = 1) or disappearing (`sign` = -1).
    fn count_value(&mut self, name: &str, cost: f32, sign: f32) {
        match name {
            "tankheavyarty" | "hoverarty" | "spidercrabe" | "amphassault" | "striderarty" => {
                self.arty_value += sign * cost;
            }
            "shieldarty" | "tankarty" => self.arty_value += sign * cost / 2.0,
            "vehheavyarty" | "cloaksnipe" => {
                self.heavy_arty_value += sign * cost;
                self.arty_value += sign * cost;
            }
            "veharty" | "cloakarty" => {
                self.arty_value += sign * cost / 2.0;
                self.light_arty_value += sign * cost / 2.0;
            }
            _ => {}
        }
        if matches!(
            name,
            "hoverassault"
                | "cloakassault"
                | "vehassault"
                | "amphfloater"
                | "shieldassault"
                | "tankassault"
                | "jumpassault"
                | "jumpsumo"
        ) {
            self.assault_value += sign * cost;
        }
        if matches!(name, "energyfusion" | "energysingu") {
            self.fighter_value += sign * 250.0;
        }
    }
}

/// Factory module events.
impl Ai {
    pub fn fac_init(&mut self) {
        // Everything the original wired up here (manager pointers) is
        // reachable through `Ai`.
    }

    pub fn fac_update(&mut self, frame: i32) {
        if frame % 60 != self.offset % 60 {
            return;
        }
        let Some(hub_id) = self.fac.strider_hub else { return };
        match self.fac.strider_target {
            None => self.fac_assign_factory_task(hub_id),
            Some(target) => {
                if let Some(hub) = self.fac.factories.get(&hub_id) {
                    hub.unit.repair(target, 0);
                }
            }
        }
    }

    pub fn fac_unit_created(&mut self, unit: Unit, builder: Option<Unit>) {
        let Some(def) = unit.def() else { return };
        let name = self.defs.name(def);
        let cost = def.cost(self.metal);
        let is_com = self.defs.is_commander(def);
        let fm = &mut self.fac;

        if name.contains("hub") {
            unit.set_move_state(0, 0);
        }

        if name.contains("factory") {
            // Set default rally point.
            let mut target = unit.pos();
            let rallydist = 175.0;
            match unit.building_facing() {
                0 => target.z += rallydist,
                1 => target.x += rallydist,
                2 => target.z -= rallydist,
                3 => target.x -= rallydist,
                _ => {}
            }
            kgb_move_to(unit, target, 0, self.frame);
        }

        let (build_speed, speed) = (def.build_speed(), def.speed());
        let is_aa = self.unit_classes.aas.contains(name);
        if build_speed > 0.0 && speed > 0.0 {
            fm.num_workers += 1;
            if !is_com {
                fm.worker_value += cost;
            }
            fm.mobile_bp += build_speed;
        } else if speed > 0.0 && !def.is_able_to_fly() && !is_aa {
            fm.new_units.insert(unit.id());
            fm.army_value += cost / 2.0;
        } else if is_aa {
            fm.new_units.insert(unit.id());
            fm.num_aas += 1;
            fm.aa_value += cost;
            fm.army_value += cost / 2.0;
        }

        let role = builder.and_then(|b| fm.factories.get(&b.id())).and_then(|f| f.building_for);
        if name == crate::military::puppies::PUPPY {
            if builder.is_some() {
                fm.num_puppies += 1;
            } else {
                self.puppies.born += 1;
            }
        }

        fm.count_value(name, cost, 1.0);
        fm.count_unit(name, 1);
        if matches!(
            name,
            "striderdante"
                | "striderscorpion"
                | "striderarty"
                | "striderfunnelweb"
                | "striderantiheavy"
                | "striderbantha"
                | "striderdetriment"
        ) {
            fm.strider_target = Some(unit);
        }

        // It joins its role's handler when finished (`war_unit_finished`).
        if let Some(role) = role {
            self.role_declare(unit.id(), role);
        }
    }

    pub fn fac_unit_finished(&mut self, unit: Unit) {
        if self.fac.strider_target.is_some_and(|t| t.id() == unit.id()) {
            self.fac.strider_target = None;
        }

        let Some(def) = unit.def() else { return };
        let name = self.defs.name(def);
        if self.fac.new_units.remove(&unit.id()) {
            let cost = def.cost(self.metal);
            if !self.unit_classes.aas.contains(name) {
                self.fac.fighter_value += cost;
            }
            self.fac.army_value += cost / 2.0;
        }

        if name.contains("factory") || name.contains("hub") {
            self.log(&format!("economy: {name} finished"));
            let first_fac = (self.fac.factories.len() as i32) < 1 + self.merged_allies;
            let mut fac = Factory::new(unit, name, first_fac, self.map_diag);
            if !name.contains("hub") {
                // Worker cost-effectiveness, from the first constructor this factory can build.
                if let Some(con) = def.build_options().into_iter().find(|ud| !ud.build_options().is_empty()) {
                    fac.cost_per_bp = con.cost(self.metal) / con.build_speed();
                    fac.worker_bp = con.build_speed();
                }
            }
            let is_hub = fac.kind() == FacKind::StriderHub;
            let id = fac.id;
            self.fac.factories.insert(id, fac);

            self.fac_assign_factory_task(id);
            unit.set_move_state(2, 0);

            if is_hub {
                self.fac.strider_hub = Some(id);
            }
        }
    }

    pub fn fac_unit_destroyed(&mut self, unit: Unit, attacker: Option<Unit>) {
        let _ = attacker;
        let id = unit.id();
        if self.fac.strider_target.is_some_and(|t| t.id() == id) {
            self.fac.strider_target = None;
        }

        if let Some(def) = unit.def() {
            let name = self.defs.name(def);
            let cost = def.cost(self.metal);
            let is_aa = self.unit_classes.aas.contains(name);
            let is_com = self.defs.is_commander(def);
            let fm = &mut self.fac;

            if name == "striderhub" {
                fm.strider_hub = None;
            }

            if def.is_builder() && def.speed() > 0.0 {
                fm.num_workers -= 1;
                if !is_com {
                    fm.worker_value -= cost;
                }
                fm.mobile_bp -= def.build_speed();
            } else if fm.new_units.remove(&id) {
                fm.army_value -= cost / 2.0;
                if is_aa {
                    // port: the original leaked these for AA killed while under construction.
                    fm.num_aas -= 1;
                    fm.aa_value -= cost;
                }
            } else if def.speed() > 0.0 && !def.is_able_to_fly() && !is_aa {
                fm.fighter_value -= cost;
                fm.army_value -= cost;
            } else if is_aa {
                fm.num_aas -= 1;
                fm.aa_value -= cost;
                fm.army_value -= cost;
            }

            fm.count_value(name, cost, -1.0);
            fm.count_unit(name, -1);
            if name == crate::military::puppies::PUPPY && fm.num_puppies > 0 {
                fm.num_puppies -= 1;
            }
        }

        if self.fac.strider_hub == Some(id) {
            self.fac.strider_hub = None;
        }
        self.fac.factories.remove(&id);
    }

    pub fn fac_unit_captured(&mut self, unit: Unit, old_team: i32, new_team: i32) {
        let _ = new_team;
        if old_team == self.team_id {
            self.fac_unit_destroyed(unit, None);
        }
    }

    pub fn fac_unit_given(&mut self, unit: Unit, old_team: i32, new_team: i32) {
        let _ = old_team;
        if new_team == self.team_id {
            if self.defs.name_of(unit) == "striderhub" {
                unit.self_destruct(0);
            } else {
                self.fac_unit_created(unit, None);
            }
        }
    }

    pub fn fac_unit_idle(&mut self, unit: Unit) {
        let id = unit.id();
        if !self.fac.factories.contains_key(&id) {
            return;
        }
        let name = self.defs.name_of(unit);
        if name != "striderhub" && name != "athena" {
            self.fac_assign_factory_task(id);
        }
    }

    pub fn fac_enemy_enter_los(&mut self, enemy: Unit) {
        let Some(def) = enemy.def() else { return };
        let name = self.defs.name(def);
        let is_drone = matches!(name, "dronelight" | "droneheavyslow" | "dronecarry");
        if def.is_able_to_fly() && !is_drone {
            // TODO (from the C++ port): also ignore sparrows (and athenas?).
            self.fac.enemy_has_air = true;
        } else if is_drone {
            self.fac.enemy_has_drones = true;
        }

        if !self.fac.enemy_has_amphs && def.tooltip().contains("Amph") {
            self.fac.enemy_has_amphs = true;
        }

        if matches!(
            name,
            "turretheavylaser" | "turretriot" | "turretgauss" | "turretheavy" | "turretantiheavy" | "spidercrabe"
        ) {
            self.fac.enemy_has_heavy_porc = true;
        }
    }
}

/// Production choices.
///
/// The `fac_get_*` choosers only read the AI state and mutate the factory
/// passed in, which is a copy written back by `fac_assign_factory_task`.
impl Ai {
    /// Queue the next unit for factory (or strider hub) `fac_id`.
    fn fac_assign_factory_task(&mut self, fac_id: i32) {
        // A staged strike builds its Widows itself (see `strike_test`); the
        // `hijack` and `impalerwar` tests play the economy as usual.
        // The staged raid game builds only firefighters (see `raid_test`).
        if self.test.as_deref() == Some("qrf") {
            self.fac_build_firefighter(fac_id);
            return;
        }
        if self.test.as_deref().is_some_and(|t| t != "hijack" && t != "impalerwar") {
            return;
        }
        let Some(f) = self.fac.factories.get_mut(&fac_id) else { return };
        // (Stakeouts are built for what the stakeouts are short of, not out
        // of the factory's scout allowance.)
        if f.building_for.take() == Some(Role::SoloRaider) {
            f.scout_allowance -= 1;
        }
        let Some(f) = self.fac.factories.get(&fac_id) else { return };
        let mut fac = f.clone();

        let mut scouts_needed: i32 = self.fac.factories.values().map(|f| f.max_scout_allowance).sum();
        if fac.kind() == FacKind::Tank {
            scouts_needed -= self.raider_handler.kodachis.len() as i32;
        } else {
            scouts_needed -= self.raider_handler.solo_raiders.len() as i32;
            scouts_needed -= self.stakeouts.count() as i32;
        }
        // Where the scout is a lane picket (Fleas, Darts; see `stakeouts`),
        // the stakeouts say how many: a post unmanned is a raid unseen, and
        // a picket more than they want is metal idle. Other factories build
        // their cheapest fast unit for the stakeouts while they are short
        // (feature `anyscout`), then scouts for raiding alone as before.
        let short = self.stakeouts_short(fac.kind());
        self.fac.scout_budget = if self.stakeouts_own(fac.kind()) || short > 0 {
            short
        } else {
            scouts_needed.min(fac.scout_allowance).max(0)
        };
        self.fac.scout_role = if short > 0 { Role::Stakeout } else { Role::SoloRaider };

        self.fac.has_fusion = self.eco.fusions.first().is_some_and(|fusion| {
            !fusion.is_being_built() || fusion.def().is_some_and(|d| fusion.health() > d.health() / 2.0)
        });

        let kind = fac.kind();
        let choice = match kind {
            FacKind::Cloak => self.fac_get_cloaky(&mut fac),
            FacKind::Ship => self.fac_get_boats(&mut fac),
            FacKind::Shield => self.fac_get_shields(&mut fac),
            FacKind::Gunship => self.fac_get_gunship(&mut fac),
            FacKind::Plane => self.fac_get_planes(&mut fac),
            FacKind::Amph => self.fac_get_amphs(&mut fac),
            FacKind::Veh => self.fac_get_lv(&mut fac),
            FacKind::Hover => self.fac_get_hovers(&mut fac),
            FacKind::Tank => self.fac_get_tanks(&mut fac),
            FacKind::Spider => self.fac_get_spiders(&mut fac),
            FacKind::Jump => self.fac_get_jumps(&mut fac),
            FacKind::StriderHub => self.fac_get_strider(),
            FacKind::Other => return,
        };
        // The operator's production orders replace the AI's own pick, but
        // never a constructor it picked: an operator steering production
        // must not move the economy off its builder curve.
        let ai_wants_builder = self.defs.get(choice).is_some_and(|d| d.speed() > 0.0 && !d.build_options().is_empty());
        // Raiders for home defense while its budget is short (see `qrf`),
        // in place of a plain fighter. (Not of a jump opening's Puppy
        // scouts: the QRF floor took the second for a Pyro guard.)
        let special = matches!(choice, "amphtele" | "gunshiptrans" | "striderantiheavy")
            || choice == scouts::WIDOW
            || choice == scouts::SCYTHE
            || self.firefighters_order(kind) == Some(choice)
            || (choice == crate::military::puppies::PUPPY && fac.building_for == Some(Role::Stakeout));
        // Dirtbags for the strikes while snipers are about (see `strikes`).
        let picked = choice;
        let choice = match (ai_wants_builder || special, fac.unit) {
            (false, unit) => {
                (self.strikes_production(unit, choice)).or_else(|| self.qrf_production(unit, choice)).unwrap_or(choice)
            }
            _ => choice,
        };
        // A unit other than the one picked is not for the pick's role.
        if choice != picked {
            fac.building_for = None;
        }
        if !ai_wants_builder && let Some(def) = self.op_factory_order(fac_id, false) {
            fac.building_for = None;
            self.fac.factories.insert(fac_id, fac);
            self.fac_build_ordered(fac_id, def);
            return;
        }
        // Now and then, in place of a fighter, a unit a taskforce is short
        // of (a role with `produce`; see `taskforce`).
        let options = fac.unit.def().map(|d| d.build_options()).unwrap_or_default();
        if !ai_wants_builder
            && !special
            && let Some((def, doctrine, role)) = self.squad_produce(&options)
        {
            let name = self.defs.name(def).to_owned();
            self.log(&format!("taskforce: a {name} for a {doctrine} force short of its {role}"));
            self.taskforce_note_produced(&doctrine, &role);
            fac.building_for = None;
            self.fac.factories.insert(fac_id, fac);
            self.fac_build_ordered(fac_id, def);
            return;
        }
        let (unit, pos) = (fac.unit, fac.get_pos());
        self.fac.factories.insert(fac_id, fac);
        match choice {
            "amphtele" => self.teleport_ordered(),
            "gunshiptrans" => self.drops_ordered(),
            scouts::WIDOW if self.scouts_want_widow() => self.scouts_ordered(),
            scouts::WIDOW | "striderantiheavy" => self.jaeger_ordered(choice),
            // (Then asked for by the scouts: the factory builds no other
            // Scythes while they want one.)
            scouts::SCYTHE if self.scouts_want_scythe() => self.scouts_ordered_scythe(),
            n if self.firefighters_order(kind) == Some(n) => self.firefighters_ordered(n),
            _ => {}
        }

        if kind == FacKind::StriderHub {
            unit.stop(0);
        }
        if let Some(con) = self.defs.get(choice).filter(|d| !d.build_options().is_empty())
            && let Some(n) = self.eco_wants_stranded_con(con, pos)
        {
            self.log(&format!("economy: {choice} for {n} metal spots only it gets to"));
        }
        let Some(def) = self.defs.get(choice) else {
            if self.fac.missing_defs.insert(choice) {
                self.log(&format!("FactoryManager: unknown unit def '{choice}', not building it."));
            }
            return;
        };
        if kind == FacKind::StriderHub {
            let site = self.fac.cb.map().find_closest_build_site(def, pos, 250.0, 3, 0);
            unit.build(def, site, 0, 0);
        } else {
            unit.build(def, pos, 0, 0);
        }
    }

    /// Build the firefighter wanted from this factory, if any.
    pub(crate) fn fac_build_firefighter(&mut self, fac_id: i32) {
        let Some(f) = self.fac.factories.get(&fac_id) else { return };
        let (unit, pos, kind) = (f.unit, f.get_pos(), f.kind());
        let Some(name) = self.firefighters_order(kind) else { return };
        let Some(def) = self.defs.get(name) else { return };
        self.firefighters_ordered(name);
        unit.build(def, pos, 0, 0);
    }

    /// Build a unit the operator ordered, telling the modules that keep
    /// count of what they order.
    fn fac_build_ordered(&mut self, fac_id: i32, def: UnitDef) {
        let Some(f) = self.fac.factories.get(&fac_id) else { return };
        let (unit, pos, kind) = (f.unit, f.get_pos(), f.kind());
        match self.defs.name(def) {
            "amphtele" => self.teleport_ordered(),
            "gunshiptrans" => self.drops_ordered(),
            scouts::WIDOW => self.scouts_ordered(),
            _ => {}
        }
        if kind == FacKind::StriderHub {
            unit.stop(0);
            let site = self.fac.cb.map().find_closest_build_site(def, pos, 250.0, 3, 0);
            unit.build(def, site, 0, 0);
        } else {
            unit.build(def, pos, 0, 0);
        }
    }

    /// Whether we have enough army to afford mobile AA, rolled against how
    /// much AA we already have relative to the biggest enemy air force seen.
    fn fac_aa_roll(&self) -> bool {
        let fm = &self.fac;
        if fm.fighter_value > fm.aa_value {
            let ratio = fm.aa_value / self.war.max_enemy_air_value;
            kgb_rand() > f64::from(ratio * ratio)
        } else {
            false
        }
    }

    fn fac_need_workers(&self, fac: &Factory) -> bool {
        let fm = &self.fac;
        let eco = &self.eco;
        let merged = self.merged_allies;

        // Constructors for metal only they get to (aircraft for the sea).
        let con = fac.unit.def().and_then(|d| d.build_options().into_iter().find(|u| !u.build_options().is_empty()));
        if !self.dev_off("reach")
            && let Some(con) = con
            && self.eco_wants_stranded_con(con, fac.get_pos()).is_some()
        {
            return true;
        }

        // At most one worker per merged team from air factories.
        if matches!(fac.kind(), FacKind::Plane | FacKind::Gunship) && fm.num_workers > merged {
            return false;
        }

        if merged > 0 && (fac.raider_spam < 0.0 || fac.expensive_raider_spam < 0.0) {
            return false;
        }

        let reclaim_value = self.eco_get_reclaim_value();

        // port: guarded against a zero cost-per-bp (no constructor found).
        let reclaim_divisor = (2.0 + 20.0 * self.graph.territory_fraction) * fac.cost_per_bp;
        let reclaim_income = if reclaim_divisor > 0.0 { reclaim_value / reclaim_divisor } else { 0.0 };
        let income = eco.effective_income_metal + reclaim_income;
        // port: Java's Math.round (half up) rather than std::round; `as`
        // saturates like Java for infinities and maps NaN to 0.
        // The AI's own rule keeps mobile build power at income, one to one;
        // the `buildpower` dial lets it lead (a strong human runs 1.3-1.7).
        let worker_deficit = ((self.op_buildpower() * income - fm.mobile_bp) / fac.worker_bp + 0.5).floor() as i32;

        if ((worker_deficit > 0 && fm.fighter_value > fm.worker_value) || fm.num_workers == 0)
            || (eco.adjusted_income > 15.0
                && eco.metal / eco.max_storage > 0.8
                && eco.max_storage > 0.5
                && worker_deficit > -1)
            || (fm.early_worker && fm.num_workers < 2 * (1 + merged))
        {
            return true;
        }
        if fac.raider_spam >= 0.0
            && fm.scout_budget == 0
            && fac.expensive_raider_spam >= 0.0
            && fm.num_workers < 2 * (1 + merged)
        {
            return true;
        }
        merged == 0 && eco.effective_income >= 15.0 && fm.num_workers < 3
    }

    fn fac_get_cloaky(&self, fac: &mut Factory) -> &'static str {
        let fm = &self.fac;
        let war = &self.war;
        let tf = self.graph.territory_fraction;
        let income = self.eco.adjusted_income;

        if self.fac_need_workers(fac) {
            return "cloakcon";
        }

        if war.slasher_spam as f32 * 280.0 > fm.assault_value {
            return "cloakassault";
        }

        if self.scouts_want_scythe() {
            return scouts::SCYTHE;
        }

        if fac.raider_spam < 0.0 || fm.scout_budget > 0 {
            if fm.scout_budget == 0 {
                fac.raider_spam += 1.0;
            } else {
                fac.building_for = Some(fm.scout_role);
            }
            return "cloakraid";
        }

        if fac.expensive_raider_spam < 0.0 {
            fac.expensive_raider_spam += 1.0;
            return "cloakheavyraid";
        }

        if self.fac_aa_roll() {
            return "cloakaa";
        }

        // Talas (C++ port): bumped required fighter value up from 2000 and squad count up from >1.
        if fm.has_fusion
            && fm.num_erasers == 0
            && fm.fighter_value > 9000.0
            && self.squad_handler.squads.len() > 2
            && kgb_rand() > 0.75
        {
            return "cloakjammer";
        }

        if income > 15.0 && kgb_rand() > 0.95 - f64::from(tf / 20.0) {
            return "cloakbomb";
        }

        if fm.has_fusion
            && kgb_rand() < f64::from((tf / 4.0) * (1.0 - fm.heavy_arty_value / war.enemy_heavy_porc_value))
        {
            fac.add_scout_allowance(2);
            return "cloaksnipe";
        }

        if income > 20.0 && kgb_rand() < f64::from((tf / 5.0) * (1.0 - fm.light_arty_value / war.enemy_porc_value)) {
            fac.add_scout_allowance(1);
            return "cloakarty";
        }

        let raid_threshold =
            if war.sniper_sightings.is_empty() { 0.95 + f64::from(0.025f32.min(tf / 20.0)) } else { 0.85 };
        if !fm.highprio && kgb_rand() > raid_threshold {
            fac.raider_spam -= (((income / 5.0).floor() as i32).clamp(6, 12) - 1) as f32;
            return "cloakraid";
        }
        if income > 15.0 && kgb_rand() > 0.925 + f64::from(0.05f32.min(tf / 10.0)) {
            fac.expensive_raider_spam -= (((income / 10.0).floor() as i32).clamp(2, 4) - 1) as f32;
            return "cloakheavyraid";
        }

        fac.raider_spam -= 1.0;
        let rnd = kgb_rand();
        if fm.num_rockos < (2 * fm.num_warriors) + (3 * fm.num_zeus) {
            "cloakskirm"
        } else if rnd > 0.6 {
            fac.add_scout_allowance(1);
            "cloakassault"
        } else if !fm.has_fusion || rnd > 0.15 {
            fac.add_scout_allowance(1);
            "cloakriot"
        } else {
            fac.add_scout_allowance(2);
            "cloaksnipe"
        }
    }

    fn fac_get_boats(&self, fac: &mut Factory) -> &'static str {
        let fm = &self.fac;
        let war = &self.war;
        let tf = self.graph.territory_fraction;
        let income = self.eco.adjusted_income;

        if self.fac_need_workers(fac) {
            return "shipcon";
        }

        if fm.scout_budget > 0 {
            fac.building_for = Some(fm.scout_role);
            return "shiptorpraider";
        }

        if !fac.small_raider_spam {
            fac.small_raider_spam = true;
            fac.expensive_raider_spam = -5.0;
        }

        if fac.expensive_raider_spam < 0.0 {
            fac.expensive_raider_spam += 1.0;
            return "subraider";
        }

        fac.add_scout_allowance(1);

        if self.fac_aa_roll() {
            return "shipaa";
        }

        if income > 25.0
            && war.enemy_heavy_porc_value > 0.0
            && kgb_rand() < f64::from(tf * (1.0 - fm.arty_value / war.enemy_porc_value))
        {
            fac.expensive_raider_spam -= 1.0;
            return "shiparty";
        }

        if kgb_rand() > 0.9 {
            let divisor = if fm.big_map { 2.5 } else { 5.0 };
            fac.expensive_raider_spam -= (((income / divisor).floor() as i32).clamp(2, 8) + 1) as f32;
        }

        let rnd = kgb_rand();
        if income < 35.0 && !self.graph.eminent_territory {
            fac.expensive_raider_spam -= 2.0;
            if fm.army_value < 1000.0 || rnd < 0.3 {
                fac.expensive_raider_spam -= 1.0;
                return "shipassault";
            }
            if rnd < 0.2 { "shipriot" } else { "shipskirm" }
        } else {
            fac.expensive_raider_spam -= 1.0;
            if fm.army_value < 2000.0 || rnd < 0.3 {
                fac.expensive_raider_spam -= 2.0;
                fac.add_scout_allowance(1);
                return "shipassault";
            }
            if rnd < 0.1 {
                "shipriot"
            } else if rnd < 0.2 {
                fac.expensive_raider_spam -= 1.0;
                "shiparty"
            } else {
                "shipskirm"
            }
        }
    }

    fn fac_get_shields(&self, fac: &mut Factory) -> &'static str {
        let fm = &self.fac;
        let war = &self.war;
        let tf = self.graph.territory_fraction;
        let income = self.eco.adjusted_income;

        if self.fac_need_workers(fac) {
            return "shieldcon";
        }

        let slasher_value = war.slasher_spam as f32 * 140.0;
        if slasher_value > fm.assault_value && slasher_value > war.enemy_porc_value {
            return "shieldassault";
        }

        if fac.raider_spam < 0.0 || fm.scout_budget > 0 {
            if fm.scout_budget == 0 {
                fac.raider_spam += 1.0;
            } else {
                fac.building_for = Some(fm.scout_role);
                // The shield factory's picket is its scout, the Dirtbag.
                if fm.scout_role == Role::Stakeout
                    && let Some(scout) = stakeout_unit(FacKind::Shield)
                {
                    return scout;
                }
            }
            if fac.small_raider_spam && fm.scout_budget == 0 {
                return "shieldassault";
            }
            return "shieldraid";
        }

        if fac.expensive_raider_spam < 0.0 {
            fac.expensive_raider_spam += 1.0;
            if fac.small_raider_spam {
                return "shieldriot";
            }
            return "shieldskirm";
        }

        fac.small_raider_spam = false;

        if self.fac_aa_roll() {
            return "shieldaa";
        }

        fac.raider_spam -= 1.0;
        if kgb_rand() > f64::from(tf) {
            fac.raider_spam -= 1.0;
        }
        if kgb_rand() > 0.5 {
            fac.expensive_raider_spam -= 2.0;
        }
        fac.add_scout_allowance(1);

        if fm.num_felons > 0
            && (war.enemy_heavy_value.ln().round() > fm.num_rackets as f32
                || kgb_rand() < f64::from(tf * (1.0 - (fm.num_rackets as f32 * 350.0) / war.enemy_heavy_porc_value)))
        {
            return "shieldarty";
        }

        if fm.has_fusion
            && fm.num_felons > 0
            && ((fm.num_aspis < fm.num_felons && kgb_rand() > 0.5)
                || (fm.num_aspis >= fm.num_felons && fm.num_aspis < 2 * fm.num_felons && kgb_rand() > 0.8))
        {
            fac.add_scout_allowance(2);
            fac.raider_spam -= 4.0;
            fac.expensive_raider_spam -= 2.0;
            return "shieldshield";
        }

        if income > 20.0 && fm.num_felons < ((income / 20.0).floor() as i32).min(4) && kgb_rand() > 0.5 {
            fac.small_raider_spam = true;
            fac.add_scout_allowance(2);
            fac.raider_spam -= 4.0;
            fac.expensive_raider_spam -= 1.0;
            return "shieldfelon";
        }

        if income > 20.0 && kgb_rand() > 0.95 - f64::from(0.05f32.min(tf / 10.0)) {
            return "shieldbomb";
        }

        let raid_threshold =
            if war.sniper_sightings.is_empty() { 0.95 + f64::from(0.025f32.min(tf / 20.0)) } else { 0.85 };
        if !fm.highprio && kgb_rand() > raid_threshold {
            fac.raider_spam -= ((income / 5.0).floor() as i32).clamp(6, 12) as f32;
        }

        if fm.num_laws * 3 > fm.num_thugs {
            "shieldassault"
        } else if income > 20.0 && fm.num_rackets < fm.num_laws {
            "shieldarty"
        } else {
            "shieldriot"
        }
    }

    fn fac_get_amphs(&self, fac: &mut Factory) -> &'static str {
        let fm = &self.fac;
        let war = &self.war;
        let tf = self.graph.territory_fraction;
        let income = self.eco.adjusted_income;

        if self.fac_need_workers(fac) {
            return "amphcon";
        }

        if self.teleport_wants_djinn() {
            return "amphtele";
        }

        if war.slasher_spam as f32 * 140.0 > fm.assault_value {
            return "amphfloater";
        }

        if fac.raider_spam < 0.0 || fm.scout_budget > 0 {
            if fm.scout_budget == 0 {
                fac.raider_spam += 1.0;
            } else {
                fac.building_for = Some(fm.scout_role);
            }
            return "amphraid";
        }

        if self.fac_aa_roll() {
            return "amphaa";
        }

        if income > 30.0
            && fm.fighter_value - fm.arty_value > 1500.0
            && kgb_rand() < f64::from((tf / 2.0) * (1.0 - fm.arty_value / war.enemy_porc_value))
        {
            fac.add_scout_allowance(3);
            return "amphassault";
        }

        if income > 20.0 && kgb_rand() > 0.9 {
            return "amphbomb";
        }

        fac.raider_spam -= 1.0;
        if kgb_rand() > 0.5 {
            fac.add_scout_allowance(1);
        }

        let raid_threshold =
            if war.sniper_sightings.is_empty() { 0.9 + f64::from(0.05f32.min(tf / 10.0)) } else { 0.85 };
        if !fm.highprio && kgb_rand() > raid_threshold {
            fac.raider_spam -= ((income / 5.0).floor() as i32).clamp(6, 12) as f32;
        }

        if income > 35.0 && kgb_rand() > 0.9 && fm.fighter_value - fm.arty_value > 2000.0 {
            fac.add_scout_allowance(2);
            return "amphassault";
        }
        let rnd = kgb_rand();
        if rnd > 0.3 {
            "amphfloater"
        } else if kgb_rand() > 0.85 {
            "amphriot"
        } else {
            "amphimpulse"
        }
    }

    fn fac_get_lv(&self, fac: &mut Factory) -> &'static str {
        let fm = &self.fac;
        let war = &self.war;
        let tf = self.graph.territory_fraction;
        let eminent = self.graph.eminent_territory;
        let income = self.eco.adjusted_income;

        if self.fac_need_workers(fac) {
            return "vehcon";
        }

        // Screens for Impaler batteries short of them (see `impalers`).
        if kgb_rand() < f64::from(self.impalers_escort_produce("vehriot")) {
            return "vehriot";
        }

        let slasher_value = war.slasher_spam as f32 * 140.0;
        if slasher_value > fm.assault_value && slasher_value > war.enemy_porc_value {
            return "vehassault";
        }

        if fm.scout_budget > 0 {
            fac.building_for = Some(fm.scout_role);
            return "vehscout";
        }

        if fac.raider_spam < 0.0 {
            fac.raider_spam += 1.0;
            return "vehraid";
        }

        if fac.expensive_raider_spam < 0.0 {
            fac.expensive_raider_spam += 1.0;
            return "vehsupport";
        }

        fac.add_scout_allowance(1);
        if kgb_rand() > f64::from(tf) {
            fac.add_scout_allowance(1);
        }

        if eminent && kgb_rand() < f64::from((tf / 10.0) * (1.0 - fm.heavy_arty_value / war.enemy_heavy_porc_value)) {
            fac.add_scout_allowance(1);
            return "vehheavyarty";
        }

        if income > 20.0
            && kgb_rand() < f64::from(1.0f32.min(2.0 * tf) * (1.0 - fm.light_arty_value / war.enemy_porc_value))
        {
            return "veharty";
        }

        // Dominatrices for shield balls: with only the felon doctrine on (the
        // seeds), a taskforce is a shield ball, as this read before doctrines.
        if (fm.num_aspis > 0 && !self.squad_handler.taskforces.is_empty()) && fm.num_domis < 8 && kgb_rand() > 0.5 {
            return "vehcapture";
        }

        if self.fac_aa_roll() {
            return "vehaa";
        }

        let raid_threshold =
            if war.sniper_sightings.is_empty() { 0.95 + f64::from(0.025f32.min(tf / 20.0)) } else { 0.85 };
        if kgb_rand() > raid_threshold {
            fac.raider_spam -= (((income / 4.0).floor() as i32).clamp(4, 8) + 1) as f32;
            fac.add_scout_allowance(1);
        }

        if kgb_rand() > 0.9 + f64::from(0.05f32.min(tf / 10.0)) {
            fac.expensive_raider_spam -= (4 + 2i32.min((income / 25.0).floor() as i32)) as f32;
        }

        if kgb_rand() > f64::from((tf + (tf * tf)) / 2.0) {
            fac.raider_spam -= 1.0;
        }
        let rnd = kgb_rand();
        if eminent && rnd <= 0.2 {
            "veharty"
        } else if fm.num_ravagers > fm.num_levelers * 3 || fm.num_levelers == 0 {
            "vehriot"
        } else {
            "vehassault"
        }
    }

    fn fac_get_hovers(&self, fac: &mut Factory) -> &'static str {
        let fm = &self.fac;
        let war = &self.war;
        let tf = self.graph.territory_fraction;
        let income = self.eco.adjusted_income;

        if self.fac_need_workers(fac) {
            return "hovercon";
        }

        let slasher_value = war.slasher_spam as f32 * 140.0;
        if slasher_value > fm.assault_value && slasher_value > war.enemy_porc_value {
            return "hoverassault";
        }

        if fac.raider_spam < 0.0 || fm.scout_budget > 0 {
            if fm.scout_budget == 0 {
                fac.raider_spam += 1.0;
            } else {
                fac.building_for = Some(fm.scout_role);
            }
            return "hoverraid";
        }

        if fac.expensive_raider_spam < 0.0 {
            fac.expensive_raider_spam += 1.0;
            return "hoverassault";
        }

        if income > 30.0
            && fm.army_value - fm.arty_value > 2000.0
            && kgb_rand() < f64::from(tf * (1.0 - fm.arty_value / war.enemy_porc_value))
        {
            fac.add_scout_allowance(2);
            return "hoverarty";
        }

        if self.fac_aa_roll() {
            return "hoveraa";
        }

        let raid_threshold =
            if war.sniper_sightings.is_empty() { 0.9 + f64::from(0.05f32.min(tf / 10.0)) } else { 0.85 };
        if !fm.highprio && kgb_rand() > raid_threshold {
            fac.raider_spam -= ((income / 5.0).floor() as i32).clamp(4, 8) as f32;
        }

        if kgb_rand() < f64::from(tf) {
            fac.expensive_raider_spam -= 1.0;
        } else {
            fac.raider_spam -= 1.0;
        }
        if kgb_rand() > 0.8 {
            fac.add_scout_allowance(1);
            let rnd = kgb_rand();
            let depthcharge_chance = if self.eco.ships_viable { 0.5 } else { 0.7 };
            if rnd > depthcharge_chance {
                return "hoverdepthcharge";
            }
            return "hoverriot";
        }

        if self.graph.eminent_territory && kgb_rand() > 0.9 {
            fac.add_scout_allowance(2);
            return "hoverarty";
        }
        if kgb_rand() > 0.5 {
            fac.add_scout_allowance(1);
        }
        "hoverskirm"
    }

    fn fac_get_tanks(&self, fac: &mut Factory) -> &'static str {
        let fm = &self.fac;
        let war = &self.war;
        let tf = self.graph.territory_fraction;
        let income = self.eco.adjusted_income;

        if self.fac_need_workers(fac) {
            return "tankcon";
        }

        if war.slasher_spam as f32 * 140.0 > war.enemy_porc_value || war.slasher_spam as f32 * 560.0 > fm.assault_value
        {
            return "tankassault";
        }

        if fm.scout_budget > 0 {
            fac.building_for = Some(fm.scout_role);
            return "tankraid";
        }

        if !fac.small_raider_spam {
            fac.small_raider_spam = true;
            fac.expensive_raider_spam = -5.0;
        }

        if fac.expensive_raider_spam < 0.0 {
            fac.expensive_raider_spam += 1.0;
            return "tankheavyraid";
        }

        fac.add_scout_allowance(1);

        if self.fac_aa_roll() {
            return "tankaa";
        }

        if income > 25.0
            && war.enemy_heavy_porc_value > 0.0
            && kgb_rand() < f64::from(tf * (1.0 - fm.arty_value / war.enemy_porc_value))
        {
            fac.expensive_raider_spam -= 1.0;
            return "tankarty";
        }

        let raid_threshold = if war.sniper_sightings.is_empty() { 0.9 } else { 0.75 };
        if kgb_rand() > raid_threshold {
            let divisor = if fm.big_map { 2.5 } else { 5.0 };
            fac.expensive_raider_spam -= (((income / divisor).floor() as i32).clamp(2, 8) + 1) as f32;
        }

        let rnd = kgb_rand();
        if income < 35.0 && !self.graph.eminent_territory {
            fac.expensive_raider_spam -= 2.0;
            if fm.num_banishers > fm.num_reapers || war.enemy_heavy_porc_value > 850.0 * fm.num_reapers as f32 {
                fac.expensive_raider_spam -= 1.0;
                "tankassault"
            } else {
                "tankriot"
            }
        } else {
            fac.expensive_raider_spam -= 1.0;
            if fm.army_value > 2000.0 && kgb_rand() > 0.9 {
                fac.expensive_raider_spam -= 2.0;
                fac.add_scout_allowance(1);
                return "tankheavyassault";
            }
            if rnd > 0.15 {
                fac.expensive_raider_spam -= 1.0;
                "tankassault"
            } else {
                "tankriot"
            }
        }
    }

    fn fac_get_spiders(&self, fac: &mut Factory) -> &'static str {
        let fm = &self.fac;
        let war = &self.war;
        let tf = self.graph.territory_fraction;

        if self.fac_need_workers(fac) {
            return "spidercon";
        }

        if war.slasher_spam as f32 * 140.0 > fm.assault_value {
            return if kgb_rand() > 0.5 { "spiderassault" } else { "spiderskirm" };
        }

        if self.scouts_want_widow() || self.jaeger_wants(scouts::WIDOW) {
            return scouts::WIDOW;
        }

        if fm.scout_budget > 0 {
            fac.building_for = Some(fm.scout_role);
            return "spiderscout";
        }

        // Raiders (feature `venomraid`): Fleas in the opening, Venoms after
        // it; the Fleas then are the stakeouts' alone (the scout detachment).
        let venom_raid = !self.dev_off("venomraid");
        if venom_raid && fac.raider_spam < 0.0 {
            return self.fac_spider_raider(fac);
        }

        if self.fac_aa_roll() {
            return "spideraa";
        }

        if self.eco.adjusted_income > 30.0
            && kgb_rand() < f64::from((tf / 5.0) * (1.0 - fm.arty_value / war.enemy_porc_value))
        {
            fac.add_scout_allowance(6);
            return "spidercrabe";
        }

        // Now and then a wave of raiders, as the cloak factory sends Glaives.
        if venom_raid && !fm.highprio && kgb_rand() > 0.95 + f64::from(0.025f32.min(tf / 20.0)) {
            fac.raider_spam -= (((self.eco.adjusted_income / 5.0).floor() as i32).clamp(6, 12) - 1) as f32;
            return self.fac_spider_raider(fac);
        }

        fac.add_scout_allowance(3);
        // The Venoms of the army: those raiding are not.
        let venoms = fm.num_venoms - self.roles_count_named(Role::SquadRaider, "spideremp");
        if fm.num_redbacks < venoms {
            return "spiderriot";
        }

        if fm.num_recluses < (venoms + fm.num_redbacks) {
            return "spiderskirm";
        }

        if fm.num_hermits < (venoms + fm.num_redbacks) {
            return "spiderassault";
        }

        let rnd = kgb_rand();
        if rnd > 0.9 || venoms == 0 {
            "spideremp"
        } else if kgb_rand() > 0.5 {
            "spiderskirm"
        } else {
            "spiderassault"
        }
    }

    /// A spider raider: a Flea in the opening, a Venom after it. The
    /// raider budget counts Glaive-sized raiders; a Venom costs as much as
    /// [`VENOM_RAIDER_SLOTS`] of them.
    fn fac_spider_raider(&self, fac: &mut Factory) -> &'static str {
        if self.frame < VENOM_RAIDERS_FROM {
            fac.raider_spam += 1.0;
            fac.building_for = Some(Role::SoloRaider);
            "spiderscout"
        } else {
            fac.raider_spam = (fac.raider_spam + VENOM_RAIDER_SLOTS).min(0.0);
            fac.building_for = Some(Role::SquadRaider);
            "spideremp"
        }
    }

    /// Jumpjets. Puppies to look and picket for the opening (see
    /// `fac_wants_puppy_scout`), Pyro pressure (the opening's solo raiders
    /// run fire-shadow raids, see `precision`), then Moderator squads with a
    /// Placeholder per four Moderators to hold what they shoot, a Jack per
    /// two of those up front, and Pyros guarding them from swarms (see
    /// `war_unit_idle`); Firewalkers against masses of light skirmishers,
    /// Jugglenauts against crowds of close-range units, Puppies to eat the
    /// wrecks between the armies (see `puppies`), Skuttles for the jaegers.
    fn fac_get_jumps(&self, fac: &mut Factory) -> &'static str {
        let fm = &self.fac;
        let war = &self.war;
        let tf = self.graph.territory_fraction;
        let income = self.eco.adjusted_income;

        if self.fac_need_workers(fac) || self.fac_wants_early_constable() {
            return "jumpcon";
        }

        if self.fac_wants_puppy_scout(fac) {
            fac.building_for = Some(Role::Stakeout);
            return crate::military::puppies::PUPPY;
        }

        if self.fac_wants_firewalker() {
            return "jumparty";
        }

        // Jacks walk through Fencer fire (6000 health).
        if war.slasher_spam as f32 * 280.0 > fm.assault_value {
            return "jumpassault";
        }

        if self.jaeger_wants("jumpbomb") {
            return "jumpbomb";
        }

        if self.fac_wants_puppy() {
            return crate::military::puppies::PUPPY;
        }

        if fm.scout_budget > 0 {
            fac.building_for = Some(fm.scout_role);
            return "jumpraid";
        }

        if fac.raider_spam < 0.0 {
            fac.raider_spam += 1.0;
            return "jumpraid";
        }

        // Moderators shoot aircraft too.
        if self.fac_aa_roll() {
            return if kgb_rand() > 0.5 { "jumpaa" } else { "jumpskirm" };
        }

        // An early Jack: a commander killer that jumps out again.
        if fm.num_jacks == 0 && fm.num_pyros >= 2 {
            return "jumpassault";
        }

        if self.fac_wants_jugglenaut() {
            fac.add_scout_allowance(1);
            return "jumpsumo";
        }

        // Pyro waves now and then.
        let raid_threshold =
            if war.sniper_sightings.is_empty() { 0.9 + f64::from(0.05f32.min(tf / 10.0)) } else { 0.8 };
        if !fm.highprio && kgb_rand() > raid_threshold {
            fac.raider_spam -= ((income / 8.0).floor() as i32).clamp(2, 5) as f32;
        }
        if kgb_rand() > 0.5 {
            fac.add_scout_allowance(1);
        }

        // Firewalkers in the army as we grow: the cheapest killers per metal
        // of the jump factory in self-play.
        if income > 25.0 && (fm.num_firewalkers as f32) < (income / 25.0).floor() && kgb_rand() > 0.6 {
            fac.add_scout_allowance(1);
            return "jumparty";
        }

        let support = fm.num_mods + fm.num_placeholders;
        if fm.num_mods >= 4 * (fm.num_placeholders + 1) {
            "jumpblackhole"
        } else if fm.num_jacks * 2 < support {
            "jumpassault"
        } else {
            "jumpskirm"
        }
    }

    /// Constables early, in step with income, as CircuitAI does with jumpjets
    /// (its second by 1.5 minutes, then about one per 5.5 metal of income:
    /// 9 at minute 8 against our 3.5, when it had 18 mexes to our 10; we
    /// built Pyros first). For the first EARLY_CONS_FRAMES; off with
    /// `disable=earlycons`.
    fn fac_wants_early_constable(&self) -> bool {
        if self.frame >= EARLY_CONS_FRAMES || self.dev_off("earlycons") {
            return false;
        }
        // Constables begun or rolling out count too (`new_workers`, until
        // their first idle): the factory picks its next unit as the last
        // one leaves the yard, and missing that one made every opening
        // three Constables.
        let eco = &self.eco;
        let cons = eco.workers.values().filter(|w| !w.is_com).count()
            + eco.new_workers.iter().filter(|id| !eco.workers.contains_key(id)).count();
        cons < EARLY_CONS_MIN.max((self.eco.adjusted_income / EARLY_CONS_INCOME) as usize).min(EARLY_CONS_MAX)
    }

    /// Puppies for the stakeouts from the opening jump factory while the
    /// map is sparse (density under the `density_ref` dial), feature
    /// `puppyscout`: the first two units after the Constables, then one for
    /// every two Pyros. At 45 metal and sight 640 they look and picket where
    /// 220-metal Pyros scouted; none while the stakeouts' loss cap holds
    /// (`stakeouts_capped`; not the delay after each loss: with a pick per
    /// Pyro, 40-60 s apart, it kept one Puppy to six Pyros on Comet).
    fn fac_wants_puppy_scout(&self, fac: &Factory) -> bool {
        if !fac.opener || self.dev_off("puppyscout") || self.dev_off("stakeouts") || self.stakeouts_capped() {
            return false;
        }
        if self.posture.density >= self.posture_levers().density_ref {
            return false;
        }
        let have = self.roles_count_named(Role::Stakeout, crate::military::puppies::PUPPY);
        have < PUPPY_SCOUTS_FIRST.max(self.fac.num_pyros / PYROS_PER_PUPPY_SCOUT)
    }

    /// Firewalkers while the enemy's light skirmishers (Ronins, Rogues,
    /// Recluses...) outweigh what we have of them; a jump factory taken
    /// for that (see `eco_find_requisition`) builds little else.
    pub(crate) fn fac_wants_firewalker(&self) -> bool {
        let skirms = self.war.enemy_light_skirm_value;
        let have = self.fac.num_firewalkers as f32 * FIREWALKER_COST;
        self.eco.adjusted_income > 20.0 && skirms >= FIREWALKER_SKIRMS && have < 0.5 * skirms
    }

    /// Jugglenauts while the enemy's close-range army (raiders, riots,
    /// assaults) is big and we are rich enough.
    fn fac_wants_jugglenaut(&self) -> bool {
        let brawlers = self.war.enemy_brawler_value;
        let have = self.fac.num_jugglenauts as f32 * JUGGLENAUT_COST;
        self.eco.adjusted_income > 35.0 && brawlers >= JUGGLENAUT_BRAWLERS && have < 0.4 * brawlers && kgb_rand() > 0.5
    }

    /// Puppies for a herd that grows with the wreckage away from our base
    /// (they eat it and split, see `puppies`), a few early between the
    /// Pyros, more against cloakbots (mass Puppies are said to beat them).
    fn fac_wants_puppy(&self) -> bool {
        use crate::military::puppies::HERD_CAP;
        let herd = self.puppies_count();
        if herd >= HERD_CAP || self.dev_off("puppies") {
            return false;
        }
        // (No herd of its own in the first minutes: Constables then.)
        let mut want = if self.frame < 30 * 60 * 4 { 0 } else { 2 } + (self.puppies_wreck_metal() / 150.0) as usize;
        if self.war.enemy_cloaky_value > 600.0 {
            want += 6;
        }
        herd < want.min(HERD_CAP) && kgb_rand() > 0.5
    }

    fn fac_get_gunship(&self, fac: &mut Factory) -> &'static str {
        let tf = self.graph.territory_fraction;
        let eminent = self.graph.eminent_territory;

        if fac.raider_spam < 0.0 && kgb_rand() > 0.5 {
            fac.raider_spam += 1.0;
        }
        // The C++ port no longer builds Blastwings (gunshipbomb) for
        // raider spam: the AI does not know how to use them.

        if fac.expensive_raider_spam < 0.0 {
            fac.expensive_raider_spam += 1.0;
            return "gunshipheavyskirm";
        }

        if self.fac_need_workers(fac) || self.eco_wants_air_colonist() {
            return "gunshipcon";
        }
        if let Some(name) = self.firefighters_order(FacKind::Gunship) {
            return name;
        }

        if self.drops_want_charon() {
            return "gunshiptrans";
        }

        if eminent && self.fac_aa_roll() {
            return "gunshipaa";
        }

        if kgb_rand() > 0.75 {
            fac.raider_spam -= 2.0;
        }

        if eminent && kgb_rand() > 0.95 - 0.1f64.max(0.2 * f64::from(tf)) {
            fac.raider_spam -= 3.0;
            fac.expensive_raider_spam -= 6.0;
            return "gunshipkrow";
        }

        if !eminent {
            return "gunshipskirm";
        }

        if kgb_rand() > 0.9 {
            fac.expensive_raider_spam -= 6.0;
        }

        if kgb_rand() > 0.85 {
            return "gunshipassault";
        }
        "gunshipskirm"
    }

    fn fac_get_planes(&self, fac: &mut Factory) -> &'static str {
        if self.fac_need_workers(fac) || self.eco_wants_air_colonist() {
            return "planecon";
        }
        if let Some(name) = self.firefighters_order(FacKind::Plane) {
            return name;
        }
        if self.eco.sparrow.is_none() {
            return "planescout";
        }
        // raven raven raven raven raven... :D
        "bomberprec"
    }

    fn fac_get_strider(&self) -> &'static str {
        let fm = &self.fac;
        let eco = &self.eco;
        let war = &self.war;

        if fm.has_fusion && self.jaeger_wants("striderantiheavy") {
            return "striderantiheavy";
        }

        if eco.bots_viable
            && eco.adjusted_income > 60.0
            && (self.misc_handler.striders.len() as i32) > 1 + fm.num_heavy_striders
            && kgb_rand() > 0.5
        {
            if fm.num_heavy_striders < 2 || kgb_rand() > 0.35 {
                return "striderbantha";
            } else {
                return "striderdetriment";
            }
        }

        if (fm.num_ultis as f32) < war.enemy_heavy_value.ln().round() - 3.0
            && fm.has_fusion
            && ((fm.num_striders > 0 && kgb_rand() > 0.5) || war.enemy_has_troll_com)
        {
            return "striderantiheavy";
        }

        if eco.bots_viable {
            let rnd = kgb_rand();
            if fm.has_fusion && rnd > 0.75 {
                return "striderscorpion";
            }
            return "striderdante";
        }
        if eco.ships_viable {
            if fm.has_fusion && eco.adjusted_income > 100.0 && kgb_rand() < 0.35 {
                return "striderdetriment";
            }
            return "shipheavyarty"; // Shogun
        }
        if fm.has_fusion && eco.spiders_viable {
            return "striderscorpion";
        }
        "striderantiheavy"
    }
}
