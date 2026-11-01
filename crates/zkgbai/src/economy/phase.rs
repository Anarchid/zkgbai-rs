//! `densgate` (off by default; `enable=densgate`): the gates that only stand
//! in for how far the game has come run on map density instead of the clock
//! and our income (`zkgbai-runs/procure/DESIGN.md` §§9-10).
//!
//! Density at a given minute is mostly a property of the map (its spot
//! metal, usable area and player count): the opponent moves it by ~2-17%,
//! the map by 1.5-1.9x (DESIGN.md §9.2). So each map has its own clock: the
//! *phase minute* is the minute at which a reference map (a 1v1 fitted to
//! the measured density of four arena maps) had the density this map's
//! forecast has now. Time gates compare it in place of the game minute;
//! income gates compare our income scaled by what the reference map would
//! pay at the phase minute over what this map pays now. On a map like the
//! reference both are what they were; on small maps gates open earlier, on
//! big rich ones later. Gates for what we can afford (constructors,
//! striders, superweapons, turrets by income) keep the plain income.

use crate::Ai;
use crate::military::scouts::density::DensityMap;

impl Ai {
    /// Whether gates run on density (`enable=densgate`).
    pub(crate) fn phase_on(&self) -> bool {
        !self.dev_off("densgate")
    }

    /// (reference minutes, income scale) now, computed once a frame.
    fn phase(&self) -> (f32, f32) {
        let (frame, minutes, scale) = self.eco.phase_cache.get();
        if frame == self.frame {
            return (minutes, scale);
        }
        let (minutes, scale) = phase_of(self.density_forecast(), f64::from(self.frame.max(0)) / 1800.0);
        let (minutes, scale) = (minutes as f32, scale as f32);
        self.eco.phase_cache.set((self.frame, minutes, scale));
        (minutes, scale)
    }

    /// The game minute, or (`densgate`) the reference map's minute at our
    /// density now.
    pub(crate) fn phase_minutes(&self) -> f32 {
        if !self.phase_on() {
            return self.frame as f32 / 1800.0;
        }
        self.phase().0
    }

    /// [`Self::phase_minutes`] in frames: for the time gates.
    pub(crate) fn phase_frame(&self) -> i32 {
        if !self.phase_on() {
            return self.frame;
        }
        (self.phase().0 * 1800.0) as i32
    }

    /// Our static income as the reference map would have paid it at the
    /// phase minute: for the factory timing gates.
    pub(crate) fn phase_static_income(&self) -> f32 {
        if !self.phase_on() {
            return self.eco.static_income;
        }
        self.eco.static_income * self.phase().1
    }

    /// `adjusted_income` (static income per merged player plus a metal a
    /// minute) on the phase clock: for the choosers' unit gates.
    pub(crate) fn phase_income(&self) -> f32 {
        if !self.phase_on() {
            return self.eco.adjusted_income;
        }
        let (minutes, _) = self.phase();
        let merged = self.merged_allies as f32;
        self.phase_static_income() / (1.0 + merged) + (1.0 + merged) * minutes
    }
}

/// The phase on `map` `now` minutes in: the reference map's minute at the
/// density `map`'s forecast has now, and what the reference pays a player
/// then over what `map` pays now.
fn phase_of(map: &DensityMap, now: f64) -> (f64, f64) {
    let reference = DensityMap::reference();
    let minutes = reference.minutes_at(map.density(now));
    (minutes, reference.income_per_player(minutes) / map.income_per_player(now))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_reference_keeps_its_clock_and_maps_shift_it_by_density() {
        let reference = DensityMap::reference();
        for now in [3.0, 7.0, 12.0, 20.0] {
            let (minutes, scale) = phase_of(&reference, now);
            assert!((minutes - now).abs() < 1.0 / 30.0, "{now}: {minutes}");
            assert!((scale - 1.0).abs() < 0.01, "{now}: {scale}");
        }
        // TitanDuel (M 64, U 25.1) runs ahead, Comet Catcher (M 183, U 50.3)
        // behind (procure/translate: gates open earlier and later there).
        let titan = DensityMap::from_parts(2, 64.0, 25.1);
        let comet = DensityMap::from_parts(2, 183.0, 50.3);
        for now in [5.0, 10.0, 15.0] {
            assert!(phase_of(&titan, now).0 > now + 0.5, "titan {now}");
            assert!(phase_of(&comet, now).0 < now - 0.5, "comet {now}");
        }
    }
}
