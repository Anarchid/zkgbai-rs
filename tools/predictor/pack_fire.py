#!/usr/bin/env python3
"""Did every unit of a pack fight? Participation and first-shot spread from the shot rows (exploratory).

usage: pack_fire.py <refit-dir> <alpha_strike.tsv-free selection: unit def> [weapon prefix]
  reads <refit-dir>/fights.pkl, empcap_out/fights_pred.tsv, cache/; ZKGBAI-owned fights where one
  side fields the unit against <= 3 armed, no fire unit on the other side, no statics (as
  alpha_strike.py). Per fight: share of the pack that fired at least once inside the fight window,
  spread between the pack members' first shots, and owner bias of dps_stop split by participation.
"""
import os
import pickle
import re
import sys
from collections import defaultdict

import numpy as np
import pandas as pd

import combatdata

FIRE = re.compile(r"(?:^| )(?:tankraid|jumpraid|jumparty)x")
TAIL = 90


def main(rdir, unit="hoverraid", wprefix=None):
    wprefix = wprefix or unit + "_"
    d = pd.read_csv(os.path.join(rdir, "empcap_out", "fights_pred.tsv"), sep="\t", low_memory=False,
                    usecols=["game", "fid", "split", "compA", "compB", "nA", "nB", "win", "dps_stop_pAwin"])
    d = d[(d.win >= 0) & d.split.isin(["train", "test"])]
    want = {(g, i): (w, p) for g, i, w, p in zip(d.game, d.fid, d.win, d.dps_stop_pAwin)}
    comp = {(g, i): (a, b, na, nb) for g, i, a, b, na, nb in zip(d.game, d.fid, d.compA, d.compB, d.nA, d.nB)}
    fs = pickle.load(open(os.path.join(rdir, "fights.pkl"), "rb"))
    by_series = defaultdict(list)
    for f in fs:
        k = (f["game"], f["fid"])
        if k not in want:
            continue
        a = [r for r in f["A"] if r["def"] == unit]
        b = [r for r in f["B"] if r["def"] == unit]
        if bool(a) == bool(b):
            continue
        own_a = bool(a)
        owner = f["nameA"] if own_a else f["nameB"]
        ca, cb, na, nb = comp[k]
        opp, n_opp = (cb, nb) if own_a else (ca, na)
        if "ZKGBAI" not in owner or n_opp > 3 or any(r["static"] for r in f["A"] + f["B"]) or FIRE.search(opp if isinstance(opp, str) else ""):
            continue
        win, p = want[k]
        by_series[f["series"]].append({"game": f["game"], "fid": f["fid"], "start": f["start"], "end": f["end"],
                                       "uids": [r["uid"] for r in (a if own_a else b)],
                                       "won": win if own_a else 1 - win, "p": p if own_a else 1 - p})
    del fs
    rows = []
    for series, fights in by_series.items():
        dd = combatdata.load_cached(os.path.join(rdir, "cache", series + ".pkl"))
        s = dd.get("shot")
        if s is None or len(s) == 0:
            continue
        s = s[s["wd"].astype(str).str.startswith(wprefix)]
        for fi in fights:
            m = s["own"].isin(fi["uids"]) & (s["f0"] >= fi["start"]) & (s["f0"] <= fi["end"] + TAIL)
            first = s[m].groupby("own")["f0"].min()
            n = len(fi["uids"])
            rows.append({"game": fi["game"], "pack": "2+" if n >= 2 else "1", "n": n, "fired": len(first),
                         "share": len(first) / n, "spread_s": (first.max() - first.min()) / 30 if len(first) >= 2 else np.nan,
                         "shots_per_unit": m.sum() / n, "won": fi["won"], "p": fi["p"]})
    r = pd.DataFrame(rows)
    print(f"{unit}: {len(r)} fights")
    for pack, g in r.groupby("pack"):
        print(f"\npack {pack}: n {len(g)}; all fired {np.mean(g.share == 1):.2f}, none fired {np.mean(g.fired == 0):.2f}, mean share fired {g.share.mean():.2f}, shots/unit {g.shots_per_unit.mean():.1f}")
        if pack == "2+":
            sp = g.spread_s.dropna()
            print(f"  first-shot spread among those that fired (>= 2): median {sp.median():.1f} s, p25 {sp.quantile(.25):.1f}, p75 {sp.quantile(.75):.1f}, p90 {sp.quantile(.9):.1f}")
        g = g.assign(part=np.where(g.share == 1, "all fired", np.where(g.fired == 0, "none fired", "some fired")))
        t = g.groupby("part").agg(n=("won", "size"), won=("won", "mean"), sim_P=("p", "mean"))
        t["bias"] = t.won - t.sim_P
        print(t.to_string(float_format=lambda v: f"{v:.3f}"))


if __name__ == "__main__":
    main(*sys.argv[1:4])
