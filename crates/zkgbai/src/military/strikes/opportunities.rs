//! Off-brand targets are raids on genuinely unprotected ground, not suicide jobs.
use super::*;

const ESCORT_RADIUS: f32 = 600.0;
const RANGE_MARGIN: f32 = 150.0;

pub(super) struct Threat {
    pos: Vec3,
    radius: f32,
}

/// Check the approach corridor as well as the destination. Friendly strength
/// never cancels these threat discs: winning a fight is not "no threat".
pub(super) fn clear(from: Vec3, to: Vec3, threats: &[Threat]) -> bool {
    let dx = to.x - from.x;
    let dz = to.z - from.z;
    let len2 = dx * dx + dz * dz;
    threats.iter().all(|t| {
        let along = (((t.pos.x - from.x) * dx + (t.pos.z - from.z) * dz) / len2.max(1.0)).clamp(0.0, 1.0);
        let closest = Vec3::xz(from.x + along * dx, from.z + along * dz);
        distance(t.pos, closest) > t.radius
    })
}

impl Ai {
    pub(super) fn strike_opportunity_threats(&self) -> Vec<Threat> {
        let mut threats: Vec<_> = self
            .war
            .targets
            .values()
            .filter(|e| !e.is_dead && e.position != NULLPOS && (e.is_static || self.frame - e.last_seen < JOB_MEMORY))
            .filter(|e| {
                // A dedicated AA formation does not defend itself against Fleas.
                // Unknown contacts and ground-capable escorts do, including ones
                // temporarily stunned/disarmed or under construction.
                !e.identified || e.ud.is_none_or(|d| !self.strike_defenseless(d))
            })
            .map(|e| Threat { pos: e.position, radius: (e.def_max_weapon_range() + RANGE_MARGIN).max(ESCORT_RADIUS) })
            .collect();
        let sniper_range = self.defs.get("cloaksnipe").map_or(700.0, |d| d.max_weapon_range());
        threats.extend(
            self.phantom_fixes(JOB_MEMORY)
                .into_iter()
                .map(|fix| Threat { pos: fix.pos, radius: sniper_range + PHANTOM_FIX_RADIUS }),
        );
        threats
    }

    pub(super) fn strike_opportunity_clear(&self, from: Vec3, to: Vec3) -> bool {
        clear(from, to, &self.strike_opportunity_threats())
    }

    pub(super) fn strike_opportunity_wave_clear(&self, wave: &Wave, to: Vec3) -> bool {
        let threats = self.strike_opportunity_threats();
        wave.members.iter().filter_map(|id| self.fighters.get(id)).all(|f| clear(f.get_pos(), to, &threats))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ravaged_mex_guarded_by_lotus_is_not_an_opportunity() {
        let from = Vec3::xz(1700.0, 2650.0);
        let mex = Vec3::xz(2104.0, 4680.0);
        let lotus = [
            Threat { pos: Vec3::xz(1968.0, 4592.0), radius: 610.0 },
            Threat { pos: Vec3::xz(2176.0, 4576.0), radius: 610.0 },
        ];
        assert!(!clear(mex, mex, &lotus));
        assert!(!clear(from, mex, &lotus));
        assert!(clear(from, mex, &[]));
    }

    #[test]
    fn safe_destination_does_not_excuse_a_guarded_approach() {
        let from = Vec3::xz(1000.0, 1000.0);
        let target = Vec3::xz(3000.0, 1000.0);
        let guard = [Threat { pos: Vec3::xz(2000.0, 1500.0), radius: ESCORT_RADIUS }];
        assert!(clear(from, from, &guard));
        assert!(clear(target, target, &guard));
        assert!(!clear(from, target, &guard));
        // The same check runs again during a dive when an escort appears.
        assert!(!clear(Vec3::xz(2100.0, 1000.0), target, &guard));
        assert!(clear(Vec3::xz(1000.0, 2500.0), Vec3::xz(3000.0, 2500.0), &guard));
    }
}
