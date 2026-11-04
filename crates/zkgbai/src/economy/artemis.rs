//! An Artemis against a big air force.
//!
//! An Artemis (`turretaaheavy`: 2400 metal, missiles out to 2400 elmos,
//! 4 m/s while it stockpiles) holds a wide circle against air. Once the
//! armed enemy aircraft we know of are worth over [`AIR_TRIGGER`] metal, we
//! build one at the centroid of our front. Income must allow it:
//! [`MIN_INCOME`], or [`MIN_INCOME_DENSE`] once the map is full (density at
//! the reference, see `posture`: the front holds still and statics pay).
//! It steps back toward our center, [`STEP`] at a time up to [`MAX_BACK`],
//! only while the site is unsafe, and only as long as it still reaches
//! [`KEEP`] of what one at the centroid would. Front spots count as reached
//! inside its range less [`MARGIN`], for gunships that fire on a spot from
//! beyond it.
//!
//! Before this rule, the only Artemis was one after five fusions
//! (`eco_create_screamer_task`, kept), so a big air force rarely met one
//! (user, 2026-11-02: "currently it seems underbuilt in such situations").
//! The first version wanted one Artemis to reach every front spot. That
//! never held on a big map: on Eye of Horus (2026-11-03) 12-15k of Nimbus
//! met none, because the front was 29 spots 4370 elmos about their center.
//! The user picked the centroid instead.

use serde_json::json;
use springai::Vec3;

use crate::Ai;
use crate::economy::List;
use crate::util::distance;

const ARTEMIS: &str = "turretaaheavy";

/// Armed enemy aircraft worth more than this (metal) call for an Artemis.
const AIR_TRIGGER: f32 = 4000.0;

/// A front spot counts as reached this far inside its range, elmos:
/// gunships fire on a spot from beyond it (Nimbus 600, Rapier 400).
const MARGIN: f32 = 400.0;

/// Steps back from the centroid toward our center while the site is not
/// safe, elmos, and how far back at most.
const STEP: f32 = 250.0;
const MAX_BACK: f32 = 1000.0;

/// A site must reach this share of the front spots one at the centroid
/// would; an Artemis of ours that does already covers the front.
const KEEP: f32 = 0.8;

/// Effective income (metal/s) from which one is built, and from which on a
/// full map.
const MIN_INCOME: f32 = 40.0;
const MIN_INCOME_DENSE: f32 = 25.0;

/// At most this many of ours, standing or planned.
const MAX_ARTEMIS: usize = 2;

/// Frames between looks, and after no site was found.
const PERIOD: i32 = 30 * 5;
const RETRY: i32 = 30 * 60;

impl Ai {
    /// Plan an Artemis if a big air force faces a front one would cover.
    pub(super) fn eco_plan_artemis(&mut self) {
        let frame = self.frame;
        if self.dev_off("artemis") || frame < self.eco.artemis_after {
            return;
        }
        self.eco.artemis_after = frame + PERIOD;
        let Some(def) = self.defs.get(ARTEMIS) else { return };
        let air = self.war.enemy_air_armed;
        if air <= AIR_TRIGGER {
            self.eco.artemis_why = None;
            return;
        }
        // One planned at a time; started ones are among the screamers.
        let planned = self.eco.construction_tasks.iter().any(|&id| {
            self.eco.tasks.get(id).and_then(|t| t.as_construction()).is_some_and(|c| c.build_type.id() == def.id())
        });
        let ours: Vec<Vec3> = self.eco.screamers.iter().filter(|u| u.health() > 0.0).map(|u| u.pos()).collect();
        if planned {
            return;
        }
        if ours.len() >= MAX_ARTEMIS {
            return self.artemis_note("cap", format!("{} of ours already", ours.len()));
        }
        let front: Vec<Vec3> = self.graph.get_front_line_spots().iter().map(|s| s.get_pos()).collect();
        if front.is_empty() {
            return self.artemis_note("nofront", "no front line".into());
        }
        let reach = def.max_weapon_range() - MARGIN;
        let center = centroid(&front);
        let at_center = covered_share(&front, center, reach);
        if at_center <= 0.0 {
            return self.artemis_note(
                "spread",
                format!(
                    "front {} spots, none within {reach:.0} of their centroid ({:.0}, {:.0})",
                    front.len(),
                    center.x,
                    center.z
                ),
            );
        }
        if let Some(share) = ours.iter().map(|&p| covered_share(&front, p, reach)).reduce(f32::max)
            && share >= KEEP * at_center
        {
            return self.artemis_note(
                "covered",
                format!("ours reach {:.0}% of the front, one at its centroid {:.0}%", 100.0 * share, 100.0 * at_center),
            );
        }
        let income = self.eco.effective_income;
        let (density, dense_at) = (self.posture.density, self.posture_levers().density_ref);
        let dense = density >= dense_at;
        if income < MIN_INCOME && !(dense && income >= MIN_INCOME_DENSE) {
            let need = if dense { MIN_INCOME_DENSE } else { MIN_INCOME };
            return self.artemis_note(
                "income",
                format!("income {income:.0} < {need:.0} (density {density:.0} of {dense_at:.0})"),
            );
        }
        if self.eco_energy_starved() {
            return self.artemis_note("energy", "energy-starved".into());
        }
        // At the front's centroid; back toward our center only while the
        // site there is unsafe, keeping most of what the centroid reaches.
        let base = self.graph.get_ally_center();
        let mut site = None;
        for aim in back_steps(center, base) {
            let at = self.eco_build_site(def, aim, 0);
            if at.x >= 0.0 && covered_share(&front, at, reach) >= KEEP * at_center && self.artemis_site_safe(at) {
                site = Some(at);
                break;
            }
        }
        let Some(at) = site else {
            self.eco.artemis_after = frame + RETRY;
            return self.artemis_note(
                "site",
                format!(
                    "no safe site within {MAX_BACK:.0} behind the front's centroid ({:.0}, {:.0})",
                    center.x, center.z
                ),
            );
        };
        if self.eco_add_simple(def, at, List::Construction, None).is_none() {
            self.eco.artemis_after = frame + RETRY;
            return self.artemis_note("site", format!("site ({:.0}, {:.0}) refused", at.x, at.z));
        }
        self.eco.artemis_why = None;
        let behind = distance(at, center);
        let reached = (covered_share(&front, at, reach) * front.len() as f32).round() as usize;
        let reached_center = (at_center * front.len() as f32).round() as usize;
        self.log(&format!(
            "artemis: planned at ({:.0}, {:.0}) against {air:.0} metal of enemy air: {behind:.0} from the front's \
             centroid ({:.0}, {:.0}), reaches {reached} of {} front spots (the centroid {reached_center}); \
             income {income:.0}, density {density:.0}",
            at.x,
            at.z,
            center.x,
            center.z,
            front.len()
        ));
        self.op_emit(
            "artemis",
            json!({
                "x": at.x.round(), "z": at.z.round(), "air": air.round(), "front": front.len(),
                "reached": reached, "reached_centroid": reached_center,
                "cx": center.x.round(), "cz": center.z.round(), "behind": behind.round(),
                "income": income.round(), "density": density.round(), "ours": ours.len(),
            }),
        );
    }

    /// No enemy defense covers it (`eco_clean_orders` would drop it) and it
    /// is not the enemy's ground. Not the mobile threat map: gunships paint
    /// it, and the air force this answers would rule out every site.
    fn artemis_site_safe(&self, at: Vec3) -> bool {
        self.war.get_porc_threat(at) <= 0.0 && !self.graph.is_enemy_territory(at)
    }

    /// Log why no Artemis, when the reason changes.
    fn artemis_note(&mut self, key: &'static str, why: String) {
        if self.eco.artemis_why != Some(key) {
            self.eco.artemis_why = Some(key);
            let air = self.war.enemy_air_armed;
            self.log(&format!("artemis: none against {air:.0} metal of enemy air: {why}"));
        }
    }
}

/// The share of `points` within `reach` of `at`.
fn covered_share(points: &[Vec3], at: Vec3, reach: f32) -> f32 {
    if points.is_empty() {
        return 1.0;
    }
    points.iter().filter(|&&p| distance(p, at) <= reach).count() as f32 / points.len() as f32
}

/// The mean of `points` on the ground. Points must not be empty.
fn centroid(points: &[Vec3]) -> Vec3 {
    let n = points.len() as f32;
    let (x, z) = points.iter().fold((0.0, 0.0), |(x, z), p| (x + p.x, z + p.z));
    Vec3::xz(x / n, z / n)
}

/// Aim points from `from` toward `toward`, [`STEP`] apart, out to
/// [`MAX_BACK`] or `toward` itself, whichever is nearer; `from` first.
fn back_steps(from: Vec3, toward: Vec3) -> Vec<Vec3> {
    let from = Vec3::xz(from.x, from.z);
    let len = distance(from, toward);
    let far = MAX_BACK.min(len);
    let mut aims = vec![from];
    let mut t: f32 = 0.0;
    while t < far - 1.0 {
        t = (t + STEP).min(far);
        aims.push(Vec3::xz(from.x + (toward.x - from.x) * t / len, from.z + (toward.z - from.z) * t / len));
    }
    aims
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(x: f32, z: f32) -> Vec3 {
        Vec3::xz(x, z)
    }

    #[test]
    fn centroid_is_the_mean() {
        let c = centroid(&[p(0.0, 0.0), p(1000.0, 0.0), p(500.0, 900.0)]);
        assert!((c.x - 500.0).abs() < 0.01 && (c.z - 300.0).abs() < 0.01, "c = {c:?}");
        let c = centroid(&[p(42.0, 7.0)]);
        assert!((c.x - 42.0).abs() < 0.01 && (c.z - 7.0).abs() < 0.01);
    }

    #[test]
    fn centroid_follows_the_bulk_not_an_outlier() {
        // Ten spots along a line and one far corner: the enclosing circle
        // would sit halfway to the corner, the centroid stays by the line.
        let mut front: Vec<Vec3> = (0..10).map(|i| p(2000.0 + 200.0 * i as f32, 4000.0)).collect();
        front.push(p(300.0, 300.0));
        let c = centroid(&front);
        assert!(covered_share(&front, c, 2000.0) >= 10.0 / 11.0, "c = {c:?}");
    }

    #[test]
    fn back_steps_walk_toward_base_and_stop() {
        // Base 600 behind: the centroid, 250, 500, then the base itself.
        let aims = back_steps(p(0.0, 0.0), p(0.0, 600.0));
        let zs: Vec<f32> = aims.iter().map(|a| a.z).collect();
        assert_eq!(zs.len(), 4, "{zs:?}");
        assert!((zs[0]).abs() < 0.01 && (zs[1] - 250.0).abs() < 0.01 && (zs[2] - 500.0).abs() < 0.01);
        assert!((zs[3] - 600.0).abs() < 0.01);
        // Base far behind: no farther than MAX_BACK.
        let aims = back_steps(p(0.0, 0.0), p(0.0, 5000.0));
        assert_eq!(aims.len(), 1 + (MAX_BACK / STEP) as usize);
        assert!((aims.last().unwrap().z - MAX_BACK).abs() < 0.01);
        // Base at the centroid: just the centroid.
        assert_eq!(back_steps(p(10.0, 10.0), p(10.0, 10.5)).len(), 1);
    }

    #[test]
    fn share_of_front_covered() {
        let front = [p(0.0, 0.0), p(1000.0, 0.0), p(3000.0, 0.0), p(5000.0, 0.0)];
        assert_eq!(covered_share(&front, p(0.0, 0.0), 2000.0), 0.5);
        assert_eq!(covered_share(&[], p(0.0, 0.0), 2000.0), 1.0);
    }
}
