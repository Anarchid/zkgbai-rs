# Assassination waves

The controller accepts only Halberds, Dirtbags and Fleas. It flags commanders,
Phantoms, Lances, Firewalkers, Merlins and Tremors, and remembered Phantom firing
areas. It borrows eligible existing units from ordinary handlers, requests only
missing units from an existing factory, gathers the full wave, dives, searches,
focuses a revealed target, and returns. Halberds can take another fresh job after
returning; without one they rejoin normal assault squads. Operator-controlled
squads and QRF members are excluded from recruitment. Strike units have no
ordinary retreat handler while committed.

While Phantom evidence remains within the three-minute memory, an existing hover
factory also requisitions one Halberd every 30 seconds, topping up to 5–10 owned
Halberds even without a launchable job or during a strike cooldown. Existing
assault squads, strike members, nanoframes and unstarted requisitions all count.
These units join ordinary assault squads until a strike needs them. Active wave
shortfalls retain their immediate procurement priority.

Sizing uses live definitions: 5–10 Halberds; the targets' metal cost in Dirtbags
(25 for one 750-metal Phantom with 30-metal Dirtbags); `max(20, 15 * count)` Fleas.
There is one active wave, bounded to 128 members. Larger jobs are skipped rather
than sent with an undersized wave. Fleas require at most one known riot/unknown
screen within 600 elmos. A completed attempt gives the area a 90-second cooldown.

`Sweeps` estimates count from the maximum number of impacts within a reload,
allowing one second of flight-time variation. This is an approximate lower bound:
misses are absent and nearby firing areas may merge. Seen and inferred counts
merge by maximum, rather than adding the same Phantoms twice. A fix is fresh for
20 seconds; production can remember it for three minutes. Confirmed kills reduce
estimates; a contact disappearing does not count as a kill.

The mechanics predictor and its fitted tables come from `fightsim-mech`, commit
`75fc518`, with its Python parity fixtures. `FateMode::Assassinate` adds movement
and fire directed at a particular snapshot target. A bounded worker evaluates the
wave, target and known escorts along the approach. Answers expire after three
seconds and must match the current membership and fix. The launch gate is a 55%
modeled target-death probability, regardless of whether the wave survives.

This focused mode is new and has not been calibrated on assassination matches.
The inherited model is one-dimensional, lacks shields and water, and does not
model Halberd approach armor or locating a cloaked target. Terrain reachability,
recent fixes and the bounded search timeout are separate runtime checks. The
calibrated probability is a decision heuristic, not a measured assassination
success rate.

Two fresh inferred Phantoms also enable Infernos from an existing silo, with the
usual range, shield, friendly-fire and already-burning checks. Badgers enable
Fire Towards Enemies while Phantoms are implied. In-range Firewalkers receive a
ground target for approximately three reloads, then a cooldown, with freshness
and friendly-fire checks throughout.

Inspect `military.strikes` for jobs, estimated counts, ages, cooldowns and wave
membership. The `strikes` feature switch controls the policy.

## Checks

`cargo test -p zkgbai --lib --offline` covers cadence, wave sizing, target-focused
simulation and the inherited model's Python parity fixtures.

The headless scenarios use an explicit approximate fix, an actually cloaked
Phantom, and actual engine movement and firing. Run `tools/arena/match.sh` with
`test=assassin-halberd`, `assassin-dirtbag`, or `assassin-flea` against
`test=assassin-target`; `assassin-support` against `assassin-target2` exercises
Badger, Firewalker and Inferno support. These isolate controller behavior; they
do not measure noisy bearing accuracy, escorted dives or economic performance.
