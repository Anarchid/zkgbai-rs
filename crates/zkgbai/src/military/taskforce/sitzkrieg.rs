//! Stable opposing concentrations, measured in local zones rather than whole
//! terrain regions (a large region can contain both sides of a front).
use crate::{
    Ai,
    util::{NULLPOS, distance},
};
use springai::Vec3;
use std::collections::BTreeMap;

const PERIOD: i32 = 30 * 5;
const HOLD: i32 = 30 * 90;
const CELL: f32 = 512.0;
const DENSITY: f32 = 2000.0; // metal / square kiloelmo
const MIN_VALUE: f32 = 1500.0;
const DRIFT: f32 = 128.0;
const FRONT: f32 = 1400.0;
type Cell = (i32, i32);
type Pair = (Cell, Cell);

#[derive(Clone, Copy, Default)]
struct Mass {
    value: f32,
    x: f32,
    z: f32,
}
impl Mass {
    fn add(&mut self, p: Vec3, value: f32) {
        self.value += value;
        self.x += p.x * value;
        self.z += p.z * value;
    }
    fn center(self) -> Vec3 {
        Vec3::xz(self.x / self.value, self.z / self.value)
    }
    fn dense(self) -> bool {
        self.value >= MIN_VALUE.max(DENSITY * CELL * CELL / 1_000_000.0)
    }
}
struct Held {
    ours: Vec3,
    theirs: Vec3,
    since: i32,
    last: i32,
}
#[derive(Default)]
pub struct Sitzkrieg {
    held: BTreeMap<Pair, Held>,
    pub unlocked: bool,
}
impl Sitzkrieg {
    /// Fixed anchors prevent slow, steady advances from accumulating hold time.
    pub fn observe(&mut self, frame: i32, ours: &[(Vec3, f32)], theirs: &[(Vec3, f32)]) -> bool {
        if self.unlocked {
            return false;
        }
        let mut zones: BTreeMap<Cell, [Mass; 2]> = BTreeMap::new();
        for (side, samples) in [ours, theirs].iter().enumerate() {
            for &(p, value) in *samples {
                if p == NULLPOS || !value.is_finite() || value <= 0.0 {
                    continue;
                }
                let cell = ((p.x / CELL).floor() as i32, (p.z / CELL).floor() as i32);
                zones.entry(cell).or_default()[side].add(p, value);
            }
        }
        let held_by = |m: &[Mass; 2], side: usize| m[side].dense() && m[side].value >= 1.5 * m[1 - side].value;
        for (&a, am) in zones.iter().filter(|(_, m)| held_by(m, 0)) {
            for (&b, bm) in zones.iter().filter(|(_, m)| held_by(m, 1)) {
                let (ours, theirs) = (am[0].center(), bm[1].center());
                if distance(ours, theirs) > FRONT {
                    continue;
                }
                let h = self.held.entry((a, b)).or_insert(Held { ours, theirs, since: frame, last: frame });
                if frame - h.last > PERIOD * 2 || distance(h.ours, ours) > DRIFT || distance(h.theirs, theirs) > DRIFT {
                    *h = Held { ours, theirs, since: frame, last: frame };
                }
                h.last = frame;
                if frame - h.since >= HOLD {
                    self.unlocked = true;
                }
            }
        }
        self.held.retain(|_, h| h.last == frame);
        self.unlocked
    }
}
impl Ai {
    pub(crate) fn sitzkrieg_update(&mut self, frame: i32) {
        if (frame + self.offset) % PERIOD != 0 || self.dev_off("siege") || self.taskforces.sitzkrieg.unlocked {
            return;
        }
        self.taskforce_load();
        let ours: Vec<_> = self
            .cb
            .friendly_units()
            .into_iter()
            .filter_map(|u| {
                let d = u.def()?;
                (!u.is_being_built() && u.health() > 0.0 && !d.is_able_to_fly() && d.max_weapon_range() > 0.0)
                    .then(|| (u.pos(), d.cost(self.metal)))
            })
            .collect();
        let theirs: Vec<_> = self
            .war
            .targets
            .values()
            .filter(|e| e.identified && !e.is_dead && !e.can_fly() && e.can_attack())
            .filter(|e| e.is_static || frame - e.last_seen <= 30 * 10)
            .map(|e| (e.position, e.ud.map_or(e.value, |d| d.cost(self.metal))))
            .collect();
        if self.taskforces.sitzkrieg.observe(frame, &ours, &theirs) {
            let mut names = Vec::new();
            for d in &mut self.taskforces.doctrines {
                if self.taskforces.automatic_siege.contains(&d.name) {
                    d.enabled = true;
                    names.push(d.name.clone());
                }
            }
            // Artillery already in service should benefit too, without waiting
            // for another factory completion. Only take the ordinary arty pool.
            let existing: Vec<_> = self
                .misc_handler
                .arties
                .iter()
                .filter_map(|id| self.fighters.get(id))
                .filter(|f| {
                    self.taskforces.doctrines.iter().any(|d| {
                        names.contains(&d.name) && d.is_core(self.defs.name_of(f.unit), &self.taskforces.classes)
                    })
                })
                .map(|f| f.unit)
                .collect();
            for u in existing {
                self.misc_remove_unit(u);
                if let Some(d) = u.def() {
                    self.squad_add_taskforce_member(crate::military::Fighter::new(u, d.cost(self.metal), self.team_id));
                }
            }
            self.log(&format!("sitzkrieg: opposing dense zones held for 90 s; unlocked {}", names.join(", ")));
            self.op_emit("sitzkrieg", serde_json::json!({"event":"unlocked", "hold_seconds":90, "doctrines":names}));
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn sample(x: f32, value: f32) -> [(Vec3, f32); 1] {
        [(Vec3::xz(x, 256.0), value)]
    }
    #[test]
    fn sustained_front_unlocks_once() {
        let mut s = Sitzkrieg::default();
        for f in (0..HOLD).step_by(PERIOD as usize) {
            assert!(!s.observe(f, &sample(256.0, 2000.0), &sample(1024.0, 2000.0)));
        }
        assert!(s.observe(HOLD, &sample(256.0, 2000.0), &sample(1024.0, 2000.0)));
        assert!(!s.observe(HOLD + PERIOD, &[], &[]));
        assert!(s.unlocked);
    }
    #[test]
    fn sparse_distant_and_interrupted_fronts_do_not_unlock() {
        for (x, value) in [(1024.0, 1400.0), (3072.0, 2000.0)] {
            let mut s = Sitzkrieg::default();
            for f in (0..HOLD * 2).step_by(PERIOD as usize) {
                assert!(!s.observe(f, &sample(256.0, 2000.0), &sample(x, value)));
            }
        }
        let mut s = Sitzkrieg::default();
        for f in (0..HOLD * 2).step_by(PERIOD as usize) {
            let enemy = if f == HOLD - PERIOD { vec![] } else { sample(1024.0, 2000.0).to_vec() };
            assert!(!s.observe(f, &sample(256.0, 2000.0), &enemy));
        }
    }
    #[test]
    fn contested_zones_do_not_count_as_opposing_held_zones() {
        let mut s = Sitzkrieg::default();
        let samples = [(Vec3::xz(256.0, 256.0), 2000.0), (Vec3::xz(1024.0, 256.0), 2000.0)];
        for f in (0..HOLD * 2).step_by(PERIOD as usize) {
            assert!(!s.observe(f, &samples, &samples));
        }
    }
    #[test]
    fn small_jitter_is_allowed_but_one_advancing_side_resets() {
        let mut s = Sitzkrieg::default();
        for f in (0..=HOLD).step_by(PERIOD as usize) {
            let jitter = if f % (PERIOD * 2) == 0 { 10.0 } else { -10.0 };
            assert_eq!(s.observe(f, &sample(256.0 + jitter, 2000.0), &sample(1100.0, 2000.0)), f == HOLD);
        }
        let mut s = Sitzkrieg::default();
        for f in (0..HOLD * 2).step_by(PERIOD as usize) {
            assert!(!s.observe(f, &sample(256.0, 2000.0), &sample(1024.0 + f as f32 / 10.0, 2000.0)));
        }
    }
    #[test]
    fn slow_drift_and_unobserved_time_reset_the_clock() {
        let mut s = Sitzkrieg::default();
        for f in (0..HOLD * 3).step_by(PERIOD as usize) {
            let dx = f as f32 / 30.0 * 3.0;
            assert!(!s.observe(f, &sample(256.0 + dx, 2000.0), &sample(1024.0 + dx, 2000.0)));
        }
        let mut s = Sitzkrieg::default();
        assert!(!s.observe(0, &sample(256.0, 2000.0), &sample(1024.0, 2000.0)));
        assert!(!s.observe(HOLD, &sample(256.0, 2000.0), &sample(1024.0, 2000.0)));
    }
}
