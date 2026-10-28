use springai::{Callback, Vec3};

/// Cached terrain heights (as at the start of the game) for "find high
/// ground" queries, sight lines and shell paths (radar and artillery
/// placement).
#[derive(PartialEq)]
pub struct HeightMap {
    data: Vec<f32>,
    width: i32,
    height: i32,
}

impl HeightMap {
    pub fn new(cb: Callback) -> Self {
        let map = cb.map();
        HeightMap { data: map.height_map(), width: map.width(), height: map.height() }
    }

    /// The highest heightmap square within `r` of `pos` (y = 0); the map
    /// origin if the area lies entirely off the map.
    pub fn get_highest_point_in_radius(&self, pos: Vec3, r: f32) -> Vec3 {
        let cx = (pos.x / 8.0).round() as i32;
        let cy = (pos.z / 8.0).round() as i32;
        let radius = (r / 8.0).round() as i32;

        let begin_x = cx.saturating_sub(radius).saturating_add(1).max(0);
        let begin_y = cy.saturating_sub(radius).saturating_add(1).max(0);
        let end_x = cx.saturating_add(radius).min(self.width);
        let end_y = cy.saturating_add(radius).min(self.height);

        let mut max_height = f32::MIN;
        let mut first = true;
        let (mut max_x, mut max_y) = (0, 0);
        // technically this searches a square. Calculating a circle isn't difficult but it's probably expensive,
        // and for the usage here there's no harm and probably some benefit in searching the extra points.
        for x in begin_x..end_x {
            for y in begin_y..end_y {
                let Some(&tmpheight) = self.data.get((y * self.width + x) as usize) else {
                    continue;
                };
                if first || tmpheight > max_height {
                    max_height = tmpheight;
                    max_x = x;
                    max_y = y;
                    first = false;
                }
            }
        }
        Vec3::new(max_x as f32 * 8.0, 0.0, max_y as f32 * 8.0)
    }

    /// Terrain height at `(x, z)` (the nearest square, clamped to the map).
    pub fn height_at(&self, x: f32, z: f32) -> f32 {
        let cx = ((x / 8.0) as i32).clamp(0, self.width - 1);
        let cz = ((z / 8.0) as i32).clamp(0, self.height - 1);
        self.data.get((cz * self.width + cx) as usize).copied().unwrap_or(0.0)
    }

    /// Whether a sight line from `eye` elmos above the ground (or sea) at
    /// `from` reaches `target` elmos above it at `to`, no terrain between
    /// rising above it: how the engine's line of sight and radar see.
    pub fn sees(&self, from: Vec3, eye: f32, to: Vec3, target: f32) -> bool {
        let h0 = self.height_at(from.x, from.z).max(0.0) + eye;
        let h1 = self.height_at(to.x, to.z).max(0.0) + target;
        self.clears(from, to, |t, _| h0 + (h1 - h0) * t)
    }

    /// Whether a cannon shell fired at `speed` (elmos/s) under `gravity`
    /// (elmos/s²) from `muzzle` elmos above the ground at `from` reaches
    /// `target` elmos above it at `to` on its low arc without hitting the
    /// terrain between; false out of its reach.
    pub fn shell_clears(&self, from: Vec3, muzzle: f32, to: Vec3, target: f32, speed: f32, gravity: f32) -> bool {
        let h0 = self.height_at(from.x, from.z).max(0.0) + muzzle;
        let h = self.height_at(to.x, to.z).max(0.0) + target - h0;
        let d = ((to.x - from.x).powi(2) + (to.z - from.z).powi(2)).sqrt().max(1.0);
        let (v2, g) = (speed * speed, gravity.max(1.0));
        let disc = v2 * v2 - g * (g * d * d + 2.0 * h * v2);
        if disc < 0.0 {
            return false;
        }
        let tan = (v2 - disc.sqrt()) / (g * d);
        let fall = g * (1.0 + tan * tan) / (2.0 * v2);
        self.clears(from, to, |_, x| h0 + x * tan - fall * x * x)
    }

    /// Whether `line` (height at share `t` of the way, `x` elmos along)
    /// stays above the terrain between `from` and `to`, sampled every 16
    /// elmos.
    fn clears(&self, from: Vec3, to: Vec3, line: impl Fn(f32, f32) -> f32) -> bool {
        let d = ((to.x - from.x).powi(2) + (to.z - from.z).powi(2)).sqrt();
        let steps = (d / 16.0).ceil() as i32;
        (1..steps).all(|k| {
            let t = k as f32 / steps as f32;
            let (x, z) = (from.x + (to.x - from.x) * t, from.z + (to.z - from.z) * t);
            self.height_at(x, z) <= line(t, t * d) + 1.0
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map() -> HeightMap {
        // 4 x 3 squares, all below water except one peak.
        let mut data = vec![-50.0; 12];
        data[4 + 2] = -10.0;
        data[2 * 4 + 3] = 30.0;
        HeightMap { data, width: 4, height: 3 }
    }

    /// A 64-square strip, flat at 0 but for a ridge of `ridge` at squares
    /// 30-33.
    fn strip(ridge: f32) -> HeightMap {
        let mut data = vec![0.0; 64];
        for h in &mut data[30..34] {
            *h = ridge;
        }
        HeightMap { data, width: 64, height: 1 }
    }

    #[test]
    fn ridges_block_sight_and_low_shells() {
        let (a, b) = (Vec3::new(8.0, 0.0, 0.0), Vec3::new(500.0, 0.0, 0.0));
        assert!(strip(10.0).sees(a, 32.0, b, 10.0));
        assert!(!strip(60.0).sees(a, 32.0, b, 10.0));
        // High enough on a spike, over the ridge.
        assert!(!strip(60.0).sees(a, 32.0 + 64.0, b, 10.0));
        assert!(strip(60.0).sees(a, 32.0 + 128.0, b, 10.0));
        // A Bertha's shell (1050 elmos/s, gravity 130) flies flat over 500
        // elmos: a low ridge passes, a high one does not; out of reach is
        // out of reach.
        assert!(strip(20.0).shell_clears(a, 30.0, b, 20.0, 1050.0, 130.0));
        assert!(!strip(120.0).shell_clears(a, 30.0, b, 20.0, 1050.0, 130.0));
        assert!(!strip(0.0).shell_clears(a, 30.0, b, 20.0, 50.0, 130.0));
    }

    #[test]
    fn finds_peak_in_square() {
        assert_eq!(map().get_highest_point_in_radius(Vec3::new(24.0, 0.0, 16.0), 16.0), Vec3::new(24.0, 0.0, 16.0));
    }

    #[test]
    fn underwater_maximum_is_still_found() {
        // Squares x, y in 0..3 exclude the peak: the highest is the -10 one.
        assert_eq!(map().get_highest_point_in_radius(Vec3::new(8.0, 0.0, 8.0), 16.0), Vec3::new(16.0, 0.0, 8.0));
    }

    #[test]
    fn off_map_area_gives_origin() {
        assert_eq!(map().get_highest_point_in_radius(Vec3::new(-500.0, 0.0, -500.0), 16.0), Vec3::ZERO);
        assert_eq!(map().get_highest_point_in_radius(Vec3::new(5000.0, 0.0, 5000.0), 16.0), Vec3::ZERO);
    }
}
