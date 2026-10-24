"""Unit def table for the predictor (from dump_unitdefs.lua) and ZKGBAI's flat threat."""
import math
import os
import re

import pandas as pd

RUNS = os.path.expanduser("~/Programs/zkgbai-runs")


def _num(v, default=0.0):
    try:
        return float(v)
    except (TypeError, ValueError):
        return default


class Weapon:
    __slots__ = ("name", "range", "reload", "damage", "projectiles", "burst", "paralyzer", "aoe",
                 "extra_damage", "impulse", "slow", "flags", "fake")

    def __init__(self, name):
        self.name = name
        self.range = self.reload = self.damage = 0.0
        self.projectiles = self.burst = 1
        self.paralyzer = 0
        self.aoe = 0.0
        self.extra_damage = self.impulse = self.slow = 0.0
        self.flags = 0  # 1 disarm-only, 2 sets on fire
        self.fake = "fake" in name or "noweapon" in name

    def power(self):
        """ZKGBAI weapon_power (military/wrappers/mod.rs) with paralyzer bonus."""
        shot = self.damage * self.projectiles * self.burst
        # sorted customParams: disarmDamageOnly, extra_damage, impulse, setunitsonfire, timeslow_damagefactor
        if self.flags == 1:
            shot /= 10.0
        shot += self.extra_damage
        shot += self.impulse / 10.0
        if self.flags == 2:
            shot *= 2.0
        if self.slow:
            shot += min(shot * self.slow, shot / 2.0)
        if self.paralyzer:
            shot *= 2.0
        shot *= 1.0 + self.aoe / 100.0
        return shot / self.reload if self.reload > 0 else shot


class UnitDef:
    __slots__ = ("name", "human", "cost", "health", "speed", "category", "canfly", "radius", "builder",
                 "weapons", "shield", "shieldregen", "canmove", "description", "is_com", "is_arty",
                 "is_riot", "static")

    def __init__(self, row):
        self.name = row["name"]
        self.human = row["humanname"]
        self.cost = _num(row["cost"])
        self.health = _num(row["health"])
        self.speed = _num(row["speed"]) / 30.0  # elmos per frame
        self.category = row["category"] or ""
        self.canfly = row["canfly"] == "1"
        self.radius = _num(row["radius"], 0) or 8.0 * _num(row["footprint"], 2) / 2.0
        self.builder = row["builder"] == "1"
        self.shield = _num(row["shieldpower"])
        self.shieldregen = _num(row["shieldregen"])
        self.canmove = row["canmove"] == "1" and self.speed > 0
        self.static = not self.canmove
        self.description = row["description"] or ""
        self.is_com = False
        d = self.description
        self.is_arty = ("Arti" in d or "Skirm" in d or self.name == "vehsupport") or (self.builder and "Riot" not in d)
        self.is_riot = "Riot" in d or "Anti-Swarm" in d
        weps = {}
        for item in (row["weapons"] or "").split(","):
            if not item:
                continue
            parts = item.split(":")
            w = Weapon(parts[0])
            w.range = _num(parts[1])
            w.reload = _num(parts[2])
            w.damage = _num(parts[4]) if len(parts) > 4 else 0.0
            weps[parts[0]] = w
        for item in (row["wextra"] or "").split(","):
            if not item:
                continue
            p = item.split(":")
            w = weps.get(p[0])
            if w is None:
                continue
            w.projectiles = int(_num(p[1], 1))
            w.burst = int(_num(p[2], 1))
            w.paralyzer = int(_num(p[3], 0))
            w.aoe = _num(p[4])
            w.extra_damage = _num(p[5])
            w.impulse = _num(p[6])
            w.slow = _num(p[7])
            w.flags = int(_num(p[8], 0))
        self.weapons = [w for w in weps.values() if not w.fake and w.damage > 0 and w.range > 0]

    @property
    def armed(self):
        return bool(self.weapons)

    @property
    def max_range(self):
        return max((w.range for w in self.weapons), default=0.0)

    def power(self):
        if self.is_com:
            return 300.0
        return sum(w.power() for w in self.weapons if "torpedo" not in w.name or self.static)

    def danger(self, health=None):
        """ZKGBAI Enemy::get_danger: (power + health) / 10, / 3 for arty, skirm, builders."""
        if not self.armed:
            return 0.0
        d = (self.power() + (self.health if health is None else health)) / 10.0
        if (self.is_arty and self.name != "amphfloater") or self.builder:
            d /= 3.0
        return d

    def size_class(self):
        if self.static:
            return "static"
        if self.canfly:
            return "air"
        if self.radius <= 12:
            return "small"
        if self.radius <= 24:
            return "medium"
        return "large"


_COM = re.compile(r"^(dyn\w+?\d|armcom\d|benzcom\d|comm?_\w+|cremcom\d|dyntrainer\w+)$")


def load(path=os.path.join(RUNS, "predictor", "unitdefs.tsv")):
    df = pd.read_csv(path, sep="\t", dtype=str, keep_default_na=False, quoting=3)
    defs = {}
    for _, row in df.iterrows():
        ud = UnitDef(row)
        defs[ud.name] = ud
    return UnitDefs(defs)


class UnitDefs:
    def __init__(self, defs):
        self.defs = defs
        self._dyn = {}

    def get(self, name, hpmax=None):
        ud = self.defs.get(name)
        if ud is not None:
            return ud
        if name in self._dyn:
            return self._dyn[name]
        if _COM.match(name) or name.startswith("dyn"):
            # Commander: cost 1200 + 250 per level (dynassault3 -> level 3), weapons unknown here
            m = re.search(r"(\d+)$", name)
            level = int(m.group(1)) if m else 0
            row = {"name": name, "humanname": "Commander", "cost": str(1200 + 250 * level), "health": str(hpmax or 3000),
                   "speed": "37.5", "category": "LAND", "canfly": "0", "footprint": "2", "radius": "16", "builder": "1",
                   "weapons": f"{name}_laser:300:0.1::15", "shieldpower": "", "shieldregen": "", "canmove": "1",
                   "description": "Commander", "wextra": ""}
            ud = UnitDef(row)
            ud.is_com = True
            ud.builder = False
            ud.is_arty = False
            self._dyn[name] = ud
            return ud
        return None
