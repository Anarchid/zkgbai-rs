//! A friendly static defense, tracked for defense-power estimates.

use springai::{Unit, Vec3};

pub struct Porc {
    pub id: i32,
    pub unit: Unit,
    /// Threat radius in 64-elmo grid cells.
    pub radius: i32,
    power: f32,
    team: i32,
}

impl Porc {
    pub fn new(unit: Unit, team: i32) -> Self {
        let mut power = 0.0;
        for wd in unit.def().into_iter().flat_map(|d| d.weapon_mounts()).filter_map(|m| m.weapon_def()) {
            if wd.name().contains("fake") {
                continue;
            }
            power += super::weapon_power(wd, true);
        }
        Porc { id: unit.id(), unit, radius: (unit.max_range() / 64.0).round() as i32, power, team }
    }

    pub fn get_power(&self) -> i32 {
        let mut pow = self.power + self.unit.max_health();
        pow /= 10.0;
        if self.unit.is_being_built() {
            let p = self.unit.build_progress();
            pow *= p * p;
        }
        pow.round() as i32
    }

    pub fn get_pos(&self) -> Vec3 {
        self.unit.pos()
    }

    pub fn is_dead(&self) -> bool {
        self.unit.health() <= 0.0 || self.unit.team() != self.team
    }
}
