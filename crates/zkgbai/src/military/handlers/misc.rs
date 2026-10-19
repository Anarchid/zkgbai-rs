//! Everything that is not a raider or squad unit: supports, loners,
//! artillery, striders, krows, sappers, superweapons, swifts...

use std::collections::{BTreeMap, BTreeSet};

use springai::{NO_TIMEOUT, Unit, Vec3};

use crate::Ai;
use crate::defs::cmd;
use crate::kgbutil::kgb_move_to;
use crate::military::{Fighter, SquadKind};
use crate::nav;
use crate::util::{NULLPOS, distance, get_angular_point, get_directional_point, get_radial_point};

#[derive(Default)]
pub struct MiscHandler {
    /// Fighter-store ids.
    pub supports: BTreeSet<i32>,
    /// Fighter-store ids.
    pub loners: BTreeSet<i32>,
    /// Fighter-store ids.
    pub striders: BTreeSet<i32>,
    /// Fighter-store ids.
    pub arties: BTreeSet<i32>,
    /// Fighter-store ids.
    pub krows: BTreeSet<i32>,
    /// Fighter-store ids of Ultimatums (raider-kind fighters).
    ultis: BTreeSet<i32>,
    sappers: BTreeMap<i32, Unit>,
    berthas: BTreeMap<i32, Unit>,
    /// Swifts waiting for enough company before scouting.
    swifts: BTreeMap<i32, Unit>,
    active_swifts: BTreeMap<i32, Unit>,
    nuke: Option<Unit>,
    zenith: Option<Unit>,
    derp: Option<Unit>,
    last_nuke_frame: i32,
    scouted: bool,
}

impl MiscHandler {
    pub fn new() -> Self {
        Self::default()
    }

    fn contains(&self, id: i32) -> bool {
        self.supports.contains(&id)
            || self.loners.contains(&id)
            || self.striders.contains(&id)
            || self.arties.contains(&id)
            || self.krows.contains(&id)
            || self.ultis.contains(&id)
    }
}

/// Pick the unowned spot a swift should scout next: the one seen longest ago
/// (by the original's comparison, which mixes "frames since seen" with the
/// last-seen frame of the best candidate so far).
fn pick_swift_spot(spots: &[(Vec3, i32)], frame: i32) -> Option<usize> {
    let mut best = None;
    let mut last_seen = 0.0f32;
    for (i, &(_, seen)) in spots.iter().enumerate() {
        let tmpseen = (frame - seen) as f32;
        if tmpseen > last_seen {
            best = Some(i);
            last_seen = seen as f32;
        }
    }
    best
}

/// Value of ground targets in a krow's dgun radius, stopping once it is
/// clearly worth firing. Items are (metal cost, build fraction, is static).
fn krow_dgun_worth(targets: impl IntoIterator<Item = (f32, f32, bool)>) -> bool {
    let mut cost = 0.0;
    for (metal, built, is_static) in targets {
        let mut val = metal * built;
        if is_static {
            val *= 2.0;
        }
        cost += val;
        if cost > 300.0 {
            return true;
        }
    }
    false
}

impl Ai {
    pub fn misc_update(&mut self, frame: i32) {
        if frame % 15 != (self.offset + 10) % 15 {
            return;
        }
        self.misc_clean_units();
        self.misc_update_supports();
        self.misc_update_sappers(frame);
        self.misc_update_swifts(frame);

        if frame % 30 == (self.offset + 10) % 30 {
            self.misc_assign_ultis();
            self.misc_assign_striders();
        }

        self.misc_dgun_striders(frame);
        self.misc_dgun_krows(frame);

        if frame % 90 == (self.offset + 10) % 90 {
            self.misc_assign_krows();
            self.misc_assign_loners();
            self.misc_assign_arties();
        }

        if frame % 200 == (self.offset + 10) % 200 {
            // Assign berthas.
            let berthas: Vec<Unit> = self.misc_handler.berthas.values().copied().collect();
            for b in berthas {
                let target = self.war_get_bertha_target(b.pos());
                if target != NULLPOS {
                    b.attack_area(target, 0.0, 0);
                } else {
                    b.stop(0);
                }
            }
        }

        if frame % 300 == (self.offset + 10) % 300 {
            // Use nukes: where no anti-nuke guards enough value.
            if let Some(nuke) = self.misc_handler.nuke
                && nuke.stockpile() > 0
                && frame - self.misc_handler.last_nuke_frame > 1800
                && self.nukes_fire(nuke)
            {
                self.misc_handler.last_nuke_frame = frame;
            }
        }

        if frame % 450 == (self.offset + 10) % 450
            && let Some(zenith) = self.misc_handler.zenith
        {
            let meteors = zenith.rules_param_float("meteorsControlled", 0.0) as i32;
            if meteors > 75 {
                let target = self.war_get_super_wep_target(zenith, true);
                if target != NULLPOS {
                    zenith.attack_area(target, 0.0, 0);
                } else {
                    zenith.stop(0);
                }
            } else {
                zenith.stop(0);
            }
        }

        if frame % 900 == (self.offset + 10) % 900
            && let Some(derp) = self.misc_handler.derp
        {
            let target = self.war_get_super_wep_target(derp, false);
            if target != NULLPOS {
                derp.attack_area(target, 0.0, 0);
            } else {
                derp.stop(0);
            }
        }
    }

    /// Store a new fighter unless some handler already controls it.
    fn misc_insert_fighter(&mut self, f: Fighter) -> Option<i32> {
        // port: one fighter store serves all handlers, so also refuse units
        // another handler already controls instead of replacing its wrapper.
        if self.misc_handler.contains(f.id) || self.fighters.contains_key(&f.id) {
            return None;
        }
        let id = f.id;
        self.fighters.insert(id, f);
        Some(id)
    }

    pub fn misc_add_loner(&mut self, f: Fighter) {
        if let Some(id) = self.misc_insert_fighter(f) {
            self.misc_handler.loners.insert(id);
        }
    }

    pub fn misc_add_arty(&mut self, f: Fighter) {
        if let Some(id) = self.misc_insert_fighter(f) {
            self.misc_handler.arties.insert(id);
        }
    }

    pub fn misc_add_support(&mut self, f: Fighter) {
        if let Some(id) = self.misc_insert_fighter(f) {
            self.misc_handler.supports.insert(id);
        }
    }

    pub fn misc_add_sapper(&mut self, u: Unit) {
        self.misc_handler.sappers.insert(u.id(), u);
    }

    pub fn misc_add_strider(&mut self, f: Fighter) {
        if let Some(id) = self.misc_insert_fighter(f) {
            self.misc_handler.striders.insert(id);
        }
    }

    pub fn misc_add_krow(&mut self, f: Fighter) {
        if let Some(id) = self.misc_insert_fighter(f) {
            self.misc_handler.krows.insert(id);
        }
    }

    pub fn misc_add_ulti(&mut self, f: Fighter) {
        if let Some(id) = self.misc_insert_fighter(f) {
            self.misc_handler.ultis.insert(id);
        }
    }

    pub fn misc_add_bertha(&mut self, u: Unit) {
        self.misc_handler.berthas.insert(u.id(), u);
        u.set_fire_state(0, 0);
    }

    /// Whether we have a nuke silo.
    pub fn misc_has_nuke(&self) -> bool {
        self.misc_handler.nuke.is_some()
    }

    pub fn misc_add_nuke(&mut self, u: Unit) {
        self.misc_handler.nuke = Some(u);
    }

    pub fn misc_add_zenith(&mut self, u: Unit) {
        self.misc_handler.zenith = Some(u);
        u.set_fire_state(0, 0);
    }

    pub fn misc_add_drp(&mut self, u: Unit) {
        self.misc_handler.derp = Some(u);
        u.set_fire_state(0, 0);
        u.set_trajectory(1, 0);

        let target = self.war_get_super_wep_target(u, true);
        if target != NULLPOS {
            u.attack_area(target, 0.0, 0);
        } else {
            u.stop(0);
        }
    }

    pub fn misc_add_swift(&mut self, u: Unit) {
        let h = &mut self.misc_handler;
        h.swifts.insert(u.id(), u);
        if h.swifts.len() > 4 || !h.scouted {
            h.scouted = true;
            h.active_swifts.append(&mut h.swifts);
        }
    }

    pub fn misc_remove_unit(&mut self, u: Unit) {
        let id = u.id();
        let h = &mut self.misc_handler;
        for slot in [&mut h.nuke, &mut h.zenith, &mut h.derp] {
            if slot.is_some_and(|s| s.id() == id) {
                *slot = None;
                return;
            }
        }

        if h.supports.remove(&id) {
            self.misc_forget_support(id);
        }
        let h = &mut self.misc_handler;
        if h.loners.remove(&id)
            | h.striders.remove(&id)
            | h.krows.remove(&id)
            | h.arties.remove(&id)
            | h.ultis.remove(&id)
        {
            self.fighters.remove(&id);
        }
        let h = &mut self.misc_handler;
        h.sappers.remove(&id);
        h.swifts.remove(&id);
        // port: the original never dropped active swifts; unit ids get reused.
        h.active_swifts.remove(&id);
        h.berthas.remove(&id);
    }

    /// Delete a support's fighter, keeping the aspis count of the shield
    /// squad it follows up to date.
    fn misc_forget_support(&mut self, id: i32) {
        if let Some(f) = self.fighters.remove(&id)
            && self.defs.name_of(f.unit) == "shieldshield"
        {
            // Decrement aspis count.
            if let Some(sq) = f.squad.and_then(|s| self.squads.get_mut(s))
                && sq.kind == SquadKind::Shield
            {
                sq.num_aspis -= 1;
            }
        }
    }

    /// Remove dead/captured units because spring devs are stupid and call
    /// update before unitDestroyed.
    fn misc_clean_units(&mut self) {
        let team = self.team_id;
        let gone = |u: Unit| u.health() <= 0.0 || u.team() != team;
        let fighters = &mut self.fighters;
        let h = &mut self.misc_handler;
        for set in [&mut h.loners, &mut h.striders, &mut h.krows, &mut h.arties, &mut h.ultis] {
            set.retain(|id| match fighters.get(id) {
                Some(f) if !gone(f.unit) => true,
                Some(_) => {
                    fighters.remove(id);
                    false
                }
                None => false,
            });
        }
        let dead_supports: Vec<i32> =
            h.supports.iter().copied().filter(|id| fighters.get(id).is_none_or(|f| gone(f.unit))).collect();
        for id in dead_supports {
            self.misc_handler.supports.remove(&id);
            self.misc_forget_support(id);
        }

        let h = &mut self.misc_handler;
        h.sappers.retain(|_, u| !gone(*u));
        h.swifts.retain(|_, u| !gone(*u));
        // port: see misc_remove_unit.
        h.active_swifts.retain(|_, u| !gone(*u));
        h.berthas.retain(|_, u| !gone(*u));
        for slot in [&mut h.nuke, &mut h.zenith, &mut h.derp] {
            if slot.is_some_and(gone) {
                *slot = None;
            }
        }
    }

    fn misc_update_supports(&mut self) {
        let ids: Vec<i32> = self.misc_handler.supports.iter().copied().collect();
        for id in ids {
            if self.retreat_handler.is_retreating(id) {
                continue;
            }
            let Some(s) = self.fighters.get(&id) else { continue };
            let mut squad = s.squad;
            let my_pos = s.get_pos();

            if squad.is_some_and(|sid| self.squads.get(sid).is_none_or(|sq| sq.is_dead(&self.fighters))) {
                squad = None;
            }
            if squad.is_none() && !self.squad_handler.squads.is_empty() {
                let mut max_value = 0.0;
                for sq in self.squad_handler.squads.iter().filter_map(|&sid| self.squads.get(sid)) {
                    if sq.is_dead(&self.fighters) {
                        continue;
                    }
                    if sq.metal_value > max_value {
                        max_value = sq.metal_value;
                        squad = Some(sq.id);
                    }
                }
            }
            if let Some(s) = self.fighters.get_mut(&id) {
                s.squad = squad;
            }

            let target = match squad.and_then(|sid| self.squads.get(sid)) {
                Some(sq) => {
                    let spos = sq.get_pos(&self.fighters);
                    let starget = sq.target;
                    if spos == starget {
                        self.say("Iris's squad's pos equals its target!");
                    }
                    if starget != NULLPOS {
                        if self.war.get_threat(starget) > 0.0 || self.war.get_bp(starget) == 0.0 {
                            get_directional_point(starget, spos, distance(spos, starget) + 125.0)
                        } else {
                            get_directional_point(spos, starget, 125.0)
                        }
                    } else {
                        spos
                    }
                }
                None => self.graph_get_closest_haven(my_pos),
            };
            let mut nav = nav!(self);
            if let Some(s) = self.fighters.get_mut(&id) {
                s.move_to(target, &mut nav);
            }
        }
    }

    fn misc_update_sappers(&mut self, frame: i32) {
        let sappers: Vec<Unit> = self.misc_handler.sappers.values().copied().collect();
        for s in sappers {
            if s.health() <= 0.0 || s.num_current_commands() > 0 {
                continue;
            }
            let pos = s.pos();
            let enemy = self
                .cb
                .enemy_units_in(pos, 350.0)
                .into_iter()
                .find(|e| e.def().is_some_and(|d| d.cost(self.metal) > 100.0));
            if let Some(enemy) = enemy {
                s.attack_timeout(enemy, 0, frame + 300);
                continue;
            }

            if self.defs.name_of(s) == "gunshipbomb" {
                if let Some(ms) = self.graph.get_closest_enemy_spot(pos) {
                    s.fight(ms.get_pos(), 0);
                }
                continue;
            }
            let ms = self
                .graph
                .get_closest_front_line_spot(pos)
                .or_else(|| self.graph.get_closest_neutral_spot(pos))
                .map(|ms| ms.get_pos());
            if let Some(ms) = ms.filter(|&ms| distance(pos, ms) > 500.0) {
                s.fight(get_angular_point(ms, self.graph.get_enemy_center(), 350.0), 0);
            }
        }
    }

    fn misc_update_swifts(&mut self, frame: i32) {
        let mut spots: Vec<(Vec3, i32)> =
            self.graph.get_unowned_spots().iter().map(|ms| (ms.get_pos(), ms.get_last_seen())).collect();
        let enemy_center = self.graph.get_enemy_center();
        let ally_center = self.graph.get_ally_center();
        for u in self.misc_handler.active_swifts.values() {
            if u.health() <= 0.0 {
                continue;
            }
            let Some(best) = pick_swift_spot(&spots, frame) else { continue };
            kgb_move_to(*u, spots[best].0, 0, frame);
            let pos = u.pos();
            if distance(pos, enemy_center) < distance(pos, ally_center) {
                u.execute_custom_command(cmd::ONECLICK_WEAPON, &[], 0, NO_TIMEOUT);
            }
            spots.remove(best);
        }
    }

    fn misc_assign_ultis(&mut self) {
        let ids: Vec<i32> = self.misc_handler.ultis.iter().copied().collect();
        for id in ids {
            if self.retreat_handler.is_retreating(id) {
                continue;
            }
            let Some(pos) = self.fighters.get(&id).map(|f| f.get_pos()) else { continue };
            let target =
                self.war_get_ulti_target(pos).and_then(|e| self.war.targets.get(&e)).map(|e| (e.position, e.unit));
            match target {
                Some((epos, eunit)) => {
                    if distance(pos, epos) > 300.0 {
                        self.misc_sneak(id, epos);
                    } else if let Some(f) = self.fighters.get(&id) {
                        f.unit.attack(eunit, 0);
                    }
                }
                None => {
                    let Some(spot) = self.graph.get_closest_enemy_spot(pos).map(|ms| ms.get_pos()) else { continue };
                    let tgt = self.graph_get_closest_haven(spot);
                    if tgt != NULLPOS && distance(tgt, pos) > 300.0 {
                        self.misc_sneak(id, tgt);
                    }
                }
            }
        }
    }

    fn misc_sneak(&mut self, id: i32, target: Vec3) {
        let mut nav = nav!(self);
        if let Some(f) = self.fighters.get_mut(&id) {
            f.sneak(target, &mut nav);
        }
    }

    fn misc_fight_to(&mut self, id: i32, target: Vec3) {
        let mut nav = nav!(self);
        if let Some(f) = self.fighters.get_mut(&id) {
            f.fight_to(target, &mut nav);
        }
    }

    fn misc_move_to(&mut self, id: i32, target: Vec3) {
        let mut nav = nav!(self);
        if let Some(f) = self.fighters.get_mut(&id) {
            f.move_to(target, &mut nav);
        }
    }

    fn misc_assign_striders(&mut self) {
        let ids: Vec<i32> = self.misc_handler.striders.iter().copied().collect();
        for id in ids {
            let Some(u) = self.fighters.get(&id).map(|f| f.unit) else { continue };
            if self.retreat_handler.is_retreating(id) {
                continue;
            }
            let pos = u.pos();
            let target = if u.rules_param_float("disarmed", 0.0) > 0.0 || self.war.get_effective_threat(pos) > 0.0 {
                self.graph_get_closest_haven(pos)
            } else {
                self.war_get_target(u, false)
            };
            self.misc_fight_to(id, target);
        }
    }

    fn misc_assign_krows(&mut self) {
        let ids: Vec<i32> = self.misc_handler.krows.iter().copied().collect();
        for id in ids {
            let Some(u) = self.fighters.get(&id).map(|f| f.unit) else { continue };
            if self.retreat_handler.is_retreating(id) {
                continue;
            }
            let pos = u.pos();
            let target = if u.rules_param_float("disarmed", 0.0) > 0.0 {
                self.graph_get_closest_air_haven(pos)
            } else {
                self.war_get_krow_target(pos)
            };
            let mut nav = nav!(self);
            if let Some(f) = self.fighters.get_mut(&id) {
                f.fly_to(target, &mut nav);
            }
        }
    }

    fn misc_assign_loners(&mut self) {
        let ids: Vec<i32> = self.misc_handler.loners.iter().copied().collect();
        for id in ids {
            let Some(u) = self.fighters.get(&id).map(|f| f.unit) else { continue };
            if self.retreat_handler.is_retreating(id) {
                continue;
            }
            let pos = u.pos();
            if (u.rules_param_float("disarmed", 0.0) > 0.0 || self.war.get_effective_threat(pos) > 0.0)
                && self.defs.name_of(u) != "spidercrabe"
            {
                let haven = self.graph_get_closest_haven(pos);
                self.misc_move_to(id, haven);
            } else {
                let target = self.war_get_target(u, false);
                self.misc_fight_to(id, target);
            }
        }
    }

    fn misc_assign_arties(&mut self) {
        let ids: Vec<i32> = self.misc_handler.arties.iter().copied().collect();
        for id in ids {
            let Some(u) = self.fighters.get(&id).map(|f| f.unit) else { continue };
            if self.retreat_handler.is_retreating(id) {
                continue;
            }
            let pos = u.pos();
            let name = self.defs.name_of(u);
            let (amph_assault, heavy_arty) = (name == "amphassault", name == "vehheavyarty");
            if u.rules_param_float("disarmed", 0.0) > 0.0 || (self.war.get_effective_threat(pos) > 0.0 && amph_assault)
            {
                let haven = self.graph_get_closest_haven(pos);
                self.misc_move_to(id, haven);
            } else if heavy_arty {
                let target = self.war_get_arty_target(pos, false);
                let target = get_radial_point(self.graph_get_closest_haven(target), 350.0);
                self.misc_move_to(id, target);
            } else {
                let target = self.war_get_arty_target(pos, false);
                self.misc_fight_to(id, target);
            }
        }
    }

    fn misc_dgun_striders(&mut self, frame: i32) {
        for id in &self.misc_handler.striders {
            let Some(s) = self.fighters.get(id) else { continue };
            if !matches!(self.defs.name_of(s.unit), "striderdante" | "striderscorpion" | "striderbantha") {
                continue;
            }
            if s.is_dgun_ready(frame)
                && let Some(target) = self.misc_get_dgun_target(s)
            {
                s.unit.dgun(target, 0);
            }
        }
    }

    fn misc_get_dgun_target(&self, s: &Fighter) -> Option<Unit> {
        let enemies = self.cb.enemy_units_in(s.get_pos(), 450.0);
        let mut target = None;
        let mut best_score = 0.0;
        // 74145: TODO: better dgun handling, consider AOE perhaps?
        let dante = self.defs.name_of(s.unit) == "striderdante";
        for e in enemies {
            let Some(ed) = e.def() else { continue };
            let cost = ed.cost(self.metal);
            let ename = self.defs.name(ed);
            let mobile = e.max_speed() > 0.0;
            let eligible = if dante {
                (mobile && !ed.is_able_to_fly() && cost > 200.0)
                    || (!mobile && ename != "wolverine_mine" && ename != "turretaalaser")
            } else {
                // Scorpion and bantha both have emp dguns, so they shouldn't shoot at unarmed crap.
                if !ed.is_able_to_attack() || ed.tooltip().contains("Anti-Air") {
                    continue;
                }
                (mobile && (!ed.is_able_to_fly() || ename == "gunshipkrow") && cost > 200.0)
                    || (!mobile && ename != "wolverine_mine")
            };
            if eligible && cost > best_score {
                best_score = cost;
                target = Some(e);
            }
        }
        target
    }

    fn misc_dgun_krows(&mut self, frame: i32) {
        for id in &self.misc_handler.krows {
            let Some(s) = self.fighters.get(id) else { continue };
            if !s.is_dgun_ready(frame) {
                continue;
            }
            let targets = self.cb.enemy_units_in(s.get_pos(), 175.0).into_iter().filter_map(|e| {
                let ed = e.def()?;
                if ed.is_able_to_fly() || self.defs.name(ed) == "wolverine_mine" {
                    return None;
                }
                let max_health = e.max_health();
                let built = if e.is_being_built() && max_health > 0.0 { e.health() / max_health } else { 1.0 };
                Some((ed.cost(self.metal), built, e.max_speed() == 0.0))
            });
            if krow_dgun_worth(targets) {
                s.unit.execute_custom_command(cmd::ONECLICK_WEAPON, &[], 0, NO_TIMEOUT);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn swift_spot_choice_matches_original_comparison() {
        let spots = [(Vec3::xz(0.0, 0.0), 100), (Vec3::xz(1.0, 0.0), 50), (Vec3::xz(2.0, 0.0), 900)];
        // frame 1000: 900 > 0 picks #0 (last_seen := 100); 950 > 100 picks
        // #1 (last_seen := 50); 100 > 50 picks #2.
        assert_eq!(pick_swift_spot(&spots, 1000), Some(2));
        assert_eq!(pick_swift_spot(&spots[..2], 1000), Some(1));
        assert_eq!(pick_swift_spot(&[], 1000), None);
        // Spots seen this very frame are never picked first.
        assert_eq!(pick_swift_spot(&[(Vec3::ZERO, 1000)], 1000), None);
    }

    #[test]
    fn krow_dgun_needs_enough_ground_value() {
        assert!(!krow_dgun_worth([(100.0, 1.0, false), (150.0, 1.0, false)]));
        assert!(krow_dgun_worth([(100.0, 1.0, false), (150.0, 1.0, true)]));
        assert!(!krow_dgun_worth([(400.0, 0.5, false)]));
        assert!(krow_dgun_worth([(400.0, 0.5, true)]));
        assert!(!krow_dgun_worth([]));
    }
}
