--
--  Info Definition Table format
--
--  These keywords must be lowercase for LuaParser to read them.
--
--  key:      user defined or one of the SKIRMISH_AI_PROPERTY_* defines in
--            SSkirmishAILibrary.h
--  value:    the value of the property
--  desc:     the description (could be used as a tooltip)
--

local infos = {
	{
		key    = 'shortName',
		value  = 'ZKGBAI', -- AI name - !This comment is used for parsing!
		desc   = 'machine conform name.',
	},
	{
		key    = 'version',
		value  = '@VERSION@', -- AI version - !This comment is used for parsing!
	},
	{
		key    = 'name',
		value  = 'Zero-K Graph Based AI',
		desc   = 'human readable name.',
	},
	{
		key    = 'description',
		value  = 'Plays Zero-K by reasoning over a graph of the map\'s metal spots.',
		desc   = 'tooltip.',
	},
	{
		key    = 'url',
		value  = 'https://github.com/Anarchid/zkgbai-rs',
		desc   = 'URL with more detailed info about the AI',
	},
	{
		key    = 'loadSupported',
		value  = 'no',
		desc   = 'whether this AI supports loading or not',
	},
	{
		key    = 'interfaceShortName',
		value  = 'C', -- AI Interface name - !This comment is used for parsing!
		desc   = 'the shortName of the AI interface this AI needs',
	},
	{
		key    = 'interfaceVersion',
		value  = '0.1', -- AI Interface version - !This comment is used for parsing!
		desc   = 'the minimum version of the AI interface this AI needs',
	},
}

return infos
