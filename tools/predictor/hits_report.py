#!/usr/bin/env python3
"""Per-weapon hit-rate tables from the fitted cells (training games)."""
import pickle
import sys

import numpy as np
import pandas as pd

import units as unitmod

cells = pickle.load(open(sys.argv[1], "rb"))["train"]
ud = unitmod.load()
c = cells.reset_index()
c["cls"] = [ud.get(t).size_class() if ud.get(t) else "?" for t in c["tdef"]]
c["dmg_per_shot"] = c["dmg"]
out = sys.argv[2]
with open(out, "w") as fh:
    def table(df, keys, title, min_n=200):
        g = df.groupby(keys).sum(numeric_only=True)
        g = g[g["n"] >= min_n]
        g["p_any"] = (g["hit_any"] / g["n"]).round(3)
        g["p_tgt"] = (g["hit_tgt"] / g["n"]).round(3)
        g["dmg/shot"] = (g["dmg"] / g["n"]).round(1)
        g["nominal/shot"] = (g["nominal"] / g["n"]).round(1)
        g["landed"] = (g["dmg"] / g["nominal"].replace(0, np.nan)).round(3)
        fh.write(f"\n## {title}\n\n")
        fh.write(g[["n", "p_any", "p_tgt", "dmg/shot", "nominal/shot", "landed"]].sort_values("n", ascending=False).to_string())
        fh.write("\n")
    table(c, ["wd"], "by weapon (n >= 200 shots at enemy units)")
    table(c, ["wd", "cls"], "by weapon and target size class", 500)
    table(c, ["wd", "rb"], "by weapon and range band (fifths of range; 5 = beyond)", 500)
    table(c, ["wd", "sb"], "by weapon and target speed band (0: <0.5, 1: <2, 2: <4, 3: >=4 elmos/frame)", 500)
print("->", out)
