//! Spotters: members of a doctrine's role with `spot`, who see for the
//! core's guns instead of fighting. Guns that outrange their own sight
//! (a Lance sees 660 and reaches 980, a Phantom 400 and 700) shoot what is
//! only a radar dot blind; a spotter goes where a dot lies within the guns'
//! reach (or nearly), until it is identified, then to the next, and with
//! nothing to look at orbits the force as any member. Spotters hold fire
//! by their role's `fire_state`. Three ways, by the role's `spot`:
//!
//! - `tank`: an armored scout (a Halberd, armored while closed) drives up
//!   to within sight of the dot;
//! - `cloak`: a cloaked loiterer (a Scythe, Widow or Gremlin) stands off
//!   within sight, beyond its decloak distance;
//! - `air`: a plane (a Sparrow or Owl) flies over the dot and back.
//!
//! Hurt, a spotter goes for repairs (the retreat handler has it) and, healed,
//! rejoins a force short of a spotter. Spotters are claimed as they are
//! finished, before the handlers that would take them otherwise (Widows
//! scout, Sparrows raid), and only while some force is short of one; a
//! spotter no force wants goes back to them.
//!
//! Any doctrine of the squad handler may have the role (a siege doctrine as
//! a lance one): the squad handler calls [`Ai::taskforce_spot`] after each
//! force's conduct, [`Ai::taskforce_spotter_claims`] as units are
//! finished and [`Ai::taskforce_spotter_lost`] as they die.
//!
//! Log lines (`taskforce:`): `a X spots for the D force (squad N, state), M
//! from its leader (unit U)`; `a X spotting for the D force (squad N)
//! identifies a Y (M metal), R from the guns`; `a X spotter of the D force
//! (squad N, state) goes for repairs at H health, R from the force`; `a X
//! spotter of the D force (squad N, state) is lost JOB to a K, R from the
//! force` (JOB: escorting, scouting, loitering, overflight, retreating).

use std::collections::{BTreeMap, BTreeSet};

use springai::{NO_TIMEOUT, Unit, UnitDef, Vec3};

use crate::Ai;
use crate::defs::cmd;
use crate::kgbutil::kgb_move_to;
use crate::military::{Fighter, SquadId};
use crate::util::{NULLPOS, OPTION_SHIFT_KEY, distance, get_directional_point};

/// How a spotter sees.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Spot {
    Tank,
    Cloak,
    Air,
}

const SPOTS: [Spot; 3] = [Spot::Tank, Spot::Cloak, Spot::Air];

impl Spot {
    pub fn name(self) -> &'static str {
        match self {
            Spot::Tank => "tank",
            Spot::Cloak => "cloak",
            Spot::Air => "air",
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

    /// Health share below which it goes for repairs.
    fn repair(self) -> f32 {
        match self {
            Spot::Tank | Spot::Air => 0.6,
            Spot::Cloak => 0.5,
        }
    }

    /// What it does with a dot, for the log.
    fn spotting(self) -> &'static str {
        match self {
            Spot::Tank => "scouting",
            Spot::Cloak => "loitering",
            Spot::Air => "overflight",
        }
    }
}

/// Dots this far beyond a gun's range are worth a look.
const LOOK: f32 = 250.0;

/// A tank stops this share of its sight short of the dot; a loiterer this
/// share, and its decloak distance plus `CLOAK_MARGIN` at least.
const TANK_SIGHT: f32 = 0.6;
const CLOAK_SIGHT: f32 = 0.8;
const CLOAK_MARGIN: f32 = 120.0;

/// Orders are given again when the point moves this far, or this often.
const RESEND_MOVED: f32 = 120.0;
const RESEND_FRAMES: i32 = 90;

/// A dot not identified after this long is given up (by that spotter).
const GIVE_UP_FRAMES: i32 = 40 * 30;

/// An escorting spotter this far from the leader is sent back to it.
const STRAYED: f32 = 600.0;

/// What a spotter is doing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Job {
    /// With the force, orbiting the leader.
    Escorting,
    /// Going to see a radar dot (an enemy id) for the guns.
    Spotting(i32),
    /// Gone for repairs (the retreat handler has it).
    Repairing,
}

#[derive(Clone, Debug)]
pub struct Spotter {
    pub sid: SquadId,
    pub spot: Spot,
    pub job: Job,
    since: i32,
    sent: Vec3,
    sent_at: i32,
    /// Escorting: told to orbit the leader since its last errand.
    orbiting: bool,
    gave_up: BTreeSet<i32>,
}

impl Spotter {
    /// The job's name in the log.
    pub fn doing(&self) -> &'static str {
        match self.job {
            Job::Escorting => "escorting",
            Job::Spotting(_) => self.spot.spotting(),
            Job::Repairing => "retreating",
        }
    }
}

/// Spotters by unit id, and those let go (never claimed again).
#[derive(Default)]
pub struct Spotters {
    pub by_unit: BTreeMap<i32, Spotter>,
    released: BTreeSet<i32>,
}

fn health_share(u: Unit) -> f32 {
    let max = u.max_health();
    if max > 0.0 { u.health() / max } else { 0.0 }
}

/// Where a spotter goes to see a dot at `dot` for a gun at `gun`.
fn spot_point(spot: Spot, def: Option<UnitDef>, dot: Vec3, gun: Vec3) -> Vec3 {
    let sight = def.map_or(400.0, |d| d.los_radius());
    let off = match spot {
        Spot::Air => return dot,
        Spot::Tank => TANK_SIGHT * sight,
        Spot::Cloak => (CLOAK_SIGHT * sight).max(def.map_or(0.0, |d| d.decloak_distance()) + CLOAK_MARGIN),
    };
    if distance(dot, gun) <= off {
        return gun;
    }
    let p = get_directional_point(dot, gun, off);
    Vec3::new(p.x, dot.y, p.z)
}

impl Ai {
    /// A unit just finished that some force short of a spotter takes:
    /// it joins (see the module notes). False: it goes its usual way.
    pub fn taskforce_spotter_claims(&mut self, unit: Unit, name: &str, cost: f32) -> bool {
        self.taskforce_load();
        if self.taskforces.spotters.released.contains(&unit.id()) {
            return false;
        }
        let tf = &self.taskforces;
        let wanted: Vec<(String, usize)> = (tf.doctrines.iter())
            .filter(|d| d.enabled && super::squad_aim(d.aim))
            .filter_map(|d| {
                let r = d.role_of(name, &tf.classes)?;
                d.roles[r].spot.is_some().then(|| (d.name.clone(), r))
            })
            .collect();
        let short = wanted.iter().any(|(d, r)| {
            (self.squad_handler.taskforces.iter())
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

    /// Steer the spotters of force `sid` (after its conduct).
    pub(crate) fn taskforce_spot(&mut self, sid: SquadId) {
        let frame = self.frame;
        let Some(s) = self.squads.get(sid) else { return };
        let shape = s.shape.clone();
        let members: Vec<(i32, Unit, Spot)> = (s.everyone().filter_map(|id| self.fighters.get(&id)))
            .filter_map(|f| shape.spot(f.unit.def().map(|d| d.id())).map(|sp| (f.id, f.unit, sp)))
            .collect();
        // Spotters that left the force (other than for repairs) are done.
        let here: BTreeSet<i32> = members.iter().map(|m| m.0).collect();
        (self.taskforces.spotters.by_unit)
            .retain(|id, sp| sp.sid != sid || sp.job == Job::Repairing || here.contains(id));
        if members.is_empty() {
            return;
        }
        let Some(leader) = s.get_leader(&self.fighters) else { return };
        let lpos = leader.pos();
        let state = force_state(s.engaging, s.engaged);
        let guns: Vec<(Vec3, f32)> = (s.everyone().filter_map(|id| self.fighters.get(&id)))
            .filter(|f| shape.is_core(f.unit.def().map(|d| d.id())))
            .map(|f| (f.unit.pos(), f.unit.weapon(0).map_or(f.unit.max_range(), |w| w.range())))
            .collect();
        if guns.is_empty() {
            return;
        }
        // How far beyond the guns' reach a point lies, and the nearest gun.
        let beyond = |p: Vec3| {
            (guns.iter())
                .map(|&(g, r)| (distance(g, p) - r, g))
                .min_by(|a, b| a.0.total_cmp(&b.0))
                .unwrap_or((f32::INFINITY, NULLPOS))
        };
        let dots: Vec<(i32, Vec3, f32, Vec3)> = (self.war.targets.values())
            .filter(|e| !e.identified && !e.visible && !e.is_dead && e.position != NULLPOS)
            .filter_map(|e| {
                let (b, g) = beyond(e.position);
                (b <= LOOK).then_some((e.unit_id, e.position, b, g))
            })
            .collect();
        let orbit = |unit: Unit| shape.orbit(unit.def().map(|d| d.id()));
        let mut lines = Vec::new();
        for (id, unit, spot) in members {
            let entry = self.taskforces.spotters.by_unit.get(&id);
            if entry.is_none_or(|sp| sp.sid != sid || sp.job == Job::Repairing) {
                lines.push(format!(
                    "taskforce: a {} spots for the {} force (squad {sid}, {state}), {:.0} from its leader (unit {id})",
                    self.defs.name_of(unit),
                    shape.doctrine,
                    distance(unit.pos(), lpos)
                ));
                let sp = Spotter {
                    sid,
                    spot,
                    job: Job::Escorting,
                    since: frame,
                    sent: NULLPOS,
                    sent_at: i32::MIN / 2,
                    orbiting: true,
                    gave_up: entry.map(|e| e.gave_up.clone()).unwrap_or_default(),
                };
                self.taskforces.spotters.by_unit.insert(id, sp);
            }
            if health_share(unit) < spot.repair() {
                lines.push(format!(
                    "taskforce: a {} spotter of the {} force (squad {sid}, {state}) goes for repairs at {:.0}% health, \
                     {:.0} from the force",
                    self.defs.name_of(unit),
                    shape.doctrine,
                    100.0 * health_share(unit),
                    distance(unit.pos(), lpos)
                ));
                if let Some(sp) = self.taskforces.spotters.by_unit.get_mut(&id) {
                    sp.job = Job::Repairing;
                    sp.since = frame;
                }
                self.retreat_send(unit);
                continue;
            }
            let taken: BTreeSet<i32> = (self.taskforces.spotters.by_unit.iter())
                .filter(|(u, _)| **u != id)
                .filter_map(|(_, sp)| if let Job::Spotting(e) = sp.job { Some(e) } else { None })
                .collect();
            let Some(sp) = self.taskforces.spotters.by_unit.get_mut(&id) else { continue };
            // The dot it looks at: done once identified, gone or out of
            // the guns' look, given up after a while.
            if let Job::Spotting(e) = sp.job {
                let target = self.war.targets.get(&e);
                let looking = dots.iter().any(|d| d.0 == e);
                if let Some(t) = target.filter(|t| (t.identified || t.visible) && !t.is_dead) {
                    lines.push(format!(
                        "taskforce: a {} spotting for the {} force (squad {sid}) identifies a {} ({:.0} metal), {:.0} \
                         from the guns",
                        self.defs.name_of(unit),
                        shape.doctrine,
                        t.def_name,
                        t.ud.map_or(t.value, |d| d.cost(self.metal)),
                        beyond(t.position).0.max(0.0)
                    ));
                }
                if !looking || frame - sp.since > GIVE_UP_FRAMES {
                    if looking {
                        sp.gave_up.insert(e);
                    }
                    sp.job = Job::Escorting;
                    sp.since = frame;
                    sp.sent = NULLPOS;
                }
            }
            if sp.job == Job::Escorting {
                let upos = unit.pos();
                let pick = (dots.iter())
                    .filter(|d| !taken.contains(&d.0) && !sp.gave_up.contains(&d.0))
                    .min_by(|a, b| (a.2 + 0.25 * distance(a.1, upos)).total_cmp(&(b.2 + 0.25 * distance(b.1, upos))));
                if let Some(&(e, ..)) = pick {
                    sp.job = Job::Spotting(e);
                    sp.since = frame;
                    sp.sent = NULLPOS;
                }
            }
            match sp.job {
                Job::Spotting(e) => {
                    let Some(&(_, dot, _, gun)) = dots.iter().find(|d| d.0 == e) else { continue };
                    let to = spot_point(spot, unit.def(), dot, gun);
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
        assert!(Spot::parse("boat").unwrap_err().contains("tank, cloak, air"));
    }

    #[test]
    fn spot_points_stand_off_toward_the_gun() {
        let (dot, gun) = (Vec3::xz(1000.0, 0.0), Vec3::xz(0.0, 0.0));
        let p = spot_point(Spot::Tank, None, dot, gun);
        assert!((p.x - 760.0).abs() < 1.0 && p.z.abs() < 1.0, "{p:?}");
        assert_eq!(spot_point(Spot::Air, None, dot, gun), dot);
        // Closer than the stand-off: stay with the gun.
        assert_eq!(spot_point(Spot::Tank, None, Vec3::xz(100.0, 0.0), gun), gun);
    }
}
