//! Tactical missiles. A silo (`staticmissilesilo`: four pads, 1200 metal)
//! goes up where it is safe and within Eos range of more than one enemy
//! building worth a missile (a Fusion and up), or of an anti-nuke in the
//! way of our nukes. Coverage of more than 70% of known and inferred static
//! value is another trigger once observed battlefield density exceeds
//! `missile_silo_density`, discounted on lane-like terrain. Constructors rush it once
//! threatened sites have completed their defenses (see `economy::silo`).
//! Its Eos (`tacnuke`: 600 metal, 3502 damage,
//! 3500 range; anti-nukes do not stop them, shields do) go at those
//! buildings, as many as it takes each. Its Shockleys (`empmissile`: stun
//! for up to 45 s, longer than a nuke's flight) go at the anti-nukes in the
//! way of a loaded nuke, fired together with it (see `nukes`). With
//! nothing left in reach for those, its Infernos (`napalmmissile`: 500
//! metal) roast clusters of light things (see [`LIGHT`]) out of shields:
//! mexes in a cluster, wind generators, caretaker farms, picket turrets,
//! and whatever stands with them, or crowds on radar we cannot make out
//! (the burning ground keeps their builders from repairing them).
//! Two identified Phantoms together also warrant an Inferno, provided
//! neither is covered by an Aspis, Aegis or Funnelweb.
//!
//! The AI hears of no death it does not see, and an Eos is gone from the
//! game once launched: a building an Eos hit (all it took, its shield
//! drained first) is counted gone once the missile is due, unless it is
//! seen standing. An anti-nuke we struck (with Eos, or shelled with
//! Berthas) is not: a nuke flown at it on a guess costs too much. It stays
//! on the books until a look tells (see `scouts`).

use std::collections::BTreeMap;

use springai::{NO_TIMEOUT, Unit, UnitDef, Vec3};

use crate::Ai;
use crate::terrain::HELD_POWER;
use crate::util::{NULLPOS, distance, get_directional_point};

pub const SILO: &str = "staticmissilesilo";
const EOS: &str = "tacnuke";
const SHOCKLEY: &str = "empmissile";
const INFERNO: &str = "napalmmissile";

/// What the silo builds: units on its pads until fired.
pub const MISSILES: [&str; 5] = ["tacnuke", "empmissile", "napalmmissile", "seismic", "missileslow"];

const PADS: usize = 4;

/// Eos and Shockley range (3500), less a margin for the pads.
pub const REACH: f32 = 3450.0;

/// Silo sites are tried at our metal and this far ahead of it, elmos.
const SITE_STEPS: [f32; 3] = [400.0, 800.0, 1200.0];
const STATIC_SHARE: f32 = 0.7;
const DEFAULT_SIEGE_DENSITY: f32 = 1500.0;
const MAX_SIEGE_DENSITY: f32 = 10000.0;
/// A lost silo or nanoframe is evidence against the whole nearby site,
/// not just its exact footprint. Match the former safe-site policy.
const LOSS_MEMORY: i32 = 30 * 300;
const LOSS_RADIUS: f32 = 800.0;

/// A final buildable firing position and the reasons it pays.
pub struct SiloSite {
    pub pos: Vec3,
    pub payoff: f32,
    pub targets: usize,
    pub coverage: StaticCoverage,
    pub siege: SiegeReadiness,
}

#[derive(Clone, Copy, Default)]
pub struct StaticCoverage {
    pub value: f32,
    pub share: f32,
}

impl StaticCoverage {
    fn warrants_silo(self, siege: SiegeReadiness) -> bool {
        self.share > STATIC_SHARE && siege.density > siege.threshold
    }
}

/// Progress towards siege, independent of the value inside a silo's range.
/// The lane discount is a bounded tuning choice, not a fitted silo model.
#[derive(Clone, Copy)]
pub struct SiegeReadiness {
    pub density: f32,
    pub threshold: f32,
    pub laneness: f32,
}

impl SiegeReadiness {
    fn new(density: f32, minimum: f32, laneness: f32) -> Self {
        let laneness = laneness.clamp(0.0, 1.0);
        Self { density, threshold: minimum / (1.0 + 0.5 * laneness), laneness }
    }
}

fn siege_density_option(value: Option<&str>) -> f32 {
    value
        .and_then(|s| s.parse::<f32>().ok())
        .filter(|v| v.is_finite())
        .map_or(DEFAULT_SIEGE_DENSITY, |v| v.clamp(0.0, MAX_SIEGE_DENSITY))
}

const EOS_DAMAGE: f32 = 3502.0;

/// Value per missile that makes a target worth an Eos (600 metal): a
/// Fusion's.
const WORTH: f32 = 1000.0;

/// What a silo and a missile cost: a silo is worth it where what is in
/// reach pays for both.
const SILO_COST: f32 = 1200.0;
const MISSILE_COST: f32 = 600.0;

/// Income from which a silo and its missiles are affordable.
pub const SILO_INCOME: f32 = 30.0;

/// Enemy shields stop an Eos: an Aegis or Aspis network (3600 power each,
/// radius 350) takes one more, fired first, to drain it below what the
/// rest needs to get through; a Funnelweb's (19400, radius 550) takes too
/// many, and it alone stops Shockleys too.
const SHIELDS: [&str; 2] = ["staticshield", "shieldshield"];
const SHIELD_RADIUS: f32 = 350.0;
const FUNNELWEB: &str = "striderfunnelweb";
const FUNNELWEB_RADIUS: f32 = 550.0;

/// Frames between the Eos that drains a shield and those that follow.
const STAGGER: i32 = 60;

/// A struck target is left alone until seen standing this long after the
/// strike; a struck anti-nuke not seen for this long wants a look. Strikes
/// are forgotten after a while.
const LOOK_AFTER: i32 = 30 * 20;
const STRUCK_MEMORY: i32 = 30 * 600;

/// An Eos's flight: three seconds up, then 1200 elmos/s (and a margin).
const EOS_RISE: f32 = 5.0;
const EOS_SPEED: f32 = 1000.0;

const CMD_ATTACK: i32 = 20;

/// An Inferno burns what is within this of its burst: 151 damage on
/// impact, fire, and more on the burning ground for 45 s. What stays in it
/// (buildings, units standing still) takes about `BURN_STAYING` (measured:
/// one Inferno burnt four 500-health caretakers, and twelve wind
/// generators; it took two for ten 600-health Solars and a mex, the first
/// killing three), what walks out `BURN_PASSING`. It is for light
/// things, up to `LIGHT` health, this many together at least, and worth it
/// where what it burns comes to `INFERNO_WORTH`: each thing's value by the
/// share of its health left that the fire takes (a mex's or an energy
/// building's `ECONOMY` times: its income goes with it), and `NUISANCE`
/// for each thing in the fire besides. So four caretakers are worth it
/// (three are not), seven wind generators, three mexes with a picket and a
/// wind generator, or ten contacts we cannot make out standing together.
const INFERNO_RADIUS: f32 = 256.0;
const BURN_STAYING: f32 = 520.0;
const BURN_PASSING: f32 = 150.0;
const LIGHT: f32 = 2000.0;
const MIN_CLUSTER: usize = 3;
const PHANTOM: &str = "cloaksnipe";
const INFERNO_WORTH: f32 = 600.0;
const ECONOMY: f32 = 2.0;
const NUISANCE: f32 = 20.0;

/// The health a contact of unknown type is taken to have, and how high
/// above the ground one flies (it is not burnt), elmos.
const UNKNOWN_HEALTH: f32 = 500.0;
const FLYING: f32 = 60.0;

/// A unit seen this lately (frames), moving slower than this (elmos a
/// frame), stays in the fire.
const INFERNO_SEEN: i32 = 30 * 3;
const STILL: f32 = 0.5;

/// Infernos on the pads at most (the rest are kept for Eos and
/// Shockleys), and how long a place burnt is left alone, and its ground
/// burns: once its fire is out, what it should have burnt and was not
/// seen since counts as gone (seen while it burns, it may yet die).
const MAX_INFERNOS: usize = 2;
const BURN_FRAMES: i32 = 30 * 45;

/// Frames between reports on what Infernos would burn.
const INFERNO_REPORT: i32 = 30 * 120;

/// Frames between reports of why there is no silo.
const REPORT_FRAMES: i32 = 30 * 300;

#[derive(Default)]
pub struct Missiles {
    silo: Option<Unit>,
    /// Missiles on the pads.
    ready: Vec<Unit>,
    /// Enemy buildings struck, since when.
    struck: BTreeMap<i32, i32>,
    /// When a silo was last planned (a plan dropped is not remade at once).
    pub planned: Option<i32>,
    lost_sites: Vec<(Vec3, i32)>,
    /// Eos to follow those that drained shields: at whom, where, how many,
    /// from when.
    followups: Vec<(i32, Vec3, usize, i32)>,
    /// Buildings all the Eos they take are flying at: which, and when the
    /// last is due.
    strikes: Vec<(i32, i32)>,
    /// Where Infernos burst, and when.
    burnt: Vec<(Vec3, i32)>,
    /// What Infernos did to enemies (by id), as far as we know: damage,
    /// and when the fire is out (from when what is seen of them counts).
    burns: BTreeMap<i32, (f32, i32)>,
    last_report: i32,
    last_inferno_report: i32,
}

impl Missiles {
    fn note_loss(&mut self, pos: Vec3, frame: i32) {
        self.lost_sites.retain(|&(_, at)| frame - at < LOSS_MEMORY);
        self.lost_sites.push((pos, frame));
    }

    fn shuns(&self, pos: Vec3, frame: i32) -> bool {
        self.lost_sites.iter().any(|&(site, at)| frame - at < LOSS_MEMORY && distance(pos, site) < LOSS_RADIUS)
    }
}

/// An enemy building worth Eos: how many it takes.
struct Target {
    id: i32,
    pos: Vec3,
    value: f32,
    needed: usize,
    /// Under an Aegis or Aspis: the first Eos drains it, the rest follow.
    shielded: bool,
}

struct LightTarget {
    id: i32,
    pos: Vec3,
    value: f32,
    burn: f32,
    /// Only identified Phantoms outside every known shield count toward
    /// the two-sniper exception. A shield over either sniper rules it out.
    unshielded_phantom: bool,
}

/// Geometric candidates; range, shields over the aim point, friendly units
/// and existing fires are checked by the caller before firing.
fn inferno_cluster_candidates(light: &[LightTarget], worth: f32) -> Vec<(Vec3, f32, usize)> {
    let within = |c: Vec3| light.iter().filter(move |l| distance(l.pos, c) <= INFERNO_RADIUS);
    let mut clusters = Vec::new();
    let mut consider = |mid| {
        let (burnt, n, snipers) =
            within(mid).fold((0.0, 0, 0), |(v, n, s), l| (v + l.value, n + 1, s + usize::from(l.unshielded_phantom)));
        if (n >= MIN_CLUSTER && burnt > worth) || snipers >= 2 {
            clusters.push((mid, burnt, n));
        }
    };
    let phantoms: Vec<_> = light.iter().filter(|l| l.unshielded_phantom).collect();
    // Two snipers can share a blast without either being within a blast
    // radius of the other. Include their midpoint as an aim candidate.
    for (i, a) in phantoms.iter().enumerate() {
        for b in &phantoms[i + 1..] {
            if distance(a.pos, b.pos) <= 2.0 * INFERNO_RADIUS {
                consider(Vec3::xz((a.pos.x + b.pos.x) / 2.0, (a.pos.z + b.pos.z) / 2.0));
            }
        }
    }
    for c in light.iter().map(|l| l.pos) {
        let n = within(c).count();
        if n < 2 {
            continue;
        }
        let (x, z) = within(c).fold((0.0, 0.0), |(x, z), l| (x + l.pos.x / n as f32, z + l.pos.z / n as f32));
        consider(Vec3::xz(x, z));
    }
    clusters
}

/// Allied nanoframes count as commitments too. Our own silos must never
/// suppress our targets; destroyed or transferred silos cease to cover them.
fn allied_silos(units: impl Iterator<Item = Unit>, team: i32, silo: UnitDef, own: Option<Unit>) -> Vec<Vec3> {
    units
        .filter(|u| u.health() > 0.0 && u.team() != team && u.def() == Some(silo))
        // Once both bots have silos, the lower unit id owns overlapping
        // targets. Otherwise both would defer forever to each other.
        .filter(|u| own.is_none_or(|s| u.id() < s.id()))
        .map(|u| u.pos())
        .collect()
}

fn ally_covers(silos: &[Vec3], target: Vec3) -> bool {
    silos.iter().any(|&p| distance(p, target) <= REACH)
}

impl Ai {
    fn missiles_allied_silos(&self, own: Option<Unit>) -> Vec<Vec3> {
        let Some(silo) = self.defs.get(SILO) else { return Vec::new() };
        allied_silos(self.cb.friendly_units().into_iter(), self.team_id, silo, own)
    }

    pub(crate) fn missiles_note_loss(&mut self, pos: Vec3) {
        self.missiles.note_loss(pos, self.frame);
        self.log(&format!(
            "missiles: silo lost at ({:.0}, {:.0}); excluding sites within {LOSS_RADIUS:.0} for {} seconds",
            pos.x,
            pos.z,
            LOSS_MEMORY / 30
        ));
    }

    pub(crate) fn missiles_shuns_site(&self, pos: Vec3) -> bool {
        self.missiles.shuns(pos, self.frame)
    }

    pub(crate) fn missiles_siege_readiness(&self) -> SiegeReadiness {
        let minimum = siege_density_option(self.cb.option("missile_silo_density").as_deref());
        SiegeReadiness::new(self.posture.density, minimum, self.posture.laneness())
    }

    pub fn missiles_silo(&self) -> Option<Unit> {
        self.missiles.silo.filter(|s| s.health() > 0.0)
    }

    pub fn missiles_add_silo(&mut self, silo: Unit) {
        self.missiles.silo = Some(silo);
        let p = silo.pos();
        self.log(&format!("missiles: silo up at ({:.0}, {:.0})", p.x, p.z));
        self.eco_fortify_silo(silo);
    }

    /// A missile on a pad: ours to fire.
    pub fn missiles_add_missile(&mut self, missile: Unit) {
        missile.set_fire_state(0, 0);
        self.missiles.ready.push(missile);
    }

    /// Whether a known shield covers `p`: an Aegis or Aspis (`Some(false)`)
    /// or a Funnelweb (`Some(true)`).
    fn missiles_shield(&self, p: Vec3) -> Option<bool> {
        let live = self.war.targets.values().filter(|e| !e.is_dead);
        let mut light = false;
        for e in live {
            let d = distance(e.position, p);
            if e.def_name == FUNNELWEB && d <= FUNNELWEB_RADIUS {
                return Some(true);
            }
            light |= SHIELDS.contains(&e.def_name.as_str()) && d <= SHIELD_RADIUS;
        }
        light.then_some(false)
    }

    /// Enemy buildings worth Eos: unshielded, not struck lately (unless
    /// seen standing since).
    fn missiles_eos_targets(&self) -> Vec<Target> {
        let allies = self.missiles_allied_silos(self.missiles_silo());
        let struck = |id: i32, seen: i32| self.missiles.struck.get(&id).is_some_and(|&t| seen < t + LOOK_AFTER);
        (self.war.targets.values())
            .filter(|e| e.identified && e.is_static && !e.is_dead && !struck(e.unit_id, e.last_seen))
            .filter(|e| !ally_covers(&allies, e.position))
            .filter_map(|e| {
                let needed = (e.ud?.health() / EOS_DAMAGE).ceil().max(1.0) as usize;
                let shielded = match self.missiles_shield(e.position) {
                    Some(true) => return None,
                    Some(false) => true,
                    None => false,
                };
                let needed = needed + usize::from(shielded);
                let t = Target { id: e.unit_id, pos: e.position, value: e.value, needed, shielded };
                (e.value / needed as f32 >= WORTH && needed <= PADS).then_some(t)
            })
            .collect()
    }

    /// Known anti-nukes guarding something from our nukes (their value is
    /// more than their cost), shielded or not: what Shockleys are for.
    fn missiles_stun_targets(&self) -> Vec<(i32, Vec3, f32)> {
        if !self.misc_has_nuke() {
            return Vec::new();
        }
        let allies = self.missiles_allied_silos(self.missiles_silo());
        (self.war.targets.values())
            .filter(|e| !e.is_dead && e.def_name == "staticantinuke")
            .filter(|e| !ally_covers(&allies, e.position))
            .filter(|e| e.ud.is_some_and(|ud| e.value > ud.cost(self.metal)))
            .filter(|e| self.missiles_shield(e.position) != Some(true))
            .map(|e| (e.unit_id, e.position, e.value))
            .collect()
    }

    /// Clusters of light enemies an Inferno would roast, out of shields,
    /// clear of our units and not burning already: where, what it would
    /// burn (see [`INFERNO_WORTH`]) and how many things, the best first,
    /// none overlapping a better one.
    fn missiles_inferno_targets(&self) -> Vec<(Vec3, f32, usize)> {
        let mut targets = self.missiles_clusters(INFERNO_WORTH);
        // A recent multi-Phantom fix enables a broad-area Inferno from an
        // existing silo. It does not by itself commission another silo.
        if self.missiles_silo().is_some() && !self.dev_off("strikes") {
            for (mut pos, count) in self.strikes_phantom_targets() {
                if count < 2
                    || self.missiles_ground_burning(pos)
                    || self.missiles_shield(pos).is_some()
                    || self
                        .cb
                        .friendly_units_in(pos, INFERNO_RADIUS + 100.0)
                        .iter()
                        .any(|u| !u.def().is_some_and(|d| d.is_able_to_fly()))
                    || targets.iter().any(|t| distance(t.0, pos) < 2.0 * INFERNO_RADIUS)
                {
                    continue;
                }
                pos.y = self.cb.map().elevation_at(pos.x, pos.z);
                let cost = self.defs.get("cloaksnipe").map_or(750.0, |d| d.cost(self.metal));
                targets.push((pos, count as f32 * cost, count));
            }
        }
        let allies = self.missiles_allied_silos(self.missiles_silo());
        targets.retain(|t| !ally_covers(&allies, t.0));
        targets.sort_by(|a, b| b.1.total_cmp(&a.1));
        targets
    }

    pub(crate) fn missiles_ground_burning(&self, pos: Vec3) -> bool {
        self.missiles
            .burnt
            .iter()
            .any(|(p, at)| self.frame - *at < BURN_FRAMES + 30 * 10 && distance(*p, pos) < INFERNO_RADIUS + 100.0)
    }

    /// Light enemies an Inferno would burn: their ids, where they are, what
    /// burning them is worth (see [`INFERNO_WORTH`]) and how much damage it
    /// would do. Contacts of unknown type count as light; what our fires
    /// should have burnt already, unseen since, does not.
    fn missiles_light(&self) -> Vec<LightTarget> {
        let frame = self.frame;
        let map = self.cb.map();
        (self.war.targets.values())
            .filter(|e| !e.is_dead && !e.can_fly() && !e.is_nano_spam && e.position != NULLPOS)
            .filter_map(|e| {
                let known = e.ud.is_some();
                let health = if known { e.def_health() } else { UNKNOWN_HEALTH };
                if health <= 0.0 || health > LIGHT {
                    return None;
                }
                if !known && e.position.y - map.elevation_at(e.position.x, e.position.z).max(0.0) > FLYING {
                    return None;
                }
                let burn = if e.is_static && known {
                    BURN_STAYING
                } else if frame - e.last_seen > INFERNO_SEEN {
                    return None;
                } else if e.unit.vel().x.hypot(e.unit.vel().z) < STILL {
                    BURN_STAYING
                } else {
                    BURN_PASSING
                };
                // Less what our fires did since it was last seen.
                let mut left = e.get_health() * health;
                if let Some(&(done, _)) = self.missiles.burns.get(&e.unit_id).filter(|b| e.last_seen < b.1) {
                    left -= done;
                }
                if left <= 0.0 {
                    return None;
                }
                let economy = e.is_mex || e.def_name.starts_with("energy");
                let value = if economy { ECONOMY * e.value } else { e.value };
                Some(LightTarget {
                    id: e.unit_id,
                    pos: e.position,
                    value: value * (burn / left).min(1.0) + NUISANCE,
                    burn,
                    unshielded_phantom: e.identified
                        && e.def_name == PHANTOM
                        && self.missiles_shield(e.position).is_none(),
                })
            })
            .collect()
    }

    /// [`missiles_inferno_targets`](Self::missiles_inferno_targets), of
    /// those burning more than `worth`, or at least two exposed Phantoms.
    fn missiles_clusters(&self, worth: f32) -> Vec<(Vec3, f32, usize)> {
        let light = self.missiles_light();
        let burning = |p: Vec3| self.missiles.burnt.iter().any(|b| distance(b.0, p) <= INFERNO_RADIUS);
        // None of ours but aircraft in the fire.
        let clear = |p: Vec3| {
            let ours = self.cb.friendly_units_in(p, INFERNO_RADIUS + 100.0);
            ours.iter().all(|u| u.def().is_some_and(|d| d.is_able_to_fly()))
        };
        let mut clusters = inferno_cluster_candidates(&light, worth);
        clusters.sort_by(|a, b| b.1.total_cmp(&a.1));
        let mut chosen: Vec<(Vec3, f32, usize)> = Vec::new();
        for mut c in clusters {
            c.0.y = self.cb.map().elevation_at(c.0.x, c.0.z);
            if chosen.iter().all(|o| distance(o.0, c.0) > 2.0 * INFERNO_RADIUS)
                && !burning(c.0)
                && self.missiles_shield(c.0).is_none()
                && clear(c.0)
            {
                chosen.push(c);
            }
        }
        chosen
    }

    /// Where a silo would do most: a safe place on our ground within reach
    /// of enough to pay for it and its missiles (three Fusions, one
    /// Singularity or anti-nuke; an anti-nuke a Shockley would stun counts
    /// for what it keeps from our nukes), or covering most static value at
    /// sufficient density. Returns the actual buildable site and its value.
    pub fn missiles_site(&self) -> Option<SiloSite> {
        let eos = self.missiles_eos_targets();
        let stun = self.missiles_stun_targets();
        let allies = self.missiles_allied_silos(None);
        let statics = self.missiles_static_value();
        if eos.is_empty() && stun.is_empty() && statics.is_empty() {
            return None;
        }
        let enemy = self.graph.get_enemy_center();
        let mut sites: Vec<Vec3> = self.graph.get_owned_spots().iter().map(|m| m.get_pos()).collect();
        sites.push(self.graph.get_ally_center());
        // And ahead of our metal, towards the enemy.
        let ahead: Vec<Vec3> =
            (sites.iter()).flat_map(|&p| SITE_STEPS.iter().map(move |&d| get_directional_point(p, enemy, d))).collect();
        sites.extend(ahead);
        let silo = self.defs.get(SILO)?;
        // Resolve the engine's footprint/grid adjustment BEFORE scoring range
        // and safety. The economy must queue this exact position unchanged.
        let sites = sites.into_iter().map(|p| self.cb.map().find_closest_build_site(silo, p, 600.0, 3, 0));
        let sites = sites.filter(|&p| {
            p.x >= 0.0
                && !self.missiles_shuns_site(p)
                && self.eco_build_check(silo, p, 0)
                && self.graph.is_ally_territory(p)
                && (self.war.get_threat(p) <= 0.0
                    || (self.war.get_effective_threat(p) <= 0.0 && self.war.get_friendly_porc_threat(p) >= HELD_POWER))
        });
        let mut targets: Vec<_> = eos.iter().map(|t| (t.pos, t.value - MISSILE_COST * t.needed as f32)).collect();
        targets.extend(stun.iter().filter(|a| !eos.iter().any(|t| t.id == a.0)).map(|a| (a.1, a.2 - MISSILE_COST)));
        choose_silo_site(&allies, sites, &targets, &statics, self.missiles_siege_readiness(), enemy)
    }

    /// Remembered live statics plus spatially inferred mexes. A shadowed
    /// spot carries the same conservative confidence as the income model;
    /// visible empty spots and mexes already in the contact book add nothing.
    /// Unlocated economic residuals cannot establish missile-range coverage.
    fn missiles_static_value(&self) -> Vec<(Vec3, f32)> {
        let mut statics: Vec<_> = self
            .war
            .targets
            .values()
            .filter(|e| !e.is_dead && e.is_static && !e.is_nano_spam && e.position != NULLPOS)
            .map(|e| (e.position, e.value))
            .collect();
        let Some(mex) = self.defs.get("staticmex") else { return statics };
        let cost = mex.cost(self.metal);
        for spot in &self.graph.metal_spots {
            if spot.owned || spot.visible || !(spot.hostile || spot.enemy_shadowed) {
                continue;
            }
            let known = self
                .war
                .targets
                .values()
                .any(|e| !e.is_dead && e.is_mex && distance(e.position, spot.position) < 100.0);
            if !known {
                let confidence = if spot.hostile { 1.0 } else { super::intel::INFERRED_WEIGHT };
                statics.push((spot.position, cost * confidence));
            }
        }
        statics
    }

    /// Say now and then why there is no silo though we could afford one.
    pub fn missiles_report_no_site(&mut self) {
        if self.frame - self.missiles.last_report < REPORT_FRAMES {
            return;
        }
        self.missiles.last_report = self.frame;
        let targets = self.missiles_eos_targets();
        let ours = self.graph.get_owned_spots().iter().map(|m| m.get_pos()).collect::<Vec<_>>();
        let nearest = (targets.iter())
            .flat_map(|t| ours.iter().map(move |&p| distance(p, t.pos)))
            .reduce(f32::min)
            .map_or("none".to_owned(), |d| format!("{d:.0}"));
        let value: f32 = targets.iter().map(|t| t.value).sum();
        let siege = self.missiles_siege_readiness();
        let static_value: f32 = self.missiles_static_value().iter().map(|t| t.1).sum();
        self.log(&format!(
            "missiles: no silo site: {} targets worth Eos ({value:.0} metal), the nearest {nearest} from our metal \
             (reach {REACH:.0}); {static_value:.0} known/inferred static metal, coverage needs >70%; \
             battlefield density {:.0}, needs >{:.0} metal/Melmo² (laneness {:.2})",
            targets.len(),
            siege.density,
            siege.threshold,
            siege.laneness
        ));
    }

    /// Say now and then what an Inferno would find to burn.
    fn missiles_report_infernos(&mut self, silo: Unit) {
        if self.frame - self.missiles.last_inferno_report < INFERNO_REPORT {
            return;
        }
        self.missiles.last_inferno_report = self.frame;
        let here = silo.pos();
        let best = (self.missiles_clusters(0.0).into_iter())
            .filter(|c| distance(c.0, here) <= REACH)
            .max_by(|a, b| a.1.total_cmp(&b.1));
        let pads = self.missiles_ready(INFERNO).len();
        let queued = silo.current_commands().iter().filter(|c| c.id() < 0).count();
        let energy_gate = if self.eco.hard_estalled { "energy shortage" } else { "open" };
        let best = best.map_or("none".to_owned(), |(p, v, n)| {
            format!("{v:.0} metal in {n} light things at ({:.0}, {:.0})", p.x, p.z)
        });
        self.log(&format!(
            "missiles: Infernos: {pads} on the pads; best cluster in reach {best} \
             (fire above {INFERNO_WORTH:.0} or at two exposed Phantoms); \
             restock energy gate: {energy_gate}, {} missiles ready, {queued} queued",
            self.missiles.ready.len()
        ));
    }

    pub fn missiles_update(&mut self, frame: i32) {
        if (frame + self.offset) % 90 != 0 {
            return;
        }
        self.missiles.ready.retain(|m| m.health() > 0.0);
        self.missiles.struck.retain(|_, t| frame - *t < STRUCK_MEMORY);
        self.missiles.burnt.retain(|b| frame - b.1 < BURN_FRAMES);
        self.missiles_settle_strikes();
        self.missiles_settle_burns();
        let Some(silo) = self.missiles_silo() else { return };
        self.missiles_fire_eos();
        self.missiles_fire_infernos();
        self.missiles_report_infernos(silo);
        self.missiles_restock(silo);
    }

    fn missiles_ready(&self, kind: &str) -> Vec<Unit> {
        (self.missiles.ready.iter().copied()).filter(|m| self.defs.name_of(*m) == kind && !m.is_being_built()).collect()
    }

    /// Fire the Eos on the pads at the best targets in reach, as many at
    /// each as it takes.
    fn missiles_fire_eos(&mut self) {
        let mut free = self.missiles_ready(EOS);
        if free.is_empty() {
            return;
        }
        let frame = self.frame;
        // Those following Eos that drained shields first, then the rest
        // (keeping enough for the follow-ups to come).
        let followups = std::mem::take(&mut self.missiles.followups);
        for (id, p, n, after) in followups {
            let near: Vec<usize> = (0..free.len()).filter(|&i| distance(free[i].pos(), p) <= REACH).collect();
            if after > frame || near.len() < n {
                self.missiles.followups.push((id, p, n, after));
                continue;
            }
            for &i in near.iter().take(n).rev() {
                let m = free.remove(i);
                self.missiles_launch(m, p);
            }
            self.missiles_strike_due(id, p);
        }
        let reserved: usize = self.missiles.followups.iter().map(|f| f.2).sum();
        let mut targets = self.missiles_eos_targets();
        targets.retain(|t| free.iter().any(|m| distance(m.pos(), t.pos) <= REACH));
        targets.sort_by(|a, b| (b.value / b.needed as f32).total_cmp(&(a.value / a.needed as f32)));
        for t in targets {
            if free.len() < t.needed + reserved {
                continue;
            }
            let now = if t.shielded { 1 } else { t.needed };
            for m in free.drain(..now).collect::<Vec<_>>() {
                self.missiles_launch(m, t.pos);
            }
            if t.shielded {
                self.missiles.followups.push((t.id, t.pos, t.needed - 1, frame + STAGGER));
            } else {
                self.missiles_strike_due(t.id, t.pos);
            }
            self.missiles.struck.insert(t.id, frame);
            let name = self.war.targets.get(&t.id).map_or("?", |e| e.def_name.as_str());
            let shield = if t.shielded { ", the first to drain its shield" } else { "" };
            self.log(&format!(
                "missiles: {} Eos at a {name} worth {:.0} at ({:.0}, {:.0}){shield}",
                t.needed, t.value, t.pos.x, t.pos.z
            ));
        }
    }

    /// Fire the Infernos on the pads at the clusters in reach, one each.
    fn missiles_fire_infernos(&mut self) {
        let mut free = self.missiles_ready(INFERNO);
        if free.is_empty() {
            return;
        }
        for (p, value, n) in self.missiles_inferno_targets() {
            let Some(i) = free.iter().position(|m| distance(m.pos(), p) <= REACH) else { continue };
            let m = free.remove(i);
            // When its fire is out (an Eos's flight is taken for its own).
            let frame = self.frame;
            let due = frame + (30.0 * (EOS_RISE + distance(m.pos(), p) / EOS_SPEED)) as i32 + BURN_FRAMES;
            self.missiles_launch(m, p);
            self.missiles.burnt.push((p, frame));
            for l in self.missiles_light().into_iter().filter(|l| distance(l.pos, p) <= INFERNO_RADIUS) {
                let b = self.missiles.burns.entry(l.id).or_insert((0.0, due));
                *b = (b.0 + l.burn, due);
            }
            self.log(&format!(
                "missiles: an Inferno at {n} targets, {value:.0} estimated target value, at ({:.0}, {:.0})",
                p.x, p.z
            ));
            if free.is_empty() {
                break;
            }
        }
    }

    /// Once their fires are out, count the buildings our Infernos should
    /// have burnt down gone, unless seen standing since; forget the rest.
    fn missiles_settle_burns(&mut self) {
        let frame = self.frame;
        let out: Vec<(i32, f32, i32)> =
            (self.missiles.burns.iter()).filter(|b| frame >= b.1.1).map(|(&id, &(done, at))| (id, done, at)).collect();
        for (id, done, at) in out {
            self.missiles.burns.remove(&id);
            // (Of unknown type, its health is unknown too.)
            let Some(e) = self.war.targets.get(&id).filter(|e| !e.is_dead && e.is_static && e.ud.is_some()) else {
                continue;
            };
            if e.last_seen < at && done >= e.get_health() * e.def_health() {
                let (unit, name, pos) = (e.unit, e.def_name.clone(), e.position);
                self.war_enemy_destroyed(unit, None);
                self.log(&format!("missiles: the {name} at ({:.0}, {:.0}) counts as burnt", pos.x, pos.z));
            }
        }
    }

    /// All the Eos building `id` takes are flying at it from the pads.
    fn missiles_strike_due(&mut self, id: i32, at: Vec3) {
        let from = self.missiles_silo().map_or(at, |s| s.pos());
        let due = self.frame + (30.0 * (EOS_RISE + distance(from, at) / EOS_SPEED)) as i32;
        self.missiles.strikes.push((id, due));
    }

    /// Count buildings our Eos are due at gone, unless seen standing since
    /// (anti-nukes wait for a look).
    fn missiles_settle_strikes(&mut self) {
        let frame = self.frame;
        let (due, flying): (Vec<_>, Vec<_>) = self.missiles.strikes.iter().partition(|s| s.1 <= frame);
        self.missiles.strikes = flying;
        for (id, at) in due {
            let Some(e) = self.war.targets.get(&id).filter(|e| !e.is_dead) else { continue };
            let (unit, name, pos) = (e.unit, e.def_name.clone(), e.position);
            let place = format!("({:.0}, {:.0})", pos.x, pos.z);
            if e.last_seen >= at {
                self.log(&format!("missiles: the {name} at {place} stands after its Eos"));
            } else if name != "staticantinuke" {
                self.war_enemy_destroyed(unit, None);
                self.log(&format!("missiles: the {name} at {place} counts as gone, its Eos due"));
            }
        }
    }

    fn missiles_launch(&mut self, missile: Unit, at: Vec3) {
        missile.execute_custom_command(CMD_ATTACK, &[at.x, at.y, at.z], 0, NO_TIMEOUT);
        self.missiles.ready.retain(|r| r.id() != missile.id());
    }

    /// Keep the pads stocked: Shockleys for the anti-nukes in the way of
    /// our nukes, Eos for what is worth them, and with neither left in
    /// reach, a few Infernos for clusters of light things.
    fn missiles_restock(&mut self, silo: Unit) {
        let (Some(eos), Some(shockley)) = (self.defs.get(EOS), self.defs.get(SHOCKLEY)) else { return };
        let queued: Vec<i32> = silo.current_commands().iter().map(|c| -c.id()).filter(|&id| id > 0).collect();
        let count = |kind: &str, id: i32| self.missiles_ready(kind).len() + queued.iter().filter(|&&q| q == id).count();
        let (n_eos, n_shock) = (count(EOS, eos.id()), count(SHOCKLEY, shockley.id()));
        // Overdrive's desired energy/mex ratio is an investment target,
        // not a reason to leave a completed silo idle. In particular, a
        // missing Shockley must not strand an already loaded nuclear silo.
        if self.missiles.ready.len() + queued.len() >= PADS || self.eco.hard_estalled {
            return;
        }
        let here = silo.pos();
        let stun = self.missiles_stun_targets().iter().filter(|a| distance(a.1, here) <= REACH).count();
        let needed: usize =
            self.missiles_eos_targets().iter().filter(|t| distance(t.pos, here) <= REACH).map(|t| t.needed).sum();
        let needed = needed + self.missiles.followups.iter().map(|f| f.2).sum::<usize>();
        let infernos = || {
            let def = self.defs.get(INFERNO)?;
            let n = count(INFERNO, def.id());
            let roast = self.missiles_inferno_targets().iter().filter(|c| distance(c.0, here) <= REACH).count();
            (n < roast.min(MAX_INFERNOS)).then_some(def)
        };
        let phantom = !self.dev_off("strikes")
            && self
                .strikes_phantom_targets()
                .iter()
                .any(|(p, n)| *n >= 2 && distance(*p, here) <= REACH && !self.missiles_ground_burning(*p));
        let urgent_inferno = if phantom { infernos() } else { None };
        let def = if n_shock < stun.min(2) {
            shockley
        } else if let Some(inferno) = urgent_inferno {
            inferno
        } else if n_eos < needed.min(PADS) {
            eos
        } else if let Some(inferno) = (stun == 0 && needed == 0).then(infernos).flatten() {
            inferno
        } else {
            return;
        };
        silo.build(def, here, 0, 0);
        self.log(&format!(
            "missiles: queued {} ({stun} anti-nukes in reach, {needed} Eos needed)",
            self.defs.name(def)
        ));
    }

    /// Fire a Shockley at each of `antis` (stunning them through a nuke's
    /// flight), if there are enough on the pads; whether it did.
    pub fn missiles_stun(&mut self, antis: &[(i32, Vec3)]) -> bool {
        let allies = self.missiles_allied_silos(self.missiles_silo());
        if antis.iter().any(|a| ally_covers(&allies, a.1)) {
            return false;
        }
        let ready = self.missiles_ready(SHOCKLEY);
        let fits = |a: &(i32, Vec3)| ready.iter().filter(|m| distance(m.pos(), a.1) <= REACH).count();
        if ready.len() < antis.len() || antis.iter().any(|a| fits(a) == 0) {
            return false;
        }
        let mut free = ready;
        for &(_, p) in antis {
            let Some(i) = free.iter().position(|m| distance(m.pos(), p) <= REACH) else { return false };
            let m = free.remove(i);
            m.execute_custom_command(CMD_ATTACK, &[p.x, p.y, p.z], 0, NO_TIMEOUT);
            self.missiles.ready.retain(|r| r.id() != m.id());
        }
        true
    }

    /// Whether Shockleys on the pads reach the anti-nuke at `p`.
    pub fn missiles_can_stun(&self, p: Vec3) -> bool {
        !ally_covers(&self.missiles_allied_silos(self.missiles_silo()), p)
            && self.missiles_ready(SHOCKLEY).iter().any(|m| distance(m.pos(), p) <= REACH)
    }

    /// Note a strike at `p` (a Bertha shelling, say) on an anti-nuke
    /// there, from when it began.
    pub fn missiles_note_strike(&mut self, p: Vec3) {
        let frame = self.frame;
        let hit: Vec<i32> = (self.war.targets.values())
            .filter(|e| !e.is_dead && e.def_name == "staticantinuke" && distance(e.position, p) < 150.0)
            .map(|e| e.unit_id)
            .collect();
        for id in hit {
            self.missiles.struck.entry(id).or_insert(frame);
        }
    }

    /// Struck anti-nukes still on the books and not seen for a while: where
    /// to look whether they are gone.
    pub fn missiles_looks(&self) -> Vec<Vec3> {
        let frame = self.frame;
        (self.missiles.struck.iter())
            .filter(|&(_, &t)| frame - t > LOOK_AFTER)
            .filter_map(|(id, _)| self.war.targets.get(id))
            .filter(|e| !e.is_dead && e.def_name == "staticantinuke" && frame - e.last_seen > LOOK_AFTER)
            .map(|e| e.position)
            .collect()
    }

    /// What our Eos strike or would: worth watching by radar, to hear
    /// whether it falls.
    pub fn missiles_watch_points(&self) -> Vec<Vec3> {
        let Some(silo) = self.missiles_silo() else { return Vec::new() };
        let here = silo.pos();
        let struck = (self.missiles.struck.keys()).filter_map(|id| self.war.targets.get(id)).map(|e| e.position);
        let targets = self.missiles_eos_targets().into_iter().map(|t| t.pos).filter(|&p| distance(p, here) <= REACH);
        struck.chain(targets).collect()
    }

    /// The nearest of [`missiles_looks`](Self::missiles_looks) to `from`.
    pub fn missiles_scout_target(&self, from: Vec3) -> Option<Vec3> {
        self.missiles_looks().into_iter().min_by(|a, b| distance(*a, from).total_cmp(&distance(*b, from)))
    }
}

/// Score only final buildable positions, never the seeds passed to the
/// engine's build-site search. Keep the safest of the similarly good sites.
fn choose_silo_site(
    allies: &[Vec3],
    sites: impl Iterator<Item = Vec3>,
    targets: &[(Vec3, f32)],
    statics: &[(Vec3, f32)],
    siege: SiegeReadiness,
    enemy: Vec3,
) -> Option<SiloSite> {
    // Score only work not already claimed by an ally, for both triggers.
    let targets: Vec<_> = targets.iter().filter(|t| !ally_covers(allies, t.0)).collect();
    let statics: Vec<_> = statics.iter().filter(|t| !ally_covers(allies, t.0)).collect();
    let total: f32 = statics.iter().map(|t| t.1).sum();
    let scored: Vec<_> = sites
        .filter_map(|p| {
            let (payoff, n) =
                targets.iter().filter(|t| distance(t.0, p) <= REACH).fold((0.0, 0), |(v, n), t| (v + t.1, n + 1));
            let value: f32 = statics.iter().filter(|t| distance(t.0, p) <= REACH).map(|t| t.1).sum();
            let coverage = StaticCoverage { value, share: if total > 0.0 { value / total } else { 0.0 } };
            (payoff >= SILO_COST || coverage.warrants_silo(siege)).then_some(SiloSite {
                pos: p,
                payoff,
                targets: n,
                coverage,
                siege,
            })
        })
        .collect();
    // The coverage trigger admits a site even without a known Eos target.
    // Among those sites, retain the same preference for distance from danger.
    let score = |s: &SiloSite| s.payoff.max(SILO_COST);
    let best = scored.iter().map(score).fold(0.0, f32::max);
    scored
        .into_iter()
        .filter(|s| score(s) >= 0.8 * best)
        .max_by(|a, b| distance(a.pos, enemy).total_cmp(&distance(b.pos, enemy)))
}

#[cfg(test)]
mod inferno_tests {
    use super::*;

    fn light(x: f32, value: f32, unshielded_phantom: bool) -> LightTarget {
        LightTarget { id: 0, pos: Vec3::xz(x, 1000.0), value, burn: BURN_PASSING, unshielded_phantom }
    }

    #[test]
    fn two_exposed_phantoms_warrant_an_inferno_even_when_moving() {
        // Moving snipers have less predicted burn value than the usual
        // threshold, but two together are explicitly worth suppressing.
        let pair = [light(1000.0, 220.0, true), light(1100.0, 220.0, true)];
        assert!(!inferno_cluster_candidates(&pair, INFERNO_WORTH).is_empty());
        assert!(inferno_cluster_candidates(&pair[..1], INFERNO_WORTH).is_empty());
    }

    #[test]
    fn both_phantoms_must_be_identified_and_outside_shields() {
        // A shielded or unidentified contact cannot supply the second
        // Phantom, even if the estimated metal value is very high.
        let mut pair = [light(1000.0, 750.0, true), light(1100.0, 750.0, false)];
        assert!(inferno_cluster_candidates(&pair, INFERNO_WORTH).is_empty());
        pair[0].unshielded_phantom = false;
        assert!(inferno_cluster_candidates(&pair, INFERNO_WORTH).is_empty());
    }

    #[test]
    fn pair_must_fit_one_blast_and_other_contacts_must_not_displace_it() {
        let mut group = [light(1000.0, 220.0, true), light(1400.0, 220.0, true), light(950.0, 1.0, false)];
        let targets = inferno_cluster_candidates(&group, INFERNO_WORTH);
        assert!(targets.iter().any(|t| t.0 == Vec3::xz(1200.0, 1000.0) && t.2 == 3));
        group[1].pos.x = 1000.0 + 2.0 * INFERNO_RADIUS + 1.0;
        assert!(inferno_cluster_candidates(&group, INFERNO_WORTH).is_empty());
    }

    #[test]
    fn ordinary_clusters_keep_the_count_and_value_thresholds() {
        let mut group = [light(1000.0, 201.0, false), light(1100.0, 201.0, false), light(1200.0, 201.0, false)];
        assert!(!inferno_cluster_candidates(&group, INFERNO_WORTH).is_empty());
        assert!(inferno_cluster_candidates(&group[..2], INFERNO_WORTH).is_empty());
        group[0].value = 198.0;
        assert!(inferno_cluster_candidates(&group, INFERNO_WORTH).is_empty());
    }
}

#[cfg(test)]
mod site_tests {
    use super::*;

    // Payoff triggers must work even before the battlefield is dense.
    const EARLY: SiegeReadiness = SiegeReadiness { density: 0.0, threshold: 1500.0, laneness: 0.0 };
    const DENSE: SiegeReadiness = SiegeReadiness { density: 1600.0, ..EARLY };

    #[test]
    fn lost_silo_excludes_nearby_final_sites_until_memory_expires() {
        let mut missiles = Missiles::default();
        let lost = Vec3::xz(2000.0, 2608.0); // TitanDuel's repeatedly rebuilt nanoframe.
        let when = 38390;
        missiles.note_loss(lost, when);
        let shifted = Vec3::xz(2032.0, 2608.0);
        let other = Vec3::xz(1100.0, 2608.0);
        let enemy = Vec3::xz(4000.0, 4000.0);
        let targets = [(enemy, 5000.0)];
        let candidates = [lost, shifted, other].into_iter().filter(|&p| !missiles.shuns(p, when + 1));
        let chosen = choose_silo_site(&[], candidates, &targets, &[], EARLY, enemy).unwrap();
        assert_eq!(chosen.pos, other);
        assert!(missiles.shuns(lost, when + LOSS_MEMORY - 1));
        assert!(!missiles.shuns(lost, when + LOSS_MEMORY));
        // A subsequent loss refreshes protection and expires older sites.
        missiles.note_loss(shifted, when + LOSS_MEMORY);
        assert_eq!(missiles.lost_sites.len(), 1);
        assert!(missiles.shuns(lost, when + LOSS_MEMORY + 1));
    }

    #[test]
    fn allied_coverage_suppresses_both_silo_triggers_but_leaves_uncovered_work() {
        let at = Vec3::xz(1000.0, 1000.0);
        let enemy = Vec3::xz(3000.0, 1000.0);
        let targets = [(enemy, 2400.0)];
        let statics = [(enemy, 60000.0)];
        assert!(choose_silo_site(&[], [at].into_iter(), &targets, &statics, DENSE, enemy).is_some());
        assert!(choose_silo_site(&[at], [at].into_iter(), &targets, &statics, DENSE, enemy).is_none());
        let boundary = Vec3::xz(enemy.x + REACH, enemy.z);
        assert!(choose_silo_site(&[boundary], [at].into_iter(), &targets, &statics, DENSE, enemy).is_none());
        let outside = Vec3::xz(boundary.x + 1.0, boundary.z);
        assert!(choose_silo_site(&[outside], [at].into_iter(), &targets, &statics, DENSE, enemy).is_some());
        // An ally covering one side of the map must not block a silo on
        // another side, nor make its covered value count toward our payoff.
        let uncovered = Vec3::xz(9000.0, 1000.0);
        let targets = [(enemy, 2400.0), (uncovered, 600.0)];
        assert!(choose_silo_site(&[at], [uncovered].into_iter(), &targets, &[], DENSE, enemy).is_none());
        assert!(choose_silo_site(&[at], [uncovered].into_iter(), &[], &[(uncovered, 60000.0)], DENSE, enemy).is_some());
    }

    #[test]
    fn allied_silos_include_nanoframes_but_exclude_ours_dead_and_other_buildings() {
        use springai::Callback;
        unsafe extern "C" fn health(_: i32, id: i32) -> f32 {
            if id == 3 { 0.0 } else { 100.0 }
        }
        unsafe extern "C" fn team(_: i32, id: i32) -> i32 {
            if id == 0 || id == 5 { 1 } else { 2 }
        }
        unsafe extern "C" fn def(_: i32, id: i32) -> i32 {
            if id == 4 { 11 } else { 10 }
        }
        unsafe extern "C" fn building(_: i32, id: i32) -> bool {
            id == 2
        }
        unsafe extern "C" fn pos(_: i32, id: i32, out: *mut f32) {
            unsafe {
                std::ptr::copy_nonoverlapping([id as f32 * 10000.0, 500.0, 1000.0].as_ptr(), out, 3);
            }
        }
        let table = springai::ffi::SSkirmishAICallback {
            Unit_getHealth: Some(health),
            Unit_getTeam: Some(team),
            Unit_getDef: Some(def),
            Unit_isBeingBuilt: Some(building),
            Unit_getPos: Some(pos),
            ..Default::default()
        };
        let cb = unsafe { Callback::from_raw(0, Box::leak(Box::new(table))) };
        let silo = UnitDef::from_id(cb, 10).unwrap();
        // 0 ours, 1 completed ally, 2 allied nanoframe, 3 dead,
        // 4 another building, 5 transferred to us.
        let allies = allied_silos((0..6).map(|id| Unit::from_id(cb, id).unwrap()), 1, silo, None);
        assert_eq!(allies.len(), 2);
        // Two bots with overlapping operational silos must not both defer.
        let first = Unit::from_id(cb, 0).unwrap();
        let later = Unit::from_id(cb, 5).unwrap();
        assert!(allied_silos((0..6).map(|id| Unit::from_id(cb, id).unwrap()), 1, silo, Some(first)).is_empty());
        assert_eq!(allied_silos((0..6).map(|id| Unit::from_id(cb, id).unwrap()), 1, silo, Some(later)), allies);
        assert!(ally_covers(&allies, Vec3::xz(10000.0, 1000.0)));
        assert!(ally_covers(&allies, Vec3::xz(20000.0, 1000.0)));
        for x in [0.0, 30000.0, 40000.0, 50000.0] {
            assert!(!ally_covers(&allies, Vec3::xz(x, 1000.0)));
        }
    }

    #[test]
    fn inward_build_site_adjustment_must_not_lose_the_target() {
        let target = Vec3::xz(5000.0, 1000.0);
        let seed = Vec3::xz(1600.0, 1000.0);
        let shifted = Vec3::xz(1000.0, 1000.0);
        let targets = [(target, 2400.0)];
        assert!(choose_silo_site(&[], [seed].into_iter(), &targets, &[], EARLY, target).is_some());
        assert!(choose_silo_site(&[], [shifted].into_iter(), &targets, &[], EARLY, target).is_none());
        let fallback = Vec3::xz(1800.0, 1000.0);
        assert_eq!(
            choose_silo_site(&[], [shifted, fallback].into_iter(), &targets, &[], EARLY, target).unwrap().pos,
            fallback
        );
    }

    #[test]
    fn final_site_must_reach_enough_value_to_pay_for_the_silo() {
        let targets = [(Vec3::xz(5000.0, 1000.0), 600.0), (Vec3::xz(5000.0, 1800.0), 600.0)];
        let only_one = Vec3::xz(1550.0, 1000.0);
        let both = Vec3::xz(2000.0, 1400.0);
        assert!(choose_silo_site(&[], [only_one].into_iter(), &targets, &[], EARLY, targets[0].0).is_none());
        let chosen = choose_silo_site(&[], [only_one, both].into_iter(), &targets, &[], EARLY, targets[0].0).unwrap();
        assert_eq!((chosen.pos, chosen.payoff, chosen.targets), (both, 1200.0, 2));
    }

    #[test]
    fn static_coverage_is_an_independent_trigger_with_strict_thresholds() {
        let at = Vec3::xz(1000.0, 1000.0);
        let enemy = Vec3::xz(3000.0, 1000.0);
        let far = Vec3::xz(8000.0, 1000.0);
        // No individually worthwhile Eos targets: battlefield density,
        // not the amount of metal inside the firing disk, enables siege.
        let statics = [(enemy, 6000.0), (far, 2000.0)];
        let site = choose_silo_site(&[], [at].into_iter(), &[], &statics, DENSE, enemy).unwrap();
        assert_eq!(site.targets, 0);
        assert_eq!(site.coverage.share, 0.75);
        assert_eq!(site.coverage.value, 6000.0);
        assert!(choose_silo_site(&[], [at].into_iter(), &[], &statics, EARLY, enemy).is_none());
        // Value beyond missile range belongs in the denominator, whether
        // observed or inferred. Exactly 70% is not enough.
        let only_seventy_percent = [(enemy, 7000.0), (far, 3000.0)];
        assert!(choose_silo_site(&[], [at].into_iter(), &[], &only_seventy_percent, DENSE, enemy).is_none());
        let coverage = StaticCoverage { share: 0.71, value: 1.0 };
        assert!(!coverage.warrants_silo(SiegeReadiness { density: 1500.0, ..EARLY }));
        assert!(coverage.warrants_silo(SiegeReadiness { density: 1500.1, ..EARLY }));
        assert!(choose_silo_site(&[], [at].into_iter(), &[], &[], DENSE, enemy).is_none());
    }

    #[test]
    fn coverage_trigger_also_uses_the_final_position() {
        let enemy = Vec3::xz(5000.0, 1000.0);
        let statics = [(enemy, 6000.0)];
        let seed = Vec3::xz(1600.0, 1000.0);
        let shifted = Vec3::xz(1000.0, 1000.0);
        assert!(choose_silo_site(&[], [seed].into_iter(), &[], &statics, DENSE, enemy).is_some());
        assert!(choose_silo_site(&[], [shifted].into_iter(), &[], &statics, DENSE, enemy).is_none());
    }

    #[test]
    fn closed_terrain_enables_siege_earlier_but_does_not_bypass_progression() {
        let coverage = StaticCoverage { share: 0.84, value: 8250.0 };
        assert!(!coverage.warrants_silo(SiegeReadiness::new(1100.0, 1500.0, 0.0)));
        assert!(coverage.warrants_silo(SiegeReadiness::new(1100.0, 1500.0, 1.0)));
        assert!(!coverage.warrants_silo(SiegeReadiness::new(1000.0, 1500.0, 1.0)));
        assert!(!coverage.warrants_silo(SiegeReadiness::new(100.0, 1500.0, 1.0)));
        // Unknown lanes use zero laneness; out-of-range inputs cannot
        // remove the density gate or penalise open maps beyond the slider.
        assert_eq!(SiegeReadiness::new(0.0, 1500.0, -1.0).threshold, 1500.0);
        assert_eq!(SiegeReadiness::new(0.0, 1500.0, 2.0).threshold, 1000.0);
        assert!(!coverage.warrants_silo(SiegeReadiness::new(1600.0, 3000.0, 1.0)));
    }

    #[test]
    fn ravaged_progression_passes_with_modest_static_value() {
        // Recorded at 20:00: 5435 known static metal, battlefield D=2254.
        // If a final buildable site reaches it all, siege should qualify.
        let at = Vec3::xz(2000.0, 2000.0);
        let enemy = Vec3::xz(3000.0, 3000.0);
        let statics = [(enemy, 5435.0)];
        let siege = SiegeReadiness::new(2254.0, 1500.0, 0.7);
        assert!(choose_silo_site(&[], [at].into_iter(), &[], &statics, siege, enemy).is_some());
        // Ravaged's laneness discounts the default to about 1111, not zero.
        assert!((siege.threshold - 1111.11).abs() < 0.1);
        assert!(
            choose_silo_site(&[], [at].into_iter(), &[], &statics, SiegeReadiness::new(1000.0, 1500.0, 0.7), enemy)
                .is_none()
        );
    }

    #[test]
    fn density_slider_defaults_and_bounds_are_safe() {
        for value in [None, Some(""), Some("invalid"), Some("NaN"), Some("inf")] {
            assert_eq!(siege_density_option(value), 1500.0);
        }
        assert_eq!(siege_density_option(Some("400")), 400.0);
        assert_eq!(siege_density_option(Some("-1")), 0.0);
        assert_eq!(siege_density_option(Some("20000")), 10000.0);
    }
}
