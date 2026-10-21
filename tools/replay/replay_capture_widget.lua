-- Replay capture (installed by tools/replay/capture.sh into a demo playback's
-- write-dir): dumps the game as JSON lines to replaycap.jsonl so a finished
-- game can be read at any moment ("time travel") without the engine.
--   meta    map, teams, start positions, metal spots, a coarse height grid,
--           unit types (cost, build power, speed, range...)
--   snap    every SNAP_FRAMES: each team's resources and ZK economy params,
--           every unit [id, def, team, x, z, hp%, build%, seen-by-enemy bits,
--           first command, command target x, z], each allyteam's LOS/radar
--           grid, wrecks with metal
--   new/done/dead/give   unit lifecycle with positions and killers
--   cmd     orders given (grouped per frame, team, command and parameters);
--           AI teams only their build orders
--   dmg     damage per window by attacker team/def, victim team/def, 512-cell
--   chat    console lines from players
--   over/stats   the result and the engine's team stats history
-- Ends the playback (quitforce) after game over or the last demo frame.
function widget:GetInfo()
	return {
		name    = "Replay Capture",
		desc    = "Dumps periodic game state and events of a replay as JSON lines",
		layer   = 0,
		enabled = true,
	}
end

local SNAP_FRAMES = 300
local DMG_FRAMES = 150
local CELL = 512
local HEIGHT_CELL = 128
local SIGHT_CELL = 256
local MIN_WRECK_METAL = 20
-- Frames to write a Lua save at (Saves/replay_f<frame>.slsf), substituted
-- by capture.sh -S.
local SAVE_FRAMES = {@SAVE_FRAMES@}

local out
local gaia = Spring.GetGaiaTeamID()
local aiTeam = {}
local allyOf = {}
local enemyAlly = {}
local cmdNames = {}
local cmdBuf = {}
local dmgBuf = {}
local overAt

local function enc(v)
	local t = type(v)
	if t == "number" then
		if v == math.floor(v) and math.abs(v) < 1e15 then return string.format("%d", v) end
		return string.format("%.4g", v)
	elseif t == "string" then
		return '"' .. v:gsub('[%c"\\]', function(c)
			if c == '"' then return '\\"' elseif c == "\\" then return "\\\\" end
			return string.format("\\u%04x", c:byte())
		end) .. '"'
	elseif t == "boolean" then
		return tostring(v)
	elseif t == "table" then
		local n = #v
		if n > 0 or next(v) == nil then
			local parts = {}
			for i = 1, n do parts[i] = enc(v[i]) end
			return "[" .. table.concat(parts, ",") .. "]"
		end
		local parts = {}
		for k, x in pairs(v) do parts[#parts + 1] = enc(tostring(k)) .. ":" .. enc(x) end
		return "{" .. table.concat(parts, ",") .. "}"
	end
	return "null"
end

local function emit(rec)
	if out then out:write(enc(rec), "\n") end
end

local function r(x) return x and math.floor(x + 0.5) or nil end

local function cmdName(id)
	if id < 0 then
		local ud = UnitDefs[-id]
		return ud and ("build:" .. ud.name) or ("build:" .. -id)
	end
	return cmdNames[id] or tostring(id)
end

local function teamMeta()
	local teams = {}
	for _, team in ipairs(Spring.GetTeamList()) do
		if team ~= gaia then
			local _, leader, _, isAI, side, ally = Spring.GetTeamInfo(team, false)
			local name = leader and Spring.GetPlayerInfo(leader, false) or "?"
			if isAI then
				local _, aiName, _, shortName = Spring.GetAIInfo(team)
				name = (aiName or "AI") .. " [" .. tostring(shortName) .. "]"
				aiTeam[team] = true
			end
			allyOf[team] = ally
			local sx, _, sz = Spring.GetTeamStartPosition(team)
			teams[#teams + 1] = {team = team, ally = ally, name = name, ai = isAI and true or false, side = side,
				start = {r(sx), r(sz)}}
		end
	end
	for team, ally in pairs(allyOf) do
		for other, oa in pairs(allyOf) do
			if oa ~= ally then enemyAlly[team] = oa end
		end
	end
	return teams
end

local function writeMeta()
	local spots = {}
	for i = 1, Spring.GetGameRulesParam("mex_count") or 0 do
		spots[#spots + 1] = {r(Spring.GetGameRulesParam("mex_x" .. i)), r(Spring.GetGameRulesParam("mex_z" .. i)),
			Spring.GetGameRulesParam("mex_metal" .. i) or 0}
	end
	local rows = {}
	for z = HEIGHT_CELL / 2, Game.mapSizeZ, HEIGHT_CELL do
		local row = {}
		for x = HEIGHT_CELL / 2, Game.mapSizeX, HEIGHT_CELL do row[#row + 1] = r(Spring.GetGroundHeight(x, z)) end
		rows[#rows + 1] = row
	end
	-- name -> [human name, metal, build power, speed, weapons, factory,
	-- health, max weapon range, LOS radius]
	local defs = {}
	for _, ud in pairs(UnitDefs) do
		defs[ud.name] = {ud.humanName or ud.name, ud.metalCost or 0, ud.buildSpeed or 0, r(ud.speed or 0),
			#(ud.weapons or {}), ud.isFactory and true or false, r(ud.health or 0), r(ud.maxWeaponRange or 0),
			r(ud.losRadius or 0)}
	end
	emit({k = "meta", f = Spring.GetGameFrame(), map = Game.mapName, game = Game.gameName .. " " .. Game.gameVersion,
		mapX = Game.mapSizeX, mapZ = Game.mapSizeZ, teams = teamMeta(), mex = spots,
		heights = {cell = HEIGHT_CELL, rows = rows}, defs = defs})
end

local function resources(team, kind)
	local cur, stor, pull, inc, exp, share, sent, recv = Spring.GetTeamResources(team, kind)
	return {r(cur), r(stor), r(pull), (inc or 0), (exp or 0), share or 0, r(sent), r(recv)}
end

local function rp(team, key)
	return Spring.GetTeamRulesParam(team, key) or -1
end

local function unitRow(u)
	local d = Spring.GetUnitDefID(u)
	local ud = d and UnitDefs[d]
	local team = Spring.GetUnitTeam(u)
	local x, _, z = Spring.GetUnitPosition(u)
	if not (ud and x) then return nil end
	local hp, maxhp, _, _, bp = Spring.GetUnitHealth(u)
	local seen = 0
	if enemyAlly[team] then seen = Spring.GetUnitLosState(u, enemyAlly[team], true) or 0 end
	local cmd, cx, cz
	local cmds = Spring.GetUnitCommands(u, 1)
	if (not cmds or #cmds == 0) and ud.isFactory then cmds = Spring.GetFactoryCommands(u, 1) end
	local c = cmds and cmds[1]
	if c then
		cmd = cmdName(c.id)
		local p = c.params or {}
		if #p >= 3 then
			cx, cz = p[1], p[3]
		elseif #p == 1 then
			local tx, _, tz = Spring.GetUnitPosition(p[1])
			cx, cz = tx, tz
		end
	end
	return {u, ud.name, team, r(x), r(z), maxhp and maxhp > 0 and math.floor(100 * hp / maxhp) or 0,
		math.floor(100 * (bp or 1)), seen, cmd or "", r(cx) or -1, r(cz) or -1}
end

local function snapshot(f)
	local res = {}
	for team in pairs(allyOf) do
		res[tostring(team)] = {m = resources(team, "metal"), e = resources(team, "energy"),
			od = {rp(team, "OD_metalBase"), rp(team, "OD_metalOverdrive"), rp(team, "OD_metalMisc"),
				rp(team, "OD_energyIncome"), rp(team, "OD_team_energyWaste"), rp(team, "OD_team_energyOverdrive")}}
	end
	local units = {}
	for _, u in ipairs(Spring.GetAllUnits()) do
		local row = unitRow(u)
		if row then units[#units + 1] = row end
	end
	-- What each allyteam sees: one char per SIGHT_CELL, 0 none, 1 radar,
	-- 2 LOS, 3 both; rows north to south.
	local sight = {}
	local allies = {}
	for _, ally in pairs(allyOf) do allies[ally] = true end
	for ally in pairs(allies) do
		local rows = {}
		for z = SIGHT_CELL / 2, Game.mapSizeZ, SIGHT_CELL do
			local row = {}
			for x = SIGHT_CELL / 2, Game.mapSizeX, SIGHT_CELL do
				local y = Spring.GetGroundHeight(x, z)
				row[#row + 1] = (Spring.IsPosInLos(x, y, z, ally) and 2 or 0) + (Spring.IsPosInRadar(x, y, z, ally) and 1 or 0)
			end
			rows[#rows + 1] = table.concat(row)
		end
		sight[tostring(ally)] = rows
	end
	-- Reclaimable metal lying around (wrecks, map features).
	local wrecks = {}
	for _, fid in ipairs(Spring.GetAllFeatures()) do
		local metal = Spring.GetFeatureResources(fid)
		if metal and metal >= MIN_WRECK_METAL then
			local fd = FeatureDefs[Spring.GetFeatureDefID(fid)]
			local x, _, z = Spring.GetFeaturePosition(fid)
			wrecks[#wrecks + 1] = {fd and fd.name or "?", r(x), r(z), r(metal)}
		end
	end
	emit({k = "snap", f = f, res = res, units = units, sight = {cell = SIGHT_CELL, ally = sight}, wrecks = wrecks})
end

local function flushCmds(f)
	for _, c in pairs(cmdBuf) do
		emit({k = "cmd", f = f, t = c.t, c = c.c, p = c.p, o = c.o, d = c.d, u = c.u})
	end
	cmdBuf = {}
end

local function flushDmg(f)
	local rows = {}
	for key, v in pairs(dmgBuf) do
		local at, ad, vt, vd, cx, cz = key:match("^(-?%d+)|([^|]*)|(%d+)|([^|]*)|(%d+)|(%d+)$")
		rows[#rows + 1] = {tonumber(at), ad, tonumber(vt), vd, tonumber(cx), tonumber(cz), r(v[1]), r(v[2])}
	end
	if #rows > 0 then emit({k = "dmg", f = f, cell = CELL, rows = rows}) end
	dmgBuf = {}
end

local function writeStats()
	for team in pairs(allyOf) do
		local n = Spring.GetTeamStatsHistory(team) or 0
		if n > 0 then
			emit({k = "stats", team = team, hist = Spring.GetTeamStatsHistory(team, 0, n)})
		end
	end
end

local function finish()
	local f = Spring.GetGameFrame()
	flushCmds(f)
	flushDmg(f)
	snapshot(f)
	writeStats()
	emit({k = "end", f = f})
	out:close()
	out = nil
	Spring.SendCommands("quitforce")
end

function widget:Initialize()
	out = io.open("replaycap.jsonl", "w")
	for name, id in pairs(CMD) do
		if type(id) == "number" and type(name) == "string" and name:upper() == name then cmdNames[id] = name end
	end
	local ok, custom = pcall(VFS.Include, "LuaRules/Configs/customcmds.lua")
	if ok and type(custom) == "table" then
		for name, id in pairs(custom) do
			if type(id) == "number" and type(name) == "string" then cmdNames[id] = name end
		end
	end
	local _, fullView = Spring.GetSpectatingState()
	if not fullView then Spring.SendCommands("specfullview 1") end
	Spring.SendCommands("setmaxspeed 200", "setspeed 200")
	if Spring.GetGameFrame() > 0 then writeMeta() end
end

function widget:GameStart()
	writeMeta()
end

local lastFrame, lastTimer = -1, nil

-- A demo that ends without game over leaves the engine idle: finish once
-- the frame stops moving for 20 s of wall time.
function widget:Update()
	local f = Spring.GetGameFrame()
	if f ~= lastFrame then
		lastFrame, lastTimer = f, Spring.GetTimer()
	elseif out and f > 0 and Spring.DiffTimers(Spring.GetTimer(), lastTimer) > 20 then
		finish()
	end
end

function widget:GameFrame(n)
	flushCmds(n)
	if n % DMG_FRAMES == 0 then flushDmg(n) end
	if n % SNAP_FRAMES == 0 then snapshot(n) end
	for _, sf in ipairs(SAVE_FRAMES) do
		if n == sf then Spring.SendCommands("luasave replay_f" .. n .. " -y") end
	end
	if overAt and n >= overAt + 90 and out then finish() end
end

local function pos(u)
	local x, _, z = Spring.GetUnitPosition(u)
	return r(x), r(z)
end

function widget:UnitCreated(u, d, team, builder)
	local x, z = pos(u)
	emit({k = "new", f = Spring.GetGameFrame(), u = u, d = UnitDefs[d].name, t = team, x = x, z = z, b = builder})
end

function widget:UnitFinished(u, d, team)
	local x, z = pos(u)
	emit({k = "done", f = Spring.GetGameFrame(), u = u, d = UnitDefs[d].name, t = team, x = x, z = z})
end

function widget:UnitDestroyed(u, d, team, attacker, attackerDef, attackerTeam)
	local x, z = pos(u)
	local ax, az
	if attacker then ax, az = pos(attacker) end
	local _, _, _, _, bp = Spring.GetUnitHealth(u)
	emit({k = "dead", f = Spring.GetGameFrame(), u = u, d = UnitDefs[d].name, t = team, x = x, z = z,
		bp = math.floor(100 * (bp or 1)), a = attacker, ad = attackerDef and UnitDefs[attackerDef] and UnitDefs[attackerDef].name,
		at = attackerTeam, ax = ax, az = az})
end

function widget:UnitGiven(u, d, newTeam, oldTeam)
	local x, z = pos(u)
	emit({k = "give", f = Spring.GetGameFrame(), u = u, d = UnitDefs[d].name, t = newTeam, from = oldTeam, x = x, z = z})
end

function widget:UnitCommand(u, d, team, cmdID, params, opts)
	if aiTeam[team] and cmdID >= 0 then return end
	local p = {}
	for i = 1, #(params or {}) do p[i] = r(params[i]) end
	local o = type(opts) == "table" and opts.coded or opts or 0
	local key = team .. "|" .. cmdID .. "|" .. table.concat(p, ",") .. "|" .. tostring(o)
	local c = cmdBuf[key]
	if not c then
		c = {t = team, c = cmdName(cmdID), p = p, o = o, d = {}, u = {}}
		cmdBuf[key] = c
	end
	c.u[#c.u + 1] = u
	c.d[#c.d + 1] = UnitDefs[d] and UnitDefs[d].name or "?"
end

function widget:UnitDamaged(u, d, team, damage, paralyzer, weaponDef, projectile, attacker, attackerDef, attackerTeam)
	if not damage or damage <= 0 then return end
	local x, _, z = Spring.GetUnitPosition(u)
	if not x then return end
	local ad = attackerDef and UnitDefs[attackerDef] and UnitDefs[attackerDef].name or "?"
	local key = (attackerTeam or -1) .. "|" .. ad .. "|" .. team .. "|" .. UnitDefs[d].name .. "|" ..
		math.floor(x / CELL) .. "|" .. math.floor(z / CELL)
	local v = dmgBuf[key]
	if not v then v = {0, 0}; dmgBuf[key] = v end
	if paralyzer then v[2] = v[2] + damage else v[1] = v[1] + damage end
end

function widget:AddConsoleLine(line)
	if line:sub(1, 1) == "<" or line:find("^%[") == nil and line:find("resigned") then
		emit({k = "chat", f = Spring.GetGameFrame(), msg = line})
	end
end

function widget:GameOver(winners)
	emit({k = "over", f = Spring.GetGameFrame(), winners = winners or {}})
	overAt = Spring.GetGameFrame()
end

function widget:Shutdown()
	if out then
		writeStats()
		emit({k = "end", f = Spring.GetGameFrame(), shutdown = true})
		out:close()
		out = nil
	end
end
