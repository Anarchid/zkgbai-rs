//! Opening heavies use Pack navigation with a fixed strategic destination.
//! Local probes may bend the route; sightings never replace the destination.
use super::*;
use crate::defs::cmd;
use crate::military::rush_push::rush_prize;
use crate::military::taskforce::Aim;
use springai::NO_TIMEOUT;

const PROBE_EVERY: i32 = 90;
const COMMIT_FOR: i32 = 240;
const STEP: f32 = 650.0;
const ARRIVED: f32 = 900.0;
const SHOT_FOR: i32 = 90;
const MOVE_BETWEEN_SHOTS: i32 = 90;
const REPAIR_FOR: i32 = 450;

#[derive(Clone, Debug, Default)]
pub(super) struct RushRun {
    waypoint: Option<Vec3>,
    committed: i32,
    probed: i32,
    retreat_since: Option<i32>,
    shot: Option<Shot>,
    next_shot: i32,
    driven: i32,
    hold: Option<RangeHold>,
    blocked: Option<(i32, i32)>,
}

#[derive(Clone, Debug)]
struct RangeHold {
    target: i32,
    hp: f32,
    progress: i32,
}

#[derive(Clone, Debug)]
struct Shot {
    target: i32,
    until: i32,
    weapon: usize,
    reload: i32,
}

struct Probe {
    at: Vec3,
    judgement: Judgement,
    detail: Vec<Value>,
    danger: f32,
    score: f32,
}

/// Before entering the start zone, every local endpoint makes strategic
/// progress. Inside it, sweep all directions for lasting damage.
fn directions(pos: Vec3, goal: Vec3, axis: (f32, f32)) -> Vec<Vec3> {
    let angles: &[f32] = if distance(pos, goal) > ARRIVED {
        &[-60.0, -30.0, 0.0, 30.0, 60.0]
    } else {
        &[0.0, 45.0, 90.0, 135.0, 180.0, 225.0, 270.0, 315.0]
    };
    angles
        .iter()
        .map(|a| {
            let (sin, cos) = a.to_radians().sin_cos();
            Vec3::xz(pos.x + STEP * (axis.0 * cos - axis.1 * sin), pos.z + STEP * (axis.0 * sin + axis.1 * cos))
        })
        .filter(|&p| distance(pos, goal) <= ARRIVED || distance(p, goal) < distance(pos, goal))
        .collect()
}

fn replace_route(age: i32, reached: bool, old_score: f32, fresh_score: f32, unsafe_old: bool) -> bool {
    reached || unsafe_old || (age >= COMMIT_FOR && fresh_score > old_score + old_score.abs().max(400.0) * 0.25)
}

// Equal forecasts do not justify continuing a lethal advance: the pass
// predictor can include the same nearby guns on both routes. Prefer repairs.
fn repair_needed(health: f32, danger: f32, home_danger: f32, worth: f32, cost: f32) -> bool {
    health < 0.3 || ((danger > 0.45 || (health < 0.8 && worth < -0.15 * cost)) && home_danger <= danger + 0.05)
}

fn repaired(health: f32, elapsed: i32) -> bool {
    health >= 0.9 && elapsed >= REPAIR_FOR
}

/// A prediction can retain escorts after losing the heavy. Read the heavy's
/// own fate, not merely the total number of survivors.
fn heavy_danger(detail: &[Value], id: i32) -> f32 {
    detail
        .iter()
        .filter_map(|d| {
            let index = d["ids_a"].as_array()?.iter().position(|v| v.as_i64() == Some(i64::from(id)))?;
            if d["death_a"][index].as_i64().is_some_and(|f| f >= 0) {
                return Some(1.0);
            }
            d["p_dead_a"][index].as_f64().map(|p| p as f32)
        })
        .fold(0.0, f32::max)
}

impl Ai {
    fn rush_probe(&mut self, sid: SquadId, heavy: Unit, goal: Vec3, at: Vec3, assault: bool) -> Option<Probe> {
        let pos = heavy.pos();
        let def = heavy.def()?;
        let map = self.cb.map();
        if at.x < 32.0
            || at.z < 32.0
            || at.x > map.width() as f32 * 8.0 - 32.0
            || at.z > map.height() as f32 * 8.0 - 32.0
            || (!def.is_able_to_fly() && !self.terrain_reaches(def, pos, at, 0.0))
        {
            return None;
        }
        let way = self.pack_way(sid, pos, heading(heavy.vel(), pos, at), at, assault);
        let way = if assault { self.rush_range_way(heavy, &way, None) } else { way };
        if way.is_empty() {
            return None;
        }
        let route: Vec<_> = std::iter::once(pos).chain(way).collect();
        let (judgement, detail) = self.pack_fates(sid, &route, false)?;
        let danger = heavy_danger(&detail, heavy.id());
        // Killing infrastructure dominates a small forward-progress bonus.
        // Zero-contact probes still advance instead of parking for a sighting.
        let progress = (distance(pos, goal) - distance(*route.last()?, goal)).clamp(-STEP, STEP);
        let score = judgement.worth() + 0.2 * progress;
        Some(Probe { at, judgement, detail, danger, score })
    }

    pub(super) fn taskforce_rush_pack(&mut self, sid: SquadId) -> (Option<Vec3>, String) {
        let Some(heavy) = self.squads.get(sid).and_then(|s| s.get_leader(&self.fighters)) else {
            return (None, "no rush core".into());
        };
        let Some(goal) = self.rush_push_target(heavy.id()) else { return (None, "no rush mission".into()) };
        let pos = heavy.pos();
        if let Some(at) = self.op_squad_target(sid) {
            self.pack_go(sid, pos, heading(heavy.vel(), pos, at), at, true);
            return (Some(at), "operator".into());
        }
        // A short Cyclops cannon correction already underway may finish;
        // this controller's bounded shots otherwise keep it from chasing.
        if self.cyclopes.holds(heavy.id(), self.frame) {
            return (Some(goal), "cannon correction".into());
        }
        let frame = self.frame;
        let health = health_fraction(heavy);
        let mut pack = self.taskforces.packs.remove(&sid).unwrap_or_default();
        let st = &mut pack.rush;
        let home = if heavy.def().is_some_and(|d| d.is_able_to_fly()) {
            self.graph_get_closest_air_haven(pos)
        } else {
            self.graph_get_closest_haven(pos)
        };
        if let Some(since) = st.retreat_since {
            if repaired(health, frame - since) {
                st.retreat_since = None;
                st.waypoint = None;
                st.probed = 0;
                st.driven = 0;
                self.op_emit("rush-pack", json!({"event":"repaired", "unit":heavy.id(), "mission":[goal.x,goal.z]}));
            }
        } else if health < 0.3 {
            st.retreat_since = Some(frame);
            st.driven = 0;
        }
        if st.retreat_since.is_none() && (frame - st.probed >= PROBE_EVERY || st.waypoint.is_none()) {
            let axis = heading(Vec3::ZERO, pos, goal);
            let old = st.waypoint.and_then(|at| self.rush_probe(sid, heavy, goal, at, true));
            let mut probes: Vec<_> = directions(pos, goal, axis)
                .into_iter()
                .filter_map(|at| self.rush_probe(sid, heavy, goal, at, true))
                .collect();
            // The strategic path supplies a way through terrain when local
            // angular probes are obstructed. It keeps the same destination.
            let strategic = self.pack_way(sid, pos, axis, goal, true);
            if let Some(at) = strategic.last().copied()
                && let Some(p) = self.rush_probe(sid, heavy, goal, at, true)
            {
                probes.push(p);
            }
            let scores: Vec<_> = probes
                .iter()
                .map(|p| {
                    json!({"at":[p.at.x,p.at.z],
                "gain":p.judgement.gain, "loss":p.judgement.loss, "heavy_danger":p.danger, "score":p.score})
                })
                .collect();
            let fresh = probes.into_iter().max_by(|a, b| a.score.total_cmp(&b.score));
            let selected = match (old, fresh) {
                (Some(old), Some(new))
                    if !replace_route(
                        frame - st.committed,
                        distance(pos, old.at) < 180.0,
                        old.score,
                        new.score,
                        old.danger > 0.45 && new.danger < old.danger,
                    ) =>
                {
                    Some(old)
                }
                (_, Some(new)) => Some(new),
                (old, None) => old,
            };
            if let Some(p) = selected {
                let retreat = if p.danger > 0.45 || (health < 0.8 && p.judgement.worth() < 0.0) {
                    self.rush_probe(sid, heavy, goal, home, false)
                } else {
                    None
                };
                let home_danger = retreat.as_ref().map_or(1.0, |r| r.danger);
                let cost = heavy.def().map_or(1.0, |d| d.cost(self.metal));
                if repair_needed(health, p.danger, home_danger, p.judgement.worth(), cost) {
                    st.retreat_since = Some(frame);
                    st.driven = 0;
                } else if st.waypoint != Some(p.at) {
                    st.waypoint = Some(p.at);
                    st.committed = frame;
                    st.driven = 0;
                }
                let verdict = if st.retreat_since.is_some() { "rush repairs" } else { "rush advances" };
                self.pack_emit_judgement(sid, p.at, &p.judgement, p.detail, verdict);
                self.op_emit("rush-pack", json!({"event":"probe", "unit":heavy.id(), "mission":[goal.x,goal.z],
                    "selected":[p.at.x,p.at.z], "retreat":st.retreat_since.is_some(), "home_danger":home_danger, "directions":scores}));
            } else {
                // Model unavailable or no local route: continue on the fixed
                // mission, with the usual health safety floor still applied.
                st.waypoint = Some(goal);
            }
            st.probed = frame;
        }
        pack.parked = st.retreat_since.is_some();
        let retreating = pack.parked || heavy.rules_param_float("disarmed", 0.0) > 0.0;
        let destination = if retreating { home } else { st.waypoint.unwrap_or(goal) };
        if retreating {
            if st.shot.take().is_some() {
                heavy.execute_custom_command(cmd::UNIT_CANCEL_TARGET, &[], 0, NO_TIMEOUT);
                st.driven = 0;
            }
            heavy.set_fire_state(0, 0);
        } else if self.rush_hold_range(sid, heavy, st) {
            pack.target = goal;
            self.taskforces.packs.insert(sid, pack);
            return (Some(goal), "rush fires from range".into());
        } else if self.rush_pause_for_shot(heavy, st, None) {
            pack.target = goal;
            self.taskforces.packs.insert(sid, pack);
            return (Some(goal), "rush firing pause".into());
        }
        // Repath at most every three seconds, or immediately after a shot or
        // route change. These are Pack raw-move waypoints, never fight orders.
        if frame - st.driven >= PROBE_EVERY || heavy.num_current_commands() == 0 || st.driven == 0 {
            let way = self.pack_way(sid, pos, heading(heavy.vel(), pos, destination), destination, !retreating);
            let way = if retreating {
                way
            } else {
                self.rush_range_way(heavy, &way, st.blocked.filter(|b| b.1 > frame).map(|b| b.0))
            };
            self.pack_drive(sid, &way);
            st.driven = frame;
        }
        pack.target = destination;
        self.taskforces.packs.insert(sid, pack);
        (
            if retreating { None } else { Some(goal) },
            if retreating { "rush repairs" } else { "rush start-zone advance" }.into(),
        )
    }

    /// Use the actual main damaging gun, not a fake weapon or a long-range
    /// utility beam, to decide where the heavy can stop and keep firing.
    fn rush_gun_range(&self, heavy: Unit) -> f32 {
        heavy
            .weapons()
            .into_iter()
            .filter_map(|w| {
                let d = w.def()?;
                if crate::military::water::is_fake(&d.name()) {
                    return None;
                }
                let damage = d.damage_types().into_iter().fold(0.0, f32::max);
                (damage > 0.0 && w.range() > 0.0).then_some((damage, w.range()))
            })
            .max_by(|a, b| a.0.total_cmp(&b.0))
            .map_or(0.0, |v| v.1)
    }

    fn rush_range_way(&self, heavy: Unit, way: &[Vec3], blocked: Option<i32>) -> Vec<Vec3> {
        let range = 0.9 * self.rush_gun_range(heavy);
        if range <= 0.0 {
            return way.to_vec();
        }
        let hits = self.war.reach(heavy.def());
        let mut out = way.to_vec();
        for e in self.war.targets.values().filter(|e| {
            !e.is_dead && e.visible && e.is_static && blocked != Some(e.unit_id) && self.war.can_hit(hits, e)
        }) {
            out = super::range::stop_at_range(heavy.pos(), &out, e.position, range);
        }
        out
    }

    fn rush_hold_range(&mut self, sid: SquadId, heavy: Unit, st: &mut RushRun) -> bool {
        let frame = self.frame;
        let reach = self.rush_gun_range(heavy);
        let hits = self.war.reach(heavy.def());
        let valid = |e: &crate::military::enemy::Enemy| {
            !e.is_dead
                && e.visible
                && e.is_static
                && self.war.can_hit(hits, e)
                && distance(heavy.pos(), e.position) <= 0.92 * reach
                && !st.blocked.is_some_and(|(id, until)| id == e.unit_id && frame < until)
        };
        let target = st
            .hold
            .as_ref()
            .and_then(|h| self.war.targets.get(&h.target))
            .filter(|e| valid(e))
            .or_else(|| self.war.targets.values().filter(|e| valid(e)).max_by(|a, b| a.value.total_cmp(&b.value)))
            .map(|e| (e.unit_id, e.position, e.get_health() * e.def_health()));
        let Some((id, at, hp)) = target else {
            st.hold = None;
            return false;
        };
        if let Some(h) = st.hold.as_mut().filter(|h| h.target == id) {
            if hp < h.hp - 1.0 {
                h.progress = frame;
            }
            h.hp = hp;
            if frame - h.progress > 450 {
                // A blocked shot must not pin the rush forever. A fresh
                // position may clear terrain; bounded shots still apply.
                st.blocked = Some((id, frame + 300));
                st.hold = None;
                return false;
            }
        } else {
            st.hold = Some(RangeHold { target: id, hp, progress: frame });
            self.op_emit("rush-pack",json!({"event":"range-hold","unit":heavy.id(),"target":id,"range":reach,"distance":distance(heavy.pos(),at)}));
        }
        st.driven = 0;
        heavy.set_move_state(0, 0);
        if distance(heavy.pos(), at) < 0.82 * reach {
            let angle = (heavy.pos().z - at.z).atan2(heavy.pos().x - at.x);
            let stand = Vec3::xz(at.x + 0.9 * reach * angle.cos(), at.z + 0.9 * reach * angle.sin());
            if heavy.def().is_some_and(|d| self.terrain_reaches(d, heavy.pos(), stand, 0.0)) {
                st.shot = None;
                heavy.execute_custom_command(cmd::UNIT_CANCEL_TARGET, &[], 0, NO_TIMEOUT);
                self.pack_go(sid, heavy.pos(), heading(heavy.vel(), heavy.pos(), stand), stand, false);
                return true;
            }
        }
        if !self.rush_pause_for_shot(heavy, st, Some(id)) {
            // Reload here. The old mandatory three-second advance spent
            // the entire range advantage between cannon shots.
            heavy.stop(0);
        }
        true
    }

    fn rush_pause_for_shot(&mut self, heavy: Unit, st: &mut RushRun, focus: Option<i32>) -> bool {
        let frame = self.frame;
        let weapons: Vec<_> = heavy
            .weapons()
            .into_iter()
            .filter(|w| {
                w.def().is_some_and(|d| {
                    !crate::military::water::is_fake(&d.name())
                        && d.range() > 0.0
                        && d.damage_types().iter().any(|&d| d > 0.0)
                })
            })
            .collect();
        let hits = self.war.reach(heavy.def());
        let valid = |e: &crate::military::enemy::Enemy| {
            !e.is_dead
                && focus.is_none_or(|id| e.unit_id == id)
                && e.visible
                && rush_prize(e.is_static, e.is_raider, e.value)
                && self.war.can_hit(hits, e)
                && weapons.iter().any(|w| distance(heavy.pos(), e.position) <= w.range() * 0.92)
        };
        if let Some(shot) = &st.shot {
            let fired = weapons.get(shot.weapon).is_none_or(|w| w.reload_frame() > shot.reload);
            let in_reach = self.war.targets.get(&shot.target).is_some_and(valid);
            if !fired && in_reach && frame < shot.until {
                return true;
            }
            st.shot = None;
            heavy.execute_custom_command(cmd::UNIT_CANCEL_TARGET, &[], 0, NO_TIMEOUT);
            st.next_shot = frame + MOVE_BETWEEN_SHOTS;
            st.driven = 0; // Replace the bounded attack immediately with movement.
            return false;
        }
        if frame < st.next_shot {
            return false;
        }
        let target = self
            .war
            .targets
            .values()
            .filter(|e| valid(e))
            .filter(|e| {
                weapons
                    .iter()
                    .any(|w| w.reload_frame() <= frame + 15 && distance(heavy.pos(), e.position) <= w.range() * 0.92)
            })
            .max_by(|a, b| {
                (a.value * if a.is_static { 2.5 } else { 1.0 })
                    .total_cmp(&(b.value * if b.is_static { 2.5 } else { 1.0 }))
            })
            .map(|e| e.unit);
        if let Some(target) = target {
            // Wait for the main ready gun, not a secondary beam that fires
            // first and would cancel the cannon's aiming pause.
            let weapon = weapons
                .iter()
                .enumerate()
                .filter(|(_, w)| {
                    w.reload_frame() <= frame + 15 && distance(heavy.pos(), target.pos()) <= w.range() * 0.92
                })
                .max_by(|(_, a), (_, b)| {
                    let damage = |w: &springai::Weapon| {
                        w.def().map_or(0.0, |d| d.damage_types().into_iter().fold(0.0, f32::max))
                    };
                    damage(a).total_cmp(&damage(b))
                })
                .map(|(i, _)| i)
                .unwrap_or(0);
            heavy.set_fire_state(0, 0);
            // An explicit ATTACK can path toward a clearer firing line even
            // on hold-position. Aim only the weapon while movement is stopped.
            heavy.stop(0);
            if heavy.execute_custom_command(cmd::UNIT_SET_TARGET, &[target.id() as f32], 0, NO_TIMEOUT) {
                st.shot = Some(Shot {
                    target: target.id(),
                    until: frame + SHOT_FOR,
                    weapon,
                    reload: weapons[weapon].reload_frame(),
                });
                self.op_emit(
                    "rush-pack",
                    json!({"event":"shot", "unit":heavy.id(), "target":target.id(), "until":frame+SHOT_FOR}),
                );
                return true;
            }
        }
        false
    }

    pub(super) fn taskforce_rush_guard(&mut self, sid: SquadId) -> (Option<Vec3>, String) {
        let Some(unit) = self.squads.get(sid).and_then(|s| s.get_leader(&self.fighters)) else {
            return (None, "no guard".into());
        };
        let Some(heavy) = self.rush_push_unit() else {
            if let Some(s) = self.squads.get_mut(sid) {
                Arc::make_mut(&mut s.shape).aim = Aim::Assault;
            }
            return (None, "rush ended; ordinary assault".into());
        };
        if self.retreat_is_retreating(unit) {
            return (None, "guard repairs".into());
        }
        if let Some(at) = self.op_squad_target(sid) {
            self.squad_set_target(sid, at);
            return (Some(at), "operator".into());
        }
        if health_fraction(unit) < 0.35 {
            self.retreat_send(unit);
            return (None, "guard repairs".into());
        }
        let home = self.graph.get_ally_base();
        let at = get_directional_point(heavy.pos(), home, 180.0);
        if unit.def().is_some_and(|d| self.terrain_reaches(d, unit.pos(), at, 0.0)) {
            self.squad_set_target(sid, at); // Riot guns handle the chaff the core ignores.
            (Some(at), "riot guards rush".into())
        } else {
            (None, "guard cannot follow terrain".into())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn advance_probes_cannot_turn_back_for_a_nearby_sighting() {
        let p = Vec3::xz(1000.0, 1000.0);
        let goal = Vec3::xz(6000.0, 1000.0);
        let dirs = directions(p, goal, (1.0, 0.0));
        assert_eq!(dirs.len(), 5);
        assert!(dirs.iter().all(|&d| distance(d, goal) < distance(p, goal) && d.x > p.x));
        assert_eq!(directions(goal, goal, (1.0, 0.0)).len(), 8);
    }
    #[test]
    fn small_score_changes_do_not_oscillate_a_committed_route() {
        assert!(!replace_route(90, false, 100.0, 500.0, false));
        assert!(!replace_route(300, false, 500.0, 550.0, false));
        assert!(replace_route(300, false, 500.0, 800.0, false));
        assert!(replace_route(30, false, 500.0, 450.0, true));
        assert!(replace_route(30, true, 500.0, 450.0, false));
    }
    #[test]
    fn repairs_use_survival_and_have_recovery_hysteresis() {
        assert!(repair_needed(0.9, 0.9, 0.1, 100.0, 2000.0));
        assert!(!repair_needed(0.9, 0.9, 1.0, 100.0, 2000.0));
        assert!(repair_needed(0.6, 1.0, 1.0, -1200.0, 4200.0));
        assert!(repair_needed(0.6, 0.08, 0.08, -300.0, 1600.0));
        assert!(repair_needed(0.2, 0.0, 0.0, 100.0, 2000.0));
        assert!(!repaired(0.7, 900));
        assert!(!repaired(1.0, 30));
        assert!(repaired(0.95, 450));
    }
    #[test]
    fn surviving_escorts_do_not_hide_a_dead_heavy() {
        let details = vec![json!({"ids_a":[1,2],"p_dead_a":[0.2,0.1],"death_a":[60,-1]})];
        assert_eq!(heavy_danger(&details, 1), 1.0);
        assert_eq!(heavy_danger(&details, 2), 0.1);
    }
}
