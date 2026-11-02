//! An Artemis against a big air force.
//!
//! An Artemis (`turretaaheavy`: 2400 metal, missiles out to 2400 elmos,
//! 4 m/s while it stockpiles) can cover a whole front by itself. Once the
//! armed enemy aircraft we know of are worth over [`AIR_TRIGGER`] metal,
//! and one Artemis set back from our front would reach every spot on it,
//! we build one. Income must allow it: [`MIN_INCOME`], or
//! [`MIN_INCOME_DENSE`] once the map is full (density at the reference,
//! see `regime`: the front holds still and statics pay). It goes as far
//! back toward our center as it can while still reaching the whole front,
//! less [`MARGIN`] for gunships that fire on a spot from beyond it.
//!
//! Before this rule, the only Artemis was one after five fusions
//! (`eco_create_screamer_task`, kept), so a big air force rarely met one
//! (user, 2026-11-02: "currently it seems underbuilt in such situations").

use serde_json::json;
use springai::Vec3;

use crate::Ai;
use crate::economy::List;
use crate::util::distance;

const ARTEMIS: &str = "turretaaheavy";

/// Armed enemy aircraft worth more than this (metal) call for an Artemis.
const AIR_TRIGGER: f32 = 4000.0;

/// The front must lie this far inside its range, elmos: gunships fire on
/// a spot from beyond it (Nimbus 600, Rapier 400).
const MARGIN: f32 = 400.0;

/// The site search aims this much inside the reach, elmos, so the build
/// site the engine finds near the aim point still reaches the whole front.
const SLACK: f32 = 150.0;

/// Effective income (metal/s) from which one is built, and from which on a
/// full map.
const MIN_INCOME: f32 = 40.0;
const MIN_INCOME_DENSE: f32 = 25.0;

/// An Artemis already reaching this share of the front's spots covers it.
const COVERED: f32 = 0.5;

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
        if let Some(share) = ours.iter().map(|&p| covered_share(&front, p, reach)).reduce(f32::max)
            && share >= COVERED
        {
            return self.artemis_note("covered", format!("ours reach {:.0}% of the front", 100.0 * share));
        }
        let income = self.eco.effective_income;
        let (density, dense_at) = (self.regime.density, self.regime_density_ref());
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
        let (center, radius) = enclosing_circle(&front);
        if radius > reach {
            return self.artemis_note(
                "wide",
                format!(
                    "front {} spots, {radius:.0} elmos about its center: one would not reach it ({reach:.0})",
                    front.len()
                ),
            );
        }
        // Farthest back toward our center that still reaches all of it;
        // failing a safe site there, nearer the front's center.
        let base = self.graph.get_ally_center();
        let back = back_site(&front, center, base, reach - SLACK);
        let mut site = None;
        for k in [1.0, 0.75, 0.5, 0.25, 0.0] {
            let aim = Vec3::xz(center.x + k * (back.x - center.x), center.z + k * (back.z - center.z));
            let at = self.eco_build_site(def, aim, 0);
            if at.x >= 0.0 && covered_share(&front, at, reach) >= 1.0 && self.artemis_site_safe(at) {
                site = Some(at);
                break;
            }
        }
        let Some(at) = site else {
            self.eco.artemis_after = frame + RETRY;
            return self.artemis_note("site", "no safe site reaches the whole front".into());
        };
        if self.eco_add_simple(def, at, List::Construction, None).is_none() {
            self.eco.artemis_after = frame + RETRY;
            return self.artemis_note("site", format!("site ({:.0}, {:.0}) refused", at.x, at.z));
        }
        self.eco.artemis_why = None;
        let behind = distance(at, center);
        self.log(&format!(
            "artemis: planned at ({:.0}, {:.0}) against {air:.0} metal of enemy air: reaches all {} front spots \
             ({radius:.0} about their center, {behind:.0} behind it); income {income:.0}, density {density:.0}",
            at.x,
            at.z,
            front.len()
        ));
        self.op_emit(
            "artemis",
            json!({
                "x": at.x.round(), "z": at.z.round(), "air": air.round(), "front": front.len(),
                "radius": radius.round(), "behind": behind.round(), "income": income.round(),
                "density": density.round(), "ours": ours.len(),
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

/// The smallest circle (on the ground) holding every point: center and
/// radius. Points must not be empty.
fn enclosing_circle(points: &[Vec3]) -> (Vec3, f32) {
    const EPS: f32 = 1e-3;
    let outside = |p: Vec3, c: Vec3, r: f32| distance(p, c) > r + EPS;
    let mut c = Vec3::xz(points[0].x, points[0].z);
    let mut r = 0.0;
    for i in 1..points.len() {
        if !outside(points[i], c, r) {
            continue;
        }
        (c, r) = (Vec3::xz(points[i].x, points[i].z), 0.0);
        for j in 0..i {
            if !outside(points[j], c, r) {
                continue;
            }
            (c, r) = diameter_circle(points[i], points[j]);
            for k in 0..j {
                if outside(points[k], c, r) {
                    (c, r) = circumcircle(points[i], points[j], points[k]);
                }
            }
        }
    }
    (c, r)
}

fn diameter_circle(a: Vec3, b: Vec3) -> (Vec3, f32) {
    (Vec3::xz((a.x + b.x) / 2.0, (a.z + b.z) / 2.0), distance(a, b) / 2.0)
}

/// The circle through three points; for (nearly) collinear ones, the
/// circle on the farthest pair.
fn circumcircle(a: Vec3, b: Vec3, c: Vec3) -> (Vec3, f32) {
    let (bx, bz) = (b.x - a.x, b.z - a.z);
    let (cx, cz) = (c.x - a.x, c.z - a.z);
    let d = 2.0 * (bx * cz - bz * cx);
    if d.abs() < 1e-3 {
        let pairs = [(a, b), (a, c), (b, c)];
        let (p, q) = pairs.into_iter().max_by(|x, y| distance(x.0, x.1).total_cmp(&distance(y.0, y.1))).unwrap();
        return diameter_circle(p, q);
    }
    let (b2, c2) = (bx * bx + bz * bz, cx * cx + cz * cz);
    let ux = (cz * b2 - bz * c2) / d;
    let uz = (bx * c2 - cx * b2) / d;
    let center = Vec3::xz(a.x + ux, a.z + uz);
    (center, (ux * ux + uz * uz).sqrt())
}

/// From `from` (which reaches every point of `front`) toward `toward`, the
/// farthest point that still reaches every one. The points that reach all
/// of them form a convex set, so the ray leaves it once.
fn back_site(front: &[Vec3], from: Vec3, toward: Vec3, reach: f32) -> Vec3 {
    let reaches = |p: Vec3| front.iter().all(|&f| distance(f, p) <= reach);
    let along = |t: f32| {
        let len = distance(from, toward);
        if len < 1.0 {
            return from;
        }
        Vec3::xz(from.x + (toward.x - from.x) * t / len, from.z + (toward.z - from.z) * t / len)
    };
    let len = distance(from, toward);
    if reaches(along(len)) {
        return along(len);
    }
    let (mut lo, mut hi) = (0.0, len);
    for _ in 0..24 {
        let mid = (lo + hi) / 2.0;
        if reaches(along(mid)) {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    along(lo)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(x: f32, z: f32) -> Vec3 {
        Vec3::xz(x, z)
    }

    #[test]
    fn circle_holds_every_point_and_is_tight() {
        let pts = [p(0.0, 0.0), p(1000.0, 0.0), p(500.0, 300.0), p(500.0, -200.0), p(200.0, 100.0)];
        let (c, r) = enclosing_circle(&pts);
        assert!(pts.iter().all(|&q| distance(q, c) <= r + 0.01));
        // The two far points are a diameter here.
        assert!((r - 500.0).abs() < 1.0, "r = {r}");
        assert!((c.x - 500.0).abs() < 1.0 && c.z.abs() < 1.0);
    }

    #[test]
    fn circle_of_a_triangle_is_its_circumcircle() {
        let pts = [p(0.0, 0.0), p(1000.0, 0.0), p(500.0, 800.0)];
        let (c, r) = enclosing_circle(&pts);
        assert!(pts.iter().all(|&q| (distance(q, c) - r).abs() < 0.5));
        assert!(r < 600.0, "r = {r}");
    }

    #[test]
    fn collinear_points() {
        let pts = [p(0.0, 0.0), p(300.0, 0.0), p(900.0, 0.0)];
        let (c, r) = enclosing_circle(&pts);
        assert!((r - 450.0).abs() < 0.5 && (c.x - 450.0).abs() < 0.5);
    }

    #[test]
    fn back_site_retreats_until_the_front_is_at_reach() {
        // A front 2000 wide along z = 3000; our base at the origin.
        let front = [p(-1000.0, 3000.0), p(0.0, 3000.0), p(1000.0, 3000.0)];
        let (c, r) = enclosing_circle(&front);
        assert!((r - 1000.0).abs() < 1.0);
        let at = back_site(&front, c, p(0.0, 0.0), 2000.0);
        // Back by sqrt(2000^2 - 1000^2) from the front's center.
        assert!(front.iter().all(|&f| distance(f, at) <= 2000.0 + 0.5));
        assert!((at.z - (3000.0 - 1732.05)).abs() < 2.0, "at = {at:?}");
        // A base close behind the front: all the way back.
        let at = back_site(&front, c, p(0.0, 2500.0), 2000.0);
        assert!((at.z - 2500.0).abs() < 0.5);
    }

    #[test]
    fn share_of_front_covered() {
        let front = [p(0.0, 0.0), p(1000.0, 0.0), p(3000.0, 0.0), p(5000.0, 0.0)];
        assert_eq!(covered_share(&front, p(0.0, 0.0), 2000.0), 0.5);
        assert_eq!(covered_share(&[], p(0.0, 0.0), 2000.0), 1.0);
    }
}
