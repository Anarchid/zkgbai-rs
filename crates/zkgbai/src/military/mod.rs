//! Military: enemy tracking, threat/value maps, targeting, and the unit
//! handlers (raiders, squads, retreat, misc, bombers).

pub mod air_intel;
pub mod arsenal;
pub mod blasts;
pub mod drops;
pub mod enemy;
pub mod fightsim;
pub mod firefighters;
pub mod handlers;
pub mod impalers;
pub mod incursions;
pub mod intel;
pub mod jaeger;
pub mod jumps;
pub mod missiles;
pub mod nukes;
pub mod precision;
pub mod puppies;
pub mod qrf;
pub mod radar;
pub mod raid_test;
pub mod roles;
pub mod scouts;
pub mod shadow_probe;
pub mod stakeouts;
pub mod strike_test;
pub mod strikes;
pub mod sweeps;
pub mod taskforce;
pub mod tasks;
pub mod teleport;
pub mod unit_classes;
pub mod water;
pub mod wrappers;

pub use enemy::Enemy;
pub use tasks::{DefenseTarget, ScoutTask, ScoutTaskId, ScoutTasks, TargetMarker};
pub use unit_classes::UnitClasses;
pub use wrappers::{Bomber, Coward, Fighter, FighterKind, FighterStore, Porc, Squad, SquadId, SquadKind, Squads};

use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::io::{BufWriter, Write};

use springai::{Callback, NO_TIMEOUT, Unit, Vec3, WeaponDef};

use crate::Ai;
use crate::defs::cmd;
use crate::kgbutil::{ArrayGraphics, ByteArrayGraphics};
use crate::military::roles::Role;
use crate::nav;
use crate::util::{NULLPOS, distance, get_angular_point, get_directional_point, get_radial_point, kgb_rand};
use radar::RadarIdentifier;

/// Starting cost for target searches ("nothing found yet").
const NO_COST: f32 = 65535.0;

/// Java's `Float.MIN_VALUE` (the smallest positive float), which the original
/// used as the starting "best" score of maximizing searches.
const JAVA_FLOAT_MIN_VALUE: f32 = f32::from_bits(1);

/// Threat-grid cell (64 elmos) of a map position.
#[inline]
fn cell(p: Vec3) -> (i32, i32) {
    ((p.x / 64.0).round() as i32, (p.z / 64.0).round() as i32)
}

/// Grid circle `(x, y, radius, power)` of a static AA's threat paint.
fn aa_porc_circle(e: &mut Enemy) -> (i32, i32, i32, i32) {
    let effective_power = e.get_danger_mut() as i32;
    let (x, y) = cell(e.position);
    let r = (e.threat_radius / 64.0).round() as i32;
    (x, y, r + 1, effective_power)
}

/// Weapon range at which an enemy fighter outranges light raiders (Glaive
/// 185, Dagger 212, Kodachi 210; Scalpel 330, Rocko 275).
pub const OUTRANGES_RAIDERS: f32 = 260.0;

pub struct MilitaryManager {
    cb: Callback,

    /// Known enemy units by id (LOS or radar contacts, plus remembered porc).
    pub targets: BTreeMap<i32, Enemy>,
    /// Ids (into `targets`) of enemy static defenses.
    pub enemy_porcs: BTreeSet<i32>,

    pub sniper_sightings: Vec<DefenseTarget>,
    /// Fighter-store ids of mobile anti-air units.
    pub aas: BTreeSet<i32>,
    /// Fighter-store ids of hawks (air superiority).
    pub hawks: BTreeSet<i32>,
    pub target_markers: Vec<TargetMarker>,

    pub available_mobile_threat: f32,
    pub available_raider_threat: f32,
    pub enemy_porc_value: f32,
    pub enemy_light_porc_value: f32,
    pub enemy_heavy_porc_value: f32,
    pub max_enemy_air_value: f32,
    pub slasher_spam: i32,
    pub enemy_heavy_value: f32,
    pub enemy_has_troll_com: bool,
    pub enemy_fighter_value: f32,
    /// Share (by cost) of the enemy's seen mobile fighters whose weapons
    /// outrange light raiders (skirmishers and the like): the raider-layer
    /// matchup, for the operator's `military` view.
    pub outranging_share: f32,
    /// Metal of the enemy's light skirmishers (Ronins, Rogues, Recluses,
    /// Scalpels, Moderators, Fencers, Buoys), Firewalker food.
    pub enemy_light_skirm_value: f32,
    /// Metal of the enemy's close-range army (raiders, riots, assaults),
    /// Jugglenaut food.
    pub enemy_brawler_value: f32,
    /// Metal of the enemy's mobile cloakbots.
    pub enemy_cloaky_value: f32,
    pub enemy_has_anti_nuke: bool,
    pub enemy_has_nuke: bool,

    defense_targets: Vec<DefenseTarget>,
    /// Where enemy incursions will strike first (see [`incursions`]).
    pub predicted_defense_targets: Vec<DefenseTarget>,
    air_defense_targets: Vec<DefenseTarget>,

    radar_id: RadarIdentifier,
    water: water::Water,

    threat_graphics: ArrayGraphics,
    enemy_porc_graphics: ArrayGraphics,
    ally_threat_graphics: ArrayGraphics,
    ally_raider_graphics: ArrayGraphics,
    ally_porc_graphics: ArrayGraphics,
    aa_threat_graphics: ArrayGraphics,
    aa_porc_graphics: ArrayGraphics,
    /// Enemy threat to things under water, of it their defenses', and ours
    /// (see [`water`]).
    sub_threat_graphics: ArrayGraphics,
    sub_porc_graphics: ArrayGraphics,
    ally_sub_graphics: ArrayGraphics,
    value_graphics: ArrayGraphics,
    raider_value_graphics: ArrayGraphics,
    bp_graphics: ByteArrayGraphics,
    riot_graphics: ByteArrayGraphics,
    scythe_graphics: ByteArrayGraphics,

    frame: i32,
    last_defense_frame: i32,
    last_value_redraw_frame: i32,

    has_nuke: bool,

    /// Freshly finished mobile units, dispatched on their first idle.
    new_units: BTreeSet<i32>,
    /// Whether the war has updated yet: units finished before (those
    /// standing when a saved game is loaded) are dispatched at once.
    updated: bool,
    unbuilt_porcs: BTreeSet<i32>,

    // unit and weapon def IDs for unitDamaged (-1 if missing).
    wolv_mine_id: i32,
    wolv_mine_bomblet: i32,
    sniper_bullet_id: i32,
    brawler_bullet_id: i32,

    /// The fight predictor (see [`fightsim::live`]).
    pub fightsim: fightsim::live::FightSim,
}

impl MilitaryManager {
    pub fn new(cb: Callback) -> Self {
        let map = cb.map();
        // port: the C++ used the map width for both dimensions.
        let width = map.width() / 8;
        let height = map.height() / 8;
        MilitaryManager {
            cb,
            targets: BTreeMap::new(),
            enemy_porcs: BTreeSet::new(),
            sniper_sightings: Vec::new(),
            aas: BTreeSet::new(),
            hawks: BTreeSet::new(),
            target_markers: Vec::new(),
            available_mobile_threat: 0.0,
            available_raider_threat: 0.0,
            enemy_porc_value: 0.0,
            enemy_light_porc_value: 0.0,
            enemy_heavy_porc_value: 0.0,
            max_enemy_air_value: 0.0,
            slasher_spam: 0,
            enemy_heavy_value: 0.0,
            enemy_has_troll_com: false,
            enemy_fighter_value: 0.0,
            outranging_share: 0.0,
            enemy_light_skirm_value: 0.0,
            enemy_brawler_value: 0.0,
            enemy_cloaky_value: 0.0,
            enemy_has_anti_nuke: false,
            enemy_has_nuke: false,
            defense_targets: Vec::new(),
            predicted_defense_targets: Vec::new(),
            air_defense_targets: Vec::new(),
            radar_id: RadarIdentifier::new(cb),
            water: water::Water::new(cb, width, height),
            threat_graphics: ArrayGraphics::new(width, height),
            enemy_porc_graphics: ArrayGraphics::new(width, height),
            ally_threat_graphics: ArrayGraphics::new(width, height),
            ally_raider_graphics: ArrayGraphics::new(width, height),
            ally_porc_graphics: ArrayGraphics::new(width, height),
            aa_threat_graphics: ArrayGraphics::new(width, height),
            aa_porc_graphics: ArrayGraphics::new(width, height),
            sub_threat_graphics: ArrayGraphics::new(width, height),
            sub_porc_graphics: ArrayGraphics::new(width, height),
            ally_sub_graphics: ArrayGraphics::new(width, height),
            value_graphics: ArrayGraphics::new(width, height),
            raider_value_graphics: ArrayGraphics::new(width, height),
            bp_graphics: ByteArrayGraphics::new(width, height),
            riot_graphics: ByteArrayGraphics::new(width, height),
            scythe_graphics: ByteArrayGraphics::new(width, height),
            frame: 0,
            last_defense_frame: 0,
            last_value_redraw_frame: 0,
            has_nuke: false,
            new_units: BTreeSet::new(),
            updated: false,
            unbuilt_porcs: BTreeSet::new(),
            wolv_mine_id: -1,
            wolv_mine_bomblet: -1,
            sniper_bullet_id: -1,
            brawler_bullet_id: -1,
            fightsim: fightsim::live::FightSim::default(),
        }
    }

    pub fn get_threat(&self, position: Vec3) -> f32 {
        let (x, y) = cell(position);
        self.threat_graphics.get_value(x, y) / 500.0
    }

    pub fn get_porc_threat(&self, position: Vec3) -> f32 {
        let (x, y) = cell(position);
        self.enemy_porc_graphics.get_value(x, y) / 500.0
    }

    pub fn get_riot_threat(&self, position: Vec3) -> f32 {
        let (x, y) = cell(position);
        self.riot_graphics.get_value(x, y)
    }

    pub fn get_scythe_threat(&self, position: Vec3) -> f32 {
        let (x, y) = cell(position);
        self.scythe_graphics.get_value(x, y)
    }

    pub fn get_effective_threat(&self, position: Vec3) -> f32 {
        let (x, y) = cell(position);
        (self.threat_graphics.get_value(x, y)
            - (self.ally_threat_graphics.get_value(x, y) + self.ally_porc_graphics.get_value(x, y)))
            / 500.0
    }

    pub fn get_tactical_threat(&self, position: Vec3) -> f32 {
        let (x, y) = cell(position);
        (self.threat_graphics.get_value(x, y) - self.ally_porc_graphics.get_value(x, y)) / 500.0
    }

    pub fn get_friendly_threat(&self, position: Vec3) -> f32 {
        let (x, y) = cell(position);
        self.ally_threat_graphics.get_value(x, y) / 500.0
    }

    pub fn get_friendly_raider_threat(&self, position: Vec3) -> f32 {
        let (x, y) = cell(position);
        self.ally_raider_graphics.get_value(x, y) / 500.0
    }

    pub fn get_friendly_porc_threat(&self, position: Vec3) -> f32 {
        let (x, y) = cell(position);
        self.ally_porc_graphics.get_value(x, y)
    }

    /// Strength of the enemy's units and defenses covering `position`, on
    /// the scale of [`Enemy::get_danger`].
    pub fn get_enemy_power(&self, position: Vec3) -> f32 {
        let (x, y) = cell(position);
        self.threat_graphics.get_value(x, y)
    }

    /// Strength of our units and defenses covering `position`, on the scale
    /// of [`Enemy::get_danger`].
    pub fn get_total_friendly_power(&self, position: Vec3) -> f32 {
        let (x, y) = cell(position);
        self.ally_threat_graphics.get_value(x, y) + self.ally_porc_graphics.get_value(x, y)
    }

    pub fn get_total_friendly_threat(&self, position: Vec3) -> f32 {
        let (x, y) = cell(position);
        (self.ally_threat_graphics.get_value(x, y) + self.ally_porc_graphics.get_value(x, y)) / 500.0
    }

    pub fn get_aa_threat(&self, position: Vec3) -> f32 {
        let (x, y) = cell(position);
        (self.aa_threat_graphics.get_value(x, y) + self.aa_porc_graphics.get_value(x, y)) / 500.0
    }

    pub fn get_effective_aa_threat(&self, position: Vec3) -> f32 {
        let (x, y) = cell(position);
        (((self.aa_threat_graphics.get_value(x, y)
            + self.aa_porc_graphics.get_value(x, y)
            + self.threat_graphics.get_value(x, y))
            - (self.ally_threat_graphics.get_value(x, y) + self.ally_porc_graphics.get_value(x, y)))
            / 500.0)
            .max(0.0)
    }

    pub fn get_value(&self, position: Vec3) -> f32 {
        let (x, y) = cell(position);
        self.value_graphics.get_value(x, y)
    }

    pub fn get_raider_value(&self, position: Vec3) -> f32 {
        let (x, y) = cell(position);
        self.raider_value_graphics.get_value(x, y)
    }

    /// Friendly build power painted around `position`.
    pub fn get_bp(&self, position: Vec3) -> f32 {
        let (x, y) = cell(position);
        self.bp_graphics.get_value(x, y)
    }

    pub fn add_defense_target(&mut self, d: DefenseTarget) {
        self.defense_targets.push(d);
    }

    /// Add a defense target, or renew the one at the same place.
    pub fn refresh_defense_target(&mut self, d: DefenseTarget) {
        let same = |t: &DefenseTarget| t.position.x == d.position.x && t.position.z == d.position.z;
        match self.defense_targets.iter_mut().find(|t| same(t)) {
            Some(t) => *t = d,
            None => self.defense_targets.push(d),
        }
    }

    /// Places to defend: attacked lately (`false`) and predicted (`true`).
    pub fn defense_targets(&self) -> impl Iterator<Item = (&DefenseTarget, bool)> {
        self.defense_targets.iter().map(|d| (d, false)).chain(self.predicted_defense_targets.iter().map(|d| (d, true)))
    }

    /// Names of the threat and value maps, for [`MilitaryManager::grid_value`].
    pub const GRIDS: [&'static str; 15] = [
        "threat",
        "enemy_porc",
        "ally_threat",
        "ally_raider",
        "ally_porc",
        "aa_threat",
        "aa_porc",
        "sub_threat",
        "sub_porc",
        "ally_sub",
        "value",
        "raider_value",
        "bp",
        "riot",
        "scythe",
    ];

    /// Size in cells (of 64 elmos) of the threat and value maps.
    pub fn grid_size(&self) -> (i32, i32) {
        (self.threat_graphics.width, self.threat_graphics.height)
    }

    /// Raw cell value of a threat or value map by name (see [`Self::GRIDS`]).
    pub fn grid_value(&self, name: &str, x: i32, y: i32) -> Option<f32> {
        Some(match name {
            "threat" => self.threat_graphics.get_value(x, y),
            "enemy_porc" => self.enemy_porc_graphics.get_value(x, y),
            "ally_threat" => self.ally_threat_graphics.get_value(x, y),
            "ally_raider" => self.ally_raider_graphics.get_value(x, y),
            "ally_porc" => self.ally_porc_graphics.get_value(x, y),
            "aa_threat" => self.aa_threat_graphics.get_value(x, y),
            "aa_porc" => self.aa_porc_graphics.get_value(x, y),
            "sub_threat" => self.sub_threat_graphics.get_value(x, y),
            "sub_porc" => self.sub_porc_graphics.get_value(x, y),
            "ally_sub" => self.ally_sub_graphics.get_value(x, y),
            "value" => self.value_graphics.get_value(x, y),
            "raider_value" => self.raider_value_graphics.get_value(x, y),
            "bp" => self.bp_graphics.get_value(x, y),
            "riot" => self.riot_graphics.get_value(x, y),
            "scythe" => self.scythe_graphics.get_value(x, y),
            _ => return None,
        })
    }

    /// Places attacked lately and places about to be.
    fn all_defense_targets(&self) -> impl Iterator<Item = &DefenseTarget> {
        self.defense_targets.iter().chain(&self.predicted_defense_targets)
    }

    /// Id (into `targets`) of the closest known enemy porc.
    pub fn get_closest_enemy_porc(&self, position: Vec3) -> Option<i32> {
        let mut best = None;
        let mut dist = 0.0;
        for id in &self.enemy_porcs {
            let Some(e) = self.targets.get(id) else { continue };
            if !e.identified
                || e.ud.is_none()
                || !e.is_static
                || e.get_danger() == 0.0
                || e.is_aa
                || e.is_nano_spam
                || e.position == NULLPOS
            {
                continue;
            }
            let tmpdist = distance(position, e.position) + e.def_max_weapon_range();
            if best.is_none() || tmpdist < dist {
                best = Some(*id);
                dist = tmpdist;
            }
        }
        best
    }

    /// Ids (into `targets`) of all current targets.
    pub fn get_targets(&self) -> Vec<i32> {
        self.targets.keys().copied().collect()
    }

    /// Speed-based guesser for radar contacts (built like the original, but
    /// its guesses are not used by the targeting logic).
    pub fn radar_identifier(&self) -> &RadarIdentifier {
        &self.radar_id
    }

    /// Whether `pos` is (nearly) under water, for torpedo-only units.
    fn is_water(&self, pos: Vec3) -> bool {
        self.cb.map().elevation_at(pos.x, pos.z) < 10.0
    }

    pub fn paint_ascii_threat_map(&self) {
        self.write_ascii_map(
            "zkgbai_threatmap.txt",
            self.threat_graphics.width,
            self.threat_graphics.height,
            32767,
            |x, y| self.threat_graphics.get_value(x, y),
        );
    }

    pub fn paint_ascii_porc_map(&self) {
        let g = &self.enemy_porc_graphics;
        self.write_ascii_map("zkgbai_porcmap.txt", g.width, g.height, 32767, |x, y| g.get_value(x, y));
    }

    pub fn paint_ascii_riot_map(&self) {
        let g = &self.riot_graphics;
        self.write_ascii_map("zkgbai_riotmap.txt", g.width, g.height, 127, |x, y| g.get_value(x, y));
    }

    fn write_ascii_map(&self, file: &str, width: i32, height: i32, max: i32, value: impl Fn(i32, i32) -> f32) {
        let write = || -> std::io::Result<()> {
            let mut out = BufWriter::new(File::create(file)?);
            for y in 0..height {
                let line: String = (0..width).map(|x| ascii_value(value(x, y) as i32, max)).collect();
                writeln!(out, "{line}")?;
            }
            out.flush()
        };
        if let Err(e) = write() {
            self.cb.log(&format!("could not write {file}: {e}"));
        }
    }
}

/// One character of a logarithmic ASCII heat map.
fn ascii_value(val: i32, max: i32) -> char {
    const VALS: [char; 11] = ['.', '0', '1', '2', '3', '4', '5', '6', '7', '8', '9'];
    let lv = if val == 0 { 0.0 } else { (val as f32).ln() };
    let lm = (max as f32).ln();
    // port: the C++ indexed out of bounds for val == max.
    let idx = (11.0 * lv / lm) as i32;
    VALS[idx.clamp(0, 10) as usize]
}

/// Cross-module military logic and module events.
impl Ai {
    pub fn war_init(&mut self) {
        self.retreat_init();

        self.war.wolv_mine_id = self.defs.id("wolverine_mine");
        let weapon_id = |name: &str| self.cb.weapon_def_by_name(name).map_or(-1, |w| w.id());
        let wolv_mine_bomblet = weapon_id("wolverine_mine_bomblet");
        let sniper_bullet_id = weapon_id("cloaksnipe_shockrifle");
        let brawler_bullet_id = weapon_id("gunshipheavyskirm_emg");
        self.war.wolv_mine_bomblet = wolv_mine_bomblet;
        self.war.sniper_bullet_id = sniper_bullet_id;
        self.war.brawler_bullet_id = brawler_bullet_id;
    }

    pub fn war_update(&mut self, frame: i32) {
        self.war.frame = frame;
        self.pathfinder.now = frame;
        self.war.updated = true;

        // Each step is guarded separately: a fault in one handler must not
        // stop the others, and the profile then shows each step's cost.
        if frame % 15 == (self.offset + 3) % 15 {
            self.guard("war.updateTargets", |ai| ai.war_update_targets());
        }
        if frame % 7 == (self.offset + 2) % 7 {
            self.guard("war.paintThreatMap", |ai| ai.war_paint_threat_map());
        }
        if frame % 150 == (self.offset + 5) % 150 {
            self.guard("war.idleSweep", |ai| ai.war_idle_sweep());
        }

        // Before the squads decide: their fight predictions arrive here.
        self.guard("fightsim.update", |ai| ai.fightsim_update(frame));
        self.guard("retreat.update", |ai| ai.retreat_update(frame));
        self.guard("raider.update", |ai| ai.raider_update(frame));
        self.guard("squad.update", |ai| ai.squad_update(frame));
        self.guard("misc.update", |ai| ai.misc_update(frame));
        self.guard("bomber.update", |ai| ai.bomber_update(frame));
        self.guard("teleport.update", |ai| ai.teleport_update(frame));
        self.guard("drops.update", |ai| ai.drops_update(frame));
        self.guard("intel.update", |ai| ai.intel_update(frame));
        self.guard("nukes.update", |ai| ai.nukes_update(frame));
        self.guard("blasts.update", |ai| ai.blasts_update(frame));
        self.guard("missiles.update", |ai| ai.missiles_update(frame));
        self.guard("scouts.update", |ai| ai.scouts_update(frame));
        self.guard("jaeger.update", |ai| ai.jaeger_update(frame));
        self.guard("impalers.update", |ai| ai.impalers_update(frame));
        self.guard("firefighters.update", |ai| ai.firefighters_update(frame));
        self.guard("jumps.update", |ai| ai.jumps_update(frame));
        self.guard("puppies.update", |ai| ai.puppies_update(frame));
        self.guard("stakeouts.update", |ai| ai.stakeouts_update(frame));
        self.guard("airIntel.update", |ai| ai.air_intel_update(frame));
        self.guard("precision.update", |ai| ai.precision_update(frame));
        self.guard("water.update", |ai| ai.water_update(frame));
        self.guard("strikes.update", |ai| ai.strikes_update(frame));
        self.guard("strike_test.update", |ai| ai.strike_test_update(frame));
        self.guard("raid_test.update", |ai| ai.raid_test_update(frame));
        self.guard("qrf.update", |ai| ai.qrf_update(frame));

        if frame % 22 == self.offset % 22 && !self.war.aas.is_empty() {
            self.guard("war.updateAAs", |ai| ai.war_update_aas());
        }
    }

    /// Drive the mobile anti-air: hide from ground threat, else hunt air.
    fn war_update_aas(&mut self) {
        let mut dead = Vec::new();
        let ids: Vec<i32> = self.war.aas.iter().copied().collect();
        // The fallback is identical for every AA in this update. In dense
        // bases its engine build-site query can cost over a millisecond.
        let mut fallback = None;
        for id in ids {
            let Some(r) = self.fighters.get(&id) else {
                dead.push(id);
                continue;
            };
            let unit = r.unit;
            if unit.health() <= 0.0 || unit.team() != self.team_id {
                dead.push(id);
                continue;
            }
            if self.retreat_handler.is_retreating(id) {
                continue;
            }
            let pos = r.get_pos();
            if self.war.get_porc_threat(pos) > 0.0 || self.war.get_effective_threat(pos) > 0.0 {
                let haven = if unit.def().is_some_and(|d| d.is_able_to_fly()) {
                    self.graph_get_closest_air_haven(pos)
                } else {
                    self.graph_get_closest_haven(pos)
                };
                let mut nav = nav!(self);
                if let Some(r) = self.fighters.get_mut(&id) {
                    r.sneak(haven, &mut nav);
                }
            } else {
                let target = get_angular_point(self.war_get_aa_target(id, &mut fallback), pos, 350.0);
                let mut nav = nav!(self);
                if let Some(r) = self.fighters.get_mut(&id) {
                    r.raid(target, &mut nav);
                }
            }
        }
        for id in dead {
            self.war.aas.remove(&id);
            self.fighters.remove(&id);
        }
    }

    pub fn war_unit_finished(&mut self, unit: Unit) {
        // The staged raid game's force is the QRF's alone (see `qrf`).
        if self.qrf_claims(unit.id()) {
            return;
        }
        let Some(ud) = unit.def() else { return };
        let def_name = self.defs.name(ud).to_owned();
        let name = def_name.as_str();
        let cost = ud.cost(self.metal);
        let team = self.team_id;
        // A unit built for a role joins its handler before anyone else
        // claims it (see `roles`).
        if let Some(role) = self.role_take_declared(unit.id()) {
            self.assign_role(unit, role);
            if role == Role::Stakeout {
                return;
            }
            // Raiders run home to be repaired, and are set up when first
            // idle (where no other handler takes them).
            self.war.new_units.insert(unit.id());
            if !self.unit_classes.no_retreat.contains(name) {
                self.retreat_add_coward(unit);
            }
            if !self.war.updated {
                self.war_unit_idle(unit);
            }
            return;
        }
        // Forces short of a spotter take one first (see `taskforce::spotter`).
        if self.taskforce_spotter_claims(unit, name, cost) {
            return;
        }
        // Raids on our economy call for air (see `firefighters`).
        if self.firefighters_claims(name) {
            self.firefighters_add(unit);
            return;
        }
        // Scouts picket the lanes to our economy (see `stakeouts`).
        if self.stakeouts_claims(name) {
            self.assign_role(unit, Role::Stakeout);
            return;
        }

        if name == "planelightscout" || name == "planescout" {
            unit.set_idle_mode(0, 0);
            unit.set_move_state(1, 0);
            self.raider_add_sparrow(Fighter::new_raider(unit, cost, team));
        }

        if self.unit_classes.bombers.contains(name) {
            unit.set_idle_mode(0, 0);
            unit.set_move_state(0, 0);
            unit.set_fire_state(0, 0);
            self.bomber_add_bomber(Bomber::new(unit, team));
        }

        // Deactivate outlaws so they don't destroy reclaim
        if name == "shieldriot" {
            unit.execute_custom_command(cmd::WANT_ONOFF, &[0.0], 0, NO_TIMEOUT);
        }

        if name == "staticnuke" {
            self.war.has_nuke = true;
            self.misc_add_nuke(unit);
        }

        if name == missiles::SILO {
            self.missiles_add_silo(unit);
            return;
        }
        if missiles::MISSILES.contains(&name) {
            self.missiles_add_missile(unit);
            return;
        }

        if name == "zenith" {
            self.misc_add_zenith(unit);
        }

        if name == "raveparty" {
            self.misc_add_drp(unit);
        }

        if name == "staticheavyarty" {
            self.misc_add_bertha(unit);
        }

        if name == "amphtele" {
            self.teleport_add_djinn(unit);
            return;
        }
        if name == "gunshiptrans" {
            self.drops_add_charon(unit);
            return;
        }
        // Impalers form batteries (see `impalers`).
        if name == impalers::IMPALER && self.impalers_claims(unit, cost) {
            return;
        }
        // Batteries take escorts (see `impalers`).
        if self.impalers_claims_escort(unit, name, cost) {
            self.retreat_add_coward(unit);
            return;
        }
        // Anti-heavies hunt heavies (see `jaeger`); other Widows scout.
        if self.jaeger_claims(name) {
            self.jaeger_add(unit, cost);
            return;
        }
        if name == scouts::WIDOW || (name == scouts::SCYTHE && self.scouts_adopt_scythe()) {
            self.scouts_add(unit);
            return;
        }
        // Puppies herd to wrecks (see `puppies`); those split off a Puppy
        // guard it and never idle, so they are taken in here.
        if name == puppies::PUPPY {
            self.puppies_add(unit);
            return;
        }
        // Jugglenauts pull (see `jumps`).
        if name == "jumpsumo" {
            unit.execute_custom_command(cmd::PUSH_PULL, &[0.0], 0, NO_TIMEOUT);
        }

        let mobile_combat = ud.speed() > 0.0 && ud.build_options().is_empty();
        if self.unit_classes.striders.contains(name) {
            unit.set_move_state(1, 0);
            self.misc_add_strider(Fighter::new_strider(unit, cost, team));
        } else if mobile_combat {
            self.war.new_units.insert(unit.id());
        }

        if mobile_combat && !self.unit_classes.no_retreat.contains(name) {
            self.retreat_add_coward(unit);
        }
        // Standing at a load, they may keep orders and never idle.
        if !self.war.updated {
            self.war_unit_idle(unit);
        }
    }

    /// Route new units that stand idle without having said so: those
    /// standing when a saved game is loaded (finished again, but idle
    /// already), or whose idle event was missed.
    fn war_idle_sweep(&mut self) {
        let idle: Vec<Unit> = (self.war.new_units.iter())
            .filter_map(|&id| Unit::from_id(self.cb, id))
            .filter(|u| u.health() > 0.0 && u.team() == self.team_id && u.num_current_commands() == 0)
            .collect();
        for unit in idle {
            self.war_unit_idle(unit);
        }
    }

    pub fn war_unit_idle(&mut self, unit: Unit) {
        if !self.war.new_units.remove(&unit.id()) {
            return;
        }
        // Posted sniper teams stay posted (see `strike_test`).
        if self.strike_test_posted() {
            return;
        }
        // Staged raids are squads already (see `raid_test`).
        if self.raid_test_claims(unit.id()) || self.qrf_claims(unit.id()) {
            return;
        }

        let Some(ud) = unit.def() else { return };
        let def_name = self.defs.name(ud).to_owned();
        let name = def_name.as_str();
        let cost = ud.cost(self.metal);
        let team = self.team_id;

        // enable snipers and penetrators to shoot radar dots
        if name == "cloaksnipe" || name == "hoverarty" {
            unit.execute_custom_command(cmd::DONT_FIRE_AT_RADAR, &[0.0], 0, NO_TIMEOUT);
            unit.set_move_state(1, 0);
        }

        // disable air strafe for brawlers
        if name == "gunshipheavyskirm" {
            unit.execute_custom_command(cmd::AIR_STRAFE, &[0.0], 0, NO_TIMEOUT);
        }

        // set blastwings to land when idle
        if name == "gunshipbomb" {
            unit.set_idle_mode(1, 0);
        }

        if self.unit_classes.planes.contains(name) {
            unit.set_idle_mode(0, 0);
        }

        if name == "planefighter" {
            unit.set_move_state(0, 0);
            self.misc_add_swift(unit);
        }

        if name == "planeheavyfighter" {
            unit.set_move_state(1, 0);
            let f = Fighter::new_raider(unit, cost, team);
            self.war.hawks.insert(f.id);
            self.fighters.insert(f.id, f);
            if self.war.hawks.len() > 4 {
                let mut hawks = std::mem::take(&mut self.war.hawks);
                self.war.aas.append(&mut hawks);
            }
        }

        if name == "gunshipkrow" {
            unit.set_move_state(1, 0);
            self.misc_add_krow(Fighter::new_krow(unit, cost, team));
        }

        // Pyros give themselves away firing at radar dots.
        if name == "jumpraid" {
            unit.execute_custom_command(cmd::DONT_FIRE_AT_RADAR, &[1.0], 0, NO_TIMEOUT);
        }
        // Units with a role are their handler's already (see `roles`), but
        // for raiding Venoms: the spider raider phase makes them to be the
        // home defense, so the QRF's floor may post one (see `qrf::floor`).
        if let Some(role) = self.role_of(unit.id()) {
            if role == Role::SquadRaider && name == "spideremp" && self.qrf_floor_can_take(unit) {
                self.raider_remove_unit(unit);
                self.qrf_floor_take(unit);
            }
            return;
        }
        // Raiders for home defense, while its budget wants more (see `qrf`).
        if self.qrf_recruit(unit) {
            return;
        }
        // A Pyro per three Moderators and Placeholders guards their squads
        // from swarms; a few raid alone (fire-shadow raids: their flames go
        // through hills, see `precision`), the rest in squads.
        if name == "jumpraid" {
            if self.war_wants_pyro_guard() {
                unit.set_move_state(1, 0);
                self.squad_add_assault(Fighter::new(unit, cost, team));
                return;
            }
            let solo = (self.raider_handler.solo_raiders.iter())
                .filter(|id| self.fighters.get(id).is_some_and(|f| self.defs.name_of(f.unit) == "jumpraid"))
                .count();
            if solo < (3 + (self.eco.adjusted_income / 30.0) as usize).min(6) {
                self.assign_role(unit, Role::SoloRaider);
                return;
            }
        }

        // Units a doctrine takes form taskforces (see `taskforce`): the
        // shield ball, and the forces other doctrines make.
        self.taskforce_load();
        if self.taskforces.routes(name) {
            let mut f = Fighter::new(unit, cost, team);
            f.shield_mob = true;
            self.squad_add_taskforce_member(f);
            return;
        }
        // Dirtbags the strikes ordered (see `strikes_production`) that no
        // stakeout took raid solo until a strike conscripts them.
        if name == strikes::DIRTBAG {
            self.assign_role(unit, Role::SoloRaider);
            return;
        }
        let classes = &self.unit_classes;
        if classes.small_raiders.contains(name) {
            if name == "amphbomb" {
                unit.set_move_state(2, 0);
                unit.set_fire_state(2, 0);
                unit.execute_custom_command(cmd::UNIT_AI, &[0.0], 0, NO_TIMEOUT);
            }
            unit.set_move_state(1, 0);
            self.raider_add_small_raider(Fighter::new_raider(unit, cost, team));
        } else if classes.medium_raiders.contains(name) {
            unit.set_move_state(1, 0);
            self.raider_add_medium_raider(Fighter::new_raider(unit, cost, team));
        } else if classes.assaults.contains(name) {
            unit.set_move_state(1, 0);
            self.squad_add_assault(Fighter::new(unit, cost, team));
        } else if classes.mob_supports.contains(name) {
            self.misc_add_support(Fighter::new(unit, cost, team));
        } else if classes.loners.contains(name) {
            if name == "vehsupport" || name == "spidercrabe" {
                unit.set_move_state(0, 0);
                unit.execute_custom_command(cmd::UNIT_AI, &[1.0], 0, NO_TIMEOUT);
            } else {
                unit.set_move_state(1, 0);
            }
            self.misc_add_loner(Fighter::new(unit, cost, team));
        } else if classes.arties.contains(name) {
            if matches!(name, "vehheavyarty" | "tankarty" | "striderarty" | "tankheavyarty") {
                unit.set_move_state(0, 0);
                unit.execute_custom_command(cmd::UNIT_AI, &[1.0], 0, NO_TIMEOUT);
            } else {
                unit.set_move_state(1, 0);
            }
            self.misc_add_arty(Fighter::new(unit, cost, team));
        } else if classes.aas.contains(name) {
            unit.set_move_state(2, 0);
            let f = Fighter::new_raider(unit, cost, team);
            self.war.aas.insert(f.id);
            self.fighters.insert(f.id, f);
            let pos = self.graph.get_ally_center();
            if pos != NULLPOS {
                unit.fight(pos, 0);
            }
        } else if classes.sappers.contains(name) {
            unit.set_move_state(2, 0);
            unit.set_fire_state(2, 0);
            unit.execute_custom_command(cmd::UNIT_AI, &[0.0], 0, NO_TIMEOUT);
            unit.fight(get_radial_point(self.graph.get_ally_center(), 800.0), 0);
            self.misc_add_sapper(unit);
        }
    }

    /// Whether the squads' Moderators and Placeholders want another Pyro
    /// with them.
    fn war_wants_pyro_guard(&self) -> bool {
        let (mut guards, mut held) = (0, 0);
        for id in &self.squad_handler.fighters {
            match self.fighters.get(id).map(|f| self.defs.name_of(f.unit)) {
                Some("jumpraid") => guards += 1,
                Some("jumpskirm" | "jumpblackhole") => held += 1,
                _ => {}
            }
        }
        held >= 2 && guards * 3 < held
    }

    pub fn war_unit_damaged(
        &mut self,
        unit: Unit,
        attacker: Option<Unit>,
        damage: f32,
        dir: Vec3,
        weapon: Option<WeaponDef>,
        paralyzer: bool,
    ) {
        let _ = damage;
        self.raider_friendly_hit(unit, attacker, paralyzer);
        if self.test.as_deref() == Some("shadowprobe") {
            self.shadow_probe_damaged(unit, weapon);
        }
        let h = unit;
        let hdef = h.def();
        if hdef.is_some_and(|d| d.id() == self.war.wolv_mine_id)
            || weapon.is_some_and(|w| w.id() == self.war.wolv_mine_bomblet)
        {
            return;
        }
        self.qrf_asset_pressed(unit, attacker);
        self.strikes_unit_damaged(unit, attacker, dir, weapon);
        self.taskforce_spotter_damaged(unit);
        self.pack_touched(unit, true);

        // check if the damaged unit is on fire.
        let on_fire = h.rules_param_float("on_fire", 0.0) > 0.0;

        // retreat scouting raiders so that they don't suicide into enemy raiders
        if h.max_speed() > 0.0 {
            self.retreat_check_unit(h, on_fire);
            if !on_fire {
                self.raider_avoid_enemies(h, attacker, dir);
            }
            self.precision_damaged(h, attacker, dir, weapon);
        }

        // create a defense task, if appropriate.
        let Some(wd) = weapon else { return };
        let frame = self.war.frame;
        let wd_id = wd.id();
        let static_builder = hdef.is_some_and(|d| d.build_speed() > 0.0 && d.speed() == 0.0);
        // (The original also tested "no def and not in LOS", which the "no
        // def" test already covers.)
        let attacker_hidden = attacker.is_none_or(|a| a.def().is_none() || a.pos() == NULLPOS);
        if (attacker_hidden || static_builder)
            && (frame - self.war.last_defense_frame > 30 || wd_id == self.war.sniper_bullet_id)
        {
            // only create defense targets on unitDamaged if the attacker is invisible or out of los.
            if self.unit_classes.porc_weps.contains(&wd_id) {
                return;
            }
            let target = match attacker {
                Some(a) if a.pos() != NULLPOS => a.pos(),
                _ => {
                    let dist = wd.range();
                    let pos = h.pos();
                    let target = Vec3::xz(pos.x + dist * dir.x, pos.z + dist * dir.z);
                    if self.los.is_in_los(target) {
                        return;
                    }
                    target
                }
            };
            let mut dt = if static_builder {
                DefenseTarget::new(target, h.max_health(), frame)
            } else {
                DefenseTarget::new(target, wd.range(), frame)
            };
            // Hit by a water weapon from the water: what did it may be under
            // water, where only water weapons reach it.
            dt.under_water = wd.is_water_weapon() && self.war.water_depth(target) > 0.0;

            let war = &mut self.war;
            war.last_defense_frame = frame;
            if wd_id == war.sniper_bullet_id {
                war.defense_targets.push(dt);
                war.sniper_sightings.push(dt);
            } else if wd_id == war.brawler_bullet_id {
                war.air_defense_targets.push(dt);
            } else {
                war.defense_targets.push(dt);
            }
        }
    }

    pub fn war_unit_destroyed(&mut self, unit: Unit, attacker: Option<Unit>) {
        let ud = unit.def();
        let name = ud.map_or("", |d| self.defs.name(d));
        if name == "wolverine_mine" {
            return;
        }
        let is_nuke = name == "staticnuke";
        let is_sapper = self.unit_classes.sappers.contains(name);
        // Missiles on their pads paint no threat either (they fly off).
        let is_heavy_arty = name == "staticheavyarty" || missiles::MISSILES.contains(&name);
        let id = unit.id();

        self.war.new_units.remove(&id);
        self.raid_test_unit_destroyed(unit);
        self.firefighters_unit_destroyed(unit, attacker);
        self.qrf_asset_lost(unit, attacker);
        self.qrf_remove_unit(id);
        self.strikes_unit_destroyed(unit, attacker);

        // (Before the retreat handler forgets whether it was retreating.)
        self.impalers_remove_unit(unit);
        self.retreat_remove_unit(unit);
        self.raider_remove_unit(unit);
        self.squad_log_death(unit, attacker);
        self.taskforce_spotter_lost(unit, attacker);
        self.squad_remove_unit(unit);
        self.bomber_remove_unit(unit);
        self.misc_remove_unit(unit);
        self.jaeger_remove_unit(unit);
        self.jumps_remove_unit(unit);
        self.puppies_remove_unit(unit);
        self.stakeouts_remove_unit(unit, attacker);
        self.roles_unit_destroyed(id);

        if is_nuke {
            self.war.has_nuke = false;
        }

        if self.war.aas.remove(&id) {
            self.fighters.remove(&id);
        }
        if self.war.hawks.remove(&id) {
            self.fighters.remove(&id);
        }

        if is_sapper {
            // rally units to fight if a tick/roach etc dies.
            let dt = DefenseTarget::new(unit.pos(), 1500.0, self.war.frame);
            self.war.defense_targets.push(dt);
        }

        // Unpaint ally threat for porc
        if let Some(ud) = ud
            && ud.speed() == 0.0
            && ud.is_able_to_attack()
            && !is_heavy_arty
            && !self.war.unbuilt_porcs.remove(&id)
        {
            let power = ((unit.power() + unit.max_health()) / 10.0) as i32;
            let radius = unit.max_range();
            let (x, y) = cell(unit.pos());
            let rad = (radius / 64.0).round() as i32;
            self.war.ally_porc_graphics.unpaint_circle(x, y, rad, power);
        }
    }

    pub fn war_unit_captured(&mut self, unit: Unit, old_team: i32, new_team: i32) {
        let _ = new_team;
        if old_team == self.team_id {
            self.war_unit_destroyed(unit, None);
        }
    }

    pub fn war_unit_given(&mut self, unit: Unit, old_team: i32, new_team: i32) {
        let _ = old_team;
        // remove units that allies captured from enemy targets
        let id = unit.id();
        if self.war.targets.contains_key(&id) {
            self.bomber_remove_target(id);
            self.war.enemy_porcs.remove(&id);
            self.war.targets.remove(&id);
        }
        if new_team == self.team_id {
            self.war_unit_finished(unit);
            self.war_unit_idle(unit);
        }
    }

    pub fn war_enemy_enter_los(&mut self, enemy: Unit) {
        // port: the C++ dereferenced the def unchecked; LOS contacts have one.
        let Some(def) = enemy.def() else { return };
        let id = enemy.id();
        let frame = self.war.frame;
        let is_mine = def.id() == self.war.wolv_mine_id;
        let cost = def.cost(self.metal);

        if self.war.targets.contains_key(&id) {
            if is_mine {
                self.bomber_remove_target(id);
                self.war.targets.remove(&id);
                // port: also forget it as porc (the C++ left a dangling pointer).
                self.war.enemy_porcs.remove(&id);
                return;
            }
            let war = &mut self.war;
            let Some(e) = war.targets.get_mut(&id) else { return };
            e.visible = true;
            e.last_seen = frame;
            e.check_nano();
            if !e.identified {
                e.identified = true;
                e.update_from_unit_def(def, cost);
                if e.is_porc && !e.is_nano_spam {
                    war.enemy_porcs.insert(id);
                }
            }

            // paint enemy threat for aa
            if e.is_static && (e.is_aa || e.is_flex_aa) && !e.is_painted && !e.unit.is_being_built() {
                let (x, y, r, p) = aa_porc_circle(e);
                war.aa_porc_graphics.paint_circle(x, y, r, p);
                e.is_painted = true;
            }
        } else if !is_mine {
            let war = &mut self.war;
            let mut e = Enemy::new(enemy, cost);
            e.visible = true;
            e.identified = true;
            e.last_seen = frame;
            e.check_nano();

            if e.is_porc && !e.is_nano_spam {
                war.enemy_porcs.insert(id);
            }

            if e.is_static && e.is_aa && !e.is_painted && !e.unit.is_being_built() {
                let (x, y, r, p) = aa_porc_circle(&mut e);
                war.aa_porc_graphics.paint_circle(x, y, r, p);
                e.is_painted = true;
            }
            war.targets.insert(id, e);
        }
        self.strikes_enemy_seen(enemy);
    }

    pub fn war_enemy_finished(&mut self, enemy: Unit) {
        let Some(def) = enemy.def() else { return };
        let id = enemy.id();
        let frame = self.war.frame;
        let cost = def.cost(self.metal);
        let war = &mut self.war;

        if let Some(e) = war.targets.get_mut(&id) {
            e.check_nano();
            if !e.identified {
                e.identified = true;
                e.update_from_unit_def(def, cost);
                if e.is_porc && !e.is_nano_spam {
                    war.enemy_porcs.insert(id);
                }
            }
            // paint enemy threat for aa
            // (Into the mobile AA grid, as in the original; it is repainted
            // from scratch every 14 frames.)
            if e.is_static && (e.is_aa || e.is_flex_aa) && !e.is_painted {
                let (x, y, r, p) = aa_porc_circle(e);
                war.aa_threat_graphics.paint_circle(x, y, r, p);
                e.is_painted = true;
            }
        } else if def.id() != war.wolv_mine_id {
            let mut e = Enemy::new(enemy, cost);
            e.visible = true;
            e.identified = true;
            e.last_seen = frame;

            if e.is_porc && !e.is_nano_spam {
                war.enemy_porcs.insert(id);
            }
            war.targets.insert(id, e);
        }
    }

    pub fn war_enemy_leave_los(&mut self, enemy: Unit) {
        let frame = self.war.frame;
        if let Some(e) = self.war.targets.get_mut(&enemy.id()) {
            e.visible = false;
            e.last_seen = frame;
        }
    }

    pub fn war_enemy_enter_radar(&mut self, enemy: Unit) {
        let frame = self.war.frame;
        let e = self.war.targets.entry(enemy.id()).or_insert_with(|| Enemy::new(enemy, 50.0));
        e.is_radar_visible = true;
        e.last_seen = frame;
    }

    pub fn war_enemy_leave_radar(&mut self, enemy: Unit) {
        let frame = self.war.frame;
        if let Some(e) = self.war.targets.get_mut(&enemy.id()) {
            e.is_radar_visible = false;
            e.last_seen = frame;
        }
    }

    pub fn war_enemy_destroyed(&mut self, enemy: Unit, attacker: Option<Unit>) {
        let id = enemy.id();
        // Commanders come free, so they do not count as metal destroyed.
        let cost = self.war.targets.get(&id).and_then(|e| e.ud.filter(|_| !e.is_com)).map(|ud| ud.cost(self.metal));
        self.intel_enemy_destroyed(id, cost);
        self.jaeger_enemy_destroyed(id);
        self.fightsim_enemy_destroyed(id);
        self.strikes_enemy_destroyed(enemy, attacker);
        self.impalers_enemy_destroyed(enemy, attacker);
        self.taskforce_enemy_destroyed(enemy, attacker);
        self.raid_test_enemy_destroyed(enemy);
        self.firefighters_enemy_destroyed(enemy, attacker);
        let war = &mut self.war;
        let Some(e) = war.targets.get_mut(&id) else { return };
        e.is_dead = true;
        let shiny = e.identified
            && e.ud.is_some()
            && !e.is_nano_spam
            && (e.def_name == "energysingu" || e.def_name == "energyheavygeo")
            && (!enemy.is_being_built() || enemy.build_progress() > 0.8);

        // Unpaint enemy threat for statics
        if e.is_static && (e.is_aa || e.is_flex_aa) && e.is_painted {
            let (x, y, r, p) = aa_porc_circle(e);
            war.aa_porc_graphics.unpaint_circle(x, y, r, p);
        }

        if shiny {
            self.say("SHINY!");
        }

        self.bomber_remove_target(id);
        self.war.enemy_porcs.remove(&id);
        self.war.targets.remove(&id);
    }

    /// Paint enemy value (minus our own exposed value) for superweapons.
    fn war_paint_value_map(&mut self, wep_name: &str) {
        let (r, rr) = if wep_name == "staticnuke" {
            (14, 20) // general superwep kill radius
        } else {
            (10, 13)
        };
        let metal = self.metal;
        let war = &mut self.war;
        war.value_graphics.clear();
        for t in war.targets.values() {
            let Some(ud) = t.ud else { continue };
            if t.position != NULLPOS
                && t.identified
                && !t.is_nano_spam
                && !t.can_fly()
                && t.def_name != "turretaalaser"
                && t.def_name != "turretgauss"
            {
                let (x, y) = cell(t.position);
                let value = (ud.cost(metal) / 10.0).min(750.0) as i32;
                war.value_graphics.paint_circle(x, y, r, value);
            }
        }

        let fighters = &self.fighters;
        for s in self.squad_handler.squads.iter().filter_map(|&id| self.squads.get(id)) {
            if !s.is_dead(fighters) {
                let (x, y) = cell(s.get_pos(fighters));
                let value = (s.metal_value / 10.0) as i32;
                war.value_graphics.unpaint_circle(x, y, rr, value);
            }
        }

        for s in self.misc_handler.striders.iter().filter_map(|id| fighters.get(id)) {
            let (x, y) = cell(s.get_pos());
            let value = (s.metal_value / 10.0).min(750.0) as i32;
            war.value_graphics.unpaint_circle(x, y, rr, value);
        }

        for s in self.squad_handler.taskforces.iter().filter_map(|&id| self.squads.get(id)) {
            if !s.is_dead(fighters) {
                let (x, y) = cell(s.get_pos(fighters));
                let value = (s.metal_value / 10.0) as i32;
                war.value_graphics.unpaint_circle(x, y, r, value);
            }
        }
    }

    /// Repaint the threat grids, alternating every 7 frames between allied
    /// mobile threat + enemy threat and allied porc + enemy porc/loot value.
    fn war_paint_threat_map(&mut self) {
        let frame = self.war.frame;
        let team = self.team_id;
        // Anti-air we know of only by its fire (see `air_intel`).
        let unseen_aa = self.air_intel_threat();
        let fighters = &self.fighters;
        let squads = &self.squads;
        let war = &mut self.war;

        if frame % 14 == (self.offset + 2) % 14 {
            war.ally_threat_graphics.clear();
            war.ally_raider_graphics.clear();
            let mut mobile_threat = 0.0f32;
            let mut raider_threat = 0.0f32;
            let alive = |u: Unit| u.health() > 0.0 && u.team() == team;

            // paint allythreat for raiders
            for r in self.raider_handler.solo_raiders.iter().filter_map(|id| fighters.get(id)) {
                if !alive(r.unit) {
                    continue;
                }
                let power = ((r.power + r.unit.max_health()) / 10.0) as i32;
                raider_threat += power as f32;
                let (x, y) = cell(r.get_pos());
                let rad = 7;
                war.ally_threat_graphics.paint_circle(x, y, rad, power);
                war.ally_raider_graphics.paint_circle(x, y, rad, power);
            }

            for r in self.raider_handler.kodachis.iter().filter_map(|id| fighters.get(id)) {
                if !alive(r.unit) {
                    continue;
                }
                let power = ((r.power + r.unit.max_health()) / 10.0) as i32;
                let (x, y) = cell(r.get_pos());
                let rad = 7;
                war.ally_threat_graphics.paint_circle(x, y, rad, power);
            }

            // The QRF's own teams (moonlighters are painted as squads).
            for m in self.qrf.members.values().filter(|m| m.squad.is_none()) {
                if !alive(m.unit) {
                    continue;
                }
                let power = ((m.threat + m.unit.max_health()) / 10.0) as i32;
                if !self.retreat_handler.is_retreating(m.unit.id()) {
                    mobile_threat += power as f32;
                }
                let (x, y) = cell(m.pos());
                war.ally_threat_graphics.paint_circle(x, y, 8, power);
            }

            for rs in self.raider_handler.raider_squads.iter().filter_map(|&id| squads.get(id)) {
                if rs.is_dead(fighters) {
                    continue;
                }
                let power = rs.get_threat(fighters);
                if rs.status != 'a' {
                    mobile_threat += power as f32;
                } else {
                    raider_threat += power as f32;
                }
                let (x, y) = cell(rs.get_pos(fighters));
                let rad = if rs.raider_type == 's' { 8 } else { 10 };
                war.ally_threat_graphics.paint_circle(x, y, rad, power);
                if rs.status != 'f' {
                    war.ally_raider_graphics.paint_circle(x, y, rad, power);
                }
            }

            // paint allythreat for squads
            for (list, rad) in [
                (&self.squad_handler.squads, 10),
                (&self.squad_handler.air_squads, 13),
                (&self.squad_handler.taskforces, 10),
            ] {
                for s in list.iter().filter_map(|&id| squads.get(id)) {
                    if s.is_dead(fighters) {
                        continue;
                    }
                    let power = s.get_threat(fighters);
                    mobile_threat += power as f32;
                    let (x, y) = cell(s.get_pos(fighters));
                    war.ally_threat_graphics.paint_circle(x, y, rad, power);
                }
            }

            // paint allythreat for loners
            for f in self.misc_handler.loners.iter().filter_map(|id| fighters.get(id)) {
                if f.is_dead() {
                    continue;
                }
                let power = ((f.power + f.unit.max_health()) / 10.0) as i32;
                if !self.retreat_handler.is_retreating(f.id) {
                    mobile_threat += power as f32;
                }
                let (x, y) = cell(f.get_pos());
                let rad = 10;
                war.ally_threat_graphics.paint_circle(x, y, rad, power);
            }

            // paint allythreat for striders, then krows
            for list in [&self.misc_handler.striders, &self.misc_handler.krows] {
                for s in list.iter().filter_map(|id| fighters.get(id)) {
                    if !alive(s.unit) {
                        continue;
                    }
                    let power = ((s.power + s.unit.max_health()) / 10.0) as i32;
                    if !self.retreat_handler.is_retreating(s.id) {
                        mobile_threat += power as f32;
                    }
                    let (x, y) = cell(s.get_pos());
                    let rad = 15;
                    war.ally_threat_graphics.paint_circle(x, y, rad, power);
                }
            }

            mobile_threat /= 500.0;
            mobile_threat *= 1.5;
            raider_threat /= 500.0;
            war.available_mobile_threat = mobile_threat;
            war.available_raider_threat = raider_threat;

            // paint allythreat for commanders and welders
            for w in self.eco.workers.values() {
                if !alive(w.unit) || !w.unit.def().is_some_and(|d| d.is_able_to_attack()) {
                    continue;
                }
                let power = ((w.power + w.unit.max_health()) / 14.0) as i32;
                let (x, y) = cell(w.get_pos());
                let rad = 10;
                war.ally_threat_graphics.paint_circle(x, y, rad, power);
            }

            // paint buildpower
            // (Never cleared, as in the original.)
            for w in self.eco.workers.values() {
                if !alive(w.unit) {
                    continue;
                }
                let (x, y) = cell(w.get_pos());
                war.bp_graphics.paint_circle(x, y, w.build_range, w.bp.round() as i32);
            }

            // paint ally threat under water, for whatever has water weapons
            war.ally_sub_graphics.clear();
            if war.water.wet {
                for f in fighters.values() {
                    let deep = war.deep_power(f.unit.def());
                    if deep > 0.0 && alive(f.unit) {
                        let (x, y) = cell(f.get_pos());
                        war.ally_sub_graphics.paint_circle(x, y, 8, ((deep + f.unit.max_health()) / 10.0) as i32);
                    }
                }
                for p in self.eco.dporcs.values().filter(|p| !p.is_dead()) {
                    if war.deep_power(p.unit.def()) > 0.0 {
                        let (x, y) = cell(p.get_pos());
                        war.ally_sub_graphics.paint_circle(x, y, p.radius, p.get_power());
                    }
                }
            }

            war.threat_graphics.clear();
            war.aa_threat_graphics.clear();
            war.riot_graphics.clear();
            war.sub_threat_graphics.clear();
            let planes = &self.unit_classes.planes;
            for t in war.targets.values_mut() {
                let mut effective_power = t.get_danger_mut() as i32;
                let position = t.position;
                let (x, y) = cell(position);
                if position != NULLPOS
                    && t.ud.is_some()
                    && effective_power > 0
                    && !planes.contains(t.def_name.as_str())
                    && !t.unit.is_being_built()
                    && !t.unit.is_paralyzed()
                    && t.unit.rules_param_float("disarmed", 0.0) == 0.0
                {
                    let mut r = (t.threat_radius / 64.0).round() as i32;
                    if t.def_name == "gunshipskirm" {
                        r *= 5;
                    }
                    // What it hits above water from where it is, and under
                    // water (see `water`).
                    let surface = t.surface_danger(t.is_submerged()) > 0.0;
                    let mut deep_power = t.deep_danger();
                    let rd = (t.deep_range() / 64.0).round() as i32;
                    if !t.is_static {
                        if !t.can_fly() {
                            // Reduce threat gradually for units that haven't been seen recently.
                            let mut time_mod = (frame - t.last_seen) as f32 / 1800.0;
                            time_mod *= time_mod;
                            time_mod = (1.0 - time_mod).max(0.0);
                            effective_power = (effective_power as f32 * time_mod) as i32;
                            deep_power *= time_mod;
                        }
                        // paint enemy threat for mobiles
                        if t.is_aa || t.is_flex_aa {
                            war.aa_threat_graphics.paint_circle(
                                x,
                                y,
                                (r as f32 * 1.25).round() as i32,
                                effective_power,
                            );
                        } else if surface {
                            if t.is_riot {
                                war.riot_graphics.paint_circle(x, y, r + 2, 1);
                            }
                            war.threat_graphics.paint_circle(x, y, r + 2, effective_power);
                        }
                        war.sub_threat_graphics.paint_circle(x, y, rd + 2, deep_power as i32);
                    } else if !t.is_aa {
                        // for statics
                        if surface {
                            if t.is_riot {
                                war.riot_graphics.paint_circle(x, y, r + 1, 1);
                            }
                            war.threat_graphics.paint_circle(x, y, r + 1, effective_power);
                        }
                        war.sub_threat_graphics.paint_circle(x, y, rd + 1, deep_power as i32);
                    }
                }
            }
            for &(p, radius, power) in &unseen_aa {
                let (x, y) = cell(p);
                war.aa_threat_graphics.paint_circle(x, y, (radius / 64.0).round() as i32 + 1, power as i32);
            }
        } else {
            war.ally_porc_graphics.clear();
            for p in self.eco.dporcs.values() {
                if p.is_dead() {
                    continue;
                }
                let (x, y) = cell(p.get_pos());
                war.ally_porc_graphics.paint_circle(x, y, p.radius, p.get_power());
            }

            war.enemy_porc_graphics.clear();
            war.sub_porc_graphics.clear();
            war.raider_value_graphics.clear();
            war.scythe_graphics.clear();
            let available_raider_threat = war.available_raider_threat;
            // Update porc threat and loot value for raiders.
            for t in war.targets.values_mut() {
                let position = t.position;
                let (x, y) = cell(position);
                if !t.identified || t.ud.is_none() || !t.can_fly() {
                    war.scythe_graphics.paint_circle(x, y, 4, 1);
                }
                if !t.identified || t.ud.is_none() || (!t.is_worker && !t.is_static) || t.is_com {
                    continue;
                }
                let mut value = t.value / 10.0;
                if t.is_porc {
                    value /= 2.0;
                    if !t.unit.is_being_built()
                        && !t.unit.is_paralyzed()
                        && t.unit.rules_param_float("disarmed", 0.0) == 0.0
                    {
                        // paint trimmed porc threat radii for workers and raiders to reference
                        // (torpedo turrets only for what is in water, see `water`)
                        let r = (t.threat_radius / 64.0).round() as i32;
                        let effective_power = t.get_danger_mut() as i32;
                        if t.surface_danger(false) > 0.0 {
                            war.enemy_porc_graphics.paint_circle(x, y, r - 1, effective_power);
                        }
                        let rd = (t.deep_range() / 64.0).round() as i32;
                        war.sub_porc_graphics.paint_circle(x, y, rd - 1, t.deep_danger() as i32);
                    }
                }
                let r = 7;

                let rthreat = war.threat_graphics.get_value(x, y) / 500.0 * (1.0 + war.riot_graphics.get_value(x, y));
                if rthreat > available_raider_threat {
                    continue;
                }
                // port: with no raider threat, 0/0 made the Java value NaN
                // (Math.max propagates it) so nothing was painted; C++'s
                // std::max dropped the NaN instead.
                if available_raider_threat == 0.0 {
                    continue;
                }
                let mut threat_mod = rthreat / available_raider_threat;
                threat_mod *= threat_mod;
                value *= (1.0 - threat_mod).max(0.5);
                war.raider_value_graphics.paint_circle(x, y, r, value.round() as i32);
            }
        }
    }

    /// Refresh enemy positions, drop stale contacts, expire defense targets
    /// and recount enemy composition.
    fn war_update_targets(&mut self) {
        let frame = self.war.frame;
        let mut outdated = Vec::new();
        {
            let war = &mut self.war;
            let los = &self.los;
            for (&id, t) in war.targets.iter_mut() {
                let tpos = t.unit.pos();
                if tpos != NULLPOS {
                    t.last_seen = frame;
                    t.position = tpos;
                } else if frame - t.last_seen > 1800 && !t.is_static {
                    // remove mobiles that haven't been seen for over 60 seconds.
                    outdated.push(id);
                } else if t.position != NULLPOS && los.is_in_los(t.position) {
                    // remove targets that aren't where we last saw them.
                    outdated.push(id);

                    // Unpaint enemy threat for aa
                    if (t.is_aa || t.is_flex_aa) && t.is_painted {
                        let (x, y, r, p) = aa_porc_circle(t);
                        war.aa_porc_graphics.unpaint_circle(x, y, r, p);
                    }
                }
            }
        }

        for id in outdated {
            self.bomber_remove_target(id);
            self.war.targets.remove(&id);
            self.war.enemy_porcs.remove(&id);
        }

        let metal = self.metal;
        let war = &mut self.war;
        war.sniper_sightings.retain(|d| frame - d.frame_issued <= 900);
        war.defense_targets.retain(|d| frame - d.frame_issued <= 300);
        war.air_defense_targets.retain(|d| frame - d.frame_issued <= 900);

        war.enemy_porc_value = 0.0;
        war.enemy_light_porc_value = 0.0;
        war.enemy_heavy_porc_value = 0.0;
        war.slasher_spam = 0;
        // add up the value of heavy porc that the enemy has
        // and also check for slasher spam
        war.enemy_has_anti_nuke = false;
        war.enemy_has_nuke = false;
        let mut enemy_air_value = 0.0f32;
        war.enemy_heavy_value = 0.0;
        war.enemy_fighter_value = 0.0;
        war.enemy_light_skirm_value = 0.0;
        war.enemy_brawler_value = 0.0;
        war.enemy_cloaky_value = 0.0;
        let (mut mobile_value, mut outranging_value) = (0.0f32, 0.0f32);
        for t in war.targets.values() {
            if !t.identified {
                continue;
            }
            let Some(ud) = t.ud else { continue };
            let name = t.def_name.as_str();
            let cost = ud.cost(metal);
            if !t.is_static && !t.is_worker && t.power > 0.0 && ud.speed() > 0.0 && frame - t.last_seen < 1800 {
                mobile_value += cost;
                if t.threat_radius >= OUTRANGES_RAIDERS {
                    outranging_value += cost;
                }
            }
            match name {
                "turretheavylaser" | "turretgauss" | "turretriot" | "turretemp" | "turretimpulse" | "spidercrabe" => {
                    war.enemy_porc_value += cost;
                    war.enemy_heavy_porc_value += cost;
                }
                "turretmissile" | "turretlaser" => {
                    war.enemy_porc_value += cost;
                    war.enemy_light_porc_value += cost;
                }
                "turretantiheavy" | "turretheavy" | "staticshield" | "shieldshield" => {
                    war.enemy_porc_value += 1.5 * cost;
                    war.enemy_heavy_porc_value += 1.5 * cost;
                }
                "shieldfelon" | "shieldassault" | "amphassault" => {
                    war.enemy_porc_value += 2.0 * cost;
                    war.enemy_light_porc_value += cost;
                    war.enemy_heavy_porc_value += cost;
                }
                "vehsupport" => war.slasher_spam += 1,
                _ if t.is_raider => war.slasher_spam -= 1,
                "staticantinuke" => war.enemy_has_anti_nuke = true,
                "staticnuke" => war.enemy_has_nuke = true,
                // port: the C++ compared against "strider" instead of the
                // Java's startsWith("strider").
                _ if name.starts_with("strider")
                    || matches!(
                        name,
                        "tankassault" | "tankheavyassault" | "amphassault" | "spidercrabe" | "jumpsumo" | "jumpassault"
                    ) =>
                {
                    war.enemy_heavy_value += cost;
                }
                _ if t.can_fly() && !t.is_nano_spam => {
                    if matches!(name, "dronelight" | "droneheavyslow" | "dronecarry") {
                        enemy_air_value += 25.0;
                    } else {
                        enemy_air_value += cost;
                    }
                }
                _ => {
                    if t.unit.rules_param_float("comm_level", 0.0) > 3.0 {
                        war.enemy_has_troll_com = true;
                    }
                }
            }

            if t.def_speed() > 0.0 && t.def_max_weapon_range() > 0.0 && t.def_builds_nothing() {
                war.enemy_fighter_value += cost;
                if !t.can_fly() {
                    if matches!(
                        name,
                        "cloakskirm"
                            | "shieldskirm"
                            | "spiderskirm"
                            | "hoverskirm"
                            | "jumpskirm"
                            | "vehsupport"
                            | "amphfloater"
                            | "shipskirm"
                    ) {
                        war.enemy_light_skirm_value += cost;
                    } else if t.is_raider
                        || t.is_riot
                        || matches!(
                            name,
                            "cloakassault"
                                | "shieldassault"
                                | "vehassault"
                                | "tankassault"
                                | "hoverassault"
                                | "spiderassault"
                                | "amphimpulse"
                                | "jumpassault"
                                | "jumpsumo"
                        )
                    {
                        war.enemy_brawler_value += cost;
                    }
                    if name.starts_with("cloak") {
                        war.enemy_cloaky_value += cost;
                    }
                }
            }
        }
        war.outranging_share = if mobile_value > 0.0 { outranging_value / mobile_value } else { 0.0 };

        war.max_enemy_air_value = enemy_air_value.max(war.max_enemy_air_value);

        war.target_markers.retain(|tm| frame - tm.frame <= 150);
    }

    /// Record `target` as attacked now (spreads attacks around) and return it.
    pub fn war_mark_target(&mut self, target: Vec3) -> Vec3 {
        let frame = self.war.frame;
        self.war.target_markers.push(TargetMarker::new(target, frame));
        target
    }

    /// Fallback target: the most promising enemy-shadowed metal spot cheaper
    /// than `cost` that `engage` accepts, or `NULLPOS`; and its cost.
    fn war_shadowed_spot_target(
        &self,
        origin: Vec3,
        mut cost: f32,
        water_targets_only: bool,
        engage: &dyn Fn(Vec3) -> bool,
    ) -> (Vec3, f32) {
        let frame = self.war.frame;
        let mut target = NULLPOS;
        for ms in self.graph.get_unowned_spots() {
            if !ms.enemy_shadowed {
                continue;
            }
            let p = ms.get_pos();
            if (water_targets_only && !self.war.is_water(p)) || !engage(p) {
                continue;
            }
            let mut tmpcost = distance(p, origin);
            tmpcost /= 1.0 + self.war.get_threat(p);
            // (Integer division, as in the original.)
            tmpcost /= ((frame - ms.get_last_seen()) / 600) as f32;

            if tmpcost < cost {
                target = p;
                cost = tmpcost;
            }
        }
        (target, cost)
    }

    /// Attack target for a mobile unit (or squad leader).
    pub fn war_get_target(&mut self, u: Unit, defend: bool) -> Vec3 {
        let (target, _) = self.war_best_target(u, u.pos(), defend);
        if target != NULLPOS {
            return self.war_mark_target(target);
        }
        NULLPOS
    }

    /// The best attack target for `u` if it were at `origin`, and its cost
    /// (lower is better; `NULLPOS` if none).
    pub fn war_best_target(&mut self, u: Unit, origin: Vec3, defend: bool) -> (Vec3, f32) {
        self.war_best_target_shunning(u, origin, defend, None, &[])
    }

    /// [`Ai::war_best_target`] of those under no more enemy porc than
    /// `porc_cap` (none: any) nor within 300 of `shun` (Ravager packs, which
    /// outrange no turret).
    pub fn war_best_target_shunning(
        &mut self,
        u: Unit,
        origin: Vec3,
        defend: bool,
        porc_cap: Option<f32>,
        shun: &[Vec3],
    ) -> (Vec3, f32) {
        let mut target = NULLPOS;
        // The threat maps as the unit sees them, under water too (see `water`).
        let war = &self.war;
        let view = war.view(u.def());
        // The strength around the unit goes with it.
        let fthreat = view.get_friendly_threat(u.pos());
        let mut cost = NO_COST;
        // What its weapons hit: torpedoes only what is in water, and under
        // water only water weapons.
        let hits = war.reach(u.def());
        let water_targets_only = !hits.dry_land;
        // Reachability for all candidates at once (one flood fill from the
        // unit, filled lazily; elsewhere the terrain test must do).
        let mut reach = None;
        let at_unit = distance(origin, u.pos()) < 1.0;
        // Where the terrain lets the unit get in range at all.
        let terrain = if self.dev_off("reach") { None } else { self.terrain.get() };
        let in_reach = crate::terrain::engage_test_from(terrain, u.def(), origin);
        let engage = |p: Vec3| {
            in_reach(p)
                && !porc_cap.is_some_and(|c| view.get_porc_threat(p) > c)
                && !shun.iter().any(|&s| distance(s, p) < 300.0)
        };

        // first check defense targets
        if defend {
            for d in war.all_defense_targets() {
                if (water_targets_only && !war.is_water(d.position))
                    || (d.under_water && !hits.under_water)
                    || !engage(d.position)
                {
                    continue;
                }
                if view.get_tactical_threat(d.position) > war.available_mobile_threat {
                    continue;
                }
                let tmpcost = distance(origin, d.position) - (d.damage * (1.0 + view.get_effective_threat(d.position)));
                if tmpcost < cost {
                    cost = tmpcost;
                    target = d.position;
                }
            }
        }

        // then look for enemy mexes to kill, and see which is cheaper
        for ms in self.graph.get_enemy_spots() {
            let p = ms.get_pos();
            // (Mexes float: anything hits one in reach.)
            if (water_targets_only && !war.is_water(p)) || !engage(p) {
                continue;
            }
            if view.get_threat(p) > fthreat
                || (at_unit
                    && !reach.get_or_insert_with(|| self.pathfinder.assault_reachability(war, u, fthreat)).reaches(p))
            {
                continue;
            }
            let mut tmpcost = (distance(p, origin) / 4.0) - 500.0;
            tmpcost += 500.0 * view.get_threat(p);
            if tmpcost < cost {
                target = p;
                cost = tmpcost;
            }
        }

        // then look for enemy units to kill, and see if any are better targets
        // then check for nearby enemies that aren't raiders.
        for e in war.targets.values() {
            if !war.can_hit(hits, e) || !engage(e.position) {
                continue;
            }
            let ally_territory = self.graph.is_ally_territory(e.position);
            let ethreat = view.get_effective_threat(e.position);
            if (ethreat > fthreat && (!ally_territory || view.get_porc_threat(e.position) > 0.0))
                || view.get_tactical_threat(e.position) > war.available_mobile_threat
                || (e.identified && e.can_fly())
                || ((!e.identified || e.is_raider) && distance(origin, e.position) > 1250.0)
            {
                continue;
            }
            let mut tmpcost = distance(origin, e.position);
            if !e.is_aa {
                if e.identified && !e.is_important && !e.is_raider && ally_territory {
                    tmpcost /= 2.0;
                    tmpcost -= e.get_danger() * (1.0 + view.get_threat(e.position));
                } else if e.is_important
                    && (!at_unit
                        || reach
                            .get_or_insert_with(|| self.pathfinder.assault_reachability(war, u, fthreat))
                            .reaches(e.position))
                {
                    tmpcost /= 4.0;
                    tmpcost -= 750.0;
                } else {
                    tmpcost -= e.get_danger();
                }
            }

            if tmpcost < cost {
                cost = tmpcost;
                target = e.position;
            }
        }

        // if no targets are available attack enemyshadowed metal spots until we find one
        if target != NULLPOS {
            return (target, cost);
        }
        self.war_shadowed_spot_target(origin, cost, water_targets_only, &engage)
    }

    pub fn war_get_arty_target(&mut self, origin: Vec3, defend: bool) -> Vec3 {
        let mut target = NULLPOS;
        let mut cost = NO_COST;
        let war = &self.war;

        // first check defense targets
        if defend {
            for d in &war.defense_targets {
                let tmpcost = distance(origin, d.position) - d.damage;
                if tmpcost < cost {
                    cost = tmpcost;
                    target = d.position;
                }
            }
        }

        // then look for enemy mexes to kill, and see which is cheaper
        for ms in self.graph.get_enemy_spots() {
            let p = ms.get_pos();
            let tmpcost = (distance(p, origin) / 4.0) - (500.0 * war.get_threat(p));
            if tmpcost < cost {
                target = p;
                cost = tmpcost;
            }
        }

        // then look for enemy units to kill, and see if any are better targets
        // then check for nearby enemies that aren't raiders.
        for e in war.targets.values() {
            if (e.identified && e.can_fly()) || e.is_raider {
                continue;
            }
            let mut tmpcost = distance(origin, e.position) - (500.0 * war.get_threat(e.position));

            if e.is_major_cancer {
                tmpcost /= 4.0;
                tmpcost -= 1000.0;
            } else if e.is_minor_cancer {
                tmpcost /= 2.0;
                tmpcost -= 500.0;
            }

            if self.graph.is_ally_territory(e.position) {
                tmpcost /= 2.0;
                tmpcost -= 250.0;
            }

            if tmpcost < cost {
                cost = tmpcost;
                target = e.position;
            }
        }

        if target != NULLPOS {
            return self.war_mark_target(target);
        }

        // if no targets are available attack enemyshadowed metal spots until we find one
        let (target, _) = self.war_shadowed_spot_target(origin, cost, false, &|_| true);
        if target != NULLPOS {
            return self.war_mark_target(target);
        }
        NULLPOS
    }

    pub fn war_get_air_target(&mut self, u: Unit, defend: bool) -> Vec3 {
        let origin = u.pos();
        let mut target = NULLPOS;
        let fthreat = self.war.get_friendly_threat(origin);
        let mut cost = NO_COST;
        let war = &self.war;
        // Reachability for all candidates at once (one flood fill, filled lazily).
        let mut reach = None;

        // first check defense targets
        if defend {
            for d in war.all_defense_targets() {
                if war.get_effective_aa_threat(d.position) > fthreat {
                    continue;
                }
                let tmpcost = distance(origin, d.position) - d.damage;
                if tmpcost < cost {
                    cost = tmpcost;
                    target = d.position;
                }
            }
        }

        // then look for enemy mexes to kill, and see which is cheaper
        for ms in self.graph.get_enemy_spots() {
            let p = ms.get_pos();
            if war.get_effective_aa_threat(p) > fthreat
                || !reach.get_or_insert_with(|| self.pathfinder.assault_reachability(war, u, fthreat)).reaches(p)
            {
                continue;
            }
            let tmpcost = (distance(p, origin) / 4.0) - 500.0;
            if tmpcost < cost {
                target = p;
                cost = tmpcost;
            }
        }

        // then look for enemy units to kill, and see if any are better targets
        for e in war.targets.values() {
            let ally_territory = self.graph.is_ally_territory(e.position);
            if e.position != NULLPOS && e.identified && war.get_effective_aa_threat(e.position) < fthreat {
                let mut tmpcost = distance(origin, e.position);
                if !e.is_aa {
                    if !e.is_raider && ally_territory {
                        tmpcost /= 1.0 + war.get_effective_threat(e.position).max(0.0);
                    }
                    tmpcost -= e.get_danger();
                }

                if e.is_major_cancer {
                    tmpcost /= 4.0;
                } else if e.is_minor_cancer {
                    tmpcost /= 2.0;
                }

                tmpcost += 2000.0 * war.get_aa_threat(e.position);

                if tmpcost < cost {
                    cost = tmpcost;
                    target = e.position;
                }
            }
        }

        if target != NULLPOS {
            return self.war_mark_target(target);
        }

        // if no targets are available attack enemyshadowed metal spots until we find one
        let (target, _) = self.war_shadowed_spot_target(origin, cost, false, &|_| true);
        if target != NULLPOS {
            return self.war_mark_target(target);
        }
        NULLPOS
    }

    pub fn war_get_krow_target(&mut self, origin: Vec3) -> Vec3 {
        let mut target = NULLPOS;
        let fthreat = self.war.get_friendly_threat(origin);
        let mut cost = NO_COST;
        let metal = self.metal;
        let war = &self.war;

        // then look for enemy mexes to kill, and see which is cheaper
        for ms in self.graph.get_enemy_spots() {
            let p = ms.get_pos();
            let mut tmpcost = distance(p, origin);
            tmpcost += 2000.0 * war.get_aa_threat(p);
            if tmpcost < cost && war.get_effective_aa_threat(p) < fthreat {
                target = p;
                cost = tmpcost;
            }
        }

        // then look for enemy units to kill, and see if any are better targets
        for e in war.targets.values() {
            let Some(ud) = e.ud else { continue };
            if e.position != NULLPOS && e.identified && !e.can_fly() {
                let ecost = ud.cost(metal);
                let mut tmpcost =
                    distance(origin, e.position) - if !e.is_aa { 10.0 * ecost.min(1500.0) } else { ecost };
                tmpcost += 10000.0 * war.get_aa_threat(e.position);

                if tmpcost < cost {
                    cost = tmpcost;
                    target = e.position;
                }
            }
        }

        if target != NULLPOS {
            return self.war_mark_target(target);
        }

        // if no targets are available attack enemyshadowed metal spots until we find one
        let (target, _) = self.war_shadowed_spot_target(origin, cost, false, &|_| true);
        if target != NULLPOS {
            return self.war_mark_target(target);
        }
        NULLPOS
    }

    /// Enemy ids (into `war.targets`) worth bombing.
    pub fn war_get_bomber_targets(&mut self) -> Vec<i32> {
        let ac = self.graph.get_ally_center();
        let war = &self.war;
        let mut result: Vec<(f32, i32)> = war
            .targets
            .iter()
            .filter(|(_, t)| !(!t.identified || t.ud.is_none() || (!t.is_static && t.value < 700.0) || t.is_nano_spam))
            .map(|(&id, t)| {
                let threat = 1.0 + war.get_aa_threat(t.position);
                let cost = distance(t.position, ac) * threat - t.value / threat;
                (cost, id)
            })
            .collect();
        result.sort_by(|a, b| a.0.total_cmp(&b.0));
        result.into_iter().map(|(_, id)| id).collect()
    }

    /// Target for an anti-air raider (fighter-store id).
    fn war_get_aa_target(&mut self, raider: i32, fallback: &mut Option<Vec3>) -> Vec3 {
        let Some(r) = self.fighters.get(&raider) else { return NULLPOS };
        let origin = r.get_pos();
        let range = r.unit.max_range();
        let mut target = NULLPOS;
        let mut cost = NO_COST;
        let war = &self.war;

        // first check defense targets
        for d in &war.air_defense_targets {
            if war.get_total_friendly_threat(d.position) == 0.0
                || war.get_porc_threat(get_directional_point(d.position, origin, range)) > 0.0
            {
                continue;
            }
            let tmpcost = distance(origin, d.position) - d.damage;
            if tmpcost < cost {
                cost = tmpcost;
                target = d.position;
            }
        }

        // then look for enemy air units to kill, and see if any are better targets
        for e in war.targets.values() {
            if !e.identified
                || !e.can_fly()
                || war.get_effective_threat(get_directional_point(e.position, origin, range)) > 0.0
                || war.get_porc_threat(e.position) > 0.0
            {
                continue;
            }
            let tmpcost = distance(origin, e.position) - e.get_danger();
            if tmpcost < cost {
                cost = tmpcost;
                target = e.position;
            }
        }

        if target == NULLPOS {
            target = *fallback.get_or_insert_with(|| {
                self.graph
                    .get_closest_front_line_spot(self.graph.get_enemy_center())
                    .map(|spot| spot.get_pos())
                    .map_or(NULLPOS, |pos| self.graph_get_closest_air_haven(pos))
            });
        }
        if target != NULLPOS {
            return self.war_mark_target(target);
        }
        NULLPOS
    }

    pub fn war_get_merlin_target(&mut self, u: Unit) -> Vec3 {
        let max_range = u.max_range();
        let upos = u.pos();
        let metal = self.metal;
        let mut cost = NO_COST;
        let mut target = NULLPOS;
        let war = &self.war;
        for e in war.targets.values() {
            let Some(ud) = e.ud else { continue };
            if !e.identified
                || e.can_fly()
                || e.is_raider
                || !self.graph.is_ally_territory(get_directional_point(e.position, upos, max_range))
            {
                continue;
            }
            let mut tmpcost = distance(e.position, upos);
            tmpcost += 250.0 * e.def_speed();
            tmpcost -= ud.cost(metal) * war.get_scythe_threat(e.position);
            if tmpcost < cost {
                cost = tmpcost;
                target = e.position;
            }
        }
        target
    }

    pub fn war_get_bertha_target(&mut self, origin: Vec3) -> Vec3 {
        let mut target = NULLPOS;
        // port: Java started from Float.MIN_VALUE (> 0); the C++ accepted any
        // first target, even with a negative score.
        let mut cost = JAVA_FLOAT_MIN_VALUE;
        let maxrange = self.defs.get("staticheavyarty").map_or(0.0, |d| d.max_weapon_range());
        let metal = self.metal;
        let war = &self.war;
        let frame = war.frame;
        // for berthas the more expensive the target the better
        for e in war.targets.values() {
            let Some(ud) = e.ud else { continue };
            if !e.identified || e.position == NULLPOS || e.is_nano_spam {
                continue;
            }
            // berthas can't target things outside their range, and are not good vs air.
            if distance(e.position, origin) > maxrange || e.can_fly() {
                continue;
            }
            let name = e.def_name.as_str();
            if (name == "staticantinuke" && !war.has_nuke) || e.is_com {
                // we also ignore antinuke unless we actually have nukes.
                continue;
            }
            let mut tmpcost =
                (ud.cost(metal) * war.get_scythe_threat(e.position)) - ((frame - e.last_seen) as f32 / 2.0);
            if name.contains("factory") || name.contains("hub") || name.contains("felon") {
                tmpcost += 800.0;
                if name.contains("hub") {
                    tmpcost *= 2.0;
                }
            }
            // An anti-nuke is worth what it alone keeps from our nukes too
            // (its value beyond its cost; see `nukes`).
            if name == "staticantinuke" {
                tmpcost += (e.value - ud.cost(metal)).max(0.0);
            }
            if tmpcost > cost {
                cost = tmpcost;
                target = e.position;
            }
        }
        if target != NULLPOS {
            return self.war_mark_target(target);
        }
        NULLPOS
    }

    pub fn war_get_super_wep_target(&mut self, u: Unit, cook: bool) -> Vec3 {
        let frame = self.war.frame;
        if frame - self.war.last_value_redraw_frame > 350 {
            let wep_name = self.defs.name_of(u).to_owned();
            self.war_paint_value_map(&wep_name);
            self.war.last_value_redraw_frame = frame;
        }
        let origin = u.pos();
        let range = u.max_range();
        let mut target = NULLPOS;
        // port: Java started from Float.MIN_VALUE (> 0); the C++ started from
        // -65535 and so also fired at worthless (value <= 0) targets.
        let mut cost = JAVA_FLOAT_MIN_VALUE;
        let war = &self.war;

        for e in war.targets.values() {
            if !e.identified
                || e.position == NULLPOS
                || distance(e.position, origin) > range
                || e.can_fly()
                || self.graph.is_ally_territory(e.position)
            {
                // superweapons should avoid teamkilling.
                continue;
            }
            let tmpcost = war.get_value(e.position);
            if tmpcost > cost {
                cost = tmpcost;
                target = e.position;
            }
        }
        if target != NULLPOS {
            self.war_mark_target(target);
            if cook {
                self.war_cook();
            }
            return target;
        }
        NULLPOS
    }

    fn war_cook(&self) {
        let rnd = kgb_rand();
        if rnd > 0.8 {
            self.say("Nice knowing you.");
        } else if rnd > 0.6 {
            self.say("It's cooking time!");
        } else if rnd > 0.4 {
            self.say("SHINY!");
        } else if rnd > 0.2 {
            self.say("R.I.P.");
        } else {
            self.say("Get rekt.");
        }
    }

    /// Origin for rally searches: `start`, else the ally center, else any
    /// friendly unit; `None` if there is nothing at all.
    fn war_rally_origin(&self, start: Vec3) -> Option<Vec3> {
        if start != NULLPOS {
            return Some(start);
        }
        let pos = self.graph.get_ally_center();
        if pos != NULLPOS {
            return Some(pos);
        }
        self.cb.friendly_units().first().map(|u| u.pos())
    }

    pub fn war_get_rally_point(&mut self, pos: Vec3) -> Vec3 {
        let Some(pos) = self.war_rally_origin(pos) else { return NULLPOS };
        let war = &self.war;
        let fthreat = war.get_friendly_threat(pos);
        let mut target = NULLPOS;
        let mut cost = 0.0;
        let mut found = false;

        // check for defense targets first
        for d in war.all_defense_targets() {
            if war.get_porc_threat(d.position) > 0.0
                || war.get_tactical_threat(d.position) > war.available_mobile_threat
                || !self.graph.is_ally_territory(d.position)
            {
                continue;
            }
            let tmpcost = distance(pos, d.position) - d.damage;
            if !found || tmpcost < cost {
                found = true;
                cost = tmpcost;
                target = d.position;
            }
        }

        // then check for nearby enemies
        for e in war.targets.values() {
            let ally_territory = self.graph.is_ally_territory(e.position);
            let ethreat = war.get_effective_threat(e.position);
            let pthreat = war.get_porc_threat(e.position);
            if (ethreat > fthreat && (!ally_territory || pthreat > 0.0))
                || war.get_tactical_threat(e.position) > war.available_mobile_threat
                || (e.identified && e.can_fly())
                || ((!e.identified || e.is_raider) && distance(pos, e.position) > 800.0)
                || (!ally_territory || pthreat > 0.0)
            {
                continue;
            }
            let mut tmpcost = distance(pos, e.position);
            if !e.is_aa {
                if e.identified && !e.is_raider {
                    tmpcost /= 2.0;
                    tmpcost -= e.get_danger() * (1.0 + war.get_threat(e.position));
                } else {
                    tmpcost -= e.get_danger();
                }
            }
            if !found || tmpcost < cost {
                found = true;
                cost = tmpcost;
                target = e.position;
            }
        }
        if found {
            return target;
        }

        // The operator's line, if one is set: the army stands there when
        // nothing calls it (instead of escorting workers or the front spots).
        if let Some(line) = self.op_line_point() {
            return line;
        }
        //if there aren't any, then rally near the front line.
        let position = self.war_get_worker_escort(pos);
        if position != NULLPOS {
            return position;
        }
        if let Some(spot) = self.graph.get_closest_front_line_spot(pos)
            && spot.get_pos() != NULLPOS
        {
            return spot.get_pos();
        }

        let position = self.graph_get_closest_leading_link(pos);
        if position != NULLPOS {
            return position;
        }

        //otherwise rally near ally center.
        let position = self.graph_get_closest_haven(self.graph.get_ally_center());
        if position != NULLPOS {
            return position;
        }
        let position = self.graph.get_ally_center();
        if position != NULLPOS {
            return position;
        }

        self.say("getRallyPoint is fukt.");
        if let Some(spot) = self.graph.get_closest_spot(pos)
            && spot.get_pos() != NULLPOS
        {
            return spot.get_pos();
        }
        pos
    }

    pub fn war_get_air_rally_point(&mut self, pos: Vec3) -> Vec3 {
        let Some(pos) = self.war_rally_origin(pos) else { return NULLPOS };
        let war = &self.war;
        let fthreat = war.get_friendly_threat(pos);
        let mut target = NULLPOS;
        let mut cost = 0.0;
        let mut found = false;

        // check for defense targets first
        for d in war.all_defense_targets() {
            if war.get_effective_aa_threat(d.position) > fthreat || war.get_porc_threat(d.position) > 0.0 {
                continue;
            }
            let mut tmpcost = distance(pos, d.position) - d.damage;
            tmpcost += 2000.0 * war.get_aa_threat(d.position);
            if !found || tmpcost < cost {
                found = true;
                cost = tmpcost;
                target = d.position;
            }
        }

        // then check for nearby enemies
        for e in war.targets.values() {
            if war.get_effective_aa_threat(e.position) > fthreat
                || !self.graph.is_ally_territory(e.position)
                || war.get_porc_threat(e.position) > 0.0
            {
                continue;
            }
            let mut tmpcost = distance(pos, e.position);
            if !e.is_aa {
                tmpcost -= e.get_danger() * (1.0 + war.get_effective_threat(e.position).max(0.0));
            }
            tmpcost += 2000.0 * war.get_aa_threat(e.position);
            if e.is_major_cancer {
                tmpcost /= 4.0;
                tmpcost -= 1000.0;
            } else if e.is_minor_cancer {
                tmpcost /= 2.0;
                tmpcost -= 500.0;
            }
            if !found || tmpcost < cost {
                found = true;
                cost = tmpcost;
                target = e.position;
            }
        }
        if found {
            return target;
        }

        //if there aren't any, then rally near the front line.
        let position = self.war_get_worker_escort(pos);
        if position != NULLPOS {
            return position;
        }

        if let Some(spot) = self.graph.get_closest_front_line_spot(pos)
            && spot.get_pos() != NULLPOS
        {
            return self.graph_get_closest_air_haven(spot.get_pos());
        }

        let position = self.graph_get_closest_air_leading_link(pos);
        if position != NULLPOS {
            return position;
        }

        let position = self.graph.get_ally_center();
        if position != NULLPOS {
            return position;
        }
        if let Some(spot) = self.graph.get_closest_spot(pos)
            && spot.get_pos() != NULLPOS
        {
            return spot.get_pos();
        }
        pos
    }

    pub fn war_get_raider_rally(&mut self, pos: Vec3) -> Vec3 {
        self.war_get_raider_rally_skipping(pos, &[])
    }

    /// [`Ai::war_get_raider_rally`], passing over defense targets and
    /// enemies within `CLAIM_RADIUS` of `handled` (where other raiders are
    /// going with enough for what is there, see `raider_handled`).
    pub fn war_get_raider_rally_skipping(&mut self, pos: Vec3, handled: &[Vec3]) -> Vec3 {
        let skip = |p: Vec3| handled.iter().any(|&h| distance(h, p) < handlers::raider::CLAIM_RADIUS);
        let Some(pos) = self.war_rally_origin(pos) else { return NULLPOS };
        let war = &self.war;
        let fthreat = war.get_friendly_threat(pos);
        let no_assaults = self.squad_handler.fighters.is_empty();
        let mut target = NULLPOS;
        let mut cost = 0.0;
        let mut found = false;

        // check for defense targets first
        for d in war.all_defense_targets() {
            if skip(d.position) {
                continue;
            }
            if ((war.get_effective_threat(d.position) > fthreat
                || war.get_porc_threat(d.position) > 0.0
                || war.get_riot_threat(d.position) > 0.0)
                && !no_assaults)
                || !self.graph.is_ally_territory(d.position)
            {
                continue;
            }
            let tmpcost = distance(pos, d.position) - d.damage;
            if !found || tmpcost < cost {
                found = true;
                cost = tmpcost;
                target = d.position;
            }
        }

        // then check for nearby enemies
        for e in war.targets.values() {
            if skip(e.position) {
                continue;
            }
            let ethreat = (war.get_threat(e.position) * (1.0 + war.get_riot_threat(e.position)))
                - war.get_total_friendly_threat(e.position);
            if ((ethreat > fthreat || (e.identified && e.can_fly())) && !no_assaults)
                || (!self.graph.is_ally_territory(e.position) || war.get_porc_threat(e.position) > 0.0)
            {
                continue;
            }
            let mut tmpcost = distance(pos, e.position) - e.get_danger();
            if e.is_major_cancer {
                tmpcost /= 4.0;
                tmpcost -= 1000.0;
            } else if e.is_minor_cancer || (e.is_aa && e.def_name != "turretaalaser") {
                tmpcost /= 2.0;
                tmpcost -= 500.0;
            }
            if !found || tmpcost < cost {
                found = true;
                cost = tmpcost;
                target = e.position;
            }
        }

        if found {
            return target;
        }

        let position = self.war_get_worker_escort(pos);
        if position != NULLPOS {
            return position;
        }
        if let Some(spot) = self.graph.get_closest_front_line_spot(pos)
            && spot.get_pos() != NULLPOS
        {
            return spot.get_pos();
        }

        //if there aren't any, then rally near the front line.
        let position = self.graph_get_closest_leading_link(pos);
        if position != NULLPOS {
            return position;
        }

        //otherwise rally near ally center.
        let position = self.graph_get_closest_haven(self.graph.get_ally_center());
        if position != NULLPOS {
            return position;
        }

        let position = self.graph.get_ally_center();
        if position != NULLPOS {
            return position;
        }
        if let Some(spot) = self.graph.get_closest_spot(pos)
            && spot.get_pos() != NULLPOS
        {
            return spot.get_pos();
        }
        pos
    }

    /// A buildable spot next to the safest worker closest to the enemy, for
    /// rallying units to escort workers.
    fn war_get_worker_escort(&self, pos: Vec3) -> Vec3 {
        let war = &self.war;
        let mut target = NULLPOS;
        let mut mindist = 0.0;
        let mut found = false;
        let mut enemy_center = self.graph.get_enemy_center();
        if enemy_center == NULLPOS {
            enemy_center = pos;
        }
        let num_workers = self.eco.workers.len();
        for w in self.eco.workers.values() {
            let wpos = w.get_pos();
            let ethreat = war.get_effective_threat(wpos);
            if ethreat > war.available_mobile_threat || (w.is_com && num_workers > 1) {
                continue;
            }
            let dist = distance(wpos, enemy_center) + (1000.0 * (war.get_total_friendly_threat(wpos) - ethreat));
            if !found || dist < mindist {
                found = true;
                target = wpos;
                mindist = dist;
            }
        }
        if target == NULLPOS {
            return self.graph.get_ally_center();
        }
        let near = get_angular_point(target, self.graph.get_map_center(), 250.0);
        match self.defs.get("mahlazer") {
            Some(def) => self.cb.map().find_closest_build_site(def, near, 600.0, 3, 0),
            None => near,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ascii_value_is_logarithmic_and_bounded() {
        assert_eq!(ascii_value(0, 32767), '.');
        assert_eq!(ascii_value(1, 127), '.');
        assert_eq!(ascii_value(2, 127), '0');
        assert_eq!(ascii_value(127, 127), '9');
        assert_eq!(ascii_value(32767, 32767), '9');
    }

    #[test]
    fn cell_rounds_to_the_64_elmo_grid() {
        assert_eq!(cell(Vec3::xz(95.0, 31.0)), (1, 0));
        assert_eq!(cell(Vec3::xz(96.0, 32.0)), (2, 1));
        assert_eq!(cell(NULLPOS), (0, 0));
    }

    const _: () = assert!(JAVA_FLOAT_MIN_VALUE > 0.0);

    #[test]
    fn java_min_value_is_smallest_positive_float() {
        assert_eq!(JAVA_FLOAT_MIN_VALUE, 1.4e-45_f32);
    }
}
