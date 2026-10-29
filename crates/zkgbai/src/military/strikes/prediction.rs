//! The mechanics model runs off the simulation thread. Engine handles are
//! resolved into plain snapshots before submission; stale answers never launch.
use super::{COOLDOWN, DIVE_TIMEOUT, Phase, Wave, launchable};
use crate::Ai;
use crate::military::fightsim::{FateMode, FightSnapshot, Model, SnapUnit, UnitSpec, WeaponSpec};
use crate::util::distance;
use springai::UnitDef;
use std::sync::{
    Arc,
    mpsc::{self, Receiver, SyncSender},
};

struct Request {
    key: u64,
    snapshot: FightSnapshot,
    target: usize,
    pos: springai::Vec3,
    members: Vec<i32>,
    paralysis: Option<f32>,
    stun_required: bool,
}
struct Answer {
    key: u64,
    kill_frames: Option<i32>,
    probability: f32,
    pos: springai::Vec3,
    members: Vec<i32>,
}
pub(super) struct Predictor {
    tx: SyncSender<Request>,
    rx: Receiver<Answer>,
}
impl Predictor {
    fn start() -> Result<Self, String> {
        let model = Model::load()?;
        let (tx, requests) = mpsc::sync_channel::<Request>(1);
        let (answers, rx) = mpsc::sync_channel(1);
        std::thread::Builder::new()
            .name("strike-fates".into())
            .spawn(move || {
                while let Ok(r) = requests.recv() {
                    let stun_frames = r.paralysis.map(super::super::fightsim::mech::paralysis_frames).unwrap_or(0);
                    // Leave time for the async response and the next command tick.
                    let horizon =
                        if r.stun_required { (stun_frames - 90).clamp(1, DIVE_TIMEOUT) } else { DIVE_TIMEOUT };
                    let paralysis: Vec<_> = r.paralysis.map(|ratio| (r.target, ratio)).into_iter().collect();
                    let fates = model.fates_with_paralysis(
                        &r.snapshot,
                        horizon,
                        FateMode::Assassinate { target: r.target },
                        &paralysis,
                    );
                    let fate = fates.get(r.target);
                    let kill_frames = fate.and_then(|f| f.death_frame);
                    let probability = fate.filter(|f| !r.stun_required || f.dead).map_or(0.0, |f| f.p_dead);
                    if answers
                        .send(Answer { key: r.key, kill_frames, probability, pos: r.pos, members: r.members })
                        .is_err()
                    {
                        break;
                    }
                }
            })
            .map_err(|e| e.to_string())?;
        Ok(Self { tx, rx })
    }
}

impl Ai {
    fn strike_spec(&self, def: UnitDef, target: Option<UnitDef>) -> Arc<UnitSpec> {
        let mut weapons: Vec<WeaponSpec> = Vec::new();
        for mount in def.weapon_mounts() {
            let Some(wd) = mount.weapon_def() else { continue };
            if target.is_some_and(|d| d.category() != 0 && mount.only_target_category() & d.category() == 0) {
                continue;
            }
            let name = wd.name();
            if weapons.iter().any(|w| w.name == name)
                || wd.is_shield()
                || name.contains("fake")
                || name.contains("noweapon")
            {
                continue;
            }
            let damage = wd.damage_types().first().copied().unwrap_or(0.0);
            if damage <= 0.0 || wd.range() <= 0.0 {
                continue;
            }
            weapons.push(WeaponSpec {
                name,
                range: wd.range(),
                reload: wd.reload(),
                salvo: wd.salvo_size(),
                damage,
                weapon_type: wd.weapon_type(),
            });
        }
        Arc::new(UnitSpec {
            def: self.defs.name(def).into(),
            cost: def.cost(self.metal),
            speed: def.speed() / 30.0,
            mobile: def.is_able_to_move() && def.speed() > 0.0,
            weapons,
            health: def.health(),
            air: def.is_able_to_fly(),
            builder: def.is_builder(),
            com: self.defs.is_commander(def),
            radius: def.radius(),
        })
    }

    pub(super) fn strike_predict(&mut self, w: &mut Wave) {
        if self.strikes.model_failed {
            return;
        }
        if self.strikes.predictor.is_none() {
            match Predictor::start() {
                Ok(p) => self.strikes.predictor = Some(p),
                Err(e) => {
                    self.log(&format!("strikes: model unavailable: {e}"));
                    self.strikes.model_failed = true;
                    return;
                }
            }
        }
        let p = self.strikes.predictor.as_ref().unwrap();
        while let Ok(a) = p.rx.try_recv() {
            if w.query.is_some_and(|(key, at)| key == a.key && self.frame - at < 90)
                && a.members == w.members
                && distance(a.pos, w.job.pos) < 150.0
            {
                w.query = None;
                let fits = !w.job.prey.needs_stun(w.tactic)
                    || stun_window_fits(self.strike_stun(&w.job).map_or(0, |(_, frames)| frames), a.kill_frames);
                if launchable(a.probability) && fits {
                    w.phase = Phase::Diving;
                    w.since = self.frame;
                    self.log(&format!(
                        "strikes: {} {} dive {} (target death {:.0}%)",
                        w.members.len(),
                        w.tactic.name(),
                        w.job.name,
                        100.0 * a.probability
                    ));
                } else {
                    // A refusal is a completed assessment, not permission to
                    // spend all CPU rechecking the same rejected job each second.
                    w.phase = Phase::Returning;
                    w.since = self.frame;
                    self.strikes.cooldowns.push((w.job.pos, self.frame - COOLDOWN + 30 * 30));
                    self.log(&format!("strikes: defer {} (target death {:.0}%)", w.job.name, 100.0 * a.probability));
                }
                return;
            }
        }
        if w.query.is_some_and(|(_, at)| self.frame - at < 90) {
            return;
        }
        let mut snapshot = FightSnapshot::default();
        for id in &w.members {
            let Some(f) = self.fighters.get(id) else { return };
            let Some(def) = f.unit.def() else { return };
            let pos = f.get_pos();
            snapshot.units.push(SnapUnit {
                side: 0,
                spec: self.strike_spec(def, None),
                hp: f.unit.health(),
                x: pos.x,
                z: pos.z,
            });
        }
        let Some(def) =
            w.job.id.and_then(|id| self.war.targets.get(&id)).and_then(|e| e.ud).or_else(|| self.defs.get(&w.job.name))
        else {
            return;
        };
        let hp =
            w.job.id.and_then(|id| self.war.targets.get(&id)).map_or(def.health(), |e| e.get_health() * e.def_health());
        let target = snapshot.units.len();
        let seen = w.job.id.and_then(|id| self.war.targets.get(&id)).filter(|e| e.visible);
        let ground_target = self.defs.get(w.tactic.name());
        let mut spec = self.strike_spec(def, ground_target);
        if let Some(e) = seen {
            Arc::make_mut(&mut spec).health = e.unit.max_health().max(1.0);
        }
        let hp = seen.map_or(hp, |e| e.unit.health());
        snapshot.units.push(SnapUnit { side: 1, spec, hp, x: w.job.pos.x, z: w.job.pos.z });
        // Inferred extra Phantoms also shoot; never treat a three-Phantom fix as
        // just one defender merely because the current focus is one of them.
        if w.job.phantom() {
            for i in 1..w.job.count {
                snapshot.units.push(SnapUnit {
                    side: 1,
                    spec: self.strike_spec(def, ground_target),
                    hp: def.health(),
                    x: w.job.pos.x + i as f32 * 80.0,
                    z: w.job.pos.z,
                });
            }
        }
        let start = w.home;
        let end = w.job.pos;
        let dx = end.x - start.x;
        let dz = end.z - start.z;
        for e in self.war.targets.values().filter(|e| {
            !e.is_dead
                && e.identified
                && e.can_attack()
                && (e.is_static || self.frame - e.last_seen < 30 * 30)
                && Some(e.unit_id) != w.job.id
        }) {
            // Already represented by the cadence/seen max above.
            if w.job.phantom() && e.def_name == "cloaksnipe" && distance(e.position, end) < 400.0 {
                continue;
            }
            let t = (((e.position.x - start.x) * dx + (e.position.z - start.z) * dz) / (dx * dx + dz * dz).max(1.0))
                .clamp(0.0, 1.0);
            let closest = springai::Vec3::xz(start.x + t * dx, start.z + t * dz);
            if distance(e.position, closest) > e.def_max_weapon_range().max(450.0) + 150.0 {
                continue;
            }
            let Some(def) = e.ud else { continue };
            if snapshot.units.len() >= 192 {
                return;
            } // Fail closed for unbounded fronts.
            snapshot.units.push(SnapUnit {
                side: 1,
                spec: self.strike_spec(def, ground_target),
                hp: e.get_health() * e.def_health(),
                x: e.position.x,
                z: e.position.z,
            });
        }
        let paralysis = self.strike_stun(&w.job).map(|(ratio, _)| ratio);
        let stun_required = w.job.prey.needs_stun(w.tactic);
        if stun_required && paralysis.is_none() {
            return;
        }
        self.strikes.serial += 1;
        let key = self.strikes.serial;
        if self
            .strikes
            .predictor
            .as_ref()
            .unwrap()
            .tx
            .try_send(Request {
                key,
                snapshot,
                target,
                pos: w.job.pos,
                members: w.members.clone(),
                paralysis,
                stun_required,
            })
            .is_ok()
        {
            w.query = Some((key, self.frame));
        }
    }
}

/// Recheck the live window when an asynchronous answer comes back.
fn stun_window_fits(remaining: i32, kill_frames: Option<i32>) -> bool {
    kill_frames.is_some_and(|kill| kill >= 0 && remaining > kill.saturating_add(super::PERIOD))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn stale_prediction_cannot_spend_an_expired_stun() {
        assert!(stun_window_fits(600, Some(300)));
        assert!(!stun_window_fits(330, Some(300)));
        assert!(!stun_window_fits(0, Some(300)));
        assert!(!stun_window_fits(600, None));
    }
}
