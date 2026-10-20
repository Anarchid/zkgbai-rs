//! The tool registry: what an operator can ask and tell the AI.
//!
//! Every tool runs on the AI's thread with the whole [`Ai`] at hand. Its
//! parameters are declared once, for the MCP input schema and for the
//! argument checks.

use serde_json::{Value, json};

use super::{actions, views};
use crate::Ai;

#[derive(Clone, Copy)]
pub enum Ty {
    Num,
    Int,
    Str,
    Bool,
    /// Unit ids: a list (or one number).
    Ids,
}

/// A parameter: name, type, required, description.
pub struct P(pub &'static str, pub Ty, pub bool, pub &'static str);

pub struct Tool {
    pub name: &'static str,
    pub description: &'static str,
    pub params: &'static [P],
    /// Changes what the AI does (journaled); else read-only.
    pub writes: bool,
    /// Needs the AI's modules up (after init, not merged into another).
    pub needs_modules: bool,
    pub run: fn(&mut Ai, &Args) -> Result<Value, String>,
}

impl Tool {
    /// The tool's MCP `tools/list` entry.
    pub fn listing(&self) -> Value {
        let mut props = serde_json::Map::new();
        let mut required = Vec::new();
        for P(name, ty, req, doc) in self.params {
            let schema = match ty {
                Ty::Num => json!({"type": "number"}),
                Ty::Int => json!({"type": "integer"}),
                Ty::Str => json!({"type": "string"}),
                Ty::Bool => json!({"type": "boolean"}),
                Ty::Ids => json!({"type": "array", "items": {"type": "integer"}}),
            };
            let mut schema = schema;
            schema["description"] = json!(doc);
            props.insert((*name).to_owned(), schema);
            if *req {
                required.push(*name);
            }
        }
        json!({
            "name": self.name,
            "description": self.description,
            "inputSchema": {"type": "object", "properties": props, "required": required},
            "annotations": {"readOnlyHint": !self.writes, "destructiveHint": false, "openWorldHint": false},
        })
    }
}

pub fn find(name: &str) -> Option<&'static Tool> {
    TOOLS.iter().find(|t| t.name == name)
}

/// Tool arguments (a JSON object), with lenient number parsing: values
/// that came in a URL query are strings.
pub struct Args<'a>(pub &'a Value);

impl Args<'_> {
    pub fn get(&self, key: &str) -> Option<&Value> {
        self.0.get(key).filter(|v| !v.is_null())
    }

    pub fn has(&self, key: &str) -> bool {
        self.get(key).is_some()
    }

    pub fn opt_f32(&self, key: &str) -> Result<Option<f32>, String> {
        match self.get(key) {
            None => Ok(None),
            Some(Value::Number(n)) => Ok(n.as_f64().map(|f| f as f32)),
            Some(Value::String(s)) => s.trim().parse().map(Some).map_err(|_| format!("'{key}' must be a number")),
            Some(_) => Err(format!("'{key}' must be a number")),
        }
    }

    pub fn f32(&self, key: &str) -> Result<f32, String> {
        self.opt_f32(key)?.ok_or_else(|| format!("'{key}' is required"))
    }

    pub fn f32_or(&self, key: &str, default: f32) -> Result<f32, String> {
        Ok(self.opt_f32(key)?.unwrap_or(default))
    }

    pub fn opt_i32(&self, key: &str) -> Result<Option<i32>, String> {
        Ok(self.opt_f32(key)?.map(|f| f.round() as i32))
    }

    pub fn i32(&self, key: &str) -> Result<i32, String> {
        self.opt_i32(key)?.ok_or_else(|| format!("'{key}' is required"))
    }

    pub fn opt_str(&self, key: &str) -> Result<Option<&str>, String> {
        match self.get(key) {
            None => Ok(None),
            Some(Value::String(s)) => Ok(Some(s.as_str())),
            Some(_) => Err(format!("'{key}' must be a string")),
        }
    }

    pub fn str(&self, key: &str) -> Result<&str, String> {
        self.opt_str(key)?.ok_or_else(|| format!("'{key}' is required"))
    }

    pub fn bool_or(&self, key: &str, default: bool) -> bool {
        match self.get(key) {
            Some(Value::Bool(b)) => *b,
            Some(Value::String(s)) => matches!(s.as_str(), "true" | "1" | "yes" | "on"),
            Some(Value::Number(n)) => n.as_f64().is_some_and(|f| f != 0.0),
            _ => default,
        }
    }

    pub fn ids(&self, key: &str) -> Result<Vec<i32>, String> {
        let bad = || format!("'{key}' must be a list of unit ids");
        match self.get(key) {
            None => Err(format!("'{key}' is required")),
            Some(Value::Number(n)) => n.as_f64().map(|f| vec![f as i32]).ok_or_else(bad),
            Some(Value::Array(a)) => a.iter().map(|v| v.as_f64().map(|f| f as i32).ok_or_else(bad)).collect(),
            Some(Value::String(s)) => s.split(',').map(|p| p.trim().parse::<i32>().map_err(|_| bad())).collect(),
            Some(_) => Err(bad()),
        }
    }
}

use Ty::*;

const XZ: [P; 2] = [P("x", Num, true, "map x (elmos)"), P("z", Num, true, "map z (elmos)")];

pub static TOOLS: &[Tool] = &[
    // ---- reading ----
    Tool {
        name: "status",
        description: "Overview of the game and the AI: frame and time, map, teams, state (starting, playing, merged, \
                       resigned), resources, army and enemy values, territory, features switched off, operator \
                       overrides in force. Start here.",
        params: &[],
        writes: false,
        needs_modules: false,
        run: views::status,
    },
    Tool {
        name: "economy",
        description: "Economy detail: metal and energy (current, income, usage, storage), the AI's income \
                       estimates, greed, energy stall, build power, workers, worker tasks by kind, buildings, the \
                       factory the economy wants next (requisition).",
        params: &[],
        writes: false,
        needs_modules: true,
        run: views::economy,
    },
    Tool {
        name: "workers",
        description: "Our constructors and commanders, each with its position and current task.",
        params: &[],
        writes: false,
        needs_modules: true,
        run: views::workers,
    },
    Tool {
        name: "tasks",
        description: "Worker tasks (construction, reclaim, repair, combat_reclaim): what, where, who works on it, \
                       whether started and whether the operator prioritized it.",
        params: &[
            P("kind", Str, false, "only this kind: construction, reclaim, repair or combat_reclaim"),
            P("limit", Int, false, "at most this many (default 200)"),
        ],
        writes: false,
        needs_modules: true,
        run: views::tasks,
    },
    Tool {
        name: "factories",
        description: "Our factories (what each is building), army composition counts and values, what the enemy \
                       was seen to field (air, drones, amphibious, heavy defenses), operator production orders.",
        params: &[],
        writes: false,
        needs_modules: true,
        run: views::factories,
    },
    Tool {
        name: "military",
        description: "Military summary: enemy army, defense and air values, nukes and anti-nukes, our available \
                       threat, how many units each handler runs, places to defend, enemy incursions and the \
                       blocks sent against them, and the intel estimate of enemy production.",
        params: &[],
        writes: false,
        needs_modules: true,
        run: views::military,
    },
    Tool {
        name: "squads",
        description: "Our squads: id, kind (assault, raider, shield, hover; air), status (forming, rallying, \
                       attacking), position, target, members and value.",
        params: &[],
        writes: false,
        needs_modules: true,
        run: views::squads,
    },
    Tool {
        name: "units",
        description: "Our units with their role in the AI (worker, factory, raider, squad member, retreating, \
                       building, reserved for the operator...). Filter by unit def name, role or area.",
        params: &[
            P("def", Str, false, "unit def name contains this (e.g. 'cloakraid', 'turret')"),
            P("role", Str, false, "role starts with this (e.g. 'raider', 'squad', 'worker', 'building')"),
            P("x", Num, false, "with z and radius: only units near this point"),
            P("z", Num, false, "see x"),
            P("radius", Num, false, "see x (default 600)"),
            P("limit", Int, false, "at most this many (default 300)"),
        ],
        writes: false,
        needs_modules: true,
        run: views::units,
    },
    Tool {
        name: "unit",
        description: "One unit in detail, ours (role, orders, health, the AI's bookkeeping on it) or an enemy \
                       the AI knows (type, last seen, value, threat, classification).",
        params: &[P("id", Int, true, "unit id")],
        writes: false,
        needs_modules: true,
        run: views::unit,
    },
    Tool {
        name: "enemies",
        description: "Enemy units and buildings the AI knows of (seen, on radar, or remembered), most valuable \
                       first, with how long ago each was seen.",
        params: &[
            P(
                "kind",
                Str,
                false,
                "fighter, porc (static defense), static, mex, com, worker, aa, air, radar (unidentified) or all",
            ),
            P("visible", Bool, false, "only those in sight or on radar now"),
            P("x", Num, false, "with z and radius: only near this point"),
            P("z", Num, false, "see x"),
            P("radius", Num, false, "see x (default 1000)"),
            P("limit", Int, false, "at most this many (default 100)"),
        ],
        writes: false,
        needs_modules: true,
        run: views::enemies,
    },
    Tool {
        name: "threat_at",
        description: "All threat and value maps at one point, and whether the point is our territory, the enemy's \
                       or the front line.",
        params: &XZ,
        writes: false,
        needs_modules: true,
        run: views::threat_at,
    },
    Tool {
        name: "threat_map",
        description: "One threat or value map as a grid of numbers (rows north to south). Layers: threat, \
                       enemy_porc, ally_threat, ally_raider, ally_porc, aa_threat, aa_porc, sub_threat, sub_porc, \
                       ally_sub, value, raider_value, bp, riot, scythe.",
        params: &[
            P("layer", Str, false, "map name (default threat)"),
            P("max_size", Int, false, "at most this many cells a side (default 96); cells are merged by max"),
        ],
        writes: false,
        needs_modules: true,
        run: views::threat_map,
    },
    Tool {
        name: "spots",
        description: "Metal spots: position, value, whether ours, the enemy's, seen, in whose territory shadow.",
        params: &[],
        writes: false,
        needs_modules: true,
        run: views::spots,
    },
    Tool {
        name: "map_info",
        description: "Map size, metal spots, start position, our and the enemy's centers, and a coarse height \
                       grid (0-255 between the lowest and highest point; below 0 elevation is water).",
        params: &[P("heights", Bool, false, "include the height grid (default true)")],
        writes: false,
        needs_modules: true,
        run: views::map_info,
    },
    Tool {
        name: "terrain",
        description: "The terrain analysis, once ready: regions and chokepoints of the enemy's movement tier, \
                       chokepoints to fortify, sheltered regions, and the strategic options found (walls, holds, \
                       bridges, drops, teleports).",
        params: &[],
        writes: false,
        needs_modules: true,
        run: views::terrain,
    },
    Tool {
        name: "events",
        description: "Recent journal records: log lines, chat, unit births and deaths, kills, enemies first seen, \
                       operator actions and notes, per-second ticks. Filter by kind and text.",
        params: &[
            P(
                "kinds",
                Str,
                false,
                "comma list of: log, say, chat, unit+, unit-, kill, enemy+, given, idle, op, note, tick, snap, run, map, \
                 end (default: all but tick and snap)",
            ),
            P("grep", Str, false, "only records containing this text"),
            P("since", Int, false, "from this sequence number on (else the latest)"),
            P("limit", Int, false, "at most this many (default 100)"),
        ],
        writes: false,
        needs_modules: false,
        run: views::events,
    },
    Tool {
        name: "defs",
        description: "Unit definitions: search by name, or one def in full (cost, health, speed, weapons, what it \
                       builds).",
        params: &[
            P("name", Str, false, "exact internal name for full detail"),
            P("search", Str, false, "names or human names containing this"),
        ],
        writes: false,
        needs_modules: false,
        run: views::defs,
    },
    Tool {
        name: "snapshot",
        description: "Compact positions of everything: our units [id, def, x, z, health, role, squad], known \
                       enemies [id, def, x, z, frames since seen, flags], squads, metal spot owners, places to \
                       defend, resources. What the dashboard draws.",
        params: &[],
        writes: false,
        needs_modules: true,
        run: views::snapshot,
    },
    Tool {
        name: "overrides",
        description: "Everything the operator has set: reserved units, production orders, factory requisition, \
                       construction priorities, defense points, squad targets, features switched off.",
        params: &[],
        writes: false,
        needs_modules: false,
        run: views::overrides,
    },
    Tool {
        name: "profile",
        description: "CPU time per AI module handler, and panics per handler.",
        params: &[],
        writes: false,
        needs_modules: false,
        run: views::profile,
    },
    // ---- acting ----
    Tool {
        name: "say",
        description: "Say something in the game chat (everyone sees it).",
        params: &[P("text", Str, true, "what to say")],
        writes: true,
        needs_modules: false,
        run: actions::say,
    },
    Tool {
        name: "mark",
        description: "Put a marker (with a label) on the map, or draw a line to (to_x, to_z).",
        params: &[
            P("x", Num, true, "map x"),
            P("z", Num, true, "map z"),
            P("label", Str, false, "marker text"),
            P("to_x", Num, false, "draw a line to this x instead"),
            P("to_z", Num, false, "and this z"),
        ],
        writes: true,
        needs_modules: false,
        run: actions::mark,
    },
    Tool {
        name: "note",
        description: "Write a note into the run's journal (to record why the operator did something).",
        params: &[P("text", Str, true, "the note"), P("tag", Str, false, "a short tag")],
        writes: true,
        needs_modules: false,
        run: actions::note,
    },
    Tool {
        name: "feature",
        description: "Switch an AI feature off or back on (the dev switches): porc, reach, refuge, teleport, \
                       drops, ditch, reanalysis, intel, grid, precision, nukes, missiles, scouts, radar, \
                       requisition, colonize, aaintel.",
        params: &[
            P("name", Str, true, "feature name"),
            P("off", Bool, true, "true to switch it off, false to switch it back on"),
        ],
        writes: true,
        needs_modules: false,
        run: actions::feature,
    },
    Tool {
        name: "fac_queue",
        description: "Order units from our factories: the next time a factory that can build the unit picks what \
                       to make, it makes this instead, until the count is done. Orders are served in sequence.",
        params: &[
            P("unit", Str, false, "unit def name (see defs)"),
            P("count", Int, false, "how many (default 1)"),
            P("factory", Int, false, "only this factory (unit id)"),
            P("clear", Bool, false, "drop all production orders instead"),
        ],
        writes: true,
        needs_modules: true,
        run: actions::fac_queue,
    },
    Tool {
        name: "requisition",
        description: "Have the economy build a factory of this type next (as the AI's own modules ask for \
                       factories), until one stands or is under way.",
        params: &[
            P("factory", Str, false, "factory def name, e.g. factoryshield, factorycloak, factoryhover, factoryplane"),
            P("clear", Bool, false, "drop the request instead"),
        ],
        writes: true,
        needs_modules: true,
        run: actions::requisition,
    },
    Tool {
        name: "build",
        description: "Add a construction task for a building at (x, z) (the nearest legal site); workers take it \
                       first when prioritized.",
        params: &[
            P("unit", Str, true, "building def name, e.g. turretlaser, turretaalaser, staticradar, energysolar"),
            P("x", Num, true, "map x"),
            P("z", Num, true, "map z"),
            P("facing", Int, false, "0-3 (default 0)"),
            P("priority", Bool, false, "workers prefer it to everything (default true)"),
        ],
        writes: true,
        needs_modules: true,
        run: actions::build,
    },
    Tool {
        name: "defend",
        description: "Make a place one the army defends (as if our units were under attack there) for a while. \
                       Higher weight pulls harder. clear drops them all.",
        params: &[
            P("x", Num, false, "map x"),
            P("z", Num, false, "map z"),
            P("weight", Num, false, "pull, as damage taken (default 2000)"),
            P("seconds", Num, false, "how long (default 60)"),
            P("clear", Bool, false, "drop all defense points instead"),
        ],
        writes: true,
        needs_modules: true,
        run: actions::defend,
    },
    Tool {
        name: "squad_target",
        description: "Send a squad (any kind: assault, raider, shield, hover, air) to a place for a while, \
                       overriding its own targeting; or clear to hand it back.",
        params: &[
            P("squad", Int, true, "squad id (see squads)"),
            P("x", Num, false, "map x"),
            P("z", Num, false, "map z"),
            P("seconds", Num, false, "how long (default 60)"),
            P("clear", Bool, false, "hand the squad back"),
        ],
        writes: true,
        needs_modules: true,
        run: actions::squad_target,
    },
    Tool {
        name: "take",
        description: "Reserve units for the operator: the AI keeps tracking them but its orders to them are \
                       dropped, so only `order` moves them. They stay put until ordered.",
        params: &[P("units", Ids, true, "unit ids")],
        writes: true,
        needs_modules: true,
        run: actions::take,
    },
    Tool {
        name: "release",
        description: "Give reserved units back to the AI (it picks them up as if they had gone idle).",
        params: &[P("units", Ids, false, "unit ids"), P("all", Bool, false, "release all of them")],
        writes: true,
        needs_modules: true,
        run: actions::release,
    },
    Tool {
        name: "order",
        description: "Order our units directly. Commands: move, fight (attack-move), patrol, attack (target unit, \
                       or ground at x,z), area_attack, guard, repair, reclaim (target unit), area_reclaim, \
                       build (unit def at x,z), stop, wait, fire_state, move_state, cloak, on_off, self_destruct. \
                       Units are reserved first (take) unless take is false, in which case the AI may override \
                       the order at once.",
        params: &[
            P("units", Ids, true, "unit ids"),
            P("cmd", Str, true, "the command"),
            P("x", Num, false, "map x"),
            P("z", Num, false, "map z"),
            P("target", Int, false, "target unit id"),
            P("unit", Str, false, "build: building def name"),
            P("facing", Int, false, "build: 0-3"),
            P("radius", Num, false, "area commands (default 300)"),
            P("value", Int, false, "fire_state 0-2, move_state 0-2, cloak/on_off 0-1"),
            P("queue", Bool, false, "append to the unit's orders (shift)"),
            P("take", Bool, false, "reserve the units first (default true)"),
        ],
        writes: true,
        needs_modules: true,
        run: actions::order,
    },
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_unique_and_valid_for_llm_apis() {
        let mut names: Vec<&str> = TOOLS.iter().map(|t| t.name).collect();
        assert!(names.iter().all(|n| n.len() <= 64 && n.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')));
        names.sort();
        names.dedup();
        assert_eq!(names.len(), TOOLS.len());
    }

    #[test]
    fn listings_have_schemas() {
        let l = find("units").unwrap().listing();
        assert_eq!(l["inputSchema"]["type"], "object");
        assert_eq!(l["inputSchema"]["properties"]["def"]["type"], "string");
        assert_eq!(find("unit").unwrap().listing()["inputSchema"]["required"], json!(["id"]));
        assert_eq!(find("take").unwrap().listing()["annotations"]["readOnlyHint"], false);
    }

    #[test]
    fn arguments_parse_leniently() {
        let v = json!({"x": "12.5", "ids": "3, 4", "one": 7, "on": "true", "nil": null});
        let a = Args(&v);
        assert_eq!(a.f32("x"), Ok(12.5));
        assert_eq!(a.ids("ids"), Ok(vec![3, 4]));
        assert_eq!(a.ids("one"), Ok(vec![7]));
        assert!(a.bool_or("on", false));
        assert!(!a.has("nil"));
        assert!(a.f32("missing").is_err());
    }
}
