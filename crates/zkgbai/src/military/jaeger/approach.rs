//! Live inputs and route checks for the jaegers' interference map.

use super::*;
use interference::{Assessment, Contact, Crossing, Map, Observation};

pub(super) struct QuietApproach {
    pub to: Vec3,
    pub assessment: Assessment,
}

impl Ai {
    pub(super) fn jaeger_update_interference(&mut self, frame: i32) {
        if self.jaegers.interference.frame.is_some_and(|last| frame - last < 30) {
            return;
        }
        let map = self.cb.map();
        let size = [map.width() as f32 * 8.0, map.height() as f32 * 8.0];
        let centres = Map::centres(size[0], size[1]);
        let positions: Vec<_> = centres.iter().map(|p| Vec3::xz(p[0], p[1])).collect();
        let radar = self.los.in_radar(&positions);
        let observations: Vec<_> = positions
            .iter()
            .zip(radar)
            .map(|(&p, radar)| Observation {
                los: self.los.is_in_los(p),
                radar,
                age: (frame - self.scouts_last_seen(p)).max(0) as f32 / 30.0,
            })
            .collect();
        let contacts: Vec<_> = self
            .war
            .targets
            .values()
            .filter(|e| !e.is_dead && e.position != NULLPOS && (e.is_static || frame - e.last_seen <= STALE))
            .map(|e| {
                let track = self.qrf.tracks.get(&e.unit_id).filter(|t| t.last_seen >= e.last_seen);
                let pos = track.map_or(e.position, |t| t.pos);
                let seen = track.map_or(e.last_seen, |t| t.last_seen);
                let speed = if e.is_static {
                    0.0
                } else {
                    let known = e.ud.map_or(115.0, |_| e.def_speed());
                    // A past Lobster throw must not turn this unit into a
                    // permanently supersonic patrol in the interference map.
                    track.map_or(known, |t| known.max(t.top_speed.min(1.2 * known)))
                };
                let radius = if e.is_static && e.can_attack() && !e.is_aa {
                    e.def_max_weapon_range().max(e.def_radius().unwrap_or(16.0))
                } else {
                    e.def_radius().unwrap_or(16.0)
                };
                Contact { id: e.unit_id, pos: [pos.x, pos.z], speed, age: (frame - seen).max(0) as f32 / 30.0, radius }
            })
            .collect();
        let decloak = KINDS.map(|kind| {
            self.defs.get(kind.name).map_or(kind.decloak, |d| d.decloak_distance()).max(kind.decloak) + DODGE_MARGIN
        });
        self.jaegers.interference.rebuild(size, frame, decloak, &contacts, &observations);
    }

    /// Existing support still permits ordinary fighting. The map only grants
    /// short independent approaches; it never substitutes for Widow finishers.
    pub(super) fn jaeger_supported(&self, at: Vec3, army: &[Vec3]) -> bool {
        self.war.get_total_friendly_power(at) > 0.0
            || self.graph.is_ally_territory(at)
            || army.iter().any(|&p| distance(p, at) <= SUPPORT_RANGE)
    }

    pub(super) fn jaeger_quiet_approach(
        &self,
        kind: Kind,
        from: Vec3,
        target: i32,
        stand: f32,
        continuing: bool,
    ) -> Option<QuietApproach> {
        let e = self.war.targets.get(&target)?;
        if e.is_dead || !e.visible || e.position == NULLPOS || self.frame - e.last_seen > POUNCE_SEEN {
            return None;
        }
        if kind.stuns && !self.jaeger_finishes(e.position, e.def_health()) {
            return None;
        }
        let def = self.defs.get(kind.name)?;
        let profile = KINDS.iter().position(|k| k.name == kind.name)?;
        let to = if distance(from, e.position) > stand { get_directional_point(e.position, from, stand) } else { from };
        // A short straight crossing must really be walkable. Connectivity
        // alone could hide a long detour through unexamined fog or defenses.
        let grid = self.qrf.grid.as_ref()?;
        let mobility = Mobility::of(def)?;
        let steps = (distance(from, to) / (CELL * 0.5)).ceil().max(1.0) as usize;
        if distance(from, to) > def.speed() * interference::MAX_WALK {
            return None;
        }
        for i in 0..=steps {
            let k = i as f32 / steps as f32;
            let p = Vec3::xz(from.x + (to.x - from.x) * k, from.z + (to.z - from.z) * k);
            if !grid.passable(p, mobility) || self.war.get_porc_threat(p) > 0.0 {
                return None;
            }
        }
        if !reaches(self.reach_terrain(), def, from, to, 0.0) {
            return None;
        }
        // Reserve the jump flight or final weapon approach as well as the walk.
        let hold = if kind.suicide { 3.0 } else { 2.0 };
        let assessment = self.jaegers.interference.assess(
            Crossing { from: [from.x, from.z], to: [to.x, to.z], speed: def.speed(), hold, profile, target },
            self.frame,
        )?;
        let allowed = assessment.allows(continuing);
        let index = if allowed {
            0
        } else if !assessment.clear_of_screens(continuing) {
            1
        } else {
            2
        };
        let mut checks = self.jaegers.interference_checks.get();
        checks[index] += 1;
        self.jaegers.interference_checks.set(checks);
        allowed.then_some(QuietApproach { to, assessment })
    }
}
