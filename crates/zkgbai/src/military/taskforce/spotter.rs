//! Spotters: a member of a doctrine's role with `spot`, one per force
//! whatever its number of guns, who holds the core's mark in sight from
//! the mark to the volley. A Lance or a Phantom firing at a radar dot hits
//! about half as often as at a target in sight (0.49 against 0.90 for the
//! Lance over 472 games, predictor/LOS-READ.md), because dots wobble.
//!
//! The errand is timed to the gun that fires next: early enough before
//! its reload to get there (its way at its speed, and 2 s; 12 s at most),
//! the spotter goes within sight of that gun's mark, or of the target it
//! would mark were it in sight (one shot kills it, worth its floor: radar
//! dots of a type we have seen count), and holds there until the gun has
//! fired; then it comes back to the force. Spotters hold fire by their
//! role's `fire_state`. How it stands, by the role's `spot` (`any`: by the
//! unit's type):
//!
//! - `tank`: an armored scout (a Halberd, armored while closed) drives to
//!   within 0.6 of its sight of the target; hurt, it goes for repairs (the
//!   retreat handler has it) and, healed, is assigned again;
//! - `cloak`: a cloaked unit (a Widow, Scythe or Gremlin) stands off within
//!   sight, past its decloak distance;
//! - `air`: a plane (a Sparrow) overflies the target at the volley and
//!   waits over the force; fired on, it flees home.
//!
//! A force short of its spotter calls one from our standing units, in the
//! order of the role's `units` (a plane only while the enemy shows no
//! anti-air, seen or felt), from wherever it serves (assault squads,
//! raiders, scouts, anti-heavy hunters, anti-air); none is built for it,
//! but for one Sparrow a game from a standing plane factory (see
//! `handlers::squad::squad_produce`). One just finished is claimed too,
//! before the handlers that would take it; a spotter no force wants goes
//! back to them.
//!
//! Any doctrine of the squad handler may have the role (a siege doctrine as
//! a lance one): the squad handler calls [`Ai::taskforce_spot`] after each
//! force's conduct, [`Ai::taskforce_spotter_claims`] as units are
//! finished and [`Ai::taskforce_spotter_lost`] as they die.
//!
//! Log lines (`taskforce:`): `a X is called to spot for the D force (squad
//! N, state), M from its leader (unit U)`; `a X spots for the D force
//! (squad N, state), M from its leader (unit U)`; `a X spotting for the D
//! force (squad N) goes to see a Y (W metal, in sight|on radar) for a G's
//! volley in T s, R away`; `... sights|identifies a Y (M metal), R from the
//! guns`; `a X spotter of the D force (squad N, state) goes for repairs at
//! H% health|flees anti-air fire, R from the force`; `a X spotter of the D
//! force (squad N, state) is lost JOB to a K, R from the force` (JOB:
//! escorting, scouting, loitering, overflight, retreating).

use std::collections::{BTreeMap, BTreeSet};

use springai::{NO_TIMEOUT, Unit, UnitDef, Vec3};

use super::marksman::Fix;
use crate::Ai;
use crate::defs::cmd;
use crate::kgbutil::kgb_move_to;
use crate::military::{Fighter, SquadId};
use crate::nav;
use crate::util::{NULLPOS, OPTION_SHIFT_KEY, distance, get_directional_point};

/// How a spotter stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Spot {
    Tank,
    Cloak,
    Air,
    /// By the unit's type: a plane, a cloaked unit, or else a tank.
    Any,
}

const SPOTS: [Spot; 4] = [Spot::Tank, Spot::Cloak, Spot::Air, Spot::Any];

impl Spot {
    pub fn name(self) -> &'static str {
        match self {
            Spot::Tank => "tank",
            Spot::Cloak => "cloak",
            Spot::Air => "air",
            Spot::Any => "any",
        }
    }

    pub fn parse(s: &str) -> Result<Spot, String> {
        SPOTS.into_iter().find(|a| a.name() == s).ok_or_else(|| {
            let names: Vec<&str> = SPOTS.iter().map(|a| a.name()).collect();
            format!("spot '{s}': one of {}", names.join(", "))
        })
    }

    /// Whether unit type `d` spots this way: planes fly, loiterers cloak.
    pub fn fits(self, d: UnitDef) -> Result<(), String> {
        match self {
            Spot::Air if !d.is_able_to_fly() => Err("cannot fly (spot air)".into()),
            Spot::Cloak if !d.is_able_to_cloak() => Err("cannot cloak (spot cloak)".into()),
            Spot::Tank | Spot::Cloak if d.is_able_to_fly() => Err("flies (spot air)".into()),
            _ => Ok(()),
        }
    }

    /// How a unit of type `d` spots, for a role spotting this way.
    pub fn way(self, d: Option<UnitDef>) -> Spot {
        match (self, d) {
            (Spot::Any, Some(d)) if d.is_able_to_fly() => Spot::Air,
            (Spot::Any, Some(d)) if d.is_able_to_cloak() => Spot::Cloak,
            (Spot::Any, _) => Spot::Tank,
            (s, _) => s,
        }
    }

    /// Health share below which it goes for repairs (a plane flees when
    /// fired on instead).
    fn repair(self) -> f32 {
        match self {
            Spot::Tank | Spot::Any => 0.6,
            Spot::Cloak => 0.5,
            Spot::Air => 0.0,
        }
    }

    /// What it does on an errand, for the log.
    fn spotting(self) -> &'static str {
        match self {
            Spot::Tank | Spot::Any => "scouting",
            Spot::Cloak => "loitering",
            Spot::Air => "overflight",
        }
    }

    /// Seconds to spare on arriving before the volley.
    fn lead(self) -> f32 {
        if self == Spot::Air { 1.0 } else { 2.0 }
    }
}

/// A tank stops this share of its sight short of the target; a loiterer
/// this share, and its decloak distance plus `CLOAK_MARGIN` at least.
const TANK_SIGHT: f32 = 0.6;
const CLOAK_SIGHT: f32 = 0.8;
const CLOAK_MARGIN: f32 = 120.0;

/// An errand starts at most this long before the volley.
const MAX_LEAD: i32 = 12 * 30;

/// An errand whose volley has not come this long after it began is given
/// up.
const ERRAND_FRAMES: i32 = 25 * 30;

/// Reach beyond the gun's range for a target to see.
const REACH: f32 = 20.0;

/// Orders are given again when the point moves this far, or this often.
const RESEND_MOVED: f32 = 120.0;
const RESEND_FRAMES: i32 = 90;

/// An escorting spotter this far from the leader is sent back to it.
const STRAYED: f32 = 600.0;

/// What a spotter is doing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Job {
    /// With the force, orbiting the leader (a plane: over it).
    Escorting,
    /// Holding `target` in sight for `gun`'s volley: done once the gun's
    /// reload frame is no longer `reload`.
    Spotting { gun: i32, target: i32, reload: i32 },
    /// Gone for repairs, or fled (the retreat handler has it).
    Repairing,
}

#[derive(Clone, Debug)]
pub struct Spotter {
    pub unit: i32,
    pub sid: SquadId,
    /// How it stands (the role's `spot`, resolved for its type).
    pub spot: Spot,
    pub job: Job,
    since: i32,
    sent: Vec3,
    sent_at: i32,
    /// Escorting: told to orbit the leader since its last errand.
    orbiting: bool,
    /// Spotting: the target was never seen when the errand began, and
    /// whether it has come into sight since.
    unknown: bool,
    sighted: bool,
}

impl Spotter {
    /// The job's name in the log.
    pub fn doing(&self) -> &'static str {
        match self.job {
            Job::Escorting => "escorting",
            Job::Spotting { .. } => self.spot.spotting(),
            Job::Repairing => "retreating",
        }
    }

    /// The target it holds in sight for `gun`, if on such an errand.
    pub fn errand_for(&self, gun: i32) -> Option<i32> {
        match self.job {
            Job::Spotting { gun: g, target, .. } if g == gun => Some(target),
            _ => None,
        }
    }

    /// Whether it holds `target` in sight for `gun`.
    pub fn serves(&self, gun: i32, target: i32) -> bool {
        self.errand_for(gun) == Some(target)
    }
}

/// Spotters by unit id, those let go (never claimed again as they are
/// finished), and how many were built for forces this game.
#[derive(Default)]
pub struct Spotters {
    pub by_unit: BTreeMap<i32, Spotter>,
    released: BTreeSet<i32>,
    pub ordered: u32,
}

fn health_share(u: Unit) -> f32 {
    let max = u.max_health();
    if max > 0.0 { u.health() / max } else { 0.0 }
}

/// Where a spotter goes to see a target at `at` for a gun at `gun`.
fn spot_point(spot: Spot, def: Option<UnitDef>, at: Vec3, gun: Vec3) -> Vec3 {
    let sight = def.map_or(400.0, |d| d.los_radius());
    let off = match spot {
        Spot::Air => return at,
        Spot::Tank | Spot::Any => TANK_SIGHT * sight,
        Spot::Cloak => (CLOAK_SIGHT * sight).max(def.map_or(0.0, |d| d.decloak_distance()) + CLOAK_MARGIN),
    };
    if distance(at, gun) <= off {
        return gun;
    }
    let p = get_directional_point(at, gun, off);
    Vec3::new(p.x, at.y, p.z)
}

/// A core gun of a force, for the errands.
struct Gun {
    id: i32,
    unit: Unit,
    /// When it fires next (its reload frame, or now if loaded).
    next: i32,
    reload: i32,
}

impl Ai {
    /// Whether the enemy shows anti-air: seen, or felt (hits on our
    /// aircraft, see `air_intel`).
    pub fn taskforce_enemy_aa(&self) -> bool {
        self.war.targets.values().any(|e| !e.is_dead && (e.is_aa || e.is_flex_aa))
            || !self.air_intel_threat().is_empty()
    }

    /// Whether a unit of type `d` may spot now (a plane not against
    /// anti-air).
    fn taskforce_spotter_may(&self, spot: Spot, d: Option<UnitDef>) -> bool {
        spot.way(d) != Spot::Air || !self.taskforce_enemy_aa()
    }

    /// A unit just finished that some force short of a spotter takes:
    /// it joins (see the module notes). False: it goes its usual way.
    pub fn taskforce_spotter_claims(&mut self, unit: Unit, name: &str, cost: f32) -> bool {
        self.taskforce_load();
        if self.taskforces.spotters.released.contains(&unit.id()) {
            return false;
        }
        let tf = &self.taskforces;
        let wanted: Vec<(String, usize, Spot)> = (tf.doctrines.iter())
            .filter(|d| d.enabled && super::squad_aim(d.aim))
            .filter_map(|d| {
                let r = d.role_of(name, &tf.classes)?;
                d.roles[r].spot.map(|s| (d.name.clone(), r, s))
            })
            .collect();
        let short = wanted.iter().any(|(d, r, s)| {
            self.taskforce_spotter_may(*s, unit.def())
                && (self.squad_handler.taskforces.iter())
                    .filter(|&&sid| self.squads.get(sid).is_some_and(|q| &q.shape.doctrine == d))
                    .any(|&sid| self.squad_short_of(sid, *r))
        });
        if !short {
            return false;
        }
        if unit.def().is_some_and(|d| d.is_able_to_fly()) {
            unit.set_idle_mode(0, 0);
        }
        self.squad_add_taskforce_member(Fighter::new(unit, cost, self.team_id));
        true
    }

    /// A spotter no force wants goes back to its usual handler, for good.
    pub(crate) fn taskforce_spotter_release(&mut self, id: i32) {
        let Some(unit) = self.fighters.get(&id).map(|f| f.unit) else { return };
        self.taskforces.spotters.by_unit.remove(&id);
        self.taskforces.spotters.released.insert(id);
        self.squad_forget_fighter(id);
        unit.stop(0);
        self.war_unit_finished(unit);
        self.war_unit_idle(unit);
    }

    /// A unit for spotting built at a force's call (see `squad_produce`).
    pub(crate) fn taskforce_note_produced(&mut self, doctrine: &str, role: &str) {
        let spots = self.taskforces.doctrine(doctrine).and_then(|d| d.role(role)).is_some_and(|r| r.spot.is_some());
        if spots {
            self.taskforces.spotters.ordered += 1;
        }
    }

    /// Take `unit` from whatever handler has it, for a force: false if it is
    /// busy (retreating, home defense, another force).
    fn taskforce_detach(&mut self, unit: Unit) -> bool {
        let id = unit.id();
        if self.retreat_handler.is_retreating(id) || self.qrf_claims(id) || unit.is_being_built() {
            return false;
        }
        let in_force = self.fighters.get(&id).and_then(|f| f.squad).and_then(|s| self.squads.get(s));
        if in_force.is_some_and(|q| q.is_taskforce()) || self.taskforces.spotters.by_unit.contains_key(&id) {
            return false;
        }
        if self.squad_handler.fighters.contains(&id) {
            self.squad_forget_fighter(id);
        }
        self.raider_remove_unit(unit);
        self.jaeger_remove_unit(unit);
        self.scouts_release(id);
        self.war.aas.remove(&id);
        self.war.new_units.remove(&id);
        self.fighters.remove(&id);
        unit.stop(0);
        true
    }

    /// Force `sid`, short of its spotter, calls one from our standing units
    /// (see the module notes).
    fn taskforce_spotter_call(&mut self, sid: SquadId, role: usize) {
        let Some(s) = self.squads.get(sid) else { return };
        let Some(leader) = s.get_leader(&self.fighters) else { return };
        let Some(d) = self.taskforces.doctrine(&s.shape.doctrine) else { return };
        let r = &d.roles[role];
        let (spot, units) = (r.spot.unwrap_or(Spot::Any), r.units.clone());
        let (doctrine, state) = (d.name.clone(), force_state(s.engaging, s.engaged));
        let lpos = leader.pos();
        let standing = self.cb.team_units();
        for name in &units {
            let Some(def) = self.defs.get(name) else { continue };
            if !self.taskforce_spotter_may(spot, Some(def)) {
                continue;
            }
            let mut ours: Vec<Unit> =
                standing.iter().copied().filter(|u| u.def().is_some_and(|d| d.id() == def.id())).collect();
            ours.sort_by(|a, b| distance(a.pos(), lpos).total_cmp(&distance(b.pos(), lpos)));
            for unit in ours {
                if unit.health() <= 0.0 || !self.taskforce_detach(unit) {
                    continue;
                }
                let id = unit.id();
                let mut f = Fighter::new(unit, def.cost(self.metal), self.team_id);
                f.shield_mob = true;
                self.fighters.insert(id, f);
                self.squad_handler.fighters.insert(id);
                self.retreat_remove_unit(unit);
                if def.is_able_to_fly() {
                    unit.set_idle_mode(0, 0);
                }
                let mut nav = nav!(self);
                if let Some(s) = self.squads.get_mut(sid) {
                    s.add_unit_nav(id, &mut self.fighters, &mut nav);
                }
                self.log(&format!(
                    "taskforce: a {name} is called to spot for the {doctrine} force (squad {sid}, {state}), {:.0} from \
                     its leader (unit {id})",
                    distance(unit.pos(), lpos)
                ));
                return;
            }
        }
    }

    /// The target a spotter should hold in sight for `gun`: its mark, or
    /// what it would mark were it in sight (see the module notes).
    fn taskforce_spot_target(&self, gun: &Gun, share: f32, shields: &[(Vec3, f32)]) -> Option<(i32, f32)> {
        let w = gun.unit.weapon(0)?;
        let damage = w.def().and_then(|d| d.damage_types().first().copied()).unwrap_or(0.0);
        let (pos, reach) = (gun.unit.pos(), w.range() + REACH);
        let marked = self.taskforces.marks.get(&gun.id).map(|m| m.target);
        if let Some(e) = marked.and_then(|t| self.war.targets.get(&t))
            && let Ok((worth, _)) = self.marksman_shot(e, damage, shields, Fix::Radar)
        {
            return Some((e.unit_id, worth));
        }
        let floor = self.marksman_floor(gun.unit, share);
        let taken: BTreeSet<i32> = self.taskforces.marks.values().map(|m| m.target).collect();
        (self.war.targets.values())
            .filter(|e| !e.can_fly() && e.def_name != "wolverine_mine" && !taken.contains(&e.unit_id))
            .filter(|e| distance(pos, e.position) <= reach)
            .filter_map(|e| self.marksman_shot(e, damage, shields, Fix::Radar).ok().map(|(w, _)| (e.unit_id, w)))
            .filter(|&(_, w)| w >= floor)
            .max_by(|a, b| a.1.total_cmp(&b.1))
    }

    /// Steer the spotters of force `sid` (after its conduct).
    pub(crate) fn taskforce_spot(&mut self, sid: SquadId) {
        let frame = self.frame;
        let Some(s) = self.squads.get(sid) else { return };
        let shape = s.shape.clone();
        let members: Vec<(i32, Unit, Spot)> = (s.everyone().filter_map(|id| self.fighters.get(&id)))
            .filter_map(|f| shape.spot(f.unit.def().map(|d| d.id())).map(|sp| (f.id, f.unit, sp.way(f.unit.def()))))
            .collect();
        // Spotters that left the force (other than for repairs) are done.
        let here: BTreeSet<i32> = members.iter().map(|m| m.0).collect();
        (self.taskforces.spotters.by_unit)
            .retain(|id, sp| sp.sid != sid || sp.job == Job::Repairing || here.contains(id));
        let Some(leader) = s.get_leader(&self.fighters) else { return };
        let lpos = leader.pos();
        let state = force_state(s.engaging, s.engaged);
        let guns: Vec<Gun> = (s.everyone().filter_map(|id| self.fighters.get(&id)))
            .filter(|f| shape.is_core(f.unit.def().map(|d| d.id())))
            .filter_map(|f| {
                let reload = f.unit.weapon(0)?.reload_frame();
                Some(Gun { id: f.id, unit: f.unit, next: reload.max(frame), reload })
            })
            .collect();
        if guns.is_empty() {
            return;
        }
        // Short of its spotter (none with it, none away for repairs): call
        // one from the standing units.
        let role = shape.roles.iter().position(|r| r.spot.is_some());
        let away = self.taskforces.spotters.by_unit.values().any(|sp| sp.sid == sid && sp.job == Job::Repairing);
        if let Some(role) = role.filter(|_| members.is_empty() && !away)
            && self.squad_short_of(sid, role)
        {
            self.taskforce_spotter_call(sid, role);
            return;
        }
        let shields = self.impalers_shields();
        let share = shape.mark_share;
        let orbit = |unit: Unit| shape.orbit(unit.def().map(|d| d.id()));
        let mut lines = Vec::new();
        for (id, unit, spot) in members {
            let name = self.defs.name_of(unit).to_owned();
            let entry = self.taskforces.spotters.by_unit.get(&id);
            if entry.is_none_or(|sp| sp.sid != sid || sp.job == Job::Repairing) {
                lines.push(format!(
                    "taskforce: a {name} spots for the {} force (squad {sid}, {state}), {:.0} from its leader (unit {id})",
                    shape.doctrine,
                    distance(unit.pos(), lpos)
                ));
                let sp = Spotter {
                    unit: id,
                    sid,
                    spot,
                    job: Job::Escorting,
                    since: frame,
                    sent: NULLPOS,
                    sent_at: i32::MIN / 2,
                    orbiting: true,
                    unknown: false,
                    sighted: false,
                };
                self.taskforces.spotters.by_unit.insert(id, sp);
            }
            if health_share(unit) < spot.repair() {
                lines.push(format!(
                    "taskforce: a {name} spotter of the {} force (squad {sid}, {state}) goes for repairs at {:.0}% \
                     health, {:.0} from the force",
                    shape.doctrine,
                    100.0 * health_share(unit),
                    distance(unit.pos(), lpos)
                ));
                self.taskforce_spotter_away(id, unit);
                continue;
            }
            // The gun that fires next, and what to see for it.
            let Some(gun) = guns.iter().min_by_key(|g| g.next) else { continue };
            let target = self.taskforce_spot_target(gun, share, &shields);
            let Some(sp) = self.taskforces.spotters.by_unit.get_mut(&id) else { continue };
            if let Job::Spotting { gun: g, target: t, reload } = sp.job {
                let fired = guns.iter().find(|x| x.id == g).is_none_or(|x| x.reload != reload && x.reload > frame);
                let gone = self.war.targets.get(&t).is_none_or(|e| e.is_dead);
                if fired || gone || frame - sp.since > ERRAND_FRAMES {
                    sp.job = Job::Escorting;
                    sp.since = frame;
                    sp.orbiting = false;
                } else if let Some(e) = self.war.targets.get(&t).filter(|e| e.visible && !sp.sighted) {
                    sp.sighted = true;
                    lines.push(format!(
                        "taskforce: a {name} spotting for the {} force (squad {sid}) {} a {} ({:.0} metal), {:.0} from \
                         the guns",
                        shape.doctrine,
                        if sp.unknown { "identifies" } else { "sights" },
                        e.def_name,
                        e.ud.map_or(e.value, |d| d.cost(self.metal)),
                        distance(e.position, gun.unit.pos())
                    ));
                }
            }
            if sp.job == Job::Escorting
                && let Some((t, worth)) = target
                && let Some(e) = self.war.targets.get(&t)
            {
                let to = spot_point(spot, unit.def(), e.position, gun.unit.pos());
                let speed = unit.def().map_or(60.0, |d| d.speed()).max(1.0);
                let need = (30.0 * (distance(unit.pos(), to) / speed + spot.lead())) as i32;
                if gun.next - frame <= need.min(MAX_LEAD) {
                    sp.job = Job::Spotting { gun: gun.id, target: t, reload: gun.reload };
                    sp.since = frame;
                    sp.sent = NULLPOS;
                    sp.unknown = !e.identified;
                    sp.sighted = e.visible;
                    lines.push(format!(
                        "taskforce: a {name} spotting for the {} force (squad {sid}) goes to see a {} ({worth:.0} metal, \
                         {}) for a {}'s volley in {:.0} s, {:.0} away",
                        shape.doctrine,
                        e.def_name,
                        if e.visible { "in sight" } else { "on radar" },
                        self.defs.name_of(gun.unit),
                        (gun.next - frame) as f32 / 30.0,
                        distance(unit.pos(), to)
                    ));
                }
            }
            match sp.job {
                Job::Spotting { gun: g, target: t, .. } => {
                    let Some(e) = self.war.targets.get(&t) else { continue };
                    let gpos = guns.iter().find(|x| x.id == g).map_or(lpos, |x| x.unit.pos());
                    let to = spot_point(spot, unit.def(), e.position, gpos);
                    if distance(to, sp.sent) > RESEND_MOVED || frame - sp.sent_at > RESEND_FRAMES {
                        sp.sent = to;
                        sp.sent_at = frame;
                        sp.orbiting = false;
                        if spot == Spot::Air {
                            unit.move_to(to, 0);
                        } else {
                            kgb_move_to(unit, to, 0, frame);
                        }
                    }
                }
                Job::Escorting => {
                    let back = !sp.orbiting || distance(unit.pos(), lpos) > STRAYED;
                    if back && frame - sp.sent_at > RESEND_FRAMES {
                        sp.orbiting = true;
                        sp.sent_at = frame;
                        if spot == Spot::Air {
                            unit.move_to(lpos, 0);
                        } else {
                            let lid = leader.id() as f32;
                            unit.execute_custom_command(cmd::ORBIT, &[lid, orbit(unit)], 0, NO_TIMEOUT);
                            unit.execute_custom_command(cmd::ORBIT_DRAW, &[lid], OPTION_SHIFT_KEY, NO_TIMEOUT);
                        }
                    }
                }
                Job::Repairing => {}
            }
        }
        for line in lines {
            self.log(&line);
        }
    }

    /// Spotter `id` goes for repairs, or flees: out of its force for good.
    fn taskforce_spotter_away(&mut self, id: i32, unit: Unit) {
        if let Some(sp) = self.taskforces.spotters.by_unit.get_mut(&id) {
            sp.job = Job::Repairing;
            sp.since = self.frame;
        }
        self.retreat_send(unit);
    }

    /// A spotter plane fired on flees home (see the module notes).
    pub fn taskforce_spotter_damaged(&mut self, unit: Unit) {
        let id = unit.id();
        let Some(sp) = self.taskforces.spotters.by_unit.get(&id) else { return };
        if sp.spot != Spot::Air || sp.job == Job::Repairing {
            return;
        }
        let sid = sp.sid;
        let force = self.squads.get(sid).filter(|s| s.is_taskforce());
        let (doctrine, state, far) = match force {
            Some(s) => (
                s.shape.doctrine.clone(),
                force_state(s.engaging, s.engaged),
                s.get_leader(&self.fighters).map_or(0.0, |l| distance(l.pos(), unit.pos())),
            ),
            None => ("gone".to_owned(), "gone", 0.0),
        };
        self.log(&format!(
            "taskforce: a {} spotter of the {doctrine} force (squad {sid}, {state}) flees anti-air fire, {far:.0} from \
             the force",
            self.defs.name_of(unit)
        ));
        self.taskforce_spotter_away(id, unit);
    }

    /// A spotter's death, in its force or gone for repairs.
    pub fn taskforce_spotter_lost(&mut self, unit: Unit, attacker: Option<Unit>) {
        let Some(sp) = self.taskforces.spotters.by_unit.remove(&unit.id()) else { return };
        let by = attacker.and_then(|a| a.def()).map_or("?".to_owned(), |d| self.defs.name(d).to_owned());
        let force = self.squads.get(sp.sid).filter(|s| s.is_taskforce());
        let (doctrine, state, far) = match force {
            Some(s) => (
                s.shape.doctrine.clone(),
                force_state(s.engaging, s.engaged),
                s.get_leader(&self.fighters)
                    .map_or("-".to_owned(), |l| format!("{:.0}", distance(l.pos(), unit.pos()))),
            ),
            None => ("gone".to_owned(), "gone", "-".to_owned()),
        };
        self.log(&format!(
            "taskforce: a {} spotter of the {doctrine} force (squad {}, {state}) is lost {} to a {by}, {far} from the \
             force",
            self.defs.name_of(unit),
            sp.sid,
            sp.doing()
        ));
    }

    /// What spotter `id` is doing, for the death line (`None`: no spotter).
    pub fn taskforce_spotter_doing(&self, id: i32) -> Option<&'static str> {
        self.taskforces.spotters.by_unit.get(&id).map(Spotter::doing)
    }
}

/// A force's state for the log: forming (never engaged), engaged, rallying.
pub fn force_state(engaging: bool, engaged: i32) -> &'static str {
    match (engaging, engaged) {
        (true, _) => "engaged",
        (false, 0) => "forming",
        _ => "rallying",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spots_parse() {
        assert_eq!(Spot::parse("cloak"), Ok(Spot::Cloak));
        assert_eq!(Spot::parse("any"), Ok(Spot::Any));
        assert!(Spot::parse("boat").unwrap_err().contains("tank, cloak, air, any"));
    }

    #[test]
    fn spot_points_stand_off_toward_the_gun() {
        let (at, gun) = (Vec3::xz(1000.0, 0.0), Vec3::xz(0.0, 0.0));
        let p = spot_point(Spot::Tank, None, at, gun);
        assert!((p.x - 760.0).abs() < 1.0 && p.z.abs() < 1.0, "{p:?}");
        assert_eq!(spot_point(Spot::Air, None, at, gun), at);
        // Closer than the stand-off: stay with the gun.
        assert_eq!(spot_point(Spot::Tank, None, Vec3::xz(100.0, 0.0), gun), gun);
    }
}
