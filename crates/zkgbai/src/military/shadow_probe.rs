//! Development aid: the fire-shadow model (see `precision`) checked against
//! the engine. With the AI option `test=fort`, a passive defender gets
//! (by cheats) a mex at each of its nearest metal spots, a defense of a
//! different kind by each, toward the middle of the map, and solars beside
//! them, and Owls patrolling overhead so that it sees whatever comes near.
//! With `test=shadowprobe`, the attacker, cheating to see all of it, sets
//! Glaives down around one defense after another (holding fire and
//! position): most where the model says no defense can hit them (the
//! closest calls first), one where it says this one can. And one more, by
//! the defense's mex, set to target the mex (ZK's set-target, as precision
//! raiders do): where the model says its line to the mex is free, or, every
//! other round, where it says the line is blocked. A few seconds later it
//! logs which were hit, and by what weapon, which shot the mex, and running
//! tallies.

use std::collections::BTreeMap;

use springai::{Unit, Vec3, WeaponDef};

use crate::Ai;
use crate::defs::cmd;
use crate::military::precision::{HIDE_MARGIN, Standing};
use crate::util::distance;

/// The probe unit.
const PROBE: &str = "cloakraid";
/// The first round, and how long each lasts.
const FIRST_ROUND: i32 = 30 * 40;
const ROUND: i32 = 30 * 8;
/// Probes the model calls hidden, per round (and one it calls open).
const HIDDEN_PER_ROUND: usize = 3;
/// Probes stand this far apart, and this far from the one in the open
/// (beyond the splash of the shots at it).
const SPACING: f32 = 64.0;
const OPEN_SPACING: f32 = 320.0;

/// A probe set down this round.
struct Probe {
    unit: Unit,
    gun: String,
    dist: f32,
    /// The defense's least height to hit there, and the probe's aim height
    /// the model assumed.
    need: f32,
    aim: f32,
    hidden: bool,
    /// Hidden only by a building.
    built: bool,
    /// Where it was set down, and every defense's least height to hit
    /// there.
    at: Vec3,
    needs: String,
}

/// A probe set to shoot a mex this round.
struct Shot {
    unit: Unit,
    target: Unit,
    target_health: f32,
    /// Whether the model says its line to the target is free.
    free: bool,
    dist: f32,
    frame: i32,
}

#[derive(Default)]
pub struct Probing {
    probes: Vec<Probe>,
    shots: Vec<Shot>,
    next_gun: usize,
    /// Probes called hidden, and those of them hit; called open, and hit.
    hidden: u32,
    hidden_hit: u32,
    open: u32,
    open_hit: u32,
    /// Of the hidden ones, those only a building hides (the terrain alone
    /// would not), and of those hit.
    built: u32,
    built_hit: u32,
    /// Shots the model calls free, and those that hit the mex; blocked,
    /// and hit.
    free: u32,
    free_hit: u32,
    blocked: u32,
    blocked_hit: u32,
    /// Weapons that hit each probe this round.
    hits: BTreeMap<i32, Vec<String>>,
}

impl Ai {
    pub fn shadow_probe_damaged(&mut self, unit: Unit, weapon: Option<WeaponDef>) {
        let probing = &mut self.precision.probing;
        if probing.probes.iter().any(|p| p.unit.id() == unit.id()) {
            let name = weapon.map_or_else(|| "?".to_owned(), |w| w.name());
            probing.hits.entry(unit.id()).or_default().push(name);
        }
    }

    pub fn shadow_probe_update(&mut self, frame: i32) {
        if frame < FIRST_ROUND || (frame - FIRST_ROUND) % ROUND != 0 {
            return;
        }
        // The model, from everything the enemy has (we cheat).
        let map = self.cb.map();
        if self.precision.ground().is_none() {
            self.precision.set_terrain(map.corner_height_map(), map.width(), map.height());
        }
        let found: Vec<(springai::Unit, springai::UnitDef)> = (self.cb.enemy_units().into_iter())
            .filter_map(|u| Some((u, u.def().filter(|d| d.speed() == 0.0)?)))
            .collect();
        let standing: Vec<Standing> = (found.into_iter())
            .map(|(u, def)| {
                let armed = !def.weapon_mounts().is_empty() && !def.tooltip().contains("Anti-Air");
                self.precision_stand(u.id(), def, u.pos(), armed, u)
            })
            .collect();
        self.precision.sync(&standing, map.gravity());

        self.shadow_probe_score();

        let Some(def) = self.defs.get(PROBE) else { return };
        let Some(sh) = self.precision_shooter(def) else { return };
        let guns = self.precision.gun_ids();
        if guns.is_empty() {
            self.log("probe: no defenses seen");
            return;
        }
        let id = guns[self.precision.probing.next_gun % guns.len()];
        self.precision.probing.next_gun += 1;
        let Some((at, name, range)) = self.precision.gun_info(id).map(|(p, n, r)| (p, n.to_owned(), r)) else {
            return;
        };
        let Some(ground) = self.precision.ground() else { return };

        // Candidate cells in the defense's reach, away from its body and
        // the rim of its range.
        let (mut hidden, mut open) = (Vec::new(), Vec::new());
        let r = (range / 16.0) as i32;
        for cz in -r..=r {
            for cx in -r..=r {
                let (x, z) = (at.x + cx as f32 * 16.0, at.z + cz as f32 * 16.0);
                let d = (x - at.x).hypot(z - at.z);
                if !(64.0..=range - 40.0).contains(&d) || !self.precision.can_stand(x, z, &sh) {
                    continue;
                }
                let need = self.precision.need_of(id, x, z, sh.aim);
                let bare = self.precision.bare_need_of(id, x, z, sh.aim);
                if need > sh.aim + HIDE_MARGIN && self.precision.hidden_at(x, z, sh.aim) {
                    // Building shadows first: they are what is in question.
                    let built = bare <= sh.aim;
                    hidden.push((need - sh.aim - if built { 1000.0 } else { 0.0 }, x, z, d, need));
                } else if need < sh.aim - 4.0 {
                    open.push((d, x, z, d, need));
                }
            }
        }
        // The closest calls first, then the deepest.
        hidden.sort_by(|a, b| a.0.total_cmp(&b.0));
        let mut picks: Vec<(f32, f32, f32, f32, bool)> = Vec::new();
        let spaced = |picks: &[(f32, f32, f32, f32, bool)], x: f32, z: f32| {
            picks.iter().all(|p| (p.0 - x).hypot(p.1 - z) >= SPACING)
        };
        for (&(_, x, z, d, need), limit) in
            (hidden.iter().map(|h| (h, HIDDEN_PER_ROUND - 1))).chain(hidden.iter().rev().map(|h| (h, HIDDEN_PER_ROUND)))
        {
            if picks.len() < limit && spaced(&picks, x, z) {
                picks.push((x, z, d, need, true));
            }
        }
        open.sort_by(|a, b| a.0.total_cmp(&b.0));
        if let Some(&(_, x, z, d, need)) = open
            .iter()
            .filter(|o| picks.iter().all(|p| (p.0 - o.1).hypot(p.1 - o.2) >= OPEN_SPACING))
            .nth(open.len() / 4)
        {
            picks.push((x, z, d, need, false));
        }
        self.log(&format!(
            "probe: round at {name} ({:.0}, {:.0}): {} hidden and {} open cells, {} probes",
            at.x,
            at.z,
            hidden.len(),
            open.len(),
            picks.len()
        ));
        let stands: Vec<f32> = picks.iter().map(|p| ground.stand(p.0, p.1)).collect();
        for (&(x, z, dist, need, is_hidden), stand) in picks.iter().zip(stands) {
            let needs = (self.precision.needs_at(x, z, sh.aim).iter())
                .map(|(n, d, need, muzzle)| format!("{n}@{d:.0}:{need:.1}/m{muzzle:.0}"))
                .collect::<Vec<_>>()
                .join(" ");
            let Some(unit) = self.cb.cheat_new_unit(def, springai::Vec3::new(x, stand, z)) else { continue };
            unit.set_fire_state(0, 0);
            unit.set_move_state(0, 0);
            if distance(unit.pos(), springai::Vec3::new(x, unit.pos().y, z)) > 8.0 {
                self.log(&format!("probe: set down off its cell, at ({:.0}, {:.0})", unit.pos().x, unit.pos().z));
            }
            let built = is_hidden && self.precision.bare_need_of(id, x, z, sh.aim) <= sh.aim;
            self.precision.probing.probes.push(Probe {
                unit,
                gun: name.clone(),
                dist,
                need,
                aim: sh.aim,
                hidden: is_hidden,
                built,
                at: Vec3::new(x, stand, z),
                needs,
            });
        }

        // A shooter at the defense's radar, if it has one, else its mex.
        let near = |name: &str| {
            (self.cb.enemy_units().into_iter())
                .filter(|u| self.defs.name_of(*u) == name && distance(u.pos(), at) < range + 300.0)
                .min_by(|a, b| distance(a.pos(), at).total_cmp(&distance(b.pos(), at)))
        };
        let mex = near("staticradar").or_else(|| near("staticmex"));
        let Some(mex) = mex else { return };
        let Some(mdef) = mex.def() else { return };
        let half = mdef.x_size().min(mdef.z_size()) as f32 * 4.0;
        let want_free = self.precision.probing.next_gun % 2 == 1;
        let taken: Vec<Vec3> = self.precision.probing.probes.iter().map(|p| p.unit.pos()).collect();
        let spots = self.precision.hidden_spots(mex.pos(), mex.pos().y, half, &sh);
        let (free, blocked) = (spots.iter().filter(|s| s.1).count(), spots.iter().filter(|s| !s.1).count());
        let pick = (spots.iter())
            .filter(|s| s.1 == want_free && taken.iter().all(|t| distance(*t, s.0) >= SPACING))
            .min_by(|a, b| distance(a.0, mex.pos()).total_cmp(&distance(b.0, mex.pos())));
        self.log(&format!(
            "probe: {} at ({:.0}, {:.0}): {free} free and {blocked} blocked hidden spots",
            self.defs.name_of(mex),
            mex.pos().x,
            mex.pos().z
        ));
        let Some(&(spot, free)) = pick else { return };
        let Some(unit) = self.cb.cheat_new_unit(def, spot) else { return };
        unit.set_fire_state(2, 0);
        unit.set_move_state(0, 0);
        unit.execute_custom_command(cmd::UNIT_SET_TARGET, &[mex.id() as f32], 0, frame + ROUND);
        self.precision.probing.shots.push(Shot {
            unit,
            target: mex,
            target_health: mex.health(),
            free,
            dist: distance(spot, mex.pos()),
            frame,
        });
    }

    /// Score last round's probes, and clear them away.
    fn shadow_probe_score(&mut self) {
        let probes = std::mem::take(&mut self.precision.probing.probes);
        let hits = std::mem::take(&mut self.precision.probing.hits);
        let shots = std::mem::take(&mut self.precision.probing.shots);
        for s in &shots {
            let fired = s.unit.weapons().iter().any(|w| w.reload_frame() > s.frame);
            let dealt = s.target_health - s.target.health().max(0.0);
            let hit = dealt > 1.0;
            let pos = s.unit.pos();
            self.log(&format!(
                "probe: shot at {} d={:.0} {} fired={} dealt={dealt:.0} hurt={} at ({:.0}, {:.0})",
                self.defs.name_of(s.target),
                s.dist,
                if s.free { "free" } else { "blocked" },
                fired as u8,
                hits.contains_key(&s.unit.id()) as u8,
                pos.x,
                pos.z
            ));
            let t = &mut self.precision.probing;
            if s.free {
                t.free += 1;
                t.free_hit += hit as u32;
            } else {
                t.blocked += 1;
                t.blocked_hit += hit as u32;
            }
            if s.unit.health() > 0.0 {
                s.unit.self_destruct(0);
            }
        }
        if probes.is_empty() && shots.is_empty() {
            return;
        }
        for p in &probes {
            let u = p.unit;
            let alive = u.health() > 0.0;
            let hit = !alive || u.health() < u.max_health() - 1.0 || hits.contains_key(&u.id());
            let pos = u.pos();
            let measured =
                if alive { self.precision.ground().map_or(0.0, |g| pos.y - g.stand(pos.x, pos.z)) } else { f32::NAN };
            let mut by: Vec<String> = hits.get(&u.id()).cloned().unwrap_or_default();
            by.dedup();
            self.log(&format!(
                "probe: {} d={:.0} need={:.1} aim={:.1} (measured {:.1}) {} hit={} by=[{}] at ({:.0}, {:.0}) needs=[{}]",
                p.gun,
                p.dist,
                p.need,
                p.aim,
                measured,
                if p.built { "built" } else if p.hidden { "hidden" } else { "open" },
                hit as u8,
                by.join(","),
                p.at.x,
                p.at.z,
                p.needs
            ));
            if p.hidden && hit {
                let mut guns: Vec<&str> = by.iter().filter_map(|w| w.rsplit_once('_').map(|(g, _)| g)).collect();
                guns.dedup();
                for gun in guns {
                    let name = gun.split('_').next().unwrap_or(gun);
                    for l in self.precision.profiles(name, p.at.x, p.at.z, p.aim) {
                        self.log(&format!("probe:   {l}"));
                    }
                }
            }
            let t = &mut self.precision.probing;
            if p.hidden {
                t.hidden += 1;
                t.hidden_hit += hit as u32;
                t.built += p.built as u32;
                t.built_hit += (p.built && hit) as u32;
            } else {
                t.open += 1;
                t.open_hit += hit as u32;
            }
            if alive {
                u.self_destruct(0);
            }
        }
        let t = &self.precision.probing;
        self.log(&format!(
            "probe tally: hidden {} (hit {}; by buildings only {}, hit {}), open {} (hit {}); shots free {} (hit {}), blocked {} (hit {})",
            t.hidden, t.hidden_hit, t.built, t.built_hit, t.open, t.open_hit, t.free, t.free_hit, t.blocked, t.blocked_hit
        ));
    }
}
