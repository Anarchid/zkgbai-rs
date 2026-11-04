//! Mission-driven airplane production and sorties. Policy is independent of
//! engine handles so target selection and procurement can be exercised offline.

mod anti_air;
mod escort;
mod group;
mod live;
mod scenario;
mod shields;
#[cfg(test)]
mod tests;
mod turns;

use crate::util::distance;
use group::{Arms, Blast, GroupStrike};
use shields::Shield;
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
/// A Raven's guided bomb flies 135 elmos/s and homes only weakly: in 37k
/// bombs of our arena combat logs it hit 86-88% of targets moving under 80
/// elmos/s at release, 73% at 80-100 and about 25% above. By top speed:
/// Scorcher 32%, Kodachi 30%, Dagger 10%, Glaive 50%, Pyro 56%, against
/// Rocko 94%, Ravager 91%, Fencer 88%, Reaper 85%. Units this fast or faster
/// are Magpie and Likho work, never Ravens'.
const RAVEN_MAX_TARGET_SPEED: f32 = 90.0;
/// Magpie strafes: one Magpie's load is twelve EMG salvos of 48, so it
/// answers 460 HP of a slow target (with a 20% margin) and 200 of a raider
/// (its spray misses those often, see `firefighters::MAGPIE_ANSWERS`). A
/// target needing more than `STRAFE_WING` Magpies is left alone; at least
/// `QRF_MAGPIES` stay idle at home (firefighters' included) for raids.
const MAGPIE_LOAD: f32 = 460.0;
pub(super) const STRAFE_WING: usize = 4;
pub(super) const QRF_MAGPIES: usize = 3;
fn strafe_magpies(hp: f32, fast: bool) -> usize {
    let answers = if fast { crate::military::firefighters::MAGPIE_ANSWERS } else { MAGPIE_LOAD };
    (hp.max(1.0) / answers).ceil().max(1.0) as usize
}

/// Clumps for group strikes: ground units of this much metal each (heavies
/// such as Grizzly 1900, Goliath 2200 and Crab 1600 included, striders left
/// to single-target strikes), at least two worth this much together. A pair
/// of Grizzlies is one target: five Likhos at one of them burn both (about
/// 2800 metal of damage value), where each alone would need six to kill.
/// Anti-air flying this close to an aircraft covers it (a Trident reaches 750,
/// a Raptor 800): fighters going at it would meet that escort.
const ESCORT_REACH: f32 = 1000.0;
/// A Raptor's laser does 100 damage per second to aircraft (about 14 in
/// practice against a hovering gunship: strafing passes). Against a quarry
/// without counted anti-air or escort, whatever Raptors are ready go (see
/// `Mission::wearable`) where this little ground anti-air is on the way.
pub(super) const WEAR_MAX_AA: f32 = 0.15;
/// Aircraft that hunt ours: anti-air whatever their tooltip says.
pub(super) const FIGHTERS: [&str; 3] = ["planefighter", "planeheavyfighter", "planesupport"];

/// Whether a plan whose full package cannot be filled may go with the
/// Raptors that are ready (see `Mission::wearable`).
fn wears_down(kind: MissionKind, wearable: bool, risk: Risk) -> bool {
    kind == MissionKind::Intercept && wearable && risk.aa <= WEAR_MAX_AA && !risk.intercepted
}

const GROUP_MIN_UNIT_METAL: f32 = 150.0;
const GROUP_MAX_UNIT_METAL: f32 = 2500.0;
const GROUP_MIN_SIZE: usize = 2;
const GROUP_MIN_METAL: f32 = 600.0;

/// A Likho (2000 metal) flies at a single target only for at least this much
/// metal per Likho: the worth a group strike asks of each. A 125-metal Quill
/// or a 35-metal tidal is Raven work or none.
pub(super) const LIKHO_MIN_VALUE: f32 = 250.0;

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
    /// Odin's heavy airframe accepts twice the AA exposure budget. Apply
    /// the same discount during procurement, dispatch and active flight.
    fn survives(self, risk: Risk, health: f32, speed: f32) -> bool {
        let damage = risk.damage(speed) * if self == Self::Odin { 0.5 } else { 1.0 };
        damage.is_finite() && damage < health * 0.55
    }

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
    HeavyAir,
    Interdict,
    Group,
    Heavy,
    Speculative,
    Carpet,
    Wind,
    Thermite,
    Assassinate,
    Opportunistic,
    Static,
    Shield,
    /// Spare Phoenixes on a clump of buildings: a mex among its Solars.
    Outpost,
    /// Spare Magpies on anything that cannot shoot back, away from AA.
    Strafe,
    /// A spare Odin's thermite on a heavy defense (Gauss, Stinger).
    Turret,
}
impl MissionKind {
    fn label(self) -> &'static str {
        match self {
            Self::Heavy => "Priority strike",
            Self::Speculative => "Artillery hunt",
            Self::Carpet => "Carpet bombing",
            Self::Opportunistic => "Opportunistic strike",
            Self::Group => "Group strike",
            Self::Intercept => "Interception",
            Self::HeavyAir => "Heavy air strike",
            Self::Interdict => "Interdiction",
            Self::Wind => "Wind farm strike",
            Self::Thermite => "Thermite strike",
            Self::Assassinate => "Assassination",
            Self::Static => "Static strike",
            Self::Shield => "Shield drop",
            Self::Outpost => "Outpost burn",
            Self::Strafe => "Magpie strafe",
            Self::Turret => "Turret burn",
        }
    }

    fn primary(self) -> bool {
        !matches!(
            self,
            Self::Assassinate
                | Self::Opportunistic
                | Self::Static
                | Self::Shield
                | Self::Outpost
                | Self::Turret
                | Self::Strafe
        )
    }
    fn priority(self) -> f32 {
        match self {
            Self::Intercept => 10.0,
            Self::Interdict => 9.0,
            Self::Heavy | Self::HeavyAir | Self::Carpet => 8.0,
            Self::Wind => 7.0,
            Self::Thermite => 7.0,
            Self::Group => 6.0,
            Self::Speculative => 5.0,
            Self::Assassinate | Self::Opportunistic => 3.0,
            Self::Shield | Self::Turret | Self::Strafe => 2.0,
            Self::Outpost => 1.5,
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
    heavy_air: bool,
    submerged: bool,
    mex: bool,
    static_: bool,
    heavy: bool,
    raider: bool,
    wind: bool,
    singu: bool,
    /// A factory or strider hub: what the enemy's army comes out of.
    factory: bool,
    /// A nanoframe in sight, and how far it is built: it cannot move yet.
    frame: bool,
    progress: f32,
    home: bool,
    fighting: bool,
    /// Its shield's radius and charge (full unless in sight).
    shield: Option<(f32, f32)>,
    /// Damage taken per point dealt while armored (closed Solars, Gausses: 1/3).
    armor: f32,
    /// It has a weapon: a building with one is a defense.
    armed: bool,
    /// Top speed (elmos/s), 0 while unidentified.
    speed: f32,
    /// It can shoot at aircraft (fighters included).
    aa: bool,
    /// An armed aircraft over our or allied ground units or buildings, in
    /// reach of them: an interception wherever it is (`airmenace`).
    menacing: bool,
}

impl Contact {
    fn strike_value(&self) -> f32 {
        // Destroying extraction denies recurring income and a contested spot.
        // Keep this separate from raw metal for eligibility/cost thresholds.
        self.value * if self.mex { 4.0 } else { 1.0 }
    }
}

/// A factory is worth more than its metal: it is where the army comes from,
/// and losing it costs the enemy the rebuild time too. Their last one, more.
const FACTORY_WORTH: f32 = 3.0;
const LAST_FACTORY_WORTH: f32 = 4.0;

/// An unfinished strider (or anything else mobile at least this dear) sits
/// still until it is done. Its worth is the metal already in it, twice: it
/// was paid for, and it cannot run from the burn.
const FRAME_MIN_COST: f32 = 2000.0;
const FRAME_WORTH: f32 = 2.0;

/// Thermite value: superweapon-class statics of at least 1000 metal
/// (Singularities with a bonus), every factory, priced above cost, and frames
/// of expensive mobile units. Submerged ones too: thermite burns underwater.
fn thermite_value(e: &Contact, factories: usize) -> Option<f32> {
    if e.factory {
        Some(e.value * if factories <= 1 { LAST_FACTORY_WORTH } else { FACTORY_WORTH })
    } else if e.static_ {
        (e.value >= 1000.0).then_some(e.value + if e.singu { 4000.0 } else { 0.0 })
    } else if e.frame && e.value >= FRAME_MIN_COST {
        Some(e.value * e.progress.clamp(0.0, 1.0) * FRAME_WORTH)
    } else {
        None
    }
}

/// A heavy defense worth a spare Odin's thermite (Gauss 400, Stinger 450,
/// Sunlance 700, Thresher 450): an armed building of this much metal that is
/// not already a thermite target. Stardusts, Faradays and Lotuses are left to
/// Ravens.
const TURRET_MIN_COST: f32 = 400.0;
fn heavy_defense(e: &Contact) -> bool {
    e.static_ && e.armed && e.shield.is_none() && !e.factory && e.value >= TURRET_MIN_COST
}

/// Buildings an outpost burn counts: the eco and light porc of an expansion
/// (mex 85, Solar 70, wind 35, Lotus 90, pylon 200), not factories, dear
/// statics (thermite) or shields.
const OUTPOST_MAX_COST: f32 = 300.0;
/// An outpost: at least this many such buildings within this reach of a seed.
const OUTPOST_MIN: usize = 3;
const OUTPOST_REACH: f32 = 260.0;
fn outpost_member(c: &Contact) -> bool {
    c.static_
        && c.known
        && !c.air
        && !c.submerged
        && !c.factory
        && !c.singu
        && c.shield.is_none()
        && c.value <= OUTPOST_MAX_COST
}

#[derive(Clone)]
struct Inference {
    id: i32,
    def: String,
    pos: Vec3,
    uncertainty: f32,
    last: i32,
    /// The last impact's origin and unit bearing toward the shooter, and the
    /// weapon's range: a carpet lays its lines along that bearing.
    bearing: Option<(Vec3, Vec3)>,
    range: f32,
}

#[derive(Clone)]
struct Mission {
    kind: MissionKind,
    submerged: bool,
    target: i32,
    pos: Vec3,
    hp: f32,
    value: f32,
    group: Option<GroupStrike>,
    inference: Option<Inference>,
    /// All targets covered by this job, also used to avoid overlapping sorties.
    targets: Vec<i32>,
    seen: i32,
    deep: bool,
    /// Enemy shields over the targets: bombs they take count for nothing.
    shields: Vec<Shield>,
    /// Too fast for a Raven's guided bomb (`RAVEN_MAX_TARGET_SPEED`).
    fast: bool,
    /// An identified aircraft carrying no anti-air we count (no AA or flex-AA
    /// weapon, not a fighter), with none flying within `ESCORT_REACH` of it (a
    /// Revenant, a Gnat): Raptors keep firing at it until it dies or flees,
    /// so they need not kill it in one pass, and fewer than the full package
    /// may go (`raptorwear`). Not harmless: a Revenant's rocket salvo (210 a
    /// rocket) hit strafing Raptors with 10 of 142 shots in the staged runs
    /// and killed one already hurt Raptor, while Raptors did ~19.8k damage
    /// to Revenants; the 50%-health abort sends hurt ones home.
    wearable: bool,
}
impl Mission {
    fn key(&self) -> (MissionKind, i32) {
        (self.kind, self.target)
    }
    /// Bombs of this shield damage and blast radius the shields over the aim
    /// take before one gets through.
    fn soak(&self, damage: f32, radius: f32) -> usize {
        shields::soak(&self.shields, self.pos, damage, radius)
    }
    /// A charged bubble over the target eats an Odin's thermite whole.
    fn thermite_blocked(&self) -> bool {
        self.soak(shields::THERMITE_DAMAGE, shields::THERMITE_RADIUS) > 0
    }
    fn score(&self) -> f32 {
        let reward = if matches!(self.kind, MissionKind::Heavy | MissionKind::Opportunistic | MissionKind::Assassinate)
        {
            self.value / self.hp.max(1.0) * 3000.0
        } else if self.kind == MissionKind::HeavyAir {
            self.damage_value(&[(Plane::Likho, 1)])
        } else if self.kind == MissionKind::Strafe {
            // Metal per Magpie flown.
            self.value / strafe_magpies(self.hp, self.fast) as f32
        } else if let Some(per_plane) = self.group_per_plane() {
            // Groups and outposts: the best value per aircraft, after the
            // bombs any shields over them take (one aircraft's pass without).
            per_plane
        } else {
            self.value
        };
        // Keep inbound-air defense urgent; offensive mission value must not
        // disappear beneath a large flat tier bonus.
        let reward = reward.clamp(0.0, 9000.0);
        if self.kind == MissionKind::Intercept { 100000.0 + reward } else { self.kind.priority() * reward }
    }
}

/// See `Mission::wearable`. Unidentified contacts never are: what they
/// carry is unknown.
fn wearable(e: &Contact, contacts: &[Contact], frame: i32) -> bool {
    e.air
        && e.known
        && !e.aa
        && !contacts
            .iter()
            .any(|c| c.id != e.id && c.air && c.aa && frame - c.seen <= FRESH && distance(c.pos, e.pos) < ESCORT_REACH)
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
    let factories = contacts.iter().filter(|c| c.factory && c.static_ && c.known).count();
    let carriers = shields::carriers(contacts);
    for e in contacts {
        let fresh = e.live && frame - e.seen <= FRESH;
        if !e.static_ && !fresh {
            continue;
        }
        let here = shields::over(&carriers, &[e.pos]);
        let wearable = wearable(e, contacts, frame);
        let mut add = |kind, targets: Vec<i32>, hp, value| {
            jobs.push(Mission {
                kind,
                submerged: e.submerged,
                group: None,
                inference: None,
                target: e.id,
                pos: e.pos,
                hp,
                value,
                targets,
                seen: frame,
                deep: e.singu || distance(e.pos, home) >= 3000.0,
                shields: here.clone(),
                fast: !e.static_ && e.speed >= RAVEN_MAX_TARGET_SPEED,
                wearable,
            })
        };
        if e.air {
            if fresh && e.known && e.heavy_air {
                add(MissionKind::HeavyAir, vec![e.id], e.hp, e.value);
            }
            if fresh && (e.menacing || inbound(e.pos, e.vel, home, e.home)) {
                add(MissionKind::Intercept, vec![e.id], e.hp.max(1000.0), e.value);
            }
            continue;
        }
        if !e.known {
            continue;
        }
        if e.static_ {
            if let Some(value) = thermite_value(e, factories) {
                add(MissionKind::Thermite, vec![e.id], e.hp, value);
            } else if heavy_defense(e) {
                // Thermite against the hit points behind its armor (a closed
                // Gauss takes a third): one Odin still burns any of them.
                add(MissionKind::Turret, vec![e.id], e.hp / e.armor.clamp(0.05, 1.0), e.value);
            }
            add(MissionKind::Static, vec![e.id], e.hp, e.strike_value());
        } else {
            if let Some(value) = thermite_value(e, factories) {
                add(MissionKind::Thermite, vec![e.id], e.hp, value);
            }
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
            if e.raider && e.home && !e.submerged {
                add(MissionKind::Interdict, vec![e.id], e.hp, e.value);
            }
            if e.fighting && !e.submerged {
                add(MissionKind::Shield, vec![e.id], e.hp, e.value);
            }
        }
        if fresh
            && !e.aa
            && !e.submerged
            && strafe_magpies(e.hp, !e.static_ && e.speed >= RAVEN_MAX_TARGET_SPEED) <= STRAFE_WING
        {
            add(MissionKind::Strafe, vec![e.id], e.hp, e.strike_value());
        }
        let medium = |c: &Contact| {
            !c.air
                && !c.submerged
                && !c.static_
                && c.known
                && c.live
                && frame - c.seen <= FRESH
                && (GROUP_MIN_UNIT_METAL..=GROUP_MAX_UNIT_METAL).contains(&c.value)
        };
        if (e.wind || medium(e)) && !clustered.contains(&e.id) {
            let group: Vec<_> = contacts
                .iter()
                .filter(|c| distance(c.pos, e.pos) < 260.0 && if e.wind { c.wind } else { medium(c) })
                .collect();
            let value = group.iter().map(|c| c.value).sum::<f32>();
            if (e.wind && group.len() >= 5) || (!e.wind && group.len() >= GROUP_MIN_SIZE && value >= GROUP_MIN_METAL) {
                let targets = group.iter().map(|c| c.id).collect::<Vec<_>>();
                clustered.extend(targets.iter().copied());
                add(
                    if e.wind { MissionKind::Wind } else { MissionKind::Group },
                    targets,
                    group.iter().map(|c| c.hp).sum(),
                    value,
                );
                if !e.wind {
                    let points: Vec<_> = group.iter().map(|c| c.pos).collect();
                    let job = jobs.last_mut().unwrap();
                    job.shields = shields::over(&carriers, &points);
                    job.group = Some(GroupStrike::new(&group, job.shields.clone()));
                    job.aim_group(Arms::default());
                }
            }
        }
    }
    jobs.extend(outposts(contacts, &carriers, home, frame));
    jobs.sort_by(|a, b| {
        b.kind
            .primary()
            .cmp(&a.kind.primary())
            .then_with(|| b.score().total_cmp(&a.score()))
            .then_with(|| a.key().cmp(&b.key()))
    });
    jobs.truncate(128);
    jobs
}

/// Clumps of cheap buildings for spare Phoenixes: a mex among its Solars,
/// turbines and light porc. Mexes seed first, so an expansion is taken about
/// its mex; each building joins one outpost.
fn outposts(contacts: &[Contact], carriers: &[Shield], home: Vec3, frame: i32) -> Vec<Mission> {
    let members: Vec<&Contact> = contacts.iter().filter(|c| outpost_member(c)).collect();
    let mut taken = std::collections::BTreeSet::new();
    let mut jobs = Vec::new();
    for seed in members.iter().filter(|c| c.mex).chain(members.iter().filter(|c| !c.mex)) {
        if taken.contains(&seed.id) {
            continue;
        }
        let group: Vec<&Contact> = members
            .iter()
            .filter(|c| !taken.contains(&c.id) && distance(c.pos, seed.pos) < OUTPOST_REACH)
            .copied()
            .collect();
        if group.len() < OUTPOST_MIN {
            continue;
        }
        taken.extend(group.iter().map(|c| c.id));
        let points: Vec<_> = group.iter().map(|c| c.pos).collect();
        let shields = shields::over(carriers, &points);
        let mut job = Mission {
            kind: MissionKind::Outpost,
            submerged: false,
            group: Some(GroupStrike::outpost(&group, shields.clone(), home)),
            inference: None,
            target: seed.id,
            pos: seed.pos,
            hp: group.iter().map(|c| c.hp).sum(),
            value: group.iter().map(|c| c.strike_value()).sum(),
            targets: group.iter().map(|c| c.id).collect(),
            seen: frame,
            deep: distance(seed.pos, home) >= 3000.0,
            shields,
            fast: false,
            wearable: false,
        };
        job.aim_group(Arms::default());
        jobs.push(job);
    }
    jobs
}

/// Incoming fire is normalized to 500 DPS; exposure integrates it over
/// 64-elmo cells. Include approach/egress, a 1500-elmo release/turn allowance
/// flown at this aircraft's speed, and ready volleys from encountered weapons.
#[derive(Clone, Copy)]
struct Risk {
    aa: f32,
    exposure: f32,
    intercepted: bool,
    burst: f32,
}
impl Risk {
    fn damage(self, speed: f32) -> f32 {
        (2.0 * self.exposure * 64.0 + 1500.0 * self.aa) * 500.0 / speed.max(1.0) + self.burst
    }
    fn survives(self, health: f32, speed: f32) -> bool {
        self.damage(speed).is_finite() && self.damage(speed) < health * 0.55
    }
}

/// Retrace the outbound corridor, including the exact home position. The
/// threat sampler checks this explicit egress independently of ingress.
fn return_route(home: Vec3, outbound: &[Vec3]) -> Vec<Vec3> {
    if outbound.is_empty() {
        return Vec::new();
    }
    outbound.iter().rev().skip(1).copied().chain(std::iter::once(home)).collect()
}

fn route_risk(
    from: Vec3,
    route: &[Vec3],
    mut threat: impl FnMut(Vec3) -> f32,
    intercepted: impl Fn(Vec3) -> bool,
) -> Risk {
    let Some(&end) = route.last() else {
        return Risk { aa: 0.0, exposure: f32::INFINITY, intercepted: false, burst: 0.0 };
    };
    let mut previous = from;
    let _ = threat(from); // account for a ready volley at the start of a leg
    let mut exposure = 0.0;
    let mut blocked = intercepted(from);
    for &next in route {
        let leg = distance(previous, next);
        let steps = (leg / 32.0).ceil().max(1.0) as usize;
        for step in 0..steps {
            let p = previous + (next - previous) * ((step as f32 + 0.5) / steps as f32);
            exposure += threat(p) * leg / steps as f32 / 64.0;
            blocked |= intercepted(p);
        }
        blocked |= intercepted(next);
        previous = next;
    }
    Risk { aa: threat(end), exposure, intercepted: blocked, burst: 0.0 }
}

fn round_trip(ingress: Risk, egress: Risk) -> Risk {
    Risk {
        aa: ingress.aa.max(egress.aa),
        exposure: (ingress.exposure + egress.exposure) * 0.5,
        intercepted: ingress.intercepted || egress.intercepted,
        burst: ingress.burst + egress.burst,
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
    // Implosion bombs the shields over the target take come on top (Likho,
    // 2000 damage over 96 elmos).
    let likhos = salvo(2000.0) + job.soak(2000.0, 96.0);
    let raven = job.ravens(raven_damage);
    let clear = risk.aa <= 0.02 && !risk.intercepted;
    let very_light = risk.aa <= 0.15 && !risk.intercepted;
    // Likho/Odin tolerance comes from each aircraft's round-trip survival
    // check, not a Raven-derived local-AA band. Packages remain provisional.
    let unopposed = !risk.intercepted;
    let choices = match job.kind {
        HeavyAir if unopposed => job.heavy_air_packages(),
        // Raptors do the killing; a Magpie's slowing EMG assists when one is
        // ready, and is never bought or waited for.
        Intercept if risk.aa <= 0.4 => {
            let raptors = count(job.hp, 700.0).max(2);
            vec![vec![(Magpie, 1), (Raptor, raptors)], vec![(Raptor, raptors)]]
        }
        // Magpies by the raider health one answers (a Flea takes one, a
        // Glaive two). Launch and procurement take the cheaper of the two
        // (see `Ai::airstrike_cheapest_first`).
        Interdict if clear => {
            let magpies = count(job.hp, crate::military::firefighters::MAGPIE_ANSWERS);
            vec![vec![(Magpie, magpies)], vec![(Phoenix, count(job.hp, 1000.0))]]
        }
        Group if clear => job.group_packages(&[Phoenix, Likho]),
        Group if unopposed => job.group_packages(&[Likho]),
        Outpost if clear => job.group_packages(&[Phoenix]),
        Strafe if clear => {
            let n = strafe_magpies(job.hp, job.fast);
            if n <= STRAFE_WING { vec![vec![(Magpie, n)]] } else { vec![] }
        }
        Wind if clear => vec![vec![(Phoenix, job.targets.len().div_ceil(12))]],
        Speculative if very_light => vec![vec![(Likho, salvo(2000.0))]],
        Carpet if very_light => job.carpet_packages(),
        Heavy | Opportunistic if unopposed => {
            let mut options = Vec::new();
            if very_light && !job.fast {
                options.push(vec![(Raven, raven)]);
            }
            if job.value >= LIKHO_MIN_VALUE * likhos as f32 {
                options.push(vec![(Likho, likhos)]);
            }
            options
        }
        Thermite | Turret if !risk.intercepted && !job.thermite_blocked() => {
            vec![vec![(Odin, count(job.hp, 14000.0))]]
        }
        Assassinate | Static if very_light && !job.fast => vec![vec![(Raven, raven)]],
        Shield if !risk.intercepted => vec![vec![(Odin, 1)]],
        _ => vec![],
    };
    choices
        .into_iter()
        .filter(|p| {
            p.iter().map(|(_, n)| n).sum::<usize>() <= MAX_WING
                && p.iter().all(|(k, _)| can_hit_submerged(*k, job.kind, job.submerged))
        })
        .collect()
}

/// Raven bombs reach submerged targets, and so does Odin thermite (a water
/// weapon that burns where it lands, on factories as on turrets); nothing
/// else does.
fn can_hit_submerged(plane: Plane, kind: MissionKind, submerged: bool) -> bool {
    !submerged
        || plane == Plane::Raven
        || (plane == Plane::Odin && matches!(kind, MissionKind::Thermite | MissionKind::Turret))
}

impl Mission {
    fn ravens(&self, damage: f32) -> usize {
        // Bombs the shields over the target take come first: a Felon's own
        // bubble takes two, an Aspis over it four more.
        let soaked = self.soak(damage, shields::RAVEN_RADIUS);
        soaked
            + if self.kind == MissionKind::Static {
                // Stationary secondary targets need the lethal payload, no spare.
                (self.hp / damage.max(1.0)).ceil().max(1.0) as usize
            } else {
                let salvo = (f64::from(self.hp) * 1.2 / f64::from(damage.max(1.0))).ceil().max(1.0) as usize;
                salvo + usize::from(matches!(self.kind, MissionKind::Heavy | MissionKind::Assassinate))
            }
    }
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
