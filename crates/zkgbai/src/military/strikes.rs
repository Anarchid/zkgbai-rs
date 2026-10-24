//! Assassination strikes: the moment we have a fix on a sniper (a Phantom,
//! a Lance), a wave of cheap fast units dives it, kills it, and whoever is
//! left goes back to whatever they were doing.
//!
//! The yardstick is metal per shot absorbed, not survival. A Phantom
//! (750 metal, 560 hp) fires once in 17 s, a Lance (1100, 1000 hp) once
//! in 23 s, so a wave that reaches it inside one reload loses one unit
//! at most on the way: a Glaive at 65 or a Blitz at 300 against 750 or
//! 1100. What fails is a wave too slow to close in a reload, or one dear
//! enough per unit that a shot costs more than the kill is worth.
//!
//! The wave is conscripted from whatever qualifies nearby (raiders in
//! their handlers, home-defense raiders with no raid to answer), and
//! released when the strike settles; a dive lasts seconds, not minutes.
//!
//! Enablers, by how exact the fix is: a sniper in sight (a Sparrow sweep
//! decloaks a Phantom, see `sweeps`; the Show Shooter gadget reveals a
//! Lance that fires inside our air LOS; a Phantom is never revealed by
//! firing, `no_decloak_on_weapon_fire`); a radar dot that shot us; two
//! bearings of unseen shots crossing; one bearing (the Phantom stands
//! 250 to 720 elmos from what it hit, along the shot's direction: the
//! wave combs that line, 140 elmos apart, under its 155 decloak radius,
//! and the moment it shows the whole wave is on it).
//!
//! The gate: expected profit (the kill's chance times the target's metal,
//! less the expected loss) must be at least `strike_margin` (1.5) times
//! the expected loss. The loss is one shot's worth (the dearest member)
//! when nothing but the target guards the way, and the whole wave when
//! escorts or turrets do: conscripts are spent, not borrowed.

use std::collections::BTreeMap;

use springai::{Unit, Vec3, WeaponDef};

use crate::Ai;
use crate::military::FighterKind;
use crate::util::{NULLPOS, OPTION_SHIFT_KEY, distance};

/// Snipers worth a strike: def name, reload (s), range.
pub const SNIPERS: [(&str, f32, f32); 2] = [("cloaksnipe", 17.0, 700.0), ("hoverarty", 23.0, 980.0)];

/// Frames between updates.
const PERIOD: i32 = 15;

/// An impactor: at most this much metal, at least this fast (elmos/s),
/// armed, on the ground.
const IMPACTOR_COST: f32 = 350.0;
const IMPACTOR_SPEED: f32 = 80.0;

/// It must reach the fix within this many seconds of now (a reload, less
/// the time the fix is already old), and the wave kills within this many
/// more; it brings this many times the target's health in that time.
const REACH_SECONDS: f32 = 15.0;
const KILL_SECONDS: f32 = 6.0;
const OVERKILL: f32 = 2.5;

/// Escorted, the wave is at least this many (the escort's fire spreads
/// over more of it); its size is then the one with the best expected
/// profit.
const ESCORT_MIN: usize = 4;

/// Sustained-damage estimates (`weapon_power`) against what is actually
/// dealt in a fight (misses, overkill, moving): this share. The wave is
/// under the escorts' guns this long before it reaches the target.
const EFFECTIVE: f32 = 0.4;
const CLOSE_SECONDS: f32 = 3.0;

/// Diving, the wave's members stand about this far apart: an escort's
/// splash of `aoe` catches about 1 + (aoe / CLUMP)^2 of them (a Mace's 8
/// one, a Reaver's 96 two or three, an Ogre's 160 five).
const CLUMP: f32 = 80.0;

/// After the kill, the survivors have this long to take the escorts too
/// (then half their worth is credited: six Blitzes take a Phantom AND
/// its Reaver).
const ESCORT_FIGHT: f32 = 8.0;
const ESCORT_CREDIT: f32 = 0.5;

/// What the sniper killed of ours lately (within this far, this many
/// frames back) counts toward the kill's worth at this share: it would
/// go on killing.
const AVERTED_RADIUS: f32 = 900.0;
const AVERTED_MEMORY: i32 = 30 * 90;
const AVERTED_SHARE: f32 = 0.5;

/// A wave is at most this many; a comb of a bearing at least this many.
const MAX_WAVE: usize = 12;
const COMB_MIN: usize = 3;

/// A sniper is worth this many times its metal: alive, it deletes a unit
/// of ours every reload for as long as it stands (the user's edge: five
/// Halberds for a Lance and a Mace is barely worth it; a dozen Glaives
/// for the Lance alone is fine).
const SNIPER_WORTH: f32 = 1.5;

/// Where a Phantom stands from what it hit, along the bearing, and how far
/// apart the wave combs it (under its 155 decloak radius).
const CONE_NEAR: f32 = 250.0;
const CONE_FAR: f32 = 720.0;
const CONE_STEP: f32 = 140.0;
const CONE_SIDE: f32 = 120.0;

/// Bearings of unseen shots are kept this long for crossing; two cross
/// when their victims stand this far apart and their directions differ
/// by this much (sine of the angle: about 12 degrees).
const BEARING_MEMORY: i32 = 30 * 20;
const BEARING_APART: f32 = 150.0;
const CROSS_MIN_SIN: f32 = 0.2;

/// A strike lapses this long after its last fix, or when a seen target
/// has run this far from it.
const LAPSE: i32 = 30 * 25;
const RAN: f32 = 1200.0;

/// A target struck and not killed is left alone this long; so is a
/// bearing's neighbourhood.
const COOLDOWN: i32 = 30 * 60;
const COOLDOWN_NEAR: f32 = 500.0;

/// Kill chances by fix: seen, a radar dot, a crossing, a bearing.
const P_SEEN: f32 = 0.8;
const P_RADAR: f32 = 0.7;
const P_CROSS: f32 = 0.6;
const P_CONE: f32 = 0.45;

/// Enemies within this far of the fix guard it (the target's own power
/// does not count: it is what the wave is for).
const ESCORT_RADIUS: f32 = 450.0;

/// The default gate margin (option `strike_margin`): profit at least
/// this many times the loss; 0 is break-even on the premium-weighted
/// worth. Negative: every wave goes.
const MARGIN: f32 = 0.0;

/// A seen target is approached from around: each member by its own
/// point this far from the target (a Lance's beam goes through a column;
/// a riot's splash through a clump), when it is still farther than that.
const SPREAD: f32 = 200.0;

/// The way to the fix is sampled this many times for turrets (stage3: a
/// wave drafted from raiders at the enemy's fort died to its Lotuses on
/// the way to a Phantom 1000 elmos off).
const WAY_SAMPLES: i32 = 6;

/// Dirtbags (`shieldscout`: 30 metal, 560 hp, speed 84, a 48-damage
/// headbutt, and any hit decloaks): the sponge of a wave, built by a
/// shield factory while snipers have been seen this lately, up to this
/// many alive.
pub const DIRTBAG: &str = "shieldscout";
const DIRTBAGS: usize = 6;
const SNIPERS_LATELY: i32 = 30 * 240;

/// Orders are repeated this often, and refusals logged at most this often.
const REORDER: i32 = 30 * 2;
const REFUSAL_LOG: i32 = 30 * 20;

/// How the target was fixed.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Fix {
    /// In sight.
    Seen(Vec3),
    /// A radar dot that shot us.
    Radar(Vec3),
    /// Two bearings of unseen shots crossing.
    Cross(Vec3),
    /// One bearing: from the victim, along the shot.
    Cone { from: Vec3, dir: Vec3 },
}

impl Fix {
    pub fn kind(&self) -> &'static str {
        match self {
            Fix::Seen(_) => "seen",
            Fix::Radar(_) => "on radar",
            Fix::Cross(_) => "crossed",
            Fix::Cone { .. } => "a bearing",
        }
    }

    /// The best single point.
    pub fn pos(&self) -> Vec3 {
        match *self {
            Fix::Seen(p) | Fix::Radar(p) | Fix::Cross(p) => p,
            Fix::Cone { from, dir } => along(from, dir, (CONE_NEAR + CONE_FAR) / 2.0, 0.0),
        }
    }

    /// Where the wave looks when the target is not in sight: a comb of
    /// the bearing, else a ring about the point.
    pub fn points(&self) -> Vec<Vec3> {
        match *self {
            Fix::Cone { from, dir } => cone_points(from, dir),
            Fix::Seen(p) | Fix::Radar(p) | Fix::Cross(p) => {
                let mut v = vec![p];
                for k in 0..6 {
                    let a = k as f32 * std::f32::consts::FRAC_PI_3;
                    v.push(Vec3::xz(p.x + a.cos() * CONE_SIDE, p.z + a.sin() * CONE_SIDE));
                }
                v
            }
        }
    }

    fn p_kill(&self) -> f32 {
        match self {
            Fix::Seen(_) => P_SEEN,
            Fix::Radar(_) => P_RADAR,
            Fix::Cross(_) => P_CROSS,
            Fix::Cone { .. } => P_CONE,
        }
    }
}

/// `from` moved `t` along `dir` and `s` to its right (xz).
fn along(from: Vec3, dir: Vec3, t: f32, s: f32) -> Vec3 {
    Vec3::xz(from.x + dir.x * t - dir.z * s, from.z + dir.z * t + dir.x * s)
}

/// The comb of a bearing: points along it from near to far, and one to
/// each side of its middle.
pub fn cone_points(from: Vec3, dir: Vec3) -> Vec<Vec3> {
    let mut v = Vec::new();
    let mut t = CONE_NEAR;
    while t <= CONE_FAR {
        v.push(along(from, dir, t, 0.0));
        t += CONE_STEP;
    }
    let mid = (CONE_NEAR + CONE_FAR) / 2.0;
    v.push(along(from, dir, mid, CONE_SIDE));
    v.push(along(from, dir, mid, -CONE_SIDE));
    v
}

/// Where two bearings cross, if their victims stand apart, their
/// directions differ enough, and the crossing is where a Phantom would
/// stand from both.
pub fn cross(a: (Vec3, Vec3), b: (Vec3, Vec3)) -> Option<Vec3> {
    let ((p1, d1), (p2, d2)) = (a, b);
    if distance(p1, p2) < BEARING_APART {
        return None;
    }
    let det = d1.x * d2.z - d1.z * d2.x;
    if det.abs() < CROSS_MIN_SIN {
        return None;
    }
    let (ex, ez) = (p2.x - p1.x, p2.z - p1.z);
    let t = (ex * d2.z - ez * d2.x) / det;
    let s = (ex * d1.z - ez * d1.x) / det;
    let ok = |u: f32| (CONE_NEAR - 80.0..=CONE_FAR + 80.0).contains(&u);
    (ok(t) && ok(s)).then(|| Vec3::xz(p1.x + d1.x * t, p1.z + d1.z * t))
}

/// One who could go.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Cand {
    pub id: i32,
    pub cost: f32,
    /// Seconds to the fix.
    pub arrive: f32,
    /// Sustained damage per second.
    pub power: f32,
    pub health: f32,
}

/// The wave: the cheapest first (then the soonest), until it brings
/// `need` damage per second, and at least `min` of them.
pub fn pick(cands: &[Cand], need: f32, min: usize) -> Vec<Cand> {
    let mut sorted: Vec<Cand> = cands.to_vec();
    sorted.sort_by(|a, b| a.cost.total_cmp(&b.cost).then(a.arrive.total_cmp(&b.arrive)));
    let mut wave = Vec::new();
    let mut power = 0.0;
    for c in sorted {
        if wave.len() >= MAX_WAVE || (power >= need && wave.len() >= min) {
            break;
        }
        power += c.power;
        wave.push(c);
    }
    if power < need || wave.len() < min { Vec::new() } else { wave }
}

/// What a strike is after: the target's health, its worth, the fix's
/// kill chance, and its reload (seconds).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Mark {
    pub health: f32,
    pub value: f32,
    pub p_kill: f32,
    pub reload: f32,
}

/// The wave of the best expected profit: `pick`'s wave, then one more of
/// the rest (cheapest first) while that improves the odds; `shots` are
/// recomputed from the slowest member.
pub fn pick_best(cands: &[Cand], need: f32, min: usize, mark: Mark, escorts: &[Escort]) -> (Vec<Cand>, Odds) {
    let Mark { health, value, p_kill, reload } = mark;
    let wave = pick(cands, need, min);
    if wave.is_empty() {
        return (Vec::new(), odds(&[], health, value, p_kill, 1, escorts));
    }
    let shots = |w: &[Cand]| ((w.iter().map(|c| c.arrive).fold(0.0, f32::max) / reload).ceil() as u32).max(1);
    let mut best = (wave.clone(), odds(&wave, health, value, p_kill, shots(&wave), escorts));
    let mut rest: Vec<Cand> = cands.iter().filter(|c| !wave.iter().any(|w| w.id == c.id)).copied().collect();
    rest.sort_by(|a, b| a.cost.total_cmp(&b.cost).then(a.arrive.total_cmp(&b.arrive)));
    let mut grown = wave;
    for c in rest {
        if grown.len() >= MAX_WAVE {
            break;
        }
        grown.push(c);
        let o = odds(&grown, health, value, p_kill, shots(&grown), escorts);
        if o.profit > best.1.profit {
            best = (grown.clone(), o);
        }
    }
    best
}

/// An enemy guarding the target: its power, its splash, and what it is.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Escort {
    pub power: f32,
    pub aoe: f32,
    pub health: f32,
    pub value: f32,
}

/// What `escorts` deal per second to a wave of `n` (in a fight, not on
/// paper): splash catches more of a bigger wave.
fn escort_fire(escorts: &[Escort], n: usize) -> f32 {
    (escorts.iter()).map(|e| e.power * EFFECTIVE * (1.0 + (e.aoe / CLUMP).powi(2)).min(n as f32)).sum()
}

/// The gate's reckoning for a wave.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Odds {
    pub value: f32,
    pub p_kill: f32,
    /// What the wave is expected to lose.
    pub loss: f32,
    /// The escorts' worth credited when the survivors can take them too.
    pub bonus: f32,
    pub profit: f32,
    pub quiet: bool,
}

/// The expected profit of `wave` against a target of `health` worth
/// `value` with `p_kill`, taking `shots` of its on the way (one unit's
/// worth each, the dearest) and the fire of `escorts` while it closes
/// and kills (their hits, at what is dealt in a fight and as many as
/// their splash catches, turned into metal at the wave's own metal per
/// health). The wave is not expected to kill when less than half of it
/// would be left; when what is left can take the escorts in a few
/// seconds more, half their worth is credited.
pub fn odds(wave: &[Cand], health: f32, value: f32, p_kill: f32, shots: u32, escorts: &[Escort]) -> Odds {
    let total: f32 = wave.iter().map(|c| c.cost).sum();
    let hp: f32 = wave.iter().map(|c| c.health).sum::<f32>().max(1.0);
    let power: f32 = wave.iter().map(|c| c.power).sum::<f32>().max(1.0);
    let dearest = wave.iter().map(|c| c.cost).fold(0.0, f32::max);
    let kill_time = health / (power * EFFECTIVE);
    let escort_hits = escort_fire(escorts, wave.len()) * (CLOSE_SECONDS + kill_time);
    let escort_loss = (escort_hits * total / hp).min(total);
    let loss = (shots as f32 * dearest + escort_loss).min(total);
    // (A sniper's shot takes one unit; the rest dive on.)
    let left = 1.0 - escort_loss / total.max(1.0);
    let p = if left >= 0.5 { p_kill } else { p_kill * 2.0 * left.max(0.0) };
    let escort_hp: f32 = escorts.iter().map(|e| e.health).sum();
    let escort_value: f32 = escorts.iter().map(|e| e.value).sum();
    let takes_them = escort_hp > 0.0 && escort_hp / (power * left.max(0.0) * EFFECTIVE) <= ESCORT_FIGHT;
    let bonus = if takes_them { ESCORT_CREDIT * escort_value } else { 0.0 };
    Odds { value, p_kill: p, loss, bonus, profit: p * (value + bonus) - loss, quiet: escorts.is_empty() }
}

/// Whether the gate opens: profit at least `margin` times the loss. A
/// negative margin opens it for any wave (to see what strikes the gate
/// refuses would do).
pub fn go(o: Odds, margin: f32) -> bool {
    margin < 0.0 || o.profit > 0.0 && o.profit >= margin * o.loss
}

/// A strike under way.
pub struct Strike {
    pub id: u32,
    /// The enemy, once known.
    pub target: Option<i32>,
    /// Its kind (def name).
    pub kind: &'static str,
    pub fix: Fix,
    /// When the fix was made, and the strike started.
    pub fixed: i32,
    pub started: i32,
    pub members: Vec<i32>,
    pub wave_cost: f32,
    pub lost: f32,
    pub lost_n: u32,
    pub odds: Odds,
    /// Each member's next point to look at.
    next_point: BTreeMap<i32, usize>,
    ordered: i32,
}

#[derive(Default)]
pub struct Stats {
    pub started: u32,
    pub killed: u32,
    pub lapsed: u32,
    pub wiped: u32,
    pub refused: u32,
    pub lost: f32,
    pub won: f32,
    /// Snipers of the enemy's that died, struck or not, and ours lost to
    /// snipers outside strikes.
    pub snipers_died: u32,
    pub snipers_seen: u32,
    pub sniped: u32,
    pub sniped_metal: f32,
}

#[derive(Default)]
pub struct Strikes {
    pub strikes: Vec<Strike>,
    /// Conscripts: by unit id, the strike and the unit's cost.
    pub members: BTreeMap<i32, (u32, Unit, f32)>,
    /// Bearings of unseen sniper shots: victim, direction, frame.
    bearings: Vec<(Vec3, Vec3, i32)>,
    /// What snipers killed of ours outside strikes: where, cost, when.
    sniped: Vec<(Vec3, f32, i32)>,
    /// Enemy snipers seen so far, by id, and when one was last seen.
    seen: std::collections::BTreeSet<i32>,
    last_sniper_seen: i32,
    /// Targets, and places, left alone until a frame.
    cooldown: BTreeMap<i32, i32>,
    cooldown_near: Vec<(Vec3, i32)>,
    next_id: u32,
    margin: f32,
    set: bool,
    last_refusal: i32,
    pub stats: Stats,
}

/// The sniper kind a weapon or unit name belongs to.
fn sniper_kind(name: &str) -> Option<(&'static str, f32, f32)> {
    SNIPERS.iter().copied().find(|(n, _, _)| name.starts_with(n))
}

impl Ai {
    fn strikes_on(&self) -> bool {
        !self.dev_off("strikes")
    }

    fn strikes_setup(&mut self) {
        if self.strikes.set {
            return;
        }
        self.strikes.set = true;
        self.strikes.margin = self.cb.option("strike_margin").and_then(|v| v.parse().ok()).unwrap_or(MARGIN);
    }

    /// One of ours was hit by a sniper: a fix on it, or a bearing.
    pub fn strikes_unit_damaged(&mut self, unit: Unit, attacker: Option<Unit>, dir: Vec3, weapon: Option<WeaponDef>) {
        if !self.strikes_on() {
            return;
        }
        let Some(kind) = weapon.and_then(|w| sniper_kind(&w.name())) else { return };
        let frame = self.frame;
        if let Some(a) = attacker.filter(|a| a.pos() != NULLPOS) {
            let id = a.id();
            let seen = a.def().is_some();
            let fix = if seen { Fix::Seen(a.pos()) } else { Fix::Radar(a.pos()) };
            if let Some(s) = self.strikes.strikes.iter_mut().find(|s| s.target == Some(id)) {
                s.fix = fix;
                s.fixed = frame;
                return;
            }
            self.strikes_consider(Some(id), kind.0, fix);
            return;
        }
        // Unseen: the bearing from the victim. A strike already combing
        // near it is refreshed; else bearings apart cross, else it is a
        // cone of its own.
        let from = unit.pos();
        let n = (dir.x * dir.x + dir.z * dir.z).sqrt();
        if n < 1e-3 {
            return;
        }
        let d = Vec3::xz(dir.x / n, dir.z / n);
        self.strikes.bearings.retain(|b| frame - b.2 < BEARING_MEMORY);
        let crossing = self.strikes.bearings.iter().find_map(|b| cross((b.0, b.1), (from, d)));
        self.strikes.bearings.push((from, d, frame));
        let fix = match crossing {
            Some(p) => Fix::Cross(p),
            None => Fix::Cone { from, dir: d },
        };
        let guess = fix.pos();
        if let Some(s) = (self.strikes.strikes.iter_mut())
            .filter(|s| s.target.is_none() || !matches!(s.fix, Fix::Seen(_)))
            .find(|s| distance(s.fix.pos(), guess) < COOLDOWN_NEAR)
        {
            if !matches!(s.fix, Fix::Seen(_)) {
                s.fix = fix;
            }
            s.fixed = frame;
            return;
        }
        self.strikes_consider(None, kind.0, fix);
    }

    /// An enemy came in sight: a sniper is a fix (a strike combing for it
    /// nearby takes it as its target).
    pub fn strikes_enemy_seen(&mut self, enemy: Unit) {
        let Some(def) = enemy.def() else { return };
        let name = self.defs.name(def);
        let Some(kind) = sniper_kind(name) else { return };
        let (id, p, frame) = (enemy.id(), enemy.pos(), self.frame);
        self.strikes.last_sniper_seen = frame;
        // Each sniper's first sighting is logged, strikes on or off: the
        // A/B's "seen vs killed".
        if self.strikes.seen.insert(id) {
            self.strikes.stats.snipers_seen += 1;
            let line = format!("strikes: a {} first seen at ({:.0}, {:.0}) at {}:{:02}", kind.0, p.x, p.z, frame / 1800, frame / 30 % 60);
            self.log(&line);
        }
        if !self.strikes_on() {
            return;
        }
        if let Some(s) = self.strikes.strikes.iter_mut().find(|s| s.target == Some(id)) {
            s.fix = Fix::Seen(p);
            s.fixed = frame;
            return;
        }
        if let Some(s) = (self.strikes.strikes.iter_mut())
            .filter(|s| s.target.is_none() && s.kind == kind.0)
            .min_by(|a, b| distance(a.fix.pos(), p).total_cmp(&distance(b.fix.pos(), p)))
            .filter(|s| distance(s.fix.pos(), p) < CONE_FAR + 300.0)
        {
            s.target = Some(id);
            s.fix = Fix::Seen(p);
            s.fixed = frame;
            let line = format!("strikes: #{} sees its {}: a {} at ({:.0}, {:.0})", s.id, kind.0, name, p.x, p.z);
            self.log(&line);
            return;
        }
        self.strikes_consider(Some(id), kind.0, Fix::Seen(p));
    }

    /// Weigh a strike on `target` (or an unseen one of `kind`) fixed by
    /// `fix`, and launch it when the gate opens.
    fn strikes_consider(&mut self, target: Option<i32>, kind: &'static str, fix: Fix) {
        self.strikes_setup();
        let frame = self.frame;
        let at = fix.pos();
        if target.is_some_and(|t| self.strikes.strikes.iter().any(|s| s.target == Some(t)))
            || self.strikes.strikes.iter().any(|s| distance(s.fix.pos(), at) < COOLDOWN_NEAR)
            || target.is_some_and(|t| self.strikes.cooldown.get(&t).is_some_and(|&f| f > frame))
            || self.strikes.cooldown_near.iter().any(|(p, f)| *f > frame && distance(*p, at) < COOLDOWN_NEAR)
        {
            return;
        }
        let (_, reload, _) = sniper_kind(kind).unwrap_or((kind, 20.0, 700.0));
        // The target's worth and health: as seen, else as its kind.
        let def = self.defs.get(kind);
        let (value, health) = match target.and_then(|t| self.war.targets.get(&t)) {
            Some(e) => {
                let max = e.ud.map_or(def.map_or(500.0, |d| d.health()), |d| d.health());
                (e.value.max(def.map_or(0.0, |d| d.cost(self.metal))), max * e.get_health())
            }
            None => (def.map_or(750.0, |d| d.cost(self.metal)), def.map_or(560.0, |d| d.health())),
        };
        // Escorts: enemies about the fix other than the target.
        let mut escorts: Vec<Escort> = (self.war.targets.iter())
            .filter(|(id, e)| Some(**id) != target && !e.is_dead && e.can_attack() && e.power > 0.0)
            .filter(|(_, e)| distance(e.position, at) < ESCORT_RADIUS && frame - e.last_seen < 30 * 90)
            .map(|(_, e)| {
                let aoe = (e.ud.map(|d| d.weapon_mounts()).unwrap_or_default().iter())
                    .filter_map(|m| m.weapon_def())
                    .map(|w| w.area_of_effect())
                    .fold(0.0, f32::max);
                let health = e.ud.map_or(1000.0, |d| d.health()) * e.get_health();
                Escort { power: e.power, aoe, health, value: e.value }
            })
            .collect();
        let escort: f32 = escorts.iter().map(|e| e.power).sum();
        let cands = self.strikes_candidates(at);
        let need = health * OVERKILL / KILL_SECONDS;
        let mut min = if matches!(fix, Fix::Cone { .. }) { COMB_MIN } else { 1 };
        if escort > 0.0 {
            min = min.max(ESCORT_MIN);
        }
        // Its worth: the premium on its metal, and what it has killed of
        // ours lately (which would go on dying).
        self.strikes.sniped.retain(|s| frame - s.2 < AVERTED_MEMORY);
        let averted: f32 =
            (self.strikes.sniped.iter()).filter(|s| distance(s.0, at) < AVERTED_RADIUS).map(|s| s.1).sum::<f32>() * AVERTED_SHARE;
        let value = value * SNIPER_WORTH + averted;
        // Turrets at the fix: the wave is as good as lost.
        if self.war.get_porc_threat(at) > 0.0 {
            escorts.push(Escort { power: 10_000.0, aoe: 0.0, health: 1e9, value: 0.0 });
        }
        let mark = Mark { health, value, p_kill: fix.p_kill(), reload };
        let (wave, o) = pick_best(&cands, need, min, mark, &escorts);
        let n_cands = cands.len();
        let log_refusal = |ai: &mut Ai, why: String| {
            ai.strikes.stats.refused += 1;
            if frame - ai.strikes.last_refusal >= REFUSAL_LOG {
                ai.strikes.last_refusal = frame;
                let line =
                    format!("strikes: no strike on the {kind} {} at ({:.0}, {:.0}): {why}", fix.kind(), at.x, at.z);
                ai.log(&line);
            }
        };
        if wave.is_empty() {
            let power: f32 = cands.iter().map(|c| c.power).sum();
            log_refusal(self, format!("{n_cands} in reach bring {power:.0}/s, {need:.0}/s wanted (escorts {escort:.0}/s)"));
            return;
        }
        let wave_power: f32 = wave.iter().map(|c| c.power).sum();
        let slowest = wave.iter().map(|c| c.arrive).fold(0.0, f32::max);
        let quiet = o.quiet;
        let margin = self.strikes.margin;
        if !go(o, margin) {
            log_refusal(
                self,
                format!(
                    "{} units ({:.0} metal) would lose {:.0} for {:.0} x {:.2}: profit {:.0} under {margin} x loss{}",
                    wave.len(),
                    wave.iter().map(|c| c.cost).sum::<f32>(),
                    o.loss,
                    o.value,
                    o.p_kill,
                    o.profit,
                    if quiet { String::new() } else { format!(" (escorts {escort:.0}/s)") }
                ),
            );
            return;
        }
        let ids: Vec<i32> = wave.iter().map(|c| c.id).collect();
        let id = self.strikes.next_id;
        self.strikes.next_id += 1;
        self.strikes_conscript(id, &ids);
        let wave_cost: f32 = wave.iter().map(|c| c.cost).sum();
        self.strikes.strikes.push(Strike {
            id,
            target,
            kind,
            fix,
            fixed: frame,
            started: frame,
            members: ids,
            wave_cost,
            lost: 0.0,
            lost_n: 0,
            odds: o,
            next_point: BTreeMap::new(),
            ordered: 0,
        });
        self.strikes.stats.started += 1;
        let line = format!(
            "strikes: #{id} on a {kind} {} at ({:.0}, {:.0}): {} units ({wave_cost:.0} metal, {wave_power:.0}/s) go, \
             the slowest in {slowest:.0} s; expected loss {:.0}, profit {:.0} ({} x {:.0}, averted {averted:.0}){}",
            fix.kind(),
            at.x,
            at.z,
            wave.len(),
            o.loss,
            o.profit,
            o.p_kill,
            o.value,
            if quiet { String::new() } else { format!(", escorts {escort:.0}/s") }
        );
        self.log(&line);
        self.strikes_drive(self.strikes.strikes.len() - 1);
    }

    /// A Dirtbag from a shield factory about to build `choice` (a plain
    /// fighter), while snipers have been seen lately and we have fewer
    /// than a wave's worth: the cheapest thing to take a sniper's shot
    /// or a riot's splash, and a hit from it decloaks.
    pub fn strikes_production(&self, factory: Unit, choice: &str) -> Option<&'static str> {
        if !self.strikes_on() || choice == DIRTBAG || self.strikes.last_sniper_seen == 0 {
            return None;
        }
        if self.frame - self.strikes.last_sniper_seen > SNIPERS_LATELY {
            return None;
        }
        let def = self.defs.get(DIRTBAG)?;
        if !factory.def()?.build_options().iter().any(|d| d.id() == def.id()) {
            return None;
        }
        let have = (self.fighters.values().filter(|f| f.unit.def().is_some_and(|d| d.id() == def.id())).count())
            + (self.strikes.members.values().filter(|m| m.1.def().is_some_and(|d| d.id() == def.id())).count());
        (have < DIRTBAGS).then_some(DIRTBAG)
    }

    /// Who could dive `at` in time: cheap, fast, armed ground units of
    /// ours in their handlers (not retreating, not sent at a raid, not a
    /// shield ball's, not the operator's), and home-defense raiders with
    /// nothing to do.
    fn strikes_candidates(&self, at: Vec3) -> Vec<Cand> {
        let mut cands = Vec::new();
        let mut consider = |id: i32, unit: Unit, cost: f32, speed: f32, power: f32| {
            if self.strikes.members.contains_key(&id) || springai::reserved::is_reserved(self.cb, id) {
                return;
            }
            let Some(def) = unit.def() else { return };
            if cost > IMPACTOR_COST
                || speed < IMPACTOR_SPEED
                || power <= 0.0
                || def.is_able_to_fly()
                || unit.is_being_built()
                || unit.health() <= 0.0
                || self.retreat_is_retreating(unit)
            {
                return;
            }
            let name = self.defs.name(def);
            if self.unit_classes.sappers.contains(name) || name == "spiderantiheavy" {
                return;
            }
            let p = unit.pos();
            let arrive = distance(p, at) * 1.2 / speed;
            if arrive > REACH_SECONDS {
                return;
            }
            // Not through turrets.
            let under_guns = (1..WAY_SAMPLES).any(|k| {
                let t = k as f32 / WAY_SAMPLES as f32;
                self.war.get_porc_threat(Vec3::xz(p.x + (at.x - p.x) * t, p.z + (at.z - p.z) * t)) > 0.0
            });
            if !under_guns {
                cands.push(Cand { id, cost, arrive, power, health: unit.health().max(1.0) });
            }
        };
        for f in self.fighters.values() {
            if !matches!(f.kind, FighterKind::Basic | FighterKind::Raider) || f.shield_mob {
                continue;
            }
            if f.squad.is_some_and(|sid| self.qrf_drafted(sid)) {
                continue;
            }
            let Some(def) = f.unit.def() else { continue };
            consider(f.id, f.unit, f.metal_value, def.speed(), f.power);
        }
        for (&id, m) in &self.qrf.members {
            if m.squad.is_some() || self.qrf.doing.contains_key(&m.team) {
                continue;
            }
            consider(id, m.unit, m.cost, m.speed, m.threat);
        }
        cands
    }

    /// Take `ids` out of their handlers into strike `strike`.
    fn strikes_conscript(&mut self, strike: u32, ids: &[i32]) {
        for &id in ids {
            let unit = match self.fighters.get(&id).map(|f| f.unit).or_else(|| self.qrf.members.get(&id).map(|m| m.unit))
            {
                Some(u) => u,
                None => continue,
            };
            let cost = unit.def().map_or(0.0, |d| d.cost(self.metal));
            if self.qrf.members.contains_key(&id) {
                self.qrf_remove_unit(id);
            }
            self.raider_remove_unit(unit);
            self.squad_remove_unit(unit);
            self.misc_remove_unit(unit);
            self.jaeger_remove_unit(unit);
            self.fighters.remove(&id);
            self.war.new_units.remove(&id);
            // A dive is not a place to be a coward.
            self.retreat_remove_unit(unit);
            unit.set_move_state(2, 0);
            unit.set_fire_state(2, 0);
            self.strikes.members.insert(id, (strike, unit, cost));
        }
    }

    /// Give a strike's survivors back to their handlers.
    fn strikes_release(&mut self, ids: &[i32]) {
        for id in ids {
            let Some((_, unit, _)) = self.strikes.members.remove(id) else { continue };
            if unit.health() <= 0.0 {
                continue;
            }
            unit.set_move_state(1, 0);
            self.war.new_units.insert(*id);
            self.war_unit_idle(unit);
        }
    }

    /// One of ours died: a conscript is counted against its strike.
    pub fn strikes_unit_destroyed(&mut self, unit: Unit, attacker: Option<Unit>) {
        let id = unit.id();
        let frame = self.frame;
        if let Some((strike, _, cost)) = self.strikes.members.remove(&id) {
            if let Some(s) = self.strikes.strikes.iter_mut().find(|s| s.id == strike) {
                s.members.retain(|m| *m != id);
                s.lost += cost;
                s.lost_n += 1;
                s.next_point.remove(&id);
            }
            self.strikes.stats.lost += cost;
            return;
        }
        // Sniped outside a strike: what the strikes are to stop.
        let sniper = attacker.and_then(|a| a.def()).map(|d| self.defs.name(d).to_owned()).filter(|n| sniper_kind(n).is_some());
        if let Some(n) = sniper {
            let cost = unit.def().map_or(0.0, |d| d.cost(self.metal));
            self.strikes.stats.sniped += 1;
            self.strikes.stats.sniped_metal += cost;
            self.strikes.sniped.push((unit.pos(), cost, frame));
            let line = format!("strikes: a {} ({cost:.0}) lost to a {n} at {}:{:02}", self.defs.name_of(unit), frame / 1800, frame / 30 % 60);
            self.log(&line);
        }
    }

    /// An enemy died: a struck sniper settles its strike.
    pub fn strikes_enemy_destroyed(&mut self, enemy: Unit, _attacker: Option<Unit>) {
        let id = enemy.id();
        let name = self.war.targets.get(&id).map(|e| e.def_name.clone()).unwrap_or_default();
        let struck = self.strikes.strikes.iter().position(|s| s.target == Some(id));
        if sniper_kind(&name).is_some() {
            self.strikes.stats.snipers_died += 1;
            let frame = self.frame;
            let line = format!(
                "strikes: a {name} died {} at {}:{:02}",
                if struck.is_some() { "struck" } else { "unstruck" },
                frame / 1800,
                frame / 30 % 60
            );
            self.log(&line);
        }
        if let Some(i) = struck {
            self.strikes_settle(i, "killed");
        }
    }

    /// End strike `i`: `why` is killed, lapsed, wiped or ran.
    fn strikes_settle(&mut self, i: usize, why: &str) {
        let s = self.strikes.strikes.remove(i);
        let frame = self.frame;
        let st = &mut self.strikes.stats;
        match why {
            "killed" => {
                st.killed += 1;
                st.won += s.odds.value;
            }
            "wiped" => st.wiped += 1,
            _ => st.lapsed += 1,
        }
        if why != "killed" {
            if let Some(t) = s.target {
                self.strikes.cooldown.insert(t, frame + COOLDOWN);
            }
            self.strikes.cooldown_near.push((s.fix.pos(), frame + COOLDOWN));
        }
        let line = format!(
            "strikes: #{} {why}: the {} {} after {} s; lost {:.0} metal ({} of {} units), {} back",
            s.id,
            s.kind,
            if why == "killed" { "dead" } else { "not" },
            (frame - s.started) / 30,
            s.lost,
            s.lost_n,
            s.lost_n as usize + s.members.len(),
            s.members.len()
        );
        self.log(&line);
        self.strikes_release(&s.members);
    }

    /// Order strike `i`: at the target while it is in sight, else over
    /// the fix's points.
    fn strikes_drive(&mut self, i: usize) {
        let frame = self.frame;
        let (target, fix, members, ordered) = {
            let s = &self.strikes.strikes[i];
            (s.target, s.fix, s.members.clone(), s.ordered)
        };
        let seen = target
            .and_then(|t| self.war.targets.get(&t))
            .filter(|e| e.visible && !e.is_dead && e.unit.health() > 0.0 && e.unit.pos() != NULLPOS)
            .map(|e| e.unit);
        if let Some(t) = seen {
            if frame - ordered < REORDER {
                return;
            }
            self.strikes.strikes[i].ordered = frame;
            let tp = t.pos();
            self.strikes.strikes[i].fix = Fix::Seen(tp);
            self.strikes.strikes[i].fixed = frame;
            let n = members.len().max(1);
            for (k, id) in members.iter().enumerate() {
                let Some(&(_, u, _)) = self.strikes.members.get(id) else { continue };
                // A jumper (a Dirtbag: 400 elmos at twice its walking
                // speed) jumps onto the target.
                if self.jump_strike(u, t, tp, "strike") {
                    continue;
                }
                // From around, not in a column: by its own point beside
                // the target first, while it is still far.
                let a = std::f32::consts::TAU * k as f32 / n as f32;
                let side = Vec3::xz(tp.x + a.cos() * SPREAD, tp.z + a.sin() * SPREAD);
                if distance(u.pos(), tp) > 1.5 * SPREAD {
                    u.fight(side, 0);
                    u.attack(t, OPTION_SHIFT_KEY);
                } else {
                    u.attack(t, 0);
                    u.fight(tp, OPTION_SHIFT_KEY);
                }
            }
            return;
        }
        let points = fix.points();
        for (k, id) in members.iter().enumerate() {
            let Some(&(_, u, _)) = self.strikes.members.get(id) else { continue };
            let s = &mut self.strikes.strikes[i];
            let next = s.next_point.entry(*id).or_insert(k % points.len());
            let p = points[*next % points.len()];
            if distance(u.pos(), p) < 60.0 {
                *next += 1;
                let q = points[*next % points.len()];
                if !self.jump_to(u, q, "strike comb") {
                    u.fight(q, 0);
                }
            } else if frame - ordered >= REORDER && !self.jump_to(u, p, "strike comb") {
                u.fight(p, 0);
            }
        }
        if frame - ordered >= REORDER {
            self.strikes.strikes[i].ordered = frame;
        }
    }

    pub fn strikes_update(&mut self, frame: i32) {
        if frame % PERIOD != 0 || self.strikes.strikes.is_empty() {
            return;
        }
        let mut i = 0;
        while i < self.strikes.strikes.len() {
            let s = &self.strikes.strikes[i];
            let ran = s.target.and_then(|t| self.war.targets.get(&t)).is_some_and(|e| {
                e.visible && distance(e.unit.pos(), s.fix.pos()) > RAN
            });
            // (A kill is heard of as the enemy dies; a target forgotten as
            // outdated is one that got away.)
            let gone = s.target.is_some_and(|t| !self.war.targets.contains_key(&t));
            let why = if s.members.is_empty() {
                Some("wiped")
            } else if gone {
                Some("lapsed")
            } else if ran {
                Some("ran")
            } else if frame - s.fixed > LAPSE {
                Some("lapsed")
            } else {
                None
            };
            if let Some(why) = why {
                self.strikes_settle(i, why);
                continue;
            }
            self.strikes_drive(i);
            i += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(id: i32, cost: f32, arrive: f32, power: f32) -> Cand {
        Cand { id, cost, arrive, power, health: cost * 3.5 }
    }

    const REAVER: Escort = Escort { power: 400.0, aoe: 96.0, health: 820.0, value: 210.0 };
    const MACE: Escort = Escort { power: 280.0, aoe: 8.0, health: 1000.0, value: 400.0 };
    const OGRE: Escort = Escort { power: 900.0, aoe: 160.0, health: 1950.0, value: 500.0 };

    #[test]
    fn the_wave_is_the_cheapest_that_brings_the_damage() {
        let cands = [c(1, 300.0, 3.0, 60.0), c(2, 65.0, 8.0, 40.0), c(3, 65.0, 5.0, 40.0), c(4, 130.0, 4.0, 70.0)];
        let wave = pick(&cands, 100.0, 1);
        assert_eq!(wave.iter().map(|c| c.id).collect::<Vec<_>>(), vec![3, 2, 4]);
        assert!(pick(&cands, 1000.0, 1).is_empty());
        assert_eq!(pick(&cands, 10.0, 3).len(), 3);
        assert!(pick(&cands[..2], 10.0, 3).is_empty());
    }

    #[test]
    fn a_quiet_way_costs_one_shot_an_escorted_one_its_hits() {
        let wave = [c(1, 65.0, 5.0, 100.0), c(2, 65.0, 6.0, 100.0), c(3, 300.0, 4.0, 150.0)];
        let q = odds(&wave, 560.0, 750.0, P_SEEN, 1, &[]);
        assert_eq!(q.loss, 300.0);
        assert!((q.profit - (0.8 * 750.0 - 300.0)).abs() < 1e-3);
        // A Blitz among the Glaives makes a Phantom shot dear; at
        // break-even on the plain metal it still goes.
        assert!(go(q, MARGIN) && !go(q, 1.5));
        // Glaives alone: the loss is one Glaive.
        assert!(go(odds(&wave[..2], 560.0, 750.0, P_SEEN, 1, &[]), MARGIN));
        // Six Glaives at a Phantom with a Reaver beside it: its splash
        // catches two or three at a time, the wave is mostly gone before
        // the kill: a donation.
        let six: Vec<Cand> = (1..=6).map(|k| c(k, 65.0, 5.0, 109.0)).collect();
        let e = odds(&six, 560.0, 750.0 * SNIPER_WORTH, P_SEEN, 1, &[REAVER]);
        assert!(e.p_kill < 0.4 && !go(e, MARGIN), "{e:?}");
        // Two Glaives there: wiped before the kill.
        let two = odds(&six[..2], 560.0, 750.0, P_SEEN, 1, &[REAVER]);
        assert!(two.p_kill < 0.3 && !go(two, MARGIN), "{two:?}");
        // Three Glaives at a lone Phantom: an easy go even on a bearing.
        let glaives = [c(1, 65.0, 5.0, 100.0), c(2, 65.0, 6.0, 100.0), c(3, 65.0, 7.0, 100.0)];
        assert!(go(odds(&glaives, 560.0, 750.0, P_CONE, 1, &[]), MARGIN));
        // A Blitz at a Lance: 800 up.
        assert!(go(odds(&[c(1, 300.0, 9.0, 200.0)], 1000.0, 1100.0, P_SEEN, 1, &[]), MARGIN));
    }

    #[test]
    fn the_users_edge_a_dozen_glaives_for_a_lance_five_halberds_for_a_lance_and_a_mace() {
        // Twelve Glaives at a Lance with a Mace (riot, ~280/s estimated)
        // beside it: the Mace eats a few Glaives, the Lance dies. Worth is
        // the premium-weighted metal.
        let dozen: Vec<Cand> = (1..=12).map(|k| c(k, 65.0, 6.0, 109.0)).collect();
        let lance = Mark { health: 1000.0, value: 1100.0 * SNIPER_WORTH, p_kill: P_SEEN, reload: 23.0 };
        let (wave, o) = pick_best(&dozen, 1000.0 * OVERKILL / KILL_SECONDS, ESCORT_MIN, lance, &[MACE]);
        assert!(wave.len() >= 4 && o.p_kill == P_SEEN && go(o, MARGIN), "{} {o:?}", wave.len());
        // The same dozen at a Lance beside an Ogre: the splash takes five
        // at a time, the wave is gone before the kill: a donation.
        let (_, o) = pick_best(&dozen, 1000.0 * OVERKILL / KILL_SECONDS, ESCORT_MIN, lance, &[OGRE]);
        assert!(!go(o, MARGIN), "{o:?}");
        // Five Halberds (210 each, armoured 1450 hp) for the Lance and the
        // Mace: the Lance dies, and they take the Mace too: a go.
        let halberds: Vec<Cand> = (1..=5).map(|k| Cand { id: k, cost: 210.0, arrive: 6.0, power: 250.0, health: 1450.0 }).collect();
        let o = odds(&halberds, 1000.0, 1100.0 * SNIPER_WORTH, P_SEEN, 1, &[MACE]);
        assert!(o.bonus > 0.0 && go(o, MARGIN), "{o:?}");
        // Five Halberds for a Phantom alone: one shot, one Halberd.
        let o = odds(&halberds, 560.0, 750.0 * SNIPER_WORTH, P_SEEN, 1, &[]);
        assert!(go(o, MARGIN) && o.loss == 210.0, "{o:?}");
        // Six Blitzes at a Phantom with a Reaver: on paper shy, in
        // practice they take both; the escort credit says go.
        let blitzes: Vec<Cand> = (1..=6).map(|k| Cand { id: k, cost: 300.0, arrive: 6.0, power: 250.0, health: 1300.0 }).collect();
        let o = odds(&blitzes, 560.0, 750.0 * SNIPER_WORTH, P_SEEN, 1, &[REAVER]);
        assert!(o.bonus > 0.0 && o.p_kill == P_SEEN && go(o, MARGIN), "{o:?}");
    }

    #[test]
    fn bearings_apart_cross_where_a_phantom_stands() {
        let a = (Vec3::xz(0.0, 0.0), Vec3::xz(0.0, 1.0));
        let b = (Vec3::xz(400.0, 0.0), Vec3::xz(-0.6, 0.8));
        let p = cross(a, b).expect("cross");
        assert!((p.x).abs() < 1.0 && (p.z - 533.3).abs() < 1.0);
        // Too close together, or parallel: no crossing.
        assert!(cross(a, (Vec3::xz(50.0, 0.0), Vec3::xz(-0.6, 0.8))).is_none());
        assert!(cross(a, (Vec3::xz(400.0, 0.0), Vec3::xz(0.0, 1.0))).is_none());
        // Crossing behind the victims: not where a sniper stands.
        assert!(cross(a, (Vec3::xz(400.0, 0.0), Vec3::xz(-0.6, -0.8))).is_none());
    }

    #[test]
    fn a_cone_is_combed_under_the_decloak_radius() {
        let pts = cone_points(Vec3::xz(0.0, 0.0), Vec3::xz(1.0, 0.0));
        assert_eq!(pts.len(), 6);
        for w in pts[..4].windows(2) {
            assert!(distance(w[0], w[1]) < 155.0);
        }
        assert!(pts[0].x >= CONE_NEAR && pts[3].x <= CONE_FAR);
        assert!((pts[4].z - CONE_SIDE).abs() < 1e-3 && (pts[5].z + CONE_SIDE).abs() < 1e-3);
    }
}
