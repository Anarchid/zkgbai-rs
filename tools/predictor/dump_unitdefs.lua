-- Dump the ZK unit defs the predictor needs as TSV.
-- usage: lua5.1 dump_unitdefs.lua <zk-dir> > unitdefs.tsv
local dir = arg[1]
Shared = setmetatable({}, {__index = function(t, k) return k end})
local p = io.popen('ls "' .. dir .. '/units"')
local cols = {"name", "humanname", "cost", "health", "speed", "category", "moveclass", "canfly",
	"footprint", "radius", "builder", "weapons", "shieldpower", "shieldregen", "canmove", "description", "wextra"}
print(table.concat(cols, "\t"))
local function s(v) return v == nil and "" or tostring(v) end
for file in p:lines() do
	if file:match("%.lua$") then
		local ok, t = pcall(dofile, dir .. "/units/" .. file)
		if ok and type(t) == "table" then
			for name, ud in pairs(t) do
				local weps = {}
				local wdefs = ud.weaponDefs or {}
				local wkeys = {}
				for k in pairs(wdefs) do wkeys[#wkeys + 1] = k end
				table.sort(wkeys)
				-- weapons: list of "<weapondefname>:<range>:<slot>" where slot order follows ud.weapons
				for i, w in ipairs(ud.weapons or {}) do
					local key = w.def and tostring(w.def):lower()
					local wd = key and (wdefs[w.def] or wdefs[key])
					if not wd then
						for k, v in pairs(wdefs) do if tostring(k):lower() == key then wd = v end end
					end
					local cp = wd and wd.customParams or {}
					weps[#weps + 1] = string.format("%s_%s:%s:%s:%s:%s", name, key or "?",
						s(wd and wd.range), s(wd and wd.reloadtime), s(cp.slot or ""), s(wd and wd.damage and wd.damage.default))
				end
				local wx = {}
				for _, k in ipairs(wkeys) do
					local wd = wdefs[k]
					local cp = wd.customParams or {}
					wx[#wx + 1] = string.format("%s_%s:%s:%s:%s:%s:%s:%s:%s:%s", name, tostring(k):lower(),
						s(wd.projectiles or 1), s(wd.burst or 1), s(wd.paralyzer and 1 or 0),
						s(wd.areaOfEffect or wd.damageAreaOfEffect or 0), s(cp.extra_damage or ""), s(cp.impulse or ""),
						s(cp.timeslow_damagefactor or ""), s((cp.disarmdamageonly or cp.disarmDamageOnly) and 1 or (cp.setunitsonfire and 2 or 0)))
				end
				local sh = ""
				local shregen = ""
				for k, wd in pairs(wdefs) do
					if wd.weaponType == "Shield" then sh = s(wd.shieldPower); shregen = s(wd.shieldPowerRegen) end
				end
				local radius = ""
				if ud.collisionVolumeScales then
					local a, b, c = tostring(ud.collisionVolumeScales):match("([%d%.]+)%s+([%d%.]+)%s+([%d%.]+)")
					if a then radius = s(math.max(tonumber(a), tonumber(c)) / 2) end
				end
				print(table.concat({
					name, s(ud.name), s(ud.metalCost or ud.buildCostMetal), s(ud.health or ud.maxDamage), s(ud.speed or ud.maxVelocity),
					s(ud.category), s(ud.movementClass), s(ud.canFly and 1 or 0), s(ud.footprintX),
					radius, s(ud.builder and 1 or 0), table.concat(weps, ","), sh, shregen, s(ud.canMove and 1 or 0), s(ud.description), table.concat(wx, ","),
				}, "\t"))
			end
		else
			io.stderr:write("skip " .. file .. ": " .. tostring(t) .. "\n")
		end
	end
end
