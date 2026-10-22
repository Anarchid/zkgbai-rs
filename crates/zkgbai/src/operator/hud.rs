//! What a pilot sees without asking: the HUD, a compact text block of the
//! AI's situation rendered for an LLM's context before each of its
//! inferences, and the feed, the operator stream digested into a few lines
//! per push (see [`super::mcpl`]).
//!
//! Both are configured by the operator (`hud` and `feed` tools) and shared
//! between the AI's thread, which renders, and the server's, which pushes.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::sync::Arc;

use serde_json::Value;
use springai::reserved;

use super::bus::Record;
use super::views::{def_by_id, game_time};
use crate::Ai;
use crate::military::SquadKind;

/// The HUD's sections, in rendering order, with what each shows.
pub const SECTIONS: &[(&str, &str)] = &[
    ("status", "time, resources, income, metal spots, territory, army values"),
    ("workers", "constructors: count, fleeing, reserved, the commander's position and task"),
    ("factories", "each factory, what it builds, operator production orders"),
    ("squads", "each squad: kind, status, position, target, size"),
    ("threats", "enemy incursions and the squads blocking them, places defended"),
    ("overrides", "what the operator has set: reserved units, requisition, defense points, muted modules, features off"),
    ("board", "the map as a picture (MCPL hosts only): terrain, grid in thousands of elmos, metal spots (ours blue, enemy red, free white), our units (commander yellow, workers green, fighters cyan, buildings blue), enemies (red; orange when remembered), enemy defenses with their reach, squad lines to targets, defended places in magenta"),
];

/// A read tool the HUD runs on every render, with its arguments.
#[derive(Clone, Debug)]
pub struct Pin {
    pub tool: String,
    pub args: Value,
}

/// Longest pinned result shown (characters of compact JSON).
pub const PIN_MAX_CHARS: usize = 2500;

/// Record kinds the feed pushes as they happen unless told otherwise.
pub const DEFAULT_FEED_KINDS: &[&str] = &["run", "end", "chat", "kill", "unit-", "enemy+", "idle", "given", "alert"];

#[derive(Clone, Debug)]
pub struct HudConfig {
    /// Sections shown, in [`SECTIONS`] order; empty for none.
    pub sections: Vec<String>,
    /// Read tools run on every render, results appended.
    pub pins: Vec<Pin>,
}

impl Default for HudConfig {
    fn default() -> Self {
        HudConfig {
            // The board is opt-in: it costs image tokens on every inference.
            sections: SECTIONS.iter().map(|(s, _)| (*s).to_owned()).filter(|s| s != "board").collect(),
            pins: Vec::new(),
        }
    }
}

impl HudConfig {
    pub fn shows_board(&self) -> bool {
        self.sections.iter().any(|s| s == "board")
    }
}

#[derive(Clone, Debug)]
pub struct FeedConfig {
    /// Kinds pushed as they happen (batched).
    pub kinds: BTreeSet<String>,
    /// Records within this long of each other go in one push.
    pub batch_ms: u64,
    /// A situation report (the HUD plus counts of everything since the
    /// last one) every this many seconds; 0 for none.
    pub sitrep_s: u64,
}

impl Default for FeedConfig {
    fn default() -> Self {
        FeedConfig {
            kinds: DEFAULT_FEED_KINDS.iter().map(|k| (*k).to_owned()).collect(),
            batch_ms: 3000,
            sitrep_s: 30,
        }
    }
}

fn n0(v: f32) -> i64 {
    v.round() as i64
}

fn xz(ai: &Ai, p: springai::Vec3) -> String {
    let _ = ai;
    format!("({},{})", p.x.round() as i64, p.z.round() as i64)
}

/// The HUD text for the sections in `cfg` (one line per item, no trailing
/// newline).
pub fn render(ai: &mut Ai, cfg: &HudConfig) -> String {
    let mut out = String::new();
    let line = |out: &mut String, s: String| {
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(&s);
    };
    let active = ai.active();
    for (name, _) in SECTIONS {
        if !cfg.sections.iter().any(|s| s == name) {
            continue;
        }
        match *name {
            "status" => {
                let e = ai.cb.economy();
                let (m, en) = (ai.metal, ai.energy);
                let mut s = format!(
                    "t={} {} | metal {} (+{:.1} -{:.1}, cap {}) energy {} (+{:.1} -{:.1})",
                    game_time(ai.frame),
                    ai.state(),
                    n0(e.current(m)),
                    e.income(m),
                    e.usage(m),
                    n0(e.storage(m)),
                    n0(e.current(en)),
                    e.income(en),
                    e.usage(en),
                );
                if active {
                    let _ = write!(
                        s,
                        " | income {:.1}{} | mexes {} ours, {} enemy, of {} | territory {:.0}%",
                        ai.eco.effective_income,
                        if ai.eco.estalled { " E-STALL" } else { "" },
                        ai.graph.get_owned_spots().len(),
                        ai.graph.get_enemy_spots().len(),
                        ai.graph.metal_spots.len(),
                        ai.graph.territory_fraction * 100.0,
                    );
                    line(&mut out, s);
                    line(
                        &mut out,
                        format!(
                            "army {} ({} fighters, {} squads, {} retreating) | enemy SEEN: army {} porc {} ({} units known, {} defenses, {:.0}% outrange light raiders) | enemy ESTIMATED from its income: produced {} so far, {} of it unaccounted for (in the fog)",
                            n0(ai.fac.army_value),
                            ai.fighters.len(),
                            ai.squads.ids().len(),
                            ai.retreat_handler.num_retreating(),
                            n0(ai.war.enemy_fighter_value),
                            n0(ai.war.enemy_porc_value),
                            ai.war.targets.len(),
                            ai.war.enemy_porcs.len(),
                            ai.war.outranging_share * 100.0,
                            n0(ai.intel.produced),
                            n0(ai.intel.unaccounted()),
                        ),
                    );
                } else {
                    line(&mut out, s);
                }
            }
            "workers" if active => {
                let ws = &ai.eco.workers;
                let fleeing = ws.values().filter(|w| w.is_chicken).count();
                let reserved = ws.values().filter(|w| reserved::is_reserved(ai.cb, w.id)).count();
                let mut s = format!("workers {} ({} fleeing, {} reserved)", ws.len(), fleeing, reserved);
                for w in ws.values().filter(|w| w.is_com) {
                    let task = w
                        .task
                        .and_then(|t| ai.eco.tasks.get(t))
                        .and_then(|t| t.as_construction().map(|c| ai.defs.name(c.build_type).to_owned()))
                        .unwrap_or_else(|| "no construction".into());
                    let _ = write!(
                        s,
                        " | commander #{} {} at {} hp {:.0}% {}{}",
                        w.id,
                        ai.defs.name_of(w.unit),
                        xz(ai, w.unit.pos()),
                        w.unit.health() / w.unit.max_health().max(1.0) * 100.0,
                        task,
                        if reserved::is_reserved(ai.cb, w.id) { " (reserved)" } else { "" }
                    );
                }
                line(&mut out, s);
            }
            "factories" if active => {
                let mut s = String::from("factories:");
                if ai.fac.factories.is_empty() {
                    s.push_str(" none");
                }
                for f in ai.fac.factories.values() {
                    let queue: Vec<&str> = f
                        .unit
                        .current_commands()
                        .iter()
                        .filter(|c| c.id() < 0)
                        .filter_map(|c| def_by_id(ai, -c.id()))
                        .map(|d| ai.defs.name(d))
                        .take(3)
                        .collect();
                    let _ = write!(
                        s,
                        " #{} {} at {} building [{}]",
                        f.id,
                        ai.defs.name_of(f.unit),
                        xz(ai, f.get_pos()),
                        queue.join(",")
                    );
                }
                let orders: Vec<String> = ai
                    .op
                    .over
                    .fac_orders
                    .iter()
                    .map(|o| format!("{}x{}", o.left, o.unit))
                    .collect();
                if !orders.is_empty() {
                    let _ = write!(s, " | operator orders: {}", orders.join(" "));
                }
                if let Some(r) = &ai.eco.requisition {
                    let _ = write!(s, " | wants {}", r.0);
                }
                line(&mut out, s);
            }
            "squads" if active => {
                let ids = ai.squads.ids();
                if ids.is_empty() {
                    line(&mut out, "squads: none".into());
                } else {
                    let mut s = String::from("squads:");
                    for sid in ids {
                        let Some(sq) = ai.squads.get(sid) else { continue };
                        let kind = match sq.kind {
                            SquadKind::Assault => "assault",
                            SquadKind::Raider => "raider",
                            SquadKind::Shield => "shield",
                            SquadKind::Hover => "hover",
                            SquadKind::Jaeger => "jaeger",
                        };
                        let status = match sq.status {
                            'f' => "forming",
                            'r' => "rallying",
                            'a' => "attacking",
                            _ => "?",
                        };
                        let pos = sq.get_pos(&ai.fighters);
                        let _ = write!(
                            s,
                            " #{} {}{} {} x{} at {} -> {}{}",
                            sq.id,
                            if sq.is_air_squad { "air " } else { "" },
                            kind,
                            status,
                            sq.fighters.len(),
                            xz(ai, pos),
                            if sq.target == crate::util::NULLPOS { "-".into() } else { xz(ai, sq.target) },
                            match ai.op.over.squad_intents.get(&sq.id) {
                                Some(i) => format!(" [{}]", i.kind.name()),
                                None if ai.op_squad_target(sq.id).is_some() => " [sent]".to_owned(),
                                None => String::new(),
                            } + &if (sq.courage - 1.0).abs() > 0.01 { format!(" courage {:.1}", sq.courage) } else { String::new() }
                        );
                    }
                    line(&mut out, s);
                }
            }
            "threats" if active => {
                let inc = &ai.incursions;
                let mut s = format!("incursions {}", inc.list.len());
                for i in &inc.list {
                    let _ = write!(s, " [{:?} x{} exposes {} protected {}]", i.tier, i.units, n0(i.exposed), n0(i.protected));
                }
                if !inc.orders.is_empty() {
                    let blocks: Vec<String> =
                        inc.orders.iter().map(|(sid, b)| format!("#{} at {}", sid, xz(ai, b.pos))).collect();
                    let _ = write!(s, " | blocks: {}", blocks.join(", "));
                }
                let defend: Vec<String> = ai
                    .war
                    .defense_targets()
                    .map(|(d, predicted)| {
                        format!("{} dmg {}{}", xz(ai, d.position), n0(d.damage), if predicted { " (predicted)" } else { "" })
                    })
                    .take(4)
                    .collect();
                if !defend.is_empty() {
                    let _ = write!(s, " | defending: {}", defend.join(", "));
                }
                line(&mut out, s);
            }
            "overrides" => {
                let o = &ai.op.over;
                let mut parts = Vec::new();
                let reserved = reserved::reserved(ai.cb);
                if !reserved.is_empty() {
                    parts.push(format!("reserved {} units", reserved.len()));
                }
                if let Some(r) = &o.requisition {
                    parts.push(format!("requisition {r}"));
                }
                if !o.defend.is_empty() {
                    parts.push(format!("{} defense points", o.defend.len()));
                }
                if !o.squad_targets.is_empty() {
                    parts.push(format!("{} squad targets", o.squad_targets.len()));
                }
                if !o.priority_tasks.is_empty() {
                    parts.push(format!("{} priority tasks", o.priority_tasks.len()));
                }
                if !ai.op.over.porc_zones.is_empty() {
                    parts.push(format!("{} porc zones", ai.op.over.porc_zones.len()));
                }
                if (ai.op_greed() - 1.0).abs() > 0.01 {
                    parts.push(format!("greed {:.2}", ai.op_greed()));
                }
                if ai.op.over.line > 0.0 {
                    parts.push(format!("line {:.2}", ai.op.over.line));
                }
                let muted = reserved::muted(ai.cb);
                if !muted.is_empty() {
                    parts.push(format!("muted: {}", muted.join(",")));
                }
                let off = ai.features_off();
                if !off.is_empty() {
                    parts.push(format!("features off: {}", off.join(",")));
                }
                line(&mut out, format!("overrides: {}", if parts.is_empty() { "none".into() } else { parts.join(" | ") }));
            }
            _ => {}
        }
    }
    for pin in &cfg.pins {
        let Some(tool) = super::tools::find(&pin.tool) else { continue };
        if tool.needs_modules && !active {
            continue;
        }
        let result = (tool.run)(ai, &super::tools::Args(&pin.args));
        let mut body = match result {
            Ok(v) => v.to_string(),
            Err(e) => format!("error: {e}"),
        };
        if body.len() > PIN_MAX_CHARS {
            let mut cut = PIN_MAX_CHARS;
            while !body.is_char_boundary(cut) {
                cut -= 1;
            }
            body.truncate(cut);
            body.push_str("...(truncated)");
        }
        let args = if pin.args.as_object().is_some_and(|m| m.is_empty()) { String::new() } else { format!(" {}", pin.args) };
        line(&mut out, format!("{}{}: {}", pin.tool, args, body));
    }
    out
}

/// Frame `f` of a record as `m:ss`.
fn at(v: &Value) -> String {
    game_time(v.get("f").and_then(Value::as_i64).unwrap_or(0) as i32)
}

fn pos(v: &Value) -> String {
    match v.get("pos").and_then(Value::as_array) {
        Some(p) if p.len() == 2 => format!("({},{})", p[0], p[1]),
        _ => String::new(),
    }
}

fn s<'a>(v: &'a Value, key: &str) -> &'a str {
    v.get(key).and_then(Value::as_str).unwrap_or("")
}

fn unit(v: &Value) -> String {
    format!("{}#{}", s(v, "def"), v.get("id").and_then(Value::as_i64).unwrap_or(0))
}

/// One record as a line for the pilot.
pub fn line(r: &Record) -> String {
    let v: Value = serde_json::from_str(&r.line).unwrap_or(Value::Null);
    let t = at(&v);
    match r.kind.as_str() {
        "unit+" => format!("{t} built {} at {}", unit(&v), pos(&v)),
        "unit-" => {
            let by = s(&v, "by");
            format!(
                "{t} LOST {} at {}{}{}",
                unit(&v),
                pos(&v),
                if by.is_empty() { String::new() } else { format!(" to {by}") },
                if v.get("reserved").and_then(Value::as_bool) == Some(true) { " (was reserved)" } else { "" }
            )
        }
        "kill" => {
            let by = s(&v, "by");
            format!("{t} killed {} at {}{}", unit(&v), pos(&v), if by.is_empty() { String::new() } else { format!(" with {by}") })
        }
        "enemy+" => format!("{t} enemy seen {} at {}", unit(&v), pos(&v)),
        "idle" => format!("{t} idle {} at {} (reserved: give it an order or release it)", unit(&v), pos(&v)),
        "given" => format!(
            "{t} {} {} team {} -> {}",
            if v.get("captured").and_then(Value::as_bool) == Some(true) { "captured" } else { "given" },
            unit(&v),
            v.get("from").and_then(Value::as_i64).unwrap_or(-1),
            v.get("to").and_then(Value::as_i64).unwrap_or(-1)
        ),
        "chat" => format!("{t} chat from player {}: {}", v.get("player").and_then(Value::as_i64).unwrap_or(-1), s(&v, "text")),
        "specchat" => String::new(),
        "say" => format!("{t} we said: {}", s(&v, "text")),
        "log" => format!("{t} log {}", s(&v, "msg")),
        "op" => {
            if let Some(ev) = v.get("event").and_then(Value::as_str) {
                format!("{t} {ev} {}", s(&v, "factory").to_owned() + s(&v, "unit"))
            } else {
                format!(
                    "{t} operator {} {}",
                    s(&v, "tool"),
                    if v.get("ok").and_then(Value::as_bool) == Some(true) { "ok" } else { "FAILED" }
                )
            }
        }
        "note" => format!("{t} note: {}", s(&v, "text")),
        "alert" => format!("{t} ALERT {}", s(&v, "text")),
        "run" => format!(
            "{t} run started: {} on {} as team {} (allies {}), seed {}",
            s(&v, "run"),
            s(&v, "map"),
            v.get("team").and_then(Value::as_i64).unwrap_or(-1),
            v.get("allies").map(Value::to_string).unwrap_or_default(),
            v.get("seed").and_then(Value::as_u64).unwrap_or(0)
        ),
        "end" => format!("{t} GAME OVER: state {}, reason {}", s(&v, "state"), v.get("reason").and_then(Value::as_i64).unwrap_or(-1)),
        "map" => format!("{t} map {}", s(&v, "name")),
        kind => {
            let mut body = r.line.clone();
            if body.len() > 200 {
                body.truncate(200);
                body.push_str("...");
            }
            format!("{t} {kind} {body}")
        }
    }
}

/// Counts of records by kind, and of losses and kills by unit def, as a
/// sentence; empty when there were none.
pub fn tally(records: &[Arc<Record>]) -> String {
    let mut kinds: BTreeMap<&str, usize> = BTreeMap::new();
    let mut lost: BTreeMap<String, usize> = BTreeMap::new();
    let mut killed: BTreeMap<String, usize> = BTreeMap::new();
    let mut seen: BTreeMap<String, usize> = BTreeMap::new();
    for r in records {
        if matches!(r.kind.as_str(), "tick" | "snap" | "specchat") {
            continue;
        }
        *kinds.entry(r.kind.as_str()).or_default() += 1;
        if matches!(r.kind.as_str(), "unit-" | "kill" | "enemy+") {
            let v: Value = serde_json::from_str(&r.line).unwrap_or(Value::Null);
            let def = s(&v, "def").to_owned();
            let m = match r.kind.as_str() {
                "unit-" => &mut lost,
                "kill" => &mut killed,
                _ => &mut seen,
            };
            *m.entry(def).or_default() += 1;
        }
    }
    if kinds.is_empty() {
        return String::new();
    }
    let list = |m: &BTreeMap<String, usize>| {
        let mut v: Vec<_> = m.iter().collect();
        v.sort_by_key(|(d, n)| (std::cmp::Reverse(**n), (*d).clone()));
        v.iter().take(6).map(|(d, n)| format!("{n} {d}")).collect::<Vec<_>>().join(", ")
    };
    let mut out = kinds.iter().map(|(k, n)| format!("{n} {k}")).collect::<Vec<_>>().join(", ");
    if !lost.is_empty() {
        let _ = write!(out, "; lost: {}", list(&lost));
    }
    if !killed.is_empty() {
        let _ = write!(out, "; killed: {}", list(&killed));
    }
    if !seen.is_empty() {
        let _ = write!(out, "; new enemies: {}", list(&seen));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::operator::bus::format_line;

    fn rec(seq: u64, frame: i32, kind: &str, data: serde_json::Value) -> Arc<Record> {
        Arc::new(Record { seq, kind: kind.to_owned(), line: format_line(seq, frame, kind, &data) })
    }

    #[test]
    fn lines_read_as_sentences() {
        let r = rec(1, 4500, "unit-", serde_json::json!({"id": 12, "def": "cloakraid", "pos": [100, 200], "by": "tankraid"}));
        assert_eq!(line(&r), "2:30 LOST cloakraid#12 at (100,200) to tankraid");
        let r = rec(2, 60, "chat", serde_json::json!({"player": 1, "text": "glhf"}));
        assert_eq!(line(&r), "0:02 chat from player 1: glhf");
        let r = rec(3, 0, "weird", serde_json::json!({"x": 1}));
        assert!(line(&r).starts_with("0:00 weird {"));
    }

    #[test]
    fn tallies_count_by_kind_and_def() {
        let rs = vec![
            rec(1, 0, "unit-", serde_json::json!({"def": "cloakraid"})),
            rec(2, 0, "unit-", serde_json::json!({"def": "cloakraid"})),
            rec(3, 0, "kill", serde_json::json!({"def": "tankraid"})),
            rec(4, 0, "tick", serde_json::json!({})),
        ];
        assert_eq!(tally(&rs), "1 kill, 2 unit-; lost: 2 cloakraid; killed: 1 tankraid");
        assert_eq!(tally(&[]), "");
    }

    #[test]
    fn defaults_cover_the_sections() {
        let h = HudConfig::default();
        assert_eq!(h.sections.len(), SECTIONS.len() - 1);
        assert!(!h.shows_board());
        let f = FeedConfig::default();
        assert!(f.kinds.contains("end") && !f.kinds.contains("tick"));
    }
}
