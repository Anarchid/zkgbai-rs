//! A completed opening heavy keeps its mission across normal handler ticks.
//! All allied instances can screen it and lend engineers; only its owner
//! drives it. Crab terrain work remains in the Crab doctrine alone.
use std::collections::BTreeMap;

use serde_json::json;
use springai::{Unit, UnitDef, Vec3};

use crate::Ai;
use crate::military::SquadId;
use crate::util::{NULLPOS, distance, get_directional_point};

pub(crate) struct Push {
    pub unit: Unit,
    owner: i32,
    target: Option<Vec3>,
    engineers: Vec<Unit>,
    screen: Option<SquadId>,
    turret_at: i32,
    last_order: i32,
}

impl Push {
    pub(crate) fn new(unit: Unit, owner: i32) -> Self {
        Self { unit, owner, target: None, engineers: Vec::new(), screen: None, turret_at: 0, last_order: 0 }
    }
}

/// Compact enemy metal, with a modest travel discount. Retain a productive
/// destination until another cluster offers at least 25% more value.
fn concentration_target(samples: &[(Vec3, f32)], from: Vec3, old: Option<Vec3>) -> Option<Vec3> {
    let mut cells: BTreeMap<(i32, i32), (Vec3, f32)> = BTreeMap::new();
    for &(p, value) in samples {
        let cell = cells.entry(((p.x / 400.0) as i32, (p.z / 400.0) as i32)).or_insert((p, 0.0));
        cell.1 += value.max(0.0);
    }
    let score = |p: Vec3| {
        let (x, z) = ((p.x / 400.0) as i32, (p.z / 400.0) as i32);
        let mut value = 0.0;
        for dx in -1..=1 {
            for dz in -1..=1 {
                value += cells.get(&(x + dx, z + dz)).map_or(0.0, |c| c.1);
            }
        }
        value / (1.0 + distance(from, p) / 8000.0)
    };
    let best = cells.values().map(|c| c.0).max_by(|a, b| score(*a).total_cmp(&score(*b)))?;
    Some(old.filter(|&p| score(p) > 0.0 && score(best) < score(p) * 1.25).unwrap_or(best))
}

impl Ai {
    fn rush_push_unit(&self) -> Option<Unit> {
        let p = self.eco.rush.push.as_ref()?;
        (p.unit.health() > 0.0 && p.unit.team() == p.owner && !p.unit.is_being_built()).then_some(p.unit)
    }

    pub(crate) fn rush_push_target(&self, id: i32) -> Option<Vec3> {
        let p = self.eco.rush.push.as_ref()?;
        (self.rush_push_unit()?.id() == id).then_some(p.target).flatten()
    }

    pub(crate) fn rush_push_escort(&self, id: i32) -> bool {
        self.rush_push_unit().is_some()
            && self.eco.rush.push.as_ref().is_some_and(|p| p.engineers.iter().any(|u| u.id() == id))
    }

    pub(crate) fn rush_push_update(&mut self, frame: i32) {
        if self.eco.rush.push.is_none() || frame % 30 != 0 {
            return;
        }
        let Some(heavy) = self.rush_push_unit() else {
            if let Some(p) = self.eco.rush.push.take() {
                self.eco_release_terraform(&p.engineers);
                for u in p.engineers {
                    if u.health() > 0.0 && u.team() == self.team_id {
                        u.stop(0);
                        if !self.eco.idlers.contains(&u.id()) {
                            self.eco.idlers.push_back(u.id());
                        }
                    }
                }
                self.op_emit("rush-push", json!({"event": "ended", "unit": p.unit.id()}));
            }
            return;
        };
        let Some(def) = heavy.def() else { return };
        if frame % 90 == 0 || self.eco.rush.push.as_ref().unwrap().target.is_none() {
            let hits = self.war.reach(Some(def));
            let samples: Vec<_> = self
                .war
                .targets
                .values()
                .filter(|e| !e.is_dead && e.position != NULLPOS && !e.can_fly() && self.war.can_hit(hits, e))
                .filter(|e| e.is_static || self.frame - e.last_seen <= 30 * 15)
                .filter(|e| {
                    def.is_able_to_fly() || self.terrain_reaches(def, heavy.pos(), e.position, def.max_weapon_range())
                })
                .map(|e| (e.position, e.value.max(50.0)))
                .collect();
            let old = self.eco.rush.push.as_ref().unwrap().target;
            let target = concentration_target(&samples, heavy.pos(), old).or_else(|| self.graph.get_enemy_base());
            self.eco.rush.push.as_mut().unwrap().target = target;
            if target != old {
                self.op_emit(
                    "rush-push",
                    json!({"event": "target", "unit": heavy.id(), "target": target.map(|p| [p.x,p.z])}),
                );
            }
        }
        // Crab keeps its perch and repair doctrine, but its mobile mission
        // uses the same concentration target. No other unit ever enters it.
        let crab = self.defs.name_of(heavy) == "spidercrabe";
        let crab_doctrine = crab
            && self
                .fighters
                .get(&heavy.id())
                .and_then(|f| f.squad)
                .and_then(|sid| self.squads.get(sid))
                .is_some_and(|s| s.shape.doctrine == "crab");
        if heavy.team() == self.team_id
            && !crab_doctrine
            && !self.retreat_is_retreating(heavy)
            && heavy.rules_param_float("disarmed", 0.0) <= 0.0
            && !self.cyclopes.holds(heavy.id(), frame)
            && let Some(target) = self.rush_push_target(heavy.id())
        {
            let p = self.eco.rush.push.as_mut().unwrap();
            // Avoid resetting weapons or the command queue every second.
            let on_course = heavy.current_commands().iter().any(|c| {
                let args = c.params();
                c.id() == 16 && args.len() >= 3 && distance(Vec3::new(args[0], args[1], args[2]), target) < 120.0
            });
            if !on_course && frame - p.last_order >= 90 {
                heavy.fight(target, 0);
                p.last_order = frame;
            }
        }
        self.rush_push_recruit_screen(heavy);
        // The Crab owner's existing doctrine already maintains its engineers.
        if !crab_doctrine {
            self.rush_push_engineers(heavy);
        }
    }

    fn rush_push_recruit_screen(&mut self, heavy: Unit) {
        let old = self.eco.rush.push.as_ref().and_then(|p| p.screen);
        let eligible = |sid: SquadId| {
            self.squads.get(sid).is_some_and(|s| {
                s.status == 'a'
                    && !s.fighters.is_empty()
                    && !self.qrf_drafted(sid)
                    && !self.coalesce_owns_squad(sid)
                    && self.op_squad_target(sid).is_none()
                    && distance(s.get_pos(&self.fighters), heavy.pos()) < 2400.0
            })
        };
        let screen = old.filter(|&sid| eligible(sid)).or_else(|| {
            self.raider_handler.raider_squads.iter().copied().filter(|&sid| eligible(sid)).min_by(|&a, &b| {
                let at =
                    |sid| self.squads.get(sid).map_or(f32::MAX, |s| distance(s.get_pos(&self.fighters), heavy.pos()));
                at(a).total_cmp(&at(b))
            })
        });
        self.eco.rush.push.as_mut().unwrap().screen = screen;
        if screen != old {
            self.op_emit("rush-push", json!({"event": "screen", "unit": heavy.id(), "squad": screen}));
        }
    }

    /// One active raider squad per allied AI screens the heavy. Its normal
    /// QRF/operator/retreat checks take priority; unreachable or lethal
    /// ground is never a screen point (especially underneath a flying Krow).
    pub(crate) fn rush_push_screen(&self, sid: SquadId) -> Option<Vec3> {
        let p = self.eco.rush.push.as_ref()?;
        if p.screen != Some(sid) {
            return None;
        }
        let heavy = self.rush_push_unit()?;
        let squad = self.squads.get(sid)?;
        let leader = squad.get_leader(&self.fighters)?;
        let def = leader.def()?;
        let goal = p.target?;
        let front = get_directional_point(heavy.pos(), goal, distance(heavy.pos(), goal).min(220.0));
        let rear = get_directional_point(heavy.pos(), self.graph.get_ally_base(), 220.0);
        [front, rear].into_iter().find(|&at| {
            self.terrain_reaches(def, leader.pos(), at, 0.0)
                && self.war.view(Some(def)).get_porc_threat(at) <= 0.0
                && self.war.view(Some(def)).get_threat(at) <= self.war.view(Some(def)).get_total_friendly_threat(at)
        })
    }

    pub(crate) fn rush_support_produce(&self, options: &[UnitDef]) -> Option<UnitDef> {
        let heavy = self.rush_push_unit()?;
        if self.defs.name_of(heavy) == "spidercrabe" && heavy.team() == self.team_id {
            return None;
        }
        let con = options.iter().copied().find(|d| d.is_builder() && d.speed() > 0.0 && d.is_able_to_repair())?;
        let have = self.cb.team_units().iter().filter(|u| u.def() == Some(con) && u.health() > 0.0).count();
        // Two stay with the economy, two join the push; unfinished ones count.
        (have < 4).then_some(con)
    }

    fn rush_push_engineers(&mut self, heavy: Unit) {
        let old = self.eco.rush.push.as_ref().unwrap().engineers.clone();
        let released: Vec<_> = old
            .into_iter()
            .filter(|u| {
                u.health() <= 0.0
                    || u.team() != self.team_id
                    || distance(u.pos(), heavy.pos()) > 4000.0
                    || !u.def().is_some_and(|d| self.terrain_reaches(d, u.pos(), heavy.pos(), d.build_distance()))
            })
            .collect();
        self.eco_release_terraform(&released);
        for u in &released {
            if u.health() > 0.0 && u.team() == self.team_id {
                if !self.eco.workers.get(&u.id()).is_some_and(|w| w.is_chicken) {
                    u.stop(0);
                }
                if !self.eco.idlers.contains(&u.id()) {
                    self.eco.idlers.push_back(u.id());
                }
            }
        }
        let p = self.eco.rush.push.as_mut().unwrap();
        p.engineers.retain(|u| !released.iter().any(|r| r.id() == u.id()));
        p.engineers.retain(|u| u.health() > 0.0 && u.team() == self.team_id);
        let have = p.engineers.len();
        let mut free: Vec<_> = self
            .eco
            .workers
            .values()
            .filter(|w| !w.is_com && !w.is_chicken && !w.assigned_plop)
            .filter(|w| w.unit.health() > 0.0 && !w.unit.is_being_built() && !self.eco_worker_held(w.id))
            .filter(|w| !crate::economy::terraform::is_terraforming(w.unit))
            .filter(|w| {
                w.unit.def().is_some_and(|d| {
                    d.is_able_to_repair() && self.terrain_reaches(d, w.get_pos(), heavy.pos(), d.build_distance())
                })
            })
            .map(|w| (distance(w.get_pos(), heavy.pos()), w.id))
            .collect();
        free.sort_by(|a, b| a.0.total_cmp(&b.0));
        let take = 2usize.saturating_sub(have).min(free.len().saturating_sub(2));
        for (_, id) in free.into_iter().filter(|(d, _)| *d < 3000.0).take(take) {
            if let Some(w) = self.eco.workers.get_mut(&id) {
                w.clear_task(&mut self.eco.tasks);
                self.eco.rush.push.as_mut().unwrap().engineers.push(w.unit);
            }
        }
        let escorts = self.eco.rush.push.as_ref().unwrap().engineers.clone();
        if escorts.len() != have || !released.is_empty() {
            self.op_emit(
                "rush-push",
                json!({"event": "engineers", "unit": heavy.id(),
                "escorts": escorts.iter().map(|u| u.id()).collect::<Vec<_>>() }),
            );
        }
        self.eco_hold_terraform(&escorts, 90);
        for (i, u) in escorts.into_iter().enumerate() {
            if self.eco.workers.get(&u.id()).is_some_and(|w| w.is_chicken) {
                continue;
            }
            let Some(def) = u.def() else { continue };
            if !self.terrain_reaches(def, u.pos(), heavy.pos(), def.build_distance()) {
                continue;
            }
            let nearby = self.cb.friendly_units_in_cylinder(heavy.pos(), 350.0);
            let repair = std::iter::once(heavy)
                .chain(nearby.iter().copied())
                .filter(|t| {
                    t.id() != u.id() && t.health() > 0.0 && !t.is_being_built() && t.health() < t.max_health() * 0.98
                })
                .min_by(|a, b| {
                    (a.id() != heavy.id())
                        .cmp(&(b.id() != heavy.id()))
                        .then((a.health() / a.max_health().max(1.0)).total_cmp(&(b.health() / b.max_health().max(1.0))))
                });
            if let Some(t) = repair {
                order_unit(u, 40, t, |u, t| {
                    u.repair(t, 0);
                });
            } else if i == 1 && self.rush_push_turret(u, heavy, &nearby) {
                // The second engineer fortifies behind contact while the first guards.
            } else {
                order_unit(u, 25, heavy, |u, t| {
                    u.guard(t, 0);
                });
            }
        }
    }

    fn rush_push_turret(&mut self, u: Unit, heavy: Unit, nearby: &[Unit]) -> bool {
        let Some(def) = self.defs.get("turretlaser") else { return false };
        if !u.def().is_some_and(|d| d.build_options().contains(&def)) {
            return false;
        }
        if let Some(t) = nearby.iter().find(|t| self.defs.name_of(**t) == "turretlaser" && t.is_being_built()) {
            order_unit(u, 40, *t, |u, t| {
                u.repair(t, 0);
            });
            return true;
        }
        if u.current_commands().iter().any(|c| c.id() == -def.id()) {
            return true;
        }
        if self.frame - self.eco.rush.push.as_ref().unwrap().turret_at < 30 * 60
            || distance(u.pos(), heavy.pos()) > 350.0
            || nearby.iter().filter(|t| self.defs.name_of(**t) == "turretlaser").count() >= 2
            || !self.war.targets.values().any(|e| {
                !e.is_dead
                    && (e.is_static || self.frame - e.last_seen < 300)
                    && distance(e.position, heavy.pos()) < 900.0
            })
        {
            return false;
        }
        let anchor = get_directional_point(heavy.pos(), self.graph.get_ally_base(), 200.0);
        let site = self.cb.map().find_closest_build_site(def, anchor, 100.0, 8, 0);
        if site.x < 0.0
            || !self.eco_build_check(def, site, 0)
            || distance(site, heavy.pos()) > 350.0
            || self.war.view(u.def()).get_effective_threat(site) > 0.0
            || !u.def().is_some_and(|d| self.terrain_reaches(d, u.pos(), site, d.build_distance()))
        {
            return false;
        }
        if u.build(def, site, 0, 0) {
            self.eco.rush.push.as_mut().unwrap().turret_at = self.frame;
            return true;
        }
        false
    }
}

fn order_unit(u: Unit, cmd: i32, target: Unit, order: impl FnOnce(Unit, Unit)) {
    if !u
        .current_commands()
        .iter()
        .any(|c| c.id() == cmd && c.params().first().is_some_and(|p| *p as i32 == target.id()))
    {
        order(u, target);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn concentration_beats_nearest_straggler_and_keeps_a_productive_front() {
        let close = Vec3::xz(100.0, 100.0);
        let mass = Vec3::xz(3000.0, 3000.0);
        let samples = [(close, 100.0), (mass, 2000.0), (Vec3::xz(3100.0, 3000.0), 1000.0)];
        assert_eq!(concentration_target(&samples, Vec3::ZERO, None), Some(mass));
        assert_eq!(concentration_target(&samples, Vec3::ZERO, Some(close)), Some(mass));
        assert_eq!(concentration_target(&samples, Vec3::ZERO, Some(mass)), Some(mass));
        assert_eq!(concentration_target(&[], Vec3::ZERO, Some(mass)), None);
    }
}
