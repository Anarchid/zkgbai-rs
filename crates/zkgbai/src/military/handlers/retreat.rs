//! Pulls damaged units back to heal.

use std::collections::{BTreeMap, BTreeSet};

use springai::{Unit, Vec3};

use crate::Ai;
use crate::military::Coward;
use crate::nav;

#[derive(Default)]
pub struct RetreatHandler {
    retreating_units: BTreeSet<i32>,
    /// Units that may retreat, by unit id.
    cowards: BTreeMap<i32, Coward>,
    /// Retreating units that were already given a retreat order.
    retreated_units: BTreeSet<i32>,
    /// Cowards with a shield weapon.
    shields: BTreeSet<i32>,

    // Unit def ids by retreat category (filled by `retreat_init`).
    aa_defs: BTreeSet<i32>,
    low_maneuverability: BTreeSet<i32>,
    striders: BTreeSet<i32>,
    small_raiders: BTreeSet<i32>,
    planes: BTreeSet<i32>,
    scythe_id: i32,
    initialized: bool,
}

impl RetreatHandler {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn num_retreating(&self) -> usize {
        self.retreating_units.len()
    }

    /// Whether the unit is retreating. Unlike [`Ai::retreat_is_retreating`]
    /// this skips the original's special case for uncloaked scythes, which
    /// needs the threat map; only the raider handler controls scythes.
    pub fn is_retreating(&self, unit_id: i32) -> bool {
        self.retreating_units.contains(&unit_id)
    }

    pub fn is_coward(&self, unit_id: i32) -> bool {
        self.cowards.contains_key(&unit_id)
    }
}

impl Ai {
    /// Classify unit types by retreat level (the original's constructor).
    pub fn retreat_init(&mut self) {
        let h = &mut self.retreat_handler;
        for def in self.defs.all() {
            if def.tooltip().contains("Anti-Air") {
                h.aa_defs.insert(def.id());
            } else if self.defs.name(def).starts_with("strider") {
                h.striders.insert(def.id());
            }
        }

        for name in &self.unit_classes.small_raiders {
            if let Some(d) = self.defs.get(name) {
                h.small_raiders.insert(d.id());
            }
        }

        if let Some(fac) = self.defs.get("factoryplane") {
            for ud in fac.build_options() {
                if ud.build_options().is_empty() {
                    h.planes.insert(ud.id());
                }
            }
        }

        h.scythe_id = self.defs.id("cloakheavyraid");

        for name in
            ["tankraid", "tankheavyraid", "tankriot", "hoverskirm", "hoverriot", "vehriot", "vehassault", "shieldriot"]
        {
            if let Some(d) = self.defs.get(name) {
                h.low_maneuverability.insert(d.id());
            }
        }
        h.initialized = true;
    }

    pub fn retreat_update(&mut self, frame: i32) {
        if frame % 15 == self.offset % 15 {
            if self.retreat_handler.cowards.is_empty() {
                return;
            }
            self.retreat_clean_units();
            self.retreat_check_shields();
            self.retreat_cowards();
        }
    }

    pub fn retreat_add_coward(&mut self, unit: Unit) {
        if !self.retreat_handler.initialized {
            self.retreat_init();
        }
        let Some(def) = unit.def() else {
            return;
        };
        let def_id = def.id();
        let h = &mut self.retreat_handler;

        let rlvl = if h.aa_defs.contains(&def_id) {
            0.85
        } else if def.is_able_to_fly() || h.low_maneuverability.contains(&def_id) {
            0.65
        } else if h.striders.contains(&def_id) {
            0.6
        } else {
            0.5
        };

        let mut c = Coward::new(unit, def, self.defs.name(def), rlvl, self.team_id);
        c.small_raider = h.small_raiders.contains(&def_id);
        c.is_scythe = def_id == h.scythe_id;
        c.is_plane = h.planes.contains(&def_id);

        if c.shield.is_some() {
            h.shields.insert(c.id);
        }
        h.cowards.insert(c.id, c);
    }

    /// Whether `unit` is retreating; uncloaked scythes out of danger are not
    /// considered retreating.
    pub fn retreat_is_retreating(&self, unit: Unit) -> bool {
        let h = &self.retreat_handler;
        let id = unit.id();
        if !h.retreating_units.contains(&id) {
            return false;
        }
        let is_scythe = match h.cowards.get(&id) {
            Some(c) => c.is_scythe,
            None => unit.def().is_some_and(|d| d.id() == h.scythe_id),
        };
        // Don't retreat scythes unless they're cloaked.
        !(is_scythe && !unit.is_cloaked() && self.war.get_effective_threat(unit.pos()) <= 0.0)
    }

    /// Re-evaluate a damaged unit; `on_fire` if it is burning.
    pub fn retreat_check_unit(&mut self, unit: Unit, on_fire: bool) {
        let id = unit.id();
        let Some(c) = self.retreat_handler.cowards.get_mut(&id) else {
            return; // Not our unit.
        };
        if c.is_retreating || (on_fire && c.small_raider) {
            return;
        }
        if c.should_retreat() {
            self.retreat_handler.retreating_units.insert(id);
            self.squad_remove_from_squad(unit);
            self.raider_remove_from_squad(unit);
        }
    }

    pub fn retreat_remove_unit(&mut self, unit: Unit) {
        let h = &mut self.retreat_handler;
        let id = unit.id();
        h.cowards.remove(&id);
        h.retreating_units.remove(&id);
        h.retreated_units.remove(&id);
        h.shields.remove(&id);
    }

    fn retreat_coward_to(&mut self, id: i32, pos: Vec3) {
        let mut nav = nav!(self);
        if let Some(c) = self.retreat_handler.cowards.get_mut(&id) {
            c.retreat_to(pos, &mut nav);
        }
    }

    fn retreat_cowards(&mut self) {
        let mut retreated = false;
        let ids: Vec<i32> = self.retreat_handler.retreating_units.iter().copied().collect();
        for id in ids {
            let h = &mut self.retreat_handler;
            let Some(c) = h.cowards.get_mut(&id) else {
                // port: the original assumed every retreating unit is a coward.
                h.retreating_units.remove(&id);
                h.retreated_units.remove(&id);
                continue;
            };
            if c.is_healed() {
                h.retreated_units.remove(&id);
                h.retreating_units.remove(&id);
                continue;
            }

            if c.is_dead() {
                h.retreated_units.remove(&id);
                h.shields.remove(&id);
                h.cowards.remove(&id);
                h.retreating_units.remove(&id);
                continue;
            }
            // On its way home through the Djinn's lamp.
            if self.teleports.evacuating(id) {
                continue;
            }

            let (unit, pos, is_scythe, is_plane, is_flyer, cloaked) =
                (c.unit, c.get_pos(), c.is_scythe, c.is_plane, c.is_flyer, c.is_cloaked());

            // Don't retreat scythes unless they're cloaked.
            if is_scythe && !cloaked && self.war.get_effective_threat(pos) <= 0.0 {
                continue;
            }

            if !self.retreat_handler.retreated_units.contains(&id) || self.war.get_threat(pos) > 0.0 {
                if !is_plane {
                    let haven = if is_flyer {
                        self.graph_get_closest_air_haven(pos)
                    } else {
                        // A haven an enemy group gets to first is none.
                        let haven = self.graph_get_closest_haven(pos);
                        if self.incursion_imminent(haven, (unit.power() + unit.health()) / 10.0) {
                            self.incursion_refuge(unit).unwrap_or(haven)
                        } else {
                            haven
                        }
                    };
                    self.retreat_coward_to(id, haven);
                } else if !self.graph.is_enemy_territory(pos) {
                    if let Some(c) = self.retreat_handler.cowards.get(&id) {
                        c.find_pad();
                    }
                } else {
                    // If in enemy territory, maneuver back to safety before finding an airpad.
                    let center = self.graph.get_ally_center();
                    self.retreat_coward_to(id, center);
                }
                self.retreat_handler.retreated_units.insert(id);
                retreated = true;
            }
        }
        if !retreated {
            self.retreat_handler.retreated_units.clear();
        }
    }

    fn retreat_check_shields(&mut self) {
        let ids: Vec<i32> = self.retreat_handler.shields.iter().copied().collect();
        for id in ids {
            let h = &mut self.retreat_handler;
            let Some(c) = h.cowards.get_mut(&id) else {
                continue;
            };
            if !c.is_retreating && c.check_shield() {
                let unit = c.unit;
                h.retreating_units.insert(id);
                self.squad_remove_from_squad(unit);
            }
        }
    }

    fn retreat_clean_units(&mut self) {
        let h = &mut self.retreat_handler;
        let dead: Vec<i32> = h.cowards.values().filter(|c| c.is_dead()).map(|c| c.id).collect();
        for id in dead {
            h.cowards.remove(&id);
            h.retreating_units.remove(&id);
            h.retreated_units.remove(&id);
            h.shields.remove(&id);
        }
    }
}
