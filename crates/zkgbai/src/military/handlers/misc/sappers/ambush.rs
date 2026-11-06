//! Observed squads, asset approaches and bounded deployment. All policy history
//! lives in MineReserve/Sapper; the route calculations themselves are stateless.
use super::*;
use std::collections::BTreeSet;
use zk_terrain::Tier;

const CLUMP_KEEP: i32 = 30 * 90;
const EXIT_GRACE: i32 = 30 * 3;
const STALL: i32 = 30 * 12;
const COMMIT: i32 = 30 * 15;
const BLOCKED_FOR: i32 = 30 * 30;
const IMP_RESPONSE: f32 = 4.0;

#[derive(Clone)]
pub(super) struct Clump {
    members: BTreeMap<i32, f32>,
    tier: Tier,
    position: Vec3,
    range: f32,
    speed: f32,
    value: f32,
    seen: i32,
}

#[derive(Clone, Copy)]
struct Contact {
    id: i32,
    tier: Tier,
    position: Vec3,
    range: f32,
    speed: f32,
    value: f32,
    seen: i32,
}

/// Disjoint radius-bounded groups, not a transitive chain stretching across
/// the map. Stable IDs and a densest-first seed make the result deterministic.
fn observed_clumps(mut contacts: Vec<Contact>) -> Vec<Clump> {
    contacts.sort_by_key(|c| c.id);
    let mut groups = Vec::new();
    while !contacts.is_empty() {
        let count = |a: &Contact| {
            contacts.iter().filter(|b| a.tier == b.tier && distance(a.position, b.position) <= CLUMP_RADIUS).count()
        };
        let seed = contacts.iter().max_by_key(|a| (count(a), std::cmp::Reverse(a.id))).unwrap();
        if count(seed) < CLUMP_UNITS {
            break;
        }
        let (tier, center) = (seed.tier, seed.position);
        let mut group = Clump {
            members: BTreeMap::new(),
            tier,
            position: Vec3::ZERO,
            range: 0.0,
            speed: f32::MAX,
            value: 0.0,
            seen: 0,
        };
        contacts.retain(|c| {
            if c.tier != tier || distance(center, c.position) > CLUMP_RADIUS {
                return true;
            }
            group.members.insert(c.id, c.value);
            group.position.x += c.position.x;
            group.position.z += c.position.z;
            group.range = group.range.max(c.range);
            group.speed = group.speed.min(c.speed);
            group.value += c.value;
            group.seen = group.seen.max(c.seen);
            false
        });
        let n = group.members.len() as f32;
        group.position.x /= n;
        group.position.z /= n;
        groups.push(group);
    }
    groups
}

fn remember_clumps(memory: &mut Vec<Clump>, fresh: Vec<Clump>, dead: &BTreeSet<i32>, frame: i32) {
    let observed: BTreeSet<_> = fresh.iter().flat_map(|g| g.members.keys().copied()).collect();
    for g in memory.iter_mut() {
        g.members.retain(|id, _| !dead.contains(id) && !observed.contains(id));
        g.value = g.members.values().sum();
    }
    memory.retain(|g| frame - g.seen <= CLUMP_KEEP && g.members.len() >= CLUMP_UNITS);
    memory.extend(fresh);
    memory.sort_by_key(|g| g.members.keys().next().copied());
}

pub(super) fn mine_quota(clumps: &[Clump], mine_cost: f32) -> usize {
    if !mine_cost.is_finite() || mine_cost <= 0.0 {
        return 0;
    }
    clumps
        .iter()
        .map(|g| {
            // Each slot needs a pair of clustered combat units and twice the
            // producing mine's metal cost. One expensive escort is not eight jobs.
            let affordable = (g.value / (2.0 * mine_cost)).floor() as usize;
            affordable.min(g.members.len() / CLUMP_UNITS).min(RESERVE_MINES)
        })
        .sum::<usize>()
        .min(RESERVE_MINES)
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Phase {
    Exit,
    Travel,
    Ready,
    Staging,
}

#[derive(Clone, Copy)]
pub(super) struct Deployment {
    pub(super) born: i32,
    pub(super) assigned: i32,
    pub(super) progress: i32,
    pub(super) at: Vec3,
    blocked: Option<(Vec3, i32)>,
    pub(super) staging: Option<Vec3>,
    pub phase: Phase,
}

impl Deployment {
    pub(super) fn new(frame: i32, at: Vec3) -> Self {
        Self { born: frame, assigned: frame, progress: frame, at, blocked: None, staging: None, phase: Phase::Exit }
    }
    pub(super) fn committed(&self, frame: i32) -> bool {
        frame - self.assigned < COMMIT
    }
    pub(super) fn exiting(&self, frame: i32, commands: i32) -> bool {
        self.phase == Phase::Exit && commands > 0 && frame - self.born < EXIT_GRACE
    }
    pub(super) fn stalled(&mut self, from: Vec3, frame: i32, moving: bool) -> bool {
        if !moving || distance(from, self.at) >= 48.0 {
            self.at = from;
            self.progress = frame;
        }
        moving && frame - self.progress >= STALL
    }
    pub(super) fn allows(&self, p: Vec3, frame: i32) -> bool {
        self.blocked.is_none_or(|(q, until)| frame >= until || distance(p, q) > 128.0)
    }
    pub(super) fn block(&mut self, p: Vec3, frame: i32) {
        self.blocked = Some((p, frame + BLOCKED_FOR));
        self.progress = frame;
        self.staging = None;
    }
    pub(super) fn assign(&mut self, from: Vec3, frame: i32) {
        self.assigned = frame;
        self.progress = frame;
        self.at = from;
        self.staging = None;
    }
}

pub(super) fn site_score(id: i32, from: Vec3, site: &Site, reservations: &[Reservation]) -> f32 {
    let load = reservations
        .iter()
        .filter(|r| r.id != id && r.site.is_some_and(|s| distance(s.gate, site.gate) < 256.0))
        .count();
    site.value
        / ((1.0 + distance(from, site.position) / 1600.0) * (1.0 + load as f32 * 2.0) * (1.0 + site.depth as f32))
}

/// Refresh the old score from this pass's live opportunities. A vanished job
/// cannot keep its historical score forever. Require a substantial improvement
/// and a minimum commitment before moving between still useful sites.
pub(super) fn refresh_site(old: Site, sites: &[Site], radius: f32, committed: bool, anchor_live: bool) -> Option<Site> {
    if let Some(live) = sites.iter().find(|s| {
        s.ambush == old.ambush
            && distance(s.gate, old.gate) < 32.0
            && distance(s.position, old.position) <= (radius * 0.5).max(64.0)
    }) {
        return Some(Site { value: live.value, ..old });
    }
    // A route sample disappearing is not an unsafe or destroyed asset. Keep
    // a live commitment through its dwell time; then retain only modest value
    // as a frontier guard, allowing a materially better expansion to win.
    anchor_live.then_some(Site { value: if committed { old.value } else { old.value.min(0.25) }, ..old })
}

pub(super) fn select_assignment(
    sapper: &Sapper,
    from: Vec3,
    sites: &[Site],
    reservations: &[Reservation],
    frame: i32,
) -> Option<Site> {
    let best = choose_site(sapper.unit.id(), from, sapper.radius, sites, reservations)?;
    let old = sapper
        .site
        .and_then(|old| {
            sites.iter().find(|s| distance(s.position, old.position) < 1.0 && distance(s.gate, old.gate) < 64.0)
        })
        .copied();
    if let Some(old) = old {
        let id = sapper.unit.id();
        if frame - sapper.deployment.assigned < COMMIT
            || site_score(id, from, &best, reservations) < site_score(id, from, &old, reservations) * 1.4
        {
            return Some(old);
        }
    }
    Some(best)
}

#[derive(Clone, Copy)]
struct Bait {
    position: Vec3,
    footprint: f32,
    value: f32,
}

/// Walk backwards along an actual attacker route, away from the asset. Unlike
/// projecting a direction vector, this stays on the approach around corners.
fn route_standoff(route: &[Vec3], mut lead: f32) -> Option<Vec3> {
    for pair in route.windows(2) {
        let d = distance(pair[0], pair[1]);
        if d >= lead {
            let t = lead / d;
            return Some(Vec3::xz(pair[0].x + (pair[1].x - pair[0].x) * t, pair[0].z + (pair[1].z - pair[0].z) * t));
        }
        lead -= d;
    }
    None
}

/// Every possible detonation along a straight charge must spare the bait.
pub(super) fn charge_clear(from: Vec3, to: Vec3, radius: f32, bystanders: &[Building]) -> bool {
    let (dx, dz) = (to.x - from.x, to.z - from.z);
    let length2 = dx * dx + dz * dz;
    bystanders.iter().all(|b| {
        let t = if length2 > 0.0 { ((b.center.x - from.x) * dx + (b.center.z - from.z) * dz) / length2 } else { 0.0 }
            .clamp(0.0, 1.0);
        distance(b.center, Vec3::xz(from.x + dx * t, from.z + dz * t)) >= radius + b.footprint + CLEARANCE
    })
}

impl Ai {
    pub(super) fn sapper_bystanders(&self) -> Vec<Building> {
        self.cb
            .friendly_units()
            .into_iter()
            .filter_map(|u| {
                let def = u.def()?;
                if u.health() <= 0.0 || (def.speed() > 0.0 && def.build_options().is_empty()) {
                    return None;
                }
                Some(Building { center: u.pos(), footprint: footprint(def) })
            })
            .collect()
    }

    pub(super) fn sapper_observe_clumps(&mut self, frame: i32) {
        let contacts = self
            .war
            .targets
            .values()
            .filter(|e| {
                !e.is_dead
                    && e.identified
                    && !e.is_static
                    && !e.can_fly()
                    && !e.is_worker
                    && e.can_attack()
                    && e.position.y >= 0.0
                    && (e.visible || frame - e.last_seen <= CLUMP_MEMORY)
            })
            .filter_map(|e| {
                Some(Contact {
                    id: e.unit_id,
                    tier: tier_of(e.ud?)?,
                    position: e.position,
                    range: e.def_max_weapon_range(),
                    speed: e.def_speed(),
                    value: e.value,
                    seen: if e.visible { frame } else { e.last_seen },
                })
            })
            .collect();
        let dead = self.war.targets.values().filter(|e| e.is_dead).map(|e| e.unit_id).collect();
        let before = (self.sapper_mine_quota("cloakbomb"), self.sapper_mine_quota("shieldbomb"));
        remember_clumps(&mut self.misc_handler.mines.clumps, observed_clumps(contacts), &dead, frame);
        let after = (self.sapper_mine_quota("cloakbomb"), self.sapper_mine_quota("shieldbomb"));
        if before != after {
            self.log(&format!(
                "sappers: {} observed clumps, mine targets Imp {} -> {}, Snitch {} -> {}",
                self.misc_handler.mines.clumps.len(),
                before.0,
                after.0,
                before.1,
                after.1
            ));
        }
    }

    fn sapper_baits(&self) -> Vec<Bait> {
        let mut assets: Vec<_> = self
            .cb
            .friendly_units()
            .into_iter()
            .filter_map(|u| {
                let def = u.def()?;
                if u.health() <= 0.0 || def.speed() > 0.0 {
                    return None;
                }
                Some(Bait { position: u.pos(), footprint: footprint(def), value: def.cost(self.metal).max(50.0) })
            })
            .collect();
        // Protect the builder's committed construction destination, not its
        // changing position on the way there. Once started, its nanoframe is
        // already in the actual-asset list above.
        assets.extend(self.eco.tasks.iter().filter_map(|task| {
            let build = task.as_construction()?;
            if task.assigned_workers.is_empty() || build.target.is_some() {
                return None;
            }
            Some(Bait {
                position: task.position,
                footprint: footprint(build.build_type),
                value: build.build_type.cost(self.metal).max(50.0),
            })
        }));
        assets
    }

    pub(super) fn sapper_live_anchor(&self, p: Vec3) -> bool {
        self.sapper_baits().iter().any(|a| distance(a.position, p) < 32.0)
    }

    pub(super) fn sapper_ambush_sites(&self, radius: f32) -> Vec<Site> {
        let Some(terrain) = self.terrain() else { return Vec::new() };
        let assets = self.sapper_baits();
        let mut sites = Vec::new();
        for group in &self.misc_handler.mines.clumps {
            let Some(map) = terrain.tier(group.tier) else { continue };
            let paths = Paths::new(map, pos(group.position), |_| false);
            let mut jobs: Vec<_> = assets
                .iter()
                .filter_map(|a| {
                    let travel = paths.distance_to(pos(a.position))?;
                    if travel > (group.speed * 90.0).max(2400.0) {
                        return None;
                    }
                    let predicted = self
                        .incursions
                        .predicted
                        .iter()
                        .any(|p| p.tier == group.tier && distance(p.pos, a.position) < 256.0);
                    let exposed = self.graph.is_front_line(a.position) || predicted;
                    let defense = self.war.get_total_friendly_power(a.position).max(0.0);
                    let score = (group.value.sqrt() + a.value.sqrt()) * if exposed { 2.0 } else { 0.5 }
                        / ((1.0 + travel / 1200.0) * (1.0 + defense));
                    Some((a, score))
                })
                .collect();
            jobs.sort_by(|a, b| b.1.total_cmp(&a.1));
            for (asset, value) in jobs.into_iter().take(16) {
                let mut route = vec![asset.position];
                route.extend(
                    paths
                        .route(pos(asset.position))
                        .into_iter()
                        .rev()
                        .map(|c| Vec3::xz(map.chokes[c].pos.x, map.chokes[c].pos.z)),
                );
                route.push(group.position);
                let safe = radius + asset.footprint + CLEARANCE + ARRIVED;
                // Intercept before a ranged attacker stops to shoot the bait.
                let lead = safe.max(group.range + asset.footprint + CLEARANCE);
                for offset in [0.0, radius * 0.75, radius * 1.5] {
                    let Some(p) = route_standoff(&route, lead + offset) else { continue };
                    if distance(p, asset.position) < safe || map.region_at(pos(p)).is_none() {
                        continue;
                    }
                    // Candidate must belong to this attacker's connected route,
                    // even when the mine itself can climb inaccessible cliffs.
                    if paths.distance_to(pos(p)).is_none() {
                        continue;
                    }
                    sites.push(Site { position: p, gate: asset.position, value, depth: 0, ambush: true });
                }
            }
        }
        sites
    }

    pub(super) fn sapper_response(&self, p: Vec3, radius: f32) -> bool {
        // A nearby combat unit can finish stunned prey within a short response
        // window. Builders, other mines, aircraft and paralyzer-only units do
        // not count as that follow-up.
        self.fighters.values().any(|f| {
            let Some(def) = f.unit.def() else { return false };
            if f.unit.health() <= 0.0 || def.is_able_to_fly() || mine_type(self.defs.name(def)) {
                return false;
            }
            let range = def
                .weapon_mounts()
                .iter()
                .filter_map(|w| {
                    let w = w.weapon_def()?;
                    (!w.is_paralyzer()).then_some(w.range())
                })
                .fold(0.0, f32::max);
            if range <= 0.0 || distance(f.unit.pos(), p) + radius * COVER > range + def.speed() * IMP_RESPONSE {
                return false;
            }
            let Some(map) = tier_of(def).and_then(|tier| self.terrain()?.tier(tier)) else { return false };
            Paths::new(map, pos(f.unit.pos()), |_| false)
                .distance_to(pos(p))
                .is_some_and(|d| d + radius * COVER <= range + def.speed() * IMP_RESPONSE)
        })
    }

    pub(super) fn sapper_stage(
        &mut self,
        unit: Unit,
        radius: f32,
        statics: &[Building],
        frame: i32,
        deployment: &Deployment,
    ) -> Option<Vec3> {
        let from = unit.pos();
        let reachable = self.pathfinder.raider_reachability(&self.war, unit, 0.0);
        let safe = |p: Vec3| {
            deployment.allows(p, frame)
                && clear_of_statics(true, false, p, radius, statics)
                && self.war.get_threat(p) <= 0.0
                && reachable.reaches(p)
        };
        if let Some(p) = deployment.staging.filter(|&p| safe(p)) {
            return Some(p);
        }
        // Move out of the production area, with a deterministic angle per unit.
        for ring in [radius + 256.0, radius + 512.0, radius + 896.0] {
            for i in 0..12 {
                let angle = (i + unit.id().rem_euclid(12)) as f32 * std::f32::consts::TAU / 12.0;
                let p = Vec3::xz(from.x + ring * angle.cos(), from.z + ring * angle.sin());
                if safe(p) {
                    return Some(p);
                }
            }
        }
        None
    }

    pub(super) fn sapper_move(&mut self, unit: Unit, mut p: Vec3) {
        use crate::kgbutil::pathfinder::PathCost;
        let route = self.pathfinder.find_path(&self.war, unit, p, PathCost::AvoidEnemies);
        let mut flags = 0;
        // Queued waypoints must not expire after 30 seconds before reaching
        // the front. The deployment watchdog owns their timeout instead.
        for waypoint in route.into_iter().filter(|q| distance(*q, p) > 48.0) {
            unit.execute_custom_command(
                cmd::RAW_MOVE,
                &[waypoint.x, waypoint.y, waypoint.z, 32.0],
                springai::opt::RIGHT_MOUSE_KEY | flags,
                NO_TIMEOUT,
            );
            flags = crate::util::OPTION_SHIFT_KEY;
        }
        p.y = self.cb.map().elevation_at(p.x, p.z);
        unit.execute_custom_command(
            cmd::RAW_MOVE,
            &[p.x, p.y, p.z, 4.0],
            springai::opt::RIGHT_MOUSE_KEY | flags,
            NO_TIMEOUT,
        );
    }
}

#[cfg(test)]
pub(super) fn test_clumps(n: usize) -> Vec<Clump> {
    observed_clumps(
        (0..n)
            .map(|id| Contact {
                id: id as i32,
                tier: Tier::Vehicle,
                position: Vec3::xz(1000.0, 1000.0),
                range: 300.0,
                speed: 100.0,
                value: 100.0,
                seen: 0,
            })
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_sapper(unit: Unit) -> Sapper {
        Sapper {
            unit,
            radius: 176.0,
            imp: true,
            snitch: false,
            site: None,
            placed: false,
            held: false,
            charging: None,
            logged: -1000,
            next_judge: 0,
            deployment: Deployment::new(0, Vec3::ZERO),
        }
    }

    #[test]
    fn pack_value_relative_to_mine_cost_controls_demand() {
        let mut three = test_clumps(3);
        three[0].members.values_mut().for_each(|v| *v = 130.0);
        three[0].value = 390.0;
        assert_eq!(mine_quota(&three, 170.0), 1, "three Scorchers pay for a Snitch");
        assert_eq!(mine_quota(&three, 125.0), 1, "and an Imp");
        assert_eq!(mine_quota(&three, 200.0), 0, "use the actual producer cost");
        let mut cheap = three.clone();
        cheap[0].value = 120.0;
        assert_eq!(mine_quota(&cheap, 170.0), 0, "three cheap scouts do not pay");
        assert_eq!(mine_quota(&test_clumps(1), 25.0), 0, "a lone unit is not a pack");
        assert_eq!(mine_quota(&test_clumps(10), 170.0), 2);
        assert_eq!(mine_quota(&test_clumps(100), 170.0), RESERVE_MINES);
        let mut two = three.clone();
        let mut other = three.remove(0);
        other.members = (10..13).map(|id| (id, 130.0)).collect();
        other.position.x += 2000.0;
        two.push(other);
        assert_eq!(mine_quota(&two, 170.0), 2);
        assert_eq!(mine_quota(&two, 0.0), 0);
    }

    #[test]
    fn a_dead_expensive_member_does_not_leave_phantom_pack_value() {
        let mut memory = test_clumps(3);
        memory[0].members = [(0, 1000.0), (1, 40.0), (2, 40.0)].into_iter().collect();
        memory[0].value = 1080.0;
        assert_eq!(mine_quota(&memory, 170.0), 1);
        remember_clumps(&mut memory, vec![], &[0].into_iter().collect(), 100);
        assert_eq!(memory[0].value, 80.0);
        assert_eq!(mine_quota(&memory, 170.0), 0);
    }

    #[test]
    fn repeated_sightings_merges_deaths_and_fog_do_not_inflate_demand() {
        let mut memory = test_clumps(10);
        remember_clumps(&mut memory, test_clumps(10), &BTreeSet::new(), 100);
        assert_eq!(mine_quota(&memory, 250.0), 2);
        remember_clumps(&mut memory, vec![], &[0, 1, 2, 3, 4].into_iter().collect(), 200);
        assert_eq!(mine_quota(&memory, 250.0), 1);
        remember_clumps(&mut memory, vec![], &BTreeSet::new(), CLUMP_KEEP);
        assert_eq!(mine_quota(&memory, 250.0), 1);
        remember_clumps(&mut memory, vec![], &BTreeSet::new(), CLUMP_KEEP + 1);
        assert_eq!(mine_quota(&memory, 250.0), 0);
    }

    #[test]
    fn sparse_chain_or_mixed_mobility_is_not_a_squad() {
        let contacts = |step| {
            (0..5)
                .map(|id| Contact {
                    id,
                    tier: Tier::Vehicle,
                    position: Vec3::xz(1000.0 + id as f32 * step, 1000.0),
                    range: 300.0,
                    speed: 100.0,
                    value: 100.0,
                    seen: 0,
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(mine_quota(&observed_clumps(contacts(250.0)), 170.0), 0);
        assert_eq!(mine_quota(&observed_clumps(contacts(100.0)), 170.0), 1);
        let mut mixed = contacts(0.0);
        mixed.truncate(3);
        mixed[0].tier = Tier::Spider;
        assert_eq!(mine_quota(&observed_clumps(mixed), 125.0), 0);
    }

    #[test]
    fn standoff_follows_a_bent_attacker_route_and_cannot_project_past_the_enemy() {
        let route = [Vec3::xz(100.0, 100.0), Vec3::xz(100.0, 300.0), Vec3::xz(700.0, 300.0)];
        let p = route_standoff(&route, 500.0).unwrap();
        assert_eq!((p.x, p.z), (400.0, 300.0));
        assert!(route_standoff(&route, 900.0).is_none());
    }

    #[test]
    fn charges_spare_buildings_along_the_entire_run_not_only_at_its_end() {
        let bait = Building { center: Vec3::xz(1500.0, 1000.0), footprint: 40.0 };
        assert!(!charge_clear(Vec3::xz(1000.0, 1000.0), Vec3::xz(2000.0, 1000.0), 176.0, &[bait]));
        assert!(charge_clear(Vec3::xz(1000.0, 1400.0), Vec3::xz(2000.0, 1400.0), 176.0, &[bait]));
    }

    #[test]
    fn deployment_has_bounded_exit_and_detects_a_nonempty_stalled_queue() {
        let p = Vec3::xz(1000.0, 1000.0);
        let mut d = Deployment::new(100, p);
        assert!(d.exiting(101, 1));
        assert!(!d.exiting(101, 0));
        assert!(!d.exiting(100 + EXIT_GRACE, 1));
        assert!(!d.stalled(p, 100 + STALL - 1, true));
        assert!(d.stalled(p, 100 + STALL, true));
        d.block(p, 500);
        assert!(!d.allows(p, 501));
        assert!(d.allows(p, 500 + BLOCKED_FOR));
        assert!(!d.stalled(Vec3::xz(1200.0, 1000.0), 900, true));
    }

    #[test]
    fn live_commitments_survive_resampling_but_dead_assets_do_not() {
        let old = Site {
            position: Vec3::xz(1000.0, 1000.0),
            gate: Vec3::xz(500.0, 1000.0),
            value: 10.0,
            depth: 0,
            ambush: true,
        };
        let drift = Site { position: Vec3::xz(1100.0, 1000.0), value: 12.0, ..old };
        let held = refresh_site(old, &[drift], 288.0, false, true).unwrap();
        assert_eq!((held.position.x, held.value), (1000.0, 12.0));
        assert_eq!(refresh_site(old, &[], 288.0, true, true).unwrap().value, 10.0);
        assert_eq!(refresh_site(old, &[], 288.0, false, true).unwrap().value, 0.25);
        assert!(refresh_site(old, &[], 288.0, true, false).is_none());
    }

    #[test]
    fn expansion_replaces_stale_jobs_but_small_improvements_do_not_shuffle_mines() {
        let ai = super::super::tests::reserve_ai();
        let unit = Unit::from_id(ai.cb, 11).unwrap();
        let mut sapper = test_sapper(unit);
        sapper.radius = 100.0;
        let a = Site {
            position: Vec3::xz(1000.0, 1000.0),
            gate: Vec3::xz(500.0, 1000.0),
            value: 10.0,
            depth: 0,
            ambush: true,
        };
        let b = Site { position: Vec3::xz(2000.0, 1000.0), gate: Vec3::xz(1500.0, 1000.0), value: 40.0, ..a };
        sapper.site = Some(a);
        let drift = Site { position: Vec3::xz(a.position.x + 8.0, a.position.z), value: 11.0, ..a };
        assert_eq!(
            select_assignment(&sapper, a.position, &[drift, a], &[], COMMIT + 1).unwrap().position.x,
            a.position.x,
            "a small route shift must not walk a cloaked mine away from its exact site"
        );
        assert_eq!(select_assignment(&sapper, a.position, &[a, b], &[], 100).unwrap().position.x, a.position.x);
        assert_eq!(select_assignment(&sapper, a.position, &[a, b], &[], COMMIT + 1).unwrap().position.x, b.position.x);
        assert_eq!(select_assignment(&sapper, a.position, &[b], &[], 100).unwrap().position.x, b.position.x);
        assert!(select_assignment(&sapper, a.position, &[], &[], 100).is_none());
    }
}
