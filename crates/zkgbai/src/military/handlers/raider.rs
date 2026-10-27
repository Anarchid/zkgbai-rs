//! Raiders: scouting tasks, raider squads, solo raiders and sparrows.

use std::collections::{BTreeSet, VecDeque};

use springai::{Unit, Vec3};

use crate::Ai;
use crate::defs::Defs;
use crate::graph::MetalSpot;
use crate::kgbutil::kgb_move_to;
use crate::military::sweeps::DETONATE_RANGE;
use crate::military::{Enemy, Fighter, MilitaryManager, ScoutTask, ScoutTaskId, ScoutTasks, SquadId, SquadKind};
use crate::nav;
use crate::util::{NULLPOS, distance};

// Slots of the squads currently being formed, per raider type (the
// original's `nextGlaiveSquad` etc.).
const GLAIVE: usize = 0;
const BANDIT: usize = 1;
const AMPH: usize = 2;
const DAGGER: usize = 3;
const HUNTER: usize = 4;
const SCORCHER: usize = 5;
const SCYTHE: usize = 6;
const HALBERD: usize = 7;
const PANTHER: usize = 8;
const SUB: usize = 9;
const PYRO: usize = 10;
const NUM_SQUAD_SLOTS: usize = 11;

#[derive(Default)]
pub struct RaiderHandler {
    /// Fighter-store ids.
    pub solo_raiders: VecDeque<i32>,
    /// Fighter-store ids.
    pub kodachis: Vec<i32>,
    /// Fighter-store ids.
    pub small_raiders: BTreeSet<i32>,
    /// Fighter-store ids.
    pub medium_raiders: BTreeSet<i32>,
    pub raider_squads: VecDeque<SquadId>,
    pub scout_tasks: ScoutTasks,
    /// Fighter-store id of the scout plane.
    sparrow: Option<i32>,
    pub(crate) next_squads: [Option<SquadId>; NUM_SQUAD_SLOTS],
    /// The original's `scoutTasks`: used by raider squads and the sparrow.
    scout_task_ids: Vec<ScoutTaskId>,
    /// The original's `soloScoutTasks`: the subset reachable by solo raiders.
    /// May hold a different task object for the same spot than
    /// `scout_task_ids` (as in the original).
    solo_scout_task_ids: Vec<ScoutTaskId>,
    next_task_id: ScoutTaskId,
}

impl RaiderHandler {
    pub fn new() -> Self {
        Self::default()
    }
}

/// What a solo raider or kodachi decided to do.
enum SoloOrder {
    /// Attack a position (`porc`: the target is static, so sneak up on it).
    Target {
        pos: Vec3,
        porc: bool,
    },
    Task(ScoutTaskId),
    Rally,
}

/// The original `getScoutCost`: how attractive scouting `task` is for the
/// raider `raider` at `pos`.
fn scout_cost(task: &ScoutTask, spots: &[MetalSpot], frame: i32, raider: i32, pos: Vec3) -> f32 {
    let Some(spot) = spots.get(task.spot) else {
        return f32::INFINITY;
    };
    let mut cost = distance(task.target, pos);
    let since_seen = frame as f32 - spot.get_last_seen() as f32;
    if spot.hostile {
        cost /= 3.0 + (since_seen / 900.0).max(1.0);
    } else {
        cost /= (if spot.enemy_shadowed { 2.0f32 } else { 1.0 }).max(since_seen / 900.0);
        cost += (4.0 * (900.0 - since_seen)).max(0.0);

        // Disprefer scouting the same non-hostile spot with more than one raider.
        if task.count_raiders() > 0 && !task.has_raider(raider) {
            cost += 2000.0;
        }
    }
    cost
}

/// The original `getSparrowCost`.
fn sparrow_cost(task: &ScoutTask, spots: &[MetalSpot], frame: i32, raider: i32, pos: Vec3) -> f32 {
    let Some(spot) = spots.get(task.spot) else {
        return f32::INFINITY;
    };
    let since_seen = frame as f32 - spot.get_last_seen() as f32;
    let mut cost = distance(task.target, pos);
    cost /= (since_seen / 900.0).max(1.0);
    cost += (4.0 * (900.0 - since_seen)).max(0.0);

    // Disprefer scouting the same non-hostile spot with more than one raider.
    if task.count_raiders() > 0 && !task.has_raider(raider) {
        cost += 2000.0;
    }
    cost
}

/// Cost of raiding enemy `e` from `pos`, where `ethreat` is the threat at the
/// enemy (shared by solo raiders and raider squads).
fn raid_cost(war: &MilitaryManager, defs: &Defs, e: &Enemy, pos: Vec3, ethreat: f32) -> f32 {
    let mut cost = distance(pos, e.position);
    let ename = if e.identified { e.ud.map(|d| defs.name(d)).unwrap_or("") } else { "" };
    if !e.is_arty && !e.is_worker && !e.is_static && (!e.is_aa || ename == "turretaalaser") {
        cost += 1250.0;
        if e.is_riot {
            cost *= 2.0;
        }
    } else if e.is_porc {
        if e.unit.is_being_built() {
            cost -= 1000.0 * (1.0 - e.unit.build_progress());
            cost += 1500.0 * ethreat;
        } else {
            let mut value_mod = war.get_raider_value(e.position).ln();
            value_mod *= value_mod;
            value_mod -= 10.0 * ethreat;
            cost /= value_mod.max(1.0);
        }
    } else if e.is_com {
        cost -= 250.0;
    } else if e.is_worker {
        cost = (cost / 2.0) - (500.0 + (500.0 * (1.0 - e.get_health())));
        cost += 2500.0 * ethreat;
    } else if e.is_static {
        cost = (cost / 2.0) - 750.0;
        cost += 2500.0 * ethreat;
    } else {
        cost += 750.0 * ethreat;
    }
    cost
}

impl Ai {
    pub fn raider_add_sparrow(&mut self, r: Fighter) {
        if let Some(old) = self.raider_handler.sparrow.take() {
            self.raider_clear_task(old);
            self.raider_drop_fighter(old);
        }
        self.raider_handler.sparrow = Some(r.id);
        self.fighters.insert(r.id, r);
    }

    pub fn raider_add_small_raider(&mut self, r: Fighter) {
        let id = r.id;
        let h = &mut self.raider_handler;
        if h.small_raiders.contains(&id) || h.kodachis.contains(&id) || h.solo_raiders.contains(&id) {
            return;
        }
        h.small_raiders.insert(id);
        self.fighters.insert(id, r);
        self.raider_assign_small_raider(id);
    }

    pub fn raider_add_medium_raider(&mut self, r: Fighter) {
        let id = r.id;
        let h = &mut self.raider_handler;
        if h.medium_raiders.contains(&id) || h.kodachis.contains(&id) || h.solo_raiders.contains(&id) {
            return;
        }
        h.medium_raiders.insert(id);
        self.fighters.insert(id, r);
        self.raider_assign_medium_raider(id);
    }

    pub fn raider_add_solo_raider(&mut self, r: Fighter) {
        let id = r.id;
        let h = &mut self.raider_handler;
        if self.defs.name_of(r.unit) == "tankraid" {
            if h.kodachis.contains(&id) {
                return;
            }
            h.kodachis.push(id);
        } else {
            if h.small_raiders.contains(&id) || h.solo_raiders.contains(&id) {
                return;
            }
            h.solo_raiders.push_back(id);
        }
        self.fighters.insert(id, r);
    }

    /// Take raiders in as one squad of their own, ready to raid (the
    /// staged raid game's raids, see `raid_test`).
    pub(crate) fn raider_add_squad(&mut self, raiders: Vec<Fighter>, raider_type: char) {
        let sid = self.squads.create(SquadKind::Raider, self.team_id);
        if let Some(sq) = self.squads.get_mut(sid) {
            sq.raider_type = raider_type;
        }
        self.raider_handler.raider_squads.push_back(sid);
        let frame = self.frame;
        for r in raiders {
            let id = r.id;
            if raider_type == 's' {
                self.raider_handler.small_raiders.insert(id);
            } else {
                self.raider_handler.medium_raiders.insert(id);
            }
            self.fighters.insert(id, r);
            if let Some(sq) = self.squads.get_mut(sid) {
                sq.add_unit(id, &mut self.fighters, frame);
            }
        }
        if let Some(sq) = self.squads.get_mut(sid) {
            sq.status = 'r';
        }
    }

    pub fn raider_remove_unit(&mut self, u: Unit) {
        let id = u.id();
        let h = &mut self.raider_handler;
        if let Some(i) = h.solo_raiders.iter().position(|&r| r == id) {
            h.solo_raiders.remove(i);
            self.raider_clear_task(id);
            self.raider_drop_fighter(id);
            return;
        }
        if let Some(i) = h.kodachis.iter().position(|&r| r == id) {
            h.kodachis.remove(i);
            self.raider_clear_task(id);
            self.raider_drop_fighter(id);
            return;
        }
        if h.small_raiders.remove(&id) || h.medium_raiders.remove(&id) {
            self.raider_discard(id);
        }
        // port: the original never released a dead sparrow; with a shared
        // fighter store its id could be reused by another unit.
        if self.raider_handler.sparrow == Some(id) {
            self.raider_handler.sparrow = None;
            self.raider_clear_task(id);
            self.raider_drop_fighter(id);
        }
        if self.sweeps.detonating == Some(id) {
            self.sweeps.detonating = None;
        }
    }

    pub fn raider_remove_from_squad(&mut self, u: Unit) {
        let id = u.id();
        let h = &self.raider_handler;
        if h.small_raiders.contains(&id) || h.medium_raiders.contains(&id) {
            // The task is cleared first so that the squad cannot drop it
            // without leaving the task's raider list.
            self.raider_clear_task(id);
            self.raider_leave_squad(id);
        }
    }

    pub fn raider_update(&mut self, frame: i32) {
        let offset = self.offset;

        if frame % 15 == (offset + 4) % 15 {
            let small: Vec<i32> = self.raider_handler.small_raiders.iter().copied().collect();
            for id in small {
                if self.raider_is_dead(id) {
                    self.raider_handler.small_raiders.remove(&id);
                    self.raider_discard(id);
                    continue;
                }
                if self.raider_needs_squad(id) {
                    self.raider_assign_small_raider(id);
                }
            }

            let medium: Vec<i32> = self.raider_handler.medium_raiders.iter().copied().collect();
            for id in medium {
                if self.raider_is_dead(id) {
                    self.raider_handler.medium_raiders.remove(&id);
                    self.raider_discard(id);
                    continue;
                }
                if self.raider_needs_squad(id) {
                    self.raider_assign_medium_raider(id);
                }
            }

            let solo: Vec<i32> = self.raider_handler.solo_raiders.iter().copied().collect();
            for id in solo {
                if self.raider_is_lost(id) {
                    self.raider_handler.solo_raiders.retain(|&r| r != id);
                    self.raider_clear_task(id);
                    self.raider_drop_fighter(id);
                }
            }

            let kodachis = self.raider_handler.kodachis.clone();
            for id in kodachis {
                if self.raider_is_lost(id) {
                    self.raider_handler.kodachis.retain(|&r| r != id);
                    self.raider_clear_task(id);
                    self.raider_drop_fighter(id);
                }
            }

            let sparrow = self
                .raider_handler
                .sparrow
                .filter(|s| self.fighters.get(s).is_some_and(|f| !f.is_dead() && !self.retreat_is_retreating(f.unit)));
            if let Some(s) = sparrow {
                self.raider_assign_sparrow(s);
            }

            self.raider_create_scout_tasks();
            self.raider_check_scout_tasks();

            self.raider_super_kodachi_rally();
        }

        if frame % 3 == offset % 3 {
            self.raider_assign_raider_squads();
        }

        if frame % 2 == offset % 2 {
            self.raider_assign_solo_raiders();
        }

        // Unstick raiders that are caught on buildings or wrecks.
        if frame % 30 == (offset + 1) % 30 {
            let small: Vec<i32> = self.raider_handler.small_raiders.iter().copied().collect();
            for id in small {
                if let Some(f) = self.fighters.get_mut(&id).filter(|f| !f.is_dead()) {
                    f.unstick(frame, &mut self.raider_handler.scout_tasks);
                }
            }
        }

        if frame % 30 == (offset + 5) % 30 {
            let medium: Vec<i32> = self.raider_handler.medium_raiders.iter().copied().collect();
            for id in medium {
                let Some(f) = self.fighters.get_mut(&id) else {
                    continue;
                };
                let attacking = f.squad.and_then(|s| self.squads.get(s)).is_some_and(|s| s.status == 'a');
                if !f.is_dead() && attacking && f.unstick(frame, &mut self.raider_handler.scout_tasks) {
                    // Too bad, so sad!
                    let unit = f.unit;
                    self.raider_handler.medium_raiders.remove(&id);
                    self.raider_discard(id);
                    self.eco_add_combat_reclaim_task(unit);
                    self.retreat_remove_unit(unit);
                }
            }
        }

        if frame % 30 == (offset + 8) % 30 {
            let mut ids: Vec<i32> = self.raider_handler.solo_raiders.iter().copied().collect();
            ids.extend(self.raider_handler.kodachis.iter().copied());
            // Raiders parked at a precision spot stand still on purpose.
            ids.retain(|id| !self.precision.missions.contains_key(id));
            for id in ids {
                if let Some(f) = self.fighters.get_mut(&id).filter(|f| !f.is_dead()) {
                    f.unstick(frame, &mut self.raider_handler.scout_tasks);
                }
            }
        }
    }

    /// Dodge away from an attacker that just damaged raider `h`.
    pub fn raider_avoid_enemies(&mut self, h: Unit, attacker: Option<Unit>, dir: Vec3) {
        let id = h.id();
        if !self.raider_handler.small_raiders.contains(&id) {
            return;
        }
        let frame = self.frame;
        let Some(r) = self.fighters.get_mut(&id) else {
            return;
        };
        if frame - r.last_rotation_frame < 5 {
            return;
        }

        let health = h.health();
        if health > 0.0
            && (health / h.max_health() < 0.8 || h.paralyze_damage() > 0.0 || h.capture_progress() > 0.0)
            && attacker.is_some_and(|a| a.max_speed() > 0.0)
        {
            r.last_rotation_frame = frame;
            let movdist = if self.defs.name_of(h).starts_with('h') || h.paralyze_damage() > 0.0 {
                -450.0 // for daggers
            } else {
                -100.0
            };
            let pos = h.pos();
            let target = Vec3::xz(pos.x + movdist * dir.x, pos.z + movdist * dir.z);
            kgb_move_to(h, target, 0, frame);
        }
    }

    // ── Bookkeeping ─────────────────────────────────────────────────────

    fn raider_is_dead(&self, id: i32) -> bool {
        self.fighters.get(&id).is_none_or(|f| f.is_dead())
    }

    /// Solo raiders and kodachis: dead or given away.
    fn raider_is_lost(&self, id: i32) -> bool {
        self.fighters.get(&id).is_none_or(|f| f.unit.health() <= 0.0 || f.unit.team() != self.team_id)
    }

    /// Squad-less, non-retreating squad raider.
    fn raider_needs_squad(&self, id: i32) -> bool {
        self.fighters.get(&id).is_some_and(|f| f.squad.is_none() && !self.retreat_is_retreating(f.unit))
    }

    /// Leave the current scout task and stop (the original `clearTask`).
    fn raider_clear_task(&mut self, id: i32) {
        if let Some(f) = self.fighters.get_mut(&id) {
            f.clear_task(&mut self.raider_handler.scout_tasks);
        }
    }

    /// Switch raider `id` to scout task `task` unless it already has it.
    fn raider_take_task(&mut self, id: i32, task: ScoutTaskId) {
        let tasks = &mut self.raider_handler.scout_tasks;
        let Some(f) = self.fighters.get_mut(&id) else {
            return;
        };
        if f.task != Some(task) {
            f.clear_task(tasks);
            if let Some(t) = tasks.get_mut(&task) {
                t.add_raider(id);
            }
            f.set_task(task, tasks);
        }
    }

    fn raider_leave_squad(&mut self, id: i32) {
        let frame = self.frame;
        let squad = self.fighters.get(&id).and_then(|f| f.squad);
        if let Some(sq) = squad.and_then(|sid| self.squads.get_mut(sid)) {
            sq.remove_unit(id, &mut self.fighters, frame);
        }
    }

    /// Release a squad raider (already removed from the handler's sets).
    fn raider_discard(&mut self, id: i32) {
        self.raider_clear_task(id);
        self.raider_leave_squad(id);
        self.raider_drop_fighter(id);
    }

    /// Remove a fighter from the store, and from any task still listing it.
    fn raider_drop_fighter(&mut self, id: i32) {
        for t in self.raider_handler.scout_tasks.values_mut() {
            t.remove_raider(id);
        }
        self.fighters.remove(&id);
    }

    fn raider_sneak(&mut self, id: i32, pos: Vec3) {
        let mut nav = nav!(self);
        if let Some(f) = self.fighters.get_mut(&id) {
            f.sneak(pos, &mut nav);
        }
    }

    fn raider_raid(&mut self, id: i32, pos: Vec3) {
        let mut nav = nav!(self);
        if let Some(f) = self.fighters.get_mut(&id) {
            f.raid(pos, &mut nav);
        }
    }

    fn raider_squad_sneak(&mut self, sid: SquadId, pos: Vec3) {
        let mut nav = nav!(self);
        if let Some(sq) = self.squads.get_mut(sid) {
            sq.sneak(pos, &mut self.fighters, &mut nav);
        }
    }

    pub(crate) fn raider_squad_raid(&mut self, sid: SquadId, pos: Vec3) {
        let mut nav = nav!(self);
        if let Some(sq) = self.squads.get_mut(sid) {
            sq.raid(pos, &mut self.fighters, &mut nav);
        }
    }

    // ── Squad formation ─────────────────────────────────────────────────

    fn raider_assign_small_raider(&mut self, id: i32) {
        let Some(unit) = self.fighters.get(&id).map(|f| f.unit) else {
            return;
        };
        let inc = (self.eco.adjusted_income / 5.0).floor() as i32;
        // glaive, bandit, duck/archer, dagger, hunter
        let (slot, size) = match self.defs.name_of(unit) {
            "cloakraid" => (GLAIVE, 12.min(6.max(inc))),
            "shieldraid" => (BANDIT, 12.min(6.max(inc))),
            n if n.starts_with("amph") => (AMPH, 12.min(6.max(inc))),
            "hoverraid" => (DAGGER, 10.min(5.max(inc))),
            "shiptorpraider" => (HUNTER, 9.min(4.max(inc))),
            _ => return,
        };
        self.raider_join_next_squad(slot, id, 's', size);
    }

    fn raider_assign_medium_raider(&mut self, id: i32) {
        let Some(unit) = self.fighters.get(&id).map(|f| f.unit) else {
            return;
        };
        let income = self.eco.adjusted_income;
        let (slot, size) = match self.defs.name_of(unit) {
            "vehraid" => (SCORCHER, 5.min(4 + (income / 25.0).floor() as i32)),
            "hoverassault" => (HALBERD, 8.min(2 + (income / 15.0).floor() as i32)),
            "cloakheavyraid" => (SCYTHE, 4.min(2.max((income / 10.0).floor() as i32))),
            "tankheavyraid" => (PANTHER, 8.min(4.max((income / 5.0).floor() as i32))),
            "subraider" => (SUB, 8.min(4.max((income / 5.0).floor() as i32))),
            "jumpraid" => (PYRO, 6.min(3.max((income / 10.0).floor() as i32))),
            _ => return,
        };
        self.raider_join_next_squad(slot, id, 'm', size);
    }

    /// Add raider `id` to the forming squad in `slot` (creating it with
    /// raider class `raider_type` if needed); the squad starts rallying
    /// once it has `size` members.
    fn raider_join_next_squad(&mut self, slot: usize, id: i32, raider_type: char, size: i32) {
        let sid = match self.raider_handler.next_squads[slot] {
            Some(sid) if self.squads.contains(sid) => sid,
            _ => {
                let sid = self.squads.create(SquadKind::Raider, self.team_id);
                if let Some(sq) = self.squads.get_mut(sid) {
                    sq.raider_type = raider_type;
                }
                self.raider_handler.raider_squads.push_back(sid);
                self.raider_handler.next_squads[slot] = Some(sid);
                sid
            }
        };
        let frame = self.frame;
        let Some(sq) = self.squads.get_mut(sid) else {
            return;
        };
        sq.add_unit(id, &mut self.fighters, frame);
        if sq.fighters.len() as i32 >= size {
            sq.status = 'r';
            self.raider_handler.next_squads[slot] = None;
        }
    }

    // ── Scout tasks ─────────────────────────────────────────────────────

    fn raider_create_scout_tasks(&mut self) {
        let h = &mut self.raider_handler;
        let mut unscouted: Vec<usize> = self.graph.get_enemy_territory().iter().map(|s| s.index).collect();
        if unscouted.is_empty() && h.scout_task_ids.is_empty() {
            // If enemy territory is not known, get all spots not in our own territory.
            unscouted = self.graph.get_unowned_spots().iter().map(|s| s.index).collect();
        }

        for spot in unscouted {
            let Some(target) = self.graph.metal_spots.get(spot).map(|s| s.get_pos()) else {
                continue;
            };
            let tasks = &h.scout_tasks;
            let same = |tid: &ScoutTaskId| tasks.get(tid).is_some_and(|t| t.spot == spot && t.target == target);
            let in_scout = h.scout_task_ids.iter().any(same);
            let in_solo = h.solo_scout_task_ids.iter().any(same);
            if in_scout && in_solo {
                continue;
            }
            h.next_task_id += 1;
            let tid = h.next_task_id;
            h.scout_tasks.insert(tid, ScoutTask::new(tid, target, spot));
            if !in_scout {
                h.scout_task_ids.push(tid);
            }
            if !in_solo {
                h.solo_scout_task_ids.push(tid);
            }
        }
    }

    fn raider_check_scout_tasks(&mut self) {
        // Only soloScoutTasks do pathfinding.
        let no_enemy_territory = self.graph.get_enemy_territory().is_empty();
        let spots = &self.graph.metal_spots;
        let h = &self.raider_handler;
        let finished: Vec<ScoutTaskId> = h
            .scout_task_ids
            .iter()
            .copied()
            .filter(|tid| {
                let spot = h.scout_tasks.get(tid).and_then(|t| spots.get(t.spot));
                match spot {
                    None => true,
                    Some(s) if no_enemy_territory => s.visible,
                    Some(s) => !s.hostile && !s.enemy_shadowed,
                }
            })
            .collect();

        for tid in finished {
            self.raider_end_scout_task(tid);
            let h = &mut self.raider_handler;
            let key = h.scout_tasks.get(&tid).map(|t| (t.spot, t.target));
            h.scout_task_ids.retain(|&t| t != tid);
            // port: the C++ erased only the identical task pointer from the
            // solo list; the Java removed every equal task (same spot).
            let tasks = &h.scout_tasks;
            h.solo_scout_task_ids.retain(|t| t != &tid && tasks.get(t).map(|t| (t.spot, t.target)) != key);
        }

        if !no_enemy_territory {
            // Use the raider with the highest individual threat for maxthreat and the raider with the greatest
            // all-terrain mobility for determining reachability, just to be conservative.
            let mut scout: Option<Unit> = None;
            let mut max_threat = 0.0f32;
            let mut max_slope = 0.0f32;
            let h = &self.raider_handler;
            for id in h.solo_raiders.iter().chain(h.kodachis.iter()) {
                let Some(ra) = self.fighters.get(id) else {
                    continue;
                };
                if ra.is_dead() {
                    continue;
                }
                let u = ra.unit;
                let tmpthreat = ((u.power() + u.max_health()) / 14.0) / 500.0;
                if tmpthreat > max_threat {
                    max_threat = tmpthreat;
                }
                if max_slope == 2.0 {
                    continue;
                }
                let Some(def) = u.def() else {
                    continue;
                };
                if let Some(md) = def.move_data() {
                    let tmp_slope = md.max_slope();
                    if tmp_slope > max_slope {
                        max_slope = tmp_slope;
                        scout = Some(u);
                    }
                } else if u.health() > 0.0 {
                    max_slope = 2.0;
                    scout = Some(u);
                }
            }

            if let Some(r) = scout {
                let frame = self.frame;
                // One flood answers every task's reachability check at once.
                let reach = self.pathfinder.raider_reachability(&self.war, r, max_threat);
                let h = &mut self.raider_handler;
                let tasks = &mut h.scout_tasks;
                h.solo_scout_task_ids.retain(|tid| {
                    let Some(st) = tasks.get_mut(tid) else {
                        return false;
                    };
                    if !reach.reaches(st.target) {
                        return false;
                    }
                    st.last_path_check_frame = frame;
                    true
                });
            }
        }

        self.raider_gc_scout_tasks();
    }

    /// The original `ScoutTask::endTask`: every assigned raider forgets the
    /// task and stops.
    fn raider_end_scout_task(&mut self, tid: ScoutTaskId) {
        let Some(task) = self.raider_handler.scout_tasks.get(&tid) else {
            return;
        };
        for fid in &task.assigned_raiders {
            if let Some(f) = self.fighters.get_mut(fid).filter(|f| f.task == Some(tid)) {
                f.end_task();
            }
        }
    }

    /// Drop tasks that are in neither list. Raiders still holding one just
    /// forget it (the original kept a dangling reference until reassigned).
    fn raider_gc_scout_tasks(&mut self) {
        let h = &mut self.raider_handler;
        let orphans: Vec<ScoutTaskId> = h
            .scout_tasks
            .keys()
            .filter(|t| !h.scout_task_ids.contains(t) && !h.solo_scout_task_ids.contains(t))
            .copied()
            .collect();
        for tid in orphans {
            let Some(task) = h.scout_tasks.remove(&tid) else {
                continue;
            };
            for fid in task.assigned_raiders {
                if let Some(f) = self.fighters.get_mut(&fid).filter(|f| f.task == Some(tid)) {
                    f.task = None;
                }
            }
        }
    }

    // ── Assignment ──────────────────────────────────────────────────────

    fn raider_assign_raider_squads(&mut self) {
        let frame = self.frame;
        while let Some(sid) = self.raider_handler.raider_squads.pop_front() {
            let Some(rs) = self.squads.get(sid) else {
                continue;
            };
            if rs.is_dead(&self.fighters) {
                if rs.status == 'f' {
                    self.raider_handler.raider_squads.push_back(sid);
                    return;
                }
                self.squads.remove(sid);
                for slot in self.raider_handler.next_squads.iter_mut() {
                    if *slot == Some(sid) {
                        *slot = None;
                    }
                }
                continue;
            } else if frame - rs.last_assignment_frame < 15 {
                self.raider_handler.raider_squads.push_front(sid);
                return;
            }
            self.raider_assign_raider_squad(sid);
            if let Some(rs) = self.squads.get_mut(sid) {
                rs.last_assignment_frame = frame;
            }
            self.raider_handler.raider_squads.push_back(sid);
            return;
        }
    }

    fn raider_assign_solo_raiders(&mut self) {
        // Assign up to one solo raider per frame, with each being assigned up to twice per second.
        let frame = self.frame;
        while let Some(id) = self.raider_handler.solo_raiders.pop_front() {
            if self.raider_is_dead(id) {
                self.raider_clear_task(id);
                self.raider_drop_fighter(id);
                continue;
            }
            let Some(f) = self.fighters.get(&id) else {
                continue;
            };
            if f.unit.is_being_built() || self.retreat_is_retreating(f.unit) {
                self.raider_handler.solo_raiders.push_back(id);
            } else if frame - f.last_task_frame < 15 {
                self.raider_handler.solo_raiders.push_front(id);
            } else {
                self.raider_assign_solo_raider(id);
                if let Some(f) = self.fighters.get_mut(&id) {
                    f.last_task_frame = frame;
                }
                self.raider_handler.solo_raiders.push_back(id);
            }
            return;
        }
    }

    fn raider_assign_solo_raider(&mut self, id: i32) {
        if self.precision_step(id) {
            return;
        }
        let Some(r) = self.fighters.get(&id) else {
            return;
        };
        let (unit, pos, rtarget) = (r.unit, r.get_pos(), r.target);
        let war = &self.war;
        // The threat maps as the raider sees them, under water too (see `water`).
        let view = war.view(unit.def());
        let raider_threat = view.get_friendly_raider_threat(pos);
        let over_threat = view.get_threat(pos) * (1.0 + view.get_riot_threat(pos)) > raider_threat
            || (rtarget != NULLPOS && view.get_threat(rtarget) > raider_threat);
        if over_threat {
            // Try to keep raiders from suiciding.
            let haven = self.graph_get_closest_raider_haven(pos);
            self.raider_sneak(id, haven);
            return;
        }

        let name = self.defs.name_of(unit);
        let is_spider = name.starts_with("spider");
        let is_blastwing = name == "gunshipbomb";

        let mut best_task = None;
        let mut best_target = NULLPOS;
        // port: the C++ started from 65535, which ignores far targets; the
        // Java used Float.MAX_VALUE.
        let mut cost = f32::MAX;
        let mut porc = true;

        if self.raider_handler.raider_squads.is_empty()
            && self.squad_handler.fighters.is_empty()
            && self.fac.early_worker
            && !is_spider
        {
            best_target = self.war_get_raider_rally(pos);
            if distance(best_target, pos) > 500.0 {
                self.raider_sneak(id, best_target);
            } else {
                self.raider_raid(id, best_target);
            }
        }

        let war = &self.war;
        let view = war.view(unit.def());
        let h = &self.raider_handler;
        let friendly_threat = view.get_friendly_threat(pos);
        // Where the terrain lets the raider get in range at all.
        let terrain = if self.dev_off("reach") { None } else { self.terrain.get() };
        let engage = crate::terrain::engage_test(terrain, unit);
        let hits = war.reach(unit.def());
        for &tid in &h.solo_scout_task_ids {
            let Some(s) = h.scout_tasks.get(&tid) else {
                continue;
            };
            if view.get_riot_threat(s.target) > 0.0
                || view.get_effective_threat(s.target) > friendly_threat
                || !engage(s.target)
            {
                continue;
            }
            let tmpcost = scout_cost(s, &self.graph.metal_spots, self.frame, id, pos);
            if tmpcost < cost {
                cost = tmpcost;
                best_task = Some(tid);
            }
        }

        for d in &war.sniper_sightings {
            if view.get_riot_threat(d.position) > 0.0 || !engage(d.position) {
                continue;
            }
            let tmpcost = distance(pos, d.position) / 4.0 - 100.0;
            if tmpcost < cost {
                cost = tmpcost;
                best_target = d.position;
            }
        }

        if !is_blastwing {
            for e in war.targets.values() {
                if e.is_riot
                    || view.get_riot_threat(e.position) > 0.0
                    || view.get_effective_threat(e.position) > friendly_threat
                    || (!e.is_porc && view.get_porc_threat(e.position) > 0.0)
                    || !engage(e.position)
                    || !war.can_hit(hits, e)
                {
                    continue;
                }
                let ethreat = view.get_threat(e.position);
                let tmpcost = raid_cost(war, &self.defs, e, pos, ethreat);
                if tmpcost < cost {
                    cost = tmpcost;
                    best_target = e.position;
                    porc = e.is_static;
                }
            }
        }

        drop(engage);
        let order = if best_target != NULLPOS {
            SoloOrder::Target { pos: best_target, porc }
        } else if let Some(tid) = best_task {
            SoloOrder::Task(tid)
        } else {
            SoloOrder::Rally
        };
        self.raider_apply_solo_order(id, order);
    }

    fn raider_apply_solo_order(&mut self, id: i32, order: SoloOrder) {
        let Some(pos) = self.fighters.get(&id).map(|f| f.get_pos()) else {
            return;
        };
        match order {
            SoloOrder::Target { pos: target, porc } => {
                self.raider_clear_task(id);
                if distance(target, pos) > 500.0 || porc {
                    self.raider_sneak(id, target);
                } else {
                    self.raider_raid(id, target);
                }
            }
            SoloOrder::Task(tid) => {
                let Some(target) = self.raider_handler.scout_tasks.get(&tid).map(|t| t.target) else {
                    return;
                };
                self.raider_take_task(id, tid);
                if distance(target, pos) > 500.0 {
                    self.raider_sneak(id, target);
                } else {
                    self.raider_raid(id, target);
                }
            }
            SoloOrder::Rally => {
                self.raider_clear_task(id);
                let rally = self.war_get_raider_rally(pos);
                self.raider_sneak(id, rally);
            }
        }
    }

    fn raider_assign_sparrow(&mut self, sparrow: i32) {
        let Some((unit, pos)) = self.fighters.get(&sparrow).map(|f| (f.unit, f.get_pos())) else {
            return;
        };
        if self.sweeps.detonating == Some(sparrow) {
            return;
        }
        // A Sparrow (not an Owl) sweeps where unseen snipers shoot from, and
        // whether anti-nukes we struck are gone: it flies at them and, near
        // enough, detonates.
        let sweeps = self.defs.name_of(unit) == "planelightscout";
        let sniper = if sweeps { self.sweeps_sniper_target(pos) } else { None };
        let sweep = sniper.map(|p| (p, true)).or_else(|| self.missiles_scout_target(pos).map(|p| (p, false)));
        if let Some((p, fight)) = sweep.filter(|_| sweeps) {
            if distance(pos, p) <= DETONATE_RANGE {
                self.sweeps_detonate(unit, p, fight);
            } else {
                self.raider_clear_task(sparrow);
                self.raider_raid(sparrow, p);
            }
            return;
        }
        // With nukes to fire, find the anti-nukes that stopped them; see
        // whether anti-nukes we struck are gone.
        let look = if self.misc_has_nuke() { self.nukes_scout_target(pos) } else { None };
        if let Some(p) = look.or_else(|| self.missiles_scout_target(pos)) {
            self.raider_clear_task(sparrow);
            self.raider_raid(sparrow, p);
            return;
        }
        let mut cost = f32::MAX; // port: 65535 in the C++, see raider_assign_solo_raider
        let mut best_task = None;
        let mut reach = None;
        let h = &self.raider_handler;
        for &tid in &h.scout_task_ids {
            let Some(s) = h.scout_tasks.get(&tid) else {
                continue;
            };
            if self.war.get_aa_threat(s.target) > 0.0
                || !reach
                    .get_or_insert_with(|| self.pathfinder.raider_reachability(&self.war, unit, 0.0))
                    .reaches(s.target)
            {
                continue;
            }
            let tmpcost = sparrow_cost(s, &self.graph.metal_spots, self.frame, sparrow, pos);
            if tmpcost < cost {
                cost = tmpcost;
                best_task = Some(tid);
            }
        }

        let target = best_task.and_then(|tid| self.raider_handler.scout_tasks.get(&tid).map(|t| (tid, t.target)));
        if let Some((tid, target)) = target {
            self.raider_take_task(sparrow, tid);
            self.raider_raid(sparrow, target);
        } else {
            let link = self.graph_get_closest_leading_link(pos);
            self.raider_raid(sparrow, link);
        }
    }

    fn raider_super_kodachi_rally(&mut self) {
        let kodachis = self.raider_handler.kodachis.clone();
        for id in kodachis {
            let Some(r) = self.fighters.get(&id) else {
                continue;
            };
            let u = r.unit;
            if u.is_being_built() || u.health() <= 0.0 || u.team() != self.team_id || self.retreat_is_retreating(u) {
                continue;
            }

            if self.precision_step(id) {
                continue;
            }
            let Some(r) = self.fighters.get(&id) else {
                continue;
            };
            let reloading = r.is_reloading(self.frame);
            let (pos, rtarget) = (r.get_pos(), r.target);
            let max_threat = if reloading { 0.0 } else { 0.4 };
            let war = &self.war;
            let over_threat = war.get_effective_threat(pos) > 0.4
                || war.get_riot_threat(pos) > 0.0
                || (rtarget != NULLPOS && (war.get_riot_threat(rtarget) > 0.0 || war.get_threat(rtarget) > max_threat));
            if over_threat {
                // Try to keep raiders from suiciding.
                let haven = self.graph_get_closest_raider_haven(pos);
                self.raider_sneak(id, haven);
                continue;
            }

            let mut best_task = None;
            let mut best_target = NULLPOS;
            let mut cost = f32::MAX; // port: 65535 in the C++, see raider_assign_solo_raider
            let mut porc = true;

            let h = &self.raider_handler;
            for &tid in &h.solo_scout_task_ids {
                let Some(s) = h.scout_tasks.get(&tid) else {
                    continue;
                };
                if war.get_riot_threat(s.target) > 0.0 || war.get_effective_threat(s.target) > max_threat {
                    continue;
                }
                let tmpcost = scout_cost(s, &self.graph.metal_spots, self.frame, id, pos);
                if tmpcost < cost {
                    cost = tmpcost;
                    best_task = Some(tid);
                }
            }

            for d in &war.sniper_sightings {
                if war.get_riot_threat(d.position) > 0.0 {
                    continue;
                }
                let tmpcost = distance(pos, d.position) / 4.0 - 100.0;
                if tmpcost < cost {
                    cost = tmpcost;
                    best_target = d.position;
                }
            }

            if !reloading {
                for e in war.targets.values() {
                    if (!e.is_mex && war.get_threat(e.position) > 0.0)
                        || war.get_effective_threat(e.position) > 0.4
                        || war.get_riot_threat(e.position) > 0.0
                    {
                        continue;
                    }

                    let mut tmpcost = distance(pos, e.position);
                    if e.is_mex {
                        tmpcost = (tmpcost / 4.0) - 500.0;
                    }

                    if tmpcost < cost {
                        cost = tmpcost;
                        best_target = e.position;
                        porc = e.is_static;
                    }
                }
            }

            let order = if best_target != NULLPOS {
                SoloOrder::Target { pos: best_target, porc }
            } else if let Some(tid) = best_task {
                SoloOrder::Task(tid)
            } else {
                SoloOrder::Rally
            };
            self.raider_apply_solo_order(id, order);
        }
    }

    fn raider_assign_raider_squad(&mut self, sid: SquadId) {
        // Moonlighting at home against a raid (see `qrf::live`).
        if self.qrf_drafted(sid) {
            return;
        }
        // Sent somewhere by the operator.
        if let Some(target) = self.op_squad_target(sid) {
            self.raider_squad_raid(sid, target);
            return;
        }
        let Some(rs) = self.squads.get(sid) else {
            return;
        };
        let pos = rs.get_pos(&self.fighters);
        let (status, rs_target, raider_type, leader, courage) =
            (rs.status, rs.target, rs.raider_type, rs.leader, rs.courage);
        // The operator's courage (see `Squad::courage`).
        let tolerance = |friendly: f32| (courage - 1.0).max(0.0) * friendly;
        let members: Vec<(Vec3, bool)> = rs
            .fighters
            .iter()
            .filter_map(|id| self.fighters.get(id))
            .map(|f| (f.get_pos(), f.unit.is_cloaked()))
            .collect();
        let def = rs.fighters.iter().find_map(|id| self.fighters.get(id)).and_then(|f| f.unit.def());
        let war = &self.war;
        // The threat maps as the squad sees them, under water too (see `water`).
        let view = war.view(def);

        if status != 'a' {
            // Assign forming and rallying squads.
            let over_threat = (rs_target != NULLPOS
                && (view.get_porc_threat(rs_target) / courage > tolerance(war.available_mobile_threat)
                    || view.get_tactical_threat(rs_target) / courage > war.available_mobile_threat))
                || members.iter().any(|&(rpos, _)| {
                    view.get_threat(rpos) * (1.0 + view.get_riot_threat(rpos)) / courage
                        > view.get_total_friendly_threat(rpos)
                        || view.get_porc_threat(rpos) / courage > tolerance(view.get_total_friendly_threat(rpos))
                });
            if !over_threat {
                // Rallying squads may be sent to block an incursion's way.
                let rally = self.incursion_block(sid).unwrap_or_else(|| self.war_get_raider_rally(pos));
                self.raider_squad_raid(sid, rally);
            } else {
                let haven = self.graph_get_closest_haven(pos);
                self.raider_squad_sneak(sid, haven);
            }
            if status == 'r' {
                // Rally rallying squads.
                let mut nav = nav!(self);
                let Some(rs) = self.squads.get_mut(sid) else {
                    return;
                };
                if rs.is_rallied(&mut self.fighters, &mut nav) {
                    rs.status = 'a';
                    if raider_type == 's' {
                        let units: Vec<Unit> =
                            rs.fighters.iter().filter_map(|id| self.fighters.get(id)).map(|f| f.unit).collect();
                        for u in units {
                            self.retreat_remove_unit(u);
                        }
                    }
                }
            }
            return;
        }

        // Get targets for actively raiding squads.
        let threat = view.get_friendly_raider_threat(pos);
        let over_threat = view.get_threat(rs_target) * (1.0 + view.get_riot_threat(rs_target)) > threat
            || members.iter().any(|&(rpos, cloaked)| {
                !cloaked && view.get_threat(rpos) * (1.0 + view.get_riot_threat(rpos)) > threat
            });
        if over_threat {
            // Try to keep raider squads from suiciding.
            let haven = self.graph_get_closest_raider_haven(pos);
            self.raider_squad_sneak(sid, haven);
            return;
        }

        // Sent to block an incursion's way.
        if let Some(block) = self.incursion_block(sid) {
            self.raider_squad_raid(sid, block);
            return;
        }
        let war = &self.war;
        let view = war.view(def);

        // For ready squads, assign to a target.
        let Some((leader, leader_unit, leader_pos)) =
            leader.and_then(|l| self.fighters.get(&l)).map(|f| (f.id, f.unit, f.get_pos()))
        else {
            return;
        };
        let mut best_task = None;
        let mut best_target = NULLPOS;
        let mut cost = f32::MAX; // port: 65535 in the C++, see raider_assign_solo_raider

        let mut reach = None;
        // Where the terrain lets the squad get in range at all.
        let terrain = if self.dev_off("reach") { None } else { self.terrain.get() };
        let engage = crate::terrain::engage_test(terrain, leader_unit);
        let hits = war.reach(leader_unit.def());
        let h = &self.raider_handler;
        for &tid in &h.scout_task_ids {
            let Some(s) = h.scout_tasks.get(&tid) else {
                continue;
            };
            let tmpcost = scout_cost(s, &self.graph.metal_spots, self.frame, leader, leader_pos);
            if tmpcost >= cost || !engage(s.target) {
                continue;
            }
            if view.get_threat(s.target) * (1.0 + view.get_riot_threat(s.target)) > threat
                || !reach
                    .get_or_insert_with(|| self.pathfinder.raider_reachability(war, leader_unit, threat))
                    .reaches(s.target)
            {
                continue;
            }
            cost = tmpcost;
            best_task = Some(tid);
        }

        for d in &war.sniper_sightings {
            if view.get_riot_threat(d.position) > 0.0
                || view.get_threat(d.position) > war.available_mobile_threat
                || !engage(d.position)
            {
                continue;
            }
            let tmpcost = distance(pos, d.position) / 4.0 - 1000.0;
            if tmpcost < cost {
                cost = tmpcost;
                best_target = d.position;
            }
        }
        // TODO: also attack assaults close to home?
        // TODO: also defend against enemy raiders?

        let mut porc = false;
        for e in war.targets.values() {
            if !e.identified {
                continue;
            }
            let ethreat = view.get_threat(e.position) * (1.0 + view.get_riot_threat(e.position));
            if ethreat > threat
                || (!e.is_porc && view.get_porc_threat(e.position) > 0.0)
                || !engage(e.position)
                || !war.can_hit(hits, e)
            {
                continue;
            }
            let tmpcost = raid_cost(war, &self.defs, e, pos, ethreat);
            if tmpcost < cost {
                cost = tmpcost;
                best_target = e.position;
                porc = e.is_static;
            }
        }

        drop(engage);
        let best_task = best_task.and_then(|tid| self.raider_handler.scout_tasks.get(&tid).map(|t| (tid, t.target)));
        if best_target != NULLPOS {
            if self.fighters.get(&leader).is_some_and(|f| f.task.is_some()) {
                self.raider_clear_task(leader);
            }
            if porc || distance(best_target, pos) > 400.0 {
                self.raider_squad_sneak(sid, best_target);
            } else {
                self.raider_squad_raid(sid, best_target);
            }
        } else if let Some((tid, target)) = best_task {
            self.raider_take_task(leader, tid);
            self.raider_squad_sneak(sid, target);
        } else {
            self.raider_clear_task(leader);
            let rally = self.war_get_raider_rally(pos);
            self.raider_squad_sneak(sid, rally);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spot(hostile: bool, enemy_shadowed: bool, last_seen: i32) -> MetalSpot {
        MetalSpot {
            index: 0,
            position: Vec3::xz(1000.0, 0.0),
            value: 1.0,
            visible: false,
            owned: false,
            touched: false,
            hostile,
            enemy_shadowed,
            ally_shadowed: false,
            extractor: None,
            last_seen,
            pylons: Vec::new(),
            links: Vec::new(),
        }
    }

    #[test]
    fn hostile_spots_are_cheaper() {
        let task = ScoutTask::new(1, Vec3::xz(1000.0, 0.0), 0);
        let origin = Vec3::xz(0.0, 0.0);
        // Seen 1800 frames ago: hostile cost = 1000 / (3 + 2).
        let hostile = scout_cost(&task, &[spot(true, false, 0)], 1800, 7, origin);
        assert!((hostile - 200.0).abs() < 1e-3);
        // Non-hostile: 1000 / max(1, 2), no recency penalty after 900 frames.
        let neutral = scout_cost(&task, &[spot(false, false, 0)], 1800, 7, origin);
        assert!((neutral - 500.0).abs() < 1e-3);
        // Recently seen spots get a penalty of 4 per frame under 900.
        let recent = scout_cost(&task, &[spot(false, false, 0)], 450, 7, origin);
        assert!((recent - (1000.0 + 4.0 * 450.0)).abs() < 1e-3);
    }

    #[test]
    fn shared_non_hostile_spots_are_dispreferred() {
        let mut task = ScoutTask::new(1, Vec3::xz(1000.0, 0.0), 0);
        let spots = [spot(false, false, 0)];
        let origin = Vec3::xz(0.0, 0.0);
        task.add_raider(3);
        let other = scout_cost(&task, &spots, 1800, 7, origin);
        let own = scout_cost(&task, &spots, 1800, 3, origin);
        assert!((other - own - 2000.0).abs() < 1e-3);
        assert!((sparrow_cost(&task, &spots, 1800, 7, origin) - 2500.0).abs() < 1e-3);
        // Unknown spots are never chosen.
        assert_eq!(scout_cost(&task, &[], 1800, 7, origin), f32::INFINITY);
    }
}
