//! Nuclear strikes by counting value: when one missile's blast would take
//! out enough of the enemy's slow value (buildings, and units too slow to
//! get away) that no anti-nuke known or suspected covers, a silo pays.
//!
//! An anti-nuke stops any missile whose flight crosses its coverage, not
//! only those aimed within it (Zero-K's antinuke handler: the remaining
//! path from the missile to its target, on the ground plane, must meet the
//! coverage circle), so a target is open only if the line from our silo to
//! it keeps clear of every anti.
//!
//! The AI cannot see a missile intercepted (it gets no explosion events),
//! but it can see what is left: if the buildings in the blast are seen
//! standing once the missile is due, an anti-nuke must be within its
//! coverage of the flight line, short of our own ground, and lines passing
//! near that stretch are suspect until the anti is found or a while has
//! passed. Only what is seen counts: the engine reports enemy deaths in our
//! sight or radar only, so unseen buildings stay on the books whether the
//! missile landed or not. Until a look tells (scouts are sent), lines near
//! that stretch wait.

use springai::{NO_TIMEOUT, Unit, Vec3};

use crate::Ai;
use crate::util::distance;

/// Blast radius of a nuke (`staticnuke`'s `areaOfEffect` 1920, a diameter).
const BLAST: f32 = 960.0;

/// How far from itself an anti-nuke guards (`nuke_coverage`).
const ANTI_COVERAGE: f32 = 2500.0;

/// Value in one blast that makes a silo worth building (it and a missile
/// cost about 11k), and that a missile is worth firing at (twice its cost).
pub const STRIKE_VALUE: f32 = 10000.0;
const FIRE_VALUE: f32 = 6480.0;

/// Units slower than this (elmos/s) count, at half their value (they may
/// still move off).
const SLOW: f32 = 75.0;

/// From this income on (with the economy not losing) a silo is affordable.
const NUKE_INCOME: f32 = 50.0;

/// How a silo is told to fire: an attack order at a position (an area
/// attack works as well). Its weapon is `commandfire`, but the manual-fire
/// command is refused; see the test below.
const CMD_ATTACK: i32 = 20;
const CMD_MANUALFIRE: i32 = 105;

/// Missile speed (elmos/s), and time to add for launch (10 s straight up)
/// and descent.
const MISSILE_SPEED: f32 = 800.0;
const FLIGHT_EXTRA: f32 = 30.0;

/// A hit must have removed this share of the buildings' value.
const HIT_SHARE: f32 = 0.3;

/// Frames an interception keeps an area suspect.
const SUSPICION_FRAMES: i32 = 30 * 600;

/// Frames to wait for a look at what a missile did, after it is due.
const VERIFY_FRAMES: i32 = 30 * 180;

/// Frames between reports of why missiles in store stay there.
const HOLD_REPORT: i32 = 30 * 120;

/// A missile fired.
struct Launch {
    silo: Unit,
    /// Missiles in store before it.
    stock: i32,
    from: Vec3,
    target: Vec3,
    /// The buildings in the blast at launch, and their value.
    buildings: Vec<(i32, f32)>,
    due: i32,
}

#[derive(Default)]
pub struct Nukes {
    launch: Option<Launch>,
    /// Missiles fired whose effect is not seen yet.
    unverified: Vec<Launch>,
    /// Development aid: the firing test, when on.
    test: Option<NukeTest>,
    /// Flights that were intercepted.
    intercepted: Vec<Interception>,
    last_hold_report: i32,
}

/// An intercepted flight: an anti-nuke is within its coverage of the
/// stretch of it beyond our ground (sampled), suspected until when, and
/// whether a scout has flown out to the aim point since.
struct Interception {
    from: Vec3,
    target: Vec3,
    stretch: Vec<Vec3>,
    until: i32,
    scouted: bool,
}

/// Flight lines are sampled this often, elmos.
const SAMPLE: f32 = 250.0;

/// Distance from `p` to the segment from `a` to `b`, on the ground plane.
fn segment_distance(p: Vec3, a: Vec3, b: Vec3) -> f32 {
    let (dx, dz) = (b.x - a.x, b.z - a.z);
    let len2 = dx * dx + dz * dz;
    let t = if len2 > 0.0 { (((p.x - a.x) * dx + (p.z - a.z) * dz) / len2).clamp(0.0, 1.0) } else { 0.0 };
    distance(p, Vec3::xz(a.x + t * dx, a.z + t * dz))
}

/// A scout has looked at an aim point when it came this close, elmos.
const SCOUTED: f32 = 400.0;

impl Ai {
    /// What a blast at `at` takes out: (all of it, buildings only).
    fn nukes_blast_value(&self, at: Vec3) -> (f32, f32) {
        let frame = self.frame;
        let (mut all, mut buildings) = (0.0, 0.0);
        for e in self.war.targets.values() {
            if !e.identified || e.is_dead || e.can_fly() || distance(e.position, at) > BLAST {
                continue;
            }
            if e.is_static {
                all += e.value;
                buildings += e.value;
            } else if !e.is_raider && e.def_speed() < SLOW && frame - e.last_seen < 900 {
                all += 0.5 * e.value;
            }
        }
        (all, buildings)
    }

    /// Where our missiles fly from: the silo, or our base before there is one.
    fn nukes_origin(&self) -> Vec3 {
        self.misc_nuke().map_or_else(|| self.graph.get_ally_center(), |s| s.pos())
    }

    /// The known enemy anti-nukes.
    fn nukes_antis(&self) -> impl Iterator<Item = Vec3> + '_ {
        (self.war.targets.values()).filter(|e| !e.is_dead && e.def_name == "staticantinuke").map(|e| e.position)
    }

    /// Samples of the line from `from` to `to` beyond our ground.
    fn nukes_stretch(&self, from: Vec3, to: Vec3) -> Vec<Vec3> {
        let n = (distance(from, to) / SAMPLE).ceil().max(1.0) as usize;
        (0..=n)
            .map(|i| i as f32 / n as f32)
            .map(|t| Vec3::xz(from.x + t * (to.x - from.x), from.z + t * (to.z - from.z)))
            .filter(|&p| !self.graph.is_ally_territory(p))
            .collect()
    }

    /// Whether an anti-nuke guards `at` against our missiles, or may: why,
    /// if so.
    fn nukes_guard(&self, at: Vec3) -> Option<&'static str> {
        let from = self.nukes_origin();
        let known = self.nukes_antis().any(|a| segment_distance(a, from, at) <= ANTI_COVERAGE);
        // An anti within its coverage of a stretch is within twice that of
        // lines passing near it.
        let near = |stretch: &[Vec3]| stretch.iter().any(|&p| segment_distance(p, from, at) <= 2.0 * ANTI_COVERAGE);
        let suspected = self.nukes.intercepted.iter().any(|i| i.until > self.frame && near(&i.stretch));
        let unverified = self.nukes.unverified.iter().any(|l| near(&self.nukes_stretch(l.from, l.target)));
        if known {
            Some("a known anti-nuke")
        } else if suspected {
            Some("an interception")
        } else if unverified {
            Some("a strike not seen yet")
        } else {
            None
        }
    }

    fn nukes_covered(&self, at: Vec3) -> bool {
        self.nukes_guard(at).is_some()
    }

    /// What an anti-nuke at `anti` denies our nukes: the best blast it
    /// guards (its coverage meets the line to it).
    pub fn nukes_shielded_value(&self, anti: Vec3) -> f32 {
        let from = self.nukes_origin();
        (self.war.targets.values())
            .filter(|e| e.identified && e.is_static && !e.is_dead)
            .filter(|e| segment_distance(anti, from, e.position) <= ANTI_COVERAGE)
            .map(|e| self.nukes_blast_value(e.position).0)
            .fold(0.0, f32::max)
    }

    /// The best aim point no anti-nuke guards, and the value there.
    pub fn nukes_target(&self) -> Option<(Vec3, f32)> {
        self.nukes_aim_points().filter(|&(p, _)| !self.nukes_covered(p)).max_by(|a, b| a.1.total_cmp(&b.1))
    }

    /// Enemy buildings to aim at, and the value in the blast at each.
    fn nukes_aim_points(&self) -> impl Iterator<Item = (Vec3, f32)> + '_ {
        (self.war.targets.values())
            .filter(|e| e.identified && e.is_static && !e.is_dead && !self.graph.is_ally_territory(e.position))
            .map(|e| (e.position, self.nukes_blast_value(e.position).0))
    }

    /// Say now and then why missiles in store stay there.
    fn nukes_report_hold(&mut self, silo: Unit) {
        if self.frame - self.nukes.last_hold_report < HOLD_REPORT {
            return;
        }
        self.nukes.last_hold_report = self.frame;
        let best = |guarded: bool| {
            (self.nukes_aim_points())
                .filter(|&(p, _)| self.nukes_covered(p) == guarded)
                .max_by(|a, b| a.1.total_cmp(&b.1))
        };
        let open = best(false).map_or(0.0, |b| b.1);
        let guarded = best(true).map(|(p, v)| format!("; {v:.0} behind {}", self.nukes_guard(p).unwrap_or("?")));
        let flying = if self.nukes.launch.is_some() { "; one in the air" } else { "" };
        self.log(&format!(
            "nukes: holding {} missiles: best open blast {open:.0} (fire at {FIRE_VALUE:.0}){}{flying}",
            silo.stockpile(),
            guarded.unwrap_or_default()
        ));
    }

    /// Whether to build a silo: a strike would pay and we can afford one.
    pub fn nukes_wanted(&self) -> bool {
        !self.dev_off("nukes")
            && self.eco.adjusted_income >= NUKE_INCOME
            && self.eco.static_income >= 0.8 * self.intel.income
            && self.nukes_target().is_some_and(|(_, v)| v >= STRIKE_VALUE)
    }

    /// Fire `silo` at the best uncovered target worth a missile; whether it
    /// fired.
    pub fn nukes_fire(&mut self, silo: Unit) -> bool {
        let target = self.nukes_target().filter(|&(_, v)| v >= FIRE_VALUE);
        let Some((target, value)) = target.filter(|_| self.nukes.launch.is_none()) else {
            self.nukes_report_hold(silo);
            return false;
        };
        let (stock, from) = (silo.stockpile(), silo.pos());
        silo.execute_custom_command(CMD_ATTACK, &[target.x, target.y, target.z], 0, NO_TIMEOUT);
        let due = self.frame + (30.0 * (distance(silo.pos(), target) / MISSILE_SPEED + FLIGHT_EXTRA)) as i32;
        let buildings = (self.war.targets.values())
            .filter(|e| e.identified && e.is_static && !e.is_dead && distance(e.position, target) <= BLAST)
            .map(|e| (e.unit_id, e.value))
            .collect();
        self.nukes.launch = Some(Launch { silo, stock, from, target, buildings, due });
        self.log(&format!("nukes: missile away at ({:.0}, {:.0}), {value:.0} metal in the blast", target.x, target.z));
        true
    }

    /// See how the missile in the air did once it is due.
    pub fn nukes_update(&mut self, frame: i32) {
        if frame == 60 && std::env::var_os("ZKGBAI_NUKE_TEST").is_some() {
            self.nukes.test = Some(NukeTest::default());
        }
        if self.nukes.test.is_some() {
            self.nukes_test_update(frame);
        }
        // An interception is explained once an anti is seen within reach of
        // the line.
        let antis: Vec<Vec3> = self.nukes_antis().collect();
        let explained =
            |i: &Interception| antis.iter().any(|&a| segment_distance(a, i.from, i.target) <= ANTI_COVERAGE);
        self.nukes.intercepted.retain(|i| i.until > frame && !explained(i));
        if let Some(l) = self.nukes.launch.take_if(|l| frame >= l.due) {
            if l.silo.health() > 0.0 && l.silo.stockpile() < l.stock {
                self.nukes.unverified.push(l);
            } else {
                self.log("nukes: the missile never left");
            }
        }
        if frame % 30 == 0 {
            let unverified = std::mem::take(&mut self.nukes.unverified);
            self.nukes.unverified = unverified.into_iter().filter(|l| !self.nukes_verify(l)).collect();
        }
    }

    /// See what missile `l` did, by the buildings in its blast seen gone or
    /// seen standing since it was due; whether that is known now (or never
    /// will be).
    fn nukes_verify(&mut self, l: &Launch) -> bool {
        let (target, frame) = (l.target, self.frame);
        let standing: f32 = l.buildings.iter().map(|b| b.1).sum();
        let (mut gone, mut seen) = (0.0, 0.0);
        for &(id, v) in &l.buildings {
            match self.war.targets.get(&id) {
                Some(e) if e.is_dead => gone += v,
                Some(e) if e.last_seen > l.due => seen += v,
                Some(_) => {}
                None => gone += v,
            }
        }
        let at = format!("({:.0}, {:.0})", target.x, target.z);
        if standing <= 0.0 || gone >= HIT_SHARE * standing {
            self.log(&format!("nukes: missile at {at} landed, {gone:.0} of {standing:.0} metal of buildings gone"));
        } else if seen > (1.0 - HIT_SHARE) * standing {
            let stretch = self.nukes_stretch(l.from, target);
            let until = frame + SUSPICION_FRAMES;
            self.nukes.intercepted.push(Interception { from: l.from, target, stretch, until, scouted: false });
            self.log(&format!(
                "nukes: missile at {at} did not land ({seen:.0} of {standing:.0} seen standing): \
                 an anti-nuke must be within {ANTI_COVERAGE:.0} of its flight"
            ));
        } else if frame - l.due > VERIFY_FRAMES {
            self.log(&format!("nukes: missile at {at}: no look at what it did ({gone:.0} gone, {seen:.0} seen)"));
        } else {
            return false;
        }
        true
    }

    /// Where a scout at `from` should look: at what a missile did, or for
    /// the anti-nuke that stopped one, flying out along its line to the
    /// aim point (one within reach now counts as looked at).
    pub fn nukes_scout_target(&mut self, from: Vec3) -> Option<Vec3> {
        for i in self.nukes.intercepted.iter_mut().filter(|i| distance(i.target, from) < SCOUTED) {
            i.scouted = true;
        }
        let todo = self.nukes.unverified.iter().map(|l| l.target);
        let todo = todo.chain(self.nukes.intercepted.iter().filter(|i| !i.scouted).map(|i| i.target));
        todo.min_by(|a, b| distance(*a, from).total_cmp(&distance(*b, from)))
    }
}

/// Development aid (`ZKGBAI_NUKE_TEST`): cheat a silo and metal, and fire its
/// missiles at the far side of the map, ordering each a different way in
/// turn, to log which way launches. (Against NullAI on Comet Catcher: an
/// area attack and an attack launched, manual fire did nothing.)
#[derive(Default)]
pub struct NukeTest {
    silo: Option<Unit>,
    variant: usize,
    /// The order out: since when, and missiles in store before it.
    pending: Option<(i32, i32)>,
}

/// The ways tried: attack an area, manual fire, attack a position.
const TEST_VARIANTS: [&str; 3] = ["attack area", "manual fire", "attack"];

impl Ai {
    fn nukes_test_update(&mut self, frame: i32) {
        let map = self.cb.map();
        if frame == 60 {
            let ok = self.cb.cheats_set_enabled(true);
            let (Some(def), Some(&u)) = (self.defs.get("staticnuke"), self.cb.team_units().first()) else { return };
            let p = u.pos();
            let site = map.find_closest_build_site(def, Vec3::xz(p.x + 300.0, p.z + 300.0), 1000.0, 4, 0);
            let silo = self.cb.cheat_new_unit(def, site);
            for res in [self.metal, self.energy] {
                self.cb.cheat_resource(res, 50000.0);
            }
            self.log(&format!(
                "nuke-test: cheats {ok}, silo {:?} at ({:.0}, {:.0})",
                silo.map(|s| s.id()),
                site.x,
                site.z
            ));
            if let Some(t) = self.nukes.test.as_mut() {
                t.silo = silo;
            }
            return;
        }
        let Some(t) = self.nukes.test.as_mut() else { return };
        let Some(silo) = t.silo.filter(|s| s.health() > 0.0) else { return };
        if frame % 30 != 0 || t.variant >= TEST_VARIANTS.len() {
            return;
        }
        if silo.stockpile() + silo.stockpile_queued() < 2 {
            silo.queue_stockpile(0);
        }
        let commands: Vec<i32> = silo.current_commands().iter().map(|c| c.id()).collect();
        match t.pending {
            Some((since, stock)) if frame - since >= 600 => {
                let launched = silo.stockpile() < stock;
                let way = TEST_VARIANTS[t.variant];
                t.pending = None;
                t.variant += 1;
                self.log(&format!(
                    "nuke-test: {way}: {} (orders now {commands:?})",
                    if launched { "launched" } else { "nothing" }
                ));
            }
            Some(_) => {}
            None if silo.stockpile() > 0 => {
                let p = silo.pos();
                let (w, h) = (map.width() as f32 * 8.0, map.height() as f32 * 8.0);
                let target = Vec3::new(w - p.x, 0.0, h - p.z);
                let target = Vec3::new(target.x, map.elevation_at(target.x, target.z), target.z);
                let stock = silo.stockpile();
                match t.variant {
                    0 => silo.attack_area(target, 0.0, 0),
                    1 => silo.execute_custom_command(CMD_MANUALFIRE, &[target.x, target.y, target.z], 0, NO_TIMEOUT),
                    _ => silo.execute_custom_command(CMD_ATTACK, &[target.x, target.y, target.z], 0, NO_TIMEOUT),
                };
                let after: Vec<i32> = silo.current_commands().iter().map(|c| c.id()).collect();
                t.pending = Some((frame, stock));
                let way = TEST_VARIANTS[t.variant];
                self.log(&format!(
                    "nuke-test: {way} at ({:.0}, {:.0}) with {stock} in store; orders {after:?}",
                    target.x, target.z
                ));
            }
            None => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn distance_to_a_flight_line() {
        let (a, b) = (Vec3::xz(0.0, 0.0), Vec3::xz(1000.0, 0.0));
        assert_eq!(segment_distance(Vec3::xz(500.0, 300.0), a, b), 300.0);
        // Beyond the ends: to the nearer end.
        assert_eq!(segment_distance(Vec3::xz(-400.0, 300.0), a, b), 500.0);
        assert_eq!(segment_distance(Vec3::xz(1300.0, -400.0), a, b), 500.0);
        assert_eq!(segment_distance(Vec3::xz(3.0, 4.0), a, a), 5.0);
    }
}
