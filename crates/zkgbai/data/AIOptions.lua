--
--  Custom Options Definition Table format
--
--  See the engine's AI/Skirmish/*/data/AIOptions.lua for the format.
--
--  Development options, for A/B tests between builds or feature sets in the
--  same game (the environment variables ZKGBAI_DISABLE and
--  ZKGBAI_FACTORY_PICK do the same for every instance at once).
--

local options = {
	{
		key    = 'disable',
		name   = 'Disabled features',
		desc   = 'Comma-separated features to switch off: porc, reach, refuge, teleport, drops, ditch, reanalysis, intel, grid, nukes.',
		type   = 'string',
		def    = '',
		maxlen = 200,
	},
	{
		key    = 'factory_pick',
		name   = 'Opening factory pick',
		desc   = 'Open with the n-th (modulo) of the factories viable on the map; -1 to choose freely.',
		type   = 'number',
		def    = -1,
		min    = -1,
		max    = 1000,
		step   = 1,
	},
}

return options
