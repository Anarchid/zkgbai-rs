#!/usr/bin/env python3
"""Short range and own units in the line of fire as predictors of a silent participant (exploratory).

usage: blocking.py <refit-dir> [games=400]
  Train-split AI games (same sample as participation.py). Mobile armed units seen in a fight's
  first 10 s, starting 0.5-1.5x their max range from the nearest enemy. Blockers = own units whose
  position projects onto the segment unit -> nearest enemy (between 5% and 95% of it) within
  their radius + 8 elmos of the line. Silent = no shot row of the unit inside the fight window.
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
    info = {}

    def main_weapon(name):
        if name not in info:
            d = ud.get(name)
            ws = [(float(wtab.loc[w.name, "range"]), str(wtab.loc[w.name, "type"]), w.name) for w in (d.weapons if d else [])
                  if w.name in wtab.index and float(wtab.loc[w.name, "damage"]) > 0]
            info[name] = max(ws) if ws else (0.0, "", "")
        return info[name]

    def radius(name):
        d = ud.get(name)
        return float(getattr(d, "radius", 0) or 12.0) if d is not None else 12.0

    fs = pickle.load(open(os.path.join(rdir, "fights.pkl"), "rb"))
    by_series = defaultdict(list)
    for f in fs:
        if split.split_of(f["game"], f["series"]) == "train" and f["costA"] > 0 and f["costB"] > 0:
            by_series[f["series"]].append(f)
    del fs
    random.seed(3)
    rows = []
    for series in random.sample(sorted(by_series), min(int(ngames), len(by_series))):
        s = combatdata.load_cached(os.path.join(rdir, "cache", series + ".pkl")).get("shot")
        if s is None or len(s) == 0:
            continue
        shots = s.groupby("own")["f0"].apply(np.sort)
        wshots = s.groupby(["own", "wd"], observed=True)[["f0", "n"]].apply(lambda g: (g["f0"].values, g["n"].values))
        for f in by_series[series]:
            for side, other in (("A", "B"), ("B", "A")):
                foes = np.array([(r["x"], r["z"]) for r in f[other]])
                if len(foes) == 0:
                    continue
                own = f[side]
                for r in own:
                    if not r["armed"] or r["static"] or r["com"] or r["first"] > 300:
                        continue
                    mr, wtype, wname = main_weapon(r["def"])
                    if mr <= 0:
                        continue
                    dist = np.hypot(foes[:, 0] - r["x"], foes[:, 1] - r["z"])
                    j = int(dist.argmin())
                    ratio = dist[j] / mr
                    if not 0.5 <= ratio <= 1.5:
                        continue
                    ex, ez = foes[j] - (r["x"], r["z"])
                    l2 = ex * ex + ez * ez
                    nblock = 0
                    crowd = 0
                    for o in own:
                        if o is r:
                            continue
                        ox, oz = o["x"] - r["x"], o["z"] - r["z"]
                        if ox * ox + oz * oz < 100.0 ** 2:
                            crowd += 1
                        t = (ox * ex + oz * ez) / l2 if l2 > 0 else -1
                        if 0.05 < t < 0.95 and abs(ox * ez - oz * ex) / np.sqrt(l2) < radius(o["def"]) + 8.0:
                            nblock += 1
                    sh = shots.get(r["uid"])
                    fired = sh is not None and bool(((sh >= f["start"]) & (sh <= f["end"] + TAIL)).any())
                    ws_ = wshots.get((r["uid"], wname))
                    nshot = float(ws_[1][(ws_[0] >= f["start"]) & (ws_[0] <= f["end"] + TAIL)].sum()) if ws_ is not None else 0.0
                    w = wtab.loc[wname]
                    cap = (f["end"] + TAIL - f["start"]) / 30.0 / max(float(w["reload"]), 0.05) * max(int(w["salvo"]), 1)
                    rows.append({"rate": min(nshot / max(cap, 1e-9), 2.0), "silent": not fired, "blockers": min(nblock, 3), "crowd": min(crowd, 8), "range": mr,
                                 "short": "range < 300" if mr < 300 else "range >= 300", "wtype": wtype, "def": r["def"]})
    t = pd.DataFrame(rows)
    print(f"{len(t)} mobile units starting 0.5-1.5x range from the nearest enemy; silent {t.silent.mean():.3f}")
    print("\nsilent share by blockers in the line (0..3+) and range class:")
    print(t.pivot_table(index="blockers", columns="short", values="silent", aggfunc=["mean", "size"]).round(3).to_string())
    print("\nsilent share by blockers and weapon type (types with n >= 2000):")
    big = t.wtype.value_counts(); big = big[big >= 2000].index
    print(t[t.wtype.isin(big)].pivot_table(index="blockers", columns="wtype", values="silent", aggfunc="mean").round(3).to_string())
    print("\nrealized fire rate / reload-limited max (units that fired), by blockers and range class:")
    print(t[~t.silent].pivot_table(index="blockers", columns="short", values="rate", aggfunc="median").round(3).to_string())
    print(t[~t.silent].pivot_table(index="blockers", columns="short", values="rate", aggfunc="mean").round(3).to_string())
    print("\nsilent share by own units within 100 elmos (crowding), no blocker in the line:")
    print(t[t.blockers == 0].groupby("crowd").silent.agg(["mean", "size"]).round(3).T.to_string())


if __name__ == "__main__":
    main(*sys.argv[1:3])
