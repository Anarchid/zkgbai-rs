#!/usr/bin/env python3
"""Isolate the slow effect from blast HP damage and nofriendlyfire.

python3 tools/predictor/ablate_limpet.py INPUT.json ABLATION.json
Illustrations selected after the sweep, not a held-out evaluation set. Python's
ordinary expected-damage trajectories are used here, not the Rust bomb-charge
one-hit-survival mixture; chosen cases have p_through=1 in that mixture.
"""
import json
import math
import sys
from pathlib import Path

import export_mech as E
import mech


def run(input_path):
    E.init(str(Path(E.ROOT)/"refit2"))
    data = json.loads(Path(input_path).read_text())
    names = ["4amphraid-2vehriot-spread150-lead80", "4amphraid-1hoverriot-spread35-lead80",
             "4amphraid-8cloakraid-spread150-lead-80", "4amphraid-3energysolar-spread35-lead80"]
    output = []
    for name in names:
        c = next(c for c in data["cases"] if c["id"] == name)
        fight = {"A": [], "B": [], "game": "ablation", "fid": 0}
        for u in c["units"]:
            spec = data["specs"][u["def"]]
            v = dict(u, hpmax=spec["health"], cost=spec["cost"], armed=bool(spec["weapons"]),
                     static=not spec["mobile"], com=False)
            fight["A" if u["side"] == 0 else "B"].append(v)
        result = dict(id=name, horizon=600, twod=True)
        for mode in ("reserve", "slow", "damage_only"):
            f = dict(fight)
            if mode == "reserve":
                f["A"] = f["A"][:c["bomb"]]+f["A"][c["bomb"]+1:]
            cent = lambda s: [sum(u[k] for u in s)/len(s) for k in ("x", "z")]
            ca, cb = cent(f["A"]), cent(f["B"])
            f["d0"] = math.hypot(cb[0]-ca[0], cb[1]-ca[1])
            fates = {}
            mech.simulate_mech(f, dict(E.opts_of("2d"), bombs=1,
                                      limpet_slow=0 if mode == "damage_only" else 10),
                               fates=fates, horizon=600)
            us = f["A"]+f["B"]
            loss = [sum(u["cost"]*p for u, p in zip(us, fates["pdead"]) if u["side"] == s) for s in (0, 1)]
            result[mode] = dict(ally_loss=loss[0], enemy_loss=loss[1], trade=loss[1]-loss[0])
        result["marginal_with_slow"] = result["slow"]["trade"]-result["reserve"]["trade"]
        result["marginal_damage_only"] = result["damage_only"]["trade"]-result["reserve"]["trade"]
        output.append(result)
    return output


if __name__ == "__main__":
    output = run(sys.argv[1])
    Path(sys.argv[2]).write_text(json.dumps(output, indent=2)+"\n")
    print(json.dumps(output, indent=2))
