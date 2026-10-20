//! Development aid: a staged strike on an anti-nuke, set up with cheats by
//! the AI option `test`, without a war in the way. With `test=anti`, the AI
//! gets four Fusions at its base and an anti-nuke well off to the side
//! (out of sight of them, with turrets by it), and otherwise stands by.
//! With `test=bite`, its factories build nothing but the Widows wanted; it
//! gets a spider factory at once, and once its economy is going (a silo
//! makes the economy guard it with turrets) a nuke silo and a radar by the
//! enemy's base, that sees the Fusions but not the anti-nuke. Both have
//! their storage topped up every 10 s and do not resign. Put against each
//! other, the attacker should fire, be intercepted, find the anti-nuke with
//! Widows, bite it, and get its next missile through. With `test=eos`
//! instead, the attacker gets a radar by the enemy's base that sees the
//! Fusions and then goes, and a missile silo within reach of them: its Eos
//! should count each Fusion they hit gone, unseen, and strike each once.
//! With `test=drop` (hovers) or `test=dropbot` (bots), the attacker gets a
//! radar by the enemy's base and, every few minutes, four Charons and four
//! assaults and riots at home; any landing counts, walkable or not (see
//! `drops`), so each round flies a sortie to the enemy's buildings. With
//! `test=bertha`, the attacker gets five Fusions and a radar at home at
//! once, so its economy goes for a superweapon (a Bertha, mostly) soon.
//! With `test=colony`, it gets a ground constructor and a Charon (or with
//! `test=colonyair` an air constructor) to colonize the free metal far off
//! (see `economy::colonize`). With `test=aafort`, a passive defender gets
//! static and mobile anti-air around its base; with `test=aaprobe`, the
//! attacker keeps flying unarmed Charons over the enemy's base, to be shot
//! at by what it does not see (see `air_intel`; cheats name every attacker,
//! so the fixes are scored against where the shooters truly are). With
//! `test=fort` (or `test=radarfort`: a Lotus by each mex and a radar
//! farther out, which only the radar shadows) and `test=shadowprobe`, see
//! `shadow_probe`. With
//! `test=precision` (against `test=fort` or `test=radarfort`), the
//! attacker gets two each of Glaives, Scythes, Scorchers and Kodachis every
//! two minutes, as solo raiders, and a
//! radar toward the enemy: their precision missions (see
//! `handlers::precision`) are all that should hurt the fort.

use springai::Vec3;

use crate::Ai;
use crate::military::missiles;
use crate::util::{OPTION_SHIFT_KEY, distance, get_directional_point};

/// When the attacker gets its silo, and its radar for `eos`.
const SILO_FRAME: i32 = 30 * 300;
const RADAR_FRAME: i32 = 30 * 100;

/// How often probing Charons come.
const PROBE_ROUND: i32 = 30 * 40;

/// How many metal spots the fort holds, and the defenses by them.
const FORT_SPOTS: usize = 8;
const FORT_GUNS: [&str; 8] = [
    "turretlaser",
    "turretmissile",
    "turretriot",
    "turretlaser",
    "turretheavylaser",
    "turretemp",
    "turretgauss",
    "turretheavy",
];

/// When precision raiders come, and how often.
const PRECISION_FRAME: i32 = 30 * 60;
const PRECISION_ROUND: i32 = 30 * 120;

/// When the drop rounds start, and how often they come.
const DROP_FRAME: i32 = 30 * 60;
const DROP_ROUND: i32 = 30 * 180;

impl Ai {
    pub fn strike_test_update(&mut self, frame: i32) {
        if self.test.as_deref() == Some("shadowprobe") {
            self.shadow_probe_update(frame);
        }
        if frame != 60 && frame % 300 != 0 {
            return;
        }
        let Some(test) = self.test.clone() else { return };
        let base = self.graph.get_ally_center();
        let toward = self.graph.get_enemy_center();
        let ahead = |d: f32| get_directional_point(base, toward, d);
        let map = self.cb.map();
        let (w, h) = (map.width() as f32 * 8.0, map.height() as f32 * 8.0);
        // The enemy's base, before we have seen it: across the map from ours.
        let enemy = Vec3::xz(w - base.x, h - base.z);
        // Off to the side of the way from the enemy, on the map.
        let (dx, dz) = (toward.x - base.x, toward.z - base.z);
        let n = (dx * dx + dz * dz).sqrt().max(1.0);
        let side = [1.0, -1.0]
            .map(|s| Vec3::xz(base.x - s * dz / n * 1800.0, base.z + s * dx / n * 1800.0))
            .into_iter()
            .find(|p| p.x > 100.0 && p.z > 100.0 && p.x < w - 100.0 && p.z < h - 100.0)
            .unwrap_or_else(|| ahead(1800.0));
        let spawn: Vec<(&str, Vec3)> = match (test.as_str(), frame) {
            ("anti", 60) => vec![
                ("staticantinuke", side),
                ("turretheavylaser", Vec3::xz(side.x + 200.0, side.z)),
                ("turretheavylaser", Vec3::xz(side.x - 200.0, side.z)),
                ("turretriot", Vec3::xz(side.x, side.z + 200.0)),
                ("energyfusion", ahead(300.0)),
                ("energyfusion", ahead(500.0)),
                ("energyfusion", ahead(-300.0)),
                ("energyfusion", ahead(-500.0)),
            ],
            ("bite", 60) => vec![("factoryspider", ahead(400.0))],
            ("bite", SILO_FRAME) => vec![("staticnuke", ahead(-500.0)), ("staticradar", enemy)],
            ("eos", RADAR_FRAME) => vec![("staticradar", enemy)],
            ("eos", SILO_FRAME) => vec![(missiles::SILO, get_directional_point(enemy, base, 2500.0))],
            ("bertha", 60) => {
                let mut units = [-600.0, -400.0, -200.0, 200.0, 400.0]
                    .map(|d| ("energyfusion", Vec3::xz(base.x - dz / n * d, base.z + dx / n * d)))
                    .to_vec();
                // Superweapons wait for a radar.
                units.push(("staticradar", ahead(300.0)));
                units
            }
            ("aafort", 60) => {
                let aside = |d: f32, s: f32| {
                    let p = ahead(d);
                    Vec3::xz(p.x - dz / n * s, p.z + dx / n * s)
                };
                vec![
                    ("turretaafar", aside(700.0, 0.0)),
                    ("turretaaheavy", aside(-200.0, 900.0)),
                    ("turretaalaser", aside(400.0, 500.0)),
                    ("turretaalaser", aside(400.0, -500.0)),
                    ("turretaaflak", aside(-400.0, -600.0)),
                    ("vehaa", aside(200.0, -1300.0)),
                    ("vehaa", aside(250.0, -1350.0)),
                    ("vehaa", aside(150.0, -1250.0)),
                ]
            }
            ("fort", 60) => {
                let mut spots: Vec<Vec3> = self.graph.metal_spots.iter().map(|m| m.position).collect();
                spots.sort_by(|a, b| distance(*a, base).total_cmp(&distance(*b, base)));
                let mut units = vec![("planescout", ahead(-100.0)), ("planescout", ahead(-200.0))];
                for (k, &s) in spots.iter().take(FORT_SPOTS).enumerate() {
                    let gun = get_directional_point(s, toward, 140.0);
                    let (gx, gz) = ((gun.x - s.x) / 140.0, (gun.z - s.z) / 140.0);
                    units.push(("staticmex", s));
                    units.push((FORT_GUNS[k % FORT_GUNS.len()], gun));
                    units.push(("energysolar", Vec3::xz(s.x - gz * 110.0, s.z + gx * 110.0)));
                    units.push(("energysolar", Vec3::xz(gun.x + gz * 90.0 + gx * 50.0, gun.z - gx * 90.0 + gz * 50.0)));
                    // A factory beside some, to hide behind.
                    if k % 3 == 1 {
                        units.push((
                            "factorycloak",
                            Vec3::xz(gun.x - gz * 160.0 + gx * 60.0, gun.z + gx * 160.0 + gz * 60.0),
                        ));
                    }
                }
                units
            }
            ("precision", f) if f >= PRECISION_FRAME && (f - PRECISION_FRAME) % PRECISION_ROUND == 0 => {
                let mut units: Vec<(&str, Vec3)> = Vec::new();
                for name in ["cloakraid", "cloakheavyraid", "vehraid", "tankraid"] {
                    units.extend([(name, ahead(-100.0)), (name, ahead(-160.0))]);
                }
                if f == PRECISION_FRAME {
                    units.push(("staticradar", get_directional_point(enemy, base, 1500.0)));
                }
                units
            }
            ("radarfort", 60) => {
                let mut spots: Vec<Vec3> = self.graph.metal_spots.iter().map(|m| m.position).collect();
                spots.sort_by(|a, b| distance(*a, base).total_cmp(&distance(*b, base)));
                let mut units = vec![("planescout", ahead(-100.0)), ("planescout", ahead(-200.0))];
                for &s in spots.iter().take(FORT_SPOTS) {
                    units.push(("staticmex", s));
                    units.push(("turretlaser", get_directional_point(s, toward, 120.0)));
                    units.push(("staticradar", get_directional_point(s, toward, 270.0)));
                }
                units
            }
            ("aaprobe", f) if f % PROBE_ROUND == 0 && f > 0 => vec![("gunshiptrans", ahead(-200.0)); 2],
            ("colony", 60) => vec![("cloakcon", ahead(-150.0)), ("gunshiptrans", ahead(-250.0))],
            ("colonyair", 60) => vec![("gunshipcon", ahead(-150.0))],
            ("drop" | "dropbot", f) if f >= DROP_FRAME && (f - DROP_FRAME) % DROP_ROUND == 0 => {
                let cargo = if test == "drop" { ["hoverassault", "hoverriot"] } else { ["cloakassault", "cloakriot"] };
                let mut units = vec![("gunshiptrans", ahead(-200.0)); 4];
                units.extend((0..4).map(|k| (cargo[k % 2], ahead(-100.0))));
                if f == DROP_FRAME {
                    units.push(("staticradar", enemy));
                }
                units
            }
            _ => Vec::new(),
        };
        if frame == 60 {
            self.cb.cheats_set_enabled(true);
        }
        if test == "eos" && frame == RADAR_FRAME + 1500 {
            let radars = self.cb.team_units().into_iter().filter(|u| self.defs.name_of(*u) == "staticradar");
            for r in radars.filter(|r| distance(r.pos(), enemy) < 1500.0) {
                r.self_destruct(0);
                self.log("strike-test: the radar goes");
            }
        }
        for (name, at) in spawn {
            let Some(def) = self.defs.get(name) else { continue };
            let site = self.cb.map().find_closest_build_site(def, at, 1200.0, 4, 0);
            let unit = self.cb.cheat_new_unit(def, if site.x >= 0.0 { site } else { at });
            // A short walk, so it goes idle and joins its handler as if
            // built (cheated units get no idle event otherwise).
            if let Some(u) = unit.filter(|_| def.speed() > 0.0) {
                let p = u.pos();
                u.move_to(Vec3::new(p.x + 40.0, p.y, p.z + 40.0), 0);
            }
            // Precision raiders go solo (see `handlers::precision`).
            if let Some(u) = unit.filter(|_| test == "precision" && def.speed() > 0.0) {
                let fighter = crate::military::Fighter::new_raider(u, def.cost(self.metal), self.team_id);
                self.raider_add_solo_raider(fighter);
            }
            let p = unit.map(|u| u.pos());
            self.log(&format!("strike-test: {name} at {:?}", p.map(|p| (p.x as i32, p.z as i32))));
        }
        if frame % 300 == 0 {
            for res in [self.metal, self.energy] {
                self.cb.cheat_resource(res, 20000.0);
            }
        }
        // Owls keep watch over the fort, and the commander holds its fire
        // (it would shoot the probes; losing it ends the game).
        if (test == "fort" || test == "radarfort") && frame == 300 {
            for c in self.cb.team_units().into_iter().filter(|u| self.defs.name_of(*u).starts_with("dyn")) {
                c.set_fire_state(0, 0);
                c.set_move_state(0, 0);
            }
        }
        if (test == "fort" || test == "radarfort") && frame == 60 {
            let mut spots: Vec<Vec3> = self.graph.metal_spots.iter().map(|m| m.position).collect();
            spots.sort_by(|a, b| distance(*a, base).total_cmp(&distance(*b, base)));
            spots.truncate(FORT_SPOTS);
            let owls = self.cb.team_units().into_iter().filter(|u| self.defs.name_of(*u) == "planescout");
            for (k, o) in owls.enumerate() {
                for (j, s) in spots.iter().enumerate() {
                    o.patrol_to(spots[(j + k * 3) % spots.len()], if j == 0 { 0 } else { OPTION_SHIFT_KEY });
                    let _ = s;
                }
            }
        }
        // Probing Charons fly over the enemy's base, this way and that.
        if test == "aaprobe" && frame % 300 == 0 {
            let charons = self.cb.team_units().into_iter().filter(|u| self.defs.name_of(*u) == "gunshiptrans");
            for (k, c) in charons.enumerate() {
                let s = if (k + (frame / 300) as usize).is_multiple_of(2) { 1.0 } else { -1.0 };
                let across = Vec3::xz(enemy.x - s * dz / n * 1500.0, enemy.z + s * dx / n * 1500.0);
                c.move_to(across, 0);
                c.move_to(Vec3::xz(enemy.x + s * dz / n * 1500.0, enemy.z - s * dx / n * 1500.0), OPTION_SHIFT_KEY);
            }
        }
        // The Widows wanted, from an idle spider factory.
        let spiders = self.fac.factories.values().map(|f| f.unit).find(|u| self.defs.name_of(*u) == "factoryspider");
        if test == "bite"
            && let Some(fac) = spiders.filter(|f| f.current_commands().is_empty())
            && self.scouts_want_widow()
            && let Some(widow) = self.defs.get(crate::military::scouts::WIDOW)
        {
            fac.build(widow, fac.pos(), 0, 0);
            self.scouts_ordered();
            self.log("strike-test: a Widow ordered");
        }
    }
}
