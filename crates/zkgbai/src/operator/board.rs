//! The board: the map as a small picture for a pilot that reads images.
//!
//! Terrain in grey by height (water blue), a grid every 1000 elmos labelled
//! in thousands along the top and left edges, metal spots as rings (ours
//! blue, the enemy's red, free white), our units as dots (workers green,
//! fighters cyan, buildings blue, the commander yellow), enemies as red dots
//! (orange once out of sight, dimmer the longer ago), enemy static defenses
//! as red squares with a faint ring of their reach, squads as cyan lines to
//! their targets, places defended as magenta rings. The terrain is sampled
//! once and kept; everything else is drawn fresh each time.

use std::io::Cursor;

use springai::Vec3;

use crate::Ai;
use crate::operator::views::role;

/// Picture width in pixels; the height follows the map's aspect.
pub const WIDTH: usize = 512;

/// The cached terrain layer.
pub struct Terrain {
    pub w: usize,
    pub h: usize,
    rgb: Vec<[u8; 3]>,
}

struct Canvas {
    w: usize,
    h: usize,
    px: Vec<[u8; 3]>,
    /// Pixels per elmo.
    scale: f32,
}

impl Canvas {
    fn put(&mut self, x: i32, y: i32, c: [u8; 3]) {
        if x >= 0 && y >= 0 && (x as usize) < self.w && (y as usize) < self.h {
            self.px[y as usize * self.w + x as usize] = c;
        }
    }

    fn blend(&mut self, x: i32, y: i32, c: [u8; 3], alpha: f32) {
        if x >= 0 && y >= 0 && (x as usize) < self.w && (y as usize) < self.h {
            let p = &mut self.px[y as usize * self.w + x as usize];
            for i in 0..3 {
                p[i] = (p[i] as f32 * (1.0 - alpha) + c[i] as f32 * alpha).round() as u8;
            }
        }
    }

    fn at(&self, p: Vec3) -> (i32, i32) {
        ((p.x * self.scale).round() as i32, (p.z * self.scale).round() as i32)
    }

    fn disc(&mut self, x: i32, y: i32, r: i32, c: [u8; 3]) {
        for dy in -r..=r {
            for dx in -r..=r {
                if dx * dx + dy * dy <= r * r {
                    self.put(x + dx, y + dy, c);
                }
            }
        }
    }

    fn ring(&mut self, x: i32, y: i32, r: i32, c: [u8; 3]) {
        let n = (r * 8).max(12);
        for i in 0..n {
            let a = i as f32 / n as f32 * std::f32::consts::TAU;
            self.put(x + (r as f32 * a.cos()).round() as i32, y + (r as f32 * a.sin()).round() as i32, c);
        }
    }

    fn faint_disc(&mut self, x: i32, y: i32, r: i32, c: [u8; 3], alpha: f32) {
        for dy in -r..=r {
            for dx in -r..=r {
                if dx * dx + dy * dy <= r * r {
                    self.blend(x + dx, y + dy, c, alpha);
                }
            }
        }
    }

    fn square(&mut self, x: i32, y: i32, r: i32, c: [u8; 3]) {
        for dy in -r..=r {
            for dx in -r..=r {
                self.put(x + dx, y + dy, c);
            }
        }
    }

    fn line(&mut self, (mut x0, mut y0): (i32, i32), (x1, y1): (i32, i32), c: [u8; 3]) {
        let (dx, dy) = ((x1 - x0).abs(), -(y1 - y0).abs());
        let (sx, sy) = (if x0 < x1 { 1 } else { -1 }, if y0 < y1 { 1 } else { -1 });
        let mut err = dx + dy;
        for _ in 0..(self.w + self.h) {
            self.put(x0, y0, c);
            if x0 == x1 && y0 == y1 {
                break;
            }
            let e2 = 2 * err;
            if e2 >= dy {
                err += dy;
                x0 += sx;
            }
            if e2 <= dx {
                err += dx;
                y0 += sy;
            }
        }
    }

    /// A 3x5 glyph for digits and 'k'.
    fn glyph(&mut self, x: i32, y: i32, ch: char, c: [u8; 3]) {
        const DIGITS: [[u8; 5]; 10] = [
            [0b111, 0b101, 0b101, 0b101, 0b111],
            [0b010, 0b110, 0b010, 0b010, 0b111],
            [0b111, 0b001, 0b111, 0b100, 0b111],
            [0b111, 0b001, 0b111, 0b001, 0b111],
            [0b101, 0b101, 0b111, 0b001, 0b001],
            [0b111, 0b100, 0b111, 0b001, 0b111],
            [0b111, 0b100, 0b111, 0b101, 0b111],
            [0b111, 0b001, 0b010, 0b010, 0b010],
            [0b111, 0b101, 0b111, 0b101, 0b111],
            [0b111, 0b101, 0b111, 0b001, 0b111],
        ];
        const K: [u8; 5] = [0b100, 0b101, 0b110, 0b101, 0b101];
        let rows = match ch {
            '0'..='9' => DIGITS[ch as usize - '0' as usize],
            'k' => K,
            _ => return,
        };
        for (dy, row) in rows.iter().enumerate() {
            for dx in 0..3 {
                if row & (0b100 >> dx) != 0 {
                    self.put(x + dx, y + dy as i32, c);
                }
            }
        }
    }

    fn text(&mut self, x: i32, y: i32, s: &str, c: [u8; 3]) {
        for (i, ch) in s.chars().enumerate() {
            self.glyph(x + i as i32 * 4, y, ch, c);
        }
    }
}

/// Sample the terrain into a background layer (once per run; terraforming
/// is not redrawn).
pub fn terrain(ai: &Ai) -> Terrain {
    let map = ai.cb.map();
    let (mw, mh) = (map.width() as f32 * 8.0, map.height() as f32 * 8.0);
    let w = WIDTH;
    let h = ((w as f32 * mh / mw).round() as usize).clamp(64, 1024);
    let scale = w as f32 / mw;
    // Height range over a coarse sample, between the 5th and 95th
    // percentiles so a few peaks do not flatten the rest.
    let mut sample: Vec<f32> = Vec::with_capacity(w * h / 64);
    for y in (0..h).step_by(8) {
        for x in (0..w).step_by(8) {
            let e = map.elevation_at(x as f32 / scale, y as f32 / scale);
            if e >= 0.0 {
                sample.push(e);
            }
        }
    }
    sample.sort_by(|a, b| a.total_cmp(b));
    let pick = |q: f32| sample.get(((sample.len() as f32 - 1.0) * q) as usize).copied().unwrap_or(0.0);
    let (lo, hi) = (pick(0.05), pick(0.95));
    let span = (hi - lo).max(1.0);
    let mut rgb = vec![[0u8; 3]; w * h];
    for y in 0..h {
        for x in 0..w {
            let e = map.elevation_at((x as f32 + 0.5) / scale, (y as f32 + 0.5) / scale);
            rgb[y * w + x] = if e < 0.0 {
                let d = ((-e) / 100.0).clamp(0.0, 1.0);
                [20, (60.0 - 30.0 * d) as u8, (110.0 - 40.0 * d) as u8]
            } else {
                // Quantized to 16 steps: flat areas compress to almost nothing.
                let g = (60.0 + 130.0 * ((e - lo) / span).clamp(0.0, 1.0)) as u8 / 8 * 8;
                [g, g, g.saturating_sub(10)]
            };
        }
    }
    Terrain { w, h, rgb }
}

/// Draw the board over the terrain; PNG bytes.
pub fn render(ai: &Ai, terrain: &Terrain) -> Vec<u8> {
    let map = ai.cb.map();
    let mw = map.width() as f32 * 8.0;
    let mut c = Canvas { w: terrain.w, h: terrain.h, px: terrain.rgb.clone(), scale: terrain.w as f32 / mw };
    let grid = [120u8, 120, 120];
    let label = [230u8, 230, 230];
    // Grid every 1000 elmos, labelled in thousands.
    let mut k = 1;
    while (k as f32 * 1000.0 * c.scale) < c.w as f32 {
        let x = (k as f32 * 1000.0 * c.scale).round() as i32;
        for y in 0..c.h as i32 {
            if y % 3 == 0 {
                c.put(x, y, grid);
            }
        }
        c.text(x + 2, 1, &format!("{k}k"), label);
        k += 1;
    }
    let mut k = 1;
    while (k as f32 * 1000.0 * c.scale) < c.h as f32 {
        let y = (k as f32 * 1000.0 * c.scale).round() as i32;
        for x in 0..c.w as i32 {
            if x % 3 == 0 {
                c.put(x, y, grid);
            }
        }
        c.text(1, y + 2, &format!("{k}k"), label);
        k += 1;
    }
    let active = ai.active();
    // Enemy static defenses' reach, faint, under everything else.
    if active {
        for e in ai.war.targets.values() {
            if e.is_static && e.threat_radius > 0.0 && e.power > 0.0 {
                let (x, y) = c.at(e.position);
                c.faint_disc(x, y, (e.threat_radius * c.scale) as i32, [255, 60, 60], 0.12);
            }
        }
        // Metal spots.
        for s in &ai.graph.metal_spots {
            let (x, y) = c.at(s.get_pos());
            let col = if s.owned {
                [80, 140, 255]
            } else if s.hostile {
                [255, 70, 70]
            } else {
                [235, 235, 235]
            };
            c.ring(x, y, 4, col);
        }
        // Places defended.
        for (d, _) in ai.war.defense_targets() {
            let (x, y) = c.at(d.position);
            c.ring(x, y, 7, [255, 80, 255]);
            c.ring(x, y, 8, [255, 80, 255]);
        }
        // Squads: a line to the target.
        for sid in ai.squads.ids() {
            if let Some(s) = ai.squads.get(sid) {
                let p = s.get_pos(&ai.fighters);
                if s.target != crate::util::NULLPOS && p != crate::util::NULLPOS {
                    c.line(c.at(p), c.at(s.target), [0, 220, 220]);
                }
            }
        }
        // Enemies.
        for e in ai.war.targets.values() {
            let (x, y) = c.at(e.position);
            let age = (ai.frame - e.last_seen).max(0) as f32 / 30.0;
            let col = if e.visible || e.is_radar_visible {
                [255, 40, 40]
            } else {
                let f = 1.0 - (age / 300.0).min(0.7);
                [(255.0 * f) as u8, (120.0 * f) as u8, 0]
            };
            if e.is_static {
                c.square(x, y, if e.power > 0.0 { 2 } else { 1 }, col);
            } else {
                c.disc(x, y, if e.is_important { 2 } else { 1 }, col);
            }
        }
    }
    // Our units.
    for u in ai.cb.team_units() {
        let (x, y) = c.at(u.pos());
        let r = role(ai, u);
        if r.starts_with("worker") || r.starts_with("commander") {
            let com = ai.eco.workers.get(&u.id()).is_some_and(|w| w.is_com);
            if com {
                c.disc(x, y, 3, [255, 230, 40]);
            } else {
                c.disc(x, y, 2, [60, 230, 60]);
            }
        } else if r.starts_with("building") || r.starts_with("factory") || u.def().is_some_and(|d| d.speed() <= 0.0) {
            c.square(x, y, 1, [70, 110, 255]);
        } else {
            c.disc(x, y, 1, [0, 255, 255]);
        }
    }
    encode(&c)
}

fn encode(c: &Canvas) -> Vec<u8> {
    let mut out = Vec::with_capacity(c.w * c.h / 4);
    {
        let mut enc = png::Encoder::new(Cursor::new(&mut out), c.w as u32, c.h as u32);
        enc.set_color(png::ColorType::Rgb);
        enc.set_depth(png::BitDepth::Eight);
        enc.set_compression(png::Compression::default());
        let mut w = match enc.write_header() {
            Ok(w) => w,
            Err(_) => return Vec::new(),
        };
        let flat: Vec<u8> = c.px.iter().flat_map(|p| p.iter().copied()).collect();
        if w.write_image_data(&flat).is_err() {
            return Vec::new();
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canvas_draws_and_encodes() {
        let mut c = Canvas { w: 64, h: 48, px: vec![[0; 3]; 64 * 48], scale: 0.01 };
        c.disc(10, 10, 3, [255, 0, 0]);
        c.ring(30, 20, 5, [0, 255, 0]);
        c.line((0, 0), (63, 47), [0, 0, 255]);
        c.text(2, 40, "12k", [255, 255, 255]);
        assert_eq!(c.px[10 * 64 + 10], [255, 0, 0]);
        assert_eq!(c.px[47 * 64 + 63], [0, 0, 255]);
        assert_eq!(c.px[40 * 64 + 3], [255, 255, 255]);
        c.put(-1, 1000, [1, 1, 1]);
        let png = encode(&c);
        assert_eq!(&png[1..4], b"PNG");
        assert!(png.len() < 64 * 48 * 3);
    }
}
