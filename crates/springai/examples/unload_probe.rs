//! Test library for an AI freed while a thread of it still runs (see
//! `tools/dll-smoke --unload`). Creating the AI starts a detached thread
//! that sleeps, then writes the file named by `UNLOAD_PROBE_MARKER`;
//! `unload_probe_spawn` starts the same thread without going through
//! `init`.

use std::time::Duration;

use springai::{Callback, Event, SkirmishAi};

/// How long the thread sleeps before running the library's code again.
const DELAY: Duration = Duration::from_millis(500);

fn spawn_detached() {
    std::thread::spawn(|| {
        std::thread::sleep(DELAY);
        if let Some(path) = std::env::var_os("UNLOAD_PROBE_MARKER") {
            let _ = std::fs::write(path, "done");
        }
    });
}

struct Probe;

impl SkirmishAi for Probe {
    fn new(_cb: Callback) -> Self {
        spawn_detached();
        Probe
    }

    fn handle_event(&mut self, _event: Event) -> i32 {
        0
    }
}

springai::export_ai!(Probe);

/// Start the thread outside `init`, so nothing the AI glue does at `init`
/// applies to the library.
#[unsafe(no_mangle)]
pub extern "C" fn unload_probe_spawn() {
    spawn_detached();
}
