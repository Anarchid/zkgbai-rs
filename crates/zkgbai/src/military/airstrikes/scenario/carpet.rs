//! Phoenix carpets on an unseen Phantom. `airstrike-carpet` (three Phoenixes,
//! a pad, two storages as victims; one synthetic impact bearing at frame 180)
//! versus `airstrike-carpet-target` (a cloaked Phantom holding fire).
//! `airstrike-carpet-live` (no synthetic bearing) versus
//! `airstrike-carpet-shooter` (the Phantom fires at the storages, a radar
//! tower beside it lends sight). The defender logs its Phantom's health,
//! cloak and burning every second: `carpet-test: phantom ...`.
//! `airstrike-carpet-crowd` is `airstrike-carpet` with four held Glaives of
//! ours on the line, 220-280 elmos short of the Phantom (friendly fire is
//! accepted by default; `enable=carpetspare` should hold the carpet).
use crate::{Ai, util::get_directional_point};
use springai::Vec3;

const SNIPER: &str = "cloaksnipe";

impl Ai {
    pub(super) fn airstrike_carpet_scenario(&mut self, test: &str, frame: i32) {
        let map = self.cb.map();
        let site = Vec3::xz(map.width() as f32 * 4.0, map.height() as f32 * 4.0 + 400.0);
        let defender = matches!(test, "airstrike-carpet-target" | "airstrike-carpet-shooter");
        let range = self.defs.get(SNIPER).map_or(700.0, |d| d.max_weapon_range());
        if frame == 60 {
            self.cb.cheats_set_enabled(true);
            let base = self.graph.get_ally_center();
            let mut spawn = Vec::new();
            if defender {
                spawn.push((SNIPER, site));
                if test == "airstrike-carpet-shooter" {
                    spawn.push(("staticradar", Vec3::xz(site.x + 120.0, site.z + 120.0)));
                }
            } else {
                for i in 0..3 {
                    spawn.push(("bomberriot", Vec3::xz(base.x + i as f32 * 60.0, base.z + 100.0)));
                }
                spawn.push(("staticrearm", get_directional_point(base, site, 350.0)));
                spawn.push(("energyfusion", get_directional_point(base, site, -300.0)));
                for x in [-400.0, 400.0] {
                    spawn.push(("staticstorage", Vec3::xz(site.x + x, site.z - range * 0.7)));
                }
                if test == "airstrike-carpet-crowd" {
                    // The synthetic bearing comes from the first storage.
                    let from = Vec3::xz(site.x - 400.0, site.z - range * 0.7);
                    for (i, d) in [220.0, 240.0, 260.0, 280.0].into_iter().enumerate() {
                        let p = get_directional_point(site, from, d);
                        let side = if i % 2 == 0 { 40.0 } else { -40.0 };
                        spawn.push(("cloakraid", Vec3::xz(p.x + side, p.z)));
                    }
                }
            }
            for (name, pos) in spawn {
                if let Some(d) = self.defs.get(name) {
                    let p = map.find_closest_build_site(d, pos, 100.0, 1, 0);
                    if let Some(u) = self.cb.cheat_new_unit(d, if p.x >= 0.0 { p } else { pos }) {
                        if matches!(name, SNIPER | "staticstorage" | "cloakraid") {
                            u.set_fire_state(if test == "airstrike-carpet-shooter" { 2 } else { 0 }, 0);
                            u.set_move_state(0, 0);
                            springai::reserved::reserve(self.cb, u.id());
                        }
                        self.log(&format!(
                            "carpet-test: spawned {name} {} at {:.0},{:.0}",
                            u.id(),
                            u.pos().x,
                            u.pos().z
                        ));
                    }
                }
            }
        }
        if defender {
            if frame % 30 == 0 {
                let phantoms: Vec<_> =
                    self.cb.team_units().into_iter().filter(|u| self.defs.name_of(*u) == SNIPER).collect();
                for u in &phantoms {
                    springai::reserved::as_owner(|| {
                        if test != "airstrike-carpet-shooter" {
                            u.stop(0);
                            u.set_fire_state(0, 0);
                        }
                    });
                    self.log(&format!(
                        "carpet-test: phantom {} hp={:.0}/{:.0} cloaked={} on_fire={:.0}",
                        u.id(),
                        u.health(),
                        u.max_health(),
                        u.is_cloaked(),
                        u.rules_param_float("on_fire", 0.0)
                    ));
                }
                if phantoms.is_empty() && frame % 300 == 0 {
                    self.log("carpet-test: phantom gone");
                }
            }
            return;
        }
        if test == "airstrike-carpet-crowd" && frame % 30 == 0 {
            for u in self.cb.team_units().into_iter().filter(|u| self.defs.name_of(*u) == "cloakraid") {
                springai::reserved::as_owner(|| {
                    u.stop(0);
                    u.set_fire_state(0, 0);
                });
                if frame % 150 == 0 {
                    self.log(&format!("carpet-test: glaive {} hp={:.0}/{:.0}", u.id(), u.health(), u.max_health()));
                }
            }
        }
        if frame == 180 && matches!(test, "airstrike-carpet" | "airstrike-carpet-crowd") {
            let weapon = self
                .defs
                .get(SNIPER)
                .unwrap()
                .weapon_mounts()
                .into_iter()
                .filter_map(|m| m.weapon_def())
                .max_by(|a, b| a.range().total_cmp(&b.range()));
            if let Some(u) = self.cb.team_units().into_iter().find(|u| self.defs.name_of(*u) == "staticstorage") {
                self.sweeps_unit_damaged(u, None, site - u.pos(), weapon);
            }
            self.log(&format!("carpet-test: one impact bearing; fixes={}", self.counterbattery_status()));
        }
        if frame % 150 == 0 {
            self.log(&format!("carpet-test: {}", self.airstrikes_status()));
        }
    }
}
