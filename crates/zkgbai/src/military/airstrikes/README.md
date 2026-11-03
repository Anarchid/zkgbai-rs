# Airstrike planning

The airplane plant buys deficits for viable primary missions observed in the
last 90 seconds. Completed aircraft (including those rearming or on sorties),
nanoframes and unstarted factory orders count. No primary demand means no combat
production. The existing constructor curve, Owl and firefighter squad remain;
when known air coverage closes every available mission, the plant also pauses
those automatic orders. Explicit operator production still takes precedence.
Idle plants reconsider every two seconds, so they resume without an idle event.

Primary missions:

- Groups of at least two ground units of 150-2500 metal each, worth 600
  together (heavies such as Grizzly, Goliath and Crab included, striders left to
  single-target strikes; a pair of Grizzlies is one target that five Likhos burn
  both of, where each alone needs six for a sure kill): Phoenix or existing Likhos in
  clear air, Likhos where their own survival budget permits. Passes are valued by `metal * min(predicted
  damage / current HP, 1)` per victim rather than requiring the whole clump to
  die. Likho uses the installed weapon's blast radius and falloff, a 20% damage
  reserve, and the best member to aim at; center-distance sampling conservatively
  ignores collision-volume extent. Phoenix retains a heuristic 1800 total HP
  damage budget spread across the group. Every additional Likho/Phoenix must
  add at least 250/150 metal of damage value, on average over the aircraft added
  since the last worthwhile package (see Shields). Smaller worthwhile packages remain
  eligible, so one ready Likho need not wait for a lethal fleet. These are damage
  estimates, not predictions of kills; movement and napalm placement can change
  realized damage. Candidate ranking and combined-wave allocation
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
- Aircraft on LOS/radar inbound to our territory: at least two Raptors, with a
  Magpie to assist (its EMG slows the quarry) when one is ready; it is never
  bought or waited for. Unidentified radar contacts can count when clearly
  above terrain. An identified armed aircraft whose weapon reaches our or
  allied ground units or buildings (its range plus 200 elmos) is intercepted
  wherever it is, at the front or over an expansion (`disable=airmenace`:
  only in our territory or inbound to home).
  The package is a one-pass kill, one Raptor per 700 HP (six for a
  Revenant), and procurement buys toward it. When it cannot be filled and the
  quarry carries no anti-air we count (no AA or flex-AA weapon, not a
  fighter) with no AA or fighter within 1000 elmos of it, and little ground AA
  is on the way (risk <= 0.15), the Raptors that are ready go anyway, even
  one: they keep firing until it dies or flees, for up to 300 s instead of the
  usual 150 (`disable=raptorwear`). A ready Magpie joins them as in the full
  package: its EMG slows the quarry. Staged (`airstrike-raptors` vs
  `airstrike-revenants`, smoke runs in zkgbai-runs/gunships): two Raptors do
  ~28 damage/s to a hovering Revenant (strafing passes) and kill one in about
  three minutes. A Revenant is not harmless: its rocket salvos hit strafing
  Raptors 10 times in 142 shots (210 each) and killed one hurt Raptor, against
  ~19.8k damage dealt to Revenants. Before this, the 2-3 ready
  Raptors never went at a Revenant pair working over our ground for four
  minutes (Agalta, 2026-11-03: six asked for), and flew at Gnats instead.
- Statics worth at least 1000 metal, and every identified factory or strider
  hub: Odin thermite. Singularities get a bonus; a factory counts three times
  its cost (it is where the army comes from, and the rebuild costs time), four
  times when it is the only enemy factory we know of. One Odin's thermite
  (14000 damage) burns a 4000-HP factory down. `disable=odinfactories` leaves
  factories to Ravens' ordinary static strikes. Nanoframes in sight of mobile
  units costing at least 2000 metal (every strider) are thermite jobs too:
  they cannot move until done, and count twice the metal already in them. A
  frame seen finished calls its sortie off (`hatched`). Thermite is a water
  weapon, so submerged targets are Odin work as well as Ravens'; every other
  mission against them stays Raven-only.
  A route of at least 3000 elmos, or any Singularity mission, may take a ready
  Sparrow already within 75% of its jammer radius of an Odin. Escorts are
  optional and expendable: no rendezvous wait, no Sparrow survival veto, and
  losing the jammer does not abort Odin. Each escort guards one Odin until its
  payload is spent or its mission ends. After all combat-aircraft deficits are
  filled, the factory may maintain one spare escort for recent deep missions;
  this includes queued/building/active Sparrows. The economy's scout is not
  borrowed or sent to detonate.
- Raiders on our territory: Magpies, one per 200 raider health (a Flea takes
  one, a Glaive two), or Phoenixes, one per 1000, whichever puts less metal
  in the air; launch takes the cheapest package it can fill, so a Phoenix
  goes at a light raider only when no Magpie is ready. Raiders the
  firefighters are already on are left to them.
- Airpads (`unit_refuel_pad_handler`): a landed aircraft rearms (Likho 25 s,
  Odin 30 s), then sits until fully repaired at the pad's 2.5 buildpower, HP/s
  = 2.5 x max HP / (0.5 x cost): a Likho 5.9 (30% takes ~120 s), an Odin 17
  (one back at 43% sat 250 s on Agalta, 2026-11-03, flight and rearm
  included), a Raptor 18 (measured 18.8). Zero-K's `noammo` reads
  3 for that repair phase; the aircraft is armed again and any order lifts it
  off. A primary mission may take it at 70% health or more when its own
  survival check at current health passes (`disable=padlaunch`); secondary
  ones wait for 95%. The economy puts a caretaker (15 repair buildpower, three
  times the pad; HP/s = 15 x max HP / cost, a Raptor 55 as measured) beside
  each airpad with a standing area repair over it once combat aircraft are
  worth 1000 metal (`disable=padnano`), outside the one-per-10-income
  caretaker budget. Staged with the `zk-padlab.sdd` mutator
  (zkgbai-runs/padrepair, wounds team 0 Raptors to 35% every 120 s;
  `airstrike-raptors-pad` vs `airstrike-revenants`): 40 s on the pad with
  both off, 4-11 s with both on (`from_pad=1` launches at >= 70%).
- Heavy aircraft in sight (Nimbus, Krow, Odin, Revenant): Likhos, as many as
  each adds 250 metal of damage value, when the target can be reached within
  25 seconds. ZK aims the bomb at the quarry's straight-line lead point, so
  speed alone does not save it (combat logs: 57/64 hits on aircraft, Revenant
  4/4; `disable=likhorevenant` leaves Revenants to Raptors).
  The quarry and the heavy aircraft within 600 elmos of it (its flock: Nimbus
  fly in clumps and cover each other) are the target, not interceptors; other
  fighters and AA still gate the route (`disable=heavyflock` makes every flock
  mate an interceptor again, which vetoed every strike on a clump).
- Carpet bombing (`carpet`, priority 8): an unseen cloaker worth a priority
  strike (in practice the Phantom) that shot us within the last 20 seconds.
  Zero-K decloaks any unit that takes damage for three seconds, and Phoenix
  napalm keeps it burning (and so visible) for 8-12 s more. The engine starts a
  bomber's salvo half a salvo length before its aim point, so each Phoenix
  flies two lead-in waypoints on the shot's bearing and lays its ~390-elmo
  line along it, centred at 0.85 of the weapon's range from the victim (or on
  a bearing intersection when two impacts crossed). Three lines 110 elmos
  apart for a single bearing, two for an intersection; one fewer still flies.
  Very light AA only. Like the artillery hunt this is evidence, not a unit
  handle: it uses Phoenixes already built and never buys any. New impacts
  re-aim a carpet still more than 900 elmos out; a sniper seen near the fix
  turns the sortie into an attack on the unit. Phantoms sit in contested
  ground, so by default a carpet burns whoever is under it: each line logs the
  friendly ground units (ours and allied) and their metal under its strip at
  launch and again at release. `enable=carpetspare` instead requires every
  strip clear at launch and again within 1500 elmos of the aim (abort reason
  `friendly_fire`). Carpets never join combined waves or local retargeting.
  `disable=carpet` switches it off.

Shields (`shields.rs`, `disable=airshields` plans as if no enemy had one).
Every Zero-K shield stops every bomb our planes drop. Under the default
`shield_merge` mode a bubble takes a bomb only while its charge exceeds the
bomb's shield damage (it then loses that much); a weaker one lets it through
untouched. So an Aspis (3600) takes one Likho bomb (2000), four Raven bombs
(800), 90 napalm bombs (40, so a Phoenix pass drains 720) and in practice all
Odin thermite (2), while a Thug's bubble (1200) takes no Likho bomb at all. A
stopped bomb bursts on the bubble, so only bubbles at least as wide as the blast
protect the units beneath (napalm 160, implosion 96, Raven 16, thermite 20).
Charge is read for bubbles in sight and assumed full otherwise; shield links and
regeneration within a pass are ignored. Group values spend each package's bombs
on the bubbles over the aim first; the package rule takes the planes that only
feed a bubble together with the ones whose bombs then land, so a lone Likho is no
longer sent at an Aspis ball (two are: the first spends the Aspis), and a Phoenix
package must outlast the dome (six or more against one Aspis). The aim is the
member giving the best value per Likho flown once the bubbles over it are spent,
and groups rank by their best value per aircraft over all package sizes. A clump's metal is worth more under bubbles
(`disable=shieldworth` counts plain metal): ground fire is small shots every
bubble takes, while an implosion bomb goes through any bubble short of an Aspis,
so a bomb kill spares our army the shields as well as the hit points. Each
victim counts `(HP + its share of the bubbles over it) / HP` times its metal, at
most three: a Thug among Thugs 2.25. Ranked by plain metal, a Felon ball lost to
the Felon's own Raven priority strike, whose sortie then covered the Felon and
kept the Likhos off the clump; with this the clump strike ranks first. Single
target strikes add the bombs the bubbles over the target take: a Felon (its own
bubble takes two Raven bombs) under an Aspis (four more) needs ten Ravens or two
Likhos instead of four or one. Thermite and turret burns are not flown at targets
under a charged bubble.

Friendly fire (`disable=collateral` restores the old rule: no Likho or Phoenix
drop with any of our or allied ground units or buildings within 220 elmos of the
aim). A drop's implosion bombs and napalm are priced on our own and allied ground
units and buildings within reach (about 355 elmos, half a napalm line plus its
blast) like enemy victims, with no reserve: units by the damaged share of their
metal, buildings whole when destroyed and a quarter of the damaged share
otherwise, mexes four times their metal, Solars behind their armor. The napalm
line runs along the final route leg. Launch, combined-wave assignment and local
retargets require our losses to stay under half of what the strike removes (its
damage value for groups, the target's value otherwise). Holds are counted in
`collateral_holds`.

Ravens never take targets with a top speed of 90 elmos/s or more: their guided
bomb flies 135 elmos/s and homes weakly. Over 37k Raven bombs in our arena combat
logs it hit 86-88% of targets moving under 80 elmos/s at release, 73% at 80-100
and about 25% faster (Scorcher 32%, Kodachi 30%, Dagger 10%, Glaive 50%, Pyro 56%;
Rocko 94%, Ravager 91%, Fencer 88%). Such targets are Magpie and Likho work; this
covers retargets onto the nearest mobile enemy too. A Likho flies at a single
target (priority, opportunistic, static or retarget) only for at least 250 metal
per Likho, the worth a group strike asks of each: a 125-metal Quill is Raven work.

Secondary missions use existing assets and cannot cause purchases: damaged
heavies reachable within 12 seconds by Ravens, ordinary statics for Ravens,
cheap artillery such as Fencers and still-tough targets such as healthy Cyclops for a sufficient
existing Raven/Likho fleet, outposts and heavy defenses (below), and an Odin shield cluster over friendly
ground troops in an ongoing fight. The
drop point follows those troops (our mobile ground fighter nearest the enemy,
within 600 elmos) and the sortie is called off (`no_troops`) once none are left.

- Outpost burns (`outposts`, `disable=outposts`): clumps of at least three cheap
  buildings (each at most 300 metal: mexes, Solars, turbines, pylons, Lotuses;
  not factories, shields or anything submerged) within 260 elmos of a seed, mexes
  seeding first so an expansion is taken about its mex. Phoenixes only, in clear
  air, at most six. Each salvo is modelled as 18 bombs along the final approach
  (taken as the line from home), 390 elmos long and centred on the aim, plus
  the fire (about 150 damage per victim, once), at 80% and behind each
  building's armor (a Solar closes to a third, conservatively for the whole
  pass). A building is worth its strike value (a mex four times its metal) if
  destroyed and a quarter of its damaged share if not; the aim is a member or
  the centroid. Against a mex among four Solars one salvo leaves the mex at
  about 430 of 560 and two burn it down; Solars on the line need six, so in
  practice outposts cost the enemy mexes, turbines and light porc. Priority 1.5,
  between Raven statics and shield drops.
- Turret burns (`odinturrets`, `disable=odinturrets`): a spare Odin's thermite
  on an armed building of at least 400 metal that is not already a thermite
  target (Gauss 400, Stinger 450, Thresher 450, Sunlance 700), counted against
  its hit points behind its armor (a closed Gauss takes a third: one Odin still
  burns it), priority 2 like shield drops. Like thermite these take optional
  Sparrow escorts on deep routes, burn submerged targets, and are not flown at
  a target under a charged bubble.

Ordinary dispatch takes launchable primary missions before secondary ones.
Within each tier, mission score is discounted by actual route length
(`score / (1 + length / 1200)`); defensive interception keeps its urgency.
Procurement uses the same distance discount. Unfillable primary
packages do not reserve every aircraft of a type; spare aircraft may take
secondary opportunities while the remaining package is built. Overlapping jobs
cannot launch duplicate sorties against the same group. There are at most ten
ordinary simultaneous sorties, at most four of them secondary (so single-plane
static and opportunity sorties cannot crowd out a primary strike), and 24
aircraft in a package; oversized jobs are rejected,
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
An armed sortie may switch once to a known local opportunity even while its
original mission remains healthy. The available payload must fit the new job,
its estimated damage value must improve by more than 10%, and every loaded
member must arrive within six seconds, either at least one second sooner or
later but worth the wait: its value per second over the rest of the sortie
(the drop plus 30 s of flight home and rearm) at least 1.5 times the current
one's (a Lance a second past a mex qualifies). Arrival
estimates include route detours and target motion; candidates must lie within
800 elmos of every loaded member and retain the survival, friendly-fire and
target-reservation checks. The original target need not be nearby. The new
attack is ordered immediately and gets the existing eight-second terminal
commitment, preventing repeated shopping instead of releasing. Existing local
fallbacks for lost, destroyed or unsafe targets remain available. Retarget logs
distinguish `better_drop` from `fallback`.

Pooling (`live/pool.rs`, `disable=airpool`): a sortie retargets only its own
bombers, so lone Ravens sent at mexes past a Lance could never take it (one 800
bomb for 1000 HP). Once an update, the nearest free sorties (not yet switched,
not in a wave, of strike kinds, every member a loaded, healthy Raven or Likho
within 800 elmos of the target) whose bombers together fit a strike job give
them up to one pooled sortie, when the pooled drop's value is at least 1.25
times the drops they give up, the route survives and friendly fire is within
bounds. The first donor becomes the pooled sortie (switched, so it does not
switch again); the others dissolve and their targets are free. Logs read
`airstrikes: pool N sorties (M bombers) -> Kind:target`.

Magpie strafes (`disable=magpiestrafe`, secondary, priority 2): any identified
ground unit or building in sight that cannot shoot at aircraft, along a route
clear of AA. A Magpie's load (twelve EMG salvos of 48) answers 460 HP of a slow
target and 200 of a raider (90 elmos/s or faster); targets needing more than
four are left alone. A strafe takes Magpies only while at least three stay idle
at home, the firefighters' included, for the next raid; Interdict on raiders in
our territory remains a primary mission.
Corridor selection retains the shared strategic threat map. Survival uses a
separate estimate: known ground AA weapon damage against plane armor, projectile
and salvo counts, and reload time. Enemy HP and splash weighting do not count
as DPS. Each encountered weapon contributes a ready volley on each leg, plus
sustained fire integrated along ingress and egress. A 1500-elmo allowance for
release/turning is also flown at the individual aircraft's speed. The estimate
must stay below 55% of that aircraft's health (current health on active flights).
Likho has no additional fixed local-AA ceiling. Combat aircraft must qualify;
optional sacrificial Sparrows are added after these checks and keep escorting
through damage. Odin still needs its own safe ingress and return path.
Unseen AA retains its conservative inferred power estimate until identified.
The forecast remains a heuristic, not a combat simulation or survival guarantee.
`aa` now reports incoming DPS divided by 500, `exposure` integrates that rate,
and candidate snapshots also expose `incoming_dps` and `ready_volley_damage`. Sparrow jamming never discounts LOS AA or known fighters;
New AA invalidates procurement
memory and causes active sorties to abort or update their approaches. Spent or
damaged aircraft return via threat-aware routes and rearm.

Wide-turning airframes (`disable=odinroutes`; a real turn of 300 elmos or more,
in practice the Odin at about 400) get their own routes (`AirField::route_for`).
The cheapest way runs over 64-elmo cells in eight directions, so a slanting
flight staircases in 45-degree steps a cell or two apart, turns an Odin needs
`400 * tan(22.5)`, about 166 elmos of straight flight on each side, to make.
Its route is pulled straight instead: from each waypoint the next is the
farthest point of the cheapest way whose straight leg costs at most 5% (plus
two open cells) more than the way it replaces, so it cuts across open air but
not into anti-air the cheapest way went round, among legs whose turn fits in
half of each leg it joins; when none does, the safe leg overrunning least. A
sortie flying an Odin uses it for the plan, its in-flight re-routes (from the
lead's heading) and the egress; an Odin flying home alone, from its own
heading. On a test wall of anti-air with a gap, 11 cell waypoints became 3 with
39-degree turns.

Every route an aircraft is given skips the waypoints it cannot turn onto from
its current flight (`turns.rs`, `disable=airturns` restores the old routes).
The engine completes a move only within `max(turnRadius + 16, 128)` elmos, far
less than the circle a heavy airframe really turns on: full rudder yaws the
nose by `maxRudder` times speed per frame, `frontToSpeed`/`speedToFront` make
the velocity follow at a fixed fraction, so the radius is about
`2.6 / (maxRudder + maxElevator * max(maxBank, 0.4))` elmos regardless of
speed (the engine banks past `maxBank` in a hard turn). Measured in
`airstrike-turns`: Odin 401 (model 404), Likho 194, Phoenix 155, Raven 100,
fighters and Sparrow 70-90. A waypoint deeper inside that circle (which
starts about half a second ahead, the control lag) than the arrival radius is
orbited until the next order, and one behind the plane nearer than the turn's
diameter costs most of a loop; both are skipped, with half the arrival radius
as margin. Before this an Odin, re-routed home every ten
seconds with a first waypoint beside or behind it, circled over an enemy
factory until AA killed it. Odin shield drops use
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
requirements; `optional_escort` identifies optional Sparrow support for deep
thermite candidates. Active packages list only escorts actually assigned. `fleet` distinguishes ready, in-progress, rearming, repairing,
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

`airstrike-carpet` versus `airstrike-carpet-target` gives three Phoenixes,
a pad and two storages near a cloaked, hold-fire Phantom, and one synthetic
impact bearing at frame 180; `airstrike-carpet-live` versus
`airstrike-carpet-shooter` lets the Phantom shoot the storages itself (a
radar tower beside it lends sight). The defender logs `carpet-test: phantom
... hp=... cloaked=... on_fire=...` every second. On Comet Catcher Redux
v3.1 (2026-10-31) both variants launched within a second of the bearing and
killed the Phantom with one three-line pass, all Phoenixes returning; with
`disable=carpet` nothing launched and the Phantom stayed at full health.

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

`airstrike-likhos` (five Likhos) versus `airstrike-shieldball` (an Aspis and four
Thugs, held) checks Likho package sizing against a dome. `airstrike-phoenixes`
(four Phoenixes) versus `airstrike-outpost` (a mex, four Solars 90 out and two
turbines) and `airstrike-odins` (two Odins) versus `airstrike-turrets` (a Gauss
and a Stinger, held fire) exercise the outpost and turret burns; the defender
logs `airstrike-test: standing name:hp/max ...` every ten seconds.

`airstrike-odin` versus `airstrike-factory` adds a Shieldbot factory at the
same site: the thermite target, with `airstrike-track:` flight lines for the
Odin's way out and home (compare with `?disable=airturns`). `airstrike-turns`
(against any passive defender) flies one of each airplane out and back home
and logs its turn every five frames, for fits against the turn model.

Use `airstrike-aa` as the defender to surround the Singularity with four
Artemises; the attacker should report `grounded=true`, no purchase and no sortie.
These are controller smoke tests, not full-match effectiveness measurements.

On Comet Catcher Redux v3.1, the staged wind test destroyed 10 of 12 generators
in one Phoenix pass. The thermite test built an Odin and Sparrow and destroyed
the Singularity with both aircraft surviving. The Artemis test stayed grounded
with zero aircraft built and zero sorties. All three completed without AI
panics or reported AI errors. Interception, shield deployment and AA thresholds
still need broader live-match calibration.
