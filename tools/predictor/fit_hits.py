#!/usr/bin/env python3
"""Fit per-weapon hit rates and damage per hit from the training games.

usage: fit_hits.py <cache-dir> <out.pkl> [workers]
Tables: keyed by (weapon, target def, range band, speed band) with shrinkage
towards (weapon, target def), (weapon, target size class) and (weapon).
"""
import os
import pickle
import sys
from collections import defaultdict
from multiprocessing import Pool

import numpy as np
import pandas as pd

import combatdata
import split
import units as unitmod

RBANDS = 5       # distance / range in fifths; band 5 = beyond nominal range
SPEED_EDGES = (0.5, 2.0, 4.0)  # elmos per frame -> bands 0..3
SHRINK = 20.0

UD = None
NOSHIELD = os.environ.get("PRED_HITS_NOSHIELD") == "1"
# PRED_HITS_NOBUBBLE=<shield.json>: body hits only, also dropping shots at targets inside a charged
# bubble (> 50) of a shield unit of their team with the shooter outside it (mechanics round 4)
NOBUBBLE = os.environ.get("PRED_HITS_NOBUBBLE")
SHIELD_R = None


def covered(d, s):
    """Mask over shot rows s: target inside a charged ally bubble at fire time, shooter outside."""
    global SHIELD_R
    if SHIELD_R is None:
        import json
        SHIELD_R = {k: v["radius"] for k, v in json.load(open(NOBUBBLE)).items() if not k.startswith("_")}
    u = d.get("unit")
    out = np.zeros(len(s), bool)
    if u is None or not len(u) or "sh" not in u.columns:
        return out
    u = u[u["udef"].astype(str).isin(SHIELD_R.keys())]
    f0 = s["f0"].values; tt = s["tteam"].values
    tx = s["tx"].values.astype(float); tz = s["tz"].values.astype(float)
    ox = s["ox"].values.astype(float); oz = s["oz"].values.astype(float)
    for (uid, udef, team), g in u.sort_values("f").groupby(["uid", u["udef"].astype(str), "uteam"]):
        rad = SHIELD_R[udef]
        fr = g["f"].values
        i = np.clip(np.searchsorted(fr, f0), 0, len(fr) - 1)
        j = np.clip(i - 1, 0, len(fr) - 1)
        k = np.where(np.abs(fr[j] - f0) < np.abs(fr[i] - f0), j, i)
        near = np.abs(fr[k] - f0) <= 30
        sx = g["x"].values[k].astype(float); sz = g["z"].values[k].astype(float); sh = g["sh"].values[k].astype(float)
        inside = np.hypot(tx - sx, tz - sz) <= rad
        shooter_out = np.hypot(ox - sx, oz - sz) > rad
        out |= near & (tt == team) & (sh > 50) & inside & shooter_out
    return out


def sband(spd):
    return np.searchsorted(np.array(SPEED_EDGES), spd, side="right")


def game_counts(path):
    """Per-cell sums for one game: n, hit_any, hit_target, dmg, dmgt, nominal."""
    global UD
    if UD is None:
        UD = unitmod.load()
    d = combatdata.load_cached(path)
    meta = d["meta"]
    series = os.path.basename(path)[:-4]
    sp = split.split_of(meta.get("game", ""), series)
    s = d["shot"]
    s = s[(s["tt"] == "u") & (s["tid"] >= 0) & (s["tteam"] != s["oteam"])]
    if NOSHIELD and "tsh" in s.columns:
        s = s[~(s["tsh"] > 50)]  # body hits only: the target's own shield was down (mech.py shield switch)
    if NOBUBBLE:
        if "tsh" in s.columns:
            s = s[~(s["tsh"] > 50)]
        s = s[~covered(d, s)]
    if not len(s):
        return sp, None
    w = meta["weapons"]
    wd = s["wd"].astype(str)
    rng = wd.map(w["range"]).astype(float).values
    dmg_nom = wd.map(w["damage"]).astype(float).values
    salvo = wd.map(w["salvo"]).astype(float).fillna(1).values
    n = s["n"].values.astype(float)
    src = s["src"].astype(str).values
    hitscan = src == "s"
    nominal = np.where(hitscan, dmg_nom * salvo * n, dmg_nom * np.maximum(salvo, 1))
    # projectile rows are one projectile each: a salvo of k projectiles is k rows
    nominal = np.where(hitscan, nominal, dmg_nom)
    with np.errstate(divide="ignore", invalid="ignore"):
        rb = np.where(rng > 0, np.floor(RBANDS * s["tdist"].values / rng), RBANDS).astype(int)
    rb = np.clip(rb, 0, RBANDS)
    sb = sband(s["tspd"].values)
    dmg = s["dmg"].values.astype(float); dmgt = s["dmgt"].values.astype(float)
    hits = s["hits"].values.astype(float)
    # hit indicator: projectiles -> hit any unit; hitscan -> landed share of nominal (capped at n)
    hit_any = np.where(hitscan, np.minimum(n, np.where(nominal > 0, dmg / np.maximum(nominal / np.maximum(n, 1), 1e-9), 0.0)), (hits > 0).astype(float))
    hit_tgt = np.where(hitscan, np.minimum(n, np.where(nominal > 0, dmgt / np.maximum(nominal / np.maximum(n, 1), 1e-9), 0.0)), (dmgt > 0).astype(float))
    df = pd.DataFrame({
        "wd": wd.values, "tdef": s["tdef"].astype(str).values, "rb": rb, "sb": sb,
        "n": n, "hit_any": hit_any, "hit_tgt": hit_tgt, "dmg": dmg, "dmgt": dmgt, "nominal": nominal,
        "para": s["para"].values.astype(float), "tstatic": np.zeros(len(s)),
    })
    g = df.groupby(["wd", "tdef", "rb", "sb"], sort=False).sum(numeric_only=True)
    return sp, g


class HitModel:
    """p_hit(weapon, target def, range band, speed band) with hierarchical shrinkage."""

    def __init__(self, cells, ud):
        self.cells = cells            # DataFrame indexed by (wd, tdef, rb, sb)
        self.ud = ud
        c = cells.reset_index()
        c["cls"] = [self._cls(t) for t in c["tdef"]]
        self.by_wd = c.groupby("wd").sum(numeric_only=True)
        self.by_wd_cls = c.groupby(["wd", "cls"]).sum(numeric_only=True)
        self.by_wd_tdef = c.groupby(["wd", "tdef"]).sum(numeric_only=True)
        self.by_wd_tdef_rb = c.groupby(["wd", "tdef", "rb"]).sum(numeric_only=True)
        self.cache = {}

    def _cls(self, tdef):
        d = self.ud.get(tdef)
        return d.size_class() if d is not None else "medium"

    @staticmethod
    def _rate(row, prior, k=SHRINK):
        n = row["n"]
        return (row["hit_any"] + k * prior) / (n + k), (row["hit_tgt"] + k * prior) / (n + k)

    def lookup(self, wd, tdef, rb, sb):
        key = (wd, tdef, int(rb), int(sb))
        r = self.cache.get(key)
        if r is not None:
            return r
        p_any = p_tgt = 0.7
        dmg_frac = 1.0
        tot_n = 0.0
        if wd in self.by_wd.index:
            row = self.by_wd.loc[wd]
            p_any, p_tgt = self._rate(row, 0.7)
            tot_n = row["n"]
            if row["hit_any"] > 0 and row["nominal"] > 0:
                # damage landed per hit relative to nominal per shot
                dmg_frac = float(row["dmg"] / max(row["hit_any"], 1e-9) / (row["nominal"] / max(row["n"], 1e-9)))
            cls = self._cls(tdef)
            if (wd, cls) in self.by_wd_cls.index:
                row = self.by_wd_cls.loc[(wd, cls)]
                p_any, p_tgt = self._rate(row, p_any)
            if (wd, tdef) in self.by_wd_tdef.index:
                row = self.by_wd_tdef.loc[(wd, tdef)]
                p_any, p_tgt = self._rate(row, p_any)
                if row["hit_any"] > 5 and row["nominal"] > 0:
                    dmg_frac = float(row["dmg"] / max(row["hit_any"], 1e-9) / (row["nominal"] / max(row["n"], 1e-9)))
                if (wd, tdef, rb) in self.by_wd_tdef_rb.index:
                    row = self.by_wd_tdef_rb.loc[(wd, tdef, rb)]
                    p_any, p_tgt = self._rate(row, p_any)
                    if (wd, tdef, rb, sb) in self.cells.index:
                        row = self.cells.loc[(wd, tdef, rb, sb)]
                        p_any, p_tgt = self._rate(row, p_any)
        r = (float(min(p_any, 1.0)), float(min(p_tgt, 1.0)), dmg_frac, tot_n)
        self.cache[key] = r
        return r


def main(cache, out, workers=4):
    paths = list(combatdata.cached(cache))
    parts = {"train": [], "test": [], "human": [], "replay": [], "dev": []}
    with Pool(workers) as pool:
        for i, (sp, g) in enumerate(pool.imap_unordered(game_counts, paths, chunksize=4)):
            if g is not None:
                parts[sp].append(g)
            if (i + 1) % 200 == 0:
                print(f"{i + 1}/{len(paths)}", flush=True)
    tables = {}
    for sp, gs in parts.items():
        if gs:
            tables[sp] = pd.concat(gs).groupby(level=[0, 1, 2, 3]).sum()
        print(sp, "cells", len(tables.get(sp, ())), "shots", float(tables[sp]["n"].sum()) if sp in tables else 0)
    with open(out, "wb") as fh:
        pickle.dump(tables, fh, protocol=5)
    print("->", out)


if __name__ == "__main__":
    main(sys.argv[1], sys.argv[2], int(sys.argv[3]) if len(sys.argv) > 3 else 4)
