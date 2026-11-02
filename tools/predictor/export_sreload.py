#!/usr/bin/env python3
"""Parity cases for the `sreload` switch (tube weapons: ZK scriptReload.lua, customParams script_burst tubes
each reloading script_reload s after it fires; Picket, Ogre, Trident, Hacksaw, Angler, Disco Rave Party):
mech.simulate_mech with each of export_mech.VARIANTS plus sreload, on Pickets and Ogres against raiders and
assaults, crowded and ringed. The tubes come from the crate's data/fightsim_sreload.tsv (made by sreload.lua over
the unit defs), so Python and Rust read the same table.

usage: python3 export_sreload.py <refit dir, e.g. ~/Programs/zkgbai-runs/predictor/refit2> <crate dir>
writes src/military/fightsim/testdata/mech_sreload_{units,cases,fates}.tsv"""
import os
import sys

import export_mech as E
import mech
import predict

# (A, B, centroid distance, layout): Pickets and Ogres on either side.
CASES = {
    "picket_kodachis": ([("tankraid", 1.0)] * 4, [("turretmissile", 1.0)], 450.3, "crowd"),
    "pickets_mex_glaives": ([("cloakraid", 1.0)] * 5, [("turretmissile", 1.0), ("turretmissile", 0.6), ("staticmex", 1.0)], 520.7,
                            "crowd"),
    "picket_ring": ([("tankraid", 1.0)] * 4, [("turretmissile", 1.0)], 400.0, "ring:0"),
    "ogre_daggers": ([("hoverraid", 1.0)] * 6, [("tankriot", 1.0)], 330.1, "crowd"),
    "ogres_thugs": ([("shieldassault", 1.0)] * 3, [("tankriot", 1.0)] * 2, 360.4, "crowd"),
    "own_pickets": ([("turretmissile", 1.0)] * 2 + [("cloakraid", 1.0)], [("hoverraid", 1.0)] * 5, 380.9, "crowd"),
}
# Fates horizons (frames) for some of them.
FATES = {"picket_kodachis": 300, "picket_ring": 450, "ogre_daggers": 300, "ogres_thugs": 900, "own_pickets": 600}


def load_crate_sreload(crate):
    """The crate's tube table, as fightsim::Model::parse reads it."""
    tab = {}
    for line in open(os.path.join(crate, "data", "fightsim_sreload.tsv")):
        if not line.strip() or line.startswith("#") or line.startswith("weapon\t"):
            continue
        name, tubes, secs = line.rstrip("\n").split("\t")
        tab[name] = (max(1, int(tubes)), max(1, int(round(float(secs) * 30))))
    mech.M["sreload"] = tab


def main(rdir, crate):
    E.init(rdir)
    load_crate_sreload(crate)
    ud, wtab = predict.G["ud"], predict.G["wtab"]
    tdir = os.path.join(crate, "src", "military", "fightsim", "testdata")
    defs = sorted({n for a, b, *_ in CASES.values() for n, _ in a + b})
    with open(os.path.join(tdir, "mech_sreload_units.tsv"), "w") as fh:
        fh.write("# unit specs for the sreload parity cases, as mech_parity_units.tsv\n")
        fh.write("def\tcost\thealth\tspeed\tmobile\tair\tbuilder\tcom\tradius\tweapons\n")
        for name in defs:
            d = ud.get(name)
            ws = []
            for w in d.weapons:
                if w.name in wtab.index and float(wtab.loc[w.name, "range"]) > 0 and wtab.loc[w.name, "type"] != "Shield":
                    r = wtab.loc[w.name]
                    ws.append(f"{w.name}:{r['type']}:{float(r['range']):.10g}:{float(r['reload']):.10g}:{int(r['salvo'])}:{float(r['damage']):.10g}")
            fh.write(f"{name}\t{d.cost:.10g}\t{d.health:.10g}\t{d.speed:.10g}\t{int(d.canmove)}\t{int(d.canfly)}\t{int(d.builder)}\t0\t"
                     f"{d.radius:.10g}\t{'|'.join(ws)}\n")
    with open(os.path.join(tdir, "mech_sreload_cases.tsv"), "w") as fh:
        fh.write("# Python mech.simulate_mech per variant + sreload (tube weapons, the crate's fightsim_sreload.tsv), opts six +"
                 " heatray + slow2d: units side:def:hp:x:z; dps = no hazard, stop = with the hazard, wd = A withdraws;"
                 " off_dps = the same without sreload\n")
        fh.write("case\tvariant\tunits\tdps_A\tdps_B\tstop_A\tstop_B\twd_A\twd_B\toff_dps_A\toff_dps_B\n")
        for name, (a, b, d0, layout) in CASES.items():
            f = E.make_vfight(ud, a, b, d0, layout)
            for v in E.VARIANTS:
                o = dict(E.opts_of(v), sreload=1)
                dps = mech.simulate_mech(f, o, None)
                stop = mech.simulate_mech(f, o, E.G["hz"])
                wd = mech.simulate_mech(f, o, E.G["hz"], withdraw=True)
                off = mech.simulate_mech(f, E.opts_of(v), None)
                fh.write(f"{name}\t{v}\t{E._units(f)}\t{dps[0]:.10g}\t{dps[1]:.10g}\t{stop[0]:.10g}\t{stop[1]:.10g}\t{wd[0]:.10g}"
                         f"\t{wd[1]:.10g}\t{off[0]:.10g}\t{off[1]:.10g}\n")
                print(f"{name:20} {v:6} dps {dps[0]:.3f} {dps[1]:.3f} (off {off[0]:.3f} {off[1]:.3f})  stop {stop[0]:.3f} {stop[1]:.3f}"
                      f"  wd {wd[0]:.3f} {wd[1]:.3f}")
    with open(os.path.join(tdir, "mech_sreload_fates.tsv"), "w") as fh:
        fh.write("# Python mech.simulate_mech fates per variant + sreload (no hazard), a-move: units side:def:hp:x:z; fates per"
                 " unit dead:death frame (-1 alive):hp left:pdead\n")
        fh.write("case\tvariant\tunits\thorizon\tfates\n")
        for name, horizon in FATES.items():
            a, b, d0, layout = CASES[name]
            f = E.make_vfight(ud, a, b, d0, layout)
            for v in E.VARIANTS:
                fa = {}
                mech.simulate_mech(f, dict(E.opts_of(v), sreload=1), None, fates=fa, horizon=horizon)
                fs = ";".join(f"{int(d)}:{-1 if t is None else t}:{h:.10g}:{p:.10g}"
                              for d, t, h, p in zip(fa["dead"], fa["death_f"], fa["hp"], fa["pdead"]))
                fh.write(f"{name}\t{v}\t{E._units(f)}\t{horizon}\t{fs}\n")
                print(f"{name:20} {v:6} " + " ".join(f"{'X' if d else '.'}{p:.2f}" for d, p in zip(fa["dead"], fa["pdead"])))


if __name__ == "__main__":
    main(os.path.expanduser(sys.argv[1]), os.path.expanduser(sys.argv[2]))
