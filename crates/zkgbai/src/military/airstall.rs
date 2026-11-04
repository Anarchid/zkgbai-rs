//! `airstall` (on; `disable=airstall`): air production stops while air
//! does not pay, and an AI with nothing but air factories moves its metal
//! to the ground: a plate on an ally's ground factory, else a superweapon.
//!
//! The ledger: over the last [`WINDOW`], the metal our armed aircraft
//! killed (commanders aside) against the metal of our armed aircraft lost
//! (constructors, transports, scouts and commander drones aside). Air
//! stalls once at least [`EVIDENCE`] metal of it was lost and it killed
//! under [`STALL`] times that. It resumes once it killed [`RESUME`] times
//! what it lost, over at least [`EVIDENCE`] of both (the fleet still flying
//! fights on), or after [`PROBE`] stalled with kills at least even with the
//! losses in the window (none of either, say), to try again. The `airstall_evidence` option moves
//! [`EVIDENCE`] (for staged games). Read on three 3v3 games on Adamantine Mountain 2 (2026-11-04): a
//! gunship AI whose last 5 minutes traded 805 for 1800 at minute 9 and
//! 8535 for 600 at 16 stalls at 8 and resumes at 13; the lobby's, which
//! traded well until a Krow and more went for 870 at 25, stalls at 25 and
//! resumes at 30.
//!
//! Stalled, the gunship and plane factories build constructors, the
//! firefighters' orders, Owls and anti-air gunships only (more constructors
//! while metal piles up with nothing but air, see [`Ai::airstall_wants_con`]),
//! and an idle plant
//! stays idle (no garrison guard or taskforce unit in the slot: they would
//! be aircraft too); idle air plants are asked again every 2 s (plane and
//! gunship plants alike, `fac_update`), so production comes back when the
//! stall ends. With no ground
//! factory or plate of our own, the requisition asks for the type of the
//! nearest allied ground factory we would plate (`plate`), and only a
//! plate will do; with none to plate, the superweapon gate (more than four
//! reactors) is waived, so the metal goes to a superweapon, an ally's
//! first (`allied`).

use std::collections::VecDeque;

use serde_json::json;
use springai::{Unit, UnitDef};

use crate::Ai;

/// The switch.
pub(crate) const SWITCH: &str = "airstall";
/// The ledger's window, frames.
const WINDOW: i32 = 30 * 300;
/// Metal lost (to stall) or traded (to resume) over the window before the
/// trade counts.
const EVIDENCE: f32 = 1500.0;
/// Stall when kills fall under this share of losses.
const STALL: f32 = 0.6;
/// Resume when kills reach this share of losses.
const RESUME: f32 = 1.2;
/// Stalled this long with too few losses to judge, air is tried again.
const PROBE: i32 = 30 * 300;
/// Aircraft that scout rather than fight.
const SCOUTS: [&str; 2] = ["planescout", "planelightscout"];
/// Constructors a stalled air-only AI keeps (commander included).
const STALL_CONS: i32 = 4;
/// The factory types that make aircraft.
pub(crate) const AIR_FACTORIES: [&str; 2] = ["factorygunship", "factoryplane"];

/// What our armed aircraft killed and cost us lately, and whether air
/// production stands stalled.
#[derive(Debug, Default)]
pub struct AirLedger {
    kills: VecDeque<(i32, f32)>,
    losses: VecDeque<(i32, f32)>,
    /// Since when air production stands stalled.
    pub stalled_since: Option<i32>,
    /// Stalls so far.
    pub stalls: u32,
    /// When the economy last said why no plate went up (see `plate`).
    pub plate_logged: i32,
    /// [`EVIDENCE`], or the `airstall_evidence` option.
    evidence: Option<f32>,
}

impl AirLedger {
    /// Metal killed and lost within the window.
    pub fn trade(&self) -> (f32, f32) {
        // (Folded from +0: an empty f32 sum is -0.)
        let sum = |q: &VecDeque<(i32, f32)>| q.iter().fold(0.0, |a, x| a + x.1);
        (sum(&self.kills), sum(&self.losses))
    }

    fn prune(&mut self, frame: i32) {
        for q in [&mut self.kills, &mut self.losses] {
            while q.front().is_some_and(|&(f, _)| frame - f > WINDOW) {
                q.pop_front();
            }
        }
    }
}

/// Whether air stands stalled at `frame`, given since when it stood
/// stalled, the window's trade and the evidence wanted.
fn stalled(since: Option<i32>, frame: i32, killed: f32, lost: f32, evidence: f32) -> bool {
    match since {
        None => lost >= evidence && killed < STALL * lost,
        Some(s) => {
            let resume = killed + lost >= evidence && killed >= RESUME * lost;
            // (Not on a bad trade however small, 0 killed for 1200 lost
            // say, nor on one just over the stall line: 960 for 1500
            // stalled again 17 s after it resumed, 2026-11-04.)
            let probe = frame - s >= PROBE && killed >= lost;
            !(resume || probe)
        }
    }
}

impl Ai {
    /// Whether air production stands stalled (`airstall`).
    pub(crate) fn air_stalled(&self) -> bool {
        !self.dev_off(SWITCH) && self.war.air.stalled_since.is_some()
    }

    /// Whether every factory and plate of ours, standing or planned, makes
    /// aircraft (one at least): with air stalled, nothing of ours spends on
    /// an army. (A plate still rising counts: a second one was asked for in
    /// the frame the first finished, 2026-11-04.)
    pub(crate) fn air_only(&self) -> bool {
        let air = |tech: &str| AIR_FACTORIES.contains(&tech);
        let planned = (self.eco.factory_tasks.iter())
            .filter_map(|&id| self.eco.tasks.get(id).and_then(|t| t.as_construction()))
            .all(|c| air(&self.fac_tech(c.build_type)));
        self.air_only_standing() && planned
    }

    /// Whether every factory and plate of ours standing makes aircraft (one
    /// at least), whatever is planned: the plate asked for still needs
    /// builders (none came while it waited, staged 2026-11-04).
    fn air_only_standing(&self) -> bool {
        let air = |tech: &str| AIR_FACTORIES.contains(&tech);
        !self.fac.factories.is_empty()
            && self.fac.factories.values().all(|f| air(&f.tech))
            && self.eco.plates.iter().all(|&p| p.def().is_some_and(|d| air(&self.fac_tech(d))))
    }

    /// Nothing but air factories, air stalled and metal piling up (over
    /// half our storage): the plant builds constructors (up to
    /// [`STALL_CONS`] workers), which raise the plate or the superweapon and
    /// spend the metal. Staged 2026-11-04: with the commander the only
    /// worker, the plate waited 5 minutes for it (a remex first) while 500
    /// to 1000 metal sat in storage.
    pub(crate) fn airstall_wants_con(&self) -> bool {
        self.air_stalled()
            && self.air_only_standing()
            && self.eco.metal > 0.5 * self.eco.max_storage.max(1.0)
            && self.fac.num_workers < STALL_CONS
    }

    /// As air stalls, the combat aircraft the air plants queued but have
    /// not begun go (a factory may queue ahead: two Harpies stood queued at
    /// one stall, and a Krow queued so held a plant for 7 minutes of
    /// another, 2026-11-04). A build order with the right mouse key takes
    /// one of its type off the back of the queue, so the first command, the
    /// one being built, stays.
    fn airstall_dequeue(&mut self) {
        let mut removed: Vec<String> = Vec::new();
        let plants: Vec<Unit> = (self.fac.factories.values())
            .filter(|f| AIR_FACTORIES.contains(&f.tech.as_str()))
            .map(|f| f.unit)
            .collect();
        for plant in plants {
            let queued: Vec<UnitDef> = (plant.current_commands().iter().skip(1))
                .map(|c| c.id())
                .filter(|&id| id < 0)
                .filter_map(|id| self.defs.name_by_id(-id).and_then(|n| self.defs.get(n)))
                .filter(|&d| self.airstall_counts(d))
                .collect();
            for d in queued {
                plant.build(d, plant.pos(), 0, springai::opt::RIGHT_MOUSE_KEY);
                removed.push(self.defs.name(d).to_owned());
            }
        }
        if !removed.is_empty() {
            self.log(&format!("airstall: taken off the air plants' queues: {}", removed.join(", ")));
        }
    }

    /// Whether `name` is an aircraft a stalled plant should not be building
    /// (for the leak log in `fac_assign_factory_task`).
    pub(crate) fn airstall_leaks(&self, name: &str) -> bool {
        self.air_stalled() && self.defs.get(name).is_some_and(|d| self.airstall_counts(d))
    }

    /// An aircraft that fights: armed, not a constructor, a scout or a
    /// commander's drone.
    fn airstall_counts(&self, d: UnitDef) -> bool {
        let name = self.defs.name(d);
        d.is_able_to_fly()
            && d.is_able_to_attack()
            && !d.is_builder()
            && !name.starts_with("drone")
            && !SCOUTS.contains(&name)
    }

    /// An enemy worth `cost` died to `attacker`.
    pub(crate) fn airstall_enemy_destroyed(&mut self, cost: Option<f32>, attacker: Option<Unit>) {
        let Some(a) = attacker.filter(|a| a.team() == self.team_id) else { return };
        let (Some(d), Some(c)) = (a.def(), cost.filter(|&c| c > 0.0)) else { return };
        if self.airstall_counts(d) {
            self.war.air.kills.push_back((self.frame, c));
        }
    }

    /// A unit of ours died.
    pub(crate) fn airstall_unit_destroyed(&mut self, unit: Unit) {
        let Some(d) = unit.def().filter(|_| !unit.is_being_built()) else { return };
        if self.airstall_counts(d) {
            self.war.air.losses.push_back((self.frame, d.cost(self.metal)));
        }
    }

    /// Judge the window once a second; say when air stalls or resumes.
    pub(crate) fn airstall_update(&mut self, frame: i32) {
        if frame % 30 != (self.offset + 19) % 30 || self.dev_off(SWITCH) {
            return;
        }
        if self.war.air.evidence.is_none() {
            let opt = self.cb.option("airstall_evidence").and_then(|v| v.trim().parse::<f32>().ok());
            self.war.air.evidence = Some(opt.unwrap_or(EVIDENCE).max(1.0));
        }
        let air = &mut self.war.air;
        air.prune(frame);
        let (killed, lost) = air.trade();
        let was = air.stalled_since;
        let now = stalled(was, frame, killed, lost, air.evidence.unwrap_or(EVIDENCE));
        if now == was.is_some() {
            return;
        }
        let event = if now {
            air.stalled_since = Some(frame);
            air.stalls += 1;
            "stall"
        } else {
            air.stalled_since = None;
            "resume"
        };
        let only = self.air_only();
        if now {
            self.airstall_dequeue();
        }
        // What the air plants have queued as air stalls, after that (the
        // build begun is not cancelled).
        let queues: Vec<String> = if now {
            (self.fac.factories.values())
                .filter(|f| AIR_FACTORIES.contains(&f.tech.as_str()))
                .map(|f| {
                    let q: Vec<&str> = (f.unit.current_commands().iter())
                        .map(|c| c.id())
                        .filter(|&id| id < 0)
                        .map(|id| self.defs.name_by_id(-id).unwrap_or("?"))
                        .collect();
                    format!("{} [{}]", f.tech, q.join(", "))
                })
                .collect()
        } else {
            Vec::new()
        };
        if !queues.is_empty() {
            self.log(&format!("airstall: air plants' queues: {}", queues.join("; ")));
        }
        self.log(&format!(
            "airstall: air {} ({killed:.0} metal killed by our aircraft for {lost:.0} lost in 5 minutes){}",
            if now { "stops: it does not pay" } else { "resumes" },
            if now && only {
                "; nothing but air factories: a plate on an ally's ground factory, else a superweapon"
            } else {
                ""
            }
        ));
        self.op_emit(
            "airstall",
            json!({"event": event, "t": frame / 30, "killed": killed.round(), "lost": lost.round(), "air_only": only}),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stalls_on_enough_bad_trades_only() {
        let e = EVIDENCE;
        // Three Harpies lost for nothing is not yet evidence.
        assert!(!stalled(None, 0, 0.0, 900.0, e));
        assert!(stalled(None, 0, 805.0, 1800.0, e));
        assert!(!stalled(None, 0, 1200.0, 1800.0, e));
        // Unless a staged game asks for less.
        assert!(stalled(None, 0, 0.0, 900.0, 600.0));
    }

    #[test]
    fn resumes_on_good_trades_or_probes() {
        let (s, e) = (Some(0), EVIDENCE);
        // Still bad, or good on too little: stays stalled.
        assert!(stalled(s, 30 * 60, 1015.0, 1800.0, e));
        assert!(stalled(s, 30 * 60, 600.0, 0.0, e));
        // The surviving fleet trades well again.
        assert!(!stalled(s, 30 * 60, 1655.0, 760.0, e));
        // Nothing to judge for a whole window: try again.
        assert!(stalled(s, PROBE - 1, 0.0, 0.0, e));
        assert!(!stalled(s, PROBE, 0.0, 0.0, e));
        // Not while the losses go on, small or not, nor just over the line.
        assert!(stalled(s, PROBE, 300.0, 1600.0, e));
        assert!(stalled(s, PROBE, 0.0, 1200.0, e));
        assert!(stalled(s, PROBE, 960.0, 1500.0, e));
        assert!(!stalled(s, PROBE, 600.0, 500.0, e));
    }

    #[test]
    fn the_window_forgets() {
        let mut l = AirLedger::default();
        l.kills.push_back((0, 500.0));
        l.losses.push_back((50, 300.0));
        l.losses.push_back((WINDOW + 50, 760.0));
        l.prune(WINDOW + 60);
        assert_eq!(l.trade(), (0.0, 760.0));
    }
}
