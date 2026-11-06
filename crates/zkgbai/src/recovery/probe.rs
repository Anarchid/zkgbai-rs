//! Explicit developer scenario (`test=recovery`), never used in normal games.
//! Five closed pens exercise Solar clearance and each evacuation mechanism.

use crate::{Ai, util::distance};
use springai::{
    Unit, Vec3,
    reserved::{self, Owner},
};

#[derive(Default)]
#[checkpoint::state]
pub(super) struct Probe {
    cases: Vec<Case>,
}

#[checkpoint::state]
struct Case {
    name: &'static str,
    cargo: Unit,
    helper: Option<Unit>,
    at: Vec3,
    goal: Vec3,
    since: i32,
    commanded: bool,
    reported: bool,
}

impl Ai {
    pub(super) fn recovery_probe(&mut self) {
        if !matches!(self.test.as_deref(), Some("recovery" | "recoverywind")) {
            return;
        }
        let mut probe = std::mem::take(&mut self.recovery.probe);
        for case in &mut probe.cases {
            if !case.commanded && self.frame > case.since {
                let owner = Owner { module: "recovery.probe", helpers: &["recovery"] };
                // Isolate test movement from ordinary tactical decisions while
                // leaving it visible to the real detector and rescue planner.
                reserved::own(self.cb, case.cargo.id(), owner);
                case.cargo.move_to(case.goal, 0);
                if let Some(helper) = case.helper.filter(|_| case.name != "djinn") {
                    reserved::own(self.cb, helper.id(), owner);
                    helper.stop(0);
                }
                case.commanded = true;
            }
            if !case.reported && self.frame - case.since >= 140 * 30 {
                let travel = distance(case.at, case.cargo.pos());
                self.log(&format!(
                    "recovery-probe: {} {} cargo=#{} displacement={travel:.0}",
                    case.name,
                    if case.cargo.health() > 0.0
                        && travel > 256.0
                        && (self.recovery.resolved.get(&case.cargo.id()).copied() == Some(case.name)
                            || (matches!(case.name, "solar" | "wind")
                                && case
                                    .helper
                                    .is_some_and(|u| self.recovery.resolved.get(&u.id()).copied() == Some(case.name))))
                    {
                        "PASS"
                    } else {
                        "FAIL"
                    },
                    case.cargo.id()
                ));
                reserved::reserve(self.cb, case.cargo.id());
                if let Some(helper) = case.helper {
                    reserved::reserve(self.cb, helper.id());
                }
                case.reported = true;
            }
        }
        let names: &[&str] = if self.test.as_deref() == Some("recoverywind") {
            &["wind", "wind", "wind"]
        } else {
            &["solar", "lobster", "djinn", "charon", "hercules"]
        };
        let index = probe.cases.len();
        if index < names.len() && self.frame >= 30 * (30 + index as i32 * 180) {
            let name = names[index];
            let grid = self.cb.map();
            let (w, h) = (grid.width() as f32 * 8.0, grid.height() as f32 * 8.0);
            let offsets = [(-0.2, -0.2), (0.2, -0.2), (-0.2, 0.2), (0.2, 0.2), (0.0, -0.3)];
            let desired = Vec3::xz(w * (0.5 + offsets[index].0), h * (0.5 + offsets[index].1));
            let cargo_name = match name {
                "solar" => "cloakriot",
                "hercules" => "striderdante",
                _ => "cloakriot",
            };
            if let Some(def) = self.defs.get(cargo_name) {
                let at = grid.find_closest_build_site(def, desired, 600.0, 0, 0);
                if at.x >= 0.0 && self.cb.cheats_set_enabled(true) {
                    // A square pen, open only where its Solar stands in the
                    // first case. Static storage is deliberately not expendable.
                    let spawn = |what: &str, dx: f32, dz: f32| {
                        self.defs.get(what).and_then(|d| {
                            self.cb.cheat_new_unit(d, self.recovery_ground(Vec3::xz(at.x + dx, at.z + dz)))
                        })
                    };
                    for side in [-1.0, 1.0] {
                        for k in -2..=2 {
                            spawn("staticstorage", side * 144.0, k as f32 * 48.0);
                        }
                        for k in -3i32..=3 {
                            if side > 0.0 && ((name == "solar" && k.abs() <= 1) || (name == "wind" && k.abs() <= 1)) {
                                continue;
                            }
                            spawn("staticstorage", k as f32 * 48.0, side * 144.0);
                        }
                    }
                    if name == "wind" {
                        // One, two, then three turbines in a narrow exit. The
                        // last case starts its constructor outside reclaim range.
                        for row in 0..=index {
                            let z = 144.0 + row as f32 * 48.0;
                            spawn("energywind", 0.0, z);
                            for x in [-72.0, 72.0] {
                                spawn("staticstorage", x, z);
                            }
                        }
                    }
                    if matches!(name, "solar" | "wind") {
                        if name == "solar" {
                            spawn("energysolar", 0.0, 144.0);
                        }
                        spawn("energyfusion", -400.0, 0.0);
                        spawn("energyfusion", -400.0, 200.0);
                    }
                    let helper = match name {
                        "solar" => spawn("cloakcon", -48.0, 64.0),
                        "wind" => spawn("cloakcon", -48.0, if index == 2 { -80.0 } else { 64.0 }),
                        "lobster" => spawn("amphlaunch", -64.0, 0.0),
                        "djinn" => spawn("amphtele", 800.0, 0.0),
                        "charon" => spawn("gunshiptrans", 500.0, 0.0),
                        "hercules" => spawn("gunshipheavytrans", 500.0, 0.0),
                        _ => None,
                    };
                    if let Some(cargo) = spawn(cargo_name, 0.0, 0.0) {
                        probe.cases.push(Case {
                            name,
                            cargo,
                            helper,
                            at,
                            goal: self.recovery_ground(Vec3::xz(at.x, at.z + 1000.0)),
                            since: self.frame,
                            commanded: false,
                            reported: false,
                        });
                        self.log(&format!(
                            "recovery-probe: {name} pen at ({:.0},{:.0}), cargo #{}",
                            at.x,
                            at.z,
                            cargo.id()
                        ));
                    }
                    self.cb.cheats_set_enabled(false);
                }
            }
        }
        self.recovery.probe = probe;
    }
}
