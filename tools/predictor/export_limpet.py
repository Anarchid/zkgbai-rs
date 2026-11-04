#!/usr/bin/env python3
"""Export Limpet mechanics parity fixtures (no refit).

python3 tools/predictor/export_limpet.py REFIT2_DIR CRATE_DIR
Uses the existing mech_bombs_units.tsv specs and compiled-model hit tables.
"""
import sys
from pathlib import Path

import export_mech as E
import mech
import predict

CASES = {
    "ducks_daggers": ([("amphbomb", 1), ("amphraid", 1), ("amphraid", 1)], [("hoverraid", 1)]*4, 300.4),
    "ducks_mace": ([("amphbomb", 1)]+[("amphraid", 1)]*4, [("hoverriot", 1)], 320.3),
    "ducks_cons": ([("amphbomb", 1)]+[("amphraid", 1)]*4, [("amphcon", 1)]*3, 320.3),
    "glaives_rogues": ([("amphbomb", 1)]+[("cloakraid", 1)]*4, [("shieldskirm", 1)]*3, 300.3),
    "limpet_alone": ([("amphbomb", 1)], [("hoverraid", 1)]*4, 300.4),
}


def main(refit, crate):
    E.init(refit)
    out = Path(crate)/"src/military/fightsim/testdata/mech_limpet_fates.tsv"
    with out.open("w") as fh:
        fh.write("# Limpet death customparams in every formulation; Python mechanics fates, no hazard.\n")
        fh.write("case\tvariant\tbombs\tunits\thorizon\tfates\n")
        for name, (a, b, distance) in CASES.items():
            fight = E.make_fight(predict.G["ud"], a, b, distance, "crowd")
            for variant in ("base", "sr", "focus", "2d"):
                for bombs in (0, 1):
                    for horizon in (300, 900):
                        fates = {}
                        mech.simulate_mech(fight, dict(E.opts_of(variant), bombs=bombs),
                                           fates=fates, horizon=horizon)
                        rows = ";".join(f"{int(d)}:{-1 if t is None else t}:{h:.10g}:{p:.10g}"
                                        for d, t, h, p in zip(fates["dead"], fates["death_f"], fates["hp"], fates["pdead"]))
                        fh.write(f"{name}\t{variant}\t{bombs}\t{E._units(fight)}\t{horizon}\t{rows}\n")
    print(out)


if __name__ == "__main__":
    main(*sys.argv[1:])
