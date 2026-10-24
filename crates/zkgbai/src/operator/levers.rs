//! Levers: the AI's settings an operator may turn while it plays (the
//! dials, the home defense's budget), each described for a UI to draw as
//! a slider, a choice or a switch. `levers` lists them with their values,
//! `lever` turns one; the dashboard and the LuaUI widget draw whatever is
//! listed here. A lever turns through the tool that owns the setting
//! (`dial`, `qrf_set`), so the ranges and checks stay in one place.

use serde_json::{Value, json};

use super::actions;
use super::tools::Args;
use crate::Ai;
use crate::military::qrf::live::{CAP, GAIN, Mode};
use crate::military::qrf::ops as qrf_ops;

pub enum Kind {
    /// A number from `min` to `max` in steps of `step`; `log`: the slider
    /// is spaced by ratio.
    Range { min: f32, max: f32, step: f32, log: bool, format: Format },
    Choice(&'static [&'static str]),
    Switch,
}

#[derive(Clone, Copy)]
pub enum Format {
    /// x1.50
    Times,
    /// 30%
    Percent,
    /// 3
    Count,
}

/// A lever's state: its value (`null` when the AI's own choice has no
/// number), whether that is the AI's own choice, and a word on what it
/// does now.
pub struct Reading {
    pub value: Value,
    pub auto: bool,
    pub note: Option<String>,
}

pub struct Lever {
    pub name: &'static str,
    pub group: &'static str,
    pub label: &'static str,
    pub help: &'static str,
    pub kind: Kind,
    /// What the two ends of a range mean.
    pub ends: (&'static str, &'static str),
    pub read: fn(&Ai) -> Reading,
    /// Turns the lever; `None` puts it back to the AI's own choice.
    pub set: fn(&mut Ai, Option<&Value>) -> Result<(), String>,
}

impl Lever {
    fn listing(&self, ai: &Ai) -> Value {
        let r = (self.read)(ai);
        let mut v = json!({
            "name": self.name, "group": self.group, "label": self.label, "help": self.help,
            "value": r.value, "auto": r.auto,
        });
        match &self.kind {
            Kind::Range { min, max, step, log, format } => {
                v["kind"] = json!("range");
                v["min"] = json!(min);
                v["max"] = json!(max);
                v["step"] = json!(step);
                v["log"] = json!(log);
                v["format"] = json!(match format {
                    Format::Times => "times",
                    Format::Percent => "percent",
                    Format::Count => "count",
                });
                v["ends"] = json!([self.ends.0, self.ends.1]);
            }
            Kind::Choice(options) => {
                v["kind"] = json!("choice");
                v["options"] = json!(options);
            }
            Kind::Switch => v["kind"] = json!("switch"),
        }
        if let Some(n) = r.note {
            v["note"] = json!(n);
        }
        v
    }
}

fn r2(v: f32) -> Value {
    json!((f64::from(v) * 100.0).round() / 100.0)
}

fn num(v: &Value) -> Result<f32, String> {
    match v {
        Value::Number(n) => Ok(n.as_f64().unwrap_or(0.0) as f32),
        Value::String(s) => s.trim().trim_end_matches('%').parse::<f32>().map_err(|_| format!("'{s}' is not a number")),
        _ => Err("the value must be a number".into()),
    }
}

fn switch(v: &Value) -> Result<bool, String> {
    match v {
        Value::Bool(b) => Ok(*b),
        Value::Number(n) => Ok(n.as_f64().is_some_and(|f| f != 0.0)),
        Value::String(s) => match s.trim() {
            "on" | "true" | "1" | "yes" => Ok(true),
            "off" | "false" | "0" | "no" => Ok(false),
            s => Err(format!("'{s}': on or off")),
        },
        _ => Err("the value must be on or off".into()),
    }
}

fn word(v: &Value) -> Result<&str, String> {
    v.as_str().map(str::trim).ok_or_else(|| "the value must be one of the options".into())
}

/// Through an owning tool, its reply dropped.
fn via(ai: &mut Ai, run: fn(&mut Ai, &Args) -> Result<Value, String>, args: Value) -> Result<(), String> {
    run(ai, &Args(&args)).map(|_| ())
}

fn qrf_note(ai: &Ai) -> Option<String> {
    (!ai.qrf.on).then(|| "the QRF is off in this game (feature qrf)".into())
}

pub static LEVERS: &[Lever] = &[
    Lever {
        name: "greed",
        group: "Economy",
        label: "Greed",
        help: "The threat workers and jobs perceive is divided by it: far and contested metal spots get taken, \
               workers flee later.",
        kind: Kind::Range { min: 0.25, max: 4.0, step: 0.05, log: true, format: Format::Times },
        ends: ("careful", "greedy"),
        read: |ai| {
            let g = if ai.op.over.greed > 0.0 { ai.op.over.greed } else { 1.0 };
            Reading {
                value: r2(g),
                auto: (g - 1.0).abs() < 1e-3,
                note: (ai.op_greed() < g).then(|| "held at x1 while energy is stalled".into()),
            }
        },
        set: |ai, v| match v {
            None => {
                ai.op.over.greed = 0.0;
                Ok(())
            }
            Some(v) => via(ai, actions::dial, json!({"name": "greed", "value": num(v)?})),
        },
    },
    Lever {
        name: "buildpower",
        group: "Economy",
        label: "Build power",
        help: "Mobile build power the factories keep, as a multiple of metal income (the AI's own rule is one to \
               one; a strong human runs 1.3-1.7).",
        kind: Kind::Range { min: 0.5, max: 3.0, step: 0.05, log: false, format: Format::Times },
        ends: ("lean", "builders"),
        read: |ai| {
            let b = ai.op_buildpower();
            Reading { value: r2(b), auto: (b - 1.0).abs() < 1e-3, note: None }
        },
        set: |ai, v| match v {
            None => {
                ai.op.over.buildpower = 0.0;
                Ok(())
            }
            Some(v) => via(ai, actions::dial, json!({"name": "buildpower", "value": num(v)?})),
        },
    },
    Lever {
        name: "line",
        group: "Army",
        label: "Army line",
        help: "Where the army stands when nothing calls it, as a share of the way from our base to the enemy's \
               center. Auto: the AI's own rally.",
        kind: Kind::Range { min: 0.05, max: 0.9, step: 0.05, log: false, format: Format::Percent },
        ends: ("home", "forward"),
        read: |ai| {
            let l = ai.op.over.line;
            Reading {
                value: if l > 0.0 { r2(l) } else { Value::Null },
                auto: l <= 0.0,
                note: (l > 0.0 && ai.op_line_point().is_none()).then(|| "no line yet: the enemy's center is unknown".into()),
            }
        },
        set: |ai, v| {
            let l = match v {
                None => 0.0,
                Some(v) => num(v)?.max(0.05),
            };
            via(ai, actions::dial, json!({"name": "line", "value": l}))
        },
    },
    Lever {
        name: "qrf_defense",
        group: "Home defense",
        label: "Budget",
        help: "Raiders held back to keep raids off our economy, as a share of the mobile army. 0: none (forming \
               squads still moonlight). Auto: none until something is lost to a raid, then sized to the raids \
               that cost us, at most the ceiling.",
        kind: Kind::Range { min: 0.0, max: 1.0, step: 0.05, log: false, format: Format::Percent },
        ends: ("none", "all home"),
        read: |ai| {
            let b = &ai.qrf.budget;
            let share = |metal: f32| if b.army > 0.0 { r2((metal / b.army).clamp(0.0, 1.0)) } else { Value::Null };
            let value = match b.mode {
                Mode::Off => json!(0.0),
                Mode::Share(s) => r2(s),
                Mode::Auto | Mode::Metal(_) => share(b.wanted),
            };
            let set_as = match b.mode {
                Mode::Metal(m) => format!(" (set as {m:.0} metal)"),
                _ => String::new(),
            };
            let note = qrf_note(ai).unwrap_or_else(|| {
                format!("wants {:.0}, has {:.0} (army {:.0}){set_as}: {}", b.wanted, b.have, b.army, b.reason)
            });
            Reading { value, auto: b.mode == Mode::Auto, note: Some(note) }
        },
        set: |ai, v| {
            let defense = match v {
                None => "auto".to_owned(),
                Some(Value::String(s)) if matches!(s.trim(), "auto" | "off") => s.trim().to_owned(),
                Some(v) => match num(v)? {
                    s if s <= 0.0 => "off".to_owned(),
                    s => format!("{}%", (s.min(1.0) * 100.0).round()),
                },
            };
            via(ai, qrf_ops::set, json!({"defense": defense}))
        },
    },
    Lever {
        name: "qrf_cap",
        group: "Home defense",
        label: "Auto ceiling",
        help: "On auto, the most of the mobile army the home defense holds back.",
        kind: Kind::Range { min: 0.0, max: 1.0, step: 0.05, log: false, format: Format::Percent },
        ends: ("", ""),
        read: |ai| {
            let c = ai.qrf.budget.cap;
            Reading { value: r2(c), auto: (c - CAP).abs() < 1e-3, note: None }
        },
        set: |ai, v| via(ai, qrf_ops::set, json!({"cap": v.map_or(Ok(CAP), num)?})),
    },
    Lever {
        name: "qrf_gain",
        group: "Home defense",
        label: "Auto gain",
        help: "On auto, a team's power as a multiple of the strongest raid that cost us.",
        kind: Kind::Range { min: 0.5, max: 4.0, step: 0.1, log: false, format: Format::Times },
        ends: ("", ""),
        read: |ai| {
            let g = ai.qrf.budget.gain;
            Reading { value: r2(g), auto: (g - GAIN).abs() < 1e-3, note: None }
        },
        set: |ai, v| via(ai, qrf_ops::set, json!({"gain": v.map_or(Ok(GAIN), num)?})),
    },
    Lever {
        name: "qrf_size",
        group: "Home defense",
        label: "Auto sizing",
        help: "On auto, the raid a team is sized against: lossy (the raids that cost us; nothing until one did), \
               typical (a typical recent raid) or largest.",
        kind: Kind::Choice(&["lossy", "typical", "largest"]),
        ends: ("", ""),
        read: |ai| {
            let s = ai.qrf.budget.size.name();
            Reading { value: json!(s), auto: s == "lossy", note: None }
        },
        set: |ai, v| via(ai, qrf_ops::set, json!({"size": v.map_or(Ok("lossy"), word)?})),
    },
    Lever {
        name: "qrf_team_size",
        group: "Home defense",
        label: "Team size",
        help: "Raiders per dedicated team.",
        kind: Kind::Range { min: 1.0, max: 8.0, step: 1.0, log: false, format: Format::Count },
        ends: ("", ""),
        read: |ai| {
            let n = ai.qrf.team_size;
            Reading { value: json!(n), auto: n == 3, note: None }
        },
        set: |ai, v| via(ai, qrf_ops::set, json!({"team_size": v.map_or(Ok(3.0), num)?.round()})),
    },
    Lever {
        name: "qrf_moonlight",
        group: "Home defense",
        label: "Moonlighting",
        help: "Raider squads still forming answer raids at home while they fill.",
        kind: Kind::Switch,
        ends: ("", ""),
        read: |ai| {
            let on = !ai.qrf.no_moonlight;
            Reading { value: json!(on), auto: on, note: None }
        },
        set: |ai, v| via(ai, qrf_ops::set, json!({"moonlight": v.map_or(Ok(true), switch)?})),
    },
    Lever {
        name: "qrf_production",
        group: "Home defense",
        label: "Production",
        help: "Factories build raiders for the force while it is short of the budget (it costs the factories \
               their own picks).",
        kind: Kind::Switch,
        ends: ("", ""),
        read: |ai| {
            let on = ai.qrf.budget.production;
            Reading { value: json!(on), auto: !on, note: None }
        },
        set: |ai, v| via(ai, qrf_ops::set, json!({"production": v.map_or(Ok(false), switch)?})),
    },
    Lever {
        name: "qrf_algo",
        group: "Home defense",
        label: "Tactics",
        help: "How teams answer raids: mark (stand between the raid and our value, strike when stronger), cover, \
               station or intent (the research algorithms).",
        kind: Kind::Choice(&["mark", "cover", "station", "intent"]),
        ends: ("", ""),
        read: |ai| {
            let a = ai.qrf.algo.as_str();
            Reading { value: json!(a), auto: a == "mark", note: None }
        },
        set: |ai, v| via(ai, qrf_ops::set, json!({"algo": v.map_or(Ok("mark"), word)?})),
    },
];

pub fn list(ai: &mut Ai, _: &Args) -> Result<Value, String> {
    Ok(json!({"levers": LEVERS.iter().map(|l| l.listing(ai)).collect::<Vec<_>>()}))
}

pub fn set(ai: &mut Ai, a: &Args) -> Result<Value, String> {
    let name = a.str("name")?.trim();
    let lever = LEVERS.iter().find(|l| l.name == name).ok_or_else(|| {
        format!("unknown lever '{name}': {}", LEVERS.iter().map(|l| l.name).collect::<Vec<_>>().join(", "))
    })?;
    let value = a.get("value").filter(|v| v.as_str().is_none_or(|s| s.trim() != "auto"));
    (lever.set)(ai, value)?;
    Ok(lever.listing(ai))
}
