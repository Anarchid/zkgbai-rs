//! Puppies (`jumpscout`): walking missiles that eat wrecks.
//!
//! ZK's unit_grey_goo: a Puppy eats 5 metal/s of the nearest wreck within
//! 120 (unit wrecks only, `fromunit`), whatever it is doing, and a new
//! Puppy splits off every 45 metal, guarding its parent. So a herd parked
//! on a wreck field doubles about every 9 s while the wrecks last, and the
//! metal is the enemy's no more. The herd goes after the fields away from
//! our base first (ours, our constructors reclaim), not where enemies would
//! eat it; with no field worth it, Puppies fight where our army rallies.
//! Raids on our land come first: two Puppies (90 metal) kill a Pyro, and
//! cheap fast units chasing raiders are what keeps constructors alive.

use std::collections::{BTreeMap, BTreeSet};

use springai::{Unit, Vec3};

use crate::Ai;
use crate::kgbutil::kgb_move_to;
use crate::util::{NULLPOS, distance};

pub const PUPPY: &str = "jumpscout";

/// Frames between updates.
const PERIOD: i32 = 60;

/// Wrecks are grouped into fields of this many elmos square.
const FIELD: f32 = 256.0;

/// A field is worth going to from this much metal (per Puppy sent, it
/// should feed at least one split).
const MIN_FIELD: f32 = 90.0;

/// A Puppy eats within 120; it parks this close to its wreck.
const EAT_RANGE: f32 = 70.0;

/// Fields under more enemy threat than the herd's power are left alone.
const PUPPY_POWER: f32 = 50.0;

/// The factory builds Puppies while the herd is smaller than this.
pub const HERD_CAP: usize = 15;

/// The herd answers raids on our land this far away.
const DEFEND_RANGE: f32 = 2500.0;

#[derive(Default)]
pub struct Puppies {
    pub members: BTreeSet<i32>,
    /// Field each Puppy was sent to (its centre).
    sent: BTreeMap<i32, Vec3>,
    /// Puppies split off since the last summary.
    pub born: u32,
    /// Wreck metal per feature def (0: not a unit's wreck).
    wreck_defs: BTreeMap<i32, f32>,
    /// Wreck fields as last counted: (centre, metal, wrecks).
    fields: Vec<(Vec3, f32, Vec<Vec3>)>,
    /// Their metal outside our base and defenses.
    wild_metal: f32,
}

impl Ai {
    pub fn puppies_add(&mut self, unit: Unit) {
        if self.puppies.members.insert(unit.id()) && self.frame > 0 {
            unit.set_move_state(2, 0);
        }
    }

    /// A Puppy split off a Puppy (no factory built it).
    pub fn puppies_remove_unit(&mut self, unit: Unit) {
        self.puppies.members.remove(&unit.id());
        self.puppies.sent.remove(&unit.id());
    }

    pub fn puppies_count(&self) -> usize {
        self.puppies.members.len()
    }

    /// Wreck metal outside our base and the enemy's defenses.
    pub fn puppies_wreck_metal(&self) -> f32 {
        self.puppies.wild_metal
    }

    /// Count the wreck fields.
    fn puppies_count_fields(&mut self) {
        let mut cells: BTreeMap<(i32, i32), (f32, Vec<Vec3>)> = BTreeMap::new();
        for f in self.cb.features() {
            let Some(d) = f.def() else { continue };
            let metal = *self.puppies.wreck_defs.entry(d.id()).or_insert_with(|| {
                let name = d.name();
                if name.ends_with("_dead") || name.ends_with("_heap") { d.contained_resource(self.metal) } else { 0.0 }
            });
            if metal <= 0.0 {
                continue;
            }
            let p = f.pos();
            let c = cells.entry(((p.x / FIELD) as i32, (p.z / FIELD) as i32)).or_default();
            c.0 += metal;
            c.1.push(p);
        }
        self.puppies.fields = cells
            .into_values()
            .map(|(metal, ps)| {
                let n = ps.len() as f32;
                let c =
                    Vec3::new(ps.iter().map(|p| p.x).sum::<f32>() / n, 0.0, ps.iter().map(|p| p.z).sum::<f32>() / n);
                (c, metal, ps)
            })
            .collect();
        self.puppies.wild_metal = (self.puppies.fields.iter())
            .filter(|f| !self.graph.is_ally_territory(f.0) && self.war.get_porc_threat(f.0) <= 0.0)
            .map(|f| f.1)
            .sum();
    }

    pub fn puppies_update(&mut self, frame: i32) {
        if frame % 1800 == (self.offset + 23) % 1800
            && (self.puppies.born > 0 || !self.puppies.members.is_empty() || self.puppies.wild_metal > 0.0)
        {
            let total: f32 = self.puppies.fields.iter().map(|f| f.1).sum();
            self.log(&format!(
                "puppies: {} in the herd, {} split off wrecks in the last minute; wrecks {:.0} metal in {} fields, {:.0} of it away from our base",
                self.puppies.members.len(),
                self.puppies.born,
                total,
                self.puppies.fields.len(),
                self.puppies.wild_metal
            ));
            self.puppies.born = 0;
        }
        if frame % PERIOD != (self.offset + 13) % PERIOD || self.dev_off("puppies") {
            return;
        }
        self.puppies_count_fields();
        if self.puppies.members.is_empty() {
            return;
        }
        let herd: Vec<Unit> = self
            .puppies
            .members
            .iter()
            .filter_map(|&id| Unit::from_id(self.cb, id))
            .filter(|u| u.health() > 0.0 && u.team() == self.team_id && !u.is_being_built())
            .collect();
        self.puppies.members = herd.iter().map(|u| u.id()).collect();
        if herd.is_empty() {
            return;
        }
        let n = herd.len() as f32;
        let at = Vec3::new(
            herd.iter().map(|u| u.pos().x).sum::<f32>() / n,
            0.0,
            herd.iter().map(|u| u.pos().z).sum::<f32>() / n,
        );
        let power = n * PUPPY_POWER;

        // The best field for the herd: metal, near, outside our base, not
        // under more than it takes.
        let mut best: Option<(f32, Vec3, Vec<Vec3>)> = None;
        for (c, metal, wrecks) in self.puppies.fields.clone() {
            if metal < MIN_FIELD || self.war.get_threat(c) * 500.0 > power || self.war.get_porc_threat(c) > 0.0 {
                continue;
            }
            let home = if self.graph.is_ally_territory(c) { 0.4 } else { 1.0 };
            let score = home * metal / (1.0 + distance(at, c) / 1500.0);
            if best.as_ref().is_none_or(|b| score > b.0) {
                best = Some((score, c, wrecks));
            }
        }

        let frame = self.frame;
        // A raid on our land: the nearest one, in our territory, fresh.
        let raid = (self.war.defense_targets())
            .filter(|(d, predicted)| !predicted && !d.under_water && frame - d.frame_issued <= 150)
            .map(|(d, _)| d.position)
            .filter(|&p| self.graph.is_ally_territory(p) && distance(p, at) <= DEFEND_RANGE)
            .min_by(|a, b| distance(*a, at).total_cmp(&distance(*b, at)));
        if let Some(p) = raid {
            for u in herd {
                if self.puppies.sent.get(&u.id()).is_none_or(|s| distance(*s, p) > 200.0) {
                    u.fight(p, 0);
                    self.puppies.sent.insert(u.id(), p);
                }
            }
            return;
        }
        match best {
            Some((_, _, wrecks)) => {
                // Spread over the wrecks, each Puppy to the nearest one.
                for u in herd {
                    let p = u.pos();
                    let Some(&w) = wrecks.iter().min_by(|a, b| distance(p, **a).total_cmp(&distance(p, **b))) else {
                        continue;
                    };
                    if distance(p, w) > EAT_RANGE && self.puppies.sent.get(&u.id()) != Some(&w) {
                        kgb_move_to(u, w, 0, frame);
                        self.puppies.sent.insert(u.id(), w);
                    }
                }
            }
            None => {
                // Nothing to eat: fight where the army gathers.
                let rally = self.war_get_rally_point(at);
                if rally == NULLPOS {
                    return;
                }
                for u in herd {
                    if self.puppies.sent.get(&u.id()) != Some(&rally) && u.num_current_commands() == 0 {
                        u.fight(rally, 0);
                        self.puppies.sent.insert(u.id(), rally);
                    }
                }
            }
        }
    }
}
