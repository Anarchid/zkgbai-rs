//! The AI root: owns every module, dispatches engine events, and handles
//! game-level concerns (start position, team merging, resigning, chat).

use std::collections::{BTreeMap, BTreeSet};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::time::{Duration, Instant};

use springai::{Callback, Event, Resource, SkirmishAi, Unit, Vec3, reserved};

use crate::defs::Defs;
use crate::economy::{EconomyManager, FactoryManager};
use crate::graph::GraphManager;
use crate::kgbutil::Pathfinder;
use crate::logistics::Logistics;
use crate::los::LosManager;
use crate::military::ammo::Ammo;
use crate::military::blasts::Blasts;
use crate::military::coalesce::Coalesce;
use crate::military::drops::Drops;
use crate::military::handlers::{BomberHandler, MiscHandler, RaiderHandler, RetreatHandler, SquadHandler};
use crate::military::incursions::Incursions;
use crate::military::intel::Intel;
use crate::military::jaeger::Jaegers;
use crate::military::jumps::Jumps;
use crate::military::missiles::Missiles;
use crate::military::nukes::Nukes;
use crate::military::puppies::Puppies;
use crate::military::scouts::Scouts;
use crate::military::stakeouts::Stakeouts;
use crate::military::stayveto::StayVeto;
use crate::military::strikes::Strikes;
use crate::military::sweeps::Sweeps;
use crate::military::teleport::Teleports;
use crate::military::{FighterStore, MilitaryManager, Squads, UnitClasses};
use crate::operator::Operator;
use crate::posture::Posture;
use crate::terrain::{Strategy, TerrainModel};
use crate::util::{distance, get_directional_point, kgb_rand, seed_rand};

/// Allied starts at least this far apart, elmos: where our start box has
/// fewer metal spots than we are, the later ones start off the spots.
const START_SPREAD: f32 = 300.0;
/// No start is worth more for being farther than this from the others.
const START_ROOM: f32 = 450.0;

/// Experimental features that are off unless named in the `enable` option
/// (or ZKGBAI_ENABLE): `pairs` (Constables pair up on builds) lost its
/// mirror 3-5, expanding slower (12.5 mexes to 16.5 at minute 10) though
/// losing fewer; `currency` (the danger of a job on an issue's crew priced
/// as the worker's expected loss, and the crew holding where it pays) lost
/// its mirrors 17-23 twice where the issue clients alone won 25-15.
/// `permanence` (the interception planner keeps unseen enemy groups 45 s,
/// dead reckoned toward our assets) doubled refuge orders (6→15 a game)
/// without changing mex losses (9.1→9.7) or blocks (0.8→0.5) against the
/// frozen hover baseline, 20 games each, Oct 22: the memory frightens
/// workers more than it positions squads.
/// `mexporc` (a turret by every mex and link) is off by default since
/// Oct 24: with the home-defense force standing in for the turret it won
/// 33-4-3 vs Circuit against 19-8-13 and 18-2 in self-play mirrors, with
/// +9 income at 10:00 and +2000 army at 15:00, for +1.6 mexes lost by
/// 10:00 pooled (5-8 on the lane maps: Altair, Trojan, Hide and Seek).
/// `airtempo` (an air factory weighed up by the clock and the map's size)
/// is unverified: its A/B is queued (see economy/planning.rs AIR_TEMPO_*).
/// `posture` (a fixed-defense budget from lanes and density) and
/// `ecoladder` (fusions from 30 metal income) are off since Oct 27: in the
/// Eye of Horus cloak mirror (zkgbai-runs/horus/mirror2, 10 games an arm)
/// both off went 9-0-1 against 3-3-4 with both on, with the old economy at
/// 10:00 (18.5 mexes to 12, 11.5 constructors to 8); either alone was no
/// better than both, each taking up the metal the other left.
/// The crushing-defeat veto (`fightsim`) is on. Raider packs and organic
/// fire squads remain opt-in. Ravager and Minotaur packs are on for WIP
/// observation (disable=ravager,minotaur to opt out). `coalesce` (raider
/// gangs judged by the fight predictor) is on for WIP observation
/// (2026-10-31, disable=coalesce to opt out). `stayveto` (retreats the
/// predictor says throw a won fight away) is off since the 2026-11-01
/// batteries (zkgbai-runs/coalesce/RESULTS.md): it lost its mirror 3-15
/// and changed nothing against Circuit or Wires, and its standing units
/// died more without killing more (enable=stayveto to opt in). The shadow
/// switches (journal only) stay off.
/// `densgate` (phase gates on the map's density clock) waits for its A/B
/// (enable=densgate to opt in).
/// `regionsplit` (terrain regions holding many metal spots cut into pieces
/// around clusters of them, 2026-11-02) is on in the wip line for
/// eyeballing (disable=regionsplit); off on main until its A/B.
/// `chatcommands` (kgbdebug, kgbfac...): the engine hands every AI every
/// chat line, whispers and the other side's ally chat included, with only
/// its sender, so anyone in the game could set our first factory or switch
/// our debug output.
/// `conquest` (the commander walks to forward ground a few turrets hold,
/// takes its metal, and fortifies it when the denial pays) is off in the
/// wip line since 2026-11-03 (user): in the cloak battery
/// (zkgbai-runs/conquest/compush b1, 36 pairs against Wires 0.1.2) the trip
/// was -0.15 +- 0.12 and compush -0.12 +- 0.12 against it off, and the
/// 317 sweep had the trip -0.14 +- 0.04 over 116 maps (enable=conquest to
/// opt in: the trip, or compush, below, with enable=compush). `conquestsym` (both
/// sides' walks to an objective measured alike) stays on for it
/// (2026-11-02; sym - on +0.07). `conquestleash`
/// (the commander keeps near the ground it took, b3 -0.11) and
/// `conquestseq` (a held objective leads on to the next, b4 -0.22 against
/// sym) stay off, as does `conquestescort` (a QRF team sized by the fight
/// predictor stands on the objective, 2026-11-03; battery e1 -0.14).
/// `compush` (with or without `conquest`): no conquest trip; the
/// commander's mex jobs lean toward the enemy, a constructor builds the
/// home energy, and objectives whose spots become ours by any hands get
/// their turrets. Off in the wip line since 2026-11-03 evening (user: "a bit
/// less certain" than energyneed; its 344 form never had a battery):
/// enable=compush to opt in (with or without enable=conquest, compush wins
/// over the trip). `conquestroute` (the trip's start and opening mexes planned
/// with it, one on one) is off (enable=conquestroute, with compush off).
/// `eramp` (energy wanted ramps with the alliance's share of the metal
/// spots and the clock, see economy/mod.rs ERAMP_*) is on in the wip line
/// (user, 2026-11-03; disable=eramp to opt out) while its A/B runs.
/// `ammo` (enemy tube-weapon shots counted from hits into the predictor's
/// snapshots, military/ammo.rs) is off: its A/B (zkgbai-runs/ammo/ab,
/// 2026-11-03) lost the mirror 19-33 and was -0.02 against Wires:v2 with a
/// ledger that is 99.5% right; the ledger still counts and journals.
pub(crate) const OFF_BY_DEFAULT: [&str; 25] = [
    "pairs",
    "currency",
    "permanence",
    "mexporc",
    "airtempo",
    "airfire",
    "posture",
    "ecoladder",
    "scorcher",
    "dagger",
    "blitz",
    "firesquads",
    "coalesceshadow",
    "stayshadow",
    "carpetspare",
    "chatcommands",
    "densgate",
    "fight2d",
    "conquest",
    "compush",
    "conquestleash",
    "conquestseq",
    "conquestescort",
    "conquestroute",
    "ammo",
];
// Siege unlocks after sitzkrieg; Crab support unlocks with a Crab.
// Both can still be disabled with `disable=siege,crab`.

/// A lost game (`resignvalue`): the alliance holds under half the spots we
/// hold hostile, and the enemy is estimated worth at least four times what
/// the alliance has. Hostile spots include mexes killed out of sight: the
/// enemy rebuilds them soon enough.
const RESIGN_SPOTS: f32 = 2.0;
const RESIGN_VALUE: f32 = 4.0;

/// Whether the alliance's numbers read lost (see `Ai::alliance_reads_lost`).
/// Replayed over the 512 arena games against Wires:v2 of Nov 1, this fires
/// before the old rule's resign in 6 of its 213 resigns, and in none of
/// the 298 games won or played to the cap; at the old rule's resigns the
/// enemy was worth a median 1.5 times us, and at 1.5 or 2 times this
/// would have given up 6 and 1 of those games. The old rule gave up Urban
/// Hell (Nov 1, 0.5.3 vs Wires) at 1.5x on 17 hostile spots to 11, 3 of
/// them mexes our Infernos had burnt out of sight. In team games it
/// read one player's share against all it saw of the enemy, and said
/// "looks resignable" at the first check in all six 3v3 smokes of Nov 1.
fn resign_reading(our_value: f32, enemy_value: f32, our_spots: usize, enemy_spots: usize) -> bool {
    enemy_spots as f32 > RESIGN_SPOTS * our_spots as f32 && enemy_value >= RESIGN_VALUE * our_value
}

/// The features switched off: the `disable` option (or ZKGBAI_DISABLE),
/// and those off by default that `enable` (or ZKGBAI_ENABLE) does not name.
/// An option carried empty (a loaded save keeps the slot's options, blank)
/// falls through to the environment.
fn dev_features(cb: Callback) -> BTreeSet<String> {
    let list = |key: &str, var: &str| -> BTreeSet<String> {
        cb.option(key)
            .filter(|v| !v.trim().is_empty())
            .or_else(|| std::env::var(var).ok())
            .map(|v| v.split(',').map(|f| f.trim().to_owned()).filter(|f| !f.is_empty()).collect())
            .unwrap_or_default()
    };
    let mut off = list("disable", "ZKGBAI_DISABLE");
    let on = list("enable", "ZKGBAI_ENABLE");
    off.extend(OFF_BY_DEFAULT.iter().filter(|f| !on.contains(**f)).map(|f| f.to_string()));
    off
}

/// Modules muted from the start: the `mute` option (or ZKGBAI_MUTE), a
/// comma list of module prefixes (see `springai::reserved::mute` and the
/// operator's `modules` tool).
fn muted_modules(cb: Callback) -> Vec<String> {
    cb.option("mute")
        .filter(|v| !v.trim().is_empty())
        .or_else(|| std::env::var("ZKGBAI_MUTE").ok())
        .map(|v| v.split(',').map(|m| m.trim().to_owned()).filter(|m| !m.is_empty()).collect())
        .unwrap_or_default()
}

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
    /// The project tracker (see `logistics`).
    pub logistics: Logistics,
    /// Fixed defenses' share and sites (see `posture`).
    pub posture: Posture,
    /// Our teleport bridge and the squads going through it.
    pub teleports: Teleports,
    /// Our air transports and the drop under way.
    pub drops: Drops,
    /// What we know, and can guess, of the enemy's metal.
    pub intel: Intel,
    /// Our nuclear strikes, and where they were intercepted.
    pub nukes: Nukes,
    pub blasts: Blasts,
    pub missiles: Missiles,
    pub sweeps: Sweeps,
    pub strikes: Strikes,
    /// Invasion targets (`invade`).
    pub invade: crate::military::invade::Invade,
    /// Widows scouting for our strikes.
    pub scouts: Scouts,
    /// Squads hunting the enemy's heavies.
    pub jaegers: Jaegers,
    /// Impaler batteries.
    pub impalers: crate::military::impalers::Impalers,
    pub taskforces: crate::military::taskforce::Taskforces,
    pub qrf: crate::military::qrf::Qrf,
    pub firefighters: crate::military::firefighters::Firefighters,
    pub raid_test: crate::military::raid_test::RaidTest,
    pub stomp_test: crate::military::stomp_test::StompTest,
    pub jumps: Jumps,
    /// Cyclopes driving into their cannon's reach.
    pub cyclopes: crate::military::cyclops::Cyclopes,
    /// The allies' initial queues, and our opening sent to them.
    pub iq: crate::iq::AllyPlans,
    pub puppies: Puppies,
    pub stakeouts: Stakeouts,
    /// Raider gangs judged by the fight predictor (`coalesce`).
    pub coalesce: Coalesce,
    /// What the enemy's tube weapons have left (`ammo`).
    pub ammo: Ammo,
    /// Retreats judged by the fight predictor (`stayveto`).
    pub stayveto: StayVeto,
    /// Which handler each unit belongs to (see `military::roles`).
    pub roles: crate::military::roles::Roles,
    pub air_intel: crate::military::air_intel::AirIntel,
    /// Fire shadows of enemy defenses, for precision raids.
    pub precision: crate::military::precision::Precision,

    /// All mobile combat units under handler control, by unit id.
    pub fighters: FighterStore,
    pub squads: Squads,
    pub retreat_handler: RetreatHandler,
    pub raider_handler: RaiderHandler,
    /// Raider squads' decisions (see `squad_trace`); every one journaled
    /// with `ZKGBAI_SQUAD_TRACE` set.
    pub squad_trace: crate::military::handlers::squad_trace::SquadTrace,
    pub squad_trace_detail: bool,
    pub squad_handler: SquadHandler,
    pub misc_handler: MiscHandler,
    pub bomber_handler: BomberHandler,

    /// The operator API and the run's journal.
    pub op: Operator,

    pub(crate) enemy_teams: BTreeSet<i32>,
    enemy_ally_teams: BTreeSet<i32>,
    pub debug_activated: bool,
    /// Development aid: features switched off, for A/B tests (the AI
    /// option `disable`, else `ZKGBAI_DISABLE`; comma separated).
    disabled: BTreeSet<String>,
    /// A staged test to play (AI option `test`, see `strike_test`).
    pub test: Option<String>,
    /// What it says in chat (see `banter`).
    pub banter: Option<crate::banter::Banter>,
    resigned: bool,
    /// The last resign check read the game lost (logged when it turns).
    reads_lost: bool,
    initialized: bool,
    /// Another instance of this AI on our team leads; we only hand over units.
    slave: bool,
    merge_target: i32,
    /// Per-module panic counts, for log rate limiting.
    pub(crate) panics: BTreeMap<&'static str, u32>,
    /// Per-handler CPU time, reported on release and by `kgbprofile`.
    pub(crate) profile: BTreeMap<&'static str, Timing>,
    /// Nesting depth of `guard` calls (handlers may guard their sub-steps).
    guard_depth: u32,
}

/// Accumulated wall time of one module handler.
#[derive(Default)]
pub(crate) struct Timing {
    pub calls: u64,
    pub total: Duration,
    pub max: Duration,
    /// Runs inside another guarded handler (excluded from the grand total).
    pub nested: bool,
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
        // The switches and metal spots first: `regionsplit` cuts the terrain
        // model's regions around the spots.
        let disabled = dev_features(cb);
        let graph = GraphManager::new(cb);
        let terrain =
            TerrainModel::start(cb, &defs, crate::terrain::split_spots(&graph, !disabled.contains("regionsplit")));
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
            graph,
            eco: EconomyManager::new(cb),
            war: MilitaryManager::new(cb),
            fac: FactoryManager::new(cb),
            pathfinder,
            terrain,
            strategy: None,
            incursions: Incursions::default(),
            logistics: Logistics::default(),
            posture: Posture::default(),
            teleports: Teleports::default(),
            drops: Drops::default(),
            intel: Intel::default(),
            nukes: Nukes::default(),
            blasts: Blasts::default(),
            missiles: Missiles::default(),
            sweeps: Sweeps::default(),
            strikes: Strikes::default(),
            invade: Default::default(),
            scouts: Scouts::default(),
            jaegers: Jaegers::default(),
            impalers: Default::default(),
            taskforces: Default::default(),
            qrf: Default::default(),
            firefighters: Default::default(),
            raid_test: Default::default(),
            stomp_test: Default::default(),
            jumps: Jumps::default(),
            cyclopes: Default::default(),
            iq: Default::default(),
            puppies: Puppies::default(),
            stakeouts: Stakeouts::default(),
            coalesce: Coalesce::default(),
            ammo: Ammo::default(),
            stayveto: StayVeto::default(),
            roles: Default::default(),
            air_intel: Default::default(),
            precision: Default::default(),
            defs,
            fighters: FighterStore::new(),
            squads: Squads::new(),
            retreat_handler: RetreatHandler::new(),
            raider_handler: RaiderHandler::new(),
            squad_trace: Default::default(),
            squad_trace_detail: std::env::var_os("ZKGBAI_SQUAD_TRACE").is_some(),
            squad_handler: SquadHandler::new(),
            misc_handler: MiscHandler::new(),
            bomber_handler: BomberHandler::new(),
            op: Operator::new(cb, seed),
            enemy_teams: BTreeSet::new(),
            enemy_ally_teams: BTreeSet::new(),
            debug_activated: false,
            test: cb.option("test"),
            disabled,
            banter: None,
            resigned: false,
            reads_lost: false,
            initialized: false,
            slave: false,
            merge_target: i32::MAX,
            panics: BTreeMap::new(),
            profile: BTreeMap::new(),
            guard_depth: 0,
        }
    }

    fn handle_event(&mut self, event: Event) -> i32 {
        if !self.op_observe(&event) {
            return 0;
        }
        match event {
            Event::Init { saved_game } => return self.init(saved_game),
            Event::Release { reason } => {
                self.log_profile();
                self.op_finish(reason);
                self.initialized = false;
            }
            Event::Update { frame } => self.update(frame),
            Event::Message { player, text } => self.message(player, &text),
            Event::LuaMessage { data } => {
                if self.active() {
                    self.guard("graph.luaMessage", |ai| ai.graph_lua_message(&data));
                    self.guard("nukes.luaMessage", |ai| ai.nukes_lua_message(&data));
                    self.guard("blasts.luaMessage", |ai| ai.blasts_lua_message(&data));
                    if data.starts_with("IQ|") {
                        self.guard("iq.luaMessage", |ai| ai.iq_lua_message(&data));
                    }
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
    pub(crate) fn active(&self) -> bool {
        self.initialized && !self.slave && !self.resigned
    }

    /// Where the AI is in its life: starting, playing, merged (handing its
    /// units to another instance) or resigned.
    pub fn state(&self) -> &'static str {
        if self.slave {
            "merged"
        } else if self.resigned {
            "resigned"
        } else if self.initialized {
            "playing"
        } else {
            "starting"
        }
    }

    /// Run one module's handler, isolating panics so one faulty module does
    /// not take the others down (the original caught exceptions per module).
    pub fn guard(&mut self, what: &'static str, f: impl FnOnce(&mut Ai)) {
        let start = Instant::now();
        self.guard_depth += 1;
        let result = catch_unwind(AssertUnwindSafe(|| reserved::as_module(what, || f(self))));
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
        // Units per role, and orders dropped because another module owned
        // the unit (see `military::roles`).
        self.log(&format!("roles: {}", self.roles_summary()));
    }

    /// `saved_game`: the engine resumed a save; the game is on, the
    /// commander and start position are history and the factories stand
    /// (the engine replays UnitCreated/UnitFinished for every unit next).
    fn init(&mut self, saved_game: bool) -> i32 {
        if saved_game {
            // The journal's run record and anything else written before the
            // first Update carry the resumed frame, not 0.
            self.frame = self.cb.game().current_frame();
            let standing = self.cb.team_units().len();
            self.log(&format!("init: loaded game, standing units {standing}"));
        }
        let h = self.cb.map().height() as f32;
        let w = self.cb.map().width() as f32;
        self.map_diag = (h * h + w * w).sqrt();

        self.identify_enemy_teams();
        self.identify_ally_teams();
        self.check_teams_merging();
        self.log("GraphManager initialized.");

        if self.slave {
            if !saved_game {
                self.select_random_commander();
                self.choose_start_pos();
            }
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

        if !saved_game {
            // The start first: what the opening's units and the commander
            // get to from it decides both (see `eco_opening_in_reach`).
            let opening = self.eco_roll_opening();
            self.choose_start_pos();
            let (opening, penned) = self.eco_opening_in_reach(opening);
            self.select_commander(opening.as_deref(), penned);
            self.guard("iq.init", |ai| ai.iq_init(opening.as_deref()));
        }
        if !self.disabled.is_empty() {
            let off: Vec<&str> = self.disabled.iter().map(String::as_str).collect();
            self.log(&format!("dev: features off: {}", off.join(", ")));
        }
        for m in muted_modules(self.cb) {
            reserved::mute(self.cb, &m);
        }
        let muted = reserved::muted(self.cb);
        if !muted.is_empty() {
            self.log(&format!("dev: modules muted: {}", muted.join(", ")));
        }
        self.banter_init();
        self.initialized = true;
        self.op_start();
        0
    }

    fn update(&mut self, frame: i32) {
        if self.slave {
            return;
        }
        self.frame = frame;
        self.iq.started = true;
        self.guard("operator.update", |ai| ai.op_update(frame));
        if frame == 0 {
            self.banter_start();
        }
        if self.resigned {
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
        self.guard("logistics.update", |ai| ai.logistics_update(frame));
        self.guard("posture.update", |ai| ai.posture_update(frame));
        if frame % 1800 == 0 {
            self.guard("pathfinder.updateSlopeMap", |ai| ai.pathfinder.update_slope_map());
        }
        if frame % 300 == 0 && frame > 18000 {
            self.check_resign();
        }
        if frame % 300 == 0 && frame > 18000 {
            self.maybe_taunt();
        }
        self.guard("banter.update", |ai| ai.banter_update(frame));

        self.guard("los.update", |ai| ai.los.update(frame));
        self.guard("graph.update", |ai| ai.graph_update(frame));
        // A staged anti-nuke stands by (see `strike_test`).
        if matches!(
            self.test.as_deref(),
            Some(
                "anti"
                    | "aafort"
                    | "fort"
                    | "skirmfort"
                    | "radarfort"
                    | "shadowprobe"
                    | "heavies"
                    | "veto-scalpels"
                    | "veto-scalpels10"
                    | "heavypush"
                    | "paladin"
                    | "farm"
                    | "snipers"
                    | "lances"
                    | "assassin-battery-observer"
                    | "assassin-battery-tremor"
                    | "assassin-battery-emissary"
                    | "assassin-target"
                    | "assassin-target2"
                    | "airstrike-targeting-targets"
                    | "airstrike-opportunity-targets"
                    | "airstrike-hunt-target"
                    | "airstrike-hunt-decoy"
                    | "airstrike-carpet-target"
                    | "airstrike-carpet-shooter"
                    | "airstrike-wave-targets"
                    | "airstrike-farm"
                    | "airstrike-singu"
                    | "airstrike-aa"
                    | "airstrike-priority-target"
                    | "airstrike-tough-target"
                    | "airstrike-fencer"
                    | "airstrike-shieldball"
                    | "airstrike-outpost"
                    | "airstrike-turrets"
                    | "airstrike-ravagers"
                    | "airstrike-ravagers-aa"
                    | "assassin-raider-lance"
                    | "assassin-raider-guarded"
                    | "assassin-lance"
                    | "assassin-aa"
                    | "assassin-aa-guarded"
                    | "assassin-aa-late-guard"
                    | "assassin-heavy"
                    | "assassin-commander"
            )
        ) {
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
        // With an ally the game is the alliance's to give up: "gg" and a
        // resign beside an ally is a base gone in a game that goes on (a 2v2
        // on The Cold Place, Oct 23: the enemy alliance's spots against this
        // team's own, 28 to 18, and the base gone at 15:00 with the human
        // ally still fighting). So with any ally but our own merged
        // instances, play on, and say to the team that it looks resignable
        // (their call). The reading is the alliance's either way (see
        // `alliance_reads_lost`). Never in an FFA: the rivals fight each
        // other too.
        if self.is_ffa {
            return;
        }
        let lost = if self.dev_off("resignvalue") { self.legacy_reads_lost() } else { self.alliance_reads_lost() };
        if lost != self.reads_lost {
            self.reads_lost = lost;
            if !lost {
                self.log("resign: no longer reads lost");
            }
        }
        if !lost {
            return;
        }
        if self.allies.iter().any(|a| !self.comrades.contains(a)) {
            self.banter_offer("hopeless", crate::banter::Fields::new());
            return;
        }
        self.resign();
    }

    /// Lost by the whole alliance's numbers (`resignvalue`, default on, see
    /// `resign_reading`): spots with our or an ally's mex against those we
    /// hold hostile, and the metal in our and our allies' units and
    /// buildings (commanders left out, as `Intel` leaves out the enemy's;
    /// nanoframes at full cost) against the enemy's estimated worth.
    fn alliance_reads_lost(&mut self) -> bool {
        let metal = self.metal;
        let mut value = 0.0f32;
        let mut spots = BTreeSet::new();
        for u in self.cb.friendly_units() {
            let Some(def) = u.def() else { continue };
            if self.defs.is_commander(def) {
                continue;
            }
            value += def.cost(metal);
            if self.graph.is_mex_def(def)
                && let Some(i) = self.graph.spot_under(u.pos())
            {
                spots.insert(i);
            }
        }
        let enemy_value = self.intel.estimated_value();
        let enemy_spots = self.graph.get_enemy_spots().len();
        let lost = resign_reading(value, enemy_value, spots.len(), enemy_spots);
        if lost && !self.reads_lost {
            self.log(&format!(
                "resign: reads lost: the alliance's {value:.0} metal against the enemy's ~{enemy_value:.0} ({:.1}x), \
                 {} spots against {enemy_spots} hostile",
                enemy_value / value.max(1.0),
                spots.len()
            ));
        }
        lost
    }

    /// The rule before `resignvalue`: this team's own spots and army against
    /// all it sees of the enemy, or no factory.
    fn legacy_reads_lost(&self) -> bool {
        let espots = self.graph.get_enemy_spots().len() as f32;
        let ospots = self.graph.get_owned_spots().len() as f32;
        espots > 1.5 * ospots
            && (self.war.enemy_fighter_value > self.fac.army_value * 1.5 || self.fac.factories.is_empty())
    }

    /// Give up the game by leaving it. Merged instances follow as our units
    /// reach them (see `unit_given`). A self-destruct is no resign: Zero-K's
    /// blocker (unit_self_destruct.lua) cancels one in any game but a 1v1 or
    /// FFA.
    fn resign(&mut self) {
        self.banter_resign();
        self.emote("resigned");
        self.leave_game();
        self.log("resign: left the game");
    }

    /// Leave the game as a crashed AI does: the hosting player kills this
    /// AI (`/aikill`), and Zero-K hands the team's units to an AI ally
    /// (game_lagmonitor.lua, never to a human one) and counts a team with
    /// neither AI nor players out (game_over.lua).
    fn leave_game(&mut self) {
        self.cb.game().send_text_message(&format!("/aikill {}", self.team_id), 0);
        self.resigned = true;
    }

    /// "It's over": well ahead, a taunt; behind after one (fewer spots than
    /// the enemy, or under two-thirds its army), the climb-down (see
    /// `banter::glue`).
    fn maybe_taunt(&mut self) {
        if self.is_ffa {
            return;
        }
        let espots = self.graph.get_enemy_spots().len() as f32;
        let ospots = self.graph.get_owned_spots().len() as f32;
        let (army, enemy) = (self.fac.army_value, self.war.enemy_fighter_value);
        let ahead = self.graph.territory_fraction > 0.5 && ospots > 1.5 * espots && army > enemy * 2.0 && espots > 0.0;
        let behind = ospots < espots || army * 1.5 < enemy;
        self.banter_claim(ahead, behind);
    }

    fn message(&mut self, player: i32, text: &str) {
        if !self.active() {
            return;
        }
        self.banter_hear(text);
        // Chat commands take anyone's line (see `OFF_BY_DEFAULT`).
        if self.dev_off("chatcommands") {
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
        self.guard("strikes.unitCreated", |ai| ai.strikes_created(unit, builder));
    }

    fn unit_given(&mut self, unit: Unit, old_team: i32, new_team: i32) {
        if self.slave && new_team == self.team_id {
            if self.comrades.contains(&old_team) {
                // A comrade's units come to us when it has left the game
                // (Zero-K hands a gone AI's units to an AI ally): follow it
                // out, the units passing on to the next of us.
                if !self.resigned {
                    self.leave_game();
                }
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
    pub(crate) fn dispatch(&mut self, event: Event) {
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
                self.guard("strikes.unitDamaged", |ai| ai.strikes_unit_damaged(unit, attacker, dir));
                self.guard("airIntel.unitDamaged", |ai| ai.air_intel_unit_damaged(unit, attacker, dir, weapon));
                self.guard("impalers.unitDamaged", |ai| ai.impalers_unit_damaged(unit, attacker, damage, weapon));
            }
            Event::UnitDestroyed { unit, attacker } => {
                self.guard("graph.unitDestroyed", |ai| ai.graph_unit_destroyed(unit, attacker));
                self.guard("eco.unitDestroyed", |ai| ai.eco_unit_destroyed(unit, attacker));
                self.guard("war.unitDestroyed", |ai| ai.war_unit_destroyed(unit, attacker));
                self.guard("fac.unitDestroyed", |ai| ai.fac_unit_destroyed(unit, attacker));
                self.guard("banter.unitDestroyed", |ai| ai.banter_unit_destroyed(unit));
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
            Event::EnemyDamaged { enemy, attacker, damage, .. } => {
                self.guard("impalers.enemyDamaged", |ai| ai.impalers_enemy_damaged(enemy, attacker, damage));
            }
            Event::WeaponFired { unit, weapon } => {
                if weapon.is_none_or(|w| w.range() > 0.0 && !w.name().contains("fake")) {
                    self.guard("pack.weaponFired", |ai| ai.pack_fired(unit));
                }
            }
            Event::EnemyDestroyed { enemy, attacker } => {
                self.guard("war.enemyDestroyed", |ai| ai.war_enemy_destroyed(enemy, attacker))
            }
            Event::EnemyCreated { enemy } => self.guard("eco.enemyCreated", |ai| ai.eco_enemy_created(enemy)),
            Event::EnemyFinished { enemy } => self.guard("war.enemyFinished", |ai| ai.war_enemy_finished(enemy)),
            _ => {}
        }
    }

    fn select_random_commander(&mut self) {
        self.select_commander(None, false);
    }

    /// The commander for opening with factory `opening`: a recon commander
    /// for jumpjets and spiders (it jumps too, and goes where they go), or
    /// where a walking one would be `penned` in at the start (see
    /// `eco_opening_in_reach`); a support commander otherwise.
    /// ZKGBAI_COMMANDER=<def> overrides.
    fn select_commander(&mut self, opening: Option<&str>, penned: bool) {
        let jumper = penned || matches!(opening, Some("factoryjump" | "factoryspider"));
        let name = match std::env::var("ZKGBAI_COMMANDER").ok().filter(|c| self.defs.get(c).is_some()) {
            Some(c) => c,
            None if jumper && self.defs.get("dyntrainer_recon_base").is_some() => "dyntrainer_recon_base".to_owned(),
            None => "dyntrainer_support_base".to_owned(),
        };
        let name = name.as_str();
        self.cb.call_lua_rules(&format!("ai_commander:{name}"));
        let human = self.defs.get(name).map(|d| d.human_name()).unwrap_or_else(|| name.to_owned());
        match opening {
            Some(f) => self.log(&format!("Selected Commander: {human} (opening with {f}).")),
            None => self.log(&format!("Selected Commander: {human}.")),
        }
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

    /// Allied instances of this AI merge into one (a hivemind) only when
    /// each has the `hivemind` option on (or ZKGBAI_HIVEMIND): the lowest
    /// team leads, the others hand it their units and stop playing. Off by
    /// default: each instance plays its own team, as beside any other ally.
    fn check_teams_merging(&mut self) {
        let script = self.cb.game().setup_script();
        let env = std::env::var("ZKGBAI_HIVEMIND").unwrap_or_default();
        // An instance's option, or the environment's when it is empty.
        let on = |v: &str| {
            let v = if v.trim().is_empty() { env.as_str() } else { v };
            matches!(v.trim().to_ascii_lowercase().as_str(), "on" | "1" | "true" | "yes")
        };
        // Ours comes with its AIOptions.lua default filled in by the engine.
        let hivemind = on(&self.cb.option("hivemind").unwrap_or_default());
        for t in self.allies.clone() {
            let in_group = match ingroup(&script, t, "hivemind") {
                Some(ally) => {
                    // The script holds only what the lobby set: an ally's
                    // option left unset takes the default in its own
                    // AIOptions.lua, as ours did, so both sides agree.
                    let theirs = if ally.value.trim().is_empty() {
                        let def = self.option_default(&ally.version, "hivemind").unwrap_or_default();
                        if !def.trim().is_empty() {
                            self.log(&format!(
                                "Allied {AI_SHORT_NAME} on team {t}: hivemind '{def}' by its AIOptions.lua default."
                            ));
                        }
                        def
                    } else {
                        ally.value
                    };
                    if hivemind && on(&theirs) {
                        true
                    } else {
                        self.iq.peers.insert(t);
                        self.log(&format!("Allied {AI_SHORT_NAME} on team {t} plays on its own (hivemind off)."));
                        false
                    }
                }
                None => false,
            };
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

    /// The default of option `key` in the AIOptions.lua of this AI's
    /// `version` (ours when empty): what the engine gives an instance of
    /// that version whose start script leaves `key` unset. Other versions
    /// are looked for beside ours, by the version their AIInfo.lua declares
    /// (as the engine knows them, whatever their directory is called).
    fn option_default(&self, version: &str, key: &str) -> Option<String> {
        use std::path::{Path, PathBuf};
        let read = |dir: &Path, file: &str| std::fs::read_to_string(dir.join(file)).ok();
        let declares = |dir: &Path| {
            read(dir, "AIInfo.lua").and_then(|info| lua_def_field(&info, "version", "value")).as_deref()
                == Some(version)
        };
        let ours = PathBuf::from(self.cb.config_dir()?);
        let dir = if version.is_empty() || declares(&ours) {
            Some(ours)
        } else {
            let beside = ours.parent().and_then(|p| std::fs::read_dir(p).ok());
            beside.into_iter().flatten().filter_map(|e| Some(e.ok()?.path())).find(|p| p.is_dir() && declares(p))
        };
        let Some(dir) = dir else {
            self.log(&format!("No {AI_SHORT_NAME} {version} found beside this one: its {key} option counts as unset."));
            return None;
        };
        read(&dir, "AIOptions.lua").and_then(|o| lua_def_field(&o, key, "def"))
    }

    fn choose_start_pos(&mut self) {
        let map_center = self.graph.get_map_center();
        // A rush planned (see `economy::rush`): we start together.
        if let Some(site) = self.rush_start() {
            self.send_start_pos(site, map_center);
            self.log("Teams Mode: Start Position together, for the rush.");
            return;
        }
        let mut spots: Vec<Vec3> = self.graph.get_ally_territory().iter().map(|s| s.get_pos()).collect();
        let all_spots: Vec<Vec3> = self.graph.get_metal_spots().iter().map(|s| s.get_pos()).collect();
        if spots.is_empty() {
            self.log("chooseStartPos: Startbox inference failed, or there were no mexes within the startbox.");
            return;
        }
        let priority_starts = self.allies.iter().filter(|&&a| a < self.team_id).count();

        // The alliance's water side (see `economy::plop`): its start is
        // decided, and taken out of the others' choice.
        let water = self.eco.water_side;
        if let Some((team, site)) = water {
            if team == self.team_id {
                self.send_start_pos(site, map_center);
                self.log("Teams Mode: Start Position by the water, for a water-capable opening.");
                return;
            }
            spots.retain(|&s| s != site);
        }
        if spots.is_empty() {
            self.log("chooseStartPos: no spot left in the start box but the water side's.");
            return;
        }

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

        // One on one with `conquestroute`: the start for the conquest trip's
        // walk (see economy/conquest/compush.rs).
        if self.allies.is_empty()
            && let Some(site) = self.conquest_route_start()
        {
            self.send_start_pos(site, map_center);
            self.log("Solo Mode: Start Position for the conquest route!");
            return;
        }
        if self.allies.is_empty() || (self.allies.len() > 1 && priority_starts == 0) {
            // Starting solo (or first of a team): pick a spot near map center.
            let best = central_spot(&spots);
            self.send_start_pos(best, map_center);
            self.log("Solo Mode: Start Position Selected!");
            return;
        }

        // On teams: spread out from previously chosen spots by team order
        // (and from the water side's).
        let mut cached: Vec<Vec3> = water.map(|(_, site)| site).into_iter().collect();
        if self.allies.len() > 1 {
            cached.push(central_spot(&spots));
        }
        // (Where the box has fewer spots than we are, a start off them: see
        // `spread_start`.)
        let box_spots = spots.clone();
        let mut best = *spots
            .iter()
            .max_by(|a, b| distance(**a, map_center).total_cmp(&distance(**b, map_center)))
            .expect("spots is not empty");
        if cached.iter().any(|&c| distance(c, best) < START_SPREAD) {
            best = self.spread_start(&cached, &box_spots).unwrap_or(best);
        }
        cached.push(best);
        spots.retain(|&s| s != best);

        if priority_starts > 1 || (self.allies.len() == 1 && priority_starts > 0) {
            for _ in 0..priority_starts.saturating_sub(1).max(1) {
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
                if chosen.is_none() || dist < START_SPREAD {
                    // Out of spots: AlienDesert's box has two for three of us,
                    // and the third started on the first (2026-11-03).
                    chosen = self.spread_start(&cached, &box_spots).or(chosen);
                }
                let Some(pick) = chosen else {
                    break;
                };
                best = pick;
                cached.push(best);
                spots.retain(|&s| s != best);
            }
        }

        self.send_start_pos(best, map_center);
        self.log("Teams Mode: Start Position Selected!");
    }

    /// A start in our box at least [`START_SPREAD`] from each of `taken`,
    /// for when the box has fewer metal spots than allies: on rings around
    /// the taken starts, the most spread one (up to [`START_ROOM`]), then
    /// the one nearest the box's metal (`spots`). Deterministic, since
    /// later allies replay the earlier ones' picks. `None` if no ring point
    /// is in the box that far from them all.
    /// Off with `disable=startspread`.
    fn spread_start(&self, taken: &[Vec3], spots: &[Vec3]) -> Option<Vec3> {
        if self.dev_off("startspread") {
            return None;
        }
        let size = self.graph.get_map_center() * 2.0;
        let mut best: Option<(f32, Vec3)> = None;
        for &t in taken {
            for radius in [START_SPREAD, 1.5 * START_SPREAD] {
                for k in 0..16 {
                    let a = k as f32 * std::f32::consts::TAU / 16.0;
                    let p = Vec3::xz(t.x + radius * a.cos(), t.z + radius * a.sin());
                    if p.x < 64.0 || p.z < 64.0 || p.x > size.x - 64.0 || p.z > size.z - 64.0 {
                        continue;
                    }
                    let spread = taken.iter().map(|&q| distance(q, p)).fold(f32::MAX, f32::min);
                    if spread < START_SPREAD {
                        continue;
                    }
                    let metal = spots.iter().map(|&s| distance(s, p)).fold(f32::MAX, f32::min);
                    let score = spread.min(START_ROOM) - 0.25 * metal;
                    if best.is_some_and(|(b, _)| b >= score) {
                        continue;
                    }
                    let check = format!("ai_is_valid_startpos:{:.6}/{:.6}", p.x, p.z);
                    if self.cb.call_lua_rules(&check).as_deref() == Some("1") {
                        best = Some((score, p));
                    }
                }
            }
        }
        if let Some((_, p)) = best {
            self.log(&format!(
                "Teams Mode: {} metal spots in our box for more starts: one off them at ({:.0}, {:.0})",
                spots.len(),
                p.x,
                p.z
            ));
        }
        best.map(|(_, p)| p)
    }

    /// Start a little toward the map center from the chosen spot, unless
    /// that leaves the start box (Zero-K then drops the position and the
    /// commander lands at a point of its own choosing). The position goes
    /// out at ground height, since Zero-K's pregame marker skips y = 0, and
    /// not-ready: the engine stamps an AI's start-pos packet with its host's
    /// player number and applies the ready flag to that player, so sending
    /// it ready readied the human hosting a local skirmish and started the
    /// game with no pregame. An AI team has no readiness of its own.
    fn send_start_pos(&mut self, spot: Vec3, map_center: Vec3) {
        let shifted = get_directional_point(spot, map_center, 75.0);
        let check = format!("ai_is_valid_startpos:{:.6}/{:.6}", shifted.x, shifted.z);
        let p = if self.cb.call_lua_rules(&check).as_deref() == Some("1") { shifted } else { spot };
        let startpos = Vec3::new(p.x, self.cb.map().elevation_at(p.x, p.z), p.z);
        self.cb.game().send_start_position(false, startpos);
        self.graph.set_start_pos(startpos);
    }

    pub fn get_enemy_ally_team_ids(&self) -> &BTreeSet<i32> {
        &self.enemy_ally_teams
    }

    pub fn get_enemy_team_ids(&self) -> &BTreeSet<i32> {
        &self.enemy_teams
    }

    /// Whether the game is one team against one: no allies, one enemy.
    pub fn is_1v1(&self) -> bool {
        self.allies.is_empty() && self.enemy_teams.len() == 1
    }

    /// Whether a feature is switched off for testing (see `disabled`).
    pub fn dev_off(&self, feature: &str) -> bool {
        self.disabled.contains(feature)
    }

    /// The fight predictor's mechanics fight as the switches ask for it, on
    /// every snapshot this AI builds: `fight2d` (units keep their places in
    /// the plane, no overlaps, splash, least-health targeting) and
    /// `fightfocus` (least-health targeting on the axis, on by default in the
    /// wip line; `disable=fightfocus`). `fight2d` is off by default.
    /// `sreload` (on; `disable=sreload`): tube weapons (Picket, Ogre, ...)
    /// fire from tubes that each reload on their own clock. `capture` (on;
    /// `disable=capture`): Dominatrix and Reef Gull capture rays take units
    /// over instead of doing nothing.
    pub fn sim_flags(&self) -> crate::military::fightsim::SimFlags {
        crate::military::fightsim::SimFlags {
            capture: !self.dev_off("capture"),
            ..crate::military::fightsim::SimFlags::from_switches(
                !self.dev_off("fight2d"),
                !self.dev_off("fightfocus"),
                !self.dev_off("sreload"),
            )
        }
    }

    /// Whom raiders shoot in the QRF predictor's raid-defense rollouts:
    /// `raidfire` (on by default; `disable=raidfire` restores the frozen
    /// rollout) lets them turn on our defenders once the asset they were seen
    /// shooting is gone, so a lone raider sent at a riot is priced as dying.
    pub fn raid_aim(&self) -> crate::military::fightsim::mech::RaidAim {
        use crate::military::fightsim::mech::RaidAim;
        if self.dev_off("raidfire") { RaidAim::Assets } else { RaidAim::Hold }
    }

    pub fn features_off(&self) -> Vec<String> {
        self.disabled.iter().cloned().collect()
    }

    /// Switch a feature off (or back on) while playing.
    pub fn set_feature_off(&mut self, feature: &str, off: bool) {
        let changed = self.dev_off(feature) != off;
        if off {
            self.disabled.insert(feature.to_owned());
        } else {
            self.disabled.remove(feature);
        }
        if feature == "commander" && off {
            self.eco_release_commander_defense();
        }
        if feature == "fortify" && changed {
            self.eco_fortify_switch_changed(off);
        }
    }

    pub fn log(&self, s: &str) {
        self.cb.log(s);
        if self.op.is_on() {
            // Lines are mostly "module: what happened".
            let module = s.split_once(':').map(|(m, _)| m).filter(|m| m.len() <= 24 && !m.contains(' '));
            self.op_emit("log", serde_json::json!({"mod": module, "msg": s}));
        }
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
        self.op_emit("say", serde_json::json!({"text": s}));
    }

    /// Say something to the spectators only (the engine's `s:` chat).
    pub fn say_spec(&self, s: &str) {
        self.cb.game().send_text_message(&format!("/say s:<{AI_SHORT_NAME}> {s}"), 0);
        self.op_emit("say", serde_json::json!({"text": s, "spec": true}));
    }

    pub fn emote(&self, s: &str) {
        self.cb.game().send_text_message(&format!("/say <{AI_SHORT_NAME} {s}>"), 0);
    }

    pub fn marker(&self, pos: Vec3, s: &str) {
        self.cb.map().add_point(pos, s);
    }
}

/// Another instance of this AI, as its start-script `[AI*]` section sets it.
#[derive(Debug, PartialEq)]
struct Instance {
    /// The option asked for, lowercased; empty when the script leaves it
    /// unset (the engine then applies that version's AIOptions.lua default,
    /// which the script does not show).
    value: String,
    /// Its `Version`, as written; empty when not given.
    version: String,
}

/// Whether team `team` is controlled by another instance of this AI,
/// according to the start script's `[AI*]` sections, and if so its option
/// `key` and version.
fn ingroup(script: &str, team: i32, key: &str) -> Option<Instance> {
    let team_key = format!("team={team}");
    let name_key = format!("shortname={}", AI_SHORT_NAME.to_ascii_lowercase());
    let option_key = format!("{}=", key.to_ascii_lowercase());
    let mut depth = 0;
    let mut in_ai = false;
    // The depth of the `[AI*]` header: the section's own keys sit one
    // level in, its `[Options]` deeper.
    let mut top = 0;
    let (mut right_id, mut right_ai) = (false, false);
    let mut found = Instance { value: String::new(), version: String::new() };
    for raw in script.lines() {
        let compact: String = raw.chars().filter(|c| !c.is_whitespace()).collect();
        let line = compact.to_ascii_lowercase();
        if line.starts_with("[ai") {
            in_ai = true;
            top = depth;
            (right_id, right_ai) = (false, false);
            found = Instance { value: String::new(), version: String::new() };
        }
        depth += line.matches('{').count() as i32;
        depth -= line.matches('}').count() as i32;
        if in_ai {
            let kv = line.trim_end_matches(';');
            if depth == top + 1 {
                right_id |= kv == team_key;
                right_ai |= kv == name_key;
                if kv.starts_with("version=") {
                    found.version = compact.trim_end_matches(';')["version=".len()..].to_owned();
                }
            } else if let Some(v) = kv.strip_prefix(&option_key) {
                found.value = v.to_owned();
            }
            // The options may follow the team, so read the whole section.
            if line.contains('}') && depth <= top {
                if right_id && right_ai {
                    return Some(found);
                }
                in_ai = false;
            }
        }
    }
    (in_ai && right_id && right_ai).then_some(found)
}

/// Field `field` of the entry whose `key` is `key` in a Lua definition
/// table such as AIOptions.lua or AIInfo.lua (`{ { key = 'k', def = 'v',
/// ... }, ... }`): strings unquoted, numbers and booleans as written. Reads
/// the literal tables these files hold, not Lua code.
fn lua_def_field(lua: &str, key: &str, field: &str) -> Option<String> {
    enum Tok {
        Str(String),
        Word(String),
        Open,
        Close,
        Eq,
    }
    let b = lua.as_bytes();
    let find = |from: usize, pat: &[u8]| b[from.min(b.len())..].windows(pat.len()).position(|w| w == pat);
    let mut toks = Vec::new();
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'-' if b.get(i + 1) == Some(&b'-') => {
                // A comment: a --[[ ]] block, or to the end of the line.
                i = if b[i + 2..].starts_with(b"[[") {
                    find(i + 4, b"]]").map_or(b.len(), |p| i + 4 + p + 2)
                } else {
                    find(i, b"\n").map_or(b.len(), |p| i + p + 1)
                };
            }
            q @ (b'\'' | b'"') => {
                let mut s = Vec::new();
                i += 1;
                while i < b.len() && b[i] != q {
                    if b[i] == b'\\' && i + 1 < b.len() {
                        i += 1;
                        s.push(match b[i] {
                            b'n' => b'\n',
                            b't' => b'\t',
                            c => c,
                        });
                    } else {
                        s.push(b[i]);
                    }
                    i += 1;
                }
                i += 1;
                toks.push(Tok::Str(String::from_utf8_lossy(&s).into_owned()));
            }
            b'{' | b'}' | b'=' => {
                toks.push(match b[i] {
                    b'{' => Tok::Open,
                    b'}' => Tok::Close,
                    _ => Tok::Eq,
                });
                i += 1;
            }
            c if c.is_ascii_alphanumeric() || c == b'_' || c == b'-' => {
                let start = i;
                i += 1;
                while i < b.len() && (b[i].is_ascii_alphanumeric() || matches!(b[i], b'_' | b'.')) {
                    i += 1;
                }
                toks.push(Tok::Word(lua[start..i].to_owned()));
            }
            _ => i += 1,
        }
    }
    // The `name = value` fields of each table open at this point.
    let mut open: Vec<Vec<(&str, &str)>> = Vec::new();
    let mut t = 0;
    while t < toks.len() {
        match (&toks[t], toks.get(t + 1), toks.get(t + 2)) {
            (Tok::Open, ..) => open.push(Vec::new()),
            (Tok::Close, ..) => {
                let fields = open.pop().unwrap_or_default();
                let get = |n: &str| fields.iter().find(|(k, _)| k.eq_ignore_ascii_case(n)).map(|&(_, v)| v);
                if get("key").is_some_and(|k| k.eq_ignore_ascii_case(key))
                    && let Some(v) = get(field)
                {
                    return Some(v.to_owned());
                }
            }
            (Tok::Word(name), Some(Tok::Eq), Some(Tok::Str(v) | Tok::Word(v))) => {
                if let Some(fields) = open.last_mut() {
                    fields.push((name, v));
                }
                t += 3;
                continue;
            }
            _ => {}
        }
        t += 1;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::{Instance, ingroup, lua_def_field, resign_reading};

    const SCRIPT: &str = "[GAME]\n{\n  [AI0]\n  {\n    Name=a;\n    ShortName=ZKGBAI;\n    Team=1;\n  }\n  [AI1]\n  {\n    ShortName=CircuitAI;\n    Team=2;\n  }\n  [AI2]\n  {\n    ShortName=ZKGBAI;\n    [Options]\n    {\n      Hivemind=On;\n      version=2;\n    }\n    Team=3;\n    Version=WIP-1;\n  }\n  [TEAM1]\n  {\n    TeamLeader=0;\n  }\n}\n";

    fn instance(value: &str, version: &str) -> Option<Instance> {
        Some(Instance { value: value.into(), version: version.into() })
    }

    #[test]
    fn resigns_only_crushed_on_both_counts() {
        // Urban Hell at the old rule's gg: 1.5x the worth, 17 hostile to 11.
        assert!(!resign_reading(28_864.0, 44_310.0, 11, 17));
        // Four times the worth on more than twice the spots.
        assert!(resign_reading(5_000.0, 20_000.0, 3, 7));
        // Either alone is not enough.
        assert!(!resign_reading(5_000.0, 20_000.0, 4, 8));
        assert!(!resign_reading(5_000.0, 19_900.0, 1, 9));
        // Nothing left: any hostile spot will do.
        assert!(resign_reading(0.0, 500.0, 0, 1));
        assert!(!resign_reading(0.0, 500.0, 0, 0));
    }

    #[test]
    fn finds_teams_run_by_this_ai() {
        assert_eq!(ingroup(SCRIPT, 1, "hivemind"), instance("", ""));
        assert_eq!(ingroup(SCRIPT, 2, "hivemind"), None);
        // The version keeps its case; an option of the same name is not it.
        assert_eq!(ingroup(SCRIPT, 3, "hivemind"), instance("on", "WIP-1"));
        assert_eq!(ingroup(SCRIPT, 4, "hivemind"), None);
    }

    /// The start script Chobby writes for a skirmish: tables first, tabs.
    #[test]
    fn reads_chobby_scripts() {
        let script = "[Game]\n{\n\n\t[ai0]\n\t{\n\t\t[options]\n\t\t{\n\t\t\thivemind = on;\n\t\t}\n\t\tName = ZKGBAI;\n\t\tTeam = 1;\n\t\tShortName = ZKGBAI;\n\t\tVersion = 0.5.1;\n\t}\n\n\t[ai1]\n\t{\n\t\tTeam = 2;\n\t\tShortName = ZKGBAI;\n\t\tVersion = wip;\n\t}\n\n\tmapname = Comet Catcher Redux v3.1;\n}";
        assert_eq!(ingroup(script, 1, "hivemind"), instance("on", "0.5.1"));
        assert_eq!(ingroup(script, 2, "hivemind"), instance("", "wip"));
    }

    /// The shape the engine's Lua parser needs, as far as our data files go:
    /// every short string closes on its own line (comments skipped) and the
    /// braces balance.
    fn lua_shape(lua: &str) -> Result<(), String> {
        let (mut depth, mut line, mut quote, mut escaped, mut closed) = (0i32, 1, None, false, false);
        let mut chars = lua.chars().peekable();
        while let Some(c) = chars.next() {
            // Right after a string: a separator, a closing bracket, `..` or a comment.
            if closed && !c.is_whitespace() {
                closed = false;
                if !matches!(c, ',' | '}' | ')' | ']' | '.' | ';' | '-') {
                    return Err(format!("line {line}: `{c}` right after a string"));
                }
            }
            if c == '\n' {
                if quote.is_some() {
                    return Err(format!("line {line}: a string runs past the end of its line"));
                }
                line += 1;
                continue;
            }
            match quote {
                Some(q) => {
                    if escaped {
                        escaped = false;
                    } else if c == '\\' {
                        escaped = true;
                    } else if c == q {
                        quote = None;
                        closed = true;
                    }
                }
                None => match c {
                    '\'' | '"' => quote = Some(c),
                    '[' if chars.peek() == Some(&'[') => return Err(format!("line {line}: long strings not checked")),
                    // A comment, to the end of the line.
                    '-' if chars.peek() == Some(&'-') => {
                        while chars.peek().is_some_and(|&n| n != '\n') {
                            chars.next();
                        }
                    }
                    '{' => depth += 1,
                    '}' if depth == 0 => return Err(format!("line {line}: a brace closes nothing")),
                    '}' => depth -= 1,
                    _ => {}
                },
            }
        }
        match (quote, depth) {
            (Some(_), _) => Err("a string is still open at the end".to_owned()),
            (None, 0) => Ok(()),
            (None, d) => Err(format!("{d} braces left open")),
        }
    }

    /// A quote inside a single-quoted `desc` ends the string early, and the
    /// engine, which parses every installed AI's options at startup, then
    /// dies for every game on the machine (`map's` in densgate's line,
    /// 2026-11-02). `lua_def_field` reads past such a quote, so the option
    /// tests did not see it.
    #[test]
    fn lua_data_files_have_closed_strings_and_balanced_braces() {
        for (name, lua) in [
            ("AIOptions.lua", include_str!("../data/AIOptions.lua")),
            ("AIInfo.lua", include_str!("../data/AIInfo.lua")),
        ] {
            assert_eq!(lua_shape(lua), Ok(()), "{name}");
        }
        assert!(lua_shape("local t = {\n\tdesc = 'the map's density clock',\n}\nreturn t\n").is_err());
        assert!(lua_shape("local t = {\n\tdesc = 'the map\\'s clock', -- the engine's\n}\nreturn t\n").is_ok());
        assert!(lua_shape("local t = {\n\tkey = 'x',\n").is_err());
        // Two stray quotes balance, but Lua still rejects the word after the first.
        assert!(lua_shape("local t = {\n\tdesc = 'the map's and the engine's clock',\n}\n").is_err());
    }

    #[test]
    fn reads_option_defaults() {
        let options = include_str!("../data/AIOptions.lua");
        assert_eq!(lua_def_field(options, "hivemind", "def").as_deref(), Some(""));
        assert_eq!(lua_def_field(options, "missile_silo_density", "def").as_deref(), Some("1500"));
        // Escaped quotes in the descriptions are skipped over.
        assert_eq!(lua_def_field(options, "colonize_policy", "def").as_deref(), Some("combined"));
        assert_eq!(lua_def_field(options, "nosuch", "def"), None);
        // The edit a player makes to switch it on for every instance.
        let at = options.find("key    = 'hivemind',").expect("hivemind is declared");
        let (head, entry) = options.split_at(at);
        let entry = entry.replacen("key    = 'hivemind',", "key = \"hivemind\", --[[ edited ]]", 1).replacen(
            "def    = '',",
            "def    = 'on', -- was ''",
            1,
        );
        let edited = format!("{head}{entry}");
        assert_eq!(lua_def_field(&edited, "hivemind", "def").as_deref(), Some("on"));
        assert_eq!(lua_def_field(&edited, "missile_silo_density", "def").as_deref(), Some("1500"));
        // List items (nested tables with keys of their own) are not options.
        let list = "local o = { { key = 'mode', type = 'list', def = 'b', items = { { key = 'a', name = 'A' }, { key = 'mode', name = 'M' } } } }";
        assert_eq!(lua_def_field(list, "mode", "def").as_deref(), Some("b"));
        let info = "local infos = {\n\t{\n\t\tkey    = 'version',\n\t\tvalue  = 'wip', -- AI version - !This comment is used for parsing!\n\t},\n}";
        assert_eq!(lua_def_field(info, "version", "value").as_deref(), Some("wip"));
    }
}
