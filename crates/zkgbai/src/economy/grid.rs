//! The energy grid as the game sees it (`LuaRules/Gadgets/
//! unit_mex_overdrive.lua`): buildings with a pylon range link when closer
//! than their two ranges together; a grid's capacity is the energy its
//! members make; a building that needs power (`neededlink`: superweapons,
//! Berthas, heavy turrets) works only on a grid with at least that much.
//!
//! The economy's own pylon building links metal spots as the territory
//! grows. On top of it, this planner links buildings starved of power to
//! the strongest grid, and late in the game joins every grid that makes
//! energy (or needs it) into one along a minimum spanning tree of pylon
//! chains. Chains are laid a pylon at a time and checked against the grid
//! as built, so a misplaced pylon is made up for next time.
//!
//! A building that needs power (a Bertha, a superweapon) is not started
//! until its site is on a grid making enough: the pylons go first, so 6000
//! metal does not stand idle for minutes waiting to be linked (see
//! [`Ai::eco_add_powered`]).

use std::collections::HashMap;

use springai::{UnitDef, Vec3};

use super::List;
use super::tasks::WorkerTask;
use crate::Ai;
use crate::util::distance;

/// Frames between plans.
const GRID_PERIOD: i32 = 30 * 10;

/// Pylons queued at most at a time; more while a building waits for its
/// site to have power.
const MAX_PYLON_TASKS: usize = 3;
const MAX_PYLON_TASKS_WAITING: usize = 6;

/// Grids making less energy than this (metal spots, a few solars) are left
/// to the economy's own pylon building.
const MIN_GRID_ENERGY: f32 = 20.0;

/// Pylons in a chain stand this share of their reach apart.
const CHAIN_SLACK: f32 = 0.85;

/// A planned pylon is already there if a pylon (or one queued) is this
/// close, elmos.
const PYLON_SAME: f32 = 250.0;

/// Frames a building waits for its site to have power before it is started
/// anyway.
const POWER_WAIT: i32 = 30 * 180;

/// A building that needs power, waiting for its site to be linked.
pub struct PendingPower {
    def: UnitDef,
    pos: Vec3,
    list: List,
    since: i32,
}

/// Grid ranges and power needs of the unit types that have them.
#[derive(Default)]
pub struct GridDefs {
    /// Pylon range and power needed (0 if none), by unit def id.
    by_def: HashMap<i32, (f32, f32)>,
    /// The pylon's own range.
    pylon_range: f32,
}

impl GridDefs {
    pub fn new(cb: springai::Callback) -> Self {
        let mut by_def = HashMap::new();
        let mut pylon_range = 0.0;
        for def in cb.unit_defs() {
            let param = |key: &str| def.custom_param(key).and_then(|v| v.trim().parse::<f32>().ok());
            if let Some(range) = param("pylonrange").filter(|&r| r > 0.0) {
                by_def.insert(def.id(), (range, param("neededlink").unwrap_or(0.0)));
                if def.name() == "energypylon" {
                    pylon_range = range;
                }
            }
        }
        GridDefs { by_def, pylon_range }
    }

    /// Power a unit type needs from its grid (0 if none).
    pub fn needs(&self, def_id: i32) -> f32 {
        self.by_def.get(&def_id).map_or(0.0, |&(_, n)| n)
    }
}

/// One of our grid members (or a building's site waiting for power).
struct Member {
    what: String,
    pos: Vec3,
    range: f32,
    energy: f32,
    needs: f32,
}

/// Disjoint sets over members.
fn find(parent: &mut [usize], mut i: usize) -> usize {
    while parent[i] != i {
        parent[i] = parent[parent[i]];
        i = parent[i];
    }
    i
}

impl Ai {
    /// Energy our buildings make (what a single grid could have).
    pub fn eco_energy_production(&self) -> f32 {
        let eco = &self.eco;
        eco.solars
            .iter()
            .chain(&eco.windgens)
            .chain(&eco.fusions)
            .map(|u| u.rules_param_float("current_energyIncome", 0.0))
            .sum()
    }

    /// Queue `def` at `pos` (on task list `list`); one that needs power
    /// waits until its site has it, while the planner links it up.
    pub(super) fn eco_add_powered(&mut self, def: UnitDef, pos: Vec3, list: List) {
        let needs = self.eco.grid_defs.needs(def.id());
        if needs <= 0.0 || self.dev_off("grid") {
            self.eco_start_powered(def, pos, list);
            return;
        }
        if self.eco.pending_power.is_none() {
            self.log(&format!(
                "grid: linking the site of a {} at ({:.0}, {:.0}) to {needs:.0} energy before building it",
                self.defs.name(def),
                pos.x,
                pos.z
            ));
            self.eco.pending_power = Some(PendingPower { def, pos, list, since: self.frame });
        }
    }

    /// Whether a building waits for its site to have power.
    pub(super) fn eco_awaiting_power(&self) -> bool {
        self.eco.pending_power.is_some()
    }

    /// The building that waits for its site to have power, and where.
    pub(crate) fn eco_awaiting(&self) -> Option<(UnitDef, Vec3)> {
        self.eco.pending_power.as_ref().map(|p| (p.def, p.pos))
    }

    fn eco_start_powered(&mut self, def: UnitDef, pos: Vec3, list: List) {
        let mut ct = WorkerTask::construction(def, pos, 0);
        if let Some(c) = ct.as_construction_mut() {
            c.frame_issued = self.frame;
        }
        self.eco_add_construction(ct, Some(list), Some(list));
    }

    /// Link starved buildings to power, and (late) the grids to each other.
    pub fn eco_plan_grid(&mut self, frame: i32) {
        if (frame + self.offset + 13) % GRID_PERIOD != 0 || self.dev_off("grid") {
            return;
        }
        let gd = &self.eco.grid_defs;
        let pr = gd.pylon_range;
        if pr <= 0.0 {
            return;
        }
        let mut members: Vec<Member> = self
            .cb
            .team_units()
            .into_iter()
            .filter(|u| u.health() > 0.0)
            .filter_map(|u| {
                let &(range, needs) = gd.by_def.get(&u.def()?.id())?;
                // Unfinished buildings are no grid members yet, but those
                // that will need power are planned for already.
                if u.is_being_built() && needs <= 0.0 {
                    return None;
                }
                let energy = u.rules_param_float("current_energyIncome", 0.0);
                Some(Member { what: self.defs.name_of(u).to_owned(), pos: u.pos(), range, energy, needs })
            })
            .collect();
        // The site of a building waiting for power, as if it stood there.
        let pending = self.eco.pending_power.as_ref().and_then(|p| {
            let &(range, needs) = gd.by_def.get(&p.def.id())?;
            members.push(Member {
                what: format!("{} site", self.defs.name(p.def)),
                pos: p.pos,
                range,
                energy: 0.0,
                needs,
            });
            Some(members.len() - 1)
        });

        // The grids, and what each makes.
        let n = members.len();
        let mut parent: Vec<usize> = (0..n).collect();
        for i in 0..n {
            for j in i + 1..n {
                if distance(members[i].pos, members[j].pos) < members[i].range + members[j].range {
                    let (a, b) = (find(&mut parent, i), find(&mut parent, j));
                    parent[a] = b;
                }
            }
        }
        let grid: Vec<usize> = (0..n).map(|i| find(&mut parent, i)).collect();
        let mut capacity = vec![0.0f32; n];
        for (i, m) in members.iter().enumerate() {
            capacity[grid[i]] += m.energy;
        }
        // A waiting building starts once its site has power (or has waited
        // long enough).
        if let (Some(i), Some(p)) = (pending, self.eco.pending_power.as_ref()) {
            let (has, needs) = (capacity[grid[i]], members[i].needs);
            let (def, pos, list, since) = (p.def, p.pos, p.list, p.since);
            if has >= needs || frame - since > POWER_WAIT {
                self.eco.pending_power = None;
                self.log(&format!(
                    "grid: the {} at ({:.0}, {:.0}) {}; building it",
                    members[i].what,
                    pos.x,
                    pos.z,
                    if has >= needs {
                        format!("has {has:.0} of {needs:.0} energy")
                    } else {
                        format!("waited {} s for power", (frame - since) / 30)
                    }
                ));
                self.eco_start_powered(def, pos, list);
            }
        } else if self.eco.pending_power.is_some() {
            self.eco.pending_power = None;
        }
        if self.eco.pylon_tasks.len() >= self.eco_max_pylon_tasks() {
            return;
        }

        // Grids worth linking: those making energy, and those needing it.
        let mut of: HashMap<usize, Vec<usize>> = HashMap::new();
        for (i, &g) in grid.iter().enumerate() {
            of.entry(g).or_default().push(i);
        }
        of.retain(|&g, list| capacity[g] >= MIN_GRID_ENERGY || list.iter().any(|&i| members[i].needs > 0.0));
        let mut nodes: Vec<usize> = of.keys().copied().collect();
        nodes.sort_unstable();
        // The shortest link between two grids: (pylons needed, members).
        let link = |a: usize, b: usize| {
            let (mut best, mut ends) = (f32::INFINITY, (0, 0));
            for &i in &of[&a] {
                for &j in &of[&b] {
                    let gap = distance(members[i].pos, members[j].pos)
                        - CHAIN_SLACK * (members[i].range + members[j].range + 2.0 * pr);
                    if gap < best {
                        best = gap;
                        ends = (i, j);
                    }
                }
            }
            (1.0 + (best.max(0.0) / (2.0 * pr * CHAIN_SLACK)).ceil(), ends)
        };

        // First the starved: the building needing the most power, linked to
        // the grid that makes up its shortfall the cheapest (or else the
        // strongest).
        // A building waiting for power goes first.
        let starved = pending.filter(|&i| members[i].needs > capacity[grid[i]]).or_else(|| {
            (0..n)
                .filter(|&i| members[i].needs > capacity[grid[i]])
                .max_by(|&a, &b| members[a].needs.total_cmp(&members[b].needs))
        });
        let plan = if let Some(s) = starved {
            let (g, needs) = (grid[s], members[s].needs);
            let options: Vec<(usize, f32, (usize, usize))> = nodes
                .iter()
                .filter(|&&h| h != g && capacity[h] > 0.0)
                .map(|&h| {
                    let (pylons, ends) = link(g, h);
                    (h, pylons, ends)
                })
                .collect();
            let enough =
                options.iter().filter(|o| capacity[g] + capacity[o.0] >= needs).min_by(|a, b| a.1.total_cmp(&b.1));
            let strongest = options.iter().max_by(|a, b| capacity[a.0].total_cmp(&capacity[b.0]));
            enough
                .or(strongest)
                .map(|o| (o.2, format!("{} needing {needs:.0} energy (has {:.0})", members[s].what, capacity[g])))
        } else if self.eco.fusions.len() >= 2 && nodes.len() > 1 {
            // Late game: the cheapest link of a minimum spanning tree over
            // the grids (Kruskal; the tree's other links come later).
            let mut edges = Vec::new();
            for (k, &a) in nodes.iter().enumerate() {
                for &b in &nodes[k + 1..] {
                    let (pylons, ends) = link(a, b);
                    edges.push((pylons, ends, a, b));
                }
            }
            edges.sort_by(|x, y| x.0.total_cmp(&y.0));
            edges.first().map(|e| (e.1, format!("grids making {:.0} and {:.0} energy", capacity[e.2], capacity[e.3])))
        } else {
            None
        };
        let Some(((i, j), why)) = plan else { return };
        drop(of);
        let (a, b) = (&members[i], &members[j]);
        self.eco_lay_pylons(a.pos, a.range, b.pos, b.range, &why);
    }

    fn eco_max_pylon_tasks(&self) -> usize {
        if self.eco.pending_power.is_some() { MAX_PYLON_TASKS_WAITING } else { MAX_PYLON_TASKS }
    }

    /// Queue the pylons of a chain from `a` (range `ra`) to `b` that are not
    /// there yet.
    fn eco_lay_pylons(&mut self, a: Vec3, ra: f32, b: Vec3, rb: f32, why: &str) {
        let Some(pylon) = self.defs.get("energypylon") else { return };
        let mut queued = 0;
        for p in chain(a, ra, b, rb, self.eco.grid_defs.pylon_range) {
            if self.eco.pylon_tasks.len() >= self.eco_max_pylon_tasks() {
                break;
            }
            let there = self.eco.pylons.iter().any(|u| distance(u.pos(), p) < PYLON_SAME)
                || self.eco.pylon_tasks.iter().any(|&t| distance(self.eco.task_pos(t), p) < PYLON_SAME);
            if !there {
                let site = self.eco_build_site(pylon, p, 0);
                if self.eco_add_simple(pylon, site, List::Pylon, Some(List::Pylon)).is_some() {
                    queued += 1;
                }
            }
        }
        if queued > 0 {
            self.log(&format!("grid: {queued} pylons to link {why}"));
        }
    }
}

/// Where the pylons (of range `pr`) of a chain from `a` (range `ra`) to `b`
/// (range `rb`) stand: the first within reach of `a`, each next within
/// reach of the last, the last within reach of `b`.
fn chain(a: Vec3, ra: f32, b: Vec3, rb: f32, pr: f32) -> Vec<Vec3> {
    let d = distance(a, b);
    if d <= 0.0 || pr <= 0.0 {
        return Vec::new();
    }
    let (ux, uz) = ((b.x - a.x) / d, (b.z - a.z) / d);
    let mut along = (CHAIN_SLACK * (ra + pr)).min(d);
    let mut points = Vec::new();
    loop {
        points.push(Vec3::xz(a.x + ux * along, a.z + uz * along));
        if d - along <= CHAIN_SLACK * (rb + pr) {
            return points;
        }
        along = (along + 2.0 * CHAIN_SLACK * pr).min(d);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chains_reach_end_to_end() {
        let (a, b) = (Vec3::xz(0.0, 0.0), Vec3::xz(3000.0, 400.0));
        let (ra, rb, pr) = (150.0, 50.0, 500.0);
        let points = chain(a, ra, b, rb, pr);
        assert!(distance(a, points[0]) < ra + pr);
        assert!(points.windows(2).all(|w| distance(w[0], w[1]) < 2.0 * pr));
        assert!(distance(*points.last().unwrap(), b) < rb + pr);
        // Four pylons cover the 3 km, with slack (each reaching 850 elmos
        // on), as many as the link estimate counts.
        assert_eq!(points.len(), 4);
        // Close by, one does.
        assert_eq!(chain(a, ra, Vec3::xz(800.0, 0.0), rb, pr).len(), 1);
    }
}
