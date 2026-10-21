#!/usr/bin/env python3
"""Summarise the economy timelines of a series (games run with
ARENA_WIDGETS=tools/arena/econ_widget.lua): per AI (ShortName:Version
from each game's script.txt; two builds of one AI need distinct versions),
averages by minute of metal income (mex base, overdrive), energy income,
mexes, constructors alive/built/lost (home or away), army and defense
metal; and where, when and by what constructors and mexes died.

usage: econ.py <series-dir> [minutes, e.g. 3,5,8,12,16]
"""
import glob
import os
import re
import sys
from collections import Counter, defaultdict

base = sys.argv[1]
minutes = [int(m) for m in (sys.argv[2] if len(sys.argv) > 2 else "3,5,8,10,12,16,20").split(",")]
FIELDS = ["minc", "base", "od", "einc", "mex", "cons", "conbuilt", "conlost", "conaway", "army", "porc"]

timeline = defaultdict(lambda: defaultdict(lambda: defaultdict(list)))  # ai -> minute -> field -> values
deaths = defaultdict(lambda: defaultdict(Counter))  # ai -> kind -> Counter
games = Counter()


def ais_of(script):
    out = {}
    text = open(script, errors="ignore").read()
    for block in re.findall(r"\[AI\d+\]\s*\{(.*?)\}", text, re.S):
        team = re.search(r"Team=(\d+);", block)
        short = re.search(r"ShortName=([^;]+);", block)
        ver = re.search(r"Version=([^;]+);", block)
        if team and short:
            out[team.group(1)] = short.group(1) + (":" + ver.group(1) if ver else "")
    return out


def bucket(d):
    return "home" if d <= 1500 else ("mid" if d <= 3000 else "far")


for d in sorted(glob.glob(os.path.join(base, "m*"))):
    log, script = os.path.join(d, "infolog.txt"), os.path.join(d, "script.txt")
    if not (os.path.exists(log) and os.path.exists(script)):
        continue
    ai = ais_of(script)
    if len(set(ai.values())) < len(ai):
        # Same AI on both sides: tell them apart by team.
        ai = {t: f"{a}#t{t}" for t, a in ai.items()}
    seen = set()
    for line in open(log, errors="ignore"):
        m = re.search(r"ECON f=(\d+) team(\d+) (.*)", line)
        if m:
            f, team = int(m.group(1)), m.group(2)
            if team not in ai:
                continue
            seen.add(team)
            minute = f // 1800
            if f % 1800 == 0 and minute in minutes:
                vals = dict(re.findall(r"(\w+)=(-?[\d.]+)", m.group(3)))
                for k in FIELDS:
                    if k in vals:
                        timeline[ai[team]][minute][k].append(float(vals[k]))
            continue
        m = re.search(r"(CONDEATH|MEXDEATH) f=(\d+) team(\d+) .*home=(-?\d+) age=(\d+) by=(\S+)", line)
        if m and m.group(3) in ai:
            kind, f, team, home, age, by = m.groups()
            who = ai[team]
            k = "con" if kind == "CONDEATH" else "mex"
            deaths[who][k + " by"][by] += 1
            deaths[who][k + " where"][bucket(int(home))] += 1
            deaths[who][k + " when"][f"{(int(f) // 1800) // 4 * 4:02d}-{(int(f) // 1800) // 4 * 4 + 3:02d}m"] += 1
    for t in seen:
        games[ai[t]] += 1

for who in sorted(timeline):
    print(f"== {who} ({games[who]} games): averages by minute")
    print("  min " + " ".join(f"{k:>8s}" for k in FIELDS) + "   n")
    for minute in minutes:
        row = timeline[who].get(minute)
        if not row:
            continue
        n = len(row["minc"])
        print(f"  {minute:3d} " + " ".join(f"{sum(row[k]) / max(1, len(row[k])):8.1f}" for k in FIELDS) + f" {n:3d}")
    for k in ["con by", "con where", "con when", "mex by", "mex where", "mex when"]:
        c = deaths[who][k]
        if c:
            top = ", ".join(f"{name} {n}" for name, n in (sorted(c.items()) if "when" in k else c.most_common(8)))
            print(f"  {k} (deaths of {who}'s): {top}")
