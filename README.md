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
| `crates/zkgbai` | The AI. `src/ai.rs` owns all state and dispatches events to the modules: `graph` (metal-spot graph, territory, overdrive grid), `economy` (workers, tasks, factories), `military` (enemy tracking, threat maps, targeting, unit handlers), `kgbutil` (pathfinder, terrain checks), `los`. |
| `tools/arena` | Headless match harness: `match.sh` plays one game, `series.sh` plays a map pool in parallel and summarises win rates. |
| `tools/dll-smoke` | Loads a built library outside the engine and checks its FFI contract (exports, panic containment). |

Building
--------

Development build (needs Rust ≥ 1.85):

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

In-game commands
----------------

Type these in chat in a game with the AI:

- `kgbdebug` — toggle debug mode (internal errors are also said in chat) and print economy figures.
- `kgbprofile` — write per-module CPU time to the infolog (also logged when the game ends).
- `kgbgraph` — dump the metal-spot graph as ASCII art to `zkgbai_graph.txt`.
- `kgbmap threat|porc|riot` — dump a threat map as ASCII art.
- `kgbfac veh|hover|tank|cloak|shield|amph|spider` — choose the AI's first factory (1v1 only).

Lineage
-------

The original Java AI lives at https://github.com/Anarchid/zkgbai and a C++ port at
https://gitlab.com/talas777/ZKGBAI. This rewrite was ported from the C++ version and cross-checked
function by function against the Java original, fixing translation bugs found along the way.

License
-------

GNU General Public License, version 2 (see `LICENSE`).
