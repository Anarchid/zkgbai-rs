//! Landed riots own a local campaign, including while the final transport is
//! still unloading. They never join assault assignment or retreat-to-haven.
use super::*;
use crate::kgbutil::kgb_move_to;
use policy::danger_at;

const CHECKPOINT_TIMEOUT: i32 = 30 * 60;
const RETRY: i32 = 30 * 90;
const COHESION: f32 = 400.0;

#[derive(Clone)]
pub(super) struct Raid {
    pub members: Vec<Unit>,
    anchor: Vec3,
    pub(super) tier: Tier,
    target: Option<(i32, i32)>,
    /// Temporarily skip a checkpoint that could not be reached or destroyed.
    skipped: BTreeMap<i32, i32>,
    patrol: Option<(usize, i32)>,
    checked: BTreeMap<usize, i32>,
}
impl Raid {
    pub(super) fn new(anchor: Vec3, tier: Tier) -> Self {
        Self {
            members: Vec::new(),
            anchor,
            tier,
            target: None,
            skipped: BTreeMap::new(),
            patrol: None,
            checked: BTreeMap::new(),
        }
    }
}

fn center(units: &[Unit]) -> Vec3 {
    let n = units.len() as f32;
    Vec3::xz(units.iter().map(|u| u.pos().x).sum::<f32>() / n, units.iter().map(|u| u.pos().z).sum::<f32>() / n)
}

/// A ranged counter is dangerous outside the riots' reply range. Once it
/// gets caught in riot range, finish it rather than flee a favorable fight.
fn kiting_counter(c: &Contact, at: Vec3, range: f32) -> bool {
    let d = distance(c.at, at);
    c.skirm && c.danger > 0.0 && d > range * 0.9 && d < c.range + 180.0
}

impl Ai {
    pub(super) fn drops_raids_update(&mut self) {
        let mut raids = std::mem::take(&mut self.drops.raids);
        for r in &mut raids {
            self.drops_raid_command(r);
        }
        raids.retain(|r| !r.members.is_empty());
        self.drops.raids = raids;
    }

    pub(super) fn drops_raid_command(&self, raid: &mut Raid) {
        raid.members.retain(|u| u.health() > 0.0 && u.team() == self.team_id);
        if raid.members.is_empty() {
            return;
        }
        let Some(map) = self.terrain().and_then(|t| t.tier(raid.tier)) else { return };
        let contacts = self.drops_contacts();
        let here = center(&raid.members);
        let origin = Paths::new(map, pos(raid.anchor), |_| false);
        let local = |p: Vec3| origin.distance_to(pos(p)).is_some_and(|d| d <= RAID_REACH);
        let power: f32 = raid.members.iter().map(|u| (u.power() + u.health()) / 10.0).sum();
        let paths = policy::combat_paths(map, &contacts, here, power * 1.2);
        let range = raid.members.iter().map(|u| u.max_range()).fold(f32::INFINITY, f32::min);
        raid.skipped.retain(|_, until| self.frame < *until);
        if let Some((id, since)) = raid.target
            && self.frame - since > CHECKPOINT_TIMEOUT
        {
            raid.skipped.insert(id, self.frame + RETRY);
            raid.target = None;
        }
        // Landed survivors accept nearly even fights. Launch remains much
        // stricter; casualties never trigger a strategic trip home.
        let suitable = |c: &&Contact| {
            c.value > 0.0
                && local(c.at)
                && !raid.skipped.contains_key(&c.id)
                && danger_at(&contacts, c.at) <= power * 1.2
                && paths.distance_to(pos(c.at)).is_some()
        };
        let target =
            raid.target.and_then(|(id, _)| contacts.iter().filter(suitable).find(|c| c.id == id)).or_else(|| {
                contacts.iter().filter(suitable).min_by(|a, b| {
                    // Nearby checkpoints first, with a preference for lasting
                    // economic damage; never chase mobile QRF away from the job.
                    let score =
                        |c: &Contact| paths.distance_to(pos(c.at)).unwrap_or(f32::INFINITY) / c.value.sqrt().max(1.0);
                    score(a).total_cmp(&score(b))
                })
            });
        if target.map(|c| c.id) != raid.target.map(|(id, _)| id) {
            raid.target = target.map(|c| (c.id, self.frame));
            if let Some(c) = target {
                self.log(&format!(
                    "drops: {} landed riots raid checkpoint {} at ({:.0}, {:.0})",
                    raid.members.len(),
                    c.id,
                    c.at.x,
                    c.at.z
                ));
            }
        }
        // Scout every local mex checkpoint, including ones not yet in LOS.
        // Revisits find rebuilt mexes; distant mainland spots never enter
        // this itinerary. Keep a destination until reached or timed out.
        let patrol = if target.is_none() {
            if let Some((id, since)) = raid.patrol
                && self
                    .graph
                    .metal_spots
                    .get(id)
                    .is_none_or(|m| distance(m.position, here) < 180.0 || self.frame - since > CHECKPOINT_TIMEOUT)
            {
                raid.checked.insert(id, self.frame);
                raid.patrol = None;
            }
            let eligible = |m: &&crate::graph::MetalSpot| {
                !m.owned
                    && local(m.position)
                    && danger_at(&contacts, m.position) <= power * 1.2
                    && paths.distance_to(pos(m.position)).is_some()
                    && raid.checked.get(&m.index).is_none_or(|f| self.frame - f >= RETRY)
            };
            let spot = raid.patrol.and_then(|(id, _)| self.graph.metal_spots.get(id).filter(eligible)).or_else(|| {
                self.graph.metal_spots.iter().filter(eligible).min_by(|a, b| {
                    let score =
                        |m: &crate::graph::MetalSpot| paths.distance_to(pos(m.position)).unwrap_or(f32::INFINITY);
                    score(a).total_cmp(&score(b))
                })
            });
            if spot.map(|m| m.index) != raid.patrol.map(|(id, _)| id) {
                raid.patrol = spot.map(|m| (m.index, self.frame));
            }
            spot.map(|m| m.position)
        } else {
            raid.patrol = None;
            None
        };
        let threatened =
            contacts.iter().any(|c| kiting_counter(c, here, range)) || danger_at(&contacts, here) > power * 1.2;
        // Find a LOCAL escape, on this shore, not a haven on the mainland.
        // Penalise range exposure continuously so moving away still helps
        // when all nearby samples remain in a skirmisher's weapon circle.
        let safety = |p: Vec3| {
            danger_at(&contacts, p)
                + contacts
                    .iter()
                    .filter(|c| c.skirm && c.danger > 0.0)
                    .map(|c| c.danger * (c.range + 250.0 - distance(c.at, p)).max(0.0) / 250.0)
                    .sum::<f32>()
        };
        let escape = if threatened {
            let mut best = here;
            let mut risk = safety(here);
            for ring in [200.0, 400.0, 600.0] {
                for i in 0..16 {
                    let angle = i as f32 * std::f32::consts::TAU / 16.0;
                    let p = Vec3::xz(here.x + ring * angle.cos(), here.z + ring * angle.sin());
                    if p.x < 16.0
                        || p.z < 16.0
                        || p.x >= map.width as f32 * zk_terrain::CELL - 16.0
                        || p.z >= map.height as f32 * zk_terrain::CELL - 16.0
                        || !map.is_passable(pos(p))
                        || !local(p)
                        || !paths.distance_to(pos(p)).is_some_and(|d| d < ring * 1.5)
                    {
                        continue;
                    }
                    let candidate = safety(p);
                    if candidate < risk {
                        best = p;
                        risk = candidate;
                    }
                }
            }
            Some(best)
        } else {
            None
        };
        let lagging = raid.members.iter().any(|u| distance(u.pos(), here) > COHESION);
        for &u in &raid.members {
            u.set_fire_state(2, 0);
            u.set_move_state(0, 0); // Fire freely without chasing QRF off the shore.
            let goal = if let Some(p) = escape {
                p
            } else if lagging {
                here
            } else if let Some(c) = target {
                c.at
            } else if let Some(p) = patrol {
                p
            } else {
                // Cleared pocket: stay available for rebuilds and QRF.
                // Clear obsolete move/attack orders, not ownership.
                u.stop(0);
                continue;
            };
            if escape.is_none()
                && !lagging
                && let Some(e) = target
                    .and_then(|c| self.war.targets.get(&c.id))
                    .filter(|e| e.visible && distance(e.position, u.pos()) <= u.max_range())
            {
                u.attack(e.unit, 0);
                continue;
            }
            // Follow terrain waypoints, not the ordinary threat-avoiding
            // army pathfinder which can send the unit back to the mainland.
            let route = policy::combat_paths(map, &contacts, u.pos(), power * 1.2);
            if route.distance_to(pos(goal)).is_none() {
                continue;
            }
            let next = route
                .route(pos(goal))
                .first()
                .map(|&c| {
                    let p = map.chokes[c].pos;
                    Vec3::xz(p.x, p.z)
                })
                .filter(|p| distance(u.pos(), *p) > 100.0)
                .unwrap_or(goal);
            let d = distance(u.pos(), next);
            if d < 1.0 {
                u.stop(0);
                continue;
            }
            let mut step = crate::util::get_directional_point(u.pos(), next, d.min(300.0));
            step.y = self.cb.map().elevation_at(step.x, step.z);
            kgb_move_to(u, step, 0, self.frame);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn evade_ranged_counters_but_finish_ones_caught_close() {
        let mut rogue = Contact {
            id: 1,
            at: Vec3::xz(0.0, 0.0),
            value: 0.0,
            danger: 100.0,
            range: 450.0,
            fort: false,
            skirm: true,
        };
        assert!(kiting_counter(&rogue, Vec3::xz(400.0, 0.0), 270.0));
        assert!(!kiting_counter(&rogue, Vec3::xz(180.0, 0.0), 270.0));
        assert!(!kiting_counter(&rogue, Vec3::xz(800.0, 0.0), 270.0));
        rogue.skirm = false; // Raiders/light gunships are not a retreat trigger.
        assert!(!kiting_counter(&rogue, Vec3::xz(400.0, 0.0), 270.0));
    }
}
