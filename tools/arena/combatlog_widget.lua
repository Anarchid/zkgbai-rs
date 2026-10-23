-- Combat log: every shot, what it was aimed at, the target's state when it
-- was fired and what it did; unit state while in combat; deaths and stuns.
-- Data for fitting hit and combat models (projectile hit probability by
-- target speed, distance and motion; EMP, slow, disarm, shields, fire).
--
-- Install with ARENA_WIDGETS for headless matches, or drop into
-- LuaUI/Widgets for a human client (as a player the engine hides projectile
-- ids and unseen attackers, so damage is matched to shots by attacker and
-- weapon instead; projectiles are only seen in LOS).
--
-- Writes <write-dir>/combatlog/<date>_<game>.tsv, tab-separated, one row
-- kind per first column, with a "#<kind>" header line per kind:
--   shot  one projectile (src=p), the shots of a hitscan weapon in one poll
--         period (src=s, n = how many, estimated for weapons firing faster
--         than the poll), or a projectile only seen by the damage it did
--         (src=d); meta weapon rows give each weapon's stats
--   dmg   damage that matched no shot (death explosions have no attacker)
--   burn  a fire tick (ZK fire gadget damage of a setunitsonfire weapon)
--   env   engine damage (collision, fall, fire, crush: Game.envDamageTypes)
--   kill  a unit died
--   stun  EMP stun on (1) or off (0)
--   unit  state of a unit in combat, every UNIT_SAMPLE frames
--   meta  game, map, teams
-- Positions are elmos, speeds elmos per frame, frames at 30 per second.
-- UnitDamaged for frame f is delivered after GameFrame(f); the matching
-- windows account for that (a projectile closes one poll after it vanishes).
-- Echoes "COMBATLOG rows=<n> shots=<n> file=<path>" at game over.
function widget:GetInfo()
	return {
		name    = "Combat Log",
		desc    = "Logs shots, hits and unit state for combat model fitting",
		author  = "zkgbai",
		date    = "2026-10-24",
		license = "GNU GPL, v2 or later",
		layer   = 0,
		enabled = true,
	}
end

local VERSION = 1
local UNIT_SAMPLE = 30     -- frames between unit rows
local ACTIVE_FRAMES = 600  -- a unit stays in combat this long after its last shot or wound
local SALVO_POLL = 6       -- longest poll period of a unit in combat (shorter for fast lasers)
local IDLE_POLL = 30       -- ... and of those that have not shot or been shot lately
local PROJ_POLL = 1        -- frames between projectile polls
local PENDING_FRAMES = 12  -- unmatched damage waits this long for its salvo
local PROJ_MATCH_FRAMES = 300 -- player view: damage matches a projectile this old at most
local FLUSH_FRAMES = 150

local sp = Spring
-- LuaUI gets GetAllProjectiles (and the sphere query), not the rectangle one.
local GetAllProjectiles = sp.GetAllProjectiles
local GetProjectilesInSphere = sp.GetProjectilesInSphere
local GetProjectileDefID = sp.GetProjectileDefID
local GetProjectileOwnerID = sp.GetProjectileOwnerID
local GetProjectileVelocity = sp.GetProjectileVelocity
local GetProjectileTarget = sp.GetProjectileTarget
local GetUnitPosition = sp.GetUnitPosition
local GetUnitVelocity = sp.GetUnitVelocity
local GetUnitHealth = sp.GetUnitHealth
local GetUnitIsStunned = sp.GetUnitIsStunned
local GetUnitRulesParam = sp.GetUnitRulesParam
local GetUnitShieldState = sp.GetUnitShieldState
local GetUnitDefID = sp.GetUnitDefID
local GetUnitTeam = sp.GetUnitTeam
local GetUnitWeaponState = sp.GetUnitWeaponState
local GetUnitWeaponTarget = sp.GetUnitWeaponTarget
local GetGameFrame = sp.GetGameFrame
local format = string.format
local sqrt = math.sqrt

local MAP_X, MAP_Z = Game.mapSizeX, Game.mapSizeZ

-- Per weapon def: name, whether it is hitscan (no projectile outlives the
-- frame it fires in: shots are detected from reload state instead), shots
-- per salvo.
local wdInfo = {}
-- Per unit def: list of {weaponNum, weaponDefID} for its hitscan weapons.
local udHitscan = {}
-- Engine damage ids -> names.
local envName = {}

local rows = {}        -- buffered output lines
local nrows, nshots = 0, 0
local file             -- path once chosen
local gameID
local headersWritten = false

local shots = {}       -- projectile id -> open shot record
local latestProj = {}  -- attacker id -> weapon def id -> projectile id (player view matching)
local pending = {}     -- attacker id -> weapon def id -> list of damage waiting for a salvo
local lastReload = {}  -- unit id -> weapon num -> last reloadFrame seen
local openSalvo = {}   -- unit id -> weapon num -> salvo record of the last poll period
local lastPolled = {}  -- unit id -> frame its reloads were last polled
local clock = os.clock
local spent = { proj = 0, salvo = 0, units = 0, dmg = 0 }  -- seconds of CPU per section
local pollable = {}    -- unit id -> udHitscan entry
local active = {}      -- unit id -> last combat frame
local stateCache = {}  -- unit id -> state sampled this frame
local cacheFrame = -1
local fullView = false -- spectator with full view: projectile ids are complete

local function num(v, digits)
	if v == nil then return "" end
	if digits then return format("%." .. digits .. "f", v) end
	return format("%d", v)
end

local function unitDefName(udid)
	local ud = udid and UnitDefs[udid]
	return ud and ud.name or ""
end

local function emit(line)
	nrows = nrows + 1
	rows[#rows + 1] = line
end

-- A unit's state this frame, cached per frame since many shots hit the same
-- unit at once. nil when the unit is not visible.
local function unitState(uid, f)
	if f ~= cacheFrame then
		stateCache = {}
		cacheFrame = f
	end
	local s = stateCache[uid]
	if s ~= nil then return s or nil end
	local x, y, z = GetUnitPosition(uid)
	if not x then
		stateCache[uid] = false
		return nil
	end
	local vx, vy, vz = GetUnitVelocity(uid)
	local hp, hpmax, para = GetUnitHealth(uid)
	local _, stunned = GetUnitIsStunned(uid)
	local shEnabled, shPower = GetUnitShieldState(uid)
	s = {
		x = x, z = z,
		spd = vx and sqrt(vx * vx + vz * vz) or 0,
		hp = hp, hpmax = hpmax, para = para,
		stun = stunned and 1 or 0,
		slow = GetUnitRulesParam(uid, "slowState") or 0,
		dis = GetUnitRulesParam(uid, "disarmed") or 0,
		sh = shEnabled ~= nil and shPower or nil,
		fire = GetUnitRulesParam(uid, "on_fire") or 0,
		def = GetUnitDefID(uid),
		team = GetUnitTeam(uid),
	}
	stateCache[uid] = s
	return s
end

local function touch(uid, f)
	if uid then active[uid] = f end
end

-- Columns of a shot row; src p/s/d as described above.
local SHOT_HEADER = "#shot\tsrc\tn\tf0\tf1\tlag\tpid\twd\town\todef\toteam\tox\toz\tospd\tpspd" ..
	"\ttt\ttid\ttdef\ttteam\ttx\ttz\ttdist\ttspd\tthp\tthpmax\ttpara\ttstun\ttslow\ttdis\ttsh\ttfire" ..
	"\thits\tdmg\tdmgt\tnvict\tpara\ttdead\ttspd1\ttsh1"

local function newShot(src, f, wdid, owner, f0)
	local rec = {
		src = src, n = 1, f0 = f0 or f, fseen = f, wd = wdid, own = owner,
		hits = 0, dmg = 0, dmgt = 0, nvict = 0, para = 0, victims = {},
	}
	if owner then
		local s = unitState(owner, f)
		if s then
			rec.odef, rec.oteam, rec.ox, rec.oz, rec.ospd = s.def, s.team, s.x, s.z, s.spd
		else
			rec.odef = GetUnitDefID(owner)
			rec.oteam = GetUnitTeam(owner)
		end
		touch(owner, f)
	end
	return rec
end

-- Record what a shot is aimed at: a unit (with its state now) or ground.
local function aimAt(rec, f, ttype, target)
	if ttype == "u" and target then
		rec.tt = "u"
		rec.tid = target
		local s = unitState(target, f)
		if s then
			rec.tdef, rec.tteam, rec.tx, rec.tz, rec.tspd = s.def, s.team, s.x, s.z, s.spd
			rec.thp, rec.thpmax, rec.tpara, rec.tstun = s.hp, s.hpmax, s.para, s.stun
			rec.tslow, rec.tdis, rec.tsh, rec.tfire = s.slow, s.dis, s.sh, s.fire
		else
			rec.tdef = GetUnitDefID(target)
			rec.tteam = GetUnitTeam(target)
		end
	elseif ttype == "g" and target then
		rec.tt = "g"
		rec.tx, rec.tz = target[1], target[3]
	else
		rec.tt = "n"
	end
	if rec.tx and rec.ox then
		local dx, dz = rec.tx - rec.ox, rec.tz - rec.oz
		rec.tdist = sqrt(dx * dx + dz * dz)
	end
end

local function addDamage(rec, f, vid, vdef, vteam, damage, paralyzer)
	rec.hits = rec.hits + 1
	rec.dmg = rec.dmg + damage
	if vid == rec.tid then rec.dmgt = rec.dmgt + damage end
	if not rec.victims[vid] then
		rec.victims[vid] = true
		rec.nvict = rec.nvict + 1
	end
	if paralyzer then rec.para = 1 end
	touch(vid, f)
end

local function closeShot(rec, f)
	local w = wdInfo[rec.wd]
	local tdead, tspd1, tsh1
	if rec.tt == "u" then
		local s = unitState(rec.tid, f)
		if s then
			tdead, tspd1, tsh1 = 0, s.spd, s.sh
		elseif GetUnitDefID(rec.tid) == nil then
			tdead = 1
		end
	end
	nshots = nshots + 1
	emit(table.concat({
		"shot", rec.src, num(rec.n), num(rec.f0), num(f), num(rec.fseen - rec.f0), num(rec.pid),
		w and w.name or tostring(rec.wd), num(rec.own), unitDefName(rec.odef), num(rec.oteam),
		num(rec.ox), num(rec.oz), num(rec.ospd, 2), num(rec.pspd, 2),
		rec.tt or "n", num(rec.tid), unitDefName(rec.tdef), num(rec.tteam), num(rec.tx), num(rec.tz),
		num(rec.tdist), num(rec.tspd, 2), num(rec.thp), num(rec.thpmax), num(rec.tpara), num(rec.tstun),
		num(rec.tslow, 3), num(rec.tdis), num(rec.tsh), num(rec.tfire),
		num(rec.hits), num(rec.dmg, 1), num(rec.dmgt, 1), num(rec.nvict), num(rec.para),
		num(tdead), num(tspd1, 2), num(tsh1),
	}, "\t"))
end

local function emitDamage(f, vid, vdef, vteam, damage, paralyzer, wdid, aid, adef, ateam, pid)
	local w = wdInfo[wdid]
	emit(table.concat({
		"dmg", num(f), num(vid), unitDefName(vdef), num(vteam), num(damage, 1), paralyzer and "1" or "0",
		w and w.name or tostring(wdid), num(aid), unitDefName(adef), num(ateam), num(pid),
	}, "\t"))
end

-------------------------------------------------------------------------------
-- Projectiles

local function projectileTarget(pid)
	local ttype, target = GetProjectileTarget(pid)
	if ttype == nil then return nil end
	if type(target) == "table" then return "g", target end
	if ttype == 117 or ttype == "u" or ttype == "unit" then return "u", target end -- 'u'
	return nil
end

local function openProjectile(pid, f)
	local wdid = GetProjectileDefID(pid)
	if not wdid then return end
	local w = wdInfo[wdid]
	if not w or w.hitscan then return end
	local owner = GetProjectileOwnerID(pid)
	local rec = shots[pid]
	if rec then
		-- Seen first by the damage it did (src=d): now the projectile itself.
		rec.src = "d"
	else
		rec = newShot("p", f, wdid, owner)
		rec.pid = pid
		shots[pid] = rec
	end
	local vx, vy, vz = GetProjectileVelocity(pid)
	if vx then rec.pspd = sqrt(vx * vx + vy * vy + vz * vz) end
	-- The projectile's own target beats the victim assumed by a src=d record.
	local ttype, target = projectileTarget(pid)
	if ttype or not rec.tt then aimAt(rec, f, ttype, target) end
	if owner then
		local lp = latestProj[owner]
		if not lp then lp = {}; latestProj[owner] = lp end
		lp[wdid] = pid
	end
end

local liveSet = {}
local gone = {}  -- projectiles that vanished at the last poll: closed at this one

local function pollProjectiles(f)
	local ids
	if GetAllProjectiles then
		ids = GetAllProjectiles(false, true)
	else
		ids = GetProjectilesInSphere(MAP_X / 2, 0, MAP_Z / 2, MAP_X + MAP_Z)
	end
	local now = {}
	for i = 1, #ids do
		local pid = ids[i]
		now[pid] = true
		if not liveSet[pid] then openProjectile(pid, f) end
	end
	for pid, rec in pairs(gone) do
		if shots[pid] == rec then
			closeShot(rec, rec.gone)
			shots[pid] = nil
		end
	end
	gone = {}
	for pid in pairs(liveSet) do
		if not now[pid] then
			local rec = shots[pid]
			if rec then
				rec.gone = f
				gone[pid] = rec
			end
		end
	end
	liveSet = now
end

-------------------------------------------------------------------------------
-- Hitscan salvos, detected from reload state

-- Damage this attacker's weapon did in frames [fromFrame, toFrame) goes to
-- the record. (UnitDamaged for frame f arrives after GameFrame(f), so a poll
-- at f has seen damage up to f-1.)
local function consumePending(aid, wdid, rec, fromFrame, toFrame)
	local pa = pending[aid]
	local list = pa and pa[wdid]
	if not list then return end
	local keep = {}
	for i = 1, #list do
		local d = list[i]
		if d.f >= fromFrame and d.f < toFrame then
			addDamage(rec, d.f, d.vid, d.vdef, d.vteam, d.dmg, d.para)
		else
			keep[#keep + 1] = d
		end
	end
	pa[wdid] = #keep > 0 and keep or nil
end

local function weaponTarget(uid, num)
	local ttype, _, target = GetUnitWeaponTarget(uid, num)
	if ttype == 1 and target then return "u", target end
	if ttype == 2 and type(target) == "table" then return "g", target end
	return nil
end

local function closeOpenSalvos(uid, f)
	local open = openSalvo[uid]
	if not open then return end
	for wnum, rec in pairs(open) do
		closeShot(rec, f)
	end
	openSalvo[uid] = nil
end

-- Hitscan weapons leave no projectile to see, so shots are read off the
-- reload state: reloadFrame = frame of the last shot + reload time. Each
-- poll period yields one record for the shots fired in it (n estimated from
-- the reload advance for weapons that fire faster than the poll). A beam's
-- damage ticks can trail the shot (beamTime), so the record stays open until
-- the next poll and takes the period's damage if the weapon fired no more.
-- The first damage this attacker's weapon did in [fromFrame, toFrame).
local function firstPending(aid, wdid, fromFrame, toFrame)
	local pa = pending[aid]
	local list = pa and pa[wdid]
	if not list then return nil end
	for i = 1, #list do
		local d = list[i]
		if d.f >= fromFrame and d.f < toFrame then return d end
	end
	return nil
end

-- Hitscan weapons leave no projectile to see, so shots are read off the
-- reload state: reloadFrame = frame of the last shot + reload time. A unit
-- in combat is polled every few frames (its shortest hitscan reload, so a
-- continuous laser yields one shot per poll), an idle one every IDLE_POLL.
-- Each poll yields one record for the shots fired since the last one, with
-- the period's damage; a beam's damage ticks can trail the shot (beamTime),
-- so the record stays open until the next poll and takes that period's
-- damage if the weapon fired no more. A reload advance without a target
-- (commander module slots churn) is not a shot, unless damage says so.
local function pollSalvos(f)
	for uid, weapons in pairs(pollable) do
		local since = lastPolled[uid]
		local hot = active[uid] and f - active[uid] <= ACTIVE_FRAMES
		if since == nil or f - since >= (hot and weapons.period or IDLE_POLL) then
			local lastPoll = since or (f - 1)
			lastPolled[uid] = f
			local last = lastReload[uid]
			if not last then last = {}; lastReload[uid] = last end
			local open = openSalvo[uid]
			for i = 1, #weapons do
				local wnum, wdid = weapons[i][1], weapons[i][2]
				local rf = GetUnitWeaponState(uid, wnum, "reloadFrame")
				if rf then
					local prev = last[wnum]
					local prevRec = open and open[wnum]
					local ttype, target
					if prev and rf > prev then
						ttype, target = weaponTarget(uid, wnum)
						if not ttype then
							local d = firstPending(uid, wdid, lastPoll, f)
							if d then ttype, target = "u", d.vid end
						end
					end
					if ttype then
						local info = wdInfo[wdid]
						local reload = GetUnitWeaponState(uid, wnum, "reloadTime") or info.reload
						local rframes = math.max(1, math.floor(reload * 30 + 0.5))
						local f0 = rf - rframes
						if f0 > f then f0 = f end
						-- Shots this period: one if the weapon was idle at the last
						-- poll (when it started is unknown), else the reload advance,
						-- at most what fits in the period.
						local n = 1
						if prev > lastPoll then
							n = math.floor((rf - prev) / rframes + 0.5)
							local cap = math.ceil((f - lastPoll) / rframes)
							if n > cap then n = cap end
							if n < 1 then n = 1 end
						end
						active[uid] = f
						local rec = newShot("s", f, wdid, uid, f0)
						rec.n = n * info.salvo
						aimAt(rec, f, ttype, target)
						consumePending(uid, wdid, rec, lastPoll, f)
						if prevRec then closeShot(prevRec, f) end
						if not open then open = {}; openSalvo[uid] = open end
						open[wnum] = rec
					elseif prevRec then
						consumePending(uid, wdid, prevRec, lastPoll, f)
						closeShot(prevRec, f)
						open[wnum] = nil
					end
					last[wnum] = rf
				end
			end
			if open and next(open) == nil then openSalvo[uid] = nil end
		end
	end
end

local function flushPending(f)
	for aid, pa in pairs(pending) do
		for wdid, list in pairs(pa) do
			local keep = {}
			for i = 1, #list do
				local d = list[i]
				if f - d.f > PENDING_FRAMES then
					emitDamage(d.f, d.vid, d.vdef, d.vteam, d.dmg, d.para, wdid, aid, d.adef, d.ateam, d.pid)
				else
					keep[#keep + 1] = d
				end
			end
			pa[wdid] = #keep > 0 and keep or nil
		end
		if next(pa) == nil then pending[aid] = nil end
	end
end

-------------------------------------------------------------------------------
-- Units

local function trackUnit(uid, udid)
	local hs = udHitscan[udid]
	if hs then pollable[uid] = hs end
end

local function forgetUnit(uid)
	pollable[uid] = nil
	lastReload[uid] = nil
	latestProj[uid] = nil
	active[uid] = nil
end

local function sampleUnits(f)
	for uid, last in pairs(active) do
		if f - last > ACTIVE_FRAMES then
			active[uid] = nil
		else
			local s = unitState(uid, f)
			if s then
				emit(table.concat({
					"unit", num(f), num(uid), unitDefName(s.def), num(s.team), num(s.x), num(s.z),
					num(s.hp), num(s.hpmax), num(s.para), num(s.stun), num(s.slow, 3), num(s.dis),
					num(s.sh), num(s.fire), num(s.spd, 2),
				}, "\t"))
			end
		end
	end
end

-------------------------------------------------------------------------------
-- Output

local HEADERS = {
	SHOT_HEADER,
	"#dmg\tf\tvid\tvdef\tvteam\tdmg\tpara\twd\taid\tadef\tateam\tpid",
	"#burn\tf\tvid\tvdef\tvteam\tdmg\twd\taid\tadef\tateam",
	"#env\tf\tvid\tvdef\tvteam\tdmg\tkind\taid\tadef\tateam",
	"#kill\tf\tuid\tudef\tuteam\tx\tz\taid\tadef\tateam",
	"#stun\tf\tuid\tudef\tuteam\ton",
	"#unit\tf\tuid\tudef\tuteam\tx\tz\thp\thpmax\tpara\tstun\tslow\tdis\tsh\tfire\tspd",
	"#meta\tkey\tvalue...",
}

local function flush()
	if #rows == 0 and headersWritten then return end
	if not file then
		sp.CreateDir("combatlog")
		local id = gameID and gameID:sub(1, 8) or format("%d", os.time() % 100000)
		file = format("combatlog/%s_%s.tsv", os.date("%Y%m%d-%H%M%S"), id)
	end
	local fh = io.open(file, "a")
	if not fh then return end
	if not headersWritten then
		fh:write("# combatlog v", VERSION, "\n", table.concat(HEADERS, "\n"), "\n")
		headersWritten = true
	end
	if #rows > 0 then
		fh:write(table.concat(rows, "\n"), "\n")
		rows = {}
	end
	fh:close()
end

local function meta(...)
	emit("meta\t" .. table.concat({...}, "\t"))
end

local function writeMeta()
	local spec, fullView = sp.GetSpectatingState()
	meta("version", VERSION)
	meta("game", gameID or "")
	meta("map", Game.mapName)
	meta("mod", Game.gameName or "", Game.gameVersion or "")
	meta("engine", Engine and Engine.versionFull or Game.version)
	meta("view", spec and 1 or 0, fullView and 1 or 0, sp.GetMyTeamID())
	meta("date", os.date("%Y-%m-%d %H:%M:%S"))
	meta("weaponcols", "name", "type", "reload", "salvo", "beam", "range", "pspeed", "aoe", "edge", "damage", "paralyzer", "slow", "disarm", "fire")
	for _, w in pairs(wdInfo) do
		if w.damage > 0 or w.paralyzer == 1 then
			meta("weapon", w.name, w.type, num(w.reload, 2), num(w.salvo), num(w.beam, 2), num(w.range), num(w.pspeed, 1),
				num(w.aoe), num(w.edge, 2), num(w.damage, 1), num(w.paralyzer), num(w.slow, 2), num(w.disarm, 2), num(w.fire))
		end
	end
	for _, team in ipairs(sp.GetTeamList()) do
		local _, leader, _, isAI, side, ally = sp.GetTeamInfo(team, false)
		local who = ""
		if isAI then
			local _, name, _, short, version = sp.GetAIInfo(team)
			who = format("%s:%s (%s)", short or "", version or "", name or "")
		elseif leader and leader >= 0 then
			who = sp.GetPlayerInfo(leader, false) or ""
		end
		meta("team", team, ally, isAI and "ai" or "player", who)
	end
end

-------------------------------------------------------------------------------
-- Callins

function widget:Initialize()
	for wdid, wd in pairs(WeaponDefs) do
		local t = wd.type
		local cp = wd.customParams or {}
		wdInfo[wdid] = {
			name = wd.name,
			type = t,
			beam = wd.beamtime or 0,
			range = wd.range,
			pspeed = wd.projectilespeed,
			aoe = wd.damageAreaOfEffect,
			edge = wd.edgeEffectiveness,
			damage = wd.damages and wd.damages[0] or 0,
			paralyzer = wd.paralyzer and 1 or 0,
			slow = cp.timeslow_damagefactor or 0,
			disarm = cp.disarmdamagemult or (cp.disarmdamageonly and 1) or 0,
			fire = cp.setunitsonfire and 1 or 0,
			hitscan = t == "BeamLaser" or t == "LightningCannon" or t == "Rifle",
			reload = wd.reload or 1,
			salvo = math.max(1, (wd.salvoSize or 1) * (wd.projectiles or 1)),
		}
	end
	for udid, ud in pairs(UnitDefs) do
		local list
		for i, w in ipairs(ud.weapons) do
			local wi = wdInfo[w.weaponDef]
			if wi and wi.hitscan then
				list = list or { period = SALVO_POLL }
				list[#list + 1] = { i, w.weaponDef }
				local rframes = math.max(1, math.floor(wi.reload * 30 + 0.5))
				if rframes < list.period then list.period = rframes end
			end
		end
		udHitscan[udid] = list
	end
	if Game.envDamageTypes then
		for name, id in pairs(Game.envDamageTypes) do envName[id] = name end
	end
	for _, uid in ipairs(sp.GetAllUnits()) do
		trackUnit(uid, GetUnitDefID(uid))
	end
	local spec, full = sp.GetSpectatingState()
	fullView = spec and full or false
	if GetGameFrame() > 0 then writeMeta() end
end

function widget:GameID(id)
	gameID = id
end

function widget:GameStart()
	writeMeta()
end

function widget:GameFrame(f)
	local t0 = clock()
	if f % PROJ_POLL == 0 then pollProjectiles(f) end
	local t1 = clock()
	pollSalvos(f)
	if f % SALVO_POLL == 0 then flushPending(f) end
	local t2 = clock()
	if f % UNIT_SAMPLE == 0 then sampleUnits(f) end
	local t3 = clock()
	spent.proj, spent.salvo, spent.units = spent.proj + (t1 - t0), spent.salvo + (t2 - t1), spent.units + (t3 - t2)
	if f % FLUSH_FRAMES == 0 then flush() end
end

local function unitDamaged(uid, udid, team, damage, paralyzer, wdid, pid, aid, adef, ateam)
	local f = GetGameFrame()
	touch(uid, f)
	if wdid == nil or wdid < 0 then
		emit(table.concat({
			"env", num(f), num(uid), unitDefName(udid), num(team), num(damage, 1),
			envName[wdid] or tostring(wdid), num(aid), unitDefName(adef), num(ateam),
		}, "\t"))
		return
	end
	touch(aid, f)
	local w = wdInfo[wdid]
	if w and w.fire == 1 and (pid == nil or pid < 0) then
		emit(table.concat({ "burn", num(f), num(uid), unitDefName(udid), num(team), num(damage, 1), w.name, num(aid), unitDefName(adef), num(ateam) }, "\t"))
		return
	end
	if w and not w.hitscan then
		if pid and pid >= 0 then
			local rec = shots[pid]
			if not rec then
				-- Hit before any poll saw it: a projectile that died the frame it
				-- was fired, or (Lance) one that goes on through its victims.
				rec = newShot("d", f, wdid, aid)
				rec.pid = pid
				aimAt(rec, f, "u", uid)
				shots[pid] = rec
				if aid then
					local lp = latestProj[aid]
					if not lp then lp = {}; latestProj[aid] = lp end
					lp[wdid] = pid
				end
			end
			addDamage(rec, f, uid, udid, team, damage, paralyzer)
			return
		end
		-- Player view: no projectile id. The attacker's latest projectile of
		-- this weapon, if it is still in flight. (With full view a hit without
		-- an id is damage the game applied itself: slow, burn.)
		if aid and not fullView then
			local lp = latestProj[aid]
			local lpid = lp and lp[wdid]
			local rec = lpid and shots[lpid]
			if rec and f - rec.f0 <= PROJ_MATCH_FRAMES then
				addDamage(rec, f, uid, udid, team, damage, paralyzer)
				return
			end
		end
	end
	if aid then
		-- Hitscan: the salvo is detected at the next reload poll and takes
		-- this; otherwise it flushes as unmatched.
		local pa = pending[aid]
		if not pa then pa = {}; pending[aid] = pa end
		local list = pa[wdid]
		if not list then list = {}; pa[wdid] = list end
		list[#list + 1] = { f = f, vid = uid, vdef = udid, vteam = team, dmg = damage, para = paralyzer, adef = adef, ateam = ateam, pid = pid }
	else
		emitDamage(f, uid, udid, team, damage, paralyzer, wdid, nil, adef, ateam, pid)
	end
end

function widget:UnitDamaged(...)
	local t0 = clock()
	unitDamaged(...)
	spent.dmg = spent.dmg + (clock() - t0)
end

function widget:UnitDestroyed(uid, udid, team, aid, adef, ateam)
	local f = GetGameFrame()
	local x, _, z = GetUnitPosition(uid)
	emit(table.concat({
		"kill", num(f), num(uid), unitDefName(udid), num(team), num(x), num(z),
		num(aid), unitDefName(adef), num(ateam),
	}, "\t"))
	closeOpenSalvos(uid, f)
	forgetUnit(uid)
end

function widget:UnitStunned(uid, udid, team, stunned)
	emit(table.concat({ "stun", num(GetGameFrame()), num(uid), unitDefName(udid), num(team), stunned and "1" or "0" }, "\t"))
end

function widget:UnitCreated(uid, udid)
	trackUnit(uid, udid)
end

function widget:UnitEnteredLos(uid, team, allyTeam, udid)
	trackUnit(uid, udid or GetUnitDefID(uid))
end

function widget:UnitLeftLos(uid)
	-- Keep polling: reload state reads nil out of LOS and resumes on return.
end

function widget:UnitTaken(uid, udid)
	trackUnit(uid, udid)
end

local function summary()
	sp.Echo(format("COMBATLOG rows=%d shots=%d file=%s cpu proj=%.1fs salvo=%.1fs units=%.1fs dmg=%.1fs",
		nrows, nshots, file or "", spent.proj, spent.salvo, spent.units, spent.dmg))
end

function widget:GameOver()
	local f = GetGameFrame()
	for pid, rec in pairs(shots) do closeShot(rec, f) end
	shots = {}
	for uid in pairs(openSalvo) do closeOpenSalvos(uid, f) end
	flush()
	summary()
end

function widget:Shutdown()
	flush()
	summary()
end
