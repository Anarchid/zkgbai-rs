//! Reveal a more valuable, closer Lance while a loaded Likho is already
//! attacking a live, safe Fencer. Hold fire prevents incidental auto-targeting.
use crate::{
    Ai,
    util::{distance, get_directional_point},
};
use springai::Vec3;

impl Ai {
    pub(super) fn airstrike_opportunity_scenario(&mut self, test: &str, frame: i32) {
        let defender = test == "airstrike-opportunity-targets";
        let map = self.cb.map();
        let site = Vec3::xz(map.width() as f32 * 4.0, map.height() as f32 * 4.0 + 400.0);
        let base = self.graph.get_ally_center();
        let mut spawn = Vec::new();
        if frame == 60 {
            self.cb.cheats_set_enabled(true);
            if defender {
                spawn.push(("vehsupport", site));
            } else {
                spawn.extend([
                    ("bomberheavy", base),
                    ("staticrearm", get_directional_point(base, site, 350.0)),
                    ("energyfusion", get_directional_point(base, site, -300.0)),
                    ("planescout", site),
                ]);
            }
        }
        if defender
            && frame > 90
            && frame % 30 == 0
            && !self.cb.team_units().iter().any(|u| self.defs.name_of(*u) == "staticstorage")
            && let Some(bomber) = self.cb.enemy_units().into_iter().find(|u| {
                self.defs.name_of(*u) == "bomberheavy"
                    && u.rules_param_float("noammo", 0.0) == 0.0
                    && (1100.0..1800.0).contains(&distance(u.pos(), site))
            })
        {
            spawn.push(("hoverarty", get_directional_point(bomber.pos(), site, 550.0)));
            // Persistent marker prevents respawning a successfully bombed Lance.
            spawn.push(("staticstorage", base));
        }
        for (name, pos) in spawn {
            if let Some(d) = self.defs.get(name) {
                let p = map.find_closest_build_site(d, pos, 100.0, 1, 0);
                if let Some(u) = self.cb.cheat_new_unit(d, if p.x >= 0.0 { p } else { pos }) {
                    u.set_fire_state(0, 0);
                    if defender || name == "planescout" {
                        springai::reserved::reserve(self.cb, u.id());
                    }
                    self.log(&format!("opportunity-test: spawned {name} {}", u.id()));
                }
            }
        }
        if frame % 30 == 0 {
            for u in self.cb.team_units() {
                if springai::reserved::is_reserved(self.cb, u.id()) {
                    springai::reserved::as_owner(|| {
                        if self.defs.name_of(u) == "planescout" {
                            u.set_idle_mode(0, 0);
                            let dx = if (frame / 150) % 2 == 0 { -200.0 } else { 200.0 };
                            u.move_to(Vec3::xz(site.x + dx, site.z), 0);
                        } else {
                            u.stop(0);
                            u.set_move_state(0, 0);
                        }
                        u.set_fire_state(0, 0);
                    });
                } else if self.defs.name_of(u) == "bomberheavy" {
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
                    .filter(|u| matches!(self.defs.name_of(*u), "vehsupport" | "hoverarty"))
                    .map(|u| (u.id(), self.defs.name_of(u).to_owned(), u.health()))
                    .collect();
                self.log(&format!("opportunity-test: targets {targets:?}"));
            } else {
                self.log(&format!("opportunity-test: {}", self.airstrikes_status()));
            }
        }
    }
}
