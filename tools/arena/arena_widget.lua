-- Installed into each arena match's write-dir; reports the outcome and
-- periodic per-team stats to the infolog for tools/arena/match.sh.
function widget:GetInfo()
	return {
		name    = "Arena Reporter",
		desc    = "Logs game over and team stats for headless AI matches",
		layer   = 0,
		enabled = true,
	}
end

-- Substituted by match.sh; 0 = no limit.
local MAX_FRAMES = @MAX_FRAMES@
local done = false

local function report(tag)
	local parts = {}
	for _, teamID in ipairs(Spring.GetTeamList()) do
		local _, _, isDead, _, _, allyTeamID = Spring.GetTeamInfo(teamID, false)
		local metalIncome = select(4, Spring.GetTeamResources(teamID, "metal")) or 0
		local units = Spring.GetTeamUnitCount(teamID) or 0
		local stats = Spring.GetTeamStatsHistory(teamID, 0, 1)
		local hist = Spring.GetTeamStatsHistory(teamID)
		local last = hist and hist > 0 and Spring.GetTeamStatsHistory(teamID, hist - 1, hist)
		local produced = last and last[1] and last[1].metalProduced or 0
		local dealt = last and last[1] and last[1].damageDealt or 0
		parts[#parts + 1] = string.format("team%d(ally%d dead=%s units=%d minc=%.1f mprod=%.0f dmg=%.0f)",
			teamID, allyTeamID, tostring(isDead), units, metalIncome, produced, dealt)
	end
	Spring.Echo("ARENA_" .. tag .. " " .. table.concat(parts, " "))
end

function widget:GameFrame(n)
	if done then
		return
	end
	if n > 0 and n % 1800 == 0 then
		report("STATS f=" .. n)
	end
	if MAX_FRAMES > 0 and n >= MAX_FRAMES then
		done = true
		report("FINAL f=" .. n)
		Spring.Echo("ARENA_TIMEOUT f=" .. n)
		Spring.SendCommands("quitforce")
	end
end

function widget:GameOver(winningAllyTeams)
	if done then
		return
	end
	done = true
	report("FINAL f=" .. Spring.GetGameFrame())
	Spring.Echo("ARENA_GAMEOVER f=" .. Spring.GetGameFrame() .. " winners=" .. table.concat(winningAllyTeams or {}, ","))
	Spring.SendCommands("quitforce")
end
