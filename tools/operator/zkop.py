#!/usr/bin/env python3
"""Talk to running ZKGBAI instances (the operator API), and read run journals.

usage:
  zkop.py ls                                  running instances (with the operator on)
  zkop.py call [-t TEAM|-u URL] TOOL [k=v ...] call a tool; values parse as JSON when they can
  zkop.py tools [-t TEAM|-u URL]              list the tools
  zkop.py events [-t TEAM|-u URL] [-k kinds] [--since N]
                                              follow the event stream (Ctrl-C ends)
  zkop.py journal FILE [-k kinds] [-g text]   summarize a run journal, or print its records

Instances announce themselves in <tmp>/zkgbai-operator/<pid>-t<team>.json while they run.
Examples:
  zkop.py call status
  zkop.py call -t 1 enemies kind=porc limit=10
  zkop.py call order units=[1234,1235] cmd=fight x=3000 z=2500
  zkop.py journal run.jsonl -k op,note
"""

import argparse
import glob
import json
import os
import sys
import tempfile
import urllib.error
import urllib.request


def instances():
    out = []
    for path in sorted(glob.glob(os.path.join(tempfile.gettempdir(), "zkgbai-operator", "*.json"))):
        try:
            with open(path) as f:
                note = json.load(f)
        except (OSError, ValueError):
            continue
        try:
            os.kill(int(note.get("pid", 0)), 0)
        except (OSError, ValueError):
            continue  # left behind by a crashed engine
        out.append(note)
    return out


def pick_url(args):
    if args.url:
        return args.url.rstrip("/")
    found = instances()
    if args.team is not None:
        found = [i for i in found if i.get("team") == args.team]
    if not found:
        sys.exit("no running ZKGBAI with the operator on (start it with ZKGBAI_OPERATOR=on)")
    if len(found) > 1 and args.team is None:
        teams = ", ".join(str(i.get("team")) for i in found)
        print(f"several instances (teams {teams}); using team {found[0].get('team')} (-t picks)", file=sys.stderr)
    return found[0]["url"]


def fetch(url, body=None):
    data = None if body is None else json.dumps(body).encode()
    req = urllib.request.Request(url, data=data, headers={"Content-Type": "application/json"})
    try:
        with urllib.request.urlopen(req, timeout=30) as r:
            return json.load(r)
    except urllib.error.HTTPError as e:
        try:
            return json.load(e)
        except ValueError:
            sys.exit(f"{e.code} {e.reason}")


def parse_value(v):
    try:
        return json.loads(v)
    except ValueError:
        return v


def cmd_ls(_):
    found = instances()
    if not found:
        print("none")
    for i in found:
        print(f"team {i.get('team')}  {i.get('url')}  {i.get('map')}  pid {i.get('pid')}  journal {i.get('journal')}")


def cmd_call(args):
    params = {}
    for kv in args.args:
        k, _, v = kv.partition("=")
        params[k] = parse_value(v)
    print(json.dumps(fetch(f"{pick_url(args)}/call/{args.tool}", params), indent=1))


def cmd_tools(args):
    for t in fetch(f"{pick_url(args)}/tools")["tools"]:
        props = t["inputSchema"].get("properties", {})
        req = set(t["inputSchema"].get("required", []))
        params = " ".join(f"{k}" if k in req else f"[{k}]" for k in props)
        acts = "" if t.get("annotations", {}).get("readOnlyHint", True) else "  (acts)"
        print(f"{t['name']} {params}{acts}\n    {t['description']}")


def line_text(r):
    k = r.get("k")
    if k == "log":
        return r.get("msg", "")
    if k in ("unit+", "enemy+"):
        return f"{r.get('def')} #{r.get('id')} at {r.get('pos')}"
    if k in ("unit-", "kill"):
        return f"{r.get('def')} #{r.get('id')} at {r.get('pos')}" + (f" by {r['by']}" if r.get("by") else "")
    if k == "op":
        if "tool" in r:
            return f"{r['tool']} {json.dumps(r.get('args'))} -> " + ("ok" if r.get("ok") else f"error: {r.get('error')}")
        return json.dumps({x: y for x, y in r.items() if x not in ("seq", "f", "k")})
    if k in ("note", "say"):
        return r.get("text", "")
    return json.dumps({x: y for x, y in r.items() if x not in ("seq", "f", "k")})[:300]


def clock(f):
    s = max(0, int(f) // 30)
    return f"{s // 60}:{s % 60:02d}"


def cmd_events(args):
    url = f"{pick_url(args)}/events"
    q = []
    if args.kinds:
        q.append(f"kinds={args.kinds}")
    if args.since is not None:
        q.append(f"since={args.since}")
    if q:
        url += "?" + "&".join(q)
    try:
        with urllib.request.urlopen(url) as r:
            for raw in r:
                line = raw.decode().rstrip("\n")
                if line.startswith("data: "):
                    rec = json.loads(line[6:])
                    print(f"{clock(rec['f']):>6} {rec['k']:<7} {line_text(rec)}", flush=True)
    except KeyboardInterrupt:
        pass


def cmd_journal(args):
    with open(args.file) as f:
        records = [json.loads(line) for line in f if line.strip()]
    kinds = set(args.kinds.split(",")) if args.kinds else None
    if kinds or args.grep:
        for r in records:
            if kinds and r.get("k") not in kinds:
                continue
            text = line_text(r)
            if args.grep and args.grep not in json.dumps(r):
                continue
            print(f"{clock(r.get('f', 0)):>6} {r.get('k'):<7} {text}")
        return
    run = next((r for r in records if r.get("k") == "run"), {})
    end = next((r for r in records if r.get("k") == "end"), None)
    ticks = [r for r in records if r.get("k") == "tick"]
    counts = {}
    for r in records:
        counts[r.get("k")] = counts.get(r.get("k"), 0) + 1
    print(f"run     {run.get('run')}  ({run.get('ai')} {run.get('version')}, build {run.get('build')})")
    print(f"map     {run.get('map')}  game {run.get('game')}  engine {run.get('engine')}")
    print(f"team    {run.get('team')} (ally team {run.get('ally_team')}) vs teams {run.get('enemy_teams')}  seed {run.get('seed')}")
    if run.get("options") or run.get("env"):
        print(f"options {json.dumps(run.get('options'))}  env {json.dumps(run.get('env'))}")
    last = records[-1].get("f", 0) if records else 0
    print(f"length  {clock(last)}" + (f"  ended: {end.get('state')} (reason {end.get('reason')})" if end else "  (no end record: still running, or crashed)"))
    if ticks:
        t = ticks[-1]
        peak = max(ticks, key=lambda x: x.get("army", 0))
        print(f"last    metal income {t['m'][1]}, army {t['army']} vs enemy seen {t['enemy_army']}, spots {t['spots'][0]}:{t['spots'][1]}")
        print(f"peak    army {peak['army']} at {clock(peak['f'])}")
    lost = sum(1 for r in records if r.get("k") == "unit-")
    kills = sum(1 for r in records if r.get("k") == "kill")
    print(f"units   built {counts.get('unit+', 0)}, lost {lost}, enemies killed {kills}, enemies seen {counts.get('enemy+', 0)}")
    errors = [r for r in records if r.get("k") == "log" and (r.get("mod") == "error" or "panicked" in r.get("msg", ""))]
    print(f"errors  {len(errors)}")
    for r in errors[:5]:
        print(f"        {clock(r['f'])} {r['msg'][:160]}")
    ops = [r for r in records if r.get("k") in ("op", "note")]
    print(f"operator {len(ops)} actions and notes")
    for r in ops[:20]:
        print(f"        {clock(r['f'])} {r['k']:<4} {line_text(r)[:160]}")
    print("records " + ", ".join(f"{k} {n}" for k, n in sorted(counts.items(), key=lambda x: -x[1])))


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = ap.add_subparsers(dest="cmd", required=True)
    ap_ls = sub.add_parser("ls")
    ap_ls.set_defaults(fn=cmd_ls)
    for name, fn in (("call", cmd_call), ("tools", cmd_tools), ("events", cmd_events)):
        p = sub.add_parser(name)
        p.add_argument("-t", "--team", type=int)
        p.add_argument("-u", "--url")
        p.set_defaults(fn=fn)
        if name == "call":
            p.add_argument("tool")
            p.add_argument("args", nargs="*")
        if name == "events":
            p.add_argument("-k", "--kinds")
            p.add_argument("--since", type=int)
    p = sub.add_parser("journal")
    p.add_argument("file")
    p.add_argument("-k", "--kinds")
    p.add_argument("-g", "--grep")
    p.set_defaults(fn=cmd_journal)
    args = ap.parse_args()
    args.fn(args)


if __name__ == "__main__":
    main()
