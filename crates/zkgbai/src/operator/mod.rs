//! The operator API: a surface through which another process (an LLM agent,
//! a dashboard, a script) reads what the AI knows and steers what it does,
//! and the journal that records each run.
//!
//! - [`tools`]: the calls, read-only and acting, run on the AI's thread at
//!   frame boundaries ([`Ai::op_update`]).
//! - [`server`]: HTTP serving MCP (Streamable HTTP), plain JSON-RPC, a
//!   curl-friendly `/call/<tool>`, server-sent events and the dashboard.
//! - [`mcpl`]: MCPL over a WebSocket ([`ws`]) for agent hosts: the feed
//!   pushed, the HUD injected before each inference ([`hud`]), the agent's
//!   turns journaled.
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
//! squad targets (squad handler), reserved units, whose orders from the
//! AI the engine wrapper drops ([`springai::reserved`]), and muted
//! modules, whose orders it drops the same way.

pub mod actions;
pub mod board;
pub mod bus;
pub mod hud;
pub mod levers;
pub mod mcpl;
pub mod server;
pub mod tools;
pub mod views;
pub mod ws;

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
use springai::{Callback, Event, Unit, UnitDef, Vec3, reserved};

use crate::Ai;
use crate::ai::AI_SHORT_NAME;
use crate::economy::TaskId;
use crate::military::{DefenseTarget, SquadId};
use crate::util::{NULLPOS, distance};
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
    /// Standing intentions for squads, resolved to a target every two
    /// seconds (`op_keep_overrides`); a squad target above wins while set.
    pub squad_intents: BTreeMap<SquadId, Intent>,
    /// Construction tasks workers prefer to all others.
    pub priority_tasks: BTreeSet<TaskId>,
    /// The economy's appetite for risk: the threat a job and a worker
    /// perceive is divided by this (1 = the AI's own; 0 means default).
    pub greed: f32,
    /// Where turrets by mexes and links are allowed while `mexporc` is off.
    pub porc_zones: Vec<PorcZone>,
    /// The army's line: where squads rally when nothing calls them, as a
    /// fraction of the way from our base to the enemy's center (0 = the
    /// AI's own choice, 0.2 home, 0.5 the middle).
    pub line: f32,
    /// Mobile build power the factories keep, as a multiple of metal
    /// income (0 = the AI's own rule, one to one).
    pub buildpower: f32,
    /// The fixed-defense budget's levers (see `posture`; `None`: the AI's
    /// own).
    pub defense_floor: Option<f32>,
    pub defense_lane: Option<f32>,
    pub defense_density: Option<f32>,
    pub defense_cap: Option<f32>,
    pub density_ref: Option<f32>,
}

pub struct PorcZone {
    pub pos: Vec3,
    pub radius: f32,
    pub until: i32,
}

/// What the operator wants a squad to keep doing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IntentKind {
    /// Stand at a place (fight what comes, stay).
    Hold,
    /// Hunt the enemy's economy around a place: the nearest known enemy
    /// that is not a turret within the radius, else the place itself.
    Raid,
    /// Go where a unit of ours goes.
    Escort,
    /// Go to a place; once there, the AI has the squad back.
    Attack,
}

impl IntentKind {
    pub fn name(self) -> &'static str {
        match self {
            IntentKind::Hold => "hold",
            IntentKind::Raid => "raid",
            IntentKind::Escort => "escort",
            IntentKind::Attack => "attack",
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Intent {
    pub kind: IntentKind,
    pub pos: Vec3,
    pub radius: f32,
    /// The unit escorted.
    pub unit: Option<i32>,
    pub until: i32,
    /// Where the squad is sent right now.
    pub resolved: Vec3,
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
    /// When each alert key last fired (rate limiting).
    alerts: BTreeMap<String, i32>,
    /// The operator profile (AI option `profile` / ZKGBAI_PROFILE): what an
    /// operator may do. `posture`: no hands on units or construction
    /// (`take`, `release`, `order`, `build`, `issue` are not offered),
    /// production orders only from 2:00 and at most two deep, a factory
    /// requisition only once income is past 30.
    pub profile: Option<String>,
    /// What the HUD shows and the feed pushes (shared with the server).
    pub hud: Arc<Mutex<hud::HudConfig>>,
    pub feed: Arc<Mutex<hud::FeedConfig>>,
    /// The board's terrain layer, sampled on first use.
    terrain: RefCell<Option<board::Terrain>>,
    /// The terrain analysis has been written to the journal (once).
    terrain_journaled: bool,
}

/// A setting from the AI option `key`, else the environment; `None` if
/// unset or off.
/// The posture's levers as start options (and environment variables), with
/// where each goes.
type Slot = fn(&mut Overrides) -> &mut Option<f32>;
pub(crate) const POSTURE_SETTINGS: [(&str, &str, Slot); 5] = [
    ("defense_floor", "ZKGBAI_DEFENSE_FLOOR", |o| &mut o.defense_floor),
    ("defense_lane", "ZKGBAI_DEFENSE_LANE", |o| &mut o.defense_lane),
    ("defense_density", "ZKGBAI_DEFENSE_DENSITY", |o| &mut o.defense_density),
    ("defense_cap", "ZKGBAI_DEFENSE_CAP", |o| &mut o.defense_cap),
    ("density_ref", "ZKGBAI_DENSITY_REF", |o| &mut o.density_ref),
];

/// A posture lever's value within its range.
pub(crate) fn posture_clamp(key: &str, v: f32) -> f32 {
    match key {
        "density_ref" => v.clamp(100.0, 10000.0),
        "defense_cap" => v.clamp(0.0, 0.9),
        _ => v.clamp(0.0, 0.9),
    }
}

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
            alerts: BTreeMap::new(),
            profile: setting(cb, "profile", "ZKGBAI_PROFILE").map(|v| v.to_ascii_lowercase()),
            hud: Arc::new(Mutex::new(hud::HudConfig::default())),
            feed: Arc::new(Mutex::new(hud::FeedConfig::default())),
            terrain: RefCell::new(None),
            terrain_journaled: false,
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
        // Drop the call queue before joining the server's threads: that drops
        // the reply channels of calls still queued, so their callers return at
        // once instead of waiting out CALL_TIMEOUT (15 s) on the engine's
        // thread at game end.
        self.calls = None;
        if let Some(mut s) = self.server.take() {
            s.stop();
        }
        self.shared = None;
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
        if let Some(g) = setting(self.cb, "greed", "ZKGBAI_GREED").and_then(|v| v.parse::<f32>().ok()) {
            self.op.over.greed = g.clamp(0.25, 4.0);
            self.log(&format!("operator: greed {:.2}", self.op.over.greed));
        }
        if let Some(l) = setting(self.cb, "line", "ZKGBAI_LINE").and_then(|v| v.parse::<f32>().ok()) {
            self.op.over.line = l.clamp(0.0, 0.9);
            self.log(&format!("operator: line {:.2}", self.op.over.line));
        }
        if let Some(b) = setting(self.cb, "buildpower", "ZKGBAI_BUILDPOWER").and_then(|v| v.parse::<f32>().ok()) {
            self.op.over.buildpower = b.clamp(0.5, 3.0);
            self.log(&format!("operator: buildpower {:.2}", self.op.over.buildpower));
        }
        for (key, env, slot) in POSTURE_SETTINGS {
            if let Some(v) = setting(self.cb, key, env).and_then(|v| v.parse::<f32>().ok()) {
                *slot(&mut self.op.over) = Some(posture_clamp(key, v));
                self.log(&format!("operator: {key} {v:.2}"));
            }
        }
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
                tools: tools::TOOLS.iter().filter(|t| self.op_tool_offered(t.name)).map(|t| t.listing()).collect(),
                name: AI_SHORT_NAME.to_ascii_lowercase(),
                version: format!("{} ({})", env!("CARGO_PKG_VERSION"), env!("ZKGBAI_BUILD")),
                instructions: format!(
                    "ZKGBAI, a Zero-K skirmish AI, playing team {} on {}. Read the game through it (status first, \
                     then economy, military, squads, units, enemies, threat maps) and steer it: production \
                     orders, factory requisitions, buildings, defense points, squad targets, muted modules, or take units and \
                     order them directly. Positions are map elmos (x east, z south); 30 frames are one second. \
                     Calls are answered while the game runs.",
                    self.team_id, map
                ),
                snapshot: Mutex::new(None),
                snapshot_wanted: Default::default(),
                frame: Default::default(),
                team: self.team_id,
                ally_team: self.ally_team_id,
                journal: self.op.journal_path.clone(),
                shutdown: Default::default(),
                run_id: self.op.run_id.clone(),
                hud: self.op.hud.clone(),
                feed: self.op.feed.clone(),
            });
            match Server::start(&spec, shared.clone()) {
                Ok(server) => {
                    let url = format!("http://{}", server.addr);
                    self.log(&format!("operator: listening on {url} (dashboard {url}/, MCP {url}/mcp, MCPL ws://{}/mcpl)", server.addr));
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
            self.op_economy_alerts(frame);
            // The strategic analysis, once it is there (chokepoints to
            // hold, sheltered regions): what a porc or line policy is fit on.
            if !self.op.terrain_journaled && self.strategy.is_some() {
                self.op.terrain_journaled = true;
                let mut v = None;
                self.guard("operator.terrain", |ai| v = views::terrain(ai, &Args(&Value::Null)).ok());
                if let Some(v) = v {
                    self.op_emit("terrain", v);
                }
            }
        }
        if frame % TICK == 0 {
            self.op.flush();
        }
    }

    fn op_serve(&mut self, call: Call) {
        match call.tool.as_str() {
            // Internal calls from the server's threads (not tools).
            "hud_render" => {
                let cfg = self.op.hud.lock().unwrap_or_else(|e| e.into_inner()).clone();
                let mut text = None;
                self.guard("operator.hud", |ai| text = Some(hud::render(ai, &cfg)));
                let mut v = json!({"text": text.unwrap_or_default()});
                if cfg.shows_board() && self.active() {
                    v["image"] = json!(self.board_png_base64());
                }
                let _ = call.reply.send(Ok(v));
                return;
            }
            "board_png" => {
                let v = if self.active() { json!({"image": self.board_png_base64()}) } else { json!({"image": ""}) };
                let _ = call.reply.send(Ok(v));
                return;
            }
            "inference" => {
                self.op_emit("inference", call.args);
                let _ = call.reply.send(Ok(json!({})));
                return;
            }
            _ => {}
        }
        let Some(tool) = tools::find(&call.tool).filter(|t| self.op_tool_offered(t.name)) else {
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

    /// The board as a PNG, base64 (empty if drawing failed).
    pub(crate) fn board_png_base64(&mut self) -> String {
        if self.op.terrain.borrow().is_none() {
            let t = board::terrain(self);
            *self.op.terrain.borrow_mut() = Some(t);
        }
        let mut png = Vec::new();
        self.guard("operator.board", |ai| {
            if let Some(t) = ai.op.terrain.borrow().as_ref() {
                png = board::render(ai, t);
            }
        });
        ws::base64(&png)
    }

    /// Whether the operator profile offers tool `name`.
    pub(crate) fn op_tool_offered(&self, name: &str) -> bool {
        match self.op.profile.as_deref() {
            Some("posture") => !matches!(name, "take" | "release" | "order" | "build" | "issue"),
            _ => true,
        }
    }

    /// Whether the profile is `posture`.
    pub(crate) fn op_posture(&self) -> bool {
        self.op.profile.as_deref() == Some("posture")
    }

    /// An `alert` record (pushed at once to an MCPL host, and meant to wake
    /// its agent), at most once per `cooldown` frames per key.
    pub(crate) fn op_alert(&mut self, key: &str, cooldown: i32, text: String) {
        if !self.op.on {
            return;
        }
        let frame = self.frame;
        if self.op.alerts.get(key).is_some_and(|&last| frame - last < cooldown) {
            return;
        }
        self.op.alerts.insert(key.to_owned(), frame);
        self.op_emit("alert", json!({"key": key, "text": text}));
    }

    /// The economy's two chronic failures under a pilot, as alerts: an
    /// energy stall that lasts, and metal floating unspent.
    fn op_economy_alerts(&mut self, frame: i32) {
        if frame < 1800 {
            return;
        }
        let e = self.cb.economy();
        let (metal, energy) = (e.current(self.metal), e.current(self.energy));
        let (m_income, m_usage) = (e.income(self.metal), e.usage(self.metal));
        if self.eco.estalled && energy < 50.0 {
            let n = self.op.alerts.entry("estall-count".into()).or_insert(0);
            *n += 1;
            if *n >= 10 {
                self.op.alerts.insert("estall-count".into(), 0);
                self.op_alert(
                    "estall",
                    1800,
                    format!("ENERGY STALL for 20 s: energy {energy:.0}, every builder throttled. Solars (or wind) before anything else."),
                );
            }
        } else {
            self.op.alerts.insert("estall-count".into(), 0);
        }
        if metal > 400.0 && m_income > m_usage + 2.0 {
            let n = self.op.alerts.entry("float-count".into()).or_insert(0);
            *n += 1;
            if *n >= 15 {
                self.op.alerts.insert("float-count".into(), 0);
                self.op_alert(
                    "float",
                    2700,
                    format!("METAL FLOATING: {metal:.0} stored, income {m_income:.1} vs spending {m_usage:.1} for 30 s. More builders or a second factory, or release reserved workers."),
                );
            }
        } else {
            self.op.alerts.insert("float-count".into(), 0);
        }
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
        over.squad_intents.retain(|sid, i| i.until > frame && self.squads.contains(*sid));
        over.porc_zones.retain(|z| z.until > frame);
        over.priority_tasks.retain(|&id| self.eco.tasks.get(id).is_some());
        over.fac_orders.retain(|o| o.factory.is_none_or(|f| self.fac.factories.contains_key(&f)));
        self.op_resolve_intents(frame);
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
                // Spectator chat would be journaled for analysts and shown
                // to no operator surface, as a human player's client hides
                // it. But the engine hands AIs every message with its
                // destination stripped (`CGame::HandleChatMsg` →
                // `eoh->SendChatMessage`), so spectator lines cannot be told
                // apart here; the fix is the engine's. Our own lines are
                // dropped from the feed by the MCPL layer (`own_says`).
                let kind = if text.starts_with("s:") { "specchat" } else { "chat" };
                self.op_emit(kind, json!({"player": player, "text": text}));
            }
            Event::UnitDamaged { unit, attacker, .. } if reserved::is_reserved(self.cb, unit.id()) => {
                let (id, def, p, hp) = (unit.id(), self.defs.name_of(*unit).to_owned(), unit.pos(), unit.health() / unit.max_health().max(1.0));
                let by = attacker.map(|a| self.defs.name_of(a).to_owned()).unwrap_or_else(|| "unseen".into());
                self.op_alert(
                    &format!("hit-{id}"),
                    600,
                    format!("reserved {def}#{id} UNDER FIRE at ({},{}) by {by}, {:.0}% health: it is yours, nobody else moves it", p.x.round(), p.z.round(), hp * 100.0),
                );
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
                   "served": {"read": self.op.served.0, "acting": self.op.served.1}, "profile": profile,
                   "roles": self.roles_summary()}),
        );
        self.op.stop();
    }

    // ---- hooks in the modules ----

    /// A unit the operator ordered from factory `fac_id`, if one is due.
    /// With `workers_first`, the factory wants a constructor by the AI's own
    /// rule, and only an order for a constructor is served now: production
    /// orders never starve the economy of builders.
    pub(crate) fn op_factory_order(&mut self, fac_id: i32, workers_first: bool) -> Option<UnitDef> {
        if self.op.over.fac_orders.is_empty() {
            return None;
        }
        let fac = self.fac.factories.get(&fac_id)?.unit;
        let options: Vec<String> = fac.def()?.build_options().iter().map(|d| self.defs.name(*d).to_owned()).collect();
        let builder = |name: &str| self.defs.get(name).is_some_and(|d| d.speed() > 0.0 && !d.build_options().is_empty());
        let orders = &mut self.op.over.fac_orders;
        let i = orders
            .iter()
            .position(|o| o.factory.is_none_or(|f| f == fac_id) && options.contains(&o.unit) && (!workers_first || builder(&o.unit)))?;
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

    /// Where the operator sent squad `sid`, while that holds: an explicit
    /// target first, else its standing intent.
    pub(crate) fn op_squad_target(&self, sid: SquadId) -> Option<Vec3> {
        if let Some(p) = self.op.over.squad_targets.get(&sid).filter(|(_, until)| *until > self.frame).map(|(p, _)| *p) {
            return Some(p);
        }
        self.op.over.squad_intents.get(&sid).filter(|i| i.resolved != NULLPOS).map(|i| i.resolved)
    }

    /// Turn each squad's intent into where it goes now.
    fn op_resolve_intents(&mut self, frame: i32) {
        let ids: Vec<SquadId> = self.op.over.squad_intents.keys().copied().collect();
        for sid in ids {
            let Some(mut intent) = self.op.over.squad_intents.get(&sid).copied() else { continue };
            let squad_pos = self.squads.get(sid).map(|s| s.get_pos(&self.fighters)).unwrap_or(NULLPOS);
            let mut done = false;
            intent.resolved = match intent.kind {
                IntentKind::Hold => intent.pos,
                IntentKind::Attack => {
                    if squad_pos != NULLPOS && distance(squad_pos, intent.pos) < intent.radius {
                        done = true;
                    }
                    intent.pos
                }
                IntentKind::Escort => match intent.unit.and_then(|id| Unit::from_id(self.cb, id)) {
                    Some(u) if u.max_health() > 0.0 && u.health() > 0.0 && u.team() == self.team_id => u.pos(),
                    _ => {
                        done = true;
                        NULLPOS
                    }
                },
                IntentKind::Raid => {
                    // The nearest known enemy that is not a defense, by
                    // distance to the place (mexes and workers first).
                    let mut best: Option<(f32, Vec3)> = None;
                    for e in self.war.targets.values() {
                        if e.is_static && e.power > 0.0 {
                            continue;
                        }
                        let d = distance(e.position, intent.pos);
                        if d > intent.radius {
                            continue;
                        }
                        let score = if e.is_worker || e.ud.is_some_and(|d| d.speed() <= 0.0) { d } else { d * 2.0 };
                        if best.is_none_or(|(b, _)| score < b) {
                            best = Some((score, e.position));
                        }
                    }
                    best.map_or(intent.pos, |(_, p)| p)
                }
            };
            if done {
                self.op.over.squad_intents.remove(&sid);
                self.op_emit("op", json!({"event": "intent_done", "squad": sid, "intent": intent.kind.name(), "frame": frame}));
            } else {
                self.op.over.squad_intents.insert(sid, intent);
            }
        }
    }

    /// Whether a turret by a mex or link may go at `pos`: always with the
    /// `mexporc` feature on, else only inside an operator's porc zone.
    pub(crate) fn op_porc_allowed(&self, pos: Vec3) -> bool {
        !self.dev_off("mexporc") || self.op.over.porc_zones.iter().any(|z| z.until > self.frame && distance(z.pos, pos) <= z.radius)
    }

    /// The army's line, if the operator set one: the point that far along
    /// the way from our base to the enemy's center.
    pub(crate) fn op_line_point(&self) -> Option<Vec3> {
        let line = self.op.over.line;
        if line <= 0.0 {
            return None;
        }
        let (base, enemy) = (self.graph.get_ally_base(), self.graph.get_enemy_center());
        if base == NULLPOS || enemy == NULLPOS {
            return None;
        }
        let p = crate::util::get_directional_point(base, enemy, distance(base, enemy) * line);
        Some(Vec3::new(p.x, self.cb.map().elevation_at(p.x, p.z), p.z))
    }

    /// The economy's greed: perceived threat is divided by it. Greed never
    /// widens the horizon while energy is stalled (the far mexes would
    /// starve what is already there).
    pub(crate) fn op_greed(&self) -> f32 {
        let g = if self.op.over.greed > 0.0 { self.op.over.greed } else { 1.0 };
        if self.eco.estalled && !self.dev_off("greedcap") { g.min(1.0) } else { g }
    }

    /// The constructor rule's target: mobile build power as a multiple of
    /// metal income.
    pub(crate) fn op_buildpower(&self) -> f32 {
        if self.op.over.buildpower > 0.0 { self.op.over.buildpower } else { 1.0 }
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
