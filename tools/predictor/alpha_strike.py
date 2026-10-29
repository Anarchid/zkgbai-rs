#!/usr/bin/env python3
"""Does the sim over-rate packs because it gives them a perfect alpha strike? (exploratory)

usage: alpha_strike.py <pdir> <fights.pkl> <fights_pred.tsv> <coef.tsv> <out.tsv> [workers]
  <pdir> holds hitcells.pkl, unitdefs.tsv, wtab.pkl, commw.pkl, cache/ (refit1/empcap);
  PRED_HAZARD must name the hazard the predictions were made with.

The dps_stop sim starts every weapon ready (a pack that reaches range together fires one
synchronized volley), lands damage instantly and re-picks targets among the living per shot (no
overkill). Variants: base (as fitted); stagger (each weapon's first shot at a uniform frame in
[0, reload), mean over 8 seeded draws); simul (a step's damage lands after every unit fired:
overkill within a step); stagger+simul; desync S (within each side the first unit fires on
arrival and the others U(0, S) later, so a lone unit keeps its loaded first shot). Each variant goes through the same win calibration
(win.w* in coef.tsv). Subsets: ZKGBAI fights where one side fields >= 2 / exactly 1 of a raider
against <= 3 armed, no fire unit on the other side, no statics; Dagger and Glaive (control:
0.3 s reload, near-continuous fire).
"""
import hashlib
import os
import pickle
import re
import sys
from multiprocessing import Pool

import numpy as np
import pandas as pd
from scipy.special import expit

import predict

K = 8
FIRE = re.compile(r"(?:^| )(?:tankraid|jumpraid|jumparty)x")


def count(comp, unit):
    m = re.search(rf"(?:^| ){unit}x(\d+)", comp if isinstance(comp, str) else "")
    return int(m.group(1)) if m else 0


def run(args):
    fight, = args
    units = predict.build_units(fight)
    hz = predict.G["hazard"]
    out = {}
    seed = int(hashlib.md5(f"{fight['game']}:{fight['fid']}".encode()).hexdigest(), 16) % (2 ** 32)
    for name, stagger, simul in (("base", False, False), ("simul", False, True), ("stagger", True, False), ("stagger+simul", True, True)):
        predict.SIMUL = simul
        draws = []
        for k in range(K if stagger else 1):
            rng = np.random.default_rng(seed + k)
            predict.FIRST_SHOT = (lambda u, w: int(rng.integers(0, w.reload))) if stagger else None
            draws.append(predict.simulate(units, None, hz))
        out[name] = np.mean(draws, axis=0)
    # desync S: within each side the first unit fires on arrival, the others U(0, S) later
    for S in (30, 60, 90):
        draws = []
        for k in range(K):
            rng = np.random.default_rng(seed + 100 + k)
            off = {}
            for s in (0, 1):
                side = [u for u in units if u.side == s]
                o = rng.integers(0, S, len(side))
                o = o - (o.min() if len(o) else 0)
                for u, v in zip(side, o):
                    off[(u.side, u.x, u.def_, u.hp)] = int(v)
            predict.FIRST_SHOT = lambda u, w: off.get((u.side, u.x, u.def_, u.hp), 0)
            draws.append(predict.simulate(units, None, hz))
        out[f"desync {S // 30}s"] = np.mean(draws, axis=0)
    predict.SIMUL = False
    predict.FIRST_SHOT = None
    return (fight["game"], fight["fid"]), out


def init(pdir):
    predict._init(pdir, os.path.join(pdir, "cache"))


def main(pdir, fights_path, pred_path, coef_path, out, workers=3):
    kv = {l.split("\t")[0]: float(l.split("\t")[1]) for l in open(coef_path) if l.startswith("win.")}
    w0, w1 = kv["win.w0"], kv["win.w1"]
    d = pd.read_csv(pred_path, sep="\t", low_memory=False,
                    usecols=["game", "fid", "split", "compA", "compB", "nA", "nB", "win", "dstop_A", "dstop_B", "dps_stop_pAwin"])
    d = d[(d.win >= 0) & d.split.isin(["train", "test"])]
    fs = pickle.load(open(fights_path, "rb"))
    byk = {(f["game"], f["fid"]): f for f in fs}
    del fs
    rows = []
    for unit, label in (("hoverraid", "Dagger"), ("cloakraid", "Glaive")):
        cA, cB = d.compA.map(lambda c: count(c, unit)), d.compB.map(lambda c: count(c, unit))
        for i in np.where(((cA > 0) ^ (cB > 0)).values)[0]:
            r = d.iloc[i]
            f = byk.get((r.game, r.fid))
            if f is None:
                continue
            a = cA.iloc[i] > 0
            owner = f["nameA"] if a else f["nameB"]
            n_own, n_opp = (cA.iloc[i], r.nB) if a else (cB.iloc[i], r.nA)
            opp = r.compB if a else r.compA
            static = any(u["static"] for u in f["A"] + f["B"])
            if "ZKGBAI" not in owner or n_opp > 3 or static or FIRE.search(opp if isinstance(opp, str) else ""):
                continue
            rows.append({"key": (r.game, r.fid), "game": r.game, "unit": label, "pack": "2+" if n_own >= 2 else "1",
                         "owner_A": a, "won": r.win if a else 1 - r.win, "check_A": r.dstop_A, "check_B": r.dstop_B})
    sel = pd.DataFrame(rows)
    print(sel.groupby(["unit", "pack"]).size().to_string(), flush=True)
    with Pool(workers, initializer=init, initargs=(pdir,)) as pool:
        res = dict(pool.imap(run, [(byk[k],) for k in sel["key"]], chunksize=8))
    out_rows = []
    for _, s in sel.iterrows():
        o = res[s["key"]]
        for v, (kA, kB) in o.items():
            p = expit(w0 + w1 * (kB - kA))
            out_rows.append({"game": s.game, "unit": s.unit, "pack": s.pack, "variant": v, "won": s.won,
                             "p": p if s.owner_A else 1 - p, "check": abs(kA - s.check_A) + abs(kB - s.check_B) if v == "base" else np.nan})
    o = pd.DataFrame(out_rows)
    o.to_csv(out, sep="\t", index=False, float_format="%.5f")
    print("base vs fights_pred, max |dstop diff|:", o["check"].max())
    rng = np.random.default_rng(7)
    rep = []
    for k, s in o.groupby(["unit", "pack", "variant"], sort=False):
        r = (s.won - s.p).values
        g, gi = np.unique(s.game.values, return_inverse=True)
        W = np.stack([np.bincount(rng.integers(0, len(g), len(g)), minlength=len(g)) for _ in range(400)]).astype(float)[:, gi]
        lo, hi = np.percentile((W * r).sum(1) / W.sum(1), [2.5, 97.5])
        p = np.clip(s.p.values, 1e-6, 1 - 1e-6)
        ll = -(s.won * np.log(p) + (1 - s.won) * np.log(1 - p)).mean()
        rep.append({"unit": k[0], "pack": k[1], "variant": k[2], "n": len(s), "won": s.won.mean(), "sim_P": s.p.mean(),
                    "bias": r.mean(), "lo": lo, "hi": hi, "logloss": ll})
    print(pd.DataFrame(rep).to_string(index=False, float_format=lambda v: f"{v:.3f}"))


if __name__ == "__main__":
    main(*sys.argv[1:6], *(int(x) for x in sys.argv[6:7]))
