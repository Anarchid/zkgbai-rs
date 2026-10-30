//! Deterministic impact bearings exercise inference without exposing an enemy
//! handle. `airstrike-hunt[-sparrow|-owl]` versus `airstrike-hunt-target`.
use crate::{Ai, util::get_directional_point};
use springai::Vec3;

impl Ai {
    pub(super) fn airstrike_hunt_scenario(&mut self, test: &str, frame: i32) {
        let map = self.cb.map();
        let site = Vec3::xz(map.width() as f32 * 4.0, map.height() as f32 * 4.0 + 400.0);
        if frame == 60 {
            self.cb.cheats_set_enabled(true);
            let base = self.graph.get_ally_center();
            let mut spawn = Vec::new();
            if test == "airstrike-hunt-target" {
                spawn.push(("tankarty", site));
            } else {
                let scout = match test {
                    "airstrike-hunt-sparrow" => Some("planelightscout"),
                    "airstrike-hunt-owl" => Some("planescout"),
                    _ => None,
                };
                let bomber = if scout.is_some() { "bomberprec" } else { "bomberheavy" };
                for i in 0..if scout.is_some() { 4 } else { 2 } {
                    spawn.push((bomber, Vec3::xz(base.x + i as f32 * 50.0, base.z + 100.0)));
                }
                if let Some(scout) = scout {
                    spawn.push((scout, base));
                }
                spawn.push(("staticrearm", get_directional_point(base, site, 350.0)));
                spawn.push(("energyfusion", get_directional_point(base, site, -300.0)));
                let range = self.defs.get("tankarty").unwrap().max_weapon_range();
                for x in [-400.0, 400.0] {
                    spawn.push(("staticstorage", Vec3::xz(site.x + x, site.z - range * 0.7)));
                }
            }
            for (name, pos) in spawn {
                if let Some(d) = self.defs.get(name) {
                    let p = map.find_closest_build_site(d, pos, 100.0, 1, 0);
                    if let Some(u) = self.cb.cheat_new_unit(d, if p.x >= 0.0 { p } else { pos }) {
                        if name == "tankarty" || name == "staticstorage" {
                            u.set_fire_state(0, 0);
                            u.set_move_state(0, 0);
                            springai::reserved::reserve(self.cb, u.id());
                        }
                        self.log(&format!("hunt-test: spawned {name} {} at {:.0},{:.0}", u.id(), u.pos().x, u.pos().z));
                    }
                }
            }
        }
        if test == "airstrike-hunt-target" {
            if frame % 30 == 0 {
                for u in self.cb.team_units().into_iter().filter(|u| self.defs.name_of(*u) == "tankarty") {
                    springai::reserved::as_owner(|| {
                        u.stop(0);
                        u.set_fire_state(0, 0);
                    });
                }
            }
            return;
        }
        if frame == 180 {
            let weapon = self
                .defs
                .get("tankarty")
                .unwrap()
                .weapon_mounts()
                .into_iter()
                .filter_map(|m| m.weapon_def())
                .max_by(|a, b| a.range().total_cmp(&b.range()));
            for u in self
                .cb
                .team_units()
                .into_iter()
                .filter(|u| self.defs.name_of(*u) == "staticstorage")
                .collect::<Vec<_>>()
            {
                self.sweeps_unit_damaged(u, None, site - u.pos(), weapon);
            }
            self.log(&format!("hunt-test: two impact bearings; fixes={}", self.counterbattery_status()));
        }
        if frame % 150 == 0 {
            self.log(&format!("hunt-test: {}", self.airstrikes_status()));
        }
    }
}
