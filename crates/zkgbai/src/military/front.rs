//! `frontgate` (on by default in the wip line; `disable=frontgate`):
//! composition gates on the density of our fights (`zkgbai-runs/procure/DESIGN.md` §17).
//!
//! Fights get denser as the armies grow: on open 1v1 maps the density the
//! average unit stands in is the total armed mobile metal over ~8 km-elmo²,
//! whatever the map's size, and more where fronts are narrow or teams big.
//! In even fights past ~2-4k metal per km-elmo² heavies lose a third to a
//! fifth of the share of their metal per minute that light units lose, so
//! the weight of the army should follow the density of its fights, not the
//! clock or the map.
//!
//! Measured live every [`PERIOD`] frames: over our armed ground units in
//! contact (known armed enemy metal within [`RADIUS`]), the metal-weighted
//! mean of the armed metal within [`RADIUS`] (ours but the unit itself, the
//! enemy's mobile units and turrets) per km-elmo². That is smoothed over
//! [`TAU_S`] seconds of contact and ratcheted. The escalation E rises from 0
//! at `front_lo` ([`LO`]) to 1 at `front_hi` ([`HI`]). Offline on 137 replays
//! against Wires v2 (`procure/combatdens/frontcal.py`) the ratchet reads 1080
//! when Wires finishes its first heavy (median minute 10.3), and passes 700
//! at ~8 min and 2000 at ~15, the span of Wires' climb from 8% to 33% heavy.
//!
//! Under the switch, owed raiders are dropped with chance E
//! ([`Ai::fac_front_cancel`]), and factories with a heavy pick it with chance
//! E x min(1, `front_heavy` / cost) once our income pays for it in
//! [`HEAVY_PAYBACK_S`] seconds ([`Ai::fac_front_heavy`]). The measurement
//! runs and goes in the journal's `front` tick field either way.

use crate::Ai;
use crate::util::kgb_rand;

/// Radius of a unit's neighbourhood, elmos (as the offline calibration).
pub(crate) const RADIUS: f32 = 500.0;
/// Smoothing time constant, seconds of contact.
const TAU_S: f32 = 90.0;
/// Frames between samples.
const PERIOD: i32 = 60;
/// Escalation starts at this fight density (metal per km-elmo²).
pub(crate) const LO: f32 = 700.0;
/// Escalation is full at this fight density.
pub(crate) const HI: f32 = 2000.0;
/// Metal of heavies per pick that reaches the roll, at full escalation.
/// Only ~a third of the tank factory's picks get past its constructors,
/// scouts, raider slots and AA (smoke game, 2026-11-02): 500 makes a
/// Cyclops (2200) ~23% of those, ~8% of all its picks, ~35-40% of its metal.
const HEAVY_METAL_PER_PICK: f32 = 500.0;
/// A heavy is picked only once our income pays for it in this long.
const HEAVY_PAYBACK_S: f32 = 90.0;

/// The fight-density measurement and its settings.
#[derive(Debug, Default)]
pub struct Front {
    /// Smoothed fight density, from the first contact on.
    pub ema: Option<f32>,
    /// The running maximum of `ema`.
    pub ratchet: f32,
    /// The last sample (`None` out of contact).
    pub raw: Option<f32>,
    /// Our armed ground metal in contact, and all of it, at the last sample.
    pub contact: (f32, f32),
    settings: Option<(f32, f32, f32)>,
}

impl Front {
    fn settings(&self) -> (f32, f32, f32) {
        self.settings.unwrap_or((LO, HI, HEAVY_METAL_PER_PICK))
    }

    /// E from the ratchet (whether or not the switch is on).
    pub fn escalation(&self) -> f32 {
        let (lo, hi, _) = self.settings();
        escalation(self.ratchet, lo, hi)
    }

    /// Fold in a sample taken `seconds` after the previous one.
    fn sample(&mut self, raw: Option<f32>, seconds: f32) {
        self.raw = raw;
        if let Some(r) = raw {
            let a = 1.0 - (-seconds / TAU_S).exp();
            let ema = self.ema.map_or(r, |e| e + (r - e) * a);
            self.ema = Some(ema);
            self.ratchet = self.ratchet.max(ema);
        }
    }
}

/// E: 0 below `lo`, 1 from `hi`, linear between.
pub(crate) fn escalation(ratchet: f32, lo: f32, hi: f32) -> f32 {
    ((ratchet - lo) / (hi - lo).max(1.0)).clamp(0.0, 1.0)
}

/// Over `ours` (x, z, metal) with enemy metal within `radius`, the
/// metal-weighted mean of the metal within `radius` (the others of ours and
/// `foes`) per km-elmo²; `None` if none of ours is in contact. Also the
/// metal of ours in contact.
pub(crate) fn fight_density(ours: &[(f32, f32, f32)], foes: &[(f32, f32, f32)], radius: f32) -> (Option<f32>, f32) {
    let r2 = radius * radius;
    let area = std::f32::consts::PI * r2 / 1.0e6;
    let near = |a: &(f32, f32, f32), b: &(f32, f32, f32)| (a.0 - b.0).powi(2) + (a.1 - b.1).powi(2) <= r2;
    let (mut weighted, mut metal) = (0.0f64, 0.0f64);
    for (i, u) in ours.iter().enumerate() {
        let enemy: f32 = foes.iter().filter(|f| near(u, f)).map(|f| f.2).sum();
        if enemy <= 0.0 {
            continue;
        }
        let own: f32 = (ours.iter().enumerate()).filter(|&(j, o)| j != i && near(u, o)).map(|(_, o)| o.2).sum();
        weighted += f64::from(u.2) * f64::from((own + enemy) / area);
        metal += f64::from(u.2);
    }
    ((metal > 0.0).then(|| (weighted / metal) as f32), metal as f32)
}

impl Ai {
    /// Whether composition follows the fight density (`frontgate`, on in
    /// the wip line; `disable=frontgate`).
    pub(crate) fn front_on(&self) -> bool {
        !self.dev_off("frontgate")
    }

    /// E under the switch, 0 without it.
    pub(crate) fn front_escalation(&self) -> f32 {
        if !self.front_on() {
            return 0.0;
        }
        self.war.front.escalation()
    }

    /// Sample the density of our fights (switch on or off).
    pub(crate) fn front_update(&mut self, frame: i32) {
        if frame % PERIOD != (self.offset + 11) % PERIOD {
            return;
        }
        if self.war.front.settings.is_none() {
            let opt = |k: &str, d: f32| self.cb.option(k).and_then(|v| v.trim().parse::<f32>().ok()).unwrap_or(d);
            self.war.front.settings =
                Some((opt("front_lo", LO), opt("front_hi", HI), opt("front_heavy", HEAVY_METAL_PER_PICK)));
        }
        let ours: Vec<(f32, f32, f32)> = (self.fighters.values())
            .filter(|f| f.metal_value > 0.0 && f.unit.def().is_some_and(|d| !d.is_able_to_fly()))
            .map(|f| {
                let p = f.unit.pos();
                (p.x, p.z, f.metal_value)
            })
            .collect();
        let metal = self.metal;
        let foes: Vec<(f32, f32, f32)> = (self.war.targets.values())
            .filter(|e| {
                e.identified
                    && !e.is_dead
                    && e.can_attack()
                    && !e.can_fly()
                    && !e.is_worker
                    && !e.is_com
                    && (e.visible || e.is_radar_visible || e.is_static)
            })
            .filter_map(|e| Some((e.position.x, e.position.z, e.ud?.cost(metal))))
            .collect();
        let (raw, in_contact) = fight_density(&ours, &foes, RADIUS);
        let front = &mut self.war.front;
        front.contact = (in_contact, ours.iter().map(|u| u.2).sum());
        front.sample(raw, PERIOD as f32 / 30.0);
    }

    /// `frontgate`: with chance E, drop one owed raider (`debt` < 0) instead
    /// of building it. True if dropped.
    pub(crate) fn fac_front_cancel(&self, debt: &mut f32) -> bool {
        if *debt >= 0.0 {
            return false;
        }
        let e = self.front_escalation();
        if e > 0.0 && kgb_rand() < f64::from(e) {
            *debt += 1.0;
            return true;
        }
        false
    }

    /// `frontgate`: the factory's heavy `name`, with chance
    /// E x min(1, `front_heavy` / its cost), once our income pays for it in
    /// [`HEAVY_PAYBACK_S`] seconds.
    pub(crate) fn fac_front_heavy(&self, name: &'static str) -> Option<&'static str> {
        let e = self.front_escalation();
        if e <= 0.0 {
            return None;
        }
        let cost = self.defs.get(name)?.cost(self.metal);
        if cost <= 0.0 || self.phase_income() * HEAVY_PAYBACK_S < cost {
            return None;
        }
        let (_, _, per_pick) = self.war.front.settings();
        (kgb_rand() < f64::from(e * (per_pick / cost).min(1.0))).then_some(name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn density_counts_only_units_in_contact_weighted_by_metal() {
        let area = std::f32::consts::PI * RADIUS * RADIUS / 1.0e6;
        // Two of ours together, one far off; an enemy next to the pair.
        let ours = [(0.0, 0.0, 100.0), (100.0, 0.0, 300.0), (5000.0, 0.0, 1000.0)];
        let foes = [(200.0, 0.0, 400.0)];
        let (raw, contact) = fight_density(&ours, &foes, RADIUS);
        assert_eq!(contact, 400.0);
        // Unit 0 sees 300 + 400, unit 1 sees 100 + 400.
        let want = (100.0 * 700.0 + 300.0 * 500.0) / 400.0 / area;
        assert!((raw.unwrap() - want).abs() < 1e-3, "{raw:?} {want}");
        assert_eq!(fight_density(&ours[2..], &foes, RADIUS), (None, 0.0));
    }

    #[test]
    fn escalation_ramps_and_the_ratchet_holds() {
        assert_eq!(escalation(500.0, LO, HI), 0.0);
        assert!((escalation(1350.0, LO, HI) - 0.5).abs() < 1e-6);
        assert_eq!(escalation(5000.0, LO, HI), 1.0);
        let mut f = Front::default();
        f.sample(Some(3000.0), 2.0);
        assert_eq!(f.ratchet, 3000.0);
        f.sample(Some(100.0), 2.0);
        f.sample(None, 2.0);
        assert!(f.ema.unwrap() < 3000.0 && f.ratchet == 3000.0);
        assert_eq!(f.escalation(), 1.0);
        // 90 s of contact at 2000 after a start at 0 reach ~63% of the way.
        let mut g = Front::default();
        g.sample(Some(0.0), 2.0);
        for _ in 0..45 {
            g.sample(Some(2000.0), 2.0);
        }
        assert!((g.ema.unwrap() - 2000.0 * (1.0 - (-1.0f32).exp())).abs() < 5.0, "{:?}", g.ema);
    }
}
