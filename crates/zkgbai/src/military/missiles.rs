//! Tactical missiles. A silo (`staticmissilesilo`: four pads, 1200 metal)
//! goes up where it is safe and within Eos range of more than one enemy
//! building worth a missile (a Fusion and up), or of an anti-nuke in the
//! way of our nukes; the constructors near it are summoned to rush it (see
//! the economy's job costs). Its Eos (`tacnuke`: 600 metal, 3502 damage,
//! 3500 range; anti-nukes do not stop them, shields do) go at those
//! buildings, as many as it takes each. Its Shockleys (`empmissile`: stun
//! for up to 45 s, longer than a nuke's flight) go at the anti-nukes in the
//! way of a loaded nuke, fired together with it (see `nukes`).
//!
//! An anti-nuke we struck (with an Eos, or shelled with Berthas) and have
//! not seen since stays on the books whether dead or not: scouts look.

use std::collections::BTreeMap;

use springai::{NO_TIMEOUT, Unit, Vec3};

use crate::Ai;
use crate::util::{distance, get_directional_point};

pub const SILO: &str = "staticmissilesilo";
const EOS: &str = "tacnuke";
const SHOCKLEY: &str = "empmissile";

/// What the silo builds: units on its pads until fired.
pub const MISSILES: [&str; 5] = ["tacnuke", "empmissile", "napalmmissile", "seismic", "missileslow"];

const PADS: usize = 4;

/// Eos and Shockley range (3500), less a margin for the pads.
pub const REACH: f32 = 3450.0;

/// Silo sites are tried at our metal and this far ahead of it, elmos.
const SITE_STEPS: [f32; 3] = [400.0, 800.0, 1200.0];
const EOS_DAMAGE: f32 = 3502.0;

/// Value per missile that makes a target worth an Eos (600 metal): a
/// Fusion's.
const WORTH: f32 = 1000.0;

/// Income from which a silo and its missiles are affordable.
pub const SILO_INCOME: f32 = 30.0;

/// Enemy shields stop an Eos (3600 shield power against 3502 damage).
const SHIELDS: [&str; 2] = ["staticshield", "shieldshield"];
const SHIELD_RADIUS: f32 = 350.0;

/// A struck target is left alone until seen standing this long after the
/// strike (the AI hears of no death it does not see: a target struck and
/// not seen since is most likely gone); a struck anti-nuke not seen for
/// this long gets a scout. Strikes are forgotten after a while.
const LOOK_AFTER: i32 = 30 * 20;
const STRUCK_MEMORY: i32 = 30 * 600;

const CMD_ATTACK: i32 = 20;

/// Frames between reports of why there is no silo.
const REPORT_FRAMES: i32 = 30 * 300;

#[derive(Default)]
pub struct Missiles {
    silo: Option<Unit>,
    /// Missiles on the pads.
    ready: Vec<Unit>,
    /// Enemy buildings struck, since when.
    struck: BTreeMap<i32, i32>,
    last_report: i32,
}

/// An enemy building worth Eos: how many it takes.
struct Target {
    id: i32,
    pos: Vec3,
    value: f32,
    needed: usize,
}

impl Ai {
    pub fn missiles_silo(&self) -> Option<Unit> {
        self.missiles.silo.filter(|s| s.health() > 0.0)
    }

    pub fn missiles_add_silo(&mut self, silo: Unit) {
        self.missiles.silo = Some(silo);
        let p = silo.pos();
        self.log(&format!("missiles: silo up at ({:.0}, {:.0})", p.x, p.z));
    }

    /// A missile on a pad: ours to fire.
    pub fn missiles_add_missile(&mut self, missile: Unit) {
        missile.set_fire_state(0, 0);
        self.missiles.ready.push(missile);
    }

    fn missiles_shielded(&self, p: Vec3) -> bool {
        (self.war.targets.values())
            .any(|e| !e.is_dead && SHIELDS.contains(&e.def_name.as_str()) && distance(e.position, p) <= SHIELD_RADIUS)
    }

    /// Enemy buildings worth Eos: unshielded, not struck lately (unless
    /// seen standing since).
    fn missiles_eos_targets(&self) -> Vec<Target> {
        let struck = |id: i32, seen: i32| self.missiles.struck.get(&id).is_some_and(|&t| seen < t + LOOK_AFTER);
        (self.war.targets.values())
            .filter(|e| e.identified && e.is_static && !e.is_dead && !struck(e.unit_id, e.last_seen))
            .filter_map(|e| {
                let needed = (e.ud?.health() / EOS_DAMAGE).ceil().max(1.0) as usize;
                let t = Target { id: e.unit_id, pos: e.position, value: e.value, needed };
                (e.value / needed as f32 >= WORTH && needed <= PADS).then_some(t)
            })
            .filter(|t| !self.missiles_shielded(t.pos))
            .collect()
    }

    /// Known anti-nukes guarding something from our nukes (their value is
    /// more than their cost), shielded or not: what Shockleys are for.
    fn missiles_stun_targets(&self) -> Vec<(i32, Vec3, f32)> {
        if !self.misc_has_nuke() {
            return Vec::new();
        }
        (self.war.targets.values())
            .filter(|e| !e.is_dead && e.def_name == "staticantinuke")
            .filter(|e| e.ud.is_some_and(|ud| e.value > ud.cost(self.metal)))
            .map(|e| (e.unit_id, e.position, e.value))
            .collect()
    }

    /// Where a silo would do most: a safe place on our ground within reach
    /// of more than one target, or of an anti-nuke in the way of our nukes;
    /// with the value and the number of targets in reach.
    pub fn missiles_site(&self) -> Option<(Vec3, f32, usize)> {
        let eos = self.missiles_eos_targets();
        let stun = self.missiles_stun_targets();
        if eos.is_empty() && stun.is_empty() {
            return None;
        }
        let enemy = self.graph.get_enemy_center();
        let mut sites: Vec<Vec3> = self.graph.get_owned_spots().iter().map(|m| m.get_pos()).collect();
        sites.push(self.graph.get_ally_center());
        // And ahead of our metal, towards the enemy.
        let ahead: Vec<Vec3> =
            (sites.iter()).flat_map(|&p| SITE_STEPS.iter().map(move |&d| get_directional_point(p, enemy, d))).collect();
        sites.extend(ahead);
        let scored: Vec<(Vec3, f32, usize)> = sites
            .into_iter()
            // Safe enough: ours, and our forces there outweigh the enemy's.
            .filter(|&p| self.graph.is_ally_territory(p) && self.war.get_effective_threat(p) <= 0.0)
            .filter_map(|p| {
                let near = eos.iter().filter(|t| distance(t.pos, p) <= REACH);
                let (mut value, mut n) = near.fold((0.0, 0), |(v, n), t| (v + t.value, n + 1));
                let antis = stun.iter().filter(|a| distance(a.1, p) <= REACH && !eos.iter().any(|t| t.id == a.0));
                for a in antis {
                    value += a.2;
                    n += 2;
                }
                (n >= 2).then_some((p, value, n))
            })
            .collect();
        let best = scored.iter().map(|s| s.1).fold(0.0, f32::max);
        // Of the best few, the farthest from the enemy.
        scored
            .into_iter()
            .filter(|s| s.1 >= 0.8 * best)
            .max_by(|a, b| distance(a.0, enemy).total_cmp(&distance(b.0, enemy)))
    }

    /// Say now and then why there is no silo though we could afford one.
    pub fn missiles_report_no_site(&mut self) {
        if self.frame - self.missiles.last_report < REPORT_FRAMES {
            return;
        }
        self.missiles.last_report = self.frame;
        let targets = self.missiles_eos_targets();
        let ours = self.graph.get_owned_spots().iter().map(|m| m.get_pos()).collect::<Vec<_>>();
        let nearest = (targets.iter())
            .flat_map(|t| ours.iter().map(move |&p| distance(p, t.pos)))
            .reduce(f32::min)
            .map_or("none".to_owned(), |d| format!("{d:.0}"));
        let value: f32 = targets.iter().map(|t| t.value).sum();
        self.log(&format!(
            "missiles: no silo site: {} targets worth Eos ({value:.0} metal), the nearest {nearest} from our metal \
             (reach {REACH:.0})",
            targets.len()
        ));
    }

    pub fn missiles_update(&mut self, frame: i32) {
        if (frame + self.offset) % 90 != 0 {
            return;
        }
        self.missiles.ready.retain(|m| m.health() > 0.0);
        self.missiles.struck.retain(|_, t| frame - *t < STRUCK_MEMORY);
        let Some(silo) = self.missiles_silo() else { return };
        self.missiles_fire_eos();
        self.missiles_restock(silo);
    }

    fn missiles_ready(&self, kind: &str) -> Vec<Unit> {
        (self.missiles.ready.iter().copied()).filter(|m| self.defs.name_of(*m) == kind && !m.is_being_built()).collect()
    }

    /// Fire the Eos on the pads at the best targets in reach, as many at
    /// each as it takes.
    fn missiles_fire_eos(&mut self) {
        let mut free = self.missiles_ready(EOS);
        if free.is_empty() {
            return;
        }
        let mut targets = self.missiles_eos_targets();
        targets.retain(|t| free.iter().any(|m| distance(m.pos(), t.pos) <= REACH));
        targets.sort_by(|a, b| (b.value / b.needed as f32).total_cmp(&(a.value / a.needed as f32)));
        for t in targets {
            if free.len() < t.needed {
                continue;
            }
            for m in free.drain(..t.needed) {
                m.execute_custom_command(CMD_ATTACK, &[t.pos.x, t.pos.y, t.pos.z], 0, NO_TIMEOUT);
                self.missiles.ready.retain(|r| r.id() != m.id());
            }
            self.missiles.struck.insert(t.id, self.frame);
            let name = self.war.targets.get(&t.id).map_or("?", |e| e.def_name.as_str());
            self.log(&format!(
                "missiles: {} Eos at a {name} worth {:.0} at ({:.0}, {:.0})",
                t.needed, t.value, t.pos.x, t.pos.z
            ));
        }
    }

    /// Keep the pads stocked: Shockleys for the anti-nukes in the way of
    /// our nukes, Eos for what is worth them.
    fn missiles_restock(&mut self, silo: Unit) {
        let (Some(eos), Some(shockley)) = (self.defs.get(EOS), self.defs.get(SHOCKLEY)) else { return };
        let queued: Vec<i32> = silo.current_commands().iter().map(|c| -c.id()).filter(|&id| id > 0).collect();
        let count = |kind: &str, id: i32| self.missiles_ready(kind).len() + queued.iter().filter(|&&q| q == id).count();
        let (n_eos, n_shock) = (count(EOS, eos.id()), count(SHOCKLEY, shockley.id()));
        if self.missiles.ready.len() + queued.len() >= PADS || self.eco.estalled {
            return;
        }
        let here = silo.pos();
        let stun = self.missiles_stun_targets().iter().filter(|a| distance(a.1, here) <= REACH).count();
        let needed: usize =
            self.missiles_eos_targets().iter().filter(|t| distance(t.pos, here) <= REACH).map(|t| t.needed).sum();
        let def = if n_shock < stun.min(2) {
            shockley
        } else if n_eos < needed.min(PADS) {
            eos
        } else {
            return;
        };
        silo.build(def, here, 0, 0);
    }

    /// Fire a Shockley at each of `antis` (stunning them through a nuke's
    /// flight), if there are enough on the pads; whether it did.
    pub fn missiles_stun(&mut self, antis: &[(i32, Vec3)]) -> bool {
        let ready = self.missiles_ready(SHOCKLEY);
        let fits = |a: &(i32, Vec3)| ready.iter().filter(|m| distance(m.pos(), a.1) <= REACH).count();
        if ready.len() < antis.len() || antis.iter().any(|a| fits(a) == 0) {
            return false;
        }
        let mut free = ready;
        for &(_, p) in antis {
            let Some(i) = free.iter().position(|m| distance(m.pos(), p) <= REACH) else { return false };
            let m = free.remove(i);
            m.execute_custom_command(CMD_ATTACK, &[p.x, p.y, p.z], 0, NO_TIMEOUT);
            self.missiles.ready.retain(|r| r.id() != m.id());
        }
        true
    }

    /// Whether Shockleys on the pads reach the anti-nuke at `p`.
    pub fn missiles_can_stun(&self, p: Vec3) -> bool {
        self.missiles_ready(SHOCKLEY).iter().any(|m| distance(m.pos(), p) <= REACH)
    }

    /// Note a strike at `p` (a Bertha shelling, say) on an anti-nuke
    /// there, from when it began.
    pub fn missiles_note_strike(&mut self, p: Vec3) {
        let frame = self.frame;
        let hit: Vec<i32> = (self.war.targets.values())
            .filter(|e| !e.is_dead && e.def_name == "staticantinuke" && distance(e.position, p) < 150.0)
            .map(|e| e.unit_id)
            .collect();
        for id in hit {
            self.missiles.struck.entry(id).or_insert(frame);
        }
    }

    /// A struck anti-nuke still on the books and not seen for a while: where
    /// a scout should look whether it is gone.
    pub fn missiles_scout_target(&self, from: Vec3) -> Option<Vec3> {
        let frame = self.frame;
        (self.missiles.struck.iter())
            .filter(|&(_, &t)| frame - t > LOOK_AFTER)
            .filter_map(|(id, _)| self.war.targets.get(id))
            .filter(|e| !e.is_dead && e.def_name == "staticantinuke" && frame - e.last_seen > LOOK_AFTER)
            .map(|e| e.position)
            .min_by(|a, b| distance(*a, from).total_cmp(&distance(*b, from)))
    }
}
