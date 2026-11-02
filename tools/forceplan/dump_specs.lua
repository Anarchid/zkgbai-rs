-- Unit specs for the force planner (crates/zkgbai/src/military/fightsim/plan.rs),
-- from ZK's raw unit files, in the fight predictor's units format: speed in
-- elmos per frame; weapons name:type:range:reload:salvo:damage (salvo =
-- projectiles x burst; shields, fakes and harmless weapons left out, one
-- entry per weapon def, as the AI's fight_spec does); and the build options
-- of factories and constructors (who can make what).
-- usage: lua5.1 dump_specs.lua <zk-dir> > specs.tsv
local dir = arg[1]
Shared = setmetatable({}, {__index = function(t, k) return k end})
local p = io.popen('ls "' .. dir .. '/units"')
print(table.concat({"def", "cost", "health", "speed", "mobile", "air", "builder", "com", "radius", "weapons", "humanname", "buildoptions"}, "\t"))
local function s(v) return v == nil and "" or tostring(v) end
for file in p:lines() do
	if file:match("%.lua$") then
		local ok, t = pcall(dofile, dir .. "/units/" .. file)
		if ok and type(t) == "table" then
			for name, ud in pairs(t) do
				local wdefs = ud.weaponDefs or {}
				local seen, weps = {}, {}
				for _, w in ipairs(ud.weapons or {}) do
					local key = w.def and tostring(w.def):lower()
					local wd = key and (wdefs[w.def] or wdefs[key])
					if not wd and key then
						for k, v in pairs(wdefs) do if tostring(k):lower() == key then wd = v end end
					end
					local wname = name .. "_" .. (key or "?")
					local dmg = wd and wd.damage and tonumber(wd.damage.default) or 0
					local range = wd and tonumber(wd.range) or 0
					if wd and not seen[wname] and wd.weaponType ~= "Shield" and not wname:find("fake") and not wname:find("noweapon")
						and dmg > 0 and range > 0 then
						seen[wname] = true
						local salvo = (tonumber(wd.projectiles) or 1) * (tonumber(wd.burst) or 1)
						weps[#weps + 1] = string.format("%s:%s:%s:%s:%s:%s", wname, s(wd.weaponType or "Cannon"), s(range),
							s(wd.reloadtime or 1), s(math.max(1, salvo)), s(dmg))
					end
				end
				local radius = 12
				if ud.collisionVolumeScales then
					local a, b, c = tostring(ud.collisionVolumeScales):match("([%d%.]+)%s+([%d%.]+)%s+([%d%.]+)")
					if a then radius = math.max(tonumber(a), tonumber(c)) / 2 end
				end
				local speed = tonumber(ud.speed or ud.maxVelocity) or 0
				local mobile = (speed > 0 and ud.canMove ~= false) and 1 or 0
				local com = (name:match("^dyn") or name:match("^comm")) and 1 or 0
				print(table.concat({
					name, s(ud.metalCost or ud.buildCostMetal or 0), s(ud.health or ud.maxDamage or 1), s(speed / 30), s(mobile),
					s(ud.canFly and 1 or 0), s(ud.builder and 1 or 0), s(com), s(radius), table.concat(weps, "|"), s(ud.name),
					table.concat(ud.buildoptions or {}, ","),
				}, "\t"))
			end
		else
			io.stderr:write("skip " .. file .. ": " .. tostring(t) .. "\n")
		end
	end
end
