//! Opt-in experiment: permission to commit ordinary assaults and assault-doctrine
//! taskforces (shield balls). Raiders, aircraft and specialist doctrines retain
//! their dedicated controllers. No standing permission is given to reinforcements.
//! A refused encounter retains its ground: an enemy backing away cannot drag it
//! forward. Advancing enemies push the anchor back outside their firing reach.
use std::collections::BTreeSet;

use crate::kgbutil::{Nav, PathCost, kgb_move_to};
use crate::military::fightsim::{FightSnapshot, SnapUnit, UnitFate};
use crate::military::taskforce::Aim;
use crate::military::{MilitaryManager, SquadId, SquadKind};
use crate::util::{NULLPOS, distance, get_directional_point};
use crate::{Ai, nav};
use serde_json::json;
use springai::{Unit, Vec3};

const HORIZON: i32 = 900;
const RECHECK: i32 = 90;
const MEMORY: i32 = 600;
const GATHER: f32 = 400.0;
const MARGIN: f32 = 192.0;

/// A sighting remains a movement hazard after the transient target cache drops
/// it. Only a new observation, confirmed death or visibly empty location changes it.
#[derive(Clone, Debug, PartialEq)]
#[checkpoint::state]
pub struct Hazard {
    pub pos: Vec3,
    pub range: f32,
    pub seen: i32,
    pub identified: bool,
}

pub(crate) fn remembered_threat(war: &MilitaryManager, p: Vec3) -> f32 {
    if !war.license_enabled {
        return 0.0;
    }
    war.license_hazards.values().map(|h| ((h.range + 160.0 - distance(h.pos, p)) / 160.0).clamp(0.0, 1.0)).sum()
}

#[derive(Clone, Debug, Default, PartialEq)]
#[checkpoint::state]
pub struct License {
    /// Exact roster and opposition approved for this commitment.
    pub members: BTreeSet<i32>,
    pub enemies: BTreeSet<i32>,
    pub until: i32,
    pub next_check: i32,
    /// Persisted world-space stand-off, never advanced while its encounter lives.
    pub anchor: Option<Vec3>,
    pub contact_at: i32,
    pub committed_at: i32,
    pub goal: Vec3,
    pub judgments: u64,
    pub grants: u64,
    pub refusals: u64,
}
impl License {
    pub fn active(&self, frame: i32) -> bool {
        frame < self.until && !self.members.is_empty()
    }
    pub fn permits(&self, id: i32, frame: i32) -> bool {
        self.active(frame) && self.members.contains(&id)
    }
}

/// Charge all damage to our side, but credit only enemy deaths. This deliberately
/// rejects chipping a repairable heavy while donating units. Accept up to 1.5 metal
/// lost per metal killed, with at least 30 metal of concrete expected enemy kills.
fn worthwhile(harm: f32, kills: f32) -> bool {
    harm.is_finite() && kills.is_finite() && harm >= 0.0 && kills >= 30.0 && kills >= harm * (2.0 / 3.0)
}
fn trade(snap: &FightSnapshot, fates: &[UnitFate]) -> (f32, f32) {
    let (mut harm, mut kills) = (0.0, 0.0);
    for (u, f) in snap.units.iter().zip(fates) {
        if u.side == 0 {
            let hp_loss = ((u.hp - f.hp_left) / u.spec.health.max(1.0)).clamp(0.0, 1.0);
            harm += u.spec.cost * hp_loss.max(f.p_raw);
        } else {
            kills += u.spec.cost * f.p_raw;
        }
    }
    (harm, kills)
}

/// Move an anchor only away from danger. A kiter retreating never advances it.
fn stand_off(mut anchor: Vec3, enemy: Vec3, reach: f32, away: Vec3) -> Vec3 {
    if distance(anchor, enemy) < reach {
        if distance(anchor, enemy) < 1.0 {
            anchor = away;
        }
        anchor = get_directional_point(enemy, anchor, reach);
    }
    anchor
}

/// Sample the member's footprint, not just the formation centre or destination.
fn exposure(war: &MilitaryManager, unit: Unit, p: Vec3) -> f32 {
    let view = war.view(unit.def());
    [(0.0, 0.0), (48.0, 0.0), (-48.0, 0.0), (0.0, 48.0), (0.0, -48.0)]
        .into_iter()
        .map(|(x, z)| {
            let q = Vec3::new(p.x + x, p.y, p.z + z);
            view.get_threat(q).max(remembered_threat(war, q))
        })
        .fold(0.0, f32::max)
}

/// Maximal safe prefix. Already exposed units may escape along equal/decreasing
/// threat; a safe unit never enters a threatened sample, even on A* fallback.
fn safe_prefix(from: Vec3, to: Vec3, mut risk: impl FnMut(Vec3) -> f32) -> Vec3 {
    let steps = (distance(from, to) / 16.0).ceil().max(1.0) as usize;
    let mut last = from;
    let mut ceiling = risk(from);
    for i in 1..=steps {
        let t = i as f32 / steps as f32;
        let p = Vec3::new(from.x + (to.x - from.x) * t, from.y + (to.y - from.y) * t, from.z + (to.z - from.z) * t);
        let r = risk(p);
        if !r.is_finite() || r > ceiling + 0.00001 {
            break;
        }
        ceiling = ceiling.min(r);
        last = p;
    }
    last
}

/// Cautious transit has no queued guard/orbit or attack order. Only the first
/// checked segment is submitted: waypoint skipping must not cut a danger corner.
pub fn transit(unit: Unit, goal: Vec3, nav: &mut Nav) {
    unit.set_move_state(0, 0);
    let at = unit.pos();
    nav.pathfinder.walk_only = true;
    let path = nav.pathfinder.find_path(nav.war, unit, goal, PathCost::SafeTransit);
    let next = path.into_iter().find(|p| distance(at, *p) > 12.0).unwrap_or(at);
    let mut to = safe_prefix(at, next, |p| exposure(nav.war, unit, p));
    // A moving enemy may engulf a holding squad. Escape locally if the chosen
    // route initially increases exposure; don't sit and wait to be killed.
    if distance(at, to) < 12.0 && exposure(nav.war, unit, at) > 0.0 {
        let mut best = (exposure(nav.war, unit, at), f32::INFINITY);
        for i in 0..16 {
            let a = i as f32 * std::f32::consts::TAU / 16.0;
            let p = Vec3::new(at.x + 192.0 * a.cos(), at.y, at.z + 192.0 * a.sin());
            nav.pathfinder.walk_only = true;
            let route = nav.pathfinder.find_path(nav.war, unit, p, PathCost::SafeTransit);
            let next = route.into_iter().find(|q| distance(at, *q) > 12.0).unwrap_or(at);
            let end = safe_prefix(at, next, |p| exposure(nav.war, unit, p));
            let score = (exposure(nav.war, unit, end), distance(end, goal));
            if distance(at, end) > 12.0 && score < best {
                best = score;
                to = end;
            }
        }
    }
    if distance(at, to) > 12.0 {
        kgb_move_to(unit, to, 0, nav.frame);
    } else {
        unit.stop(0);
    }
}

impl Ai {
    pub(crate) fn license_observe(&mut self) {
        self.war.license_hazards.retain(|id, h| {
            let present = self.war.targets.get(id).is_some_and(|e| !e.is_dead && e.last_seen >= self.frame - 30);
            present || !self.los.is_in_los(h.pos)
        });
        for (&id, e) in &self.war.targets {
            if !e.is_dead && !e.can_fly() && e.can_attack() && e.position != NULLPOS && self.frame - e.last_seen <= 30 {
                self.war.license_hazards.insert(
                    id,
                    Hazard {
                        pos: e.position,
                        range: e.def_max_weapon_range().max(350.0),
                        seen: e.last_seen,
                        identified: e.identified,
                    },
                );
            }
        }
    }
    pub(crate) fn license_covers(&self, sid: SquadId) -> bool {
        !self.dev_off("fightlicense")
            && self.squads.get(sid).is_some_and(|s| {
                !s.is_air_squad
                    && (matches!(s.kind, SquadKind::Assault | SquadKind::Hover)
                        || s.kind == SquadKind::Taskforce && s.shape.aim == Aim::Assault)
            })
    }
    pub(crate) fn license_committed(&self, id: i32) -> bool {
        !self.dev_off("fightlicense")
            && self
                .fighters
                .get(&id)
                .and_then(|f| f.squad)
                .and_then(|sid| self.squads.get(sid))
                .and_then(|s| s.license.as_ref())
                .is_some_and(|l| l.permits(id, self.frame))
    }
    pub(crate) fn license_switch(&mut self, off: bool) {
        self.war.license_enabled = !off;
        if !off {
            self.license_observe();
        }
        // Explicit intervention cancels grants and refusal caches; no old answer
        // authorizes the new policy. Force reassignment and cancel engine queues.
        for sid in self.squads.ids() {
            let covered = self.license_covers(sid);
            let s = self.squads.get_mut(sid).unwrap();
            if s.license.take().is_some() || covered {
                for id in s.everyone() {
                    if let Some(f) = self.fighters.get(&id) {
                        f.unit.stop(0);
                    }
                }
                if off && s.kind == SquadKind::Taskforce {
                    use crate::military::taskforce::Keep;
                    s.rekeep(&self.fighters, &[Keep::Orbit, Keep::Guard]);
                } else if covered {
                    s.license = Some(License::default());
                }
                s.last_assignment_frame = self.frame - 30;
            }
        }
    }
    pub(crate) fn license_assign(&mut self, sid: SquadId) -> bool {
        if !self.license_covers(sid) {
            return false;
        }
        self.fire_release(sid);
        self.squad_strike_cancel(sid);
        let Some(s) = self.squads.get(sid) else { return true };
        let Some(leader) = s.get_leader(&self.fighters) else { return true };
        let pos = s.get_pos(&self.fighters);
        let taskforce = s.kind == SquadKind::Taskforce;
        let shape = s.shape.clone();
        let mut l = s.license.clone().unwrap_or_default();
        let ids: Vec<_> = s.everyone().collect::<BTreeSet<_>>().into_iter().collect();
        let local: Vec<_> = ids
            .iter()
            .filter_map(|id| self.fighters.get(id))
            .filter(|f| {
                !f.is_dead() && !self.retreat_handler.is_retreating(f.id) && distance(f.get_pos(), pos) <= GATHER
            })
            .map(|f| f.id)
            .collect();
        let local_metal: f32 = local.iter().filter_map(|id| self.fighters.get(id)).map(|f| f.metal_value).sum();
        let gathered = local_metal >= 0.8 * s.metal_value;
        let health = s.get_health(&self.fighters);
        let low_shields = taskforce && shape.low_shields(s.low_shields, s.get_shields());
        let ready = if taskforce {
            !low_shields
                && health >= shape.retreat_health
                && gathered
                && (s.metal_value > self.squad_engage_threshold(sid)
                    || shape.muster > 0.0 && (self.frame - s.formed) as f32 >= shape.muster * 30.0)
        } else {
            s.status != 'f' && gathered
        };
        let target = if let Some(p) = self.op_squad_target(sid) {
            p
        } else if ready {
            self.war_best_target(leader, pos, true).0
        } else {
            self.war_get_rally_point(pos)
        };
        // Enemy fire covering our next 1200 elmos, plus remembered opponents of
        // the held encounter. Do not count unrelated friends as future participants.
        let toward =
            if target == NULLPOS { pos } else { get_directional_point(pos, target, distance(pos, target).min(1200.0)) };
        let contacts: Vec<_> = self
            .war
            .license_hazards
            .iter()
            .filter(|(id, h)| {
                let reach = h.range + MARGIN;
                let dx = toward.x - pos.x;
                let dz = toward.z - pos.z;
                let t =
                    (((h.pos.x - pos.x) * dx + (h.pos.z - pos.z) * dz) / (dx * dx + dz * dz).max(1.0)).clamp(0.0, 1.0);
                l.enemies.contains(id) || distance(h.pos, Vec3::xz(pos.x + t * dx, pos.z + t * dz)) <= reach
            })
            .map(|(&id, h)| (id, h.pos, h.range, h.identified, h.seen))
            .collect();
        let enemies: BTreeSet<_> = contacts.iter().map(|e| e.0).collect();
        let valid = l.active(self.frame) && enemies.is_subset(&l.enemies) && !enemies.is_empty();
        if !valid {
            l.members.clear();
            l.until = 0;
        }
        if !contacts.is_empty() {
            l.contact_at = self.frame;
            let home = self.graph.get_start_pos();
            let first_post = if ready {
                contacts
                    .iter()
                    .min_by(|a, b| distance(pos, a.1).total_cmp(&distance(pos, b.1)))
                    .map(|e| get_directional_point(e.1, pos, e.2.max(350.0) + MARGIN + 200.0))
                    .unwrap_or(pos)
            } else {
                pos
            };
            let mut anchor = l.anchor.unwrap_or(first_post);
            // Iteration handles overlapping firing envelopes. Individual transit
            // sampling still guards any unsatisfied boundary or formation offset.
            for _ in 0..4 {
                for &(_, p, r, _, _) in &contacts {
                    anchor = stand_off(anchor, p, r.max(350.0) + MARGIN + 200.0, home);
                }
            }
            let map = self.cb.map();
            anchor.x = anchor.x.clamp(64.0, map.width() as f32 * 8.0 - 64.0);
            anchor.z = anchor.z.clamp(64.0, map.height() as f32 * 8.0 - 64.0);
            l.anchor = Some(anchor);
        } else if self.frame - l.contact_at > MEMORY {
            l.anchor = None;
            l.enemies.clear();
        }
        let mut reason = if ready { "no contact" } else { "assembling" };
        if !valid && !contacts.is_empty() && ready && self.frame >= l.next_check {
            l.next_check = self.frame + RECHECK;
            l.judgments += 1;
            l.enemies = enemies.clone();
            let (mut snap, mut unknown) = (FightSnapshot { flags: self.sim_flags(), ..Default::default() }, false);
            for id in &local {
                let f = &self.fighters[id];
                let unit = f.unit;
                let p = unit.pos();
                if p.y < -5.0 || unit.is_cloaked() {
                    unknown = true;
                }
                let Some(d) = unit.def() else {
                    unknown = true;
                    continue;
                };
                let hp = (unit.health() - unit.paralyze_damage()).max(0.0) * (1.0 - unit.capture_progress());
                let spec = self.fight_spec_of(*id, d);
                snap.units.push(SnapUnit {
                    side: 0,
                    spec,
                    hp,
                    x: p.x,
                    z: p.z,
                    reloads: self.snap_reloads(*id),
                    tubes: self.snap_tubes(*id),
                });
            }
            for &(id, p, _, identified, seen) in &contacts {
                let Some(e) = self.war.targets.get(&id) else {
                    unknown = true;
                    continue;
                };
                let Some(d) = e.ud else {
                    unknown = true;
                    continue;
                };
                // Our shields are ignored conservatively. Enemy shields cannot
                // be ignored optimistically: deny unsupported shield matchups.
                if !identified
                    || self.frame - seen > 90
                    || p.y < -5.0
                    || d.weapon_mounts().iter().filter_map(|w| w.weapon_def()).any(|w| w.is_shield())
                {
                    unknown = true;
                }
                let hp = e.get_health() * e.def_health();
                let spec = self.fight_spec_of(id, d);
                snap.units.push(SnapUnit {
                    side: 1,
                    spec,
                    hp,
                    x: p.x,
                    z: p.z,
                    reloads: self.snap_reloads(id),
                    tubes: self.snap_tubes(id),
                });
            }
            let mut assessment = None;
            if !unknown && snap.units.len() <= 96 && snap.armed(0) > 0 && snap.armed(1) > 0 {
                if let Some(model) = self.local_model() {
                    let fates = model.ordered_kiting_fates(&snap, HORIZON);
                    assessment = Some(trade(&snap, &fates));
                }
            }
            if assessment.is_some_and(|(harm, kills)| worthwhile(harm, kills)) {
                l.members = local.iter().copied().collect();
                l.until = self.frame + HORIZON;
                l.committed_at = self.frame;
                l.goal = target;
                l.grants += 1;
                l.anchor = None;
                reason = "commit";
            } else {
                l.refusals += 1;
                reason = if assessment.is_some() { "poor trade" } else { "unknown" };
            }
            self.op_emit("fightlicense",json!({"event":reason,"squad":sid,"members":local,"enemies":enemies,"harm":assessment.map(|x|x.0),"kills":assessment.map(|x|x.1),"until":l.until,"anchor":l.anchor.map(|p|[p.x,p.z])}));
        }
        if l.active(self.frame) {
            let goal = if target == NULLPOS { l.goal } else { target };
            let s = self.squads.get_mut(sid).unwrap();
            s.license = Some(l);
            s.low_shields = low_shields;
            s.engaging = true;
            if s.status == 'r' {
                s.status = 'a';
            }
            // Avoid a fresh policy decision overriding the promised fight.
            let mut nav = nav!(self);
            s.set_target(goal, &mut self.fighters, &mut nav);
        } else {
            let goal = l.anchor.unwrap_or(if target == NULLPOS { pos } else { target });
            let s = self.squads.get_mut(sid).unwrap();
            s.license = Some(l);
            s.low_shields = low_shields;
            s.engaging = false;
            if ready && s.status == 'r' {
                s.status = 'a';
            }
            s.target = goal;
            let mut nav = nav!(self);
            for id in ids {
                let Some(f) = self.fighters.get_mut(&id) else { continue };
                if self.retreat_handler.is_retreating(id) {
                    continue;
                }
                // Offset in a compact deterministic formation; no shared leader
                // shortcut can put the front member inside a firing envelope.
                let i = f.index as f32;
                let a = i * 2.399963;
                let r = 32.0 * (i + 1.0).sqrt().min(5.0);
                let p = Vec3::new(goal.x + r * a.cos(), goal.y, goal.z + r * a.sin());
                transit(f.unit, p, &mut nav);
            }
        }
        let _ = reason;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn forgotten_target_cache_does_not_erase_unobserved_firing_hazard() {
        let mut ai = crate::military::roles::retirement_tests::raider_test_ai(c"cloakraid");
        ai.war.license_enabled = true;
        ai.war
            .license_hazards
            .insert(91, Hazard { pos: Vec3::xz(1000.0, 1000.0), range: 600.0, seen: 60, identified: true });
        ai.war.targets.clear();
        ai.frame = 9000;
        ai.license_observe();
        assert!(remembered_threat(&ai.war, Vec3::xz(1500.0, 1000.0)) > 0.0);
        assert_eq!(remembered_threat(&ai.war, Vec3::xz(1900.0, 1000.0)), 0.0);
        let bytes = checkpoint::encode(&ai.war.license_hazards).unwrap();
        let restored: std::collections::BTreeMap<i32, Hazard> = checkpoint::rmp_serde::from_slice(&bytes).unwrap();
        assert_eq!(restored, ai.war.license_hazards);
    }

    #[test]
    fn accepts_attrition_but_never_free_damage_or_token_returns() {
        assert!(worthwhile(300.0, 220.0));
        assert!(!worthwhile(300.0, 20.0));
        assert!(!worthwhile(0.0, 0.0));
        assert!(!worthwhile(300.0, 0.0));
        assert!(!worthwhile(f32::NAN, 200.0));
    }
    #[test]
    fn a_retreating_kiter_cannot_pull_the_anchor_forward() {
        let origin = Vec3::xz(1000.0, 1000.0);
        let home = Vec3::xz(100.0, 1000.0);
        let held = stand_off(origin, Vec3::xz(1600.0, 1000.0), 800.0, home);
        assert_eq!(held, Vec3::xz(800.0, 1000.0));
        assert_eq!(stand_off(held, Vec3::xz(2000.0, 1000.0), 800.0, home), held);
        assert_eq!(stand_off(held, Vec3::xz(1400.0, 1000.0), 800.0, home), Vec3::xz(600.0, 1000.0));
    }
    #[test]
    fn transit_clips_crossings_and_permits_escape_only_downhill() {
        let a = Vec3::xz(0.0, 1.0);
        let b = Vec3::xz(1000.0, 1.0);
        let stop = safe_prefix(a, b, |p| if (300.0..=600.0).contains(&p.x) { 1.0 } else { 0.0 });
        assert!(stop.x < 300.0 && stop.x > 280.0);
        assert_eq!(safe_prefix(b, a, |p| p.x / 1000.0), a);
        assert_eq!(safe_prefix(a, b, |p| p.x / 1000.0), a);
    }
    #[test]
    fn licence_roundtrip_keeps_roster_deadlines_and_the_held_encounter() {
        let l = License {
            members: BTreeSet::from([3, 8]),
            enemies: BTreeSet::from([42]),
            until: 1234,
            next_check: 1000,
            anchor: Some(Vec3::xz(18.0, 29.0)),
            contact_at: 900,
            committed_at: 784,
            goal: Vec3::xz(800.0, 900.0),
            judgments: 9,
            grants: 2,
            refusals: 7,
        };
        let bytes = checkpoint::rmp_serde::to_vec_named(&l).unwrap();
        let restored: License = checkpoint::rmp_serde::from_slice(&bytes).unwrap();
        assert_eq!(l, restored);
        assert!(restored.permits(3, 1000));
        assert!(!restored.permits(9, 1000));
        assert!(!restored.permits(3, 1234));
        let old = b"\x80";
        assert!(checkpoint::rmp_serde::from_slice::<License>(old).is_err());
    }
}
