#!/usr/bin/env python3
"""How often does an armed fight participant neither fire nor die, and can the snapshot tell? (exploratory)

usage: participation.py <refit-dir> [games=400]
  Train-split AI games only (random sample). Per initial armed unit of a fight (seen in the first
  10 s): fired = a shot row of its own inside [start, end + 3 s]; died = the fight's death flag.
  Bands by distance at the start to the nearest enemy over the unit's max weapon range, and by role.
"""
import os
import pickle
import random
import sys
from collections import defaultdict

import numpy as np
import pandas as pd

import combatdata
import split
import units as unitmod

TAIL = 90


def main(rdir, ngames=400):
    ud = unitmod.load(os.path.join(rdir, "unitdefs.tsv"))
    wtab = pickle.load(open(os.path.join(rdir, "wtab.pkl"), "rb"))

    def max_range(name):
        d = ud.get(name)
        if d is None:
            return 0.0
        return max((float(wtab.loc[w.name, "range"]) for w in d.weapons if w.name in wtab.index and float(wtab.loc[w.name, "damage"]) > 0), default=0.0)

    fs = pickle.load(open(os.path.join(rdir, "fights.pkl"), "rb"))
    by_series = defaultdict(list)
    for f in fs:
        if split.split_of(f["game"], f["series"]) == "train" and f["costA"] > 0 and f["costB"] > 0:
            by_series[f["series"]].append(f)
    del fs
    random.seed(3)
    rows = []
    rng_cache = {}
    for series in random.sample(sorted(by_series), min(int(ngames), len(by_series))):
        s = combatdata.load_cached(os.path.join(rdir, "cache", series + ".pkl")).get("shot")
        if s is None or len(s) == 0:
            continue
        first_shot = s.groupby("own")["f0"].apply(np.sort)
        for f in by_series[series]:
            for side, other in (("A", "B"), ("B", "A")):
                foes = [(r["x"], r["z"]) for r in f[other]]
                for r in f[side]:
                    if not r["armed"] or r["first"] > 300 or not foes:
                        continue
                    name = r["def"]
                    if name not in rng_cache:
                        rng_cache[name] = max_range(name)
                    mr = rng_cache[name]
                    d0 = min(np.hypot(r["x"] - x, r["z"] - z) for x, z in foes)
                    shots = first_shot.get(r["uid"])
                    fired = shots is not None and bool(((shots >= f["start"]) & (shots <= f["end"] + TAIL)).any())
                    dd = ud.get(name)
                    role = ("static" if r["static"] else "commander" if r["com"] else "constructor" if dd is not None and dd.builder
                            else "anti-air" if name.endswith("aa") or "aa" in name.replace("aamp", "") and name.startswith(("turretaa", "gunshipaa", "planefighter")) else "mobile")
                    rows.append({"fired": fired, "died": bool(r["died"]), "role": role, "def": name, "ratio": d0 / mr if mr > 0 else np.nan,
                                 "hpfrac": r["hp"] / max(r["hpmax"], 1.0), "nfoes": len(foes)})
    t = pd.DataFrame(rows)
    t["outcome"] = np.where(t.fired, np.where(t.died, "fired, died", "fired, lived"), np.where(t.died, "silent, died", "silent, lived"))
    print(f"{len(t)} armed initial participants in {t.shape[0] and len(set())}{min(int(ngames), len(by_series))} train games")
    print(t.outcome.value_counts(normalize=True).round(3).to_string())
    print("\nby role (share):")
    print(pd.crosstab(t.role, t.outcome, normalize="index").round(3).assign(n=t.role.value_counts()).to_string())
    t["band"] = pd.cut(t.ratio, [0, 0.5, 1.0, 1.5, 2.5, 5, np.inf], labels=["<0.5", "0.5-1", "1-1.5", "1.5-2.5", "2.5-5", ">5"])
    print("\nmobile, by start distance to nearest enemy / own max range:")
    m = t[t.role == "mobile"]
    print(pd.crosstab(m.band, m.outcome, normalize="index").round(3).assign(n=m.band.value_counts()).to_string())
    print("\nmobile defs silent-and-lived most often (n >= 300):")
    g = m.groupby("def").agg(n=("fired", "size"), silent_lived=("outcome", lambda o: np.mean(o == "silent, lived")))
    print(g[g.n >= 300].sort_values("silent_lived", ascending=False).head(12).to_string(float_format=lambda v: f"{v:.3f}"))


if __name__ == "__main__":
    main(*sys.argv[1:3])
