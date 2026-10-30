# Assassination waves

The controller accepts Halberds, Dirtbags, Fleas and Scythes. It flags commanders,
Phantoms, Lances, Firewalkers, Merlins and Tremors, and remembered Phantom firing
areas. It borrows eligible existing units from ordinary handlers, requests only
missing units from an existing factory, gathers the full wave, dives, searches,
focuses a revealed target, and returns. Halberds can take another fresh job after
returning; without one they rejoin normal assault squads. Operator-controlled
squads and QRF members are excluded from recruitment. Strike units have no
ordinary retreat handler while committed.

Commanders never requisition strike units, regardless of tactic or paralysis.
They are opportunities for an already available full wave only; neither an early
sighting nor a waiting/returning wave can start commander-driven production.
Halberds and Dirtbags can use such opportunities without a stun; Fleas still
require one.

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
model Halberd approach armor, Scythe approach cloak or locating a cloaked target. Terrain reachability,
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

## Flea finishers and forward readiness

Fleas still attack glass cannons directly. Commanders require a currently visible,
paralyzed target, and other mobile heavies (the jaeger classification) become
Flea-only opportunities during paralysis. A Widow hunt/ambush warns an existing
Flea wave to muster before the stun lands. These transient jobs require a full
existing wave; they do not order new Fleas. Waiting or returning Flea waves may
switch to one, but committed dives keep their target. Any observed EMP qualifies;
the AI does not need to attribute the stun to a particular Widow.

The worker seeds observed paralysis into the mechanics model, with normal decay.
A finisher must be predicted to kill before the remaining stun expires, with a
response margin, and the live window is checked again when the answer arrives.
Recovery or lost LOS ends a stun-dependent dive. Heavy and commander weapons are
not assumed permanently disabled.

Waves stage toward the nearest friendly front, normally 250–900 elmos behind it,
and prefer a point 600–950 from the job where both constraints permit it. Threat
and terrain checks can retain the previous staging point when no safe forward
point exists. Fleas and Dirtbags screen in a tangent line up to 1600 elmos wide
while evidence is quiet. Fresh activity (45 seconds) or a Widow hunt brings them
into a compact muster before a call; launching still requires a fresh fix (20
seconds), all members gathered and a successful prediction. Bot dives advance as
a line, converging to focus a revealed target; Halberds retain pack pacing.

`assassin-widow` versus `assassin-heavy` or `assassin-commander` exercises a real
Widow EMP and an existing 20-Flea force through the normal selector.
`assassin-no-widow` versus `assassin-commander` checks the unstunned commander veto,
and versus `assassin-aa` exercises dedicated-AA target masks and the swarm attack.
`assassin-aa-guarded` adds a Lotus beside that same formation: it must not
produce an opportunistic strike job or dive. `assassin-aa-late-guard` adds the
Lotus after launch and must end that dive when the new threat is observed.

Defenseless ground targets, including dedicated AA formations and unarmed economy,
are Flea opportunities without a stun only when both the target area and the
approach have no known threats. Classification uses real weapon mount masks
against the Flea category, not an AA tooltip; flex-AA and ground weapons are
still dangerous. Even one escort, turret covering the route, unidentified nearby
contact or inferred Phantom excludes an opportunistic job. Friendly superiority
does not cancel enemy threat. Dedicated Phantom/artillery assassination rules
remain separate. Nearby soft targets share a value budget: an existing wave
can raid a cheap target, but production requires at least the whole wave's cost
in target value. Twenty Fleas sweep a soft formation, continuing to nearby
eligible targets after each kill. Enemy snapshot weapons use the same target
masks; escorts remain in the prediction and riot-cover checks.

Released Fleas and Dirtbags explicitly rejoin solo-raider roles. Fresh jobs sort
ahead of stale fixes, and waiting waves may switch to fresh activity rather than
holding on an old Phantom estimate. Operator status includes the job position,
age, staging point, gathered-member count and whether a prediction is pending.

Fleas and Dirtbags fire at will while screening, moving and diving. Halberds keep
hold fire for approach armor and explicitly attack the revealed victim in range.
A confirmed Phantom kill near an inferred fix also resolves that target before
unit-id acquisition, including kills by allied fire between strike ticks. The
wave returns after the last expected victim dies.

Scythes form dedicated artillery-hunting squads: three per estimated Phantom,
and three against a single Lance or another named glass cannon. A cloak factory
can fill the shortfall; ordinary Scythe raiders can be borrowed, while dedicated
scouts and operator forces keep their assignments. They enable cloak, hold fire
while approaching and circling the inferred firing area, then explicitly focus
the revealed target. Returning survivors rejoin normal raider squads. The same
kill/death-evidence rules apply. The predictor still checks escorts conservatively
without assuming a perfectly cloaked approach. `assassin-scythe` versus
`assassin-target`, and `assassin-scythe-lance` versus `assassin-lance`, exercise
these paths in the engine.

Opportunistic cluster budgets exclude guarded targets and unsafe connections.
The controller checks threat before procurement and launch, while mustering,
and throughout the dive. A newly observed escort ends the opportunistic dive;
subsequent targets in a formation must pass the same checks. Checks use remembered
weapons/ranges plus a clearance margin around the direct approach, and are
repeated from actual member positions during movement; unseen threats and engine
pathing detours cannot be ruled out in advance.
