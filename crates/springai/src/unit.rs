use std::ffi::c_void;

use springai_sys as sys;
use sys::CommandTopic;

use crate::callback::{c_string, ffi, owned_str, read_floats};
use crate::macros::{getters, handle};
use crate::{Feature, UnitDef, Vec3, WeaponDef};

/// Command option flags (`UnitCommandOptions`), combinable with `|`.
pub mod opt {
    pub const NONE: i16 = 0;
    pub const INTERNAL_ORDER: i16 = 1 << 3;
    pub const RIGHT_MOUSE_KEY: i16 = 1 << 4;
    pub const SHIFT_KEY: i16 = 1 << 5;
    pub const CONTROL_KEY: i16 = 1 << 6;
    pub const ALT_KEY: i16 = 1 << 7;
}

/// Command timeout meaning "never time out" (the engine default).
pub const NO_TIMEOUT: i32 = i32::MAX;

handle!(
    /// A unit, friendly or enemy. Queries on units the AI cannot see return
    /// engine defaults (zero positions, `None` defs).
    Unit, unit_id
);

getters!(Unit.unit_id {
    fn team() -> i32 = Unit_getTeam;
    fn ally_team() -> i32 = Unit_getAllyTeam;
    fn stockpile() -> i32 = Unit_getStockpile;
    fn stockpile_queued() -> i32 = Unit_getStockpileQueued;
    fn max_speed() -> f32 = Unit_getMaxSpeed;
    fn max_range() -> f32 = Unit_getMaxRange;
    fn max_health() -> f32 = Unit_getMaxHealth;
    fn experience() -> f32 = Unit_getExperience;
    fn health() -> f32 = Unit_getHealth;
    fn paralyze_damage() -> f32 = Unit_getParalyzeDamage;
    fn capture_progress() -> f32 = Unit_getCaptureProgress;
    fn build_progress() -> f32 = Unit_getBuildProgress;
    fn speed() -> f32 = Unit_getSpeed;
    fn power() -> f32 = Unit_getPower;
    fn is_activated() -> bool = Unit_isActivated;
    fn is_being_built() -> bool = Unit_isBeingBuilt;
    fn is_cloaked() -> bool = Unit_isCloaked;
    fn is_paralyzed() -> bool = Unit_isParalyzed;
    fn is_neutral() -> bool = Unit_isNeutral;
    fn building_facing() -> i32 = Unit_getBuildingFacing;
    fn last_user_order_frame() -> i32 = Unit_getLastUserOrderFrame;
    fn num_current_commands() -> i32 = Unit_getCurrentCommands;
    fn num_weapons() -> i32 = Unit_getWeapons;
    res fn resource_use() = Unit_getResourceUse;
    res fn resource_make() = Unit_getResourceMake;
});

impl Unit {
    /// The unit's definition, or `None` if the engine does not reveal it
    /// (e.g. radar-only contacts).
    pub fn def(&self) -> Option<UnitDef> {
        UnitDef::from_id(self.cb, ffi!(self.cb, Unit_getDef, self.unit_id))
    }

    pub fn pos(&self) -> Vec3 {
        let mut out = [0f32; 3];
        ffi!(self.cb, Unit_getPos, self.unit_id, out.as_mut_ptr());
        Vec3::from_array(out)
    }

    /// Velocity in elmos per frame.
    pub fn vel(&self) -> Vec3 {
        let mut out = [0f32; 3];
        ffi!(self.cb, Unit_getVel, self.unit_id, out.as_mut_ptr());
        Vec3::from_array(out)
    }

    pub fn rules_param_float(&self, name: &str, default: f32) -> f32 {
        let n = c_string(name);
        ffi!(self.cb, Unit_getRulesParamFloat, self.unit_id, n.as_ptr(), default)
    }

    pub fn rules_param_string(&self, name: &str, default: &str) -> String {
        let (n, d) = (c_string(name), c_string(default));
        owned_str(ffi!(self.cb, Unit_getRulesParamString, self.unit_id, n.as_ptr(), d.as_ptr()))
    }

    pub fn current_commands(&self) -> Vec<CurrentCommand> {
        (0..self.num_current_commands().max(0)).map(|index| CurrentCommand { unit: *self, index }).collect()
    }

    pub fn weapons(&self) -> Vec<Weapon> {
        (0..self.num_weapons().max(0)).map(|index| Weapon { unit: *self, index }).collect()
    }

    /// The live weapon instance for the given weapon mount index, if any.
    pub fn weapon(&self, mount: i32) -> Option<Weapon> {
        let index = ffi!(self.cb, Unit_getWeapon, self.unit_id, mount);
        (index >= 0).then_some(Weapon { unit: *self, index })
    }
}

/// An order currently in a unit's command queue.
#[derive(Clone, Copy, Debug)]
pub struct CurrentCommand {
    unit: Unit,
    index: i32,
}

impl CurrentCommand {
    /// The engine/game command id (e.g. `CMD_MOVE`, or a gadget's custom id).
    pub fn id(&self) -> i32 {
        ffi!(self.unit.cb, Unit_CurrentCommand_getId, self.unit.unit_id, self.index)
    }

    pub fn options(&self) -> i16 {
        ffi!(self.unit.cb, Unit_CurrentCommand_getOptions, self.unit.unit_id, self.index)
    }

    pub fn tag(&self) -> i32 {
        ffi!(self.unit.cb, Unit_CurrentCommand_getTag, self.unit.unit_id, self.index)
    }

    pub fn timeout(&self) -> i32 {
        ffi!(self.unit.cb, Unit_CurrentCommand_getTimeOut, self.unit.unit_id, self.index)
    }

    pub fn params(&self) -> Vec<f32> {
        let (cb, u, i) = (self.unit.cb, self.unit.unit_id, self.index);
        read_floats(|buf, max| ffi!(cb, Unit_CurrentCommand_getParams, u, i, buf, max))
    }
}

/// A weapon instance mounted on a live unit.
#[derive(Clone, Copy, Debug)]
pub struct Weapon {
    unit: Unit,
    index: i32,
}

impl Weapon {
    pub fn unit(&self) -> Unit {
        self.unit
    }

    pub fn def(&self) -> Option<WeaponDef> {
        WeaponDef::from_id(self.unit.cb, ffi!(self.unit.cb, Unit_Weapon_getDef, self.unit.unit_id, self.index))
    }

    /// Frame at which the weapon will next be ready to fire.
    pub fn reload_frame(&self) -> i32 {
        ffi!(self.unit.cb, Unit_Weapon_getReloadFrame, self.unit.unit_id, self.index)
    }

    pub fn reload_time(&self) -> i32 {
        ffi!(self.unit.cb, Unit_Weapon_getReloadTime, self.unit.unit_id, self.index)
    }

    pub fn range(&self) -> f32 {
        ffi!(self.unit.cb, Unit_Weapon_getRange, self.unit.unit_id, self.index)
    }

    pub fn is_shield_enabled(&self) -> bool {
        ffi!(self.unit.cb, Unit_Weapon_isShieldEnabled, self.unit.unit_id, self.index)
    }

    pub fn shield_power(&self) -> f32 {
        ffi!(self.unit.cb, Unit_Weapon_getShieldPower, self.unit.unit_id, self.index)
    }
}

/// Issue a unit command whose struct starts with the standard
/// `unitId, groupId, options, timeOut` header.
macro_rules! unit_cmd {
    ($self:ident, $topic:ident, $S:ident, $opts:expr, $timeout:expr $(, $f:ident : $v:expr)* $(,)?) => {{
        let mut data = sys::$S {
            unitId: $self.unit_id,
            groupId: -1,
            options: $opts,
            timeOut: $timeout,
            $($f: $v,)*
        };
        $self.cb.handle_command(CommandTopic::$topic, (&mut data as *mut sys::$S).cast())
    }};
}

/// Unit orders. Every order returns `true` if the engine accepted it; orders
/// use the default timeout (never) unless the name says otherwise.
impl Unit {
    pub fn build(&self, def: UnitDef, pos: Vec3, facing: i32, opts: i16) -> bool {
        let mut p = pos.to_array();
        unit_cmd!(self, COMMAND_UNIT_BUILD, SBuildUnitCommand, opts, NO_TIMEOUT,
            toBuildUnitDefId: def.id(), buildPos_posF3: p.as_mut_ptr(), facing: facing)
    }

    pub fn stop(&self, opts: i16) -> bool {
        unit_cmd!(self, COMMAND_UNIT_STOP, SStopUnitCommand, opts, NO_TIMEOUT)
    }

    pub fn wait(&self, opts: i16) -> bool {
        unit_cmd!(self, COMMAND_UNIT_WAIT, SWaitUnitCommand, opts, NO_TIMEOUT)
    }

    pub fn move_to(&self, pos: Vec3, opts: i16) -> bool {
        self.move_to_timeout(pos, opts, NO_TIMEOUT)
    }

    /// Move with an absolute expiry frame for the order.
    pub fn move_to_timeout(&self, pos: Vec3, opts: i16, timeout: i32) -> bool {
        let mut p = pos.to_array();
        unit_cmd!(self, COMMAND_UNIT_MOVE, SMoveUnitCommand, opts, timeout, toPos_posF3: p.as_mut_ptr())
    }

    pub fn patrol_to(&self, pos: Vec3, opts: i16) -> bool {
        let mut p = pos.to_array();
        unit_cmd!(self, COMMAND_UNIT_PATROL, SPatrolUnitCommand, opts, NO_TIMEOUT, toPos_posF3: p.as_mut_ptr())
    }

    pub fn fight(&self, pos: Vec3, opts: i16) -> bool {
        let mut p = pos.to_array();
        unit_cmd!(self, COMMAND_UNIT_FIGHT, SFightUnitCommand, opts, NO_TIMEOUT, toPos_posF3: p.as_mut_ptr())
    }

    pub fn attack(&self, target: Unit, opts: i16) -> bool {
        self.attack_timeout(target, opts, NO_TIMEOUT)
    }

    /// Attack with an absolute expiry frame for the order.
    pub fn attack_timeout(&self, target: Unit, opts: i16, timeout: i32) -> bool {
        unit_cmd!(self, COMMAND_UNIT_ATTACK, SAttackUnitCommand, opts, timeout, toAttackUnitId: target.id())
    }

    pub fn attack_area(&self, pos: Vec3, radius: f32, opts: i16) -> bool {
        let mut p = pos.to_array();
        unit_cmd!(self, COMMAND_UNIT_ATTACK_AREA, SAttackAreaUnitCommand, opts, NO_TIMEOUT,
            toAttackPos_posF3: p.as_mut_ptr(), radius: radius)
    }

    pub fn guard(&self, target: Unit, opts: i16) -> bool {
        unit_cmd!(self, COMMAND_UNIT_GUARD, SGuardUnitCommand, opts, NO_TIMEOUT, toGuardUnitId: target.id())
    }

    pub fn repair(&self, target: Unit, opts: i16) -> bool {
        unit_cmd!(self, COMMAND_UNIT_REPAIR, SRepairUnitCommand, opts, NO_TIMEOUT, toRepairUnitId: target.id())
    }

    pub fn reclaim_unit(&self, target: Unit, opts: i16) -> bool {
        unit_cmd!(self, COMMAND_UNIT_RECLAIM_UNIT, SReclaimUnitUnitCommand, opts, NO_TIMEOUT,
            toReclaimUnitId: target.id())
    }

    pub fn reclaim_feature(&self, target: Feature, opts: i16) -> bool {
        unit_cmd!(self, COMMAND_UNIT_RECLAIM_FEATURE, SReclaimFeatureUnitCommand, opts, NO_TIMEOUT,
            toReclaimFeatureId: target.id())
    }

    pub fn reclaim_area(&self, pos: Vec3, radius: f32, opts: i16) -> bool {
        let mut p = pos.to_array();
        unit_cmd!(self, COMMAND_UNIT_RECLAIM_AREA, SReclaimAreaUnitCommand, opts, NO_TIMEOUT,
            pos_posF3: p.as_mut_ptr(), radius: radius)
    }

    pub fn resurrect(&self, target: Feature, opts: i16) -> bool {
        unit_cmd!(self, COMMAND_UNIT_RESURRECT, SResurrectUnitCommand, opts, NO_TIMEOUT,
            toResurrectFeatureId: target.id())
    }

    pub fn capture(&self, target: Unit, opts: i16) -> bool {
        unit_cmd!(self, COMMAND_UNIT_CAPTURE, SCaptureUnitCommand, opts, NO_TIMEOUT, toCaptureUnitId: target.id())
    }

    /// 0 = hold position, 1 = maneuver, 2 = roam.
    pub fn set_move_state(&self, state: i32, opts: i16) -> bool {
        unit_cmd!(self, COMMAND_UNIT_SET_MOVE_STATE, SSetMoveStateUnitCommand, opts, NO_TIMEOUT, moveState: state)
    }

    /// 0 = hold fire, 1 = return fire, 2 = fire at will.
    pub fn set_fire_state(&self, state: i32, opts: i16) -> bool {
        unit_cmd!(self, COMMAND_UNIT_SET_FIRE_STATE, SSetFireStateUnitCommand, opts, NO_TIMEOUT, fireState: state)
    }

    /// Aircraft idle behaviour: 0 = fly, 1 = land.
    pub fn set_idle_mode(&self, mode: i32, opts: i16) -> bool {
        unit_cmd!(self, COMMAND_UNIT_SET_IDLE_MODE, SSetIdleModeUnitCommand, opts, NO_TIMEOUT, idleMode: mode)
    }

    pub fn set_repeat(&self, repeat: bool, opts: i16) -> bool {
        unit_cmd!(self, COMMAND_UNIT_SET_REPEAT, SSetRepeatUnitCommand, opts, NO_TIMEOUT, repeat: repeat)
    }

    /// 0 = low, 1 = high trajectory.
    pub fn set_trajectory(&self, trajectory: i32, opts: i16) -> bool {
        unit_cmd!(self, COMMAND_UNIT_SET_TRAJECTORY, SSetTrajectoryUnitCommand, opts, NO_TIMEOUT,
            trajectory: trajectory)
    }

    pub fn set_on_off(&self, on: bool, opts: i16) -> bool {
        unit_cmd!(self, COMMAND_UNIT_SET_ON_OFF, SSetOnOffUnitCommand, opts, NO_TIMEOUT, on: on)
    }

    pub fn cloak(&self, cloak: bool, opts: i16) -> bool {
        unit_cmd!(self, COMMAND_UNIT_CLOAK, SCloakUnitCommand, opts, NO_TIMEOUT, cloak: cloak)
    }

    pub fn self_destruct(&self, opts: i16) -> bool {
        unit_cmd!(self, COMMAND_UNIT_SELF_DESTROY, SSelfDestroyUnitCommand, opts, NO_TIMEOUT)
    }

    /// Queue one more stockpile build (e.g. a missile or nuke).
    pub fn queue_stockpile(&self, opts: i16) -> bool {
        unit_cmd!(self, COMMAND_UNIT_STOCKPILE, SStockpileUnitCommand, opts, NO_TIMEOUT)
    }

    /// Manual-fire ("D-gun") at a unit.
    pub fn dgun(&self, target: Unit, opts: i16) -> bool {
        unit_cmd!(self, COMMAND_UNIT_D_GUN, SDGunUnitCommand, opts, NO_TIMEOUT, toAttackUnitId: target.id())
    }

    /// Manual-fire ("D-gun") at a position.
    pub fn dgun_pos(&self, pos: Vec3, opts: i16) -> bool {
        let mut p = pos.to_array();
        unit_cmd!(self, COMMAND_UNIT_D_GUN_POS, SDGunPosUnitCommand, opts, NO_TIMEOUT, pos_posF3: p.as_mut_ptr())
    }

    /// Issue a game-defined (LuaRules) command such as Zero-K's priority or
    /// morph commands.
    pub fn execute_custom_command(&self, cmd_id: i32, params: &[f32], opts: i16, timeout: i32) -> bool {
        let mut params = params.to_vec();
        unit_cmd!(self, COMMAND_UNIT_CUSTOM, SCustomUnitCommand, opts, timeout,
            cmdId: cmd_id, params: params.as_mut_ptr(), params_size: params.len() as i32)
    }
}

impl crate::Callback {
    /// Send a raw command struct to the engine. Returns `true` on success.
    pub(crate) fn handle_command(&self, topic: CommandTopic::Type, data: *mut c_void) -> bool {
        ffi!(self, Engine_handleCommand, -1, -1, topic as i32, data) == 0
    }
}
