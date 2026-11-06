//! `airstrike-wind` versus `airstrike-farm`: real factory procurement and
//! Phoenix bombing of a ZKGBAI wind farm. `airstrike-odin` / `airstrike-singu`
//! exercise deep thermite procurement and optional Sparrow support;
//! `airstrike-odin` / `airstrike-factory` burns a factory and tracks the
//! Odin's flight home. `airstrike-turns` measures real turning circles.
//! `airstrike-likhos` / `airstrike-shieldball` sizes Likho packages for an
//! Aspis; `airstrike-phoenixes` / `airstrike-outpost` burns a mex among its
//! Solars with spare Phoenixes; `airstrike-odins` / `airstrike-turrets` burns
//! a Gauss and a Stinger with spare Odins. `airstrike-raptors` /
//! `airstrike-revenants`: a Raptor pair on Revenants over an outpost.
mod carpet;
mod flight;
mod gunships;
mod hunt;
mod opportunity;
mod targeting;
use super::Plane;
use crate::Ai;
use crate::util::get_directional_point;
use springai::Vec3;

impl Ai {
    pub(crate) fn airstrike_scenario(&mut self, frame: i32) {
        let Some(test) = self.test.clone().filter(|s| s.starts_with("airstrike-")) else { return };
        if test.starts_with("airstrike-opportunity") {
            self.airstrike_opportunity_scenario(&test, frame);
            return;
        }
        if test.starts_with("airstrike-targeting") {
            self.airstrike_targeting_scenario(&test, frame);
            return;
        }
        if test.starts_with("airstrike-carpet") {
            self.airstrike_carpet_scenario(&test, frame);
            return;
        }
        if matches!(
            test.as_str(),
            "airstrike-raptors"
                | "airstrike-raptors-likho"
                | "airstrike-raptors-pad"
                | "airstrike-raptors-pad-lend"
                | "airstrike-revenants"
        ) {
            self.airstrike_gunship_scenario(&test, frame);
            return;
        }
        if test.starts_with("airstrike-hunt") {
            self.airstrike_hunt_scenario(&test, frame);
            return;
        }
        if test == "airstrike-turns" {
            self.airstrike_turns_scenario(frame);
            return;
        }
        if test == "airstrike-odin" && frame % 10 == 0 {
            self.airstrike_track(frame);
        }
        if test == "airstrike-strider" {
            self.airstrike_strider_frame(frame);
        }
        if test == "airstrike-factory" && frame % 150 == 0 {
            for u in self.cb.team_units() {
                let name = self.defs.name_of(u);
                if name.starts_with("factory") {
                    self.log(&format!(
                        "airstrike-test: factory {name} {} hp={:.0}/{:.0}",
                        u.id(),
                        u.health(),
                        u.max_health()
                    ));
                }
            }
        }
        let map = self.cb.map();
        let center = Vec3::xz(map.width() as f32 * 4.0, map.height() as f32 * 4.0);
        let site = if test.starts_with("airstrike-wave") {
            // Comet can swap the teams' start corners. Both sides infer the
            // attacker's corner from their own commander, so this remains a
            // shallow-wave fixture instead of accidentally testing deep vetoes.
            let base = self
                .cb
                .team_units()
                .into_iter()
                .find(|u| self.defs.name_of(*u).starts_with("dyn"))
                .map_or(self.graph.get_ally_center(), |u| u.pos());
            let west = if test == "airstrike-wave" { base.x < center.x } else { base.x > center.x };
            if west { Vec3::xz(center.x * 0.3, center.z * 1.5) } else { Vec3::xz(center.x * 2.0 - 2000.0, 1000.0) }
        } else {
            Vec3::xz(center.x, center.z + 400.0)
        };
        // Spawn close targets just before release, so the ordinary friendly
        // commander/army cannot clear the fixture during the held-fleet period.
        if frame == if test == "airstrike-wave-targets" { 870 } else { 60 } {
            self.cb.cheats_set_enabled(true);
            let base = self.graph.get_ally_center();
            let mut spawn = Vec::new();
            match test.as_str() {
                "airstrike-wave" => {
                    for (i, name) in [
                        "bomberprec",
                        "bomberprec",
                        "bomberprec",
                        "bomberprec",
                        "bomberprec",
                        "bomberheavy",
                        "bomberheavy",
                        "bomberriot",
                        "bomberassault",
                    ]
                    .into_iter()
                    .enumerate()
                    {
                        spawn.push((name, Vec3::xz(base.x + (i % 3) as f32 * 60.0, base.z + (i / 3) as f32 * 60.0)));
                    }
                    spawn.extend([
                        ("staticrearm", get_directional_point(base, center, 400.0)),
                        ("energyfusion", get_directional_point(base, center, -300.0)),
                    ]);
                }
                "airstrike-wave-targets" => {
                    spawn.push(("hoverarty", site));
                    for i in 0..4 {
                        spawn.push((
                            "shieldassault",
                            Vec3::xz(site.x + 600.0 + (i % 2) as f32 * 70.0, site.z + (i / 2) as f32 * 70.0),
                        ));
                    }
                    for i in 0..8 {
                        spawn.push((
                            "energywind",
                            Vec3::xz(site.x + 1200.0 + (i % 4) as f32 * 60.0, site.z + (i / 4) as f32 * 60.0),
                        ));
                    }
                    spawn.push(("energyfusion", Vec3::xz(site.x + 600.0, site.z + 700.0)));
                    spawn.push(("staticradar", Vec3::xz(site.x, site.z + 700.0)));
                }
                "airstrike-farm" => {
                    for i in 0..12 {
                        spawn.push((
                            "energywind",
                            Vec3::xz(site.x + (i % 4) as f32 * 70.0, site.z + (i / 4) as f32 * 70.0),
                        ));
                    }
                }
                "airstrike-priority-target" => spawn.push(("hoverarty", site)),
                "airstrike-tough-target" => spawn.push(("tankassault", site)),
                "airstrike-ravagers" | "airstrike-ravagers-aa" => {
                    for dx in [-40.0, 0.0, 40.0] {
                        spawn.push(("vehassault", Vec3::xz(site.x + dx, site.z)));
                    }
                    if test == "airstrike-ravagers-aa" {
                        spawn.push(("turretaalaser", get_directional_point(site, base, 900.0)));
                    }
                }
                "airstrike-fencer" => spawn.push(("vehsupport", site)),
                "airstrike-shieldball" => {
                    spawn.push(("shieldshield", site));
                    for i in 0..4 {
                        spawn.push((
                            "shieldassault",
                            Vec3::xz(site.x + 70.0 * (i % 2) as f32, site.z + 70.0 * (i / 2) as f32),
                        ));
                    }
                }
                "airstrike-outpost" => {
                    spawn.push(("staticmex", site));
                    for (dx, dz) in [(90.0, 0.0), (-90.0, 0.0), (0.0, 90.0), (0.0, -90.0)] {
                        spawn.push(("energysolar", Vec3::xz(site.x + dx, site.z + dz)));
                    }
                    for dx in [-170.0, 170.0] {
                        spawn.push(("energywind", Vec3::xz(site.x + dx, site.z + 90.0)));
                    }
                }
                "airstrike-turrets" => {
                    spawn.push(("turretgauss", site));
                    spawn.push(("turretheavylaser", Vec3::xz(site.x + 400.0, site.z)));
                }
                "airstrike-likhos"
                | "airstrike-likho-fencer"
                | "airstrike-likho-aa"
                | "airstrike-padrepair"
                | "airstrike-phoenixes"
                | "airstrike-odins" => {
                    let (plane, n) = match test.as_str() {
                        "airstrike-likhos" => ("bomberheavy", 5),
                        // Two Likhos through AA come home hurt to one pad,
                        // with a constructor at hand for its caretaker.
                        "airstrike-padrepair" => ("bomberheavy", 2),
                        "airstrike-phoenixes" => ("bomberriot", 4),
                        "airstrike-odins" => ("bomberassault", 2),
                        _ => ("bomberheavy", 1),
                    };
                    for i in 0..n {
                        spawn.push((plane, Vec3::xz(base.x + 40.0 * i as f32, base.z + 100.0)));
                    }
                    spawn.extend([
                        ("staticrearm", get_directional_point(base, center, 400.0)),
                        ("energyfusion", get_directional_point(base, center, -300.0)),
                    ]);
                    if test == "airstrike-padrepair" {
                        spawn.push(("cloakcon", get_directional_point(base, center, 250.0)));
                    }
                }
                "airstrike-singu" => spawn.push(("energysingu", site)),
                "airstrike-factory" => spawn.push(("factoryshield", site)),
                // The hub works only on a grid of 50 energy: two fusions in
                // link range (hub 50 + fusion 150), away from the Dante, which
                // goes up beside the attacker's observer radar at site - 300.
                "airstrike-strider" => spawn.extend([
                    ("striderhub", site),
                    ("energyfusion", Vec3::xz(site.x + 90.0, site.z - 90.0)),
                    ("energyfusion", Vec3::xz(site.x + 90.0, site.z + 90.0)),
                ]),
                "airstrike-aa" => {
                    spawn.push(("energysingu", site));
                    for (dx, dz) in [(-240.0, 0.0), (240.0, 0.0), (0.0, -240.0), (0.0, 240.0)] {
                        spawn.push(("turretaafar", Vec3::xz(site.x + dx, site.z + dz)));
                    }
                }
                "airstrike-wind" | "airstrike-odin" => {
                    spawn.extend([
                        ("factoryplane", get_directional_point(base, center, 350.0)),
                        ("staticrearm", get_directional_point(base, center, 650.0)),
                        ("staticradar", Vec3::xz(site.x - 300.0, site.z)),
                        ("energyfusion", get_directional_point(base, center, -300.0)),
                    ]);
                }
                _ => return,
            }
            for (name, pos) in spawn {
                if let Some(d) = self.defs.get(name) {
                    let p = map.find_closest_build_site(d, pos, 250.0, 1, 0);
                    if let Some(u) = self.cb.cheat_new_unit(d, if p.x >= 0.0 { p } else { pos }) {
                        // These fixtures exercise delivery, not survival of the
                        // close observer radar against a Lance or Fencer.
                        if matches!(
                            test.as_str(),
                            "airstrike-wave-targets"
                                | "airstrike-priority-target"
                                | "airstrike-tough-target"
                                | "airstrike-fencer"
                                | "airstrike-shieldball"
                                | "airstrike-turrets"
                                | "airstrike-ravagers"
                                | "airstrike-ravagers-aa"
                        ) && name != "turretaalaser"
                        {
                            springai::reserved::as_owner(|| {
                                u.stop(0);
                                u.set_fire_state(0, 0);
                                u.set_move_state(0, 0);
                            });
                            // Hold-fire alone still permits explicit attack
                            // orders from initial unit-idle registration.
                            springai::reserved::reserve(self.cb, u.id());
                        }
                        // The fixture alone drives the hub (see `airstrike_strider_frame`).
                        if test == "airstrike-strider" && name == "striderhub" {
                            springai::reserved::reserve(self.cb, u.id());
                        }
                        if test == "airstrike-wave" && Plane::of(name).is_some() {
                            springai::reserved::as_owner(|| {
                                u.stop(0);
                                u.set_fire_state(0, 0);
                                u.set_move_state(0, 0);
                            });
                            springai::reserved::reserve(self.cb, u.id());
                        }
                        self.log(&format!(
                            "airstrike-test: spawned {name} {} at {:.0},{:.0}",
                            u.id(),
                            u.pos().x,
                            u.pos().z
                        ));
                    }
                }
            }
        }
        // Cheats create units before their ordinary initialization callbacks
        // settle. Apply passive state after creation, and add the observer only
        // afterwards so a Lance cannot begin charging at it on its spawn frame.
        if frame >= 90
            && frame % 30 == 0
            && matches!(
                test.as_str(),
                "airstrike-wave"
                    | "airstrike-wave-targets"
                    | "airstrike-priority-target"
                    | "airstrike-tough-target"
                    | "airstrike-fencer"
                    | "airstrike-shieldball"
                    | "airstrike-turrets"
                    | "airstrike-ravagers"
                    | "airstrike-ravagers-aa"
            )
        {
            for u in self.cb.team_units() {
                if springai::reserved::is_reserved(self.cb, u.id()) {
                    springai::reserved::as_owner(|| {
                        u.stop(0);
                        u.set_fire_state(0, 0);
                        u.set_move_state(0, 0);
                    });
                }
            }
        }
        if test == "airstrike-wave" && frame == 899 {
            for u in self.cb.team_units() {
                if Plane::of(self.defs.name_of(u)).is_some() {
                    springai::reserved::release(self.cb, u.id());
                }
            }
        }
        if test == "airstrike-wave"
            && frame == 150
            && let Some(d) = self.defs.get("staticradar")
        {
            for (x, z) in [(300.0, -300.0), (900.0, 1000.0), (1500.0, 0.0)] {
                self.cb.cheat_new_unit(d, Vec3::xz(site.x + x, site.z + z));
            }
        }
        if frame == 150
            && matches!(
                test.as_str(),
                "airstrike-likhos"
                    | "airstrike-likho-fencer"
                    | "airstrike-likho-aa"
                    | "airstrike-padrepair"
                    | "airstrike-phoenixes"
                    | "airstrike-odins"
            )
            && let Some(d) = self.defs.get("staticradar")
        {
            self.cb.cheat_new_unit(d, Vec3::xz(site.x - 300.0, site.z));
            if matches!(test.as_str(), "airstrike-likho-aa" | "airstrike-padrepair") {
                self.cb.cheat_new_unit(d, get_directional_point(site, self.graph.get_ally_center(), -600.0));
            }
        }
        // Each Likho's health, ammo state (Zero-K `noammo`: 3 = repairing on
        // the pad) and place, and the caretakers: what the pad time costs.
        if test == "airstrike-padrepair" && frame % 150 == 0 {
            let mut line = String::new();
            for u in self.cb.team_units() {
                let name = self.defs.name_of(u);
                if name == "bomberheavy" || name == "staticcon" {
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
                }
            }
            self.log(&format!("airstrike-test: pads{line}"));
            if frame % 600 == 0 {
                let jobs = self.eco_nano_debug();
                self.log(&format!("airstrike-test: jobs{jobs}"));
            }
        }
        if frame % 300 == 0 {
            for r in [self.metal, self.energy] {
                self.cb.cheat_resource(r, 20000.0);
            }
            if matches!(
                test.as_str(),
                "airstrike-wave-targets"
                    | "airstrike-farm"
                    | "airstrike-singu"
                    | "airstrike-aa"
                    | "airstrike-priority-target"
                    | "airstrike-tough-target"
                    | "airstrike-fencer"
                    | "airstrike-shieldball"
                    | "airstrike-outpost"
                    | "airstrike-turrets"
                    | "airstrike-ravagers"
                    | "airstrike-ravagers-aa"
            ) {
                if matches!(test.as_str(), "airstrike-outpost" | "airstrike-turrets" | "airstrike-shieldball") {
                    // Which buildings stand, and how hurt: the value model's
                    // kills and partial damage, per pass.
                    let mut line = String::new();
                    for u in self.cb.team_units() {
                        let name = self.defs.name_of(u);
                        if Plane::of(name).is_none() && !name.starts_with("dyn") && !name.starts_with("factory") {
                            line.push_str(&format!(" {name}:{:.0}/{:.0}", u.health(), u.max_health()));
                        }
                    }
                    self.log(&format!("airstrike-test: standing{line}"));
                }
                let alive = self
                    .cb
                    .team_units()
                    .iter()
                    .filter(|u| {
                        matches!(
                            self.defs.name_of(**u),
                            "energyfusion"
                                | "staticradar"
                                | "energywind"
                                | "energysingu"
                                | "tankassault"
                                | "hoverarty"
                                | "vehsupport"
                                | "shieldshield"
                                | "shieldassault"
                                | "vehassault"
                                | "staticmex"
                                | "energysolar"
                                | "turretgauss"
                                | "turretheavylaser"
                        )
                    })
                    .count();
                self.log(&format!("airstrike-test: targets remaining={alive}"));
            }
        }
    }
}
