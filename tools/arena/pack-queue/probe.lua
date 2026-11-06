function widget:GetInfo()
 return {name='Pack queue regression',desc='Probe synchronous short waypoint completion',layer=0,enabled=true}
end
local units={}
function widget:GameFrame(f)
 if f==5 then Spring.SendCommands('cheat','godmode','team 0') end
 if f==120 then
  Spring.SendCommands('give 1 tankassault 0 @2600,0,2400','give 1 vehassault 0 @2600,0,3200','give 1 factorycloak 1 @2850,0,2400','give 1 factorycloak 1 @2850,0,3200')
 end
 if f==180 or f==300 then
  for _,u in ipairs(Spring.GetTeamUnits(0)) do
   local d=UnitDefs[Spring.GetUnitDefID(u)]
   if d and (d.name=='tankassault' or d.name=='vehassault') then
    units[u]=true
    local x,y,z=Spring.GetUnitPosition(u)
    if f==180 then
     Spring.GiveOrderToUnit(u,31109,{x,y,z},0)
     Spring.GiveOrderToUnit(u,31109,{x,y,z+500},CMD.OPT_SHIFT)
    else
     Spring.GiveOrderToUnit(u,31109,{x,y,z+500},0)
     Spring.GiveOrderToUnit(u,CMD.INSERT,{0,31109,0,x,y,z},CMD.OPT_ALT)
    end
    Spring.Echo('QUEUEPROBE order '..f..' '..u..' '..d.name)
   end
  end
 end
 if f>=180 and f<=360 and f%5==0 then
  for u in pairs(units) do
   local x,y,z=Spring.GetUnitPosition(u)
   local cmds={}
   for _,c in ipairs(Spring.GetUnitCommands(u,8) or {}) do cmds[#cmds+1]=c.id..':'..table.concat(c.params,',') end
   Spring.Echo('QUEUEPROBE '..f..' '..u..' '..x..' '..z..' '..table.concat(cmds,';'))
  end
 end
end
