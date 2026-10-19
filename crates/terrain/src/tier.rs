/// Movement class, as far as terrain passability is concerned.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Tier {
    /// All-terrain walkers (any slope, shallow water).
    Spider,
    /// Bots: moderate slopes, shallow water.
    Bot,
    /// Wheeled and tracked vehicles: gentle slopes, shallow water.
    Vehicle,
    /// Hovercraft: gentle slopes on land, any water.
    Hover,
    /// Ships: deep enough water only.
    Ship,
    /// Amphibious bots: moderate slopes, any water depth.
    Amphib,
}

impl Tier {
    pub const ALL: [Tier; 6] = [Tier::Spider, Tier::Bot, Tier::Vehicle, Tier::Hover, Tier::Ship, Tier::Amphib];

    /// Classify a ground unit from its movement properties.
    ///
    /// - `max_slope`: the engine's move-data max slope
    ///   (`1 - cos(1.5 * min(degrees, 60))`, see [`engine_max_slope`]).
    /// - `min_water_depth` / `max_water_depth`: the unit def's limits.
    /// - `speed_mod_class`: the engine's move class (0 tank, 1 kbot,
    ///   2 hover, 3 ship).
    pub fn classify(max_slope: f32, min_water_depth: f32, max_water_depth: f32, speed_mod_class: i32) -> Tier {
        let deep_water = max_water_depth > 100.0;
        if speed_mod_class == 3 || min_water_depth > 0.0 {
            Tier::Ship
        } else if speed_mod_class == 2 {
            Tier::Hover
        } else if max_slope >= 0.5 {
            Tier::Spider
        } else if max_slope >= 0.15 {
            if deep_water { Tier::Amphib } else { Tier::Bot }
        } else {
            Tier::Vehicle
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Tier::Spider => "spider",
            Tier::Bot => "bot",
            Tier::Vehicle => "vehicle",
            Tier::Hover => "hover",
            Tier::Ship => "ship",
            Tier::Amphib => "amphib",
        }
    }

    /// The movement class of Zero-K's standard movedef for this tier
    /// (TKBOT3, KBOT3, TANK3, HOVER3, BOAT3, AKBOT3).
    pub fn zero_k_class(self) -> MoveClass {
        let land = |deg| MoveClass {
            max_slope: engine_max_slope(deg),
            max_water_depth: 22.0,
            min_water_depth: 0.0,
            hover: false,
        };
        match self {
            Tier::Spider => land(70.0),
            Tier::Bot => land(36.0),
            Tier::Vehicle => land(18.0),
            Tier::Hover => MoveClass { max_water_depth: f32::INFINITY, hover: true, ..land(18.0) },
            Tier::Ship => MoveClass { min_water_depth: 5.0, max_water_depth: f32::INFINITY, ..land(90.0) },
            Tier::Amphib => MoveClass { max_water_depth: f32::INFINITY, ..land(36.0) },
        }
    }
}

/// The engine's conversion of a movedef `maxSlope` in degrees to the slope
/// measure of the slope map (`1 - normal.y`): the angle is clamped to 60
/// degrees and scaled by 1.5 (`MoveDefHandler.cpp`, `DegreesToMaxSlope`).
pub fn engine_max_slope(degrees: f32) -> f32 {
    1.0 - (degrees.clamp(0.0, 60.0) * 1.5).to_radians().cos()
}

/// Terrain limits of a movement class, in the engine's units.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MoveClass {
    /// Steepest passable slope-map value (`1 - normal.y`).
    pub max_slope: f32,
    /// Deepest passable water (elmos below sea level).
    pub max_water_depth: f32,
    /// Shallowest passable water; non-zero for ships.
    pub min_water_depth: f32,
    /// Hovercraft ignore slope and depth on water.
    pub hover: bool,
}

impl MoveClass {
    /// The class with its slope limit scaled by `factor`. Below the engine's
    /// limit, units slow down as `1 / (1 + 4 * slope / max_slope)`: at half
    /// the limit they move at a third of their speed, steep enough that
    /// players treat such slopes as cliffs.
    pub fn softened(self, factor: f32) -> MoveClass {
        MoveClass { max_slope: self.max_slope * factor, ..self }
    }

    /// Whether a cell with this slope and water depth blocks the class
    /// (the engine's ground speed modifier is zero there).
    pub fn blocks(&self, slope: f32, water_depth: f32) -> bool {
        if self.min_water_depth > 0.0 {
            return water_depth < self.min_water_depth;
        }
        if self.hover && water_depth > 0.0 {
            return false;
        }
        slope > self.max_slope || water_depth > self.max_water_depth
    }
}

#[cfg(test)]
mod tests {
    use super::Tier;

    use super::engine_max_slope as slope;

    #[test]
    fn zero_k_move_classes() {
        assert_eq!(Tier::classify(slope(36.0), 0.0, 22.0, 1), Tier::Bot);
        assert_eq!(Tier::classify(slope(36.0), 0.0, 5000.0, 1), Tier::Amphib);
        assert_eq!(Tier::classify(slope(70.0), 0.0, 22.0, 1), Tier::Spider);
        assert_eq!(Tier::classify(slope(18.0), 0.0, 22.0, 0), Tier::Vehicle);
        assert_eq!(Tier::classify(slope(18.0), 0.0, 5000.0, 2), Tier::Hover);
        assert_eq!(Tier::classify(slope(18.0), 5.0, 5000.0, 3), Tier::Ship);
    }

    #[test]
    fn standard_classes_classify_as_their_tier() {
        for t in Tier::ALL {
            let c = t.zero_k_class();
            let class = if t == Tier::Ship {
                3
            } else if c.hover {
                2
            } else {
                1
            };
            assert_eq!(Tier::classify(c.max_slope, c.min_water_depth, c.max_water_depth, class), t);
        }
    }

    #[test]
    fn engine_slope_conversion() {
        assert!((slope(18.0) - 0.1090).abs() < 1e-3);
        assert!((slope(36.0) - 0.4122).abs() < 1e-3);
        assert!((slope(70.0) - 1.0).abs() < 1e-6);
    }
}
