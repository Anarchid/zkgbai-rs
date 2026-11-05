//! The energy grid as the game sees it (`LuaRules/Gadgets/
//! unit_mex_overdrive.lua`): buildings with a pylon range link when closer
//! than their two ranges together; a grid's capacity is the energy its
//! members make; a building that needs power (`neededlink`: superweapons,
//! Berthas, heavy turrets) works only on a grid with at least that much.
//!
//! The economy's own pylon building links metal spots as the territory
//! grows. On top of it, this planner links buildings starved of power to
//! the strongest grid, and in the overdrive phase links grids by the
//! overdrive they gain (below). Chains are laid a pylon at a time and
//! checked against the grid as built, so a misplaced pylon is made up for
//! next time.
//!
//! Overdrive: late in the game the economy's own linking stops short. Its small energy goes up only while
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
use super::grid_support::{Connection, PowerNeed};
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
/// metal.
const MEX_PAYBACK: f32 = 600.0;

/// Share of the energy made that overdrive gets at least, whatever the
/// spending of the moment.
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

/// One of our grid members (or a building's site waiting for power).
struct Member {
    what: String,
    def: UnitDef,
    conducts: bool,
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
        if self.eco_super_project_exposed(def, pos) {
            return;
        }
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
        if self.dev_off("grid") {
            self.eco.grid_support.jobs.clear();
            return;
        }
        if (frame + self.offset + 13) % GRID_PERIOD != 0 {
            return;
        }
        if self.eco.pending_power.as_ref().is_some_and(|p| self.eco_super_project_exposed(p.def, p.pos)) {
            self.eco.pending_power = None;
            self.log("grid: dropping superweapon site now exposed to tactical missiles");
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
                let def = u.def()?;
                let id = def.id();
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
                Some(Member {
                    what: self.defs.name_of(u).to_owned(),
                    def,
                    conducts: !u.is_being_built(),
                    pos,
                    range,
                    energy,
                    needs,
                    metal,
                })
            })
            .collect();
        // Accepted construction jobs still need a connection before their
        // nanoframe exists, including a site released by POWER_WAIT above.
        for task in self.eco.construction_tasks.iter().filter_map(|&id| self.eco.tasks.get(id)) {
            let Some(c) = task.as_construction() else { continue };
            let Some(&(range, needs)) = gd.by_def.get(&c.build_type.id()).filter(|(_, needs)| *needs > 0.0) else {
                continue;
            };
            if members.iter().any(|m| m.def.id() == c.build_type.id() && distance(m.pos, task.position) < 25.0) {
                continue;
            }
            members.push(Member {
                what: format!("{} construction", self.defs.name(c.build_type)),
                def: c.build_type,
                conducts: false,
                pos: task.position,
                range,
                energy: 0.0,
                needs,
                metal: 0.0,
            });
        }
        // The site of a building waiting for power, as if it stood there.
        let pending = self.eco.pending_power.as_ref().and_then(|p| {
            let &(range, needs) = gd.by_def.get(&p.def.id())?;
            members.push(Member {
                what: format!("{} site", self.defs.name(p.def)),
                def: p.def,
                conducts: false,
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
        let (grid, capacity) = components(&members);
        let needs: Vec<PowerNeed> = (0..n)
            .filter(|&i| members[i].needs > capacity[grid[i]])
            .map(|i| self.eco_power_need(members[i].def, members[i].pos))
            .collect();
        let powered: Vec<_> = (0..n)
            .filter(|&i| members[i].conducts && capacity[grid[i]] >= MIN_GRID_ENERGY)
            .map(|i| (members[i].pos, members[i].range))
            .collect();
        self.eco_refresh_grid_support(needs, &powered);
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

        // Restore useful capital before overdrive or speculative new weapons.
        // Value includes the nearby enemy force the weapon keeps out.
        let mut starved: Vec<usize> = (0..n).filter(|&i| members[i].needs > capacity[grid[i]]).collect();
        starved.sort_by(|&a, &b| {
            let value = |i: usize| self.eco_power_need(members[i].def, members[i].pos).value;
            value(b).total_cmp(&value(a)).then_with(|| a.cmp(&b))
        });
        for s in starved.iter().copied() {
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
            let Some(o) = enough.or(strongest) else { continue };
            let (a, b) = (&members[o.2.0], &members[o.2.1]);
            let beneficiaries = starved.iter().filter(|&&i| grid[i] == g).map(|&i| members[i].pos).collect();
            let job = self.eco_power_connection(beneficiaries);
            self.eco_reconnect_power(a.pos, a.range, b.pos, b.range, job);
        }
        // Revalue existing overdrive links from today's separated components.
        // A completed bridge, a lost endpoint, or lost generation retires the bid.
        let spare = self.eco_grid_spare(&all, &capacity);
        let updates: Vec<_> = self
            .eco
            .grid_support
            .jobs
            .iter()
            .filter_map(|(&id, job)| {
                let (a, b) = job.overdrive?;
                let component =
                    |p| (0..n).find(|&i| members[i].conducts && distance(members[i].pos, p) < 16.0).map(|i| grid[i]);
                let gain = match (component(a), component(b)) {
                    (Some(a), Some(b)) if a != b => overdrive_gain(
                        capacity[a],
                        grid_metal_sq(&members, &all[&a]),
                        capacity[b],
                        grid_metal_sq(&members, &all[&b]),
                        spare,
                    ),
                    _ => 0.0,
                };
                Some((id, gain))
            })
            .collect();
        for (id, gain) in updates {
            if gain > 0.0 {
                if let Some(job) = self.eco.grid_support.jobs.get_mut(&id) {
                    job.value = gain * MEX_PAYBACK;
                    job.rate = gain;
                    job.why = format!("restore +{gain:.2} metal/s of overdrive");
                }
            } else {
                self.eco.grid_support.jobs.remove(&id);
            }
        }
        if self.eco_overdrive_phase() {
            if let Some(((i, j), why, gain)) = self.eco_overdrive_link(&members, &all, &capacity, pr) {
                let (a, b) = (&members[i], &members[j]);
                // The endpoint on the higher-capacity component supplies power.
                let (goal, source) = if capacity[grid[i]] > capacity[grid[j]] { (b, a) } else { (a, b) };
                let job = Connection {
                    assets: Vec::new(),
                    overdrive: Some((goal.pos, source.pos)),
                    value: gain * MEX_PAYBACK,
                    rate: gain,
                    threat: 0.0,
                    why,
                };
                self.eco_reconnect_power(goal.pos, goal.range, source.pos, source.range, job);
            }
        }
    }

    fn eco_grid_spare(&self, grids: &HashMap<usize, Vec<usize>>, capacity: &[f32]) -> f32 {
        let made: f32 = grids.keys().map(|&g| capacity[g]).sum();
        if made <= 0.0 {
            return MIN_SPARE;
        }
        ((made - self.cb.economy().usage(self.energy)) / made).clamp(MIN_SPARE, 1.0)
    }

    /// The overdrive link: of the links between a grid making
    /// energy and another grid with mexes (or energy), the one gaining the
    /// most overdrive metal per pylon, if it pays back in [`MEX_PAYBACK`];
    /// none under enemy porc, none already being laid.
    fn eco_overdrive_link(
        &self,
        members: &[Member],
        grids: &HashMap<usize, Vec<usize>>,
        capacity: &[f32],
        pr: f32,
    ) -> Option<((usize, usize), String, f32)> {
        let pylon_cost = self.defs.get("energypylon")?.cost(self.metal);
        // What overdrive gets: the energy made less what is spent.
        let made: f32 = grids.keys().map(|&g| capacity[g]).sum();
        if made <= 0.0 {
            return None;
        }
        let spare = self.eco_grid_spare(grids, capacity);
        let metal_sq = |g: usize| grid_metal_sq(members, &grids[&g]);

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
                let gain = overdrive_gain(ca, sa, cb, sb, spare);
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
            gain,
        ))
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
    pub(super) fn eco_lay_pylons(&mut self, a: Vec3, ra: f32, b: Vec3, rb: f32, why: &str) {
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

/// Pending buildings can receive a grid but cannot conduct between live grids.
fn components(members: &[Member]) -> (Vec<usize>, Vec<f32>) {
    let n = members.len();
    let mut parent: Vec<usize> = (0..n).collect();
    for i in 0..n {
        for j in i + 1..n {
            if members[i].conducts
                && members[j].conducts
                && distance(members[i].pos, members[j].pos) < members[i].range + members[j].range
            {
                let (a, b) = (find(&mut parent, i), find(&mut parent, j));
                parent[a] = b;
            }
        }
    }
    let mut grid: Vec<usize> = (0..n).map(|i| find(&mut parent, i)).collect();
    let mut capacity = vec![0.0f32; n];
    for (i, m) in members.iter().enumerate() {
        capacity[grid[i]] += m.energy;
    }
    // A nanoframe can receive power at its site, but cannot bridge two
    // live grids before completion. Pick a real adjacent component.
    for i in 0..n {
        if !members[i].conducts {
            if let Some(j) = (0..n)
                .filter(|&j| {
                    members[j].conducts
                        && distance(members[i].pos, members[j].pos) < members[i].range + members[j].range
                })
                .max_by(|&a, &b| capacity[grid[a]].total_cmp(&capacity[grid[b]]))
            {
                grid[i] = grid[j];
            }
        }
    }
    (grid, capacity)
}

fn grid_metal_sq(members: &[Member], grid: &[usize]) -> f32 {
    grid.iter().map(|&i| members[i].metal * members[i].metal).sum()
}

fn overdrive_gain(ca: f32, sa: f32, cb: f32, sb: f32, spare: f32) -> f32 {
    let od = |c: f32, s: f32| 0.25 * (spare * c * s).max(0.0).sqrt();
    (od(ca + cb, sa + sb) - od(ca, sa) - od(cb, sb)).max(0.0)
}

/// The shortest link between two grids (lists of members): pylons needed,
/// and the members it joins.
fn shortest_link(members: &[Member], a: &[usize], b: &[usize], pr: f32) -> (f32, (usize, usize)) {
    let (mut best, mut ends) = (f32::INFINITY, (0, 0));
    for &i in a {
        for &j in b {
            if !members[j].conducts {
                continue;
            }
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
    fn high_voltage_disconnection_can_cost_a_hundred_metal_per_second() {
        assert_eq!(overdrive_gain(1600.0, 0.0, 0.0, 100.0, 1.0), 100.0);
        assert_eq!(overdrive_gain(1600.0, 0.0, 0.0, 100.0, 0.25), 50.0);
        // Equal energy per squared base income offers no gain from joining.
        assert_eq!(overdrive_gain(100.0, 4.0, 100.0, 4.0, 1.0), 0.0);
        assert_eq!(overdrive_gain(0.0, 100.0, 0.0, 100.0, 1.0), 0.0);
    }

    #[test]
    fn unfinished_weapons_do_not_create_phantom_connections() {
        let ai = crate::economy::test_ai_with(|_| {});
        let def = UnitDef::from_id(ai.cb, 1).unwrap();
        let member = |x, energy, conducts| Member {
            what: String::new(),
            def,
            conducts,
            pos: Vec3::xz(x, 1000.0),
            range: 100.0,
            energy,
            needs: 0.0,
            metal: 0.0,
        };
        let mut members = vec![member(1000.0, 100.0, true), member(1150.0, 0.0, false), member(1300.0, 200.0, true)];
        let (grid, capacity) = components(&members);
        assert_ne!(grid[0], grid[2]);
        assert_eq!(capacity[grid[1]], 200.0);
        members[1].conducts = true;
        let (grid, capacity) = components(&members);
        assert_eq!(grid[0], grid[2]);
        assert_eq!(capacity[grid[1]], 300.0);
    }

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
