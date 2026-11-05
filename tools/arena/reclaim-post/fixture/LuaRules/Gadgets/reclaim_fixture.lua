function gadget:GetInfo()
 return {name='Reclaim post mechanics fixture',desc='Artificial scene; not a historical counterfactual',author='ZKGBAI',date='2026-11-05',license='GPL v2',layer=100000,enabled=true}
end
if not gadgetHandler:IsSyncedCode() then return end
local mode=VFS.Include('LuaRules/Configs/reclaim_fixture.lua')
local height=mode.height or 200
local ready=false
local units={}
local wrecks={}
local support={}
local function make(name,x,z,team)
 local u=Spring.CreateUnit(name,x,Spring.GetGroundHeight(x,z),z,0,team or 0)
 assert(u,'create '..name)
 Spring.GiveOrderToUnit(u,CMD.FIRE_STATE,{0},0)
 Spring.GiveOrderToUnit(u,CMD.MOVE_STATE,{0},0)
 units[#units+1]=u
 return u
end
function gadget:GameFrame(f)
 if f==90 then
  for _,u in ipairs(Spring.GetAllUnits()) do
   local d=UnitDefs[Spring.GetUnitDefID(u)]
   if d.name:find('dyn') then
    local team=Spring.GetUnitTeam(u)
    local x,z=team==0 and 512 or Game.mapSizeX-512,team==0 and 512 or Game.mapSizeZ-512
    Spring.MoveCtrl.Enable(u);Spring.MoveCtrl.SetPosition(u,x,height,z)
    Spring.GiveOrderToUnit(u,CMD.STOP,{},0)
   else Spring.DestroyUnit(u,false,true) end
  end
  for _,u in ipairs(Spring.GetAllFeatures()) do Spring.DestroyFeature(u) end
  Spring.LevelHeightMap(0,0,Game.mapSizeX,Game.mapSizeZ,height)
  make('energyfusion',512,768,0)
  make('energyfusion',512,1280,0)
  for i=1,8 do
   local u=make('staticmex',256+i*96,1024,0)
   Spring.SetUnitRulesParam(u,'mexIncome',5)
   Spring.SetUnitRulesParam(u,'current_metalIncome',5)
  end
  make('turretheavylaser',1792,1856,0)
  make('turretheavylaser',1792,2240,0)
  local enemy=make('turretheavylaser',2816,2048,1)
  Spring.SetUnitLosMask(enemy,0,{los=true,radar=true,prevLos=true,contRadar=true})
  Spring.SetUnitLosState(enemy,0,{los=true,radar=true,prevLos=true,contRadar=true})
  make('staticradar',1408,2048,0)
  for _,p in ipairs({{1920,2048},{2048,1920},{2048,2176}}) do make('spidercon',p[1],p[2],0) end
  for i=1,6 do
   local id=Spring.CreateFeature('tankassault_dead',2304+(i%3)*48,height,1952+math.floor(i/3)*64,0,Spring.GetGaiaTeamID())
   assert(id,'feature');wrecks[#wrecks+1]=id
  end
  if mode.threat then
   local artillery=make('tankarty',2500,2200,1)
   Spring.SetUnitLosMask(artillery,0,{los=true,radar=true,prevLos=true,contRadar=true})
   Spring.SetUnitLosState(artillery,0,{los=true,radar=true,prevLos=true,contRadar=true})
  end
  Spring.Echo('RECLAIM_FIXTURE setup height='..height)
  ready=true
 end
 if ready then
  if f==6000 then
   for _,id in ipairs(wrecks) do Spring.DestroyFeature(id) end
   for _,u in ipairs(Spring.GetTeamUnits(0)) do
    if UnitDefs[Spring.GetUnitDefID(u)].name=='staticcon' then
     local x,y,z=Spring.GetUnitPosition(u)
     local damaged=make('staticradar',x+128,z,0)
     Spring.SetUnitHealth(damaged,20)
     local assist=Spring.CreateUnit('energysolar',x,Spring.GetGroundHeight(x,z+128),z+128,0,0,true)
     Spring.SetUnitHealth(assist,{health=50,build=0.1})
     support={damaged,assist}
     Spring.Echo('RECLAIM_FIXTURE support damaged='..damaged..' assist='..assist)
    end
   end
  end
  Spring.AddTeamResource(0,'metal',10)
  Spring.AddTeamResource(0,'energy',10)
  if f%30==0 then
   for _,u in ipairs(units) do
    if Spring.ValidUnitID(u) and UnitDefs[Spring.GetUnitDefID(u)].name=='staticmex' then
     Spring.SetUnitRulesParam(u,'current_metalIncome',5)
    end
   end
  end
  if f%300==0 then
   local stock=0
   for _,id in ipairs(wrecks) do local m=Spring.GetFeatureResources(id);stock=stock+(m or 0) end
   local nanos={}
   for _,u in ipairs(Spring.GetTeamUnits(0)) do
    if UnitDefs[Spring.GetUnitDefID(u)].name=='staticcon' then
     local x,y,z=Spring.GetUnitPosition(u);local hp,_,_,_,bp=Spring.GetUnitHealth(u)
     nanos[#nanos+1]=string.format('%d:%.0f,%.0f,%.0f:%.3f:%.0f',u,x,y,z,bp,hp)
    end
   end
   local repair={}
   for _,u in ipairs(support) do
    local hp,mhp,_,_,bp=Spring.GetUnitHealth(u)
    repair[#repair+1]=string.format('%d:%.0f/%.0f:%.3f',u,hp or 0,mhp or 0,bp or 0)
   end
   Spring.Echo('RECLAIM_FIXTURE f='..f..' stock='..stock..' caretakers='..table.concat(nanos,';')..' support='..table.concat(repair,';'))
  end
 end
end
