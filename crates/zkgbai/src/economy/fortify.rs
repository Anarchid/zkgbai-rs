//! Shared fire positions for valuable gates, admitted only with a safe
//! constructor approach. Queued turrets reserve coverage, never support.

use springai::{UnitDef, Vec3};
use zk_terrain::paths::Paths;
use zk_terrain::{Pos, TierMap};

use super::{List, Worker, WorkerTask};
use crate::Ai;
use crate::kgbutil::FireGround;
use crate::terrain::{Fortification, pos, tier_of};
use crate::util::distance;

const MAX_TRAVEL: f32 = 1800.0;
const PLAN_PERIOD: i32 = 150;
const MAX_GATES: usize = 16;
const MAX_SEEDS: usize = 64;
const SUPPORT_POWER: f32 = 150.0;

/// Smooth travel cost, separate from the distance of the gate from base.
pub(super) fn travel_discount(travel: f32) -> f32 {
    1.0 / (1.0 + travel / 600.0).powi(2)
}

fn supported(enemy: f32, friendly: f32, recent: f32, established: bool) -> bool {
    friendly >= 1.5 * enemy.max(recent) && (established || friendly >= SUPPORT_POWER)
}

/// Sample the whole hold front, including the sides when they are passable.
fn front(map: &TierMap, gate: &Fortification) -> [Pos; 3] {
    let c = &map.chokes[gate.choke];
    let (dx, dz) = c.gate.map_or_else(
        || {
            let home = map.regions[gate.home].center;
            (c.pos.z - home.z, home.x - c.pos.x)
        },
        |[a, b]| (b.x - a.x, b.z - a.z),
    );
    let len = dx.hypot(dz).max(1.0);
    let (x, z) = (dx / len * gate.width * 0.5, dz / len * gate.width * 0.5);
    [Pos::new(c.pos.x - x, c.pos.z - z), c.pos, Pos::new(c.pos.x + x, c.pos.z + z)]
}

/// Gate seeds and pair midpoints expose shared sites to the build-site
/// search. A cluster centroid also catches three-way intersections.
fn seeds(points: &[Pos], range: f32) -> Vec<Pos> {
    let mut out = points.to_vec();
    for (i, &a) in points.iter().enumerate() {
        let mut cluster = vec![a];
        for &b in &points[i + 1..] {
            if a.distance(b) <= 1.6 * range {
                cluster.push(b);
                out.push(Pos::new((a.x + b.x) * 0.5, (a.z + b.z) * 0.5));
            }
        }
        if cluster.len() > 2 {
            let n = cluster.len() as f32;
            out.push(Pos::new(
                cluster.iter().map(|p| p.x).sum::<f32>() / n,
                cluster.iter().map(|p| p.z).sum::<f32>() / n,
            ));
        }
    }
    out.truncate(MAX_SEEDS);
    out
}

/// Full front coverage; a ridge or an out-of-range endpoint invalidates
/// the gate even when its centre lies inside the range circle.
fn covers_front(ground: &FireGround, site: Vec3, height: f32, radius: f32, range: f32, targets: &[Pos; 3]) -> bool {
    let muzzle = ground.ground(site.x, site.z).max(0.0) + height;
    targets.iter().all(|p| {
        let y = ground.ground(p.x, p.z).max(0.0) + 16.0;
        pos(site).distance(*p).hypot((y - muzzle).abs()) <= range
            && ground.line_is_free([site.x, muzzle, site.z], [p.x, y, p.z], 0.0, radius + 16.0, 0.0)
    })
}

/// Check the approach between waypoints too: safe endpoints do not make
/// a route through enemy range safe.
fn safe_approach(waypoints: &[Vec3], safe: impl Fn(Vec3) -> bool) -> bool {
    waypoints.windows(2).all(|leg| {
        let steps = (distance(leg[0], leg[1]) / 128.0).ceil().max(1.0) as usize;
        (0..=steps).all(|k| {
            let t = k as f32 / steps as f32;
            safe(Vec3::xz(leg[0].x + t * (leg[1].x - leg[0].x), leg[0].z + t * (leg[1].z - leg[0].z)))
        })
    })
}

impl Ai {
    /// Recompute gates immediately on a live switch. Cancel unstarted
    /// shared-site orders on disable; started structures retain their
    /// safety checks until finished by the ordinary worker loop.
    pub(crate) fn eco_fortify_switch_changed(&mut self, off: bool) {
        if off {
            let pending: Vec<_> = self
                .eco
                .construction_tasks
                .iter()
                .copied()
                .filter(|&id| {
                    self.eco
                        .tasks
                        .get(id)
                        .and_then(|t| t.as_construction())
                        .is_some_and(|c| c.fortification && c.target.is_none())
                })
                .collect();
            for id in pending {
                self.logistics_close(id, "fortify disabled");
                self.eco.eco_logistics_drop(id);
                self.eco.fortify_tasks.remove(&id);
            }
        }
        self.eco.last_fortify_plan = self.frame - PLAN_PERIOD;
        self.strategy_refresh(self.frame);
    }

    /// No fortification push into unsupported fog. Owned infrastructure
    /// permits quiet home construction; forward sites need real firepower.
    pub(super) fn eco_fortify_supported(&self, p: Vec3) -> bool {
        let base = self.graph.get_start_pos();
        let base = if base.is_zero() { self.graph.get_ally_base() } else { base };
        let established =
            distance(base, p) < 650.0 || self.graph.get_owned_spots().iter().any(|s| distance(s.get_pos(), p) < 450.0);
        supported(
            self.war.get_enemy_power(p),
            self.war.get_total_friendly_power(p),
            self.eco_raiders_near(p),
            established,
        )
    }

    /// Ground route and support are checked for the actual worker's tier,
    /// including intermediate gates and samples along each segment. Aircraft
    /// use a direct route with their own anti-air threat view.
    pub(super) fn eco_fortify_travel(&self, worker: &Worker, target: Vec3) -> Option<f32> {
        if worker.is_chicken
            || worker.assigned_plop
            || worker.unit.is_being_built()
            || worker.unit.health() <= 0.0
            || self.eco_in_transit(worker.id)
        {
            return None;
        }
        let def = worker.unit.def()?;
        let from = worker.get_pos();
        if distance(from, target) > MAX_TRAVEL || !self.eco_fortify_supported(target) {
            return None;
        }
        let mut waypoints = vec![from];
        let travel = if def.is_able_to_fly() {
            distance(from, target)
        } else {
            let map = self.terrain()?.tier(tier_of(def)?)?;
            let paths = Paths::new(map, pos(from), |_| false);
            let travel = paths.distance_to(pos(target))?;
            waypoints
                .extend(paths.route(pos(target)).iter().map(|&c| Vec3::xz(map.chokes[c].pos.x, map.chokes[c].pos.z)));
            travel
        };
        if travel > MAX_TRAVEL {
            return None;
        }
        waypoints.push(target);
        let view = self.war.view(Some(def));
        let raiders = self.eco_recent_raiders();
        let flying = def.is_able_to_fly();
        let safe = safe_approach(&waypoints, |p| {
            // Ground support cannot make a constructor safe from anti-air.
            // Route samples also include the destination and its approach.
            (!flying || self.war.get_aa_threat(p) <= 0.0)
                && view.get_threat(p) * 1.5 <= view.get_total_friendly_threat(p)
                && raiders.iter().filter(|r| distance(r.0, p) <= 800.0).map(|r| r.1).sum::<f32>() * 1.5
                    <= self.war.get_total_friendly_power(p)
        });
        if !safe {
            return None;
        }
        Some(travel)
    }

    /// A conservative direct fire test against all three front samples.
    /// Do not infer coverage through cliffs from a weapon range circle.
    fn eco_covers_gate(&self, def: UnitDef, site: Vec3, targets: &[Pos; 3]) -> bool {
        let Some(ground) = self.precision.ground() else { return false };
        if def.name().starts_with("turretaa") {
            return false;
        }
        let range = def
            .weapon_mounts()
            .iter()
            .filter_map(|m| m.weapon_def())
            .filter(|w| !w.name().contains("fake") && !w.is_shield() && w.is_able_to_attack_ground())
            .map(|w| w.range())
            .fold(0.0, f32::max);
        covers_front(ground, site, 0.8 * def.height(), def.radius(), (range - 32.0).max(0.0), targets)
    }

    pub(super) fn eco_defend_shared_chokes(&mut self) -> bool {
        if self.frame - self.eco.last_fortify_plan < PLAN_PERIOD || self.eco.adjusted_income < 10.0 {
            return false;
        }
        self.eco.last_fortify_plan = self.frame;
        let eco = &mut self.eco;
        eco.fortify_tasks.retain(|&id| eco.tasks.get(id).is_some());
        let posture = self.posture_on();
        if posture
            && (self.posture_deficit() <= 0.0 || self.posture_waiting() >= (self.fac.num_workers / 2).max(2) as usize)
        {
            return false;
        }
        let Some(strategy) = &self.strategy else { return false };
        let Some(map) = self.terrain().and_then(|t| t.tier(strategy.tier)) else { return false };
        let gates: Vec<_> = strategy.fortify.iter().take(MAX_GATES).cloned().collect();
        let targets: Vec<_> = gates.iter().map(|g| front(map, g)).collect();
        let wanted = 1 + usize::from(self.eco.adjusted_income > 25.0) + usize::from(self.eco.adjusted_income > 45.0);
        // Nanoframes occur in both lists: count each reservation only once.
        let mut defenses: Vec<_> = self
            .eco
            .porcs
            .iter()
            .filter(|u| u.health() > 0.0)
            .filter_map(|u| Some((u.def()?, u.pos(), Some(u.id()))))
            .collect();
        for task in self.eco.porc_tasks.iter().filter_map(|&id| self.eco.tasks.get(id)) {
            let Some(c) = task.as_construction() else { continue };
            if c.target.is_some_and(|u| defenses.iter().any(|d| d.2 == Some(u.id()))) {
                continue;
            }
            defenses.push((c.build_type, task.position, c.target.map(|u| u.id())));
        }
        let have: Vec<_> = targets
            .iter()
            .map(|t| defenses.iter().filter(|(d, p, _)| self.eco_covers_gate(*d, *p, t)).count())
            .collect();
        let mut types = Vec::new();
        for (i, g) in gates.iter().enumerate() {
            if have[i] < wanted
                && let Some(d) = self.eco_shared_choke_turret(g.width, have[i])
                && !types.iter().any(|t: &UnitDef| t.id() == d.id())
            {
                types.push(d);
            }
        }
        // A longer-ranged first turret may cost less than several short
        // ones. Let it compete on value per metal before a gate has porc.
        for (name, income) in [("turretlaser", 10.0), ("turretheavylaser", 25.0), ("turretgauss", 60.0)] {
            if self.eco.adjusted_income >= income
                && let Some(d) = self.defs.get(name)
                && !types.iter().any(|t| t.id() == d.id())
            {
                types.push(d);
            }
        }
        let points: Vec<_> =
            gates.iter().enumerate().filter(|(i, _)| have[*i] < wanted).map(|(_, g)| pos(g.pos)).collect();
        let mut best: Option<(f32, UnitDef, Vec3, Vec<usize>, f32)> = None;
        for def in types {
            if posture && def.cost(self.metal) > self.posture_deficit() {
                continue;
            }
            let mut visited = Vec::new();
            for seed in seeds(&points, def.max_weapon_range()) {
                let site = self.cb.map().find_closest_build_site(def, Vec3::xz(seed.x, seed.z), 160.0, 3, 0);
                if site.x < 0.0 || visited.iter().any(|&p| distance(p, site) < 32.0) {
                    continue;
                }
                visited.push(site);
                if !self.eco_build_check(def, site, 0) || (posture && self.posture_shuns(site)) {
                    continue;
                }
                let Some(region) = map.region_at(pos(site)) else { continue };
                // Stand on the home side of at least one gate, and only
                // credit gates facing into this region.
                let covered: Vec<_> = gates
                    .iter()
                    .enumerate()
                    .filter(|(i, g)| {
                        have[*i] < wanted && g.home == region && self.eco_covers_gate(def, site, &targets[*i])
                    })
                    .map(|(i, _)| i)
                    .collect();
                if covered.is_empty() || !self.eco_fortify_supported(site) {
                    continue;
                }
                let travel = self
                    .eco
                    .workers
                    .values()
                    .filter(|w| {
                        !super::terraform::is_terraforming(w.unit)
                            && w.unit.def().is_some_and(|d| d.build_options().iter().any(|b| b.id() == def.id()))
                    })
                    .filter_map(|w| self.eco_fortify_travel(w, site))
                    .min_by(f32::total_cmp);
                let Some(travel) = travel else { continue };
                // Sum only uncovered demand: shared coverage earns value
                // immediately and existing/queued defenses remove it.
                let value: f32 = covered.iter().map(|&i| gates[i].score / (have[i] + 1) as f32).sum();
                let score = value * travel_discount(travel) / def.cost(self.metal).max(1.0);
                if best.as_ref().is_none_or(|b| score > b.0) {
                    best = Some((score, def, site, covered, travel));
                }
            }
        }
        if let Some((_, def, site, covered, travel)) = best {
            let mut task = WorkerTask::construction(def, site, 0);
            let c = task.as_construction_mut().unwrap();
            c.fortification = true;
            c.frame_issued = if posture { 0 } else { self.frame };
            if let Some(id) = self.eco_add_construction(task, Some(List::Porc), Some(List::Porc)) {
                self.eco.fortify_tasks.insert(id);
                if posture {
                    let cost = def.cost(self.metal);
                    self.posture.stats.front_orders += 1;
                    self.posture_ordered(id, site, cost, cost * 2.0, "fortify: shared gate coverage");
                    self.posture.defense += cost;
                }
                let ids: Vec<_> = covered.iter().map(|&i| gates[i].choke).collect();
                self.log(&format!(
                    "terrain: {} at ({:.0}, {:.0}) covers gates {ids:?}, constructor travel {travel:.0}",
                    def.name(),
                    site.x,
                    site.z
                ));
                return true;
            }
        }
        false
    }

    fn eco_shared_choke_turret(&self, width: f32, nth: usize) -> Option<UnitDef> {
        let income = self.eco.adjusted_income;
        self.defs.get(match nth {
            0 if width < 250.0 && income > 20.0 => "turretriot",
            0 => "turretlaser",
            1 => "turretheavylaser",
            _ if income > 60.0 => "turretgauss",
            _ if width < 250.0 => "turretemp",
            _ => "turretlaser",
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_site_reaches_two_gates_that_individual_sites_cannot() {
        let points = [Pos::new(0.0, 0.0), Pos::new(600.0, 0.0)];
        let candidates = seeds(&points, 400.0);
        assert!(candidates.iter().any(|p| points.iter().all(|q| p.distance(*q) < 400.0)));
        assert!(points.iter().all(|p| points.iter().filter(|q| p.distance(**q) < 400.0).count() == 1));
    }

    #[test]
    fn gate_coverage_requires_clear_fire_to_the_whole_front() {
        let flat = FireGround::new(vec![0.0; 81 * 81], 80, 80);
        let site = Vec3::xz(100.0, 200.0);
        let gate = [Pos::new(300.0, 100.0), Pos::new(300.0, 200.0), Pos::new(300.0, 300.0)];
        assert!(covers_front(&flat, site, 30.0, 20.0, 300.0, &gate));
        assert!(!covers_front(&flat, site, 30.0, 20.0, 210.0, &gate));
        let mut heights = vec![0.0; 81 * 81];
        for z in 0..=80 {
            heights[z * 81 + 25] = 100.0;
        }
        let ridge = FireGround::new(heights, 80, 80);
        assert!(!covers_front(&ridge, site, 30.0, 20.0, 300.0, &gate));
    }

    #[test]
    fn safe_endpoints_do_not_hide_a_dangerous_approach() {
        let from = Vec3::xz(0.0, 0.0);
        let to = Vec3::xz(1000.0, 0.0);
        let safe = |p: Vec3| distance(p, Vec3::xz(500.0, 0.0)) > 150.0;
        assert!(safe(from) && safe(to));
        assert!(!safe_approach(&[from, to], safe));
        assert!(safe_approach(&[from, Vec3::xz(0.0, 400.0), Vec3::xz(1000.0, 400.0), to], safe));
    }

    #[test]
    fn unsupported_frontier_and_recent_raids_are_hard_vetoes() {
        assert!(!supported(0.0, 0.0, 0.0, false));
        assert!(supported(0.0, 0.0, 0.0, true));
        assert!(supported(80.0, 150.0, 0.0, false));
        assert!(!supported(80.0, 100.0, 0.0, true));
        assert!(!supported(0.0, 150.0, 150.0, true));
        assert!(travel_discount(200.0) > 5.0 * travel_discount(1600.0));
    }
}
