//! Anti-air we do not see, from what it does to our aircraft. Of every hit
//! on one of our units the engine tells the weapon and a bearing towards
//! where it came from: across the ground it points at the shooter within a
//! few degrees, even for homing missiles, seen or not; its slope is less
//! sure (hits come in shallow, so a small error in it is a long way on the
//! ground).
//!
//! Hits by one weapon type whose bearings pass close to one place make a
//! contact: an installation where only static defenses carry the weapon (a
//! Chainsaw, an Artemis), or else a mobile group, which may have moved
//! since and counts less the staler it is. A contact stands where its
//! bearings cross (least squares, once they come from far enough apart), or
//! else where its bearings, followed down, reach the shooter's height above
//! the ground. The most hits a contact dealt within a few of its weapon's
//! reload times bound from below how many fire there. Contacts paint
//! anti-air threat as the units would (see `war_paint_threat_map`), stand in
//! for them in who holds which region (see `region_presence`), and go when
//! the real units are seen there, when a look at the place finds nothing,
//! or (mobile ones) when old.

use std::collections::BTreeMap;

use springai::{Unit, Vec3, WeaponDef};

use crate::Ai;
use crate::military::wrappers::weapon_power;
use crate::util::distance;

/// A bearing passes a contact if it comes this close to it (elmos, or this
/// share of the distance along it, whichever is more); a mobile group's
/// spread and drift more.
const STATIC_PASS: f32 = 200.0;
const MOBILE_PASS: f32 = 500.0;
const PASS_SHARE: f32 = 0.15;

/// A bearing is followed this much past the weapon's range (it is the 3D
/// distance), in steps this long, elmos; it must point this far down.
const RANGE_SLACK: f32 = 1.3;
const STEP: f32 = 16.0;
const MIN_DOWN: f32 = 0.01;

/// Bearings cross well enough once they come from directions this far
/// apart (the sine of the angle between the outermost).
const CROSSING: f32 = 0.2;

/// Hits are counted over this many reload times.
const RELOADS: i32 = 4;

/// Bearings and hit times kept per contact (a mobile group's latest only).
const RAYS: usize = 24;
const MOBILE_RAYS: usize = 6;
const HITS: usize = 64;

/// Mobile contacts last this long since their last hit, frames; static
/// ones until seen or refuted (and at most this long).
const MOBILE_MEMORY: i32 = 30 * 120;
const STATIC_MEMORY: i32 = 30 * 900;

/// A contact is resolved by a seen unit of its type this close, elmos.
const RESOLVE: f32 = 400.0;

/// Frames between upkeeps; staged tests list the contacts this often.
const PERIOD: i32 = 30;
const TEST_REPORT: i32 = 30 * 60;

/// Raw hits logged in staged tests.
const RAW_HITS: u32 = 400;

/// What carries a weapon.
#[derive(Clone)]
struct Carrier {
    name: String,
    human: String,
    is_static: bool,
    range: f32,
    /// Its weapon's height above the ground, elmos.
    muzzle: f32,
    /// Frames between shots, and projectiles per shot.
    reload: i32,
    shots: u32,
    /// Danger of one, on the scale of `Enemy::get_danger`; its speed.
    danger: f32,
    speed: f32,
}

/// A hit's bearing: where the aircraft was, the way across the ground to
/// the shooter, and where following it down reaches the shooter's
/// height.
#[derive(Clone, Copy)]
struct Ray {
    from: Vec3,
    across: (f32, f32),
    fix: Option<Vec3>,
}

impl Ray {
    /// How far `p` lies off the bearing, and how far along it.
    fn offset(&self, p: Vec3) -> (f32, f32) {
        let (vx, vz) = (p.x - self.from.x, p.z - self.from.z);
        let along = vx * self.across.0 + vz * self.across.1;
        ((vx * self.across.1 - vz * self.across.0).abs(), along)
    }
}

/// Unseen anti-air, by where it fired from.
pub struct AaContact {
    weapon: i32,
    pub pos: Vec3,
    pub is_static: bool,
    rays: Vec<Ray>,
    hits: Vec<i32>,
    /// At least this many fire there.
    pub shooters: u32,
    pub last: i32,
    /// Whether its bearings cross (else it stands by their slope alone).
    crossed: bool,
    reported: bool,
}

#[derive(Default)]
pub struct AirIntel {
    carriers: BTreeMap<i32, Carrier>,
    pub contacts: Vec<AaContact>,
    raw: u32,
    last_report: i32,
}

impl AaContact {
    /// Where its bearings place it: where they cross, if they come from
    /// far enough apart and the crossing lies ahead of all of them within
    /// `reach`; else the mean of where they reach the shooter's height.
    fn place(&mut self, reach: f32) {
        let rays: &[Ray] = &self.rays;
        // Least squares: the point nearest all the bearing lines.
        let (mut a, mut b, mut c, mut bx, mut bz) = (0.0f32, 0.0f32, 0.0f32, 0.0f32, 0.0f32);
        for r in rays {
            let (ux, uz) = r.across;
            let (m00, m01, m11) = (1.0 - ux * ux, -ux * uz, 1.0 - uz * uz);
            a += m00;
            b += m01;
            c += m11;
            bx += m00 * r.from.x + m01 * r.from.z;
            bz += m01 * r.from.x + m11 * r.from.z;
        }
        let det = a * c - b * b;
        let spread = (rays.iter())
            .flat_map(|r| rays.iter().map(move |o| (r.across.0 * o.across.1 - r.across.1 * o.across.0).abs()))
            .fold(0.0f32, f32::max);
        if rays.len() >= 2 && spread >= CROSSING && det.abs() > 1e-3 {
            let p = Vec3::xz((c * bx - b * bz) / det, (a * bz - b * bx) / det);
            if rays.iter().all(|r| (0.0..=reach).contains(&r.offset(p).1)) {
                (self.pos, self.crossed) = (p, true);
                return;
            }
        }
        let fixes: Vec<Vec3> = rays.iter().filter_map(|r| r.fix).collect();
        if !fixes.is_empty() {
            let n = fixes.len() as f32;
            self.pos = fixes.iter().fold(Vec3::ZERO, |s, p| Vec3::xz(s.x + p.x / n, s.z + p.z / n));
            self.crossed = false;
        }
    }

    /// How far its bearings pass from where it stands, at most.
    fn spread(&self) -> f32 {
        self.rays.iter().map(|r| r.offset(self.pos).0).fold(0.0, f32::max)
    }
}

impl Ai {
    /// The weapons of every type that is not an aircraft, once.
    fn air_intel_carriers(&mut self) {
        if !self.air_intel.carriers.is_empty() {
            return;
        }
        for def in self.cb.unit_defs().into_iter().filter(|d| !d.is_able_to_fly()) {
            let danger = |wd: WeaponDef| (weapon_power(wd, true) + def.health()) / 10.0;
            for wd in def.weapon_mounts().into_iter().filter_map(|m| m.weapon_def()) {
                let shots = (wd.salvo_size().max(1) * wd.projectiles_per_shot().max(1)) as u32;
                self.air_intel.carriers.entry(wd.id()).or_insert_with(|| Carrier {
                    name: def.name(),
                    human: def.human_name(),
                    is_static: def.speed() <= 0.0,
                    range: wd.range(),
                    muzzle: 0.75 * def.height(),
                    reload: ((wd.reload() * 30.0).round() as i32).max(1),
                    shots,
                    danger: danger(wd),
                    speed: def.speed(),
                });
            }
        }
    }

    /// One of our units was hit: an aircraft hit by what we do not see
    /// gives a bearing on the shooter.
    pub fn air_intel_unit_damaged(&mut self, unit: Unit, attacker: Option<Unit>, dir: Vec3, weapon: Option<WeaponDef>) {
        if self.dev_off("aaintel") || !unit.def().is_some_and(|d| d.is_able_to_fly()) {
            return;
        }
        let Some(wd) = weapon else { return };
        // Seen shooters the war tracker knows of.
        if attacker.is_some_and(|a| self.los.is_in_los(a.pos())) {
            return;
        }
        self.air_intel_carriers();
        let Some(c) = self.air_intel.carriers.get(&wd.id()).cloned() else { return };
        let at = unit.pos();
        if self.test.is_some() && self.air_intel.raw < RAW_HITS {
            self.air_intel.raw += 1;
            self.log(&format!(
                "air-intel hit: {} at ({:.0}, {:.0}, {:.0}) dir ({:.3}, {:.3}, {:.3})",
                c.name, at.x, at.y, at.z, dir.x, dir.y, dir.z
            ));
        }
        let h = (dir.x * dir.x + dir.z * dir.z).sqrt();
        if h < 0.1 {
            return;
        }
        let frame = self.frame;
        let ray = Ray { from: at, across: (dir.x / h, dir.z / h), fix: self.air_intel_fix(at, dir, &c) };
        let reach = c.range * RANGE_SLACK;
        let pass = if c.is_static { STATIC_PASS } else { MOBILE_PASS };
        let ai = &mut self.air_intel;
        let passes = |k: &AaContact| {
            let (off, along) = ray.offset(k.pos);
            (0.0..=reach).contains(&along) && off <= pass.max(PASS_SHARE * along)
        };
        let near = (ai.contacts.iter_mut())
            .filter(|k| k.weapon == wd.id() && passes(k))
            .min_by(|a, b| ray.offset(a.pos).0.total_cmp(&ray.offset(b.pos).0));
        let contact = match near {
            Some(k) => k,
            None => {
                // A first bearing: the contact is where it reaches the
                // shooter's height, or half the weapon's range along it.
                let pos = ray
                    .fix
                    .unwrap_or(Vec3::xz(at.x + ray.across.0 * c.range / 2.0, at.z + ray.across.1 * c.range / 2.0));
                ai.contacts.push(AaContact {
                    weapon: wd.id(),
                    pos,
                    is_static: c.is_static,
                    rays: Vec::new(),
                    hits: Vec::new(),
                    shooters: 1,
                    last: frame,
                    crossed: false,
                    reported: false,
                });
                ai.contacts.last_mut().expect("just pushed")
            }
        };
        contact.rays.push(ray);
        contact.hits.push(frame);
        let keep = if contact.is_static { RAYS } else { MOBILE_RAYS };
        if contact.rays.len() > keep {
            contact.rays.remove(0);
        }
        if contact.hits.len() > HITS {
            contact.hits.remove(0);
        }
        contact.last = frame;
        contact.place(reach);
        // The most hits within a few reload times, by the shots one fires
        // in them (less one: flight times differ, so shots bunch up).
        let window = RELOADS * c.reload;
        let most = (contact.hits.iter())
            .map(|&h| contact.hits.iter().filter(|&&o| o >= h && o - h < window).count())
            .max()
            .unwrap_or(1) as u32;
        let per = RELOADS as u32 * c.shots;
        contact.shooters = contact.shooters.max(most.saturating_sub(1).div_ceil(per).max(1));
    }

    /// Where a hit's bearing `dir` from an aircraft at `at`, followed down,
    /// reaches the height of carrier `c`'s weapon above the ground (or sea)
    /// within its reach; `None` if it points up or misses.
    fn air_intel_fix(&self, at: Vec3, dir: Vec3, c: &Carrier) -> Option<Vec3> {
        if dir.y > -MIN_DOWN {
            return None;
        }
        let hm = self.eco_height_map();
        let mut t = STEP;
        while t <= c.range * RANGE_SLACK {
            let p = Vec3::new(at.x + dir.x * t, at.y + dir.y * t, at.z + dir.z * t);
            let ground = hm.height_at(p.x, p.z).max(0.0);
            if p.y <= ground + c.muzzle {
                return Some(Vec3::new(p.x, ground, p.z));
            }
            t += STEP;
        }
        None
    }

    /// Resolve contacts by what we see, forget stale ones, and report.
    pub fn air_intel_update(&mut self, frame: i32) {
        if (frame + self.offset + 11) % PERIOD != 0 || self.air_intel.contacts.is_empty() {
            return;
        }
        let carriers = std::mem::take(&mut self.air_intel.carriers);
        let contacts = std::mem::take(&mut self.air_intel.contacts);
        let mut kept = Vec::new();
        for mut k in contacts {
            let Some(c) = carriers.get(&k.weapon) else { continue };
            let at = format!("({:.0}, {:.0})", k.pos.x, k.pos.z);
            let seen = (self.war.targets.values())
                .filter(|e| e.identified && !e.is_dead && e.def_name == c.name)
                .map(|e| distance(e.position, k.pos))
                .filter(|&d| d <= RESOLVE)
                .fold(None, |m: Option<f32>, d| Some(m.map_or(d, |m| m.min(d))));
            let age = frame - k.last;
            let why = if let Some(off) = seen {
                Some(format!("is seen, {off:.0} from where {} hits put it", k.hits.len()))
            } else if k.is_static && k.crossed && self.los.is_in_los(k.pos) && age > 30 * 5 {
                Some("is not there, a look finds".to_owned())
            } else if age > if k.is_static { STATIC_MEMORY } else { MOBILE_MEMORY } {
                Some(format!("is forgotten, unheard of for {} s", age / 30))
            } else {
                None
            };
            if let Some(why) = why {
                if k.reported {
                    self.log(&format!("air-intel: the unseen {} at {at} {why}", c.human));
                }
                continue;
            }
            if !k.reported && k.rays.len() >= 2 {
                k.reported = true;
                self.log(&format!(
                    "air-intel: unseen {} ({}, {}) at {at}, {}, from {} hits: at least {} firing",
                    c.human,
                    c.name,
                    if k.is_static { "static" } else { "mobile" },
                    if k.crossed {
                        format!("where bearings cross within {:.0}", k.spread())
                    } else {
                        "by the slope of its bearings".to_owned()
                    },
                    k.hits.len(),
                    k.shooters
                ));
            }
            kept.push(k);
        }
        self.air_intel.carriers = carriers;
        self.air_intel.contacts = kept;
        if self.test.is_some() && frame - self.air_intel.last_report >= TEST_REPORT {
            self.air_intel.last_report = frame;
            let list: Vec<String> = (self.air_intel.contacts.iter())
                .filter_map(|k| {
                    let c = self.air_intel.carriers.get(&k.weapon)?;
                    Some(format!(
                        "{} ({:.0}, {:.0}) {} rays{} x{}",
                        c.name,
                        k.pos.x,
                        k.pos.z,
                        k.rays.len(),
                        if k.crossed { " crossed" } else { "" },
                        k.shooters
                    ))
                })
                .collect();
            self.log(&format!("air-intel contacts: {}", list.join("; ")));
        }
    }

    /// A contact's anti-air threat as `(position, radius in elmos,
    /// power)`: an installation as it stands, a mobile group weaker and
    /// wider the staler it is.
    fn air_intel_circle(&self, k: &AaContact) -> Option<(Vec3, f32, f32)> {
        let c = self.air_intel.carriers.get(&k.weapon)?;
        let power = c.danger * k.shooters as f32;
        if k.is_static {
            return Some((k.pos, c.range, power));
        }
        let age = (self.frame - k.last).max(0) as f32;
        let fade = (1.0 - age / MOBILE_MEMORY as f32).max(0.0);
        Some((k.pos, c.range + c.speed * age / 30.0, power * fade * fade)).filter(|t| t.2 > 0.0)
    }

    /// The anti-air threat of our unseen contacts (see `air_intel_circle`).
    pub fn air_intel_threat(&self) -> Vec<(Vec3, f32, f32)> {
        self.air_intel.contacts.iter().filter_map(|k| self.air_intel_circle(k)).collect()
    }

    /// The contacts as (position, anti-air power, frame last heard of), for
    /// region occupancy.
    pub fn air_intel_presence(&self) -> Vec<(Vec3, f32, i32)> {
        (self.air_intel.contacts.iter()).filter_map(|k| self.air_intel_circle(k).map(|t| (t.0, t.2, k.last))).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A bearing from `from` towards `to`, turned by `err` degrees.
    fn ray(from: (f32, f32), to: (f32, f32), err: f32) -> Ray {
        let a = (to.1 - from.1).atan2(to.0 - from.0) + err.to_radians();
        Ray { from: Vec3::xz(from.0, from.1), across: (a.cos(), a.sin()), fix: None }
    }

    fn contact(rays: Vec<Ray>) -> AaContact {
        AaContact {
            weapon: 0,
            pos: Vec3::ZERO,
            is_static: true,
            rays,
            hits: Vec::new(),
            shooters: 1,
            last: 0,
            crossed: false,
            reported: false,
        }
    }

    #[test]
    fn bearings_from_apart_cross_at_the_shooter() {
        // A Chainsaw at (1000, 1000), hit at from three sides, bearings a
        // few degrees off as observed in game.
        let shooter = (1000.0, 1000.0);
        let mut k = contact(vec![
            ray((300.0, 1100.0), shooter, 4.0),
            ray((900.0, 300.0), shooter, -5.0),
            ray((1600.0, 1500.0), shooter, 3.0),
        ]);
        k.place(2300.0);
        assert!(k.crossed);
        assert!(distance(k.pos, Vec3::xz(1000.0, 1000.0)) < 120.0, "{:?}", (k.pos.x, k.pos.z));
        // Bearings from nearly one way do not cross well: no crossing.
        let mut k = contact(vec![ray((300.0, 1000.0), shooter, 2.0), ray((310.0, 1010.0), shooter, -2.0)]);
        k.place(2300.0);
        assert!(!k.crossed);
    }
}
