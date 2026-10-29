#!/usr/bin/env python3
"""Cut combat logs into fights (engagements) with initial composition and outcome.

usage: segment.py <cache-dir> <out.pkl> [workers]
Protocol: zkgbai-runs/predictor/SUMMARY.md "Predeclared protocol".
"""
import math
import os
import pickle
import sys
from collections import defaultdict
from multiprocessing import Pool

import numpy as np

import combatdata
import units as unitmod

GAP = 300        # frames: events of one fight are at most this far apart
LINK_R = 600.0   # elmos: an event this close to a fight's recent centre joins it
INIT = int(os.environ.get("SEG_INIT", 90))  # frames: units first seen within this of the start are initial
TAIL = 90        # frames after the last event still counted for deaths

UD = None


def _ud():
    global UD
    if UD is None:
        UD = unitmod.load()
    return UD


class UF:
    def __init__(self):
        self.p = []

    def new(self):
        self.p.append(len(self.p))
        return len(self.p) - 1

    def find(self, i):
        while self.p[i] != i:
            self.p[i] = self.p[self.p[i]]
            i = self.p[i]
        return i

    def union(self, a, b):
        a, b = self.find(a), self.find(b)
        if a != b:
            self.p[max(a, b)] = min(a, b)
        return min(a, b)


def events_of(d):
    """(frame, a, ateam, ax, az, b, bteam, bx, bz, kind) for shots at enemy units and attributed damage."""
    s = d["shot"]
    m = (s["tt"] == "u") & (s["tid"] >= 0) & (s["tteam"] != s["oteam"]) & (s["own"] >= 0)
    s = s[m]
    ev = np.empty(len(s), dtype=[("f", "i4"), ("a", "i4"), ("at", "i4"), ("ax", "f4"), ("az", "f4"),
                                 ("b", "i4"), ("bt", "i4"), ("bx", "f4"), ("bz", "f4"), ("k", "i1")])
    ev["f"] = s["f0"]; ev["a"] = s["own"]; ev["at"] = s["oteam"]; ev["ax"] = s["ox"]; ev["az"] = s["oz"]
    ev["b"] = s["tid"]; ev["bt"] = s["tteam"]; ev["bx"] = s["tx"]; ev["bz"] = s["tz"]; ev["k"] = 0
    g = d["dmg"]
    if len(g):
        m = (g["aid"] >= 0) & (g["ateam"] != g["vteam"]) & (g["ateam"] >= 0)
        g = g[m]
        if len(g):
            # attacker position unknown in dmg rows: use the victim's last known position (unit rows) if any
            e2 = np.empty(len(g), dtype=ev.dtype)
            e2["f"] = g["f"]; e2["a"] = g["aid"]; e2["at"] = g["ateam"]; e2["b"] = g["vid"]; e2["bt"] = g["vteam"]
            e2["ax"] = e2["az"] = e2["bx"] = e2["bz"] = np.nan; e2["k"] = 1
            ev = np.concatenate([ev, e2])
    ev.sort(order="f", kind="stable")
    return ev


def unit_index(d):
    """uid -> (frames, hp, hpmax, x, z) sorted, from unit rows."""
    u = d["unit"]
    idx = {}
    if not len(u):
        return idx
    order = np.lexsort((u["f"].values, u["uid"].values))
    uid = u["uid"].values[order]; f = u["f"].values[order]
    hp = u["hp"].values[order]; hpmax = u["hpmax"].values[order]
    x = u["x"].values[order]; z = u["z"].values[order]
    para = u["para"].values[order]
    sh = u["sh"].values[order] if "sh" in u.columns else np.full(len(order), np.nan)
    cuts = np.flatnonzero(np.diff(uid)) + 1
    starts = np.concatenate([[0], cuts]); ends = np.concatenate([cuts, [len(uid)]])
    for s0, e0 in zip(starts, ends):
        idx[int(uid[s0])] = (f[s0:e0], hp[s0:e0], hpmax[s0:e0], x[s0:e0], z[s0:e0], para[s0:e0], sh[s0:e0])
    return idx


def state_near(uidx, uid, frame, before=30, after=30):
    """Unit row nearest to frame within [frame-before, frame+after] -> (hp, hpmax, x, z) or None."""
    r = uidx.get(uid)
    if r is None:
        return None
    f = r[0]
    i = np.searchsorted(f, frame)
    best = None
    for j in (i - 1, i):
        if 0 <= j < len(f) and frame - before <= f[j] <= frame + after:
            if best is None or abs(f[j] - frame) < abs(f[best] - frame):
                best = j
    if best is None:
        return None
    return float(r[1][best]), float(r[2][best]), float(r[3][best]), float(r[4][best])


def shield_first(uidx, uid, f_from, f_to):
    """Shield charge at the unit's first row in [f_from, f_to]; None without a shield or a row."""
    r = uidx.get(uid)
    if r is None:
        return None
    i = np.searchsorted(r[0], f_from)
    if i < len(r[0]) and r[0][i] <= f_to and not np.isnan(r[6][i]):
        return float(r[6][i])
    return None


def state_last_before(uidx, uid, frame):
    r = uidx.get(uid)
    if r is None:
        return None
    i = np.searchsorted(r[0], frame, side="right") - 1
    if i < 0:
        return None
    return float(r[1][i]), float(r[2][i]), float(r[3][i]), float(r[4][i]), int(r[0][i])


def segment_game(path):
    d = combatdata.load_cached(path)
    meta = d["meta"]
    ud = _ud()
    ev = events_of(d)
    if not len(ev):
        return []
    # unit -> def name / team from shot rows (both roles) and kill rows
    s = d["shot"]
    udef = {}
    for col_id, col_def, col_team in (("own", "odef", "oteam"), ("tid", "tdef", "tteam")):
        ids = s[col_id].values; defs = s[col_def].astype(str).values; teams = s[col_team].values
        for i in range(len(ids)):
            if ids[i] >= 0 and ids[i] not in udef:
                udef[int(ids[i])] = (defs[i], int(teams[i]))
    for col_id, col_def, col_team in (("vid", "vdef", "vteam"), ("aid", "adef", "ateam")):
        g = d["dmg"]
        if len(g):
            ids = g[col_id].values; defs = g[col_def].astype(str).values; teams = g[col_team].values
            for i in range(len(ids)):
                if ids[i] >= 0 and ids[i] not in udef and defs[i]:
                    udef[int(ids[i])] = (defs[i], int(teams[i]))
    k = d["kill"]
    killed = dict(zip(k["uid"].values.tolist(), k["f"].values.tolist()))
    for i in range(len(k)):
        if int(k["uid"].values[i]) not in udef:
            udef[int(k["uid"].values[i])] = (str(k["udef"].values[i]), int(k["uteam"].values[i]))
    uidx = unit_index(d)
    # thp/thpmax of targets at shot time, for initial hp
    thp = {}
    for uid_, f0, hp, hpmax in zip(s["tid"].values.tolist(), s["f0"].values.tolist(), s["thp"].values.tolist(), s["thpmax"].values.tolist()):
        if uid_ >= 0:
            thp.setdefault(uid_, []).append((f0, hp, hpmax))

    uf = UF()
    unit_fight = {}   # uid -> (fid, last frame)
    open_f = {}       # root fid -> [last, cx, cz]
    members = defaultdict(dict)  # root -> {uid: first frame}
    frames = {}       # root -> [start, end, nevents]
    for e in ev:
        f = int(e["f"]); a = int(e["a"]); b = int(e["b"])
        cand = set()
        for u in (a, b):
            r = unit_fight.get(u)
            if r is not None and f - r[1] <= GAP:
                cand.add(uf.find(r[0]))
        px = pz = None
        if not math.isnan(e["ax"]) and not math.isnan(e["bx"]):
            px = (float(e["ax"]) + float(e["bx"])) / 2; pz = (float(e["az"]) + float(e["bz"])) / 2
        if not cand and px is not None:
            for fid, st in open_f.items():
                if f - st[0] <= GAP and st[1] is not None and (st[1] - px) ** 2 + (st[2] - pz) ** 2 <= LINK_R ** 2:
                    cand.add(fid)
                    break
        if not cand:
            root = uf.new()
            open_f[root] = [f, px, pz]
            frames[root] = [f, f, 0]
        else:
            root = None
            for c in cand:
                root = c if root is None else uf.union(root, c)
            # merge bookkeeping into root
            for c in cand:
                if c != root:
                    st = open_f.pop(c, None)
                    members[root].update({u: min(ff, members[root].get(u, ff)) for u, ff in members.pop(c, {}).items()})
                    fr = frames.pop(c, None)
                    if fr:
                        frames[root][0] = min(frames[root][0], fr[0]); frames[root][1] = max(frames[root][1], fr[1]); frames[root][2] += fr[2]
            st = open_f.setdefault(root, [f, px, pz])
            st[0] = f
            if px is not None:
                st[1] = px if st[1] is None else 0.8 * st[1] + 0.2 * px
                st[2] = pz if st[2] is None else 0.8 * st[2] + 0.2 * pz
        frames[root][1] = f; frames[root][2] += 1
        m = members[root]
        if a not in m:
            m[a] = f
        if b not in m:
            m[b] = f
        unit_fight[a] = (root, f); unit_fight[b] = (root, f)
        # drop stale open fights now and then
        if len(open_f) > 64:
            for fid in [fid for fid, st in open_f.items() if f - st[0] > GAP]:
                del open_f[fid]

    teams = meta.get("teams", {})
    ai_team = None
    for t, (ally, kind, name) in teams.items():
        if "ZKGBAI" in name and ai_team is None:
            ai_team = t
    fights = []
    series = os.path.basename(path)[:-4]
    for root, m in members.items():
        fr = frames.get(root)
        if not fr:
            continue
        start, end, nev = fr
        tset = sorted({udef[u][1] for u in m if u in udef})
        if len(tset) != 2:
            continue
        ta, tb = tset
        if ai_team in tset and ai_team != ta:
            ta, tb = tb, ta
        sides = {ta: [], tb: []}
        reinf = {ta: 0.0, tb: 0.0}
        for u, ff in m.items():
            if u not in udef:
                continue
            dname, team = udef[u]
            dd = ud.get(dname)
            if dd is None:
                continue
            hp = hpmax = None
            st = state_near(uidx, u, ff)
            if st is not None:
                hp, hpmax, x, z = st
            else:
                x = z = float("nan")
            if hp is None and u in thp:
                for f0, h, hm in thp[u]:
                    if abs(f0 - ff) <= 30:
                        hp, hpmax = h, hm
                        break
            if hpmax is None or not (hpmax > 0):
                hpmax = dd.health if not dd.is_com else 3000.0
            if hp is None or not (hp >= 0):
                hp = hpmax
            if dd.is_com:
                dd = ud.get(dname, hpmax)
            initial = ff - start <= INIT
            kf = killed.get(u)
            died = kf is not None and start <= kf <= end + TAIL
            last = state_last_before(uidx, u, end + TAIL)
            hp_end = 0.0 if died else (last[0] if last is not None else hp)
            rec = {"uid": u, "def": dname, "cost": dd.cost, "armed": dd.armed, "static": dd.static, "com": dd.is_com,
                   "hp": hp, "hpmax": hpmax, "x": x, "z": z, "first": ff - start, "died": died, "hp_end": hp_end,
                   "kill_f": (kf - start) if died else None,
                   "sh": shield_first(uidx, u, ff - 30, end + TAIL)}
            if initial:
                sides[team].append(rec)
            else:
                if dd.armed:
                    reinf[team] += dd.cost
        A, B = sides[ta], sides[tb]
        def armed_cost(side):
            return sum(r["cost"] for r in side if r["armed"])
        def killed_cost(side):
            return sum(r["cost"] for r in side if r["armed"] and r["died"])
        def centroid(side):
            xs = [r["x"] for r in side if not math.isnan(r["x"])]; zs = [r["z"] for r in side if not math.isnan(r["z"])]
            return (sum(xs) / len(xs), sum(zs) / len(zs)) if xs else (float("nan"), float("nan"))
        ca, cb = centroid(A), centroid(B)
        d0 = math.hypot(ca[0] - cb[0], ca[1] - cb[1]) if not math.isnan(ca[0]) and not math.isnan(cb[0]) else float("nan")
        acA, acB = armed_cost(A), armed_cost(B)
        fights.append({
            "game": meta.get("game", ""), "series": series, "map": meta.get("map", ""), "fid": root,
            "start": start, "end": end, "nev": nev, "teamA": ta, "teamB": tb, "ai_is_A": ai_team == ta,
            "nameA": teams.get(ta, (0, "", ""))[2], "nameB": teams.get(tb, (0, "", ""))[2],
            "A": A, "B": B, "costA": acA, "costB": acB,
            "yA": killed_cost(A) / acA if acA > 0 else float("nan"), "yB": killed_cost(B) / acB if acB > 0 else float("nan"),
            "reinfA": reinf[ta], "reinfB": reinf[tb], "d0": d0,
            "closed": (reinf[ta] < 0.1 * acA if acA > 0 else reinf[ta] == 0) and (reinf[tb] < 0.1 * acB if acB > 0 else reinf[tb] == 0),
        })
    return fights


def main(cache, out, workers=4):
    paths = list(combatdata.cached(cache))
    allf = []
    with Pool(workers) as pool:
        for i, fs in enumerate(pool.imap_unordered(segment_game, paths, chunksize=4)):
            allf.extend(fs)
            if (i + 1) % 100 == 0:
                print(f"{i + 1}/{len(paths)} fights={len(allf)}", flush=True)
    with open(out, "wb") as fh:
        pickle.dump(allf, fh, protocol=5)
    print(f"{len(allf)} fights from {len(paths)} games -> {out}")


if __name__ == "__main__":
    main(sys.argv[1], sys.argv[2], int(sys.argv[3]) if len(sys.argv) > 3 else 4)
