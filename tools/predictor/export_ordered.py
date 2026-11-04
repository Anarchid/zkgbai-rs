#!/usr/bin/env python3
"""Parity cases for the `ordered` switch (side A fires for certain in place of the engage prior; fight lab
2026-11-03): mech.simulate_mech on export_sreload's cases (Pickets and Ogres against raiders and assaults, some with
empty tubes) with each of export_mech.VARIANTS + sreload + ordered, and the same without ordered.

usage: python3 export_ordered.py <refit dir, e.g. ~/Programs/zkgbai-runs/predictor/refit2> <crate dir>
writes src/military/fightsim/testdata/mech_ordered_{cases,fates}.tsv (units: mech_sreload_units.tsv)"""
import os
import sys

import export_mech as E
import export_sreload as S
import mech
import predict


def main(rdir, crate):
    E.init(rdir)
    ud = predict.G["ud"]
    tdir = os.path.join(crate, "src", "military", "fightsim", "testdata")
    with open(os.path.join(tdir, "mech_ordered_cases.tsv"), "w") as fh:
        fh.write("# Python mech.simulate_mech per variant + sreload + ordered (side A fires for certain), opts six + heatray +"
                 " slow2d: units side:def:hp:x:z[:empty tubes]; dps = no hazard, stop = with the hazard, wd = A withdraws;"
                 " off_dps = the same without ordered\n")
        fh.write("case\tvariant\tunits\tdps_A\tdps_B\tstop_A\tstop_B\twd_A\twd_B\toff_dps_A\toff_dps_B\n")
        for name in S.CASES:
            f = S.fight_of(ud, name)
            for v in E.VARIANTS:
                o = dict(E.opts_of(v), sreload=1, ordered=1)
                o_off = dict(E.opts_of(v), sreload=1)
                dps = mech.simulate_mech(f, o, None)
                stop = mech.simulate_mech(f, o, E.G["hz"])
                wd = mech.simulate_mech(f, o, E.G["hz"], withdraw=True)
                off = mech.simulate_mech(f, o_off, None)
                fh.write(f"{name}\t{v}\t{E._units(f)}\t{dps[0]:.10g}\t{dps[1]:.10g}\t{stop[0]:.10g}\t{stop[1]:.10g}\t{wd[0]:.10g}"
                         f"\t{wd[1]:.10g}\t{off[0]:.10g}\t{off[1]:.10g}\n")
                print(f"{name:20} {v:6} dps {dps[0]:.3f} {dps[1]:.3f} (off {off[0]:.3f} {off[1]:.3f})")
    with open(os.path.join(tdir, "mech_ordered_fates.tsv"), "w") as fh:
        fh.write("# Python mech.simulate_mech fates per variant + sreload + ordered (no hazard), a-move: units side:def:hp:x:z;"
                 " fates per unit dead:death frame (-1 alive):hp left:pdead\n")
        fh.write("case\tvariant\tunits\thorizon\tfates\n")
        for name, horizon in S.FATES.items():
            f = S.fight_of(ud, name)
            for v in E.VARIANTS:
                fa = {}
                mech.simulate_mech(f, dict(E.opts_of(v), sreload=1, ordered=1), None, fates=fa, horizon=horizon)
                fs = ";".join(f"{int(d)}:{-1 if t is None else t}:{h:.10g}:{p:.10g}"
                              for d, t, h, p in zip(fa["dead"], fa["death_f"], fa["hp"], fa["pdead"]))
                fh.write(f"{name}\t{v}\t{E._units(f)}\t{horizon}\t{fs}\n")
                print(f"{name:20} {v:6} " + " ".join(f"{'X' if d else '.'}{p:.2f}" for d, p in zip(fa["dead"], fa["pdead"])))


if __name__ == "__main__":
    main(os.path.expanduser(sys.argv[1]), os.path.expanduser(sys.argv[2]))
