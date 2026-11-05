//! A shared energy budget for allied games. Count production and the full
//! output of unfinished plants, then let one active ZKGBAI start a reactor.
//! Other instances help the frame, even when it already covers the deficit.
//! A fixed starter (lowest eligible team id) also owns invisible queued jobs:
//! unlike timeout-based turns, it cannot race another instance's private queue.

use std::collections::{BTreeMap, BTreeSet};

use springai::UnitDef;

use super::{List, OD_ENERGY_RATIO, WorkerTask, eramp_wanted, wind::energy_make};
use crate::Ai;

const FUSION: &str = "energyfusion";
const SINGU: &str = "energysingu";
// Census/cleanup runs every second with different offsets. Wait two seconds
// after a starter change so the former starter cancels its unstarted job.
const HANDOVER: i32 = 60;

fn starter(
    peers: impl Iterator<Item = i32>,
    factories: &BTreeSet<i32>,
    fusion_builders: &BTreeSet<i32>,
    singu_builders: &BTreeSet<i32>,
    reactors: usize,
) -> Option<i32> {
    let eligible: Vec<_> = peers.filter(|t| factories.contains(t) && fusion_builders.contains(t)).collect();
    let advanced =
        (reactors >= 3).then(|| eligible.iter().copied().filter(|t| singu_builders.contains(t)).min()).flatten();
    advanced.or_else(|| eligible.into_iter().min())
}

#[derive(Clone, Copy, Debug, Default)]
struct Budget {
    raw_metal: f32,
    metal: f32,
    made: f32,
    /// Current unit energy use, excluding discretionary overdrive.
    used: f32,
    /// Energy-only spending (cloak, radar, repairs, etc.), additional to metal.
    energy_only: f32,
    pending: f32,
    queued: f32,
    target: f32,
    reactors: usize,
    building_reactor: bool,
}

impl Budget {
    fn production(&mut self, made: f32, finished_output: f32, unfinished: bool) {
        self.made += made;
        if unfinished {
            self.pending += (finished_output - made).max(0.0);
        }
    }

    fn set_target(&mut self, odreserve: bool, eramp: bool, share: f32, phase: i32) {
        // Use the mature overdrive ratio. Reactors do not raise their own
        // demand; territory and the phase clock drive further escalation.
        let full = self.raw_metal * (if odreserve { OD_ENERGY_RATIO } else { 2.0 } + 2.0);
        self.target = if eramp { eramp_wanted(full, self.metal, share, phase).0 } else { full.max(self.metal) };
    }

    fn shortfall(self) -> f32 {
        (self.target - self.made - self.pending - self.queued).max(0.0)
    }

    fn needs_energy(self) -> bool {
        self.made < self.metal.max(self.used) || self.shortfall() > 0.0
    }

    fn spending(&mut self, energy: f32, metal: f32) {
        self.used += energy.max(0.0);
        self.energy_only += (energy - metal.max(0.0)).max(0.0);
    }

    /// Only working production can fund a morph. Energy-only costs must
    /// be reserved in addition to spending the ongoing metal income, even
    /// when builders are idle. Also cover any higher live spending.
    fn surplus(self) -> f32 {
        self.made - (self.metal + self.energy_only).max(self.used)
    }

    fn reactor(self, eminent: bool, fusion_output: f32) -> Option<&'static str> {
        if !eminent || self.metal <= 35.0 || self.building_reactor || fusion_output <= 0.0 {
            return None;
        }
        let short = self.shortfall();
        // Leave small gaps to wind/solar. Buy a Singu for a substantial gap
        // once the alliance has three reactors, whoever owns them.
        if short < fusion_output * 0.5 {
            None
        } else if self.reactors >= 3 && short >= 2.0 * fusion_output {
            Some(SINGU)
        } else {
            Some(FUSION)
        }
    }
}

#[derive(Default)]
pub(super) struct TeamEnergy {
    budget: Option<Budget>,
    starter: Option<i32>,
    since: i32,
    logged: Option<i32>,
}

impl TeamEnergy {
    fn ready(&mut self, starter: Option<i32>, team: i32, frame: i32) -> bool {
        if self.starter != starter {
            self.starter = starter;
            self.since = frame;
        }
        starter == Some(team) && frame - self.since >= HANDOVER
    }
}

impl Ai {
    /// A dedicated energy hand stops prioritizing plants only when their
    /// output exists. Unfinished capacity needs hands even if it covers the plan.
    pub(super) fn eco_energy_goal_unmet(&self) -> bool {
        self.eco.hard_estalled
            || self
                .eco
                .team_energy
                .budget
                .filter(|_| !self.allies.is_empty())
                .map_or(self.eco.effective_income_energy < self.eco.energy_target, |b| b.made < b.target)
    }

    /// Small plants can bridge an actual alliance-wide stall while a reactor
    /// builds. Otherwise don't fill a deficit already covered by projects.
    pub(super) fn eco_team_energy_needed(&self) -> bool {
        if self.allies.is_empty() {
            self.eco.estalled
        } else {
            self.eco.team_energy.budget.map_or(self.eco.estalled, Budget::needs_energy)
        }
    }

    /// Shared income lets allied economies invest in a geo earlier than
    /// one player's income would allow. Wait for the census in team games.
    pub(super) fn eco_geo_income(&self) -> f32 {
        if self.allies.is_empty() {
            self.eco.static_income
        } else {
            self.eco.team_energy.budget.map_or(0.0, |b| b.metal)
        }
    }

    pub(super) fn eco_geo_upgrade_surplus(&self) -> Option<f32> {
        if !self.allies.is_empty() {
            return self.eco.team_energy.budget.map(Budget::surplus);
        }
        let mut budget =
            Budget { made: self.eco.effective_income_energy, metal: self.eco.static_income, ..Budget::default() };
        for u in self.cb.friendly_units().into_iter().filter(|u| u.team() == self.team_id && u.health() > 0.0) {
            budget.spending(u.resource_use(self.energy), u.resource_use(self.metal));
        }
        Some(budget.surplus())
    }

    pub(super) fn eco_geo_energy_stalled(&self) -> bool {
        // The priority gadget turns off upkeep (including cloakers) when
        // it cannot pay. Their reduced actual draw is not spare energy.
        self.eco.hard_estalled
            || self.cb.friendly_units().into_iter().any(|u| {
                u.health() > 0.0
                    && !u.is_being_built()
                    && u.rules_param_float("forcedOff", 0.0) > 0.0
                    && u.is_activated()
            })
    }

    fn eco_planned_energy(&self, def: UnitDef, pos: springai::Vec3, wind: Option<super::wind::WindModel>) -> f32 {
        if self.defs.name(def) == "energywind" {
            wind.map_or(0.0, |w| w.worth(self.cb.map().elevation_at(pos.x, pos.z)))
        } else {
            energy_make(def)
        }
    }

    /// Refresh from friendly units, rather than summing team income APIs:
    /// resource transfers/overdrive sharing must not count production twice.
    pub(super) fn eco_plan_team_energy(&mut self) {
        if self.allies.is_empty() {
            return;
        }
        let Some(fusion) = self.defs.get(FUSION) else { return };
        let singu = self.defs.get(SINGU);
        let wind = self.eco_wind_model();
        let advanced_geo = self.defs.get("energyheavygeo").map_or(0.0, energy_make);
        let mut budget = Budget::default();
        let mut teams = BTreeSet::new();
        let mut factories = BTreeSet::new();
        let mut fusion_builders = BTreeSet::new();
        let mut singu_builders = BTreeSet::new();
        let mut builder_types = BTreeMap::new();
        let mut seen = BTreeSet::new();
        for u in self.cb.friendly_units().into_iter().filter(|u| u.health() > 0.0) {
            if !seen.insert(u.id()) {
                continue;
            }
            let Some(def) = u.def() else { continue };
            let name = self.defs.name(def);
            let team = u.team();
            teams.insert(team);
            let building = u.is_being_built();
            budget.spending(u.resource_use(self.energy), u.resource_use(self.metal));
            let output = u.rules_param_float("current_energyIncome", 0.0).max(0.0);
            let expected = if building { self.eco_planned_energy(def, u.pos(), wind) } else { output };
            budget.production(output, expected, building);
            if !building {
                budget.metal += u.rules_param_float("current_metalIncome", 0.0).max(0.0);
                if name == "staticmex" {
                    budget.raw_metal += u.rules_param_float("mexIncome", 0.0).max(0.0);
                }
                if name.starts_with("factory") || name.starts_with("plate") {
                    factories.insert(team);
                }
                if def.is_builder() && def.speed() > 0.0 {
                    let (can_fusion, can_singu) = *builder_types.entry(def.id()).or_insert_with(|| {
                        let options = def.build_options();
                        (
                            options.iter().any(|d| d.id() == fusion.id()),
                            singu.is_some_and(|s| options.iter().any(|d| d.id() == s.id())),
                        )
                    });
                    if can_fusion {
                        fusion_builders.insert(team);
                    }
                    if can_singu {
                        singu_builders.insert(team);
                    }
                }
            }
            if matches!(name, FUSION | SINGU) {
                budget.building_reactor |= building;
                budget.reactors += usize::from(!building);
            }
            // A geo morph is still an energygeo until it completes. Reserve
            // the incremental output, including our not-yet-acknowledged order.
            if name == "energygeo"
                && (u.rules_param_float("morphing", 0.0) > 0.0 || self.eco.geo.morph.is_some_and(|(g, _, _)| g == u))
            {
                budget.pending += (advanced_geo - output).max(0.0);
                budget.building_reactor = true;
            }
        }
        // The same innate team income included by eco_update_income.
        budget.metal += 2.0 * teams.len() as f32;
        budget.made += 2.0 * teams.len() as f32;
        budget.set_target(
            !self.dev_off("odreserve"),
            !self.dev_off("eramp"),
            self.graph.territory_fraction,
            self.phase_frame(),
        );

        // Frames were counted above. Only target-less jobs add capacity here;
        // assisting a frame must never count its output a second time.
        let mut queued_reactors = Vec::new();
        for &id in &self.eco.construction_tasks {
            let Some(t) = self.eco.tasks.get(id) else { continue };
            let Some(c) = t.as_construction().filter(|c| c.target.is_none()) else { continue };
            let output = self.eco_planned_energy(c.build_type, t.position, wind);
            if self.eco.fusion_tasks.contains(&id) {
                queued_reactors.push((id, output));
            } else {
                budget.queued += output;
            }
        }
        let wanted = budget.reactor(self.graph.eminent_territory, energy_make(fusion));
        // Election must not depend on each instance's private queued energy
        // or chosen plant: those can differ even on the same census. Advance
        // to Singu-capable builders from the public reactor count alone.
        let starter = starter(
            self.iq.peers.iter().copied().chain([self.team_id]),
            &factories,
            &fusion_builders,
            &singu_builders,
            budget.reactors,
        );
        let ready = self.eco.team_energy.ready(starter, self.team_id, self.frame);
        let keep_queue = ready && wanted.is_some();
        for (id, output) in queued_reactors {
            if keep_queue {
                budget.queued += output;
            } else {
                // Stop orders too: removing only the task leaves a constructor
                // free to put down its frame after a new starter takes over.
                self.eco.drop_construction_task(id);
            }
        }
        self.eco.team_energy.budget = Some(budget);
        if self.eco.team_energy.logged.is_none_or(|f| self.frame - f >= 1800) {
            self.eco.team_energy.logged = Some(self.frame);
            self.log(&format!(
                "team energy: goal {:.1}, made {:.1}, pending {:.1}, queued {:.1}, deficit {:.1}; {} reactors, starter {:?}",
                budget.target, budget.made, budget.pending, budget.queued, budget.shortfall(), budget.reactors, starter
            ));
        }
        if !self.eco.fusion_tasks.is_empty() {
            return;
        }
        // Help before checking the budget: a sufficient unfinished Singu
        // still needs constructors to finish it.
        for def in singu.into_iter().chain([fusion]) {
            if self.eco_assist_ally(def) {
                return;
            }
        }
        if !ready {
            return;
        }
        let Some(wanted) = budget.reactor(self.graph.eminent_territory, energy_make(fusion)) else { return };
        let name = if wanted == SINGU && !singu_builders.contains(&self.team_id) { FUSION } else { wanted };
        if self.eco_upgrade_geo(self.frame) {
            return;
        }
        let Some(def) = self.defs.get(name) else { return };
        let Some(pos) = self.eco_reactor_site(def) else { return };
        // A safe site must also be reachable by at least one of our builders.
        let workers: Vec<i32> = self.eco.workers.keys().copied().collect();
        if !workers.into_iter().any(|w| self.eco_can_reach(w, pos)) {
            return;
        }
        if self.eco_add_construction(WorkerTask::construction(def, pos, 0), None, Some(List::Fusion)).is_some() {
            let output = energy_make(def);
            if let Some(b) = &mut self.eco.team_energy.budget {
                b.queued += output;
            }
            self.log(&format!("team energy: starting {name} for {:.1} E/s deficit (+{output:.1})", budget.shortfall()));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rich() -> Budget {
        Budget { raw_metal: 90.0, metal: 100.0, made: 150.0, target: 300.0, reactors: 3, ..Budget::default() }
    }

    #[test]
    fn industrious_workers_resume_other_jobs_at_actual_energy_goal() {
        let mut ai = super::super::commander::tests::test_ai_with(|_| {});
        ai.eco.energy_target = 100.0;
        ai.eco.effective_income_energy = 90.0;
        assert!(ai.eco_energy_goal_unmet());
        ai.eco.effective_income_energy = 100.0;
        assert!(!ai.eco_energy_goal_unmet());
        ai.allies.push(3);
        ai.eco.team_energy.budget = Some(Budget { made: 90.0, target: 100.0, pending: 200.0, ..Budget::default() });
        assert!(ai.eco_energy_goal_unmet(), "finish the pending plants before returning to other work");
        ai.eco.team_energy.budget.as_mut().unwrap().made = 100.0;
        assert!(!ai.eco_energy_goal_unmet());
        ai.eco.hard_estalled = true;
        assert!(ai.eco_energy_goal_unmet());
    }

    #[test]
    fn geo_timing_uses_allied_income_and_demand_even_when_locally_energy_rich() {
        let mut ai = super::super::test_ai_with(|_| {});
        ai.eco.static_income = 12.0;
        ai.eco.estalled = false;
        assert_eq!(ai.eco_geo_income(), 12.0);
        assert!(!ai.eco_team_energy_needed());
        ai.allies.push(3);
        assert_eq!(ai.eco_geo_income(), 0.0);
        assert_eq!(ai.eco_geo_upgrade_surplus(), None);

        let b = Budget { metal: 30.0, made: 40.0, target: 90.0, ..Budget::default() };
        ai.eco.team_energy.budget = Some(b);
        assert_eq!(ai.eco_geo_income(), 30.0);
        assert!(ai.eco_team_energy_needed());
        assert_eq!(ai.eco_geo_upgrade_surplus(), Some(10.0));

        // Enough future production stops duplicate projects, but cannot
        // pay for an upgrade now. An actual shortage still needs relief.
        ai.eco.team_energy.budget = Some(Budget { pending: 100.0, ..b });
        assert!(!ai.eco_team_energy_needed());
        assert_eq!(ai.eco_geo_upgrade_surplus(), Some(10.0));
        ai.eco.team_energy.budget = Some(Budget { made: 20.0, pending: 100.0, ..b });
        assert!(ai.eco_team_energy_needed());
        assert_eq!(ai.eco_geo_upgrade_surplus(), Some(-10.0));
    }

    #[test]
    fn upgrade_surplus_adds_misc_to_metal_and_ignores_unfinished_capacity() {
        let mut b = Budget { made: 160.0, metal: 100.0, pending: 1000.0, queued: 1000.0, ..Budget::default() };
        assert_eq!(b.surplus(), 60.0);
        // Idle builders do not make a 25 E/s cloaker free: preserve the
        // 100 E/s needed to spend the metal income when building resumes.
        b.spending(25.0, 0.0);
        assert_eq!(b.surplus(), 35.0);
        b.spending(80.0, 80.0);
        assert_eq!(b.surplus(), 35.0);
        // Spending stored/reclaimed metal can exceed ongoing income.
        b.spending(40.0, 40.0);
        assert_eq!(b.surplus(), 15.0);
    }

    #[test]
    fn geo_budget_counts_cloakers_and_rejects_priority_disabled_upkeep() {
        use std::ffi::{CStr, c_char};
        thread_local! {
            static FORCED_OFF: std::cell::Cell<f32> = const { std::cell::Cell::new(0.0) };
            static OWNER: std::cell::Cell<i32> = const { std::cell::Cell::new(2) };
            static ACTIVE: std::cell::Cell<bool> = const { std::cell::Cell::new(true) };
        }
        unsafe extern "C" fn units(_: i32, out: *mut i32, size: i32) -> i32 {
            if !out.is_null() && size > 0 {
                unsafe { *out = 10 };
            }
            1
        }
        unsafe extern "C" fn health(_: i32, _: i32) -> f32 {
            100.0
        }
        unsafe extern "C" fn team(_: i32, _: i32) -> i32 {
            OWNER.with(std::cell::Cell::get)
        }
        unsafe extern "C" fn active(_: i32, _: i32) -> bool {
            ACTIVE.with(std::cell::Cell::get)
        }
        unsafe extern "C" fn resource(_: i32, name: *const c_char) -> i32 {
            i32::from(unsafe { CStr::from_ptr(name) } == c"Energy")
        }
        unsafe extern "C" fn used(_: i32, _: i32, res: i32) -> f32 {
            if res == 1 { 25.0 } else { 0.0 }
        }
        unsafe extern "C" fn rules(_: i32, _: i32, name: *const c_char, default: f32) -> f32 {
            if unsafe { CStr::from_ptr(name) } == c"forcedOff" {
                FORCED_OFF.with(std::cell::Cell::get)
            } else {
                default
            }
        }
        let mut ai = super::super::test_ai_with(|t| {
            t.getFriendlyUnits = Some(units);
            t.Unit_getHealth = Some(health);
            t.Unit_getTeam = Some(team);
            t.getResourceByName = Some(resource);
            t.Unit_getResourceUse = Some(used);
            t.Unit_isActivated = Some(active);
            t.Unit_getRulesParamFloat = Some(rules);
        });
        ai.eco.effective_income_energy = 160.0;
        ai.eco.static_income = 100.0;
        assert_eq!(ai.eco_geo_upgrade_surplus(), Some(35.0));
        assert!(!ai.eco_geo_energy_stalled());
        FORCED_OFF.with(|v| v.set(1.0));
        assert!(ai.eco_geo_energy_stalled());
        // An ally losing upkeep also forbids the upgrade.
        OWNER.with(|v| v.set(3));
        ai.allies.push(3);
        assert!(ai.eco_geo_energy_stalled());
        // The gadget may leave forcedOff set on a deliberately disabled
        // unit; that stale flag must not veto upgrades indefinitely.
        ACTIVE.with(|v| v.set(false));
        assert!(!ai.eco_geo_energy_stalled());
        FORCED_OFF.with(|v| v.set(0.0));
        ai.eco.hard_estalled = true;
        assert!(ai.eco_geo_energy_stalled());
    }

    #[test]
    fn three_allied_fusions_unlock_one_singu() {
        assert_eq!(rich().reactor(true, 35.0), Some(SINGU));
        assert_eq!(Budget { reactors: 2, ..rich() }.reactor(true, 35.0), Some(FUSION));
        assert_eq!(rich().reactor(false, 35.0), None);
    }

    #[test]
    fn full_output_of_a_frame_or_queue_covers_the_goal() {
        for b in [Budget { pending: 225.0, ..rich() }, Budget { queued: 225.0, ..rich() }] {
            assert_eq!(b.shortfall(), 0.0);
            assert_eq!(b.reactor(true, 35.0), None);
        }
        assert_eq!(Budget { made: 350.0, ..rich() }.reactor(true, 35.0), None);
        assert_eq!(Budget { building_reactor: true, ..rich() }.reactor(true, 35.0), None);
    }

    #[test]
    fn smaller_deficits_get_fusions_or_small_plants() {
        assert_eq!(Budget { target: 200.0, ..rich() }.reactor(true, 35.0), Some(FUSION));
        assert_eq!(Budget { target: 160.0, ..rich() }.reactor(true, 35.0), None);
    }

    #[test]
    fn unfinished_capacity_is_not_scaled_by_progress_or_counted_twice() {
        let mut b = Budget { target: 300.0, ..Budget::default() };
        b.production(100.0, 100.0, false);
        // Even a just-started Singu reserves its entire eventual output.
        b.production(0.0, 225.0, true);
        assert_eq!(b.shortfall(), 0.0);
        assert_eq!(b.pending, 225.0);
        // A partially productive plant contributes only its remaining gain.
        let mut b = Budget::default();
        b.production(10.0, 35.0, true);
        assert_eq!(b.made + b.pending, 35.0);
        // Once finished it moves from pending to made, not both.
        let mut b = Budget::default();
        b.production(35.0, 35.0, false);
        assert_eq!(b.made + b.pending, 35.0);
        assert_eq!(b.pending, 0.0);
    }

    #[test]
    fn goal_grows_with_economy_and_time_not_with_new_reactors() {
        let mut b = rich();
        b.set_target(true, true, 0.4, 18000);
        let target = b.target;
        b.reactors += 1;
        b.set_target(true, true, 0.4, 18000);
        assert_eq!(b.target, target);
        b.set_target(true, true, 0.4, 36000);
        assert!(b.target > target);
        b.raw_metal *= 2.0;
        b.metal *= 2.0;
        b.set_target(true, true, 0.4, 18000);
        assert!((b.target - target * 2.0).abs() < 0.001);
    }

    #[test]
    fn one_starter_even_when_no_frame_exists_and_after_long_waits() {
        let mut states = [TeamEnergy::default(), TeamEnergy::default(), TeamEnergy::default()];
        for frame in [0, 30, 60, 1800, 3600, 18000] {
            let ready: Vec<_> = states
                .iter_mut()
                .enumerate()
                .filter_map(|(i, s)| s.ready(Some(1), i as i32 + 1, frame).then_some(i + 1))
                .collect();
            assert_eq!(ready, if frame < HANDOVER { vec![] } else { vec![1] });
        }
        // A dead starter's replacement waits while old queues are cleared.
        assert!(!states[0].ready(Some(2), 1, 18030));
        assert!(!states[1].ready(Some(2), 2, 18030));
        assert!(states[1].ready(Some(2), 2, 18090));
    }

    #[test]
    fn election_ignores_humans_and_dead_peers_and_falls_back_without_singu_builders() {
        let factories = BTreeSet::from([1, 2, 3, 4]);
        let builders = BTreeSet::from([2, 3, 4]);
        let advanced = BTreeSet::from([3, 4]);
        assert_eq!(starter([1, 2, 3].into_iter(), &factories, &builders, &advanced, 0), Some(2));
        assert_eq!(starter([1, 2, 3].into_iter(), &factories, &builders, &advanced, 3), Some(3));
        // Team 4 is a human; only they can build a Singu. AI 2 can still
        // supply Fusions, without appointing the human as its coordinator.
        assert_eq!(starter([1, 2].into_iter(), &factories, &builders, &advanced, 3), Some(2));
        assert_eq!(starter([1].into_iter(), &factories, &builders, &advanced, 3), None);
    }
}
