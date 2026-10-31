//! `airstrike-targeting` vs `airstrike-targeting-targets`: first economical
//! Raven releases on a mex/solar pair, then a real Likho anti-air pass.
use crate::{Ai, util::get_directional_point};
use springai::Vec3;

impl Ai {
    pub(super) fn airstrike_targeting_scenario(&mut self, test: &str, frame: i32) {
        let defender = test == "airstrike-targeting-targets";
        let map = self.cb.map();
        let site = Vec3::xz(map.width() as f32 * 4.0, map.height() as f32 * 4.0 + 400.0);
        let base = self.graph.get_ally_center();
        let mut spawn = Vec::new();
        if frame == 60 {
            self.cb.cheats_set_enabled(true);
            if defender {
                spawn.extend([("staticmex", site), ("energysolar", Vec3::xz(site.x + 600.0, site.z))]);
            } else {
                spawn.extend([
                    ("bomberprec", base),
                    ("bomberprec", Vec3::xz(base.x + 80.0, base.z)),
                    ("staticrearm", get_directional_point(base, site, 350.0)),
                    ("energyfusion", get_directional_point(base, site, -300.0)),
                    ("planescout", Vec3::xz(site.x + 250.0, site.z - 300.0)),
                ]);
            }
        }
        if frame == 900 {
            if defender {
                spawn.push(("gunshipheavyskirm", site));
            } else {
                spawn.extend([("bomberheavy", base), ("bomberheavy", Vec3::xz(base.x + 80.0, base.z))]);
            }
        }
        for (name, pos) in spawn {
            if let Some(d) = self.defs.get(name) {
                let p = map.find_closest_build_site(d, pos, 100.0, 1, 0);
                if let Some(u) = self.cb.cheat_new_unit(d, if p.x >= 0.0 { p } else { pos }) {
                    u.set_fire_state(0, 0);
                    if defender || name == "planescout" {
                        springai::reserved::reserve(self.cb, u.id());
                    }
                    self.log(&format!("targeting-test: spawned {name} {}", u.id()));
                }
            }
        }
        if frame % 30 == 0 {
            for u in self.cb.team_units() {
                if springai::reserved::is_reserved(self.cb, u.id()) {
                    springai::reserved::as_owner(|| {
                        u.stop(0);
                        u.set_fire_state(0, 0);
                    });
                } else if matches!(frame, 90 | 930) && matches!(self.defs.name_of(u), "bomberprec" | "bomberheavy") {
                    // Require planner attacks, not opportunistic auto-fire.
                    u.set_fire_state(0, 0);
                }
            }
        }
        if frame % 150 == 0 {
            if defender {
                let targets: Vec<_> = self
                    .cb
                    .team_units()
                    .into_iter()
                    .filter(|u| matches!(self.defs.name_of(*u), "staticmex" | "energysolar" | "gunshipheavyskirm"))
                    .map(|u| (u.id(), self.defs.name_of(u).to_owned(), u.health()))
                    .collect();
                self.log(&format!("targeting-test: targets {targets:?}"));
            } else {
                self.log(&format!("targeting-test: {}", self.airstrikes_status()));
            }
        }
    }
}
