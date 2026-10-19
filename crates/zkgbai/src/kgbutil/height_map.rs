use springai::{Callback, Vec3};

/// Cached terrain heights for "find high ground" queries (radar placement).
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
