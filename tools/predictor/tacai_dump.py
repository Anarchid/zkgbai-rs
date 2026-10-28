#!/usr/bin/env python3
"""Resolve ZK's Tactical Unit AI config (LuaRules/Configs/tactical_ai_defs.lua) into plain tables.

usage: tacai_dump.py <unitdefs.tsv> <wtab.pkl> <mech.pkl> <out.json> [zk.sdd]
Runs the real config in lua5.1 with UnitDefs / UnitDefNames / WeaponDefs stubbed from our unit-def
dump (canFly, isImmobile, canAttack, speed, maxWeaponRange, weapons with range and whether the first
targets land, commtype for commanders), then writes per unit: skirms / flees / swarms (unit def
names), skirmLeeway, reloadSkirmLeeway, skirmRange (its weapon's range) and the flee distances.
"""
import json
import os
import pickle
import subprocess
import sys
import tempfile

import units as unitmod


def lua_str(s):
    return '"' + s.replace("\\", "\\\\").replace('"', '\\"') + '"'


def main(udp, wtp, mechp, out, zk=os.path.expanduser("~/.spring/games/zk.sdd")):
    ud = unitmod.load(udp)
    wtab = pickle.load(open(wtp, "rb"))
    air_rules = pickle.load(open(mechp, "rb")).get("air", {})
    names = sorted(ud.defs)
    wid = {}
    lines = ["WeaponDefs = {}", "UnitDefs = {}", "UnitDefNames = {}"]
    for i, n in enumerate(names, 1):
        d = ud.defs[n]
        ws = []
        for w in d.weapons:
            if w.name not in wid:
                wid[w.name] = len(wid) + 1
                rng = float(wtab.loc[w.name, "range"]) if w.name in wtab.index else w.range
                lines.append(f"WeaponDefs[{wid[w.name]}] = {{range = {rng}, name = {lua_str(w.name)}}}")
            land = air_rules.get(w.name, (True, True))[1]
            ws.append(f"{{weaponDef = {wid[w.name]}, onlyTargets = {{land = {'true' if land else 'false'}}}}}")
        com = "true" if d.is_com or n.startswith(("dyn", "comm")) else "nil"
        lines.append(f"UnitDefs[{i}] = {{id = {i}, name = {lua_str(n)}, canFly = {'true' if d.canfly else 'false'}, "
                     f"isImmobile = {'true' if d.static else 'false'}, canAttack = {'true' if d.weapons else 'false'}, "
                     f"speed = {d.speed * 30.0}, maxWeaponRange = {d.max_range}, weapons = {{{', '.join(ws)}}}, "
                     f"customParams = {{commtype = {com}}}}}")
        lines.append(f"UnitDefNames[{lua_str(n)}] = UnitDefs[{i}]")
    lines.append("function include() end")
    cfg = os.path.join(zk, "LuaRules", "Configs", "tactical_ai_defs.lua")
    lines.append(f"local B = dofile({lua_str(cfg)})")
    lines.append(r'''
local function set(t)
  local o = {}
  if t then for id, v in pairs(t) do if v and UnitDefs[id] then o[#o + 1] = UnitDefs[id].name end end end
  table.sort(o)
  return table.concat(o, ",")
end
for id, b in pairs(B) do
  if b.land then b = b.land end
  print(table.concat({UnitDefs[id].name, set(b.skirms), set(b.flees), set(b.swarms), tostring(b.skirmLeeway or ""),
    tostring(b.reloadSkirmLeeway or ""), tostring(b.skirmRange or ""), tostring(b.fleeLeeway or ""), tostring(b.fleeDistance or ""),
    tostring(b.skirmEverything or ""), tostring(b.fleeEverything or ""), tostring(b.defaultAIState or "")}, "\t"))
end''')
    with tempfile.NamedTemporaryFile("w", suffix=".lua", delete=False) as fh:
        fh.write("\n".join(lines))
        path = fh.name
    res = subprocess.run(["lua5.1", path], capture_output=True, text=True)
    os.unlink(path)
    if res.returncode != 0:
        sys.exit(res.stderr)
    beh = {}
    for line in res.stdout.splitlines():
        p = line.split("\t")
        num = lambda x: float(x) if x not in ("", "nil") else None
        beh[p[0]] = {"skirms": [x for x in p[1].split(",") if x], "flees": [x for x in p[2].split(",") if x],
                     "swarms": [x for x in p[3].split(",") if x], "skirmLeeway": num(p[4]), "reloadSkirmLeeway": num(p[5]),
                     "skirmRange": num(p[6]), "fleeLeeway": num(p[7]), "fleeDistance": num(p[8]),
                     "skirmEverything": p[9] == "true", "fleeEverything": p[10] == "true", "defaultAIState": num(p[11])}
    json.dump(beh, open(out, "w"), indent=1, sort_keys=True)
    sk = {k: v for k, v in beh.items() if v["skirms"] or v["skirmEverything"]}
    print(f"{len(beh)} units with tactical AI; {len(sk)} skirm, {sum(1 for v in beh.values() if v['flees'])} flee, "
          f"{sum(1 for v in beh.values() if v['swarms'])} swarm")
    for n in ("hoverskirm", "cloakskirm", "shieldskirm", "shieldfelon", "tankriot", "cloakraid"):
        b = beh.get(n)
        if b:
            print(f"  {n}: skirms {len(b['skirms'])} defs (e.g. {', '.join(b['skirms'][:6])}), leeway {b['skirmLeeway']}, "
                  f"reload leeway {b['reloadSkirmLeeway']}, range {b['skirmRange']}, flees {len(b['flees'])}, state {b['defaultAIState']}")


if __name__ == "__main__":
    main(*sys.argv[1:6])
