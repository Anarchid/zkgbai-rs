//! Jaeger squads: hunters of the enemy's heavies.
//!
//! Once heavies (Grizzlies, Cyclopes, Jacks, Jugglenauts, striders...) are
//! a good part of the enemy's army seen lately, or simply many, the
//! factories we have build anti-heavy units, for up to half the heavies'
//! metal: Widows (`spiderantiheavy`, 280 metal: an 8000 stun every 35 s at
//! range 120, and ZK stuns on max health) from a spider factory, and
//! Ultimatums (`striderantiheavy`, 2400 metal: 2000 a shot every 2 s at
//! range 250) from a strider hub. Skuttles (`jumpbomb`: an 8000 blast) are
//! taken in while heavies are about. Every Ultimatum is a jaeger.
//!
//! Jaegers walk cloaked and hold their fire, in squads of a kind, which
//! form until they can take the heavies seen. A squad strikes a heavy
//! together once it can take it: Widows when their stuns together pass its
//! health, Skuttles when their blasts do, an Ultimatum any. A stunned heavy
//! is the army's to kill (it paints no threat while stunned, and a defense
//! target calls our squads), so stuns and Ultimatums go only where our army
//! or ground is near, and none of them under enemy defenses but Skuttles,
//! which die striking anyway. Widows that have fired go back to a haven
//! until they are reloaded.

use std::collections::{BTreeMap, BTreeSet};

use springai::{Resource, Unit, Vec3};

use crate::Ai;
use crate::military::{DefenseTarget, Enemy, Fighter, SquadId, SquadKind};
use crate::nav;
use crate::terrain::reaches;
use crate::util::{NULLPOS, distance};

/// Frames between updates.
const PERIOD: i32 = 15;

/// Enemies seen within this many frames count toward the army seen.
const RECENT: i32 = 30 * 180;

/// A heavy: a mobile ground or sea unit of at least this much health and
/// metal (not a commander).
const HEAVY_HEALTH: f32 = 4500.0;
const HEAVY_COST: f32 = 600.0;

/// Jaegers are wanted from this much heavy metal seen, when it is this
/// share of the enemy's army seen; or from this much alone.
const HEAVY_VALUE: f32 = 2000.0;
const HEAVY_SHARE: f32 = 0.25;
const HEAVY_ALONE: f32 = 6000.0;

/// Jaegers get up to this share of the heavies' metal, and are at most
/// this many.
const JAEGER_SHARE: f32 = 0.5;
const MAX_JAEGERS: usize = 8;

/// A squad grows to at most this many.
const SQUAD_MAX: usize = 4;

/// An order not met in this many frames is forgotten.
const ORDER_TIMEOUT: i32 = 30 * 240;

/// A heavy unseen this long is not hunted (it has moved on).
const STALE: i32 = 30 * 15;

/// A squad strikes from this far beyond its range, and gives up a strike
/// after this many frames.
const STRIKE_RANGE: f32 = 300.0;
const STRIKE_TIMEOUT: i32 = 30 * 25;

/// Our army within this distance of a heavy gets to it while it is
/// stunned (30 s).
const SUPPORT_RANGE: f32 = 1500.0;

/// A kind of jaeger.
#[derive(Clone, Copy)]
struct Kind {
    name: &'static str,
    /// What one strike does to a heavy (stun or damage); `None`: it wears
    /// down any heavy.
    alpha: Option<f32>,
    /// Its strike stuns rather than kills.
    stuns: bool,
    /// It dies striking, so it goes under enemy defenses as well.
    suicide: bool,
    /// Weapon range, elmos.
    range: f32,
    /// Only heavies of at least this much metal are worth one (a Skuttle
    /// costs 550: not for a 600-metal Jack).
    min_value: f32,
}

const KINDS: [Kind; 3] = [
    Kind { name: "spiderantiheavy", alpha: Some(8000.1), stuns: true, suicide: false, range: 120.0, min_value: 0.0 },
    Kind { name: "jumpbomb", alpha: Some(8002.4), stuns: false, suicide: true, range: 25.0, min_value: 1000.0 },
    Kind { name: "striderantiheavy", alpha: None, stuns: false, suicide: false, range: 250.0, min_value: 0.0 },
];

fn kind_of(name: &str) -> Option<Kind> {
    KINDS.iter().copied().find(|k| k.name == name)
}

/// Metal cost of an identified enemy, by its def (`value` stays a
/// placeholder for contacts first met on radar, when cheats name them).
fn cost(e: &Enemy, metal: Resource) -> f32 {
    e.ud.map_or(e.value, |d| d.cost(metal))
}

/// Whether an enemy is a heavy.
fn is_heavy(e: &Enemy, metal: Resource) -> bool {
    is_army(e) && e.def_health() >= HEAVY_HEALTH && cost(e, metal) >= HEAVY_COST
}

/// Whether an enemy counts toward its army: armed ground or sea units, not
/// workers or commanders.
fn is_army(e: &Enemy) -> bool {
    e.identified && !e.is_static && !e.can_fly() && e.can_attack() && !e.is_worker && !e.is_com
}

/// How many of a kind take a heavy of `health`: `None` if more than a
/// squad.
fn needed(kind: Kind, health: f32) -> Option<usize> {
    let n = kind.alpha.map_or(1, |a| (health / a).floor() as usize + 1);
    (n <= SQUAD_MAX).then_some(n)
}

/// An enemy army unit seen lately.
struct Seen {
    value: f32,
    /// Max health, if it is a heavy.
    heavy: Option<f32>,
    last_seen: i32,
}

#[derive(Clone, Copy, PartialEq)]
enum Phase {
    /// Gathering at a haven until it can take the heavies seen.
    Forming,
    /// After a heavy (enemy id), or waiting by our army for one.
    Hunting(Option<i32>),
    /// Attacking a heavy (enemy id) since a frame.
    Striking(i32, i32),
    /// Fired: at a haven until reloaded.
    Reloading,
}

struct Hunt {
    kind: Kind,
    phase: Phase,
}

#[derive(Default)]
pub struct Jaegers {
    /// Enemy army units seen lately, by enemy id.
    seen: BTreeMap<i32, Seen>,
    /// Fighter-store ids of our jaegers.
    members: BTreeSet<i32>,
    /// Our jaeger squads and what each is doing.
    hunts: BTreeMap<SquadId, Hunt>,
    /// The squad forming for each kind.
    forming: BTreeMap<&'static str, SquadId>,
    /// Jaegers ordered from factories, and when.
    ordered: Vec<(&'static str, i32)>,
}

impl Ai {
    /// Metal of heavies and of the whole army seen lately.
    fn jaeger_heavy_value(&self) -> (f32, f32) {
        let seen = self.jaegers.seen.values();
        let heavy = seen.clone().filter(|s| s.heavy.is_some()).map(|s| s.value).sum();
        (heavy, seen.map(|s| s.value).sum())
    }

    /// The most of a kind that the heavies seen need, if any heavy seen
    /// is one a squad of it takes.
    fn jaeger_needed(&self, kind: Kind) -> Option<usize> {
        (self.jaegers.seen.values())
            .filter(|s| s.value >= kind.min_value)
            .filter_map(|s| s.heavy)
            .filter_map(|h| needed(kind, h))
            .max()
    }

    /// Whether a factory that can build `name` should build it now.
    pub fn jaeger_wants(&self, name: &str) -> bool {
        let Some(kind) = kind_of(name) else { return false };
        if self.dev_off("jaeger") || self.jaeger_needed(kind).is_none() {
            return false;
        }
        let (heavy, army) = self.jaeger_heavy_value();
        if heavy < HEAVY_ALONE && (heavy < HEAVY_VALUE || heavy < HEAVY_SHARE * army) {
            return false;
        }
        let frame = self.frame;
        let orders = self.jaegers.ordered.iter().filter(|o| frame - o.1 <= ORDER_TIMEOUT);
        let ordered: f32 = orders.clone().filter_map(|o| self.defs.get(o.0)).map(|d| d.cost(self.metal)).sum();
        let have: f32 = self.jaegers.members.iter().filter_map(|id| self.fighters.get(id)).map(|f| f.metal_value).sum();
        self.jaegers.members.len() + orders.count() < MAX_JAEGERS && have + ordered < JAEGER_SHARE * heavy
    }

    /// A factory has been told to build jaeger `name`.
    pub fn jaeger_ordered(&mut self, name: &str) {
        if let Some(kind) = kind_of(name) {
            self.jaegers.ordered.push((kind.name, self.frame));
        }
    }

    /// Whether a new unit of type `name` is ours to command: an Ultimatum,
    /// a Widow we ordered, or a Skuttle while heavies are about.
    pub fn jaeger_claims(&mut self, name: &str) -> bool {
        let Some(kind) = kind_of(name) else { return false };
        let frame = self.frame;
        self.jaegers.ordered.retain(|o| frame - o.1 <= ORDER_TIMEOUT);
        if let Some(k) = self.jaegers.ordered.iter().position(|o| o.0 == kind.name) {
            self.jaegers.ordered.remove(k);
            return true;
        }
        match kind.name {
            "striderantiheavy" => true,
            "jumpbomb" => self.jaeger_wants(name),
            _ => false,
        }
    }

    /// A new jaeger: it holds its fire (it would show itself) and its
    /// ground, until its squad strikes.
    pub fn jaeger_add(&mut self, unit: Unit, cost: f32) {
        unit.set_move_state(0, 0);
        if self.defs.name_of(unit) == "striderantiheavy" {
            // It is costly and lives through a strike: it goes home hurt.
            unit.set_fire_state(1, 0);
            self.retreat_add_coward(unit);
        } else {
            unit.set_fire_state(0, 0);
        }
        let f = Fighter::new_raider(unit, cost, self.team_id);
        self.jaegers.members.insert(f.id);
        self.fighters.insert(f.id, f);
    }

    pub fn jaeger_remove_unit(&mut self, unit: Unit) {
        let id = unit.id();
        if self.jaegers.members.remove(&id) {
            self.jaeger_leave_squad(id);
            self.fighters.remove(&id);
        }
    }

    pub fn jaeger_enemy_destroyed(&mut self, id: i32) {
        self.jaegers.seen.remove(&id);
    }

    fn jaeger_leave_squad(&mut self, id: i32) {
        let Some(sid) = self.fighters.get(&id).and_then(|f| f.squad) else { return };
        let mut nav = nav!(self);
        if let Some(s) = self.squads.get_mut(sid) {
            s.remove_unit_nav(id, &mut self.fighters, &mut nav);
        }
    }

    pub fn jaeger_update(&mut self, frame: i32) {
        if (frame + self.offset + 11) % PERIOD != 0 {
            return;
        }
        self.jaeger_remember(frame);
        if frame % 1800 < PERIOD {
            let (heavy, army) = self.jaeger_heavy_value();
            if heavy > 0.0 || !self.jaegers.members.is_empty() {
                self.log(&format!(
                    "jaeger: {heavy:.0} of {army:.0} enemy army metal seen lately is heavy; {} jaegers, {} squads, {} ordered",
                    self.jaegers.members.len(),
                    self.jaegers.hunts.len(),
                    self.jaegers.ordered.len()
                ));
            }
        }
        if self.jaegers.members.is_empty() && self.jaegers.hunts.is_empty() {
            return;
        }
        self.jaeger_muster();
        let sids: Vec<SquadId> = self.jaegers.hunts.keys().copied().collect();
        for sid in sids {
            self.jaeger_assign(sid, frame);
        }
    }

    /// Keep count of the enemy's army seen lately.
    fn jaeger_remember(&mut self, frame: i32) {
        for (&id, e) in &self.war.targets {
            if !e.is_dead && is_army(e) {
                let heavy = is_heavy(e, self.metal).then(|| e.def_health());
                self.jaegers.seen.insert(id, Seen { value: cost(e, self.metal), heavy, last_seen: e.last_seen });
            }
        }
        self.jaegers.seen.retain(|_, s| frame - s.last_seen <= RECENT);
    }

    /// Drop the lost and the retreating from their squads, put the rest
    /// that have none in the squad forming for their kind, and drop the
    /// squads left empty.
    fn jaeger_muster(&mut self) {
        let team = self.team_id;
        let ids: Vec<i32> = self.jaegers.members.iter().copied().collect();
        for id in ids {
            let Some(unit) = self.fighters.get(&id).map(|f| f.unit) else {
                self.jaegers.members.remove(&id);
                continue;
            };
            if unit.health() <= 0.0 || unit.team() != team {
                self.jaeger_remove_unit(unit);
                continue;
            }
            if self.retreat_handler.is_retreating(id) {
                self.jaeger_leave_squad(id);
                continue;
            }
            if self.fighters.get(&id).is_some_and(|f| f.squad.is_some()) {
                continue;
            }
            let Some(kind) = kind_of(self.defs.name_of(unit)) else { continue };
            let sid = match self.jaegers.forming.get(kind.name) {
                Some(&sid) if self.squads.contains(sid) => sid,
                _ => {
                    let sid = self.squads.create(SquadKind::Jaeger, team);
                    self.jaegers.forming.insert(kind.name, sid);
                    self.jaegers.hunts.insert(sid, Hunt { kind, phase: Phase::Forming });
                    sid
                }
            };
            let mut nav = nav!(self);
            if let Some(s) = self.squads.get_mut(sid) {
                s.add_unit_nav(id, &mut self.fighters, &mut nav);
            }
        }
        let empty: Vec<SquadId> = (self.jaegers.hunts.keys().copied())
            .filter(|&sid| self.squads.get(sid).is_none_or(|s| s.is_dead(&self.fighters)))
            .collect();
        for sid in empty {
            self.jaegers.hunts.remove(&sid);
            self.jaegers.forming.retain(|_, &mut f| f != sid);
            self.squads.remove(sid);
        }
    }

    /// Positions of our army: squads under way and the big loners.
    fn jaeger_army(&self) -> Vec<Vec3> {
        let squads = (self.squad_handler.squads.iter().chain(&self.squad_handler.taskforces))
            .filter_map(|&sid| self.squads.get(sid))
            .filter(|s| s.status != 'f' && !s.is_dead(&self.fighters))
            .map(|s| s.get_pos(&self.fighters));
        let loners = (self.misc_handler.loners.iter().chain(&self.misc_handler.striders))
            .filter_map(|id| self.fighters.get(id))
            .map(|f| f.get_pos());
        squads.chain(loners).collect()
    }

    /// How dear heavy `id` is to a squad of `n` of `kind` at `from` (the
    /// nearer and the more valuable, the less), if it takes it, gets to it,
    /// and may strike there.
    fn jaeger_cost(&self, kind: Kind, n: usize, from: Vec3, id: i32, army: &[Vec3]) -> Option<f32> {
        let war = &self.war;
        let e = war.targets.get(&id)?;
        // An Ultimatum also takes what the original sent it after: anything
        // big on the ground, commanders too.
        let value = cost(e, self.metal);
        let prey = is_heavy(e, self.metal)
            || (kind.alpha.is_none()
                && e.identified
                && !e.is_static
                && !e.can_fly()
                && value >= HEAVY_COST
                && e.def_health() >= 1000.0);
        if e.is_dead || !prey || value < kind.min_value || e.position == NULLPOS || self.frame - e.last_seen > STALE {
            return None;
        }
        // A stun holds on max health; damage kills what is left.
        let health = if kind.stuns { e.def_health() } else { e.get_health() * e.def_health() };
        if needed(kind, health).is_none_or(|k| k > n) {
            return None;
        }
        let p = e.position;
        if !kind.suicide {
            let supported = war.get_total_friendly_power(p) > 0.0
                || self.graph.is_ally_territory(p)
                || army.iter().any(|&a| distance(a, p) <= SUPPORT_RANGE);
            if war.get_porc_threat(p) > 0.0 || !supported {
                return None;
            }
        }
        let def = self.defs.get(kind.name)?;
        // (A Grizzly under water is out of reach but of water weapons; see
        // `water`.)
        if !war.can_hit(war.reach(Some(def)), e) || !reaches(self.reach_terrain(), def, from, p, kind.range) {
            return None;
        }
        Some(distance(from, p) / (1.0 + value / 1000.0))
    }

    /// The heavy (enemy id) a squad of `n` of `kind` at `from` should go
    /// for, but those in `taken`.
    fn jaeger_target(&self, kind: Kind, n: usize, from: Vec3, taken: &[i32], army: &[Vec3]) -> Option<i32> {
        (self.war.targets.keys().copied())
            .filter(|id| !taken.contains(id))
            .filter_map(|id| Some((id, self.jaeger_cost(kind, n, from, id, army)?)))
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(id, _)| id)
    }

    fn jaeger_assign(&mut self, sid: SquadId, frame: i32) {
        let Some(hunt) = self.jaegers.hunts.get(&sid) else { return };
        let (kind, phase) = (hunt.kind, hunt.phase);
        let Some(s) = self.squads.get(sid) else { return };
        let pos = s.get_pos(&self.fighters);
        let members: Vec<i32> = s.fighters.clone();
        let reloading =
            members.iter().filter(|id| self.fighters.get(id).is_some_and(|f| f.is_reloading(frame))).count();
        let ready = members.len() - reloading;
        let taken: Vec<i32> = (self.jaegers.hunts.iter())
            .filter(|&(&o, _)| o != sid)
            .filter_map(|(_, h)| match h.phase {
                Phase::Hunting(Some(t)) | Phase::Striking(t, _) => Some(t),
                _ => None,
            })
            .collect();
        let army = self.jaeger_army();

        let next = match phase {
            Phase::Forming => {
                let full = members.len() >= SQUAD_MAX
                    || self.jaeger_needed(kind).is_some_and(|n| members.len() >= n)
                    || self.jaeger_target(kind, members.len(), pos, &taken, &army).is_some();
                if full {
                    self.jaegers.forming.retain(|_, &mut f| f != sid);
                    self.log(&format!("jaeger: a squad of {} {} hunts", members.len(), kind.name));
                    Phase::Hunting(None)
                } else {
                    let haven = self.graph_get_closest_haven(pos);
                    self.jaeger_go(sid, haven, false);
                    Phase::Forming
                }
            }
            Phase::Hunting(target) => {
                let keep = target.filter(|&t| self.jaeger_cost(kind, ready, pos, t, &army).is_some());
                let target = keep.or_else(|| self.jaeger_target(kind, ready, pos, &taken, &army));
                match target.and_then(|t| Some((t, self.jaeger_heavy(t)?))) {
                    Some((t, (at, unit, value))) if distance(pos, at) <= kind.range + STRIKE_RANGE => {
                        self.jaeger_strike(sid, unit, true);
                        self.log(&format!(
                            "jaeger: {ready} {} strike a heavy ({value:.0} metal) at ({:.0}, {:.0})",
                            kind.name, at.x, at.z
                        ));
                        Phase::Striking(t, frame)
                    }
                    Some((t, (at, ..))) => {
                        if phase != Phase::Hunting(Some(t)) {
                            self.log(&format!(
                                "jaeger: {} {} go after a heavy at ({:.0}, {:.0})",
                                ready, kind.name, at.x, at.z
                            ));
                        }
                        self.jaeger_go(sid, at, false);
                        Phase::Hunting(Some(t))
                    }
                    None => {
                        // Wait by our army, out of the way.
                        let rally = self.war_get_rally_point(pos);
                        let wait = if rally == NULLPOS { self.graph_get_closest_haven(pos) } else { rally };
                        self.jaeger_go(sid, wait, false);
                        Phase::Hunting(None)
                    }
                }
            }
            Phase::Striking(t, since) => {
                match self.jaeger_heavy(t) {
                    None => Phase::Hunting(None),
                    Some((at, unit, value)) => {
                        if unit.is_paralyzed() {
                            // The army's to kill.
                            self.war.refresh_defense_target(DefenseTarget::new(at, value, frame));
                        }
                        if kind.stuns && reloading > 0 && reloading == members.len() {
                            if unit.is_paralyzed() {
                                self.log(&format!("jaeger: a heavy stunned at ({:.0}, {:.0})", at.x, at.z));
                            }
                            let haven = self.graph_get_closest_haven(pos);
                            self.jaeger_go(sid, haven, true);
                            Phase::Reloading
                        } else if frame - since > STRIKE_TIMEOUT
                            || (!kind.suicide && self.war.get_porc_threat(at) > 0.0)
                        {
                            Phase::Hunting(None)
                        } else {
                            self.jaeger_strike(sid, unit, false);
                            Phase::Striking(t, since)
                        }
                    }
                }
            }
            Phase::Reloading => {
                if reloading == 0 {
                    Phase::Hunting(None)
                } else {
                    let haven = self.graph_get_closest_haven(pos);
                    self.jaeger_go(sid, haven, true);
                    Phase::Reloading
                }
            }
        };
        if let Some(h) = self.jaegers.hunts.get_mut(&sid) {
            h.phase = next;
        }
    }

    /// Where a heavy (enemy id) is, its unit and its metal, while it lives.
    fn jaeger_heavy(&self, id: i32) -> Option<(Vec3, Unit, f32)> {
        let e = self.war.targets.get(&id).filter(|e| !e.is_dead)?;
        Some((e.position, e.unit, cost(e, self.metal)))
    }

    /// Send a squad toward `to`, cloaked and by the ways clear of the
    /// enemy; `away` after a strike, straight off.
    fn jaeger_go(&mut self, sid: SquadId, to: Vec3, away: bool) {
        if to == NULLPOS {
            return;
        }
        let mut nav = nav!(self);
        let Some(s) = self.squads.get_mut(sid) else { return };
        if away {
            s.retreat_to(to, &mut self.fighters, &mut nav);
        } else if distance(s.get_pos(&self.fighters), to) > 100.0 {
            s.set_target(to, &mut self.fighters, &mut nav);
        }
    }

    /// A squad attacks `target`: all of it, or (not `all`) those left idle
    /// that are not reloading.
    fn jaeger_strike(&mut self, sid: SquadId, target: Unit, all: bool) {
        let Some(s) = self.squads.get(sid) else { return };
        let frame = self.frame;
        let strikers: Vec<Unit> = (s.fighters.iter().filter_map(|id| self.fighters.get(id)))
            .filter(|f| all || (f.unit.num_current_commands() == 0 && !f.is_reloading(frame)))
            .map(|f| f.unit)
            .collect();
        for unit in strikers {
            // Skuttles jump the last stretch onto it (see `jumps`).
            if !self.jump_strike(unit, target, NULLPOS, "skuttle") {
                unit.attack(target, 0);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn squads_take_what_their_strikes_pass() {
        let widow = kind_of("spiderantiheavy").unwrap();
        let ulti = kind_of("striderantiheavy").unwrap();
        // Grizzly, Cyclops, Paladin, Detriment.
        assert_eq!(needed(widow, 8700.0), Some(2));
        assert_eq!(needed(widow, 12000.0), Some(2));
        assert_eq!(needed(widow, 32000.0), Some(4));
        assert_eq!(needed(widow, 76000.0), None);
        assert_eq!(needed(ulti, 76000.0), Some(1));
    }
}
