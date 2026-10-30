//! Incoming weapon fire, separate from the strategic danger/health map used
//! to choose corridors. No enemy HP or splash multiplier masquerades as DPS.
use super::*;

#[derive(Clone, Copy)]
pub(super) struct FireSource {
    pos: Vec3,
    radius: f32,
    band: f32,
    dps: f32,
    burst: f32,
}
impl FireSource {
    fn coverage(self, pos: Vec3) -> f32 {
        let outside = distance(self.pos, pos) - self.radius;
        if outside <= 0.0 {
            1.0
        } else if self.band > 0.0 {
            (1.0 - outside / self.band).clamp(0.0, 1.0)
        } else {
            0.0
        }
    }
}

pub(super) fn forecast(from: Vec3, route: &[Vec3], sources: &[FireSource], interceptors: &[(Vec3, f32, f32)]) -> Risk {
    let mut touched = vec![0.0_f32; sources.len()];
    let mut risk = route_risk(
        from,
        route,
        |p| {
            let dps = sources
                .iter()
                .zip(&mut touched)
                .map(|(source, peak)| {
                    let coverage = source.coverage(p);
                    *peak = peak.max(coverage);
                    source.dps * coverage
                })
                .sum::<f32>();
            dps / 500.0 + interceptors.iter().filter(|(at, r, _)| distance(*at, p) < *r).map(|(_, _, t)| t).sum::<f32>()
        },
        |p| interceptors.iter().any(|(at, r, _)| distance(*at, p) < *r),
    );
    // One ready volley per encountered weapon per leg, not per sample. The
    // return leg reserves a fresh volley because the weapon may have reloaded.
    risk.burst = sources.iter().zip(touched).map(|(s, coverage)| s.burst * coverage).sum();
    risk
}

impl Ai {
    pub(super) fn airstrike_defenses(&self) -> Vec<FireSource> {
        // ZK aircraft share plane armor. Read it from installed definitions;
        // take the worst applicable damage if a mod gives them different armor.
        let planes: Vec<_> = Plane::ALL.into_iter().filter_map(|p| self.defs.get(p.name())).collect();
        let mut sources = Vec::new();
        for e in self.war.targets.values() {
            if e.is_dead || !e.identified || !(e.is_aa || e.is_flex_aa) || e.can_fly() {
                continue;
            }
            let Some(def) = e.ud else { continue };
            if e.visible
                && (e.unit.is_being_built() || e.unit.is_paralyzed() || e.unit.rules_param_float("disarmed", 0.0) > 0.0)
            {
                continue;
            }
            let fade = if e.is_static {
                1.0
            } else {
                (1.0 - ((self.frame - e.last_seen).max(0) as f32 / 1800.0).powi(2)).max(0.0)
            };
            for mount in def.weapon_mounts() {
                let Some(w) = mount.weapon_def() else { continue };
                if w.name().contains("fake")
                    || w.is_shield()
                    || w.interceptor() > 0
                    || w.weapon_type() == "TorpedoLauncher"
                {
                    continue;
                }
                let damages = w.damage_types();
                let damage = planes
                    .iter()
                    .filter(|p| mount.only_target_category() & p.category() != 0)
                    .filter_map(|p| damages.get(p.armor_type().max(0) as usize))
                    .copied()
                    .fold(0.0, f32::max);
                if damage <= 0.0 {
                    continue;
                }
                let burst = damage * w.salvo_size().max(1) as f32 * w.projectiles_per_shot().max(1) as f32;
                // EMP/disarm damage is counted conservatively as lost survivability.
                // Slow can at most double exposure; do not discount these effects.
                let slow = w.custom_param("timeslow_damagefactor").and_then(|s| s.parse::<f32>().ok()).unwrap_or(0.0);
                let extra = w.custom_param("extra_damage").and_then(|s| s.parse::<f32>().ok()).unwrap_or(0.0);
                let burst = (burst + extra.max(0.0)) * (1.0 + slow.clamp(0.0, 1.0)) * fade;
                sources.push(FireSource {
                    pos: e.position,
                    radius: if e.is_static { w.range() + 64.0 } else { w.range() * 1.25 + 32.0 },
                    band: if e.is_static { 0.0 } else { e.def_speed() * 3.0 },
                    dps: burst / w.reload().max(0.1),
                    burst,
                });
            }
        }
        // Unseen AA has uncertain position/count: retain its conservative
        // existing power estimate until a real carrier is identified.
        sources.extend(self.air_intel_threat().into_iter().map(|(pos, radius, power)| FireSource {
            pos,
            radius: radius + 64.0,
            band: 0.0,
            dps: power,
            burst: power,
        }));
        sources
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn roundtrip(dps: f32, burst: f32) -> Risk {
        let source = FireSource { pos: Vec3::ZERO, radius: 300.0, band: 0.0, dps, burst };
        let home = Vec3::xz(-600.0, 0.0);
        round_trip(forecast(home, &[Vec3::ZERO], &[source], &[]), forecast(Vec3::ZERO, &[home], &[source], &[]))
    }
    #[test]
    fn same_aa_distinguishes_raven_likho_and_odin() {
        let modest = roundtrip(140.0, 14.0);
        assert!(!modest.survives(1000.0, 246.0));
        assert!(modest.survives(2360.0, 270.0));
        assert!(modest.survives(5200.0, 185.0));
        let stronger = roundtrip(220.0, 22.0);
        assert!(!stronger.survives(2360.0, 270.0));
        assert!(stronger.survives(5200.0, 185.0));
        assert!(!roundtrip(900.0, 90.0).survives(5200.0, 185.0));
    }
    #[test]
    fn ready_volley_blocks_low_dps_heavy_missiles() {
        let risk = roundtrip(10.0, 1600.0);
        assert!(risk.burst >= 3200.0);
        assert!(!risk.survives(5200.0, 185.0));
    }
    #[test]
    fn speed_affects_release_allowance_as_well_as_transit() {
        let risk = roundtrip(140.0, 14.0);
        assert!(risk.damage(270.0) < risk.damage(185.0));
        assert!(risk.survives(2360.0, 270.0));
        assert!(!risk.survives(1200.0, 270.0));
    }
    #[test]
    fn mobile_band_tapers_and_uncovered_corridor_has_no_volley() {
        let source = FireSource { pos: Vec3::ZERO, radius: 100.0, band: 300.0, dps: 200.0, burst: 400.0 };
        assert_eq!(source.coverage(Vec3::xz(100.0, 0.0)), 1.0);
        assert_eq!(source.coverage(Vec3::xz(250.0, 0.0)), 0.5);
        assert_eq!(source.coverage(Vec3::xz(400.0, 0.0)), 0.0);
        let risk = forecast(Vec3::xz(500.0, 0.0), &[Vec3::xz(800.0, 0.0)], &[source], &[]);
        assert_eq!(risk.damage(200.0), 0.0);
    }
    #[test]
    fn volley_is_reserved_once_per_weapon_and_missing_return_is_unsafe() {
        let source = FireSource { pos: Vec3::ZERO, radius: 200.0, band: 0.0, dps: 10.0, burst: 100.0 };
        let risk = forecast(Vec3::ZERO, &[Vec3::xz(20.0, 0.0), Vec3::xz(60.0, 0.0)], &[source], &[]);
        assert_eq!(risk.burst, 100.0);
        assert!(!round_trip(risk, forecast(Vec3::ZERO, &[], &[source], &[])).survives(5200.0, 185.0));
    }
}
