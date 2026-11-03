//! `ammo` (on by default, `disable=ammo`): what the enemy's tube weapons
//! have left (user, 2026-11-03: a Kodachi that has tanked a Picket's three
//! missiles gets to kill it for free).
//!
//! ZK's tube weapons (scriptReload.lua, weapon customParams `script_reload`
//! and `script_burst`: the Picket's three missiles, the Ogre's two, the
//! Hacksaw's two, the Trident's three, the Angler's four) fire from tubes
//! that each reload on their own clock, `script_reload` seconds after the
//! tube fires; the engine reload only spaces the tubes. A Picket that has
//! fired its three missiles cannot fire for about ten seconds.
//!
//! How full a tube weapon is shows only to its own alliance
//! (`scriptLoaded` is allied access), but each hit on one of our units
//! names the weapon, the shooter when it is in our LOS or on radar, and
//! the bearing to where we last saw it (the engine's UnitDamaged). A hit is
//! a shot, so hits counted by shooter are a lower bound on the tubes it has
//! spent: the ledger misses the shots that missed, or hit an ally's units
//! (their hits go to their team), and so may think a tube loaded that is
//! empty, never the reverse unless it names the wrong shooter.
//!
//! Per enemy tube unit that has hit us, the ledger keeps the frame each
//! tube is loaded again. A shot takes the tube loaded longest, as the
//! sim does, from the frame it was fired: the hit's frame less the
//! projectile's flight at its mean speed. While the shooter is in LOS, a
//! stunned one's tubes wait and a slowed one's reload slower
//! (`totalReloadSpeedChange`, as scriptReload.lua does). Our own tube
//! units read theirs off the unit (`scriptLoaded`, `scriptReloadFrame`).
//!
//! Users: fight snapshots carry each unit's empty tubes
//! (`SnapUnit::tubes`, with [`MARGIN`] off the enemy's), and coalesce judges
//! a lone raider when an enemy in its fight has every tube empty
//! ([`Ai::ammo_open_near`]). Off, the ledger still counts and journals, and
//! the predictor sees every tube loaded.
//!
//! Journal `k=ammo`: `shot` (each shot counted: shooter, how it was named,
//! the tubes left), `est` and `own` samples on frames divisible by
//! [`SAMPLE`] while a tube is empty (the ledger's estimate of an enemy
//! unit; our own units as they read), `stats` per minute. In a mirror the
//! two sides' samples meet on unit id and frame.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use serde_json::json;
use springai::{Unit, Vec3, WeaponDef};

use crate::Ai;

pub const FEATURE: &str = "ammo";

/// Frames between samples (and stun/slow updates); samples fall on frames
/// divisible by it, so both sides of a mirror sample the same frames.
pub const SAMPLE: i32 = 15;
/// Hits from one shooter this close together (frames) are one shot (its
/// splash on several of our units).
const SAME_SHOT: i32 = 3;
/// A shooter named by bearing must be within this of the bearing...
const BEARING_COS: f32 = 0.966; // 15 degrees
/// ...and within its range times this, plus [`RANGE_PAD`] elmos.
const RANGE_SLACK: f32 = 1.15;
const RANGE_PAD: f32 = 64.0;
/// Flight times are capped (frames).
const MAX_FLIGHT: f32 = 90.0;
/// Frames taken off the enemy's reload clocks before anyone acts on them:
/// a tube is counted back this early.
pub const MARGIN: i32 = 15;
/// Frames between `stats` lines.
const REPORT_PERIOD: i32 = 1800;
/// Frames between refreshes of our own tube units.
const OWN_REFRESH: i32 = 150;

/// A tube weapon (scriptReload.lua).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TubeDef {
    pub tubes: usize,
    /// Frames each tube takes to reload after it fires.
    pub reload: i32,
    /// The engine reload between tubes (frames).
    pub spacing: i32,
    /// Mean projectile speed (elmos per frame; 0 = unknown, no flight).
    pub speed: f32,
}

impl TubeDef {
    /// From a weapon's engine reload (seconds), its customParams, and its
    /// projectile's start and top speed (elmos per frame). `None` for every
    /// weapon without `script_reload`.
    pub fn from_params(reload: f32, params: &[(String, String)], start: f32, top: f32) -> Option<TubeDef> {
        let param = |key: &str| params.iter().find(|(k, _)| k == key).and_then(|(_, v)| v.trim().parse::<f32>().ok());
        let secs = param("script_reload").filter(|s| *s > 0.0)?;
        let tubes = param("script_burst").unwrap_or(1.0).max(1.0) as usize;
        let speed = match (start > 0.0, top > 0.0) {
            (true, true) => 0.5 * (start + top),
            (false, true) => top,
            (true, false) => start,
            _ => 0.0,
        };
        Some(TubeDef {
            tubes,
            reload: (secs * 30.0).round().max(1.0) as i32,
            spacing: (reload * 30.0).round().max(1.0) as i32,
            speed,
        })
    }

    fn of(wd: WeaponDef) -> Option<TubeDef> {
        TubeDef::from_params(wd.reload(), &wd.custom_params(), wd.start_velocity(), wd.projectile_speed())
    }

    /// Frames a shot flew `dist` elmos.
    pub fn flight(&self, dist: f32) -> i32 {
        if self.speed > 0.0 { (dist / self.speed).min(MAX_FLIGHT).round() as i32 } else { 0 }
    }
}

/// What a hit did to a clock.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shot {
    /// Another hit of the last shot.
    Same,
    /// A shot from a loaded tube.
    Counted,
    /// A shot when the clock had every tube empty: the clock ran late (a
    /// shot misnamed, a reload sped up); the shot is believed.
    Surprise,
}

/// One enemy tube weapon's tubes.
#[derive(Clone, Debug)]
pub struct Clock {
    pub weapon: String,
    pub def: TubeDef,
    /// The frame each tube is loaded again (at or before now: loaded).
    pub ready: Vec<i32>,
    last_hit: i32,
    pub shots: u32,
}

impl Clock {
    pub fn new(weapon: String, def: TubeDef) -> Clock {
        Clock { weapon, def, ready: vec![i32::MIN / 2; def.tubes], last_hit: i32::MIN / 2, shots: 0 }
    }

    /// A hit seen on frame `hit`, of a shot fired on frame `fired`.
    pub fn shot(&mut self, hit: i32, fired: i32) -> Shot {
        if (hit - self.last_hit).abs() <= SAME_SHOT {
            return Shot::Same;
        }
        self.last_hit = hit;
        self.shots += 1;
        let Some(k) = (0..self.ready.len()).min_by_key(|&k| self.ready[k]) else { return Shot::Counted };
        let late = self.ready[k] > fired;
        self.ready[k] = fired + self.def.reload;
        if late { Shot::Surprise } else { Shot::Counted }
    }

    /// Frames from `now` until each empty tube is loaded, soonest first,
    /// with `margin` taken off (tubes due within it count as loaded).
    pub fn empty(&self, now: i32, margin: i32) -> Vec<i32> {
        let mut e: Vec<i32> = self.ready.iter().map(|r| r - now - margin).filter(|f| *f > 0).collect();
        e.sort_unstable();
        e
    }

    pub fn loaded(&self, now: i32) -> usize {
        self.ready.iter().filter(|r| **r <= now).count()
    }

    /// With every tube empty (`margin` as [`Clock::empty`]), the frames
    /// until the first is loaded.
    pub fn open_for(&self, now: i32, margin: i32) -> Option<i32> {
        let e = self.empty(now, margin);
        (e.len() == self.ready.len()).then(|| e.first().copied()).flatten()
    }

    /// `frames` passed with the reload running at `mult` (0 stunned, 1
    /// normal): the empty tubes come back that much later.
    pub fn slowed(&mut self, now: i32, frames: i32, mult: f32) {
        let lag = ((1.0 - mult.clamp(0.0, 1.0)) * frames as f32).round() as i32;
        if lag > 0 {
            for r in self.ready.iter_mut().filter(|r| **r > now) {
                *r += lag;
            }
        }
    }
}

/// Of the candidates (id, position) for a shooter, the one on `bearing`
/// (from `at`, flat) within `reach`: the best aligned within
/// [`BEARING_COS`].
pub fn by_bearing(at: Vec3, bearing: Vec3, reach: f32, candidates: impl Iterator<Item = (i32, Vec3)>) -> Option<i32> {
    let n = bearing.x.hypot(bearing.z);
    if n < 0.1 {
        return None;
    }
    let (bx, bz) = (bearing.x / n, bearing.z / n);
    let mut best: Option<(i32, f32)> = None;
    for (id, p) in candidates {
        let (dx, dz) = (p.x - at.x, p.z - at.z);
        let d = dx.hypot(dz);
        if d > reach {
            continue;
        }
        let cos = if d < 1.0 { 1.0 } else { (dx * bx + dz * bz) / d };
        if cos >= BEARING_COS && best.is_none_or(|(_, c)| cos > c) {
            best = Some((id, cos));
        }
    }
    best.map(|(id, _)| id)
}

/// Counts for the `stats` line.
#[derive(Clone, Copy, Debug, Default)]
struct Stats {
    /// Hits by tube weapons on our units, and by how the shooter was named.
    hits: u64,
    named: u64,
    bearing: u64,
    unnamed: u64,
    allied: u64,
    same: u64,
    counted: u64,
    surprises: u64,
}

/// The ledger.
#[derive(Default)]
pub struct Ammo {
    /// Weapon def id -> its tubes (`None`: not a tube weapon).
    weapons: HashMap<i32, Option<TubeDef>>,
    /// Enemy unit id -> its tube weapon's clock.
    pub clocks: BTreeMap<i32, Clock>,
    /// Our unit def id -> its first tube weapon (built on first update).
    own_defs: Option<HashMap<i32, TubeDef>>,
    /// Our own tube units.
    own: BTreeSet<i32>,
    own_refreshed: i32,
    /// Enemy id -> the frame its open window (every tube empty) ends, as
    /// journaled (`open`), so each window is journaled once.
    opened: BTreeMap<i32, i32>,
    stats: Stats,
    last_report: i32,
}

impl Ammo {
    fn tube_def(&mut self, wd: WeaponDef) -> Option<TubeDef> {
        *self.weapons.entry(wd.id()).or_insert_with(|| TubeDef::of(wd))
    }
}

impl Ai {
    pub fn ammo_on(&self) -> bool {
        !self.dev_off(FEATURE)
    }

    /// A hit on our unit `victim` (UnitDamaged).
    pub fn ammo_hit(&mut self, victim: Unit, attacker: Option<Unit>, dir: Vec3, weapon: Option<springai::WeaponDef>) {
        let Some(wd) = weapon else { return };
        let Some(def) = self.ammo.tube_def(wd) else { return };
        let frame = self.frame;
        self.ammo.stats.hits += 1;
        let at = victim.pos();
        let name = wd.name();
        let (shooter, how) = match attacker {
            Some(a) if a.ally_team() == self.ally_team_id => {
                self.ammo.stats.allied += 1;
                return;
            }
            Some(a) => (Some((a.id(), a.pos())), "id"),
            None => {
                let reach = wd.range() * RANGE_SLACK + RANGE_PAD;
                let candidates = (self.war.targets.iter())
                    .filter(|(_, e)| {
                        !e.is_dead
                            && !e.def_name.is_empty()
                            && name.len() > e.def_name.len()
                            && name.starts_with(e.def_name.as_str())
                            && name.as_bytes()[e.def_name.len()] == b'_'
                    })
                    .map(|(&id, e)| (id, e.position));
                let id = by_bearing(at, dir, reach, candidates);
                (id.and_then(|id| self.war.targets.get(&id).map(|e| (id, e.position))), "bearing")
            }
        };
        let Some((sid, spos)) = shooter else {
            self.ammo.stats.unnamed += 1;
            return;
        };
        if how == "id" {
            self.ammo.stats.named += 1;
        } else {
            self.ammo.stats.bearing += 1;
        }
        let dist = crate::util::distance(at, spos);
        let fired = frame - def.flight(dist);
        let clock = self.ammo.clocks.entry(sid).or_insert_with(|| Clock::new(name.clone(), def));
        let shot = clock.shot(frame, fired);
        let (loaded, ready) = (clock.loaded(frame), clock.empty(frame, 0));
        match shot {
            Shot::Same => {
                self.ammo.stats.same += 1;
                return;
            }
            Shot::Counted => self.ammo.stats.counted += 1,
            Shot::Surprise => self.ammo.stats.surprises += 1,
        }
        self.op_emit(
            "ammo",
            json!({
                "event": "shot", "f": frame, "id": sid, "weapon": name, "how": how, "victim": victim.id(),
                "vdef": self.defs.name_of(victim),
                "dist": dist.round(), "fired": fired, "loaded": loaded, "ready": ready,
                "surprise": shot == Shot::Surprise,
            }),
        );
    }

    pub fn ammo_enemy_destroyed(&mut self, id: i32) {
        self.ammo.clocks.remove(&id);
    }

    /// Stun and slow, our own units' list, samples and stats.
    pub fn ammo_update(&mut self, frame: i32) {
        if frame % SAMPLE != 0 {
            return;
        }
        if self.ammo.own_defs.is_none() {
            let mut m = HashMap::new();
            for d in self.defs.all() {
                let tube =
                    (d.weapon_mounts().iter()).filter_map(|w| w.weapon_def()).find_map(|wd| self.ammo.tube_def(wd));
                if let Some(t) = tube {
                    m.insert(d.id(), t);
                }
            }
            self.ammo.own_defs = Some(m);
        }
        if frame - self.ammo.own_refreshed >= OWN_REFRESH {
            self.ammo.own_refreshed = frame;
            let defs = self.ammo.own_defs.as_ref();
            self.ammo.own = (self.cb.team_units().into_iter())
                .filter(|u| u.def().is_some_and(|d| defs.is_some_and(|m| m.contains_key(&d.id()))))
                .map(|u| u.id())
                .collect();
        }
        let targets = &self.war.targets;
        self.ammo.clocks.retain(|id, _| targets.get(id).is_some_and(|e| !e.is_dead));
        let mut lines = Vec::new();
        for (&id, c) in self.ammo.clocks.iter_mut() {
            if c.loaded(frame) == c.ready.len() {
                continue;
            }
            if let Some(e) = self.war.targets.get(&id).filter(|e| e.visible) {
                let mult =
                    if e.unit.is_paralyzed() { 0.0 } else { e.unit.rules_param_float("totalReloadSpeedChange", 1.0) };
                c.slowed(frame, SAMPLE, mult);
            }
            lines.push(
                json!({"event": "est", "f": frame, "id": id, "loaded": c.loaded(frame), "ready": c.empty(frame, 0)}),
            );
        }
        let own: Vec<i32> = self.ammo.own.iter().copied().collect();
        for id in own {
            let Some(u) = Unit::from_id(self.cb, id).filter(|u| u.health() > 0.0) else {
                self.ammo.own.remove(&id);
                continue;
            };
            let (loaded, ready) = self.ammo_own_tubes(u);
            if !ready.is_empty() {
                lines.push(json!({"event": "own", "f": frame, "id": id, "loaded": loaded, "ready": ready}));
            }
        }
        for l in lines {
            self.op_emit("ammo", l);
        }
        self.ammo_journal_open(frame);
        if frame - self.ammo.last_report >= REPORT_PERIOD {
            self.ammo.last_report = frame;
            let s = self.ammo.stats;
            self.op_emit(
                "ammo",
                json!({
                    "event": "stats", "t": frame / 30, "on": self.ammo_on(), "hits": s.hits, "named": s.named,
                    "bearing": s.bearing, "unnamed": s.unnamed, "allied": s.allied, "same": s.same,
                    "counted": s.counted, "surprises": s.surprises, "clocks": self.ammo.clocks.len(),
                    "own": self.ammo.own.len(),
                }),
            );
        }
    }

    /// Each window an enemy tube weapon has every tube empty, once: who of
    /// ours stands within reach of it, and in what state (for the A/B's
    /// chances: was a raider there, free, alone?).
    fn ammo_journal_open(&mut self, frame: i32) {
        let opened = &mut self.ammo.opened;
        opened.retain(|_, end| *end > frame);
        let fresh: Vec<(i32, i32, String)> = (self.ammo.clocks.iter())
            .filter_map(|(&id, c)| {
                let open = c.open_for(frame, 0)?;
                (!opened.contains_key(&id)).then(|| (id, open, c.weapon.clone()))
            })
            .collect();
        for (id, open, weapon) in fresh {
            self.ammo.opened.insert(id, frame + open);
            let Some(pos) = self.war.targets.get(&id).map(|e| e.position) else { continue };
            let h = &self.raider_handler;
            let near: Vec<serde_json::Value> = (self.cb.friendly_units_in(pos, super::fightsim::local::CONTACT).into_iter())
                .filter(|u| u.team() == self.team_id && u.max_speed() > 0.0 && u.max_range() > 0.0)
                .filter(|u| !u.def().is_some_and(|d| d.is_able_to_fly()))
                .map(|u| {
                    let uid = u.id();
                    let role = if h.kodachis.contains(&uid) {
                        "kodachi"
                    } else if h.solo_raiders.contains(&uid) {
                        "solo"
                    } else if h.small_raiders.contains(&uid) || h.medium_raiders.contains(&uid) {
                        "squadraider"
                    } else {
                        "other"
                    };
                    json!({
                        "id": uid, "def": self.defs.name_of(u), "hp": (u.health() / u.max_health().max(1.0) * 100.0).round() / 100.0,
                        "d": crate::util::distance(u.pos(), pos).round(), "role": role,
                        "retreating": self.retreat_handler.is_retreating(uid), "gang": self.coalesce_owns(uid),
                        "precision": self.precision.missions.contains_key(&uid),
                    })
                })
                .collect();
            self.op_emit(
                "ammo",
                json!({"event": "open", "f": frame, "id": id, "weapon": weapon, "frames": open, "near": near}),
            );
        }
    }

    /// Our own unit `u`'s tubes: (loaded, frames until each empty one is
    /// loaded). The first comes back at `scriptReloadFrame`; the others,
    /// fired after it, a tube spacing apart (at the soonest).
    fn ammo_own_tubes(&self, u: Unit) -> (usize, Vec<i32>) {
        let Some(t) = u.def().and_then(|d| self.ammo.own_defs.as_ref()?.get(&d.id()).copied()) else {
            return (0, Vec::new());
        };
        let loaded = (u.rules_param_float("scriptLoaded", t.tubes as f32).max(0.0) as usize).min(t.tubes);
        if loaded == t.tubes {
            return (loaded, Vec::new());
        }
        let first = u.rules_param_float("scriptReloadFrame", self.frame as f32) as i32 - self.frame;
        (loaded, (0..(t.tubes - loaded) as i32).map(|i| (first + i * t.spacing).max(1)).collect())
    }

    /// Unit `id`'s empty tubes for a fight snapshot ([`SnapUnit::tubes`]):
    /// our own as they read, the enemy's from the ledger less [`MARGIN`];
    /// none with `ammo` off.
    ///
    /// [`SnapUnit::tubes`]: crate::military::fightsim::SnapUnit::tubes
    pub(crate) fn snap_tubes(&self, id: i32) -> Vec<i32> {
        if !self.ammo_on() {
            return Vec::new();
        }
        if let Some(c) = self.ammo.clocks.get(&id) {
            return c.empty(self.frame, MARGIN);
        }
        if self.ammo.own.contains(&id)
            && let Some(u) = Unit::from_id(self.cb, id)
        {
            return self.ammo_own_tubes(u).1;
        }
        Vec::new()
    }

    /// Whether an enemy within `radius` of `pos` has every tube empty for
    /// at least `frames` more (the ledger, less [`MARGIN`]); never with
    /// `ammo` off.
    pub fn ammo_open_near(&self, pos: Vec3, radius: f32, frames: i32) -> bool {
        self.ammo_on()
            && self.ammo.clocks.iter().any(|(id, c)| {
                c.open_for(self.frame, MARGIN).is_some_and(|f| f >= frames)
                    && self.war.targets.get(id).is_some_and(|e| crate::util::distance(e.position, pos) <= radius)
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn picket() -> TubeDef {
        let p = vec![("script_reload".to_owned(), "12.5".to_owned()), ("script_burst".to_owned(), "3".to_owned())];
        TubeDef::from_params(1.2, &p, 10.0, 20.0).unwrap()
    }

    #[test]
    fn tube_defs_come_from_the_script_params() {
        let t = picket();
        assert_eq!((t.tubes, t.reload, t.spacing), (3, 375, 36));
        assert_eq!(t.flight(450.0), 30);
        // No script_reload: an ordinary weapon.
        assert!(TubeDef::from_params(1.2, &[], 10.0, 20.0).is_none());
        // No burst given: one tube (Disco Rave Party).
        let p = vec![("script_reload".to_owned(), "6".to_owned())];
        assert_eq!(TubeDef::from_params(1.0, &p, 0.0, 0.0).map(|t| (t.tubes, t.flight(500.0))), Some((1, 0)));
    }

    #[test]
    fn a_picket_that_fired_three_times_is_empty_until_its_first_tube_is_back() {
        let mut c = Clock::new("turretmissile_armrl_missile".into(), picket());
        assert_eq!(c.loaded(0), 3);
        assert_eq!(c.open_for(0, 0), None);
        // Three hits 1.2 s apart, each after 20 frames of flight.
        for (i, hit) in [100, 136, 172].into_iter().enumerate() {
            assert_eq!(c.shot(hit, hit - 20), Shot::Counted, "shot {i}");
        }
        assert_eq!(c.loaded(172), 0);
        // Fired at 80, 116, 152: back at 455, 491, 527.
        assert_eq!(c.empty(172, 0), vec![283, 319, 355]);
        assert_eq!(c.open_for(172, 0), Some(283));
        assert_eq!(c.open_for(172, MARGIN), Some(283 - MARGIN));
        // Splash on a second unit the same frame is the same shot.
        assert_eq!(c.shot(173, 153), Shot::Same);
        assert_eq!(c.shots, 3);
        // The first tube is back: one loaded.
        assert_eq!(c.loaded(455), 1);
        assert_eq!(c.open_for(455, 0), None);
        // A shot while the clock had every tube empty: believed, flagged.
        let mut d = c.clone();
        assert_eq!(d.shot(300, 280), Shot::Surprise);
        assert_eq!(d.empty(300, 0), vec![191, 227, 355]);
    }

    #[test]
    fn stun_and_slow_hold_the_empty_tubes_back() {
        let mut c = Clock::new("turretmissile_armrl_missile".into(), picket());
        c.shot(100, 100);
        c.slowed(115, 15, 0.0); // stunned for 15 frames
        assert_eq!(c.empty(115, 0), vec![375]);
        c.slowed(130, 15, 0.5); // half speed
        assert_eq!(c.empty(130, 0), vec![368]);
        c.slowed(145, 15, 1.0);
        assert_eq!(c.empty(145, 0), vec![353]);
        // Loaded tubes do not move.
        assert_eq!(c.loaded(145), 2);
    }

    #[test]
    fn the_shooter_by_bearing_is_the_best_aligned_in_reach() {
        let at = Vec3::xz(0.0, 0.0);
        let east = Vec3::new(1.0, 0.1, 0.0);
        let cands = [
            (1, Vec3::xz(500.0, 60.0)),
            (2, Vec3::xz(400.0, -10.0)),
            (3, Vec3::xz(0.0, 500.0)),
            (4, Vec3::xz(900.0, 0.0)),
        ];
        // 2 is on the bearing, 1 is 7 degrees off, 3 is north, 4 out of reach.
        assert_eq!(by_bearing(at, east, 700.0, cands.into_iter()), Some(2));
        assert_eq!(by_bearing(at, east, 700.0, cands.into_iter().filter(|c| c.0 != 2)), Some(1));
        assert_eq!(by_bearing(at, Vec3::new(-1.0, 0.0, 0.0), 700.0, cands.into_iter()), None);
        // No bearing (no attacker at all): nobody.
        assert_eq!(by_bearing(at, Vec3::new(0.0, 1.0, 0.0), 700.0, cands.into_iter()), None);
    }
}
