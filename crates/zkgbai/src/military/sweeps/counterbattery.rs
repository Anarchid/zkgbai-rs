//! Weapon-labelled firing evidence, kept separate from LOS contacts. A single
//! impact gives a bearing and a range bound, not an exact enemy position.
use super::*;
use std::collections::BTreeMap;

const MEMORY: i32 = 30 * 120;
pub(crate) const FRESH: i32 = 30 * 30;

#[derive(Clone)]
pub(crate) struct BatteryFix {
    /// Stable negative key, separate from real engine unit IDs.
    pub id: i32,
    pub weapon: String,
    pub def: Option<String>,
    pub source: Option<i32>,
    pub pos: Vec3,
    pub uncertainty: f32,
    pub range: f32,
    pub last: i32,
    pub ground: bool,
    origin: Vec3,
    direction: Vec3,
    reported: i32,
}

#[derive(Default)]
pub(super) struct Counterbattery {
    serial: i32,
    fixes: Vec<BatteryFix>,
    owners: BTreeMap<i32, Option<String>>,
}

fn long_gun(range: f32, stinger: f32) -> bool {
    range.is_finite() && stinger.is_finite() && stinger > 0.0 && range > stinger
}

/// Intersect sufficiently different forward bearings. Reject intersections
/// behind either victim or beyond the weapon's plausible reach.
fn intersection(a: Vec3, da: Vec3, b: Vec3, db: Vec3, range: f32) -> Option<Vec3> {
    let cross = da.x * db.z - da.z * db.x;
    if cross.abs() < 0.25 || distance(a, b) < 100.0 {
        return None;
    }
    let dx = b.x - a.x;
    let dz = b.z - a.z;
    let t = (dx * db.z - dz * db.x) / cross;
    let u = (dx * da.z - dz * da.x) / cross;
    if t < 0.0 || u < 0.0 || t > range * 1.2 || u > range * 1.2 {
        return None;
    }
    Some(Vec3::xz(a.x + t * da.x, a.z + t * da.z))
}

impl Ai {
    pub(crate) fn counterbattery_fixes(&self, age: i32) -> impl Iterator<Item = &BatteryFix> {
        self.sweeps.counterbattery.fixes.iter().filter(move |f| self.frame - f.last < age)
    }

    pub(super) fn counterbattery_damaged(&mut self, unit: Unit, attacker: Option<Unit>, dir: Vec3, w: WeaponDef) {
        if attacker.is_some_and(|a| a.ally_team() == self.ally_team_id) {
            return;
        }
        let stinger = self.defs.get("turretheavylaser").map_or(f32::INFINITY, |d| d.max_weapon_range());
        let range = w.range();
        if !long_gun(range, stinger) {
            return;
        }
        let origin = unit.pos();
        let known = attacker.filter(|a| a.pos() != crate::util::NULLPOS);
        let len = dir.x.hypot(dir.z);
        if known.is_none() && (!len.is_finite() || len < 0.01) {
            return;
        }
        let direction = if len.is_finite() && len >= 0.01 { Vec3::xz(dir.x / len, dir.z / len) } else { Vec3::ZERO };
        let mut pos = known.map_or_else(
            || Vec3::xz(origin.x + direction.x * range * 0.8, origin.z + direction.z * range * 0.8),
            |a| a.pos(),
        );
        let mut uncertainty = if known.is_some() { 64.0 } else { range * 0.6 };
        if !self.sweeps.counterbattery.owners.contains_key(&w.id()) {
            let mut names: Vec<_> = self
                .defs
                .all()
                .filter(|d| d.weapon_mounts().iter().any(|m| m.weapon_def().is_some_and(|wd| wd.id() == w.id())))
                .map(|d| self.defs.name(d).to_owned())
                .collect();
            names.sort();
            names.dedup();
            self.sweeps.counterbattery.owners.insert(w.id(), if names.len() == 1 { names.pop() } else { None });
        }
        let def = known
            .and_then(|a| a.def())
            .map(|d| self.defs.name(d).to_owned())
            .or_else(|| self.sweeps.counterbattery.owners[&w.id()].clone());
        let weapon = w.name();
        let source = known.map(|a| a.id());
        let state = &mut self.sweeps.counterbattery;
        state.fixes.retain(|f| self.frame - f.last < MEMORY);
        let index = state.fixes.iter().position(|f| {
            f.weapon == weapon
                && (source.is_some() && f.source == source
                    || (f.source.is_none() || source.is_none()) && distance(f.pos, pos) < range * 0.5)
        });
        if let Some(i) = index {
            let old = &state.fixes[i];
            if known.is_none() && self.frame - old.last < 90 {
                if let Some(p) = intersection(old.origin, old.direction, origin, direction, range) {
                    pos = p;
                    // Splash and moving shooters still leave substantial error.
                    uncertainty = (range * 0.15).max(150.0);
                } else if distance(old.pos, pos) < old.uncertainty {
                    pos = old.pos;
                    uncertainty = old.uncertainty.max(150.0);
                }
            }
        }
        let map = self.cb.map();
        pos.x = pos.x.clamp(1.0, (map.width() * 8 - 1).max(1) as f32);
        pos.z = pos.z.clamp(1.0, (map.height() * 8 - 1).max(1) as f32);
        pos.y = map.elevation_at(pos.x, pos.z);
        let report = index.is_none_or(|i| self.frame - state.fixes[i].reported >= 30);
        let reported = if report { self.frame } else { state.fixes[index.unwrap()].reported };
        let id = index.map_or_else(
            || {
                state.serial += 1;
                -state.serial
            },
            |i| state.fixes[i].id,
        );
        let fix = BatteryFix {
            id,
            weapon,
            def,
            source,
            pos,
            uncertainty,
            range,
            last: self.frame,
            ground: w.is_able_to_attack_ground(),
            origin,
            direction,
            reported,
        };
        if let Some(i) = index {
            state.fixes[i] = fix;
        } else {
            if state.fixes.len() >= 128 {
                state.fixes.remove(0);
            }
            state.fixes.push(fix);
        }
        if report {
            self.war.refresh_defense_target(DefenseTarget::new(pos, range, self.frame));
        }
    }

    pub(crate) fn counterbattery_destroyed(&mut self, id: i32) {
        let enemy = self.war.targets.get(&id);
        self.sweeps.counterbattery.fixes.retain(|f| {
            f.source != Some(id)
                && !enemy.is_some_and(|e| {
                    f.source.is_none()
                        && f.def.as_deref() == Some(e.def_name.as_str())
                        && distance(f.pos, e.position) <= f.uncertainty.max(150.0)
                })
        });
    }

    pub(crate) fn counterbattery_status(&self) -> serde_json::Value {
        serde_json::json!(
            self.counterbattery_fixes(MEMORY)
                .map(|f| serde_json::json!({
                    "id": f.id, "weapon": f.weapon, "def": f.def, "source": f.source, "pos": [f.pos.x, f.pos.z],
                    "uncertainty": f.uncertainty, "range": f.range, "ground": f.ground, "age_frames": self.frame - f.last,
                }))
                .collect::<Vec<_>>()
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn threshold_is_strictly_beyond_live_stinger_range() {
        assert!(!long_gun(620.0, 620.0));
        assert!(long_gun(1200.0, 620.0)); // Tremor/Emissary-class range.
        assert!(!long_gun(f32::NAN, 620.0));
        assert!(!long_gun(1200.0, f32::INFINITY));
    }
    #[test]
    fn bearings_refine_only_with_useful_forward_geometry() {
        let a = Vec3::xz(0.0, 0.0);
        let da = Vec3::xz(1.0, 0.0);
        let b = Vec3::xz(800.0, -800.0);
        let db = Vec3::xz(0.0, 1.0);
        assert_eq!(intersection(a, da, b, db, 1200.0), Some(Vec3::xz(800.0, 0.0)));
        assert!(intersection(a, da, b, da, 1200.0).is_none());
        assert!(intersection(a, da, b, Vec3::xz(0.0, -1.0), 1200.0).is_none());
        assert!(intersection(a, da, b, db, 400.0).is_none());
    }
}
