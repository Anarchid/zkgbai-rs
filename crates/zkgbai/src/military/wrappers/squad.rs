//! A group of fighters acting together.
//!
//! The original's `Squad`, `RaiderSquad`, `ShieldSquad` and `HoverSquad`
//! classes are flattened into one type with a [`SquadKind`] tag. Members are
//! fighter-store ids; methods take the store explicitly.
//!
//! Shield squads keep their leader *outside* `fighters` (the other members
//! orbit it); all other kinds keep the leader among `fighters`.

use std::collections::BTreeMap;

use springai::{NO_TIMEOUT, Unit, Vec3, Weapon};

use crate::defs::{Defs, cmd};
use crate::kgbutil::{Nav, kgb_move_to};
use crate::military::wrappers::fighter::next_waypoint;
use crate::military::{FighterStore, ScoutTasks};
use crate::util::{
    NULLPOS, OPTION_SHIFT_KEY, distance, get_angular_point, get_directional_point, get_formation_point,
    get_radial_point, kgb_rand, lerp,
};

pub type SquadId = u32;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SquadKind {
    Assault,
    Raider,
    Shield,
    Hover,
}

pub struct Squad {
    pub id: SquadId,
    pub kind: SquadKind,
    /// Fighter-store ids.
    pub fighters: Vec<i32>,
    pub leader: Option<i32>,
    pub metal_value: f32,
    /// 'f' = forming, 'r' = rallying, 'a' = attacking.
    pub status: char,
    pub income: f32,
    pub is_air_squad: bool,
    pub target: Vec3,
    pub last_assignment_frame: i32,
    /// Raider squads: raider class tag.
    pub raider_type: char,
    /// Shield squads.
    pub num_felons: i32,
    /// Shield squads.
    pub num_aspis: i32,
    /// Shield squads.
    pub low_shields: bool,
    team: i32,
    /// Join counter; members get increasing indices.
    index: i32,
    first_rally_frame: i32,
    /// Shield squads: weight of the current leader (felons lead first).
    leader_weight: i32,
    /// Shield squads: total capacity of members' shields.
    max_shield_power: f32,
    /// Shield squads: each shielded member's shield weapon and its capacity.
    shields: BTreeMap<i32, (Weapon, f32)>,
    felon_id: i32,
    aspis_id: i32,
    domi_id: i32,
    thug_id: i32,
    law_id: i32,
}

/// Remaining health fraction, counting paralysis and capture as damage.
fn health_fraction(u: Unit) -> f32 {
    let max = u.max_health();
    if max <= 0.0 {
        return 0.0;
    }
    ((u.health() - u.paralyze_damage()) / max) * (1.0 - u.capture_progress())
}

fn def_id(u: Unit) -> Option<i32> {
    u.def().map(|d| d.id())
}

impl Squad {
    pub fn new(id: SquadId, kind: SquadKind, team: i32, defs: &Defs) -> Self {
        Squad {
            id,
            kind,
            fighters: Vec::new(),
            leader: None,
            metal_value: 0.0,
            status: 'f',
            income: 0.0,
            is_air_squad: false,
            target: NULLPOS,
            last_assignment_frame: 0,
            raider_type: '\0',
            num_felons: 0,
            num_aspis: 0,
            low_shields: false,
            team,
            index: 0,
            first_rally_frame: 0,
            leader_weight: 0,
            max_shield_power: 0.0,
            shields: BTreeMap::new(),
            felon_id: defs.id("shieldfelon"),
            aspis_id: defs.id("shieldshield"),
            domi_id: defs.id("vehcapture"),
            thug_id: defs.id("shieldassault"),
            law_id: defs.id("shieldriot"),
        }
    }

    /// Add a member. Shield squads cannot issue path-following orders from
    /// here (a new leader resuming the attack, stragglers regrouping) and use
    /// direct orders instead; prefer [`Squad::add_unit_nav`] for them.
    pub fn add_unit(&mut self, fighter: i32, fighters: &mut FighterStore, frame: i32) {
        self.add_unit_inner(fighter, fighters, frame, None);
    }

    /// [`Squad::add_unit`] with full path-following orders.
    pub fn add_unit_nav(&mut self, fighter: i32, fighters: &mut FighterStore, nav: &mut Nav) {
        let frame = nav.frame;
        self.add_unit_inner(fighter, fighters, frame, Some(nav));
    }

    /// Remove a member. Shield squads use direct orders for regrouping
    /// stragglers here; prefer [`Squad::remove_unit_nav`] for them.
    ///
    /// Raider squads: when the leader leaves, it is stopped but keeps its
    /// scout task id, since the task list is not available here; the caller
    /// must clear the task (every original call site does, or use
    /// [`Squad::remove_raider`]).
    pub fn remove_unit(&mut self, fighter: i32, fighters: &mut FighterStore, frame: i32) {
        self.remove_unit_inner(fighter, fighters, frame, None, None);
    }

    /// [`Squad::remove_unit`] with full path-following orders.
    pub fn remove_unit_nav(&mut self, fighter: i32, fighters: &mut FighterStore, nav: &mut Nav) {
        let frame = nav.frame;
        self.remove_unit_inner(fighter, fighters, frame, Some(nav), None);
    }

    /// Raider squads: [`Squad::remove_unit`] that also clears the leaving
    /// leader's scout task, like the original `RaiderSquad::removeUnit`.
    pub fn remove_raider(&mut self, fighter: i32, fighters: &mut FighterStore, frame: i32, tasks: &mut ScoutTasks) {
        self.remove_unit_inner(fighter, fighters, frame, None, Some(tasks));
    }

    pub fn set_target(&mut self, pos: Vec3, fighters: &mut FighterStore, nav: &mut Nav) {
        match self.kind {
            SquadKind::Shield => self.shield_set_target(pos, fighters, nav),
            SquadKind::Hover => self.hover_set_target(pos, fighters, nav),
            SquadKind::Assault | SquadKind::Raider => self.base_set_target(pos, fighters, nav),
        }
    }

    pub fn retreat_to(&mut self, pos: Vec3, fighters: &mut FighterStore, nav: &mut Nav) {
        self.target = pos;
        if self.kind == SquadKind::Shield {
            let leader_alive = self.leader_fighter(fighters).is_some_and(|l| l.unit.health() > 0.0);
            if leader_alive {
                self.collect_stragglers(fighters, nav.frame, Some(&mut *nav));
                if let Some(l) = self.leader.and_then(|l| fighters.get_mut(&l)) {
                    l.move_to(self.target, nav);
                }
            }
            return;
        }
        for id in &self.fighters {
            if let Some(f) = fighters.get_mut(id) {
                f.move_to(self.target, nav);
                f.target = self.target;
            }
        }
    }

    pub fn get_pos(&self, fighters: &FighterStore) -> Vec3 {
        match self.kind {
            SquadKind::Raider => self.leader_fighter(fighters).map_or(self.target, |l| l.get_pos()),
            SquadKind::Shield => match self.leader_fighter(fighters) {
                Some(l) if l.unit.health() > 0.0 && l.unit.team() == self.team => l.get_pos(),
                // Otherwise if the squad has no units, return its target.
                _ => self.target,
            },
            SquadKind::Assault | SquadKind::Hover => {
                if self.fighters.is_empty() {
                    return self.target;
                }
                // Average position of the units near the leader.
                let maxdist = if self.is_air_squad { 300.0 } else { 200.0 };
                let leader_pos = self.leader_fighter(fighters).map(|l| l.get_pos());
                let (mut x, mut z, mut count) = (0.0, 0.0, 0);
                for f in self.fighters.iter().filter_map(|id| fighters.get(id)) {
                    let p = f.get_pos();
                    if leader_pos.is_some_and(|lp| distance(p, lp) > maxdist) {
                        continue;
                    }
                    x += p.x;
                    z += p.z;
                    count += 1;
                }
                if count == 0 {
                    return self.target;
                }
                Vec3::xz(x / count as f32, z / count as f32)
            }
        }
    }

    pub fn is_rallied(&mut self, fighters: &mut FighterStore, nav: &mut Nav) -> bool {
        if self.kind == SquadKind::Shield {
            let Some(lpos) = self.leader_fighter(fighters).map(|l| l.get_pos()) else { return false };
            let rallied = self
                .fighters
                .iter()
                .filter_map(|id| fighters.get(id))
                .filter(|f| distance(f.get_pos(), lpos) < 350.0)
                .count() as f32;
            return rallied / self.fighters.len() as f32 > 0.75;
        }
        let frame = nav.frame;
        if self.first_rally_frame == 0 {
            self.first_rally_frame = frame;
        }
        if frame - self.first_rally_frame > 900 {
            return true;
        }
        if self.kind != SquadKind::Raider {
            // The original wonders why this is done here too.
            self.set_target(self.target, fighters, nav);
        }
        !self.fighters.iter().filter_map(|id| fighters.get(id)).any(|f| f.out_of_range)
    }

    /// Whether the squad has no members left (shield squads: nor a leader).
    /// Ids missing from the store do not count as members.
    pub fn is_dead(&self, fighters: &FighterStore) -> bool {
        let no_members = !self.fighters.iter().any(|id| fighters.contains_key(id));
        match self.kind {
            SquadKind::Shield => no_members && self.leader_fighter(fighters).is_none(),
            _ => no_members,
        }
    }

    pub fn is_shield_squad(&self) -> bool {
        self.kind == SquadKind::Shield
    }

    pub fn get_threat(&self, fighters: &FighterStore) -> i32 {
        let mut threat: f32 =
            self.fighters.iter().filter_map(|id| fighters.get(id)).map(|f| f.power + f.unit.max_health()).sum();
        if self.kind == SquadKind::Shield {
            if let Some(l) = self.leader_fighter(fighters) {
                threat += l.unit.power() + l.unit.max_health();
            }
            threat += self.shields.values().map(|(_, power)| power / 2.0).sum::<f32>();
        }
        (threat / 10.0) as i32
    }

    /// Stop accepting new members (the squad is complete).
    ///
    /// Dissolves the squad if it ended up too small to be useful.
    pub fn cutoff(&mut self, fighters: &mut FighterStore) {
        if self.fighters.len() < 3 && self.metal_value < 1000.0 {
            for id in &self.fighters {
                if let Some(f) = fighters.get_mut(id) {
                    f.squad = None;
                }
            }
            self.fighters.clear();
            self.leader = None;
        }
    }

    pub fn get_size(&self) -> usize {
        self.fighters.len()
    }

    pub fn get_leader(&self, fighters: &FighterStore) -> Option<Unit> {
        self.leader.and_then(|id| fighters.get(&id)).map(|f| f.unit)
    }

    /// Assault and air squads: replace a dead leader, then let the slowest
    /// unit near the leader lead so the squad stays together (the Java
    /// original's `getBestLeader`, run whenever it checked the squad).
    /// Shield squads keep their own leader choice.
    pub fn select_leader(&mut self, fighters: &FighterStore) {
        if self.kind != SquadKind::Shield {
            self.leader = self.get_best_leader(fighters);
        }
    }

    // ── Raider squads ───────────────────────────────────────────────────

    pub fn sneak(&mut self, pos: Vec3, fighters: &mut FighterStore, nav: &mut Nav) {
        self.raider_order(pos, fighters, nav, false);
    }

    pub fn raid(&mut self, pos: Vec3, fighters: &mut FighterStore, nav: &mut Nav) {
        self.raider_order(pos, fighters, nav, true);
    }

    // ── Shield squads ───────────────────────────────────────────────────

    pub fn get_health(&self, fighters: &FighterStore) -> f32 {
        let Some(leader) = self.leader_fighter(fighters) else { return 0.0 };
        if leader.is_dead() {
            return 0.0;
        }
        let count = (self.fighters.len() + 1) as f32;
        let leader_health = health_fraction(leader.unit);
        let health = leader_health
            + self.fighters.iter().filter_map(|id| fighters.get(id)).map(|f| health_fraction(f.unit)).sum::<f32>();
        leader_health.min(health / count)
    }

    pub fn get_shields(&self) -> f32 {
        if self.max_shield_power == 0.0 {
            return 1.0;
        }
        let current: f32 = self.shields.values().map(|(w, _)| w.shield_power()).sum();
        current / self.max_shield_power
    }

    /// Dissolve the squad; returns the members' fighter ids.
    pub fn disband(&mut self, fighters: &mut FighterStore) -> Vec<i32> {
        let mut disbanded = self.fighters.clone();
        if let Some(lid) = self.leader.take() {
            if let Some(l) = fighters.get_mut(&lid) {
                if self.kind == SquadKind::Shield {
                    // Lift the leader's speed limit.
                    let speed = l.unit.max_speed() * 30.0;
                    l.unit.execute_custom_command(cmd::WANTED_SPEED, &[speed], 0, NO_TIMEOUT);
                }
                l.squad = None;
            }
            if !disbanded.contains(&lid) {
                disbanded.push(lid);
            }
            self.leader_weight = 0;
        }
        for id in &self.fighters {
            if let Some(f) = fighters.get_mut(id) {
                f.squad = None;
                f.unit.set_move_state(1, 0);
            }
        }
        self.fighters.clear();
        self.metal_value = 0.0;
        self.max_shield_power = 0.0;
        disbanded
    }

    // ── Internals ───────────────────────────────────────────────────────

    fn leader_fighter<'a>(&self, fighters: &'a FighterStore) -> Option<&'a crate::military::Fighter> {
        self.leader.and_then(|l| fighters.get(&l))
    }

    fn add_unit_inner(&mut self, id: i32, fighters: &mut FighterStore, frame: i32, nav: Option<&mut Nav>) {
        match self.kind {
            SquadKind::Shield => self.shield_add_unit(id, fighters, frame, nav),
            SquadKind::Raider => {
                let Some(f) = fighters.get_mut(&id) else { return };
                self.fighters.push(id);
                f.squad = Some(self.id);
                f.index = self.index;
                self.index += 1;
                if self.leader.is_none() {
                    self.leader = Some(id);
                }
            }
            SquadKind::Assault | SquadKind::Hover => {
                let Some(f) = fighters.get_mut(&id) else { return };
                self.fighters.push(id);
                f.squad = Some(self.id);
                f.index = self.index;
                self.index += 1;
                self.metal_value += f.metal_value;
                f.unit.set_move_state(1, 0);
                if self.leader.is_none() {
                    self.leader = Some(id);
                }
            }
        }
    }

    fn remove_unit_inner(
        &mut self,
        id: i32,
        fighters: &mut FighterStore,
        frame: i32,
        nav: Option<&mut Nav>,
        tasks: Option<&mut ScoutTasks>,
    ) {
        // port: the original assumed `id` was a member; only touch the
        // bookkeeping (metal value etc.) if it actually is one.
        if !self.fighters.contains(&id) && self.leader != Some(id) {
            if let Some(f) = fighters.get_mut(&id) {
                if f.squad == Some(self.id) && self.kind != SquadKind::Shield {
                    f.squad = None;
                }
            }
            return;
        }
        match self.kind {
            SquadKind::Shield => self.shield_remove_unit(id, fighters, frame, nav),
            SquadKind::Raider => {
                if let Some(f) = fighters.get_mut(&id) {
                    f.squad = None;
                }
                self.fighters.retain(|&f| f != id);
                if self.leader == Some(id) {
                    if let Some(f) = fighters.get_mut(&id) {
                        match tasks {
                            Some(tasks) => f.clear_task(tasks),
                            None if !f.is_dead() => {
                                f.unit.stop(0);
                            }
                            None => {}
                        }
                    }
                    self.leader = self.get_new_leader(fighters);
                }
            }
            SquadKind::Assault | SquadKind::Hover => {
                self.fighters.retain(|&f| f != id);
                if self.leader == Some(id) {
                    self.leader = self.get_new_leader(fighters);
                }
                if let Some(f) = fighters.get_mut(&id) {
                    self.metal_value -= f.metal_value;
                    f.squad = None;
                }
            }
        }
    }

    fn base_set_target(&mut self, pos: Vec3, fighters: &mut FighterStore, nav: &mut Nav) {
        // Set a target for the squad to attack.
        self.target = pos;
        let (maxdist, waydist) = if self.is_air_squad { (300.0, 150.0) } else { (200.0, 100.0) };
        let Some(leader) = self.leader_fighter(fighters) else { return };
        let leader_pos = leader.get_pos();
        let mut path = leader.get_fight_path(pos, nav);
        let skip = if self.is_air_squad { 2 } else { 1 };
        let Some(waypoint) = next_waypoint(&mut path, skip) else { return };
        for id in &self.fighters {
            let Some(f) = fighters.get_mut(id) else { continue };
            f.target = self.target;
            let fpos = f.get_pos();
            let fdist = distance(fpos, leader_pos);
            if fdist > maxdist {
                f.out_of_range = true;
                let regroup = get_angular_point(leader_pos, fpos, lerp(waydist, maxdist, kgb_rand() as f32));
                if fdist > 2.0 * maxdist {
                    f.move_to(regroup, nav);
                } else {
                    kgb_move_to(f.unit, regroup, 0, nav.frame);
                }
            } else {
                f.out_of_range = false;
                f.unit.fight(get_formation_point(fpos, leader_pos, waypoint), 0);
            }
        }

        // Add one extra waypoint.
        let Some(waypoint) = next_waypoint(&mut path, skip) else { return };
        for f in self.fighters.iter().filter_map(|id| fighters.get(id)) {
            if f.unit.health() <= 0.0 || f.out_of_range {
                continue;
            }
            f.unit.fight(get_formation_point(f.get_pos(), leader_pos, waypoint), OPTION_SHIFT_KEY);
        }
    }

    fn hover_set_target(&mut self, pos: Vec3, fighters: &mut FighterStore, nav: &mut Nav) {
        // Set a target for the squad to attack.
        self.target = pos;
        let (maxdist, waydist) = (150.0, 75.0);
        let Some(leader) = self.leader_fighter(fighters) else { return };
        let leader_pos = leader.get_pos();
        let far_away = distance(leader_pos, self.target) > 450.0;
        let mut path = leader.get_fight_path(pos, nav);
        let Some(waypoint) = next_waypoint(&mut path, 2) else { return };
        for id in &self.fighters {
            let Some(f) = fighters.get_mut(id) else { continue };
            let fpos = f.get_pos();
            let fdist = distance(leader_pos, fpos);
            if fdist > maxdist {
                f.out_of_range = true;
                let regroup = get_angular_point(leader_pos, fpos, lerp(waydist, maxdist, kgb_rand() as f32));
                if fdist > 2.0 * maxdist {
                    f.move_to(regroup, nav);
                } else {
                    kgb_move_to(f.unit, regroup, 0, nav.frame);
                }
            } else {
                f.out_of_range = false;
                if far_away {
                    kgb_move_to(f.unit, waypoint, 0, nav.frame);
                } else {
                    f.unit.fight(waypoint, 0);
                }
            }
        }
    }

    fn raider_order(&mut self, pos: Vec3, fighters: &mut FighterStore, nav: &mut Nav, attack: bool) {
        self.target = pos;
        let (maxdist, waydist) = (150.0, 75.0);
        let Some(leader) = self.leader_fighter(fighters) else { return };
        let leader_pos = leader.get_pos();
        if self.raider_type == 's' {
            // Small raiders get independent paths, but if they're out of range of the group then they move to the leader
            // instead of the target. This works surprisingly well.
            for id in &self.fighters {
                let Some(r) = fighters.get_mut(id) else { continue };
                let rpos = r.get_pos();
                let fdist = distance(leader_pos, rpos);
                if fdist > maxdist {
                    r.out_of_range = true;
                    if fdist > 2.0 * maxdist {
                        r.sneak(get_radial_point(leader_pos, waydist), nav);
                    } else {
                        let regroup = get_angular_point(leader_pos, rpos, lerp(waydist, maxdist, kgb_rand() as f32));
                        kgb_move_to(r.unit, regroup, 0, nav.frame);
                    }
                } else {
                    r.out_of_range = false;
                    if attack {
                        r.raid(pos, nav);
                    } else {
                        r.sneak(pos, nav);
                    }
                }
            }
        } else {
            // Medium raiders are treated more like assaults. The group only gets one path that all units follow.
            // This keeps them mobbed up better so they can do maximum damage. Out of range units are still moved
            // to the leader to keep them dynamically rallied in case they get split up for whatever reason.
            let mut path = leader.get_raid_path(pos, nav);
            let Some(waypoint) = next_waypoint(&mut path, 1) else { return };
            for id in &self.fighters {
                let Some(r) = fighters.get_mut(id) else { continue };
                let rpos = r.get_pos();
                let fdist = distance(leader_pos, rpos);
                if fdist > maxdist {
                    r.out_of_range = true;
                    let regroup = get_angular_point(leader_pos, rpos, lerp(waydist, maxdist, kgb_rand() as f32));
                    if fdist > 2.0 * maxdist {
                        r.sneak(regroup, nav);
                    } else {
                        kgb_move_to(r.unit, regroup, 0, nav.frame);
                    }
                } else {
                    r.out_of_range = false;
                    if attack {
                        r.unit.fight(waypoint, 0);
                    } else {
                        kgb_move_to(r.unit, waypoint, 0, nav.frame);
                    }
                }
            }
        }
    }

    fn shield_add_unit(&mut self, id: i32, fighters: &mut FighterStore, frame: i32, mut nav: Option<&mut Nav>) {
        let Some(f) = fighters.get_mut(&id) else { return };
        f.squad = Some(self.id);
        f.index = self.index;
        self.index += 1;
        self.metal_value += f.metal_value;
        let unit = f.unit;
        let def = def_id(unit);
        let weight = self.unit_weight(def, f.index);

        if def == Some(self.felon_id) {
            self.num_felons += 1;
        }
        if def == Some(self.law_id) {
            unit.execute_custom_command(cmd::WANT_ONOFF, &[1.0], 0, NO_TIMEOUT);
        }
        if def != Some(self.aspis_id) {
            for w in unit.weapons() {
                let power = w.def().map_or(0.0, |d| d.shield_power());
                if power > 0.0 {
                    self.max_shield_power += power;
                    self.shields.insert(id, (w, power));
                    break;
                }
            }
        } else {
            self.num_aspis += 1;
        }

        let old_leader = self.leader.and_then(|l| fighters.get(&l)).map(|l| (l.id, l.unit));
        match old_leader {
            None => {
                unit.set_move_state(1, 0);
                self.leader = Some(id);
                self.leader_weight = weight;
                unit.execute_custom_command(cmd::WANTED_SPEED, &[45.0], 0, NO_TIMEOUT);
            }
            Some((old_id, old_unit)) if weight < self.leader_weight => {
                unit.set_move_state(1, 0);
                old_unit.set_move_state(0, 0);
                // Set the wanted max speed for the new leader and remove the speed limit from the old leader.
                unit.execute_custom_command(cmd::WANTED_SPEED, &[45.0], 0, NO_TIMEOUT);
                // CMD_WANTED_SPEED uses elmos/sec while the game returns elmos/frame, thus MaxSpeed*30.
                old_unit.execute_custom_command(cmd::WANTED_SPEED, &[old_unit.max_speed() * 30.0], 0, NO_TIMEOUT);
                self.fighters.push(old_id);
                self.leader = Some(id);
                self.leader_weight = weight;
                // port: the C++ compared the target with nullptr, which builds
                // an AIFloat3 from a null pointer; Java checks for a target.
                if self.target != NULLPOS {
                    let target = self.target;
                    if let Some(l) = fighters.get_mut(&id) {
                        match nav.as_deref_mut() {
                            Some(nav) => l.fight_to(target, nav),
                            None => {
                                l.unit.fight(target, 0);
                            }
                        }
                    }
                }
                self.collect_stragglers(fighters, frame, nav);
            }
            Some((leader_id, _)) => {
                unit.set_move_state(0, 0);
                self.fighters.push(id);
                let radius = self.orbit_radius(def);
                unit.execute_custom_command(cmd::ORBIT, &[leader_id as f32, radius], 0, NO_TIMEOUT);
                unit.execute_custom_command(cmd::ORBIT_DRAW, &[leader_id as f32], OPTION_SHIFT_KEY, NO_TIMEOUT);
            }
        }
    }

    fn shield_remove_unit(&mut self, id: i32, fighters: &mut FighterStore, frame: i32, nav: Option<&mut Nav>) {
        let def = fighters.get(&id).and_then(|f| def_id(f.unit));
        if def == Some(self.felon_id) {
            self.num_felons -= 1;
        }
        // port: the C++ stopped recounting aspis when it dropped the Java
        // `isDead()` bookkeeping; decrement here like felons.
        if def == Some(self.aspis_id) {
            self.num_aspis -= 1;
        }
        if let Some((_, power)) = self.shields.remove(&id) {
            self.max_shield_power -= power;
        }
        if let Some(f) = fighters.get(&id) {
            self.metal_value -= f.metal_value;
        }

        if self.leader == Some(id) {
            self.leader = self.get_new_leader(fighters);
            let Some(lid) = self.leader else {
                self.leader_weight = 0;
                return;
            };
            self.fighters.retain(|&f| f != lid);
            if let Some(l) = fighters.get(&lid) {
                self.leader_weight = self.unit_weight(def_id(l.unit), l.index);
                l.unit.set_move_state(1, 0);
                l.unit.execute_custom_command(cmd::WANTED_SPEED, &[45.0], 0, NO_TIMEOUT);
            }
            self.target = NULLPOS;
            self.collect_stragglers(fighters, frame, nav);
        } else {
            self.fighters.retain(|&f| f != id);
        }
    }

    fn shield_set_target(&mut self, pos: Vec3, fighters: &mut FighterStore, nav: &mut Nav) {
        // Set a target for the squad to attack.
        self.target = pos;
        if self.leader_fighter(fighters).is_none() {
            return;
        }
        let frame = nav.frame;
        self.collect_stragglers(fighters, frame, Some(&mut *nav));
        let Some(leader) = self.leader_fighter(fighters) else { return };
        let mut path = leader.get_fight_path(get_directional_point(self.target, leader.get_pos(), 135.0), nav);
        let Some(wp) = next_waypoint(&mut path, 1) else { return };
        kgb_move_to(leader.unit, wp, 0, frame);
        // Add one extra waypoint, skipping every other one since they're close together.
        if let Some(wp) = next_waypoint(&mut path, 1) {
            kgb_move_to(leader.unit, wp, OPTION_SHIFT_KEY, frame);
        }
    }

    /// Bring members that strayed far from the leader back into orbit.
    /// Without `nav`, they move straight back instead of avoiding enemies.
    fn collect_stragglers(&self, fighters: &mut FighterStore, frame: i32, mut nav: Option<&mut Nav>) {
        let Some(lid) = self.leader else { return };
        let Some(leader_pos) = fighters.get(&lid).map(|l| l.get_pos()) else { return };
        for id in &self.fighters {
            let Some(f) = fighters.get_mut(id) else { continue };
            let fpos = f.get_pos();
            if distance(leader_pos, fpos) <= 600.0 {
                continue;
            }
            // Have units that haven't rallied yet avoid enemies.
            let regroup = get_angular_point(leader_pos, fpos, 400.0);
            match nav.as_deref_mut() {
                Some(nav) => f.move_to(regroup, nav),
                None => kgb_move_to(f.unit, regroup, 0, frame),
            }
            let radius = self.orbit_radius(def_id(f.unit));
            f.unit.execute_custom_command(cmd::ORBIT, &[lid as f32, radius], OPTION_SHIFT_KEY, NO_TIMEOUT);
            f.unit.execute_custom_command(cmd::ORBIT_DRAW, &[lid as f32], OPTION_SHIFT_KEY, NO_TIMEOUT);
        }
    }

    /// Orbit distance around the shield squad leader by unit type.
    fn orbit_radius(&self, def: Option<i32>) -> f32 {
        match def {
            Some(d) if d == self.domi_id => 250.0,
            Some(d) if d == self.aspis_id => 40.0,
            Some(d) if d == self.thug_id => 125.0,
            _ => 90.0,
        }
    }

    /// Shield squads: felons lead first, then the earliest member.
    fn unit_weight(&self, def: Option<i32>, index: i32) -> i32 {
        if def == Some(self.felon_id) { index } else { self.index + index }
    }

    fn get_new_leader(&self, fighters: &FighterStore) -> Option<i32> {
        let members = self.fighters.iter().filter_map(|id| fighters.get(id));
        match self.kind {
            SquadKind::Shield => {
                let mut best: Option<(i32, i32)> = None;
                for f in members {
                    let score = self.unit_weight(def_id(f.unit), f.index);
                    if best.is_none_or(|(_, s)| score < s) {
                        best = Some((f.id, score));
                    }
                }
                best.map(|(id, _)| id)
            }
            SquadKind::Raider => {
                // Bombs were meant to lead last, but like the original the
                // later candidates are compared against the raw index of the
                // best one so far, so the earliest member always leads.
                let mut best: Option<i32> = None;
                let mut tmpindex = 0;
                for f in members {
                    let bomb = f.unit.def().is_some_and(|d| d.name().contains("bomb"));
                    let index = if bomb { f.index + self.index } else { f.index };
                    if best.is_none() || index < tmpindex {
                        best = Some(f.id);
                        tmpindex = f.index;
                    }
                }
                best
            }
            SquadKind::Assault | SquadKind::Hover => {
                let mut best: Option<(i32, i32)> = None;
                for f in members {
                    if best.is_none_or(|(_, i)| f.index < i) {
                        best = Some((f.id, f.index));
                    }
                }
                best.map(|(id, _)| id)
            }
        }
    }

    fn get_best_leader(&mut self, fighters: &FighterStore) -> Option<i32> {
        let alive = self.leader_fighter(fighters).is_some_and(|l| l.unit.health() > 0.0 && l.unit.team() == self.team);
        if !alive {
            self.leader = self.get_new_leader(fighters);
        }
        let leader = self.leader_fighter(fighters)?;
        let leader_pos = leader.get_pos();
        let mut new_leader = leader.id;
        let mut minspeed = leader.unit.def().map_or(f32::MAX, |d| d.speed());
        for f in self.fighters.iter().filter_map(|id| fighters.get(id)) {
            if distance(f.get_pos(), leader_pos) > 300.0 {
                continue;
            }
            let speed = f.unit.speed();
            if speed < minspeed {
                minspeed = speed;
                new_leader = f.id;
            }
        }
        Some(new_leader)
    }
}

/// All squads by id.
#[derive(Default)]
pub struct Squads {
    map: BTreeMap<SquadId, Squad>,
    next_id: SquadId,
}

impl Squads {
    pub fn new() -> Self {
        Self::default()
    }

    /// Create an empty squad and return its id.
    pub fn create(&mut self, kind: SquadKind, team: i32, defs: &Defs) -> SquadId {
        self.next_id += 1;
        let id = self.next_id;
        self.map.insert(id, Squad::new(id, kind, team, defs));
        id
    }

    pub fn get(&self, id: SquadId) -> Option<&Squad> {
        self.map.get(&id)
    }

    pub fn get_mut(&mut self, id: SquadId) -> Option<&mut Squad> {
        self.map.get_mut(&id)
    }

    pub fn remove(&mut self, id: SquadId) -> Option<Squad> {
        self.map.remove(&id)
    }

    pub fn contains(&self, id: SquadId) -> bool {
        self.map.contains_key(&id)
    }

    pub fn ids(&self) -> Vec<SquadId> {
        self.map.keys().copied().collect()
    }
}

/// A minimal in-process stand-in for the engine callback table, enough to
/// exercise the wrappers' bookkeeping.
#[cfg(test)]
pub(crate) mod fake_engine {
    use std::cell::RefCell;
    use std::collections::HashMap;
    use std::ffi::{CStr, c_char, c_int, c_void};

    use springai::ffi::{CommandTopic, SCustomUnitCommand, SSkirmishAICallback};
    use springai::{Callback, Unit, Vec3};

    pub const TEAM: i32 = 0;
    pub const DEF_NAMES: [&CStr; 8] = [
        c"shieldfelon",
        c"shieldshield",
        c"vehcapture",
        c"shieldassault",
        c"shieldriot",
        c"cloakraid",
        c"cloakbomb",
        c"cloakassault",
    ];

    #[derive(Clone, Copy)]
    pub struct FakeUnit {
        pub def: i32,
        pub pos: Vec3,
        pub health: f32,
        pub team: i32,
    }

    /// An issued order: (unit id, command topic, custom command id or -1).
    pub type Order = (i32, u32, i32);

    thread_local! {
        pub static UNITS: RefCell<HashMap<i32, FakeUnit>> = RefCell::new(HashMap::new());
        pub static ORDERS: RefCell<Vec<Order>> = const { RefCell::new(Vec::new()) };
    }

    unsafe extern "C" fn handle_command(_ai: c_int, _to: c_int, _cmd: c_int, topic: c_int, data: *mut c_void) -> c_int {
        // Every unit command struct starts with the unit id.
        let unit = unsafe { *(data as *const c_int) };
        let custom = if topic as u32 == CommandTopic::COMMAND_UNIT_CUSTOM {
            unsafe { (*(data as *const SCustomUnitCommand)).cmdId }
        } else {
            -1
        };
        ORDERS.with(|o| o.borrow_mut().push((unit, topic as u32, custom)));
        0
    }

    unsafe extern "C" fn get_unit_defs(_ai: c_int, ids: *mut c_int, max: c_int) -> c_int {
        let n = DEF_NAMES.len() as c_int;
        if !ids.is_null() {
            for i in 0..n.min(max) {
                unsafe { *ids.add(i as usize) = i };
            }
        }
        n
    }

    unsafe extern "C" fn def_name(_ai: c_int, def: c_int) -> *const c_char {
        DEF_NAMES.get(def as usize).map_or(c"".as_ptr(), |n| n.as_ptr())
    }

    unsafe extern "C" fn zero_count(_ai: c_int, _id: c_int) -> c_int {
        0
    }

    fn with_unit<T>(id: c_int, f: impl FnOnce(&FakeUnit) -> T, default: T) -> T {
        UNITS.with(|u| u.borrow().get(&id).map_or(default, f))
    }

    unsafe extern "C" fn unit_def(_ai: c_int, id: c_int) -> c_int {
        with_unit(id, |u| u.def, -1)
    }

    unsafe extern "C" fn unit_team(_ai: c_int, id: c_int) -> c_int {
        with_unit(id, |u| u.team, -1)
    }

    unsafe extern "C" fn unit_health(_ai: c_int, id: c_int) -> f32 {
        with_unit(id, |u| u.health, 0.0)
    }

    unsafe extern "C" fn unit_float(_ai: c_int, _id: c_int) -> f32 {
        1.0
    }

    unsafe extern "C" fn unit_pos(_ai: c_int, id: c_int, out: *mut f32) {
        let p = with_unit(id, |u| u.pos, Vec3::ZERO);
        unsafe {
            *out = p.x;
            *out.add(1) = p.y;
            *out.add(2) = p.z;
        }
    }

    pub fn callback() -> Callback {
        let table = SSkirmishAICallback {
            Engine_handleCommand: Some(handle_command),
            getUnitDefs: Some(get_unit_defs),
            UnitDef_getName: Some(def_name),
            UnitDef_getWeaponMounts: Some(zero_count),
            Unit_getDef: Some(unit_def),
            Unit_getTeam: Some(unit_team),
            Unit_getHealth: Some(unit_health),
            Unit_getMaxHealth: Some(unit_float),
            Unit_getMaxSpeed: Some(unit_float),
            Unit_getSpeed: Some(unit_float),
            UnitDef_getSpeed: Some(unit_float),
            Unit_getWeapons: Some(zero_count),
            Unit_getPos: Some(unit_pos),
            ..Default::default()
        };
        // SAFETY: the table is leaked, so it outlives every handle.
        unsafe { Callback::from_raw(0, Box::leak(Box::new(table))) }
    }

    /// Register a live unit of ours with the def at `def` in [`DEF_NAMES`].
    pub fn spawn(cb: Callback, id: i32, def: i32) -> Unit {
        UNITS.with(|u| {
            u.borrow_mut().insert(id, FakeUnit { def, pos: Vec3::xz(100.0, 100.0), health: 100.0, team: TEAM })
        });
        Unit::from_id(cb, id).expect("valid id")
    }

    pub fn kill(id: i32) {
        UNITS.with(|u| u.borrow_mut().get_mut(&id).map(|u| u.health = 0.0));
    }

    pub fn take_orders() -> Vec<Order> {
        ORDERS.with(|o| std::mem::take(&mut *o.borrow_mut()))
    }
}

#[cfg(test)]
mod tests {
    use super::fake_engine::{self, TEAM, spawn, take_orders};
    use super::*;
    use crate::military::{Fighter, ScoutTask};
    use springai::ffi::CommandTopic;

    const FELON: i32 = 0;
    const ASPIS: i32 = 1;
    const THUG: i32 = 3;
    const GLAIVE: i32 = 5;
    const IMP: i32 = 6;
    const ZEUS: i32 = 7;

    fn setup() -> (springai::Callback, Defs, FighterStore) {
        let cb = fake_engine::callback();
        (cb, Defs::new(cb), FighterStore::new())
    }

    fn add(cb: springai::Callback, fighters: &mut FighterStore, id: i32, def: i32, metal: f32) {
        fighters.insert(id, Fighter::new(spawn(cb, id, def), metal, TEAM));
    }

    #[test]
    fn assault_squad_bookkeeping() {
        let (cb, defs, mut fighters) = setup();
        let mut s = Squad::new(1, SquadKind::Assault, TEAM, &defs);
        for (id, metal) in [(10, 300.0), (11, 400.0), (12, 500.0)] {
            add(cb, &mut fighters, id, ZEUS, metal);
            s.add_unit(id, &mut fighters, 0);
        }
        assert_eq!(s.leader, Some(10));
        assert_eq!(s.metal_value, 1200.0);
        assert_eq!(fighters[&12].index, 2);
        assert!(fighters.values().all(|f| f.squad == Some(1)));
        let move_states = take_orders().iter().filter(|o| o.1 == CommandTopic::COMMAND_UNIT_SET_MOVE_STATE).count();
        assert_eq!(move_states, 3);

        s.remove_unit(10, &mut fighters, 0);
        assert_eq!(s.leader, Some(11));
        assert_eq!(s.metal_value, 900.0);
        assert_eq!(fighters[&10].squad, None);
        // Removing a non-member changes nothing.
        s.remove_unit(10, &mut fighters, 0);
        assert_eq!(s.metal_value, 900.0);

        // Too small to be useful: dissolved.
        s.cutoff(&mut fighters);
        assert!(s.is_dead(&fighters));
        assert_eq!(s.leader, None);
        assert_eq!(fighters[&11].squad, None);
    }

    #[test]
    fn best_leader_prefers_slow_units_near_the_leader() {
        let (cb, defs, mut fighters) = setup();
        let mut s = Squad::new(1, SquadKind::Assault, TEAM, &defs);
        for id in [20, 21] {
            add(cb, &mut fighters, id, ZEUS, 100.0);
            s.add_unit(id, &mut fighters, 0);
        }
        // Current speeds equal the def speed: nobody is slower, keep the leader.
        s.select_leader(&fighters);
        assert_eq!(s.leader, Some(20));
        // A dead leader is replaced, here by the earliest member (still the
        // dead unit, until it is removed from the squad).
        fake_engine::kill(20);
        s.select_leader(&fighters);
        assert_eq!(s.leader, Some(20));
        s.remove_unit(20, &mut fighters, 0);
        s.select_leader(&fighters);
        assert_eq!(s.leader, Some(21));
        assert!(!s.is_dead(&fighters));
    }

    #[test]
    fn shield_squad_leadership() {
        let (cb, defs, mut fighters) = setup();
        let mut s = Squad::new(1, SquadKind::Shield, TEAM, &defs);
        add(cb, &mut fighters, 30, FELON, 600.0);
        add(cb, &mut fighters, 31, THUG, 300.0);
        add(cb, &mut fighters, 32, ASPIS, 600.0);
        for id in [30, 31, 32] {
            s.add_unit(id, &mut fighters, 0);
        }
        // The felon leads and is not among the orbiting members.
        assert_eq!(s.leader, Some(30));
        assert_eq!(s.fighters, vec![31, 32]);
        assert_eq!((s.num_felons, s.num_aspis), (1, 1));
        assert_eq!(s.metal_value, 1500.0);
        let orbits = take_orders().iter().filter(|o| o.2 == cmd::ORBIT).count();
        assert_eq!(orbits, 2);

        // Losing the felon promotes the lowest-weight member.
        s.target = Vec3::xz(1.0, 1.0);
        s.remove_unit(30, &mut fighters, 0);
        assert_eq!(s.leader, Some(31));
        assert_eq!(s.fighters, vec![32]);
        assert_eq!(s.num_felons, 0);
        assert_eq!(s.target, NULLPOS);

        // A new felon outweighs the promoted thug and takes over.
        add(cb, &mut fighters, 33, FELON, 600.0);
        s.add_unit(33, &mut fighters, 0);
        assert_eq!(s.leader, Some(33));
        assert_eq!(s.fighters, vec![32, 31]);
        assert_eq!(s.num_felons, 1);

        s.remove_unit(32, &mut fighters, 0);
        assert_eq!(s.num_aspis, 0);
        assert!(!s.is_dead(&fighters));

        let members = s.disband(&mut fighters);
        assert_eq!(members, vec![31, 33]);
        assert!(s.is_dead(&fighters));
        assert!(members.iter().all(|id| fighters[id].squad.is_none()));
    }

    #[test]
    fn raider_tasks_stay_consistent() {
        let (cb, _defs, mut fighters) = setup();
        let mut tasks = ScoutTasks::new();
        tasks.insert(1, ScoutTask::new(1, Vec3::ZERO, 0));
        tasks.insert(2, ScoutTask::new(2, Vec3::ZERO, 1));
        fighters.insert(40, Fighter::new_raider(spawn(cb, 40, GLAIVE), 65.0, TEAM));
        let r = fighters.get_mut(&40).unwrap();

        r.set_task(1, &mut tasks);
        assert!(tasks[&1].has_raider(40));
        r.set_task(2, &mut tasks);
        assert!(!tasks[&1].has_raider(40) && tasks[&2].has_raider(40));
        assert_eq!(r.task, Some(2));

        take_orders();
        r.clear_task(&mut tasks);
        assert_eq!(r.task, None);
        assert_eq!(tasks[&2].count_raiders(), 0);
        assert_eq!(take_orders(), vec![(40, CommandTopic::COMMAND_UNIT_STOP, -1)]);

        // Ending a task does not touch the task list.
        r.set_task(1, &mut tasks);
        r.end_task();
        assert_eq!(r.task, None);
        assert!(tasks[&1].has_raider(40));
    }

    #[test]
    fn raider_squad_leaders() {
        let (cb, defs, mut fighters) = setup();
        let mut tasks = ScoutTasks::new();
        tasks.insert(1, ScoutTask::new(1, Vec3::ZERO, 0));
        let mut s = Squad::new(1, SquadKind::Raider, TEAM, &defs);
        for (id, def) in [(50, GLAIVE), (51, IMP), (52, GLAIVE)] {
            fighters.insert(id, Fighter::new_raider(spawn(cb, id, def), 65.0, TEAM));
            s.add_unit(id, &mut fighters, 0);
        }
        assert_eq!(s.leader, Some(50));
        fighters.get_mut(&50).unwrap().set_task(1, &mut tasks);

        // The earliest remaining member leads, even a bomb (see get_new_leader).
        s.remove_raider(50, &mut fighters, 0, &mut tasks);
        assert_eq!(s.leader, Some(51));
        assert_eq!(fighters[&50].task, None);
        assert_eq!(tasks[&1].count_raiders(), 0);
        assert_eq!(fighters[&50].squad, None);

        // Without the task list the leaving leader keeps its task for the caller.
        fighters.get_mut(&51).unwrap().set_task(1, &mut tasks);
        take_orders();
        s.remove_unit(51, &mut fighters, 0);
        assert_eq!(s.leader, Some(52));
        assert_eq!(fighters[&51].task, Some(1));
        assert_eq!(take_orders(), vec![(51, CommandTopic::COMMAND_UNIT_STOP, -1)]);
        assert_eq!(s.get_size(), 1);
    }
}
