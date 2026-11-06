//! Stateless attack/exit queries for the experimental harassment controller.
//! The caller supplies a reachable exit; this flat-plane query cannot certify
//! terrain, unseen reinforcement, turn rates or protection at that destination.
use super::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExitTrigger {
    Never,
    AtFrame(i32),
    AfterFirstShot(i32),
    FirstEnemyDeath,
}

#[derive(Clone, Copy, Debug)]
pub struct HarassOrder {
    pub trigger: ExitTrigger,
    /// Snapshot index for a direct Attack; None lets UnitAI choose.
    pub target: Option<usize>,
    /// World position of the Move destination; firing remains enabled.
    pub exit: (f32, f32),
    /// Whether UnitAI controls approach movement. A manual Attack still
    /// skirmishes its specified target in Zero-K; it does not disable UnitAI.
    pub own_kiting: bool,
    /// False is the enemy direct-pursuit stress branch, with firing certain.
    pub enemy_kiting: bool,
}

#[derive(Debug)]
pub struct HarassUnit {
    pub hp: f32,
    /// Raw expected-damage/variance death estimate, not a calibrated safety guarantee.
    pub p_dead: f32,
    pub death_frame: Option<i32>,
    pub position: (f32, f32),
    pub slow: f32,
    pub reloads: Vec<i32>,
}

#[derive(Debug)]
pub struct HarassOutcome {
    pub units: Vec<HarassUnit>,
    pub first_shot: Option<i32>,
    pub exit_frame: Option<i32>,
    pub shots: usize,
    /// All our surviving units reached the destination. Death is not escape.
    pub reached: bool,
    /// Minimum distance beyond any surviving armed enemy's nominal range.
    /// None means no surviving armed enemies. This is not a pursuit bound.
    pub clearance: Option<f32>,
}

pub(super) struct SortieRun {
    order: HarassOrder,
    exit: (f64, f64),
    pub(super) first_shot: Option<i32>,
    pub(super) shots: usize,
    exit_frame: Option<i32>,
}

impl SortieRun {
    pub(super) fn update(&mut self, units: &[MechUnit], frame: i32) {
        let due = match self.order.trigger {
            ExitTrigger::Never => false,
            ExitTrigger::AtFrame(at) => frame >= at.max(0),
            ExitTrigger::AfterFirstShot(delay) => self.first_shot.is_some_and(|s| frame >= s + delay.max(0)),
            ExitTrigger::FirstEnemyDeath => units.iter().any(|u| u.orig_side == 1 && !u.u.alive),
        };
        if due {
            self.exit_frame.get_or_insert(frame);
        }
    }

    pub(super) fn active_exit(&self) -> Option<(f64, f64)> {
        self.exit_frame.map(|_| self.exit)
    }

    pub(super) fn kites(&self, side: usize) -> bool {
        if side == 0 { self.order.own_kiting } else { self.order.enemy_kiting }
    }
}

impl Model {
    /// One uninterrupted rollout: the order changes, the combat state does not.
    /// Both sides retain their snapshot engagement settings. Caller sets ordered
    /// for our committed units. Runs to the horizon even after enemy deaths:
    /// pending burns must resolve and the exit must actually be reached.
    pub fn harassment(&self, snap: &FightSnapshot, horizon: i32, order: HarassOrder) -> HarassOutcome {
        assert!(snap.flags.twod, "harassment requires plane geometry");
        assert!(horizon > 0 && order.exit.0.is_finite() && order.exit.1.is_finite());
        assert!(snap.units.iter().any(|u| u.side == 0) && snap.units.iter().any(|u| u.side == 1));
        assert!(order.target.is_none_or(|i| snap.units.get(i).is_some_and(|u| u.side == 1)));
        let mut units = self.mech_units(snap);
        // A chosen chase-and-fire counterfactual must not also discount
        // enemy shots by P(the unit engages at all) from ordinary game logs.
        if !order.enemy_kiting {
            for u in units.iter_mut().filter(|u| u.u.side == 1) {
                u.engage = 1.0;
            }
        }
        let kiting = self.kiting(snap, &units);
        let center = |side| {
            let us: Vec<_> = snap.units.iter().filter(|u| u.side == side).collect();
            let n = us.len() as f64;
            (us.iter().map(|u| u.x as f64).sum::<f64>() / n, us.iter().map(|u| u.z as f64).sum::<f64>() / n)
        };
        let (a, b) = (center(0), center(1));
        let len = (b.0 - a.0).hypot(b.1 - a.1);
        let axis = if len >= 1.0 { ((b.0 - a.0) / len, (b.1 - a.1) / len) } else { (1.0, 0.0) };
        let (dx, dz) = (order.exit.0 as f64 - a.0, order.exit.1 as f64 - a.1);
        let exit = (dx * axis.0 + dz * axis.1, -dx * axis.1 + dz * axis.0);
        let n = units.len();
        let mut run = Run {
            sortie: Some(SortieRun { order, exit, first_shot: None, shots: 0, exit_frame: None }),
            construction: None,
            progress: 1.0,
            defense: None,
            defense_engagement: None,
            hazard: None,
            withdraw: false,
            pass: false,
            focus: order.target,
            insertion: None,
            kiting: Some(&kiting),
            horizon,
            track: Some(Track { var: vec![0.0; n], death_f: vec![None; n], captured_f: vec![None; n] }),
            twod: true,
            least_health: snap.flags.least_health(),
            bomb: None,
            bomb_ln_survive: 0.0,
        };
        let (_, end) = run_mech(&units, &self.hits, self.mech.stun_cap_s, &mut run);
        let tr = run.track.unwrap();
        let sortie = run.sortie.unwrap();
        let ours: Vec<_> = end.iter().filter(|u| u.orig_side == 0).collect();
        let enemies: Vec<_> = end.iter().filter(|u| u.u.side == 1 && u.u.alive && u.u.armed).collect();
        let reached = sortie.exit_frame.is_some()
            && ours.iter().all(|u| u.u.alive && u.u.side == 0 && (u.u.x - exit.0).hypot(u.lat - exit.1) <= 20.0);
        let clearance = ours
            .iter()
            .filter(|u| u.u.alive)
            .flat_map(|u| enemies.iter().map(move |e| (apart(u, e) - e.u.max_range) as f32))
            .reduce(f32::min);
        let result = end
            .iter()
            .enumerate()
            .map(|(i, u)| {
                let captured = u.u.side != u.orig_side;
                HarassUnit {
                    hp: if u.u.alive && !captured { u.u.hp.max(0.0) as f32 } else { 0.0 },
                    p_dead: if captured {
                        1.0
                    } else {
                        p_dead(units[i].u.hp, u.u.hp, tr.death_f[i], tr.var[i], horizon) as f32
                    },
                    death_frame: if captured { tr.captured_f[i] } else { tr.death_f[i] },
                    position: (
                        (a.0 + u.u.x * axis.0 - u.lat * axis.1) as f32,
                        (a.1 + u.u.x * axis.1 + u.lat * axis.0) as f32,
                    ),
                    slow: u.slowed() as f32,
                    reloads: u.u.weapons.iter().map(|w| (w.next - horizon).max(0)).collect(),
                }
            })
            .collect();
        HarassOutcome {
            units: result,
            first_shot: sortie.first_shot,
            exit_frame: sortie.exit_frame,
            shots: sortie.shots,
            reached,
            clearance,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::military::fightsim::{SnapUnit, UnitSpec, WeaponSpec};
    use std::sync::Arc;

    fn fixture() -> FightSnapshot {
        let spec = |name: &str, damage, speed| {
            Arc::new(UnitSpec {
                def: name.into(),
                health: 1000.0,
                cost: 100.0,
                speed,
                mobile: speed > 0.0,
                weapons: vec![WeaponSpec {
                    name: "test_beam".into(),
                    weapon_type: "BeamLaser".into(),
                    range: 150.0,
                    reload: 1.0,
                    salvo: 1,
                    damage,
                    leading: None,
                }],
                ..Default::default()
            })
        };
        let mut snap = FightSnapshot {
            units: vec![
                SnapUnit { spec: spec("ours", 10.0, 3.0), side: 0, hp: 1000.0, ..Default::default() },
                SnapUnit { spec: spec("theirs", 10.0, 0.0), side: 1, hp: 1000.0, x: 200.0, ..Default::default() },
            ],
            ..Default::default()
        };
        snap.flags.twod = true;
        snap.flags.ordered = true;
        snap
    }

    fn order(trigger: ExitTrigger) -> HarassOrder {
        HarassOrder { trigger, target: None, exit: (-300.0, 100.0), own_kiting: true, enemy_kiting: true }
    }

    #[test]
    fn never_exit_matches_ordinary_fates_while_both_sides_live() {
        let m = Model::load().unwrap();
        let s = fixture();
        let expected = m.kiting_fates(&s, 180);
        let got = m.harassment(&s, 180, order(ExitTrigger::Never));
        for (e, g) in expected.iter().zip(&got.units) {
            assert_eq!(e.hp_left, g.hp);
            assert_eq!(e.p_raw, g.p_dead);
            assert_eq!(e.death_frame, g.death_frame);
        }
        assert_eq!(got.exit_frame, None);
    }

    #[test]
    fn first_shot_exit_is_latched_and_reaches_the_actual_waypoint() {
        let m = Model::load().unwrap();
        let s = fixture();
        let got = m.harassment(&s, 180, order(ExitTrigger::AfterFirstShot(0)));
        assert_eq!(got.exit_frame, got.first_shot.map(|f| f + DT));
        assert!(got.units[1].hp < 1000.0);
        assert!(got.units[0].hp < 1000.0, "damage before turning must persist");
        assert!(got.reached);
        assert!(got.clearance.unwrap() > 200.0);
        assert!((got.units[0].position.0 + 300.0).abs() < 0.01);
        assert!((got.units[0].position.1 - 100.0).abs() < 0.01);
        assert!(got.shots < m.harassment(&s, 180, order(ExitTrigger::Never)).shots);
    }

    #[test]
    fn delayed_exit_preserves_reload_and_never_fires_a_fresh_opening_shot() {
        let m = Model::load().unwrap();
        let s = fixture();
        let short = m.harassment(&s, 20, order(ExitTrigger::Never));
        let exit = m.harassment(&s, 20, order(ExitTrigger::AfterFirstShot(0)));
        assert_eq!(short.shots, 1);
        assert_eq!(exit.shots, 1);
        assert_eq!(short.units[0].reloads, exit.units[0].reloads);
        let delayed = m.harassment(&s, 180, order(ExitTrigger::AfterFirstShot(60)));
        assert_eq!(delayed.exit_frame, delayed.first_shot.map(|f| f + 60));
        assert!(delayed.shots > exit.shots);
    }

    #[test]
    fn pursuit_stress_commits_enemy_fire_instead_of_using_the_engagement_prior() {
        let m = Model::load().unwrap();
        let mut s = fixture();
        s.units[1].x = 100.0;
        let prior = m.harassment(&s, 3, order(ExitTrigger::AtFrame(0)));
        let mut chase = order(ExitTrigger::AtFrame(0));
        chase.enemy_kiting = false;
        let committed = m.harassment(&s, 3, chase);
        assert!(committed.units[0].hp < prior.units[0].hp);
        assert_eq!(committed.units[1].hp, prior.units[1].hp);
    }

    #[test]
    fn finishing_the_enemy_does_not_teleport_us_to_safety() {
        let m = Model::load().unwrap();
        let mut s = fixture();
        s.units[1].hp = 1.0;
        let short = m.harassment(&s, 30, order(ExitTrigger::FirstEnemyDeath));
        assert_eq!(short.units[1].hp, 0.0);
        assert!(!short.reached);
        let long = m.harassment(&s, 180, order(ExitTrigger::FirstEnemyDeath));
        assert!(long.reached);
        assert!(long.clearance.is_none());
    }
}
