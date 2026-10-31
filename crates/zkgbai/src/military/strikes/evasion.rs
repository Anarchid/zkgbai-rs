//! Waiting waves retain ownership while dodging; muster orders must not erase
//! an evasive move on the next tick, nor send recruits back into recent impacts.
use super::*;

const EVADE_FOR: i32 = 30 * 3;
const HIT_MEMORY: i32 = 30 * 15;
const HIT_RADIUS: f32 = 320.0;

pub(super) fn coverage(point: Vec3, source: Vec3, reach: f32) -> f32 {
    ((reach - distance(point, source)) / reach.max(1.0)).max(0.0)
}

impl Ai {
    /// Absolute enemy fire coverage, independent of friendly threat. Include
    /// inference even when no enemy has ever entered LOS or radar.
    pub(super) fn strike_waiting_risk(&self, p: Vec3, padding: f32) -> f32 {
        let known: f32 = self
            .war
            .targets
            .values()
            .filter(|e| {
                !e.is_dead
                    && e.identified
                    && e.can_attack()
                    && !e.is_aa
                    && (e.is_static || self.frame - e.last_seen < 30 * 30)
            })
            .map(|e| coverage(p, e.position, e.def_max_weapon_range() + padding + 100.0))
            .sum();
        let inferred: f32 = self
            .counterbattery_fixes(FIRE_MEMORY)
            .filter(|f| f.ground)
            .map(|f| coverage(p, f.pos, f.range + f.uncertainty + padding))
            .sum();
        let hits: f32 = self
            .strikes
            .hits
            .iter()
            .filter(|(_, at)| self.frame - *at < HIT_MEMORY)
            .map(|(at, _)| 4.0 * coverage(p, *at, HIT_RADIUS + padding))
            .sum();
        known + inferred + hits + if self.missiles_ground_burning(p) { 10.0 } else { 0.0 }
    }

    pub(crate) fn strikes_unit_damaged(&mut self, u: Unit, attacker: Option<Unit>, dir: Vec3) {
        self.lobster_strike_damaged(u.id());
        let Some(fire) = self
            .strikes
            .wave
            .as_ref()
            .filter(|w| matches!(w.phase, Phase::Forming | Phase::Screening) && w.members.contains(&u.id()))
            .map(|w| w.tactic.fire_state())
            .or_else(|| self.squad_strike_waiting_fire(u.id()))
        else {
            return;
        };

        let p = u.pos();
        self.strikes.hits.retain(|(_, at)| self.frame - *at < HIT_MEMORY);
        if let Some(hit) = self.strikes.hits.iter_mut().find(|(at, _)| distance(*at, p) < 100.0) {
            *hit = (p, self.frame);
        } else {
            if self.strikes.hits.len() >= 64 {
                self.strikes.hits.remove(0);
            }
            self.strikes.hits.push((p, self.frame));
        }
        // Even tiny, continuous damage keeps the site unsafe, without flooding
        // the engine with replacement orders on every Tremor fragment.
        if self.strikes.evasion.get(&u.id()).is_some_and(|(_, until)| *until - self.frame > EVADE_FOR - 10) {
            return;
        }
        let source = attacker.map(|a| a.pos()).filter(|p| *p != NULLPOS);
        let away = source.map_or(Vec3::xz(-dir.x, -dir.z), |a| Vec3::xz(p.x - a.x, p.z - a.z));
        let len = away.x.hypot(away.z);
        let away = if len.is_finite() && len > 0.01 {
            Vec3::xz(away.x / len, away.z / len)
        } else {
            let ally = self.graph.get_ally_center();
            let l = distance(ally, p).max(1.0);
            Vec3::xz((ally.x - p.x) / l, (ally.z - p.z) / l)
        };
        let Some(def) = u.def() else { return };
        let map = self.cb.map();
        let mut candidates = Vec::new();
        for back in [300.0, 500.0] {
            for side in [-300.0, 0.0, 300.0] {
                let mut goal = Vec3::xz(p.x + away.x * back - away.z * side, p.z + away.z * back + away.x * side);
                goal.x = goal.x.clamp(1.0, (map.width() * 8 - 1).max(1) as f32);
                goal.z = goal.z.clamp(1.0, (map.height() * 8 - 1).max(1) as f32);
                goal.y = map.elevation_at(goal.x, goal.z);
                if reaches(self.reach_terrain(), def, p, goal, 40.0) {
                    candidates.push(goal);
                }
            }
        }
        let Some(goal) = candidates
            .into_iter()
            .min_by(|a, b| self.strike_waiting_risk(*a, 40.0).total_cmp(&self.strike_waiting_risk(*b, 40.0)))
        else {
            return;
        };
        if self.test.as_deref() == Some("assassin-battery-observer") {
            self.log(&format!(
                "battery-test: incomplete wave unit {} evades from {:.0},{:.0} to {:.0},{:.0}",
                u.id(),
                p.x,
                p.z,
                goal.x,
                goal.z
            ));
        }
        self.strikes.evasion.insert(u.id(), (goal, self.frame + EVADE_FOR));
        self.squad_strike_evade(u.id(), goal);
        if let Some(w) = self.strikes.wave.as_mut() {
            w.query = None;
        }
        u.set_fire_state(fire, 0);
        kgb_move_to(u, goal, 0, self.frame);
    }

    pub(super) fn strike_evasion_goal(&self, id: i32) -> Option<Vec3> {
        self.strikes.evasion.get(&id).filter(|(_, until)| self.frame < *until).map(|(p, _)| *p)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn enemy_coverage_keeps_muster_out_even_with_friendly_support() {
        let gun = Vec3::xz(0.0, 0.0);
        assert!(coverage(Vec3::xz(900.0, 0.0), gun, 1200.0 + 200.0) > 0.0);
        assert_eq!(coverage(Vec3::xz(1500.0, 0.0), gun, 1200.0 + 200.0), 0.0);
        assert!(coverage(Vec3::xz(300.0, 0.0), gun, HIT_RADIUS + 180.0) > 0.0);
    }
}
