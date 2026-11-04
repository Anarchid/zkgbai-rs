#!/usr/bin/env python3
"""Prepare replay-derived and synthetic inputs for examples/limpet_probe.rs.

python3 tools/predictor/limpet_probe.py INPUT.json [--root PATH]
Uses the existing fitted tables; neither refits nor starts an engine. Recorded
fights are observational snapshots, not ground truth for the withheld action.
"""
import argparse
import collections
import json
import math
import pickle
from pathlib import Path

import export_mech as E
import predict


def prepare(root):
    for name, value in list(vars(E).items()):
        if isinstance(value, str) and value.startswith(E.ROOT + "/"):
            setattr(E, name, str(root) + value[len(E.ROOT):])
    E.init(str(root / "refit2"))
    ud, wtab = predict.G["ud"], predict.G["wtab"]
    specs = {}
    for name, d in ud.defs.items():
        weapons = []
        for w in d.weapons:
            if w.name not in wtab.index:
                continue
            r = wtab.loc[w.name]
            if float(r["range"]) <= 0 or r["type"] == "Shield":
                continue
            weapons.append(dict(name=w.name, weapon_type=r["type"], range=float(r["range"]),
                                reload=float(r["reload"]), salvo=int(r["salvo"]), damage=float(r["damage"])))
        specs[name] = dict(cost=d.cost, health=d.health, speed=d.speed, mobile=d.canmove,
                           air=d.canfly, builder=d.builder, radius=d.radius, weapons=weapons)
    fights = pickle.load(open(root / "refit2/fights.pkl", "rb"))
    native = [f for f in fights if any(u["def"] == "amphbomb" for s in ("A", "B") for u in f[s])]
    counts = collections.Counter(total_fights=len(fights), limpet_fights=len(native),
                                 limpet_games=len({f["game"] for f in native}))
    cases = []
    for f in native:
        initial = {s: [u for u in f[s] if u["first"] <= 90] for s in ("A", "B")}
        for a, b in (("A", "B"), ("B", "A")):
            bombs = [i for i, u in enumerate(initial[a]) if u["def"] == "amphbomb"]
            if not bombs:
                if any(u["def"] == "amphbomb" for u in f[a]):
                    counts["excluded_limpet_arrives_after_90_frames"] += 1
                continue
            if not initial[b]:
                counts["excluded_no_initial_enemy"] += 1
                continue
            # A single marginal decision; multiple simultaneous Limpets need a
            # joint policy, and cannot be interpreted as independent gains.
            if len(bombs) != 1:
                counts["excluded_multiple_limpets"] += 1
                continue
            units = [dict({k: u[k] for k in ("def", "hp", "x", "z", "first", "kill_f", "died", "cost")}, side=s)
                     for s, team in enumerate((a, b)) for u in initial[team]]
            if len(units) > 48:
                counts["excluded_over_48_units"] += 1
                continue
            if any(u["def"] not in specs or not all(math.isfinite(u[k]) for k in ("hp", "x", "z"))
                   for u in units):
                counts["excluded_missing_specs_or_position"] += 1
                continue
            # Shields/air are outside this ground-squad experiment. Water and
            # terrain cannot be screened from the segmented x/z-only snapshots.
            if any(ud.get(u["def"]).shield > 0 or ud.get(u["def"]).canfly for u in units):
                counts["excluded_shield_or_air"] += 1
                continue
            if not any(u["armed"] and u["def"] != "amphbomb" for u in initial[a]):
                counts["excluded_no_armed_escort"] += 1
                continue
            cases.append(dict(id=f"{f['game']}:{f['fid']}:{a}", kind="recorded", units=units,
                              bomb=bombs[0], game=f["game"], duration=f["end"]-f["start"],
                              reinforcements=f["reinfA"]+f["reinfB"]+sum(
                                  u["cost"] for s in ("A", "B") for u in f[s] if u["first"] > 90 and u["armed"]),
                              max_first=max(u.get("first", 0) for u in units)))
    counts["selected_snapshots"] = len(cases)
    counts["selected_games"] = len({c["game"] for c in cases})

    def unit(name, side, x, z):
        return dict(def_=name, side=side, hp=ud.get(name).health, x=x, z=z)

    enemies = [("staticmex", 3), ("energysolar", 3), ("amphcon", 3), ("cloakraid", 8),
               ("hoverraid", 6), ("vehriot", 2), ("hoverriot", 1), ("tankassault", 2),
               ("amphassault", 1), ("turretlaser", 3)]
    for ally in ("amphraid", "cloakraid", "cloakassault"):
        for count in (2, 4, 8):
            for enemy, number in enemies:
                for spacing in (35, 150):
                    for lead in (-80, 80):
                        us = [unit(ally, 0, -20*(i % 2), (i-(count-1)/2)*35) for i in range(count)]
                        us += [unit("amphbomb", 0, lead, 0)]
                        us += [unit(enemy, 1, 320.3, (i-(number-1)/2)*spacing) for i in range(number)]
                        for u in us:
                            u["def"] = u.pop("def_")
                        cases.append(dict(id=f"{count}{ally}-{number}{enemy}-spread{spacing}-lead{lead}",
                                          kind="synthetic", units=us, bomb=count))
    counts["synthetic_snapshots"] = sum(c["kind"] == "synthetic" for c in cases)
    return dict(counts=dict(counts), specs=specs, cases=cases)


if __name__ == "__main__":
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("output", type=Path)
    p.add_argument("--root", type=Path, default=Path.home()/"Programs/zkgbai-runs/predictor")
    args = p.parse_args()
    data = prepare(args.root)
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(data, allow_nan=False))
    print(json.dumps(data["counts"], indent=2))
