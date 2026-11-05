//! Silo placement: bounded engine queries, then pure background scoring.
//! Only one search is outstanding. Cached reachability belongs to that
//! search's exact terrain; a result never supplies unchecked new trajectories.
use std::collections::{BTreeSet, HashMap, VecDeque};

use super::*;
use crate::kgbutil::worker::Worker;

const SITES_PER_FRAME: usize = 2;
const MAX_AGE: i32 = 30 * 30;
const RETRY: i32 = 30 * 60;

#[derive(Default)]
pub(crate) struct Planner {
    preparing: Option<Preparation>,
    worker: Option<Worker<Job, Result>>,
    retry_after: i32,
    last_step: Option<i32>,
    discard: bool,
}

impl Planner {
    pub(crate) fn active(&self) -> bool {
        self.preparing.is_some() || self.worker.as_ref().is_some_and(Worker::busy)
    }

    pub(crate) fn cancel(&mut self) {
        self.preparing = None;
        // Do not join an in-flight calculation on the simulation thread.
        // Poll and discard its output; an idle worker is reusable.
        if let Some(worker) = &mut self.worker {
            worker.poll();
            self.discard = worker.busy();
        }
    }

    fn failed(&mut self, frame: i32) {
        self.preparing = None;
        self.retry_after = frame + RETRY;
    }
}

struct Preparation {
    frame: i32,
    seeds: VecDeque<Vec3>,
    sites: Vec<Vec3>,
    seen: BTreeSet<[u32; 2]>,
}

struct Demand {
    allies: Vec<Vec3>,
    targets: Vec<(Vec3, f32)>,
    statics: Vec<(Vec3, f32)>,
    territory: Vec<Vec3>,
    stun: Vec<Vec3>,
    siege: SiegeReadiness,
    enemy: Vec3,
}

impl Demand {
    fn key(&self, at: Vec3) -> [u32; 3] {
        [at.x.to_bits(), at.z.to_bits(), u32::from(self.stun.iter().any(|&p| distance(p, at) < 1.0))]
    }

    fn score(&self, site: Vec3, reaches: impl FnMut(Vec3, Vec3) -> bool) -> Option<SiloSite> {
        choose_silo_site_checked(
            &self.allies,
            std::iter::once(site),
            &self.targets,
            &self.statics,
            &self.territory,
            self.siege,
            self.enemy,
            reaches,
        )
    }
}

struct Ground {
    corners: Vec<f32>,
    w: usize,
    h: usize,
}

impl Ground {
    fn valid(&self) -> bool {
        self.w > 0 && self.h > 0 && self.corners.len() == (self.w + 1) * (self.h + 1)
    }

    /// Recoil's InterpolateCornerHeight: two triangles, not bilinear.
    fn at(&self, x: f32, z: f32) -> f32 {
        let x = x.clamp(0.0, (self.w * 8 - 1) as f32) / 8.0;
        let z = z.clamp(0.0, (self.h * 8 - 1) as f32) / 8.0;
        let (ix, iz) = (x as usize, z as usize);
        let (dx, dz) = (x - ix as f32, z - iz as f32);
        let i = ix + iz * (self.w + 1);
        let (h00, h10, h01, h11) =
            (self.corners[i], self.corners[i + 1], self.corners[i + self.w + 1], self.corners[i + self.w + 2]);
        if dx + dz < 1.0 {
            h00 + dx * (h10 - h00) + dz * (h01 - h00)
        } else {
            h11 + (1.0 - dx) * (h01 - h11) + (1.0 - dz) * (h10 - h11)
        }
    }
}

struct Job {
    frame: i32,
    sites: Vec<Vec3>,
    demand: Demand,
    ground: Ground,
    flights: [Option<flight::Flight>; 2],
}

struct Result {
    frame: i32,
    ground: Ground,
    choice: Option<(SiloSite, HashMap<[u32; 3], bool>)>,
    predictions: usize,
    hits: usize,
}

impl Result {
    fn revalidate(&self, frame: i32, corners: &[f32], demand: &Demand) -> Option<SiloSite> {
        let (choice, cache) = self.choice.as_ref()?;
        if frame < self.frame || frame - self.frame > MAX_AGE || corners != self.ground.corners {
            return None;
        }
        demand.score(choice.pos, |_, at| cache.get(&demand.key(at)).copied().unwrap_or(false))
    }
}

fn compute(job: Job) -> Result {
    let (mut predictions, mut hits) = (0, 0);
    let mut score = |site| {
        // Cache per site: targets and territory can ask the same trajectory.
        // Memory is bounded by one site's targets, not sites × territory.
        let mut cache = HashMap::new();
        let choice = job.demand.score(site, |from, mut at| {
            let key = job.demand.key(at);
            if let Some(&ok) = cache.get(&key) {
                hits += 1;
                return ok;
            }
            predictions += 1;
            at.y = job.ground.at(at.x, at.z);
            let ok = job.flights[key[2] as usize]
                .and_then(|f| f.predict(from, at, |x, z| job.ground.at(x, z)))
                .is_some_and(|p| p.blocked.is_none());
            cache.insert(key, ok);
            ok
        });
        choice.map(|s| (s, cache))
    };
    let scored: Vec<_> = job.sites.iter().filter_map(|&p| score(p).map(|(s, _)| s)).collect();
    let best = scored.iter().map(|s| s.payoff.max(SILO_COST)).fold(0.0, f32::max);
    let winner = scored
        .into_iter()
        .filter(|s| s.payoff.max(SILO_COST) >= 0.8 * best)
        .max_by(|a, b| distance(a.pos, job.demand.enemy).total_cmp(&distance(b.pos, job.demand.enemy)));
    // Recompute just the winner to retain its cache for current-state scoring.
    let choice = winner.and_then(|s| score(s.pos));
    Result { frame: job.frame, ground: job.ground, choice, predictions, hits }
}

impl Ai {
    fn missiles_site_demand(&self) -> Demand {
        let eos = self.missiles_eos_targets();
        let stun = self.missiles_stun_targets();
        let mut targets: Vec<_> = eos.iter().map(|t| (t.pos, t.value - MISSILE_COST * t.needed as f32)).collect();
        targets.extend(stun.iter().filter(|a| !eos.iter().any(|t| t.id == a.0)).map(|a| (a.1, a.2 - MISSILE_COST)));
        Demand {
            allies: self.missiles_allied_silos(None),
            targets,
            statics: self.missiles_static_value(),
            territory: self.graph.enemy_territory_cells().collect(),
            stun: stun.iter().map(|a| a.1).collect(),
            siege: self.missiles_siege_readiness(),
            enemy: self.graph.get_enemy_center(),
        }
    }

    fn missiles_site_safe(&self, silo: UnitDef, p: Vec3) -> bool {
        p.x >= 0.0
            && !self.missiles_shuns_site(p)
            && self.cb.map().is_possible_to_build_at(silo, p, 0)
            && self.eco_build_check(silo, p, 0)
            && self.graph.is_ally_territory(p)
            && (self.war.get_threat(p) <= 0.0
                || (self.war.get_effective_threat(p) <= 0.0 && self.war.get_friendly_porc_threat(p) >= HELD_POWER))
    }

    /// Called by the economy at requisition time, and each frame while pending.
    /// None means pending or cooling down; only a completed miss is reported.
    pub fn missiles_site(&mut self) -> Option<SiloSite> {
        if self.missiles.site.last_step == Some(self.frame) {
            return None;
        }
        self.missiles.site.last_step = Some(self.frame);
        let silo = self.defs.get(SILO)?;
        if let Some((result, elapsed)) = self.missiles.site.worker.as_mut().and_then(Worker::poll) {
            let result = if std::mem::take(&mut self.missiles.site.discard) { None } else { result };
            let choice = result.and_then(|result| {
                self.log(&format!(
                    "missiles: site worker {:.1}ms, {} predictions, {} cache hits, age {} frames",
                    elapsed.as_secs_f64() * 1000.0,
                    result.predictions,
                    result.hits,
                    self.frame - result.frame
                ));
                let (choice, _) = result.choice.as_ref()?;
                if self.frame - result.frame > MAX_AGE
                    || !self.missiles_site_safe(silo, choice.pos)
                    // GetBuildHeight can differ from the centre's terrain
                    // elevation. Preserve the engine's exact launch height,
                    // and reject any shift since candidate collection.
                    || self.cb.map().find_closest_build_site(silo, choice.pos, 0.0, 3, 0) != choice.pos
                {
                    return None;
                }
                // Current values, ownership and allied coverage; never run
                // the exhaustive flight simulation on the engine thread.
                let demand = self.missiles_site_demand();
                result.revalidate(self.frame, &self.cb.map().corner_height_map(), &demand)
            });
            self.missiles.site.failed(self.frame);
            if choice.is_none() {
                self.missiles_report_no_site();
            }
            return choice;
        }
        if self.missiles.site.worker.as_ref().is_some_and(Worker::busy) || self.frame < self.missiles.site.retry_after {
            return None;
        }
        if self.missiles.site.preparing.is_none() {
            let enemy = self.graph.get_enemy_center();
            let mut seeds: Vec<_> = self.graph.get_owned_spots().iter().map(|m| m.get_pos()).collect();
            seeds.push(self.graph.get_ally_center());
            let ahead: Vec<_> = seeds
                .iter()
                .flat_map(|&p| SITE_STEPS.iter().map(move |&d| get_directional_point(p, enemy, d)))
                .collect();
            seeds.extend(ahead);
            self.missiles.site.preparing =
                Some(Preparation { frame: self.frame, seeds: seeds.into(), sites: Vec::new(), seen: BTreeSet::new() });
        }
        let mut prep = self.missiles.site.preparing.take()?;
        if self.frame - prep.frame > MAX_AGE {
            self.missiles.site.failed(self.frame);
            return None;
        }
        for _ in 0..SITES_PER_FRAME {
            let Some(seed) = prep.seeds.pop_front() else { break };
            let p = self.cb.map().find_closest_build_site(silo, seed, 600.0, 3, 0);
            if self.missiles_site_safe(silo, p) && prep.seen.insert([p.x.to_bits(), p.z.to_bits()]) {
                prep.sites.push(p);
            }
        }
        if !prep.seeds.is_empty() {
            self.missiles.site.preparing = Some(prep);
            return None;
        }
        let map = self.cb.map();
        let ground = Ground {
            corners: map.corner_height_map(),
            w: map.width().max(0) as usize,
            h: map.height().max(0) as usize,
        };
        if prep.sites.is_empty() || !ground.valid() {
            self.missiles.site.failed(self.frame);
            self.missiles_report_no_site();
            return None;
        }
        let job = Job {
            frame: self.frame,
            sites: prep.sites,
            demand: self.missiles_site_demand(),
            ground,
            flights: [flight::Flight::for_kind(self, EOS), flight::Flight::for_kind(self, SHOCKLEY)],
        };
        self.missiles.site.discard = false;
        self.missiles.site.worker.get_or_insert_with(|| Worker::new("missile-sites", compute)).submit(job);
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    fn fixture() -> Job {
        let flight = flight::Flight {
            range: 3450.0,
            up_frames: 88,
            accel: 0.2,
            turn: 18000.0 * std::f32::consts::TAU / 65536.0 / 30.0,
            start: 0.2,
            end: 40.0,
            height: -42.0,
        };
        Job {
            frame: 600,
            sites: vec![Vec3::xz(80.0, 80.0), Vec3::xz(160.0, 80.0)],
            ground: Ground { corners: vec![0.0; 513 * 513], w: 512, h: 512 },
            flights: [Some(flight), Some(flight)],
            demand: Demand {
                allies: vec![],
                targets: vec![(Vec3::xz(3000.0, 80.0), 2000.0)],
                statics: vec![(Vec3::xz(3000.0, 80.0), 2600.0)],
                territory: vec![Vec3::xz(3000.0, 80.0)],
                stun: vec![],
                siege: SiegeReadiness { density: 2000.0, threshold: 1500.0, laneness: 0.0 },
                enemy: Vec3::xz(4000.0, 80.0),
            },
        }
    }

    #[test]
    fn corner_snapshot_matches_engine_triangles_and_edge_clamping() {
        let g = Ground { corners: vec![0.0, 8.0, 16.0, 40.0], w: 1, h: 1 };
        assert!(g.valid());
        assert_eq!(g.at(2.0, 2.0), 6.0);
        assert_eq!(g.at(6.0, 6.0), 26.0);
        assert_eq!(g.at(-10.0, -10.0), 0.0);
        assert_eq!(g.at(100.0, 100.0), g.at(7.0, 7.0));
        assert!(!Ground { corners: vec![], w: 1, h: 1 }.valid());
    }

    #[test]
    fn worker_preserves_exhaustive_choice_and_deduplicates_trajectories() {
        let mut job = fixture();
        // The engine's footprint-derived build height need not equal the
        // centre terrain height. The worker must retain the supplied origin.
        job.sites[0].y = 30.0;
        let expected = choose_silo_site_checked(
            &job.demand.allies,
            job.sites.iter().copied(),
            &job.demand.targets,
            &job.demand.statics,
            &job.demand.territory,
            job.demand.siege,
            job.demand.enemy,
            |from, mut to| {
                to.y = job.ground.at(to.x, to.z);
                job.flights[0]
                    .unwrap()
                    .predict(from, to, |x, z| job.ground.at(x, z))
                    .is_some_and(|p| p.blocked.is_none())
            },
        )
        .unwrap();
        let result = compute(job);
        let (actual, cache) = result.choice.unwrap();
        assert_eq!(actual.pos, expected.pos);
        assert_eq!(actual.payoff, expected.payoff);
        assert_eq!(actual.coverage.share, expected.coverage.share);
        assert_eq!(actual.territory_share, expected.territory_share);
        assert_eq!(cache.len(), 1);
        assert_eq!(result.predictions, 3); // Two sites and winner validation cache.
        assert_eq!(result.hits, 6);
    }

    #[test]
    fn result_requires_fresh_terrain_and_current_unclaimed_demand() {
        let result = compute(fixture());
        let mut current = fixture();
        assert!(result.revalidate(601, &current.ground.corners, &current.demand).is_some());
        assert!(result.revalidate(599, &current.ground.corners, &current.demand).is_none());
        assert!(result.revalidate(600 + MAX_AGE + 1, &current.ground.corners, &current.demand).is_none());
        current.ground.corners[0] = 1.0;
        assert!(result.revalidate(601, &current.ground.corners, &current.demand).is_none());
        current.ground.corners[0] = 0.0;
        current.demand.allies.push(Vec3::xz(2000.0, 80.0));
        assert!(result.revalidate(601, &current.ground.corners, &current.demand).is_none());
        current.demand.allies.clear();
        current.demand.targets.clear();
        current.demand.statics.clear();
        current.demand.territory.clear();
        assert!(result.revalidate(601, &current.ground.corners, &current.demand).is_none());
        // Neither a new position nor another missile kind can reuse a flight.
        current.demand.targets.push((Vec3::xz(3008.0, 80.0), 10000.0));
        assert!(result.revalidate(601, &current.ground.corners, &current.demand).is_none());
        current.demand.targets[0].0.x = 3000.0;
        current.demand.stun.push(current.demand.targets[0].0);
        assert!(result.revalidate(601, &current.ground.corners, &current.demand).is_none());
    }

    #[test]
    fn terrain_ridge_can_reject_a_profitable_site() {
        let mut job = fixture();
        for z in 0..=job.ground.h {
            for x in 320..=335 {
                job.ground.corners[z * (job.ground.w + 1) + x] = 2000.0;
            }
        }
        assert!(compute(job).choice.is_none());
    }

    #[test]
    fn failed_search_backs_off_and_cancelled_worker_is_drained() {
        let mut planner = Planner::default();
        planner.failed(600);
        assert_eq!(planner.retry_after, 600 + RETRY);
        assert!(!planner.active());
        planner.worker = Some(Worker::new("silo-site-test", compute));
        planner.worker.as_mut().unwrap().submit(fixture());
        planner.cancel();
        let start = Instant::now();
        while planner.active() {
            planner.cancel();
            assert!(start.elapsed() < Duration::from_secs(5));
            std::thread::yield_now();
        }
        assert!(planner.worker.as_mut().unwrap().poll().is_none());
    }

    #[test]
    #[ignore = "diagnostic late-game workload; run explicitly with --nocapture"]
    fn late_game_worker_and_validation_cost() {
        let mut job = fixture();
        job.sites = (0..88).map(|i| Vec3::xz(80.0 + (i % 11) as f32 * 64.0, 80.0 + (i / 11) as f32 * 64.0)).collect();
        job.demand.territory =
            (0..8200).map(|i| Vec3::xz(1500.0 + (i % 100) as f32 * 24.0, 200.0 + (i / 100) as f32 * 24.0)).collect();
        let mut current = fixture();
        current.demand.territory = job.demand.territory.clone();
        let start = Instant::now();
        let result = compute(job);
        let worker_ms = start.elapsed().as_secs_f64() * 1000.0;
        let start = Instant::now();
        assert!(result.revalidate(601, &current.ground.corners, &current.demand).is_some());
        eprintln!(
            "silo: 88 sites × 8200 cells; worker {worker_ms:.2}ms; cached validation {:.3}ms; predictions {}; cache hits {}",
            start.elapsed().as_secs_f64() * 1000.0,
            result.predictions,
            result.hits
        );
    }
}
