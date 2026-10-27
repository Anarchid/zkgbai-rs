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
-- metal spots and the Delaunay links between them along which it infers who
-- holds a spot unseen (dimmed where ground forces do not walk the link, so
-- nothing is inferred along it), the defense posture's lane fronts, and its
-- log. On the ground, by shaders (screen-space over the
-- map's deferred depth when AllowDeferredMapRendering is on, else a mesh
-- draped on the terrain): one of its threat or value maps, smooth with
-- contour lines, and the terrain's segmentation for a movement tier (regions
-- by id or by who holds them, sheltered ones hatched) with the chokepoints
-- between them.
--
-- Steering (as a spectator, or for an AI ally): select the AI's units and
-- right-click: move; ctrl: attack-move; on a unit: attack or guard; shift
-- queues; alt: send their whole squad there; ctrl+alt: a place to defend.
-- Units ordered so are held by the operator (the AI leaves them alone) until
-- released (panel button, or /luaui zkop release).
--
-- Controls: the panel's "controls" button opens a window of the AI's settings
-- an operator may turn while it plays (greed, build power, the army's line,
-- the home defense, the defense posture), as sliders, choices and switches
-- drawn from the AI's own list of them, and its features (the dev switches).
--
-- Intel: the panel's "intel" button opens a window of the AI's readouts: the
-- map's regime (structural openness per movement tier, the deciding tier,
-- density, the fixed-defense budget they set) and what it knows and guesses
-- of the enemy (values seen, flags, the production estimate, incursions).
--
-- Text commands (/luaui zkop ...): focus <team>, layer <name|none>,
-- terrain <off|regions|holders> [tier], render <auto|screen|mesh>, release,
-- build <unitdef> (at the mouse), fac <unitdef> [count], defend (at the
-- mouse), say <text>, note <text>, feature <name> on|off, toggle <what>,
-- lever <name> <value|auto>, controls, intel.
--
-- The panel is a Chili window in Zero-K; without Chili it is drawn by hand,
-- and without GL4 shaders the value map falls back to plain quads.
--------------------------------------------------------------------------------

function widget:GetInfo()
	return {
		name    = "ZKGBAI Operator",
		desc    = "Shows what ZKGBAI AIs think (squads, intents, memory of the enemy, metal-spot links, lane fronts, threat maps, terrain segmentation, log; the map's regime and the enemy intel in a window) and lets you steer them and turn their levers and features. Needs the AI's operator API (operator=on).",
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
-- byId, status, spots, links, feed, nextSeq, overlay, terrain, levers,
-- features, regime, military}
local instances = {}
local probing = {}
local nextProbe = 0
local focus -- port of the AI shown
local focusChosen = false -- picked by hand

local show = {squads = true, intents = true, memory = true, spots = true, links = true, fronts = true, roles = true, defend = true, chokes = true, feed = true}
local SHOW_ORDER = {"squads", "intents", "memory", "spots", "links", "fronts", "roles", "defend", "chokes", "feed"}
local LAYERS = {"none", "threat", "enemy_porc", "aa_threat", "sub_threat", "value", "ally_threat", "riot"}
local layer = "none"
local SEGMENTATIONS = {"off", "regions", "holders"}
local segmentation = "off"
local TIERS = {"strategy", "spider", "bot", "vehicle", "hover", "ship", "amphib"}
local segTier = "strategy"

local flashes = {}  -- feedback marks on the ground: {x, z, t, color}
local message, messageTime, messageBad

local panel -- the Chili panel, if any (see below)
local controlsShown = false -- the controls window (levers, features) is open
local intelShown = false -- the intel window (regime, enemy) is open

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

-- onError, if given, takes the errors (they do not count against the AI).
local function poll(inst, kind, period, path, handler, onError)
	local now = clock()
	if inst.busy[kind] or (inst.due[kind] or 0) > now then
		return
	end
	inst.busy[kind] = true
	inst.due[kind] = now + period
	httpRequest(inst.port, "GET", path, nil, function(value, err)
		inst.busy[kind] = false
		if err and onError then
			onError(err)
		elseif err then
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
-- Maps on the ground, by GL4 shaders. With the map's deferred depth buffer
-- (AllowDeferredMapRendering = 1, Zero-K's own lights and distortions need
-- it too): a screen-space pass that finds each pixel's place on the map from
-- its depth and tints it, exact on the terrain. Without it: a mesh draped on
-- the terrain from the heightmap.
--------------------------------------------------------------------------------

local overlay = {} -- ok; mesh = {shader, tiles}; screen = {shader, quad}
local renderMode = "auto" -- auto, screen, mesh

-- Mode 0: a value map (red channel, 0-1; linear filtering), tinted with
-- contour lines at quarters. Mode 1: regions (id + 1 in red and green,
-- texel-exact) coloured by id or by who holds them (attrTex, a texel per
-- region: red the enemy's strength, blue ours, alpha flags), with borders.
local TINT_SRC = [[
uniform sampler2D dataTex;
uniform sampler2D attrTex;
uniform vec4 dataRect;   // origin x, z; world width, height
uniform vec4 dataInfo;   // texels wide, high; mode; alpha
uniform vec4 valueColor; // rgb; contour strength
uniform vec4 regionInfo; // colour by holders?; regions; border alpha; fill alpha

vec3 hsv2rgb(vec3 c) {
	vec3 p = abs(fract(c.xxx + vec3(0.0, 2.0 / 3.0, 1.0 / 3.0)) * 6.0 - 3.0);
	return c.z * mix(vec3(1.0), clamp(p - 1.0, 0.0, 1.0), c.y);
}

float regionAt(ivec2 t) {
	t = clamp(t, ivec2(0), ivec2(dataInfo.xy) - 1);
	vec4 c = texelFetch(dataTex, t, 0);
	return floor(c.r * 255.0 + 0.5) + 256.0 * floor(c.g * 255.0 + 0.5) - 1.0;
}

// The colour a map point gets; alpha 0 for none.
vec4 tint(vec2 worldXZ) {
	vec2 uv = (worldXZ - dataRect.xy) / dataRect.zw;
	if (uv.x < 0.0 || uv.y < 0.0 || uv.x > 1.0 || uv.y > 1.0) return vec4(0.0);
	if (dataInfo.z < 0.5) {
		float v = texture(dataTex, uv).r;
		if (v < 0.02) return vec4(0.0);
		float levels = v * 4.0;
		float f = fract(levels);
		float line = 1.0 - smoothstep(0.0, 1.5 * fwidth(levels), min(f, 1.0 - f));
		vec3 c = valueColor.rgb * (0.65 + 0.35 * v);
		float a = dataInfo.w * (0.2 + 0.6 * sqrt(v));
		return vec4(mix(c, vec3(1.0, 0.95, 0.85), line * valueColor.a), clamp(a + line * valueColor.a * 0.6, 0.0, 0.95));
	}
	vec2 p = uv * dataInfo.xy;
	ivec2 ip = ivec2(floor(p));
	vec2 f = fract(p);
	float id = regionAt(ip);
	if (id < 0.0) return vec4(0.0);
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
	return vec4(mix(c, vec3(1.0), border * 0.3), clamp(fill + border * regionInfo.z, 0.0, 0.95));
}
]]

local MESH_VS = [[
#version 420
#line 10000
layout (location = 0) in vec2 meshXZ; // world x, z
uniform sampler2D heightmapTex;
out DataVS {
	vec2 worldXZ;
};
//__ENGINEUNIFORMBUFFERDEFS__
#line 10100
void main() {
	vec2 w = meshXZ;
	vec2 uvhm = clamp(w, vec2(8.0), mapSize.xy - 8.0) / mapSize.xy;
	float y = max(0.0, textureLod(heightmapTex, uvhm, 0.0).x) + 4.0;
	worldXZ = w;
	gl_Position = cameraViewProj * vec4(w.x, y, w.y, 1.0);
}
]]

local MESH_FS = [[
#version 420
#line 20000
in DataVS {
	vec2 worldXZ;
};
out vec4 fragColor;
//__ENGINEUNIFORMBUFFERDEFS__
]] .. TINT_SRC .. [[
#line 20500
void main() {
	fragColor = tint(worldXZ);
	if (fragColor.a <= 0.0) discard;
}
]]

local SCREEN_VS = [[
#version 420
#line 30000
layout (location = 0) in vec4 posUV; // clip x, y; screen u, v
out DataVS {
	vec2 screenUV;
};
//__ENGINEUNIFORMBUFFERDEFS__
#line 30100
void main() {
	screenUV = posUV.zw;
	gl_Position = vec4(posUV.xy, 0.0, 1.0);
}
]]

-- Depth runs 0-1 in Recoil's clip space, as in Zero-K's deferred lights.
local SCREEN_FS = [[
#version 420
#line 40000
uniform sampler2D mapDepthTex;
in DataVS {
	vec2 screenUV;
};
out vec4 fragColor;
//__ENGINEUNIFORMBUFFERDEFS__
]] .. TINT_SRC .. [[
#line 40500
void main() {
	float depth = texture(mapDepthTex, screenUV).x;
	if (depth >= 1.0) discard;
	vec4 world = cameraViewProjInv * vec4(screenUV * 2.0 - 1.0, depth, 1.0);
	world.xyz /= world.w;
	fragColor = tint(world.xz);
	if (fragColor.a <= 0.0) discard;
}
]]

-- The mesh the maps are drawn on: a grid over the whole map in world
-- coordinates, cut into tiles of whole columns, since index buffers are 16
-- bit (one mesh of more than 65536 vertices loses all past the 65536th).
local function buildMeshTiles()
	local mx, mz = Game.mapSizeX, Game.mapSizeZ
	local spacing = max(24, max(mx, mz) / 400)
	local nx, nz = math.ceil(mx / spacing), math.ceil(mz / spacing)
	local colsPerTile = floor(65536 / (nz + 1)) - 1
	local tiles = {}
	for c0 = 0, nx - 1, colsPerTile do
		local c1 = min(nx, c0 + colsPerTile)
		local verts, index = {}, {}
		for c = c0, c1 do
			local x = min(mx, c * spacing)
			for r = 0, nz do
				verts[#verts + 1] = x
				verts[#verts + 1] = min(mz, r * spacing)
			end
		end
		local col = nz + 1
		for c = 0, c1 - c0 - 1 do
			for r = 0, nz - 1 do
				local q = c * col + r
				index[#index + 1] = q
				index[#index + 1] = q + 1
				index[#index + 1] = q + col
				index[#index + 1] = q + 1
				index[#index + 1] = q + col + 1
				index[#index + 1] = q + col
			end
		end
		local vbo = gl.GetVBO(GL.ARRAY_BUFFER, false)
		local ibo = gl.GetVBO(GL.ELEMENT_ARRAY_BUFFER, false)
		if not vbo or not ibo then
			return nil
		end
		vbo:Define(#verts / 2, {{id = 0, name = "meshXZ", size = 2}})
		vbo:Upload(verts)
		ibo:Define(#index)
		ibo:Upload(index)
		local vao = gl.GetVAO()
		vao:AttachVertexBuffer(vbo)
		vao:AttachIndexBuffer(ibo)
		tiles[#tiles + 1] = vao
	end
	return tiles
end

-- Two triangles over the whole screen.
local function buildScreenQuad()
	local vbo = gl.GetVBO(GL.ARRAY_BUFFER, false)
	if not vbo then
		return nil
	end
	vbo:Define(6, {{id = 0, name = "posUV", size = 4}})
	vbo:Upload({
		-1, -1, 0, 0,   -1, 1, 0, 1,   1, 1, 1, 1,
		1, 1, 1, 1,     1, -1, 1, 0,   -1, -1, 0, 0,
	})
	local vao = gl.GetVAO()
	vao:AttachVertexBuffer(vbo)
	return vao
end

local UNIFORMS = {dataRect = {0, 0, 1, 1}, dataInfo = {1, 1, 0, 0.5}, valueColor = {1, 0.5, 0.2, 0.6}, regionInfo = {0, 1, 0.8, 0.15}}

local function compile(LuaShader, name, vs, fs, ints)
	return LuaShader.CheckShaderUpdates({
		vsSrc = vs, fsSrc = fs, shaderName = name,
		uniformInt = ints, uniformFloat = UNIFORMS, shaderConfig = {},
		-- Inline sources look unchanged to the loader; compile anyway.
		forceupdate = true,
	})
end

local function deferredMap()
	return Spring.GetConfigInt("AllowDeferredMapRendering", 0) == 1
end

local function initOverlay()
	if not gl.CreateShader or not gl.GetVAO or not gl.GetVBO then
		return "no GL4 (headless?)"
	end
	-- Included from here, so that its functions land in this widget.
	local okInclude, LuaShader = pcall(function()
		return VFS.Include("LuaUI/Widgets/Include/LuaShader.lua")
	end)
	if not okInclude or not LuaShader then
		return "no LuaShader.lua"
	end
	local shader = compile(LuaShader, "ZKGBAI Operator overlay (mesh)", MESH_VS, MESH_FS, {heightmapTex = 0, dataTex = 1, attrTex = 2})
	local tiles = shader and buildMeshTiles()
	if shader and tiles then
		overlay.mesh = {shader = shader, tiles = tiles}
	end
	if deferredMap() then
		shader = compile(LuaShader, "ZKGBAI Operator overlay (screen)", SCREEN_VS, SCREEN_FS, {dataTex = 1, attrTex = 2, mapDepthTex = 3})
		local quad = shader and buildScreenQuad()
		if shader and quad then
			overlay.screen = {shader = shader, quad = quad}
		end
	end
	if not overlay.mesh and not overlay.screen then
		return "the overlay shaders did not compile"
	end
	overlay.ok = true
	spEcho(format("ZKGBAI Operator: ground maps by %s", overlay.screen and "screen-space shader (deferred map depth)" or "mesh"))
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

local function drawOverlayPass(pass, tex, rect, w, h, mode, attr, regionInfo)
	local s = pass.shader
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
	if pass.quad then
		pass.quad:DrawArrays(GL.TRIANGLES)
	else
		for i = 1, #pass.tiles do
			pass.tiles[i]:DrawElements(GL.TRIANGLES)
		end
	end
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
	local pass = overlay.screen
	if renderMode == "mesh" or not pass then
		pass = overlay.mesh
	end
	if not pass then
		return true
	end
	gl.DepthMask(false)
	gl.Culling(false)
	gl.Blending(GL.SRC_ALPHA, GL.ONE_MINUS_SRC_ALPHA)
	if pass.quad then
		-- Every pixel finds its own depth; nothing to test against.
		glDepthTest(false)
		gl.Texture(3, "$map_gbuffer_zvaltex")
	else
		glDepthTest(GL.LEQUAL)
		gl.Texture(0, "$heightmap")
	end
	pass.shader:Activate()
	if regions then
		drawOverlayPass(pass, t.tex, t.rect, t.w, t.h, 1, t.attr, {segmentation == "holders" and 1 or 0, t.attrN, 0.8, 0.14})
	end
	if values then
		drawOverlayPass(pass, ov.tex, ov.rect, ov.w, ov.h, 0)
	end
	pass.shader:Deactivate()
	for unit = 0, 3 do
		gl.Texture(unit, false)
	end
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
	if inst.linkList then
		gl.DeleteList(inst.linkList)
	end
	inst.overlayGl, inst.terrain, inst.quadList, inst.overlay = nil, nil, nil, nil
	inst.linkList, inst.linkSig = nil, nil
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

local panelWanted = false -- a Chili panel, once there is an AI to show
local makeChiliPanel -- (below)

local function discover()
	local now = clock()
	if now < nextProbe then
		return
	end
	-- Less often in games without ZKGBAI.
	nextProbe = now + ((now > 60 and next(instances) == nil) and 15 or 5)
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
					if panelWanted and not panel then
						local ok, err = pcall(function()
							panel = makeChiliPanel()
						end)
						if not ok then
							spEcho("ZKGBAI Operator: no Chili panel: " .. tostring(err))
							panel, panelWanted = nil, false
						end
					end
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
				-- The links' ground flags change after terrain re-analyses.
				if not inst.spots or show.links then
					poll(inst, "map", inst.spots and 30 or 10, "/call/map_info?heights=false", function(v)
						inst.spots = v.spots
						inst.links = v.links
						inst.linksDirty = true
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
				if controlsShown and panel then
					-- Before the AI's modules run, the levers are an error.
					poll(inst, "levers", 2, "/call/levers", function(v)
						inst.levers, inst.features, inst.leverError, inst.leversChanged = v.levers, v.features, nil, true
					end, function(err)
						inst.leverError = err
					end)
				end
				-- An AI built before the regime tool answers with an error.
				if (intelShown and panel) or show.fronts then
					poll(inst, "regime", 3, "/call/regime", function(v)
						inst.regime, inst.regimeError = v, nil
					end, function(err)
						inst.regimeError = err
					end)
				end
				if intelShown and panel then
					poll(inst, "military", 3, "/call/military", function(v)
						inst.military, inst.militaryError = v, nil
					end, function(err)
						inst.militaryError = err
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

--------------------------------------------------------------------------------
-- Levers: the AI's settings an operator may turn (its `levers` tool)
--------------------------------------------------------------------------------

local function leverText(l, v)
	if v == nil then
		return "auto"
	elseif l.kind == "switch" then
		return v and "on" or "off"
	elseif l.kind == "choice" then
		return tostring(v)
	elseif l.format == "percent" then
		return format("%d%%", floor(v * 100 + 0.5))
	elseif l.format == "count" then
		return tostring(floor(v + 0.5))
	end
	return format("x%.2f", v)
end

-- A range lever's slider runs over log(value) when spaced by ratio.
local function leverPos(l, v)
	return l.log and math.log(v) or v
end

local function leverValue(l, pos)
	local v = l.log and math.exp(pos) or pos
	return max(l.min, min(l.max, floor(v / l.step + 0.5) * l.step))
end

local function turnLever(inst, name, value)
	local label = name
	for _, l in ipairs(inst.levers or {}) do
		if l.name == name then
			label = l.label
		end
	end
	inst.leverSending = inst.leverSending or {}
	inst.leverSending[name] = true
	httpRequest(inst.port, "POST", "/call/lever", json.encode({name = name, value = value}), function(v, err)
		inst.leverSending[name] = nil
		inst.leversChanged = true
		if err then
			say(label .. ": " .. tostring(err), true)
			return
		end
		for i, l in ipairs(inst.levers or {}) do
			if l.name == v.name then
				inst.levers[i] = v
			end
		end
		say(label .. ": " .. leverText(v, v.value) .. (v.auto and " (auto)" or ""))
	end)
end

local function turnFeature(inst, name, off)
	inst.featureSending = inst.featureSending or {}
	inst.featureSending[name] = true
	httpRequest(inst.port, "POST", "/call/feature", json.encode({name = name, off = off}), function(v, err)
		inst.featureSending[name] = nil
		inst.leversChanged = true
		if err then
			say(name .. ": " .. tostring(err), true)
			return
		end
		local now = {}
		for _, f in ipairs(v.features_off or {}) do
			now[f] = true
		end
		for _, f in ipairs(inst.features or {}) do
			f.off = now[f.name] or false
		end
		say(name .. ": " .. (now[name] and "off" or "on") .. (v.warning and (" · " .. v.warning) or ""), v.warning ~= nil)
	end)
end

local function showControls(on)
	controlsShown = on
	local inst = focused()
	if inst then
		inst.due.levers = 0
	end
end

local function showIntel(on)
	intelShown = on
	local inst = focused()
	if inst then
		inst.due.regime, inst.due.military = 0, 0
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
		setLayer(words[3])
	elseif cmd == "terrain" then
		setSegmentation(words[3], words[4])
	elseif cmd == "render" then
		renderMode = words[3] or "auto"
		say("ground maps by " .. ((renderMode ~= "mesh" and overlay.screen) and "screen-space shader" or "mesh"))
	elseif cmd == "toggle" and words[3] and show[words[3]] ~= nil then
		show[words[3]] = not show[words[3]]
	elseif cmd == "release" then
		releaseSelected()
	elseif cmd == "levers" or cmd == "controls" then
		showControls(not controlsShown)
	elseif cmd == "intel" then
		showIntel(not intelShown)
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
		turnFeature(inst, words[3], words[4] ~= "on")
	elseif cmd == "lever" and words[3] then
		turnLever(inst, words[3], tonumber(words[4]) or words[4] or "auto")
	else
		say("zkop: focus <team> | layer <name> | terrain <off|regions|holders> [tier] | render <auto|screen|mesh> | toggle <what> | release | build <def> | defend | fac <def> [n] | say | note | feature <name> on|off | controls | intel | lever <name> <value|auto>", true)
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

-- Above the ground or the water, whichever is higher (spots float).
local function surfaceY(x, z)
	return max(spGetGroundHeight(x, z), 0) + 8
end

-- Which side holds a spot (snapshot code): 1 ours, 2 theirs, 0 nobody.
local function spotSide(code)
	if code == 1 or code == 4 then
		return 1
	elseif code == 2 or code == 3 then
		return 2
	end
	return 0
end

-- The metal-spot graph's Delaunay links, coloured by who holds their ends
-- (ours, theirs, orange across the front), faint where ground forces do not
-- walk the link (territory is not inferred along it). A display list, built
-- again when the owners or the links change.
local function drawLinks(inst, s)
	local spots, links = inst.spots, inst.links
	if inst.linkSnap ~= s or inst.linksDirty then
		inst.linkSnap, inst.linksDirty = s, false
		local parts = {table.concat(s.spots, "")}
		for i = 1, #links do
			parts[#parts + 1] = tostring(links[i][3])
		end
		local sig = table.concat(parts, ",")
		if sig ~= inst.linkSig then
			inst.linkSig = sig
			if inst.linkList then
				gl.DeleteList(inst.linkList)
			end
			inst.linkList = gl.CreateList(function()
				for i = 1, #links do
					local l = links[i]
					local a, b = spots[l[1] + 1], spots[l[2] + 1]
					if a and b then
						local sa, sb = spotSide(s.spots[l[1] + 1]), spotSide(s.spots[l[2] + 1])
						local ours, theirs = sa == 1 or sb == 1, sa == 2 or sb == 2
						local c = (ours and theirs) and HEAT or ours and OURS or theirs and ENEMY or INK
						local walked = l[3] ~= 0
						glLineWidth(walked and 2 or 1)
						glColor(c[1], c[2], c[3], walked and ((ours or theirs) and 0.55 or 0.25) or 0.15)
						local len = sqrt((b[1] - a[1]) ^ 2 + (b[2] - a[2]) ^ 2)
						local n = max(1, floor(len / 48))
						glBeginEnd(GL_LINE_STRIP, function()
							for k = 0, n do
								local x, z = a[1] + (b[1] - a[1]) * k / n, a[2] + (b[2] - a[2]) * k / n
								glVertex(x, surfaceY(x, z), z)
							end
						end)
					end
				end
			end)
		end
	end
	if inst.linkList then
		gl.CallList(inst.linkList)
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
	if show.links and inst.links and inst.spots and s.spots then
		drawLinks(inst, s)
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

-- The defense posture's lane fronts for the deciding movement tier: a ring
-- as wide as the front, a diamond at each of its defense sites.
local function drawFronts(inst)
	local r = inst.regime
	for _, l in ipairs(r and r.lanes or {}) do
		if l.decides then
			for _, f in ipairs(l.fronts or {}) do
				local x, z = f.pos[1], f.pos[2]
				glLineWidth(3)
				glColor(ACCENT[1], ACCENT[2], ACCENT[3], 0.8)
				glDrawGroundCircle(x, 0, z, max(40, (f.span or 0) / 2), 48)
				glLineWidth(2)
				for _, site in ipairs(f.sites or {}) do
					glDrawGroundCircle(site[1], 0, site[2], 30, 4)
				end
				if cameraNear(x, z, 4000) then
					worldText(format("%s lane front · %d across · %d sites", l.tier, f.span or 0, #(f.sites or {})),
						x, groundY(x, z) + 40, z, 16, ACCENT)
				end
			end
		end
	end
end

local function drawWorld(inst)
	local s = inst.snap
	if show.chokes then
		drawChokes(inst)
	end
	if show.fronts then
		drawFronts(inst)
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

local GREY, WHITE, GREEN, RED = "\255\140\140\140", "\255\235\235\230", "\255\30\200\140", "\255\230\100\100"

local function n0(v)
	return type(v) == "number" and format("%d", floor(v + 0.5)) or "-"
end

local function n2(v)
	return type(v) == "number" and format("%.2f", v) or "-"
end

local function pct(v)
	return type(v) == "number" and format("%d%%", floor(v * 100 + 0.5)) or "-"
end

-- The deciding tier's lanes, if measured.
local function decidingLanes(r)
	for _, l in ipairs(r and r.lanes or {}) do
		if l.decides then
			return l
		end
	end
end

local function statusLines(inst)
	local s, st = inst.snap, inst.status
	if not s then
		return inst.lastError and ("waiting: " .. inst.lastError) or "waiting for a snapshot…"
	end
	local m, e = s.metal or {}, s.energy or {}
	local army = st and st.army
	local terr = st and st.territory
	local held = #(st and st.operator and st.operator.reserved or {})
	local text = format("metal %.1f/s (%d)   energy %.1f/s (%d)\narmy %s vs enemy seen %s   squads %d\nspots %s:%s   held by operator %d   %s",
		m[2] or 0, m[1] or 0, e[2] or 0, e[1] or 0,
		army and floor(army.value) or "?", army and floor(army.enemy_fighter_value) or "?", #s.squads,
		terr and terr.spots_ours or "?", terr and terr.spots_enemy or "?", held, timeText(s.frame or 0))
	local r = inst.regime
	if r and r.posture ~= false then
		local l = decidingLanes(r)
		text = text .. format("\n%s openness %s · density %s/%s · defense %s",
			r.tier or "no tier yet", l and n2(l.openness) or "-", n0(r.density), n0(r.density_ref), pct(r.share))
	end
	return text
end

-- A window of its own beside the panel, closed by its button (the close
-- happens on the next update, not inside Chili's click handler).
local function sideWindow(Chili, key, caption, default, onClose)
	local vsx, vsy = Spring.GetViewGeometry()
	local w = config[key] or default
	local window = Chili.Window:New{
		parent = Chili.Screen0,
		caption = caption,
		x = min(w.x, vsx - 120), y = min(w.y, vsy - 120), width = w.width, height = w.height,
		minWidth = 260, minHeight = 200,
		draggable = true, resizable = true,
	}
	Chili.Button:New{parent = window, y = 0, right = 0, width = 24, height = 20, caption = "x", tooltip = "Close", OnClick = {onClose}}
	return window
end

-- The controls: the AI's levers (a row each, built from its list of them,
-- rebuilt when that changes shape), then its features (a switch each).
-- Chili's setters call the change listeners too, so values shown from the
-- AI are set under `syncing`; a slider moved by hand is sent once the
-- mouse lets go.
local function makeControls(Chili, font)
	local c = {}
	local window = sideWindow(Chili, "controlsWin", "ZKGBAI controls", {x = 376, y = 140, width = 340, height = 560}, function()
		controlsShown = false
	end)
	c.header = Chili.Label:New{parent = window, x = 4, y = 18, right = 28, height = 18, caption = "", objectOverrideFont = font}
	local leverScroll = Chili.ScrollPanel:New{parent = window, x = 4, y = 40, right = 4, bottom = 4, horizontalScrollbar = false}
	local leverRows, featureRows, leverShape, leverPort = {}, {}, nil, nil
	local syncing = false
	local moved = {} -- name -> {pos, t}: slid by hand, not sent yet

	local function caption(l, v, auto)
		local text = leverText(l, v)
		if auto and v ~= nil then
			text = text .. " auto"
		end
		return WHITE .. l.label .. "  " .. (auto and GREY or GREEN) .. text
	end

	local function featureCaption(f)
		local color = f.off and GREY or WHITE
		if (f.off and true or false) ~= (f.default_off and true or false) then
			color = GREEN
		end
		return color .. f.name .. (f.off and " (off)" or "")
	end

	local function shapeOf(levers, features)
		local parts = {}
		for i, l in ipairs(levers) do
			parts[i] = l.name .. ":" .. l.kind .. (l.note and "+" or "")
		end
		for _, f in ipairs(features or {}) do
			parts[#parts + 1] = f.name
		end
		return table.concat(parts, ",")
	end

	local function placeholder(text)
		leverScroll:ClearChildren()
		leverRows, featureRows = {}, {}
		Chili.Label:New{parent = leverScroll, x = 0, y = 4, right = 0, height = 18, caption = GREY .. text, objectOverrideFont = font}
	end

	local function build(levers, features)
		leverScroll:ClearChildren()
		leverRows, featureRows = {}, {}
		syncing = true
		local y, group = 0, nil
		for _, l in ipairs(levers) do
			if l.group ~= group then
				group = l.group
				Chili.Label:New{parent = leverScroll, x = 0, y = y + 4, right = 0, height = 18, caption = GREY .. group, objectOverrideFont = font}
				y = y + 22
			end
			local row = {lever = l}
			row.label = Chili.Label:New{
				parent = leverScroll, x = 0, y = y + 3, width = "50%", height = 20,
				caption = l.label, tooltip = l.help, objectOverrideFont = font,
			}
			if l.kind == "range" then
				row.bar = Chili.Trackbar:New{
					parent = leverScroll, x = "51%", y = y + 2, right = 44, height = 20,
					min = leverPos(l, l.min), max = leverPos(l, l.max), step = l.log and 0.01 or l.step,
					value = leverPos(l, l.value or (l.min + l.max) / 2),
					tooltipFunction = function(_, pos)
						return leverText(l, leverValue(l, pos)) .. ": " .. l.help
					end,
					OnChange = {function(_, pos)
						if not syncing then
							moved[l.name] = {pos = pos, t = clock()}
							row.label:SetCaption(caption(l, leverValue(l, pos), false))
						end
					end},
				}
				row.reset = Chili.Button:New{
					parent = leverScroll, y = y, right = 0, width = 42, height = 24, caption = "auto",
					tooltip = "Back to the AI's own setting", objectOverrideFont = font,
					OnClick = {function()
						local inst = focused()
						if inst then
							moved[l.name] = nil
							turnLever(inst, l.name, "auto")
						end
					end},
				}
			elseif l.kind == "choice" then
				row.box = Chili.ComboBox:New{
					parent = leverScroll, x = "51%", y = y, right = 0, height = 24, items = l.options, selected = 1,
					objectOverrideFont = font,
					OnSelect = {function(_, i)
						local inst = focused()
						if not syncing and inst then
							turnLever(inst, l.name, l.options[i])
						end
					end},
				}
			else
				row.check = Chili.Checkbox:New{
					parent = leverScroll, x = "51%", y = y + 2, right = 0, height = 20, caption = "", checked = l.value and true or false,
					OnChange = {function(_, on)
						local inst = focused()
						if not syncing and inst then
							turnLever(inst, l.name, on)
						end
					end},
				}
			end
			y = y + 26
			if l.note then
				row.note = Chili.TextBox:New{parent = leverScroll, x = 4, y = y, right = 0, text = "", fontsize = 11}
				y = y + 30
			end
			leverRows[l.name] = row
		end
		if features and #features > 0 then
			Chili.Label:New{parent = leverScroll, x = 0, y = y + 8, right = 0, height = 18, caption = WHITE .. "Features", objectOverrideFont = font}
			Chili.TextBox:New{parent = leverScroll, x = 0, y = y + 28, right = 0, fontsize = 11,
				text = GREY .. "The AI's switches, on when ticked; hover for what each does. Green: not its default."}
			y = y + 58
			group = nil
			local col = 0
			for _, f in ipairs(features) do
				if f.group ~= group then
					group = f.group
					if col == 1 then
						y = y + 22
					end
					col = 0
					Chili.Label:New{parent = leverScroll, x = 0, y = y + 4, right = 0, height = 16, caption = GREY .. group, fontsize = 11}
					y = y + 20
				end
				local x = col == 0 and 0 or "50%"
				local row = {feature = f}
				local tip = (f.does ~= "" and f.does or f.name) .. (f.default_off and " (off by default)" or "")
				row.check = Chili.Checkbox:New{
					parent = leverScroll, x = x, y = y + 1, width = 20, height = 20, caption = "", checked = not f.off,
					tooltip = tip,
					OnChange = {function(_, on)
						local inst = focused()
						if not syncing and inst then
							turnFeature(inst, f.name, not on)
						end
					end},
				}
				row.label = Chili.Label:New{
					parent = leverScroll, x = col == 0 and 22 or "57%", y = y + 3, width = "42%", height = 18,
					caption = featureCaption(f), tooltip = tip, objectOverrideFont = font,
				}
				featureRows[f.name] = row
				if col == 0 then
					col = 1
				else
					col, y = 0, y + 22
				end
			end
		end
		syncing = false
	end

	local function sync(inst)
		syncing = true
		local sending = inst.leverSending or {}
		for _, l in ipairs(inst.levers) do
			local row = leverRows[l.name]
			if row and not moved[l.name] and not sending[l.name] then
				row.lever = l
				row.label:SetCaption(caption(l, l.value, l.auto))
				if row.bar then
					local pos = l.value and leverPos(l, l.value) or (row.bar.min + row.bar.max) / 2
					if math.abs(row.bar.value - pos) > 1e-4 then
						row.bar:SetValue(pos)
					end
					row.reset:SetCaption(l.auto and (GREY .. "auto") or "auto")
				elseif row.box then
					for i = 1, #l.options do
						if l.options[i] == l.value and row.box.selected ~= i then
							row.box:Select(i)
						end
					end
				elseif row.check and row.check.checked ~= (l.value and true or false) then
					row.check:Toggle() -- (the older Chili has no SetToggle)
				end
				if row.note then
					row.note:SetText(GREY .. (l.note or ""))
				end
			end
		end
		local fsending = inst.featureSending or {}
		for _, f in ipairs(inst.features or {}) do
			local row = featureRows[f.name]
			if row and not fsending[f.name] then
				if row.check.checked == (f.off and true or false) then
					row.check:Toggle()
				end
				row.label:SetCaption(featureCaption(f))
			end
		end
		syncing = false
	end

	-- Every frame: send what was slid by hand once the mouse let go.
	function c.flush()
		local inst = focused()
		if not inst or next(moved) == nil then
			return
		end
		local _, _, held = spGetMouseState()
		if held then
			return
		end
		for name, m in pairs(moved) do
			if clock() - m.t > 0.25 then
				moved[name] = nil
				local row = leverRows[name]
				if row then
					turnLever(inst, name, leverValue(row.lever, m.pos))
				end
			end
		end
	end

	function c.update(inst)
		c.header:SetCaption(format("team %d · port %d", inst.team, inst.port))
		if not inst.levers then
			local text = inst.leverError and ("levers: " .. inst.leverError) or "asking the AI for its levers…"
			if leverShape ~= text then
				leverShape = text
				placeholder(text)
			end
			return
		end
		local shape = shapeOf(inst.levers, inst.features)
		if shape ~= leverShape and next(moved) == nil then
			leverShape = shape
			build(inst.levers, inst.features)
			inst.leversChanged = true
		end
		if inst.leversChanged or leverPort ~= inst.port then
			inst.leversChanged, leverPort = false, inst.port
			sync(inst)
		end
	end

	function c.save()
		config.controlsWin = {x = window.x, y = window.y, width = window.width, height = window.height}
	end
	function c.dispose()
		c.save()
		window:Dispose()
	end
	return c
end

-- The readouts: the map's regime (`regime`) and what the AI knows and
-- guesses of the enemy (`military`), as text under two bars.
local function regimeLines(inst, add)
	local r = inst.regime
	add(WHITE .. "Regime")
	if not r then
		add(GREY .. (inst.regimeError and ("this AI has no regime readout: " .. inst.regimeError) or "asking the AI…"))
		return
	elseif r.posture == false then
		add(GREY .. (r.note or "the posture is off in this game: lanes and density are not measured"))
		return
	end
	local enemy = (r.enemy_tiers and #r.enemy_tiers > 0) and (GREY .. "  (enemy army: " .. table.concat(r.enemy_tiers, ", ") .. ")") or ""
	add(GREY .. "deciding tier " .. WHITE .. tostring(r.tier or "none yet") .. enemy)
	if #(r.lanes or {}) == 0 then
		add(GREY .. "lanes not measured yet (they wait for both starts and the terrain)")
	end
	for _, l in ipairs(r.lanes or {}) do
		add((l.decides and GREEN or WHITE) .. l.tier .. GREY .. format(": openness %s (perimeter %s, closure %s) · lanes %s · %d fronts keep %d of %d spots",
			n2(l.openness), n2(l.perimeter), n2(l.closure), n2(l.laneness), #(l.fronts or {}), l.spots_kept or 0, l.spots_ours or 0))
	end
	add(GREY .. "density " .. WHITE .. n0(r.density) .. GREY .. " of " .. n0(r.density_ref) .. " metal per sq km (both sides' standing metal over usable ground) · map richness " .. n2(r.richness))
	add(GREY .. "defense share " .. WHITE .. pct(r.share) .. GREY .. " of " .. n0(r.standing) .. " standing (cap " .. pct(r.cap) .. "): posture turrets "
		.. WHITE .. n0(r.defense) .. GREY .. " of a " .. n0(r.budget) .. " budget · all fixed defenses " .. n0(r.all_defense))
	add(GREY .. format("posture orders: %d front, %d point, %d stale", r.front_orders or 0, r.point_orders or 0, r.stale or 0))
end

local function intelLines(inst, add)
	local m = inst.military
	add(WHITE .. "Intel")
	if not m then
		add(GREY .. (inst.militaryError and ("military: " .. inst.militaryError) or "asking the AI…"))
		return
	end
	local e, i, o = m.enemy or {}, m.intel or {}, m.ours or {}
	local flags = {}
	for _, f in ipairs({{"nuke", e.has_nuke}, {"anti-nuke", e.has_anti_nuke}, {"troll commander", e.troll_commander}, {"slasher spam", e.slasher_spam}}) do
		flags[#flags + 1] = (f[2] and RED or GREY) .. f[1]
	end
	add(table.concat(flags, GREY .. " · "))
	add(GREY .. "army seen " .. WHITE .. n0(e.fighter_value) .. GREY .. " · heavies " .. n0(e.heavy_value) .. " · air " .. n0(e.max_air_value)
		.. format(" · %d%% outranges our light raiders", floor((e.outranging_share or 0) * 100 + 0.5)))
	add(GREY .. "defenses seen " .. WHITE .. n0(e.defense_value) .. GREY .. " (light " .. n0(e.light_defense_value) .. ", heavy "
		.. n0(e.heavy_defense_value) .. ") · " .. tostring(e.defenses or 0) .. " of " .. tostring(e.known or 0) .. " known")
	add(GREY .. "produced (estimate) " .. WHITE .. n0(i.enemy_produced) .. GREY .. format(" at %.1f metal/s · killed ", i.enemy_income or 0) .. n0(i.killed))
	add(GREY .. "unaccounted for " .. WHITE .. n0(i.unaccounted) .. GREY .. " (in the fog) · suspicious " .. n0(i.suspicious))
	add(GREY .. "ours: army " .. WHITE .. n0(o.army_value) .. GREY .. " · mobile threat " .. n0(o.available_mobile_threat) .. " · raiders " .. n0(o.available_raider_threat))
	local inc = m.incursions or {}
	if #inc == 0 then
		add(GREY .. "incursions: none")
	else
		local parts = {}
		for _, x in ipairs(inc) do
			parts[#parts + 1] = format("%s x%d (exposed %s)", string.lower(tostring(x.tier)), x.units or 0, n0(x.exposed))
		end
		add(GREY .. format("incursions %d, %d blocked: ", #inc, #(m.blocks or {})) .. WHITE .. table.concat(parts, GREY .. " · " .. WHITE))
	end
	add(GREY .. format("places defended: %d", #(m.defend or {})))
end

local function makeIntel(Chili, font)
	local c = {}
	local window = sideWindow(Chili, "intelWin", "ZKGBAI intel", {x = 376, y = 140, width = 380, height = 470}, function()
		intelShown = false
	end)
	c.header = Chili.Label:New{parent = window, x = 4, y = 18, right = 28, height = 18, caption = "", objectOverrideFont = font}
	c.density = Chili.Progressbar:New{parent = window, x = 4, y = 40, right = 4, height = 18, min = 0, max = 3000, value = 0, caption = "density", color = {0.85, 0.35, 0.15, 1}}
	c.share = Chili.Progressbar:New{parent = window, x = 4, y = 62, right = 4, height = 18, min = 0, max = 1, value = 0, caption = "defense share", color = {0.10, 0.62, 0.44, 1}}
	local scroll = Chili.ScrollPanel:New{parent = window, x = 4, y = 86, right = 4, bottom = 4, horizontalScrollbar = false}
	c.text = Chili.TextBox:New{parent = scroll, x = 0, y = 0, right = 0, text = "", fontsize = 12}
	local shown

	function c.update(inst)
		c.header:SetCaption(format("team %d · port %d", inst.team, inst.port))
		local r = inst.regime
		if r and r.posture ~= false then
			local ref = r.density_ref or 1500
			local top = max(ref * 2, (r.density or 0) * 1.1)
			c.density:SetMinMax(0, top)
			c.density:SetValue(r.density or 0)
			c.density:SetCaption(format("density %s of %s (the bar runs to twice it)", n0(r.density), n0(ref)))
			local cap = (r.cap and r.cap > 0) and r.cap or 0.25
			c.share:SetMinMax(0, cap)
			c.share:SetValue(min(cap, r.share or 0))
			c.share:SetCaption(format("defense share %s of a %s cap", pct(r.share), pct(cap)))
		else
			c.density:SetValue(0)
			c.density:SetCaption("density: no readout")
			c.share:SetValue(0)
			c.share:SetCaption("defense share: no readout")
		end
		local lines = {}
		local function add(text)
			lines[#lines + 1] = text
		end
		regimeLines(inst, add)
		add("")
		intelLines(inst, add)
		local text = table.concat(lines, "\n")
		if text ~= shown then
			shown = text
			c.text:SetText(text)
		end
	end

	function c.save()
		config.intelWin = {x = window.x, y = window.y, width = window.width, height = window.height}
	end
	function c.dispose()
		c.save()
		window:Dispose()
	end
	return c
end

function makeChiliPanel()
	local Chili = WG.Chili
	if not Chili then
		return nil
	end
	local vsx, vsy = Spring.GetViewGeometry()
	local w = config.win or {x = 8, y = 140, width = 360, height = 500}
	local p = {}
	local font = WG.GetFont and WG.GetFont() or nil
	local window = Chili.Window:New{
		parent = Chili.Screen0,
		caption = "ZKGBAI Operator",
		x = min(w.x, vsx - 120), y = min(w.y, vsy - 120), width = w.width, height = w.height,
		minWidth = 280, minHeight = 300,
		draggable = true, resizable = true,
	}
	p.window = window
	p.header = Chili.Label:New{parent = window, x = 4, y = 18, right = 4, height = 18, caption = "looking for ZKGBAI on ports 7460-7467…", objectOverrideFont = font}
	p.stats = Chili.TextBox:New{parent = window, x = 4, y = 40, right = 4, height = 60, text = "", fontsize = 12}
	p.checks = {}
	local rows = floor((#SHOW_ORDER + 3) / 4)
	for i, key in ipairs(SHOW_ORDER) do
		local col, row = (i - 1) % 4, floor((i - 1) / 4)
		p.checks[key] = Chili.Checkbox:New{
			parent = window,
			x = tostring(col * 25) .. "%", y = 104 + row * 22, width = "24%", height = 20,
			caption = key, checked = show[key], objectOverrideFont = font,
			OnChange = {function(_, value)
				show[key] = value
				config.show = show
			end},
		}
	end
	local y = 106 + rows * 22
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
	p.layer = combo(y, "map", LAYERS, layer, function(v)
		setLayer(v)
		config.layer = v
	end)
	p.seg = combo(y + 28, "terrain", SEGMENTATIONS, segmentation, function(v)
		setSegmentation(v)
		config.segmentation = v
	end, 64, "38%")
	p.tier = combo(y + 28, nil, TIERS, segTier, function(v)
		setSegmentation(segmentation, v)
		config.tier = v
	end, "62%", "36%")
	y = y + 58
	p.focusButton = Chili.Button:New{
		parent = window, x = 4, y = y, width = "24%", height = 24, caption = "AI: -", objectOverrideFont = font,
		tooltip = "The AI shown (cycles when there are several)",
		OnClick = {function() cycleFocus() end},
	}
	Chili.Button:New{
		parent = window, x = "25%", y = y, width = "25%", height = 24, caption = "release", objectOverrideFont = font,
		tooltip = "Hand the selected units (or all held ones) back to the AI",
		OnClick = {releaseSelected},
	}
	p.controlsButton = Chili.Button:New{
		parent = window, x = "51%", y = y, width = "24%", height = 24, caption = "controls", objectOverrideFont = font,
		tooltip = "The AI's settings and features you may turn while it plays",
		OnClick = {function()
			showControls(not controlsShown)
		end},
	}
	p.intelButton = Chili.Button:New{
		parent = window, x = "76%", y = y, right = 4, height = 24, caption = "intel", objectOverrideFont = font,
		tooltip = "The map's regime (openness, density, the defense budget) and what the AI knows of the enemy",
		OnClick = {function()
			showIntel(not intelShown)
		end},
	}
	p.message = Chili.Label:New{parent = window, x = 4, y = y + 28, right = 4, height = 16, caption = "", objectOverrideFont = font}
	local scroll = Chili.ScrollPanel:New{parent = window, x = 4, y = y + 48, right = 4, bottom = 4, horizontalScrollbar = false}
	p.feed = Chili.TextBox:New{parent = scroll, x = 0, y = 0, right = 0, text = "", fontsize = 11}
	p.scroll = scroll

	-- The side windows follow the flags (buttons, text commands, their own
	-- close buttons); a window that fails to build says why and stays shut.
	local controls, intel
	function p.syncWindows()
		if controlsShown and not controls then
			local ok, err = pcall(function()
				controls = makeControls(Chili, font)
			end)
			if not ok then
				controlsShown = false
				say("no controls window: " .. tostring(err), true)
			end
		elseif not controlsShown and controls then
			controls.dispose()
			controls = nil
		end
		if intelShown and not intel then
			local ok, err = pcall(function()
				intel = makeIntel(Chili, font)
			end)
			if not ok then
				intelShown = false
				say("no intel window: " .. tostring(err), true)
			end
		elseif not intelShown and intel then
			intel.dispose()
			intel = nil
		end
		local state = tostring(controlsShown) .. tostring(intelShown)
		if state ~= p.windowState then
			p.windowState = state
			p.controlsButton:SetCaption(controlsShown and (GREEN .. "controls") or "controls")
			p.intelButton:SetCaption(intelShown and (GREEN .. "intel") or "intel")
		end
	end

	function p.flushLevers()
		if controls then
			controls.flush()
		end
	end

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
		-- A side window that fails to update says why and closes.
		if controls then
			local ok, err = pcall(controls.update, inst)
			if not ok then
				say("controls: " .. tostring(err), true)
				controlsShown = false
			end
		end
		if intel then
			local ok, err = pcall(intel.update, inst)
			if not ok then
				say("intel: " .. tostring(err), true)
				intelShown = false
			end
		end
		local feed = inst.feed
		local last = feed[#feed]
		local key = (last and last.f or -1) .. ":" .. #feed
		if show.feed and (key ~= shownFeed or shownPort ~= inst.port) then
			shownFeed, shownPort = key, inst.port
			local lines = {}
			for i = max(1, #feed - 40), #feed do
				lines[#lines + 1] = GREY .. timeText(feed[i].f or 0) .. "  " .. WHITE .. feed[i].text
			end
			p.feed:SetText(table.concat(lines, "\n"))
			-- To the bottom; Chili clamps only once laid out (an unclamped
			-- huge value breaks its drawing), so not before.
			local sc = p.scroll
			if sc.contentArea and sc.clientArea then
				sc:SetScrollPos(nil, sc.contentArea[4], false, true)
			else
				shownFeed = nil
			end
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
		p.syncWindows()
	end
	function p.save()
		config.win = {x = window.x, y = window.y, width = window.width, height = window.height}
		if controls then
			controls.save()
		end
		if intel then
			intel.save()
		end
	end
	function p.dispose()
		p.save()
		if controls then
			controls.dispose()
		end
		if intel then
			intel.dispose()
		end
		window:Dispose()
	end
	p.sync()
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
	config.controls, config.intel = controlsShown, intelShown
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
	controlsShown = (config.controls or config.levers) and true or false
	intelShown = config.intel and true or false
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
	-- The panel appears with the first AI found (nothing shows in other games).
	panelWanted = WG.Chili ~= nil
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
	for _, pass in pairs({overlay.mesh, overlay.screen}) do
		pass.shader:Delete()
	end
end

local nextPanelUpdate = 0
local function tick()
	discover()
	pollInstances()
	pumpRequests()
	if panel then
		panel.syncWindows()
		panel.flushLevers()
		if clock() > nextPanelUpdate then
			nextPanelUpdate = clock() + 0.5
			panel.update()
		end
	end
end

function widget:Update()
	tick()
end

-- Also between frames, should Update not run (a headless engine).
function widget:GameFrame()
	tick()
end
