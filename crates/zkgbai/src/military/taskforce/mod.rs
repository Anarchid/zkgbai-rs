//! Combined-arms taskforces: forces formed around a core unit by a
//! doctrine (see `doctrine`), with wants written as expressions over what
//! the force has (see `expr`). On by default: `felon` (the shield ball),
//! `impaler` (the escorts of Impaler batteries, see `military::impalers`)
//! and `lance` (escorted Lances, marksmen: see `marksman`); `ravager`
//! (packs, see `pack`) is off by default (`enable=ravager`), `phantom` and
//! `envoy` are seeded off; the rest wait for conducts of their own. The
//! `disable` option switches off doctrines it names (`disable=lance`).
//!
//! The `doctrines` option (or ZKGBAI_DOCTRINES) names a JSON file, or is
//! the JSON itself: an object of patches by doctrine name, over the seeds
//! (a new name makes a new doctrine):
//!
//! ```json
//! {"impaler": {"roles": [{"name": "screen", "units": "@direct", "want": "ceil(0.3 * cores)"}]},
//!  "tremor": {"enabled": true}}
//! ```
//!
//! A patch that does not apply or check is refused, and logged.

pub mod doctrine;
pub mod expr;
pub mod marksman;
pub mod pack;
pub mod spotter;

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use serde_json::Value;

pub use doctrine::{Aim, Attack, Doctrine, Keep, Role, Shape, seeds};
pub use expr::{Expr, Force};

use crate::Ai;
use crate::defs::Defs;
use crate::military::unit_classes::UnitClasses;

/// Unit classes a doctrine may name instead of unit types: `@raider`,
/// `@assault`, `@arty`, `@aa`, `@shield` (mobile units with a shield),
/// `@sapper` (bombs), `@direct` (raiders, assaults, skirmishers, heavies
/// and striders that are none of artillery, anti-air, bombs, support or
/// scouts)
/// and the rest of [`UnitClasses`]' lists.
#[derive(Clone, Debug, Default)]
pub struct Classes {
    sets: BTreeMap<String, BTreeSet<String>>,
}

impl Classes {
    pub fn insert(&mut self, class: &str, units: &[&str]) {
        self.sets.entry(class.to_owned()).or_default().extend(units.iter().map(|u| (*u).to_owned()));
    }

    /// Whether `class` (`@...`) takes unit type `unit`.
    pub fn takes(&self, class: &str, unit: &str) -> bool {
        self.sets.get(class).is_some_and(|s| s.contains(unit))
    }

    pub fn known(&self, class: &str) -> bool {
        self.sets.contains_key(class)
    }

    pub fn names(&self) -> Vec<&str> {
        self.sets.keys().map(String::as_str).collect()
    }

    pub fn build(uc: &UnitClasses, defs: &Defs) -> Classes {
        let mut c = Classes::default();
        let lists: [(&str, Vec<&BTreeSet<&'static str>>); 13] = [
            ("@raider", vec![&uc.small_raiders, &uc.medium_raiders, &uc.solo_raiders]),
            ("@assault", vec![&uc.assaults]),
            ("@arty", vec![&uc.arties]),
            ("@strider", vec![&uc.striders]),
            ("@air", vec![&uc.air_mobs]),
            ("@support", vec![&uc.mob_supports]),
            ("@loner", vec![&uc.loners]),
            ("@aa", vec![&uc.aas]),
            ("@plane", vec![&uc.planes]),
            ("@bomber", vec![&uc.bombers]),
            ("@sapper", vec![&uc.sappers]),
            ("@heavy", vec![&uc.heavies]),
            ("@shieldmob", vec![&uc.shield_mobs]),
        ];
        for (class, sets) in &lists {
            let units: Vec<&str> = sets.iter().flat_map(|s| s.iter().copied()).collect();
            c.insert(class, &units);
        }
        let shielded: Vec<String> = (defs.all())
            .filter(|d| d.speed() > 0.0)
            .filter(|d| d.weapon_mounts().iter().filter_map(|m| m.weapon_def()).any(|w| w.is_shield()))
            .map(|d| defs.name(d).to_owned())
            .collect();
        c.insert("@shield", &shielded.iter().map(String::as_str).collect::<Vec<_>>());
        let not_direct: BTreeSet<&str> =
            [&uc.arties, &uc.aas, &uc.sappers, &uc.mob_supports].iter().flat_map(|s| s.iter().copied()).collect();
        let direct: Vec<&str> = [
            &uc.small_raiders,
            &uc.medium_raiders,
            &uc.solo_raiders,
            &uc.assaults,
            &uc.loners,
            &uc.heavies,
            &uc.striders,
        ]
        .iter()
        .flat_map(|s| s.iter().copied())
        .filter(|u| !not_direct.contains(u) && !u.contains("scout"))
        .collect();
        c.insert("@direct", &direct);
        c
    }
}

/// The doctrines in force, the classes they may name, and each one's
/// shape for the squad wrapper.
#[derive(Default)]
pub struct Taskforces {
    pub doctrines: Vec<Doctrine>,
    pub classes: Classes,
    shapes: BTreeMap<String, Arc<Shape>>,
    loaded: bool,
    /// Marksmen's marks by gun, and what their volleys kill by gun type
    /// (see `marksman`).
    pub marks: BTreeMap<i32, marksman::Mark>,
    pub volleys: marksman::Volleys,
    /// Spotters (see `spotter`).
    pub spotters: spotter::Spotters,
    /// Packs' targets and repairs, by force (see `pack`).
    pub packs: BTreeMap<crate::military::SquadId, pack::Pack>,
}

/// Aims whose forces are squads of the squad handler (see
/// `handlers::squad`); the others have modules of their own.
pub fn squad_aim(aim: Aim) -> bool {
    matches!(aim, Aim::Assault | Aim::Artillery | Aim::Standoff | Aim::Pack)
}

impl Taskforces {
    /// A doctrine switched on, by name.
    pub fn doctrine(&self, name: &str) -> Option<&Doctrine> {
        self.doctrines.iter().find(|d| d.name == name && d.enabled)
    }

    /// The doctrine switched on, of those the squad handler runs, that
    /// takes unit type `unit` as core or role (the first listed).
    pub fn squad_doctrine(&self, unit: &str) -> Option<&Doctrine> {
        (self.doctrines.iter())
            .filter(|d| d.enabled && squad_aim(d.aim))
            .find(|d| d.is_core(unit, &self.classes) || d.role_of(unit, &self.classes).is_some())
    }

    /// Whether a new unit of type `unit` goes to the squad handler's
    /// taskforces: some doctrine there takes it as a core or in a role
    /// other than spotting (spotters are claimed, see `spotter`).
    pub fn routes(&self, unit: &str) -> bool {
        (self.doctrines.iter()).filter(|d| d.enabled && squad_aim(d.aim)).any(|d| {
            d.is_core(unit, &self.classes) || d.role_of(unit, &self.classes).is_some_and(|r| d.roles[r].spot.is_none())
        })
    }

    pub fn shape(&self, doctrine: &str) -> Arc<Shape> {
        self.shapes.get(doctrine).cloned().unwrap_or_default()
    }
}

/// Apply `patches` (an object of patches by doctrine name) to `doctrines`:
/// what each did, or why it was refused.
pub fn patch(doctrines: &mut Vec<Doctrine>, patches: &Value, defs: &Defs, classes: &Classes) -> Vec<String> {
    let Value::Object(by_name) = patches else { return vec!["doctrines: not an object of patches by name".into()] };
    let mut said = Vec::new();
    for (name, p) in by_name {
        let Value::Object(p) = p else {
            said.push(format!("doctrines: {name}: a patch must be an object"));
            continue;
        };
        let at = doctrines.iter().position(|d| &d.name == name);
        let mut d = at.map_or_else(|| Doctrine::new(name), |i| doctrines[i].clone());
        match d.apply(p).and_then(|()| d.check(defs, classes)) {
            Ok(()) => {
                said.push(format!(
                    "doctrines: {name} {}: {}",
                    if at.is_some() { "patched" } else { "added" },
                    d.to_json()
                ));
                match at {
                    Some(i) => doctrines[i] = d,
                    None => doctrines.push(d),
                }
            }
            Err(e) => said.push(format!("doctrines: {name}: refused: {e}")),
        }
    }
    said
}

impl Ai {
    /// The map's density, openness and reach for doctrine expressions (see
    /// [`Force`]): reserved, 0 until intel measures them.
    pub fn taskforce_map(&self) -> (f32, f32, f32) {
        (0.0, 0.0, 0.0)
    }

    /// The doctrines, once: the seeds, patched by the `doctrines` option.
    pub fn taskforce_load(&mut self) {
        if self.taskforces.loaded {
            return;
        }
        self.taskforces.loaded = true;
        let classes = Classes::build(&self.unit_classes, &self.defs);
        let mut doctrines = seeds();
        for d in &mut doctrines {
            if let Err(e) = d.check(&self.defs, &classes) {
                self.cb.log(&format!("doctrines: seed {e}; switched off"));
                d.enabled = false;
            }
        }
        let option = (self.cb.option("doctrines").filter(|v| !v.trim().is_empty()))
            .or_else(|| std::env::var("ZKGBAI_DOCTRINES").ok());
        if let Some(v) = option.filter(|v| !v.trim().is_empty()) {
            let text = if v.trim_start().starts_with('{') {
                Ok(v.clone())
            } else {
                std::fs::read_to_string(v.trim()).map_err(|e| format!("doctrines: cannot read {v}: {e}"))
            };
            let said = match text.and_then(|t| serde_json::from_str::<Value>(&t).map_err(|e| format!("doctrines: {e}")))
            {
                Ok(patches) => patch(&mut doctrines, &patches, &self.defs, &classes),
                Err(e) => vec![e],
            };
            for line in said {
                self.cb.log(&line);
            }
        }
        // The `disable` option switches off the doctrines it names, whatever
        // the seeds and patches say.
        for d in doctrines.iter_mut().filter(|d| d.enabled) {
            if self.dev_off(&d.name) {
                d.enabled = false;
                self.cb.log(&format!("doctrines: {} switched off (disable)", d.name));
            }
        }
        let (a, b) = (self.graph.get_start_pos(), self.graph.get_enemy_base());
        if let Some(b) = b {
            self.cb.log(&format!("taskforce: depth axis ({:.0}, {:.0}) to ({:.0}, {:.0})", a.x, a.z, b.x, b.z));
        }
        let on: Vec<&str> = doctrines.iter().filter(|d| d.enabled).map(|d| d.name.as_str()).collect();
        let off: Vec<&str> = doctrines.iter().filter(|d| !d.enabled).map(|d| d.name.as_str()).collect();
        self.cb.log(&format!("doctrines: on {}; off {}", on.join(", "), off.join(", ")));
        self.taskforces.shapes =
            doctrines.iter().map(|d| (d.name.clone(), Arc::new(d.shape(&self.defs, &classes)))).collect();
        self.taskforces.doctrines = doctrines;
        self.taskforces.classes = classes;
    }
}
