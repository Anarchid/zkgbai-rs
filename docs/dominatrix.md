# Rover Dominatrices

The random Rover Badger slot now produces a Dominatrix. Badgers remain an
artillery response, starting at phase income 60 (previously above 20).

One or two available Dominatrices keep the existing shield-ball/assault behavior.
Three form a `dominatrix` taskforce, which accepts reinforcements up to ten cores.
A full team leaves spare singles and pairs in their old roles until another three
can form. Operator-controlled, retreating and home-defense units are not recruited.
An established team keeps its formation after casualties while reinforcements arrive.

Teams use the fire-line controller even if organic `firesquads` is disabled.
They kite and evade longer guns; they do not charge on a damage-only predictor's
approval, because that predictor does not price capture beams. Rippers can join
later, capped at `floor(cores / 3)`. Neither Rippers nor shields are prerequisites
for forming or fighting.

Above the existing projected density threshold of 2000 metal per square kiloelmo,
teams request up to one Aspis per core. Existing supports, queued orders and static
shield-to-Aspis morphs fill this requirement. Shields follow separate move orders:
the controller finds the farthest reachable point behind the actual firing line
that contains every core with a 90-elmo margin inside the shield. A dispersed team
uses separate coverage groups until its line reunites. Equipped shields constrain
advance speed; absent shields do not hold up the team. Excess escorts are released
when the core count falls.

# Retirement

A single capture worth at least 1000 metal permanently retires its Dominatrix.
A capture worth at least 3000 also earns dedicated area-cloak coverage. Captured
commanders retire with their captor regardless of price; they leave the worker
pool and hold fire so their innate income remains available.

Retirees move to reachable, unthreatened rear turrets farthest from known enemies
and the front. Existing static cloak coverage breaks ties between similarly rearward
places. Until a safe fortified location exists, they use a safe home/haven fallback.
Free Iris units prioritize retirement havens, and nearby VIPs share a field.
Dedicated cloak demand bypasses the ordinary density allowance and uses existing
factory production or the static-jammer morph route. Active static cloak coverage
also satisfies this requirement. Ordinary 1000–2999 captors use available coverage
without forcing a dedicated purchase.

Retirement and earned cloak entitlements survive the death of the prize and a
`disable=dominatrix` intervention. Disabling the doctrine releases its active
formations into their previous roles; it does not put retirees back into combat.
Capture ownership is checked at transfer and polled every 15 frames, covering
callbacks delivered before the game's `capture_controller` rules parameter changes.

# Checkpoint state

Retirement identities, destinations and cloak entitlements are declared with
`#[checkpoint::state]` in `Taskforces::dominatrices`. The state round-trip test
covers non-default retirement and entitlements, handle rebinding, missing-state
rejection and unchanged behavior across a doctrine switch. Command authority
belongs to the retirement controller; worker/combat recruitment and destructive
stuck-unit recovery cannot reclaim a retiree.

Regression tests also cover bare-team formation at three, the ten-core limit,
optional Ripper joins, density-gated shields, retirement thresholds, commander
ownership, shared cloak demand and field geometry. Full-bot save/load integration
is separate pending checkpoint work; its development suite additionally verifies
a full-bot checkpoint followed by the same capture observation. This feature
alone does not enable engine save/load on the release branches.
