#!/usr/bin/env python3
"""Water-domain inputs for mech.py's water switch.

usage: water_dump.py <unitdefs.tsv> <out.pkl> [zk.sdd]
  heights: per map (name lowercased, spaces as underscores) the 64-elmo heightmap from the first
    ZKGBAI journal "map" record found under ~/Programs/zkgbai-runs (rows are bytes between min and
    max, operator/views.rs); maps never journalled are missing (water rules then stay off).
  weapons: per weapon (<unit>_<key>, commweapon_<name>) whether it is a waterweapon or may fire
    submersed (unit and modular-comm weapon files).
  floaters: LuaRules/Configs/float_defs.lua (rise to fire when aiming and not moving).
  submerged_at: amph_submerged_at per unit def (ZK's own "under water" depth for amphibious units).
  moveclass: per unit def, from the unit-def dump.
"""
import csv
import glob
import json
import os
import pickle
import re
import sys

import numpy as np

RUNS = os.path.expanduser("~/Programs/zkgbai-runs")


def norm(name):
    return name.strip().lower().replace(" ", "_")


def weapon_flags(zk):
    out = {}
    skip = {"customparams", "damage", "damages", "weapons", "weapondefs", "featuredefs", "sfxtypes", "explosiongenerators",
            "buildoptions", "sounds", "textures", "collisionvolumes", "weapon", "select", "ok"}
    for f in glob.glob(os.path.join(zk, "units", "*.lua")):
        unit = os.path.basename(f)[:-4]
        block = None
        inw = False
        for ln in open(f, errors="ignore"):
            if re.match(r"^\s*weaponDefs\s*=\s*\{", ln, re.I):
                inw = True
                continue
            if not inw:
                continue
            m = re.match(r"^\s*([A-Za-z0-9_]+)\s*=\s*\{", ln)
            if m and m.group(1).lower() not in skip:
                block = f"{unit}_{m.group(1).lower()}"
                out.setdefault(block, {"water": False, "submersed": False})
            if block and re.search(r"waterweapon\s*=\s*true", ln, re.I):
                out[block]["water"] = True
            if block and re.search(r"firesubmersed\s*=\s*true", ln, re.I):
                out[block]["submersed"] = True
    for f in glob.glob(os.path.join(zk, "gamedata", "modularcomms", "weapons", "*.lua")):
        src = open(f, errors="ignore").read()
        name = "commweapon_" + os.path.basename(f)[:-4]
        out[name] = {"water": bool(re.search(r"waterweapon\s*=\s*true", src, re.I)),
                     "submersed": bool(re.search(r"firesubmersed\s*=\s*true", src, re.I))}
    return out


def main(udp, out, zk=os.path.expanduser("~/.spring/games/zk.sdd")):
    heights = {}
    for j in glob.iglob(os.path.join(RUNS, "**", "journal", "*.jsonl"), recursive=True):
        try:
            with open(j) as fh:
                for i, line in enumerate(fh):
                    if i > 20:
                        break
                    if '"k":"map"' in line and '"heights"' in line:
                        r = json.loads(line)
                        k = norm(r["name"])
                        if k not in heights:
                            h = r["heights"]
                            grid = h["min"] + np.array(h["rows"], dtype=np.float32) / 255.0 * max(h["max"] - h["min"], 1.0)
                            heights[k] = (float(h["cell"]), grid)
                        break
        except (OSError, ValueError, KeyError):
            continue
    weapons = weapon_flags(zk)
    fsrc = open(os.path.join(zk, "LuaRules", "Configs", "float_defs.lua"), errors="ignore").read()
    floaters = sorted(set(re.findall(r'\[\s*"([a-z0-9_]+)"\s*\]|^\s*([a-z0-9_]+)\s*=\s*\{', fsrc, re.M) and
                          [a or b for a, b in re.findall(r'\[\s*"([a-z0-9_]+)"\s*\]|^\s*([a-z0-9_]+)\s*=\s*\{', fsrc, re.M)]))
    sub_at = {}
    for f in glob.glob(os.path.join(zk, "units", "*.lua")):
        m = re.search(r"amph_submerged_at\s*=\s*([\d.]+)", open(f, errors="ignore").read())
        if m:
            sub_at[os.path.basename(f)[:-4]] = float(m.group(1))
    moveclass = {}
    with open(udp) as fh:
        for row in csv.DictReader(fh, delimiter="\t"):
            moveclass[row["name"]] = row.get("moveclass", "") or ""
    pickle.dump({"heights": heights, "weapons": weapons, "floaters": floaters, "submerged_at": sub_at, "moveclass": moveclass},
                open(out, "wb"))
    print(f"heightmaps for {len(heights)} maps; {sum(v['water'] for v in weapons.values())} water weapons, "
          f"{sum(v['submersed'] for v in weapons.values())} fire submersed; floaters {floaters}; "
          f"amph_submerged_at for {len(sub_at)} defs")
    print("water weapons:", sorted(k for k, v in weapons.items() if v["water"])[:40])


if __name__ == "__main__":
    main(*sys.argv[1:4])
