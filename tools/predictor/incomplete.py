#!/usr/bin/env python3
"""Cloaked snipers, in-battle repair and unfinished combatants (exploratory, mechanics round 2 triage).

usage: incomplete.py <refit-dir> [games=600]
  Phantom: closed fights with Phantoms (cloaksnipe) on one side only: who hunts them, how often the
    Phantom dies, the hunters' losses, the table model's owner bias; shots at Phantoms by the
    shooter team's LOS on it (tlos: 2 sight, 1 radar, 0 none).
  Repair: unit rows inside fight windows (sampled games): units whose HP rose during the fight,
    HP regained vs HP lost.
  Frames (exact, ZKGBAI side): the journal's "unit+" (written on UnitFinished) for ZKGBAI statics in
    closed fights: finished before the fight, during it, or never (an unfinished frame).
"""
import glob
import json
import os
import pickle
import random
import sys
from collections import Counter, defaultdict

import numpy as np
import pandas as pd

import combatdata
import split

RUNS = os.path.expanduser("~/Programs/zkgbai-runs")
RAIDERS = {"cloakraid", "shieldraid", "vehraid", "hoverraid", "tankraid", "jumpraid", "spiderscout", "amphraid", "vehscout",
           "hoverheavyraid", "cloakheavyraid", "jumpscout", "gunshipraid", "tankheavyraid"}


def main(rdir, n_games=600):
    fs = pickle.load(open(os.path.join(rdir, "fights.pkl"), "rb"))
    pred = pd.read_csv(os.path.join(rdir, "empcap_out", "fights_pred.tsv"), sep="\t", low_memory=False,
                       usecols=["game", "fid", "split", "win", "dps_stop_pAwin"]).set_index(["game", "fid"])
    # ---- Phantom fights
    rows = []
    for f in fs:
        if not f["closed"]:
            continue
        pa = [r for r in f["A"] if r["def"] == "cloaksnipe"]; pb = [r for r in f["B"] if r["def"] == "cloaksnipe"]
        if bool(pa) == bool(pb):
            continue
        own, opp = ("A", "B") if pa else ("B", "A")
        ph = pa or pb
        o = f[opp]
        k = (f["game"], f["fid"])
        p = pred.loc[k] if k in pred.index else None
        rows.append({"game": f["game"], "n_ph": len(ph), "ph_died": sum(r["died"] for r in ph),
                     "opp_armed_mobile": sum(r["armed"] and not r["static"] for r in o),
                     "opp_raiders": sum(r["def"] in RAIDERS for r in o), "opp_static": sum(r["armed"] and r["static"] for r in o),
                     "opp_cost": sum(r["cost"] for r in o if r["armed"]), "opp_lost": sum(r["cost"] for r in o if r["armed"] and r["died"]),
                     "own_lost": sum(r["cost"] for r in f[own] if r["armed"] and r["died"]), "ph_cost": sum(r["cost"] for r in ph),
                     "win_own": None if p is None or p["win"] < 0 else int(p["win"] == (1 if own == "A" else 0)),
                     "p_own": None if p is None else float(p["dps_stop_pAwin"] if own == "A" else 1 - p["dps_stop_pAwin"])})
    ph = pd.DataFrame(rows)
    print(f"PHANTOM: {len(ph)} closed fights with Phantoms on one side ({ph.game.nunique()} games)")
    hunt = ph[ph.opp_raiders >= 1]
    for name, s in (("all", ph), ("opponent fields raiders", hunt), ("opponent raiders only, no statics", ph[(ph.opp_raiders >= 1) & (ph.opp_static == 0)])):
        d = s[s.win_own.notna()]
        print(f"  {name:34} n {len(s):5}: Phantom died {s.ph_died.sum() / s.n_ph.sum():.2f} per Phantom; "
              f"opponent lost {s.opp_lost.sum():8.0f} metal vs {s.ph_died.mul(750).sum():8.0f} metal of Phantoms killed; "
              f"Phantom side won {d.win_own.mean():.2f} (sim said {d.p_own.mean():.2f}, n {len(d)})")
    paths = list(combatdata.cached(os.path.join(rdir, "cache")))
    random.seed(17)
    tl = Counter(); tl_hit = Counter()
    for pth in random.sample(paths, min(len(paths), 1200)):
        s = combatdata.load_cached(pth).get("shot")
        if s is None or not len(s) or "tlos" not in s.columns:
            continue
        a = s[s["tdef"].astype(str) == "cloaksnipe"]
        for v, g in a.groupby("tlos"):
            tl[int(v)] += len(g); tl_hit[int(v)] += int((g["dmgt"] > 0).sum())
    print("  shots at Phantoms by the shooter team's view of it: " + ", ".join(
        f"{({2: 'in sight', 1: 'radar only', 0: 'unseen', -1: 'unknown (v1 log)'}).get(k, k)} {n} (hit {tl_hit[k] / max(n, 1):.2f})" for k, n in sorted(tl.items())))
    # ---- repair inside fight windows + exact frames from ZKGBAI journals
    by_series = defaultdict(list)
    for f in fs:
        if f["closed"]:
            by_series[f["series"]].append(f)
    del fs
    ai = [p for p in paths if split.split_of("", os.path.basename(p)[:-4]) not in ("human", "replay")]
    rep = Counter(); regained = lost = 0.0
    frames = Counter()
    for pth in random.sample(ai, min(int(n_games), len(ai))):
        series = os.path.basename(pth)[:-4]
        fights = by_series.get(series, [])
        if not fights:
            continue
        d = combatdata.load_cached(pth)
        u = d.get("unit")
        if u is not None and len(u):
            g_by = {uid: g.sort_values("f") for uid, g in u.groupby("uid")}
            for f in fights:
                for side in ("A", "B"):
                    for r in f[side]:
                        g = g_by.get(r["uid"])
                        if g is None:
                            continue
                        w = g[(g["f"] >= f["start"]) & (g["f"] <= f["end"] + 90)]["hp"].values
                        if len(w) < 2:
                            continue
                        dh = np.diff(w)
                        up = dh[dh > 0].sum(); down = -dh[dh < 0].sum()
                        kind = "static" if r["static"] else "mobile"
                        rep[(kind, "rows")] += 1
                        if up >= 0.05 * max(r["hpmax"] or 1, 1):
                            rep[(kind, "rose")] += 1
                        regained += up; lost += down
        jdir = os.path.join(RUNS, *series.split("__"), "journal")
        js = glob.glob(os.path.join(jdir, "*.jsonl"))
        if not js:
            continue
        team_of = {}
        for t, (ally, kind, name) in d["meta"]["teams"].items():
            if "ZKGBAI" in name:
                team_of[t] = True
        finished = {}
        for j in js:
            try:
                for line in open(j):
                    if '"k":"unit+"' in line:
                        rr = json.loads(line)
                        finished[rr["id"]] = rr["f"]
            except OSError:
                continue
        if not finished:
            continue
        for f in fights:
            for side, team in (("A", f["teamA"]), ("B", f["teamB"])):
                if team not in team_of:
                    continue
                for r in f[side]:
                    if not (r["static"] and r["armed"]):
                        continue
                    fin = finished.get(r["uid"])
                    if fin is not None and fin < f["start"]:
                        st = "finished before"
                    elif fin is not None and fin <= f["end"] + 90:
                        st = "finished during the fight"
                    elif fin is not None:
                        st = "finished after"
                    else:
                        st = "never finished (frame)" if r["died"] else "no record (survived)"
                    frames[st] += 1
    print("\nREPAIR in fight windows (sampled AI games; unit rows at ~1 s):")
    for kind in ("mobile", "static"):
        n = rep[(kind, "rows")]
        print(f"  {kind:6}: {n} unit-fights with >= 2 rows; HP rose by >= 5% of max during the fight in {rep[(kind, 'rose')] / max(n, 1):.1%}")
    print(f"  HP regained {regained:.0f} vs HP lost {lost:.0f} in those rows ({regained / max(lost, 1):.1%})")
    print("\nFRAMES (ZKGBAI armed statics in closed fights, journal unit+ = UnitFinished):")
    tot = sum(frames.values())
    for k, v in frames.most_common():
        print(f"  {k:28} {v:6} ({v / max(tot, 1):.1%})")


if __name__ == "__main__":
    main(*sys.argv[1:3])
