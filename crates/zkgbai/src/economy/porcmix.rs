//! Turret mix and spacing for all our porc (`porcmix`, on by default;
//! `disable=porcmix`).
//!
//! The user, 2026-11-04: "Another thing to avoid in general: multiple
//! turrets of the same type, and placed together; Stardusts for example not
//! only block each other's line of fire, but also explode violently; a 3
//! stardust forfification would much better be a stardust+faraday+2 lotus
//! or stardust + stinger" (asked to apply it to every porc planner, not
//! only the walk-in guard: "Yes!").
//!
//! No ground turret goes where, within [`MIX_RADIUS`] of its site, we
//! already have (built, being built or planned) [`kind_cap`] of its kind
//! (two Lotuses or Pickets, one of the rest), nor where its death blast and
//! a neighbour's overlap the other: the larger blast radius apart at least
//! ([`blast_room`]; the engine's figures, `UnitDef::death_explosion`:
//! Stardust 170 at 600 damage, Faraday and Stinger 130, Lotus, Picket and
//! Gauss 90, Desolator and Lucifer 260 at 1270). The rule sits in
//! `eco_build_check`, so every planner that checks its sites (the shared
//! gate fortifier, the posture, the Lotus pool, the walk-in guard) passes
//! over sites that break it, and those that add blindly (the front and push
//! porc) are refused and pick another kind of their own mix.

use std::collections::BTreeMap;

use springai::{UnitDef, Vec3};

use crate::Ai;
use crate::util::distance;

/// The switch.
pub const FEATURE: &str = "porcmix";

/// How far round a site its kind is counted, elmos.
pub const MIX_RADIUS: f32 = 500.0;

/// The ground turrets the rule is for (anti-air, artillery, superweapons
/// and the like keep their own rules).
const MIXED: [&str; 9] = [
    "turretlaser",
    "turretmissile",
    "turretriot",
    "turretemp",
    "turretgauss",
    "turretheavylaser",
    "turrettorp",
    "turretheavy",
    "turretantiheavy",
];

/// Whether `name` is a turret the rule is for.
pub fn is_mixed(name: &str) -> bool {
    MIXED.contains(&name)
}

/// How many of a kind may stand within [`MIX_RADIUS`]: two of the cheap
/// fillers (Lotus, Picket), one of the rest.
pub fn kind_cap(name: &str) -> usize {
    if matches!(name, "turretlaser" | "turretmissile") { 2 } else { 1 }
}

/// Blasts are kept this much further apart than their radius, elmos: a
/// built turret stands up to a build square off its planned site
/// (wirestank2: a Lucifer and a Faraday planned clear stood 253 apart, the
/// Lucifer's blast 260).
const BLAST_MARGIN: f32 = 16.0;

/// The mid-heavy turrets, of which at most [`MID_CAP`] together stand
/// within [`MIX_RADIUS`] (`midcap`; user, 2026-11-04: "no more than 2
/// mid-heavy turrets per place").
const MID_HEAVY: [&str; 2] = ["turretgauss", "turretheavylaser"];
const MID_CAP: usize = 2;
const MIDCAP: &str = "midcap";

/// How far apart two turrets with death blasts of radius `a` and `b` stand
/// (`d`) against how far they should: under 1, one's blast reaches the
/// other.
pub fn blast_room(d: f32, a: f32, b: f32) -> f32 {
    let need = a.max(b) + if a.max(b) > 0.0 { BLAST_MARGIN } else { 0.0 };
    if need <= 0.0 { f32::INFINITY } else { d / need }
}

/// The radius of `def`'s death blast, elmos (the engine halves the
/// weapon's `areaOfEffect` already).
pub fn blast(def: UnitDef) -> f32 {
    def.death_explosion().map_or(0.0, |w| w.area_of_effect())
}

impl Ai {
    /// Our ground turrets (built, being built) and planned ones within
    /// `radius` of `p`: where, what.
    pub(crate) fn eco_turrets_near(&self, p: Vec3, radius: f32) -> Vec<(Vec3, UnitDef)> {
        let team = self.team_id;
        // A cylinder, not the engine's sphere: sites carry no height, and on
        // high ground a sphere about one misses what stands beside it.
        let mut out: Vec<(Vec3, UnitDef)> = (self.cb.friendly_units_in_cylinder(p, radius).into_iter())
            .filter(|u| u.team() == team && u.health() > 0.0)
            .filter_map(|u| u.def().filter(|d| is_mixed(self.defs.name(*d))).map(|d| (u.pos(), d)))
            .collect();
        // A platform rising for a heavy is its turret already.
        out.extend(self.logistics.walkin.heavy.raising_turret().filter(|(q, _)| distance(*q, p) <= radius));
        out.extend((self.eco.porc_tasks.iter()).filter_map(|&id| {
            let t = self.eco.tasks.get(id)?;
            let c = t.as_construction()?;
            let pos = t.get_pos();
            (c.target.is_none() && is_mixed(self.defs.name(c.build_type)) && distance(pos, p) <= radius)
                .then_some((pos, c.build_type))
        }));
        out
    }

    /// Our ground turrets standing or planned within `radius` of `p`, by
    /// kind.
    pub(crate) fn eco_turret_kinds(&self, p: Vec3, radius: f32) -> BTreeMap<String, usize> {
        let mut kinds: BTreeMap<String, usize> = BTreeMap::new();
        for (_, def) in self.eco_turrets_near(p, radius) {
            *kinds.entry(self.defs.name(def).to_owned()).or_default() += 1;
        }
        kinds
    }

    /// Whether `def` may go at `pos` (see the module notes): always, for
    /// what is not a ground turret or with the switch off.
    pub(crate) fn eco_mix_allows(&self, def: UnitDef, pos: Vec3) -> bool {
        let name = self.defs.name(def);
        if !is_mixed(name) || self.dev_off(FEATURE) {
            return true;
        }
        let near = self.eco_turrets_near(pos, MIX_RADIUS);
        if near.iter().filter(|(_, d)| d.id() == def.id()).count() >= kind_cap(name) {
            return false;
        }
        if MID_HEAVY.contains(&name)
            && !self.dev_off(MIDCAP)
            && near.iter().filter(|(_, d)| MID_HEAVY.contains(&self.defs.name(*d))).count() >= MID_CAP
        {
            return false;
        }
        let mine = blast(def);
        near.iter().all(|&(p, d)| blast_room(distance(p, pos), mine, blast(d)) >= 1.0)
    }

    /// The first of `kinds` that may go at the site nearest `pos` it has,
    /// and that site.
    pub(crate) fn eco_mix_pick(&self, kinds: &[UnitDef], pos: Vec3) -> Option<(UnitDef, Vec3)> {
        kinds.iter().find_map(|&def| {
            let site = self.cb.map().find_closest_build_site(def, pos, 600.0, 3, 0);
            (site.x >= 0.0 && self.eco_mix_allows(def, site)).then_some((def, site))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn turrets_keep_out_of_each_other_s_blast() {
        // A Stardust (170) 120 from a Lotus (90): inside its blast.
        assert!(blast_room(120.0, 90.0, 170.0) < 1.0);
        // A Stinger (130) 150 from a Faraday (130): clear; 140 is within
        // the margin a build square shifts a turret by.
        assert!(blast_room(150.0, 130.0, 130.0) >= 1.0);
        assert!(blast_room(140.0, 130.0, 130.0) < 1.0);
        assert_eq!(blast_room(10.0, 0.0, 0.0), f32::INFINITY);
    }

    #[test]
    fn two_fillers_one_of_the_rest() {
        assert_eq!(kind_cap("turretlaser"), 2);
        assert_eq!(kind_cap("turretmissile"), 2);
        assert_eq!(kind_cap("turretriot"), 1);
        assert_eq!(kind_cap("turretheavy"), 1);
        assert!(is_mixed("turretgauss"));
        assert!(!is_mixed("turretaaflak"));
        assert!(!is_mixed("staticarty"));
    }
}
