# Ravager pack drives versus assault orders: exploratory estimate

29 October 2026. The local sweep is restored on main and in the lobby WIP;
Ravager and Minotaur packs are enabled in WIP. This change does not introduce
an automatic switch between tactics.

## What the existing comparisons can identify

The strongest direct control is `zkgbai-runs/pack/scen/score2.csv`: 718 completed
loads, 30 saved situations, five parent games. The primary comparison uses
**240 loads**: four repeats of `mixedC` (charge mobile targets, pass buildings,
finish committed runs) and four repeats of `plain` (Ravagers in assault squads)
for each save. The other arms supply sensitivity checks, not extra independent
examples of the primary comparison. Saves overlap within games, and their
opponent factory families are confounded with their parent games.

These runs used p6, before local sweep, the later repair/porc changes and the
fates planner. Later sweep and judge experiments have different controls and
cannot be pooled as if they were additional pack-versus-assault comparisons.
The harvest also selected saves shortly before a pack engagement, excluding
many defensive/idle situations and a sixth game lost before a qualifying save.

The estimator reconstructs the latest AI journal snapshot at or before each
save, 5–35 seconds before the harvest's subsequent engagement. It uses the AI's
known enemies and unit roles, never the opponent's eventual factory family,
post-treatment positions or realised kills. Replicates average within a save;
95% intervals use parent-game cluster standard errors and Student t with G−1
degrees of freedom. With five games these intervals remain approximate.

## Data audit

The old whole-team totals count `terraunit` at its nominal 100,000-metal cost.
This is a temporary terraforming helper, not a real army loss. The analysis
rebuilds kills from all 718 raw logs within each save's 150-second horizon and
excludes terraform helpers, spawned Wolverine mines and drones. Across these
logs it excludes 305 helper deaths, 60,471 mine deaths and 624 drone deaths.

This matters: the mixed-versus-assault whole-team trade calculated directly
from the original CSV is approximately −3,990 metal per situation; the corrected
estimate is **+178**, with a 95% interval of **−408 to +764**. The old total is
not suitable for tactic selection. Ravager-only trade changes little. This
does not erase the separate full-game evidence of reduced income/production
in `pack/full/RESULTS.md`; those measurements do not rely on this kill total.

## Conditional results

The primary outcome is Ravager-credited mobile kills plus 1.5 times economy
and turret kills, minus all Ravager losses, in metal over 150 seconds. Last-hit
credit and this short horizon miss shared damage, economic denial and later
home-defense costs; corrected whole-team trade is a secondary outcome.

* Overall, mixed driving gains **143 weighted metal**, interval **−188 to +474**,
  over assault orders. The unweighted difference is **+123 metal**. Neither
  establishes a general advantage.
* In five saves from three games with **at least 250 metal of nearby lightly
  defended economy**, the weighted gain is **+735**, interval **−869 to +2,339**.
  Other saves average **+25**. This is the clearest operational lead, but far
  from a validated threshold. Nearby means within 1,500 elmos of the largest
  available Ravager group; lightly defended means known ground guns covering
  that economy sum to at most one quarter of the group's health-adjusted metal.
* That subgroup's extra value mostly comes from **+449 mobile metal killed and
  250 fewer Ravager metal lost**, with only **+56 economy metal killed**. It does
  not demonstrate that old pack driving already cleaned up economy well.
* Health, cohesion, nearby mobile pressure, riot/EMP value and the pack's share
  of the assigned ground army do not yield a reliable separator. The apparently
  better trade for wounded packs should not be turned into a health rule;
  scenario timing and composition can explain that association.

A deliberately small switching experiment chooses at most one operational
split using four parent games, then evaluates it on the fifth, repeating for
all five held-out games. It requires six training saves in each branch and
rejects a rule with negative mean corrected team trade in training. It chooses
packs in 25 of the 30 held-out saves. Its weighted gain over always assault is
**+78**, interval **−129 to +286**, and its gain over always driving is **−65**,
interval **−215 to +86**. The secondary team-trade estimate is +236 (nominal
interval +18 to +453), but this exploratory secondary result does not validate
the primary switching objective or an individual threshold. There is no
established switching policy in this data.

The separate, newer cleanup experiment (`pack/clean/README.md`) supports the
sweep itself: Ravagers clear about 304 more weighted metal in 180 seconds with
sweep than with the global picker, mostly mobiles. It has no assault arm, so it
cannot identify sweep-driving's advantage over assault orders. Mostly single
Minotaurs showed no clear improvement.

## Implication for adaptive tactics

The promising hypothesis is an **opportunity mode inside an ordinary assault
squad**, triggered by a nearby cluster that can be traversed cheaply, with home
defense and operator assignments taking precedence. That would preserve squad
membership and defense eligibility, avoiding the structural cost of moving all
Ravagers and Rippers into a permanently separate doctrine. Size, cohesion,
health, turret exposure and a clear route out remain eligibility checks; their
cutoffs have not been estimated reliably here.

The useful next control is an assault arm on the existing cleanup saves, using
the current sweep build, plus a longer whole-game defense check. No new harvest
is needed for that local comparison. The fates predictor should be assessed on
**the difference between drive and assault on the same situation**, rather than
using its absolute death predictions as an already calibrated selector.

## Reproduce

Run `python3 tools/predictor/pack_situations.py --runs
/home/annarhiid/Programs/zkgbai-runs --out /tmp/pack-tactics-estimate`.
The script writes the corrected run totals, every scenario's pre-save features
and paired effects, held-out policy decisions, complete intervals, and a
labelled PNG/SVG figure. `estimate.json` records the original CSV's SHA-256 and
all limitations. All inputs remain read-only.
