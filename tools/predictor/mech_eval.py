#!/usr/bin/env python3
"""Score mechanics variants of dps_stop on the dev fold (mechanics round 1).

usage: mech_eval.py <pdir> <fights.pkl> <mech.pkl> <out-dir> <variants> [workers] [n_cal]
  PRED_HAZARD must be set (the hazard fitted on the same train games as the tables);
  PRED_DEV_FOLD when scoring the dev fold. MECH_SCORE: comma-separated splits to score
  (default "dev"; e.g. "test,human,replay,fresh"); MECH_FRESH: a fights.pkl of fresh games,
  scored as split "fresh" (live human logs among them as "fresh-human").
  <variants>: "single" = base + each component alone (with the declared grids), or
  "combo:air,burn,rate=0.2,..." = that full set, plus the set minus each component;
  "pair:..." = base and the full set only; "base=a,b:c,d" = that set as the base, plus it with each
  of c, d alone and with all of them (mechanics round 2).
  MECH_TACAI: tacai.json for the tacai switch; MECH_HAZARD_MOB: the hazard used by hazmob variants.
Each variant gets its own calibration (ZOIBeta per side, Logit1 on decided fights) fitted on a
sample of train fights; scored on all dev fights. Writes scores.tsv, bootstrap.tsv,
owner_bias.tsv and RESULTS.md into <out-dir>.
"""
import math
import os
import pickle
import random
import re
import sys
from collections import Counter
from multiprocessing import Pool

import numpy as np
import pandas as pd
from scipy.special import logit

import mech
import predict
import split

G = {}
SINGLE = [("base", {}), ("air", {"air": 1}), ("burn", {"burn": 1}), ("death", {"death": 1}), ("pulse", {"pulse": 1}),
          ("emp", {"emp": 1}), ("slow", {"slow": 1}), ("engage", {"engage": 1}), ("rate 0.1", {"rate": 0.1}),
          ("rate 0.2", {"rate": 0.2}), ("rate 0.3", {"rate": 0.3}), ("desync 1", {"desync": 1}),
          ("desync 2", {"desync": 2}), ("desync 3", {"desync": 3}), ("kite", {"kite": 1})]
TAGS = {
    "fire": {"tankraid", "jumpraid", "jumparty", "bomberriot"},
    "kamikaze": mech.KAMIKAZE,
    "outlaw": {"shieldriot"},
    "emp": {"spideremp", "tankheavyraid", "cloakassault", "turretemp", "cloakbomb", "striderscorpion"},
    "slow": {"shieldriot", "amphfloater", "hoverheavyraid", "jumpskirm", "gunshipskirm", "tankheavyassault", "vehscout", "subraider", "planesupport"},
    "shield": {"shieldassault", "shieldfelon", "shieldshield", "staticshield", "shieldcon", "striderfunnelweb"},
    "dagger": {"hoverraid"},
}
RELEVANT = {"fire": "burn", "kamikaze": "death", "outlaw": "pulse", "emp": "emp", "slow": "slow", "air": "air", "dagger": "desync 2"}


def parse_variants(spec):
    if spec == "single":
        return SINGLE
    if spec.startswith("base="):
        def comps_of(s):
            out = {}
            for item in s.split(","):
                k, _, v = item.partition("=")
                if k:
                    out[k] = float(v) if v else 1
            return out
        base_s, _, add_s = spec[5:].partition(":")
        base, add = comps_of(base_s), comps_of(add_s)
        out = [("base", {}), ("R1", dict(base))]
        for k, v in add.items():
            out.append((f"R1 + {k}", dict(base, **{k: v})))
        if len(add) > 1:
            out.append(("R1 + all", dict(base, **add)))
            for k in add:
                out.append((f"R1 + all - {k}", dict(base, **{kk: vv for kk, vv in add.items() if kk != k})))
        return out
    comps = {}
    for item in spec.split(":", 1)[1].split(","):
        k, _, v = item.partition("=")
        comps[k] = float(v) if v else 1
    out = [("base", {}), ("all", dict(comps))]
    if spec.startswith("pair:"):
        return out
    for k in comps:
        out.append((f"all - {k}", {kk: vv for kk, vv in comps.items() if kk != k}))
    return out


def init(pdir, mech_path, variants):
    predict._init(pdir, os.path.join(pdir, "cache"))
    mech.load(mech_path)
    G["variants"] = variants
    if os.environ.get("MECH_TACAI"):
        mech.load_tacai(os.environ["MECH_TACAI"])
    if os.environ.get("MECH_WATER"):
        mech.load_water(os.environ["MECH_WATER"], os.environ.get("MECH_WATER_HITTERS"))
    if os.environ.get("MECH_SHIELD"):  # shield variants: shield pools + hit tables of body hits
        mech.load_shield(os.environ["MECH_SHIELD"], os.path.join(pdir, "hitcells.pkl"))
        cells = pickle.load(open(os.environ["MECH_HITS_SHIELD"], "rb"))["train"]
        predict.G["hits_shield"] = predict.HitModel(cells, predict.G["ud"])
    if os.environ.get("MECH_HAZARD_MOB"):  # mechanics round 2: variants with hazmob use this hazard
        import disengage
        hd = pickle.load(open(os.environ["MECH_HAZARD_MOB"], "rb"))
        predict.G["hazard_mob"] = disengage.Hazard(hd["w"], mob=hd.get("mob", False))


def run(fight):
    out = {}
    hits = predict.G["hits"]
    for name, opts in G["variants"]:
        hz = predict.G["hazard_mob"] if opts.get("hazmob") else predict.G["hazard"]
        predict.G["hits"] = predict.G["hits_shield"] if (opts.get("shield") or opts.get("shield2")) else hits
        out[name] = mech.simulate_mech(fight, opts, hz)
    predict.G["hits"] = hits
    hz = predict.G["hazard"]
    units = predict.build_units(fight)
    na = sum(u.armed for u in units if u.side == 0); nb = sum(u.armed for u in units if u.side == 1)
    chk = predict.simulate(units, None, hz) if G.get("check", True) else None
    return (fight["game"], fight["fid"]), out, na, nb, chk


def owner_bias(d, col, tagA, tagB, rng, reps=400):
    one = (tagA ^ tagB) & (d["win"].values >= 0)
    s = d[one]
    if len(s) < 30:
        return {"n": int(len(s))}
    a = tagA[one]
    won = np.where(a, s["win"].values, 1 - s["win"].values)
    p = np.where(a, s[col].values, 1 - s[col].values)
    r = won - p
    g, gi = np.unique(s["game"].values, return_inverse=True)
    W = np.stack([np.bincount(rng.integers(0, len(g), len(g)), minlength=len(g)) for _ in range(reps)]).astype(float)[:, gi]
    lo, hi = np.percentile((W * r).sum(1) / W.sum(1), [2.5, 97.5])
    return {"n": int(len(s)), "games": len(g), "bias": float(r.mean()), "lo": float(lo), "hi": float(hi)}


def main(pdir, fights_path, mech_path, out, spec, workers=3, n_cal=12000):
    os.makedirs(out, exist_ok=True)
    variants = parse_variants(spec)
    fs = pickle.load(open(fights_path, "rb"))
    sel = [f for f in fs if f["closed"] and f["costA"] >= predict.MIN_COST and f["costB"] >= predict.MIN_COST
           and not math.isnan(f["yA"]) and not math.isnan(f["yB"])]
    del fs
    for f in sel:
        f["split"] = split.split_of(f["game"], f["series"])
    if os.environ.get("MECH_FRESH"):
        fresh = pickle.load(open(os.environ["MECH_FRESH"], "rb"))
        for f in fresh:
            if f["closed"] and f["costA"] >= predict.MIN_COST and f["costB"] >= predict.MIN_COST \
                    and not math.isnan(f["yA"]) and not math.isnan(f["yB"]):
                f["split"] = "fresh-human" if split.split_of(f["game"], f["series"]) == "human" else "fresh"
                sel.append(f)
    score_splits = os.environ.get("MECH_SCORE", "dev").split(",")
    if "fresh" in score_splits:
        score_splits.append("fresh-human")
    tr = [f for f in sel if f["split"] == "train"]
    dev = [f for f in sel if f["split"] in score_splits]
    random.seed(13)
    if os.environ.get("MECH_DEV_N"):  # smoke runs only
        dev = random.sample(dev, min(int(os.environ["MECH_DEV_N"]), len(dev)))
    cal = random.sample(tr, min(int(n_cal), len(tr)))
    todo = cal + dev
    print(f"variants {len(variants)}; calibration {len(cal)} train fights, scored {Counter(f['split'] for f in dev)}", flush=True)
    rows = []
    worst = 0.0
    with Pool(int(workers), initializer=init, initargs=(pdir, mech_path, variants)) as pool:
        for i, (key, res, na, nb, chk) in enumerate(pool.imap(run, todo, chunksize=16)):
            f = todo[i]
            if chk is not None:
                worst = max(worst, abs(chk[0] - res["base"][0]), abs(chk[1] - res["base"][1]))
            defsA = {r["def"] for r in f["A"]}; defsB = {r["def"] for r in f["B"]}
            ud = None
            row = {"game": f["game"], "fid": f["fid"], "split": f["split"], "yA": f["yA"], "yB": f["yB"], "nA": na, "nB": nb,
                   "defsA": " ".join(sorted(defsA)), "defsB": " ".join(sorted(defsB)),
                   "daggersA": sum(r["def"] == "hoverraid" for r in f["A"]), "daggersB": sum(r["def"] == "hoverraid" for r in f["B"])}
            for name, (kA, kB) in res.items():
                row[f"{name}|A"] = kA; row[f"{name}|B"] = kB
            rows.append(row)
            if (i + 1) % 2000 == 0:
                print(f"{i + 1}/{len(todo)}", flush=True)
    print(f"base vs predict.simulate: max |diff| {worst:.2e}", flush=True)
    df = pd.DataFrame(rows)
    df["win"] = np.where(df["yB"] - df["yA"] > predict.DECIDE, 1, np.where(df["yA"] - df["yB"] > predict.DECIDE, 0, -1))
    trm = (df["split"] == "train").values
    dec = (df["win"] >= 0).values
    for name, _ in variants:
        kA, kB = df[f"{name}|A"].values, df[f"{name}|B"].values
        zA = logit(np.clip(kA, 0.005, 0.995)); zB = logit(np.clip(kB, 0.005, 0.995)); zw = kB - kA
        cA = predict.ZOIBeta().fit(zA[trm], df["yA"].values[trm])
        cB = predict.ZOIBeta().fit(zB[trm], df["yB"].values[trm])
        lw = predict.Logit1().fit(zw[trm & dec], df["win"].values[trm & dec])
        dv = ~trm
        df[f"{name}_pAwin"] = lw.prob(zw)
        df[f"{name}_mA"] = cA.mean(zA); df[f"{name}_mB"] = cB.mean(zB)
        cr = np.full(len(df), np.nan)
        cr[dv] = (cA.crps(zA[dv], df["yA"].values[dv]) + cB.crps(zB[dv], df["yB"].values[dv])) / 2
        df[f"{name}_crpsA"] = cr; df[f"{name}_crpsB"] = cr  # metric_arrays averages the two
    d = df[df["split"].isin(score_splits)].reset_index(drop=True)
    names = [n for n, _ in variants]
    big = ((d["nA"] >= 3) & (d["nB"] >= 3)).values
    sets = []
    for sp in score_splits:
        inb = (d["split"] == sp).values
        if inb.any():
            sets += [(f"{sp} >=3 armed/side", inb & big), (f"{sp} all", inb)]
    sc = []
    for sub, m in sets:
        s = d[m]
        for n in names:
            ma = predict.metric_arrays(s, n)
            sc.append({"set": sub, "variant": n, "n": len(s), "n_decided": int((s["win"] >= 0).sum()), "games": s["game"].nunique(),
                       "logloss": np.nanmean(ma["logloss"]), "brier": np.nanmean(ma["brier"]), "crps": np.nanmean(ma["crps"])})
    sc = pd.DataFrame(sc)
    boots = []
    for sub, m in sets:
        if d[m]["game"].nunique() < 10:
            continue
        b = predict.bootstrap(d[m], names)
        b = b[b["a"].isin(["base", "all", "R1", "R1 + all"]) | b["b"].isin(["all", "R1 + all"])]
        b.insert(0, "set", sub)
        boots.append(b)
    bs = pd.concat(boots)
    rng = np.random.default_rng(7)
    ob = []
    for tag, defs in list(TAGS.items()) + [("air", None)]:
        if defs is None:
            udd = predict.G.get("ud") or __import__("units").load(os.path.join(pdir, "unitdefs.tsv"))
            air = lambda s: any((udd.get(n) is not None and udd.get(n).canfly) for n in s.split())
            tA = d["defsA"].map(air).values; tB = d["defsB"].map(air).values
        elif tag == "dagger":
            tA = (d["daggersA"] >= 2).values & (d["nB"] <= 3).values; tB = (d["daggersB"] >= 2).values & (d["nA"] <= 3).values
        else:
            tA = d["defsA"].map(lambda s: bool(defs & set(s.split()))).values
            tB = d["defsB"].map(lambda s: bool(defs & set(s.split()))).values
        for n in names:
            ob.append({"tag": tag, "variant": n, **owner_bias(d, f"{n}_pAwin", tA, tB, rng)})
    ob = pd.DataFrame(ob)
    # mechanics round 2 checks: calibrated predicted own-loss share vs actual, by side kind
    SKIRM = {"hoverskirm", "cloakskirm", "shieldskirm", "spiderskirm", "jumpskirm", "amphfloater"}
    SHIELDS = {"shieldassault", "shieldfelon", "shieldshield", "staticshield", "shieldcon", "striderfunnelweb"}
    SUBS = set()
    if os.environ.get("MECH_WATER"):
        SUBS = {k for k, v in pickle.load(open(os.environ["MECH_WATER"], "rb"))["moveclass"].items() if v.startswith("UBOAT")}
    udd = predict.G.get("ud") or __import__("units").load(os.path.join(pdir, "unitdefs.tsv"))
    def static_side(defs):
        ds = [udd.get(n) for n in defs.split()]
        armed = [x for x in ds if x is not None and x.weapons]
        return bool(armed) and all(not x.canmove for x in armed)
    mc = []
    for tag, fa, fb in (("skirmisher side", lambda r: bool(SKIRM & set(r.defsA.split())), lambda r: bool(SKIRM & set(r.defsB.split()))),
                        ("all-static side", lambda r: static_side(r.defsA), lambda r: static_side(r.defsB)),
                        ("shield side", lambda r: bool(SHIELDS & set(r.defsA.split())), lambda r: bool(SHIELDS & set(r.defsB.split()))),
                        ("Felon side", lambda r: "shieldfelon" in r.defsA.split(), lambda r: "shieldfelon" in r.defsB.split()),
                        ("sub side", lambda r: bool(SUBS & set(r.defsA.split())), lambda r: bool(SUBS & set(r.defsB.split())))):
        tA = d.apply(fa, axis=1).values; tB = d.apply(fb, axis=1).values
        one = tA ^ tB
        for n in names:
            own_p = np.where(tA[one], d[f"{n}_mA"].values[one], d[f"{n}_mB"].values[one])
            own_y = np.where(tA[one], d["yA"].values[one], d["yB"].values[one])
            opp_p = np.where(tA[one], d[f"{n}_mB"].values[one], d[f"{n}_mA"].values[one])
            opp_y = np.where(tA[one], d["yB"].values[one], d["yA"].values[one])
            mc.append({"side": tag, "variant": n, "n": int(one.sum()), "own_pred": own_p.mean(), "own_actual": own_y.mean(),
                       "opp_pred": opp_p.mean(), "opp_actual": opp_y.mean()})
    mc = pd.DataFrame(mc)
    mc.to_csv(os.path.join(out, "loss_checks.tsv"), sep="\t", index=False, float_format="%.4f")
    sc.to_csv(os.path.join(out, "scores.tsv"), sep="\t", index=False, float_format="%.4f")
    bs.to_csv(os.path.join(out, "bootstrap.tsv"), sep="\t", index=False, float_format="%.4f")
    ob.to_csv(os.path.join(out, "owner_bias.tsv"), sep="\t", index=False, float_format="%.4f")
    f3 = lambda v: f"{v:.4f}"
    with open(os.path.join(out, "RESULTS.md"), "w") as fh:
        fh.write(f"# mech_eval {spec}\n\ncalibration {len(cal)} train fights; scored {dict(Counter(d['split']))}; base check max |diff| {worst:.2e}\n\n")
        fh.write("## Scores\n\n```\n" + sc.to_string(index=False, float_format=f3) + "\n```\n\n")
        fh.write("## Bootstrap (a - b, positive = b better)\n\n```\n" + bs[bs["metric"].isin(["logloss", "brier", "crps"])].to_string(index=False, float_format=f3) + "\n```\n\n")
        fh.write("## Loss checks (calibrated predicted vs actual loss shares; side = the one with the feature)\n\n```\n" + mc.to_string(index=False, float_format=f3) + "\n```\n\n")
        fh.write("## Owner bias, scored splits pooled (+ = sim under-rates the side with the feature)\n\n```\n" + ob.to_string(index=False, float_format=f3) + "\n```\n")
    print(open(os.path.join(out, "RESULTS.md")).read())


if __name__ == "__main__":
    main(*sys.argv[1:6], *(int(x) for x in sys.argv[6:8]))
