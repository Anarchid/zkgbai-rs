//! Small-force assignment by predicted friendly losses, including the economy.
//! Every raid is evaluated with every eligible team subset (including none),
//! then a disjoint-subset search chooses a plan. One bounded background job;
//! stale or changed encounters keep their orders while the next job completes.
use std::collections::BTreeMap;
use std::sync::{Arc, OnceLock};

use serde_json::json;
use springai::Unit;

use super::{field::Field, mark::Target};
use crate::Ai;
use crate::kgbutil::worker::Worker;
use crate::military::fightsim::{FightSnapshot, Model, SnapUnit, mech::Defense};
use crate::util::distance;

pub(super) const MAX_TEAMS: usize = 4;
pub(super) const MAX_RAIDS: usize = 4;
const HORIZON: i32 = 30 * 30;
const MAX_UNITS: usize = 32;
const STALE: i32 = 45;
const KEEP: f32 = 5.0; // metal: a small tie preference, not a commitment lock

#[derive(Clone, Debug)]
struct Observed {
    id: i32,
    unit: SnapUnit,
    paralysis: f32,
}

#[derive(Clone)]
struct Raid {
    enemies: Vec<Observed>,
    assets: Vec<Observed>,
    weights: Vec<f32>,
    // One candidate force per team, with the extra terrain travel delay.
    defenders: Vec<Vec<(Observed, i32)>>,
    allowed: usize,
    kept: usize,
    // Observed asset the raider was shooting, by unit id.
    focus: Vec<i32>,
}

#[derive(Clone)]
struct Request {
    frame: i32,
    teams: Vec<usize>,
    raids: Vec<Raid>,
}

impl Request {
    fn reusable(&self, now: &Self) -> bool {
        fn same(a: &Observed, b: &Observed) -> bool {
            a.id == b.id
                && a.unit.spec.def == b.unit.spec.def
                && (a.unit.hp - b.unit.hp).abs() <= (a.unit.spec.health * 0.15).max(10.0)
                && (a.paralysis - b.paralysis).abs() <= 0.1
                && (a.unit.x - b.unit.x).hypot(a.unit.z - b.unit.z) <= 64.0 + a.unit.spec.speed * 30.0
        }
        (0..=STALE).contains(&(now.frame - self.frame))
            && self.teams == now.teams
            && self.raids.len() == now.raids.len()
            && self.raids.iter().zip(&now.raids).all(|(a, b)| {
                a.allowed == b.allowed
                    && a.focus == b.focus
                    && a.weights == b.weights
                    && a.enemies.len() == b.enemies.len()
                    && a.assets.len() == b.assets.len()
                    && a.enemies.iter().zip(&b.enemies).all(|(a, b)| same(a, b))
                    && a.assets.iter().zip(&b.assets).all(|(a, b)| same(a, b))
                    && a.defenders.len() == b.defenders.len()
                    && a.defenders.iter().zip(&b.defenders).all(|(a, b)| {
                        a.len() == b.len()
                            && a.iter().zip(b).all(|((a, da), (b, db))| same(a, b) && (da - db).abs() <= 30)
                    })
            })
    }
}

#[derive(Clone, Debug)]
struct Decision {
    masks: Vec<usize>,
    loss: f32,
    undefended: f32,
    simulations: usize,
}

type Answer = (Request, Result<Decision, String>);
#[derive(Default)]
pub(super) struct Predictor {
    worker: Option<Worker<Request, Answer>>,
    cached: Option<(Request, Decision, u128)>,
    failed: bool,
    last_log: i32,
    last_plan: Vec<(usize, Vec<i32>)>,
}

pub(super) enum Plan {
    Legacy,
    Pending,
    Ready(Vec<(usize, usize)>),
}

/// Enumerate disjoint subsets, not one mandatory assignee per raid. Unused
/// defenders and entirely unanswered raids are both legal alternatives.
fn choose(costs: &[Vec<f32>], teams: usize) -> (f32, Vec<usize>) {
    fn visit(r: usize, used: usize, sum: f32, costs: &[Vec<f32>], cur: &mut Vec<usize>, best: &mut (f32, Vec<usize>)) {
        if r == costs.len() {
            if sum < best.0 {
                *best = (sum, cur.clone());
            }
            return;
        }
        for (mask, &cost) in costs[r].iter().enumerate() {
            if used & mask != 0 || !cost.is_finite() {
                continue;
            }
            cur.push(mask);
            visit(r + 1, used | mask, sum + cost, costs, cur, best);
            cur.pop();
        }
    }
    assert!(teams <= MAX_TEAMS && costs.len() <= MAX_RAIDS);
    assert!(costs.iter().all(|c| c.len() == 1 << teams));
    let mut best = (f32::INFINITY, Vec::new());
    visit(0, 0, 0.0, costs, &mut Vec::new(), &mut best);
    best
}

fn losses(model: &Model, raid: &Raid, mask: usize) -> f32 {
    let mut snap = FightSnapshot::default();
    let mut defense = Defense::default();
    let mut paralysis = Vec::new();
    let mut weights = Vec::new();
    for (i, a) in raid.assets.iter().enumerate() {
        defense.assets.push(snap.units.len());
        snap.units.push(a.unit.clone());
        defense.arrivals.push(0);
        defense.focus.push(None);
        weights.push(raid.weights[i]);
        paralysis.push((i, a.paralysis));
    }
    for (i, enemy) in raid.enemies.iter().enumerate() {
        let index = snap.units.len();
        snap.units.push(enemy.unit.clone());
        defense.arrivals.push(0);
        defense.focus.push(raid.assets.iter().position(|a| a.id == raid.focus[i]));
        weights.push(0.0);
        paralysis.push((index, enemy.paralysis));
    }
    for (team, members) in raid.defenders.iter().enumerate() {
        if mask & (1 << team) == 0 {
            continue;
        }
        for (member, delay) in members {
            let index = snap.units.len();
            snap.units.push(member.unit.clone());
            defense.arrivals.push(*delay);
            defense.focus.push(None);
            weights.push(1.0);
            paralysis.push((index, member.paralysis));
        }
    }
    let fates = model.defense_fates(&snap, HORIZON, &defense, &paralysis);
    snap.units
        .iter()
        .zip(&fates)
        .zip(weights)
        .map(|((u, f), w)| {
            // Calibration alone must not charge for untouched or absent units.
            let damage = ((u.hp - f.hp_left) / u.hp.max(1.0)).clamp(0.0, 1.0);
            let death = if damage > 0.0 { f.p_dead } else { 0.0 };
            w * u.spec.cost * (death + 0.25 * damage * (1.0 - death))
        })
        .sum()
}

fn compute(request: Request) -> Answer {
    static MODEL: OnceLock<Result<Model, String>> = OnceLock::new();
    let result = (|| {
        let model = MODEL.get_or_init(Model::load).as_ref().map_err(Clone::clone)?;
        let mut simulations = 0;
        let costs: Vec<Vec<f32>> = request
            .raids
            .iter()
            .map(|r| {
                (0..1usize << request.teams.len())
                    .map(|mask| {
                        if mask & !r.allowed != 0 {
                            return f32::INFINITY;
                        }
                        simulations += 1;
                        losses(model, r, mask)
                    })
                    .collect()
            })
            .collect();
        let ranked: Vec<Vec<f32>> = costs
            .iter()
            .zip(&request.raids)
            .map(|(c, r)| {
                c.iter()
                    .enumerate()
                    .map(|(mask, &loss)| {
                        // Travel is only a tie-breaker. It never substitutes for
                        // simulating a late defender while assets take damage.
                        loss - KEEP * (mask & r.kept).count_ones() as f32 + 0.01 * mask.count_ones() as f32
                    })
                    .collect()
            })
            .collect();
        let (_, masks) = choose(&ranked, request.teams.len());
        let loss = costs.iter().zip(&masks).map(|(c, &m)| c[m]).sum();
        let undefended = costs.iter().map(|c| c[0]).sum();
        Ok(Decision { masks, loss, undefended, simulations })
    })();
    (request, result)
}

impl Ai {
    fn qrf_observed(&mut self, u: Unit) -> Option<Observed> {
        let def = u.def()?;
        let p = u.pos();
        Some(Observed {
            id: u.id(),
            paralysis: u.paralyze_damage() / u.max_health().max(1.0),
            unit: SnapUnit { side: 0, spec: self.fight_spec(def), hp: u.health(), x: p.x, z: p.z },
        })
    }

    pub(super) fn qrf_predict_plan(
        &mut self,
        teams: &BTreeMap<usize, Vec<i32>>,
        team_ids: &[usize],
        targets: &[Target],
        fields: &[Option<Arc<Field>>],
        allowed: &[Vec<bool>],
    ) -> Plan {
        if self.dev_off("qrfpredict")
            || targets.is_empty()
            || teams.len() > MAX_TEAMS
            || targets.len() > MAX_RAIDS
            || self.qrf.predict.failed
        {
            return Plan::Legacy;
        }
        let mut defenders = Vec::new();
        for &team in team_ids {
            let units: Vec<_> = teams[&team].iter().filter_map(|id| self.qrf.members.get(id).map(|m| m.unit)).collect();
            let Some(ms) = units.into_iter().map(|u| self.qrf_observed(u)).collect::<Option<Vec<_>>>() else {
                return Plan::Legacy;
            };
            defenders.push(ms);
        }
        let mut raids = Vec::new();
        for (ri, target) in targets.iter().enumerate() {
            let mut enemies = Vec::new();
            let mut focus = Vec::new();
            for &gi in &target.groups {
                for &id in &self.qrf.groups[gi].members.clone() {
                    let Some(t) = self.qrf.tracks.get(&id) else { return Plan::Legacy };
                    let Some(def) = t.kind() else { return Plan::Legacy };
                    let (pos, hp, pressed_on) = (t.pos, t.health, t.pressed_on);
                    let para = if t.in_los { t.unit.paralyze_damage() / def.health().max(1.0) } else { 0.0 };
                    let spec = self.fight_spec(def);
                    enemies.push(Observed {
                        id,
                        paralysis: para,
                        unit: SnapUnit { side: 1, hp: hp.max(1.0), x: pos.x, z: pos.z, spec },
                    });
                    focus.push(pressed_on);
                }
            }
            let mut threatened = target.threatened.clone();
            threatened.sort_by(|&(a, ta), &(b, tb)| {
                let pressed = |i: usize| focus.contains(&self.qrf.assets[i].unit.id());
                pressed(b).cmp(&pressed(a)).then_with(|| ta.total_cmp(&tb))
            });
            // Bound the local fight; include the actual victim before nearby
            // alternatives. The next assets matter when the first mex dies.
            threatened.truncate(4);
            let mut assets = Vec::new();
            let mut weights = Vec::new();
            for (index, _) in threatened {
                let Some(a) = self.qrf_observed(self.qrf.assets[index].unit) else { return Plan::Legacy };
                // Several raids can threaten the same asset. Do not count
                // its entire value once per raid in the additive objective.
                let overlaps = targets.iter().filter(|t| t.threatened.iter().any(|&(j, _)| j == index)).count();
                weights.push(1.0 / overlaps.max(1) as f32);
                assets.push(a);
            }
            if assets.is_empty() {
                return Plan::Legacy;
            }
            let mut candidates = Vec::new();
            let mut allowed_mask = 0;
            let mut kept = 0;
            for (i, &team) in team_ids.iter().enumerate() {
                let (pos, speed) = self.qrf_team_pos(&teams[&team]);
                let path = fields[i].as_ref().map_or(f32::INFINITY, |f| f.at(target.center));
                let direct = distance(pos, target.center) / speed.max(1.0);
                let delay = ((path - direct).max(0.0) * 30.0).min(HORIZON as f32) as i32;
                if path.is_finite() && allowed[i][ri] {
                    allowed_mask |= 1 << i;
                }
                if self.qrf.mark.marks.get(&team).is_some_and(|ids| enemies.iter().any(|e| ids.contains(&e.id))) {
                    kept |= 1 << i;
                }
                candidates.push(defenders[i].iter().cloned().map(|m| (m, delay)).collect());
            }
            if enemies.len() + assets.len() + defenders.iter().map(Vec::len).sum::<usize>() > MAX_UNITS {
                return Plan::Legacy;
            }
            raids.push(Raid { enemies, assets, weights, defenders: candidates, allowed: allowed_mask, kept, focus });
        }
        let request = Request { frame: self.frame, teams: team_ids.to_vec(), raids };
        let state = &mut self.qrf.predict;
        let worker = state.worker.get_or_insert_with(|| Worker::new("qrf-predict", compute));
        if let Some((result, elapsed)) = worker.poll() {
            match result {
                Some((old, Ok(decision))) => state.cached = Some((old, decision, elapsed.as_micros())),
                _ => {
                    state.failed = true;
                    self.log("qrfpredict: worker failed; using legacy assignments");
                    return Plan::Legacy;
                }
            }
        }
        let ready = state.cached.as_ref().is_some_and(|(old, _, _)| old.reusable(&request));
        if !worker.busy() {
            worker.submit(request.clone());
        }
        if !ready {
            self.qrf.plan_pending = true;
            return Plan::Pending;
        }
        let (_, decision, micros) = state.cached.as_ref().unwrap();
        let mut plan = Vec::new();
        let mut named = Vec::new();
        for (ri, &mask) in decision.masks.iter().enumerate() {
            for (i, &team) in team_ids.iter().enumerate() {
                if mask & (1 << i) != 0 {
                    plan.push((team, ri));
                    named.push((team, request.raids[ri].enemies.iter().map(|e| e.id).collect::<Vec<_>>()));
                }
            }
        }
        if named != state.last_plan || self.frame - state.last_log >= 150 {
            state.last_plan = named.clone();
            state.last_log = self.frame;
            let data = json!({"event":"plan", "teams":named, "loss":decision.loss,
                "undefended":decision.undefended, "simulations":decision.simulations, "micros":micros,
                "age":self.frame-state.cached.as_ref().unwrap().0.frame});
            self.op_emit("qrf-predict", data);
        }
        Plan::Ready(plan)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn can_keep_both_defenders_on_the_raid_that_would_cost_more() {
        let costs = vec![vec![255.0, 200.0, 150.0, 10.0], vec![60.0, 5.0, 5.0, 0.0]];
        let (loss, masks) = choose(&costs, 2);
        assert_eq!(masks, vec![3, 0]);
        assert_eq!(loss, 70.0);
    }
    #[test]
    fn nearby_counter_stays_and_distant_team_covers_other_raid() {
        let (_, masks) = choose(&[vec![255.0, 5.0, 240.0, 4.0], vec![85.0, 5.0, 20.0, 0.0]], 2);
        assert_eq!(masks, vec![1, 2]);
    }
    #[test]
    fn expensive_suicide_and_ineligible_subsets_are_not_required() {
        let (_, masks) = choose(&[vec![10.0, 200.0, f32::INFINITY, f32::INFINITY]], 2);
        assert_eq!(masks, vec![0]);
    }
}

#[cfg(test)]
mod regression {
    use super::*;
    use crate::military::fightsim::{UnitSpec, WeaponSpec};
    fn spec(def: &str, cost: f32, health: f32, speed: f32, weapon: Option<(&str, f32, f32, f32)>) -> Arc<UnitSpec> {
        Arc::new(UnitSpec {
            def: def.into(),
            cost,
            health,
            speed,
            mobile: speed > 0.0,
            radius: 19.0,
            weapons: weapon
                .into_iter()
                .map(|(name, range, reload, damage)| WeaponSpec {
                    name: name.into(),
                    weapon_type: if def == "spideremp" { "LightningCannon" } else { "Cannon" }.into(),
                    range,
                    reload,
                    damage,
                    salvo: 1,
                })
                .collect(),
            ..Default::default()
        })
    }
    fn observed(id: i32, spec: Arc<UnitSpec>, side: u8, x: f32, z: f32) -> Observed {
        Observed { id, paralysis: 0.0, unit: SnapUnit { hp: spec.health, spec, side, x, z } }
    }
    fn incident() -> Raid {
        let venom = spec("spideremp", 190.0, 740.0, 2.8, Some(("spideremp_spider", 240.0, 1.2, 65.0)));
        let dagger = spec("hoverraid", 80.0, 300.0, 4.8, Some(("hoverraid_gauss", 212.0, 2.8 + 1.0 / 30.0, 110.01)));
        let mex = spec("staticmex", 85.0, 560.0, 0.0, None);
        // Adansonia partial replay, frame 7500: the east Venom was redirected
        // north while two Daggers shot this triplet; the west Venom was 2k away.
        let mut raid = Raid {
            enemies: vec![
                observed(20418, dagger.clone(), 1, 4860.0, 6015.0),
                observed(28266, dagger, 1, 4780.0, 5882.0),
            ],
            assets: vec![
                observed(25023, mex.clone(), 0, 4728.0, 6056.0),
                observed(7880, mex.clone(), 0, 4488.0, 6040.0),
                observed(23814, mex, 0, 4600.0, 6280.0),
            ],
            weights: vec![1.0; 3],
            defenders: vec![
                vec![(observed(15482, venom.clone(), 0, 4537.0, 5554.0), 0)],
                vec![(observed(3473, venom, 0, 2744.0, 5754.0), 0)],
            ],
            allowed: 3,
            kept: 3,
            focus: vec![25023, 25023],
        };
        // Health percentages from the replay snapshot at that frame.
        raid.assets[0].unit.hp *= 0.21;
        raid.assets[2].unit.hp *= 0.61;
        raid
    }
    #[test]
    fn adansonia_nearby_venom_is_not_equivalent_to_a_distant_assignment() {
        let model = Model::load().unwrap();
        let raid = incident();
        let costs: Vec<_> = (0..4).map(|mask| losses(&model, &raid, mask)).collect();
        eprintln!("Adansonia predicted loss [none, near, far, both]: {costs:?}");
        assert!(costs[1] + 20.0 < costs[2], "arrival must change the defense outcome");
        let mut delayed = raid.clone();
        delayed.defenders[0][0].1 = HORIZON;
        assert!(
            (losses(&model, &delayed, 1) - costs[0]).abs() < 0.01,
            "a defender that arrives after the horizon cannot save anything"
        );
        // Do not force a save when the actual damaged mexes are already
        // beyond rescue according to this model. Earlier, while still healthy,
        // the same positions make keeping the nearby responder worthwhile.
        let mut early = raid.clone();
        for a in &mut early.assets {
            a.unit.hp = a.unit.spec.health;
        }
        let early_costs: Vec<_> = (0..4).map(|mask| losses(&model, &early, mask)).collect();
        eprintln!("Healthy triplet at the same positions: {early_costs:?}");
        let (_, masks) = choose(&[early_costs, vec![25.0, 0.0, 10.0, 0.0]], 2);
        assert!(masks[0] & 1 != 0, "keep the nearby defender when it can save the mexes");
    }
    #[test]
    fn stale_health_membership_and_eligibility_invalidate_answers() {
        let req = Request { frame: 10, teams: vec![0, 1], raids: vec![incident()] };
        assert!(req.reusable(&req));
        let mut now = req.clone();
        now.frame += STALE + 1;
        assert!(!req.reusable(&now));
        let mut now = req.clone();
        now.raids[0].assets[0].unit.hp = 1.0;
        assert!(!req.reusable(&now));
        let mut now = req.clone();
        now.raids[0].defenders[0][0].0.id += 1;
        assert!(!req.reusable(&now));
        let mut now = req.clone();
        now.raids[0].allowed = 2;
        assert!(!req.reusable(&now));
    }
    #[test]
    fn local_predictor_cost_is_bounded() {
        let request = Request { frame: 0, teams: vec![0, 1], raids: vec![incident()] };
        let (_, result) = compute(request.clone());
        assert!(result.is_ok());
        let start = std::time::Instant::now();
        for _ in 0..20 {
            assert!(compute(request.clone()).1.is_ok());
        }
        eprintln!("QRF incident batch: {:.1} us (four subset simulations)", start.elapsed().as_micros() as f64 / 20.0);
    }
}
