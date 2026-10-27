//! The floor: a minimum density of home defense, one recruit standing
//! guard per `qrf_floor` naked mexes of ours (default 4) from the first
//! mex on, before anything is lost (the autopilot's budget stays at nothing
//! until a raid has cost us something, and on Eye of Horus one or two
//! raiders through took whole flanks of jumpjet and spider economies).
//!
//! Our mexes without turret cover are cut into sectors of up to that many
//! neighbours, the most exposed first (travel time from where raids come
//! from, see `qrf_mark_sources`), and the most exposed sectors get a guard
//! each: a team of one with a zone of its own (see `RoleKind::Zone`). It
//! waits ahead of its sector's first mex on the road from the enemy's side,
//! front towards the enemy, to meet a raid at that mex rather than in the
//! sector's middle, and answers only raids on its sector, by the mark
//! algorithm (goalkeep, strike when clearly stronger): one guard holds a
//! lone raider and buys time against two.
//!
//! Guards are the floor's own: the first recruit-class units out of the
//! factories while a sector is unguarded, raiding Venoms included (the
//! spider factory's raider phase makes them for home defense, see
//! `fac_spider_raider`), and, unguarded a while, ordered in place of a
//! plain fighter (whatever `qrf_production` says). They are not counted in
//! the autopilot's budget, which runs as before. Feature `garrison` (off by
//! default).

use std::collections::BTreeMap;

use serde_json::{Value, json};
use springai::{Unit, Vec3};

use super::Member;
use super::field::Mobility;
use super::live::{Role, RoleKind, Tactic, is_recruit};
use crate::Ai;
use crate::military::Fighter;
use crate::util::distance;

/// Naked mexes per guard, unless `qrf_floor` says otherwise.
const PER: usize = 4;
/// At most this many guards.
const MAX_GUARDS: usize = 8;
/// Sectors are re-cut this often (frames).
const RESITE: i32 = 30 * 15;
/// A mex joins a sector within this of its first mex (elmos).
const REACH: f32 = 1200.0;
/// The post stands this far ahead of the first mex, along the road raids
/// take to it (elmos): about a raider's reach.
const AHEAD: f32 = 200.0;
/// A guard's zone: its sector's mexes, and this much more (elmos).
const ZONE_MARGIN: f32 = 300.0;
/// Unguarded this long, the factories build guards (frames); an order
/// counts as on its way this long.
const SHORT_FOR: i32 = 30 * 10;
const ORDER_PENDING: i32 = 30 * 60;
/// Raid routes are measured for a bot this fast (elmos per second).
const RAID_SPEED: f32 = 100.0;

#[derive(Clone, Debug)]
pub struct Sector {
    /// Its most exposed mex.
    pub front: Vec3,
    pub post: Vec3,
    pub radius: f32,
    pub mexes: usize,
}

#[derive(Default)]
pub struct Floor {
    pub on: bool,
    pub per: usize,
    /// Guarded sectors, most exposed first.
    pub sectors: Vec<Sector>,
    /// Floor teams and their sectors.
    pub guards: BTreeMap<usize, usize>,
    pub sited: i32,
    pub naked: usize,
    pub pending: Vec<(i32, f32)>,
    pub short_since: Option<i32>,
    pub recruited: u32,
    pub ordered: u32,
    pub released: u32,
}

impl Floor {
    fn unguarded(&self) -> usize {
        self.sectors.len().saturating_sub(self.guards.len())
    }
}

impl Ai {
    pub(super) fn qrf_floor_start(&mut self) {
        let on = !self.dev_off("garrison");
        let per = (self.cb.option("qrf_floor").filter(|v| !v.trim().is_empty()))
            .or_else(|| std::env::var("ZKGBAI_QRF_FLOOR").ok())
            .and_then(|v| v.trim().parse::<usize>().ok())
            .unwrap_or(PER)
            .max(1);
        self.qrf.floor = Floor { on, per, sited: -RESITE, ..Default::default() };
        if on {
            self.log(&format!("qrf: floor on, a guard per {per} naked mexes"));
        }
    }

    /// Re-cut the sectors now and then, keep the guards on them, and note
    /// when the floor is short.
    pub(super) fn qrf_floor_update(&mut self, frame: i32) {
        if !self.qrf.floor.on {
            return;
        }
        // Teams whose members all died are gone.
        let members = &self.qrf.members;
        let gone: Vec<usize> =
            (self.qrf.floor.guards.keys()).filter(|t| !members.values().any(|m| m.team == **t)).copied().collect();
        for t in gone {
            self.qrf.floor.guards.remove(&t);
            self.qrf.roles.remove(&t);
        }
        if frame - self.qrf.floor.sited >= RESITE {
            self.qrf.floor.sited = frame;
            self.qrf_floor_site();
            self.qrf_floor_assign();
        }
        let f = &mut self.qrf.floor;
        f.pending.retain(|p| frame - p.0 < ORDER_PENDING);
        if f.unguarded() > f.pending.len() {
            f.short_since.get_or_insert(frame);
        } else {
            f.short_since = None;
        }
    }

    /// Our naked mexes in sectors, the most exposed first, as many as the
    /// floor guards.
    fn qrf_floor_site(&mut self) {
        let naked: Vec<Vec3> = (self.qrf.assets.iter())
            .filter(|a| a.is_mex && self.war.get_friendly_porc_threat(a.pos) <= 0.0)
            .map(|a| a.pos)
            .collect();
        self.qrf.floor.naked = naked.len();
        let want = naked.len().div_ceil(self.qrf.floor.per).min(MAX_GUARDS);
        if want == 0 {
            self.qrf.floor.sectors.clear();
            return;
        }
        let sources = self.qrf_mark_sources();
        let Some(grid) = self.qrf.grid.as_ref() else { return };
        let starts: Vec<(Vec3, f32)> = sources.iter().map(|&s| (s, 0.0)).collect();
        let field = grid.field(Mobility::BOT, RAID_SPEED, &starts);
        let nearest_source =
            |p: Vec3| (sources.iter().copied()).min_by(|a, b| distance(*a, p).total_cmp(&distance(*b, p))).unwrap_or(p);
        // Seconds from the raids' side: by road, else as the crow flies.
        let exposure = |p: Vec3| {
            let t = field.at(p);
            if t.is_finite() { t } else { distance(nearest_source(p), p) / RAID_SPEED }
        };
        let mut left: Vec<(Vec3, f32)> = naked.iter().map(|&p| (p, exposure(p))).collect();
        left.sort_by(|a, b| a.1.total_cmp(&b.1));
        let mut sectors = Vec::new();
        while !left.is_empty() && sectors.len() < want {
            let (front, _) = left.remove(0);
            let mut near: Vec<usize> = (0..left.len()).filter(|&i| distance(left[i].0, front) <= REACH).collect();
            near.sort_by(|&a, &b| distance(left[a].0, front).total_cmp(&distance(left[b].0, front)));
            near.truncate(self.qrf.floor.per - 1);
            near.sort_unstable_by(|a, b| b.cmp(a));
            let mut mexes = vec![front];
            for i in near {
                mexes.push(left.remove(i).0);
            }
            // Ahead of the first mex on the road back to where raids come
            // from; straight towards it where there is no road.
            let path = field.descent(front, 64);
            let mut post = None;
            for &c in &path {
                let p = grid.centre(c);
                if distance(p, front) >= AHEAD {
                    post = Some(p);
                    break;
                }
            }
            let post = post.unwrap_or_else(|| {
                let e = nearest_source(front);
                let d = distance(e, front).max(1.0);
                front + (e - front) * (AHEAD.min(d * 0.5) / d)
            });
            let radius = mexes.iter().map(|&m| distance(m, post)).fold(0.0, f32::max) + ZONE_MARGIN;
            sectors.push(Sector { front, post, radius, mexes: mexes.len() });
        }
        self.qrf.floor.sectors = sectors;
        let posts: Vec<String> =
            (self.qrf.floor.sectors.iter()).map(|s| format!("{:.0},{:.0}x{}", s.post.x, s.post.z, s.mexes)).collect();
        let line = format!(
            "qrffloor: t={} naked={} sectors={} guards={} posts=[{}]",
            self.frame / 30,
            self.qrf.floor.naked,
            self.qrf.floor.sectors.len(),
            self.qrf.floor.guards.len(),
            posts.join(" ")
        );
        self.log(&line);
    }

    /// Each guard to the nearest sector still unguarded, the most exposed
    /// first; guards with no sector left go back to the usual handlers.
    fn qrf_floor_assign(&mut self) {
        let mut free: Vec<(usize, Vec3)> = (self.qrf.floor.guards.keys())
            .map(|&t| {
                let at = (self.qrf.members.values().find(|m| m.team == t)).map_or(Vec3::ZERO, Member::pos);
                (t, at)
            })
            .collect();
        let mut guards = BTreeMap::new();
        for (i, s) in self.qrf.floor.sectors.iter().enumerate() {
            let Some(k) =
                (0..free.len()).min_by(|&a, &b| distance(free[a].1, s.post).total_cmp(&distance(free[b].1, s.post)))
            else {
                break;
            };
            guards.insert(free.remove(k).0, i);
        }
        for (t, _) in free {
            self.qrf.floor.guards.remove(&t);
            self.qrf_release_team(t, "floor: no sector left");
            self.qrf.floor.released += 1;
        }
        for (&t, &i) in &guards {
            let s = &self.qrf.floor.sectors[i];
            self.qrf
                .roles
                .insert(t, Role { kind: RoleKind::Zone, pos: s.post, radius: s.radius, tactic: Tactic::Mark });
        }
        self.qrf.floor.guards = guards;
    }

    /// Whether the floor would take `unit` now (see [`Ai::qrf_floor_take`]).
    pub(crate) fn qrf_floor_can_take(&self, unit: Unit) -> bool {
        let q = &self.qrf;
        q.on && q.live
            && !q.releasing
            && q.floor.on
            && q.floor.unguarded() > 0
            && unit.def().is_some_and(|d| is_recruit(self.defs.name(d)) && Mobility::of(d).is_some())
    }

    /// Take a recruit-class unit out of the factory as the guard of an
    /// unguarded sector (the one nearest it).
    pub(crate) fn qrf_floor_take(&mut self, unit: Unit) -> bool {
        let f = &self.qrf.floor;
        if !f.on || f.unguarded() == 0 {
            return false;
        }
        let Some(def) = unit.def() else { return false };
        if !is_recruit(self.defs.name(def)) {
            return false;
        }
        let Some(mobility) = Mobility::of(def) else { return false };
        let taken: Vec<usize> = f.guards.values().copied().collect();
        let pos = unit.pos();
        let Some(i) = (0..f.sectors.len())
            .filter(|i| !taken.contains(i))
            .min_by(|&a, &b| distance(f.sectors[a].post, pos).total_cmp(&distance(f.sectors[b].post, pos)))
        else {
            return false;
        };
        let cost = def.cost(self.metal);
        let ordered = f.pending.iter().position(|p| (p.1 - cost).abs() < 1.0);
        if let Some(k) = ordered {
            self.qrf.floor.pending.remove(k);
        }
        let team = self.qrf.next_team;
        self.qrf.next_team += 1;
        let threat = Fighter::new(unit, cost, self.team_id).power;
        self.qrf.members.insert(
            unit.id(),
            Member {
                unit,
                def,
                team,
                cost,
                speed: def.speed(),
                mobility,
                range: def.max_weapon_range(),
                squad: None,
                threat,
                order: None,
            },
        );
        let s = &self.qrf.floor.sectors[i];
        self.qrf.roles.insert(team, Role { kind: RoleKind::Zone, pos: s.post, radius: s.radius, tactic: Tactic::Mark });
        self.qrf.floor.guards.insert(team, i);
        self.qrf.floor.recruited += 1;
        unit.set_move_state(1, 0);
        unit.set_fire_state(2, 0);
        true
    }

    /// A guard from a factory about to build `choice` (a plain fighter),
    /// if a sector has gone unguarded a while.
    pub(super) fn qrf_floor_production(&mut self, factory: Unit, choice: &str) -> Option<&'static str> {
        let f = &self.qrf.floor;
        if !f.on || f.short_since.is_none_or(|s| self.frame - s < SHORT_FOR) || is_recruit(choice) {
            return None;
        }
        if f.unguarded() <= f.pending.len() {
            return None;
        }
        let name = self.qrf_pick(factory)?;
        let cost = self.defs.get(name)?.cost(self.metal);
        let frame = self.frame;
        self.qrf.floor.pending.push((frame, cost));
        self.qrf.floor.ordered += 1;
        Some(name)
    }

    /// Whether a team is a floor guard.
    pub fn qrf_floor_guard(&self, team: usize) -> bool {
        self.qrf.floor.on && self.qrf.floor.guards.contains_key(&team)
    }

    /// The floor's part of the QRF's journal record.
    pub(super) fn qrf_floor_json(&self) -> Value {
        let f = &self.qrf.floor;
        if !f.on {
            return Value::Null;
        }
        let power: f32 =
            (self.qrf.members.values()).filter(|m| f.guards.contains_key(&m.team)).map(Member::power).sum();
        let posts: Vec<Value> = (f.sectors.iter())
            .map(|s| json!([s.post.x.round(), s.post.z.round(), s.radius.round(), s.mexes]))
            .collect();
        json!({
            "per": f.per, "naked": f.naked, "sectors": f.sectors.len(), "guards": f.guards.len(), "power": power.round(),
            "pending": f.pending.len(), "recruited": f.recruited, "ordered": f.ordered, "released": f.released, "posts": posts,
        })
    }
}
