--
--  Custom Options Definition Table format
--
--  See the engine's AI/Skirmish/*/data/AIOptions.lua for the format.
--
--  Development options, for A/B tests between builds or feature sets in the
--  same game (the environment variables ZKGBAI_DISABLE and
--  ZKGBAI_FACTORY_PICK do the same for every instance at once), and the
--  operator API and run journal (ZKGBAI_OPERATOR, ZKGBAI_JOURNAL).
--

local options = {
	{
		key    = 'disable',
		name   = 'Disabled features',
		desc   = 'Comma-separated features to switch off: porc, reach, refuge, teleport, drops, ditch, reanalysis, intel, grid, nukes, missiles, scouts, radar, requisition, colonize, aaintel, precision.',
		type   = 'string',
		def    = '',
		maxlen = 200,
	},
	{
		key    = 'operator',
		name   = 'Operator API',
		desc   = 'Serve the operator API (dashboard, MCP and JSON over HTTP): on (127.0.0.1:7460, or the next free port), a port, or host:port. Empty: off (or the ZKGBAI_OPERATOR environment variable).',
		type   = 'string',
		def    = '',
		maxlen = 64,
	},
	{
		key    = 'journal',
		name   = 'Run journal',
		desc   = 'Record the run as JSON lines: on (journal/ in the AI data dir) or a directory. Empty: off (or the ZKGBAI_JOURNAL environment variable).',
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
	{
		key    = 'factory',
		name   = 'Factories',
		desc   = 'The factories to build, first to last, comma-separated (e.g. factoryjump,factorycloak); empty to choose freely.',
		type   = 'string',
		def    = '',
		maxlen = 200,
	},
}

return options
