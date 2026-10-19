//! Assault squads, shield balls and air squads.

use std::collections::{BTreeSet, VecDeque};

use springai::{Unit, Vec3};

use crate::Ai;
use crate::military::{Fighter, SquadId, SquadKind};
use crate::nav;
use crate::util::{NULLPOS, distance};

#[derive(Default)]
pub struct SquadHandler {
    /// Fighter-store ids of all assault-type fighters.
    pub fighters: BTreeSet<i32>,
    pub next_cloaky_squad: Option<SquadId>,
    pub next_shield_squad: Option<SquadId>,
    pub next_amph_squad: Option<SquadId>,
    pub next_recluse_squad: Option<SquadId>,
    pub next_spider_squad: Option<SquadId>,
    pub next_veh_squad: Option<SquadId>,
    pub next_halberd_squad: Option<SquadId>,
    pub next_hover_squad: Option<SquadId>,
    pub next_tank_squad: Option<SquadId>,
    pub next_boat_squad: Option<SquadId>,
    pub next_air_squad: Option<SquadId>,
    pub next_brawler_squad: Option<SquadId>,
    pub shield_squads: VecDeque<SquadId>,
    pub squads: VecDeque<SquadId>,
    pub air_squads: VecDeque<SquadId>,
}

/// Which forming squad an assault unit joins.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Slot {
    Cloaky,
    Shield,
    Amph,
    Recluse,
    Spider,
    Veh,
    Halberd,
    Hover,
    Tank,
    Boat,
    Brawler,
    /// Rapier/revenant and other air assaults.
    Air,
}

impl Slot {
    fn for_name(name: &str) -> Slot {
        if name.starts_with("cloak") {
            Slot::Cloaky
        } else if name.starts_with("shield") {
            Slot::Shield
        } else if name.starts_with("amph") {
            Slot::Amph
        } else if name == "spiderskirm" {
            Slot::Recluse
        } else if name.starts_with("spider") {
            Slot::Spider
        } else if name.starts_with("veh") {
            Slot::Veh
        } else if name == "hoverassault" {
            Slot::Halberd
        } else if name.starts_with("hover") {
            Slot::Hover
        } else if name.starts_with("tank") {
            Slot::Tank
        } else if name.starts_with("ship") {
            Slot::Boat
        } else if name == "gunshipheavyskirm" {
            Slot::Brawler
        } else {
            Slot::Air
        }
    }

    fn is_air(self) -> bool {
        matches!(self, Slot::Brawler | Slot::Air)
    }

    /// Whether a forming squad of this slot is complete and should rally.
    fn is_complete(self, metal_value: f32, income: f32, size: usize, adjusted_income: f32) -> bool {
        match self {
            Slot::Halberd => size as i32 >= 8.min(2 + (adjusted_income / 15.0).floor() as i32),
            Slot::Brawler | Slot::Air => metal_value > income * 45.0,
            _ => ((metal_value > income * 45.0 && metal_value > 1000.0) || metal_value > 2500.0) && size > 2,
        }
    }
}

impl SquadHandler {
    pub fn new() -> Self {
        Self::default()
    }

    fn next_squad(&mut self, slot: Slot) -> &mut Option<SquadId> {
        match slot {
            Slot::Cloaky => &mut self.next_cloaky_squad,
            Slot::Shield => &mut self.next_shield_squad,
            Slot::Amph => &mut self.next_amph_squad,
            Slot::Recluse => &mut self.next_recluse_squad,
            Slot::Spider => &mut self.next_spider_squad,
            Slot::Veh => &mut self.next_veh_squad,
            Slot::Halberd => &mut self.next_halberd_squad,
            Slot::Hover => &mut self.next_hover_squad,
            Slot::Tank => &mut self.next_tank_squad,
            Slot::Boat => &mut self.next_boat_squad,
            Slot::Brawler => &mut self.next_brawler_squad,
            Slot::Air => &mut self.next_air_squad,
        }
    }
}

/// Low-shield hysteresis for shield balls: retreat below half shields and
/// come back once they are mostly recharged.
fn update_low_shields(low_shields: bool, shields: f32) -> bool {
    if low_shields && shields > 0.8 {
        false
    } else if !low_shields && shields < 0.5 {
        true
    } else {
        low_shields
    }
}

impl Ai {
    pub fn squad_update(&mut self, frame: i32) {
        if frame % 30 == (self.offset + 7) % 30 {
            self.squad_check_fighters(frame);
        }
        if frame % 7 == self.offset % 7 {
            self.squad_assign_squads();
        }
        if frame % 7 == (self.offset + 4) % 7 {
            self.squad_assign_shield_squads();
        }
        if frame % 14 == (self.offset + 9) % 14 {
            self.squad_assign_air_squads();
        }
    }

    /// Drop dead and stuck fighters, and (re)assign squadless ones.
    fn squad_check_fighters(&mut self, frame: i32) {
        let ids: Vec<i32> = self.squad_handler.fighters.iter().copied().collect();
        for id in ids {
            let Some(f) = self.fighters.get(&id) else {
                self.squad_handler.fighters.remove(&id);
                continue;
            };
            let unit = f.unit;
            if f.is_dead() {
                self.squad_leave_squad(id);
                self.squad_delete_fighter(id);
                continue;
            }
            let retreating = self.retreat_handler.is_retreating(id);
            let flies = unit.def().is_some_and(|d| d.is_able_to_fly());
            if !retreating && !flies && self.fighters.get_mut(&id).is_some_and(|f| f.is_stuck(frame)) {
                self.squad_leave_squad(id);
                self.eco_add_combat_reclaim_task(unit);
                self.retreat_remove_unit(unit);
                self.squad_delete_fighter(id);
                continue;
            }
            // A squad id that no longer exists counts as no squad.
            let Some(f) = self.fighters.get_mut(&id) else { continue };
            if f.squad.is_some_and(|s| !self.squads.contains(s)) {
                f.squad = None;
            }
            if f.squad.is_none() && !retreating {
                if self.unit_classes.assaults.contains(self.defs.name_of(unit)) {
                    self.squad_assign_assault(id);
                } else {
                    self.squad_assign_shield_mob(id);
                }
            }
        }
    }

    fn squad_delete_fighter(&mut self, id: i32) {
        self.fighters.remove(&id);
        self.squad_handler.fighters.remove(&id);
    }

    /// Remove a fighter from whatever squad it is in.
    fn squad_leave_squad(&mut self, id: i32) {
        let Some(sid) = self.fighters.get(&id).and_then(|f| f.squad) else { return };
        let mut nav = nav!(self);
        match self.squads.get_mut(sid) {
            Some(s) => s.remove_unit_nav(id, &mut self.fighters, &mut nav),
            None => {
                if let Some(f) = self.fighters.get_mut(&id) {
                    f.squad = None;
                }
            }
        }
    }

    /// Delete a squad, letting go of supports that were following it.
    fn squad_delete_squad(&mut self, sid: SquadId) {
        for id in &self.misc_handler.supports {
            if let Some(g) = self.fighters.get_mut(id)
                && g.squad == Some(sid)
            {
                g.squad = None;
            }
        }
        self.squads.remove(sid);
    }

    fn squad_assign_assault(&mut self, id: i32) {
        let Some(unit) = self.fighters.get(&id).map(|f| f.unit) else { return };
        let slot = Slot::for_name(self.defs.name_of(unit));
        let tf = self.graph.territory_fraction;
        let inc = self.eco.effective_income;
        let allies = self.merged_allies;

        // Create a new squad if there isn't one.
        let sid = match *self.squad_handler.next_squad(slot) {
            Some(sid) if self.squads.contains(sid) => sid,
            _ => {
                let kind = if slot == Slot::Halberd { SquadKind::Hover } else { SquadKind::Assault };
                let sid = self.squads.create(kind, self.team_id, &self.defs);
                if let Some(s) = self.squads.get_mut(sid) {
                    if slot.is_air() {
                        // For brawlers/nimbus and rapier/revenant.
                        s.income = inc / if allies == 0 { 1.0 } else { 2.0 };
                        s.is_air_squad = true;
                    } else {
                        s.income = inc / (1.0 + (allies as f32 * tf * tf));
                    }
                }
                if slot.is_air() {
                    self.squad_handler.air_squads.push_back(sid);
                } else {
                    self.squad_handler.squads.push_back(sid);
                }
                *self.squad_handler.next_squad(slot) = Some(sid);
                sid
            }
        };

        let mut nav = nav!(self);
        let Some(s) = self.squads.get_mut(sid) else { return };
        s.add_unit_nav(id, &mut self.fighters, &mut nav);
        if slot.is_complete(s.metal_value, s.income, s.get_size(), self.eco.adjusted_income) {
            s.status = 'r';
            *self.squad_handler.next_squad(slot) = None;
        }
    }

    pub fn squad_add_assault(&mut self, f: Fighter) {
        // port: one fighter store serves all handlers, so also refuse units
        // another handler already controls instead of replacing its wrapper.
        if self.squad_handler.fighters.contains(&f.id) || self.fighters.contains_key(&f.id) {
            return;
        }
        let id = f.id;
        self.fighters.insert(id, f);
        self.squad_handler.fighters.insert(id);
        self.squad_assign_assault(id);
    }

    fn squad_assign_shield_mob(&mut self, id: i32) {
        let Some(unit) = self.fighters.get(&id).map(|f| f.unit) else { return };
        let name = self.defs.name_of(unit);
        let (is_felon, is_aspis) = (name == "shieldfelon", name == "shieldshield");
        let shield_squads: Vec<SquadId> =
            self.squad_handler.shield_squads.iter().copied().filter(|&s| self.squads.contains(s)).collect();

        let needed = if is_felon && (self.merged_allies == 0 || shield_squads.is_empty()) {
            // Create a new shieldball for each felon, unless on teams where you want just one giant death ball.
            self.retreat_remove_unit(unit);
            let sid = self.squads.create(SquadKind::Shield, self.team_id, &self.defs);
            self.squad_handler.shield_squads.push_back(sid);
            let mut nav = nav!(self);
            if let Some(s) = self.squads.get_mut(sid) {
                s.status = 'a';
                s.add_unit_nav(id, &mut self.fighters, &mut nav);
            }
            self.squad_call_shield_mobs(sid);
            return;
        } else if is_aspis && !shield_squads.is_empty() {
            // Assign aspis to the squad with the fewest aspis, giving preference to higher metal value shieldballs
            // where they do the most good and are more likely to survive.
            self.retreat_remove_unit(unit);
            let mut needed: Option<(SquadId, f32, i32)> = None;
            for s in shield_squads.iter().filter_map(|&sid| self.squads.get(sid)) {
                if needed.is_none_or(|(_, metalv, min_aspis)| {
                    s.num_aspis < min_aspis || (s.num_aspis == min_aspis && s.metal_value > metalv)
                }) {
                    needed = Some((s.id, s.metal_value, s.num_aspis));
                }
            }
            needed.map(|(sid, _, _)| sid)
        } else if shield_squads.is_empty() {
            // If there's no felon, treat shield mobs as regular assaults.
            self.retreat_add_coward(unit);
            self.squad_assign_assault(id);
            return;
        } else {
            // Add the unit to the squad with the lowest value.
            self.retreat_remove_unit(unit);
            let mut needed: Option<(SquadId, f32)> = None;
            for s in shield_squads.iter().filter_map(|&sid| self.squads.get(sid)) {
                if needed.is_none_or(|(_, metalv)| s.metal_value < metalv) {
                    needed = Some((s.id, s.metal_value));
                }
            }
            needed.map(|(sid, _)| sid)
        };

        let mut nav = nav!(self);
        if let Some(s) = needed.and_then(|sid| self.squads.get_mut(sid)) {
            s.add_unit_nav(id, &mut self.fighters, &mut nav);
        }
    }

    pub fn squad_add_shield_mob(&mut self, f: Fighter) {
        // port: see squad_add_assault.
        if self.squad_handler.fighters.contains(&f.id) || self.fighters.contains_key(&f.id) {
            return;
        }
        let mut f = f;
        f.shield_mob = true;
        let id = f.id;

        let mut fit = Vec::new();
        for sid in std::mem::take(&mut self.squad_handler.shield_squads) {
            let Some(s) = self.squads.get_mut(sid) else { continue };
            if s.is_dead(&self.fighters) || s.num_felons == 0 {
                // Disband felonless squads and repurpose the units as regular mobs until a new felon appears.
                fit.extend(s.disband(&mut self.fighters));
                self.squad_delete_squad(sid);
            } else {
                self.squad_handler.shield_squads.push_back(sid);
            }
        }
        for fi in fit {
            self.squad_assign_shield_mob(fi);
        }
        self.fighters.insert(id, f);
        self.squad_handler.fighters.insert(id);
        self.squad_assign_shield_mob(id);
    }

    /// Call a flash mob when a felon appears.
    fn squad_call_shield_mobs(&mut self, sid: SquadId) {
        let ids: Vec<i32> = self.squad_handler.fighters.iter().copied().collect();
        for id in ids {
            let Some(f) = self.fighters.get(&id) else { continue };
            if !f.shield_mob {
                continue;
            }
            if f.squad.and_then(|s| self.squads.get(s)).is_some_and(|s| s.is_shield_squad()) {
                continue;
            }
            let unit = f.unit;
            if self.retreat_handler.is_retreating(id) || unit.health() <= 0.0 || unit.team() != self.team_id {
                continue;
            }
            if f.squad.is_some() {
                self.squad_leave_squad(id);
                if let Some(f) = self.fighters.get_mut(&id) {
                    f.squad = None;
                }
            }
            self.retreat_remove_unit(unit);
            let mut nav = nav!(self);
            if let Some(s) = self.squads.get_mut(sid) {
                s.add_unit_nav(id, &mut self.fighters, &mut nav);
            }
        }
    }

    pub fn squad_remove_unit(&mut self, u: Unit) {
        let id = u.id();
        if !self.squad_handler.fighters.contains(&id) {
            return;
        }
        self.squad_leave_squad(id);
        self.squad_delete_fighter(id);
    }

    pub fn squad_remove_from_squad(&mut self, u: Unit) {
        if self.squad_handler.fighters.contains(&u.id()) {
            self.squad_leave_squad(u.id());
        }
    }

    /// The Java original's `Squad.isDead()` housekeeping, which the C++ port
    /// reduced to a `cutoff()` call before checking ground squads.
    fn squad_refresh(&mut self, sid: SquadId) {
        let Some(s) = self.squads.get_mut(sid) else { return };
        // port: the C++ cut off forming squads too, dissolving every new
        // squad below 3 units and re-adding its units (inflating its metal
        // value) until it filled up; Java only cuts off finished squads.
        // It also dropped the cutoff for air squads and the leader choice.
        if s.status != 'f' {
            s.cutoff(&mut self.fighters);
        }
        s.select_leader(&self.fighters);
    }

    /// Assign up to one squad at a time, up to four per second.
    fn squad_assign_squads(&mut self) {
        while let Some(sid) = self.squad_handler.squads.pop_front() {
            self.squad_refresh(sid);
            let Some(s) = self.squads.get(sid) else { continue };
            if s.is_dead(&self.fighters) {
                if s.status == 'f' {
                    self.squad_handler.squads.push_back(sid);
                    return;
                }
                self.squad_delete_squad(sid);
                continue;
            } else if self.frame - s.last_assignment_frame < 30 {
                self.squad_handler.squads.push_front(sid);
                return;
            } else {
                self.squad_assign_squad(sid);
                if let Some(s) = self.squads.get_mut(sid) {
                    s.last_assignment_frame = self.frame;
                }
                self.squad_handler.squads.push_back(sid);
                return;
            }
        }
    }

    /// Assign up to one air squad at a time, up to two per second.
    fn squad_assign_air_squads(&mut self) {
        while let Some(sid) = self.squad_handler.air_squads.pop_front() {
            self.squad_refresh(sid);
            let Some(s) = self.squads.get(sid) else { continue };
            if s.is_dead(&self.fighters) {
                if s.status == 'f' {
                    self.squad_handler.air_squads.push_back(sid);
                    return;
                }
                self.squad_delete_squad(sid);
                continue;
            } else if self.frame - s.last_assignment_frame < 60 {
                self.squad_handler.air_squads.push_front(sid);
                return;
            } else {
                self.squad_assign_air_squad(sid);
                if let Some(s) = self.squads.get_mut(sid) {
                    s.last_assignment_frame = self.frame;
                }
                self.squad_handler.air_squads.push_back(sid);
                return;
            }
        }
    }

    fn squad_set_target(&mut self, sid: SquadId, target: Vec3) {
        let mut nav = nav!(self);
        if let Some(s) = self.squads.get_mut(sid) {
            s.set_target(target, &mut self.fighters, &mut nav);
        }
    }

    fn squad_retreat_to(&mut self, sid: SquadId, pos: Vec3) {
        let mut nav = nav!(self);
        if let Some(s) = self.squads.get_mut(sid) {
            s.retreat_to(pos, &mut self.fighters, &mut nav);
        }
    }

    fn squad_is_rallied(&mut self, sid: SquadId) -> bool {
        let mut nav = nav!(self);
        self.squads.get_mut(sid).is_some_and(|s| s.is_rallied(&mut self.fighters, &mut nav))
    }

    fn squad_set_status(&mut self, sid: SquadId, status: char) {
        if let Some(s) = self.squads.get_mut(sid) {
            s.status = status;
        }
    }

    fn squad_assign_squad(&mut self, sid: SquadId) {
        let Some(s) = self.squads.get(sid) else { return };
        let pos = s.get_pos(&self.fighters);
        let (status, target) = (s.status, s.target);

        match status {
            'f' => {
                if self.war.get_threat(pos) > 2.0 * self.war.get_total_friendly_threat(pos)
                    || self.war.get_porc_threat(pos) > 0.0
                    || (target != NULLPOS && self.war.get_porc_threat(target) > 0.0)
                {
                    let haven = self.graph_get_closest_haven(pos);
                    self.squad_retreat_to(sid, haven);
                } else {
                    let rally = self.war_get_rally_point(pos);
                    self.squad_set_target(sid, rally);
                }
            }
            'r' => {
                // Set rallying for squads that are finished forming and gathering to attack.
                let rally = self.war_get_rally_point(pos);
                self.squad_set_target(sid, rally);
                if self.squad_is_rallied(sid) {
                    self.squad_set_status(sid, 'a');
                }
            }
            'a' => {
                if self.war.get_effective_threat(pos) > 0.0
                    && (!self.graph.is_ally_territory(pos) || self.war.get_porc_threat(pos) > 0.0)
                {
                    let haven = self.graph_get_closest_haven(pos);
                    self.squad_retreat_to(sid, haven);
                }
                let Some(leader) = self.squads.get(sid).and_then(|s| s.get_leader(&self.fighters)) else { return };
                let target = self.war_get_target(leader, true);
                self.squad_set_target(sid, target);
            }
            _ => {}
        }
    }

    fn squad_assign_air_squad(&mut self, sid: SquadId) {
        let Some(s) = self.squads.get(sid) else { return };
        let pos = s.get_pos(&self.fighters);

        match s.status {
            'f' => {
                let target = self.war_get_air_rally_point(pos);
                if distance(pos, target) > 1800.0 {
                    self.squad_retreat_to(sid, target);
                } else {
                    self.squad_set_target(sid, target);
                }
            }
            'r' => {
                // Set rallying for squads that are finished forming and gathering to attack.
                let rally = self.war_get_air_rally_point(pos);
                self.squad_set_target(sid, rally);
                if self.squad_is_rallied(sid) {
                    self.squad_set_status(sid, 'a');
                }
            }
            'a' => {
                if self.war.get_effective_aa_threat(pos) > 0.0 {
                    let haven = self.graph_get_closest_air_haven(pos);
                    self.squad_retreat_to(sid, haven);
                }
                let Some(leader) = self.squads.get(sid).and_then(|s| s.get_leader(&self.fighters)) else { return };
                let target = self.war_get_air_target(leader, true);
                if distance(target, pos) > 1800.0 {
                    self.squad_retreat_to(sid, target);
                } else {
                    self.squad_set_target(sid, target);
                }
            }
            _ => {}
        }
    }

    /// Process up to one shield squad at a time, four per second.
    fn squad_assign_shield_squads(&mut self) {
        while let Some(sid) = self.squad_handler.shield_squads.pop_front() {
            let Some(s) = self.squads.get_mut(sid) else { continue };
            if s.is_dead(&self.fighters) || s.num_felons == 0 {
                // Disband felonless squads and assign them to other felons if available, else dump them back as assaults.
                let fit = s.disband(&mut self.fighters);
                for fi in fit {
                    self.squad_assign_shield_mob(fi);
                }
                self.squad_delete_squad(sid);
                continue;
            }
            if self.frame - s.last_assignment_frame < 30 {
                self.squad_handler.shield_squads.push_front(sid);
                return;
            }

            s.low_shields = update_low_shields(s.low_shields, s.get_shields());
            let low_shields = s.low_shields;
            let pos = s.get_pos(&self.fighters);
            let health = s.get_health(&self.fighters);
            let metal_value = s.metal_value;
            let leader = s.get_leader(&self.fighters);

            if low_shields {
                let haven = self.graph_get_closest_raider_haven(pos);
                self.squad_retreat_to(sid, haven);
            } else if health < 0.85 {
                let haven = self.graph_get_closest_haven(pos);
                self.squad_retreat_to(sid, haven);
            } else if let Some(leader) = leader.filter(|&l| {
                metal_value > 1300.0 && self.defs.name_of(l) == "shieldfelon" && self.squad_is_rallied(sid)
            }) {
                let target = self.war_get_target(leader, false);
                self.squad_set_target(sid, target);
            } else if self.war.get_effective_threat(pos) > 0.0 || self.war.get_porc_threat(pos) > 0.0 {
                let haven = self.graph_get_closest_haven(pos);
                self.squad_retreat_to(sid, haven);
            } else {
                let rally = self.war_get_rally_point(pos);
                self.squad_set_target(sid, rally);
            }
            if let Some(s) = self.squads.get_mut(sid) {
                s.last_assignment_frame = self.frame;
            }
            self.squad_handler.shield_squads.push_back(sid);
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slots_follow_name_prefixes() {
        assert_eq!(Slot::for_name("cloakassault"), Slot::Cloaky);
        assert_eq!(Slot::for_name("shieldassault"), Slot::Shield);
        assert_eq!(Slot::for_name("amphfloater"), Slot::Amph);
        assert_eq!(Slot::for_name("spiderskirm"), Slot::Recluse);
        assert_eq!(Slot::for_name("spiderassault"), Slot::Spider);
        assert_eq!(Slot::for_name("vehassault"), Slot::Veh);
        assert_eq!(Slot::for_name("hoverassault"), Slot::Halberd);
        assert_eq!(Slot::for_name("hoverriot"), Slot::Hover);
        assert_eq!(Slot::for_name("tankassault"), Slot::Tank);
        assert_eq!(Slot::for_name("shipassault"), Slot::Boat);
        assert_eq!(Slot::for_name("gunshipheavyskirm"), Slot::Brawler);
        assert_eq!(Slot::for_name("gunshipassault"), Slot::Air);
        assert!(Slot::Air.is_air() && Slot::Brawler.is_air() && !Slot::Halberd.is_air());
    }

    #[test]
    fn ground_squads_need_value_and_three_units() {
        let s = Slot::Cloaky;
        // Enough value relative to income, but too few units.
        assert!(!s.is_complete(1100.0, 10.0, 2, 10.0));
        assert!(s.is_complete(1100.0, 10.0, 3, 10.0));
        // Value below income * 45.
        assert!(!s.is_complete(1100.0, 30.0, 5, 10.0));
        // Big enough regardless of income.
        assert!(s.is_complete(2600.0, 100.0, 3, 10.0));
        // Never below 1000 metal unless above 2500.
        assert!(!s.is_complete(900.0, 1.0, 5, 10.0));
    }

    #[test]
    fn halberd_squads_scale_with_income() {
        let s = Slot::Halberd;
        assert!(!s.is_complete(0.0, 0.0, 2, 29.0));
        assert!(s.is_complete(0.0, 0.0, 2, 14.0));
        assert!(!s.is_complete(0.0, 0.0, 3, 45.0));
        assert!(s.is_complete(0.0, 0.0, 5, 45.0));
        assert!(s.is_complete(0.0, 0.0, 8, 1000.0));
        assert!(!s.is_complete(0.0, 0.0, 7, 1000.0));
    }

    #[test]
    fn air_squads_only_need_value() {
        assert!(Slot::Air.is_complete(500.0, 10.0, 1, 0.0));
        assert!(!Slot::Brawler.is_complete(450.0, 10.0, 9, 0.0));
    }

    #[test]
    fn low_shields_hysteresis() {
        assert!(!update_low_shields(false, 0.6));
        assert!(update_low_shields(false, 0.4));
        assert!(update_low_shields(true, 0.7));
        assert!(!update_low_shields(true, 0.85));
    }
}
