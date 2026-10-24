//! Firefighters: airborne quick-reaction squads against raids on our
//! economy, once we have an air factory and raiders have been killing our
//! mexes lately (Scythes count double: cloaked, they get through where
//! nothing else does).
//!
//! A squad is of one kind, picked at random among those our air factories
//! build, sized for the raids seen at our losses (their health, the 75th
//! percentile over the memory):
//! - Locusts (`gunshipraid`), enough to kill such a raid in a few seconds
//!   (raiders mostly cannot shoot back at them);
//! - Blastwings (`gunshipbomb`): napalm drones, two hits' worth, kept
//!   landed at home and thrown at a raid a few at a time (they die
//!   hitting);
//! - Phoenixes (`bomberriot`): napalm carpets on the raid, rearmed on a
//!   pad between runs;
//! - Magpies (`planesupport`): strafing, slowing support planes that stay
//!   over the raid while their twelve shots last.
//!
//! A fire is a group of enemy ground units, mostly raiders, near our
//! mexes. The squad answers only isolated fires: no anti-air known about
//! it, no stronger army with it. It never goes after anything else. Squads
//! below strength are refilled by the factory; one that has lost most of
//! itself is let go (its survivors stay firefighters) and a new kind is
//! picked if the losses go on.

use std::collections::{BTreeMap, BTreeSet};

use springai::{NO_TIMEOUT, Unit, Vec3};

use crate::Ai;
use crate::defs::cmd;
use crate::economy::factory::FacKind;
use crate::military::Enemy;
use crate::util::{distance, kgb_rand};

/// Frames between updates.
const PERIOD: i32 = 15;

/// Losses are remembered this long.
pub(crate) const MEMORY: i32 = 30 * 300;

/// Firefighters are wanted from this much loss weight in the memory (a
/// mex counts 1, an energy building 0.5; to Scythes, double).
pub(crate) const WANTED_AT: f32 = 2.0;

/// A loss goes unanswered when the nearest home defense team was further
/// than this (elmos), or there was none: the ground answer was not there
/// in time, which is what aircraft buy (see `eco_air_fire`).
pub(crate) const UNANSWERED_DIST: f32 = 1500.0;

/// Raiders within this far of a loss, seen within this many frames, were
/// the raid.
const RAID_RADIUS: f32 = 600.0;
const RAID_SEEN: i32 = 30 * 10;

/// A raid's health if none was seen.
const DEFAULT_RAID_HP: f32 = 700.0;

/// Enemies within this far of each other are one fire; a fire is ours to
/// fight within this far of our mexes.
const FIRE_LINK: f32 = 400.0;
const FIRE_REACH: f32 = 1200.0;
/// Enemies seen within this many frames make fires.
const FIRE_SEEN: i32 = 60;

/// Known anti-air within this far of a fire keeps the squad off it.
const AA_RADIUS: f32 = 1200.0;

/// A squad answers fires once it has this share of its strength.
const READY_SHARE: f32 = 0.6;

/// A squad down to this share of its strength, with nothing on order, is
/// let go.
const SPENT_SHARE: f32 = 0.35;

/// Orders not met in this many frames are forgotten.
const ORDER_TIMEOUT: i32 = 30 * 120;

/// Blastwings sent at one fire wait this long before more follow.
const HIT_INTERVAL: i32 = 30 * 8;

/// A member this hurt goes home (Locusts heal idle).
const HURT: f32 = 0.35;

/// An order to a firefighter.
#[derive(Clone, Copy, PartialEq, Debug)]
enum Order {
    Move(Vec3),
    Fight(Vec3),
    Attack(i32),
    Pad,
}

/// An order to the same unit is repeated after this many frames at most;
/// to a new place only if it is this far from the old one.
const REORDER: i32 = 30 * 4;
const REORDER_DIST: f32 = 150.0;

/// A kind of firefighter.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Locust,
    Blastwing,
    Phoenix,
    Magpie,
}

const KINDS: [Kind; 4] = [Kind::Locust, Kind::Blastwing, Kind::Phoenix, Kind::Magpie];

impl Kind {
    pub fn unit(self) -> &'static str {
        match self {
            Kind::Locust => "gunshipraid",
            Kind::Blastwing => "gunshipbomb",
            Kind::Phoenix => "bomberriot",
            Kind::Magpie => "planesupport",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Kind::Locust => "locust",
            Kind::Blastwing => "blastwing",
            Kind::Phoenix => "phoenix",
            Kind::Magpie => "magpie",
        }
    }

    fn fac(self) -> FacKind {
        match self {
            Kind::Locust | Kind::Blastwing => FacKind::Gunship,
            Kind::Phoenix | Kind::Magpie => FacKind::Plane,
        }
    }

    fn of(name: &str) -> Option<Kind> {
        KINDS.iter().copied().find(|k| k.unit() == name)
    }

    fn by_label(label: &str) -> Option<Kind> {
        KINDS.iter().copied().find(|k| k.label() == label)
    }

    /// Squad strength against raids of `hp` health.
    fn size(self, hp: f32) -> usize {
        match self {
            // A Locust's beam does ~61 a second: the raid dies in ~6 s.
            Kind::Locust => (hp / (61.0 * 6.0)).ceil().clamp(3.0, 8.0) as usize,
            // A drone's 250 napalm blast takes ~400 health off a clumped
            // raid; two hits.
            Kind::Blastwing => (2.0 * (hp / 400.0).ceil()).clamp(3.0, 10.0) as usize,
            // 18 bombs of 40 over 320 elmos, then fire.
            Kind::Phoenix => {
                if hp > 2500.0 {
                    3
                } else {
                    2
                }
            }
            // ~60 a second (and a slow) for twelve shots, but its cannon
            // misses moving raiders: twice what the damage alone needs.
            Kind::Magpie => (hp / 350.0).ceil().clamp(3.0, 6.0) as usize,
        }
    }
}

/// One of our losses to a raid.
struct Loss {
    frame: i32,
    weight: f32,
    /// Health of the raid seen about it.
    raid_hp: f32,
    scythes: usize,
    /// No home defense team within `UNANSWERED_DIST` when it died.
    unanswered: bool,
    /// What it cost, plus a mex's income over the memory.
    metal: f32,
}

/// A group of enemies near our mexes.
pub struct Fire {
    pub pos: Vec3,
    /// Mean velocity, elmos per second.
    pub vel: Vec3,
    /// Enemy ids.
    pub enemies: Vec<i32>,
    pub hp: f32,
    /// Share of its metal that is raiders.
    raiders: f32,
    /// Our mex metal within reach of it.
    at_risk: f32,
}

struct Squad {
    kind: Kind,
    size: usize,
    members: BTreeSet<i32>,
    /// It has been ready once (a squad never built is not spent).
    formed: bool,
    /// The fire it is on (its first enemy's id), if any.
    on: Option<i32>,
}

#[derive(Default)]
pub struct Firefighters {
    losses: Vec<Loss>,
    squad: Option<Squad>,
    /// Firefighters of squads let go: still ours, idle at home.
    reserve: BTreeSet<i32>,
    units: BTreeMap<i32, Unit>,
    ordered: Vec<(Kind, i32)>,
    /// When Blastwings were last sent at each fire (by its first enemy).
    hits: BTreeMap<i32, i32>,
    /// The last order to each unit, and when.
    last: BTreeMap<i32, (Order, i32)>,
    /// Firefighters dead lately, and when (kills by Blastwings come after
    /// they die).
    dead: BTreeMap<i32, i32>,
    /// Where Blastwings went off lately, and when: the engine names no
    /// attacker for what a dead one's blast kills.
    blasts: Vec<(Vec3, i32)>,
    pub fires: Vec<Fire>,
    stats: Stats,
}

#[derive(Default)]
struct Stats {
    squads: u32,
    sorties: u32,
    kills: u32,
    kills_metal: f32,
    lost: u32,
}

fn is_raider_name(name: &str) -> bool {
    matches!(
        name,
        "cloakraid"
            | "cloakheavyraid"
            | "shieldraid"
            | "amphraid"
            | "hoverraid"
            | "vehraid"
            | "vehscout"
            | "tankraid"
            | "tankheavyraid"
            | "jumpraid"
            | "spiderscout"
            | "shieldscout"
            | "gunshipraid"
    )
}

/// An enemy that counts toward a fire: a mobile ground unit, armed.
fn is_ground_fighter(e: &Enemy) -> bool {
    !e.is_dead && !e.is_static && !e.can_fly() && (!e.identified || e.can_attack())
}

impl Ai {
    /// The kind of firefighter wanted, if a factory of `fac` should build
    /// one now.
    pub(crate) fn firefighters_order(&self, fac: FacKind) -> Option<&'static str> {
        let s = self.firefighters.squad.as_ref()?;
        if s.kind.fac() != fac || self.dev_off("firefighters") {
            return None;
        }
        let frame = self.frame;
        let pending =
            (self.firefighters.ordered.iter()).filter(|o| o.0 == s.kind && frame - o.1 <= ORDER_TIMEOUT).count();
        (s.members.len() + pending < s.size).then(|| s.kind.unit())
    }

    /// A factory has been told to build `name`.
    pub fn firefighters_ordered(&mut self, name: &str) {
        if let Some(k) = Kind::of(name) {
            self.firefighters.ordered.push((k, self.frame));
        }
    }

    /// Whether a new unit of type `name` is ours: one we ordered, or one
    /// the squad still wants.
    pub fn firefighters_claims(&mut self, name: &str) -> bool {
        let Some(kind) = Kind::of(name) else { return false };
        let frame = self.frame;
        self.firefighters.ordered.retain(|o| frame - o.1 <= ORDER_TIMEOUT);
        if let Some(k) = self.firefighters.ordered.iter().position(|o| o.0 == kind) {
            self.firefighters.ordered.remove(k);
            return true;
        }
        self.firefighters.squad.as_ref().is_some_and(|s| s.kind == kind && s.members.len() < s.size)
    }

    pub fn firefighters_add(&mut self, unit: Unit) {
        let name = self.defs.name_of(unit);
        match Kind::of(name) {
            // Gunships land when idle (and Locusts heal).
            Some(Kind::Locust | Kind::Blastwing) => unit.set_idle_mode(1, 0),
            _ => unit.set_idle_mode(0, 0),
        };
        unit.set_move_state(0, 0);
        unit.set_fire_state(2, 0);
        self.firefighters.units.insert(unit.id(), unit);
        match &mut self.firefighters.squad {
            Some(s) if Kind::of(name) == Some(s.kind) => {
                s.members.insert(unit.id());
            }
            _ => {
                self.firefighters.reserve.insert(unit.id());
            }
        }
        self.firefighters_go_home(unit);
    }

    /// Give an order unless it was just given.
    fn firefighters_command(&mut self, u: Unit, order: Order) {
        let frame = self.frame;
        if let Some(&(last, at)) = self.firefighters.last.get(&u.id()) {
            let same = match (last, order) {
                (Order::Move(a), Order::Move(b)) | (Order::Fight(a), Order::Fight(b)) => distance(a, b) < REORDER_DIST,
                (a, b) => a == b,
            };
            if same && frame - at < REORDER {
                return;
            }
        }
        match order {
            Order::Move(p) => {
                u.move_to(p, 0);
            }
            Order::Fight(p) => {
                u.fight(p, 0);
            }
            Order::Attack(id) => {
                if let Some(t) = Unit::from_id(self.cb, id) {
                    u.attack(t, 0);
                }
            }
            Order::Pad => {
                u.execute_custom_command(cmd::FIND_PAD, &[], 0, NO_TIMEOUT);
            }
        }
        self.firefighters.last.insert(u.id(), (order, frame));
    }

    pub fn firefighters_remove_unit(&mut self, id: i32) {
        let frame = self.frame;
        let f = &mut self.firefighters;
        if let Some(u) = f.units.remove(&id) {
            f.stats.lost += 1;
            f.dead.insert(id, frame);
            if self.defs.name_of(u) == Kind::Blastwing.unit() {
                f.blasts.push((u.pos(), frame));
            }
        }
        f.blasts.retain(|b| frame - b.1 < 30 * 3);
        f.dead.retain(|_, at| frame - *at < 30 * 10);
        f.reserve.remove(&id);
        f.last.remove(&id);
        if let Some(s) = &mut f.squad {
            s.members.remove(&id);
        }
    }

    /// One of our units died: remember it if raiders killed economy.
    pub fn firefighters_unit_destroyed(&mut self, unit: Unit, attacker: Option<Unit>) {
        self.firefighters_remove_unit(unit.id());
        let Some(def) = unit.def() else { return };
        let name = self.defs.name(def);
        let weight = if name == "staticmex" {
            1.0
        } else if name.starts_with("energy") {
            0.5
        } else {
            return;
        };
        let pos = unit.pos();
        let frame = self.frame;
        let near: Vec<&Enemy> = (self.war.targets.values())
            .filter(|e| is_ground_fighter(e) && frame - e.last_seen <= RAID_SEEN)
            .filter(|e| distance(e.position, pos) < RAID_RADIUS)
            .collect();
        let by_raiders = match attacker.and_then(|a| a.def()) {
            Some(d) => is_raider_name(self.defs.name(d)),
            None => near.iter().any(|e| is_raider_name(&e.def_name) || !e.identified),
        };
        if !by_raiders {
            return;
        }
        let scythes = near.iter().filter(|e| e.def_name == "cloakheavyraid").count()
            + usize::from(attacker.and_then(|a| a.def()).is_some_and(|d| self.defs.name(d) == "cloakheavyraid"));
        let mut raid_hp: f32 = near.iter().map(|e| if e.identified { e.def_health() } else { 300.0 }).sum();
        if raid_hp <= 0.0 {
            raid_hp = attacker.and_then(|a| a.def()).map_or(DEFAULT_RAID_HP, |d| d.health());
        }
        let weight = weight * if scythes > 0 { 2.0 } else { 1.0 };
        let nearest =
            (self.qrf_teams().values()).map(|ids| distance(self.qrf_team_pos(ids).0, pos)).min_by(f32::total_cmp);
        let unanswered = nearest.is_none_or(|d| d > UNANSWERED_DIST);
        let mut metal = def.cost(self.metal);
        if name == "staticmex" {
            let income = self.graph.get_closest_spot(pos).map_or(0.0, |s| s.value);
            metal += income * MEMORY as f32 / 30.0;
        }
        self.firefighters.losses.push(Loss { frame, weight, raid_hp, scythes, unanswered, metal });
    }

    /// Of the losses remembered, those no home defense team was near: their
    /// weight and their metal (see `UNANSWERED_DIST`).
    pub(crate) fn firefighters_unanswered(&self) -> (f32, f32) {
        let ls = self.firefighters.losses.iter().filter(|l| l.unanswered);
        ls.fold((0.0, 0.0), |(w, m), l| (w + l.weight, m + l.metal))
    }

    pub fn firefighters_enemy_destroyed(&mut self, enemy: Unit, attacker: Option<Unit>) {
        let ours = |id: i32| self.firefighters.units.contains_key(&id) || self.firefighters.dead.contains_key(&id);
        let p = enemy.pos();
        let blasted = attacker.is_none() && self.firefighters.blasts.iter().any(|b| distance(b.0, p) < 320.0);
        if attacker.is_some_and(|a| ours(a.id())) || blasted {
            self.firefighters.stats.kills += 1;
            self.firefighters.stats.kills_metal += enemy.def().map_or(0.0, |d| d.cost(self.metal));
        }
        self.firefighters.hits.remove(&enemy.id());
    }

    /// Loss weight remembered, and the raids' typical health.
    pub(crate) fn firefighters_pressure(&self) -> (f32, f32, usize) {
        let ls = &self.firefighters.losses;
        let weight = ls.iter().map(|l| l.weight).sum();
        let mut hp: Vec<f32> = ls.iter().map(|l| l.raid_hp).collect();
        hp.sort_by(f32::total_cmp);
        let typical = if hp.is_empty() { DEFAULT_RAID_HP } else { hp[((hp.len() - 1) as f32 * 0.75).round() as usize] };
        (weight, typical, ls.iter().map(|l| l.scythes).sum())
    }

    pub fn firefighters_update(&mut self, frame: i32) {
        if (frame + self.offset + 7) % PERIOD != 0 || self.dev_off("firefighters") {
            return;
        }
        self.firefighters.losses.retain(|l| frame - l.frame <= MEMORY);
        self.firefighters_muster();
        if self.firefighters.units.is_empty() && self.firefighters.squad.is_none() {
            return;
        }
        self.firefighters_find_fires(frame);
        self.firefighters_fight(frame);
        if frame % 1800 < PERIOD {
            self.firefighters_log();
        }
    }

    /// Pick a squad when one is wanted; let go of a spent one.
    fn firefighters_muster(&mut self) {
        let (weight, typical, _) = self.firefighters_pressure();
        let frame = self.frame;
        if let Some(s) = &self.firefighters.squad {
            let pending = self.firefighters.ordered.iter().any(|o| o.0 == s.kind && frame - o.1 <= ORDER_TIMEOUT);
            // Blastwings are spent hitting: they are only ever refilled.
            let spent = s.formed
                && s.kind != Kind::Blastwing
                && (s.members.len() as f32) < SPENT_SHARE * s.size as f32
                && !pending;
            // Spent, and wanted again: the survivors go to the reserve.
            if spent && weight >= WANTED_AT {
                let s = self.firefighters.squad.take().expect("squad");
                self.firefighters.reserve.extend(s.members);
                self.log(&format!("firefighters: the {} squad is spent", s.kind.label()));
            } else {
                // Grow with the raids.
                let size = s.kind.size(typical);
                if let Some(s) = &mut self.firefighters.squad {
                    s.size = s.size.max(size);
                }
                return;
            }
        }
        if weight < WANTED_AT {
            return;
        }
        let facs: Vec<FacKind> = self.fac.factories.values().map(|f| f.kind()).collect();
        let forced = self.cb.option("firefighters").and_then(|k| Kind::by_label(&k));
        let can: Vec<Kind> = KINDS.iter().copied().filter(|k| facs.contains(&k.fac())).collect();
        let kind = match forced {
            Some(k) if can.contains(&k) => k,
            Some(_) => return,
            None if can.is_empty() => return,
            None => can[((kgb_rand() * can.len() as f64) as usize).min(can.len() - 1)],
        };
        let size = kind.size(typical);
        // Firefighters of the kind in the reserve join at once.
        let mut members = BTreeSet::new();
        let reserve: Vec<i32> = self.firefighters.reserve.iter().copied().collect();
        for id in reserve {
            if members.len() < size
                && self.firefighters.units.get(&id).is_some_and(|u| Kind::of(self.defs.name_of(*u)) == Some(kind))
            {
                self.firefighters.reserve.remove(&id);
                members.insert(id);
            }
        }
        self.firefighters.stats.squads += 1;
        self.log(&format!(
            "firefighters: a {} squad of {size} (losses {weight:.1}, raids of {typical:.0} hp)",
            kind.label()
        ));
        self.firefighters.squad = Some(Squad { kind, size, members, formed: false, on: None });
    }

    /// Groups of enemy ground units near our mexes.
    fn firefighters_find_fires(&mut self, frame: i32) {
        let mexes: Vec<Vec3> =
            (self.graph.metal_spots.iter()).filter(|m| m.extractor.is_some() && m.owned).map(|m| m.position).collect();
        let es: Vec<(&i32, &Enemy)> = (self.war.targets.iter())
            .filter(|(_, e)| is_ground_fighter(e) && frame - e.last_seen <= FIRE_SEEN)
            .filter(|(_, e)| mexes.iter().any(|&m| distance(m, e.position) < FIRE_REACH + FIRE_LINK))
            .collect();
        let mut label: Vec<usize> = (0..es.len()).collect();
        fn root(l: &mut [usize], mut i: usize) -> usize {
            while l[i] != i {
                l[i] = l[l[i]];
                i = l[i];
            }
            i
        }
        for i in 0..es.len() {
            for j in i + 1..es.len() {
                if distance(es[i].1.position, es[j].1.position) < FIRE_LINK {
                    let (a, b) = (root(&mut label, i), root(&mut label, j));
                    label[a] = b;
                }
            }
        }
        let mut groups: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
        for i in 0..es.len() {
            let r = root(&mut label, i);
            groups.entry(r).or_default().push(i);
        }
        let metal = self.metal;
        let mut fires = Vec::new();
        for idx in groups.into_values() {
            let n = idx.len() as f32;
            let (mut pos, mut vel, mut hp, mut raid_metal, mut all_metal) = (Vec3::ZERO, Vec3::ZERO, 0.0, 0.0, 0.0);
            for &i in &idx {
                let e = es[i].1;
                pos += e.position * (1.0 / n);
                let v = e.unit.vel();
                vel += Vec3::new(v.x * 30.0, 0.0, v.z * 30.0) * (1.0 / n);
                hp += if e.identified { e.def_health() } else { 300.0 };
                let c = e.ud.map_or(100.0, |d| d.cost(metal));
                all_metal += c;
                if !e.identified || is_raider_name(&e.def_name) {
                    raid_metal += c;
                }
            }
            let near = mexes.iter().filter(|&&m| distance(m, pos) < FIRE_REACH).count();
            if near == 0 {
                continue;
            }
            fires.push(Fire {
                pos,
                vel,
                enemies: idx.iter().map(|&i| *es[i].0).collect(),
                hp,
                raiders: raid_metal / all_metal.max(1.0),
                at_risk: near as f32 * 85.0,
            });
        }
        self.firefighters.fires = fires;
    }

    /// Whether a fire is ours to fight: raiders, with no anti-air known
    /// about.
    fn firefighters_isolated(&self, f: &Fire) -> bool {
        if f.raiders < 0.6 || self.war.get_aa_threat(f.pos) > 0.0 {
            return false;
        }
        !(self.war.targets.values())
            .any(|e| !e.is_dead && (e.is_aa || e.is_flex_aa) && distance(e.position, f.pos) < AA_RADIUS)
    }

    fn firefighters_fight(&mut self, frame: i32) {
        // Spent squads' survivors stay home.
        let reserve: Vec<Unit> =
            (self.firefighters.reserve.iter()).filter_map(|id| self.firefighters.units.get(id).copied()).collect();
        for u in reserve {
            self.firefighters_go_home(u);
        }
        let Some(s) = &self.firefighters.squad else { return };
        let (kind, size) = (s.kind, s.size);
        let members: Vec<Unit> = (s.members.iter()).filter_map(|id| self.firefighters.units.get(id).copied()).collect();
        let ready = members.len() as f32 >= (READY_SHARE * size as f32).ceil().max(1.0);
        if ready && let Some(s) = &mut self.firefighters.squad {
            s.formed = true;
        }
        // The fire most worth it, by our metal at risk over the way there.
        let home = self.graph.get_ally_center();
        let fire = (self.firefighters.fires.iter())
            .filter(|f| self.firefighters_isolated(f))
            .max_by(|a, b| {
                let va = a.at_risk / (1.0 + distance(home, a.pos) / 2000.0);
                let vb = b.at_risk / (1.0 + distance(home, b.pos) / 2000.0);
                va.total_cmp(&vb)
            })
            .map(|f| (f.pos, f.vel, f.enemies.clone(), f.hp));
        let on = fire.as_ref().filter(|_| ready).map(|f| f.2[0]);
        if let Some(s) = &mut self.firefighters.squad {
            if on.is_some() && s.on != on {
                self.firefighters.stats.sorties += 1;
            }
            s.on = on;
        }
        let Some((pos, vel, enemies, hp)) = fire.filter(|_| ready) else {
            for u in members {
                self.firefighters_go_home(u);
            }
            return;
        };
        match kind {
            Kind::Locust => {
                let aim = pos + vel * 1.5;
                for u in members {
                    let max = u.max_health().max(1.0);
                    let noammo = u.rules_param_float("noammo", 0.0) > 0.0;
                    if u.health() / max < HURT || noammo {
                        self.firefighters_go_home(u);
                    } else {
                        self.firefighters_command(u, Order::Fight(aim));
                    }
                }
            }
            // Bombers and strafers each take one raider at a time.
            Kind::Phoenix | Kind::Magpie => {
                let target = (enemies.iter())
                    .filter_map(|id| self.war.targets.get(id))
                    .min_by(|a, b| distance(a.position, pos).total_cmp(&distance(b.position, pos)))
                    .map(|e| e.unit.id());
                for u in members {
                    let max = u.max_health().max(1.0);
                    let noammo = u.rules_param_float("noammo", 0.0) > 0.0;
                    if noammo || u.health() / max < HURT {
                        self.firefighters_go_home(u);
                    } else if let Some(t) = target {
                        self.firefighters_command(u, Order::Attack(t));
                    }
                }
            }
            Kind::Blastwing => {
                let key = enemies[0];
                if self.firefighters.hits.get(&key).is_some_and(|&f| frame - f < HIT_INTERVAL) {
                    return;
                }
                let k = ((hp / 350.0).ceil() as usize).max(2).min(members.len());
                let mut idle: Vec<Unit> = members;
                idle.sort_by(|a, b| distance(a.pos(), pos).total_cmp(&distance(b.pos(), pos)));
                let targets: Vec<i32> =
                    (enemies.iter()).filter(|id| self.war.targets.contains_key(id)).copied().collect();
                if targets.is_empty() {
                    return;
                }
                for (i, u) in idle.into_iter().take(k).enumerate() {
                    self.firefighters_command(u, Order::Attack(targets[i % targets.len()]));
                }
                self.firefighters.hits.insert(key, frame);
            }
        }
    }

    fn firefighters_go_home(&mut self, u: Unit) {
        let haven = self.graph_get_closest_air_haven(u.pos());
        let max = u.max_health().max(1.0);
        let needs_pad = u.rules_param_float("noammo", 0.0) > 0.0 || u.health() < max;
        if matches!(Kind::of(self.defs.name_of(u)), Some(Kind::Phoenix | Kind::Magpie)) && needs_pad {
            self.firefighters_command(u, Order::Pad);
        } else if distance(u.pos(), haven) > 400.0 {
            self.firefighters_command(u, Order::Move(haven));
        }
    }

    fn firefighters_log(&mut self) {
        let (weight, typical, scythes) = self.firefighters_pressure();
        let f = &self.firefighters;
        let squad = f.squad.as_ref().map_or("none".to_owned(), |s| {
            format!(
                "{} {}/{}{}",
                s.kind.label(),
                s.members.len(),
                s.size,
                if s.on.is_some() { " on a fire" } else { "" }
            )
        });
        let line = format!(
            "firefighters: losses {weight:.1} (scythes {scythes}) raids {typical:.0} hp; squad {squad}; reserve {}; fires {}; squads {} sorties {} kills {} ({:.0} metal) lost {}",
            f.reserve.len(),
            f.fires.len(),
            f.stats.squads,
            f.stats.sorties,
            f.stats.kills,
            f.stats.kills_metal,
            f.stats.lost,
        );
        self.log(&line);
    }
}
