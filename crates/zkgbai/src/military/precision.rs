//! Precision raids: raiders that shoot from where enemy defenses cannot
//! shoot back.
//!
//! A threat map of its own, apart from the war's (which paints every
//! defense as a disc of its range): for each weapon of each enemy defense
//! we know of, a [`Shadow`] — where a unit's aim point is out of its line of
//! fire, behind hills or behind the enemy's own buildings (see
//! `kgbutil::line_of_fire`). Over it, [`Precision::firing_spots`] finds where
//! a raider can hit an enemy building without being hit by any defense, and
//! [`Precision::approach`] plans the way in under as little fire as can be.
//! Raiders on precision missions use it (see `handlers::precision`); the
//! rest of the AI does not. Buildings cast shadows too: see
//! [`BUILDINGS_SHADOW`].

use std::collections::{BTreeMap, BTreeSet, BinaryHeap};
use std::sync::Arc;

use springai::{UnitDef, Vec3, WeaponDef};

use crate::Ai;
use crate::kgbutil::line_of_fire::CELL;
use crate::kgbutil::worker::Worker;
use crate::kgbutil::{FireGround, Flight, Gun, Shadow, Shape, Volume};
use crate::military::handlers::precision::{Mission, MissionStats};
use crate::military::wrappers::weapon_power;

/// Share of a defense's model height that its weapons fire from, at the
/// least; and how far above its aim point (ZK's `aimposoffset` over its
/// middle: where the turret head is) they are taken to fire from. Staged
/// probes (see `shadow_probe`) put the Lotus's muzzle at least 61 up (its
/// aim point is 52), the Stinger's 83 (86), the Gauss's below 54 (51).
pub const MUZZLE_SHARE: f32 = 0.75;
const MUZZLE_OVER_AIM: f32 = 10.0;
/// Whether buildings shadow defenses at all. Staged probes found turrets
/// firing past their own solars that footprint-by-model-height prisms said
/// they could not: the engine tests collision volumes, which the AI
/// interface does not give, so they are read from the unit definitions
/// (see [`parse_volume`]).
const BUILDINGS_SHADOW: bool = true;
/// ZK's nanoframes (`unit_centeroffset`): the collision volume grows from
/// nothing at this much metal in to full height at this share built.
const GROW_FROM_METAL: f32 = 15.0;
const FULL_GROW: f32 = 0.5;
/// A unit counts as hidden from a gun only with its aim point this far
/// below the gun's line of fire, there and this far around (where it may
/// end up standing, and what the shadow cells blur).
pub const HIDE_MARGIN: f32 = 6.0;
/// ...and this far on along each defense's line of fire, and this far to
/// either side of it (the shadow behind a single tower is a narrow cone:
/// slipping along it is safe, across it is not; raiders park within a few
/// elmos when told to, see `handlers::precision`).
const HIDE_ALONG: f32 = 20.0;
const HIDE_ACROSS: f32 = 6.0;
/// Elmos added to defense ranges.
pub const RANGE_MARGIN: f32 = 24.0;
/// Firing spots stay this far inside our raiders' range (they park only
/// roughly where sent), or this share of it for short-ranged ones.
const SPOT_SLACK: f32 = 40.0;
const SPOT_SLACK_SHARE: f32 = 0.2;
/// Raiders keep this far off buildings' footprints.
const UNIT_CLEARANCE: f32 = 16.0;
/// How often the model catches up with what we know, re-reads the terrain,
/// and reports.
const SYNC_FRAMES: i32 = 30;
const TERRAIN_FRAMES: i32 = 30 * 60;
const REPORT_FRAMES: i32 = 30 * 60;
/// Half the side of the window an approach is planned in, and its cells.
const APPROACH_WINDOW: f32 = 1200.0;
const APPROACH_CELL: f32 = 16.0;
/// Firing spots are looked for this far apart.
const SPOT_STEP: f32 = 8.0;
/// How much time under fire counts against an approach: per second losing
/// all of the raider's health, this many seconds more.
const EXPOSURE_WEIGHT: f32 = 30.0;
/// The engine's `Collision::NOGROUND` bit: shots that pass through terrain.
const NO_GROUND: i32 = 1 << 6;
/// Our raider types, for the report.
const RAIDERS: [&str; 7] = ["cloakraid", "shieldraid", "hoverraid", "vehraid", "amphraid", "jumpraid", "tankraid"];

/// One weapon of an enemy defense, and where it hits.
struct Arm {
    gun: Gun,
    shadow: Option<Shadow>,
    /// Where it hits as far as the terrain goes (buildings aside).
    bare: Option<Shadow>,
    dps: f32,
    /// Torpedoes: hits only what is on or in the water.
    water_only: bool,
}

/// An enemy defense in the model.
struct Armed {
    pos: Vec3,
    name: String,
    arms: Vec<Arm>,
    /// Shadows to recompute: the ground around it changed.
    stale: bool,
    revision: u64,
}

/// An enemy building as the model takes it in.
#[derive(Clone, Copy)]
pub struct Standing {
    pub id: i32,
    pub def: UnitDef,
    pub pos: Vec3,
    /// A defense (with weapons that may fire at our units).
    pub armed: bool,
    /// Finished (a frame does not fire).
    pub ready: bool,
    /// Its collision volume as it is (a nanoframe's is squashed), if it
    /// has grown any.
    pub volume: Option<Volume>,
    /// Half its footprint along x and z.
    pub foot: (f32, f32),
}

/// A unit type's collision volume, relative to its middle.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct VolumeDef {
    pub scales: [f32; 3],
    pub offsets: [f32; 3],
    pub shape: Shape,
    pub sphere: bool,
}

impl VolumeDef {
    /// The engine's default: a sphere of the model's radius.
    pub fn sphere(radius: f32) -> Self {
        let d = 2.0 * radius;
        VolumeDef { scales: [d; 3], offsets: [0.0; 3], shape: Shape::Ellipsoid, sphere: true }
    }

    /// The volume of a unit of this type with its middle at `mid`, turned
    /// to `facing` (1 and 3 swap x and z), on ground at height `ground`,
    /// `grow` of the way grown (ZK's nanoframes: the top sinks, the bottom
    /// stays); `None` if it has not grown at all.
    pub fn at(&self, mid: Vec3, facing: i32, ground: f32, grow: f32) -> Option<Volume> {
        let (mut sx, sy, mut sz) = (self.scales[0], self.scales[1], self.scales[2]);
        let (mut ox, oy, mut oz) = (self.offsets[0], self.offsets[1], self.offsets[2]);
        if facing % 2 == 1 {
            std::mem::swap(&mut sx, &mut sz);
            std::mem::swap(&mut ox, &mut oz);
        }
        let (mut y, mut hy) = (mid.y + oy, sy / 2.0);
        if grow < 1.0 {
            if grow <= 0.0 {
                return None;
            }
            let below = (ground - (y - hy)).max(0.0);
            let off = (sy - below - if self.sphere { 8.0 } else { 2.0 }).max(0.0) * (1.0 - grow);
            if self.sphere {
                y -= off;
            } else {
                hy -= off / 2.0;
                y -= off / 2.0;
            }
        }
        let shape = match (self.shape, facing % 2 == 1) {
            (Shape::CylinderX, true) => Shape::CylinderZ,
            (Shape::CylinderZ, true) => Shape::CylinderX,
            (s, _) => s,
        };
        Some(Volume { x: mid.x + ox, y, z: mid.z + oz, hx: sx / 2.0, hy, hz: sz / 2.0, shape })
    }
}

/// The collision volume a unit definition file (`units/<name>.lua`)
/// declares for the unit itself (not its wrecks): `None` if it declares
/// none (the engine then uses its default sphere).
pub fn parse_volume(text: &str) -> Option<VolumeDef> {
    let lower = text.to_ascii_lowercase();
    let unit = lower.split("featuredefs").next().unwrap_or(&lower);
    let field = |key: &str| -> Option<&str> {
        let at = unit.find(key)? + key.len();
        let rest = unit[at..].trim_start().strip_prefix('=')?.trim_start();
        let rest = rest.strip_prefix("[[").or_else(|| rest.strip_prefix('"'))?;
        let end = rest.find("]]").or_else(|| rest.find('"'))?;
        Some(rest[..end].trim())
    };
    let triple = |v: &str| -> Option<[f32; 3]> {
        let n: Vec<f32> = v.split_whitespace().filter_map(|t| t.parse().ok()).collect();
        (n.len() == 3).then(|| [n[0], n[1], n[2]])
    };
    let mut scales = triple(field("collisionvolumescales")?)?;
    let offsets = field("collisionvolumeoffsets").and_then(triple).unwrap_or([0.0; 3]);
    let kind = field("collisionvolumetype").unwrap_or("ellipsoid");
    let (shape, sphere) = if kind.starts_with("sph") {
        let d = scales.iter().copied().fold(0.0, f32::max);
        scales = [d; 3];
        (Shape::Ellipsoid, true)
    } else if kind.starts_with("box") {
        (Shape::Box, false)
    } else if kind.starts_with("cyl") {
        let shape = match kind.chars().nth(3) {
            Some('x') => Shape::CylinderX,
            Some('y') => Shape::CylinderY,
            _ => Shape::CylinderZ,
        };
        (shape, false)
    } else {
        (Shape::Ellipsoid, false)
    };
    Some(VolumeDef { scales, offsets, shape, sphere })
}

/// How far a ZK nanoframe of a unit costing `cost` metal, `progress` built,
/// has grown (see [`GROW_FROM_METAL`]).
pub fn nanoframe_growth(progress: f32, cost: f32) -> f32 {
    ((progress * cost - GROW_FROM_METAL) / (cost * FULL_GROW).max(1.0)).clamp(0.0, 1.0)
}

/// A building's footprint and collision volume.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Footing {
    x: f32,
    z: f32,
    hx: f32,
    hz: f32,
    volume: Option<Volume>,
}

/// What a raider of ours can do, for precision missions.
#[derive(Clone, Copy, Debug)]
pub struct Shooter {
    pub range: f32,
    /// Its aim point above the ground: where enemy guns shoot at it.
    pub aim: f32,
    /// Where its own shots leave from, above the ground.
    pub muzzle: f32,
    /// Ground this close to its target does not stop its shots (any, for
    /// shots that go through terrain).
    pub aoe: f32,
    /// Its weapon's `heightMod` (height differences count against range).
    pub height_mod: f32,
    pub max_slope: f32,
    /// Deepest water it goes into; any for hovers.
    pub max_depth: f32,
    /// Elmos per second.
    pub speed: f32,
    pub health: f32,
    pub dps: f32,
    /// For a cloaked raider, how near enemies reveal it (0: never
    /// cloaked): out of their reach, no defense can see it to fire.
    pub cloak: f32,
}

/// The way into a firing spot.
#[derive(Clone, Debug)]
pub struct Approach {
    /// Where it leaves ground the defenses do not cover (or the raider's
    /// own position, if it is already in the spot's window and unexposed).
    pub entry: Vec3,
    /// Waypoints from the entry to the spot.
    pub path: Vec<Vec3>,
    /// Expected damage taken on the way in.
    pub damage: f32,
    /// Seconds from the entry to the spot.
    pub time: f32,
}

#[derive(Default)]
pub struct Precision {
    ground: Option<Arc<FireGround>>,
    revision: u64,
    worker: Option<Worker<ShadowJob, ShadowResult>>,
    corners: Vec<f32>,
    guns: BTreeMap<i32, Armed>,
    blocks: BTreeMap<i32, Footing>,
    /// Collision volumes by unit type id.
    pub(crate) volume_defs: BTreeMap<i32, VolumeDef>,
    /// Defenses that hit us where we thought them blind: they get no
    /// shadows from then on.
    pub distrusted: BTreeSet<i32>,
    last_terrain: i32,
    last_report: i32,
    /// Raiders on precision missions, by unit id (see
    /// `handlers::precision`).
    pub missions: BTreeMap<i32, Mission>,
    /// When each raider last looked for a mission.
    pub planned: BTreeMap<i32, i32>,
    pub stats: MissionStats,
    /// Spots missions gave up on (unreachable, or not hidden after all),
    /// and when.
    pub bad_spots: Vec<(Vec3, i32)>,
    /// Targets a raider gave up on: (raider, target, until frame).
    pub banned: Vec<(i32, i32, i32)>,
    /// The staged calibration (see `shadow_probe`).
    pub(crate) probing: super::shadow_probe::Probing,
}

impl Shooter {
    /// A unit of type `def` with its aim point `aim` and its muzzle
    /// `muzzle` above the ground.
    pub fn of(def: UnitDef, aim: f32, muzzle: f32) -> Option<Shooter> {
        let md = def.move_data()?;
        let wd: WeaponDef = def
            .weapon_mounts()
            .into_iter()
            .filter_map(|m| m.weapon_def())
            .find(|w| !w.name().contains("fake") && !w.is_shield())?;
        let hover = def.is_able_to_hover() || def.is_floater();
        // Flames (the Pyro's) go through hills: no ground stops its shots.
        let through_ground = wd.collision_flags() & NO_GROUND != 0;
        Some(Shooter {
            range: wd.range(),
            aim,
            muzzle,
            aoe: if through_ground { f32::INFINITY } else { wd.area_of_effect() },
            height_mod: wd.height_mod(),
            max_slope: md.max_slope(),
            max_depth: if hover { f32::INFINITY } else { def.max_water_depth() },
            speed: def.speed(),
            health: def.health(),
            dps: weapon_power(wd, false),
            cloak: if def.is_able_to_cloak() && def.is_start_cloaked() { def.decloak_distance().max(1.0) } else { 0.0 },
        })
    }
}

/// ZK's `aimposoffset` of a unit type: where others aim at it, over its
/// middle (for a turret, about where its head is).
fn aim_offset(def: UnitDef) -> f32 {
    def.custom_param("aimposoffset")
        .and_then(|v| v.split_whitespace().nth(1).and_then(|y| y.parse().ok()))
        .unwrap_or(0.0)
}

/// The weapons of a defense of type `def` standing at `pos` (its middle)
/// on `ground`.
fn arm_specs_of(ground: &FireGround, def: UnitDef, pos: Vec3, gravity: f32) -> Vec<Arm> {
    let base = ground.ground(pos.x, pos.z).max(0.0);
    let muzzle = (base + MUZZLE_SHARE * def.height()).max(pos.y + aim_offset(def) + MUZZLE_OVER_AIM);
    let body = (def.x_size().max(def.z_size()) as f32) * 4.0;
    let lob_unit = def.high_trajectory_type() != 0;
    let mut arms = Vec::new();
    for wd in def.weapon_mounts().into_iter().filter_map(|m| m.weapon_def()) {
        let name = wd.name();
        if name.contains("fake") || wd.is_shield() || wd.range() <= 0.0 {
            continue;
        }
        let kind = wd.weapon_type();
        let flight = match kind.as_str() {
            _ if lob_unit || wd.high_trajectory() == 1 => Flight::Lob,
            "StarburstLauncher" | "TorpedoLauncher" | "AircraftBomb" => Flight::Lob,
            "MissileLauncher" if wd.trajectory_height() > 0.0 => Flight::Lob,
            "Cannon" => {
                // Elmos per frame, as the engine keeps it.
                let speed = wd.projectile_speed();
                Flight::Arc { speed: if speed < 100.0 { speed * 30.0 } else { speed }, gravity }
            }
            _ => Flight::Line,
        };
        let gun = Gun {
            x: pos.x,
            z: pos.z,
            muzzle,
            range: wd.range(),
            height_mod: wd.height_mod(),
            aoe: wd.area_of_effect(),
            body,
            flight,
            avoids_friends: wd.is_avoid_friendly(),
            through_ground: wd.collision_flags() & NO_GROUND != 0,
        };
        let (shadow, bare) = (None, None);
        arms.push(Arm { gun, shadow, bare, dps: weapon_power(wd, false), water_only: kind == "TorpedoLauncher" });
    }
    arms
}

// No UnitDef or other callback handle is captured by this job.
type ShadowResult = Vec<(i32, u64, Vec<Arm>)>;
type ShadowJob = (Arc<FireGround>, ShadowResult);
fn cast_shadows((ground, mut guns): ShadowJob) -> ShadowResult {
    for (_, _, arms) in &mut guns {
        for a in arms {
            a.shadow = Some(Shadow::compute(&ground, &a.gun, RANGE_MARGIN + CELL));
            a.bare = Some(Shadow::compute(&ground, &Gun { avoids_friends: false, ..a.gun }, RANGE_MARGIN + CELL));
        }
    }
    guns
}

impl Arm {
    fn need_at(&self, x: f32, z: f32) -> f32 {
        self.shadow.as_ref().map_or(f32::NEG_INFINITY, |s| s.need_at(x, z))
    }
    fn bare_need_at(&self, x: f32, z: f32) -> f32 {
        self.bare.as_ref().map_or(f32::NEG_INFINITY, |s| s.need_at(x, z))
    }
}

impl Precision {
    pub fn ground(&self) -> Option<&FireGround> {
        self.ground.as_deref()
    }

    /// Bring the model up to date with the enemy buildings we know of.
    pub fn sync(&mut self, standing: &[Standing], gravity: f32) {
        self.sync_model(standing, gravity, false);
    }

    fn sync_model(&mut self, standing: &[Standing], gravity: f32, asynchronous: bool) {
        self.revision += 1;
        let Some(ground) = self.ground.as_mut() else { return };
        // Buildings: re-stand them all when any came, went or moved, and
        // recompute the shadows of the defenses near those.
        let blocks: BTreeMap<i32, Footing> = (standing.iter())
            .map(|s| (s.id, Footing { x: s.pos.x, z: s.pos.z, hx: s.foot.0, hz: s.foot.1, volume: s.volume }))
            .collect();
        let mut changed: Vec<Footing> = Vec::new();
        for (id, b) in &blocks {
            if self.blocks.get(id) != Some(b) {
                changed.push(*b);
                if let Some(old) = self.blocks.get(id) {
                    changed.push(*old);
                }
            }
        }
        changed.extend(self.blocks.iter().filter(|(id, _)| !blocks.contains_key(id)).map(|(_, b)| *b));
        if !changed.is_empty() {
            // Footprints always (raiders cannot stand on them); lines of
            // fire only see them if buildings shadow.
            if BUILDINGS_SHADOW {
                Arc::make_mut(ground).set_volumes(blocks.values().filter_map(|b| b.volume));
            }
            self.blocks = blocks;
            for g in self.guns.values_mut() {
                let reach = g.arms.iter().map(|a| a.gun.range).fold(0.0, f32::max) + RANGE_MARGIN + 64.0;
                if changed.iter().any(|b| (b.x - g.pos.x).hypot(b.z - g.pos.z) < reach + b.hx.hypot(b.hz)) {
                    g.stale = true;
                    g.revision = self.revision;
                }
            }
        }

        // Defenses: new ones in, gone ones out.
        let armed: BTreeMap<i32, &Standing> =
            standing.iter().filter(|s| s.armed && s.ready).map(|s| (s.id, s)).collect();
        self.guns.retain(|id, g| armed.get(id).is_some_and(|s| s.pos == g.pos));
        for (id, s) in armed {
            self.guns.entry(id).or_insert_with(|| Armed {
                pos: s.pos,
                name: s.def.name(),
                arms: Vec::new(),
                stale: true,
                revision: self.revision,
            });
            if let Some(g) = self.guns.get_mut(&id).filter(|g| g.stale) {
                g.arms = arm_specs_of(ground, s.def, s.pos, gravity);
            }
        }
        // Validate after updating invalidations, never before. A removed or
        // changed gun cannot acquire shadows from an obsolete snapshot.
        self.poll_shadows();
        if asynchronous && self.worker.as_ref().is_some_and(|w| w.busy()) {
            return;
        }
        let pending: Vec<_> = self
            .guns
            .iter()
            .filter(|(_, g)| g.stale)
            .map(|(&id, g)| {
                let arms = g
                    .arms
                    .iter()
                    .map(|a| Arm { gun: a.gun, dps: a.dps, water_only: a.water_only, shadow: None, bare: None })
                    .collect();
                (id, g.revision, arms)
            })
            .collect();
        if pending.is_empty() {
            return;
        }
        let job = (self.ground.as_ref().unwrap().clone(), pending);
        if asynchronous {
            self.worker.get_or_insert_with(|| Worker::new("precision-shadows", cast_shadows)).submit(job);
        } else {
            self.accept_shadows(cast_shadows(job));
        }
    }

    fn accept_shadows(&mut self, result: ShadowResult) {
        for (id, revision, arms) in result {
            if let Some(g) = self.guns.get_mut(&id).filter(|g| g.revision == revision) {
                g.arms = arms;
                g.stale = false;
            }
        }
    }

    fn poll_shadows(&mut self) {
        if let Some((Some(result), _elapsed)) = self.worker.as_mut().and_then(Worker::poll) {
            self.accept_shadows(result);
        }
    }

    /// Take in the terrain as it is now (corner heights); recompute the
    /// shadows of the defenses whose ground changed.
    pub fn set_terrain(&mut self, corners: Vec<f32>, w: i32, h: i32) {
        self.revision += 1;
        let Some(ground) = self.ground.as_mut() else {
            self.ground = Some(Arc::new(FireGround::new(corners.clone(), w, h)));
            self.corners = corners;
            return;
        };
        if corners.len() != self.corners.len() {
            return;
        }
        let row = (w + 1) as usize;
        let changed: Vec<(f32, f32)> = (corners.iter().zip(&self.corners).enumerate())
            .filter(|(_, (a, b))| (*a - *b).abs() > 1.0)
            .map(|(i, _)| ((i % row) as f32 * 8.0, (i / row) as f32 * 8.0))
            .collect();
        if changed.is_empty() {
            return;
        }
        Arc::make_mut(ground).set_terrain(corners.clone());
        self.corners = corners;
        for g in self.guns.values_mut() {
            let reach = g.arms.iter().map(|a| a.gun.range).fold(0.0, f32::max) + RANGE_MARGIN + 64.0;
            if changed.len() > 5000 || changed.iter().any(|&(x, z)| (x - g.pos.x).hypot(z - g.pos.z) < reach) {
                g.stale = true;
                g.revision = self.revision;
            }
        }
    }

    /// Damage per second the defenses we know of would deal to a unit at
    /// `(x, z)` with its aim point `aim` above the ground there.
    pub fn exposure(&self, x: f32, z: f32, aim: f32) -> f32 {
        let Some(ground) = self.ground.as_ref() else { return 0.0 };
        let (terrain, stand) = (ground.ground(x, z), ground.stand(x, z));
        let mut dps = 0.0;
        for (id, g) in &self.guns {
            for a in &g.arms {
                if (a.water_only && terrain >= 0.0) || !a.gun.reaches(x, z, stand + aim, RANGE_MARGIN) {
                    continue;
                }
                if g.stale || self.distrusted.contains(id) || a.need_at(x, z) <= aim + HIDE_MARGIN {
                    dps += a.dps;
                }
            }
        }
        dps
    }

    /// Whether a unit with its aim point `aim` up is hidden from every
    /// defense we know of at `(x, z)` and around it.
    pub fn hidden_at(&self, x: f32, z: f32, aim: f32) -> bool {
        self.exposure_around(x, z, aim) == 0.0
    }

    /// The damage per second a unit with its aim point `aim` up would take
    /// at `(x, z)` or near it (where it may end up standing): from each
    /// defense that hits it there, or [`HIDE_ALONG`] on or back along its
    /// line of fire, or [`HIDE_ACROSS`] to either side.
    pub fn exposure_around(&self, x: f32, z: f32, aim: f32) -> f32 {
        let Some(ground) = self.ground.as_ref() else { return 0.0 };
        let terrain = ground.ground(x, z);
        let mut dps = 0.0;
        for (id, g) in &self.guns {
            let (dx, dz) = (x - g.pos.x, z - g.pos.z);
            let d = dx.hypot(dz).max(1.0);
            let (ax, az) = (dx / d, dz / d);
            let points = [(0.0, 0.0), (HIDE_ALONG, 0.0), (-HIDE_ALONG, 0.0), (0.0, HIDE_ACROSS), (0.0, -HIDE_ACROSS)];
            for a in &g.arms {
                if a.water_only && terrain >= 0.0 {
                    continue;
                }
                let hits = points.iter().any(|&(along, across)| {
                    let (px, pz) = (x + ax * along - az * across, z + az * along + ax * across);
                    let stand = ground.stand(px, pz);
                    a.gun.reaches(px, pz, stand + aim, RANGE_MARGIN)
                        && (g.stale || self.distrusted.contains(id) || a.need_at(px, pz) <= aim + HIDE_MARGIN)
                });
                if hits {
                    dps += a.dps;
                }
            }
        }
        dps
    }

    /// Whether a unit like `sh` can stand at `(x, z)`: not too steep, not
    /// too deep, not on a building.
    fn passable(&self, ground: &FireGround, x: f32, z: f32, sh: &Shooter) -> bool {
        let (w, h) = ground.size();
        if x < 8.0 || z < 8.0 || x > w - 8.0 || z > h - 8.0 {
            return false;
        }
        let terrain = ground.ground(x, z);
        if terrain < -sh.max_depth {
            return false;
        }
        let (gx, gz) = ((ground.ground(x + 8.0, z) - ground.ground(x - 8.0, z)) / 16.0, {
            (ground.ground(x, z + 8.0) - ground.ground(x, z - 8.0)) / 16.0
        });
        let slope = 1.0 - 1.0 / (1.0 + gx * gx + gz * gz).sqrt();
        (slope <= sh.max_slope || (terrain < 0.0 && sh.max_depth.is_infinite()))
            && self
                .blocks
                .values()
                .all(|b| (b.x - x).abs() > b.hx + UNIT_CLEARANCE || (b.z - z).abs() > b.hz + UNIT_CLEARANCE)
    }

    /// Where a raider like `sh` can hit the building at `target` (its aim
    /// point at absolute height `target_aim`, `half` elmos across its
    /// footprint) without any defense we know of hitting it: within its
    /// range, with a free line to the target, on ground it can stand on.
    pub fn firing_spots(&self, target: Vec3, target_aim: f32, half: f32, sh: &Shooter) -> Vec<Vec3> {
        self.hidden_spots(target, target_aim, half, sh).into_iter().filter(|s| s.1).map(|s| s.0).collect()
    }

    /// Where a raider like `sh` is in range of `target` and hidden from
    /// every defense we know of, and whether its line to the target is free
    /// there (see [`Precision::firing_spots`]).
    pub fn hidden_spots(&self, target: Vec3, target_aim: f32, half: f32, sh: &Shooter) -> Vec<(Vec3, bool)> {
        self.spots_under(target, target_aim, half, sh, 0.0).into_iter().map(|s| (s.0, s.1)).collect()
    }

    /// Where a raider like `sh` is in range of `target` and takes at most
    /// `max_dps` from the defenses we know of: each spot, whether its line
    /// to the target is free, and the damage per second it takes there.
    pub fn spots_under(
        &self,
        target: Vec3,
        target_aim: f32,
        half: f32,
        sh: &Shooter,
        max_dps: f32,
    ) -> Vec<(Vec3, bool, f32)> {
        let Some(ground) = self.ground.as_ref() else { return Vec::new() };
        let mut spots = Vec::new();
        let slack = SPOT_SLACK.min(SPOT_SLACK_SHARE * sh.range);
        let mut r = half + SPOT_STEP;
        while r <= sh.range - slack {
            let n = (std::f32::consts::TAU * r / SPOT_STEP).ceil() as i32;
            for k in 0..n {
                let a = k as f32 / n as f32 * std::f32::consts::TAU;
                let (x, z) = (target.x + r * a.cos(), target.z + r * a.sin());
                if !self.passable(ground, x, z, sh) {
                    continue;
                }
                let dps = self.exposure_around(x, z, sh.aim);
                if dps > max_dps {
                    continue;
                }
                let stand = ground.stand(x, z);
                let dh = (target_aim - stand - sh.muzzle) * sh.height_mod;
                if r * r > (sh.range - slack).powi(2) - dh * dh {
                    continue;
                }
                let from = [x, stand + sh.muzzle, z];
                let free = ground.line_is_free(from, [target.x, target_aim, target.z], sh.aoe, 0.0, half + 8.0);
                spots.push((Vec3::new(x, stand, z), free, dps));
            }
            r += SPOT_STEP;
        }
        spots
    }

    /// Whether a raider like `sh` at `at` can hit the building at `target`
    /// (as for [`Precision::firing_spots`]) unseen, standing right there.
    pub fn fires_unseen_from(&self, at: Vec3, target: Vec3, target_aim: f32, half: f32, sh: &Shooter) -> bool {
        let Some(ground) = self.ground.as_ref() else { return false };
        let stand = ground.stand(at.x, at.z);
        let r = (at.x - target.x).hypot(at.z - target.z);
        let dh = (target_aim - stand - sh.muzzle) * sh.height_mod;
        r * r <= (sh.range - 8.0).powi(2) - dh * dh
            && self.hidden_at(at.x, at.z, sh.aim)
            && ground.line_is_free(
                [at.x, stand + sh.muzzle, at.z],
                [target.x, target_aim, target.z],
                sh.aoe,
                0.0,
                half + 8.0,
            )
    }

    /// The damage a raider like `sh` would take getting out of reach after
    /// killing the building at `target` (`half` across) from `spot`, where
    /// that building itself is what hides it from a defense (the narrow
    /// cone behind a lone tower: it goes with the tower). Assumes a straight
    /// run away from each such defense.
    pub fn after_kill_damage(&self, spot: Vec3, target: Vec3, half: f32, sh: &Shooter) -> f32 {
        let Some(ground) = self.ground.as_ref() else { return 0.0 };
        let stand = ground.stand(spot.x, spot.z);
        let mut damage = 0.0;
        for g in self.guns.values() {
            for a in &g.arms {
                if !a.gun.reaches(spot.x, spot.z, stand + sh.aim, RANGE_MARGIN)
                    || (!g.stale && a.bare_need_at(spot.x, spot.z) > sh.aim + HIDE_MARGIN)
                {
                    continue;
                }
                // Does its line to the spot pass through the target?
                let (dx, dz) = (spot.x - a.gun.x, spot.z - a.gun.z);
                let len2 = (dx * dx + dz * dz).max(1.0);
                let t = (((target.x - a.gun.x) * dx + (target.z - a.gun.z) * dz) / len2).clamp(0.0, 1.0);
                let (px, pz) = (a.gun.x + dx * t, a.gun.z + dz * t);
                if (target.x - px).hypot(target.z - pz) > half + 8.0
                    || self.others_cover([a.gun.x, a.gun.muzzle, a.gun.z], [spot.x, stand + sh.aim, spot.z], target)
                {
                    continue;
                }
                let out = (a.gun.range + RANGE_MARGIN - len2.sqrt()).max(0.0);
                damage += a.dps * out / sh.speed.max(1.0);
            }
        }
        damage
    }

    /// Whether a building other than the one standing at `except` covers
    /// the line from `from` to `to` (rises over it somewhere).
    fn others_cover(&self, from: [f32; 3], to: [f32; 3], except: Vec3) -> bool {
        let d = (to[0] - from[0]).hypot(to[2] - from[2]).max(1.0);
        let steps = (d / 8.0).ceil() as i32;
        self.blocks.values().filter_map(|b| b.volume.filter(|_| (b.x - except.x).hypot(b.z - except.z) > 2.0)).any(
            |v| {
                // Near the line at all?
                let t = (((v.x - from[0]) * (to[0] - from[0]) + (v.z - from[2]) * (to[2] - from[2])) / (d * d))
                    .clamp(0.0, 1.0);
                let (px, pz) = (from[0] + (to[0] - from[0]) * t, from[2] + (to[2] - from[2]) * t);
                (v.x - px).hypot(v.z - pz) <= v.hx.max(v.hz)
                    && (1..steps).any(|k| {
                        let t = k as f32 / steps as f32;
                        let (x, z) = (from[0] + (to[0] - from[0]) * t, from[2] + (to[2] - from[2]) * t);
                        v.top_at(x, z) > from[1] + (to[1] - from[1]) * t
                    })
            },
        )
    }

    /// The cheapest way for a raider like `sh` at `from` to `spot`: from
    /// ground where `safe` holds and no defense we know of reaches (or from
    /// where it stands, if that is in the window), weighing time against
    /// damage taken. A cloaked raider is fired at only where it comes near
    /// enough one of `revealers` (enemy units) to be seen. `None` if no way
    /// is found in the window.
    pub fn approach(
        &self,
        spot: Vec3,
        from: Vec3,
        sh: &Shooter,
        revealers: &[Vec3],
        safe: impl Fn(Vec3) -> bool,
    ) -> Option<Approach> {
        let reveal = (sh.cloak + APPROACH_CELL).powi(2);
        let seen = |x: f32, z: f32| {
            sh.cloak <= 0.0 || revealers.iter().any(|r| (r.x - x).powi(2) + (r.z - z).powi(2) < reveal)
        };
        let ground = self.ground.as_ref()?;
        let n = (2.0 * APPROACH_WINDOW / APPROACH_CELL) as i32 + 1;
        let (x0, z0) = (spot.x - APPROACH_WINDOW, spot.z - APPROACH_WINDOW);
        let center = |i: usize| {
            let (cx, cz) = ((i as i32 % n) as f32, (i as i32 / n) as f32);
            (x0 + cx * APPROACH_CELL, z0 + cz * APPROACH_CELL)
        };
        let index = |x: f32, z: f32| {
            let (cx, cz) = (((x - x0) / APPROACH_CELL).round() as i32, ((z - z0) / APPROACH_CELL).round() as i32);
            (cx >= 0 && cz >= 0 && cx < n && cz < n).then(|| (cz * n + cx) as usize)
        };
        let len = (n * n) as usize;
        let mut cost = vec![f32::INFINITY; len];
        let mut damage = vec![0.0f32; len];
        let mut parent = vec![usize::MAX; len];
        // Per cell: NaN until looked at, else the damage per second taken
        // there (infinite where impassable).
        let mut dps = vec![f32::NAN; len];
        let start = index(spot.x, spot.z)?;
        let own = index(from.x, from.z);
        cost[start] = 0.0;
        let mut heap = BinaryHeap::new();
        heap.push(Visit(0.0, start));
        let speed = sh.speed.max(1.0);
        let mut best: Option<(f32, usize)> = None;
        while let Some(Visit(c, i)) = heap.pop() {
            if c > cost[i] {
                continue;
            }
            if best.is_some_and(|(b, _)| c >= b) {
                break;
            }
            let (x, z) = center(i);
            // Ground the defenses do not reach: the way in starts here, after
            // the raider gets here on its own.
            if Some(i) == own || (dps[i] == 0.0 && safe(Vec3::xz(x, z))) {
                let total = c + if Some(i) == own { 0.0 } else { (x - from.x).hypot(z - from.z) / speed };
                if best.is_none_or(|(b, _)| total < b) {
                    best = Some((total, i));
                }
            }
            for (dx, dz) in [(1, 0), (-1, 0), (0, 1), (0, -1), (1, 1), (1, -1), (-1, 1), (-1, -1)] {
                let (cx, cz) = (i as i32 % n + dx, i as i32 / n + dz);
                if cx < 0 || cz < 0 || cx >= n || cz >= n {
                    continue;
                }
                let j = (cz * n + cx) as usize;
                if dps[j].is_nan() {
                    let (x, z) = center(j);
                    dps[j] = if !self.passable(ground, x, z, sh) {
                        f32::INFINITY
                    } else if seen(x, z) {
                        self.exposure(x, z, sh.aim)
                    } else {
                        0.0
                    };
                }
                if dps[j].is_infinite() {
                    continue;
                }
                let dt = APPROACH_CELL * ((dx * dx + dz * dz) as f32).sqrt() / speed;
                let next = c + dt * (1.0 + EXPOSURE_WEIGHT * dps[j] / sh.health.max(1.0));
                if next < cost[j] {
                    cost[j] = next;
                    damage[j] = damage[i] + dps[j] * dt;
                    parent[j] = i;
                    heap.push(Visit(next, j));
                }
            }
        }
        let (_, entry) = best?;
        let mut path = Vec::new();
        let mut i = entry;
        let mut time = 0.0;
        while i != usize::MAX {
            let (x, z) = center(i);
            let next = parent[i];
            if next != usize::MAX {
                let (nx, nz) = center(next);
                time += (nx - x).hypot(nz - z) / speed;
            }
            path.push(Vec3::new(x, ground.stand(x, z), z));
            i = next;
        }
        // Thin the waypoints: every other, and the spot itself.
        let last = path.len() - 1;
        let path: Vec<Vec3> =
            (path.iter().enumerate()).filter(|(k, _)| *k == last || (*k > 0 && k % 2 == 0)).map(|(_, p)| *p).collect();
        let (ex, ez) = center(entry);
        Some(Approach { entry: Vec3::new(ex, ground.stand(ex, ez), ez), path, damage: damage[entry], time })
    }

    /// Whether unit `id` is a defense in the model.
    pub fn is_gun(&self, id: i32) -> bool {
        self.guns.contains_key(&id)
    }

    /// The defense in reach of `at` that lies along bearing `dir` from it
    /// (a hit's direction points back at its shooter), within 15 degrees.
    pub fn gun_toward(&self, at: Vec3, dir: Vec3) -> Option<i32> {
        let h = dir.x.hypot(dir.z);
        if h < 1e-3 {
            return None;
        }
        let (bx, bz) = (dir.x / h, dir.z / h);
        let mut best: Option<(f32, i32)> = None;
        for (&id, g) in &self.guns {
            let (dx, dz) = (g.pos.x - at.x, g.pos.z - at.z);
            let d = dx.hypot(dz);
            let reach = g.arms.iter().map(|a| a.gun.range).fold(0.0, f32::max) + RANGE_MARGIN;
            if d < 1.0 || d > reach {
                continue;
            }
            let cos = (dx * bx + dz * bz) / d;
            if cos > 15f32.to_radians().cos() && best.is_none_or(|b| cos > b.0) {
                best = Some((cos, id));
            }
        }
        best.map(|b| b.1)
    }

    /// The defenses in the model, by unit id.
    pub(crate) fn gun_ids(&self) -> Vec<i32> {
        self.guns.keys().copied().collect()
    }

    /// Where defense `id` stands, its type, and the reach of its longest
    /// weapon.
    pub(crate) fn gun_info(&self, id: i32) -> Option<(Vec3, &str, f32)> {
        let g = self.guns.get(&id)?;
        Some((g.pos, g.name.as_str(), g.arms.iter().map(|a| a.gun.range).fold(0.0, f32::max)))
    }

    /// The least height above the ground at `(x, z)` at which defense `id`
    /// hits a target, over its weapons in reach.
    pub(crate) fn need_of(&self, id: i32, x: f32, z: f32, aim: f32) -> f32 {
        let (Some(g), Some(ground)) = (self.guns.get(&id), self.ground.as_ref()) else { return f32::INFINITY };
        let stand = ground.stand(x, z);
        (g.arms.iter())
            .filter(|a| a.gun.reaches(x, z, stand + aim, RANGE_MARGIN))
            .map(|a| if g.stale { f32::NEG_INFINITY } else { a.need_at(x, z) })
            .fold(f32::INFINITY, f32::min)
    }

    /// Each defense whose weapons reach `(x, z)`, how far it is, the least
    /// height there at which it hits, and the least muzzle height from which
    /// it would see a target `aim` up over the terrain.
    pub(crate) fn needs_at(&self, x: f32, z: f32, aim: f32) -> Vec<(&str, f32, f32, f32)> {
        (self.guns.keys())
            .filter_map(|&id| {
                let g = &self.guns[&id];
                let need = self.need_of(id, x, z, aim);
                let muzzle = self.least_muzzle(id, x, z, aim);
                need.is_finite().then(|| (g.name.as_str(), (g.pos.x - x).hypot(g.pos.z - z), need, muzzle))
            })
            .collect()
    }

    /// The least muzzle height (above its ground) from which defense `id`
    /// has a line clear of the terrain to a target `aim` above the ground
    /// at `(x, z)` (ground within its area of effect of the target does not
    /// count); `INFINITY` if none up to 400 elmos.
    pub(crate) fn least_muzzle(&self, id: i32, x: f32, z: f32, aim: f32) -> f32 {
        let (Some(g), Some(ground)) = (self.guns.get(&id), self.ground.as_ref()) else { return f32::INFINITY };
        let Some(a) = g.arms.first() else { return f32::INFINITY };
        let base = ground.ground(g.pos.x, g.pos.z).max(0.0);
        let to = [x, ground.stand(x, z) + aim, z];
        let free = |m: f32| ground.line_is_free([g.pos.x, base + m, g.pos.z], to, a.gun.aoe, f32::INFINITY, 0.0);
        if !free(400.0) {
            return f32::INFINITY;
        }
        let (mut lo, mut hi) = (0.0, 400.0);
        for _ in 0..12 {
            let mid = (lo + hi) / 2.0;
            if free(mid) { hi = mid } else { lo = mid }
        }
        hi
    }

    /// The terrain and buildings between each defense of type `name` that
    /// reaches `(x, z)` and a target there, for calibration logs.
    pub(crate) fn profiles(&self, name: &str, x: f32, z: f32, aim: f32) -> Vec<String> {
        let Some(ground) = self.ground.as_ref() else { return Vec::new() };
        let mut out = Vec::new();
        for (&id, g) in self.guns.iter().filter(|(_, g)| g.name == name) {
            let need = self.need_of(id, x, z, aim);
            if !need.is_finite() {
                continue;
            }
            let Some(a) = g.arms.first() else { continue };
            let d = (x - g.pos.x).hypot(z - g.pos.z);
            let mut line = format!(
                "{name} at ({:.0}, {:.0}) y={:.0} ground={:.0} muzzle={:.0} {:?} need={need:.1}, target stand {:.0}:",
                g.pos.x,
                g.pos.z,
                g.pos.y,
                ground.ground(g.pos.x, g.pos.z),
                a.gun.muzzle,
                a.gun.flight,
                ground.stand(x, z)
            );
            let steps = (d / 16.0).ceil() as i32;
            for k in 1..steps {
                let t = k as f32 / steps as f32;
                let (px, pz) = (g.pos.x + (x - g.pos.x) * t, g.pos.z + (z - g.pos.z) * t);
                let top = ground.block_top_at(px, pz);
                line += &format!(" {:.0}", ground.ground(px, pz));
                if top.is_finite() {
                    line += &format!("[{top:.0}]");
                }
            }
            out.push(line);
        }
        out
    }

    /// The least height above the ground at `(x, z)` at which defense `id`
    /// hits a target as far as the terrain goes (buildings aside).
    pub(crate) fn bare_need_of(&self, id: i32, x: f32, z: f32, aim: f32) -> f32 {
        let (Some(g), Some(ground)) = (self.guns.get(&id), self.ground.as_ref()) else { return f32::INFINITY };
        let stand = ground.stand(x, z);
        (g.arms.iter())
            .filter(|a| a.gun.reaches(x, z, stand + aim, RANGE_MARGIN))
            .map(|a| if g.stale { f32::NEG_INFINITY } else { a.bare_need_at(x, z) })
            .fold(f32::INFINITY, f32::min)
    }

    /// Whether a unit like `sh` can stand at `(x, z)`.
    pub(crate) fn can_stand(&self, x: f32, z: f32, sh: &Shooter) -> bool {
        self.ground.as_ref().is_some_and(|g| self.passable(g, x, z, sh))
    }

    /// Cells in reach of the defenses we know of, and the share of them
    /// where a unit with its aim point `aim` above the ground is shadowed.
    fn shadowed_share(&self, aim: f32) -> (usize, f32) {
        let Some(ground) = self.ground.as_ref() else { return (0, 0.0) };
        let (mut reach, mut shadowed) = (0, 0);
        for g in self.guns.values() {
            for a in &g.arms {
                let Some(shadow) = a.shadow.as_ref().filter(|_| !g.stale) else { continue };
                let (r, s) = shadow.count(ground, &a.gun, aim + HIDE_MARGIN);
                reach += r;
                shadowed += s;
            }
        }
        (reach, if reach > 0 { shadowed as f32 / reach as f32 } else { 0.0 })
    }
}

/// A Dijkstra queue entry, cheapest first.
#[derive(PartialEq)]
struct Visit(f32, usize);

impl Eq for Visit {}

impl PartialOrd for Visit {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Visit {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        other.0.total_cmp(&self.0).then(other.1.cmp(&self.1))
    }
}

impl Ai {
    /// Keep the precision map current, and report on it now and then.
    pub fn precision_update(&mut self, frame: i32) {
        self.precision.poll_shadows();
        if frame % SYNC_FRAMES != (self.offset + 11) % SYNC_FRAMES {
            return;
        }
        let map = self.cb.map();
        if self.precision.ground.is_none() || frame - self.precision.last_terrain >= TERRAIN_FRAMES {
            self.precision.last_terrain = frame;
            self.precision.set_terrain(map.corner_height_map(), map.width(), map.height());
        }
        let standing = self.precision_standing();
        let gravity = map.gravity();
        self.guard("precision.sync", |ai| ai.precision.sync_model(&standing, gravity, true));
        self.precision_cleanup();
        if frame - self.precision.last_report >= REPORT_FRAMES {
            self.precision.last_report = frame;
            self.guard("precision.report", |ai| ai.precision_report(frame));
        }
    }

    /// The enemy buildings we know of.
    fn precision_standing(&mut self) -> Vec<Standing> {
        let found: Vec<(i32, UnitDef, Vec3, bool, springai::Unit)> = (self.war.targets.values())
            .filter(|e| e.identified && e.is_static && !e.is_dead)
            .filter_map(|e| Some((e.unit_id, e.ud?, e.position, e.is_porc && !e.is_nano_spam, e.unit)))
            .collect();
        found.into_iter().map(|(id, def, pos, armed, unit)| self.precision_stand(id, def, pos, armed, unit)).collect()
    }

    /// An enemy building as the model takes it in: its collision volume
    /// (grown as far as a nanoframe has) and footprint, turned its way.
    pub(crate) fn precision_stand(
        &mut self,
        id: i32,
        def: UnitDef,
        pos: Vec3,
        armed: bool,
        unit: springai::Unit,
    ) -> Standing {
        let vd = self.precision_volume_def(def);
        let ready = !unit.is_being_built();
        let grow = if ready || def.custom_param("no_grow").is_some() {
            1.0
        } else {
            nanoframe_growth(unit.build_progress(), def.cost(self.metal))
        };
        let facing = unit.building_facing().max(0);
        let ground = self.precision.ground().map_or(0.0, |g| g.ground(pos.x, pos.z));
        let volume = vd.at(pos, facing, ground, grow);
        let (fx, fz) = (def.x_size() as f32 * 4.0, def.z_size() as f32 * 4.0);
        let foot = if facing % 2 == 1 { (fz, fx) } else { (fx, fz) };
        Standing { id, def, pos, armed, ready, volume, foot }
    }

    /// The collision volume of unit type `def`: as its definition file
    /// declares it, or else the engine's default sphere.
    fn precision_volume_def(&mut self, def: UnitDef) -> VolumeDef {
        if let Some(v) = self.precision.volume_defs.get(&def.id()) {
            return *v;
        }
        let name = def.name();
        let file = self.cb.file_content(&format!("units/{name}.lua"));
        let parsed = file.as_deref().and_then(|t| parse_volume(&String::from_utf8_lossy(t)));
        let v = parsed.unwrap_or_else(|| VolumeDef::sphere(def.radius()));
        self.log(&format!(
            "precision: {name} volume {:?} {:?} {:?}{} ({})",
            v.shape,
            v.scales,
            v.offsets,
            if v.sphere { " sphere" } else { "" },
            if parsed.is_some() {
                "declared"
            } else if file.is_some() {
                "default"
            } else {
                "default, no file"
            }
        ));
        self.precision.volume_defs.insert(def.id(), v);
        v
    }

    /// A shooter model for our unit type `def`, its aim point taken as half
    /// its model height, and its muzzle as no higher (a higher one would
    /// find spots it cannot shoot from).
    pub fn precision_shooter(&self, def: UnitDef) -> Option<Shooter> {
        let aim = 0.5 * def.height();
        Shooter::of(def, aim, aim)
    }

    /// Log how much of the enemy's covered buildings our raider types could
    /// hit from the shadows: the measure of whether precision raids pay.
    fn precision_report(&self, frame: i32) {
        let p = &self.precision;
        let Some(ground) = p.ground() else { return };
        let covered: Vec<(Vec3, f32, f32)> = (self.war.targets.values())
            .filter(|e| e.identified && e.is_static && !e.is_dead && !e.is_porc && e.ud.is_some())
            .filter(|e| self.war.get_porc_threat(e.position) > 0.0)
            .filter_map(|e| {
                let def = e.ud?;
                let half = def.x_size().min(def.z_size()) as f32 * 4.0;
                let aim = e.position.y.max(ground.stand(e.position.x, e.position.z) + 0.5 * def.height());
                Some((e.position, aim, half))
            })
            .collect();
        let buildings = self.war.targets.values().filter(|e| e.identified && e.is_static && !e.is_dead).count();
        let st = &p.stats;
        let mut line = format!(
            "precision: f={frame} guns={} distrusted={} buildings={buildings} covered={} missions: on {} started {} fired {} kills {} aborted {} lost {} hit-at-spot {}",
            p.guns.len(),
            p.distrusted.len(),
            covered.len(),
            p.missions.len(),
            st.started,
            st.fired,
            st.kills,
            st.aborted,
            st.lost,
            st.hit_at_spot
        );
        if let Some(worker) = &p.worker {
            line += &format!(
                "; shadow worker {} pending_guns={}",
                worker.stats(),
                p.guns.values().filter(|g| g.stale).count()
            );
        }
        // Exhaustive diagnostics used to stall normal play once a minute.
        // Opt in explicitly when calibrating precision raids.
        if std::env::var("ZKGBAI_PRECISION_REPORT").as_deref() != Ok("full") {
            self.log(&line);
            return;
        }
        // The raider types we have (the search is costly at fine cells).
        let ours: BTreeSet<&str> = self.fighters.values().map(|f| self.defs.name_of(f.unit)).collect();
        let mut shadow_done = false;
        for name in RAIDERS.into_iter().filter(|n| ours.contains(n)) {
            let Some(sh) = self.defs.get(name).and_then(|d| self.precision_shooter(d)) else { continue };
            let hittable =
                covered.iter().filter(|(pos, aim, half)| !p.firing_spots(*pos, *aim, *half, &sh).is_empty()).count();
            line += &format!(" {name}(aim {:.0}, range {:.0}): {hittable}", sh.aim, sh.range);
            if !shadow_done {
                let (reach, share) = p.shadowed_share(sh.aim);
                line += &format!(" [shadow {:.0}% of {reach}]", share * 100.0);
                shadow_done = true;
            }
        }
        self.log(&line);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flat(n: i32) -> FireGround {
        FireGround::new(vec![0.0; ((n + 1) * (n + 1)) as usize], n, n)
    }

    fn glaive() -> Shooter {
        Shooter {
            range: 185.0,
            aim: 14.0,
            muzzle: 14.0,
            aoe: 4.0,
            height_mod: 0.2,
            max_slope: 0.5,
            max_depth: 22.0,
            speed: 100.0,
            health: 230.0,
            dps: 60.0,
            cloak: 0.0,
        }
    }

    /// A Lotus at `(x, z)` whose line of fire is blocked by a wall `wall`
    /// elmos east of it.
    fn walled_lotus(x: f32, z: f32, wall: f32) -> Precision {
        let mut ground = flat(256);
        let volume = Volume { x: x + wall, y: 30.0, z, hx: 16.0, hy: 30.0, hz: 120.0, shape: Shape::Box };
        ground.set_volumes([volume]);
        let gun = Gun {
            x,
            z,
            muzzle: 30.0,
            range: 460.0,
            height_mod: 0.2,
            aoe: 4.0,
            body: 16.0,
            flight: Flight::Line,
            avoids_friends: true,
            through_ground: false,
        };
        let shadow = Shadow::compute(&ground, &gun, RANGE_MARGIN + CELL);
        let bare = Shadow::compute(&ground, &Gun { avoids_friends: false, ..gun }, RANGE_MARGIN + CELL);
        let arm = Arm { gun, shadow: Some(shadow), bare: Some(bare), dps: 100.0, water_only: false };
        let mut p = Precision { ground: Some(Arc::new(ground)), ..Default::default() };
        p.blocks.insert(1, Footing { x: volume.x, z, hx: 16.0, hz: 120.0, volume: Some(volume) });
        p.guns.insert(
            2,
            Armed { pos: Vec3::xz(x, z), name: "turretlaser".into(), arms: vec![arm], stale: false, revision: 0 },
        );
        p
    }

    #[test]
    fn stale_shadows_are_not_safe_and_obsolete_results_are_rejected() {
        let mut p = walled_lotus(500.0, 500.0, 100.0);
        assert!(p.hidden_at(700.0, 500.0, 14.0));
        let gun = p.guns.get_mut(&2).unwrap();
        let arms = std::mem::take(&mut gun.arms);
        gun.arms = arms
            .iter()
            .map(|a| Arm { gun: a.gun, dps: a.dps, water_only: a.water_only, shadow: None, bare: None })
            .collect();
        gun.stale = true;
        gun.revision = 2;
        assert!(!p.hidden_at(700.0, 500.0, 14.0));
        p.accept_shadows(vec![(2, 1, arms)]);
        assert!(p.guns[&2].stale);
        assert!(!p.hidden_at(700.0, 500.0, 14.0));
        let arms = std::mem::take(&mut p.guns.get_mut(&2).unwrap().arms);
        let result = cast_shadows((p.ground.as_ref().unwrap().clone(), vec![(2, 2, arms)]));
        p.accept_shadows(result);
        assert!(!p.guns[&2].stale);
        assert!(p.hidden_at(700.0, 500.0, 14.0));
        p.guns.remove(&2);
        p.accept_shadows(vec![(2, 2, vec![])]);
        assert!(p.guns.is_empty());
    }

    #[test]
    fn reads_declared_volumes_of_the_unit_not_its_wreck() {
        let mex = r#"unitDef = {
            collisionVolumeOffsets = [[0 -8 0]],
            collisionVolumeScales  = [[40 58 40]],
            collisionVolumeType    = [[ellipsoid]],
          featureDefs = { DEAD = { collisionVolumeScales = [[10 10 10]], } }"#;
        let v = parse_volume(mex).unwrap();
        assert_eq!(
            (v.scales, v.offsets, v.shape, v.sphere),
            ([40.0, 58.0, 40.0], [0.0, -8.0, 0.0], Shape::Ellipsoid, false)
        );
        let lotus = "collisionVolumeScales = [[30 90 30]], collisionVolumeType = [[CylY]],";
        assert_eq!(parse_volume(lotus).unwrap().shape, Shape::CylinderY);
        let ball = "collisionVolumeScales = [[20 30 40]], collisionVolumeType = [[sphere]],";
        assert_eq!(parse_volume(ball).unwrap().scales, [40.0; 3]);
        // A solar declares none: the engine's default sphere.
        assert_eq!(
            parse_volume("unitDef = { footprintX = 5, featureDefs = { collisionVolumeScales = [[1 1 1]] } }"),
            None
        );
    }

    #[test]
    fn nanoframes_grow_from_the_ground_up() {
        let mex =
            VolumeDef { scales: [40.0, 58.0, 40.0], offsets: [0.0, -8.0, 0.0], shape: Shape::Ellipsoid, sphere: false };
        let mid = Vec3::new(100.0, 29.0, 100.0);
        let full = mex.at(mid, 0, 0.0, 1.0).unwrap();
        assert_eq!(full.y + full.hy, 50.0);
        let half = mex.at(mid, 0, 0.0, 0.5).unwrap();
        assert!(half.y + half.hy < 30.0 && half.y - half.hy == full.y - full.hy);
        assert_eq!(mex.at(mid, 0, 0.0, 0.0), None);
        // 15 metal in nothing shows; full height 15 metal past half built.
        assert_eq!(nanoframe_growth(0.1, 150.0), 0.0);
        assert_eq!(nanoframe_growth(0.6, 150.0), 1.0);
        // Turned, a long box swaps its sides.
        let fac = VolumeDef { scales: [100.0, 40.0, 60.0], offsets: [0.0; 3], shape: Shape::Box, sphere: false };
        assert_eq!(fac.at(mid, 1, 0.0, 1.0).unwrap().hx, 30.0);
    }

    #[test]
    fn a_lone_lotus_cannot_cover_its_own_radar() {
        // A radar (ZK's: an upright cylinder 32 wide, 90 tall, 32 sunk)
        // 150 east of a Lotus: in the narrow cone behind it, a Glaive kills
        // it unseen.
        let mut ground = flat(256);
        let (lx, lz) = (600.0, 1000.0);
        let radar = Volume { x: lx + 150.0, y: 13.0, z: lz, hx: 16.0, hy: 45.0, hz: 16.0, shape: Shape::CylinderY };
        ground.set_volumes([radar]);
        let gun = Gun {
            x: lx,
            z: lz,
            muzzle: 62.0,
            range: 460.0,
            height_mod: 0.2,
            aoe: 4.0,
            body: 15.0,
            flight: Flight::Line,
            avoids_friends: true,
            through_ground: false,
        };
        let shadow = Shadow::compute(&ground, &gun, RANGE_MARGIN + CELL);
        let bare = Shadow::compute(&ground, &Gun { avoids_friends: false, ..gun }, RANGE_MARGIN + CELL);
        let arm = Arm { gun, shadow: Some(shadow), bare: Some(bare), dps: 100.0, water_only: false };
        let mut p = Precision { ground: Some(Arc::new(ground)), ..Default::default() };
        p.guns.insert(
            1,
            Armed { pos: Vec3::xz(lx, lz), name: "turretlaser".into(), arms: vec![arm], stale: false, revision: 0 },
        );
        p.blocks.insert(2, Footing { x: radar.x, z: lz, hx: 16.0, hz: 16.0, volume: Some(radar) });
        let spots = p.firing_spots(Vec3::new(radar.x, 30.0, lz), 30.0, 16.0, &glaive());
        assert!(spots.len() >= 6, "{}", spots.len());
        // All of them in the cone behind the radar.
        assert!(spots.iter().all(|s| s.x > radar.x + 16.0 && (s.z - lz).abs() < 40.0), "{spots:?}");
        // But the cone goes with the radar: a Glaive left standing in it
        // runs out of the Lotus's reach under fire.
        let after = p.after_kill_damage(spots[0], Vec3::xz(radar.x, lz), 16.0, &glaive());
        assert!(after > 100.0, "{after}");
        // Not so behind a wall that stays.
        let w = walled_lotus(600.0, 1000.0, 100.0);
        let s = w.firing_spots(Vec3::new(850.0, 29.0, 1000.0), 29.0, 24.0, &glaive());
        assert_eq!(w.after_kill_damage(s[0], Vec3::xz(850.0, 1000.0), 24.0, &glaive()), 0.0);
    }

    #[test]
    fn spots_behind_the_wall_only() {
        let p = walled_lotus(600.0, 1000.0, 100.0);
        // A mex 250 east of the Lotus, behind its wall: Glaives can hit it
        // from the east, out of the Lotus's line.
        let mex = Vec3::new(850.0, 29.0, 1000.0);
        let spots = p.firing_spots(mex, 29.0, 24.0, &glaive());
        assert!(!spots.is_empty());
        assert!(spots.iter().all(|s| s.x > 700.0 + 16.0));
        assert!(spots.iter().all(|s| p.exposure(s.x, s.z, 14.0) == 0.0));
        // In the open, west of the wall, nothing.
        let open = Vec3::new(450.0, 29.0, 1000.0);
        assert!(p.firing_spots(open, 29.0, 24.0, &glaive()).is_empty());
        // Distrusted, the Lotus hits everywhere in range.
        let mut p = p;
        p.distrusted.insert(2);
        assert!(p.firing_spots(mex, 29.0, 24.0, &glaive()).is_empty());
    }

    #[test]
    fn approach_comes_in_behind_the_wall() {
        let p = walled_lotus(600.0, 1000.0, 100.0);
        let sh = glaive();
        let spot = Vec3::xz(900.0, 1000.0);
        // Safe ground: beyond the Lotus's reach.
        let safe = |q: Vec3| (q.x - 600.0).hypot(q.z - 1000.0) > 500.0;
        // From the east, straight in: no damage.
        let a = p.approach(spot, Vec3::xz(1600.0, 1000.0), &sh, &[], safe).unwrap();
        assert_eq!(a.damage, 0.0);
        assert!(a.entry.x > 1050.0, "{:?}", a.entry);
        assert_eq!(*a.path.last().unwrap(), Vec3::new(spot.x, 0.0, spot.z));
        // From the west, it goes around the Lotus's reach rather than
        // through its fire... or takes a little; never a lot.
        let a = p.approach(spot, Vec3::xz(0.0, 1000.0), &sh, &[], safe).unwrap();
        assert!(a.damage < 0.3 * sh.health, "{}", a.damage);
        // Cloaked, it walks through the Lotus's fire unseen, but for near
        // the Lotus itself.
        let scythe = Shooter { cloak: 75.0, ..sh };
        let a = p.approach(spot, Vec3::xz(0.0, 1000.0), &scythe, &[Vec3::xz(600.0, 1000.0)], safe).unwrap();
        assert_eq!(a.damage, 0.0);
        // Standing in the shadow already: the way in starts where it is.
        let a = p.approach(spot, Vec3::xz(860.0, 1000.0), &sh, &[], safe).unwrap();
        assert!((a.entry.x - 860.0).abs() <= APPROACH_CELL && a.damage == 0.0, "{:?}", a);
    }
}
