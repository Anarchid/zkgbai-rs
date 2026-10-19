//! Unit-def ids of common economy buildings.

use std::collections::BTreeSet;

use crate::defs::Defs;

pub struct BuildIds {
    pub mex_id: i32,
    pub solar_id: i32,
    pub wind_id: i32,
    pub nano_id: i32,
    pub storage_id: i32,
    pub airpad_id: i32,
    pub pylon_id: i32,
    pub radar_id: i32,
    pub fac_ids: BTreeSet<i32>,
    pub expensive_ids: BTreeSet<i32>,
}

/// Placeholder before `eco_init`: every id is -1 (matches nothing).
impl Default for BuildIds {
    fn default() -> Self {
        BuildIds {
            mex_id: -1,
            solar_id: -1,
            wind_id: -1,
            nano_id: -1,
            storage_id: -1,
            airpad_id: -1,
            pylon_id: -1,
            radar_id: -1,
            fac_ids: BTreeSet::new(),
            expensive_ids: BTreeSet::new(),
        }
    }
}

impl BuildIds {
    /// Missing definitions get id -1 (never matches).
    pub fn new(defs: &Defs) -> Self {
        let mut fac_ids = BTreeSet::new();
        let mut expensive_ids = BTreeSet::new();
        let metal = defs.all().next().and_then(|d| d.callback().resource_by_name("Metal"));
        for ud in defs.all() {
            let name = defs.name(ud);
            if name.contains("factory") || name.contains("hub") {
                fac_ids.insert(ud.id());
            } else if ud.speed() == 0.0
                && metal.is_some_and(|m| ud.cost(m) > 200.0)
                && ud.is_able_to_attack()
                && !ud.tooltip().contains("Anti-Air")
            {
                expensive_ids.insert(ud.id());
            }
        }
        BuildIds {
            mex_id: defs.id("staticmex"),
            solar_id: defs.id("energysolar"),
            wind_id: defs.id("energywind"),
            nano_id: defs.id("staticcon"),
            storage_id: defs.id("staticstorage"),
            airpad_id: defs.id("staticrearm"),
            pylon_id: defs.id("energypylon"),
            radar_id: defs.id("staticradar"),
            fac_ids,
            expensive_ids,
        }
    }
}
