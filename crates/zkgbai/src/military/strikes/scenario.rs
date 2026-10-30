//! Reproducible controller smoke test: `test=assassin-halberd` (or dirtbag,
//! flea) versus `test=assassin-target` on a flat map. A single approximate
//! fix is seeded; the target must actually be found, decloaked and killed.
use super::*;
impl Ai {
    pub(crate) fn strikes_scenario(&mut self, frame: i32) {
        let Some(test) = self.test.clone() else { return };
        let test = test.as_str();
        if test.starts_with("assassin-") && frame == 1500 {
            let orphaned = self
                .cb
                .friendly_units()
                .iter()
                .filter(|u| {
                    self.defs.name_of(**u) == "spiderscout"
                        && u.health() > 0.0
                        && !self.fighters.contains_key(&u.id())
                        && !self.war.new_units.contains(&u.id())
                        && self.role_of(u.id()).is_none()
                })
                .count();
            self.log(&format!("assassin-test: orphaned Fleas={orphaned}; {}", self.strikes_status()));
        }
        if matches!(test, "assassin-widow" | "assassin-no-widow") && frame == 90 {
            // UnitFinished has now run: detach the staged Fleas from automatic
            // picket claims and offer a real ordinary raider force to strikes.
            for u in self
                .cb
                .friendly_units()
                .into_iter()
                .filter(|u| self.defs.name_of(*u) == "spiderscout")
                .collect::<Vec<_>>()
            {
                self.assign_role(u, super::super::roles::Role::SoloRaider);
                springai::reserved::own(
                    self.cb,
                    u.id(),
                    springai::reserved::Owner { module: "strike_test", helpers: &["war.unitFinished", "war.unitIdle"] },
                );
                u.stop(0);
                u.set_fire_state(0, 0);
            }
            let target = self
                .war
                .targets
                .values()
                .find(|e| e.visible && !e.is_dead && (e.is_com || super::super::jaeger::is_heavy(e, self.metal)))
                .map(|e| e.unit);
            if let Some(target) = target {
                for u in self.cb.friendly_units().into_iter().filter(|u| self.defs.name_of(*u) == "spiderantiheavy") {
                    springai::reserved::as_owner(|| u.attack(target, 0));
                    self.log("assassin-test: Widow attacks the finisher target");
                }
            }
            return;
        }
        if test == "assassin-aa-late-guard" && frame == 450 {
            let map = self.cb.map();
            self.strikes_scenario_guard(Vec3::xz(map.width() as f32 * 4.0, map.height() as f32 * 4.0 + 300.0));
            return;
        }
        if !test.starts_with("assassin-") || frame != 60 {
            return;
        }
        let map = self.cb.map();
        let center = Vec3::xz(map.width() as f32 * 4.0, map.height() as f32 * 4.0);
        let pos = Vec3::xz(center.x, center.z + 300.0);
        self.cb.cheats_set_enabled(true);
        if matches!(test, "assassin-aa" | "assassin-aa-guarded" | "assassin-aa-late-guard") {
            if let Some(def) = self.defs.get("vehaa") {
                for i in 0..4 {
                    if let Some(u) = self.cb.cheat_new_unit(def, Vec3::xz(pos.x + i as f32 * 70.0, pos.z)) {
                        u.set_move_state(0, 0);
                        u.set_fire_state(0, 0); // Keep the LOS observer alive.
                        // Keep the fixture beside its guard instead of letting
                        // normal AA retreat orders change the test conditions.
                        springai::reserved::reserve(self.cb, u.id());
                        self.log(&format!("assassin-test: AA {} awaits the swarm", u.id()));
                    }
                }
            }
            if test == "assassin-aa-guarded" {
                self.strikes_scenario_guard(pos);
            }
            return;
        }
        if test == "assassin-heavy" || test == "assassin-commander" {
            let def = if test == "assassin-heavy" {
                self.defs.get("tankassault")
            } else {
                self.cb.friendly_units().iter().filter_map(|u| u.def()).find(|d| self.defs.is_commander(*d))
            };
            if let Some(def) = def
                && let Some(u) = self.cb.cheat_new_unit(def, pos)
            {
                u.set_move_state(0, 0);
                self.log(&format!("assassin-test: {} {} waits for the Widow", self.defs.name(def), u.id()));
            }
            return;
        }
        if test == "assassin-widow" || test == "assassin-no-widow" {
            let home = Vec3::xz(center.x, center.z - 350.0);
            if let Some(def) = self.defs.get("spiderscout") {
                for i in 0..20 {
                    let at = Vec3::xz(home.x + (i % 5) as f32 * 35.0, home.z + (i / 5) as f32 * 35.0);
                    if let Some(u) = self.cb.cheat_new_unit(def, at) {
                        self.assign_role(u, super::super::roles::Role::SoloRaider);
                        u.set_fire_state(0, 0);
                        // Hold the ordinary raiders for the real strike selector.
                        // Adopting a raider releases this role ownership.
                        springai::reserved::own(
                            self.cb,
                            u.id(),
                            springai::reserved::Owner {
                                module: "strike_test",
                                helpers: &["war.unitFinished", "war.unitIdle"],
                            },
                        );
                    }
                }
            }
            let name = if test == "assassin-widow" { "spiderantiheavy" } else { "planescout" };
            if let Some(def) = self.defs.get(name)
                && let Some(u) = self.cb.cheat_new_unit(def, Vec3::xz(pos.x + 100.0, pos.z))
            {
                springai::reserved::own(
                    self.cb,
                    u.id(),
                    springai::reserved::Owner { module: "strike_test", helpers: &["war.unitFinished", "war.unitIdle"] },
                );
                u.set_fire_state(0, 0);
                u.set_move_state(0, 0);
            }
            self.log("assassin-test: 20 ordinary Fleas await a stunned finisher target");
            return;
        }
        if matches!(test, "assassin-target" | "assassin-target2" | "assassin-lance") {
            let name = if test == "assassin-lance" { "hoverarty" } else { "cloaksnipe" };
            if let Some(def) = self.defs.get(name)
                && let Some(u) = self.cb.cheat_new_unit(def, pos)
            {
                u.set_move_state(0, 0);
                if test == "assassin-target2" {
                    if let Some(other) = self.cb.cheat_new_unit(def, Vec3::xz(pos.x + 100.0, pos.z)) {
                        other.set_move_state(0, 0);
                    }
                }
                self.log(&format!("assassin-test: {name} {} at {:.0},{:.0}", u.id(), pos.x, pos.z));
            }
            return;
        }
        if test == "assassin-support" {
            for (i, name) in ["veharty", "jumparty", "staticmissilesilo", "napalmmissile"].into_iter().enumerate() {
                if let Some(def) = self.defs.get(name) {
                    // Keep the Firewalker comfortably in range even if its
                    // normal artillery handler moves before the strike tick.
                    let z = if name == "jumparty" { center.z - 300.0 } else { center.z - 600.0 };
                    let at = Vec3::xz(center.x + i as f32 * 100.0, z);
                    if let Some(u) = self.cb.cheat_new_unit(def, at) {
                        self.war_unit_finished(u);
                        self.war_unit_idle(u);
                    }
                }
            }
            self.sweeps_note_sniper(pos, 510);
            self.sweeps_note_sniper(pos, 510);
            self.log("assassin-test: two-Phantom cadence seeded for support fire");
            return;
        }
        let tactic = match test {
            "assassin-halberd" => Tactic::Halberd,
            "assassin-scythe" | "assassin-scythe-lance" => Tactic::Scythe,
            "assassin-dirtbag" => Tactic::Dirtbag,
            "assassin-flea" => Tactic::Flea,
            _ => return,
        };
        let Some(def) = self.defs.get(tactic.name()) else { return };
        let home = Vec3::xz(center.x, center.z - 700.0);
        let target_name = if test == "assassin-scythe-lance" { "hoverarty" } else { "cloaksnipe" };
        let cost = self.defs.get(target_name).map_or(750.0, |d| d.cost(self.metal));
        let count = tactic.size(1, cost, def.cost(self.metal));
        self.strikes.wave = Some(Wave {
            tactic,
            job: Job {
                id: None,
                pos,
                last: frame,
                count: 1,
                value: cost,
                name: target_name.into(),
                prey: Prey::GlassCannon,
            },
            needed: count,
            members: Vec::new(),
            phase: Phase::Forming,
            since: frame,
            home,
            query: None,
        });
        for i in 0..count {
            let at = Vec3::xz(home.x + (i % 5) as f32 * 80.0 - 160.0, home.z + (i / 5) as f32 * 80.0);
            if let Some(u) = self.cb.cheat_new_unit(def, at) {
                // Exercise the production claim before general scout roles.
                self.war_unit_finished(u);
                self.war_unit_idle(u);
                u.stop(0);
            }
        }
        if target_name == "cloaksnipe" {
            self.sweeps_seed_phantom_fix(pos, 1);
        } else if let Some(scout) = self.defs.get("planescout") {
            self.cb.cheat_new_unit(scout, Vec3::xz(pos.x + 150.0, pos.z));
        }
        self.log(&format!("assassin-test: {count} {} assembled", tactic.name()));
    }
    fn strikes_scenario_guard(&self, pos: Vec3) {
        if let Some(def) = self.defs.get("turretlaser")
            && let Some(u) = self.cb.cheat_new_unit(def, Vec3::xz(pos.x, pos.z + 250.0))
        {
            // Observe the policy decision without the guard destroying the
            // wave or its scout before the next controller tick.
            u.set_fire_state(0, 0);
            springai::reserved::reserve(self.cb, u.id());
            self.log(&format!("assassin-test: Lotus {} guards the AA formation", u.id()));
        }
    }
}
