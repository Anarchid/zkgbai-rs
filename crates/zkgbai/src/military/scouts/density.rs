//! Map-wide density forecast from the Claude-side Wires research:
//! `zkgbai-runs/wires/ESCALATION.md` §§4,8 and `teamgames/teamfit.py`.
//! Standing metal = N * 1100 + survival(t) * cumulative income(t).

use springai::Callback;

use crate::graph::MetalSpot;

const HEIGHT_CELL: f32 = 128.0;

pub(super) struct DensityMap {
    players: f64,
    spot_income: f64,
    area: f64,
}

impl DensityMap {
    pub fn new(cb: Callback, spots: &[MetalSpot], players: usize) -> Self {
        let map = cb.map();
        let w = (map.width().max(0) as usize / 16).max(1);
        let h = (map.height().max(0) as usize / 16).max(1);
        // Match the research capture's 128-elmo height grid, in elmos,
        // and its land/slope mask rather than the whole map rectangle.
        let heights: Vec<f32> = (0..h)
            .flat_map(|z| {
                (0..w).map(move |x| map.elevation_at((x as f32 + 0.5) * HEIGHT_CELL, (z as f32 + 0.5) * HEIGHT_CELL))
            })
            .collect();
        let land = usable_area(&heights, w, h);
        // Pure sea maps have no bot-usable land. Use the map area there
        // instead of dividing by zero and requesting a maximum-size fleet.
        let area = if land > 0.0 { land } else { (w * h) as f64 * (HEIGHT_CELL as f64).powi(2) / 1e6 };
        Self { players: players.max(1) as f64, spot_income: spots.iter().map(|s| s.value.max(0.0) as f64).sum(), area }
    }

    pub fn at_frame(&self, frame: i32) -> f32 {
        self.density(frame.max(0) as f64 / 1800.0) as f32
    }

    fn density(&self, minutes: f64) -> f64 {
        // The pooled fit transfers across team sizes. For 1v1, use the
        // separately fitted arena-AI curve: the pooled fit was ~5 min late
        // reaching D=1500, while AIs expand and retain metal faster.
        let (a, beta, g, c0, c1) =
            if self.players <= 2.0 { (5.5, 1.35, 0.02, 1.97, 0.41) } else { (7.0, 0.95, 0.03, 1.95, 0.45) };
        let rate = a * self.players.powf(beta);
        // The fit integrates right endpoints every ten seconds. Include a
        // final partial step so arbitrary build-completion frames are smooth.
        let mut cumulative = 0.0;
        let mut t = 0.0;
        while t < minutes {
            let dt = (minutes - t).min(1.0 / 6.0);
            t += dt;
            let mex = if self.spot_income > 0.0 {
                self.spot_income
                    * (1.0 + g * (t - self.spot_income / rate).max(0.0))
                    * (1.0 - (-rate * t / self.spot_income).exp())
            } else {
                0.0
            };
            cumulative += (6.0 * self.players + mex) * dt * 60.0;
        }
        let survival = if minutes > 0.0 { (c0 * minutes.powf(-c1)).min(1.0) } else { 1.0 };
        (self.players * 1100.0 + survival * cumulative) / self.area
    }
}

/// Same finite differences as numpy.gradient in teamfit.py (one-sided
/// edges, centred interior), WATER=-22 and BOT_SLOPE=0.412.
fn usable_area(heights: &[f32], w: usize, h: usize) -> f64 {
    let mut cells = 0;
    for z in 0..h {
        for x in 0..w {
            let left = x.saturating_sub(1);
            let right = (x + 1).min(w - 1);
            let up = z.saturating_sub(1);
            let down = (z + 1).min(h - 1);
            let dx = (heights[z * w + right] - heights[z * w + left]) / ((right - left).max(1) as f32 * HEIGHT_CELL);
            let dz = (heights[down * w + x] - heights[up * w + x]) / ((down - up).max(1) as f32 * HEIGHT_CELL);
            let slope = 1.0 - 1.0 / (1.0 + dx * dx + dz * dz).sqrt();
            if heights[z * w + x] > -22.0 && slope <= 0.412 {
                cells += 1;
            }
        }
    }
    cells as f64 * (HEIGHT_CELL as f64).powi(2) / 1e6
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forecast_grows_earlier_with_players_and_on_smaller_maps() {
        let duel = DensityMap { players: 2.0, spot_income: 182.6, area: 50.3 };
        let team = DensityMap { players: 16.0, ..duel };
        assert!((duel.density(0.0) - 2200.0 / 50.3).abs() < 1e-6);
        assert!(duel.density(5.0) < duel.density(10.0));
        assert!(duel.density(10.0) < duel.density(20.0));
        assert!(team.density(5.0) > duel.density(10.0));
        let small = DensityMap { area: 25.15, ..duel };
        assert!((small.density(10.0) - 2.0 * duel.density(10.0)).abs() < 1e-6);
        let no_mex = DensityMap { spot_income: 0.0, ..duel };
        assert!(no_mex.density(10.0).is_finite());
        assert!(no_mex.density(10.0) > no_mex.density(0.0));
    }

    #[test]
    fn forecast_matches_the_independent_python_research_at_whole_minutes() {
        // teamfit.predict with N=2/16, C=1100, M=182.6, U=50.3;
        // arena-AI coefficients for 1v1, pooled coefficients for teams.
        for (players, expected) in [
            (2.0, [305.761496, 664.941712, 1056.462785, 1492.290455]),
            (16.0, [1605.023911, 2527.595270, 3322.751321, 4063.255383]),
        ] {
            let map = DensityMap { players, spot_income: 182.6, area: 50.3 };
            for (minute, value) in [5.0, 10.0, 15.0, 20.0].into_iter().zip(expected) {
                assert!((map.density(minute) - value).abs() < 1e-5);
            }
        }
    }

    #[test]
    fn usable_area_excludes_water_and_cliffs_and_keeps_shallow_water() {
        let cell = (HEIGHT_CELL as f64).powi(2) / 1e6;
        assert_eq!(usable_area(&[0.0; 9], 3, 3), 9.0 * cell);
        assert_eq!(usable_area(&[-23.0; 9], 3, 3), 0.0);
        assert_eq!(usable_area(&[-21.0; 9], 3, 3), 9.0 * cell);
        assert_eq!(usable_area(&[0.0, 256.0, 512.0, 0.0, 256.0, 512.0, 0.0, 256.0, 512.0], 3, 3), 0.0);
    }
}
