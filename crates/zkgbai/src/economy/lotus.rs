//! Deferred Lotus demand. Any overlapping Lotus becomes a request for
//! investment, not another turret. Distinct requests pool their metal value.

use springai::{UnitDef, Vec3};
use zk_terrain::Pos;

use super::{List, WorkerTask};
use crate::{
    Ai,
    terrain::pos,
    util::{distance, kgb_rand},
};

const PERIOD: i32 = 150;
const AREA_REACH: f32 = 460.0;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum DemandKey {
    Gate(usize, usize),
    Asset(i32, i32),
    Area(i32, i32),
}

pub(super) struct Demand {
    key: DemandKey,
    at: Vec3,
    targets: Vec<Pos>,
    metal: f32,
    until: i32,
}

/// Circles overlap even when neither turret centre is in the other's
/// range. Tangency is allowed; any shared area rejects the new Lotus.
fn overlaps(a: Pos, b: Pos, range: f32) -> bool {
    a.distance(b) < 2.0 * range
}

/// Choose a turret type only after both site searches have established
/// affordability, useful coverage and safe access. No coin flip can make
/// an invalid choice win or stall the one valid alternative.
fn random_upgrade<T>(stinger: Option<T>, gauss: Option<T>, roll: impl FnOnce() -> f64) -> Option<T> {
    match (stinger, gauss) {
        (Some(s), Some(g)) => Some(if roll() < 0.5 { s } else { g }),
        (s, g) => s.or(g),
    }
}

fn remember(book: &mut Vec<Demand>, demand: Demand) {
    if let Some(old) = book.iter_mut().find(|d| {
        d.key == demand.key
            || (matches!(d.key, DemandKey::Area(..))
                && matches!(demand.key, DemandKey::Area(..))
                && distance(d.at, demand.at) < AREA_REACH)
    }) {
        // A planner polling or moving a seed does not create more money.
        old.until = demand.until;
        if !matches!(old.key, DemandKey::Area(..)) {
            old.at = demand.at;
            old.targets = demand.targets;
            old.metal = demand.metal;
        }
        return;
    }
    if book.len() < 256 {
        book.push(demand);
    }
}

impl Ai {
    pub(super) fn eco_lotus_crowded(&self, at: Vec3) -> bool {
        if self.dev_off("fortify") {
            return false;
        }
        let Some(lotus) = self.defs.get("turretlaser") else { return false };
        let range = lotus.max_weapon_range();
        self.eco_fire_positions().into_iter().any(|(d, p, _)| d.id() == lotus.id() && overlaps(pos(at), pos(p), range))
    }

    pub(super) fn eco_defer_lotus(&mut self, key: DemandKey, at: Vec3, targets: Vec<Pos>) {
        let Some(lotus) = self.defs.get("turretlaser") else { return };
        let until = match key {
            DemandKey::Gate(..) => self.frame + 2 * self.eco_fortify_plan_period(),
            DemandKey::Area(..) => self.frame + 30 * 600,
            DemandKey::Asset(..) => i32::MAX,
        };
        remember(&mut self.eco.lotus_demand, Demand { key, at, targets, metal: lotus.cost(self.metal), until });
    }

    /// True consumes the request, including when it must wait for more demand.
    pub(super) fn eco_defer_point_lotus(&mut self, def: UnitDef, asset: Vec3, preferred: Vec3) -> bool {
        if def.name() != "turretlaser" || self.dev_off("fortify") {
            return false;
        }
        let site = self.eco_build_site(def, preferred, 0);
        if site.x < 0.0 || !self.eco_lotus_crowded(site) {
            return false;
        }
        let key = DemandKey::Asset((asset.x / 16.0).round() as i32, (asset.z / 16.0).round() as i32);
        self.eco_defer_lotus(key, asset, super::fortify::asset_front(asset).to_vec());
        true
    }

    pub(super) fn eco_disable_lotus_pool(&mut self) {
        let book = std::mem::take(&mut self.eco.lotus_demand);
        let Some(lotus) = self.defs.get("turretlaser") else { return };
        let defenses = self.eco_fire_positions();
        let assets: Vec<_> = self.cb.team_units().into_iter().filter(|u| u.health() > 0.0).map(|u| u.pos()).collect();
        for request in book.into_iter().filter(|d| matches!(d.key, DemandKey::Asset(..))) {
            if !assets.iter().any(|&p| distance(p, request.at) < 32.0)
                || defenses.iter().any(|(d, p, _)| self.eco_lotus_upgrade_covers(*d, *p, &request.targets, lotus))
            {
                continue;
            }
            let site = self.eco_build_site(lotus, request.at, 0);
            if site.x >= 0.0 {
                self.eco_add_simple(lotus, site, List::Porc, Some(List::Porc));
            }
        }
    }

    pub(super) fn eco_plan_lotus_upgrades(&mut self, frame: i32) {
        if self.dev_off("fortify") || frame % PERIOD != self.offset % PERIOD {
            return;
        }
        let Some(lotus) = self.defs.get("turretlaser") else { return };
        if self.eco.lotus_demand.is_empty() {
            return;
        }
        let defenses = self.eco_fire_positions();
        let gate_layers = self.eco_unmet_gate_layers();
        let assets: Vec<_> = if self.eco.lotus_demand.iter().any(|d| matches!(d.key, DemandKey::Asset(..))) {
            self.cb.team_units().into_iter().filter(|u| u.health() > 0.0).map(|u| u.pos()).collect()
        } else {
            Vec::new()
        };
        // Finished upgrades retire demand. Queued upgrades reserve it; if
        // cancelled, the same requests become available again below.
        let book = std::mem::take(&mut self.eco.lotus_demand);
        self.eco.lotus_demand = book
            .into_iter()
            .filter(|d| {
                d.until >= frame
                    && match d.key {
                        DemandKey::Gate(choke, layer) => gate_layers.contains(&(choke, layer)),
                        _ => true,
                    }
                    && (!matches!(d.key, DemandKey::Asset(..)) || assets.iter().any(|&p| distance(p, d.at) < 32.0))
                    && !self.eco.porcs.iter().any(|u| {
                        !u.is_being_built()
                            && u.health() > 0.0
                            && u.def().is_some_and(|def| self.eco_lotus_upgrade_covers(def, u.pos(), &d.targets, lotus))
                    })
            })
            .collect();
        let active: Vec<_> = self
            .eco
            .lotus_demand
            .iter()
            .enumerate()
            .filter(|(_, d)| {
                !defenses.iter().any(|(def, p, _)| self.eco_lotus_upgrade_covers(*def, *p, &d.targets, lotus))
            })
            .map(|(i, _)| i)
            .collect();
        if active.is_empty() {
            return;
        }
        let posture = self.posture_on();
        let tempo = self.eco_fortify_tempo();
        if posture && self.posture_waiting() >= (self.fac.num_workers / 2).max(2) as usize {
            return;
        }
        if self.eco.adjusted_income < 25.0 {
            return;
        }
        let mut choices = [None, None];
        for (index, name) in ["turretheavylaser", "turretgauss"].into_iter().enumerate() {
            let mut best: Option<(f32, UnitDef, Vec3, bool, f32)> = None;
            let Some(def) = self.defs.get(name) else { continue };
            let cost = def.cost(self.metal);
            if posture && cost > self.posture_deficit() {
                continue;
            }
            let points: Vec<_> = active.iter().map(|&i| pos(self.eco.lotus_demand[i].at)).collect();
            for seed in super::fortify::seeds(&points, def.max_weapon_range()) {
                let site = self.cb.map().find_closest_build_site(def, Vec3::xz(seed.x, seed.z), 160.0, 3, 0);
                if site.x < 0.0 || !self.eco_build_check(def, site, 0) || (posture && self.posture_shuns(site)) {
                    continue;
                }
                let requests: Vec<_> = active
                    .iter()
                    .map(|&i| &self.eco.lotus_demand[i])
                    .filter(|d| self.eco_lotus_upgrade_covers(def, site, &d.targets, lotus))
                    .collect();
                let budget: f32 = requests.iter().map(|d| d.metal).sum();
                if budget * tempo < cost {
                    continue;
                }
                let Some(travel) = self.eco_shared_site_travel(def, site) else { continue };
                let score = budget * tempo * super::fortify::travel_discount(travel) / cost.max(1.0);
                let point = requests.iter().any(|d| !matches!(d.key, DemandKey::Gate(..)));
                if best.is_none_or(|b| score > b.0) {
                    best = Some((score, def, site, point, budget));
                }
            }
            choices[index] = best;
        }
        let [stinger, gauss] = choices;
        let Some((_, def, site, point, budget)) = random_upgrade(stinger, gauss, kgb_rand) else { return };
        let mut task = WorkerTask::construction(def, site, 0);
        let c = task.as_construction_mut().unwrap();
        c.fortification = true;
        c.point_defense = point;
        c.frame_issued = if posture { 0 } else { frame };
        if let Some(id) = self.eco_add_construction(task, Some(List::Porc), Some(List::Porc)) {
            self.eco.fortify_tasks.insert(id);
            if posture {
                let cost = def.cost(self.metal);
                self.posture_ordered(id, site, cost, cost * 2.0, "fortify: pooled Lotus demand");
                self.posture.defense += cost;
            }
            self.log(&format!(
                "terrain: pooled {budget:.0} metal of Lotus requests (tempo weight {tempo:.2}) into {} at ({:.0}, {:.0})",
                def.name(),
                site.x,
                site.z
            ));
        }
    }

    fn eco_lotus_upgrade_covers(&self, def: UnitDef, site: Vec3, targets: &[Pos], lotus: UnitDef) -> bool {
        matches!(def.name().as_str(), "turretheavylaser" | "turretgauss")
            && def.cost(self.metal) > lotus.cost(self.metal)
            && targets.iter().all(|p| self.eco_fire_coverage(def, site, &[*p])[0])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn even_a_small_overlap_with_one_lotus_is_forbidden() {
        let a = Pos::new(0.0, 0.0);
        // Both centres outside the other's range, yet their circles overlap.
        assert!(overlaps(a, Pos::new(700.0, 0.0), 460.0));
        assert!(overlaps(a, Pos::new(919.9, 0.0), 460.0));
        assert!(overlaps(a, a, 460.0));
        assert!(!overlaps(a, Pos::new(920.0, 0.0), 460.0));
        assert!(!overlaps(a, Pos::new(921.0, 0.0), 460.0));
    }

    #[test]
    fn upgrades_randomize_between_valid_types_without_discarding_a_valid_fallback() {
        assert_eq!(random_upgrade(Some("stinger"), Some("gauss"), || 0.25), Some("stinger"));
        assert_eq!(random_upgrade(Some("stinger"), Some("gauss"), || 0.75), Some("gauss"));
        assert_eq!(random_upgrade(Some("stinger"), None, || panic!("only one eligible type")), Some("stinger"));
        assert_eq!(random_upgrade(None, Some("gauss"), || panic!("only one eligible type")), Some("gauss"));
        assert_eq!(random_upgrade::<&str>(None, None, || panic!("no eligible types")), None);
    }

    #[test]
    fn distinct_requests_accumulate_but_polling_and_jitter_do_not() {
        let mut book = Vec::new();
        for frame in 0..10 {
            for gate in 0..5 {
                remember(
                    &mut book,
                    Demand {
                        key: DemandKey::Gate(gate, 2),
                        at: Vec3::xz(gate as f32 * 80.0, 0.0),
                        targets: Vec::new(),
                        metal: 90.0,
                        until: frame,
                    },
                );
            }
        }
        assert_eq!(book.len(), 5);
        assert_eq!(book.iter().map(|d| d.metal).sum::<f32>(), 450.0);
        assert!(book.iter().take(4).map(|d| d.metal).sum::<f32>() < 420.0);
        for x in [100.0, 130.0, 250.0] {
            remember(
                &mut book,
                Demand {
                    key: DemandKey::Area(x as i32, 0),
                    at: Vec3::xz(x, 0.0),
                    targets: Vec::new(),
                    metal: 90.0,
                    until: 10,
                },
            );
        }
        assert_eq!(book.len(), 6);
    }
}
