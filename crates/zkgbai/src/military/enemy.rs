//! A tracked enemy unit (seen in LOS or on radar).

use std::cell::Cell;

use springai::{Unit, UnitDef, Vec3};

use super::radar::RadarDef;
use super::water::{is_fake, is_floater};
use super::wrappers::weapon_power;

/// Under water, a contact of unknown type is below this height (the sea is
/// at 0), elmos.
const UNKNOWN_SUBMERGED: f32 = -10.0;

pub struct Enemy {
    pub unit: Unit,
    pub unit_id: i32,
    /// `None` until identified (radar-only contacts).
    pub ud: Option<UnitDef>,
    pub position: Vec3,
    pub threat_radius: f32,
    pub power: f32,
    pub value: f32,
    pub last_seen: i32,
    pub is_painted: bool,
    pub visible: bool,
    pub is_static: bool,
    pub is_important: bool,
    pub is_radar_only: bool,
    pub is_radar_visible: bool,
    pub identified: bool,
    pub is_worker: bool,
    pub is_riot: bool,
    pub is_raider: bool,
    pub is_arty: bool,
    pub is_aa: bool,
    pub is_flex_aa: bool,
    pub is_porc: bool,
    pub is_mex: bool,
    pub is_com: bool,
    pub is_super_wep: bool,
    pub is_minor_cancer: bool,
    pub is_major_cancer: bool,
    pub is_nano_spam: bool,
    pub is_strong: bool,
    pub is_dead: bool,
    /// Internal name of `ud` (`""` while unidentified), cached so hot loops
    /// need no engine string queries.
    pub def_name: String,
    speed: f32,
    last_health: Cell<f32>,
    is_flamer: bool,
    // Immutable unit-def properties cached at identification (hot paths).
    can_fly: bool,
    can_attack: bool,
    def_is_builder: bool,
    def_health: f32,
    def_speed: f32,
    def_max_weapon_range: f32,
    def_builds_nothing: bool,
    def_radius: f32,
    /// Whether it has a weapon for targets above water (not a torpedo).
    hits_surface: bool,
    /// Strength and reach of its water weapons (see [`super::water`]).
    deep_power: f32,
    deep_range: f32,
    /// Rises to the surface to fire (ZK's float toggle).
    floater: bool,
}

impl Enemy {
    /// `cost` is the unit's metal cost (if its def is known).
    pub fn new(unit: Unit, cost: f32) -> Self {
        let mut e = Enemy {
            unit,
            unit_id: unit.id(),
            ud: None,
            position: unit.pos(),
            threat_radius: 0.0,
            power: 0.0,
            value: 0.0,
            last_seen: 0,
            is_painted: false,
            visible: false,
            is_static: false,
            is_important: false,
            is_radar_only: true,
            is_radar_visible: false,
            identified: false,
            is_worker: false,
            is_riot: false,
            is_raider: false,
            is_arty: false,
            is_aa: false,
            is_flex_aa: false,
            is_porc: false,
            is_mex: false,
            is_com: false,
            is_super_wep: false,
            is_minor_cancer: false,
            is_major_cancer: false,
            is_nano_spam: false,
            is_strong: false,
            is_dead: false,
            def_name: String::new(),
            speed: 0.0,
            last_health: Cell::new(0.0),
            is_flamer: false,
            can_fly: false,
            can_attack: false,
            def_is_builder: false,
            def_health: 0.0,
            def_speed: 0.0,
            def_max_weapon_range: 0.0,
            def_builds_nothing: true,
            def_radius: 0.0,
            hits_surface: false,
            deep_power: 0.0,
            deep_range: 0.0,
            floater: false,
        };
        match unit.def() {
            Some(def) => {
                e.update_from_unit_def(def, cost);
                e.check_nano();
            }
            None => {
                e.value = 50.0;
                e.is_static = false;
            }
        }
        e
    }

    pub fn check_nano(&mut self) {
        self.is_nano_spam = self.unit.is_being_built() && self.unit.build_progress() < 0.25;
    }

    pub fn update_from_unit_def(&mut self, ud: UnitDef, cost: f32) {
        if self.ud.is_some() {
            return;
        }
        self.identified = true;
        self.value = cost;
        self.def_speed = ud.speed();
        self.is_static = self.def_speed == 0.0;
        self.ud = Some(ud);
        self.can_fly = ud.is_able_to_fly();
        self.can_attack = ud.is_able_to_attack();
        self.def_is_builder = ud.is_builder();
        self.def_health = ud.health();
        self.def_max_weapon_range = ud.max_weapon_range();
        self.def_builds_nothing = ud.build_options().is_empty();
        self.def_radius = ud.radius();

        if ud.build_speed() > 0.0 {
            self.is_worker = true;
        }

        let def_name = ud.name();
        let name = def_name.as_str();

        if name.starts_with("dyn") {
            self.is_com = true;
        }

        if matches!(name, "veharty" | "vehheavyarty" | "cloaksnipe" | "hoverarty" | "amphfloater" | "spiderskirm") {
            self.is_minor_cancer = true;
        }

        if matches!(
            name,
            "striderdante"
                | "striderscorpion"
                | "striderfunnelweb"
                | "striderbantha"
                | "striderdetriment"
                | "amphassault"
                | "striderarty"
                | "spidercrabe"
                | "tankheavyassault"
                | "tankassault"
                | "shieldfelon"
                | "vehcapture"
        ) || self.is_com
        {
            self.is_major_cancer = true;
        }

        let tooltip = ud.tooltip();
        if tooltip.contains("Anti-Air") {
            self.is_aa = true;
        }

        if matches!(
            name,
            "staticheavyarty" | "staticnuke" | "tacnuke" | "napalmmissile" | "raveparty" | "zenith" | "mahlazer"
        ) {
            self.is_super_wep = true;
        }

        let mounts = ud.weapon_mounts();
        if !mounts.is_empty() {
            self.threat_radius = self.def_max_weapon_range;
            if self.is_static && !self.is_aa && !self.is_super_wep {
                self.is_porc = true;
            }

            self.floater = is_floater(ud);
            for wd in mounts.iter().filter_map(|m| m.weapon_def()) {
                if is_fake(&wd.name()) {
                    continue;
                }
                if wd.weapon_type() != "TorpedoLauncher" || wd.is_sub_missile() {
                    self.hits_surface = true;
                }
                if wd.is_water_weapon() {
                    self.deep_power += weapon_power(wd, true);
                    self.deep_range = self.deep_range.max(wd.range());
                }
            }
            if self.is_com {
                self.hits_surface = true;
                self.power = 300.0;
            } else {
                for wd in mounts.iter().filter_map(|m| m.weapon_def()) {
                    let wd_name = wd.name();
                    if (!wd_name.contains("torpedo") || self.is_static) && !wd_name.contains("fake") {
                        // take the max between burst damage and continuous dps as weapon power.
                        if wd.custom_param("setunitsonfire").is_some() {
                            self.is_flamer = true;
                        }
                        self.power += weapon_power(wd, true);
                    }
                }
            }
        }

        if tooltip.contains("Riot")
            || tooltip.contains("Anti-Swarm")
            || matches!(name, "shieldfelon" | "turretemp" | "turretheavy" | "turretaaheavy" | "turretaaflak")
            || self.is_com
        {
            // identify riots
            self.is_riot = true;
        }

        if name == "turretheavylaser" || name == "amphraid" {
            self.is_strong = true;
        }

        if name.contains("factory")
            || name.contains("hub")
            || name == "energyfusion"
            || name == "energysingu"
            || self.is_com
        {
            self.is_important = true;
        }

        if tooltip.contains("aider") || tooltip.contains("cout") {
            // this is for keeping assault mobs from chasing raiders around.
            self.is_raider = true;
            if !name.starts_with("amph") {
                self.is_riot = false; // count archers as riots but not pyros.
            }
        }

        if (tooltip.contains("Arti") || tooltip.contains("Skirm") || name == "vehsupport")
            || (self.def_is_builder && !tooltip.contains("Riot"))
        {
            // identify arty/skirms
            self.is_arty = true;
        }

        if matches!(name, "gunshipskirm" | "gunshipheavyskirm" | "gunshipkrow" | "jumpskirm" | "turretemp") {
            self.is_flex_aa = true;
        }

        if name == "staticmex" {
            self.is_mex = true;
        }
        self.speed = self.def_speed / 30.0;
        self.def_name = def_name;
    }

    pub fn update_from_radar_def(&mut self, rd: &RadarDef) {
        self.identified = true;
        self.value = rd.get_value();
        self.is_static = rd.get_speed() == 0.0;
        self.speed = rd.get_speed();
        self.threat_radius = rd.get_range();
    }

    /// Threat estimate: (weapon power + health) / 10, reduced for skirmishers
    /// and builders; 0 for unarmed units, superweapons and unknown types.
    ///
    /// The original also flagged high-level commanders as strong here; that
    /// needs `&mut self`, see [`Enemy::get_danger_mut`].
    pub fn get_danger(&self) -> f32 {
        self.danger_of(self.power)
    }

    /// [`Enemy::get_danger`] with weapons of strength `power`.
    fn danger_of(&self, power: f32) -> f32 {
        if self.ud.is_none() || !self.can_attack || self.is_super_wep {
            return 0.0;
        }
        let health = self.current_health();
        let mut danger = (power + health) / 10.0;
        if (self.is_arty && self.def_name != "amphfloater") || self.def_is_builder {
            danger /= 3.0;
        }
        danger
    }

    /// Danger to what is above water: none from torpedoes alone, nor from
    /// under water (`submerged`) unless it is a floater, which rises to
    /// fire.
    pub fn surface_danger(&self, submerged: bool) -> f32 {
        if !self.hits_surface || (submerged && !self.floater) { 0.0 } else { self.get_danger() }
    }

    /// Danger to what is under water (from its water weapons).
    pub fn deep_danger(&self) -> f32 {
        if self.deep_power > 0.0 { self.danger_of(self.deep_power) } else { 0.0 }
    }

    /// Whether it is all under water (not flying), where last seen: its
    /// middle plus radius below the surface, as the engine has it.
    pub fn is_submerged(&self) -> bool {
        if self.can_fly {
            return false;
        }
        match self.def_radius() {
            Some(r) => self.position.y + r < 0.0,
            None => self.position.y < UNKNOWN_SUBMERGED,
        }
    }

    /// Range of its water weapons, elmos (0 without any).
    pub fn deep_range(&self) -> f32 {
        self.deep_range
    }

    /// `ud.radius()`, `None` while unidentified.
    pub fn def_radius(&self) -> Option<f32> {
        self.ud.map(|_| self.def_radius)
    }

    /// [`Enemy::get_danger`], also refreshing `is_strong` for upgraded
    /// commanders exactly like the original `getDanger`.
    pub fn get_danger_mut(&mut self) -> f32 {
        if self.ud.is_some()
            && self.can_attack
            && !self.is_super_wep
            && self.unit.rules_param_float("comm_level", 0.0) > 1.0
        {
            self.is_strong = true;
        }
        self.get_danger()
    }

    /// Fraction of max health left (1 if unidentified).
    pub fn get_health(&self) -> f32 {
        if !self.identified || self.ud.is_none() || self.def_health <= 0.0 {
            return 1.0;
        }
        self.current_health() / self.def_health
    }

    /// Current health, falling back to the last observed value when the unit
    /// is out of sight, and to the def's max health if never observed.
    fn current_health(&self) -> f32 {
        let h = self.unit.health();
        if h > 0.0 {
            self.last_health.set(h);
            h
        } else if self.last_health.get() > 0.0 {
            self.last_health.get()
        } else {
            self.def_health
        }
    }

    /// Speed in elmos per frame (from the def or the radar guess).
    pub fn speed(&self) -> f32 {
        self.speed
    }

    /// Whether any weapon sets units on fire.
    pub fn is_flamer(&self) -> bool {
        self.is_flamer
    }

    /// `ud.is_able_to_fly()`, false while unidentified.
    pub fn can_fly(&self) -> bool {
        self.can_fly
    }

    /// `ud.is_able_to_attack()`, false while unidentified.
    pub fn can_attack(&self) -> bool {
        self.can_attack
    }

    /// `ud.speed()` (elmos/s), 0 while unidentified.
    pub fn def_speed(&self) -> f32 {
        self.def_speed
    }

    /// `ud.health()`, 0 while unidentified.
    pub fn def_health(&self) -> f32 {
        self.def_health
    }

    /// `ud.max_weapon_range()`, 0 while unidentified.
    pub fn def_max_weapon_range(&self) -> f32 {
        self.def_max_weapon_range
    }

    /// `ud.build_options().is_empty()` (true while unidentified).
    pub fn def_builds_nothing(&self) -> bool {
        self.def_builds_nothing
    }
}
