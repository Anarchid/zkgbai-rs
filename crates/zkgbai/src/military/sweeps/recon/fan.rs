//! Swift reconnaissance must cross together, not feed one plane at a time.
//! Muster well behind the front, then fly separate lanes through enemy land.
use super::*;

const RUN_UP: f32 = 1600.0;
const ASSEMBLY: i32 = 30 * 180;
const READY_RADIUS: f32 = 600.0;

#[derive(Clone, Copy)]
struct Lane {
    muster: Vec3,
    target: Vec3,
}

pub(super) struct Wave {
    pub target: Vec3,
    pub purpose: Purpose,
    requested: i32,
    first_member: Option<i32>,
    lanes: Vec<Lane>,
    members: BTreeMap<i32, (Unit, usize)>,
}

impl Wave {
    pub(super) fn claims(&self, id: i32) -> bool {
        self.members.contains_key(&id)
    }

    pub(super) fn forget(&mut self, id: i32) {
        self.members.remove(&id);
    }

    pub(super) fn holding_nuke(&self, target: Vec3, frame: i32) -> bool {
        distance(self.target, target) < SPREAD
            && !self.members.is_empty()
            && self.first_member.is_some_and(|f| frame - f < NUKE_WAIT)
    }

    pub(super) fn wanted(&self) -> usize {
        self.lanes.len()
    }
}

fn wave_size(income: f32) -> usize {
    5 + ((income - 100.0).max(0.0) / 20.0).floor().min(5.0) as usize
}

/// Clip the whole line, not its individual points: edge lanes stay distinct.
fn line(center: Vec3, side: Vec3, span: f32, count: usize, size: Vec3) -> Vec<Vec3> {
    let (mut lo, mut hi) = (f32::NEG_INFINITY, f32::INFINITY);
    for (p, d, limit) in [(center.x, side.x, size.x), (center.z, side.z, size.z)] {
        if d.abs() > 0.0001 {
            let (a, b) = ((128.0 - p) / d, (limit - 128.0 - p) / d);
            lo = lo.max(a.min(b));
            hi = hi.min(a.max(b));
        }
    }
    if lo > hi || count < 2 {
        return Vec::new();
    }
    let width = span.min(hi - lo);
    let start = (-width * 0.5).clamp(lo, hi - width);
    (0..count).map(|i| center + side * (start + width * i as f32 / (count - 1) as f32)).collect()
}

fn may_depart(count: usize, wanted: usize, ready: bool, age: i32) -> bool {
    ready && count >= 5 && (count == wanted || age >= ASSEMBLY)
}

impl Ai {
    pub(crate) fn sweeps_wants_swift(&self) -> bool {
        !self.dev_off("scouts")
            && self.sweeps.recon.wave.as_ref().is_some_and(|wave| self.scouts_air_needed(SWIFT, wave.wanted()))
    }

    fn sweeps_plan_fan(&self, target: Vec3, count: usize) -> Vec<Lane> {
        let home = self.graph.get_ally_center();
        let length = distance(home, target);
        if length < RUN_UP {
            return Vec::new();
        }
        let forward = Vec3::xz((target.x - home.x) / length, (target.z - home.z) / length);
        let side = Vec3::xz(-forward.z, forward.x);
        let mut fronts: Vec<_> = self
            .graph
            .get_front_line_spots()
            .iter()
            .map(|s| (s.position.x - home.x) * forward.x + (s.position.z - home.z) * forward.z)
            .filter(|&d| d > 0.0 && d < length)
            .collect();
        fronts.sort_by(f32::total_cmp);
        let front = fronts.get(fronts.len() / 2).copied().unwrap_or(length * 0.5);
        let origin = home + forward * (front - RUN_UP).max(0.0);
        let map = self.cb.map();
        let size = Vec3::xz(map.width() as f32 * 8.0, map.height() as f32 * 8.0);
        let muster = line(origin, side, (count - 1) as f32 * 600.0, count, size);
        let goals = line(target, side, (count - 1) as f32 * 800.0, count, size);
        muster
            .into_iter()
            .zip(goals)
            .filter_map(|(mut muster, target)| {
                // Pull exposed wings back too. Leave a clear initial run-up, not
                // merely a safe point on the very edge of an AA circle.
                for _ in 0..32 {
                    if muster.x < 128.0 || muster.z < 128.0 || muster.x > size.x - 128.0 || muster.z > size.z - 128.0 {
                        return None;
                    }
                    let run = target - muster;
                    let direction = run * (1.0 / distance(muster, target).max(1.0));
                    if (0..=16).all(|i| !self.sweeps_dangerous(muster + direction * (i as f32 * 128.0))) {
                        return Some(Lane { muster, target });
                    }
                    muster = muster - forward * STEP;
                }
                None
            })
            .collect()
    }

    pub(super) fn sweeps_request_fan(&mut self, target: Vec3, purpose: Purpose) {
        // A broad sweep already in flight should finish before another is bought.
        if self.sweeps.recon.wave.is_some()
            || self.sweeps.recon.active.values().any(|d| d.swift)
            || self
                .sweeps
                .recon
                .last_fan
                .is_some_and(|f| self.frame - f < if purpose == Purpose::Nuke { RETRY_NUKE } else { STALE })
        {
            return;
        }
        let lanes = self.sweeps_plan_fan(target, wave_size(self.eco.effective_income));
        if lanes.len() < 5 {
            return;
        }
        self.log(&format!(
            "sweeps: assembling {:?} fan of {} Swifts; muster run-up {:.0} behind front",
            purpose,
            lanes.len(),
            RUN_UP
        ));
        self.sweeps.recon.wave =
            Some(Wave { target, purpose, requested: self.frame, first_member: None, lanes, members: BTreeMap::new() });
    }

    pub(super) fn sweeps_update_fan(&mut self) {
        let Some(mut wave) = self.sweeps.recon.wave.take() else { return };
        wave.members.retain(|&id, (u, _)| {
            u.health() > 0.0 && u.team() == self.team_id && !springai::reserved::is_reserved(self.cb, id)
        });
        // Hold the commandeered planes out of the ordinary Swift scout loop.
        if !self.dev_off("scouts") {
            let mut available: Vec<_> = self
                .misc_recon_swifts()
                .into_iter()
                .filter(|&u| {
                    !wave.claims(u.id())
                        && self.sweeps_available(u, wave.purpose)
                        && u.rules_param_float("specialReloadRemaining", 0.0) <= 0.0
                })
                .collect();
            for slot in 0..wave.lanes.len() {
                if wave.members.values().any(|&(_, i)| i == slot) {
                    continue;
                }
                let lane = wave.lanes[slot];
                available.sort_by(|a, b| distance(b.pos(), lane.muster).total_cmp(&distance(a.pos(), lane.muster)));
                let Some(u) = available.pop() else { break };
                self.retreat_remove_unit(u);
                u.set_fire_state(0, 0);
                u.set_idle_mode(1, 0);
                wave.first_member.get_or_insert(self.frame);
                kgb_move_to(u, lane.muster, 0, self.frame);
                wave.members.insert(u.id(), (u, slot));
                self.log(&format!(
                    "sweeps: Swift {} mustering lane {} at ({:.0}, {:.0}) for ({:.0}, {:.0})",
                    u.id(),
                    slot,
                    lane.muster.x,
                    lane.muster.z,
                    lane.target.x,
                    lane.target.z
                ));
            }
        }
        let age = self.frame - wave.requested;
        let exposed = wave.lanes.iter().any(|lane| self.sweeps_dangerous(lane.muster));
        let ready = wave.members.values().all(|(u, slot)| distance(u.pos(), wave.lanes[*slot].muster) <= READY_RADIUS);
        let depart = !self.dev_off("scouts") && !exposed && may_depart(wave.members.len(), wave.wanted(), ready, age);
        if depart {
            self.log(&format!("sweeps: {:?} fan departing with {} Swifts", wave.purpose, wave.members.len()));
            self.sweeps.recon.last_fan = Some(self.frame);
            self.sweeps.recon.attempts.push(Attempt {
                target: wave.target,
                started: self.frame,
                purpose: wave.purpose,
            });
            for (id, (unit, slot)) in wave.members {
                unit.set_idle_mode(0, 0);
                let target = wave.lanes[slot].target;
                let attempt = Attempt { target, started: self.frame, purpose: wave.purpose };
                self.sweeps
                    .recon
                    .active
                    .insert(id, Dive { unit, attempt, swift: true, boosted: false, burst_range: 0.0 });
                kgb_move_to(unit, target, 0, self.frame);
            }
        } else if self.dev_off("scouts") || exposed || age >= ASSEMBLY * 2 {
            self.log(&format!(
                "sweeps: fan cancelled after {}s; {} of {} Swifts mustered; exposed={exposed}",
                age / 30,
                wave.members.len(),
                wave.wanted()
            ));
            for (_, (u, _)) in wave.members {
                u.set_fire_state(2, 0);
                u.set_idle_mode(0, 0);
                self.retreat_add_coward(u);
                kgb_move_to(u, self.graph.get_ally_center(), 0, self.frame);
            }
        } else {
            self.sweeps.recon.wave = Some(wave);
        }
    }
}

impl Ai {
    /// Engine fixture: `test=recon-fan5` or `recon-fan10` against `aafort`.
    /// Only the aircraft and request are injected; muster, orders and boost
    /// are the production controller. Track real velocity and ability state.
    pub(crate) fn sweeps_fan_scenario(&mut self, frame: i32) {
        let count = match self.test.as_deref() {
            Some("recon-fan5") => 5,
            Some("recon-fan10") => 10,
            _ => return,
        };
        let home = self.graph.get_ally_center();
        let map = self.cb.map();
        let target = Vec3::xz(map.width() as f32 * 8.0 - home.x, map.height() as f32 * 8.0 - home.z);
        if frame == 60 {
            self.cb.cheats_set_enabled(true);
            if let Some(def) = self.defs.get(SWIFT) {
                for i in 0..count {
                    self.cb
                        .cheat_new_unit(def, Vec3::xz(home.x + (i % 5) as f32 * 80.0, home.z + (i / 5) as f32 * 80.0));
                }
            }
            self.cb.cheats_set_enabled(false);
        }
        if frame == 150 {
            let lanes = self.sweeps_plan_fan(target, count);
            self.log(&format!("recon-fan-test: request {} lanes", lanes.len()));
            self.sweeps.recon.wave = Some(Wave {
                target,
                purpose: Purpose::Economy,
                requested: frame,
                first_member: None,
                lanes,
                members: BTreeMap::new(),
            });
        }
        if frame <= 3600 && frame % 15 == 0 {
            for u in self.misc_recon_swifts() {
                let p = u.pos();
                self.log(&format!(
                    "recon-fan-track: f={frame} id={} x={:.0} z={:.0} speed={:.0} reload={:.3} claimed={} aa={}",
                    u.id(),
                    p.x,
                    p.z,
                    u.vel().x.hypot(u.vel().z) * 30.0,
                    u.rules_param_float("specialReloadRemaining", 0.0),
                    self.sweeps_claims(u.id()),
                    self.sweeps_dangerous(p)
                ));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rich_waves_need_five_to_ten_planes_together() {
        assert_eq!(wave_size(50.0), 5);
        assert_eq!(wave_size(125.0), 6);
        assert_eq!(wave_size(200.0), 10);
        assert_eq!(wave_size(1000.0), 10);
        assert!(!may_depart(1, 6, true, ASSEMBLY * 2));
        assert!(!may_depart(5, 6, true, ASSEMBLY - 1));
        assert!(!may_depart(6, 6, false, 0));
        assert!(may_depart(6, 6, true, 0));
        assert!(may_depart(5, 6, true, ASSEMBLY));
    }

    #[test]
    fn fan_clips_as_a_line_without_piling_up_at_map_edges() {
        let size = Vec3::xz(8192.0, 6144.0);
        for center in [Vec3::xz(7288.0, 5555.0), Vec3::xz(900.0, 650.0)] {
            let points = line(center, Vec3::xz(-0.6, 0.8), 7200.0, 10, size);
            assert_eq!(points.len(), 10);
            for p in &points {
                assert!(p.x >= 127.9 && p.x <= size.x - 127.9 && p.z >= 127.9 && p.z <= size.z - 127.9);
            }
            for pair in points.windows(2) {
                assert!(distance(pair[0], pair[1]) > 100.0);
            }
        }
    }
}
