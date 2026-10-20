//! The operator API: a surface through which another process (an LLM agent,
//! a dashboard, a script) reads what the AI knows and steers what it does,
//! and the journal that records each run.
//!
//! - [`tools`]: the calls, read-only and acting, run on the AI's thread at
//!   frame boundaries ([`Ai::op_update`]).
//! - [`server`]: HTTP serving MCP (Streamable HTTP), plain JSON-RPC, a
//!   curl-friendly `/call/<tool>`, server-sent events and the dashboard.
//! - [`bus`]: the stream of records (log lines, unit births and deaths,
//!   kills, operator actions, per-second ticks, periodic snapshots).
//! - The journal: the same records as JSON lines in a file per run, headed
//!   by the run's identity and the map, so a run can be read (or replayed in
//!   the dashboard) afterwards.
//!
//! Both are off unless asked for, per instance by AI option or for every
//! instance by environment variable:
//! - `operator` / `ZKGBAI_OPERATOR`: `on` (127.0.0.1:7460, or the next
//!   free port), a port, or `host:port`.
//! - `journal` / `ZKGBAI_JOURNAL`: `on` (`journal/` in the AI's data dir)
//!   or a directory.
//!
//! The operator steers through hooks in the modules: production orders
//! (factory choice), a factory requisition (economy), prioritized
//! construction (worker job costs), defense points (military targeting),
//! squad targets (squad handler), and reserved units, whose orders from the
//! AI the engine wrapper drops ([`springai::reserved`]).

pub mod actions;
pub mod bus;
pub mod server;
pub mod tools;
pub mod views;

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::mpsc::{Receiver, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use springai::{Callback, Event, UnitDef, Vec3, reserved};

use crate::Ai;
use crate::ai::AI_SHORT_NAME;
use crate::economy::TaskId;
use crate::military::{DefenseTarget, SquadId};
use bus::Bus;
use server::{Call, Server, Shared, unix_ms};
use tools::Args;

const DEFAULT_PORT: u16 = 7460;
/// Frames between ticks (resources and values) in the stream.
const TICK: i32 = 30;
/// Frames between snapshots in the stream (and journal).
const SNAPSHOT_PERIOD: i32 = 150;
/// The live snapshot is refreshed while someone asked within this long.
const SNAPSHOT_INTEREST_MS: u64 = 5000;
/// Time per frame the AI spends on operator calls, at most.
const CALL_BUDGET: Duration = Duration::from_millis(8);

/// What the operator has set.
#[derive(Default)]
pub struct Overrides {
    pub fac_orders: Vec<FacOrder>,
    /// A factory to build next, until one stands or is under way.
    pub requisition: Option<String>,
    pub defend: Vec<DefendPoint>,
    /// Squad targets, until a frame.
    pub squad_targets: BTreeMap<SquadId, (Vec3, i32)>,
    /// Construction tasks workers prefer to all others.
    pub priority_tasks: BTreeSet<TaskId>,
}

pub struct FacOrder {
    pub unit: String,
    pub left: u32,
    /// Only this factory.
    pub factory: Option<i32>,
}

pub struct DefendPoint {
    pub pos: Vec3,
    pub weight: f32,
    pub until: i32,
}

pub struct Operator {
    cb: Callback,
    pub bus: Arc<Bus>,
    listen: Option<String>,
    journal_dir: Option<PathBuf>,
    /// Records go to the bus (and journal) only when on.
    on: bool,
    server: Option<Server>,
    shared: Option<Arc<Shared>>,
    calls: Option<Receiver<Call>>,
    journal: RefCell<Option<BufWriter<File>>>,
    pub journal_path: Option<PathBuf>,
    /// Where other processes find this instance's server.
    discovery: Option<PathBuf>,
    pub url: Option<String>,
    pub run_id: String,
    /// The AI's random seed, for the run header.
    pub seed: u64,
    pub over: Overrides,
    /// Calls served, read and acting.
    pub served: (u64, u64),
    /// Enemies reported seen (once each).
    spotted: BTreeSet<i32>,
}

/// A setting from the AI option `key`, else the environment; `None` if
/// unset or off.
fn setting(cb: Callback, key: &str, env: &str) -> Option<String> {
    let raw = cb.option(key).filter(|v| !v.trim().is_empty()).or_else(|| std::env::var(env).ok())?;
    let v = raw.trim().to_owned();
    (!matches!(v.to_ascii_lowercase().as_str(), "" | "0" | "off" | "false" | "no")).then_some(v)
}

fn is_on(v: &str) -> bool {
    matches!(v.to_ascii_lowercase().as_str(), "1" | "on" | "true" | "yes" | "auto")
}

/// UTC `YYYYMMDD-HHMMSS` of a unix time.
fn utc_stamp(secs: u64) -> String {
    let days = (secs / 86400) as i64;
    let rem = secs % 86400;
    // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}{m:02}{d:02}-{:02}{:02}{:02}", rem / 3600, rem / 60 % 60, rem % 60)
}

impl Operator {
    pub fn new(cb: Callback, seed: u64) -> Self {
        let listen = setting(cb, "operator", "ZKGBAI_OPERATOR").map(|v| {
            if is_on(&v) {
                format!("127.0.0.1:{DEFAULT_PORT}")
            } else if v.chars().all(|c| c.is_ascii_digit()) {
                format!("127.0.0.1:{v}")
            } else {
                v
            }
        });
        let journal_dir = setting(cb, "journal", "ZKGBAI_JOURNAL").map(|v| {
            if is_on(&v) {
                cb.writeable_dir().map(PathBuf::from).unwrap_or_else(std::env::temp_dir).join("journal")
            } else {
                PathBuf::from(v)
            }
        });
        Operator {
            cb,
            bus: Arc::new(Bus::default()),
            listen,
            journal_dir,
            on: false,
            server: None,
            shared: None,
            calls: None,
            journal: RefCell::new(None),
            journal_path: None,
            discovery: None,
            url: None,
            run_id: String::new(),
            seed,
            over: Overrides::default(),
            served: (0, 0),
            spotted: BTreeSet::new(),
        }
    }

    pub fn is_on(&self) -> bool {
        self.on
    }

    /// Append a record to the stream (and the journal).
    pub fn emit(&self, frame: i32, kind: &str, data: Value) {
        if !self.on {
            return;
        }
        let record = self.bus.push(frame, kind, &data);
        if let Some(j) = self.journal.borrow_mut().as_mut() {
            let _ = writeln!(j, "{}", record.line);
        }
    }

    fn flush(&self) {
        if let Some(j) = self.journal.borrow_mut().as_mut() {
            let _ = j.flush();
        }
    }

    fn stop(&mut self) {
        self.flush();
        *self.journal.borrow_mut() = None;
        if let Some(mut s) = self.server.take() {
            s.stop();
        }
        self.shared = None;
        self.calls = None;
        if let Some(p) = self.discovery.take() {
            let _ = std::fs::remove_file(p);
        }
        reserved::clear(self.cb);
        self.on = false;
    }
}

impl Drop for Operator {
    fn drop(&mut self) {
        self.stop();
    }
}

impl Ai {
    /// Append a record to the operator stream, stamped with this frame.
    pub fn op_emit(&self, kind: &str, data: Value) {
        self.op.emit(self.frame, kind, data);
    }

    /// Open the journal and start the server, as configured (on init).
    pub(crate) fn op_start(&mut self) {
        let listen = self.op.listen.clone();
        let journal_dir = self.op.journal_dir.clone();
        if listen.is_none() && journal_dir.is_none() {
            return;
        }
        let started = unix_ms() / 1000;
        let map = self.cb.map().name();
        let safe_map: String = map.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '_' }).take(40).collect();
        self.op.run_id = format!("{}-{}-t{}", utc_stamp(started), safe_map.trim_matches('_'), self.team_id);
        self.op.on = true;

        if let Some(dir) = journal_dir {
            let path = dir.join(format!("{}.jsonl", self.op.run_id));
            match std::fs::create_dir_all(&dir).and_then(|_| File::create(&path)) {
                Ok(f) => {
                    *self.op.journal.borrow_mut() = Some(BufWriter::with_capacity(1 << 16, f));
                    self.log(&format!("operator: journal at {}", path.display()));
                    self.op.journal_path = Some(path);
                }
                Err(e) => self.log(&format!("operator: no journal in {}: {e}", dir.display())),
            }
        }

        if let Some(spec) = listen {
            let (tx, rx) = channel();
            let shared = Arc::new(Shared {
                bus: self.op.bus.clone(),
                calls: tx,
                tools: tools::TOOLS.iter().map(|t| t.listing()).collect(),
                name: AI_SHORT_NAME.to_ascii_lowercase(),
                version: format!("{} ({})", env!("CARGO_PKG_VERSION"), env!("ZKGBAI_BUILD")),
                instructions: format!(
                    "ZKGBAI, a Zero-K skirmish AI, playing team {} on {}. Read the game through it (status first, \
                     then economy, military, squads, units, enemies, threat maps) and steer it: production \
                     orders, factory requisitions, buildings, defense points, squad targets, or take units and \
                     order them directly. Positions are map elmos (x east, z south); 30 frames are one second. \
                     Calls are answered while the game runs.",
                    self.team_id, map
                ),
                snapshot: Mutex::new(None),
                snapshot_wanted: Default::default(),
                frame: Default::default(),
                journal: self.op.journal_path.clone(),
                shutdown: Default::default(),
            });
            match Server::start(&spec, shared.clone()) {
                Ok(server) => {
                    let url = format!("http://{}", server.addr);
                    self.log(&format!("operator: listening on {url} (dashboard {url}/, MCP {url}/mcp)"));
                    self.op.discovery = write_discovery(&url, self.team_id, &map, self.op.journal_path.as_ref());
                    self.op.url = Some(url);
                    self.op.server = Some(server);
                    self.op.shared = Some(shared);
                    self.op.calls = Some(rx);
                }
                Err(e) => self.log(&format!("operator: cannot listen on {spec}: {e}")),
            }
        }

        let header = self.op_run_header(started);
        self.op_emit("run", header);
        let map_info = views::map_info_value(self, true);
        self.op_emit("map", map_info);
        self.op.flush();
    }

    fn op_run_header(&self, started: u64) -> Value {
        let game = self.cb.game();
        let teams: Vec<Value> = (0..game.teams())
            .map(|t| json!({"team": t, "ally_team": game.team_ally_team(t), "ai": game.team_has_ai_controller(t)}))
            .collect();
        let options: serde_json::Map<String, Value> = ["disable", "test", "factory_pick", "operator", "journal"]
            .iter()
            .filter_map(|k| self.cb.option(k).map(|v| ((*k).to_owned(), json!(v))))
            .collect();
        let env: serde_json::Map<String, Value> =
            std::env::vars().filter(|(k, _)| k.starts_with("ZKGBAI_")).map(|(k, v)| (k, json!(v))).collect();
        let m = self.cb.mod_info();
        json!({
            "run": self.op.run_id,
            "ai": AI_SHORT_NAME,
            "version": env!("CARGO_PKG_VERSION"),
            "build": env!("ZKGBAI_BUILD"),
            "started": utc_stamp(started),
            "unix": started,
            "team": self.team_id,
            "ally_team": self.ally_team_id,
            "allies": self.allies,
            "enemy_teams": self.get_enemy_team_ids(),
            "ffa": self.is_ffa,
            "teams": teams,
            "map": self.cb.map().name(),
            "game": format!("{} {}", m.short_name(), m.version()),
            "engine": self.cb.engine_version(),
            "options": options,
            "env": env,
            "seed": self.op.seed,
            "state": self.state(),
            "url": self.op.url,
        })
    }

    /// Serve operator calls and keep the stream, snapshots and overrides
    /// up to date (every frame, before the modules).
    pub(crate) fn op_update(&mut self, frame: i32) {
        if !self.op.on {
            return;
        }
        if let Some(s) = &self.op.shared {
            s.frame.store(frame, Ordering::Relaxed);
        }
        let start = Instant::now();
        while start.elapsed() < CALL_BUDGET {
            let Some(call) = self.op.calls.as_ref().and_then(|rx| rx.try_recv().ok()) else { break };
            self.op_serve(call);
        }

        let modules = self.active();
        if modules && frame % TICK == 0 {
            let tick = views::tick_value(self);
            self.op_emit("tick", tick);
        }
        let wanted = self.op.shared.as_ref().is_some_and(|s| {
            unix_ms().saturating_sub(s.snapshot_wanted.load(Ordering::Relaxed)) < SNAPSHOT_INTEREST_MS
        });
        let live = wanted && frame % TICK == 0;
        let periodic = frame % SNAPSHOT_PERIOD == 0;
        if modules && (live || periodic) {
            let snap = views::snapshot_value(self);
            if let Some(s) = &self.op.shared {
                *s.snapshot.lock().unwrap_or_else(|e| e.into_inner()) = Some(snap.to_string());
            }
            if periodic {
                self.op_emit("snap", snap);
            }
        }
        if modules && frame % 60 == 0 {
            self.op_keep_overrides(frame);
        }
        if frame % TICK == 0 {
            self.op.flush();
        }
    }

    fn op_serve(&mut self, call: Call) {
        let Some(tool) = tools::find(&call.tool) else {
            let _ = call.reply.send(Err(format!("unknown tool '{}'", call.tool)));
            return;
        };
        let result = if tool.needs_modules && !self.active() {
            Err(format!("the AI's modules are not running (state: {})", self.state()))
        } else {
            let mut out = None;
            let args = call.args;
            self.guard("operator.call", |ai| out = Some((tool.run)(ai, &Args(&args))));
            let result = out.unwrap_or_else(|| Err("the tool panicked (see the infolog)".into()));
            if tool.writes {
                let mut rec = json!({"tool": tool.name, "args": args, "ok": result.is_ok()});
                match &result {
                    Ok(v) => rec["result"] = v.clone(),
                    Err(e) => rec["error"] = json!(e),
                }
                self.op_emit("op", rec);
            }
            result
        };
        if tool.writes {
            self.op.served.1 += 1;
        } else {
            self.op.served.0 += 1;
        }
        let _ = call.reply.send(result);
    }

    /// Refresh defense points; drop overrides that ran out or lost their
    /// subject.
    fn op_keep_overrides(&mut self, frame: i32) {
        let over = &mut self.op.over;
        over.defend.retain(|d| d.until > frame);
        for d in &over.defend {
            self.war.refresh_defense_target(DefenseTarget::new(d.pos, d.weight, frame));
        }
        over.squad_targets.retain(|sid, (_, until)| *until > frame && self.squads.contains(*sid));
        over.priority_tasks.retain(|&id| self.eco.tasks.get(id).is_some());
        over.fac_orders.retain(|o| o.factory.is_none_or(|f| self.fac.factories.contains_key(&f)));
    }

    /// Record what an engine event tells about the game, before the modules
    /// see it; `false` keeps it from them (reserved units going idle).
    pub(crate) fn op_observe(&mut self, event: &Event) -> bool {
        if !self.op.on {
            return true;
        }
        let pos = |p: Vec3| json!([p.x.round() as i32, p.z.round() as i32]);
        match event {
            Event::UnitFinished { unit } => {
                self.op_emit(
                    "unit+",
                    json!({"id": unit.id(), "def": self.defs.name_of(*unit), "pos": pos(unit.pos())}),
                );
            }
            Event::UnitDestroyed { unit, attacker } => {
                let by = attacker.map(|a| self.defs.name_of(a).to_owned());
                let reserved = reserved::release(self.cb, unit.id());
                self.op_emit(
                    "unit-",
                    json!({"id": unit.id(), "def": self.defs.name_of(*unit), "pos": pos(unit.pos()), "by": by,
                           "by_id": attacker.map(|a| a.id()), "reserved": reserved}),
                );
            }
            Event::UnitGiven { unit, old_team, new_team } | Event::UnitCaptured { unit, old_team, new_team } => {
                if *old_team == self.team_id {
                    reserved::release(self.cb, unit.id());
                }
                self.op_emit(
                    "given",
                    json!({"id": unit.id(), "def": self.defs.name_of(*unit), "from": old_team, "to": new_team,
                           "captured": matches!(event, Event::UnitCaptured { .. })}),
                );
            }
            Event::EnemyEnterLos { enemy } => {
                if self.op.spotted.insert(enemy.id()) {
                    self.op_emit(
                        "enemy+",
                        json!({"id": enemy.id(), "def": self.defs.name_of(*enemy), "pos": pos(enemy.pos())}),
                    );
                }
            }
            Event::EnemyDestroyed { enemy, attacker } => {
                let def = self.war.targets.get(&enemy.id()).map(|e| e.def_name.clone()).unwrap_or_default();
                let at = self.war.targets.get(&enemy.id()).map(|e| e.position).unwrap_or(enemy.pos());
                self.op_emit(
                    "kill",
                    json!({"id": enemy.id(), "def": def, "pos": pos(at),
                           "by": attacker.map(|a| self.defs.name_of(a).to_owned()), "by_id": attacker.map(|a| a.id())}),
                );
            }
            Event::Message { player, text } => {
                self.op_emit("chat", json!({"player": player, "text": text}));
            }
            Event::UnitIdle { unit } if reserved::is_reserved(self.cb, unit.id()) => {
                self.op_emit("idle", json!({"id": unit.id(), "def": self.defs.name_of(*unit), "pos": pos(unit.pos())}));
                return false;
            }
            _ => {}
        }
        true
    }

    /// Close the run: the closing record, then the journal and server.
    pub(crate) fn op_finish(&mut self, reason: i32) {
        if !self.op.on {
            return;
        }
        let profile = views::profile_value(self, 8);
        self.op_emit(
            "end",
            json!({"reason": reason, "time": views::game_time(self.frame), "state": self.state(),
                   "served": {"read": self.op.served.0, "acting": self.op.served.1}, "profile": profile}),
        );
        self.op.stop();
    }

    // ---- hooks in the modules ----

    /// A unit the operator ordered from factory `fac_id`, if one is due.
    pub(crate) fn op_factory_order(&mut self, fac_id: i32) -> Option<UnitDef> {
        if self.op.over.fac_orders.is_empty() {
            return None;
        }
        let fac = self.fac.factories.get(&fac_id)?.unit;
        let options: Vec<String> = fac.def()?.build_options().iter().map(|d| self.defs.name(*d).to_owned()).collect();
        let orders = &mut self.op.over.fac_orders;
        let i = orders.iter().position(|o| o.factory.is_none_or(|f| f == fac_id) && options.contains(&o.unit))?;
        orders[i].left = orders[i].left.saturating_sub(1);
        let unit = orders[i].unit.clone();
        if orders[i].left == 0 {
            orders.remove(i);
        }
        self.op_emit("op", json!({"event": "fac_order", "factory": fac_id, "unit": unit}));
        self.defs.get(&unit)
    }

    /// The factory the operator asked for, until it stands or is under way.
    pub(crate) fn op_requisition(&mut self) -> Option<(String, String)> {
        let name = self.op.over.requisition.clone()?;
        let standing = self.fac.factories.values().any(|f| self.defs.name_of(f.unit) == name);
        let planned = self
            .eco
            .factory_tasks
            .iter()
            .filter_map(|&id| self.eco.tasks.get(id))
            .any(|t| t.as_construction().is_some_and(|c| self.defs.name(c.build_type) == name));
        if standing || planned {
            self.op.over.requisition = None;
            self.op_emit("op", json!({"event": "requisition_met", "factory": name}));
            return None;
        }
        Some((name, "the operator asked for it".to_owned()))
    }

    /// Where the operator sent squad `sid`, while that holds.
    pub(crate) fn op_squad_target(&self, sid: SquadId) -> Option<Vec3> {
        self.op.over.squad_targets.get(&sid).filter(|(_, until)| *until > self.frame).map(|(p, _)| *p)
    }

    /// Whether the operator prioritized construction task `id`.
    pub(crate) fn op_priority(&self, id: TaskId) -> bool {
        id != 0 && self.op.over.priority_tasks.contains(&id)
    }
}

/// Leave a note where other processes look for running instances
/// (`<temp>/zkgbai-operator/<pid>-t<team>.json`).
fn write_discovery(url: &str, team: i32, map: &str, journal: Option<&PathBuf>) -> Option<PathBuf> {
    let dir = std::env::temp_dir().join("zkgbai-operator");
    std::fs::create_dir_all(&dir).ok()?;
    let path = dir.join(format!("{}-t{team}.json", std::process::id()));
    let note = json!({"url": url, "team": team, "map": map, "pid": std::process::id(),
                      "journal": journal.map(|p| p.to_string_lossy()), "started": unix_ms() / 1000});
    std::fs::write(&path, note.to_string()).ok()?;
    Some(path)
}

#[cfg(test)]
mod tests {
    use super::utc_stamp;

    #[test]
    fn stamps_are_utc_dates() {
        assert_eq!(utc_stamp(0), "19700101-000000");
        assert_eq!(utc_stamp(1_792_500_000), "20261020-124000");
        assert_eq!(utc_stamp(951_782_400), "20000229-000000");
    }
}
