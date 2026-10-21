-- Arena economy timeline (install with ARENA_WIDGETS; summarise with
-- econ.py). Every 30 s per team:
--   ECON f=<frame> team<N> minc= einc= base= od= misc= mex= cons= conbuilt=
--        conlost= conhome= conaway= army= porc= ecur= ewaste= odenergy=
-- (ewaste / odenergy: the allyteam's energy wasted / spent on overdrive)
-- (base/od/misc: ZK's metal from mexes, overdrive and the rest; conhome /
-- conaway: constructors within / beyond 1500 of the team's start; army /
-- porc: metal of mobile / static armed units alive), and events:
--   CONDEATH f= team<N> <def> at=<x>,<z> home=<dist> age=<s> by=<def>|?
--   MEXDEATH f= team<N> at=<x>,<z> home=<dist> age=<s> by=<def>|?
--   MEXUP    f= team<N> at=<x>,<z> home=<dist>
--   BUILD    f= team<N> <def>   (every unit finished in the first 6 minutes)
function widget:GetInfo()
	return {
		name    = "Arena Economy Timeline",
		desc    = "Logs per-team economy and constructor/mex events",
		layer   = 0,
		enabled = true,
	}
end

local HOME = 1500
-- Every unit finished before this frame is logged (the build orders).
local BUILD_ORDER_FRAMES = 30 * 360
local born = {}      -- unitID -> frame finished
local conBuilt = {}
local conLost = {}
local mexDef = UnitDefNames.staticmex and UnitDefNames.staticmex.id

local function isCon(ud)
	return ud and (ud.buildSpeed or 0) > 0 and (ud.speed or 0) > 0 and not ud.name:find("^dyn")
end

local function home(team, x, z)
	local sx, _, sz = Spring.GetTeamStartPosition(team)
	if not sx then return -1 end
	return math.floor(math.sqrt((x - sx) ^ 2 + (z - sz) ^ 2))
end

function widget:UnitFinished(unitID, unitDefID, team)
	born[unitID] = Spring.GetGameFrame()
	local ud = UnitDefs[unitDefID]
	if born[unitID] < BUILD_ORDER_FRAMES and ud then
		Spring.Echo(string.format("BUILD f=%d team%d %s", born[unitID], team, ud.name))
	end
	if isCon(ud) then
		conBuilt[team] = (conBuilt[team] or 0) + 1
	elseif unitDefID == mexDef then
		local x, _, z = Spring.GetUnitPosition(unitID)
		Spring.Echo(string.format("MEXUP f=%d team%d at=%d,%d home=%d", Spring.GetGameFrame(), team, x, z,
			home(team, x, z)))
	end
end

function widget:UnitDestroyed(unitID, unitDefID, team, attackerID, attackerDefID, attackerTeam)
	local ud = UnitDefs[unitDefID]
	local f = Spring.GetGameFrame()
	local con, mex = isCon(ud), unitDefID == mexDef
	if (con or mex) and born[unitID] then
		local x, _, z = Spring.GetUnitPosition(unitID)
		local by = (attackerDefID and UnitDefs[attackerDefID] and attackerTeam ~= team) and UnitDefs[attackerDefID].name or "?"
		local age = math.floor((f - born[unitID]) / 30)
		if con then
			conLost[team] = (conLost[team] or 0) + 1
			Spring.Echo(string.format("CONDEATH f=%d team%d %s at=%d,%d home=%d age=%d by=%s", f, team, ud.name, x, z,
				home(team, x, z), age, by))
		else
			Spring.Echo(string.format("MEXDEATH f=%d team%d at=%d,%d home=%d age=%d by=%s", f, team, x, z,
				home(team, x, z), age, by))
		end
	end
	born[unitID] = nil
end

local function rp(team, key)
	return Spring.GetTeamRulesParam(team, key) or -1
end

local function report(f)
	local gaia = Spring.GetGaiaTeamID()
	for _, team in ipairs(Spring.GetTeamList()) do
		if team ~= gaia then
			local _, _, _, minc = Spring.GetTeamResources(team, "metal")
			local ecur, estor, _, einc = Spring.GetTeamResources(team, "energy")
			local mex, cons, conhome, conaway, army, porc = 0, 0, 0, 0, 0, 0
			for _, u in ipairs(Spring.GetTeamUnits(team) or {}) do
				local d = Spring.GetUnitDefID(u)
				local ud = UnitDefs[d]
				local _, _, _, _, bp = Spring.GetUnitHealth(u)
				if ud and (bp or 1) >= 1 then
					if d == mexDef then
						mex = mex + 1
					elseif isCon(ud) then
						cons = cons + 1
						local x, _, z = Spring.GetUnitPosition(u)
						if home(team, x, z) <= HOME then conhome = conhome + 1 else conaway = conaway + 1 end
					elseif ud.canAttack and #(ud.weapons or {}) > 0 and not ud.name:find("^dyn") then
						if (ud.speed or 0) > 0 then army = army + (ud.metalCost or 0) else porc = porc + (ud.metalCost or 0) end
					end
				end
			end
			Spring.Echo(string.format(
				"ECON f=%d team%d minc=%.1f einc=%.1f base=%.1f od=%.1f misc=%.1f mex=%d cons=%d conbuilt=%d conlost=%d conhome=%d conaway=%d army=%d porc=%d ecur=%d ewaste=%.1f odenergy=%.1f",
				f, team, minc or 0, einc or 0, rp(team, "OD_metalBase"), rp(team, "OD_metalOverdrive"),
				rp(team, "OD_metalMisc"), mex, cons, conBuilt[team] or 0, conLost[team] or 0, conhome, conaway, army, porc,
				ecur or 0, rp(team, "OD_team_energyWaste"), rp(team, "OD_team_energyOverdrive")))
		end
	end
end

function widget:GameFrame(n)
	if n > 0 and n % 900 == 0 then report(n) end
end
