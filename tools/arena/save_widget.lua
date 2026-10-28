-- Installed into an arena match's write-dir by tools/arena/savematch.sh:
-- saves the game once at frame @SAVE_FRAME@ as Saves/@SAVE_NAME@.ssf
-- (engine creg save, the console "save <name> -y" command), for load tests with
-- tools/arena/loadmatch.sh; with SAVE_EVERY > 0, again every that many frames
-- after it, as Saves/arena_<frame>.ssf (scenario harvests).
function widget:GetInfo()
	return {
		name    = "Arena Saver",
		desc    = "Saves the game at a fixed frame (and every so often after) for headless load tests",
		layer   = 0,
		enabled = true,
	}
end

-- Substituted by savematch.sh; 0 = never.
local SAVE_FRAME = @SAVE_FRAME@
local SAVE_NAME = "@SAVE_NAME@"
local SAVE_EVERY = @SAVE_EVERY@
local done = false

function widget:GameFrame(n)
	if SAVE_FRAME <= 0 or n < SAVE_FRAME then
		return
	end
	local name
	if SAVE_EVERY > 0 then
		if (n - SAVE_FRAME) % SAVE_EVERY ~= 0 then
			return
		end
		name = "arena_" .. n
	else
		if done or n ~= SAVE_FRAME then
			return
		end
		done = true
		name = SAVE_NAME
	end
	Spring.Echo("ARENA_SAVE f=" .. n .. " name=" .. name)
	-- Engine form: /save <name> [description]; ZK passes "-y" as the
	-- description to allow overwriting (there is no -y flag before the name).
	Spring.SendCommands("save " .. name .. " -y")
end
