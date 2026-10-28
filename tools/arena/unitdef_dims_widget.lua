-- Dumps every unit def's engine-computed size and water fields to <write-dir>/unitdef_dims.tsv:
-- name, height, radius, waterline, min/maxWaterDepth, floatOnWater, and the model's midpoint height
-- when loaded. For the fight predictor's water rule (under water iff the water at a unit's position
-- is at least its height deep). Run once in a short headless match (match.sh -f 60).
function widget:GetInfo()
	return {
		name    = "UnitDef Dims",
		desc    = "Dumps UnitDefs height, radius and water fields as TSV",
		author  = "zkgbai",
		date    = "2026-10-28",
		license = "GNU GPL, v2 or later",
		layer   = 0,
		enabled = true,
	}
end

local function s(v)
	if v == nil then return "" end
	return tostring(v)
end

function widget:Initialize()
	local fh = io.open("unitdef_dims.tsv", "w")
	if not fh then
		Spring.Echo("UNITDEF_DIMS failed: cannot open the file")
		return
	end
	fh:write("name\theight\tradius\twaterline\tminwaterdepth\tmaxwaterdepth\tfloatonwater\tmidy\n")
	local n = 0
	for _, ud in pairs(UnitDefs) do
		local midy = ud.model and ud.model.midy
		fh:write(table.concat({ud.name, s(ud.height), s(ud.radius), s(ud.waterline), s(ud.minWaterDepth),
			s(ud.maxWaterDepth), s(ud.floatOnWater), s(midy)}, "\t") .. "\n")
		n = n + 1
	end
	fh:close()
	Spring.Echo("UNITDEF_DIMS rows=" .. n)
end
