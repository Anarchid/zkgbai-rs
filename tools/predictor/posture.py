#!/usr/bin/env python3
"""Posture labels for fights (round 3 predeclaration input): what each side did.

Per closed fight (10 s window) and side, from the unit rows (1 s cadence while a
unit is in combat) of its initial units:
  For each unit: d(t) = distance to the enemy side's centroid at the fight start.
  A surviving unit WITHDREW if its last logged position within the fight (end + 3 s)
  is farther from the enemy centroid than its closest approach by >= WITHDRAW elmos
  AND beyond its own max weapon range + 100 (it left the engagement envelope); its
  withdrawal time is the first second after its closest approach at which d(t) has
  grown by WITHDRAW.
  Side labels (armed initial units only):
    wiped           all armed units died
    commit          < 20% of the side's armed units withdrew (survivors stayed in or died)
    retreat-whole   >= 80% of surviving armed units withdrew within SYNC seconds of each other
    retreat-piece   otherwise (withdrawals spread over > SYNC s, or 20-80% withdrew)
    unresolved      no unit row within the fight for the side (cannot tell)
  Timing: first withdrawal relative to the side's first loss (before / after / no loss).
usage: posture.py <cache-dir> <fights.pkl> <out.tsv>
"""
import math
import os
import pickle
import sys
from collections import Counter
from multiprocessing import Pool

import numpy as np

import combatdata
import split
import units as unitmod

WITHDRAW = 200.0   # elmos of ground given up after the closest approach
SYNC = 5           # seconds: withdrawals within this of each other are "together"
TAIL = 90          # frames after the last event still part of the fight
UD = None
FIGHTS = None


def _init(fights_path):
    global UD, FIGHTS
    UD = unitmod.load()
    fights = pickle.load(open(fights_path, "rb"))
    FIGHTS = {}
    for f in fights:
        if f["closed"] and f["costA"] >= 100 and f["costB"] >= 100:
            FIGHTS.setdefault(f["series"], []).append(f)


def side_posture(side, enemy_c, uidx, start, end):
    armed = [r for r in side if r["armed"]]
    if not armed:
        return "noarmed", 0, 0, 0, None, None
    dead = [r for r in armed if r["died"]]
    alive = [r for r in armed if not r["died"]]
    if not alive:
        return "wiped", len(armed), len(dead), 0, None, None
    first_loss = min((r["kill_f"] for r in dead), default=None)
    withdrew = []
    seen = 0
    for r in alive:
        tr = uidx.get(r["uid"])
        if tr is None:
            continue
        f, x, z = tr
        m = (f >= start) & (f <= end + TAIL)
        if not m.any():
            continue
        seen += 1
        f, x, z = f[m], x[m], z[m]
        d = np.hypot(x - enemy_c[0], z - enemy_c[1])
        imin = int(np.argmin(d))
        dmin = d[imin]
        ud = UD.get(r["def"])
        rng = ud.max_range if ud is not None else 0.0
        grown = np.flatnonzero(d[imin:] - dmin >= WITHDRAW)
        if len(grown) and d[-1] - dmin >= WITHDRAW and d[-1] > rng + 100:
            withdrew.append(int(f[imin + grown[0]]) - start)
    if seen == 0:
        return "unresolved", len(armed), len(dead), 0, first_loss, None
    share = len(withdrew) / len(alive)
    tw = min(withdrew) if withdrew else None
    if share < 0.2:
        lab = "commit"
    elif share >= 0.8 and (max(withdrew) - min(withdrew)) <= SYNC * 30:
        lab = "retreat-whole"
    else:
        lab = "retreat-piece"
    return lab, len(armed), len(dead), len(withdrew), first_loss, tw


def unit_tracks(d):
    u = d["unit"]
    out = {}
    if not len(u):
        return out
    order = np.lexsort((u["f"].values, u["uid"].values))
    uid = u["uid"].values[order]; f = u["f"].values[order]; x = u["x"].values[order]; z = u["z"].values[order]
    cuts = np.flatnonzero(np.diff(uid)) + 1
    for s0, e0 in zip(np.concatenate([[0], cuts]), np.concatenate([cuts, [len(uid)]])):
        out[int(uid[s0])] = (f[s0:e0], x[s0:e0], z[s0:e0])
    return out


def game(path):
    series = os.path.basename(path)[:-4]
    fights = FIGHTS.get(series)
    if not fights:
        return []
    d = combatdata.load_cached(path)
    uidx = unit_tracks(d)
    rows = []
    for fi in fights:
        def cent(side):
            xs = [(r["x"], r["z"]) for r in side if not math.isnan(r["x"])]
            return (sum(a for a, _ in xs) / len(xs), sum(b for _, b in xs) / len(xs)) if xs else None
        ca, cb = cent(fi["A"]), cent(fi["B"])
        if ca is None or cb is None:
            continue
        pa = side_posture(fi["A"], cb, uidx, fi["start"], fi["end"])
        pb = side_posture(fi["B"], ca, uidx, fi["start"], fi["end"])
        def timing(p):
            lab, n, nd, nw, fl, tw = p
            if tw is None:
                return "none"
            if fl is None:
                return "no-loss"
            return "before-loss" if tw < fl else "after-loss"
        rows.append({"game": fi["game"], "fid": fi["fid"], "series": series, "split": split.split_of(fi["game"], series),
                     "nA": pa[1], "nB": pb[1], "postureA": pa[0], "postureB": pb[0], "timingA": timing(pa), "timingB": timing(pb),
                     "withdrewA": pa[3], "withdrewB": pb[3], "yA": fi["yA"], "yB": fi["yB"], "dur_s": (fi["end"] - fi["start"]) / 30.0,
                     "multi": int(pa[1] >= 3 and pb[1] >= 3)})
    return rows


def main(cache, fights_path, out):
    import pandas as pd
    paths = list(combatdata.cached(cache))
    rows = []
    with Pool(4, initializer=_init, initargs=(fights_path,)) as pool:
        for r in pool.imap_unordered(game, paths, chunksize=8):
            rows.extend(r)
    df = pd.DataFrame(rows)
    df.to_csv(out, sep="\t", index=False, float_format="%.3f")
    print(len(df), "fights labelled")
    for col in ("postureA", "postureB"):
        print(col, df[col].value_counts().to_dict())
    print("pair (A, B):")
    print(pd.crosstab(df.postureA, df.postureB))
    m = df[df.multi == 1]
    print("multi-unit (>=3 armed/side):", len(m)); print(pd.crosstab(m.postureA, m.postureB))
    print("timing A:", df.timingA.value_counts().to_dict(), " B:", df.timingB.value_counts().to_dict())
    print("mean yA by postureA:", df.groupby("postureA").yA.mean().round(3).to_dict())
    print("mean yB by postureA (enemy losses):", df.groupby("postureA").yB.mean().round(3).to_dict())


if __name__ == "__main__":
    main(sys.argv[1], sys.argv[2], sys.argv[3])
