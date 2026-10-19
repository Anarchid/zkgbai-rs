//! Glue between the engine's C entry points and a Rust [`SkirmishAi`].
//!
//! Guarantees:
//! - **No unwinding into the engine.** Every call into AI code runs under
//!   `catch_unwind`; a panic is logged to the infolog and turned into an
//!   error code (which the engine only warns about, except for `EVENT_INIT`).
//! - **Several instances per process.** The engine loads one library for all
//!   AI slots using it; instances are keyed by `skirmishAIId`.
//! - **Re-entrancy safety.** If the engine delivers an event while the same
//!   instance is still handling another one, the nested event is queued and
//!   delivered right after the outer handler returns, instead of aliasing
//!   `&mut` state.

use std::any::Any;
use std::cell::RefCell;
use std::collections::{BTreeMap, VecDeque};
use std::ffi::c_void;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::{Arc, Mutex, Once, TryLockError};

use springai_sys::SSkirmishAICallback;

use crate::{Callback, Event};

/// A Rust Skirmish AI.
pub trait SkirmishAi: Send + 'static {
    /// Create the AI instance. Called from the engine's `init()` entry point,
    /// before `Event::Init` is delivered.
    fn new(cb: Callback) -> Self
    where
        Self: Sized;

    /// Handle one engine event. Return `0` for success; any other value is
    /// reported by the engine (and is fatal for `Event::Init`).
    fn handle_event(&mut self, event: Event) -> i32;
}

/// Error codes returned to the engine.
pub mod error {
    /// `handleEvent` called for an id that was never initialised.
    pub const UNKNOWN_INSTANCE: i32 = 100;
    /// A panic escaped AI code.
    pub const PANIC: i32 = 101;
}

struct Slot<T> {
    cb: Callback,
    ai: Mutex<T>,
    pending: Mutex<VecDeque<Event>>,
}

/// Per-library table of live AI instances. Created by [`export_ai!`].
pub struct Registry<T> {
    slots: Mutex<BTreeMap<i32, Arc<Slot<T>>>>,
}

impl<T> Registry<T> {
    pub const fn new() -> Self {
        Self { slots: Mutex::new(BTreeMap::new()) }
    }

    fn get(&self, id: i32) -> Option<Arc<Slot<T>>> {
        self.slots.lock().unwrap_or_else(|e| e.into_inner()).get(&id).cloned()
    }
}

impl<T> Default for Registry<T> {
    fn default() -> Self {
        Self::new()
    }
}

thread_local! {
    static LAST_PANIC: RefCell<Option<String>> = const { RefCell::new(None) };
}

/// Install (once per process) a panic hook that records the panic message and
/// location for the catching site to log through the engine, instead of
/// printing to stderr where nobody sees it.
fn install_panic_hook() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            let msg = payload_str(info.payload());
            let loc = info.location().map(|l| format!(" at {}:{}", l.file(), l.line())).unwrap_or_default();
            let recorded = LAST_PANIC.try_with(|slot| {
                *slot.borrow_mut() = Some(format!("{msg}{loc}"));
            });
            if recorded.is_err() {
                previous(info);
            }
        }));
    });
}

/// Take the message of the most recent panic on this thread, if the panic
/// hook recorded one. For AIs that catch panics themselves (e.g. to isolate
/// sub-modules) and want to log what happened.
pub fn take_last_panic() -> Option<String> {
    LAST_PANIC.try_with(|s| s.borrow_mut().take()).ok().flatten()
}

fn payload_str(p: &(dyn Any + Send)) -> String {
    if let Some(s) = p.downcast_ref::<&str>() {
        (*s).to_owned()
    } else if let Some(s) = p.downcast_ref::<String>() {
        s.clone()
    } else {
        "non-string panic payload".to_owned()
    }
}

/// Run `f`, converting a panic into a logged error.
fn guarded<R>(cb: Option<Callback>, what: &str, f: impl FnOnce() -> R) -> Result<R, ()> {
    catch_unwind(AssertUnwindSafe(f)).map_err(|payload| {
        let detail =
            LAST_PANIC.try_with(|s| s.borrow_mut().take()).ok().flatten().unwrap_or_else(|| payload_str(&*payload));
        if let Some(cb) = cb {
            // Logging must not itself unwind into the engine.
            let _ = catch_unwind(AssertUnwindSafe(|| cb.log(&format!("[rust-ai] panic in {what}: {detail}"))));
        }
    })
}

/// Implementation of the `init` export.
///
/// # Safety
/// Must only be called by the engine with a valid callback table.
pub unsafe fn init<T: SkirmishAi>(reg: &Registry<T>, ai_id: i32, raw: *const SSkirmishAICallback) -> i32 {
    install_panic_hook();
    if raw.is_null() {
        return error::UNKNOWN_INSTANCE;
    }
    let cb = unsafe { Callback::from_raw(ai_id, raw) };
    match guarded(Some(cb), "init", || T::new(cb)) {
        Ok(ai) => {
            let slot = Arc::new(Slot { cb, ai: Mutex::new(ai), pending: Mutex::new(VecDeque::new()) });
            reg.slots.lock().unwrap_or_else(|e| e.into_inner()).insert(ai_id, slot);
            0
        }
        Err(()) => error::PANIC,
    }
}

/// Implementation of the `release` export.
pub fn release<T: SkirmishAi>(reg: &Registry<T>, ai_id: i32) -> i32 {
    let slot = reg.slots.lock().unwrap_or_else(|e| e.into_inner()).remove(&ai_id);
    match slot {
        Some(slot) => {
            let cb = slot.cb;
            // Dropping the AI runs user destructors; keep them contained too.
            match guarded(Some(cb), "release", move || drop(slot)) {
                Ok(()) => 0,
                Err(()) => error::PANIC,
            }
        }
        None => error::UNKNOWN_INSTANCE,
    }
}

/// Implementation of the `handleEvent` export.
///
/// # Safety
/// `data` must be the event struct the engine passed for `topic`.
pub unsafe fn handle_event<T: SkirmishAi>(reg: &Registry<T>, ai_id: i32, topic: i32, data: *const c_void) -> i32 {
    let Some(slot) = reg.get(ai_id) else {
        return error::UNKNOWN_INSTANCE;
    };
    let cb = slot.cb;
    let Ok(event) = guarded(Some(cb), "event decoding", || unsafe { Event::decode(cb, topic, data) }) else {
        return error::PANIC;
    };

    let mut ai = match slot.ai.try_lock() {
        Ok(guard) => guard,
        Err(TryLockError::Poisoned(p)) => p.into_inner(),
        Err(TryLockError::WouldBlock) => {
            // Nested delivery while this instance is busy: defer it.
            slot.pending.lock().unwrap_or_else(|e| e.into_inner()).push_back(event);
            return 0;
        }
    };

    let name = event.name();
    let mut ret = guarded(Some(cb), name, || ai.handle_event(event)).unwrap_or(error::PANIC);
    loop {
        let next = slot.pending.lock().unwrap_or_else(|e| e.into_inner()).pop_front();
        let Some(next) = next else { break };
        let name = next.name();
        let r = guarded(Some(cb), name, || ai.handle_event(next)).unwrap_or(error::PANIC);
        if ret == 0 {
            ret = r;
        }
    }
    ret
}

/// Export the engine entry points (`init`, `release`, `handleEvent`) for an
/// AI type. Use exactly once, in the `cdylib` crate.
///
/// ```ignore
/// springai::export_ai!(my_ai::MyAi);
/// ```
#[macro_export]
macro_rules! export_ai {
    ($ty:ty) => {
        static __SPRINGAI_REGISTRY: $crate::export::Registry<$ty> = $crate::export::Registry::new();

        /// Engine entry point: create the AI instance for `skirmish_ai_id`.
        ///
        /// # Safety
        /// Called by the engine only.
        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn init(
            skirmish_ai_id: ::std::ffi::c_int,
            callback: *const $crate::ffi::SSkirmishAICallback,
        ) -> ::std::ffi::c_int {
            unsafe { $crate::export::init::<$ty>(&__SPRINGAI_REGISTRY, skirmish_ai_id, callback) }
        }

        /// Engine entry point: destroy the AI instance for `skirmish_ai_id`.
        #[unsafe(no_mangle)]
        pub extern "C" fn release(skirmish_ai_id: ::std::ffi::c_int) -> ::std::ffi::c_int {
            $crate::export::release::<$ty>(&__SPRINGAI_REGISTRY, skirmish_ai_id)
        }

        /// Engine entry point: deliver one event.
        ///
        /// # Safety
        /// Called by the engine only.
        #[unsafe(no_mangle)]
        #[allow(non_snake_case)]
        pub unsafe extern "C" fn handleEvent(
            skirmish_ai_id: ::std::ffi::c_int,
            topic: ::std::ffi::c_int,
            data: *const ::std::ffi::c_void,
        ) -> ::std::ffi::c_int {
            unsafe { $crate::export::handle_event::<$ty>(&__SPRINGAI_REGISTRY, skirmish_ai_id, topic, data) }
        }
    };
}

#[cfg(test)]
mod tests {
    use std::ffi::{c_char, c_int};
    use std::sync::Mutex as StdMutex;

    use springai_sys::EventTopic;

    use super::*;

    static LOG: StdMutex<Vec<String>> = StdMutex::new(Vec::new());

    unsafe extern "C" fn log_stub(_ai: c_int, msg: *const c_char) {
        let s = unsafe { std::ffi::CStr::from_ptr(msg) }.to_string_lossy().into_owned();
        LOG.lock().unwrap().push(s);
    }

    /// A callback table whose only working function is `Log_log`.
    fn table() -> &'static SSkirmishAICallback {
        Box::leak(Box::new(SSkirmishAICallback { Log_log: Some(log_stub), ..Default::default() }))
    }

    fn update(reg: &Registry<impl SkirmishAi>, ai: i32, frame: c_int) -> i32 {
        let evt = springai_sys::SUpdateEvent { frame };
        unsafe {
            handle_event(reg, ai, EventTopic::EVENT_UPDATE as i32, (&evt as *const springai_sys::SUpdateEvent).cast())
        }
    }

    /// Records update frames; panics on frame 13; on frame 1 re-enters the
    /// library with frame 2, as an engine callback might.
    struct Probe {
        cb: Callback,
        frames: Vec<i32>,
    }

    static PROBES: Registry<Probe> = Registry::new();
    static SEEN: StdMutex<Vec<(i32, i32)>> = StdMutex::new(Vec::new());

    impl SkirmishAi for Probe {
        fn new(cb: Callback) -> Self {
            Probe { cb, frames: Vec::new() }
        }

        fn handle_event(&mut self, event: Event) -> i32 {
            if let Event::Update { frame } = event {
                assert_ne!(frame, 13, "unlucky frame");
                self.frames.push(frame);
                SEEN.lock().unwrap().push((self.cb.ai_id(), frame));
                if frame == 1 {
                    // Nested delivery must be deferred, not run re-entrantly.
                    assert_eq!(update(&PROBES, self.cb.ai_id(), 2), 0);
                    assert_eq!(self.frames, vec![1], "nested event ran inside the outer handler");
                }
            }
            0
        }
    }

    #[test]
    fn panics_are_contained_and_reentrant_events_are_queued() {
        let t = table();
        assert_eq!(unsafe { init::<Probe>(&PROBES, 3, t) }, 0);
        assert_eq!(unsafe { init::<Probe>(&PROBES, 4, t) }, 0);

        assert_eq!(update(&PROBES, 3, 1), 0);
        assert_eq!(update(&PROBES, 3, 13), error::PANIC);
        assert!(LOG.lock().unwrap().iter().any(|l| l.contains("panic in update") && l.contains("unlucky frame")));
        // The instance keeps working after a panic.
        assert_eq!(update(&PROBES, 3, 14), 0);
        assert_eq!(update(&PROBES, 4, 1), 0);
        // Unknown instances are reported, not crashed on.
        assert_eq!(update(&PROBES, 99, 1), error::UNKNOWN_INSTANCE);

        let seen: Vec<_> = SEEN.lock().unwrap().clone();
        assert_eq!(seen, vec![(3, 1), (3, 2), (3, 14), (4, 1), (4, 2)]);

        assert_eq!(release::<Probe>(&PROBES, 3), 0);
        assert_eq!(update(&PROBES, 3, 15), error::UNKNOWN_INSTANCE);
        assert_eq!(release::<Probe>(&PROBES, 3), error::UNKNOWN_INSTANCE);
        assert_eq!(release::<Probe>(&PROBES, 4), 0);
    }

    struct FailsToStart;

    static FAILING: Registry<FailsToStart> = Registry::new();

    impl SkirmishAi for FailsToStart {
        fn new(cb: Callback) -> Self {
            // Any engine call missing from the table panics.
            let _ = cb.game().my_team();
            FailsToStart
        }

        fn handle_event(&mut self, _event: Event) -> i32 {
            0
        }
    }

    #[test]
    fn panic_during_construction_fails_init() {
        assert_eq!(unsafe { init::<FailsToStart>(&FAILING, 1, table()) }, error::PANIC);
        assert_eq!(update(&FAILING, 1, 0), error::UNKNOWN_INSTANCE);
        assert!(LOG.lock().unwrap().iter().any(|l| l.contains("Game_getMyTeam is missing")));
    }
}
