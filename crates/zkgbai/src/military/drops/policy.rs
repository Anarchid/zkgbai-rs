//! Drop-specific economics and opposition, kept independent of engine handles
//! so bare and fortified shorelines can be calibrated with identical terrain.
use super::*;
use crate::military::Enemy;

/// Limit a sortie to a local ground itinerary, not every building in the
/// connected mainland. Ground distance, rather than a circle across water.
pub(super) const RAID_REACH: f32 = 2800.0;
const MOBILE_MEMORY: i32 = 30 * 30;
const MAX_FORTIFIED_SHARE: f32 = 0.35;
/// A mex denies roughly 90 seconds of base income as well as its replacement.
/// This is a strategic utility, not a prediction of metal actually destroyed.
const MEX_DENIAL: f32 = 180.0;

pub(super) fn cargo_allowed(name: &str) -> bool {
    matches!(name, "cloakriot" | "vehriot" | "spiderriot" | "hoverriot" | "amphriot")
}

pub(super) fn full_flight(aboard: usize) -> bool {
    aboard >= FLIGHT
}

#[derive(Clone, Debug)]
pub(super) struct Contact {
    pub id: i32,
    pub at: Vec3,
    /// Economic utility only. Turrets and mobile units never inflate it.
    pub value: f32,
    pub danger: f32,
    pub range: f32,
    pub fort: bool,
    pub skirm: bool,
}

fn light_qrf(name: &str, raider: bool, flying: bool) -> bool {
    (!flying && raider) || matches!(name, "gunshipraid" | "gunshipskirm" | "gunshipemp")
}

fn counter(e: &Enemy, cargo_range: f32) -> bool {
    !e.is_static && !e.can_fly() && !e.is_worker && e.is_arty && e.def_max_weapon_range() > cargo_range + 60.0
}

impl Ai {
    pub(super) fn drops_contacts(&self) -> Vec<Contact> {
        self.war
            .targets
            .values()
            .filter(|e| !e.is_dead && (e.is_static || self.frame - e.last_seen <= MOBILE_MEMORY))
            .map(|e| {
                let economic = e.identified
                    && e.ud.is_some()
                    && e.is_static
                    && (!e.can_attack() || e.is_super_wep)
                    && !self.graph.is_ally_territory(e.position);
                // Only known ground-firing defenses are fortifications. AA
                // is assessed separately over the transport route.
                let fort = e.is_static && e.surface_danger(e.is_submerged()) > 0.0;
                let skirm = counter(e, 345.0);
                let mut danger = e.surface_danger(e.is_submerged());
                if e.ud.is_none() {
                    danger = danger.max(100.0);
                }
                if !e.is_static && light_qrf(&e.def_name, e.is_raider, e.can_fly()) {
                    danger *= 0.1;
                } else if skirm {
                    // Generic danger divides arty/skirm strength by three;
                    // riots must respect these counters instead.
                    danger *= 3.0;
                }
                Contact {
                    id: e.unit_id,
                    at: e.position,
                    value: if economic { e.value + if e.is_mex { MEX_DENIAL } else { 0.0 } } else { 0.0 },
                    danger,
                    range: e.def_max_weapon_range().max(e.threat_radius),
                    fort,
                    skirm,
                }
            })
            .collect()
    }
}

/// Economic value behind an overwhelming held chokepoint is not a locally
/// reachable prize. Use riot-specific danger so light QRF does not close it.
pub(super) fn combat_paths<'a>(map: &'a TierMap, contacts: &[Contact], from: Vec3, budget: f32) -> Paths<'a> {
    Paths::new(map, pos(from), |c| {
        let gate = map.chokes[c].pos;
        danger_at(contacts, Vec3::xz(gate.x, gate.z)) > budget
    })
}

pub(super) fn danger_at(contacts: &[Contact], at: Vec3) -> f32 {
    contacts.iter().filter(|c| distance(c.at, at) <= c.range + 200.0).map(|c| c.danger).sum()
}

/// Reject routine defenses spread throughout the prize, even when five riots
/// could overpower any one Lotus. A lone guarded checkpoint in an otherwise
/// open pocket is fine; every mex guarded is not. Inaccessible value counts
/// neither as a prize nor toward diluting the fortified fraction.
pub(super) fn pocket_value(
    contacts: &[Contact],
    at: Vec3,
    power: f32,
    reachable: impl Fn(Vec3) -> bool,
) -> Option<f32> {
    if danger_at(contacts, at) > GROUND_ODDS * power {
        return None;
    }
    let (mut total, mut guarded, mut value) = (0.0, 0.0, 0.0);
    for c in contacts.iter().filter(|c| c.value > 0.0 && reachable(c.at)) {
        let fortified = contacts.iter().any(|d| d.fort && distance(d.at, c.at) <= d.range + 200.0);
        total += c.value;
        if fortified {
            guarded += c.value;
        }
        // Never profit from defenses themselves, nor count heavily held
        // economic checkpoints as damage the sortie can realistically do.
        if danger_at(contacts, c.at) <= GROUND_ODDS * power {
            value += c.value * if fortified { 0.1 } else { 1.0 };
        }
    }
    (total > 0.0 && guarded <= MAX_FORTIFIED_SHARE * total).then_some(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mex(id: i32, x: f32) -> Contact {
        Contact {
            id,
            at: Vec3::xz(x, 0.0),
            value: 85.0 + MEX_DENIAL,
            danger: 0.0,
            range: 0.0,
            fort: false,
            skirm: false,
        }
    }
    fn lotus(x: f32) -> Contact {
        Contact { id: 100, at: Vec3::xz(x, 100.0), value: 0.0, danger: 100.0, range: 460.0, fort: true, skirm: false }
    }
    #[test]
    fn only_the_five_true_riots_fit_the_policy() {
        for name in ["cloakriot", "vehriot", "spiderriot", "hoverriot", "amphriot"] {
            assert!(cargo_allowed(name));
        }
        for name in ["cloakskirm", "shieldskirm", "hoverassault", "cloakassault", "cloakraid", "shieldriot", "tankriot"]
        {
            assert!(!cargo_allowed(name));
        }
        for n in 0..5 {
            assert!(!full_flight(n));
        }
        assert!(full_flight(5));
    }
    #[test]
    fn bare_shore_qualifies_but_one_lotus_per_mex_does_not() {
        let bare: Vec<_> = (0..5).map(|i| mex(i, i as f32 * 700.0)).collect();
        let score = pocket_value(&bare, bare[0].at, 600.0, |_| true).unwrap();
        assert!(score >= MIN_DROP_VALUE);
        let mut circuit = bare.clone();
        circuit.extend((0..5).map(|i| lotus(i as f32 * 700.0)));
        assert!(pocket_value(&circuit, bare[0].at, 600.0, |_| true).is_none());
        // Force size must not turn the whole fortified shore into a prize.
        assert!(pocket_value(&circuit, bare[0].at, 2000.0, |_| true).is_none());
    }
    #[test]
    fn sparse_defense_and_reachability() {
        let mut c: Vec<_> = (0..5).map(|i| mex(i, i as f32 * 700.0)).collect();
        c.push(lotus(0.0));
        assert!(pocket_value(&c, c[0].at, 600.0, |_| true).unwrap() >= MIN_DROP_VALUE);
        assert!(pocket_value(&c, c[0].at, 600.0, |p| p.x < 500.0).is_none());
        assert!(pocket_value(&[lotus(0.0)], Vec3::xz(0.0, 0.0), 600.0, |_| true).is_none());
    }
    #[test]
    fn light_reactions_are_discounted_but_heavy_air_is_not() {
        assert!(light_qrf("cloakraid", true, false));
        assert!(light_qrf("gunshipraid", false, true));
        assert!(light_qrf("gunshipskirm", false, true));
        assert!(!light_qrf("gunshipheavyskirm", false, true));
        assert!(!light_qrf("cloakskirm", false, false));
    }
}
