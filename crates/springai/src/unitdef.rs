use crate::callback::{ffi, owned_str, read_ids, read_params};
use crate::macros::{getters, handle};
use crate::{Callback, WeaponDef};

handle!(
    /// A unit type definition.
    UnitDef, def_id
);

getters!(UnitDef.def_id {
    /// Internal name, e.g. `"cloakraid"`.
    str fn name() = UnitDef_getName;
    /// Display name, e.g. `"Glaive"`.
    str fn human_name() = UnitDef_getHumanName;
    str fn tooltip() = UnitDef_getTooltip;
    str fn wreck_name() = UnitDef_getWreckName;
    res fn cost() = UnitDef_getCost;
    res fn upkeep() = UnitDef_getUpkeep;
    res fn resource_make() = UnitDef_getResourceMake;
    res fn makes_resource() = UnitDef_getMakesResource;
    res fn extracts_resource() = UnitDef_getExtractsResource;
    res fn resource_extractor_range() = UnitDef_getResourceExtractorRange;
    res fn wind_resource_generator() = UnitDef_getWindResourceGenerator;
    res fn tidal_resource_generator() = UnitDef_getTidalResourceGenerator;
    res fn storage() = UnitDef_getStorage;
    fn height() -> f32 = UnitDef_getHeight;
    fn radius() -> f32 = UnitDef_getRadius;
    fn build_time() -> f32 = UnitDef_getBuildTime;
    fn power() -> f32 = UnitDef_getPower;
    fn health() -> f32 = UnitDef_getHealth;
    fn category() -> i32 = UnitDef_getCategory;
    fn speed() -> f32 = UnitDef_getSpeed;
    fn turn_rate() -> f32 = UnitDef_getTurnRate;
    /// Engine turn speed floor, in elmos per frame.
    fn turn_in_place_speed_limit() -> f32 = UnitDef_getTurnInPlaceSpeedLimit;
    fn los_radius() -> f32 = UnitDef_getLosRadius;
    fn air_los_radius() -> f32 = UnitDef_getAirLosRadius;
    fn radar_radius() -> i32 = UnitDef_getRadarRadius;
    fn sonar_radius() -> i32 = UnitDef_getSonarRadius;
    fn jammer_radius() -> i32 = UnitDef_getJammerRadius;
    fn seismic_radius() -> i32 = UnitDef_getSeismicRadius;
    fn is_stealth() -> bool = UnitDef_isStealth;
    fn is_sonar_stealth() -> bool = UnitDef_isSonarStealth;
    fn build_distance() -> f32 = UnitDef_getBuildDistance;
    fn build_speed() -> f32 = UnitDef_getBuildSpeed;
    fn reclaim_speed() -> f32 = UnitDef_getReclaimSpeed;
    fn repair_speed() -> f32 = UnitDef_getRepairSpeed;
    fn terraform_speed() -> f32 = UnitDef_getTerraformSpeed;
    fn mass() -> f32 = UnitDef_getMass;
    fn max_height_dif() -> f32 = UnitDef_getMaxHeightDif;
    fn min_water_depth() -> f32 = UnitDef_getMinWaterDepth;
    fn waterline() -> f32 = UnitDef_getWaterline;
    fn max_water_depth() -> f32 = UnitDef_getMaxWaterDepth;
    fn armored_multiple() -> f32 = UnitDef_getArmoredMultiple;
    fn armor_type() -> i32 = UnitDef_getArmorType;
    fn max_weapon_range() -> f32 = UnitDef_getMaxWeaponRange;
    fn is_able_to_self_d() -> bool = UnitDef_isAbleToSelfD;
    fn is_able_to_submerge() -> bool = UnitDef_isAbleToSubmerge;
    fn is_able_to_fly() -> bool = UnitDef_isAbleToFly;
    fn is_able_to_move() -> bool = UnitDef_isAbleToMove;
    fn is_able_to_hover() -> bool = UnitDef_isAbleToHover;
    fn is_floater() -> bool = UnitDef_isFloater;
    fn is_builder() -> bool = UnitDef_isBuilder;
    fn is_activate_when_built() -> bool = UnitDef_isActivateWhenBuilt;
    fn is_on_offable() -> bool = UnitDef_isOnOffable;
    fn is_reclaimable() -> bool = UnitDef_isReclaimable;
    fn is_capturable() -> bool = UnitDef_isCapturable;
    fn is_able_to_repair() -> bool = UnitDef_isAbleToRepair;
    fn is_able_to_reclaim() -> bool = UnitDef_isAbleToReclaim;
    fn is_able_to_attack() -> bool = UnitDef_isAbleToAttack;
    fn is_able_to_patrol() -> bool = UnitDef_isAbleToPatrol;
    fn is_able_to_fight() -> bool = UnitDef_isAbleToFight;
    fn is_able_to_guard() -> bool = UnitDef_isAbleToGuard;
    fn is_able_to_assist() -> bool = UnitDef_isAbleToAssist;
    fn is_able_to_repeat() -> bool = UnitDef_isAbleToRepeat;
    fn is_able_to_fire_control() -> bool = UnitDef_isAbleToFireControl;
    fn is_able_to_cloak() -> bool = UnitDef_isAbleToCloak;
    fn is_start_cloaked() -> bool = UnitDef_isStartCloaked;
    fn cloak_cost() -> f32 = UnitDef_getCloakCost;
    fn cloak_cost_moving() -> f32 = UnitDef_getCloakCostMoving;
    fn decloak_distance() -> f32 = UnitDef_getDecloakDistance;
    fn is_able_to_kamikaze() -> bool = UnitDef_isAbleToKamikaze;
    fn kamikaze_dist() -> f32 = UnitDef_getKamikazeDist;
    fn can_manual_fire() -> bool = UnitDef_canManualFire;
    fn is_able_to_resurrect() -> bool = UnitDef_isAbleToResurrect;
    fn is_able_to_capture() -> bool = UnitDef_isAbleToCapture;
    fn high_trajectory_type() -> i32 = UnitDef_getHighTrajectoryType;
    fn is_level_ground() -> bool = UnitDef_isLevelGround;
    fn max_this_unit() -> i32 = UnitDef_getMaxThisUnit;
    fn is_dont_land() -> bool = UnitDef_isDontLand;
    fn fire_state() -> i32 = UnitDef_getFireState;
    fn move_state() -> i32 = UnitDef_getMoveState;
    fn turn_radius() -> f32 = UnitDef_getTurnRadius;
    fn wanted_height() -> f32 = UnitDef_getWantedHeight;
    fn is_hover_attack() -> bool = UnitDef_isHoverAttack;
    fn is_air_strafe() -> bool = UnitDef_isAirStrafe;
    fn max_acceleration() -> f32 = UnitDef_getMaxAcceleration;
    fn max_deceleration() -> f32 = UnitDef_getMaxDeceleration;
    /// Footprint in heightmap squares (8 elmos).
    fn x_size() -> i32 = UnitDef_getXSize;
    /// Footprint in heightmap squares (8 elmos).
    fn z_size() -> i32 = UnitDef_getZSize;
    fn transport_capacity() -> i32 = UnitDef_getTransportCapacity;
    /// Largest footprint a transport carries (in footprint squares, half
    /// of [`x_size`](Self::x_size)'s units).
    fn transport_size() -> i32 = UnitDef_getTransportSize;
    fn is_air_base() -> bool = UnitDef_isAirBase;
    fn is_need_geo() -> bool = UnitDef_isNeedGeo;
    fn is_feature() -> bool = UnitDef_isFeature;
    fn num_weapon_mounts() -> i32 = UnitDef_getWeaponMounts;
});

impl UnitDef {
    pub fn build_options(&self) -> Vec<UnitDef> {
        let (cb, id) = (self.cb, self.def_id);
        read_ids(|buf, max| ffi!(cb, UnitDef_getBuildOptions, id, buf, max))
            .into_iter()
            .filter_map(|d| UnitDef::from_id(cb, d))
            .collect()
    }

    pub fn custom_params(&self) -> Vec<(String, String)> {
        let (cb, id) = (self.cb, self.def_id);
        read_params(|k, v| ffi!(cb, UnitDef_getCustomParams, id, k, v))
    }

    /// Look up a single `customParams` entry.
    pub fn custom_param(&self, key: &str) -> Option<String> {
        self.custom_params().into_iter().find(|(k, _)| k == key).map(|(_, v)| v)
    }

    pub fn weapon_mounts(&self) -> Vec<WeaponMount> {
        (0..self.num_weapon_mounts().max(0)).map(|index| WeaponMount { def: *self, index }).collect()
    }

    /// Ground/sea movement properties; `None` for aircraft and buildings.
    pub fn move_data(&self) -> Option<MoveData> {
        ffi!(self.cb, UnitDef_isMoveDataAvailable, self.def_id).then_some(MoveData { def: *self })
    }

    /// The stockpile weapon, if any.
    pub fn stockpile_def(&self) -> Option<WeaponDef> {
        WeaponDef::from_id(self.cb, ffi!(self.cb, UnitDef_getStockpileDef, self.def_id))
    }

    /// The shield weapon, if any.
    pub fn shield_def(&self) -> Option<WeaponDef> {
        WeaponDef::from_id(self.cb, ffi!(self.cb, UnitDef_getShieldDef, self.def_id))
    }

    /// The explosion when this unit is killed.
    pub fn death_explosion(&self) -> Option<WeaponDef> {
        WeaponDef::from_id(self.cb, ffi!(self.cb, UnitDef_getDeathExplosion, self.def_id))
    }

    /// The explosion when this unit self-destructs (including kamikaze bombs).
    pub fn self_d_explosion(&self) -> Option<WeaponDef> {
        WeaponDef::from_id(self.cb, ffi!(self.cb, UnitDef_getSelfDExplosion, self.def_id))
    }
}

/// A weapon slot on a unit definition.
#[derive(Clone, Copy, Debug)]
pub struct WeaponMount {
    def: UnitDef,
    index: i32,
}

impl WeaponMount {
    pub fn index(&self) -> i32 {
        self.index
    }

    pub fn name(&self) -> String {
        owned_str(ffi!(self.def.cb, UnitDef_WeaponMount_getName, self.def.def_id, self.index))
    }

    pub fn weapon_def(&self) -> Option<WeaponDef> {
        WeaponDef::from_id(
            self.def.cb,
            ffi!(self.def.cb, UnitDef_WeaponMount_getWeaponDef, self.def.def_id, self.index),
        )
    }

    pub fn only_target_category(&self) -> i32 {
        ffi!(self.def.cb, UnitDef_WeaponMount_getOnlyTargetCategory, self.def.def_id, self.index)
    }

    pub fn bad_target_category(&self) -> i32 {
        ffi!(self.def.cb, UnitDef_WeaponMount_getBadTargetCategory, self.def.def_id, self.index)
    }
}

/// Movement class of a mobile, non-flying unit definition.
#[derive(Clone, Copy, Debug)]
pub struct MoveData {
    def: UnitDef,
}

macro_rules! movedata_getters {
    ($($name:ident -> $ret:ty = $cfn:ident;)*) => {
        impl MoveData {
            $(
                #[inline]
                pub fn $name(&self) -> $ret {
                    ffi!(self.def.cb, $cfn, self.def.def_id)
                }
            )*
        }
    };
}

movedata_getters! {
    x_size -> i32 = UnitDef_MoveData_getXSize;
    z_size -> i32 = UnitDef_MoveData_getZSize;
    depth -> f32 = UnitDef_MoveData_getDepth;
    max_slope -> f32 = UnitDef_MoveData_getMaxSlope;
    slope_mod -> f32 = UnitDef_MoveData_getSlopeMod;
    path_type -> i32 = UnitDef_MoveData_getPathType;
    crush_strength -> f32 = UnitDef_MoveData_getCrushStrength;
    speed_mod_class -> i32 = UnitDef_MoveData_getSpeedModClass;
    terrain_class -> i32 = UnitDef_MoveData_getTerrainClass;
    follow_ground -> bool = UnitDef_MoveData_getFollowGround;
    is_submarine -> bool = UnitDef_MoveData_isSubMarine;
}

impl MoveData {
    pub fn unit_def(&self) -> UnitDef {
        self.def
    }

    pub fn name(&self) -> String {
        owned_str(ffi!(self.def.cb, UnitDef_MoveData_getName, self.def.def_id))
    }
}

impl Callback {
    /// All unit definitions in the game.
    pub fn unit_defs(&self) -> Vec<UnitDef> {
        let cb = *self;
        read_ids(|buf, max| ffi!(cb, getUnitDefs, buf, max))
            .into_iter()
            .filter_map(|d| UnitDef::from_id(cb, d))
            .collect()
    }

    pub fn unit_def_by_name(&self, name: &str) -> Option<UnitDef> {
        let n = crate::callback::c_string(name);
        UnitDef::from_id(*self, ffi!(self, getUnitDefByName, n.as_ptr()))
    }
}
