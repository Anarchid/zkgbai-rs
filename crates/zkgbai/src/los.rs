//! Line-of-sight grid cache (the original `LosManager`).

use springai::{Callback, Vec3};

pub struct LosManager {
    cb: Callback,
    los_map: Vec<i32>,
    /// Map size in heightmap squares (8 elmos).
    map_width: i32,
    map_height: i32,
    /// LOS map size in cells.
    grid_width: i32,
    grid_height: i32,
}

impl LosManager {
    pub fn new(cb: Callback) -> Self {
        let map = cb.map();
        let map_width = map.width();
        let map_height = map.height();
        let los_resolution = cb.mod_info().los_mip_level();
        // Heightmap squares per LOS cell, per axis.
        let los_grid_size = 1i32 << los_resolution.clamp(0, 30);
        let mut los_map = Vec::new();
        cb.map().los_map_into(&mut los_map);
        LosManager {
            cb,
            los_map,
            map_width,
            map_height,
            grid_width: map_width / los_grid_size,
            grid_height: map_height / los_grid_size,
        }
    }

    /// Module event: refresh the cached LOS map when due (every frame).
    pub fn update(&mut self, _frame: i32) {
        self.cb.map().los_map_into(&mut self.los_map);
    }

    pub fn is_in_los(&self, position: Vec3) -> bool {
        self.is_in_los_level(position, 0)
    }

    /// Whether each of `positions` is in our radar now (the radar map is
    /// read for the call).
    pub fn in_radar(&self, positions: &[Vec3]) -> Vec<bool> {
        let radar = self.cb.map().radar_map();
        let cell = 1i32 << self.cb.mod_info().radar_mip_level().clamp(0, 30);
        let (gw, gh) = (self.map_width / cell, self.map_height / cell);
        (positions.iter())
            .map(|&p| los_index(p, self.map_width, self.map_height, gw, gh, radar.len()).is_some_and(|i| radar[i] > 0))
            .collect()
    }

    /// LOS check requiring at least `level` overlapping LOS sources.
    pub fn is_in_los_level(&self, position: Vec3, level: i32) -> bool {
        los_index(position, self.map_width, self.map_height, self.grid_width, self.grid_height, self.los_map.len())
            .is_some_and(|i| self.los_map[i] > level)
    }
}

/// Index into the LOS map for a world position, `None` if it is off the
/// low edges of the map or there is no LOS data. Positions past the far
/// edges clamp to the last cell, as in the original.
fn los_index(
    position: Vec3,
    map_width: i32,
    map_height: i32,
    grid_width: i32,
    grid_height: i32,
    len: usize,
) -> Option<usize> {
    //the value for the full resolution position (x, z) is at index ((z * width + x) / res) -
    //the last value, bottom right, is at index (width/res * height/res - 1)
    if len == 0 || map_width <= 0 || map_height <= 0 {
        return None;
    }

    // convert from world coordinates to heightmap coordinates
    let x = (position.x / 8.0).floor() as f64;
    let z = (position.z / 8.0).floor() as f64;

    let grid_x = ((x / map_width as f64) * grid_width as f64).floor() as i64;
    let grid_z = ((z / map_height as f64) * grid_height as f64).floor() as i64;
    if grid_x < 0 || grid_z < 0 {
        return None;
    }

    let index = grid_x.saturating_add(grid_z.saturating_mul(grid_width as i64));
    Some((index as u64).min(len as u64 - 1) as usize)
}

#[cfg(test)]
mod tests {
    use super::los_index;
    use springai::Vec3;

    #[test]
    fn index_mapping() {
        // 64 x 32 heightmap squares at LOS mip 2: 16 x 8 cells of 32 elmos.
        let idx = |x, z| los_index(Vec3::new(x, 0.0, z), 64, 32, 16, 8, 128);
        assert_eq!(idx(0.0, 0.0), Some(0));
        assert_eq!(idx(31.9, 31.9), Some(0));
        assert_eq!(idx(32.0, 0.0), Some(1));
        assert_eq!(idx(40.0, 70.0), Some(2 * 16 + 1));
        assert_eq!(idx(-1.0, 10.0), None);
        assert_eq!(idx(10.0, -1.0), None);
        // Past the far edge: clamped to the last cell.
        assert_eq!(idx(1e9, 1e9), Some(127));
        assert_eq!(los_index(Vec3::ZERO, 64, 32, 16, 8, 0), None);
    }
}
