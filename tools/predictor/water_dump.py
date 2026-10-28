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
    """waterweapon / fireSubmersed per weapon: weapon blocks are the tables one level inside
    weaponDefs (brace depth, so rgbColor = {...} and other inline tables are not blocks)."""
    out = {}
    for f in glob.glob(os.path.join(zk, "units", "*.lua")):
        unit = os.path.basename(f)[:-4]
        depth = None  # brace depth inside weaponDefs; None outside it
        block = None
        for ln in open(f, errors="ignore"):
            code = ln.split("--", 1)[0]
            if depth is None:
                if re.match(r"^\s*weaponDefs\s*=\s*\{", code, re.I):
                    depth = 1
                continue
            m = re.match(r"^\s*([A-Za-z0-9_]+)\s*=\s*\{", code)
            if m and depth == 1:
                block = f"{unit}_{m.group(1).lower()}"
                out.setdefault(block, {"water": False, "submersed": False})
            if block and depth >= 2:
                if re.search(r"waterweapon\s*=\s*true", code, re.I):
                    out[block]["water"] = True
                if re.search(r"firesubmersed\s*=\s*true", code, re.I):
                    out[block]["submersed"] = True
            depth += code.count("{") - code.count("}")
            if depth <= 0:
                depth = None
                block = None
    for f in glob.glob(os.path.join(zk, "gamedata", "modularcomms", "weapons", "*.lua")):
        src = open(f, errors="ignore").read()
        name = "commweapon_" + os.path.basename(f)[:-4]
        out[name] = {"water": bool(re.search(r"waterweapon\s*=\s*true", src, re.I)),
                     "submersed": bool(re.search(r"firesubmersed\s*=\s*true", src, re.I))}
    return out


def categories(zk):
    """Unit def -> its target categories; weapon (<unit>_<def key>) -> its slot's onlyTargetCategory
    (None = anything). A weapon can target a unit only when they share a category (engine rule)."""
    cats, only = {}, {}
    for f in glob.glob(os.path.join(zk, "units", "*.lua")):
        unit = os.path.basename(f)[:-4]
        src = open(f, errors="ignore").read()
        m = re.search(r"\bcategory\s*=\s*\[\[([^\]]*)\]\]", src)
        if m:
            cats[unit] = set(m.group(1).split())
        w = re.search(r"\bweapons\s*=\s*\{(.*?)\n\s*\},?\s*\n\s*weaponDefs", src, re.S | re.I)
        if not w:
            continue
        entries = re.split(r"(?=\bdef\s*=)", w.group(1))
        for e in entries:
            d = re.match(r"def\s*=\s*(?:\[\[|\")([A-Za-z0-9_]+)", e)
            if not d:
                continue
            o = re.search(r"onlyTargetCategory\s*=\s*\[\[([^\]]*)\]\]", e)
            only[f"{unit}_{d.group(1).lower()}"] = set(o.group(1).split()) if o else None
    return cats, only


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
    cats, only = categories(zk)
    pickle.dump({"heights": heights, "weapons": weapons, "floaters": floaters, "submerged_at": sub_at, "moveclass": moveclass,
                 "category": cats, "only_target": only}, open(out, "wb"))
    print(f"categories for {len(cats)} defs, target lists for {len(only)} weapon slots;",
          "subraider:", sorted(cats.get("subraider", [])), "| hoverriot_laser1:", sorted(only.get("hoverriot_laser1") or ["any"]))
    print(f"heightmaps for {len(heights)} maps; {sum(v['water'] for v in weapons.values())} water weapons, "
          f"{sum(v['submersed'] for v in weapons.values())} fire submersed; floaters {floaters}; "
          f"amph_submerged_at for {len(sub_at)} defs")
    print("water weapons:", sorted(k for k, v in weapons.items() if v["water"] and not k.startswith("chicken")))


if __name__ == "__main__":
    main(*sys.argv[1:4])
