# Assassination waves

The controller accepts Halberds, Dirtbags, Fleas, Darts and Scythes. It flags commanders,
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
Halberds and Dirtbags can use such opportunities without a stun; Fleas and Darts
require one.

While Phantom evidence remains within the three-minute memory, an existing hover
factory also requisitions one Halberd every 30 seconds, topping up to 5–10 owned
Halberds even without a launchable job or during a strike cooldown. Existing
assault squads, strike members, nanoframes and unstarted requisitions all count.
These units join ordinary assault squads until a strike needs them. Active wave
shortfalls retain their immediate procurement priority.

Sizing uses live definitions: 5–10 Halberds; the targets' metal cost in Dirtbags
(25 for one 750-metal Phantom with 30-metal Dirtbags); `max(20, 15 * count)` Fleas; 20 Darts per estimated Phantom (20 for other targets).
The 800-metal Dart wave may exceed one Phantom's metal cost: killing it justifies
that expense. Darts borrow ordinary raiders or requisition from an existing Rover
factory, fire at will, and return to solo-raider duty after a job.
There is one active wave, bounded to 128 members. Larger jobs are skipped rather
than sent with an undersized wave. Fleas and Darts require at most one known riot/unknown
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
`test=assassin-halberd`, `assassin-dirtbag`, `assassin-flea`, or `assassin-dart` against
`test=assassin-target`; `assassin-support` against `assassin-target2` exercises
Badger, Firewalker and Inferno support. These isolate controller behavior; they
do not measure noisy bearing accuracy, escorted dives or economic performance.

## Wounded valuable targets

`strikefragile` is on by default; `disable=strikefragile` or the operator toggle
turns this additional target class off. A visible non-riot ground unit worth at
least 600 metal qualifies when its current HP / metal is at most half the full-health
Lance's HP / metal. Both the target cost and Lance reference come from live unit
definitions. The normal assassination predictor still decides whether to dive;
qualifying wounded heavies do not require paralysis. All five tactics may recruit
for these jobs. Commanders keep their existing rules and never drive procurement.

The controller rechecks eligibility before procurement and launch, and during the
dive. Repair above the threshold, lost LOS or disabling the switch ends this
opportunity. Existing named artillery jobs and stun-dependent jobs retain their
own eligibility rules. The operator's strike status marks these jobs as `fragile`.

## Light-swarm finishers and forward readiness

Fleas and Darts attack glass cannons directly. Commanders require a currently visible,
paralyzed target, and other mobile heavies (the jaeger classification) become
light-swarm opportunities during paralysis even if they do not meet the fragile threshold. A Widow hunt/ambush warns an existing
Flea or Dart wave to muster before the stun lands. These transient jobs require a full
existing wave; they do not order new Fleas or Darts. Waiting or returning Flea or Dart waves may
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
point exists. Phantom staging keeps the muster outside weapon range around the
estimated fix, even when only an inferred fix exists and the threat map has no
contact. Fleas, Darts and Dirtbags screen in a tangent line up to 1600 elmos wide
while evidence is quiet. Fresh activity (45 seconds) or a Widow hunt brings them
into a compact muster before a call; launching still requires a fresh fix (20
seconds), all members gathered and a successful prediction. Swarm dives advance as
a line, converging to focus a revealed target; Halberds retain pack pacing.

`assassin-widow` versus `assassin-heavy` or `assassin-commander` exercises a real
Widow EMP and an existing 20-Flea force through the normal selector.
`assassin-no-widow` versus `assassin-commander` checks the unstunned commander veto,
and versus `assassin-aa` exercises dedicated-AA target masks and the swarm attack.
`assassin-aa-guarded` adds a Lotus beside that same formation: it must not
produce an opportunistic strike job or dive. `assassin-aa-late-guard` adds the
Lotus after launch and must end that dive when the new threat is observed.

Defenseless ground targets, including dedicated AA formations and unarmed economy,
are Flea and Dart opportunities without a stun only when both the target area and the
approach have no known threats. Classification uses real weapon mount masks
against both scout categories, not an AA tooltip; flex-AA and ground weapons are
still dangerous. Even one escort, turret covering the route, unidentified nearby
contact or inferred Phantom excludes an opportunistic job. Friendly superiority
does not cancel enemy threat. Dedicated Phantom/artillery assassination rules
remain separate. Nearby soft targets share a value budget: an existing wave
can raid a cheap target, but production requires at least the whole wave's cost
in target value. Twenty Fleas or Darts sweep a soft formation, continuing to nearby
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

`assassin-dart-opportunity` offers 20 ordinary Darts to the selector. Against
`assassin-aa` it exercises recruitment, firing and release; against
`assassin-aa-guarded` it checks the same threat-free policy as Fleas.

The Phantom fixture stays at its seeded firing source, cloaked and firing normally.

## Existing Halberd assault squads

Ready, exclusively Halberd squads (the ordinary `Hover` kind, or pure `Assault`
squads) can take nearby assassination jobs without waiting for the dedicated
wave slot. They gather their actual surviving membership, ask the same target-death
predictor, then use the wide, hold-fire Halberd pack movement and search pattern.
Fresh inferred Phantom/artillery fixes qualify as well as seen targets. Escorts
are included in the prediction; Halberds do not inherit the raiders' hard riot veto.
These squad missions never requisition units and retain their normal squad identity.
QRF and operator orders take priority. Target death, expired evidence or a search
timeout releases the survivors to ordinary assault behavior. `squad_missions`
reports gathering, assessing and diving (also exposed through the legacy
`raider_missions` status key).

`test=assassin-halberd-squad` versus `test=assassin-raider-lance` exercises normal
Halberd squad formation and mission selection, with no preseeded strike wave.
`test=assassin-halberd-squad-phantom` versus `test=assassin-target` adds a seeded
approximate firing fix and a genuinely cloaked victim.
