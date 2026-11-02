//! Blind napalm passes along the bearing of an unseen cloaked sniper's shot.
//!
//! Zero-K decloaks any unit that takes damage for three seconds
//! (`unit_decloak_damaged.lua`, refreshed on every hit), and Phoenix napalm
//! sets what it hits burning for 8-12 s, with damage every 6 frames
//! (`unit_is_on_fire.lua`): a Phantom caught by the carpet stays visible for
//! some 11-15 s, long enough for the army and the Ravens. Phoenix does not
//! need to aim. The engine starts a bomber's salvo half its length before the
//! aim point (`CBombDropper::TestRange`), so a Phoenix flown along the
//! bearing lays its ~390-elmo line centred on the aim, along the line on
//! which the sniper must stand.
//!
//! Phantoms sit in contested ground, so by default a carpet does not spare
//! friendly units under its lines (`enable=carpetspare` does); each release
//! logs who was under it.
//!
//! Like the artillery hunt, this is evidence, never an engine handle: only
//! aircraft already built fly it, and a real sighting of the sniper turns the
//! sortie into an ordinary attack on the unit.
use super::*;

/// Evidence older than this launches nothing: a sniper that stopped shooting
/// may have walked off.
const LAUNCH_FRESH: i32 = 30 * 20;
/// A blind pass that has not released by then is abandoned.
pub(super) const WINDOW: i32 = 30 * 50;
/// Snipers shoot from near their reach; centre a bearing-only carpet there.
/// The line's ±195 elmos then cover 0.57-1.13 of the Phantom's 700 range.
const CENTER: f32 = 0.85;
/// Lines of one sortie sit this far apart across the bearing, which carries
/// the shooter's radar error.
pub(super) const SPACING: f32 = 110.0;
/// Waypoints behind each aim on the bearing, so the pass is flown along it.
const LEAD_FAR: f32 = 750.0;
const LEAD_NEAR: f32 = 400.0;
/// Napalm reaches 160 elmos from each bomb of a line about 390 long.
const HALF_LINE: f32 = 200.0;
const FRIENDLY_CLEAR: f32 = 220.0;
/// Bearing intersections leave this much error at most; one pass covers it.
const TIGHT: f32 = 200.0;
/// New impacts that move the centre this far re-issue the orders, unless
/// the leading aircraft is already this close (its pass stands).
const MOVED: f32 = 150.0;
const COMMITTED: f32 = 900.0;

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Line {
    pub far: Vec3,
    pub near: Vec3,
    pub aim: Vec3,
}

fn unit_bearing(dir: Vec3) -> Option<Vec3> {
    let len = dir.x.hypot(dir.z);
    (len.is_finite() && len > 0.01).then(|| Vec3::xz(dir.x / len, dir.z / len))
}

fn tight(fix: &Inference) -> bool {
    fix.uncertainty <= TIGHT
}

/// A tight intersection fix as it is; otherwise the likeliest stand-off on
/// the last impact's bearing.
pub(super) fn center(fix: &Inference) -> Vec3 {
    match fix.bearing.and_then(|(o, d)| unit_bearing(d).map(|d| (o, d))) {
        Some((o, d)) if !tight(fix) => Vec3::xz(o.x + d.x * fix.range * CENTER, o.z + d.z * fix.range * CENTER),
        _ => fix.pos,
    }
}

/// `n` parallel lines along the bearing, centred on [`center`].
pub(super) fn lines(fix: &Inference, n: usize) -> Vec<Line> {
    let Some(d) = fix.bearing.and_then(|(_, d)| unit_bearing(d)) else { return Vec::new() };
    let c = center(fix);
    let side = Vec3::xz(-d.z, d.x);
    (0..n)
        .map(|i| {
            let off = (i as f32 - (n as f32 - 1.0) / 2.0) * SPACING;
            let aim = Vec3::xz(c.x + side.x * off, c.z + side.z * off);
            Line { far: aim - d * LEAD_FAR, near: aim - d * LEAD_NEAR, aim }
        })
        .collect()
}

/// Points sampling the napalm strip of a line, for friendly-fire checks.
fn strip(line: Line) -> impl Iterator<Item = Vec3> {
    let d = unit_bearing(line.aim - line.near).unwrap_or(Vec3::ZERO);
    (-2..=2).map(move |k| line.aim + d * (k as f32 * HALF_LINE / 2.0))
}

impl Mission {
    /// Three lines for a bearing, two for a tight fix; one fewer still flies.
    pub(in crate::military::airstrikes) fn carpet_packages(&self) -> Vec<Vec<(Plane, usize)>> {
        let n = if self.inference.as_ref().is_some_and(tight) { 2 } else { 3 };
        vec![vec![(Plane::Phoenix, n)], vec![(Plane::Phoenix, n - 1)]]
    }
}

impl Ai {
    pub(super) fn airstrike_carpet_jobs(&self) -> Vec<Mission> {
        if self.dev_off("airstrikes") || self.dev_off("carpet") {
            return Vec::new();
        }
        self.counterbattery_fixes(LAUNCH_FRESH)
            .filter(|f| f.ground && f.source.is_none() && f.uncertainty.is_finite() && f.uncertainty <= f.range)
            .filter_map(|f| {
                let name = f.def.as_deref()?;
                let d = self.defs.get(name)?;
                let value = d.cost(self.metal);
                // Cloakers only (the Phantom): seen guns get ordinary missions.
                // Zero-K moves `initCloaked` into customParams for its gadget.
                let cloaker = d.is_able_to_cloak()
                    && (d.is_start_cloaked() || d.custom_param("initcloaked").is_some_and(|v| v != "0"));
                if !cloaker || !priority_target(value, d.health()) {
                    return None;
                }
                unit_bearing(f.direction)?;
                if self.war.targets.values().any(|e| {
                    !e.is_dead
                        && e.identified
                        && (e.visible || e.is_radar_visible)
                        && e.def_name == name
                        && distance(e.position, f.pos) <= f.uncertainty + 300.0
                }) {
                    return None;
                }
                let fix = Inference {
                    id: f.id,
                    def: name.to_owned(),
                    pos: f.pos,
                    uncertainty: f.uncertainty,
                    last: f.last,
                    bearing: Some((f.origin, f.direction)),
                    range: f.range,
                };
                Some(Mission {
                    kind: MissionKind::Carpet,
                    submerged: false,
                    target: f.id,
                    pos: center(&fix),
                    hp: d.health(),
                    value,
                    group: None,
                    targets: vec![f.id],
                    seen: f.last,
                    deep: false,
                    shields: Vec::new(),
                    fast: false,
                    inference: Some(fix),
                })
            })
            .collect()
    }

    /// Lines with ground heights, clamped to the map.
    pub(super) fn carpet_lines(&self, job: &Mission, n: usize) -> Vec<Line> {
        let Some(fix) = &job.inference else { return Vec::new() };
        let map = self.cb.map();
        let (w, h) = ((map.width() * 8 - 1).max(1) as f32, (map.height() * 8 - 1).max(1) as f32);
        let ground = |p: Vec3| {
            let (x, z) = (p.x.clamp(1.0, w), p.z.clamp(1.0, h));
            Vec3::new(x, map.elevation_at(x, z), z)
        };
        lines(fix, n)
            .into_iter()
            .map(|l| Line { far: ground(l.far), near: ground(l.near), aim: ground(l.aim) })
            .collect()
    }

    /// Ground units of ours and our allies under a line's napalm: how many,
    /// and their metal.
    pub(super) fn carpet_exposure(&self, line: Line) -> (usize, f32) {
        let mut under = BTreeMap::new();
        for p in strip(line) {
            for u in self.cb.friendly_units_in_cylinder(p, FRIENDLY_CLEAR) {
                if let Some(d) = u.def().filter(|d| !d.is_able_to_fly()) {
                    under.insert(u.id(), d.cost(self.metal));
                }
            }
        }
        (under.len(), under.values().sum())
    }

    /// Phantoms sit in contested ground: by default a carpet burns whoever is
    /// under it. `enable=carpetspare` holds it off friendly ground units.
    pub(super) fn carpet_spares(&self) -> bool {
        !self.dev_off("carpetspare")
    }

    pub(super) fn carpet_clear(&self, line: Line) -> bool {
        !self.carpet_spares() || self.carpet_exposure(line).0 == 0
    }

    /// Fly the threat-aware route to the far lead-in, along the bearing,
    /// then bomb the aim point.
    pub(super) fn carpet_order(u: Unit, route: &[Vec3], line: Line) {
        let n = route.len().saturating_sub(1);
        for (i, p) in route.iter().take(n).enumerate() {
            u.move_to(*p, if i == 0 { 0 } else { OPTION_SHIFT_KEY });
        }
        u.move_to(line.far, if n == 0 { 0 } else { OPTION_SHIFT_KEY });
        u.move_to(line.near, OPTION_SHIFT_KEY);
        u.attack_area(line.aim, 0.0, OPTION_SHIFT_KEY);
    }

    /// Newer impacts refine the aim of a carpet in flight. Whether to re-issue
    /// the orders: the centre moved and the pass is not yet being flown.
    pub(super) fn carpet_refresh(&self, sortie: &mut Sortie, from: Vec3) -> bool {
        let Some(fix) = sortie.job.inference.as_mut() else { return false };
        let Some(f) = self.counterbattery_fixes(WINDOW).find(|f| f.id == fix.id && f.last > fix.last) else {
            return false;
        };
        fix.pos = f.pos;
        fix.uncertainty = f.uncertainty;
        fix.last = f.last;
        fix.bearing = Some((f.origin, f.direction));
        fix.range = f.range;
        let c = center(fix);
        let moved = distance(c, sortie.job.pos) > MOVED;
        sortie.job.pos = c;
        sortie.job.seen = f.last;
        moved && distance(from, c) > COMMITTED
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fix(uncertainty: f32) -> Inference {
        Inference {
            id: -1,
            def: "cloaksnipe".into(),
            pos: Vec3::xz(1560.0, 1000.0),
            uncertainty,
            last: 0,
            // Shot from the east of a victim at (1000, 1000).
            bearing: Some((Vec3::xz(1000.0, 1000.0), Vec3::new(2.0, 0.3, 0.0))),
            range: 700.0,
        }
    }

    #[test]
    fn bearing_carpets_lie_along_the_shot_at_the_snipers_reach() {
        let ls = lines(&fix(420.0), 3);
        assert_eq!(ls.len(), 3);
        let mid = ls[1];
        assert!((mid.aim.x - 1595.0).abs() < 0.1 && (mid.aim.z - 1000.0).abs() < 0.1);
        // Flown outward from the victim's side, along the bearing.
        assert!((mid.far.x - 845.0).abs() < 0.1 && (mid.near.x - 1195.0).abs() < 0.1);
        assert!((mid.far.z - 1000.0).abs() < 0.1);
        // Spread across the bearing, not along it.
        assert!((ls[0].aim.x - mid.aim.x).abs() < 0.1);
        assert!(((ls[2].aim.z - ls[0].aim.z).abs() - 2.0 * SPACING).abs() < 0.1);
        // The line (±195 elmos around the aim) covers Phantoms from 0.57 to 1.13 range.
        assert!(mid.aim.x - 1000.0 - 195.0 < 0.6 * 700.0 && mid.aim.x - 1000.0 + 195.0 > 700.0);
    }

    #[test]
    fn tight_fixes_are_bombed_where_bearings_cross() {
        let f = fix(150.0);
        assert_eq!(center(&f), f.pos);
        let ls = lines(&f, 1);
        assert!((ls[0].aim.x - f.pos.x).abs() < 0.1 && (ls[0].aim.z - f.pos.z).abs() < 0.1);
        assert!(ls[0].near.x < ls[0].aim.x);
    }

    #[test]
    fn no_bearing_no_lines() {
        let mut f = fix(420.0);
        f.bearing = Some((Vec3::xz(1000.0, 1000.0), Vec3::new(0.0, 1.0, 0.0)));
        assert!(lines(&f, 2).is_empty());
        f.bearing = None;
        assert!(lines(&f, 2).is_empty());
        assert_eq!(center(&f), f.pos);
    }

    #[test]
    fn friendly_strip_samples_the_whole_line() {
        let l = lines(&fix(420.0), 1)[0];
        let xs: Vec<_> = strip(l).map(|p| p.x).collect();
        assert_eq!(xs.len(), 5);
        assert!((xs[0] - (l.aim.x - HALF_LINE)).abs() < 0.1 && (xs[4] - (l.aim.x + HALF_LINE)).abs() < 0.1);
    }

    #[test]
    fn carpet_packages_and_tiers() {
        let mut job = crate::military::airstrikes::tests::job(MissionKind::Carpet, 560.0);
        job.inference = Some(fix(420.0));
        let clear = Risk { aa: 0.0, exposure: 0.0, intercepted: false, burst: 0.0 };
        assert_eq!(packages(&job, clear, 800.0), vec![vec![(Plane::Phoenix, 3)], vec![(Plane::Phoenix, 2)]]);
        job.inference = Some(fix(150.0));
        assert_eq!(packages(&job, clear, 800.0), vec![vec![(Plane::Phoenix, 2)], vec![(Plane::Phoenix, 1)]]);
        assert!(packages(&job, Risk { aa: 0.16, ..clear }, 800.0).is_empty());
        assert!(packages(&job, Risk { intercepted: true, ..clear }, 800.0).is_empty());
        assert!(MissionKind::Carpet.primary());
        assert!(MissionKind::Carpet.priority() > MissionKind::Group.priority());
        assert!(!crate::ai::OFF_BY_DEFAULT.contains(&"carpet"));
        // Friendly fire is accepted unless asked otherwise.
        assert!(crate::ai::OFF_BY_DEFAULT.contains(&"carpetspare"));
    }
}
