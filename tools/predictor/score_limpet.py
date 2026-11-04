#!/usr/bin/env python3
"""Summarize an offline Limpet probe; never treats counterfactuals as observed outcomes.

python3 tools/predictor/score_limpet.py INPUT.json RESULTS.jsonl SUMMARY.json
"""
import hashlib
import json
import sys
from pathlib import Path

import numpy as np
import pandas as pd


EPS = 0.01  # metal: treat f32 subtraction noise as a tie, not a commitment


def summarize(input_path, result_path):
    data = json.loads(Path(input_path).read_text())
    cases = {c["id"]: c for c in data["cases"]}
    rows = pd.read_json(result_path, lines=True)
    assert True in set(rows.limpet), "Corrected mechanics missing"
    assert len(rows) == len(cases)*6*rows.limpet.nunique(), "Incomplete sweep"
    assert not rows.duplicated(["id", "twod", "horizon", "limpet"]).any()
    groups = []
    for (kind, twod, horizon, limpet), g in rows.groupby(["kind", "twod", "horizon", "limpet"]):
        groups.append(dict(kind=kind, twod=bool(twod), horizon=int(horizon), limpet=bool(limpet),
                           n=len(g), positive=int((g.net > EPS).sum()),
                           positive_but_losing_trade=int(((g.net > EPS) & (g.charge_trade < 0)).sum()),
                           median_net=float(g.net.median())))
    fixed = rows[rows.limpet]
    robust = []
    for kind, g in fixed.groupby("kind"):
        by_case = g.groupby("id").net.agg(["min", "max"])
        robust.append(dict(kind=kind, n=len(by_case), positive_all_six=int((by_case["min"] > EPS).sum()),
                           nonpositive_all_six=int((by_case["max"] <= EPS).sum()),
                           sign_sensitive=int(((by_case["min"] <= EPS) & (by_case["max"] > EPS)).sum())))
    paired = rows.pivot(index=["id", "kind", "twod", "horizon"], columns="limpet", values="net")
    flips = None
    if False in paired.columns:
        flips = dict(reject_to_commit=int(((paired[False] <= EPS) & (paired[True] > EPS)).sum()),
                 commit_to_reject=int(((paired[False] > EPS) & (paired[True] <= EPS)).sum()))
    eco = fixed[(fixed.kind == "synthetic") & fixed.id.str.contains("staticmex|energysolar|amphcon")]
    # Descriptive factual score only: complete observation window, no logged
    # reinforcements, initial cohort within the segmenter's 90-frame window.
    scores = []
    for (twod, horizon, limpet), g in rows[rows.kind == "recorded"].groupby(["twod", "horizon", "limpet"]):
        errors, games, ncases = [], set(), 0
        for r in g.itertuples():
            c = cases[r.id]
            if c["duration"] < horizon or c["reinforcements"] > 0 or c["max_first"] > 90:
                continue
            y = [int(u["kill_f"] is not None and u["kill_f"] <= horizon) for u in c["units"]]
            errors.extend((np.array(r.p_raw)-np.array(y))**2)
            games.add(c["game"])
            ncases += 1
        scores.append(dict(twod=bool(twod), horizon=int(horizon), limpet=bool(limpet),
                           fights=ncases, games=len(games), unit_observations=len(errors),
                           raw_brier=float(np.mean(errors)) if errors else None))
    # These are illustrative, selected after the sweep; not an evaluation set.
    examples = fixed[(fixed.kind == "synthetic") & fixed.twod & (fixed.horizon == 600)]
    examples = examples[examples.id.str.contains("amphraid")].sort_values("net", ascending=False).head(8)
    keep = ["id", "net", "gain", "saved", "spent", "p_through", "charge_trade"]
    return dict(decision_epsilon_metal=EPS, counts=data["counts"], groups=groups, robustness=robust, sign_flips=flips,
                unarmed_controls=dict(decisions=len(eco), positive=int((eco.net > EPS).sum()),
                                      max_net=float(eco.net.max())),
                factual_scores=scores, illustrative_ducks=examples[keep].to_dict("records"),
                files={str(p): hashlib.sha256(Path(p).read_bytes()).hexdigest() for p in (input_path, result_path)},
                caveats=["Observed engagements do not identify the outcome of withholding a Limpet.",
                         "No terrain, water, current slow state or reliable reload phase in these snapshots.",
                         "Reserve means removed safely from the fight, not holding exposed under enemy fire.",
                         "Positive marginal value does not imply a favorable total squad trade.",
                         "Allied commitment is assumed; recorded players may have retreated instead."])


if __name__ == "__main__":
    result = summarize(*sys.argv[1:3])
    Path(sys.argv[3]).write_text(json.dumps(result, indent=2)+"\n")
    for key in ("robustness", "sign_flips", "unarmed_controls", "illustrative_ducks"):
        print(key, json.dumps(result[key]))
