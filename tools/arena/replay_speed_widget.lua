-- Installed by replay-harvest.sh: plays a demo at the given speed and quits
-- when it ends (a headless engine otherwise idles at the end of a replay).
function widget:GetInfo()
	return { name = "Replay Speed", desc = "sets demo playback speed, quits at the end", layer = 0, enabled = true }
end
local SPEED = @SPEED@
local lastFrame, idle, started = -1, 0, false
function widget:GameStart()
	started = true
	Spring.Echo("REPLAY_START f=" .. Spring.GetGameFrame())
end
function widget:Initialize()
	if SPEED > 0 then Spring.SendCommands("setminspeed " .. SPEED, "setmaxspeed " .. SPEED) end
	Spring.Echo("REPLAY_INIT speed=" .. SPEED .. " replay=" .. tostring(Spring.IsReplay()))
end
function widget:GameOver()
	Spring.Echo("REPLAY_GAMEOVER f=" .. Spring.GetGameFrame())
end
function widget:Update()
	local f = Spring.GetGameFrame()
	if not started and f > 0 then started = true end
	if not started then return end
	if f == lastFrame then
		idle = idle + 1
		if idle > 1800 then  -- ~30 s of no sim progress: the demo has ended
			Spring.Echo("REPLAY_END f=" .. f)
			Spring.SendCommands("quitforce")
		end
	else
		lastFrame, idle = f, 0
	end
end
