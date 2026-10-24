-- Installed into an arena match's write-dir by tools/arena/reloadmatch.sh:
-- at frame @RELOAD_FRAME@ reloads the Skirmish AIs of teams @RELOAD_TEAMS@
-- (console "aireload <team>": the engine kills the AI instance and creates
-- a fresh one with the same key and options, sends it Init and then
-- UnitCreated/UnitFinished for every standing unit, the same PostLoad path
-- a loaded save game uses, minus Init.savedGame=true). A save-free proxy
-- for "does the AI survive being loaded mid-game", because the engine's
-- /save segfaults on the current Zero-K Lua state.
function widget:GetInfo()
	return {
		name    = "Arena AI Reloader",
		desc    = "Reloads skirmish AIs at a fixed frame",
		layer   = 0,
		enabled = true,
	}
end

local RELOAD_FRAME = @RELOAD_FRAME@
local RELOAD_TEAMS = { @RELOAD_TEAMS@ }
local done = false

function widget:GameFrame(n)
	if done or RELOAD_FRAME <= 0 or n ~= RELOAD_FRAME then
		return
	end
	done = true
	for _, t in ipairs(RELOAD_TEAMS) do
		local units = Spring.GetTeamUnitCount(t) or 0
		Spring.Echo("ARENA_RELOAD f=" .. n .. " team=" .. t .. " units=" .. units)
		Spring.SendCommands("aireload " .. t)
	end
end
