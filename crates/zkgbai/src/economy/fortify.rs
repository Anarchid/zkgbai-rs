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
pub(super) fn seeds(points: &[Pos], range: f32) -> Vec<Pos> {
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

/// Credit each visible sample independently. Complementary turrets can
/// guard a front together, without each having to reach both endpoints.
fn coverage<const N: usize>(
    ground: &FireGround,
    site: Vec3,
    height: f32,
    radius: f32,
    range: f32,
    targets: &[Pos; N],
) -> [bool; N] {
    let muzzle = ground.ground(site.x, site.z).max(0.0) + height;
    targets.map(|p| {
        let y = ground.ground(p.x, p.z).max(0.0) + 16.0;
        pos(site).distance(p).hypot((y - muzzle).abs()) <= range
            && ground.line_is_free([site.x, muzzle, site.z], [p.x, y, p.z], 0.0, radius + 16.0, 0.0)
    })
}

/// Only unsatisfied samples earn value; saturated overlap earns nothing.
fn marginal(have: &[usize; 3], covered: [bool; 3], wanted: usize) -> f32 {
    have.iter().zip(covered).filter(|(n, hit)| **n < wanted && *hit).map(|(n, _)| 1.0 / (3 * (n + 1)) as f32).sum()
}

/// Keep immediate approaches around the protected asset in reach even
/// when moving its guard toward a gate. Covering only its centre is too weak.
pub(super) fn asset_front(at: Vec3) -> [Pos; 5] {
    let p = pos(at);
    [p, Pos::new(p.x - 96.0, p.z), Pos::new(p.x + 96.0, p.z), Pos::new(p.x, p.z - 96.0), Pos::new(p.x, p.z + 96.0)]
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
    /// opportunistic orders on disable; asset guards and started structures
    /// retain their safety checks until finished by the ordinary worker loop.
    pub(crate) fn eco_fortify_switch_changed(&mut self, off: bool) {
        if off {
            self.eco_disable_lotus_pool();
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
                        .is_some_and(|c| c.fortification && !c.point_defense && c.target.is_none())
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

    /// Do not infer coverage through cliffs from a weapon range circle.
    pub(super) fn eco_fire_coverage<const N: usize>(&self, def: UnitDef, site: Vec3, targets: &[Pos; N]) -> [bool; N] {
        let Some(ground) = self.precision.ground() else { return [false; N] };
        if def.name().starts_with("turretaa") {
            return [false; N];
        }
        let range = def
            .weapon_mounts()
            .iter()
            .filter_map(|m| m.weapon_def())
            .filter(|w| !w.name().contains("fake") && !w.is_shield() && w.is_able_to_attack_ground())
            .map(|w| w.range())
            .fold(0.0, f32::max);
        coverage(ground, site, 0.8 * def.height(), def.radius(), (range - 32.0).max(0.0), targets)
    }

    /// Include all sources of ground defenses, with nanoframes counted once.
    pub(super) fn eco_fire_positions(&self) -> Vec<(UnitDef, Vec3, Option<i32>)> {
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
        defenses
    }

    fn eco_front_counts(&self, targets: &[[Pos; 3]]) -> Vec<[usize; 3]> {
        let defenses = self.eco_fire_positions();
        targets
            .iter()
            .map(|t| {
                let mut count = [0; 3];
                for (def, p, _) in &defenses {
                    for (n, hit) in count.iter_mut().zip(self.eco_fire_coverage(*def, *p, t)) {
                        *n += usize::from(hit);
                    }
                }
                count
            })
            .collect()
    }

    pub(super) fn eco_shared_site_travel(&self, def: UnitDef, site: Vec3) -> Option<f32> {
        self.eco
            .workers
            .values()
            .filter(|w| {
                !super::terraform::is_terraforming(w.unit)
                    && w.unit.def().is_some_and(|d| d.build_options().iter().any(|b| b.id() == def.id()))
            })
            .filter_map(|w| self.eco_fortify_travel(w, site))
            .min_by(f32::total_cmp)
    }

    /// Fulfil a point-defense request with an existing/planned turret of
    /// the requested type, or a shared gate site that still guards the asset.
    /// False retains the caller's ordinary local placement as a fallback.
    pub(super) fn eco_share_point_defense(&mut self, def: UnitDef, asset: Vec3, preferred: Vec3) -> bool {
        if self.dev_off("fortify") || def.name() == "turrettorp" {
            return false;
        }
        let guard = asset_front(asset);
        let existing = self
            .eco_fire_positions()
            .into_iter()
            .find(|(d, p, _)| d.id() == def.id() && self.eco_fire_coverage(*d, *p, &guard).iter().all(|&hit| hit));
        if let Some((_, site, unit)) = existing {
            // A gate order adopted as an asset guard is no longer merely
            // opportunistic. Keep its reservation across a live disable.
            for &id in &self.eco.porc_tasks {
                let Some(task) = self.eco.tasks.get_mut(id) else { continue };
                let at = task.position;
                if let Some(c) = task.as_construction_mut()
                    && c.fortification
                    && c.build_type.id() == def.id()
                    && (distance(at, site) < 1.0 || unit.is_some_and(|id| c.target.is_some_and(|u| u.id() == id)))
                {
                    c.point_defense = true;
                }
            }
            return true;
        }
        if self.eco_defer_point_lotus(def, asset, preferred) {
            return true;
        }
        let Some(strategy) = &self.strategy else { return false };
        let Some(map) = self.terrain().and_then(|t| t.tier(strategy.tier)) else { return false };
        let gates: Vec<_> = strategy.fortify.iter().take(MAX_GATES).cloned().collect();
        let targets: Vec<_> = gates.iter().map(|g| front(map, g)).collect();
        let have = self.eco_front_counts(&targets);
        let wanted = self.eco_gate_demand();
        let range = def.max_weapon_range();
        let mut points = vec![pos(preferred), pos(asset)];
        points.extend(gates.iter().filter(|g| distance(g.pos, asset) <= 1.6 * range).map(|g| pos(g.pos)));
        let mut best: Option<(f32, Vec3)> = None;
        for seed in seeds(&points, range) {
            let site = self.cb.map().find_closest_build_site(def, Vec3::xz(seed.x, seed.z), 160.0, 3, 0);
            if site.x < 0.0
                || !self.eco_build_check(def, site, 0)
                || (def.name() == "turretlaser" && self.eco_lotus_crowded(site))
                || !self.eco_fire_coverage(def, site, &guard).iter().all(|&hit| hit)
                || (self.posture_on() && self.posture_shuns(site))
            {
                continue;
            }
            let value: f32 = gates
                .iter()
                .enumerate()
                .map(|(i, g)| g.score * marginal(&have[i], self.eco_fire_coverage(def, site, &targets[i]), wanted))
                .sum();
            if value <= 0.0 {
                continue;
            }
            let Some(travel) = self.eco_shared_site_travel(def, site) else { continue };
            let score = value * travel_discount(travel);
            if best.is_none_or(|b| score > b.0) {
                best = Some((score, site));
            }
        }
        let Some((_, site)) = best else { return false };
        let mut task = WorkerTask::construction(def, site, 0);
        let c = task.as_construction_mut().unwrap();
        c.fortification = true;
        c.point_defense = true;
        c.frame_issued = self.frame;
        let Some(id) = self.eco_add_construction(task, Some(List::Porc), Some(List::Porc)) else { return false };
        self.eco.fortify_tasks.insert(id);
        self.log(&format!(
            "terrain: {} at ({:.0}, {:.0}) shares gate coverage with point defense at ({:.0}, {:.0})",
            def.name(),
            site.x,
            site.z,
            asset.x,
            asset.z
        ));
        true
    }

    fn eco_gate_demand(&self) -> usize {
        1 + usize::from(self.eco.adjusted_income > 25.0) + usize::from(self.eco.adjusted_income > 45.0)
    }

    pub(super) fn eco_unmet_gate_layers(&self) -> std::collections::BTreeSet<(usize, usize)> {
        let Some(strategy) = &self.strategy else { return Default::default() };
        let Some(map) = self.terrain().and_then(|t| t.tier(strategy.tier)) else { return Default::default() };
        let gates: Vec<_> = strategy.fortify.iter().take(MAX_GATES).collect();
        let targets: Vec<_> = gates.iter().map(|g| front(map, g)).collect();
        let counts = self.eco_front_counts(&targets);
        let wanted = self.eco_gate_demand();
        gates
            .iter()
            .zip(counts)
            .flat_map(|(g, count)| (*count.iter().min().unwrap()..wanted).map(move |layer| (g.choke, layer)))
            .collect()
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
        let wanted = self.eco_gate_demand();
        let have = self.eco_front_counts(&targets);
        let mut types = Vec::new();
        for (i, g) in gates.iter().enumerate() {
            if have[i].iter().any(|&n| n < wanted)
                && let Some(d) = self.eco_shared_choke_turret(g.width, *have[i].iter().min().unwrap())
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
        let points: Vec<_> = gates
            .iter()
            .enumerate()
            .filter(|(i, _)| have[*i].iter().any(|&n| n < wanted))
            .map(|(_, g)| pos(g.pos))
            .collect();
        let mut deferred = Vec::new();
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
                // Stand on the home side of at least one useful gate.
                // Clear fire also counts across other region boundaries.
                let covered: Vec<_> = gates
                    .iter()
                    .enumerate()
                    .filter(|(i, _)| marginal(&have[*i], self.eco_fire_coverage(def, site, &targets[*i]), wanted) > 0.0)
                    .map(|(i, _)| i)
                    .collect();
                if !covered.iter().any(|&i| gates[i].home == region) || !self.eco_fortify_supported(site) {
                    continue;
                }
                if self.eco_lotus_crowded(site) {
                    for &i in &covered {
                        for layer in *have[i].iter().min().unwrap()..wanted {
                            deferred.push((
                                super::lotus::DemandKey::Gate(gates[i].choke, layer),
                                gates[i].pos,
                                targets[i].to_vec(),
                            ));
                        }
                    }
                    // The pooled-request planner spends this budget when
                    // it can afford a larger turret; never a third Lotus.
                    continue;
                }
                let Some(travel) = self.eco_shared_site_travel(def, site) else { continue };
                // Sum only uncovered demand: shared coverage earns value
                // immediately and existing/queued defenses remove it.
                let value: f32 = covered
                    .iter()
                    .map(|&i| {
                        gates[i].score * marginal(&have[i], self.eco_fire_coverage(def, site, &targets[i]), wanted)
                    })
                    .sum();
                let score = value * travel_discount(travel) / def.cost(self.metal).max(1.0);
                if best.as_ref().is_none_or(|b| score > b.0) {
                    best = Some((score, def, site, covered, travel));
                }
            }
        }
        for (key, at, targets) in deferred {
            self.eco_defer_lotus(key, at, targets);
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
    fn gate_samples_require_range_and_clear_fire() {
        let flat = FireGround::new(vec![0.0; 81 * 81], 80, 80);
        let site = Vec3::xz(100.0, 200.0);
        let gate = [Pos::new(300.0, 100.0), Pos::new(300.0, 200.0), Pos::new(300.0, 300.0)];
        assert_eq!(coverage(&flat, site, 30.0, 20.0, 300.0, &gate), [true; 3]);
        assert_eq!(coverage(&flat, site, 30.0, 20.0, 210.0, &gate), [false, true, false]);
        let mut heights = vec![0.0; 81 * 81];
        for z in 0..=80 {
            heights[z * 81 + 25] = 100.0;
        }
        let ridge = FireGround::new(heights, 80, 80);
        assert_eq!(coverage(&ridge, site, 30.0, 20.0, 300.0, &gate), [false; 3]);
    }

    #[test]
    fn complementary_turrets_satisfy_a_front_without_individually_covering_it() {
        let flat = FireGround::new(vec![0.0; 101 * 101], 100, 100);
        let gate = [Pos::new(300.0, 100.0), Pos::new(300.0, 350.0), Pos::new(300.0, 600.0)];
        let a = coverage(&flat, Vec3::xz(200.0, 200.0), 30.0, 20.0, 260.0, &gate);
        let b = coverage(&flat, Vec3::xz(200.0, 500.0), 30.0, 20.0, 260.0, &gate);
        assert_eq!(a, [true, true, false]);
        assert_eq!(b, [false, true, true]);
        let counts = std::array::from_fn(|i| usize::from(a[i]) + usize::from(b[i]));
        assert_eq!(marginal(&counts, [true; 3], 1), 0.0);
        // At a higher coverage target only the two outer gaps earn value.
        assert!((marginal(&counts, [true; 3], 2) - 1.0 / 3.0).abs() < 1e-6);
    }

    #[test]
    fn two_shared_reservations_saturate_a_triangle_of_gates() {
        let flat = FireGround::new(vec![0.0; 101 * 101], 100, 100);
        let centers = [Pos::new(150.0, 150.0), Pos::new(600.0, 150.0), Pos::new(375.0, 600.0)];
        for center in centers {
            let gate = [Pos::new(center.x - 40.0, center.z), center, Pos::new(center.x + 40.0, center.z)];
            let mut count = [0; 3];
            // The second position represents a queued turret: it reserves
            // coverage just like the already standing first turret.
            for site in [Vec3::xz(350.0, 300.0), Vec3::xz(400.0, 340.0)] {
                for (n, hit) in count.iter_mut().zip(coverage(&flat, site, 30.0, 20.0, 420.0, &gate)) {
                    *n += usize::from(hit);
                }
            }
            assert_eq!(count, [2; 3]);
            assert_eq!(marginal(&count, [true; 3], 2), 0.0);
        }
    }

    #[test]
    fn sharing_must_retain_the_assets_nearby_approaches() {
        let flat = FireGround::new(vec![0.0; 101 * 101], 100, 100);
        let asset = Vec3::xz(250.0, 300.0);
        let guard = asset_front(asset);
        let shared = Vec3::xz(400.0, 300.0);
        assert!(coverage(&flat, shared, 30.0, 20.0, 300.0, &guard).iter().all(|&hit| hit));
        // Moving further still covers the asset centre but leaves its rear
        // approach exposed, so it cannot fulfil this point-defense request.
        let too_far = Vec3::xz(500.0, 300.0);
        let hits = coverage(&flat, too_far, 30.0, 20.0, 300.0, &guard);
        assert!(hits[0]);
        assert!(!hits.iter().all(|&hit| hit));
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
