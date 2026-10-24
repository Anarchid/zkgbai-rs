#!/usr/bin/env python3
"""Score assassination strikes from ZKGBAI infologs (the `strikes:` lines).

usage: strikes.py [-v] <infolog or match dir>...

Per game and pooled: strikes started by fix kind, outcomes (killed, lapsed,
wiped, ran), metal lost in strikes per sniper killed, refusals, our units
lost to snipers outside strikes (count, metal), and enemy snipers that died
unstruck. Run with python3 -I.
"""
import os
import re
import sys

START = re.compile(r"strikes: #(\d+) on a (\w+) (seen|on radar|crossed|a bearing) at \((-?\d+), (-?\d+)\): (\d+) units \(([\d.]+) metal, ([\d.]+)/s\) go, the slowest in ([\d.]+) s; expected loss ([\d.]+), profit (-?[\d.]+) \(([\d.]+) x ([\d.]+)(?:, averted (-?[\d.]+))?\)(, escorts)?")
END = re.compile(r"strikes: #(\d+) (killed|lapsed|wiped|ran): the (\w+) (dead|not) after (\d+) s; lost ([\d.]+) metal \((\d+) of (\d+) units\), (\d+) back")
SEES = re.compile(r"strikes: #(\d+) sees its")
REFUSED = re.compile(r"strikes: no strike on the (\w+) (seen|on radar|crossed|a bearing) at \((-?\d+), (-?\d+)\): (.*)")
SNIPED = re.compile(r"strikes: a (\w+) \(([\d.]+)\) lost to a (\w+)")
DIED = re.compile(r"strikes: a (\w+) died (struck|unstruck)")
SEEN = re.compile(r"strikes: a (\w+) first seen")
TIME = re.compile(r"\[f=(\d+)\]")


def infologs(args):
    for a in args:
        if os.path.isdir(a):
            for root, _, files in os.walk(a):
                if "infolog.txt" in files:
                    yield os.path.join(root, "infolog.txt")
        else:
            yield a


def score(path, verbose):
    g = dict(started=0, killed=0, lapsed=0, wiped=0, ran=0, lost=0.0, refused=0, sniped=0, sniped_metal=0.0,
             unstruck=0, struck=0, seen={}, died={}, averted=0.0, worth=0.0, escorted_go=0,
             by_fix={}, kills_by_fix={}, refusals={}, lines=[])
    starts = {}
    for line in open(path, errors="replace"):
        if "strikes:" not in line:
            continue
        f = TIME.search(line)
        t = int(f.group(1)) // 30 if f else 0
        m = START.search(line)
        if m:
            sid, kind, fix = m.group(1), m.group(2), m.group(3)
            g["started"] += 1
            g["by_fix"][fix] = g["by_fix"].get(fix, 0) + 1
            starts[sid] = fix
            g["worth"] += float(m.group(13))
            g["averted"] += float(m.group(14) or 0)
            g["escorted_go"] += 1 if m.group(15) else 0
            g["lines"].append(f"{t//60:2d}:{t%60:02d} #{sid} {kind} {fix}: {m.group(6)} units {float(m.group(7)):.0f} metal, loss {float(m.group(10)):.0f} profit {float(m.group(11)):.0f} (worth {float(m.group(13)):.0f}, averted {float(m.group(14) or 0):.0f}){' escorted' if m.group(15) else ''}")
            continue
        m = END.search(line)
        if m:
            sid, why = m.group(1), m.group(2)
            g[why] += 1
            g["lost"] += float(m.group(6))
            if why == "killed":
                fix = starts.get(sid, "?")
                g["kills_by_fix"][fix] = g["kills_by_fix"].get(fix, 0) + 1
            g["lines"].append(f"{t//60:2d}:{t%60:02d} #{sid} {why} after {m.group(5)} s, lost {float(m.group(6)):.0f} ({m.group(7)}/{m.group(8)})")
            continue
        m = REFUSED.search(line)
        if m:
            g["refused"] += 1
            why = m.group(5)
            cls = ("nobody in reach" if "0 in reach" in why else "too little power in reach" if "in reach bring" in why
                   else "gate, escorted" if "escorts" in why else "gate, quiet")
            g["refusals"][cls] = g["refusals"].get(cls, 0) + 1
            g["lines"].append(f"{t//60:2d}:{t%60:02d} refused {m.group(1)} {m.group(2)} [{cls}]: {why[:90]}")
            continue
        m = SNIPED.search(line)
        if m:
            g["sniped"] += 1
            g["sniped_metal"] += float(m.group(2))
            continue
        m = DIED.search(line)
        if m:
            g["died"][m.group(1)] = g["died"].get(m.group(1), 0) + 1
            g["struck" if m.group(2) == "struck" else "unstruck"] += 1
            continue
        m = SEEN.search(line)
        if m:
            g["seen"][m.group(1)] = g["seen"].get(m.group(1), 0) + 1
    return g


def show(name, g):
    per_kill = g["lost"] / g["killed"] if g["killed"] else float("nan")
    share = g["averted"] / g["worth"] if g["worth"] else 0.0
    fmt = lambda d: ", ".join(f"{k} {v}" for k, v in d.items()) or "-"
    print(f"{name}: strikes {g['started']} ({fmt(g['by_fix'])}; escorted {g['escorted_go']}; averted share of worth {share:.2f}); "
          f"killed {g['killed']} ({fmt(g['kills_by_fix'])}), lapsed {g['lapsed']}, wiped {g['wiped']}, ran {g['ran']}; "
          f"metal lost in strikes {g['lost']:.0f} ({per_kill:.0f}/kill); refused {g['refused']} ({fmt(g['refusals'])}); "
          f"sniped outside strikes {g['sniped']} ({g['sniped_metal']:.0f} metal); "
          f"snipers seen {fmt(g['seen'])}, died {fmt(g['died'])} (struck {g['struck']}, unstruck {g['unstruck']})")


def main():
    args = sys.argv[1:]
    verbose = "-v" in args
    args = [a for a in args if a != "-v"]
    pooled = None
    for path in infologs(args):
        g = score(path, verbose)
        if g["started"] == 0 and g["sniped"] == 0 and g["refused"] == 0 and not g["seen"] and not g["died"]:
            continue
        show(os.path.relpath(path), g)
        if verbose:
            for l in g["lines"]:
                print("   ", l)
        if pooled is None:
            pooled = {k: (dict(v) if isinstance(v, dict) else v) for k, v in g.items() if k != "lines"}
        else:
            for k, v in g.items():
                if k == "lines":
                    continue
                if isinstance(v, dict):
                    for kk, vv in v.items():
                        pooled[k][kk] = pooled[k].get(kk, 0) + vv
                else:
                    pooled[k] += v
    if pooled:
        show("POOLED", pooled)


if __name__ == "__main__":
    main()
