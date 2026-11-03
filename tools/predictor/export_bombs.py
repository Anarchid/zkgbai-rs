#!/usr/bin/env python3
"""Parity cases for the `bombs` flag (2026-11-03): each kamikaze goes off within its def's kamikazeDistance
(mech.KAMIKAZE_DIST: Imp and Snitch 80, Skuttle 25, Limpet 120) of its nearest enemy instead of 20. At 20 the
plane fight's overlap pass held every bomb off its target (radii add up past 20), so none went off on contact.
mech.simulate_mech with each of export_mech.VARIANTS plus bombs, on Imps, Snitches, a Skuttle and a Limpet against
skirmishers, a Mace and raiders. The crate's bomb-charge tests (fightsim/bomb.rs) read the same unit specs.

usage: python3 export_bombs.py <refit dir, e.g. ~/Programs/zkgbai-runs/predictor/refit2> <crate dir>
writes src/military/fightsim/testdata/mech_bombs_{units,cases,fates}.tsv"""
import os
import sys

import export_mech as E
import mech
import predict

# (A, B, centroid distance, layout)
CASES = {
    "imp_glaives_rogues": ([("cloakbomb", 1.0)] + [("cloakraid", 1.0)] * 3, [("shieldskirm", 1.0)] * 3, 300.3, "crowd"),
    "snitch_ronins": ([("shieldbomb", 1.0)] + [("cloakraid", 1.0)] * 2, [("cloakskirm", 1.0)] * 4, 260.7, "crowd"),
    "imp_lotus_mace": ([("cloakbomb", 1.0), ("turretlaser", 1.0)], [("hoverriot", 1.0)], 250.2, "crowd"),
    "snitch_knight_mace_rogues": ([("shieldbomb", 1.0), ("cloakassault", 1.0)],
                                  [("hoverriot", 1.0)] + [("shieldskirm", 1.0)] * 2, 300.9, "crowd"),
    "skuttle_grizzly": ([("jumpbomb", 1.0)], [("amphassault", 1.0)], 250.0, "crowd"),
    "limpet_daggers": ([("amphbomb", 1.0), ("amphraid", 1.0)], [("hoverraid", 1.0)] * 4, 300.4, "crowd"),
    # Nothing shoots these bombs down first, so where they go off shows.
    "imp_glaive_cons": ([("cloakbomb", 1.0), ("cloakraid", 1.0)], [("cloakcon", 1.0)] * 3, 250.6, "crowd"),
    "snitch_solars": ([("shieldbomb", 1.0)], [("energysolar", 1.0)] * 3, 240.2, "crowd"),
    "skuttle_cons": ([("jumpbomb", 1.0)], [("amphcon", 1.0)] * 2, 200.8, "crowd"),
    "snitch_ring_cons": ([("shieldbomb", 1.0)] * 2, [("shieldcon", 1.0)] * 2, 220.0, "ring:0"),
}
# Fates horizons (frames).
FATES = {"imp_glaives_rogues": 450, "snitch_ronins": 450, "imp_lotus_mace": 300, "skuttle_grizzly": 450,
         "limpet_daggers": 450, "imp_glaive_cons": 300, "snitch_solars": 300, "skuttle_cons": 450, "snitch_ring_cons": 300}


def main(rdir, crate):
    E.init(rdir)
    ud, wtab = predict.G["ud"], predict.G["wtab"]
    tdir = os.path.join(crate, "src", "military", "fightsim", "testdata")
    defs = sorted({n for a, b, *_ in CASES.values() for n, _ in a + b})
    with open(os.path.join(tdir, "mech_bombs_units.tsv"), "w") as fh:
        fh.write("# unit specs for the bombs parity cases and the bomb-charge tests, as mech_parity_units.tsv\n")
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
    with open(os.path.join(tdir, "mech_bombs_cases.tsv"), "w") as fh:
        fh.write("# Python mech.simulate_mech per variant + bombs (kamikazeDistance per def), opts six + heatray + slow2d"
                 " + sreload: units side:def:hp:x:z; dps = no hazard, stop = with the hazard, wd = A withdraws;"
                 " off_dps = the same without bombs\n")
        fh.write("case\tvariant\tunits\tdps_A\tdps_B\tstop_A\tstop_B\twd_A\twd_B\toff_dps_A\toff_dps_B\n")
        for name, (a, b, d0, layout) in CASES.items():
            f = E.make_vfight(ud, a, b, d0, layout)
            for v in E.VARIANTS:
                o = dict(E.opts_of(v), bombs=1)
                o_off = dict(E.opts_of(v), bombs=0)
                dps = mech.simulate_mech(f, o, None)
                stop = mech.simulate_mech(f, o, E.G["hz"])
                wd = mech.simulate_mech(f, o, E.G["hz"], withdraw=True)
                off = mech.simulate_mech(f, o_off, None)
                fh.write(f"{name}\t{v}\t{E._units(f)}\t{dps[0]:.10g}\t{dps[1]:.10g}\t{stop[0]:.10g}\t{stop[1]:.10g}\t{wd[0]:.10g}"
                         f"\t{wd[1]:.10g}\t{off[0]:.10g}\t{off[1]:.10g}\n")
                print(f"{name:26} {v:6} dps {dps[0]:.3f} {dps[1]:.3f} (off {off[0]:.3f} {off[1]:.3f})  stop {stop[0]:.3f} {stop[1]:.3f}"
                      f"  wd {wd[0]:.3f} {wd[1]:.3f}")
    with open(os.path.join(tdir, "mech_bombs_fates.tsv"), "w") as fh:
        fh.write("# Python mech.simulate_mech fates per variant + bombs (no hazard), a-move: units side:def:hp:x:z; fates per"
                 " unit dead:death frame (-1 alive):hp left:pdead\n")
        fh.write("case\tvariant\tunits\thorizon\tfates\n")
        for name, horizon in FATES.items():
            a, b, d0, layout = CASES[name]
            f = E.make_vfight(ud, a, b, d0, layout)
            for v in E.VARIANTS:
                fa = {}
                mech.simulate_mech(f, dict(E.opts_of(v), bombs=1), None, fates=fa, horizon=horizon)
                fs = ";".join(f"{int(d)}:{-1 if t is None else t}:{h:.10g}:{p:.10g}"
                              for d, t, h, p in zip(fa["dead"], fa["death_f"], fa["hp"], fa["pdead"]))
                fh.write(f"{name}\t{v}\t{E._units(f)}\t{horizon}\t{fs}\n")
                print(f"{name:26} {v:6} " + " ".join(f"{'X' if d else '.'}{p:.2f}@{-1 if t is None else t}"
                                                      for d, t, p in zip(fa["dead"], fa["death_f"], fa["pdead"])))


if __name__ == "__main__":
    main(os.path.expanduser(sys.argv[1]), os.path.expanduser(sys.argv[2]))
