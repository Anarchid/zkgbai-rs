//! Mobile shields and cloakers escort combatants, never lead an attack.

use springai::{Unit, Vec3};

use crate::Ai;
use crate::defs::Defs;
use crate::kgbutil::{PathCost, kgb_move_to};
use crate::military::unit_classes::UnitClasses;
use crate::military::wrappers::fighter::next_waypoint;
use crate::military::{FighterStore, Squad, SquadId};
use crate::util::{NULLPOS, OPTION_SHIFT_KEY, distance, get_directional_point};

/// Taskforce leaders live outside the member list. Count actual living
/// combatants, not squad bookkeeping or the supports following them.
fn combat_anchor(
    squad: &Squad,
    fighters: &FighterStore,
    defs: &Defs,
    classes: &UnitClasses,
    mut available: impl FnMut(i32) -> bool,
) -> Option<(Vec3, f32)> {
    if squad.is_air_squad {
        return None;
    }
    let mut anchor = None;
    let mut metal = 0.0;
    for id in squad.leader.into_iter().chain(squad.fighters.iter().copied().filter(|id| Some(*id) != squad.leader)) {
        let Some(f) = fighters.get(&id) else { continue };
        if f.is_dead() || classes.mob_supports.contains(defs.name_of(f.unit)) || !available(id) {
            continue;
        }
        anchor.get_or_insert(f.get_pos());
        metal += f.metal_value;
    }
    anchor.map(|pos| (pos, metal))
}

/// Stay behind the combatant, within its protection radius. A stationary
/// squad has no heading; normalizing that zero-length vector yields NaNs.
fn escort_position(pos: Vec3, heading: Vec3) -> Vec3 {
    if heading == NULLPOS || distance(pos, heading) < 1.0 { pos } else { get_directional_point(pos, heading, -125.0) }
}

fn escort_score(metal: f32, escorts: usize, travel: f32) -> f32 {
    metal / (escorts + 1) as f32 / (1.0 + travel / 2000.0)
}

/// Frontline defenses first, then other frontline assets, then rear
/// turrets. Empty ground and other shield/cloak generators are not anchors.
fn fallback_priority(front: bool, turret: bool, combatant: bool, building: bool) -> Option<u8> {
    if front && turret {
        Some(0)
    } else if front && (combatant || building) {
        Some(1)
    } else if turret {
        Some(2)
    } else {
        None
    }
}

impl Ai {
    fn support_squad_anchor(&self, sid: SquadId, support: Unit) -> Option<(Vec3, f32)> {
        let squad = self.squads.get(sid)?;
        let (pos, metal) = combat_anchor(squad, &self.fighters, &self.defs, &self.unit_classes, |id| {
            !self.retreat_handler.is_retreating(id)
                && self.fighters.get(&id).and_then(|f| f.unit.def()).is_some_and(|d| !d.is_able_to_fly())
        })?;
        let target = escort_position(pos, squad.target);
        self.terrain_reaches(support.def()?, support.pos(), target, 0.0).then_some((target, metal))
    }

    fn support_fallback(&self, support: Unit, friendly: &[Unit]) -> Option<Vec3> {
        let def = support.def()?;
        let pos = support.pos();
        friendly
            .iter()
            .filter_map(|&unit| {
                if unit.id() == support.id() || unit.health() <= 0.0 || unit.is_being_built() {
                    return None;
                }
                let ud = unit.def()?;
                let name = self.defs.name(ud);
                if ud.is_able_to_fly()
                    || self.unit_classes.mob_supports.contains(name)
                    || matches!(name, "staticshield" | "staticjammer")
                    || self.retreat_handler.is_retreating(unit.id())
                {
                    return None;
                }
                let at = unit.pos();
                let building = ud.speed() <= 0.0;
                let combatant = ud.is_able_to_attack();
                let front = self.graph.is_front_line(at) || self.war.get_threat(at) > 0.0;
                let priority = fallback_priority(front, building && combatant, combatant, building)?;
                // Stand on the friendly side of a static's footprint.
                let home = self.graph.get_ally_center();
                let target = if home != NULLPOS && distance(at, home) >= 1.0 {
                    get_directional_point(at, home, 125.0)
                } else {
                    at
                };
                self.terrain_reaches(def, pos, target, 0.0).then_some((priority, distance(pos, target), target))
            })
            .min_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.total_cmp(&b.1)))
            .map(|(_, _, pos)| pos)
    }

    pub(super) fn misc_update_supports(&mut self) {
        if self.misc_handler.supports.is_empty() {
            return;
        }
        // Fetch once, even when many supports need a fallback.
        let friendly = self.cb.friendly_units();
        let ids: Vec<_> = self.misc_handler.supports.iter().copied().collect();
        for id in ids {
            if self.lobster_support(id) || self.retreat_handler.is_retreating(id) {
                continue;
            }
            let Some(f) = self.fighters.get(&id) else { continue };
            let unit = f.unit;
            let name = self.defs.name_of(unit);
            let mut squad = f.squad.filter(|&sid| self.support_squad_anchor(sid, unit).is_some());
            if squad.is_none() {
                // Divide new escorts between squads by combat value and
                // existing escorts of the same kind. Keep valid assignments
                // so moving squads do not make supports oscillate.
                squad = self
                    .squad_handler
                    .squads
                    .iter()
                    .chain(&self.squad_handler.taskforces)
                    .filter_map(|&sid| {
                        let (target, metal) = self.support_squad_anchor(sid, unit)?;
                        let escorts = self
                            .misc_handler
                            .supports
                            .iter()
                            .filter(|&&other| {
                                other != id
                                    && self.fighters.get(&other).is_some_and(|f| {
                                        f.squad == Some(sid) && !f.is_dead() && self.defs.name_of(f.unit) == name
                                    })
                            })
                            .count();
                        Some((sid, escort_score(metal, escorts, distance(unit.pos(), target))))
                    })
                    .max_by(|a, b| a.1.total_cmp(&b.1))
                    .map(|(sid, _)| sid);
            }
            self.fighters.get_mut(&id).unwrap().squad = squad;
            let target = squad
                .and_then(|sid| self.support_squad_anchor(sid, unit).map(|a| a.0))
                .or_else(|| self.support_fallback(unit, &friendly))
                .unwrap_or_else(|| self.graph_get_closest_haven(unit.pos()));
            // No attack/fight orders and no random wandering around an
            // anchor: hold coverage once in place.
            if target == NULLPOS || distance(unit.pos(), target) < 60.0 {
                unit.stop(0);
                continue;
            }
            let mut path = self.pathfinder.find_path(&self.war, unit, target, PathCost::AvoidEnemies);
            if let Some(wp) = next_waypoint(&mut path, 1) {
                kgb_move_to(unit, wp, 0, self.frame);
                if let Some(wp) = next_waypoint(&mut path, 1) {
                    kgb_move_to(unit, wp, OPTION_SHIFT_KEY, self.frame);
                }
            } else {
                unit.stop(0);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::military::wrappers::squad::fake_engine::{self, TEAM, spawn};
    use crate::military::{Fighter, SquadKind};

    #[test]
    fn shields_and_cloakers_are_exclusively_supports() {
        let classes = UnitClasses::new();
        for name in ["shieldshield", "cloakjammer"] {
            assert!(classes.mob_supports.contains(name));
            assert!(!classes.shield_mobs.contains(name));
            assert!(!classes.assaults.contains(name));
        }
        assert!(classes.shield_mobs.contains("shieldfelon"));
        assert!(classes.shield_mobs.contains("shieldassault"));
    }

    #[test]
    fn supports_cannot_anchor_each_other_and_retarget_after_last_fighter_dies() {
        let cb = fake_engine::callback();
        let defs = Defs::new(cb);
        let classes = UnitClasses::new();
        let mut fighters = FighterStore::new();
        let mut squad = Squad::new(1, SquadKind::Assault, TEAM);
        for (id, def) in [(10, 1), (11, 1), (12, 7)] {
            fighters.insert(id, Fighter::new(spawn(cb, id, def), 500.0, TEAM));
            squad.fighters.push(id);
        }
        squad.leader = Some(12);
        let anchor = || combat_anchor(&squad, &fighters, &defs, &classes, |_| true);
        assert_eq!(anchor().unwrap().1, 500.0); // Does not double-count the leader or add support value.
        assert!(combat_anchor(&squad, &fighters, &defs, &classes, |id| id != 12).is_none());
        fake_engine::kill(12); // Still present in the store until destruction cleanup.
        assert!(anchor().is_none());
    }

    #[test]
    fn felon_leader_outside_member_list_is_an_anchor_but_captured_units_are_not() {
        let cb = fake_engine::callback();
        let defs = Defs::new(cb);
        let classes = UnitClasses::new();
        let mut fighters = FighterStore::new();
        fighters.insert(20, Fighter::new(spawn(cb, 20, 0), 620.0, TEAM));
        let mut squad = Squad::new(2, SquadKind::Taskforce, TEAM);
        squad.leader = Some(20);
        assert_eq!(combat_anchor(&squad, &fighters, &defs, &classes, |_| true).unwrap().1, 620.0);
        fake_engine::UNITS.with(|u| u.borrow_mut().get_mut(&20).unwrap().team = TEAM + 1);
        assert!(combat_anchor(&squad, &fighters, &defs, &classes, |_| true).is_none());
    }

    #[test]
    fn stationary_squad_has_finite_escort_position_and_moving_squad_is_followed() {
        let at = Vec3::xz(400.0, 500.0);
        assert_eq!(escort_position(at, at), at);
        assert_eq!(escort_position(at, NULLPOS), at);
        assert_eq!(escort_position(at, Vec3::xz(800.0, 500.0)), Vec3::xz(275.0, 500.0));
    }

    #[test]
    fn fallback_prefers_front_defenses_and_never_empty_ground() {
        assert_eq!(fallback_priority(true, true, true, true), Some(0));
        assert_eq!(fallback_priority(true, false, true, false), Some(1));
        assert_eq!(fallback_priority(true, false, false, true), Some(1));
        assert_eq!(fallback_priority(false, true, true, true), Some(2));
        assert_eq!(fallback_priority(false, false, false, true), None);
        assert_eq!(fallback_priority(true, false, false, false), None);
        assert!(escort_score(2000.0, 0, 100.0) > escort_score(2000.0, 1, 100.0));
        assert!(escort_score(2000.0, 0, 100.0) > escort_score(2000.0, 0, 1000.0));
    }
}
