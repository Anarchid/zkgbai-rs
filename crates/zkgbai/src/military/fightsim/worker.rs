//! The fight predictor's thread: a request queue in front of [`Model`].
//!
//! As the terrain analysis (`terrain.rs`), the engine thread copies what the
//! model needs into a [`FightRequest`] and a named thread that never touches
//! the engine does the work; answers are picked up later with `try_recv`
//! (see `live.rs`, which drains them at the top of `war_update`). The
//! request channel is bounded: when it is full the caller is told so at
//! once and falls back to the flat threat, so the engine thread never
//! waits on the predictor.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, SyncSender, TrySendError, channel, sync_channel};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use super::{FightOutcome, FightSnapshot, Model};

/// Requests waiting at most; beyond it callers fall back.
pub const QUEUE: usize = 8;

/// A fight to predict, from the frame the snapshot was taken.
pub struct FightRequest {
    /// The requester (a squad id); answers are kept per key.
    pub key: u32,
    pub frame: i32,
    pub snapshot: FightSnapshot,
    /// Also run the mechanics model.
    pub mech: bool,
}

/// The model's answer to a request.
#[derive(Clone, Copy, Debug)]
pub struct FightAnswer {
    pub key: u32,
    /// The request's frame.
    pub frame: i32,
    pub outcome: FightOutcome,
    /// Microseconds the prediction took on the worker.
    pub micros: u32,
}

/// Running totals for the cost line.
#[derive(Clone, Copy, Debug, Default)]
pub struct WorkerStats {
    pub queries: u64,
    /// Requests refused because the queue was full.
    pub full: u64,
    pub answers: u64,
    pub micros: u64,
    pub max_micros: u64,
}

pub struct FightWorker {
    tx: Option<SyncSender<FightRequest>>,
    rx: Receiver<FightAnswer>,
    shutdown: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    stats: WorkerStats,
}

impl FightWorker {
    /// Start the thread with a loaded model.
    pub fn start(model: Model) -> Result<FightWorker, String> {
        let (tx, rx) = sync_channel::<FightRequest>(QUEUE);
        let (atx, arx) = channel::<FightAnswer>();
        let shutdown = Arc::new(AtomicBool::new(false));
        let flag = shutdown.clone();
        let thread = std::thread::Builder::new()
            .name("zkgbai-fights".into())
            .spawn(move || serve(model, rx, atx, &flag))
            .map_err(|e| format!("cannot start the fight predictor thread ({e})"))?;
        Ok(FightWorker { tx: Some(tx), rx: arx, shutdown, thread: Some(thread), stats: WorkerStats::default() })
    }

    /// Queue a request; false when the queue is full (or the thread is
    /// gone), in which case the caller decides without the model.
    pub fn request(&mut self, req: FightRequest) -> bool {
        self.stats.queries += 1;
        match self.tx.as_ref().map(|tx| tx.try_send(req)) {
            Some(Ok(())) => true,
            Some(Err(TrySendError::Full(_))) => {
                self.stats.full += 1;
                false
            }
            _ => false,
        }
    }

    /// Every answer that has arrived since the last drain.
    pub fn drain(&mut self) -> Vec<FightAnswer> {
        let mut out = Vec::new();
        while let Ok(a) = self.rx.try_recv() {
            self.stats.answers += 1;
            self.stats.micros += a.micros as u64;
            self.stats.max_micros = self.stats.max_micros.max(a.micros as u64);
            out.push(a);
        }
        out
    }

    pub fn stats(&self) -> WorkerStats {
        self.stats
    }
}

impl Drop for FightWorker {
    fn drop(&mut self) {
        self.shutdown.store(true, Ordering::Relaxed);
        // Closing the request channel wakes the thread at once.
        self.tx = None;
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn serve(model: Model, rx: Receiver<FightRequest>, tx: Sender<FightAnswer>, shutdown: &AtomicBool) {
    while !shutdown.load(Ordering::Relaxed) {
        match rx.recv_timeout(Duration::from_millis(200)) {
            Ok(req) => {
                let start = Instant::now();
                let outcome = model.predict_with(&req.snapshot, req.mech);
                let micros = start.elapsed().as_micros().min(u32::MAX as u128) as u32;
                if tx.send(FightAnswer { key: req.key, frame: req.frame, outcome, micros }).is_err() {
                    break;
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::military::fightsim::{SnapUnit, UnitSpec, WeaponSpec};

    fn raider() -> Arc<UnitSpec> {
        Arc::new(UnitSpec {
            def: "cloakraid".into(),
            cost: 65.0,
            speed: 3.85,
            mobile: true,
            weapons: vec![WeaponSpec {
                name: "cloakraid_emg".into(),
                range: 185.0,
                reload: 0.3,
                salvo: 3,
                damage: 10.5,
                weapon_type: "Cannon".into(),
            }],
            health: 230.0,
            radius: 9.0,
            ..Default::default()
        })
    }

    #[test]
    fn round_trip_and_full_queue() {
        let mut w = FightWorker::start(Model::load().unwrap()).unwrap();
        let snap = FightSnapshot {
            units: (0..6)
                .map(|i| SnapUnit {
                    side: (i % 2) as u8,
                    spec: raider(),
                    hp: 230.0,
                    x: if i % 2 == 0 { 0.0 } else { 400.0 },
                    z: i as f32 * 10.0,
                })
                .collect(),
        };
        // More requests than the queue holds: some are refused, none block.
        let mut accepted = 0;
        for i in 0..(QUEUE as u32 * 4) {
            if w.request(FightRequest { key: i, frame: 100, snapshot: snap.clone(), mech: i % 2 == 0 }) {
                accepted += 1;
            }
        }
        assert!(accepted >= QUEUE as u32);
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut answers = Vec::new();
        while answers.len() < accepted as usize && Instant::now() < deadline {
            answers.extend(w.drain());
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(answers.len(), accepted as usize);
        let a = &answers[0];
        assert_eq!(a.frame, 100);
        // A mirror: both sides expect to lose alike, within the sim's asymmetry.
        assert!((a.outcome.killed_a - a.outcome.killed_b).abs() < 0.6, "{:?}", a.outcome);
        assert!(a.outcome.armed_a == 3 && a.outcome.armed_b == 3);
        assert!(answers.iter().all(|a| a.outcome.mech.is_some() == (a.key % 2 == 0)));
        let s = w.stats();
        assert_eq!(s.queries, QUEUE as u64 * 4);
        assert_eq!(s.answers, accepted as u64);
        assert!(s.micros > 0);
        drop(w);
    }
}
