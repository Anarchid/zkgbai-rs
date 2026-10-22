//! Development aid: a staged game of raid defense, set up with cheats by
//! the AI option `test`. Cheats are on only while spawning: the AIs see
//! what radar and sight show them, nothing more.
//!
//! With `test=qrf` (the defender), the commander goes (self-destructs) and
//! a factory in the corner behind the start keeps the team alive; then a
//! mex stands on every metal spot of our half of the map, a solar by every
//! other one, radars cover our half and the approaches to it, and a fixed
//! force (AI option `qrf_force`, default `tankraid:6`, as `name:count`
//! joined by `,`) stands at the start. No one builds anything, so nothing
//! lost comes back (but with `qrf_air=gunship`, `plane` or `both`, air
//! factories by the start build the firefighters wanted, see
//! `firefighters`). The force is the QRF's (see `qrf`), or with
//! `disable=qrf` the usual handlers'. `qrf_radar`: `on` (default), `off`
//! (sight only) or `full` (radars over the whole map). Every 30 s the log
//! has a `qrfscore:` line: mexes standing, buildings and metal lost, the
//! force left, raiders killed, metal income and its sum so far.
//!
//! With `test=raidgen` (the attacker), the commander goes as well, and
//! raids come: `raid_groups` (default 2) groups at a time, each of a
//! composition picked at random from `raid_comp` (default `cloakraid:3`;
//! compositions as for `qrf_force`, alternatives joined by `/`), at a place
//! picked at random on the attacker's half at least `raid_dist` (default
//! 2500) elmos from every metal spot of the defender's half (so radar sees
//! them coming), the first after `raid_first` seconds (default 30), each
//! next one `raid_respawn` seconds (default 30) after its group is gone;
//! a raid lasts `raid_life` seconds at most (default 180), then what is
//! left of it self-destructs.
//! Each group is a raider squad of the usual raider handler. `raidscore:`
//! lines tell the groups spawned and the raiders lost.
//!
//! A fair game: the raids are always interceptable in principle (they show
//! up on radar well away from the value), each group is weaker than a team
//! of the default force, and there are no more groups at a time than
//! teams.

use std::collections::{BTreeMap, BTreeSet};

use springai::Vec3;

use crate::Ai;
use crate::military::Fighter;
use crate::military::qrf::field::{Grid, Mobility};
use crate::util::{NULLPOS, distance, get_directional_point};

/// When the commander goes, and when the stage is set.
const CLEAR_FRAME: i32 = 30;
const STAGE_FRAME: i32 = 30 * 10;

/// Frames between score lines.
const SCORE_PERIOD: i32 = 30 * 30;

/// Spawn places are sampled this far apart.
const SAMPLE: f32 = 256.0;

/// Radars see this far (staticradar: 2100), less a margin.
const RADAR_REACH: f32 = 1900.0;

/// Our radars cover what is within this far of our mexes.
const APPROACH: f32 = 1500.0;

#[derive(Default)]
pub struct RaidTest {
    /// Where we start, and the enemy.
    base: Vec3,
    enemy: Vec3,
    /// Our buildings the score counts: cost, mex.
    assets: BTreeMap<i32, (f32, bool)>,
    mexes: usize,
    assets_metal: f32,
    lost: usize,
    lost_mexes: usize,
    lost_metal: f32,
    /// Our force: cost.
    force: BTreeMap<i32, f32>,
    force_initial: usize,
    force_lost: usize,
    force_lost_metal: f32,
    kills: usize,
    kills_metal: f32,
    income_sum: f32,
    /// The attacker's groups: unit ids, per slot.
    groups: Vec<BTreeSet<i32>>,
    /// When each slot's group came.
    born: Vec<i32>,
    next_spawn: Vec<i32>,
    spawned: usize,
    spawn_places: Vec<Vec3>,
    /// The attacker's raiders: cost.
    raiders: BTreeMap<i32, f32>,
    raiders_lost: usize,
    raiders_lost_metal: f32,
    /// Raiders whose raid lasted too long.
    expired: usize,
    /// The raids' own random numbers (splitmix64): both AIs of a game draw
    /// from the shared one, so only this one repeats with `raid_seed`.
    rng: u64,
}

impl RaidTest {
    fn rand(&mut self) -> f64 {
        self.rng = self.rng.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.rng;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^= z >> 31;
        (z >> 11) as f64 / (1u64 << 53) as f64
    }

    fn pick<T: Copy>(&mut self, v: &[T]) -> Option<T> {
        let r = self.rand();
        v.get(((r * v.len() as f64) as usize).min(v.len().max(1) - 1)).copied()
    }
}

/// `name:count,name:count`.
fn parse_comp(s: &str) -> Vec<(String, usize)> {
    (s.split(','))
        .filter_map(|p| {
            let mut it = p.trim().split(':');
            let name = it.next()?.trim().to_owned();
            let n = it.next().and_then(|n| n.trim().parse().ok()).unwrap_or(1);
            (!name.is_empty()).then_some((name, n))
        })
        .collect()
}

impl Ai {
    fn raid_test_opt<T: std::str::FromStr>(&self, key: &str, default: T) -> T {
        self.cb.option(key).and_then(|v| v.parse().ok()).unwrap_or(default)
    }

    /// Whether this is the staged raid game, on either side.
    pub fn raid_testing(&self) -> bool {
        matches!(self.test.as_deref(), Some("qrf" | "raidgen"))
    }

    /// Raiders spawned as squads are the raider handler's already.
    pub fn raid_test_claims(&self, id: i32) -> bool {
        self.raid_test.raiders.contains_key(&id)
    }

    pub fn raid_test_update(&mut self, frame: i32) {
        let Some(test) = self.test.clone() else { return };
        if !self.raid_testing() {
            return;
        }
        if frame == CLEAR_FRAME {
            self.raid_test_clear();
        }
        if test == "qrf" {
            if frame == STAGE_FRAME {
                self.raid_test_stage_defense();
            }
            if frame > STAGE_FRAME && frame % SCORE_PERIOD == 0 {
                self.raid_test_score(frame);
            }
            // Idle factories build what the firefighters want, if anything.
            if frame > STAGE_FRAME && frame % 60 == 0 {
                let idle: Vec<i32> = (self.fac.factories.values())
                    .filter(|f| f.unit.current_commands().is_empty())
                    .map(|f| f.id)
                    .collect();
                for id in idle {
                    self.fac_build_firefighter(id);
                }
            }
        } else {
            if frame == STAGE_FRAME {
                self.raid_test_stage_raids();
            }
            if frame > STAGE_FRAME && frame % 30 == 0 {
                self.raid_test_spawn_raids(frame);
            }
            if frame > STAGE_FRAME && frame % SCORE_PERIOD == 0 {
                let t = &self.raid_test;
                let alive: usize = t.groups.iter().map(|g| g.len()).sum();
                let line = format!(
                    "raidscore: t={} spawned={} alive={} raiders_lost={} raiders_lost_metal={:.0} expired={}",
                    frame / 30,
                    t.spawned,
                    alive,
                    t.raiders_lost,
                    t.raiders_lost_metal,
                    t.expired
                );
                self.log(&line);
            }
        }
    }

    /// A factory behind the start keeps the team alive; the commander goes.
    fn raid_test_clear(&mut self) {
        let coms: Vec<_> =
            self.cb.team_units().into_iter().filter(|u| self.defs.name_of(*u).starts_with("dyn")).collect();
        let base = coms.first().map(|c| c.pos()).unwrap_or_else(|| self.graph.get_ally_center());
        let map = self.cb.map();
        let (w, h) = (map.width() as f32 * 8.0, map.height() as f32 * 8.0);
        let enemy = match self.graph.get_enemy_center() {
            e if e != NULLPOS && distance(e, base) > 1000.0 => e,
            _ => Vec3::xz(w - base.x, h - base.z),
        };
        self.raid_test.base = base;
        self.raid_test.enemy = enemy;
        let corner = Vec3::xz(
            if base.x < w / 2.0 { 150.0 } else { w - 150.0 },
            if base.z < h / 2.0 { 150.0 } else { h - 150.0 },
        );
        self.cb.cheats_set_enabled(true);
        if let Some(def) = self.defs.get("factorycloak") {
            let site = map.find_closest_build_site(def, corner, 1500.0, 4, 0);
            self.cb.cheat_new_unit(def, if site.x >= 0.0 { site } else { corner });
        }
        self.cb.cheats_set_enabled(false);
        for c in coms {
            c.self_destruct(0);
        }
        self.log(&format!("raidtest: base {:.0},{:.0} enemy {:.0},{:.0}", base.x, base.z, enemy.x, enemy.z));
    }

    /// Whether a place is on our side of the map.
    fn raid_test_ours(&self, p: Vec3) -> bool {
        distance(p, self.raid_test.base) < distance(p, self.raid_test.enemy)
    }

    fn raid_test_spawn(&mut self, name: &str, at: Vec3, building: bool) -> Option<springai::Unit> {
        let def = self.defs.get(name)?;
        let site = if building {
            self.cb.map().find_closest_build_site(def, at, 400.0, 2, 0)
        } else {
            self.cb.map().find_closest_build_site(def, at, 600.0, 4, 0)
        };
        let u = self.cb.cheat_new_unit(def, if site.x >= 0.0 { site } else { at })?;
        Some(u)
    }

    fn raid_test_stage_defense(&mut self) {
        let spots: Vec<Vec3> =
            (self.graph.metal_spots.iter().map(|m| m.position)).filter(|&p| self.raid_test_ours(p)).collect();
        let map = self.cb.map();
        let (w, h) = (map.width() as f32 * 8.0, map.height() as f32 * 8.0);
        // Radars: greedily, the place that covers most of what is left.
        let radar = self.cb.option("qrf_radar").unwrap_or_else(|| "on".into());
        let mut demand = Vec::new();
        let (mut x, mut z) = (SAMPLE / 2.0, SAMPLE / 2.0);
        while z < h {
            while x < w {
                let p = Vec3::xz(x, z);
                if radar == "full" || spots.iter().any(|&s| distance(s, p) < APPROACH) {
                    demand.push(p);
                }
                x += SAMPLE;
            }
            x = SAMPLE / 2.0;
            z += SAMPLE;
        }
        let mut radars = Vec::new();
        if radar != "off" {
            let candidates: Vec<Vec3> =
                demand.iter().copied().filter(|&p| radar == "full" || self.raid_test_ours(p)).collect();
            let mut left = demand.clone();
            while !left.is_empty() && radars.len() < 12 {
                let best = candidates
                    .iter()
                    .copied()
                    .max_by_key(|&c| left.iter().filter(|&&d| distance(c, d) < RADAR_REACH).count());
                let Some(best) = best else { break };
                let n = left.len();
                left.retain(|&d| distance(best, d) >= RADAR_REACH);
                if left.len() == n {
                    break;
                }
                radars.push(best);
                if (left.len() as f32) < 0.03 * demand.len() as f32 {
                    break;
                }
            }
        }
        let force = parse_comp(&self.cb.option("qrf_force").unwrap_or_else(|| "tankraid:6".into()));
        let base = self.raid_test.base;
        self.cb.cheats_set_enabled(true);
        let mut built = Vec::new();
        for (k, &s) in spots.iter().enumerate() {
            built.extend(self.raid_test_spawn("staticmex", s, true));
            if k % 2 == 0 {
                let side = Vec3::xz(s.x + 100.0, s.z + 60.0);
                built.extend(self.raid_test_spawn("energysolar", side, true));
            }
        }
        for &r in &radars {
            built.extend(self.raid_test_spawn("staticradar", r, true));
        }
        // Air factories for firefighters (see `firefighters`).
        let air = self.cb.option("qrf_air").unwrap_or_default();
        for (key, fac, d) in [("gunship", "factorygunship", 350.0), ("plane", "factoryplane", -350.0)] {
            if air == key || air == "both" {
                let at = get_directional_point(base, self.raid_test.enemy, d);
                built.extend(self.raid_test_spawn(fac, at, true));
            }
        }
        let mut units = Vec::new();
        let mut k = 0;
        for (name, n) in &force {
            for _ in 0..*n {
                let at = Vec3::xz(base.x + 40.0 * (k % 4) as f32, base.z + 40.0 * (k / 4) as f32);
                units.extend(self.raid_test_spawn(name, at, false));
                k += 1;
            }
        }
        self.cb.cheats_set_enabled(false);
        let metal = self.metal;
        for u in &built {
            let Some(d) = u.def() else { continue };
            let mex = self.defs.name(d) == "staticmex";
            self.raid_test.assets.insert(u.id(), (d.cost(metal), mex));
            self.raid_test.mexes += usize::from(mex);
            self.raid_test.assets_metal += d.cost(metal);
        }
        self.qrf_start();
        for &u in &units {
            let cost = u.def().map_or(0.0, |d| d.cost(metal));
            self.raid_test.force.insert(u.id(), cost);
            if self.qrf_enabled() {
                self.qrf_add(u);
            } else {
                // A short walk, so it goes idle and joins its handler.
                let p = u.pos();
                u.move_to(Vec3::new(p.x + 40.0, p.y, p.z + 40.0), 0);
            }
        }
        self.raid_test.force_initial = units.len();
        let line = format!(
            "raidtest: staged {} mexes, {} buildings ({:.0} metal), {} radars ({radar}), force {} ({})",
            self.raid_test.mexes,
            self.raid_test.assets.len(),
            self.raid_test.assets_metal,
            radars.len(),
            units.len(),
            force.iter().map(|(n, c)| format!("{n}:{c}")).collect::<Vec<_>>().join(","),
        );
        self.log(&line);
    }

    fn raid_test_score(&mut self, frame: i32) {
        let income = self.cb.economy().income(self.metal);
        let t = &mut self.raid_test;
        t.income_sum += income * (SCORE_PERIOD as f32 / 30.0);
        let force_metal: f32 = t.force.values().sum();
        let line = format!(
            "qrfscore: t={} mexes={}/{} lost={} lost_mexes={} lost_metal={:.0}/{:.0} force={}/{} force_metal={:.0} force_lost_metal={:.0} kills={} kills_metal={:.0} income={:.1} income_sum={:.0}",
            frame / 30,
            t.mexes - t.lost_mexes,
            t.mexes,
            t.lost,
            t.lost_mexes,
            t.lost_metal,
            t.assets_metal,
            t.force.len(),
            t.force_initial,
            force_metal,
            t.force_lost_metal,
            t.kills,
            t.kills_metal,
            income,
            t.income_sum,
        );
        self.log(&line);
    }

    /// Where raids may come from: our half, far enough from the defender's
    /// metal.
    fn raid_test_stage_raids(&mut self) {
        let min = self.raid_test_opt("raid_dist", 2500.0f32);
        let theirs: Vec<Vec3> =
            (self.graph.metal_spots.iter().map(|m| m.position)).filter(|&p| !self.raid_test_ours(p)).collect();
        let grid = Grid::new(self.cb);
        let map = self.cb.map();
        let (w, h) = (map.width() as f32 * 8.0, map.height() as f32 * 8.0);
        let mut places = Vec::new();
        let (mut x, mut z) = (SAMPLE / 2.0, SAMPLE / 2.0);
        while z < h {
            while x < w {
                let p = Vec3::xz(x, z);
                if self.raid_test_ours(p)
                    && theirs.iter().all(|&s| distance(s, p) >= min)
                    && grid.passable(p, Mobility::BOT)
                {
                    places.push(p);
                }
                x += SAMPLE;
            }
            x = SAMPLE / 2.0;
            z += SAMPLE;
        }
        let seed = self.cb.option("raid_seed").and_then(|v| v.parse::<u64>().ok()).unwrap_or_else(|| {
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(1, |d| d.as_nanos() as u64)
        });
        self.raid_test.rng = seed;
        self.log(&format!("raidtest: raid_seed {seed}"));
        let k = self.raid_test_opt("raid_groups", 2usize);
        let first = self.raid_test_opt("raid_first", 30i32) * 30;
        let respawn = self.raid_test_opt("raid_respawn", 30i32) * 30;
        self.raid_test.groups = vec![BTreeSet::new(); k];
        self.raid_test.born = vec![0; k];
        self.raid_test.next_spawn = (0..k).map(|i| first + i as i32 * respawn / k.max(1) as i32).collect();
        self.log(&format!("raidtest: {} spawn places, {k} groups", places.len()));
        self.raid_test.spawn_places = places;
    }

    fn raid_test_spawn_raids(&mut self, frame: i32) {
        let respawn = self.raid_test_opt("raid_respawn", 30i32) * 30;
        let comps: Vec<String> = (self.cb.option("raid_comp").unwrap_or_else(|| "cloakraid:3".into()))
            .split('/')
            .map(str::to_owned)
            .collect();
        let life = self.raid_test_opt("raid_life", 180i32) * 30;
        for slot in 0..self.raid_test.groups.len() {
            let alive = !self.raid_test.groups[slot].is_empty();
            // A raid lasts so long: the rest of it goes, and the next comes.
            if alive && frame - self.raid_test.born[slot] > life {
                let ids = std::mem::take(&mut self.raid_test.groups[slot]);
                for id in &ids {
                    if let Some(u) = springai::Unit::from_id(self.cb, *id) {
                        u.self_destruct(0);
                    }
                    self.raid_test.raiders.remove(id);
                }
                self.raid_test.expired += ids.len();
                self.log(&format!("raidgen: slot {slot}'s raid expires ({} left)", ids.len()));
                continue;
            }
            if alive {
                continue;
            }
            if self.raid_test.next_spawn[slot] == i32::MAX {
                self.raid_test.next_spawn[slot] = frame + respawn;
            }
            if frame < self.raid_test.next_spawn[slot] || self.raid_test.spawn_places.is_empty() {
                continue;
            }
            let places = std::mem::take(&mut self.raid_test.spawn_places);
            let at = self.raid_test.pick(&places).unwrap_or(self.raid_test.base);
            self.raid_test.spawn_places = places;
            let ci = self.raid_test.pick(&(0..comps.len()).collect::<Vec<_>>()).unwrap_or(0);
            let comp = parse_comp(&comps[ci]);
            self.cb.cheats_set_enabled(true);
            let mut units = Vec::new();
            let mut k = 0;
            for (name, n) in &comp {
                for _ in 0..*n {
                    let p = Vec3::xz(at.x + 40.0 * (k % 3) as f32, at.z + 40.0 * (k / 3) as f32);
                    units.extend(self.raid_test_spawn(name, p, false));
                    k += 1;
                }
            }
            self.cb.cheats_set_enabled(false);
            let metal = self.metal;
            let team = self.team_id;
            let mut fighters = Vec::new();
            let mut medium = false;
            for &u in &units {
                let name = self.defs.name_of(u);
                medium |= self.unit_classes.medium_raiders.contains(name);
                u.set_move_state(1, 0);
                let cost = u.def().map_or(0.0, |d| d.cost(metal));
                fighters.push(Fighter::new_raider(u, cost, team));
                self.raid_test.raiders.insert(u.id(), cost);
                self.raid_test.groups[slot].insert(u.id());
            }
            self.raider_add_squad(fighters, if medium { 'm' } else { 's' });
            self.raid_test.spawned += 1;
            self.raid_test.born[slot] = frame;
            self.raid_test.next_spawn[slot] = i32::MAX;
            let names: Vec<String> = comp.iter().map(|(n, c)| format!("{n}:{c}")).collect();
            let line = format!(
                "raidgen: group {} ({}) at {:.0},{:.0}, slot {slot}",
                self.raid_test.spawned,
                names.join(","),
                at.x,
                at.z
            );
            self.log(&line);
        }
    }

    pub fn raid_test_unit_destroyed(&mut self, unit: springai::Unit) {
        if !self.raid_testing() {
            return;
        }
        let id = unit.id();
        let frame = self.frame;
        let t = &mut self.raid_test;
        if let Some((cost, mex)) = t.assets.remove(&id) {
            t.lost += 1;
            t.lost_mexes += usize::from(mex);
            t.lost_metal += cost;
            let p = unit.pos();
            let name = self.defs.name_of(unit).to_owned();
            self.log(&format!("qrfloss: t={} {name} at {:.0},{:.0}", frame / 30, p.x, p.z));
        }
        let t = &mut self.raid_test;
        if let Some(cost) = t.force.remove(&id) {
            t.force_lost += 1;
            t.force_lost_metal += cost;
        }
        if let Some(cost) = t.raiders.remove(&id) {
            t.raiders_lost += 1;
            t.raiders_lost_metal += cost;
            for g in &mut t.groups {
                g.remove(&id);
            }
        }
        self.qrf_remove_unit(id);
    }

    pub fn raid_test_enemy_destroyed(&mut self, enemy: springai::Unit) {
        if !self.raid_testing() {
            return;
        }
        if enemy.def().is_some_and(|d| d.speed() > 0.0) || enemy.def().is_none() {
            self.raid_test.kills += 1;
            self.raid_test.kills_metal += enemy.def().map_or(0.0, |d| d.cost(self.metal));
        }
        self.qrf_enemy_destroyed(enemy.id());
    }
}
