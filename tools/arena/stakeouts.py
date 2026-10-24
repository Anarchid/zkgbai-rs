#!/usr/bin/env python3
"""Read the `stakeout` journal records of one or more matches.

usage: stakeouts.py <match dir or journal .jsonl>...

For each ZKGBAI side with stakeout records: posts planned and the share of
summaries with each manned, pickets claimed and lost (by duty, with the
attacker's unit type), sightings (by enemy type, and whether followed),
lead time from the first picket sighting of an armed mobile enemy to the
first asset loss to a raid (`qrf-loss` records), and the drain in metal.
"""
import collections
import glob
import json
import os
import sys

SCOUT_COST = {"spiderscout": 25, "vehscout": 40, "shieldscout": 20}


def journals(arg):
    if arg.endswith(".jsonl"):
        return [arg]
    return sorted(glob.glob(os.path.join(arg, "**", "journal", "*.jsonl"), recursive=True))


def read(path):
    recs = []
    with open(path) as f:
        for line in f:
            try:
                r = json.loads(line)
            except ValueError:
                continue
            recs.append(r)
    return recs


def summarise(path):
    recs = read(path)
    stake = [r for r in recs if r.get("k") == "stakeout"]
    if not stake:
        return None
    out = {"journal": os.path.basename(path)}
    summaries = [r for r in stake if r.get("event") == "summary"]
    lost = [r for r in stake if r.get("event") == "lost"]
    sightings = [r for r in stake if r.get("event") == "sighting"]
    posts = [r for r in stake if r.get("event") == "posts"]
    out["posts_planned"] = max((len(r["posts"]) for r in posts), default=0)
    manned = collections.Counter()
    for r in summaries:
        posts_now = r.get("posts")
        if not isinstance(posts_now, list):
            continue
        for p in posts_now:
            if p.get("manned") is not None:
                manned[p["choke"]] += 1
    out["summaries"] = len(summaries)
    out["manned_share"] = {c: round(n / len(summaries), 2) for c, n in manned.items()} if summaries else {}
    last = summaries[-1] if summaries else {}
    out["claimed"] = last.get("claimed")
    out["lost"] = len(lost)
    out["lost_by_duty"] = dict(collections.Counter(r["duty"] for r in lost))
    out["killed_by"] = dict(collections.Counter((r.get("attacker") or {}).get("def", "?") for r in lost))
    out["drain_metal"] = sum(SCOUT_COST.get(r.get("def"), 25) for r in lost)
    out["sightings"] = len(sightings)
    out["sightings_by_def"] = dict(collections.Counter(r["def"] or "radar" for r in sightings).most_common(8))
    out["flights"], out["follows"] = last.get("flights"), last.get("follows")
    out["sorties"], out["kills"] = last.get("sorties"), last.get("kills")
    mobile = [r for r in sightings if r["def"] and not r["def"].startswith("turret") and not r["def"].startswith("static")]
    first_sight = min((r["t"] for r in mobile), default=None)
    losses = [r for r in recs if r.get("k") == "qrf-loss"]
    first_loss = min((r.get("t", r.get("f", 0) // 30) for r in losses), default=None)
    out["first_mobile_sighting_s"] = first_sight
    out["first_asset_loss_s"] = first_loss
    if first_sight is not None and first_loss is not None:
        out["lead_s"] = first_loss - first_sight
    minutes = collections.Counter()
    for r in summaries:
        for m in r.get("pickets") or []:
            minutes[m["duty"]] += 0.5
    out["duty_minutes"] = dict(minutes)
    return out


def main():
    for arg in sys.argv[1:]:
        for j in journals(arg):
            s = summarise(j)
            if s:
                print(json.dumps(s, indent=1))


if __name__ == "__main__":
    main()
