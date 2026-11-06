# Movement recovery

Factory exits reserve 320 elmos in front and 64 extra elmos on either side.
Construction also leaves 64 elmos around existing and queued factories; new
ground factories keep that distance from existing static units. Metal spots
retain the existing exemption from the forward exit lane.

`recovery` samples owned ground units once per second. It requires an actual
move/fight order, an out-of-range worker task, or an uncleared factory output.
Holds, combat firing on fight orders, paralysis, substantial slow, airborne
units, existing transports/teleports/throws, operator reservations and committed
tactical controllers are excluded. Equivalent reorders and small oscillations
do not erase evidence. New ground explored, progress toward the goal, or time without movement intent
resets it. Confirmed failures retain their destination when raw-move drops its
command, so an unavailable rescue method can be tried later.

After 20 eligible seconds (12 for workers or with a recent move-failed event),
one suspect per second gets a 16-elmo local occupancy search. The search expands live buildings,
mobile units and blocking features by the passenger's footprint, samples current
slope/depth, and distinguishes a static enclosure from temporary traffic.

Recovery tries a bounded walk out, then an owned mobile blocker or a short
corridor through owned Solars and wind generators. The corridor search keeps
terrain, features, allied buildings and other structures impassable. It proposes
at most three cheap buildings (150 metal total), whose combined removal opens
an exit; it does not require each removal to open the entire route individually.
A constructor must be in reclaim range or have a verified, locally reachable
approach into range without known ground threat. Energy checks budget the entire
remaining corridor, using current output (at least 2 E per Solar and 2.5 E per
wind), and preserve connectivity among surviving grid nodes, allowing alternate
grid paths. Recheck these conditions before each removal and during reclaim.
The constructor may be the trapped unit itself. No stuck fighter is scrapped.

The recovery state codec preserves the ordered remaining blockers, current
reclaim, worker lease and deadlines. Engine save/load activation belongs to the
repository-wide checkpoint integration. Disabling recovery cancels the mission on the next
update; it does not discard the saved history. Ownership changes cancel affected
clearance immediately. Active-mission snapshots predating the added mission
field are rejected by the strict schema rather than silently losing pending
removals.

Evacuation uses an available Lobster, an idle deployed Djinn, or a compatible
Charon/Hercules. Lobster checks every potential passenger and each translated
landing, declining launches involving allies or committed units. A Lamp must be
placeable within 180 elmos of the passenger (inside the gadget's 200-elmo capture
radius), with an explicit occupancy check because the engine's build-site query
can permit mobile occupants that the beacon gadget rejects. Air rescue requires clear AA samples along pickup and delivery paths,
a legal landing footprint, local space to walk away, and terrain connectivity
to the destination or home. These are checks against known threats, not proof
of absence of unseen AA.

If necessary, an existing gunship factory can build one suitable transport.
Demand expires, outstanding orders suppress duplicates, and a stranded group
must justify the transport's cost unless it is blocking factory output.

One mission runs at a time. Temporary command leases preserve the units' normal
roles and respect operator reservations/mutes. Failed methods back off; after
three minutes the controller can reconsider them as conditions change. Rescue
requires observed ground movement after landing, then resumes the destination.
Loading, flight and unloading
have separate timeouts. An emergency unload order survives final release of a
still-loaded transport; loaded aircraft are excluded from new drops/ferries.

Use `disable=recovery` to disable interventions. `recovery:` log lines record
the evidence, selected method and result. `drops`, `teleport`, `lobsters` and
`jumps` retain their respective feature gates.

## Validation

Unit tests cover persistent stalls, legitimate detours, suspended intent, local
traffic versus static/terrain enclosure, counterfactual blocker removal and
factory exit rotation. Run `cargo test -p zkgbai recovery` and the factory
clearance test (or the complete workspace suite).

`test=recovery` is an explicit cheat-enabled developer scenario, following the
repository's existing `test=` convention. Against NullAI on a sufficiently large
flat map, it creates five closed pens, 180 seconds apart, for Solar clearance,
Lobster, Djinn, Charon and Hercules. `recovery-probe:` reports each case after
140 seconds; a PASS requires the expected method to have completed and the cargo
to have travelled more than 256 elmos. Solar clearance may rescue the trapped
constructor first and thereby open the same exit for the fighter. This mode is inactive in normal games.

On 2026-11-04, the headless `test=recovery` scenario passed all five cases on
Comet Catcher Redux v3.1 with Recoil 2026.07.04 (27,000 frames, seed 714),
with zero reported panics/errors. Solar, Lobster, Djinn, Charon and Hercules
completed their dispatched missions in 18, 11, 17, 13 and 22 seconds respectively.
This verifies isolated mechanisms; contested, densely populated matches still
need observation to tune intervention thresholds and rescue throughput.

`test=recoverywind` exercises one, two and three wind generators in a narrow
exit, including a constructor initially outside reclaim range. Its three cases
use the normal recovery detector and require actual post-clearance movement.

On 2026-11-06, all three `recoverywind` cases passed on Comet Catcher Redux
v3.1 with Recoil 2026.07.04 (16,200 frames, seed 714). The one-, two-, and
three-turbine missions completed in 11, 21 and 33 seconds; their Reavers moved
959, 1,045 and 464 elmos. The existing five-case `recovery` suite also passed
on the same library (27,001 frames). Both runs reported zero panics/errors.
These isolated live checks validate clearance and evacuation mechanisms; they
are not a replay counterfactual of the reported Ravaged match.
