//! A completed opening heavy keeps its mission across normal handler ticks.
//! All allied instances can screen it and lend engineers; only its owner
//! drives it as a Pack toward the enemy start zone. Normal Crabs retain
//! their siege doctrine; the opening rush stays mobile.
use std::sync::Arc;

use serde_json::json;
use springai::{NO_TIMEOUT, Unit, UnitDef, Vec3};

use crate::Ai;
use crate::defs::cmd;
use crate::military::taskforce::{Aim, Doctrine, Judge, Keep};
use crate::military::{SquadId, SquadKind};
use crate::nav;
use crate::util::{distance, get_directional_point};

pub(crate) struct Push {
    pub unit: Unit,
    owner: i32,
    target: Option<Vec3>,
    engineers: Vec<Unit>,
    screen: Option<SquadId>,
    turret_at: i32,
    force: Option<SquadId>,
    guard: Option<SquadId>,
}

impl Push {
    pub(crate) fn new(unit: Unit, owner: i32) -> Self {
        Self { unit, owner, target: None, engineers: Vec::new(), screen: None, turret_at: 0, force: None, guard: None }
    }
}

/// Lights still count as danger in combat predictions, but they never
/// earn a diversion or a heavy shot. Structures are lasting damage.
pub(crate) fn rush_prize(is_static: bool, is_raider: bool, value: f32) -> bool {
    is_static || (!is_raider && value >= 500.0)
}

pub(crate) fn rush_riot(name: &str) -> bool {
    matches!(
        name,
        "cloakriot" | "shieldriot" | "vehriot" | "tankriot" | "hoverriot" | "amphriot" | "spiderriot" | "shipriot"
    )
}

impl Ai {
    pub(crate) fn rush_push_unit(&self) -> Option<Unit> {
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
        if self.eco.rush.push.as_ref().unwrap().target.is_none() {
            // Inferred once from enemy start boxes, never from sighted units.
            let target = Some(self.graph.get_enemy_base().unwrap_or_else(|| {
                // Maps without enemy start-box metal spots still get a fixed
                // mission, mirrored across the map from our own starting side.
                let c = self.graph.get_map_center();
                let a = self.graph.get_ally_base();
                Vec3::xz(
                    (2.0 * c.x - a.x).clamp(32.0, 2.0 * c.x - 32.0),
                    (2.0 * c.z - a.z).clamp(32.0, 2.0 * c.z - 32.0),
                )
            }));
            self.eco.rush.push.as_mut().unwrap().target = target;
            self.op_emit(
                "rush-push",
                json!({"event": "mission", "unit": heavy.id(),
                "target": target.map(|p| [p.x, p.z])}),
            );
        }
        if heavy.team() == self.team_id
            && self.eco.rush.push.as_ref().unwrap().force.is_none()
            && self.fighters.contains_key(&heavy.id())
        {
            let sid = self.rush_force(heavy, "rush");
            self.eco.rush.push.as_mut().unwrap().force = Some(sid);
            self.retreat_remove_unit(heavy); // Pack owns its predicted repair sorties.
            // Filter cheap automatic targets without silencing the heavy on
            // its raw-move advance or retreat. Deliberate economy shots remain.
            heavy.execute_custom_command(cmd::PREVENT_BAIT, &[4.0], 0, NO_TIMEOUT);
            heavy.set_fire_state(2, 0);
        }
        self.rush_push_recruit_screen(heavy);
        self.rush_recruit_riot(heavy);
        self.rush_push_engineers(heavy);
    }

    fn rush_force(&mut self, unit: Unit, name: &str) -> SquadId {
        self.squad_leave_squad(unit.id());
        // Transfer the existing Fighter rather than letting two handlers drive it.
        self.misc_handler.loners.remove(&unit.id());
        self.misc_handler.krows.remove(&unit.id());
        self.misc_handler.striders.remove(&unit.id());
        self.misc_handler.arties.remove(&unit.id());
        self.squad_handler.fighters.insert(unit.id());
        let mut doctrine = Doctrine::new(name);
        doctrine.core = vec![self.defs.name_of(unit).into()];
        doctrine.aim = Aim::Pack;
        doctrine.core_keep = Keep::Formation;
        doctrine.cores_to_go = 1;
        doctrine.cores_to_stay = 1;
        doctrine.retreat_health = 0.3;
        doctrine.heal_to = 0.9;
        doctrine.judge = Judge::Fates;
        doctrine.fate_secs = 8.0;
        doctrine.pass_reach = 0.9;
        doctrine.lasting = 2.5;
        doctrine.fire_state = (name == "rush").then_some(2);
        let shape = Arc::new(doctrine.shape(&self.defs, &self.taskforces.classes));
        let sid = self.squads.create(SquadKind::Taskforce, self.team_id);
        self.squad_handler.taskforces.push_back(sid);
        let frame = self.frame;
        let mut nav = nav!(self);
        if let Some(s) = self.squads.get_mut(sid) {
            s.shape = shape;
            s.formed = frame;
            s.status = 'a';
            s.add_unit_nav(unit.id(), &mut self.fighters, &mut nav);
        }
        self.op_emit("rush-push", json!({"event": "force", "unit": unit.id(), "squad": sid, "role": name}));
        sid
    }

    fn rush_recruit_riot(&mut self, heavy: Unit) {
        if self
            .eco
            .rush
            .push
            .as_ref()
            .unwrap()
            .guard
            .is_some_and(|sid| self.squads.get(sid).is_some_and(|s| !s.is_dead(&self.fighters)))
        {
            return;
        }
        // One riot per allied instance; borrow ordinary troops, never another
        // doctrine's core, a QRF defender or an operator-controlled squad.
        let riot = self
            .fighters
            .values()
            .filter(|f| rush_riot(self.defs.name_of(f.unit)))
            .filter(|f| f.unit.health() > f.unit.max_health() * 0.5 && !self.retreat_is_retreating(f.unit))
            .filter(|f| !self.coalesce_owns(f.id) && !self.limpet_controls(f.id))
            .filter(|f| {
                f.squad.is_none_or(|sid| {
                    self.squads.get(sid).is_some_and(|s| {
                        !s.is_taskforce() && !self.qrf_drafted(sid) && self.op_squad_target(sid).is_none()
                    })
                })
            })
            .filter(|f| distance(f.get_pos(), heavy.pos()) < 4000.0)
            .filter(|f| f.unit.def().is_some_and(|d| self.terrain_reaches(d, f.get_pos(), heavy.pos(), 300.0)))
            .min_by(|a, b| distance(a.get_pos(), heavy.pos()).total_cmp(&distance(b.get_pos(), heavy.pos())))
            .map(|f| f.unit);
        if let Some(unit) = riot {
            let sid = self.rush_force(unit, "rush-guard");
            self.eco.rush.push.as_mut().unwrap().guard = Some(sid);
        }
    }

    fn rush_push_recruit_screen(&mut self, heavy: Unit) {
        let old = self.eco.rush.push.as_ref().and_then(|p| p.screen);
        let eligible = |sid: SquadId| {
            self.squads.get(sid).is_some_and(|s| {
                !s.fighters.is_empty()
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

    /// One raider squad per allied AI screens the heavy, including a forming
    /// squad rallying around its flank. Its normal
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
        if self.rush_push_unit().is_none()
            && !matches!(
                self.eco.rush.state,
                crate::economy::rush::State::Planned { .. } | crate::economy::rush::State::On(_)
            )
        {
            return None;
        }
        let units = self.cb.team_units();
        let con = options.iter().copied().find(|d| d.is_builder() && d.speed() > 0.0 && d.is_able_to_repair());
        let cons = con.map_or(0, |d| units.iter().filter(|u| u.def() == Some(d) && u.health() > 0.0).count());
        // Keep an economic constructor, then get a riot on the way early,
        // including nonparticipants while the heavy is still building.
        // Unfinished units count so requests cannot pile up.
        if cons < 1 && con.is_some() {
            return con;
        }
        if !units.iter().any(|u| u.health() > 0.0 && rush_riot(self.defs.name_of(*u)))
            && let Some(riot) = options
                .iter()
                .copied()
                .filter(|d| rush_riot(self.defs.name(*d)))
                .min_by(|a, b| a.cost(self.metal).total_cmp(&b.cost(self.metal)))
        {
            return Some(riot);
        }
        if cons < 4 { con } else { None }
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
    fn lights_never_earn_a_heavy_diversion_but_buildings_do() {
        assert!(!rush_prize(false, true, 900.0));
        assert!(!rush_prize(false, false, 499.0));
        assert!(rush_prize(false, false, 1000.0));
        assert!(rush_prize(true, false, 85.0));
        assert!(rush_riot("vehriot") && rush_riot("shieldriot"));
        assert!(!rush_riot("vehraid") && !rush_riot("gunshipkrow"));
    }
}
