//! The floor: a minimum density of home defense, one recruit standing
//! guard per `qrf_floor` naked mexes of ours (default 4) from the first
//! mex on, before anything is lost (the autopilot's budget stays at nothing
//! until a raid has cost us something, and on Eye of Horus one or two
//! raiders through took whole flanks of jumpjet and spider economies).
//!
//! The guards stand on the rim of our territory, not inside it: the rim is
//! the convex hull of our mexes and constructors (commanders aside, and
//! constructors only near a mex of ours), so it grows as the constructors
//! walk out to expand. Naked mexes and constructors on that rim, facing
//! the raids (the road from where raids come from, see `qrf_mark_sources`,
//! comes in from outside, not across our land from behind as on a rim
//! along a map edge), are the places to guard, the most exposed (by that
//! road's travel time) first, each post at least `SPACING` from the others
//! (closer only if there are more guards than room). A guard
//! waits ahead of its place on the road from the enemy's side, front
//! towards the enemy, to meet a raid there rather than among the mexes
//! behind, and answers only raids on its zone (see `RoleKind::Zone`), by
//! the mark algorithm (goalkeep, strike when clearly stronger): one guard
//! holds a lone raider and buys time against two. A guard is a few units
//! at most, so the posts are re-sited every few seconds and follow the rim
//! as it moves.
//!
//! Guards are the floor's own: the first recruit-class units out of the
//! factories while a post is unguarded, raiding Venoms included (the
//! spider factory's raider phase makes them for home defense, see
//! `fac_spider_raider`), and, unguarded a while, ordered in place of a
//! plain fighter (whatever `qrf_production` says). They are not counted in
//! the autopilot's budget, which runs as before. Feature `garrison`, on by
//! default: on Eye of Horus against Circuit (zkgbai-runs/garrison, ab2, 40
//! games an arm over four rosters, with posts spread over every naked
//! mex) about three mexes fewer lost by 20:00 and half the flank wipes, at
//! no cost measured in income, army or results; and in play it holds
//! territory and a front as a strong human does.

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
/// Posts are re-sited this often (frames); the log line comes at most
/// this often unless the posts change in number.
const RESITE: i32 = 30 * 5;
const LOG_EVERY: i32 = 30 * 15;
/// A constructor is part of our territory within this of a mex of ours
/// (elmos): out expanding, not walking off into the enemy's land.
const LEASH: f32 = 1200.0;
/// On the rim: this close to the hull's boundary (elmos).
const RIM: f32 = 450.0;
/// Facing the raids: the post is at most this much deeper inside the hull
/// than its place (elmos): the road comes in from outside, or along the
/// rim, not across our territory.
const INWARD: f32 = 100.0;
/// Posts this far apart (elmos), closer (down to `MIN_SPACING`) only when
/// the rim has no room for every guard otherwise.
const SPACING: f32 = 900.0;
const MIN_SPACING: f32 = 300.0;
/// The post stands this far ahead of the first mex, along the road raids
/// take to it (elmos): about a raider's reach.
const AHEAD: f32 = 200.0;
/// A guard's zone: the rim's places nearest its post, those within `ZONE`
/// of it, and this much more (elmos).
const ZONE: f32 = 1500.0;
const ZONE_MARGIN: f32 = 300.0;
/// Unguarded this long, the factories build guards (frames); an order
/// counts as on its way this long.
const SHORT_FOR: i32 = 30 * 10;
const ORDER_PENDING: i32 = 30 * 60;
/// Raid routes are measured for a bot this fast (elmos per second).
const RAID_SPEED: f32 = 100.0;

#[derive(Clone, Debug)]
pub struct Sector {
    /// The place on the rim it guards (a naked mex or a constructor).
    pub front: Vec3,
    pub post: Vec3,
    pub radius: f32,
    /// The rim's naked mexes and constructors nearest this post.
    pub mexes: usize,
    pub cons: usize,
}

/// A naked mex or constructor on the rim.
struct Place {
    pos: Vec3,
    mex: bool,
    /// Seconds from the raids' side.
    exposure: f32,
    post: Vec3,
    facing: bool,
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
    pub logged: i32,
    pub naked: usize,
    /// Naked mexes and constructors on the rim, and of those, the ones
    /// facing the raids.
    pub rim: usize,
    pub facing: usize,
    /// Where the floor takes raids to come from.
    pub sources: Vec<Vec3>,
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
        self.qrf.floor = Floor { on, per, sited: -RESITE, logged: -LOG_EVERY, ..Default::default() };
        if on {
            self.log(&format!("qrf: floor on, a guard per {per} naked mexes"));
        }
    }

    /// Re-site the posts every few seconds, keep the guards on them, and
    /// note when the floor is short.
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

    /// The posts: the naked mexes and constructors on the rim of our
    /// territory, the most exposed first and spaced apart, as many as the
    /// floor guards (doubled up at the most exposed when the rim is short
    /// of places).
    fn qrf_floor_site(&mut self) {
        let covered = |ai: &Ai, p: Vec3| ai.war.get_friendly_porc_threat(p) > 0.0;
        let mexes: Vec<Vec3> = (self.qrf.assets.iter()).filter(|a| a.is_mex).map(|a| a.pos).collect();
        let naked = mexes.iter().filter(|&&p| !covered(self, p)).count();
        self.qrf.floor.naked = naked;
        // `frontretire`: no garrison once the fights are dense (see
        // `military::front`); the guards go back (`qrf_floor_assign`).
        let want = if self.front_retired() { 0 } else { naked.div_ceil(self.qrf.floor.per).min(MAX_GUARDS) };
        if want == 0 {
            self.qrf.floor.sectors.clear();
            self.qrf.floor.rim = 0;
            self.qrf.floor.facing = 0;
            return;
        }
        // Our constructors out near our mexes: the rim grows where they
        // expand. Not the commander: it goes where the fighting is, and
        // has guards of its own (`eco_defend_commanders`).
        let cons: Vec<Vec3> = (self.qrf.assets.iter())
            .filter(|a| !a.is_mex && a.unit.def().is_some_and(|d| d.speed() > 0.0 && !self.defs.is_commander(d)))
            .map(|a| a.pos)
            .filter(|&p| mexes.iter().any(|&m| distance(m, p) <= LEASH))
            .collect();
        let points: Vec<Vec3> = mexes.iter().chain(&cons).copied().collect();
        let hull = convex_hull(&points);
        let places: Vec<(Vec3, bool)> = (mexes.iter().map(|&p| (p, true)))
            .chain(cons.iter().map(|&p| (p, false)))
            .filter(|&(p, _)| rim_depth(&hull, p) <= RIM && !covered(self, p))
            .collect();
        self.qrf.floor.rim = places.len();
        // Where raids come from: the enemy's side, and where raids were
        // first seen outside our territory (one first seen inside it came
        // in unseen, and the road back to there runs across our land).
        let mut sources: Vec<Vec3> =
            (self.qrf.mark.entries.iter()).copied().filter(|&e| signed_depth(&hull, e) < 0.0).collect();
        sources.push(self.qrf_enemy_side());
        self.qrf.floor.sources = sources.clone();
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
        // Ahead of a place on the road back to where raids come from;
        // straight towards there where there is no road.
        let post_of = |front: Vec3| {
            let path = field.descent(front, 64);
            (path.iter().map(|&c| grid.centre(c))).find(|&p| distance(p, front) >= AHEAD).unwrap_or_else(|| {
                let e = nearest_source(front);
                let d = distance(e, front).max(1.0);
                front + (e - front) * (AHEAD.min(d * 0.5) / d)
            })
        };
        let mut places: Vec<Place> = (places.into_iter())
            .map(|(pos, mex)| {
                let post = post_of(pos);
                // Facing the raids: their road comes in from outside our
                // territory (the post no deeper in the hull than the place),
                // not across it from the back, as on the rim along a map
                // edge behind us.
                let facing = signed_depth(&hull, post) <= signed_depth(&hull, pos) + INWARD;
                Place { pos, mex, exposure: exposure(pos), post, facing }
            })
            .collect();
        places.sort_by(|a, b| a.exposure.total_cmp(&b.exposure));
        let mut candidates: Vec<usize> = (0..places.len()).filter(|&i| places[i].facing).collect();
        self.qrf.floor.facing = candidates.len();
        if candidates.is_empty() {
            // None for now (a constructor or two walking about can do
            // that): the posts stay where they are, else the whole rim.
            if !self.qrf.floor.sectors.is_empty() {
                return;
            }
            candidates = (0..places.len()).collect();
        }
        let at: Vec<Vec3> = candidates.iter().map(|&i| places[i].pos).collect();
        let mut sectors: Vec<Sector> = (spread(&at, want).into_iter())
            .map(|j| {
                let p = &places[candidates[j]];
                Sector { front: p.pos, post: p.post, radius: AHEAD, mexes: 0, cons: 0 }
            })
            .collect();
        // Each place on the rim in the zone of the post nearest it, within
        // reach.
        for p in &places {
            let Some(k) = (0..sectors.len())
                .min_by(|&a, &b| distance(sectors[a].front, p.pos).total_cmp(&distance(sectors[b].front, p.pos)))
            else {
                break;
            };
            let s = &mut sectors[k];
            let d = distance(s.post, p.pos);
            if d > ZONE {
                continue;
            }
            s.radius = s.radius.max(d);
            if p.mex {
                s.mexes += 1;
            } else {
                s.cons += 1;
            }
        }
        for s in &mut sectors {
            s.radius += ZONE_MARGIN;
        }
        // More guards than places: the spare ones double up, the most
        // exposed post first.
        let k = sectors.len();
        if k > 0 {
            for i in 0..want.saturating_sub(k) {
                sectors.push(sectors[i % k].clone());
            }
        }
        let changed = sectors.len() != self.qrf.floor.sectors.len();
        self.qrf.floor.sectors = sectors;
        if !changed && self.frame - self.qrf.floor.logged < LOG_EVERY {
            return;
        }
        self.qrf.floor.logged = self.frame;
        let posts: Vec<String> = (self.qrf.floor.sectors.iter())
            .map(|s| format!("{:.0},{:.0}x{}+{}", s.post.x, s.post.z, s.mexes, s.cons))
            .collect();
        let line = format!(
            "qrffloor: t={} naked={} rim={} facing={} sectors={} guards={} posts=[{}]",
            self.frame / 30,
            self.qrf.floor.naked,
            self.qrf.floor.rim,
            self.qrf.floor.facing,
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
            .map(|s| json!([s.post.x.round(), s.post.z.round(), s.radius.round(), s.mexes, s.cons]))
            .collect();
        let sources: Vec<Value> = f.sources.iter().map(|p| json!([p.x.round(), p.z.round()])).collect();
        json!({
            "per": f.per, "naked": f.naked, "rim": f.rim, "facing": f.facing, "sectors": f.sectors.len(), "guards": f.guards.len(), "power": power.round(),
            "pending": f.pending.len(), "recruited": f.recruited, "ordered": f.ordered, "released": f.released, "posts": posts,
            "sources": sources,
        })
    }
}

/// The convex hull of `points` on the map (x, z), counter-clockwise, with
/// no collinear points; one or two points if that is all there is.
fn convex_hull(points: &[Vec3]) -> Vec<Vec3> {
    let mut p: Vec<Vec3> = points.to_vec();
    p.sort_by(|a, b| a.x.total_cmp(&b.x).then(a.z.total_cmp(&b.z)));
    p.dedup_by(|a, b| a.x == b.x && a.z == b.z);
    if p.len() < 3 {
        return p;
    }
    let cross = |o: Vec3, a: Vec3, b: Vec3| (a.x - o.x) * (b.z - o.z) - (a.z - o.z) * (b.x - o.x);
    let chain = |pts: &mut dyn Iterator<Item = &Vec3>| {
        let mut h: Vec<Vec3> = Vec::new();
        for &q in pts {
            while h.len() >= 2 && cross(h[h.len() - 2], h[h.len() - 1], q) <= 0.0 {
                h.pop();
            }
            h.push(q);
        }
        h.pop();
        h
    };
    let mut hull = chain(&mut p.iter());
    hull.extend(chain(&mut p.iter().rev()));
    hull
}

/// How far `p` is from the boundary of `hull` (elmos): 0 on it, and for a
/// point inside, the way to the nearest edge.
fn rim_depth(hull: &[Vec3], p: Vec3) -> f32 {
    match hull.len() {
        0 => 0.0,
        1 => distance(hull[0], p),
        n => (0..n).map(|i| segment_distance(hull[i], hull[(i + 1) % n], p)).fold(f32::INFINITY, f32::min),
    }
}

/// [`rim_depth`] inside the hull, less than nothing outside it.
fn signed_depth(hull: &[Vec3], p: Vec3) -> f32 {
    let d = rim_depth(hull, p);
    if inside(hull, p) { d } else { -d }
}

/// Whether `p` is inside a counter-clockwise hull (of three or more
/// points), boundary included.
fn inside(hull: &[Vec3], p: Vec3) -> bool {
    let n = hull.len();
    n >= 3
        && (0..n).all(|i| {
            let (a, b) = (hull[i], hull[(i + 1) % n]);
            (b.x - a.x) * (p.z - a.z) - (b.z - a.z) * (p.x - a.x) >= 0.0
        })
}

fn segment_distance(a: Vec3, b: Vec3, p: Vec3) -> f32 {
    let (dx, dz) = (b.x - a.x, b.z - a.z);
    let len2 = dx * dx + dz * dz;
    let t = if len2 > 0.0 { (((p.x - a.x) * dx + (p.z - a.z) * dz) / len2).clamp(0.0, 1.0) } else { 0.0 };
    distance(Vec3::xz(a.x + t * dx, a.z + t * dz), p)
}

/// Up to `want` of `places` (the most exposed first), each at least
/// `SPACING` from the others, closer (halving, down to `MIN_SPACING`) only
/// when there is no room otherwise; indices, in the order of `places`.
fn spread(places: &[Vec3], want: usize) -> Vec<usize> {
    let mut picked: Vec<usize> = Vec::new();
    let mut spacing = SPACING;
    while picked.len() < want.min(places.len()) {
        let next = (0..places.len())
            .find(|i| !picked.contains(i) && picked.iter().all(|&j| distance(places[*i], places[j]) >= spacing));
        match next {
            Some(i) => picked.push(i),
            None if spacing > MIN_SPACING => spacing = (spacing * 0.5).max(MIN_SPACING),
            None => break,
        }
    }
    picked.sort_unstable();
    picked
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(x: f32, z: f32) -> Vec3 {
        Vec3::xz(x, z)
    }

    #[test]
    fn hull_drops_the_inside_and_the_collinear() {
        let pts =
            [at(0.0, 0.0), at(1000.0, 0.0), at(1000.0, 1000.0), at(0.0, 1000.0), at(500.0, 500.0), at(500.0, 0.0)];
        let h = convex_hull(&pts);
        assert_eq!(h.len(), 4);
        assert!(!h.iter().any(|p| p.x == 500.0));
    }

    #[test]
    fn rim_depth_is_zero_on_the_boundary_and_grows_inside() {
        let pts = [at(0.0, 0.0), at(2000.0, 0.0), at(2000.0, 2000.0), at(0.0, 2000.0)];
        let h = convex_hull(&pts);
        assert!(rim_depth(&h, at(2000.0, 0.0)) < 1e-3);
        assert!(rim_depth(&h, at(1000.0, 0.0)) < 1e-3);
        assert!((rim_depth(&h, at(1000.0, 1000.0)) - 1000.0).abs() < 1e-3);
        assert!((rim_depth(&h, at(300.0, 1000.0)) - 300.0).abs() < 1e-3);
    }

    #[test]
    fn signed_depth_is_negative_outside() {
        let h = convex_hull(&[at(0.0, 0.0), at(2000.0, 0.0), at(2000.0, 2000.0), at(0.0, 2000.0)]);
        assert!((signed_depth(&h, at(1000.0, 300.0)) - 300.0).abs() < 1e-3);
        assert!((signed_depth(&h, at(1000.0, -200.0)) + 200.0).abs() < 1e-3);
        assert!(signed_depth(&[at(0.0, 0.0)], at(100.0, 0.0)) < 0.0);
    }

    #[test]
    fn degenerate_hulls() {
        assert_eq!(convex_hull(&[at(5.0, 5.0), at(5.0, 5.0)]).len(), 1);
        let line = convex_hull(&[at(0.0, 0.0), at(500.0, 0.0), at(1000.0, 0.0)]);
        assert_eq!(line.len(), 2);
        assert!(rim_depth(&line, at(500.0, 0.0)) < 1e-3);
    }

    #[test]
    fn spread_keeps_posts_apart_and_halves_when_short() {
        // Exposure order: three close together, one far.
        let places = [at(0.0, 0.0), at(200.0, 0.0), at(400.0, 0.0), at(2000.0, 0.0)];
        assert_eq!(spread(&places, 2), vec![0, 3]);
        // A third post only once the spacing is down to the minimum.
        assert_eq!(spread(&places, 3), vec![0, 2, 3]);
        // Never closer than the minimum.
        assert_eq!(spread(&places, 4), vec![0, 2, 3]);
        assert_eq!(spread(&places, 1), vec![0]);
    }
}
