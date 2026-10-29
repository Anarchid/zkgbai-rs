# Airstrike planning

The airplane plant buys deficits for viable primary missions observed in the
last 90 seconds. Completed aircraft (including those rearming or on sorties),
nanoframes and unstarted factory orders count. No primary demand means no combat
production. The existing constructor curve, Owl and firefighter squad remain;
when known air coverage closes every available mission, the plant also pauses
those automatic orders. Explicit operator production still takes precedence.
Idle plants reconsider every two seconds, so they resume without an idle event.

Primary missions:

- Groups of at least three medium ground units: Phoenix in clear air, Likho with
  light AA. Package size scales with the group's health.
- Artillery, commanders and heavy units: Ravens in clear/very light AA, Likhos
  with more AA. Raven count is `ceil(1.2 * HP / bomb damage) + 1`; damage comes
  from the installed game definition.
- At least five nearby wind generators: Phoenix, with an approach clear of
  known interceptors.
- Aircraft on LOS/radar inbound to our territory: one Magpie and at least two
  Raptors. Unidentified radar contacts can count when clearly above terrain.
- Statics worth at least 1000 metal: Odin thermite; Singularities get a bonus.
  A route of at least 3000 elmos, or any Singularity mission, also requires one
  dedicated Sparrow per Odin. They gather within 75% of the live jammer radius,
  then the Sparrow guards its Odin through approach and egress. The economy's
  scout is not borrowed, replaced or sent to detonate.
- Raiders on our territory: Phoenix or Magpies.

Secondary missions use existing assets and cannot cause purchases: damaged
heavies reachable within 12 seconds by Ravens, ordinary statics for Ravens,
and an Odin shield cluster over friendly ground troops in an ongoing fight.
Available primary missions reserve their aircraft types first. Overlapping jobs
cannot launch duplicate sorties against the same group. There are at most four
simultaneous sorties and 24 aircraft in a package; oversized jobs are rejected,
not silently reduced. Mobile fixes must be live and at most five seconds old.

Approach routes use the shared air threat grid, inferred unseen AA and a local
interceptor envelope. The controller estimates exposure for ingress, egress and
six seconds at the target, requiring damage below 55% of aircraft health. This
is a conservative, **uncalibrated heuristic**, not a combat simulation or a
survival guarantee. Sparrow jamming never discounts LOS AA or known fighters;
the escort must pass its own survival check. New AA invalidates procurement
memory and causes active sorties to abort or update their approaches. Spent or
damaged aircraft return via threat-aware routes and rearm. Odin shield drops use
`AIR_MANUALFIRE` (38571); normal attack releases thermite.

Inspect `military.airstrikes` or the snapshot's `airstrikes` for mission IDs,
`available` / `in_progress` states, positions, packages, planned routes, assigned
members, exposure, production and grounded state. Active missions suppress
candidates that overlap their targets. Snapshot journals preserve these states
for replay. The browser dashboard and in-game operator widget show available
missions as diamonds with broken routes and active missions as double rings
with solid routes; the dashboard card and in-game Intel list include aircraft
requirements. The operator HUD and map board expose the same distinction. `disable=airstrikes` disables the policy.

## Verification

`cargo test -p zkgbai --lib --offline` covers target classes, freshness, inbound
motion, AA-dependent packages, casualty margins, exposure rejection, production
reservations and deep-strike escorts.

Staged headless games exercise real factory production and flight commands:

```
cargo build -p zkgbai --offline
tools/arena/match.sh -t 180 -f 10800 -s 20 \
  "ZKGBAI:airstrike@$PWD/target/debug/libzkgbai.so?test=airstrike-wind" \
  "ZKGBAI:airstrike@$PWD/target/debug/libzkgbai.so?test=airstrike-farm"
tools/arena/match.sh -t 180 -f 14400 -s 20 \
  "ZKGBAI:airstrike@$PWD/target/debug/libzkgbai.so?test=airstrike-odin" \
  "ZKGBAI:airstrike@$PWD/target/debug/libzkgbai.so?test=airstrike-singu"
```

Use `airstrike-aa` as the defender to surround the Singularity with four
Artemises; the attacker should report `grounded=true`, no purchase and no sortie.
These are controller smoke tests, not full-match effectiveness measurements.

On Comet Catcher Redux v3.1, the staged wind test destroyed 10 of 12 generators
in one Phoenix pass. The thermite test built an Odin and Sparrow and destroyed
the Singularity with both aircraft surviving. The Artemis test stayed grounded
with zero aircraft built and zero sorties. All three completed without AI
panics or reported AI errors. Interception, shield deployment and AA thresholds
still need broader live-match calibration.
