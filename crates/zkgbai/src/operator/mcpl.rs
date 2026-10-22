//! MCPL (MCP Live) over a WebSocket: the protocol an agent host speaks to a
//! server that is more than a tool box. On top of MCP's `tools/*`:
//!
//! - **push events** (server → host): the operator feed, batched into a few
//!   lines per push, and a situation report on a timer (see
//!   [`super::hud`]). The host decides what wakes its agent.
//! - **context hooks** (host → server): before each of the agent's
//!   inferences the host asks for context, and gets the HUD.
//! - **inference lifecycle** (host → server): the agent's turns go into
//!   the journal, so a run can be read against what the pilot was doing.
//! - **feature sets**: `zkgbai.read`, `zkgbai.act`, `zkgbai.hud`,
//!   `zkgbai.events`; the host enables them after the handshake.
//!
//! Messages are newline-delimited JSON-RPC 2.0, one per text frame. One
//! thread reads the socket and answers; a second pushes the feed. The
//! endpoint is `GET /mcpl` with an upgrade on the operator's port.

use std::collections::BTreeSet;
use std::io;
use std::net::TcpStream;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::bus::Record;
use super::hud;
use super::server::{Shared, call_internal, rpc, unix_ms};
use super::ws::{self, Message, Reader};

/// How long the HUD may take before the host gives up (its hook timeout is
/// about 5 s): the AI answers at its next frame while the game runs.
const HUD_TIMEOUT: Duration = Duration::from_millis(3500);
/// The AI's time for the sitrep status (skipped when paused).
const SITREP_TIMEOUT: Duration = Duration::from_millis(3000);
/// Idle ping, so a dead peer is found.
const PING_EVERY: Duration = Duration::from_secs(30);

pub const PROTOCOL_VERSION: &str = "2024-11-05";
/// The game chat channel's id.
pub const CHAT_CHANNEL: &str = "zk:chat";
/// Lines said per publish at most (the engine's chat is one line each).
const SAY_LINES: usize = 3;

fn chat_descriptor(shared: &Shared) -> Value {
    json!({
        "id": CHAT_CHANNEL,
        "type": "zero-k",
        "label": format!("spectator chat (team {}; the opponent does not hear it)", shared.team),
        "direction": "bidirectional",
        "address": {"team": shared.team},
        "metadata": {"serverId": "zkgbai"},
    })
}
pub const MCPL_VERSION: &str = "0.5";

/// What the host enabled (`featureSets/update`), shared with the feed
/// thread.
#[derive(Default)]
struct Policy {
    ready: bool,
    enabled: BTreeSet<String>,
    capabilities: BTreeSet<String>,
    /// The chat channel has been registered with the host.
    chat_registered: bool,
}

impl Policy {
    fn allows(&self, feature: &str, capability: &str) -> bool {
        self.ready && self.enabled.contains(feature) && self.capabilities.contains(capability)
    }
}

/// The MCPL manifest: what this server can do.
pub fn capabilities() -> Value {
    json!({
        "version": MCPL_VERSION,
        "pushEvents": true,
        "contextHooks": {
            "beforeInference": {
                "observe": false,
                "inject": {"system": false, "beforeUser": false, "afterUser": true}
            }
        },
        "inferenceLifecycle": true,
        "channels": {"register": true, "publish": true, "incoming": true},
        "featureSets": {
            "zkgbai.chat": {
                "description": "The game chat as a channel: the agent's plain speech goes to the spectator chat (which it cannot read, as a human cannot), and what players say arrives as channel messages",
                "uses": ["channels.register", "channels.publish", "channels.incoming"]
            },
            "zkgbai.read": {
                "description": "Read what the AI knows: status, economy, military, squads, units, enemies, threat and terrain maps, the journal",
                "uses": ["tools"]
            },
            "zkgbai.act": {
                "description": "Steer the AI: production orders, requisitions, construction, defense points, squad targets, muting modules, taking and ordering units",
                "uses": ["tools"]
            },
            "zkgbai.hud": {
                "description": "The HUD: a compact situation block injected after the user turn before each inference (sections set with the hud tool)",
                "uses": ["contextHooks.beforeInference.inject.afterUser", "tools"]
            },
            "zkgbai.events": {
                "description": "The feed: journal records pushed as they happen (kinds and batching set with the feed tool) and a situation report on a timer",
                "uses": ["pushEvents", "tools"]
            }
        }
    })
}

/// Serve an MCPL session on an upgraded connection until the peer leaves
/// or the server stops.
pub fn serve(stream: TcpStream, shared: &Arc<Shared>) {
    let _ = stream.set_read_timeout(Some(Duration::from_millis(250)));
    let _ = stream.set_nodelay(true);
    let writer = Arc::new(Mutex::new(stream.try_clone().ok()));
    let policy = Arc::new(Mutex::new(Policy::default()));
    let done = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let session = Session { writer: writer.clone(), policy: policy.clone() };

    let feed = {
        let (writer, policy, done, shared) = (writer.clone(), policy.clone(), done.clone(), shared.clone());
        std::thread::Builder::new()
            .name("zkgbai-mcpl-feed".into())
            .spawn(move || feed_loop(&shared, &writer, &policy, &done))
            .ok()
    };

    let mut reader = Reader::new();
    let mut last_ping = Instant::now();
    let mut initialized = false;
    while !shared.shutdown.load(Ordering::SeqCst) {
        let msg = match reader.read(&stream) {
            Ok(Some(m)) => m,
            Ok(None) => {
                if last_ping.elapsed() > PING_EVERY {
                    last_ping = Instant::now();
                    if session.ping().is_err() {
                        break;
                    }
                }
                continue;
            }
            Err(_) => break,
        };
        match msg {
            Message::Text(text) => {
                for line in text.split('\n').map(str::trim).filter(|l| !l.is_empty()) {
                    let Ok(v) = serde_json::from_str::<Value>(line) else {
                        session.send(&json!({"jsonrpc": "2.0", "id": null,
                            "error": {"code": -32700, "message": "parse error"}}));
                        continue;
                    };
                    let msgs: Vec<Value> = match v {
                        Value::Array(a) => a,
                        m => vec![m],
                    };
                    for m in msgs {
                        if let Some(reply) = session.handle(shared, &m, &mut initialized) {
                            session.send(&reply);
                        }
                    }
                }
            }
            Message::Ping(data) => {
                if let Some(w) = writer.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
                    let _ = ws::write_pong(w, &data);
                }
            }
            Message::Pong(_) | Message::Binary(_) => {}
            Message::Close(..) => {
                if let Some(w) = writer.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
                    let _ = ws::write_close(w, 1000, "bye");
                }
                break;
            }
        }
    }
    done.store(true, Ordering::SeqCst);
    if let Some(t) = feed {
        let _ = t.join();
    }
    if shared.shutdown.load(Ordering::SeqCst)
        && let Some(w) = writer.lock().unwrap_or_else(|e| e.into_inner()).as_ref()
    {
        let _ = ws::write_close(w, 1001, "game over");
    }
    *writer.lock().unwrap_or_else(|e| e.into_inner()) = None;
}

struct Session {
    writer: Arc<Mutex<Option<TcpStream>>>,
    policy: Arc<Mutex<Policy>>,
}

fn send_to(writer: &Mutex<Option<TcpStream>>, v: &Value) -> io::Result<()> {
    let guard = writer.lock().unwrap_or_else(|e| e.into_inner());
    match guard.as_ref() {
        Some(w) => ws::write_text(w, &v.to_string()),
        None => Err(io::Error::other("closed")),
    }
}

impl Session {
    fn send(&self, v: &Value) {
        let _ = send_to(&self.writer, v);
    }

    fn ping(&self) -> io::Result<()> {
        let guard = self.writer.lock().unwrap_or_else(|e| e.into_inner());
        match guard.as_ref() {
            Some(w) => ws::write_ping(w, b"zk"),
            None => Err(io::Error::other("closed")),
        }
    }

    /// Handle one message from the host; the reply, if any.
    fn handle(&self, shared: &Shared, msg: &Value, initialized: &mut bool) -> Option<Value> {
        let id = msg.get("id").cloned().filter(|v| !v.is_null());
        let method = msg.get("method").and_then(Value::as_str);
        let params = msg.get("params").cloned().unwrap_or(Value::Null);
        let Some(method) = method else {
            // A response to one of our push requests: nothing to do with it.
            return None;
        };
        let ok = |result: Value| id.clone().map(|id| json!({"jsonrpc": "2.0", "id": id, "result": result}));
        let err = |code: i32, message: String| {
            id.clone().map(|id| json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}}))
        };
        match method {
            "initialize" => {
                let asked = params.get("protocolVersion").and_then(Value::as_str).unwrap_or("");
                let version = if asked.is_empty() { PROTOCOL_VERSION } else { asked };
                ok(json!({
                    "protocolVersion": version,
                    "capabilities": {
                        "tools": {"listChanged": false},
                        "experimental": {"mcpl": capabilities()},
                    },
                    "serverInfo": {"name": shared.name, "version": shared.version},
                    "instructions": shared.instructions,
                }))
            }
            "notifications/initialized" => {
                *initialized = true;
                None
            }
            "featureSets/update" => {
                let mut p = self.policy.lock().unwrap_or_else(|e| e.into_inner());
                let list = |key: &str| -> BTreeSet<String> {
                    params
                        .get(key)
                        .and_then(Value::as_array)
                        .map(|a| a.iter().filter_map(Value::as_str).map(str::to_owned).collect())
                        .unwrap_or_default()
                };
                let manifest = capabilities();
                let declared = manifest["featureSets"].as_object().cloned().unwrap_or_default();
                let disabled = list("disabled");
                if id.is_none() {
                    // A notification may only narrow (§6.7).
                    for d in &disabled {
                        p.enabled.remove(d);
                    }
                    return None;
                }
                let caps = list("effectiveCapabilities");
                let enabled = params.get("enabled").and_then(Value::as_array).map(|a| {
                    a.iter().filter_map(Value::as_str).map(str::to_owned).collect::<BTreeSet<String>>()
                });
                let mut unavailable = Vec::new();
                let mut on = BTreeSet::new();
                for (name, decl) in &declared {
                    if disabled.contains(name) || enabled.as_ref().is_some_and(|e| !e.contains(name)) {
                        continue;
                    }
                    let missing: Vec<&str> = decl["uses"]
                        .as_array()
                        .map(|a| a.iter().filter_map(Value::as_str).filter(|u| !caps.contains(*u)).collect())
                        .unwrap_or_default();
                    if missing.is_empty() {
                        on.insert(name.clone());
                    } else {
                        unavailable.push(json!({"featureSet": name, "missingCapabilities": missing, "effect": "disabled"}));
                    }
                }
                p.enabled = on;
                p.capabilities = caps;
                p.ready = true;
                let mut receipt = json!({"accepted": true, "unavailableFeatures": unavailable, "notes": []});
                if !unavailable_is_empty(&receipt) {
                    receipt["mode"] = json!("degraded");
                }
                let register = p.allows("zkgbai.chat", "channels.register") && !p.chat_registered;
                if register {
                    p.chat_registered = true;
                }
                drop(p);
                // The receipt first (the host activates on it), then the chat
                // channel, then a first message from the game so the host has
                // a locus for the agent's speech from the start.
                if let Some(r) = ok(receipt) {
                    self.send(&r);
                }
                if register {
                    self.send(&json!({"jsonrpc": "2.0", "id": "zk-register", "method": "channels/register",
                        "params": {"channels": [chat_descriptor(shared)]}}));
                    let hello = format!("Game on, team {}: the match is running. What you say here goes to the spectator chat: the opponent does not hear it (use the say tool to address it), and you cannot read what spectators say, as no player can.", shared.team);
                    self.send(&incoming(shared, "game", "the game", &hello, "zk-hello"));
                }
                None
            }
            "channels/list" => ok(json!({"channels": [chat_descriptor(shared)]})),
            "channels/publish" => {
                let allowed = self.policy.lock().unwrap_or_else(|e| e.into_inner()).allows("zkgbai.chat", "channels.publish");
                if !allowed {
                    return err(-32001, "feature set zkgbai.chat not enabled".into());
                }
                if params.get("channelId").and_then(Value::as_str).is_some_and(|c| c != CHAT_CHANNEL) {
                    return err(-32602, "unknown channel".into());
                }
                let text: String = match params.get("content") {
                    Some(Value::String(s)) => s.clone(),
                    Some(Value::Array(blocks)) => blocks
                        .iter()
                        .filter(|b| b.get("type").and_then(Value::as_str) == Some("text"))
                        .filter_map(|b| b.get("text").and_then(Value::as_str))
                        .collect::<Vec<_>>()
                        .join("\n"),
                    _ => String::new(),
                };
                let mut said = 0;
                for line in text.lines().map(str::trim).filter(|l| !l.is_empty()).take(SAY_LINES) {
                    if call_internal(shared, "say", json!({"text": line, "spec": true}), Duration::from_secs(5)).is_ok() {
                        said += 1;
                    }
                }
                ok(json!({"delivered": said > 0, "messageId": format!("say-{}", unix_ms()), "lines": said}))
            }
            "context/beforeInference" => {
                let allowed = self
                    .policy
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .allows("zkgbai.hud", "contextHooks.beforeInference.inject.afterUser");
                let mut injections = Vec::new();
                if allowed {
                    let rendered = call_internal(shared, "hud_render", json!({}), HUD_TIMEOUT).ok();
                    let text = rendered
                        .as_ref()
                        .and_then(|v| v.get("text").and_then(Value::as_str).map(str::to_owned))
                        .unwrap_or_default();
                    let image = rendered
                        .as_ref()
                        .and_then(|v| v.get("image").and_then(Value::as_str))
                        .filter(|s| !s.is_empty())
                        .map(str::to_owned);
                    if !text.is_empty() || image.is_some() {
                        let mut content = vec![json!({"type": "text", "text": format!("--- ZKGBAI HUD ---\n{text}")})];
                        if let Some(data) = image {
                            content.push(json!({"type": "image", "data": data, "mimeType": "image/png"}));
                        }
                        injections.push(json!({
                            "namespace": "zkgbai",
                            "position": "afterUser",
                            "content": content,
                        }));
                    }
                }
                ok(json!({"featureSet": "zkgbai.hud", "contextInjections": injections}))
            }
            "inference/lifecycle" => {
                let rec = json!({
                    "phase": params.get("phase").cloned().unwrap_or(Value::Null),
                    "inference": params.get("inferenceId").cloned().unwrap_or(Value::Null),
                    "turn": params.get("turnIndex").cloned().unwrap_or(Value::Null),
                    "usage": params.get("usage").cloned().unwrap_or(Value::Null),
                });
                let _ = call_internal(shared, "inference", rec, Duration::from_millis(2000));
                None
            }
            "mcpl/manifest" => ok(capabilities()),
            "tools/list" | "tools/call" | "ping" | "resources/list" | "prompts/list" => {
                if method == "tools/call" {
                    let name = params.get("name").and_then(Value::as_str).unwrap_or("");
                    let writes = shared.tools.iter().any(|t| {
                        t["name"] == name && t["annotations"]["readOnlyHint"] == json!(false)
                    });
                    let feature = if writes { "zkgbai.act" } else { "zkgbai.read" };
                    let p = self.policy.lock().unwrap_or_else(|e| e.into_inner());
                    if p.ready && !p.enabled.contains(feature) {
                        return err(-32001, format!("feature set {feature} not enabled"));
                    }
                }
                rpc(shared, msg)
            }
            other if other.starts_with("notifications/") => None,
            other => err(-32601, format!("unknown method '{other}'")),
        }
    }
}

/// A `channels/incoming` request with one message on the chat channel.
fn incoming(shared: &Shared, author_id: &str, author_name: &str, text: &str, message_id: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": format!("zk-in-{message_id}"),
        "method": "channels/incoming",
        "params": {"messages": [{
            "channelId": CHAT_CHANNEL,
            "messageId": format!("{}-{message_id}", shared.run_id),
            "author": {"id": author_id, "name": author_name},
            "timestamp": iso_now(),
            "content": [{"type": "text", "text": text}],
            "metadata": {"team": shared.team},
        }]}
    })
}

fn unavailable_is_empty(receipt: &Value) -> bool {
    receipt["unavailableFeatures"].as_array().is_none_or(|a| a.is_empty())
}

fn iso_now() -> String {
    let secs = unix_ms() / 1000;
    let stamp = super::utc_stamp(secs);
    format!("{}-{}-{}T{}:{}:{}Z", &stamp[0..4], &stamp[4..6], &stamp[6..8], &stamp[9..11], &stamp[11..13], &stamp[13..15])
}

/// Push the feed: records of the configured kinds as they come, batched,
/// and a situation report on the timer.
fn feed_loop(
    shared: &Shared,
    writer: &Arc<Mutex<Option<TcpStream>>>,
    policy: &Arc<Mutex<Policy>>,
    done: &std::sync::atomic::AtomicBool,
) {
    let bus = &shared.bus;
    // Start from now: the host does not want the whole game so far.
    let mut live_since = bus.next_seq();
    let mut sitrep_since = live_since;
    let mut last_sitrep = Instant::now();
    let mut pending: Vec<Arc<Record>> = Vec::new();
    let mut first_pending = Instant::now();
    let mut own_says: std::collections::VecDeque<String> = std::collections::VecDeque::new();
    let mut next_id: u64 = 1;
    let mut push = |kind: &str, text: String, tags: Vec<String>, seq: u64| -> bool {
        next_id += 1;
        let req = json!({
            "jsonrpc": "2.0",
            "method": "push/event",
            "id": format!("zk-{next_id}"),
            "params": {
                "featureSet": "zkgbai.events",
                "eventId": format!("{}-{}-{}", shared.run_id, kind, seq),
                "timestamp": iso_now(),
                "origin": {"server": "zkgbai", "team": shared.team, "kind": kind},
                "tags": tags,
                "payload": {"content": [{"type": "text", "text": text}]},
            }
        });
        send_to(writer, &req).is_ok()
    };
    let mut closed = false;
    while !done.load(Ordering::SeqCst) && !closed {
        let records = match bus.wait(live_since, Duration::from_millis(250)) {
            Some(r) => r,
            None => {
                // The bus closed (game over): flush what is left, once.
                closed = true;
                bus.since(live_since, 2000)
            }
        };
        let (kinds, batch_ms, sitrep_s) = {
            let f = shared.feed.lock().unwrap_or_else(|e| e.into_inner());
            (f.kinds.clone(), f.batch_ms, f.sitrep_s)
        };
        let (allowed, chat) = {
            let p = policy.lock().unwrap_or_else(|e| e.into_inner());
            (p.allows("zkgbai.events", "pushEvents"), p.allows("zkgbai.chat", "channels.incoming") && p.chat_registered)
        };
        for r in records {
            live_since = r.seq + 1;
            if r.kind == "say" {
                // Our own lines come back as chat; remember them to drop the echo.
                if let Ok(v) = serde_json::from_str::<Value>(&r.line) {
                    own_says.push_back(v.get("text").and_then(Value::as_str).unwrap_or("").to_owned());
                    if own_says.len() > 16 {
                        own_says.pop_front();
                    }
                }
            }
            if r.kind == "chat" {
                let v: Value = serde_json::from_str(&r.line).unwrap_or(Value::Null);
                let text = v.get("text").and_then(Value::as_str).unwrap_or("").to_owned();
                let player = v.get("player").and_then(Value::as_i64).unwrap_or(-1);
                let echo = text.strip_prefix("<ZKGBAI> ").is_some_and(|t| own_says.iter().any(|s| s == t));
                let _ = player;
                if echo {
                    continue;
                }
                if chat {
                    let (who, name) = if text.starts_with("<ZKGBAI") {
                        ("zkgbai-other".to_owned(), "the other ZKGBAI".to_owned())
                    } else {
                        (format!("player-{player}"), format!("player {player}"))
                    };
                    if send_to(writer, &incoming(shared, &who, &name, &text, &format!("chat-{}", r.seq))).is_err() {
                        closed = true;
                    }
                    continue;
                }
            }
            if allowed && kinds.contains(&r.kind) && r.kind != "specchat" {
                if pending.is_empty() {
                    first_pending = Instant::now();
                }
                pending.push(r);
            }
        }
        if !allowed {
            // Nothing enabled yet: do not accumulate a backlog for later.
            pending.clear();
            sitrep_since = live_since;
            last_sitrep = Instant::now();
            continue;
        }
        let urgent = pending.iter().any(|r| matches!(r.kind.as_str(), "end" | "chat" | "alert" | "run"));
        if !pending.is_empty() && (closed || urgent || first_pending.elapsed() >= Duration::from_millis(batch_ms)) {
            let first = pending.first().map_or(0, |r| r.seq);
            let text = batch_text(shared, &pending);
            let kinds: BTreeSet<String> = pending.iter().map(|r| r.kind.clone()).collect();
            let mut tags: Vec<String> = kinds.iter().map(|k| format!("zkgbai:{k}")).collect();
            if kinds.contains("chat") {
                tags.push("chat:message".into());
            }
            if !push("feed", text, tags, first) {
                break;
            }
            pending.clear();
        }
        if !closed && sitrep_s > 0 && last_sitrep.elapsed() >= Duration::from_secs(sitrep_s) {
            last_sitrep = Instant::now();
            let since = bus.since(sitrep_since, 5000);
            sitrep_since = live_since;
            let status = call_internal(shared, "hud_render", json!({}), SITREP_TIMEOUT)
                .ok()
                .and_then(|v| v.get("text").and_then(Value::as_str).map(str::to_owned));
            let Some(status) = status else { continue };
            let tally = hud::tally(&since);
            let text = format!(
                "[zkgbai sitrep team {} seq {}]\n{status}\nsince last sitrep ({}s): {}",
                shared.team,
                sitrep_since,
                sitrep_s,
                if tally.is_empty() { "nothing".to_owned() } else { tally }
            );
            if !push("sitrep", text, vec!["zkgbai:sitrep".into()], sitrep_since) {
                break;
            }
        }
    }
}

/// A batch of records as the pilot reads them.
fn batch_text(shared: &Shared, records: &[Arc<Record>]) -> String {
    let mut lines: Vec<String> = Vec::with_capacity(records.len() + 1);
    let (first, last) = (records.first().map_or(0, |r| r.seq), records.last().map_or(0, |r| r.seq));
    lines.push(format!("[zkgbai feed team {} seq {first}-{last}]", shared.team));
    const MAX_LINES: usize = 40;
    if records.len() > MAX_LINES {
        // Keep the head and tail, count the middle.
        let head = MAX_LINES / 2;
        let tail = records.len() - head;
        lines.extend(records[..head].iter().map(|r| hud::line(r)));
        lines.push(format!("... {} more records: {}", tail - head, hud::tally(&records[head..tail])));
        lines.extend(records[tail..].iter().map(|r| hud::line(r)));
    } else {
        lines.extend(records.iter().map(|r| hud::line(r)));
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_declares_the_feature_sets() {
        let c = capabilities();
        assert_eq!(c["version"], MCPL_VERSION);
        assert_eq!(c["pushEvents"], true);
        assert_eq!(c["channels"]["publish"], true);
        let fs = c["featureSets"].as_object().unwrap();
        assert_eq!(fs.len(), 5);
        assert_eq!(fs["zkgbai.chat"]["uses"][1], "channels.publish");
        assert_eq!(fs["zkgbai.hud"]["uses"][0], "contextHooks.beforeInference.inject.afterUser");
    }

    #[test]
    fn policy_requires_readiness_feature_and_capability() {
        let mut p = Policy::default();
        assert!(!p.allows("zkgbai.events", "pushEvents"));
        p.ready = true;
        p.enabled.insert("zkgbai.events".into());
        assert!(!p.allows("zkgbai.events", "pushEvents"));
        p.capabilities.insert("pushEvents".into());
        assert!(p.allows("zkgbai.events", "pushEvents"));
        assert!(!p.allows("zkgbai.hud", "pushEvents"));
    }

    #[test]
    fn iso_times_are_well_formed() {
        let t = iso_now();
        assert_eq!(t.len(), 20);
        assert!(t.ends_with('Z') && t.as_bytes()[10] == b'T');
    }
}
