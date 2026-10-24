-- Installed into the write-dir by tools/arena/loadmatch.sh for a resumed
-- (loaded) game: restores the arena sim speed, which a load does not take
-- from the saved MODOPTIONS (the resumed game ran at 1x), and logs which
-- LuaUI callins a loaded game delivers (ARENA_LOAD_* lines).
function widget:GetInfo()
	return {
		name    = "Arena Loader",
		desc    = "Sets the sim speed and reports callins after a save game load",
		layer   = 0,
		enabled = true,
	}
end

local SPEED = @SPEED@
local MAX_FRAMES = @MAX_FRAMES@
local seenFrame, seenUpdate, done = false, false, false

function widget:Initialize()
	Spring.Echo("ARENA_LOAD_INIT f=" .. Spring.GetGameFrame() .. " speed=" .. SPEED)
	if SPEED > 0 then
		Spring.SendCommands("setminspeed " .. SPEED, "setmaxspeed " .. SPEED)
	end
end

function widget:GameFrame(n)
	if not seenFrame then
		seenFrame = true
		Spring.Echo("ARENA_LOAD_GAMEFRAME f=" .. n)
	end
	if not done and MAX_FRAMES > 0 and n >= MAX_FRAMES then
		done = true
		Spring.Echo("ARENA_TIMEOUT f=" .. n)
		Spring.SendCommands("quitforce")
	end
end

function widget:Update(dt)
	if not seenUpdate then
		seenUpdate = true
		Spring.Echo("ARENA_LOAD_UPDATE f=" .. Spring.GetGameFrame())
	end
end
