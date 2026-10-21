-- Arena census (install with ARENA_WIDGETS; summarise with census.py): per
-- team and unit type, units finished, lost and the enemy metal they killed,
-- as "CENSUS f=<frame> team<N> <def>=<built>/<lost>/<killed metal> ...";
-- and what killed constructors, mexes and energy, as "ECOLOSS f=<frame>
-- team<N> <con|mex|energy> <killer def>=<count> ...". Every 3 minutes and
-- at game over.
function widget:GetInfo()
	return {
		name    = "Arena Census",
		desc    = "Logs units built and metal killed per unit type",
		layer   = 0,
		enabled = true,
	}
end

local built = {}   -- built[team][defName] = count
local killed = {}  -- killed[team][defName] = metal of enemies killed
local lost = {}    -- lost[team][defName] = count lost
local eco = {}     -- eco[team][victim kind][attacker defName] = count (constructors, mexes, energy)

local function ecoKind(ud)
	local n = ud.name
	if (ud.isBuilder or ud.buildSpeed > 0) and (ud.speed or 0) > 0 and not n:find("dyn") then return "con" end
	if n == "staticmex" then return "mex" end
	if n:find("^energy") then return "energy" end
	return nil
end

local function bump(t, team, name, v)
	t[team] = t[team] or {}
	t[team][name] = (t[team][name] or 0) + v
end

function widget:UnitFinished(unitID, unitDefID, unitTeam)
	local ud = UnitDefs[unitDefID]
	if ud then bump(built, unitTeam, ud.name, 1) end
end

function widget:UnitDestroyed(unitID, unitDefID, unitTeam, attackerID, attackerDefID, attackerTeam)
	local ud = UnitDefs[unitDefID]
	if not ud then return end
	bump(lost, unitTeam, ud.name, 1)
	if attackerDefID and attackerTeam and attackerTeam ~= unitTeam then
		local ad = UnitDefs[attackerDefID]
		if ad then
			bump(killed, attackerTeam, ad.name, ud.metalCost or 0)
			local k = ecoKind(ud)
			if k then
				eco[unitTeam] = eco[unitTeam] or {}
				bump(eco[unitTeam], k, ad.name, 1)
			end
		end
	end
end

local function report(f)
	for _, team in ipairs(Spring.GetTeamList()) do
		local names = {}
		for name in pairs(built[team] or {}) do names[name] = true end
		for name in pairs(killed[team] or {}) do names[name] = true end
		local parts = {}
		for name in pairs(names) do
			parts[#parts + 1] = string.format("%s=%d/%d/%.0f", name, (built[team] or {})[name] or 0,
				(lost[team] or {})[name] or 0, (killed[team] or {})[name] or 0)
		end
		table.sort(parts)
		if #parts > 0 then
			Spring.Echo("CENSUS f=" .. f .. " team" .. team .. " " .. table.concat(parts, " "))
		end
		for kind, by in pairs(eco[team] or {}) do
			local ps = {}
			for name, n in pairs(by) do ps[#ps + 1] = name .. "=" .. n end
			table.sort(ps)
			Spring.Echo("ECOLOSS f=" .. f .. " team" .. team .. " " .. kind .. " " .. table.concat(ps, " "))
		end
	end
end

function widget:GameFrame(n)
	if n > 0 and n % 5400 == 0 then report(n) end
end

function widget:GameOver()
	report(Spring.GetGameFrame())
end

function widget:Shutdown()
	report(Spring.GetGameFrame())
end
