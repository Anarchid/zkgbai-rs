//! Guessing unit types of radar-only contacts from their speed.

use springai::{Callback, Resource, UnitDef};

/// Averaged stats of all unit types sharing a max speed.
pub struct RadarDef {
    average_speed: f32,
    average_value: f32,
    average_range: f32,
    average_danger: f32,
    sample_name: String,
}

impl RadarDef {
    pub fn new(defs: &[UnitDef], metal: Resource) -> Self {
        let mut total_speed = 0.0;
        let mut total_value = 0.0;
        let mut total_danger = 0.0;

        let sample_name = defs.first().map(|d| d.name()).unwrap_or_default();

        for ud in defs {
            total_speed += ud.speed();
            total_value += ud.cost(metal);
            let armed = ud.num_weapon_mounts().clamp(0, 1) as f32;
            total_danger += (ud.power() * armed).sqrt();
        }

        let ud_size = defs.len().max(1) as f32;
        // The original computed the average max weapon range, then overwrote
        // it with the average danger (leaving the danger at 0); kept as is.
        RadarDef {
            average_speed: total_speed / ud_size / 30.0,
            average_value: total_value / ud_size,
            average_range: total_danger / ud_size,
            average_danger: 0.0,
            sample_name,
        }
    }

    pub fn get_danger(&self) -> f32 {
        self.average_danger
    }

    /// Average speed in elmos per frame.
    pub fn get_speed(&self) -> f32 {
        self.average_speed
    }

    pub fn get_name(&self) -> &str {
        &self.sample_name
    }

    pub fn get_range(&self) -> f32 {
        self.average_range
    }

    pub fn get_value(&self) -> f32 {
        self.average_value
    }
}

pub struct RadarIdentifier {
    radar_defs: Vec<RadarDef>,
}

impl RadarIdentifier {
    pub fn new(cb: Callback) -> Self {
        let mut unit_defs: Vec<(f32, UnitDef)> = cb.unit_defs().into_iter().map(|ud| (ud.speed(), ud)).collect();
        unit_defs.sort_by(|a, b| a.0.total_cmp(&b.0));

        let mut radar_defs = Vec::new();
        let Some(metal) = cb.resource_by_name("Metal") else {
            return RadarIdentifier { radar_defs };
        };

        // Group defs into speed bands 3 elmos/s wide.
        let mut last_speed = 0.0;
        let mut speed_list: Vec<UnitDef> = Vec::new();
        for (speed, ud) in unit_defs {
            if speed - last_speed < 3.0 {
                speed_list.push(ud);
            } else {
                if !speed_list.is_empty() {
                    radar_defs.push(RadarDef::new(&speed_list, metal));
                }
                speed_list.clear();
                speed_list.push(ud);
                last_speed = speed;
            }
        }
        if !speed_list.is_empty() {
            radar_defs.push(RadarDef::new(&speed_list, metal));
        }
        RadarIdentifier { radar_defs }
    }

    /// The speed band closest to `speed` (elmos per frame).
    pub fn get_def_by_speed(&self, speed: f32) -> Option<&RadarDef> {
        let mut closest: Option<&RadarDef> = None;
        let mut min_diff = 0.0;
        for rd in &self.radar_defs {
            let speed_diff = (speed - rd.get_speed()).abs();
            if closest.is_none() || speed_diff < min_diff {
                closest = Some(rd);
                min_diff = speed_diff;
            }
        }
        closest
    }
}
