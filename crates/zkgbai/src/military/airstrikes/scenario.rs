//! `airstrike-wind` versus `airstrike-farm`: real factory procurement and
//! Phoenix bombing of a ZKGBAI wind farm. `airstrike-odin` / `airstrike-singu`
//! exercise a deep thermite strike and its dedicated Sparrow escort.
use crate::Ai;
use crate::util::get_directional_point;
use springai::Vec3;

impl Ai {
    pub(crate) fn airstrike_scenario(&mut self, frame: i32) {
        let Some(test) = self.test.clone().filter(|s| s.starts_with("airstrike-")) else { return };
        let map = self.cb.map();
        let center = Vec3::xz(map.width() as f32 * 4.0, map.height() as f32 * 4.0);
        let site = Vec3::xz(center.x, center.z + 400.0);
        if frame == 60 {
            self.cb.cheats_set_enabled(true);
            let base = self.graph.get_ally_center();
            let mut spawn = Vec::new();
            match test.as_str() {
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
                "airstrike-likhos" | "airstrike-likho-fencer" => {
                    for i in 0..if test == "airstrike-likho-fencer" { 1 } else { 5 } {
                        spawn.push(("bomberheavy", Vec3::xz(base.x + 40.0 * i as f32, base.z + 100.0)));
                    }
                    spawn.extend([
                        ("staticrearm", get_directional_point(base, center, 400.0)),
                        ("staticradar", Vec3::xz(site.x - 300.0, site.z)),
                        ("energyfusion", get_directional_point(base, center, -300.0)),
                    ]);
                }
                "airstrike-singu" => spawn.push(("energysingu", site)),
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
                            "airstrike-priority-target"
                                | "airstrike-tough-target"
                                | "airstrike-fencer"
                                | "airstrike-shieldball"
                        ) {
                            u.set_fire_state(0, 0);
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
        if frame % 300 == 0 {
            for r in [self.metal, self.energy] {
                self.cb.cheat_resource(r, 20000.0);
            }
            if matches!(
                test.as_str(),
                "airstrike-farm"
                    | "airstrike-singu"
                    | "airstrike-aa"
                    | "airstrike-priority-target"
                    | "airstrike-tough-target"
                    | "airstrike-fencer"
                    | "airstrike-shieldball"
            ) {
                let alive = self
                    .cb
                    .team_units()
                    .iter()
                    .filter(|u| {
                        matches!(
                            self.defs.name_of(**u),
                            "energywind"
                                | "energysingu"
                                | "tankassault"
                                | "hoverarty"
                                | "vehsupport"
                                | "shieldshield"
                                | "shieldassault"
                        )
                    })
                    .count();
                self.log(&format!("airstrike-test: targets remaining={alive}"));
            }
        }
    }
}
