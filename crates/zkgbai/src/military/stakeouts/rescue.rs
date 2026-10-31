//! Damage-triggered stakeout recruitment by time to save the victim.
//! This is a sustained-fire estimate, not a combat simulation: the attacker
//! keeps firing at this victim, and the rescuer can fire once it arrives.

use serde_json::json;
use springai::{Unit, UnitDef};

use super::economy_damage_target;
use crate::Ai;
use crate::military::unit_classes::UnitClasses;
use crate::military::water::{Medium, is_fake};
use crate::util::{NULLPOS, distance};

const RESPONSE_MARGIN: f32 = 1.0;
const RECHECK: i32 = 15;

fn rescue_route(gap: f32, range: f32, query: impl FnOnce() -> f32) -> f32 {
    if gap <= range {
        return gap;
    }
    let path = query();
    // Do not let max(gap) turn the pathfinder's -1 (unreachable) into
    // a valid straight-line route. Grid rounding can only shorten a real path.
    if path.is_finite() && path >= 0.0 { path.max(gap) } else { -1.0 }
}

fn rescue_due(checked: &mut std::collections::BTreeMap<(i32, i32), i32>, pair: (i32, i32), frame: i32) -> bool {
    checked.retain(|_, since| frame - *since < RECHECK);
    if checked.contains_key(&pair) {
        return false;
    }
    checked.insert(pair, frame);
    true
}

fn rescue_raider(classes: &UnitClasses, name: &str, tooltip: &str) -> bool {
    classes.small_raiders.contains(name)
        || classes.medium_raiders.contains(name)
        || classes.solo_raiders.contains(name)
        || tooltip.contains("aider")
        || tooltip.contains("cout")
}

#[derive(Clone, Copy, Debug)]
struct Firepower {
    dps: f32,
    /// All counted weapons can fire at this range.
    range: f32,
}

/// HP damage against the actual armor class. Threat power also counts EMP,
/// slow, impulse and AoE; those must not turn into fictitious HP damage.
fn firepower(shooter: UnitDef, target: UnitDef, medium: Medium) -> Firepower {
    let mut result = Firepower { dps: 0.0, range: f32::INFINITY };
    for mount in shooter.weapon_mounts() {
        if mount.only_target_category() & target.category() == 0 {
            continue;
        }
        let Some(w) = mount.weapon_def() else { continue };
        if w.is_shield() || w.is_paralyzer() || w.is_manual_fire() || is_fake(&w.name()) {
            continue;
        }
        if medium == Medium::Submerged && !w.is_water_weapon()
            || medium == Medium::Dry && w.weapon_type() == "TorpedoLauncher" && !w.is_sub_missile()
        {
            continue;
        }
        let effects = w.custom_params();
        if effects
            .iter()
            .any(|(k, v)| matches!(k.as_str(), "disarmDamageOnly" | "timeslow_only") && v != "0" && v != "false")
        {
            continue;
        }
        let damages = w.damage_types();
        let damage = damages.get(target.armor_type().max(0) as usize).or(damages.first()).copied().unwrap_or(0.0);
        let dps = damage * w.salvo_size().max(0) as f32 * w.projectiles_per_shot().max(0) as f32 / w.reload();
        let range = w.range();
        if dps.is_finite() && dps > 0.0 && range.is_finite() && range > 0.0 {
            result.dps += dps;
            result.range = result.range.min(range);
        }
    }
    result
}

/// Terrain cost is in elmos, speed in elmos/second. Invalid or unreachable
/// routes never qualify; neither do unarmed responders or an infinite
/// survival time caused by zero incoming HP damage.
fn rescue_time(path: f32, speed: f32, gun: Firepower, attacker_hp: f32, survival: f32) -> Option<(f32, f32)> {
    if ![path, speed, gun.dps, gun.range, attacker_hp, survival].iter().all(|v| v.is_finite())
        || path < 0.0
        || speed <= 0.0
        || gun.dps <= 0.0
        || gun.range <= 0.0
        || attacker_hp <= 0.0
        || survival <= 0.0
    {
        return None;
    }
    let travel = (path - gun.range).max(0.0) / speed;
    let kill = attacker_hp / gun.dps;
    (travel + kill + RESPONSE_MARGIN < survival).then_some((travel, kill))
}

impl Ai {
    pub(crate) fn stakeouts_economy_damaged(&mut self, victim: Unit, attacker: Option<Unit>, damage: f32) {
        if self.dev_off("stakeoutqrf")
            || self.dev_off("stakeouts")
            || self.dev_off("qrf")
            || !self.qrf.on
            || !self.qrf.live
            || !damage.is_finite()
            || damage <= 0.0
            || victim.team() != self.team_id
            || self.stakeouts.members.is_empty()
        {
            return;
        }
        let Some(victim_def) = victim.def() else { return };
        if !economy_damage_target(self.defs.name(victim_def), victim_def.build_speed()) {
            return;
        }
        let Some(attacker) = attacker else { return };
        if attacker.ally_team() == self.ally_team_id {
            return;
        }
        let contact = self.war.targets.get(&attacker.id());
        if contact.is_some_and(|e| e.is_dead) {
            return;
        }
        let Some(attacker_def) = attacker.def().or_else(|| contact.and_then(|e| e.ud)) else { return };
        if attacker_def.speed() <= 0.0
            || attacker_def.is_able_to_fly()
            || !rescue_raider(&self.unit_classes, self.defs.name(attacker_def), &attacker_def.tooltip())
        {
            return;
        }
        let enemy_pos = attacker.pos();
        let target = if enemy_pos != NULLPOS { enemy_pos } else { contact.map_or(NULLPOS, |e| e.position) };
        let at = victim.pos();
        if target == NULLPOS || at == NULLPOS {
            return;
        }
        let attacker_hp = contact.map_or_else(|| attacker.health(), |e| e.current_health());
        let attacker_hp = if attacker_hp > 0.0 { attacker_hp } else { attacker_def.health() };
        // The first damage event responds immediately. Further projectiles
        // in the burst must not launch identical terrain searches every frame.
        let frame = self.frame;
        if !rescue_due(&mut self.stakeouts.rescue_checked, (victim.id(), attacker.id()), frame) {
            return;
        }
        let victim_medium = self.war.dive(Some(victim_def)).medium(self.war.water_depth(at));
        let attacker_medium = self.war.dive(Some(attacker_def)).medium(self.war.water_depth(target));
        let incoming = firepower(attacker_def, victim_def, victim_medium).dps;
        let survival = victim.health() / incoming;
        if !survival.is_finite() || survival <= 0.0 {
            return;
        }
        let candidates: Vec<_> = self
            .stakeouts
            .members
            .values()
            .filter_map(|m| {
                let u = m.f.unit;
                (u.team() == self.team_id
                    && !m.f.is_dead()
                    && !u.is_being_built()
                    && !u.is_paralyzed()
                    && u.rules_param_float("disarmed", 0.0) <= 0.0
                    && !springai::reserved::is_reserved(self.cb, u.id())
                    && !self.retreat_is_retreating(u))
                .then_some((u, m.def))
            })
            .collect();
        let mut units = Vec::new();
        let mut estimates = Vec::new();
        for (u, def) in candidates {
            let pos = u.pos();
            if pos == NULLPOS {
                continue;
            }
            let gun = firepower(def, attacker_def, attacker_medium);
            let slow = u.rules_param_float("slowState", 0.0).clamp(0.0, 0.5);
            let speed = def.speed() * (1.0 - slow);
            let gun = Firepower { dps: gun.dps * (1.0 - slow), ..gun };
            let gap = distance(pos, target);
            // Cheap optimistic bound before the terrain search. Already in
            // weapon range needs no walking, even across impassable terrain.
            if rescue_time(gap, speed, gun, attacker_hp, survival).is_none() {
                continue;
            }
            let path = rescue_route(gap, gun.range, || self.pathfinder.get_path_cost(&self.war, pos, target, def));
            let Some((travel, kill)) = rescue_time(path, speed, gun, attacker_hp, survival) else { continue };
            units.push(u);
            estimates.push(json!({"unit": u.id(), "travel_s": travel, "kill_s": kill}));
        }
        let Some(team) = self.qrf_draft_units(&units) else { return };
        let ids: Vec<_> = units.iter().map(|u| u.id()).filter(|id| self.qrf.members.contains_key(id)).collect();
        self.qrf_order_team(&ids, target, true);
        self.op_emit(
            "stakeout",
            json!({
                "event": "damage_qrf", "t": frame / 30, "units": ids, "team": team,
                "victim": victim.id(), "attacker": attacker.id(), "survival_s": survival,
                "incoming_dps": incoming, "estimates": estimates,
            }),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dagger() -> Firepower {
        Firepower { dps: 110.01 / (2.8 + 1.0 / 30.0), range: 212.0 }
    }

    #[test]
    fn daggers_are_raiders_even_without_raider_or_scout_in_the_tooltip() {
        let classes = UnitClasses::new();
        assert!(rescue_raider(&classes, "hoverraid", "Fast Attack Hovercraft (Anti-Sub)"));
        assert!(!rescue_raider(&classes, "hovercon", "Construction Hovercraft"));
    }

    #[test]
    fn fast_daggers_can_rescue_a_quill_from_well_outside_the_old_radius() {
        let gun = dagger();
        let survival = (960.0 - 110.01) / gun.dps;
        for path in [1000.0, 1500.0, 2000.0] {
            assert!(rescue_time(path, 144.0, gun, 300.0, survival).is_some());
        }
        assert!(rescue_time(2400.0, 144.0, gun, 300.0, survival).is_none());
        assert!(rescue_time(2000.0, 60.0, gun, 300.0, survival).is_none());
    }

    #[test]
    fn victim_health_incoming_dps_and_kill_time_all_limit_the_rescue() {
        let gun = dagger();
        assert!(rescue_time(1500.0, 144.0, gun, 300.0, 850.0 / gun.dps).is_some());
        assert!(rescue_time(1500.0, 144.0, gun, 300.0, 200.0 / gun.dps).is_none());
        assert!(rescue_time(1500.0, 144.0, gun, 300.0, 850.0 / 150.0).is_none());
        assert!(rescue_time(1500.0, 144.0, gun, 1500.0, 850.0 / gun.dps).is_none());
        // A nearly dead attacker can still be killed in time.
        assert!(rescue_time(212.0, 144.0, gun, 10.0, 200.0 / gun.dps).is_some());
    }

    #[test]
    fn terrain_detours_and_invalid_estimates_do_not_recruit() {
        let gun = dagger();
        assert!(rescue_time(1500.0, 144.0, gun, 300.0, 22.0).is_some());
        for path in [-1.0, 3000.0, f32::INFINITY, f32::NAN] {
            assert!(rescue_time(path, 144.0, gun, 300.0, 22.0).is_none());
        }
        assert!(rescue_time(0.0, 144.0, Firepower { dps: 0.0, ..gun }, 300.0, 22.0).is_none());
        assert!(rescue_time(0.0, 0.0, gun, 300.0, 22.0).is_none());
        assert!(rescue_time(0.0, 144.0, gun, 300.0, f32::INFINITY).is_none());
        let deadline = 300.0 / gun.dps + RESPONSE_MARGIN;
        assert!(rescue_time(212.0, 144.0, gun, 300.0, deadline).is_none());
    }

    #[test]
    fn unreachable_paths_stay_unreachable_and_in_range_needs_no_path() {
        assert_eq!(rescue_route(1500.0, 212.0, || -1.0), -1.0);
        assert_eq!(rescue_route(1500.0, 212.0, || f32::NAN), -1.0);
        assert_eq!(rescue_route(1500.0, 212.0, || 1450.0), 1500.0);
        assert_eq!(rescue_route(1500.0, 212.0, || 3000.0), 3000.0);
        assert_eq!(rescue_route(200.0, 212.0, || panic!("already in range")), 200.0);
    }

    #[test]
    fn continuous_fire_rechecks_without_delaying_the_first_hit_or_other_victims() {
        let mut checked = Default::default();
        assert!(rescue_due(&mut checked, (1, 2), 100));
        for frame in 101..115 {
            assert!(!rescue_due(&mut checked, (1, 2), frame));
        }
        assert!(rescue_due(&mut checked, (3, 2), 114));
        assert!(rescue_due(&mut checked, (1, 2), 115));
    }
}
