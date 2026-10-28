#!/usr/bin/env python3
"""Rearm, jumps and manual fire in the combat logs, by controller (exploratory, mechanics round 2 triage).

usage: specials.py <refit-dir> [ai games=500]
  A random sample of arena games plus every replay-harvested and live human log. Controller =
  the shooter's team: ZKGBAI / Circuit / human (replays and live logs: every non-AI team).
  - manual fire: shots of commandFire weapons (commander specials, strider specials) per game
  - bombers: interval between a bomber's consecutive shots vs its weapon reload
  - jumps: unit rows of jumpers moving faster than 2.5x their ground speed (and > 4 elmos/frame)
  - commw: whether the sim's commander weapon list carries the manual-fire weapons at all
"""
import os
import pickle
import random
import re
import sys
from collections import Counter, defaultdict

import numpy as np
import pandas as pd

import combatdata
import split
import units as unitmod

MANUAL = re.compile(r"commweapon_(?:disintegrator|concussion|clusterbomb|disruptorbomb|napalmgrenade|multistunner|slamrocket|sunburst)"
                    r"|^(?:armcom1_disintegrator|corcom1_disintegrator|gunshipkrow_clusterbomb|shipcarrier_disarm_rocket|"
                    r"striderbantha_emp_missile|striderdante_napalm_rockets_salvo|striderdetriment_obliteration_blaster|"
                    r"striderscorpion_multilightning|subtacmissile_tacnuke|staticnuke_crblmssl)$")
BOMBERS = ("bomberprec", "bomberriot", "bomberdisarm", "bomberheavy", "bomberassault", "bomberstrike")
JUMPERS = ("jumpraid", "jumpassault", "jumpsumo", "jumpbomb", "jumpcon", "jumpaa", "jumpscout", "jumpskirm", "jumparty", "jumpblackhole")


def controller(meta, team, human_log):
    t = meta["teams"].get(int(team)) if team is not None and team >= 0 else None
    if t is None:
        return "?"
    kind, name = t[1], t[2]
    if kind == "ai" or "ZKGBAI" in name or "Circuit" in name:
        return "ZKGBAI" if "ZKGBAI" in name else "Circuit" if "Circuit" in name else "other AI"
    return "human" if human_log else "?"


def main(rdir, n_ai=500):
    ud = unitmod.load(os.path.join(rdir, "unitdefs.tsv"))
    wtab = pickle.load(open(os.path.join(rdir, "wtab.pkl"), "rb"))
    commw = pickle.load(open(os.path.join(rdir, "commw.pkl"), "rb"))
    paths = list(combatdata.cached(os.path.join(rdir, "cache")))
    hum = [p for p in paths if split.split_of("", os.path.basename(p)[:-4]) in ("human", "replay")]
    ai = [p for p in paths if p not in set(hum)]
    random.seed(21)
    sample = random.sample(ai, min(int(n_ai), len(ai))) + hum
    manual = Counter(); games = Counter(); comm_games = Counter(); mshots_by_w = Counter()
    gaps = defaultdict(list)
    jump_rows = Counter(); jumper_units = Counter(); jumped_units = Counter()
    for p in sample:
        d = combatdata.load_cached(p)
        meta = d["meta"]
        human_log = p in hum
        teams = {t: controller(meta, t, human_log) for t in meta["teams"]}
        for c in set(teams.values()):
            games[c] += 1
        s = d.get("shot")
        if s is not None and len(s):
            wd = s["wd"].astype(str)
            m = wd.str.contains(MANUAL)
            for (team, w), k in s[m].groupby([s.loc[m, "oteam"], wd[m]]).size().items():
                c = teams.get(int(team), "?")
                manual[c] += int(k); mshots_by_w[(c, re.sub(r"^comm\w*?_(commweapon)", r"\1", w))] += int(k)
            odef = s["odef"].astype(str)
            comm_owner = odef.str.match(r"^(dyn|comm|armcom|corcom|benzcom|cremcom)")
            for team in s.loc[comm_owner, "oteam"].unique():
                comm_games[teams.get(int(team), "?")] += 1
            b = s[odef.isin(BOMBERS)]
            for (own, odf), g in b.groupby([b["own"], b["odef"].astype(str)]):
                f0 = np.sort(g["f0"].values)
                if len(f0) >= 2:
                    dd = np.diff(f0)
                    gaps[(teams.get(int(g["oteam"].iloc[0]), "?"), odf)].extend((dd[dd > 15] / 30.0).tolist())
        u = d.get("unit")
        if u is not None and len(u):
            ju = u[u["udef"].astype(str).isin(JUMPERS)]
            for (uid, udef, team), g in ju.groupby([ju["uid"], ju["udef"].astype(str), ju["uteam"]]):
                dd = ud.get(udef)
                if dd is None or dd.speed <= 0:
                    continue
                c = teams.get(int(team), "?")
                fast = (g["spd"] > max(2.5 * dd.speed, 4.0)).sum()
                jumper_units[(c, udef)] += 1
                jump_rows[(c, udef)] += int(fast)
                if fast:
                    jumped_units[(c, udef)] += 1
    print(f"games: {dict(games)}")
    print("\nmanual-fire shots per game the controller played (commander games in brackets):")
    for c in sorted(games):
        print(f"  {c:8} {manual[c] / max(games[c], 1):6.2f}  [{comm_games[c]} games with a commander firing]")
    print("  top manual-fire weapons:", ", ".join(f"{c}:{w} {k}" for (c, w), k in mshots_by_w.most_common(12)))
    inl = sorted({w for ws in commw.values() for w in ws if MANUAL.search(w)})
    print(f"\nsim commander weapon lists (commw.pkl, {len(commw)} defs) holding a manual-fire weapon: {inl or 'none'}")
    print("\nbombers: seconds between a bomber's consecutive shots (> 0.5 s) vs weapon reload:")
    for (c, odf), g in sorted(gaps.items()):
        if len(g) < 20:
            continue
        rl = [float(wtab.loc[w.name, "reload"]) for w in ud.get(odf).weapons if w.name in wtab.index] if ud.get(odf) else []
        print(f"  {c:8} {odf:14} n {len(g):5}  median {np.median(g):5.1f} s  p25 {np.percentile(g, 25):5.1f}  p75 {np.percentile(g, 75):5.1f}   weapon reload {min(rl) if rl else float('nan'):.1f} s")
    print("\njumpers in combat: share that jumped at least once (unit rows faster than 2.5x ground speed):")
    for (c, udef), n in sorted(jumper_units.items()):
        if n >= 20:
            print(f"  {c:8} {udef:13} units {n:5}  jumped {jumped_units[(c, udef)] / n:.2f}  fast rows/unit {jump_rows[(c, udef)] / n:.2f}")


if __name__ == "__main__":
    main(*sys.argv[1:3])
