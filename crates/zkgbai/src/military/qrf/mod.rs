//! Quick-reaction forces: a small mobile force, not replenished, that keeps
//! enemy raiders off our economy by where it stands and where it goes,
//! rather than by killing them.
//!
//! This is the shared part: the force (its members, in teams), enemy
//! contacts on radar or in sight with their true velocities (the engine
//! gives an AI the velocity of a radar dot, and its type once it was seen
//! and stayed on radar since; others are guessed from the fastest they were
//! seen to go), the contacts grouped into raids, our assets, travel times
//! over the ground ([`field`]), and orders that are not repeated needlessly.
//! How the force is used is up to an *algorithm* (AI option `qrf_algo`):
//! each keeps its state in [`Qrf`] and its code in a file of its own.
//!
//! Two modes:
//! - *staged*, in the raid-defense game (`test=qrf`, see `raid_test`): the
//!   force is given and never replenished (`disable=qrf`: the usual
//!   handlers take it instead);
//! - *live*, in normal play (see [`live`]): the force is raiders drawn from
//!   production by a defense budget, and raider squads still forming,
//!   which answer raids while they wait to fill (moonlighting). Off with
//!   `disable=qrf`; steered by the operator ([`ops`]).

pub mod cover;
pub mod field;
pub mod intent;
pub mod live;
pub mod mark;
pub mod ops;
pub mod station;

use std::collections::BTreeMap;

use springai::{Unit, UnitDef, Vec3, reserved};

use crate::Ai;
use crate::military::SquadId;
use crate::util::distance;
use field::{Field, Grid, Mobility};

/// Frames between updates of contacts and raids.
pub const PERIOD: i32 = 15;

/// A contact unseen this long is forgotten.
const FORGET: i32 = 30 * 6;

/// Contacts this close (elmos) are one raid (single linkage).
const LINK: f32 = 400.0;

/// An order to the same place is repeated after this many frames at most;
/// to a new place only if it is this far from the old one.
const REORDER: i32 = 30 * 3;
const REORDER_DIST: f32 = 64.0;

/// Frames between summary log lines.
const LOG_PERIOD: i32 = 30 * 30;

/// One of ours.
pub struct Member {
    pub unit: Unit,
    pub def: UnitDef,
    pub team: usize,
    pub cost: f32,
    /// Elmos per second.
    pub speed: f32,
    pub mobility: Mobility,
    pub range: f32,
    /// The raider squad it is in, if it moonlights (live mode).
    pub squad: Option<SquadId>,
    /// Weapon power, as the threat maps count it (see `Fighter::new`).
    pub threat: f32,
    order: Option<(Vec3, bool, i32)>,
}

impl Member {
    pub fn pos(&self) -> Vec3 {
        self.unit.pos()
    }

    /// Metal it is worth now, by its health.
    pub fn power(&self) -> f32 {
        let max = self.unit.max_health().max(1.0);
        self.cost * (self.unit.health() / max).clamp(0.0, 1.0)
    }
}

/// An enemy ground unit on radar or in sight.
pub struct Track {
    pub unit: Unit,
    pub pos: Vec3,
    /// Elmos per second.
    pub vel: Vec3,
    pub first_seen: i32,
    pub last_seen: i32,
    /// The fastest it has been seen to go, elmos per second.
    pub top_speed: f32,
    /// Its type, if the engine tells it.
    pub def: Option<UnitDef>,
    /// Its type as guessed from its top speed, while unknown.
    pub guess: Option<UnitDef>,
    pub in_los: bool,
    /// Health, known in sight; else its type's.
    pub health: f32,
}

impl Track {
    pub fn kind(&self) -> Option<UnitDef> {
        self.def.or(self.guess)
    }

    /// Metal: its type's cost; a typical raider's if nothing is known.
    pub fn cost(&self, metal: springai::Resource) -> f32 {
        self.kind().map_or(100.0, |d| d.cost(metal))
    }

    /// Metal it is worth now, by its health.
    pub fn power(&self, metal: springai::Resource) -> f32 {
        let Some(d) = self.kind() else { return 100.0 };
        d.cost(metal) * (self.health / d.health().max(1.0)).clamp(0.0, 1.0)
    }

    /// Top speed of its type (elmos per second), or the fastest seen.
    pub fn max_speed(&self) -> f32 {
        self.kind().map_or(self.top_speed, |d| d.speed()).max(self.top_speed)
    }
}

/// Contacts near each other: a raid.
pub struct RaidGroup {
    /// Track ids.
    pub members: Vec<i32>,
    pub center: Vec3,
    /// Mean velocity, elmos per second.
    pub vel: Vec3,
    /// Its slowest member's top speed.
    pub speed: f32,
    pub power: f32,
    pub mobility: Mobility,
}

/// Something of ours worth defending.
#[derive(Clone, Copy)]
pub struct Asset {
    pub unit: Unit,
    pub pos: Vec3,
    pub cost: f32,
    pub is_mex: bool,
}

#[derive(Default)]
pub struct Qrf {
    pub on: bool,
    pub algo: String,
    /// Members per team.
    pub team_size: usize,
    pub members: BTreeMap<i32, Member>,
    pub tracks: BTreeMap<i32, Track>,
    pub groups: Vec<RaidGroup>,
    pub assets: Vec<Asset>,
    pub grid: Option<Grid>,
    /// Types a contact may be, slowest first: armed mobile ground units.
    candidates: Vec<(f32, UnitDef)>,
    pub initial_members: usize,
    /// Seconds between trace lines (0: none).
    pub trace: i32,
    pub station: station::Station,
    pub cover: cover::Cover,
    pub mark: mark::Mark,
    pub intent: intent::Intent,
    /// Normal play (else the staged game).
    pub live: bool,
    pub budget: live::Budget,
    /// The operator's roles for teams.
    pub roles: BTreeMap<usize, live::Role>,
    /// Moonlighting squads sent at a raid, with the frame last sent.
    pub drafted: BTreeMap<SquadId, i32>,
    /// What each team did last (for the operator and the log).
    pub doing: BTreeMap<usize, live::Doing>,
    pub next_team: usize,
    /// Moonlighting is off (operator).
    pub no_moonlight: bool,
    pub live_stats: live::Stats,
    /// Teams are being given back (none are taken meanwhile).
    releasing: bool,
    stats: Stats,
}

/// Types a contact may be, slowest first: armed mobile ground units.
fn candidates(cb: springai::Callback) -> Vec<(f32, UnitDef)> {
    let mut c: Vec<(f32, UnitDef)> = (cb.unit_defs().into_iter())
        .filter(|d| d.speed() > 0.0 && !d.is_able_to_fly() && d.build_options().is_empty() && d.num_weapon_mounts() > 0)
        .map(|d| (d.speed(), d))
        .collect();
    c.sort_by(|a, b| a.0.total_cmp(&b.0));
    c
}

#[derive(Default)]
struct Stats {
    orders: u32,
    guessed: u32,
    guessed_right: u32,
}

impl Ai {
    /// Whether the force is the QRF's (in the staged game, unless disabled).
    pub fn qrf_enabled(&self) -> bool {
        self.qrf.on
    }

    /// Set up from the AI options once the game is staged.
    pub fn qrf_start(&mut self) {
        let on = !self.dev_off("qrf");
        let q = &mut self.qrf;
        q.on = on;
        q.algo = self.cb.option("qrf_algo").unwrap_or_else(|| "station".into());
        q.team_size = self.cb.option("qrf_team").and_then(|v| v.parse().ok()).unwrap_or(3).max(1);
        q.grid = Some(Grid::new(self.cb));
        q.trace = self.cb.option("qrf_trace").and_then(|v| v.parse().ok()).unwrap_or(0);
        q.candidates = candidates(self.cb);
        let algo = q.algo.clone();
        self.log(&format!("qrf: on={} algo={algo} team={}", self.qrf.on, self.qrf.team_size));
    }

    /// Take a unit into the force.
    pub fn qrf_add(&mut self, unit: Unit) {
        let Some(def) = unit.def() else { return };
        let Some(mobility) = Mobility::of(def) else { return };
        let n = self.qrf.members.len();
        let team = n / self.qrf.team_size;
        self.qrf.members.insert(
            unit.id(),
            Member {
                unit,
                def,
                team,
                cost: def.cost(self.metal),
                speed: def.speed(),
                mobility,
                range: def.max_weapon_range(),
                squad: None,
                threat: 0.0,
                order: None,
            },
        );
        self.qrf.initial_members = self.qrf.initial_members.max(self.qrf.members.len());
        // Ours to move: no running home to be repaired (there is no one to
        // repair it), no usual handler.
        unit.set_move_state(1, 0);
        unit.set_fire_state(2, 0);
    }

    /// Whether the unit is the QRF's own (moonlighters are their squads').
    pub fn qrf_claims(&self, id: i32) -> bool {
        self.qrf.on && self.qrf.members.get(&id).is_some_and(|m| m.squad.is_none())
    }

    pub fn qrf_remove_unit(&mut self, id: i32) {
        self.qrf.members.remove(&id);
    }

    pub fn qrf_enemy_destroyed(&mut self, id: i32) {
        self.qrf.tracks.remove(&id);
    }

    pub fn qrf_update(&mut self, frame: i32) {
        if frame % PERIOD != self.offset % PERIOD {
            return;
        }
        if !self.qrf.on {
            if !self.qrf_live_wanted(frame) {
                return;
            }
            self.qrf_live_start();
        }
        if self.qrf.grid.is_none() {
            return;
        }
        if self.qrf.live && self.dev_off("qrf") {
            self.qrf_live_stop();
            return;
        }
        let team = self.team_id;
        self.qrf.members.retain(|_, m| m.unit.health() > 0.0 && m.unit.team() == team);
        if self.qrf.live {
            self.qrf_live_roster(frame);
        }
        self.qrf_track(frame);
        self.qrf_group();
        self.qrf_assets();
        if self.qrf.live {
            self.qrf_live_filter(frame);
            self.qrf_live_budget(frame);
        }
        match self.qrf.algo.as_str() {
            "station" => self.qrf_station_update(frame),
            "cover" => self.qrf_cover_update(frame),
            "mark" => self.qrf_mark_update(frame),
            "intent" => self.qrf_intent_update(frame),
            other => {
                if frame % LOG_PERIOD < PERIOD {
                    self.log(&format!("qrf: no algorithm {other}, using station"));
                }
                self.qrf_station_update(frame)
            }
        }
        if self.qrf.live {
            self.qrf_live_after(frame);
        }
        if frame % LOG_PERIOD < PERIOD {
            self.qrf_log(frame);
        }
        if self.qrf.trace > 0 && frame % (self.qrf.trace * 30) < PERIOD {
            self.qrf_trace(frame);
        }
    }

    /// With `qrf_trace=<seconds>`: where every team and raid is, so often.
    fn qrf_trace(&mut self, frame: i32) {
        let teams = self.qrf_teams();
        let ts: Vec<String> = (teams.iter())
            .map(|(t, ids)| {
                let (p, _) = self.qrf_team_pos(ids);
                format!("{t}:{}@{:.0},{:.0}", ids.len(), p.x, p.z)
            })
            .collect();
        let rs: Vec<String> = (self.qrf.groups.iter())
            .map(|g| format!("{}@{:.0},{:.0}v{:.0},{:.0}", g.members.len(), g.center.x, g.center.z, g.vel.x, g.vel.z))
            .collect();
        self.log(&format!("qrftrace: t={} teams=[{}] raids=[{}]", frame / 30, ts.join(" "), rs.join(" ")));
    }

    /// Contacts: positions, velocities, top speeds, types.
    fn qrf_track(&mut self, frame: i32) {
        for u in self.cb.enemy_units_in_radar_and_los() {
            let def = u.def();
            if def.is_some_and(|d| d.speed() <= 0.0 || d.is_able_to_fly()) {
                continue;
            }
            let pos = u.pos();
            // Aircraft on radar: well above the ground.
            if def.is_none() && pos.y - self.cb.map().elevation_at(pos.x, pos.z).max(0.0) > 60.0 {
                continue;
            }
            let v = u.vel();
            let vel = Vec3::new(v.x * 30.0, 0.0, v.z * 30.0);
            let speed = vel.length_2d();
            let health = u.health();
            let t = self.qrf.tracks.entry(u.id()).or_insert(Track {
                unit: u,
                pos,
                vel,
                first_seen: frame,
                last_seen: frame,
                top_speed: 0.0,
                def: None,
                guess: None,
                in_los: false,
                health: 0.0,
            });
            t.pos = pos;
            t.vel = vel;
            t.last_seen = frame;
            t.top_speed = t.top_speed.max(speed);
            if let Some(d) = def
                && t.def.is_none()
            {
                // The guess, scored the first time the type is known.
                if let Some(g) = t.guess {
                    self.qrf.stats.guessed += 1;
                    if g.id() == d.id() || (g.speed() - d.speed()).abs() < 3.0 {
                        self.qrf.stats.guessed_right += 1;
                    }
                }
                t.def = Some(d);
            }
            t.in_los = health > 0.0;
            t.health = if health > 0.0 { health } else { t.kind().map_or(300.0, |d| d.health()) };
        }
        self.qrf.tracks.retain(|_, t| frame - t.last_seen < FORGET);
        // Guess the unknown by top speed: the slowest type that goes that
        // fast (raiders go flat out).
        let cands = std::mem::take(&mut self.qrf.candidates);
        for t in self.qrf.tracks.values_mut().filter(|t| t.def.is_none() && frame - t.first_seen >= 45) {
            let s = t.top_speed;
            if s < 20.0 {
                continue;
            }
            t.guess = cands.iter().find(|(v, _)| *v >= s * 0.97).or(cands.last()).map(|(_, d)| *d);
            if t.health <= 0.0 || !t.in_los {
                t.health = t.kind().map_or(300.0, |d| d.health());
            }
        }
        self.qrf.candidates = cands;
    }

    /// Contacts near each other, as raids.
    fn qrf_group(&mut self) {
        let ids: Vec<i32> = self.qrf.tracks.keys().copied().collect();
        let pos: Vec<Vec3> = ids.iter().map(|id| self.qrf.tracks[id].pos).collect();
        let mut label: Vec<usize> = (0..ids.len()).collect();
        fn root(l: &mut [usize], mut i: usize) -> usize {
            while l[i] != i {
                l[i] = l[l[i]];
                i = l[i];
            }
            i
        }
        for i in 0..ids.len() {
            for j in i + 1..ids.len() {
                if distance(pos[i], pos[j]) < LINK {
                    let (a, b) = (root(&mut label, i), root(&mut label, j));
                    label[a] = b;
                }
            }
        }
        let mut by_root: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
        for i in 0..ids.len() {
            let r = root(&mut label, i);
            by_root.entry(r).or_default().push(i);
        }
        let metal = self.metal;
        self.qrf.groups = (by_root.into_values())
            .map(|idx| {
                let n = idx.len() as f32;
                let (mut c, mut v, mut speed, mut power) = (Vec3::ZERO, Vec3::ZERO, f32::INFINITY, 0.0);
                let mut mobility = None;
                for &i in &idx {
                    let t = &self.qrf.tracks[&ids[i]];
                    c += t.pos * (1.0 / n);
                    v += t.vel * (1.0 / n);
                    speed = speed.min(t.max_speed().max(30.0));
                    power += t.power(metal);
                    if mobility.is_none() {
                        mobility = t.kind().and_then(Mobility::of);
                    }
                }
                RaidGroup {
                    members: idx.iter().map(|&i| ids[i]).collect(),
                    center: c,
                    vel: v,
                    speed,
                    power,
                    mobility: mobility.unwrap_or(Mobility::BOT),
                }
            })
            .collect();
    }

    /// Our buildings (and, in normal play, our constructors).
    fn qrf_assets(&mut self) {
        let metal = self.metal;
        let live = self.qrf.live;
        self.qrf.assets = (self.cb.team_units().into_iter())
            .filter_map(|u| {
                let d = u.def()?;
                let worker = live && d.speed() > 0.0 && !d.build_options().is_empty() && !d.is_able_to_fly();
                ((d.speed() <= 0.0 || worker) && !u.is_being_built()).then(|| Asset {
                    unit: u,
                    pos: u.pos(),
                    cost: d.cost(metal),
                    is_mex: self.defs.name(d) == "staticmex",
                })
            })
            .collect();
    }

    /// Members by team: those at hand (not gone home to be repaired, not
    /// taken by the operator).
    pub fn qrf_teams(&self) -> BTreeMap<usize, Vec<i32>> {
        let mut teams: BTreeMap<usize, Vec<i32>> = BTreeMap::new();
        for (&id, m) in &self.qrf.members {
            if self.qrf.live && (self.retreat_handler.is_retreating(id) || reserved::is_reserved(self.cb, id)) {
                continue;
            }
            teams.entry(m.team).or_default().push(id);
        }
        teams
    }

    /// Where a team is (its members' mean position) and its slowest speed.
    pub fn qrf_team_pos(&self, ids: &[i32]) -> (Vec3, f32) {
        let ms: Vec<&Member> = ids.iter().filter_map(|id| self.qrf.members.get(id)).collect();
        let n = ms.len().max(1) as f32;
        let c = ms.iter().fold(Vec3::ZERO, |a, m| a + m.pos() * (1.0 / n));
        (c, ms.iter().map(|m| m.speed).fold(f32::INFINITY, f32::min))
    }

    pub fn qrf_team_power(&self, ids: &[i32]) -> f32 {
        ids.iter().filter_map(|id| self.qrf.members.get(id)).map(Member::power).sum()
    }

    /// Seconds for a team to get anywhere (its slowest member from each
    /// member's place).
    pub fn qrf_team_field(&self, ids: &[i32]) -> Option<Field> {
        self.qrf_team_field_within(ids, field::NEVER)
    }

    /// [`Ai::qrf_team_field`] out to `max` seconds.
    pub fn qrf_team_field_within(&self, ids: &[i32], max: f32) -> Option<Field> {
        let grid = self.qrf.grid.as_ref()?;
        let ms: Vec<&Member> = ids.iter().filter_map(|id| self.qrf.members.get(id)).collect();
        let m = ms.first()?;
        let speed = ms.iter().map(|m| m.speed).fold(f32::INFINITY, f32::min);
        let sources: Vec<(Vec3, f32)> = ms.iter().map(|m| (m.pos(), 0.0)).collect();
        Some(grid.field_within(m.mobility, speed, &sources, max))
    }

    /// Seconds for a raid to get anywhere.
    pub fn qrf_group_field(&self, g: &RaidGroup) -> Option<Field> {
        self.qrf_group_field_within(g, field::NEVER)
    }

    /// [`Ai::qrf_group_field`] out to `max` seconds.
    pub fn qrf_group_field_within(&self, g: &RaidGroup, max: f32) -> Option<Field> {
        let grid = self.qrf.grid.as_ref()?;
        Some(grid.field_within(g.mobility, g.speed, &[(g.center, 0.0)], max))
    }

    /// Order a member to `to`; fighting what it meets on the way if
    /// `fight`. Repeats nothing it was told lately.
    pub fn qrf_order(&mut self, id: i32, to: Vec3, fight: bool) {
        let frame = self.frame;
        let Some(m) = self.qrf.members.get_mut(&id) else { return };
        if let Some((p, f, at)) = m.order
            && f == fight
            && distance(p, to) < REORDER_DIST
            && frame - at < REORDER
        {
            return;
        }
        let y = self.cb.map().elevation_at(to.x, to.z);
        let to = Vec3::new(to.x, y, to.z);
        if fight {
            m.unit.fight(to, 0);
        } else {
            m.unit.move_to(to, 0);
        }
        m.order = Some((to, fight, frame));
        self.qrf.stats.orders += 1;
    }

    /// Order a whole team.
    pub fn qrf_order_team(&mut self, ids: &[i32], to: Vec3, fight: bool) {
        for &id in ids {
            self.qrf_order(id, to, fight);
        }
    }

    fn qrf_log(&mut self, frame: i32) {
        let q = &self.qrf;
        let power: f32 = q.members.values().map(Member::power).sum();
        let raids: Vec<String> = (q.groups.iter())
            .map(|g| format!("{}x{:.0}@{:.0},{:.0}", g.members.len(), g.power, g.center.x, g.center.z))
            .collect();
        let line = format!(
            "qrf: t={} algo={} members={}/{} power={:.0} tracks={} raids=[{}] orders={} guesses={}/{}",
            frame / 30,
            q.algo,
            q.members.values().filter(|m| m.squad.is_none()).count(),
            q.initial_members,
            power,
            q.tracks.len(),
            raids.join(" "),
            q.stats.orders,
            q.stats.guessed_right,
            q.stats.guessed,
        );
        self.log(&line);
    }
}
