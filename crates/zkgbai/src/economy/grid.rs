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
//! Overdrive (feature `gridmex`, on by default): late in the game the
//! economy's own linking stops short. Its small energy goes up only while
//! energy stalls, which the plants end early, and its pylons stay within
//! 1500 of a plant, all in the base: on Folsom 20 of 33 mexes stood on no
//! grid next to three singularities (2026-10-27). So in the overdrive
//! phase this planner links grids by what they gain instead. The gadget
//! gives a mex of base income `b` and energy `e` an extra
//! `b * 0.25 * sqrt(e)`, the energy shared in proportion to `b²`, so a grid
//! spending `c` on mexes with `S = sum b²` overdrives `0.25 * sqrt(c * S)`.
//! The link gaining most per pylon goes first, unless its pylons pay back
//! slower than [`MEX_PAYBACK`].
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

/// Seconds a link's pylons may take to pay for themselves in overdrive
/// metal (`gridmex`).
const MEX_PAYBACK: f32 = 600.0;

/// Share of the energy made that overdrive gets at least, whatever the
/// spending of the moment (`gridmex`).
const MIN_SPARE: f32 = 0.25;

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
    pub pylon_range: f32,
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

    /// A unit type's pylon range, if it links.
    pub fn range(&self, def_id: i32) -> Option<f32> {
        self.by_def.get(&def_id).map(|&(r, _)| r)
    }
}

/// What power a site has (see [`Ai::eco_grid_access`]).
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum GridAccess {
    /// The grids it would join make this much, enough.
    Powered(f32),
    /// Not enough, but `pylons` pylons in all, chains from the members at
    /// `links` (where, pylon range), link it to grids making `energy`
    /// together.
    Link { links: Vec<(Vec3, f32)>, pylons: f32, energy: f32 },
    /// Not enough and no grid that makes up the rest: what it would have.
    Unpowered(f32),
}

/// One of our grid members (or a building's site waiting for power).
struct Member {
    what: String,
    pos: Vec3,
    range: f32,
    energy: f32,
    needs: f32,
    /// A mex's base income (its spot's metal), 0 for the rest.
    metal: f32,
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
            .chain(&eco.geos)
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
        // An ally may have started one meanwhile (see `allied`).
        if self.eco_assist_ally(def) {
            return;
        }
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
        let mex = self.eco.build_ids.mex_id;
        let mut members: Vec<Member> = self
            .cb
            .team_units()
            .into_iter()
            .filter(|u| u.health() > 0.0)
            .filter_map(|u| {
                let id = u.def()?.id();
                let &(range, needs) = gd.by_def.get(&id)?;
                // Unfinished buildings are no grid members yet, but those
                // that will need power are planned for already.
                if u.is_being_built() && needs <= 0.0 {
                    return None;
                }
                let energy = u.rules_param_float("current_energyIncome", 0.0);
                let pos = u.pos();
                let metal = if id == mex {
                    self.graph.get_closest_spot_index(pos).map_or(0.0, |s| self.graph.metal_spots[s].value)
                } else {
                    0.0
                };
                Some(Member { what: self.defs.name_of(u).to_owned(), pos, range, energy, needs, metal })
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
                metal: 0.0,
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
        // Once a minute, what overdrive makes (the gadget's own figures)
        // and how many mexes stand on a grid making energy.
        if (frame + self.offset + 13) % (6 * GRID_PERIOD) == 0 && frame > 0 {
            let game = self.cb.game();
            let param = |name: &str| game.team_rules_param_float(self.team_id, name, 0.0);
            let mexes = (0..n).filter(|&i| members[i].metal > 0.0);
            let on = mexes.clone().filter(|&i| capacity[grid[i]] >= MIN_GRID_ENERGY).count();
            self.log(&format!(
                "grid: overdrive +{:.1} metal on {:.1} base, {:.0} energy spent, {:.0} wasted; {on} of {} mexes on a grid making energy",
                param("OD_metalOverdrive"),
                param("OD_metalBase"),
                -param("OD_energyOverdrive"),
                param("OD_team_energyWaste"),
                mexes.count()
            ));
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
        let mut all: HashMap<usize, Vec<usize>> = HashMap::new();
        for (i, &g) in grid.iter().enumerate() {
            all.entry(g).or_default().push(i);
        }
        let mut of = all.clone();
        of.retain(|&g, list| capacity[g] >= MIN_GRID_ENERGY || list.iter().any(|&i| members[i].needs > 0.0));
        let mut nodes: Vec<usize> = of.keys().copied().collect();
        nodes.sort_unstable();
        let link = |a: usize, b: usize| shortest_link(&members, &of[&a], &of[&b], pr);

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
        } else if self.eco_overdrive_phase() && !self.dev_off("gridmex") {
            self.eco_overdrive_link(&members, &all, &capacity, pr)
        } else if self.eco_overdrive_phase() && nodes.len() > 1 {
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

    /// The overdrive link (`gridmex`): of the links between a grid making
    /// energy and another grid with mexes (or energy), the one gaining the
    /// most overdrive metal per pylon, if it pays back in [`MEX_PAYBACK`];
    /// none under enemy porc, none already being laid.
    fn eco_overdrive_link(
        &self,
        members: &[Member],
        grids: &HashMap<usize, Vec<usize>>,
        capacity: &[f32],
        pr: f32,
    ) -> Option<((usize, usize), String)> {
        let pylon_cost = self.defs.get("energypylon")?.cost(self.metal);
        // What overdrive gets: the energy made less what is spent.
        let made: f32 = grids.keys().map(|&g| capacity[g]).sum();
        if made <= 0.0 {
            return None;
        }
        let spent = self.cb.economy().usage(self.energy);
        let spare = ((made - spent) / made).clamp(MIN_SPARE, 1.0);
        let od = |c: f32, s: f32| 0.25 * (spare * c * s).max(0.0).sqrt();
        let metal_sq = |g: usize| grids[&g].iter().map(|&i| members[i].metal * members[i].metal).sum::<f32>();

        let mut keys: Vec<usize> = grids.keys().copied().collect();
        keys.sort_unstable();
        // (gain, pylons, members joined, grids joined)
        struct Pick(f32, f32, (usize, usize), (usize, usize));
        let mut best: Option<Pick> = None;
        for &a in keys.iter().filter(|&&g| capacity[g] >= MIN_GRID_ENERGY) {
            let (ca, sa) = (capacity[a], metal_sq(a));
            for &b in &keys {
                let (cb, sb) = (capacity[b], metal_sq(b));
                // Each pair once: an energy grid with one making less, or
                // with a stronger one listed after it.
                if b == a || (cb >= MIN_GRID_ENERGY && b < a) || sa + sb <= 0.0 {
                    continue;
                }
                let gain = od(ca + cb, sa + sb) - od(ca, sa) - od(cb, sb);
                if gain <= 0.0 {
                    continue;
                }
                let (pylons, ends) = shortest_link(members, &grids[&a], &grids[&b], pr);
                if gain * MEX_PAYBACK < pylons * pylon_cost || best.as_ref().is_some_and(|x| gain / pylons <= x.0 / x.1)
                {
                    continue;
                }
                let (m, n) = (&members[ends.0], &members[ends.1]);
                let points = chain(m.pos, m.range, n.pos, n.range, pr);
                if points.iter().all(|&p| self.eco_pylon_near(p))
                    || points.iter().any(|&p| self.war.get_porc_threat(p) > 0.0)
                {
                    continue;
                }
                best = Some(Pick(gain, pylons, ends, (a, b)));
            }
        }
        let Pick(gain, pylons, ends, (a, b)) = best?;
        let mexes = |g: usize| grids[&g].iter().filter(|&&i| members[i].metal > 0.0).count();
        Some((
            ends,
            format!(
                "grids making {:.0} and {:.0} energy, with {} and {} mexes: +{gain:.1} metal overdrive for {pylons:.0} pylons",
                capacity[a],
                capacity[b],
                mexes(a),
                mexes(b)
            ),
        ))
    }

    /// What a building of pylon range `range` at `pos` that needs `needs`
    /// energy would have: the energy of the grids it would join standing
    /// there, or else the fewest pylons that link it to a grid making the
    /// rest (see [`GridAccess`]).
    pub(crate) fn eco_grid_access(&self, pos: Vec3, range: f32, needs: f32) -> GridAccess {
        let gd = &self.eco.grid_defs;
        let pr = gd.pylon_range;
        let members: Vec<(Vec3, f32, f32)> = (self.cb.team_units().into_iter())
            .filter(|u| u.health() > 0.0 && !u.is_being_built())
            .filter_map(|u| {
                let range = gd.range(u.def()?.id())?;
                Some((u.pos(), range, u.rules_param_float("current_energyIncome", 0.0).max(0.0)))
            })
            .collect();
        let n = members.len();
        let mut parent: Vec<usize> = (0..n).collect();
        for i in 0..n {
            for j in i + 1..n {
                if distance(members[i].0, members[j].0) < members[i].1 + members[j].1 {
                    let (a, b) = (find(&mut parent, i), find(&mut parent, j));
                    parent[a] = b;
                }
            }
        }
        let grid: Vec<usize> = (0..n).map(|i| find(&mut parent, i)).collect();
        let mut capacity = vec![0.0f32; n];
        for (i, m) in members.iter().enumerate() {
            capacity[grid[i]] += m.2;
        }
        let mut joined: Vec<usize> =
            (0..n).filter(|&i| distance(members[i].0, pos) < members[i].1 + range).map(|i| grid[i]).collect();
        joined.sort_unstable();
        joined.dedup();
        let has: f32 = joined.iter().map(|&g| capacity[g]).sum();
        if has >= needs {
            return GridAccess::Powered(has);
        }
        if pr <= 0.0 {
            return GridAccess::Unpowered(has);
        }
        // Each other grid making energy by the fewest pylons to it, then
        // the cheapest in pylons per energy first until the rest is made up
        // (several small grids may: a base of turbines is that, 2026-11-04
        // Thornford: 100-120 energy in all, no single grid of 50 near).
        let mut nearest: Vec<(usize, f32, usize)> = Vec::new();
        for i in (0..n).filter(|&i| !joined.contains(&grid[i]) && capacity[grid[i]] > 0.0) {
            let pylons = link_pylons(distance(members[i].0, pos), members[i].1, range, pr);
            match nearest.iter_mut().find(|e| e.0 == grid[i]) {
                Some(e) if pylons < e.1 => (e.1, e.2) = (pylons, i),
                Some(_) => {}
                None => nearest.push((grid[i], pylons, i)),
            }
        }
        let (links, pylons, energy) = gather_links(&nearest, &capacity, has, needs);
        if energy < needs {
            return GridAccess::Unpowered(has);
        }
        let links = links.into_iter().map(|i| (members[i].0, members[i].1)).collect();
        GridAccess::Link { links, pylons, energy }
    }

    /// Whether a pylon stands (or is queued) where one is planned.
    fn eco_pylon_near(&self, p: Vec3) -> bool {
        self.eco.pylons.iter().any(|u| distance(u.pos(), p) < PYLON_SAME)
            || self.eco.pylon_tasks.iter().any(|&t| distance(self.eco.task_pos(t), p) < PYLON_SAME)
    }

    fn eco_max_pylon_tasks(&self) -> usize {
        if self.eco.pending_power.is_some() { MAX_PYLON_TASKS_WAITING } else { MAX_PYLON_TASKS }
    }

    /// Members (position, pylon range) of our grids that have a metal spot
    /// on them: where energy linked in can overdrive.
    pub(super) fn eco_mex_grid_members(&self) -> Vec<(Vec3, f32)> {
        let gd = &self.eco.grid_defs;
        let eco = &self.eco;
        let mex = eco.build_ids.mex_id;
        let members: Vec<(Vec3, f32, bool)> = (eco.mexes.iter())
            .chain(&eco.solars)
            .chain(&eco.windgens)
            .chain(&eco.pylons)
            .chain(&eco.fusions)
            .chain(&eco.geos)
            .filter_map(|u| {
                let id = u.def()?.id();
                Some((u.pos(), gd.range(id)?, id == mex))
            })
            .collect();
        let n = members.len();
        let mut parent: Vec<usize> = (0..n).collect();
        for i in 0..n {
            for j in i + 1..n {
                if distance(members[i].0, members[j].0) < members[i].1 + members[j].1 {
                    let (a, b) = (find(&mut parent, i), find(&mut parent, j));
                    parent[a] = b;
                }
            }
        }
        let mut with_mex = vec![false; n];
        for (i, m) in members.iter().enumerate() {
            if m.2 {
                with_mex[find(&mut parent, i)] = true;
            }
        }
        (0..n).filter(|&i| with_mex[find(&mut parent, i)]).map(|i| (members[i].0, members[i].1)).collect()
    }

    /// Queue the pylons of a chain from `a` (range `ra`) to `b` that are not
    /// there yet.
    pub(crate) fn eco_lay_pylons(&mut self, a: Vec3, ra: f32, b: Vec3, rb: f32, why: &str) {
        let Some(pylon) = self.defs.get("energypylon") else { return };
        let mut queued = 0;
        for p in chain(a, ra, b, rb, self.eco.grid_defs.pylon_range) {
            if self.eco.pylon_tasks.len() >= self.eco_max_pylon_tasks() {
                break;
            }
            if !self.eco_pylon_near(p) {
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

/// The shortest link between two grids (lists of members): pylons needed,
/// and the members it joins.
fn shortest_link(members: &[Member], a: &[usize], b: &[usize], pr: f32) -> (f32, (usize, usize)) {
    let (mut best, mut ends) = (f32::INFINITY, (0, 0));
    for &i in a {
        for &j in b {
            let gap = distance(members[i].pos, members[j].pos)
                - CHAIN_SLACK * (members[i].range + members[j].range + 2.0 * pr);
            if gap < best {
                best = gap;
                ends = (i, j);
            }
        }
    }
    (1.0 + (best.max(0.0) / (2.0 * pr * CHAIN_SLACK)).ceil(), ends)
}

/// Which of the grids `nearest` (grid, pylons to it, member nearest) to link
/// to make `needs` from `has`: the cheapest in pylons per energy first.
/// Their members, the pylons in all, the energy then.
fn gather_links(nearest: &[(usize, f32, usize)], capacity: &[f32], has: f32, needs: f32) -> (Vec<usize>, f32, f32) {
    let mut order: Vec<&(usize, f32, usize)> = nearest.iter().collect();
    order.sort_by(|a, b| (a.1 / capacity[a.0]).total_cmp(&(b.1 / capacity[b.0])));
    let (mut links, mut pylons, mut energy) = (Vec::new(), 0.0, has);
    for &&(g, p, i) in &order {
        if energy >= needs {
            break;
        }
        links.push(i);
        pylons += p;
        energy += capacity[g];
    }
    (links, pylons, energy)
}

/// Pylons (of range `pr`) a link between buildings `d` apart, of pylon
/// ranges `ra` and `rb`, takes (as [`shortest_link`] counts them).
fn link_pylons(d: f32, ra: f32, rb: f32, pr: f32) -> f32 {
    let gap = d - CHAIN_SLACK * (ra + rb + 2.0 * pr);
    1.0 + (gap.max(0.0) / (2.0 * pr * CHAIN_SLACK)).ceil()
}

/// Where the pylons (of range `pr`) of a chain from `a` (range `ra`) to `b`
/// (range `rb`) stand: the first within reach of `a`, each next within
/// reach of the last, the last within reach of `b`.
pub(super) fn chain(a: Vec3, ra: f32, b: Vec3, rb: f32, pr: f32) -> Vec<Vec3> {
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

    #[test]
    fn small_grids_add_up_cheapest_first() {
        // Grids 0..3 making 20, 30, 60, 10; pylons to them 1, 1, 3, 1.
        let capacity = [20.0, 30.0, 60.0, 10.0];
        let nearest = [(0, 1.0, 10), (1, 1.0, 11), (2, 3.0, 12), (3, 1.0, 13)];
        // 50 from nothing: the 30 and the 20 (2 pylons) beat the 60 (3).
        let (links, pylons, energy) = gather_links(&nearest, &capacity, 0.0, 50.0);
        assert_eq!((links, pylons, energy), (vec![11, 10], 2.0, 50.0));
        // Never enough: all of it.
        assert_eq!(gather_links(&nearest, &capacity, 0.0, 500.0).2, 120.0);
    }

    #[test]
    fn link_pylons_count_as_the_chain_lays_them() {
        // A Lucifer (range 50) 900 from a solar (100), pylons 500: one
        // pylon spans 0.85 x 1150 = 977.
        assert_eq!(link_pylons(900.0, 100.0, 50.0, 500.0), 1.0);
        assert_eq!(link_pylons(1500.0, 100.0, 50.0, 500.0), 2.0);
        for d in [900.0, 1500.0, 2400.0] {
            let n = chain(Vec3::xz(0.0, 0.0), 100.0, Vec3::xz(d, 0.0), 50.0, 500.0).len() as f32;
            assert!(n <= link_pylons(d, 100.0, 50.0, 500.0), "{d}: chain {n}");
        }
    }
}
