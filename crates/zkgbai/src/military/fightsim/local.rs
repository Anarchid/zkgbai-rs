//! Local fights priced in metal, for decisions that ask the predictor
//! synchronously (`coalesce`, `stayveto`): the fight a few of our units are
//! in now, each unit's fate from [`Model::fates`] (calibrated P(dead) at a
//! short horizon), and what those deaths are worth to us.
//!
//! An enemy's death is worth its metal times [`weight`]: commanders,
//! factories and economy are lasting damage, worth more than the metal
//! (a commander is also the opponent's best builder and its D-gun). Our
//! own units are worth their metal.
//!
//! Decisions read the sim's own P(dead) ([`UnitFate::p_raw`]), not the
//! calibrated one: the calibration was fitted on every fight the segmenter
//! found, a third of which never made contact, and squeezes every unit
//! into about 0.08-0.66 (eight Glaives on a lone Lotus: the Lotus 0.66,
//! each Glaive 0.08), so it cannot say "if we commit, this dies". The raw
//! sim is over-sure and fires a perfect alpha strike (it over-rates
//! packs), and a-move judges over-predict enemy losses (1.7x in the pack
//! scenarios): the callers' thresholds are demanding, and every judgement
//! journals the calibrated sums beside the raw ones for a refit on these
//! decisions.

use std::sync::Arc;
use std::time::Instant;

use springai::{Unit, UnitDef, Vec3};

use super::{FateMode, FightSnapshot, Model, SnapUnit, UnitFate};
use crate::Ai;
use crate::military::Enemy;
use crate::util::distance;

/// Fates horizon (frames): 10 s, the fight at hand.
pub const HORIZON: i32 = 300;
/// Enemies this close to one of our units are in its fight...
pub const CONTACT: f32 = 700.0;
/// ...and armed ground enemies this close to the fight's middle join it.
pub const SUPPORT: f32 = 1100.0;
/// Mobile enemies unseen for longer are not counted (frames).
const SEEN_WITHIN: i32 = 90;
/// A larger fight is not judged (cost, and a battle is not a skirmish).
const MAX_UNITS: usize = 64;

/// What an enemy's death is worth per metal.
pub fn weight(e: &Enemy) -> f32 {
    if e.is_com {
        2.0
    } else if e.is_static && (!e.def_builds_nothing() || !e.can_attack()) {
        // Factories, nanos and economy: lasting damage.
        1.5
    } else if e.is_worker {
        1.3
    } else {
        1.0
    }
}

/// An enemy worth a gang of raiders paying for: a commander, a factory, a
/// heavy, or a big economy building.
pub fn prize(e: &Enemy, cost: f32) -> bool {
    !e.can_fly()
        && (e.is_com
            || (e.is_static && !e.def_builds_nothing() && cost >= 250.0)
            || (!e.is_static && cost >= 600.0)
            || (e.is_static && !e.can_attack() && cost >= 300.0))
}

/// One of our units in a local fight.
#[derive(Clone, Debug)]
pub struct Ours {
    pub id: i32,
    pub cost: f32,
}

/// One enemy in a local fight.
#[derive(Clone, Debug)]
pub struct Theirs {
    pub id: i32,
    pub def: String,
    pub cost: f32,
    /// What its death is worth (cost x [`weight`]).
    pub value: f32,
    pub armed: bool,
    pub prize: bool,
    pub pos: Vec3,
}

/// A local fight: the snapshot (ours first, then theirs, in the orders of
/// `ours` and `theirs`).
pub struct Local {
    pub snap: FightSnapshot,
    pub ours: Vec<Ours>,
    pub theirs: Vec<Theirs>,
    /// An unidentified mobile contact is in the fight: it cannot be judged.
    pub unknown: bool,
}

impl Local {
    pub fn armed_theirs(&self) -> bool {
        self.theirs.iter().any(|t| t.armed)
    }

    pub fn metal(&self) -> [f32; 2] {
        [self.ours.iter().map(|o| o.cost).sum(), self.theirs.iter().map(|t| t.cost).sum()]
    }

    /// The same fight without our units `ids`.
    pub fn without(&self, ids: &[i32]) -> Local {
        let keep: Vec<bool> = self.ours.iter().map(|o| !ids.contains(&o.id)).collect();
        let n = self.ours.len();
        let snap = FightSnapshot {
            flags: Default::default(),
            units: (self.snap.units.iter().enumerate())
                .filter(|(i, _)| *i >= n || keep[*i])
                .map(|(_, u)| u.clone())
                .collect(),
        };
        let ours = self.ours.iter().zip(&keep).filter(|(_, k)| **k).map(|(o, _)| o.clone()).collect();
        Local { snap, ours, theirs: self.theirs.clone(), unknown: self.unknown }
    }
}

/// A fates query's sums.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Tally {
    /// Our metal and its expected loss.
    pub own_cost: f32,
    pub own_loss: f32,
    /// Expected value of the enemies killed (weighted).
    pub kill_value: f32,
    /// Their armed metal, its expected share killed, and the least
    /// P(dead) of any of their armed units (1 without any).
    pub armed_cost: f32,
    pub armed_share: f32,
    pub min_armed_p: f32,
}

impl Tally {
    pub fn net(&self) -> f32 {
        self.kill_value - self.own_loss
    }
}

/// A unit's P(dead): the sim's own (`raw`, what decisions read) or the
/// calibrated one.
pub fn p(f: &UnitFate, raw: bool) -> f32 {
    if raw { f.p_raw } else { f.p_dead }
}

/// Sum a fates answer (in snapshot order) over the fight's sides.
pub fn tally(local: &Local, fates: &[UnitFate], raw: bool) -> Tally {
    let n = local.ours.len();
    let mut t = Tally { min_armed_p: 1.0, ..Default::default() };
    for (o, f) in local.ours.iter().zip(fates) {
        t.own_cost += o.cost;
        t.own_loss += o.cost * p(f, raw);
    }
    let mut armed_killed = 0.0;
    for (e, f) in local.theirs.iter().zip(&fates[n.min(fates.len())..]) {
        t.kill_value += e.value * p(f, raw);
        if e.armed {
            t.armed_cost += e.cost;
            armed_killed += e.cost * p(f, raw);
            t.min_armed_p = t.min_armed_p.min(p(f, raw));
        }
    }
    t.armed_share = if t.armed_cost > 0.0 { armed_killed / t.armed_cost } else { 1.0 };
    t
}

/// Each enemy's expected value killed (sim's own P(dead)), `fates` in
/// snapshot order.
pub fn kills(local: &Local, fates: &[UnitFate]) -> Vec<f32> {
    let n = local.ours.len();
    local.theirs.iter().zip(&fates[n.min(fates.len())..]).map(|(e, f)| e.value * f.p_raw).collect()
}

/// A time budget for synchronous queries in one tick.
pub struct Budget {
    start: Instant,
    limit_us: u128,
    pub queries: u32,
}

impl Budget {
    pub fn new(limit_us: u128) -> Self {
        Budget { start: Instant::now(), limit_us, queries: 0 }
    }

    pub fn spent(&self) -> bool {
        self.start.elapsed().as_micros() >= self.limit_us
    }

    pub fn micros(&self) -> u64 {
        self.start.elapsed().as_micros() as u64
    }
}

/// Our side's view of one of our units: (def, health as the sim counts it).
pub fn our_state(u: Unit) -> Option<(UnitDef, f32)> {
    let def = u.def()?;
    let hp = (u.health() - u.paralyze_damage()).max(0.0) * (1.0 - u.capture_progress());
    (u.health() > 0.0).then_some((def, hp))
}

impl Ai {
    /// The model for synchronous local judgements, loaded once.
    pub(crate) fn local_model(&mut self) -> Option<Arc<Model>> {
        if self.war.fightsim.local_model.is_none() {
            let m = Model::load().map(Arc::new);
            if let Err(e) = &m {
                self.log(&format!("fightsim: local judgements cannot load the predictor ({e})"));
            }
            self.war.fightsim.local_model = Some(m.ok());
        }
        self.war.fightsim.local_model.clone().flatten()
    }

    /// The local fight of our `units` (any of our units, ids from the
    /// engine): every identified enemy within [`CONTACT`] of one of them and
    /// every armed ground enemy within [`SUPPORT`] of their middle, as last
    /// seen (statics as known). `None` when one of ours is submerged or the
    /// fight is too big.
    pub(crate) fn local_fight(&mut self, units: &[Unit]) -> Option<Local> {
        let frame = self.frame;
        let mut ours: Vec<(Unit, UnitDef, f32)> = Vec::new();
        for &u in units {
            let Some((def, hp)) = our_state(u) else { continue };
            if u.pos().y < -5.0 {
                return None;
            }
            ours.push((u, def, hp));
        }
        if ours.is_empty() {
            return None;
        }
        let n = ours.len() as f32;
        let mid = Vec3::xz(
            ours.iter().map(|o| o.0.pos().x).sum::<f32>() / n,
            ours.iter().map(|o| o.0.pos().z).sum::<f32>() / n,
        );
        let positions: Vec<Vec3> = ours.iter().map(|o| o.0.pos()).collect();
        let near = |p: Vec3| positions.iter().any(|&q| distance(p, q) <= CONTACT);
        let mut unknown = false;
        let mut theirs: Vec<(i32, UnitDef, f32, Vec3, bool, String)> = Vec::new();
        for (&eid, e) in &self.war.targets {
            if e.is_dead || e.position.y < -5.0 || (!e.is_static && frame - e.last_seen > SEEN_WITHIN) {
                continue;
            }
            let contact = near(e.position);
            let armed = e.can_attack() && !e.is_aa;
            let joins = armed && !e.can_fly() && distance(e.position, mid) <= SUPPORT;
            if !contact && !joins {
                continue;
            }
            let Some(def) = e.ud.filter(|_| e.identified) else {
                unknown |= contact && !e.is_static;
                continue;
            };
            // Aircraft only over the fight itself.
            if e.can_fly() && !contact {
                continue;
            }
            theirs.push((eid, def, e.get_health() * e.def_health(), e.position, armed, e.def_name.clone()));
        }
        if ours.len() + theirs.len() > MAX_UNITS {
            return None;
        }
        let snap = FightSnapshot { flags: self.sim_flags(), ..Default::default() };
        let mut local = Local { snap, ours: Vec::new(), theirs: Vec::new(), unknown };
        for (u, def, hp) in ours {
            let spec = self.fight_spec_of(u.id(), def);
            let p = u.pos();
            local.ours.push(Ours { id: u.id(), cost: spec.cost });
            local.snap.units.push(SnapUnit { side: 0, spec, hp, x: p.x, z: p.z });
        }
        for (eid, def, hp, p, armed, name) in theirs {
            let spec = self.fight_spec_of(eid, def);
            let Some(e) = self.war.targets.get(&eid) else { continue };
            let (value, is_prize) = (spec.cost * weight(e), prize(e, spec.cost));
            let armed = armed && !spec.weapons.is_empty();
            local.theirs.push(Theirs { id: eid, def: name, cost: spec.cost, value, armed, prize: is_prize, pos: p });
            local.snap.units.push(SnapUnit { side: 1, spec, hp, x: p.x, z: p.z });
        }
        Some(local)
    }

    /// The fates of a local fight; `mode` as [`Model::fates`].
    pub(crate) fn local_fates(&mut self, local: &Local, mode: FateMode, budget: &mut Budget) -> Option<Vec<UnitFate>> {
        let model = self.local_model()?;
        budget.queries += 1;
        Some(model.fates(&local.snap, HORIZON, mode))
    }
}

/// Snapshot index of their unit `i` (into `theirs`).
pub fn their_index(local: &Local, i: usize) -> usize {
    local.ours.len() + i
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::military::fightsim::UnitSpec;

    fn fate(p: f32) -> UnitFate {
        UnitFate { p_dead: p, p_raw: p, ..Default::default() }
    }

    fn local(ours: &[f32], theirs: &[(f32, f32, bool)]) -> Local {
        let spec = Arc::new(UnitSpec::default());
        let mut l = Local { snap: FightSnapshot::default(), ours: Vec::new(), theirs: Vec::new(), unknown: false };
        for (i, &c) in ours.iter().enumerate() {
            l.ours.push(Ours { id: i as i32, cost: c });
            l.snap.units.push(SnapUnit { side: 0, spec: spec.clone(), hp: 1.0, x: 0.0, z: 0.0 });
        }
        for (i, &(c, v, armed)) in theirs.iter().enumerate() {
            l.theirs.push(Theirs {
                id: 100 + i as i32,
                def: String::new(),
                cost: c,
                value: v,
                armed,
                prize: false,
                pos: Vec3::xz(0.0, 0.0),
            });
            l.snap.units.push(SnapUnit { side: 1, spec: spec.clone(), hp: 1.0, x: 0.0, z: 0.0 });
        }
        l
    }

    #[test]
    fn tally_sums_each_side_in_snapshot_order() {
        let l = local(&[100.0, 50.0], &[(300.0, 450.0, false), (200.0, 200.0, true)]);
        let t = tally(&l, &[fate(0.5), fate(1.0), fate(0.2), fate(0.9)], true);
        assert_eq!(t.own_cost, 150.0);
        assert!((t.own_loss - 100.0).abs() < 1e-4);
        assert!((t.kill_value - (90.0 + 180.0)).abs() < 1e-3);
        assert_eq!(t.armed_cost, 200.0);
        assert!((t.armed_share - 0.9).abs() < 1e-6);
        assert!((t.min_armed_p - 0.9).abs() < 1e-6);
        let w = l.without(&[0]);
        assert_eq!(w.ours.len(), 1);
        assert_eq!(w.ours[0].id, 1);
        assert_eq!(w.snap.units.len(), 3);
        assert_eq!(their_index(&w, 0), 1);
    }

    #[test]
    fn no_armed_enemy_is_an_armed_share_of_one() {
        let l = local(&[100.0], &[(50.0, 75.0, false)]);
        let t = tally(&l, &[fate(0.0), fate(1.0)], false);
        assert_eq!(t.armed_share, 1.0);
        assert_eq!(t.min_armed_p, 1.0);
    }
}

/// What the fates say on textbook cases, printed (`cargo test probe_cases
/// -- --ignored --nocapture`): the gang and stand thresholds were set
/// against them.
#[cfg(test)]
mod probe {
    use super::*;
    use crate::military::fightsim::{UnitSpec, WeaponSpec};

    fn spec(name: &str) -> Arc<UnitSpec> {
        let row = include_str!("testdata/mech_parity_units.tsv")
            .lines()
            .find(|l| l.starts_with(&format!("{name}\t")))
            .unwrap();
        let f: Vec<_> = row.split('\t').collect();
        Arc::new(UnitSpec {
            def: name.into(),
            cost: f[1].parse().unwrap(),
            health: f[2].parse().unwrap(),
            speed: f[3].parse().unwrap(),
            mobile: f[4] == "1",
            radius: f[8].parse().unwrap(),
            weapons: f[9]
                .split('|')
                .filter(|w| !w.is_empty())
                .map(|w| {
                    let p: Vec<_> = w.split(':').collect();
                    WeaponSpec {
                        name: p[0].into(),
                        weapon_type: p[1].into(),
                        range: p[2].parse().unwrap(),
                        reload: p[3].parse().unwrap(),
                        salvo: p[4].parse().unwrap(),
                        damage: p[5].parse().unwrap(),
                    }
                })
                .collect(),
            ..Default::default()
        })
    }

    /// (side, def, count, x, hp share, worth weight)
    fn fight(units: &[(u8, &str, usize, f32, f32, f32)]) -> Local {
        let mut l = Local { snap: FightSnapshot::default(), ours: Vec::new(), theirs: Vec::new(), unknown: false };
        let mut id = 0;
        for &(side, name, n, x, hp, w) in units.iter().filter(|u| u.0 == 0).chain(units.iter().filter(|u| u.0 == 1)) {
            let s = spec(name);
            for i in 0..n {
                id += 1;
                let z = i as f32 * 40.0;
                if side == 0 {
                    l.ours.push(Ours { id, cost: s.cost });
                } else {
                    l.theirs.push(Theirs {
                        id,
                        def: name.into(),
                        cost: s.cost,
                        value: s.cost * w,
                        armed: !s.weapons.is_empty(),
                        prize: w >= 1.5 || s.cost >= 600.0,
                        pos: springai::Vec3::xz(x, z),
                    });
                }
                l.snap.units.push(SnapUnit { side, spec: s.clone(), hp: s.health * hp, x, z });
            }
        }
        l
    }

    fn show(label: &str, l: &Local, mode: FateMode) -> (Tally, Vec<UnitFate>) {
        let m = Model::load().unwrap();
        let f = m.fates(&l.snap, HORIZON, mode);
        let t = tally(l, &f, true);
        let ps: Vec<String> =
            f.iter().map(|x| format!("{:.2}/{:.2}{}", x.p_dead, x.p_raw, if x.dead { "x" } else { "" })).collect();
        println!(
            "{label}: own {:.0}/{:.0} kill {:.0} armed_share {:.2} min_p {:.2} | p {}",
            t.own_loss,
            t.own_cost,
            t.kill_value,
            t.armed_share,
            t.min_armed_p,
            ps.join(" ")
        );
        (t, f)
    }

    #[test]
    #[ignore = "prints the fates of textbook fights; run with --ignored --nocapture"]
    fn probe_cases() {
        for n in [2, 3, 4, 6, 8] {
            let l = fight(&[
                (0, "cloakraid", n, 0.0, 1.0, 1.0),
                (1, "turretlaser", 1, 500.0, 1.0, 1.0),
                (1, "staticmex", 2, 560.0, 1.0, 1.5),
            ]);
            show(&format!("{n} Glaives vs Lotus+2 mex"), &l, FateMode::AMove);
        }
        for n in [3, 4, 6] {
            let l = fight(&[
                (0, "vehraid", n, 0.0, 1.0, 1.0),
                (1, "turretlaser", 1, 500.0, 1.0, 1.0),
                (1, "staticmex", 2, 560.0, 1.0, 1.5),
            ]);
            show(&format!("{n} Scorchers vs Lotus+2 mex"), &l, FateMode::AMove);
        }
        let l = fight(&[
            (0, "cloakraid", 2, 0.0, 1.0, 1.0),
            (0, "shieldraid", 1, 0.0, 1.0, 1.0),
            (1, "cloakriot", 1, 450.0, 1.0, 1.0),
            (1, "cloakcon", 1, 500.0, 1.0, 1.3),
        ]);
        show("2 Glaive+Bandit vs Reaver+con", &l, FateMode::AMove);
        for n in [4, 6, 8] {
            let l = fight(&[(0, "vehraid", n, 0.0, 1.0, 1.0), (1, "tankassault", 1, 500.0, 1.0, 1.0)]);
            let (_, f) = show(&format!("{n} Scorchers vs Reaper (a-move)"), &l, FateMode::AMove);
            let _ = f;
            let k = l.ours.len();
            show(&format!("{n} Scorchers vs Reaper (dive)"), &l, FateMode::Assassinate { target: k });
        }
        for n in [4, 6, 8] {
            let l = fight(&[
                (0, "tankraid", n, 0.0, 1.0, 1.0),
                (1, "amphassault", 1, 500.0, 0.4, 1.0),
                (1, "cloakriot", 1, 550.0, 1.0, 1.0),
            ]);
            let k = l.ours.len();
            show(&format!("{n} Kodachis vs Grizzly@40%+Reaver (dive)"), &l, FateMode::Assassinate { target: k });
        }
        // stayveto, groups: everyone in the fight hurt below the retreat line.
        for (us, n, heavy, hp_heavy, hp_us) in [
            ("vehassault", 3, "tankassault", 0.2, 0.45),
            ("vehassault", 3, "tankassault", 0.6, 0.45),
            ("cloakassault", 3, "tankriot", 0.25, 0.45),
            ("cloakraid", 5, "cloakriot", 0.3, 0.45),
            ("hoverassault", 3, "tankassault", 0.3, 0.6),
        ] {
            let l = fight(&[(0, us, n, 0.0, hp_us, 1.0), (1, heavy, 1, 400.0, hp_heavy, 1.0)]);
            let (ts, _) = show(&format!("group stay: {n} {us}@{hp_us} vs {heavy}@{hp_heavy}"), &l, FateMode::AMove);
            let ids: Vec<i32> = l.ours.iter().map(|o| o.id).collect();
            let w = l.without(&ids);
            let (tl, _) = show("  leave", &w, FateMode::AMove);
            println!("  gain {:.0} loss {:.0}", ts.kill_value - tl.kill_value, ts.own_loss - tl.own_loss);
        }
        // stayveto: a hurt Ravager beside two healthy ones, vs a Reaper at 20%.
        for (hp_heavy, hp_us) in [(0.2, 0.45), (0.5, 0.45), (0.2, 0.2)] {
            let l = fight(&[
                (0, "vehassault", 1, 0.0, hp_us, 1.0),
                (0, "vehassault", 2, 0.0, 1.0, 1.0),
                (1, "tankassault", 1, 400.0, hp_heavy, 1.0),
            ]);
            let (ts, _) = show(&format!("stay: Ravager@{hp_us} +2 vs Reaper@{hp_heavy}"), &l, FateMode::AMove);
            let w = l.without(&[1]);
            let (tl, _) = show("  leave", &w, FateMode::AMove);
            println!("  gain {:.0} loss {:.0}", ts.kill_value - tl.kill_value, ts.own_loss - tl.own_loss);
        }
        for (hp_heavy, hp_us) in [(0.15, 0.4), (0.6, 0.4)] {
            let l = fight(&[
                (0, "cloakassault", 1, 0.0, hp_us, 1.0),
                (0, "cloakassault", 1, 0.0, 1.0, 1.0),
                (1, "tankassault", 1, 400.0, hp_heavy, 1.0),
                (1, "tankriot", 1, 450.0, 1.0, 1.0),
            ]);
            let (ts, _) =
                show(&format!("stay: Knight@{hp_us} + Knight vs Reaper@{hp_heavy}+Ogre"), &l, FateMode::AMove);
            let w = l.without(&[1]);
            let (tl, _) = show("  leave", &w, FateMode::AMove);
            println!("  gain {:.0} loss {:.0}", ts.kill_value - tl.kill_value, ts.own_loss - tl.own_loss);
        }
    }
}
