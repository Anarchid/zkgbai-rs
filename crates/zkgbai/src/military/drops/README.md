# Riot drops

Charons carry Reavers, Rippers, Redbacks, Maces or Scallops only. Each sortie
requires five compatible riots of one movement tier, five available transports,
and five living cargos actually aboard before departure. A ferrying Charon does
not count. Loading failure returns loaded troops home before restoring their
ordinary handlers. Transport losses after departure can still reduce the force.
Recruitment takes gathering troops or active troops still safely at home, and
excludes retreating, QRF and operator-controlled units.

Target selection evaluates economic checkpoints within 2,800 elmos of ground
travel from the proposed landing. Turrets and mobile units never increase the
prize. Mex utility includes its replacement cost and 180 metal of income denial
(an approximate 90 seconds at 2 metal/s). The minimum utility is 800. These are
policy weights, not predictions of metal destroyed or measured game outcomes.

A pocket is rejected when ground defenses cover more than 35% of its economic
utility. Guarded checkpoints contribute only 10% of their utility, and checkpoints
with too much local opposition contribute nothing. Thus a bare chain of mexes
can qualify while the same chain with a Lotus per mex cannot, even when the
riots could beat an individual Lotus. One isolated Lotus need not veto an
otherwise open pocket. Static coverage includes a 200-elmo approach margin.

Ground raiders and light gunships (Locust, Harpy, Gnat) contribute 10% of ordinary
danger. Ground skirmishers that outrange the riots lose the generic artillery
danger discount. Other opposition retains its ordinary strength. Unknown contacts
add danger and cannot masquerade as unarmed economic prizes. Mobile evidence
expires after 30 seconds; static defenses remain remembered until disproved.

Launch requires local opposition below 60% of riot power, a useful saving over
the ground journey, and a safe air route. Gunship-factory and Charon procurement
use the same pocket assessment; inaccessible fortified value alone does not
justify a transport investment.

Landed troops stay in a dedicated controller, including while other transports
are still unloading. They attack nearby economic checkpoints, then scout local
mex sites and revisit them for rebuilds. They fire freely at QRF without chasing
it away from the job. Surviving troops accept opposition up to 120% of remaining
power, with no health-based retreat to the mainland. Ranged ground counters
outside reply range trigger a local evasion; counters caught within riot range
do not. Unreachable or stalled checkpoints are temporarily skipped. Emergency
landings outside the intended pocket start local raids where they actually land.

## Validation

`cargo test -p zkgbai --lib --offline` includes a paired synthetic shoreline:
five bare mexes qualify; the identical positions with a Lotus per mex do not.
It also covers sparse defenses, unreachable prizes, cargo restrictions, minimum
flight size, light QRF classification and local ranged-counter evasion.

`test=dropbot` (Reavers) or `test=drop` (Maces) supplies five riots and Charons
for a transport smoke test. These modes waive the walk-saving and minimum-value
requirements, but retain defense and air-route checks. Against `test=farm` on
FolsomDamDeluxeV4, the smoke test checks full loading, landing and successive
local economic targets. This is a controller test, not a measured natural-game
win rate against Circuit or ZKGBAI.

The fortified Folsom smoke test (`dropbot` against `fort`, 6,000 frames)
rejected 14 candidates for defenses and four for air-route constraints with a
full five-Reaver force available; no sortie launched. Both engine smoke tests
completed without AI panics. These staged opponents isolate mechanics and
selection; they are not Circuit/ZKGBAI natural-game benchmarks.
