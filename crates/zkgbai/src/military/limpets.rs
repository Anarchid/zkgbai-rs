//! Ordinary squads keep their Limpets in reserve until spending one improves
//! the committed fight AND the whole trade is nonnegative. Recruitment stays
//! with the existing handlers. Ownership protects reserve moves and latched
//! charges from a-move, regroup and damage-retreat orders.
use std::collections::BTreeMap;

use serde_json::json;
use springai::{
    Unit, Vec3,
    reserved::{self, Owner},
};

use crate::military::fightsim::BombVerdict;
use crate::military::{SquadId, SquadKind};
use crate::util::{NULLPOS, distance};
use crate::{Ai, nav};

pub const FEATURE: &str = "limpet";
pub const SHADOW: &str = "limpetshadow";
const OWNER: Owner = Owner { module: "limpet", helpers: &[] };
const HORIZON: i32 = 600;
const PERIOD: i32 = 15;
const MAX_UNITS: usize = 24;
const MARGIN: f32 = 10.0;

#[derive(Clone)]
struct Charge {
    bomb: i32,
    escorts: Vec<i32>,
    target: i32,
    since: i32,
}

#[derive(Default)]
pub struct Limpets {
    owned: BTreeMap<i32, Option<Owner>>,
    charges: BTreeMap<SquadId, Charge>,
    query_frame: i32,
    queries: usize,
}

/// A small positive improvement alone cannot buy a losing squad attack.
fn worthwhile(v: &BombVerdict) -> bool {
    [v.net, v.reserve_trade, v.spent].iter().all(|v| v.is_finite())
        && v.net > MARGIN
        && v.reserve_trade + v.net >= 0.0
        && v.went_off.is_some()
        && v.spent > 0.0
}

fn charge_alive(frame: i32, since: i32, bomb_alive: bool, target_fresh: bool) -> bool {
    bomb_alive && target_fresh && frame - since < HORIZON
}

fn ordinary(kind: SquadKind, air: bool) -> bool {
    !air && matches!(kind, SquadKind::Raider | SquadKind::Assault | SquadKind::Hover)
}

impl Ai {
    pub(crate) fn limpet_controls(&self, id: i32) -> bool {
        !self.dev_off(FEATURE)
            && self.war.limpets.owned.contains_key(&id)
            && reserved::owner(self.cb, id) == Some(OWNER)
    }

    pub(crate) fn limpet_committed(&self, id: i32) -> bool {
        self.limpet_controls(id) && self.war.limpets.charges.values().any(|c| c.bomb == id || c.escorts.contains(&id))
    }

    pub(crate) fn limpet_squad_committed(&self, sid: SquadId) -> bool {
        !self.dev_off(FEATURE) && self.war.limpets.charges.contains_key(&sid) && self.limpet_members(sid).is_some()
    }

    pub(crate) fn limpet_squad_managed(&self, sid: SquadId) -> bool {
        !self.dev_off(FEATURE) && self.limpet_members(sid).is_some()
    }

    /// Only ordinary, available squads; never silently borrow a special
    /// controller's escorts or operator-owned units to justify a sacrifice.
    fn limpet_members(&self, sid: SquadId) -> Option<Vec<i32>> {
        let s = self.squads.get(sid)?;
        if !ordinary(s.kind, s.is_air_squad)
            || self.qrf_drafted(sid)
            || self.op_squad_target(sid).is_some()
            || self.squad_strike_active(sid)
            || self.coalesce_owns_squad(sid)
        {
            return None;
        }
        let mut ids = s.fighters.clone();
        ids.extend(s.leader);
        ids.sort_unstable();
        ids.dedup();
        ids.retain(|id| self.fighters.get(id).is_some_and(|f| !f.is_dead() && f.squad == Some(sid)));
        if ids.iter().any(|id| {
            self.retreat_handler.is_retreating(*id)
                || self.coalesce_owns(*id)
                || self.fire_controls(*id)
                || reserved::is_reserved(self.cb, *id)
                || reserved::owner(self.cb, *id).is_some_and(|o| o != OWNER)
        }) {
            return None;
        }
        ids.iter().any(|id| self.defs.name_of(self.fighters[id].unit) == "amphbomb").then_some(ids)
    }

    fn limpet_release(&mut self, id: i32) {
        let Some(previous) = self.war.limpets.owned.remove(&id) else { return };
        if let Some(f) = self.fighters.get_mut(&id) {
            f.limpet_controlled = false;
        }
        // Another controller may have taken over. Never restore stale ownership.
        if reserved::owner(self.cb, id) != Some(OWNER) {
            return;
        }
        if let Some(u) = Unit::from_id(self.cb, id).filter(|u| u.health() > 0.0 && u.team() == self.team_id) {
            u.stop(0);
            if self.defs.name_of(u) == "amphbomb" {
                u.set_move_state(1, 0);
                u.set_fire_state(2, 0);
            }
        }
        match previous {
            Some(o) => {
                reserved::own(self.cb, id, o);
            }
            None => {
                reserved::disown(self.cb, id);
            }
        }
    }

    fn limpet_own(&mut self, id: i32) {
        if self.war.limpets.owned.contains_key(&id) {
            return;
        }
        let previous = reserved::own(self.cb, id, OWNER);
        self.war.limpets.owned.insert(id, previous);
        if let Some(f) = self.fighters.get_mut(&id)
            && self.defs.name_of(f.unit) == "amphbomb"
        {
            f.limpet_controlled = true;
            f.unit.stop(0);
            f.unit.set_move_state(0, 0);
            f.unit.set_fire_state(0, 0);
        }
    }

    /// Before ordinary handlers run: reconcile switches and ownership each
    /// frame, maintain orders twice a second. No simulation is done here.
    pub(crate) fn limpet_update(&mut self, frame: i32) {
        if self.dev_off(FEATURE) && self.war.limpets.owned.is_empty() {
            self.war.limpets.charges.clear();
            return;
        }
        let squads: Vec<_> = self
            .fighters
            .values()
            .filter(|f| self.defs.name_of(f.unit) == "amphbomb")
            .filter_map(|f| f.squad)
            .collect();
        let eligible: BTreeMap<_, _> = if self.dev_off(FEATURE) {
            BTreeMap::new()
        } else {
            squads.into_iter().filter_map(|sid| self.limpet_members(sid).map(|ids| (sid, ids))).collect()
        };
        let ended: Vec<_> = self
            .war
            .limpets
            .charges
            .iter()
            .filter_map(|(&sid, c)| {
                let members = eligible.get(&sid);
                let alive = members.is_some_and(|m| {
                    m.contains(&c.bomb)
                        && c.escorts
                            .iter()
                            .all(|id| self.fighters.get(id).is_none_or(|f| f.is_dead() || m.contains(id)))
                });
                let fresh = self
                    .war
                    .targets
                    .get(&c.target)
                    .is_some_and(|e| !e.is_dead && e.identified && frame - e.last_seen <= 90);
                (!charge_alive(frame, c.since, alive, fresh)).then_some(sid)
            })
            .collect();
        for sid in ended {
            self.war.limpets.charges.remove(&sid);
            self.op_emit(FEATURE, json!({"event":"release", "squad":sid, "t":frame/30}));
        }
        let mut desired: Vec<_> = eligible
            .values()
            .flatten()
            .copied()
            .filter(|id| self.defs.name_of(self.fighters[id].unit) == "amphbomb")
            .collect();
        desired.extend(self.war.limpets.charges.values().flat_map(|c| c.escorts.iter().copied()));
        let release: Vec<_> = self.war.limpets.owned.keys().copied().filter(|id| !desired.contains(id)).collect();
        for id in release {
            self.limpet_release(id);
        }
        for id in desired {
            self.limpet_own(id);
        }
        // A reserved bomb must not be the anchor that its escorts rally to.
        for (&sid, ids) in &eligible {
            let escort = ids.iter().find(|id| self.defs.name_of(self.fighters[id].unit) != "amphbomb").copied();
            if let Some(s) = self.squads.get_mut(sid)
                && s.leader.is_none_or(|id| self.fighters.get(&id).is_some_and(|f| f.limpet_controlled))
                && escort.is_some()
            {
                s.leader = escort;
            }
        }
        if frame % PERIOD != self.offset % PERIOD {
            return;
        }
        let charges: Vec<_> = self.war.limpets.charges.values().cloned().collect();
        for c in charges {
            self.limpet_charge_orders(&c);
        }
        for (sid, ids) in eligible {
            let escort_positions: Vec<_> = ids
                .iter()
                .filter_map(|id| {
                    let f = &self.fighters[id];
                    (self.defs.name_of(f.unit) != "amphbomb").then_some(f.get_pos())
                })
                .collect();
            for id in ids {
                if self.defs.name_of(self.fighters[&id].unit) != "amphbomb" || self.limpet_committed(id) {
                    continue;
                }
                let u = self.fighters[&id].unit;
                let p = u.pos();
                let n = escort_positions.len() as f32;
                let middle = if n > 0.0 {
                    Vec3::xz(
                        escort_positions.iter().map(|p| p.x).sum::<f32>() / n,
                        escort_positions.iter().map(|p| p.z).sum::<f32>() / n,
                    )
                } else {
                    p
                };
                let enemy = self
                    .war
                    .targets
                    .values()
                    .filter(|e| !e.is_dead && !e.can_fly() && (e.is_static || frame - e.last_seen <= 90))
                    .min_by(|a, b| distance(middle, a.position).total_cmp(&distance(middle, b.position)))
                    .filter(|e| distance(middle, e.position) < 1200.0)
                    .map(|e| e.position);
                let mut to = enemy.map_or(middle, |e| reserve_point(middle, p, e));
                if enemy.is_some() && self.war.get_threat(to) > 0.0 {
                    to = self.graph_get_closest_raider_haven(p);
                }
                if distance(p, to) > 50.0 {
                    let mut nav = nav!(self);
                    if let Some(f) = self.fighters.get_mut(&id) {
                        f.move_to(to, &mut nav);
                    }
                } else {
                    u.stop(0);
                }
            }
            let _ = sid;
        }
    }

    fn limpet_charge_orders(&mut self, c: &Charge) {
        let Some(e) = self.war.targets.get(&c.target) else { return };
        let (at, target, visible) = (e.position, e.unit, e.visible);
        if let Some(f) = self.fighters.get(&c.bomb) {
            if visible {
                f.unit.attack(target, 0);
            } else {
                crate::kgbutil::kgb_move_to_exact(f.unit, at, 4.0, 0, self.frame);
            }
        }
        let mut nav = nav!(self);
        for id in &c.escorts {
            if let Some(f) = self.fighters.get_mut(id) {
                f.fight_to(at, &mut nav);
            }
        }
    }

    /// Called only at an ordinary engagement decision. No cached answer can
    /// commit a changed roster. Other reserved Limpets are absent in BOTH
    /// counterfactuals, so the first charge cannot spend them implicitly.
    pub(crate) fn limpet_verdict(&mut self, sid: SquadId, target: Vec3, flat: bool) -> Option<bool> {
        let live = !self.dev_off(FEATURE);
        if !live && self.dev_off(SHADOW) {
            return None;
        }
        let ids = self.limpet_members(sid)?;
        if self.limpet_squad_committed(sid) {
            return Some(false);
        }
        if target == NULLPOS {
            return live.then_some(flat);
        }
        let units: Vec<_> = ids.iter().map(|id| self.fighters[id].unit).collect();
        let mut local = self.local_fight(&units)?;
        let unsupported = local.unknown
            || local.snap.units.len() > MAX_UNITS
            || local.snap.units.iter().any(|u| u.spec.air)
            || units.iter().any(|u| {
                u.is_cloaked()
                    || u.def()
                        .is_some_and(|d| d.weapon_mounts().iter().filter_map(|m| m.weapon_def()).any(|w| w.is_shield()))
            })
            || self.war.targets.values().any(|e| {
                !e.is_dead
                    && self.frame - e.last_seen <= 90
                    && units.iter().any(|u| distance(u.pos(), e.position) < 1100.0)
                    && (e.position.y < -5.0
                        || e.ud.is_some_and(|d| {
                            d.weapon_mounts().iter().filter_map(|m| m.weapon_def()).any(|w| w.is_shield())
                        }))
            });
        if unsupported || !local.armed_theirs() {
            return live.then_some(flat);
        }
        let bomb = local
            .ours
            .iter()
            .enumerate()
            .filter(|(i, _)| local.snap.units[*i].spec.def == "amphbomb")
            .min_by(|(a, _), (b, _)| {
                let near = |i: usize| {
                    local
                        .theirs
                        .iter()
                        .map(|e| distance(Vec3::xz(local.snap.units[i].x, local.snap.units[i].z), e.pos))
                        .fold(f32::INFINITY, f32::min)
                };
                near(*a).total_cmp(&near(*b))
            })
            .map(|(_, u)| u.id)?;
        let others: Vec<_> = local
            .ours
            .iter()
            .zip(&local.snap.units)
            .filter(|(u, s)| u.id != bomb && s.spec.def == "amphbomb")
            .map(|(u, _)| u.id)
            .collect();
        local = local.without(&others);
        if local.snap.armed(0) == 0 {
            return live.then_some(flat);
        }
        let index = local.ours.iter().position(|u| u.id == bomb)?;
        let p = Vec3::xz(local.snap.units[index].x, local.snap.units[index].z);
        let nearest = local.theirs.iter().min_by(|a, b| distance(p, a.pos).total_cmp(&distance(p, b.pos)))?;
        let target_id = nearest.id;
        if distance(p, nearest.pos) > 700.0 {
            return live.then_some(flat);
        }
        if self.war.limpets.query_frame != self.frame {
            self.war.limpets.query_frame = self.frame;
            self.war.limpets.queries = 0;
        }
        if self.war.limpets.queries >= 2 {
            return live.then_some(flat);
        }
        self.war.limpets.queries += 1;
        let model = self.local_model()?;
        let started = std::time::Instant::now();
        let v = model.limpet_charge(&local.snap, index, HORIZON);
        let charge = worthwhile(&v);
        self.op_emit(
            FEATURE,
            json!({"event":if live {if charge {"charge"} else {"reserve"}} else {"shadow"},
            "t":self.frame/30, "squad":sid, "unit":bomb, "target":target_id, "would_charge":charge,
            "gain":v.gain, "saved":v.saved, "spent":v.spent, "net":v.net,
            "reserve_trade":v.reserve_trade, "charge_trade":v.reserve_trade+v.net,
            "flat":flat, "micros":started.elapsed().as_micros() as u64,
            "members":local.ours.iter().map(|u|u.id).collect::<Vec<_>>(),
            "snap":local.snap.units.iter().map(|u|json!([u.spec.def,u.side,u.hp,u.x,u.z])).collect::<Vec<_>>() }),
        );
        if !live {
            return None;
        }
        if charge {
            let escorts: Vec<_> = local.ours.iter().filter(|u| u.id != bomb).map(|u| u.id).collect();
            let c = Charge { bomb, escorts, target: target_id, since: self.frame };
            reserved::as_module("limpet.commit", || {
                self.limpet_own(bomb);
                for &id in &c.escorts {
                    self.limpet_own(id);
                }
                self.limpet_charge_orders(&c);
            });
            self.war.limpets.charges.insert(sid, c);
            Some(false)
        } else {
            // The squad can still kill things without spending its Limpet.
            Some(flat || v.reserve_trade < 0.0)
        }
    }
}

fn reserve_point(middle: Vec3, bomb: Vec3, enemy: Vec3) -> Vec3 {
    let (dx, dz) = (middle.x - enemy.x, middle.z - enemy.z);
    let len = dx.hypot(dz);
    if len < 1.0 {
        return bomb;
    }
    Vec3::xz(middle.x + 350.0 * dx / len, middle.z + 350.0 * dz / len)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn marginal_gain_must_pay_and_whole_squad_must_survive_the_trade() {
        let good =
            BombVerdict { net: 475.0, reserve_trade: -320.0, spent: 1.0, went_off: Some(60), ..Default::default() };
        assert!(worthwhile(&good));
        assert!(!worthwhile(&BombVerdict { net: -160.0, reserve_trade: 400.0, ..good }));
        assert!(!worthwhile(&BombVerdict { net: 254.0, reserve_trade: -320.0, ..good }));
        assert!(!worthwhile(&BombVerdict { went_off: None, ..good }));
        assert!(!worthwhile(&BombVerdict { net: f32::NAN, ..good }));
    }
    #[test]
    fn commitments_end_on_death_missing_target_or_timeout_not_damage() {
        assert!(charge_alive(599, 0, true, true));
        assert!(!charge_alive(600, 0, true, true));
        assert!(!charge_alive(60, 0, false, true));
        assert!(!charge_alive(60, 0, true, false));
    }
    #[test]
    fn reserve_is_behind_escorts_and_special_squads_are_excluded() {
        assert_eq!(
            reserve_point(Vec3::xz(500.0, 500.0), Vec3::xz(450.0, 500.0), Vec3::xz(800.0, 500.0)),
            Vec3::xz(150.0, 500.0)
        );
        assert!(ordinary(SquadKind::Raider, false));
        assert!(ordinary(SquadKind::Assault, false));
        assert!(!ordinary(SquadKind::Jaeger, false));
        assert!(!ordinary(SquadKind::Taskforce, false));
        assert!(!ordinary(SquadKind::Assault, true));
    }
    #[test]
    fn charge_owner_blocks_ordinary_orders_and_retreat() {
        assert!(OWNER.admits("limpet.commit"));
        assert!(OWNER.admits("limpet.update"));
        for module in ["squad.update", "raider.update", "retreat.update", "war.unitDamaged", "facframe.update"] {
            assert!(!OWNER.admits(module));
        }
        assert!(!crate::ai::OFF_BY_DEFAULT.contains(&FEATURE));
        assert!(crate::ai::OFF_BY_DEFAULT.contains(&SHADOW));
    }
}
