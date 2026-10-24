#!/usr/bin/env python3
"""Read combat logs (tools/arena/combatlog_widget.lua) and summarise them.

usage: combatlog.py [-w] <file.tsv[.gz] | dir>...
  Directories are searched for combatlog.tsv.gz (arena match dirs) and
  combatlog/*.tsv (a Spring write dir).
  -w  per-weapon table (shots, hit rate against units, mean damage); the
      default prints row counts and the unmatched share.

load() returns {kind: [row dict, ...]} for use from other scripts.
"""
import gzip
import os
import sys
from collections import defaultdict


def _open(path):
    return gzip.open(path, "rt") if path.endswith(".gz") else open(path)


def load(path, into=None):
    """Rows of one log, by kind, as dicts keyed by the header columns."""
    rows = into if into is not None else defaultdict(list)
    headers = {}
    with _open(path) as fh:
        for line in fh:
            line = line.rstrip("\n")
            if not line:
                continue
            if line.startswith("#"):
                parts = line[1:].split("\t")
                if len(parts) > 1:
                    headers[parts[0]] = parts[1:]
                continue
            parts = line.split("\t")
            kind = parts[0]
            cols = headers.get(kind)
            if cols is None:
                continue
            row = dict(zip(cols, parts[1:]))
            row["_file"] = path
            rows[kind].append(row)
    return rows


def find(paths):
    for p in paths:
        if os.path.isdir(p):
            for root, _, files in os.walk(p):
                for f in files:
                    if f == "combatlog.tsv.gz" or (root.endswith("combatlog") and f.endswith(".tsv")):
                        yield os.path.join(root, f)
        else:
            yield p


def num(v, default=0.0):
    try:
        return float(v)
    except (TypeError, ValueError):
        return default


def summary(rows):
    print("rows:", " ".join(f"{k}={len(v)}" for k, v in sorted(rows.items())))
    shots = rows.get("shot", [])
    by_src = defaultdict(int)
    for s in shots:
        by_src[s["src"]] += 1
    print("shots by source:", dict(by_src), " unmatched damage rows:", len(rows.get("dmg", [])))
    env = defaultdict(float)
    for e in rows.get("env", []):
        env[e["kind"]] += num(e["dmg"])
    if env:
        print("engine damage:", ", ".join(f"{k}={v:.0f}" for k, v in sorted(env.items(), key=lambda kv: -kv[1])))


def per_weapon(rows):
    agg = defaultdict(lambda: [0, 0, 0, 0.0, 0, 0.0])  # shots, aimed at units, hit a unit, damage, hit target, lag
    for s in rows.get("shot", []):
        a = agg[s["wd"]]
        n = int(num(s["n"], 1))
        a[0] += n
        if s["tt"] == "u":
            a[1] += n
            if num(s["hits"]) > 0:
                a[2] += n
            if num(s["dmgt"]) > 0:
                a[4] += n
        a[3] += num(s["dmg"])
    print(f"{'weapon':28} {'shots':>7} {'at unit':>8} {'hit any':>8} {'hit tgt':>8} {'p(hit)':>7} {'dmg/shot':>9}")
    for wd, a in sorted(agg.items(), key=lambda kv: -kv[1][0]):
        p = a[2] / a[1] if a[1] else float("nan")
        print(f"{wd:28} {a[0]:7d} {a[1]:8d} {a[2]:8d} {a[4]:8d} {p:7.2f} {a[3] / a[0] if a[0] else 0:9.1f}")


def main(argv):
    weapons = "-w" in argv
    paths = [a for a in argv if a != "-w"]
    rows = defaultdict(list)
    n = 0
    for f in find(paths):
        load(f, rows)
        n += 1
    print(f"{n} log(s)")
    summary(rows)
    if weapons:
        per_weapon(rows)


if __name__ == "__main__":
    main(sys.argv[1:])
