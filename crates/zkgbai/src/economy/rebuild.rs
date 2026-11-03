//! Rebuilding the only factory (`facrebuild`, on; `disable=facrebuild`).
//!
//! When the last factory falls (a base trade), its replacement used to go
//! by the opening's rule from wherever the constructor that happened to
//! plan it stood, with no look at danger. The job then outlived every
//! frame raiders killed on its site, so the builders went back to the same
//! spot (Ravaged, 11-03: four Hover frames killed in one place), and an
//! energy stall after the solars were lost kept any other from being
//! planned (AlienDesert, 11-03: 520 metal banked, no factory for 3 min).
//!
//! The replacement goes where we are strong now and where most of our
//! build power gets soonest (user, 11-03): candidate spots by our builders,
//! caretakers and turrets are scored by the build power that can reach
//! them (each walker's power halved per [`WALK_HALF`] of walk; a
//! caretaker's only within its range) times how well the spot is held
//! (ours against theirs there now, raiders seen lately included). Sites of
//! frames killed lately count little (see [`BURN_FACTOR`]).

use serde_json::json;
use springai::{UnitDef, Vec3};

use crate::Ai;
use crate::util::distance;

/// Seconds of walk that halve a builder's weight.
const WALK_HALF: f32 = 20.0;
/// Walk speed floor, elmos per second (a stuck or slow builder still counts).
const MIN_SPEED: f32 = 30.0;
/// How held a spot is: (floor + ours) / (2 floor + ours + weight * theirs),
/// on the scale of `Enemy::get_danger` (about a Lotus or a riot). Empty
/// ground scores 0.5; ours alone tends to 1, theirs alone to 0.
const HELD_FLOOR: f32 = 100.0;
const ENEMY_WEIGHT: f32 = 2.0;
/// Raiders seen within this of a spot count against it (as `assign` does).
const RAID_RADIUS: f32 = 800.0;
/// A factory frame killed (or a job dropped as unsafe) marks its site for
/// this long; spots within the radius are scored this much lower.
const BURN_FRAMES: i32 = 30 * 120;
const BURN_RADIUS: f32 = 600.0;
const BURN_FACTOR: f32 = 0.1;
/// Seeds closer than this count once.
const SEED_GRID: f32 = 256.0;
/// Seeds sited in full (each costs a site search with exit checks).
const SITED: usize = 5;
/// Between failed attempts.
const RETRY_FRAMES: i32 = 60;

/// A builder as the scoring sees it.
#[derive(Clone, Copy, Debug)]
pub(super) struct Builder {
    pub pos: Vec3,
    pub power: f32,
    /// Elmos per second; 0 for a caretaker.
    pub speed: f32,
    pub range: f32,
}

/// Build power that gets to `p` soon: a walker's at the half-life of its
/// walk to within its range, a caretaker's only within its range. Only the
/// builders `reaches` allows count.
pub(super) fn power_near(p: Vec3, builders: &[Builder], reaches: impl Fn(usize) -> bool) -> f32 {
    let mut sum = 0.0;
    for (i, b) in builders.iter().enumerate() {
        let gap = (distance(b.pos, p) - b.range).max(0.0);
        if b.speed <= 0.0 {
            if gap <= 0.0 && reaches(i) {
                sum += b.power;
            }
            continue;
        }
        if !reaches(i) {
            continue;
        }
        let walk = gap / b.speed.max(MIN_SPEED);
        sum += b.power * 0.5f32.powf(walk / WALK_HALF);
    }
    sum
}

/// How held a spot is, from 0 (theirs) through 0.5 (nobody's) to 1 (ours).
pub(super) fn held(ours: f32, theirs: f32) -> f32 {
    (HELD_FLOOR + ours.max(0.0)) / (2.0 * HELD_FLOOR + ours.max(0.0) + ENEMY_WEIGHT * theirs.max(0.0))
}

/// The weight of a spot near sites burnt lately.
pub(super) fn burn_weight(p: Vec3, burned: &[(Vec3, i32)], frame: i32) -> f32 {
    if burned.iter().any(|&(s, until)| until > frame && distance(s, p) < BURN_RADIUS) { BURN_FACTOR } else { 1.0 }
}

/// Seeds one per [`SEED_GRID`] cell, first come first kept.
pub(super) fn dedup_seeds(seeds: impl IntoIterator<Item = (Vec3, &'static str)>) -> Vec<(Vec3, &'static str)> {
    let mut kept: Vec<(Vec3, &'static str)> = Vec::new();
    for (p, why) in seeds {
        if p.x < 0.0 || !p.x.is_finite() || !p.z.is_finite() {
            continue;
        }
        if kept.iter().any(|&(k, _)| distance(k, p) < SEED_GRID) {
            continue;
        }
        kept.push((p, why));
    }
    kept
}

/// One candidate's reading.
#[derive(Clone, Copy, Debug)]
struct Reading {
    power: f32,
    held: f32,
    ours: f32,
    theirs: f32,
    burn: f32,
}

impl Reading {
    fn score(&self) -> f32 {
        self.power * self.held * self.burn
    }
}

impl Ai {
    /// Whether the rebuild rules apply now: no factory stands, the plop is
    /// spent, and the switch is on.
    pub(super) fn eco_rebuilding(&self) -> bool {
        self.fac.factories.is_empty() && !self.dev_off("facrebuild")
    }

    /// Whether a rebuild attempt that planned nothing was too recent to try
    /// again (a site search runs the exit checks of several spots).
    pub(super) fn eco_rebuild_waits(&self) -> bool {
        self.eco.rebuild_tried_at.is_some_and(|t| self.frame - t < RETRY_FRAMES)
    }

    /// Our builders as the scoring sees them (finished mobile ones and
    /// caretakers).
    fn eco_rebuild_builders(&self) -> (Vec<Builder>, Vec<Option<UnitDef>>) {
        let mut out = Vec::new();
        let mut defs = Vec::new();
        for w in self.eco.workers.values() {
            let u = w.unit;
            let Some(d) = u.def() else { continue };
            if u.health() <= 0.0 || u.is_being_built() || d.build_speed() <= 0.0 {
                continue;
            }
            out.push(Builder { pos: u.pos(), power: d.build_speed(), speed: d.speed(), range: d.build_distance() });
            defs.push(Some(d));
        }
        for n in self.eco.nanos.values() {
            let u = n.unit;
            let Some(d) = u.def() else { continue };
            if u.health() <= 0.0 || u.is_being_built() {
                continue;
            }
            out.push(Builder { pos: u.pos(), power: d.build_speed(), speed: 0.0, range: d.build_distance() });
            defs.push(None);
        }
        (out, defs)
    }

    fn eco_rebuild_reading(
        &self,
        p: Vec3,
        builders: &[Builder],
        defs: &[Option<UnitDef>],
        raiders: &[(Vec3, f32)],
    ) -> Reading {
        let reaches = |i: usize| match defs[i] {
            Some(d) => self.terrain_reaches(d, builders[i].pos, p, builders[i].range + 32.0),
            None => true,
        };
        let power = power_near(p, builders, reaches);
        let raiders: f32 = raiders.iter().filter(|r| distance(r.0, p) <= RAID_RADIUS).map(|r| r.1).sum();
        let ours = self.war.get_total_friendly_power(p);
        let theirs = self.war.get_enemy_power(p).max(raiders);
        Reading {
            power,
            held: held(ours, theirs),
            ours,
            theirs,
            burn: burn_weight(p, &self.eco.burned_fac_sites, self.frame),
        }
    }

    /// Where the replacement of our only factory goes, and its facing (see
    /// the module doc). `None` when no spot fits. Marks the attempt: until
    /// a factory job is planned, the next waits [`RETRY_FRAMES`].
    pub(super) fn eco_rebuild_factory_site(&mut self, factory: UnitDef) -> Option<(Vec3, i32)> {
        let frame = self.frame;
        self.eco.rebuild_tried_at = Some(frame);
        self.eco.burned_fac_sites.retain(|&(_, until)| until > frame);

        let (builders, defs) = self.eco_rebuild_builders();
        if builders.is_empty() {
            return None;
        }
        let raiders = self.eco_recent_raiders();
        let commanders: Vec<Vec3> =
            (self.eco.commanders.iter()).filter_map(|id| self.eco.workers.get(id)).map(|w| w.get_pos()).collect();
        let seeds = dedup_seeds(
            (commanders.into_iter().map(|p| (p, "commander")))
                .chain(self.eco.nanos.values().map(|n| (n.unit.pos(), "caretaker")))
                .chain(self.eco.workers.values().filter(|w| !w.is_com).map(|w| (w.get_pos(), "constructor")))
                .chain(self.eco.porcs.iter().map(|u| (u.pos(), "turret")))
                .chain(std::iter::once((self.graph.get_ally_center(), "ally center"))),
        );
        // Cheap reading first; only the best few are sited.
        let mut ranked: Vec<(Vec3, &str, f32)> = (seeds.into_iter())
            .filter(|&(p, _)| self.war.get_porc_threat(p) <= 0.0)
            .map(|(p, why)| (p, why, self.eco_rebuild_reading(p, &builders, &defs, &raiders).score()))
            .filter(|s| s.2 > 0.0)
            .collect();
        ranked.sort_by(|a, b| b.2.total_cmp(&a.2));
        ranked.truncate(SITED);

        let mut best: Option<(Vec3, i32, Reading, &str)> = None;
        for (seed, why, _) in ranked {
            let Some((site, facing)) = self.eco_first_factory_site(factory, seed) else { continue };
            if self.war.get_porc_threat(site) > 0.0 {
                continue;
            }
            let r = self.eco_rebuild_reading(site, &builders, &defs, &raiders);
            if r.power <= 0.0 {
                continue;
            }
            if best.as_ref().is_none_or(|b| r.score() > b.2.score()) {
                best = Some((site, facing, r, why));
            }
        }
        let (site, facing, r, why) = best?;
        let total: f32 = builders.iter().map(|b| b.power).sum();
        self.log(&format!(
            "economy: rebuild: {} at ({:.0}, {:.0}) by a {why}: build power {:.1} of {:.1} near, held {:.2} (ours {:.0}, \
             theirs {:.0}){}",
            self.defs.name(factory),
            site.x,
            site.z,
            r.power,
            total,
            r.held,
            r.ours,
            r.theirs,
            if r.burn < 1.0 { ", by a burnt site" } else { "" },
        ));
        self.op_emit(
            "facrebuild",
            json!({
                "event": "site", "t": frame / 30, "def": self.defs.name(factory), "x": site.x, "z": site.z,
                "seed": why, "power": r.power, "total": total, "held": r.held, "ours": r.ours,
                "theirs": r.theirs, "burnt": r.burn < 1.0,
            }),
        );
        Some((site, facing))
    }

    /// A factory frame of ours died, or its job was dropped as unsafe: mark
    /// the site (see [`BURN_FRAMES`]).
    pub(super) fn eco_burn_factory_site(&mut self, p: Vec3, why: &str) {
        let frame = self.frame;
        self.eco.burned_fac_sites.retain(|&(_, until)| until > frame);
        self.eco.burned_fac_sites.push((p, frame + BURN_FRAMES));
        self.op_emit("facrebuild", json!({"event": "burnt", "t": frame / 30, "x": p.x, "z": p.z, "why": why}));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn walker(x: f32, power: f32) -> Builder {
        Builder { pos: Vec3::xz(x, 0.0), power, speed: 60.0, range: 128.0 }
    }

    #[test]
    fn near_power_beats_far_power_of_the_same_size() {
        let b = [walker(0.0, 10.0)];
        let near = power_near(Vec3::xz(100.0, 0.0), &b, |_| true);
        let far = power_near(Vec3::xz(128.0 + 60.0 * WALK_HALF, 0.0), &b, |_| true);
        assert_eq!(near, 10.0);
        assert!((far - 5.0).abs() < 1e-3);
    }

    #[test]
    fn a_commander_pulls_the_site_over_two_constructors_far_off() {
        // A commander of 12 build power at x=0; two 5-power constructors at
        // x=3000 (a 47 s walk from the commander's spot).
        let b = [walker(0.0, 12.0), walker(3000.0, 5.0), walker(3000.0, 5.0)];
        let by_com = power_near(Vec3::xz(0.0, 0.0), &b, |_| true);
        let by_cons = power_near(Vec3::xz(3000.0, 0.0), &b, |_| true);
        assert!(by_com > by_cons, "{by_com} vs {by_cons}");
    }

    #[test]
    fn caretakers_count_only_in_range() {
        let b = [Builder { pos: Vec3::xz(0.0, 0.0), power: 10.0, speed: 0.0, range: 500.0 }];
        assert_eq!(power_near(Vec3::xz(400.0, 0.0), &b, |_| true), 10.0);
        assert_eq!(power_near(Vec3::xz(600.0, 0.0), &b, |_| true), 0.0);
    }

    #[test]
    fn unreachable_builders_do_not_count() {
        let b = [walker(0.0, 10.0), walker(0.0, 5.0)];
        assert_eq!(power_near(Vec3::xz(0.0, 0.0), &b, |i| i == 1), 5.0);
    }

    #[test]
    fn held_reads_ours_against_theirs() {
        assert!((held(0.0, 0.0) - 0.5).abs() < 1e-6);
        assert!(held(500.0, 0.0) > 0.8);
        assert!(held(0.0, 300.0) < 0.2);
        // A turret line beats empty ground; raiders beside it undo that.
        assert!(held(300.0, 0.0) > held(0.0, 0.0));
        assert!(held(300.0, 400.0) < held(0.0, 0.0));
    }

    #[test]
    fn burnt_sites_weigh_little_until_they_cool() {
        let burned = [(Vec3::xz(0.0, 0.0), 100)];
        assert_eq!(burn_weight(Vec3::xz(300.0, 0.0), &burned, 50), BURN_FACTOR);
        assert_eq!(burn_weight(Vec3::xz(300.0, 0.0), &burned, 100), 1.0);
        assert_eq!(burn_weight(Vec3::xz(900.0, 0.0), &burned, 50), 1.0);
    }

    #[test]
    fn seeds_dedup_by_grid_first_kept() {
        let s = dedup_seeds([
            (Vec3::xz(0.0, 0.0), "commander"),
            (Vec3::xz(100.0, 0.0), "constructor"),
            (Vec3::xz(1000.0, 0.0), "turret"),
            (Vec3::xz(-1.0, 0.0), "nowhere"),
        ]);
        assert_eq!(s.len(), 2);
        assert_eq!(s[0].1, "commander");
        assert_eq!(s[1].1, "turret");
    }
}
