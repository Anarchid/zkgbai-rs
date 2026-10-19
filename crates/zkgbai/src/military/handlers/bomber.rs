//! Bomber flocks.

use std::collections::{BTreeMap, VecDeque};

use springai::Unit;

use crate::Ai;
use crate::military::{Bomber, Enemy};
use crate::nav;

pub struct BomberHandler {
    bombers: BTreeMap<i32, Bomber>,
    /// Bombers rearming/repairing (ids into `bombers`).
    unarmed_bombers: Vec<i32>,
    /// Armed bombers waiting for a flock to form.
    ready_bombers: VecDeque<i32>,
    /// Bombers on a bombing run.
    active_bombers: Vec<i32>,
    /// Bombers needed before a flock is sent out.
    min_flock: i32,
}

impl Default for BomberHandler {
    fn default() -> Self {
        BomberHandler {
            bombers: BTreeMap::new(),
            unarmed_bombers: Vec::new(),
            ready_bombers: VecDeque::new(),
            active_bombers: Vec::new(),
            min_flock: 8,
        }
    }
}

impl BomberHandler {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn get_bomber_size(&self) -> usize {
        self.unarmed_bombers.len() + self.ready_bombers.len() + self.active_bombers.len()
    }

    fn forget(&mut self, id: i32) {
        self.unarmed_bombers.retain(|&b| b != id);
        self.ready_bombers.retain(|&b| b != id);
        self.active_bombers.retain(|&b| b != id);
        self.bombers.remove(&id);
    }
}

/// Bombers needed to kill `e` (`None` if its type is unknown).
fn needed_bombers(e: &Enemy) -> Option<i32> {
    e.ud.map(|ud| ((ud.health() / 800.0).ceil() + if e.is_important { 1.0 } else { 0.0 }) as i32)
}

impl Ai {
    pub fn bomber_add_bomber(&mut self, b: Bomber) {
        let h = &mut self.bomber_handler;
        if h.bombers.contains_key(&b.id) {
            return;
        }
        h.ready_bombers.push_back(b.id);
        h.bombers.insert(b.id, b);
    }

    pub fn bomber_remove_unit(&mut self, u: Unit) {
        let h = &mut self.bomber_handler;
        if h.bombers.contains_key(&u.id()) {
            h.forget(u.id());
        }
    }

    /// Forget an enemy (id into `war.targets`) that bombers may be targeting.
    pub fn bomber_remove_target(&mut self, enemy: i32) {
        for b in self.bomber_handler.bombers.values_mut() {
            if b.target == Some(enemy) {
                b.target = None;
            }
        }
    }

    pub fn bomber_update(&mut self, frame: i32) {
        if frame % 60 == self.offset % 60 {
            let h = &self.bomber_handler;
            if h.unarmed_bombers.is_empty() && h.ready_bombers.is_empty() && h.active_bombers.is_empty() {
                return;
            }
            self.bomber_clean_units();
            self.bomber_check_bombers();
            self.bomber_assign_bombers();
        }
    }

    /// Move rearmed and repaired bombers to the ready list.
    fn bomber_check_bombers(&mut self) {
        let h = &mut self.bomber_handler;
        let bombers = &h.bombers;
        let ready = &mut h.ready_bombers;
        h.unarmed_bombers.retain(|id| {
            let Some(b) = bombers.get(id) else {
                return false;
            };
            let u = b.get_unit();
            let unarmed = u.health() < u.max_health() || u.rules_param_float("noammo", 0.0) > 0.0;
            if !unarmed {
                ready.push_back(*id);
            }
            unarmed
        });
    }

    fn bomber_assign_bombers(&mut self) {
        let active: Vec<i32> = self.bomber_handler.active_bombers.clone();
        for id in active {
            let Some(b) = self.bomber_handler.bombers.get(&id) else {
                self.bomber_handler.active_bombers.retain(|&x| x != id);
                continue;
            };
            // Sift unarmed bombers back into the unarmed list.
            if b.get_unit().rules_param_float("noammo", 0.0) > 0.0 {
                self.bomber_handler.unarmed_bombers.push(id);
                self.bomber_handler.active_bombers.retain(|&x| x != id);
                continue;
            }

            if b.target_missing(&self.war.targets) {
                // port: the C++ moved these to unarmedBombers (copy-paste slip);
                // the Java returns them to the ready list.
                self.bomber_handler.ready_bombers.push_back(id);
                self.bomber_handler.active_bombers.retain(|&x| x != id);
                let center = self.graph.get_ally_center();
                let mut nav = nav!(self);
                if let Some(b) = self.bomber_handler.bombers.get_mut(&id) {
                    b.fly_to(center, &mut nav);
                }
            } else {
                let mut nav = nav!(self);
                if let Some(b) = self.bomber_handler.bombers.get_mut(&id)
                    && let Some(e) = b.target.and_then(|t| self.war.targets.get(&t))
                {
                    b.bomb(e, &mut nav);
                }
            }
        }

        // Have all unarmed bombers reload/heal themselves.
        let unarmed: Vec<i32> = self.bomber_handler.unarmed_bombers.clone();
        for id in unarmed {
            let Some(pos) = self.bomber_handler.bombers.get(&id).map(|b| b.get_pos()) else {
                continue;
            };
            if !self.graph.is_enemy_territory(pos) {
                if let Some(b) = self.bomber_handler.bombers.get(&id) {
                    b.find_pad();
                }
            } else {
                // If in enemy territory, maneuver back to safety before finding an airpad.
                let center = self.graph.get_ally_center();
                let mut nav = nav!(self);
                if let Some(b) = self.bomber_handler.bombers.get_mut(&id) {
                    b.fly_to(center, &mut nav);
                }
            }
        }

        let h = &self.bomber_handler;
        if h.unarmed_bombers.is_empty() && h.ready_bombers.len() as i32 >= h.min_flock {
            let targets = self.war_get_bomber_targets();
            if let Some(n) = targets.first().and_then(|t| self.war.targets.get(t)).and_then(needed_bombers) {
                self.bomber_handler.min_flock = n.max(8);
            }

            let h = &mut self.bomber_handler;
            for eid in targets {
                if h.ready_bombers.is_empty() {
                    break;
                }
                // port: the original dereferenced `ud` unchecked.
                let Some(mut needed) = self.war.targets.get(&eid).and_then(needed_bombers) else {
                    continue;
                };
                if needed > h.ready_bombers.len() as i32 {
                    continue;
                }
                while needed > 0 {
                    let Some(bid) = h.ready_bombers.pop_front() else {
                        break;
                    };
                    if let Some(b) = h.bombers.get_mut(&bid) {
                        b.target = Some(eid);
                    }
                    h.active_bombers.push(bid);
                    needed -= 1;
                }
            }
        }
    }

    fn bomber_clean_units(&mut self) {
        let h = &mut self.bomber_handler;
        let dead: Vec<i32> = h.bombers.values().filter(|b| b.is_dead()).map(|b| b.id).collect();
        for id in dead {
            h.forget(id);
        }
    }
}
