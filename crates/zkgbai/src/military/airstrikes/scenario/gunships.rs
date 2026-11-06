//! `airstrike-raptors` versus `airstrike-revenants`: two Raptors at home, an
//! outpost (a mex and three Solars) at the map center, outside our territory,
//! and a pair of Revenants the defender flies over it (held by the fixture,
//! firing at will). The Raptors should go at them as a pair (`raptorwear`)
//! although the full package is six, because they are over our buildings
//! (`airmenace`). Compare `?disable=raptorwear,airmenace`.
//! `airstrike-raptors-likho` adds a Likho (`likhorevenant`).
//! `airstrike-raptors-pad`: a bigger outpost (the Revenants stay in sight
//! longer) and, unless `padnano` is off, a caretaker beside the airpad
//! adopted as the economy adopts a built one: hurt Raptors come home to it,
//! and with `padlaunch` leave the pad at 70% while a job is open.
//! `airstrike-raptors-pad-lend`: the same, and the caretaker is lent away at
//! 0:30 as the walk-in guard lends one (back on the next logistics pass) and
//! ordered to guard the pad at 3:00 (any other order): each time it must
//! go back to its area repair over the pad.
use crate::{Ai, util::get_directional_point};
use springai::{Unit, Vec3};

impl Ai {
    pub(super) fn airstrike_gunship_scenario(&mut self, test: &str, frame: i32) {
        let map = self.cb.map();
        let center = Vec3::xz(map.width() as f32 * 4.0, map.height() as f32 * 4.0);
        let site = Vec3::xz(center.x, center.z + 400.0);
        let gunships = test == "airstrike-revenants";
        let watched: &[&str] = if gunships {
            &["gunshipassault"]
        } else {
            &["planeheavyfighter", "bomberheavy", "staticmex", "energysolar", "staticcon"]
        };
        if frame == 60 {
            self.cb.cheats_set_enabled(true);
            let base = self.graph.get_ally_center();
            let mut spawn = Vec::new();
            if gunships {
                // They come in from our side, so they are seen arriving.
                let start = get_directional_point(site, base, 1500.0);
                for i in 0..2 {
                    spawn.push(("gunshipassault", Vec3::xz(start.x + 60.0 * i as f32, start.z)));
                }
            } else {
                for i in 0..2 {
                    spawn.push(("planeheavyfighter", Vec3::xz(base.x + 40.0 * i as f32, base.z + 100.0)));
                }
                if test == "airstrike-raptors-likho" {
                    spawn.push(("bomberheavy", Vec3::xz(base.x + 80.0, base.z + 100.0)));
                }
                spawn.push(("staticrearm", get_directional_point(base, center, 400.0)));
                spawn.push(("energyfusion", get_directional_point(base, center, -300.0)));
                spawn.push(("staticmex", site));
                for (dx, dz) in [(90.0, 0.0), (-90.0, 0.0), (0.0, 90.0)] {
                    spawn.push(("energysolar", Vec3::xz(site.x + dx, site.z + dz)));
                }
                if test.starts_with("airstrike-raptors-pad") {
                    for (dx, dz) in [(180.0, 0.0), (-180.0, 0.0), (0.0, 180.0), (90.0, 180.0), (-90.0, 180.0)] {
                        spawn.push(("energysolar", Vec3::xz(site.x + dx, site.z + dz)));
                    }
                }
            }
            for (name, pos) in spawn {
                let Some(d) = self.defs.get(name) else { continue };
                let p = map.find_closest_build_site(d, pos, 250.0, 1, 0);
                let Some(u) = self.cb.cheat_new_unit(d, if p.x >= 0.0 { p } else { pos }) else { continue };
                if gunships {
                    springai::reserved::reserve(self.cb, u.id());
                }
                self.log(&format!("airstrike-test: spawned {name} {} at {:.0},{:.0}", u.id(), u.pos().x, u.pos().z));
            }
        }
        if test.starts_with("airstrike-raptors-pad")
            && frame == 90
            && !self.dev_off("padnano")
            && let Some(pad) = self.cb.team_units().into_iter().find(|u| self.defs.name_of(*u) == "staticrearm")
            && let Some(d) = self.defs.get("staticcon")
        {
            let near = get_directional_point(pad.pos(), center, -130.0);
            let p = map.find_closest_build_site(d, near, 120.0, 1, 0);
            if let Some(u) = self.cb.cheat_new_unit(d, if p.x >= 0.0 { p } else { near }) {
                self.eco_adopt_pad_nano(u, pad);
            }
        }
        if test == "airstrike-raptors-pad-lend" && matches!(frame, 900 | 5400) {
            let pad_nanos: Vec<(Unit, Unit)> = (self.eco.nanos.values())
                .filter(|n| self.defs.name_of(n.target) == "staticrearm")
                .map(|n| (n.unit, n.target))
                .collect();
            for (nano, pad) in pad_nanos {
                if frame == 900 {
                    self.walkin_lend_caretaker(nano, pad);
                } else {
                    nano.guard(pad, 0);
                }
                self.log(&format!(
                    "airstrike-test: caretaker {} {} pad {}",
                    nano.id(),
                    if frame == 900 { "lent away from" } else { "told to guard" },
                    pad.id()
                ));
            }
        }
        // The fixture alone flies the Revenants: over the outpost, shooting.
        if gunships && frame >= 120 && frame % 90 == 0 {
            for u in self.cb.team_units() {
                if springai::reserved::is_reserved(self.cb, u.id()) {
                    springai::reserved::as_owner(|| {
                        u.set_fire_state(2, 0);
                        u.set_move_state(2, 0);
                        u.fight(site, 0);
                    });
                }
            }
        }
        if frame % 300 == 0 {
            for r in [self.metal, self.energy] {
                self.cb.cheat_resource(r, 20000.0);
            }
        }
        if frame >= 150 && frame % 150 == 0 {
            let mut line = String::new();
            for u in self.cb.team_units() {
                let name = self.defs.name_of(u);
                if watched.contains(&name) {
                    let p = u.pos();
                    line.push_str(&format!(
                        " {name}:{}:{:.0}/{:.0}:n{:.0}@{:.0},{:.0}",
                        u.id(),
                        u.health(),
                        u.max_health(),
                        u.rules_param_float("noammo", 0.0),
                        p.x,
                        p.z
                    ));
                    if name == "staticcon" {
                        // Its orders: command id.parameter count.
                        let q: Vec<String> =
                            u.current_commands().iter().map(|c| format!("{}.{}", c.id(), c.params().len())).collect();
                        line.push_str(&format!(":q{}", q.join(",")));
                    }
                }
            }
            self.log(&format!("airstrike-test: standing{line}"));
        }
    }
}
