#!/usr/bin/env python3
"""Radar vs LOS read: hit rate of few-shot guns by the target's visibility at the fire frame.

Combat logs before widget v2 carry no LOS flag (v2 adds shot.tlos), so two proxies:
  track  from the log's unit rows: self = target within the shooter's own sightDistance;
         friend = another logged (combat-active) friendly unit within its sightDistance of
         the target at f0 +-30 frames; none = no logged friendly unit sees it (radar-only OR
         seen by an unlogged unit: scouts, radars, idle units, buildings).
  journal  ZKGBAI's own journal snap (every 150 frames = 5 s, lag <= 2.5 s): the target's
         flags in snap.enemies at the nearest snap: vis (bit 1), radar (bit 2 without 1),
         absent = not in the AI's enemy list. Only shots by a team with a journal.
usage: los_read.py <cache-dir> <out-dir>
"""
import json
import os
import sys
from collections import defaultdict
from multiprocessing import Pool

import numpy as np
import pandas as pd

import combatdata
import units as unitmod

WEAPONS = ["hoverarty_ata", "cloaksnipe_shockrifle", "striderantiheavy_shockrifle", "shieldarty_hammer_weapon", "tankarty_corearty_weapon",
           "cloakarty_hammer_weapon", "tankheavyarty_plasma", "staticarty_plasma", "staticheavyarty_plasma", "amphsupport_cannon",
           "spiderskirm_adv_rocket", "hoverskirm_missile", "vehsupport_cortruck_missile", "shieldskirm_storm_rocket", "cloakskirm_bot_rocket",
           "jumparty_napalm_sprayer", "veharty_mine", "turretheavylaser_laser", "turretantiheavy_atak", "tankassault_cor_reap", "hoverassault_dew",
           "jumpskirm_slowbeam", "striderdetriment_arm_detriment", "turretmissile_armrl_missile", "amphimpulse_sonic"]
GATE = ["hoverarty_ata", "cloaksnipe_shockrifle"]
SIGHT = {}
UD = None


def _init():
    global UD
    UD = unitmod.load()
    df = pd.read_csv(os.path.join(unitmod.RUNS, "predictor", "unitdefs.tsv"), sep="\t", dtype=str, keep_default_na=False, quoting=3)
    for _, r in df.iterrows():
        try:
            SIGHT[r["name"]] = float(r["sight"])
        except ValueError:
            pass


def journal_snaps(match_dir):
    """team -> sorted list of (frame, {enemy id: flags})."""
    out = {}
    jd = os.path.join(match_dir, "journal")
    if not os.path.isdir(jd):
        return out
    for fn in os.listdir(jd):
        if not fn.endswith(".jsonl"):
            continue
        team = None
        snaps = []
        with open(os.path.join(jd, fn)) as fh:
            for line in fh:
                if '"k":"run"' in line[:40]:
                    try:
                        team = json.loads(line).get("team")
                    except ValueError:
                        pass
                elif '"k":"snap"' in line[:40]:
                    try:
                        d = json.loads(line)
                    except ValueError:
                        continue
                    snaps.append((d["frame"], {e[0]: e[5] for e in d.get("enemies", [])}))
        if team is not None and snaps:
            snaps.sort()
            out[team] = snaps
    return out


def game(path):
    if UD is None:
        _init()
    d = combatdata.load_cached(path)
    s = d["shot"]
    s = s[(s["tt"] == "u") & (s["tid"] >= 0) & (s["tteam"] != s["oteam"]) & s["wd"].astype(str).isin(WEAPONS)]
    if not len(s):
        return None
    game_id = d["meta"].get("game", "")
    snaps = journal_snaps(os.path.dirname(d["meta"]["path"]))
    snap_frames = {t: np.array([f for f, _ in v]) for t, v in snaps.items()}
    u = d["unit"]
    uf = u["f"].values; order = np.argsort(uf, kind="stable")
    uf = uf[order]; ux = u["x"].values[order]; uz = u["z"].values[order]; ut = u["uteam"].values[order]; uid = u["uid"].values[order]
    usight = np.array([SIGHT.get(n, 0.0) for n in u["udef"].astype(str).values])[order] if len(u) else np.zeros(0)
    wd = s["wd"].astype(str).values; odef = s["odef"].astype(str).values; tdef = s["tdef"].astype(str).values
    f0 = s["f0"].values; own = s["own"].values; oteam = s["oteam"].values; tid = s["tid"].values
    tx = s["tx"].values; tz = s["tz"].values; tdist = s["tdist"].values; n = s["n"].values
    dmgt = s["dmgt"].values; dmg = s["dmg"].values
    rows = []
    for i in range(len(s)):
        dist = float(tdist[i])
        if np.isnan(dist):
            track = "nodist"
        elif dist <= SIGHT.get(odef[i], 0.0):
            track = "self"
        else:
            lo = np.searchsorted(uf, f0[i] - 30); hi = np.searchsorted(uf, f0[i] + 31)
            m = (ut[lo:hi] == oteam[i]) & (uid[lo:hi] != own[i])
            if m.any():
                dx = ux[lo:hi][m] - tx[i]; dz = uz[lo:hi][m] - tz[i]
                track = "friend" if np.any(dx * dx + dz * dz <= usight[lo:hi][m] ** 2) else "none"
            else:
                track = "none"
        jl = "nojournal"
        if int(oteam[i]) in snaps:
            fr = snap_frames[int(oteam[i])]
            k = int(np.clip(np.searchsorted(fr, f0[i]), 0, len(fr) - 1))
            if k > 0 and abs(fr[k - 1] - f0[i]) < abs(fr[k] - f0[i]):
                k -= 1
            flags = snaps[int(oteam[i])][k][1].get(int(tid[i]))
            if flags is None:
                jl = "absent"
            elif flags & 1:
                jl = "vis"
            elif flags & 2:
                jl = "radar"
            else:
                jl = "neither"
        td = UD.get(tdef[i])
        cls = "static" if (td is not None and td.static) else "mobile"
        rows.append((game_id, wd[i], cls, track, jl, int(n[i]), float(dmgt[i] > 0) * int(n[i]), float(dmg[i]), dist if not np.isnan(dist) else 0.0))
    return rows


def boot_gap(df, col, a, b, reps=2000, seed=11):
    """Bootstrap by game of p_hit(a) - p_hit(b) for one proxy column."""
    if len(df) == 0:
        return None
    g = df.groupby(["game", col])[["n", "hit"]].sum().unstack(fill_value=0)
    if "n" not in g.columns.get_level_values(0) or (a not in g["n"].columns) or (b not in g["n"].columns):
        return None
    na, ha, nb, hb = g["n"][a].values, g["hit"][a].values, g["n"][b].values, g["hit"][b].values
    rng = np.random.default_rng(seed)
    idx = rng.integers(0, len(na), size=(reps, len(na)))
    pa = ha[idx].sum(1) / np.maximum(na[idx].sum(1), 1); pb = hb[idx].sum(1) / np.maximum(nb[idx].sum(1), 1)
    diff = pa - pb
    return {"games": len(na), "n_a": int(na.sum()), "n_b": int(nb.sum()), "p_a": ha.sum() / max(na.sum(), 1), "p_b": hb.sum() / max(nb.sum(), 1),
            "gap": ha.sum() / max(na.sum(), 1) - hb.sum() / max(nb.sum(), 1), "lo": float(np.percentile(diff, 2.5)), "hi": float(np.percentile(diff, 97.5))}


def main(cache, out):
    os.makedirs(out, exist_ok=True)
    pk = os.path.join(out, "los_shots.pkl")
    if os.environ.get("LOS_REUSE") == "1" and os.path.exists(pk):
        df = pd.read_pickle(pk)
    else:
        paths = list(combatdata.cached(cache))
        rows = []
        with Pool(4, initializer=_init) as pool:
            for r in pool.imap_unordered(game, paths, chunksize=8):
                if r:
                    rows.extend(r)
        df = pd.DataFrame(rows, columns=["game", "wd", "cls", "track", "journal", "n", "hit", "dmg", "dist"])
        df.to_pickle(pk)
    lines = ["# Radar vs LOS read (existing logs, no tlos column: two proxies, see los_read.py)\n"]
    def table(keys, title, min_n=100):
        g = df.groupby(keys).agg(shots=("n", "sum"), hits=("hit", "sum"), dmg=("dmg", "sum"), dist=("dist", "mean")).reset_index()
        g = g[g["shots"] >= min_n]
        g["p_hit"] = g["hits"] / g["shots"]; g["dmg_per_shot"] = g["dmg"] / g["shots"]
        lines.append(f"\n## {title}\n")
        lines.append(g.drop(columns=["hits", "dmg"]).to_string(index=False, float_format=lambda v: f"{v:.3f}"))
        lines.append("")
    table(["wd", "journal"], "by weapon x journal visibility (ZKGBAI shooters only; vis = in sight, radar = radar only, absent = not in the AI's enemy list)")
    table(["wd", "cls", "journal"], "by weapon x target class x journal visibility")
    table(["wd", "track"], "by weapon x track proxy (self = within own sight, friend = a logged friendly unit sees it, none = no logged friendly sees it)")
    table(["wd", "cls", "track"], "by weapon x target class x track proxy")
    lines.append("\n## Gate: p(hit | LOS) - p(hit | radar only), bootstrap by game (2000 resamples)\n")
    recs = []
    for wd in GATE + ["striderantiheavy_shockrifle", "shieldarty_hammer_weapon", "tankarty_corearty_weapon", "cloakarty_hammer_weapon"]:
        for cls in ("all", "static", "mobile"):
            d = df[(df["wd"] == wd) & ((df["cls"] == cls) if cls != "all" else True)]
            for col, a, b in (("journal", "vis", "radar"), ("track", "friend", "none")):
                r = boot_gap(d, col, a, b)
                if r and r["n_a"] >= 50 and r["n_b"] >= 50:
                    recs.append({"weapon": wd, "targets": cls, "proxy": f"{col}: {a} vs {b}", **r})
    gate = pd.DataFrame(recs)
    gate.to_csv(os.path.join(out, "los_gate.tsv"), sep="\t", index=False, float_format="%.3f")
    lines.append(gate.to_string(index=False, float_format=lambda v: f"{v:.3f}"))
    open(os.path.join(out, "los_read.md"), "w").write("\n".join(lines) + "\n")
    print("\n".join(lines[-2:]))


if __name__ == "__main__":
    main(sys.argv[1], sys.argv[2])
