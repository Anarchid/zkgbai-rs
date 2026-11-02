#!/usr/bin/env python3
"""Splash geometry fit for the 2-D predictor's `aoe` switch (2-D round, zkgbai-runs/predictor/twod).

usage: aoe_fit.py <cache-dir> <unitdefs.tsv> <out.pkl> [workers] [shots-per-game]
  Train games only (PRED_DEV_FOLD=k leaves fold k out, as every fitter does).

For every logged projectile shot of a weapon with areaOfEffect >= AOE_MIN aimed at an enemy unit
(a sample per game), the units around the target at impact (unit rows every 30 frames, positions
interpolated to the shot's close frame) get the engine's falloff: damage x (aoe - d) / (aoe - d x edge)
for d = distance to the unit's centre minus its radius, d < aoe (Recoil DoExplosionDamage, with the
model radius standing in for the collision volume). Units in another domain than the target (air vs
ground) are left out. Predicted splash on the target's ally team (P_t) and on the shooter's (P_f)
against logged splash = dmg - dmgt (damage of the shot on units other than its target):
  obs ~ kappa_t * P_t + kappa_f * P_f     (non-negative least squares, by weapon class and pooled)
kappa_t is the share of the geometric splash that lands (projectiles that miss land elsewhere,
shields, collision volumes smaller than the model radius); the 2-D sim applies kappa x falloff.
"""
import math
import os
import pickle
import random
import sys
import zlib
from collections import defaultdict
from multiprocessing import Pool

import numpy as np

import combatdata
import split
import units as unitmod

AOE_MIN = 16.0
SAMPLE_F = 30  # frames between unit rows (combatlog_widget UNIT_SAMPLE)
G = {}


def _init(udpath):
    G["ud"] = unitmod.load(udpath)


def wclass(wtype):
    return {"Cannon": "cannon", "MissileLauncher": "missile", "StarburstLauncher": "missile", "AircraftBomb": "bomb",
            "TorpedoLauncher": "torpedo", "LaserCannon": "laser"}.get(str(wtype), "other")


def game(args):
    path, cap = args
    d = combatdata.load_cached(path)
    meta = d["meta"]
    series = os.path.basename(path)[:-4]
    if split.split_of(meta.get("game", ""), series) != "train":
        return None
    w = meta["weapons"]
    s = d["shot"]
    u = d.get("unit")
    if w is None or s is None or u is None or not len(s) or not len(u):
        return None
    wd = s["wd"].astype(str)
    aoe = wd.map(w["aoe"]).astype(float).values
    keep = ((s["tt"] == "u").values & (s["tid"] >= 0).values & (s["tteam"] != s["oteam"]).values
            & (s["src"].astype(str) == "p").values & (aoe >= AOE_MIN))
    idx = np.flatnonzero(keep)
    if not len(idx):
        return None
    rng = random.Random(zlib.crc32(series.encode()))  # reproducible (str hash is salted per process)
    if len(idx) > cap:
        idx = np.array(sorted(rng.sample(list(idx), cap)))
    ally = {t: v[0] for t, v in meta["teams"].items()}
    ud = G["ud"]
    # unit rows by sample frame: frame -> (uid array, x, z, team, def)
    uf = u["f"].values; uu = u["uid"].values; ux = u["x"].values; uz = u["z"].values
    ut = u["uteam"].values; udef = u["udef"].astype(str).values
    order = np.argsort(uf, kind="stable")
    uf, uu, ux, uz, ut, udef = uf[order], uu[order], ux[order], uz[order], ut[order], udef[order]
    frames, starts = np.unique(uf, return_index=True)
    ends = np.append(starts[1:], len(uf))
    by_f = {int(f): (a, b) for f, a, b in zip(frames, starts, ends)}

    def rows_at(f):
        r = by_f.get(int(f))
        if r is None:
            return {}
        a, b = r
        return {int(uu[i]): (float(ux[i]), float(uz[i]), int(ut[i]), udef[i]) for i in range(a, b)}

    cache = {}

    def at(f):
        r = cache.get(f)
        if r is None:
            if len(cache) > 64:
                cache.clear()
            r = cache[f] = rows_at(f)
        return r

    out = []
    sv = {c: s[c].values for c in ("f1", "tid", "tteam", "oteam", "tx", "tz", "dmg", "dmgt", "tdef")}
    for i in idx:
        wname = wd.iat[i]
        a, edge, nominal = float(w.at[wname, "aoe"]), float(w.at[wname, "edge"]), float(w.at[wname, "damage"])
        if nominal <= 0:
            continue
        F = int(sv["f1"][i])
        f0 = (F // SAMPLE_F) * SAMPLE_F
        r0, r1 = at(f0), at(f0 + SAMPLE_F)
        t = (F - f0) / SAMPLE_F
        pos = {}
        for uid in set(r0) | set(r1):
            p, q = r0.get(uid), r1.get(uid)
            if p and q:
                pos[uid] = (p[0] + t * (q[0] - p[0]), p[1] + t * (q[1] - p[1]), p[2], p[3])
            else:
                pos[uid] = p or q
        tid = int(sv["tid"][i])
        tp = pos.get(tid)
        tx, tz = (tp[0], tp[1]) if tp else (float(sv["tx"][i]), float(sv["tz"][i]))
        if not (math.isfinite(tx) and math.isfinite(tz)):
            continue
        td = ud.get(str(sv["tdef"][i]))
        t_air = bool(td is not None and td.canfly)
        ta, oa = ally.get(int(sv["tteam"][i]), int(sv["tteam"][i])), ally.get(int(sv["oteam"][i]), int(sv["oteam"][i]))
        pt = pf = 0.0
        nt = nf = 0
        for uid, (x, z, team, dname) in pos.items():
            if uid == tid:
                continue
            dd = ud.get(dname)
            rad = dd.radius if dd is not None else 12.0
            if (dd is not None and dd.canfly) != t_air:
                continue
            dist = max(0.0, math.hypot(x - tx, z - tz) - rad)
            if dist >= a:
                continue
            mod = (a - dist) / (a - dist * edge) if a - dist * edge > 1e-6 else 1.0
            al = ally.get(team, team)
            if al == ta:
                pt += nominal * mod; nt += 1
            elif al == oa:
                pf += nominal * mod; nf += 1
        obs = max(0.0, float(sv["dmg"][i]) - float(sv["dmgt"][i]))
        out.append((wname, wclass(w.at[wname, "type"]), a, obs, pt, pf, nt, nf, float(sv["dmgt"][i]), nominal))
    return out


def nnls2(obs, a, b, nom=None):
    """obs ~ ka a + kb b (+ c nom: splash the geometry does not place, a share of nominal), all >= 0.
    With nom the intercept takes up shots whose neighbours the 1-s unit rows misplace, so the slopes
    are the geometric splash that lands."""
    from scipy.optimize import nnls
    X = np.stack([a, b] + ([nom] if nom is not None else []), 1)
    k, _ = nnls(X, obs)
    return float(k[0]), float(k[1])


def main(cache, udpath, out, workers=2, cap=3000):
    jobs = [(p, int(cap)) for p in combatdata.cached(cache)]
    rows = []
    with Pool(int(workers), initializer=_init, initargs=(udpath,)) as pool:
        for k, r in enumerate(pool.imap_unordered(game, jobs, chunksize=4)):
            if r:
                rows += r
            if (k + 1) % 200 == 0:
                print(f"{k + 1}/{len(jobs)} games, {len(rows)} shots", flush=True)
    names = ["wd", "cls", "aoe", "obs", "pt", "pf", "nt", "nf", "dmgt", "nominal"]
    cols = {n: np.array([r[j] for r in rows]) for j, n in enumerate(names)}
    obs, pt, pf = cols["obs"].astype(float), cols["pt"].astype(float), cols["pf"].astype(float)
    res = {"aoe_min": AOE_MIN, "shots": len(rows), "fold_out": split.DEV_FOLD}
    nom = cols["nominal"].astype(float)
    # the sim uses the slopes without an intercept: they match dense clumps (the band with P_t >= 2 nominal),
    # where the intercept model under-predicts; the intercept fit is reported beside them
    res["kappa"], res["kappa_f"] = nnls2(obs, pt, pf)
    res["kappa_int"] = nnls2(obs, pt, pf, nom)
    print(f"{len(rows)} shots; pooled kappa_t {res['kappa']:.3f} kappa_f {res['kappa_f']:.3f}")
    print(f"logged splash {obs.sum():.0f}; geometric target-side {pt.sum():.0f}, own-side {pf.sum():.0f}")
    pred = res["kappa"] * pt + res["kappa_f"] * pf
    r = np.corrcoef(obs, pred)[0, 1] if len(obs) > 2 else float("nan")
    print(f"per-shot corr(obs, fitted) {r:.3f}; shots with obs>0: {np.mean(obs > 0):.3f}, with pt>0: {np.mean(pt > 0):.3f}")
    res["by_class"] = {}
    for c in sorted(set(cols["cls"])):
        m = cols["cls"] == c
        if m.sum() >= 500:
            kt, kf = nnls2(obs[m], pt[m], pf[m])
            res["by_class"][c] = (kt, kf, int(m.sum()))
            print(f"  {c:8s} n={m.sum():7d} kappa_t {kt:.3f} kappa_f {kf:.3f} obs/shot {obs[m].mean():7.2f} pt/shot {pt[m].mean():7.2f}")
    # calibration by predicted geometric splash decile: does more geometry mean more splash?
    print(f"  (with an intercept ~ nominal: kappa_t {res['kappa_int'][0]:.3f} kappa_f {res['kappa_int'][1]:.3f})")
    print("  by P_t band (share of nominal): mean obs/nominal vs kappa*P_t/nominal (slope part only)")
    x = pt / nom
    for lo, hi in ((0, 1e-9), (1e-9, 0.25), (0.25, 0.5), (0.5, 1.0), (1.0, 2.0), (2.0, 99)):
        m = (x >= lo) & (x < hi)
        if m.sum():
            print(f"    [{lo:5.2f},{hi:5.2f}) n={m.sum():7d} obs {np.mean(obs[m] / nom[m]):.3f} fitted {np.mean(pred[m] / nom[m]):.3f}")
    per = defaultdict(lambda: [0, 0.0, 0.0, 0.0])
    for wname, o, a, b in zip(cols["wd"], obs, pt, pf):
        e = per[wname]; e[0] += 1; e[1] += o; e[2] += a; e[3] += b
    res["per_weapon"] = {k: tuple(v) for k, v in per.items()}
    with open(out, "wb") as fh:
        pickle.dump(res, fh)
    print("wrote", out)


if __name__ == "__main__":
    main(*sys.argv[1:4], *sys.argv[4:6])
