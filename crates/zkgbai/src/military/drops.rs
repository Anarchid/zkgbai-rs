//! Air drops: Charons (light air transports, one unit each) fly raiders
//! past the chokepoints guarding the enemy's value and drop them where
//! that value is far by ground (see
//! [`Analysis::drops`](zk_terrain::strategy::Analysis)).
//!
//! With a gunship factory, the AI keeps a flight of Charons. Once they are
//! all at home and enough light raiders are about, they load one each, fly
//! along a path that avoids anti-air to an undefended enemy metal spot, and
//! drop them; the raiders then raid from there on their own and the
//! Charons fly home for the next sortie.

use std::collections::BTreeMap;

use springai::{NO_TIMEOUT, Unit, Vec3};
use zk_terrain::paths::SNAP;

use crate::Ai;
use crate::kgbutil::{PathCost, kgb_move_to};
use crate::military::Fighter;
use crate::nav;
use crate::terrain::pos;
use crate::util::{OPTION_SHIFT_KEY, distance};

/// Engine commands.
const CMD_LOAD_UNITS: i32 = 75;
const CMD_UNLOAD_UNITS: i32 = 80;

/// Frames between updates.
const PERIOD: i32 = 30;

/// Charons per sortie.
const FLIGHT: usize = 4;

/// Drops are worth making from this income on.
const MIN_INCOME: f32 = 20.0;

/// Frames between sorties, and before an unfulfilled production order is
/// forgotten.
const SORTIE_DELAY: i32 = 30 * 60;
const ORDER_TIMEOUT: i32 = 30 * 60;

/// Frames allowed for loading, and for the whole sortie.
const LOAD_TIMEOUT: i32 = 30 * 30;
const SORTIE_TIMEOUT: i32 = 30 * 120;

/// Raiders this far from the Charons are not picked up.
const PICKUP_RANGE: f32 = 2000.0;

/// What a Charon can carry (its `transportMass` and `transportSize`).
const MAX_MASS: f32 = 330.0;
const MAX_SIZE: i32 = 4;

/// Unload radius around the drop point, elmos.
const DROP_RADIUS: f32 = 120.0;

/// Enemy mobile strength a flight of raiders takes on at the drop (on the
/// scale of enemy danger).
const CARGO_POWER: f32 = 120.0;

/// Drop regions considered, best first.
const DROP_REGIONS: usize = 8;

/// A Charon and the raider it carries.
#[derive(Clone, Copy)]
struct Load {
    charon: Unit,
    cargo: Unit,
    landed: bool,
}

#[derive(Clone, Copy, PartialEq)]
enum Stage {
    Loading,
    Flying,
}

struct Sortie {
    loads: Vec<Load>,
    drop: Vec3,
    stage: Stage,
    since: i32,
}

/// Our Charons and the sortie under way.
#[derive(Default)]
pub struct Drops {
    charons: BTreeMap<i32, Unit>,
    sortie: Option<Sortie>,
    /// When a Charon was queued at a factory.
    ordered: Option<i32>,
    last_sortie: i32,
}

impl Ai {
    /// Whether a gunship factory should build a Charon now.
    pub fn drops_want_charon(&self) -> bool {
        let d = &self.drops;
        !self.dev_off("drops")
            && d.charons.len() < FLIGHT
            && d.ordered.is_none_or(|f| self.frame - f > ORDER_TIMEOUT)
            && self.eco.adjusted_income >= MIN_INCOME
            && self.raider_handler.small_raiders.len() >= FLIGHT
            // Somewhere guarded by chokepoints, or out of reach, by ground.
            && self.strategy.as_ref().is_some_and(|s| {
                s.analysis.drops.iter().take(DROP_REGIONS).any(|d| d.guards > 0 || d.ground_distance.is_none())
            })
    }

    /// A factory has been told to build a Charon.
    pub fn drops_ordered(&mut self) {
        self.drops.ordered = Some(self.frame);
        self.log(&format!("drops: Charon {} of {FLIGHT} ordered", self.drops.charons.len() + 1));
    }

    pub fn drops_add_charon(&mut self, unit: Unit) {
        self.drops.ordered = None;
        self.drops.charons.insert(unit.id(), unit);
        self.drops_go_home(unit);
    }

    fn drops_go_home(&self, charon: Unit) {
        let home = self.graph.get_ally_center();
        if home.x >= 0.0 {
            charon.move_to(home, 0);
        }
    }

    pub fn drops_update(&mut self, frame: i32) {
        if (frame + self.offset + 23) % PERIOD != 0 {
            return;
        }
        let team = self.team_id;
        self.drops.charons.retain(|_, u| u.health() > 0.0 && u.team() == team);
        if self.drops.sortie.is_some() {
            self.drops_fly(frame);
        } else if self.drops.charons.len() >= FLIGHT && frame - self.drops.last_sortie >= SORTIE_DELAY {
            self.drops_launch(frame);
        }
    }

    /// An enemy metal spot without static or anti-air defenses, and with
    /// no more enemies about than the raiders take on, in the best drop
    /// region that has one (the most valuable, least defended there).
    fn drops_target(&self) -> Option<Vec3> {
        let s = self.strategy.as_ref()?;
        let map = self.terrain()?.tier(s.tier)?;
        let war = &self.war;
        s.analysis.drops.iter().take(DROP_REGIONS).find_map(|d| {
            self.graph
                .metal_spots
                .iter()
                .filter(|m| m.hostile && map.region_near(pos(m.position), SNAP) == Some(d.region))
                .filter(|m| {
                    war.get_porc_threat(m.position) <= 0.0
                        && war.get_aa_threat(m.position) <= 0.0
                        && war.get_threat(m.position) <= CARGO_POWER
                })
                .max_by(|a, b| {
                    let score = |m: &&crate::graph::MetalSpot| m.value / (1.0 + war.get_threat(m.position));
                    score(a).total_cmp(&score(b))
                })
                .map(|m| m.position)
        })
    }

    /// Pick up a raider each and set off, if there is somewhere to go.
    fn drops_launch(&mut self, frame: i32) {
        let Some(drop) = self.drops_target() else { return };
        let charons: Vec<Unit> = self.drops.charons.values().copied().collect();
        let Some(&first) = charons.first() else { return };
        let base = first.pos();
        // Light raiders still forming squads near the Charons, nearest first.
        let mut cargo: Vec<(f32, Unit)> = self
            .raider_handler
            .small_raiders
            .iter()
            .filter_map(|id| self.fighters.get(id))
            .filter(|f| f.squad.and_then(|s| self.squads.get(s)).is_none_or(|s| s.status == 'f'))
            .map(|f| f.unit)
            .filter(|u| {
                u.def().is_some_and(|d| d.mass() <= MAX_MASS && d.move_data().is_some_and(|m| m.x_size() <= MAX_SIZE))
                    && !self.retreat_is_retreating(*u)
            })
            .map(|u| (distance(u.pos(), base), u))
            .filter(|&(d, _)| d <= PICKUP_RANGE)
            .collect();
        if cargo.len() < FLIGHT {
            return;
        }
        cargo.sort_by(|a, b| a.0.total_cmp(&b.0));
        let mut loads = Vec::new();
        for (&charon, &(_, raider)) in charons.iter().zip(&cargo).take(FLIGHT) {
            self.raider_remove_unit(raider);
            self.retreat_remove_unit(raider);
            raider.stop(0);
            charon.execute_custom_command(CMD_LOAD_UNITS, &[raider.id() as f32], 0, NO_TIMEOUT);
            loads.push(Load { charon, cargo: raider, landed: false });
        }
        self.log(&format!("drops: {} Charons loading raiders for ({:.0}, {:.0})", loads.len(), drop.x, drop.z));
        self.drops.sortie = Some(Sortie { loads, drop, stage: Stage::Loading, since: frame });
    }

    fn drops_fly(&mut self, frame: i32) {
        let Some(s) = self.drops.sortie.as_ref() else { return };
        let (drop, stage, since) = (s.drop, s.stage, s.since);
        let mut loads = s.loads.clone();
        let map = self.cb.map();
        let carried = |l: &Load| {
            let p = l.cargo.pos();
            distance(p, l.charon.pos()) < 40.0 && p.y > map.elevation_at(p.x, p.z).max(0.0) + 10.0
        };
        let team = self.team_id;
        let alive = |u: Unit| u.health() > 0.0 && u.team() == team;

        match stage {
            Stage::Loading => {
                let all = loads.iter().all(|l| !alive(l.cargo) || !alive(l.charon) || carried(l));
                if all || frame - since > LOAD_TIMEOUT {
                    for l in loads.iter_mut() {
                        if alive(l.charon) && alive(l.cargo) && carried(l) {
                            self.drops_route(l.charon, drop);
                        } else {
                            // Not picked up: back to raiding.
                            l.landed = true;
                            self.drops_release(l.cargo);
                            if alive(l.charon) {
                                self.drops_go_home(l.charon);
                            }
                        }
                    }
                    if let Some(s) = self.drops.sortie.as_mut() {
                        s.stage = Stage::Flying;
                        s.since = frame;
                    }
                    self.log("drops: flight away");
                }
            }
            Stage::Flying => {
                let late = frame - since > SORTIE_TIMEOUT;
                for l in loads.iter_mut().filter(|l| !l.landed) {
                    if !alive(l.cargo) {
                        l.landed = true;
                    } else if !carried(l) {
                        // Dropped (or its Charon was shot down over it).
                        l.landed = true;
                        self.drops_release(l.cargo);
                        if alive(l.charon) {
                            self.drops_go_home(l.charon);
                        }
                    } else if late || !alive(l.charon) {
                        let p = l.charon.pos();
                        l.charon.execute_custom_command(CMD_UNLOAD_UNITS, &[p.x, p.y, p.z, DROP_RADIUS], 0, NO_TIMEOUT);
                    }
                }
            }
        }

        let done = loads.iter().all(|l| l.landed);
        if let Some(s) = self.drops.sortie.as_mut() {
            s.loads = loads;
        }
        if done {
            self.log("drops: sortie over");
            self.drops.sortie = None;
            self.drops.last_sortie = frame;
        }
    }

    /// Fly a loaded Charon to the drop along a path that avoids anti-air,
    /// and unload there.
    fn drops_route(&mut self, charon: Unit, drop: Vec3) {
        let frame = self.frame;
        let nav = nav!(self);
        let mut path = nav.pathfinder.find_path(nav.war, charon, drop, PathCost::AirPath);
        path.pop_back();
        let mut opts = 0;
        while let Some(p) = path.pop_front() {
            if path.len().is_multiple_of(3) {
                kgb_move_to(charon, p, opts, frame);
                opts = OPTION_SHIFT_KEY;
            }
        }
        charon.execute_custom_command(CMD_UNLOAD_UNITS, &[drop.x, drop.y, drop.z, DROP_RADIUS], opts, NO_TIMEOUT);
    }

    /// A raider is on the ground again: it raids on its own from here.
    fn drops_release(&mut self, raider: Unit) {
        if raider.health() <= 0.0 || raider.team() != self.team_id {
            return;
        }
        let Some(def) = raider.def() else { return };
        let cost = def.cost(self.metal);
        self.raider_add_solo_raider(Fighter::new_raider(raider, cost, self.team_id));
        self.retreat_add_coward(raider);
    }
}
