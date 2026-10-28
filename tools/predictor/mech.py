#!/usr/bin/env python3
"""dps_stop with mechanics (mechanics round 1, zkgbai-runs/predictor/mech1/PREDECLARATION.md).

simulate_mech(fight, opts, hazard) -> expected killed cost fractions (A, B), deterministic.
opts is a dict of switches: air, burn, death, pulse, emp, slow, engage, rate (blocking factor per
own unit in the line), desync (seconds), kite. With opts empty it is predict.simulate's
deterministic path exactly. Fitted parameters (fit_mech.py) live in M, loaded by load().
"""
import math
import pickle

import numpy as np

import disengage
import predict
from predict import DT, HORIZON, RBANDS, sband

M = {}
KAMIKAZE = {"cloakbomb", "shieldbomb", "jumpbomb", "amphbomb", "gunshipbomb", "subscout"}
STUN_DECAY_S = 40.0        # unit_paralysis_damage.lua: 100% para decays to 0 in 40 s
SLOW_MAX = 0.5             # timeslow_defs.lua
SLOW_DECAY = 0.04          # of health per second, after 0.5 s without a slow hit
SLOW_GRACE = 15            # frames
KITE_MARGIN = 50.0
ENGAGE_BANDS = (0.5, 1.0, 1.5, 2.5, 5.0)


def load(path):
    M.clear()
    M.update(pickle.load(open(path, "rb")))


def load_tacai(path):
    """ZK's Tactical Unit AI, resolved by tacai_dump.py: per unit def, whom it skirms and flees."""
    import json
    M["tacai"] = {n: dict(b, skirms=set(b["skirms"]), flees=set(b["flees"])) for n, b in json.load(open(path)).items()}


# ----------------------------------------------------------------------------- per-unit snapshot features
def line_blockers(rec, own, foes):
    """Own units in the line from rec to its nearest enemy (2-D), and own units within 100 elmos."""
    ud = predict.G["ud"]
    if not foes:
        return 0, 0, float("inf")
    fx = np.array([r["x"] for r in foes]); fz = np.array([r["z"] for r in foes])
    d = np.hypot(fx - rec["x"], fz - rec["z"])
    j = int(np.nanargmin(d)) if np.isfinite(d).any() else 0
    ex, ez = fx[j] - rec["x"], fz[j] - rec["z"]
    l2 = ex * ex + ez * ez
    nb = crowd = 0
    for o in own:
        if o is rec:
            continue
        ox, oz = o["x"] - rec["x"], o["z"] - rec["z"]
        if ox * ox + oz * oz < 100.0 ** 2:
            crowd += 1
        if l2 > 0:
            t = (ox * ex + oz * ez) / l2
            dd = ud.get(o["def"])
            rad = dd.radius if dd is not None else 12.0
            if 0.05 < t < 0.95 and abs(ox * ez - oz * ex) / math.sqrt(l2) < rad + 8.0:
                nb += 1
    return nb, crowd, float(d[j])


def engage_features(rec, max_range, own, foes):
    """Feature vector for P(the unit fires at all in the fight); order = M['engage_names']."""
    ud = predict.G["ud"]
    nb, crowd, dist = line_blockers(rec, own, foes)
    ratio = dist / max_range if max_range > 0 else 10.0
    band = [1.0 if ratio > e else 0.0 for e in ENGAGE_BANDS]  # cumulative steps
    dd = ud.get(rec["def"])
    return np.array([1.0, *band, 1.0 if max_range < 300 else 0.0, 1.0 if rec["static"] else 0.0,
                     1.0 if rec["com"] else 0.0, 1.0 if (dd is not None and dd.builder) else 0.0,
                     float(min(nb, 3)), math.log1p(crowd), math.log1p(len(foes))]), nb


ENGAGE_NAMES = ["const", "ratio>0.5", "ratio>1", "ratio>1.5", "ratio>2.5", "ratio>5", "short", "static",
                "com", "builder", "blockers", "log1p(crowd)", "log1p(foes)"]


# ----------------------------------------------------------------------------- sim
class X:
    """Per-unit mechanics state riding on a SimUnit."""
    __slots__ = ("maxhp", "air", "kamikaze", "death", "engage", "blockers", "para", "slowdmg", "slow_last",
                 "burns", "range_", "speed_")


def _setup(fight, opts):
    units = predict.build_units(fight)
    recs = list(fight["A"]) + list(fight["B"])
    ud = predict.G["ud"]
    xs = []
    for u, r in zip(units, recs):
        x = X()
        d = ud.get(r["def"])
        x.maxhp = float(r["hpmax"]) if r["hpmax"] else max(u.hp, 1.0)
        x.air = bool(d is not None and d.canfly)
        x.kamikaze = r["def"] in KAMIKAZE
        x.death = M.get("death", {}).get(r["def"]) if opts.get("death") else None
        x.engage = 1.0
        x.blockers = 0
        x.para = 0.0
        x.slowdmg = 0.0
        x.slow_last = -10 ** 9
        x.burns = []
        xs.append(x)
        if opts.get("emp") and d is not None:
            # pure EMP weapons (damage 0 in the weapon table) are dropped by SimUnit: add them back
            have = {w.name for w in u.weapons}
            for w in d.weapons:
                if w.name not in have and w.paralyzer and w.name in predict.G["wtab"].index:
                    row = predict.G["wtab"].loc[w.name]
                    if float(row["range"]) > 0:
                        sw = predict.SimWeapon(w.name, row)
                        sw.nominal = 0.0
                        u.weapons.append(sw)
                        u.max_range = max(u.max_range, sw.range)
                        u.armed = True
    if opts.get("engage") or opts.get("rate"):
        w = M.get("engage_w")
        for i, (u, r, x) in enumerate(zip(units, recs, xs)):
            own = fight["A"] if i < len(fight["A"]) else fight["B"]
            foes = fight["B"] if i < len(fight["A"]) else fight["A"]
            if not u.armed or u.max_range <= 0:
                continue
            feats, nb = engage_features(r, u.max_range, own, foes)
            x.blockers = nb
            if opts.get("engage") and w is not None:
                x.engage = float(1.0 / (1.0 + math.exp(-float(feats @ w))))
    return units, xs


def _weapon_info(w):
    """(can_air, can_ground, burn per hit, burn length, emp per shot, slow factor, pulse)."""
    cache = M.setdefault("_winfo", {})
    r = cache.get(w.name)
    if r is not None:
        return r
    ud_w = M.get("udw", {}).get(w.name)
    air = M.get("air", {}).get(w.name, (True, True))
    burn = M.get("burn", {}).get(w.name, (0.0, 0))
    emp = 0.0
    slow = 0.0
    if ud_w is not None:
        if ud_w["paralyzer"]:
            emp = ud_w["damage"]
        elif ud_w["extra_damage"] > 0:
            emp = ud_w["extra_damage"]
        slow = ud_w["slow"]
    pulse = M.get("pulse", {}).get(w.name)
    r = (air[0], air[1], burn[0], burn[1], emp, slow, pulse)
    cache[w.name] = r
    return r


def simulate_mech(fight, opts, hazard=None, trace=None, withdraw=False, posture=None):
    """trace: a dict to fill with per-unit fates in build_units order (A's records, then B's):
    pdead = probability the unit is dead when the fight stops, ehp = expected HP then (with a hazard;
    without one, the end state). withdraw: the "A withdraws now" query (predict.simulate's): every mobile
    A unit reverses away from B at full speed. posture: (A's, B's) each "push" (base movement), "hold"
    (no advancing; skirming still backs off) or "withdraw" (reverse at own speed; statics cannot); an
    arm runs without the stopping rule and ends 10 s after the last shot or approach (5 s with a
    withdrawal). Hazards fitted with mobility features (hazmob) get disengage.mob_feats(fight)."""
    hits = predict.G["hits"]
    us, xs = _setup(fight, opts)
    xof = {id(u): x for u, x in zip(us, xs)}
    for u in us:
        for w in u.weapons:
            w.next = 0
    if opts.get("desync"):
        S = int(opts["desync"] * 30)
        for s in (0, 1):
            side = [u for u in us if u.side == s and u.armed]
            foes = [u for u in us if u.side != s]
            if len(side) < 2 or not foes:
                continue
            cx = sum(e.x for e in foes) / len(foes)
            side.sort(key=lambda u: abs(u.x - cx))
            for k, u in enumerate(side):
                for w in u.weapons:
                    w.next = int(round(S * k / (len(side) - 1)))
    death_on = bool(opts.get("death"))
    cost0 = [sum(u.cost for u in us if u.side == s and u.armed) for s in (0, 1)]
    killed = [0.0, 0.0]
    quiet = 0
    f = 0
    lr = math.log(max(cost0[0], 1.0) / max(cost0[1], 1.0))
    surv = 1.0
    exp_k = [0.0, 0.0]

    def kfrac():
        return (killed[0] / cost0[0] if cost0[0] > 0 else 0.0, killed[1] / cost0[1] if cost0[1] > 0 else 0.0)

    def kill(t):
        t.alive = False
        if t.armed:
            killed[t.side] += t.cost
        x = xof[id(t)]
        if x.death is not None:
            dmg, aoe, edge, para = x.death
            for o in us:
                if o.alive:
                    dd = abs(o.x - t.x)
                    if dd < aoe:
                        v = dmg * (1.0 - edge * dd / aoe)
                        if para:
                            if opts.get("emp"):
                                xof[id(o)].para += v
                        else:
                            hurt(o, v)

    def hurt(t, dmg):
        if not t.alive:
            return
        t.hp -= dmg
        if t.hp <= 0:
            kill(t)

    def stunned(u):
        x = xof[id(u)]
        return x.para > x.maxhp

    def slowf(u):
        x = xof[id(u)]
        return min(SLOW_MAX, x.slowdmg / max(u.hp, 1.0)) if x.slowdmg > 0 else 0.0

    def is_armed(u):
        return u.armed or (death_on and xof[id(u)].kamikaze)

    if trace is not None:
        trace["pdead"] = [0.0] * len(us); trace["ehp"] = [0.0] * len(us)
    mobf = disengage.mob_feats(fight) if hazard is not None and getattr(hazard, "mob", False) else None
    tac = M.get("tacai", {}) if opts.get("tacai") else {}
    while f < HORIZON:
        if hazard is not None and f > 0 and f % 30 == 0:
            kA, kB = kfrac()
            h = hazard.h(f // 30 - 1, kA, kB, lr, None, mobf)
            exp_k[0] += surv * h * kA; exp_k[1] += surv * h * kB
            if trace is not None:
                for i, u in enumerate(us):
                    trace["pdead"][i] += surv * h * (0.0 if u.alive else 1.0)
                    trace["ehp"][i] += surv * h * (max(u.hp, 0.0) if u.alive else 0.0)
            surv *= 1.0 - h
        alive = [u for u in us if u.alive]
        sides = [[u for u in alive if u.side == s] for s in (0, 1)]
        if not any(is_armed(u) for u in sides[0]) or not any(is_armed(u) for u in sides[1]):
            break
        shot_fired = False
        moved = False
        approached = False
        # status: burn ticks, EMP decay, slow decay
        if opts.get("burn") or opts.get("emp") or opts.get("slow"):
            for u in alive:
                x = xof[id(u)]
                if x.burns:
                    b = sum(r for end, r in x.burns if end > f)
                    x.burns = [(end, r) for end, r in x.burns if end > f + DT]
                    if b > 0:
                        hurt(u, b * DT)
                if x.para > 0:
                    x.para = max(0.0, x.para - x.maxhp / STUN_DECAY_S * DT / 30.0)
                if x.slowdmg > 0 and f - x.slow_last > SLOW_GRACE:
                    x.slowdmg = max(0.0, x.slowdmg - SLOW_DECAY * max(u.hp, 1.0) * DT / 30.0)
            alive = [u for u in us if u.alive]
            sides = [[u for u in alive if u.side == s] for s in (0, 1)]
        # movement: close until an enemy is within own range
        for u in alive:
            u.moving = False
            if not u.mobile or u.speed <= 0:
                continue
            if opts.get("emp") and stunned(u):
                continue
            foes = sides[1 - u.side]
            if not foes:
                continue
            nearest = min(foes, key=lambda e: abs(e.x - u.x))
            dist = abs(nearest.x - u.x)
            speed = u.speed * (1.0 - slowf(u)) if opts.get("slow") else u.speed
            x = xof[id(u)]
            pa = posture[u.side] if posture else None
            if pa == "withdraw":
                u.x -= speed * DT if nearest.x > u.x else -speed * DT
                u.moving = True
                moved = True
                continue
            if withdraw and u.side == 0:
                u.x -= speed * DT if nearest.x > u.x else -speed * DT
                u.moving = True
                moved = True
                continue
            if death_on and x.kamikaze:
                if dist <= 20.0:
                    kill(u)
                    continue
                step = min(speed * DT, dist)
                u.x += step if nearest.x > u.x else -step
                u.moving = True
                moved = True
                approached = True
                continue
            if not u.armed:
                continue
            b = tac.get(u.def_)
            if b is not None:
                # ZK tactical AI: only the closest enemy decides; skirm it at range (more while
                # reloading) or flee it beyond its range
                if (nearest.def_ in b["skirms"] or b["skirmEverything"]) and b["skirmRange"] and u.weapons:
                    w0 = max(u.weapons, key=lambda w: w.range)
                    want = b["skirmRange"] - (b["skirmLeeway"] or 0.0) + (b["reloadSkirmLeeway"] or 0.0) * max(0, w0.next - f)
                elif nearest.def_ in b["flees"] or b["fleeEverything"]:
                    want = max(dist, nearest.max_range + (b["fleeLeeway"] or 100.0))
                else:
                    want = None
                if want is not None:
                    if dist < want - 1.0:
                        step = min(speed * DT, want - dist)
                        u.x -= step if nearest.x > u.x else -step
                        u.moving = True
                        moved = True
                    elif dist > want + 1.0 and pa != "hold":
                        step = min(speed * DT, dist - want)
                        u.x += step if nearest.x > u.x else -step
                        u.moving = True
                        moved = True
                        approached = True
                    continue
            if pa == "hold":
                continue
            if dist > u.max_range * 0.95:
                step = min(speed * DT, dist - u.max_range * 0.9)
                u.x += step if nearest.x > u.x else -step
                u.moving = True
                moved = True
                approached = True
            elif (opts.get("kite") and nearest.armed and nearest.mobile and u.max_range - nearest.max_range >= KITE_MARGIN
                  and u.speed >= nearest.speed and dist < u.max_range * 0.9):
                step = min(speed * DT, u.max_range * 0.9 - dist)
                u.x -= step if nearest.x > u.x else -step
                u.moving = True
                moved = True
        alive = [u for u in us if u.alive]
        # fire
        for u in alive:
            if not u.armed or not u.alive:
                continue
            if opts.get("emp") and stunned(u):
                continue
            foes = [e for e in sides[1 - u.side] if e.alive]
            if not foes:
                continue
            ux = xof[id(u)]
            sf = slowf(u) if opts.get("slow") else 0.0
            for w in u.weapons:
                if f < w.next:
                    continue
                can_air, can_ground, burn, blen, emp, slow, pulse = _weapon_info(w)
                eff = ux.engage
                if opts.get("rate") and w.range >= 300 and ux.blockers:
                    eff *= max(0.0, 1.0 - opts["rate"] * min(ux.blockers, 3))
                if opts.get("pulse") and pulse is not None:
                    per_victim, radius = pulse
                    vic = [e for e in foes if e.alive and abs(e.x - u.x) <= radius]
                    if not vic:
                        continue
                    w.next = f + (int(w.reload / (1.0 - sf)) if sf > 0 else w.reload)
                    shot_fired = True
                    for e in vic:
                        if opts.get("slow") and slow > 0:
                            xe = xof[id(e)]
                            xe.slowdmg += slow * per_victim * eff; xe.slow_last = f
                        hurt(e, per_victim * eff)
                    continue
                inr = [(abs(e.x - u.x), e) for e in foes if abs(e.x - u.x) <= w.range and e.alive
                       and (not opts.get("air") or (can_air if xof[id(e)].air else can_ground))]
                if not inr:
                    continue
                w.next = f + (int(w.reload / (1.0 - sf)) if sf > 0 else w.reload)
                shot_fired = True
                dist, tgt = min(inr, key=lambda t: t[0])
                rb = min(int(RBANDS * dist / w.range), RBANDS) if w.range > 0 else RBANDS
                sb = sband(tgt.speed if tgt.moving else 0.0)
                p_any, p_tgt, frac, _ = hits.lookup(w.name, tgt.def_, rb, sb)
                shots = 1 if w.hitscan else w.salvo
                dmg = p_tgt * frac * w.nominal * shots * eff
                xt = xof[id(tgt)]
                if opts.get("burn") and burn > 0 and blen > 0:
                    xt.burns.append((f + blen, burn * p_tgt * shots * eff / blen))
                if opts.get("emp") and emp > 0:
                    xt.para += emp * p_tgt * shots * eff * (xt.maxhp / max(tgt.hp, 1.0))
                    cap = xt.maxhp * (1.0 + M.get("stun_cap_s", 4.0) / STUN_DECAY_S)
                    xt.para = min(xt.para, cap)
                if opts.get("slow") and slow > 0:
                    xt.slowdmg += slow * w.nominal * frac * p_tgt * shots * eff
                    xt.slow_last = f
                if dmg > 0:
                    hurt(tgt, dmg)
        if posture:
            quiet = 0 if (shot_fired or approached) else quiet + DT
            limit = 150 if "withdraw" in posture else 300
        else:
            quiet = 0 if (shot_fired or (moved and not withdraw)) else quiet + DT
            limit = 150 if withdraw else 300
        if quiet > limit:
            break
        f += DT
    kA, kB = kfrac()
    if trace is not None:
        w = surv if hazard is not None else 1.0
        for i, u in enumerate(us):
            trace["pdead"][i] += w * (0.0 if u.alive else 1.0)
            trace["ehp"][i] += w * (max(u.hp, 0.0) if u.alive else 0.0)
    if hazard is not None:
        return exp_k[0] + surv * kA, exp_k[1] + surv * kB
    return kA, kB
