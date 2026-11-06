//! Range applies to the whole driven route, not only its final waypoint.
use super::*;

/// End a move at the first entry into a firing circle. Do not queue the
/// original destinations afterwards: that used to carry reloading heavies
/// straight through the unit they could already shoot.
pub(super) fn stop_at_range(pos: Vec3, way: &[Vec3], target: Vec3, range: f32) -> Vec<Vec3> {
    let mut from = pos;
    let mut out = Vec::new();
    for &to in way {
        let (dx, dz) = (to.x - from.x, to.z - from.z);
        let (x, z) = (from.x - target.x, from.z - target.z);
        let a = dx * dx + dz * dz;
        let c = x * x + z * z - range * range;
        if c <= 0.0 {
            return vec![pos];
        }
        let b = x * dx + z * dz;
        let disc = b * b - a * c;
        if a > 0.0 && disc >= 0.0 {
            let t = (-b - disc.sqrt()) / a;
            if (0.0..=1.0).contains(&t) {
                out.push(Vec3::xz(from.x + t * dx, from.z + t * dz));
                return out;
            }
        }
        out.push(to);
        from = to;
    }
    out
}

/// Route each approach/turn segment around the pass's firing circle.
/// A circumscribed polygon keeps its chords outside the circle as well;
/// putting vertices on the circle would still cut inside on every leg.
pub(super) fn around_range(pos: Vec3, way: &[Vec3], target: Vec3, radius: f32) -> Vec<Vec3> {
    if radius < 1.0 {
        return way.to_vec();
    }
    let mut from = pos;
    let mut out = Vec::new();
    for &dest in way {
        let d = distance(from, target);
        if d < radius - 1.0 {
            // Already too close: first recover range directly, never queue
            // a return across the target behind that escape.
            let angle = (from.z - target.z).atan2(from.x - target.x);
            return vec![Vec3::xz(target.x + (radius + 20.0) * angle.cos(), target.z + (radius + 20.0) * angle.sin())];
        }
        let (_, gap) = route_project(&[from, dest], target);
        if gap < radius - 0.5 {
            let a = (from.z - target.z).atan2(from.x - target.x);
            let b = (dest.z - target.z).atan2(dest.x - target.x);
            let delta = (b - a + std::f32::consts::PI).rem_euclid(std::f32::consts::TAU) - std::f32::consts::PI;
            let count = (delta.abs() / 15.0f32.to_radians()).ceil().max(1.0) as usize;
            let step = delta / count as f32;
            let outer = (radius + 2.0) / (step.abs() / 2.0).cos();
            for i in 0..=count {
                let angle = a + step * i as f32;
                out.push(Vec3::xz(target.x + outer * angle.cos(), target.z + outer * angle.sin()));
            }
            if distance(dest, target) < radius {
                return out;
            }
        }
        out.push(dest);
        from = dest;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn a_reloading_grizzly_never_queues_a_factory_center_after_its_range_stop() {
        let p = Vec3::xz(0.0, 0.0);
        let t = Vec3::xz(1400.0, 0.0);
        let way = stop_at_range(p, &[Vec3::xz(600.0, 0.0), t, Vec3::xz(1800.0, 0.0)], t, 800.0);
        assert_eq!(way.len(), 1);
        assert!(distance(way[0], Vec3::xz(600.0, 0.0)) < 0.01);
        assert!(distance(way[0], t) >= 799.99);
        assert_eq!(stop_at_range(Vec3::xz(650.0, 0.0), &[t], t, 800.0), vec![Vec3::xz(650.0, 0.0)]);
        assert_eq!(stop_at_range(p, &[Vec3::xz(0.0, 500.0)], t, 800.0), vec![Vec3::xz(0.0, 500.0)]);
    }
    #[test]
    fn tangent_entries_and_return_turns_keep_range_along_every_segment() {
        let t = Vec3::xz(1000.0, 1000.0);
        for p in [Vec3::xz(100.0, 1000.0), Vec3::xz(1500.0, 1300.0), Vec3::xz(1500.0, 700.0)] {
            let way = around_range(p, &[Vec3::xz(1000.0, 1300.0), Vec3::xz(1500.0, 1300.0)], t, 300.0);
            let route: Vec<_> = std::iter::once(p).chain(way).collect();
            assert!(route.windows(2).all(|w| route_project(w, t).1 >= 299.5), "{route:?}");
        }
    }
    #[test]
    fn a_pack_already_inside_the_circle_recovers_outwards_first() {
        let t = Vec3::xz(1000.0, 1000.0);
        let p = Vec3::xz(1100.0, 1000.0);
        let way = around_range(p, &[Vec3::xz(500.0, 1000.0)], t, 300.0);
        assert_eq!(way.len(), 1);
        assert!(way[0].x > p.x && distance(way[0], t) >= 300.0);
        let centered = around_range(t, &[p], t, 300.0);
        assert!(centered[0].x.is_finite() && distance(centered[0], t) >= 300.0);
    }
}
