//! Fighter handlers: each owns the logic for one family of combat units.
//! They live at the top level of [`Ai`](crate::Ai) (e.g. `ai.raider_handler`)
//! rather than inside the military manager, so that path queries can borrow
//! `ai.war` while fighters are mutated.

pub mod bomber;
pub mod misc;
pub mod raider;
pub mod retreat;
pub mod squad;

pub use bomber::BomberHandler;
pub use misc::MiscHandler;
pub use raider::RaiderHandler;
pub use retreat::RetreatHandler;
pub use squad::SquadHandler;
