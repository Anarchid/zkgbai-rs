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
//! teams (`lance`, `phantom`, `envoy`; a doctrine's spotter role, see
//! `spotter`, sees for them), siege detachments (`tremor`,
//! `emissary`), Ravager packs (`ravager`, see `pack`) and cloaked bombs
//! (`ghostbombs`). A file named by the `doctrines` option patches them and
//! adds more (see `taskforce::load`).

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{Map, Value, json};

use super::Classes;
use super::expr::{Expr, Force};
use super::spotter::Spot;
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
    /// Always on the move as one body (Ravagers; see `pack`): at what it
    /// survives, circling it under move orders instead of stopping, back to
    /// its screen from raiders, stopping only to park for repairs.
    Pack,
}

const AIMS: [Aim; 7] = [Aim::Assault, Aim::Artillery, Aim::Standoff, Aim::Siege, Aim::Battery, Aim::Strike, Aim::Pack];

impl Aim {
    pub fn name(self) -> &'static str {
        match self {
            Aim::Assault => "assault",
            Aim::Artillery => "artillery",
            Aim::Standoff => "standoff",
            Aim::Siege => "siege",
            Aim::Battery => "battery",
            Aim::Strike => "strike",
            Aim::Pack => "pack",
        }
    }

    fn parse(s: &str) -> Result<Aim, String> {
        AIMS.into_iter().find(|a| a.name() == s).ok_or_else(|| {
            let names: Vec<&str> = AIMS.iter().map(|a| a.name()).collect();
            format!("aim '{s}': one of {}", names.join(", "))
        })
    }
}

/// How a pack runs at a target (see `pack`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Attack {
    /// Passing it on the line that brings the most guns within reach.
    Pass,
    /// Charging it head-on, aimed anew each conduct until close, then
    /// ramming through: units push mobiles out of their way, and a dodging
    /// target cannot sidestep what runs over it.
    Charge,
    /// Charging what moves, passing what stands.
    Mixed,
}

const ATTACKS: [Attack; 3] = [Attack::Pass, Attack::Charge, Attack::Mixed];

impl Attack {
    pub fn name(self) -> &'static str {
        match self {
            Attack::Pass => "pass",
            Attack::Charge => "charge",
            Attack::Mixed => "mixed",
        }
    }

    fn parse(s: &str) -> Result<Attack, String> {
        ATTACKS.into_iter().find(|a| a.name() == s).ok_or_else(|| {
            let names: Vec<&str> = ATTACKS.iter().map(|a| a.name()).collect();
            format!("attack '{s}': one of {}", names.join(", "))
        })
    }
}

/// How a pack judges whether a run is worth it (see `pack`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Judge {
    /// The threat maps: the target's threat less ours no more than ours
    /// where the pack stands.
    Map,
    /// The fight predictor (`military::fightsim`), asked about each
    /// engagement along the run in turn.
    Predictor,
    /// The same, by the predictor's mechanics model (air, burn, death
    /// blasts, EMP, slow, engagement odds; `fightsim::mech`).
    Mech,
    /// Planned by who dies (`fightsim::mech` fates): a run as a chain of
    /// short engagements along its way, each `fate_secs` long, worth the
    /// expected value of the enemies killed (statics `lasting` times
    /// theirs) less our own expected losses; the best of a few targets and
    /// of ramming or passing each wins (user, 2026-10-28).
    Fates,
}

const JUDGES: [Judge; 4] = [Judge::Map, Judge::Predictor, Judge::Mech, Judge::Fates];

impl Judge {
    pub fn name(self) -> &'static str {
        match self {
            Judge::Map => "map",
            Judge::Predictor => "predictor",
            Judge::Mech => "mech",
            Judge::Fates => "fates",
        }
    }

    fn parse(s: &str) -> Result<Judge, String> {
        JUDGES.into_iter().find(|j| j.name() == s).ok_or_else(|| {
            let names: Vec<&str> = JUDGES.iter().map(|j| j.name()).collect();
            format!("judge '{s}': one of {}", names.join(", "))
        })
    }
}

/// How a member keeps with its force's leader.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Keep {
    /// Circling it at `orbit` (ZK's circle guard): shieldbots round a
    /// Felon, for escorts faster than what they circle.
    Orbit,
    /// Guarding it (ZK's guard): following it, standing beside it when it
    /// stops, and striking back at what hits it: a Lance's Mace.
    Guard,
    /// Moving with it in formation: each move it gets, shifted by a place of
    /// their own round it (rings of 6, 12, ... `orbit` apart, on the map's
    /// axes, so the formation slides and never turns: nobody runs round the
    /// outside of a turn), at its pace, a little faster while behind their
    /// place and slower while ahead. For equals, which cannot circle one of
    /// their own: a Ravager pack. Only packs' conduct moves them (see
    /// `pack`).
    Formation,
    /// Not with it: following it at `orbit` behind on its way home, a
    /// screen of their own the force falls back to (a pack's Rippers; see
    /// `pack`). They go for repairs alone, and do not count as rallied.
    Trail,
}

const KEEPS: [Keep; 4] = [Keep::Orbit, Keep::Guard, Keep::Formation, Keep::Trail];

impl Keep {
    pub fn name(self) -> &'static str {
        match self {
            Keep::Orbit => "orbit",
            Keep::Guard => "guard",
            Keep::Formation => "formation",
            Keep::Trail => "trail",
        }
    }

    fn parse(s: &str) -> Result<Keep, String> {
        KEEPS.into_iter().find(|k| k.name() == s).ok_or_else(|| {
            let names: Vec<&str> = KEEPS.iter().map(|k| k.name()).collect();
            format!("keep '{s}': one of {}", names.join(", "))
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
    /// How they keep with the leader (circling it, guarding it, ...).
    pub keep: Keep,
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
    /// Spotters: they see for the core's guns instead of fighting (see
    /// `spotter`), each in its way.
    pub spot: Option<Spot>,
}

impl Role {
    pub fn new(name: &str) -> Role {
        Role {
            name: name.to_owned(),
            units: Vec::new(),
            want: Some(Expr::per_core(1.0)),
            orbit: 120.0,
            keep: Keep::Orbit,
            to_strongest: false,
            gauge: true,
            switch_on: false,
            produce: 0.0,
            fire_state: None,
            fire_at_radar: None,
            release: false,
            spot: None,
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

    /// Draw a ground spotter per gun from the standing army.
    fn spotter() -> Role {
        Role {
            fire_state: Some(0),
            fire_at_radar: Some(false),
            spot: Some(Spot::Watch),
            ..Role::escort(
                "spotter",
                &["hoverassault", "spiderantiheavy", "cloakheavyraid", "cloakaa"],
                "cores",
                120.0,
                0.0,
            )
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
            "keep": self.keep.name(),
            "to_strongest": self.to_strongest,
            "gauge": self.gauge,
            "switch_on": self.switch_on,
            "produce": self.produce,
            "fire_state": self.fire_state,
            "fire_at_radar": self.fire_at_radar,
            "release": self.release,
            "spot": self.spot.map(Spot::name),
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
                "spot" => self.spot = optional(v, |v| Spot::parse(&string(k, v)?))?,
                "orbit" => self.orbit = num(k, v, 20.0, 600.0)?,
                "keep" => self.keep = Keep::parse(&string(k, v)?)?,
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
    /// Going in, the leader also keeps to this share of its slowest
    /// member's speed (0: no), so a slower screen keeps up (Levelers with
    /// Ravagers); forming, rallying or falling back, it does not.
    pub pace_slowest: f32,
    pub aim: Aim,
    /// A force engages once worth this much metal (and rallied)...
    pub engage_value: f32,
    /// ...or, given, worth more than this, over the force and our `income`
    /// (`"min(2500, max(1200, 45 * income))"`, as assault squads complete);
    /// engaged, it then stays in while worth more than `engage_value`...
    pub engage: Option<Expr>,
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
    /// Its cores, firing at will, are each set on the most valuable target
    /// in reach that one shot kills, of those whose place we know exactly
    /// (Lances, Phantoms; see `marksman`).
    pub marksman: bool,
    /// Marksmen: a gun keeps its mark until it has fired (or the mark is
    /// gone), picking one only as it reloads; else it re-picks every pass.
    pub mark_hold: bool,
    /// Marksmen: a mark is worth setting from this share of what a volley
    /// of the gun's type has killed so far (0: a tenth of the gun's cost).
    pub mark_share: f32,
    /// Marksmen fire only with a fix in sight: marks only what is in sight
    /// (no building ghosts); give the core `fire_state` 0 so that nothing
    /// else is shot at (a loaded gun with no mark waits), and `okp` 4 to
    /// keep ZK's overkill prevention on hold fire.
    pub losfire: bool,
    /// The core's overkill prevention state (see `cmd::PREVENT_OVERKILL`);
    /// `None`: ZK's default.
    pub okp: Option<i32>,
    /// Marksmen hold a loaded gun's fire up to `mark_hold_for` seconds for
    /// a target worth this many times the best in reach that will be
    /// hittable by then, moving it up to `mark_move` (0: never; see
    /// `marksman`).
    pub mark_wait_ratio: f32,
    pub mark_hold_for: f32,
    pub mark_move: f32,
    /// How cores other than the leader keep with it (see [`Keep`]), and at
    /// what `orbit`.
    pub core_keep: Keep,
    pub core_orbit: f32,
    /// Packs (see `pack`): the cores it takes to go in, and to stay in.
    pub cores_to_go: u32,
    pub cores_to_stay: u32,
    /// Packs: below `retreat_health` they park for repairs until their
    /// cores average this (0: until back above `retreat_health`).
    pub heal_to: f32,
    /// Packs: a pass counts the guns that come within this share of their
    /// reach of the target.
    pub pass_reach: f32,
    /// Packs: enemy raiders worth this much metal near one send it back to
    /// its screen (0: never).
    pub rip_value: f32,
    /// Local sweep radius (zero disables it) and minimum cluster value.
    pub sweep: f32,
    pub sweep_min: f32,
    /// Packs: how they run at a target.
    pub attack: Attack,
    /// Packs: a run begun is finished through the target before the pack
    /// judges threat, raiders or its health again.
    pub commit: bool,
    /// Packs: targets, run lines and ground under more enemy porc than
    /// `porc_share` of the pack's own threat are shunned (off: porc is
    /// not looked at).
    pub shun_porc: bool,
    pub porc_share: f32,
    /// Packs: after `quiet` seconds without firing, members below
    /// `repair_below` health go for repairs if their own route is clear.
    /// Weak packs near a haven can also rest when healing beats the next run.
    /// Zero `repair_below` disables discretionary repairs.
    pub quiet: f32,
    pub repair_below: f32,
    /// Packs: how a run is judged worth going on, and (the predictor) by how
    /// much metal its expected kills must beat its expected losses.
    pub judge: Judge,
    pub judge_margin: f32,
    /// Packs judged by fates: each engagement's length (s), and what a
    /// static killed (economy, turrets) is worth over its metal.
    pub fate_secs: f32,
    pub lasting: f32,
    /// It sleeps (its units fight as they would without it) until more than
    /// this share of the armed ground units on the map are not raiders: the
    /// game's middle (0: awake from the start; see `taskforce_wake`).
    pub wake_share: f32,
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
            engage: None,
            muster: 60.0,
            retreat_health: 0.6,
            shields_low: 0.5,
            shields_ok: 0.8,
            rallied: 0.75,
            flee_threat: true,
            strays: false,
            regroup: false,
            reinforce_engaged: false,
            marksman: false,
            mark_hold: true,
            mark_share: 0.5,
            losfire: false,
            okp: None,
            mark_wait_ratio: 0.0,
            mark_hold_for: 3.0,
            mark_move: 150.0,
            core_keep: Keep::Orbit,
            core_orbit: ORBIT,
            cores_to_go: 0,
            cores_to_stay: 0,
            heal_to: 0.0,
            pass_reach: 0.7,
            rip_value: 0.0,
            sweep: 0.0,
            sweep_min: 100.0,
            attack: Attack::Pass,
            commit: false,
            shun_porc: true,
            porc_share: 0.0,
            quiet: 5.0,
            repair_below: 0.0,
            judge: Judge::Map,
            judge_margin: 0.0,
            fate_secs: 5.0,
            lasting: 1.0,
            wake_share: 0.0,
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
        let mut v = json!({
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
            "engage": self.engage.as_ref().map(Expr::source),
            "muster": self.muster,
            "retreat_health": self.retreat_health,
            "shields_low": self.shields_low,
            "shields_ok": self.shields_ok,
            "rallied": self.rallied,
            "flee_threat": self.flee_threat,
            "strays": self.strays,
            "regroup": self.regroup,
            "reinforce_engaged": self.reinforce_engaged,
            "marksman": self.marksman,
            "mark_hold": self.mark_hold,
            "mark_share": self.mark_share,
            "losfire": self.losfire,
            "okp": self.okp,
            "mark_wait_ratio": self.mark_wait_ratio,
            "mark_hold_for": self.mark_hold_for,
            "mark_move": self.mark_move,
        });
        // Packs' fields (one json! with all of them overflows its recursion).
        let pack = json!({
            "core_keep": self.core_keep.name(),
            "core_orbit": self.core_orbit,
            "cores_to_go": self.cores_to_go,
            "cores_to_stay": self.cores_to_stay,
            "heal_to": self.heal_to,
            "pass_reach": self.pass_reach,
            "rip_value": self.rip_value,
            "sweep": self.sweep,
            "sweep_min": self.sweep_min,
            "attack": self.attack.name(),
            "commit": self.commit,
            "shun_porc": self.shun_porc,
            "porc_share": self.porc_share,
            "quiet": self.quiet,
            "repair_below": self.repair_below,
            "judge": self.judge.name(),
            "judge_margin": self.judge_margin,
            "fate_secs": self.fate_secs,
            "lasting": self.lasting,
            "wake_share": self.wake_share,
        });
        if let (Value::Object(m), Value::Object(p)) = (&mut v, pack) {
            m.extend(p);
        }
        v
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
                "engage" => {
                    self.engage = match v {
                        Value::Null => None,
                        Value::String(s) => Some(Expr::parse(s).map_err(|e| format!("engage: {e}"))?),
                        _ => return Err("'engage' must be an expression (a string) or null".into()),
                    }
                }
                "muster" => self.muster = num(k, v, 0.0, 3600.0)?,
                "retreat_health" => self.retreat_health = num(k, v, 0.0, 1.0)?,
                "shields_low" => self.shields_low = num(k, v, 0.0, 1.0)?,
                "shields_ok" => self.shields_ok = num(k, v, 0.0, 1.0)?,
                "rallied" => self.rallied = num(k, v, 0.0, 1.0)?,
                "flee_threat" => self.flee_threat = flag(k, v)?,
                "strays" => self.strays = flag(k, v)?,
                "regroup" => self.regroup = flag(k, v)?,
                "reinforce_engaged" => self.reinforce_engaged = flag(k, v)?,
                "marksman" => self.marksman = flag(k, v)?,
                "mark_hold" => self.mark_hold = flag(k, v)?,
                "mark_share" => self.mark_share = num(k, v, 0.0, 10.0)?,
                "losfire" => self.losfire = flag(k, v)?,
                "okp" => self.okp = optional(v, |v| Ok(num(k, v, 0.0, 4.0)? as i32))?,
                "mark_wait_ratio" => self.mark_wait_ratio = num(k, v, 0.0, 100.0)?,
                "mark_hold_for" => self.mark_hold_for = num(k, v, 0.0, 30.0)?,
                "mark_move" => self.mark_move = num(k, v, 0.0, 1000.0)?,
                "core_keep" => self.core_keep = Keep::parse(&string(k, v)?)?,
                "core_orbit" => self.core_orbit = num(k, v, 20.0, 600.0)?,
                "cores_to_go" => self.cores_to_go = num(k, v, 0.0, 100.0)? as u32,
                "cores_to_stay" => self.cores_to_stay = num(k, v, 0.0, 100.0)? as u32,
                "heal_to" => self.heal_to = num(k, v, 0.0, 1.0)?,
                "pass_reach" => self.pass_reach = num(k, v, 0.2, 1.0)?,
                "rip_value" => self.rip_value = num(k, v, 0.0, 1e6)?,
                "sweep" => self.sweep = num(k, v, 0.0, 5000.0)?,
                "sweep_min" => self.sweep_min = num(k, v, 0.0, 1e5)?,
                "attack" => self.attack = Attack::parse(&string(k, v)?)?,
                "commit" => self.commit = flag(k, v)?,
                "shun_porc" => self.shun_porc = flag(k, v)?,
                "porc_share" => self.porc_share = num(k, v, 0.0, 10.0)?,
                "quiet" => self.quiet = num(k, v, 1.0, 300.0)?,
                "repair_below" => self.repair_below = num(k, v, 0.0, 1.0)?,
                "judge" => self.judge = Judge::parse(&string(k, v)?)?,
                "judge_margin" => self.judge_margin = num(k, v, -1e5, 1e5)?,
                "fate_secs" => self.fate_secs = num(k, v, 1.0, 30.0)?,
                "lasting" => self.lasting = num(k, v, 0.0, 10.0)?,
                "wake_share" => self.wake_share = num(k, v, 0.0, 1.0)?,
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
        // Only packs' conduct moves formations and screens.
        let moved = |k: Keep| matches!(k, Keep::Formation | Keep::Trail);
        if self.aim != Aim::Pack && (moved(self.core_keep) || self.roles.iter().any(|r| moved(r.keep))) {
            return Err(format!("doctrine {}: keep formation or trail needs aim pack", self.name));
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
            if super::expr::GLOBALS.contains(&r.name.as_str()) {
                return Err(format!("doctrine {}: '{}' is no role name (expressions read it)", self.name, r.name));
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
        for n in self.engage.as_ref().map_or(Vec::new(), Expr::names) {
            if !role_names.contains(n) {
                return Err(format!("doctrine {}: engage reads '{n}', no role", self.name));
            }
        }
        for r in &self.roles {
            let Some(spot) = r.spot else { continue };
            for u in r.units.iter().filter(|u| !u.starts_with('@')) {
                let Some(d) = defs.get(u) else { continue };
                if let Err(e) = spot.fits(d) {
                    return Err(format!("doctrine {}: role {}: '{u}' {e}", self.name, r.name));
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
                    keep: r.keep,
                    gauge: r.gauge,
                    switch_on: r.switch_on,
                    fire_state: r.fire_state,
                    fire_at_radar: r.fire_at_radar,
                    spot: r.spot,
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
            engage: self.engage.clone(),
            muster: self.muster,
            retreat_health: self.retreat_health,
            shields_low: self.shields_low,
            shields_ok: self.shields_ok,
            flee_threat: self.flee_threat,
            strays: self.strays,
            regroup: self.regroup,
            reinforce_engaged: self.reinforce_engaged,
            marksman: self.marksman,
            mark_hold: self.mark_hold,
            mark_share: self.mark_share,
            losfire: self.losfire,
            okp: self.okp,
            mark_wait_ratio: self.mark_wait_ratio,
            mark_hold_for: self.mark_hold_for,
            mark_move: self.mark_move,
            fire_state: self.fire_state,
            fire_at_radar: self.fire_at_radar,
            core_keep: self.core_keep,
            core_orbit: self.core_orbit,
            cores_to_go: self.cores_to_go,
            cores_to_stay: self.cores_to_stay,
            heal_to: self.heal_to,
            pass_reach: self.pass_reach,
            rip_value: self.rip_value,
            sweep: self.sweep,
            sweep_min: self.sweep_min,
            attack: self.attack,
            commit: self.commit,
            shun_porc: self.shun_porc,
            porc_share: self.porc_share,
            quiet: self.quiet,
            repair_below: self.repair_below,
            judge: self.judge,
            judge_margin: self.judge_margin,
            fate_secs: self.fate_secs,
            lasting: self.lasting,
        }
    }
}

/// A role as the squad wrapper sees it.
#[derive(Clone, Debug, PartialEq)]
pub struct RoleShape {
    pub orbit: f32,
    pub keep: Keep,
    pub gauge: bool,
    pub switch_on: bool,
    pub fire_state: Option<i32>,
    pub fire_at_radar: Option<bool>,
    pub spot: Option<Spot>,
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
    /// Share of the slowest member's speed the leader keeps to going in
    /// (0: none).
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
    pub engage: Option<Expr>,
    pub muster: f32,
    pub retreat_health: f32,
    pub shields_low: f32,
    pub shields_ok: f32,
    pub flee_threat: bool,
    pub strays: bool,
    pub regroup: bool,
    pub reinforce_engaged: bool,
    pub marksman: bool,
    pub mark_hold: bool,
    pub mark_share: f32,
    pub losfire: bool,
    pub okp: Option<i32>,
    pub mark_wait_ratio: f32,
    pub mark_hold_for: f32,
    pub mark_move: f32,
    pub fire_state: Option<i32>,
    pub fire_at_radar: Option<bool>,
    pub core_keep: Keep,
    pub core_orbit: f32,
    pub cores_to_go: u32,
    pub cores_to_stay: u32,
    pub heal_to: f32,
    pub pass_reach: f32,
    pub rip_value: f32,
    /// Local sweep radius (zero disables it) and minimum cluster value.
    pub sweep: f32,
    pub sweep_min: f32,
    pub attack: Attack,
    pub commit: bool,
    pub shun_porc: bool,
    pub porc_share: f32,
    pub quiet: f32,
    pub repair_below: f32,
    pub judge: Judge,
    pub judge_margin: f32,
    pub fate_secs: f32,
    pub lasting: f32,
}

/// Orbit radius of members that are in no role (extra cores), unless the
/// doctrine says (`core_orbit`).
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
            engage: None,
            muster: 0.0,
            retreat_health: 0.6,
            shields_low: 0.5,
            shields_ok: 0.8,
            flee_threat: true,
            strays: false,
            regroup: false,
            reinforce_engaged: false,
            marksman: false,
            mark_hold: true,
            mark_share: 0.5,
            losfire: false,
            okp: None,
            mark_wait_ratio: 0.0,
            mark_hold_for: 3.0,
            mark_move: 150.0,
            fire_state: None,
            fire_at_radar: None,
            core_keep: Keep::Orbit,
            core_orbit: ORBIT,
            cores_to_go: 0,
            cores_to_stay: 0,
            heal_to: 0.0,
            pass_reach: 0.7,
            rip_value: 0.0,
            sweep: 0.0,
            sweep_min: 100.0,
            attack: Attack::Pass,
            commit: false,
            shun_porc: true,
            porc_share: 0.0,
            quiet: 5.0,
            repair_below: 0.0,
            judge: Judge::Map,
            judge_margin: 0.0,
            fate_secs: 5.0,
            lasting: 1.0,
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

    /// Whether members of this type are spotters (see `spotter`).
    pub fn spot(&self, def: Option<i32>) -> Option<Spot> {
        self.role(def).and_then(|r| r.spot)
    }

    pub fn orbit(&self, def: Option<i32>) -> f32 {
        self.role(def).map_or(self.core_orbit, |r| r.orbit)
    }

    /// How members of this type keep with the leader: their role's way,
    /// else the cores' (other members than roles' are cores).
    pub fn keep(&self, def: Option<i32>) -> Keep {
        self.role(def).map_or(self.core_keep, |r| r.keep)
    }

    /// Whether members of this type go their own ways rather than keep
    /// with the leader (spotters, trailing screens): not straggling, not
    /// counted as rallied, not in the force's health.
    pub fn own_way(&self, def: Option<i32>) -> bool {
        self.spot(def).is_some() || self.keep(def) == Keep::Trail
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
    lance.note = "Lances (980 range, 3000 a shot) with a Mace, or two Bolas, against the raiders that pick \
                  them off alone, guarding it (beside it, striking back at what hits it) rather than \
                  circling it; firing at will, each set on the best target in reach one shot kills. On \
                  since the 2026-10-24 Circuit read (ab16: army@15 +1402, 15-3-2 against 12-7-1); \
                  disable=lance switches it off."
        .into();
    lance.core = vec!["hoverarty".into()];
    lance.roles = vec![
        Role { keep: Keep::Guard, ..Role::escort("mace", &["hoverriot"], "cores", 130.0, 0.5) },
        Role {
            keep: Keep::Guard,
            ..Role::escort("bolas", &["hoverheavyraid"], "2 * max(0, cores - mace)", 150.0, 0.0)
        },
        Role::spotter(),
    ];
    lance.marksman = true;
    lance.engage_value = 1400.0;
    lance.muster = 0.0;
    lance.retreat_health = 0.7;

    let mut phantom = Doctrine::new("phantom");
    phantom.note = "Phantoms (700 range, 1500 a shot) with a spotter per gun; firing at will, \
                    each set on the best target in reach one shot kills."
        .into();
    phantom.enabled = false;
    phantom.core = vec!["cloaksnipe".into()];
    phantom.roles = vec![Role::spotter()];
    phantom.marksman = true;
    phantom.engage_value = 900.0;
    phantom.muster = 0.0;
    phantom.retreat_health = 0.7;

    let mut envoy = Doctrine::new("envoy");
    envoy.note = "Envoys (1200 range, 600 a shell every 7.3 s, fired on the move) with a Siren each, which \
                  tanks for them and takes riot and anti-sub duty (its Sonic Blaster hits under water: \
                  subs are SINK); firing at will, each set on the best target in reach one shell kills."
        .into();
    envoy.enabled = false;
    envoy.core = vec!["shiparty".into()];
    envoy.roles = vec![Role::escort("siren", &["shipassault"], "cores", 130.0, 0.5)];
    envoy.marksman = true;
    envoy.engage_value = 1800.0;
    envoy.muster = 0.0;
    envoy.retreat_health = 0.6;

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
    emissary.note = "An artillery detachment: Emissaries advance into range and fire at everything, \
                     Ogres against raiders, an Ettin past 3000 metal, and a standing spotter per gun."
        .into();
    emissary.enabled = false;
    emissary.core = vec!["tankarty".into()];
    emissary.cores_per_force = 4;
    emissary.roles = vec![
        Role::escort("screen", &["tankriot"], "ceil(0.5 * cores)", 140.0, 0.5),
        Role::escort("aa", &["tankaa"], "value > 3000 ? 1 : 0", 90.0, 0.0),
        Role::spotter(),
    ];
    // Use the implemented artillery conduct; siege stations remain a seed.
    emissary.aim = Aim::Artillery;
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
    ravager.note = "A Ravager pack (see `pack`), from the middle game (once most armed ground units, and most \
                    of the enemy's, are not raiders): five or more go in as one body, in a formation that slides \
                    with the leader's moves (60 apart: their shells go over each other), always on the move under \
                    move orders; they charge what moves and pass over what stands on the line that brings the \
                    most guns within 70% of their reach, finishing a run once begun; only at what the threat maps \
                    say they survive. From raiders they fall back to their Ripper screen, which trails them \
                    home-side at its own pace; below half health they park for repairs until 90%. Off by \
                    default: enable=ravager (Ravagers otherwise fight in the vehicle assault squads)."
        .into();
    ravager.core = vec!["vehassault".into()];
    ravager.core_keep = Keep::Formation;
    ravager.core_orbit = 60.0;
    ravager.cores_per_force = 0;
    ravager.roles =
        vec![Role { keep: Keep::Trail, ..Role::escort("screen", &["vehriot"], "ceil(cores / 3)", 450.0, 0.0) }];
    ravager.aim = Aim::Pack;
    ravager.pace = 80.0;
    ravager.muster = 0.0;
    ravager.retreat_health = 0.5;
    ravager.heal_to = 0.9;
    ravager.cores_to_go = 5;
    ravager.cores_to_stay = 3;
    ravager.rip_value = 400.0;
    // Charging what moves, passing what stands, committed: the best net of
    // the scenario runs (zkgbai-runs/pack/scen: +117 (87), +127 (85) metal
    // a scenario against uncommitted passes).
    ravager.attack = Attack::Mixed;
    ravager.commit = true;
    // Porc weighed, not shunned outright (user, 2026-10-28: five Ravagers
    // shied off from two Lotus), and the hurt sent for repairs between
    // fights.
    ravager.porc_share = 0.5;
    ravager.repair_below = 0.6;
    // Clear nearby value before taking the global picker's next destination.
    ravager.sweep = 1500.0;
    // In the sweep and fates judge, a dead mex or turret counts half again its metal.
    ravager.lasting = 1.5;
    // The middle game: once most armed ground units are not raiders (user,
    // 2026-10-28, for the full-play test).
    ravager.wake_share = 0.5;

    // Minotaurs as Ravagers, but a pack from one (user, 2026-10-28): Ogres
    // their screen.
    let mut minotaur = Doctrine {
        note: "A Minotaur pack, as the Ravagers' (see `ravager`) but one or more go in, an Ogre screen \
               trailing. Off by default: enable=minotaur."
            .into(),
        core: vec!["tankassault".into()],
        core_orbit: 75.0,
        roles: vec![Role { keep: Keep::Trail, ..Role::escort("screen", &["tankriot"], "ceil(cores / 3)", 450.0, 0.0) }],
        pace: 66.0,
        cores_to_go: 1,
        cores_to_stay: 1,
        ..ravager.clone()
    };
    minotaur.name = "minotaur".into();

    // Raider packs, a side experiment (user, 2026-10-28): the same conduct,
    // no screen, awake from the start.
    let raiders = |name: &str, unit: &str, pace: f32, orbit: f32, go: u32| Doctrine {
        name: name.into(),
        note: format!(
            "A {name} pack, as the Ravagers' (see `ravager`) but awake from the start, no screen, {go} or more \
             going in. Off by default: enable={name}."
        ),
        core: vec![unit.into()],
        core_orbit: orbit,
        roles: Vec::new(),
        pace,
        cores_to_go: go,
        cores_to_stay: 2,
        rip_value: 0.0,
        wake_share: 0.0,
        ..ravager.clone()
    };
    let scorcher = raiders("scorcher", "vehraid", 100.0, 45.0, 4);
    let dagger = raiders("dagger", "hoverraid", 130.0, 45.0, 4);
    let blitz = raiders("blitz", "tankheavyraid", 88.0, 55.0, 3);

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

    vec![
        felon, lance, phantom, envoy, tremor, emissary, impaler, ravager, minotaur, scorcher, dagger, blitz, ghostbombs,
    ]
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
    fn gun_doctrines_draw_one_hold_fire_spotter_per_core() {
        let classes = Classes::default();
        for (name, core) in [("lance", "hoverarty"), ("phantom", "cloaksnipe"), ("emissary", "tankarty")] {
            let d = seeds().into_iter().find(|d| d.name == name).unwrap();
            assert!(super::super::squad_aim(d.aim), "{name} needs a conduct that drives its spotters");
            let role = d.role("spotter").unwrap();
            assert_eq!((role.spot, role.fire_state, role.produce), (Some(Spot::Watch), Some(0), 0.0));
            let force = d.force([(core, 1000.0), (core, 1000.0)], &classes);
            assert_eq!(role.want(&force), Some(2));
            for unit in ["hoverassault", "spiderantiheavy", "cloakheavyraid", "cloakaa"] {
                assert!(role.units.iter().any(|u| u == unit));
                assert_eq!(d.role_of(unit, &classes), d.roles.iter().position(|r| r.name == "spotter"));
            }
        }
    }

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
        assert!(felon.roles.iter().all(|r| r.keep == Keep::Orbit) && felon.core_keep == Keep::Orbit);
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
    fn a_mace_or_two_bolas_per_lance() {
        let classes = Classes::default();
        let lance = seeds().into_iter().find(|d| d.name == "lance").unwrap();
        assert!(lance.marksman && lance.enabled);
        // Bodyguards, not orbiters.
        assert!(lance.roles.iter().filter(|r| r.spot.is_none()).all(|r| r.keep == Keep::Guard));
        let force = |units: &[&'static str]| lance.force(units.iter().map(|u| (*u, 100.0)), &classes);
        let (mace, bolas) = (&lance.roles[0], &lance.roles[1]);
        let one = force(&["hoverarty"]);
        assert_eq!((mace.want(&one), bolas.want(&one)), (Some(1), Some(2)));
        let maced = force(&["hoverarty", "hoverriot"]);
        assert_eq!((mace.want(&maced), bolas.want(&maced)), (Some(1), Some(0)));
        let two = force(&["hoverarty", "hoverarty", "hoverriot"]);
        assert_eq!((mace.want(&two), bolas.want(&two)), (Some(2), Some(2)));
    }

    #[test]
    fn engage_scales_with_income() {
        let mut d = seeds().into_iter().find(|d| d.name == "ravager").unwrap();
        let Value::Object(p) = json!({"engage": "max(1200, 45 * income)"}) else { panic!() };
        d.apply(&p).unwrap();
        let e = d.engage.as_ref().unwrap();
        let at = |income: f32| e.eval(&Force { income, ..Force::default() });
        assert_eq!((at(10.0), at(40.0)), (1200.0, 1800.0));
        assert!(e.names().is_empty());
        let Value::Object(p) = json!({"engage": "2 * guns"}) else { panic!() };
        d.apply(&p).unwrap();
        assert!(d.engage.as_ref().unwrap().names() == vec!["guns"]);
    }

    #[test]
    fn ravagers_are_a_pack() {
        // Five or more go in as one body, each in a place of its own, grown
        // without end; the Rippers trail as a screen of their own, at their
        // own pace; parked below half health until 90%.
        let mut d = seeds().into_iter().find(|d| d.name == "ravager").unwrap();
        assert_eq!((d.aim, d.core_keep, d.cores_per_force, d.pace_slowest), (Aim::Pack, Keep::Formation, 0, 0.0));
        assert_eq!((d.cores_to_go, d.cores_to_stay, d.retreat_health, d.heal_to), (5, 3, 0.5, 0.9));
        assert_eq!(d.wake_share, 0.5);
        assert_eq!((d.sweep, d.sweep_min, d.lasting), (1500.0, 100.0, 1.5));
        let Value::Object(p) = json!({"sweep": 0, "sweep_min": 250}) else { panic!() };
        d.apply(&p).unwrap();
        assert_eq!((d.sweep, d.sweep_min), (0.0, 250.0));
        assert_eq!((d.porc_share, d.quiet, d.repair_below), (0.5, 5.0, 0.6));
        assert!(!d.regroup && !d.reinforce_engaged && !d.strays);
        assert_eq!(d.role("screen").unwrap().keep, Keep::Trail);
        let Value::Object(p) = json!({"core_keep": "orbit", "pass_reach": 0.5, "pace_slowest": 0.85}) else { panic!() };
        d.apply(&p).unwrap();
        assert_eq!((d.core_keep, d.pass_reach, d.pace_slowest), (Keep::Orbit, 0.5, 0.85));
        let Value::Object(p) = json!({"core_keep": "circle"}) else { panic!() };
        assert!(d.apply(&p).is_err());
        assert_eq!((d.attack, d.commit, d.shun_porc), (Attack::Mixed, true, true));
        let Value::Object(p) = json!({"attack": "charge", "commit": true, "shun_porc": false}) else { panic!() };
        d.apply(&p).unwrap();
        assert_eq!((d.attack, d.commit, d.shun_porc), (Attack::Charge, true, false));
        let Value::Object(p) = json!({"attack": "ram"}) else { panic!() };
        assert!(d.apply(&p).is_err());
        assert_eq!(d.judge, Judge::Map);
        let Value::Object(p) = json!({"judge": "predictor", "judge_margin": 200}) else { panic!() };
        d.apply(&p).unwrap();
        assert_eq!((d.judge, d.judge_margin), (Judge::Predictor, 200.0));
        let Value::Object(p) = json!({"pace_slowest": 2}) else { panic!() };
        assert!(d.apply(&p).is_err());
        // Formations and screens only move under a pack's conduct.
        let mut felon = seeds().into_iter().find(|d| d.name == "felon").unwrap();
        let Value::Object(p) = json!({"core_keep": "formation"}) else { panic!() };
        felon.apply(&p).unwrap();
        let defs = Defs::new(crate::military::wrappers::squad::fake_engine::callback());
        assert!(felon.check(&defs, &Classes::default()).is_err_and(|e| e.contains("aim pack")));
    }

    #[test]
    fn minotaurs_and_raiders_pack_too() {
        let seeds = seeds();
        let get = |n: &str| seeds.iter().find(|d| d.name == n).unwrap();
        let m = get("minotaur");
        assert_eq!((m.core[0].as_str(), m.aim, m.cores_to_go, m.cores_to_stay), ("tankassault", Aim::Pack, 1, 1));
        assert_eq!((m.role("screen").unwrap().units[0].as_str(), m.wake_share), ("tankriot", 0.5));
        for (n, unit, go) in [("scorcher", "vehraid", 4), ("dagger", "hoverraid", 4), ("blitz", "tankheavyraid", 3)] {
            let d = get(n);
            assert_eq!((d.core[0].as_str(), d.cores_to_go, d.wake_share), (unit, go, 0.0));
            assert!(d.roles.is_empty() && d.aim == Aim::Pack && d.core_keep == Keep::Formation);
        }
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
