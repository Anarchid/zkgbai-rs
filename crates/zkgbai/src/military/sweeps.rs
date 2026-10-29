//! A Sparrow's last act: its one-click ability ("Detonate",
//! `CMD_ONECLICK_WEAPON`) sprints it at five times its speed for three
//! seconds, then blows it up into a scan sweep (400 to 640 elmos, for 12 s)
//! that reveals and decloaks. It is spent where seeing is worth a Sparrow:
//! an anti-nuke we struck that may be gone (see `missiles`), and cloaked
//! snipers shooting at us unseen (they are where the shots come from; the
//! army is called there to kill what the sweep shows).

use springai::{NO_TIMEOUT, Unit, Vec3, WeaponDef};

use crate::Ai;
use crate::kgbutil::kgb_move_to;
use crate::military::DefenseTarget;
use crate::util::{OPTION_SHIFT_KEY, distance};

/// Zero-K's one-click ability command, its first ability.
const CMD_ONECLICK_WEAPON: i32 = 35000;

/// A Sparrow detonates this close to where it is to sweep: it gets there
/// within its three-second sprint.
pub const DETONATE_RANGE: f32 = 2500.0;

/// How far from what it hits a sniper (the Phantom, range 700) stands.
const SNIPER_RANGE: f32 = 650.0;

/// Shots from unseen snipers are remembered this long, and shots this
/// close together come from the same ones; this many of them call for a
/// sweep (one stray shot does not).
pub const SNIPER_MEMORY: i32 = 30 * 20;
const PRESENCE_MEMORY: i32 = 30 * 180;
const SNIPER_MERGE: f32 = 400.0;
const SNIPER_SHOTS: u32 = 2;

#[derive(Default)]
pub struct Sweeps {
    /// Where unseen snipers shot from lately, when last, and how often.
    snipers: Vec<Snipers>,
    /// The Sparrow sprinting to its sweep.
    pub detonating: Option<i32>,
}

impl Ai {
    /// One of our units was hit: by an unseen sniper, note where from.
    pub fn sweeps_unit_damaged(&mut self, unit: Unit, attacker: Option<Unit>, dir: Vec3, weapon: Option<WeaponDef>) {
        let Some(weapon) = weapon.filter(|w| w.name().starts_with("cloaksnipe")) else { return };
        let p = unit.pos();
        let len = dir.x.hypot(dir.z);
        let known = attacker.map(|a| a.pos()).filter(|p| *p != crate::util::NULLPOS);
        if known.is_none() && (!len.is_finite() || len < 0.01) {
            return;
        }
        let at =
            known.unwrap_or_else(|| Vec3::new(p.x + dir.x / len * SNIPER_RANGE, p.y, p.z + dir.z / len * SNIPER_RANGE));
        self.sweeps_note_sniper(at, (weapon.reload() * 30.0).round().max(1.0) as i32);
    }

    pub(crate) fn sweeps_note_sniper(&mut self, at: Vec3, reload: i32) {
        let frame = self.frame;
        self.sweeps.snipers.retain(|s| frame - s.last < PRESENCE_MEMORY);
        let cluster = self.sweeps.snipers.iter().position(|s| distance(s.pos, at) < SNIPER_MERGE);
        let i = cluster.unwrap_or_else(|| {
            self.sweeps.snipers.push(Snipers { pos: at, last: frame, shots: Vec::new(), residual: 1, reload });
            self.sweeps.snipers.len() - 1
        });
        let s = &mut self.sweeps.snipers[i];
        s.shots.retain(|f| frame - *f < SNIPER_MEMORY);
        // Smooth noisy bearings without pretending the result is an exact fix.
        s.pos = Vec3::new((s.pos.x + at.x) / 2.0, at.y, (s.pos.z + at.z) / 2.0);
        s.last = frame;
        s.reload = reload;
        s.shots.push(frame);
    }

    /// Approximate infestations, with a lower-bound count from impact cadence.
    pub fn phantom_fixes(&self, max_age: i32) -> Vec<PhantomFix> {
        self.sweeps
            .snipers
            .iter()
            .filter(|s| self.frame - s.last < max_age)
            .map(|s| PhantomFix { pos: s.pos, last: s.last, count: impact_count(&s.shots, s.reload).max(s.residual) })
            .collect()
    }

    pub fn sweeps_sniper_target(&mut self, from: Vec3) -> Option<Vec3> {
        self.sweeps.snipers.retain(|s| self.frame - s.last < PRESENCE_MEMORY);
        self.sweeps
            .snipers
            .iter()
            .filter(|s| self.frame - s.last < SNIPER_MEMORY && s.shots.len() >= SNIPER_SHOTS as usize)
            .map(|s| s.pos)
            .min_by(|a, b| distance(*a, from).total_cmp(&distance(*b, from)))
    }

    pub(crate) fn sweeps_phantom_destroyed(&mut self, pos: Vec3) {
        if let Some(s) = self
            .sweeps
            .snipers
            .iter_mut()
            .filter(|s| distance(s.pos, pos) < SNIPER_MERGE)
            .min_by(|a, b| distance(a.pos, pos).total_cmp(&distance(b.pos, pos)))
        {
            s.residual = impact_count(&s.shots, s.reload).max(s.residual).saturating_sub(1);
            s.shots.clear();
        }
        self.sweeps.snipers.retain(|s| s.residual > 0 || !s.shots.is_empty());
    }

    /// Sprint Sparrow `sparrow` at `target` and sweep there; with `fight`,
    /// call the army to kill what it shows.
    pub fn sweeps_detonate(&mut self, sparrow: Unit, target: Vec3, fight: bool) {
        sparrow.execute_custom_command(CMD_ONECLICK_WEAPON, &[1.0], 0, NO_TIMEOUT);
        kgb_move_to(sparrow, target, OPTION_SHIFT_KEY, self.frame);
        self.sweeps.detonating = Some(sparrow.id());
        if fight {
            // A scan is not evidence that the snipers died; retain their cadence.
            self.war.add_defense_target(DefenseTarget::new(target, 3000.0, self.frame));
        }
        let why = if fight { "unseen snipers" } else { "a struck anti-nuke" };
        self.log(&format!("sweeps: Sparrow sweeps ({:.0}, {:.0}) for {why}", target.x, target.z));
    }
}

struct Snipers {
    pos: Vec3,
    last: i32,
    shots: Vec<i32>,
    reload: i32,
    residual: usize,
}

#[derive(Clone, Copy, Debug)]
pub struct PhantomFix {
    pub pos: Vec3,
    pub last: i32,
    pub count: usize,
}

/// Maximum impacts inside one reload (with a second of flight-time slack).
/// A single Phantom firing every reload remains one, even over many reloads.
fn impact_count(shots: &[i32], reload: i32) -> usize {
    let window = (reload - 30).max(1);
    shots
        .iter()
        .enumerate()
        .map(|(i, f)| shots[i..].iter().take_while(|t| **t - *f < window).count())
        .max()
        .unwrap_or(1)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cadence_counts_snipers_not_lifetime_shots() {
        assert_eq!(impact_count(&[0, 510, 1020, 1530], 510), 1);
        assert_eq!(impact_count(&[0, 10, 510, 520], 510), 2);
        assert_eq!(impact_count(&[0, 160, 320, 510, 670, 830], 510), 3);
        assert_eq!(impact_count(&[0, 490, 1000], 510), 1);
        assert_eq!(impact_count(&[0, 0, 0], 510), 3);
    }
}
