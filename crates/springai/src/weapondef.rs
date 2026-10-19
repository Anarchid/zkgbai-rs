use crate::Callback;
use crate::callback::{ffi, read_floats, read_params};
use crate::macros::{getters, handle};

handle!(
    /// A weapon type definition.
    WeaponDef, def_id
);

getters!(WeaponDef.def_id {
    str fn name() = WeaponDef_getName;
    /// Engine weapon class, e.g. `"BeamLaser"`, `"Cannon"`, `"MissileLauncher"`.
    str fn weapon_type() = WeaponDef_getType;
    str fn description() = WeaponDef_getDescription;
    res fn cost() = WeaponDef_getCost;
    fn range() -> f32 = WeaponDef_getRange;
    fn height_mod() -> f32 = WeaponDef_getHeightMod;
    fn accuracy() -> f32 = WeaponDef_getAccuracy;
    fn spray_angle() -> f32 = WeaponDef_getSprayAngle;
    fn area_of_effect() -> f32 = WeaponDef_getAreaOfEffect;
    fn edge_effectiveness() -> f32 = WeaponDef_getEdgeEffectiveness;
    fn salvo_size() -> i32 = WeaponDef_getSalvoSize;
    fn salvo_delay() -> f32 = WeaponDef_getSalvoDelay;
    /// Reload time in seconds.
    fn reload() -> f32 = WeaponDef_getReload;
    fn beam_time() -> f32 = WeaponDef_getBeamTime;
    fn projectiles_per_shot() -> i32 = WeaponDef_getProjectilesPerShot;
    fn is_turret() -> bool = WeaponDef_isTurret;
    fn is_water_weapon() -> bool = WeaponDef_isWaterWeapon;
    fn is_fire_submersed() -> bool = WeaponDef_isFireSubmersed;
    fn is_sub_missile() -> bool = WeaponDef_isSubMissile;
    fn is_tracks() -> bool = WeaponDef_isTracks;
    fn is_dropped() -> bool = WeaponDef_isDropped;
    fn is_paralyzer() -> bool = WeaponDef_isParalyzer;
    fn is_impact_only() -> bool = WeaponDef_isImpactOnly;
    fn is_no_auto_target() -> bool = WeaponDef_isNoAutoTarget;
    fn is_manual_fire() -> bool = WeaponDef_isManualFire;
    fn interceptor() -> i32 = WeaponDef_getInterceptor;
    fn targetable() -> i32 = WeaponDef_getTargetable;
    fn is_stockpileable() -> bool = WeaponDef_isStockpileable;
    fn coverage_range() -> f32 = WeaponDef_getCoverageRange;
    fn stockpile_time() -> f32 = WeaponDef_getStockpileTime;
    fn is_self_explode() -> bool = WeaponDef_isSelfExplode;
    fn is_gravity_affected() -> bool = WeaponDef_isGravityAffected;
    fn high_trajectory() -> i32 = WeaponDef_getHighTrajectory;
    fn start_velocity() -> f32 = WeaponDef_getStartVelocity;
    fn max_velocity() -> f32 = WeaponDef_getMaxVelocity;
    fn projectile_speed() -> f32 = WeaponDef_getProjectileSpeed;
    fn only_target_category() -> i32 = WeaponDef_getOnlyTargetCategory;
    fn trajectory_height() -> f32 = WeaponDef_getTrajectoryHeight;
    fn is_shield() -> bool = WeaponDef_isShield;
    fn is_able_to_attack_ground() -> bool = WeaponDef_isAbleToAttackGround;
    fn dyn_damage_exp() -> f32 = WeaponDef_getDynDamageExp;
    fn paralyze_damage_time() -> i32 = WeaponDef_Damage_getParalyzeDamageTime;
    fn shield_radius() -> f32 = WeaponDef_Shield_getRadius;
    fn shield_power() -> f32 = WeaponDef_Shield_getPower;
    fn shield_power_regen() -> f32 = WeaponDef_Shield_getPowerRegen;
    fn shield_starting_power() -> f32 = WeaponDef_Shield_getStartingPower;
});

impl WeaponDef {
    /// Damage per armor type (index 0 is the default armor class).
    pub fn damage_types(&self) -> Vec<f32> {
        let (cb, id) = (self.cb, self.def_id);
        read_floats(|buf, max| ffi!(cb, WeaponDef_Damage_getTypes, id, buf, max))
    }

    pub fn custom_params(&self) -> Vec<(String, String)> {
        let (cb, id) = (self.cb, self.def_id);
        read_params(|k, v| ffi!(cb, WeaponDef_getCustomParams, id, k, v))
    }

    pub fn custom_param(&self, key: &str) -> Option<String> {
        self.custom_params().into_iter().find(|(k, _)| k == key).map(|(_, v)| v)
    }
}

impl Callback {
    pub fn weapon_def_by_name(&self, name: &str) -> Option<WeaponDef> {
        let n = crate::callback::c_string(name);
        WeaponDef::from_id(*self, ffi!(self, getWeaponDefByName, n.as_ptr()))
    }

    pub fn num_weapon_defs(&self) -> i32 {
        ffi!(self, getWeaponDefs)
    }
}
