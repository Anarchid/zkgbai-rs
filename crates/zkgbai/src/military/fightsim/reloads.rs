//! Live engine cooldowns, independent of the optional scripted-tube ledger.
//! Query at snapshot time: never keep weapon handles across LOS changes.

use super::SnapUnit;
use crate::Ai;
use springai::Unit;
use std::hash::{Hash, Hasher};

/// Stable while a cooldown counts down; changes on firing, readiness or LOS
/// changes. A cached prediction must not retain an expired opening.
pub(crate) fn deadlines(reloads: &[(String, i32)], frame: i32) -> Vec<(&str, i32)> {
    reloads
        .iter()
        .map(|(name, left)| (name.as_str(), if *left > 0 { frame.saturating_add(*left) } else { 0 }))
        .collect()
}

impl SnapUnit {
    pub(crate) fn same_reload_window(&self, other: &Self, frame: i32, other_frame: i32) -> bool {
        deadlines(&self.reloads, frame) == deadlines(&other.reloads, other_frame)
    }

    pub(crate) fn hash_reloads(&self, frame: i32, hash: &mut impl Hasher) {
        deadlines(&self.reloads, frame).hash(hash);
    }

    pub(super) fn reload(&self, name: &str) -> Option<i32> {
        // The model combines duplicate weapon definitions. Use the first
        // mount able to fire, not whichever mount happened to be enumerated last.
        self.reloads.iter().filter(|(n, _)| n == name).map(|(_, f)| *f).min()
    }
}

impl Ai {
    pub(crate) fn reload_stamp(&self, units: &[i32]) -> u64 {
        let mut hash = std::collections::hash_map::DefaultHasher::new();
        for &id in units {
            id.hash(&mut hash);
            deadlines(&self.snap_reloads(id), self.frame).hash(&mut hash);
        }
        hash.finish()
    }

    /// Remaining engine reloads for allied units and enemies currently in LOS.
    /// Empty means unknown, not observed ready. No relationship to `ammo`'s
    /// feature switch: script magazines and engine cooldowns are separate.
    pub(crate) fn snap_reloads(&self, id: i32) -> Vec<(String, i32)> {
        if self.dev_off("reloads") {
            return Vec::new();
        }
        let Some(unit) = Unit::from_id(self.cb, id) else { return Vec::new() };
        observe_reloads(unit, self.frame)
    }
}

fn observe_reloads(unit: Unit, frame: i32) -> Vec<(String, i32)> {
    // Unit_getWeapons itself enforces allied-or-LOS access. The individual
    // reload getters do not, so enumerate afresh before querying any mount.
    unit.weapons()
        .into_iter()
        .filter_map(|w| {
            let def = w.def()?;
            let active = ((def.salvo_size().max(1) - 1) as f32 * def.salvo_delay()
                + if def.weapon_type() == "BeamLaser" { def.beam_time() } else { 0.0 })
                * 30.0;
            let remaining = cooldown(frame, w.reload_frame(), w.reload_time(), active.ceil() as i32)?;
            Some((def.name(), remaining))
        })
        .collect()
}

/// Reload starts before a burst finishes. During its active portion, keep an
/// immediate attack in the model rather than treating the ongoing burst as safe.
fn cooldown(now: i32, ready: i32, period: i32, active: i32) -> Option<i32> {
    if ready < 0 || period <= 0 {
        return None;
    }
    let left = ready.saturating_sub(now).max(0);
    let elapsed = period.saturating_sub(left);
    Some(if active > 0 && elapsed <= active { 0 } else { left })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loss_of_los_stops_reload_reads_immediately() {
        use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
        static VISIBLE: AtomicBool = AtomicBool::new(true);
        static READS: AtomicUsize = AtomicUsize::new(0);
        unsafe extern "C" fn weapons(_: i32, _: i32) -> i32 {
            i32::from(VISIBLE.load(Ordering::Relaxed))
        }
        unsafe extern "C" fn def(_: i32, _: i32, _: i32) -> i32 {
            1
        }
        unsafe extern "C" fn ready(_: i32, _: i32, _: i32) -> i32 {
            READS.fetch_add(1, Ordering::Relaxed);
            400
        }
        unsafe extern "C" fn period(_: i32, _: i32, _: i32) -> i32 {
            300
        }
        unsafe extern "C" fn salvo(_: i32, _: i32) -> i32 {
            1
        }
        unsafe extern "C" fn delay(_: i32, _: i32) -> f32 {
            0.0
        }
        unsafe extern "C" fn name(_: i32, _: i32) -> *const std::ffi::c_char {
            c"hoverskirm_missile".as_ptr()
        }
        unsafe extern "C" fn kind(_: i32, _: i32) -> *const std::ffi::c_char {
            c"MissileLauncher".as_ptr()
        }
        let table = springai::ffi::SSkirmishAICallback {
            Unit_getWeapons: Some(weapons),
            Unit_Weapon_getDef: Some(def),
            Unit_Weapon_getReloadFrame: Some(ready),
            Unit_Weapon_getReloadTime: Some(period),
            WeaponDef_getSalvoSize: Some(salvo),
            WeaponDef_getSalvoDelay: Some(delay),
            WeaponDef_getName: Some(name),
            WeaponDef_getType: Some(kind),
            ..Default::default()
        };
        // SAFETY: the callback table outlives all calls below.
        let cb = unsafe { springai::Callback::from_raw(0, &table) };
        let unit = Unit::from_id(cb, 1).unwrap();
        assert_eq!(observe_reloads(unit, 100), vec![("hoverskirm_missile".into(), 300)]);
        VISIBLE.store(false, Ordering::Relaxed);
        assert!(observe_reloads(unit, 101).is_empty());
        assert_eq!(READS.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn cached_windows_survive_countdown_but_not_firing_expiry_or_lost_los() {
        let old = SnapUnit { reloads: vec![("gun".into(), 30)], ..Default::default() };
        let mut now = old.clone();
        now.reloads[0].1 = 15;
        assert!(old.same_reload_window(&now, 100, 115));
        now.reloads[0].1 = 0;
        assert!(!old.same_reload_window(&now, 100, 130));
        now.reloads[0].1 = 60;
        assert!(!old.same_reload_window(&now, 100, 115));
        now.reloads.clear();
        assert!(!old.same_reload_window(&now, 100, 115));
    }

    #[test]
    fn observed_ready_is_distinct_from_unknown_and_bursts_stay_dangerous() {
        assert_eq!(cooldown(100, -1, 300, 0), None);
        assert_eq!(cooldown(100, 90, 300, 0), Some(0));
        assert_eq!(cooldown(100, 400, 300, 0), Some(300));
        assert_eq!(cooldown(100, 395, 300, 12), Some(0));
        assert_eq!(cooldown(110, 395, 300, 12), Some(285));
    }

    #[test]
    fn duplicate_mounts_use_earliest_reload_and_names_survive_reordering() {
        let unit = SnapUnit {
            reloads: vec![("gun".into(), 120), ("beam".into(), 0), ("gun".into(), 30)],
            ..Default::default()
        };
        assert_eq!(unit.reload("gun"), Some(30));
        assert_eq!(unit.reload("beam"), Some(0));
        assert_eq!(unit.reload("unknown"), None);
    }
}
