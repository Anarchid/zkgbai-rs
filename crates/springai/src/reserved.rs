//! Units reserved for an outside controller.
//!
//! While a unit is reserved, orders given to it are dropped (they return
//! `false` as if the engine refused them) unless they are issued inside
//! [`as_owner`]. An AI can thus hand some of its units to an operator without
//! teaching every module to keep its hands off them.
//!
//! The engine calls all AIs of a process from its simulation thread, so the
//! set is thread-local; entries are keyed by AI instance.

use std::cell::{Cell, RefCell};
use std::collections::BTreeSet;

use crate::Callback;

thread_local! {
    static RESERVED: RefCell<BTreeSet<(i32, i32)>> = const { RefCell::new(BTreeSet::new()) };
    static OWNER: Cell<bool> = const { Cell::new(false) };
}

/// Reserve unit `unit_id` of AI `cb`; `false` if it already was.
pub fn reserve(cb: Callback, unit_id: i32) -> bool {
    RESERVED.with_borrow_mut(|r| r.insert((cb.ai_id(), unit_id)))
}

/// Give unit `unit_id` back to the AI; `false` if it was not reserved.
pub fn release(cb: Callback, unit_id: i32) -> bool {
    RESERVED.with_borrow_mut(|r| r.remove(&(cb.ai_id(), unit_id)))
}

pub fn is_reserved(cb: Callback, unit_id: i32) -> bool {
    RESERVED.with_borrow(|r| r.contains(&(cb.ai_id(), unit_id)))
}

/// All units reserved from AI `cb`.
pub fn reserved(cb: Callback) -> Vec<i32> {
    let ai = cb.ai_id();
    RESERVED.with_borrow(|r| r.range((ai, i32::MIN)..=(ai, i32::MAX)).map(|&(_, u)| u).collect())
}

/// Release every unit of AI `cb` (on release of the AI).
pub fn clear(cb: Callback) {
    let ai = cb.ai_id();
    RESERVED.with_borrow_mut(|r| r.retain(|&(a, _)| a != ai));
}

/// Run `f` with orders to reserved units let through.
pub fn as_owner<R>(f: impl FnOnce() -> R) -> R {
    struct Restore(bool);
    impl Drop for Restore {
        fn drop(&mut self) {
            OWNER.set(self.0);
        }
    }
    let _restore = Restore(OWNER.replace(true));
    f()
}

/// Whether an order to `unit_id` may go to the engine.
#[inline]
pub(crate) fn may_command(cb: &Callback, unit_id: i32) -> bool {
    OWNER.get() || RESERVED.with_borrow(|r| r.is_empty() || !r.contains(&(cb.ai_id(), unit_id)))
}
