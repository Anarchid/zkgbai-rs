//! Role classification of Zero-K unit types by internal name.

use std::collections::BTreeSet;

pub struct UnitClasses {
    pub small_raiders: BTreeSet<&'static str>,
    pub medium_raiders: BTreeSet<&'static str>,
    pub solo_raiders: BTreeSet<&'static str>,
    /// Stuff that attacks in mobs.
    pub assaults: BTreeSet<&'static str>,
    /// Long range; can attack high-threat areas without suiciding.
    pub arties: BTreeSet<&'static str>,
    /// Stuff that can dgun.
    pub striders: BTreeSet<&'static str>,
    pub air_mobs: BTreeSet<&'static str>,
    pub shield_mobs: BTreeSet<&'static str>,
    /// Things that increase the strength of mobs.
    pub mob_supports: BTreeSet<&'static str>,
    /// Strider-like stuff that does better on its own than in mobs.
    pub loners: BTreeSet<&'static str>,
    pub aas: BTreeSet<&'static str>,
    pub planes: BTreeSet<&'static str>,
    pub bombers: BTreeSet<&'static str>,
    pub sappers: BTreeSet<&'static str>,
    /// Things which count higher towards fighter value.
    pub heavies: BTreeSet<&'static str>,
    /// Things that should not retreat.
    pub no_retreat: BTreeSet<&'static str>,
    /// Weapon def ids of static defenses; filled in at init.
    pub porc_weps: BTreeSet<i32>,
}

fn set(names: &[&'static str]) -> BTreeSet<&'static str> {
    names.iter().copied().collect()
}

impl Default for UnitClasses {
    fn default() -> Self {
        Self::new()
    }
}

impl UnitClasses {
    /// Firewalker joins ordinary jumpbot squads only for the opt-in fire
    /// tactics; its default assignment remains the artillery handler.
    pub fn is_assault(&self, name: &str, firesquads: bool) -> bool {
        self.assaults.contains(name) || (firesquads && name == "jumparty")
    }

    pub fn new() -> Self {
        UnitClasses {
            small_raiders: set(&[
                "cloakraid",
                "shieldraid",
                "amphraid",
                "amphbomb",
                "hoverraid",
                "shipscout",
                "shiptorpraider",
            ]),
            medium_raiders: set(&[
                "vehraid",
                "gunshipraid",
                "cloakheavyraid",
                "tankheavyraid",
                "subraider",
                "jumpraid",
            ]),
            solo_raiders: set(&["spiderscout", "vehscout", "shieldscout", "gunshipbomb"]),
            assaults: set(&[
                "cloakassault",
                "cloakriot",
                "cloakskirm",
                "amphfloater",
                "amphriot",
                "amphimpulse",
                "vehriot",
                "vehassault",
                "hoverskirm",
                "hoverriot",
                "hoverassault",
                "hoverdepthcharge",
                "spiderassault",
                "spiderskirm",
                "spideremp",
                "spiderriot",
                "tankriot",
                "tankassault",
                "shieldskirm",
                "gunshipskirm",
                "gunshipheavyskirm",
                "gunshipassault",
                "shipriot",
                "shipskirm",
                "shipassault",
                "jumpskirm",
                "jumpblackhole",
                "jumpassault",
                "jumpsumo",
            ]),
            shield_mobs: set(&[
                "shieldassault",
                "shieldriot",
                "shieldarty",
                "shieldfelon",
                "vehcapture",
                "shieldshield",
            ]),
            mob_supports: set(&["cloakjammer", "amphlaunch"]),
            striders: set(&["striderdante", "striderscorpion", "striderbantha", "striderdetriment"]),
            loners: set(&["vehsupport", "tankheavyassault", "spidercrabe", "amphassault"]),
            arties: set(&[
                "cloaksnipe",
                "cloakarty",
                "veharty",
                "vehheavyarty",
                "hoverarty",
                "tankarty",
                "tankheavyarty",
                "striderarty",
                "shiparty",
                "shipheavyarty",
                "jumparty",
            ]),
            aas: set(&[
                "cloakaa",
                "gunshipaa",
                "amphaa",
                "hoveraa",
                "jumpaa",
                "shieldaa",
                "shipaa",
                "spideraa",
                "tankaa",
                "vehaa",
            ]),
            planes: set(&[
                "planefighter",
                "bomberdive",
                "bomberdisarm",
                "planeheavyfighter",
                "bomberprec",
                "bomberriot",
                "bomberheavy",
                "planescout",
            ]),
            bombers: set(&["bomberdive", "bomberprec", "bomberheavy", "bomberriot", "bomberdisarm"]),
            sappers: set(&["cloakbomb", "shieldbomb", "jumpbomb"]),
            no_retreat: set(&[
                "spidercrabe",
                "shieldshield",
                "shieldfelon",
                "bomberprec",
                "gunshipbomb",
                "planefighter",
            ]),
            heavies: set(&[
                "tankassault",
                "tankheavyassault",
                "spidercrabe",
                "amphassault",
                "shipassault",
                "jumpassault",
                "jumpsumo",
            ]),
            air_mobs: BTreeSet::new(),
            porc_weps: BTreeSet::new(),
        }
    }
}

#[cfg(test)]
mod fire_routing_tests {
    use super::*;
    #[test]
    fn firewalker_routing_is_opt_in_and_siege_artillery_is_unchanged() {
        let classes = UnitClasses::new();
        assert!(crate::ai::OFF_BY_DEFAULT.contains(&"firesquads"));
        assert!(!classes.is_assault("jumparty", false));
        assert!(classes.arties.contains("jumparty"));
        assert!(classes.is_assault("jumparty", true));
        for name in ["cloakarty", "vehheavyarty"] {
            assert!(!classes.is_assault(name, false));
            assert!(!classes.is_assault(name, true));
            assert!(classes.arties.contains(name));
        }
        assert!(classes.is_assault("cloakskirm", false));
    }
}
