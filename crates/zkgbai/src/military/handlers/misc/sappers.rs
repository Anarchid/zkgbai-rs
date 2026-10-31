//! Patient minefields: spread across front gates, then across each gate.
//! Both destinations and live positions reserve blast clearance.

use springai::{NO_TIMEOUT, Unit, UnitDef, Vec3};

use crate::Ai;
use crate::defs::cmd;
use crate::kgbutil::kgb_move_to_exact;
use crate::terrain::{pos, tier_of};
use crate::util::{distance, get_directional_point};

const PERIOD: i32 = 150;
const ARRIVED: f32 = 16.0;
// Covers arrival tolerance at both ends and the receiving unit's footprint.
const CLEARANCE: f32 = 64.0;

#[derive(Clone, Copy)]
struct Site {
    position: Vec3,
    gate: Vec3,
    value: f32,
    depth: usize,
}

#[derive(Clone, Copy)]
pub(super) struct Sapper {
    pub unit: Unit,
    radius: f32,
    imp: bool,
    snitch: bool,
    site: Option<Site>,
}

#[derive(Clone, Copy)]
struct Reservation {
    id: i32,
    position: Vec3,
    radius: f32,
    site: Option<Site>,
}

fn blast_radius(def: UnitDef) -> f32 {
    // Bomb explosions are not mounted weapons. Read both so a killed mine
    // is just as safe for its neighbours as one that triggers normally.
    [def.death_explosion(), def.self_d_explosion()]
        .into_iter()
        .flatten()
        .map(|w| w.area_of_effect())
        .fold(0.0, f32::max)
}

fn configure(unit: Unit) {
    unit.set_move_state(0, 0);
    unit.set_fire_state(2, 0);
    unit.execute_custom_command(cmd::UNIT_AI, &[0.0], 0, NO_TIMEOUT);
}

fn supported(imp: bool, turret: f32, mobile: f32) -> bool {
    !imp || turret > 0.0 || mobile > 0.0
}

fn turret_covers_approach(mine: Vec3, turret: Vec3, enemy: Vec3, range: f32) -> bool {
    distance(mine, turret) <= range
        && (mine.x - turret.x) * (enemy.x - turret.x) + (mine.z - turret.z) * (enemy.z - turret.z) > 0.0
}

fn spaced(id: i32, p: Vec3, radius: f32, reservations: &[Reservation]) -> bool {
    reservations.iter().filter(|r| r.id != id).all(|r| {
        let separation = radius.max(r.radius) + CLEARANCE;
        distance(p, r.position) >= separation && r.site.is_none_or(|s| distance(p, s.position) >= separation)
    })
}

/// Keep the whole building footprint outside a Snitch's blast, including
/// arrival tolerance. Imps may still shelter under nearby turret coverage.
fn clear_of_statics(snitch: bool, p: Vec3, radius: f32, statics: &[(Vec3, f32)]) -> bool {
    !snitch || statics.iter().all(|&(center, footprint)| distance(p, center) >= radius + footprint + CLEARANCE)
}

/// First occupy other gates, then add depth at already occupied gates.
/// Travel and strategic value break ties, without letting one valuable
/// gate take every mine. Existing valid sites are kept by the caller.
fn choose_site(id: i32, from: Vec3, radius: f32, sites: &[Site], reservations: &[Reservation]) -> Option<Site> {
    sites
        .iter()
        .filter(|s| spaced(id, s.position, radius, reservations))
        .min_by(|a, b| {
            let load = |s: &Site| {
                reservations
                    .iter()
                    .filter(|r| r.id != id)
                    .filter(|r| r.site.is_some_and(|other| distance(other.gate, s.gate) < 64.0))
                    .count()
            };
            let cost = |s: &Site| (1.0 + distance(from, s.position) / 1200.0) / s.value.max(0.01);
            load(a).cmp(&load(b)).then(a.depth.cmp(&b.depth)).then_with(|| cost(a).total_cmp(&cost(b)))
        })
        .copied()
}

/// A line across the approach, with additional rows behind it. The first
/// row faces the enemy; depth stays on the home side of the gate.
fn rows(center: Vec3, forward: Vec3, width: f32, spacing: f32) -> Vec<(Vec3, usize)> {
    let length = forward.x.hypot(forward.z).max(1.0);
    let (dx, dz) = (forward.x / length, forward.z / length);
    let half = ((width * 0.5 - 32.0).max(0.0) / spacing).floor() as i32;
    let mut out = Vec::new();
    for row in 0..3 {
        for offset in -half..=half {
            let side = offset as f32 * spacing;
            let back = row as f32 * spacing;
            out.push((Vec3::xz(center.x - dx * back - dz * side, center.z - dz * back + dx * side), row));
        }
    }
    out
}

impl Ai {
    pub fn misc_prepare_sapper(&self, unit: Unit) {
        configure(unit);
    }

    pub fn misc_add_sapper(&mut self, unit: Unit) {
        let Some(def) = unit.def() else { return };
        configure(unit);
        self.misc_handler.sappers.insert(
            unit.id(),
            Sapper {
                unit,
                radius: blast_radius(def),
                imp: self.defs.name(def) == "cloakbomb",
                snitch: self.defs.name(def) == "shieldbomb",
                site: None,
            },
        );
    }

    fn sapper_front(&self, p: Vec3) -> bool {
        self.graph.is_front_line(p)
            && !self.graph.is_enemy_territory(p)
            && (self.graph.is_ally_territory(p) || self.war.get_total_friendly_power(p) > 0.0)
    }

    fn sapper_supported(&self, imp: bool, p: Vec3) -> bool {
        // Sappers never paint these maps, so mines cannot count one another
        // (or themselves) as the firepower that finishes stunned enemies.
        let mobile = self.war.get_friendly_threat(p);
        if supported(imp, 0.0, mobile) {
            return true;
        }
        // The general porc map also paints nanoframes. Only a completed,
        // functioning turret behind the mine can finish its stunned prey.
        let enemy = self.graph.get_enemy_center();
        let turret = !enemy.is_zero()
            && self.eco.dporcs.values().any(|t| {
                !t.is_dead()
                    && !t.unit.is_being_built()
                    && !t.unit.is_paralyzed()
                    && turret_covers_approach(p, t.get_pos(), enemy, t.unit.max_range())
            });
        supported(imp, if turret { 1.0 } else { 0.0 }, mobile)
    }

    fn sapper_sites(&self, radius: f32) -> Vec<Site> {
        let mut sites = Vec::new();
        if let Some(strategy) = &self.strategy
            && let Some(map) = self.terrain().and_then(|t| t.tier(strategy.tier))
        {
            for (rank, gate) in strategy.fortify.iter().enumerate() {
                let c = &map.chokes[gate.choke];
                let crossing = Vec3::xz(c.pos.x, c.pos.z);
                let setback = distance(crossing, gate.pos);
                if setback < 1.0 {
                    continue;
                }
                let center = get_directional_point(crossing, gate.pos, setback.min(32.0));
                if !self.sapper_front(center) {
                    continue;
                }
                let forward = Vec3::xz(c.pos.x - gate.pos.x, c.pos.z - gate.pos.z);
                for (p, depth) in rows(center, forward, gate.width, radius + CLEARANCE) {
                    // The strategy already orders gates by priority.
                    sites.push(Site { position: p, gate: crossing, value: 1.0 / (1.0 + rank as f32), depth });
                }
            }
        }
        // Open maps and the period before terrain analysis still get a
        // distributed minefield, using owned front spots as sectors.
        if sites.is_empty() {
            let enemy = self.graph.get_enemy_center();
            if enemy.is_zero() {
                return sites;
            }
            for spot in self.graph.get_front_line_spots().into_iter().filter(|s| s.owned) {
                let gate = spot.get_pos();
                if distance(gate, enemy) < 1.0 {
                    continue;
                }
                let center = get_directional_point(gate, enemy, 200.0);
                let forward = Vec3::xz(enemy.x - gate.x, enemy.z - gate.z);
                for (p, depth) in rows(center, forward, 800.0, radius + CLEARANCE) {
                    sites.push(Site { position: p, gate, value: spot.value.max(1.0), depth });
                }
            }
        }
        sites
    }

    pub(super) fn misc_update_sappers(&mut self, frame: i32) {
        if frame % PERIOD != (self.offset + 10) % PERIOD || self.misc_handler.sappers.is_empty() {
            return;
        }
        let sappers: Vec<_> = self.misc_handler.sappers.values().copied().collect();
        // Snapshot all friendly buildings, including allies and nanoframes,
        // once per planning pass. New construction also invalidates old sites.
        let statics: Vec<_> = if sappers.iter().any(|s| s.snitch) {
            self.cb
                .friendly_units()
                .into_iter()
                .filter_map(|u| {
                    let def = u.def()?;
                    if u.health() <= 0.0 || def.speed() > 0.0 {
                        return None;
                    }
                    // Footprint sizes are in 8-elmo squares; use half the diagonal.
                    let footprint = 4.0 * (def.x_size() as f32).hypot(def.z_size() as f32);
                    Some((u.pos(), footprint))
                })
                .collect()
        } else {
            Vec::new()
        };
        let mut reservations: Vec<_> = sappers
            .iter()
            .map(|s| Reservation { id: s.unit.id(), position: s.unit.pos(), radius: s.radius, site: s.site })
            .collect();
        for (i, sapper) in sappers.iter().enumerate() {
            let unit = sapper.unit;
            let from = reservations[i].position;
            // Do not uncover an ambush just as the enemy walks into it.
            // Fire-at-will handles contact; never give ATTACK or FIGHT here.
            if !self.cb.enemy_units_in(from, sapper.radius + 128.0).is_empty() {
                if unit.num_current_commands() > 0 {
                    unit.stop(0);
                }
                continue;
            }
            let kept = sapper.site.filter(|s| {
                self.sapper_front(s.position)
                    && self.sapper_supported(sapper.imp, s.position)
                    && clear_of_statics(sapper.snitch, s.position, sapper.radius, &statics)
                    && spaced(unit.id(), s.position, sapper.radius, &reservations)
            });
            let selected = kept.or_else(|| {
                let reachable = self.pathfinder.raider_reachability(&self.war, unit, 0.0);
                let map = unit.def().and_then(tier_of).and_then(|tier| self.terrain()?.tier(tier))?;
                let sites: Vec<_> = self
                    .sapper_sites(sapper.radius)
                    .into_iter()
                    .filter(|s| {
                        self.sapper_front(s.position)
                            && self.sapper_supported(sapper.imp, s.position)
                            && clear_of_statics(sapper.snitch, s.position, sapper.radius, &statics)
                            && self.war.get_porc_threat(s.position) <= 0.0
                            && map.region_at(pos(s.position)).is_some()
                            && reachable.reaches(s.position)
                    })
                    .collect();
                choose_site(unit.id(), from, sapper.radius, &sites, &reservations)
            });
            reservations[i].site = selected;
            self.misc_handler.sappers.get_mut(&unit.id()).unwrap().site = selected;
            match selected {
                Some(mut s) if distance(from, s.position) > ARRIVED => {
                    if kept.is_none() || unit.num_current_commands() == 0 {
                        s.position.y = self.cb.map().elevation_at(s.position.x, s.position.z);
                        kgb_move_to_exact(unit, s.position, 4.0, 0, frame);
                    }
                }
                _ if unit.num_current_commands() > 0 => {
                    unit.stop(0);
                }
                _ => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn site(x: f32, gate: f32, value: f32) -> Site {
        Site { position: Vec3::xz(x, 100.0), gate: Vec3::xz(gate, 100.0), value, depth: 0 }
    }

    #[test]
    fn imps_need_independent_support_but_snitches_do_not() {
        assert!(!supported(true, 0.0, 0.0));
        assert!(supported(true, 100.0, 0.0));
        assert!(supported(true, 0.0, 0.1));
        assert!(supported(false, 0.0, 0.0));
    }

    #[test]
    fn snitches_keep_blast_clearance_from_building_edges_but_imps_can_use_cover() {
        let building = [(Vec3::xz(1000.0, 1000.0), 100.0)];
        // Outside the centre's blast radius is insufficient for a large building.
        let near = Vec3::xz(1400.0, 1000.0);
        assert!(!clear_of_statics(true, near, 384.0, &building));
        assert!(!clear_of_statics(true, Vec3::xz(1547.0, 1000.0), 384.0, &building));
        assert!(clear_of_statics(true, Vec3::xz(1548.0, 1000.0), 384.0, &building));
        assert!(clear_of_statics(false, near, 384.0, &building));
        assert!(clear_of_statics(true, near, 384.0, &[]));
        // A previously valid placement becomes invalid when a building appears.
        assert!(!clear_of_statics(true, near, 384.0, &[(near, 16.0)]));
    }

    #[test]
    fn mixed_mines_reserve_the_larger_blast_at_both_live_and_future_positions() {
        let reservations = [Reservation {
            id: 1,
            position: Vec3::xz(100.0, 100.0),
            radius: 384.0,
            site: Some(site(1500.0, 1500.0, 1.0)),
        }];
        assert!(!spaced(2, Vec3::xz(400.0, 100.0), 100.0, &reservations));
        assert!(!spaced(2, Vec3::xz(1200.0, 100.0), 100.0, &reservations));
        assert!(spaced(2, Vec3::xz(700.0, 100.0), 100.0, &reservations));
        assert!(!spaced(2, Vec3::xz(700.0, 100.0), 600.0, &reservations));
        assert!(spaced(1, Vec3::xz(100.0, 100.0), 384.0, &reservations));
    }

    #[test]
    fn another_gate_wins_over_packing_the_nearest_high_value_gate() {
        let first = site(1000.0, 1000.0, 100.0);
        let reservations = [Reservation { id: 1, position: first.position, radius: 200.0, site: Some(first) }];
        let candidates = [site(1400.0, 1000.0, 100.0), site(4000.0, 4000.0, 1.0)];
        assert_eq!(choose_site(2, Vec3::ZERO, 200.0, &candidates, &reservations).unwrap().gate.x, 4000.0);
        assert!(choose_site(2, Vec3::ZERO, 200.0, &[first], &reservations).is_none());
    }

    #[test]
    fn rows_cover_the_width_and_add_depth_only_on_the_home_side() {
        let points = rows(Vec3::xz(1000.0, 1000.0), Vec3::xz(1.0, 0.0), 1000.0, 400.0);
        assert_eq!(points.len(), 9);
        assert!(points.iter().all(|(p, _)| p.x <= 1000.0 && p.z >= 600.0 && p.z <= 1400.0));
        for (i, a) in points.iter().enumerate() {
            assert!(points[i + 1..].iter().all(|b| distance(a.0, b.0) >= 400.0));
        }
        assert_eq!(rows(Vec3::ZERO, Vec3::xz(0.0, 1.0), 100.0, 400.0).len(), 3);
    }

    #[test]
    fn fills_the_enemy_facing_row_before_closer_rear_rows() {
        let front = site(1000.0, 1000.0, 1.0);
        let rear = Site { depth: 1, ..site(500.0, 1000.0, 1.0) };
        assert_eq!(choose_site(1, Vec3::ZERO, 200.0, &[rear, front], &[]).unwrap().position.x, 1000.0);
    }

    #[test]
    fn turret_support_places_the_mine_in_front_and_in_weapon_range() {
        let turret = Vec3::xz(1000.0, 1000.0);
        let enemy = Vec3::xz(2000.0, 1000.0);
        assert!(turret_covers_approach(Vec3::xz(1200.0, 1000.0), turret, enemy, 300.0));
        assert!(!turret_covers_approach(Vec3::xz(800.0, 1000.0), turret, enemy, 300.0));
        assert!(!turret_covers_approach(Vec3::xz(1400.0, 1000.0), turret, enemy, 300.0));
    }

    #[test]
    fn engine_orders_hold_position_fire_at_will_and_disable_chasing() {
        use springai::ffi::{
            CommandTopic as Topic, SCustomUnitCommand, SSetFireStateUnitCommand, SSetMoveStateUnitCommand,
            SSkirmishAICallback,
        };
        use std::cell::RefCell;
        use std::ffi::c_void;

        thread_local! { static ORDERS: RefCell<Vec<(u32, i32)>> = const { RefCell::new(Vec::new()) }; }
        unsafe extern "C" fn command(_: i32, _: i32, _: i32, topic: i32, data: *mut c_void) -> i32 {
            let value = unsafe {
                match topic as u32 {
                    Topic::COMMAND_UNIT_SET_MOVE_STATE => (*(data as *const SSetMoveStateUnitCommand)).moveState,
                    Topic::COMMAND_UNIT_SET_FIRE_STATE => (*(data as *const SSetFireStateUnitCommand)).fireState,
                    Topic::COMMAND_UNIT_CUSTOM => {
                        let c = &*(data as *const SCustomUnitCommand);
                        if c.cmdId == cmd::UNIT_AI && c.params_size == 1 && *c.params == 0.0 { 0 } else { -1 }
                    }
                    _ => -1,
                }
            };
            ORDERS.with(|o| o.borrow_mut().push((topic as u32, value)));
            0
        }
        let table = SSkirmishAICallback { Engine_handleCommand: Some(command), ..Default::default() };
        // SAFETY: every handle is used only while this table is alive.
        let cb = unsafe { springai::Callback::from_raw(0, &table) };
        configure(Unit::from_id(cb, 1).unwrap());
        ORDERS.with(|o| {
            assert_eq!(
                *o.borrow(),
                vec![
                    (Topic::COMMAND_UNIT_SET_MOVE_STATE, 0),
                    (Topic::COMMAND_UNIT_SET_FIRE_STATE, 2),
                    (Topic::COMMAND_UNIT_CUSTOM, 0),
                ]
            )
        });
    }

    #[test]
    fn blast_spacing_reads_explosions_even_without_mounted_weapons() {
        use springai::ffi::SSkirmishAICallback;
        unsafe extern "C" fn death(_: i32, _: i32) -> i32 {
            10
        }
        unsafe extern "C" fn self_d(_: i32, _: i32) -> i32 {
            11
        }
        unsafe extern "C" fn area(_: i32, weapon: i32) -> f32 {
            if weapon == 10 { 352.0 } else { 384.0 }
        }
        let table = SSkirmishAICallback {
            UnitDef_getDeathExplosion: Some(death),
            UnitDef_getSelfDExplosion: Some(self_d),
            WeaponDef_getAreaOfEffect: Some(area),
            ..Default::default()
        };
        // SAFETY: every handle is used only while this table is alive.
        let cb = unsafe { springai::Callback::from_raw(0, &table) };
        assert_eq!(blast_radius(UnitDef::from_id(cb, 1).unwrap()), 384.0);
    }
}
