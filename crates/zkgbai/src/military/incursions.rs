//! Enemy incursions: enemy ground forces on the move, reasoned about over
//! the terrain model's region graph (see `zk_terrain::threat`).
//!
//! Every second the enemy's recently seen fighters are grouped by movement
//! tier and region. For each group the AI works out how soon it can reach
//! every region (never through a chokepoint our defenses hold against it)
//! and which of our squads at home (rallying or attacking) can stand in a
//! chokepoint ahead of it to cut it off from our value or make it go the
//! long way round. Squads with such a block order go there instead of to
//! their rally point or a target of their own; where value stays at stake,
//! the first of our assets the group can reach becomes a defense target
//! before it gets there.

use std::collections::BTreeMap;

use springai::Vec3;
use zk_terrain::paths::SNAP;
use zk_terrain::strategy::Side;
use zk_terrain::threat::{Force, arrival_times, intercept};
use zk_terrain::{Pos, Tier};

use crate::Ai;
use crate::military::{DefenseTarget, SquadId};
use crate::terrain::{pos, setback, tier_of};
use crate::util::distance;

/// Frames between re-assessments.
const PERIOD: i32 = 30;

/// Enemy units seen within this many frames count.
const RECENT: i32 = 60;

/// Groups weaker than this (a couple of raiders) are left to our defenses
/// and the squads nearby.
const MIN_POWER: f32 = 100.0;

/// How far ahead, in seconds of the raid's travel, our value is defended.
pub const HORIZON: f32 = 30.0;

/// Frames a block order lasts unless renewed.
const ORDER_FRAMES: i32 = 90;

/// How far behind its chokepoint a blocking squad stands, elmos.
const BLOCK_SETBACK: f32 = 150.0;

/// Squads this close to a raid fight it rather than block its way.
const ENGAGE_RANGE: f32 = 800.0;

/// A group of enemy fighters in one region of their movement tier.
pub struct Incursion {
    pub tier: Tier,
    pub force: Force,
    pub units: usize,
    /// Seconds to each region of the tier; `INFINITY` where it cannot go.
    pub arrival: Vec<f32>,
    /// Our value within the horizon that the blocks keep it from.
    pub protected: f32,
    /// Our value within the horizon it can still reach.
    pub exposed: f32,
}

/// A squad sent to stand in a chokepoint ahead of an incursion.
#[derive(Clone, Copy, Debug)]
pub struct BlockOrder {
    pub tier: Tier,
    pub choke: usize,
    /// Where the squad stands, on the side it defends.
    pub pos: Vec3,
    /// Seconds it gets there ahead of the incursion.
    pub slack: f32,
    pub until: i32,
}

/// The latest assessment.
#[derive(Default)]
pub struct Incursions {
    pub frame: i32,
    /// Incursions that threaten our value within the horizon.
    pub list: Vec<Incursion>,
    pub orders: BTreeMap<SquadId, BlockOrder>,
    stats: Stats,
}

/// Running totals over assessments, logged every [`STATS_PERIOD`] frames.
#[derive(Default)]
struct Stats {
    /// Groups at least [`MIN_POWER`] strong.
    groups: u32,
    /// Of those, threatening our value within the horizon.
    threats: u32,
    /// Of those, with a squad at least as strong available.
    matched: u32,
    /// Of those, blocked somewhere.
    blocked: u32,
    /// Squads past forming, and those in our territory.
    squads: u32,
    home: u32,
}

/// Frames between summaries in the log.
const STATS_PERIOD: i32 = 1800;

/// An enemy fighter: position, speed (elmos/s), danger.
type Member = (Vec3, f32, f32);

/// A squad that can be sent to block: position, speed (elmos/s), power.
struct Defender {
    squad: SquadId,
    pos: Vec3,
    speed: f32,
    power: f32,
}

impl Ai {
    /// Re-assess the enemy's incursions every [`PERIOD`] frames.
    pub fn incursions_update(&mut self, frame: i32) {
        if (frame + self.offset + 11) % PERIOD != 0 {
            return;
        }
        self.incursions.orders.retain(|_, o| o.until > frame);
        let Some(terrain) = self.terrain.get() else { return };

        // Enemy fighters by tier and region.
        let mut groups: BTreeMap<(Tier, usize), Vec<Member>> = BTreeMap::new();
        for e in self.war.targets.values() {
            if !e.identified || e.is_static || e.can_fly() || (e.is_worker && !e.is_com) || frame - e.last_seen > RECENT
            {
                continue;
            }
            let danger = e.get_danger();
            let Some(tier) = e.ud.and_then(tier_of).filter(|_| danger > 0.0) else { continue };
            let Some(r) = terrain.tier(tier).and_then(|m| m.region_near(pos(e.position), SNAP)) else { continue };
            groups.entry((tier, r)).or_default().push((e.position, e.def_speed(), danger));
        }
        let mut forces: Vec<(Tier, Force, usize)> = groups
            .into_iter()
            .map(|((tier, _), units)| {
                let n = units.len() as f32;
                let (x, z) = units.iter().fold((0.0, 0.0), |(x, z), (p, ..)| (x + p.x / n, z + p.z / n));
                let center = Vec3::xz(x, z);
                // The member nearest the middle stands for the group (the
                // middle itself may be off passable ground).
                let at = units.iter().map(|u| u.0).min_by(|a, b| distance(*a, center).total_cmp(&distance(*b, center)));
                let speed = units.iter().map(|u| u.1).fold(f32::INFINITY, f32::min);
                let power = units.iter().map(|u| u.2).sum();
                (tier, Force { pos: pos(at.unwrap_or(center)), speed, power }, units.len())
            })
            .filter(|(_, f, _)| f.power >= MIN_POWER)
            .collect();
        // The strongest have first pick of the defenders.
        forces.sort_by(|a, b| b.1.power.total_cmp(&a.1.power));

        let mut stats = std::mem::take(&mut self.incursions.stats);
        stats.groups += forces.len() as u32;
        let mut defenders = Vec::new();
        for &sid in self.squad_handler.squads.iter().chain(&self.raider_handler.raider_squads) {
            let Some(s) = self.squads.get(sid).filter(|s| s.status != 'f' && !s.is_dead(&self.fighters)) else {
                continue;
            };
            stats.squads += 1;
            let p = s.get_pos(&self.fighters);
            let speed = s
                .fighters
                .iter()
                .filter_map(|id| self.fighters.get(id)?.unit.def())
                .map(|d| d.speed())
                .fold(f32::INFINITY, f32::min);
            if self.graph.is_ally_territory(p) && speed.is_finite() {
                stats.home += 1;
                defenders.push(Defender { squad: sid, pos: p, speed, power: s.get_threat(&self.fighters) as f32 });
            }
        }
        let mut free = vec![true; defenders.len()];

        let assets = self.strategy_assets();
        let elevation = self.cb.map();
        let at = |p: Pos| Vec3::new(p.x, elevation.elevation_at(p.x, p.z), p.z);
        // Our defenses at each chokepoint, by tier.
        let mut defenses: BTreeMap<Tier, Vec<f32>> = BTreeMap::new();
        let mut list = Vec::new();
        let mut orders = Vec::new();
        let mut predicted = Vec::new();
        for (tier, force, units) in forces {
            let Some(map) = terrain.tier(tier) else { continue };
            let porc = defenses.entry(tier).or_insert_with(|| {
                map.chokes.iter().map(|c| self.war.get_friendly_porc_threat(Vec3::xz(c.pos.x, c.pos.z))).collect()
            });
            let held = |c: usize| porc[c] >= force.power;
            let near = |d: &Defender| distance(d.pos, Vec3::xz(force.pos.x, force.pos.z)) <= ENGAGE_RANGE;
            let idx: Vec<usize> = (0..defenders.len()).filter(|&i| free[i] && !near(&defenders[i])).collect();
            let sent: Vec<Force> = idx
                .iter()
                .map(|&i| Force { pos: pos(defenders[i].pos), speed: defenders[i].speed, power: defenders[i].power })
                .collect();
            let plan = intercept(map, &force, held, &sent, &assets, HORIZON);
            if plan.protected + plan.exposed <= 0.0 {
                continue;
            }
            stats.threats += 1;
            stats.matched += sent.iter().any(|d| d.power >= force.power) as u32;
            stats.blocked += !plan.blocks.is_empty() as u32;
            for b in &plan.blocks {
                let d = &defenders[idx[b.defender]];
                free[idx[b.defender]] = false;
                let order = BlockOrder {
                    tier,
                    choke: b.choke,
                    pos: at(setback(map, b.choke, b.behind, BLOCK_SETBACK)),
                    slack: b.slack,
                    until: frame + ORDER_FRAMES,
                };
                if self.incursions.orders.get(&d.squad).is_none_or(|o| o.tier != tier || o.choke != b.choke) {
                    self.log(&format!(
                        "intercept: squad {} (power {:.0}) blocks {} C{} {:.0}s ahead of {units} units (power {:.0}), \
                         saving {:.1} of {:.1}",
                        d.squad,
                        d.power,
                        tier.name(),
                        b.choke,
                        b.slack,
                        force.power,
                        plan.protected,
                        plan.protected + plan.exposed
                    ));
                }
                orders.push((d.squad, order));
            }
            // Where nobody blocks, meet it at the first asset it can reach
            // (if our army can take it on).
            let arrival = arrival_times(map, &force, held);
            if plan.exposed > 0.0 && force.power <= self.war.available_mobile_threat {
                let reach = arrival_times(map, &force, |c| held(c) || plan.blocks.iter().any(|b| b.choke == c));
                let first = assets
                    .iter()
                    .filter(|a| a.side == Side::Ally)
                    .filter_map(|a| Some((a.pos, reach[map.region_near(a.pos, SNAP)?])))
                    .filter(|&(_, t)| t <= HORIZON)
                    .min_by(|a, b| a.1.total_cmp(&b.1));
                if let Some((p, _)) = first {
                    predicted.push(DefenseTarget::new(at(p), force.power, frame));
                }
            }
            list.push(Incursion { tier, force, units, arrival, protected: plan.protected, exposed: plan.exposed });
        }

        if frame / STATS_PERIOD != self.incursions.frame / STATS_PERIOD {
            self.log(&format!(
                "incursions: {} groups, {} threatening, {} with a strong enough squad free, {} blocked; \
                 squads per assessment: {:.1}, {:.1} at home",
                stats.groups,
                stats.threats,
                stats.matched,
                stats.blocked,
                stats.squads as f32 / (STATS_PERIOD / PERIOD) as f32,
                stats.home as f32 / (STATS_PERIOD / PERIOD) as f32,
            ));
            stats = Stats::default();
        }
        self.incursions.stats = stats;
        self.incursions.frame = frame;
        self.incursions.list = list;
        self.incursions.orders.extend(orders);
        self.war.predicted_defense_targets = predicted;
    }

    /// Where squad `sid` stands to block an incursion, if it has been sent.
    pub fn incursion_block(&self, sid: SquadId) -> Option<Vec3> {
        self.incursions.orders.get(&sid).filter(|o| o.until > self.frame).map(|o| o.pos)
    }

    /// Mark the incursions, the regions they reach within the horizon and
    /// the block orders (`kgbterrain threat` chat command).
    pub fn incursions_draw(&self) {
        let Some(terrain) = self.terrain() else { return };
        let draw = self.cb.map();
        let at = |p: Pos| Vec3::new(p.x, draw.elevation_at(p.x, p.z), p.z);
        for i in &self.incursions.list {
            let Some(map) = terrain.tier(i.tier) else { continue };
            draw.add_point(
                at(i.force.pos),
                &format!(
                    "{} {} units, power {:.0}, {:.0} elmos/s: saving {:.1} of {:.1}",
                    i.units,
                    i.tier.name(),
                    i.force.power,
                    i.force.speed,
                    i.protected,
                    i.protected + i.exposed
                ),
            );
            for (r, t) in i.arrival.iter().enumerate().filter(|&(_, &t)| t > 0.0 && t <= HORIZON) {
                draw.add_point(at(map.regions[r].center), &format!("R{r}: {} in {t:.0}s", i.tier.name()));
            }
        }
        for (sid, o) in &self.incursions.orders {
            draw.add_point(o.pos, &format!("squad {sid} blocks C{} ({:.0}s ahead)", o.choke, o.slack));
        }
        self.say(&format!(
            "Frame {}: {} incursions, {} block orders.",
            self.incursions.frame,
            self.incursions.list.len(),
            self.incursions.orders.len()
        ));
    }
}
