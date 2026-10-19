//! Cached unit definition lookups and Zero-K command ids.

use std::collections::HashMap;

use springai::{Callback, Unit, UnitDef};

/// Zero-K LuaRules custom command ids.
pub mod cmd {
    pub const ORBIT: i32 = 13923;
    pub const ORBIT_DRAW: i32 = 13924;
    pub const RAW_MOVE: i32 = 31109;
    pub const MORPH_UPGRADE_INTERNAL: i32 = 31207;
    pub const MORPH: i32 = 31210;
    pub const FIND_PAD: i32 = 33411;
    pub const PRIORITY: i32 = 34220;
    pub const MISC_PRIORITY: i32 = 34221;
    pub const AP_FLY_STATE: i32 = 34569;
    pub const ONECLICK_WEAPON: i32 = 35000;
    pub const WANT_ONOFF: i32 = 35667;
    pub const UNIT_AI: i32 = 36214;
    pub const DONT_FIRE_AT_RADAR: i32 = 38372;
    pub const WANTED_SPEED: i32 = 38825;
    pub const AIR_STRAFE: i32 = 39381;
}

/// Unit definitions indexed by name and id, built once at startup.
///
/// Engine name lookups cross the FFI boundary and allocate; the AI does them
/// constantly (type checks in damage handlers etc.), so cache them here.
pub struct Defs {
    by_name: HashMap<String, UnitDef>,
    names: Vec<String>,
}

impl Defs {
    pub fn new(cb: Callback) -> Self {
        let mut by_name = HashMap::new();
        let mut names = Vec::new();
        for def in cb.unit_defs() {
            let name = def.name();
            let id = def.id() as usize;
            if names.len() <= id {
                names.resize(id + 1, String::new());
            }
            names[id] = name.clone();
            by_name.insert(name, def);
        }
        Defs { by_name, names }
    }

    /// Look up a unit definition by internal name (e.g. `"cloakraid"`).
    pub fn get(&self, name: &str) -> Option<UnitDef> {
        self.by_name.get(name).copied()
    }

    /// Internal name of a definition; empty if unknown.
    pub fn name(&self, def: UnitDef) -> &str {
        self.names.get(def.id() as usize).map(String::as_str).unwrap_or("")
    }

    /// Internal name of a unit's definition; empty if not visible.
    pub fn name_of(&self, unit: Unit) -> &str {
        unit.def().map(|d| self.name(d)).unwrap_or("")
    }

    /// Id of a definition by name, or -1 (the original's sentinel for "none").
    pub fn id(&self, name: &str) -> i32 {
        self.get(name).map(|d| d.id()).unwrap_or(-1)
    }

    pub fn all(&self) -> impl Iterator<Item = UnitDef> + '_ {
        self.by_name.values().copied()
    }
}
