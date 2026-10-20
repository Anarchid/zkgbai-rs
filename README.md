![zkgbai](https://cloud.githubusercontent.com/assets/3822768/11410309/23799b46-93cf-11e5-8761-e92bacbf0763.png)

zkgbai-rs
=========

The "Zero-K Graph-Based AI": a skirmish AI for the [Zero-K](https://zero-k.info) RTS on the
[Recoil](https://github.com/beyond-all-reason/RecoilEngine) engine. It reasons over a graph of the
map's metal spots (a Delaunay triangulation) to infer territory, front lines and enemy expansion in
the fog of war, and drives raiders, assault squads, artillery, air and the economy from it.

This is the Rust rewrite. It is a native engine AI (a plain `cdylib` speaking the engine's C AI
interface), so unlike the original Java AI it needs no JVM and ships the same way CircuitAI does:
a folder with `libSkirmishAI.so`, `SkirmishAI.dll`, `AIInfo.lua` and `AIOptions.lua`.

Layout
------

| Path | What |
|------|------|
| `crates/springai-sys` | Raw bindings to the engine's Skirmish AI C interface, pre-generated from the Recoil headers (no libclang or engine checkout needed to build). Regenerate with `tools/regen-bindings.sh`. |
| `crates/springai` | Safe Rust interface: `Copy` handles for units/defs/features, engine queries and orders, decoded events, and `export_ai!`, which contains every panic at the FFI boundary, supports several AI instances per process and queues re-entrant events. |
| `crates/terrain` | `zk-terrain`: engine-independent terrain model. For each movement tier (spider, bot, vehicle, hover, ship, amphibious) the map is split by [zkta](https://github.com/Anarchid/zkta-rust) into regions joined by chokepoints: zkta's gates, and borders where regions touch over slopes units crawl up (or open ground) without one. Each chokepoint is measured on the ground: how much longer than flat ground crossing it takes either way (units slow down only climbing), how much longer going round a gate over its sides takes, and how much wall would close it. Routes over the region graph count the climb; holding a chokepoint costs more the wider the front its defenders must cover and less the longer attackers climb into it. With a region lookup per position and static connectivity. `strategy` analyses the region graph against the game state: which chokepoints guard which regions, which to hold or wall, and where bridges, air drops or teleports would hurt the enemy most. `occupancy` keeps track of who holds each region (strength, anti-air and value of either side) and of the ground ways a force has if it must fight through them. `threat` reasons about forces on the move: how soon a raid can reach each region (chokepoints held against it stop it) and where defenders can stand first to keep it from the most value. |
| `crates/zkgbai` | The AI. `src/ai.rs` owns all state and dispatches events to the modules: `graph` (metal-spot graph, territory, overdrive grid), `economy` (workers, tasks, factories, the energy grid and linking power-hungry buildings to it before they are built, terraforming: ditches against vehicles and spikes for advanced radar sited by sight lines, Berthas sited by shell paths, missile silos made firebases, colonies of far-off free metal), `military` (enemy tracking, threat maps, anti-air we do not see placed from the bearings of its hits on our aircraft, targeting, unit handlers, enemy incursions and the squads sent to block them, the Djinn evacuating retreating striders, Charon air drops where the walk is long or held against us and the sky clear, nukes and tactical missiles, Widows scouting for them and deep into the enemy's land, intel: how much enemy metal is unaccounted for, and when that calls for an anti-nuke), `kgbutil` (pathfinder, terrain checks), `terrain` (the terrain model, built on a background thread and re-analysed every minute so terraforming shows up, and the periodic strategic analysis), `los`, and `operator` (the operator API and run journal, below). |
| `tools/arena` | Headless match harness: `match.sh` plays one game, `series.sh` plays a map pool in parallel and summarises win rates. |
| `tools/terrain-view` | Renders a tier's regions (darker on slopes units crawl up), gates (red, orange where going round them is easy) and borders (magenta where crossing is free, blue with a yellow tick into the side it climbs into) to PNG and, given a scenario dumped by the AI (`ZKGBAI_DUMP_TERRAIN=<dir>`), overlays and prints the strategic analysis. |
| `tools/operator` | `zkop.py`: finds running instances, calls their tools, follows their events, summarizes run journals. `luaui/gui_zkgbai_operator.lua`: the same in the game, as a LuaUI widget. |
| `tools/dll-smoke` | Loads a built library outside the engine and checks its FFI contract (exports, panic containment). |

Building
--------

Development build (needs Rust ≥ 1.88):

    cargo build --release -p zkgbai
    cargo test -p zkgbai

Release package for Linux and Windows (cross-built; the Linux library targets glibc 2.17):

    tools/setup-toolchain.sh   # once: zig + cargo-zigbuild into .toolchains
    tools/package.sh           # -> dist-rs/AI/Skirmish/ZKGBAI/<version>/

`tools/package.sh --native` skips cross-building and only produces the `.so` for this machine.

Inside a Recoil engine checkout, this repository can live at `AI/Skirmish/ZKGBAI`: the root
`CMakeLists.txt` builds it with cargo as part of the engine's AI build and installs it like the C++
AIs (version from `VERSION`).

Installing
----------

Copy `dist-rs/AI/Skirmish/ZKGBAI/<version>` to `<spring data dir>/AI/Skirmish/ZKGBAI/<version>`,
e.g. `~/.spring/AI/Skirmish/ZKGBAI/0.2.0`. Lobbies listing local AIs will then offer it.

Testing against other AIs
-------------------------

Requires a Zero-K data dir (`~/.spring` with an engine under `engine/linux64`, the game and maps).

    tools/arena/match.sh -m "Comet Catcher Redux v3.1" \
        "ZKGBAI:dev@$PWD/dist-rs/AI/Skirmish/ZKGBAI/0.2.0/libSkirmishAI.so" "CircuitAI:stable"
    tools/arena/series.sh -n 2 -j 4 "ZKGBAI:dev@<lib>" "1052188CircuitAIBrutal64:stable"

The harness runs `spring-headless` at 20× speed in throw-away write dirs, injects a small LuaUI widget
that reports the winner and per-team stats, and ends the game on game over or at a frame cap
(`-f`). Each match prints a `RESULT winner=A|B|draw|frames|timeout|crash ... panics=N` line.
Setting `ZKGBAI_FACTORY` to factory unit names (e.g. `factorycloak,factorygunship`) in the
environment makes the AI build those factories first, in that order (the first as `kgbfac` below);
`ZKGBAI_FACTORY_PICK=<n>` makes it open with the n-th (modulo) of the factories it deems viable on
the map, which `series.sh -R` sets per job so that two series face the same matchups.
With `ZKGBAI_TERRAFORM_TEST` set, the AI does nothing but have its commander dig test ditches of
several depths and log how steep they come out and which movement tiers can cross them (play it
against `NullAI:0.1`). `ZKGBAI_DISABLE` switches features off for A/B series, comma-separated:
`porc` (no turrets where enemy ground forces cannot get), `reach` (reachability checks for jobs,
targets and factory choice), `refuge`, `teleport`, `drops`, `ditch`, `reanalysis`, `intel`, `grid`, `nukes`, `missiles`,
`scouts`, `radar` (watchtowers), `requisition`, `colonize`, `aaintel` (anti-air inferred from hits on our aircraft).
`ZKGBAI_NUKE_TEST` cheats the AI a nuke silo and logs which way of ordering it fires a missile.
The AI option `test` stages a strike on an anti-nuke with cheats: play `ZKGBAI?test=bite` against
`ZKGBAI?test=anti` (an anti-nuke and four Fusions at its base) and the attacker, given a silo and a
spider factory, should be intercepted, find the anti-nuke with Widows, stun it and get a missile through;
with `test=eos` it gets a missile silo by the Fusions instead, and its Eos should count each one they hit gone.
With `test=drop` (hovers) or `test=dropbot` (bots) it gets four Charons and four assaults and riots every
three minutes and drops them by the defender's buildings, walkable or not: a check of the air route,
landing spots and unload handling (`drops:` lines, with a status line every 30 s in the air).
`test=bertha` gives it five Fusions and a radar at once (a superweapon soon, sited by shell paths, its
site linked to power first); `test=colony` a constructor and a Charon, `test=colonyair` an air
constructor, to colonize the free metal far off. `ZKGBAI?test=aaprobe` against `ZKGBAI?test=aafort`
flies unarmed Charons over static and mobile anti-air the attacker never sees (`air-intel:` lines:
the raw hits, and every minute the contacts it has placed, to compare with the fort's).
`ZKGBAI_SEED=<n>` fixes the AI's random seed (otherwise it comes from the clock).

Operator API and run journal
----------------------------

Another process (an LLM agent, the dashboard, a script) can read what the AI knows and steer what it
does while it plays, and every run can be recorded for afterwards. Both are off unless asked for, per
instance by AI option or for all instances by environment variable:

- `operator` / `ZKGBAI_OPERATOR`: `on` serves on `127.0.0.1:7460` (or the next free port: a second
  instance takes 7461), or give a port or `host:port`.
- `journal` / `ZKGBAI_JOURNAL`: `on` writes `journal/<run>.jsonl` in the AI's data dir, or give a
  directory. With `tools/arena/match.sh`, `ZKGBAI_JOURNAL=out` puts each match's journals next to its
  infolog.

The server answers on one port:

| Endpoint | What |
|----------|------|
| `/` | The dashboard: the map (terrain, metal spots and owners, our units by role, enemies fading with time since seen, squads and their targets, places to defend, threat and value overlays), economy and army charts, the event log, an inspector, and a console for every tool. Select units (click, shift-drag) and right-click the map to order them, send a squad, set a defense point or a construction task. Scrub the timeline back over the game, or open a journal (from disk or the AI's journal list) to replay it. |
| `/mcp` | MCP over Streamable HTTP: `claude mcp add --transport http zkgbai http://127.0.0.1:7460/mcp` gives an agent the tools. |
| `/rpc` | Plain JSON-RPC 2.0: the method is a tool's name, the params its arguments. |
| `/call/<tool>` | `curl 'localhost:7460/call/enemies?kind=porc&limit=5'`, or POST the arguments as JSON. |
| `/tools` | Every tool with its description and input schema. |
| `/events` | The event stream as server-sent events (`?kinds=log,kill&since=<seq>`). |
| `/snapshot`, `/journal`, `/journals`, `/health` | The latest positions (served even while the game is paused), this run's journal, the journal directory. |

Tools that read: `status`, `economy`, `workers`, `tasks`, `factories`, `military`, `squads`,
`units`, `unit`, `enemies`, `threat_at`, `threat_map`, `spots`, `map_info`, `terrain`, `terrain_map`
(the segmentation of a movement tier: regions per cell, who holds them, chokepoints), `events`,
`defs`, `snapshot`, `overrides`, `profile`. Tools that act: `say`, `mark`, `note` (into the
journal), `feature` (switch a dev feature off or on mid-game), `fac_queue` (production orders the
factories serve before their own choice), `requisition` (the next factory), `build` (a prioritized
construction task), `defend` (a place the army defends as if attacked there), `squad_target`
(override a squad's target for a while), `take` / `release` / `order` (hold units: the engine
wrapper drops the AI's own orders to them, `springai::reserved`, until they are released). Calls run
on the AI's thread at its next frame, so they are answered while the game runs.

A journal is JSON lines, `{"seq":..,"f":frame,"k":kind,...}`: a `run` header (build, map, teams,
options, seed), the `map`, then `log` lines (each AI log line, `mod` its module), `unit+`/`unit-`,
`kill`, `enemy+`, `given`, `say`/`chat`, `op` (each operator action with its result) and `note`,
`tick` every second (resources, army values, spots) and `snap` every five seconds (positions), and an
`end` record with the CPU profile. `tools/operator/zkop.py journal <file>` summarizes one;
`zkop.py ls`, `call`, `tools` and `events` talk to the running instances, which announce
themselves in `<tmp>/zkgbai-operator/`.

In the game itself, the LuaUI widget `tools/operator/luaui/gui_zkgbai_operator.lua` (copy it to
`<spring data dir>/LuaUI/Widgets/`; it needs local widgets on, Zero-K's default) finds the instances
on this machine over LuaSocket and draws what the one in focus thinks into the world: its squads and
where they go, places it defends, where its other units mean to go (a worker's task, a raider's
target), its memory of the enemy (where it last saw each unit and how long ago, and the reach of
remembered defenses), its view of the metal spots, the units the operator holds, and the chokepoints
of the terrain analysis. A GL4 shader draped over the terrain (Zero-K's `LuaShader`) shows one of
its threat or value maps, smooth with contour lines, and the segmentation of a movement tier:
regions by id, or by who holds them (ours blue, the enemy's red, sheltered ones hatched). Its panel
is a Chili window: toggles, the map and terrain pickers, the AI in focus, and its log. Select the
AI's units (as a spectator, or an AI ally's units) and right-click to steer them: move, attack-move
with ctrl, attack or guard a unit, queue with shift; alt sends their whole squad, ctrl+alt makes a
place to defend. `/luaui zkop` takes `focus <team>`, `layer <name>`, `terrain <off|regions|holders>
[tier]`, `release`, `build <unitdef>` and `defend` (at the mouse), `fac <unitdef> [n]`, `say`,
`note`, `feature <name> on|off`. Without GL4 the value map is drawn as quads, without Chili the
panel by hand. `tools/arena/match.sh` installs extra widgets for a match
from `ARENA_WIDGETS`.

In-game commands
----------------

Type these in chat in a game with the AI:

- `kgbdebug` — toggle debug mode (internal errors are also said in chat) and print economy figures.
- `kgbprofile` — write per-module CPU time to the infolog (also logged when the game ends).
- `kgbgraph` — dump the metal-spot graph as ASCII art to `zkgbai_graph.txt`.
- `kgbmap threat|porc|riot` — dump a threat map as ASCII art.
- `kgbterrain spider|bot|vehicle|hover|ship|amphib` — mark a tier's regions, chokepoints and gates
  on the map (gates with their walls and ways round, borders, and climbs into either side);
  `kgbterrain strategy` marks the chokepoints the AI holds, `kgbterrain threat` the enemy
  incursions, the regions they reach within 30 s and the squads blocking them, `kgbterrain occupancy`
  who holds each region of the strategy's tier (enemy and our strength, enemy anti-air and value).
- `kgbfac veh|hover|tank|cloak|shield|amph|spider` — choose the AI's first factory (1v1 only).

Lineage
-------

The original Java AI lives at https://github.com/Anarchid/zkgbai and a C++ port at
https://gitlab.com/talas777/ZKGBAI. This rewrite was ported from the C++ version and cross-checked
function by function against the Java original, fixing translation bugs found along the way.

License
-------

GNU General Public License, version 2 (see `LICENSE`).
