//! Assault squads, shield balls and air squads.
//!
//! A squad goes where its leader can, so its members move alike: squads
//! form by kind of unit and by the ground they can cross (a vehicle does
//! not follow bots up a slope), but for amphibious bots, which go wherever
//! bots do and may join them.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use springai::{Unit, UnitDef, Vec3};
use zk_terrain::Tier;

use crate::Ai;
use crate::military::fire_squad::{self, Mode as FireMode};
use crate::military::taskforce::{self, Aim};
use crate::military::{Fighter, SquadId, SquadKind};
use crate::nav;
use crate::terrain::tier_of;
use crate::util::{NULLPOS, distance, kgb_rand};

#[derive(Default)]
pub struct SquadHandler {
    /// Fighter-store ids of all assault-type fighters.
    pub fighters: BTreeSet<i32>,
    /// The squads forming, by kind of unit and movement tier (none for
    /// aircraft).
    next_squads: BTreeMap<(Slot, Option<Tier>), SquadId>,
    /// Taskforces (see `military::taskforce`): the shield ball and the
    /// other forces doctrines make.
    pub taskforces: VecDeque<SquadId>,
    pub squads: VecDeque<SquadId>,
    pub air_squads: VecDeque<SquadId>,
}

/// Which forming squad an assault unit joins.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Slot {
    Cloaky,
    Shield,
    Amph,
    Recluse,
    Spider,
    Veh,
    Halberd,
    Hover,
    Tank,
    Boat,
    Jump,
    Brawler,
    /// Rapier/revenant and other air assaults.
    Air,
}

impl Slot {
    fn for_name(name: &str) -> Slot {
        if name.starts_with("cloak") {
            Slot::Cloaky
        } else if name.starts_with("shield") {
            Slot::Shield
        } else if name.starts_with("amph") {
            Slot::Amph
        } else if name == "spiderskirm" {
            Slot::Recluse
        } else if name.starts_with("spider") {
            Slot::Spider
        } else if name.starts_with("veh") {
            Slot::Veh
        } else if name == "hoverassault" {
            Slot::Halberd
        } else if name.starts_with("hover") {
            Slot::Hover
        } else if name.starts_with("tank") {
            Slot::Tank
        } else if name.starts_with("ship") {
            Slot::Boat
        } else if name.starts_with("jump") {
            Slot::Jump
        } else if name == "gunshipheavyskirm" {
            Slot::Brawler
        } else {
            Slot::Air
        }
    }

    fn for_assault(name: &str, firesquads: bool) -> Slot {
        // Recluses used to form alone. Fire teams need their Venoms and
        // Redbacks in the same forming squad, as other factories do.
        match Self::for_name(name) {
            Self::Recluse if firesquads => Self::Spider,
            slot => slot,
        }
    }

    fn is_air(self) -> bool {
        matches!(self, Slot::Brawler | Slot::Air)
    }

    /// Whether a forming squad of this slot is complete and should rally.
    fn is_complete(self, metal_value: f32, income: f32, size: usize, adjusted_income: f32) -> bool {
        match self {
            Slot::Halberd => size as i32 >= 8.min(2 + (adjusted_income / 15.0).floor() as i32),
            Slot::Brawler | Slot::Air => metal_value > income * 45.0,
            _ => ((metal_value > income * 45.0 && metal_value > 1000.0) || metal_value > 2500.0) && size > 2,
        }
    }
}

impl SquadHandler {
    pub fn new() -> Self {
        Self::default()
    }
}

/// Whether role `role` of doctrine `doctrine` spots (see `taskforce::spotter`).
fn doctrine_role_spots(tf: &taskforce::Taskforces, doctrine: Option<&str>, role: Option<usize>) -> bool {
    doctrine.and_then(|d| tf.doctrine(d)).zip(role).is_some_and(|(d, r)| d.roles[r].spot.is_some())
}

/// Whether units of movement tiers `a` and `b` go the same ways (as far as
/// following a leader of either goes: amphibious bots with bots).
fn same_ways(a: Option<Tier>, b: Option<Tier>) -> bool {
    a == b || matches!((a, b), (Some(Tier::Bot), Some(Tier::Amphib)) | (Some(Tier::Amphib), Some(Tier::Bot)))
}

fn fire_escort(name: &str) -> bool {
    matches!(
        name,
        "cloakriot"
            | "amphriot"
            | "amphimpulse"
            | "vehriot"
            | "hoverriot"
            | "spiderriot"
            | "spideremp"
            | "tankriot"
            | "shipriot"
    )
}

fn escort_room(cores: f32, total: f32, extra: f32, distance: f32) -> bool {
    // Keep the stronger entry majority, so a screen cannot make the
    // squad stop being a fire team. Only assimilate nearby units.
    cores > 0.0 && cores >= (total + extra) * 0.55 && distance <= 600.0
}

/// A force of a core's doctrine, as the core sees it.
struct Opening {
    id: SquadId,
    cores: usize,
    engaging: bool,
    formed: i32,
    value: f32,
}

/// The force a new core joins: pooled with allies, the least valuable;
/// with room for it (`cores_per_force` 0: no limit), for a doctrine that
/// reinforces engaged forces the least valuable (the shield ball, as
/// always), else the newest not engaged, so cores never trickle to a fight
/// alone. None: it starts one.
fn core_room(forces: &[Opening], pooled: bool, reinforce: bool, cores_per_force: usize) -> Option<SquadId> {
    let room = |f: &&Opening| cores_per_force == 0 || f.cores < cores_per_force;
    if pooled {
        return forces.iter().min_by(|a, b| a.value.total_cmp(&b.value)).map(|f| f.id);
    }
    if reinforce {
        return forces.iter().filter(room).min_by(|a, b| a.value.total_cmp(&b.value)).map(|f| f.id);
    }
    forces.iter().filter(room).filter(|f| !f.engaging).max_by_key(|f| f.formed).map(|f| f.id)
}

/// "; engaged N s, its leader averaging V" for a force's log line.
fn engaged_for(ran: Option<(f32, f32)>) -> String {
    ran.map_or(String::new(), |(secs, speed)| format!("; engaged {secs:.0} s, its leader averaging {speed:.0}"))
}

impl Ai {
    pub fn squad_update(&mut self, frame: i32) {
        if frame % 30 == (self.offset + 7) % 30 {
            self.squad_check_fighters(frame);
        }
        if frame % 7 == self.offset % 7 {
            self.squad_assign_squads();
        }
        if frame % 7 == (self.offset + 4) % 7 {
            self.squad_assign_taskforces();
        }
        self.taskforce_watch_volleys();
        if frame % 300 == (self.offset + 13) % 300 {
            self.taskforce_wake();
        }
        if frame % 14 == (self.offset + 9) % 14 {
            self.squad_assign_air_squads();
        }
        if frame % 1800 == (self.offset + 11) % 1800 {
            self.squad_check_tiers();
        }
    }

    /// Report squads whose members do not go the same ways (none should).
    fn squad_check_tiers(&mut self) {
        let mut mixed = Vec::new();
        for sid in self.squads.ids() {
            let Some(s) = self.squads.get(sid) else { continue };
            let spots =
                |m: &i32| self.fighters.get(m).is_some_and(|f| s.shape.spot(f.unit.def().map(|d| d.id())).is_some());
            let tiers: BTreeSet<Option<Tier>> =
                s.fighters.iter().filter(|m| !spots(m)).map(|&m| self.squad_tier(m)).collect();
            if tiers.iter().any(|&a| tiers.iter().any(|&b| !same_ways(a, b))) {
                mixed.push(format!("squad {sid}: {tiers:?}"));
            }
        }
        if !mixed.is_empty() {
            self.debug(&format!("squads mixing movement tiers: {}", mixed.join("; ")));
        }
    }

    /// Drop dead and stuck fighters, and (re)assign squadless ones.
    fn squad_check_fighters(&mut self, frame: i32) {
        let ids: Vec<i32> = self.squad_handler.fighters.iter().copied().collect();
        for id in ids {
            let Some(f) = self.fighters.get(&id) else {
                self.squad_handler.fighters.remove(&id);
                continue;
            };
            let unit = f.unit;
            if f.is_dead() {
                self.squad_leave_squad(id);
                self.squad_delete_fighter(id);
                continue;
            }
            let retreating = self.retreat_handler.is_retreating(id);
            let flies = unit.def().is_some_and(|d| d.is_able_to_fly());
            // A squad the operator holds somewhere stands still on purpose:
            // not stuck, not for the reclaimers; so does one waiting out
            // a blast of ours (see `blasts`).
            let held = f
                .squad
                .is_some_and(|s| self.op.over.squad_intents.contains_key(&s) || self.op_squad_target(s).is_some())
                || self.blasts_waiting(unit.pos())
                || self.fire_controls(id)
                || self.squad_strike_owns(id);
            if !retreating && !flies && !held && self.fighters.get_mut(&id).is_some_and(|f| f.is_stuck(frame)) {
                self.squad_leave_squad(id);
                self.eco_add_combat_reclaim_task(unit);
                self.retreat_remove_unit(unit);
                self.squad_delete_fighter(id);
                continue;
            }
            // A squad id that no longer exists counts as no squad.
            let Some(f) = self.fighters.get_mut(&id) else { continue };
            if f.squad.is_some_and(|s| !self.squads.contains(s)) {
                f.squad = None;
            }
            if f.squad.is_none() && !retreating {
                let candidate = self.fighters.get(&id).is_some_and(|f| f.shield_mob);
                // A Firewalker already owned by this handler stays an assault
                // after repair, including if the switch was turned off in play.
                if !candidate && self.unit_classes.is_assault(self.defs.name_of(unit), true) {
                    self.squad_assign_assault(id);
                } else {
                    self.squad_assign_taskforce_member(id);
                }
            }
        }
        self.squad_recruit_fire_escorts();
    }

    /// Nearby riots from ordinary assault squads can fill a fire team's
    /// screen after it forms. Do not split another skirmisher force or
    /// commandeer doctrine, home-defense, retreating or operator-led units.
    fn squad_recruit_fire_escorts(&mut self) {
        if self.dev_off("firesquads") {
            return;
        }
        let autonomous = |sid| {
            !self.qrf_drafted(sid)
                && self.op_squad_target(sid).is_none()
                && !self.op.over.squad_intents.contains_key(&sid)
        };
        let teams: Vec<_> = self
            .squad_handler
            .squads
            .iter()
            .copied()
            .filter(|&sid| {
                autonomous(sid)
                    && self.squads.get(sid).is_some_and(|s| {
                        s.kind == SquadKind::Assault && s.status == 'a' && s.fire.mode == FireMode::Kite
                    })
            })
            .collect();
        if teams.is_empty() {
            return;
        }
        let escorts: Vec<_> = self
            .squad_handler
            .fighters
            .iter()
            .copied()
            .filter(|id| {
                let Some(f) = self.fighters.get(id) else { return false };
                if f.is_dead() || self.retreat_handler.is_retreating(*id) || !fire_escort(self.defs.name_of(f.unit)) {
                    return false;
                }
                f.squad.is_some_and(|sid| {
                    autonomous(sid)
                        && self.squads.get(sid).is_some_and(|s| {
                            s.kind == SquadKind::Assault
                                && !s.fire.active()
                                && s.fighters.iter().all(|m| {
                                    self.fighters
                                        .get(m)
                                        .is_none_or(|f| !fire_squad::core_type(self.defs.name_of(f.unit)))
                                })
                        })
                })
            })
            .collect();
        for id in escorts {
            let Some(f) = self.fighters.get(&id) else { continue };
            let unit = f.unit;
            let metal = f.metal_value;
            let pos = unit.pos();
            let target = teams
                .iter()
                .filter_map(|&sid| {
                    let s = self.squads.get(sid)?;
                    let cores: f32 = s
                        .fire
                        .cores
                        .iter()
                        .filter_map(|id| self.fighters.get(id))
                        .filter(|f| !f.is_dead() && !self.retreat_handler.is_retreating(f.id))
                        .map(|f| f.metal_value)
                        .sum();
                    let at = s.get_pos(&self.fighters);
                    if !escort_room(cores, s.metal_value, metal, distance(pos, at)) || !self.squad_fits(sid, id) {
                        return None;
                    }
                    let nav = nav!(self);
                    nav.pathfinder
                        .is_reachable(nav.war, unit, at, crate::kgbutil::PathCost::AssaultPath)
                        .then_some((sid, distance(pos, at)))
                })
                .min_by(|a, b| a.1.total_cmp(&b.1))
                .map(|x| x.0);
            if let Some(sid) = target {
                self.squad_leave_squad(id);
                let mut nav = nav!(self);
                if let Some(s) = self.squads.get_mut(sid) {
                    s.add_unit_nav(id, &mut self.fighters, &mut nav);
                }
            }
        }
    }

    fn squad_delete_fighter(&mut self, id: i32) {
        self.fighters.remove(&id);
        self.squad_handler.fighters.remove(&id);
    }

    /// Let go of fighter `id` altogether (out of its squad and the store),
    /// for another handler to take.
    pub(crate) fn squad_forget_fighter(&mut self, id: i32) {
        self.squad_leave_squad(id);
        self.squad_delete_fighter(id);
    }

    /// Remove a fighter from whatever squad it is in.
    pub(crate) fn squad_leave_squad(&mut self, id: i32) {
        let Some(sid) = self.fighters.get(&id).and_then(|f| f.squad) else { return };
        let mut nav = nav!(self);
        match self.squads.get_mut(sid) {
            Some(s) => s.remove_unit_nav(id, &mut self.fighters, &mut nav),
            None => {
                if let Some(f) = self.fighters.get_mut(&id) {
                    f.squad = None;
                }
            }
        }
    }

    /// Delete a squad, letting go of supports that were following it.
    pub(crate) fn squad_delete_squad(&mut self, sid: SquadId) {
        for id in &self.misc_handler.supports {
            if let Some(g) = self.fighters.get_mut(id)
                && g.squad == Some(sid)
            {
                g.squad = None;
            }
        }
        self.squads.remove(sid);
        self.taskforces.packs.remove(&sid);
    }

    /// The movement tier of fighter `id` (none for aircraft).
    fn squad_tier(&self, id: i32) -> Option<Tier> {
        self.fighters.get(&id).and_then(|f| f.unit.def()).and_then(tier_of)
    }

    /// Whether fighter `id` goes the same ways as every member of squad
    /// `sid`.
    fn squad_fits(&self, sid: SquadId, id: i32) -> bool {
        let Some(s) = self.squads.get(sid) else { return false };
        // Spotters go their own ways.
        let spots =
            |m: i32| self.fighters.get(&m).is_some_and(|f| s.shape.spot(f.unit.def().map(|d| d.id())).is_some());
        if spots(id) {
            return true;
        }
        let tier = self.squad_tier(id);
        s.fighters.iter().filter(|&&m| !spots(m)).all(|&m| same_ways(self.squad_tier(m), tier))
    }

    fn squad_assign_assault(&mut self, id: i32) {
        let Some(unit) = self.fighters.get(&id).map(|f| f.unit) else { return };
        let slot = Slot::for_assault(self.defs.name_of(unit), !self.dev_off("firesquads"));
        let key = (slot, unit.def().and_then(tier_of));
        let tf = self.graph.territory_fraction;
        let inc = self.eco.effective_income;
        let allies = self.merged_allies;

        // Create a new squad if there isn't one.
        let sid = match self.squad_handler.next_squads.get(&key).copied() {
            Some(sid) if self.squads.contains(sid) => sid,
            _ => {
                let kind = if slot == Slot::Halberd { SquadKind::Hover } else { SquadKind::Assault };
                let sid = self.squads.create(kind, self.team_id);
                if let Some(s) = self.squads.get_mut(sid) {
                    if slot.is_air() {
                        // For brawlers/nimbus and rapier/revenant.
                        s.income = inc / if allies == 0 { 1.0 } else { 2.0 };
                        s.is_air_squad = true;
                    } else {
                        s.income = inc / (1.0 + (allies as f32 * tf * tf));
                    }
                }
                if slot.is_air() {
                    self.squad_handler.air_squads.push_back(sid);
                } else {
                    self.squad_handler.squads.push_back(sid);
                }
                self.squad_handler.next_squads.insert(key, sid);
                sid
            }
        };

        let mut nav = nav!(self);
        let Some(s) = self.squads.get_mut(sid) else { return };
        s.add_unit_nav(id, &mut self.fighters, &mut nav);
        if slot.is_complete(s.metal_value, s.income, s.get_size(), self.eco.adjusted_income) {
            s.status = 'r';
            self.squad_handler.next_squads.remove(&key);
        }
    }

    pub fn squad_add_assault(&mut self, f: Fighter) {
        // port: one fighter store serves all handlers, so also refuse units
        // another handler already controls instead of replacing its wrapper.
        if self.squad_handler.fighters.contains(&f.id) || self.fighters.contains_key(&f.id) {
            return;
        }
        let id = f.id;
        self.fighters.insert(id, f);
        self.squad_handler.fighters.insert(id);
        self.squad_assign_assault(id);
    }

    /// Make `units` (no longer any handler's) attacking squads where they
    /// stand, one for each kind of ground they cross: a force landed by
    /// air, say.
    pub fn squad_adopt(&mut self, units: &[Unit]) -> Vec<SquadId> {
        let team = self.team_id;
        let fresh: Vec<Fighter> = units
            .iter()
            .filter(|u| u.health() > 0.0 && u.team() == team && !self.fighters.contains_key(&u.id()))
            .filter_map(|&u| Some(Fighter::new(u, u.def()?.cost(self.metal), team)))
            .collect();
        let mut squads: Vec<SquadId> = Vec::new();
        for f in fresh {
            let id = f.id;
            self.fighters.insert(id, f);
            self.squad_handler.fighters.insert(id);
            let sid = match squads.iter().copied().find(|&sid| self.squad_fits(sid, id)) {
                Some(sid) => sid,
                None => {
                    let sid = self.squads.create(SquadKind::Assault, team);
                    squads.push(sid);
                    sid
                }
            };
            let mut nav = nav!(self);
            if let Some(s) = self.squads.get_mut(sid) {
                s.add_unit_nav(id, &mut self.fighters, &mut nav);
            }
        }
        for &sid in &squads {
            if let Some(s) = self.squads.get_mut(sid) {
                s.status = 'a';
            }
            self.squad_handler.squads.push_back(sid);
        }
        squads
    }

    /// Whether taskforce `sid` is short of role `role` of its doctrine
    /// (a role that takes all that come always is).
    pub(crate) fn squad_short_of(&self, sid: SquadId, role: usize) -> bool {
        let Some(q) = self.squads.get(sid) else { return false };
        let tf = &self.taskforces;
        let Some(d) = tf.doctrine(&q.shape.doctrine) else { return false };
        let Some(r) = d.roles.get(role) else { return false };
        if r.want.is_none() {
            return true;
        }
        let units = q.everyone().filter_map(|id| self.fighters.get(&id));
        let away = tf
            .spotters
            .by_unit
            .values()
            .filter(|sp| r.spot.is_some() && sp.sid == sid && sp.job == taskforce::spotter::Job::Repairing)
            .filter_map(|sp| self.fighters.get(&sp.unit));
        let force = d.force(units.chain(away).map(|f| (self.defs.name_of(f.unit), f.metal_value)), &tf.classes);
        r.want(&force).is_some_and(|w| force.roles.get(role).is_some_and(|x| x.1 < w))
    }

    /// The metal force `sid` must be worth to go in: its doctrine's `engage`
    /// over the force and our income (as assault squads count it), else
    /// its `engage_value`.
    fn squad_engage_threshold(&self, sid: SquadId) -> f32 {
        let Some(q) = self.squads.get(sid) else { return f32::INFINITY };
        let Some(e) = q.shape.engage.as_ref() else { return q.shape.engage_value };
        let tf = &self.taskforces;
        let Some(d) = tf.doctrine(&q.shape.doctrine) else { return q.shape.engage_value };
        let units = q.everyone().filter_map(|id| self.fighters.get(&id));
        let mut force = d.force(units.map(|f| (self.defs.name_of(f.unit), f.metal_value)), &tf.classes);
        let terr = self.graph.territory_fraction;
        force.income = self.eco.effective_income / (1.0 + self.merged_allies as f32 * terr * terr);
        (force.density, force.openness, force.reach) = self.taskforce_map();
        e.eval(&force)
    }

    /// How far `p` lies along the line from our start to the enemy's start
    /// box (0 ours, 1 theirs; see `taskforce: depth axis` in the log).
    fn taskforce_depth(&self, p: Vec3) -> Option<f32> {
        let (a, b) = (self.graph.get_start_pos(), self.graph.get_enemy_base()?);
        let (dx, dz) = (b.x - a.x, b.z - a.z);
        let len2 = dx * dx + dz * dz;
        (len2 > 0.0 && a != NULLPOS).then(|| ((p.x - a.x) * dx + (p.z - a.z) * dz) / len2)
    }

    /// The median depth of our ground fighters outside force `sid`, and how
    /// many they are.
    fn squad_army_depth(&self, sid: SquadId) -> Option<(f32, usize)> {
        let mut ds: Vec<f32> = (self.fighters.values())
            .filter(|f| f.squad != Some(sid) && f.unit.health() > 0.0)
            .filter(|f| f.unit.def().is_some_and(|d| !d.is_able_to_fly()))
            .filter_map(|f| self.taskforce_depth(f.get_pos()))
            .collect();
        if ds.is_empty() {
            return None;
        }
        ds.sort_by(f32::total_cmp);
        Some((ds[ds.len() / 2], ds.len()))
    }

    /// A unit of `options` (what a factory builds) that a force is short of,
    /// for a role with `produce`: rolled at its `produce` chance, the first
    /// that comes up; with the doctrine and role, for the log.
    pub(crate) fn squad_produce(&self, options: &[UnitDef]) -> Option<(UnitDef, String, String)> {
        let tf = &self.taskforces;
        for d in tf.doctrines.iter().filter(|d| d.enabled && taskforce::squad_aim(d.aim)) {
            for (ri, r) in d.roles.iter().enumerate().filter(|(_, r)| r.produce > 0.0) {
                let Some(&def) = options.iter().find(|o| r.units.iter().any(|u| u == self.defs.name(**o))) else {
                    continue;
                };
                let short = (self.squad_handler.taskforces.iter())
                    .filter(|&&sid| self.squads.get(sid).is_some_and(|q| q.shape.doctrine == d.name))
                    .any(|&sid| self.squad_short_of(sid, ri));
                if short && kgb_rand() < f64::from(r.produce) {
                    return Some((def, d.name.clone(), r.name.clone()));
                }
            }
            // A spotter is called from the standing units, never built, but
            // for one plane a game while the enemy shows no anti-air (see
            // `taskforce::spotter`).
            if self.taskforces.spotters.ordered > 0
                || !d.roles.iter().any(|r| r.spot.is_some())
                || self.taskforce_enemy_aa()
            {
                continue;
            }
            for (ri, r) in d.roles.iter().enumerate().filter(|(_, r)| r.spot.is_some()) {
                let plane = |o: &&UnitDef| o.is_able_to_fly() && r.units.iter().any(|u| u == self.defs.name(**o));
                let Some(&def) = options.iter().find(plane) else { continue };
                let short = (self.squad_handler.taskforces.iter())
                    .filter(|&&sid| self.squads.get(sid).is_some_and(|q| q.shape.doctrine == d.name))
                    .any(|&sid| self.squad_short_of(sid, ri));
                if short {
                    return Some((def, d.name.clone(), r.name.clone()));
                }
            }
        }
        None
    }

    /// Place a unit in a taskforce (see `military::taskforce`), by the
    /// doctrine that takes its type. A core starts a force of its own
    /// unless one not engaged has room for more cores (see `core_room`;
    /// merged with allies, the doctrine may pool them all in one). A role
    /// member joins the force of
    /// its doctrine short of the role: with `to_strongest` the one with
    /// fewest of the role (the most valuable of those, where it does most
    /// good, as Aspis do), else the least valuable. With no force to join
    /// it fights as an assault. A unit no doctrine takes (a squadless Pyro)
    /// joins the least valuable force of a doctrine taking `strays` (the
    /// shield ball, as always), else fights as an assault too.
    pub(crate) fn squad_assign_taskforce_member(&mut self, id: i32) {
        if self.taskforce_watch_rejoin(id) {
            return;
        }
        let Some(unit) = self.fighters.get(&id).map(|f| f.unit) else { return };
        let name = self.defs.name_of(unit).to_owned();
        let tf = &self.taskforces;
        // Shared spotter types join a doctrine that actually has a vacancy.
        let doctrine = tf
            .doctrines
            .iter()
            .find(|d| {
                d.enabled
                    && taskforce::squad_aim(d.aim)
                    && d.role_of(&name, &tf.classes).is_some_and(|r| {
                        d.roles[r].spot.is_some()
                            && self.squad_handler.taskforces.iter().any(|&sid| {
                                self.squads.get(sid).is_some_and(|q| q.shape.doctrine == d.name)
                                    && self.squad_short_of(sid, r)
                            })
                    })
            })
            .or_else(|| tf.squad_doctrine(&name));
        let dname = doctrine.map(|d| d.name.clone());
        let is_core = doctrine.is_some_and(|d| d.is_core(&name, &tf.classes));
        let role = doctrine.and_then(|d| d.role_of(&name, &tf.classes));
        let to_strongest = doctrine.zip(role).is_some_and(|(d, r)| d.roles[r].to_strongest);
        let (pool, reinforce, cores_per_force) = doctrine
            .map_or((false, false, 1), |d| (d.pool_with_allies, d.reinforce_engaged, d.cores_per_force as usize));
        // Forces of its doctrine (of those taking strays, for a unit none
        // takes) that go where it goes.
        let forces: Vec<SquadId> = (self.squad_handler.taskforces.iter().copied())
            .filter(|&sid| {
                self.squads.get(sid).is_some_and(|q| dname.as_ref().map_or(q.shape.strays, |d| &q.shape.doctrine == d))
            })
            .filter(|&sid| is_core || self.squad_fits(sid, id))
            .collect();
        let needed = if is_core {
            let pooled = pool && self.merged_allies > 0;
            let open: Vec<Opening> = (forces.iter().filter_map(|&sid| self.squads.get(sid)))
                .map(|q| Opening {
                    id: q.id,
                    cores: q.cores(&self.fighters),
                    engaging: q.engaging,
                    formed: q.formed,
                    value: q.metal_value,
                })
                .collect();
            match core_room(&open, pooled, reinforce, cores_per_force) {
                Some(sid) => {
                    self.retreat_remove_unit(unit);
                    Some(sid)
                }
                None => {
                    // A new force around the core.
                    self.retreat_remove_unit(unit);
                    let sid = self.squads.create(SquadKind::Taskforce, self.team_id);
                    self.squad_handler.taskforces.push_back(sid);
                    let shape = self.taskforces.shape(dname.as_deref().unwrap_or_default());
                    let frame = self.frame;
                    let mut nav = nav!(self);
                    if let Some(s) = self.squads.get_mut(sid) {
                        s.shape = shape;
                        s.formed = frame;
                        s.status = 'a';
                        s.add_unit_nav(id, &mut self.fighters, &mut nav);
                    }
                    self.squad_call_taskforce_mobs(sid);
                    let n = self.squads.get(sid).map_or(0, |q| q.everyone().count());
                    let d = dname.unwrap_or_default();
                    self.log(&format!("taskforce: a {name} forms a {d} force (squad {sid}, {n} strong)"));
                    return;
                }
            }
        } else {
            // Forces that take members now (engaged ones only if their
            // doctrine reinforces them, or for spotters), short of its role.
            let spots = doctrine_role_spots(&self.taskforces, dname.as_deref(), role);
            let open =
                |sid: SquadId| self.squads.get(sid).is_some_and(|q| !q.engaging || q.shape.reinforce_engaged || spots);
            let short: Vec<SquadId> = match role {
                Some(r) => forces.iter().copied().filter(|&sid| open(sid) && self.squad_short_of(sid, r)).collect(),
                None => forces.iter().copied().filter(|&sid| open(sid)).collect(),
            };
            if short.is_empty() {
                // A spotter no force wants goes back where it came from.
                if doctrine_role_spots(&self.taskforces, dname.as_deref(), role) {
                    self.taskforce_spotter_release(id);
                    return;
                }
                // No force to join: fight as an assault.
                self.retreat_add_coward(unit);
                self.squad_assign_assault(id);
                return;
            }
            self.retreat_remove_unit(unit);
            if let (true, Some(r)) = (to_strongest, role) {
                let fewest = |sid: SquadId| self.squads.get(sid).map_or(usize::MAX, |q| q.census(&self.fighters)[r]);
                let mut best: Option<(SquadId, f32, usize)> = None;
                for q in short.iter().filter_map(|&sid| self.squads.get(sid)) {
                    let n = fewest(q.id);
                    if best.is_none_or(|(_, v, m)| n < m || (n == m && q.metal_value > v)) {
                        best = Some((q.id, q.metal_value, n));
                    }
                }
                best.map(|b| b.0)
            } else {
                (short.iter().filter_map(|&sid| self.squads.get(sid)))
                    .min_by(|a, b| a.metal_value.total_cmp(&b.metal_value))
                    .map(|q| q.id)
            }
        };

        let Some(sid) = needed else { return };
        self.squad_log_join(sid, id, "joins");
        let mut nav = nav!(self);
        if let Some(s) = self.squads.get_mut(sid) {
            s.add_unit_nav(id, &mut self.fighters, &mut nav);
        }
    }

    /// Log a taskforce member's death: its force, whether that force was
    /// forming (never yet engaged), engaged or rallying, what killed it, and
    /// where along the start axis.
    pub fn squad_log_death(&mut self, unit: Unit, attacker: Option<Unit>) {
        let Some(sid) = self.fighters.get(&unit.id()).and_then(|f| f.squad) else { return };
        let Some(s) = self.squads.get(sid).filter(|s| s.is_taskforce()) else { return };
        let state = match (s.engaging, s.engaged) {
            (true, _) => "engaged",
            (false, 0) => "forming",
            _ => "rallying",
        };
        let by = attacker.and_then(|a| a.def()).map_or("?".to_owned(), |d| self.defs.name(d).to_owned());
        let depth = self.taskforce_depth(unit.pos()).map_or("-".to_owned(), |d| format!("{d:.2}"));
        // How far it was from its leader: in transit to the force, or with it.
        let far = match s.get_leader(&self.fighters) {
            Some(l) if l.id() == unit.id() => "the leader".to_owned(),
            Some(l) => format!("{:.0} from its leader", distance(l.pos(), unit.pos())),
            None => "no leader".to_owned(),
        };
        let spotter = self.taskforce_spotter_doing(unit.id()).map_or(String::new(), |j| format!(", spotter {j}"));
        let line = format!(
            "taskforce: a {} of the {} force (squad {sid}, {state}) dies to a {by}, depth {depth} ({far}){spotter}",
            self.defs.name_of(unit),
            s.shape.doctrine
        );
        self.log(&line);
    }

    /// Log a unit about to join force `sid`: the force's state and how far
    /// the unit has to go to its leader.
    pub(crate) fn squad_log_join(&mut self, sid: SquadId, id: i32, how: &str) {
        let Some(s) = self.squads.get(sid) else { return };
        let Some(f) = self.fighters.get(&id) else { return };
        let state = match (s.engaging, s.engaged) {
            (true, _) => "engaged",
            (false, 0) => "forming",
            _ => "rallying",
        };
        let far = s.get_leader(&self.fighters).map_or(0.0, |l| distance(l.pos(), f.get_pos()));
        let line = format!(
            "taskforce: a {} {how} the {} force (squad {sid}, {state}), {far:.0} from its leader",
            self.defs.name_of(f.unit),
            s.shape.doctrine
        );
        self.log(&line);
    }

    /// A unit some doctrine takes: drop forces that lost their cores
    /// (their members find new forces, or fight as assaults until a core
    /// appears), then place it.
    pub fn squad_add_taskforce_member(&mut self, f: Fighter) {
        // port: see squad_add_assault.
        if self.squad_handler.fighters.contains(&f.id) || self.fighters.contains_key(&f.id) {
            return;
        }
        let mut f = f;
        f.shield_mob = true;
        let id = f.id;

        let mut fit = Vec::new();
        for sid in std::mem::take(&mut self.squad_handler.taskforces) {
            let Some(s) = self.squads.get_mut(sid) else { continue };
            if s.is_dead(&self.fighters) || s.cores(&self.fighters) == 0 {
                let doctrine = s.shape.doctrine.clone();
                let gone = s.disband(&mut self.fighters);
                let line = format!(
                    "taskforce: the {doctrine} force (squad {sid}) lost its cores; {} go elsewhere",
                    gone.len()
                );
                self.log(&line);
                fit.extend(gone);
                self.squad_delete_squad(sid);
            } else {
                self.squad_handler.taskforces.push_back(sid);
            }
        }
        for fi in fit {
            self.squad_assign_taskforce_member(fi);
        }
        self.fighters.insert(id, f);
        self.squad_handler.fighters.insert(id);
        self.squad_assign_taskforce_member(id);
    }

    /// A new force calls the units its doctrine takes that fight elsewhere
    /// (in assault squads, or alone) and go where it goes, while it is short
    /// of them.
    fn squad_call_taskforce_mobs(&mut self, sid: SquadId) {
        let ids: Vec<i32> = self.squad_handler.fighters.iter().copied().collect();
        for id in ids {
            let Some(f) = self.fighters.get(&id) else { continue };
            if !f.shield_mob {
                continue;
            }
            if f.squad.and_then(|s| self.squads.get(s)).is_some_and(|s| s.is_taskforce()) || !self.squad_fits(sid, id) {
                continue;
            }
            let unit = f.unit;
            if self.retreat_handler.is_retreating(id) || unit.health() <= 0.0 || unit.team() != self.team_id {
                continue;
            }
            let name = self.defs.name_of(unit);
            let Some(doctrine) = self.squads.get(sid).map(|q| q.shape.doctrine.clone()) else { return };
            let tf = &self.taskforces;
            let Some(d) = tf.doctrine(&doctrine) else { return };
            let wanted = if d.is_core(name, &tf.classes) {
                false
            } else {
                d.role_of(name, &tf.classes).is_some_and(|r| self.squad_short_of(sid, r))
            };
            if !wanted {
                continue;
            }
            if f.squad.is_some() {
                self.squad_leave_squad(id);
                if let Some(f) = self.fighters.get_mut(&id) {
                    f.squad = None;
                }
            }
            self.retreat_remove_unit(unit);
            self.squad_log_join(sid, id, "is called to");
            let mut nav = nav!(self);
            if let Some(s) = self.squads.get_mut(sid) {
                s.add_unit_nav(id, &mut self.fighters, &mut nav);
            }
        }
    }

    pub fn squad_remove_unit(&mut self, u: Unit) {
        let id = u.id();
        if !self.squad_handler.fighters.contains(&id) {
            return;
        }
        self.squad_leave_squad(id);
        self.squad_delete_fighter(id);
    }

    pub fn squad_remove_from_squad(&mut self, u: Unit) {
        if self.squad_handler.fighters.contains(&u.id()) {
            self.squad_leave_squad(u.id());
        }
    }

    /// The Java original's `Squad.isDead()` housekeeping, which the C++ port
    /// reduced to a `cutoff()` call before checking ground squads.
    fn squad_refresh(&mut self, sid: SquadId) {
        let striking = self.squad_strike_active(sid);
        let Some(s) = self.squads.get_mut(sid) else { return };
        // port: the C++ cut off forming squads too, dissolving every new
        // squad below 3 units and re-adding its units (inflating its metal
        // value) until it filled up; Java only cuts off finished squads.
        // It also dropped the cutoff for air squads and the leader choice.
        if s.status != 'f' && !striking {
            s.cutoff(&mut self.fighters);
        }
        s.select_leader(&self.fighters);
    }

    /// Assign up to one squad at a time, up to four per second.
    fn squad_assign_squads(&mut self) {
        while let Some(sid) = self.squad_handler.squads.pop_front() {
            self.squad_refresh(sid);
            let Some(s) = self.squads.get(sid) else { continue };
            if s.is_dead(&self.fighters) {
                if s.status == 'f' {
                    self.squad_handler.squads.push_back(sid);
                    return;
                }
                self.squad_delete_squad(sid);
                continue;
            } else if self.frame - s.last_assignment_frame < 30 {
                self.squad_handler.squads.push_front(sid);
                return;
            } else {
                self.squad_assign_squad(sid);
                if let Some(s) = self.squads.get_mut(sid) {
                    s.last_assignment_frame = self.frame;
                }
                self.squad_handler.squads.push_back(sid);
                return;
            }
        }
    }

    /// Assign up to one air squad at a time, up to two per second.
    fn squad_assign_air_squads(&mut self) {
        while let Some(sid) = self.squad_handler.air_squads.pop_front() {
            self.squad_refresh(sid);
            let Some(s) = self.squads.get(sid) else { continue };
            if s.is_dead(&self.fighters) {
                if s.status == 'f' {
                    self.squad_handler.air_squads.push_back(sid);
                    return;
                }
                self.squad_delete_squad(sid);
                continue;
            } else if self.frame - s.last_assignment_frame < 60 {
                self.squad_handler.air_squads.push_front(sid);
                return;
            } else {
                self.squad_assign_air_squad(sid);
                if let Some(s) = self.squads.get_mut(sid) {
                    s.last_assignment_frame = self.frame;
                }
                self.squad_handler.air_squads.push_back(sid);
                return;
            }
        }
    }

    pub(crate) fn squad_set_target(&mut self, sid: SquadId, target: Vec3) {
        if self.lobster_squad_move(sid, target, false) {
            return;
        }
        let mut nav = nav!(self);
        if let Some(s) = self.squads.get_mut(sid) {
            s.set_target(target, &mut self.fighters, &mut nav);
        }
    }

    fn squad_retreat_to(&mut self, sid: SquadId, pos: Vec3) {
        if self.lobster_squad_move(sid, pos, true) {
            return;
        }
        let mut nav = nav!(self);
        if let Some(s) = self.squads.get_mut(sid) {
            s.retreat_to(pos, &mut self.fighters, &mut nav);
        }
    }

    fn squad_is_rallied(&mut self, sid: SquadId) -> bool {
        let mut nav = nav!(self);
        self.squads.get_mut(sid).is_some_and(|s| s.is_rallied(&mut self.fighters, &mut nav))
    }

    fn squad_set_status(&mut self, sid: SquadId, status: char) {
        if let Some(s) = self.squads.get_mut(sid) {
            s.status = status;
        }
    }

    fn squad_assign_squad(&mut self, sid: SquadId) {
        // Moonlighting at home against a raid (see `qrf::live`).
        if self.qrf_drafted(sid) {
            self.squad_strike_cancel(sid);
            self.fire_release(sid);
            return;
        }
        // Sent somewhere by the operator.
        if let Some(target) = self.op_squad_target(sid) {
            self.squad_strike_cancel(sid);
            self.fire_release(sid);
            self.squad_set_target(sid, target);
            return;
        }
        if self.squad_strike(sid).is_some() {
            self.fire_release(sid);
            return;
        }
        if self.squads.get(sid).is_some_and(|s| s.status != 'a') {
            self.fire_release(sid);
        }
        let Some(s) = self.squads.get(sid) else { return };
        let pos = s.get_pos(&self.fighters);
        let (status, target, courage) = (s.status, s.target, s.courage);
        // The threat maps as the squad sees them, under water too (see `water`).
        let view = self.war.view(s.get_leader(&self.fighters).and_then(|l| l.def()));
        let dive = view.dive();
        // The operator's courage: perceived threat is divided by it, and a
        // bold squad (courage > 1) tolerates danger up to that share of its
        // own strength where a plain one tolerates none.
        let tolerance = |friendly: f32| (courage - 1.0).max(0.0) * friendly;

        match status {
            'f' => {
                if view.get_threat(pos) / courage > 2.0 * view.get_total_friendly_threat(pos)
                    || view.get_porc_threat(pos) / courage > tolerance(view.get_total_friendly_threat(pos))
                    || (target != NULLPOS
                        && view.get_porc_threat(target) / courage > tolerance(view.get_total_friendly_threat(pos)))
                {
                    let haven = self.graph_get_closest_haven(pos);
                    self.squad_retreat_to(sid, haven);
                } else {
                    let rally = self.war_get_rally_point(pos);
                    self.squad_set_target(sid, rally);
                }
            }
            'r' => {
                // Set rallying for squads that are finished forming and gathering to attack
                // (where an incursion needs blocking, if one does).
                let rally = self.incursion_block(sid).unwrap_or_else(|| self.war_get_rally_point(pos));
                self.squad_set_target(sid, rally);
                if self.squad_is_rallied(sid) {
                    self.squad_set_status(sid, 'a');
                }
            }
            'a' => {
                if self.fire_assign(sid) {
                    return;
                }
                let view = self.war.view_dive(dive);
                let friendly = view.get_total_friendly_threat(pos);
                let flat = view.get_effective_threat(pos) / courage > tolerance(friendly);
                let porc = view.get_porc_threat(pos) / courage > tolerance(friendly);
                // The fight predictor's verdict where it is on and has a
                // fresh answer (see `fightsim::live`); the threat map's else.
                let danger = self.fight_verdict(sid, pos, courage, flat);
                if danger && (!self.graph.is_ally_territory(pos) || porc) {
                    let haven = self.graph_get_closest_haven(pos);
                    self.squad_retreat_to(sid, haven);
                    return;
                }
                // Sent to block an incursion's way.
                if let Some(block) = self.incursion_block(sid) {
                    self.squad_set_target(sid, block);
                    return;
                }
                let Some(leader) = self.squads.get(sid).and_then(|s| s.get_leader(&self.fighters)) else { return };
                let target = self.war_get_target(leader, true);
                self.squad_set_target(sid, target);
            }
            _ => {}
        }
    }

    fn squad_assign_air_squad(&mut self, sid: SquadId) {
        if let Some(target) = self.op_squad_target(sid) {
            self.fire_release(sid);
            self.squad_set_target(sid, target);
            return;
        }
        let Some(s) = self.squads.get(sid) else { return };
        let pos = s.get_pos(&self.fighters);

        match s.status {
            'f' => {
                let target = self.war_get_air_rally_point(pos);
                if distance(pos, target) > 1800.0 {
                    self.squad_retreat_to(sid, target);
                } else {
                    self.squad_set_target(sid, target);
                }
            }
            'r' => {
                // Set rallying for squads that are finished forming and gathering to attack.
                let rally = self.war_get_air_rally_point(pos);
                self.squad_set_target(sid, rally);
                if self.squad_is_rallied(sid) {
                    self.squad_set_status(sid, 'a');
                }
            }
            'a' => {
                if self.war.get_effective_aa_threat(pos) > 0.0 {
                    let haven = self.graph_get_closest_air_haven(pos);
                    self.squad_retreat_to(sid, haven);
                }
                let Some(leader) = self.squads.get(sid).and_then(|s| s.get_leader(&self.fighters)) else { return };
                let target = self.war_get_air_target(leader, true);
                if distance(target, pos) > 1800.0 {
                    self.squad_retreat_to(sid, target);
                } else {
                    self.squad_set_target(sid, target);
                }
            }
            _ => {}
        }
    }

    /// Process up to one taskforce at a time, four per second, by its
    /// doctrine's conduct: fall back on low shields or health; engage once
    /// worth enough (or mustered long enough), rallied and led by a core
    /// (once engaged, it stays in though scattered unless it regroups),
    /// at what its aim says (falling back from threat it cannot take if
    /// the doctrine says to); else keep out of threat, at the rally point.
    fn squad_assign_taskforces(&mut self) {
        while let Some(sid) = self.squad_handler.taskforces.pop_front() {
            let Some(s) = self.squads.get_mut(sid) else { continue };
            if s.is_dead(&self.fighters) || s.cores(&self.fighters) == 0 {
                // Disband coreless forces: their members join other forces
                // if there are, else fight as assaults.
                let doctrine = s.shape.doctrine.clone();
                let ran = if s.engaging { s.engaged_speed(self.frame) } else { None };
                let fit = s.disband(&mut self.fighters);
                self.log(&format!(
                    "taskforce: the {doctrine} force (squad {sid}) lost its cores; {} go elsewhere{}",
                    fit.len(),
                    engaged_for(ran)
                ));
                for fi in fit {
                    self.squad_assign_taskforce_member(fi);
                }
                self.squad_delete_squad(sid);
                continue;
            }
            if self.frame - s.last_assignment_frame < 30 {
                self.squad_handler.taskforces.push_front(sid);
                return;
            }
            // Packs fight on the move, by a conduct of their own.
            if s.shape.aim == Aim::Pack {
                let (engage_at, why) = self.taskforce_pack(sid);
                self.squad_conducted(sid, engage_at, &why);
                self.squad_handler.taskforces.push_back(sid);
                return;
            }

            let shape = s.shape.clone();
            s.low_shields = shape.low_shields(s.low_shields, s.get_shields());
            let low_shields = s.low_shields;
            let pos = s.get_pos(&self.fighters);
            let health = s.get_health(&self.fighters);
            let metal_value = s.metal_value;
            let courage = s.courage;
            let mustered = shape.muster > 0.0 && (self.frame - s.formed) as f32 >= shape.muster * 30.0;
            let leader = s.get_leader(&self.fighters);
            let engaging = s.engaging;
            // Going in takes the threshold; engaged, a force stays in above
            // `engage_value` (the same, without an `engage` expression), as
            // an attacking assault squad does.
            let threshold = self.squad_engage_threshold(sid);
            let worth = mustered || metal_value > if engaging { shape.engage_value.min(threshold) } else { threshold };
            let led = leader.filter(|l| shape.is_core(l.def().map(|d| d.id())));
            // Being rallied lets it go in; engaged, only a doctrine that
            // regroups breaks off when its members scatter.
            let rallied = self.squad_is_rallied(sid) || (engaging && !shape.regroup);

            // Where it engages, if it does; else why not.
            let mut engage_at = None;
            let why = if low_shields {
                let haven = self.graph_get_closest_raider_haven(pos);
                self.squad_retreat_to(sid, haven);
                "low shields"
            } else if health < shape.retreat_health {
                let haven = self.graph_get_closest_haven(pos);
                self.squad_retreat_to(sid, haven);
                "hurt"
            } else if let Some(leader) = led.filter(|_| worth && rallied) {
                let view = self.war.view(leader.def());
                let tolerance = (courage - 1.0).max(0.0) * view.get_total_friendly_threat(pos);
                if shape.flee_threat && view.get_effective_threat(pos) / courage > tolerance {
                    let haven = self.graph_get_closest_haven(pos);
                    self.squad_retreat_to(sid, haven);
                    "threat"
                } else {
                    let target = match shape.aim {
                        Aim::Artillery => self.war_get_arty_target(pos, false),
                        Aim::Standoff => {
                            let at = self.war_get_arty_target(pos, false);
                            self.graph_get_closest_haven(at)
                        }
                        _ => self.war_get_target(leader, false),
                    };
                    self.squad_set_target(sid, target);
                    engage_at = Some(target);
                    ""
                }
            } else {
                let short = if !worth {
                    "worth too little"
                } else if led.is_none() {
                    "led by no core"
                } else {
                    "scattered"
                };
                if self.war.get_effective_threat(pos) > 0.0 || self.war.get_porc_threat(pos) > 0.0 {
                    let haven = self.graph_get_closest_haven(pos);
                    self.squad_retreat_to(sid, haven);
                    self.squad_conducted(sid, None, &format!("{short}, under threat"));
                    self.squad_handler.taskforces.push_back(sid);
                    return;
                }
                let rally = self.war_get_rally_point(pos);
                self.squad_set_target(sid, rally);
                short
            };
            self.squad_conducted(sid, engage_at, why);
            self.squad_handler.taskforces.push_back(sid);
            return;
        }
    }

    /// After a force's conduct: keep its pace, note whether it engages, and
    /// log when it starts (its make-up then) or stops (and why).
    fn squad_conducted(&mut self, sid: SquadId, engage_at: Option<Vec3>, why: &str) {
        let frame = self.frame;
        if self.squads.get(sid).is_some_and(|s| s.shape.marksman) {
            self.taskforce_mark(sid);
        }
        if self.squads.get(sid).is_some_and(|s| s.shape.roles.iter().any(|r| r.spot.is_some())) {
            self.taskforce_spot(sid);
        }
        let Some(s) = self.squads.get_mut(sid) else { return };
        s.last_assignment_frame = frame;
        s.keep_pace(&self.fighters, engage_at.is_some());
        let was = s.engaging;
        s.engaging = engage_at.is_some();
        let ran = if was { s.engaged_speed(frame) } else { None };
        s.track_engaged(&self.fighters, frame);
        if was == s.engaging {
            return;
        }
        let first = s.engaged == 0;
        if s.engaging && first {
            s.engaged = frame;
        }
        let s = &*s;
        let doctrine = s.shape.doctrine.clone();
        let roles = self.taskforces.doctrine(&doctrine).map(|d| d.roles.clone()).unwrap_or_default();
        let census = s.census(&self.fighters);
        let make_up: Vec<String> = roles.iter().zip(&census).map(|(r, n)| format!("{} {n}", r.name)).collect();
        let line = format!(
            "{} cores, {} members ({}), {:.0} metal, rallied {}",
            s.cores(&self.fighters),
            s.fighters.len(),
            make_up.join(", "),
            s.metal_value,
            s.rally_fraction(&self.fighters).map_or("-".to_owned(), |r| format!("{:.0}%", 100.0 * r))
        );
        let pace = if s.paced() > 0.0 { format!("{:.0}", s.paced()) } else { "free".to_owned() };
        let pos = s.get_pos(&self.fighters);
        let threshold = self.squad_engage_threshold(sid);
        let depth = self.taskforce_depth(pos).map_or("-".to_owned(), |d| format!("{d:.2}"));
        let army = self.squad_army_depth(sid).map_or("-".to_owned(), |(d, n)| format!("{d:.2} (n {n})"));
        let line = match engage_at {
            Some(t) => format!(
                "taskforce: the {doctrine} force (squad {sid}) engages{}: {line}; pace {pace}; to go {threshold:.0}; \
                 depth {depth}, our army {army}; target ({:.0}, {:.0})",
                if first { " first" } else { " again" },
                t.x,
                t.z
            ),
            None => {
                format!("taskforce: the {doctrine} force (squad {sid}) breaks off ({why}): {line}{}", engaged_for(ran))
            }
        };
        self.log(&line);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cores_join_the_newest_force_not_engaged() {
        let f = |id, cores, engaging, formed, value| Opening { id, cores, engaging, formed, value };
        let forces = [f(1, 3, false, 100, 900.0), f(2, 1, true, 300, 200.0), f(3, 2, false, 200, 500.0)];
        // Not the engaged force, though it is the newest and least valuable.
        assert_eq!(core_room(&forces, false, false, 0), Some(3));
        // Full forces take no more; with none open, a new force forms.
        assert_eq!(core_room(&forces, false, false, 2), None);
        assert_eq!(core_room(&forces, false, false, 3), Some(3));
        assert_eq!(core_room(&[f(1, 1, true, 100, 250.0)], false, false, 0), None);
        // Pooled with allies, the least valuable takes it, engaged or not.
        assert_eq!(core_room(&forces, true, false, 1), Some(2));
        // A doctrine reinforcing engaged forces (the shield ball): the least
        // valuable with room, engaged or not, as before doctrines.
        assert_eq!(core_room(&forces, false, true, 0), Some(2));
        assert_eq!(core_room(&forces, false, true, 3), Some(2));
        assert_eq!(core_room(&forces, false, true, 2), Some(2));
        assert_eq!(core_room(&forces, false, true, 1), None);
    }

    #[test]
    fn slots_follow_name_prefixes() {
        assert_eq!(Slot::for_name("cloakassault"), Slot::Cloaky);
        assert_eq!(Slot::for_name("shieldassault"), Slot::Shield);
        assert_eq!(Slot::for_name("amphfloater"), Slot::Amph);
        assert_eq!(Slot::for_name("spiderskirm"), Slot::Recluse);
        assert_eq!(Slot::for_name("spiderassault"), Slot::Spider);
        assert_eq!(Slot::for_name("vehassault"), Slot::Veh);
        assert_eq!(Slot::for_name("hoverassault"), Slot::Halberd);
        assert_eq!(Slot::for_name("hoverriot"), Slot::Hover);
        assert_eq!(Slot::for_name("tankassault"), Slot::Tank);
        assert_eq!(Slot::for_name("shipassault"), Slot::Boat);
        assert_eq!(Slot::for_name("gunshipheavyskirm"), Slot::Brawler);
        assert_eq!(Slot::for_name("gunshipassault"), Slot::Air);
        assert!(Slot::Air.is_air() && Slot::Brawler.is_air() && !Slot::Halberd.is_air());
    }

    #[test]
    fn fire_squads_form_with_their_riot_screen_only_when_enabled() {
        for screen in ["spideremp", "spiderriot", "spiderassault"] {
            assert_eq!(Slot::for_assault("spiderskirm", true), Slot::for_assault(screen, true));
            assert_ne!(Slot::for_assault("spiderskirm", false), Slot::for_assault(screen, false));
        }
        for (core, screen) in [("cloakskirm", "cloakriot"), ("hoverskirm", "hoverriot"), ("amphfloater", "amphriot")] {
            assert_eq!(Slot::for_assault(core, true), Slot::for_assault(screen, true));
        }
    }

    #[test]
    fn local_riot_reinforcements_preserve_a_skirmisher_majority() {
        assert!(fire_escort("spideremp") && fire_escort("spiderriot"));
        assert!(!fire_escort("spiderskirm") && !fire_escort("cloakarty"));
        assert!(escort_room(1000.0, 1200.0, 400.0, 400.0));
        assert!(!escort_room(1000.0, 1600.0, 400.0, 400.0));
        assert!(!escort_room(1000.0, 1200.0, 400.0, 900.0));
        assert!(!escort_room(0.0, 1000.0, 100.0, 100.0));
    }

    #[test]
    fn ground_squads_need_value_and_three_units() {
        let s = Slot::Cloaky;
        // Enough value relative to income, but too few units.
        assert!(!s.is_complete(1100.0, 10.0, 2, 10.0));
        assert!(s.is_complete(1100.0, 10.0, 3, 10.0));
        // Value below income * 45.
        assert!(!s.is_complete(1100.0, 30.0, 5, 10.0));
        // Big enough regardless of income.
        assert!(s.is_complete(2600.0, 100.0, 3, 10.0));
        // Never below 1000 metal unless above 2500.
        assert!(!s.is_complete(900.0, 1.0, 5, 10.0));
    }

    #[test]
    fn halberd_squads_scale_with_income() {
        let s = Slot::Halberd;
        assert!(!s.is_complete(0.0, 0.0, 2, 29.0));
        assert!(s.is_complete(0.0, 0.0, 2, 14.0));
        assert!(!s.is_complete(0.0, 0.0, 3, 45.0));
        assert!(s.is_complete(0.0, 0.0, 5, 45.0));
        assert!(s.is_complete(0.0, 0.0, 8, 1000.0));
        assert!(!s.is_complete(0.0, 0.0, 7, 1000.0));
    }

    #[test]
    fn air_squads_only_need_value() {
        assert!(Slot::Air.is_complete(500.0, 10.0, 1, 0.0));
        assert!(!Slot::Brawler.is_complete(450.0, 10.0, 9, 0.0));
    }

    #[test]
    fn squads_keep_to_one_kind_of_ground() {
        let (bot, amph, veh) = (Some(Tier::Bot), Some(Tier::Amphib), Some(Tier::Vehicle));
        assert!(same_ways(bot, bot) && same_ways(bot, amph) && same_ways(amph, bot));
        assert!(!same_ways(bot, veh) && !same_ways(veh, amph) && !same_ways(None, bot));
        assert!(same_ways(None, None));
    }
}
