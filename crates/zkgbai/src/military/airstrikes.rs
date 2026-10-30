//! Mission-driven airplane production and sorties. Policy is independent of
//! engine handles so target selection and procurement can be exercised offline.

mod live;
mod scenario;
#[cfg(test)]
mod tests;

use crate::util::distance;
use springai::Vec3;

pub use live::BomberHandler;

pub const MEMORY: i32 = 30 * 90;
pub const FRESH: i32 = 30 * 5;
pub const MAX_WING: usize = 24;
const MIN_PRIORITY_METAL: f32 = 600.0;
// Current fragility drives procurement, so wounded expensive units can become
// priority targets even when their undamaged counterparts are too tough.
const MAX_PRIORITY_HP_PER_METAL: f32 = 3.0;
fn priority_target(value: f32, hp: f32) -> bool {
    value >= MIN_PRIORITY_METAL && hp / value <= MAX_PRIORITY_HP_PER_METAL
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Plane {
    Raven,
    Phoenix,
    Likho,
    Magpie,
    Raptor,
    Odin,
    Sparrow,
}

impl Plane {
    pub const ALL: [Self; 7] =
        [Self::Raven, Self::Phoenix, Self::Likho, Self::Magpie, Self::Raptor, Self::Odin, Self::Sparrow];
    pub fn name(self) -> &'static str {
        match self {
            Self::Raven => "bomberprec",
            Self::Phoenix => "bomberriot",
            Self::Likho => "bomberheavy",
            Self::Magpie => "planesupport",
            Self::Raptor => "planeheavyfighter",
            Self::Odin => "bomberassault",
            Self::Sparrow => "planelightscout",
        }
    }
    pub fn of(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|p| p.name() == name)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum MissionKind {
    Intercept,
    Interdict,
    Group,
    Heavy,
    Wind,
    Thermite,
    Assassinate,
    Opportunistic,
    Static,
    Shield,
}
impl MissionKind {
    fn label(self) -> &'static str {
        match self {
            Self::Heavy => "Priority strike",
            Self::Opportunistic => "Opportunistic strike",
            Self::Group => "Group strike",
            Self::Intercept => "Interception",
            Self::Interdict => "Interdiction",
            Self::Wind => "Wind farm strike",
            Self::Thermite => "Thermite strike",
            Self::Assassinate => "Assassination",
            Self::Static => "Static strike",
            Self::Shield => "Shield drop",
        }
    }

    fn primary(self) -> bool {
        !matches!(self, Self::Assassinate | Self::Opportunistic | Self::Static | Self::Shield)
    }
    fn priority(self) -> f32 {
        match self {
            Self::Intercept => 10.0,
            Self::Interdict => 9.0,
            Self::Heavy => 8.0,
            Self::Wind => 7.0,
            Self::Thermite => 7.0,
            Self::Group => 6.0,
            Self::Assassinate | Self::Opportunistic => 3.0,
            Self::Shield => 2.0,
            Self::Static => 1.0,
        }
    }
}

#[derive(Clone)]
struct Contact {
    id: i32,
    pos: Vec3,
    vel: Vec3,
    value: f32,
    hp: f32,
    max_hp: f32,
    seen: i32,
    known: bool,
    live: bool,
    air: bool,
    static_: bool,
    heavy: bool,
    raider: bool,
    wind: bool,
    singu: bool,
    home: bool,
    fighting: bool,
}

#[derive(Clone)]
struct Mission {
    kind: MissionKind,
    target: i32,
    pos: Vec3,
    hp: f32,
    value: f32,
    /// All targets covered by this job, also used to avoid overlapping sorties.
    targets: Vec<i32>,
    seen: i32,
    deep: bool,
}
impl Mission {
    fn key(&self) -> (MissionKind, i32) {
        (self.kind, self.target)
    }
    fn score(&self) -> f32 {
        let reward = if matches!(self.kind, MissionKind::Heavy | MissionKind::Opportunistic | MissionKind::Assassinate)
        {
            self.value / self.hp.max(1.0) * 3000.0
        } else {
            self.value
        };
        self.kind.priority() * 10000.0 + reward.min(9000.0)
    }
}

fn inbound(pos: Vec3, vel: Vec3, home: Vec3, home_territory: bool) -> bool {
    if home_territory {
        return true;
    }
    let (dx, dz) = (home.x - pos.x, home.z - pos.z);
    let speed2 = vel.x * vel.x + vel.z * vel.z;
    if speed2 < 1.0 {
        return false;
    }
    let t = (dx * vel.x + dz * vel.z) / speed2;
    (0.0..=25.0).contains(&t) && distance(pos + vel * t, home) < 1800.0
}

/// Earliest intercept of a constant-velocity target, in seconds; no solution
/// when it is escaping faster than the aircraft can close.
fn intercept_time(from: Vec3, target: Vec3, velocity: Vec3, speed: f32) -> Option<f32> {
    let (x, z) = (target.x - from.x, target.z - from.z);
    let a = velocity.x * velocity.x + velocity.z * velocity.z - speed * speed;
    let b = 2.0 * (x * velocity.x + z * velocity.z);
    let c = x * x + z * z;
    if c < 1.0 {
        return Some(0.0);
    }
    if a.abs() < 0.01 {
        return (b < 0.0).then(|| -c / b);
    }
    let d = b * b - 4.0 * a * c;
    if d < 0.0 {
        return None;
    }
    [(-b - d.sqrt()) / (2.0 * a), (-b + d.sqrt()) / (2.0 * a)]
        .into_iter()
        .filter(|t| t.is_finite() && *t >= 0.0)
        .min_by(f32::total_cmp)
}

fn missions(contacts: &[Contact], home: Vec3, frame: i32) -> Vec<Mission> {
    let mut jobs = Vec::new();
    let mut clustered = std::collections::BTreeSet::new();
    for e in contacts {
        let fresh = e.live && frame - e.seen <= FRESH;
        if !e.static_ && !fresh {
            continue;
        }
        let mut add = |kind, targets: Vec<i32>, hp, value| {
            jobs.push(Mission {
                kind,
                target: e.id,
                pos: e.pos,
                hp,
                value,
                targets,
                seen: frame,
                deep: e.singu || distance(e.pos, home) >= 3000.0,
            })
        };
        if e.air {
            if fresh && inbound(e.pos, e.vel, home, e.home) {
                add(MissionKind::Intercept, vec![e.id], e.hp.max(1000.0), e.value);
            }
            continue;
        }
        if !e.known {
            continue;
        }
        if e.static_ {
            if e.value >= 1000.0 {
                add(MissionKind::Thermite, vec![e.id], e.hp, e.value + if e.singu { 4000.0 } else { 0.0 });
            }
            add(MissionKind::Static, vec![e.id], e.hp, e.value);
        } else {
            if e.heavy {
                add(
                    if priority_target(e.value, e.hp) { MissionKind::Heavy } else { MissionKind::Opportunistic },
                    vec![e.id],
                    e.hp,
                    e.value,
                );
            }
            if e.value >= 700.0 && e.hp < e.max_hp * 0.55 {
                add(MissionKind::Assassinate, vec![e.id], e.hp, e.value);
            }
            if e.raider && e.home {
                add(MissionKind::Interdict, vec![e.id], e.hp, e.value);
            }
            if e.fighting {
                add(MissionKind::Shield, vec![e.id], e.hp, e.value);
            }
        }
        let medium = |c: &Contact| {
            !c.air && !c.static_ && c.known && c.live && frame - c.seen <= FRESH && (150.0..=1200.0).contains(&c.value)
        };
        if (e.wind || medium(e)) && !clustered.contains(&e.id) {
            let group: Vec<_> = contacts
                .iter()
                .filter(|c| distance(c.pos, e.pos) < 260.0 && if e.wind { c.wind } else { medium(c) })
                .collect();
            let value = group.iter().map(|c| c.value).sum::<f32>();
            if (e.wind && group.len() >= 5) || (!e.wind && group.len() >= 3 && value >= 600.0) {
                let targets = group.iter().map(|c| c.id).collect::<Vec<_>>();
                clustered.extend(targets.iter().copied());
                add(
                    if e.wind { MissionKind::Wind } else { MissionKind::Group },
                    targets,
                    group.iter().map(|c| c.hp).sum(),
                    value,
                );
            }
        }
    }
    jobs.sort_by(|a, b| b.score().total_cmp(&a.score()).then_with(|| a.key().cmp(&b.key())));
    jobs.truncate(128);
    jobs
}

/// Threat is the existing AA map's scale, not DPS. Exposure is integrated
/// over 64-elmo cells. Convert conservatively for a round trip plus six seconds
/// turning/releasing at the target. This is a tunable survival heuristic.
#[derive(Clone, Copy)]
struct Risk {
    aa: f32,
    exposure: f32,
    intercepted: bool,
}
impl Risk {
    fn damage(self, speed: f32) -> f32 {
        (2.0 * self.exposure * 64.0 / speed.max(1.0) + 6.0 * self.aa) * 500.0
    }
    fn survives(self, health: f32, speed: f32) -> bool {
        self.damage(speed).is_finite() && self.damage(speed) < health * 0.55
    }
}

/// Alternatives in preference order; interception is a joint package.
fn packages(job: &Mission, risk: Risk, raven_damage: f32) -> Vec<Vec<(Plane, usize)>> {
    use MissionKind::*;
    use Plane::*;
    let count = |hp: f32, damage: f32| (hp / damage.max(1.0)).ceil().max(1.0) as usize;
    // Apply the margin in f64 so exact multiples do not round just above
    // a bomb boundary and demand another aircraft (e.g. 12000 HP / 800).
    let salvo = |damage: f32| (f64::from(job.hp) * 1.2 / f64::from(damage.max(1.0))).ceil().max(1.0) as usize;
    let raven = salvo(raven_damage) + 1;
    let clear = risk.aa <= 0.02 && !risk.intercepted;
    let very_light = risk.aa <= 0.15 && !risk.intercepted;
    let light = risk.aa <= 1.25 && !risk.intercepted;
    let choices = match job.kind {
        Intercept if risk.aa <= 0.4 => vec![vec![(Magpie, 1), (Raptor, count(job.hp, 700.0).max(2))]],
        Interdict if clear => vec![vec![(Phoenix, count(job.hp, 1000.0))], vec![(Magpie, count(job.hp, 350.0).max(2))]],
        Group if clear => vec![vec![(Phoenix, count(job.hp, 1800.0))], vec![(Likho, count(job.hp, 3500.0))]],
        Group if light => vec![vec![(Likho, count(job.hp, 3500.0))]],
        Wind if clear => vec![vec![(Phoenix, job.targets.len().div_ceil(12))]],
        Heavy | Opportunistic if very_light => vec![vec![(Raven, raven)], vec![(Likho, salvo(2000.0))]],
        Heavy | Opportunistic if light => vec![vec![(Likho, salvo(2000.0))]],
        Thermite if !risk.intercepted => {
            let n = count(job.hp, 14000.0);
            let mut package = vec![(Odin, n)];
            if job.deep {
                package.push((Sparrow, n));
            }
            vec![package]
        }
        Assassinate | Static if very_light => vec![vec![(Raven, raven)]],
        Shield if !risk.intercepted => vec![vec![(Odin, 1)]],
        _ => vec![],
    };
    choices.into_iter().filter(|p| p.iter().map(|(_, n)| n).sum::<usize>() <= MAX_WING).collect()
}

/// One purchase toward the cheapest deficit in metal. A completed
/// alternative wins over every incomplete one. Inventory includes reservations
/// from other factories, nanoframes, rearming aircraft and active sorties.
fn purchase(
    packages: &[Vec<(Plane, usize)>],
    have: &std::collections::BTreeMap<Plane, usize>,
    cost: impl Fn(Plane) -> f32,
) -> Option<Plane> {
    let deficit = |p: &Vec<(Plane, usize)>| {
        p.iter().map(|(k, n)| n.saturating_sub(*have.get(k).unwrap_or(&0)) as f32 * cost(*k)).sum::<f32>()
    };
    let package = packages.iter().min_by(|a, b| deficit(a).total_cmp(&deficit(b)))?;
    package.iter().find(|(k, n)| have.get(k).copied().unwrap_or(0) < *n).map(|(k, _)| *k)
}
