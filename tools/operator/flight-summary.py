#!/usr/bin/env python3
"""Summarise piloted flights (tools/arena/pilot.sh + a connectome-host pilot)
as markdown: both sides' mexes/income/army over time from the ZKGBAI
journals, the end state, the pilot's operator calls and notes, and the host's
call log (turns, calls per turn, latency, context size) when present.

usage: flight-summary.py <flight dir>...   (a dir holding match/journal/*.jsonl and
       optionally data/llm-calls.*.jsonl and conhost.log)
       flight-summary.py --series <series out dir>...   (series.sh results.csv)
"""
import csv
import glob
import json
import os
import sys
from datetime import datetime, timedelta

MARKS = (180, 300, 420, 600, 780)


def mmss(s):
    return f"{s // 60}:{s % 60:02d}"


def journal(path):
    ticks, ops, notes, end, run = {}, [], [], None, None
    with open(path) as f:
        for line in f:
            try:
                r = json.loads(line)
            except ValueError:
                break
            k = r["k"]
            if k == "tick":
                ticks[r["f"] // 30] = r
            elif k == "op":
                ops.append(r)
            elif k == "note":
                notes.append(r)
            elif k == "end":
                end = r
            elif k == "run":
                run = r
    return ticks, ops, notes, end, run


def side_rows(ticks):
    last = max(ticks) if ticks else 0
    out = []
    for s in sorted(set(MARKS) | {last}):
        t = ticks.get(s)
        if t:
            out.append((s, t["spots"][0], t["income"], t["army"], t["workers"]))
    return out


def calls(flight):
    fs = glob.glob(os.path.join(flight, "data", "llm-calls.*.jsonl"))
    if not fs:
        return None
    rows = [json.loads(l) for l in open(fs[0])]
    turns, cur = [], []
    for r in rows:
        cur.append(r)
        if (r.get("rawResponse") or {}).get("stop_reason") != "tool_use":
            turns.append(cur)
            cur = []
    if cur:
        turns.append(cur)
    durs = sorted(r["durationMs"] for r in rows)
    toks = []
    for r in rows:
        u = (r.get("rawResponse") or {}).get("usage") or {}
        toks.append(u.get("input_tokens", 0) + u.get("cache_read_input_tokens", 0) + u.get("cache_creation_input_tokens", 0))
    per_turn = sorted(len(t) for t in turns)
    models = sorted({r["requestSummary"].get("model") for r in rows if r.get("requestSummary")})
    return {
        "models": models,
        "calls": len(rows),
        "turns": len(turns),
        "calls_per_turn_median": per_turn[len(per_turn) // 2] if per_turn else 0,
        "calls_per_turn_max": max(per_turn) if per_turn else 0,
        "ms_per_call_median": durs[len(durs) // 2] if durs else 0,
        "context_first": toks[0] if toks else 0,
        "context_last": toks[-1] if toks else 0,
    }


def flight(path):
    out = [f"## {path}\n"]
    js = sorted(glob.glob(os.path.join(path, "match", "journal", "*.jsonl")))
    if not js:
        out.append("no journals\n")
        return "\n".join(out)
    sides = {}
    for j in js:
        team = j.rsplit("-t", 1)[1].split(".")[0]
        sides[team] = journal(j)
    pilot_team = max(sides, key=lambda t: len(sides[t][1]))
    runs = sides[pilot_team][4] or {}
    out.append(f"map {runs.get('map')}, pilot = team {pilot_team}, build {runs.get('build')}, options {runs.get('options')}\n")
    out.append("| time | " + " | ".join(f"t{t} mexes | t{t} income | t{t} army" for t in sorted(sides)) + " |")
    out.append("|---|" + "---|" * (3 * len(sides)))
    marks = sorted({s for t in sides for (s, *_) in side_rows(sides[t][0])})
    for s in marks:
        cells = []
        for t in sorted(sides):
            row = {r[0]: r for r in side_rows(sides[t][0])}.get(s)
            cells.append(f"{row[1]} | {row[2]} | {row[3]}" if row else " | | ")
        out.append(f"| {mmss(s)} | " + " | ".join(cells) + " |")
    for t in sorted(sides):
        end = sides[t][3]
        out.append(f"- team {t} end: {end and (end.get('state'), end.get('time'))}")
    _, ops, notes, _, _ = sides[pilot_team]
    by_tool = {}
    for o in ops:
        by_tool[o.get("tool", o.get("event", "?"))] = by_tool.get(o.get("tool", o.get("event", "?")), 0) + 1
    out.append(f"- pilot operator calls: {len(ops)}: " + ", ".join(f"{n} {t}" for t, n in sorted(by_tool.items(), key=lambda x: -x[1])))
    c = calls(path)
    if c:
        out.append(f"- host: models {c['models']}, {c['calls']} calls in {c['turns']} turns (median {c['calls_per_turn_median']}, max {c['calls_per_turn_max']} per turn), "
                   f"{c['ms_per_call_median']} ms per call median, context {c['context_first']} -> {c['context_last']} tokens")
    out.append("\n### pilot notes\n")
    for n in notes:
        out.append(f"- {mmss(n['f'] // 30)} {n['text']}")
    out.append("")
    return "\n".join(out)


import re

STAT_RE = re.compile(r"ARENA_STATS f=(\d+) team0\(.*?minc=([\d.]+) mprod=(\d+).*?\) team1\(.*?minc=([\d.]+) mprod=(\d+)")


def arena_stats(infolog):
    """Metal income and production per team at each minute, from the arena
    widget's ARENA_STATS lines."""
    out = {}
    try:
        with open(infolog, errors="replace") as f:
            for line in f:
                m = STAT_RE.search(line)
                if m:
                    out[int(m.group(1)) // 30] = ((float(m.group(2)), int(m.group(3))), (float(m.group(4)), int(m.group(5))))
    except OSError:
        pass
    return out


def series(path):
    """A series.sh run: the result per game, and A's and B's metal income at
    3:00, 5:00 and 10:00 (A is team 0 when a_slot is A)."""
    out = [f"## series {path}\n"]
    csvp = os.path.join(path, "results.csv")
    if not os.path.exists(csvp):
        out.append("no results.csv\n")
        return "\n".join(out)
    with open(csvp) as f:
        rows = list(csv.DictReader(f))
    out.append("| game | map | A side | winner | frame | A income 3:00 / 5:00 / 10:00 | B income 3:00 / 5:00 / 10:00 | A prod 10:00 | B prod 10:00 |")
    out.append("|---|---|---|---|---|---|---|---|---|")
    sums = {"A": [0.0, 0.0, 0.0], "B": [0.0, 0.0, 0.0]}
    n = 0
    for r in rows:
        stats = arena_stats(os.path.join(path, f"m{r['idx']}", "infolog.txt"))
        a_is_0 = r["a_slot"] == "A"
        def side(s, which):
            t = stats.get(s)
            if not t:
                return None
            return t[0] if (which == "A") == a_is_0 else t[1]
        inc = {w: [side(s, w) for s in (180, 300, 600)] for w in ("A", "B")}
        prod = {w: side(600, w) for w in ("A", "B")}
        fmt = lambda xs: " / ".join("-" if x is None else f"{x[0]:.0f}" for x in xs)
        out.append(f"| {r['idx']} | {r['map']} | {r['a_slot']} | {r['winner']} | {r['frame']} | {fmt(inc['A'])} | {fmt(inc['B'])} | "
                   f"{'-' if prod['A'] is None else prod['A'][1]} | {'-' if prod['B'] is None else prod['B'][1]} |")
        if all(x is not None for x in inc["A"] + inc["B"]):
            n += 1
            for w in ("A", "B"):
                for i in range(3):
                    sums[w][i] += inc[w][i][0]
    if n:
        out.append(f"\nmean income over {n} games: A {' / '.join(f'{v / n:.1f}' for v in sums['A'])}, B {' / '.join(f'{v / n:.1f}' for v in sums['B'])}")
    out.append("")
    return "\n".join(out)


def main(argv):
    mode = "flight"
    parts = []
    for a in argv:
        if a == "--series":
            mode = "series"
            continue
        parts.append(series(a) if mode == "series" else flight(a))
    print("\n".join(parts))


if __name__ == "__main__":
    main(sys.argv[1:])
