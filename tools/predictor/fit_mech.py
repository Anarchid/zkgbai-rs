#!/usr/bin/env python3
"""Fit the mechanics parameters for mech.py on train games (respects PRED_DEV_FOLD).

usage: fit_mech.py <pdir> <fights.pkl> <out mech.pkl> [workers] [engage games]
  <pdir> holds cache/, unitdefs.tsv, wtab.pkl, commw.pkl, hitcells.pkl (predict._init).
Writes: air {wd: (can_air, can_ground)}, burn {wd: (burn damage per hit, mean burn frames)},
death {def: (damage, aoe, edge, paralyzer)}, pulse {wd: (HP damage per victim, radius)},
stun_cap_s, udw {wd: def-dump weapon fields}, engage_w (logistic, mech.ENGAGE_NAMES).
"""
import os
import pickle
import random
import sys
from collections import Counter, defaultdict
from multiprocessing import Pool

import numpy as np
import pandas as pd
from scipy.optimize import minimize
from scipy.special import expit

import combatdata
import mech
import predict
import split

TAIL = 90
G = {}


def init(pdir, fights_by_series):
    predict._init(pdir, os.path.join(pdir, "cache"))
    G["fights"] = fights_by_series


def per_game(args):
    path, want_engage = args
    series = os.path.basename(path)[:-4]
    d = combatdata.load_cached(path)
    if split.split_of(d["meta"].get("game", ""), series) != "train":
        return None
    ud = predict.G["ud"]
    out = {"air": Counter(), "burn_dmg": Counter(), "burn_len": defaultdict(list), "death": defaultdict(Counter),
           "shot_wd": set(), "pulse": defaultdict(lambda: [0, 0, 0.0]), "stun": [], "engage": [], "kdeath": defaultdict(list)}
    s = d.get("shot")
    if s is not None and len(s):
        out["shot_wd"] = set(s["wd"].astype(str).unique())
        e = s[(s["tt"] == "u") & (s["tid"] >= 0) & (s["tteam"] != s["oteam"])]
        air = e["tdef"].astype(str).map(lambda n: bool(ud.get(n) is not None and ud.get(n).canfly))
        for (wd, a), k in e.groupby([e["wd"].astype(str), air]).size().items():
            out["air"][(wd, bool(a))] += int(k)
    b = d.get("burn")
    if b is not None and len(b):
        b = b[b["dmg"] > 0]
        for wd, v in b.groupby(b["wd"].astype(str))["dmg"].sum().items():
            out["burn_dmg"][wd] += float(v)
        for (vid, wd), g in b.groupby([b["vid"], b["wd"].astype(str)]):
            fr = np.sort(g["f"].values)
            cut = np.where(np.diff(fr) > 30)[0]
            starts = np.r_[fr[0], fr[cut + 1]]; ends = np.r_[fr[cut], fr[-1]]
            out["burn_len"][wd].extend((ends - starts + 1).tolist())
    m = d.get("dmg")
    k = d.get("kill")
    if m is not None and len(m):
        mm = m[m["dmg"] > 0]
        # kamikaze death explosions often carry no attacker id (self-destruct): by weapon name
        kw = mm["wd"].astype(str)
        for kname in mech.KAMIKAZE:
            v = mm.loc[kw == f"{kname}_{kname}_death", "dmg"]
            if len(v):
                out["kdeath"][f"{kname}_{kname}_death"].extend(v.tolist())
        if k is not None and len(k):
            kf = dict(zip(k["uid"].values, k["f"].values))
            kd = dict(zip(k["uid"].values, k["udef"].astype(str).values))
            hit = mm[mm["aid"].isin(kf.keys())]
            for aid, f, wd in zip(hit["aid"].values, hit["f"].values, hit["wd"].astype(str).values):
                if 0 <= f - kf[aid] <= 3:
                    out["death"][kd[aid]][wd] += 1
        for (aid, adef, wd), g in mm[mm["para"] == 0].groupby([mm["aid"], mm["adef"].astype(str), mm["wd"].astype(str)]):
            dd = ud.get(adef)
            if dd is None or wd not in {w.name for w in dd.weapons}:
                continue
            p = out["pulse"][wd]
            p[0] += g["f"].nunique(); p[1] += len(g); p[2] += float(g["dmg"].sum())
    st = d.get("stun")
    if st is not None and len(st):
        for uid, g in st.sort_values("f").groupby("uid"):
            on = None
            for f, o in zip(g["f"].values, g["on"].values):
                if o == 1 and on is None:
                    on = f
                elif o == 0 and on is not None:
                    out["stun"].append(f - on); on = None
    if want_engage and s is not None and len(s):
        first = s.groupby("own")["f0"].apply(np.sort)
        for fight in G["fights"].get(series, []):
            if not (fight["costA"] > 0 and fight["costB"] > 0):
                continue
            units = predict.build_units(fight)
            recs = list(fight["A"]) + list(fight["B"])
            na = len(fight["A"])
            for i, (u, r) in enumerate(zip(units, recs)):
                if not u.armed or u.max_range <= 0 or r["first"] > 300:
                    continue
                own, foes = (fight["A"], fight["B"]) if i < na else (fight["B"], fight["A"])
                feats, _ = mech.engage_features(r, u.max_range, own, foes)
                sh = first.get(r["uid"])
                fired = sh is not None and bool(((sh >= fight["start"]) & (sh <= fight["end"] + TAIL)).any())
                out["engage"].append((feats, fired))
    out["burn_len"] = dict(out["burn_len"]); out["death"] = dict(out["death"]); out["pulse"] = dict(out["pulse"])
    out["kdeath"] = dict(out["kdeath"])
    return out


def main(pdir, fights_path, out_path, workers=3, engage_games=700):
    fs = pickle.load(open(fights_path, "rb"))
    by_series = defaultdict(list)
    for f in fs:
        by_series[f["series"]].append(f)
    del fs
    paths = list(combatdata.cached(os.path.join(pdir, "cache")))
    # human and replay logs are known by series; dev and test games are dropped in per_game by game id
    train = [p for p in paths if split.split_of("", os.path.basename(p)[:-4]) not in ("human", "replay")]
    random.seed(11)
    eng = set(random.sample(train, min(int(engage_games), len(train))))
    eng_series = {os.path.basename(p)[:-4] for p in eng}
    by_series = {k: v for k, v in by_series.items() if k in eng_series}
    acc = {"air": Counter(), "burn_dmg": Counter(), "burn_len": defaultdict(list), "death": defaultdict(Counter),
           "shot_wd": set(), "pulse": defaultdict(lambda: [0, 0, 0.0]), "stun": [], "engage": [], "kdeath": defaultdict(list)}
    n = 0
    with Pool(int(workers), initializer=init, initargs=(pdir, by_series)) as pool:
        for r in pool.imap_unordered(per_game, [(p, p in eng) for p in train], chunksize=4):
            n += 1
            if n % 300 == 0:
                print(f"{n}/{len(train)}", flush=True)
            if r is None:
                continue
            acc["air"].update(r["air"]); acc["burn_dmg"].update(r["burn_dmg"]); acc["shot_wd"] |= r["shot_wd"]
            for k2, v in r["burn_len"].items():
                acc["burn_len"][k2].extend(v)
            for k2, v in r["death"].items():
                acc["death"][k2].update(v)
            for k2, v in r["pulse"].items():
                p = acc["pulse"][k2]; p[0] += v[0]; p[1] += v[1]; p[2] += v[2]
            acc["stun"].extend(r["stun"]); acc["engage"].extend(r["engage"])
            for k2, v in r["kdeath"].items():
                acc["kdeath"][k2].extend(v)
    predict._init(pdir, os.path.join(pdir, "cache"))
    ud, wtab = predict.G["ud"], predict.G["wtab"]
    cells = pickle.load(open(os.path.join(pdir, "hitcells.pkl"), "rb"))["train"]
    hits_tgt = cells.groupby(level=0)["hit_tgt"].sum().to_dict()
    M = {}
    # air / ground
    tot = Counter()
    for (wd, a), k in acc["air"].items():
        tot[wd] += k
    M["air"] = {}
    for wd, t in tot.items():
        if t < 200:
            continue
        na = acc["air"].get((wd, True), 0); ng = acc["air"].get((wd, False), 0)
        M["air"][wd] = (na / t >= 0.01, ng / t >= 0.01)
    # burn
    M["burn"] = {}
    for wd, v in acc["burn_dmg"].items():
        h = hits_tgt.get(wd, 0.0)
        if h >= 50 and acc["burn_len"].get(wd):
            M["burn"][wd] = (v / h, int(np.mean(acc["burn_len"][wd])))
    # death weapons
    M["death"] = {}
    for dname, c in acc["death"].items():
        wd, k = c.most_common(1)[0]
        own = {w.name for w in ud.get(dname).weapons} if ud.get(dname) is not None else set()
        if k < 10 or k < 0.5 * sum(c.values()) or wd not in wtab.index or (wd in own and dname not in mech.KAMIKAZE):
            continue  # own weapons still landing at the death frame are not a death explosion (a kamikaze's is)
        row = wtab.loc[wd]
        aoe = float(row["aoe"])
        if aoe <= 0:
            continue
        para = int(row["paralyzer"]) == 1
        dmg = float(row["damage"])
        if para:
            dw = [w for w in ud.get(dname).weapons if w.name == wd] if ud.get(dname) else []
            dmg = dw[0].damage if dw else 0.0
        if dmg > 0:
            M["death"][dname] = (dmg, aoe, float(row["edge"]), para)
    for kname in sorted(mech.KAMIKAZE - set(M["death"])):
        wd = f"{kname}_{kname}_death"
        v = np.array(acc["kdeath"].get(wd, []))
        if len(v) >= 30 and wd in wtab.index and float(wtab.loc[wd, "aoe"]) > 0:
            # full damage = p95 of per-victim damage (victims spread over the falloff)
            M["death"][kname] = (float(np.percentile(v, 95)), float(wtab.loc[wd, "aoe"]), float(wtab.loc[wd, "edge"]),
                                 int(wtab.loc[wd, "paralyzer"]) == 1)
    # pulse weapons: in a unit's weapon list, in damage rows, never a shot row
    M["pulse"] = {}
    for wd, (pulses, victims, dmg) in acc["pulse"].items():
        if wd in acc["shot_wd"] or pulses < 200 or wd not in wtab.index or float(wtab.loc[wd, "reload"]) > 3.0:
            continue  # a periodic weapon (a jump landing is not)
        M["pulse"][wd] = (dmg / victims, float(wtab.loc[wd, "aoe"]) or float(wtab.loc[wd, "range"]))
    # stun length cap
    st = np.array(acc["stun"], dtype=float)
    M["stun_cap_s"] = float(np.median(st) / 30.0) if len(st) else 4.0
    # def-dump weapon fields
    M["udw"] = {}
    for name, d in ud.defs.items():
        for w in d.weapons:
            M["udw"][w.name] = {"paralyzer": bool(getattr(w, "paralyzer", 0)), "damage": float(w.damage),
                               "extra_damage": float(getattr(w, "extra_damage", 0.0) or 0.0), "slow": float(getattr(w, "slow", 0.0) or 0.0)}
    # engage logistic
    X = np.array([e[0] for e in acc["engage"]]); y = np.array([1.0 if e[1] else 0.0 for e in acc["engage"]])

    def nll(w):
        p = np.clip(expit(X @ w), 1e-9, 1 - 1e-9)
        return -(y * np.log(p) + (1 - y) * np.log(1 - p)).sum() + 1e-2 * np.sum(w[1:] ** 2)

    w0 = np.zeros(X.shape[1]); w0[0] = 1.0
    M["engage_w"] = minimize(nll, w0, method="L-BFGS-B").x
    M["engage_names"] = mech.ENGAGE_NAMES
    pickle.dump(M, open(out_path, "wb"))
    lines = [f"games {n}; engage rows {len(y)} (fired {y.mean():.3f})"]
    lines.append("engage w: " + ", ".join(f"{k} {v:+.2f}" for k, v in zip(mech.ENGAGE_NAMES, M["engage_w"])))
    lines.append(f"air rules {len(M['air'])}: no air {sum(1 for a, g in M['air'].values() if not a)}, no ground {sum(1 for a, g in M['air'].values() if not g)}")
    lines.append("burn: " + ", ".join(f"{k} {v[0]:.0f}/hit over {v[1] / 30:.1f}s" for k, v in sorted(M["burn"].items(), key=lambda kv: -kv[1][0])[:10]))
    lines.append(f"death weapons for {len(M['death'])} defs; kamikaze: " + ", ".join(f"{k} {v[0]:.0f}@{v[1]:.0f}{' EMP' if v[3] else ''}" for k, v in sorted(M["death"].items()) if k in mech.KAMIKAZE))
    lines.append("death, biggest: " + ", ".join(f"{k} {v[0]:.0f}@{v[1]:.0f}" for k, v in sorted(M["death"].items(), key=lambda kv: -kv[1][0] * kv[1][1])[:12]))
    lines.append("pulse: " + ", ".join(f"{k} {v[0]:.1f}/victim r{v[1]:.0f}" for k, v in M["pulse"].items()))
    lines.append(f"stun median {M['stun_cap_s']:.2f} s over {len(st)} stuns")
    print("\n".join(lines))


if __name__ == "__main__":
    main(*sys.argv[1:4], *(int(x) for x in sys.argv[4:6]))
