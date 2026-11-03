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
//! mexes. Firefighters answer only isolated fires: no anti-air known about
//! it, no stronger army with it. They never go after anything else. Squads
//! below strength are refilled by the factory; one that has lost most of
//! itself is let go (its survivors stay firefighters, in the reserve) and a
//! new kind is picked if the losses go on.
//!
//! Each fire gets what its health calls for (`Kind::need`: one Magpie for a
//! Flea, not the squad), of the kind that puts the least metal in the air
//! among the squad and the reserve; the rest stay home for the next fire. A
//! kind already on a fire keeps it while it is enough. Health a bomber
//! sortie is already out against (see `airstrikes`) is not counted, and the
//! bomber planner in turn leaves alone the raiders firefighters are on.

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

/// Raiders within this far of a loss, seen within this many frames, were
/// the raid.
const RAID_RADIUS: f32 = 600.0;
const RAID_SEEN: i32 = 30 * 10;

/// A raid's health if none was seen.
const DEFAULT_RAID_HP: f32 = 700.0;

/// An unidentified raider's health, with no guess at its type.
const UNKNOWN_HP: f32 = 300.0;

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

    /// Health of raiders one firefighter takes off in an answer.
    fn answers(self) -> f32 {
        match self {
            // Six seconds of a beam that does not miss.
            Kind::Locust => 61.0 * 6.0,
            // A blast on a clump; less on a lone raider (see `ONE_BLAST`).
            Kind::Blastwing => 350.0,
            Kind::Phoenix => 1000.0,
            Kind::Magpie => MAGPIE_ANSWERS,
        }
    }

    /// Firefighters of this kind a fire of `hp` health needs. Blastwings go
    /// two at a time (a drone that misses is gone, and one sent is spent),
    /// except at a fire one blast is sure to kill.
    fn need(self, hp: f32) -> usize {
        let n = (hp / self.answers()).ceil().max(1.0) as usize;
        match self {
            Kind::Blastwing if hp > ONE_BLAST => n.max(2),
            _ => n,
        }
    }
}

/// What one Blastwing is sure to kill: half its 250 blast, as a drone goes
/// off up to 60 elmos from its target and the blast falls off to nothing
/// over its 128 radius (a Flea or a Dart, not a Glaive).
const ONE_BLAST: f32 = 125.0;

/// Raider health one Magpie answers: its 60 a second spray misses a fast
/// raider often enough that a Glaive takes most of a load (twelve salvos of
/// 48), so a Glaive gets two and a Flea one. (Its squad is sized as before,
/// in `size`.)
pub(crate) const MAGPIE_ANSWERS: f32 = 200.0;

/// Firefighters sent at a fire: its middle and drift, the enemies counted
/// in it and their health.
struct Answer {
    pos: Vec3,
    vel: Vec3,
    enemies: Vec<i32>,
    hp: f32,
    kind: Kind,
    units: Vec<Unit>,
}

/// What answers a fire of `hp` health, and how many: of the kinds with
/// firefighters free (`free`), one with enough for the fire, or the squad's
/// kind (`ready`) with its ready share at a fire bigger than that; a kind
/// already on the fire (`on`) keeps it, else the least metal in the air.
fn pick(
    hp: f32,
    free: &[(Kind, usize)],
    cost: impl Fn(Kind) -> f32,
    ready: Option<(Kind, usize)>,
    on: Option<Kind>,
) -> Option<(Kind, usize)> {
    let options: Vec<(Kind, usize)> = (free.iter())
        .filter_map(|&(k, n)| {
            let need = k.need(hp);
            if n >= need { Some((k, need)) } else { ready.filter(|&(rk, share)| rk == k && n >= share).map(|_| (k, n)) }
        })
        .collect();
    if let Some(&o) = options.iter().find(|o| Some(o.0) == on) {
        return Some(o);
    }
    options.into_iter().min_by(|a, b| (a.1 as f32 * cost(a.0)).total_cmp(&(b.1 as f32 * cost(b.0))))
}

/// One of our losses to a raid.
struct Loss {
    frame: i32,
    weight: f32,
    /// Health of the raid seen about it.
    raid_hp: f32,
    scythes: usize,
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
    /// The enemy each firefighter out on a fire was sent at.
    on: BTreeMap<i32, i32>,
    /// Enemies of the fires answered, and those fires (by first enemy).
    engaged: BTreeSet<i32>,
    answered: BTreeSet<i32>,
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
        f.on.remove(&id);
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
        self.firefighters.losses.push(Loss { frame, weight, raid_hp, scythes });
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
        self.firefighters.engaged.remove(&enemy.id());
    }

    /// An enemy's health in an answer: as seen; else its type's as the QRF
    /// guesses it from its top speed (see `qrf`); else a raider's. (A Flea
    /// and a Dagger both go 144, and the guess is the Dagger: a Flea on
    /// radar counts 300 until it is in sight, when the spare goes home.)
    fn firefighters_hp(&self, e: &Enemy) -> f32 {
        if e.identified {
            return e.get_health() * e.def_health();
        }
        (self.qrf.tracks.get(&e.unit_id))
            .filter(|t| self.qrf.on && t.health > 0.0 && self.frame - t.last_seen <= FIRE_SEEN)
            .map_or(UNKNOWN_HP, |t| t.health)
    }

    /// Whether firefighters are out against this enemy.
    /// Firefighters of this unit type not out on a fire.
    pub(crate) fn firefighters_idle(&self, name: &str) -> usize {
        let Some(kind) = Kind::of(name) else { return 0 };
        let f = &self.firefighters;
        f.units
            .iter()
            .filter(|(id, u)| Kind::of(self.defs.name_of(**u)) == Some(kind) && !f.on.contains_key(*id))
            .count()
    }

    pub(crate) fn firefighters_engaged(&self, enemy: i32) -> bool {
        self.firefighters.engaged.contains(&enemy) && !self.dev_off("firefighters")
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
            self.firefighters.engaged.clear();
            self.firefighters.answered.clear();
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
        self.firefighters.squad = Some(Squad { kind, size, members, formed: false });
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
        let squad = self.firefighters.squad.as_ref().map(|s| (s.kind, s.size, s.members.clone()));
        // The squad's ready share: the least of it that goes out at a fire
        // bigger than it.
        let ready = squad.as_ref().map(|(k, size, _)| (*k, (READY_SHARE * *size as f32).ceil().max(1.0) as usize));
        if let (Some((_, share)), Some((_, _, members))) = (ready, &squad)
            && members.iter().filter(|id| self.firefighters.units.contains_key(id)).count() >= share
            && let Some(s) = &mut self.firefighters.squad
        {
            s.formed = true;
        }
        // The squad and the reserve, and those of them fit to go out.
        let ids = (squad.iter().flat_map(|s| s.2.iter())).chain(self.firefighters.reserve.iter());
        let pool: Vec<(Unit, Kind)> = (ids.filter_map(|id| self.firefighters.units.get(id).copied()))
            .filter_map(|u| Kind::of(self.defs.name_of(u)).map(|k| (u, k)))
            .collect();
        let fit = |u: &Unit| u.health() / u.max_health().max(1.0) >= HURT && u.rules_param_float("noammo", 0.0) <= 0.0;
        let mut free: Vec<(Unit, Kind)> = pool.iter().copied().filter(|(u, _)| fit(u)).collect();
        let cost = |k: Kind| self.defs.get(k.unit()).map_or(f32::INFINITY, |d| d.cost(self.metal));
        // The fires most worth it first, by our metal at risk over the way.
        let home = self.graph.get_ally_center();
        let mut fires: Vec<(f32, Vec3, Vec3, Vec<i32>)> = (self.firefighters.fires.iter())
            .filter(|f| self.firefighters_isolated(f))
            .map(|f| (f.at_risk / (1.0 + distance(home, f.pos) / 2000.0), f.pos, f.vel, f.enemies.clone()))
            .collect();
        fires.sort_by(|a, b| b.0.total_cmp(&a.0));
        let mut plan: Vec<Answer> = Vec::new();
        for (_, pos, vel, enemies, ..) in fires {
            // What a sortie is already out against is not ours to count.
            let live: Vec<i32> = (enemies.iter().copied())
                .filter(|id| self.war.targets.contains_key(id) && !self.airstrike_engaged(*id))
                .collect();
            let hp: f32 =
                (live.iter().filter_map(|id| self.war.targets.get(id))).map(|e| self.firefighters_hp(e)).sum();
            if live.is_empty() || hp <= 0.0 {
                continue;
            }
            let on_it = |u: &Unit| self.firefighters.on.get(&u.id()).is_some_and(|t| enemies.contains(t));
            let counts: Vec<(Kind, usize)> =
                (KINDS.iter()).map(|&k| (k, free.iter().filter(|f| f.1 == k).count())).filter(|c| c.1 > 0).collect();
            let on = free.iter().find(|(u, _)| on_it(u)).map(|f| f.1);
            let Some((kind, n)) = pick(hp, &counts, cost, ready, on) else { continue };
            // Those already on it first, then the nearest.
            let (mut mine, rest): (Vec<_>, Vec<_>) = free.into_iter().partition(|f| f.1 == kind);
            mine.sort_by(|a, b| {
                (!on_it(&a.0)).cmp(&!on_it(&b.0)).then(distance(a.0.pos(), pos).total_cmp(&distance(b.0.pos(), pos)))
            });
            let kept = mine.split_off(n.min(mine.len()));
            free = rest.into_iter().chain(kept).collect();
            plan.push(Answer { pos, vel, enemies: live, hp, kind, units: mine.into_iter().map(|f| f.0).collect() });
        }
        let mut busy = BTreeSet::new();
        let mut engaged = BTreeSet::new();
        let mut answered = BTreeSet::new();
        for Answer { pos, vel, enemies, hp, kind, units } in plan {
            busy.extend(units.iter().map(|u| u.id()));
            engaged.extend(enemies.iter().copied());
            answered.insert(enemies[0]);
            if !self.firefighters.answered.contains(&enemies[0]) {
                self.log(&format!(
                    "firefighters: {} {} at a fire of {} ({hp:.0} hp)",
                    units.len(),
                    kind.label(),
                    enemies.len()
                ));
            }
            match kind {
                Kind::Locust => {
                    let aim = pos + vel * 1.5;
                    for u in units {
                        self.firefighters_command(u, Order::Fight(aim));
                        self.firefighters.on.insert(u.id(), enemies[0]);
                    }
                }
                // A carpet takes the clump about the raider nearest its middle.
                Kind::Phoenix => {
                    let target = (enemies.iter())
                        .filter_map(|id| self.war.targets.get(id))
                        .min_by(|a, b| distance(a.position, pos).total_cmp(&distance(b.position, pos)))
                        .map(|e| e.unit_id);
                    let Some(t) = target else { continue };
                    for u in units {
                        self.firefighters_command(u, Order::Attack(t));
                        self.firefighters.on.insert(u.id(), t);
                    }
                }
                // Strafers spread over the raiders, each as much health as
                // it takes off; one keeps its raider while it lives.
                Kind::Magpie => {
                    let mut cover: Vec<(i32, Vec3, f32, f32)> = (enemies.iter())
                        .filter_map(|id| self.war.targets.get(id))
                        .map(|e| (e.unit_id, e.position, self.firefighters_hp(e).max(1.0), 0.0))
                        .collect();
                    let mut rest = Vec::new();
                    for u in units {
                        let keep = self.firefighters.on.get(&u.id()).and_then(|t| cover.iter().position(|c| c.0 == *t));
                        match keep {
                            Some(i) => {
                                cover[i].3 += Kind::Magpie.answers();
                                self.firefighters_command(u, Order::Attack(cover[i].0));
                            }
                            None => rest.push(u),
                        }
                    }
                    for u in rest {
                        let p = u.pos();
                        let Some(c) = cover.iter_mut().min_by(|a, b| {
                            (a.3 / a.2).total_cmp(&(b.3 / b.2)).then(distance(a.1, p).total_cmp(&distance(b.1, p)))
                        }) else {
                            break;
                        };
                        c.3 += Kind::Magpie.answers();
                        let t = c.0;
                        self.firefighters_command(u, Order::Attack(t));
                        self.firefighters.on.insert(u.id(), t);
                    }
                }
                // Drones go a few at a time; those sent are on their way
                // until the next may follow, the rest wait at home.
                Kind::Blastwing => {
                    let key = enemies[0];
                    if self.firefighters.hits.get(&key).is_some_and(|&f| frame - f < HIT_INTERVAL) {
                        continue;
                    }
                    for (i, u) in units.into_iter().enumerate() {
                        let t = enemies[i % enemies.len()];
                        self.firefighters_command(u, Order::Attack(t));
                        self.firefighters.on.insert(u.id(), t);
                    }
                    self.firefighters.hits.insert(key, frame);
                }
            }
        }
        // Everyone else goes (or stays) home.
        for (u, _) in pool {
            if !busy.contains(&u.id()) {
                self.firefighters.on.remove(&u.id());
                self.firefighters_go_home(u);
            }
        }
        self.firefighters.stats.sorties += answered.difference(&self.firefighters.answered).count() as u32;
        self.firefighters.answered = answered;
        self.firefighters.engaged = engaged;
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
        let squad = f
            .squad
            .as_ref()
            .map_or("none".to_owned(), |s| format!("{} {}/{}", s.kind.label(), s.members.len(), s.size));
        let line = format!(
            "firefighters: losses {weight:.1} (scythes {scythes}) raids {typical:.0} hp; squad {squad}; reserve {}; fires {} (on {}, with {}); squads {} sorties {} kills {} ({:.0} metal) lost {}",
            f.reserve.len(),
            f.fires.len(),
            f.answered.len(),
            f.on.len(),
            f.stats.squads,
            f.stats.sorties,
            f.stats.kills,
            f.stats.kills_metal,
            f.stats.lost,
        );
        self.log(&line);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cost(k: Kind) -> f32 {
        match k {
            Kind::Locust => 220.0,
            Kind::Blastwing => 55.0,
            Kind::Phoenix => 520.0,
            Kind::Magpie => 250.0,
        }
    }

    const FLEA: f32 = 40.0;

    #[test]
    fn a_flea_takes_one_magpie_not_the_squad() {
        let ready = Some((Kind::Magpie, 4));
        assert_eq!(pick(FLEA, &[(Kind::Magpie, 6)], cost, ready, None), Some((Kind::Magpie, 1)));
    }

    #[test]
    fn a_ready_magpie_answers_a_flea_before_a_phoenix() {
        let free = [(Kind::Phoenix, 2), (Kind::Magpie, 1)];
        assert_eq!(pick(FLEA, &free, cost, Some((Kind::Phoenix, 2)), None), Some((Kind::Magpie, 1)));
        // With no Magpie, one Phoenix, not two.
        assert_eq!(pick(FLEA, &free[..1], cost, Some((Kind::Phoenix, 2)), None), Some((Kind::Phoenix, 1)));
    }

    #[test]
    fn a_kind_on_a_fire_keeps_it_while_it_is_enough() {
        let free = [(Kind::Phoenix, 1), (Kind::Magpie, 2)];
        assert_eq!(pick(FLEA, &free, cost, None, Some(Kind::Phoenix)), Some((Kind::Phoenix, 1)));
        // Not when it is no longer enough.
        let free = [(Kind::Phoenix, 1), (Kind::Magpie, 8)];
        assert_eq!(pick(1500.0, &free, cost, None, Some(Kind::Phoenix)), Some((Kind::Magpie, 8)));
    }

    #[test]
    fn heavier_raids_take_more_and_the_cheaper_kind() {
        let free = [(Kind::Phoenix, 3), (Kind::Magpie, 6)];
        // A Glaive: two Magpies (500 metal) over a Phoenix (520).
        assert_eq!(pick(230.0, &free, cost, None, None), Some((Kind::Magpie, 2)));
        // A Glaive pair: three Magpies, as the squad of three always sent.
        assert_eq!(pick(460.0, &free[1..], cost, None, None), Some((Kind::Magpie, 3)));
        // Five Glaives: two Phoenixes (1040) over six Magpies (1500).
        assert_eq!(pick(1150.0, &free, cost, None, None), Some((Kind::Phoenix, 2)));
        // A Scythe: one Phoenix over five Magpies.
        assert_eq!(pick(820.0, &free, cost, None, None), Some((Kind::Phoenix, 1)));
    }

    #[test]
    fn a_fire_bigger_than_the_squad_takes_its_ready_share_or_more() {
        let ready = Some((Kind::Magpie, 4));
        assert_eq!(pick(3000.0, &[(Kind::Magpie, 5)], cost, ready, None), Some((Kind::Magpie, 5)));
        assert_eq!(pick(3000.0, &[(Kind::Magpie, 3)], cost, ready, None), None);
        // Reserve kinds go only with enough.
        assert_eq!(pick(3000.0, &[(Kind::Locust, 3)], cost, ready, None), None);
    }

    #[test]
    fn blastwings_go_in_pairs_except_at_what_one_blast_kills() {
        assert_eq!(Kind::Blastwing.need(FLEA), 1);
        assert_eq!(Kind::Blastwing.need(120.0), 1);
        assert_eq!(Kind::Blastwing.need(230.0), 2);
        assert_eq!(Kind::Blastwing.need(460.0), 2);
        assert_eq!(Kind::Blastwing.need(1400.0), 4);
        // A Locust's beam (61 a second, it does not miss) kills a Glaive in
        // four seconds: one each, two for a pair.
        assert_eq!(Kind::Locust.need(FLEA), 1);
        assert_eq!(Kind::Locust.need(230.0), 1);
        assert_eq!(Kind::Locust.need(460.0), 2);
        assert_eq!(Kind::Phoenix.need(2500.0), 3);
    }
}
