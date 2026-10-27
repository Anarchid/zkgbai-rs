//! Units reserved for an outside controller, and modules muted by one.
//!
//! While a unit is reserved, orders given to it are dropped (they return
//! `false` as if the engine refused them) unless they are issued inside
//! [`as_owner`]. An AI can thus hand some of its units to an operator without
//! teaching every module to keep its hands off them.
//!
//! An AI that runs its modules inside [`as_module`] can likewise have the
//! operator [`mute`] a module: the module keeps running (its bookkeeping, its
//! view of the game) but every order it issues is dropped. A mute names a
//! module by prefix: `raider` mutes `raider.update` and `raider.unitIdle`,
//! `war` mutes every handler under `war.`. Orders are counted per module
//! either way ([`order_stats`]), so an operator can see what each module
//! does before deciding to silence it.
//!
//! A unit can also be [owned](own) by one module of the AI itself: then
//! only that module (and the helpers it names) may order it, and orders
//! from any other module are dropped and counted as `foreign`. Two modules
//! that both think a unit is theirs thus cannot fight over it, and the
//! counts show where they would have.
//!
//! The engine calls all AIs of a process from its simulation thread, so the
//! sets are thread-local; entries are keyed by AI instance.

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, BTreeSet};

use crate::Callback;

/// Orders a module tried to issue, and how many of them were dropped
/// (the unit reserved, or the module muted, or the unit another module's:
/// those also count as `foreign`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct OrderStats {
    pub issued: u64,
    pub dropped: u64,
    pub foreign: u64,
}

/// The module a unit belongs to (by prefix, see [`as_module`]), and the
/// modules that may order it besides.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Owner {
    pub module: &'static str,
    pub helpers: &'static [&'static str],
}

impl Owner {
    /// Whether orders from `module` may reach the unit. Orders issued
    /// outside any module are let through (no handler to blame).
    fn admits(&self, module: &str) -> bool {
        module.is_empty() || covers(self.module, module) || self.helpers.iter().any(|h| covers(h, module))
    }
}

thread_local! {
    static RESERVED: RefCell<BTreeSet<(i32, i32)>> = const { RefCell::new(BTreeSet::new()) };
    static OWNER: Cell<bool> = const { Cell::new(false) };
    /// The module whose handler is running (see [`as_module`]).
    static MODULE: Cell<&'static str> = const { Cell::new("") };
    static MUTED: RefCell<BTreeSet<(i32, String)>> = const { RefCell::new(BTreeSet::new()) };
    static ORDERS: RefCell<BTreeMap<(i32, &'static str), OrderStats>> = const { RefCell::new(BTreeMap::new()) };
    static OWNED: RefCell<BTreeMap<(i32, i32), Owner>> = const { RefCell::new(BTreeMap::new()) };
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

/// Make unit `unit_id` of AI `cb` module `owner`'s alone; the previous
/// owner, if any.
pub fn own(cb: Callback, unit_id: i32, owner: Owner) -> Option<Owner> {
    OWNED.with_borrow_mut(|o| o.insert((cb.ai_id(), unit_id), owner))
}

/// Let any module order unit `unit_id` again; its owner, if it had one.
pub fn disown(cb: Callback, unit_id: i32) -> Option<Owner> {
    OWNED.with_borrow_mut(|o| o.remove(&(cb.ai_id(), unit_id)))
}

pub fn owner(cb: Callback, unit_id: i32) -> Option<Owner> {
    OWNED.with_borrow(|o| o.get(&(cb.ai_id(), unit_id)).copied())
}

/// Release every unit of AI `cb`, unmute its modules and forget their order
/// counts (on release of the AI).
pub fn clear(cb: Callback) {
    let ai = cb.ai_id();
    OWNED.with_borrow_mut(|o| o.retain(|&(a, _), _| a != ai));
    RESERVED.with_borrow_mut(|r| r.retain(|&(a, _)| a != ai));
    MUTED.with_borrow_mut(|m| m.retain(|(a, _)| *a != ai));
    ORDERS.with_borrow_mut(|o| o.retain(|(a, _), _| *a != ai));
}

/// Run `f` with orders to reserved units (and from muted modules) let
/// through.
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

/// Run `f` as module `name`: its orders count against `name`, and are
/// dropped while `name` (or a prefix of it, at a dot) is muted. Nested
/// calls see the innermost name. Unwinds restore the outer name.
pub fn as_module<R>(name: &'static str, f: impl FnOnce() -> R) -> R {
    struct Restore(&'static str);
    impl Drop for Restore {
        fn drop(&mut self) {
            MODULE.set(self.0);
        }
    }
    let _restore = Restore(MODULE.replace(name));
    f()
}

/// The module whose handler is running (`""` outside any).
pub fn current_module() -> &'static str {
    MODULE.get()
}

/// Whether `module` matches `prefix`: equal, or `prefix` followed by a dot.
fn covers(prefix: &str, module: &str) -> bool {
    !prefix.is_empty()
        && (module == prefix
            || (module.len() > prefix.len()
                && module.starts_with(prefix)
                && module.as_bytes()[prefix.len()] == b'.'))
}

/// Mute module `prefix` of AI `cb`; `false` if it already was.
pub fn mute(cb: Callback, prefix: &str) -> bool {
    MUTED.with_borrow_mut(|m| m.insert((cb.ai_id(), prefix.to_owned())))
}

/// Unmute module `prefix` of AI `cb`; `false` if it was not muted.
pub fn unmute(cb: Callback, prefix: &str) -> bool {
    MUTED.with_borrow_mut(|m| m.remove(&(cb.ai_id(), prefix.to_owned())))
}

/// The muted module prefixes of AI `cb`.
pub fn muted(cb: Callback) -> Vec<String> {
    let ai = cb.ai_id();
    MUTED.with_borrow(|m| m.iter().filter(|(a, _)| *a == ai).map(|(_, p)| p.clone()).collect())
}

/// Whether orders from module `module` of AI `cb` are dropped.
pub fn is_muted(cb: Callback, module: &str) -> bool {
    let ai = cb.ai_id();
    MUTED.with_borrow(|m| m.iter().any(|(a, p)| *a == ai && covers(p, module)))
}

/// Orders counted per module of AI `cb`, by module name.
pub fn order_stats(cb: Callback) -> Vec<(&'static str, OrderStats)> {
    let ai = cb.ai_id();
    ORDERS.with_borrow(|o| o.iter().filter(|((a, _), _)| *a == ai).map(|((_, m), s)| (*m, *s)).collect())
}

/// Whether an order to `unit_id` may go to the engine.
#[inline]
pub(crate) fn may_command(cb: &Callback, unit_id: i32) -> bool {
    if OWNER.get() {
        return true;
    }
    gate(cb.ai_id(), unit_id, MODULE.get())
}

/// [`may_command`] for AI `ai`, from `module`, counted.
fn gate(ai: i32, unit_id: i32, module: &'static str) -> bool {
    let reserved = RESERVED.with_borrow(|r| !r.is_empty() && r.contains(&(ai, unit_id)));
    let muted =
        !reserved && MUTED.with_borrow(|m| !m.is_empty() && m.iter().any(|(a, p)| *a == ai && covers(p, module)));
    let foreign = !reserved
        && !muted
        && OWNED.with_borrow(|o| o.get(&(ai, unit_id)).is_some_and(|owner| !owner.admits(module)));
    let dropped = reserved || muted || foreign;
    ORDERS.with_borrow_mut(|o| {
        let s = o.entry((ai, module)).or_default();
        s.issued += 1;
        s.dropped += u64::from(dropped);
        s.foreign += u64::from(foreign);
    });
    !dropped
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefixes_cover_at_dots() {
        assert!(covers("war", "war"));
        assert!(covers("war", "war.update"));
        assert!(covers("raider", "raider.update"));
        assert!(!covers("war", "warx.update"));
        assert!(!covers("war.update", "war"));
        assert!(!covers("", "war"));
        assert!(!covers("", ""));
    }

    #[test]
    fn owned_units_take_orders_from_their_module_and_helpers_only() {
        const AI: i32 = 7_001;
        const OWNER: Owner = Owner { module: "stakeouts", helpers: &["retreat", "war.unitFinished"] };
        OWNED.with_borrow_mut(|o| o.insert((AI, 5), OWNER));
        assert!(gate(AI, 5, "stakeouts.update"));
        assert!(gate(AI, 5, "stakeouts"));
        assert!(gate(AI, 5, "retreat.update"));
        assert!(gate(AI, 5, "war.unitFinished"));
        assert!(!gate(AI, 5, "war.unitIdle"));
        assert!(!gate(AI, 5, "raider.update"));
        assert!(!gate(AI, 5, "raider.update"));
        // Outside any module: let through.
        assert!(gate(AI, 5, ""));
        // Another unit, and the same unit of another AI, are anyone's.
        assert!(gate(AI, 6, "raider.update"));
        assert!(gate(AI + 1, 5, "raider.update"));
        let stats = |m: &'static str| ORDERS.with_borrow(|o| o.get(&(AI, m)).copied().unwrap_or_default());
        assert_eq!(stats("raider.update"), OrderStats { issued: 3, dropped: 2, foreign: 2 });
        assert_eq!(stats("stakeouts.update"), OrderStats { issued: 1, dropped: 0, foreign: 0 });
        // Disowned, it is anyone's again.
        OWNED.with_borrow_mut(|o| o.remove(&(AI, 5)));
        assert!(gate(AI, 5, "raider.update"));
    }

    #[test]
    fn reserved_beats_ownership_in_the_counts() {
        const AI: i32 = 7_002;
        OWNED.with_borrow_mut(|o| o.insert((AI, 9), Owner { module: "raider", helpers: &[] }));
        RESERVED.with_borrow_mut(|r| r.insert((AI, 9)));
        assert!(!gate(AI, 9, "squad.update"));
        let s = ORDERS.with_borrow(|o| o.get(&(AI, "squad.update")).copied().unwrap_or_default());
        assert_eq!(s, OrderStats { issued: 1, dropped: 1, foreign: 0 });
        RESERVED.with_borrow_mut(|r| r.remove(&(AI, 9)));
        OWNED.with_borrow_mut(|o| o.remove(&(AI, 9)));
    }

    #[test]
    fn module_name_is_scoped() {
        assert_eq!(current_module(), "");
        as_module("eco.update", || {
            assert_eq!(current_module(), "eco.update");
            as_module("eco.inner", || assert_eq!(current_module(), "eco.inner"));
            assert_eq!(current_module(), "eco.update");
        });
        assert_eq!(current_module(), "");
    }
}
