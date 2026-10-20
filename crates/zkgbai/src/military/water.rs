//! Under water. The engine lets only water weapons (`waterweapon`) hit what
//! is under water — its middle plus radius below the surface — and
//! torpedoes hit only what is in water (its base below the surface). So a
//! submersible's danger changes with depth: deep enough, only water weapons
//! reach it; wading or afloat, everything does; on dry land, everything but
//! torpedoes. What it can fire changes too: under water only weapons that
//! fire submerged do (torpedoes), and the floaters (Buoy, Archer, Grizzly)
//! rise to the surface to fire (ZK's float toggle).
//!
//! Enemies paint what they hit into two layers: the usual threat map
//! (targets above water) and one for targets under water. A submerged Duck
//! or Seawolf paints only the latter (nothing on land fears it), a floater
//! paints the surface layer wherever it is (it comes up to fight). Our units
//! read the layers that reach them where they are (see [`ThreatView`]): a
//! Duck under water fears only torpedoes, a ship or a wading bot fears both.

use springai::{Callback, UnitDef, Vec3};

use zk_terrain::Tier;

use super::{Enemy, MilitaryManager, cell};
use crate::Ai;
use crate::terrain::tier_of;

/// Engine weapon class of torpedoes: they hit only what is in water.
const TORPEDO: &str = "TorpedoLauncher";

/// Frames between the summary lines in the log.
const STATS_PERIOD: i32 = 1800;

/// Types whose profiles are logged on a wet map, to check them by.
const LOGGED: [&str; 9] = [
    "amphraid",
    "amphriot",
    "amphassault",
    "amphfloater",
    "amphimpulse",
    "amphcon",
    "subraider",
    "shiptorpraider",
    "turrettorp",
];

/// Where a unit is, for what can hit it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Medium {
    /// On land (or in the air): everything but torpedoes.
    Dry,
    /// In water but not all under it (wading, afloat): everything.
    Wading,
    /// All under water: water weapons only.
    Submerged,
}

/// The water depth from which a type of unit is all under water: never
/// (infinite) for hovers, ships, land units and floaters (which surface to
/// fight, so they count as surface units), 0 for submarines (whenever in
/// water), and the unit's height for amphibious walkers. Aircraft (negative)
/// are never in water at all.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Dive(pub f32);

impl Dive {
    pub const SURFACE: Dive = Dive(f32::INFINITY);
    pub const AIR: Dive = Dive(-1.0);

    /// Where a unit of this type is, in water `depth` deep.
    pub fn medium(self, depth: f32) -> Medium {
        if depth <= 0.0 || self.0 < 0.0 {
            Medium::Dry
        } else if depth >= self.0 {
            Medium::Submerged
        } else {
            Medium::Wading
        }
    }
}

/// What a type of unit's weapons hit: anything under water (a water
/// weapon), and anything on dry land (a weapon that is not a torpedo).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Reach {
    pub under_water: bool,
    pub dry_land: bool,
}

impl Reach {
    /// What unarmed units, and types we know nothing of, are taken to hit.
    pub const SURFACE: Reach = Reach { under_water: false, dry_land: true };
}

/// How a type of unit stands to water, worked out once per type.
#[derive(Clone, Copy)]
struct Profile {
    dive: Dive,
    reach: Reach,
    /// Strength of its water weapons (as [`super::wrappers::weapon_power`]).
    deep_power: f32,
}

/// Whether a type of unit rises to the surface to fire (ZK's float toggle).
pub fn is_floater(def: UnitDef) -> bool {
    def.custom_param("floattoggle").is_some()
}

/// Whether a weapon is only there to aim others (ZK's fake weapons).
pub fn is_fake(name: &str) -> bool {
    name.contains("fake") || name.contains("bogus")
}

fn profile(def: UnitDef) -> Profile {
    let weapons: Vec<_> = (def.weapon_mounts().iter())
        .filter_map(|m| m.weapon_def())
        .filter(|w| !is_fake(&w.name().to_lowercase()))
        .collect();
    let torpedo = |w: &springai::WeaponDef| w.weapon_type() == TORPEDO && !w.is_sub_missile();
    let deep: Vec<_> = weapons.iter().filter(|w| w.is_water_weapon()).collect();
    let reach = if weapons.is_empty() {
        Reach::SURFACE
    } else {
        Reach { under_water: !deep.is_empty(), dry_land: weapons.iter().any(|w| !torpedo(w)) }
    };
    // (Other water weapons need not fire submerged: the Archer's scripts
    // hold its fire under water.)
    let fights_submerged = weapons.iter().any(|w| torpedo(w) && w.is_fire_submersed());
    let dive = if def.is_able_to_fly() {
        Dive::AIR
    } else if def.move_data().is_some_and(|md| md.is_submarine()) {
        Dive(0.0)
    } else if tier_of(def) == Some(Tier::Amphib) && (fights_submerged || !is_floater(def)) {
        // A floater counts as an amphibious walker only if it can fight
        // from the depths (Scallop's torpedoes); else it surfaces to fight
        // (Buoy, Archer, Grizzly).
        Dive(def.height().max(1.0))
    } else {
        Dive::SURFACE
    };
    let deep_power = deep.iter().map(|&&w| super::wrappers::weapon_power(w, false)).sum();
    Profile { dive, reach, deep_power }
}

/// Water depth and unit profiles for [`MilitaryManager`].
pub struct Water {
    /// Water depth at each threat-map cell (0 on land), elmos, rows of
    /// `width`.
    depth: Vec<f32>,
    width: i32,
    /// Profiles by unit def id.
    profiles: Vec<Option<Profile>>,
    /// Whether the map has any water at all.
    pub wet: bool,
}

impl Water {
    pub fn new(cb: Callback, width: i32, height: i32) -> Self {
        let map = cb.map();
        let (w, h) = (map.width(), map.height());
        let heights = map.height_map();
        let mut depth = Vec::with_capacity((width.max(0) * height.max(0)) as usize);
        for y in 0..height {
            for x in 0..width {
                let (sx, sz) = ((x * 8).clamp(0, w - 1), (y * 8).clamp(0, h - 1));
                let elevation = heights.get((sz * w + sx) as usize).copied().unwrap_or(0.0);
                depth.push((-elevation).max(0.0));
            }
        }
        let wet = depth.iter().any(|&d| d > 0.0);
        let mut profiles = Vec::new();
        if wet {
            for def in cb.unit_defs() {
                let id = def.id() as usize;
                if profiles.len() <= id {
                    profiles.resize(id + 1, None);
                }
                profiles[id] = Some(profile(def));
            }
        }
        Water { depth, width, profiles, wet }
    }

    fn profile(&self, def: UnitDef) -> Option<&Profile> {
        self.profiles.get(def.id() as usize)?.as_ref()
    }
}

impl MilitaryManager {
    /// Water depth at `p` (0 on land), elmos.
    pub fn water_depth(&self, p: Vec3) -> f32 {
        let (x, y) = cell(p);
        let w = &self.water;
        if x < 0 || y < 0 || x >= w.width {
            return 0.0;
        }
        w.depth.get((x + y * w.width) as usize).copied().unwrap_or(0.0)
    }

    /// From what depth units of type `def` are all under water.
    pub fn dive(&self, def: Option<UnitDef>) -> Dive {
        def.and_then(|d| self.water.profile(d)).map_or(Dive::SURFACE, |p| p.dive)
    }

    /// What units of type `def` hit.
    pub fn reach(&self, def: Option<UnitDef>) -> Reach {
        def.and_then(|d| self.water.profile(d)).map_or(Reach::SURFACE, |p| p.reach)
    }

    /// Strength of the water weapons of units of type `def`.
    pub fn deep_power(&self, def: Option<UnitDef>) -> f32 {
        def.and_then(|d| self.water.profile(d)).map_or(0.0, |p| p.deep_power)
    }

    /// Whether units hitting what `reach` says can hit enemy `e`.
    pub fn can_hit(&self, reach: Reach, e: &Enemy) -> bool {
        if e.is_submerged() { reach.under_water } else { reach.dry_land || self.water_depth(e.position) > 0.0 }
    }

    /// The threat maps as units of type `def` see them.
    pub fn view(&self, def: Option<UnitDef>) -> ThreatView<'_> {
        ThreatView { war: self, dive: self.dive(def) }
    }

    /// The threat maps as units that are under water from depth `dive` on
    /// see them.
    pub fn view_dive(&self, dive: Dive) -> ThreatView<'_> {
        ThreatView { war: self, dive }
    }

    /// Enemy threat to things under water at `p`.
    pub fn get_sub_threat(&self, p: Vec3) -> f32 {
        let (x, y) = cell(p);
        self.sub_threat_graphics.get_value(x, y) / 500.0
    }
}

impl Ai {
    /// On a wet map, log the profiles of some types once, and then every
    /// minute where our fighters are (dry, wading, under water), how many
    /// enemies were seen under water, and how many of ours under water the
    /// surface threat alone would have chased off ("spared").
    pub fn water_update(&mut self, frame: i32) {
        if !self.war.water.wet || frame % STATS_PERIOD != (self.offset + 17) % STATS_PERIOD {
            return;
        }
        let war = &self.war;
        if frame < STATS_PERIOD {
            for name in LOGGED {
                let Some(def) = self.defs.get(name) else { continue };
                let p = war.water.profile(def).copied().unwrap_or(Profile {
                    dive: Dive::SURFACE,
                    reach: Reach::SURFACE,
                    deep_power: 0.0,
                });
                self.log(&format!(
                    "water: {name} dives at {:.0} (height {:.0}, radius {:.0}), hits under water {}, dry land {}, \
                     deep power {:.0}",
                    p.dive.0,
                    def.height(),
                    def.radius(),
                    p.reach.under_water,
                    p.reach.dry_land,
                    p.deep_power
                ));
            }
        }
        let (mut dry, mut wading, mut submerged, mut spared) = (0, 0, 0, 0);
        // Divers the depth grid and the engine's rule (middle plus radius
        // below the surface) call submerged: both, only the grid, only the
        // engine.
        let (mut agree, mut grid_only, mut live_only) = (0, 0, 0);
        for f in self.fighters.values() {
            let pos = f.get_pos();
            let view = war.view(f.unit.def());
            if let Some(def) = f.unit.def().filter(|_| view.dive().0.is_finite() && view.dive().0 > 0.0) {
                let grid = view.medium(pos) == Medium::Submerged;
                let live = pos.y + def.radius() < 0.0;
                match (grid, live) {
                    (true, true) => agree += 1,
                    (true, false) => grid_only += 1,
                    (false, true) => live_only += 1,
                    _ => {}
                }
            }
            match view.medium(pos) {
                Medium::Dry => dry += 1,
                Medium::Wading => wading += 1,
                Medium::Submerged => {
                    submerged += 1;
                    if war.get_threat(pos) > 0.0 && view.get_threat(pos) == 0.0 {
                        spared += 1;
                    }
                }
            }
        }
        let recent = |e: &&Enemy| frame - e.last_seen < STATS_PERIOD;
        let enemies = war.targets.values().filter(recent).filter(|e| e.is_submerged()).count();
        let deep_only = (war.targets.values().filter(recent))
            .filter(|e| e.deep_danger() > 0.0 && e.surface_danger(e.is_submerged()) == 0.0)
            .count();
        self.log(&format!(
            "water: fighters {dry} dry, {wading} wading, {submerged} under water ({spared} spared); \
             enemies seen under water {enemies}, threatening only under water {deep_only}; \
             divers under water by grid and engine {agree}, grid only {grid_only}, engine only {live_only}"
        ));
    }
}

/// The threat maps as one type of unit sees them: where it is under water,
/// only what hits under water (enemy and ours) counts; where it wades or
/// floats, torpedoes count too. The methods mirror [`MilitaryManager`]'s.
#[derive(Clone, Copy)]
pub struct ThreatView<'a> {
    war: &'a MilitaryManager,
    dive: Dive,
}

impl ThreatView<'_> {
    /// From what depth the viewer is all under water.
    pub fn dive(&self) -> Dive {
        self.dive
    }

    pub fn medium(&self, p: Vec3) -> Medium {
        self.dive.medium(self.war.water_depth(p))
    }

    pub fn get_threat(&self, p: Vec3) -> f32 {
        let war = self.war;
        match self.medium(p) {
            Medium::Dry => war.get_threat(p),
            Medium::Wading => war.get_threat(p) + war.get_sub_threat(p),
            Medium::Submerged => war.get_sub_threat(p),
        }
    }

    /// Riot weapons (shotguns, flamers) do not reach under water.
    pub fn get_riot_threat(&self, p: Vec3) -> f32 {
        if self.medium(p) == Medium::Submerged { 0.0 } else { self.war.get_riot_threat(p) }
    }

    pub fn get_porc_threat(&self, p: Vec3) -> f32 {
        let war = self.war;
        let (x, y) = cell(p);
        let sub = war.sub_porc_graphics.get_value(x, y) / 500.0;
        match self.medium(p) {
            Medium::Dry => war.get_porc_threat(p),
            Medium::Wading => war.get_porc_threat(p) + sub,
            Medium::Submerged => sub,
        }
    }

    pub fn get_effective_threat(&self, p: Vec3) -> f32 {
        let war = self.war;
        match self.medium(p) {
            Medium::Dry => war.get_effective_threat(p),
            Medium::Wading => war.get_effective_threat(p) + war.get_sub_threat(p),
            Medium::Submerged => war.get_sub_threat(p) - self.get_friendly_threat(p),
        }
    }

    pub fn get_tactical_threat(&self, p: Vec3) -> f32 {
        let war = self.war;
        match self.medium(p) {
            Medium::Dry => war.get_tactical_threat(p),
            Medium::Wading => war.get_tactical_threat(p) + war.get_sub_threat(p),
            Medium::Submerged => war.get_sub_threat(p),
        }
    }

    pub fn get_friendly_threat(&self, p: Vec3) -> f32 {
        match self.medium(p) {
            Medium::Submerged => {
                let (x, y) = cell(p);
                self.war.ally_sub_graphics.get_value(x, y) / 500.0
            }
            _ => self.war.get_friendly_threat(p),
        }
    }

    pub fn get_friendly_raider_threat(&self, p: Vec3) -> f32 {
        match self.medium(p) {
            Medium::Submerged => self.get_friendly_threat(p),
            _ => self.war.get_friendly_raider_threat(p),
        }
    }

    pub fn get_total_friendly_threat(&self, p: Vec3) -> f32 {
        match self.medium(p) {
            Medium::Submerged => self.get_friendly_threat(p),
            _ => self.war.get_total_friendly_threat(p),
        }
    }

    /// Where enemy ground units stand (cloak bumps), as for anyone.
    pub fn get_scythe_threat(&self, p: Vec3) -> f32 {
        self.war.get_scythe_threat(p)
    }

    /// Anti-air, as for anyone (it does not reach under water, but what
    /// asks for it flies).
    pub fn get_aa_threat(&self, p: Vec3) -> f32 {
        self.war.get_aa_threat(p)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn divers_go_under_at_their_depth() {
        let duck = Dive(20.0);
        assert_eq!(duck.medium(0.0), Medium::Dry);
        assert_eq!(duck.medium(12.0), Medium::Wading);
        assert_eq!(duck.medium(20.0), Medium::Submerged);
        assert_eq!(Dive(0.0).medium(5.0), Medium::Submerged);
        assert_eq!(Dive(0.0).medium(0.0), Medium::Dry);
        assert_eq!(Dive::SURFACE.medium(500.0), Medium::Wading);
        assert_eq!(Dive::AIR.medium(500.0), Medium::Dry);
    }
}
