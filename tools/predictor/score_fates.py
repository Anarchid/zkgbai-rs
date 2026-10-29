#!/usr/bin/env python3
"""Score the mechanics model's per-unit fates (simulate_mech(fates=...)) against what happened.

usage: score_fates.py <fights.pkl> <out-dir> [workers] [horizons, default 150,300,900]
Fights of the test fold (split.split_of == "test") with an armed unit on each side at the start. The
snapshot is the units present in the fight's first second (record "first" <= 30), as a planner would
see them; each is scored on whether it died within the horizon (kill_f <= H). Model: the a-move fates
on the crate's tables (export_mech.init: refit-1 tables and hazard, refit2 mechanics, opts six +
heatray), no hazard. Compared with:
  base rate     the scored set's own death rate by side and static/mobile (in-sample)
  side share    each side's killed share of cost by the horizon in the same sim, the same p for every
                unit of the side (what the side-level answer knows)
  p_dead cal    p_dead recalibrated on the train fold's fights (logistic on logit p_dead, static,
                log(h / 150); cal.tsv)
Writes scores.tsv (Brier, log loss, AUC per horizon and model), reliability.tsv, within.tsv (within
a side where some but not all present units died: does p_dead rank the dead first, pairwise
concordance), units.tsv (one row per scored unit).
"""
import os
import pickle
import sys
from multiprocessing import Pool

import numpy as np

import export_mech
import mech
import split

G = {}


def init(horizons):
    export_mech.init(os.path.join(export_mech.ROOT, "refit2"))
    G["H"] = horizons


def present(fight):
    return {"A": [r for r in fight["A"] if r["first"] <= 30], "B": [r for r in fight["B"] if r["first"] <= 30]}


def run(fight):
    snap = dict(fight, **present(fight))
    recs = snap["A"] + snap["B"]
    if not any(r["armed"] for r in snap["A"]) or not any(r["armed"] for r in snap["B"]):
        return []
    rows = []
    for h in G["H"]:
        fa = {}
        try:
            mech.simulate_mech(snap, export_mech.OPTS, None, fates=fa, horizon=h)
        except Exception as e:  # a def missing from the tables
            print("skip", fight["game"], fight["fid"], e, file=sys.stderr)
            return []
        cost = [sum(r["cost"] for r in snap["A"]), sum(r["cost"] for r in snap["B"])]
        dead_cost = [0.0, 0.0]
        for i, r in enumerate(recs):
            if fa["dead"][i]:
                dead_cost[0 if i < len(snap["A"]) else 1] += r["cost"]
        for i, r in enumerate(recs):
            s = 0 if i < len(snap["A"]) else 1
            y = bool(r["died"] and r["kill_f"] is not None and r["kill_f"] <= h)
            rows.append((fight["game"], fight["fid"], h, s, r["def"], int(r["static"]), int(r["armed"]), r["cost"], r["hp"],
                         int(y), float(fa["pdead"][i]), int(fa["dead"][i]), dead_cost[s] / max(cost[s], 1.0)))
    return rows


def auc(p, y):
    order = np.argsort(p, kind="mergesort")
    r = np.empty(len(p)); r[order] = np.arange(1, len(p) + 1)
    # ties share their mean rank
    ps = p[order]
    i = 0
    while i < len(ps):
        j = i
        while j + 1 < len(ps) and ps[j + 1] == ps[i]:
            j += 1
        if j > i:
            r[order[i:j + 1]] = (i + j + 2) / 2.0
        i = j + 1
    pos = y.sum(); neg = len(y) - pos
    return (r[y == 1].sum() - pos * (pos + 1) / 2) / max(pos * neg, 1)


def cal_feats(d):
    p = np.clip(d.pdead.values, 0.005, 0.995)
    return np.column_stack([np.ones(len(d)), np.log(p / (1 - p)), d.static.values, np.log(d.h.values / 150.0)])


def fit_logistic(X, y, l2=1e-3):
    from scipy.optimize import minimize

    def nll(w):
        q = np.clip(1.0 / (1.0 + np.exp(-X @ w)), 1e-9, 1 - 1e-9)
        return -(y * np.log(q) + (1 - y) * np.log(1 - q)).sum() + l2 * np.sum(w[1:] ** 2)
    return minimize(nll, np.zeros(X.shape[1]), method="L-BFGS-B").x


def scores(p, y):
    q = np.clip(p, 1e-3, 1 - 1e-3)
    return {"n": len(y), "rate": y.mean(), "mean_p": p.mean(), "brier": np.mean((p - y) ** 2),
            "logloss": -np.mean(y * np.log(q) + (1 - y) * np.log(1 - q)), "auc": auc(p, y)}


def main(path, out, workers=2, horizons="150,300,900"):
    import pandas as pd
    hs = [int(h) for h in horizons.split(",")]
    fights = pickle.load(open(path, "rb"))
    fold = {id(f): split.split_of(f["game"], f["series"]) for f in fights}
    fights = [f for f in fights if fold[id(f)] in ("train", "test")]
    print(sum(fold[id(f)] == "test" for f in fights), "test fights,", sum(fold[id(f)] == "train" for f in fights), "train fights")
    with Pool(int(workers), initializer=init, initargs=(hs,)) as pool:
        rows = [(fold[id(f)],) + r for f, rs in zip(fights, pool.imap(run, fights, chunksize=64)) for r in rs]
    os.makedirs(out, exist_ok=True)
    d = pd.DataFrame(rows, columns=["fold", "game", "fid", "h", "side", "def", "static", "armed", "cost", "hp", "y", "pdead", "det",
                                    "share"])
    d.to_csv(os.path.join(out, "units.tsv"), sep="\t", index=False)
    # recalibration on the train fold: logistic on logit(p_dead), static, log(h / 150)
    tr = d[d.fold == "train"]
    w = fit_logistic(cal_feats(tr), tr.y.values.astype(float))
    pd.DataFrame({"name": ["const", "logit p_dead", "static", "log(h/150)"], "w": w}).to_csv(
        os.path.join(out, "cal.tsv"), sep="\t", index=False, float_format="%.6f")
    print("calibration on", len(tr), "train unit rows:", " ".join(f"{v:+.3f}" for v in w))
    d = d[d.fold == "test"].copy()
    d["cal"] = 1.0 / (1.0 + np.exp(-cal_feats(d) @ w))
    res, rel, within = [], [], []
    for h, g in d.groupby("h"):
        y = g.y.values.astype(float)
        base = g.groupby(["side", "static"]).y.transform("mean").values
        for name, p in (("p_dead", g.pdead.values), ("p_dead cal", g.cal.values), ("dead flag", g.det.values.astype(float)),
                        ("side share", g.share.values), ("base rate", base)):
            for sub, m in (("all", np.ones(len(g), bool)), ("statics", g.static.values == 1), ("mobile", g.static.values == 0)):
                if m.sum():
                    res.append(dict(h=h, model=name, units=sub, **scores(p[m], y[m])))
        for col in ("pdead", "cal"):
            bins = pd.cut(g[col], [-0.01, 0.05, 0.2, 0.4, 0.6, 0.8, 0.95, 1.0])
            for b, gb in g.groupby(bins, observed=True):
                rel.append(dict(h=h, p=col, bin=str(b), n=len(gb), mean_p=gb[col].mean(), dead=gb.y.mean()))
        # within a side with mixed outcomes: P(a dead unit ranks above a living one)
        conc = ties = pairs = 0
        conc_s = pairs_s = 0
        for _, gs in g.groupby(["game", "fid", "side"]):
            if gs.y.nunique() < 2:
                continue
            dead, live = gs[gs.y == 1], gs[gs.y == 0]
            pd_, pl = dead.pdead.values[:, None], live.pdead.values[None, :]
            conc += (pd_ > pl).sum(); ties += (pd_ == pl).sum(); pairs += pd_.size * pl.size
            sd, sl = dead.share.values[:, None], live.share.values[None, :]
            conc_s += (sd > sl).sum() + 0.5 * (sd == sl).sum(); pairs_s += sd.size * sl.size
        within.append(dict(h=h, pairs=pairs, p_dead_concordance=(conc + 0.5 * ties) / max(pairs, 1),
                           p_dead_ties=ties / max(pairs, 1), side_share_concordance=conc_s / max(pairs_s, 1)))
    pd.DataFrame(res).to_csv(os.path.join(out, "scores.tsv"), sep="\t", index=False, float_format="%.4f")
    pd.DataFrame(rel).to_csv(os.path.join(out, "reliability.tsv"), sep="\t", index=False, float_format="%.4f")
    pd.DataFrame(within).to_csv(os.path.join(out, "within.tsv"), sep="\t", index=False, float_format="%.4f")
    with pd.option_context("display.width", 200):
        print(pd.DataFrame(res).to_string(index=False, float_format=lambda v: f"{v:.3f}"))
        print(pd.DataFrame(rel).to_string(index=False, float_format=lambda v: f"{v:.3f}"))
        print(pd.DataFrame(within).to_string(index=False, float_format=lambda v: f"{v:.3f}"))


if __name__ == "__main__":
    main(*sys.argv[1:5])
