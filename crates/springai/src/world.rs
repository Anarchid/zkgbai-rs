use springai_sys as sys;
use sys::CommandTopic;

use crate::callback::{c_string, ffi, owned_str, read_floats, read_ids};
use crate::{Callback, Unit, UnitDef, Vec3};

/// Engine limit for Lua call-in replies (`aidefines.h`).
const MAX_RESPONSE_SIZE: usize = 10240;

/// A resource type (Zero-K has `"Metal"` and `"Energy"`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Resource {
    id: i32,
}

impl Resource {
    pub fn id(&self) -> i32 {
        self.id
    }
}

/// Global game state queries and messages.
#[derive(Clone, Copy, Debug)]
pub struct Game {
    cb: Callback,
}

/// The map: dimensions, terrain, sensor grids and build-site helpers.
#[derive(Clone, Copy, Debug)]
pub struct Map {
    cb: Callback,
}

/// The game ("mod") archive's global settings.
#[derive(Clone, Copy, Debug)]
pub struct Mod {
    cb: Callback,
}

/// This team's resource economy.
#[derive(Clone, Copy, Debug)]
pub struct Economy {
    cb: Callback,
}

impl Callback {
    pub fn game(&self) -> Game {
        Game { cb: *self }
    }

    pub fn map(&self) -> Map {
        Map { cb: *self }
    }

    pub fn mod_info(&self) -> Mod {
        Mod { cb: *self }
    }

    pub fn economy(&self) -> Economy {
        Economy { cb: *self }
    }

    pub fn resource_by_name(&self, name: &str) -> Option<Resource> {
        let n = c_string(name);
        let id = ffi!(self, getResourceByName, n.as_ptr());
        (id >= 0).then_some(Resource { id })
    }

    /// This AI's start-script option `key` (its `[AIn] { [Options] }`
    /// block), if set.
    pub fn option(&self, key: &str) -> Option<String> {
        let k = c_string(key);
        let v = ffi!(self, SkirmishAI_OptionValues_getValueByKey, k.as_ptr());
        (!v.is_null()).then(|| owned_str(v))
    }

    /// Write a line to the engine's infolog.
    pub fn log(&self, msg: &str) {
        let m = c_string(msg);
        ffi!(self, Log_log, m.as_ptr());
    }

    /// Call the game's LuaRules `AICallIn`; returns its reply, if any.
    pub fn call_lua_rules(&self, data: &str) -> Option<String> {
        let d = c_string(data);
        let mut out = vec![0 as std::ffi::c_char; MAX_RESPONSE_SIZE];
        let mut cmd = sys::SCallLuaRulesCommand { inData: d.as_ptr(), inSize: -1, ret_outData: out.as_mut_ptr() };
        if !self
            .handle_command(CommandTopic::COMMAND_CALL_LUA_RULES, (&mut cmd as *mut sys::SCallLuaRulesCommand).cast())
        {
            return None;
        }
        let s = owned_str(out.as_ptr());
        (!s.is_empty()).then_some(s)
    }

    // ── unit queries ────────────────────────────────────────────────────

    fn units_from(&self, ids: Vec<i32>) -> Vec<Unit> {
        ids.into_iter().filter_map(|u| Unit::from_id(*self, u)).collect()
    }

    /// All units owned by this AI's team.
    pub fn team_units(&self) -> Vec<Unit> {
        let cb = *self;
        self.units_from(read_ids(|buf, max| ffi!(cb, getTeamUnits, buf, max)))
    }

    /// All units of allied teams, including our own.
    pub fn friendly_units(&self) -> Vec<Unit> {
        let cb = *self;
        self.units_from(read_ids(|buf, max| ffi!(cb, getFriendlyUnits, buf, max)))
    }

    /// Friendly units within a sphere of `radius` around `pos`.
    pub fn friendly_units_in(&self, pos: Vec3, radius: f32) -> Vec<Unit> {
        self.friendly_units_in_shape(pos, radius, true)
    }

    /// Friendly units within an infinite vertical cylinder around `pos`.
    pub fn friendly_units_in_cylinder(&self, pos: Vec3, radius: f32) -> Vec<Unit> {
        self.friendly_units_in_shape(pos, radius, false)
    }

    fn friendly_units_in_shape(&self, pos: Vec3, radius: f32, spherical: bool) -> Vec<Unit> {
        let cb = *self;
        let mut p = pos.to_array();
        self.units_from(read_ids(|buf, max| ffi!(cb, getFriendlyUnitsIn, p.as_mut_ptr(), radius, spherical, buf, max)))
    }

    /// Enemy units currently in LOS.
    pub fn enemy_units(&self) -> Vec<Unit> {
        let cb = *self;
        self.units_from(read_ids(|buf, max| ffi!(cb, getEnemyUnits, buf, max)))
    }

    /// Enemy units in LOS within a sphere of `radius` around `pos`.
    pub fn enemy_units_in(&self, pos: Vec3, radius: f32) -> Vec<Unit> {
        self.enemy_units_in_shape(pos, radius, true)
    }

    /// Enemy units in LOS within an infinite vertical cylinder around `pos`.
    pub fn enemy_units_in_cylinder(&self, pos: Vec3, radius: f32) -> Vec<Unit> {
        self.enemy_units_in_shape(pos, radius, false)
    }

    fn enemy_units_in_shape(&self, pos: Vec3, radius: f32, spherical: bool) -> Vec<Unit> {
        let cb = *self;
        let mut p = pos.to_array();
        self.units_from(read_ids(|buf, max| ffi!(cb, getEnemyUnitsIn, p.as_mut_ptr(), radius, spherical, buf, max)))
    }

    /// Enemy units in LOS or on radar.
    pub fn enemy_units_in_radar_and_los(&self) -> Vec<Unit> {
        let cb = *self;
        self.units_from(read_ids(|buf, max| ffi!(cb, getEnemyUnitsInRadarAndLos, buf, max)))
    }

    /// Ids of all teams that are not allied with us.
    pub fn enemy_teams(&self) -> Vec<i32> {
        let cb = *self;
        read_ids(|buf, max| ffi!(cb, getEnemyTeams, buf, max))
    }

    /// Give units to another team; returns how many were transferred.
    pub fn send_units(&self, units: &[Unit], to_team: i32) -> i32 {
        let mut ids: Vec<i32> = units.iter().map(Unit::id).collect();
        let mut cmd = sys::SSendUnitsCommand {
            unitIds: ids.as_mut_ptr(),
            unitIds_size: ids.len() as i32,
            receivingTeamId: to_team,
            ret_sentUnits: 0,
        };
        if self.handle_command(CommandTopic::COMMAND_SEND_UNITS, (&mut cmd as *mut sys::SSendUnitsCommand).cast()) {
            cmd.ret_sentUnits
        } else {
            0
        }
    }
}

impl Game {
    pub fn current_frame(&self) -> i32 {
        ffi!(self.cb, Game_getCurrentFrame)
    }

    pub fn my_team(&self) -> i32 {
        ffi!(self.cb, Game_getMyTeam)
    }

    pub fn my_ally_team(&self) -> i32 {
        ffi!(self.cb, Game_getMyAllyTeam)
    }

    pub fn player_team(&self, player: i32) -> i32 {
        ffi!(self.cb, Game_getPlayerTeam, player)
    }

    /// Number of teams in the game (including Gaia).
    pub fn teams(&self) -> i32 {
        ffi!(self.cb, Game_getTeams)
    }

    pub fn team_ally_team(&self, team: i32) -> i32 {
        ffi!(self.cb, Game_getTeamAllyTeam, team)
    }

    pub fn team_side(&self, team: i32) -> String {
        owned_str(ffi!(self.cb, Game_getTeamSide, team))
    }

    pub fn is_allied(&self, ally_team_a: i32, ally_team_b: i32) -> bool {
        ffi!(self.cb, Game_isAllied, ally_team_a, ally_team_b)
    }

    pub fn team_has_ai_controller(&self, team: i32) -> bool {
        ffi!(self.cb, Team_hasAIController, team)
    }

    pub fn team_resource_current(&self, team: i32, res: Resource) -> f32 {
        ffi!(self.cb, Game_getTeamResourceCurrent, team, res.id())
    }

    pub fn team_resource_income(&self, team: i32, res: Resource) -> f32 {
        ffi!(self.cb, Game_getTeamResourceIncome, team, res.id())
    }

    pub fn team_rules_param_float(&self, team: i32, name: &str, default: f32) -> f32 {
        let n = c_string(name);
        ffi!(self.cb, Team_getRulesParamFloat, team, n.as_ptr(), default)
    }

    pub fn team_rules_param_string(&self, team: i32, name: &str, default: &str) -> String {
        let (n, d) = (c_string(name), c_string(default));
        owned_str(ffi!(self.cb, Team_getRulesParamString, team, n.as_ptr(), d.as_ptr()))
    }

    /// The full start script (`script.txt`) of the running game.
    pub fn setup_script(&self) -> String {
        owned_str(ffi!(self.cb, Game_getSetupScript))
    }

    pub fn rules_param_float(&self, name: &str, default: f32) -> f32 {
        let n = c_string(name);
        ffi!(self.cb, Game_getRulesParamFloat, n.as_ptr(), default)
    }

    pub fn rules_param_string(&self, name: &str, default: &str) -> String {
        let (n, d) = (c_string(name), c_string(default));
        owned_str(ffi!(self.cb, Game_getRulesParamString, n.as_ptr(), d.as_ptr()))
    }

    /// Send a chat message or `/command`. `zone` 0 = all players.
    pub fn send_text_message(&self, text: &str, zone: i32) -> bool {
        let t = c_string(text);
        let mut cmd = sys::SSendTextMessageCommand { text: t.as_ptr(), zone };
        self.cb.handle_command(
            CommandTopic::COMMAND_SEND_TEXT_MESSAGE,
            (&mut cmd as *mut sys::SSendTextMessageCommand).cast(),
        )
    }

    /// Choose a start position during the pre-game placement phase.
    pub fn send_start_position(&self, ready: bool, pos: Vec3) -> bool {
        let mut p = pos.to_array();
        let mut cmd = sys::SSendStartPosCommand { ready, pos_posF3: p.as_mut_ptr() };
        self.cb
            .handle_command(CommandTopic::COMMAND_SEND_START_POS, (&mut cmd as *mut sys::SSendStartPosCommand).cast())
    }

    pub fn set_pause(&self, enable: bool, reason: &str) -> bool {
        let r = c_string(reason);
        let mut cmd = sys::SPauseCommand { enable, reason: r.as_ptr() };
        self.cb.handle_command(CommandTopic::COMMAND_PAUSE, (&mut cmd as *mut sys::SPauseCommand).cast())
    }
}

impl Map {
    /// Width in heightmap squares (8 elmos each).
    pub fn width(&self) -> i32 {
        ffi!(self.cb, Map_getWidth)
    }

    /// Height (z extent) in heightmap squares (8 elmos each).
    pub fn height(&self) -> i32 {
        ffi!(self.cb, Map_getHeight)
    }

    pub fn name(&self) -> String {
        owned_str(ffi!(self.cb, Map_getName))
    }

    pub fn human_name(&self) -> String {
        owned_str(ffi!(self.cb, Map_getHumanName))
    }

    pub fn elevation_at(&self, x: f32, z: f32) -> f32 {
        ffi!(self.cb, Map_getElevationAt, x, z)
    }

    pub fn min_height(&self) -> f32 {
        ffi!(self.cb, Map_getMinHeight)
    }

    pub fn max_height(&self) -> f32 {
        ffi!(self.cb, Map_getMaxHeight)
    }

    pub fn water_damage(&self) -> f32 {
        ffi!(self.cb, Map_getWaterDamage)
    }

    /// Corner heights, `(width+1) * (height+1)` values, row-major.
    pub fn corner_height_map(&self) -> Vec<f32> {
        let cb = self.cb;
        read_floats(|buf, max| ffi!(cb, Map_getCornersHeightMap, buf, max))
    }

    /// Centre heights, `width * height` values, row-major.
    pub fn height_map(&self) -> Vec<f32> {
        let cb = self.cb;
        read_floats(|buf, max| ffi!(cb, Map_getHeightMap, buf, max))
    }

    /// Slopes at half resolution, `(width/2) * (height/2)` values.
    pub fn slope_map(&self) -> Vec<f32> {
        let cb = self.cb;
        read_floats(|buf, max| ffi!(cb, Map_getSlopeMap, buf, max))
    }

    /// LOS coverage at `Mod::los_mip_level` resolution; non-zero = visible.
    pub fn los_map(&self) -> Vec<i32> {
        let cb = self.cb;
        read_ids(|buf, max| ffi!(cb, Map_getLosMap, buf, max))
    }

    /// Like [`los_map`](Self::los_map), but fills `buf`, reusing its
    /// allocation (the LOS map is typically re-read every frame). `buf` is
    /// left empty if the engine reports no data.
    pub fn los_map_into(&self, buf: &mut Vec<i32>) {
        let n = ffi!(self.cb, Map_getLosMap, std::ptr::null_mut(), i32::MAX);
        if n <= 0 {
            buf.clear();
            return;
        }
        buf.resize(n as usize, 0);
        let got = ffi!(self.cb, Map_getLosMap, buf.as_mut_ptr(), n).clamp(0, n);
        buf.truncate(got as usize);
    }

    /// Radar coverage at `Mod::radar_mip_level` resolution.
    pub fn radar_map(&self) -> Vec<i32> {
        let cb = self.cb;
        read_ids(|buf, max| ffi!(cb, Map_getRadarMap, buf, max))
    }

    pub fn is_possible_to_build_at(&self, def: UnitDef, pos: Vec3, facing: i32) -> bool {
        let mut p = pos.to_array();
        ffi!(self.cb, Map_isPossibleToBuildAt, def.id(), p.as_mut_ptr(), facing)
    }

    /// The closest valid build position, or `(-1, 0, 0)` if none was found.
    pub fn find_closest_build_site(
        &self,
        def: UnitDef,
        pos: Vec3,
        search_radius: f32,
        min_dist: i32,
        facing: i32,
    ) -> Vec3 {
        let mut p = pos.to_array();
        let mut out = [0f32; 3];
        ffi!(
            self.cb,
            Map_findClosestBuildSite,
            def.id(),
            p.as_mut_ptr(),
            search_radius,
            min_dist,
            facing,
            out.as_mut_ptr()
        );
        Vec3::from_array(out)
    }

    /// Draw a labelled marker point (visible to allies).
    pub fn add_point(&self, pos: Vec3, label: &str) -> bool {
        let mut p = pos.to_array();
        let l = c_string(label);
        let mut cmd = sys::SAddPointDrawCommand { pos_posF3: p.as_mut_ptr(), label: l.as_ptr() };
        self.cb
            .handle_command(CommandTopic::COMMAND_DRAWER_POINT_ADD, (&mut cmd as *mut sys::SAddPointDrawCommand).cast())
    }

    pub fn remove_point(&self, pos: Vec3) -> bool {
        let mut p = pos.to_array();
        let mut cmd = sys::SRemovePointDrawCommand { pos_posF3: p.as_mut_ptr() };
        self.cb.handle_command(
            CommandTopic::COMMAND_DRAWER_POINT_REMOVE,
            (&mut cmd as *mut sys::SRemovePointDrawCommand).cast(),
        )
    }

    pub fn add_line(&self, from: Vec3, to: Vec3) -> bool {
        let (mut a, mut b) = (from.to_array(), to.to_array());
        let mut cmd = sys::SAddLineDrawCommand { posFrom_posF3: a.as_mut_ptr(), posTo_posF3: b.as_mut_ptr() };
        self.cb
            .handle_command(CommandTopic::COMMAND_DRAWER_LINE_ADD, (&mut cmd as *mut sys::SAddLineDrawCommand).cast())
    }
}

impl Mod {
    /// Mip level of the LOS map relative to the heightmap (each step halves).
    pub fn los_mip_level(&self) -> i32 {
        ffi!(self.cb, Mod_getLosMipLevel)
    }

    pub fn air_mip_level(&self) -> i32 {
        ffi!(self.cb, Mod_getAirMipLevel)
    }

    pub fn radar_mip_level(&self) -> i32 {
        ffi!(self.cb, Mod_getRadarMipLevel)
    }

    pub fn human_name(&self) -> String {
        owned_str(ffi!(self.cb, Mod_getHumanName))
    }

    pub fn short_name(&self) -> String {
        owned_str(ffi!(self.cb, Mod_getShortName))
    }

    pub fn version(&self) -> String {
        owned_str(ffi!(self.cb, Mod_getVersion))
    }
}

impl Economy {
    pub fn current(&self, res: Resource) -> f32 {
        ffi!(self.cb, Economy_getCurrent, res.id())
    }

    pub fn income(&self, res: Resource) -> f32 {
        ffi!(self.cb, Economy_getIncome, res.id())
    }

    pub fn usage(&self, res: Resource) -> f32 {
        ffi!(self.cb, Economy_getUsage, res.id())
    }

    pub fn storage(&self, res: Resource) -> f32 {
        ffi!(self.cb, Economy_getStorage, res.id())
    }

    pub fn pull(&self, res: Resource) -> f32 {
        ffi!(self.cb, Economy_getPull, res.id())
    }

    pub fn excess(&self, res: Resource) -> f32 {
        ffi!(self.cb, Economy_getExcess, res.id())
    }
}

/// Engine version string, e.g. `"2025.06.19"`.
impl Callback {
    pub fn engine_version(&self) -> String {
        let major = owned_str(ffi!(self, Engine_Version_getMajor));
        let minor = owned_str(ffi!(self, Engine_Version_getMinor));
        let patch = owned_str(ffi!(self, Engine_Version_getPatchset));
        [major, minor, patch].into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join(".")
    }
}
