#!/usr/bin/env python3
"""Which non-water weapons were seen hitting submerged targets (train games; respects PRED_DEV_FOLD).

usage: water_hitters.py <cache-dir> <water.pkl> <out.pkl> [min hits=20]
The engine lets only water weapons hit a unit under water, but the journal heightmap (64-elmo cells,
byte heights) cannot tell a sub at depth from one in the shallows, and some non-water weapons do
hit subs there. So mech.py's water switch lets a weapon hit a sub in water, or a non-floater amph
deeper than its amph_submerged_at, only if it is a water weapon or appears here: at least `min
hits` hits on such targets in train games.
"""
import os
import pickle
import sys
from collections import Counter

import numpy as np

import combatdata
import split


def main(cache, water, out, min_hits=20):
    W = pickle.load(open(water, "rb"))
    mc = W["moveclass"]
    floaters = set(W["floaters"])
    sub_hits, amph_hits = Counter(), Counter()
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
        tdef = s["tdef"].astype(str).values
        wd = s["wd"].astype(str).values
        for w, t, dp in zip(wd, tdef, depth):
            m = mc.get(t, "")
            if m.startswith("UBOAT") and dp > 0:
                sub_hits[w] += 1
            elif m.startswith(("AKBOT", "ATKBOT")) and t not in floaters and dp >= W["submerged_at"].get(t, 40.0):
                amph_hits[w] += 1
    hitters = {"sub": {w for w, k in sub_hits.items() if k >= int(min_hits)},
               "deep_amph": {w for w, k in amph_hits.items() if k >= int(min_hits)}}
    pickle.dump(hitters, open(out, "wb"))
    nonwater = lambda ws: sorted(w for w in ws if not W["weapons"].get(w, {}).get("water"))
    print(f"sub hitters {len(hitters['sub'])} (non-water: {nonwater(hitters['sub'])}); deep-amph hitters "
          f"{len(hitters['deep_amph'])} (non-water: {nonwater(hitters['deep_amph'])})")


if __name__ == "__main__":
    main(*sys.argv[1:4], *sys.argv[4:5])
