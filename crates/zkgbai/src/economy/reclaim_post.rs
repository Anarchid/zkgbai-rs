//! Fortified reclaim posts. Observation is deliberately wider than admission.
//! Uses only AI-visible wrecks and remembered contacts; never spectator state.
use std::collections::{BTreeMap, BTreeSet, VecDeque};

use serde_json::json;
use springai::{Feature, Unit, Vec3};

use super::tasks::TaskKind;
use crate::{Ai, util::distance};

const SAMPLE: i32 = 300;
const HEIGHT: f32 = 56.0; // staticcon collision-volume height; model header is only 27.
const MIN_STOCK: f32 = 360.0;
const EARTH_BUDGET: f32 = 400.0; // Still capped at half the stock left after buying the Caretaker.
const MAX_SITES: usize = 128;

#[derive(Clone, Copy, Debug, PartialEq)]
enum Shelter {
    Pit,
    Wall,
    Water,
}

#[derive(Clone, Copy)]
struct Geometry {
    kind: Shelter,
    center: Vec3,
    level: f32,
    radius: f32,
}

impl Geometry {
    fn new(at: Vec3, low: f32, high: f32, footprint: f32) -> Self {
        let kind = if low <= 0.0 {
            Shelter::Water
        } else if low - 2.0 * HEIGHT > 8.0 {
            Shelter::Pit
        } else {
            Shelter::Wall
        };
        let level = match kind {
            Shelter::Pit => low - 2.0 * HEIGHT,
            Shelter::Wall => high + HEIGHT,
            Shelter::Water => 0.0,
        };
        // The line brush and inward slope must leave the entire snapped build footprint flat.
        Self { kind, center: at, level, radius: footprint + if kind == Shelter::Wall { 64.0 } else { 8.0 } }
    }
    fn base(&self) -> Vec3 {
        Vec3::new(
            self.center.x,
            if self.kind == Shelter::Pit { self.level } else { self.center.y.max(0.0) },
            self.center.z,
        )
    }
    fn polygon(&self) -> Vec<Vec3> {
        let p = self.center;
        let r = self.radius;
        vec![
            Vec3::xz(p.x - r, p.z - r),
            Vec3::xz(p.x + r, p.z - r),
            Vec3::xz(p.x + r, p.z + r),
            Vec3::xz(p.x - r, p.z + r),
        ]
    }
}

#[derive(Clone)]
struct Wreck {
    feature: Feature,
    at: Vec3,
    metal: f32,
}
struct Gun {
    at: Vec3,
    range: f32,
    dps: f32,
    muzzle: f32,
}
struct Contact {
    at: Vec3,
    range: f32,
    unknown: bool,
}

#[derive(Clone, Default)]
struct Reading {
    frame: i32,
    metal: f32,
    contested: f32,
    angle: usize,
    guns: usize,
    dps: f32,
    covered: usize,
    exposed: bool,
    long: bool,
    unknown: bool,
}
impl Reading {
    fn good(&self) -> bool {
        self.metal >= MIN_STOCK
            && self.contested >= self.metal * 0.25
            && self.guns >= 2
            && self.dps >= 100.0
            && self.covered >= 22
            && !self.exposed
            && !self.unknown
    }
}

fn persistent(history: &VecDeque<Reading>, frame: i32) -> bool {
    history.len() == 7
        && history.front().is_some_and(|s| frame - s.frame == 6 * SAMPLE)
        && history.iter().all(Reading::good)
        && history.iter().all(|s| s.angle == history.back().unwrap().angle)
        && history.iter().filter(|s| s.long).count() as f32 / history.len() as f32 <= 0.1
}

fn in_half(base: Vec3, at: Vec3, range: f32, angle: usize) -> bool {
    let bearing = angle as f32 * std::f32::consts::TAU / 24.0;
    let dx = at.x - base.x;
    let dz = at.z - base.z;
    dx * dx + dz * dz + (at.y - base.y).powi(2) <= range * range && dx * bearing.cos() + dz * bearing.sin() >= -0.01
}

struct Project {
    geometry: Geometry,
    angle: usize,
    worker: Unit,
    created: i32,
    issued: i32,
    building: bool,
    unit: Option<Unit>,
    helpers_before: BTreeSet<i32>,
    helper_spent: BTreeMap<i32, f32>,
    budget: f32,
    stock: f32,
    quoted: bool,
    last_order: i32,
    sampled_spend: f32,
}
struct Post {
    unit: Unit,
    geometry: Geometry,
    angle: usize,
    since: i32,
    reclaimed_estimate: f32,
    spent_estimate: f32,
    last: i32,
    last_order: i32,
    activity: Option<(bool, i32)>, // true = feature reclaim; false = repair/assist
}

#[derive(Default)]
pub(super) struct ReclaimPosts {
    history: BTreeMap<(i32, i32), VecDeque<Reading>>,
    project: Option<Project>,
    posts: Vec<Post>,
    retry: BTreeMap<(i32, i32), i32>,
    conch_until: i32,
}

impl Ai {
    /// Staged engine fixture: leave strategic constructors alone so the real
    /// detector and construction lifecycle can be exercised on a fixed scene.
    pub(crate) fn reclaim_post_fixture(&mut self, frame: i32) {
        // Spawned fixture constructors do not receive a factory-exit idle
        // event. Register them without the normal opening task generation.
        for unit in self.cb.team_units() {
            if self.eco.new_workers.contains(&unit.id()) && !unit.is_being_built() {
                self.eco.insert_worker(super::Worker::new(unit, self.team_id));
                self.eco.new_workers.remove(&unit.id());
            }
        }
        self.eco_update_income(frame);
        self.eco_reclaim_post_update(frame);
    }

    fn reclaim_wrecks(&self) -> Vec<Wreck> {
        self.cb
            .features()
            .into_iter()
            .filter_map(|feature| {
                let def = feature.def()?;
                if !def.is_reclaimable() || !self.los.is_in_los(feature.pos()) {
                    return None;
                }
                let metal = def.contained_resource(self.metal) * feature.reclaim_left().clamp(0.0, 1.0);
                (metal >= 1.0).then(|| Wreck { feature, at: feature.pos(), metal })
            })
            .collect()
    }

    fn reclaim_contacts(&self) -> Vec<Contact> {
        self.war
            .targets
            .values()
            .filter(|e| {
                !e.is_dead && (e.visible || e.is_radar_visible || e.is_static || self.frame - e.last_seen <= 30 * 60)
            })
            .filter(|e| !e.identified || e.can_attack())
            .map(|e| Contact {
                at: e.position,
                range: e.def_max_weapon_range().max(e.threat_radius),
                unknown: !e.identified,
            })
            .collect()
    }

    fn reclaim_guns(&self) -> Vec<Gun> {
        self.cb
            .friendly_units()
            .into_iter()
            .filter(|u| {
                u.health() > 0.0
                    && !u.is_being_built()
                    && !u.is_paralyzed()
                    && u.rules_param_float("disarmed", 0.0) == 0.0
                    && u.rules_param_float("lowpower", 0.0) == 0.0
            })
            .filter_map(|u| {
                let d = u.def()?;
                if !matches!(
                    self.defs.name(d),
                    "turretlaser"
                        | "turretmissile"
                        | "turretriot"
                        | "turretheavylaser"
                        | "turretgauss"
                        | "turretheavy"
                        | "turretantiheavy"
                        | "turrettorp"
                ) {
                    return None;
                }
                let dps = d
                    .weapon_mounts()
                    .iter()
                    .filter_map(|m| m.weapon_def())
                    .filter(|w| w.reload() > 0.0)
                    .map(|w| {
                        w.damage_types().first().copied().unwrap_or(0.0)
                            * w.salvo_size() as f32
                            * w.projectiles_per_shot() as f32
                            / w.reload()
                    })
                    .sum();
                Some(Gun { at: u.pos(), range: d.max_weapon_range(), dps, muzzle: d.height().max(16.0) })
            })
            .collect()
    }

    fn reclaim_geometry(&self, at: Vec3) -> Geometry {
        let def = self.defs.get("staticcon").unwrap();
        let r = def.x_size().max(def.z_size()) as f32 * 4.0;
        let heights: Vec<_> = [-r, 0.0, r]
            .into_iter()
            .flat_map(|x| [-r, 0.0, r].map(|z| self.cb.map().elevation_at(at.x + x, at.z + z)))
            .collect();
        Geometry::new(
            at,
            heights.iter().copied().fold(f32::INFINITY, f32::min),
            heights.iter().copied().fold(f32::NEG_INFINITY, f32::max),
            r,
        )
    }

    fn reclaim_line(&self, gun: &Gun, at: Vec3) -> bool {
        let from = Vec3::new(gun.at.x, gun.at.y + gun.muzzle, gun.at.z);
        let to = Vec3::new(at.x, self.cb.map().elevation_at(at.x, at.z).max(0.0) + 16.0, at.z);
        let d = distance(from, to);
        if d.hypot(from.y - to.y) > gun.range {
            return false;
        }
        let n = (d / 32.0).ceil().max(1.0) as usize;
        (1..n).all(|i| {
            let t = i as f32 / n as f32;
            let x = from.x + (to.x - from.x) * t;
            let z = from.z + (to.z - from.z) * t;
            self.cb.map().elevation_at(x, z) < from.y + (to.y - from.y) * t
        })
    }

    fn reclaim_reading(&self, g: Geometry, wrecks: &[Wreck], guns: &[Gun], contacts: &[Contact]) -> Reading {
        let range = self.defs.get("staticcon").unwrap().build_distance();
        let mut s = Reading { frame: self.frame, ..Reading::default() };
        for angle in 0..24 {
            let mut metal = 0.0;
            let mut contested = 0.0;
            for w in wrecks.iter().filter(|w| in_half(g.base(), w.at, range, angle)) {
                metal += w.metal;
                if contacts.iter().any(|e| !e.unknown && distance(w.at, e.at) <= e.range) {
                    contested += w.metal;
                }
            }
            if metal > s.metal {
                s.metal = metal;
                s.contested = contested;
                s.angle = angle;
            }
        }
        // The shelter deliberately denies shots into its interior. Count
        // nominal supporting reach here; actual fire lines cover the ring.
        for gun in guns.iter().filter(|gun| distance(gun.at, g.center).hypot(gun.at.y - g.center.y) <= gun.range) {
            s.guns += 1;
            s.dps += gun.dps;
        }
        s.covered = (0..24)
            .filter(|i| {
                let a = *i as f32 * std::f32::consts::TAU / 24.0;
                let p = Vec3::xz(g.center.x + 300.0 * a.cos(), g.center.z + 300.0 * a.sin());
                guns.iter().any(|gun| self.reclaim_line(gun, p))
            })
            .count();
        for e in contacts {
            let d = distance(g.center, e.at);
            s.unknown |= e.unknown && d < 1000.0;
            s.exposed |= !e.unknown && d < e.range + 80.0;
            s.long |= !e.unknown && e.range > 500.0 && d < e.range + 80.0;
        }
        s
    }

    fn reclaim_clear(&self, g: Geometry) -> bool {
        // Reserve room for slopes; never excavate another building or worker.
        self.cb
            .friendly_units_in_cylinder(g.center, g.radius + 96.0)
            .iter()
            .all(|u| u.def().is_some_and(|d| d.speed() > 0.0) && distance(u.pos(), g.center) > g.radius + 32.0)
            && self.eco.construction_tasks.iter().all(|id| distance(self.eco.task_pos(*id), g.center) > g.radius + 96.0)
    }

    fn reclaim_worker(&self, g: Geometry) -> Option<Unit> {
        let nano = self.defs.get("staticcon")?;
        self.eco
            .workers
            .values()
            .filter(|w| {
                !w.is_com
                    && !w.is_chicken
                    && !w.assigned_plop
                    && !self.eco_worker_held(w.id)
                    && !springai::reserved::is_reserved(self.cb, w.id)
                    && !self.recovery_owns(w.id)
                    && !self.jump_in_progress(w.unit)
                    && self.logistics_assignment(w.id).is_none()
                    && w.task
                        .is_none_or(|id| self.eco.tasks.get(id).is_some_and(|t| matches!(t.kind, TaskKind::Reclaim(_))))
            })
            .filter(|w| {
                w.unit.health() > 0.0
                    && w.unit.team() == self.team_id
                    && w.unit.def().is_some_and(|d| {
                        d.terraform_speed() > 0.0
                            && d.build_options().iter().any(|b| b.id() == nano.id())
                            && distance(w.get_pos(), g.center) > g.radius + 32.0
                            && distance(w.get_pos(), g.center).hypot(w.get_pos().y - g.base().y)
                                < d.build_distance() - 16.0
                    })
            })
            .min_by(|a, b| distance(a.get_pos(), g.center).total_cmp(&distance(b.get_pos(), g.center)))
            .map(|w| w.unit)
    }

    fn reclaim_note(&self, event: &str, g: Geometry, s: &Reading, why: &str) {
        let range = self.defs.get("staticcon").map_or(500.0, |d| d.build_distance());
        let nearby: Vec<_> =
            self.reclaim_wrecks().into_iter().filter(|w| in_half(g.base(), w.at, range, s.angle)).collect();
        let wrecks: Vec<_> = nearby
            .iter()
            .take(256)
            .map(|w| json!({"id":w.feature.id(),"pos":[w.at.x,w.at.y,w.at.z],"metal":w.metal}))
            .collect();
        let support:Vec<_>=self.cb.friendly_units_in_cylinder(g.center,1200.0).into_iter()
            .filter(|u|u.def().is_some_and(|d|d.speed()==0.0 && d.max_weapon_range()>0.0))
            .take(64).map(|u|json!({"id":u.id(),"def":self.defs.name_of(u),"pos":[u.pos().x,u.pos().y,u.pos().z],"built":!u.is_being_built()})).collect();
        self.op_emit(
            "reclaim_post",
            json!({"event":event,"frame":self.frame,"site":[g.center.x,g.center.y,g.center.z],
            "shelter":format!("{:?}",g.kind),"level":g.level,"radius":g.radius,"metal":s.metal,"contested":s.contested,
            "half_disc_degrees":s.angle*15,"guns":s.guns,"dps":s.dps,"covered":s.covered,"bearings":24,
            "enemy_reach":s.exposed,"long_range_reach":s.long,"unknown_contact":s.unknown,"why":why,
            "policy_version":1,"wrecks":wrecks,"wreck_count":nearby.len(),"wrecks_truncated":nearby.len()>256,"support":support}),
        );
    }

    pub(super) fn eco_reclaim_post_update(&mut self, frame: i32) {
        if frame % 30 != self.offset % 30 || self.defs.get("staticcon").is_none() {
            return;
        }
        if frame % SAMPLE != self.offset % SAMPLE
            && self.eco.reclaim_posts.project.is_none()
            && self.eco.reclaim_posts.posts.is_empty()
        {
            return;
        }
        let wrecks = self.reclaim_wrecks();
        self.reclaim_service_posts(&wrecks);
        self.reclaim_service_project(&wrecks);
        if frame % SAMPLE != self.offset % SAMPLE {
            return;
        }
        let guns = self.reclaim_guns();
        let contacts = self.reclaim_contacts();
        let mut seeds = BTreeSet::new();
        for w in wrecks.iter().filter(|w| w.metal >= 20.0 && guns.iter().any(|g| distance(g.at, w.at) < 900.0)) {
            let x = (w.at.x / 128.0).round() as i32;
            let z = (w.at.z / 128.0).round() as i32;
            for dx in -3..=3 {
                for dz in -3..=3 {
                    seeds.insert((x + dx, z + dz));
                }
            }
        }
        // Stable coordinate ordering keeps persistence meaningful. Rotate the
        // bounded window only once per two minutes so each site gets a minute.
        let seeds: Vec<_> = seeds.into_iter().collect();
        let begin = if seeds.is_empty() { 0 } else { (frame / (30 * 120)) as usize * MAX_SITES % seeds.len() };
        let mut selected = None;
        let mut counts = BTreeMap::<&str, usize>::new();
        let mut best = None;
        for key in seeds.iter().cycle().skip(begin).take(seeds.len().min(MAX_SITES)).copied() {
            let at = Vec3::new(
                key.0 as f32 * 128.0,
                self.cb.map().elevation_at(key.0 as f32 * 128.0, key.1 as f32 * 128.0),
                key.1 as f32 * 128.0,
            );
            if at.x < 128.0
                || at.z < 128.0
                || at.x > self.cb.map().width() as f32 * 8.0 - 128.0
                || at.z > self.cb.map().height() as f32 * 8.0 - 128.0
            {
                continue;
            }
            let g = self.reclaim_geometry(at);
            let s = self.reclaim_reading(g, &wrecks, &guns, &contacts);
            let history = self.eco.reclaim_posts.history.entry(key).or_default();
            if history.back().is_some_and(|old| frame - old.frame != SAMPLE) {
                history.clear();
            }
            history.push_back(s.clone());
            while history.len() > 7 {
                history.pop_front();
            }
            let ready = persistent(history, frame);
            let why = if s.metal < MIN_STOCK {
                "stock"
            } else if s.guns < 2 || s.dps < 100.0 || s.covered < 22 {
                "fire_cover"
            } else if s.unknown {
                "unknown_threat"
            } else if s.exposed {
                "enemy_reach"
            } else if s.contested < s.metal * 0.25 {
                "ordinary_reclaim"
            } else if !ready {
                "persistence"
            } else {
                "eligible"
            };
            *counts.entry(why).or_default() += 1;
            if best.as_ref().is_none_or(|(_, old): &(Geometry, Reading)| s.metal > old.metal) {
                best = Some((g, s.clone()));
            }
            if !ready {
                continue;
            }
            if g.kind == Shelter::Water {
                if !self.dev_off("reclaimpost") {
                    self.eco.reclaim_posts.conch_until = frame + SAMPLE * 2;
                }
                self.reclaim_note("water", g, &s, "request Conch from an existing amphibious factory");
                continue;
            }
            let why = if self.dev_off("reclaimpost") {
                "disabled"
            } else if self.eco.reclaim_posts.project.is_some() || self.eco.reclaim_posts.posts.len() >= 2 {
                "capacity"
            } else if self.eco.hard_estalled || self.eco.effective_income < 10.0 {
                "economy"
            } else if self.eco.reclaim_posts.retry.iter().any(|(old, until)| {
                frame < *until && ((old.0 - key.0) as f32).hypot((old.1 - key.1) as f32) * 128.0 < 700.0
            }) {
                "retry"
            } else if self.cb.friendly_units_in_cylinder(at, 700.0).iter().any(|u| self.defs.name_of(*u) == "staticcon")
            {
                "existing_caretaker"
            } else if !self.reclaim_clear(g) {
                "occupied"
            } else if !self.eco_build_check(self.defs.get("staticcon").unwrap(), at, 0)
                || !self.cb.map().is_possible_to_build_at(self.defs.get("staticcon").unwrap(), at, 0)
            {
                "build_site"
            } else if self.reclaim_worker(g).is_none() {
                "builder"
            } else {
                "admit"
            };
            self.reclaim_note("candidate", g, &s, why);
            if why == "admit" && selected.as_ref().is_none_or(|(_, old): &(Geometry, Reading)| s.metal > old.metal) {
                selected = Some((g, s));
            }
        }
        self.eco.reclaim_posts.history.retain(|_, h| h.back().is_some_and(|s| frame - s.frame <= SAMPLE));
        self.eco.reclaim_posts.retry.retain(|_, until| frame < *until);
        self.op_emit("reclaim_post",json!({"event":"survey","frame":frame,"seeds":seeds.len(),"sampled":counts.values().sum::<usize>(),"reasons":counts,"enabled":!self.dev_off("reclaimpost")}));
        if let Some((g, s)) = best {
            self.reclaim_note("best", g, &s, "highest stock in this scan");
        }
        if let Some((g, s)) = selected {
            self.reclaim_begin(g, s);
        }
    }

    pub(super) fn reclaim_wants_conch(&self) -> bool {
        !self.dev_off("reclaimpost")
            && self.frame < self.eco.reclaim_posts.conch_until
            && !self.cb.team_units().iter().any(|u| self.defs.name_of(*u) == "amphcon")
    }

    fn reclaim_begin(&mut self, g: Geometry, s: Reading) {
        let Some(worker) = self.reclaim_worker(g) else { return };
        let cost = self.defs.get("staticcon").unwrap().cost(self.metal);
        let budget = EARTH_BUDGET.min((s.metal - cost) * 0.5).max(0.0);
        let before =
            self.cb.team_units().into_iter().filter(|u| self.defs.name_of(*u) == "terraunit").map(|u| u.id()).collect();
        self.eco_suspend_assignment(worker.id(), "fortified reclaim");
        if let Some(w) = self.eco.workers.get_mut(&worker.id()) {
            w.clear_task(&mut self.eco.tasks);
        }
        let mut polygon = g.polygon();
        if g.kind == Shelter::Pit {
            self.terraform_level(&[worker], &polygon, g.level, false);
        } else {
            polygon.push(polygon[0]);
            self.terraform_line_level(&[worker], &polygon, g.level, false);
        }
        self.eco_hold_terraform(&[worker], 90);
        self.reclaim_note("start", g, &s, "earthwork quote checked on next update; cost cap applies");
        self.eco.reclaim_posts.project = Some(Project {
            geometry: g,
            angle: s.angle,
            worker,
            created: self.frame,
            issued: self.frame,
            building: false,
            unit: None,
            helpers_before: before,
            helper_spent: BTreeMap::new(),
            budget,
            stock: s.metal,
            quoted: false,
            last_order: self.frame,
            sampled_spend: 0.0,
        });
    }

    fn reclaim_service_project(&mut self, wrecks: &[Wreck]) {
        let Some(mut p) = self.eco.reclaim_posts.project.take() else { return };
        let frame = self.frame;
        let g = p.geometry;
        let def = self.defs.get("staticcon").unwrap();
        let ours = p.worker.health() > 0.0 && p.worker.team() == self.team_id;
        let available = ours
            && !springai::reserved::is_reserved(self.cb, p.worker.id())
            && !self.recovery_owns(p.worker.id())
            && self.eco.workers.get(&p.worker.id()).is_some_and(|w| !w.is_chicken && w.raid_defense.is_none());
        if ours {
            p.sampled_spend += p.worker.resource_use(self.metal).max(0.0);
        }
        let contacts = self.reclaim_contacts();
        let danger = contacts.iter().any(|e| {
            distance(e.at, g.center) < if e.unknown { 1000.0 } else { e.range + 80.0 }
                || (ours && distance(e.at, p.worker.pos()) < if e.unknown { 1000.0 } else { e.range + 32.0 })
        });
        let cover_lost = if frame % SAMPLE == self.offset % SAMPLE {
            let reading = self.reclaim_reading(g, wrecks, &self.reclaim_guns(), &contacts);
            reading.guns < 2 || reading.dps < 100.0 || reading.covered < 22
        } else {
            false
        };
        let stock: f32 =
            wrecks.iter().filter(|w| in_half(g.base(), w.at, def.build_distance(), p.angle)).map(|w| w.metal).sum();
        let helpers: Vec<_> = self
            .cb
            .team_units()
            .into_iter()
            .filter(|u| {
                self.defs.name_of(*u) == "terraunit"
                    && !p.helpers_before.contains(&u.id())
                    && distance(u.pos(), g.center) < g.radius + 160.0
            })
            .collect();
        let mut quote = 0.0;
        for u in &helpers {
            let spent = u.rules_param_float("terraform_spent", f32::NAN);
            let estimate = u.rules_param_float("terraform_estimate", f32::NAN);
            if spent.is_finite() && estimate.is_finite() {
                p.helper_spent.insert(u.id(), spent);
                quote += estimate;
                p.quoted = true;
            } else {
                quote = f32::INFINITY;
            }
        }
        let spent: f32 = p.helper_spent.values().sum();
        let terrain_done = self.reclaim_terrain_done(g);
        let reason = if self.dev_off("reclaimpost") {
            Some("disabled")
        } else if !available {
            Some("builder_unavailable")
        } else if danger {
            Some("enemy_reach")
        } else if cover_lost {
            Some("fire_cover_lost")
        } else if self.eco.hard_estalled {
            Some("energy_stall")
        } else if frame - p.created > 30 * 180 {
            Some("timeout")
        } else if !p.building && (quote > p.budget || spent > p.budget) {
            Some("earthwork_cost")
        } else if !p.building && !p.quoted && frame - p.created > 90 && !terrain_done {
            Some("missing_quote")
        } else if !p.building && stock < def.cost(self.metal) + p.budget + 60.0 {
            Some("stock_depleted")
        } else {
            None
        };
        if let Some(reason) = reason {
            if available {
                p.worker.stop(0);
            }
            self.eco_release_terraform(&[p.worker]);
            self.eco
                .reclaim_posts
                .retry
                .insert(((g.center.x / 128.0).round() as i32, (g.center.z / 128.0).round() as i32), frame + 30 * 600);
            self.op_emit("reclaim_post",json!({"event":"abort","frame":frame,"site":[g.center.x,g.center.z],"why":reason,
                "earthwork_quote":quote.is_finite().then_some(quote),"earthwork_spent_observed":spent,"budget":p.budget,"worker_spend_estimate":p.sampled_spend,"stock":stock}));
            return;
        }
        self.eco_hold_terraform(&[p.worker], 90);
        if !p.building && terrain_done {
            let at = Vec3::new(g.center.x, self.cb.map().elevation_at(g.center.x, g.center.z), g.center.z);
            if self.eco_build_check(def, at, 0) && self.cb.map().is_possible_to_build_at(def, at, 0) {
                p.worker.build(def, at, 0, 0);
                p.building = true;
                p.issued = frame;
                p.last_order = frame;
                self.op_emit("reclaim_post",json!({"event":"build","frame":frame,"site":[at.x,at.y,at.z],"earthwork_spent_observed":spent,"earthwork_quote":quote,"budget":p.budget}));
            }
        }
        if p.building {
            p.unit = self
                .cb
                .team_units()
                .into_iter()
                .find(|u| self.defs.name_of(*u) == "staticcon" && distance(u.pos(), g.center) < 16.0);
            if let Some(unit) = p.unit {
                if !unit.is_being_built() {
                    self.eco_release_terraform(&[p.worker]);
                    self.op_emit("reclaim_post",json!({"event":"ready","frame":frame,"unit":unit.id(),"site":[g.center.x,g.center.z],"initial_stock":p.stock,"remaining_stock":stock,"build_seconds":(frame-p.created)/30,"worker_spend_estimate":p.sampled_spend}));
                    self.eco.reclaim_posts.posts.push(Post {
                        unit,
                        geometry: g,
                        angle: p.angle,
                        since: frame,
                        reclaimed_estimate: 0.0,
                        spent_estimate: p.sampled_spend,
                        last: frame,
                        last_order: 0,
                        activity: None,
                    });
                    return;
                }
                if frame - p.last_order >= 150 {
                    p.worker.repair(unit, 0);
                    p.last_order = frame;
                }
            } else if frame - p.last_order >= 150 {
                p.worker.build(def, g.base(), 0, 0);
                p.last_order = frame;
            }
        } else if frame - p.issued > 90 && !super::terraform::is_terraforming(p.worker) {
            // Do not repeat excavation blindly. Interrupted shapes are abandoned.
            self.eco_release_terraform(&[p.worker]);
            self.eco
                .reclaim_posts
                .retry
                .insert(((g.center.x / 128.0).round() as i32, (g.center.z / 128.0).round() as i32), frame + 30 * 600);
            self.op_emit(
                "reclaim_post",
                json!({"event":"abort","frame":frame,"why":"terraform_interrupted","site":[g.center.x,g.center.z]}),
            );
            return;
        }
        self.eco.reclaim_posts.project = Some(p);
    }

    fn reclaim_terrain_done(&self, g: Geometry) -> bool {
        let r = g.radius;
        let step = 8.0;
        let n = (2.0 * r / step).ceil() as usize;
        (0..=n).all(|x| {
            (0..=n).all(|z| {
                if g.kind == Shelter::Wall && x != 0 && z != 0 && x != n && z != n {
                    return true;
                }
                let h = self.cb.map().elevation_at(g.center.x - r + x as f32 * step, g.center.z - r + z as f32 * step);
                if g.kind == Shelter::Pit { (h - g.level).abs() <= 4.0 && h > 0.0 } else { h >= g.level - 4.0 }
            })
        })
    }

    fn reclaim_service_posts(&mut self, wrecks: &[Wreck]) {
        let mut posts = std::mem::take(&mut self.eco.reclaim_posts.posts);
        let mut kept = Vec::new();
        for mut p in posts.drain(..) {
            if p.unit.health() <= 0.0 || p.unit.team() != self.team_id {
                self.op_emit("reclaim_post",json!({"event":"lost","frame":self.frame,"unit":p.unit.id(),"lifetime_seconds":(self.frame-p.since)/30,"reclaimed_estimate":p.reclaimed_estimate,"worker_spend_estimate":p.spent_estimate}));
                continue;
            }
            p.reclaimed_estimate += p.unit.resource_make(self.metal).max(0.0) * (self.frame - p.last) as f32 / 30.0;
            p.last = self.frame;
            let range = p.unit.def().map_or(500.0, |d| d.build_distance());
            let targets: Vec<_> = wrecks.iter().filter(|w| in_half(p.unit.pos(), w.at, range, p.angle)).collect();
            if !springai::reserved::is_reserved(self.cb, p.unit.id()) && self.frame - p.last_order >= 150 {
                let reclaim = targets.iter().max_by(|a, b| a.metal.total_cmp(&b.metal));
                let repair = if reclaim.is_none() {
                    self.cb
                        .friendly_units_in_cylinder(p.unit.pos(), range)
                        .into_iter()
                        .filter(|u| {
                            u.id() != p.unit.id()
                                && u.health() > 0.0
                                && (u.is_being_built() || u.health() + 1.0 < u.max_health())
                                && distance(u.pos(), p.unit.pos()).hypot(u.pos().y - p.unit.pos().y) <= range
                        })
                        .max_by(|a, b| {
                            // Repair damage first, then help complete construction.
                            let value = |u: Unit| {
                                if u.is_being_built() {
                                    1.0 + u.build_progress()
                                } else {
                                    2.0 + (1.0 - u.health() / u.max_health().max(1.0))
                                }
                            };
                            value(*a).total_cmp(&value(*b))
                        })
                } else {
                    None
                };
                let next = reclaim.map(|w| (true, w.feature.id())).or_else(|| repair.map(|u| (false, u.id())));
                if next != p.activity || p.unit.num_current_commands() == 0 {
                    if let Some(w) = reclaim {
                        p.unit.reclaim_feature(w.feature, 0);
                    } else if let Some(u) = repair {
                        p.unit.repair(u, 0);
                    }
                    p.activity = next;
                }
                p.last_order = self.frame;
            }
            if self.frame % SAMPLE == self.offset % SAMPLE {
                self.op_emit("reclaim_post",json!({"event":"yield","frame":self.frame,"unit":p.unit.id(),"site":[p.geometry.center.x,p.geometry.center.z],"hp":p.unit.health(),"activity":p.activity.map(|(reclaim,id)|json!({"kind":if reclaim{"reclaim"}else{"repair_or_assist"},"target":id})),"remaining_stock":targets.iter().map(|w|w.metal).sum::<f32>(),"reclaimed_estimate":p.reclaimed_estimate,"worker_spend_estimate":p.spent_estimate}));
            }
            kept.push(p);
        }
        self.eco.reclaim_posts.posts = kept;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn never_dig_a_floating_caretaker() {
        assert_eq!(Geometry::new(Vec3::new(0.0, 60.0, 0.0), 58.0, 61.0, 24.0).kind, Shelter::Wall);
        let g = Geometry::new(Vec3::new(0.0, 178.0, 0.0), 177.0, 179.0, 24.0);
        assert_eq!(g.kind, Shelter::Pit);
        assert_eq!(g.level, 65.0);
        assert_eq!(Geometry::new(Vec3::ZERO, -1.0, 3.0, 24.0).kind, Shelter::Water);
    }
    #[test]
    fn pit_depth_reduces_actual_reclaim_reach() {
        let base = Vec3::new(1000.0, 66.0, 1000.0);
        assert!(!in_half(base, Vec3::new(1490.0, 178.0, 1000.0), 500.0, 0));
        assert!(in_half(base, Vec3::new(1480.0, 178.0, 1000.0), 500.0, 0));
        assert!(!in_half(base, Vec3::new(900.0, 178.0, 1000.0), 500.0, 0));
    }
    #[test]
    fn intermittent_pressure_or_missing_history_is_not_persistence() {
        let mut h: VecDeque<_> = (0..7)
            .map(|i| Reading {
                frame: i * SAMPLE,
                metal: 500.0,
                contested: 200.0,
                guns: 3,
                dps: 200.0,
                covered: 24,
                ..Reading::default()
            })
            .collect();
        assert!(persistent(&h, 1800));
        h[3].long = true;
        assert!(!persistent(&h, 1800));
        h[3].long = false;
        h[3].unknown = true;
        assert!(!persistent(&h, 1800));
        h[3].unknown = false;
        h[3].angle = 1;
        assert!(!persistent(&h, 1800));
        h[3].angle = 0;
        h.pop_front();
        assert!(!persistent(&h, 1800));
    }
}
