//! Nuclear strikes by counting value: when one missile's blast would take
//! out enough of the enemy's slow value (buildings, and units too slow to
//! get away) that no anti-nuke known or suspected covers, a silo pays.
//!
//! The AI cannot see a missile intercepted, but it can see what is left:
//! if the buildings in the blast still stand once the missile is due, an
//! anti-nuke must be guarding the aim point (antis guard what is within
//! their coverage of them), and the area is suspect until one is seen there
//! or a while has passed.

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

/// How a silo is told to fire: an attack order at a position. (Its weapon
/// is `commandfire`, but the manual-fire command is refused; see the test
/// below.)
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

/// A missile in the air.
struct Launch {
    silo: Unit,
    /// Missiles in store before it.
    stock: i32,
    target: Vec3,
    /// Building value in the blast at launch.
    standing: f32,
    due: i32,
}

#[derive(Default)]
pub struct Nukes {
    launch: Option<Launch>,
    /// Development aid: the firing test, when on.
    test: Option<NukeTest>,
    /// Aim points where missiles were intercepted, until when an anti-nuke
    /// is suspected within its coverage of them, and whether a scout has
    /// been there since.
    intercepted: Vec<(Vec3, i32, bool)>,
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

    /// Whether an anti-nuke guards `at`, or may.
    fn nukes_covered(&self, at: Vec3) -> bool {
        let known = self
            .war
            .targets
            .values()
            .any(|e| !e.is_dead && e.def_name == "staticantinuke" && distance(e.position, at) <= ANTI_COVERAGE);
        // The anti is within its coverage of the intercepted aim point.
        let suspected = self
            .nukes
            .intercepted
            .iter()
            .any(|&(p, until, _)| until > self.frame && distance(p, at) <= 2.0 * ANTI_COVERAGE);
        known || suspected
    }

    /// What an anti-nuke at `anti` denies our nukes: the best blast it
    /// guards.
    pub fn nukes_shielded_value(&self, anti: Vec3) -> f32 {
        self.war
            .targets
            .values()
            .filter(|e| e.identified && e.is_static && !e.is_dead && distance(e.position, anti) <= ANTI_COVERAGE)
            .map(|e| self.nukes_blast_value(e.position).0)
            .fold(0.0, f32::max)
    }

    /// The best aim point no anti-nuke guards, and the value there.
    pub fn nukes_target(&self) -> Option<(Vec3, f32)> {
        self.war
            .targets
            .values()
            .filter(|e| e.identified && e.is_static && !e.is_dead && !self.graph.is_ally_territory(e.position))
            .map(|e| (e.position, self.nukes_blast_value(e.position).0))
            .filter(|&(p, _)| !self.nukes_covered(p))
            .max_by(|a, b| a.1.total_cmp(&b.1))
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
        if self.nukes.launch.is_some() {
            return false;
        }
        let Some((target, value)) = self.nukes_target().filter(|&(_, v)| v >= FIRE_VALUE) else { return false };
        let stock = silo.stockpile();
        silo.execute_custom_command(CMD_ATTACK, &[target.x, target.y, target.z], 0, NO_TIMEOUT);
        let due = self.frame + (30.0 * (distance(silo.pos(), target) / MISSILE_SPEED + FLIGHT_EXTRA)) as i32;
        let standing = self.nukes_blast_value(target).1;
        self.nukes.launch = Some(Launch { silo, stock, target, standing, due });
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
        self.nukes.intercepted.retain(|&(_, until, _)| until > frame);
        let Some(l) = self.nukes.launch.as_ref().filter(|l| frame >= l.due) else { return };
        let (target, standing) = (l.target, l.standing);
        let fired = l.silo.health() > 0.0 && l.silo.stockpile() < l.stock;
        self.nukes.launch = None;
        if !fired {
            self.log("nukes: the missile never left");
            return;
        }
        let left = self.nukes_blast_value(target).1;
        if standing > 0.0 && left > (1.0 - HIT_SHARE) * standing {
            self.nukes.intercepted.push((target, frame + SUSPICION_FRAMES, false));
            self.log(&format!(
                "nukes: missile at ({:.0}, {:.0}) did not land ({left:.0} of {standing:.0} still standing): \
                 an anti-nuke must be within {ANTI_COVERAGE:.0} of it",
                target.x, target.z
            ));
        } else {
            self.log(&format!(
                "nukes: missile landed, {:.0} of {standing:.0} metal of buildings gone",
                standing - left
            ));
        }
    }

    /// Where a scout at `from` should look for the anti-nuke that stopped a
    /// missile: the nearest intercepted aim point not looked at yet (one
    /// within reach now counts as looked at).
    pub fn nukes_scout_target(&mut self, from: Vec3) -> Option<Vec3> {
        for s in self.nukes.intercepted.iter_mut().filter(|s| distance(s.0, from) < SCOUTED) {
            s.2 = true;
        }
        let todo = self.nukes.intercepted.iter().filter(|s| !s.2);
        todo.map(|s| s.0).min_by(|a, b| distance(*a, from).total_cmp(&distance(*b, from)))
    }
}

/// Development aid (`ZKGBAI_NUKE_TEST`): cheat a silo and metal, and fire its
/// missiles at the far side of the map, ordering each a different way in
/// turn, to log which way launches. (Against NullAI on Comet Catcher:
/// manual fire nothing, attack launched.)
#[derive(Default)]
pub struct NukeTest {
    silo: Option<Unit>,
    variant: usize,
    /// The order out: since when, and missiles in store before it.
    pending: Option<(i32, i32)>,
}

/// The ways tried: manual fire, attack a position, attack an area.
const TEST_VARIANTS: [&str; 3] = ["manual fire", "attack", "attack area"];

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
                    0 => silo.execute_custom_command(CMD_MANUALFIRE, &[target.x, target.y, target.z], 0, NO_TIMEOUT),
                    1 => silo.execute_custom_command(CMD_ATTACK, &[target.x, target.y, target.z], 0, NO_TIMEOUT),
                    _ => silo.attack_area(target, 0.0, 0),
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
