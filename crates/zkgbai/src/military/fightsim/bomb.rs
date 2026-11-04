//! Should a placed Imp or Snitch charge the enemies near it, or sit tight?
//! (`bombcharge`, 2026-11-03.)
//!
//! These bombs cloak only while burrowed: the first step decloaks them
//! (cloakbomb.lua / shieldbomb.lua `UnBurrow`), so the run is in the open
//! from its first step, and a fight without cloak is the right picture of
//! it. Sitting tight is the same fight without the bomb (it stays hidden).
//! A bomb shot down still blows up where it dies.
//!
//! The expected-damage fight cannot take a 50-hp bomb's run on its own: any
//! one shot whose hit chance times damage passes 50 kills it there. So the
//! charge is two branches. In one the bomb gets through every shot that
//! would kill it with one hit (their chances of hitting make the chance of
//! that branch), while hits too weak to kill alone still land as expected
//! damage (a Mace's beam burns it down whatever its luck). In the other it
//! is shot down where it stands. Enemies busy shooting at something else
//! have just fired: their weapons start partway through their reload,
//! averaged over the reload; idle ones fire at once.

use super::FightSnapshot;
use super::Model;
use super::mech::{BombMode, BombRun};

/// Busy units' weapons start this far through their reload, in turn.
const BUSY_PHASES: [f64; 3] = [1.0 / 6.0, 0.5, 5.0 / 6.0];

/// The judgement of one charge.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct BombVerdict {
    /// Chance the bomb gets through the one-hit kills aimed at it.
    pub p_through: f32,
    /// Their metal the charge kills beyond what dies anyway (expected).
    pub gain: f32,
    /// Our metal (but the bomb) the charge spares.
    pub saved: f32,
    /// Chance the bomb is spent by the horizon.
    pub spent: f32,
    /// `gain + saved - spent * cost`: what charging is worth over sitting.
    pub net: f32,
    /// Enemy losses less allied losses while reserving this bomb.
    pub reserve_trade: f32,
    /// When the bomb went off or died in the run that got through, in the
    /// middle phase (frames).
    pub went_off: Option<i32>,
}

impl Model {
    /// Charge or sit for bomb `bomb` (a side-A kamikaze of `snap`): every
    /// unit's fate over `horizon` frames with the bomb running at its
    /// nearest enemy, against the same fight without it. `busy[i]`: unit
    /// `i` is shooting at something else (its weapons are mid-reload).
    /// Costs are the snapshot's; deaths are the sim's own chances
    /// (`p_raw`, as the veto counts them): the calibrated ones are squeezed
    /// towards the middle (a Glaive a Snitch surely blows up counted 0.6 of
    /// its metal), while the bomb is spent for certain.
    pub fn bomb_charge(&self, snap: &FightSnapshot, bomb: usize, busy: &[bool], horizon: i32) -> BombVerdict {
        self.bomb_charge_inner(snap, bomb, busy, horizon, false)
    }

    /// Squad commitment: the escorts actually receive attack orders, so
    /// side A fires for certain instead of using the fitted engagement prior.
    pub fn limpet_charge(&self, snap: &FightSnapshot, bomb: usize, horizon: i32) -> BombVerdict {
        self.bomb_charge_inner(snap, bomb, &[], horizon, true)
    }

    fn bomb_charge_inner(
        &self,
        snap: &FightSnapshot,
        bomb: usize,
        busy: &[bool],
        horizon: i32,
        ordered: bool,
    ) -> BombVerdict {
        let mut snap = snap.clone();
        snap.flags.bombs = true;
        let n = snap.units.len();
        if bomb >= n {
            return BombVerdict::default();
        }
        let cost = snap.units[bomb].spec.cost;
        let mut sit = snap.clone();
        sit.units.remove(bomb);
        let phases: &[f64] = if busy.iter().any(|&b| b) { &BUSY_PHASES } else { &[0.0] };
        // Their metal killed less ours lost (the bomb aside), from fates in
        // snapshot order; `skip` = the bomb's index in it, if there.
        let tally = |s: &FightSnapshot, fates: &[super::UnitFate], skip: Option<usize>| {
            let (mut theirs, mut ours) = (0.0f32, 0.0f32);
            for (i, (u, f)) in s.units.iter().zip(fates).enumerate() {
                if Some(i) == skip {
                    continue;
                }
                if u.side == 1 {
                    theirs += u.spec.cost * f.p_raw;
                } else {
                    ours += u.spec.cost * f.p_raw;
                }
            }
            (theirs, ours)
        };
        let mut v = BombVerdict::default();
        for (k, &ph) in phases.iter().enumerate() {
            let phase: Vec<f64> =
                (0..n).map(|i| if busy.get(i).copied().unwrap_or(false) { ph } else { 0.0 }).collect();
            let sit_phase: Vec<f64> = phase.iter().enumerate().filter(|&(i, _)| i != bomb).map(|(_, &p)| p).collect();
            let (fs, _) = self.bomb_fates(&sit, horizon, &sit_phase, None, ordered);
            let (fc, ln_s) =
                self.bomb_fates(&snap, horizon, &phase, Some(BombRun { index: bomb, mode: BombMode::Charge }), ordered);
            let (fd, _) =
                self.bomb_fates(&snap, horizon, &phase, Some(BombRun { index: bomb, mode: BombMode::Down }), ordered);
            let s = ln_s.exp() as f32;
            let (bs, as_) = tally(&sit, &fs, None);
            let (bc, ac) = tally(&snap, &fc, Some(bomb));
            let (bd, ad) = tally(&snap, &fd, Some(bomb));
            let spent = s * if fc[bomb].dead { 1.0 } else { 0.0 } + (1.0 - s);
            let w = 1.0 / phases.len() as f32;
            v.reserve_trade += w * (bs - as_);
            v.p_through += w * s;
            v.gain += w * (s * bc + (1.0 - s) * bd - bs);
            v.saved += w * (as_ - (s * ac + (1.0 - s) * ad));
            v.spent += w * spent;
            if k == phases.len() / 2 {
                v.went_off = fc[bomb].death_frame;
            }
        }
        v.net = v.gain + v.saved - v.spent * cost;
        v
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Arc;

    use super::*;
    use crate::military::fightsim::{SimFlags, SnapUnit, UnitSpec, WeaponSpec, data_lines};

    const UNITS: &str = include_str!("testdata/mech_bombs_units.tsv");

    fn specs() -> HashMap<String, Arc<UnitSpec>> {
        data_lines(UNITS)
            .map(|l| {
                let f: Vec<&str> = l.split('\t').collect();
                let weapons = (f[9].split('|').filter(|w| !w.is_empty()))
                    .map(|w| {
                        let p: Vec<&str> = w.split(':').collect();
                        WeaponSpec {
                            name: p[0].into(),
                            weapon_type: p[1].into(),
                            range: p[2].parse().unwrap(),
                            reload: p[3].parse().unwrap(),
                            salvo: p[4].parse().unwrap(),
                            damage: p[5].parse().unwrap(),
                        }
                    })
                    .collect();
                let spec = UnitSpec {
                    def: f[0].into(),
                    cost: f[1].parse().unwrap(),
                    health: f[2].parse().unwrap(),
                    speed: f[3].parse().unwrap(),
                    mobile: f[4] == "1",
                    air: f[5] == "1",
                    builder: f[6] == "1",
                    com: f[7] == "1",
                    radius: f[8].parse().unwrap(),
                    weapons,
                };
                (f[0].to_owned(), Arc::new(spec))
            })
            .collect()
    }

    /// Our Lotus at the origin, the bomb 330 out in front (last), and the
    /// enemy: `theirs` as (def, x, z); everyone of theirs busy with the
    /// Lotus when `busy`.
    fn staged(bomb: &str, theirs: &[(&str, f32, f32)], twod: bool, busy: bool) -> (FightSnapshot, usize, Vec<bool>) {
        let s = specs();
        let unit =
            |side, def: &str, x, z| SnapUnit { side, spec: s[def].clone(), hp: s[def].health, x, z, tubes: Vec::new() };
        let mut units = vec![unit(0, "turretlaser", 0.0, 0.0)];
        units.extend(theirs.iter().map(|&(d, x, z)| unit(1, d, x, z)));
        units.push(unit(0, bomb, 0.0, 330.0));
        let bomb_i = units.len() - 1;
        let flags = SimFlags::from_switches(twod, true, true);
        let busy = units.iter().map(|u| busy && u.side == 1).collect();
        (FightSnapshot { units, flags }, bomb_i, busy)
    }

    #[test]
    fn limpet_squad_trade_prices_the_bomb_and_escorts_in_both_branches() {
        let mut specs = specs();
        specs.insert(
            "vehriot".into(),
            Arc::new(UnitSpec {
                def: "vehriot".into(),
                cost: 240.0,
                health: 1020.0,
                speed: 2.1,
                mobile: true,
                radius: 12.0,
                weapons: vec![WeaponSpec {
                    name: "vehriot_vehriot_weapon".into(),
                    weapon_type: "Cannon".into(),
                    range: 285.0,
                    reload: 1.77,
                    salvo: 1,
                    damage: 250.2,
                }],
                ..Default::default()
            }),
        );
        let unit = |side, name: &str, x, z| SnapUnit {
            side,
            spec: specs[name].clone(),
            hp: specs[name].health,
            x,
            z,
            tubes: Vec::new(),
        };
        let m = Model::load().unwrap();
        for flags in [
            SimFlags::default(),
            SimFlags::from_switches(false, false, true),
            SimFlags::from_switches(false, true, true),
            SimFlags::from_switches(true, true, true),
        ] {
            let mut units: Vec<_> =
                (0..4).map(|i| unit(0, "amphraid", -20.0 * (i % 2) as f32, (i as f32 - 1.5) * 35.0)).collect();
            units.push(unit(0, "amphbomb", 80.0, 0.0));
            units.push(unit(1, "vehriot", 320.3, -75.0));
            units.push(unit(1, "vehriot", 320.3, 75.0));
            let mut snap = FightSnapshot { units, flags };
            let v = m.limpet_charge(&snap, 4, 600);
            assert!(v.reserve_trade.is_finite() && v.net.is_finite());
            assert!((v.net - (v.gain + v.saved - v.spent * 160.0)).abs() < 0.01);
            if flags.twod {
                assert!(v.net > 400.0 && v.reserve_trade + v.net > 100.0, "{v:?}");
            }
            snap.units.truncate(5);
            snap.units.extend((0..3).map(|i| unit(1, "energysolar", 320.3, (i as f32 - 1.0) * 35.0)));
            let waste = m.limpet_charge(&snap, 4, 600);
            assert!(waste.net < -150.0, "{flags:?} {waste:?}");
            assert!(waste.reserve_trade > 200.0);
        }
    }

    const ROGUES: [(&str, f32, f32); 4] = [
        ("shieldskirm", -45.0, 520.0),
        ("shieldskirm", 45.0, 520.0),
        ("shieldskirm", -45.0, 580.0),
        ("shieldskirm", 45.0, 580.0),
    ];
    const RONINS: [(&str, f32, f32); 5] = [
        ("cloakskirm", -60.0, 450.0),
        ("cloakskirm", 0.0, 440.0),
        ("cloakskirm", 60.0, 450.0),
        ("cloakskirm", -30.0, 490.0),
        ("cloakskirm", 30.0, 490.0),
    ];

    #[test]
    fn sit_tight_by_a_mace_whatever_its_reload() {
        let m = Model::load().unwrap();
        for bomb in ["cloakbomb", "shieldbomb"] {
            for twod in [false, true] {
                for busy in [false, true] {
                    let (snap, b, busy_v) = staged(bomb, &[("hoverriot", 260.0, 330.0)], twod, busy);
                    let v = m.bomb_charge(&snap, b, &busy_v, 450);
                    // Ten beam shots a second, each a kill on its own: no luck gets it through.
                    assert!(v.net < 0.0 && v.p_through < 0.01, "{bomb} 2d={twod} busy={busy}: {v:?}");
                }
            }
        }
    }

    #[test]
    fn a_snitch_charges_busy_skirmishers_and_ready_ones_are_worth_less() {
        let m = Model::load().unwrap();
        for twod in [false, true] {
            for theirs in [&ROGUES[..], &RONINS[..]] {
                let (snap, b, busy) = staged("shieldbomb", theirs, twod, true);
                let v = m.bomb_charge(&snap, b, &busy, 450);
                assert!(v.net > 0.0 && v.gain > 0.0, "busy {} 2d={twod}: {v:?}", theirs[0].0);
                let (snap, b, idle) = staged("shieldbomb", theirs, twod, false);
                let w = m.bomb_charge(&snap, b, &idle, 450);
                assert!(w.p_through < v.p_through, "ready {} 2d={twod}: {w:?} vs busy {v:?}", theirs[0].0);
            }
        }
        // Ready Rogues: four missiles at the start, each a kill if it hits.
        let (snap, b, busy) = staged("shieldbomb", &ROGUES, true, true);
        let v = m.bomb_charge(&snap, b, &busy, 450);
        let (snap, b, idle) = staged("shieldbomb", &ROGUES, true, false);
        let w = m.bomb_charge(&snap, b, &idle, 450);
        assert!(w.net < v.net, "ready {w:?} vs busy {v:?}");
    }

    #[test]
    fn an_imp_stun_is_worth_nothing_with_no_one_to_finish() {
        // The Lotus (460) cannot reach Rogues at 520+: the stun kills
        // nothing, and the Imp is gone.
        let m = Model::load().unwrap();
        let (snap, b, busy) = staged("cloakbomb", &ROGUES, true, true);
        let v = m.bomb_charge(&snap, b, &busy, 450);
        assert!(v.gain.abs() < 25.0 && v.net < 0.0, "{v:?}");
    }

    #[test]
    fn bombcharge_is_on_by_default() {
        assert!(!crate::ai::OFF_BY_DEFAULT.contains(&"bombcharge"));
    }

    #[test]
    fn a_bomb_nobody_shoots_at_gets_through() {
        let m = Model::load().unwrap();
        let (snap, b, busy) =
            staged("shieldbomb", &[("energysolar", 0.0, 520.0), ("energysolar", 40.0, 540.0)], true, false);
        let v = m.bomb_charge(&snap, b, &busy, 450);
        assert!((v.p_through - 1.0).abs() < 1e-6 && v.went_off.is_some() && v.gain > 0.0, "{v:?}");
    }

    /// What one judgement costs (all branches and phases, plane fight), by
    /// size: `cargo test --release bomb_charge_cost -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn bomb_charge_cost() {
        let m = Model::load().unwrap();
        let s = specs();
        for k in [3usize, 8, 13, 19] {
            let mut theirs = Vec::new();
            for i in 0..k {
                let def = ["shieldskirm", "cloakskirm", "hoverriot", "cloakraid"][i % 4];
                theirs.push((def, (i % 5) as f32 * 50.0 - 100.0, 480.0 + (i / 5) as f32 * 50.0));
            }
            let (mut snap, b, busy) = staged("shieldbomb", &theirs, true, true);
            for i in 0..k {
                let x = (i % 5) as f32 * 50.0 - 100.0;
                snap.units.insert(
                    1,
                    SnapUnit {
                        side: 0,
                        spec: s["cloakraid"].clone(),
                        hp: 230.0,
                        x,
                        z: -80.0 - (i / 5) as f32 * 40.0,
                        tubes: Vec::new(),
                    },
                );
            }
            let b = b + k;
            let busy: Vec<bool> = std::iter::repeat_n(false, k).chain(busy).collect();
            let n = 20;
            let start = std::time::Instant::now();
            for _ in 0..n {
                let _ = m.bomb_charge(&snap, b, &busy, 450);
            }
            let us = start.elapsed().as_secs_f64() * 1e6 / n as f64;
            eprintln!("bomb_charge: {} units, {us:.0} us per judgement", snap.units.len());
        }
    }
}
