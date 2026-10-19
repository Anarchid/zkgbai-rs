//! A unit that retreats to heal when damaged (owned by the retreat handler).

use springai::{NO_TIMEOUT, Unit, UnitDef, Vec3, Weapon};

use crate::defs::cmd;
use crate::kgbutil::{Nav, PathCost, kgb_move_to};
use crate::util::OPTION_SHIFT_KEY;

pub struct Coward {
    pub id: i32,
    pub unit: Unit,
    /// The unit's shield weapon, if it has one.
    pub shield: Option<Weapon>,
    /// Full power of `shield` (from its weapon def).
    shield_max_power: f32,

    pub small_raider: bool,
    pub is_retreating: bool,
    pub is_scythe: bool,
    pub is_plane: bool,
    pub is_flyer: bool,
    pub hold_fire: bool,
    pub is_strider: bool,

    /// Health fraction below which the unit retreats.
    retreat_level: f32,
    team: i32,
}

impl Coward {
    /// `def`/`def_name` are the unit's definition and its internal name;
    /// `retreat_level` is the health fraction below which it retreats.
    pub fn new(unit: Unit, def: UnitDef, def_name: &str, retreat_level: f32, team: i32) -> Self {
        let mut shield = None;
        let mut shield_max_power = 0.0;
        for w in unit.weapons() {
            if let Some(wd) = w.def() {
                let power = wd.shield_power();
                if power > 0.0 {
                    shield = Some(w);
                    shield_max_power = power;
                    break;
                }
            }
        }
        Coward {
            id: unit.id(),
            unit,
            shield,
            shield_max_power,
            small_raider: false,
            is_retreating: false,
            is_scythe: false,
            is_plane: false,
            is_flyer: def.is_able_to_fly(),
            hold_fire: def_name == "hoverassault",
            is_strider: def_name.starts_with("strider")
                || def_name == "gunshipkrow"
                || def_name == "tankassault"
                || def_name == "tankheavyassault",
            retreat_level,
            team,
        }
    }

    pub fn retreat_to(&mut self, pos: Vec3, nav: &mut Nav) {
        let mut path = nav.pathfinder.find_path(nav.war, self.unit, pos, PathCost::AvoidEnemies);
        if path.len() > 1 && self.is_flyer {
            path.pop_front();
        }
        if path.len() > 1 && self.is_flyer {
            path.pop_front();
        }
        let Some(first) = path.pop_front() else {
            return;
        };
        kgb_move_to(self.unit, first, 0, nav.frame);

        // Queue up to the first 3 waypoints.
        let path_size = path.len().min(3);
        let mut i = 0;
        while i < path_size && !path.is_empty() {
            // Skip every other waypoint except the last, since they're not very far apart.
            if path.len() > 1 {
                path.pop_front();
            }
            // Skip two out of three waypoints for flying units, since they move quickly.
            if path.len() > 1 && self.is_flyer {
                path.pop_front();
            }
            if let Some(wp) = path.pop_front() {
                kgb_move_to(self.unit, wp, OPTION_SHIFT_KEY, nav.frame);
            }
            i += 1;
        }
    }

    pub fn find_pad(&self) {
        self.unit.execute_custom_command(cmd::FIND_PAD, &[], 0, NO_TIMEOUT);
    }

    /// Whether the unit is hurt (or being captured / paralyzed) enough to
    /// retreat; updates `is_retreating`.
    pub fn should_retreat(&mut self) -> bool {
        let u = self.unit;
        let retreat = u.health() / u.max_health() < self.retreat_level
            || 1.0 - u.capture_progress() < self.retreat_level
            || (self.is_strider && 1.0 - (u.paralyze_damage() / u.health()) < self.retreat_level);
        if retreat && self.hold_fire {
            u.set_fire_state(0, 0);
        }
        self.is_retreating = retreat;
        retreat
    }

    /// Whether the unit is fully repaired (and its shield recharged); updates
    /// `is_retreating`.
    pub fn is_healed(&mut self) -> bool {
        let u = self.unit;
        let healed = u.health() == u.max_health()
            && u.capture_progress() == 0.0
            && u.paralyze_damage() == 0.0
            && self.shield.is_none_or(|s| s.shield_power() > 0.5 * self.shield_max_power);
        if healed && self.hold_fire {
            u.set_fire_state(2, 0);
        }
        self.is_retreating = !healed;
        healed
    }

    /// Whether the shield is depleted enough to retreat; updates
    /// `is_retreating`. Units without a shield never retreat for this.
    pub fn check_shield(&mut self) -> bool {
        let Some(s) = self.shield else {
            return false;
        };
        let retreat = s.shield_power() < 0.25 * self.shield_max_power;
        self.is_retreating = retreat;
        retreat
    }

    pub fn is_dead(&self) -> bool {
        self.unit.health() <= 0.0 || self.unit.team() != self.team
    }

    pub fn get_pos(&self) -> Vec3 {
        self.unit.pos()
    }

    pub fn get_unit(&self) -> Unit {
        self.unit
    }

    pub fn is_cloaked(&self) -> bool {
        self.unit.is_cloaked()
    }
}
