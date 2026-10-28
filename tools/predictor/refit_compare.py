#!/usr/bin/env python3
"""Refit 1 scorer: shipped dps_stop tables vs refits on games the shipped tables never saw.

usage: refit_compare.py <refit-dir> <old-cache-dir> <shipped coef tsv> <zk units dir>
  <refit-dir>  predictor/refit1 (refit/, empcap_out/, shipped_out/ each with fights_pred.tsv;
               unitdefs.tsv, wtab.pkl)
Arms: shipped (round-2 tables, shipped calibration applied verbatim), shipped_recal (same tables,
calibration refitted on the new train split; diagnostic), refit, empcap, and flat for reference.
Writes COMPARE.md, compare.tsv, bootstrap_compare.tsv and owner_bias.tsv into <refit-dir>.
"""
import os
import pickle
import re
import sys

import numpy as np
import pandas as pd
from scipy.special import logit

import predict

ARMS = ["flat", "shipped", "shipped_recal", "refit", "empcap"]


def load_coef(path):
    kv = {}
    for line in open(path):
        if line.startswith("#") or line.startswith("key\t"):
            continue
        k, v = line.rstrip("\n").split("\t")
        kv[k] = float(v)
    win = predict.Logit1(); win.w = np.array([kv["win.w0"], kv["win.w1"]])
    cal = {}
    for side in "AB":
        c = predict.ZOIBeta(); c.theta = np.array([kv[f"zoib{side}.t{i}"] for i in range(7)]); cal[side] = c
    return win, cal


def combined(rdir, coef):
    key = ["game", "fid"]
    base = pd.read_csv(os.path.join(rdir, "refit", "fights_pred.tsv"), sep="\t", low_memory=False)
    keep = key + ["series", "split", "compA", "compB", "nA", "nB", "costA", "costB", "yA", "yB", "win"]
    df = base[keep].copy()
    for c in ("crpsA", "crpsB", "pAwin"):
        df[f"flat_{c}"] = base[f"flat_{c}"].values
        df[f"refit_{c}"] = base[f"dps_stop_{c}"].values
    for arm, sub in (("empcap", "empcap_out"), ("shipped_recal", "shipped_out")):
        o = pd.read_csv(os.path.join(rdir, sub, "fights_pred.tsv"), sep="\t", low_memory=False)
        o = df[key].merge(o, on=key, how="left")
        assert len(o) == len(df) and o["dstop_A"].notna().all(), arm
        for c in ("crpsA", "crpsB", "pAwin"):
            df[f"{arm}_{c}"] = o[f"dps_stop_{c}"].values
        if arm == "shipped_recal":
            dA, dB = o["dstop_A"].values, o["dstop_B"].values
    win, cal = coef
    zA = logit(np.clip(dA, 0.005, 0.995)); zB = logit(np.clip(dB, 0.005, 0.995))
    df["shipped_crpsA"] = cal["A"].crps(zA, df["yA"].values)
    df["shipped_crpsB"] = cal["B"].crps(zB, df["yB"].values)
    df["shipped_pAwin"] = win.prob(dB - dA)
    return df


def grep_units(zk, pat):
    out = set()
    for f in os.listdir(zk):
        if f.endswith(".lua") and re.search(pat, open(os.path.join(zk, f), errors="ignore").read()):
            out.add(f[:-4])
    return out


def side_tagger(rdir, zk):
    ud = pd.read_csv(os.path.join(rdir, "unitdefs.tsv"), sep="\t", dtype=str, keep_default_na=False).set_index("name")
    w = pickle.load(open(os.path.join(rdir, "wtab.pkl"), "rb"))
    extra = {"emp": grep_units(zk, r"extra_damage"), "slow": grep_units(zk, r"timeslow_damagefactor|timeslow_onlyslow"),
             "fire": grep_units(zk, r"setunitsonfire")}
    memo = {}

    def unit_tags(name):
        t = set()
        if name not in ud.index:
            return t
        r = ud.loc[name]
        for wn in (x.split(":")[0] for x in r["weapons"].split(",") if x):
            if wn in w.index:
                row = w.loc[wn]
                if row["paralyzer"] == 1:
                    t.add("emp")
                if row["slow"] > 0:
                    t.add("slow")
                if row["fire"] > 0:
                    t.add("fire")
                if row["type"] == "Shield":
                    t.add("shield")
        if r["shieldpower"] not in ("", "0"):
            t.add("shield")
        for k, s in extra.items():
            if name in s:
                t.add(k)
        if r["canfly"] == "1":
            t.add("air")
        return t

    def tags(comp):
        out = set()
        if isinstance(comp, str):
            for tok in comp.split():
                n = tok.rsplit("x", 1)[0]
                if n not in memo:
                    memo[n] = unit_tags(n)
                out |= memo[n]
        return out
    return tags


def owner_bias(d, arm, tag, rng, reps=500):
    a = d["tA"].map(lambda s: tag in s).values
    b = d["tB"].map(lambda s: tag in s).values
    one = (a ^ b) & (d["win"].values >= 0)
    s = d[one]
    if len(s) < 20:
        return {"n": len(s)}
    won = np.where(a[one], s["win"].values, 1 - s["win"].values)
    p = np.where(a[one], s[f"{arm}_pAwin"].values, 1 - s[f"{arm}_pAwin"].values)
    res = won - p
    games, gi = np.unique(s["game"].values, return_inverse=True)
    W = np.stack([np.bincount(rng.integers(0, len(games), len(games)), minlength=len(games)) for _ in range(reps)]).astype(float)[:, gi]
    bs = (W * res).sum(1) / W.sum(1)
    lo, hi = np.percentile(bs, [2.5, 97.5])
    return {"n": len(s), "games": len(games), "owner_won": won.mean(), "P_owner": p.mean(), "bias": res.mean(), "lo": lo, "hi": hi}


def main(rdir, old_cache, coef_path, zk):
    df = combined(rdir, load_coef(coef_path))
    old = set(os.listdir(old_cache))
    df["old_game"] = (df["series"].astype(str) + ".pkl").isin(old)
    big = (df["nA"] >= 3) & (df["nB"] >= 3)
    test = df["split"] == "test"
    sets = {
        "PRIMARY new-test >=3 armed/side": test & ~df["old_game"] & big,
        "new-test all": test & ~df["old_game"],
        "old-test >=3 armed/side": test & df["old_game"] & big,
        "old-test all": test & df["old_game"],
        "human (live) all": df["split"] == "human",
        "replay all": df["split"] == "replay",
        "replay >=3 armed/side": (df["split"] == "replay") & big,
    }
    rows, boots = [], []
    for name, m in sets.items():
        d = df[m.values]
        if len(d) == 0:
            continue
        for arm in ARMS:
            ma = predict.metric_arrays(d, arm)
            rows.append({"set": name, "arm": arm, "n": len(d), "n_decided": int((d["win"] >= 0).sum()), "games": d["game"].nunique(),
                         "logloss": np.nanmean(ma["logloss"]), "brier": np.nanmean(ma["brier"]), "crps": np.nanmean(ma["crps"])})
        if d["game"].nunique() >= 10:
            b = predict.bootstrap(d, ARMS)
            b.insert(0, "set", name)
            boots.append(b)
    sc = pd.DataFrame(rows)
    bs = pd.concat(boots)
    sc.to_csv(os.path.join(rdir, "compare.tsv"), sep="\t", index=False, float_format="%.4f")
    bs.to_csv(os.path.join(rdir, "bootstrap_compare.tsv"), sep="\t", index=False, float_format="%.4f")
    tagger = side_tagger(rdir, zk)
    df["tA"] = df["compA"].map(tagger); df["tB"] = df["compB"].map(tagger)
    rng = np.random.default_rng(7)
    ob = []
    for tag in ("emp", "fire", "air", "slow", "shield"):
        for arm in ("shipped", "refit", "empcap"):
            r = owner_bias(df[(test & ~df["old_game"]).values], arm, tag, rng)
            ob.append({"set": "new-test", "tag": tag, "arm": arm, **r})
    ob = pd.DataFrame(ob)
    ob.to_csv(os.path.join(rdir, "owner_bias.tsv"), sep="\t", index=False, float_format="%.4f")
    f3 = lambda v: f"{v:.3f}"
    sel = bs[(bs["metric"].isin(["logloss", "brier", "crps"])) &
             (((bs["a"] == "shipped") & bs["b"].isin(["refit", "empcap", "shipped_recal"])) | ((bs["a"] == "refit") & (bs["b"] == "empcap")) |
              ((bs["a"] == "flat") & bs["b"].isin(["shipped", "empcap"])))]
    with open(os.path.join(rdir, "COMPARE.md"), "w") as fh:
        fh.write("# Refit 1 comparison (tools/predictor/refit_compare.py)\n\n")
        fh.write(f"Fights: {len(df)} closed evaluation fights; splits {df['split'].value_counts().to_dict()}; "
                 f"test fights in games new since the 1151-game corpus: {int((test & ~df['old_game']).sum())}\n\n")
        fh.write("## Scores (dps_stop per arm; flat = the AI's threat rule, calibrated)\n\n```\n" + sc.to_string(index=False, float_format=f3) + "\n```\n\n")
        fh.write("## Paired-by-game bootstrap, a - b (positive = b better)\n\n```\n" + sel.to_string(index=False, float_format=f3) + "\n```\n\n")
        fh.write("## Owner bias on new-test decided fights (fields the feature on one side only; + = sim under-rates the owner)\n\n```\n"
                 + ob.to_string(index=False, float_format=f3) + "\n```\n")
    print(open(os.path.join(rdir, "COMPARE.md")).read())


if __name__ == "__main__":
    main(*sys.argv[1:5])
