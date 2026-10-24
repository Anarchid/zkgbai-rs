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
    /// Fire at a unit (param: its id) while doing whatever else is ordered
    /// (ZK's unit_target_on_the_move).
    pub const UNIT_SET_TARGET: i32 = 34923;
    pub const UNIT_CANCEL_TARGET: i32 = 34924;
    pub const ONECLICK_WEAPON: i32 = 35000;
    /// ZK's unit_overkill_prevention state: 0 off, 1 automatic targets on
    /// fire at will, 2 on fire at will (the Lance's and Phantom's default),
    /// 3 unless a single target is given, 4 always.
    pub const PREVENT_OVERKILL: i32 = 38291;
    /// Puppies' wreck eating (ZK's unit_grey_goo): 0 off, 1 on, 2 also
    /// while cloaked.
    pub const GOO_GATHER: i32 = 35646;
    /// Jugglenaut's gravity guns (cmd_onoff): 0 pulls, 1 pushes.
    pub const PUSH_PULL: i32 = 35666;
    pub const WANT_ONOFF: i32 = 35667;
    pub const UNIT_AI: i32 = 36214;
    pub const DONT_FIRE_AT_RADAR: i32 = 38372;
    /// Jump to a point (ZK's unit_jumpjets; params x, y, z): walks into
    /// range first, waits for the reload.
    pub const JUMP: i32 = 38521;
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

    /// Internal name of the definition with engine id `id`, if any.
    pub fn name_by_id(&self, id: i32) -> Option<&str> {
        usize::try_from(id).ok().and_then(|i| self.names.get(i)).map(String::as_str).filter(|n| !n.is_empty())
    }

    /// Whether `def` is a commander (Zero-K's are all `dyn*`, the dynamic
    /// commanders). The original told them by build power over 8, which the
    /// gunship constructor (10) has now.
    pub fn is_commander(&self, def: UnitDef) -> bool {
        self.name(def).starts_with("dyn")
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
