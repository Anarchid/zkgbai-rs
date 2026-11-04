//! Drone AA demand comes from simultaneous contacts, less one drone per
//! enemy player. Fog memories and successive replacement drones do not
//! accumulate. The usual historical air-force maximum is applied afterwards.

pub(crate) fn is_drone(name: &str) -> bool {
    matches!(name, "dronelight" | "droneheavyslow" | "dronecarry" | "dronefighter")
}

/// One fresh census per enemy-composition update.
#[derive(Default)]
pub(super) struct DroneSightings {
    total: usize,
    armed: usize,
    faced: usize,
}

impl DroneSightings {
    /// `present` means alive and currently in LOS or on radar, not merely
    /// remembered among the targets from an earlier sighting.
    pub fn observe(&mut self, present: bool, armed: bool, faced: bool) {
        if present {
            self.total += 1;
            self.armed += usize::from(armed);
            self.faced += usize::from(faced);
        }
    }

    /// Total, armed and locally faced metal. Distribute the single global
    /// allowance proportionally, so dividing drones among allied fronts
    /// neither multiplies the allowance nor bypasses it.
    pub fn values(&self, enemy_players: usize) -> (f32, f32, f32) {
        if self.total == 0 {
            return (0.0, 0.0, 0.0);
        }
        let total = 25.0 * self.total.saturating_sub(enemy_players) as f32;
        let per_drone = total / self.total as f32;
        (total, per_drone * self.armed as f32, per_drone * self.faced as f32)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn three_enemy_players_get_three_free_drones() {
        for (count, expected) in [(0, 0.0), (1, 0.0), (2, 0.0), (3, 0.0), (4, 25.0), (6, 75.0)] {
            let mut drones = DroneSightings::default();
            for _ in 0..count {
                drones.observe(true, true, true);
            }
            assert_eq!(drones.values(3), (expected, expected, expected));
        }
    }

    #[test]
    fn successive_sightings_and_fog_memories_do_not_accumulate() {
        let mut peak = 0.0_f32;
        // Nine distinct drones have been identified, but no more than
        // three are present together. The other six are fog memories/dead.
        for frame in 0..3 {
            let mut drones = DroneSightings::default();
            for id in 0..9 {
                drones.observe(id / 3 == frame, true, true);
            }
            peak = peak.max(drones.values(3).0);
        }
        assert_eq!(peak, 0.0);
    }

    #[test]
    fn allowance_is_shared_by_armed_and_local_air_demand() {
        let mut drones = DroneSightings::default();
        for (armed, faced) in [(true, true), (true, false), (true, false), (false, false)] {
            drones.observe(true, armed, faced);
        }
        assert_eq!(drones.values(3), (25.0, 18.75, 6.25));
        assert_eq!(drones.values(0), (100.0, 75.0, 25.0));
    }

    #[test]
    fn commander_and_carrier_drones_share_the_classification() {
        for name in ["dronelight", "droneheavyslow", "dronecarry", "dronefighter"] {
            assert!(is_drone(name));
        }
        for name in ["gunshipbomb", "planefighter", "dynrecon1"] {
            assert!(!is_drone(name));
        }
    }
}
