//! Doctrines: what a combined-arms taskforce is made of and how it fights,
//! as data.
//!
//! A doctrine names its core (the lynchpin a force forms around: a Felon,
//! a Lance), the roles that join the core (each a list of unit types or
//! classes such as `@direct`, how many it wants as an expression over what
//! the force has (see `expr`), how far from the leader they orbit, their
//! fire orders) and the force's conduct: its pace, where it aims once
//! engaged, what it musters before engaging, and when it falls back. A
//! force missing some of its roles still fights as it is.
//!
//! The seeds are the shield ball as it always was (`felon`), Impaler
//! batteries' escorts (`impaler`), and, off until switched on, shooter
//! teams with spotters (`lance`, `phantom`), siege detachments (`tremor`,
//! `emissary`), Ravager squads (`ravager`) and cloaked bombs
//! (`ghostbombs`). A file named by the `doctrines` option patches them and
//! adds more (see `taskforce::load`).

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{Map, Value, json};

use super::Classes;
use super::expr::{Expr, Force};
use crate::defs::Defs;

/// Where an engaged force goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Aim {
    /// At the enemy, as an assault squad would (the shield ball).
    Assault,
    /// At what artillery should hit, advancing under fight orders so the
    /// core stops where it reaches.
    Artillery,
    /// Near our haven closest to what artillery should hit, out of the
    /// fight (long-range rockets).
    Standoff,
    /// At a station from which the core reaches the most, standing there
    /// and firing at will (Tremors, Emissaries).
    Siege,
    /// At a station, holding fire for volleys at what stands still (see
    /// `military::impalers`).
    Battery,
    /// Unseen to a valuable target, the core leading, then the roles that
    /// `release` charge in (bombs under an area cloak).
    Strike,
}

const AIMS: [Aim; 6] = [Aim::Assault, Aim::Artillery, Aim::Standoff, Aim::Siege, Aim::Battery, Aim::Strike];

impl Aim {
    pub fn name(self) -> &'static str {
        match self {
            Aim::Assault => "assault",
            Aim::Artillery => "artillery",
            Aim::Standoff => "standoff",
            Aim::Siege => "siege",
            Aim::Battery => "battery",
            Aim::Strike => "strike",
        }
    }

    fn parse(s: &str) -> Result<Aim, String> {
        AIMS.into_iter().find(|a| a.name() == s).ok_or_else(|| {
            let names: Vec<&str> = AIMS.iter().map(|a| a.name()).collect();
            format!("aim '{s}': one of {}", names.join(", "))
        })
    }
}

/// Units that join a core.
#[derive(Clone, Debug, PartialEq)]
pub struct Role {
    pub name: String,
    /// Unit types (internal names) and classes (`@direct`, see
    /// [`Classes`]); a unit type a role names goes to it before one whose
    /// class takes it.
    pub units: Vec<String>,
    /// How many a force wants (see `expr`); `None`: all that come.
    pub want: Option<Expr>,
    /// How far from the leader they orbit.
    pub orbit: f32,
    /// A new one goes to the force with the fewest of the role, the most
    /// valuable first (true: where it does most good, as Aspis do); else
    /// to the least valuable force.
    pub to_strongest: bool,
    /// Their shields count toward the force's shield gauge (an Aspis's
    /// area shield does not).
    pub gauge: bool,
    /// Switched on when they join (Outlaws).
    pub switch_on: bool,
    /// The chance a factory pick that could build one builds it instead,
    /// while a force is short of the role (0: only recruited).
    pub produce: f32,
    /// Their fire state (0 hold, 1 return fire, 2 at will) and whether they
    /// fire at radar dots, when they join; `None`: as they are.
    pub fire_state: Option<i32>,
    pub fire_at_radar: Option<bool>,
    /// Striking forces: they charge in once the force is there.
    pub release: bool,
}

impl Role {
    pub fn new(name: &str) -> Role {
        Role {
            name: name.to_owned(),
            units: Vec::new(),
            want: Some(Expr::per_core(1.0)),
            orbit: 120.0,
            to_strongest: false,
            gauge: true,
            switch_on: false,
            produce: 0.0,
            fire_state: None,
            fire_at_radar: None,
            release: false,
        }
    }

    fn fill(name: &str, units: &[&str], orbit: f32) -> Role {
        Role { want: None, orbit, units: units.iter().map(|u| (*u).to_owned()).collect(), ..Role::new(name) }
    }

    fn escort(name: &str, units: &[&str], want: &str, orbit: f32, produce: f32) -> Role {
        Role {
            want: Some(Expr::parse(want).expect("seed want")),
            orbit,
            produce,
            units: units.iter().map(|u| (*u).to_owned()).collect(),
            ..Role::new(name)
        }
    }

    /// How many `force` wants; `None`: all that come.
    pub fn want(&self, force: &Force) -> Option<usize> {
        self.want.as_ref().map(|w| w.want(force))
    }

    pub fn to_json(&self) -> Value {
        json!({
            "name": self.name,
            "units": self.units,
            "want": self.want.as_ref().map(Expr::source),
            "orbit": self.orbit,
            "to_strongest": self.to_strongest,
            "gauge": self.gauge,
            "switch_on": self.switch_on,
            "produce": self.produce,
            "fire_state": self.fire_state,
            "fire_at_radar": self.fire_at_radar,
            "release": self.release,
        })
    }

    /// Change the fields `patch` names.
    pub fn apply(&mut self, patch: &Map<String, Value>) -> Result<(), String> {
        for (k, v) in patch {
            match k.as_str() {
                "name" => self.name = string(k, v)?,
                "units" => self.units = names(k, v)?,
                "want" => {
                    self.want = match v {
                        Value::Null => None,
                        Value::String(s) if s == "all" || s == "fill" => None,
                        Value::String(s) => Some(Expr::parse(s)?),
                        Value::Number(n) => Some(Expr::parse(&n.to_string())?),
                        _ => return Err("'want' must be an expression, a number, or \"all\"".into()),
                    }
                }
                // What `want` was: so many per core, rounded up.
                "per_core" => {
                    self.want = match v {
                        Value::Null => None,
                        Value::String(s) if s == "all" || s == "fill" => None,
                        _ => Some(Expr::per_core(num(k, v, 0.0, 20.0)?)),
                    }
                }
                "fire_state" => self.fire_state = optional(v, |v| Ok(num(k, v, 0.0, 2.0)? as i32))?,
                "fire_at_radar" => self.fire_at_radar = optional(v, |v| flag(k, v))?,
                "release" => self.release = flag(k, v)?,
                "orbit" => self.orbit = num(k, v, 20.0, 600.0)?,
                "to_strongest" => self.to_strongest = flag(k, v)?,
                "gauge" => self.gauge = flag(k, v)?,
                "switch_on" => self.switch_on = flag(k, v)?,
                "produce" => self.produce = num(k, v, 0.0, 1.0)?,
                _ => return Err(format!("roles have no field '{k}' (see doctrines)")),
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Doctrine {
    pub name: String,
    /// What it is for, for whoever edits it.
    pub note: String,
    pub enabled: bool,
    /// The unit types (or classes) a force forms around.
    pub core: Vec<String>,
    /// The core's fire state and whether it fires at radar dots (see
    /// [`Role::fire_state`]).
    pub fire_state: Option<i32>,
    pub fire_at_radar: Option<bool>,
    pub roles: Vec<Role>,
    /// Cores a force takes before a new core starts its own (0: no limit,
    /// a composition grown without end). Only a force that is not engaged
    /// takes cores, so new ones never trickle to a fight alone.
    pub cores_per_force: u32,
    /// With allies, every core joins one force (one giant death ball).
    pub pool_with_allies: bool,
    /// The leader's speed limit, elmos/s (0: none).
    pub pace: f32,
    /// The leader also keeps to this share of its slowest member's speed
    /// (0: no), so a slower screen keeps up (Levelers with Ravagers).
    pub pace_slowest: f32,
    pub aim: Aim,
    /// A force engages once worth this much metal (and rallied)...
    pub engage_value: f32,
    /// ...or once it has mustered this many seconds (0: never; it waits
    /// for its value).
    pub muster: f32,
    /// It falls back below this health (the core's, or the average if
    /// lower).
    pub retreat_health: f32,
    /// It falls back once its shields drop below `shields_low`, and comes
    /// back above `shields_ok`.
    pub shields_low: f32,
    pub shields_ok: f32,
    /// It counts as rallied once this share of its members is near the
    /// leader.
    pub rallied: f32,
    /// Engaged, it falls back from enemy threat where it stands (beyond
    /// what the operator's courage tolerates), as assault squads do.
    pub flee_threat: bool,
    /// Units no doctrine takes, left without a squad, join its least
    /// valuable force (the shield ball's old habit with stray Pyros).
    pub strays: bool,
    /// Engaged, it breaks off to regroup whenever its members scatter (the
    /// shield ball, whose shields must overlap); else being rallied only
    /// lets it go in, as with assault squads.
    pub regroup: bool,
    /// Members join it while engaged (the shield ball); else only while it
    /// is not, so none drives to a fight alone.
    pub reinforce_engaged: bool,
}

impl Doctrine {
    pub fn new(name: &str) -> Doctrine {
        Doctrine {
            name: name.to_owned(),
            note: String::new(),
            enabled: true,
            core: Vec::new(),
            fire_state: None,
            fire_at_radar: None,
            roles: Vec::new(),
            cores_per_force: 1,
            pool_with_allies: false,
            pace: 0.0,
            pace_slowest: 0.0,
            aim: Aim::Artillery,
            engage_value: 0.0,
            muster: 60.0,
            retreat_health: 0.6,
            shields_low: 0.5,
            shields_ok: 0.8,
            rallied: 0.75,
            flee_threat: true,
            strays: false,
            regroup: false,
            reinforce_engaged: false,
        }
    }

    pub fn role(&self, name: &str) -> Option<&Role> {
        self.roles.iter().find(|r| r.name == name)
    }

    /// The role taking unit type `unit`: the first naming it, else the
    /// first with a class taking it; never one the core takes.
    pub fn role_of(&self, unit: &str, classes: &Classes) -> Option<usize> {
        if self.is_core(unit, classes) {
            return None;
        }
        (self.roles.iter().position(|r| r.units.iter().any(|u| u == unit)))
            .or_else(|| self.roles.iter().position(|r| r.units.iter().any(|u| classes.takes(u, unit))))
    }

    pub fn is_core(&self, unit: &str, classes: &Classes) -> bool {
        self.core.iter().any(|c| c == unit || classes.takes(c, unit))
    }

    /// What a force made of these units has, for the roles' wants: by
    /// unit type and metal.
    pub fn force<'a>(&self, units: impl IntoIterator<Item = (&'a str, f32)>, classes: &Classes) -> Force {
        let mut f = Force { roles: self.roles.iter().map(|r| (r.name.clone(), 0, 0.0)).collect(), ..Force::default() };
        for (unit, metal) in units {
            if self.is_core(unit, classes) {
                f.cores += 1;
                f.core_value += metal;
            } else if let Some(i) = self.role_of(unit, classes) {
                f.roles[i].1 += 1;
                f.roles[i].2 += metal;
            }
        }
        f
    }

    pub fn to_json(&self) -> Value {
        json!({
            "name": self.name,
            "note": self.note,
            "enabled": self.enabled,
            "core": self.core,
            "fire_state": self.fire_state,
            "fire_at_radar": self.fire_at_radar,
            "roles": self.roles.iter().map(Role::to_json).collect::<Vec<_>>(),
            "cores_per_force": self.cores_per_force,
            "pool_with_allies": self.pool_with_allies,
            "pace": self.pace,
            "pace_slowest": self.pace_slowest,
            "aim": self.aim.name(),
            "engage_value": self.engage_value,
            "muster": self.muster,
            "retreat_health": self.retreat_health,
            "shields_low": self.shields_low,
            "shields_ok": self.shields_ok,
            "rallied": self.rallied,
            "flee_threat": self.flee_threat,
            "strays": self.strays,
            "regroup": self.regroup,
            "reinforce_engaged": self.reinforce_engaged,
        })
    }

    /// Change the fields `patch` names. `roles` replaces them all (each
    /// role given in full or in part over the defaults).
    pub fn apply(&mut self, patch: &Map<String, Value>) -> Result<(), String> {
        for (k, v) in patch {
            match k.as_str() {
                "name" => {
                    if string(k, v)? != self.name {
                        return Err("a doctrine cannot be renamed (make a new one)".into());
                    }
                }
                "note" => self.note = string(k, v)?,
                "enabled" => self.enabled = flag(k, v)?,
                "core" => self.core = names(k, v)?,
                "fire_state" => self.fire_state = optional(v, |v| Ok(num(k, v, 0.0, 2.0)? as i32))?,
                "fire_at_radar" => self.fire_at_radar = optional(v, |v| flag(k, v))?,
                "roles" => {
                    let Value::Array(list) = v else { return Err("'roles' must be a list of roles".into()) };
                    let mut roles = Vec::new();
                    for r in list {
                        let Value::Object(r) = r else { return Err("each role must be an object".into()) };
                        let name = r.get("name").and_then(Value::as_str).ok_or("each role needs a name")?;
                        let mut role = Role::new(name);
                        role.apply(r)?;
                        roles.push(role);
                    }
                    self.roles = roles;
                }
                "cores_per_force" => self.cores_per_force = num(k, v, 0.0, 1000.0)? as u32,
                "pool_with_allies" => self.pool_with_allies = flag(k, v)?,
                "pace" => self.pace = num(k, v, 0.0, 500.0)?,
                "pace_slowest" => self.pace_slowest = num(k, v, 0.0, 1.0)?,
                "aim" => self.aim = Aim::parse(&string(k, v)?)?,
                "engage_value" => self.engage_value = num(k, v, 0.0, 1e6)?,
                "muster" => self.muster = num(k, v, 0.0, 3600.0)?,
                "retreat_health" => self.retreat_health = num(k, v, 0.0, 1.0)?,
                "shields_low" => self.shields_low = num(k, v, 0.0, 1.0)?,
                "shields_ok" => self.shields_ok = num(k, v, 0.0, 1.0)?,
                "rallied" => self.rallied = num(k, v, 0.0, 1.0)?,
                "flee_threat" => self.flee_threat = flag(k, v)?,
                "strays" => self.strays = flag(k, v)?,
                "regroup" => self.regroup = flag(k, v)?,
                "reinforce_engaged" => self.reinforce_engaged = flag(k, v)?,
                _ => return Err(format!("doctrines have no field '{k}' (see doctrines)")),
            }
        }
        Ok(())
    }

    /// Whether the doctrine makes sense: known unit types and classes, a
    /// core, roles with distinct names and wants that read only roles it
    /// has, shield thresholds in order. A unit type may be named once
    /// only (classes may overlap: the name wins).
    pub fn check(&self, defs: &Defs, classes: &Classes) -> Result<(), String> {
        if self.core.is_empty() {
            return Err(format!("doctrine {}: no core", self.name));
        }
        let known = |u: &str| {
            if u.starts_with('@') { classes.known(u) } else { defs.get(u).is_some_and(|d| d.speed() > 0.0) }
        };
        let mut seen: BTreeSet<&str> = BTreeSet::new();
        for u in &self.core {
            if !known(u) {
                return Err(format!("doctrine {}: core '{u}' is no mobile unit type or class", self.name));
            }
            seen.insert(u);
        }
        let mut role_names = BTreeSet::new();
        for r in &self.roles {
            if !role_names.insert(r.name.as_str()) {
                return Err(format!("doctrine {}: two roles named {}", self.name, r.name));
            }
            for u in &r.units {
                if !known(u) {
                    return Err(format!(
                        "doctrine {}: role {}: '{u}' is no mobile unit type or class ({})",
                        self.name,
                        r.name,
                        classes.names().join(" ")
                    ));
                }
                if !seen.insert(u) {
                    return Err(format!("doctrine {}: '{u}' is listed twice", self.name));
                }
            }
        }
        for r in &self.roles {
            for n in r.want.as_ref().map_or(Vec::new(), Expr::names) {
                if !role_names.contains(n) {
                    return Err(format!("doctrine {}: role {}: its want reads '{n}', no role", self.name, r.name));
                }
            }
        }
        if self.shields_low > self.shields_ok {
            return Err(format!("doctrine {}: shields_low above shields_ok", self.name));
        }
        Ok(())
    }

    /// What the squad wrapper needs of the doctrine, by unit def id.
    pub fn shape(&self, defs: &Defs, classes: &Classes) -> Shape {
        let mut by_def = BTreeMap::new();
        let mut core = BTreeSet::new();
        for d in defs.all() {
            let name = defs.name(d);
            if self.is_core(name, classes) {
                core.insert(d.id());
            } else if let Some(i) = self.role_of(name, classes) {
                by_def.insert(d.id(), i);
            }
        }
        Shape {
            doctrine: self.name.clone(),
            core,
            roles: (self.roles.iter())
                .map(|r| RoleShape {
                    orbit: r.orbit,
                    gauge: r.gauge,
                    switch_on: r.switch_on,
                    fire_state: r.fire_state,
                    fire_at_radar: r.fire_at_radar,
                })
                .collect(),
            by_def,
            pace: self.pace,
            pace_slowest: self.pace_slowest,
            fight: self.aim == Aim::Artillery,
            rallied: self.rallied,
            aim: self.aim,
            cores_per_force: self.cores_per_force,
            pool_with_allies: self.pool_with_allies,
            engage_value: self.engage_value,
            muster: self.muster,
            retreat_health: self.retreat_health,
            shields_low: self.shields_low,
            shields_ok: self.shields_ok,
            flee_threat: self.flee_threat,
            strays: self.strays,
            regroup: self.regroup,
            reinforce_engaged: self.reinforce_engaged,
            fire_state: self.fire_state,
            fire_at_radar: self.fire_at_radar,
        }
    }
}

/// A role as the squad wrapper sees it.
#[derive(Clone, Debug, PartialEq)]
pub struct RoleShape {
    pub orbit: f32,
    pub gauge: bool,
    pub switch_on: bool,
    pub fire_state: Option<i32>,
    pub fire_at_radar: Option<bool>,
}

/// A doctrine compiled for the squad wrapper: by unit def id.
#[derive(Clone, Debug, PartialEq)]
pub struct Shape {
    pub doctrine: String,
    pub core: BTreeSet<i32>,
    pub roles: Vec<RoleShape>,
    /// Def id to role index.
    pub by_def: BTreeMap<i32, usize>,
    /// The leader's speed limit, elmos/s (0: none).
    pub pace: f32,
    /// Share of the slowest member's speed the leader keeps to (0: none).
    pub pace_slowest: f32,
    /// The leader advances under fight orders (stopping where it reaches)
    /// rather than move orders.
    pub fight: bool,
    pub rallied: f32,
    /// The doctrine's conduct (see [`Doctrine`]).
    pub aim: Aim,
    pub cores_per_force: u32,
    pub pool_with_allies: bool,
    pub engage_value: f32,
    pub muster: f32,
    pub retreat_health: f32,
    pub shields_low: f32,
    pub shields_ok: f32,
    pub flee_threat: bool,
    pub strays: bool,
    pub regroup: bool,
    pub reinforce_engaged: bool,
    pub fire_state: Option<i32>,
    pub fire_at_radar: Option<bool>,
}

/// Orbit radius of members that are in no role (extra cores).
pub const ORBIT: f32 = 90.0;

impl Default for Shape {
    fn default() -> Self {
        Shape {
            doctrine: String::new(),
            core: BTreeSet::new(),
            roles: Vec::new(),
            by_def: BTreeMap::new(),
            pace: 0.0,
            pace_slowest: 0.0,
            fight: false,
            rallied: 0.75,
            aim: Aim::Assault,
            cores_per_force: 1,
            pool_with_allies: false,
            engage_value: 0.0,
            muster: 0.0,
            retreat_health: 0.6,
            shields_low: 0.5,
            shields_ok: 0.8,
            flee_threat: true,
            strays: false,
            regroup: false,
            reinforce_engaged: false,
            fire_state: None,
            fire_at_radar: None,
        }
    }
}

impl Shape {
    pub fn is_core(&self, def: Option<i32>) -> bool {
        def.is_some_and(|d| self.core.contains(&d))
    }

    pub fn role(&self, def: Option<i32>) -> Option<&RoleShape> {
        def.and_then(|d| self.by_def.get(&d)).and_then(|&i| self.roles.get(i))
    }

    pub fn orbit(&self, def: Option<i32>) -> f32 {
        self.role(def).map_or(ORBIT, |r| r.orbit)
    }

    /// A member's fire orders by its doctrine: the core's, or its role's.
    pub fn fire_orders(&self, def: Option<i32>) -> (Option<i32>, Option<bool>) {
        if self.is_core(def) {
            (self.fire_state, self.fire_at_radar)
        } else {
            self.role(def).map_or((None, None), |r| (r.fire_state, r.fire_at_radar))
        }
    }

    /// Whether the shield gauge says to fall back: low below `shields_low`,
    /// until back above `shields_ok`.
    pub fn low_shields(&self, low: bool, shields: f32) -> bool {
        if low { shields <= self.shields_ok } else { shields < self.shields_low }
    }
}

/// The doctrines the AI starts with.
pub fn seeds() -> Vec<Doctrine> {
    let mut felon = Doctrine::new("felon");
    felon.note = "The shield ball: Felons lead, shieldbots orbit them, Aspis go where they do most good. \
                  Engages once worth 1300 and rallied; falls back on low shields or below 85% health."
        .into();
    felon.core = vec!["shieldfelon".into()];
    felon.roles = vec![
        Role { to_strongest: true, gauge: false, ..Role::fill("aspis", &["shieldshield"], 40.0) },
        Role::fill("thugs", &["shieldassault"], 125.0),
        Role { switch_on: true, ..Role::fill("outlaws", &["shieldriot"], ORBIT) },
        Role::fill("rackets", &["shieldarty"], ORBIT),
        Role::fill("dominatrices", &["vehcapture"], 250.0),
    ];
    felon.pool_with_allies = true;
    felon.pace = 45.0;
    felon.aim = Aim::Assault;
    felon.engage_value = 1300.0;
    felon.muster = 0.0;
    felon.retreat_health = 0.85;
    felon.flee_threat = false;
    felon.strays = true;
    felon.regroup = true;
    felon.reinforce_engaged = true;

    let mut lance = Doctrine::new("lance");
    lance.note = "Lances (980 range, 1000 hp) holding fire at radar dots, a Halberd to spot for them, a Mace \
                  screen against raiders, Scalpels as the line, a Flail when one is about."
        .into();
    lance.enabled = false;
    lance.core = vec!["hoverarty".into()];
    lance.fire_at_radar = Some(false);
    lance.roles = vec![
        Role::escort("spotter", &["hoverassault"], "1", 300.0, 0.3),
        Role::escort("screen", &["hoverriot"], "ceil(cores)", 130.0, 0.5),
        Role::escort("line", &["hoverskirm"], "ceil(cores)", 170.0, 0.0),
        Role::escort("aa", &["hoveraa"], "cores >= 2 ? 1 : 0", 80.0, 0.0),
    ];
    lance.engage_value = 1500.0;
    lance.muster = 75.0;
    lance.retreat_health = 0.7;

    let mut phantom = Doctrine::new("phantom");
    phantom.note = "Phantoms (700 range, 560 hp) holding fire at radar dots, a Scythe to spot for them, \
                    Reavers against raiders, Knights and Ronins as the line, a Gremlin when one is about."
        .into();
    phantom.enabled = false;
    phantom.core = vec!["cloaksnipe".into()];
    phantom.fire_at_radar = Some(false);
    phantom.roles = vec![
        Role::escort("spotter", &["cloakheavyraid"], "1", 300.0, 0.3),
        Role::escort("screen", &["cloakriot"], "ceil(cores)", 110.0, 0.5),
        Role::escort("line", &["cloakassault", "cloakskirm"], "ceil(cores)", 160.0, 0.0),
        Role::escort("aa", &["cloakaa"], "cores >= 2 ? 1 : 0", 70.0, 0.0),
    ];
    phantom.engage_value = 1100.0;
    phantom.muster = 75.0;
    phantom.retreat_health = 0.7;

    let mut tremor = Doctrine::new("tremor");
    tremor.note = "A siege detachment: Tremors stand where they reach the most and fire at everything, an \
                   Ogre per two against raiders, Minotaurs as the line, an Ettin past 3000 metal."
        .into();
    tremor.enabled = false;
    tremor.core = vec!["tankheavyarty".into()];
    tremor.cores_per_force = 3;
    tremor.roles = vec![
        Role::escort("screen", &["tankriot"], "ceil(0.5 * cores)", 140.0, 0.5),
        Role::escort("line", &["tankassault"], "cores", 180.0, 0.0),
        Role::escort("aa", &["tankaa"], "value > 3000 ? 1 : 0", 90.0, 0.0),
    ];
    tremor.aim = Aim::Siege;
    tremor.engage_value = 2000.0;
    tremor.muster = 90.0;

    let mut emissary = Doctrine::new("emissary");
    emissary.note = "A siege detachment: Emissaries stand where they reach the most and fire at everything, \
                     Ogres against raiders, an Ettin past 3000 metal."
        .into();
    emissary.enabled = false;
    emissary.core = vec!["tankarty".into()];
    emissary.cores_per_force = 4;
    emissary.roles = vec![
        Role::escort("screen", &["tankriot"], "ceil(0.5 * cores)", 140.0, 0.5),
        Role::escort("aa", &["tankaa"], "value > 3000 ? 1 : 0", 90.0, 0.0),
    ];
    emissary.aim = Aim::Siege;
    emissary.engage_value = 1500.0;
    emissary.muster = 90.0;

    let mut impaler = Doctrine::new("impaler");
    impaler.note = "Impaler batteries (see `military::impalers`): a Leveler per two guns against raiders, a \
                    Crasher once three guns stand together."
        .into();
    impaler.core = vec!["vehheavyarty".into()];
    impaler.roles = vec![
        Role::escort("screen", &["vehriot"], "ceil(0.5 * cores)", 120.0, 0.5),
        Role::escort("aa", &["vehaa"], "cores >= 3 ? 1 : 0", 80.0, 0.0),
    ];
    impaler.aim = Aim::Battery;
    impaler.engage_value = 900.0;

    let mut ravager = Doctrine::new("ravager");
    ravager.note = "Rover assault: Ravagers with a Leveler per five against raiders, a composition grown \
                    without end; kept to the Levelers' pace, it goes in once worth 1200 and rallied."
        .into();
    ravager.enabled = false;
    ravager.core = vec!["vehassault".into()];
    ravager.cores_per_force = 0;
    ravager.roles = vec![Role::escort("screen", &["vehriot"], "ceil(cores / 5)", 100.0, 0.5)];
    ravager.pace_slowest = 0.85;
    ravager.aim = Aim::Assault;
    ravager.engage_value = 1200.0;
    ravager.muster = 0.0;

    let mut ghostbombs = Doctrine::new("ghostbombs");
    ghostbombs.note = "An Iris cloaking bombs (orbiting within its 350 cloak, holding fire) unseen to a \
                       valuable target, where they charge in."
        .into();
    ghostbombs.enabled = false;
    ghostbombs.core = vec!["cloakjammer".into()];
    ghostbombs.roles =
        vec![Role { fire_state: Some(0), release: true, ..Role::escort("bombs", &["@sapper"], "6", 200.0, 0.0) }];
    ghostbombs.aim = Aim::Strike;
    ghostbombs.pace = 40.0;
    ghostbombs.engage_value = 600.0;
    ghostbombs.muster = 90.0;

    vec![felon, lance, phantom, tremor, emissary, impaler, ravager, ghostbombs]
}

fn string(k: &str, v: &Value) -> Result<String, String> {
    v.as_str().map(str::to_owned).ok_or_else(|| format!("'{k}' must be a string"))
}

fn flag(k: &str, v: &Value) -> Result<bool, String> {
    match v {
        Value::Bool(b) => Ok(*b),
        Value::String(s) if s == "true" || s == "on" => Ok(true),
        Value::String(s) if s == "false" || s == "off" => Ok(false),
        _ => Err(format!("'{k}' must be true or false")),
    }
}

/// `null` for none, else what `f` makes of it.
fn optional<T>(v: &Value, f: impl Fn(&Value) -> Result<T, String>) -> Result<Option<T>, String> {
    if v.is_null() { Ok(None) } else { f(v).map(Some) }
}

fn num(k: &str, v: &Value, lo: f32, hi: f32) -> Result<f32, String> {
    let x = match v {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().parse::<f64>().ok(),
        _ => None,
    }
    .ok_or_else(|| format!("'{k}' must be a number"))? as f32;
    if !(lo..=hi).contains(&x) {
        return Err(format!("'{k}' must be within {lo}..{hi}"));
    }
    Ok(x)
}

/// A list of unit type names (or one name, or a comma list).
fn names(k: &str, v: &Value) -> Result<Vec<String>, String> {
    let list: Vec<String> = match v {
        Value::Array(a) => a.iter().map(|x| string(k, x)).collect::<Result<_, _>>()?,
        Value::String(s) => s.split(',').map(|x| x.trim().to_owned()).collect(),
        _ => return Err(format!("'{k}' must be a list of unit types")),
    };
    Ok(list.into_iter().filter(|x| !x.is_empty()).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seeds_round_trip_through_json() {
        for d in seeds() {
            let Value::Object(patch) = d.to_json() else { panic!() };
            let mut back = Doctrine::new(&d.name);
            back.apply(&patch).unwrap();
            assert_eq!(back, d);
        }
    }

    #[test]
    fn seeds_list_each_unit_once() {
        for d in seeds() {
            let mut seen = BTreeSet::new();
            for u in d.core.iter().chain(d.roles.iter().flat_map(|r| &r.units)) {
                assert!(seen.insert(u.clone()), "{}: {u} twice", d.name);
            }
        }
    }

    #[test]
    fn the_shield_ball_is_the_old_one() {
        let felon = seeds().into_iter().find(|d| d.name == "felon").unwrap();
        assert_eq!(felon.aim, Aim::Assault);
        assert_eq!((felon.pace, felon.engage_value, felon.muster), (45.0, 1300.0, 0.0));
        assert_eq!((felon.retreat_health, felon.shields_low, felon.shields_ok), (0.85, 0.5, 0.8));
        assert!(felon.pool_with_allies && !felon.flee_threat && felon.strays);
        assert!(felon.regroup && felon.reinforce_engaged);
        // Every role takes all that come; Aspis go to the strongest ball
        // and their area shields are not gauged.
        assert!(felon.roles.iter().all(|r| r.want.is_none() && r.produce == 0.0));
        let aspis = felon.role("aspis").unwrap();
        assert!(aspis.to_strongest && !aspis.gauge && aspis.orbit == 40.0);
        assert!(felon.role("outlaws").unwrap().switch_on);
        assert_eq!(felon.role("thugs").unwrap().orbit, 125.0);
        assert_eq!(felon.role("dominatrices").unwrap().orbit, 250.0);
        let classes = Classes::default();
        assert_eq!(felon.role_of("shieldarty", &classes), Some(3));
        assert!(felon.is_core("shieldfelon", &classes) && felon.role_of("shieldfelon", &classes).is_none());
        assert!(felon.enabled);
    }

    #[test]
    fn patches_change_only_what_they_name() {
        let mut d = seeds().into_iter().find(|d| d.name == "lance").unwrap();
        let before = d.clone();
        let Value::Object(p) = json!({"engage_value": 2000, "aim": "standoff"}) else { panic!() };
        d.apply(&p).unwrap();
        assert_eq!((d.engage_value, d.aim), (2000.0, Aim::Standoff));
        assert_eq!(d.roles, before.roles);
        let Value::Object(p) = json!({"per_core": "all", "units": "hoverriot,hoverdepthcharge"}) else { panic!() };
        d.roles[0].apply(&p).unwrap();
        assert_eq!(d.roles[0].want, None);
        assert_eq!(d.roles[0].units, vec!["hoverriot", "hoverdepthcharge"]);
        let Value::Object(p) = json!({"want": "floor(0.2 * cores)", "fire_at_radar": false}) else { panic!() };
        d.roles[0].apply(&p).unwrap();
        assert_eq!(d.roles[0].want.as_ref().map(Expr::source), Some("floor(0.2 * cores)"));
        assert_eq!(d.roles[0].fire_at_radar, Some(false));
        let Value::Object(bad) = json!({"want": "ceil(cores"}) else { panic!() };
        assert!(d.roles[0].apply(&bad).is_err());
        let Value::Object(bad) = json!({"orbit": 5000}) else { panic!() };
        assert!(d.roles[0].apply(&bad).is_err());
        let Value::Object(bad) = json!({"speed": 1}) else { panic!() };
        assert!(d.apply(&bad).is_err());
    }

    #[test]
    fn wants_read_the_force() {
        let classes = Classes::default();
        let impaler = seeds().into_iter().find(|d| d.name == "impaler").unwrap();
        let force = |guns: usize| impaler.force((0..guns).map(|_| ("vehheavyarty", 700.0)), &classes);
        let (screen, aa) = (&impaler.roles[0], &impaler.roles[1]);
        assert_eq!((screen.want(&force(1)), screen.want(&force(4))), (Some(1), Some(2)));
        assert_eq!((aa.want(&force(2)), aa.want(&force(3))), (Some(0), Some(1)));
        let f = impaler
            .force([("vehheavyarty", 700.0), ("vehriot", 220.0), ("vehaa", 220.0), ("cloakraid", 65.0)], &classes);
        assert_eq!((f.cores, f.roles[0].1, f.roles[1].1), (1, 1, 1));
        let r = Role { want: Some(Expr::per_core(1.5)), ..Role::new("x") };
        assert_eq!((r.want(&Force { cores: 2, ..Force::default() })), Some(3));
        assert_eq!(Role::fill("y", &["b"], 90.0).want(&Force::default()), None);
    }

    #[test]
    fn low_shields_hysteresis() {
        let shape = Shape::default();
        assert!(!shape.low_shields(false, 0.6));
        assert!(shape.low_shields(false, 0.4));
        assert!(shape.low_shields(true, 0.7));
        assert!(!shape.low_shields(true, 0.85));
    }

    #[test]
    fn ravagers_are_a_composition() {
        // A Leveler per five Ravagers, grown without end, at the Levelers'
        // pace, going in on value alone (no muster clock).
        let mut d = seeds().into_iter().find(|d| d.name == "ravager").unwrap();
        assert_eq!((d.cores_per_force, d.muster, d.pace_slowest), (0, 0.0, 0.85));
        assert!(!d.regroup && !d.reinforce_engaged && !d.strays);
        let Value::Object(p) = json!({"cores_per_force": 0, "pace_slowest": 0.5}) else { panic!() };
        d.apply(&p).unwrap();
        assert_eq!((d.cores_per_force, d.pace_slowest), (0, 0.5));
        let Value::Object(p) = json!({"pace_slowest": 2}) else { panic!() };
        assert!(d.apply(&p).is_err());
    }

    #[test]
    fn classes_take_what_no_role_names() {
        let mut classes = Classes::default();
        classes.insert("@direct", &["vehriot", "vehassault", "vehraid"]);
        let mut d = Doctrine::new("x");
        d.core = vec!["vehheavyarty".into()];
        d.roles = vec![
            Role::escort("any", &["@direct"], "cores", 100.0, 0.0),
            Role::escort("screen", &["vehriot"], "1", 100.0, 0.0),
        ];
        assert_eq!(d.role_of("vehriot", &classes), Some(1));
        assert_eq!(d.role_of("vehraid", &classes), Some(0));
        assert_eq!(d.role_of("cloakraid", &classes), None);
    }
}
