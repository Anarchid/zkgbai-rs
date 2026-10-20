//! The operator's HTTP server: a thread per connection, std only.
//!
//! Endpoints (all JSON unless noted):
//! - `GET /`: the dashboard (HTML).
//! - `POST /mcp`, `POST /rpc`: JSON-RPC 2.0. MCP's `initialize`, `ping`,
//!   `tools/list` and `tools/call` (Streamable HTTP, plain JSON replies), and
//!   any tool by name as the method with its arguments as params.
//! - `GET /call/<tool>?arg=value`, `POST /call/<tool>` (arguments object):
//!   the tool's result, or `{"error": ...}` with status 400.
//! - `GET /tools`: name, description and input schema of every tool.
//! - `GET /events?since=<seq>&kinds=a,b`: the event stream (SSE).
//! - `GET /snapshot`: the latest world snapshot (served even while the game
//!   is paused; the AI refreshes it each second while someone asks).
//! - `GET /journal`, `GET /journals`, `GET /journals/<name>`: this run's
//!   journal, and the list and files of the journal directory.
//! - `GET /health`: liveness and the current frame.
//!
//! Tool calls run on the AI's thread at its next frame: the connection
//! thread hands them over and waits for the answer.

use std::io::{self, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream, ToSocketAddrs};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU64, Ordering};
use std::sync::mpsc::{Sender, SyncSender, sync_channel};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

use super::bus::Bus;

/// How long a caller waits for the AI to take a call (it only does while
/// the game runs).
const CALL_TIMEOUT: Duration = Duration::from_secs(15);
const MAX_HEADER: usize = 64 * 1024;
const MAX_BODY: usize = 4 * 1024 * 1024;
const MAX_CONNECTIONS: usize = 32;
/// Idle keep-alive connections are closed after this long.
const IDLE: Duration = Duration::from_secs(60);
/// Ports tried after the one asked for, when it is taken.
const PORT_TRIES: u16 = 32;

const DASHBOARD: &str = include_str!("dashboard.html");

/// MCP protocol versions we speak (newest first).
const MCP_VERSIONS: [&str; 3] = ["2025-06-18", "2025-03-26", "2024-11-05"];

/// A tool call for the AI's thread.
pub struct Call {
    pub tool: String,
    pub args: Value,
    pub reply: SyncSender<Result<Value, String>>,
}

/// What the server threads share with the AI.
pub struct Shared {
    pub bus: Arc<Bus>,
    pub calls: Sender<Call>,
    /// `tools/list` entries.
    pub tools: Vec<Value>,
    /// Name and version for MCP's `serverInfo`, and instructions.
    pub name: String,
    pub version: String,
    pub instructions: String,
    /// The latest snapshot (JSON), and when one was last asked for (unix ms).
    pub snapshot: Mutex<Option<String>>,
    pub snapshot_wanted: AtomicU64,
    pub frame: AtomicI32,
    /// Our team and ally team (so clients can tell instances apart).
    pub team: i32,
    pub ally_team: i32,
    pub journal: Option<PathBuf>,
    pub shutdown: AtomicBool,
}

pub fn unix_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64)
}

pub struct Server {
    pub addr: String,
    shared: Arc<Shared>,
    threads: Arc<Mutex<Vec<JoinHandle<()>>>>,
    acceptor: Option<JoinHandle<()>>,
}

impl Server {
    /// Listen on `spec` (`host:port`), or on the next free port of the
    /// following few.
    pub fn start(spec: &str, shared: Arc<Shared>) -> io::Result<Server> {
        let (host, port) = match spec.rsplit_once(':') {
            Some((h, p)) => (h.to_owned(), p.parse::<u16>().map_err(|e| io::Error::other(e.to_string()))?),
            None => return Err(io::Error::other(format!("bad address '{spec}'"))),
        };
        let mut last = io::Error::other("no port tried");
        for p in port..port.saturating_add(PORT_TRIES) {
            let addrs: Vec<_> = (host.as_str(), p).to_socket_addrs()?.collect();
            match TcpListener::bind(&addrs[..]) {
                Ok(listener) => {
                    listener.set_nonblocking(true)?;
                    let addr = listener.local_addr()?.to_string();
                    let threads = Arc::new(Mutex::new(Vec::new()));
                    let (s, t) = (shared.clone(), threads.clone());
                    let acceptor = std::thread::Builder::new()
                        .name("zkgbai-operator".into())
                        .spawn(move || accept_loop(listener, s, t))?;
                    return Ok(Server { addr, shared, threads, acceptor: Some(acceptor) });
                }
                Err(e) => last = e,
            }
        }
        Err(last)
    }

    /// Stop accepting, end all connections and wait for their threads (the
    /// engine may unload our library right after).
    pub fn stop(&mut self) {
        self.shared.shutdown.store(true, Ordering::SeqCst);
        self.shared.bus.close();
        if let Some(a) = self.acceptor.take() {
            let _ = a.join();
        }
        let threads = std::mem::take(&mut *self.threads.lock().unwrap_or_else(|e| e.into_inner()));
        for t in threads {
            let _ = t.join();
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop();
    }
}

fn accept_loop(listener: TcpListener, shared: Arc<Shared>, threads: Arc<Mutex<Vec<JoinHandle<()>>>>) {
    while !shared.shutdown.load(Ordering::SeqCst) {
        match listener.accept() {
            Ok((stream, _)) => {
                let mut list = threads.lock().unwrap_or_else(|e| e.into_inner());
                list.retain(|t| !t.is_finished());
                if list.len() >= MAX_CONNECTIONS {
                    let _ = respond(&stream, 503, "text/plain", b"too many connections", &[], false);
                    continue;
                }
                let s = shared.clone();
                if let Ok(t) =
                    std::thread::Builder::new().name("zkgbai-operator-conn".into()).spawn(move || serve(stream, &s))
                {
                    list.push(t);
                }
            }
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => std::thread::sleep(Duration::from_millis(50)),
            Err(_) => std::thread::sleep(Duration::from_millis(200)),
        }
    }
}

struct Request {
    method: String,
    path: String,
    query: Vec<(String, String)>,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Request {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }

    fn param(&self, name: &str) -> Option<&str> {
        self.query.iter().find(|(k, _)| k == name).map(|(_, v)| v.as_str())
    }
}

/// Serve one connection's requests until it closes, idles out, or we stop.
fn serve(stream: TcpStream, shared: &Shared) {
    let _ = stream.set_nonblocking(false);
    let _ = stream.set_read_timeout(Some(Duration::from_millis(500)));
    let _ = stream.set_nodelay(true);
    let mut buf = Vec::new();
    while let Ok(Some(req)) = read_request(&stream, &mut buf, shared) {
        let close = req.header("connection").is_some_and(|c| c.eq_ignore_ascii_case("close"));
        if !route(&stream, &req, shared, !close) || close {
            break;
        }
    }
    let _ = stream.shutdown(Shutdown::Both);
}

/// Read the next request; `None` when the peer is done (or we stop).
fn read_request(mut stream: &TcpStream, buf: &mut Vec<u8>, shared: &Shared) -> io::Result<Option<Request>> {
    let start = Instant::now();
    let mut chunk = [0u8; 8192];
    let header_end = loop {
        if let Some(i) = find(buf, b"\r\n\r\n") {
            break i;
        }
        if buf.len() > MAX_HEADER || shared.shutdown.load(Ordering::SeqCst) || start.elapsed() > IDLE {
            return Ok(None);
        }
        match stream.read(&mut chunk) {
            Ok(0) => return Ok(None),
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
            Err(e) if matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut) => {}
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    };
    let head = String::from_utf8_lossy(&buf[..header_end]).into_owned();
    let mut lines = head.split("\r\n");
    let mut first = lines.next().unwrap_or("").split_whitespace();
    let method = first.next().unwrap_or("").to_owned();
    let target = first.next().unwrap_or("/").to_owned();
    let headers: Vec<(String, String)> =
        lines.filter_map(|l| l.split_once(':')).map(|(k, v)| (k.trim().to_owned(), v.trim().to_owned())).collect();
    let len = headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("content-length"))
        .and_then(|(_, v)| v.parse::<usize>().ok())
        .unwrap_or(0);
    if len > MAX_BODY {
        return Ok(None);
    }
    let body_start = header_end + 4;
    while buf.len() < body_start + len {
        if shared.shutdown.load(Ordering::SeqCst) || start.elapsed() > IDLE {
            return Ok(None);
        }
        match stream.read(&mut chunk) {
            Ok(0) => return Ok(None),
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
            Err(e) if matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut) => {}
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    let body = buf[body_start..body_start + len].to_vec();
    buf.drain(..body_start + len);
    let (path, query) = match target.split_once('?') {
        Some((p, q)) => (p.to_owned(), parse_query(q)),
        None => (target, Vec::new()),
    };
    Ok(Some(Request { method, path: percent_decode(&path), query, headers, body }))
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

fn parse_query(q: &str) -> Vec<(String, String)> {
    q.split('&')
        .filter(|kv| !kv.is_empty())
        .map(|kv| match kv.split_once('=') {
            Some((k, v)) => (percent_decode(k), percent_decode(v)),
            None => (percent_decode(kv), String::new()),
        })
        .collect()
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < b.len() => {
                match std::str::from_utf8(&b[i + 1..i + 3]).ok().and_then(|h| u8::from_str_radix(h, 16).ok()) {
                    Some(v) => {
                        out.push(v);
                        i += 2;
                    }
                    None => out.push(b'%'),
                }
            }
            c => out.push(c),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Only pages from this machine (or this server) may call us.
fn allowed_origin(req: &Request) -> Option<Option<String>> {
    let Some(origin) = req.header("origin") else { return Some(None) };
    if origin == "null" {
        return Some(Some(origin.to_owned()));
    }
    let host = origin.split("://").nth(1).unwrap_or("");
    let host = host.split('/').next().unwrap_or("");
    let bare = if host.starts_with('[') {
        host.split(']').next().map(|h| &h[1..]).unwrap_or("")
    } else {
        host.split(':').next().unwrap_or("")
    };
    let same = req.header("host").is_some_and(|h| h == host);
    if same || matches!(bare, "127.0.0.1" | "localhost" | "::1") { Some(Some(origin.to_owned())) } else { None }
}

fn respond(
    mut stream: &TcpStream,
    status: u16,
    content_type: &str,
    body: &[u8],
    extra: &[(&str, String)],
    keep_alive: bool,
) -> io::Result<()> {
    let reason = match status {
        200 => "OK",
        202 => "Accepted",
        204 => "No Content",
        400 => "Bad Request",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        503 => "Service Unavailable",
        _ => "Error",
    };
    let mut head = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: {}\r\n",
        body.len(),
        if keep_alive { "keep-alive" } else { "close" }
    );
    for (k, v) in extra {
        head.push_str(&format!("{k}: {v}\r\n"));
    }
    head.push_str("\r\n");
    stream.write_all(head.as_bytes())?;
    stream.write_all(body)?;
    stream.flush()
}

fn json_response(stream: &TcpStream, status: u16, v: &Value, cors: &[(&str, String)], keep: bool) -> bool {
    respond(stream, status, "application/json", v.to_string().as_bytes(), cors, keep).is_ok()
}

/// Handle one request; `false` to close the connection.
fn route(stream: &TcpStream, req: &Request, shared: &Shared, keep: bool) -> bool {
    let Some(origin) = allowed_origin(req) else {
        let _ = respond(stream, 403, "text/plain", b"origin not allowed", &[], false);
        return false;
    };
    let mut cors: Vec<(&str, String)> = Vec::new();
    if let Some(o) = origin {
        cors.push(("Access-Control-Allow-Origin", o));
        cors.push(("Vary", "Origin".into()));
    }
    if req.method == "OPTIONS" {
        cors.push(("Access-Control-Allow-Methods", "GET, POST, OPTIONS".into()));
        cors.push((
            "Access-Control-Allow-Headers",
            "Content-Type, Mcp-Session-Id, MCP-Protocol-Version, Last-Event-ID".into(),
        ));
        return respond(stream, 204, "text/plain", b"", &cors, keep).is_ok();
    }
    let path = req.path.trim_end_matches('/');
    match (req.method.as_str(), path) {
        ("GET", "" | "/index.html" | "/dashboard") => {
            respond(stream, 200, "text/html; charset=utf-8", DASHBOARD.as_bytes(), &cors, keep).is_ok()
        }
        ("GET", "/health") => json_response(
            stream,
            200,
            &json!({"ok": true, "frame": shared.frame.load(Ordering::Relaxed), "name": shared.name,
                    "version": shared.version, "team": shared.team, "ally_team": shared.ally_team}),
            &cors,
            keep,
        ),
        ("GET", "/tools") => json_response(stream, 200, &json!({"tools": shared.tools}), &cors, keep),
        ("GET", "/snapshot") => {
            shared.snapshot_wanted.store(unix_ms(), Ordering::Relaxed);
            let snap = shared.snapshot.lock().unwrap_or_else(|e| e.into_inner()).clone();
            let body = snap.unwrap_or_else(|| "null".into());
            respond(stream, 200, "application/json", body.as_bytes(), &cors, keep).is_ok()
        }
        ("GET", "/events") => {
            events(stream, req, shared, &cors);
            false
        }
        ("GET", "/journal") => match &shared.journal {
            Some(p) => send_file(stream, p, &cors, keep),
            None => json_response(
                stream,
                404,
                &json!({"error": "no journal (set ZKGBAI_JOURNAL or the journal option)"}),
                &cors,
                keep,
            ),
        },
        ("GET", "/journals") => json_response(stream, 200, &list_journals(shared), &cors, keep),
        ("GET", p) if p.starts_with("/journals/") => {
            let name = &p["/journals/".len()..];
            let dir = shared.journal.as_ref().and_then(|j| j.parent());
            match dir {
                Some(d) if !name.contains(['/', '\\']) && !name.starts_with('.') && name.ends_with(".jsonl") => {
                    send_file(stream, &d.join(name), &cors, keep)
                }
                _ => json_response(stream, 404, &json!({"error": "no such journal"}), &cors, keep),
            }
        }
        ("GET" | "POST", p) if p.starts_with("/call/") => {
            let tool = &p["/call/".len()..];
            let args = if req.method == "POST" && !req.body.is_empty() {
                match serde_json::from_slice::<Value>(&req.body) {
                    Ok(v) => v,
                    Err(e) => {
                        return json_response(stream, 400, &json!({"error": format!("bad JSON: {e}")}), &cors, keep);
                    }
                }
            } else {
                query_args(&req.query)
            };
            match call(shared, tool, args) {
                Ok(v) => json_response(stream, 200, &v, &cors, keep),
                Err(e) => json_response(stream, 400, &json!({"error": e}), &cors, keep),
            }
        }
        ("POST", "/mcp" | "/rpc") => {
            let msg = match serde_json::from_slice::<Value>(&req.body) {
                Ok(v) => v,
                Err(e) => {
                    let err = json!({"jsonrpc": "2.0", "id": null, "error": {"code": -32700, "message": format!("parse error: {e}")}});
                    return json_response(stream, 400, &err, &cors, keep);
                }
            };
            let reply = match &msg {
                Value::Array(batch) => {
                    let out: Vec<Value> = batch.iter().filter_map(|m| rpc(shared, m)).collect();
                    if out.is_empty() { None } else { Some(Value::Array(out)) }
                }
                m => rpc(shared, m),
            };
            match reply {
                Some(r) => json_response(stream, 200, &r, &cors, keep),
                None => respond(stream, 202, "application/json", b"", &cors, keep).is_ok(),
            }
        }
        ("GET" | "DELETE", "/mcp") => {
            respond(stream, 405, "text/plain", b"no server-initiated stream", &cors, keep).is_ok()
        }
        _ => {
            json_response(stream, 404, &json!({"error": format!("no route {} {}", req.method, req.path)}), &cors, keep)
        }
    }
}

/// Query parameters as tool arguments: numbers, booleans and comma lists of
/// numbers become JSON values.
fn query_args(query: &[(String, String)]) -> Value {
    let mut m = serde_json::Map::new();
    for (k, v) in query {
        let value = if let Ok(n) = v.parse::<i64>() {
            json!(n)
        } else if let Ok(f) = v.parse::<f64>() {
            json!(f)
        } else if v == "true" || v == "false" {
            json!(v == "true")
        } else if v.contains(',') && v.split(',').all(|p| p.trim().parse::<f64>().is_ok()) {
            Value::Array(v.split(',').filter_map(|p| p.trim().parse::<f64>().ok()).map(|n| json!(n)).collect())
        } else {
            json!(v)
        };
        m.insert(k.clone(), value);
    }
    Value::Object(m)
}

/// Run a tool on the AI's thread and wait for its answer.
fn call(shared: &Shared, tool: &str, args: Value) -> Result<Value, String> {
    if !shared.tools.iter().any(|t| t["name"] == tool) {
        return Err(format!("unknown tool '{tool}' (GET /tools lists them)"));
    }
    let (tx, rx) = sync_channel(1);
    shared.calls.send(Call { tool: tool.to_owned(), args, reply: tx }).map_err(|_| "the AI is gone".to_owned())?;
    rx.recv_timeout(CALL_TIMEOUT).unwrap_or_else(|_| {
        Err(format!("no answer within {}s: the AI only answers while the game runs (paused?)", CALL_TIMEOUT.as_secs()))
    })
}

/// One JSON-RPC message; `None` for notifications and responses.
fn rpc(shared: &Shared, msg: &Value) -> Option<Value> {
    let id = msg.get("id").cloned();
    let method = msg.get("method").and_then(Value::as_str);
    let (Some(id), Some(method)) = (id, method) else { return None };
    let params = msg.get("params").cloned().unwrap_or(Value::Null);
    let ok = |result: Value| Some(json!({"jsonrpc": "2.0", "id": id, "result": result}));
    let err = |code: i32, message: String| {
        Some(json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}}))
    };
    match method {
        "initialize" => {
            let asked = params.get("protocolVersion").and_then(Value::as_str).unwrap_or("");
            let version = MCP_VERSIONS.iter().find(|v| **v == asked).copied().unwrap_or(MCP_VERSIONS[0]);
            ok(json!({
                "protocolVersion": version,
                "capabilities": {"tools": {"listChanged": false}},
                "serverInfo": {"name": shared.name, "version": shared.version},
                "instructions": shared.instructions,
            }))
        }
        "ping" => ok(json!({})),
        "tools/list" => ok(json!({"tools": shared.tools})),
        "tools/call" => {
            let name = params.get("name").and_then(Value::as_str).unwrap_or("");
            if !shared.tools.iter().any(|t| t["name"] == name) {
                return err(-32602, format!("unknown tool '{name}'"));
            }
            let args = params.get("arguments").cloned().unwrap_or_else(|| json!({}));
            let (text, structured, is_error) = match call(shared, name, args) {
                Ok(v) => (serde_json::to_string_pretty(&v).unwrap_or_default(), Some(v), false),
                Err(e) => (e, None, true),
            };
            let mut result = json!({"content": [{"type": "text", "text": text}], "isError": is_error});
            if let Some(v @ Value::Object(_)) = structured {
                result["structuredContent"] = v;
            }
            ok(result)
        }
        "resources/list" => ok(json!({"resources": []})),
        "prompts/list" => ok(json!({"prompts": []})),
        tool if shared.tools.iter().any(|t| t["name"] == tool) => {
            let args = if params.is_null() { json!({}) } else { params };
            match call(shared, tool, args) {
                Ok(v) => ok(v),
                Err(e) => err(-32000, e),
            }
        }
        other => err(-32601, format!("unknown method '{other}'")),
    }
}

/// Stream events as server-sent events until the client leaves.
fn events(mut stream: &TcpStream, req: &Request, shared: &Shared, cors: &[(&str, String)]) {
    let kinds: Vec<&str> =
        req.param("kinds").map(|k| k.split(',').filter(|s| !s.is_empty()).collect()).unwrap_or_default();
    let latest = shared.bus.next_seq();
    let mut since = req
        .header("last-event-id")
        .and_then(|v| v.parse::<u64>().ok().map(|n| n + 1))
        .or_else(|| req.param("since").and_then(|v| v.parse().ok()))
        .unwrap_or(latest.saturating_sub(500));
    let mut head = String::from(
        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-store\r\nConnection: close\r\n",
    );
    for (k, v) in cors {
        head.push_str(&format!("{k}: {v}\r\n"));
    }
    head.push_str("\r\nretry: 2000\n\n");
    if stream.write_all(head.as_bytes()).is_err() {
        return;
    }
    let mut quiet = Instant::now();
    loop {
        let Some(records) = shared.bus.wait(since, Duration::from_secs(1)) else { return };
        if shared.shutdown.load(Ordering::SeqCst) {
            return;
        }
        let mut out = String::new();
        for r in &records {
            since = r.seq + 1;
            if kinds.is_empty() || kinds.contains(&r.kind.as_str()) {
                out.push_str(&format!("id: {}\ndata: {}\n\n", r.seq, r.line));
            }
        }
        if out.is_empty() && quiet.elapsed() > Duration::from_secs(15) {
            out.push_str(": keep-alive\n\n");
        }
        if !out.is_empty() {
            quiet = Instant::now();
            if stream.write_all(out.as_bytes()).and_then(|_| stream.flush()).is_err() {
                return;
            }
        }
    }
}

fn send_file(stream: &TcpStream, path: &std::path::Path, cors: &[(&str, String)], keep: bool) -> bool {
    match std::fs::read(path) {
        Ok(data) => respond(stream, 200, "application/x-ndjson", &data, cors, keep).is_ok(),
        Err(e) => json_response(stream, 404, &json!({"error": e.to_string()}), cors, keep),
    }
}

fn list_journals(shared: &Shared) -> Value {
    let Some(dir) = shared.journal.as_ref().and_then(|j| j.parent()) else { return json!({"journals": []}) };
    let mut list: Vec<(u64, Value)> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().ends_with(".jsonl"))
        .filter_map(|e| {
            let meta = e.metadata().ok()?;
            let modified = meta.modified().ok()?.duration_since(UNIX_EPOCH).ok()?.as_secs();
            Some((modified, json!({"name": e.file_name().to_string_lossy(), "size": meta.len(), "modified": modified})))
        })
        .collect();
    list.sort_by(|a, b| b.0.cmp(&a.0));
    json!({"dir": dir.to_string_lossy(), "journals": list.into_iter().map(|(_, v)| v).collect::<Vec<_>>()})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn queries_decode_into_arguments() {
        let q = parse_query("x=1200&z=-3.5&name=turret%20laser&ids=1,2,3&on=true&f=a+b");
        let v = query_args(&q);
        assert_eq!(
            v,
            json!({"x": 1200, "z": -3.5, "name": "turret laser", "ids": [1.0, 2.0, 3.0], "on": true, "f": "a b"})
        );
        assert_eq!(percent_decode("%2Fa%"), "/a%");
        assert_eq!(percent_decode("100%25"), "100%");
    }
}
