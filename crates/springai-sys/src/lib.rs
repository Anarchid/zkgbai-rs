//! Raw bindings to the Recoil (Spring) engine's Skirmish AI C interface.
//!
//! The bindings are pre-generated from the engine headers in
//! `rts/ExternalAI/Interface` and checked in, so building an AI needs neither
//! libclang nor an engine checkout. Regenerate them with
//! `tools/regen-bindings.sh` when the interface changes.
//!
//! The callback table has been layout-stable since engine commit `473b7087d0`
//! (2022-10); the generated code carries compile-time layout assertions.

#![allow(non_upper_case_globals, non_camel_case_types, non_snake_case, dead_code)]
#![allow(clippy::all)]

include!("bindings.rs");
