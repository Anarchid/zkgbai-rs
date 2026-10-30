//! Value of a partial bombing pass, with no requirement to kill the clump.
use super::*;

#[derive(Clone, Copy)]
pub(super) struct Blast {
    pub damage: f32,
    pub radius: f32,
    pub edge: f32,
}
impl Default for Blast {
    fn default() -> Self {
        // Fallback only; live planning reads the installed Likho weapon.
        Self { damage: 2000.0, radius: 96.0, edge: 0.5 }
    }
}
impl Blast {
    fn damage_at(self, range: f32) -> f32 {
        if range >= self.radius {
            return 0.0;
        }
        // Recoil's splash falloff, using center distance conservatively instead
        // of distance to the collision volume. Reserve 20% for moving targets.
        let falloff = (self.radius - range) / (self.radius - range * self.edge.clamp(0.0, 1.0)).max(0.001);
        self.damage * 0.8 * falloff
    }
}

#[derive(Clone)]
pub(super) struct GroupStrike {
    targets: Vec<(i32, Vec3, f32, f32)>, // id, position, current HP, metal
    blast: Blast,
}
impl GroupStrike {
    pub fn new(contacts: &[&Contact]) -> Self {
        Self { targets: contacts.iter().map(|c| (c.id, c.pos, c.hp, c.value)).collect(), blast: Blast::default() }
    }
    fn value(&self, aim: Vec3, package: &[(Plane, usize)]) -> f32 {
        let hp = self.targets.iter().map(|t| t.2.max(1.0)).sum::<f32>();
        self.targets
            .iter()
            .map(|&(_, pos, health, metal)| {
                let damage = package
                    .iter()
                    .map(|&(plane, n)| {
                        n as f32
                            * match plane {
                                Plane::Likho => self.blast.damage_at(distance(aim, pos)),
                                // Phoenix's line of napalm is not one spherical blast. Retain
                                // the conservative 1800 aggregate HP budget, spread by HP.
                                Plane::Phoenix => 1800.0 * health.max(1.0) / hp.max(1.0),
                                _ => 0.0,
                            }
                    })
                    .sum::<f32>();
                metal * (damage / health.max(1.0)).clamp(0.0, 1.0)
            })
            .sum()
    }
}
impl Mission {
    pub(super) fn aim_group(&mut self, blast: Blast) {
        let Some(group) = &mut self.group else { return };
        group.blast = blast;
        if let Some(&(id, pos, _, _)) = group.targets.iter().max_by(|a, b| {
            group
                .value(a.1, &[(Plane::Likho, 1)])
                .total_cmp(&group.value(b.1, &[(Plane::Likho, 1)]))
                .then_with(|| b.0.cmp(&a.0))
        }) {
            self.target = id;
            self.pos = pos;
        }
    }
    pub(super) fn damage_value(&self, package: &[(Plane, usize)]) -> f32 {
        self.group.as_ref().map_or(self.value, |g| g.value(self.pos, package))
    }
    pub(super) fn group_packages(&self, planes: &[Plane]) -> Vec<Vec<(Plane, usize)>> {
        if self.group.is_none() {
            return Vec::new();
        }
        let mut packages = Vec::new();
        for &plane in planes {
            // Each additional aircraft must remove worthwhile extra value.
            // This is a sortie threshold, not a demand to trade the airframe.
            let minimum = if plane == Plane::Likho { 250.0 } else { 150.0 };
            let mut previous = 0.0;
            let start = packages.len();
            for n in 1..=MAX_WING {
                let package = vec![(plane, n)];
                let value = self.damage_value(&package);
                if value - previous < minimum {
                    break;
                }
                previous = value;
                packages.push(package);
            }
            // Use more ready bombers when their marginal damage is useful,
            // while keeping the smaller passes valid for launch/procurement.
            packages[start..].reverse();
        }
        packages
    }
}
