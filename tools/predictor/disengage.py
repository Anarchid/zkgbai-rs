#!/usr/bin/env python3
"""Fit the disengagement hazard: P(fight ends in second t without a wipe | state at t).

usage: disengage.py <fights.pkl> <out hazard.pkl>
Train fights only (split.py). Person-period logistic regression.
"""
import math
import os
import pickle
import sys

import numpy as np
from scipy.optimize import minimize
from scipy.special import expit

import split

MIN_COST = 100.0


T0 = os.environ.get("HZ_T0") == "1"  # exploratory: indicator for the first second
POST = False  # round 3: one-hot observed posture of A and B
POSTURES = ("commit", "retreat-whole", "retreat-piece")


def features(t, kA, kB, logratio, post=None):
    f = [1.0, math.log1p(t), t / 10.0, max(kA, kB), min(kA, kB), abs(kA - kB), logratio]
    if T0:
        f.append(1.0 if t == 0 else 0.0)
    if POST and post is not None:
        pa, pb = post
        f.extend(1.0 if pa == q else 0.0 for q in POSTURES)
        f.extend(1.0 if pb == q else 0.0 for q in POSTURES)
    return np.array(f)


NAMES = ["const", "log1p(t)", "t/10", "kmax", "kmin", "|kA-kB|", "log(costA/costB)"] + (["t==0"] if T0 else [])
POST_NAMES = [f"A={q}" for q in POSTURES] + [f"B={q}" for q in POSTURES]


def kill_curve(side, cost):
    """Killed cost fraction as a function of seconds since fight start."""
    ks = sorted((r["kill_f"], r["cost"]) for r in side if r["armed"] and r["died"] and r["kill_f"] is not None)
    def at(t):
        return sum(c for f, c in ks if f <= 30 * t) / cost if cost > 0 else 0.0
    return at


def person_periods(fights, posture=None):
    X, y = [], []
    for f in fights:
        if not (f["closed"] and f["costA"] >= MIN_COST and f["costB"] >= MIN_COST):
            continue
        post = posture.get((f["game"], f["fid"])) if posture is not None else None
        if posture is not None and post is None:
            continue
        T = int((f["end"] - f["start"]) // 30)
        kA, kB = kill_curve(f["A"], f["costA"]), kill_curve(f["B"], f["costB"])
        lr = math.log(f["costA"] / f["costB"])
        wipe = f["yA"] >= 0.999 or f["yB"] >= 0.999
        for t in range(T + 1):
            X.append(features(t, kA(t), kB(t), lr, post))
            y.append(1.0 if (t == T and not wipe) else 0.0)
    return np.array(X), np.array(y)


class Hazard:
    def __init__(self, w=None):
        self.w = w

    def fit(self, X, y):
        def nll(w):
            p = np.clip(expit(X @ w), 1e-9, 1 - 1e-9)
            return -(y * np.log(p) + (1 - y) * np.log(1 - p)).sum() + 1e-3 * np.sum(w[1:] ** 2)
        self.w = minimize(nll, np.zeros(X.shape[1]), method="L-BFGS-B").x
        return self

    def h(self, t, kA, kB, logratio, post=None):
        return float(expit(features(t, kA, kB, logratio, post) @ self.w))


def load_posture(path):
    import pandas as pd
    d = pd.read_csv(path, sep="\t")
    return {(g, int(i)): (a, b) for g, i, a, b in zip(d["game"], d["fid"], d["postureA"], d["postureB"])}


def main(fights_path, out, posture_path=None):
    global POST
    fights = pickle.load(open(fights_path, "rb"))
    train = [f for f in fights if split.split_of(f["game"], f["series"]) == "train"]
    posture = None
    if posture_path:
        POST = True
        posture = load_posture(posture_path)
    X, y = person_periods(train, posture)
    hz = Hazard().fit(X, y)
    print(f"{len(train)} train fights, {len(X)} fight-seconds, {int(y.sum())} disengagements")
    for n, w in zip(NAMES + (POST_NAMES if POST else []), hz.w):
        print(f"  {n:18} {w:+.3f}")
    # check: observed vs predicted end rate by elapsed second
    p = expit(X @ hz.w)
    t = np.expm1(X[:, 1]).round().astype(int)
    print("  t   n     obs   pred")
    for tt in (0, 1, 2, 3, 5, 8, 12, 20, 30, 60):
        m = t == tt
        if m.sum() > 20:
            print(f"  {tt:3d} {m.sum():6d} {y[m].mean():.3f} {p[m].mean():.3f}")
    for lab, m in (("no kills yet", (X[:, 3] == 0)), ("kmax>0.5", X[:, 3] > 0.5)):
        print(f"  {lab:14} n={m.sum()} obs {y[m].mean():.3f} pred {p[m].mean():.3f}")
    pickle.dump({"w": hz.w, "t0": T0, "post": POST, "names": NAMES + (POST_NAMES if POST else [])}, open(out, "wb"))
    print("->", out)


if __name__ == "__main__":
    main(sys.argv[1], sys.argv[2], sys.argv[3] if len(sys.argv) > 3 else None)
