//! Loads a SkirmishAI library and drives its entry points with an empty
//! callback table: every engine call the AI makes then panics inside the
//! AI. The library must contain that panic and report an error code
//! instead of unwinding into (and crashing) the host.
//!
//! usage: dll-smoke <path to libSkirmishAI.so / SkirmishAI.dll>

use std::ffi::{c_int, c_void};

use springai_sys::SSkirmishAICallback;

type InitFn = unsafe extern "C" fn(c_int, *const SSkirmishAICallback) -> c_int;
type ReleaseFn = unsafe extern "C" fn(c_int) -> c_int;
type HandleEventFn = unsafe extern "C" fn(c_int, c_int, *const c_void) -> c_int;

fn main() {
    let path = std::env::args().nth(1).expect("usage: dll-smoke <library>");
    let lib = unsafe { libloading::Library::new(&path) }.expect("failed to load library");
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
    println!("OK");
}
