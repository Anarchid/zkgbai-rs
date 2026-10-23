use crate::callback::{c_string, ffi, read_ids};
use crate::macros::{getters, handle};
use crate::{Callback, UnitDef, Vec3};

handle!(
    /// A map feature: wreck, rock, tree, ...
    Feature, feature_id
);

getters!(Feature.feature_id {
    fn health() -> f32 = Feature_getHealth;
    /// Fraction of the feature left to reclaim (1.0 = untouched).
    fn reclaim_left() -> f32 = Feature_getReclaimLeft;
});

impl Feature {
    pub fn def(&self) -> Option<FeatureDef> {
        FeatureDef::from_id(self.cb, ffi!(self.cb, Feature_getDef, self.feature_id))
    }

    pub fn pos(&self) -> Vec3 {
        let mut out = [0f32; 3];
        ffi!(self.cb, Feature_getPosition, self.feature_id, out.as_mut_ptr());
        Vec3::from_array(out)
    }

    /// The unit this wreck can be resurrected into, if any.
    pub fn resurrect_def(&self) -> Option<UnitDef> {
        UnitDef::from_id(self.cb, ffi!(self.cb, Feature_getResurrectDef, self.feature_id))
    }

    pub fn rules_param_float(&self, name: &str, default: f32) -> f32 {
        let n = c_string(name);
        ffi!(self.cb, Feature_getRulesParamFloat, self.feature_id, n.as_ptr(), default)
    }
}

handle!(
    /// A feature type definition.
    FeatureDef, def_id
);

getters!(FeatureDef.def_id {
    str fn name() = FeatureDef_getName;
    str fn description() = FeatureDef_getDescription;
    res fn contained_resource() = FeatureDef_getContainedResource;
    fn max_health() -> f32 = FeatureDef_getMaxHealth;
    fn is_reclaimable() -> bool = FeatureDef_isReclaimable;
    /// A geothermal vent: geothermal plants are built over it.
    fn is_geo_thermal() -> bool = FeatureDef_isGeoThermal;
    fn x_size() -> i32 = FeatureDef_getXSize;
    fn z_size() -> i32 = FeatureDef_getZSize;
});

impl Callback {
    /// All features visible to this AI.
    pub fn features(&self) -> Vec<Feature> {
        let cb = *self;
        read_ids(|buf, max| ffi!(cb, getFeatures, buf, max))
            .into_iter()
            .filter_map(|f| Feature::from_id(cb, f))
            .collect()
    }

    /// Visible features within a sphere of `radius` around `pos`.
    pub fn features_in(&self, pos: Vec3, radius: f32) -> Vec<Feature> {
        self.features_in_shape(pos, radius, true)
    }

    /// Visible features within an infinite vertical cylinder around `pos`.
    pub fn features_in_cylinder(&self, pos: Vec3, radius: f32) -> Vec<Feature> {
        self.features_in_shape(pos, radius, false)
    }

    fn features_in_shape(&self, pos: Vec3, radius: f32, spherical: bool) -> Vec<Feature> {
        let cb = *self;
        let mut p = pos.to_array();
        read_ids(|buf, max| ffi!(cb, getFeaturesIn, p.as_mut_ptr(), radius, spherical, buf, max))
            .into_iter()
            .filter_map(|f| Feature::from_id(cb, f))
            .collect()
    }
}
