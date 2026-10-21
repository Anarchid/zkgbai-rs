#!/usr/bin/env python3
"""Aggregate the census of a series (games run with
ARENA_WIDGETS=tools/arena/census_widget.lua): per AI (ShortName:Version
from each game's script.txt), per unit type: built, lost, enemy metal
killed, and metal killed per metal built; wins and final metal income.

Forcing CircuitAI onto one factory shows how it plays it, e.g.
  ARENA_WIDGETS=tools/arena/census_widget.lua tools/arena/series.sh ... \
      "ZKGBAI?factory=factoryjump" \
      "CircuitAI:stable?disabledunits=factorycloak+factoryshield+...+striderhub"

usage: census.py <series-dir> [unit-prefix ...]
Unit costs come from the game's unit files (ZK_UNITS, default
~/.spring/games/zk.sdd/units).
"""
import glob
import os
import re
import sys
from collections import defaultdict

base = sys.argv[1]
prefixes = sys.argv[2:] or ["jump", "turret", "dyntrainer", "cloak", "shield"]

agg = defaultdict(lambda: defaultdict(lambda: [0, 0, 0.0]))
wins = defaultdict(int)
games = defaultdict(int)
income = defaultdict(list)
costs = {}

for d in sorted(glob.glob(os.path.join(base, "m*"))):
    log = os.path.join(d, "infolog.txt")
    script = os.path.join(d, "script.txt")
    if not (os.path.exists(log) and os.path.exists(script)):
        continue
    text = open(script, errors="ignore").read()
    team_ai = {}
    for block in re.findall(r"\[AI\d+\]\s*\{(.*?)\}", text, re.S):
        team = re.search(r"Team=(\d+);", block)
        short = re.search(r"ShortName=([^;]+);", block)
        ver = re.search(r"Version=([^;]+);", block)
        if team and short:
            team_ai[team.group(1)] = short.group(1) + (":" + ver.group(1) if ver else "")
    lines = open(log, errors="ignore").read().splitlines()
    last = {}
    for l in lines:
        m = re.search(r"CENSUS f=\d+ team(\d+) (.*)", l)
        if m:
            last[m.group(1)] = m.group(2)
    final = [l for l in lines if "ARENA_FINAL" in l]
    over = [l for l in lines if "ARENA_GAMEOVER" in l]
    if not last or not final:
        continue
    for team, body in last.items():
        ai = team_ai.get(team, "team" + team)
        games[ai] += 1
        for name, v in re.findall(r"(\S+)=(\d+/\d+/[\d.]+)", body):
            b, lo, k = v.split("/")
            a = agg[ai][name]
            a[0] += int(b)
            a[1] += int(lo)
            a[2] += float(k)
        m = re.search(r"team%s\(ally\d+ [^)]*minc=([\d.]+)" % team, final[0])
        if m:
            income[ai].append(float(m.group(1)))
    if over:
        w = re.search(r"winners=(\d+)", over[0])
        if w:
            # 1v1: ally n = team n
            wins[team_ai.get(w.group(1), "?")] += 1

units_dir = os.environ.get("ZK_UNITS", os.path.expanduser("~/.spring/games/zk.sdd/units"))
if os.path.isdir(units_dir):
    for f in os.listdir(units_dir):
        if f.endswith(".lua"):
            m = re.search(r"\n\s*metalCost\s*=\s*([\d.]+)", open(os.path.join(units_dir, f), errors="ignore").read())
            if m:
                costs[f[:-4]] = float(m.group(1))

for ai in sorted(agg):
    inc = income[ai]
    print(f"== {ai}: {games[ai]} games, {wins[ai]} won, final income avg {sum(inc)/max(1,len(inc)):.0f}")
    rows = []
    for name, (b, lo, k) in agg[ai].items():
        if not any(name.startswith(p) for p in prefixes):
            continue
        c = costs.get(name)
        eff = f"{k / (b * c):.2f}" if c and b else "-"
        rows.append((name, b, lo, k, eff))
    rows.sort(key=lambda r: -r[3])
    for name, b, lo, k, eff in rows:
        print(f"  {name:24s} built {b:4d} lost {lo:4d} killed {k:8.0f}  per metal {eff}")
