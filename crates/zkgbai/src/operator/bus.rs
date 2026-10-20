//! The event stream: numbered JSON records of what the AI did and saw.
//!
//! The AI appends on its thread; the server's event streams read from other
//! threads, so the recent records sit in a ring behind a mutex, with a
//! condition variable to wake waiting readers.

use std::collections::VecDeque;
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use serde_json::Value;

/// Records kept for late readers.
const CAPACITY: usize = 50_000;

/// One record: `{"seq":..,"f":frame,"k":kind,...data}` as a JSON line.
pub struct Record {
    pub seq: u64,
    pub kind: String,
    pub line: String,
}

#[derive(Default)]
struct Ring {
    next: u64,
    records: VecDeque<Arc<Record>>,
    closed: bool,
}

#[derive(Default)]
pub struct Bus {
    ring: Mutex<Ring>,
    cond: Condvar,
}

/// The JSON line of a record. `data` must be an object (else it goes under
/// `data`); its fields follow the header.
pub fn format_line(seq: u64, frame: i32, kind: &str, data: &Value) -> String {
    let kind = Value::from(kind);
    let fields = match data {
        Value::Object(m) if m.is_empty() => String::new(),
        Value::Object(_) => {
            let s = data.to_string();
            format!(",{}", &s[1..s.len() - 1])
        }
        Value::Null => String::new(),
        other => format!(",\"data\":{other}"),
    };
    format!("{{\"seq\":{seq},\"f\":{frame},\"k\":{kind}{fields}}}")
}

impl Bus {
    /// Append a record; returns it.
    pub fn push(&self, frame: i32, kind: &str, data: &Value) -> Arc<Record> {
        let mut ring = self.ring.lock().unwrap_or_else(|e| e.into_inner());
        let seq = ring.next;
        ring.next += 1;
        let record = Arc::new(Record { seq, kind: kind.to_owned(), line: format_line(seq, frame, kind, data) });
        if ring.records.len() == CAPACITY {
            ring.records.pop_front();
        }
        ring.records.push_back(record.clone());
        drop(ring);
        self.cond.notify_all();
        record
    }

    /// Sequence number the next record gets.
    pub fn next_seq(&self) -> u64 {
        self.ring.lock().unwrap_or_else(|e| e.into_inner()).next
    }

    /// Up to `max` records from `since` on (oldest first), as far back as
    /// the ring goes.
    pub fn since(&self, since: u64, max: usize) -> Vec<Arc<Record>> {
        let ring = self.ring.lock().unwrap_or_else(|e| e.into_inner());
        let first = ring.records.front().map_or(ring.next, |r| r.seq);
        let skip = since.saturating_sub(first) as usize;
        ring.records.iter().skip(skip).take(max).cloned().collect()
    }

    /// The last `n` records matching `keep`, oldest first.
    pub fn tail(&self, n: usize, keep: impl Fn(&Record) -> bool) -> Vec<Arc<Record>> {
        let ring = self.ring.lock().unwrap_or_else(|e| e.into_inner());
        let mut out: Vec<_> = ring.records.iter().rev().filter(|r| keep(r)).take(n).cloned().collect();
        out.reverse();
        out
    }

    /// Wait up to `timeout` for records from `since` on; `None` once closed.
    pub fn wait(&self, since: u64, timeout: Duration) -> Option<Vec<Arc<Record>>> {
        let ring = self.ring.lock().unwrap_or_else(|e| e.into_inner());
        let (ring, _) = self
            .cond
            .wait_timeout_while(ring, timeout, |r| r.next <= since && !r.closed)
            .unwrap_or_else(|e| e.into_inner());
        if ring.closed {
            return None;
        }
        drop(ring);
        Some(self.since(since, 2000))
    }

    /// Wake and end all waiting readers.
    pub fn close(&self) {
        self.ring.lock().unwrap_or_else(|e| e.into_inner()).closed = true;
        self.cond.notify_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn lines_put_the_header_first() {
        assert_eq!(format_line(3, 90, "log", &json!({"msg": "hi"})), r#"{"seq":3,"f":90,"k":"log","msg":"hi"}"#);
        assert_eq!(format_line(0, 0, "end", &json!({})), r#"{"seq":0,"f":0,"k":"end"}"#);
        assert_eq!(format_line(1, 2, "x", &json!([1])), r#"{"seq":1,"f":2,"k":"x","data":[1]}"#);
    }

    #[test]
    fn ring_reads_from_a_sequence_number() {
        let bus = Bus::default();
        for i in 0..5 {
            bus.push(i, "t", &json!({}));
        }
        let got: Vec<u64> = bus.since(3, 10).iter().map(|r| r.seq).collect();
        assert_eq!(got, [3, 4]);
        assert_eq!(bus.tail(2, |_| true).iter().map(|r| r.seq).collect::<Vec<_>>(), [3, 4]);
        assert_eq!(bus.wait(5, Duration::from_millis(1)).map(|v| v.len()), Some(0));
        bus.close();
        assert!(bus.wait(5, Duration::from_millis(1)).is_none());
    }
}
