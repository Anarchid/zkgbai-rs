//! Jaeger squads: hunters of the enemy's heavies.
//!
//! Once heavies (Grizzlies, Cyclopes, Jacks, Jugglenauts, striders...) are
//! a good part of the enemy's army seen lately, or simply many, the
//! factories we have build anti-heavy units, for up to half the heavies'
//! metal: Widows (`spiderantiheavy`, 280 metal: an 8000 stun every 35 s at
//! range 120, and ZK stuns on max health) from a spider factory, and
//! Ultimatums (`striderantiheavy`, 2400 metal: 2000 a shot every 2 s at
//! range 250) from a strider hub. Skuttles (`jumpbomb`: an 8000 blast) are
//! taken in while heavies are about. Every Ultimatum is a jaeger. Widows
//! only stun, the army does the killing: they are kept to a share of the
//! rest of our army's metal (feature `widowcap`).
//!
//! Jaegers walk cloaked and hold their fire, in squads of a kind, which
//! form until they can take the heavies seen. A squad strikes a heavy
//! together once it can take it: Widows when their stuns together pass its
//! health, Skuttles when their blasts do, an Ultimatum any. A stunned heavy
//! is the army's to kill (it paints no threat while stunned, and a defense
//! target calls our squads), so stuns and Ultimatums go only where our army
//! or ground is near. Widows require extra support under enemy defenses;
//! Ultimatums avoid them, while Skuttles die striking anyway. Widows that
//! have fired go back to a haven until they are reloaded. Skuttles are too frail and slow to wait for
//! their squad's strike where the enemy is about: each pounces on any heavy
//! worth one within its jump, its squad's mark or not, even one its blast
//! alone does not kill (feature `pounce`), and jumps where the heavy will
//! be when it lands, by its velocity (feature `lead`).
//!
//! Hunters lie in wait rather than go after a heavy into the enemy's land
//! (feature `ambush`; supported Widows can join our army on either side
//! of the lines): its way over the ground is foreseen (a travel-time
//! field from it, as the QRF's, down to the asset of ours it heads for by
//! the QRF mark's rule), and the squad waits beside the first point of it
//! that it gets to, settled, before the heavy: out of enemy land and
//! out of the enemy's sight now, avoiding defenses unless supported Widows
//! can take the opportunity, and for Widows only where our
//! army can kill the heavy while it is stunned (a stun with nothing to
//! finish it is wasted; they strike only there). Cloaked hunters keep out
//! of sight of what moves (feature `dodge`): one that an enemy would
//! decloak within two seconds (a Skuttle shows itself to anything within
//! 180, a Widow within 60) steps aside, where it stays clearest of them all;
//! a Skuttle that cannot get clear on foot jumps clear, since its jumps
//! are silent and keep it cloaked.

use std::collections::{BTreeMap, BTreeSet};

use springai::{Resource, Unit, Vec3};

use crate::Ai;
use crate::military::jumps::SKUTTLE_PRESS;
use crate::military::qrf::field::CELL;
use crate::military::qrf::field::Mobility;
use crate::military::{DefenseTarget, Enemy, Fighter, FighterStore, SquadId, SquadKind, Squads, scouts};
use crate::nav;
use crate::terrain::reaches;
use crate::util::{NULLPOS, distance, get_directional_point};

mod opportunistic;
pub(crate) use opportunistic::BiteAction;

/// Frames between updates.
const PERIOD: i32 = 15;

/// Enemies seen within this many frames count toward the army seen.
const RECENT: i32 = 30 * 180;

/// A heavy: a mobile ground or sea unit of at least this much health and
/// metal (not a commander).
const HEAVY_HEALTH: f32 = 4500.0;
const HEAVY_COST: f32 = 600.0;

/// Jaegers are wanted from this much heavy metal seen, when it is this
/// share of the enemy's army seen; or from this much alone.
const HEAVY_VALUE: f32 = 2000.0;
const HEAVY_SHARE: f32 = 0.25;
const HEAVY_ALONE: f32 = 6000.0;

/// Jaegers get up to this share of the heavies' metal, and are at most
/// this many.
const JAEGER_SHARE: f32 = 0.5;
const MAX_JAEGERS: usize = 8;

/// Widows' metal, ordered ones too, is at most this share of the rest of
/// our army's (a fifth of the whole).
const WIDOW_ARMY_SHARE: f32 = 0.25;

/// A pounce holds its Skuttle out of its squad, its blast counted on the
/// heavy, this many frames (a jump flies about 2.5 s).
const POUNCE_HOLD: i32 = 30 * 5;

/// A heavy is pounced on only if seen within this many frames.
const POUNCE_SEEN: i32 = 30;

/// A pounce, leading its heavy, may land this much farther off than the
/// heavy is now.
const POUNCE_LEAD: f32 = 120.0;

/// No pounce where more of our own metal than the heavy's stands this close
/// to it: the blast (8000 in the middle, 40% at 180) would take it too.
/// (Other Skuttles go off with it, onto the heavy as well.)
const POUNCE_FRIENDS: f32 = 120.0;

/// A squad grows to at most this many.
const SQUAD_MAX: usize = 4;

/// An order not met in this many frames is forgotten.
const ORDER_TIMEOUT: i32 = 30 * 240;

/// A heavy unseen this long is not hunted (it has moved on).
const STALE: i32 = 30 * 15;

/// A squad strikes from this far beyond its range, and gives up a strike
/// after this many frames.
const STRIKE_RANGE: f32 = 300.0;
const STRIKE_TIMEOUT: i32 = 30 * 25;

/// A squad with no heavy to go for waits this far behind our army.
const WAIT_BEHIND: f32 = 400.0;

/// Our army within this distance of a heavy gets to it while it is
/// stunned (30 s).
const SUPPORT_RANGE: f32 = 1500.0;

/// A hunted heavy's way is foreseen this many seconds ahead, anew every
/// this many frames.
const WAY_HORIZON: f32 = 50.0;
const AMBUSH_PERIOD: i32 = 60;

/// A squad lies in wait where it is settled this many seconds before the
/// heavy comes by, its walk there reckoned this much longer than the
/// straight line.
const AMBUSH_SETTLE: f32 = 4.0;
const AMBUSH_DETOUR: f32 = 1.3;

/// Hunters in wait strike once the heavy is this far beyond their range.
const AMBUSH_STRIKE: f32 = 100.0;

/// Our army can kill a stunned heavy where our power (see
/// `get_total_friendly_power`: our units' weapon power and health over ten) is
/// at least this share of the heavy's health over ten: a few units by it
/// (1.0 kept Widows from ever striking against Circuit's tanks).
const FINISH_SHARE: f32 = 0.25;

/// A hunter dodges an enemy it would be seen by (within its decloak
/// distance and this margin) in the next this many frames, looking at
/// enemies this much farther off; on foot it steps this many frames of its
/// walk aside, or a Skuttle jumps this far.
const DODGE_MARGIN: f32 = 40.0;
const DODGE_AHEAD: i32 = 60;
const DODGE_LOOK: f32 = 300.0;
const DODGE_STEP: f32 = 60.0;
const DODGE_JUMPS: [f32; 2] = [260.0, 340.0];

/// A kind of jaeger.
#[derive(Clone, Copy)]
struct Kind {
    name: &'static str,
    /// What one strike does to a heavy (stun or damage); `None`: it wears
    /// down any heavy.
    alpha: Option<f32>,
    /// Its strike stuns rather than kills.
    stuns: bool,
    /// It dies striking, so it goes under enemy defenses as well.
    suicide: bool,
    /// Weapon range, elmos.
    range: f32,
    /// Only heavies of at least this much metal are worth one (a Skuttle
    /// costs 550: not for a 600-metal Jack).
    min_value: f32,
    /// Enemies this close decloak it (`minCloakDistance`); 0: it does not
    /// dodge.
    decloak: f32,
    /// It lies in wait this far beside the heavy's way (out of the sight of
    /// what walks along it, in reach of the heavy); 0: it does not.
    ambush: f32,
}

const KINDS: [Kind; 3] = [
    Kind {
        name: "spiderantiheavy",
        alpha: Some(8000.1),
        stuns: true,
        suicide: false,
        range: 120.0,
        min_value: 0.0,
        decloak: 60.0,
        ambush: 90.0,
    },
    Kind {
        name: "jumpbomb",
        alpha: Some(8002.4),
        stuns: false,
        suicide: true,
        range: 25.0,
        min_value: 1000.0,
        decloak: 180.0,
        ambush: 260.0,
    },
    Kind {
        name: "striderantiheavy",
        alpha: None,
        stuns: false,
        suicide: false,
        range: 250.0,
        min_value: 0.0,
        decloak: 0.0,
        ambush: 0.0,
    },
];

/// The friendly-power grid uses raw danger; porc threat is divided by 500.
/// Reserve support for the defenses as well as for finishing the stunned heavy.
fn widow_support_required(health: f32, porc_threat: f32) -> f32 {
    FINISH_SHARE * health / 10.0 + 500.0 * porc_threat
}

fn kind_of(name: &str) -> Option<Kind> {
    KINDS.iter().copied().find(|k| k.name == name)
}

/// Metal cost of an identified enemy, by its def (`value` stays a
/// placeholder for contacts first met on radar, when cheats name them).
fn cost(e: &Enemy, metal: Resource) -> f32 {
    e.ud.map_or(e.value, |d| d.cost(metal))
}

/// Whether an enemy is a heavy.
pub(crate) fn is_heavy(e: &Enemy, metal: Resource) -> bool {
    is_army(e) && e.def_health() >= HEAVY_HEALTH && cost(e, metal) >= HEAVY_COST
}

/// Whether an enemy counts toward its army: armed ground or sea units, not
/// workers or commanders.
fn is_army(e: &Enemy) -> bool {
    e.identified && !e.is_static && !e.can_fly() && e.can_attack() && !e.is_worker && !e.is_com
}

/// How many of a kind take a heavy of `health`: `None` if more than a
/// squad.
fn needed(kind: Kind, health: f32) -> Option<usize> {
    let n = kind.alpha.map_or(1, |a| (health / a).floor() as usize + 1);
    (n <= SQUAD_MAX).then_some(n)
}

/// An enemy army unit seen lately.
struct Seen {
    value: f32,
    /// Max health, if it is a heavy.
    heavy: Option<f32>,
    last_seen: i32,
}

#[derive(Clone, Copy, PartialEq)]
enum Phase {
    /// Gathering at a haven until it can take the heavies seen.
    Forming,
    /// After a heavy (enemy id), or waiting by our army for one.
    Hunting(Option<i32>),
    /// Attacking a heavy (enemy id) since a frame.
    Striking(i32, i32),
    /// Fired: at a haven until reloaded.
    Reloading,
}

struct Hunt {
    kind: Kind,
    phase: Phase,
    /// Where it lies in wait for its heavy, if it does.
    ambush: Option<Ambush>,
}

/// A place to lie in wait for a heavy.
#[derive(Clone, Copy)]
struct Ambush {
    /// The heavy (enemy id).
    target: i32,
    /// Where the squad waits, and the point of the heavy's way beside it.
    at: Vec3,
    way: Vec3,
    /// Seconds until the heavy gets there, as foreseen at `frame`.
    eta: f32,
    frame: i32,
}

#[derive(Default)]
pub struct Jaegers {
    opportunistic: opportunistic::Bites,
    /// Enemy army units seen lately, by enemy id.
    seen: BTreeMap<i32, Seen>,
    /// Fighter-store ids of our jaegers.
    members: BTreeSet<i32>,
    /// Our jaeger squads and what each is doing.
    hunts: BTreeMap<SquadId, Hunt>,
    /// The squad forming for each kind.
    forming: BTreeMap<&'static str, SquadId>,
    /// Jaegers ordered from factories, and when.
    ordered: Vec<(&'static str, i32)>,
    /// Skuttles under way onto a heavy, pouncing or striking with their
    /// squad: the heavy (enemy id), and since when.
    pounces: BTreeMap<i32, (i32, i32)>,
    /// Since the last summary: ambushes laid, dodges on foot and by jump.
    ambushes: u32,
    dodges: u32,
    dodge_jumps: u32,
    /// Hunter updates with an enemy about to see it.
    threatened: u32,
    /// Each hunter's cloak as last seen: cloaked, since when, its health.
    cloak: BTreeMap<i32, (bool, i32, f32)>,
    /// Jaegers out of squads on their own (a Skuttle that jumped when
    /// shown), until a frame.
    held: BTreeMap<i32, i32>,
}

impl Jaegers {
    /// A completed hunter may be diverted to spotting before jaeger_claims.
    /// Consume that order then, so it cannot claim a later scouting Widow.
    pub(crate) fn order_filled(&mut self, name: &str, frame: i32) -> bool {
        self.ordered.retain(|o| frame - o.1 <= ORDER_TIMEOUT);
        if let Some(k) = self.ordered.iter().position(|o| o.0 == name) {
            self.ordered.remove(k);
            return true;
        }
        false
    }

    fn forget_empty(&mut self, sid: SquadId, squads: &mut Squads, fighters: &FighterStore) {
        if squads.get(sid).is_none_or(|s| s.is_dead(fighters)) {
            self.hunts.remove(&sid);
            self.forming.retain(|_, f| *f != sid);
            squads.remove(sid);
        }
    }

    fn set_phase(&mut self, sid: SquadId, phase: Phase, squads: &mut Squads) {
        if let Some(h) = self.hunts.get_mut(&sid) {
            h.phase = phase;
        }
        if let Some(s) = squads.get_mut(sid) {
            s.status = match phase {
                Phase::Forming => 'f',
                Phase::Hunting(None) | Phase::Reloading => 'r',
                Phase::Hunting(Some(_)) | Phase::Striking(..) => 'a',
            };
        }
    }
}

impl Ai {
    /// A Widow hunt is a warning to nearby finishers to gather before EMP lands.
    pub(crate) fn jaeger_stun_expected(&self, target: i32) -> bool {
        self.jaegers.hunts.values().any(|h| {
            h.kind.stuns
                && (matches!(h.phase, Phase::Hunting(Some(id)) | Phase::Striking(id, _) if id == target)
                    || h.ambush.as_ref().is_some_and(|a| a.target == target && a.eta <= 30.0))
        })
    }

    /// Metal of heavies and of the whole army seen lately.
    fn jaeger_heavy_value(&self) -> (f32, f32) {
        let seen = self.jaegers.seen.values();
        let heavy = seen.clone().filter(|s| s.heavy.is_some()).map(|s| s.value).sum();
        (heavy, seen.map(|s| s.value).sum())
    }

    /// The most of a kind that the heavies seen need, if any heavy seen
    /// is one a squad of it takes.
    fn jaeger_needed(&self, kind: Kind) -> Option<usize> {
        (self.jaegers.seen.values())
            .filter(|s| s.value >= kind.min_value)
            .filter_map(|s| s.heavy)
            .filter_map(|h| needed(kind, h))
            .max()
    }

    /// Whether a factory that can build `name` should build it now.
    pub fn jaeger_wants(&self, name: &str) -> bool {
        let Some(kind) = kind_of(name) else { return false };
        if self.dev_off("jaeger") || self.jaeger_needed(kind).is_none() {
            return false;
        }
        let (heavy, army) = self.jaeger_heavy_value();
        if heavy < HEAVY_ALONE && (heavy < HEAVY_VALUE || heavy < HEAVY_SHARE * army) {
            return false;
        }
        let frame = self.frame;
        let orders = self.jaegers.ordered.iter().filter(|o| frame - o.1 <= ORDER_TIMEOUT);
        let ordered: f32 = orders.clone().filter_map(|o| self.defs.get(o.0)).map(|d| d.cost(self.metal)).sum();
        let have: f32 = self.jaegers.members.iter().filter_map(|id| self.fighters.get(id)).map(|f| f.metal_value).sum();
        if kind.stuns && !self.dev_off("widowcap") && !self.jaeger_widow_fits() {
            return false;
        }
        self.jaegers.members.len() + orders.count() < MAX_JAEGERS && have + ordered < JAEGER_SHARE * heavy
    }

    /// Metal of our Widows (jaegers and those ordered) and of the rest of
    /// our army, Skuttles not counted (they do their own killing).
    fn jaeger_widow_metal(&self) -> (f32, f32) {
        let frame = self.frame;
        let one = self.defs.get(scouts::WIDOW).map_or(0.0, |d| d.cost(self.metal));
        let ordered = self.jaegers.ordered.iter().filter(|o| o.0 == scouts::WIDOW && frame - o.1 <= ORDER_TIMEOUT);
        let (mut widows, mut skuttles) = (ordered.count() as f32 * one, 0.0);
        for f in self.jaegers.members.iter().filter_map(|id| self.fighters.get(id)) {
            match self.defs.name_of(f.unit) {
                scouts::WIDOW => widows += f.metal_value,
                "jumpbomb" => skuttles += f.metal_value,
                _ => {}
            }
        }
        (widows, (self.fac.army_value - widows - skuttles).max(0.0))
    }

    /// Whether one more Widow fits within the Widows' share of our army.
    fn jaeger_widow_fits(&self) -> bool {
        let one = self.defs.get(scouts::WIDOW).map_or(0.0, |d| d.cost(self.metal));
        let (widows, rest) = self.jaeger_widow_metal();
        widows + one <= WIDOW_ARMY_SHARE * rest
    }

    /// Cloak upkeep of our jaegers on the move, energy a second, those on
    /// order counted (so that the store is full when they are out): the
    /// energy reserve asked of the game holds a few seconds of it (see
    /// `scouts_reserve`; feature `jaegerreserve`). Without it the store
    /// runs dry while we build, and a cloak paid from it flickers out.
    pub fn jaeger_upkeep(&self) -> f32 {
        if self.dev_off("jaegerreserve") {
            return 0.0;
        }
        let frame = self.frame;
        let have: f32 = (self.jaegers.members.iter().filter_map(|id| self.fighters.get(id)))
            .filter(|f| f.unit.health() > 0.0)
            .filter_map(|f| f.unit.def())
            .map(|d| d.cloak_cost_moving())
            .sum();
        let ordered: f32 = (self.jaegers.ordered.iter())
            .filter(|o| frame - o.1 <= ORDER_TIMEOUT)
            .filter_map(|o| self.defs.get(o.0))
            .map(|d| d.cloak_cost_moving())
            .sum();
        have + ordered
    }

    /// A factory has been told to build jaeger `name`.
    pub fn jaeger_ordered(&mut self, name: &str) {
        if let Some(kind) = kind_of(name) {
            self.jaegers.ordered.push((kind.name, self.frame));
        }
    }

    /// Whether a new unit of type `name` is ours to command: an Ultimatum,
    /// a Widow we ordered, or a Skuttle while heavies are about.
    pub fn jaeger_claims(&mut self, name: &str) -> bool {
        let Some(kind) = kind_of(name) else { return false };
        if self.jaegers.order_filled(name, self.frame) {
            return true;
        }
        match kind.name {
            "striderantiheavy" => true,
            "jumpbomb" => self.jaeger_wants(name),
            _ => false,
        }
    }

    /// A new jaeger: it holds its fire (it would show itself) and its
    /// ground, until its squad strikes.
    pub fn jaeger_add(&mut self, unit: Unit, cost: f32) {
        unit.set_move_state(0, 0);
        if self.defs.name_of(unit) == "striderantiheavy" {
            // It is costly and lives through a strike: it goes home hurt.
            unit.set_fire_state(1, 0);
            self.retreat_add_coward(unit);
        } else {
            unit.set_fire_state(0, 0);
        }
        let f = Fighter::new_raider(unit, cost, self.team_id);
        self.jaegers.members.insert(f.id);
        self.fighters.insert(f.id, f);
    }

    pub fn jaeger_remove_unit(&mut self, unit: Unit) {
        let id = unit.id();
        self.jaegers.opportunistic.forget(id);
        self.jaegers.held.remove(&id);
        self.jaegers.pounces.remove(&id);
        if let Some((cloaked, since, _)) = self.jaegers.cloak.remove(&id)
            && unit.health() <= 0.0
        {
            let p = unit.pos();
            self.log(&format!(
                "jaeger: a {} died at ({:.0}, {:.0}), {} for {:.1} s",
                self.defs.name_of(unit),
                p.x,
                p.z,
                if cloaked { "cloaked" } else { "shown" },
                (self.frame - since) as f32 / 30.0
            ));
        }
        if self.jaegers.members.remove(&id) {
            self.jaeger_leave_squad(id);
            self.fighters.remove(&id);
        }
    }

    pub fn jaeger_enemy_destroyed(&mut self, id: i32) {
        self.jaegers.seen.remove(&id);
    }

    /// Jaeger `id` (a fighter-store id; others are let be) leaves its squad
    /// and stays out of squads for `frames`, to act on its own. Call it
    /// before ordering the unit: a squad's leader leaving is stopped.
    pub fn jaeger_hold(&mut self, id: i32, frames: i32) {
        if self.jaegers.members.contains(&id) {
            self.jaeger_leave_squad(id);
            self.jaegers.held.insert(id, self.frame + frames);
        }
    }

    /// Skuttle `id` (a fighter-store id) goes onto enemy `eid` on its own,
    /// as a pounce: out of its squad (call it before ordering the unit), its
    /// blast counted on the enemy, and attacking again should it land idle.
    pub fn jaeger_pounced(&mut self, id: i32, eid: i32) {
        if self.jaegers.members.contains(&id) {
            self.jaeger_leave_squad(id);
            self.jaegers.pounces.insert(id, (eid, self.frame));
        }
    }

    fn jaeger_leave_squad(&mut self, id: i32) {
        let Some(sid) = self.fighters.get(&id).and_then(|f| f.squad) else { return };
        let mut nav = nav!(self);
        if let Some(s) = self.squads.get_mut(sid) {
            s.remove_unit_nav(id, &mut self.fighters, &mut nav);
        }
        // Recruitment can move the last hunter into a taskforce immediately.
        // Retire its old squad before that fighter id is reused there.
        self.jaegers.forget_empty(sid, &mut self.squads, &self.fighters);
    }

    pub fn jaeger_update(&mut self, frame: i32) {
        if (frame + self.offset + 11) % PERIOD != 0 {
            return;
        }
        self.jaeger_remember(frame);
        if frame % 1800 < PERIOD {
            let (heavy, army) = self.jaeger_heavy_value();
            if heavy > 0.0 || !self.jaegers.members.is_empty() {
                let (widows, rest) = self.jaeger_widow_metal();
                let j = &self.jaegers;
                // Cloak is paid from storage (15 energy a second moving, 5
                // still): an empty one shows them.
                let cloaked =
                    (j.members.iter().filter_map(|id| self.fighters.get(id))).filter(|f| f.unit.is_cloaked()).count();
                let e = self.cb.economy();
                self.log(&format!(
                    "jaeger: {heavy:.0} of {army:.0} enemy army metal seen lately is heavy; {} jaegers ({cloaked} cloaked; energy {:.0} of {:.0}), {} squads, {} ordered; Widows {widows:.0} to {rest:.0} of the rest of our army; {} ambushes laid, {} threatened, {} dodges, {} dodge jumps",
                    j.members.len(),
                    e.current(self.energy),
                    e.storage(self.energy),
                    j.hunts.len(),
                    j.ordered.len(),
                    j.ambushes,
                    j.threatened,
                    j.dodges,
                    j.dodge_jumps
                ));
            }
            let j = &mut self.jaegers;
            (j.ambushes, j.threatened, j.dodges, j.dodge_jumps) = (0, 0, 0, 0);
        }
        if self.jaegers.members.is_empty() && self.jaegers.hunts.is_empty() {
            return;
        }
        self.jaeger_muster();
        let sids: Vec<SquadId> = self.jaegers.hunts.keys().copied().collect();
        for sid in sids {
            self.jaeger_assign(sid, frame);
        }
        // After the squads' orders, which a pounce or a dodge overrides.
        self.jaeger_pounce(frame);
        self.jaeger_dodge(frame);
        self.jaeger_watch_cloak(frame);
    }

    /// Keep count of the enemy's army seen lately.
    fn jaeger_remember(&mut self, frame: i32) {
        for (&id, e) in &self.war.targets {
            if !e.is_dead && is_army(e) {
                let heavy = is_heavy(e, self.metal).then(|| e.def_health());
                self.jaegers.seen.insert(id, Seen { value: cost(e, self.metal), heavy, last_seen: e.last_seen });
            }
        }
        self.jaegers.seen.retain(|_, s| frame - s.last_seen <= RECENT);
    }

    /// Drop the lost and the retreating from their squads, put the rest
    /// that have none in the squad forming for their kind, and drop the
    /// squads left empty.
    fn jaeger_muster(&mut self) {
        let team = self.team_id;
        let frame = self.frame;
        self.jaegers.held.retain(|_, &mut until| frame < until);
        let ids: Vec<i32> = self.jaegers.members.iter().copied().collect();
        for id in ids {
            let Some(unit) = self.fighters.get(&id).map(|f| f.unit) else {
                self.jaegers.members.remove(&id);
                continue;
            };
            if unit.health() <= 0.0 || unit.team() != team {
                self.jaeger_remove_unit(unit);
                continue;
            }
            if self.retreat_handler.is_retreating(id) {
                self.jaeger_leave_squad(id);
                continue;
            }
            if self.fighters.get(&id).is_some_and(|f| f.squad.is_some())
                || self.jaegers.pounces.contains_key(&id)
                || self.jaegers.held.contains_key(&id)
            {
                continue;
            }
            let Some(kind) = kind_of(self.defs.name_of(unit)) else { continue };
            let sid = match self.jaegers.forming.get(kind.name) {
                Some(&sid) if self.squads.contains(sid) => sid,
                _ => {
                    let sid = self.squads.create(SquadKind::Jaeger, team);
                    self.jaegers.forming.insert(kind.name, sid);
                    self.jaegers.hunts.insert(sid, Hunt { kind, phase: Phase::Forming, ambush: None });
                    sid
                }
            };
            let mut nav = nav!(self);
            if let Some(s) = self.squads.get_mut(sid) {
                s.add_unit_nav(id, &mut self.fighters, &mut nav);
            }
        }
        let empty: Vec<SquadId> = (self.jaegers.hunts.keys().copied())
            .filter(|&sid| self.squads.get(sid).is_none_or(|s| s.is_dead(&self.fighters)))
            .collect();
        for sid in empty {
            self.jaegers.hunts.remove(&sid);
            self.jaegers.forming.retain(|_, &mut f| f != sid);
            self.squads.remove(sid);
        }
    }

    /// Positions of our army: squads under way and the big loners.
    fn jaeger_army(&self) -> Vec<Vec3> {
        let squads = (self.squad_handler.squads.iter().chain(&self.squad_handler.taskforces))
            .filter_map(|&sid| self.squads.get(sid))
            .filter(|s| s.status != 'f' && !s.is_dead(&self.fighters))
            .map(|s| s.get_pos(&self.fighters));
        let loners = (self.misc_handler.loners.iter().chain(&self.misc_handler.striders))
            .filter_map(|id| self.fighters.get(id))
            .map(|f| f.get_pos());
        squads.chain(loners).collect()
    }

    /// Where a squad at `pos` with no heavy to go for waits: behind the part
    /// of our army nearest it (`army`, see [`Ai::jaeger_army`]), toward the
    /// nearest haven, or at the haven. (Not the army's rally point: that is
    /// what the army goes for, often an enemy raider, and a squad sent there
    /// tracked it into the enemy's escorts.)
    fn jaeger_wait_point(&self, pos: Vec3, army: &[Vec3]) -> Vec3 {
        let haven = self.graph_get_closest_haven(pos);
        let near = army.iter().copied().min_by(|a, b| distance(*a, pos).total_cmp(&distance(*b, pos)));
        match near {
            None => haven,
            Some(a) if haven == NULLPOS => a,
            Some(a) if distance(a, haven) <= WAIT_BEHIND => haven,
            Some(a) => get_directional_point(a, haven, WAIT_BEHIND),
        }
    }

    /// How dear heavy `id` is to a squad of `n` of `kind` at `from` (the
    /// nearer and the more valuable, the less), if it takes it, gets to it,
    /// and may strike there.
    fn jaeger_cost(&self, kind: Kind, n: usize, from: Vec3, id: i32, army: &[Vec3]) -> Option<f32> {
        let war = &self.war;
        let e = war.targets.get(&id)?;
        // An Ultimatum also takes what the original sent it after: anything
        // big on the ground, commanders too.
        let value = cost(e, self.metal);
        let prey = is_heavy(e, self.metal)
            || (kind.alpha.is_none()
                && e.identified
                && !e.is_static
                && !e.can_fly()
                && value >= HEAVY_COST
                && e.def_health() >= 1000.0);
        if e.is_dead || !prey || value < kind.min_value || e.position == NULLPOS || self.frame - e.last_seen > STALE {
            return None;
        }
        // A stun holds on max health; damage kills what is left.
        let health = if kind.stuns { e.def_health() } else { e.get_health() * e.def_health() };
        if needed(kind, health).is_none_or(|k| k > n) {
            return None;
        }
        let p = e.position;
        if !kind.suicide {
            // Hunters that lie in wait take a heavy that may come our way
            // (where to wait for it decides), Widows one that our army can
            // finish where it is.
            let supported = if kind.ambush > 0.0 && !self.dev_off("ambush") {
                (kind.stuns && self.jaeger_finishes(p, e.def_health())) || !self.graph.is_enemy_territory(p)
            } else {
                war.get_total_friendly_power(p) > 0.0
                    || self.graph.is_ally_territory(p)
                    || army.iter().any(|&a| distance(a, p) <= SUPPORT_RANGE)
            };
            if self.jaeger_defenses_too_strong(kind, p, e.def_health()) || !supported {
                return None;
            }
        }
        let def = self.defs.get(kind.name)?;
        // (A Grizzly under water is out of reach but of water weapons; see
        // `water`.)
        if !war.can_hit(war.reach(Some(def)), e) || !reaches(self.reach_terrain(), def, from, p, kind.range) {
            return None;
        }
        Some(distance(from, p) / (1.0 + value / 1000.0))
    }

    /// The heavy (enemy id) a squad of `n` of `kind` at `from` should go
    /// for, but those in `taken`.
    fn jaeger_target(&self, kind: Kind, n: usize, from: Vec3, taken: &[i32], army: &[Vec3]) -> Option<i32> {
        (self.war.targets.keys().copied())
            .filter(|id| !taken.contains(id))
            .filter_map(|id| Some((id, self.jaeger_cost(kind, n, from, id, army)?)))
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(id, _)| id)
    }

    fn jaeger_assign(&mut self, sid: SquadId, frame: i32) {
        let Some(hunt) = self.jaegers.hunts.get(&sid) else { return };
        let (kind, phase) = (hunt.kind, hunt.phase);
        let Some(s) = self.squads.get(sid) else { return };
        let pos = s.get_pos(&self.fighters);
        let members: Vec<i32> = s.fighters.clone();
        let reloading =
            members.iter().filter(|id| self.fighters.get(id).is_some_and(|f| f.is_reloading(frame))).count();
        let ready = members.len() - reloading;
        let taken: Vec<i32> = (self.jaegers.hunts.iter())
            .filter(|&(&o, _)| o != sid)
            .filter_map(|(_, h)| match h.phase {
                Phase::Hunting(Some(t)) | Phase::Striking(t, _) => Some(t),
                _ => None,
            })
            .collect();
        let army = self.jaeger_army();

        let next = match phase {
            Phase::Forming => {
                let full = members.len() >= SQUAD_MAX
                    || self.jaeger_needed(kind).is_some_and(|n| members.len() >= n)
                    || self.jaeger_target(kind, members.len(), pos, &taken, &army).is_some();
                if full {
                    self.jaegers.forming.retain(|_, &mut f| f != sid);
                    self.log(&format!("jaeger: a squad of {} {} hunts", members.len(), kind.name));
                    Phase::Hunting(None)
                } else {
                    let haven = self.graph_get_closest_haven(pos);
                    self.jaeger_go(sid, haven, false);
                    Phase::Forming
                }
            }
            Phase::Hunting(target) => {
                let keep = target.filter(|&t| self.jaeger_cost(kind, ready, pos, t, &army).is_some());
                let target = keep.or_else(|| self.jaeger_target(kind, ready, pos, &taken, &army));
                let ambushing = kind.ambush > 0.0 && !self.dev_off("ambush");
                match target.and_then(|t| Some((t, self.jaeger_heavy(t)?))) {
                    Some((t, (at, unit, value))) => {
                        let health = self.war.targets.get(&t).map_or(0.0, |e| e.def_health());
                        // A stun with nothing about to finish the heavy is wasted.
                        let finished = !ambushing || !kind.stuns || self.jaeger_finishes(at, health);
                        // Supported Widows may join a fight on either side
                        // of the lines; other hunters wait beyond them.
                        let chase = !ambushing || (finished && (kind.stuns || !self.graph.is_enemy_territory(at)));
                        let ambush = if ambushing && !chase {
                            self.jaeger_ambush(sid, kind, t, pos, frame)
                        } else {
                            if let Some(h) = self.jaegers.hunts.get_mut(&sid) {
                                h.ambush = None;
                            }
                            None
                        };
                        let d = distance(pos, at);
                        if finished && ((chase && d <= kind.range + STRIKE_RANGE) || d <= kind.range + AMBUSH_STRIKE) {
                            self.jaeger_strike(sid, unit, true);
                            self.log(&format!(
                                "jaeger: {ready} {} strike a heavy ({value:.0} metal) at ({:.0}, {:.0}){}",
                                kind.name,
                                at.x,
                                at.z,
                                if ambush.is_some() { " from an ambush" } else { "" }
                            ));
                            Phase::Striking(t, frame)
                        } else if chase {
                            if phase != Phase::Hunting(Some(t)) {
                                self.log(&format!(
                                    "jaeger: {} {} go after a heavy at ({:.0}, {:.0})",
                                    ready, kind.name, at.x, at.z
                                ));
                            }
                            self.jaeger_go(sid, at, false);
                            Phase::Hunting(Some(t))
                        } else if let Some(a) = ambush {
                            self.jaeger_go(sid, a.at, false);
                            Phase::Hunting(Some(t))
                        } else {
                            // Not after it into the enemy's land: by our
                            // army, until it comes our way.
                            let wait = self.jaeger_wait_point(pos, &army);
                            self.jaeger_go(sid, wait, false);
                            Phase::Hunting(Some(t))
                        }
                    }
                    None => {
                        // Wait by our army, out of the way.
                        let wait = self.jaeger_wait_point(pos, &army);
                        self.jaeger_go(sid, wait, false);
                        Phase::Hunting(None)
                    }
                }
            }
            Phase::Striking(t, since) => {
                match self.jaeger_heavy(t) {
                    None => Phase::Hunting(None),
                    Some((at, unit, value)) => {
                        if unit.is_paralyzed() {
                            // The army's to kill.
                            self.war.refresh_defense_target(DefenseTarget::new(at, value, frame));
                        }
                        if kind.stuns && reloading > 0 && reloading == members.len() {
                            if unit.is_paralyzed() {
                                self.log(&format!("jaeger: a heavy stunned at ({:.0}, {:.0})", at.x, at.z));
                            }
                            let haven = self.graph_get_closest_haven(pos);
                            self.jaeger_go(sid, haven, true);
                            Phase::Reloading
                        } else if frame - since > STRIKE_TIMEOUT
                            || (!kind.suicide
                                && self.jaeger_defenses_too_strong(
                                    kind,
                                    at,
                                    self.war.targets.get(&t).map_or(0.0, |e| e.def_health()),
                                ))
                        {
                            Phase::Hunting(None)
                        } else {
                            self.jaeger_strike(sid, unit, false);
                            Phase::Striking(t, since)
                        }
                    }
                }
            }
            Phase::Reloading => {
                if reloading == 0 {
                    Phase::Hunting(None)
                } else {
                    let haven = self.graph_get_closest_haven(pos);
                    self.jaeger_go(sid, haven, true);
                    Phase::Reloading
                }
            }
        };
        self.jaegers.set_phase(sid, next, &mut self.squads);
    }

    /// Where a heavy (enemy id) is, its unit and its metal, while it lives.
    fn jaeger_heavy(&self, id: i32) -> Option<(Vec3, Unit, f32)> {
        let e = self.war.targets.get(&id).filter(|e| !e.is_dead)?;
        Some((e.position, e.unit, cost(e, self.metal)))
    }

    /// Send a squad toward `to`, cloaked and by the ways clear of the
    /// enemy; `away` after a strike, straight off.
    fn jaeger_go(&mut self, sid: SquadId, to: Vec3, away: bool) {
        if to == NULLPOS {
            return;
        }
        let mut nav = nav!(self);
        let Some(s) = self.squads.get_mut(sid) else { return };
        if away {
            s.retreat_to(to, &mut self.fighters, &mut nav);
        } else if distance(s.get_pos(&self.fighters), to) > 100.0 {
            s.set_target(to, &mut self.fighters, &mut nav);
        }
    }

    /// A squad attacks `target`: all of it, or (not `all`) those left idle
    /// that are not reloading.
    fn jaeger_strike(&mut self, sid: SquadId, target: Unit, all: bool) {
        let Some(s) = self.squads.get(sid) else { return };
        let frame = self.frame;
        let suicide = self.jaegers.hunts.get(&sid).is_some_and(|h| h.kind.suicide);
        let strikers: Vec<(Unit, bool)> = (s.fighters.iter().filter_map(|id| self.fighters.get(id)))
            .map(|f| (f.unit, all || (f.unit.num_current_commands() == 0 && !f.is_reloading(frame))))
            .collect();
        for (unit, free) in strikers {
            if suicide {
                // Skuttles jump onto it, all of them each time; one that
                // cannot waits in reach rather than walk up (see `jumps`).
                if unit.rules_param_float("is_jumping", 0.0) != 1.0
                    && (self.jaeger_jump(unit, target, NULLPOS, "skuttle") || self.skuttle_wait(unit, target))
                {
                    self.jaegers.pounces.insert(unit.id(), (target.id(), frame));
                }
            } else if free {
                unit.attack(target, 0);
            }
        }
    }

    /// Skuttles pounce: each that is not under way onto a heavy already
    /// jumps (or, shown and that close, walks) onto the best heavy worth one in reach
    /// that the blasts under way do not kill, and not among more of our
    /// own: its squad's mark first, then the dearest, then the nearest. It
    /// leaves its squad for the pounce.
    fn jaeger_pounce(&mut self, frame: i32) {
        let targets = &self.war.targets;
        (self.jaegers.pounces)
            .retain(|_, p| frame - p.1 <= POUNCE_HOLD && targets.get(&p.0).is_some_and(|e| !e.is_dead));
        // One landed with nothing to do (its attack dropped) goes on at its
        // heavy: it goes off within 50 of it.
        let landed: Vec<(Unit, Unit)> = (self.jaegers.pounces.iter())
            .filter_map(|(id, p)| Some((self.fighters.get(id)?.unit, self.war.targets.get(&p.0)?.unit)))
            .filter(|(u, _)| u.rules_param_float("is_jumping", 0.0) != 1.0 && u.num_current_commands() == 0)
            .collect();
        for (unit, target) in landed {
            unit.attack(target, 0);
        }
        if self.dev_off("pounce") {
            return;
        }
        let Some(kind) = kind_of("jumpbomb") else { return };
        let Some(def) = self.defs.get(kind.name) else { return };
        let alpha = kind.alpha.unwrap_or(0.0);
        let skuttles: Vec<(i32, Unit, Option<SquadId>)> = (self.jaegers.members.iter())
            .filter(|id| !self.jaegers.pounces.contains_key(id))
            .filter_map(|id| self.fighters.get(id))
            .filter(|f| self.defs.name_of(f.unit) == kind.name && f.unit.health() > 0.0 && !f.unit.is_being_built())
            .map(|f| (f.id, f.unit, f.squad))
            .collect();
        for (id, unit, squad) in skuttles {
            // (Its jump may still be in the air: a pounce's hold ran out.)
            if unit.rules_param_float("is_jumping", 0.0) == 1.0 {
                continue;
            }
            let mark = squad.and_then(|sid| self.jaegers.hunts.get(&sid)).and_then(|h| match h.phase {
                Phase::Hunting(Some(t)) | Phase::Striking(t, _) => Some(t),
                _ => None,
            });
            let jump = self.jump_ready(unit);
            let reach = jump.map_or(SKUTTLE_PRESS, |jd| jd.range);
            // (A heavy coming on may be in reach where it will be.)
            let lead = if jump.is_some() && !self.dev_off("lead") { POUNCE_LEAD } else { 0.0 };
            let pos = unit.pos();
            let war = &self.war;
            let pounces = &self.jaegers.pounces;
            let best = (war.targets.iter())
                .filter(|(_, e)| !e.is_dead && e.position != NULLPOS && frame - e.last_seen <= POUNCE_SEEN)
                .filter(|(_, e)| is_heavy(e, self.metal) && cost(e, self.metal) >= kind.min_value)
                .filter(|(_, e)| distance(pos, e.position) <= reach + lead && war.can_hit(war.reach(Some(def)), e))
                .filter(|&(&eid, e)| {
                    let under_way = pounces.values().filter(|p| p.0 == eid).count() as f32 * alpha;
                    e.get_health() * e.def_health() > under_way
                })
                .filter(|(_, e)| self.jaeger_friends_near(e.position) <= cost(e, self.metal))
                .map(|(&eid, e)| (eid, e, mark == Some(eid), cost(e, self.metal), distance(pos, e.position)))
                .max_by(|a, b| a.2.cmp(&b.2).then(a.3.total_cmp(&b.3)).then(b.4.total_cmp(&a.4)))
                .map(|(eid, e, marked, value, d)| {
                    (eid, e.unit, e.position, e.get_health() * e.def_health(), marked, value, d)
                });
            let Some((eid, target, at, health, marked, value, d)) = best else { continue };
            // Without a jump, only one that presses on goes (never walking
            // up: see `jumps`).
            if jump.is_none() && !Self::skuttle_presses_on(unit, d) {
                continue;
            }
            // Out of its squad first: a leader leaving it is stopped, which
            // would drop the attack queued after the jump (it flies anyway,
            // and lands idle by the heavy).
            self.jaeger_leave_squad(id);
            if !(jump.is_some() && self.jaeger_jump(unit, target, at, "skuttle pounce")) {
                if !Self::skuttle_presses_on(unit, d) {
                    // (Back into a squad at the next muster.)
                    continue;
                }
                unit.attack(target, 0);
            }
            self.log(&format!(
                "jaeger: a Skuttle pounces on a heavy ({value:.0} metal, {health:.0} health{}) at ({:.0}, {:.0}){}",
                if marked { "" } else { ", not its mark" },
                at.x,
                at.z,
                if unit.is_cloaked() { "" } else { ", uncloaked" }
            ));
            self.jaegers.pounces.insert(id, (eid, frame));
        }
    }

    /// A Skuttle's jump onto `target`: leading it (feature `lead`) or not.
    fn jaeger_jump(&mut self, unit: Unit, target: Unit, known: Vec3, why: &str) -> bool {
        if self.dev_off("lead") {
            self.jump_strike(unit, target, known, why)
        } else {
            self.jump_lead(unit, target, known, why)
        }
    }

    /// Whether our army can kill a stunned heavy of `health` at `p` before
    /// it wakes (see [`FINISH_SHARE`]), with support to spare for defenses.
    fn jaeger_finishes(&self, p: Vec3, health: f32) -> bool {
        self.war.get_total_friendly_power(p) >= widow_support_required(health, self.war.get_porc_threat(p))
    }

    /// Widows need enough support for the heavy and the defenses covering it.
    /// Other surviving hunters retain their static-defense veto.
    fn jaeger_defenses_too_strong(&self, kind: Kind, p: Vec3, health: f32) -> bool {
        self.war.get_porc_threat(p) > 0.0 && (!kind.stuns || !self.jaeger_finishes(p, health))
    }

    /// The way heavy `id` goes over the ground in the next [`WAY_HORIZON`]
    /// seconds, from where it is, with its seconds to each point: down a
    /// travel-time field from it to the asset of ours it heads for (by the
    /// QRF mark's rule: soon reached and ahead of it as it goes). `None`
    /// without the QRF's ground grid or an asset of ours in its horizon.
    fn jaeger_way(&self, id: i32) -> Option<Vec<(Vec3, f32)>> {
        let e = self.war.targets.get(&id).filter(|e| !e.is_dead && e.position != NULLPOS)?;
        let def = e.ud?;
        let mobility = Mobility::of(def)?;
        let grid = self.qrf.grid.as_ref()?;
        let field = grid.field_within(mobility, def.speed().max(10.0), &[(e.position, 0.0)], WAY_HORIZON);
        let v = (self.qrf.tracks.get(&id))
            .filter(|t| self.frame - t.last_seen <= POUNCE_SEEN)
            .map_or(Vec3::default(), |t| t.vel);
        let speed = v.x.hypot(v.z);
        let p = e.position;
        let goal = (self.qrf.assets.iter())
            .filter_map(|a| {
                let t = field.at(a.pos);
                if !t.is_finite() {
                    return None;
                }
                let (dx, dz) = (a.pos.x - p.x, a.pos.z - p.z);
                let ahead = if speed > 10.0 { (dx * v.x + dz * v.z) / (dx.hypot(dz).max(1.0) * speed) } else { 0.6 };
                Some((a.pos, t * (1.6 - ahead)))
            })
            .min_by(|a, b| a.1.total_cmp(&b.1))?
            .0;
        let mut cells = field.descent(goal, 400);
        cells.reverse();
        Some(cells.into_iter().map(|c| (grid.centre(c), field.cell(c))).collect())
    }

    /// Where a squad lies in wait for heavy `t`, foreseen anew now and then
    /// (see [`Ai::jaeger_ambush_spot`]).
    fn jaeger_ambush(&mut self, sid: SquadId, kind: Kind, t: i32, from: Vec3, frame: i32) -> Option<Ambush> {
        let old = self.jaegers.hunts.get(&sid).and_then(|h| h.ambush).filter(|a| a.target == t);
        if let Some(a) = old.filter(|a| frame - a.frame < AMBUSH_PERIOD) {
            return Some(a);
        }
        let new = self.jaeger_ambush_spot(kind, from, t, old, frame);
        if let Some(a) = new
            && old.is_none_or(|o| distance(o.at, a.at) > 200.0)
        {
            self.jaegers.ambushes += 1;
            let n = self.squads.get(sid).map_or(0, |s| s.fighters.len());
            self.log(&format!(
                "jaeger: {n} {} lie in wait at ({:.0}, {:.0}) for a heavy {:.0} s off",
                kind.name, a.at.x, a.at.z, a.eta
            ));
        }
        if let Some(h) = self.jaegers.hunts.get_mut(&sid) {
            h.ambush = new;
        }
        new
    }

    /// A place for a squad of `kind` at `from` to lie in wait for heavy `t`:
    /// beside the first point of the heavy's way (see [`Ai::jaeger_way`])
    /// that the squad gets to, settled, before it, on the squad's side if
    /// it can; out of the enemy's land and out of sight of the enemy now,
    /// avoiding defenses unless Widows have support for them and the heavy. The
    /// `old` place is kept while the heavy still comes by it.
    fn jaeger_ambush_spot(&self, kind: Kind, from: Vec3, t: i32, old: Option<Ambush>, frame: i32) -> Option<Ambush> {
        let def = self.defs.get(kind.name)?;
        let mobility = Mobility::of(def)?;
        let speed = def.speed().max(10.0);
        let health = self.war.targets.get(&t)?.def_health();
        let grid = self.qrf.grid.as_ref()?;
        let way = self.jaeger_way(t)?;
        let finishes = |p: Vec3| !kind.stuns || self.jaeger_finishes(p, health);
        if let Some(o) = old
            && let Some(&(_, eta)) = way.iter().find(|w| distance(w.0, o.way) <= 2.0 * CELL)
            && finishes(o.way)
        {
            return Some(Ambush { eta, frame, ..o });
        }
        let map = self.cb.map();
        for (i, &(x, eta)) in way.iter().enumerate() {
            let (prev, next) = (way[i.saturating_sub(1)].0, way[(i + 1).min(way.len() - 1)].0);
            let (dx, dz) = (next.x - prev.x, next.z - prev.z);
            let n = dx.hypot(dz);
            if n < 1.0 || !finishes(x) {
                continue;
            }
            let (px, pz) = (-dz / n, dx / n);
            let side = if (from.x - x.x) * px + (from.z - x.z) * pz >= 0.0 { 1.0 } else { -1.0 };
            for off in [side, -side, side * 0.5] {
                let (ax, az) = (x.x + px * kind.ambush * off, x.z + pz * kind.ambush * off);
                let at = Vec3::new(ax, map.elevation_at(ax, az), az);
                if !grid.passable(at, mobility)
                    || distance(from, at) * AMBUSH_DETOUR / speed + AMBUSH_SETTLE > eta
                    || self.graph.is_enemy_territory(at)
                    || self.jaeger_defenses_too_strong(kind, at, health)
                    || self.war.get_scythe_threat(at) > 0.0
                    || !reaches(self.reach_terrain(), def, from, at, 100.0)
                {
                    continue;
                }
                return Some(Ambush { target: t, at, way: x, eta, frame });
            }
        }
        None
    }

    /// Cloaked hunters keep out of sight of what moves (see the module
    /// doc): each that an enemy would decloak within [`DODGE_AHEAD`] frames
    /// where it stands goes to the spot nearby that stays clearest of them
    /// all (nearer its goal on a tie), on foot or, a Skuttle that cannot
    /// get clear so, by a jump. Hunters striking or pouncing do not.
    fn jaeger_dodge(&mut self, frame: i32) {
        if self.dev_off("dodge") {
            return;
        }
        let ids: Vec<i32> =
            self.jaegers.members.iter().copied().filter(|id| !self.jaegers.pounces.contains_key(id)).collect();
        let map = self.cb.map();
        let (w, h) = (map.width() as f32 * 8.0, map.height() as f32 * 8.0);
        for id in ids {
            let Some(f) = self.fighters.get(&id) else { continue };
            let (unit, goal, squad) = (f.unit, f.target, f.squad);
            let Some(kind) = kind_of(self.defs.name_of(unit)) else { continue };
            let hunt = squad.and_then(|sid| self.jaegers.hunts.get(&sid));
            let striking = hunt.is_some_and(|h| matches!(h.phase, Phase::Striking(..)));
            // A Widow waits for its heavy within its range, closer than the
            // heavy sees it (a Skuttle does not).
            let mark = hunt.filter(|_| kind.stuns).and_then(|h| match h.phase {
                Phase::Hunting(t) => t,
                _ => None,
            });
            if kind.decloak <= 0.0
                || striking
                || unit.is_being_built()
                || unit.health() <= 0.0
                || unit.rules_param_float("is_jumping", 0.0) == 1.0
            {
                continue;
            }
            let Some(def) = unit.def() else { continue };
            let pos = unit.pos();
            let r = kind.decloak + DODGE_MARGIN;
            // Enemies about, where they are and their velocity (elmos a
            // frame): buildings decloak too.
            let threats: Vec<(Vec3, Vec3)> = (self.war.targets.values())
                .filter(|e| !e.is_dead && e.position != NULLPOS && distance(e.position, pos) <= r + DODGE_LOOK)
                .filter(|e| mark != Some(e.unit_id))
                .filter(|e| e.is_static || frame - e.last_seen <= POUNCE_SEEN)
                .map(|e| (e.position, if e.is_static { Vec3::default() } else { e.unit.vel() }))
                .collect();
            if threats.is_empty() {
                continue;
            }
            // How near the enemies come in the next frames to a hunter
            // walking from here to `to` at `speed` (elmos a frame): from the
            // next step on, which a move still changes.
            let clear = |to: Vec3, speed: f32| -> f32 {
                let (dx, dz) = (to.x - pos.x, to.z - pos.z);
                let len = dx.hypot(dz).max(0.001);
                let mut worst = f32::INFINITY;
                for k in 1..=DODGE_AHEAD / 10 {
                    let t = (k * 10) as f32;
                    let s = (speed * t).min(len);
                    let (x, z) = (pos.x + dx / len * s, pos.z + dz / len * s);
                    for &(q, v) in &threats {
                        worst = worst.min((x - (q.x + v.x * t)).hypot(z - (q.z + v.z * t)));
                    }
                }
                worst
            };
            let here = clear(pos, 0.0);
            if here >= r {
                continue;
            }
            self.jaegers.threatened += 1;
            let jump = self.jump_ready(unit);
            let grid = self.qrf.grid.as_ref();
            let mobility = Mobility::of(def);
            let fits = |p: Vec3| {
                p.x > 16.0
                    && p.z > 16.0
                    && p.x < w - 16.0
                    && p.z < h - 16.0
                    && grid.zip(mobility).is_none_or(|(g, m)| g.passable(p, m))
            };
            let pull = if goal == NULLPOS { pos } else { goal };
            let around = |d: f32| {
                (0..16).map(move |k| {
                    let a = k as f32 * std::f32::consts::TAU / 16.0;
                    Vec3::xz(pos.x + d * a.cos(), pos.z + d * a.sin())
                })
            };
            let better = |a: &(Vec3, f32), b: &(Vec3, f32)| {
                (a.1.min(r) - 0.05 * distance(a.0, pull)).total_cmp(&(b.1.min(r) - 0.05 * distance(b.0, pull)))
            };
            let speed = def.speed() / 30.0;
            let walk = (around(speed * DODGE_STEP)).filter(|&p| fits(p)).map(|p| (p, clear(p, speed))).max_by(better);
            let walked = walk.map_or(here, |w| w.1);
            if walked < r
                && let Some(jd) = jump
            {
                let leap = (DODGE_JUMPS.iter().flat_map(|&d| around(d).map(move |p| (p, d))))
                    .filter(|&(p, _)| fits(p))
                    .map(|(p, d)| {
                        let t = Self::jump_time(jd, d);
                        let near = threats.iter().map(|(q, v)| (p.x - (q.x + v.x * t)).hypot(p.z - (q.z + v.z * t)));
                        (p, near.fold(f32::INFINITY, f32::min))
                    })
                    .max_by(better);
                if let Some((to, c)) = leap
                    && c > walked
                    && self.jump_to(unit, to, "skuttle dodge")
                {
                    self.jaegers.dodge_jumps += 1;
                    self.log(&format!(
                        "jaeger: a {} jumps clear of the enemy at ({:.0}, {:.0}), {:.0} from it then",
                        kind.name, pos.x, pos.z, c
                    ));
                    continue;
                }
            }
            if let Some((to, c)) = walk
                && c > here
            {
                unit.move_to(Vec3::new(to.x, map.elevation_at(to.x, to.z), to.z), 0);
                self.jaegers.dodges += 1;
            }
        }
    }

    /// Hunters showing themselves (a diagnostic): each going from cloaked to
    /// seen is logged with what may have shown it: energy storage (cloak
    /// upkeep is paid from it), a hit (damage decloaks), the nearest enemy
    /// (anything within the decloak distance shows it), the gadget's
    /// `cannotcloak`, and what it was doing.
    fn jaeger_watch_cloak(&mut self, frame: i32) {
        let energy = self.cb.economy().current(self.energy);
        let ids: Vec<i32> = self.jaegers.members.iter().copied().collect();
        for id in ids {
            let Some(f) = self.fighters.get(&id) else { continue };
            let (unit, squad) = (f.unit, f.squad);
            let Some(kind) = kind_of(self.defs.name_of(unit)) else { continue };
            if kind.decloak <= 0.0 || unit.is_being_built() || unit.health() <= 0.0 {
                continue;
            }
            let (cloaked, health) = (unit.is_cloaked(), unit.health());
            let old = self.jaegers.cloak.get(&id).copied();
            let since = match old {
                Some((c, since, _)) if c == cloaked => since,
                _ => frame,
            };
            self.jaegers.cloak.insert(id, (cloaked, since, health));
            let Some((true, _, was)) = old else { continue };
            if cloaked {
                continue;
            }
            let pos = unit.pos();
            let near = (self.war.targets.values())
                .filter(|e| !e.is_dead && e.position != NULLPOS)
                .map(|e| (distance(e.position, pos), e))
                .min_by(|a, b| a.0.total_cmp(&b.0));
            let v = unit.vel();
            let doing = if self.jaegers.pounces.contains_key(&id) {
                "pouncing"
            } else {
                match squad.and_then(|sid| self.jaegers.hunts.get(&sid)) {
                    Some(h) => match h.phase {
                        Phase::Forming => "forming",
                        Phase::Hunting(_) if h.ambush.is_some() => "in wait",
                        Phase::Hunting(Some(_)) => "hunting",
                        Phase::Hunting(None) => "by the army",
                        Phase::Striking(..) => "striking",
                        Phase::Reloading => "reloading",
                    },
                    None => "alone",
                }
            };
            self.log(&format!(
                "jaeger: a {} shows itself at ({:.0}, {:.0}), {} and {}: energy {energy:.0}, hit {:.0}, nearest enemy {} at {:.0}, cannotcloak {}",
                kind.name,
                pos.x,
                pos.z,
                doing,
                if v.x.hypot(v.z) > 0.1 { "moving" } else { "standing" },
                (was - health).max(0.0),
                near.map_or("none", |n| n.1.def_name.as_str()),
                near.map_or(f32::INFINITY, |n| n.0),
                unit.rules_param_float("cannotcloak", 0.0)
            ));
        }
    }

    /// Metal of our and allied mobile units (Skuttles but) a blast at `p`
    /// would take.
    fn jaeger_friends_near(&self, p: Vec3) -> f32 {
        (self.cb.friendly_units_in(p, POUNCE_FRIENDS).iter())
            .filter(|u| self.defs.name_of(**u) != "jumpbomb")
            .filter_map(|u| u.def().filter(|d| d.speed() > 0.0).map(|d| d.cost(self.metal)))
            .sum()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recruited_hunter_cannot_leave_an_order_for_the_next_scout() {
        let mut j = Jaegers::default();
        j.ordered = vec![("spiderantiheavy", 0), ("jumpbomb", 0)];
        assert!(j.order_filled("spiderantiheavy", 30));
        assert!(!j.order_filled("spiderantiheavy", 60));
        assert_eq!(j.ordered, [("jumpbomb", 0)]);
        assert!(!j.order_filled("jumpbomb", ORDER_TIMEOUT + 1));
        assert!(j.ordered.is_empty());
    }

    #[test]
    fn recruiting_last_hunter_retires_its_squad_before_reusing_fighter() {
        use crate::military::wrappers::squad::fake_engine::{TEAM, callback, spawn};
        let cb = callback();
        let mut fighters = FighterStore::new();
        let mut squads = Squads::new();
        let mut j = Jaegers::default();
        let sid = squads.create(SquadKind::Jaeger, TEAM);
        let kind = kind_of("spiderantiheavy").unwrap();
        j.forming.insert(kind.name, sid);
        j.hunts.insert(sid, Hunt { kind, phase: Phase::Forming, ambush: None });
        for id in [10, 11] {
            fighters.insert(id, Fighter::new(spawn(cb, id, 5), 280.0, TEAM));
            squads.get_mut(sid).unwrap().add_unit(id, &mut fighters, 0);
        }
        squads.get_mut(sid).unwrap().remove_unit(10, &mut fighters, 0);
        j.forget_empty(sid, &mut squads, &fighters);
        assert!(squads.contains(sid));
        squads.get_mut(sid).unwrap().remove_unit(11, &mut fighters, 0);
        j.forget_empty(sid, &mut squads, &fighters);
        let other = squads.create(SquadKind::Assault, TEAM);
        squads.get_mut(other).unwrap().add_unit(11, &mut fighters, 0);
        assert!(!squads.contains(sid));
        assert!(j.hunts.is_empty() && j.forming.is_empty());
        assert_eq!(fighters[&11].squad, Some(other));
    }

    #[test]
    fn displayed_status_tracks_hunt_phase() {
        let mut j = Jaegers::default();
        let mut squads = Squads::new();
        let sid = squads.create(SquadKind::Jaeger, 0);
        for (phase, status) in [
            (Phase::Forming, 'f'),
            (Phase::Hunting(None), 'r'),
            (Phase::Hunting(Some(42)), 'a'),
            (Phase::Striking(42, 100), 'a'),
            (Phase::Reloading, 'r'),
        ] {
            j.set_phase(sid, phase, &mut squads);
            assert_eq!(squads.get(sid).unwrap().status, status);
        }
    }

    #[test]
    fn static_defenses_raise_widow_support_in_the_same_units() {
        // Cyclops: 300 support alone. One Lotus paints 86 raw power.
        assert_eq!(widow_support_required(12000.0, 0.0), 300.0);
        assert!((widow_support_required(12000.0, 0.172) - 386.0).abs() < 0.001);
        assert!(402.0 >= widow_support_required(12000.0, 0.172));
        assert!(402.0 < widow_support_required(12000.0, 3.0 * 0.172));
        assert!(644.0 >= widow_support_required(12000.0, 3.0 * 0.172));
    }

    #[test]
    fn squads_take_what_their_strikes_pass() {
        let widow = kind_of("spiderantiheavy").unwrap();
        let ulti = kind_of("striderantiheavy").unwrap();
        // Grizzly, Cyclops, Paladin, Detriment.
        assert_eq!(needed(widow, 8700.0), Some(2));
        assert_eq!(needed(widow, 12000.0), Some(2));
        assert_eq!(needed(widow, 32000.0), Some(4));
        assert_eq!(needed(widow, 76000.0), None);
        assert_eq!(needed(ulti, 76000.0), Some(1));
    }
}
