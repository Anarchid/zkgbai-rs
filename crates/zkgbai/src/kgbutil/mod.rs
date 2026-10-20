//! Shared utilities: grids, pathfinding, terrain analysis.

pub mod air_field;
pub mod grid;
pub mod height_map;
pub mod line_of_fire;
pub mod move_to;
pub mod pathfinder;
pub mod terrain_analyzer;

pub use air_field::AirField;
pub use grid::{ArrayGraphics, ByteArrayGraphics};
pub use height_map::HeightMap;
pub use line_of_fire::{FireGround, Flight, Gun, Shadow, Shape, Volume};
pub use move_to::{kgb_move_to, kgb_move_to_exact};
pub use pathfinder::{PathCost, Pathfinder, Reachability};
pub use terrain_analyzer::TerrainAnalyzer;

use crate::military::MilitaryManager;

/// Everything a unit wrapper needs to issue path-following orders.
///
/// Build it with [`nav!`](crate::nav) so that it borrows only the pathfinder
/// and the military manager, leaving the unit stores free to borrow mutably:
///
/// ```ignore
/// let mut nav = nav!(self);
/// if let Some(f) = self.fighters.get_mut(&id) {
///     f.fight_to(target, &mut nav);
/// }
/// ```
pub struct Nav<'a> {
    pub pathfinder: &'a mut Pathfinder,
    /// Threat maps for path costs.
    pub war: &'a MilitaryManager,
    pub frame: i32,
    pub team_id: i32,
}

/// Construct a [`Nav`] from an [`Ai`](crate::Ai) expression, borrowing
/// individual fields so it composes with other field borrows.
#[macro_export]
macro_rules! nav {
    ($ai:expr) => {
        $crate::kgbutil::Nav { pathfinder: &mut $ai.pathfinder, war: &$ai.war, frame: $ai.frame, team_id: $ai.team_id }
    };
}
