//! Forward screening, anticipation and compact mustering before a dive.
use super::*;
use crate::util::get_directional_point;

pub(super) const MUSTER_SPACING: f32 = 35.0;
const FRONT_BACK: f32 = 250.0;
const FRONT_LIMIT: f32 = 1000.0;
const SCREEN_HALF: f32 = 800.0;
const ANTICIPATION: i32 = 30 * 45;

fn outside_target_fire(point: Vec3, target: Vec3, range: f32, muster: f32) -> bool {
    distance(point, target) > range + muster
}

pub(super) fn waiting_phase(tactic: Tactic, age: i32, widow_expected: bool) -> Phase {
    if tactic == Tactic::Halberd || age < ANTICIPATION || widow_expected { Phase::Forming } else { Phase::Screening }
}

pub(super) fn muster_radius(tactic: Tactic, n: usize) -> f32 {
    if tactic == Tactic::Halberd {
        formation_radius(n)
    } else {
        ((n as f32).sqrt().ceil() * MUSTER_SPACING + 75.0).max(180.0)
    }
}

/// A line tangent to the local front/approach, bounded in width, centered
/// on the group. Uses the original approach when standing on the target.
pub(super) fn line_point(center: Vec3, target: Vec3, origin: Vec3, i: usize, n: usize, spacing: f32) -> Vec3 {
    let from = if distance(center, target) > 1.0 { center } else { origin };
    let dx = target.x - from.x;
    let dz = target.z - from.z;
    let length = dx.hypot(dz);
    let (tx, tz) = if length > 1.0 { (-dz / length, dx / length) } else { (1.0, 0.0) };
    let step = spacing.min(2.0 * SCREEN_HALF / n.saturating_sub(1).max(1) as f32);
    let offset = (i as f32 - n.saturating_sub(1) as f32 * 0.5) * step;
    Vec3::xz(center.x + tx * offset, center.z + tz * offset)
}

impl Ai {
    pub(super) fn strike_stage_clear(&self, u: Unit, from: Vec3, p: Vec3) -> bool {
        self.strike_waiting_risk(p, 40.0) <= 0.0
            && !self.graph.is_enemy_territory(p)
            && self.war.get_porc_threat(p) <= 0.0
            && self.war.get_effective_threat(p) <= 0.0
            && u.def().is_some_and(|d| reaches(self.reach_terrain(), d, from, p, 80.0))
    }

    /// Prefer 600–950 from the job when that is also behind our front;
    /// deep targets stage just behind the nearest friendly front instead.
    pub(super) fn strike_stage(&self, w: &Wave) -> Option<Vec3> {
        let def = self.defs.get(w.tactic.name())?;
        let ally = self.graph.get_ally_center();
        // Inferred sniper fixes may not have a tracked unit/threat-map entry.
        // Keep the whole muster outside its weapon reach before committing.
        let phantom_range =
            if w.job.phantom() { self.defs.get("cloaksnipe").map_or(1200.0, |d| d.max_weapon_range()) } else { 0.0 };
        let front = self
            .graph
            .get_front_line_spots()
            .into_iter()
            .min_by(|a, b| distance(a.position, w.job.pos).total_cmp(&distance(b.position, w.job.pos)))
            .map(|s| s.position);
        let mut candidates = Vec::new();
        if let Some(front) = front {
            for back in [FRONT_BACK, 400.0, 650.0, 900.0] {
                candidates.push(get_directional_point(front, ally, back));
            }
        }
        for back in [600.0_f32, 800.0, 950.0] {
            let p = get_directional_point(w.job.pos, ally, back.min(distance(w.job.pos, ally)));
            if front.is_none_or(|f| distance(f, p) <= FRONT_LIMIT) {
                candidates.push(p);
            }
        }
        let radius = muster_radius(w.tactic, w.needed);
        let risk = |p| {
            self.strike_waiting_risk(p, radius)
                + if phantom_range > 0.0 { evasion::coverage(p, w.job.pos, phantom_range + radius) } else { 0.0 }
        };
        let home_risk = risk(w.home);
        let unsafe_home = home_risk > 0.0;
        if unsafe_home {
            // Safety takes precedence over the usual 1000-elmo frontline leash.
            // Include lateral exits, and a retreat if the entire front is shelled.
            for back in [400.0_f32, 800.0, 1400.0, 2200.0] {
                let p = get_directional_point(w.home, ally, back.min(distance(w.home, ally)));
                for side in [-500.0, 0.0, 500.0] {
                    candidates.push(Vec3::xz(p.x + side, p.z));
                }
            }
            candidates.push(ally);
        }
        candidates.push(w.home);
        let map = self.cb.map();
        candidates
            .into_iter()
            .filter(|&p| {
                p.x > 0.0
                    && p.z > 0.0
                    && p.x < (map.width() * 8) as f32
                    && p.z < (map.height() * 8) as f32
                    && (unsafe_home
                        || phantom_range == 0.0
                        || outside_target_fire(p, w.job.pos, phantom_range, muster_radius(w.tactic, w.needed)))
                    && !self.graph.is_enemy_territory(p)
                    && self.war.get_porc_threat(p) <= 0.0
                    && (unsafe_home || self.war.get_effective_threat(p) <= 0.0)
                    && reaches(self.reach_terrain(), def, w.home, p, 80.0)
            })
            .filter_map(|p| {
                let danger = risk(p);
                (danger <= 0.0 || unsafe_home && danger < home_risk).then_some((p, danger))
            })
            .min_by(|(a, ar), (b, br)| {
                ar.total_cmp(br).then_with(|| distance(*a, w.job.pos).total_cmp(&distance(*b, w.job.pos)))
            })
            .map(|(p, _)| p)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn phantom_staging_keeps_even_the_nearest_member_out_of_range() {
        let target = Vec3::xz(2000.0, 2000.0);
        let radius = muster_radius(Tactic::Dart, 20);
        assert!(!outside_target_fire(Vec3::xz(2000.0, 1050.0), target, 1100.0, radius));
        assert!(!outside_target_fire(Vec3::xz(2000.0, 800.0), target, 1100.0, radius));
        assert!(outside_target_fire(Vec3::xz(2000.0, 400.0), target, 1100.0, radius));
    }
    #[test]
    fn activity_musters_before_the_launch_window_and_quiet_units_screen() {
        assert_eq!(waiting_phase(Tactic::Flea, 30 * 30, false), Phase::Forming);
        assert_eq!(waiting_phase(Tactic::Flea, 30 * 60, false), Phase::Screening);
        assert_eq!(waiting_phase(Tactic::Flea, 30 * 60, true), Phase::Forming);
        assert_eq!(waiting_phase(Tactic::Dart, 30 * 60, false), Phase::Screening);
        assert_eq!(waiting_phase(Tactic::Dart, 30 * 30, false), Phase::Forming);
        assert_eq!(waiting_phase(Tactic::Dirtbag, 30 * 60, false), Phase::Screening);
        assert_eq!(waiting_phase(Tactic::Halberd, 30 * 60, false), Phase::Forming);
    }
    #[test]
    fn screen_is_a_centered_tangent_line_and_does_not_count_as_mustered() {
        let home = Vec3::xz(1000.0, 1000.0);
        let target = Vec3::xz(1000.0, 1700.0);
        let points: Vec<_> = (0..20).map(|i| line_point(home, target, home, i, 20, SPREAD)).collect();
        assert!(points.iter().all(|p| p.z == home.z));
        assert!((points.iter().map(|p| p.x).sum::<f32>() / 20.0 - home.x).abs() < 0.01);
        assert!(distance(points[0], home) > muster_radius(Tactic::Flea, 20));
        assert!(points.iter().all(|p| distance(*p, home) <= SCREEN_HALF + 0.01));
    }
}
