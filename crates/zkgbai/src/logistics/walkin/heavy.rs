//! Heavy walk-in turrets (`heavyporc`, on by default; `disable=heavyporc`).
//!
//! The user, 2026-11-04: "Now that we have learned to fortify in response
//! to threat, we seem sometimes to fortify a lot. But the scale of it stops
//! at Stinger. It shouldn't; when we have access to enough energy and grid
//! at a place we want to fortify, and would like to queue more than 3
//! stingers, what we should do is a terraform-platform elevated Lucifer."
//! Then: "Well, or a Desolator. Or both", and "if a convenient local spire
//! is available, that also works".
//!
//! - *More than 3 Stingers*: the walk-in turrets the place has had in the
//!   last [`CAP_WINDOW`] and the Stingers it would take to make up what the
//!   force has over all that meets it, together more than [`PLACE_CAP`];
//!   or any shortfall at a place that has [`super::MID_CAP`] mid-heavy
//!   turrets standing already (`midcap`: past those, a heavy or units).
//! - *Energy and grid*: Lucifer and Desolator fire only on a grid making 50
//!   energy (`neededlink`). A site has it if the grids it would join make
//!   that much, or if [`MAX_PYLONS`] pylons link it to grids that do (they
//!   are queued with the turret).
//! - *Which*: the Lucifer (2200 metal, 1200 range, 4000 a shot every 10 s)
//!   against heavy forces and forces that outrange a Desolator; the
//!   Desolator (1500, 10000 health, heat ray and plasma to 650) against
//!   light ones, which waste most of a Lucifer shot ([`LUCIFER_PREY`]).
//!   Both when the force still has more than [`PLACE_CAP`] Stingers over
//!   what meets it once the first is counted.
//! - *Elevated*: the Lucifer stands [`PLATFORM`] elmos above the place and
//!   the force's approach. Its beam has heightMod 1, so from 80 up it still
//!   reaches 1197 across; it fires over our own buildings and units in the
//!   way, and nothing climbs to it (the terraform skirt falls at 74
//!   degrees, steeper than even spiders take). On a spire near the place as
//!   it stands (the site needing least raising, then the nearest); else the
//!   nearest constructors raise the rest first and it goes up on top (not
//!   with an enemy within [`RAISE_CLEAR`] of the site: the gadget then
//!   raises 20 times slower; one that comes that close mid-raise has the
//!   Lucifer go up on what there is). The
//!   site covers the place against the force (whatever has the place in
//!   its reach is in the Lucifer's), is not in front of the place, and sees
//!   the force's approach. The Desolator stands toward the force on the
//!   ground, as the Stingers do.
//! - *Worth it*: a heavy costs no more than what its place loses in an
//!   outage ([`WORTH_SHARE`]; a place's value is its flow for 120 s), so
//!   from 12.5 metal a second through the place for a Desolator, 18 for a
//!   Lucifer (a factory, a fusion, a mex field); cheaper places keep the
//!   usual turrets.
//! - *Not an emergency turret*: a heavy may stand after the force arrives,
//!   within [`ETA`]; its deadline is when it can stand; unstarted, it is
//!   stood down after [`STAND_DOWN`] without a force calling for it; it is
//!   built at high priority but holds no other construction back (`pause`).
//!   At most [`MAX_OPEN`] at a time, [`PER_PLACE`] per place per
//!   [`CAP_WINDOW`].
//!
//! When no heavy can be had (no grid, no site, too late, capped) the place
//! gets the usual walk-in turret.
//!
//! Journal (`k=walkin`): `heavy` (one opened), `heavy_raise` (a platform
//! ordered), `heavy_none` (what refused each turret), `heavy_refused` (the
//! first choice refused, another taken), `heavy_capped`; the
//! platform's progress in the log.

use std::collections::BTreeMap;

use serde_json::{Value, json};
use springai::{Unit, UnitDef, Vec3};

use super::{Assessment, CAP_RADIUS, CAP_WINDOW, OFFSET, PLACE_CAP, RANGE_SLACK, STRETCH, TOUCH_MIN, VAR_RADIUS};
use crate::Ai;
use crate::economy::TaskId;
use crate::economy::grid::GridAccess;
use crate::economy::terraform::is_terraforming;
use crate::logistics::{Kind, MARGIN, MAX_CREW, Worth, finish_time, porc_power};
use crate::util::{distance, get_directional_point};

/// The switch.
pub const FEATURE: &str = "heavyporc";

const LUCIFER: &str = "turretantiheavy";
const DESOLATOR: &str = "turretheavy";
const STINGER: &str = "turretheavylaser";

/// The heavy turrets.
pub(crate) const HEAVIES: [&str; 2] = [LUCIFER, DESOLATOR];

/// The Lucifer goes first against a force that puts at least this share of
/// a Lucifer shot into what it kills (value-weighted; see [`shot_use`]):
/// Glaives (230 health) 0.06, Thugs (960) 0.24, Ogres (1950) 0.49,
/// Ravagers (2200) 0.55, Knights (2500) 0.63, Minotaurs (7200) 0.9,
/// Cyclopes (12000) 1.
const LUCIFER_PREY: f32 = 0.5;

/// A heavy costs at most this share of what its place loses in an outage
/// (its value): no 1500-metal Desolator for three mexes worth 700.
const WORTH_SHARE: f32 = 1.0;

/// Pylons a heavy's site may need to its power, at most.
const MAX_PYLONS: f32 = 2.0;

/// A heavy must be able to stand within this long, seconds (forecast,
/// stretched like walk-in turrets').
const ETA: f32 = 120.0;

/// An unstarted heavy no force has called for in this many frames is
/// dropped (a platform too).
pub(super) const STAND_DOWN: i32 = 30 * 60;

/// Heavies planned at once, at most; per place per [`CAP_WINDOW`].
const MAX_OPEN: usize = 2;
const PER_PLACE: usize = 2;

/// How far the Lucifer stands above the higher of the place's ground and
/// the force's approach, elmos; no more than this is raised.
const PLATFORM: f32 = 80.0;
const MAX_RAISE: f32 = 160.0;

/// Height counts as there this close to it, elmos.
const UP_SLACK: f32 = 8.0;

/// The Lucifer's beam leaves this high above its footing, and aims this
/// high above the ground, elmos (for sight lines).
const AIM: f32 = 40.0;
const TARGET: f32 = 20.0;

/// Spire sites are looked for on a grid this fine, out to this far from
/// the place, elmos; the best this many are checked for sight.
const STEP: f32 = 32.0;
const SEARCH: f32 = 600.0;
const SIGHT_TRIES: usize = 24;

/// The Lucifer's reach less the force's, less this, is how far from the
/// place it may stand, elmos.
const COVER_SLACK: f32 = 100.0;

/// It may stand at most this much nearer the force's approach than the
/// place itself, elmos.
const FRONT_SLACK: f32 = 100.0;

/// A platform's top is the footprint and one 16-elmo ring wider; it keeps
/// this many build squares (16 elmos) from other buildings for its skirt
/// (terraforming stops at buildings).
const RING: f32 = 16.0;
const CLEARANCE: i32 = 3;

/// The Lucifer goes on the platform within this of its middle, elmos.
const SITE_SLACK: f32 = 24.0;

/// A place no heavy could be had for is not looked at again for this many
/// frames (the spire search is some thousand points).
const REFUSED: i32 = 300;

/// No platform is raised (or raised on) with an enemy seen within this of
/// its site lately: the gadget raises 20 times slower with one within
/// about 170 of it (smoke1/g3: 0 of 80 in 61 s), elmos and frames.
const RAISE_CLEAR: f32 = 300.0;
const RAISE_SEEN: i32 = 90;

/// Constructors that raise a platform; frames it gets; constructors sent
/// one after another as they are called away; frames before their orders
/// tell.
const RAISERS: usize = 2;
/// Raisers within this many seconds of the site, at most (stage3/s1 sent
/// one from 8900 elmos off); none that near: the Lucifer goes on the ground.
const RAISER_TRAVEL: f32 = 20.0;
const RAISE_FRAMES: i32 = 30 * 60;
const RAISE_TRIES: u32 = 3;
const RAISE_GRACE: i32 = 90;

/// A platform going up for a Lucifer.
struct Raise {
    def: UnitDef,
    /// The place it guards.
    anchor: Vec3,
    site: Vec3,
    square: [Vec3; 4],
    /// The height asked for the top, and the ground before.
    top: f32,
    ground: f32,
    workers: Vec<Unit>,
    since: i32,
    ordered: i32,
    tries: u32,
    /// Last frame a force had its place within reach.
    called: i32,
    value: f32,
    by: f32,
    against: f32,
    why: String,
}

#[derive(Default)]
pub struct State {
    /// Heavies planned: their task and the place each guards.
    pub(super) tasks: BTreeMap<TaskId, Vec3>,
    raise: Option<Raise>,
    /// Places a heavy went to, and when (see [`PER_PLACE`]).
    opened: Vec<(Vec3, i32)>,
    /// Places no heavy could be had for, and when (see [`REFUSED`]).
    refused: Vec<(Vec3, i32)>,
}

impl State {
    /// Whether heavy `task` guards the place at `anchor`.
    pub(super) fn guards(&self, task: TaskId, anchor: Vec3) -> bool {
        self.tasks.get(&task).is_some_and(|&p| distance(p, anchor) <= VAR_RADIUS)
    }

    /// Whether a platform rises for a heavy guarding the place at `anchor`.
    pub(super) fn raising(&self, anchor: Vec3) -> bool {
        self.raise.as_ref().is_some_and(|r| distance(r.anchor, anchor) <= VAR_RADIUS)
    }
}

#[derive(Default, Clone, Copy)]
pub struct Stats {
    /// Breaches that wanted more than [`PLACE_CAP`] Stingers.
    pub wanted: u32,
    pub lucifers: u32,
    pub desolators: u32,
    /// Lucifers on a spire as it stood; on the ground, a platform too slow.
    pub spires: u32,
    pub flat: u32,
    /// Platforms ordered; up; given up short.
    pub platforms: u32,
    pub raised: u32,
    pub short: u32,
    /// No heavy to be had: the usual turret.
    pub none: u32,
    pub capped: u32,
    pub stood_down: u32,
}

impl Stats {
    pub fn json(&self) -> Value {
        json!({
            "wanted": self.wanted, "lucifers": self.lucifers, "desolators": self.desolators, "spires": self.spires, "flat": self.flat,
            "platforms": self.platforms, "raised": self.raised, "short": self.short, "none": self.none,
            "capped": self.capped, "stood_down": self.stood_down,
        })
    }
}

/// Stingers the place would have had: those it had (`here`) and those that
/// make up `shortfall` power, of `stinger` each.
fn stingers_wanted(here: usize, shortfall: f32, stinger: f32) -> usize {
    here + (shortfall.max(0.0) / stinger.max(1.0)).ceil() as usize
}

/// The share of shots of `shot` damage that goes into what they kill, over
/// a force's members (health, value), by value.
fn shot_use(members: &[(f32, f32)], shot: f32) -> f32 {
    let total: f32 = members.iter().map(|m| m.1).sum();
    if total <= 0.0 || shot <= 0.0 {
        return 1.0;
    }
    let used = |hp: f32| {
        let hp = hp.max(1.0);
        hp / ((hp / shot).ceil().max(1.0) * shot)
    };
    members.iter().map(|&(hp, value)| value * used(hp)).sum::<f32>() / total
}

/// The heavies in the order tried: the Lucifer first against a force that
/// uses its shots (`prey`) or that a Desolator does not reach.
fn order(prey: f32, desolator_reaches: bool) -> [&'static str; 2] {
    if prey >= LUCIFER_PREY || !desolator_reaches { [LUCIFER, DESOLATOR] } else { [DESOLATOR, LUCIFER] }
}

/// Metal raising a platform with a top `side` elmos across by `h` elmos
/// costs: 0.0064 a heightmap point (8 elmos apart) an elmo, the top
/// levelled in whole 16-elmo cells (a ring of points more), a skirt
/// falling 28 every 8 elmos, 6 a terraunit (the gadget's figures, see
/// zkgbai-runs/siege/CRAB-SPIRE.md).
fn platform_metal(side: f32, h: f32) -> f32 {
    const PER: f32 = 0.0064;
    let n = (side / 8.0).ceil() + 3.0;
    let mut metal = 6.0 + n * n * h * PER;
    let (mut ring, mut rest) = (1.0, h - 28.0);
    while rest > 0.0 {
        metal += 4.0 * (n + 2.0 * ring - 1.0) * rest * PER;
        rest -= 28.0;
        ring += 1.0;
    }
    metal
}

/// The largest single hit of `def`'s weapons.
fn max_shot(def: UnitDef) -> f32 {
    (def.weapon_mounts().iter().filter_map(|m| m.weapon_def()))
        .map(|wd| {
            let d = wd.damage_types();
            d.get(1).or(d.first()).copied().unwrap_or(0.0)
        })
        .fold(0.0, f32::max)
}

/// A platform's top for `def` at `site`: the polygon's corners, just inside
/// the cells to level (the gadget levels whole 16-elmo cells from those
/// the corners floor to through the next row and column).
fn platform_square(def: UnitDef, site: Vec3) -> [Vec3; 4] {
    let (hx, hz) = (def.x_size() as f32 * 4.0 + RING - 4.0, def.z_size() as f32 * 4.0 + RING - 4.0);
    [(-hx, -hz), (hx, -hz), (hx, hz), (-hx, hz)].map(|(dx, dz)| Vec3::xz(site.x + dx, site.z + dz))
}

/// Where a Lucifer stands: its site, what to raise there, and the height
/// it is measured from.
struct Perch {
    site: Vec3,
    raise: f32,
    base: f32,
}

impl Ai {
    /// More than [`PLACE_CAP`] Stingers wanted at breach `a`'s place
    /// (`here` walk-in turrets it had lately): a Lucifer, a Desolator or
    /// both in their stead, where the grid powers them. Whether one was
    /// planned (else the usual turret goes).
    pub(super) fn walkin_heavy(&mut self, a: &Assessment, now: f32, here: usize, mid_full: bool) -> bool {
        if self.dev_off(FEATURE) {
            return false;
        }
        let Some(stinger) = self.defs.get(STINGER) else { return false };
        let s = porc_power(stinger);
        let shortfall = a.group.power - a.ours.total();
        let wanted = stingers_wanted(here, shortfall, s);
        if wanted <= PLACE_CAP && !mid_full {
            return false;
        }
        let (frame, anchor) = (self.frame, a.place.anchor);
        let st = &mut self.logistics.walkin.heavy;
        st.refused.retain(|r| frame - r.1 < REFUSED);
        if st.refused.iter().any(|r| distance(r.0, anchor) <= VAR_RADIUS) {
            return false;
        }
        st.opened.retain(|o| frame - o.1 < CAP_WINDOW);
        let open = st.tasks.len() + usize::from(st.raise.is_some());
        let lately = st.opened.iter().filter(|o| distance(o.0, anchor) <= CAP_RADIUS).count();
        // At most PER_PLACE opened lately, and as many standing (a rolling
        // window alone lets them pile up: see `MIDCAP`).
        let at_place = lately.max(self.walkin_standing(anchor, &HEAVIES));
        self.logistics.walkin.stats.heavy.wanted += 1;
        if open >= MAX_OPEN || at_place >= PER_PLACE {
            self.logistics.walkin.stats.heavy.capped += 1;
            self.walkin_note("heavy_capped", a, json!({"wanted": wanted, "open": open, "here": at_place}));
            return false;
        }
        let mut slots = (MAX_OPEN - open).min(PER_PLACE - at_place);
        let prey = shot_use(&a.group.members, self.defs.get(LUCIFER).map_or(0.0, max_shot));
        let desolator_reaches =
            self.defs.get(DESOLATOR).is_some_and(|d| d.max_weapon_range() + RANGE_SLACK >= a.group.reach);
        let (mut left, mut chosen, mut refused) = (shortfall, Vec::new(), Vec::new());
        for name in order(prey, desolator_reaches) {
            if slots == 0 || (!chosen.is_empty() && stingers_wanted(0, left, s) <= PLACE_CAP) {
                break;
            }
            let Some(def) = self.defs.get(name) else { continue };
            match self.walkin_heavy_try(def, a, now, wanted, prey) {
                Ok(()) => {
                    slots -= 1;
                    left -= porc_power(def);
                    chosen.push(name);
                }
                Err(why) => refused.push(json!([name, why])),
            }
        }
        if !chosen.is_empty() && !refused.is_empty() {
            self.walkin_note("heavy_refused", a, json!({"chosen": chosen, "refused": refused}));
        }
        if chosen.is_empty() {
            self.logistics.walkin.heavy.refused.push((anchor, frame));
            self.logistics.walkin.stats.heavy.none += 1;
            let extra = json!({"wanted": wanted, "prey": (prey * 100.0).round() / 100.0, "refused": refused});
            self.walkin_note("heavy_none", a, extra);
            return false;
        }
        true
    }

    /// Plan heavy `def` for breach `a`, or say why not.
    fn walkin_heavy_try(
        &mut self,
        def: UnitDef,
        a: &Assessment,
        now: f32,
        wanted: usize,
        prey: f32,
    ) -> Result<(), &'static str> {
        if def.max_weapon_range() + RANGE_SLACK < a.group.reach {
            return Err("outranged");
        }
        if def.cost(self.metal) > WORTH_SHARE * a.place.value {
            return Err("dear");
        }
        let name = self.defs.name(def).to_owned();
        let (anchor, approach, frame) = (a.place.anchor, a.approach, self.frame);
        let perch = if name == LUCIFER { Some(self.walkin_heavy_perch(def, a).ok_or("nosite")?) } else { None };
        let site = match &perch {
            Some(p) => p.site,
            None if distance(anchor, approach) > 1.0 => get_directional_point(anchor, approach, OFFSET),
            None => anchor,
        };
        let gd = &self.eco.grid_defs;
        let (range, needs) = (gd.range(def.id()).unwrap_or(0.0), gd.needs(def.id()));
        let (link, grid) = match self.eco_grid_access(site, range, needs) {
            GridAccess::Powered(e) => (None, e),
            GridAccess::Link { from, range: r, pylons, energy } if pylons <= MAX_PYLONS => {
                (Some((from, r, pylons)), energy)
            }
            _ => return Err("nogrid"),
        };
        let hands = self.walkin_hands(site, &[], None, false);
        if hands.is_empty() {
            return Err("nohands");
        }
        let k = hands.len().min(MAX_CREW);
        let (metal, income) = (self.eco.metal, self.eco.effective_income);
        let raise = perch.as_ref().map_or(0.0, |p| p.raise);
        let side = def.x_size().max(def.z_size()) as f32 * 8.0 + 2.0 * RING;
        // No platform where an enemy stands by already (see `RAISE_CLEAR`)
        // or with nobody near to raise it: the Lucifer goes on the ground.
        let raisers = if raise > UP_SLACK { self.walkin_heavy_raiser_hands(site) } else { Vec::new() };
        let clear = self.walkin_heavy_enemy_near(site) > RAISE_CLEAR;
        let raising = raise > UP_SLACK && clear && !raisers.is_empty();
        let raise_s = if raising {
            let m = platform_metal(side, raise);
            finish_time(&raisers, m, m, metal, income) * STRETCH
        } else {
            0.0
        };
        let eta = if raising { raise_s } else { 0.0 }
            + finish_time(&hands[..k], def.build_time(), def.cost(self.metal), metal, income) * STRETCH;
        if eta > ETA {
            return Err("late");
        }
        let by = now + (a.seconds.max(a.dwell) - MARGIN).max(eta);
        let why = format!(
            "walk-in, heavy: {} units worth {:.0} (power {:.0}, reach {:.0}) {:.0}s from {:.0} of ours; met by {:.0}: \
             {wanted} Stingers wanted here; {name} +{:.0} on {grid:.0} energy{}, eta {eta:.0}s",
            a.group.units,
            a.group.value,
            a.group.power,
            a.group.reach,
            a.seconds,
            a.place.value,
            a.ours.total(),
            porc_power(def),
            match &perch {
                Some(p) if raising => format!(", on a platform raised {:.0}", p.raise),
                Some(p) if p.raise > UP_SLACK && !clear => format!(" on the ground (an enemy within {RAISE_CLEAR:.0})"),
                Some(p) if p.raise > UP_SLACK => " on the ground (nobody near to raise it)".to_owned(),
                Some(_) => " on a spire".to_owned(),
                None => String::new(),
            }
        );
        let extra = json!({
            "turret": name, "site": [site.x.round(), site.z.round()], "eta": (eta * 10.0).round() / 10.0,
            "wanted": wanted, "prey": (prey * 100.0).round() / 100.0, "grid": grid.round(),
            "pylons": link.map_or(0.0, |l| l.2), "raise": raise.round(), "raise_s": raise_s.round(),
            "raising": raising, "hands": hands.len(),
        });
        if raising {
            let Some(p) = perch else { return Err("nosite") };
            let ids: Vec<i32> = raisers.iter().map(|h| h.0).collect();
            let workers = self.walkin_heavy_take(&ids);
            if workers.is_empty() {
                return Err("noraisers");
            }
            let square = platform_square(def, site);
            let map = self.cb.map();
            let ground = map.elevation_at(site.x, site.z);
            let top = square.iter().map(|q| map.elevation_at(q.x, q.z)).fold(p.base + PLATFORM, f32::max);
            self.terraform_level(&workers, &square, top, false);
            self.eco_hold_terraform(&workers, RAISE_FRAMES);
            self.log(&format!(
                "walkin: heavy: raising a platform {:.0} up at ({:.0}, {:.0}) for a Lucifer guarding ({:.0}, {:.0}) \
                 ({} constructors)",
                top - ground,
                site.x,
                site.z,
                anchor.x,
                anchor.z,
                workers.len()
            ));
            self.logistics.walkin.heavy.raise = Some(Raise {
                def,
                anchor,
                site,
                square,
                top,
                ground,
                workers,
                since: frame,
                ordered: frame,
                tries: 1,
                called: frame,
                value: a.place.value,
                by,
                against: a.group.power,
                why,
            });
            self.logistics.walkin.stats.heavy.platforms += 1;
            self.walkin_note("heavy_raise", a, extra);
        } else {
            let task = if perch.is_some() {
                self.eco_logistics_porc_at(def, site, SITE_SLACK)
            } else {
                [site, anchor].into_iter().find_map(|s| self.eco_logistics_porc(def, s, 0.0).map(|(t, _)| t))
            };
            let task = task.ok_or("nosite")?;
            self.walkin_heavy_open(def, task, anchor, a.place.value, by, a.group.power, &why).ok_or("refused")?;
            match perch {
                Some(_) if raise > UP_SLACK => self.logistics.walkin.stats.heavy.flat += 1,
                Some(_) => self.logistics.walkin.stats.heavy.spires += 1,
                None => {}
            }
            self.walkin_note("heavy", a, extra);
        }
        self.logistics.walkin.heavy.opened.push((anchor, frame));
        if let Some((from, r, _)) = link {
            self.eco_lay_pylons(from, r, site, range, &format!("a {name} guarding ({:.0}, {:.0})", anchor.x, anchor.z));
        }
        Ok(())
    }

    /// Where a Lucifer guarding breach `a`'s place stands: of the sites in
    /// reach that are not in front of the place and that we hold, the one
    /// needing least raising to [`PLATFORM`] (a spire needs none), then the
    /// nearest, that sees the force's approach from its top.
    fn walkin_heavy_perch(&self, def: UnitDef, a: &Assessment) -> Option<Perch> {
        let (anchor, approach) = (a.place.anchor, a.approach);
        let map = self.cb.map();
        let ground = |p: Vec3| map.elevation_at(p.x, p.z).max(0.0);
        let base = ground(anchor).max(ground(approach));
        let touch = a.group.reach.max(TOUCH_MIN);
        let cover = (def.max_weapon_range() - touch - COVER_SLACK).clamp(0.0, SEARCH);
        let n = (cover / STEP) as i32;
        let (w, h) = (map.width() as f32 * 8.0, map.height() as f32 * 8.0);
        let front = distance(anchor, approach) - FRONT_SLACK;
        let war = &self.war;
        let mut spots: Vec<(f32, f32, Vec3)> = Vec::new();
        for i in -n..=n {
            for j in -n..=n {
                let p = Vec3::xz(anchor.x + i as f32 * STEP, anchor.z + j as f32 * STEP);
                let d = distance(p, anchor);
                if d > cover || p.x < 64.0 || p.z < 64.0 || p.x > w - 64.0 || p.z > h - 64.0 {
                    continue;
                }
                if distance(p, approach) < front || (d > STEP && !self.graph.is_ally_territory(p)) {
                    continue;
                }
                let raise = (PLATFORM - (ground(p) - base)).max(0.0);
                if raise > MAX_RAISE
                    || war.get_porc_threat(p) > 0.0
                    || war.get_effective_threat(p) > war.get_total_friendly_threat(p)
                {
                    continue;
                }
                spots.push((raise, d, p));
            }
        }
        // Least to raise (to the 8 elmos), then nearest.
        let key = |r: f32| (r / UP_SLACK).ceil();
        spots.sort_by(|x, y| key(x.0).total_cmp(&key(y.0)).then(x.1.total_cmp(&y.1)));
        let hm = &self.eco.height_map;
        for &(raise, _, p) in spots.iter().take(SIGHT_TRIES) {
            if !hm.sees(p, raise + AIM, approach, TARGET) {
                continue;
            }
            let natural = raise <= UP_SLACK;
            let site = if natural && map.is_possible_to_build_at(def, p, 0) {
                p
            } else {
                map.find_closest_build_site(def, p, STEP, if natural { 0 } else { CLEARANCE }, 0)
            };
            if site.x < 0.0 {
                continue;
            }
            let raise = (PLATFORM - (ground(site) - base)).max(0.0);
            if raise <= MAX_RAISE {
                return Some(Perch { site, raise, base });
            }
        }
        None
    }

    /// The nearest free constructors for a platform at `site`, taken off
    /// their jobs.
    fn walkin_heavy_raisers(&mut self, site: Vec3) -> Vec<Unit> {
        let ids: Vec<i32> = self.walkin_heavy_raiser_hands(site).iter().map(|h| h.0).collect();
        self.walkin_heavy_take(&ids)
    }

    /// The constructors that would raise a platform at `site`: the nearest
    /// [`RAISERS`] free ones within [`RAISER_TRAVEL`] of it.
    fn walkin_heavy_raiser_hands(&self, site: Vec3) -> Vec<crate::logistics::Hand> {
        (self.logistics_hands(site, &[], None).into_iter())
            .filter(|h| h.1 <= RAISER_TRAVEL)
            .filter(|h| self.eco.workers.get(&h.0).is_some_and(|w| !w.assigned_plop && !w.unit.is_being_built()))
            .take(RAISERS)
            .collect()
    }

    /// Workers `ids`, taken off their jobs.
    fn walkin_heavy_take(&mut self, ids: &[i32]) -> Vec<Unit> {
        let mut units = Vec::new();
        for &id in ids {
            if let Some(w) = self.eco.workers.get_mut(&id) {
                w.clear_task(&mut self.eco.tasks);
                units.push(w.unit);
            }
        }
        units
    }

    /// Open the walk-in issue on heavy `def`'s task, guarding the place at
    /// `anchor` worth `value`, due `by`.
    #[allow(clippy::too_many_arguments)]
    fn walkin_heavy_open(
        &mut self,
        def: UnitDef,
        task: TaskId,
        anchor: Vec3,
        value: f32,
        by: f32,
        against: f32,
        why: &str,
    ) -> Option<u32> {
        // Unstamped, as walk-in turrets are (see `walkin_fortify`).
        if let Some(c) = self.eco.tasks.get_mut(task).and_then(|t| t.as_construction_mut()) {
            c.frame_issued = 0;
        }
        let id = self.logistics_open(task, Kind::Walkin, Worth::Deadline { value, by }, why, against)?;
        let frame = self.frame;
        let w = &mut self.logistics.walkin;
        w.opened.push((anchor, frame, task));
        w.called.insert(task, frame);
        w.stats.opened += 1;
        w.heavy.tasks.insert(task, anchor);
        if self.defs.name(def) == LUCIFER {
            w.stats.heavy.lucifers += 1;
        } else {
            w.stats.heavy.desolators += 1;
        }
        self.log(&format!(
            "walkin: heavy: a {} guards ({:.0}, {:.0}) (issue #{id})",
            self.defs.name(def),
            anchor.x,
            anchor.z
        ));
        Some(id)
    }

    /// A force has the place at `anchor` within reach: the heavies that
    /// guard it are called for.
    pub(super) fn walkin_heavy_call(&mut self, anchor: Vec3, frame: i32) {
        let w = &mut self.logistics.walkin;
        for (&task, &at) in &w.heavy.tasks {
            if distance(at, anchor) <= VAR_RADIUS {
                w.called.insert(task, frame);
            }
        }
        if let Some(r) = w.heavy.raise.as_mut()
            && distance(r.anchor, anchor) <= VAR_RADIUS
        {
            r.called = frame;
        }
    }

    /// How far the nearest enemy seen lately ([`RAISE_SEEN`]) is from `p`.
    fn walkin_heavy_enemy_near(&self, p: Vec3) -> f32 {
        (self.war.targets.values())
            .filter(|e| !e.is_dead && self.frame - e.last_seen <= RAISE_SEEN)
            .map(|e| distance(e.position, p))
            .fold(f32::INFINITY, f32::min)
    }

    /// See to the platform going up: another constructor when one is
    /// called away, the Lucifer on top once it is up (or has had its time),
    /// dropped when no force calls for it.
    pub(super) fn walkin_heavy_update(&mut self, now: f32) {
        let frame = self.frame;
        let issues = &self.logistics.issues;
        self.logistics.walkin.heavy.tasks.retain(|t, _| issues.contains_key(t));
        let Some(r) = self.logistics.walkin.heavy.raise.as_ref() else { return };
        let map = self.cb.map();
        let (site, square, top, ground) = (r.site, r.square, r.top, r.ground);
        let (up, asked) = (map.elevation_at(site.x, site.z) - ground, top - ground);
        let busy = r.workers.iter().any(|&u| u.health() > 0.0 && is_terraforming(u));
        let quiet = frame - r.ordered > RAISE_GRACE && !busy;
        let done = up >= asked - UP_SLACK;
        let (since, tries, called) = (r.since, r.tries, r.called);
        // An enemy at the platform: the Lucifer goes up on what there is.
        let enemy = self.walkin_heavy_enemy_near(site);
        let beset = !done && enemy <= RAISE_CLEAR;
        if frame - called > STAND_DOWN {
            let Some(r) = self.logistics.walkin.heavy.raise.take() else { return };
            self.eco_release_terraform(&r.workers);
            self.logistics.walkin.stats.heavy.stood_down += 1;
            self.log(&format!(
                "walkin: heavy: no force calls for the Lucifer at ({:.0}, {:.0}) any more; its platform stays {up:.0} of \
                 {asked:.0} up",
                site.x, site.z
            ));
            return;
        }
        if !done && !beset && quiet && tries < RAISE_TRIES && frame - since <= RAISE_FRAMES {
            let workers = self.walkin_heavy_raisers(site);
            if workers.is_empty() {
                return;
            }
            self.terraform_level(&workers, &square, top, false);
            self.eco_hold_terraform(&workers, RAISE_FRAMES - (frame - since));
            self.log(&format!(
                "walkin: heavy: the platform at ({:.0}, {:.0}) is {up:.0} of {asked:.0} up and nobody raises it; {} \
                 more sent ({})",
                site.x,
                site.z,
                workers.len(),
                tries + 1
            ));
            if let Some(r) = self.logistics.walkin.heavy.raise.as_mut() {
                (r.workers, r.ordered, r.tries) = (workers, frame, tries + 1);
            }
            return;
        }
        if !(done || beset || quiet || frame - since > RAISE_FRAMES) {
            return;
        }
        let Some(r) = self.logistics.walkin.heavy.raise.take() else { return };
        self.eco_release_terraform(&r.workers);
        if done {
            self.logistics.walkin.stats.heavy.raised += 1;
        } else {
            self.logistics.walkin.stats.heavy.short += 1;
        }
        // On top; else (the half-raised top refused) the nearest legal site.
        let task = self
            .eco_logistics_porc_at(r.def, site, SITE_SLACK)
            .or_else(|| self.eco_logistics_porc(r.def, site, 0.0).map(|(t, _)| t));
        // Who raised it, and whether an enemy stood by (the 20x penalty).
        let raisers: Vec<String> = (r.workers.iter())
            .map(|&u| {
                let state = if u.health() <= 0.0 {
                    "dead"
                } else if is_terraforming(u) {
                    "raising"
                } else {
                    "off"
                };
                format!("{}@{:.0} {state}", u.id(), distance(u.pos(), site))
            })
            .collect();
        self.log(&format!(
            "walkin: heavy: platform at ({:.0}, {:.0}) {up:.0} of {asked:.0} up after {:.0}s (raisers {}; nearest \
             enemy seen {enemy:.0} off); {}",
            site.x,
            site.z,
            (frame - since) as f32 / 30.0,
            raisers.join(", "),
            if task.is_some() { "the Lucifer goes on top (or nearest)" } else { "no site for the Lucifer about it" }
        ));
        let Some(task) = task else { return };
        let hands = self.walkin_hands(site, &[], None, false);
        let k = hands.len().min(MAX_CREW);
        let (metal, income) = (self.eco.metal, self.eco.effective_income);
        let eta = finish_time(&hands[..k], r.def.build_time(), r.def.cost(self.metal), metal, income) * STRETCH;
        let by = if eta.is_finite() { r.by.max(now + eta) } else { r.by };
        self.walkin_heavy_open(r.def, task, r.anchor, r.value, by, r.against, &r.why);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fourth_stinger_is_more_than_three() {
        // Two built here lately, 300 short at 244 a Stinger: two more.
        assert_eq!(stingers_wanted(2, 300.0, 244.0), 4);
        assert_eq!(stingers_wanted(0, 700.0, 244.0), 3);
        assert_eq!(stingers_wanted(0, 750.0, 244.0), 4);
        // Met already: only what it had.
        assert_eq!(stingers_wanted(3, -50.0, 244.0), 3);
    }

    #[test]
    fn lucifer_shots_go_to_waste_on_light_units() {
        let shot = 4000.0;
        // Glaives (230 hp): 6% of each shot.
        assert!((shot_use(&[(230.0, 65.0); 10], shot) - 0.0575).abs() < 1e-3);
        // A Cyclops (12000) takes three whole shots; a Minotaur (7200) two,
        // the second not all used.
        assert!((shot_use(&[(12000.0, 2200.0)], shot) - 1.0).abs() < 1e-3);
        assert!((shot_use(&[(7200.0, 850.0)], shot) - 0.9).abs() < 1e-3);
        // By value: three Ravagers (2200 each, 0.55) and 600 of Glaives.
        let m = [(2200.0, 250.0), (2200.0, 250.0), (2200.0, 250.0), (230.0, 300.0), (230.0, 300.0)];
        let u = shot_use(&m, shot);
        assert!((u - (0.55 * 750.0 + 0.0575 * 600.0) / 1350.0).abs() < 1e-3, "{u}");
        // Under the bar: the Desolator first.
        assert!(u < LUCIFER_PREY);
        // Nothing known: no waste assumed.
        assert_eq!(shot_use(&[], shot), 1.0);
    }

    #[test]
    fn lucifer_first_against_heavies_and_long_reach() {
        assert_eq!(order(0.55, true), [LUCIFER, DESOLATOR]);
        assert_eq!(order(0.24, true), [DESOLATOR, LUCIFER]);
        // Fencers outrange a Desolator: whatever they weigh.
        assert_eq!(order(0.1, false), [LUCIFER, DESOLATOR]);
    }

    #[test]
    fn a_platform_costs_a_tithe_of_its_lucifer() {
        // The Crab's 48 top, 64 up: the gadget figures say 53 metal.
        let crab = platform_metal(48.0, 64.0);
        assert!((40.0..70.0).contains(&crab), "{crab}");
        // A Lucifer's (64 + a ring each side) 80 up: well under 2200 / 10.
        let lucifer = platform_metal(96.0, 80.0);
        assert!((100.0..220.0).contains(&lucifer), "{lucifer}");
        // Higher costs more, faster than linearly once the skirt grows.
        assert!(platform_metal(96.0, 160.0) > 2.0 * lucifer);
    }
}
