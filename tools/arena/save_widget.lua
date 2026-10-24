-- Installed into an arena match's write-dir by tools/arena/savematch.sh:
-- saves the game once at frame @SAVE_FRAME@ as Saves/@SAVE_NAME@.ssf
-- (engine creg save, the console "save <name> -y" command), for load tests with
-- tools/arena/loadmatch.sh.
function widget:GetInfo()
	return {
		name    = "Arena Saver",
		desc    = "Saves the game once at a fixed frame for headless load tests",
		layer   = 0,
		enabled = true,
	}
end

-- Substituted by savematch.sh; 0 = never.
local SAVE_FRAME = @SAVE_FRAME@
local SAVE_NAME = "@SAVE_NAME@"
local done = false

function widget:GameFrame(n)
	if done or SAVE_FRAME <= 0 or n ~= SAVE_FRAME then
		return
	end
	done = true
	Spring.Echo("ARENA_SAVE f=" .. n .. " name=" .. SAVE_NAME)
	-- Engine form: /save <name> [description]; ZK passes "-y" as the
	-- description to allow overwriting (there is no -y flag before the name).
	Spring.SendCommands("save " .. SAVE_NAME .. " -y")
end
