#!/usr/bin/env python3
"""Fight outcome predictors scored on held-out fights (protocol: zkgbai-runs/predictor/SUMMARY.md).

usage: predict.py <predictor-dir> [workers] [rollouts]
Reads fights.pkl, hitcells.pkl, unitdefs.tsv and the cache; writes
fights_pred.tsv, scores.tsv and results.md in <predictor-dir>.
"""
import hashlib
import math
import os
import pickle
import sys
from collections import Counter, defaultdict
from multiprocessing import Pool

import numpy as np
import pandas as pd
from scipy.optimize import minimize
from scipy.special import betainc, expit, gammaln, logit
from scipy.stats import beta as beta_dist

import combatdata
import disengage
import split
import units as unitmod
from fit_hits import SPEED_EDGES, RBANDS, SHRINK

DT = 3              # frames per sim step
HORIZON = 2700      # frames (90 s)
HITSCAN = {"BeamLaser", "LightningCannon", "Rifle"}
MIN_COST = 100.0
DECIDE = 0.1

G = {}  # per-process globals: ud, hits, weapons, comm weapons


# ----------------------------------------------------------------------------- hit model
class HitModel:
    def __init__(self, cells, ud):
        c = cells.reset_index()
        c["cls"] = [ud.get(t).size_class() if ud.get(t) else "medium" for t in c["tdef"]]
        cols = ["n", "hit_any", "hit_tgt", "dmg", "nominal"]
        self.wd = c.groupby("wd")[cols].sum().to_dict("index")
        self.wd_cls = c.groupby(["wd", "cls"])[cols].sum().to_dict("index")
        self.wd_tdef = c.groupby(["wd", "tdef"])[cols].sum().to_dict("index")
        self.wd_tdef_rb = c.groupby(["wd", "tdef", "rb"])[cols].sum().to_dict("index")
        self.cells = c.groupby(["wd", "tdef", "rb", "sb"])[cols].sum().to_dict("index")
        self.ud = ud
        self.cache = {}

    @staticmethod
    def _rate(row, prior, k=SHRINK):
        n = row["n"]
        return (row["hit_any"] + k * prior) / (n + k), (row["hit_tgt"] + k * prior) / (n + k)

    @staticmethod
    def _frac(row):
        if row["hit_any"] > 0 and row["nominal"] > 0 and row["n"] > 0:
            return row["dmg"] / row["hit_any"] / (row["nominal"] / row["n"])
        return None

    def lookup(self, wd, tdef, rb, sb):
        """-> (p_hit_any, p_hit_target, damage per hit as share of nominal per shot, shots seen)."""
        key = (wd, tdef, rb, sb)
        r = self.cache.get(key)
        if r is not None:
            return r
        p_any = p_tgt = 0.7
        frac = 1.0
        n = 0.0
        row = self.wd.get(wd)
        if row is not None:
            n = row["n"]
            p_any, p_tgt = self._rate(row, 0.7)
            frac = self._frac(row) or 1.0
            d = self.ud.get(tdef)
            cls = d.size_class() if d is not None else "medium"
            row = self.wd_cls.get((wd, cls))
            if row is not None:
                p_any, p_tgt = self._rate(row, p_any)
            row = self.wd_tdef.get((wd, tdef))
            if row is not None:
                p_any, p_tgt = self._rate(row, p_any)
                if row["hit_any"] > 5:
                    frac = self._frac(row) or frac
                row = self.wd_tdef_rb.get((wd, tdef, rb))
                if row is not None:
                    p_any, p_tgt = self._rate(row, p_any)
                    row = self.cells.get((wd, tdef, rb, sb))
                    if row is not None:
                        p_any, p_tgt = self._rate(row, p_any)
        r = (min(float(p_any), 1.0), min(float(p_tgt), 1.0), float(frac), float(n))
        self.cache[key] = r
        return r


def sband(spd):
    return int(np.searchsorted(np.array(SPEED_EDGES), spd, side="right"))


# ----------------------------------------------------------------------------- setup
def weapon_table(cache, n=30):
    tabs = []
    for i, p in enumerate(combatdata.cached(cache)):
        if i >= n:
            break
        w = combatdata.load_cached(p)["meta"]["weapons"]
        if w is not None:
            tabs.append(w)
    return pd.concat(tabs)[~pd.concat(tabs).index.duplicated()]


def comm_weapons(cache, ud, n=150):
    """Weapons actually fired by unit defs without a static weapon list (commanders)."""
    counts = defaultdict(Counter)
    for i, p in enumerate(combatdata.cached(cache)):
        if i >= n:
            break
        d = combatdata.load_cached(p)
        s = d["shot"]
        if split.split_of(d["meta"].get("game", ""), os.path.basename(p)[:-4]) != "train":
            continue
        g = s.groupby([s["odef"].astype(str), s["wd"].astype(str)]).size()
        for (od, wd), k in g.items():
            counts[od][wd] += int(k)
    out = {}
    for od, c in counts.items():
        dd = ud.get(od)
        if dd is not None and dd.weapons and not dd.is_com:
            continue
        tot = sum(c.values())
        out[od] = [wd for wd, k in c.most_common(3) if k >= 0.1 * tot and "ex" != wd[-2:]]
    return out


class SimWeapon:
    __slots__ = ("name", "range", "reload", "hitscan", "salvo", "nominal", "next")

    def __init__(self, name, wrow):
        self.name = name
        self.range = float(wrow["range"])
        self.reload = max(int(round(float(wrow["reload"]) * 30)), 1)
        self.hitscan = wrow["type"] in HITSCAN
        self.salvo = max(int(wrow["salvo"]), 1)
        dmg = float(wrow["damage"])
        # nominal per rolled shot: hitscan = one shot of the whole salvo, projectile = one projectile
        self.nominal = dmg * self.salvo if self.hitscan else dmg
        self.next = 0


class SimUnit:
    __slots__ = ("side", "def_", "hp", "cost", "armed", "mobile", "speed", "x", "weapons", "max_range", "alive", "moving")

    def __init__(self, side, rec, x, ud, wtab, commw):
        self.side = side
        self.def_ = rec["def"]
        self.hp = float(rec["hp"])
        self.cost = rec["cost"]
        self.armed = rec["armed"]
        d = ud.get(rec["def"], rec["hpmax"])
        self.mobile = d is not None and d.canmove
        self.speed = d.speed if d is not None else 0.0
        self.x = x
        self.weapons = []
        names = commw.get(rec["def"]) or ([w.name for w in d.weapons] if d is not None else [])
        for nm in names:
            if nm in wtab.index:
                w = wtab.loc[nm]
                if float(w["damage"]) > 0 and float(w["range"]) > 0 and w["type"] != "Shield":
                    self.weapons.append(SimWeapon(nm, w))
        if self.weapons:
            self.armed = True
        self.max_range = max((w.range for w in self.weapons), default=0.0)
        self.alive = True
        self.moving = False


def project(fight):
    """1D positions: each unit's position along the axis from A's centroid to B's."""
    A, B = fight["A"], fight["B"]
    def cent(side):
        xs = [(r["x"], r["z"]) for r in side if not math.isnan(r["x"])]
        return (sum(x for x, _ in xs) / len(xs), sum(z for _, z in xs) / len(xs)) if xs else None
    ca, cb = cent(A), cent(B)
    d0 = fight["d0"] if not math.isnan(fight["d0"]) else 400.0
    if ca is None or cb is None or d0 < 1:
        return [0.0] * len(A), [d0] * len(B)
    ax, az = (cb[0] - ca[0]) / d0, (cb[1] - ca[1]) / d0
    def pos(r, default):
        if math.isnan(r["x"]):
            return default
        return (r["x"] - ca[0]) * ax + (r["z"] - ca[1]) * az
    return [pos(r, 0.0) for r in A], [pos(r, d0) for r in B]


def build_units(fight):
    ud, wtab, commw = G["ud"], G["wtab"], G["commw"]
    pa, pb = project(fight)
    units = [SimUnit(0, r, x, ud, wtab, commw) for r, x in zip(fight["A"], pa)]
    units += [SimUnit(1, r, x, ud, wtab, commw) for r, x in zip(fight["B"], pb)]
    return units


# ----------------------------------------------------------------------------- models
def flat_threat(fight):
    ud = G["ud"]
    S = [0.0, 0.0]
    for i, side in enumerate((fight["A"], fight["B"])):
        for r in side:
            d = ud.get(r["def"], r["hpmax"])
            if d is not None:
                S[i] += d.danger(r["hp"])
    return math.log((S[0] + 1.0) / (S[1] + 1.0))


def unit_dps_vs(u, enemies):
    hits = G["hits"]
    tot = 0.0
    for w in u.weapons:
        acc = 0.0
        for e in enemies:
            p_any, p_tgt, frac, _ = hits.lookup(w.name, e.def_, 2, sband(e.speed if e.mobile else 0.0))
            per_shot = p_tgt * frac * w.nominal * (1 if w.hitscan else w.salvo)
            acc += per_shot
        if enemies:
            tot += acc / len(enemies) / (w.reload / 30.0)
    return tot


def lanchester(units):
    sides = [[u for u in units if u.side == s] for s in (0, 1)]
    S = []
    for s in (0, 1):
        own, foe = sides[s], sides[1 - s]
        dps = sum(unit_dps_vs(u, foe) for u in own if u.armed)
        hp = sum(u.hp for u in own if u.armed)
        S.append(dps * hp)
    if S[0] <= 0 and S[1] <= 0:
        return 0.5, 0.5
    if S[0] >= S[1]:
        return 1.0 - math.sqrt(max(0.0, 1.0 - S[1] / S[0])) if S[0] > 0 else 1.0, 1.0
    return 1.0, 1.0 - math.sqrt(max(0.0, 1.0 - S[0] / S[1]))


def simulate(units, rng=None, hazard=None):
    """1D a-move fight. rng None -> expected damage (deterministic); else rolled shots.
    hazard: disengagement hazard applied at the end of every second (expected over stopping
    times when deterministic, sampled when rolled)."""
    hits = G["hits"]
    us = [SimUnit.__new__(SimUnit) for _ in units]
    for a, b in zip(us, units):
        for k in SimUnit.__slots__:
            setattr(a, k, getattr(b, k))
        a.weapons = [SimWeapon.__new__(SimWeapon) for _ in b.weapons]
        for wa, wb in zip(a.weapons, b.weapons):
            for k in SimWeapon.__slots__:
                setattr(wa, k, getattr(wb, k))
            wa.next = 0
    cost0 = [sum(u.cost for u in us if u.side == s and u.armed) for s in (0, 1)]
    killed = [0.0, 0.0]
    quiet = 0
    f = 0
    lr = math.log(max(cost0[0], 1.0) / max(cost0[1], 1.0))
    surv = 1.0
    exp_k = [0.0, 0.0]
    def kfrac():
        return (killed[0] / cost0[0] if cost0[0] > 0 else 0.0, killed[1] / cost0[1] if cost0[1] > 0 else 0.0)
    while f < HORIZON:
        if hazard is not None and f > 0 and f % 30 == 0:
            kA, kB = kfrac()
            h = hazard.h(f // 30 - 1, kA, kB, lr)
            if rng is None:
                exp_k[0] += surv * h * kA; exp_k[1] += surv * h * kB
                surv *= 1.0 - h
            elif rng.random() < h:
                return kA, kB
        alive = [u for u in us if u.alive]
        sides = [[u for u in alive if u.side == s] for s in (0, 1)]
        if not any(u.armed for u in sides[0]) or not any(u.armed for u in sides[1]):
            break
        shot_fired = False
        moved = False
        # movement: close until an enemy is within own range
        for u in alive:
            u.moving = False
            if not u.mobile or not u.armed or u.speed <= 0:
                continue
            foes = sides[1 - u.side]
            if not foes:
                continue
            nearest = min(foes, key=lambda e: abs(e.x - u.x))
            dist = abs(nearest.x - u.x)
            if dist > u.max_range * 0.95:
                step = min(u.speed * DT, dist - u.max_range * 0.9)
                u.x += step if nearest.x > u.x else -step
                u.moving = True
                moved = True
        # fire
        for u in alive:
            if not u.armed or not u.alive:
                continue
            foes = [e for e in sides[1 - u.side] if e.alive]
            if not foes:
                continue
            for w in u.weapons:
                if f < w.next:
                    continue
                inr = [(abs(e.x - u.x), e) for e in foes if abs(e.x - u.x) <= w.range and e.alive]
                if not inr:
                    continue
                w.next = f + w.reload
                shot_fired = True
                if rng is None:
                    dist, tgt = min(inr, key=lambda t: t[0])
                else:
                    dist, tgt = inr[rng.integers(len(inr))]
                rb = min(int(RBANDS * dist / w.range), RBANDS) if w.range > 0 else RBANDS
                sb = sband(tgt.speed if tgt.moving else 0.0)
                p_any, p_tgt, frac, _ = hits.lookup(w.name, tgt.def_, rb, sb)
                shots = 1 if w.hitscan else w.salvo
                if rng is None:
                    dmg = p_tgt * frac * w.nominal * shots
                else:
                    if w.hitscan:
                        # landed share of a hitscan salvo: Bernoulli on the shot, nominal*frac on hit
                        dmg = w.nominal * frac if rng.random() < p_tgt else 0.0
                    else:
                        k = rng.binomial(shots, p_tgt)
                        dmg = k * w.nominal * frac
                if dmg > 0:
                    tgt.hp -= dmg
                    if tgt.hp <= 0:
                        tgt.alive = False
                        if tgt.armed:
                            killed[tgt.side] += tgt.cost
        quiet = 0 if (shot_fired or moved) else quiet + DT
        if quiet > 300:
            break
        f += DT
    kA, kB = kfrac()
    if hazard is not None and rng is None:
        return exp_k[0] + surv * kA, exp_k[1] + surv * kB
    return kA, kB


def predict_fight(args):
    fight, rollouts = args
    units = build_units(fight)
    out = {"flat_r": flat_threat(fight)}
    out["lan_A"], out["lan_B"] = lanchester(units)
    out["dps_A"], out["dps_B"] = simulate(units)
    seed = int(hashlib.md5(f"{fight['game']}:{fight['fid']}".encode()).hexdigest(), 16) % (2 ** 32)
    rng = np.random.default_rng(seed)
    ys = np.array([simulate(units, rng) for _ in range(rollouts)])
    out["roll_A"] = ys[:, 0]
    out["roll_B"] = ys[:, 1]
    hz = G.get("hazard")
    if hz is not None:
        out["dstop_A"], out["dstop_B"] = simulate(units, None, hz)
        rng = np.random.default_rng(seed + 1)
        ys = np.array([simulate(units, rng, hz) for _ in range(rollouts)])
        out["rstop_A"] = ys[:, 0]
        out["rstop_B"] = ys[:, 1]
    out["n_armedA"] = sum(u.armed for u in units if u.side == 0)
    out["n_armedB"] = sum(u.armed for u in units if u.side == 1)
    return out


def _init(pdir, cache):
    G["ud"] = unitmod.load(os.path.join(pdir, "unitdefs.tsv"))
    cells = pickle.load(open(os.path.join(pdir, "hitcells.pkl"), "rb"))["train"]
    G["hits"] = HitModel(cells, G["ud"])
    G["wtab"] = pickle.load(open(os.path.join(pdir, "wtab.pkl"), "rb"))
    G["commw"] = pickle.load(open(os.path.join(pdir, "commw.pkl"), "rb"))
    hp = os.environ.get("PRED_HAZARD")
    if hp:
        hd = pickle.load(open(hp, "rb"))
        disengage.T0 = hd["t0"]
        G["hazard"] = disengage.Hazard(hd["w"])
    else:
        G["hazard"] = None


# ----------------------------------------------------------------------------- calibration + scoring
class ZOIBeta:
    """Zero-one-inflated Beta regression of y in [0,1] on one feature z."""

    def __init__(self):
        self.theta = np.array([-1.0, 0.0, -1.0, 0.0, 0.0, 0.0, math.log(5.0)])

    @staticmethod
    def _parts(theta, z):
        p0 = expit(theta[0] + theta[1] * z)
        p1 = expit(theta[2] + theta[3] * z)   # P(y=1 | y>0)
        mu = np.clip(expit(theta[4] + theta[5] * z), 1e-4, 1 - 1e-4)
        phi = math.exp(theta[6])
        return p0, p1, mu, phi

    def nll(self, theta, z, y):
        p0, p1, mu, phi = self._parts(theta, z)
        a, b = mu * phi, (1 - mu) * phi
        ll = np.where(y <= 0, np.log(p0 + 1e-12), 0.0)
        ll = np.where(y >= 1, np.log(1 - p0 + 1e-12) + np.log(p1 + 1e-12), ll)
        inner = (y > 0) & (y < 1)
        yi = np.clip(y, 1e-4, 1 - 1e-4)
        lb = gammaln(a + b) - gammaln(a) - gammaln(b) + (a - 1) * np.log(yi) + (b - 1) * np.log(1 - yi)
        ll = np.where(inner, np.log(1 - p0 + 1e-12) + np.log(1 - p1 + 1e-12) + lb, ll)
        return -ll.sum() + 1e-3 * np.sum(theta[:6] ** 2)

    def fit(self, z, y):
        z = np.asarray(z, float); y = np.asarray(y, float)
        res = minimize(self.nll, self.theta, args=(z, y), method="L-BFGS-B")
        self.theta = res.x
        return self

    def crps(self, z, y, grid=np.linspace(0, 1, 401)):
        p0, p1, mu, phi = self._parts(self.theta, np.asarray(z, float))
        a, b = mu * phi, (1 - mu) * phi
        out = np.empty(len(y))
        for i in range(len(y)):
            F = p0[i] + (1 - p0[i] - (1 - p0[i]) * p1[i]) * betainc(a[i], b[i], grid)
            F[-1] = 1.0
            H = (grid >= y[i]).astype(float)
            out[i] = np.trapezoid((F - H) ** 2, grid)
        return out

    def mean(self, z):
        p0, p1, mu, phi = self._parts(self.theta, np.asarray(z, float))
        return (1 - p0) * (p1 + (1 - p1) * mu)


class Logit1:
    def __init__(self):
        self.w = np.array([0.0, 0.0])

    def fit(self, z, y):
        z = np.asarray(z, float); y = np.asarray(y, float)
        def nll(w):
            p = np.clip(expit(w[0] + w[1] * z), 1e-6, 1 - 1e-6)
            return -(y * np.log(p) + (1 - y) * np.log(1 - p)).sum() + 1e-3 * w[1] ** 2
        self.w = minimize(nll, self.w, method="L-BFGS-B").x
        return self

    def prob(self, z):
        return expit(self.w[0] + self.w[1] * np.asarray(z, float))


def crps_ensemble(samples, y):
    s = np.sort(samples)
    n = len(s)
    term1 = np.mean(np.abs(s - y))
    # E|X-X'| = 2/n^2 * sum_i (2i-n-1) x_(i)
    i = np.arange(1, n + 1)
    term2 = 2.0 / (n * n) * np.sum((2 * i - n - 1) * s)
    return term1 - 0.5 * term2


def evaluate(pdir, workers=4, rollouts=64):
    cache = os.path.join(pdir, "cache")
    fights = pickle.load(open(os.environ.get("PRED_FIGHTS", os.path.join(pdir, "fights.pkl")), "rb"))
    out = os.environ.get("PRED_OUT", pdir)
    os.makedirs(out, exist_ok=True)
    ud = unitmod.load(os.path.join(pdir, "unitdefs.tsv"))
    if not os.path.exists(os.path.join(pdir, "wtab.pkl")):
        pickle.dump(weapon_table(cache), open(os.path.join(pdir, "wtab.pkl"), "wb"))
    if not os.path.exists(os.path.join(pdir, "commw.pkl")):
        pickle.dump(comm_weapons(cache, ud), open(os.path.join(pdir, "commw.pkl"), "wb"))
    sel = [f for f in fights if f["closed"] and f["costA"] >= MIN_COST and f["costB"] >= MIN_COST
           and not math.isnan(f["yA"]) and not math.isnan(f["yB"])]
    for f in sel:
        f["split"] = split.split_of(f["game"], f["series"])
    print(f"{len(sel)} evaluation fights:", Counter(f["split"] for f in sel), flush=True)
    with Pool(workers, initializer=_init, initargs=(pdir, cache)) as pool:
        preds = []
        for i, p in enumerate(pool.imap(predict_fight, [(f, rollouts) for f in sel], chunksize=16)):
            preds.append(p)
            if (i + 1) % 1000 == 0:
                print(f"predicted {i + 1}/{len(sel)}", flush=True)
    rows = []
    for f, p in zip(sel, preds):
        rows.append({
            "game": f["game"], "fid": f["fid"], "series": f["series"], "split": f["split"], "map": f["map"],
            "start": f["start"], "dur": f["end"] - f["start"], "d0": round(f["d0"], 0) if not math.isnan(f["d0"]) else "",
            "ai_is_A": int(f["ai_is_A"]),
            "compA": " ".join(f"{k}x{v}" for k, v in Counter(r["def"] for r in f["A"]).items()),
            "compB": " ".join(f"{k}x{v}" for k, v in Counter(r["def"] for r in f["B"]).items()),
            "costA": f["costA"], "costB": f["costB"], "nA": p["n_armedA"], "nB": p["n_armedB"],
            "static": int(any(r["static"] for r in f["A"] + f["B"])), "com": int(any(r["com"] for r in f["A"] + f["B"])),
            "yA": f["yA"], "yB": f["yB"],
            "flat_r": p["flat_r"], "lan_A": p["lan_A"], "lan_B": p["lan_B"], "dps_A": p["dps_A"], "dps_B": p["dps_B"],
            "roll_A": p["roll_A"].mean(), "roll_B": p["roll_B"].mean(),
            "roll_pAwin": (np.sum(p["roll_B"] - p["roll_A"] > DECIDE) + 0.5) / (np.sum(np.abs(p["roll_B"] - p["roll_A"]) > DECIDE) + 1.0),
            "roll_crpsA": crps_ensemble(p["roll_A"], f["yA"]), "roll_crpsB": crps_ensemble(p["roll_B"], f["yB"]),
        })
        if "dstop_A" in p:
            rows[-1].update({
                "dstop_A": p["dstop_A"], "dstop_B": p["dstop_B"],
                "rstop_A": p["rstop_A"].mean(), "rstop_B": p["rstop_B"].mean(),
                "rstop_pAwin": (np.sum(p["rstop_B"] - p["rstop_A"] > DECIDE) + 0.5) / (np.sum(np.abs(p["rstop_B"] - p["rstop_A"]) > DECIDE) + 1.0),
                "rstop_crpsA": crps_ensemble(p["rstop_A"], f["yA"]), "rstop_crpsB": crps_ensemble(p["rstop_B"], f["yB"]),
            })
    df = pd.DataFrame(rows)
    df["win"] = np.where(df["yB"] - df["yA"] > DECIDE, 1, np.where(df["yA"] - df["yB"] > DECIDE, 0, -1))
    train = df[df["split"] == "train"]
    # features per model
    feats = {
        "climatology": (np.zeros(len(df)), np.zeros(len(df)), np.zeros(len(df))),
        "flat": (-df["flat_r"].values, df["flat_r"].values, df["flat_r"].values),
        "lanchester": (logit(np.clip(df["lan_A"], 0.005, 0.995)), logit(np.clip(df["lan_B"], 0.005, 0.995)), df["lan_B"].values - df["lan_A"].values),
        "dps_sim": (logit(np.clip(df["dps_A"], 0.005, 0.995)), logit(np.clip(df["dps_B"], 0.005, 0.995)), df["dps_B"].values - df["dps_A"].values),
        "rolled_sim": (logit(np.clip(df["roll_A"], 0.005, 0.995)), logit(np.clip(df["roll_B"], 0.005, 0.995)), df["roll_B"].values - df["roll_A"].values),
    }
    if "dstop_A" in df.columns:
        feats["dps_stop"] = (logit(np.clip(df["dstop_A"], 0.005, 0.995)), logit(np.clip(df["dstop_B"], 0.005, 0.995)), df["dstop_B"].values - df["dstop_A"].values)
        feats["rolled_stop"] = (logit(np.clip(df["rstop_A"], 0.005, 0.995)), logit(np.clip(df["rstop_B"], 0.005, 0.995)), df["rstop_B"].values - df["rstop_A"].values)
    tr = (df["split"] == "train").values
    dec = (df["win"] >= 0).values
    for name, (zA, zB, zw) in feats.items():
        cA = ZOIBeta().fit(zA[tr], df["yA"].values[tr])
        cB = ZOIBeta().fit(zB[tr], df["yB"].values[tr])
        lw = Logit1().fit(zw[tr & dec], df["win"].values[tr & dec])
        df[f"{name}_mA"] = cA.mean(zA); df[f"{name}_mB"] = cB.mean(zB)
        df[f"{name}_crpsA"] = cA.crps(zA, df["yA"].values); df[f"{name}_crpsB"] = cB.crps(zB, df["yB"].values)
        df[f"{name}_pAwin"] = lw.prob(zw)
        print(name, "calibrated: thetaA", np.round(cA.theta, 2), "win w", np.round(lw.w, 2), flush=True)
    df.to_csv(os.path.join(out, "fights_pred.tsv"), sep="\t", index=False, float_format="%.4f")
    return df


RAW = {"rolled_sim": "roll", "rolled_stop": "rstop"}


def metric_arrays(d, mod):
    """Per-fight CRPS, log loss and Brier (nan where undecided) for a model, calibrated and raw."""
    out = {}
    w = d["win"].values.astype(float)
    dec = d["win"].values >= 0
    def ll_br(p):
        p = np.clip(p, 1e-6, 1 - 1e-6)
        ll = np.where(dec, -(w * np.log(p) + (1 - w) * np.log(1 - p)), np.nan)
        br = np.where(dec, (p - w) ** 2, np.nan)
        return ll, br
    out["crps"] = ((d[f"{mod}_crpsA"] + d[f"{mod}_crpsB"]) / 2).values
    out["logloss"], out["brier"] = ll_br(d[f"{mod}_pAwin"].values)
    if mod in RAW:
        r = RAW[mod]
        out["crps_raw"] = ((d[f"{r}_crpsA"] + d[f"{r}_crpsB"]) / 2).values
        out["logloss_raw"], out["brier_raw"] = ll_br(d[f"{r}_pAwin"].values)
    return out


def bootstrap(df, models, reps=1000, seed=7):
    """Paired-by-game bootstrap of every pairwise model difference, per metric."""
    rng = np.random.default_rng(seed)
    games, gidx = np.unique(df["game"].values, return_inverse=True)
    arrays = {m: metric_arrays(df, m) for m in models}
    draws = rng.integers(0, len(games), size=(reps, len(games)))
    weights = np.stack([np.bincount(dr, minlength=len(games)) for dr in draws]).astype(float)  # reps x games
    wf = weights[:, gidx]  # reps x fights
    rows = []
    def wmean(a):
        ok = ~np.isnan(a)
        return (wf[:, ok] * a[ok]).sum(1) / wf[:, ok].sum(1)
    cache = {(m, k): wmean(v) for m, arr in arrays.items() for k, v in arr.items()}
    for i, m1 in enumerate(models):
        for m2 in models[i + 1:]:
            for k in arrays[m1]:
                k2 = k if k in arrays[m2] else k.replace("_raw", "")
                a1, a2 = arrays[m1][k], arrays[m2][k2]
                point = np.nanmean(a1) - np.nanmean(a2)
                diff = cache[(m1, k)] - cache[(m2, k2)]
                lo, hi = np.percentile(diff, [2.5, 97.5])
                rows.append({"a": m1, "b": m2, "metric": k if k == k2 else f"{k} vs {k2}", "diff": point, "lo": lo, "hi": hi,
                             "resolved": int(lo > 0 or hi < 0)})
    return pd.DataFrame(rows)


def score(df, pdir):
    pdir = os.environ.get("PRED_OUT", pdir)
    models = ["climatology", "flat", "lanchester", "dps_sim", "rolled_sim"] + (["dps_stop", "rolled_stop"] if "dstop_A" in df.columns else [])
    subsets = {
        "all": np.ones(len(df), bool),
        "mobile-only": (df["static"] == 0) & (df["com"] == 0),
        "with static": df["static"] == 1,
        "with commander": df["com"] == 1,
        ">=3 armed/side": (df["nA"] >= 3) & (df["nB"] >= 3),
        ">=500 metal/side": (df["costA"] >= 500) & (df["costB"] >= 500),
    }
    out = []
    for sp in ("test", "human", "train"):
        for sub, m in subsets.items():
            mask = (df["split"] == sp).values & np.asarray(m)
            d = df[mask]
            dd = d[d["win"] >= 0]
            if len(d) == 0:
                continue
            for mod in models:
                crps = float((d[f"{mod}_crpsA"] + d[f"{mod}_crpsB"]).mean() / 2)
                p = np.clip(dd[f"{mod}_pAwin"].values, 1e-6, 1 - 1e-6); w = dd["win"].values
                ll = float(-(w * np.log(p) + (1 - w) * np.log(1 - p)).mean()) if len(dd) else float("nan")
                br = float(((p - w) ** 2).mean()) if len(dd) else float("nan")
                row = {"split": sp, "subset": sub, "model": mod, "n": len(d), "n_decided": len(dd), "crps": crps, "logloss": ll, "brier": br}
                if mod in RAW:
                    r = RAW[mod]
                    row["crps_raw"] = float((d[f"{r}_crpsA"] + d[f"{r}_crpsB"]).mean() / 2)
                    p = np.clip(dd[f"{r}_pAwin"].values, 1e-6, 1 - 1e-6)
                    row["logloss_raw"] = float(-(w * np.log(p) + (1 - w) * np.log(1 - p)).mean()) if len(dd) else float("nan")
                    row["brier_raw"] = float(((p - w) ** 2).mean()) if len(dd) else float("nan")
                out.append(row)
    sc = pd.DataFrame(out)
    sc.to_csv(os.path.join(pdir, "scores.tsv"), sep="\t", index=False, float_format="%.4f")
    with open(os.path.join(pdir, "results.md"), "w") as fh:
        for sp in ("test", "human", "train"):
            s = sc[sc["split"] == sp]
            if len(s) == 0:
                continue
            fh.write(f"\n### {sp} fights\n\n")
            fh.write(s.drop(columns=["split"]).to_string(index=False, float_format=lambda v: f"{v:.4f}"))
            fh.write("\n")
    print(sc[sc["split"] == "test"].to_string(index=False, float_format=lambda v: f"{v:.4f}"))
    if os.environ.get("PRED_BOOT", "1") == "1":
        bs = []
        for sub in ("all", ">=3 armed/side", "mobile-only", "with static", ">=500 metal/side"):
            d = df[(df["split"] == "test").values & np.asarray(subsets[sub])]
            if len(d) < 20:
                continue
            b = bootstrap(d, models)
            b.insert(0, "subset", sub)
            bs.append(b)
        bs = pd.concat(bs)
        bs.to_csv(os.path.join(pdir, "bootstrap.tsv"), sep="\t", index=False, float_format="%.4f")
        print(bs[(bs["subset"] == ">=3 armed/side") & (bs["metric"].isin(["logloss", "brier", "crps"]))].to_string(index=False, float_format=lambda v: f"{v:.3f}"))
    return sc


if __name__ == "__main__":
    pdir = sys.argv[1]
    workers = int(sys.argv[2]) if len(sys.argv) > 2 else 4
    rollouts = int(sys.argv[3]) if len(sys.argv) > 3 else 64
    df = evaluate(pdir, workers, rollouts)
    score(df, pdir)
