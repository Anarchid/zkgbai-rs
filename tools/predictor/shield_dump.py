#!/usr/bin/env python3
"""Shield parameters and distance-falloff weapons for mech.py's shield and heatray switches.

usage: shield_dump.py <out.json> [zk.sdd]
Per unit def with a Shield weapon: power (charge), regen (per second), radius, and the shield cost
of each shot of its own weapon (customParams shield_drain: the Felon's script blocks a shot its
shield cannot pay, GG.DrainShieldAndCheckProjectilePenetrate). Overlapping bubbles share charge
(unit_shield_link.lua: linked when distance < sum of radii).
"""
import glob
import json
import os
import re
import sys


def num(src, key):
    m = re.search(rf"{key}\s*=\s*([\d.]+)", src, re.I)
    return float(m.group(1)) if m else None


def main(out, zk=os.path.expanduser("~/.spring/games/zk.sdd")):
    defs = {}
    for f in glob.glob(os.path.join(zk, "units", "*.lua")):
        src = open(f, errors="ignore").read()
        if not re.search(r"weaponType\s*=\s*\[\[Shield\]\]", src):
            continue
        power, regen, radius = num(src, "shieldPower"), num(src, "shieldPowerRegen"), num(src, "shieldRadius")
        if not power or not radius:
            continue
        defs[os.path.basename(f)[:-4]] = {"power": power, "regen": regen or 0.0, "radius": radius,
                                         "drain": num(src, "shield_drain") or 0.0}
    # weapons whose damage falls off with distance (dynDamage*: heatrays), <unit>_<weapon key>
    dyn = []
    skip = {"customparams", "damage", "damages", "weapons", "weapondefs", "featuredefs", "sfxtypes"}
    for f in glob.glob(os.path.join(zk, "units", "*.lua")):
        unit, block, inw = os.path.basename(f)[:-4], None, False
        for ln in open(f, errors="ignore"):
            if re.match(r"^\s*weaponDefs\s*=\s*\{", ln, re.I):
                inw = True
                continue
            m = re.match(r"^\s*([A-Za-z0-9_]+)\s*=\s*\{", ln)
            if inw and m and m.group(1).lower() not in skip:
                block = f"{unit}_{m.group(1).lower()}"
            if inw and block and re.search(r"dynDamage(Exp|Range)\s*=", ln, re.I) and block not in dyn:
                dyn.append(block)
    defs["_dyn_weapons"] = sorted(dyn)
    json.dump(defs, open(out, "w"), indent=1, sort_keys=True)
    print("falloff weapons:", ", ".join(sorted(dyn)))
    print(len(defs) - 1, "shield defs:", ", ".join(f"{k} {v['power']:.0f}/{v['regen']:.0f}/r{v['radius']:.0f}"
                                                 + (f" drain {v['drain']:.0f}" if v["drain"] else "") for k, v in sorted(defs.items()) if not k.startswith("_")))


if __name__ == "__main__":
    main(*sys.argv[1:3])
