#!/usr/bin/env python3
"""How much damage do non-water weapons deal to targets under water by the engine rule? (train games)

usage: water_residue.py <cache-dir> <water.pkl>
mech.py's water switch puts a unit under water iff the journal heightmap's water depth at its
position, capped at the def's maxWaterDepth, exceeds its UnitDefs height (bottom walkers, sunk
statics; not floaters, ships, hovers or air), or it is a sub in water (it rides at its waterline,
deeper than it is tall), and then only water weapons may hit it. If that rule is right, non-water damage on
such targets should be ~0; what is left should sit where the heightmap is coarse: at depths near
the unit's height (byte-quantized heights) and on cells next to shallower ones (64-elmo cells).
The amph_submerged_at rule of the first water pass and the rule without the cap are shown for reference.
"""
import os
import pickle
import sys
from collections import Counter, defaultdict

import numpy as np

import combatdata
import split

AIR = {"FIXEDWING", "GUNSHIP"}


def main(cache, water):
    W = pickle.load(open(water, "rb"))
    mc, height, cats, maxwd = W["moveclass"], W["height"], W["category"], W["max_water_depth"]
    floaters = set(W["floaters"]) | set(W["float_on_water"])
    is_water = lambda w: bool(W["weapons"].get(w, {}).get("water"))
    surfacing = lambda t: (cats.get(t, set()) & AIR or mc.get(t, "").startswith(("HOVER", "BOAT")) or t in floaters)
    tot = defaultdict(float)           # (rule, water weapon?) -> damage
    margin = defaultdict(float)        # (margin bin, water weapon?) -> damage under the height rule
    edge = defaultdict(float)          # (edge cell?, water weapon?) -> damage under the height rule
    who = Counter()                    # non-water weapon -> damage under the height rule
    tdefs = Counter()                  # target def -> non-water damage under the height rule
    for p in combatdata.cached(cache):
        series = os.path.basename(p)[:-4]
        if split.split_of("", series) in ("human", "replay"):
            continue
        d = combatdata.load_cached(p)
        if split.split_of(d["meta"].get("game", ""), series) != "train":
            continue
        hm = W["heights"].get(str(d["meta"].get("map", "")).strip().lower().replace(" ", "_"))
        s = d.get("shot")
        if hm is None or s is None or not len(s):
            continue
        s = s[(s["tt"].astype(str) == "u") & (s["dmgt"] > 0) & s["tx"].notna()]
        if not len(s):
            continue
        cell, grid = hm
        r = np.clip((s["tz"].values / cell).astype(int), 0, grid.shape[0] - 1)
        c = np.clip((s["tx"].values / cell).astype(int), 0, grid.shape[1] - 1)
        depth = -grid[r, c]
        # shallowest of the 8 neighbours: a unit can stand up to half a cell into it
        pad = np.pad(-grid, 1, mode="edge")
        shallow = np.min([pad[r + 1 + i, c + 1 + j] for i in (-1, 0, 1) for j in (-1, 0, 1)], axis=0)
        for w, t, dp, sh, dm in zip(s["wd"].astype(str).values, s["tdef"].astype(str).values, depth, shallow,
                                    s["dmgt"].values):
            if dp <= 0:
                continue
            ww = is_water(w)
            m = mc.get(t, "")
            if (m.startswith("UBOAT")) or (m.startswith(("AKBOT", "ATKBOT")) and t not in floaters
                                            and dp >= W["submerged_at"].get(t, 40.0)):
                tot["old", ww] += dm
            ht = height.get(t)
            if m.startswith("UBOAT"):
                dp = sh = W["waterline"].get(t, 0.0)
            if ht is None or surfacing(t) or dp <= ht:
                continue
            tot["uncapped", ww] += dm
            dp, sh = min(dp, maxwd.get(t, dp)), min(sh, maxwd.get(t, sh))
            if dp <= ht:
                continue
            tot["height", ww] += dm
            mg = dp - ht
            margin["0-8" if mg < 8 else "8-16" if mg < 16 else "16-32" if mg < 32 else "32+", ww] += dm
            edge[sh <= ht, ww] += dm
            if not ww:
                who[w] += dm
                tdefs[t] += dm

    def share(dct, k):
        a, b = dct[k, False], dct[k, True]
        return f"non-water {a:9.0f} of {a + b:9.0f} ({a / max(a + b, 1):.1%})"
    print("damage on under-water targets, train games")
    print(f"  old rule (sub in water / amph >= submerged_at): {share(tot, 'old')}")
    print(f"  depth > UnitDefs height, depth uncapped:         {share(tot, 'uncapped')}")
    print(f"  height rule (min(depth, maxWaterDepth) > height): {share(tot, 'height')}")
    print("height rule by margin (depth - height, elmos):")
    for k in ("0-8", "8-16", "16-32", "32+"):
        print(f"  {k:>6}: {share(margin, k)}")
    print("height rule by cell:")
    print(f"  next to a cell too shallow to hide in: {share(edge, True)}")
    print(f"  interior:                              {share(edge, False)}")
    print("non-water weapons (damage):", ", ".join(f"{w} {v:.0f}" for w, v in who.most_common(12)))
    print("on targets (damage):", ", ".join(f"{t} {v:.0f}" for t, v in tdefs.most_common(10)))


if __name__ == "__main__":
    if len(sys.argv) < 3:
        sys.exit(__doc__)
    main(*sys.argv[1:3])
