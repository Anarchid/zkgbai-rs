//! Plain-text rendering of the graph for debugging (the `kgbgraph` chat
//! command), drawn the same way as the original's `paintASCIIGraph`.

/// A character canvas whose rows grow on demand.
#[derive(Default)]
pub(super) struct Canvas {
    rows: Vec<Vec<u8>>,
}

impl Canvas {
    fn cell(&mut self, x: i32, y: i32) -> Option<&mut u8> {
        let (x, y) = (usize::try_from(x).ok()?, usize::try_from(y).ok()?);
        if self.rows.len() <= y {
            self.rows.resize(y + 1, Vec::new());
        }
        let row = &mut self.rows[y];
        if row.len() <= x {
            row.resize(x + 1, b' ');
        }
        Some(&mut row[x])
    }

    /// Draw a line of `c` from `(x1, y1)` to `(x2, y2)`, stepping one unit
    /// along the major axis and plotting at `1/r` scale. Like the original it
    /// stops as soon as either coordinate reaches its target, so lines along
    /// an axis are not drawn.
    pub(super) fn draw_line(&mut self, x1: i32, y1: i32, x2: i32, y2: i32, mut c: u8, r: i32) {
        let (mut cx, mut cy) = (x1 as f32, y1 as f32);
        let (mut dx, mut dy) = ((x2 - x1) as f32, (y2 - y1) as f32);
        if dx.abs() > dy.abs() {
            dy /= dx.abs();
            dx /= dx.abs();
        } else {
            dx /= dy.abs();
            dy /= dy.abs();
            if c == b'-' {
                c = b'|';
            }
        }
        loop {
            if cx < 0.0 || cy < 0.0 {
                return;
            }
            if cx as i32 == x2 || cy as i32 == y2 {
                return;
            }
            let x = (cx / r as f32).round() as i32;
            let y = (cy / r as f32).round() as i32;
            if let Some(cell) = self.cell(x, y) {
                *cell = match (c, *cell) {
                    (b'-', b'|') | (b'|', b'-') => b'+',
                    _ => c,
                };
            }
            cx += dx;
            cy += dy;
            if cx as i32 == x2 || cy as i32 == y2 {
                return;
            }
        }
    }

    /// Fill a disc of radius `size` around `(x, y)` with `c` at `1/r` scale;
    /// with `outline`, only its outermost unit-wide ring.
    pub(super) fn paint_circle(&mut self, x: i32, y: i32, size: i32, c: u8, r: i32, outline: bool) {
        for cx in (x - size)..(x + size) {
            for cy in (y - size)..(y + size) {
                if cx < 0 || cy < 0 {
                    continue;
                }
                let dx = (x - cx) as f32;
                let dy = (y - cy) as f32;
                let d = (dx * dx + dy * dy).sqrt();
                if d > size as f32 || (outline && d < (size - 1) as f32) {
                    continue;
                }
                let px = (cx as f32 / r as f32).round() as i32;
                let py = (cy as f32 / r as f32).round() as i32;
                if let Some(cell) = self.cell(px, py) {
                    *cell = c;
                }
            }
        }
    }

    /// The canvas as text: every row padded to the widest one, one line each.
    pub(super) fn render(&self) -> String {
        let w = self.rows.iter().map(Vec::len).max().unwrap_or(0);
        let mut out = String::with_capacity((w + 1) * self.rows.len());
        for row in &self.rows {
            out.extend(row.iter().map(|&b| b as char));
            out.extend(std::iter::repeat_n(' ', w - row.len()));
            out.push('\n');
        }
        out
    }
}
