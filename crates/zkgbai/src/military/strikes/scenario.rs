//! Reproducible controller smoke test: `test=assassin-halberd` (or dirtbag,
//! flea or dart) versus `test=assassin-target` on a flat map. A single approximate
//! fix is seeded; the target must actually be found, decloaked and killed.
use super::*;
impl Ai {
    pub(crate) fn strikes_scenario(&mut self, frame: i32) {
        let Some(test) = self.test.clone() else { return };
        let test = test.as_str();
        if test.starts_with("assassin-lobster") {
            self.strikes_lobster_scenario(test, frame);
            return;
        }
        if test.starts_with("assassin-battery-") {
            self.strikes_battery_scenario(test, frame);
            return;
        }
        if matches!(test, "assassin-halberd-squad" | "assassin-halberd-squad-phantom") && frame == 90 {
            let units: Vec<_> =
                self.cb.friendly_units().into_iter().filter(|u| self.defs.name_of(*u) == "hoverassault").collect();
            let income = self.eco.adjusted_income;
            self.eco.adjusted_income = 45.0;
            for u in units {
                springai::reserved::release(self.cb, u.id());
                self.squad_remove_unit(u);
                self.squad_add_assault(Fighter::new(u, u.def().unwrap().cost(self.metal), self.team_id));
            }
            self.eco.adjusted_income = income;
            let sid = *self.squad_handler.squads.back().unwrap();
            self.squads.get_mut(sid).unwrap().status = 'a';
            self.log(&format!(
                "assassin-test: ordinary Halberd squad {sid} ready; kind={:?}, members={}",
                self.squads.get(sid).unwrap().kind,
                self.squads.get(sid).unwrap().fighters.len()
            ));
            let map = self.cb.map();
            if test == "assassin-halberd-squad-phantom" {
                self.sweeps_seed_phantom_fix(Vec3::xz(map.width() as f32 * 4.0, map.height() as f32 * 4.0 + 300.0), 1);
            } else if let Some(def) = self.defs.get("planescout") {
                self.cb
                    .cheat_new_unit(def, Vec3::xz(map.width() as f32 * 4.0 + 150.0, map.height() as f32 * 4.0 + 300.0));
            }
            return;
        }
        if matches!(test, "assassin-halberd-squad" | "assassin-halberd-squad-phantom")
            && frame >= 150
            && frame % 150 == 0
        {
            self.log(&format!("assassin-test: Halberd squads {}", self.strikes_status()));
        }
        if test == "assassin-raiders" && frame == 90 {
            let units: Vec<_> =
                self.cb.friendly_units().into_iter().filter(|u| self.defs.name_of(*u) == "cloakraid").collect();
            let mut fighters = Vec::new();
            for u in units {
                self.raider_remove_unit(u);
                self.retreat_remove_unit(u);
                fighters.push(Fighter::new_raider(u, u.def().unwrap().cost(self.metal), self.team_id));
            }
            self.raider_add_squad(fighters, 's');
            let sid = *self.raider_handler.raider_squads.back().unwrap();
            self.squads.get_mut(sid).unwrap().status = 'a';
            let map = self.cb.map();
            let pos = Vec3::xz(map.width() as f32 * 4.0, map.height() as f32 * 4.0 + 300.0);
            if let Some(def) = self.defs.get("planescout") {
                self.cb.cheat_new_unit(def, Vec3::xz(pos.x + 150.0, pos.z));
            }
            self.log(&format!("assassin-test: ordinary raider squad {sid} ready with a mex lure"));
            return;
        }
        if test.starts_with("assassin-raider") && frame % 150 == 0 && frame >= 150 {
            let counts: Vec<_> = ["cloakraid", "hoverarty", "staticmex", "hoverriot"]
                .iter()
                .map(|name| {
                    let n = self
                        .cb
                        .friendly_units()
                        .iter()
                        .filter(|u| self.defs.name_of(**u) == *name && u.health() > 0.0)
                        .count();
                    format!("{name}={n}")
                })
                .collect();
            self.log(&format!("assassin-test: raider fixture {}", counts.join(" ")));
        }
        if test.starts_with("assassin-") && frame == 1500 {
            let orphaned = self
                .cb
                .friendly_units()
                .iter()
                .filter(|u| {
                    matches!(self.defs.name_of(**u), "spiderscout" | "vehscout")
                        && u.health() > 0.0
                        && !self.fighters.contains_key(&u.id())
                        && !self.war.new_units.contains(&u.id())
                        && self.role_of(u.id()).is_none()
                })
                .count();
            self.log(&format!("assassin-test: orphaned swarm scouts={orphaned}; {}", self.strikes_status()));
        }
        if matches!(test, "assassin-widow" | "assassin-no-widow" | "assassin-dart-opportunity") && frame == 90 {
            // UnitFinished has now run: detach the staged Fleas from automatic
            // picket claims and offer a real ordinary raider force to strikes.
            for u in self
                .cb
                .friendly_units()
                .into_iter()
                .filter(|u| {
                    self.defs.name_of(*u)
                        == if test == "assassin-dart-opportunity" { "vehscout" } else { "spiderscout" }
                })
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
        if matches!(test, "assassin-raiders" | "assassin-halberd-squad" | "assassin-halberd-squad-phantom") {
            let halberds = matches!(test, "assassin-halberd-squad" | "assassin-halberd-squad-phantom");
            if let Some(def) = self.defs.get(if halberds { "hoverassault" } else { "cloakraid" }) {
                for i in 0..if halberds { 5 } else { 8 } {
                    if let Some(u) = self.cb.cheat_new_unit(
                        def,
                        Vec3::xz(center.x + (i % 4) as f32 * 75.0 - 100.0, center.z - 500.0 + (i / 4) as f32 * 75.0),
                    ) {
                        if halberds {
                            springai::reserved::reserve(self.cb, u.id());
                        }
                    }
                }
            }
            return;
        }
        if matches!(test, "assassin-raider-lance" | "assassin-raider-guarded") {
            for (name, at) in [("hoverarty", pos), ("staticmex", Vec3::xz(center.x + 600.0, center.z - 500.0))] {
                if let Some(def) = self.defs.get(name) {
                    if let Some(u) = self.cb.cheat_new_unit(def, at) {
                        u.set_move_state(0, 0);
                        u.set_fire_state(2, 0);
                        springai::reserved::reserve(self.cb, u.id());
                        self.log(&format!("assassin-test: {name} {} bait at {},{}", u.id(), at.x, at.z));
                    }
                }
            }
            if test == "assassin-raider-guarded" {
                if let Some(def) = self.defs.get("hoverriot") {
                    if let Some(u) = self.cb.cheat_new_unit(def, Vec3::xz(pos.x + 100.0, pos.z)) {
                        u.set_move_state(0, 0);
                        u.set_fire_state(0, 0);
                        springai::reserved::reserve(self.cb, u.id());
                        self.log(&format!("assassin-test: Mace {} guards Lance", u.id()));
                    }
                }
            }
            return;
        }
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
        if matches!(test, "assassin-widow" | "assassin-no-widow" | "assassin-dart-opportunity") {
            let home = Vec3::xz(center.x, center.z - 350.0);
            let scout = if test == "assassin-dart-opportunity" { "vehscout" } else { "spiderscout" };
            if let Some(def) = self.defs.get(scout) {
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
            self.log(&format!("assassin-test: 20 ordinary {scout} await a strike opportunity"));
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
                if name == "cloaksnipe" {
                    // Keep the firing source at the seeded fix; normal initial
                    // idle orders must not walk the fixture back to its base.
                    u.set_fire_state(2, 0);
                    springai::reserved::reserve(self.cb, u.id());
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
            "assassin-dart" => Tactic::Dart,
            _ => return,
        };
        let Some(def) = self.defs.get(tactic.name()) else { return };
        // Start the Dart muster outside sniper reach; entering that range is
        // the committed dive, not part of assembling the test force.
        let back = if tactic == Tactic::Dart { 1500.0 } else { 700.0 };
        let home = Vec3::xz(center.x, center.z - back);
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
    fn strikes_lobster_scenario(&mut self, test: &str, frame: i32) {
        let map = self.cb.map();
        let center = Vec3::xz(map.width() as f32 * 4.0, map.height() as f32 * 4.0);
        let home = Vec3::xz(center.x, center.z - 690.0);
        if frame == 60 {
            self.cb.cheats_set_enabled(true);
            for (name, count) in [("amphfloater", 4), ("amphlaunch", 1)] {
                if let Some(def) = self.defs.get(name) {
                    for i in 0..count {
                        let at = if name == "amphlaunch" {
                            home
                        } else {
                            Vec3::xz(home.x + i as f32 * 30.0 - 45.0, home.z - 10.0)
                        };
                        if let Some(u) = self.cb.cheat_new_unit(def, at) {
                            springai::reserved::reserve(self.cb, u.id());
                        }
                    }
                }
            }
        }
        if frame == 90 && test == "assassin-lobster" {
            if let Some(def) = self.defs.get("planescout") {
                self.cb.cheat_new_unit(def, Vec3::xz(center.x + 100.0, center.z + 300.0));
            }
        }
        // Let normal intelligence ingest LOS before releasing the squad;
        // otherwise its first ordinary move can spend the launcher reload.
        if frame == 150 {
            let units: Vec<_> =
                self.cb.friendly_units().into_iter().filter(|u| self.defs.name_of(*u) == "amphfloater").collect();
            for u in &units {
                springai::reserved::release(self.cb, u.id());
                self.squad_remove_unit(*u);
                self.war.new_units.remove(&u.id());
            }
            let sid = self.squad_adopt(&units)[0];
            if let Some(u) = self.cb.friendly_units().into_iter().find(|u| self.defs.name_of(*u) == "amphlaunch") {
                springai::reserved::release(self.cb, u.id());
                self.misc_remove_unit(u);
                self.war.new_units.remove(&u.id());
                let mut f = Fighter::new(u, u.def().unwrap().cost(self.metal), self.team_id);
                f.squad = Some(sid);
                self.misc_add_support(f);
            }
            if test == "assassin-lobster-phantom" {
                self.sweeps_seed_phantom_fix(Vec3::xz(center.x, center.z + 300.0), 1);
            }
            self.log(&format!("assassin-test: Buoy squad {sid} with attached Lobster, no preseeded mission"));
        }
        if frame >= 150 && frame % 150 == 0 {
            self.log(&format!("assassin-test: Lobster {}", self.strikes_status()));
        }
    }

    /// Real artillery fires at a durable witness; an intentionally incomplete
    /// Flea wave nearby exercises the same staging and command routines.
    fn strikes_battery_scenario(&mut self, test: &str, frame: i32) {
        let map = self.cb.map();
        let home = Vec3::xz(map.width() as f32 * 4.0, map.height() as f32 * 4.0);
        if frame == 60 {
            self.cb.cheats_set_enabled(true);
            if test == "assassin-battery-observer" {
                if let Some(def) = self.defs.get("striderbantha") {
                    if let Some(u) = self.cb.cheat_new_unit(def, home) {
                        u.set_fire_state(0, 0);
                        springai::reserved::reserve(self.cb, u.id());
                    }
                }
                if let Some(def) = self.defs.get("spiderscout") {
                    for i in 0..8 {
                        self.cb.cheat_new_unit(
                            def,
                            Vec3::xz(home.x - 180.0 + (i % 4) as f32 * 60.0, home.z - 150.0 + (i / 4) as f32 * 60.0),
                        );
                    }
                }
            } else {
                let name = if test == "assassin-battery-tremor" { "tankheavyarty" } else { "tankarty" };
                if let Some(def) = self.defs.get(name) {
                    let at = Vec3::xz(home.x, home.z + def.max_weapon_range() * 0.85);
                    if let Some(u) = self.cb.cheat_new_unit(def, at) {
                        u.set_move_state(0, 0);
                        u.attack_area(home, 0.0, 0);
                        springai::reserved::reserve(self.cb, u.id());
                        self.log(&format!(
                            "battery-test: {name} {} fires from {:.0},{:.0} range={}",
                            u.id(),
                            at.x,
                            at.z,
                            def.max_weapon_range()
                        ));
                    }
                }
            }
        }
        if test != "assassin-battery-observer" {
            return;
        }
        if frame == 90 {
            self.strikes.wave = Some(Wave {
                tactic: Tactic::Flea,
                job: Job {
                    id: None,
                    pos: Vec3::xz(home.x, home.z + 2000.0),
                    last: frame,
                    count: 1,
                    value: 750.0,
                    name: "cloaksnipe".into(),
                    prey: Prey::GlassCannon,
                },
                needed: 20,
                members: Vec::new(),
                phase: Phase::Forming,
                since: frame,
                home,
                query: None,
            });
            for u in self
                .cb
                .friendly_units()
                .into_iter()
                .filter(|u| self.defs.name_of(*u) == "spiderscout")
                .collect::<Vec<_>>()
            {
                // UnitFinished assigned these fixture Fleas to pickets. Offer
                // ordinary raiders, just as the real recruitment path requires.
                self.assign_role(u, super::super::roles::Role::SoloRaider);
                self.strikes_adopt(u);
            }
        }
        if frame >= 90 && frame % 30 == 0 {
            if let Some(mut w) = self.strikes.wave.take() {
                w.members.retain(|id| self.fighters.get(id).is_some_and(|f| f.unit.health() > 0.0));
                if let Some(stage) = self.strike_stage(&w) {
                    w.home = stage;
                }
                self.strike_command(&w);
                self.strikes.wave = Some(w);
            }
        }
        // Deterministically exercise a nonlethal damage callback on an
        // incomplete wave too; real shells often kill a Flea outright.
        if frame == 120 {
            let unit = self
                .strikes
                .wave
                .as_ref()
                .and_then(|w| w.members.first())
                .and_then(|id| self.fighters.get(id))
                .map(|f| f.unit);
            if let Some(u) = unit {
                self.log("battery-test: inject nonlethal hit to test incomplete-wave evasion ownership");
                self.strikes_unit_damaged(u, None, Vec3::xz(0.0, 1.0));
            }
        }
        if frame == 150 {
            for (&id, &(goal, until)) in &self.strikes.evasion {
                if until <= frame {
                    continue;
                }
                if let Some(f) = self.fighters.get(&id) {
                    let kept = f.unit.current_commands().iter().any(|cmd| {
                        let p = cmd.params();
                        p.len() >= 3 && distance(Vec3::xz(p[0], p[2]), goal) < 5.0
                    });
                    self.log(&format!("battery-test: evasion survives muster command unit={id} kept={kept}"));
                }
            }
        }
        if frame >= 150 && frame % 150 == 0 {
            self.log(&format!("battery-test: {}", self.strikes_status()));
        }
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
