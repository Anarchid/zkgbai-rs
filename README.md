<img width="1106" height="753" alt="Screenshot From 2026-10-27 14-54-01" src="https://github.com/user-attachments/assets/be16d3da-fc48-4778-bf22-3661d2cfec78" />
<img width="1911" height="966" alt="image" src="https://github.com/user-attachments/assets/76a7c004-719b-4891-914e-47e182f98be1" />

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

CI (`.github/workflows/rust.yml`) builds that package on every push to `main` and keeps it as a
run artifact (`ZKGBAI-<version>-g<commit>`, unpacking to `AI/Skirmish/ZKGBAI/<version>/`), after
the tests and a contract test of each library outside the engine (the `.dll` on Windows). Formatting
and clippy run beside it; they do not hold back the package, but a release needs them green.

Releasing: set `version` in `crates/zkgbai/Cargo.toml`, let cargo update `Cargo.lock`, commit, then
tag and push the tag:

    git tag v0.3.0 && git push origin main v0.3.0

The tag must match the crate version. CI then publishes a GitHub release with
`ZKGBAI-<version>.zip` (the whole `AI/Skirmish/ZKGBAI/<version>/` folder, both platforms),
`SHA256SUMS`, and build provenance attestations for the zip and both libraries. A version with a
pre-release suffix (`v0.3.0-rc1`) is published as a pre-release.

Inside a Recoil engine checkout, this repository can live at `AI/Skirmish/ZKGBAI`: the root
`CMakeLists.txt` builds it with cargo as part of the engine's AI build and installs it like the C++
AIs (version from `VERSION`).

Installing
----------

Unzip a release's `ZKGBAI-<version>.zip` into the spring data directory (the one holding `games/`
and `maps/`), or copy a local build's `dist-rs/AI/Skirmish/ZKGBAI/<version>` to
`<spring data dir>/AI/Skirmish/ZKGBAI/<version>`, e.g. `~/.spring/AI/Skirmish/ZKGBAI/0.4.0`.
Lobbies listing local AIs will then offer it.

Testing against other AIs
-------------------------

Requires a Zero-K data dir (`~/.spring` with an engine under `engine/linux64`, the game and maps).

    tools/arena/match.sh -m "Comet Catcher Redux v3.1" \
        "ZKGBAI:dev@$PWD/dist-rs/AI/Skirmish/ZKGBAI/0.4.0/libSkirmishAI.so" "CircuitAI:stable"
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
`scouts`, `radar` (watchtowers), `requisition`, `colonize`, `aaintel` (anti-air inferred from hits on our aircraft),
`mexporc` (the turret by every mex and link: the "porc tax" of `zkgbai-runs/vs-human/briefings/B4`).
`ZKGBAI_MUTE` (or the AI option `mute`) silences modules: `eco`, `fac`, `raider`, `squad`, `scouts`, ... (the
list is in `AIOptions.lua`) keep thinking but every order they give a unit is dropped, so an operator can do
that module's job itself; `ZKGBAI_MUTE=raider,squad` against the plain build shows what the army modules are
worth.
With `enable=firesquads` (or `ZKGBAI_ENABLE=firesquads`), ordinary ground assault squads
automatically adopt fire-squad tactics when anti-unit skirmishers
supply at least 55% of their metal (45% to remain in that mode). The longest range supported by
that share defines the cores; other units screen them in a paced formation. Firewalker joins
jumpbot assault squads; siege artillery such as Sling and Impaler cannot be cores. Against a
local enemy force with at least 30% of its value able to contest that range (20% while evading),
the whole squad evades unless the mechanics predictor approves a ten-second committed a-move.
The prediction uses the squad alone, expects at least 15% more enemy metal lost than friendly
metal. A commitment suppresses individual retreats until its attack window ends or its target
is gone. Outside that window, collective health below 60% or a core below 40% sends the whole
formation for repairs; it returns above 85% collective health and 80% for every core.
Fire squads are disabled by default; without the switch, Firewalker keeps its artillery role.
`disable=firesquads` restores ordinary assault conduct. Fire-squad predictions run independently
of the experimental `fightsim` switch for ordinary danger checks. The `squads` operator response
labels active formations `fireteam` and exposes their tactic, cores, range and focus; mode changes appear as `firesquad` journal
records. The dashboard, map labels, HUD and in-game operator widget show the fireteam label
and current tactic. Doctrine taskforces, aircraft, operator orders and QRF assignments keep their own conduct.

Allied instances of the AI each play their own team. With `ZKGBAI_HIVEMIND=on` (or the AI option `hivemind`
on every one of them) they merge into one instead: the lowest team leads and the others hand it their units.
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
`ZKGBAI?test=hunt` against `ZKGBAI?test=snipers` (or `test=lances`) stages assassination strikes: the defender
posts a Phantom (a Lance) 1400 elmos out every 2.5 minutes, every second one with a Reaver behind it, and the
attacker gets raiders every 2 minutes; the strikes (`strikes:` lines, scored by `tools/arena/strikes.py`) should
kill the snipers. `disable=strikes` switches them off; `strike_margin` sets the gate (0 = break-even on a sniper
worth 1.5x its metal, negative = every wave goes).
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
| `/` | The dashboard: the map (terrain, metal spots and owners, our units by role, enemies fading with time since seen, squads and their targets, places to defend, threat and value overlays, and the terrain's segmentation by region or by who holds it with its chokepoints), economy and army charts, the event log, an inspector, and a console for every tool. Select units (click, shift-drag) and right-click the map to order them, send a squad, set a defense point or a construction task. Scrub the timeline back over the game, or open a journal (from disk or the AI's journal list) to replay it. |
| `/mcp` | MCP over Streamable HTTP: `claude mcp add --transport http zkgbai http://127.0.0.1:7460/mcp` gives an agent the tools. |
| `/mcpl` | MCPL (MCP Live) over a WebSocket, for agent hosts that speak it (connectome-host): the same tools, plus the feed pushed to the host as it happens (`push/event`), the HUD injected into the agent's context before each of its inferences (`context/beforeInference`), and the agent's turns recorded in the journal (`inference/lifecycle` → `inference` records). Feature sets `zkgbai.read`, `zkgbai.act`, `zkgbai.hud`, `zkgbai.events`. `tools/operator/mcpl-smoke.mjs` exercises it. |
| `/rpc` | Plain JSON-RPC 2.0: the method is a tool's name, the params its arguments. |
| `/call/<tool>` | `curl 'localhost:7460/call/enemies?kind=porc&limit=5'`, or POST the arguments as JSON. |
| `/tools` | Every tool with its description and input schema. |
| `/events` | The event stream as server-sent events (`?kinds=log,kill&since=<seq>`). |
| `/snapshot`, `/journal`, `/journals`, `/health` | The latest positions (served even while the game is paused), this run's journal, the journal directory. |

Tools that read: `status`, `economy`, `workers`, `tasks`, `factories`, `military`, `squads`,
`units`, `unit`, `enemies`, `threat_at`, `threat_map`, `spots`, `map_info`, `terrain`, `terrain_map`
(the segmentation of a movement tier: regions per cell, who holds them, chokepoints), `events`,
`defs`, `snapshot`, `overrides`, `profile`, `modules` (each module, what it decides, the orders it
issued and had dropped). Tools that act: `say`, `mark`, `note` (into the journal), `feature` (switch
a dev feature off or on mid-game), `mute` (silence a module: it keeps its bookkeeping, its orders to
units are dropped, so the operator can do its job; `eco`, `fac`, `raider`, `squad`, ... or one handler
like `eco.unitIdle`), `hud` (which sections the HUD shows), `feed` (which record kinds are pushed to
an MCPL host, how they are batched, how often a situation report goes), `fac_queue` (production
orders the factories serve before their own choice), `requisition` (the next factory), `build` (a
prioritized construction task), `issue` (value a construction job so its crew holds), `defend` (a
place the army defends as if attacked there), `squad_target` (override a squad's target for a
while), `take` / `release` / `order` (hold units: the engine wrapper drops the AI's own orders to
them, `springai::reserved`, until they are released; `order cmd=field_fac` has a commander copy a
factory's build list, Zero-K's field factory, after which `order cmd=build unit=tankcon x z` builds
the constructor beside it). Policies rather than orders: `squad_form` (a squad of any armed units,
raider or assault), `squad_intent` (hold a place, raid the enemy's economy around one, escort a unit,
attack a place and hand the squad back; with a `courage` that divides the threat the squad
perceives and the health level its units retreat at), `squad_disband`, `dial name=greed` (the
economy's expansion horizon: perceived threat at jobs and workers divided by it), `porc_zone`
(with `mexporc` off, circles where turrets by mexes and links are still allowed), and the levers:
`levers` lists the settings an operator may turn while the AI plays (greed, build power, the army's
line, the home defense's budget and tactics) with their ranges and current values, `lever name=...
value=...` (or `value=auto`) turns one. The dashboard's Levers card and the LuaUI widget's "levers"
view draw them as sliders, choices and switches from that list. The HUD can carry
pinned reads (`hud pin=enemies args={...}`) and the `board`, the map as a PNG (`GET /board` shows it;
MCPL hosts get it as an image block). Calls run on the AI's thread at its next frame, so they are
answered while the game runs.

**Piloting.** `tools/arena/pilot.sh -m "Agalta v1.0" -f factorytank -D mexporc` starts a real-time
match of the operator-served build against an unsteered copy of itself and prints the pilot's
endpoints (`PILOT url=... mcpl=ws://127.0.0.1:7460/mcpl out=...`); the replay stays in `out/wd/demos/`
for `tools/replay/capture.sh`. The `zkchar` recipe `zaku` puts a connectome-host agent on that
endpoint.

A journal is JSON lines, `{"seq":..,"f":frame,"k":kind,...}`: a `run` header (build, map, teams,
options, seed), the `map`, then `log` lines (each AI log line, `mod` its module), `unit+`/`unit-`,
`kill`, `enemy+`, `given`, `say`/`chat`, `op` (each operator action with its result), `note`,
`inference` (an MCPL host's agent turns: phase, id, usage), `alert` (an energy stall, metal floating, a reserved
unit under fire), `incursions` (the interception statistics per minute), `terrain` (the strategic analysis, once),
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
of the terrain analysis. GL4 shaders (Zero-K's `LuaShader`) tint the ground: in screen space over
the map's deferred depth buffer when `AllowDeferredMapRendering` is on (exact on the terrain), else
on a mesh draped over it (`/luaui zkop render mesh` forces that). They show one of its threat or
value maps, smooth with contour lines, and the segmentation of a movement tier:
regions by id, or by who holds them (ours blue, the enemy's red, sheltered ones hatched). Its panel
is a Chili window: toggles, the map and terrain pickers, the AI in focus, and its log. Select the
AI's units (as a spectator, or an AI ally's units) and right-click to steer them: move, attack-move
with ctrl, attack or guard a unit, queue with shift; alt sends their whole squad, ctrl+alt makes a
place to defend. `/luaui zkop` takes `focus <team>`, `layer <name>`, `terrain <off|regions|holders>
[tier]`, `render <auto|screen|mesh>`, `release`, `build <unitdef>` and `defend` (at the mouse), `fac <unitdef> [n]`, `say`,
`note`, `feature <name> on|off`. Without GL4 the value map is drawn as quads, without Chili the
panel by hand. `tools/arena/match.sh` installs extra widgets for a match
from `ARENA_WIDGETS`.

In-game commands
----------------

Type these in chat in a game with the AI:

- `kgbdebug` — toggle debug mode (internal errors are also said in chat) and print economy figures.
- `kgbprofile` — write per-module CPU time to the infolog (also logged when the game ends).
  The [density profiling harness](tools/perf/README.md) reproduces AA, QRF and precision
  workloads. QRF and precision log background worker costs separately.
  `ZKGBAI_PRECISION_REPORT=full` enables the expensive exhaustive precision diagnostic;
  normal games log mission counters without running those searches.
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
