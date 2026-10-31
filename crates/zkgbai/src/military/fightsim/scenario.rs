//! Development aid: the veto's own case staged by the AI option `test`.
//!
//! `test=veto-scalpels`: a passive defender gets five Scalpels in a row
//! across the way halfway to the enemy (the map's middle on a mirrored
//! map), and a factory behind its base (an AI team with no factory or
//! constructor is out); its commander holds its fire behind the base. The
//! Scalpels get no orders: ZK's tactical AI has idle units engage and skirm
//! as they would in a real game.
//!
//! `test=veto-knight` (one Knight) or `test=veto-knights` (three): the
//! attacker plays as ever, and gets them by its base, made an attacking
//! squad at [`ADOPT_FRAME`], with an Owl parked short of the middle so it
//! sees the Scalpels before it walks into their range. The veto should turn
//! a lone Knight back before the first volley and keep it back; three
//! Knights lose less than the veto's 75% in its 15 s and go in. Read the
//! `k=fightsim` verdict and release records, and the `fightsim-test:` log
//! lines every 5 s: the Knights' health and distance to the nearest
//! Scalpel, and the Scalpels' health as seen.

use springai::Vec3;

use crate::Ai;
use crate::util::{distance, get_directional_point};

/// When the Knights and the Owl come, and when the Knights become a squad.
const GIVE_FRAME: i32 = 60;
pub const ADOPT_FRAME: i32 = 150;
/// The Knights' gift; the Scalpels' is in `strike_test` with the other
/// passive defenders.
const KNIGHT: &str = "cloakassault";
pub const SCALPEL: &str = "hoverskirm";

impl Ai {
    /// Our base (the commander's place) and the enemy's, guessed across the
    /// map (mirrored) when not known.
    fn veto_scenario_line(&self) -> (Vec3, Vec3) {
        let com =
            (self.cb.team_units().into_iter()).find(|u| self.defs.name_of(*u).starts_with("dyn")).map(|u| u.pos());
        let base = com.filter(|p| p.x > 0.0 || p.z > 0.0).unwrap_or_else(|| self.graph.get_ally_center());
        let map = self.cb.map();
        let (w, h) = (map.width() as f32 * 8.0, map.height() as f32 * 8.0);
        (base, Vec3::xz(w - base.x, h - base.z))
    }

    pub(crate) fn veto_scenario(&mut self, frame: i32) {
        let knights = match self.test.as_deref() {
            Some("veto-knight") => 1,
            Some("veto-knights") => 3,
            _ => return,
        };
        if frame == GIVE_FRAME {
            let (base, enemy) = self.veto_scenario_line();
            let half = distance(base, enemy) / 2.0;
            self.cb.cheats_set_enabled(true);
            let gifts = (0..knights)
                .map(|k| (KNIGHT, get_directional_point(base, enemy, 350.0 - 40.0 * k as f32)))
                .chain([("planescout", get_directional_point(base, enemy, half - 300.0))]);
            for (name, at) in gifts {
                let Some(def) = self.defs.get(name) else { continue };
                let unit = self.cb.cheat_new_unit(def, at);
                // Out of the handlers' hands until adopted (the Owl for good).
                if let Some(u) = unit {
                    springai::reserved::reserve(self.cb, u.id());
                }
                self.log(&format!(
                    "fightsim-test: {name} at ({:.0}, {:.0}){}",
                    at.x,
                    at.z,
                    if unit.is_none() { ": refused" } else { "" }
                ));
            }
            self.cb.cheats_set_enabled(false);
        }
        if frame == ADOPT_FRAME {
            let units: Vec<_> = self.cb.team_units().into_iter().filter(|u| self.defs.name_of(*u) == KNIGHT).collect();
            for u in &units {
                springai::reserved::release(self.cb, u.id());
                self.squad_remove_unit(*u);
                self.war.new_units.remove(&u.id());
            }
            let squads = self.squad_adopt(&units);
            self.log(&format!("fightsim-test: {} Knights adopted as squads {squads:?}", units.len()));
        }
        if frame > ADOPT_FRAME && frame % 150 == 0 {
            let scalpels: Vec<(Vec3, f32)> = (self.war.targets.values())
                .filter(|e| !e.is_dead && e.ud.is_some_and(|d| self.defs.name(d) == SCALPEL))
                .map(|e| (e.position, e.get_health()))
                .collect();
            let knights: Vec<String> = (self.cb.team_units().into_iter())
                .filter(|u| self.defs.name_of(*u) == KNIGHT)
                .map(|u| {
                    let p = u.pos();
                    let near = scalpels.iter().map(|s| distance(s.0, p)).fold(f32::INFINITY, f32::min);
                    let sid = self.fighters.get(&u.id()).and_then(|f| f.squad);
                    format!("{} hp {:.0} near {:.0} squad {sid:?}", u.id(), u.health(), near)
                })
                .collect();
            let hp: Vec<String> = scalpels.iter().map(|s| format!("{:.2}", s.1)).collect();
            self.log(&format!(
                "fightsim-test: t {} knights [{}] scalpels seen {} health [{}]",
                frame / 30,
                knights.join("; "),
                scalpels.len(),
                hp.join(" ")
            ));
        }
    }
}
