# Offline Limpet commitment experiment — 2026-11-04

The mechanics predictor can express the requested decision after adding Limpet's
missing explosion effects. This experiment demonstrates the mechanism and its
counterfactual accounting; it does **not** establish real-game decision accuracy.
The mechanics correction is now unconditional in every Rust/Python mechanics
formulation. The initial probe left recruitment, orders and retreat unchanged;
the live tactic added later is documented at the end of this file.

Worktree branch: `codex/limpet-probe`, based on `wip` at
`f5f9dbdad017c405f9a2ada92c039961e6615a8e`.

## What was missing

The existing death table contains Limpet's 120.1 HP damage and 250-elmo blast
radius. The weapon table even contains its slow multiplier, but death explosions
never consult it. Consequently the old simulator neither slows the enemy nor
honors Limpet's friendly-fire immunity; it damages nearby escorts instead.

The shared Rust/Python death-explosion paths now implement the
`units/amphbomb.lua` custom parameters: slow damage at 10 times actual blast
damage, 60 overslow frames (0.08 of current HP under `timeslow_defs.lua`), and no
friendly damage or slow. Existing slow mechanics reduce movement and firing
rates and decay the effect. The explosion passes its actual frame to the slow
state, including when enemy fire kills the Limpet before contact. No opt-in
flag remains; all mechanics callers receive the correction. Existing blast geometry and
damage falloff approximations are retained.

## Decision being tested

`Model::bomb_charge` already compares the same committed squad with and without
one bomb. It prices all units using raw per-unit death probabilities, including
unarmed units and the Limpet's 160-metal cost:

    marginal value = extra enemy losses + allied losses avoided - expected Limpet cost

The reserve branch removes the Limpet from the fight. This assumes it can remain
safe elsewhere; unlike a burrowed Imp, an exposed Limpet cannot necessarily make
that assumption. Unknown reload phases are treated as loaded, not invented from
the log. The existing bomb survival mixture handles one-hit kills; other hits
remain expected damage. Allied units are explicitly committed (`ordered`).

A positive marginal value alone does not authorize the squad attack. Compare
the resulting whole-squad trade with declining/withdrawing as well. For example,
four Ducks against three clustered Lotuses improve by about 254 metal with a
Limpet at 20 seconds, but still have an expected net trade of about **−66 metal**.

## Results

The original sweep had 394 situations, each evaluated at 10, 20 and 30 seconds in
both focus/axis and 2-D models, with old and corrected mechanics: 4,728 rows.
The current probe emits the 2,364 corrected rows; the scorer can also read the
original paired results. The results below describe that original experiment.
Differences within 0.01 metal count as ties to suppress f32 subtraction noise.

- **360 controlled situations:** Ducks, Glaives and Knights, squad sizes 2/4/8,
  ten enemy compositions, two spacings, and a Limpet ahead of or behind its squad.
  With the corrected mechanics, 30 situations favor spending the Limpet in
  all six horizon/model combinations; 306 never do; 24 change sign.
- **Defenseless controls:** none of the 648 corrected-model comparisons against
  mexes, solars or constructors favors spending the Limpet. Four Ducks kill three
  solars with or without it; sending it costs exactly 160 metal for no return.
- **At 20 seconds in 2-D:** 43/360 controlled situations favor adding the Limpet,
  but 16 of those still have a negative whole-squad trade. This is why the second
  decision check matters.
- **Accumulated data:** `refit2/fights.pkl` contains 240,436 fights, including
  322 with Limpets across 127 games. Restricting the initial cohort to units first
  observed within 90 frames leaves **34 usable situations across 28 games**.
  Exclusions by side: 95 late Limpet arrivals, 136 missing/nonfinite states,
  14 multiple-Limpet decisions, 43 without an armed escort, and one shield/air
  composition. The 90-frame restriction avoids using the cache's later arrivals
  as though they were present at the start.
- Of those 34 recorded situations, one favors spending in all six combinations,
  30 never do, and three change sign. The single positive 20-second 2-D case
  still has a negative total squad trade. These are replay-derived what-if
  results, **not evidence that those commands would have won the actual fights**.

The existing data provides no eligible factual scoring cohort at these horizons
after requiring a complete observation window and no reinforcements. The scorer
reports no Brier score, rather than labeling unobserved outcomes as survival.
Terrain, water, initial slow, and contemporaneous weapon reload are also absent.
These limitations prevent a claim of empirically validated Limpet decisions.

## Does slow itself make the difference?

Yes in controlled simulations. A separate Python ablation retains blast HP damage
and friendly-fire immunity but removes only slow. These illustrations were
selected after the sweep, not as a held-out accuracy test. All use a 20-second
2-D fight and the same fitted hit tables.

Four Ducks facing two Rippers, 150 elmos apart, with the Limpet 80 elmos ahead:

- Reserve it: expected allied losses **320**, enemy losses **0**.
- Commit it with slow: allied losses **325** including the Limpet, enemy losses
  **480**. The total trade is **+155**, an improvement of **475 metal**.
- Commit it with damage only: allied losses **480**, enemy losses **46**. The
  marginal value is **−114 metal**, so spending it is rejected.

Four Ducks against a Mace similarly move from **−160 marginal value** with
damage only to **+320** with slow, although the resulting total trade merely
breaks even. The solar control stays at **−160** with either explosion variant.

## Validation and next step

`cargo test -p zkgbai --lib military::fightsim --offline -j 2`:
**68 passed, 3 intentionally ignored**. Tests cover enemy slow, overslow cap and
timestamp, escort immunity, refusing a wasteful sacrifice, and 80 Python/Rust
fate fixtures spanning base, tube-reload, focus and 2-D models with both bomb
trigger modes. Existing bomb, reload, capture and model-parity tests also pass.

The existing fitted calibration coefficients are retained. Before enabling a policy, run paired engine scenarios with identical starting
states and explicit squad commitment: reserve the Limpet versus commit it.
Measure both sides' losses, blast delivery, slow duration and weapon timing,
including reload windows, damaged Limpets, exposed reserve positions, water,
and multiple Limpets. The current bomb failure branch explodes at the starting
position; that approximation deserves particular scrutiny for a moving Limpet.

A later live experiment can retain recruitment, add the marginal-value query
alongside the squad engagement verdict, and latch a successful commitment so a
spent-on-contact unit does not attempt ordinary damage-triggered retreat. The
ordinary live combat veto currently excludes mixed squads containing suicide
units, so that integration needs an explicit treatment rather than simply
calling the existing veto unchanged.

## Reproduce

Run from this worktree. All outputs and Cargo intermediates remain on disk.
Python requires the existing numpy/pandas/scipy predictor environment and fitted
assets under `~/Programs/zkgbai-runs/predictor`.

```bash
export CARGO_TARGET_DIR=/home/annarhiid/Programs/zkgbai-rs/target/limpet-probe-build
export CARGO_BUILD_BUILD_DIR="$CARGO_TARGET_DIR"
python3 tools/predictor/limpet_probe.py target/limpet-input.json
cargo run -p zkgbai --offline -j 2 --example limpet_probe -- target/limpet-input.json > target/limpet-results.jsonl
python3 tools/predictor/score_limpet.py target/limpet-input.json target/limpet-results.jsonl target/limpet-summary.json
python3 tools/predictor/ablate_limpet.py target/limpet-input.json target/limpet-ablation.json
python3 tools/predictor/export_limpet.py ~/Programs/zkgbai-runs/predictor/refit2 crates/zkgbai
cargo test -p zkgbai --lib military::fightsim --offline -j 2
```

The summary includes input/output SHA-256 hashes. Raw results, input states,
the summary, ablation, and test log are in this worktree's ignored `target/`.

## Live tactic (`limpet`, 2026-11-04)

The experiment now has an operator-controlled live policy: `limpet` defaults on
in wip and off in main. `enable=limpet` / `disable=limpet` and the operator
feature panel control it. `limpetshadow` defaults off; with `limpet` disabled,
it journals candidate decisions without taking ownership or changing orders.

Recruitment is unchanged. Eligible ordinary raider, assault and hover squads
reserve their Limpets behind their escorts. A reserved bomb cannot anchor the
squad formation. At engagement checks, the nearest available Limpet is compared
with reserving it, with the actual squad as escorts and other reserved Limpets
removed from both branches. Both branches explicitly commit allied fire, also
on main; they retain the selected base/tube/focus/2-D mechanics formulation.
The tactic requires more than 10 metal of marginal benefit, a nonnegative total
squad trade, and predicted blast delivery. Without that benefit the escorts may
still attack without the bomb. A profitable strike can override the ordinary
threat retreat. It uses observed tube state when that branch supplies it, and
otherwise assumes loaded weapons rather than inventing reload evidence.

The bomb and its escorts receive attack orders and temporary command ownership;
squad reassignment, coalescing and damage-triggered retreat cannot cancel that
commitment. It ends when the bomb dies, the target dies or becomes stale, the
squad becomes unavailable, the switch is disabled, or 20 seconds elapse. Other
controllers and operator orders take precedence; ownership is only restored if
it is still ours. A switched-off tactic releases its units on the next update.

The initial policy declines to price unknown, shielded, airborne, submerged or
larger-than-24-unit fights. Special squads, strikes, QRF drafts and units already
borrowed by another controller retain their own conduct. At most two fresh
counterfactual queries run per frame. Journal `k=limpet` records the snapshot,
member IDs, marginal and total trades, charge/reserve/shadow decision, query
cost and commitment releases. This is an experimental policy, not a claim of
validated match performance; the earlier offline and engine-model limitations
still apply.
