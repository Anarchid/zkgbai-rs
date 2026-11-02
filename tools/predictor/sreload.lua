-- usage: lua5.1 sreload.lua <units dir> ; one TSV row per weapon with customParams.script_reload
-- (ZK scriptReload.lua: each of script_burst tubes reloads script_reload s after it fires; the engine
-- reloadtime is only the spacing between tubes)
local dir = arg[1]
local function lk(t) local o = {} for k, v in pairs(t) do o[type(k) == "string" and k:lower() or k] = v end return o end
Spring = setmetatable({}, {__index = function() return function() return nil end end})
VFS = setmetatable({}, {__index = function() return function() return nil end end})
include = function() return {} end
Shared = setmetatable({}, {__index = function() return 1 end})
local p = io.popen('ls "' .. dir .. '"/*.lua')
print("unit\tname\tweapon\treloadtime\tscript_reload\tscript_burst")
for f in p:lines() do
  local ok, r = pcall(dofile, f)
  if ok and type(r) == "table" then
    for uname, u in pairs(r) do
      if type(u) == "table" then
        u = lk(u)
        for wn, w in pairs(u.weapondefs and lk(u.weapondefs) or {}) do
          w = lk(w)
          local cp = w.customparams and lk(w.customparams) or {}
          if cp.script_reload then
            print(table.concat({uname, tostring(u.name), wn, tostring(w.reloadtime), tostring(cp.script_reload),
              tostring(cp.script_burst or "")}, "\t"))
          end
        end
      end
    end
  end
end
