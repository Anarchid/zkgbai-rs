//! Dominatrix fireteams and permanent retirement of valuable captors.
//!
//! Formation policy can be disabled through the doctrine. Earned retirement
//! and cloak entitlements survive policy changes and the loss of a captive:
//! these are commitments, not a cache of the currently visible army.
use std::collections::BTreeMap;

use springai::{NO_TIMEOUT, Unit, Vec3, reserved};

use crate::Ai;
use crate::defs::cmd;
use crate::kgbutil::{PathCost, kgb_move_to};
use crate::military::SquadId;
use crate::util::{NULLPOS, distance};

const SHIELD_MARGIN: f32 = 90.0;

#[derive(Default)]
#[checkpoint::state]
pub struct Dominatrices {
    retired: BTreeMap<i32, Vip>,
}

#[checkpoint::state]
struct Vip {
    unit: Unit,
    home: Vec3,
    custom_cloak: bool,
}

fn reward(cost: f32, commander: bool) -> (bool, bool) {
    (commander || cost >= 1000.0, cost >= 3000.0)
}

/// Farthest point on the rear ray whose shield contains every core with
/// an AoE margin. Intersect disks analytically, then try nearer positions
/// if terrain blocks the ideal one. A scattered line may have no solution.
fn rear_post(cores: &[Vec3], facing: Vec3, radius: f32, mut allowed: impl FnMut(Vec3) -> bool) -> Option<Vec3> {
    if cores.is_empty() || radius <= SHIELD_MARGIN {
        return None;
    }
    let n = cores.len() as f32;
    let at = Vec3::xz(cores.iter().map(|p| p.x).sum::<f32>() / n, cores.iter().map(|p| p.z).sum::<f32>() / n);
    let d = distance(at, facing);
    if d < 1.0 {
        return None;
    }
    let (x, z) = ((at.x - facing.x) / d, (at.z - facing.z) / d);
    let r = radius - SHIELD_MARGIN;
    let (mut lo, mut hi) = (0.0f32, r);
    for p in cores {
        let (dx, dz) = (p.x - at.x, p.z - at.z);
        let along = dx * x + dz * z;
        let across = dx * z - dz * x;
        if across.abs() > r {
            return None;
        }
        let reach = (r * r - across * across).sqrt();
        lo = lo.max(along - reach);
        hi = hi.min(along + reach);
    }
    if hi < lo {
        return None;
    }
    (0..=16)
        .map(|i| {
            let t = hi - (hi - lo) * i as f32 / 16.0;
            Vec3::xz(at.x + x * t, at.z + z * t)
        })
        .find(|&p| allowed(p))
}

impl Ai {
    fn dominatrix_candidates(&self) -> Vec<i32> {
        self.fighters
            .values()
            .filter(|f| {
                !f.is_dead()
                    && self.defs.name_of(f.unit) == "vehcapture"
                    && !self.retreat_handler.is_retreating(f.id)
                    && !self.dominatrix_vip(f.id)
                    && !reserved::is_reserved(self.cb, f.id)
                    && f.squad.is_none_or(|sid| {
                        self.squads.get(sid).is_none_or(|s| {
                            s.shape.doctrine != "dominatrix"
                                && !self.qrf_drafted(sid)
                                && self.op_squad_target(sid).is_none()
                                && !self.op.over.squad_intents.contains_key(&sid)
                        })
                    })
            })
            .map(|f| f.id)
            .collect()
    }

    pub(crate) fn dominatrix_can_form(&self) -> bool {
        self.squad_handler
            .taskforces
            .iter()
            .filter_map(|&sid| self.squads.get(sid))
            .any(|s| s.shape.doctrine == "dominatrix" && s.cores(&self.fighters) < 10)
            || self.dominatrix_candidates().len() >= 3
    }

    pub(crate) fn dominatrix_form_teams(&mut self) {
        if self.taskforces.doctrine("dominatrix").is_none() {
            return;
        }
        for id in self.dominatrix_candidates() {
            if !self.dominatrix_can_form() {
                break;
            }
            self.squad_remove_from_squad(self.fighters[&id].unit);
            self.squad_assign_taskforce_member(id);
        }
    }

    pub(crate) fn dominatrix_conduct(&mut self, sid: SquadId) {
        self.dominatrix_release_excess(sid);
        // Fire control owns every core and Ripper; shields receive only
        // rearward move orders below. Escorts never gate this conduct.
        self.fire_assign(sid);
        let Some(s) = self.squads.get(sid) else { return };
        let cores: Vec<_> = s
            .everyone()
            .filter_map(|id| self.fighters.get(&id))
            .filter(|f| !f.is_dead() && self.defs.name_of(f.unit) == "vehcapture")
            .map(|f| f.unit.pos())
            .collect();
        let shields: Vec<_> = s
            .everyone()
            .filter_map(|id| self.fighters.get(&id))
            .filter(|f| !f.is_dead() && self.defs.name_of(f.unit) == "shieldshield")
            .map(|f| f.unit)
            .collect();
        if cores.is_empty() {
            return;
        }
        let facing = s
            .fire
            .focus
            .and_then(|id| self.war.targets.get(&id))
            .map(|e| e.position)
            .unwrap_or_else(|| self.graph.get_enemy_center());
        let facing = if facing == NULLPOS {
            let home = self.graph.get_ally_center();
            let at = cores[0];
            if distance(at, home) >= 1.0 {
                Vec3::xz(2.0 * at.x - home.x, 2.0 * at.z - home.z)
            } else {
                Vec3::xz(at.x + 1.0, at.z)
            }
        } else {
            facing
        };
        let map = self.cb.map();
        for (i, shield) in shields.iter().enumerate() {
            let Some(def) = shield.def() else { continue };
            let radius = def
                .weapon_mounts()
                .iter()
                .filter_map(|m| m.weapon_def())
                .map(|w| w.shield_radius())
                .fold(0.0, f32::max);
            let allowed = |p: Vec3| {
                p.x >= 8.0
                    && p.z >= 8.0
                    && p.x < map.width() as f32 * 8.0 - 8.0
                    && p.z < map.height() as f32 * 8.0 - 8.0
                    && self.terrain_reaches(def, shield.pos(), p, 0.0)
            };
            // First try to cover the whole line. If it is scattered, divide
            // cores among the shields so each still protects a distinct part.
            let group: Vec<_> =
                cores.iter().enumerate().filter(|(j, _)| j % shields.len() == i).map(|(_, p)| *p).collect();
            let post =
                rear_post(&cores, facing, radius, allowed).or_else(|| rear_post(&group, facing, radius, allowed));
            if let Some(post) = post {
                self.dominatrix_move(*shield, post, 16.0);
            }
        }
    }

    fn dominatrix_release_excess(&mut self, sid: SquadId) {
        let Some(s) = self.squads.get(sid) else { return };
        let cores = s.cores(&self.fighters);
        let mut counts = [0usize; 2];
        let release: Vec<_> = s
            .everyone()
            .filter_map(|id| {
                let f = self.fighters.get(&id)?;
                let (i, cap) = match self.defs.name_of(f.unit) {
                    "vehriot" => (0, cores / 3),
                    "shieldshield" => (1, cores),
                    _ => return None,
                };
                counts[i] += 1;
                (counts[i] > cap).then_some((id, i))
            })
            .collect();
        for (id, role) in release {
            self.squad_leave_squad(id);
            if let Some(f) = self.fighters.get_mut(&id) {
                f.squad = None;
            }
            if role == 1 {
                self.squad_handler.fighters.remove(&id);
                if let Some(f) = self.fighters.remove(&id) {
                    self.misc_add_support(f);
                }
            } else {
                self.squad_assign_taskforce_member(id);
            }
        }
    }

    /// A policy change releases formations but preserves earned retirement.
    pub(crate) fn dominatrix_switch(&mut self, off: bool) {
        if let Some(d) = self.taskforces.doctrines.iter_mut().find(|d| d.name == "dominatrix") {
            d.enabled = !off;
        }
        if !off {
            self.dominatrix_form_teams();
            return;
        }
        let teams: Vec<_> = self
            .squad_handler
            .taskforces
            .iter()
            .copied()
            .filter(|&sid| self.squads.get(sid).is_some_and(|s| s.shape.doctrine == "dominatrix"))
            .collect();
        for sid in teams {
            self.fire_release(sid);
            let members = self.squads.get_mut(sid).unwrap().disband(&mut self.fighters);
            self.squad_handler.taskforces.retain(|&s| s != sid);
            self.squad_delete_squad(sid);
            for id in members {
                self.squad_assign_taskforce_member(id);
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn dominatrix_validate_checkpoint(&self) -> Result<(), String> {
        for (&id, vip) in &self.taskforces.dominatrices.retired {
            if id != vip.unit.id() || self.fighters.contains_key(&id) || self.eco.workers.contains_key(&id) {
                return Err(format!("retired Dominatrix/commander {id} has inconsistent ownership"));
            }
        }
        Ok(())
    }

    pub(crate) fn dominatrix_vip(&self, id: i32) -> bool {
        self.taskforces.dominatrices.retired.contains_key(&id)
    }

    fn dominatrix_retire(&mut self, unit: Unit, custom_cloak: bool) {
        if let Some(vip) = self.taskforces.dominatrices.retired.get_mut(&unit.id()) {
            vip.custom_cloak |= custom_cloak;
            return;
        }
        // Remove actual membership, not just command access, so workers,
        // recruitment, retreat and density accounting stop using this unit.
        self.squad_remove_unit(unit);
        self.retreat_remove_unit(unit);
        self.raider_remove_unit(unit);
        self.misc_remove_unit(unit);
        self.eco.remove_worker(unit.id());
        self.war.new_units.remove(&unit.id());
        reserved::own(self.cb, unit.id(), reserved::Owner { module: "dominatrix", helpers: Default::default() });
        unit.stop(0);
        unit.set_fire_state(0, 0);
        unit.set_move_state(0, 0);
        unit.cloak(true, 0);
        unit.execute_custom_command(cmd::UNIT_AI, &[0.0], 0, NO_TIMEOUT);
        unit.execute_custom_command(cmd::WANTED_SPEED, &[unit.max_speed() * 30.0], 0, NO_TIMEOUT);
        self.taskforces.dominatrices.retired.insert(unit.id(), Vip { unit, home: NULLPOS, custom_cloak });
        self.log(&format!("dominatrix: {} retires; dedicated cloak={custom_cloak}", unit.id()));
    }

    /// Transfer callbacks can precede the game's capture_controller rules
    /// param update. The periodic census repeats this idempotent observation.
    pub(crate) fn dominatrix_observe_capture(&mut self, captive: Unit) {
        if captive.team() != self.team_id || captive.health() <= 0.0 {
            return;
        }
        let controller = captive.rules_param_float("capture_controller", -1.0);
        if !controller.is_finite() || controller < 0.0 || controller.fract() != 0.0 {
            return;
        }
        let Some(captor) = Unit::from_id(self.cb, controller as i32) else { return };
        if captor.team() != self.team_id || captor.health() <= 0.0 || self.defs.name_of(captor) != "vehcapture" {
            return;
        }
        let Some(def) = captive.def() else { return };
        let commander = self.defs.is_commander(def);
        let (retire, custom) = reward(def.cost(self.metal), commander);
        if !retire {
            return;
        }
        self.dominatrix_retire(captor, custom);
        if commander {
            self.dominatrix_retire(captive, false);
        }
    }

    fn dominatrix_home(&self, unit: Unit, friendly: &[Unit]) -> Vec3 {
        let Some(def) = unit.def() else { return unit.pos() };
        let fronts: Vec<_> = self
            .graph
            .get_front_line_spots()
            .iter()
            .map(|s| s.position)
            .chain(self.war.targets.values().filter(|e| !e.is_dead && e.position != NULLPOS).map(|e| e.position))
            .collect();
        let depth = |p: Vec3| {
            fronts
                .iter()
                .map(|&f| distance(p, f))
                .reduce(f32::min)
                .unwrap_or_else(|| distance(p, self.graph.get_enemy_center()))
        };
        let safe = |p: Vec3| {
            p != NULLPOS
                && self.war.get_threat(p) <= 0.0
                && self.war.get_porc_threat(p) <= 0.0
                && !self.graph.is_front_line(p)
                && self.terrain_reaches(def, unit.pos(), p, 0.0)
        };
        // Actual completed defensive buildings, not merely planned forts.
        let forts: Vec<_> = friendly
            .iter()
            .filter(|u| u.health() > 0.0 && !u.is_being_built())
            .filter(|u| {
                u.def().is_some_and(|d| {
                    d.speed() <= 0.0
                        && d.is_able_to_attack()
                        && self.defs.name(d).starts_with("turret")
                        && !self.defs.name(d).starts_with("turretaa")
                })
            })
            .map(|u| {
                let p = u.pos();
                let home = self.graph.get_start_pos();
                let d = distance(p, home).max(1.0);
                Vec3::xz(p.x + (home.x - p.x) / d * 100.0, p.z + (home.z - p.z) / d * 100.0)
            })
            .filter(|&p| safe(p))
            .collect();
        // Prefer an existing cloak within a rear fort, if one is present.
        let covered = |p: Vec3| {
            friendly.iter().any(|u| self.defs.name_of(*u) == "staticjammer" && self.existing_cloak_covers(*u, p))
        };
        forts
            .into_iter()
            .max_by(|&a, &b| {
                // Cloak breaks ties between similarly rearward fortified places.
                let score = |p| (depth(p) / 300.0).floor();
                score(a)
                    .total_cmp(&score(b))
                    .then_with(|| covered(a).cmp(&covered(b)))
                    .then_with(|| depth(a).total_cmp(&depth(b)))
            })
            .unwrap_or_else(|| {
                let home = self.graph.get_start_pos();
                if safe(home) { home } else { self.graph_get_closest_haven(unit.pos()) }
            })
    }

    fn dominatrix_move(&mut self, unit: Unit, target: Vec3, tolerance: f32) {
        if target == NULLPOS {
            return;
        }
        if distance(unit.pos(), target) <= tolerance {
            unit.stop(0);
            return;
        }
        let mut path = self.pathfinder.find_path(&self.war, unit, target, PathCost::AvoidEnemies);
        if let Some(p) = crate::military::wrappers::fighter::next_waypoint(&mut path, 1) {
            kgb_move_to(unit, p, 0, self.frame);
        }
    }

    pub(crate) fn dominatrix_update(&mut self, frame: i32) {
        if frame % 15 != self.offset % 15 {
            return;
        }
        let units = self.cb.team_units();
        for &unit in &units {
            self.dominatrix_observe_capture(unit);
        }
        let lost: Vec<_> = self
            .taskforces
            .dominatrices
            .retired
            .iter()
            .filter(|(_, v)| v.unit.health() <= 0.0 || v.unit.team() != self.team_id)
            .map(|(&id, _)| id)
            .collect();
        for id in lost {
            self.taskforces.dominatrices.retired.remove(&id);
            if reserved::owner(self.cb, id).is_some_and(|o| o.module == "dominatrix") {
                reserved::disown(self.cb, id);
            }
        }
        let friendly = self.cb.friendly_units();
        let retired: Vec<_> = self.taskforces.dominatrices.retired.values().map(|v| (v.unit, v.home)).collect();
        for (unit, old) in retired {
            let home = if old == NULLPOS || frame % 150 == self.offset % 150 || self.war.get_threat(old) > 0.0 {
                self.dominatrix_home(unit, &friendly)
            } else {
                old
            };
            self.taskforces.dominatrices.retired.get_mut(&unit.id()).unwrap().home = home;
            self.dominatrix_move(unit, home, 45.0);
        }
    }

    /// VIPs at the same haven can share one field, including their captured
    /// commanders. Expose destinations so a free Iris meets them at safety.
    pub(crate) fn dominatrix_cloak_groups(&self) -> Vec<(Vec3, Vec<(Vec3, f32)>, bool)> {
        let mut groups: Vec<(Vec3, Vec<(Vec3, f32)>, bool)> = Vec::new();
        for vip in self.taskforces.dominatrices.retired.values() {
            if vip.home == NULLPOS || vip.unit.health() <= 0.0 || vip.unit.team() != self.team_id {
                continue;
            }
            let value = vip.unit.def().map_or(1000.0, |d| d.cost(self.metal)).max(1000.0);
            if let Some(g) = groups.iter_mut().find(|g| distance(g.0, vip.home) < 180.0) {
                g.1.push((vip.home, value));
                g.2 |= vip.custom_cloak;
            } else {
                groups.push((vip.home, vec![(vip.home, value)], vip.custom_cloak));
            }
        }
        groups
    }

    pub(crate) fn dominatrix_cloaks_required(&self) -> usize {
        let friendly = self.cb.friendly_units();
        self.dominatrix_cloak_groups()
            .iter()
            .filter(|g| {
                g.2 && !friendly.iter().any(|u| {
                    self.defs.name_of(*u) == "staticjammer"
                        && g.1.iter().all(|(p, _)| self.existing_cloak_covers(*u, *p))
                })
            })
            .count()
    }
}

#[cfg(test)]
pub(crate) mod tests;

#[cfg(test)]
mod geometry_tests {
    use super::*;

    #[test]
    fn retirement_uses_individual_capture_value_and_commanders_override_price() {
        assert_eq!(reward(999.0, false), (false, false));
        assert_eq!(reward(1000.0, false), (true, false));
        assert_eq!(reward(2999.0, false), (true, false));
        assert_eq!(reward(3000.0, false), (true, true));
        assert_eq!(reward(0.0, true), (true, false));
    }

    #[test]
    fn shield_sits_at_farthest_rearward_point_covering_entire_line_with_margin() {
        let cores = [Vec3::xz(1000.0, 900.0), Vec3::xz(1000.0, 1000.0), Vec3::xz(1000.0, 1100.0)];
        let facing = Vec3::xz(1500.0, 1000.0);
        let post = rear_post(&cores, facing, 350.0, |_| true).unwrap();
        assert!(post.x < 770.0);
        assert!(cores.iter().all(|&p| distance(post, p) <= 260.01));
        let farther = Vec3::xz(post.x - 1.0, post.z);
        assert!(cores.iter().any(|&p| distance(farther, p) > 260.0));
        let blocked = rear_post(&cores, facing, 350.0, |p| p.x >= 800.0).unwrap();
        assert!(blocked.x >= 800.0 && blocked.x < 825.0);
        assert!(rear_post(&cores, facing, 350.0, |_| false).is_none());
        assert!(rear_post(&[cores[0], Vec3::xz(1000.0, 1500.0)], facing, 350.0, |_| true).is_none());
    }
}
