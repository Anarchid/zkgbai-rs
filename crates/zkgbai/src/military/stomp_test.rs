//! Development aid: a staged look at where Jugglenauts jump out of the way
//! (`stomp`, see `jumps`), set up with cheats by the AI option `test=stomp`
//! (cheats on only while spawning). Every [`TRIAL`] frames a Jugglenaut
//! stands on our side of the map with solars of ours where its jump home
//! lands best (full reach homeward, see `Ai::escape_landing`), and jumps
//! home at once, as from a chaser. A `stomptest:` line per trial tells where
//! it landed, how far from the nearest solar, and how many its stomp killed;
//! the last one sums up. With `disable=stomp` it lands on them.

use std::collections::BTreeMap;

use springai::{Unit, Vec3};

use crate::Ai;
use crate::util::{NULLPOS, distance};

/// The first trial, and one every this many frames after.
const FIRST: i32 = 30 * 15;
const TRIAL: i32 = 30 * 20;
const TRIALS: usize = 6;
/// Frames from the spawn to the jump, and from the jump to the count.
const SETTLE: i32 = 60;
const COUNT: i32 = 30 * 8;
/// Trials stand this far from the start toward the enemy, this far apart
/// across the way.
const OUT: f32 = 1400.0;
const ACROSS: f32 = 700.0;
/// Solars stand this far across the best landing.
const SOLARS: [f32; 3] = [-80.0, 0.0, 80.0];
const SUMO: &str = "jumpsumo";
const SOLAR: &str = "energysolar";

#[derive(Default)]
pub struct StompTest {
    trial: Option<Trial>,
    started: usize,
    /// Over all trials: solars stood, and lost.
    solars: usize,
    lost: usize,
}

struct Trial {
    n: usize,
    frame: i32,
    sumo: Unit,
    from: Vec3,
    home: Vec3,
    /// Our solars there, and whether each still stands.
    solars: BTreeMap<i32, (Vec3, bool)>,
    landing: Option<Vec3>,
}

impl Ai {
    fn stomp_testing(&self) -> bool {
        self.test.as_deref() == Some("stomp")
    }

    pub fn stomp_test_update(&mut self, frame: i32) {
        if !self.stomp_testing() {
            return;
        }
        let started = self.stomp_test.started;
        if frame >= FIRST && (frame - FIRST) % TRIAL == 0 && started < TRIALS {
            self.stomp_test_stage(started, frame);
        }
        let Some((n, at, sumo, home)) = self.stomp_test.trial.as_ref().map(|t| (t.n, t.frame, t.sumo, t.home)) else {
            return;
        };
        if frame == at + SETTLE {
            self.jump_escape_now(sumo, home);
            let landing = self.jump_landing(sumo.id());
            if let Some(t) = self.stomp_test.trial.as_mut() {
                t.landing = landing;
            }
            if landing.is_none() {
                self.log(&format!("stomptest: trial {n} jugglenaut {} did not jump", sumo.id()));
            }
        }
        if frame == at + SETTLE + COUNT {
            self.stomp_test_count();
        }
    }

    /// A Jugglenaut out on our side, solars where its jump home lands best.
    fn stomp_test_stage(&mut self, n: usize, frame: i32) {
        self.stomp_test.started += 1;
        let (Some(sumo_def), Some(solar_def)) = (self.defs.get(SUMO), self.defs.get(SOLAR)) else { return };
        let Some(jd) = self.jump_def(sumo_def) else { return };
        let map = self.cb.map();
        let (w, h) = (map.width() as f32 * 8.0, map.height() as f32 * 8.0);
        let base = match self.graph.get_start_pos() {
            p if !p.is_zero() => p,
            _ => self.graph.get_ally_center(),
        };
        let enemy = match self.graph.get_enemy_center() {
            e if e != NULLPOS && distance(e, base) > 1000.0 => e,
            _ => Vec3::xz(w - base.x, h - base.z),
        };
        let d = distance(base, enemy).max(1.0);
        let (ux, uz) = ((enemy.x - base.x) / d, (enemy.z - base.z) / d);
        let across = ACROSS * (n as f32 - (TRIALS as f32 - 1.0) / 2.0);
        let spot = Vec3::xz(base.x + ux * OUT - uz * across, base.z + uz * OUT + ux * across);
        let site = map.find_closest_build_site(sumo_def, spot, 600.0, 4, 0);
        let from = if site.x >= 0.0 { site } else { spot };
        self.cb.cheats_set_enabled(true);
        let sumo = self.cb.cheat_new_unit(sumo_def, from);
        // Where its jump home lands best: full reach toward the start.
        let k = (jd.reach() - 1.0) / distance(from, base).max(1.0);
        let best = Vec3::xz(from.x + (base.x - from.x) * k, from.z + (base.z - from.z) * k);
        let mut solars = BTreeMap::new();
        for off in SOLARS {
            let p = Vec3::xz(best.x - uz * off, best.z + ux * off);
            let site = map.find_closest_build_site(solar_def, p, 120.0, 0, 0);
            if let Some(u) = self.cb.cheat_new_unit(solar_def, if site.x >= 0.0 { site } else { p }) {
                solars.insert(u.id(), (u.pos(), true));
            }
        }
        self.cb.cheats_set_enabled(false);
        let Some(sumo) = sumo else {
            self.log(&format!("stomptest: trial {n}: no Jugglenaut at ({:.0}, {:.0})", from.x, from.z));
            return;
        };
        // Its orders are the test's.
        springai::reserved::own(
            self.cb,
            sumo.id(),
            springai::reserved::Owner { module: "stomp_test", helpers: &["war.unitFinished"] },
        );
        self.stomp_test.solars += solars.len();
        self.log(&format!(
            "stomptest: trial {n} jugglenaut {} at ({:.0}, {:.0}), {} solars about ({:.0}, {:.0}), home ({:.0}, {:.0})",
            sumo.id(),
            from.x,
            from.z,
            solars.len(),
            best.x,
            best.z,
            base.x,
            base.z
        ));
        self.stomp_test.trial = Some(Trial { n, frame, sumo, from, home: base, solars, landing: None });
    }

    fn stomp_test_count(&mut self) {
        let Some(t) = self.stomp_test.trial.take() else { return };
        let lost = t.solars.values().filter(|s| !s.1).count();
        self.stomp_test.lost += lost;
        let landed = t.landing.unwrap_or(t.from);
        let nearest = t.solars.values().map(|s| s.0.distance(landed)).fold(f32::INFINITY, f32::min);
        self.log(&format!(
            "stomptest: trial {} jugglenaut {} {} ({:.0}, {:.0}), {:.0} from ({:.0}, {:.0}); nearest solar {:.0}; solars lost \
             {lost} of {}; stomp {}",
            t.n,
            t.sumo.id(),
            if t.landing.is_some() { "landed at" } else { "stood at" },
            landed.x,
            landed.z,
            distance(t.from, landed),
            t.from.x,
            t.from.z,
            nearest,
            t.solars.len(),
            if self.dev_off("stomp") { "off" } else { "on" }
        ));
        if t.n + 1 == TRIALS {
            self.log(&format!(
                "stomptest: done: {} trials, solars lost {} of {}; stomp {}",
                TRIALS,
                self.stomp_test.lost,
                self.stomp_test.solars,
                if self.dev_off("stomp") { "off" } else { "on" }
            ));
        }
    }

    pub fn stomp_test_unit_destroyed(&mut self, unit: Unit) {
        if let Some(s) = self.stomp_test.trial.as_mut().and_then(|t| t.solars.get_mut(&unit.id())) {
            s.1 = false;
        }
    }
}
