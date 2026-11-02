//! Conquest escorts (`conquestescort`, off by default): while the commander
//! takes an objective, a team of the QRF's stands on it, sized by the fight
//! predictor to beat the raid expected there.
//!
//! Why: in the conquest batteries (zkgbai-runs/conquest, b2-b4, 117 games
//! where the turrets go up late) the first of our things lost at an
//! objective, a mex in 59 of 68 cases, fell a median 2:45 after the
//! commander set out, to 2-5 raiders, before any turret stood there in 85
//! games; the earlier that came, the worse the game went (win 0.33 within
//! two minutes, 0.68 never). The few of our armed units that happened to be
//! near did not delay it.
//!
//! The raid expected is [`RAID_COUNT`] of the raider the enemy has shown
//! most, else of the one its likeliest factory tier makes. The escort is
//! the cheapest set of the recruit-class units our factories make that the
//! predictor gives P(win) of at least [`WIN`] at an expected loss of at most
//! [`LOSS`] of its metal, picked one at a time by the margin each adds per
//! metal, at most [`MAX_METAL`] and [`MAX_UNITS`] (short of that, the best
//! it found). It is sized again every [`RESIZE`] as intel comes in.
//!
//! Its members are the first recruit-class units out of the factories while
//! it wants more (before the floor's guards and the budget's teams); short
//! for [`SHORT_FOR`], the factories build its units in place of plain
//! fighters. The team holds a zone on the objective ([`RADIUS`]) and plays
//! the mark algorithm (goalkeep, strike when clearly stronger). It is let
//! go when the objective ends.

use std::sync::{Arc, OnceLock};

use serde_json::json;
use springai::{Unit, Vec3};
use zk_terrain::Tier;

use super::Member;
use super::field::Mobility;
use super::live::{ANTI_RAIDERS, RAIDERS, Role, RoleKind, Tactic, is_recruit};
use crate::Ai;
use crate::military::Fighter;
use crate::military::fightsim::plan::{Caps, cheapest_force};
use crate::military::fightsim::{Model, SimFlags, UnitSpec};

/// Raiders in the raid an escort is sized against.
pub const RAID_COUNT: usize = 4;
/// The escort wants at least this chance to win...
pub const WIN: f32 = 0.8;
/// ... losing at most this share of its metal (expected).
pub const LOSS: f32 = 0.5;
/// An escort costs at most this much metal, and has at most this many units.
pub const MAX_METAL: f32 = 600.0;
pub const MAX_UNITS: usize = 8;
/// The zone the escort answers raids in, around the objective (elmos).
const RADIUS: f32 = 900.0;
/// The escort is sized again this often (frames).
const RESIZE: i32 = 30 * 15;
/// Short of its want this long, the factories build for it (frames).
const SHORT_FOR: i32 = 30 * 20;

/// The raider each movement tier's factories make, for an enemy not yet
/// seen: the sturdier one where a tier has two (Bandits, not Glaives).
fn tier_raider(t: Tier) -> &'static str {
    match t {
        Tier::Bot => "shieldraid",
        Tier::Vehicle => "vehraid",
        Tier::Hover => "hoverraid",
        Tier::Spider => "spideremp",
        Tier::Amphib => "amphraid",
        _ => "shieldraid",
    }
}

#[derive(Default)]
pub struct Escorts {
    pub cur: Option<Escort>,
    pub opened: u32,
    pub recruited: u32,
    pub ordered: u32,
}

pub struct Escort {
    /// The QRF team (its role is a zone on the objective).
    pub team: usize,
    pub center: Vec3,
    /// What the predictor asked for, by unit name, and what is still wanted.
    pub sized_for: Vec<String>,
    pub want: Vec<String>,
    /// The raid it was sized against: (unit name, count).
    pub raid: (String, usize),
    pub p_win: f32,
    pub loss: f32,
    pub since: i32,
    pub sized: i32,
    pub short_since: Option<i32>,
    /// Ordered from a factory and not yet out: (frame, unit name).
    pub pending: Vec<(i32, String)>,
}

fn model() -> Option<&'static Model> {
    static MODEL: OnceLock<Option<Model>> = OnceLock::new();
    MODEL.get_or_init(|| Model::load().ok()).as_ref()
}

/// The cheapest escort from `options` that beats `raid` comfortably (see
/// the module notes and `fightsim::plan`): the picks (indices into
/// `options`, repeats allowed), the predicted P(win) and the expected share
/// of the escort's metal lost.
pub fn size_escort(
    model: &Model,
    raid: &[Arc<UnitSpec>],
    options: &[Arc<UnitSpec>],
    flags: SimFlags,
) -> (Vec<usize>, f32, f32) {
    if raid.is_empty() || options.is_empty() {
        return (Vec::new(), 1.0, 0.0);
    }
    let against: Vec<_> = raid.iter().enumerate().map(|(i, s)| (s.clone(), 0.0, 24.0 * i as f32)).collect();
    let caps = Caps { win: WIN, loss: LOSS, metal: MAX_METAL, units: MAX_UNITS };
    let plan = cheapest_force(model, &against, options, flags, &caps);
    (plan.pick, plan.p_win, plan.loss)
}

impl Ai {
    /// The raid an escort is sized against: the raider the enemy has shown
    /// most, else the one its likeliest factory tier makes.
    fn escort_raid(&self) -> Option<(String, usize)> {
        let mut seen: std::collections::BTreeMap<String, usize> = std::collections::BTreeMap::new();
        for e in self.war.targets.values() {
            if e.is_dead || !e.identified || !e.is_raider || e.is_static {
                continue;
            }
            if let Some(d) = e.ud {
                *seen.entry(self.defs.name(d).to_owned()).or_default() += 1;
            }
        }
        if let Some((name, _)) = seen.into_iter().max_by_key(|(_, n)| *n) {
            return Some((name, RAID_COUNT));
        }
        let belief = self.conquest_belief();
        let tier = (belief.first()?.1.iter()).max_by(|a, b| a.1.total_cmp(&b.1)).map_or(Tier::Bot, |(t, _)| *t);
        Some((tier_raider(tier).to_owned(), RAID_COUNT))
    }

    /// The recruit-class units our factories can make now.
    fn escort_options(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for f in self.fac.factories.values() {
            let Some(def) = f.unit.def() else { continue };
            for d in def.build_options() {
                let name = self.defs.name(d);
                if is_recruit(name) && Mobility::of(d).is_some() && !out.iter().any(|n| n == name) {
                    out.push(name.to_owned());
                }
            }
        }
        out
    }

    /// Size an escort now: (unit names, P(win), expected loss share, raid).
    fn escort_size(&mut self) -> Option<(Vec<String>, f32, f32, (String, usize))> {
        let model = model()?;
        let raid = self.escort_raid()?;
        let raid_def = self.defs.get(&raid.0)?;
        let raid_spec = self.fight_spec(raid_def);
        let raid_specs: Vec<Arc<UnitSpec>> = (0..raid.1).map(|_| raid_spec.clone()).collect();
        let names = self.escort_options();
        let mut specs = Vec::new();
        let mut kept = Vec::new();
        for n in names {
            if let Some(d) = self.defs.get(&n) {
                specs.push(self.fight_spec(d));
                kept.push(n);
            }
        }
        if specs.is_empty() {
            return None;
        }
        let (pick, p, loss) = size_escort(model, &raid_specs, &specs, self.sim_flags());
        Some((pick.into_iter().map(|k| kept[k].clone()).collect(), p, loss, raid))
    }

    /// The commander sets out for an objective centred at `center`: open
    /// its escort.
    pub(crate) fn escort_open(&mut self, center: Vec3) {
        if self.dev_off("conquestescort") || !self.qrf.on || !self.qrf.live {
            return;
        }
        if self.qrf.escort.cur.is_some() {
            self.escort_close("a new objective");
        }
        let frame = self.frame;
        let Some((want, p_win, loss, raid)) = self.escort_size() else {
            self.log("escort: no escort (nothing to size it with)");
            return;
        };
        let team = self.qrf.next_team;
        self.qrf.next_team += 1;
        self.qrf.roles.insert(team, Role { kind: RoleKind::Zone, pos: center, radius: RADIUS, tactic: Tactic::Mark });
        self.qrf.escort.opened += 1;
        self.log(&format!(
            "escort: team {team} for the objective at ({:.0},{:.0}): {} against {}x {} (P(win) {p_win:.2}, loss {loss:.2})",
            center.x,
            center.z,
            want.join(" "),
            raid.1,
            raid.0
        ));
        self.op_emit(
            "conquest",
            json!({"event": "escort", "what": "open", "t": frame / 30, "team": team, "want": want,
                   "raid": raid.0, "raid_n": raid.1, "p": p_win, "loss": loss}),
        );
        self.qrf.escort.cur = Some(Escort {
            team,
            center,
            sized_for: want.clone(),
            want,
            raid,
            p_win,
            loss,
            since: frame,
            sized: frame,
            short_since: Some(frame),
            pending: Vec::new(),
        });
    }

    /// The objective ended: let the escort go.
    pub(crate) fn escort_close(&mut self, why: &str) {
        let Some(e) = self.qrf.escort.cur.take() else { return };
        let n = self.qrf_release_team(e.team, &format!("escort: {why}"));
        self.op_emit(
            "conquest",
            json!({"event": "escort", "what": "close", "t": self.frame / 30, "team": e.team, "why": why,
                   "members": n, "secs": (self.frame - e.since) / 30, "unfilled": e.want}),
        );
    }

    /// Members the escort has now.
    fn escort_members(&self, team: usize) -> Vec<&Member> {
        self.qrf.members.values().filter(|m| m.team == team && m.squad.is_none()).collect()
    }

    /// Size again as intel comes in (what it has stays; the want is what
    /// the new size asks beyond it).
    pub(crate) fn escort_update(&mut self) {
        let frame = self.frame;
        let Some(e) = self.qrf.escort.cur.as_ref() else { return };
        if frame - e.sized < RESIZE {
            return;
        }
        let team = e.team;
        let Some((sized_for, p_win, loss, raid)) = self.escort_size() else { return };
        let mut have: Vec<String> =
            self.escort_members(team).iter().map(|m| self.defs.name(m.def).to_owned()).collect();
        let pending: Vec<String> =
            (self.qrf.escort.cur.as_ref()).map(|e| e.pending.iter().map(|p| p.1.clone()).collect()).unwrap_or_default();
        have.extend(pending);
        let mut want = Vec::new();
        for n in &sized_for {
            if let Some(i) = have.iter().position(|h| h == n) {
                have.remove(i);
            } else {
                want.push(n.clone());
            }
        }
        let e = self.qrf.escort.cur.as_mut().expect("checked above");
        let changed = e.raid != raid || e.sized_for != sized_for;
        (e.sized_for, e.want, e.p_win, e.loss, e.raid, e.sized) = (sized_for, want, p_win, loss, raid, frame);
        if e.want.is_empty() {
            e.short_since = None;
        } else if e.short_since.is_none() {
            e.short_since = Some(frame);
        }
        if changed {
            let line = format!(
                "escort: team {} sized again: {} against {}x {} (P(win) {:.2}), still wants {}",
                e.team,
                e.sized_for.join(" "),
                e.raid.1,
                e.raid.0,
                e.p_win,
                e.want.join(" ")
            );
            self.log(&line);
        }
    }

    /// Take a unit fresh from the factory into the escort if it is one the
    /// escort wants.
    pub(crate) fn escort_take(&mut self, unit: Unit) -> bool {
        let Some(def) = unit.def() else { return false };
        let name = self.defs.name(def).to_owned();
        let Some(e) = self.qrf.escort.cur.as_mut() else { return false };
        let Some(i) = e.want.iter().position(|w| *w == name) else { return false };
        let Some(mobility) = Mobility::of(def) else { return false };
        e.want.remove(i);
        if let Some(k) = e.pending.iter().position(|p| p.1 == name) {
            e.pending.remove(k);
        }
        if e.want.is_empty() {
            e.short_since = None;
        }
        let team = e.team;
        let cost = def.cost(self.metal);
        let threat = Fighter::new(unit, cost, self.team_id).power;
        self.qrf.members.insert(
            unit.id(),
            Member {
                unit,
                def,
                team,
                cost,
                speed: def.speed(),
                mobility,
                range: def.max_weapon_range(),
                squad: None,
                threat,
                order: None,
            },
        );
        self.qrf.escort.recruited += 1;
        unit.set_move_state(1, 0);
        unit.set_fire_state(2, 0);
        true
    }

    /// Whether the escort wants `unit` (fresh from the factory) now.
    pub(crate) fn escort_wants(&self, unit: Unit) -> bool {
        let q = &self.qrf;
        if !q.on || !q.live || q.releasing {
            return false;
        }
        let Some(e) = q.escort.cur.as_ref() else { return false };
        unit.def().is_some_and(|d| e.want.iter().any(|w| w == self.defs.name(d)))
    }

    /// A unit for the escort from a factory about to build `choice`, a
    /// plain fighter (not anti-air, not a recruit already), if the escort
    /// has been short a while and its orders do not make it up.
    pub(crate) fn escort_production(&mut self, factory: Unit, choice: &str) -> Option<&'static str> {
        let frame = self.frame;
        let e = self.qrf.escort.cur.as_ref()?;
        if e.short_since.is_none_or(|s| frame - s < SHORT_FOR) || is_recruit(choice) {
            return None;
        }
        let def = self.defs.get(choice)?;
        if self.unit_classes.aas.contains(choice) || def.is_able_to_fly() || def.speed() <= 0.0 {
            return None;
        }
        let options = factory.def()?.build_options();
        let mut still = e.want.clone();
        for p in &e.pending {
            if let Some(i) = still.iter().position(|w| *w == p.1) {
                still.remove(i);
            }
        }
        let name = still.into_iter().find(|w| options.iter().any(|d| self.defs.name(*d) == w.as_str()))?;
        let name: &'static str = ANTI_RAIDERS.iter().chain(&RAIDERS).copied().find(|n| *n == name)?;
        let e = self.qrf.escort.cur.as_mut().expect("checked above");
        // Orders not out in a minute are forgotten (the factory switched).
        e.pending.retain(|p| frame - p.0 < 30 * 60);
        e.pending.push((frame, name.to_owned()));
        self.qrf.escort.ordered += 1;
        Some(name)
    }

    pub(crate) fn escort_json(&self) -> serde_json::Value {
        let q = &self.qrf.escort;
        let cur = q.cur.as_ref().map(|e| {
            json!({"team": e.team, "x": e.center.x, "z": e.center.z, "sized_for": e.sized_for, "want": e.want,
                   "raid": e.raid.0, "raid_n": e.raid.1, "p": e.p_win, "loss": e.loss,
                   "members": self.escort_members(e.team).len(), "since": e.since / 30})
        });
        json!({"cur": cur, "opened": q.opened, "recruited": q.recruited, "ordered": q.ordered})
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::military::fightsim::WeaponSpec;

    fn spec(def: &str, cost: f32, health: f32, speed: f32, weapon: (&str, f32, f32, f32)) -> Arc<UnitSpec> {
        Arc::new(UnitSpec {
            def: def.into(),
            cost,
            speed,
            mobile: true,
            weapons: vec![WeaponSpec {
                name: weapon.0.into(),
                range: weapon.1,
                reload: weapon.2,
                salvo: if weapon.0 == "cloakraid_emg" { 3 } else { 1 },
                damage: weapon.3,
                weapon_type: if weapon.0 == "vehraid_heatray" { "LaserCannon" } else { "Cannon" }.into(),
            }],
            health,
            air: false,
            builder: false,
            com: false,
            radius: 12.0,
        })
    }

    fn glaive() -> Arc<UnitSpec> {
        spec("cloakraid", 65.0, 230.0, 3.85, ("cloakraid_emg", 185.0, 0.3, 10.5))
    }

    fn scorcher() -> Arc<UnitSpec> {
        spec("vehraid", 130.0, 480.0, 3.7, ("vehraid_heatray", 262.0, 0.1, 30.0))
    }

    #[test]
    fn a_bigger_raid_asks_for_a_bigger_escort() {
        let Some(model) = model() else { return };
        let options = [glaive(), scorcher()];
        let (two, _, _) = size_escort(model, &[scorcher(), scorcher()], &options, SimFlags::default());
        let (four, _, _) =
            size_escort(model, &[scorcher(), scorcher(), scorcher(), scorcher()], &options, SimFlags::default());
        let cost = |p: &[usize]| p.iter().map(|&k| options[k].cost).sum::<f32>();
        assert!(!two.is_empty());
        assert!(cost(&four) >= cost(&two), "{two:?} {four:?}");
    }

    #[test]
    fn the_escort_stays_within_its_caps() {
        let Some(model) = model() else { return };
        let options = [glaive()];
        let raid: Vec<_> = (0..12).map(|_| scorcher()).collect();
        let (pick, _, _) = size_escort(model, &raid, &options, SimFlags::default());
        assert!(pick.len() <= MAX_UNITS);
        assert!(pick.iter().map(|&k| options[k].cost).sum::<f32>() <= MAX_METAL);
    }

    #[test]
    fn no_raid_no_escort() {
        let Some(model) = model() else { return };
        let (pick, p, _) = size_escort(model, &[], &[glaive()], SimFlags::default());
        assert!(pick.is_empty());
        assert_eq!(p, 1.0);
    }
}
