#!/usr/bin/env python3
"""Score series.sh runs of ZKGBAI (as A) for home defense: mexes lost by
10:00 (the certification measure), the enemy's, income and mexes at 10:00,
wins, and what the QRF did (from the `qrf` journal records).

usage: qrflive.py <series dir>...     (each with results.csv and m*/journal/ from ZKGBAI_JOURNAL=out)
       qrflive.py --games <series dir>  (one row per game)
"""
import csv
import glob
import json
import math
import os
import sys

T10 = 18000


def journal(path):
    out = {"mex_lost": 0, "mex_lost_10": 0, "tick10": None, "qrf10": None, "qrf_last": None, "qrf_losses": 0,
           "qrf_losses_marked": 0}
    try:
        f = open(path)
    except OSError:
        return None
    with f:
        for line in f:
            try:
                r = json.loads(line)
            except ValueError:
                break
            k, fr = r.get("k"), r.get("f", 0)
            if k == "unit-" and r.get("def") == "staticmex":
                out["mex_lost"] += 1
                if fr <= T10:
                    out["mex_lost_10"] += 1
            elif k == "tick" and fr <= T10:
                out["tick10"] = r
            elif k == "qrf":
                out["qrf_last"] = r
                if fr <= T10:
                    out["qrf10"] = r
            elif k == "qrf-loss" and fr <= T10:
                out["qrf_losses"] += 1
                out["qrf_losses_marked"] += bool(r.get("marked"))
    return out


def game(series, row):
    d = os.path.join(series, f"m{row['idx']}")
    ours = 0 if row["a_slot"] == "A" else 1
    js = {}
    for p in glob.glob(os.path.join(d, "journal", "*.jsonl")):
        team = p.rsplit("-t", 1)[-1].split(".")[0]
        if team.isdigit():
            js[int(team)] = journal(p)
    us, them = js.get(ours), js.get(1 - ours)
    if not us:
        return None
    g = {"idx": int(row["idx"]), "map": row["map"], "win": row["winner"] == "A", "frame": int(row["frame"] or 0),
         "panics": int(row["panics"] or 0), "mex_lost_10": us["mex_lost_10"],
         "enemy_mex_lost_10": them["mex_lost_10"] if them else float("nan")}
    t = us["tick10"]
    if t:
        g["income_10"] = t.get("income", 0.0)
        g["mexes_10"] = t.get("spots", [0])[0]
        g["army_10"] = t.get("army", 0)
    q = us["qrf10"]
    if q:
        g["qrf_have_10"] = q.get("have", 0.0)
        g["qrf_wanted_10"] = q.get("wanted", 0.0)
        g["qrf_drafts_10"] = q.get("drafts", 0)
        g["qrf_recruited_10"] = q.get("recruited", 0)
    g["qrf_losses_10"] = us["qrf_losses"]
    g["qrf_losses_marked_10"] = us["qrf_losses_marked"]
    return g


def load(series):
    p = os.path.join(series, "results.csv")
    if not os.path.exists(p):
        return []
    with open(p) as f:
        rows = list(csv.DictReader(f))
    return [g for g in (game(series, r) for r in rows) if g]


def mean_se(xs):
    xs = [x for x in xs if x is not None and not (isinstance(x, float) and math.isnan(x))]
    if not xs:
        return float("nan"), float("nan"), 0
    m = sum(xs) / len(xs)
    if len(xs) < 2:
        return m, float("nan"), len(xs)
    v = sum((x - m) ** 2 for x in xs) / (len(xs) - 1)
    return m, math.sqrt(v / len(xs)), len(xs)


KEYS = ["win", "mex_lost_10", "enemy_mex_lost_10", "income_10", "mexes_10", "army_10", "qrf_have_10", "qrf_drafts_10",
        "qrf_losses_marked_10"]


def main(argv):
    if argv and argv[0] == "--games":
        for s in argv[1:]:
            for g in load(s):
                print(json.dumps(g))
        return
    print("| arm | n | " + " | ".join(KEYS) + " |")
    print("|---|---|" + "---|" * len(KEYS))
    base = None
    for s in argv:
        gs = load(s)
        cells = []
        for k in KEYS:
            m, se, n = mean_se([float(g[k]) if k in g else None for g in gs])
            cells.append("-" if n == 0 else f"{m:.2f} ± {se:.2f}" if k == "win" else f"{m:.1f} ± {se:.1f}")
        print(f"| {os.path.basename(s.rstrip('/'))} | {len(gs)} | " + " | ".join(cells) + " |")
        if base is None:
            base = gs
        elif base and gs:
            ma, sa, _ = mean_se([g["mex_lost_10"] for g in base])
            mb, sb, _ = mean_se([g["mex_lost_10"] for g in gs])
            print(f"|  vs first: mexes lost by 10:00 {mb - ma:+.1f} ± {math.hypot(sa, sb):.1f} | | " + " | " * (len(KEYS) - 1))


if __name__ == "__main__":
    main(sys.argv[1:])
