//! Additive circle-painting grids used for threat/value/territory maps
//! (the original `ArrayGraphics` and `ByteArrayGraphics`).

/// Integer grid saturating at 32767.
#[derive(Clone, Debug)]
pub struct ArrayGraphics {
    data: Vec<i32>,
    pub width: i32,
    pub height: i32,
}

/// Small-integer grid saturating at 127.
#[derive(Clone, Debug)]
pub struct ByteArrayGraphics {
    data: Vec<i8>,
    pub width: i32,
    pub height: i32,
}

/// Visit every cell index inside the circle, clipped to the grid.
#[inline]
fn for_circle(width: i32, height: i32, len: usize, cx: i32, cy: i32, radius: i32, mut f: impl FnMut(usize)) {
    let begin_x = (cx - radius + 1).max(0);
    let end_x = (cx + radius).min(width);
    let begin_y = (cy - radius + 1).max(0);
    let end_y = (cy + radius).min(height);
    let radsq = radius * radius;
    for y in begin_y..end_y {
        let dy = cy - y;
        let dysq = dy * dy;
        let ypos = y * width;
        for x in begin_x..end_x {
            let dx = cx - x;
            if dx * dx + dysq <= radsq {
                let index = ypos + x;
                if index >= 0 && (index as usize) < len {
                    f(index as usize);
                }
            }
        }
    }
}

impl ArrayGraphics {
    pub fn new(width: i32, height: i32) -> Self {
        Self { data: vec![0; (width.max(0) * height.max(0)) as usize], width, height }
    }

    pub fn clear(&mut self) {
        self.data.fill(0);
    }

    pub fn paint_circle(&mut self, cx: i32, cy: i32, radius: i32, intensity: i32) {
        if intensity <= 0 {
            return;
        }
        let data = &mut self.data;
        for_circle(self.width, self.height, data.len(), cx, cy, radius, |i| {
            data[i] = data[i].saturating_add(intensity);
            if data[i] < 0 || data[i] > 32767 {
                data[i] = 32767;
            }
        });
    }

    /// Add a linear falloff outside an existing hard circle, without raising
    /// threat inside it. Radii are in grid cells; width may be fractional.
    pub fn paint_soft_band(&mut self, cx: i32, cy: i32, inner: f32, width: f32, intensity: i32) {
        if width <= 0.0 || !width.is_finite() || intensity <= 0 {
            return;
        }
        let outer = inner + width;
        let grid_width = self.width;
        let data = &mut self.data;
        for_circle(self.width, self.height, data.len(), cx, cy, outer.ceil() as i32, |i| {
            let dx = (i as i32 % grid_width - cx) as f32;
            let dy = (i as i32 / grid_width - cy) as f32;
            let distance = (dx * dx + dy * dy).sqrt();
            if distance > inner && distance < outer {
                let amount = (intensity as f32 * (outer - distance) / width).round() as i32;
                data[i] = data[i].saturating_add(amount).min(32767);
            }
        });
    }

    pub fn unpaint_circle(&mut self, cx: i32, cy: i32, radius: i32, intensity: i32) {
        if intensity <= 0 {
            return;
        }
        let data = &mut self.data;
        for_circle(self.width, self.height, data.len(), cx, cy, radius, |i| {
            data[i] = (data[i] - intensity).max(0);
        });
    }

    pub fn get_value(&self, x: i32, y: i32) -> f32 {
        if x < 0 || y < 0 {
            return 0.0;
        }
        let idx = (x + y * self.width) as usize;
        self.data.get(idx).map_or(0.0, |&v| v as f32)
    }
}

impl ByteArrayGraphics {
    pub fn new(width: i32, height: i32) -> Self {
        Self { data: vec![0; (width.max(0) * height.max(0)) as usize], width, height }
    }

    pub fn clear(&mut self) {
        self.data.fill(0);
    }

    pub fn paint_circle(&mut self, cx: i32, cy: i32, radius: i32, intensity: i32) {
        if intensity <= 0 {
            return;
        }
        let data = &mut self.data;
        for_circle(self.width, self.height, data.len(), cx, cy, radius, |i| {
            // The original relied on signed-char overflow to detect saturation.
            data[i] = (data[i] as i32 + intensity).min(127) as i8;
        });
    }

    pub fn unpaint_circle(&mut self, cx: i32, cy: i32, radius: i32, intensity: i32) {
        if intensity <= 0 {
            return;
        }
        let data = &mut self.data;
        for_circle(self.width, self.height, data.len(), cx, cy, radius, |i| {
            data[i] = (data[i] as i32 - intensity).max(0) as i8;
        });
    }

    pub fn get_value(&self, x: i32, y: i32) -> f32 {
        if x < 0 || y < 0 {
            return 0.0;
        }
        let idx = (x + y * self.width) as usize;
        self.data.get(idx).map_or(0.0, |&v| v as f32)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paint_and_unpaint_round_trip() {
        let mut g = ArrayGraphics::new(20, 20);
        g.paint_circle(10, 10, 3, 5);
        assert_eq!(g.get_value(10, 10), 5.0);
        assert_eq!(g.get_value(0, 0), 0.0);
        g.unpaint_circle(10, 10, 3, 5);
        assert_eq!(g.get_value(10, 10), 0.0);
    }

    #[test]
    fn mobile_aa_band_tapers_without_inflating_the_hard_circle() {
        let mut g = ArrayGraphics::new(30, 30);
        g.paint_circle(12, 12, 4, 100);
        g.paint_soft_band(12, 12, 4.0, 6.0, 100);
        assert_eq!(g.get_value(12, 12), 100.0);
        assert_eq!(g.get_value(15, 12), 100.0);
        assert!(g.get_value(17, 12) > g.get_value(20, 12));
        assert!(g.get_value(20, 12) > 0.0);
        assert_eq!(g.get_value(22, 12), 0.0);
        g.clear();
        g.paint_soft_band(0, 0, 4.0, 0.0, 100);
        assert_eq!(g.get_value(5, 0), 0.0);
        g.paint_soft_band(0, 0, 4.0, 3.0, 100);
        assert!(g.get_value(5, 0) > 0.0); // clipped map edge
    }

    #[test]
    fn byte_grid_saturates() {
        let mut g = ByteArrayGraphics::new(4, 4);
        for _ in 0..10 {
            g.paint_circle(1, 1, 2, 50);
        }
        assert_eq!(g.get_value(1, 1), 127.0);
    }

    #[test]
    fn out_of_bounds_reads_are_zero() {
        let g = ArrayGraphics::new(4, 4);
        assert_eq!(g.get_value(-1, 0), 0.0);
        assert_eq!(g.get_value(100, 100), 0.0);
    }
}
