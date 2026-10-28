//! Loads a SkirmishAI library and drives its entry points with an empty
//! callback table: every engine call the AI makes then panics inside the
//! AI. The library must contain that panic and report an error code
//! instead of unwinding into (and crashing) the host. On Windows the
//! library must also stay loaded once freed: the engine frees an AI's
//! library when it releases the last instance (`/aikill`, a resign), and a
//! thread of the AI still running then would run on in unmapped code.
//!
//! With `--unload`, loads the springai test library `unload_probe` in child
//! processes and frees it while a thread it started still sleeps (Windows
//! only):
//! - `keep`: the library is not freed; the thread finishes (the probe works).
//! - `spawn`: the thread is started outside `init`; freeing the library must
//!   crash the process (the hazard is real).
//! - `init`: the thread is started by the AI created in `init`; the process
//!   must live and the thread finish (the AI glue keeps the library loaded).
//!
//! usage: dll-smoke <path to libSkirmishAI.so / SkirmishAI.dll>
//!        dll-smoke --unload <path to unload_probe.dll>

use std::ffi::{c_int, c_void};
use std::path::Path;
use std::process::{Command, ExitStatus};
use std::time::{Duration, Instant};

use springai_sys::SSkirmishAICallback;

type InitFn = unsafe extern "C" fn(c_int, *const SSkirmishAICallback) -> c_int;
type ReleaseFn = unsafe extern "C" fn(c_int) -> c_int;
type HandleEventFn = unsafe extern "C" fn(c_int, c_int, *const c_void) -> c_int;
type SpawnFn = unsafe extern "C" fn();

/// How long a child waits after freeing the probe: well past the probe
/// thread's sleep.
const LINGER: Duration = Duration::from_secs(3);
/// A child taking longer than this has hung.
const CHILD_TIMEOUT: Duration = Duration::from_secs(30);

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let arg = |i: usize| args.get(i).map(String::as_str).expect("usage: dll-smoke [--unload] <library>");
    match arg(1) {
        "--unload" => unload(arg(2)),
        "--unload-child" => unload_child(arg(2), arg(3)),
        path => contract(path),
    }
}

fn contract(path: &str) {
    let lib = unsafe { libloading::Library::new(path) }.expect("failed to load library");
    let (init, release, handle_event) = unsafe {
        (
            *lib.get::<InitFn>(b"init").expect("missing export: init"),
            *lib.get::<ReleaseFn>(b"release").expect("missing export: release"),
            *lib.get::<HandleEventFn>(b"handleEvent").expect("missing export: handleEvent"),
        )
    };
    println!("loaded {path}: init/release/handleEvent exported");

    let table = SSkirmishAICallback::default();
    let ret = unsafe { init(7, &table) };
    println!("init with empty callback table -> {ret} (expect 101: panic contained)");
    assert_eq!(ret, 101);

    let frame: c_int = 0;
    let ret = unsafe { handle_event(7, 3, (&frame as *const c_int).cast()) };
    println!("handleEvent for unknown instance -> {ret} (expect 100)");
    assert_eq!(ret, 100);

    let ret = unsafe { release(7) };
    println!("release for unknown instance -> {ret} (expect 100)");
    assert_eq!(ret, 100);

    #[cfg(windows)]
    {
        let addr = init as *const c_void;
        lib.close().expect("failed to free library");
        let loaded = win::module_at(addr);
        println!("library freed -> {} (expect loaded: init pins it)", if loaded { "loaded" } else { "unloaded" });
        assert!(loaded);
    }
    println!("OK");
}

fn unload(probe: &str) {
    if !cfg!(windows) {
        println!("--unload checks how Windows frees libraries; skipped");
        return;
    }
    let marker = std::env::temp_dir().join(format!("unload-probe-{}", std::process::id()));
    let mut failed = false;
    for (mode, crash) in [("keep", false), ("spawn", true), ("init", false)] {
        let _ = std::fs::remove_file(&marker);
        let status = run_child(probe, mode, &marker);
        let finished = marker.exists();
        let ok = match status {
            Some(s) => s.success() != crash && finished != crash,
            None => false,
        };
        let exit = match status {
            Some(s) => s.code().map_or("killed by a signal".into(), |c| format!("exit {:#010x}", c as u32)),
            None => format!("hung (killed after {}s)", CHILD_TIMEOUT.as_secs()),
        };
        println!(
            "{mode:<5}: {exit}, thread {} (expect {}){}",
            if finished { "finished" } else { "did not finish" },
            if crash { "a crash, thread did not finish" } else { "exit 0, thread finished" },
            if ok { "" } else { "  <-- FAIL" }
        );
        failed |= !ok;
    }
    let _ = std::fs::remove_file(&marker);
    assert!(!failed, "unload test failed");
    println!("OK");
}

/// Run this program as `--unload-child`; `None` when it hangs.
fn run_child(probe: &str, mode: &str, marker: &Path) -> Option<ExitStatus> {
    let exe = std::env::current_exe().expect("cannot find this program");
    let mut child = Command::new(exe)
        .args(["--unload-child", probe, mode])
        .env("UNLOAD_PROBE_MARKER", marker)
        .spawn()
        .expect("cannot start child");
    let start = Instant::now();
    while start.elapsed() < CHILD_TIMEOUT {
        if let Some(status) = child.try_wait().expect("cannot wait for child") {
            return Some(status);
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let _ = child.kill();
    let _ = child.wait();
    None
}

fn unload_child(probe: &str, mode: &str) {
    #[cfg(windows)]
    win::quiet_crashes();
    let lib = unsafe { libloading::Library::new(probe) }.expect("failed to load probe");
    if mode == "init" {
        let (init, release) = unsafe {
            (
                *lib.get::<InitFn>(b"init").expect("missing export: init"),
                *lib.get::<ReleaseFn>(b"release").expect("missing export: release"),
            )
        };
        let table = SSkirmishAICallback::default();
        assert_eq!(unsafe { init(7, &table) }, 0);
        assert_eq!(unsafe { release(7) }, 0);
    } else {
        let spawn = unsafe { *lib.get::<SpawnFn>(b"unload_probe_spawn").expect("missing export: unload_probe_spawn") };
        unsafe { spawn() };
    }
    if mode == "keep" {
        std::mem::forget(lib);
    } else {
        lib.close().expect("failed to free probe");
    }
    std::thread::sleep(LINGER);
}

#[cfg(windows)]
mod win {
    use std::ffi::c_void;

    const GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT: u32 = 0x2;
    const GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS: u32 = 0x4;
    const SEM_FAILCRITICALERRORS: u32 = 0x1;
    const SEM_NOGPFAULTERRORBOX: u32 = 0x2;

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetModuleHandleExW(flags: u32, module_name: *const u16, module: *mut *mut c_void) -> i32;
        fn SetErrorMode(mode: u32) -> u32;
    }

    /// Whether `addr` lies in a loaded module.
    pub fn module_at(addr: *const c_void) -> bool {
        let mut module = std::ptr::null_mut();
        let flags = GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS | GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT;
        unsafe { GetModuleHandleExW(flags, addr.cast(), &mut module) != 0 }
    }

    /// No error dialog when this process crashes: here it is meant to.
    pub fn quiet_crashes() {
        unsafe { SetErrorMode(SEM_FAILCRITICALERRORS | SEM_NOGPFAULTERRORBOX) };
    }
}
