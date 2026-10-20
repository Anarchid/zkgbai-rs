//! The AI root: owns every module, dispatches engine events, and handles
//! game-level concerns (start position, team merging, resigning, chat).

use std::collections::{BTreeMap, BTreeSet};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::time::{Duration, Instant};

use springai::{Callback, Event, Resource, SkirmishAi, Unit, Vec3};

use crate::defs::Defs;
use crate::economy::{EconomyManager, FactoryManager};
use crate::graph::GraphManager;
use crate::kgbutil::Pathfinder;
use crate::los::LosManager;
use crate::military::drops::Drops;
use crate::military::handlers::{BomberHandler, MiscHandler, RaiderHandler, RetreatHandler, SquadHandler};
use crate::military::incursions::Incursions;
use crate::military::intel::Intel;
use crate::military::missiles::Missiles;
use crate::military::nukes::Nukes;
use crate::military::scouts::Scouts;
use crate::military::sweeps::Sweeps;
use crate::military::teleport::Teleports;
use crate::military::{FighterStore, MilitaryManager, Squads, UnitClasses};
use crate::terrain::{Strategy, TerrainModel};
use crate::util::{distance, get_directional_point, kgb_rand, seed_rand};

/// The `shortName` this AI is registered under (see `data/AIInfo.lua`).
pub const AI_SHORT_NAME: &str = "ZKGBAI";

pub struct Ai {
    pub cb: Callback,
    pub team_id: i32,
    pub ally_team_id: i32,
    /// Current simulation frame (the original `currentFrame`).
    pub frame: i32,
    /// Per-instance frame offset (0..30) to stagger periodic work between
    /// several AI instances in one game.
    pub offset: i32,
    pub map_diag: f32,
    /// Allied team ids (excluding our own).
    pub allies: Vec<i32>,
    /// Allied teams also run by this AI and merged into one.
    pub comrades: BTreeSet<i32>,
    pub merged_allies: i32,
    pub unmerged_allies: i32,
    pub merged_ally_id: i32,
    pub is_ffa: bool,

    pub metal: Resource,
    pub energy: Resource,
    pub defs: Defs,
    pub unit_classes: UnitClasses,

    pub los: LosManager,
    pub graph: GraphManager,
    pub eco: EconomyManager,
    pub war: MilitaryManager,
    pub fac: FactoryManager,
    pub pathfinder: Pathfinder,
    /// Regions, chokepoints and connectivity per movement tier (built in the
    /// background; see [`Ai::terrain`]).
    pub terrain: TerrainModel,
    /// The latest strategic analysis, once the terrain model is ready.
    pub strategy: Option<Strategy>,
    /// Enemy forces on the move and the squads sent to block them.
    pub incursions: Incursions,
    /// Our teleport bridge and the squads going through it.
    pub teleports: Teleports,
    /// Our air transports and the drop under way.
    pub drops: Drops,
    /// What we know, and can guess, of the enemy's metal.
    pub intel: Intel,
    /// Our nuclear strikes, and where they were intercepted.
    pub nukes: Nukes,
    pub missiles: Missiles,
    pub sweeps: Sweeps,
    /// Widows scouting for our strikes.
    pub scouts: Scouts,
    pub air_intel: crate::military::air_intel::AirIntel,
    /// Fire shadows of enemy defenses, for precision raids.
    pub precision: crate::military::precision::Precision,

    /// All mobile combat units under handler control, by unit id.
    pub fighters: FighterStore,
    pub squads: Squads,
    pub retreat_handler: RetreatHandler,
    pub raider_handler: RaiderHandler,
    pub squad_handler: SquadHandler,
    pub misc_handler: MiscHandler,
    pub bomber_handler: BomberHandler,

    enemy_teams: BTreeSet<i32>,
    enemy_ally_teams: BTreeSet<i32>,
    pub debug_activated: bool,
    /// Development aid: features switched off, for A/B tests (the AI
    /// option `disable`, else `ZKGBAI_DISABLE`; comma separated).
    disabled: BTreeSet<String>,
    /// A staged test to play (AI option `test`, see `strike_test`).
    pub test: Option<String>,
    rsgn: bool,
    resigned: bool,
    initialized: bool,
    /// Another instance of this AI on our team leads; we only hand over units.
    slave: bool,
    merge_target: i32,
    /// Per-module panic counts, for log rate limiting.
    panics: BTreeMap<&'static str, u32>,
    /// Per-handler CPU time, reported on release and by `kgbprofile`.
    profile: BTreeMap<&'static str, Timing>,
    /// Nesting depth of `guard` calls (handlers may guard their sub-steps).
    guard_depth: u32,
}

/// Accumulated wall time of one module handler.
#[derive(Default)]
struct Timing {
    calls: u64,
    total: Duration,
    max: Duration,
    /// Runs inside another guarded handler (excluded from the grand total).
    nested: bool,
}

impl SkirmishAi for Ai {
    fn new(cb: Callback) -> Self {
        let game = cb.game();
        let map = cb.map();
        let (w, h) = (map.width() as f32, map.height() as f32);
        let defs = Defs::new(cb);
        // The lobby runs games in its own process (one pid for all of them),
        // so the clock goes in too; ZKGBAI_SEED fixes it for tests.
        let clock =
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos() as u64);
        let entropy = std::env::var("ZKGBAI_SEED").ok().and_then(|s| s.trim().parse::<u64>().ok());
        let seed =
            (cb.ai_id() as u64) << 32 ^ entropy.unwrap_or(std::process::id() as u64 ^ clock) ^ game.my_team() as u64;
        seed_rand(seed ^ 0x2545_F491_4F6C_DD1D);
        let pathfinder = Pathfinder::new(cb, &defs);
        let terrain = TerrainModel::start(cb, &defs);
        Ai {
            cb,
            team_id: game.my_team(),
            ally_team_id: game.my_ally_team(),
            frame: 0,
            offset: (kgb_rand() * 30.0).round() as i32,
            map_diag: (w * w + h * h).sqrt(),
            allies: Vec::new(),
            comrades: BTreeSet::new(),
            merged_allies: 0,
            unmerged_allies: 0,
            merged_ally_id: 0,
            is_ffa: false,
            metal: cb.resource_by_name("Metal").expect("game has no Metal resource"),
            energy: cb.resource_by_name("Energy").expect("game has no Energy resource"),
            unit_classes: UnitClasses::new(),
            los: LosManager::new(cb),
            graph: GraphManager::new(cb),
            eco: EconomyManager::new(cb),
            war: MilitaryManager::new(cb),
            fac: FactoryManager::new(cb),
            pathfinder,
            terrain,
            strategy: None,
            incursions: Incursions::default(),
            teleports: Teleports::default(),
            drops: Drops::default(),
            intel: Intel::default(),
            nukes: Nukes::default(),
            missiles: Missiles::default(),
            sweeps: Sweeps::default(),
            scouts: Scouts::default(),
            air_intel: Default::default(),
            precision: Default::default(),
            defs,
            fighters: FighterStore::new(),
            squads: Squads::new(),
            retreat_handler: RetreatHandler::new(),
            raider_handler: RaiderHandler::new(),
            squad_handler: SquadHandler::new(),
            misc_handler: MiscHandler::new(),
            bomber_handler: BomberHandler::new(),
            enemy_teams: BTreeSet::new(),
            enemy_ally_teams: BTreeSet::new(),
            debug_activated: false,
            test: cb.option("test"),
            disabled: cb
                .option("disable")
                .or_else(|| std::env::var("ZKGBAI_DISABLE").ok())
                .map(|v| v.split(',').map(|f| f.trim().to_owned()).filter(|f| !f.is_empty()).collect())
                .unwrap_or_default(),
            rsgn: false,
            resigned: false,
            initialized: false,
            slave: false,
            merge_target: i32::MAX,
            panics: BTreeMap::new(),
            profile: BTreeMap::new(),
            guard_depth: 0,
        }
    }

    fn handle_event(&mut self, event: Event) -> i32 {
        match event {
            Event::Init { .. } => return self.init(),
            Event::Release { .. } => {
                self.log_profile();
                self.initialized = false;
            }
            Event::Update { frame } => self.update(frame),
            Event::Message { player, text } => self.message(player, &text),
            Event::LuaMessage { data } => {
                if self.active() {
                    self.guard("graph.luaMessage", |ai| ai.graph_lua_message(&data));
                    self.guard("nukes.luaMessage", |ai| ai.nukes_lua_message(&data));
                }
            }
            Event::UnitCreated { unit, builder } => self.unit_created(unit, builder),
            Event::UnitGiven { unit, old_team, new_team } => self.unit_given(unit, old_team, new_team),
            event if self.active() => self.dispatch(event),
            _ => {}
        }
        0
    }
}

impl Ai {
    /// Modules receive events only after a successful init, and never in
    /// merge (slave) mode.
    fn active(&self) -> bool {
        self.initialized && !self.slave && !self.resigned
    }

    /// Run one module's handler, isolating panics so one faulty module does
    /// not take the others down (the original caught exceptions per module).
    pub fn guard(&mut self, what: &'static str, f: impl FnOnce(&mut Ai)) {
        let start = Instant::now();
        self.guard_depth += 1;
        let result = catch_unwind(AssertUnwindSafe(|| f(self)));
        self.guard_depth -= 1;
        let elapsed = start.elapsed();
        let nested = self.guard_depth > 0;
        let t = self.profile.entry(what).or_default();
        t.nested |= nested;
        t.calls += 1;
        t.total += elapsed;
        t.max = t.max.max(elapsed);
        if result.is_err() {
            let msg = springai::export::take_last_panic().unwrap_or_else(|| "unknown panic".into());
            let n = self.panics.entry(what).or_insert(0);
            *n += 1;
            // Avoid flooding the log from a handler that fails every frame.
            if *n <= 5 || n.is_power_of_two() {
                let count = *n;
                self.debug(&format!("{what} panicked (#{count}): {msg}"));
            }
        }
    }

    /// Log per-handler timings, most expensive first.
    fn log_profile(&self) {
        let total: Duration = self.profile.values().filter(|t| !t.nested).map(|t| t.total).sum();
        let frames = self.frame.max(1);
        self.log(&format!(
            "profile: {:.1} ms total over {} frames ({:.3} ms/frame)",
            total.as_secs_f64() * 1e3,
            frames,
            total.as_secs_f64() * 1e3 / frames as f64
        ));
        let mut rows: Vec<_> = self.profile.iter().collect();
        rows.sort_by_key(|(_, t)| std::cmp::Reverse(t.total));
        for (what, t) in rows.iter().take(12) {
            self.log(&format!(
                "profile: {:<28} calls={:<7} total={:>9.1}ms avg={:>7.3}ms max={:>7.2}ms",
                if t.nested { format!("  {what}") } else { what.to_string() },
                t.calls,
                t.total.as_secs_f64() * 1e3,
                t.total.as_secs_f64() * 1e3 / t.calls.max(1) as f64,
                t.max.as_secs_f64() * 1e3
            ));
        }
    }

    fn init(&mut self) -> i32 {
        let h = self.cb.map().height() as f32;
        let w = self.cb.map().width() as f32;
        self.map_diag = (h * h + w * w).sqrt();

        self.identify_enemy_teams();
        self.identify_ally_teams();
        self.check_teams_merging();
        self.log("GraphManager initialized.");

        if self.slave {
            self.select_random_commander();
            self.choose_start_pos();
            self.log("Entering merge mode! Discarding modules.");
            return 0;
        }

        // Weapons of static defenses, for threat classification.
        const PORC_DEFS: [&str; 10] = [
            "turretmissile",
            "turretlaser",
            "turretriot",
            "turretheavylaser",
            "turretgauss",
            "turretemp",
            "turretimpulse",
            "turretheavy",
            "turretantiheavy",
            "turrettorp",
        ];
        for name in PORC_DEFS {
            match self.defs.get(name) {
                Some(ud) => {
                    for wd in ud.weapon_mounts().iter().filter_map(|m| m.weapon_def()) {
                        self.unit_classes.porc_weps.insert(wd.id());
                    }
                }
                None => self.log(&format!("warning: unknown porc def '{name}'")),
            }
        }

        let mut ok = true;
        for (what, f) in [
            ("graph.init", Ai::graph_init as fn(&mut Ai)),
            ("eco.init", Ai::eco_init),
            ("war.init", Ai::war_init),
            ("fac.init", Ai::fac_init),
        ] {
            let before = self.panics.get(what).copied().unwrap_or(0);
            self.guard(what, f);
            ok &= self.panics.get(what).copied().unwrap_or(0) == before;
        }
        if !ok {
            self.log("module initialization failed");
            return 1;
        }

        self.select_random_commander();
        self.choose_start_pos();
        if !self.disabled.is_empty() {
            let off: Vec<&str> = self.disabled.iter().map(String::as_str).collect();
            self.log(&format!("dev: features off: {}", off.join(", ")));
        }
        self.initialized = true;
        0
    }

    fn update(&mut self, frame: i32) {
        if self.slave {
            return;
        }
        self.frame = frame;
        if frame == 0 {
            self.say("glhf!");
        }
        if self.resigned {
            // Keep self-destructing whatever is left (or given to us).
            if frame % 300 == 0 {
                for u in self.cb.team_units() {
                    u.self_destruct(0);
                }
            }
            return;
        }
        if !self.initialized {
            return;
        }
        self.guard("terrain.update", |ai| {
            ai.terrain_update(frame);
            ai.terrain_dump(frame);
        });
        self.guard("terrain.strategy", |ai| ai.strategy_update(frame));
        self.guard("war.incursions", |ai| ai.incursions_update(frame));
        if frame % 1800 == 0 {
            self.guard("pathfinder.updateSlopeMap", |ai| ai.pathfinder.update_slope_map());
        }
        if frame % 300 == 0 && frame > 18000 {
            self.check_resign();
        }
        if frame % 1350 == 0 && frame > 18000 {
            self.maybe_taunt();
        }

        self.guard("los.update", |ai| ai.los.update(frame));
        self.guard("graph.update", |ai| ai.graph_update(frame));
        // A staged anti-nuke stands by (see `strike_test`).
        if matches!(self.test.as_deref(), Some("anti" | "aafort" | "fort" | "shadowprobe")) {
            self.guard("strike_test.update", |ai| ai.strike_test_update(frame));
            return;
        }
        if self.terraform_testing() {
            self.guard("eco.terraformTest", |ai| ai.terraform_test_update(frame));
        } else {
            self.guard("eco.update", |ai| ai.eco_update(frame));
        }
        self.guard("war.update", |ai| ai.war_update(frame));
        self.guard("fac.update", |ai| ai.fac_update(frame));
    }

    fn check_resign(&mut self) {
        // A staged test plays on (see `strike_test`).
        if self.test.is_some() {
            return;
        }
        let espots = self.graph.get_enemy_spots().len() as f32;
        let ospots = self.graph.get_owned_spots().len() as f32;
        if !self.is_ffa
            && espots > 1.5 * ospots
            && (self.war.enemy_fighter_value > self.fac.army_value * 1.5 || self.fac.factories.is_empty())
        {
            self.resigned = true;
            self.say("gg!");
            self.emote("resigned");
            for u in self.cb.team_units() {
                u.self_destruct(0);
            }
        }
    }

    fn maybe_taunt(&mut self) {
        let espots = self.graph.get_enemy_spots().len() as f32;
        let ospots = self.graph.get_owned_spots().len() as f32;
        if !self.is_ffa
            && self.graph.territory_fraction > 0.5
            && ospots > 1.5 * espots
            && self.fac.army_value > self.war.enemy_fighter_value * 2.0
            && (!self.rsgn || kgb_rand() > 0.5)
            && espots > 0.0
        {
            self.rsgn = true;
            let rnd = kgb_rand();
            if rnd > 0.75 {
                self.say("Do you know what time it is?");
                self.say("It's time to resign!");
            } else if rnd > 0.5 {
                if self.enemy_teams.len() > 1 {
                    self.say("Resign lobsters!");
                } else {
                    self.say("Resign lobster!");
                }
            } else if rnd > 0.25 {
                self.say("It's over, resign already.");
            } else {
                self.say("Having trouble?");
                self.say("Type \"!resign\" for help.");
            }
        }
    }

    fn message(&mut self, player: i32, text: &str) {
        if !self.active() {
            return;
        }
        match text {
            "kgbdebug" => {
                self.debug_activated = !self.debug_activated;
                let reclaim = self.eco_get_reclaim_value();
                let msg = format!(
                    "Mobile BP: {} Income: {} Income + Reclaim Bonus: {} (debug {})",
                    self.fac.mobile_bp,
                    self.eco.effective_income_metal,
                    self.eco.effective_income_metal + reclaim / ((2.0 + 20.0 * self.graph.territory_fraction) * 25.0),
                    if self.debug_activated { "on" } else { "off" }
                );
                self.say(&msg);
            }
            "kgbprofile" => {
                self.log_profile();
                self.say("Profile written to infolog.");
            }
            t if t.starts_with("kgbterrain") => {
                self.terrain_draw(t.trim_start_matches("kgbterrain").trim());
            }
            "kgbgraph" => {
                self.graph.paint_ascii_graph();
                self.say("ASCII graph saved.");
            }
            t if t.starts_with("kgbmap") => {
                match t.get(7..) {
                    Some(s) if s.starts_with("threat") => self.war.paint_ascii_threat_map(),
                    Some(s) if s.starts_with("porc") => self.war.paint_ascii_porc_map(),
                    Some(s) if s.starts_with("riot") => self.war.paint_ascii_riot_map(),
                    _ => {
                        self.say("Unknown map, nothing will be saved.");
                        return;
                    }
                }
                self.say("ASCII map saved.");
            }
            _ => {}
        }
        self.guard("eco.message", |ai| ai.eco_message(player, text));
    }

    fn unit_created(&mut self, unit: Unit, builder: Option<Unit>) {
        if self.slave {
            self.cb.send_units(&[unit], self.merge_target);
            return;
        }
        if !self.active() {
            return;
        }
        self.guard("graph.unitCreated", |ai| ai.graph_unit_created(unit, builder));
        self.guard("eco.unitCreated", |ai| ai.eco_unit_created(unit, builder));
        self.guard("fac.unitCreated", |ai| ai.fac_unit_created(unit, builder));
    }

    fn unit_given(&mut self, unit: Unit, old_team: i32, new_team: i32) {
        if self.slave && new_team == self.team_id {
            if self.comrades.contains(&old_team) {
                // Merged teams resign by self-destructing individually.
                unit.self_destruct(0);
            } else {
                // Got the unit from someone else: pass it on to the leader.
                self.cb.send_units(&[unit], self.merge_target);
            }
            return;
        }
        if !self.active() {
            return;
        }
        self.guard("eco.unitGiven", |ai| ai.eco_unit_given(unit, old_team, new_team));
        self.guard("war.unitGiven", |ai| ai.war_unit_given(unit, old_team, new_team));
        self.guard("fac.unitGiven", |ai| ai.fac_unit_given(unit, old_team, new_team));
    }

    /// Route an event to the modules that handle it, in module order
    /// (los, graph, eco, war, fac).
    fn dispatch(&mut self, event: Event) {
        match event {
            Event::UnitFinished { unit } => {
                self.guard("graph.unitFinished", |ai| ai.graph_unit_finished(unit));
                self.guard("eco.unitFinished", |ai| ai.eco_unit_finished(unit));
                self.guard("war.unitFinished", |ai| ai.war_unit_finished(unit));
                self.guard("fac.unitFinished", |ai| ai.fac_unit_finished(unit));
            }
            Event::UnitIdle { unit } => {
                self.guard("eco.unitIdle", |ai| ai.eco_unit_idle(unit));
                self.guard("war.unitIdle", |ai| ai.war_unit_idle(unit));
                self.guard("fac.unitIdle", |ai| ai.fac_unit_idle(unit));
            }
            Event::UnitDamaged { unit, attacker, damage, dir, weapon, paralyzer } => {
                self.guard("eco.unitDamaged", |ai| ai.eco_unit_damaged(unit, attacker, damage, dir, weapon, paralyzer));
                self.guard("war.unitDamaged", |ai| ai.war_unit_damaged(unit, attacker, damage, dir, weapon, paralyzer));
                self.guard("sweeps.unitDamaged", |ai| ai.sweeps_unit_damaged(unit, attacker, dir, weapon));
                self.guard("airIntel.unitDamaged", |ai| ai.air_intel_unit_damaged(unit, attacker, dir, weapon));
            }
            Event::UnitDestroyed { unit, attacker } => {
                self.guard("graph.unitDestroyed", |ai| ai.graph_unit_destroyed(unit, attacker));
                self.guard("eco.unitDestroyed", |ai| ai.eco_unit_destroyed(unit, attacker));
                self.guard("war.unitDestroyed", |ai| ai.war_unit_destroyed(unit, attacker));
                self.guard("fac.unitDestroyed", |ai| ai.fac_unit_destroyed(unit, attacker));
            }
            Event::UnitCaptured { unit, old_team, new_team } => {
                self.guard("eco.unitCaptured", |ai| ai.eco_unit_captured(unit, old_team, new_team));
                self.guard("war.unitCaptured", |ai| ai.war_unit_captured(unit, old_team, new_team));
                self.guard("fac.unitCaptured", |ai| ai.fac_unit_captured(unit, old_team, new_team));
            }
            Event::EnemyEnterLos { enemy } => {
                self.guard("graph.enemyEnterLOS", |ai| ai.graph_enemy_enter_los(enemy));
                self.guard("eco.enemyEnterLOS", |ai| ai.eco_enemy_enter_los(enemy));
                self.guard("war.enemyEnterLOS", |ai| ai.war_enemy_enter_los(enemy));
                self.guard("fac.enemyEnterLOS", |ai| ai.fac_enemy_enter_los(enemy));
            }
            Event::EnemyLeaveLos { enemy } => self.guard("war.enemyLeaveLOS", |ai| ai.war_enemy_leave_los(enemy)),
            Event::EnemyEnterRadar { enemy } => self.guard("war.enemyEnterRadar", |ai| ai.war_enemy_enter_radar(enemy)),
            Event::EnemyLeaveRadar { enemy } => self.guard("war.enemyLeaveRadar", |ai| ai.war_enemy_leave_radar(enemy)),
            Event::EnemyDestroyed { enemy, attacker } => {
                self.guard("war.enemyDestroyed", |ai| ai.war_enemy_destroyed(enemy, attacker))
            }
            Event::EnemyCreated { enemy } => self.guard("eco.enemyCreated", |ai| ai.eco_enemy_created(enemy)),
            Event::EnemyFinished { enemy } => self.guard("war.enemyFinished", |ai| ai.war_enemy_finished(enemy)),
            _ => {}
        }
    }

    fn select_random_commander(&mut self) {
        let name = "dyntrainer_support_base";
        self.cb.call_lua_rules(&format!("ai_commander:{name}"));
        let human = self.defs.get(name).map(|d| d.human_name()).unwrap_or_else(|| name.to_owned());
        self.log(&format!("Selected Commander: {human}."));
    }

    fn identify_enemy_teams(&mut self) {
        let game = self.cb.game();
        for enemy_team in self.cb.enemy_teams() {
            let ally_team = game.team_ally_team(enemy_team);
            if game.rules_param_string(&format!("allyteam_short_name_{ally_team}"), "") != "Neutral" {
                self.enemy_teams.insert(enemy_team);
                if ally_team != self.ally_team_id {
                    self.enemy_ally_teams.insert(ally_team);
                }
            }
        }
        self.is_ffa = self.enemy_ally_teams.len() > 1;
    }

    fn identify_ally_teams(&mut self) {
        let game = self.cb.game();
        for i in 0..game.teams() {
            if game.team_ally_team(i) == self.ally_team_id && i != self.team_id {
                self.allies.push(i);
            }
        }
    }

    fn check_teams_merging(&mut self) {
        let script = self.cb.game().setup_script();
        for t in self.allies.clone() {
            let in_group = is_ingroup(&script, t);
            if in_group {
                self.comrades.insert(t);
            }
            if in_group && t < self.team_id {
                self.slave = true;
                self.merged_allies += 1;
                self.merge_target = self.merge_target.min(t);
            } else if in_group {
                self.merged_allies += 1;
                self.merged_ally_id = t;
            } else {
                self.unmerged_allies += 1;
            }
        }
    }

    fn choose_start_pos(&mut self) {
        let map_center = self.graph.get_map_center();
        let mut spots: Vec<Vec3> = self.graph.get_ally_territory().iter().map(|s| s.get_pos()).collect();
        let all_spots: Vec<Vec3> = self.graph.get_metal_spots().iter().map(|s| s.get_pos()).collect();
        if spots.is_empty() {
            self.log("chooseStartPos: Startbox inference failed, or there were no mexes within the startbox.");
            return;
        }
        let priority_starts = self.allies.iter().filter(|&&a| a < self.team_id).count();

        // Central spot that also has a close neighbour.
        let central_spot = |spots: &[Vec3]| -> Vec3 {
            let mut best = spots[0];
            let mut dist = 65535.0;
            for &ms in spots {
                let mut tmpdist = distance(ms, map_center);
                let nearest =
                    all_spots.iter().filter(|&&m| m != ms).map(|&m| distance(m, ms)).fold(65535.0f32, f32::min);
                if nearest != 65535.0 {
                    tmpdist += nearest;
                }
                if tmpdist < dist {
                    best = ms;
                    dist = tmpdist;
                }
            }
            best
        };

        if self.allies.is_empty() || (self.allies.len() > 1 && priority_starts == 0) {
            // Starting solo (or first of a team): pick a spot near map center.
            let best = central_spot(&spots);
            let startpos = get_directional_point(best, map_center, 75.0);
            self.cb.game().send_start_position(false, startpos);
            self.graph.set_start_pos(startpos);
            self.log("Solo Mode: Start Position Selected!");
            return;
        }

        // On teams: spread out from previously chosen spots by team order.
        let mut cached = Vec::new();
        if self.allies.len() > 1 {
            cached.push(central_spot(&spots));
        }
        let mut best = *spots
            .iter()
            .max_by(|a, b| distance(**a, map_center).total_cmp(&distance(**b, map_center)))
            .expect("spots is not empty");
        cached.push(best);
        spots.retain(|&s| s != best);

        if priority_starts > 1 || (self.allies.len() == 1 && priority_starts > 0) {
            for _ in 0..priority_starts.saturating_sub(1).max(1) {
                if spots.is_empty() {
                    break;
                }
                // The spot farthest from its closest already-chosen position.
                let mut dist = 0.0;
                let mut chosen = None;
                for &ms in &spots {
                    let tmpdist = cached.iter().map(|&p| distance(p, ms)).fold(65535.0f32, f32::min);
                    if chosen.is_none() || tmpdist > dist {
                        dist = tmpdist;
                        chosen = Some(ms);
                    }
                }
                best = chosen.expect("spots is not empty");
                cached.push(best);
                spots.retain(|&s| s != best);
            }
        }

        let startpos = get_directional_point(best, map_center, 75.0);
        self.cb.game().send_start_position(false, startpos);
        self.graph.set_start_pos(startpos);
        self.log("Teams Mode: Start Position Selected!");
    }

    pub fn get_enemy_ally_team_ids(&self) -> &BTreeSet<i32> {
        &self.enemy_ally_teams
    }

    pub fn get_enemy_team_ids(&self) -> &BTreeSet<i32> {
        &self.enemy_teams
    }

    /// Whether a feature is switched off for testing (see `disabled`).
    pub fn dev_off(&self, feature: &str) -> bool {
        self.disabled.contains(feature)
    }

    pub fn log(&self, s: &str) {
        self.cb.log(s);
    }

    /// Report an internal error: always logged, also said in chat when
    /// debugging is enabled (`kgbdebug`).
    pub fn debug(&self, s: &str) {
        let line = format!("error: {}", s.replace('\n', " "));
        self.log(&line);
        if self.debug_activated && self.frame > 0 {
            self.say(&line);
        }
    }

    pub fn say(&self, s: &str) {
        self.cb.game().send_text_message(&format!("/say <{AI_SHORT_NAME}> {s}"), 0);
    }

    pub fn emote(&self, s: &str) {
        self.cb.game().send_text_message(&format!("/say <{AI_SHORT_NAME} {s}>"), 0);
    }

    pub fn marker(&self, pos: Vec3, s: &str) {
        self.cb.map().add_point(pos, s);
    }
}

/// Whether team `team` is controlled by another instance of this AI,
/// according to the start script's `[AI*]` sections.
fn is_ingroup(script: &str, team: i32) -> bool {
    let team_key = format!("team={team}");
    let name_key = format!("shortname={}", AI_SHORT_NAME.to_ascii_lowercase());
    let mut depth = 0;
    let mut in_ai = false;
    let (mut right_id, mut right_ai) = (false, false);
    for raw in script.lines() {
        let line: String = raw.trim().to_ascii_lowercase().chars().filter(|c| !c.is_whitespace()).collect();
        if line.starts_with("[ai") {
            in_ai = true;
            (right_id, right_ai) = (false, false);
        }
        depth += line.matches('{').count() as i32;
        depth -= line.matches('}').count() as i32;
        if in_ai {
            let kv = line.trim_end_matches(';');
            right_id |= kv == team_key;
            right_ai |= kv == name_key;
            if right_id && right_ai {
                return true;
            }
            if line.contains('}') && depth <= 1 {
                in_ai = false;
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::is_ingroup;

    const SCRIPT: &str = "[GAME]\n{\n  [AI0]\n  {\n    Name=a;\n    ShortName=ZKGBAI;\n    Team=1;\n  }\n  [AI1]\n  {\n    ShortName=CircuitAI;\n    Team=2;\n  }\n  [TEAM1]\n  {\n    TeamLeader=0;\n  }\n}\n";

    #[test]
    fn finds_teams_run_by_this_ai() {
        assert!(is_ingroup(SCRIPT, 1));
        assert!(!is_ingroup(SCRIPT, 2));
        assert!(!is_ingroup(SCRIPT, 3));
    }
}
