#!/usr/bin/env python3
"""Export the fitted combat predictor (dps_stop) for the Rust port.

usage: export_tables.py <predictor-dir> <round-dir> <crate-dir>
  <predictor-dir>  zkgbai-runs/predictor (hitcells.pkl, unitdefs.tsv, wtab.pkl, cache/)
  <round-dir>      the round whose hazard.pkl, fights.pkl and fights_pred.tsv are shipped
  <crate-dir>      crates/zkgbai: writes data/fightsim_hits.tsv, data/fightsim_defs.tsv,
                   data/fightsim_coef.tsv and the parity cases under
                   src/military/fightsim/testdata/.
Every table starts with '# fitted <date> ...' header lines the Rust loader skips.
"""
import datetime
import math
import os
import pickle
import sys

import numpy as np
import pandas as pd

import predict
import split
import units as unitmod


def header(fh, what, extra):
    fh.write(f"# fightsim {what}: fitted {datetime.date.today()} by tools/predictor/export_tables.py; {extra}\n")


def export_hits(pdir, out):
    cells = pickle.load(open(os.path.join(pdir, "hitcells.pkl"), "rb"))["train"]
    c = cells.reset_index()[["wd", "tdef", "rb", "sb", "n", "hit_any", "hit_tgt", "dmg", "nominal"]]
    with open(out, "w") as fh:
        src = os.path.join(os.path.basename(os.path.normpath(pdir)), "hitcells.pkl")
        header(fh, "hit cells", f"{len(c)} cells (weapon, target def, range band, speed band), {int(c['n'].sum())} shots, train games only; from {src}")
        fh.write("wd\ttdef\trb\tsb\tn\thit_any\thit_tgt\tdmg\tnominal\n")
        for r in c.itertuples(index=False):
            fh.write(f"{r.wd}\t{r.tdef}\t{int(r.rb)}\t{int(r.sb)}\t{r.n:.10g}\t{r.hit_any:.10g}\t{r.hit_tgt:.10g}\t{r.dmg:.10g}\t{r.nominal:.10g}\n")
    return len(c)


def export_defs(ud, out):
    with open(out, "w") as fh:
        header(fh, "unit def size classes", f"{len(ud.defs)} defs (radius <= 12 small, <= 24 medium, else large; static; air)")
        fh.write("def\tcls\n")
        for name in sorted(ud.defs):
            fh.write(f"{name}\t{ud.defs[name].size_class()}\n")


def fit_calibration(rdir):
    """Refit the round's calibration for dps_stop on its train fights (as predict.evaluate does)."""
    df = pd.read_csv(os.path.join(rdir, "fights_pred.tsv"), sep="\t")
    tr = (df["split"] == "train").values
    dec = (df["win"] >= 0).values
    from scipy.special import logit
    zA = logit(np.clip(df["dstop_A"], 0.005, 0.995)); zB = logit(np.clip(df["dstop_B"], 0.005, 0.995))
    zw = (df["dstop_B"] - df["dstop_A"]).values
    cA = predict.ZOIBeta().fit(zA[tr], df["yA"].values[tr])
    cB = predict.ZOIBeta().fit(zB[tr], df["yB"].values[tr])
    lw = predict.Logit1().fit(zw[tr & dec], df["win"].values[tr & dec])
    check = np.abs(lw.prob(zw) - df["dps_stop_pAwin"].values).max()
    return cA.theta, cB.theta, lw.w, int(tr.sum()), int((tr & dec).sum()), check


def export_coef(rdir, out, ngames):
    hz = pickle.load(open(os.path.join(rdir, "hazard.pkl"), "rb"))
    assert not hz["t0"]
    thA, thB, lw, ntr, ndec, check = fit_calibration(rdir)
    with open(out, "w") as fh:
        header(fh, "coefficients", f"hazard from {os.path.basename(rdir)}/hazard.pkl ({ngames} train games); "
               f"calibration refit on {ntr} train fights ({ndec} decided), max |refit - round| in P(A wins) = {check:.1e}")
        fh.write("key\tvalue\n")
        for n, w in zip(hz["names"], hz["w"]):
            fh.write(f"hazard.{n}\t{w:.10g}\n")
        for i, w in enumerate(lw):
            fh.write(f"win.w{i}\t{w:.10g}\n")
        for side, th in (("A", thA), ("B", thB)):
            for i, w in enumerate(th):
                fh.write(f"zoib{side}.t{i}\t{w:.10g}\n")
    return check


# ----------------------------------------------------------------------------- parity cases
CASES = {
    "raid_mirror": ([("cloakraid", 1.0)] * 3, [("cloakraid", 1.0)] * 3, 400),
    "raid_vs_riot": ([("cloakraid", 1.0)] * 4, [("cloakriot", 1.0)] * 2, 500),
    "skirm_riot_vs_bandits": ([("cloakskirm", 1.0), ("cloakskirm", 1.0), ("cloakriot", 1.0)], [("shieldraid", 1.0)] * 3, 600),
    "assault_vs_llt": ([("shieldassault", 1.0), ("shieldraid", 1.0), ("shieldraid", 1.0)], [("turretlaser", 1.0), ("cloakraid", 1.0)], 450),
    "swarm_vs_hurt_thug": ([("cloakraid", 1.0)] * 5, [("shieldassault", 0.5)], 300),
    "scalpels_vs_darts": ([("hoverskirm", 1.0)] * 2, [("vehraid", 1.0)] * 3, 700),
    "raid_vs_mex": ([("cloakraid", 1.0)] * 2, [("staticmex", 1.0)], 350),
    "big_mixed": ([("cloakraid", 0.8)] * 3 + [("cloakriot", 1.0), ("cloakskirm", 1.0), ("cloakassault", 0.9)],
                  [("shieldraid", 1.0)] * 4 + [("shieldriot", 1.0), ("shieldskirm", 0.7), ("shieldassault", 1.0)], 550),
}


def make_fight(ud, a, b, d0):
    def recs(side, x0, dx):
        out = []
        for i, (name, hpf) in enumerate(side):
            d = ud.get(name)
            out.append({"def": name, "hp": d.health * hpf, "hpmax": d.health, "cost": d.cost, "armed": d.armed,
                        "x": x0 + dx * (i % 3) * 20.0, "z": 100.0 * (i // 3) + 7.0 * i, "static": d.static, "com": False})
        return out
    A, B = recs(a, 0.0, 1.0), recs(b, d0, -1.0)
    ca = (sum(r["x"] for r in A) / len(A), sum(r["z"] for r in A) / len(A))
    cb = (sum(r["x"] for r in B) / len(B), sum(r["z"] for r in B) / len(B))
    return {"A": A, "B": B, "d0": math.hypot(ca[0] - cb[0], ca[1] - cb[1])}


def export_parity(pdir, rdir, tdir):
    ud = unitmod.load(os.path.join(pdir, "unitdefs.tsv"))
    predict.G["ud"] = ud
    predict.G["hits"] = predict.HitModel(pickle.load(open(os.path.join(pdir, "hitcells.pkl"), "rb"))["train"], ud)
    wtab = pickle.load(open(os.path.join(pdir, "wtab.pkl"), "rb"))
    predict.G["wtab"] = wtab
    predict.G["commw"] = {}
    hd = pickle.load(open(os.path.join(rdir, "hazard.pkl"), "rb"))
    import disengage
    disengage.T0 = hd["t0"]
    hz = disengage.Hazard(hd["w"])
    defs_used = sorted({n for a, b, _ in CASES.values() for n, _ in a + b})
    with open(os.path.join(tdir, "parity_units.tsv"), "w") as fh:
        fh.write("# unit specs for the parity cases: speed in elmos per frame; weapons name:type:range:reload:salvo:damage\n")
        fh.write("def\tcls\tcost\thealth\tspeed\tmobile\tweapons\n")
        for name in defs_used:
            d = ud.get(name)
            ws = []
            for w in d.weapons:
                if w.name in wtab.index:
                    r = wtab.loc[w.name]
                    ws.append(f"{w.name}:{r['type']}:{float(r['range']):.10g}:{float(r['reload']):.10g}:{int(r['salvo'])}:{float(r['damage']):.10g}")
            fh.write(f"{name}\t{d.size_class()}\t{d.cost:.10g}\t{d.health:.10g}\t{d.speed:.10g}\t{int(d.canmove)}\t{'|'.join(ws)}\n")
    with open(os.path.join(tdir, "parity_cases.tsv"), "w") as fh:
        fh.write("# Python dps_stop reference: units side:def:hp:x:z; outputs from predict.simulate (deterministic)\n")
        fh.write("case\tunits\tdps_A\tdps_B\tdstop_A\tdstop_B\twd_A\twd_B\n")
        for name, (a, b, d0) in CASES.items():
            f = make_fight(ud, a, b, d0)
            units = predict.build_units(f)
            dps = predict.simulate(units)
            dstop = predict.simulate(units, None, hz)
            wd = predict.simulate(units, None, hz, withdraw=True)
            us = ";".join(f"{s}:{r['def']}:{r['hp']:.10g}:{r['x']:.10g}:{r['z']:.10g}" for s, side in ((0, f["A"]), (1, f["B"])) for r in side)
            fh.write(f"{name}\t{us}\t{dps[0]:.10g}\t{dps[1]:.10g}\t{dstop[0]:.10g}\t{dstop[1]:.10g}\t{wd[0]:.10g}\t{wd[1]:.10g}\n")
            print(f"{name:24} dps {dps[0]:.3f} {dps[1]:.3f}  dstop {dstop[0]:.3f} {dstop[1]:.3f}  withdraw {wd[0]:.3f} {wd[1]:.3f}")


def main(pdir, rdir, crate):
    data = os.path.join(crate, "data")
    tdir = os.path.join(crate, "src", "military", "fightsim", "testdata")
    os.makedirs(tdir, exist_ok=True)
    ud = unitmod.load(os.path.join(pdir, "unitdefs.tsv"))
    n = export_hits(pdir, os.path.join(data, "fightsim_hits.tsv"))
    export_defs(ud, os.path.join(data, "fightsim_defs.tsv"))
    fights = pickle.load(open(os.path.join(rdir, "fights.pkl"), "rb"))
    ngames = len({f["game"] for f in fights if split.split_of(f["game"], f["series"]) == "train"})
    check = export_coef(rdir, os.path.join(data, "fightsim_coef.tsv"), ngames)
    print(f"{n} hit cells, {ngames} train games, calibration refit check {check:.1e}")
    export_parity(pdir, rdir, tdir)


if __name__ == "__main__":
    main(*sys.argv[1:4])
