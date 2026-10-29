//! One outstanding pure calculation per AI subsystem. Callers keep current
//! state instead of queueing obsolete snapshots while the worker is busy.
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Default)]
pub struct Stats {
    pub jobs: u64,
    pub failed: u64,
    pub total: Duration,
    pub max: Duration,
}

impl std::fmt::Display for Stats {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "jobs={} failed={} total={:.1}ms max={:.1}ms",
            self.jobs,
            self.failed,
            self.total.as_secs_f64() * 1000.0,
            self.max.as_secs_f64() * 1000.0
        )
    }
}

pub struct Worker<I, O> {
    input: Option<SyncSender<I>>,
    output: Receiver<(Option<O>, Duration)>,
    thread: Option<JoinHandle<()>>,
    busy: bool,
    stats: Stats,
}

impl<I: Send + 'static, O: Send + 'static> Worker<I, O> {
    pub fn new(name: &str, compute: fn(I) -> O) -> Self {
        let (tx, rx) = mpsc::sync_channel(1);
        let (done, output) = mpsc::sync_channel(1);
        let thread = thread::Builder::new()
            .name(name.into())
            .spawn(move || {
                while let Ok(input) = rx.recv() {
                    let start = Instant::now();
                    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| compute(input))).ok();
                    if done.send((result, start.elapsed())).is_err() {
                        break;
                    }
                }
            })
            .expect("planner worker");
        Self { input: Some(tx), output, thread: Some(thread), busy: false, stats: Stats::default() }
    }

    pub fn busy(&self) -> bool {
        self.busy
    }

    pub fn stats(&self) -> Stats {
        self.stats
    }

    pub fn submit(&mut self, input: I) {
        assert!(!self.busy, "only one outstanding planning snapshot");
        assert!(self.input.as_ref().unwrap().try_send(input).is_ok(), "idle planner worker");
        self.busy = true;
    }

    pub fn poll(&mut self) -> Option<(Option<O>, Duration)> {
        let result = self.output.try_recv().ok()?;
        self.busy = false;
        self.stats.jobs += 1;
        self.stats.failed += u64::from(result.0.is_none());
        self.stats.total += result.1;
        self.stats.max = self.stats.max.max(result.1);
        Some(result)
    }
}

impl<I, O> Drop for Worker<I, O> {
    fn drop(&mut self) {
        // The shared library must not unload with a worker still executing.
        // There is at most one result, so its send never blocks shutdown.
        self.input.take();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn recovers_after_failed_job_and_joins_on_drop() {
        let mut worker = Worker::new("worker-test", |x: u32| {
            assert!(x != 0);
            x * 2
        });
        for (input, expected) in [(0, None), (7, Some(14))] {
            worker.submit(input);
            assert!(worker.busy());
            let start = Instant::now();
            loop {
                if let Some((result, _)) = worker.poll() {
                    assert_eq!(result, expected);
                    assert!(!worker.busy());
                    break;
                }
                assert!(start.elapsed() < Duration::from_secs(5));
                thread::yield_now();
            }
        }
        worker.submit(9);
        drop(worker);
    }
}
