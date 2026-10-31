//! Dedicated Weaver escorts. Workers stay in the economy registry, but a
//! renewable reservation prevents competing assignments while their Crab lives.
use super::*;
use springai::UnitDef;

pub(super) const ENEMY_VALUE: f32 = 1500.0;
const ZONE_RADIUS: f32 = 400.0;
const CONTACT: f32 = 850.0;
const LEASH: f32 = 280.0;
const WORK_RADIUS: f32 = 220.0;
const REPAIR_HEALTH: f32 = 0.98;

#[derive(Default)]
pub(super) struct Support {
    pub units: Vec<Unit>,
    last: i32,
    orders: BTreeMap<i32, (i32, i32)>,
    pub(super) repairing: bool,
    pub(super) paused: BTreeMap<i32, Vec<(i32, Vec<f32>)>>,
    turrets: Vec<Vec3>,
    turret_at: i32,
}
fn escort_count(value: f32) -> usize {
    (value / ENEMY_VALUE).ceil().clamp(2.0, 4.0) as usize
}

/// Maximum metal clustered within 400 elmos of a known enemy, independent
/// of whether those enemies are mobile or static.
fn concentration(samples: &[(Vec3, f32)]) -> f32 {
    samples
        .iter()
        .map(|(center, _)| {
            samples.iter().filter(|(p, _)| distance(*center, *p) <= ZONE_RADIUS).map(|(_, v)| *v).sum::<f32>()
        })
        .fold(0.0, f32::max)
}

/// Repair class first, damage second; an engineer cannot repair itself.
fn repair_target(worker: i32, candidates: &[(i32, u8, f32)]) -> Option<i32> {
    candidates
        .iter()
        .filter(|(id, _, health)| *id != worker && *health > 0.0 && *health < REPAIR_HEALTH)
        .min_by(|a, b| a.1.cmp(&b.1).then(a.2.total_cmp(&b.2)))
        .map(|c| c.0)
}

impl Ai {
    pub(crate) fn crab_escort(&self, id: i32) -> bool {
        self.taskforces.siege.forces.iter().any(|(&sid, f)| {
            self.squads.get(sid).is_some_and(|q| q.cores(&self.fighters) > 0)
                && f.support.units.iter().any(|u| u.id() == id)
        })
    }

    /// Raw metal in a compact, recent enemy concentration near this Crab.
    /// Mobiles count, even with no known buildings or range gain to be had.
    pub(super) fn crab_enemy_value(&self, at: Vec3) -> f32 {
        let nearby: Vec<_> = self
            .war
            .targets
            .values()
            .filter(|e| e.identified && !e.is_dead && !e.can_fly() && e.position != NULLPOS)
            .filter(|e| e.is_static || self.frame - e.last_seen <= 30 * 10)
            .filter(|e| distance(at, e.position) <= CONTACT)
            .map(|e| (e.position, e.ud.map_or(e.value, |d| d.cost(self.metal))))
            .collect();
        concentration(&nearby)
    }

    /// Keep pedestal placement local to the Crab. Height buys protection;
    /// extended ballistic range is never a prerequisite or a scoring bonus.
    pub(super) fn crab_site(&self, leader: Unit, perch: &Perch, statics: &[Static]) -> Option<Vec3> {
        let from = leader.pos();
        if self.crab_enemy_value(from) < ENEMY_VALUE {
            return None;
        }
        let map = self.cb.map();
        let mut candidates = vec![from];
        for radius in [96.0, 192.0] {
            for n in 0..8 {
                let a = n as f32 * std::f32::consts::TAU / 8.0;
                candidates.push(Vec3::xz(from.x + radius * a.cos(), from.z + radius * a.sin()));
            }
        }
        candidates
            .into_iter()
            .find(|&p| {
                p.x >= 50.0
                    && p.z >= 50.0
                    && p.x < map.width() as f32 * 8.0 - 50.0
                    && p.z < map.height() as f32 * 8.0 - 50.0
                    && self.crab_enemy_value(p) >= ENEMY_VALUE
                    && self.siege_perch_site_ok(p, perch)
                    && !self.war.targets.values().any(|e| !e.is_dead && e.visible && distance(e.position, p) <= FOOT)
                    && self.siege_fits_by(p, leader, statics, PERCH_STANDOFF)
            })
            .map(|p| Vec3::new(p.x, map.elevation_at(p.x, p.z), p.z))
    }

    pub(crate) fn crab_support_produce(&self, options: &[UnitDef]) -> Option<UnitDef> {
        let weaver = *options.iter().find(|d| self.defs.name(**d) == "spidercon")?;
        let want: usize = self
            .squad_handler
            .taskforces
            .iter()
            .filter_map(|&sid| self.squads.get(sid))
            .filter(|q| q.shape.doctrine == "crab")
            .filter_map(|q| q.get_leader(&self.fighters))
            .map(|u| escort_count(self.crab_enemy_value(u.pos())))
            .sum();
        if want == 0 {
            return None;
        }
        let have = self
            .cb
            .friendly_units()
            .iter()
            .filter(|u| u.team() == self.team_id && self.defs.name_of(**u) == "spidercon")
            .count();
        // Two Weavers remain available to the economy. Unfinished units count.
        (have < want + 2).then_some(weaver)
    }

    pub(super) fn crab_support_tick(&mut self, sid: SquadId, leader: Unit) {
        let frame = self.frame;
        let pos = leader.pos();
        let value = self.crab_enemy_value(pos);
        let want = escort_count(value);
        let f = self.taskforces.siege.forces.entry(sid).or_default();
        if frame - f.support.last < 30 {
            return;
        }
        f.support.last = frame;
        f.support.repairing = false;
        f.support.units.retain(|u| u.health() > 0.0 && u.team() == self.team_id);
        f.support.paused.retain(|id, _| f.support.units.iter().any(|u| u.id() == *id));
        f.support.orders.retain(|id, _| f.support.units.iter().any(|u| u.id() == *id));
        let have = f.support.units.len();
        let reserved: std::collections::BTreeSet<i32> = self
            .taskforces
            .siege
            .forces
            .values()
            .flat_map(|f| {
                f.support.units.iter().chain(f.raisers.iter()).chain(f.perch.iter().flat_map(|p| p.raisers.iter()))
            })
            .map(|u| u.id())
            .collect();
        let mut free: Vec<_> = self
            .eco
            .workers
            .values()
            .filter(|w| !w.is_chicken && !w.assigned_plop && !w.unit.is_being_built() && w.unit.health() > 0.0)
            .filter(|w| self.defs.name_of(w.unit) == "spidercon" && !reserved.contains(&w.id))
            .filter(|w| !self.eco_worker_held(w.id) && !is_terraforming(w.unit))
            .map(|w| (distance(pos, w.get_pos()), w.id))
            .filter(|(d, _)| *d <= 4000.0)
            .collect();
        free.sort_by(|a, b| a.0.total_cmp(&b.0));
        // Never take the last two free economy Weavers.
        let take = want.saturating_sub(have).min(free.len().saturating_sub(2));
        let mut added = Vec::new();
        for (_, id) in free.into_iter().take(take) {
            if let Some(w) = self.eco.workers.get_mut(&id) {
                w.clear_task(&mut self.eco.tasks);
                added.push(w.unit);
            }
        }
        if !added.is_empty() {
            let ids: Vec<_> = added.iter().map(|u| u.id()).collect();
            self.op_emit(
                "crab-escort",
                json!({"event": "joined", "squad": sid, "crab": leader.id(), "units": ids,
                "have": have + added.len(), "want": want}),
            );
            self.log(&format!("taskforce: the crab force (squad {sid}) recruits Weavers {ids:?}; {} of {want} escorts, joining on the move", have + added.len()));
        }
        let f = self.taskforces.siege.forces.get_mut(&sid).unwrap();
        f.support.units.extend(added);
        let escorts = f.support.units.clone();
        if let Some(p) = f.perch.as_mut().filter(|p| !p.abandoned) {
            p.raisers = escorts.clone();
        }
        self.eco_hold_terraform(&escorts, 30 * 15);
        let mut allies = self.cb.friendly_units_in_cylinder(pos, WORK_RADIUS);
        allies.extend(escorts.iter().copied());
        allies.push(leader);
        allies.sort_by_key(|u| u.id());
        allies.dedup_by_key(|u| u.id());
        let candidates: Vec<_> = allies
            .iter()
            .filter(|u| !u.is_being_built() && distance(u.pos(), pos) <= LEASH)
            .filter(|u| {
                escorts.iter().any(|e| e.id() == u.id())
                    || u.id() == leader.id()
                    || distance(u.pos(), pos) <= WORK_RADIUS
            })
            .map(|u| {
                (
                    u.id(),
                    if escorts.iter().any(|e| e.id() == u.id()) {
                        0
                    } else if u.id() == leader.id() {
                        1
                    } else {
                        2
                    },
                    u.health() / u.max_health().max(1.0),
                )
            })
            .collect();
        let active_raise = self
            .taskforces
            .siege
            .forces
            .get(&sid)
            .and_then(|f| f.perch.as_ref())
            .is_some_and(|p| p.started && !p.up && !p.abandoned);
        let staged = self
            .taskforces
            .siege
            .forces
            .get(&sid)
            .and_then(|f| f.perch.as_ref())
            .is_some_and(|p| !p.started && !p.abandoned);
        for (index, &u) in escorts.iter().enumerate() {
            if self.eco.workers.get(&u.id()).is_some_and(|w| w.is_chicken) {
                // Retreat owns the commands for now. Reassert the escort's
                // order when it returns instead of trusting a stale cache.
                self.taskforces.siege.forces.get_mut(&sid).unwrap().support.orders.remove(&u.id());
                continue;
            }
            let eligible: Vec<_> = candidates.iter().copied().filter(|(_, class, _)| index > 0 || *class < 2).collect();
            let target = repair_target(u.id(), &eligible).and_then(|id| allies.iter().find(|a| a.id() == id)).copied();
            if let Some(t) = target {
                self.taskforces.siege.forces.get_mut(&sid).unwrap().support.repairing = true;
                self.crab_support_order(sid, u, 40, t.id(), active_raise, |u| {
                    u.repair(t, 0);
                });
                continue;
            }
            if active_raise {
                let saved = self.taskforces.siege.forces.get_mut(&sid).unwrap().support.paused.remove(&u.id());
                if let Some(commands) = saved {
                    for (i, (id, params)) in commands.iter().enumerate() {
                        u.execute_custom_command(
                            *id,
                            params,
                            if i == 0 { 0 } else { springai::opt::SHIFT_KEY },
                            NO_TIMEOUT,
                        );
                    }
                    self.taskforces.siege.forces.get_mut(&sid).unwrap().support.orders.remove(&u.id());
                } else if !is_terraforming(u)
                    && let Some(other) = escorts.iter().find(|e| is_terraforming(**e))
                {
                    for (i, c) in other.current_commands().iter().enumerate() {
                        u.execute_custom_command(
                            c.id(),
                            &c.params(),
                            if i == 0 { 0 } else { springai::opt::SHIFT_KEY },
                            NO_TIMEOUT,
                        );
                    }
                }
                continue;
            }
            self.taskforces.siege.forces.get_mut(&sid).unwrap().support.paused.remove(&u.id());
            if staged {
                self.crab_support_order(sid, u, 25, leader.id(), false, |u| {
                    u.guard(leader, 0);
                });
                continue;
            }
            // At least one engineer guards the Crab whenever others forage.
            if index > 0 && distance(u.pos(), pos) <= LEASH {
                let wreck = self
                    .cb
                    .features_in_cylinder(pos, WORK_RADIUS)
                    .into_iter()
                    .filter(|f| {
                        f.reclaim_left() > 0.0
                            && f.def().is_some_and(|d| d.is_reclaimable() && d.contained_resource(self.metal) > 0.0)
                    })
                    .filter(|f| self.war.view(u.def()).get_threat(f.pos()) <= 0.0)
                    .filter(|f| {
                        u.def().is_none_or(|d| reaches(self.reach_terrain(), d, u.pos(), f.pos(), d.build_distance()))
                    })
                    .min_by(|a, b| distance(u.pos(), a.pos()).total_cmp(&distance(u.pos(), b.pos())));
                if let Some(wreck) = wreck {
                    self.crab_support_order(sid, u, 90, wreck.id(), false, |u| {
                        u.reclaim_feature(wreck, 0);
                    });
                    continue;
                }
            }
            if index == 1 && value >= ENEMY_VALUE && self.crab_support_turret(sid, u, leader) {
                continue;
            }
            self.crab_support_order(sid, u, 25, leader.id(), false, |u| {
                u.guard(leader, 0);
            });
        }
    }

    fn crab_support_order(
        &mut self,
        sid: SquadId,
        u: Unit,
        kind: i32,
        target: i32,
        raising: bool,
        send: impl FnOnce(Unit),
    ) {
        let f = self.taskforces.siege.forces.get_mut(&sid).unwrap();
        if raising && is_terraforming(u) {
            f.support
                .paused
                .entry(u.id())
                .or_insert_with(|| u.current_commands().iter().map(|c| (c.id(), c.params())).collect());
        }
        // An economy/event handler or retreat may have replaced our order.
        // Repair and guard carry unit IDs; reclaim's engine parameter is an
        // encoded feature ID, so compare its kind and our cached target only.
        let live_order = u
            .current_commands()
            .iter()
            .any(|c| c.id() == kind && (kind == 90 || c.params().first().is_some_and(|&p| p as i32 == target)));
        if f.support.orders.get(&u.id()) != Some(&(kind, target)) || !live_order {
            f.support.orders.insert(u.id(), (kind, target));
            send(u);
        }
    }

    fn crab_support_turret(&mut self, sid: SquadId, u: Unit, leader: Unit) -> bool {
        // Finish a nearby turret before considering another, including one whose
        // build was interrupted by repairs. Count nanoframes to avoid duplicates.
        let nearby = self.cb.friendly_units_in_cylinder(leader.pos(), WORK_RADIUS);
        if let Some(t) = nearby.iter().find(|t| self.defs.name_of(**t) == "turretlaser" && t.is_being_built()) {
            self.crab_support_order(sid, u, 40, t.id(), false, |u| {
                u.repair(*t, 0);
            });
            return true;
        }
        // An accepted build may not have a nanoframe yet; keep its order.
        if u.current_commands().iter().any(|c| c.id() == -self.defs.id("turretlaser")) {
            return true;
        }
        let f = self.taskforces.siege.forces.get(&sid).unwrap();
        if self.frame - f.support.turret_at < 30 * 60
            || f.support.turrets.iter().filter(|p| distance(**p, leader.pos()) <= WORK_RADIUS * 2.0).count() >= 2
            || nearby.iter().filter(|t| self.defs.name_of(**t) == "turretlaser").count() >= 2
        {
            return false;
        }
        if distance(u.pos(), leader.pos()) > LEASH
            || !self.taskforces.siege.forces.get(&sid).and_then(|f| f.station.as_ref()).is_some_and(|s| s.deployed)
        {
            return false;
        }
        let Some(def) = self.defs.get("turretlaser") else {
            return false;
        };
        if !u.def().is_some_and(|d| d.build_options().iter().any(|d| d.id() == def.id())) {
            return false;
        }
        let home = self.graph.get_start_pos();
        let anchor = if distance(leader.pos(), home) < 1.0 {
            leader.pos()
        } else {
            get_directional_point(leader.pos(), home, 160.0)
        };
        let site = self.cb.map().find_closest_build_site(def, anchor, 96.0, 8, 0);
        if site.x < 0.0 || distance(site, leader.pos()) > WORK_RADIUS || self.war.view(u.def()).get_threat(site) > 0.0 {
            return false;
        }
        if u.build(def, site, 0, 0) {
            let f = self.taskforces.siege.forces.get_mut(&sid).unwrap();
            f.support.turrets.push(site);
            f.support.turret_at = self.frame;
            f.support.orders.insert(u.id(), (-def.id(), 0));
            return true;
        }
        false
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn repair_order_and_no_self_repair() {
        let c = [(1, 0, 0.5), (2, 0, 0.9), (3, 1, 0.1), (4, 2, 0.01)];
        assert_eq!(repair_target(1, &c), Some(2));
        assert_eq!(repair_target(1, &c[2..]), Some(3));
        assert_eq!(repair_target(1, &c[3..]), Some(4));
        assert_eq!(repair_target(1, &[(1, 0, 0.5), (2, 0, 1.0)]), None);
    }
    #[test]
    fn a_pedestal_needs_local_value_not_three_buildings() {
        assert_eq!(concentration(&[(Vec3::xz(100.0, 100.0), 1500.0)]), ENEMY_VALUE);
        assert_eq!(concentration(&[(Vec3::xz(100.0, 100.0), 750.0), (Vec3::xz(300.0, 100.0), 750.0)]), ENEMY_VALUE);
        assert!(concentration(&[(Vec3::xz(100.0, 100.0), 1499.0)]) < ENEMY_VALUE);
        assert!(concentration(&[(Vec3::xz(100.0, 100.0), 750.0), (Vec3::xz(700.0, 100.0), 750.0)]) < ENEMY_VALUE);
    }
    #[test]
    fn escorts_scale_between_two_and_four() {
        assert_eq!(escort_count(0.0), 2);
        assert_eq!(escort_count(1500.0), 2);
        assert_eq!(escort_count(4500.0), 3);
        assert_eq!(escort_count(10000.0), 4);
    }
}
