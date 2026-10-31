//! Routes an airplane can actually fly. A move order completes only within
//! `max(turnRadius + 16, 128)` elmos of its point (Recoil's `CAirCAI::
//! ExecuteMove`), and the unit def's `turnRadius` is far smaller than the
//! circle a heavy airframe really turns on. A waypoint inside that circle is
//! never passed close enough: the plane orbits it at full rudder until a new
//! order arrives. The engine's own near-goal flip uses the same small
//! `turnRadius` and does not help. An Odin (`turnRadius` 120) turns on about
//! 400 elmos, so a fresh route that started beside or behind it held one
//! circling over an enemy factory until it was shot down.

use springai::Vec3;

/// Bank of a hard turn whatever the def's `maxBank`: once past it, the
/// engine keeps rolling toward a goal far off the nose (`GetAileronDeflection`,
/// "these ignore maxBank"). Fitted to the Odin (maxBank 0.008) in the
/// `airstrike-turns` fixture: measured 401 elmos, 404 with this floor.
const HARD_TURN_BANK: f32 = 0.4;

/// Steady full-turn radius, in elmos, of a strafe-move aircraft. Each frame
/// full rudder yaws the nose by `rudder` times the per-frame speed, and full
/// elevator while banked adds about `elevator * bank` of horizontal turn.
/// `front_to_speed` then pulls the nose back toward the velocity and
/// `speed_to_front` swings the velocity after the nose, so in the steady turn
/// the velocity follows at `s(1 - f) / (f + s(1 - f))` of the nose's rate;
/// the radius does not depend on speed. Measured in `airstrike-turns`
/// (2026-10-31, model in brackets): Odin 401 (404), Likho 194 (207), Phoenix
/// 155 (118), Raven 100 (101), Magpie 86 (62), Raptor 82 (61), Sparrow 68 (85).
/// Zero when the def has no turn authority (no trimming).
pub(crate) fn turn_radius(rudder: f32, elevator: f32, bank: f32, front_to_speed: f32, speed_to_front: f32) -> f32 {
    let yaw = rudder.max(0.0) + elevator.max(0.0) * bank.clamp(HARD_TURN_BANK, 1.0);
    let (f, s) = (front_to_speed.clamp(0.0, 0.99), speed_to_front.clamp(0.001, 1.0));
    if yaw <= 1e-6 {
        return 0.0;
    }
    (1.0 + f / (s * (1.0 - f))) / yaw
}

/// Elmos within which the engine counts an aircraft's move order as done.
pub(crate) fn arrival_radius(def_turn_radius: f32) -> f32 {
    (def_turn_radius + 16.0).max(128.0)
}

fn flat(v: Vec3) -> Option<(f32, f32)> {
    let len = (v.x * v.x + v.z * v.z).sqrt();
    (len > 1e-3).then(|| (v.x / len, v.z / len))
}

/// Frames a plane flies on before its turn bites (control surfaces swing
/// over, the velocity lags the nose). Fitted to an Odin orbit in
/// `airstrike-odin`: its circle's centre sat ~95 elmos further along the
/// heading than the lag-free model's, half a second at 185 elmos/s.
const TURN_LAG_FRAMES: f32 = 15.0;

/// Whether a plane at `at` flying along unit `dir` at `speed` elmos/frame
/// gets to `p` without looping. It turns toward `p` at full rudder on a circle
/// of `radius` that starts `TURN_LAG_FRAMES` ahead, and the engine counts the
/// order done within `arrival`. A point deeper inside that circle than half
/// the arrival radius (margin for the fit) is never passed close enough: the
/// plane orbits it. A point behind and nearer than the circle's diameter is
/// reached only after most of a full loop.
fn worth_flying(at: Vec3, dir: (f32, f32), speed: f32, radius: f32, arrival: f32, p: Vec3) -> bool {
    let (dx, dz) = (p.x - at.x, p.z - at.z);
    if dir.0 * dx + dir.1 * dz < 0.0 && dx * dx + dz * dz < 4.0 * radius * radius {
        return false;
    }
    // Left of the heading is the side of (-dz, dx) for heading (dx, dz).
    let side = if dir.0 * dz - dir.1 * dx >= 0.0 { 1.0 } else { -1.0 };
    let lag = speed * TURN_LAG_FRAMES;
    let (cx, cz) = (at.x + dir.0 * lag - dir.1 * radius * side, at.z + dir.1 * lag + dir.0 * radius * side);
    let (ex, ez) = (p.x - cx, p.z - cz);
    (ex * ex + ez * ez).sqrt() >= radius - arrival / 2.0
}

/// The route's waypoints a plane at `from` with velocity `velocity`
/// (elmos/frame) can fly in turn. A waypoint it would orbit or loop for,
/// seen from the point before it (or from the plane, for the first), is
/// skipped, so the plane cuts across that bend; it could not fly it anyway.
/// The last point is always kept. A plane without a heading (landed,
/// hovering) keeps its first point.
pub(crate) fn flyable(from: Vec3, velocity: Vec3, radius: f32, arrival: f32, route: &[Vec3]) -> Vec<Vec3> {
    let mut kept = Vec::with_capacity(route.len());
    let speed = (velocity.x * velocity.x + velocity.z * velocity.z).sqrt();
    let (mut at, mut dir) = (from, flat(velocity));
    for (i, &p) in route.iter().enumerate() {
        let last = i + 1 == route.len();
        if !last && radius > arrival && dir.is_some_and(|d| !worth_flying(at, d, speed, radius, arrival, p)) {
            continue;
        }
        if let Some(d) = flat(p - at) {
            dir = Some(d);
        }
        at = p;
        kept.push(p);
    }
    kept
}

#[cfg(test)]
mod tests {
    use super::*;

    const F: f32 = 0.1;
    const S: f32 = 0.07;

    #[test]
    fn odin_turns_on_a_much_wider_circle_than_its_def_radius() {
        // bomberassault: maxRudder 0.0044, maxElevator 0.005, maxBank 0.008.
        let odin = turn_radius(0.0044, 0.005, 0.008, F, S);
        assert!((380.0..430.0).contains(&odin), "{odin}");
        // bomberprec banks hard: maxRudder 0.0135, maxElevator 0.02, maxBank 0.6.
        let raven = turn_radius(0.0135, 0.02, 0.6, F, S);
        assert!(raven < 150.0, "{raven}");
        assert_eq!(turn_radius(0.0, 0.0, 0.0, F, S), 0.0);
        assert_eq!(arrival_radius(120.0), 136.0);
        assert_eq!(arrival_radius(10.0), 128.0);
    }

    #[test]
    fn a_waypoint_behind_or_beside_a_heavy_plane_is_skipped() {
        let east = Vec3::xz(6.0, 0.0);
        let from = Vec3::xz(1000.0, 1000.0);
        let home = Vec3::xz(-5000.0, 1000.0);
        // A fresh route home starts with points beside and behind the plane.
        let route = [Vec3::xz(1100.0, 1300.0), Vec3::xz(850.0, 1250.0), Vec3::xz(-2000.0, 1000.0), home];
        assert_eq!(flyable(from, east, 400.0, 136.0, &route), vec![Vec3::xz(-2000.0, 1000.0), home]);
        // Ahead is fine, and so is a point far enough off to the side.
        let ahead = [Vec3::xz(1500.0, 1000.0), Vec3::xz(2000.0, 2500.0), home];
        assert_eq!(flyable(from, east, 400.0, 136.0, &ahead), ahead.to_vec());
        // A nimble plane keeps everything.
        assert_eq!(flyable(from, east, 100.0, 128.0, &route), route.to_vec());
        // No heading: the first point stays.
        assert_eq!(flyable(from, Vec3::ZERO, 400.0, 136.0, &route)[0], route[0]);
        // The last point always stays, even when it is right behind.
        assert_eq!(flyable(from, east, 400.0, 136.0, &[Vec3::xz(900.0, 1100.0)]), vec![Vec3::xz(900.0, 1100.0)]);
    }

    #[test]
    fn tight_bends_along_the_route_are_cut_for_heavy_planes_only() {
        let east = Vec3::xz(6.0, 0.0);
        // A square corner four cells after a waypoint: the Odin cuts it.
        let corner = [
            Vec3::xz(384.0, 0.0),
            Vec3::xz(768.0, 0.0),
            Vec3::xz(768.0, 256.0),
            Vec3::xz(768.0, 1500.0),
            Vec3::xz(768.0, 3000.0),
        ];
        let odin = flyable(Vec3::ZERO, east, 400.0, 136.0, &corner);
        assert_eq!(odin, vec![corner[0], corner[1], corner[3], corner[4]]);
        assert_eq!(flyable(Vec3::ZERO, east, 120.0, 128.0, &corner), corner.to_vec());
        // The grid's usual 45-degree bend after a 6-cell leg: an Odin cuts
        // it, a Likho (about 200, arrival 128) flies it.
        let bend = [Vec3::xz(384.0, 0.0), Vec3::xz(768.0, 0.0), Vec3::xz(1039.5, 271.5), Vec3::xz(1311.0, 543.0)];
        assert!(!flyable(Vec3::ZERO, east, 400.0, 136.0, &bend).contains(&bend[2]));
        assert_eq!(flyable(Vec3::ZERO, Vec3::xz(8.5, 0.0), 200.0, 128.0, &bend), bend.to_vec());
    }

    #[test]
    fn the_orders_an_odin_orbited_in_the_fixture_are_skipped() {
        // airstrike-odin 2026-10-31 (factory-new4): f6609, a point ~40 degrees
        // off the nose that the lag-free model passed at 68 elmos (real miss
        // 140, orbited); f6909, a point almost straight behind at 390 elmos
        // (reached after ~300 degrees of turning).
        let home = Vec3::xz(5432.0, 1560.0);
        let first = [Vec3::xz(4416.0, 2624.0), Vec3::xz(4416.0, 2240.0), home];
        let r = turn_radius(0.0044, 0.005, 0.008, F, S);
        assert!(!flyable(Vec3::xz(4390.0, 3034.0), Vec3::xz(4.4, -4.4), r, 136.0, &first).contains(&first[0]));
        let second = [Vec3::xz(3840.0, 2496.0), Vec3::xz(3840.0, 2112.0), Vec3::xz(3840.0, 2048.0), home];
        let flown = flyable(Vec3::xz(3856.0, 2886.0), Vec3::xz(3.4, 5.2), r, 136.0, &second);
        assert!(!flown.contains(&second[0]) && !flown.contains(&second[1]), "{flown:?}");
    }
}
