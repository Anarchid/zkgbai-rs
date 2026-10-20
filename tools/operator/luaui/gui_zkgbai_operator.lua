--------------------------------------------------------------------------------
-- ZKGBAI operator overlay: what the ZKGBAI instances on this machine think,
-- drawn into the game, and their units steerable from it.
--
-- It reads the AI's operator API (HTTP on 127.0.0.1, ports 7460-7467; run
-- the AI with the AI option operator=on, or ZKGBAI_OPERATOR=on) over LuaSocket
-- without blocking: requests are pumped from Update and GameFrame.
--
-- Shown for the AI in focus (the panel's "AI" button cycles, or /luaui zkop
-- focus <team>): its squads and where they go, places it defends, where its
-- other units mean to go and why, its memory of the enemy (where it last saw
-- them, how long ago, and the reach of remembered defenses), its view of the
-- metal spots, a threat or value map on the ground, and its log.
--
-- Steering (as a spectator, or for an AI ally): select the AI's units and
-- right-click: move; ctrl: attack-move; on a unit: attack or guard; shift
-- queues; alt: send their whole squad there; ctrl+alt: a place to defend.
-- Units ordered so are held by the operator (the AI leaves them alone) until
-- released (panel button, or /luaui zkop release).
--
-- Text commands (/luaui zkop ...): focus <team>, layer <name|none>, release,
-- build <unitdef> (at the mouse), fac <unitdef> [count], defend (at the
-- mouse), say <text>, note <text>, feature <name> on|off, toggle <layer>.
--------------------------------------------------------------------------------

function widget:GetInfo()
	return {
		name    = "ZKGBAI Operator",
		desc    = "Shows what ZKGBAI AIs think (squads, intents, memory of the enemy, threat maps, log) and lets you steer them. Needs the AI's operator API (operator=on).",
		author  = "ZKGBAI",
		date    = "2026-10-20",
		license = "GNU GPL, v2 or later",
		layer   = 10,
		enabled = true,
	}
end

local HOST = "127.0.0.1"
local FIRST_PORT, LAST_PORT = 7460, 7467
local REQUEST_TIMEOUT = 6     -- seconds
local DEBUG_ECHO = false      -- periodic status lines in the infolog (tests)

local spEcho               = Spring.Echo
local spGetTimer           = Spring.GetTimer
local spDiffTimers         = Spring.DiffTimers
local spGetGroundHeight    = Spring.GetGroundHeight
local spGetUnitPosition    = Spring.GetUnitPosition
local spGetUnitTeam        = Spring.GetUnitTeam
local spGetSelectedUnits   = Spring.GetSelectedUnits
local spGetSpectatingState = Spring.GetSpectatingState
local spGetMyTeamID        = Spring.GetMyTeamID
local spAreTeamsAllied     = Spring.AreTeamsAllied
local spTraceScreenRay     = Spring.TraceScreenRay
local spGetModKeyState     = Spring.GetModKeyState
local spGetCameraPosition  = Spring.GetCameraPosition
local spGetTeamColor       = Spring.GetTeamColor
local spGetMouseState      = Spring.GetMouseState
local spIsSphereInView     = Spring.IsSphereInView

local glColor      = gl.Color
local glLineWidth  = gl.LineWidth
local glBeginEnd   = gl.BeginEnd
local glVertex     = gl.Vertex
local glText       = gl.Text
local glRect       = gl.Rect
local glPushMatrix = gl.PushMatrix
local glPopMatrix  = gl.PopMatrix
local glTranslate  = gl.Translate
local glBillboard  = gl.Billboard
local glDepthTest  = gl.DepthTest
local glDrawGroundCircle = gl.DrawGroundCircle
local GL_LINES     = GL.LINES
local GL_QUADS     = GL.QUADS
local GL_LINE_LOOP = GL.LINE_LOOP

local floor, max, min, sqrt, sin = math.floor, math.max, math.min, math.sqrt, math.sin
local format = string.format

local json
local startTimer
local function clock()
	return spDiffTimers(spGetTimer(), startTimer)
end

-- Colors (the dashboard's palette, dark variant).
local ACCENT = {0.10, 0.62, 0.44}   -- squads, the operator's own marks
local ENEMY  = {0.90, 0.40, 0.40}
local OURS   = {0.22, 0.53, 0.90}
local HEAT   = {0.85, 0.35, 0.15}
local INK    = {0.94, 0.94, 0.93}
local MUTED  = {0.54, 0.53, 0.51}

--------------------------------------------------------------------------------
-- HTTP, without blocking
--------------------------------------------------------------------------------

local requests = {}

local function httpRequest(port, method, path, body, callback)
	local conn = socket.tcp()
	if not conn then
		callback(nil, "no socket")
		return
	end
	conn:settimeout(0)
	conn:connect(HOST, port)
	local out = method .. " " .. path .. " HTTP/1.1\r\nHost: " .. HOST .. ":" .. port .. "\r\nConnection: close\r\n"
	if body then
		out = out .. "Content-Type: application/json\r\nContent-Length: " .. #body .. "\r\n\r\n" .. body
	else
		out = out .. "\r\n"
	end
	requests[#requests + 1] = {conn = conn, out = out, sent = 0, parts = {}, callback = callback, started = clock()}
end

local function parseResponse(raw)
	local headEnd = raw:find("\r\n\r\n", 1, true)
	if not headEnd then
		return nil, "bad reply"
	end
	local status = tonumber(raw:match("^HTTP/%d%.%d (%d+)")) or 0
	local ok, value = pcall(json.decode, raw:sub(headEnd + 4))
	if not ok then
		return nil, "bad JSON"
	end
	if status ~= 200 then
		return nil, (type(value) == "table" and value.error) or ("HTTP " .. status)
	end
	return value
end

local function pumpRequests()
	if #requests == 0 then
		return
	end
	local now = clock()
	for i = #requests, 1, -1 do
		local r = requests[i]
		local done, err = false, nil
		if r.sent < #r.out then
			local _, writable = socket.select(nil, {r.conn}, 0)
			if writable and #writable > 0 then
				local n, e, partial = r.conn:send(r.out, r.sent + 1)
				if n then
					r.sent = n
				elseif e == "timeout" then
					r.sent = partial or r.sent
				else
					done, err = true, e
				end
			end
		end
		if not done and r.sent >= #r.out then
			local data, e, partial = r.conn:receive("*a")
			if data then
				r.parts[#r.parts + 1] = data
				done = true
			elseif e == "timeout" then
				if partial and partial ~= "" then
					r.parts[#r.parts + 1] = partial
				end
			elseif e == "closed" then
				if partial and partial ~= "" then
					r.parts[#r.parts + 1] = partial
				end
				done = true
			else
				done, err = true, e
			end
		end
		if not done and now - r.started > REQUEST_TIMEOUT then
			done, err = true, "timeout"
		end
		if done then
			r.conn:close()
			table.remove(requests, i)
			local value
			if not err then
				value, err = parseResponse(table.concat(r.parts))
			end
			local ok, msg = pcall(r.callback, value, err)
			if not ok then
				spEcho("ZKGBAI Operator: " .. tostring(msg))
			end
		end
	end
end

--------------------------------------------------------------------------------
-- The AI instances
--------------------------------------------------------------------------------

-- By port: {port, team, allyTeam, version, fails, busy = {}, due = {}, snap,
-- status, spots, feed, nextSeq, overlay, overlayList}
local instances = {}
local probing = {}
local nextProbe = 0
local focus -- port of the AI shown
local focusChosen = false -- picked by hand

local show = {squads = true, intents = true, memory = true, spots = true, roles = true, defend = true, feed = true}
local LAYERS = {"none", "threat", "enemy_porc", "aa_threat", "sub_threat", "value", "ally_threat", "riot"}
local layer = "none"

local flashes = {}  -- feedback marks on the ground: {x, z, t, color}
local message, messageTime, messageBad

local function say(text, bad)
	message, messageTime, messageBad = text, clock(), bad
end

local function teamColor(team)
	local r, g, b = spGetTeamColor(team)
	return {r or 1, g or 1, b or 1}
end

local function focused()
	return focus and instances[focus]
end

local function sortedPorts()
	local ports = {}
	for port in pairs(instances) do
		ports[#ports + 1] = port
	end
	table.sort(ports)
	return ports
end

local function poll(inst, kind, period, path, handler)
	local now = clock()
	if inst.busy[kind] or (inst.due[kind] or 0) > now then
		return
	end
	inst.busy[kind] = true
	inst.due[kind] = now + period
	httpRequest(inst.port, "GET", path, nil, function(value, err)
		inst.busy[kind] = false
		if err then
			inst.fails = inst.fails + 1
			inst.lastError = err
		else
			inst.fails = 0
			handler(value)
		end
	end)
end

local function act(inst, tool, args, label, onDone)
	httpRequest(inst.port, "POST", "/call/" .. tool, json.encode(args or {}), function(value, err)
		if err then
			say((label or tool) .. ": " .. tostring(err), true)
		else
			say((label or tool) .. ": done")
			if onDone then
				onDone(value)
			end
		end
	end)
end

local function dropInstance(port)
	local inst = instances[port]
	if inst and inst.overlayList then
		gl.DeleteList(inst.overlayList)
	end
	instances[port] = nil
	if focus == port then
		focus = sortedPorts()[1]
	end
	spEcho(format("ZKGBAI Operator: lost the AI on port %d", port))
end

local function discover()
	local now = clock()
	if now < nextProbe then
		return
	end
	nextProbe = now + 5
	for port = FIRST_PORT, LAST_PORT do
		if not instances[port] and not probing[port] then
			probing[port] = true
			httpRequest(port, "GET", "/health", nil, function(value)
				probing[port] = nil
				if type(value) == "table" and value.name == "zkgbai" and value.team then
					instances[port] = {
						port = port, team = value.team, allyTeam = value.ally_team, version = value.version,
						fails = 0, busy = {}, due = {}, feed = {}, color = teamColor(value.team),
					}
					spEcho(format("ZKGBAI Operator: found the AI of team %d on port %d (%s)", value.team, port, tostring(value.version)))
					-- Show our ally AI if we play, else the lowest team.
					if not focusChosen then
						local function rank(inst)
							local ally = not spGetSpectatingState() and spAreTeamsAllied(inst.team, spGetMyTeamID())
							return (ally and 0 or 1000) + inst.team
						end
						if not focus or rank(instances[port]) < rank(instances[focus]) then
							focus = port
						end
					end
				end
			end)
		end
	end
end

-- A journal record as a line of the feed (log lines read "module: ...").
local function feedText(e)
	local k = e.k
	local def = (e.def and e.def ~= "") and e.def or "?"
	if k == "log" then
		return e.msg
	elseif k == "kill" then
		return format("killed %s #%s%s", def, tostring(e.id), e.by and (" by " .. e.by) or "")
	elseif k == "unit-" then
		return format("lost %s #%s%s", def, tostring(e.id), e.by and (" to " .. e.by) or "")
	elseif k == "enemy+" then
		return format("spotted %s #%s", def, tostring(e.id))
	elseif k == "op" then
		if e.tool then
			return format("operator: %s %s", e.tool, e.ok and "ok" or ("failed: " .. tostring(e.error)))
		end
		return format("operator: %s %s", tostring(e.event), tostring(e.unit or e.factory or ""))
	elseif k == "note" then
		return "note: " .. tostring(e.text)
	elseif k == "say" then
		return "said: " .. tostring(e.text)
	elseif k == "idle" then
		return format("operator: held %s #%s is idle", def, tostring(e.id))
	end
	return nil
end

local function buildOverlay(inst, o)
	if inst.overlayList then
		gl.DeleteList(inst.overlayList)
		inst.overlayList = nil
	end
	if not o or not o.rows then
		return
	end
	local cell, maxv = o.cell, max(1, o.max or 1)
	-- Map cells are centered on multiples of 64 elmos.
	local sub = max(1, floor(cell / 64))
	local step = cell / sub
	inst.overlayList = gl.CreateList(function()
		glBeginEnd(GL_QUADS, function()
			for y = 1, o.height do
				local row = o.rows[y]
				for x = 1, o.width do
					local v = row and row[x] or 0
					if v > 0 then
						glColor(HEAT[1], HEAT[2], HEAT[3], 0.12 + 0.38 * min(1, sqrt(v / maxv)))
						local x0, z0 = (x - 1) * cell - 32, (y - 1) * cell - 32
						for i = 0, sub - 1 do
							for j = 0, sub - 1 do
								local ax, az = x0 + i * step, z0 + j * step
								local bx, bz = ax + step, az + step
								glVertex(ax, spGetGroundHeight(ax, az) + 4, az)
								glVertex(bx, spGetGroundHeight(bx, az) + 4, az)
								glVertex(bx, spGetGroundHeight(bx, bz) + 4, bz)
								glVertex(ax, spGetGroundHeight(ax, bz) + 4, bz)
							end
						end
					end
				end
			end
		end)
	end)
end

local lastEcho = 0
local function pollInstances()
	for port, inst in pairs(instances) do
		if inst.fails >= 4 then
			dropInstance(port)
		else
			poll(inst, "snap", 1, "/snapshot", function(v)
				if type(v) == "table" and v.units then
					inst.snap = v
					inst.byId = {}
					for i = 1, #v.units do
						local u = v.units[i]
						inst.byId[u[1]] = u
					end
				end
			end)
			if port == focus then
				if not inst.spots then
					poll(inst, "map", 10, "/call/map_info?heights=false", function(v)
						inst.spots = v.spots
					end)
				end
				poll(inst, "status", 3, "/call/status", function(v)
					inst.status = v
				end)
				local since = inst.nextSeq and ("&since=" .. inst.nextSeq) or ""
				poll(inst, "events", 1, "/call/events?limit=60" .. since, function(v)
					inst.nextSeq = v.next
					local now = clock()
					for i = 1, #(v.events or {}) do
						local text = feedText(v.events[i])
						if text then
							local feed = inst.feed
							feed[#feed + 1] = {text = text, t = now, f = v.events[i].f}
							if #feed > 60 then
								table.remove(feed, 1)
							end
						end
					end
				end)
				if layer ~= "none" then
					poll(inst, "overlay", 3, "/call/threat_map?max_size=128&layer=" .. layer, function(v)
						inst.overlay = v
						inst.overlayDirty = true
					end)
				end
			end
		end
	end
	if DEBUG_ECHO and clock() - lastEcho > 20 then
		lastEcho = clock()
		for port, inst in pairs(instances) do
			if inst.status and not inst.greeted then
				inst.greeted = true
				act(inst, "note", {text = "widget connected", tag = "widget"}, "note")
			end
			local s = inst.snap
			spEcho(format("ZKGBAI Operator: port %d team %d frame %s units %d enemies %d squads %d feed %d status %s overlay %s fails %d",
				port, inst.team, s and s.frame or -1, s and #s.units or 0, s and #s.enemies or 0, s and #s.squads or 0,
				#inst.feed, inst.status and inst.status.state or "-", inst.overlay and inst.overlay.layer or "-", inst.fails))
		end
	end
end

--------------------------------------------------------------------------------
-- Steering
--------------------------------------------------------------------------------

-- The selected units the operator may order, by AI instance; nil if the
-- selection holds units no AI here owns (then the engine handles the click).
local function steerableSelection()
	local sel = spGetSelectedUnits()
	if not sel or #sel == 0 then
		return nil
	end
	local spectating = spGetSpectatingState()
	local myTeam = spGetMyTeamID()
	local byPort = {}
	for i = 1, #sel do
		local team = spGetUnitTeam(sel[i])
		local inst
		for _, candidate in pairs(instances) do
			if candidate.team == team then
				inst = candidate
			end
		end
		if not inst or (team == myTeam and not spectating) then
			return nil
		end
		local list = byPort[inst.port]
		if not list then
			list = {inst = inst, units = {}}
			byPort[inst.port] = list
		end
		list.units[#list.units + 1] = sel[i]
	end
	return byPort
end

local function flash(x, z, color)
	flashes[#flashes + 1] = {x = x, z = z, t = clock(), color = color or ACCENT}
end

local function mouseGround()
	local mx, my = spGetMouseState()
	local kind, pos = spTraceScreenRay(mx, my, true)
	if kind == "ground" and pos then
		return pos[1], pos[3]
	end
end

local function steer(byPort, mx, my)
	local alt, ctrl, _, shift = spGetModKeyState()
	local kind, what = spTraceScreenRay(mx, my)
	local gx, gz
	if kind == "ground" then
		gx, gz = what[1], what[3]
	elseif kind == "unit" or kind == "feature" then
		local _, gpos = spTraceScreenRay(mx, my, true)
		if gpos then
			gx, gz = gpos[1], gpos[3]
		end
	end
	for _, group in pairs(byPort) do
		local inst, units = group.inst, group.units
		if alt and ctrl and gx then
			act(inst, "defend", {x = gx, z = gz, seconds = 60}, "defend")
			flash(gx, gz, ENEMY)
		elseif alt and gx then
			local squad
			for i = 1, #units do
				local row = inst.byId and inst.byId[units[i]]
				squad = squad or (row and row[7])
			end
			if squad then
				act(inst, "squad_target", {squad = squad, x = gx, z = gz, seconds = 60}, "squad " .. squad)
				flash(gx, gz, ACCENT)
			else
				say("those units are in no squad", true)
			end
		elseif kind == "unit" then
			local target = what
			local allied = spAreTeamsAllied(spGetUnitTeam(target) or -1, inst.team)
			act(inst, "order", {units = units, cmd = allied and "guard" or "attack", target = target, queue = shift}, allied and "guard" or "attack")
		elseif gx then
			act(inst, "order", {units = units, cmd = ctrl and "fight" or "move", x = gx, z = gz, queue = shift}, ctrl and "attack-move" or "move")
			flash(gx, gz, OURS)
		end
	end
end

local function releaseSelected()
	local byPort = steerableSelection()
	if byPort then
		for _, group in pairs(byPort) do
			act(group.inst, "release", {units = group.units}, "release")
		end
	else
		local inst = focused()
		if inst then
			act(inst, "release", {all = true}, "release all")
		end
	end
end

function widget:TextCommand(command)
	local words = {}
	for w in command:gmatch("%S+") do
		words[#words + 1] = w
	end
	if words[1] ~= "zkop" then
		return false
	end
	local cmd, inst = words[2], focused()
	local rest = table.concat(words, " ", 3)
	if cmd == "focus" then
		for port, candidate in pairs(instances) do
			if candidate.team == tonumber(words[3]) then
				focus, focusChosen = port, true
			end
		end
	elseif cmd == "layer" then
		layer = words[3] or "none"
	elseif cmd == "toggle" and words[3] and show[words[3]] ~= nil then
		show[words[3]] = not show[words[3]]
	elseif cmd == "release" then
		releaseSelected()
	elseif not inst then
		say("no ZKGBAI with the operator on found", true)
	elseif cmd == "build" and words[3] then
		local x, z = mouseGround()
		if x then
			act(inst, "build", {unit = words[3], x = x, z = z}, "build " .. words[3])
			flash(x, z, ACCENT)
		end
	elseif cmd == "defend" then
		local x, z = mouseGround()
		if x then
			act(inst, "defend", {x = x, z = z, seconds = tonumber(words[3]) or 60}, "defend")
			flash(x, z, ENEMY)
		end
	elseif cmd == "fac" and words[3] then
		act(inst, "fac_queue", {unit = words[3], count = tonumber(words[4]) or 1}, "fac_queue")
	elseif cmd == "say" then
		act(inst, "say", {text = rest}, "say")
	elseif cmd == "note" then
		act(inst, "note", {text = rest, tag = "widget"}, "note")
	elseif cmd == "feature" and words[3] then
		act(inst, "feature", {name = words[3], off = words[4] ~= "on"}, "feature " .. words[3])
	else
		say("zkop: focus <team> | layer <name> | toggle <what> | release | build <def> | defend | fac <def> [n] | say | note | feature <name> on|off", true)
	end
	return true
end

--------------------------------------------------------------------------------
-- Drawing in the world
--------------------------------------------------------------------------------

local function groundY(x, z)
	return spGetGroundHeight(x, z) + 6
end

local function worldText(text, x, y, z, size, color)
	glPushMatrix()
	glTranslate(x, y, z)
	glBillboard()
	glColor(color[1], color[2], color[3], color[4] or 1)
	glText(text, 0, 0, size, "co")
	glPopMatrix()
end

-- Whether the camera is within `dist` of ground point (x, z).
local function cameraNear(x, z, dist)
	local cx, cy, cz = spGetCameraPosition()
	local dx, dy, dz = cx - x, cy - spGetGroundHeight(x, z), cz - z
	return dx * dx + dy * dy + dz * dz < dist * dist
end

local function timeText(frames)
	local s = floor(frames / 30)
	return format("%d:%02d", floor(s / 60), s % 60)
end

local function drawGround(inst)
	local s = inst.snap
	if layer ~= "none" and inst.overlay then
		if inst.overlayDirty then
			inst.overlayDirty = false
			buildOverlay(inst, inst.overlay)
		end
		if inst.overlayList then
			gl.CallList(inst.overlayList)
		end
	end
	if not s then
		return
	end
	if show.spots and inst.spots and s.spots then
		glLineWidth(2)
		for i = 1, #inst.spots do
			local spot, owner = inst.spots[i], s.spots[i]
			if owner and owner > 0 then
				local c = (owner == 1 or owner == 4) and OURS or ENEMY
				glColor(c[1], c[2], c[3], (owner <= 2) and 0.9 or 0.45)
				glDrawGroundCircle(spot[1], 0, spot[2], (owner <= 2) and 46 or 38, 20)
			end
		end
	end
	if show.memory then
		glLineWidth(1)
		for i = 1, #s.enemies do
			local e = s.enemies[i]
			local flags, age, radius = e[6] or 0, e[5] or 0, e[8] or 0
			local porc = floor(flags / 8) % 2 == 1
			if porc and radius > 0 then
				glColor(ENEMY[1], ENEMY[2], ENEMY[3], 0.35)
				glDrawGroundCircle(e[3], 0, e[4], radius, 48)
			end
		end
	end
end

local function drawWorld(inst)
	local s = inst.snap
	if not s then
		return
	end
	local now = clock()
	local tc = inst.color

	-- Places to defend (pulsing).
	if show.defend then
		for i = 1, #(s.defend or {}) do
			local d = s.defend[i]
			local pulse = 60 + 20 * sin(now * 4 + i)
			glLineWidth(2)
			glColor(INK[1], INK[2], INK[3], d[4] and 0.45 or 0.85)
			glDrawGroundCircle(d[1], 0, d[2], pulse, 24)
			glDrawGroundCircle(d[1], 0, d[2], 12, 12)
		end
	end

	-- Where units outside squads mean to go.
	if show.intents then
		glLineWidth(1)
		glBeginEnd(GL_LINES, function()
			for i = 1, #s.units do
				local u = s.units[i]
				local intent = u[8]
				if intent and intent[1] then
					local x, y, z = spGetUnitPosition(u[1])
					if x then
						local worker = intent[3] ~= nil
						glColor(tc[1], tc[2], tc[3], worker and 0.35 or 0.6)
						glVertex(x, y + 8, z)
						glVertex(intent[1], groundY(intent[1], intent[2]), intent[2])
					end
				end
			end
		end)
	end

	-- Squads: a ring where they are, a line to where they go.
	if show.squads then
		for i = 1, #s.squads do
			local q = s.squads[i]
			local x, z, tx, tz, members = q[4], q[5], q[6], q[7], q[8] or 0
			if x ~= 0 or z ~= 0 then
				local y = groundY(x, z)
				local r = 70 + 12 * members
				glLineWidth(2.5)
				glColor(ACCENT[1], ACCENT[2], ACCENT[3], 0.9)
				glDrawGroundCircle(x, y, z, r, 32)
				if tx ~= 0 or tz ~= 0 then
					local ty = groundY(tx, tz)
					glLineWidth(2)
					glBeginEnd(GL_LINES, function()
						glVertex(x, y + 10, z)
						glVertex(tx, ty + 10, tz)
						glVertex(tx - 30, ty + 10, tz - 30)
						glVertex(tx + 30, ty + 10, tz + 30)
						glVertex(tx + 30, ty + 10, tz - 30)
						glVertex(tx - 30, ty + 10, tz + 30)
					end)
				end
				if cameraNear(x, z, 4500) then
					worldText(format("S%d %s %s", q[1], q[2], q[3]), x, y + r * 0.6 + 30, z, 22, ACCENT)
				end
			end
		end
	end

	-- The AI's memory of the enemy: where it last saw them.
	if show.memory then
		glLineWidth(1.5)
		for i = 1, #s.enemies do
			local e = s.enemies[i]
			local age, flags = e[5] or 0, e[6] or 0
			local seen = flags % 4 > 0 -- in sight or on radar
			local alpha = seen and 0.9 or max(0.25, 1 - age / 2700)
			glColor(ENEMY[1], ENEMY[2], ENEMY[3], alpha)
			local static = floor(flags / 4) % 2 == 1
			glDrawGroundCircle(e[3], 0, e[4], static and 40 or 26, static and 4 or 16)
			if not seen and cameraNear(e[3], e[4], 3000) then
				worldText(format("%s %s ago", e[2] ~= "" and e[2] or "?", timeText(age)), e[3], groundY(e[3], e[4]) + 40, e[4], 14, {ENEMY[1], ENEMY[2], ENEMY[3], alpha})
			end
		end
	end

	-- Roles of the AI's units, and those the operator holds.
	if show.roles then
		local labels = 0
		for i = 1, #s.units do
			local u = s.units[i]
			local role = u[6]
			local x, y, z = spGetUnitPosition(u[1])
			if x and role == "reserved" then
				glLineWidth(2.5)
				glColor(ACCENT[1], ACCENT[2], ACCENT[3], 0.6 + 0.4 * sin(now * 6))
				glDrawGroundCircle(x, y, z, 40, 20)
			elseif x and role == "retreating" then
				glLineWidth(1.5)
				glColor(ENEMY[1], ENEMY[2], ENEMY[3], 0.7)
				glDrawGroundCircle(x, y, z, 30, 12)
			end
			if x and labels < 160 and role ~= "building" and role ~= "mex" and cameraNear(x, z, 1800) and spIsSphereInView(x, y, z, 50) then
				labels = labels + 1
				local intent = u[8]
				local text = role
				if u[7] then
					text = "squad " .. u[7]
				elseif intent and intent[3] then
					text = role .. ": " .. intent[3]
				end
				worldText(text, x, y + 34, z, 11, MUTED)
			end
		end
	end
end

local function drawFlashes()
	local now = clock()
	for i = #flashes, 1, -1 do
		local f = flashes[i]
		local age = now - f.t
		if age > 1.2 then
			table.remove(flashes, i)
		else
			glLineWidth(3)
			glColor(f.color[1], f.color[2], f.color[3], 1 - age / 1.2)
			glDrawGroundCircle(f.x, 0, f.z, 20 + age * 120, 24)
		end
	end
end

function widget:DrawWorldPreUnit()
	local inst = focused()
	if not inst then
		return
	end
	glDepthTest(true)
	drawGround(inst)
	glDepthTest(false)
	glColor(1, 1, 1, 1)
	glLineWidth(1)
end

function widget:DrawWorld()
	local inst = focused()
	if inst then
		glDepthTest(true)
		drawWorld(inst)
		drawFlashes()
		glDepthTest(false)
	end
	glColor(1, 1, 1, 1)
	glLineWidth(1)
end

--------------------------------------------------------------------------------
-- The panel
--------------------------------------------------------------------------------

local vsx, vsy = Spring.GetViewGeometry()
local buttons = {}  -- {x0, y0, x1, y1, fn}
local PANEL_W = 330

function widget:ViewResize(x, y)
	vsx, vsy = x, y
end

local function button(x, y, label, on, fn)
	local w = 8 + #label * 7
	glColor(on and ACCENT[1] or 0.25, on and ACCENT[2] or 0.25, on and ACCENT[3] or 0.24, on and 0.75 or 0.6)
	glRect(x, y, x + w, y + 18)
	glColor(INK[1], INK[2], INK[3], 1)
	glText(label, x + 4, y + 4, 11, "o")
	buttons[#buttons + 1] = {x, y, x + w, y + 18, fn}
	return x + w + 4
end

function widget:DrawScreen()
	buttons = {}
	local x0, top = 12, vsy - 140
	local inst = focused()
	if not inst then
		if next(instances) == nil and clock() > 15 then
			glColor(MUTED[1], MUTED[2], MUTED[3], 0.8)
			glText("ZKGBAI Operator: no AI with operator=on on ports 7460-7467", x0, top, 11, "o")
		end
		return
	end
	local s, st = inst.snap, inst.status
	local lines = 4
	local feedLines = show.feed and 9 or 0
	local h = 22 + lines * 15 + 50 + feedLines * 13
	glColor(0.08, 0.08, 0.075, 0.72)
	glRect(x0, top - h, x0 + PANEL_W, top)
	local y = top - 18
	local tc = inst.color
	glColor(tc[1], tc[2], tc[3], 1)
	glRect(x0 + 8, y, x0 + 18, y + 10)
	glColor(INK[1], INK[2], INK[3], 1)
	glText(format("ZKGBAI team %d  ·  :%d  ·  %s", inst.team, inst.port, st and st.state or "…"), x0 + 24, y, 13, "o")
	y = y - 17
	if s then
		local m, e = s.metal or {}, s.energy or {}
		glText(format("metal %.1f/s (%d)   energy %.1f/s (%d)", m[2] or 0, m[1] or 0, e[2] or 0, e[1] or 0), x0 + 8, y, 11, "o")
		y = y - 15
		local army = st and st.army
		glText(format("army %s vs enemy seen %s   squads %d", army and floor(army.value) or "?", army and floor(army.enemy_fighter_value) or "?", #s.squads), x0 + 8, y, 11, "o")
		y = y - 15
		local terr = st and st.territory
		local held = #(st and st.operator and st.operator.reserved or {})
		glText(format("spots %s:%s   held by operator %d   frame %d", terr and terr.spots_ours or "?", terr and terr.spots_enemy or "?", held, s.frame), x0 + 8, y, 11, "o")
		y = y - 15
	else
		glText(inst.lastError and ("waiting: " .. inst.lastError) or "waiting for a snapshot…", x0 + 8, y, 11, "o")
		y = y - 45
	end
	-- Toggles.
	y = y - 6
	local bx = x0 + 8
	for _, key in ipairs({"squads", "intents", "memory", "spots", "roles", "defend", "feed"}) do
		bx = button(bx, y, key, show[key], function()
			show[key] = not show[key]
		end)
	end
	y = y - 22
	bx = x0 + 8
	bx = button(bx, y, "map: " .. layer, layer ~= "none", function()
		for i = 1, #LAYERS do
			if LAYERS[i] == layer then
				layer = LAYERS[i % #LAYERS + 1]
				break
			end
		end
		for _, other in pairs(instances) do
			other.overlay, other.overlayDirty = nil, true
			other.due.overlay = 0
		end
	end)
	local ports = sortedPorts()
	if #ports > 1 then
		bx = button(bx, y, "AI: team " .. inst.team, true, function()
			for i = 1, #ports do
				if ports[i] == focus then
					focus, focusChosen = ports[i % #ports + 1], true
					break
				end
			end
		end)
	end
	button(bx, y, "release held", false, releaseSelected)
	y = y - 6
	-- The AI's log.
	if show.feed then
		local now = clock()
		local feed = inst.feed
		for i = max(1, #feed - feedLines + 1), #feed do
			local item = feed[i]
			y = y - 13
			local fade = max(0.45, 1 - (now - item.t) / 60)
			glColor(MUTED[1], MUTED[2], MUTED[3], fade)
			glText(timeText(item.f or 0), x0 + 8, y, 10, "o")
			glColor(INK[1], INK[2], INK[3], fade)
			local text = item.text
			if #text > 64 then
				text = text:sub(1, 62) .. "…"
			end
			glText(text, x0 + 44, y, 10, "o")
		end
	end
	if message and clock() - messageTime < 4 then
		local c = messageBad and ENEMY or ACCENT
		glColor(c[1], c[2], c[3], 1)
		glText(message, x0 + 8, top - h - 16, 12, "o")
	end
	glColor(1, 1, 1, 1)
end

function widget:MousePress(mx, my, button)
	if button == 1 then
		for i = 1, #buttons do
			local b = buttons[i]
			if mx >= b[1] and mx <= b[3] and my >= b[2] and my <= b[4] then
				b[5]()
				return true
			end
		end
		return false
	end
	if button == 3 then
		local byPort = steerableSelection()
		if byPort then
			steer(byPort, mx, my)
			return true
		end
	end
	return false
end

function widget:MouseRelease()
	return -1
end

--------------------------------------------------------------------------------

function widget:Initialize()
	startTimer = spGetTimer()
	json = Spring.Utilities and Spring.Utilities.json
	if not socket or not json then
		spEcho("ZKGBAI Operator: needs LuaSocket and Spring.Utilities.json; disabled")
		widgetHandler:RemoveWidget(self)
		return
	end
end

function widget:Shutdown()
	for i = 1, #requests do
		requests[i].conn:close()
	end
	requests = {}
	for _, inst in pairs(instances) do
		if inst.overlayList then
			gl.DeleteList(inst.overlayList)
		end
	end
end

local function tick()
	discover()
	pollInstances()
	pumpRequests()
end

function widget:Update()
	tick()
end

-- Also between frames, should Update not run (a headless engine).
function widget:GameFrame()
	tick()
end
