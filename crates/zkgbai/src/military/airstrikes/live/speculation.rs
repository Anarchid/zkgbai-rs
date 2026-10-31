//! Firing evidence is a search area, never an engine unit handle or bomb aim.
use super::*;
use crate::military::sweeps::counterbattery::FRESH as FIX_FRESH;
use crate::util::get_directional_point;

const SEARCH_TIMEOUT: i32 = 30 * 45;
const LOOK_TIME: i32 = 30 * 5;
const RETRY: i32 = 30 * 30;

pub(super) struct Scout {
    fix: Inference,
    pos: Vec3,
    started: i32,
    arrived: Option<i32>,
}

#[derive(Default)]
pub(super) struct State {
    pub attempted: BTreeMap<i32, i32>,
    scouts: BTreeMap<i32, Scout>,
}

fn artillery(name: &str) -> bool {
    matches!(name, "hoverarty" | "jumparty" | "striderarty" | "tankheavyarty" | "tankarty")
}

pub(super) fn searching(frame: i32, launched: i32, arrived: Option<i32>) -> bool {
    frame - launched <= SEARCH_TIMEOUT && arrived.is_none_or(|f| frame - f <= LOOK_TIME)
}

fn retry_allowed(frame: i32, evidence: i32, attempted: Option<i32>) -> bool {
    attempted.is_none_or(|f| frame - f >= RETRY && evidence > f)
}

impl Ai {
    fn airstrike_inferences(&self) -> Vec<(Inference, f32, f32)> {
        if self.dev_off("airstrikes") || self.dev_off("airspeculate") {
            return Vec::new();
        }
        self.counterbattery_fixes(FIX_FRESH)
            .filter(|f| f.ground && f.uncertainty.is_finite() && (0.0..=1200.0).contains(&f.uncertainty))
            .filter_map(|f| {
                let name = f.def.as_deref().filter(|n| artillery(n))?;
                let d = self.defs.get(name)?;
                let value = d.cost(self.metal);
                if !priority_target(value, d.health()) {
                    return None;
                }
                // Ordinary missions take over as soon as the gun is identified.
                // Do not ask the engine for unseen unit positions or health.
                if self.war.targets.values().any(|e| {
                    !e.is_dead
                        && e.identified
                        && (e.visible
                            || e.is_radar_visible
                            || self.bomber_handler.sorties.iter().any(|s| s.job.targets.contains(&e.unit.id())))
                        && e.def_name == name
                        && distance(e.position, f.pos) <= f.uncertainty + 300.0
                }) {
                    return None;
                }
                Some((
                    Inference { id: f.id, def: name.to_owned(), pos: f.pos, uncertainty: f.uncertainty, last: f.last },
                    value,
                    d.health(),
                ))
            })
            .collect()
    }

    pub(super) fn airstrike_speculative_jobs(&self) -> Vec<Mission> {
        let sight = self.defs.get(Plane::Likho.name()).map_or(0.0, |d| d.los_radius());
        self.airstrike_inferences()
            .into_iter()
            // A Likho must be able to reveal the search area on one pass.
            .filter(|(f, _, _)| f.uncertainty <= (sight * 0.8).min(600.0))
            .map(|(f, value, hp)| Mission {
                submerged: false,
                kind: MissionKind::Speculative,
                target: f.id,
                pos: f.pos,
                hp,
                value,
                group: None,
                targets: vec![f.id],
                seen: f.last,
                deep: false,
                inference: Some(f),
            })
            .collect()
    }

    pub(super) fn airstrike_hunt_available(&self, job: &Mission) -> bool {
        let Some(fix) = &job.inference else { return true };
        !self.dev_off("airspeculate")
            && !self
                .bomber_handler
                .speculation
                .scouts
                .values()
                .any(|s| s.fix.id == fix.id && searching(self.frame, s.started, s.arrived))
            && retry_allowed(self.frame, fix.last, self.bomber_handler.speculation.attempted.get(&fix.id).copied())
    }

    pub(super) fn airstrike_acquire(&self, sortie: &mut Sortie, occupied: &BTreeSet<i32>) -> bool {
        if sortie.job.kind != MissionKind::Speculative {
            return false;
        }
        let Some(fix) = &sortie.job.inference else { return false };
        let target = self
            .war
            .targets
            .values()
            .filter(|e| {
                !e.is_dead
                    && e.identified
                    && (e.visible || e.is_radar_visible)
                    && e.def_name == fix.def
                    && distance(e.position, fix.pos) <= fix.uncertainty + 300.0
                    && !occupied.contains(&e.unit.id())
            })
            .min_by(|a, b| distance(a.position, fix.pos).total_cmp(&distance(b.position, fix.pos)));
        if let Some(e) = target {
            sortie.job.kind = MissionKind::Heavy;
            sortie.job.target = e.unit.id();
            sortie.job.targets.push(e.unit.id());
            sortie.job.pos = e.position;
            sortie.memory.last_contact = self.frame;
            // Replace the final search move with an actual target attack now.
            sortie.last_order = -1000;
            sortie.last_route = -1000;
            return true;
        }
        false
    }

    /// Called by the ordinary air scout controller. Sparrow may be expended;
    /// Owl gets a stand-off peek only if its approach and return are survivable.
    pub(crate) fn airstrike_scout_route(&mut self, unit: Unit) -> Option<Vec<Vec3>> {
        let id = unit.id();
        let frame = self.frame;
        self.bomber_handler.speculation.scouts.retain(|_, s| searching(frame, s.started, s.arrived));
        self.bomber_handler.speculation.attempted.retain(|_, f| frame - *f < 30 * 120);
        if self.dev_off("airstrikes") || self.dev_off("airspeculate") {
            self.bomber_handler.speculation.scouts.clear();
            return None;
        }
        if springai::reserved::is_reserved(self.cb, id) {
            return None;
        }
        let from = unit.pos();
        let inferences = self.airstrike_inferences();
        // Release a scout once its evidence expires or a real contact is acquired.
        self.bomber_handler.speculation.scouts.retain(|_, s| inferences.iter().any(|(f, _, _)| f.id == s.fix.id));
        if let Some(s) = self.bomber_handler.speculation.scouts.get_mut(&id) {
            if distance(from, s.pos) < 150.0 {
                s.arrived.get_or_insert(frame);
            }
        }
        let selected = self.bomber_handler.speculation.scouts.get(&id).map(|s| (s.fix.clone(), s.pos));
        let d = unit.def()?;
        let sparrow = self.defs.name_of(unit) == Plane::Sparrow.name();
        let mut options: Vec<_> = if let Some(selected) = selected {
            vec![selected]
        } else {
            let mut fixes = inferences;
            fixes.sort_by(|(a, av, _), (b, bv, _)| {
                (bv / (distance(from, b.pos) + 1200.0)).total_cmp(&(av / (distance(from, a.pos) + 1200.0)))
            });
            fixes
                .into_iter()
                .filter(|(f, _, _)| {
                    retry_allowed(frame, f.last, self.bomber_handler.speculation.attempted.get(&f.id).copied())
                        && !self.bomber_handler.speculation.scouts.values().any(|s| s.fix.id == f.id)
                        && !self
                            .bomber_handler
                            .sorties
                            .iter()
                            .any(|s| s.job.inference.as_ref().is_some_and(|i| i.id == f.id))
                })
                .map(|(f, _, _)| {
                    let offset = if sparrow { 0.0 } else { (d.los_radius() * 0.75 - f.uncertainty).max(0.0) };
                    let pos = if offset <= 0.0 || distance(f.pos, from) < 1.0 {
                        f.pos
                    } else {
                        get_directional_point(f.pos, from, offset)
                    };
                    (f, pos)
                })
                .collect()
        };
        // Most scout ticks have no artillery evidence. Avoid building two
        // map-wide flight fields when there is nowhere to investigate.
        if options.is_empty() {
            self.bomber_handler.speculation.scouts.remove(&id);
            return None;
        }
        let interceptors = self.airstrike_interceptors_except(None);
        let field = self.airstrike_field(from, &interceptors);
        let sources = self.airstrike_defenses();
        let home = self.graph.get_ally_center();
        let return_field = self.airstrike_field(home, &interceptors);
        for (fix, pos) in options.drain(..) {
            let route = field.route(pos);
            let exit = return_route(home, &return_field.route(pos));
            let risk = round_trip(
                defense::forecast(from, &route, &sources, &interceptors),
                defense::forecast(pos, &exit, &sources, &interceptors),
            );
            if route.is_empty() || (!sparrow && (risk.intercepted || !risk.survives(unit.health(), d.speed()))) {
                continue;
            }
            if !self.bomber_handler.speculation.scouts.contains_key(&id) {
                self.bomber_handler.speculation.attempted.insert(fix.id, frame);
                self.bomber_handler.speculation.scouts.insert(id, Scout { fix, pos, started: frame, arrived: None });
            }
            return Some(route);
        }
        self.bomber_handler.speculation.scouts.remove(&id);
        None
    }

    pub(super) fn airstrike_scout_status(&self) -> serde_json::Value {
        serde_json::json!(
            self.bomber_handler
                .speculation
                .scouts
                .iter()
                .filter(|(_, s)| searching(self.frame, s.started, s.arrived))
                .map(|(id, s)| serde_json::json!({"scout": id, "evidence": s.fix.id, "def": s.fix.def,
                "pos": [s.pos.x,s.pos.z], "uncertainty": s.fix.uncertainty, "started": s.started}))
                .collect::<Vec<_>>()
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn search_and_retry_are_bounded_by_time_and_new_evidence() {
        assert!(searching(600, 0, None));
        assert!(!searching(1351, 0, None));
        assert!(searching(450, 0, Some(300)));
        assert!(!searching(451, 0, Some(300)));
        assert!(retry_allowed(1200, 1100, Some(100)));
        assert!(!retry_allowed(1200, 100, Some(100)));
        assert!(!retry_allowed(600, 500, Some(100)));
    }
    #[test]
    fn inference_only_hunts_artillery() {
        for name in ["hoverarty", "jumparty", "tankarty", "tankheavyarty", "striderarty"] {
            assert!(artillery(name));
        }
        for name in ["vehsupport", "tankassault", "cloaksnipe", "turretaalaser"] {
            assert!(!artillery(name));
        }
    }
}
