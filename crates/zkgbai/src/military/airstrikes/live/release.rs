//! Finish a bounded terminal pass instead of vetoing it with a fresh round-trip
//! estimate after the aircraft has already flown into the target's defenses.
use super::*;

const RANGE: f32 = 800.0;
const WINDOW: i32 = 30 * 8;

#[derive(Default)]
pub(super) struct State {
    entered: BTreeMap<i32, i32>,
}
impl State {
    fn register(&mut self, id: i32, frame: i32, plane: Plane, mission: MissionKind, range: f32) {
        if matches!(plane, Plane::Raven | Plane::Phoenix | Plane::Likho | Plane::Odin)
            && !matches!(mission, MissionKind::Speculative | MissionKind::Intercept | MissionKind::Shield)
            && range <= RANGE
        {
            // Leaving/reentering the release area must not extend the window.
            self.entered.entry(id).or_insert(frame);
        }
    }

    pub(super) fn active(&self, id: i32, frame: i32) -> bool {
        self.entered.get(&id).is_some_and(|&entered| frame >= entered && frame - entered < WINDOW)
    }

    pub(super) fn allows(&self, id: i32, frame: i32, forecast_safe: bool, hp: f32, max_hp: f32) -> bool {
        if self.active(id, frame) { hp >= max_hp * 0.5 } else { forecast_safe }
    }
}

impl Ai {
    pub(super) fn airstrike_commitments(&self, sortie: &mut Sortie, frame: i32) {
        let pos = self
            .war
            .targets
            .get(&sortie.job.target)
            .filter(|e| !e.is_dead && (e.visible || e.is_radar_visible))
            .map_or(sortie.job.pos, |e| e.position);
        for id in &sortie.members {
            let Some(a) = self.bomber_handler.units.get(id) else { continue };
            if a.unit.rules_param_float("noammo", 0.0) == 0.0 && !springai::reserved::is_reserved(self.cb, *id) {
                sortie.release.register(*id, frame, a.kind, sortie.job.kind, distance(a.unit.pos(), pos));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_pass_ignores_revised_forecasts_but_not_actual_damage() {
        for plane in [Plane::Raven, Plane::Phoenix, Plane::Likho, Plane::Odin] {
            let mut state = State::default();
            state.register(1, 100, plane, MissionKind::Group, RANGE + 1.0);
            assert!(!state.allows(1, 100, false, 1000.0, 1000.0));
            state.register(1, 130, plane, MissionKind::Group, RANGE);
            assert!(state.allows(1, 130, false, 1000.0, 1000.0));
            assert!(state.allows(1, 160, false, 500.0, 1000.0));
            assert!(!state.allows(1, 160, true, 499.0, 1000.0));
            assert!(!state.active(2, 160)); // each aircraft must enter itself
        }
    }

    #[test]
    fn commitment_expires_without_renewal_and_resumes_forecast_checks() {
        let mut state = State::default();
        state.register(1, 100, Plane::Raven, MissionKind::Heavy, 500.0);
        assert!(state.active(1, 100 + WINDOW - 1));
        state.register(1, 100 + WINDOW, Plane::Raven, MissionKind::Heavy, 100.0);
        assert!(!state.allows(1, 100 + WINDOW, false, 1000.0, 1000.0));
        assert!(state.allows(1, 100 + WINDOW, true, 1000.0, 1000.0));
    }

    #[test]
    fn searching_and_support_aircraft_do_not_commit_to_a_bomb_release() {
        let mut state = State::default();
        for plane in Plane::ALL {
            for mission in [MissionKind::Speculative, MissionKind::Intercept, MissionKind::Shield] {
                state.register(1, 100, plane, mission, 0.0);
                assert!(!state.active(1, 100));
            }
        }
        for plane in [Plane::Magpie, Plane::Raptor, Plane::Sparrow] {
            state.register(1, 100, plane, MissionKind::Heavy, 0.0);
            assert!(!state.active(1, 100));
        }
        // Acquiring a real quarry ends the speculative search and enables release.
        state.register(1, 130, Plane::Likho, MissionKind::Heavy, 300.0);
        assert!(state.active(1, 130));
    }
}
