//! Safe Rust interface for Recoil (Spring) engine Skirmish AIs.
//!
//! Object handles ([`Unit`], [`UnitDef`], [`WeaponDef`], [`Feature`], ...)
//! are small `Copy` values carrying the engine [`Callback`], so code can call
//! `unit.pos()` without threading a context around, much like the engine's
//! own C++ OO wrapper. Identity (`Eq`, `Hash`, `Ord`) is the engine id.
//!
//! An AI implements [`SkirmishAi`] and exports itself with [`export_ai!`].

mod callback;
mod event;
pub mod export;
mod feature;
mod macros;
mod math;
mod unit;
mod unitdef;
mod weapondef;
mod world;

pub use callback::Callback;
pub use event::Event;
pub use export::SkirmishAi;
pub use feature::{Feature, FeatureDef};
pub use math::Vec3;
pub use unit::{CurrentCommand, NO_TIMEOUT, Unit, Weapon, opt};
pub use unitdef::{MoveData, UnitDef, WeaponMount};
pub use weapondef::WeaponDef;
pub use world::{Economy, Game, Map, Mod, Resource};

/// Raw engine bindings, for anything this crate does not wrap yet.
pub use springai_sys as ffi;
