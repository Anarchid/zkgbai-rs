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
		desc   = 'Comma-separated features to switch off: porc, reach, refuge, teleport, drops, ditch, reanalysis, intel, grid, nukes, missiles, scouts, radar, requisition, colonize, aaintel, precision, logistics, pause, clients, jumps, puppies, earlycons, raidmem, odreserve, qrf (home defense against raids), firefighters.',
		type   = 'string',
		def    = '',
		maxlen = 200,
	},
	{
		key    = 'enable',
		name   = 'Experimental features',
		desc   = 'Comma-separated features to switch on: mexporc (a turret by every mex and link, the old default: off since the home defense force stands in for it), pairs (Constables pair up on builds), currency (issue crews weigh danger as their expected loss and hold where it pays), permanence (the interceptor remembers unseen enemy groups for 45 s, dead reckoned toward our assets).',
		type   = 'string',
		def    = '',
		maxlen = 200,
	},
	{
		key    = 'greed',
		name   = 'Greed',
		desc   = 'The economy\'s appetite for risk, 0.25-4: the threat workers and jobs perceive is divided by it (far, contested metal spots get taken, workers flee later). Empty: 1 (or the ZKGBAI_GREED environment variable).',
		type   = 'string',
		def    = '',
		maxlen = 8,
	},
	{
		key    = 'qrf_defense',
		name   = 'Home defense budget',
		desc   = 'Raiders held back to keep raids off our economy (the quick-reaction force): auto (none until something is lost to a raid, then sized to the raids that cost us, at most qrf_cap of the army), off (forming squads moonlight, nothing held back), a metal value, or a share of the mobile army like 30%. Empty: auto.',
		type   = 'string',
		def    = '',
		maxlen = 12,
	},
	{
		key    = 'qrf_cap',
		name   = 'Home defense ceiling',
		desc   = 'On auto, the most of the mobile army the home defense holds back, 0-1. Empty: 0.3.',
		type   = 'string',
		def    = '',
		maxlen = 8,
	},
	{
		key    = 'qrf_size',
		name   = 'Home defense sizing',
		desc   = 'On auto, the raid a team is sized against: lossy (the raids that cost us, weighted by the metal lost; nothing until something was lost to a raid), typical (a typical recent raid), or largest (the largest recent raid-like group). Empty: lossy.',
		type   = 'string',
		def    = '',
		maxlen = 8,
	},
	{
		key    = 'qrf_production',
		name   = 'Home defense production',
		desc   = 'Factories build raiders for the home defense while it is short of its budget: on or off. Empty: off (the orders cost the factory its own picks).',
		type   = 'string',
		def    = '',
		maxlen = 4,
	},
	{
		key    = 'qrf_moonlight',
		name   = 'Moonlighting squads',
		desc   = 'Raider squads still forming answer raids at home while they fill: on or off. Empty: on.',
		type   = 'string',
		def    = '',
		maxlen = 4,
	},
	{
		key    = 'buildpower',
		name   = 'Build power',
		desc   = 'Mobile build power the factories keep, as a multiple of metal income, 0.5-3. Empty: 1 (the AI\'s own rule: constructors follow income one to one; a strong human runs 1.3-1.7 plus caretakers).',
		type   = 'string',
		def    = '',
		maxlen = 8,
	},
	{
		key    = 'line',
		name   = 'Army line',
		desc   = 'Where the army stands when nothing calls it, as a fraction of the way from our base to the enemy center (0.2 home, 0.5 the middle). Empty: the AI chooses (or ZKGBAI_LINE).',
		type   = 'string',
		def    = '',
		maxlen = 8,
	},
	{
		key    = 'profile',
		name   = 'Operator profile',
		desc   = 'What an operator may do: empty for everything; posture for no hands on units or construction (take, release, order, build and issue are not offered), production orders only from 2:00 and two deep, a factory requisition only past 30 income. Or ZKGBAI_PROFILE.',
		type   = 'string',
		def    = '',
		maxlen = 16,
	},
	{
		key    = 'mute',
		name   = 'Muted modules',
		desc   = 'Comma-separated modules whose orders are dropped (they keep thinking, the operator acts for them): eco, fac, war, raider, squad, retreat, scouts, drops, nukes, missiles, precision, bomber, misc, jaeger, jumps, puppies, teleport, water, or a handler like eco.unitIdle. Empty: none (or the ZKGBAI_MUTE environment variable).',
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
