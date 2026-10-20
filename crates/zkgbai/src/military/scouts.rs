//! Deep scouting with Widows (`spiderantiheavy`: cloaked spiders, 280
//! metal). They go where the army does not, for our strikes: where an
//! anti-nuke that stopped a nuke of ours may stand, whether an anti-nuke we
//! struck is gone, what a nuke did that no word came of (see `nukes` and
//! `missiles`). They walk cloaked by ways clear of what would see them; the
//! ground they bring into our sight counts as looked at. A spider factory
//! builds them while there is looking to do. Where we have no spider
//! factory, a cloak factory builds Scythes (`cloakheavyraid`: cloaked
//! raider bots, 250 metal) to look and scout deep instead; they hold their
//! fire like the Widows, and cannot stun.
//!
//! A Widow's Electro-Stunner (8000 stun damage, range 120) holds an
//! anti-nuke (3300 health) for 30 s, longer than a nuke's flight (20-25 s):
//! with a missile in store or coming and a target that known anti-nukes
//! alone guard, a Widow waits by each of them, and bites when `nukes` says
//! the missile is ready; it flies once they are stunned.
//!
//! Past that, once the game is one of strategic weapons (two Fusions of
//! ours, or a silo of either kind), a scout scouts deep for good: it goes
//! where the enemy's ground has gone unseen longest, its valuable buildings
//! first — where anti-nukes, superweapons and Fusions stand that our
//! strikes want to know of — and a spider factory is asked for to build a
//! Widow for it, unless a cloak factory can build a Scythe.
//!
//! Scouts are few and dear, so they are steered closely: every second a
//! fresh path, of which only a short leg is given, round every enemy
//! building we know of (seen or remembered) and every enemy seen lately on
//! the ground, by its size, the scout's decloak distance and a margin. A
//! leg that what we see comes to lie on is replaced at once; one that
//! cannot get on without coming that close stops short, and the scout
//! waits there (it sees farther than that).

use std::collections::BTreeMap;

use springai::{Unit, Vec3};

use crate::Ai;
use crate::kgbutil::{PathCost, kgb_move_to};
use crate::military::nukes::segment_distance;
use crate::nav;
use crate::util::{NULLPOS, OPTION_SHIFT_KEY, distance, get_directional_point};

pub const WIDOW: &str = "spiderantiheavy";
pub const SCYTHE: &str = "cloakheavyraid";

/// Scouts wanted at most: one to look, one to scout deep, the rest
/// (Widows) to bite.
const MAX_SCOUTS: usize = 4;

/// Deep scouting is wanted from this many Fusions of ours on (a Singularity
/// counts as one), or a silo.
const DEEP_FUSIONS: usize = 2;

/// A deep look goes to a place unseen at least this long (frames), the
/// longer the better up to the cap; a building counts this many times more
/// per 1000 metal of it. It is given up after this long.
const DEEP_STALE: i32 = 30 * 60;
const STALE_CAP: i32 = 30 * 300;
const VALUE_WEIGHT: f32 = 1.0;
const DEEP_TIMEOUT: i32 = 30 * 120;

/// Frames between updates of the jobs, and before an unfulfilled order is
/// forgotten.
const PERIOD: i32 = 30;
const ORDER_TIMEOUT: i32 = 30 * 120;

/// Frames between reports on the scouts in the log.
const REPORT: i32 = 30 * 60;

/// Frames between looks at the scouts' legs, and between new paths for
/// the same scout.
const STEER: i32 = 15;
const REPATH: i32 = 30;

/// Elmos of a path given at a time, and within which of a leg's end a new
/// one is due.
const LEG: f32 = 400.0;
const LEG_END: f32 = 100.0;

/// Room kept from enemies beyond their size and the scout's decloak
/// distance, elmos; more from mobiles (they move), counted while seen this
/// lately (frames). The decloak distance where a def gives none.
const KEEP_MARGIN: f32 = 40.0;
const MOBILE_MARGIN: f32 = 100.0;
const MOBILE_SEEN: i32 = 30 * 10;
const DECLOAK: f32 = 75.0;

/// Scouts on separate looks keep this far apart, elmos.
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

/// Below this share of its health a scout goes home.
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
    /// Scouting the enemy's land deep, at a place unseen long.
    Deep(Vec3),
}

struct Scout {
    unit: Unit,
    /// A Widow (else a Scythe, which cannot bite).
    widow: bool,
    job: Job,
    /// When it was last given its way, and the leg it was given.
    moved: i32,
    leg: Vec<Vec3>,
    since: i32,
    /// Where it was last seen alive, and whether it was cloaked then.
    at: Vec3,
    cloaked: bool,
}

impl Scout {
    fn kind(&self) -> &'static str {
        if self.widow { "Widow" } else { "Scythe" }
    }
}

#[derive(Default)]
pub struct Scouts {
    scouts: BTreeMap<i32, Scout>,
    /// When a Widow, and a Scythe, was queued at a factory.
    ordered: Option<i32>,
    ordered_scythe: Option<i32>,
}

impl Ai {
    /// Places to look at, while we have strikes to look after.
    fn scouts_looks(&self) -> Vec<Vec3> {
        let mut looks = self.nukes_looks();
        looks.extend(self.missiles_looks());
        looks
    }

    /// How many scouts there is work for that Scythes can do too: looking
    /// and scouting deep.
    fn scouts_eyes_needed(&self) -> usize {
        usize::from(!self.scouts_looks().is_empty()) + usize::from(self.scouts_deep_wanted())
    }

    /// How many scouts there is work for.
    fn scouts_needed(&self) -> usize {
        let bites = self.nukes_bite_plan().map_or(0, |c| c.antis.len());
        (self.scouts_eyes_needed() + bites).min(MAX_SCOUTS)
    }

    /// Whether the game is one of strategic weapons, where a scout should
    /// always be scouting the enemy's land deep.
    pub fn scouts_deep_wanted(&self) -> bool {
        !self.dev_off("scouts")
            && (self.eco.fusions.len() >= DEEP_FUSIONS || self.misc_has_nuke() || self.missiles_silo().is_some())
    }

    /// Whether we have a scout.
    pub fn scouts_have_scout(&self) -> bool {
        !self.scouts.scouts.is_empty()
    }

    /// Whether we have a spider factory to build Widows.
    fn scouts_spider_factory(&self) -> bool {
        self.fac.factories.values().any(|f| self.defs.name_of(f.unit) == "factoryspider")
    }

    /// Where a scout at `from` should scout deep, and how long it has gone
    /// unseen: of the enemy's metal spots and buildings unseen at least
    /// [`DEEP_STALE`] and not near `taken`, the longest unseen (buildings by
    /// their value), the nearer the better.
    fn scouts_deep_target(&self, from: Vec3, taken: &[Vec3]) -> Option<(Vec3, i32)> {
        let frame = self.frame;
        let spots = self.graph.get_enemy_territory().into_iter().map(|m| (m.position, m.last_seen, 1.0));
        let buildings = (self.war.targets.values())
            .filter(|e| e.is_static && !e.is_dead && e.identified && e.value >= 1000.0)
            .map(|e| (e.position, e.last_seen, 1.0 + VALUE_WEIGHT * e.value / 1000.0));
        let score = |&(p, seen, weight): &(Vec3, i32, f32)| {
            (frame - seen).min(STALE_CAP) as f32 * weight / (1.0 + distance(p, from) / 3000.0)
        };
        (spots.chain(buildings))
            .filter(|&(p, seen, _)| frame - seen >= DEEP_STALE && !self.los.is_in_los(p))
            .filter(|&(p, ..)| taken.iter().all(|&t| distance(t, p) >= SPREAD))
            .max_by(|a, b| score(a).total_cmp(&score(b)))
            .map(|(p, seen, _)| (p, frame - seen))
    }

    /// Whether a spider factory should build a Widow now: there is work
    /// for more scouts than we have, Scythes counting for looking and
    /// scouting deep only.
    pub fn scouts_want_widow(&self) -> bool {
        let s = &self.scouts;
        let scythes = s.scouts.values().filter(|w| !w.widow).count();
        let widows = s.scouts.len() - scythes;
        !self.dev_off("scouts")
            && s.ordered.is_none_or(|f| self.frame - f > ORDER_TIMEOUT)
            && widows + scythes.min(self.scouts_eyes_needed()) < self.scouts_needed()
    }

    /// Whether a cloak factory should build a Scythe now: there is looking
    /// or deep scouting to do, more than our scouts do, and no spider
    /// factory to build Widows for it.
    pub fn scouts_want_scythe(&self) -> bool {
        let s = &self.scouts;
        !self.dev_off("scouts")
            && s.ordered_scythe.is_none_or(|f| self.frame - f > ORDER_TIMEOUT)
            && !self.scouts_spider_factory()
            && s.scouts.len() < self.scouts_eyes_needed()
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

    /// A factory has been told to build a Scythe to scout.
    pub fn scouts_ordered_scythe(&mut self) {
        self.scouts.ordered_scythe = Some(self.frame);
    }

    /// Whether a Scythe just finished is one we asked for.
    pub fn scouts_adopt_scythe(&self) -> bool {
        self.scouts.ordered_scythe.is_some_and(|f| self.frame - f <= ORDER_TIMEOUT)
    }

    /// A Widow, or a Scythe asked for, is ready: it holds its fire (it
    /// would show itself) and its ground.
    pub fn scouts_add(&mut self, unit: Unit) {
        let widow = self.defs.name_of(unit) == WIDOW;
        if widow {
            self.scouts.ordered = None;
        } else {
            self.scouts.ordered_scythe = None;
        }
        unit.set_fire_state(0, 0);
        unit.set_move_state(0, 0);
        let s = Scout {
            unit,
            widow,
            job: Job::Home,
            moved: i32::MIN / 2,
            leg: Vec::new(),
            since: self.frame,
            at: unit.pos(),
            cloaked: false,
        };
        self.log(&format!("scouts: a {} joins the scouts", s.kind()));
        self.scouts.scouts.insert(unit.id(), s);
    }

    /// Whether a scout out looking or biting is within `range` of `p`.
    pub fn scouts_near(&self, p: Vec3, range: f32) -> bool {
        (self.scouts.scouts.values())
            .any(|w| !matches!(w.job, Job::Home) && w.unit.health() > 0.0 && distance(w.unit.pos(), p) <= range)
    }

    /// Whether a Widow waits close enough to anti-nuke `anti` to bite it.
    pub fn scouts_bite_ready(&self, anti: i32) -> bool {
        let Some(e) = self.war.targets.get(&anti) else { return false };
        self.scouts.scouts.values().any(|w| {
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
            if let Some(w) = self.scouts.scouts.values_mut().find(|w| w.job == Job::Wait(id)) {
                w.unit.attack(anti, 0);
                (w.job, w.since) = (Job::Bite(id), frame);
            }
        }
    }

    pub fn scouts_update(&mut self, frame: i32) {
        if self.scouts.scouts.is_empty() {
            return;
        }
        if (frame + self.offset + 7) % PERIOD == 0 {
            self.scouts_jobs(frame);
        }
        if (frame + self.offset + 7) % REPORT == 0 {
            let s = self.scouts.scouts.values();
            let count = |f: &dyn Fn(&Scout) -> bool| s.clone().filter(|w| f(w)).count();
            self.log(&format!(
                "scouts: {} Widows, {} Scythes; {} cloaked; looking {}, deep {}, by anti-nukes {}, home {}",
                count(&|w| w.widow),
                count(&|w| !w.widow),
                count(&|w| w.unit.is_cloaked()),
                count(&|w| matches!(w.job, Job::Look(_))),
                count(&|w| matches!(w.job, Job::Deep(_))),
                count(&|w| matches!(w.job, Job::Wait(_) | Job::Bite(_))),
                count(&|w| w.job == Job::Home),
            ));
        }
        if (frame + self.offset + 7) % STEER == 0 {
            self.scouts_steer();
        }
    }

    /// Count the lost, and give each scout its job.
    fn scouts_jobs(&mut self, frame: i32) {
        let team = self.team_id;
        let lost: Vec<(&'static str, Vec3, Job)> = (self.scouts.scouts.values())
            .filter(|w| w.unit.health() <= 0.0 || w.unit.team() != team)
            .map(|w| (w.kind(), w.at, w.job))
            .collect();
        self.scouts.scouts.retain(|_, w| w.unit.health() > 0.0 && w.unit.team() == team);
        // Those out of cloak, unless biting, and what is nearest them.
        let mut shown = Vec::new();
        for w in self.scouts.scouts.values_mut() {
            w.at = w.unit.pos();
            let cloaked = w.unit.is_cloaked();
            if w.cloaked && !cloaked && !matches!(w.job, Job::Bite(_)) {
                shown.push((w.kind(), w.at));
            }
            w.cloaked = cloaked;
        }
        for (kind, at) in shown {
            let near = (self.war.targets.values())
                .filter(|e| !e.is_dead && e.position != NULLPOS)
                .min_by(|a, b| distance(a.position, at).total_cmp(&distance(b.position, at)))
                .map_or(String::new(), |e| format!(", {} {:.0} away", e.def_name, distance(e.position, at)));
            self.log(&format!("scouts: a {kind} decloaked at ({:.0}, {:.0}){near}", at.x, at.z));
        }
        for (kind, at, job) in lost {
            let doing = match job {
                Job::Home => "going home",
                Job::Look(_) => "looking",
                Job::Deep(_) => "scouting deep",
                Job::Wait(_) => "on its way to bite",
                Job::Bite(_) => "biting",
            };
            self.log(&format!("scouts: {kind} lost at ({:.0}, {:.0}), {doing}", at.x, at.z));
        }
        let plan: Vec<(i32, Vec3)> = self.nukes_bite_plan().map(|c| c.antis).unwrap_or_default();
        let looks = self.scouts_looks();
        let ids: Vec<i32> = self.scouts.scouts.keys().copied().collect();

        // Biters, once done, and the hurt go home; Widows wait while the
        // plan wants their anti-nuke bitten (or did lately) and it stands.
        for &id in &ids {
            let Some(w) = self.scouts.scouts.get_mut(&id) else { continue };
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
            if self.scouts.scouts.values().any(|w| matches!(w.job, Job::Wait(a) | Job::Bite(a) if a == anti)) {
                continue;
            }
            let free = (self.scouts.scouts.iter())
                .filter(|(_, w)| {
                    w.widow
                        && matches!(w.job, Job::Home | Job::Look(_) | Job::Deep(_))
                        && w.unit.health() >= HURT * w.unit.max_health()
                })
                .min_by(|a, b| distance(a.1.unit.pos(), at).total_cmp(&distance(b.1.unit.pos(), at)))
                .map(|(&id, _)| id);
            if let Some(w) = free.and_then(|id| self.scouts.scouts.get_mut(&id)) {
                (w.job, w.since, w.moved) = (Job::Wait(anti), frame, i32::MIN / 2);
                self.log(&format!("scouts: a Widow goes to wait by the anti-nuke at ({:.0}, {:.0})", at.x, at.z));
            }
        }
        // The rest look: each at the nearest place to look that another
        // is not already after; with none, they scout deep.
        let deep = self.scouts_deep_wanted();
        for &id in &ids {
            let Some(w) = self.scouts.scouts.get(&id) else { continue };
            let here = w.unit.pos();
            let keep = match w.job {
                Job::Look(p) => looks.iter().any(|&l| distance(l, p) < 1.0),
                Job::Home => w.unit.health() < HURT * w.unit.max_health(),
                // Until it sees the place, or gives up; a look of our
                // strikes comes first.
                Job::Deep(p) => {
                    deep && looks.is_empty()
                        && !self.los.is_in_los(p)
                        && distance(here, p) > WAIT_RANGE
                        && frame - w.since <= DEEP_TIMEOUT
                }
                _ => true,
            };
            if keep {
                continue;
            }
            let taken: Vec<Vec3> = (self.scouts.scouts.iter())
                .filter(|&(&o, _)| o != id)
                .filter_map(|(_, o)| if let Job::Look(p) | Job::Deep(p) = o.job { Some(p) } else { None })
                .collect();
            let next = (looks.iter().copied())
                .filter(|&l| taken.iter().all(|&t| distance(t, l) >= SPREAD))
                .min_by(|a, b| distance(*a, here).total_cmp(&distance(*b, here)));
            let deep_next = if next.is_none() && deep { self.scouts_deep_target(here, &taken) } else { None };
            let Some(w) = self.scouts.scouts.get_mut(&id) else { continue };
            let job = match (next, deep_next) {
                (Some(p), _) => Job::Look(p),
                (None, Some((p, _))) => Job::Deep(p),
                _ => Job::Home,
            };
            if job != w.job {
                (w.job, w.since, w.moved) = (job, frame, i32::MIN / 2);
                let kind = w.kind();
                match (job, deep_next) {
                    (Job::Look(p), _) => {
                        self.log(&format!("scouts: a {kind} goes to look at ({:.0}, {:.0})", p.x, p.z))
                    }
                    (Job::Deep(p), Some((_, unseen))) => self.log(&format!(
                        "scouts: a {kind} goes deep, to ({:.0}, {:.0}), unseen for {} s",
                        p.x,
                        p.z,
                        unseen / 30
                    )),
                    _ => {}
                }
            }
        }
    }

    /// Where scouts must not go, lest they be seen: round each enemy
    /// building we know of and each enemy seen lately on the ground, by its
    /// size and a margin (a scout adds its decloak distance); and where a
    /// nuke of ours is on its way to.
    fn scouts_keepouts(&self) -> Vec<(Vec3, f32)> {
        let frame = self.frame;
        let blast = self.nukes_in_flight().map(|(p, r)| (p, r + KEEP_MARGIN));
        (self.war.targets.values())
            .filter(|e| !e.is_dead && e.position != NULLPOS && !e.can_fly())
            .filter(|e| e.is_static || frame - e.last_seen <= MOBILE_SEEN)
            .map(|e| {
                // Half the footprint's diagonal (footprint squares are 8 elmos).
                let size = e.ud.map_or(16.0, |d| 4.0 * (d.x_size() as f32).hypot(d.z_size() as f32));
                (e.position, size + if e.is_static { KEEP_MARGIN } else { KEEP_MARGIN + MOBILE_MARGIN })
            })
            .chain(blast)
            .collect()
    }

    /// Steer every scout (see the module notes).
    fn scouts_steer(&mut self) {
        let keepouts = self.scouts_keepouts();
        let ids: Vec<i32> = self.scouts.scouts.keys().copied().collect();
        for id in ids {
            self.scouts_move(id, &keepouts);
        }
    }

    /// Send scout `id` on its way: a short leg of a fresh path when one is
    /// due, or when the leg it has crosses what would see it.
    fn scouts_move(&mut self, id: i32, keepouts: &[(Vec3, f32)]) {
        let frame = self.frame;
        let Some(w) = self.scouts.scouts.get(&id) else { return };
        let (unit, here, widow, job) = (w.unit, w.unit.pos(), w.widow, w.job);
        let to = match job {
            Job::Bite(_) => return,
            Job::Look(p) | Job::Deep(p) => p,
            Job::Wait(anti) => {
                let Some(e) = self.war.targets.get(&anti) else { return };
                get_directional_point(e.position, here, WAIT_RANGE)
            }
            Job::Home => self.graph_get_closest_haven(here),
        };
        if distance(here, to) < LEG_END {
            return;
        }
        let decloak = unit.def().map(|d| d.decloak_distance()).filter(|&d| d > 0.0).unwrap_or(DECLOAK);
        let circles: Vec<(Vec3, f32)> = keepouts.iter().map(|&(p, r)| (p, r + decloak)).collect();
        // What it is in already it leaves by the shortest way (see the
        // pathfinder); the rest its way must not cross.
        let crosses =
            |a: Vec3, b: Vec3| circles.iter().any(|&(c, r)| distance(here, c) >= r && segment_distance(c, a, b) < r);
        let blocked = {
            let mut from = here;
            w.leg.iter().any(|&p| {
                let hit = crosses(from, p);
                from = p;
                hit
            })
        };
        let was_moving = !w.leg.is_empty();
        let leg_done = w.leg.last().is_some_and(|&p| distance(here, p) < LEG_END);
        if frame - w.moved < REPATH && !leg_done && !blocked {
            return;
        }
        let cost = if widow { PathCost::WidowPath } else { PathCost::ScythePath };
        let nav = nav!(self);
        let path = nav.pathfinder.find_path_keepout(nav.war, unit, to, cost, &circles);
        // A short leg of it, short of what would see it (the hurt go home
        // regardless).
        let mut leg = Vec::new();
        let (mut from, mut length) = (here, 0.0);
        for p in path {
            if crosses(from, p) && (job != Job::Home || !leg.is_empty()) {
                break;
            }
            length += distance(from, p);
            leg.push(p);
            from = p;
            if length >= LEG {
                break;
            }
        }
        if let Some(w) = self.scouts.scouts.get_mut(&id) {
            w.moved = frame;
            w.leg = leg.clone();
        }
        let Some((&first, rest)) = leg.split_first() else {
            if was_moving {
                unit.stop(0);
                let kind = if widow { "Widow" } else { "Scythe" };
                self.log(&format!("scouts: a {kind} waits at ({:.0}, {:.0}), short of being seen", here.x, here.z));
            }
            return;
        };
        kgb_move_to(unit, first, 0, frame);
        for &p in rest {
            kgb_move_to(unit, p, OPTION_SHIFT_KEY, frame);
        }
    }
}
