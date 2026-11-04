//! Rush (user, 2026-11-04): in a 3v3 or bigger on a small map (Alien
//! Desert, Fairyland), when an allied ZKGBAI opened with a factory that
//! builds a demi-strider (Cyclops, Crab, Grizzly), the alliance rolls a dice
//! weighted by how crowded the front is: its width per player a side. On
//! RUSH that factory builds the heavy next, every allied ZKGBAI's commander
//! guards it before doing anything else (the engine turns a guard on a
//! factory into assisting what it builds, and each helper's team pays for
//! its own build power), and the other ZKGBAIs' factories wait. When the
//! heavy walks out, everyone goes back to work, and the heavy fights as a
//! loner does.
//!
//! Every allied instance decides alike, with no message: the inputs are the
//! map, the start boxes, the teams and the alliance's opening factories, and
//! the dice is a hash of those factories' unit ids, which the engine draws
//! from the game's synced random seed. So the roll changes from game to game
//! and agrees across the alliance.
//!
//! `disable=rush` turns it off; the (undeclared) option `rush=force` takes
//! every eligible roll, for staged games.

use std::collections::BTreeMap;
use std::sync::mpsc;

use serde_json::json;
use springai::{Unit, UnitDef, Vec3};
use zk_terrain::lanes::LaneGrid;
use zk_terrain::{Pos, Tier, TierMap};

use crate::Ai;
use crate::terrain::pos;
use crate::util::distance;

/// The engine's CMD_GUARD.
const GUARD: i32 = 25;
/// Maps up to this diagonal (heightmap squares) are small: Alien Desert
/// (12x8) is 923, Fairyland (10x10) 905, a 12x10 map 1000; 12x12 is 1086.
const SMALL_DIAG: f32 = 1000.0;
/// Players a side, at least (3v3+).
const MIN_SIDE: usize = 3;
/// The odds of a rush fall linearly with the front's width per player a
/// side, from [`MAX_ODDS`] to none at this width, elmos. Measured fronts:
/// Fairyland 6332, Alien Desert 6748, so a 3v3 there rolls RUSH ~45% of
/// the time, a 4v4 on Fairyland 60%.
const FRONT_ZERO: f32 = 4000.0;
const MAX_ODDS: f32 = 0.75;
/// Depths (shares of the way from our start box to theirs, by ground) whose
/// passable widths average into the front, and the half-width of each band.
const DEPTHS: [f32; 3] = [0.4, 0.5, 0.6];
const BAND: f32 = 0.05;
/// Decide with what is known by this frame, whatever is missing.
const DECIDE_BY: i32 = 30 * 45;
/// A rush whose heavy is not out this long after the roll is given up.
const GIVE_UP: i32 = 30 * 60 * 4;
/// Frames between checks (alike in every instance: no offset).
const PERIOD: i32 = 15;
/// The heavy's nanoframe stands this near its factory, elmos.
const NEAR_FACTORY: f32 = 300.0;
/// An order reaches the factory's queue through the server, frames later:
/// the heavy is ordered again only if it does not stand this long after.
const REORDER: i32 = 150;

/// The factories with a demi-strider: (factory, heavy, its name).
pub(crate) const HEAVIES: [(&str, &str, &str); 3] = [
    ("factorytank", "tankheavyassault", "Cyclops"),
    ("factoryspider", "spidercrabe", "Crab"),
    ("factoryamph", "amphassault", "Grizzly"),
];

/// The rush's state (in the economy).
#[derive(Default)]
pub struct Rush {
    job: Option<mpsc::Receiver<Option<f32>>>,
    started: bool,
    /// The front's width, elmos, once measured (`Some(None)`: it could not
    /// be).
    front: Option<Option<f32>>,
    pub state: State,
}

#[derive(Default)]
pub enum State {
    #[default]
    Undecided,
    /// No rush this game.
    Off,
    On(Plan),
    /// The rush is over.
    Over,
}

pub struct Plan {
    /// The team whose factory builds the heavy.
    pub rusher: i32,
    pub factory: Unit,
    pub heavy: UnitDef,
    /// When it was rolled.
    pub since: i32,
    /// The heavy's nanoframe, once it stands.
    pub nano: Option<Unit>,
    /// When our factory was last ordered to build it.
    pub ordered: Option<i32>,
}

/// What a factory of ours does in a rush.
pub(crate) enum RushOrder {
    /// Build the heavy.
    Heavy(UnitDef),
    /// Nothing: the metal goes to the heavy.
    Hold,
}

/// The odds of a rush with a front `front` elmos wide and `side` players a
/// side.
pub(crate) fn odds(front: f32, side: f32) -> f32 {
    if front <= 0.0 || side <= 0.0 {
        return 0.0;
    }
    (1.0 - front / side / FRONT_ZERO).clamp(0.0, MAX_ODDS)
}

fn splitmix(mut z: u64) -> u64 {
    z = z.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// A throw in [0, 1) that every instance makes alike from the same unit
/// ids (in any order).
pub(crate) fn dice(ids: &[i32]) -> f64 {
    let mut ids = ids.to_vec();
    ids.sort_unstable();
    let h = ids.iter().fold(0x5EED_u64, |h, &id| splitmix(h ^ u64::from(id as u32)));
    (h >> 11) as f64 / (1u64 << 53) as f64
}

/// The front: the mean passable width of `map` at [`DEPTHS`] between the
/// two starts (see `zk_terrain::lanes`, the closure profile's `available`).
pub(crate) fn front_width(map: &TierMap, ours: Pos, theirs: Pos) -> Option<f32> {
    let g = LaneGrid::new(map, 2);
    let (oc, ec) = (g.cell_near(ours)?, g.cell_near(theirs)?);
    let (d_o, d_e) = (g.distances(oc), g.distances(ec));
    let widths: Vec<f32> =
        g.profile(ec, oc, &d_o, &d_e, &DEPTHS, BAND).iter().map(|c| c.available).filter(|&w| w > 0.0).collect();
    (!widths.is_empty()).then(|| widths.iter().sum::<f32>() / widths.len() as f32)
}

/// Of the `candidates` (factory positions), the index of the one nearest
/// all the `factories` (by summed distance; the first on ties): the
/// helpers walk least.
pub(crate) fn pick_rusher(candidates: &[Vec3], factories: &[Vec3]) -> Option<usize> {
    let walk = |c: Vec3| factories.iter().map(|&f| distance(c, f)).sum::<f32>();
    (0..candidates.len()).min_by(|&a, &b| walk(candidates[a]).total_cmp(&walk(candidates[b])).then(a.cmp(&b)))
}

/// Whether `unit` has a guard on `target` in its queue (the engine pushes
/// the assist in front of the guard).
fn guards(unit: Unit, target: Unit) -> bool {
    unit.current_commands()
        .iter()
        .any(|c| c.id() == GUARD && c.params().first().is_some_and(|&p| p as i32 == target.id()))
}

impl Ai {
    /// Decide the rush, then drive it.
    pub(crate) fn rush_update(&mut self, frame: i32) {
        if frame % PERIOD != 0 {
            return;
        }
        self.rush_measure();
        match self.eco.rush.state {
            State::Undecided => self.rush_decide(frame),
            State::On(_) => self.rush_drive(frame),
            State::Off | State::Over => {}
        }
    }

    /// Measure the front once, on a thread, when the terrain model is in.
    /// From the start boxes, not our own start: alike for every ally.
    fn rush_measure(&mut self) {
        let r = &mut self.eco.rush;
        if let Some(rx) = &r.job
            && let Ok(front) = rx.try_recv()
        {
            r.job = None;
            r.front = Some(front);
        }
        if r.started || !matches!(r.state, State::Undecided) {
            return;
        }
        let (ally, enemy) = (self.graph.get_ally_base(), self.graph.get_enemy_base());
        let Some(enemy) = enemy else { return };
        let Some(map) = self.terrain().and_then(|t| t.tier(Tier::Bot)).cloned() else { return };
        let (ours, theirs) = (pos(ally), pos(enemy));
        let (tx, rx) = mpsc::channel();
        // Pure computation: the thread must never call into the engine.
        let spawned = std::thread::Builder::new().name("zkgbai-rush".into()).spawn(move || {
            let _ = tx.send(front_width(&map, ours, theirs));
        });
        let r = &mut self.eco.rush;
        r.started = true;
        match spawned {
            Ok(_) => r.job = Some(rx),
            Err(e) => {
                r.front = Some(None);
                self.log(&format!("rush: cannot measure the front ({e})"));
            }
        }
    }

    fn rush_off(&mut self, why: &str) {
        self.log(&format!("rush: no rush: {why}"));
        self.op_emit("rush", json!({"rush": false, "why": why}));
        self.eco.rush.state = State::Off;
    }

    /// The alliance's ZKGBAIs (us included) and their living factories.
    fn rush_factories(&self) -> BTreeMap<i32, Vec<Unit>> {
        let mut teams: BTreeMap<i32, Vec<Unit>> =
            self.iq.peers.iter().chain([&self.team_id]).map(|&t| (t, Vec::new())).collect();
        let mut units = self.cb.friendly_units();
        units.extend(self.cb.team_units());
        units.sort_by_key(|u| u.id());
        units.dedup_by_key(|u| u.id());
        for u in units {
            if u.health() > 0.0
                && self.defs.name_of(u).starts_with("factory")
                && let Some(list) = teams.get_mut(&u.team())
            {
                list.push(u);
            }
        }
        teams
    }

    fn rush_decide(&mut self, frame: i32) {
        let force = self.cb.option("rush").is_some_and(|v| v.trim().eq_ignore_ascii_case("force"));
        if self.dev_off("rush") {
            return self.rush_off("switched off");
        }
        let (side, enemies) = (self.allies.len() + 1, self.enemy_teams.len());
        if self.is_ffa || side.min(enemies) < MIN_SIDE {
            return self.rush_off(&format!("{side} v {enemies}, not 3v3 or bigger"));
        }
        if self.map_diag > SMALL_DIAG {
            return self.rush_off(&format!("the map's diagonal is {:.0}, over {SMALL_DIAG:.0}", self.map_diag));
        }
        if self.iq.peers.is_empty() {
            return self.rush_off("no other ZKGBAI on our side to assist");
        }
        // Wait for the front and every ZKGBAI's opening factory.
        let factories = self.rush_factories();
        let ready = factories.values().all(|f| !f.is_empty()) && self.eco.rush.front.is_some();
        if !ready && frame < DECIDE_BY {
            return;
        }
        let Some(front) = self.eco.rush.front.flatten() else {
            return self.rush_off("the front could not be measured");
        };
        let mut candidates: Vec<(i32, Unit, UnitDef, &str)> = Vec::new();
        for (&team, list) in &factories {
            for &f in list {
                let name = self.defs.name_of(f);
                let Some(&(_, heavy, called)) = HEAVIES.iter().find(|h| h.0 == name) else { continue };
                if let Some(def) = self.defs.get(heavy) {
                    candidates.push((team, f, def, called));
                }
            }
        }
        let per_side = (side + enemies) as f32 / 2.0;
        let natural = odds(front, per_side);
        let crowd = format!(
            "front {front:.0} elmos for {per_side:.1} players a side ({:.0} each), odds {natural:.2}",
            front / per_side
        );
        if candidates.is_empty() {
            let opened: Vec<String> = (factories.iter())
                .map(|(t, l)| format!("{t}: {}", l.iter().map(|&u| self.defs.name_of(u)).collect::<Vec<_>>().join("+")))
                .collect();
            return self
                .rush_off(&format!("no demi-strider factory among our openings ({}); {crowd}", opened.join(", ")));
        }
        let p = if force { 1.0 } else { natural };
        let ids: Vec<i32> = factories.values().flatten().map(|u| u.id()).collect();
        let throw = dice(&ids);
        let rolled = format!("{crowd}{}: rolled {throw:.3}", if force { " (forced)" } else { "" });
        if throw >= f64::from(p) {
            return self.rush_off(&rolled);
        }
        let all: Vec<Vec3> = factories.values().flatten().map(|u| u.pos()).collect();
        let at: Vec<Vec3> = candidates.iter().map(|c| c.1.pos()).collect();
        let Some(k) = pick_rusher(&at, &all) else { return };
        let (rusher, factory, heavy, called) = candidates[k];
        self.log(&format!(
            "rush: RUSH! {rolled}: team {rusher}'s {} builds a {called}; every ZKGBAI's commander assists",
            self.defs.name_of(factory)
        ));
        self.op_emit(
            "rush",
            json!({"rush": true, "front": front, "side": per_side, "odds": p, "natural": natural, "dice": throw,
                   "rusher": rusher, "factory": factory.id(), "heavy": self.defs.name(heavy)}),
        );
        if rusher == self.team_id {
            let tag = self.banter.as_ref().map_or("ZKGBAI", |b| b.tag());
            let line = format!("/say a:<{tag}> Rush! A {called} from my factory: help build it");
            self.cb.game().send_text_message(&line, 0);
        }
        self.eco.rush.state = State::On(Plan { rusher, factory, heavy, since: frame, nano: None, ordered: None });
    }

    fn rush_drive(&mut self, frame: i32) {
        let State::On(plan) = &self.eco.rush.state else { return };
        let (rusher, factory, heavy, since) = (plan.rusher, plan.factory, plan.heavy, plan.since);
        let nano = plan.nano.or_else(|| {
            self.cb.friendly_units_in(factory.pos(), NEAR_FACTORY).into_iter().find(|u| {
                u.team() == rusher && u.is_being_built() && u.health() > 0.0 && u.def().is_some_and(|d| d == heavy)
            })
        });
        let end = if factory.health() <= 0.0 && nano.is_none() {
            Some("its factory is gone")
        } else if nano.is_some_and(|n| n.health() <= 0.0 || n.team() != rusher) {
            Some("the heavy died unfinished")
        } else if nano.is_some_and(|n| !n.is_being_built()) {
            Some("the heavy is out")
        } else if frame - since > GIVE_UP {
            Some("the heavy is not out in time")
        } else {
            None
        };
        if let Some(why) = end {
            return self.rush_end(why);
        }
        if let State::On(plan) = &mut self.eco.rush.state
            && plan.nano.is_none()
            && let Some(n) = nano
        {
            plan.nano = Some(n);
            self.log(&format!("rush: the {} stands (unit {})", self.defs.name(heavy), n.id()));
        }
        // Our factory builds the heavy as soon as it is free.
        if rusher == self.team_id
            && nano.is_none()
            && self.rush_may_order()
            && !factory.is_being_built()
            && factory.num_current_commands() == 0
            && self.fac.factories.contains_key(&factory.id())
        {
            self.fac_assign_factory_task(factory.id());
        }
        // Our commanders guard the factory (and so assist the heavy).
        for wid in self.eco.commanders.clone() {
            let Some(w) = self.eco.workers.get_mut(&wid) else { continue };
            if w.is_chicken || w.raid_defense.is_some() || w.has_plop() {
                continue;
            }
            if w.task.is_some() {
                w.clear_task(&mut self.eco.tasks);
            }
            let unit = w.unit;
            if !guards(unit, factory) {
                unit.guard(factory, 0);
            }
        }
    }

    fn rush_end(&mut self, why: &str) {
        let State::On(plan) = std::mem::replace(&mut self.eco.rush.state, State::Over) else { return };
        let minutes = (self.frame - plan.since) as f32 / 1800.0;
        self.log(&format!("rush: over after {minutes:.1} min: {why}"));
        self.op_emit("rush", json!({"over": why, "minutes": minutes}));
        // The commanders stop guarding (the factory builds on), and go back
        // to work.
        for wid in self.eco.commanders.clone() {
            let Some(w) = self.eco.workers.get(&wid) else { continue };
            if guards(w.unit, plan.factory) {
                w.unit.stop(0);
            }
            super::erase_one_dq(&mut self.eco.assigned, wid);
            if !self.eco.idlers.contains(&wid) {
                self.eco.idlers.push_back(wid);
            }
        }
        // The factories held go back to work.
        let idle: Vec<i32> = (self.fac.factories.iter())
            .filter(|(_, f)| !f.unit.is_being_built() && f.unit.num_current_commands() == 0)
            .map(|(&id, _)| id)
            .collect();
        for id in idle {
            self.fac_assign_factory_task(id);
        }
    }

    /// Whether worker `wid` is held for the rush: a commander of ours, its
    /// plop spent, while a rush is on.
    pub(crate) fn rush_holds(&self, wid: i32) -> bool {
        matches!(self.eco.rush.state, State::On(_))
            && self.eco.commanders.contains(&wid)
            && self.eco.workers.get(&wid).is_some_and(|w| !w.has_plop())
    }

    /// Whether the heavy may be ordered (again): not ordered lately.
    fn rush_may_order(&self) -> bool {
        let State::On(plan) = &self.eco.rush.state else { return false };
        plan.ordered.is_none_or(|f| self.frame - f >= REORDER)
    }

    /// What our factory `fac_id` builds while a rush is on: the rusher's
    /// factory the heavy until it stands (once per [`REORDER`]: the order
    /// is on its way meanwhile); every other factory nothing.
    pub(crate) fn rush_factory(&self, fac_id: i32) -> Option<RushOrder> {
        let State::On(plan) = &self.eco.rush.state else { return None };
        if plan.rusher == self.team_id && plan.factory.id() == fac_id {
            if plan.nano.is_some() {
                return None;
            }
            return Some(if self.rush_may_order() { RushOrder::Heavy(plan.heavy) } else { RushOrder::Hold });
        }
        Some(RushOrder::Hold)
    }

    /// Our factory was ordered to build the heavy.
    pub(crate) fn rush_ordered(&mut self) {
        let frame = self.frame;
        if let State::On(plan) = &mut self.eco.rush.state {
            plan.ordered = Some(frame);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn odds_rise_as_the_front_crowds() {
        // Fairyland: a 6332-elmo front.
        let three = odds(6332.0, 3.0);
        let four = odds(6332.0, 4.0);
        assert!((0.4..0.5).contains(&three) && four > three, "{three} {four}");
        assert_eq!(odds(6332.0, 1.0), 0.0);
        assert_eq!(odds(100.0, 5.0), MAX_ODDS);
        assert_eq!(odds(0.0, 3.0), 0.0);
        assert_eq!(odds(4000.0, 0.0), 0.0);
    }

    #[test]
    fn the_dice_agrees_whatever_the_order_and_moves_with_the_ids() {
        let a = dice(&[17, 4021, 9]);
        assert_eq!(a, dice(&[9, 17, 4021]));
        assert!((0.0..1.0).contains(&a));
        assert_ne!(a, dice(&[9, 17, 4022]));
        // Spread: of 1000 id sets, about half under one half.
        let under = (0..1000).filter(|&i| dice(&[i, i * 7 + 3, i * 13 + 5]) < 0.5).count();
        assert!((400..600).contains(&under), "{under}");
    }

    #[test]
    fn the_rusher_is_the_heavy_factory_the_helpers_walk_least_to() {
        let all = [Vec3::xz(0.0, 0.0), Vec3::xz(1000.0, 0.0), Vec3::xz(2000.0, 0.0)];
        // Both ends against the middle: the middle.
        assert_eq!(pick_rusher(&[all[0], all[1]], &all), Some(1));
        // Two alike: the first.
        assert_eq!(pick_rusher(&[all[0], all[2]], &all), Some(0));
        assert_eq!(pick_rusher(&[], &all), None);
    }

    #[test]
    fn every_heavy_factory_has_its_heavy() {
        let factories: Vec<&str> = HEAVIES.iter().map(|h| h.0).collect();
        assert_eq!(factories, ["factorytank", "factoryspider", "factoryamph"]);
    }
}
