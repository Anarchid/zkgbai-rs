#!/usr/bin/env python3
"""Cap the HP damage per hit of EMP weapons at nominal in a hit-cell table.

usage: emp_cap.py <hitcells.pkl in> <hitcells.pkl out> [min para share, default 0.5]

The widget sums a shot's paralyze damage into its `dmg`, so weapons whose hits carry EMP
(Venom, Panther, Knight: extra_damage) fit 2-5x their nominal HP damage per hit, and the
sim kills with stun damage. For every weapon whose shots carry paralyze damage at least
`min share` of the time, each cell's dmg is cut to hit_any x nominal per shot (damage
fraction 1). The `para` column is left as it is.
"""
import pickle
import sys

import numpy as np


def cap(cells, share):
    c = cells.copy()
    per = c.groupby(level=0)[["para", "n"]].sum()
    emp = per.index[(per["para"] / per["n"].clip(lower=1e-9)) >= share]
    m = c.index.get_level_values(0).isin(emp)
    limit = c["hit_any"] * c["nominal"] / c["n"].clip(lower=1e-9)
    c.loc[m, "dmg"] = np.minimum(c.loc[m, "dmg"], limit[m])
    if "dmgt" in c.columns:
        c.loc[m, "dmgt"] = np.minimum(c.loc[m, "dmgt"], limit[m])
    return c, sorted(emp)


def main(src, dst, share=0.5):
    tables = pickle.load(open(src, "rb"))
    for sp in list(tables):
        tables[sp], emp = cap(tables[sp], share)
        if sp == "train":
            print(f"capped {len(emp)} weapons at para share >= {share}: {' '.join(emp)}")
    with open(dst, "wb") as fh:
        pickle.dump(tables, fh, protocol=5)


if __name__ == "__main__":
    main(sys.argv[1], sys.argv[2], float(sys.argv[3]) if len(sys.argv) > 3 else 0.5)
