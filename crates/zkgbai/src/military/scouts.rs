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
//! Persistent deep scouting follows income, projected global metal density,
//! and known enemy AA density. Sparrows provide baseline coverage; Owls are
//! preferred in light AA, while Widows scout through heavier AA and are
//! always wanted at high global density. Strike reconnaissance and anti-nuke
//! bites retain their own requisitions. Owl quotas round down to zero on
//! maps below 30 square kiloelmos, so scouting alone does not request an
//! air factory there; Sparrows retain a minimum of one.
//!
//! Scouts are few and dear, so they are steered closely: six times a
//! second a look at the leg they are on, and every half second a fresh
//! path, of which only a short leg is given, round every enemy building we
//! know of (seen or remembered) and every enemy seen lately on the ground,
//! by its size, the scout's decloak distance and a margin: a small one on a
//! look (it is there to see them; it heads for the nearest place in sight
//! of what it is to look at, clear of them all), a wide one going home.
//! Enemies in sight are kept clear of along where they are going, the
//! next few seconds; those seen lately, by how far they may have walked
//! since. A leg that what we see comes to lie on is replaced at once; one
//! that cannot get on without coming that close stops short, and the scout
//! waits there for the way to clear (it sees farther than that).
//!
//! Cloaked enemies cannot be seen: where a scout lost its cloak with
//! nothing we know of near it, one stands, and the place is kept clear of
//! for a while; area cloakers seen (Iris, Cornea) are kept clear of by
//! their cloak's reach; and the more of the enemy's army seen lately cloaks,
//! the wider every berth.
//!
//! A cloak goes out when our energy store runs dry (it draws on the store
//! directly, twice a second, ahead of all else; a Widow 5 energy a second
//! standing, 15 walking). So the game is asked to hold a few seconds of
//! our scouts' upkeep back from all but high-priority spending (ZK's
//! energy reserve, where the game takes it from AIs; the jaegers' upkeep
//! too, see `jaeger_upkeep`), and should the store
//! run low all the same, scouts on the enemy's ground hold still, unless
//! something comes at them.

use std::cell::OnceCell;
use std::collections::BTreeMap;

mod density;
use density::DensityMap;

use springai::{Unit, Vec3};

use crate::Ai;
use crate::kgbutil::{PathCost, kgb_move_to};
use crate::military::nukes::segment_distance;
use crate::nav;
use crate::util::{NULLPOS, OPTION_SHIFT_KEY, distance, get_directional_point};

pub const WIDOW: &str = "spiderantiheavy";
pub const SCYTHE: &str = "cloakheavyraid";

pub const SPARROW: &str = "planelightscout";
pub const OWL: &str = "planescout";

/// Cloaked scouts wanted at most: one to look, one to scout deep, the rest
/// (Widows) to bite. The area-based air-scout quotas are separate.
const MAX_SCOUTS: usize = 4;

#[derive(Debug, PartialEq)]
pub(crate) struct ScoutPolicy {
    pub sparrows: usize,
    pub owls: usize,
    pub widows: bool,
}

/// Area is the whole map rectangle in square kiloelmos. Density uses the
/// same standing-metal forecast as raider sizing; AA is known living metal.
fn scout_policy(area: f32, income: f32, density: f32, aa_metal: f32) -> ScoutPolicy {
    let area = area.max(f32::EPSILON);
    let count = (area / 30.0).floor() as usize;
    let deep = density >= 500.0 || income >= 50.0;
    let light_aa = aa_metal / area < 25.0;
    ScoutPolicy {
        sparrows: if income >= 30.0 || density >= 200.0 { count.max(1) } else { 0 },
        owls: if deep && light_aa { count } else { 0 },
        widows: income >= 50.0 && (density >= 1000.0 || (deep && !light_aa)),
    }
}

/// The game is asked to hold back this many seconds of our scouts' cloak
/// upkeep on the move (those on order counted), and this much at least.
/// Should the store run below this many seconds of it all the same (high
/// priority spends the reserve too), scouts on the enemy's ground hold
/// still till it is back to this many (short of the reserve, where the
/// store stays while it is held).
const RESERVE_SECONDS: f32 = 4.0;
const RESERVE_MIN: f32 = 50.0;
const STILL_SECONDS: f32 = 2.0;
const MOVE_SECONDS: f32 = 3.0;

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
const STEER: i32 = 5;
const REPATH: i32 = 15;

/// Elmos of a path given at a time, and within which of a leg's end a new
/// one is due.
const LEG: f32 = 300.0;
const LEG_END: f32 = 80.0;

/// Room kept from enemies beyond their size and the scout's decloak
/// distance, elmos: on a look little, going home more; more from mobiles
/// (they move), counted while seen this lately (frames). The decloak
/// distance where a def gives none.
const LOOK_MARGIN: f32 = 16.0;
const KEEP_MARGIN: f32 = 40.0;
const MOBILE_LOOK_MARGIN: f32 = 32.0;
const MOBILE_MARGIN: f32 = 100.0;
const MOBILE_SEEN: i32 = 30 * 10;
const DECLOAK: f32 = 75.0;

/// Mobiles in sight are kept clear of where they will be each second this
/// many seconds ahead; one out of sight, by how far it may have walked
/// since, up to this far.
const AHEAD: i32 = 3;
const WANDER: f32 = 300.0;

/// A look heads for the nearest place within this share of the scout's
/// sight of what it is to look at, clear of every berth. Hills block sight:
/// where it does not see it from there, the next place is this much
/// nearer; within this of it, a deep look is given up for a while
/// (frames), another look goes straight there.
const VANTAGE: f32 = 0.8;
const NEARER: f32 = 0.6;
const CLOSEST: f32 = 100.0;
const TRIED_FRAMES: i32 = 30 * 180;

/// Where a scout lost its cloak with nothing we know of within this of it,
/// on the enemy's ground and with energy to cloak by (below this much
/// stored, cloaks flicker), a cloaked enemy is taken to stand: a circle of
/// this radius is kept clear of for this long. A scout's lost cloak, and
/// its holding still, is logged once in this long (frames).
const UNEXPLAINED: f32 = 200.0;
const CLOAK_ENERGY: f32 = 50.0;
const HIDDEN_RADIUS: f32 = 250.0;
const HIDDEN_FRAMES: i32 = 30 * 120;
const SHOWN_LOG: i32 = 30 * 10;

/// Area cloakers (cloaking what is within their reach).
const AREA_CLOAKERS: [(&str, f32); 2] = [("cloakjammer", 350.0), ("staticjammer", 400.0)];

/// Every berth widens by up to this much with the share of the enemy's
/// army seen lately (frames) that cloaks (commanders aside; of this much
/// metal at least, so that a few units do not make it all).
const CLOAK_MARGIN: f32 = 60.0;
const CLOAK_SEEN: i32 = 30 * 180;
const CLOAK_ARMY: f32 = 2000.0;

/// When each cell of a grid this coarse (elmos) was last in our sight is
/// kept (refreshed this often, frames): deep looks go also to cells on the
/// enemy's side of the map unseen long, the gaps between what we know of,
/// counting this much of a metal spot (a cell where the enemy is likelier,
/// nearer its base, up to twice that).
const SEEN_CELL: f32 = 256.0;
const SEEN_PERIOD: i32 = 60;
const CELL_WEIGHT: f32 = 0.5;

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
    /// How far from what it is to look at its vantage may be (0: as far as
    /// [`VANTAGE`] has it): where it did not see it from one, the next is
    /// nearer.
    reach: f32,
    /// When it last lost its cloak.
    shown: i32,
    /// Holding still for want of energy.
    held: bool,
}

impl Scout {
    fn kind(&self) -> &'static str {
        if self.widow { "Widow" } else { "Scythe" }
    }
}

/// Something a scout keeps clear of: where, its size (half a footprint's
/// diagonal, or its own reach), and what it is.
#[derive(Clone, Copy)]
struct Keepout {
    at: Vec3,
    size: f32,
    kind: Keep,
}

#[derive(Clone, Copy, PartialEq)]
enum Keep {
    Building,
    Unit,
    /// A cloaked enemy guessed at, or an area cloaker's reach.
    Hidden,
    /// Where a nuke of ours is on its way to.
    Blast,
}

#[derive(Default)]
pub struct Scouts {
    density_map: OnceCell<DensityMap>,
    scouts: BTreeMap<i32, Scout>,
    /// Where scouts lost their cloak with nothing we know of near, and when.
    hidden: Vec<(Vec3, i32)>,
    /// Deep looks given up (the place could not be seen), and when.
    tried: Vec<(Vec3, i32)>,
    /// The share of the enemy's army seen lately that cloaks.
    cloak_share: f32,
    /// When each cell (see [`SEEN_CELL`]) was last in our sight, in rows of
    /// `seen_width`.
    seen: Vec<i32>,
    seen_width: i32,
    /// When a Widow, and a Scythe, was queued at a factory.
    ordered: Option<i32>,
    ordered_scythe: Option<i32>,
    /// Factory orders until creation, so several factories cannot overbuy.
    air_orders: BTreeMap<i32, (&'static str, i32)>,
    /// The energy reserve last asked of the game, and whether the game
    /// took it (once asked).
    reserve: f32,
    reserve_taken: Option<bool>,
    /// Whether scouts on the enemy's ground hold still for want of energy,
    /// when one last began to (logged), and for how long they have in all.
    still: bool,
    still_log: i32,
    still_frames: i32,
}

impl Ai {
    /// Places to look at, while we have strikes to look after.
    fn scouts_looks(&self) -> Vec<Vec3> {
        let mut looks = self.nukes_looks();
        looks.extend(self.missiles_looks());
        looks
    }

    /// Cloaked scouts wanted for strike looks and persistent deep scouting.
    /// Scythes can cover strike looks; the policy explicitly requests a
    /// Widow for persistent deep scouting.
    fn scouts_eyes_needed(&self) -> usize {
        usize::from(!self.scouts_looks().is_empty()) + usize::from(self.scouts_deep_wanted())
    }

    /// How many scouts there is work for.
    fn scouts_needed(&self) -> usize {
        let bites = self.nukes_bite_plan().map_or(0, |c| c.antis.len());
        (self.scouts_eyes_needed() + bites).min(MAX_SCOUTS)
    }

    pub(crate) fn scouts_policy(&self) -> ScoutPolicy {
        let map = self.cb.map();
        let area = map.width().max(0) as f32 * map.height().max(0) as f32 * 64.0 / 1_000_000.0;
        let aa_metal = self
            .war
            .targets
            .values()
            .filter(|e| !e.is_dead && e.identified && (e.is_aa || e.is_flex_aa))
            .map(|e| e.value)
            .sum();
        scout_policy(area, self.eco.effective_income, self.projected_metal_density(), aa_metal)
    }

    /// Estimated global standing metal per million square elmos of usable land.
    pub(crate) fn projected_metal_density(&self) -> f32 {
        self.scouts
            .density_map
            .get_or_init(|| {
                let players = 1 + self.allies.len() + self.get_enemy_team_ids().len();
                DensityMap::new(self.cb, &self.graph.metal_spots, players)
            })
            .at_frame(self.frame)
    }

    /// Widows for persistent deep scouting, independent of strike look jobs.
    pub fn scouts_deep_wanted(&self) -> bool {
        !self.dev_off("scouts") && self.scouts_policy().widows
    }

    pub(crate) fn scouts_air_idle(&mut self, factory: i32) {
        self.scouts.air_orders.remove(&factory);
    }

    pub(crate) fn scouts_air_ordered(&mut self, factory: i32, name: &'static str) {
        self.scouts.air_orders.insert(factory, (name, self.frame));
    }

    pub(crate) fn scouts_air_created(&mut self, unit: Unit, builder: Option<Unit>) {
        if let Some(builder) = builder {
            if self.scouts.air_orders.get(&builder.id()).is_some_and(|&(name, _)| name == self.defs.name_of(unit)) {
                self.scouts.air_orders.remove(&builder.id());
            }
        }
    }

    pub(crate) fn scouts_air_needed(&self, name: &str, wanted: usize) -> bool {
        if self.dev_off("scouts") || wanted == 0 {
            return false;
        }
        let have = if name == SPARROW {
            self.eco.sparrows.len()
        } else {
            self.cb.team_units().into_iter().filter(|&u| self.defs.name_of(u) == name).count()
        };
        let ordered = self
            .scouts
            .air_orders
            .iter()
            .filter(|&(factory, &(n, f))| {
                self.fac.factories.contains_key(factory) && n == name && self.frame - f <= ORDER_TIMEOUT
            })
            .count();
        have + ordered < wanted
    }

    /// Our scouts' cloak upkeep on the move, energy a second.
    fn scouts_upkeep(&self) -> f32 {
        (self.scouts.scouts.values())
            .filter(|w| w.unit.health() > 0.0)
            .filter_map(|w| w.unit.def())
            .map(|d| d.cloak_cost_moving())
            .sum()
    }

    /// Energy in our store, reserve and all.
    fn scouts_stored(&self) -> f32 {
        self.cb.economy().current(self.energy)
    }

    /// Ask the game to hold energy back for our scouts' cloaks (see
    /// [`RESERVE_SECONDS`]), and note whether it does: a game that takes no
    /// reserve from AIs ignores the ask.
    fn scouts_reserve(&mut self) {
        // (Those on order count, so that the store is full when they are
        // out.)
        let ordered = |at: Option<i32>, name: &str| {
            (at.filter(|&f| self.frame - f <= ORDER_TIMEOUT))
                .and_then(|_| self.defs.get(name))
                .map_or(0.0, |d| d.cloak_cost_moving())
        };
        // (And the jaegers': hunters cloak too.)
        let upkeep = self.scouts_upkeep()
            + ordered(self.scouts.ordered, WIDOW)
            + ordered(self.scouts.ordered_scythe, SCYTHE)
            + self.jaeger_upkeep()
            + self.taskforce_spotter_upkeep();
        let want = if upkeep > 0.0 { (RESERVE_SECONDS * upkeep).max(RESERVE_MIN).round() } else { 0.0 };
        if self.scouts.reserve > 0.0 && self.scouts.reserve_taken.is_none() {
            let held = self.cb.game().team_rules_param_float(self.team_id, "energyReserve", -1.0);
            let taken = held == self.scouts.reserve;
            self.scouts.reserve_taken = Some(taken);
            self.log(if taken {
                "scouts: the game holds energy back for our cloaks"
            } else {
                "scouts: the game takes no energy reserve from AIs; scouts hold still when energy runs low"
            });
        }
        if want != self.scouts.reserve {
            self.cb.call_lua_rules(&format!("ereserve:{want}"));
            self.scouts.reserve = want;
        }
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
        self.scouts_deep_target_where(from, taken, |_| true)
    }

    pub(crate) fn scouts_air_deep_target(
        &self,
        from: Vec3,
        taken: &[Vec3],
        reachable: impl Fn(Vec3) -> bool,
    ) -> Option<Vec3> {
        self.scouts_deep_target_where(from, taken, reachable).map(|(p, _)| p)
    }

    fn scouts_deep_target_where(
        &self,
        from: Vec3,
        taken: &[Vec3],
        reachable: impl Fn(Vec3) -> bool,
    ) -> Option<(Vec3, i32)> {
        let frame = self.frame;
        let spots = self.graph.get_enemy_territory().into_iter().map(|m| (m.position, m.last_seen, 1.0));
        let cells = self.scouts_unseen_cells();
        let buildings = (self.war.targets.values())
            .filter(|e| e.is_static && !e.is_dead && e.identified && e.value >= 1000.0)
            .map(|e| (e.position, e.last_seen, 1.0 + VALUE_WEIGHT * e.value / 1000.0));
        let score = |&(p, seen, weight): &(Vec3, i32, f32)| {
            (frame - seen).min(STALE_CAP) as f32 * weight / (1.0 + distance(p, from) / 3000.0)
        };
        (spots.chain(buildings).chain(cells))
            .filter(|&(p, seen, _)| frame - seen >= DEEP_STALE && !self.los.is_in_los(p))
            .filter(|&(p, ..)| taken.iter().all(|&t| distance(t, p) >= SPREAD))
            .filter(|&(p, ..)| self.scouts.tried.iter().all(|t| distance(t.0, p) >= SEEN_CELL))
            .filter(|&(p, ..)| reachable(p))
            .max_by(|a, b| score(a).total_cmp(&score(b)))
            .map(|(p, seen, _)| (p, frame - seen))
    }

    /// Cells on the enemy's side of the map (nearer its metal than ours,
    /// not under water), when each was last seen, and what looking there is
    /// worth (see [`CELL_WEIGHT`]).
    fn scouts_unseen_cells(&self) -> Vec<(Vec3, i32, f32)> {
        let (ours, theirs) = (self.graph.get_ally_center(), self.graph.get_enemy_center());
        if self.scouts.seen_width == 0 || ours == NULLPOS || theirs == NULLPOS {
            return Vec::new();
        }
        let map = self.cb.map();
        let apart = distance(ours, theirs).max(1.0);
        let w = self.scouts.seen_width;
        (self.scouts.seen.iter().enumerate())
            .filter_map(|(i, &seen)| {
                let (x, y) = (i as i32 % w, i as i32 / w);
                let c = Vec3::xz((x as f32 + 0.5) * SEEN_CELL, (y as f32 + 0.5) * SEEN_CELL);
                let near = distance(c, theirs);
                if near >= distance(c, ours) || map.elevation_at(c.x, c.z) < -20.0 {
                    return None;
                }
                Some((c, seen, CELL_WEIGHT * (2.0 - (near / apart).min(1.0))))
            })
            .collect()
    }

    /// Note which cells (see [`SEEN_CELL`]) are in our sight now.
    fn scouts_update_seen(&mut self, frame: i32) {
        if (frame + self.offset + 11) % SEEN_PERIOD != 0 {
            return;
        }
        let map = self.cb.map();
        let w = ((map.width() * 8) as f32 / SEEN_CELL).ceil() as i32;
        let h = ((map.height() * 8) as f32 / SEEN_CELL).ceil() as i32;
        if self.scouts.seen_width != w {
            // Never seen: as stale as can be.
            self.scouts.seen = vec![-STALE_CAP; (w * h).max(0) as usize];
            self.scouts.seen_width = w;
        }
        for (i, seen) in self.scouts.seen.iter_mut().enumerate() {
            let (x, y) = (i as i32 % w, i as i32 / w);
            if self.los.is_in_los(Vec3::xz((x as f32 + 0.5) * SEEN_CELL, (y as f32 + 0.5) * SEEN_CELL)) {
                *seen = frame;
            }
        }
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
            && widows + scythes.min(usize::from(!self.scouts_looks().is_empty())) < self.scouts_needed()
    }

    /// A cloak factory can cover strike looks with a Scythe when no spider
    /// factory can build a Widow. Persistent deep requisitions ask for Widows.
    pub fn scouts_want_scythe(&self) -> bool {
        let s = &self.scouts;
        !self.dev_off("scouts")
            && s.ordered_scythe.is_none_or(|f| self.frame - f > ORDER_TIMEOUT)
            && !self.scouts_spider_factory()
            && s.scouts.len() < usize::from(!self.scouts_looks().is_empty())
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
    /// Let go of scout `id` (a force calls it to spot, see
    /// `taskforce::spotter`): whether it was one.
    pub fn scouts_release(&mut self, id: i32) -> bool {
        self.scouts.scouts.remove(&id).is_some()
    }

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
            reach: 0.0,
            shown: i32::MIN / 2,
            held: false,
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
        self.scouts_update_seen(frame);
        if (frame + self.offset + 7) % PERIOD == 0 {
            self.scouts_reserve();
        }
        if self.scouts.scouts.is_empty() {
            return;
        }
        if (frame + self.offset + 7) % PERIOD == 0 {
            self.scouts_jobs(frame);
        }
        if (frame + self.offset + 7) % REPORT == 0 {
            let s = self.scouts.scouts.values();
            let count = |f: &dyn Fn(&Scout) -> bool| s.clone().filter(|w| f(w)).count();
            // How much of the enemy's side we saw in the last few minutes.
            let cells = self.scouts_unseen_cells();
            let fresh = cells.iter().filter(|c| frame - c.1 < CLOAK_SEEN).count();
            let coverage = 100.0 * fresh as f32 / cells.len().max(1) as f32;
            let held = match self.scouts.reserve_taken {
                Some(true) => "held",
                Some(false) => "not taken",
                None => "asked",
            };
            self.log(&format!(
                "scouts: {} Widows, {} Scythes; {} cloaked; looking {}, deep {}, by anti-nukes {}, home {}; \
                 enemy side seen in 3 min {coverage:.0}%; {:.0}% of the enemy army cloaks; {} cloaked enemies guessed; \
                 energy reserve {:.0} ({held}); scouts held still for energy {} s in all",
                count(&|w| w.widow),
                count(&|w| !w.widow),
                count(&|w| w.unit.is_cloaked()),
                count(&|w| matches!(w.job, Job::Look(_))),
                count(&|w| matches!(w.job, Job::Deep(_))),
                count(&|w| matches!(w.job, Job::Wait(_) | Job::Bite(_))),
                count(&|w| w.job == Job::Home),
                100.0 * self.scouts.cloak_share,
                self.scouts.hidden.len(),
                self.scouts.reserve,
                self.scouts.still_frames / 30,
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
        for w in self.scouts.scouts.values_mut() {
            w.at = w.unit.pos();
        }
        // How much of the enemy's army seen lately cloaks.
        let (mut cloaky, mut all) = (0.0, 0.0);
        for e in (self.war.targets.values())
            .filter(|e| !e.is_dead && !e.is_static && !e.is_com && !e.can_fly() && frame - e.last_seen < CLOAK_SEEN)
        {
            let Some(ud) = e.ud else { continue };
            all += e.value;
            if ud.is_able_to_cloak() || AREA_CLOAKERS.iter().any(|a| a.0 == e.def_name) {
                cloaky += e.value;
            }
        }
        self.scouts.cloak_share = cloaky / all.max(CLOAK_ARMY);
        self.scouts.hidden.retain(|h| frame - h.1 < HIDDEN_FRAMES);
        self.scouts.tried.retain(|t| frame - t.1 < TRIED_FRAMES);
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
                    // (Given up for a while if it cannot get to see it.)
                    if frame - w.since > DEEP_TIMEOUT && !self.los.is_in_los(p) {
                        self.scouts.tried.push((p, frame));
                    }
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
                (w.job, w.since, w.moved, w.reach) = (job, frame, i32::MIN / 2, 0.0);
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

    /// What scouts keep clear of, lest they be seen: each enemy building we
    /// know of; each enemy in sight on the ground where it will be the next
    /// few seconds, and each seen lately where it may have walked since;
    /// cloaked enemies guessed at and area cloakers' reach; and where a
    /// nuke of ours is on its way to. (Each scout adds its decloak distance
    /// and a margin; see [`Ai::scouts_move`].)
    fn scouts_keepouts(&self) -> Vec<Keepout> {
        let frame = self.frame;
        let mut keep = Vec::new();
        for e in (self.war.targets.values()).filter(|e| !e.is_dead && e.position != NULLPOS && !e.can_fly()) {
            let since = frame - e.last_seen;
            if !e.is_static && since > MOBILE_SEEN {
                continue;
            }
            // Half the footprint's diagonal (footprint squares are 8 elmos).
            let size = e.ud.map_or(16.0, |d| 4.0 * (d.x_size() as f32).hypot(d.z_size() as f32));
            if let Some(&(_, reach)) = AREA_CLOAKERS.iter().find(|a| a.0 == e.def_name) {
                keep.push(Keepout { at: e.position, size: reach, kind: Keep::Hidden });
            }
            if e.is_static {
                keep.push(Keepout { at: e.position, size, kind: Keep::Building });
            } else if since == 0 {
                let v = e.unit.vel();
                for t in 0..=AHEAD {
                    let ahead = (30 * t) as f32;
                    let at = Vec3::new(e.position.x + v.x * ahead, e.position.y, e.position.z + v.z * ahead);
                    keep.push(Keepout { at, size, kind: Keep::Unit });
                }
            } else {
                let wander = (e.speed() * since as f32).min(WANDER);
                keep.push(Keepout { at: e.position, size: size + wander, kind: Keep::Unit });
            }
        }
        keep.extend(self.scouts.hidden.iter().map(|&(at, _)| Keepout { at, size: HIDDEN_RADIUS, kind: Keep::Hidden }));
        if let Some((at, r)) = self.nukes_in_flight() {
            keep.push(Keepout { at, size: r + KEEP_MARGIN, kind: Keep::Blast });
        }
        keep
    }

    /// Note scouts that lost their cloak (not biting), and what is nearest
    /// them: with nothing we know of near, a cloaked enemy is taken to
    /// stand there.
    fn scouts_watch_cloak(&mut self) {
        let frame = self.frame;
        let mut shown = Vec::new();
        for w in self.scouts.scouts.values_mut() {
            let cloaked = w.unit.is_cloaked();
            if w.cloaked && !cloaked && !matches!(w.job, Job::Bite(_)) && w.unit.health() > 0.0 {
                shown.push((w.kind(), w.unit.pos(), frame - w.shown >= SHOWN_LOG));
                w.shown = frame;
            }
            w.cloaked = cloaked;
        }
        let starved = self.scouts_stored() < CLOAK_ENERGY;
        for (kind, at, log) in shown {
            let nearest = (self.war.targets.values())
                .filter(|e| !e.is_dead && e.position != NULLPOS)
                .min_by(|a, b| distance(a.position, at).total_cmp(&distance(b.position, at)));
            let near =
                nearest.map_or(String::new(), |e| format!(", {} {:.0} away", e.def_name, distance(e.position, at)));
            let unexplained = nearest.is_none_or(|e| distance(e.position, at) > UNEXPLAINED);
            let hidden = unexplained && !starved && !self.graph.is_ally_territory(at);
            if hidden {
                self.scouts.hidden.push((at, frame));
            }
            let why = match (hidden, starved) {
                (true, _) => "; a cloaked enemy, likely",
                (false, true) => "; out of energy",
                _ => "",
            };
            if log {
                self.log(&format!("scouts: a {kind} decloaked at ({:.0}, {:.0}){near}{why}", at.x, at.z));
            }
        }
    }

    /// Steer every scout (see the module notes).
    fn scouts_steer(&mut self) {
        self.scouts_watch_cloak();
        // Short of energy for the cloaks, those on the enemy's ground hold
        // still (see [`STILL_SECONDS`]).
        let upkeep = self.scouts_upkeep();
        let enough = if self.scouts.still { MOVE_SECONDS } else { STILL_SECONDS };
        self.scouts.still = self.scouts_stored() < enough * upkeep;
        let keepouts = self.scouts_keepouts();
        let ids: Vec<i32> = self.scouts.scouts.keys().copied().collect();
        for id in ids {
            self.scouts_move(id, &keepouts);
        }
    }

    /// Send scout `id` on its way: a short leg of a fresh path when one is
    /// due, or when the leg it has crosses what would see it.
    fn scouts_move(&mut self, id: i32, keepouts: &[Keepout]) {
        let frame = self.frame;
        let Some(w) = self.scouts.scouts.get(&id) else { return };
        let (unit, here, widow, job) = (w.unit, w.unit.pos(), w.widow, w.job);
        let def = unit.def();
        let decloak = def.map(|d| d.decloak_distance()).filter(|&d| d > 0.0).unwrap_or(DECLOAK);
        // Out to see them, little room; staking out an anti-nuke or going
        // home, more.
        let looking = matches!(job, Job::Look(_) | Job::Deep(_));
        let cloak = CLOAK_MARGIN * self.scouts.cloak_share;
        let circles: Vec<(Vec3, f32)> = (keepouts.iter())
            .map(|k| {
                let margin = match (k.kind, looking) {
                    (Keep::Building, true) => decloak + LOOK_MARGIN + cloak,
                    (Keep::Building, false) => decloak + KEEP_MARGIN + cloak,
                    (Keep::Unit, true) => decloak + MOBILE_LOOK_MARGIN + cloak,
                    (Keep::Unit, false) => decloak + KEEP_MARGIN + MOBILE_MARGIN + cloak,
                    (Keep::Hidden, _) => decloak,
                    (Keep::Blast, _) => 0.0,
                };
                (k.at, k.size + margin)
            })
            .collect();
        let clear = |p: Vec3| circles.iter().all(|&(c, r)| distance(p, c) >= r);
        // A look goes to the nearest place in sight of what it is to look
        // at, clear of every berth.
        let reach = if w.reach > 0.0 { w.reach } else { VANTAGE * def.map_or(0.0, |d| d.los_radius()) };
        let vantage = |p: Vec3| {
            if clear(p) {
                return p;
            }
            let sight = reach;
            let rings = (1..).map(|k| 100.0 * k as f32).take_while(|&r| r <= sight);
            (rings.flat_map(|r| (0..16).map(move |k| (r, k as f32 * std::f32::consts::TAU / 16.0))))
                .map(|(r, a)| Vec3::xz(p.x + r * a.cos(), p.z + r * a.sin()))
                .filter(|&q| clear(q))
                .min_by(|a, b| distance(*a, here).total_cmp(&distance(*b, here)))
                .unwrap_or(p)
        };
        let to = match job {
            Job::Bite(_) => return,
            Job::Look(p) | Job::Deep(p) => vantage(p),
            Job::Wait(anti) => {
                let Some(e) = self.war.targets.get(&anti) else { return };
                get_directional_point(e.position, here, WAIT_RANGE)
            }
            Job::Home => self.graph_get_closest_haven(here),
        };
        if distance(here, to) < LEG_END {
            // There, and what it is to look at still unseen (a hill in the
            // way): nearer, or another place.
            if let Job::Look(p) | Job::Deep(p) = job
                && !self.los.is_in_los(p)
                && let Some(w) = self.scouts.scouts.get_mut(&id)
            {
                w.reach = reach * NEARER;
                if w.reach < CLOSEST {
                    if matches!(job, Job::Deep(_)) {
                        self.scouts.tried.push((p, frame));
                        w.since = i32::MIN / 2;
                    } else {
                        w.reach = 1.0;
                    }
                }
            }
            return;
        }
        // Short of energy, on the enemy's ground and with nothing coming at
        // it, it holds still (a cloak costs less standing); the time held
        // does not count against a deep look.
        if self.scouts.still && clear(here) && !self.graph.is_ally_territory(here) {
            if !w.leg.is_empty() {
                unit.stop(0);
            }
            let began = !w.held;
            if let Some(w) = self.scouts.scouts.get_mut(&id) {
                (w.leg, w.moved, w.held) = (Vec::new(), i32::MIN / 2, true);
                if matches!(job, Job::Deep(_)) {
                    w.since += STEER;
                }
            }
            self.scouts.still_frames += STEER;
            if began && frame - self.scouts.still_log >= SHOWN_LOG {
                self.scouts.still_log = frame;
                let kind = if widow { "Widow" } else { "Scythe" };
                self.log(&format!(
                    "scouts: a {kind} holds still at ({:.0}, {:.0}), {:.0} energy stored",
                    here.x,
                    here.z,
                    self.scouts_stored()
                ));
            }
            return;
        }
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
            w.held = false;
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

#[cfg(test)]
mod policy_tests {
    use super::*;

    #[test]
    fn air_scouts_round_down_with_a_minimum_of_one_only_for_sparrows() {
        for (area, count) in [
            (0.0, 0),
            (0.1, 0),
            (26.2144, 0),
            (29.99, 0),
            (30.0, 1),
            (30.01, 1),
            (59.99, 1),
            (60.0, 2),
            (60.01, 2),
            (89.99, 2),
            (90.0, 3),
        ] {
            let policy = scout_policy(area, 50.0, 0.0, 0.0);
            assert_eq!(policy.sparrows, count.max(1), "Sparrow quota at area {area}");
            assert_eq!(policy.owls, count, "Owl quota at area {area}");
        }
    }

    #[test]
    fn small_maps_do_not_request_owls_even_with_high_income_and_density() {
        for area in [0.1, 26.2144, 29.99] {
            for (income, density) in [(50.0, 0.0), (0.0, 500.0), (200.0, 2000.0)] {
                let policy = scout_policy(area, income, density, 0.0);
                assert_eq!(policy.owls, 0);
                assert_eq!(policy.sparrows, 1);
                assert_eq!(policy.widows, income >= 50.0 && density >= 1000.0);
            }
        }
    }

    #[test]
    fn sparrows_start_at_either_threshold_and_use_the_area_quota() {
        assert_eq!(scout_policy(20.0, 29.99, 199.99, 0.0).sparrows, 0);
        assert_eq!(scout_policy(20.0, 30.0, 0.0, 0.0).sparrows, 1);
        assert_eq!(scout_policy(20.0, 0.0, 200.0, 0.0).sparrows, 1);
        assert_eq!(scout_policy(60.0, 30.0, 0.0, 0.0).sparrows, 2);
        assert_eq!(scout_policy(0.1, 30.0, 0.0, 0.0).sparrows, 1);
    }

    #[test]
    fn deep_scouts_start_at_either_threshold_and_switch_at_exactly_25_aa() {
        assert_eq!(scout_policy(40.0, 49.99, 499.99, 0.0), ScoutPolicy { sparrows: 1, owls: 0, widows: false });
        for (income, density) in [(50.0, 0.0), (0.0, 500.0)] {
            let light = scout_policy(40.0, income, density, 999.99);
            assert_eq!(light.owls, 1);
            assert!(!light.widows);
            let heavy = scout_policy(40.0, income, density, 1000.0);
            assert_eq!(heavy.owls, 0);
            assert_eq!(heavy.widows, income >= 50.0);
        }
        // Changing map area changes AA density as well as scout count.
        assert_eq!(scout_policy(80.0, 50.0, 0.0, 1000.0).owls, 2);
    }

    #[test]
    fn widows_need_50_income_even_in_heavy_aa_or_at_high_density() {
        for density in [0.0, 500.0, 1000.0, 2000.0] {
            for aa in [0.0, 500.0] {
                assert!(!scout_policy(20.0, 49.99, density, aa).widows);
            }
        }
        assert!(scout_policy(20.0, 50.0, 500.0, 500.0).widows);
        assert!(scout_policy(20.0, 50.0, 1000.0, 0.0).widows);
        // Density can still request Owls while income is below the Widow floor.
        assert_eq!(scout_policy(40.0, 49.99, 1000.0, 0.0), ScoutPolicy { sparrows: 1, owls: 1, widows: false });
    }

    #[test]
    fn high_density_adds_widows_even_with_light_aa_and_owls() {
        assert!(!scout_policy(40.0, 50.0, 999.99, 0.0).widows);
        assert_eq!(scout_policy(40.0, 50.0, 1000.0, 0.0), ScoutPolicy { sparrows: 1, owls: 1, widows: true });
        assert_eq!(scout_policy(40.0, 50.0, 1000.0, 1000.0), ScoutPolicy { sparrows: 1, owls: 0, widows: true });
    }
}
