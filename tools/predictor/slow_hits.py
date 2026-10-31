#!/usr/bin/env python3
"""Hit rates of slow weapons inferred from the target's slow state (mechanics round 6).

usage: slow_hits.py <cache-dir> <unitdefs.tsv> <out.pkl> [workers]

Slow-only weapons (timeslow_onlyslow: Cyclops and commander slowbeams) deal no damage, so the
combat log never matches a hit to their shots (13k Cyclops slowbeam shots, 0 hits). A hit shows as
a jump in the target's slowState (unit_timeslow.lua: slow damage / current health, capped at 0.5 +
overslow). Readings of a unit's slow: unit rows (every 30 frames) and every shot row's tslow, timed
at f0 + lag (the widget reads the target when it sees the shot: after a hitscan beam landed). For
each shot at a unit: before = the last reading strictly before f0 (within 45 frames); after = the
first reading within 45 frames after the shot's window closes; expected jump J = min(cap,
before + slow per hit / thp) - before. Shots with J < 0.1 (near the cap, or too much health to
show it), no reading, or a dead target are dropped. A shot hits when after - before >= J / 2.

Rows are written for the slow-only weapons (cells in hitcells.pkl format, train games only, so
predict.HitModel can be rebuilt with them) and for check weapons: the same rule on the Moderator
(a damaging slow beam: inferred vs logged hit rate on the same shots) and, as a false-positive
base rate, on weapons that do not slow fired at the same targets (the Cyclops cannon).
Output: {"rows": per-shot DataFrame (game split label, fold, weapon, target, rb, sb, J, jump,
hit, logged hit), "slow_per_hit": {weapon: slow per hit}}. Aggregate with --cells.
"""
import os
import pickle
import sys
from multiprocessing import Pool

import numpy as np
import pandas as pd

import combatdata
import split

RBANDS = 5
SPEED_EDGES = (0.5, 2.0, 4.0)
WINDOW = 45
MIN_JUMP = 0.1
MAX_SLOW = 0.5
CHECK = {"jumpskirm_disruptor_beam", "tankheavyassault_cor_gol"}

SLOW = None


def slow_table(udpath):
    """weapon -> (slow per full hit, slow-only?, cap bonus, damage per hit in the dump) from the fixed unit-def dump."""
    import csv
    out = {}
    with open(udpath) as fh:
        for r in csv.DictReader(fh, delimiter="\t"):
            dmg = {}
            for item in (r["weapons"] or "").split(","):
                p = item.split(":")
                if len(p) > 4:
                    try:
                        dmg[p[0]] = float(p[4])
                    except ValueError:
                        pass
            for item in (r["wextra"] or "").split(","):
                p = item.split(":")
                if len(p) < 12:
                    continue
                name, factor, only, absol, ovs = p[0], p[7], p[9], p[10], p[11]
                base = dmg.get(name)
                if base is None:
                    continue
                per_hit = float(absol) if absol else (float(factor) if factor else (1.0 if only else 0.0)) * base
                if per_hit > 0:
                    out[name] = (per_hit, bool(only), (float(ovs) * 0.04 / 30.0) if ovs else 0.0, base)
    return out


def game_rows(path):
    d = combatdata.load_cached(path)
    meta = d["meta"]
    series = os.path.basename(path)[:-4]
    game = meta.get("game", "")
    sp = split.split_of(game, series)
    fold = split.fold(game)
    s = d.get("shot")
    u = d.get("unit")
    if s is None or not len(s) or u is None or not len(u):
        return None
    s = s[(s["tt"] == "u") & (s["tid"] >= 0) & (s["tteam"] != s["oteam"])]
    wd = s["wd"].astype(str)
    want = wd.isin(set(SLOW) | CHECK)
    s = s[want]
    if not len(s):
        return None
    w = meta["weapons"]
    # slow readings per unit: unit rows and every shot row's tslow (all weapons)
    allshot = d["shot"]
    allshot = allshot[(allshot["tt"] == "u") & (allshot["tid"] >= 0)]
    obs = pd.concat([
        pd.DataFrame({"uid": u["uid"].values, "f": u["f"].values, "slow": u["slow"].values.astype(float)}),
        # a shot row's target state is read when the widget sees the shot (fire frame + lag), which for a
        # hitscan weapon is after the beam already landed
        pd.DataFrame({"uid": allshot["tid"].values, "f": (allshot["f0"] + allshot["lag"].fillna(0)).values,
                      "slow": allshot["tslow"].values.astype(float)}),
    ]).dropna().sort_values(["uid", "f"])
    by = {uid: (g["f"].values, g["slow"].values) for uid, g in obs.groupby("uid")}
    rows = []
    wdv = s["wd"].astype(str).values
    f0 = s["f0"].values; f1 = s["f1"].values
    tid = s["tid"].values; tdef = s["tdef"].astype(str).values
    thp = s["thp"].values.astype(float)
    lag = s["lag"].fillna(0).values.astype(float)
    tdead = s["tdead"].values if "tdead" in s.columns else np.zeros(len(s))
    tspd = s["tspd"].values.astype(float); tdist = s["tdist"].values.astype(float)
    dmgt = s["dmgt"].values.astype(float)
    n = s["n"].values.astype(float)
    for i in range(len(s)):
        name = wdv[i]
        if tdead[i] == 1 or not (thp[i] > 0):
            continue
        r = by.get(tid[i])
        if r is None:
            continue
        fr, sl = r
        # before: the last reading strictly before the fire frame (within WINDOW)
        k0 = np.searchsorted(fr, f0[i], side="left") - 1
        if k0 < 0 or fr[k0] < f0[i] - WINDOW:
            continue
        before = sl[k0]
        per_hit, only, ovs, _ = SLOW.get(name, (0.0, False, 0.0, 0.0))
        if name in CHECK and name not in SLOW:
            per_hit, ovs = 2000.0, 0.0  # pretend: a slowbeam-sized jump, for the false-positive base rate
        cap = MAX_SLOW + ovs
        J = min(cap, before + per_hit / thp[i]) - before
        if J < MIN_JUMP:
            continue
        # after: the first reading after the shot's window closes (and after its own detection)
        end = max(f0[i] + lag[i], f1[i] if not np.isnan(f1[i]) else f0[i])
        k = np.searchsorted(fr, end, side="right")
        if k >= len(fr) or fr[k] > end + WINDOW:
            continue
        jump = sl[k] - before
        rng = float(w["range"].get(name, np.nan)) if hasattr(w["range"], "get") else np.nan
        rb = int(np.clip(np.floor(RBANDS * tdist[i] / rng), 0, RBANDS)) if rng > 0 and np.isfinite(tdist[i]) else RBANDS
        sb = int(np.searchsorted(np.array(SPEED_EDGES), tspd[i], side="right"))
        rows.append((sp, fold, name, tdef[i], rb, sb, J, jump, float(jump >= J / 2.0), float(dmgt[i] > 0), n[i]))
    if not rows:
        return None
    return pd.DataFrame(rows, columns=["split", "fold", "wd", "tdef", "rb", "sb", "J", "jump", "hit", "logged", "n"])


def init(udpath):
    global SLOW
    SLOW = slow_table(udpath)


def cells(rows, dev_fold=0):
    """hitcells.pkl-format cells for the slow-only weapons from train rows (minus the dev fold)."""
    r = rows[(rows["split"] == "train") & (rows["only"])]
    if dev_fold:
        r = r[r["fold"] != dev_fold]
    g = r.groupby(["wd", "tdef", "rb", "sb"]).agg(n=("hit", "size"), hit_any=("hit", "sum"))
    g["hit_tgt"] = g["hit_any"]
    g["dmg"] = 0.0; g["dmgt"] = 0.0; g["nominal"] = 0.0; g["para"] = 0.0; g["tstatic"] = 0.0
    return g


def main(cache, udpath, out, workers=2):
    init(udpath)
    paths = list(combatdata.cached(cache))
    with Pool(workers, initializer=init, initargs=(udpath,)) as pool:
        parts = [p for p in pool.imap_unordered(game_rows, paths, chunksize=8) if p is not None]
    rows = pd.concat(parts, ignore_index=True)
    rows["only"] = rows["wd"].map(lambda x: SLOW.get(x, (0, False, 0, 0))[1])
    pickle.dump({"rows": rows, "slow_per_hit": SLOW}, open(out, "wb"))
    print(f"{len(paths)} games, {len(rows)} informative shots -> {out}")
    report(rows)


def report(rows):
    print("\ninferred slow hits (all splits) vs logged damage hits on the same shots:")
    t = rows.groupby("wd").agg(shots=("hit", "size"), inferred=("hit", "mean"), logged=("logged", "mean"),
                               J=("J", "mean"), jump=("jump", "mean"))
    print(t.sort_values("shots", ascending=False).to_string(float_format=lambda v: f"{v:.3f}"))
    print("\nby target speed band:")
    print(rows.groupby(["wd", "sb"]).agg(shots=("hit", "size"), inferred=("hit", "mean"), logged=("logged", "mean"))
          .to_string(float_format=lambda v: f"{v:.3f}"))


if __name__ == "__main__":
    if sys.argv[1] == "--report":
        report(pickle.load(open(sys.argv[2], "rb"))["rows"])
    else:
        main(sys.argv[1], sys.argv[2], sys.argv[3], int(sys.argv[4]) if len(sys.argv) > 4 else 2)
