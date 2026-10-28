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

pub use doctrine::{Aim, Attack, Doctrine, Judge, Keep, Role, Shape, seeds};
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
    /// The fight predictor's model for packs that judge by it, loaded at
    /// the first ask (`None` inside if it would not load).
    pub model: Option<Option<Arc<crate::military::fightsim::Model>>>,
    /// Doctrines switched on that sleep until their `wake_share` (see
    /// `taskforce_wake`).
    pub asleep: Vec<String>,
}

/// A sleeping doctrine wakes only once the map holds this many armed ground
/// units (ours and the enemy's we know)...
const WAKE_MIN: usize = 12;
/// ...of them this many the enemy's.
const WAKE_MIN_ENEMY: usize = 6;

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
    /// Wake the sleeping doctrines whose moment has come: more than their
    /// `wake_share` of the armed ground units on the map (ours, and the
    /// enemy's we have identified; no commanders, constructors or aircraft)
    /// are not raiders, at least [`WAKE_MIN`] of them, and as much of the
    /// enemy's alone, at least [`WAKE_MIN_ENEMY`] (a vehicle opening's own
    /// Rippers and Ravagers tip the whole map at four minutes while the
    /// enemy still raids: what Ravagers fare worst against). Their units
    /// fighting elsewhere join their forces at once, cores first.
    pub fn taskforce_wake(&mut self) {
        if self.taskforces.asleep.is_empty() {
            return;
        }
        let ((our_raiders, our_others), (raiders, others)) = self.taskforce_army_mix();
        let n_enemy = raiders + others;
        let n = n_enemy + our_raiders + our_others;
        if n < WAKE_MIN || n_enemy < WAKE_MIN_ENEMY {
            return;
        }
        let share = (others + our_others) as f32 / n as f32;
        let enemy_share = others as f32 / n_enemy as f32;
        let waking: Vec<String> = (self.taskforces.asleep.iter())
            .filter(|name| {
                (self.taskforces.doctrines.iter())
                    .any(|d| &d.name == *name && share > d.wake_share && enemy_share > d.wake_share)
            })
            .cloned()
            .collect();
        for name in waking {
            self.taskforces.asleep.retain(|a| a != &name);
            let Some(d) = self.taskforces.doctrines.iter_mut().find(|d| d.name == name) else { continue };
            d.enabled = true;
            let d = d.clone();
            let classes = &self.taskforces.classes;
            let mut ids: Vec<(bool, i32)> = (self.squad_handler.fighters.iter().copied())
                .filter_map(|id| {
                    let f = self.fighters.get(&id)?;
                    let unit = self.defs.name_of(f.unit);
                    let core = d.is_core(unit, classes);
                    let taken = core || d.role_of(unit, classes).is_some();
                    let in_force = f.squad.and_then(|s| self.squads.get(s)).is_some_and(|s| s.is_taskforce());
                    (taken && !in_force && f.unit.health() > 0.0 && !self.retreat_handler.is_retreating(id))
                        .then_some((!core, id))
                })
                .collect();
            ids.sort();
            self.log(&format!(
                "taskforce: {name} wakes: {} of {n} armed ground units are not raiders ({:.0}%), {others} of the \
                 enemy's {n_enemy} ({:.0}%); {} of its units join",
                others + our_others,
                100.0 * share,
                100.0 * enemy_share,
                ids.len()
            ));
            for (_, id) in ids {
                self.squad_leave_squad(id);
                if let Some(f) = self.fighters.get_mut(&id) {
                    f.squad = None;
                    f.shield_mob = true;
                }
                self.squad_assign_taskforce_member(id);
            }
        }
    }

    /// Raiders and other armed ground units, ours and the enemy's we have
    /// identified (no commanders, constructors or aircraft).
    fn taskforce_army_mix(&self) -> ((usize, usize), (usize, usize)) {
        let uc = &self.unit_classes;
        let raider =
            |n: &str| uc.small_raiders.contains(n) || uc.medium_raiders.contains(n) || uc.solo_raiders.contains(n);
        let (mut ours, mut theirs) = ((0, 0), (0, 0));
        for f in self.fighters.values() {
            let Some(def) = f.unit.def() else { continue };
            if f.unit.health() <= 0.0
                || f.unit.team() != self.team_id
                || def.is_able_to_fly()
                || def.is_builder()
                || !def.is_able_to_attack()
                || self.defs.is_commander(def)
            {
                continue;
            }
            if raider(self.defs.name(def)) { ours.0 += 1 } else { ours.1 += 1 }
        }
        for e in self.war.targets.values() {
            if e.is_dead || !e.identified || e.is_static || e.can_fly() || e.is_worker || e.is_com || !e.can_attack() {
                continue;
            }
            if e.is_raider { theirs.0 += 1 } else { theirs.1 += 1 }
        }
        (ours, theirs)
    }

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
        // Doctrines with a `wake_share` sleep until their moment.
        for d in doctrines.iter_mut().filter(|d| d.enabled && d.wake_share > 0.0) {
            d.enabled = false;
            self.taskforces.asleep.push(d.name.clone());
            self.cb.log(&format!(
                "doctrines: {} sleeps until more than {:.0}% of the armed ground units are not raiders",
                d.name,
                100.0 * d.wake_share
            ));
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
