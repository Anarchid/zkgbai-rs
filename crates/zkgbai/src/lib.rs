//! ZKGBAI — the Zero-K Graph-Based AI.
//!
//! Architecture (Rust port of the Java/C++ original):
//!
//! - [`Ai`] is the single owner of all state. Each original "manager" is a
//!   field holding that module's data (`ai.graph`, `ai.eco`, `ai.war`, ...).
//! - Self-contained queries are methods on the module structs, e.g.
//!   `ai.war.get_threat(pos)` or `ai.graph.get_ally_center()`, so they can be
//!   used while other modules' fields are mutably borrowed.
//! - Logic that needs several modules lives in `impl Ai` blocks inside each
//!   module, and every such method is prefixed with its module tag
//!   (`eco_update`, `graph_get_closest_haven`, `raider_add_small_raider`, ...)
//!   so the shared method namespace never collides.
//! - Units are referenced by engine id. Mobile combat units live in the
//!   [`Ai::fighters`] store and squads in [`Ai::squads`]; handlers keep id
//!   lists instead of pointers.

pub mod ai;
pub mod defs;
pub mod economy;
pub mod graph;
pub mod kgbutil;
pub mod los;
pub mod military;
pub mod terrain;
pub mod util;

pub use ai::Ai;

springai::export_ai!(ai::Ai);
