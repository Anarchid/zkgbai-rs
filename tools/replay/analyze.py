#!/usr/bin/env python3
"""Summarise a replay capture (replay_capture_widget.lua) as a markdown report,
or print the state of an episode.

usage: analyze.py <replaycap.jsonl> [--maps DIR] > report.md
       analyze.py <replaycap.jsonl> --episode FROM:TO [--units 1,2 | --at X,Z,R]   (or 9:00..9:30)
       analyze.py <replaycap.jsonl> --track UID

Positions are given as depth/lateral against the axis between the two
starts: depth 0 = team A's start, 1 = team B's start; lateral is elmos to
the right (+) or left (-) of the axis as seen from A. With --maps, also
renders the map at each minute (PNG) into DIR.
"""
import argparse
import json
import math
import re
from collections import Counter, defaultdict

FPS = 30
# Captures made before the widget skipped CMD's option and state constants
# named some commands after them.
CMD_ALIASES = {"OPT_RIGHT": "FIGHT", "OPT_INTERNAL": "SQUADWAIT",
               "WAITCODE_TIME": "INSERT", "FIRESTATE_RETURNFIRE": "INSERT", "MOVESTATE_MANEUVER": "INSERT",
               "WAITCODE_DEATH": "REMOVE", "FIRESTATE_FIREATWILL": "REMOVE", "MOVESTATE_ROAM": "REMOVE",
               "FIRESTATE_HOLDFIRE": "STOP", "MOVESTATE_HOLDPOS": "STOP"}


def mmss(f):
    s = f // FPS
    return f"{s // 60}:{s % 60:02d}"


class Game:
    def __init__(self, path):
        self.recs = []
        for line in open(path):
            try:
                r = json.loads(line)
            except ValueError:
                break  # a capture cut off mid-line (engine exit)
            if r["k"] == "cmd":
                r["c"] = CMD_ALIASES.get(r["c"], r["c"])
            elif r["k"] == "snap":
                for u in r["units"]:
                    u[8] = CMD_ALIASES.get(u[8], u[8])
            self.recs.append(r)
        self.meta = next(r for r in self.recs if r["k"] == "meta")
        self.defs = self.meta.get("defs", {})
        self.teams = {t["team"]: t for t in self.meta["teams"]}
        self.snaps = [r for r in self.recs if r["k"] == "snap"]
        self.over = next((r for r in self.recs if r["k"] == "over"), None)
        ids = sorted(self.teams)
        self.a, self.b = ids[0], ids[1]
        self.sa = self.teams[self.a]["start"]
        self.sb = self.teams[self.b]["start"]
        dx, dz = self.sb[0] - self.sa[0], self.sb[1] - self.sa[1]
        self.axis_len = math.hypot(dx, dz) or 1
        self.ux, self.uz = dx / self.axis_len, dz / self.axis_len

    # --- unit classes -------------------------------------------------
    def cost(self, name):
        if name == "terraunit":
            return 0  # terraform placeholder with a huge nominal cost
        d = self.defs.get(name)
        return d[1] if d else 0

    def human(self, name):
        d = self.defs.get(name)
        return d[0] if d else name

    @staticmethod
    def is_comm(name):
        return bool(re.match(r"^(dyn|c\d+_)", name))

    def kind(self, name):
        d = self.defs.get(name)
        if not d:
            return "?"
        _, metal, bp, speed, weapons, factory, *_ = d
        if self.is_comm(name):
            return "comm"
        if factory:
            return "factory"
        if name == "staticmex":
            return "mex"
        if name.startswith("energy") or name in ("staticstorage",):
            return "energy"
        if speed > 0 and bp > 0:
            return "con"
        if speed == 0 and bp > 0:
            return "caretaker"
        if speed > 0 and weapons > 0:
            return "army"
        if speed == 0 and weapons > 0:
            return "porc"
        if speed > 0:
            return "support"
        return "static"

    # --- geometry -----------------------------------------------------
    def place(self, x, z):
        rx, rz = x - self.sa[0], z - self.sa[1]
        depth = (rx * self.ux + rz * self.uz) / self.axis_len
        lat = -rx * self.uz + rz * self.ux
        return depth, lat

    def where(self, x, z):
        d, l = self.place(x, z)
        return f"d={d:.2f} lat={l:+.0f}"

    def name(self, team):
        t = self.teams[team]
        return t["name"]


def snap_units(snap):
    # [id, def, team, x, z, hp%, build%, seen, cmd, cx, cz]
    return snap["units"]


def section_setup(g, out):
    out.append(f"# {g.meta['map']}: {g.name(g.a)} vs {g.name(g.b)}\n")
    out.append(f"Game {g.meta['game']}, map {g.meta['mapX']}x{g.meta['mapZ']}, {len(g.meta['mex'])} metal spots.")
    for t in (g.a, g.b):
        tm = g.teams[t]
        out.append(f"- team {t} (ally {tm['ally']}): **{tm['name']}**{' (AI)' if tm['ai'] else ''}, start {tm['start']}")
    if g.over:
        winners = g.over["winners"]
        wt = [t for t in g.teams if g.teams[t]["ally"] in winners]
        out.append(f"- game over at {mmss(g.over['f'])} (f={g.over['f']}), winner: "
                   + ", ".join(g.name(t) for t in wt))
    out.append(f"\nPositions: d = depth along the axis {g.name(g.a)} start (0) -> {g.name(g.b)} start (1); "
               f"lat = elmos right (+) / left (-) of it, seen from {g.name(g.a)}. Axis length {g.axis_len:.0f}.\n")


def spot_owners(g, snap):
    """owner team per metal spot index (finished or building mex within 60)."""
    mexes = [(u[3], u[4], u[2], u[6]) for u in snap_units(snap) if u[1] == "staticmex"]
    owners = {}
    for i, (sx, sz, _) in enumerate(g.meta["mex"]):
        for x, z, team, bp in mexes:
            if abs(x - sx) < 60 and abs(z - sz) < 60:
                owners[i] = (team, bp)
    return owners


def section_timeline(g, out):
    out.append("## Timeline (per minute)\n")
    out.append("Per team: metal income, energy income, mexes (finished), constructors (+commander), "
               "build power, army metal (mobile armed), porc metal, eco metal, cumulative metal lost, "
               "metal killed, and how much of its army the enemy sees (LOS/radar).\n")
    lost = defaultdict(float)
    killed = defaultdict(float)
    deaths = sorted((r for r in g.recs if r["k"] == "dead"), key=lambda r: r["f"])
    di = 0
    header = "| t | team | minc | einc | mex | cons | bp | army | porc | eco | lost | killed | seen |"
    out.append(header)
    out.append("|" + "---|" * 13)
    for s in g.snaps:
        if s["f"] % 1800 != 0 and s is not g.snaps[-1]:
            continue
        while di < len(deaths) and deaths[di]["f"] <= s["f"]:
            r = deaths[di]
            v = g.cost(r["d"]) * r.get("bp", 100) / 100
            lost[r["t"]] += v
            if r.get("at") is not None and r["at"] != r["t"] and r["at"] >= 0:
                killed[r["at"]] += v
            di += 1
        for t in (g.a, g.b):
            res = s["res"][str(t)]
            minc, einc = res["m"][3], res["e"][3]
            mex = cons = 0
            bp = army = porc = eco = 0.0
            army_n = army_seen = 0
            for u in snap_units(s):
                if u[2] != t:
                    continue
                name, done = u[1], u[6] >= 100
                k = g.kind(name)
                value = g.cost(name) * u[6] / 100
                if k == "mex" and done:
                    mex += 1
                if k in ("con", "comm") and done:
                    cons += k == "con"
                    bp += g.defs[name][2]
                if k == "caretaker" and done:
                    bp += g.defs[name][2]
                if k == "army":
                    army += value
                    if done:
                        army_n += 1
                        army_seen += (u[7] & 3) != 0
                elif k == "porc":
                    porc += value
                elif k in ("mex", "energy"):
                    eco += value
            seen = f"{army_seen}/{army_n}" if army_n else "-"
            out.append(f"| {mmss(s['f'])} | {g.name(t)[:14]} | {minc:.1f} | {einc:.0f} | {mex} | {cons} | {bp:.0f} | "
                       f"{army:.0f} | {porc:.0f} | {eco:.0f} | {lost[t]:.0f} | {killed[t]:.0f} | {seen} |")
    out.append("")


def section_build_orders(g, out, until=6 * 60 * FPS):
    out.append(f"## Build orders (finished, first {until // 1800} min)\n")
    out.append("Consecutive identical units merged; structures with their place.\n")
    for t in (g.a, g.b):
        out.append(f"**{g.name(t)}**:")
        items = []
        last = None
        for r in g.recs:
            if r["k"] != "done" or r["t"] != t or r["f"] > until:
                continue
            k = g.kind(r["d"])
            mobile = k in ("army", "con", "support")
            if mobile and last and last[1] == r["d"]:
                last[2] += 1
                continue
            last = [r["f"], r["d"], 1, None if mobile else g.where(r["x"], r["z"])]
            items.append(last)
        lines = []
        for f, d, n, w in items:
            lines.append(f"{mmss(f)} {d}{' x' + str(n) if n > 1 else ''}{' @' + w if w else ''}")
        out.append("; ".join(lines) + "\n")


def section_mexes(g, out):
    out.append("## Metal spots\n")
    spots = g.meta["mex"]
    depths = [g.place(x, z)[0] for x, z, _ in spots]
    out.append(f"{len(spots)} spots; by depth: {sum(d < 0.5 for d in depths)} on {g.name(g.a)}'s half, "
               f"{sum(d >= 0.5 for d in depths)} on {g.name(g.b)}'s half.\n")
    out.append("| t | " + " | ".join(f"{g.name(t)[:14]} spots (own half / far half)" for t in (g.a, g.b)) + " |")
    out.append("|---|---|---|")
    for s in g.snaps:
        if s["f"] % 1800 != 0:
            continue
        owners = spot_owners(g, s)
        cells = []
        for t in (g.a, g.b):
            own = far = 0
            for i, (team, bp) in owners.items():
                if team == t:
                    home = depths[i] < 0.5 if t == g.a else depths[i] >= 0.5
                    own += home
                    far += not home
            cells.append(f"{own + far} ({own} / {far})")
        out.append(f"| {mmss(s['f'])} | " + " | ".join(cells) + " |")
    out.append("")
    # Mex deaths
    out.append("Mexes destroyed (time, owner, place, killer):\n")
    rows = [r for r in g.recs if r["k"] == "dead" and r["d"] == "staticmex"]
    for r in rows:
        out.append(f"- {mmss(r['f'])} {g.name(r['t'])[:14]} @{g.where(r['x'], r['z'])} by {r.get('ad') or '?'}"
                   + ("" if r.get("bp", 100) >= 100 else f" (unfinished {r['bp']}%)"))
    out.append("")


def section_fights(g, out, gap=20 * FPS, radius=900):
    out.append("## Fights (clusters of deaths)\n")
    out.append("Deaths grouped while each is within 20 s and 900 elmos of the group. "
               "Value lost per side, what died, what killed.\n")
    deaths = sorted((r for r in g.recs if r["k"] == "dead"), key=lambda r: r["f"])
    groups = []
    for r in deaths:
        for grp in reversed(groups):
            if r["f"] - grp["last"] > gap:
                continue
            cx = sum(x["x"] for x in grp["d"]) / len(grp["d"])
            cz = sum(x["z"] for x in grp["d"]) / len(grp["d"])
            if math.hypot(r["x"] - cx, r["z"] - cz) < radius:
                grp["d"].append(r)
                grp["last"] = r["f"]
                break
        else:
            groups.append({"d": [r], "first": r["f"], "last": r["f"]})
    for grp in groups:
        ds = grp["d"]
        cx = sum(x["x"] for x in ds) / len(ds)
        cz = sum(x["z"] for x in ds) / len(ds)
        lost = defaultdict(float)
        died = defaultdict(Counter)
        by = defaultdict(Counter)
        for r in ds:
            lost[r["t"]] += g.cost(r["d"]) * r.get("bp", 100) / 100
            died[r["t"]][r["d"]] += 1
            if r.get("ad"):
                by[r["t"]][r["ad"]] += 1
        total = sum(lost.values())
        if total < 60:
            continue
        parts = []
        for t in (g.a, g.b):
            if died[t]:
                parts.append(f"{g.name(t)[:10]} lost {lost[t]:.0f} ("
                             + ", ".join(f"{n}x{d}" for d, n in died[t].most_common()) + ")"
                             + (" to " + ", ".join(f"{d}" for d, _ in by[t].most_common(3)) if by[t] else ""))
        out.append(f"- {mmss(grp['first'])}-{mmss(grp['last'])} @{g.where(cx, cz)}: " + "; ".join(parts))
    out.append("")


def cluster(points, radius=500):
    groups = []
    for p in points:
        for grp in groups:
            if math.hypot(p[0] - grp["x"], p[1] - grp["z"]) < radius:
                grp["m"].append(p)
                n = len(grp["m"])
                grp["x"] += (p[0] - grp["x"]) / n
                grp["z"] += (p[1] - grp["z"]) / n
                break
        else:
            groups.append({"x": p[0], "z": p[1], "m": [p]})
    return groups


def section_armies(g, out, every=900):
    out.append("## Army positions (every 30 s)\n")
    out.append("Mobile armed units (and commanders) grouped within 500 elmos: place, composition, "
               "and how many of them the enemy sees. Orders: the most common current command.\n")
    for s in g.snaps:
        if s["f"] % every != 0 or s["f"] == 0:
            continue
        line = [f"**{mmss(s['f'])}**"]
        for t in (g.a, g.b):
            pts = [(u[3], u[4], u) for u in snap_units(s)
                   if u[2] == t and u[6] >= 100 and g.kind(u[1]) in ("army", "comm")]
            groups = sorted(cluster(pts), key=lambda grp: -len(grp["m"]))
            descr = []
            for grp in groups[:5]:
                comp = Counter(p[2][1] for p in grp["m"])
                seen = sum((p[2][7] & 3) != 0 for p in grp["m"])
                cmds = Counter(p[2][8] for p in grp["m"] if p[2][8])
                cmd = cmds.most_common(1)[0][0] if cmds else "idle"
                descr.append(f"[{g.where(grp['x'], grp['z'])}: " + ",".join(f"{n}{d}" for d, n in comp.most_common())
                             + f"; seen {seen}/{len(grp['m'])}; {cmd}]")
            line.append(f"{g.name(t)[:10]}: " + (" ".join(descr) if descr else "none"))
        out.append("- " + " | ".join(line))
    out.append("")


def section_cons(g, out):
    out.append("## Constructors and commanders: where they were (every minute)\n")
    for s in g.snaps:
        if s["f"] % 1800 != 0 or s["f"] == 0:
            continue
        parts = []
        for t in (g.a, g.b):
            us = [u for u in snap_units(s) if u[2] == t and u[6] >= 100 and g.kind(u[1]) in ("con", "comm")]
            parts.append(f"{g.name(t)[:10]}: " + ", ".join(
                f"{'COMM' if g.kind(u[1]) == 'comm' else u[1]}({g.place(u[3], u[4])[0]:.2f},{u[5]}%hp,{u[8] or 'idle'})"
                for u in us))
        out.append(f"- **{mmss(s['f'])}** " + " | ".join(parts))
    out.append("")


def section_losses(g, out):
    out.append("## Losses by type\n")
    for t in (g.a, g.b):
        built = Counter(r["d"] for r in g.recs if r["k"] == "done" and r["t"] == t)
        lost = Counter(r["d"] for r in g.recs if r["k"] == "dead" and r["t"] == t)
        killed = defaultdict(float)
        for r in g.recs:
            if r["k"] == "dead" and r.get("at") == t and r["t"] != t and r.get("ad"):
                killed[r["ad"]] += g.cost(r["d"]) * r.get("bp", 100) / 100
        names = sorted(set(built) | set(lost) | set(killed), key=lambda n: -g.cost(n) * built.get(n, 0))
        out.append(f"**{g.name(t)}** (type: built/lost, metal it killed):")
        out.append(", ".join(f"{n} {built.get(n, 0)}/{lost.get(n, 0)}"
                             + (f" k{killed[n]:.0f}" if killed.get(n) else "") for n in names) + "\n")


def section_damage(g, out, top=20):
    out.append("## Damage (attacker -> victim, top pairs)\n")
    pairs = Counter()
    for r in g.recs:
        if r["k"] != "dmg":
            continue
        for at, ad, vt, vd, cx, cz, dmg, para in r["rows"]:
            if at != vt:
                pairs[(at, ad, vd)] += dmg
    for (at, ad, vd), dmg in pairs.most_common(top):
        out.append(f"- {g.name(at)[:10] if at in g.teams else at}: {ad} -> {vd}: {dmg:.0f}")
    out.append("")


def section_orders(g, out):
    out.append("## Orders given by players (per minute)\n")
    out.append("Count of order events (a group order counts once) by command, non-AI teams; "
               "AI teams' build orders only.\n")
    per = defaultdict(Counter)
    for r in g.recs:
        if r["k"] == "cmd":
            per[(r["f"] // 1800, r["t"])][r["c"]] += 1
    for (m, t), c in sorted(per.items()):
        out.append(f"- {m}:00 {g.name(t)[:10]}: " + ", ".join(f"{k} {n}" for k, n in c.most_common(10)))
    out.append("")


def section_chat(g, out):
    lines = [r for r in g.recs if r["k"] == "chat" and "Widget removed" not in r["msg"]]
    if lines:
        out.append("## Chat\n")
        for r in lines:
            out.append(f"- {mmss(r['f'])} {r['msg']}")
        out.append("")


def render_maps(g, outdir):
    import os
    import matplotlib
    matplotlib.use("Agg")
    import matplotlib.pyplot as plt
    import numpy as np
    os.makedirs(outdir, exist_ok=True)
    h = np.array(g.meta["heights"]["rows"], dtype=float)
    cell = g.meta["heights"]["cell"]
    colors = {g.a: "tab:blue", g.b: "tab:red"}
    markers = {"army": "o", "comm": "*", "con": "^", "porc": "s", "mex": "D", "factory": "P", "energy": ".",
               "caretaker": "x", "support": "v", "static": "."}
    for s in g.snaps:
        if s["f"] % 1800 != 0 or s["f"] == 0:
            continue
        fig, ax = plt.subplots(figsize=(10, 10 * g.meta["mapZ"] / g.meta["mapX"]))
        ax.imshow(h, cmap="gray", extent=(0, g.meta["mapX"], g.meta["mapZ"], 0), alpha=0.8)
        for x, z, _ in g.meta["mex"]:
            ax.add_patch(plt.Circle((x, z), 60, fill=False, color="gold", lw=1))
        for u in snap_units(s):
            k = g.kind(u[1])
            if u[2] not in colors:
                continue
            size = {"comm": 200, "army": 25, "con": 40, "porc": 40, "factory": 120}.get(k, 15)
            ax.scatter(u[3], u[4], c=colors[u[2]], marker=markers.get(k, "."), s=size,
                       alpha=1.0 if u[6] >= 100 else 0.4, edgecolors="white" if (u[7] & 3) else "none")
            if k in ("army", "comm") and u[9] >= 0:
                ax.plot([u[3], u[9]], [u[4], u[10]], color=colors[u[2]], lw=0.4, alpha=0.5)
        for wname, x, z, metal in s.get("wrecks", []):
            ax.scatter(x, z, c="green", marker="x", s=metal / 10)
        ax.set_xlim(0, g.meta["mapX"])
        ax.set_ylim(g.meta["mapZ"], 0)
        ax.set_title(f"{g.meta['map']} {mmss(s['f'])}: blue {g.name(g.a)}, red {g.name(g.b)} "
                     f"(white edge = seen by enemy)")
        fig.savefig(f"{outdir}/map_{s['f'] // 1800:02d}min.png", dpi=70, bbox_inches="tight")
        plt.close(fig)


# --- episode queries -----------------------------------------------------
# Unit row fields: v1 [id, def, team, x, z, hp%, build%, seen, cmd, cx, cz];
# v2 adds [hdg, hp, maxhp, cloak, sub, los digits per ally, cmd id, cmd
# target unit, queue length].
UNIT_FIELDS = ["uid", "def", "team", "x", "z", "hp%", "build%", "seen", "cmd", "cx", "cz",
               "hdg", "hp", "maxhp", "cloak", "sub", "los", "cmdid", "ctgt", "queue"]


def parse_frame(s):
    """A frame number, or seconds with an 's' suffix, or m:ss."""
    s = str(s)
    if s.endswith("s"):
        return int(float(s[:-1]) * FPS)
    if ":" in s:
        m, sec = s.split(":")
        return (int(m) * 60 + int(sec)) * FPS
    return int(s)


def unit_dict(g, row, f):
    d = {"f": f, "t": mmss(f)}
    d.update(zip(UNIT_FIELDS, row))
    if "los" not in d:
        d["los"] = str(d.get("seen", ""))
    d["team"] = f"{d['team']}:{g.name(d['team'])[:8]}" if d["team"] in g.teams else d["team"]
    return d


def episode_rows(g, f0, f1, units=None, at=None):
    """Rows of the chosen units at every snapshot in f0..f1: the given unit
    ids, or every unit that comes within `at` = (x, z, r) during the window
    (all of them if neither)."""
    snaps = [s for s in g.snaps if f0 <= s["f"] <= f1]
    chosen = set(units or [])
    if at is not None:
        x, z, r = at
        for s in snaps:
            for u in s["units"]:
                if math.hypot(u[3] - x, u[4] - z) <= r:
                    chosen.add(u[0])
    rows = []
    for s in snaps:
        for u in s["units"]:
            if not chosen or u[0] in chosen:
                rows.append(unit_dict(g, u, s["f"]))
    return rows, chosen


def episode_events(g, f0, f1, chosen):
    out = []
    for r in g.recs:
        if r["k"] in ("new", "done", "dead", "give") and f0 <= r["f"] <= f1 and (not chosen or r.get("u") in chosen):
            out.append(r)
    return out


def episode_damage(g, f0, f1, at=None):
    """dmg windows closing in (f0, f1 + window]: attacker team/def, victim
    team/def, cell, damage, paralyze damage; near `at` only if given."""
    out = []
    for r in g.recs:
        if r["k"] != "dmg" or not (f0 < r["f"] <= f1 + DMG_WINDOW):
            continue
        for at_, ad, vt, vd, cx, cz, dmg, para in r["rows"]:
            if at is not None:
                x, z, rad = at
                if math.hypot((cx + 0.5) * r["cell"] - x, (cz + 0.5) * r["cell"] - z) > rad + r["cell"]:
                    continue
            out.append({"f": r["f"], "t": mmss(r["f"]), "att_team": at_, "att": ad, "vic_team": vt, "vic": vd,
                        "cell": f"{cx},{cz}", "dmg": dmg, "para": para})
    return out


DMG_WINDOW = 150


def table(rows, cols=None):
    if not rows:
        return "(no rows)"
    cols = cols or [c for c in rows[0] if c != "f"]
    width = {c: max(len(c), *(len(str(r.get(c, ""))) for r in rows)) for c in cols}
    lines = ["  ".join(c.ljust(width[c]) for c in cols)]
    for r in rows:
        lines.append("  ".join(str(r.get(c, "")).ljust(width[c]) for c in cols))
    return "\n".join(lines)


def print_episode(g, f0, f1, units=None, at=None):
    rows, chosen = episode_rows(g, f0, f1, units, at)
    allies = g.meta.get("allies")
    print(f"# episode {mmss(f0)}-{mmss(f1)} (frames {f0}-{f1}), {len(chosen) if chosen else 'all'} units"
          + (f", within {at[2]:.0f} of ({at[0]:.0f}, {at[1]:.0f})" if at else "")
          + (f"; los digits per ally team {allies}" if allies else "; seen = by the enemy (v1 capture)"))
    cols = ["t", "uid", "def", "team", "x", "z", "hp", "maxhp", "hp%", "build%", "cloak", "sub", "los", "cmd", "ctgt", "cx", "cz", "queue"]
    cols = [c for c in cols if any(c in r for r in rows)]
    last = None
    for r in rows:
        if r["f"] != last:
            print(f"\n## {r['t']} (f={r['f']})")
            last = r["f"]
            block = [x for x in rows if x["f"] == r["f"]]
            print(table(block, cols))
    ev = episode_events(g, f0, f1, chosen)
    print("\n## events")
    print(table([{"t": mmss(r["f"]), "kind": r["k"], "uid": r.get("u"), "def": r.get("d"), "team": r.get("t"),
                  "x": r.get("x"), "z": r.get("z"), "by": r.get("ad", ""), "by_team": r.get("at", "")} for r in ev]))
    print("\n## damage windows")
    print(table(episode_damage(g, f0, f1, at)))


def print_track(g, uids):
    rows = []
    for s in g.snaps:
        for u in s["units"]:
            if u[0] in uids:
                rows.append(unit_dict(g, u, s["f"]))
    rows.sort(key=lambda r: (r["uid"], r["f"]))
    cols = ["uid", "t", "def", "team", "x", "z", "hp", "hp%", "build%", "cloak", "sub", "los", "cmd", "ctgt", "cx", "cz"]
    print(table(rows, [c for c in cols if any(c in r for r in rows)]))
    print("\n## events")
    ev = episode_events(g, 0, 10 ** 9, set(uids))
    print(table([{"t": mmss(r["f"]), "kind": r["k"], "uid": r.get("u"), "def": r.get("d"), "team": r.get("t"),
                  "by": r.get("ad", ""), "by_team": r.get("at", "")} for r in ev]))


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("capture")
    ap.add_argument("--maps")
    ap.add_argument("--episode", metavar="FROM:TO", help="state table per snapshot for a frame range "
                    "(frames or Ns seconds; m:ss with '..' between: 9:00..9:30); with --units or --at")
    ap.add_argument("--units", help="comma-separated unit ids for --episode")
    ap.add_argument("--at", metavar="X,Z,R", help="units within R elmos of (X, Z) during the episode")
    ap.add_argument("--track", metavar="UID[,UID]", help="one unit's rows over the game, with its events")
    args = ap.parse_args()
    g = Game(args.capture)
    if args.episode:
        a, b = args.episode.split("..") if ".." in args.episode else args.episode.split(":")
        units = [int(x) for x in args.units.split(",")] if args.units else None
        at = tuple(float(x) for x in args.at.split(",")) if args.at else None
        print_episode(g, parse_frame(a or "0"), parse_frame(b) if b else 10 ** 9, units, at)
        return
    if args.track:
        print_track(g, {int(x) for x in args.track.split(",")})
        return
    out = []
    section_setup(g, out)
    section_chat(g, out)
    section_timeline(g, out)
    section_mexes(g, out)
    section_build_orders(g, out)
    section_losses(g, out)
    section_fights(g, out)
    section_armies(g, out)
    section_cons(g, out)
    section_damage(g, out)
    section_orders(g, out)
    print("\n".join(out))
    if args.maps:
        render_maps(g, args.maps)


if __name__ == "__main__":
    main()
