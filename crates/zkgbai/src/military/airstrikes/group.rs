//! Value of a partial bombing pass, with no requirement to kill the clump.
//!
//! Mobile groups are worth `metal * min(damage / HP, 1)` per victim. Outposts
//! (clumps of buildings) are worth each building's strike value when the pass
//! destroys it, and only a quarter of the damaged share when it does not:
//! constructors repair buildings. Bombs that enemy shields take (see
//! `shields`) deal nothing, so a package must first spend what the bubbles
//! over the aim can absorb; packages are sized for that (`group_packages`).
//!
//! A clump's metal is worth more under bubbles. Ground fire is small shots
//! every bubble takes, so our army must chew through the shields before the
//! hit points (a Thug is 960 HP behind 1200 of bubble); an implosion bomb goes
//! through any bubble short of an Aspis. A bomb that kills it spares our army
//! that grind: each victim's metal counts `(HP + bubble charge over it) / HP`
//! times, each bubble's charge shared by the victims under it, at most three
//! times (`disable=shieldworth` counts plain metal).
use super::shields::{self, Shield};
use super::*;

/// Moving targets and estimate error: count 80% of the computed damage.
const RESERVE: f32 = 0.8;

/// Recoil's splash falloff from the blast centre.
fn falloff(radius: f32, edge: f32, range: f32) -> f32 {
    if range >= radius {
        return 0.0;
    }
    (radius - range) / (radius - range * edge.clamp(0.0, 1.0)).max(0.001)
}

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
        // Center distance, conservatively instead of distance to the
        // collision volume, with the moving-target reserve.
        self.damage * RESERVE * falloff(self.radius, self.edge, range)
    }
}

/// A Phoenix salvo (bomberriot `NAPALM`: 18 bombs of 40, blast radius 160,
/// edge 0.7). The engine lays it along the flight, about 390 elmos long and
/// centred on the aim (see `carpet`).
#[derive(Clone, Copy)]
pub(super) struct Napalm {
    pub bombs: usize,
    pub damage: f32,
    pub radius: f32,
    pub edge: f32,
}
impl Default for Napalm {
    fn default() -> Self {
        // Fallback only; live planning reads the installed Phoenix weapon.
        Self { bombs: 18, damage: 40.0, radius: 160.0, edge: 0.7 }
    }
}
const LINE: f32 = 390.0;
/// The fire under napalm: 0.5 damage a frame for 70-100% of the weapon's
/// 360-frame `burntime` (`unit_is_on_fire.lua`), once per victim however many
/// bombs reach it.
const BURN: f32 = 0.5 * 360.0 * 0.85;
/// A Phoenix's aggregate damage to a moving clump, spread by HP (heuristic).
const PHOENIX_CLUMP_HP: f32 = 1800.0;

impl Napalm {
    /// What one salvo takes from the bubble it falls on.
    fn payload(self) -> f32 {
        self.bombs as f32 * self.damage
    }
    /// One line's bomb damage to a victim `along` the flight from the aim and
    /// `across` it, and whether any bomb reaches it (setting it on fire).
    fn line(self, along: f32, across: f32) -> (f32, bool) {
        let n = self.bombs.max(1);
        let step = if n > 1 { LINE / (n - 1) as f32 } else { 0.0 };
        let start = if n > 1 { -LINE * 0.5 } else { 0.0 };
        let (mut sum, mut hit) = (0.0, false);
        for i in 0..n {
            let range = (along - (start + i as f32 * step)).hypot(across);
            if range < self.radius {
                hit = true;
                sum += self.damage * falloff(self.radius, self.edge, range);
            }
        }
        (sum, hit)
    }
}

/// Unit vectors along the final approach from `from` to `aim` and across it.
fn axes(from: Vec3, aim: Vec3) -> (Vec3, Vec3) {
    let (dx, dz) = (aim.x - from.x, aim.z - from.z);
    let len = dx.hypot(dz);
    let along = if len > 1.0 { Vec3::xz(dx / len, dz / len) } else { Vec3::xz(1.0, 0.0) };
    (along, Vec3::xz(-along.z, along.x))
}

/// One of our own or an ally's ground units or buildings near a drop.
#[derive(Clone, Copy, Debug)]
pub(super) struct Bystander {
    pub pos: Vec3,
    pub hp: f32,
    /// Metal, a mex four times its metal (as for enemy buildings).
    pub worth: f32,
    pub armor: f32,
    pub building: bool,
}

/// What a package's bombs at `aim`, flown in from `from`, take from
/// bystanders: priced like enemy victims (units by the damaged share of
/// their metal, buildings whole when destroyed and a quarter of the damaged
/// share otherwise), with no reserve: our losses are counted in full. Every
/// bomb is assumed to land; implosion and napalm are the blasts that reach
/// past the target (Raven bombs burst over 16 elmos, thermite over 20).
pub(super) fn collateral(
    arms: Arms,
    aim: Vec3,
    from: Vec3,
    package: &[(Plane, usize)],
    bystanders: &[Bystander],
) -> f32 {
    let Arms { likho, napalm, .. } = arms;
    let (along, across) = axes(from, aim);
    let count = |plane| package.iter().filter(|(p, _)| *p == plane).map(|(_, n)| *n).sum::<usize>() as f32;
    let (likhos, phoenixes) = (count(Plane::Likho), count(Plane::Phoenix));
    bystanders
        .iter()
        .map(|b| {
            let mut d = likhos * likho.damage * falloff(likho.radius, likho.edge, distance(aim, b.pos));
            if phoenixes > 0.0 {
                let (dx, dz) = (b.pos.x - aim.x, b.pos.z - aim.z);
                let (bombs, hit) = napalm.line(dx * along.x + dz * along.z, dx * across.x + dz * across.z);
                d += phoenixes * bombs + if hit { BURN } else { 0.0 };
            }
            let d = d * b.armor;
            let health = b.hp.max(1.0);
            if b.building {
                b.worth * if d >= health { 1.0 } else { PARTIAL_STATIC * d / health }
            } else {
                b.worth * (d / health).clamp(0.0, 1.0)
            }
        })
        .sum()
}
/// How far from the aim a bystander can be hurt: half a napalm line plus a
/// bomb's blast (about 355 elmos), well past an implosion bomb's 96.
pub(super) fn collateral_reach(arms: Arms) -> f32 {
    (LINE * 0.5 + arms.napalm.radius).max(arms.likho.radius)
}

/// The bombers' arms, read from the installed defs.
#[derive(Clone, Copy, Default)]
pub(super) struct Arms {
    pub likho: Blast,
    pub napalm: Napalm,
    /// `disable=shieldworth`: a clump is worth its plain metal.
    pub plain_worth: bool,
}

/// A shielded victim's metal counts at most this many times.
const MAX_WORTH: f32 = 3.0;

#[derive(Clone, Copy)]
struct Victim {
    id: i32,
    pos: Vec3,
    hp: f32,
    /// Strike value: metal, a mex four times its metal.
    metal: f32,
    /// Damage taken per point dealt (closed Solars and Gausses take a third).
    armor: f32,
    /// How many times its metal counts for a bomb: what ground fire must
    /// chew through (its HP and its share of the bubbles over it) per HP.
    worth: f32,
}

/// A building is worth this share of its damaged fraction when it survives.
const PARTIAL_STATIC: f32 = 0.25;
/// Outposts get at most this many Phoenixes at once.
pub(super) const OUTPOST_WING: usize = 6;

#[derive(Clone)]
pub(super) struct GroupStrike {
    targets: Vec<Victim>,
    shields: Vec<Shield>,
    arms: Arms,
    /// Buildings: burnt along the napalm line, valued by kills.
    statics: bool,
    /// Where the bombers come from: the line runs from here over the aim.
    from: Vec3,
    /// Best damage value per aircraft of any package size, for ranking.
    per_plane: f32,
}
impl GroupStrike {
    pub fn new(contacts: &[&Contact], shields: Vec<Shield>) -> Self {
        // Every bubble stops ground fire, whatever its size; its charge is
        // shared by the victims under it.
        let mut bubble = vec![0.0f32; contacts.len()];
        for s in &shields {
            let under: Vec<_> = (0..contacts.len()).filter(|&i| s.covers(contacts[i].pos)).collect();
            for &i in &under {
                bubble[i] += s.charge / under.len() as f32;
            }
        }
        Self {
            targets: contacts
                .iter()
                .zip(bubble)
                .map(|(c, b)| Victim {
                    id: c.id,
                    pos: c.pos,
                    hp: c.hp,
                    metal: c.value,
                    armor: 1.0,
                    worth: ((c.hp.max(1.0) + b) / c.hp.max(1.0)).min(MAX_WORTH),
                })
                .collect(),
            shields,
            arms: Arms::default(),
            statics: false,
            from: Vec3::ZERO,
            per_plane: 0.0,
        }
        .ranked()
    }
    /// A clump of buildings, approached from `from` (our home).
    pub fn outpost(contacts: &[&Contact], shields: Vec<Shield>, from: Vec3) -> Self {
        Self {
            targets: contacts
                .iter()
                .map(|c| Victim {
                    id: c.id,
                    pos: c.pos,
                    hp: c.hp,
                    metal: c.strike_value(),
                    armor: c.armor.clamp(0.05, 1.0),
                    worth: 1.0,
                })
                .collect(),
            statics: true,
            from,
            ..Self::new(&[], shields)
        }
        .ranked()
    }
    /// Rank it at its first target until `aim_group` picks the aim.
    fn ranked(mut self) -> Self {
        self.per_plane = self.targets.first().map_or(0.0, |t| self.per_plane_at(t.pos));
        self
    }
    fn wing(&self) -> usize {
        if self.statics { OUTPOST_WING } else { MAX_WING }
    }
    fn planes(&self) -> &'static [Plane] {
        if self.statics { &[Plane::Phoenix] } else { &[Plane::Likho, Plane::Phoenix] }
    }
    fn axes(&self, aim: Vec3) -> (Vec3, Vec3) {
        axes(self.from, aim)
    }
    fn value(&self, aim: Vec3, package: &[(Plane, usize)]) -> f32 {
        let Arms { likho, napalm, .. } = self.arms;
        let mut charge: Vec<f32> = self.shields.iter().map(|s| s.charge).collect();
        let mut damage = vec![0.0f32; self.targets.len()];
        let mut burning = vec![false; self.targets.len()];
        let hp = self.targets.iter().map(|t| t.hp.max(1.0)).sum::<f32>().max(1.0);
        let (along, across) = self.axes(aim);
        for &(plane, n) in package {
            for _ in 0..n {
                match plane {
                    Plane::Likho => {
                        if shields::stop(&mut charge, &self.shields, aim, likho.damage, likho.radius) {
                            continue;
                        }
                        for (d, t) in damage.iter_mut().zip(&self.targets) {
                            *d += likho.damage_at(distance(aim, t.pos)) * t.armor;
                        }
                    }
                    Plane::Phoenix => {
                        // Bombs over a charged bubble wide enough feed it, not
                        // the fire under it.
                        let mut drain = vec![0.0f32; self.shields.len()];
                        for (i, t) in self.targets.iter().enumerate() {
                            let guard = shields::guard(&charge, &self.shields, t.pos, napalm.damage, napalm.radius);
                            if self.statics {
                                let (dx, dz) = (t.pos.x - aim.x, t.pos.z - aim.z);
                                let (bombs, hit) =
                                    napalm.line(dx * along.x + dz * along.z, dx * across.x + dz * across.z);
                                if !hit {
                                    continue;
                                }
                                match guard {
                                    Some(j) => drain[j] = napalm.payload(),
                                    None => {
                                        damage[i] += bombs * RESERVE * t.armor;
                                        burning[i] = true;
                                    }
                                }
                            } else {
                                // The moving clump keeps the old aggregate
                                // budget, spread by HP.
                                let share = t.hp.max(1.0) / hp;
                                match guard {
                                    Some(j) => drain[j] += napalm.payload() * share,
                                    None => damage[i] += PHOENIX_CLUMP_HP * share,
                                }
                            }
                        }
                        for (c, d) in charge.iter_mut().zip(drain) {
                            *c -= d;
                        }
                    }
                    _ => {}
                }
            }
        }
        self.targets
            .iter()
            .zip(damage)
            .zip(burning)
            .map(|((t, mut d), burnt)| {
                let health = t.hp.max(1.0);
                if !self.statics {
                    let worth = if self.arms.plain_worth { 1.0 } else { t.worth };
                    return t.metal * worth * (d / health).clamp(0.0, 1.0);
                }
                if burnt {
                    d += BURN * RESERVE * t.armor;
                }
                t.metal * if d >= health { 1.0 } else { PARTIAL_STATIC * d / health }
            })
            .sum()
    }
    /// Best value per aircraft at `aim`, over every package size.
    fn per_plane_at(&self, aim: Vec3) -> f32 {
        let mut best = 0.0f32;
        for &plane in self.planes() {
            for n in 1..=self.wing() {
                best = best.max(self.value(aim, &[(plane, n)]) / n as f32);
            }
        }
        best
    }
    /// How good `aim` is: for a clump, one Likho bomb landing after the
    /// bubbles over it are spent, per Likho flown; for buildings, the best
    /// Phoenix package per Phoenix.
    fn aim_score(&self, aim: Vec3) -> f32 {
        if self.statics {
            return self.per_plane_at(aim);
        }
        let likho = self.arms.likho;
        let n = shields::soak(&self.shields, aim, likho.damage, likho.radius) + 1;
        self.value(aim, &[(Plane::Likho, n)]) / n as f32
    }
}
impl Mission {
    pub(super) fn aim_group(&mut self, arms: Arms) {
        let Some(group) = &mut self.group else { return };
        group.arms = arms;
        let mut aims: Vec<_> = group.targets.iter().map(|t| (t.id, t.pos)).collect();
        if group.statics && !group.targets.is_empty() {
            // A line through the middle of an outpost may reach more of it
            // than one through any single building.
            let n = group.targets.len() as f32;
            let mid = group.targets.iter().fold(Vec3::ZERO, |s, t| s + t.pos * (1.0 / n));
            if let Some(t) = group.targets.iter().min_by(|a, b| distance(a.pos, mid).total_cmp(&distance(b.pos, mid))) {
                aims.push((t.id, mid));
            }
        }
        let best = aims
            .into_iter()
            .map(|(id, pos)| (id, pos, group.aim_score(pos)))
            .max_by(|a, b| a.2.total_cmp(&b.2).then_with(|| b.0.cmp(&a.0)));
        if let Some((id, pos, _)) = best {
            self.target = id;
            self.pos = pos;
            group.per_plane = group.per_plane_at(pos);
        }
    }
    /// Ranking value of a group or outpost job: its best value per aircraft.
    pub(super) fn group_per_plane(&self) -> Option<f32> {
        self.group.as_ref().map(|g| g.per_plane)
    }
    pub(super) fn damage_value(&self, package: &[(Plane, usize)]) -> f32 {
        if self.kind == MissionKind::HeavyAir {
            let damage =
                package.iter().filter(|(p, _)| *p == Plane::Likho).map(|(_, n)| *n as f32 * 1600.0).sum::<f32>();
            return self.value * (damage / self.hp.max(1.0)).clamp(0.0, 1.0);
        }
        self.group.as_ref().map_or(self.value, |g| g.value(self.pos, package))
    }
    pub(super) fn group_packages(&self, planes: &[Plane]) -> Vec<Vec<(Plane, usize)>> {
        let Some(group) = &self.group else { return Vec::new() };
        let mut packages = Vec::new();
        for &plane in planes {
            // Every aircraft must remove worthwhile value, on average over
            // the aircraft added since the last worthwhile package: the
            // bombs a shield takes pay off only with the ones that follow.
            // This is a sortie threshold, not a demand to trade the airframe.
            let minimum = if plane == Plane::Likho { 250.0 } else { 150.0 };
            let (mut base, mut previous) = (0, 0.0);
            let start = packages.len();
            for n in 1..=group.wing() {
                let package = vec![(plane, n)];
                let value = self.damage_value(&package);
                if value - previous >= minimum * (n - base) as f32 {
                    (base, previous) = (n, value);
                    packages.push(package);
                }
            }
            // Use more ready bombers when their marginal damage is useful,
            // while keeping the smaller passes valid for launch/procurement.
            packages[start..].reverse();
        }
        packages
    }
}
