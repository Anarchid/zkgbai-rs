//! Deep scouting with Widows (`spiderantiheavy`: cloaked spiders, 280
//! metal). They go where the army does not, for our strikes: where an
//! anti-nuke that stopped a nuke of ours may stand, whether an anti-nuke we
//! struck is gone, what a nuke did that no word came of (see `nukes` and
//! `missiles`). They walk cloaked by ways clear of what would see them; the
//! ground they bring into our sight counts as looked at. A spider factory
//! builds them while there is looking to do.
//!
//! A Widow's Electro-Stunner (8000 stun damage, range 120) holds an
//! anti-nuke (3300 health) for 30 s, longer than a nuke's flight (20-25 s):
//! with a missile in store or coming and a target that known anti-nukes
//! alone guard, a Widow waits by each of them, and bites when `nukes` says
//! the missile is ready; it flies once they are stunned.

use std::collections::BTreeMap;

use springai::{Unit, Vec3};

use crate::Ai;
use crate::kgbutil::{PathCost, kgb_move_to};
use crate::nav;
use crate::util::{OPTION_SHIFT_KEY, distance, get_directional_point};

pub const WIDOW: &str = "spiderantiheavy";

/// Widows wanted at most: one to look, the rest to bite.
const MAX_WIDOWS: usize = 3;

/// Frames between updates, and before an unfulfilled order is forgotten.
const PERIOD: i32 = 30;
const ORDER_TIMEOUT: i32 = 30 * 120;

/// Frames between new paths to the same place.
const REPATH: i32 = 30 * 10;

/// Widows on separate looks keep this far apart, elmos.
const SPREAD: f32 = 1000.0;

/// A Widow waits this far from the anti-nuke it is to bite, and is ready
/// within this much more.
const WAIT_RANGE: f32 = 250.0;
const READY_SLACK: f32 = 150.0;

/// Frames a bite takes before the Widow goes back to scouting, and that a
/// Widow keeps waiting by an anti-nuke no plan wants bitten (the plan comes
/// and goes with missiles in store and targets seen).
const BITE_FRAMES: i32 = 30 * 15;
const WAIT_GRACE: i32 = 30 * 60;

/// Below this share of its health a Widow goes home.
const HURT: f32 = 0.5;

#[derive(Clone, Copy, PartialEq)]
enum Job {
    /// Nothing to do: at a haven.
    Home,
    /// Bringing a place into sight.
    Look(Vec3),
    /// Waiting by an anti-nuke (by enemy id) to bite it.
    Wait(i32),
    /// Biting an anti-nuke.
    Bite(i32),
}

struct Widow {
    unit: Unit,
    job: Job,
    /// When it was last given its way.
    moved: i32,
    since: i32,
    /// Where it was last seen alive.
    at: Vec3,
}

#[derive(Default)]
pub struct Scouts {
    widows: BTreeMap<i32, Widow>,
    /// When a Widow was queued at a factory.
    ordered: Option<i32>,
}

impl Ai {
    /// Places to look at, while we have strikes to look after.
    fn scouts_looks(&self) -> Vec<Vec3> {
        let mut looks = self.nukes_looks();
        looks.extend(self.missiles_looks());
        looks
    }

    /// How many Widows there is work for.
    fn scouts_needed(&self) -> usize {
        let bites = self.nukes_bite_plan().map_or(0, |c| c.antis.len());
        (usize::from(!self.scouts_looks().is_empty()) + bites).min(MAX_WIDOWS)
    }

    /// Whether a spider factory should build a Widow now.
    pub fn scouts_want_widow(&self) -> bool {
        let s = &self.scouts;
        !self.dev_off("scouts")
            && s.ordered.is_none_or(|f| self.frame - f > ORDER_TIMEOUT)
            && s.widows.len() < self.scouts_needed()
    }

    /// Whether to have a spider factory for Widows: how much they would
    /// open to our nukes.
    pub fn scouts_want_factory(&self) -> Option<f32> {
        if self.dev_off("scouts") || !self.misc_has_nuke() || self.scouts_needed() == 0 {
            return None;
        }
        Some(self.nukes_blocked_value())
    }

    /// A factory has been told to build a Widow.
    pub fn scouts_ordered(&mut self) {
        self.scouts.ordered = Some(self.frame);
    }

    /// A Widow is ready: it holds its fire (it would show itself) and its
    /// ground.
    pub fn scouts_add_widow(&mut self, unit: Unit) {
        self.scouts.ordered = None;
        unit.set_fire_state(0, 0);
        unit.set_move_state(0, 0);
        let w = Widow { unit, job: Job::Home, moved: i32::MIN / 2, since: self.frame, at: unit.pos() };
        self.scouts.widows.insert(unit.id(), w);
    }

    /// Whether a Widow out looking or biting is within `range` of `p`.
    pub fn scouts_near(&self, p: Vec3, range: f32) -> bool {
        (self.scouts.widows.values())
            .any(|w| !matches!(w.job, Job::Home) && w.unit.health() > 0.0 && distance(w.unit.pos(), p) <= range)
    }

    /// Whether a Widow waits close enough to anti-nuke `anti` to bite it.
    pub fn scouts_bite_ready(&self, anti: i32) -> bool {
        let Some(e) = self.war.targets.get(&anti) else { return false };
        self.scouts.widows.values().any(|w| {
            w.job == Job::Wait(anti)
                && w.unit.is_cloaked()
                && distance(w.unit.pos(), e.position) <= WAIT_RANGE + READY_SLACK
        })
    }

    /// Bite `antis`, each with the Widow waiting by it.
    pub fn scouts_bite(&mut self, antis: &[(i32, Vec3)]) {
        let frame = self.frame;
        for &(id, _) in antis {
            let Some(anti) = self.war.targets.get(&id).map(|e| e.unit) else { continue };
            if let Some(w) = self.scouts.widows.values_mut().find(|w| w.job == Job::Wait(id)) {
                w.unit.attack(anti, 0);
                (w.job, w.since) = (Job::Bite(id), frame);
            }
        }
    }

    pub fn scouts_update(&mut self, frame: i32) {
        if (frame + self.offset + 7) % PERIOD != 0 || self.scouts.widows.is_empty() {
            return;
        }
        let team = self.team_id;
        let lost: Vec<(Vec3, Job)> = (self.scouts.widows.values())
            .filter(|w| w.unit.health() <= 0.0 || w.unit.team() != team)
            .map(|w| (w.at, w.job))
            .collect();
        self.scouts.widows.retain(|_, w| w.unit.health() > 0.0 && w.unit.team() == team);
        for w in self.scouts.widows.values_mut() {
            w.at = w.unit.pos();
        }
        for (at, job) in lost {
            let doing = match job {
                Job::Home => "going home",
                Job::Look(_) => "looking",
                Job::Wait(_) => "on its way to bite",
                Job::Bite(_) => "biting",
            };
            self.log(&format!("scouts: Widow lost at ({:.0}, {:.0}), {doing}", at.x, at.z));
        }
        let plan: Vec<(i32, Vec3)> = self.nukes_bite_plan().map(|c| c.antis).unwrap_or_default();
        let looks = self.scouts_looks();
        let ids: Vec<i32> = self.scouts.widows.keys().copied().collect();

        // Biters, once done, and the hurt go home; Widows wait while the
        // plan wants their anti-nuke bitten (or did lately) and it stands.
        for &id in &ids {
            let Some(w) = self.scouts.widows.get_mut(&id) else { continue };
            let hurt = w.unit.health() < HURT * w.unit.max_health();
            let done = match w.job {
                Job::Bite(_) => frame - w.since > BITE_FRAMES,
                Job::Wait(anti) if plan.iter().any(|a| a.0 == anti) => {
                    w.since = frame;
                    false
                }
                Job::Wait(anti) => {
                    frame - w.since > WAIT_GRACE || self.war.targets.get(&anti).is_none_or(|e| e.is_dead)
                }
                _ => false,
            };
            if hurt || done {
                w.unit.set_fire_state(0, 0);
                (w.job, w.since) = (Job::Home, frame);
            }
        }
        // A Widow for each anti-nuke in the plan that has none: the nearest.
        for &(anti, at) in &plan {
            if self.scouts.widows.values().any(|w| matches!(w.job, Job::Wait(a) | Job::Bite(a) if a == anti)) {
                continue;
            }
            let free = (self.scouts.widows.iter())
                .filter(|(_, w)| {
                    matches!(w.job, Job::Home | Job::Look(_)) && w.unit.health() >= HURT * w.unit.max_health()
                })
                .min_by(|a, b| distance(a.1.unit.pos(), at).total_cmp(&distance(b.1.unit.pos(), at)))
                .map(|(&id, _)| id);
            if let Some(w) = free.and_then(|id| self.scouts.widows.get_mut(&id)) {
                (w.job, w.since, w.moved) = (Job::Wait(anti), frame, i32::MIN / 2);
                self.log(&format!("scouts: a Widow goes to wait by the anti-nuke at ({:.0}, {:.0})", at.x, at.z));
            }
        }
        // The rest look: each at the nearest place to look that another
        // is not already after.
        for &id in &ids {
            let Some(w) = self.scouts.widows.get(&id) else { continue };
            let here = w.unit.pos();
            let keep = match w.job {
                Job::Look(p) => looks.iter().any(|&l| distance(l, p) < 1.0),
                Job::Home => w.unit.health() < HURT * w.unit.max_health(),
                _ => true,
            };
            if keep {
                continue;
            }
            let taken: Vec<Vec3> = (self.scouts.widows.iter())
                .filter(|&(&o, _)| o != id)
                .filter_map(|(_, o)| if let Job::Look(p) = o.job { Some(p) } else { None })
                .collect();
            let next = (looks.iter().copied())
                .filter(|&l| taken.iter().all(|&t| distance(t, l) >= SPREAD))
                .min_by(|a, b| distance(*a, here).total_cmp(&distance(*b, here)));
            let Some(w) = self.scouts.widows.get_mut(&id) else { continue };
            let job = next.map_or(Job::Home, Job::Look);
            if job != w.job {
                (w.job, w.since, w.moved) = (job, frame, i32::MIN / 2);
                if let Job::Look(p) = job {
                    self.log(&format!("scouts: a Widow goes to look at ({:.0}, {:.0})", p.x, p.z));
                }
            }
        }
        for id in ids {
            self.scouts_move(id);
        }
    }

    /// Send Widow `id` on its way, now and then.
    fn scouts_move(&mut self, id: i32) {
        let frame = self.frame;
        let Some(w) = self.scouts.widows.get(&id) else { return };
        let (unit, here) = (w.unit, w.unit.pos());
        let to = match w.job {
            Job::Bite(_) => return,
            Job::Look(p) => p,
            Job::Wait(anti) => {
                let Some(e) = self.war.targets.get(&anti) else { return };
                get_directional_point(e.position, here, WAIT_RANGE)
            }
            Job::Home => self.graph_get_closest_haven(here),
        };
        if frame - w.moved < REPATH || distance(here, to) < 100.0 {
            return;
        }
        if let Some(w) = self.scouts.widows.get_mut(&id) {
            w.moved = frame;
        }
        // Widows path by what would see them (see the pathfinder).
        let nav = nav!(self);
        let mut path = nav.pathfinder.find_path(nav.war, unit, to, PathCost::AssaultPath);
        let Some(first) = path.pop_front() else { return };
        kgb_move_to(unit, first, 0, frame);
        while let Some(p) = path.pop_front() {
            kgb_move_to(unit, p, OPTION_SHIFT_KEY, frame);
        }
    }
}
