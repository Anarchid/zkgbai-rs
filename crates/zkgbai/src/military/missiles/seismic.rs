//! Quake opportunities are hypothetical until the real post-impact terrain
//! passes the ordinary missile/path checks. No optimistic heightmap is installed.
use super::*;
use zk_terrain::strategy::{Analysis, Scenario};
use zk_terrain::{MoveClass, Pos, Tier};

const RADIUS: f32 = 320.0;
const GATHER: f32 = 416.0;
const COST: f32 = 400.0;
const PERIOD: i32 = 30 * 30;
const COOLDOWN: i32 = 30 * 120;

#[derive(Clone)]
enum Purpose {
    Eos { id: i32, needed: usize },
    Ramp { ends: [Vec3; 2], class: MoveClass, tier: Tier },
}
#[derive(Clone)]
struct Opportunity {
    at: Vec3,
    purpose: Purpose,
    score: f32,
}
#[derive(Default)]
pub(super) struct Seismic {
    checked: Option<i32>,
    plan: Option<Opportunity>,
    pending: Option<(Opportunity, i32)>,
    tried: Vec<(Vec3, i32)>,
}
impl Seismic {
    pub fn eos_demand(&self) -> usize {
        self.plan.as_ref().or_else(|| self.pending.as_ref().map(|p| &p.0)).map_or(0, |p| match p.purpose {
            Purpose::Eos { needed, .. } => needed,
            _ => 0,
        })
    }
}

/// weapon_terraform_explosions.lua and Terraform.DoSmoothDirectly:
/// an 8-elmo gather disk, then the sigmoid falloff with exponent 0.45.
/// The current gadget approaches the gathered mean (including raising lows).
/// We only admit ramps that also work with lowering alone, never land bridges.
struct Quake {
    at: Vec3,
    mean: f32,
}
impl Quake {
    fn new(at: Vec3, ground: &impl Fn(f32, f32) -> f32) -> Self {
        let (sx, sz) = (((at.x + 4.0) / 8.0).floor() * 8.0, ((at.z + 4.0) / 8.0).floor() * 8.0);
        let (mut sum, mut n) = (0.0, 0);
        // The gadget rounds its loop bounds up one grid cell, retaining
        // the original radius for the disk membership test.
        for ix in -53..=53 {
            for iz in -53..=53 {
                let (x, z) = (sx + ix as f32 * 8.0, sz + iz as f32 * 8.0);
                if (x - at.x).powi(2) + (z - at.z).powi(2) <= GATHER * GATHER {
                    sum += ground(x, z);
                    n += 1;
                }
            }
        }
        Self { at, mean: sum / n.max(1) as f32 }
    }
    fn height(&self, x: f32, z: f32, ground: &impl Fn(f32, f32) -> f32, lowering_only: bool) -> f32 {
        // Bilinear interpolation of modified engine heightmap vertices.
        let (gx, gz) = ((x / 8.0).floor() * 8.0, (z / 8.0).floor() * 8.0);
        let (fx, fz) = ((x - gx) / 8.0, (z - gz) / 8.0);
        let vertex = |x: f32, z: f32| {
            let old = ground(x, z);
            let r = ((x - self.at.x).powi(2) + (z - self.at.z).powi(2)) / (RADIUS * RADIUS);
            if r >= 1.0 {
                return old;
            }
            let numerator = (2.0 + 3.0f32.exp() + (-3.0f32).exp()) / (3.0f32.exp() - (-3.0f32).exp());
            let falloff = numerator / (1.0 + (6.0 * r.powf(0.45) - 3.0).exp()) - numerator / (1.0 + 3.0f32.exp());
            let delta = (self.mean - old) * falloff;
            old + if lowering_only { delta.min(0.0) } else { delta }
        };
        (vertex(gx, gz) * (1.0 - fx) + vertex(gx + 8.0, gz) * fx) * (1.0 - fz)
            + (vertex(gx, gz + 8.0) * (1.0 - fx) + vertex(gx + 8.0, gz + 8.0) * fx) * fz
    }
}

/// A 48-elmo corridor, extended into the existing passable endpoints.
/// Check cross-slope too: a smooth longitudinal profile alone can still be
/// the knife edge of an impassable ridge.
fn corridor(ends: [Vec3; 2], class: MoveClass, ground: impl Fn(f32, f32) -> f32) -> bool {
    let d = distance(ends[0], ends[1]);
    if d < 1.0 {
        return false;
    }
    let (ux, uz) = ((ends[1].x - ends[0].x) / d, (ends[1].z - ends[0].z) / d);
    for k in 0..=((d + 64.0) / 8.0).ceil() as usize {
        let along = (k as f32 * 8.0).min(d + 64.0) - 32.0;
        for side in [-24.0, -16.0, -8.0, 0.0, 8.0, 16.0, 24.0] {
            let (x, z) = (ends[0].x + ux * along - uz * side, ends[0].z + uz * along + ux * side);
            let h = ground(x, z);
            for (dx, dz) in [(8.0, 8.0), (-8.0, -8.0), (-8.0, 8.0), (8.0, -8.0)] {
                let sx = (ground(x + dx, z) - h) / dx;
                let sz = (ground(x, z + dz) - h) / dz;
                let slope = 1.0 - 1.0 / (1.0 + sx * sx + sz * sz).sqrt();
                if !h.is_finite() || class.blocks(slope, -h) {
                    return false;
                }
            }
        }
    }
    true
}

fn affordable(value: f32, eos: usize) -> bool {
    // Preserve the ordinary 1000-value/600-metal Eos exchange rate for
    // the entire package, and leave a pad for the Quake.
    eos > 0 && eos < PADS && value >= (COST + MISSILE_COST * eos as f32) * WORTH / MISSILE_COST
}

impl Ai {
    fn missiles_quake_safe(&self, at: Vec3) -> bool {
        let map = self.cb.map();
        at.x >= GATHER
            && at.z >= GATHER
            && at.x < map.width() as f32 * 8.0 - GATHER
            && at.z < map.height() as f32 * 8.0 - GATHER
            && self.missiles_shield(at).is_none()
            && self.cb.friendly_units_in(at, RADIUS + 80.0).is_empty()
            && !self.eco.ditched.iter().any(|&p| distance(p, at) < RADIUS + 80.0)
            && !self.eco.ditch.as_ref().is_some_and(|d| distance(d.mid, at) < RADIUS + 80.0)
            && !self.missiles.seismic.tried.iter().any(|&(p, f)| self.frame - f < 30 * 600 && distance(p, at) < RADIUS)
    }

    fn missiles_quake_valid(&self, plan: &Opportunity, from: Vec3) -> bool {
        if !self.missiles_quake_safe(plan.at) || !self.missiles_reaches(QUAKE, from, plan.at) {
            return false;
        }
        let map = self.cb.map();
        let ground = |x, z| map.elevation_at(x, z);
        let quake = Quake::new(plan.at, &ground);
        match plan.purpose {
            Purpose::Eos { id, needed } => {
                let Some(t) =
                    self.missiles_eos_targets().into_iter().find(|t| t.id == id && t.launch && t.needed <= needed)
                else {
                    return false;
                };
                if !self.war.targets.get(&id).is_some_and(|e| e.is_static)
                    || !affordable(t.value, needed)
                    || self.missiles_reaches(EOS, from, t.pos)
                {
                    return false;
                }
                let Some(flight) = flight::Flight::for_kind(self, EOS) else { return false };
                let mut to = t.pos;
                to.y = quake.height(to.x, to.z, &ground, false);
                flight.predict(from, to, |x, z| quake.height(x, z, &ground, false)).is_some_and(|p| p.blocked.is_none())
            }
            Purpose::Ramp { ends, class, tier } => {
                let near_army: f32 = self
                    .fighters
                    .values()
                    .filter(|f| {
                        distance(f.get_pos(), plan.at) < 1200.0
                            && f.unit.def().and_then(crate::terrain::tier_of) == Some(tier)
                    })
                    .map(|f| f.metal_value)
                    .sum();
                near_army >= 3000.0
                    && self.war.get_effective_threat(plan.at) <= 0.0
                    && !corridor(ends, class, ground)
                    && corridor(ends, class, |x, z| quake.height(x, z, &ground, false))
                    && corridor(ends, class, |x, z| quake.height(x, z, &ground, true))
            }
        }
    }

    pub(super) fn missiles_refresh_seismic(&mut self, silo: Unit) {
        if let Some((p, due)) = &self.missiles.seismic.pending {
            if self.frame < *due {
                return;
            }
            let open = match p.purpose {
                Purpose::Eos { id, .. } => self
                    .war
                    .targets
                    .get(&id)
                    .is_none_or(|e| e.is_dead || self.missiles_reaches(EOS, silo.pos(), e.position)),
                Purpose::Ramp { ends, class, .. } => corridor(ends, class, |x, z| self.cb.map().elevation_at(x, z)),
            };
            self.log(&format!(
                "missiles: Quake impact checked: route {}",
                if open { "open" } else { "still blocked; no speculative follow-up" }
            ));
            self.missiles.seismic.pending = None;
        }
        if self.missiles.seismic.checked.is_some_and(|f| self.frame - f < PERIOD) {
            return;
        }
        self.missiles.seismic.checked = Some(self.frame);
        self.missiles.seismic.plan = None;
        self.missiles.seismic.tried.retain(|&(_, f)| self.frame - f < 30 * 600);
        if self.eco.adjusted_income < SILO_INCOME
            || self.eco.hard_estalled
            || self.missiles.seismic.tried.last().is_some_and(|&(_, f)| self.frame - f < COOLDOWN)
        {
            return;
        }
        let from = silo.pos();
        let mut plans = Vec::new();
        for t in self.missiles_eos_targets().into_iter().filter(|t| t.launch && affordable(t.value, t.needed)).take(16)
        {
            let Some(p) = self.missiles_prediction(EOS, from, t.pos).and_then(|p| p.blocked) else { continue };
            // Strike the actual obstruction, not the valuable target behind it.
            let plan = Opportunity { at: p, purpose: Purpose::Eos { id: t.id, needed: t.needed }, score: t.value };
            if self.missiles_quake_valid(&plan, from) {
                plans.push(plan);
            }
        }
        // Analyze the movement tier of our actual army; strategy.tier is the
        // enemy's tier and cannot authorize a route for our invading units.
        let mut tiers = BTreeMap::new();
        for f in self.fighters.values() {
            if let Some(tier) = f.unit.def().and_then(crate::terrain::tier_of) {
                *tiers.entry(tier).or_insert(0.0f32) += f.metal_value;
            }
        }
        if let Some((&tier, _)) = tiers.iter().max_by(|a, b| a.1.total_cmp(b.1))
            && let Some(map) = self.terrain().and_then(|t| t.tier(tier))
            && let Some((ally, enemy)) = self.strategy_bases()
        {
            let assets = self.strategy_assets();
            let analysis = Analysis::run(&Scenario {
                map,
                ally_base: Pos::new(ally.x, ally.z),
                enemy_base: Pos::new(enemy.x, enemy.z),
                assets: &assets,
            });
            let class = self
                .fighters
                .values()
                .filter_map(|f| f.unit.def())
                .filter(|&d| crate::terrain::tier_of(d) == Some(tier))
                .filter_map(crate::terrain::move_class)
                .map(MoveClass::soft)
                .reduce(|a, b| MoveClass {
                    max_slope: a.max_slope.min(b.max_slope),
                    max_water_depth: a.max_water_depth.min(b.max_water_depth),
                    min_water_depth: a.min_water_depth.max(b.min_water_depth),
                    ..a
                })
                .unwrap_or_else(|| tier.zero_k_class().soft());
            for b in analysis.bridges.iter().filter(|b| b.enemy_exposed > 0.0).take(12) {
                let ends = b.ends.map(|p| Vec3::xz(p.x, p.z));
                let mut at = Vec3::xz((ends[0].x + ends[1].x) * 0.5, (ends[0].z + ends[1].z) * 0.5);
                at.y = self.cb.map().elevation_at(at.x, at.z);
                let plan = Opportunity { at, purpose: Purpose::Ramp { ends, class, tier }, score: b.score };
                if self.missiles_quake_valid(&plan, from) {
                    plans.push(plan);
                }
            }
        }
        self.missiles.seismic.plan = plans.into_iter().max_by(|a, b| a.score.total_cmp(&b.score));
    }

    pub(super) fn missiles_quake_demand(&self) -> Option<UnitDef> {
        (self.missiles.seismic.plan.is_some() && self.missiles.seismic.pending.is_none())
            .then(|| self.defs.get(QUAKE))
            .flatten()
    }

    pub(super) fn missiles_fire_quake(&mut self) {
        if self.missiles.seismic.pending.is_some() {
            return;
        }
        let Some(plan) = self.missiles.seismic.plan.clone() else { return };
        let Some(missile) =
            self.missiles_ready(QUAKE).into_iter().find(|m| self.missiles_reaches(QUAKE, m.pos(), plan.at))
        else {
            return;
        };
        if !self.missiles_quake_valid(&plan, missile.pos()) {
            self.missiles.seismic.plan = None;
            return;
        }
        if let Purpose::Eos { needed, .. } = plan.purpose
            && self.missiles_ready(EOS).iter().filter(|m| self.missiles_quake_valid(&plan, m.pos())).count() < needed
        {
            return;
        }
        let due =
            self.frame + self.missiles_prediction(QUAKE, missile.pos(), plan.at).unwrap().frames.ceil() as i32 + 90;
        self.missiles_launch(missile, plan.at);
        self.log(&format!(
            "missiles: Quake at ({:.0}, {:.0}) to open {}; checking real terrain after impact",
            plan.at.x,
            plan.at.z,
            if matches!(plan.purpose, Purpose::Eos { .. }) { "an Eos trajectory" } else { "an invasion ramp" }
        ));
        self.missiles.seismic.tried.push((plan.at, self.frame));
        self.missiles.seismic.pending = Some((plan, due));
        self.missiles.seismic.plan = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn quake_uses_local_mean_and_falloff_without_creating_land_bridges() {
        let at = Vec3::xz(1024.0, 1024.0);
        let ground = |x: f32, _: f32| if (x - 1024.0).abs() < 40.0 { 100.0 } else { 0.0 };
        let q = Quake::new(at, &ground);
        assert!(q.mean > 0.0 && q.mean < 20.0);
        assert!((q.height(at.x, at.z, &ground, false) - q.mean).abs() < 0.001);
        assert_eq!(q.height(1440.0, 1024.0, &ground, false), 0.0);
        assert_eq!(q.height(1104.0, 1024.0, &ground, true), 0.0);
        assert!(q.height(1104.0, 1024.0, &ground, false) > 0.0);
    }
    #[test]
    fn one_quake_opens_a_low_narrow_ridge_but_not_a_deep_land_bridge() {
        let at = Vec3::xz(1024.0, 1024.0);
        let ends = [Vec3::xz(960.0, 1024.0), Vec3::xz(1088.0, 1024.0)];
        let ridge = |x: f32, _: f32| if (x - 1024.0).abs() <= 8.0 { 12.0 } else { 0.0 };
        let class = Tier::Vehicle.zero_k_class().soft();
        assert!(!corridor(ends, class, ridge));
        let q = Quake::new(at, &ridge);
        assert!(corridor(ends, class, |x, z| q.height(x, z, &ridge, true)));
        let sea = |x: f32, _: f32| if (x - 1024.0).abs() <= 40.0 { -100.0 } else { 0.0 };
        let q = Quake::new(at, &sea);
        assert!(!corridor(ends, class, |x, z| q.height(x, z, &sea, true)));
    }

    #[test]
    fn corridor_rejects_cross_slopes_water_and_unaffordable_packages() {
        let ends = [Vec3::xz(500.0, 500.0), Vec3::xz(700.0, 500.0)];
        let class = Tier::Vehicle.zero_k_class();
        assert!(corridor(ends, class, |_, _| 100.0));
        assert!(!corridor(ends, class, |_, z| 100.0 + (z - 500.0).abs() * 4.0));
        assert!(!corridor(ends, class, |_, _| -100.0));
        assert!(!affordable(1000.0, 1));
        assert!(affordable(2000.0, 1));
        assert!(!affordable(10000.0, 4));
    }
}
