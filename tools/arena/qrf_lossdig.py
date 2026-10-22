#!/usr/bin/env python3
"""Classify each defender loss in qrf games run with qrf_trace=2.

usage: lossdig.py DIR [DIR ...]   (match dirs or series dirs)

For each qrfloss: the trace just before it (teams, raids), the raid nearest
the loss, the nearest team and how far, and whether the cover algorithm had
a team on that raid (its dispatch lines). Classes:
  noforce   no team left
  unseen    no raid within 700 of the loss in the trace (not tracked)
  overload  every team was on some other raid
  late      a team was on this raid but more than 500 from the loss
  near      a team was within 500 (fought there and lost the asset anyway)
  idle      some team was free (on station) and none on this raid
"""
import os, re, sys, math, glob, collections

TRACE = re.compile(r"qrftrace: t=(\d+) teams=\[(.*?)\] raids=\[(.*?)\]")
LOSS = re.compile(r"qrfloss: t=(\d+) (\S+) at (-?\d+),(-?\d+)")
ON = re.compile(r"qrf cover: team (\d+)@(-?\d+),(-?\d+) on raid@(-?\d+),(-?\d+)")
FIGHT = re.compile(r"qrf cover: team (\d+) fights raid@(-?\d+),(-?\d+)")
TEAM = re.compile(r"(\d+):(\d+)@(-?\d+),(-?\d+)")
RAID = re.compile(r"(\d+)@(-?\d+),(-?\d+)v(-?\d+),(-?\d+)")


def d(a, b):
    return math.hypot(a[0] - b[0], a[1] - b[1])


def dig(path):
    traces, losses, dispatch = [], [], collections.defaultdict(list)
    for line in open(path, errors="replace"):
        if "ZKGBAI-def" not in line:
            continue
        m = TRACE.search(line)
        if m:
            t = int(m.group(1))
            teams = [(int(a), int(n), (int(x), int(z))) for a, n, x, z in TEAM.findall(m.group(2))]
            raids = [(int(n), (int(x), int(z))) for n, x, z, _, _ in RAID.findall(m.group(3))]
            traces.append((t, teams, raids))
            continue
        m = LOSS.search(line)
        if m:
            losses.append((int(m.group(1)), m.group(2), (int(m.group(3)), int(m.group(4)))))
            continue
        f = re.search(r"\]\[f=(\d+)\]", line)
        t = int(f.group(1)) // 30 if f else 0
        m = ON.search(line)
        if m:
            dispatch[t].append((int(m.group(1)), (int(m.group(4)), int(m.group(5)))))
        m = FIGHT.search(line)
        if m:
            dispatch[t].append((int(m.group(1)), (int(m.group(2)), int(m.group(3)))))
    out = []
    for (t, name, p) in losses:
        before = [tr for tr in traces if tr[0] <= t and tr[0] >= t - 6]
        if not before:
            out.append(("unseen", t, name))
            continue
        tt, teams, raids = before[-1]
        if not teams:
            out.append(("noforce", t, name))
            continue
        near_raids = [r for r in raids if d(r[1], p) < 700]
        if not near_raids:
            out.append(("unseen", t, name))
            continue
        raid = min(near_raids, key=lambda r: d(r[1], p))
        on = []
        for dt in range(t - 6, t + 1):
            on.extend(dispatch.get(dt, []))
        busy = {tm for tm, rp in on}
        mine = {tm for tm, rp in on if d(rp, raid[1]) < 600}
        nearest = min(d(tm[2], p) for tm in teams)
        if mine:
            cls = "near" if nearest < 500 else "late"
        elif len(busy) >= len(teams):
            cls = "overload"
        else:
            cls = "idle"
        out.append((cls, t, name))
    return out


def main():
    paths = []
    for a in sys.argv[1:]:
        if os.path.isfile(os.path.join(a, "infolog.txt")):
            paths.append(os.path.join(a, "infolog.txt"))
        else:
            paths.extend(sorted(glob.glob(os.path.join(a, "*", "infolog.txt"))))
    by_arm = collections.defaultdict(collections.Counter)
    for p in paths:
        arm = os.path.basename(os.path.dirname(p)).split("-")[-1]
        for cls, t, name in dig(p):
            by_arm[arm][cls] += 1
    for arm, c in sorted(by_arm.items()):
        tot = sum(c.values())
        print(f"{arm:20s} {tot:4d} losses: " + " ".join(f"{k}={v}" for k, v in c.most_common()))


if __name__ == "__main__":
    main()
