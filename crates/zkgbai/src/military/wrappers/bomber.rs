//! A bomber plane under the bomber handler.

use springai::{NO_TIMEOUT, Unit, Vec3};

use crate::defs::cmd;
use crate::kgbutil::{Nav, PathCost, kgb_move_to};
use crate::military::Enemy;
use crate::util::{NULLPOS, OPTION_SHIFT_KEY, distance};

pub struct Bomber {
    pub id: i32,
    pub unit: Unit,
    /// Enemy id (into `MilitaryManager::targets`).
    pub target: Option<i32>,
    team: i32,
}

impl Bomber {
    pub fn new(unit: Unit, team: i32) -> Self {
        Bomber { id: unit.id(), unit, target: None, team }
    }

    pub fn get_unit(&self) -> Unit {
        self.unit
    }

    pub fn get_pos(&self) -> Vec3 {
        self.unit.pos()
    }

    pub fn is_dead(&self) -> bool {
        self.unit.health() <= 0.0 || self.unit.team() != self.team
    }

    /// Attack `target` (the enemy referenced by `self.target`).
    pub fn bomb(&mut self, target: &Enemy, nav: &mut Nav) {
        if distance(self.unit.pos(), target.position) > 1000.0 {
            self.fly_to(target.position, nav);
        } else if target.is_static {
            self.unit.attack_area(target.position, 0.0, 0);
        } else if target.unit.pos() != NULLPOS {
            self.unit.attack(target.unit, 0);
        } else {
            self.fly_to(target.position, nav);
        }
    }

    /// `targets` is `MilitaryManager::targets`.
    pub fn target_missing(&self, targets: &std::collections::BTreeMap<i32, Enemy>) -> bool {
        match self.target.and_then(|t| targets.get(&t)) {
            None => true,
            Some(e) => e.is_dead || (distance(self.unit.pos(), e.position) < 500.0 && e.unit.pos() == NULLPOS),
        }
    }

    pub fn fly_to(&mut self, pos: Vec3, nav: &mut Nav) {
        let mut path = nav.pathfinder.find_path(nav.war, self.unit, pos, PathCost::AirPath);
        // Skip several waypoints since they're close together and bombers are extra fast.
        let skip = |path: &mut std::collections::VecDeque<Vec3>| {
            for _ in 0..4 {
                if path.len() > 1 {
                    path.pop_front();
                }
            }
        };
        skip(&mut path);
        // port: the original dereferenced the front of a possibly empty path.
        let Some(first) = path.pop_front() else {
            return;
        };
        kgb_move_to(self.unit, first, 0, nav.frame);

        // Add the full path, since the command may not be repeated.
        while !path.is_empty() {
            skip(&mut path);
            if let Some(wp) = path.pop_front() {
                kgb_move_to(self.unit, wp, OPTION_SHIFT_KEY, nav.frame);
            }
        }
    }

    pub fn find_pad(&self) {
        if self.unit.health() < self.unit.max_health() || self.unit.rules_param_float("noammo", 0.0) > 0.0 {
            self.unit.execute_custom_command(cmd::FIND_PAD, &[], 0, NO_TIMEOUT);
        }
    }
}
