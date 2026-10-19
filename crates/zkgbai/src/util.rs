//! Geometry and randomness helpers (the original `KgbUtil`).

use std::cell::Cell;
use std::f64::consts::PI;

use springai::{Unit, Vec3};

/// The original's "no position" sentinel.
pub const NULLPOS: Vec3 = Vec3::ZERO;

/// Command option: queue after existing orders.
pub const OPTION_SHIFT_KEY: i16 = springai::opt::SHIFT_KEY;

thread_local! {
    static RNG: Cell<u64> = const { Cell::new(0x9E37_79B9_7F4A_7C15) };
}

/// Seed the AI's random number generator.
pub fn seed_rand(seed: u64) {
    RNG.with(|r| r.set(seed | 1));
}

/// Uniform random number in `[0, 1)` (the original `kgb_rand` / `Math.random`).
pub fn kgb_rand() -> f64 {
    RNG.with(|r| {
        // splitmix64
        let mut z = r.get().wrapping_add(0x9E37_79B9_7F4A_7C15);
        r.set(z);
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^= z >> 31;
        (z >> 11) as f64 / (1u64 << 53) as f64
    })
}

/// Ground-plane distance between two points.
#[inline]
pub fn distance(a: Vec3, b: Vec3) -> f32 {
    a.distance_2d(b)
}

/// A random point on a circle of `radius` around `position` (y = 0).
pub fn get_radial_point(position: Vec3, radius: f32) -> Vec3 {
    let angle = kgb_rand() * 2.0 * PI;
    Vec3::xz(
        (position.x as f64 + radius as f64 * angle.cos()) as f32,
        (position.z as f64 + radius as f64 * angle.sin()) as f32,
    )
}

/// The point `distance` along the ground-plane direction from `start` to
/// `dest` (y = 0).
pub fn get_directional_point(start: Vec3, dest: Vec3, distance: f32) -> Vec3 {
    let (mut x, mut z) = (dest.x - start.x, dest.z - start.z);
    let d = (x * x + z * z).sqrt();
    x /= d;
    z /= d;
    Vec3::xz(start.x + x * distance, start.z + z * distance)
}

/// A point at `radius` from `start`, in the direction of `dest` jittered by
/// up to ±45°/2 (y = 0).
pub fn get_angular_point(start: Vec3, dest: Vec3, radius: f32) -> Vec3 {
    let (x, z) = ((dest.x - start.x) as f64, (dest.z - start.z) as f64);
    let mut angle = z.atan2(x);
    if kgb_rand() > 0.5 {
        angle -= kgb_rand() * 0.25 * PI;
    } else {
        angle += kgb_rand() * 0.25 * PI;
    }
    Vec3::xz(
        (start.x as f64 + radius as f64 * angle.cos()) as f32,
        (start.z as f64 + radius as f64 * angle.sin()) as f32,
    )
}

/// Translate `dest` by the offset of `start` from `compare` (keeps formation).
pub fn get_formation_point(start: Vec3, compare: Vec3, dest: Vec3) -> Vec3 {
    Vec3::new(dest.x + (start.x - compare.x), dest.y, dest.z + (start.z - compare.z))
}

/// A unit's current ground speed in elmos per second.
pub fn get_speed(u: Unit) -> f32 {
    let v = u.vel();
    (v.x * v.x + v.z * v.z).sqrt() * 30.0
}

#[inline]
pub fn lerp(start: f32, end: f32, scale: f32) -> f32 {
    end * scale + start * (1.0 - scale)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rand_is_in_unit_interval() {
        seed_rand(42);
        for _ in 0..10_000 {
            let r = kgb_rand();
            assert!((0.0..1.0).contains(&r));
        }
    }

    #[test]
    fn directional_point_has_requested_distance() {
        let p = get_directional_point(Vec3::xz(0.0, 0.0), Vec3::xz(300.0, 400.0), 50.0);
        assert!((p.x - 30.0).abs() < 1e-4 && (p.z - 40.0).abs() < 1e-4);
    }
}
