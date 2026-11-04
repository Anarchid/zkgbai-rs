//! Existing-silo opportunities: led Shockley salvos and Eos follow-through
//! on observed paralysis. Missile orders are assigned completely before firing.
use super::*;

const SHOCK_RADIUS: f32 = 120.0; // 140 blast radius, with a prediction margin.
const BALL_WORTH: f32 = 3000.0;
const SHOCK_SHIELD_DAMAGE: f32 = 10000.0;
const SHOCK_RETRY: i32 = 30 * 60;

pub(super) struct Shieldball {
    pub at: Vec3,
    pub members: Vec<i32>,
    pub value: f32,
    pub needed: usize,
}

/// Tiny bipartite matching (at most four pads), including reassignment.
/// Greedy firing could spend a missile and only then discover that the
/// remaining anti-nuke has no reachable pad.
pub(super) fn assign_pads(edges: &[Vec<usize>], pads: usize) -> Option<Vec<usize>> {
    fn visit(edges: &[Vec<usize>], used: &mut [bool], chosen: &mut Vec<usize>) -> bool {
        if chosen.len() == edges.len() {
            return true;
        }
        for &i in &edges[chosen.len()] {
            if i >= used.len() || used[i] {
                continue;
            }
            used[i] = true;
            chosen.push(i);
            if visit(edges, used, chosen) {
                return true;
            }
            chosen.pop();
            used[i] = false;
        }
        false
    }
    if edges.len() > pads {
        return None;
    }
    let mut chosen = Vec::new();
    visit(edges, &mut vec![false; pads], &mut chosen).then_some(chosen)
}

fn ball_salvo(aspis: usize, value: f32, shield: f32) -> Option<usize> {
    // Budget for fully draining the network, then one missile to stun.
    let needed = (shield / SHOCK_SHIELD_DAMAGE).ceil() as usize + 1;
    (aspis >= 2 && value >= BALL_WORTH && needed <= PADS && value >= WORTH * needed as f32).then_some(needed)
}

impl Ai {
    pub(super) fn missiles_shieldball(&self, from: Vec3) -> Option<Shieldball> {
        self.missiles_silo()?; // Never commissions a silo for this opportunity.
        let map = self.cb.map();
        let flight = flight::Flight::for_kind(self, SHOCKLEY)?;
        let enemies: Vec<_> = self
            .war
            .targets
            .values()
            .filter(|e| {
                !e.is_dead
                    && e.identified
                    && e.visible
                    && !e.is_static
                    && e.build_progress >= 1.0
                    && e.ud.is_some_and(|d| !d.is_able_to_fly())
                    && self.missiles.shocked.get(&e.unit_id).is_none_or(|f| self.frame - f >= SHOCK_RETRY)
            })
            .collect();
        let mut best: Option<Shieldball> = None;
        for seed in enemies.iter().filter(|e| e.def_name == "shieldshield") {
            let pack: Vec<_> =
                enemies.iter().filter(|e| distance(e.position, seed.position) <= 2.0 * SHOCK_RADIUS).collect();
            if pack.iter().filter(|e| e.def_name == "shieldshield").count() < 2 {
                continue;
            }
            let n = pack.len() as f32;
            let mut at = Vec3::xz(
                pack.iter().map(|e| e.position.x).sum::<f32>() / n,
                pack.iter().map(|e| e.position.z).sum::<f32>() / n,
            );
            let velocity = Vec3::xz(
                pack.iter().map(|e| e.unit.vel().x).sum::<f32>() / n,
                pack.iter().map(|e| e.unit.vel().z).sum::<f32>() / n,
            );
            let center = at;
            let mut frames = 0.0;
            let mut valid = true;
            for _ in 0..4 {
                at.y = map.elevation_at(at.x, at.z);
                let Some(p) = flight.predict(from, at, |_, _| -1.0e6) else {
                    valid = false;
                    break;
                };
                frames = p.frames;
                at = Vec3::xz(center.x + velocity.x * frames, center.z + velocity.z * frames);
            }
            at.y = map.elevation_at(at.x, at.z);
            if !valid
                || at.x < 0.0
                || at.z < 0.0
                || at.x >= map.width() as f32 * 8.0
                || at.z >= map.height() as f32 * 8.0
                || !self.missiles_reaches(SHOCKLEY, from, at)
                || !self.cb.friendly_units_in(at, SHOCK_RADIUS + 100.0).is_empty()
            {
                continue;
            }
            let hit: Vec<_> = pack
                .into_iter()
                .filter(|e| {
                    let v = e.unit.vel();
                    distance(Vec3::xz(e.position.x + v.x * frames, e.position.z + v.z * frames), at) <= SHOCK_RADIUS
                })
                .collect();
            let value = hit.iter().filter_map(|e| e.ud).map(|d| d.cost(self.metal)).sum::<f32>();
            let aspis = hit.iter().filter(|e| e.def_name == "shieldshield").count();
            // Count surrounding shields as well as those inside the EMP blast;
            // they can intercept the incoming salvo without being stunned.
            let mut shield = 0.0;
            let mut funnel = false;
            for e in self.war.targets.values().filter(|e| !e.is_dead) {
                if e.def_name == FUNNELWEB && distance(e.position, at) <= FUNNELWEB_RADIUS + 400.0 {
                    funnel = true;
                }
                if SHIELDS.contains(&e.def_name.as_str()) && distance(e.position, at) <= 2.0 * SHIELD_RADIUS + 400.0 {
                    shield += if e.visible { e.unit.shield_charge().max(0.0) } else { 3600.0 };
                }
            }
            let Some(needed) = ball_salvo(aspis, value, shield) else { continue };
            if funnel || best.as_ref().is_some_and(|b| b.value / b.needed as f32 >= value / needed as f32) {
                continue;
            }
            best = Some(Shieldball { at, members: hit.iter().map(|e| e.unit_id).collect(), value, needed });
        }
        best
    }

    pub(super) fn missiles_fire_shieldball(&mut self) {
        // Anti-nuke missions retain priority over opportunistic EMP.
        if !self.missiles_stun_targets().is_empty() {
            return;
        }
        let Some(silo) = self.missiles_silo() else { return };
        let Some(ball) = self.missiles_shieldball(silo.pos()) else { return };
        let ready: Vec<_> = self
            .missiles_ready(SHOCKLEY)
            .into_iter()
            .filter(|m| self.missiles_reaches(SHOCKLEY, m.pos(), ball.at))
            .collect();
        if ready.len() < ball.needed {
            return;
        }
        for m in ready.into_iter().take(ball.needed) {
            self.missiles_launch(m, ball.at);
        }
        for id in ball.members {
            self.missiles.shocked.insert(id, self.frame);
        }
        self.log(&format!(
            "missiles: {} Shockleys at led shieldball ({:.0}, {:.0}), {:.0} metal",
            ball.needed, ball.at.x, ball.at.z, ball.value
        ));
    }

    pub(super) fn missiles_stunned_clusters(&self) -> Vec<Target> {
        let Some(silo) = self.missiles_silo() else { return Vec::new() };
        let live: Vec<_> = self
            .war
            .targets
            .values()
            .filter(|e| {
                e.visible
                    && e.identified
                    && !e.is_static
                    && !e.is_dead
                    && e.ud.is_some_and(|d| !d.is_able_to_fly())
                    && self.missiles.struck.get(&e.unit_id).is_none_or(|f| self.frame - f >= LOOK_AFTER)
                    && self.missiles_eos_pad_reaches(silo.pos(), e.unit_id, e.position, 0)
            })
            .collect();
        let mut result = Vec::new();
        for seed in &live {
            let at = seed.position;
            if self.missiles_shield(at).is_some() || !self.cb.friendly_units_in(at, 192.0).is_empty() {
                continue;
            }
            // Use the minimum blast damage, with a 16-elmo radius margin.
            let members: Vec<_> = live
                .iter()
                .filter(|e| {
                    distance(e.position, at) <= 80.0 && self.missiles_eos_pad_reaches(silo.pos(), e.unit_id, at, 0)
                })
                .collect();
            let needed = members
                .iter()
                .map(|e| (e.unit.health() / (EOS_DAMAGE * 0.4)).ceil().max(1.0) as usize)
                .max()
                .unwrap_or(1);
            let value: f32 = members.iter().filter_map(|e| e.ud).map(|d| d.cost(self.metal)).sum();
            if members.len() >= 2 && needed <= PADS && value >= WORTH * needed as f32 {
                result.push(Target {
                    id: seed.unit_id,
                    members: members.iter().map(|e| e.unit_id).collect(),
                    pos: at,
                    value,
                    needed,
                    shielded: false,
                    silo: false,
                    launch: true,
                });
            }
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn shieldball_requires_multiple_aspis_and_three_thousand_value() {
        assert_eq!(ball_salvo(1, 5000.0, 3600.0), None);
        assert_eq!(ball_salvo(2, 2999.0, 7200.0), None);
        assert_eq!(ball_salvo(2, 3000.0, 7200.0), Some(2));
        assert_eq!(ball_salvo(8, 10000.0, 36000.0), None);
    }
    #[test]
    fn atomic_assignment_reassigns_flexible_pad_and_rejects_impossible_salvo() {
        assert_eq!(assign_pads(&[vec![0, 1], vec![0]], 2), Some(vec![1, 0]));
        assert_eq!(assign_pads(&[vec![0], vec![0]], 2), None);
        assert_eq!(assign_pads(&[vec![0], vec![]], 2), None);
    }
}
