# Airstrike planning

The airplane plant buys deficits for viable primary missions observed in the
last 90 seconds. Completed aircraft (including those rearming or on sorties),
nanoframes and unstarted factory orders count. No primary demand means no combat
production. The existing constructor curve, Owl and firefighter squad remain;
when known air coverage closes every available mission, the plant also pauses
those automatic orders. Explicit operator production still takes precedence.
Idle plants reconsider every two seconds, so they resume without an idle event.

Primary missions:

- Groups of at least three medium ground units: Phoenix or existing Likhos in
  clear air, Likhos where their own survival budget permits. Passes are valued by `metal * min(predicted
  damage / current HP, 1)` per victim rather than requiring the whole clump to
  die. Likho uses the installed weapon's blast radius and falloff, a 20% damage
  reserve, and the best member to aim at; center-distance sampling conservatively
  ignores collision-volume extent. Phoenix retains a heuristic 1800 total HP
  damage budget spread across the group. Every additional Likho/Phoenix must
  add at least 250/150 metal of damage value. Smaller worthwhile packages remain
  eligible, so one ready Likho need not wait for a lethal fleet. These are damage
  estimates, not predictions of kills; shields, movement, and napalm placement
  can change realized damage. Candidate ranking and combined-wave allocation
  use damage value for groups; operator snapshots expose `package_damage_values`
  alongside the unchanged full target `value`.
- Priority strikes target mobile units costing at least 600 metal with at most
  3 current HP per metal (including artillery and commanders meeting those
  thresholds). Wounded heavies can qualify as their health drops; fresh
  sightings of healed targets remove obsolete priority demand. Ravens or existing Likhos can strike in clear/very light AA;
  Likhos can also strike through AA when their own survival estimate permits. Within this category, metal removed
  per current HP determines preference. Raven count is
  `ceil(1.2 * HP / bomb damage) + 1`; damage comes from the installed definition.
  Procurement compares missing metal cost across alternatives, counting aircraft
  already owned or ordered. A ready Likho is usable even where Ravens would be
  cheaper to buy.
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
cheap artillery such as Fencers and still-tough targets such as healthy Cyclops for a sufficient
existing Raven/Likho fleet, and an Odin shield cluster over friendly ground troops in an ongoing fight.
Ordinary dispatch takes launchable primary missions before secondary ones.
Within each tier, mission score is discounted by actual route length
(`score / (1 + length / 1200)`); defensive interception keeps its urgency.
Procurement uses the same distance discount. Unfillable primary
packages do not reserve every aircraft of a type; spare aircraft may take
secondary opportunities while the remaining package is built. Overlapping jobs
cannot launch duplicate sorties against the same group. There are at most four
ordinary simultaneous sorties and 24 aircraft in a package; oversized jobs are rejected,
not silently reduced. Mobile fixes must be live and at most five seconds old.

Every 30 seconds, before ordinary dispatch, a combined-strike fallback checks
legacy flock readiness: at least eight ready bombers, with no idle bomber still
rearming or repairing. Operator-held planes and active sorties are excluded;
fighters and Sparrows do not count toward eight. Healthy planes within 1200
elmos of home can join from friendly territory even while circling back to the
rally; their actual approach is checked before committing. At least eight
bombers must have viable assignments, so a token sortie is left to ordinary
dispatch. It assigns the available fleet
in one update using the old distance/AA/value ranking, with a modest preference
for Ravens on individuals, Phoenixes on groups/wind, Likhos on groups or valuable
units, and Odins on expensive statics. Partial Raven/Likho wings can combine to
meet the kill margin. Only assigned aircraft launch; surplus planes are not
spent on redundant bombs. This fallback never buys aircraft.

Wave targets stay within 1800 elmos of their first target. Routes must not exceed
1.5 times straight-line distance plus 384 elmos, or traverse more than 1800
continuous elmos outside friendly territory. A moving target more than 600 elmos
from its original fix ends that wave assignment. Waves bypass the ordinary
four-sortie cap, but retain per-package size, payload, freshness, friendly-fire,
escort and survival checks. Overlapping victims are reserved across the wave.

Approach routes use the shared air threat grid, inferred unseen AA and a local
interceptor envelope. Mobile AA adds a tapered band outside its existing hard
coverage, as wide as three seconds of its movement speed; it inherits the normal
sighting-age threat decay. Both the ordinary air pathfinder and strike planner
see this shared buffer. An explicit return corridor retraces ingress to home;
both legs are sampled every 32 elmos, including between waypoints, before launch.
Active sorties recheck ingress and return corridors against current threats.
Corridor selection retains the shared strategic threat map. Survival uses a
separate estimate: known ground AA weapon damage against plane armor, projectile
and salvo counts, and reload time. Enemy HP and splash weighting do not count
as DPS. Each encountered weapon contributes a ready volley on each leg, plus
sustained fire integrated along ingress and egress. A 1500-elmo allowance for
release/turning is also flown at the individual aircraft's speed. The estimate
must stay below 55% of that aircraft's health (current health on active flights).
Likho has no additional fixed local-AA ceiling. All members, including the
350-HP Sparrow, must qualify; deep Odin missions can still be escort-limited.
Unseen AA retains its conservative inferred power estimate until identified.
The forecast remains a heuristic, not a combat simulation or survival guarantee.
`aa` now reports incoming DPS divided by 500, `exposure` integrates that rate,
and candidate snapshots also expose `incoming_dps` and `ready_volley_damage`. Sparrow jamming never discounts LOS AA or known fighters;
the escort must pass its own survival check. New AA invalidates procurement
memory and causes active sorties to abort or update their approaches. Spent or
damaged aircraft return via threat-aware routes and rearm. Odin shield drops use
`AIR_MANUALFIRE` (38571); normal attack releases thermite.

Inspect `military.airstrikes` or the snapshot's `airstrikes` for mission IDs,
`available` / `in_progress` states, positions, packages, planned routes, assigned
members, exposure, production and grounded state. `return_route` records the
escape corridor; active combined strikes carry a shared `wave` frame ID, and
`combined_strikes` reports check and launch counters. Only primary candidates are
shown as available; all active sorties remain visible, including secondary
missions. This display filter does not affect secondary dispatch. Active missions suppress
candidates that overlap their targets. Snapshot journals preserve these states
for replay. The browser dashboard and in-game operator widget show available
missions as diamonds with broken routes and active missions as double rings
with solid routes; the dashboard card and in-game Intel list include aircraft
requirements. `fleet` distinguishes ready, in-progress, rearming, repairing,
returning and operator-held aircraft; `utilization` accumulates aircraft-seconds
per state, aircraft launches, observed spent payloads and abort reasons. Spent
payloads indicate releases, not confirmed hits or kills. Aircraft still gather
within 450 elmos of home and require 95% health before launch. The operator HUD and map board expose the same distinction. `disable=airstrikes` disables the policy.

## Verification

`cargo test -p zkgbai --lib --offline` covers target classes, freshness, inbound
motion, AA-dependent packages, casualty margins, exposure rejection, production
reservations and deep-strike escorts.

Staged headless games exercise real factory production and flight commands.
`airstrike-likho-fencer` (one Likho) versus `airstrike-ravagers` (three healthy
Ravagers) exercises launching a valuable partial-damage group strike.
`airstrike-likho-aa` versus `airstrike-ravagers-aa` adds an armed Razor behind
the pack, observed before launch, to exercise aircraft-specific survival.
`airstrike-wave` versus `airstrike-wave-targets` holds a nine-bomber mixed fleet
until the first 30-second wave check, against a Lance, Thugs, wind generators,
a fusion and radar near the attacker. Logs expose `combined wave=...`, assigned
packages, payload releases and target survival. Keep match output and Cargo
artifacts in a disk-backed `target/` directory, and run games sequentially.

Examples:

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
