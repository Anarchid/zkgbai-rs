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

use springai::Vec3;

use crate::Ai;
use crate::military::missiles;
use crate::util::{distance, get_directional_point};

/// When the attacker gets its silo, and its radar for `eos`.
const SILO_FRAME: i32 = 30 * 300;
const RADAR_FRAME: i32 = 30 * 100;

impl Ai {
    pub fn strike_test_update(&mut self, frame: i32) {
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
            let p = unit.map(|u| u.pos());
            self.log(&format!("strike-test: {name} at {:?}", p.map(|p| (p.x as i32, p.z as i32))));
        }
        if frame % 300 == 0 {
            for res in [self.metal, self.energy] {
                self.cb.cheat_resource(res, 20000.0);
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
