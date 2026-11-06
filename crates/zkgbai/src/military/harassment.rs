//! Short attack-and-withdraw commitments. `harasskodachi` is on by default;
//! `harasswide` opts other named ground units into the same experimental judge.
//! New commitments require a safe return under both UnitAI and committed enemy
//! pursuit. Disabling either flag stops admissions, preserving existing exits.
//! All scheduling and commitments are frame-based and checkpointed. The lease
//! token refers to the per-AI command authority snapshot, not a saved callback.
use std::collections::BTreeMap;

use serde_json::json;
use springai::{Unit, Vec3, reserved};

use crate::Ai;
use crate::military::fightsim::{ExitTrigger, FightSnapshot, HarassOrder, HarassOutcome, Model};
use crate::military::roles::Role;
use crate::util::{distance, get_directional_point};

const HORIZON: i32 = 600;
const RECHECK: i32 = 120;
const MAX_ACTIVE: usize = 6;
const MAX_ENEMIES: usize = 6;
const OWNER: reserved::Owner = reserved::Owner { module: "harassment", helpers: &[] };

fn enabled(name: &str, kodachi: bool, wide: bool) -> bool {
    if name == "tankraid" {
        kodachi
    } else {
        wide && matches!(
            name,
            "tankheavyraid"
                | "hoverheavyraid"
                | "tankheavyassault"
                | "tankassault"
                | "vehassault"
                | "vehcapture"
                | "vehraid"
                | "jumpraid"
        )
    }
}

#[derive(Clone, Debug, PartialEq)]
#[checkpoint::state]
struct Sortie {
    target: i32,
    exit: Vec3,
    started: i32,
    attack_until: i32,
    until: i32,
    withdrawing: bool,
    exit_ordered: bool,
    first_shot: Option<i32>,
    /// Read before issuing Attack/Fight, so an already-reloading gun is not a shot.
    reloads: Vec<(i32, i32)>,
    last_order: i32,
    lease: u64,
    start_hp: f32,
    fight: bool,
}
impl Sortie {
    fn observe_shot(&mut self, frame: i32) {
        if !self.withdrawing {
            self.first_shot.get_or_insert(frame);
        }
    }
    fn should_exit(&self, frame: i32, hp: f32, target_live: bool) -> bool {
        self.first_shot.is_some() || frame >= self.attack_until || !target_live || hp < self.start_hp * 0.8
    }
}

#[derive(Default)]
#[checkpoint::state]
pub struct Harassment {
    plans: BTreeMap<i32, Sortie>,
    next_check: BTreeMap<i32, i32>,
    cursor: i32,
}

#[derive(Clone, Copy, Debug)]
struct Choice {
    fight: bool,
    shot: i32,
    advantage: f32,
}

fn safe(out: &HarassOutcome) -> bool {
    out.units.first().is_some_and(|u| u.hp > 0.0 && u.p_dead.is_finite() && u.p_dead <= 0.05)
        && out.reached
        && out.clearance.is_none_or(|c| c > 20.0)
}

/// Partial surviving damage is a small, explicit credit, never a kill. This
/// coefficient did not change the selections in the original synthetic cohort.
fn score(snap: &FightSnapshot, out: &HarassOutcome) -> f32 {
    snap.units
        .iter()
        .zip(&out.units)
        .map(|(u, fate)| {
            let dead = fate.p_dead.clamp(0.0, 1.0);
            let chip = ((u.hp - fate.hp).max(0.0) / u.spec.health.max(1.0)).clamp(0.0, 1.0);
            let value = u.spec.cost * (dead + 0.1 * (1.0 - dead) * chip);
            if u.side == 0 { -value } else { value }
        })
        .sum()
}

fn choose(model: &Model, snap: &FightSnapshot, target: usize, exit: Vec3) -> Option<Choice> {
    let order = |fight, enemy_kiting, trigger| HarassOrder {
        target: if fight { None } else { Some(target) },
        trigger,
        exit: (exit.x, exit.z),
        own_kiting: true,
        enemy_kiting,
    };
    let baseline = [true, false]
        .map(|enemy| score(snap, &model.harassment(snap, HORIZON, order(true, enemy, ExitTrigger::AtFrame(0)))));
    let mut best: Option<Choice> = None;
    for fight in [false, true] {
        let mut advantage = f32::INFINITY;
        let mut shot = 0;
        let mut accepted = true;
        for (i, enemy) in [true, false].into_iter().enumerate() {
            let out = model.harassment(snap, HORIZON, order(fight, enemy, ExitTrigger::AfterFirstShot(0)));
            let Some(first) = out.first_shot else {
                accepted = false;
                break;
            };
            if !safe(&out) {
                accepted = false;
                break;
            }
            shot = shot.max(first);
            advantage = advantage.min(score(snap, &out) - baseline[i]);
        }
        if accepted && advantage.is_finite() && advantage > 0.01 && best.is_none_or(|b| advantage > b.advantage) {
            best = Some(Choice { fight, shot, advantage });
        }
    }
    best
}

impl Ai {
    pub(crate) fn harassment_owns(&self, id: i32) -> bool {
        self.war.harassment.plans.get(&id).is_some_and(|p| reserved::lease_active(self.cb, id, p.lease))
    }

    pub(crate) fn harassment_fired(&mut self, unit: Unit) {
        let id = unit.id();
        if !self.harassment_owns(id) {
            return;
        }
        if let Some(p) = self.war.harassment.plans.get_mut(&id) {
            p.observe_shot(self.frame);
        }
        // WeaponFired can arrive between planner ticks: turn immediately.
        self.harassment_drive(id, self.frame);
    }

    fn harassment_release(&mut self, id: i32, why: &str) {
        let Some(p) = self.war.harassment.plans.remove(&id) else { return };
        let owned = reserved::lease_active(self.cb, id, p.lease);
        if owned
            && self.role_of(id) == Some(Role::Stakeout)
            && let Some(u) = Unit::from_id(self.cb, id)
            && u.health() > 0.0
            && u.team() == self.team_id
        {
            u.set_move_state(0, 0);
        }
        reserved::release_lease(self.cb, id, p.lease);
        self.war.harassment.next_check.insert(id, self.frame + RECHECK);
        if owned {
            if self.role_of(id) == Some(Role::Stakeout) {
                self.stakeouts_release(id);
            }
            self.retreat_handler.forget_retreat_order(id);
        }
        self.op_emit(
            "harassment",
            json!({"event":"end", "unit":id,"target":p.target,"why":why,
            "life":self.frame-p.started,"shot":p.first_shot,"withdrawing":p.withdrawing}),
        );
    }

    fn harassment_drive(&mut self, id: i32, frame: i32) {
        let Some(unit) = Unit::from_id(self.cb, id) else {
            self.harassment_release(id, "gone");
            return;
        };
        let Some(p) = self.war.harassment.plans.get(&id) else { return };
        if unit.health() <= 0.0 || unit.team() != self.team_id || !reserved::lease_active(self.cb, id, p.lease) {
            self.harassment_release(id, "ownership");
            return;
        }
        if reserved::is_reserved(self.cb, id) || self.recovery_owns(id) {
            self.harassment_release(id, "external");
            return;
        }
        let target_live = self.war.targets.get(&p.target).is_some_and(|e| !e.is_dead && frame - e.last_seen <= 90);
        let p = self.war.harassment.plans.get_mut(&id).unwrap();
        if !p.withdrawing {
            let fired = unit
                .weapons()
                .iter()
                .enumerate()
                .any(|(i, w)| p.reloads.iter().any(|&(index, was)| index == i as i32 && w.reload_frame() > was));
            if fired {
                p.observe_shot(frame);
            }
        }
        let turn = !p.withdrawing && p.should_exit(frame, unit.health(), target_live);
        if turn {
            p.withdrawing = true;
        }
        if frame >= p.until {
            // The last Move remains queued; return ownership so repair or a
            // stronger maneuver can recover a stalled unit, never re-attack here.
            let reason = if p.withdrawing && distance(unit.pos(), p.exit) <= 40.0 { "returned" } else { "timeout" };
            self.harassment_release(id, reason);
            return;
        }
        if p.withdrawing && p.exit_ordered && distance(unit.pos(), p.exit) <= 40.0 {
            // The forecast includes stopping here through its horizon. Keep the
            // lease, or the parent controller could charge again before the
            // predicted pursuers die. Firing stays enabled while we wait.
            return;
        }
        if p.withdrawing && (!p.exit_ordered || turn || frame - p.last_order >= 30) {
            if unit.move_to(p.exit, 0) {
                p.exit_ordered = true;
                p.last_order = frame;
            }
            if turn {
                let (exit, target, shot) = (p.exit, p.target, p.first_shot);
                self.op_emit(
                    "harassment",
                    json!({"event":"withdraw","unit":id,"target":target,
                    "shot":shot,"exit":[exit.x,exit.z]}),
                );
            }
        }
    }

    fn harassment_available(&self, u: Unit) -> bool {
        let id = u.id();
        enabled(self.defs.name_of(u), !self.dev_off("harasskodachi"), !self.dev_off("harasswide"))
            && u.health() > 0.0
            && u.team() == self.team_id
            && !u.is_being_built()
            && u.pos().y >= -5.0
            && !u.is_cloaked()
            && !self.harassment_owns(id)
            && !self.coalesce_owns(id)
            && !self.qrf.members.contains_key(&id)
            && !self.recovery_owns(id)
            && !self.retreat_is_retreating(u)
            && !self.precision.missions.contains_key(&id)
            && !self.squad_strike_owns(id)
            && !self.fire_controls(id)
            && !self.limpet_controls(id)
            && !self.license_committed(id)
            && !reserved::is_reserved(self.cb, id)
            && reserved::owner(self.cb, id).is_none_or(|o| matches!(o.module, "stakeouts" | "raider"))
            && (!self.stakeouts.contains(id) || self.stakeouts_qrf_available(id))
    }

    pub(crate) fn harassment_update(&mut self, frame: i32) {
        let ids: Vec<_> = self.war.harassment.plans.keys().copied().collect();
        for id in ids {
            self.harassment_drive(id, frame);
        }
        if frame % 6 != self.offset % 6
            || self.war.harassment.plans.len() >= MAX_ACTIVE
            || !self.fightsim_runs()
            || (self.dev_off("harasskodachi") && self.dev_off("harasswide"))
        {
            return;
        }
        let mut candidates: Vec<_> = self
            .cb
            .team_units()
            .into_iter()
            .filter(|&u| self.harassment_available(u))
            .filter(|u| self.war.harassment.next_check.get(&u.id()).is_none_or(|&f| frame >= f))
            .collect();
        let cursor = self.war.harassment.cursor;
        candidates.sort_by_key(|u| (u.id() <= cursor, u.id()));
        let Some(unit) = candidates.first().copied() else { return };
        let id = unit.id();
        self.war.harassment.cursor = id;
        self.war.harassment.next_check.retain(|_, f| *f > frame);
        self.war.harassment.next_check.insert(id, frame + RECHECK);
        self.harassment_consider(unit, frame);
    }

    fn harassment_consider(&mut self, unit: Unit, frame: i32) {
        let id = unit.id();
        let p = unit.pos();
        let Some(def) = unit.def() else { return };
        let Some(target) = self
            .war
            .targets
            .values()
            .filter(|e| e.identified && !e.is_dead && !e.is_static && !e.can_fly() && e.can_attack() && !e.is_aa)
            .filter(|e| frame - e.last_seen <= 30 && distance(p, e.position) <= 650.0)
            .min_by(|a, b| distance(p, a.position).total_cmp(&distance(p, b.position)))
            .map(|e| (e.unit_id, e.position, e.unit))
        else {
            return;
        };
        if distance(p, target.1) < 1.0 {
            return;
        }
        let exit = get_directional_point(target.1, p, distance(p, target.1) + 500.0);
        let map = self.cb.map();
        if exit.x < 32.0
            || exit.z < 32.0
            || exit.x >= map.width() as f32 * 8.0 - 32.0
            || exit.z >= map.height() as f32 * 8.0 - 32.0
        {
            return;
        }
        // Only simple ground routes. Connectivity alone does not guarantee the
        // straight line used by this predictor: also reject large path detours.
        if !self.terrain_reaches(def, p, target.1, 40.0) || !self.terrain_reaches(def, p, exit, 40.0) {
            return;
        }
        let approach = self.pathfinder.get_path_cost(&self.war, p, target.1, def);
        let retreat = self.pathfinder.get_path_cost(&self.war, target.1, exit, def);
        if approach < 0.0
            || retreat < 0.0
            || !approach.is_finite()
            || !retreat.is_finite()
            || approach > distance(p, target.1) * 1.25 + 64.0
            || retreat > distance(target.1, exit) * 1.25 + 64.0
        {
            return;
        }
        let Some(mut local) = self.local_fight(&[unit]) else { return };
        if local.unknown || local.theirs.is_empty() || local.theirs.len() > MAX_ENEMIES {
            return;
        }
        // Do not omit reinforcements or guns along the withdrawal corridor.
        // Known contacts outside the snapshot veto instead of being assumed safe.
        for e in self.war.targets.values().filter(|e| !e.is_dead && e.can_attack() && !e.is_aa) {
            if !e.is_static && frame - e.last_seen > 300 {
                continue;
            }
            let close = [p, target.1, exit].iter().any(|&q| distance(q, e.position) < e.def_max_weapon_range() + 650.0);
            if close && (!e.identified || !local.theirs.iter().any(|t| t.id == e.unit_id)) {
                return;
            }
        }
        for i in 0..=12 {
            let t = i as f32 / 12.0;
            let q = Vec3::xz(target.1.x + (exit.x - target.1.x) * t, target.1.z + (exit.z - target.1.z) * t);
            if self.war.get_porc_threat(q) > 0.0 || map.elevation_at(q.x, q.z) < -5.0 {
                return;
            }
        }
        let Some(index) = local.theirs.iter().position(|e| e.id == target.0).map(|i| i + 1) else { return };
        local.snap.flags.twod = true;
        local.snap.flags.ordered = true;
        let Some(model) = self.local_model() else { return };
        let Some(choice) = choose(&model, &local.snap, index, exit) else { return };
        let lease = reserved::lend(self.cb, id, OWNER).token();
        let reloads = unit
            .weapons()
            .iter()
            .enumerate()
            .filter(|(_, w)| w.def().is_some_and(|d| d.range() > 0.0 && !d.name().contains("fake")))
            .map(|(i, w)| (i as i32, w.reload_frame()))
            .collect();
        if self.role_of(id) == Some(Role::Stakeout) {
            unit.set_move_state(1, 0);
        }
        let accepted = if choice.fight { unit.fight(target.1, 0) } else { unit.attack(target.2, 0) };
        if !accepted {
            if self.role_of(id) == Some(Role::Stakeout) {
                unit.set_move_state(0, 0);
            }
            reserved::release_lease(self.cb, id, lease);
            return;
        }
        self.war.harassment.plans.insert(
            id,
            Sortie {
                target: target.0,
                exit,
                started: frame,
                attack_until: frame + (choice.shot + 60).clamp(60, 300),
                until: frame + HORIZON,
                withdrawing: false,
                exit_ordered: false,
                first_shot: None,
                reloads,
                last_order: frame,
                lease,
                start_hp: unit.health(),
                fight: choice.fight,
            },
        );
        self.op_emit(
            "harassment",
            json!({"event":"start","unit":id,"def":self.defs.name(def),
            "target":target.0,"fight":choice.fight,"exit":[exit.x,exit.z],"advantage":choice.advantage,
            "shot":choice.shot,"enemies":local.theirs.iter().map(|t|t.id).collect::<Vec<_>>()}),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan() -> Sortie {
        Sortie {
            target: 91,
            exit: Vec3::xz(900.0, 1200.0),
            started: 100,
            attack_until: 220,
            until: 700,
            withdrawing: false,
            exit_ordered: false,
            first_shot: None,
            reloads: vec![(0, 115)],
            last_order: 100,
            lease: 42,
            start_hp: 660.0,
            fight: false,
        }
    }

    #[test]
    fn flags_are_independent_and_roster_is_explicit() {
        assert!(enabled("tankraid", true, false));
        assert!(!enabled("tankraid", false, true));
        for name in [
            "tankheavyraid",
            "hoverheavyraid",
            "tankheavyassault",
            "tankassault",
            "vehassault",
            "vehcapture",
            "vehraid",
            "jumpraid",
        ] {
            assert!(!enabled(name, true, false));
            assert!(enabled(name, false, true));
        }
        assert!(!enabled("bomberprec", true, true));
        let ai = crate::military::qrf::loan_test_ai();
        assert!(!ai.dev_off("harasskodachi"));
        assert!(ai.dev_off("harasswide"));
        reserved::clear(ai.cb);
    }

    #[test]
    fn exit_is_latched_on_shot_not_the_forecast_clock() {
        let mut p = plan();
        assert!(!p.should_exit(115, 660.0, true), "existing cooldown is not a shot");
        p.observe_shot(137);
        p.observe_shot(151);
        assert_eq!(p.first_shot, Some(137));
        assert!(p.should_exit(137, 660.0, true));
        assert!(plan().should_exit(220, 660.0, true));
        assert!(plan().should_exit(110, 660.0, false));
        assert!(plan().should_exit(110, 500.0, true));
    }

    #[test]
    fn nondefault_checkpoint_continues_the_same_exit_and_cooldown() {
        let mut h = Harassment::default();
        h.cursor = 78;
        h.next_check.insert(78, 456);
        let mut p = plan();
        p.observe_shot(137);
        h.plans.insert(12, p);
        let bytes = checkpoint::encode(&h).unwrap();
        let restored: Harassment = checkpoint::rmp_serde::from_slice(&bytes).unwrap();
        assert_eq!(checkpoint::encode(&restored).unwrap(), bytes);
        assert_eq!(restored.plans[&12], h.plans[&12]);
        for frame in [137, 140, 180, 700] {
            assert_eq!(
                restored.plans[&12].should_exit(frame, 600.0, true),
                h.plans[&12].should_exit(frame, 600.0, true)
            );
        }
        assert_eq!(restored.cursor, 78);
        assert_eq!(restored.next_check[&78], 456);
    }

    #[test]
    fn shot_withdraws_with_authority_and_disable_preserves_the_exit() {
        let mut ai = crate::military::qrf::loan_test_ai();
        ai.frame = 137;
        let unit = Unit::from_id(ai.cb, 1).unwrap();
        let lease = reserved::lend(ai.cb, 1, OWNER).token();
        let mut p = plan();
        p.lease = lease;
        ai.war.harassment.plans.insert(1, p);
        assert!(!ai.qrf_can_borrow_stakeout(1));
        assert!(!reserved::as_module("stakeouts", || unit.move_to(Vec3::ZERO, 0)));
        assert!(!reserved::as_module("retreat", || unit.move_to(Vec3::ZERO, 0)));
        assert!(reserved::as_module("harassment", || unit.move_to(Vec3::ZERO, 0)));
        ai.set_feature_off("harasskodachi", true);
        assert!(ai.harassment_owns(1));
        reserved::as_module("harassment.weaponFired", || ai.harassment_fired(unit));
        let p = &ai.war.harassment.plans[&1];
        assert!(p.withdrawing);
        assert_eq!(p.first_shot, Some(137));
        assert_eq!(p.last_order, 137);
        assert!(!ai.retreat_is_retreating(unit));
        reserved::as_module("harassment", || ai.harassment_release(1, "test"));
        assert_eq!(reserved::owner(ai.cb, 1).unwrap().module, "stakeouts");
        assert!(!ai.harassment_owns(1));
        reserved::clear(ai.cb);
    }

    #[test]
    fn newer_lease_and_operator_reservation_win() {
        let mut ai = crate::military::qrf::loan_test_ai();
        let unit = Unit::from_id(ai.cb, 1).unwrap();
        let mut p = plan();
        p.lease = reserved::lend(ai.cb, 1, OWNER).token();
        ai.war.harassment.plans.insert(1, p);
        let recovery = reserved::lend(ai.cb, 1, reserved::Owner { module: "recovery", helpers: &[] });
        reserved::as_module("harassment", || ai.harassment_drive(1, 137));
        assert!(recovery.active());
        assert!(ai.war.harassment.plans.is_empty());
        recovery.release();
        let mut p = plan();
        p.lease = reserved::lend(ai.cb, 1, OWNER).token();
        ai.war.harassment.plans.insert(1, p);
        reserved::reserve(ai.cb, 1);
        assert!(!reserved::as_module("harassment", || unit.move_to(Vec3::ZERO, 0)));
        reserved::as_module("harassment", || ai.harassment_drive(1, 137));
        assert!(reserved::is_reserved(ai.cb, 1));
        assert!(ai.war.harassment.plans.is_empty());
        reserved::clear(ai.cb);
    }
}

#[cfg(test)]
mod continuation_tests {
    use super::*;
    #[test]
    fn arrival_holds_until_the_forecast_horizon_even_after_disable() {
        let mut ai = crate::military::qrf::loan_test_ai();
        let unit = Unit::from_id(ai.cb, 1).unwrap();
        let p = Sortie {
            target: 91,
            exit: unit.pos(),
            started: 100,
            attack_until: 220,
            until: 700,
            withdrawing: true,
            exit_ordered: true,
            first_shot: Some(137),
            reloads: vec![(0, 150)],
            last_order: 137,
            lease: reserved::lend(ai.cb, 1, OWNER).token(),
            start_hp: 660.0,
            fight: true,
        };
        ai.war.harassment.plans.insert(1, p);
        let bytes = checkpoint::encode(&ai.war.harassment).unwrap();
        ai.war.harassment = checkpoint::rmp_serde::from_slice(&bytes).unwrap();
        ai.set_feature_off("harasskodachi", true);
        ai.frame = 200;
        reserved::as_module("harassment", || ai.harassment_drive(1, 200));
        assert!(ai.harassment_owns(1));
        assert_eq!(ai.war.harassment.plans[&1].last_order, 137);
        ai.frame = 700;
        reserved::as_module("harassment", || ai.harassment_drive(1, 700));
        assert!(!ai.harassment_owns(1));
        assert_eq!(reserved::owner(ai.cb, 1).unwrap().module, "stakeouts");
        assert_eq!(ai.war.harassment.next_check[&1], 820);
        reserved::clear(ai.cb);
    }
}

#[cfg(test)]
mod command_tests {
    use super::*;
    #[test]
    fn rejected_move_retries_and_arrival_does_not_preserve_an_attack() {
        let mut ai = crate::military::qrf::loan_test_ai();
        let u = Unit::from_id(ai.cb, 1).unwrap();
        let p = Sortie {
            target: 91,
            exit: u.pos(),
            started: 100,
            attack_until: 220,
            until: 700,
            withdrawing: false,
            exit_ordered: false,
            first_shot: Some(137),
            reloads: vec![],
            last_order: 100,
            lease: reserved::lend(ai.cb, 1, OWNER).token(),
            start_hp: 660.0,
            fight: false,
        };
        ai.war.harassment.plans.insert(1, p);
        reserved::mute(ai.cb, "harassment");
        reserved::as_module("harassment", || ai.harassment_drive(1, 137));
        assert!(ai.war.harassment.plans[&1].withdrawing);
        assert!(!ai.war.harassment.plans[&1].exit_ordered);
        reserved::unmute(ai.cb, "harassment");
        reserved::as_module("harassment", || ai.harassment_drive(1, 138));
        assert!(ai.war.harassment.plans[&1].exit_ordered);
        assert_eq!(ai.war.harassment.plans[&1].last_order, 138);
        reserved::clear(ai.cb);
    }
}
