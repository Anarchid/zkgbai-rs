//! Reproducible controller smoke test: `test=assassin-halberd` (or dirtbag,
//! flea) versus `test=assassin-target` on a flat map. A single approximate
//! fix is seeded; the target must actually be found, decloaked and killed.
use super::*;
impl Ai {
    pub(crate) fn strikes_scenario(&mut self, frame: i32) {
        let Some(test) = self.test.as_deref() else { return };
        if !test.starts_with("assassin-") || frame != 60 {
            return;
        }
        let map = self.cb.map();
        let center = Vec3::xz(map.width() as f32 * 4.0, map.height() as f32 * 4.0);
        let pos = Vec3::xz(center.x, center.z + 300.0);
        self.cb.cheats_set_enabled(true);
        if test == "assassin-target" || test == "assassin-target2" {
            if let Some(def) = self.defs.get("cloaksnipe")
                && let Some(u) = self.cb.cheat_new_unit(def, pos)
            {
                u.set_move_state(0, 0);
                if test == "assassin-target2" {
                    if let Some(other) = self.cb.cheat_new_unit(def, Vec3::xz(pos.x + 100.0, pos.z)) {
                        other.set_move_state(0, 0);
                    }
                }
                self.log(&format!("assassin-test: Phantom {} at {:.0},{:.0}", u.id(), pos.x, pos.z));
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
            "assassin-dirtbag" => Tactic::Dirtbag,
            "assassin-flea" => Tactic::Flea,
            _ => return,
        };
        let Some(def) = self.defs.get(tactic.name()) else { return };
        let home = Vec3::xz(center.x, center.z - 700.0);
        let cost = self.defs.get("cloaksnipe").map_or(750.0, |d| d.cost(self.metal));
        let count = tactic.size(1, cost, def.cost(self.metal));
        self.strikes.wave = Some(Wave {
            tactic,
            job: Job { id: None, pos, last: frame, count: 1, value: cost, name: "cloaksnipe".into() },
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
        self.sweeps_note_sniper(pos, 510);
        self.log(&format!("assassin-test: {count} {} assembled", tactic.name()));
    }
}
