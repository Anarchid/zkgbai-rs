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
        let land = |deg| {
            let max_slope = engine_max_slope(deg);
            MoveClass {
                max_slope,
                slope_mod: default_slope_mod(max_slope),
                max_water_depth: 22.0,
                min_water_depth: 0.0,
                hover: false,
            }
        };
        match self {
            Tier::Spider => land(70.0),
            Tier::Bot => land(36.0),
            Tier::Vehicle => MoveClass { slope_mod: 20.0, ..land(18.0) },
            Tier::Hover => MoveClass { max_water_depth: f32::INFINITY, hover: true, slope_mod: 30.0, ..land(18.0) },
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

/// The engine's `slopeMod` for a movedef that sets none (`MoveDef.cpp`):
/// at its slope limit a unit climbs at a fifth of its speed. Zero-K sets
/// its own for tanks (20) and hovercraft (30), which climb faster.
pub fn default_slope_mod(max_slope: f32) -> f32 {
    4.0 / (max_slope + 0.001)
}

/// Going straight uphill on ground the region decomposition treats as an
/// obstacle, units move at most this fraction of their speed: slopes that
/// players treat as cliffs (see [`MoveClass::soft`]).
pub const SOFT_SPEED: f32 = 1.0 / 3.0;

/// Terrain limits of a movement class, in the engine's units.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MoveClass {
    /// Steepest passable slope-map value (`1 - normal.y`).
    pub max_slope: f32,
    /// How much slopes slow the class going uphill (the movedef's
    /// `slopeMod`, see [`MoveClass::uphill_speed`]).
    pub slope_mod: f32,
    /// Deepest passable water (elmos below sea level).
    pub max_water_depth: f32,
    /// Shallowest passable water; non-zero for ships.
    pub min_water_depth: f32,
    /// Hovercraft ignore slope and depth on water.
    pub hover: bool,
}

impl MoveClass {
    /// The fraction of its speed the class keeps going straight up a slope:
    /// `1 / (1 + slope * slope_mod)` below its limit, zero beyond. The
    /// engine scales the slowdown by how directly the unit climbs; going
    /// downhill or across a slope does not slow it at all.
    pub fn uphill_speed(&self, slope: f32) -> f32 {
        if slope > self.max_slope { 0.0 } else { 1.0 / (1.0 + slope * self.slope_mod) }
    }

    /// The class with its slope limit lowered to where climbing slows it to
    /// [`SOFT_SPEED`]: half the limit for bots (the engine's default
    /// `slopeMod`), most of it for Zero-K's tanks and hovercraft.
    pub fn soft(self) -> MoveClass {
        let slope = (1.0 / SOFT_SPEED - 1.0) / self.slope_mod.max(1e-3);
        MoveClass { max_slope: self.max_slope.min(slope), ..self }
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
    fn soft_slopes_slow_climbers_to_a_third() {
        let bot = Tier::Bot.zero_k_class();
        assert!((bot.soft().max_slope / bot.max_slope - 0.5).abs() < 0.01);
        for t in [Tier::Bot, Tier::Vehicle, Tier::Hover] {
            let c = t.zero_k_class();
            let soft = c.soft().max_slope;
            assert!((c.uphill_speed(soft) - super::SOFT_SPEED).abs() < 1e-3, "{t:?}");
            assert!(soft <= c.max_slope);
        }
        // Tanks keep 48% of their speed at half their limit, bots a third.
        let tank = Tier::Vehicle.zero_k_class();
        assert!((tank.uphill_speed(tank.max_slope * 0.5) - 0.478).abs() < 0.01);
        assert_eq!(tank.uphill_speed(tank.max_slope * 1.01), 0.0);
    }

    #[test]
    fn engine_slope_conversion() {
        assert!((slope(18.0) - 0.1090).abs() < 1e-3);
        assert!((slope(36.0) - 0.4122).abs() < 1e-3);
        assert!((slope(70.0) - 1.0).abs() < 1e-6);
    }
}
