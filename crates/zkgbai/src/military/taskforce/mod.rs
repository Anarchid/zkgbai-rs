//! Combined-arms taskforces: forces formed around a core unit by a
//! doctrine (see `doctrine`). The doctrines survived the Oct 23 crash; so
//! far only the `impaler` one is in use, for the escorts of Impaler
//! batteries (see `military::impalers`).

pub mod doctrine;

pub use doctrine::{Aim, Doctrine, Role, Shape, seeds};
