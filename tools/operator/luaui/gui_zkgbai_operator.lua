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
-- metal spots, and its log. On the ground, through a shader draped over the
-- terrain: one of its threat or value maps, smooth with contour lines, and the
-- terrain's segmentation for a movement tier (regions by id or by who holds
-- them, sheltered ones hatched) with the chokepoints between them.
--
-- Steering (as a spectator, or for an AI ally): select the AI's units and
-- right-click: move; ctrl: attack-move; on a unit: attack or guard; shift
-- queues; alt: send their whole squad there; ctrl+alt: a place to defend.
-- Units ordered so are held by the operator (the AI leaves them alone) until
-- released (panel button, or /luaui zkop release).
--
-- Text commands (/luaui zkop ...): focus <team>, layer <name|none>,
-- terrain <off|regions|holders> [tier], release, build <unitdef> (at the
-- mouse), fac <unitdef> [count], defend (at the mouse), say <text>,
-- note <text>, feature <name> on|off, toggle <what>.
--
-- The panel is a Chili window in Zero-K; without Chili it is drawn by hand,
-- and without GL4 shaders the value map falls back to plain quads.
--------------------------------------------------------------------------------

function widget:GetInfo()
	return {
		name    = "ZKGBAI Operator",
		desc    = "Shows what ZKGBAI AIs think (squads, intents, memory of the enemy, threat maps, terrain segmentation, log) and lets you steer them. Needs the AI's operator API (operator=on).",
		author  = "ZKGBAI",
		date    = "2026-10-20",
		license = "GNU GPL, v2 or later",
		layer   = 10,
		enabled = true,
	}
end

local HOST = "127.0.0.1"
local FIRST_PORT, LAST_PORT = 7460, 7467
local REQUEST_TIMEOUT = 8     -- seconds
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
local GL_LINE_STRIP = GL.LINE_STRIP
local GL_QUADS     = GL.QUADS

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
-- The AI instances, and what is shown of them
--------------------------------------------------------------------------------

-- By port: {port, team, allyTeam, version, fails, busy = {}, due = {}, snap,
-- byId, status, spots, feed, nextSeq, overlay, terrain}
local instances = {}
local probing = {}
local nextProbe = 0
local focus -- port of the AI shown
local focusChosen = false -- picked by hand

local show = {squads = true, intents = true, memory = true, spots = true, roles = true, defend = true, chokes = true, feed = true}
local SHOW_ORDER = {"squads", "intents", "memory", "spots", "roles", "defend", "chokes", "feed"}
local LAYERS = {"none", "threat", "enemy_porc", "aa_threat", "sub_threat", "value", "ally_threat", "riot"}
local layer = "none"
local SEGMENTATIONS = {"off", "regions", "holders"}
local segmentation = "off"
local TIERS = {"strategy", "spider", "bot", "vehicle", "hover", "ship", "amphib"}
local segTier = "strategy"

local flashes = {}  -- feedback marks on the ground: {x, z, t, color}
local message, messageTime, messageBad

local panel -- the Chili panel, if any (see below)

local function say(text, bad)
	message, messageTime, messageBad = text, clock(), bad
	if panel then
		panel.setMessage(text, bad)
	end
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

--------------------------------------------------------------------------------
-- Maps on the ground: a GL4 shader over a mesh draped on the terrain
--------------------------------------------------------------------------------

local overlay = {} -- shader, vao, ok

local VS_SRC = [[
#version 420
#line 10000
layout (location = 0) in vec2 xyworld_xyfract;
uniform sampler2D heightmapTex;
out DataVS {
	vec2 worldXZ;
};
//__ENGINEUNIFORMBUFFERDEFS__
#line 10100
void main() {
	vec2 w = (xyworld_xyfract * 0.5 + 0.5) * mapSize.xy;
	vec2 uvhm = clamp(w, vec2(8.0), mapSize.xy - 8.0) / mapSize.xy;
	float y = max(0.0, textureLod(heightmapTex, uvhm, 0.0).x) + 4.0;
	worldXZ = w;
	gl_Position = cameraViewProj * vec4(w.x, y, w.y, 1.0);
}
]]

-- Mode 0: a value map (red channel, 0-1; linear filtering), tinted with
-- contour lines at quarters. Mode 1: regions (id + 1 in red and green,
-- texel-exact) coloured by id or by who holds them (attrTex, a texel per
-- region: red the enemy's strength, blue ours, alpha flags), with borders.
local FS_SRC = [[
#version 420
#line 20000
uniform sampler2D dataTex;
uniform sampler2D attrTex;
uniform vec4 dataRect;   // origin x, z; world width, height
uniform vec4 dataInfo;   // texels wide, high; mode; alpha
uniform vec4 valueColor; // rgb; contour strength
uniform vec4 regionInfo; // colour by holders?; regions; border alpha; fill alpha
in DataVS {
	vec2 worldXZ;
};
out vec4 fragColor;
//__ENGINEUNIFORMBUFFERDEFS__
#line 20100

vec3 hsv2rgb(vec3 c) {
	vec3 p = abs(fract(c.xxx + vec3(0.0, 2.0 / 3.0, 1.0 / 3.0)) * 6.0 - 3.0);
	return c.z * mix(vec3(1.0), clamp(p - 1.0, 0.0, 1.0), c.y);
}

float regionAt(ivec2 t) {
	t = clamp(t, ivec2(0), ivec2(dataInfo.xy) - 1);
	vec4 c = texelFetch(dataTex, t, 0);
	return floor(c.r * 255.0 + 0.5) + 256.0 * floor(c.g * 255.0 + 0.5) - 1.0;
}

void main() {
	vec2 uv = (worldXZ - dataRect.xy) / dataRect.zw;
	if (uv.x < 0.0 || uv.y < 0.0 || uv.x > 1.0 || uv.y > 1.0) discard;
	if (dataInfo.z < 0.5) {
		float v = texture(dataTex, uv).r;
		if (v < 0.02) discard;
		float levels = v * 4.0;
		float f = fract(levels);
		float line = 1.0 - smoothstep(0.0, 1.5 * fwidth(levels), min(f, 1.0 - f));
		vec3 c = valueColor.rgb * (0.65 + 0.35 * v);
		float a = dataInfo.w * (0.2 + 0.6 * sqrt(v));
		fragColor = vec4(mix(c, vec3(1.0, 0.95, 0.85), line * valueColor.a), clamp(a + line * valueColor.a * 0.6, 0.0, 0.95));
		return;
	}
	vec2 p = uv * dataInfo.xy;
	ivec2 ip = ivec2(floor(p));
	vec2 f = fract(p);
	float id = regionAt(ip);
	if (id < 0.0) discard;
	vec2 fw = max(fwidth(p) * 1.5, vec2(0.1));
	float border = 0.0;
	if (regionAt(ip + ivec2(1, 0)) != id) border = max(border, 1.0 - smoothstep(0.0, fw.x, 1.0 - f.x));
	if (regionAt(ip - ivec2(1, 0)) != id) border = max(border, 1.0 - smoothstep(0.0, fw.x, f.x));
	if (regionAt(ip + ivec2(0, 1)) != id) border = max(border, 1.0 - smoothstep(0.0, fw.y, 1.0 - f.y));
	if (regionAt(ip - ivec2(0, 1)) != id) border = max(border, 1.0 - smoothstep(0.0, fw.y, f.y));
	vec4 attr = texelFetch(attrTex, ivec2(int(min(id, regionInfo.y - 1.0)), 0), 0);
	vec3 c;
	float fill = regionInfo.w;
	if (regionInfo.x < 0.5) {
		c = hsv2rgb(vec3(fract(id * 0.61803398875), 0.55, 0.95));
	} else {
		c = mix(vec3(0.6), vec3(0.95, 0.3, 0.3), attr.r);
		c = mix(c, vec3(0.25, 0.55, 0.98), attr.b * (1.0 - 0.5 * attr.r));
		fill *= 0.4 + 1.6 * max(attr.r, attr.b);
	}
	float flags = floor(attr.a * 255.0 + 0.5);
	if (mod(flags, 2.0) > 0.5) {
		// Sheltered from the enemy's ground forces: hatched.
		fill *= 0.4 + 1.6 * step(0.5, fract((worldXZ.x + worldXZ.y) / 64.0));
	}
	fragColor = vec4(mix(c, vec3(1.0), border * 0.3), clamp(fill + border * regionInfo.z, 0.0, 0.95));
}
]]

local function initOverlay()
	if not gl.CreateShader or not gl.GetVAO then
		return "no GL4 (headless?)"
	end
	-- Included from here, so that their functions land in this widget.
	local okInclude, LuaShader = pcall(function()
		return VFS.Include("LuaUI/Widgets/Include/LuaShader.lua")
	end)
	local okPlane = pcall(function()
		VFS.Include("LuaUI/Widgets/Include/instancevbotable.lua")
	end)
	if not okInclude or not LuaShader or not okPlane or not makePlaneVBO then
		return "no LuaShader.lua / instancevbotable.lua"
	end
	overlay.source = {
		vsSrc = VS_SRC,
		fsSrc = FS_SRC,
		shaderName = "ZKGBAI Operator overlay",
		uniformInt = {heightmapTex = 0, dataTex = 1, attrTex = 2},
		uniformFloat = {dataRect = {0, 0, 1, 1}, dataInfo = {1, 1, 0, 0.5}, valueColor = {1, 0.5, 0.2, 0.6}, regionInfo = {0, 1, 0.8, 0.15}},
		shaderConfig = {},
		-- Inline sources look unchanged to the loader; compile anyway.
		forceupdate = true,
	}
	overlay.shader = LuaShader.CheckShaderUpdates(overlay.source)
	if not overlay.shader then
		return "the overlay shader did not compile"
	end
	-- A grid of about 24 elmos (at most 384 a side).
	local nx = min(384, max(32, floor(Game.mapSizeX / 24)))
	local nz = min(384, max(32, floor(Game.mapSizeZ / 24)))
	local vbo = makePlaneVBO(1, 1, nx, nz)
	local ibo = makePlaneIndexVBO(nx, nz)
	if not vbo or not ibo then
		return "no VBOs"
	end
	overlay.vao = gl.GetVAO()
	overlay.vao:AttachVertexBuffer(vbo)
	overlay.vao:AttachIndexBuffer(ibo)
	overlay.ok = true
end

local function newDataTexture(w, h, linear)
	local filter = linear and GL.LINEAR or GL.NEAREST
	return gl.CreateTexture(w, h, {
		border = false, min_filter = filter, mag_filter = filter,
		wrap_s = GL.CLAMP_TO_EDGE, wrap_t = GL.CLAMP_TO_EDGE, fbo = true,
	})
end

-- Paint a texture: fn(rect) is given rect(x0, y0, x1, y1) in texels.
local function paint(tex, w, h, fn)
	gl.RenderToTexture(tex, function()
		gl.Blending(false)
		gl.Clear(GL.COLOR_BUFFER_BIT, 0, 0, 0, 0)
		local sx, sy = 2 / w, 2 / h
		fn(function(x0, y0, x1, y1)
			glRect(-1 + x0 * sx, -1 + y0 * sy, -1 + x1 * sx, -1 + y1 * sy)
		end)
		gl.Blending(true)
	end)
end

local function freeTexture(t)
	if t then
		gl.DeleteTextureFBO(t)
		gl.DeleteTexture(t)
	end
end

-- A threat or value map, from the threat_map tool.
local function uploadValues(inst, o)
	local ov = inst.overlayGl or {}
	inst.overlayGl = ov
	if ov.tex and (ov.w ~= o.width or ov.h ~= o.height) then
		freeTexture(ov.tex)
		ov.tex = nil
	end
	if not ov.tex then
		ov.tex = newDataTexture(o.width, o.height, true)
		ov.w, ov.h = o.width, o.height
	end
	if not ov.tex then
		return
	end
	local maxv = max(1, o.max or 1)
	paint(ov.tex, o.width, o.height, function(rect)
		for y = 1, o.height do
			local row = o.rows[y]
			for x = 1, o.width do
				local v = row and row[x] or 0
				if v > 0 then
					glColor(min(1, v / maxv), 0, 0, 1)
					rect(x - 1, y - 1, x, y)
				end
			end
		end
	end)
	-- The AI's cells are centred on multiples of 64 elmos.
	ov.rect = {-32, -32, o.width * o.cell, o.height * o.cell}
end

-- The segmentation, from the terrain_map tool: region ids per cell.
local function uploadRegions(t, m)
	if t.tex and (t.w ~= m.width or t.h ~= m.height) then
		freeTexture(t.tex)
		t.tex = nil
	end
	if not t.tex then
		t.tex = newDataTexture(m.width, m.height, false)
		t.w, t.h = m.width, m.height
	end
	if not t.tex then
		return
	end
	paint(t.tex, m.width, m.height, function(rect)
		for z = 1, m.height do
			local row = m.rows[z]
			local x = 0
			for i = 1, #row, 2 do
				local id, run = row[i], row[i + 1]
				if id >= 0 then
					local code = id + 1
					glColor((code % 256) / 255, floor(code / 256) / 255, 0, 1)
					rect(x, z - 1, x + run, z)
				end
				x = x + run
			end
		end
	end)
	t.rect = {0, 0, m.width * m.cell, m.height * m.cell}
	t.version = m.grid_version
end

-- Region attributes: the enemy's strength, ours, flags (1: sheltered).
local function uploadRegionAttributes(t, m)
	local n = max(1, #m.regions)
	if t.attr and t.attrN ~= n then
		freeTexture(t.attr)
		t.attr = nil
	end
	if not t.attr then
		t.attr = newDataTexture(n, 1, false)
		t.attrN = n
	end
	if not t.attr then
		return
	end
	local strongest = 1
	for i = 1, #m.regions do
		local r = m.regions[i]
		strongest = max(strongest, r.enemy_power or 0, r.ally_power or 0)
	end
	paint(t.attr, n, 1, function(rect)
		for i = 1, #m.regions do
			local r = m.regions[i]
			local e = sqrt(min(1, (r.enemy_power or 0) / strongest))
			local a = sqrt(min(1, (r.ally_power or 0) / strongest))
			if (r.enemy_value or 0) > 0 and e == 0 then
				e = 0.25
			end
			if (r.ally_value or 0) > 0 and a == 0 then
				a = 0.25
			end
			local flags = r.sheltered and 1 or 0
			glColor(e, 0, a, flags / 255)
			rect(r.id, 0, r.id + 1, 1)
		end
	end)
end

local function drawOverlayPass(tex, rect, w, h, mode, attr, regionInfo)
	local s = overlay.shader
	gl.Texture(1, tex)
	if attr then
		gl.Texture(2, attr)
	end
	s:SetUniform("dataRect", rect[1], rect[2], rect[3], rect[4])
	s:SetUniform("dataInfo", w, h, mode, mode == 0 and 0.55 or 1)
	s:SetUniform("valueColor", HEAT[1], HEAT[2], HEAT[3], 0.55)
	if regionInfo then
		s:SetUniform("regionInfo", regionInfo[1], regionInfo[2], regionInfo[3], regionInfo[4])
	end
	overlay.vao:DrawElements(GL.TRIANGLES)
end

local function drawOverlays(inst)
	if not overlay.ok then
		return false
	end
	local ov, t = inst.overlayGl, inst.terrain
	local values = layer ~= "none" and ov and ov.tex and ov.rect
	local regions = segmentation ~= "off" and t and t.tex and t.attr and t.rect
	if not values and not regions then
		return true
	end
	glDepthTest(GL.LEQUAL)
	gl.DepthMask(false)
	gl.Culling(false)
	gl.Blending(GL.SRC_ALPHA, GL.ONE_MINUS_SRC_ALPHA)
	gl.Texture(0, "$heightmap")
	overlay.shader:Activate()
	if regions then
		drawOverlayPass(t.tex, t.rect, t.w, t.h, 1, t.attr, {segmentation == "holders" and 1 or 0, t.attrN, 0.8, 0.14})
	end
	if values then
		drawOverlayPass(ov.tex, ov.rect, ov.w, ov.h, 0)
	end
	overlay.shader:Deactivate()
	gl.Texture(0, false)
	gl.Texture(1, false)
	gl.Texture(2, false)
	gl.DepthMask(false)
	glDepthTest(false)
	return true
end

-- Without shaders: the value map as quads (a display list per update).
local function buildQuads(inst, o)
	if inst.quadList then
		gl.DeleteList(inst.quadList)
		inst.quadList = nil
	end
	if not o or not o.rows then
		return
	end
	local cell, maxv = o.cell, max(1, o.max or 1)
	local sub = max(1, floor(cell / 64))
	local step = cell / sub
	inst.quadList = gl.CreateList(function()
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

local function forgetMaps(inst)
	if inst.overlayGl then
		freeTexture(inst.overlayGl.tex)
	end
	if inst.terrain then
		freeTexture(inst.terrain.tex)
		freeTexture(inst.terrain.attr)
	end
	if inst.quadList then
		gl.DeleteList(inst.quadList)
	end
	inst.overlayGl, inst.terrain, inst.quadList, inst.overlay = nil, nil, nil, nil
end

--------------------------------------------------------------------------------
-- Finding and polling the instances
--------------------------------------------------------------------------------

local function dropInstance(port)
	local inst = instances[port]
	if inst then
		forgetMaps(inst)
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

local function terrainPath(grid)
	local path = "/call/terrain_map?grid=" .. tostring(grid)
	if segTier ~= "strategy" then
		path = path .. "&tier=" .. segTier
	end
	return path
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
					poll(inst, "overlay", 3, "/call/threat_map?max_size=160&layer=" .. layer, function(v)
						inst.overlay = v
						inst.overlayDirty = true
					end)
				end
				if segmentation ~= "off" or show.chokes then
					-- The regions only when they changed (or the tier did).
					poll(inst, "terrain", 6, terrainPath(false), function(v)
						local t = inst.terrain or {}
						inst.terrain = t
						t.meta = v
						t.metaDirty = true
						if t.version ~= v.grid_version or t.tier ~= v.tier then
							t.wantGrid = true
						end
					end)
					if inst.terrain and inst.terrain.wantGrid then
						poll(inst, "terrainGrid", 1, terrainPath(true), function(v)
							local t = inst.terrain
							t.grid = v
							t.wantGrid = false
							t.gridDirty = true
							t.tier = v.tier
						end)
					end
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
			local s, t = inst.snap, inst.terrain
			spEcho(format("ZKGBAI Operator: port %d team %d frame %s units %d enemies %d squads %d feed %d status %s overlay %s terrain %s/%s regions %d chokes %d gl4 %s chili %s fails %d",
				port, inst.team, s and s.frame or -1, s and #s.units or 0, s and #s.enemies or 0, s and #s.squads or 0,
				#inst.feed, inst.status and inst.status.state or "-", inst.overlay and inst.overlay.layer or "-",
				segmentation, t and t.tier or "-", t and t.meta and #t.meta.regions or 0, t and t.meta and #t.meta.chokes or 0,
				tostring(overlay.ok), tostring(panel ~= nil), inst.fails))
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

local function cycle(list, current)
	for i = 1, #list do
		if list[i] == current then
			return list[i % #list + 1]
		end
	end
	return list[1]
end

local function setLayer(name)
	layer = name or "none"
	for _, inst in pairs(instances) do
		inst.overlay, inst.overlayDirty = nil, true
		inst.due.overlay = 0
	end
end

local function setSegmentation(mode, tier)
	segmentation = mode or "off"
	if tier and tier ~= segTier then
		segTier = tier
		for _, inst in pairs(instances) do
			if inst.terrain then
				inst.terrain.wantGrid = true
			end
			inst.due.terrain = 0
		end
	end
end

local function cycleFocus()
	local ports = sortedPorts()
	focus, focusChosen = cycle(ports, focus), true
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
		setLayer(words[3])
	elseif cmd == "terrain" then
		setSegmentation(words[3], words[4])
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
		say("zkop: focus <team> | layer <name> | terrain <off|regions|holders> [tier] | toggle <what> | release | build <def> | defend | fac <def> [n] | say | note | feature <name> on|off", true)
	end
	if panel then
		panel.sync()
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

-- A line over the ground from a to b.
local function groundLine(ax, az, bx, bz)
	local len = sqrt((bx - ax) ^ 2 + (bz - az) ^ 2)
	local n = max(1, floor(len / 24))
	glBeginEnd(GL_LINE_STRIP, function()
		for i = 0, n do
			local x, z = ax + (bx - ax) * i / n, az + (bz - az) * i / n
			glVertex(x, groundY(x, z) + 4, z)
		end
	end)
end

local function drawChokes(inst)
	local t = inst.terrain
	local meta = t and t.meta
	if not meta then
		return
	end
	for i = 1, #meta.chokes do
		local c = meta.chokes[i]
		local strategic = c.fortify or c.hold or c.seal
		local gate = c.gate
		if c.kind == "gate" then
			glLineWidth(strategic and 5 or 3)
			if strategic then
				glColor(ACCENT[1], ACCENT[2], ACCENT[3], 0.95)
			else
				glColor(INK[1], INK[2], INK[3], 0.75)
			end
		else
			glLineWidth(strategic and 4 or 1.5)
			glColor(INK[1], INK[2], INK[3], strategic and 0.8 or 0.35)
		end
		if gate and gate[1] and gate[2] then
			groundLine(gate[1][1], gate[1][2], gate[2][1], gate[2][2])
		else
			glDrawGroundCircle(c.pos[1], 0, c.pos[2], max(20, (c.width or 40) / 2), 16)
		end
		if cameraNear(c.pos[1], c.pos[2], 2600) then
			local tags = {}
			if c.fortify then tags[#tags + 1] = "fortify" end
			if c.hold then tags[#tags + 1] = "hold" end
			if c.seal then tags[#tags + 1] = "seal" end
			local label = format("%s %d · %d wide%s", c.kind, c.id, floor(c.width or 0), #tags > 0 and (" · " .. table.concat(tags, ", ")) or "")
			worldText(label, c.pos[1], groundY(c.pos[1], c.pos[2]) + 30, c.pos[2], 14, strategic and ACCENT or INK)
		end
	end
	if segmentation ~= "off" then
		for i = 1, #meta.regions do
			local r = meta.regions[i]
			if cameraNear(r.center[1], r.center[2], 2200) then
				local who = r.holder and r.holder ~= "neutral" and (" · " .. r.holder) or ""
				local power = (r.enemy_power or 0) > 0 and format(" · enemy %d", floor(r.enemy_power)) or ""
				worldText(format("region %d%s%s%s", r.id, who, power, r.sheltered and " · sheltered" or ""),
					r.center[1], groundY(r.center[1], r.center[2]) + 20, r.center[2], 16, MUTED)
			end
		end
	end
end

local function drawGround(inst)
	local s = inst.snap
	if (inst.overlayDirty or (inst.terrain and (inst.terrain.gridDirty or inst.terrain.metaDirty))) then
		if overlay.ok then
			if inst.overlayDirty and inst.overlay then
				uploadValues(inst, inst.overlay)
			end
			local t = inst.terrain
			if t and t.gridDirty and t.grid then
				uploadRegions(t, t.grid)
				t.gridDirty = false
			end
			if t and t.metaDirty and t.meta then
				uploadRegionAttributes(t, t.meta)
				t.metaDirty = false
			end
		elseif inst.overlayDirty then
			buildQuads(inst, inst.overlay)
		end
		inst.overlayDirty = false
	end
	if not drawOverlays(inst) and layer ~= "none" and inst.quadList then
		glDepthTest(true)
		gl.CallList(inst.quadList)
	end
	glDepthTest(true)
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
			local flags, radius = e[6] or 0, e[8] or 0
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
	if show.chokes then
		drawChokes(inst)
	end
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
-- The panel: a Chili window
--------------------------------------------------------------------------------

local config = {} -- saved between games: window place, what is shown

local function statusLines(inst)
	local s, st = inst.snap, inst.status
	if not s then
		return inst.lastError and ("waiting: " .. inst.lastError) or "waiting for a snapshot…"
	end
	local m, e = s.metal or {}, s.energy or {}
	local army = st and st.army
	local terr = st and st.territory
	local held = #(st and st.operator and st.operator.reserved or {})
	return format("metal %.1f/s (%d)   energy %.1f/s (%d)\narmy %s vs enemy seen %s   squads %d\nspots %s:%s   held by operator %d   %s",
		m[2] or 0, m[1] or 0, e[2] or 0, e[1] or 0,
		army and floor(army.value) or "?", army and floor(army.enemy_fighter_value) or "?", #s.squads,
		terr and terr.spots_ours or "?", terr and terr.spots_enemy or "?", held, timeText(s.frame or 0))
end

local function makeChiliPanel()
	local Chili = WG.Chili
	if not Chili then
		return nil
	end
	local vsx, vsy = Spring.GetViewGeometry()
	local w = config.win or {x = 8, y = 140, width = 360, height = 470}
	local p = {}
	local font = WG.GetFont and WG.GetFont() or nil
	local window = Chili.Window:New{
		parent = Chili.Screen0,
		caption = "ZKGBAI Operator",
		x = min(w.x, vsx - 120), y = min(w.y, vsy - 120), width = w.width, height = w.height,
		minWidth = 280, minHeight = 260,
		draggable = true, resizable = true,
	}
	p.window = window
	p.header = Chili.Label:New{parent = window, x = 4, y = 18, right = 4, height = 18, caption = "looking for ZKGBAI on ports 7460-7467…", objectOverrideFont = font}
	p.stats = Chili.TextBox:New{parent = window, x = 4, y = 40, right = 4, height = 46, text = "", fontsize = 12}
	p.checks = {}
	for i, key in ipairs(SHOW_ORDER) do
		local col, row = (i - 1) % 4, floor((i - 1) / 4)
		p.checks[key] = Chili.Checkbox:New{
			parent = window,
			x = tostring(col * 25) .. "%", y = 90 + row * 22, width = "24%", height = 20,
			caption = key, checked = show[key], objectOverrideFont = font,
			OnChange = {function(_, value)
				show[key] = value
				config.show = show
			end},
		}
	end
	local function combo(y, label, items, current, onSelect, x0, width)
		if label then
			Chili.Label:New{parent = window, x = 4, y = y + 4, width = 60, height = 20, caption = label, objectOverrideFont = font}
		end
		local index = 1
		for i = 1, #items do
			if items[i] == current then
				index = i
			end
		end
		local box = Chili.ComboBox:New{
			parent = window, x = x0 or 64, y = y, width = width, right = width and nil or 4, height = 24,
			items = items, selected = index, objectOverrideFont = font,
			OnSelect = {function(_, i)
				onSelect(items[i])
			end},
		}
		return box
	end
	p.layer = combo(138, "map", LAYERS, layer, function(v)
		setLayer(v)
		config.layer = v
	end)
	p.seg = combo(166, "terrain", SEGMENTATIONS, segmentation, function(v)
		setSegmentation(v)
		config.segmentation = v
	end, 64, "38%")
	p.tier = combo(166, nil, TIERS, segTier, function(v)
		setSegmentation(segmentation, v)
		config.tier = v
	end, "62%", "36%")
	p.focusButton = Chili.Button:New{
		parent = window, x = 4, y = 196, width = "48%", height = 24, caption = "AI: -", objectOverrideFont = font,
		OnClick = {function() cycleFocus() end},
	}
	Chili.Button:New{
		parent = window, x = "51%", y = 196, right = 4, height = 24, caption = "release held", objectOverrideFont = font,
		OnClick = {releaseSelected},
	}
	p.message = Chili.Label:New{parent = window, x = 4, y = 224, right = 4, height = 16, caption = "", objectOverrideFont = font}
	local scroll = Chili.ScrollPanel:New{parent = window, x = 4, y = 244, right = 4, bottom = 4, horizontalScrollbar = false}
	p.feed = Chili.TextBox:New{parent = scroll, x = 0, y = 0, right = 0, text = "", fontsize = 11}
	p.scroll = scroll

	local shownFeed, shownPort
	function p.update()
		local inst = focused()
		if not inst then
			p.header:SetCaption(next(instances) and "…" or "no ZKGBAI with operator=on on ports 7460-7467")
			return
		end
		local st = inst.status
		p.header:SetCaption(format("team %d · port %d · %s", inst.team, inst.port, st and st.state or "…"))
		p.stats:SetText(statusLines(inst))
		p.focusButton:SetCaption("AI: team " .. inst.team)
		local feed = inst.feed
		local last = feed[#feed]
		local key = (last and last.f or -1) .. ":" .. #feed
		if show.feed and (key ~= shownFeed or shownPort ~= inst.port) then
			shownFeed, shownPort = key, inst.port
			local lines = {}
			for i = max(1, #feed - 40), #feed do
				lines[#lines + 1] = "\255\140\140\140" .. timeText(feed[i].f or 0) .. "  \255\235\235\230" .. feed[i].text
			end
			p.feed:SetText(table.concat(lines, "\n"))
			p.scroll:SetScrollPos(0, math.huge)
		elseif not show.feed and shownFeed then
			shownFeed = nil
			p.feed:SetText("")
		end
	end
	function p.setMessage(text, bad)
		p.message:SetCaption((bad and "\255\230\100\100" or "\255\30\200\140") .. text)
	end
	function p.sync()
		for key, box in pairs(p.checks) do
			if box.checked ~= show[key] then
				box:Toggle()
			end
		end
		for _, pair in ipairs({{p.layer, LAYERS, layer}, {p.seg, SEGMENTATIONS, segmentation}, {p.tier, TIERS, segTier}}) do
			local box, items, current = pair[1], pair[2], pair[3]
			for i = 1, #items do
				if items[i] == current and box.selected ~= i then
					box:Select(i)
				end
			end
		end
	end
	function p.save()
		config.win = {x = window.x, y = window.y, width = window.width, height = window.height}
	end
	function p.dispose()
		p.save()
		window:Dispose()
	end
	return p
end

--------------------------------------------------------------------------------
-- The panel without Chili: drawn by hand
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

local function drawHandPanel()
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
	local feedLines = show.feed and 9 or 0
	local h = 22 + 4 * 15 + 72 + feedLines * 13
	glColor(0.08, 0.08, 0.075, 0.72)
	glRect(x0, top - h, x0 + PANEL_W, top)
	local y = top - 18
	local tc = inst.color
	glColor(tc[1], tc[2], tc[3], 1)
	glRect(x0 + 8, y, x0 + 18, y + 10)
	glColor(INK[1], INK[2], INK[3], 1)
	glText(format("ZKGBAI team %d  ·  :%d  ·  %s", inst.team, inst.port, inst.status and inst.status.state or "…"), x0 + 24, y, 13, "o")
	for line in statusLines(inst):gmatch("[^\n]+") do
		y = y - 15
		glText(line, x0 + 8, y, 11, "o")
	end
	y = y - 21
	local bx = x0 + 8
	for _, key in ipairs(SHOW_ORDER) do
		bx = button(bx, y, key, show[key], function()
			show[key] = not show[key]
		end)
	end
	y = y - 22
	bx = button(x0 + 8, y, "map: " .. layer, layer ~= "none", function()
		setLayer(cycle(LAYERS, layer))
	end)
	bx = button(bx, y, "terrain: " .. segmentation, segmentation ~= "off", function()
		setSegmentation(cycle(SEGMENTATIONS, segmentation))
	end)
	button(bx, y, segTier, false, function()
		setSegmentation(segmentation, cycle(TIERS, segTier))
	end)
	y = y - 22
	bx = x0 + 8
	if #sortedPorts() > 1 then
		bx = button(bx, y, "AI: team " .. inst.team, true, cycleFocus)
	end
	button(bx, y, "release held", false, releaseSelected)
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

function widget:DrawScreen()
	if not panel then
		drawHandPanel()
	end
end

function widget:MousePress(mx, my, button)
	if button == 1 and not panel then
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

function widget:GetConfigData()
	if panel then
		panel.save()
	end
	config.show, config.layer, config.segmentation, config.tier = show, layer, segmentation, segTier
	return config
end

function widget:SetConfigData(data)
	config = data or {}
	for key, value in pairs(config.show or {}) do
		if show[key] ~= nil then
			show[key] = value
		end
	end
	layer = config.layer or layer
	segmentation = config.segmentation or segmentation
	segTier = config.tier or segTier
end

function widget:Initialize()
	startTimer = spGetTimer()
	json = Spring.Utilities and Spring.Utilities.json
	if not socket or not json then
		spEcho("ZKGBAI Operator: needs LuaSocket and Spring.Utilities.json; disabled")
		widgetHandler:RemoveWidget(self)
		return
	end
	local ok, err = pcall(initOverlay)
	if not ok or err then
		spEcho("ZKGBAI Operator: no shader overlay, plain quads instead: " .. tostring(err))
		overlay.ok = false
	end
	ok, err = pcall(function()
		panel = makeChiliPanel()
	end)
	if not ok then
		spEcho("ZKGBAI Operator: no Chili panel: " .. tostring(err))
		panel = nil
	end
end

function widget:Shutdown()
	for i = 1, #requests do
		requests[i].conn:close()
	end
	requests = {}
	for _, inst in pairs(instances) do
		forgetMaps(inst)
	end
	if panel then
		panel.dispose()
		panel = nil
	end
	if overlay.shader then
		overlay.shader:Delete()
	end
end

local nextPanelUpdate = 0
local function tick()
	discover()
	pollInstances()
	pumpRequests()
	if panel and clock() > nextPanelUpdate then
		nextPanelUpdate = clock() + 0.5
		panel.update()
	end
end

function widget:Update()
	tick()
end

-- Also between frames, should Update not run (a headless engine).
function widget:GameFrame()
	tick()
end
