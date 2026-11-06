//! Asset ambushes: mines follow expansion and intercept enemy approaches on
//! the victim's movement tier. Gate fields remain fallback opportunities.
//! Production scales with distinct observed clumps, including orders in flight.
//! Assignments, observation memory and deployment deadlines survive checkpoints.
//!
//! `bombcharge` (on by default): an Imp or Snitch with enemies near asks
//! the fight predictor whether running at them beats sitting tight
//! ([`crate::military::fightsim::Model::bomb_charge`]): busy, inaccurate
//! skirmishers are worth a charge, a Mace driving by is not.

use serde_json::json;
use springai::{NO_TIMEOUT, Unit, UnitDef, Vec3};
use std::collections::BTreeMap;

mod ambush;
use ambush::*;
use zk_terrain::paths::Paths;

use crate::Ai;
use crate::defs::cmd;
use crate::kgbutil::kgb_move_to_exact;
use crate::military::fightsim::local::our_state;
use crate::military::fightsim::{FightSnapshot, SimFlags, SnapUnit};
use crate::terrain::{pos, tier_of};
use crate::util::{distance, get_directional_point};

const PERIOD: i32 = 90;
const ARRIVED: f32 = 16.0;
// Covers arrival tolerance at both ends and the receiving unit's footprint.
const CLEARANCE: f32 = 64.0;
/// How much of an Imp's blast radius beyond the mine (its enemy side,
/// where whoever sets it off and those behind stand) a supporting turret
/// must still reach.
const COVER: f32 = 0.75;
/// A supporting turret's mine lies within this angle (cosine: 60 degrees)
/// of the enemy's approach to it. New ones go straight up the approach, so
/// a drifting enemy centre does not walk settled mines away at once.
const UPSTREAM_COS: f32 = 0.5;
/// How far ahead of a front metal spot its minefield lies.
const SPOT_LEAD: f32 = 200.0;
/// The enemy's way in through a gate is read this far outside its
/// crossing, over the last `GATE_LOOKBACK` elmos of their route there.
const GATE_LOOKOUT: f32 = 64.0;
const GATE_LOOKBACK: f32 = 200.0;
/// How long a mobile enemy's last sighting still counts as its eyes there.
const SIGHT_MEMORY: i32 = 300;
/// Sight assumed for a contact not yet identified.
const UNKNOWN_SIGHT: f32 = 450.0;
const RESERVE_MINES: usize = 8;
const CLUMP_UNITS: usize = 2;
const CLUMP_RADIUS: f32 = 300.0;
const CLUMP_MEMORY: i32 = 30 * 10;

#[derive(Default)]
pub(super) struct MineReserve {
    /// Distinct recent squads; repeated sightings replace their members.
    clumps: Vec<Clump>,
    /// Factory -> (unit def, order frame), until its nanoframe is created.
    orders: BTreeMap<i32, (i32, i32)>,
}

fn mine_type(name: &str) -> bool {
    matches!(name, "cloakbomb" | "shieldbomb")
}

/// `bombcharge`: an Imp or Snitch judges a charge once an enemy on the
/// ground comes this near (a run of 3-4 s)...
const CHARGE_LOOK: f32 = 450.0;
/// ...on the fight of ours and theirs within this of it...
const CHARGE_REACH: f32 = 800.0;
/// ...over this many frames.
const CHARGE_HORIZON: i32 = 450;
/// What a charge must be worth over sitting tight, net of the bomb (metal).
const CHARGE_MARGIN: f32 = 25.0;
/// A charging bomb is left to its order this long, then planned again.
const CHARGE_HOLD: i32 = 30 * 8;
/// A bomb's sit-tight verdicts are logged this often.
const CHARGE_LOG: i32 = 150;
/// Enemy mobiles stand where they were seen this recently.
const CHARGE_SEEN: i32 = 45;
/// Bigger fights are not judged: a judgement is nine plane fights, about
/// 0.4 ms at 8 units, 1.4 at 18, 3 at 28, 6 at 40 (bomb_charge_cost).
const CHARGE_MAX_UNITS: usize = 32;
/// Judgements per 15-frame tick, over all bombs...
const CHARGE_PER_TICK: usize = 2;
/// ...and a bomb that sat tight is judged again after this many frames.
const CHARGE_EVERY: i32 = 30;
/// An armed unit with a foe this far inside its range is busy with it.
const BUSY_SLACK: f32 = 50.0;

#[derive(Clone, Copy)]
struct Site {
    position: Vec3,
    gate: Vec3,
    value: f32,
    depth: usize,
    /// A route to a real friendly asset, rather than a fallback gate.
    ambush: bool,
}

#[derive(Clone, Copy)]
pub(super) struct Sapper {
    pub unit: Unit,
    radius: f32,
    imp: bool,
    snitch: bool,
    site: Option<Site>,
    /// Has stood on a site: moving now would be relocating.
    placed: bool,
    /// Kept still last pass, watched by the enemy.
    held: bool,
    /// When it was sent charging (`bombcharge`).
    charging: Option<i32>,
    /// When its last sit-tight verdict was logged.
    logged: i32,
    /// When it may be judged again.
    next_judge: i32,
    deployment: Deployment,
}

#[derive(Clone, Copy)]
struct Reservation {
    id: i32,
    position: Vec3,
    radius: f32,
    site: Option<Site>,
}

/// A line across the enemy's approach to lay a minefield on.
#[derive(Clone, Copy)]
struct Field {
    /// Middle of the front row.
    center: Vec3,
    /// Unit vector towards where the enemy comes from.
    toward: Vec3,
    width: f32,
    /// Groups sites, so mines spread across fields first.
    gate: Vec3,
    value: f32,
}

/// A completed turret that can finish what an Imp stuns.
#[derive(Clone, Copy)]
struct Finisher {
    position: Vec3,
    range: f32,
    /// Half its footprint's diagonal.
    footprint: f32,
    /// Unit vector towards where the enemy comes from.
    toward: Vec3,
}

impl Finisher {
    /// The nearest a mine with this blast radius may wait: the blast stops
    /// short of the turret's footprint, with arrival tolerance to spare.
    fn standoff(&self, radius: f32) -> f32 {
        radius + self.footprint + CLEARANCE
    }

    /// Whether a mine at `p` waits on the enemy's way to this turret, its
    /// blast short of it, near enough that the turret fires on what the
    /// blast stuns.
    fn covers(&self, p: Vec3, radius: f32) -> bool {
        let (dx, dz) = (p.x - self.position.x, p.z - self.position.z);
        let d = dx.hypot(dz);
        d >= self.standoff(radius)
            && d + COVER * radius <= self.range
            && dx * self.toward.x + dz * self.toward.z >= UPSTREAM_COS * d
    }

    /// Where a mine with this blast radius waits: straight up the enemy's
    /// approach, as near as the blast allows; `None` if the turret is too
    /// short-ranged to cover any place its blast spares.
    fn site(&self, radius: f32) -> Option<Vec3> {
        // A hair past the standoff, so rounding fails no clearance check.
        let d = self.standoff(radius) + 1.0;
        let p = Vec3::xz(self.position.x + self.toward.x * d, self.position.z + self.toward.z * d);
        self.covers(p, radius).then_some(p)
    }
}

/// Where enemy ground forces come from, seen from any position: the last
/// stretch of their route from their centre on the movement tier of their
/// ground army; straight from it before the terrain analysis.
struct Approach<'a> {
    /// `None` while no enemy territory is known.
    enemy: Option<Vec3>,
    routes: Option<Paths<'a>>,
}

impl Approach<'_> {
    /// Unit vector from `p` towards where the enemy comes from, judged
    /// over the last `lookback` elmos of their way there.
    fn toward(&self, p: Vec3, lookback: f32) -> Option<Vec3> {
        let enemy = self.enemy?;
        let waypoint = self.routes.as_ref().and_then(|r| r.approach(pos(p), lookback));
        unit_toward(p, waypoint.map_or(enemy, |w| Vec3::xz(w.x, w.z)))
    }
}

/// Unit vector from `from` to `to`; `None` if they (nearly) coincide.
fn unit_toward(from: Vec3, to: Vec3) -> Option<Vec3> {
    let (dx, dz) = (to.x - from.x, to.z - from.z);
    let d = dx.hypot(dz);
    (d >= 1.0).then(|| Vec3::xz(dx / d, dz / d))
}

/// A friendly building a blast must spare.
#[derive(Clone, Copy)]
struct Building {
    center: Vec3,
    /// Half its footprint's diagonal.
    footprint: f32,
}

/// Half a footprint's diagonal (footprint sizes are in 8-elmo squares).
fn footprint(def: UnitDef) -> f32 {
    4.0 * (def.x_size() as f32).hypot(def.z_size() as f32)
}

fn blast_radius(def: UnitDef) -> f32 {
    // Bomb explosions are not mounted weapons. Read both so a killed mine
    // is just as safe for its neighbours as one that triggers normally.
    [def.death_explosion(), def.self_d_explosion()]
        .into_iter()
        .flatten()
        .map(|w| w.area_of_effect())
        .fold(0.0, f32::max)
}

fn configure(unit: Unit) {
    unit.set_move_state(0, 0);
    unit.set_fire_state(2, 0);
    unit.execute_custom_command(cmd::UNIT_AI, &[0.0], 0, NO_TIMEOUT);
}

/// Imps only stun: our army (the friendly threat map) or a turret covering
/// the mine must finish their prey.
fn supported(imp: bool, turret: bool, mobile: f32) -> bool {
    !imp || turret || mobile > 0.0
}

/// Whether a known enemy unit at `at` sees `p`: a building always, a
/// mobile one while in our sight or lately seen there.
fn sighted(p: Vec3, at: Vec3, sight: f32, fixed: bool, age: i32) -> bool {
    (fixed || age <= SIGHT_MEMORY) && distance(p, at) <= sight
}

fn spaced(id: i32, p: Vec3, radius: f32, reservations: &[Reservation]) -> bool {
    reservations.iter().filter(|r| r.id != id).all(|r| {
        let separation = radius.max(r.radius) + CLEARANCE;
        distance(p, r.position) >= separation && r.site.is_none_or(|s| distance(p, s.position) >= separation)
    })
}

/// Keep a blast off friendly buildings' footprints, with arrival tolerance:
/// both damage and EMP spare economy, allied buildings and nanoframes.
fn clear_of_statics(snitch: bool, imp: bool, p: Vec3, radius: f32, statics: &[Building]) -> bool {
    statics.iter().filter(|_| snitch || imp).all(|b| distance(p, b.center) >= radius + b.footprint + CLEARANCE)
}

/// Rows across every field and, for an Imp, the place before each turret
/// that can finish its prey.
fn candidate_sites(imp: bool, radius: f32, fields: &[Field], finishers: &[Finisher]) -> Vec<Site> {
    let mut sites = Vec::new();
    for f in fields {
        for (p, depth) in rows(f.center, f.toward, f.width, radius + CLEARANCE) {
            sites.push(Site { position: p, gate: f.gate, value: f.value * 0.2, depth, ambush: false });
        }
    }
    if imp {
        for t in finishers {
            if let Some(p) = t.site(radius) {
                sites.push(Site { position: p, gate: t.position, value: 0.2, depth: 0, ambush: false });
            }
        }
    }
    sites
}

/// Value and travel time, discounted for overlap with existing assignments.
/// A valuable exposed expansion can outweigh an unused low-traffic gate.
fn choose_site(id: i32, from: Vec3, radius: f32, sites: &[Site], reservations: &[Reservation]) -> Option<Site> {
    sites
        .iter()
        .filter(|s| spaced(id, s.position, radius, reservations))
        .max_by(|a, b| site_score(id, from, a, reservations).total_cmp(&site_score(id, from, b, reservations)))
        .copied()
}

/// A line across the approach, with additional rows behind it. The first
/// row faces the enemy; depth stays on the home side of the gate.
fn rows(center: Vec3, forward: Vec3, width: f32, spacing: f32) -> Vec<(Vec3, usize)> {
    let length = forward.x.hypot(forward.z).max(1.0);
    let (dx, dz) = (forward.x / length, forward.z / length);
    let half = ((width * 0.5 - 32.0).max(0.0) / spacing).floor() as i32;
    let mut out = Vec::new();
    for row in 0..3 {
        for offset in -half..=half {
            let side = offset as f32 * spacing;
            let back = row as f32 * spacing;
            out.push((Vec3::xz(center.x - dx * back - dz * side, center.z - dz * back + dx * side), row));
        }
    }
    out
}

impl Ai {
    /// One shared reserve, counting completed mines and nanoframes exactly
    /// once, plus accepted orders that have not created a unit yet.
    fn sapper_mine_quota(&self, mine_name: &str) -> usize {
        self.defs.get(mine_name).map_or(0, |def| mine_quota(&self.misc_handler.mines.clumps, def.cost(self.metal)))
    }

    pub(crate) fn sapper_wants_mines(&self, mine_name: &str) -> bool {
        let wanted = self.sapper_mine_quota(mine_name);
        if wanted == 0 {
            return false;
        }
        let have = self
            .cb
            .team_units()
            .into_iter()
            .filter(|u| u.team() == self.team_id && u.health() > 0.0 && mine_type(self.defs.name_of(*u)))
            .count()
            + self.misc_handler.mines.orders.len();
        have < wanted
    }

    pub(crate) fn sapper_factory_idle(&mut self, factory: i32) {
        self.misc_handler.mines.orders.remove(&factory);
    }

    pub(crate) fn sapper_ordered(&mut self, factory: i32, def: UnitDef) {
        if mine_type(self.defs.name(def)) {
            self.misc_handler.mines.orders.insert(factory, (def.id(), self.frame));
        }
    }

    pub(crate) fn sapper_created(&mut self, unit: Unit, builder: Option<Unit>) {
        if let (Some(builder), Some(def)) = (builder, unit.def())
            && self.misc_handler.mines.orders.get(&builder.id()).is_some_and(|&(id, _)| id == def.id())
        {
            self.misc_handler.mines.orders.remove(&builder.id());
        }
    }

    fn sapper_update_reserve(&mut self, frame: i32) {
        self.sapper_observe_clumps(frame);
        self.misc_handler.mines.orders.retain(|id, (def, issued)| {
            self.fac.factories.get(id).is_some_and(|f| {
                f.unit.health() > 0.0
                    && f.unit.team() == self.team_id
                    && (frame - *issued < 90 || f.unit.current_commands().iter().any(|c| c.id() == -*def))
            })
        });
    }

    pub fn misc_add_sapper(&mut self, unit: Unit) {
        let Some(def) = unit.def() else { return };
        if self.misc_handler.sappers.contains_key(&unit.id()) {
            return;
        }
        configure(unit);
        self.misc_handler.sappers.insert(
            unit.id(),
            Sapper {
                unit,
                radius: blast_radius(def),
                imp: self.defs.name(def) == "cloakbomb",
                snitch: self.defs.name(def) == "shieldbomb",
                site: None,
                placed: false,
                held: false,
                charging: None,
                logged: i32::MIN / 2,
                next_judge: 0,
                deployment: Deployment::new(self.frame, unit.pos()),
            },
        );
    }

    fn sapper_front(&self, p: Vec3) -> bool {
        self.graph.is_front_line(p)
            && !self.graph.is_enemy_territory(p)
            && (self.graph.is_ally_territory(p) || self.war.get_total_friendly_power(p) > 0.0)
    }

    fn sapper_supported(&self, imp: bool, p: Vec3, radius: f32, finishers: &[Finisher]) -> bool {
        if !imp {
            return true;
        }
        // Sappers never paint these maps, so mines cannot count one another
        // (or themselves) as the firepower that finishes stunned enemies.
        supported(imp, finishers.iter().any(|t| t.covers(p, radius)), self.war.get_friendly_threat(p))
            || (imp && self.sapper_response(p, radius))
    }

    /// Whether the enemy's ground army walks at `p` (anywhere, before the
    /// terrain analysis): a mine elsewhere is off their way.
    fn sapper_enemy_ground(&self, p: Vec3) -> bool {
        let Some(strategy) = &self.strategy else { return true };
        self.terrain().and_then(|t| t.tier(strategy.tier)).is_none_or(|map| map.region_near(pos(p), 32.0).is_some())
    }

    /// Whether the enemy can both shoot at `p` and see it, by the sight of
    /// their known units. A mine walking off from there shows itself under
    /// their guns.
    fn sapper_watched(&self, p: Vec3, frame: i32) -> bool {
        self.war.get_threat(p) > 0.0
            && self.war.targets.values().filter(|e| !e.is_dead).any(|e| {
                let sight = e.ud.map_or(UNKNOWN_SIGHT, |d| d.los_radius());
                let age = if e.visible { 0 } else { frame - e.last_seen };
                sighted(p, e.position, sight, e.is_static, age)
            })
    }

    fn sapper_approach(&self) -> Approach<'_> {
        let enemy = Some(self.graph.get_enemy_center()).filter(|e| !e.is_zero());
        let routes = enemy.and_then(|e| {
            let map = self.terrain()?.tier(self.strategy.as_ref()?.tier)?;
            Some(Paths::new(map, pos(e), |_| false))
        });
        Approach { enemy, routes }
    }

    /// Lines across the enemy's approach: our front gates, or else (open
    /// maps, and before the terrain analysis) our front metal spots.
    fn sapper_fields(&self, approach: &Approach) -> Vec<Field> {
        let mut fields = Vec::new();
        if let Some(strategy) = &self.strategy
            && let Some(map) = self.terrain().and_then(|t| t.tier(strategy.tier))
        {
            for (rank, gate) in strategy.fortify.iter().enumerate() {
                let c = &map.chokes[gate.choke];
                let crossing = Vec3::xz(c.pos.x, c.pos.z);
                let Some(outward) = unit_toward(gate.pos, crossing) else { continue };
                let center = get_directional_point(crossing, gate.pos, distance(crossing, gate.pos).min(32.0));
                if !self.sapper_front(center) {
                    continue;
                }
                // The enemy's way in, read just outside the gate; straight
                // through it if their route there comes from our side.
                let outside = Vec3::xz(crossing.x + outward.x * GATE_LOOKOUT, crossing.z + outward.z * GATE_LOOKOUT);
                let toward = (approach.toward(outside, GATE_LOOKBACK))
                    .filter(|t| t.x * outward.x + t.z * outward.z > 0.0)
                    .unwrap_or(outward);
                // The strategy already orders gates by priority.
                let value = 1.0 / (1.0 + rank as f32);
                fields.push(Field { center, toward, width: gate.width, gate: crossing, value });
            }
        }
        // The conquest objectives we hold: a field on each one's enemy side,
        // across their way in, before the gates (see `economy::conquest`).
        for (center, value) in self.conquest_mine_fields() {
            let Some(toward) = approach.toward(center, GATE_LOOKBACK) else { continue };
            fields.push(Field { center, toward, width: 600.0, gate: center, value });
        }
        // Open maps and the period before terrain analysis still get a
        // distributed minefield, using owned front spots as sectors.
        {
            for spot in self.graph.get_front_line_spots().into_iter().filter(|s| s.owned) {
                let gate = spot.get_pos();
                // On the last stretch of the enemy's way to the spot.
                let Some(toward) = approach.toward(gate, SPOT_LEAD + CLEARANCE) else { continue };
                let center = Vec3::xz(gate.x + toward.x * SPOT_LEAD, gate.z + toward.z * SPOT_LEAD);
                fields.push(Field { center, toward, width: 800.0, gate, value: spot.value.max(1.0) });
            }
        }
        fields
    }

    /// Completed turrets that deal damage (a Faraday's stun finishes
    /// nothing), each with the enemy's approach over a mine's standoff. A
    /// stunned one still counts: it recovers, and a mine walking off
    /// uncovers itself.
    fn sapper_finishers(&self, approach: &Approach, radius: f32) -> Vec<Finisher> {
        (self.eco.dporcs.values())
            .filter(|t| !t.is_dead() && !t.unit.is_being_built())
            .filter_map(|t| {
                let def = t.unit.def()?;
                if !def.weapon_mounts().iter().filter_map(|m| m.weapon_def()).any(|w| !w.is_paralyzer()) {
                    return None;
                }
                let (position, range, footprint) = (t.get_pos(), t.unit.max_range(), footprint(def));
                let mut f = Finisher { position, range, footprint, toward: Vec3::ZERO };
                f.toward = approach.toward(position, f.standoff(radius) + CLEARANCE)?;
                Some(f)
            })
            .collect()
    }

    pub(super) fn misc_update_sappers(&mut self, frame: i32) {
        if frame % PERIOD != (self.offset + 10) % PERIOD {
            return;
        }
        self.sapper_update_reserve(frame);
        if self.misc_handler.sappers.is_empty() {
            return;
        }
        let sappers: Vec<_> = self.misc_handler.sappers.values().copied().collect();
        // Snapshot friendly buildings, nanoframes and builders once per pass.
        // New construction also invalidates old sites.
        let statics = self.sapper_bystanders();
        let imp_radius = sappers.iter().filter(|s| s.imp).map(|s| s.radius).fold(0.0, f32::max);
        let (fields, finishers) = {
            let approach = self.sapper_approach();
            let finishers = if imp_radius > 0.0 { self.sapper_finishers(&approach, imp_radius) } else { Vec::new() };
            (self.sapper_fields(&approach), finishers)
        };
        // The two bomb types share generated jobs; do not rebuild attacker
        // paths and query every friendly asset once per individual mine.
        let mut ambushes: Vec<(f32, Vec<Site>)> = Vec::new();
        for sapper in &sappers {
            if (sapper.imp || sapper.snitch) && !ambushes.iter().any(|(r, _)| *r == sapper.radius) {
                ambushes.push((sapper.radius, self.sapper_ambush_sites(sapper.radius)));
            }
        }
        let mut reservations: Vec<_> = sappers
            .iter()
            .map(|s| Reservation { id: s.unit.id(), position: s.unit.pos(), radius: s.radius, site: s.site })
            .collect();
        for (i, sapper) in sappers.iter().enumerate() {
            let unit = sapper.unit;
            let from = reservations[i].position;
            let mut deployment = sapper.deployment;
            if deployment.exiting(frame, unit.num_current_commands()) {
                continue;
            }
            if let Some(f0) = sapper.charging {
                if frame - f0 < CHARGE_HOLD {
                    continue;
                }
                self.misc_handler.sappers.get_mut(&unit.id()).unwrap().charging = None;
            }
            let near = !self.cb.enemy_units_in(from, sapper.radius + 128.0).is_empty();
            let watched = !near && sapper.placed && self.sapper_watched(from, frame);
            if near || watched {
                if unit.num_current_commands() > 0 {
                    unit.stop(0);
                }
                deployment.stalled(from, frame, false);
                let entry = self.misc_handler.sappers.get_mut(&unit.id()).unwrap();
                entry.held = watched;
                entry.deployment = deployment;
                continue;
            }
            let destination = sapper.site.map(|s| s.position).or(deployment.staging);
            let moving = destination.is_some_and(|p| distance(from, p) > ARRIVED);
            let stalled = deployment.stalled(from, frame, moving)
                || (moving && sapper.site.is_some() && frame - deployment.assigned > 30 * 120);
            if stalled {
                deployment.block(destination.unwrap(), frame);
                self.log(&format!("sappers: {} deployment stalled; retrying another site", unit.id()));
            }
            let reachable = self.pathfinder.raider_reachability(&self.war, unit, 0.0);
            let mut candidates =
                ambushes.iter().find(|(r, _)| *r == sapper.radius).map_or_else(Vec::new, |(_, sites)| sites.clone());
            candidates.extend(candidate_sites(sapper.imp, sapper.radius, &fields, &finishers));
            if let Some(old) = sapper.site {
                let committed = deployment.committed(frame);
                let anchor_live = if old.ambush {
                    self.sapper_live_anchor(old.gate) && (committed || self.graph.is_front_line(old.gate))
                } else {
                    fields.iter().any(|f| distance(f.gate, old.gate) < 32.0)
                };
                if let Some(refreshed) = refresh_site(old, &candidates, sapper.radius, committed, anchor_live) {
                    // Still revalidate safety, mobility and support below.
                    candidates.push(refreshed);
                }
            }
            let own_map = unit.def().and_then(tier_of).and_then(|tier| self.terrain()?.tier(tier));
            let candidates: Vec<_> = candidates
                .into_iter()
                .filter(|s| {
                    (s.ambush || self.sapper_front(s.position) || self.conquest_mine_ground(s.position))
                        && self.sapper_supported(sapper.imp, s.position, sapper.radius, &finishers)
                        && clear_of_statics(sapper.snitch, sapper.imp, s.position, sapper.radius, &statics)
                        && self.war.get_porc_threat(s.position) <= 0.0
                        && (s.ambush || self.sapper_enemy_ground(s.position))
                        && own_map.is_some_and(|map| map.region_at(pos(s.position)).is_some())
                        && reachable.reaches(s.position)
                        && deployment.allows(s.position, frame)
                        && spaced(unit.id(), s.position, sapper.radius, &reservations)
                })
                .collect();
            let selected = select_assignment(sapper, from, &candidates, &reservations, frame);
            if selected.is_none() && (sapper.deployment.phase == Phase::Exit || frame - sapper.logged >= 30 * 30) {
                self.log(&format!(
                    "sappers: {} no eligible site: {} fields, {} valid candidates, {} clumps; staging",
                    unit.id(),
                    fields.len(),
                    candidates.len(),
                    self.misc_handler.mines.clumps.len()
                ));
                self.misc_handler.sappers.get_mut(&unit.id()).unwrap().logged = frame;
            }
            let changed = match (sapper.site, selected) {
                (Some(a), Some(b)) => distance(a.position, b.position) > ARRIVED,
                (None, None) => false,
                _ => true,
            };
            reservations[i].site = selected;
            if changed || stalled {
                deployment.assign(from, frame);
            }
            let placed = selected.is_some_and(|s| distance(from, s.position) <= ARRIVED);
            if changed {
                if let Some(s) = selected {
                    self.log(&format!(
                        "sappers: {} {} -> ({:.0}, {:.0}), bait ({:.0}, {:.0}), score {:.2}, age {}s",
                        unit.id(),
                        if s.ambush { "ambush" } else { "gate" },
                        s.position.x,
                        s.position.z,
                        s.gate.x,
                        s.gate.z,
                        s.value,
                        (frame - deployment.born) / 30
                    ));
                } else {
                    self.log(&format!("sappers: {} no safe supported reachable ambush; staging", unit.id()));
                }
            }
            if placed {
                deployment.phase = Phase::Ready;
                if !sapper.placed {
                    self.log(&format!("sappers: {} ready after {}s", unit.id(), (frame - deployment.born) / 30));
                }
                if unit.num_current_commands() > 0 {
                    unit.stop(0);
                }
            } else if let Some(s) = selected {
                deployment.phase = Phase::Travel;
                if changed || stalled || unit.num_current_commands() == 0 {
                    self.sapper_move(unit, s.position);
                }
            } else {
                let was_staging = deployment.phase == Phase::Staging;
                deployment.phase = Phase::Staging;
                if let Some(p) = self.sapper_stage(unit, sapper.radius, &statics, frame, &deployment) {
                    let new_stage = deployment.staging.is_none_or(|q| distance(p, q) > ARRIVED);
                    deployment.staging = Some(p);
                    if new_stage {
                        deployment.progress = frame;
                        deployment.at = from;
                    }
                    if distance(from, p) > ARRIVED
                        && (new_stage || stalled || !was_staging || unit.num_current_commands() == 0)
                    {
                        self.sapper_move(unit, p);
                    }
                } else if unit.num_current_commands() > 0 {
                    unit.stop(0);
                }
            }
            let entry = self.misc_handler.sappers.get_mut(&unit.id()).unwrap();
            (entry.site, entry.placed, entry.held, entry.deployment) = (selected, placed, false, deployment);
        }
    }
}

/// Armed units of the judged fight busy with a foe other than the bomb
/// (its weapons are mid-reload), by the snapshot.
fn busy_units(snap: &FightSnapshot, bomb: usize) -> Vec<bool> {
    (snap.units.iter())
        .map(|u| {
            let range = u.spec.weapons.iter().map(|w| w.range).fold(0.0f32, f32::max);
            range > 0.0
                && (snap.units.iter().enumerate())
                    .any(|(j, v)| j != bomb && v.side != u.side && (v.x - u.x).hypot(v.z - u.z) <= range + BUSY_SLACK)
        })
        .collect()
}

impl Ai {
    /// `bombcharge`: each Imp and Snitch with enemies near asks the fight
    /// predictor whether charging them beats sitting tight, and charges the
    /// nearest when it does by [`CHARGE_MARGIN`].
    pub(super) fn misc_charge_sappers(&mut self, frame: i32) {
        if self.dev_off("bombcharge") || self.misc_handler.sappers.is_empty() {
            return;
        }
        let bombs: Vec<Sapper> = (self.misc_handler.sappers.values())
            .filter(|s| (s.imp || s.snitch) && s.charging.is_none())
            .copied()
            .collect();
        let bystanders = self.sapper_bystanders();
        let mut judged = 0;
        for s in bombs {
            if judged >= CHARGE_PER_TICK {
                break;
            }
            let unit = s.unit;
            if unit.health() <= 0.0 || unit.is_being_built() || frame < s.next_judge {
                continue;
            }
            let p = unit.pos();
            // The sim's bomb runs at its nearest enemy: so does ours.
            let nearest = (self.war.targets.values())
                .filter(|e| !e.is_dead && e.identified && e.ud.is_some() && !e.can_fly())
                .filter(|e| e.is_static || frame - e.last_seen <= CHARGE_SEEN)
                .map(|e| (distance(e.position, p), e))
                .filter(|(d, _)| *d <= CHARGE_LOOK)
                .min_by(|a, b| a.0.total_cmp(&b.0))
                .map(|(d, e)| (d, e.unit, e.position, e.visible, e.ud.map_or("?", |u| self.defs.name(u)).to_owned()));
            let Some((dist, _target, at, _visible, target_name)) = nearest else { continue };
            if !charge_clear(p, at, s.radius, &bystanders) {
                continue;
            }
            let Some(model) = self.local_model() else { return };
            let Some((snap, bomb)) = self.bomb_fight(unit, p) else { continue };
            judged += 1;
            let busy = busy_units(&snap, bomb);
            let theirs = snap.units.iter().filter(|u| u.side == 1).count();
            let busy_theirs = snap.units.iter().zip(&busy).filter(|(u, b)| u.side == 1 && **b).count();
            let v = model.bomb_charge(&snap, bomb, &busy, CHARGE_HORIZON);
            let name = if s.imp { "Imp" } else { "Snitch" };
            let charge = v.net > CHARGE_MARGIN;
            let record = json!({
                "event": if charge { "charge" } else { "sit" },
                "t": frame / 30,
                "unit": unit.id(),
                "def": name,
                "pos": [p.x.round(), p.z.round()],
                "nearest": target_name,
                "dist": dist.round(),
                "theirs": theirs,
                "busy": busy_theirs,
                "through": (v.p_through * 1000.0).round() / 1000.0,
                "gain": v.gain.round(),
                "saved": v.saved.round(),
                "spent": (v.spent * 1000.0).round() / 1000.0,
                "net": v.net.round(),
                "reloads": snap.units.iter().map(|u| &u.reloads).collect::<Vec<_>>(),
                // The judged fight, for offline replay: def, side, hp, x, z, busy; the bomb last.
                "snap": snap.units.iter().zip(&busy).map(|(u, b)| json!([u.spec.def, u.side, u.hp.round(), u.x.round(), u.z.round(), *b])).collect::<Vec<_>>(),
            });
            if !charge {
                let entry = self.misc_handler.sappers.get_mut(&unit.id()).unwrap();
                entry.next_judge = frame + CHARGE_EVERY;
                if frame - s.logged >= CHARGE_LOG {
                    entry.logged = frame;
                    self.log(&format!(
                        "sappers: {name} {} sits tight by {theirs} enemies ({busy_theirs} busy; nearest {target_name} {dist:.0} \
                         away): through {:.2}, gain {:.0}, saved {:.0}, net {:+.0}",
                        unit.id(),
                        v.p_through,
                        v.gain,
                        v.saved,
                        v.net
                    ));
                    self.op_emit("bombcharge", record);
                }
                continue;
            }
            // A bounded charge to the judged position cannot chase its target
            // back into the bait's safety envelope. Fire-at-will still triggers.
            kgb_move_to_exact(unit, at, 4.0, 0, frame);
            let entry = self.misc_handler.sappers.get_mut(&unit.id()).unwrap();
            (entry.charging, entry.site, entry.placed, entry.held) = (Some(frame), None, false, false);
            self.log(&format!(
                "sappers: {name} {} charges {target_name} {} away at ({:.0}, {:.0}), {theirs} enemies ({busy_theirs} busy): \
                 through {:.2}, gain {:.0}, saved {:.0}, net {:+.0}",
                unit.id(),
                dist,
                at.x,
                at.z,
                v.p_through,
                v.gain,
                v.saved,
                v.net
            ));
            self.op_emit("bombcharge", record);
        }
    }

    /// The fight round bomb `bomb` at `p`: our units (not other mines
    /// but those charging) and identified enemies (mobiles as lately seen) within
    /// [`CHARGE_REACH`], the bomb last; in the plane, whatever `fight2d`
    /// says, since a blast's reach is geometry. `None` past
    /// [`CHARGE_MAX_UNITS`] or with anything submerged.
    fn bomb_fight(&mut self, bomb: Unit, p: Vec3) -> Option<(FightSnapshot, usize)> {
        let frame = self.frame;
        let mut ours: Vec<(Unit, UnitDef, f32)> = Vec::new();
        for u in self.cb.friendly_units_in(p, CHARGE_REACH) {
            // Other mines sit hidden, but one already charging is in the
            // fight: this bomb is judged on what it adds to that one.
            let mine = self.misc_handler.sappers.get(&u.id());
            if u.id() == bomb.id() || mine.is_some_and(|m| m.charging.is_none()) || u.is_being_built() {
                continue;
            }
            let Some((def, hp)) = our_state(u) else { continue };
            if def.is_able_to_fly() {
                continue;
            }
            if u.pos().y < -5.0 {
                return None;
            }
            ours.push((u, def, hp));
        }
        let mut theirs: Vec<(i32, UnitDef, f32, Vec3)> = Vec::new();
        for (&eid, e) in &self.war.targets {
            if e.is_dead || !e.identified || e.can_fly() || distance(e.position, p) > CHARGE_REACH {
                continue;
            }
            if !e.is_static && frame - e.last_seen > CHARGE_SEEN {
                continue;
            }
            let Some(def) = e.ud else { continue };
            if e.position.y < -5.0 {
                return None;
            }
            theirs.push((eid, def, e.get_health() * e.def_health(), e.position));
        }
        if ours.len() + theirs.len() + 1 > CHARGE_MAX_UNITS {
            return None;
        }
        let (bomb_def, bomb_hp) = our_state(bomb)?;
        let flags = SimFlags { twod: true, ..self.sim_flags() };
        let mut snap = FightSnapshot { flags, ..Default::default() };
        for (u, def, hp) in ours {
            let (spec, q) = (self.fight_spec_of(u.id(), def), u.pos());
            snap.units.push(SnapUnit { reloads: self.snap_reloads(u.id()), side: 0, spec, hp, x: q.x, z: q.z });
        }
        for (eid, def, hp, q) in theirs {
            let spec = self.fight_spec_of(eid, def);
            snap.units.push(SnapUnit { reloads: self.snap_reloads(eid), side: 1, spec, hp, x: q.x, z: q.z });
        }
        let spec = self.fight_spec_of(bomb.id(), bomb_def);
        snap.units.push(SnapUnit { reloads: Vec::new(), side: 0, spec, hp: bomb_hp, x: p.x, z: p.z });
        let index = snap.units.len() - 1;
        Some((snap, index))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};

    thread_local! {
        static MINE_ROSTER: RefCell<Vec<i32>> = const { RefCell::new(Vec::new()) };
        static MINE_DEFS_READY: Cell<bool> = const { Cell::new(false) };
    }

    pub(super) fn reserve_ai() -> Ai {
        unsafe extern "C" fn roster(_: i32, out: *mut i32, _: i32) -> i32 {
            MINE_ROSTER.with_borrow(|ids| {
                if !out.is_null() {
                    unsafe { std::ptr::copy_nonoverlapping(ids.as_ptr(), out, ids.len()) };
                }
                ids.len() as i32
            })
        }
        unsafe extern "C" fn defs(_: i32, out: *mut i32, _: i32) -> i32 {
            if !MINE_DEFS_READY.get() {
                return 0;
            }
            if !out.is_null() {
                unsafe { std::ptr::copy_nonoverlapping([1, 2, 3].as_ptr(), out, 3) };
            }
            3
        }
        unsafe extern "C" fn name(_: i32, id: i32) -> *const std::ffi::c_char {
            match id {
                1 => c"cloakbomb".as_ptr(),
                2 => c"shieldbomb".as_ptr(),
                _ => c"jumpscout".as_ptr(),
            }
        }
        unsafe extern "C" fn def(_: i32, id: i32) -> i32 {
            match id {
                41 | 51 => 1,
                _ => id / 10,
            }
        }
        unsafe extern "C" fn team(_: i32, id: i32) -> i32 {
            if id == 41 { 3 } else { 2 }
        }
        unsafe extern "C" fn health(_: i32, id: i32) -> f32 {
            if id == 51 { 0.0 } else { 100.0 }
        }
        unsafe extern "C" fn cost(_: i32, id: i32, _: i32) -> f32 {
            match id {
                1 => 125.0,
                2 => 170.0,
                _ => 100.0,
            }
        }
        MINE_ROSTER.with_borrow_mut(Vec::clear);
        MINE_DEFS_READY.set(false);
        let mut ai = crate::economy::test_ai_with(|t| {
            t.getTeamUnits = Some(roster);
            t.getUnitDefs = Some(defs);
            t.UnitDef_getName = Some(name);
            t.UnitDef_getCost = Some(cost);
            t.Unit_getDef = Some(def);
            t.Unit_getTeam = Some(team);
            t.Unit_getHealth = Some(health);
        });
        MINE_DEFS_READY.set(true);
        ai.defs = crate::defs::Defs::new(ai.cb);
        ai
    }

    #[test]
    fn mine_reserve_counts_mixed_builds_and_replaces_losses_without_conquest() {
        let mut ai = reserve_ai();
        ai.set_feature_off("conquest", true);
        assert!(ai.dev_off("conquest"));
        assert!(!ai.sapper_wants_mines("shieldbomb"));
        ai.misc_handler.mines.clumps = test_clumps(14);
        assert!(ai.sapper_wants_mines("shieldbomb"));
        // Two Imps and a Snitch; other bomb types, allies and dead units
        // cannot satisfy this reserve. None need be in the finished handler.
        MINE_ROSTER.with_borrow_mut(|ids| *ids = vec![11, 12, 21, 31, 41, 51]);
        assert!(ai.sapper_wants_mines("shieldbomb"));
        let factory = Unit::from_id(ai.cb, 100).unwrap();
        let snitch = ai.defs.get("shieldbomb").unwrap();
        ai.sapper_ordered(factory.id(), snitch);
        assert!(!ai.sapper_wants_mines("shieldbomb"), "accepted order fills the fourth slot");
        MINE_ROSTER.with_borrow_mut(|ids| ids.push(22));
        ai.sapper_created(Unit::from_id(ai.cb, 22).unwrap(), Some(factory));
        assert!(ai.misc_handler.mines.orders.is_empty());
        assert!(!ai.sapper_wants_mines("shieldbomb"), "nanoframe replaces its reservation");
        MINE_ROSTER.with_borrow_mut(|ids| ids.retain(|&id| id != 11));
        assert!(ai.sapper_wants_mines("shieldbomb"), "spent mine gets replaced");
        ai.sapper_ordered(factory.id(), snitch);
        ai.sapper_factory_idle(factory.id());
        assert!(ai.sapper_wants_mines("shieldbomb"), "canceled order releases its slot");
        ai.sapper_update_reserve(1000);
        assert!(ai.sapper_wants_mines("shieldbomb"), "short fog gaps preserve observed demand");
        ai.sapper_update_reserve(30 * 91);
        assert!(!ai.sapper_wants_mines("shieldbomb"), "old sightings expire");
    }

    fn site(x: f32, gate: f32, value: f32) -> Site {
        Site { position: Vec3::xz(x, 100.0), gate: Vec3::xz(gate, 100.0), value, depth: 0, ambush: false }
    }

    /// A Lotus-like turret at (1000, 1000), facing the enemy along `toward`.
    fn lotus(toward: Vec3) -> Finisher {
        Finisher { position: Vec3::xz(1000.0, 1000.0), range: 460.0, footprint: 23.0, toward }
    }

    /// `d` elmos from (1000, 1000) at `deg` degrees from +x.
    fn polar(d: f32, deg: f32) -> Vec3 {
        let a = deg.to_radians();
        Vec3::xz(1000.0 + d * a.cos(), 1000.0 + d * a.sin())
    }

    #[test]
    fn imps_need_independent_support_but_snitches_do_not() {
        assert!(!supported(true, false, 0.0));
        assert!(supported(true, true, 0.0));
        assert!(supported(true, false, 0.1));
        assert!(supported(false, false, 0.0));
    }

    #[test]
    fn snitches_and_imps_spare_every_building() {
        let building = |_armed| [Building { center: Vec3::xz(1000.0, 1000.0), footprint: 100.0 }];
        // Outside the centre's blast radius is insufficient for a large building.
        let near = Vec3::xz(1400.0, 1000.0);
        assert!(!clear_of_statics(true, false, near, 384.0, &building(false)));
        assert!(!clear_of_statics(true, false, Vec3::xz(1547.0, 1000.0), 384.0, &building(false)));
        assert!(clear_of_statics(true, false, Vec3::xz(1548.0, 1000.0), 384.0, &building(false)));
        assert!(clear_of_statics(true, false, near, 384.0, &[]));
        // An Imp also spares unarmed economy.
        assert!(!clear_of_statics(false, true, near, 384.0, &building(false)));
        assert!(!clear_of_statics(false, true, near, 384.0, &building(true)));
        assert!(clear_of_statics(false, true, Vec3::xz(1548.0, 1000.0), 384.0, &building(true)));
        // Other bombs (Skuttles) keep none.
        assert!(clear_of_statics(false, false, near, 384.0, &building(true)));
        // A previously valid placement becomes invalid when a building appears.
        let new = [Building { center: near, footprint: 16.0 }];
        assert!(!clear_of_statics(true, false, near, 384.0, &new));
        assert!(!clear_of_statics(false, true, near, 384.0, &new));
    }

    #[test]
    fn a_turret_covers_mines_past_its_blast_on_the_enemy_side_within_its_reach() {
        // An Imp (blast radius 176) by a Lotus: at least 263 out, and the
        // turret reaching 132 past the mine, so at most 328.
        let t = lotus(Vec3::xz(1.0, 0.0));
        assert!(t.covers(polar(270.0, 0.0), 176.0));
        assert!(t.covers(polar(320.0, 50.0), 176.0));
        // Under the turret: the stun would take it too.
        assert!(!t.covers(polar(150.0, 0.0), 176.0));
        assert!(!t.covers(polar(260.0, 0.0), 176.0));
        // Too far: the stunned stand out of its reach.
        assert!(!t.covers(polar(340.0, 0.0), 176.0));
        // Off the enemy's way in, or behind the turret.
        assert!(!t.covers(polar(270.0, 70.0), 176.0));
        assert!(!t.covers(polar(270.0, 180.0), 176.0));
    }

    #[test]
    fn a_turret_site_lies_straight_up_the_approach_just_past_its_blast() {
        let t = lotus(Vec3::xz(0.0, -1.0));
        let p = t.site(176.0).unwrap();
        assert!((p.x - 1000.0).abs() < 0.01 && (1000.0 - p.z - 264.0).abs() < 0.01, "{p:?}");
        let turret = [Building { center: t.position, footprint: t.footprint }];
        assert!(clear_of_statics(false, true, p, 176.0, &turret));
        // Too short-ranged to reach what any blast it survives stuns.
        assert!(Finisher { range: 300.0, ..t }.site(176.0).is_none());
    }

    #[test]
    fn only_imps_take_turret_sites_and_rows_run_across_the_approach() {
        let center = Vec3::xz(2000.0, 2000.0);
        let field = Field { center, toward: Vec3::xz(0.0, 1.0), width: 1000.0, gate: center, value: 1.0 };
        let t = lotus(Vec3::xz(1.0, 0.0));
        let imp = candidate_sites(true, 176.0, &[field], &[t]);
        let snitch = candidate_sites(false, 176.0, &[field], &[t]);
        assert_eq!((imp.len(), snitch.len()), (10, 9));
        assert!(imp.iter().any(|s| s.gate.x == 1000.0 && t.covers(s.position, 176.0)));
        // The enemy comes from +z: the front row runs along x, depth
        // behind it.
        assert!(snitch.iter().filter(|s| s.depth == 0).all(|s| s.position.z == 2000.0));
        assert!(snitch.iter().filter(|s| s.depth > 0).all(|s| s.position.z < 2000.0));
    }

    #[test]
    fn enemy_eyes_are_buildings_and_lately_seen_mobiles_in_sight_range() {
        let (p, at) = (Vec3::xz(0.0, 0.0), Vec3::xz(400.0, 0.0));
        assert!(sighted(p, at, 450.0, false, 0));
        assert!(!sighted(p, at, 350.0, false, 0));
        assert!(!sighted(p, at, 450.0, false, SIGHT_MEMORY + 1));
        assert!(sighted(p, at, 450.0, true, 100_000));
    }

    #[test]
    fn mixed_mines_reserve_the_larger_blast_at_both_live_and_future_positions() {
        let reservations = [Reservation {
            id: 1,
            position: Vec3::xz(100.0, 100.0),
            radius: 384.0,
            site: Some(site(1500.0, 1500.0, 1.0)),
        }];
        assert!(!spaced(2, Vec3::xz(400.0, 100.0), 100.0, &reservations));
        assert!(!spaced(2, Vec3::xz(1200.0, 100.0), 100.0, &reservations));
        assert!(spaced(2, Vec3::xz(700.0, 100.0), 100.0, &reservations));
        assert!(!spaced(2, Vec3::xz(700.0, 100.0), 600.0, &reservations));
        assert!(spaced(1, Vec3::xz(100.0, 100.0), 384.0, &reservations));
    }

    #[test]
    fn coverage_penalty_does_not_outweigh_a_much_more_valuable_approach() {
        let first = site(1000.0, 1000.0, 100.0);
        let reservations = [Reservation { id: 1, position: first.position, radius: 200.0, site: Some(first) }];
        let candidates = [site(1400.0, 1000.0, 100.0), site(4000.0, 4000.0, 1.0)];
        assert_eq!(choose_site(2, Vec3::ZERO, 200.0, &candidates, &reservations).unwrap().gate.x, 1000.0);
        assert!(choose_site(2, Vec3::ZERO, 200.0, &[first], &reservations).is_none());
    }

    #[test]
    fn rows_cover_the_width_and_add_depth_only_on_the_home_side() {
        let points = rows(Vec3::xz(1000.0, 1000.0), Vec3::xz(1.0, 0.0), 1000.0, 400.0);
        assert_eq!(points.len(), 9);
        assert!(points.iter().all(|(p, _)| p.x <= 1000.0 && p.z >= 600.0 && p.z <= 1400.0));
        for (i, a) in points.iter().enumerate() {
            assert!(points[i + 1..].iter().all(|b| distance(a.0, b.0) >= 400.0));
        }
        assert_eq!(rows(Vec3::ZERO, Vec3::xz(0.0, 1.0), 100.0, 400.0).len(), 3);
    }

    #[test]
    fn fills_the_enemy_facing_row_before_closer_rear_rows() {
        let front = site(1000.0, 1000.0, 1.0);
        let rear = Site { depth: 1, ..site(500.0, 1000.0, 1.0) };
        assert_eq!(choose_site(1, Vec3::ZERO, 200.0, &[rear, front], &[]).unwrap().position.x, 1000.0);
    }

    #[test]
    fn engine_orders_hold_position_fire_at_will_and_disable_chasing() {
        use springai::ffi::{
            CommandTopic as Topic, SCustomUnitCommand, SSetFireStateUnitCommand, SSetMoveStateUnitCommand,
            SSkirmishAICallback,
        };
        use std::cell::RefCell;
        use std::ffi::c_void;

        thread_local! { static ORDERS: RefCell<Vec<(u32, i32)>> = const { RefCell::new(Vec::new()) }; }
        unsafe extern "C" fn command(_: i32, _: i32, _: i32, topic: i32, data: *mut c_void) -> i32 {
            let value = unsafe {
                match topic as u32 {
                    Topic::COMMAND_UNIT_SET_MOVE_STATE => (*(data as *const SSetMoveStateUnitCommand)).moveState,
                    Topic::COMMAND_UNIT_SET_FIRE_STATE => (*(data as *const SSetFireStateUnitCommand)).fireState,
                    Topic::COMMAND_UNIT_CUSTOM => {
                        let c = &*(data as *const SCustomUnitCommand);
                        if c.cmdId == cmd::UNIT_AI && c.params_size == 1 && *c.params == 0.0 { 0 } else { -1 }
                    }
                    _ => -1,
                }
            };
            ORDERS.with(|o| o.borrow_mut().push((topic as u32, value)));
            0
        }
        let table = SSkirmishAICallback { Engine_handleCommand: Some(command), ..Default::default() };
        // SAFETY: every handle is used only while this table is alive.
        let cb = unsafe { springai::Callback::from_raw(0, &table) };
        configure(Unit::from_id(cb, 1).unwrap());
        ORDERS.with(|o| {
            assert_eq!(
                *o.borrow(),
                vec![
                    (Topic::COMMAND_UNIT_SET_MOVE_STATE, 0),
                    (Topic::COMMAND_UNIT_SET_FIRE_STATE, 2),
                    (Topic::COMMAND_UNIT_CUSTOM, 0),
                ]
            )
        });
    }

    #[test]
    fn blast_spacing_reads_explosions_even_without_mounted_weapons() {
        use springai::ffi::SSkirmishAICallback;
        unsafe extern "C" fn death(_: i32, _: i32) -> i32 {
            10
        }
        unsafe extern "C" fn self_d(_: i32, _: i32) -> i32 {
            11
        }
        unsafe extern "C" fn area(_: i32, weapon: i32) -> f32 {
            if weapon == 10 { 352.0 } else { 384.0 }
        }
        let table = SSkirmishAICallback {
            UnitDef_getDeathExplosion: Some(death),
            UnitDef_getSelfDExplosion: Some(self_d),
            WeaponDef_getAreaOfEffect: Some(area),
            ..Default::default()
        };
        // SAFETY: every handle is used only while this table is alive.
        let cb = unsafe { springai::Callback::from_raw(0, &table) };
        assert_eq!(blast_radius(UnitDef::from_id(cb, 1).unwrap()), 384.0);
    }
}
