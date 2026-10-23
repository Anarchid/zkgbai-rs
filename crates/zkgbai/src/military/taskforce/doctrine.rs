//! Doctrines: what a combined-arms taskforce is made of and how it fights,
//! as data.
//!
//! A doctrine names its core (the lynchpin a force forms around: a Felon,
//! a Lance), the roles that join the core (each a list of unit types, how
//! many per core or as many as come, how far from the leader they orbit)
//! and the force's conduct: its pace, where it aims once engaged, what it
//! musters before engaging, and when it falls back. A force missing some
//! of its roles still fights as it is.
//!
//! The seeds are the shield ball as it always was (`felon`) and siege
//! forces around fragile long-range units (`lance`, `phantom`, `tremor`,
//! `impaler`). The operator edits them in flight (see `ops`), as does a
//! file named by the `doctrines` option.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{Map, Value, json};

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
}

impl Aim {
    pub fn name(self) -> &'static str {
        match self {
            Aim::Assault => "assault",
            Aim::Artillery => "artillery",
            Aim::Standoff => "standoff",
        }
    }

    fn parse(s: &str) -> Result<Aim, String> {
        match s {
            "assault" => Ok(Aim::Assault),
            "artillery" => Ok(Aim::Artillery),
            "standoff" => Ok(Aim::Standoff),
            _ => Err(format!("aim '{s}': assault, artillery or standoff")),
        }
    }
}

/// Units that join a core.
#[derive(Clone, Debug, PartialEq)]
pub struct Role {
    pub name: String,
    /// Unit types (internal names).
    pub units: Vec<String>,
    /// How many a force wants per core; `None`: all that come.
    pub per_core: Option<f32>,
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
}

impl Role {
    pub fn new(name: &str) -> Role {
        Role {
            name: name.to_owned(),
            units: Vec::new(),
            per_core: Some(1.0),
            orbit: 120.0,
            to_strongest: false,
            gauge: true,
            switch_on: false,
            produce: 0.0,
        }
    }

    fn fill(name: &str, units: &[&str], orbit: f32) -> Role {
        Role { per_core: None, orbit, units: units.iter().map(|u| (*u).to_owned()).collect(), ..Role::new(name) }
    }

    fn escort(name: &str, units: &[&str], per_core: f32, orbit: f32, produce: f32) -> Role {
        Role {
            per_core: Some(per_core),
            orbit,
            produce,
            units: units.iter().map(|u| (*u).to_owned()).collect(),
            ..Role::new(name)
        }
    }

    /// How many a force with `cores` cores wants; `None`: all that come.
    pub fn want(&self, cores: usize) -> Option<usize> {
        self.per_core.map(|p| (p * cores as f32).ceil() as usize)
    }

    pub fn to_json(&self) -> Value {
        json!({
            "name": self.name,
            "units": self.units,
            "per_core": self.per_core,
            "orbit": self.orbit,
            "to_strongest": self.to_strongest,
            "gauge": self.gauge,
            "switch_on": self.switch_on,
            "produce": self.produce,
        })
    }

    /// Change the fields `patch` names.
    pub fn apply(&mut self, patch: &Map<String, Value>) -> Result<(), String> {
        for (k, v) in patch {
            match k.as_str() {
                "name" => self.name = string(k, v)?,
                "units" => self.units = names(k, v)?,
                "per_core" => {
                    self.per_core = match v {
                        Value::Null => None,
                        Value::String(s) if s == "all" || s == "fill" => None,
                        _ => Some(num(k, v, 0.0, 20.0)?),
                    }
                }
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
    /// The unit types a force forms around.
    pub core: Vec<String>,
    pub roles: Vec<Role>,
    /// Cores a force takes before a new core starts its own.
    pub cores_per_force: u32,
    /// With allies, every core joins one force (one giant death ball).
    pub pool_with_allies: bool,
    /// The leader's speed limit, elmos/s (0: none).
    pub pace: f32,
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
}

impl Doctrine {
    pub fn new(name: &str) -> Doctrine {
        Doctrine {
            name: name.to_owned(),
            note: String::new(),
            enabled: true,
            core: Vec::new(),
            roles: Vec::new(),
            cores_per_force: 1,
            pool_with_allies: false,
            pace: 0.0,
            aim: Aim::Artillery,
            engage_value: 0.0,
            muster: 60.0,
            retreat_health: 0.6,
            shields_low: 0.5,
            shields_ok: 0.8,
            rallied: 0.75,
            flee_threat: true,
        }
    }

    pub fn role(&self, name: &str) -> Option<&Role> {
        self.roles.iter().find(|r| r.name == name)
    }

    /// The index of the first role taking unit type `unit`.
    pub fn role_of(&self, unit: &str) -> Option<usize> {
        self.roles.iter().position(|r| r.units.iter().any(|u| u == unit))
    }

    pub fn is_core(&self, unit: &str) -> bool {
        self.core.iter().any(|c| c == unit)
    }

    pub fn to_json(&self) -> Value {
        json!({
            "name": self.name,
            "note": self.note,
            "enabled": self.enabled,
            "core": self.core,
            "roles": self.roles.iter().map(Role::to_json).collect::<Vec<_>>(),
            "cores_per_force": self.cores_per_force,
            "pool_with_allies": self.pool_with_allies,
            "pace": self.pace,
            "aim": self.aim.name(),
            "engage_value": self.engage_value,
            "muster": self.muster,
            "retreat_health": self.retreat_health,
            "shields_low": self.shields_low,
            "shields_ok": self.shields_ok,
            "rallied": self.rallied,
            "flee_threat": self.flee_threat,
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
                "cores_per_force" => self.cores_per_force = num(k, v, 1.0, 20.0)? as u32,
                "pool_with_allies" => self.pool_with_allies = flag(k, v)?,
                "pace" => self.pace = num(k, v, 0.0, 500.0)?,
                "aim" => self.aim = Aim::parse(&string(k, v)?)?,
                "engage_value" => self.engage_value = num(k, v, 0.0, 1e6)?,
                "muster" => self.muster = num(k, v, 0.0, 3600.0)?,
                "retreat_health" => self.retreat_health = num(k, v, 0.0, 1.0)?,
                "shields_low" => self.shields_low = num(k, v, 0.0, 1.0)?,
                "shields_ok" => self.shields_ok = num(k, v, 0.0, 1.0)?,
                "rallied" => self.rallied = num(k, v, 0.0, 1.0)?,
                "flee_threat" => self.flee_threat = flag(k, v)?,
                _ => return Err(format!("doctrines have no field '{k}' (see doctrines)")),
            }
        }
        Ok(())
    }

    /// Whether the doctrine makes sense: known unit types, a core, roles
    /// with distinct names, shield thresholds in order. Unit types may
    /// appear in one role only, and never as both core and role.
    pub fn check(&self, defs: &Defs) -> Result<(), String> {
        if self.core.is_empty() {
            return Err(format!("doctrine {}: no core", self.name));
        }
        let mut seen: BTreeSet<&str> = BTreeSet::new();
        for u in &self.core {
            if defs.get(u).is_none_or(|d| d.speed() <= 0.0) {
                return Err(format!("doctrine {}: core '{u}' is no mobile unit type", self.name));
            }
            seen.insert(u);
        }
        let mut role_names = BTreeSet::new();
        for r in &self.roles {
            if !role_names.insert(r.name.as_str()) {
                return Err(format!("doctrine {}: two roles named {}", self.name, r.name));
            }
            for u in &r.units {
                if defs.get(u).is_none_or(|d| d.speed() <= 0.0) {
                    return Err(format!("doctrine {}: role {}: '{u}' is no mobile unit type", self.name, r.name));
                }
                if !seen.insert(u) {
                    return Err(format!("doctrine {}: '{u}' is listed twice", self.name));
                }
            }
        }
        if self.shields_low > self.shields_ok {
            return Err(format!("doctrine {}: shields_low above shields_ok", self.name));
        }
        Ok(())
    }

    /// What the squad wrapper needs of the doctrine, by unit def id.
    pub fn shape(&self, defs: &Defs) -> Shape {
        let mut by_def = BTreeMap::new();
        for (i, r) in self.roles.iter().enumerate() {
            for u in &r.units {
                by_def.entry(defs.id(u)).or_insert(i);
            }
        }
        Shape {
            doctrine: self.name.clone(),
            core: self.core.iter().map(|c| defs.id(c)).filter(|&id| id >= 0).collect(),
            roles: (self.roles.iter())
                .map(|r| RoleShape { orbit: r.orbit, gauge: r.gauge, switch_on: r.switch_on })
                .collect(),
            by_def,
            pace: self.pace,
            fight: self.aim == Aim::Artillery,
            rallied: self.rallied,
        }
    }
}

/// A role as the squad wrapper sees it.
#[derive(Clone, Debug, PartialEq)]
pub struct RoleShape {
    pub orbit: f32,
    pub gauge: bool,
    pub switch_on: bool,
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
    /// The leader advances under fight orders (stopping where it reaches)
    /// rather than move orders.
    pub fight: bool,
    pub rallied: f32,
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
            fight: false,
            rallied: 0.75,
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

    let mut lance = Doctrine::new("lance");
    lance.note = "Lances (980 range, 1000 hp) under a Mace screen against raiders, Scalpels and Halberds \
                  as the line, a Flail when one is about."
        .into();
    lance.core = vec!["hoverarty".into()];
    lance.roles = vec![
        Role::escort("screen", &["hoverriot"], 2.0, 130.0, 0.5),
        Role::escort("line", &["hoverskirm", "hoverassault"], 2.0, 170.0, 0.0),
        Role::escort("aa", &["hoveraa"], 1.0, 80.0, 0.0),
    ];
    lance.engage_value = 1500.0;
    lance.muster = 75.0;
    lance.retreat_health = 0.7;

    let mut phantom = Doctrine::new("phantom");
    phantom.note = "Phantoms (700 range, 560 hp) behind Reavers against raiders, Knights and Ronins as the \
                    line, a Gremlin when one is about."
        .into();
    phantom.core = vec!["cloaksnipe".into()];
    phantom.roles = vec![
        Role::escort("screen", &["cloakriot"], 2.0, 110.0, 0.5),
        Role::escort("line", &["cloakassault", "cloakskirm"], 2.0, 160.0, 0.0),
        Role::escort("aa", &["cloakaa"], 1.0, 70.0, 0.0),
    ];
    phantom.engage_value = 1100.0;
    phantom.muster = 75.0;
    phantom.retreat_health = 0.7;

    let mut tremor = Doctrine::new("tremor");
    tremor.note = "Tremors with an Ogre against raiders, a Minotaur as the line, an Ettin when one is about.".into();
    tremor.core = vec!["tankheavyarty".into()];
    tremor.roles = vec![
        Role::escort("screen", &["tankriot"], 1.0, 140.0, 0.5),
        Role::escort("line", &["tankassault"], 1.0, 180.0, 0.0),
        Role::escort("aa", &["tankaa"], 1.0, 90.0, 0.0),
    ];
    tremor.engage_value = 2000.0;
    tremor.muster = 90.0;

    let mut impaler = Doctrine::new("impaler");
    impaler.note = "Impalers (1500 range) standing off behind our lines with Rippers against raiders, \
                    a Crasher when one is about."
        .into();
    impaler.core = vec!["vehheavyarty".into()];
    impaler.roles = vec![
        Role::escort("screen", &["vehriot"], 2.0, 120.0, 0.5),
        Role::escort("aa", &["vehaa"], 1.0, 80.0, 0.0),
    ];
    impaler.aim = Aim::Standoff;
    impaler.engage_value = 900.0;

    vec![felon, lance, phantom, tremor, impaler]
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
        assert!(felon.pool_with_allies && !felon.flee_threat);
        // Every role takes all that come; Aspis go to the strongest ball
        // and their area shields are not gauged.
        assert!(felon.roles.iter().all(|r| r.per_core.is_none() && r.produce == 0.0));
        let aspis = felon.role("aspis").unwrap();
        assert!(aspis.to_strongest && !aspis.gauge && aspis.orbit == 40.0);
        assert!(felon.role("outlaws").unwrap().switch_on);
        assert_eq!(felon.role("thugs").unwrap().orbit, 125.0);
        assert_eq!(felon.role("dominatrices").unwrap().orbit, 250.0);
        assert_eq!(felon.role_of("shieldarty"), Some(3));
        assert!(felon.is_core("shieldfelon") && felon.role_of("shieldfelon").is_none());
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
        assert_eq!(d.roles[0].per_core, None);
        assert_eq!(d.roles[0].units, vec!["hoverriot", "hoverdepthcharge"]);
        let Value::Object(bad) = json!({"orbit": 5000}) else { panic!() };
        assert!(d.roles[0].apply(&bad).is_err());
        let Value::Object(bad) = json!({"speed": 1}) else { panic!() };
        assert!(d.apply(&bad).is_err());
    }

    #[test]
    fn wants_round_up_per_core() {
        let r = Role::escort("x", &["a"], 1.5, 100.0, 0.0);
        assert_eq!((r.want(1), r.want(2), r.want(0)), (Some(2), Some(3), Some(0)));
        assert_eq!(Role::fill("y", &["b"], 90.0).want(3), None);
    }
}
