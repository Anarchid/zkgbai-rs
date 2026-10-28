#!/usr/bin/env python3
"""Export the mechanics model (round-1 six + heatray) for the Rust port.

usage: export_mech.py <refit2-dir> <crate-dir> [workers] [parity]
  <refit2-dir>  zkgbai-runs/predictor/refit2: mech.pkl (fitted mechanics), empcap/hitcells.pkl
                (heatray damage by range band), fights.pkl (calibration fights)
  <crate-dir>   crates/zkgbai: writes data/fightsim_mech.tsv and the parity cases
                src/military/fightsim/testdata/mech_parity_{units,cases}.tsv and mech_fates_cases.tsv
  parity        only rewrite the parity cases (no calibration fit, tables untouched)
The sim runs on the crate's own base tables (refit-1 hit cells and hazard, the ones in
data/fightsim_hits.tsv and fightsim_coef.tsv): predict._init on ~/.../refit1/empcap with
PRED_HAZARD=refit1/hazard.pkl, so the calibration and the parity numbers match what Rust runs.
Opts: air, burn, death, emp, slow, engage, heatray (mech.py).
"""
import datetime
import math
import os
import pickle
import random
import sys
from multiprocessing import Pool

import numpy as np
from scipy.special import logit

import mech
import predict
import split

ROOT = os.path.expanduser("~/Programs/zkgbai-runs/predictor")
BASE = os.path.join(ROOT, "refit1", "empcap")           # the crate's hit tables
HAZARD = os.path.join(ROOT, "refit1", "hazard.pkl")     # the crate's hazard
SHIELD = os.path.join(ROOT, "mech2", "shield.json")      # _dyn_weapons for heatray
FATES_CAL = os.path.join(ROOT, "fates", "cal.tsv")       # score_fates.py: the p_dead recalibration
OPTS = {"air": 1, "burn": 1, "death": 1, "emp": 1, "slow": 1, "engage": 1, "heatray": 1}
G = {}


def init(rdir):
    os.environ["PRED_HAZARD"] = HAZARD
    predict._init(BASE, os.path.join(BASE, "cache"))
    mech.load(os.path.join(rdir, "mech.pkl"))
    mech.load_shield(SHIELD, os.path.join(rdir, "empcap", "hitcells.pkl"))
    G["hz"] = predict.G["hazard"]


def run(f):
    return mech.simulate_mech(f, OPTS, G["hz"])


def header(fh, what):
    fh.write(f"# fightsim {what}: fitted {datetime.date.today()} by tools/predictor/export_mech.py; {G['note']}\n")


# ----------------------------------------------------------------------------- tables
def export_tables(out, cal):
    M = mech.M
    names = set(M.get("air", {})) | set(M.get("burn", {}))
    names |= {k for k, v in M.get("udw", {}).items() if v["paralyzer"] or v["extra_damage"] > 0 or v["slow"] > 0}
    rows = 0
    with open(out, "w") as fh:
        header(fh, "mechanics")
        fh.write("kind\tname\tvalues\n")
        for n in sorted(names):
            air = M.get("air", {}).get(n, (True, True))
            burn = M.get("burn", {}).get(n, (0.0, 0))
            u = M.get("udw", {}).get(n)
            emp = 0.0
            slow = 0.0
            para = 0
            if u is not None:
                para = int(u["paralyzer"])
                emp = u["damage"] if u["paralyzer"] else (u["extra_damage"] if u["extra_damage"] > 0 else 0.0)
                slow = u["slow"]
            fh.write(f"weapon\t{n}\t{int(air[0])}\t{int(air[1])}\t{burn[0]:.10g}\t{int(burn[1])}\t{emp:.10g}\t{slow:.10g}\t{para}\n")
            rows += 1
        for d, (dmg, aoe, edge, para) in sorted(M.get("death", {}).items()):
            fh.write(f"death\t{d}\t{dmg:.10g}\t{aoe:.10g}\t{edge:.10g}\t{int(bool(para))}\n")
        for wd, bands in sorted(M.get("dynfrac", {}).items()):
            for rb, v in sorted(bands.items()):
                fh.write(f"dyn\t{wd}\t{rb}\t{v:.10g}\n")
        for n, w in zip(mech.ENGAGE_NAMES, M["engage_w"]):
            fh.write(f"engage\t{n}\t{w:.10g}\n")
        fh.write(f"const\tstun_cap_s\t{M.get('stun_cap_s', 4.0):.10g}\n")
        for k, v in cal.items():
            fh.write(f"cal\t{k}\t{v:.10g}\n")
        # per-unit fates: score_fates.py's recalibration of p_dead (logistic on logit p_dead, static, log(h / 150))
        for i, v in enumerate(float(l.split("\t")[1]) for l in open(FATES_CAL).read().split("\n")[1:] if l):
            fh.write(f"cal\tfate.t{i}\t{v:.10g}\n")
    return rows


def fit_calibration(rdir, workers, n_cal=12000):
    fs = pickle.load(open(os.path.join(rdir, "fights.pkl"), "rb"))
    sel = [f for f in fs if f["closed"] and f["costA"] >= predict.MIN_COST and f["costB"] >= predict.MIN_COST
           and not math.isnan(f["yA"]) and not math.isnan(f["yB"]) and split.split_of(f["game"], f["series"]) == "train"]
    del fs
    random.seed(17)
    sel = random.sample(sel, min(n_cal, len(sel)))
    with Pool(workers, initializer=init, initargs=(rdir,)) as pool:
        out = pool.map(run, sel, chunksize=32)
    kA = np.array([o[0] for o in out]); kB = np.array([o[1] for o in out])
    yA = np.array([f["yA"] for f in sel]); yB = np.array([f["yB"] for f in sel])
    win = np.where(yB - yA > predict.DECIDE, 1, np.where(yA - yB > predict.DECIDE, 0, -1))
    zA = logit(np.clip(kA, 0.005, 0.995)); zB = logit(np.clip(kB, 0.005, 0.995)); zw = kB - kA
    cA = predict.ZOIBeta().fit(zA, yA); cB = predict.ZOIBeta().fit(zB, yB)
    dec = win >= 0
    lw = predict.Logit1().fit(zw[dec], win[dec])
    cal = {"win.w0": lw.w[0], "win.w1": lw.w[1]}
    for side, c in (("A", cA), ("B", cB)):
        for i, t in enumerate(c.theta):
            cal[f"zoib{side}.t{i}"] = t
    return cal, len(sel), int(dec.sum())


# ----------------------------------------------------------------------------- parity cases
# Each case exercises a component: side A units, side B units (def, hp share), A-B centroid distance.
# Rust carries unit speeds as f32: a case whose units stop exactly on 95% of their range (round
# distances) can split on that tie, so distances here are chosen off such ties.
CASES = {
    "fire_kodachi": ([("tankraid", 1.0)] * 3, [("cloakraid", 1.0)] * 4, 260),
    "emp_venom": ([("spideremp", 1.0)] * 2, [("shieldassault", 1.0)] * 2, 300),
    # pure EMP: inert on the refit-1 tables (the Faraday's fitted hit rate is ~1e-8), the code path still runs
    "emp_faraday": ([("turretemp", 1.0)], [("cloakraid", 1.0)] * 3, 380),
    "imp_death": ([("cloakbomb", 1.0), ("cloakraid", 1.0)], [("shieldraid", 1.0)] * 3, 150.7),
    "slow_bolas": ([("hoverheavyraid", 1.0)] * 3, [("tankassault", 1.0)], 301.1),
    "aa_vs_air": ([("gunshipraid", 1.0)] * 2, [("shieldraid", 1.0)] * 3, 301.9),
    "kamikaze_skuttle": ([("jumpbomb", 1.0)], [("amphassault", 0.6)], 250),
    "heatray_short": ([("vehraid", 1.0)] * 2, [("shieldassault", 1.0)] * 2, 60),
    "heatray_long": ([("vehraid", 1.0)] * 2, [("turretlaser", 1.0)], 255),
    "engage_crowd": ([("cloakskirm", 1.0)] * 6, [("shieldassault", 1.0)] * 3 + [("shieldriot", 1.0)] * 2, 470.3),
    "engage_line": ([("hoverskirm", 1.0)] * 2 + [("hoverassault", 1.0)] * 2, [("tankassault", 1.0), ("tankriot", 1.0)], 450),
    "mirror_mixed": ([("cloakraid", 1.0)] * 4 + [("cloakriot", 0.7)], [("shieldraid", 1.0)] * 4 + [("shieldassault", 0.8)], 303.7),
    "imps_and_fire": ([("cloakbomb", 1.0)] * 2 + [("cloakraid", 1.0)] * 3, [("tankraid", 1.0), ("spideremp", 0.9), ("vehraid", 1.0)], 260),
    "big_mixed": ([("cloakraid", 0.8)] * 3 + [("cloakriot", 1.0), ("cloakskirm", 1.0), ("cloakassault", 0.9), ("cloakcon", 1.0)],
                  [("shieldraid", 1.0)] * 4 + [("shieldriot", 1.0), ("shieldskirm", 0.7), ("shieldassault", 1.0), ("turretlaser", 1.0)], 550),
}

# Fates cases: (A, B, centroid distance, pass offset or None for the a-move, horizon in frames).
FATE_CASES = {
    "amove_mirror": (CASES["mirror_mixed"][0], CASES["mirror_mixed"][1], 303.7, None, 300),
    "amove_eco": ([("cloakraid", 1.0)] * 2, [("staticmex", 1.0)] * 3, 260.3, None, 450),
    "amove_imps": (CASES["imps_and_fire"][0], CASES["imps_and_fire"][1], 260, None, 2700),
    "pass_ram": ([("vehassault", 1.0)] * 4, [("staticmex", 1.0)] * 3 + [("turretlaser", 1.0)], 420.3, 0, 150),
    "pass_wide": ([("vehassault", 1.0)] * 4, [("staticmex", 1.0)] * 3 + [("turretlaser", 1.0)], 420.3, 220, 150),
    "pass_raiders": ([("vehassault", 0.8)] * 3, [("shieldraid", 1.0)] * 4 + [("turretlaser", 1.0)], 380.9, 220, 300),
    "pass_bolas": ([("hoverheavyraid", 1.0)] * 3, [("tankassault", 1.0), ("turretriot", 1.0)], 350.2, 120, 240),
}


def make_fight(ud, a, b, d0, layout):
    def recs(side, x0, dx):
        out = []
        for i, (name, hpf) in enumerate(side):
            d = ud.get(name)
            if layout == "line":  # units in single file along the axis: front units block the rear
                x, z = x0 + dx * 30.0 * i, 3.0 * i
            else:  # a crowd: rows of three, 20-25 elmos apart
                x, z = x0 + dx * (i % 3) * 20.0, 25.0 * (i // 3) + 7.0 * i
            out.append({"def": name, "hp": d.health * hpf, "hpmax": d.health, "cost": d.cost, "armed": d.armed,
                        "x": x, "z": z, "static": d.static, "com": False})
        return out
    A, B = recs(a, 0.0, -1.0), recs(b, d0, 1.0)
    ca = (sum(r["x"] for r in A) / len(A), sum(r["z"] for r in A) / len(A))
    cb = (sum(r["x"] for r in B) / len(B), sum(r["z"] for r in B) / len(B))
    return {"A": A, "B": B, "d0": math.hypot(ca[0] - cb[0], ca[1] - cb[1]), "game": "parity", "fid": 0}


def export_parity(tdir):
    ud, wtab = predict.G["ud"], predict.G["wtab"]
    defs = sorted({n for a, b, *_ in list(CASES.values()) + list(FATE_CASES.values()) for n, _ in a + b})
    with open(os.path.join(tdir, "mech_parity_units.tsv"), "w") as fh:
        fh.write("# unit specs for the mechanics parity cases (speed elmos/frame); weapons name:type:range:reload:salvo:damage,"
                 " every weapon with a weapon-table row and range (EMP-only ones included)\n")
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
    with open(os.path.join(tdir, "mech_parity_cases.tsv"), "w") as fh:
        fh.write("# Python mech.simulate_mech, opts six + heatray: units side:def:hp:x:z; dps = no hazard, stop = with the hazard,"
                 " wd = A withdraws, with the hazard\n")
        fh.write("case\tunits\tdps_A\tdps_B\tstop_A\tstop_B\twd_A\twd_B\n")
        for name, (a, b, d0) in CASES.items():
            f = make_fight(ud, a, b, d0, "line" if name == "engage_line" else "crowd")
            dps = mech.simulate_mech(f, OPTS, None)
            stop = mech.simulate_mech(f, OPTS, G["hz"])
            wd = mech.simulate_mech(f, OPTS, G["hz"], withdraw=True)
            us = ";".join(f"{s}:{r['def']}:{r['hp']:.10g}:{r['x']:.10g}:{r['z']:.10g}" for s, side in ((0, f["A"]), (1, f["B"])) for r in side)
            fh.write(f"{name}\t{us}\t{dps[0]:.10g}\t{dps[1]:.10g}\t{stop[0]:.10g}\t{stop[1]:.10g}\t{wd[0]:.10g}\t{wd[1]:.10g}\n")
            print(f"{name:18} dps {dps[0]:.3f} {dps[1]:.3f}  stop {stop[0]:.3f} {stop[1]:.3f}  wd {wd[0]:.3f} {wd[1]:.3f}")
    with open(os.path.join(tdir, "mech_fates_cases.tsv"), "w") as fh:
        fh.write("# Python mech.simulate_mech fates (no hazard), opts six + heatray: units side:def:hp:x:z; pass = the pass line's"
                 " offset from B's statics, empty for the a-move; fates per unit dead:death frame (-1 alive):hp left:pdead\n")
        fh.write("case\tunits\tpass\thorizon\tfates\n")
        for name, (a, b, d0, offset, horizon) in FATE_CASES.items():
            f = make_fight(ud, a, b, d0, "crowd")
            fa = {}
            mech.simulate_mech(f, OPTS, None, fates=fa, horizon=horizon, passing=offset)
            us = ";".join(f"{s}:{r['def']}:{r['hp']:.10g}:{r['x']:.10g}:{r['z']:.10g}" for s, side in ((0, f["A"]), (1, f["B"])) for r in side)
            fs = ";".join(f"{int(d)}:{-1 if t is None else t}:{h:.10g}:{p:.10g}"
                          for d, t, h, p in zip(fa["dead"], fa["death_f"], fa["hp"], fa["pdead"]))
            fh.write(f"{name}\t{us}\t{'' if offset is None else offset}\t{horizon}\t{fs}\n")
            print(f"{name:18} " + " ".join(f"{'X' if d else '.'}{p:.2f}" for d, p in zip(fa["dead"], fa["pdead"])))


def main(rdir, crate, workers=2, what="all"):
    workers = int(workers)
    if what != "parity":
        cal, n, nd = fit_calibration(rdir, workers)
    init(rdir)
    if what != "parity":
        G["note"] = (f"mechanics from {os.path.basename(rdir)}/mech.pkl, heatray bands from its empcap hit cells, on the crate's "
                     f"refit-1 tables and hazard; calibration on {n} train fights ({nd} decided)")
        rows = export_tables(os.path.join(crate, "data", "fightsim_mech.tsv"), cal)
        print(f"{rows} weapon rows; calibration on {n} fights: win {cal['win.w0']:.3f} {cal['win.w1']:.3f}")
    export_parity(os.path.join(crate, "src", "military", "fightsim", "testdata"))


if __name__ == "__main__":
    main(*sys.argv[1:5])
