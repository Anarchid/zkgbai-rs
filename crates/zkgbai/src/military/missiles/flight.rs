//! Vertical launch prediction ported from Zero-K's gui_attack_aoe.lua,
//! CalculateVlaunchImpact. Keep the rise, non-accelerating turn, snap angle
//! and weapon-specific model offsets. Sample every eight elmos instead of
//! the widget's adaptive skips so a narrow ridge cannot disappear between samples.
use super::*;

#[derive(Clone, Copy)]
pub(super) struct Flight {
    pub range: f32,
    pub up_frames: i32,
    pub accel: f32,
    pub turn: f32,
    pub start: f32,
    pub end: f32,
    pub height: f32,
}

pub(super) struct Prediction {
    pub blocked: Option<Vec3>,
    pub frames: f32,
}

impl Flight {
    pub fn for_kind(ai: &Ai, kind: &str) -> Option<Self> {
        let def = ai.defs.get(kind)?.weapon_mounts().first()?.weapon_def()?;
        let height = match kind {
            EOS => -42.0,
            SHOCKLEY => -37.0,
            INFERNO | QUAKE => -47.0,
            "missileslow" => -60.0,
            _ => return None,
        };
        Some(Self {
            range: def.range() - 50.0,
            up_frames: (def.up_time() * 30.0 + 0.5).floor() as i32 - 2,
            accel: def.weapon_acceleration(),
            turn: def.turn_rate(),
            start: (def.start_velocity() + def.weapon_acceleration()).min(def.projectile_speed()),
            end: def.projectile_speed(),
            height,
        })
    }

    pub fn predict(self, from: Vec3, to: Vec3, ground: impl Fn(f32, f32) -> f32) -> Option<Prediction> {
        let d = distance(from, to);
        if !d.is_finite() || d > self.range || d < 100.0 || self.end <= 0.0 || self.turn <= 0.0 || self.up_frames < 0 {
            return None;
        }
        let (dx, dz) = ((to.x - from.x) / d, (to.z - from.z) / d);
        let (mut speed, mut y) = (self.start, self.height);
        for _ in 0..self.up_frames {
            speed = (speed + self.accel).min(self.end);
            y += speed;
        }
        let mut frames = self.up_frames as f32;
        let (mut pitch, mut hor) = (std::f32::consts::FRAC_PI_2, 0.0);
        let mut snapped = false;
        for _ in 0..1000 {
            pitch -= self.turn;
            let samples = (speed / 8.0).ceil().max(1.0) as usize;
            for k in 1..=samples {
                let step = speed * k as f32 / samples as f32;
                let h = hor + pitch.cos() * step;
                let (x, z) = (from.x + dx * h, from.z + dz * h);
                let gh = ground(x, z);
                if !gh.is_finite() {
                    return None;
                }
                if gh + 5.0 > from.y + y + pitch.sin() * step {
                    return Some(Prediction {
                        blocked: Some(Vec3::new(x, gh, z)),
                        frames: frames + k as f32 / samples as f32,
                    });
                }
            }
            hor += pitch.cos() * speed;
            y += pitch.sin() * speed;
            frames += 1.0;
            // The widget assumes no obstruction inside the turning circle.
            // We instead decline this unsupported geometry.
            if hor >= d || pitch < -1.7 {
                return None;
            }
            let aim = ((to.y - from.y - y) / (d - hor)).atan();
            if (aim - pitch).abs() < 0.14154 {
                pitch = aim;
                snapped = true;
                break;
            }
        }
        if !snapped {
            return None;
        }
        for _ in 0..1000 {
            let remaining = ((d - hor).powi(2) + (to.y - from.y - y).powi(2)).sqrt();
            if remaining <= 55.0 {
                return Some(Prediction { blocked: None, frames: frames + remaining / speed.max(0.01) });
            }
            speed = (speed + self.accel).min(self.end);
            let step = speed.min((remaining - 55.0).max(0.01));
            let samples = (step / 8.0).ceil().max(1.0) as usize;
            for k in 1..=samples {
                let s = step * k as f32 / samples as f32;
                let h = hor + pitch.cos() * s;
                let p = Vec3::new(from.x + dx * h, from.y + y + pitch.sin() * s, from.z + dz * h);
                let gh = ground(p.x, p.z);
                if !gh.is_finite() {
                    return None;
                }
                if gh + 5.0 > p.y {
                    return Some(Prediction { blocked: Some(Vec3::new(p.x, gh, p.z)), frames: frames + s / speed });
                }
            }
            hor += pitch.cos() * step;
            y += pitch.sin() * step;
            frames += step / speed;
        }
        None
    }
}

impl Ai {
    pub(super) fn missiles_prediction(&self, kind: &str, from: Vec3, mut at: Vec3) -> Option<Prediction> {
        let map = self.cb.map();
        at.y = map.elevation_at(at.x, at.z);
        Flight::for_kind(self, kind)?.predict(from, at, |x, z| map.elevation_at(x, z))
    }

    pub(super) fn missiles_reaches(&self, kind: &str, from: Vec3, at: Vec3) -> bool {
        self.missiles_prediction(kind, from, at).is_some_and(|p| p.blocked.is_none())
    }

    pub(super) fn missiles_eos_pad_reaches(&self, from: Vec3, id: i32, at: Vec3, delay: i32) -> bool {
        let Some(p) = self.missiles_prediction(EOS, from, at).filter(|p| p.blocked.is_none()) else { return false };
        self.war.targets.get(&id).is_some_and(|e| {
            !e.is_dead
                && (e.is_static
                    || (e.visible
                        && e.unit.is_paralyzed()
                        && stun_frames(e.unit.paralyze_damage(), e.unit.max_health()) > p.frames + delay as f32 + 60.0))
        })
    }
}

/// Zero-K uses max-health paralysis and a 40 second decline from 100% to 0%.
/// No extrapolation of a remembered stun through fog of war.
pub(super) fn stun_frames(emp: f32, max_health: f32) -> f32 {
    if max_health <= 0.0 || !emp.is_finite() || !max_health.is_finite() {
        return 0.0;
    }
    crate::military::fightsim::mech::paralysis_frames(emp / max_health) as f32
}

#[cfg(test)]
mod tests {
    use super::*;
    fn eos() -> Flight {
        Flight {
            range: 3450.0,
            up_frames: 88,
            accel: 0.2,
            turn: 18000.0 * std::f32::consts::TAU / 65536.0 / 30.0,
            start: 0.2,
            end: 40.0,
            height: -42.0,
        }
    }
    #[test]
    fn flat_ground_and_high_launch_clear_but_descent_ridge_blocks() {
        let from = Vec3::new(0.0, 30.0, 0.0);
        let to = Vec3::xz(3000.0, 0.0);
        let p = eos().predict(from, to, |_, _| 0.0).unwrap();
        assert!(p.blocked.is_none());
        assert!(p.frames > 180.0 && p.frames < 400.0);
        let p = eos().predict(from, to, |x, _| if (2600.0..2620.0).contains(&x) { 500.0 } else { 0.0 }).unwrap();
        assert!(p.blocked.is_some());
        assert!(eos().predict(from, Vec3::xz(3600.0, 0.0), |_, _| 0.0).is_none());
    }
    #[test]
    fn stun_requires_excess_over_full_health() {
        assert_eq!(stun_frames(1000.0, 1000.0), 0.0);
        assert_eq!(stun_frames(1500.0, 1000.0), 600.0);
        assert_eq!(stun_frames(1500.0, 0.0), 0.0);
        assert_eq!(stun_frames(f32::NAN, 1000.0), 0.0);
    }
}
