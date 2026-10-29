//! Organic fire squads. Ordinary assault squads keep their membership and
//! production, but a majority of anti-unit skirmishers can fight as a line:
//! long guns behind a screen, a common pace, and one retreat decision.
//! Opt in with `enable=firesquads`; disabled by default. The predictor's
//! mechanics model approves commitments; missing answers mean evade.

use serde_json::json;
use springai::{NO_TIMEOUT, Unit, Vec3};

use crate::Ai;
use crate::defs::cmd;
use crate::kgbutil::{PathCost, kgb_move_to};
use crate::military::{FighterStore, Squad, SquadId, SquadKind};
use crate::nav;
use crate::util::{NULLPOS, distance};

const MARGIN: f32 = 60.0;
pub(crate) const COMMIT_FRAMES: i32 = 300;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Mode {
    #[default]
    Assault,
    Kite,
    Evade,
    Commit,
    Recover,
}

#[derive(Clone, Debug, Default)]
pub struct FireState {
    pub mode: Mode,
    pub cores: Vec<i32>,
    pub range: f32,
    pub commit_until: i32,
    pub focus: Option<i32>,
}

impl Mode {
    pub fn name(self) -> &'static str {
        match self {
            Self::Assault => "assault",
            Self::Kite => "kiting",
            Self::Evade => "evading",
            Self::Commit => "committing",
            Self::Recover => "recovering",
        }
    }
}

impl FireState {
    pub fn active(&self) -> bool {
        self.mode != Mode::Assault
    }
}

/// Anti-unit long guns, including Firewalker but excluding siege artillery.
/// Range alone would make a Sling or an Impaler the wrong kind of core.
pub(crate) fn core_type(name: &str) -> bool {
    matches!(
        name,
        "cloakskirm"
            | "shieldskirm"
            | "hoverskirm"
            | "spiderskirm"
            | "jumpskirm"
            | "shipskirm"
            | "amphfloater"
            | "jumparty"
            | "hoverarty"
            | "cloaksnipe"
            | "spidercrabe"
            | "amphassault"
    )
}

/// Longest range backed by a majority of the squad's metal, not one long
/// gun among many short ones. Hysteresis keeps casualties from toggling it.
fn core_range(total: f32, mut guns: Vec<(f32, f32)>, active: bool) -> Option<f32> {
    let needed = total * if active { 0.45 } else { 0.55 };
    if total <= 0.0 {
        return None;
    }
    guns.sort_by(|a, b| b.0.total_cmp(&a.0));
    let mut value = 0.0;
    for (range, metal) in guns {
        value += metal;
        if value >= needed && range >= 400.0 {
            return Some(range);
        }
    }
    None
}

#[derive(Clone, Copy)]
struct Contact {
    id: i32,
    pos: Vec3,
    range: f32,
    value: f32,
    kiteable: bool,
}

struct Member {
    id: i32,
    unit: Unit,
    value: f32,
    range: f32,
    skirmisher: bool,
    health: f32,
    speed: f32,
}

impl Contact {
    // The spacing margin is not a range advantage. Equal-range enemies
    // can trade with us; they cannot kite our cores without taking fire.
    fn outranges(&self, range: f32) -> bool {
        !self.kiteable || self.range > range
    }

    fn exposure(&self, pos: Vec3, range: f32, front: f32) -> f32 {
        let danger = self.range + MARGIN + front;
        // Against guns we can answer, do not buy safety by walking out
        // of our own firing envelope. This includes equal-range mirrors.
        let danger = if self.outranges(range) { danger } else { danger.min(firing_distance(range)) };
        self.value * ((danger - distance(self.pos, pos)) / danger.max(1.0)).max(0.0)
    }
}

fn firing_distance(range: f32) -> f32 {
    range - 75.0
}

fn outranged(contacts: &[Contact], range: f32, was_evading: bool) -> bool {
    let total: f32 = contacts.iter().map(|e| e.value).sum();
    let long: f32 = contacts.iter().filter(|e| e.outranges(range)).map(|e| e.value).sum();
    total > 0.0 && long >= total * if was_evading { 0.20 } else { 0.30 }
}

fn choose_mode(old: Mode, hurt: bool, recovered: bool, out: bool, favorable: bool, committed: bool) -> Mode {
    // Commit means finishing the attack window even through losses, as
    // with packs. Health is judged again after it, or when its target goes.
    if old == Mode::Commit && committed {
        Mode::Commit
    } else if hurt || (old == Mode::Recover && !recovered) {
        Mode::Recover
    } else if out {
        if favorable { Mode::Commit } else { Mode::Evade }
    } else {
        Mode::Kite
    }
}

fn toward(from: Vec3, to: Vec3, step: f32) -> Vec3 {
    let d = distance(from, to);
    if d < 1.0 {
        return from;
    }
    let t = (step / d).min(1.0);
    Vec3::xz(from.x + (to.x - from.x) * t, from.z + (to.z - from.z) * t)
}

/// A bounded line: cores behind the shorter ranged screen. The line's
/// front never advances beyond the chosen target's safe firing envelope.
fn slot(anchor: Vec3, facing: Vec3, index: usize, count: usize, front: f32) -> Vec3 {
    let d = distance(anchor, facing).max(1.0);
    let (x, z) = ((facing.x - anchor.x) / d, (facing.z - anchor.z) / d);
    let columns = count.min(7);
    let row = index / columns.max(1);
    let row_count = (count - row * columns).min(columns);
    let lateral = ((index % columns.max(1)) as f32 - row_count.saturating_sub(1) as f32 / 2.0) * 65.0;
    let front = front - row as f32 * 65.0;
    Vec3::xz(anchor.x + x * front - z * lateral, anchor.z + z * front + x * lateral)
}

pub(crate) fn release_unit(unit: Unit) {
    unit.set_move_state(1, 0);
    unit.execute_custom_command(cmd::UNIT_AI, &[1.0], 0, NO_TIMEOUT);
    unit.execute_custom_command(cmd::WANTED_SPEED, &[unit.max_speed() * 30.0], 0, NO_TIMEOUT);
}

impl Squad {
    pub(crate) fn release_fire(&mut self, fighters: &FighterStore) {
        if self.fire.active() {
            for f in self.fighters.iter().filter_map(|id| fighters.get(id)) {
                release_unit(f.unit);
            }
            self.fire = FireState::default();
        }
    }
}

impl Ai {
    pub(crate) fn fire_release(&mut self, sid: SquadId) {
        if let Some(s) = self.squads.get_mut(sid) {
            s.release_fire(&self.fighters);
        }
    }

    /// Damage/shield callbacks leave these units with the squad. Recovery
    /// is decided below for the whole formation, never one gun at a time.
    pub(crate) fn fire_controls(&self, id: i32) -> bool {
        !self.dev_off("firesquads")
            && self.fighters.get(&id).and_then(|f| f.squad).and_then(|sid| self.squads.get(sid)).is_some_and(|s| {
                s.fire.active() && s.status == 'a' && !self.qrf_drafted(s.id) && self.op_squad_target(s.id).is_none()
            })
    }

    /// Returns true when this tactical controller owns the squad's orders.
    pub(crate) fn fire_assign(&mut self, sid: SquadId) -> bool {
        let Some(s) = self.squads.get(sid) else {
            return false;
        };
        if self.dev_off("firesquads") || s.kind != SquadKind::Assault || s.is_air_squad {
            self.fire_release(sid);
            return false;
        }
        let old = s.fire.clone();
        let members: Vec<_> = s
            .fighters
            .iter()
            .filter_map(|id| self.fighters.get(id))
            .filter(|f| !f.is_dead() && !self.retreat_handler.is_retreating(f.id))
            .filter_map(|f| {
                let d = f.unit.def()?;
                let hp = ((f.unit.health() - f.unit.paralyze_damage()) / f.unit.max_health().max(1.0)
                    * (1.0 - f.unit.capture_progress()))
                .clamp(0.0, 1.0);
                Some(Member {
                    id: f.id,
                    unit: f.unit,
                    value: f.metal_value,
                    range: d.max_weapon_range(),
                    skirmisher: core_type(self.defs.name(d)),
                    health: hp,
                    speed: d.speed(),
                })
            })
            .collect();
        let total: f32 = members.iter().map(|m| m.value).sum();
        let guns = members.iter().filter(|m| m.skirmisher).map(|m| (m.range, m.value)).collect();
        let Some(range) = core_range(total, guns, old.active()) else {
            self.fire_release(sid);
            return false;
        };
        let cores: Vec<i32> = members.iter().filter(|m| m.skirmisher && m.range >= range).map(|m| m.id).collect();
        let mut anchor = Vec3::ZERO;
        for m in members.iter().filter(|m| cores.contains(&m.id)) {
            anchor.x += m.unit.pos().x / cores.len() as f32;
            anchor.z += m.unit.pos().z / cores.len() as f32;
        }
        let leader = members
            .iter()
            .filter(|m| cores.contains(&m.id))
            .min_by(|a, b| distance(a.unit.pos(), anchor).total_cmp(&distance(b.unit.pos(), anchor)))
            .expect("a supported core range has cores")
            .unit;
        let reach = self.war.reach(leader.def());
        let wet = self.war.water_depth(anchor) > 0.0;
        let contacts: Vec<Contact> = self
            .war
            .targets
            .values()
            .filter(|e| {
                !e.is_dead
                    && e.identified
                    && !e.is_aa
                    && e.can_attack()
                    && (e.surface_danger(e.is_submerged()) > 0.0 || (wet && e.deep_danger() > 0.0))
                    && !e.is_super_wep
                    && e.position != NULLPOS
                    && (e.is_static || self.frame - e.last_seen <= 150)
                    && e.def_max_weapon_range() > 0.0
                    && distance(e.position, anchor) <= range.max(e.def_max_weapon_range()) + 600.0
            })
            .map(|e| Contact {
                id: e.unit_id,
                pos: e.position,
                range: e.def_max_weapon_range(),
                value: e.value,
                kiteable: !e.can_fly() && self.war.can_hit(reach, e),
            })
            .collect();
        let out = outranged(&contacts, range, old.mode == Mode::Evade);
        // With no contact a newly formed squad still adopts ordinary
        // assault targeting; an existing line stays together in transit.
        if !old.active() && (contacts.is_empty() || (!out && !contacts.iter().any(|e| !e.outranges(range)))) {
            return false;
        }
        let health = members.iter().map(|m| m.value * m.health).sum::<f32>() / total.max(1.0);
        let core_health = members.iter().filter(|m| cores.contains(&m.id)).map(|m| m.health).fold(1.0, f32::min);
        let hurt = health < 0.60 || core_health < 0.40;
        let recovered = health > 0.85 && core_health > 0.80;
        let enemy_ids: Vec<_> = contacts.iter().map(|e| e.id).collect();
        let favorable = out && self.fire_commit_favorable(sid, &enemy_ids);
        let focus_alive = old.focus.is_some_and(|id| contacts.iter().any(|e| e.id == id));
        let committed = self.frame < old.commit_until && focus_alive;
        let mode = choose_mode(old.mode, hurt, recovered, out, favorable, committed);
        let target = contacts
            .iter()
            .filter(|e| e.kiteable && (mode != Mode::Kite || !e.outranges(range)))
            .min_by(|a, b| distance(a.pos, anchor).total_cmp(&distance(b.pos, anchor)))
            .copied();
        let target = if mode == Mode::Commit && committed {
            contacts.iter().find(|e| Some(e.id) == old.focus).copied().or(target)
        } else {
            target
        };
        let goal = if mode == Mode::Recover {
            self.graph_get_closest_haven(anchor)
        } else if mode == Mode::Evade {
            // Back away from the nearest long gun; candidate scoring below
            // also accounts for guns on our flanks and behind us.
            let danger = contacts
                .iter()
                .filter(|e| e.outranges(range))
                .min_by(|a, b| distance(a.pos, anchor).total_cmp(&distance(b.pos, anchor)));
            danger.map_or_else(
                || self.graph_get_closest_haven(anchor),
                |e| Vec3::xz(anchor.x + anchor.x - e.pos.x, anchor.z + anchor.z - e.pos.z),
            )
        } else if let Some(e) = target {
            if mode == Mode::Commit {
                e.pos
            } else {
                let d = distance(e.pos, anchor).max(1.0);
                Vec3::xz(
                    e.pos.x + (anchor.x - e.pos.x) / d * firing_distance(range),
                    e.pos.z + (anchor.z - e.pos.z) / d * firing_distance(range),
                )
            }
        } else {
            self.incursion_block(sid).unwrap_or_else(|| self.war_get_target(leader, true))
        };
        let focus = target.map(|e| e.id);
        let commit_until = if mode == Mode::Commit && (old.mode != mode || !committed) {
            self.frame + COMMIT_FRAMES
        } else {
            old.commit_until
        };
        if old.mode != mode {
            self.op_emit(
                "firesquad",
                json!({"squad":sid,"mode":format!("{mode:?}"),"range":range,
                "cores":cores,"outranged":out,"favorable":favorable,"health":health}),
            );
        }
        if let Some(s) = self.squads.get_mut(sid) {
            s.fire = FireState { mode, cores: cores.clone(), range, commit_until, focus };
            s.target = goal;
            s.leader = Some(leader.id());
        }
        let speed = members.iter().map(|m| m.speed).filter(|s| *s > 0.0).fold(f32::MAX, f32::min);
        let step = (speed * 1.5).clamp(45.0, 180.0);
        let scattered = members.iter().any(|m| distance(m.unit.pos(), anchor) > 450.0);
        let facing = target.map_or(goal, |e| e.pos);
        let front = if matches!(mode, Mode::Kite | Mode::Evade) {
            target.map_or(100.0, |e| (distance(anchor, e.pos) - e.range - MARGIN).clamp(0.0, 150.0))
        } else {
            80.0
        };
        let map = self.cb.map();
        let (width, height) = (map.width() as f32 * 8.0, map.height() as f32 * 8.0);
        let clamp = |p: Vec3| Vec3::xz(p.x.clamp(8.0, (width - 8.0).max(8.0)), p.z.clamp(8.0, (height - 8.0).max(8.0)));
        let goal = clamp(goal);
        let nav = nav!(self);
        // Shared anchor follows a real path. Use a short reachable step
        // and score exposure before translating the entire formation.
        let cost =
            if matches!(mode, Mode::Evade | Mode::Recover) { PathCost::AvoidEnemies } else { PathCost::AssaultPath };
        let path = nav.pathfinder.find_path(nav.war, leader, goal, cost);
        let waypoint = path.iter().find(|p| distance(**p, anchor) > 40.0).copied().unwrap_or(goal);
        let mut next = clamp(toward(anchor, waypoint, step));
        if mode == Mode::Kite && scattered {
            next = anchor;
        }
        if matches!(mode, Mode::Kite | Mode::Evade) && !contacts.is_empty() {
            let exposure = |p: Vec3| contacts.iter().map(|e| e.exposure(p, range, front)).sum::<f32>();
            let score = |p: Vec3| exposure(p) * 20.0 + distance(p, next);
            let mut best = anchor;
            let mut best_score = score(best);
            if score(next) < best_score && nav.pathfinder.is_reachable(nav.war, leader, next, cost) {
                best = next;
                best_score = score(next);
            }
            for i in 0..16 {
                let angle = i as f32 * std::f32::consts::TAU / 16.0;
                let p = clamp(Vec3::xz(anchor.x + step * angle.cos(), anchor.z + step * angle.sin()));
                if score(p) < best_score && nav.pathfinder.is_reachable(nav.war, leader, p, cost) {
                    best = p;
                    best_score = score(p);
                }
            }
            next = best;
        }
        let mut ci = 0;
        let mut ei = 0;
        for member in &members {
            let (id, unit) = (&member.id, &member.unit);
            let core = cores.contains(id);
            let (index, count, depth) = if core {
                let i = ci;
                ci += 1;
                (i, cores.len(), 0.0)
            } else {
                let i = ei;
                ei += 1;
                (i, members.len() - cores.len(), front)
            };
            let dest = clamp(slot(next, facing, index, count, depth));
            unit.set_move_state(0, 0);
            unit.set_fire_state(2, 0);
            unit.execute_custom_command(cmd::UNIT_AI, &[0.0], 0, NO_TIMEOUT);
            let pace = if mode == Mode::Commit || distance(unit.pos(), anchor) > 400.0 {
                unit.max_speed() * 30.0
            } else {
                speed * 0.90
            };
            unit.execute_custom_command(cmd::WANTED_SPEED, &[pace], 0, NO_TIMEOUT);
            if let Some(f) = self.fighters.get_mut(id) {
                f.target = dest;
                f.out_of_range = distance(unit.pos(), dest) > 200.0;
            }
            if mode == Mode::Commit {
                // With unit AI disabled, fight orders close and fire;
                // individual health/shield retreat is suppressed above.
                let mut path = nav.pathfinder.find_path(nav.war, *unit, goal, PathCost::AssaultPath);
                if let Some(p) = crate::military::wrappers::fighter::next_waypoint(&mut path, 1) {
                    crate::military::wrappers::fighter::fight_clear(*unit, p, 0, &nav);
                }
            } else if distance(unit.pos(), dest) > 25.0 {
                let mut path = nav.pathfinder.find_path(nav.war, *unit, dest, cost);
                if let Some(p) = crate::military::wrappers::fighter::next_waypoint(&mut path, 1) {
                    kgb_move_to(*unit, p, 0, nav.frame);
                }
            } else {
                unit.stop(0);
            }
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn anti_static_artillery_cannot_anchor_a_fire_squad() {
        for name in ["vehheavyarty", "cloakarty", "veharty", "tankarty", "tankheavyarty", "cloakaa"] {
            assert!(!core_type(name), "{name}");
        }
        for name in ["jumparty", "cloakskirm", "spiderskirm", "hoverarty"] {
            assert!(core_type(name));
        }
    }
    #[test]
    fn range_must_have_metal_behind_it() {
        assert_eq!(core_range(1000.0, vec![(900.0, 100.0), (600.0, 600.0)], false), Some(600.0));
        assert_eq!(core_range(1000.0, vec![(900.0, 100.0)], false), None);
        assert_eq!(core_range(1000.0, vec![(600.0, 500.0)], false), None);
        assert_eq!(core_range(1000.0, vec![(600.0, 500.0)], true), Some(600.0));
    }
    #[test]
    fn enemy_value_not_count_and_hysteresis_decide_range_threat() {
        let contact = |range, value| Contact { id: 1, pos: Vec3::ZERO, range, value, kiteable: true };
        let mut es = vec![contact(300.0, 700.0), contact(900.0, 300.0)];
        assert!(outranged(&es, 600.0, false));
        es[1].value = 200.0;
        assert!(!outranged(&es, 600.0, false));
        assert!(outranged(&es, 600.0, true));
    }
    #[test]
    fn equal_range_trades_instead_of_evading_even_after_a_longer_threat_leaves() {
        let mirror = Contact { id: 1, pos: Vec3::ZERO, range: 570.0, value: 1000.0, kiteable: true };
        for was_evading in [false, true] {
            assert!(!outranged(&[mirror], 570.0, was_evading));
            assert!(!outranged(&[Contact { range: 540.0, ..mirror }], 570.0, was_evading));
            assert!(outranged(&[Contact { range: 620.0, ..mirror }], 570.0, was_evading));
        }
        assert_eq!(choose_mode(Mode::Evade, false, true, false, false, false), Mode::Kite);
        // The same predicate admits a mirror as a firing target. A blind
        // spot (e.g. aircraft our guns cannot hit) still demands evasion.
        assert!(!mirror.outranges(570.0));
        assert!(Contact { kiteable: false, ..mirror }.outranges(570.0));
    }

    #[test]
    fn equal_range_movement_prefers_firing_distance_over_yielding_ground() {
        let enemy = Contact { id: 1, pos: Vec3::ZERO, range: 570.0, value: 1000.0, kiteable: true };
        let next = Vec3::xz(firing_distance(570.0), 0.0);
        let score = |p| enemy.exposure(p, 570.0, 100.0) * 20.0 + distance(p, next);
        assert_eq!(enemy.exposure(next, 570.0, 100.0), 0.0);
        assert!(score(next) < score(Vec3::xz(570.0, 0.0)));
        assert!(score(next) < score(Vec3::xz(700.0, 0.0)));
        assert!(enemy.exposure(Vec3::xz(300.0, 0.0), 570.0, 100.0) > 0.0);
        assert!(Contact { range: 900.0, ..enemy }.exposure(next, 570.0, 100.0) > 0.0);
    }

    #[test]
    fn commitment_finishes_before_collective_recovery() {
        assert_eq!(choose_mode(Mode::Kite, false, true, true, false, false), Mode::Evade);
        assert_eq!(choose_mode(Mode::Evade, false, true, true, true, false), Mode::Commit);
        assert_eq!(choose_mode(Mode::Commit, false, true, false, false, true), Mode::Commit);
        assert_eq!(choose_mode(Mode::Commit, true, false, true, true, true), Mode::Commit);
        assert_eq!(choose_mode(Mode::Commit, true, false, true, true, false), Mode::Recover);
        assert_eq!(choose_mode(Mode::Recover, false, false, false, false, false), Mode::Recover);
        assert_eq!(choose_mode(Mode::Recover, false, true, false, false, false), Mode::Kite);
        assert_eq!(choose_mode(Mode::Commit, false, true, true, false, false), Mode::Evade);
    }
    #[test]
    fn screen_faces_enemy_and_coincident_points_are_finite() {
        let a = Vec3::xz(500.0, 500.0);
        let e = Vec3::xz(1000.0, 500.0);
        assert!(slot(a, e, 0, 1, 100.0).x > slot(a, e, 0, 1, 0.0).x);
        assert_eq!(toward(a, a, 100.0), a);
        assert!(slot(a, a, 0, 2, 100.0).x.is_finite());
    }
}

#[cfg(test)]
mod lifecycle_tests {
    use super::*;
    use crate::military::{
        Fighter,
        wrappers::squad::fake_engine::{self, TEAM},
    };

    #[test]
    fn leaving_and_dissolving_restore_unit_controls() {
        let cb = fake_engine::callback();
        let mut fighters = FighterStore::new();
        let mut squad = Squad::new(100, SquadKind::Assault, TEAM);
        for id in [701, 702, 703] {
            fighters.insert(id, Fighter::new(fake_engine::spawn(cb, id, 7), 300.0, TEAM));
            squad.add_unit(id, &mut fighters, 0);
        }
        squad.fire.mode = Mode::Commit;
        fake_engine::take_orders();
        squad.remove_unit(701, &mut fighters, 10);
        let orders = fake_engine::take_orders();
        assert!(orders.iter().any(|o| o.0 == 701 && o.2 == cmd::UNIT_AI));
        assert!(orders.iter().any(|o| o.0 == 701 && o.2 == cmd::WANTED_SPEED));
        assert!(squad.fire.active());
        squad.cutoff(&mut fighters);
        assert!(!squad.fire.active());
        assert!(squad.fighters.is_empty());
        let orders = fake_engine::take_orders();
        for id in [702, 703] {
            assert_eq!(fighters[&id].squad, None);
            assert!(orders.iter().any(|o| o.0 == id && o.2 == cmd::UNIT_AI));
            assert!(orders.iter().any(|o| o.0 == id && o.2 == cmd::WANTED_SPEED));
        }
    }
}
