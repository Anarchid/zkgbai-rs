# Density and simulation-thread stalls

This pass caches the shared AA fallback within each handler batch, moves live
Mark QRF travel fields to a worker, and moves precision shadow casting to a
worker. Assignment, orders, callbacks, and immediate threat checks stay on the
simulation thread. Precision's exhaustive minute report is opt-in with
`ZKGBAI_PRECISION_REPORT=full`; normal mission counters remain enabled.

Each subsystem permits one outstanding calculation. While it runs, current
state replaces the need for a pending queue. QRF refreshes fields before expiry,
checks terrain identity, membership, movement parameters, source displacement,
and age, and retains orders while a replacement is pending. A pending plan does
not count as an unmatched draft. Precision validates per-gun revisions and treats
stale or missing shadows as exposed within weapon reach. Calibration probes
retain synchronous precision and QRF calculations. Worker panics yield failed
results; workers are joined on destruction before the AI library can unload.

Normal logs include separate worker job counts, failures, total time, and maximum
time. These are background costs, not simulation-thread profile durations.

## Measurements, 2026-10-29

Compared with `f790ff3` on FolsomDamDeluxeV4, engine 2026.07.04, speed 1:

- Obstructed AA fallback, 76 units, five repeats: **90.35 ms → 1.84 ms**
  mean handler time. Engine build-site calls: **76 → 1** per batch. All 76
  selections still targeted `(744, 4280)` in every repeat on both versions.
- QRF stress workload, 23 updates: **22.11 ms → 2.80 ms** mean main-thread
  time; **23.95 ms → 3.65 ms** maximum. The initial update waited for fields;
  all 22 subsequent updates had a plan. Worker batches averaged **17.78 ms**
  (26.29 ms maximum), so most of the benefit is moving the cost off the engine
  thread. Calculation and assignment were not eliminated.
- Precision cold update: **50.20 ms → 1.85 ms** main-thread time. Shadow
  casting still took **49.89 ms** on the worker.
- The naturally selected cheap-haven AA case stayed around 7 ms:
  **6.79 ms → 7.41 ms** for 76 units. It did not improve in this reconstruction;
  per-unit navigation is still performed, and the engine state differs slightly
  between reconstructed runs. Do not generalize the obstructed-site speedup to
  every AA update.

[Raw samples and source/fixture fingerprint](2026-10-29-density.json) retain both
costs and query counts. Full logs remain under `target/density-baseline2/run1`
and `target/density-final/run1` in the investigation worktree.

Validation: **220 workspace tests passed**, two pre-existing doctests ignored,
strict Clippy passed, release build succeeded. Five diagnostic engine runs
completed without panics/errors. A normal production-library Folsom self-play
smoke test reached **18,000 frames** with no panics/errors, active QRF orders and
drafting, and successful worker jobs. This does not establish late-game win-rate
parity or a guarantee that all frame stalls have been removed.

## Reproduction

`density.py` makes a **diagnostic-only source copy which bypasses normal AI
updates**. Never install that library for gameplay. The normal production build
is unaffected. `--ref` copies an exact Git revision; omitting it copies current
working sources. Output directories must be new.

Input is one complete `snap` JSON object from a journal, saved as a JSON file.
For this investigation it was frame 82800 of
`20261028-184337-FolsomDamDeluxeV4-t0.jsonl` (626 friendly units, 53 identified
enemies). The script restores unit types/positions and metal-spot ownership flags.
It does not restore health, orders, accumulated buildpower, every enemy, or all
engine state. Spawn collisions and engine randomness can produce small differences
between runs; this is a controlled reconstruction, not an exact match replay.

```sh
python3 tools/perf/density.py --snapshot /path/to/snapshot.json \
  --out target/density-base --ref f790ff31a9ccb140e010ae4eb3cbe8333772e582
python3 tools/perf/density.py --snapshot /path/to/snapshot.json \
  --out target/density-changed

cargo build --release --offline -p zkgbai \
  --manifest-path target/density-base/source/Cargo.toml --target-dir target/density-base/build
cargo build --release --offline -p zkgbai \
  --manifest-path target/density-changed/source/Cargo.toml --target-dir target/density-changed/build
```

Run each version separately, so they do not compete for CPU. For each directory
(substitute `density-changed` on the second run):

```sh
DENSITY_FIXTURE="$PWD/target/density-base/fixture.json" \
ZKGBAI_OPERATOR=off ZKGBAI_JOURNAL=off ZKGBAI_SEED=1 \
tools/arena/match.sh -m FolsomDamDeluxeV4 -s 1 -f 450 -t 120 -k \
  -o "$PWD/target/density-base/run" \
  "ZKGBAI:profile@$PWD/target/density-base/build/release/libzkgbai.so" \
  "ZKGBAI:profile@$PWD/target/density-base/build/release/libzkgbai.so"

python3 tools/perf/summarize.py \
  target/density-base/run/stdout.txt target/density-changed/run/stdout.txt
```

The harness first measures normal AA updates. It then finds the most expensive
actual worker build-site candidate in the reconstructed base and forces AA's
fallback there. That stress case counts engine queries and records selected
rally positions, so it can verify the batching optimization directly. QRF uses
24 actual Glaives in eight initial teams, actual map terrain and economy assets,
and 16 **synthetic** raid groups. Precision uses the identified enemy buildings.
Five AA repeats per size and 23 QRF updates are recorded. Precision calls below
0.1 ms are omitted; use its cold maximum, not the mean of logged calls.

The manifest fingerprints the uninstrumented Rust sources and input snapshot.
The two `Worker` timings report actual calculation cost separately from dispatch
and result application. Wall time in these benchmarks is not a statistical
estimate of late-game frame-time percentiles.

## Scope

The measured stalls are addressed without changing AA force composition or
replacing navigation. Multi-detachment AA and region-based strategic routing
remain separate behavior changes. Local pathfinding still contributes several
milliseconds to the cheap-haven AA workload. The QRF change covers live `mark`;
other experimental algorithms remain synchronous. Main-thread snapshot work,
precision mission searches, and other AI handlers can still cause stalls.
