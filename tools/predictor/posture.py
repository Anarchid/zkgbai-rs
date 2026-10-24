#!/usr/bin/env python3
"""Posture labels for fights (round 3 predeclaration input): what each side did.

Rule v2 (after the Analyst's hand check of v1, HANDCHECK.md): mobile ground
units only; denominator = every engaged mobile armed unit (died = did not
withdraw); tail end + 10 s; enemy wiped or enemy left first => commit; no rows =>
not part of the posture; fights under 2 s unresolved; whole/piece spread 10 s.
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
SYNC = 10          # seconds: withdrawals within this of each other are "together"
TAIL = 300         # frames after the last event still part of the fight (10 s)
MIN_DUR = 60       # frames: shorter fights are unresolved
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
    """-> (label, n_engaged, n_dead, n_withdrew, first_loss_f, first_withdraw_f)"""
    armed = [r for r in side if r["armed"]]
    if not armed:
        return "noarmed", 0, 0, 0, None, None
    mobile = []
    for r in armed:
        ud = UD.get(r["def"])
        if ud is None or ud.static:
            continue
        if ud.canfly:
            continue
        mobile.append(r)
    if not mobile:
        kinds = {("air" if (UD.get(r["def"]) and UD.get(r["def"]).canfly) else "static") for r in armed}
        return ("strike" if kinds == {"air"} else "static"), 0, sum(r["died"] for r in armed), 0, None, None
    if end - start < MIN_DUR:
        return "unresolved", 0, 0, 0, None, None
    engaged, withdrew = [], []
    for r in mobile:
        tr = uidx.get(r["uid"])
        has_rows = False
        if tr is not None:
            f, x, z = tr
            m = (f >= start) & (f <= end + TAIL)
            has_rows = bool(m.any())
        if not (has_rows or r["died"]):
            continue
        engaged.append(r)
        if r["died"] or not has_rows:
            continue
        f, x, z = f[m], x[m], z[m]
        d = np.hypot(x - enemy_c[0], z - enemy_c[1])
        imin = int(np.argmin(d))
        dmin = d[imin]
        ud = UD.get(r["def"])
        rng = ud.max_range if ud is not None else 0.0
        grown = np.flatnonzero(d[imin:] - dmin >= WITHDRAW)
        if len(grown) and d[-1] - dmin >= WITHDRAW and d[-1] > rng + 100:
            withdrew.append(int(f[imin + grown[0]]) - start)
    if not engaged:
        return "unresolved", 0, 0, 0, None, None
    dead = [r for r in engaged if r["died"]]
    first_loss = min((r["kill_f"] for r in dead), default=None)
    if len(dead) == len(engaged):
        return "wiped", len(engaged), len(dead), 0, first_loss, None
    share = len(withdrew) / len(engaged)
    tw = min(withdrew) if withdrew else None
    if share < 0.2:
        lab = "commit"
    elif share >= 0.8 and (max(withdrew) - min(withdrew)) <= SYNC * 30:
        lab = "retreat-whole"
    else:
        lab = "retreat-piece"
    return lab, len(engaged), len(dead), len(withdrew), first_loss, tw


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
        pa = list(side_posture(fi["A"], cb, uidx, fi["start"], fi["end"]))
        pb = list(side_posture(fi["B"], ca, uidx, fi["start"], fi["end"]))
        # rule 4: a side whose enemy was wiped, or left first, committed by definition
        def enemy_gone(own, foe, foe_y):
            if foe[0] == "wiped" or foe_y >= 0.999:  # every armed enemy unit dead, statics included
                return True
            return foe[0] in ("retreat-whole", "retreat-piece") and own[5] is not None and foe[5] is not None and foe[5] < own[5]
        oa, ob = pa[0], pb[0]
        if oa in ("retreat-whole", "retreat-piece") and enemy_gone(pa, pb, fi["yB"]):
            pa[0] = "commit"
        if ob in ("retreat-whole", "retreat-piece") and enemy_gone(pb, pa, fi["yA"]):
            pb[0] = "commit"
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
                     "multi": int(pa[1] >= 3 and pb[1] >= 3), "rawA": oa, "rawB": ob})
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
