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
    /// The squad's courage (see `Squad::courage`); `base_courage` outside
    /// a squad.
    pub courage: f32,
    /// Courage outside a squad: 1, less for units that should run at the
    /// first scratch (pickets that cannot cloak, see `stakeouts`).
    pub base_courage: f32,
    team: i32,
}

impl Coward {
    /// Spotters repair below 60%, including Scythes while uncloaked, and
    /// never resume firing when healed (a Halberd must keep its armour).
    pub(crate) fn new_spotter(unit: Unit, def: UnitDef, def_name: &str, team: i32) -> Self {
        let mut c = Self::new(unit, def, def_name, 0.6, team);
        // `hold_fire` toggles fire at will back on after repair for ordinary
        // Halberds. A spotter stays on the hold-fire state it was given.
        c.hold_fire = false;
        c
    }

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
            courage: 1.0,
            base_courage: 1.0,
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
        let level = (self.retreat_level / self.courage.max(0.01)).min(0.95);
        let retreat = u.health() / u.max_health() < level
            || 1.0 - u.capture_progress() < level
            || (self.is_strider && 1.0 - (u.paralyze_damage() / u.health()) < level);
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

#[cfg(test)]
mod tests {
    use super::*;
    use springai::ffi::SSkirmishAICallback;
    use std::cell::Cell;
    use std::ffi::c_void;

    #[test]
    fn spotter_repairs_at_sixty_percent_and_never_resumes_fire() {
        thread_local! {
            static HEALTH: Cell<f32> = const { Cell::new(1.0) };
            static ORDERS: Cell<usize> = const { Cell::new(0) };
        }
        unsafe extern "C" fn health(_: i32, _: i32) -> f32 {
            HEALTH.with(Cell::get)
        }
        unsafe extern "C" fn full(_: i32, _: i32) -> f32 {
            1.0
        }
        unsafe extern "C" fn zero(_: i32, _: i32) -> f32 {
            0.0
        }
        unsafe extern "C" fn count(_: i32, _: i32) -> i32 {
            0
        }
        unsafe extern "C" fn fly(_: i32, _: i32) -> bool {
            false
        }
        unsafe extern "C" fn order(_: i32, _: i32, _: i32, _: i32, _: *mut c_void) -> i32 {
            ORDERS.with(|n| n.set(n.get() + 1));
            0
        }
        let table = SSkirmishAICallback {
            UnitDef_isAbleToFly: Some(fly),
            Unit_getWeapons: Some(count),
            Unit_getHealth: Some(health),
            Unit_getMaxHealth: Some(full),
            Unit_getCaptureProgress: Some(zero),
            Unit_getParalyzeDamage: Some(zero),
            Engine_handleCommand: Some(order),
            ..Default::default()
        };
        // SAFETY: all handles stay within the callback table's lifetime.
        let cb = unsafe { springai::Callback::from_raw(0, &table) };
        let unit = Unit::from_id(cb, 1).unwrap();
        let def = UnitDef::from_id(cb, 1).unwrap();
        for name in ["hoverassault", "spiderantiheavy", "cloakheavyraid", "cloakaa"] {
            let mut c = Coward::new_spotter(unit, def, name, 0);
            assert!(!c.is_scythe);
            HEALTH.with(|h| h.set(0.61));
            assert!(!c.should_retreat());
            HEALTH.with(|h| h.set(0.59));
            assert!(c.should_retreat());
            assert!(!c.is_healed());
            ORDERS.with(|n| n.set(0));
            HEALTH.with(|h| h.set(1.0));
            assert!(c.is_healed());
            assert_eq!(ORDERS.with(Cell::get), 0, "healing must not enable fire for {name}");
        }
    }
}
