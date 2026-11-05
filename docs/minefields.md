# Mine ambushes

Cloak and shield factories share a demand target based on observed enemy ground
combat squads. Five identified combat units within 300 elmos of a common member
unlock one mine; every additional five in that squad unlock another. Distinct
squads add together, capped at eight mines. The grouping separates movement tiers
and does not join long chains of scattered units. Existing mines, nanoframes and
accepted production orders all count. Conquest no longer creates a separate
unconditional quota. The older random bomb choices are removed, so neither
factory can bypass observed-clump demand. Excess mines are retained when demand falls.

Squad membership is remembered for 90 seconds after the last qualifying sighting.
Repeated sightings replace overlapping membership; confirmed deaths remove it.
This is observation memory, not a permanent high-water mark.

Every three seconds, deployment considers actual friendly buildings and committed
construction destinations. It follows each observed squad's terrain route to those assets,
using the attacker's movement tier, and samples ambushes before the attacker's
firing position and outside the asset's blast clearance. Incursion predictions
and frontline exposure increase an asset's priority. These are heuristic approach
predictions, not knowledge of an opponent's orders. Gate/conquest fields remain
lower-valued fallback opportunities; owned frontier mexes are always considered.

Both Imp stun and Snitch damage spare friendly buildings, nanoframes and builders,
including allies. An Imp additionally needs existing fire support or a damaging
ground unit close enough by its terrain route to respond within four seconds.
Assignments discount overlapping protection and travel. A still-valid assignment
gets a 15-second commitment and requires a 40% score improvement to move; vanished
or unsafe jobs can be replaced immediately. Small approach shifts refresh a job's
value without moving its exact destination. Resampling or a lost clump sighting
does not erase a safe commitment; afterward a surviving frontier asset keeps
a modest guard value. Mines hold when enemies are near.

New mines are adopted immediately, with up to three seconds for a queued factory
exit. Travel uses avoid-enemy waypoints. A mine that moves less than 48 elmos over
12 seconds is retried elsewhere; a two-minute assignment deadline also catches
circling. Failed destinations are excluded for 30 seconds. Without an eligible
ambush, a mine seeks a safe staging point outside production buildings. Logs
report demand changes, assignments, staging, stalls and arrival times.

The existing bombcharge evaluator remains. Charge orders are bounded to the
judged position, with building/builder clearance checked along the straight
charge corridor, so an attack order cannot chase a target behind the bait.

## Checkpoints and policy

In the checkpoint-enabled working tree, squad membership, observation times,
production reservations, exact sites, deployment phases, commitments, progress
and retry deadlines are all persisted with `#[checkpoint::state]`. This schema
rejects older payloads missing those fields. Existing state is preserved under
unchanged policy; changing observations rescans jobs through the ordinary planner.
There is no new runtime feature toggle or asynchronous work queue.

The currently shipped older main/wip branches do not yet implement checkpoints.
Their mine-only port omits checkpoint annotations and the codec-specific test;
no save/load capability is advertised. The compatibility delta is kept with the
mine shipping records for the separate checkpoint integration.
