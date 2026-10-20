//! Precision missions: a raider (a solo raider or a Kodachi) that finds
//! itself near an enemy building that defenses cover goes for it, from a
//! spot they can fire at little or not at all (see `military::precision`).
//! Missions are taken only in passing — short, near, and worth the raider's
//! time: sent across the map for them, raiders spent minutes per kill away
//! from the fighting, and lost games (self-play mirrors, tank 3-13). It walks to where the way in starts, takes
//! the planned way in, parks at the spot with the building set as its target
//! (ZK's set-target: it shoots it without chasing it), and leaves the way it
//! came once the building is dead, it has taken what the building was worth,
//! or enemies close in. The damage it may take is a budget: more for a
//! building worth more than the raider. A cloaked raider (a Scythe) holds its
//! fire, and so its cloak, until it is at its spot. A defense that hits it
//! at a spot planned out of all fire is distrusted from then on.

use springai::{NO_TIMEOUT, Unit, Vec3, WeaponDef};

use crate::Ai;
use crate::defs::cmd;
use crate::kgbutil::kgb_move_to;
use crate::military::precision::Approach;
use crate::util::{OPTION_SHIFT_KEY, distance};

/// Raiders on precision missions at once.
const MAX_MISSIONS: usize = 4;
/// How often an idle raider looks for a mission.
const PLAN_FRAMES: i32 = 30 * 4;
/// Buildings farther than this from the raider are not considered...
const MAX_TARGET_DISTANCE: f32 = 900.0;
/// ...nor ways in that start farther than this from it...
const MAX_ENTRY_DISTANCE: f32 = 400.0;
/// ...nor missions that take longer than this (seconds) from where it is
/// to the kill...
const MAX_MISSION_SECONDS: f32 = 25.0;
/// ...or kill less than the raider's own worth per this many seconds.
const WORTH_SECONDS: f32 = 30.0;
/// Buildings considered per look, nearest first.
const TARGETS_PER_PLAN: usize = 4;
/// Spots tried per building.
const SPOTS_PER_TARGET: usize = 3;
/// The damage a mission may cost, as a share of the raider's health: this
/// for a building worth as much as the raider, scaling with the square root
/// of the worth ratio, within bounds.
const BUDGET_SHARE: f32 = 0.25;
const MIN_BUDGET_SHARE: f32 = 0.1;
const MAX_BUDGET_SHARE: f32 = 0.6;
/// A kill may take this long (seconds), up to three times more for a
/// building worth more than the raider.
const MAX_KILL_TIME: f32 = 40.0;
/// Seconds of a mission that a raider's whole health is worth, in planning.
const DAMAGE_SECONDS: f32 = 60.0;
/// The engine's fire states.
const HOLD_FIRE: i32 = 0;
const FIRE_AT_WILL: i32 = 2;
/// Enemy fighters this close to the spot (beyond their range) call it off,
/// if they are dangerous enough (this share of the raider's own danger, in
/// `Enemy::get_danger` terms) for long enough (frames).
const MOBILE_CLEARANCE: f32 = 250.0;
const DANGER_SHARE: f32 = 0.5;
const DANGER_FRAMES: i32 = 60;
/// After giving up, a raider waits this long before it plans again, and
/// leaves that target alone longer.
const GIVE_UP_FRAMES: i32 = 30 * 20;
const TARGET_BAN_FRAMES: i32 = 30 * 60;
/// The raider counts as there within this distance (no more than the
/// spot's hidden surroundings, `HIDE_JITTER`).
const ARRIVED: f32 = 20.0;
/// Spots given up on stay off the plan this long, and this far around.
const BAD_SPOT_FRAMES: i32 = 30 * 180;
const BAD_SPOT_RADIUS: f32 = 48.0;
/// The engine's move state that holds position.
const HOLD_POSITION: i32 = 0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    /// On its own to the entry.
    Travel,
    /// Along the planned way to the spot.
    Approach,
    /// At the spot, shooting.
    Fire,
    /// Back along the way in.
    Leave,
}

pub struct Mission {
    pub target: i32,
    pub target_pos: Vec3,
    /// The target's aim point (absolute height) and half its footprint.
    pub target_aim: f32,
    pub target_half: f32,
    /// Where the raider was at the last progress check, and when.
    pub last_pos: Vec3,
    pub moved: i32,
    pub spot: Vec3,
    pub approach: Approach,
    pub phase: Phase,
    /// Frame the phase began.
    pub since: i32,
    /// Expected seconds to kill the target from the spot.
    pub kill_time: f32,
    /// Health when the way in began.
    pub health: f32,
    /// Last time the raider was sent somewhere in this phase.
    pub ordered: i32,
    /// The target's health when last checked while firing, when, and
    /// whether it had gone down since the check before.
    pub target_health: f32,
    pub checked: i32,
    pub falling: bool,
    /// Whether it was hit on the way in yet.
    pub hurt: bool,
    /// The damage it may take, and the damage per second planned at the
    /// spot.
    pub budget: f32,
    pub dwell_dps: f32,
    /// Cloaked (it holds fire until at the spot).
    pub cloaked: bool,
    /// Enemy danger near the spot it can live with, and since when there
    /// has been more (0: not now).
    pub danger_limit: f32,
    pub danger_since: i32,
}

#[derive(Default, Debug)]
pub struct MissionStats {
    pub started: u32,
    pub kills: u32,
    pub fired: u32,
    pub aborted: u32,
    pub lost: u32,
    pub hit_at_spot: u32,
}

impl Ai {
    /// Drive raider `id` if it is on a precision mission, or start one if
    /// one is worth it; `false` leaves it to the usual solo raider logic.
    pub fn precision_step(&mut self, id: i32) -> bool {
        if self.dev_off("precision") {
            return false;
        }
        if self.precision.missions.contains_key(&id) {
            return self.precision_drive(id);
        }
        let frame = self.frame;
        if self.precision.missions.len() >= MAX_MISSIONS
            || self.precision.planned.get(&id).is_some_and(|&f| frame - f < PLAN_FRAMES)
        {
            return false;
        }
        self.precision.planned.insert(id, frame);
        let Some(m) = self.precision_plan(id) else { return false };
        self.log(&format!(
            "precision: raider {id} ({}) goes for {} at ({:.0}, {:.0}) from ({:.0}, {:.0}): way in {:.0} s, {:.0} damage expected, kill in {:.0} s under {:.0} dps, budget {:.0}{}",
            self.fighters.get(&id).map_or("?", |f| self.defs.name_of(f.unit)),
            self.war.targets.get(&m.target).map_or("?", |e| e.def_name.as_str()),
            m.target_pos.x,
            m.target_pos.z,
            m.spot.x,
            m.spot.z,
            m.approach.time,
            m.approach.damage,
            m.kill_time,
            m.dwell_dps,
            m.budget,
            if m.cloaked { ", cloaked" } else { "" }
        ));
        self.precision.stats.started += 1;
        self.precision.missions.insert(id, m);
        self.precision_drive(id)
    }

    /// The best mission for raider `id` now, if any.
    fn precision_plan(&self, id: i32) -> Option<Mission> {
        let f = self.fighters.get(&id)?;
        let (unit, pos) = (f.unit, f.get_pos());
        let sh = self.precision_shooter(unit.def()?)?;
        let ground = self.precision.ground()?;
        let war = &self.war;
        let mut taken: Vec<i32> = self.precision.missions.values().map(|m| m.target).collect();
        taken.extend(self.precision.banned.iter().filter(|b| b.0 == id && b.2 > self.frame).map(|b| b.1));
        let danger_limit = DANGER_SHARE * (sh.dps + sh.health) / 10.0;
        let mut targets: Vec<(f32, &crate::military::Enemy)> = (war.targets.values())
            .filter(|e| e.identified && e.is_static && !e.is_dead && e.ud.is_some() && !taken.contains(&e.unit_id))
            .filter(|e| !e.is_porc || e.unit.is_being_built())
            .filter(|e| war.get_porc_threat(e.position) > 0.0)
            .map(|e| (distance(pos, e.position), e))
            .filter(|(d, _)| *d < MAX_TARGET_DISTANCE)
            .collect();
        targets.sort_by(|a, b| (a.0 / a.1.value.max(1.0)).total_cmp(&(b.0 / b.1.value.max(1.0))));
        let safe = |p: Vec3| war.get_porc_threat(p) == 0.0;
        let worth = unit.def().map_or(1.0, |d| d.cost(self.metal)).max(1.0);
        let frame = self.frame;
        let mut best: Option<(f32, Mission)> = None;
        for &(_, e) in targets.iter().take(TARGETS_PER_PLAN) {
            let def = e.ud?;
            let half = def.x_size().min(def.z_size()) as f32 * 4.0;
            let aim = e.position.y.max(ground.stand(e.position.x, e.position.z) + 0.5 * def.height());
            let kill_time = e.get_health() * e.def_health() / sh.dps.max(1.0);
            let ratio = e.value.max(1.0) / worth;
            if kill_time > MAX_KILL_TIME * ratio.clamp(1.0, 3.0) {
                continue;
            }
            let budget = sh.health * (BUDGET_SHARE * ratio.sqrt()).clamp(MIN_BUDGET_SHARE, MAX_BUDGET_SHARE);
            let mut spots = self.precision.spots_under(e.position, aim, half, &sh, budget / kill_time.max(1.0));
            let bad = &self.precision.bad_spots;
            spots.retain(|s| {
                s.1 && !bad.iter().any(|b| distance(b.0, s.0) < BAD_SPOT_RADIUS)
                    && self.precision_mobile_danger(s.0) <= danger_limit
            });
            // Out of fire first, then nearest; and others spread around.
            spots.sort_by(|a, b| a.2.total_cmp(&b.2).then(distance(a.0, pos).total_cmp(&distance(b.0, pos))));
            // Who could see a cloaked raider on the way: enemies near the
            // building.
            let revealers: Vec<Vec3> = if sh.cloak > 0.0 {
                (war.targets.values())
                    .filter(|r| {
                        !r.is_dead && frame - r.last_seen < 30 * 60 && distance(r.position, e.position) < 1500.0
                    })
                    .map(|r| r.position)
                    .collect()
            } else {
                Vec::new()
            };
            let mut tried: Vec<Vec3> = Vec::new();
            for (s, _, dps) in spots {
                if tried.len() >= SPOTS_PER_TARGET || tried.iter().any(|t| distance(*t, s) < 64.0) {
                    continue;
                }
                tried.push(s);
                let Some(a) = self.precision.approach(s, pos, &sh, &revealers, safe) else { continue };
                // In and out, and the stay.
                let damage = 2.0 * a.damage + dps * kill_time;
                if damage > budget {
                    continue;
                }
                let to_entry = distance(pos, a.entry);
                let time = to_entry / sh.speed.max(1.0) + a.time + kill_time;
                if to_entry > MAX_ENTRY_DISTANCE || time > MAX_MISSION_SECONDS || e.value < worth * time / WORTH_SECONDS
                {
                    continue;
                }
                let score = (time + DAMAGE_SECONDS * damage / sh.health) / e.value.max(1.0);
                if best.as_ref().is_none_or(|b| score < b.0) {
                    let m = Mission {
                        target: e.unit_id,
                        target_pos: e.position,
                        target_aim: aim,
                        target_half: half,
                        last_pos: pos,
                        moved: self.frame,
                        spot: s,
                        approach: a,
                        phase: Phase::Travel,
                        since: self.frame,
                        kill_time,
                        health: unit.health(),
                        ordered: 0,
                        target_health: f32::INFINITY,
                        checked: 0,
                        falling: true,
                        hurt: false,
                        budget,
                        dwell_dps: dps,
                        cloaked: sh.cloak > 0.0,
                        danger_limit,
                        danger_since: 0,
                    };
                    best = Some((score, m));
                }
            }
        }
        best.map(|b| b.1)
    }

    /// Enemy fighters that could come down on a raider at `p`: the summed
    /// danger of those within their range plus [`MOBILE_CLEARANCE`], seen
    /// lately.
    fn precision_mobile_danger(&self, p: Vec3) -> f32 {
        let frame = self.frame;
        (self.war.targets.values())
            .filter(|e| !e.is_static && !e.is_dead && e.identified && e.can_attack() && !e.can_fly())
            .filter(|e| frame - e.last_seen < 30 * 20)
            .filter(|e| distance(e.position, p) < e.threat_radius + MOBILE_CLEARANCE)
            .map(|e| e.get_danger())
            .sum()
    }

    /// One step of raider `id`'s mission; `false` once it is over.
    fn precision_drive(&mut self, id: i32) -> bool {
        let frame = self.frame;
        let Some(f) = self.fighters.get(&id) else {
            self.precision.missions.remove(&id);
            return false;
        };
        let (unit, pos) = (f.unit, f.get_pos());
        let Some(m) = self.precision.missions.get(&id) else { return false };
        let (phase, since, spot, target) = (m.phase, m.since, m.spot, m.target);
        let target_alive = self.war.targets.get(&target).is_some_and(|e| !e.is_dead);
        let entry = m.approach.entry;
        // Enemies near the spot, for long enough to matter.
        let near = self.precision_mobile_danger(spot) > m.danger_limit;
        let (moved, danger_since) = (distance(pos, m.last_pos) > 16.0, m.danger_since);
        if let Some(m) = self.precision.missions.get_mut(&id) {
            if moved {
                m.last_pos = pos;
                m.moved = frame;
            }
            m.danger_since = if !near {
                0
            } else if danger_since == 0 {
                frame
            } else {
                danger_since
            };
        }
        let danger = if near && danger_since > 0 && frame - danger_since >= DANGER_FRAMES { 1.0 } else { 0.0 };
        let Some(m) = self.precision.missions.get(&id) else { return false };
        let cloaked = m.cloaked;

        let mut why = "";
        let next = match phase {
            Phase::Travel if !target_alive || danger > 0.0 => {
                why = if target_alive { "enemies near the spot" } else { "target gone" };
                None
            }
            Phase::Travel if distance(pos, entry) < 120.0 => {
                // From here on it goes only where it is sent, and shoots
                // the target as it can: movement and fire apart. Cloaked,
                // it keeps its cloak (and fire) until it is there.
                unit.set_move_state(HOLD_POSITION, 0);
                self.precision_order_path(id, false);
                if cloaked {
                    unit.set_fire_state(HOLD_FIRE, 0);
                } else {
                    unit.execute_custom_command(cmd::UNIT_SET_TARGET, &[target as f32], 0, NO_TIMEOUT);
                }
                Some(Phase::Approach)
            }
            Phase::Travel => {
                if frame - m.ordered > 30 * 3 {
                    let mut nav = crate::nav!(self);
                    if let Some(f) = self.fighters.get_mut(&id) {
                        f.sneak(entry, &mut nav);
                    }
                    if let Some(m) = self.precision.missions.get_mut(&id) {
                        m.ordered = frame;
                    }
                }
                if frame - since > 30 * 20 {
                    why = "never got to the way in";
                    None
                } else {
                    Some(Phase::Travel)
                }
            }
            Phase::Approach | Phase::Fire if !target_alive => {
                self.precision.stats.kills += 1;
                self.log(&format!("precision: raider {id} killed its target"));
                Some(Phase::Leave)
            }
            Phase::Approach if danger > 0.0 || unit.health() < m.health - 1.3 * m.budget => {
                why = if danger > 0.0 { "enemies near the spot" } else { "hurt on the way in" };
                Some(Phase::Leave)
            }
            Phase::Approach if distance(pos, spot) < ARRIVED => {
                if cloaked {
                    unit.set_fire_state(FIRE_AT_WILL, 0);
                    unit.execute_custom_command(cmd::UNIT_SET_TARGET, &[target as f32], 0, NO_TIMEOUT);
                }
                self.precision.stats.fired += 1;
                Some(Phase::Fire)
            }
            // Stalled short of the spot (on a slope, between buildings),
            // but where it can do the job anyway: here will do.
            Phase::Approach
                if frame - m.moved > 30 * 3
                    && unit.def().and_then(|d| self.precision_shooter(d)).is_some_and(|sh| {
                        self.precision.fires_unseen_from(pos, m.target_pos, m.target_aim, m.target_half, &sh)
                    }) =>
            {
                self.log(&format!(
                    "precision: raider {id} fires from where it stalled, {:.0} short",
                    distance(pos, spot)
                ));
                if let Some(m) = self.precision.missions.get_mut(&id) {
                    m.spot = pos;
                }
                kgb_move_to(unit, pos, 0, frame);
                if cloaked {
                    unit.set_fire_state(FIRE_AT_WILL, 0);
                    unit.execute_custom_command(cmd::UNIT_SET_TARGET, &[target as f32], 0, NO_TIMEOUT);
                }
                self.precision.stats.fired += 1;
                Some(Phase::Fire)
            }
            Phase::Approach => {
                if (frame - since) as f32 > 30.0 * (2.0 * m.approach.time + 8.0) {
                    why = "way in took too long";
                    Some(Phase::Leave)
                } else {
                    Some(Phase::Approach)
                }
            }
            Phase::Fire => {
                // Late, unless the target is still going down (armoured
                // buildings take longer than their health says).
                if frame - m.checked >= 30 * 5 {
                    let health = self.war.targets.get(&target).map_or(0.0, |e| e.unit.health());
                    if let Some(m) = self.precision.missions.get_mut(&id) {
                        m.falling = health < m.target_health - 1.0;
                        m.target_health = health;
                        m.checked = frame;
                    }
                }
                let m = &self.precision.missions[&id];
                let late = (frame - since) as f32 > 30.0 * (2.0 * m.kill_time + 10.0) && !m.falling;
                let spent = unit.health() < m.health - 1.3 * m.budget;
                if late || danger > 0.0 || spent {
                    why = if late {
                        "target outlasted us"
                    } else if spent {
                        "took what it was worth"
                    } else {
                        "enemies near the spot"
                    };
                    Some(Phase::Leave)
                } else {
                    // Keep at it (the set-target may be dropped by other
                    // orders), and in the shadow.
                    let aim = unit.def().and_then(|d| self.precision_shooter(d)).map_or(0.0, |s| s.aim);
                    let off = distance(pos, spot) > ARRIVED || !self.precision.hidden_at(pos.x, pos.z, aim);
                    if frame - m.ordered > 30 * 5 || (off && frame - m.ordered > 30) {
                        unit.execute_custom_command(cmd::UNIT_SET_TARGET, &[target as f32], 0, NO_TIMEOUT);
                        if off {
                            kgb_move_to(unit, spot, 0, frame);
                        }
                        if let Some(m) = self.precision.missions.get_mut(&id) {
                            m.ordered = frame;
                        }
                    }
                    Some(Phase::Fire)
                }
            }
            Phase::Leave => {
                if distance(pos, entry) < 120.0 || frame - since > 30 * 30 {
                    None
                } else {
                    Some(Phase::Leave)
                }
            }
        };

        if matches!(why, "way in took too long" | "hurt on the way in" | "never got to the way in") {
            self.precision.bad_spots.push((spot, frame));
        }
        if !why.is_empty() {
            self.precision.planned.insert(id, frame + GIVE_UP_FRAMES - PLAN_FRAMES);
            self.precision.banned.push((id, target, frame + TARGET_BAN_FRAMES));
        }
        if !why.is_empty() {
            self.log(&format!(
                "precision: raider {id} gives up ({phase:?}): {why}, at ({:.0}, {:.0}), {:.0} from its spot",
                pos.x,
                pos.z,
                distance(pos, spot)
            ));
        }
        match next {
            None => {
                self.precision_end(id, unit);
                false
            }
            Some(p) if p != phase => {
                if p == Phase::Leave {
                    if !why.is_empty() {
                        self.precision.stats.aborted += 1;
                    }
                    unit.execute_custom_command(cmd::UNIT_CANCEL_TARGET, &[], 0, NO_TIMEOUT);
                    self.precision_order_path(id, true);
                }
                if let Some(m) = self.precision.missions.get_mut(&id) {
                    m.phase = p;
                    m.since = frame;
                    if p == Phase::Approach {
                        m.health = unit.health();
                    }
                }
                true
            }
            Some(_) => true,
        }
    }

    /// Send raider `id` along its way in (or back out, `reverse`).
    fn precision_order_path(&mut self, id: i32, reverse: bool) {
        let frame = self.frame;
        let (Some(f), Some(m)) = (self.fighters.get(&id), self.precision.missions.get_mut(&id)) else { return };
        let mut path = m.approach.path.clone();
        if reverse {
            f.unit.set_move_state(f.unit.def().map_or(1, |d| d.move_state().max(0)), 0);
            f.unit.set_fire_state(FIRE_AT_WILL, 0);
            path.reverse();
            path.push(m.approach.entry);
        }
        for (k, p) in path.iter().enumerate() {
            kgb_move_to(f.unit, *p, if k == 0 { 0 } else { OPTION_SHIFT_KEY }, frame);
        }
        m.ordered = frame;
    }

    fn precision_end(&mut self, id: i32, unit: Unit) {
        self.precision.missions.remove(&id);
        if unit.health() > 0.0 {
            unit.execute_custom_command(cmd::UNIT_CANCEL_TARGET, &[], 0, NO_TIMEOUT);
            unit.set_move_state(unit.def().map_or(1, |d| d.move_state().max(0)), 0);
            unit.set_fire_state(FIRE_AT_WILL, 0);
        }
    }

    /// Raider `unit` was hit (by `attacker`'s `weapon`, from `dir`).
    pub fn precision_damaged(&mut self, unit: Unit, attacker: Option<Unit>, dir: Vec3, weapon: Option<WeaponDef>) {
        let id = unit.id();
        let Some(m) = self.precision.missions.get(&id) else { return };
        if m.phase == Phase::Approach && !m.hurt {
            let wname = weapon.map_or_else(|| "?".to_owned(), |w| w.name());
            let pos = unit.pos();
            self.log(&format!(
                "precision: raider {id} first hit on the way in by {wname} at ({:.0}, {:.0}), exposure there {:.0}",
                pos.x,
                pos.z,
                self.precision.exposure(
                    pos.x,
                    pos.z,
                    unit.def().and_then(|d| self.precision_shooter(d)).map_or(0.0, |s| s.aim)
                )
            ));
            if let Some(m) = self.precision.missions.get_mut(&id) {
                m.hurt = true;
            }
        }
        let Some(m) = self.precision.missions.get(&id) else { return };
        // Fire at a spot planned under fire is in the budget.
        if m.phase != Phase::Fire || m.dwell_dps > 0.0 {
            return;
        }
        // At the spot, nothing should reach it: if a defense did, it sees
        // more than we thought.
        let pos = unit.pos();
        let gun = match attacker.filter(|a| self.precision.is_gun(a.id())) {
            Some(a) => Some(a.id()),
            None => self.precision.gun_toward(pos, dir),
        };
        let wname = weapon.map_or_else(|| "?".to_owned(), |w| w.name());
        let spot = m.spot;
        if let Some(g) = gun.filter(|g| !self.precision.distrusted.contains(g)) {
            self.precision.bad_spots.push((spot, self.frame));
            self.precision.distrusted.insert(g);
            self.precision.stats.hit_at_spot += 1;
            let aim = unit.def().and_then(|d| self.precision_shooter(d)).map_or(0.0, |s| s.aim);
            let ours = self.cb.friendly_units_in(pos, 200.0).len();
            self.log(&format!(
                "precision: {wname} hit raider {id} at its spot ({:.0}, {:.0}), {:.0} from it, {ours} of ours within 200, \
                 attacker seen: {}; defense {g} distrusted",
                pos.x,
                pos.z,
                distance(pos, m.spot),
                attacker.is_some()
            ));
            if let Some(name) = self.war.targets.get(&g).map(|e| e.def_name.clone()) {
                for l in self.precision.profiles(&name, pos.x, pos.z, aim) {
                    self.log(&format!("precision:   {l}"));
                }
            }
        }
        let frame = self.frame;
        if let Some(m) = self.precision.missions.get_mut(&id) {
            m.phase = Phase::Leave;
            m.since = frame;
        }
        self.precision.stats.aborted += 1;
        unit.execute_custom_command(cmd::UNIT_CANCEL_TARGET, &[], 0, NO_TIMEOUT);
        self.precision_order_path(id, true);
    }

    /// Forget missions whose raiders died, were given away, or retreat.
    pub fn precision_cleanup(&mut self) {
        let ids: Vec<i32> = self.precision.missions.keys().copied().collect();
        for id in ids {
            let gone = self.fighters.get(&id).is_none_or(|f| f.is_dead());
            let retreating = self.fighters.get(&id).is_some_and(|f| self.retreat_is_retreating(f.unit));
            if gone {
                self.precision.stats.lost += 1;
                self.precision.missions.remove(&id);
            } else if retreating && let Some(unit) = self.fighters.get(&id).map(|f| f.unit) {
                self.precision_end(id, unit);
            }
        }
        self.precision.planned.retain(|id, _| self.fighters.contains_key(id));
        let frame = self.frame;
        self.precision.bad_spots.retain(|b| frame - b.1 < BAD_SPOT_FRAMES);
        self.precision.banned.retain(|b| b.2 > frame);
    }
}
