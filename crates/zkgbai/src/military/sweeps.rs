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
const SNIPER_MEMORY: i32 = 30 * 20;
const SNIPER_MERGE: f32 = 400.0;
const SNIPER_SHOTS: u32 = 2;

#[derive(Default)]
pub struct Sweeps {
    /// Where unseen snipers shot from lately, when last, and how often.
    snipers: Vec<(Vec3, i32, u32)>,
    /// The Sparrow sprinting to its sweep.
    pub detonating: Option<i32>,
}

impl Ai {
    /// One of our units was hit: by an unseen sniper, note where from.
    pub fn sweeps_unit_damaged(&mut self, unit: Unit, attacker: Option<Unit>, dir: Vec3, weapon: Option<WeaponDef>) {
        if attacker.is_some() || !weapon.is_some_and(|w| w.name().starts_with("cloaksnipe")) {
            return;
        }
        let p = unit.pos();
        let at = Vec3::new(p.x + dir.x * SNIPER_RANGE, p.y, p.z + dir.z * SNIPER_RANGE);
        let frame = self.frame;
        match self.sweeps.snipers.iter_mut().find(|s| distance(s.0, at) < SNIPER_MERGE) {
            Some(s) => *s = (at, frame, s.2 + 1),
            None => self.sweeps.snipers.push((at, frame, 1)),
        }
    }

    /// Where unseen snipers shot from lately, the nearest to `from`.
    pub fn sweeps_sniper_target(&mut self, from: Vec3) -> Option<Vec3> {
        let frame = self.frame;
        self.sweeps.snipers.retain(|s| frame - s.1 < SNIPER_MEMORY);
        (self.sweeps.snipers.iter().filter(|s| s.2 >= SNIPER_SHOTS).map(|s| s.0))
            .min_by(|a, b| distance(*a, from).total_cmp(&distance(*b, from)))
    }

    /// Sprint Sparrow `sparrow` at `target` and sweep there; with `fight`,
    /// call the army to kill what it shows.
    pub fn sweeps_detonate(&mut self, sparrow: Unit, target: Vec3, fight: bool) {
        sparrow.execute_custom_command(CMD_ONECLICK_WEAPON, &[1.0], 0, NO_TIMEOUT);
        kgb_move_to(sparrow, target, OPTION_SHIFT_KEY, self.frame);
        self.sweeps.detonating = Some(sparrow.id());
        if fight {
            self.sweeps.snipers.retain(|s| distance(s.0, target) >= SNIPER_MERGE);
            self.war.add_defense_target(DefenseTarget::new(target, 3000.0, self.frame));
        }
        let why = if fight { "unseen snipers" } else { "a struck anti-nuke" };
        self.log(&format!("sweeps: Sparrow sweeps ({:.0}, {:.0}) for {why}", target.x, target.z));
    }
}
