use super::*;
use crate::economy::factory::FacKind;
use crate::kgbutil::AirField;
use crate::military::Bomber;
use crate::util::{OPTION_SHIFT_KEY, distance};
use crate::{Ai, defs::cmd};
use springai::{NO_TIMEOUT, Unit, Vec3};
use std::collections::{BTreeMap, BTreeSet};

mod carpet;
mod defense;
mod pool;
mod release;
mod retarget;
mod speculation;
mod wave;
use defense::FireSource;

const PERIOD: i32 = 30;
/// Ordinary sorties out at once, and how many of them may be secondary.
const MAX_SORTIES: usize = 10;
const MAX_SECONDARY_SORTIES: usize = 4;
/// Our own losses a drop may cost, as a share of what it removes.
const COLLATERAL_SHARE: f32 = 0.5;
const SORTIE_TIMEOUT: i32 = 30 * 150;
/// Raptors wearing down an aircraft without counted anti-air stay on it longer:
/// strafing passes keep each firing about 14% of the time (two did 28 damage
/// a second to a hovering Revenant in the staged fixture, and the 150-second
/// limit called them home with it at 639 of 4200 HP, to regenerate).
const WEAR_TIMEOUT: i32 = 30 * 300;
fn sortie_timeout(job: &Mission) -> i32 {
    if job.kind == MissionKind::Intercept && job.wearable { WEAR_TIMEOUT } else { SORTIE_TIMEOUT }
}
const COOLDOWN: i32 = 30 * 15;

#[derive(Default)]
struct Utilization {
    state_frames: BTreeMap<&'static str, u64>,
    launched: usize,
    payload_spent: usize,
    aborted: BTreeMap<&'static str, usize>,
}

/// Zero-K's `noammo` rules param (`unit_bomber_command`): 0 armed, 1 flying
/// to a pad for ammo, 2 rearming on a pad, 3 armed again and sitting on the
/// pad to be repaired (any new order lifts it off, armed).
const PAD_REPAIRING: f32 = 3.0;
fn needs_ammo(noammo: f32) -> bool {
    noammo == 1.0 || noammo == 2.0
}
/// A rearmed aircraft still on the pad may leave for a primary mission at
/// this much health (`padlaunch`), when the mission's own survival check at
/// its current health passes too. An airpad repairs at 2.5 buildpower: a
/// Likho takes ~120 s to mend 30%, an Odin back from 43% sat 250 s (with its
/// flight home and 30 s rearm). Staged (zkgbai-runs/padrepair/smoke5):
/// Raptors wounded to 35% sat 40 s on the pad alone, 4-11 s with this and
/// a pad caretaker.
pub(super) const PAD_LAUNCH_HEALTH: f32 = 0.7;

/// Home is the ally center (the middle of the metal spots we own): every
/// plan's route, risk and length are reckoned from it. Within this radius
/// an aircraft always counts as home; with `airhome` so does anywhere over
/// our territory, since a sortie checks each plane's own first leg anyway
/// (`airstrike_dispatch`). The radius alone flapped idle planes between
/// ready and returning: circling drifts them past it, and the center moves
/// whenever a metal spot changes hands.
const HOME_RADIUS: f32 = 450.0;
/// The planes of one package start at most this much farther than each
/// other from the route's first waypoint, so they still arrive together
/// (`airhome`): the old home circle's diameter.
const PACK_SPREAD: f32 = 2.0 * HOME_RADIUS;

/// An aircraft free for a sortie (see `Ai::airstrike_candidates`).
#[derive(Clone, Copy, Debug)]
struct Candidate {
    id: i32,
    kind: Plane,
    /// From where it is to the route's first waypoint.
    reach: f32,
    /// Rearmed on a pad and still repairing (`padlaunch`).
    on_pad: bool,
    health: f32,
}

/// Members for `package` whose reaches differ by at most `spread`, so they
/// arrive together: of the windows of reach that fill it, the nearest (the
/// earliest there), taking idle planes before those on a pad, then the
/// nearest. A window is a stretch of distance to the route, not of ground:
/// planes abreast of the route a base apart still arrive together.
fn pick(package: &[(Plane, usize)], candidates: &[Candidate], spread: f32) -> Option<Vec<i32>> {
    let mut starts: Vec<f32> = candidates.iter().map(|c| c.reach).collect();
    starts.sort_by(f32::total_cmp);
    starts.dedup();
    starts.into_iter().find_map(|from| {
        let mut members = Vec::new();
        for &(kind, n) in package {
            let mut fit: Vec<&Candidate> = (candidates.iter())
                .filter(|c| c.kind == kind && c.reach >= from && c.reach <= from + spread && !members.contains(&c.id))
                .collect();
            if fit.len() < n {
                return None;
            }
            fit.sort_by(|a, b| a.on_pad.cmp(&b.on_pad).then(a.reach.total_cmp(&b.reach)).then(a.id.cmp(&b.id)));
            members.extend(fit[..n].iter().map(|c| c.id));
        }
        (!members.is_empty()).then_some(members)
    })
}

fn readiness(reserved: bool, active: bool, empty: bool, healthy: bool, at_home: bool) -> &'static str {
    if reserved {
        "operator"
    } else if active {
        "in_progress"
    } else if empty {
        "rearming"
    } else if !healthy {
        "repairing"
    } else if !at_home {
        "returning"
    } else {
        "ready"
    }
}

struct Aircraft {
    unit: Unit,
    kind: Plane,
    returning: bool,
    last_order: i32,
    escort: Option<i32>,
}
#[derive(Clone)]
struct Plan {
    job: Mission,
    packages: Vec<Vec<(Plane, usize)>>,
    risk: Risk,
    length: f32,
    route: Vec<Vec3>,
    return_route: Vec<Vec3>,
}
impl Plan {
    fn score(&self) -> f32 {
        // Preserve primary-before-secondary dispatch, but favor value reachable
        // in a shorter flight within each tier. Interceptions remain urgent.
        if self.job.kind == MissionKind::Intercept {
            self.job.score()
        } else {
            self.job.score() / (1.0 + self.length / 1200.0)
        }
    }
}

#[derive(Clone, Copy)]
struct WaveFlight {
    id: i32,
    target: Vec3,
}

/// Preserve the engine's queued pursuit through brief LOS/radar gaps.
#[derive(Default)]
struct TargetMemory {
    last_contact: i32,
    destroyed: bool,
}
impl TargetMemory {
    fn valid(&mut self, frame: i32, observed: bool, static_known: bool) -> bool {
        if observed {
            self.last_contact = frame;
        }
        !self.destroyed && (static_known || frame - self.last_contact <= 30 * 20)
    }
}

struct Sortie {
    memory: TargetMemory,
    searched_since: Option<i32>,
    retargeted: bool,
    release: release::State,
    wave: Option<WaveFlight>,
    job: Mission,
    members: Vec<i32>,
    launched: i32,
    last_order: i32,
    last_route: i32,
    route: Vec<Vec3>,
    return_route: Vec<Vec3>,
    package: Vec<(Plane, usize)>,
}

/// A unit def's shield (radius, full charge) and armor.
type DefShield = (Option<(f32, f32)>, f32);

#[derive(Default)]
pub struct BomberHandler {
    units: BTreeMap<i32, Aircraft>,
    recent: BTreeMap<(MissionKind, i32), Plan>,
    plans: Vec<Plan>,
    sorties: Vec<Sortie>,
    cooldown: BTreeMap<i32, i32>,
    /// Orders not yet represented by a nanoframe; one per factory.
    orders: BTreeMap<i32, (Plane, i32)>,
    building: BTreeMap<i32, Plane>,
    pub grounded: bool,
    last_report: i32,
    last_wave_check: i32,
    wave_checks: usize,
    waves_launched: usize,
    last_wave_launch: i32,
    launched: usize,
    raven_damage: Option<f32>,
    /// Per unit def: its shield's radius and full charge, and its armor.
    def_shields: std::cell::RefCell<BTreeMap<i32, DefShield>>,
    /// Launches and retargets held back for what they would do to our own.
    collateral_holds: std::cell::Cell<usize>,
    /// Packages not launched because their planes started too far apart
    /// (`PACK_SPREAD`).
    spread_holds: usize,
    /// Each aircraft's last readiness, for the transition log of the
    /// `airstrike-` fixtures.
    test_states: BTreeMap<i32, &'static str>,
    defenses: Vec<FireSource>,
    utilization: BTreeMap<Plane, Utilization>,
    speculation: speculation::State,
}
impl BomberHandler {
    pub fn new() -> Self {
        Self::default()
    }
    fn refresh_priority_memory(&mut self, contacts: &[Contact], frame: i32) {
        // Fresh observations replace remembered priority demand, including when
        // healing demotes a target. Unseen targets retain the normal memory.
        self.recent.retain(|_, p| {
            !contacts.iter().any(|e| {
                e.id == p.job.target
                    && e.live
                    && e.submerged
                    && !p.packages.iter().any(|pkg| pkg.iter().all(|(k, _)| can_hit_submerged(*k, p.job.kind, true)))
            })
        });
        for e in contacts.iter().filter(|e| e.known && !e.static_ && e.live && frame - e.seen <= FRESH) {
            self.recent.remove(&(MissionKind::Heavy, e.id));
        }
    }

    pub fn get_bomber_size(&self) -> usize {
        self.units.values().filter(|a| !matches!(a.kind, Plane::Sparrow | Plane::Raptor)).count()
    }
}

impl Ai {
    pub(crate) fn airstrike_manages(&self, id: i32) -> bool {
        self.bomber_handler.units.contains_key(&id)
    }

    pub(crate) fn airstrike_claims(&self, id: i32) -> bool {
        self.bomber_handler.building.contains_key(&id) || self.bomber_handler.units.contains_key(&id)
    }

    /// Whether a sortie is out against this enemy.
    pub(crate) fn airstrike_engaged(&self, enemy: i32) -> bool {
        self.bomber_handler.sorties.iter().any(|s| s.job.targets.contains(&enemy))
    }

    /// Alternatives ordered by the metal they put in the air, cheapest
    /// first (stable: equal ones keep their order). Launch takes the first
    /// it can fill, so a Phoenix no longer goes at a Flea a Magpie is ready
    /// for.
    fn airstrike_cheapest_first(&self, choices: &mut [Vec<(Plane, usize)>]) {
        let metal = |p: &Vec<(Plane, usize)>| {
            (p.iter())
                .map(|(k, n)| *n as f32 * self.defs.get(k.name()).map_or(f32::INFINITY, |d| d.cost(self.metal)))
                .sum::<f32>()
        };
        choices.sort_by(|a, b| metal(a).total_cmp(&metal(b)));
    }

    pub fn bomber_add_bomber(&mut self, b: Bomber) {
        let Some(kind) = Plane::of(self.defs.name_of(b.unit)) else { return };
        self.bomber_handler.building.remove(&b.id);
        self.bomber_handler.units.entry(b.id).or_insert(Aircraft {
            unit: b.unit,
            kind,
            returning: true,
            last_order: -1000,
            escort: None,
        });
    }

    pub fn bomber_remove_unit(&mut self, u: Unit) {
        self.bomber_handler.units.remove(&u.id());
        self.bomber_handler.building.remove(&u.id());
        self.bomber_handler.orders.remove(&u.id());
    }

    pub(crate) fn bomber_target_destroyed(&mut self, enemy: i32) {
        for sortie in &mut self.bomber_handler.sorties {
            if sortie.job.target == enemy {
                sortie.memory.destroyed = true;
            }
        }
        self.bomber_remove_target(enemy);
    }

    pub fn bomber_remove_target(&mut self, enemy: i32) {
        self.bomber_handler.recent.retain(|_, p| !p.job.targets.contains(&enemy));
    }

    pub(crate) fn airstrike_created(&mut self, u: Unit, builder: Option<Unit>) {
        if let Some((kind, _)) = builder.and_then(|b| self.bomber_handler.orders.remove(&b.id()))
            && Plane::of(self.defs.name_of(u)) == Some(kind)
        {
            self.bomber_handler.building.insert(u.id(), kind);
        }
    }

    pub(crate) fn airstrike_ordered(&mut self, factory: i32, name: &str) {
        if let Some(p) = Plane::of(name) {
            self.bomber_handler.orders.insert(factory, (p, self.frame));
        }
    }

    /// A deficit for a recent primary mission. Existing, repairing, committed,
    /// unfinished and ordered aircraft all count. Secondary jobs never buy units.
    pub(crate) fn airstrike_production(&self) -> Option<&'static str> {
        let h = &self.bomber_handler;
        if h.grounded || self.dev_off("airstrikes") {
            return None;
        }
        let mut have = BTreeMap::<Plane, usize>::new();
        for a in h.units.values() {
            *have.entry(a.kind).or_default() += 1;
        }
        for &p in h.building.values() {
            *have.entry(p).or_default() += 1;
        }
        for &(p, _) in h.orders.values().filter(|o| self.frame - o.1 < 30 * 30) {
            *have.entry(p).or_default() += 1;
        }
        let mut plans: Vec<_> = h.recent.values().filter(|p| self.frame - p.job.seen <= MEMORY).collect();
        plans.sort_by(|a, b| b.score().total_cmp(&a.score()));
        for plan in &plans {
            if let Some(kind) = purchase(&plan.packages, &have, |k| {
                self.defs.get(k.name()).map_or(f32::INFINITY, |d| d.cost(self.metal))
            }) {
                return Some(kind.name());
            }
        }
        // An optional jammer is a fleet extra, behind all combat-aircraft
        // demand. Its absence never blocks an Odin mission.
        escort::want_spare(plans.iter().any(|p| p.job.optional_escort()), &have).then_some(Plane::Sparrow.name())
    }

    fn airstrike_contacts(&self) -> Vec<Contact> {
        let ground = self.eco_height_map();
        self.war
            .targets
            .values()
            // `is_nano_spam` is refreshed only when a unit enters LOS, so a
            // strider frame watched from 10% to done would stay "spam": read
            // frames in sight live.
            .filter(|e| {
                !e.is_dead
                    && !if e.visible {
                        e.unit.is_being_built() && e.unit.build_progress() < 0.25
                    } else {
                        e.is_nano_spam
                    }
            })
            .map(|e| {
                let live = e.visible || e.is_radar_visible;
                let v = if live { e.unit.vel() * 30.0 } else { Vec3::ZERO };
                // An unidentified radar return well above terrain can still be
                // intercepted; never use a hidden unit definition to identify it.
                let air = e.can_fly()
                    || (!e.identified
                        && live
                        && e.position.y > ground.height_at(e.position.x, e.position.z).max(0.0) + 100.0);
                // `disable=airshields` plans as if no enemy had a shield.
                let (shield, armor) = e.ud.map_or((None, 1.0), |d| self.airstrike_def_shield(d));
                let shield = shield.filter(|_| !self.dev_off("airshields")).map(|(radius, full)| {
                    // The charge of a bubble in sight; a full one otherwise.
                    let now = if e.visible { e.unit.shield_charge() } else { -1.0 };
                    (radius, if now >= 0.0 { now.min(full) } else { full })
                });
                Contact {
                    id: e.unit_id,
                    pos: e.position,
                    vel: v,
                    value: e.value,
                    hp: e.get_health().max(0.0) * e.def_health(),
                    max_hp: e.def_health(),
                    seen: e.last_seen,
                    known: e.identified,
                    live,
                    air,
                    heavy_air: e.identified
                        && anti_air::heavy_air_target(&e.def_name)
                        && !(e.def_name == anti_air::REVENANT && self.dev_off("likhorevenant")),
                    submerged: e.is_submerged(),
                    mex: e.is_mex,
                    static_: e.is_static,
                    heavy: e.is_com || e.is_arty || e.is_major_cancer || (!e.is_static && e.value >= 600.0),
                    raider: e.is_raider
                        || self.unit_classes.small_raiders.contains(e.def_name.as_str())
                        || self.unit_classes.medium_raiders.contains(e.def_name.as_str()),
                    wind: e.def_name == "energywind",
                    singu: e.def_name == "energysingu",
                    // `disable=odinfactories` keeps factories out of thermite jobs.
                    factory: e.is_static
                        && (e.def_name.starts_with("factory") || e.def_name == "striderhub")
                        && !self.dev_off("odinfactories"),
                    frame: e.visible && e.unit.is_being_built(),
                    progress: if e.visible { e.unit.build_progress() } else { 1.0 },
                    home: self.graph.is_ally_territory(e.position),
                    fighting: !e.is_static
                        && e.can_attack()
                        && self.war.get_friendly_threat(e.position) > 0.5
                        && self.war.get_threat(e.position) > 0.3,
                    shield,
                    armor,
                    armed: e.can_attack(),
                    speed: e.def_speed(),
                    aa: e.is_aa || e.is_flex_aa || FIGHTERS.contains(&e.def_name.as_str()),
                    // Marked once a planning pass (`airstrike_mark_menacing`).
                    menacing: false,
                }
            })
            .collect()
    }

    /// Armed enemy aircraft in sight or on radar whose weapons reach our or
    /// allied ground units or buildings (its range and 200 elmos more, in a
    /// vertical cylinder: it flies above them) are `menacing`.
    /// `disable=airmenace` leaves the old rule: an interception only in our
    /// territory or inbound to home.
    fn airstrike_mark_menacing(&self, contacts: &mut [Contact]) {
        if self.dev_off("airmenace") {
            return;
        }
        for c in contacts.iter_mut().filter(|c| c.air && c.live && c.known && c.armed) {
            let Some(e) = self.war.targets.get(&c.id) else { continue };
            c.menacing = self
                .cb
                .friendly_units_in_cylinder(c.pos, e.threat_radius.max(250.0) + 200.0)
                .iter()
                .any(|u| u.def().is_some_and(|d| !d.is_able_to_fly()));
        }
    }

    /// Whether an aircraft at `pos` is home: see `HOME_RADIUS`.
    fn airstrike_home(&self, pos: Vec3, home: Vec3) -> bool {
        distance(pos, home) < HOME_RADIUS || (!self.dev_off("airhome") && self.graph.is_ally_territory(pos))
    }

    /// Aircraft of these kinds free for a new sortie: idle at home, loaded
    /// and healthy, and with `pad_floor` also those rearmed on a pad and
    /// repaired to at least that share of their health.
    fn airstrike_candidates(
        &self,
        kinds: &[Plane],
        busy: &BTreeSet<i32>,
        home: Vec3,
        anchor: Vec3,
        pad_floor: Option<f32>,
    ) -> Vec<Candidate> {
        (self.bomber_handler.units.iter())
            .filter(|(id, a)| {
                kinds.contains(&a.kind) && !busy.contains(id) && !springai::reserved::is_reserved(self.cb, **id)
            })
            .filter_map(|(&id, a)| {
                let health = a.unit.health() / a.unit.max_health().max(1.0);
                let noammo = a.unit.rules_param_float("noammo", 0.0);
                let idle = !a.returning && noammo == 0.0 && health >= 0.95 && self.airstrike_home(a.unit.pos(), home);
                let on_pad = noammo == PAD_REPAIRING && pad_floor.is_some_and(|floor| health >= floor);
                let reach = distance(a.unit.pos(), anchor);
                (idle || on_pad).then_some(Candidate { id, kind: a.kind, reach, on_pad: !idle, health })
            })
            .collect()
    }

    /// Up to `n` of the candidates of this kind: those idle first, nearest to
    /// `anchor` (the route's first waypoint) with `airhome`, else the
    /// healthiest; then those on a pad, the healthiest first.
    fn airstrike_ready(
        &self,
        kind: Plane,
        busy: &BTreeSet<i32>,
        home: Vec3,
        anchor: Vec3,
        n: usize,
        pad_floor: Option<f32>,
    ) -> Vec<i32> {
        let nearest = !self.dev_off("airhome");
        let mut ready = self.airstrike_candidates(&[kind], busy, home, anchor, pad_floor);
        let order = |c: &Candidate| if nearest && !c.on_pad { c.reach } else { -c.health };
        ready.sort_by(|a, b| a.on_pad.cmp(&b.on_pad).then(order(a).total_cmp(&order(b))).then(a.id.cmp(&b.id)));
        ready.into_iter().take(n).map(|c| c.id).collect()
    }

    /// A threat-aware way home for `u` from where it flies now.
    fn airstrike_home_route(&self, u: Unit, kind: Plane, home: Vec3, interceptors: &[(Vec3, f32, f32)]) -> Vec<Vec3> {
        let v = u.vel();
        let len = (v.x * v.x + v.z * v.z).sqrt();
        let heading = (len > 1e-3).then(|| (v.x / len, v.z / len));
        self.airstrike_field(u.pos(), interceptors).route_for(home, self.airstrike_turn_radius(kind), heading)
    }

    /// Metal in combat aircraft (Sparrows aside): what airpad caretakers
    /// are for (`padnano`).
    pub(crate) fn airstrike_fleet_metal(&self) -> f32 {
        (self.bomber_handler.units.values())
            .filter(|a| a.kind != Plane::Sparrow)
            .filter_map(|a| a.unit.def())
            .map(|d| d.cost(self.metal))
            .sum()
    }

    /// Whether a package's implosion bombs and napalm spare our own and
    /// allied ground units and buildings enough: the damage they would take
    /// (priced like enemy victims, see `group::collateral`) must stay under
    /// half of what the strike removes. `disable=collateral` restores the old
    /// rule: no Likho or Phoenix drop with any of ours within 220 elmos. Carpets
    /// weigh their own strips (see `carpet`).
    fn airstrike_spares_friends(&self, job: &Mission, package: &[(Plane, usize)], route: &[Vec3]) -> bool {
        if job.kind == MissionKind::Carpet || !package.iter().any(|(k, _)| matches!(k, Plane::Phoenix | Plane::Likho)) {
            return true;
        }
        let ground = |u: &Unit| u.def().is_some_and(|d| !d.is_able_to_fly());
        if self.dev_off("collateral") {
            return !self.cb.friendly_units_in(job.pos, 220.0).iter().any(ground);
        }
        let arms = self.airstrike_arms();
        let bystanders: Vec<_> = self
            .cb
            .friendly_units_in(job.pos, group::collateral_reach(arms))
            .into_iter()
            .filter(ground)
            .filter_map(|u| {
                let d = u.def()?;
                let building = d.speed() <= 0.0;
                let mex = self.defs.name_of(u) == "staticmex";
                Some(group::Bystander {
                    pos: u.pos(),
                    hp: u.health(),
                    worth: d.cost(self.metal) * if mex { 4.0 } else { 1.0 },
                    // Our Solars close when hit too; units are taken unarmored.
                    armor: if building { self.airstrike_def_shield(d).1 } else { 1.0 },
                    building,
                })
            })
            .collect();
        if bystanders.is_empty() {
            return true;
        }
        // The line runs along the final leg: from the last waypoint short of
        // the aim (or home).
        let from = route
            .iter()
            .rev()
            .find(|p| distance(**p, job.pos) > 100.0)
            .copied()
            .unwrap_or_else(|| self.graph.get_ally_center());
        let cost = group::collateral(arms, job.pos, from, package, &bystanders);
        let gain = if job.group.is_some() || job.kind == MissionKind::HeavyAir {
            job.damage_value(package)
        } else {
            job.value
        };
        let spares = cost <= COLLATERAL_SHARE * gain;
        if !spares {
            self.bomber_handler.collateral_holds.set(self.bomber_handler.collateral_holds.get() + 1);
        }
        spares
    }

    /// Mission kinds switched off: `disable=outposts` (spare Phoenixes on
    /// clumps of buildings), `disable=odinturrets` (spare Odins on heavy
    /// defenses), `disable=magpiestrafe` (spare Magpies on whatever cannot
    /// shoot back).
    fn airstrike_kind_off(&self, kind: MissionKind) -> bool {
        match kind {
            MissionKind::Outpost => self.dev_off("outposts"),
            MissionKind::Turret => self.dev_off("odinturrets"),
            MissionKind::Strafe => self.dev_off("magpiestrafe"),
            _ => false,
        }
    }

    /// Where an Odin's shield cluster covers our troops fighting at `fight`:
    /// on the mobile ground fighter nearest to it, within 600 elmos. None
    /// once they have left or died, so a drop never lands on empty ground.
    fn airstrike_shield_point(&self, fight: Vec3) -> Option<Vec3> {
        self.cb
            .friendly_units_in(fight, 600.0)
            .into_iter()
            .filter(|u| u.def().is_some_and(|d| !d.is_able_to_fly() && d.speed() > 0.0 && d.is_able_to_attack()))
            .map(|u| u.pos())
            .min_by(|a, b| distance(*a, fight).total_cmp(&distance(*b, fight)))
    }

    // A HeavyAir mission deliberately engages its quarry, and the heavy
    // flock around it: Nimbus fly in clumps, and each would otherwise veto
    // every other one's strike (see `anti_air::flock_mate`). Escorting
    // fighters and other AA still gate the route.
    fn airstrike_interceptors_except(&self, quarry: Option<i32>) -> Vec<(Vec3, f32, f32)> {
        let flock =
            quarry.filter(|_| !self.dev_off("heavyflock")).and_then(|id| self.war.targets.get(&id)).map(|q| q.position);
        self.war
            .targets
            .values()
            .filter(|e| {
                !e.is_dead
                    && Some(e.unit_id) != quarry
                    && !flock.is_some_and(|at| e.identified && super::anti_air::flock_mate(at, e.position, &e.def_name))
                    && e.can_fly()
                    && (e.is_aa || e.is_flex_aa || FIGHTERS.contains(&e.def_name.as_str()))
                    && self.frame - e.last_seen < 30 * 20
            })
            .map(|e| (e.position, e.threat_radius.max(600.0) + e.def_speed() * 5.0, e.get_danger().max(250.0) / 500.0))
            .collect()
    }

    fn airstrike_field(&self, from: Vec3, interceptors: &[(Vec3, f32, f32)]) -> AirField {
        let map = self.cb.map();
        AirField::with_interceptors(&self.war, map.width() / 8, map.height() / 8, from, |p| {
            interceptors.iter().filter(|(at, r, _)| distance(*at, p) < *r).map(|(_, _, t)| t).sum()
        })
    }

    fn airstrike_path_risk(&self, from: Vec3, route: &[Vec3], interceptors: &[(Vec3, f32, f32)]) -> Risk {
        defense::forecast(from, route, &self.bomber_handler.defenses, interceptors)
    }

    fn airstrike_risk(&self, air: &AirField, from: Vec3, pos: Vec3, interceptors: &[(Vec3, f32, f32)]) -> Risk {
        let route = air.route(pos);
        round_trip(
            self.airstrike_path_risk(from, &route, interceptors),
            self.airstrike_path_risk(pos, &return_route(from, &route), interceptors),
        )
    }

    /// A def's shield (radius, full charge) and armor, read once.
    fn airstrike_def_shield(&self, d: springai::UnitDef) -> DefShield {
        *self.bomber_handler.def_shields.borrow_mut().entry(d.id()).or_insert_with(|| {
            let shield =
                d.shield_def().map(|w| (w.shield_radius(), w.shield_power())).filter(|s| s.0 > 0.0 && s.1 > 0.0);
            let armor = d.armored_multiple();
            (shield, if armor > 0.0 && armor <= 1.0 { armor } else { 1.0 })
        })
    }

    fn airstrike_arms(&self) -> Arms {
        Arms {
            likho: self.airstrike_blast(),
            napalm: self.airstrike_napalm(),
            plain_worth: self.dev_off("shieldworth"),
        }
    }

    /// The installed Phoenix salvo, for buildings under its line.
    fn airstrike_napalm(&self) -> group::Napalm {
        self.defs
            .get(Plane::Phoenix.name())
            .into_iter()
            .flat_map(|d| d.weapon_mounts())
            .filter_map(|m| m.weapon_def())
            .filter(|w| !w.name().to_lowercase().contains("fake"))
            .filter_map(|w| {
                w.damage_types().first().copied().map(|damage| group::Napalm {
                    bombs: (w.salvo_size().max(1) * w.projectiles_per_shot().max(1)) as usize,
                    damage,
                    radius: w.area_of_effect(),
                    edge: w.edge_effectiveness(),
                })
            })
            .find(|n| n.damage > 0.0 && n.radius > 0.0)
            .unwrap_or_default()
    }

    fn airstrike_blast(&self) -> Blast {
        self.defs
            .get(Plane::Likho.name())
            .into_iter()
            .flat_map(|d| d.weapon_mounts())
            .filter_map(|m| m.weapon_def())
            .filter(|w| !w.name().contains("fake"))
            .filter_map(|w| {
                w.damage_types().first().copied().map(|damage| Blast {
                    damage,
                    radius: w.area_of_effect(),
                    edge: w.edge_effectiveness(),
                })
            })
            .filter(|b| b.damage > 0.0 && b.radius > 0.0)
            .max_by(|a, b| a.damage.total_cmp(&b.damage))
            .unwrap_or_default()
    }

    /// The first loaded bomber of a sortie: where its flight is planned from.
    fn airstrike_lead(&self, sortie: &Sortie) -> Option<Vec3> {
        self.airstrike_lead_unit(sortie).map(|u| u.pos())
    }

    fn airstrike_lead_unit(&self, sortie: &Sortie) -> Option<Unit> {
        sortie
            .members
            .iter()
            .filter_map(|id| self.bomber_handler.units.get(id))
            .find(|a| a.kind != Plane::Sparrow && a.unit.rules_param_float("noammo", 0.0) == 0.0)
            .map(|a| a.unit)
    }

    /// The circle an airframe really turns on (see `turns`), which decides
    /// whether its routes are pulled straight (`AirField::route_for`); zero
    /// with `disable=odinroutes`.
    fn airstrike_turn_radius(&self, plane: Plane) -> f32 {
        if self.dev_off("odinroutes") {
            return 0.0;
        }
        self.defs.get(plane.name()).map_or(0.0, |d| {
            super::turns::turn_radius(
                d.max_rudder(),
                d.max_elevator(),
                d.max_bank(),
                d.front_to_speed(),
                d.speed_to_front(),
            )
        })
    }

    /// The widest turn among aircraft that fly a route (escorts guard theirs).
    fn airstrike_route_radius(&self, planes: impl IntoIterator<Item = Plane>) -> f32 {
        planes.into_iter().filter(|p| *p != Plane::Sparrow).map(|p| self.airstrike_turn_radius(p)).fold(0.0, f32::max)
    }

    pub fn bomber_update(&mut self, frame: i32) {
        if frame % PERIOD != self.offset % PERIOD {
            return;
        }
        let has_factory = self.fac.factories.values().any(|f| f.kind() == FacKind::Plane);
        if !has_factory && self.bomber_handler.units.is_empty() {
            return;
        }
        let home = self.graph.get_ally_center();
        let team = self.team_id;
        self.bomber_handler.units.retain(|_, a| a.unit.health() > 0.0 && a.unit.team() == team);
        let alive: BTreeSet<_> = self.cb.team_units().into_iter().map(|u| u.id()).collect();
        self.bomber_handler.building.retain(|id, _| alive.contains(id));
        self.bomber_handler.orders.retain(|id, (_, f)| alive.contains(id) && frame - *f < 30 * 30);
        self.bomber_handler.cooldown.retain(|_, f| frame - *f < COOLDOWN);
        if self.bomber_handler.raven_damage.is_none() {
            let damage = self
                .defs
                .get(Plane::Raven.name())
                .into_iter()
                .flat_map(|d| d.weapon_mounts())
                .filter_map(|m| m.weapon_def())
                .filter(|w| !w.name().contains("fake") && !w.name().contains("torpedo"))
                .filter_map(|w| w.damage_types().first().copied())
                .fold(0.0, f32::max);
            self.bomber_handler.raven_damage = Some(if damage > 0.0 { damage } else { 800.0 });
        }
        self.bomber_handler.defenses = self.airstrike_defenses();
        let interceptors = self.airstrike_interceptors_except(None);
        // Defensive interception accepts the target fighter itself; ground AA
        // still gates the mission and its route.
        let air = self.airstrike_field(home, &interceptors);
        let defense_air = self.airstrike_field(home, &[]);
        let mut contacts = self.airstrike_contacts();
        self.airstrike_mark_menacing(&mut contacts);
        self.bomber_handler.refresh_priority_memory(&contacts, frame);
        let arms = self.airstrike_arms();
        let mut jobs = missions(&contacts, home, frame);
        jobs.retain(|j| !self.airstrike_kind_off(j.kind));
        // Raiders the firefighters are already on are theirs (they in turn
        // leave alone what a sortie is on, see `firefighters`).
        jobs.retain(|j| j.kind != MissionKind::Interdict || !self.firefighters_engaged(j.target));
        let theirs: Vec<_> = (self.bomber_handler.recent.keys())
            .filter(|k| k.0 == MissionKind::Interdict && self.firefighters_engaged(k.1))
            .copied()
            .collect();
        for key in theirs {
            self.bomber_handler.recent.remove(&key);
        }
        jobs.extend(self.airstrike_speculative_jobs());
        jobs.extend(self.airstrike_carpet_jobs());
        let had_jobs = !jobs.is_empty();
        let mut plans = Vec::new();
        for mut job in jobs {
            job.aim_group(arms);
            if matches!(job.kind, MissionKind::Thermite | MissionKind::Turret)
                && air.length(job.pos).unwrap_or(f32::INFINITY) >= 3000.0
            {
                job.deep = true;
            }
            if job.kind == MissionKind::Shield {
                let Some(drop) = self.airstrike_shield_point(job.pos) else { continue };
                job.pos = drop;
            }
            let quarry_enemies =
                (job.kind == MissionKind::HeavyAir).then(|| self.airstrike_interceptors_except(Some(job.target)));
            let enemies = if job.kind == MissionKind::Intercept {
                &[][..]
            } else {
                quarry_enemies.as_deref().unwrap_or(&interceptors)
            };
            let quarry_field = quarry_enemies.as_ref().map(|_| self.airstrike_field(home, enemies));
            let field =
                if job.kind == MissionKind::Intercept { &defense_air } else { quarry_field.as_ref().unwrap_or(&air) };
            let risk = self.airstrike_risk(field, home, job.pos, enemies);
            let mut choices: Vec<_> = packages(&job, risk, self.bomber_handler.raven_damage.unwrap_or(800.0))
                .into_iter()
                .filter(|p| {
                    p.iter()
                        .all(|(k, _)| self.defs.get(k.name()).is_some_and(|d| k.survives(risk, d.health(), d.speed())))
                })
                .collect();
            if job.kind == MissionKind::Interdict {
                self.airstrike_cheapest_first(&mut choices);
            }
            // Seeing a still-existing job become unsafe invalidates its memory
            // immediately; a mere loss of radar contact may retain procurement.
            self.bomber_handler.recent.remove(&job.key());
            if choices.is_empty() {
                continue;
            }
            let length = field.length(job.pos).unwrap_or(f32::INFINITY);
            // An Odin's package flies fewer, gentler legs (see `route_for`).
            let radius = self.airstrike_route_radius(choices.iter().flatten().map(|(k, _)| *k));
            let route = field.route_for(job.pos, radius, None);
            let return_route = return_route(home, &route);
            let plan = Plan { job, packages: choices, risk, length, route, return_route };
            // Inferences use existing aircraft; only confirmed targets buy a fleet.
            if plan.job.kind.primary() && plan.job.inference.is_none() {
                self.bomber_handler.recent.insert(plan.job.key(), plan.clone());
            }
            plans.push(plan);
        }
        let known: BTreeSet<_> = self.war.targets.keys().copied().collect();
        self.bomber_handler.recent.retain(|_, p| frame - p.job.seen <= MEMORY && known.contains(&p.job.target));
        // Recheck remembered routes too: an AA sighting elsewhere can close a
        // route without refreshing the target's radar contact.
        let unsafe_keys: Vec<_> = self
            .bomber_handler
            .recent
            .iter()
            .filter(|(_, p)| {
                let defensive = p.job.kind == MissionKind::Intercept;
                let quarry_enemies = (p.job.kind == MissionKind::HeavyAir)
                    .then(|| self.airstrike_interceptors_except(Some(p.job.target)));
                let enemies = if defensive { &[][..] } else { quarry_enemies.as_deref().unwrap_or(&interceptors) };
                let quarry_field = quarry_enemies.as_ref().map(|_| self.airstrike_field(home, enemies));
                let r = self.airstrike_risk(
                    if defensive { &defense_air } else { quarry_field.as_ref().unwrap_or(&air) },
                    home,
                    p.job.pos,
                    enemies,
                );
                let allowed = packages(&p.job, r, self.bomber_handler.raven_damage.unwrap());
                !p.packages.iter().any(|pkg| {
                    allowed.contains(pkg)
                        && pkg
                            .iter()
                            .all(|(k, _)| self.defs.get(k.name()).is_some_and(|d| k.survives(r, d.health(), d.speed())))
                })
            })
            .map(|(k, _)| *k)
            .collect();
        for key in unsafe_keys {
            self.bomber_handler.recent.remove(&key);
        }
        self.bomber_handler.grounded = self.dev_off("airstrikes")
            || (had_jobs && plans.is_empty() && self.bomber_handler.recent.is_empty())
            || self.war.get_aa_threat(home) > 1.25;
        plans.sort_by(|a, b| {
            b.job
                .kind
                .primary()
                .cmp(&a.job.kind.primary())
                .then_with(|| b.score().total_cmp(&a.score()))
                .then_with(|| a.job.key().cmp(&b.job.key()))
        });
        self.bomber_handler.plans = plans;
        self.airstrike_pool(home, &air, &interceptors);
        self.airstrike_fly(frame, home, &air, &defense_air, &interceptors);
        self.airstrike_wave(frame, home);
        self.airstrike_launch(frame, home);
        let busy: BTreeSet<_> = self.bomber_handler.sorties.iter().flat_map(|s| s.members.iter().copied()).collect();
        for (&id, a) in &self.bomber_handler.units {
            let state = readiness(
                springai::reserved::is_reserved(self.cb, id),
                busy.contains(&id),
                needs_ammo(a.unit.rules_param_float("noammo", 0.0)),
                a.unit.health() >= a.unit.max_health() * 0.95,
                !a.returning && self.airstrike_home(a.unit.pos(), home),
            );
            *self.bomber_handler.utilization.entry(a.kind).or_default().state_frames.entry(state).or_default() +=
                PERIOD as u64;
            if self.test.as_deref().is_some_and(|t| t.starts_with("airstrike-"))
                && self.bomber_handler.test_states.insert(id, state) != Some(state)
            {
                let p = a.unit.pos();
                self.log(&format!(
                    "airstrike-test: state {id} {:?} {state} at {:.0},{:.0} home_d={:.0} territory={}",
                    a.kind,
                    p.x,
                    p.z,
                    distance(p, home),
                    self.graph.is_ally_territory(p)
                ));
            }
        }
        if frame - self.bomber_handler.last_report >= 30 * 30 {
            self.bomber_handler.last_report = frame;
            self.log(&format!(
                "airstrikes: {} jobs, {} recent primary, {} planes, {} sorties; grounded={} next={:?}; launched={} spread_holds={}",
                self.bomber_handler.plans.len(),
                self.bomber_handler.recent.len(),
                self.bomber_handler.units.len(),
                self.bomber_handler.sorties.len(),
                self.bomber_handler.grounded,
                self.airstrike_production(),
                self.bomber_handler.launched,
                self.bomber_handler.spread_holds
            ));
        }
    }

    fn airstrike_fly(
        &mut self,
        frame: i32,
        home: Vec3,
        air: &AirField,
        defense_air: &AirField,
        interceptors: &[(Vec3, f32, f32)],
    ) {
        let mut occupied: BTreeSet<_> =
            self.bomber_handler.sorties.iter().flat_map(|s| s.job.targets.iter().copied()).collect();
        let sorties = std::mem::take(&mut self.bomber_handler.sorties);
        let turns = !self.dev_off("airturns");
        let mut active = BTreeSet::new();
        let aircraft: BTreeMap<_, _> = self.bomber_handler.units.iter().map(|(&id, a)| (id, a.unit)).collect();
        for mut sortie in sorties {
            let acquired = self.airstrike_acquire(&mut sortie, &occupied);
            self.airstrike_commitments(&mut sortie, frame);
            let retargeted = self.airstrike_retarget(&mut sortie, home, air, interceptors, &occupied);
            if retargeted {
                self.airstrike_commitments(&mut sortie, frame);
            }
            let acquired = acquired || retargeted;
            occupied.extend(sortie.job.targets.iter().copied());
            let hunting = sortie.job.kind == MissionKind::Speculative;
            let carpet = sortie.job.kind == MissionKind::Carpet;
            let refreshed = carpet && {
                let lead = self.airstrike_lead(&sortie).unwrap_or(home);
                self.carpet_refresh(&mut sortie, lead)
            };
            let target = self.war.targets.get(&sortie.job.target);
            let observed = target.is_some_and(|e| !e.is_dead && (e.visible || e.is_radar_visible));
            let remembered = sortie.memory.valid(frame, observed, target.is_some_and(|e| e.is_static && !e.is_dead));
            let valid = if hunting {
                speculation::searching(frame, sortie.launched, sortie.searched_since)
            } else if carpet {
                frame - sortie.launched <= carpet::WINDOW
            } else {
                remembered
            };
            // A shield drop follows the troops it is for: our fighter nearest
            // the enemy (or the last drop point while it is out of sight). It
            // is called off once none are left there.
            let shield_drop = (sortie.job.kind == MissionKind::Shield).then(|| {
                self.airstrike_shield_point(target.filter(|_| observed).map_or(sortie.job.pos, |e| e.position))
            });
            let troops_gone = matches!(shield_drop, Some(None));
            // A strider frame that got finished walks out of the burn.
            let hatched = sortie.job.kind == MissionKind::Thermite
                && target.is_some_and(|e| !e.is_static && e.visible && !e.unit.is_being_built());
            let pos = match shield_drop {
                Some(drop) => drop.unwrap_or(sortie.job.pos),
                None => target.filter(|_| observed).map_or(sortie.job.pos, |e| e.position),
            };
            let quarry_enemies = (sortie.job.kind == MissionKind::HeavyAir)
                .then(|| self.airstrike_interceptors_except(Some(sortie.job.target)));
            let enemies = if sortie.job.kind == MissionKind::Intercept {
                &[][..]
            } else {
                quarry_enemies.as_deref().unwrap_or(interceptors)
            };
            let quarry_field = quarry_enemies.as_ref().map(|_| self.airstrike_field(home, enemies));
            let field = if sortie.job.kind == MissionKind::Intercept {
                defense_air
            } else {
                quarry_field.as_ref().unwrap_or(air)
            };
            let lead = self.airstrike_lead_unit(&sortie);
            let from = lead.map_or(home, |u| u.pos());
            let heading = lead.and_then(|u| {
                let v = u.vel();
                let len = (v.x * v.x + v.z * v.z).sqrt();
                (len > 1e-3).then(|| (v.x / len, v.z / len))
            });
            let radius = self.airstrike_route_radius(
                sortie.members.iter().filter_map(|id| self.bomber_handler.units.get(id)).map(|a| a.kind),
            );
            let approach = self.airstrike_field(from, enemies);
            let approach_route = approach.route_for(pos, radius, heading);
            let exit = return_route(home, &field.route_for(pos, radius, None));
            let risk = round_trip(
                self.airstrike_path_risk(from, &approach_route, enemies),
                self.airstrike_path_risk(pos, &exit, enemies),
            );
            sortie.return_route = exit;
            if hunting && distance(from, pos) < 200.0 {
                sortie.searched_since.get_or_insert(frame);
            }
            // Blind pursuit keeps the engine's last unit attack intact. Never
            // replace it with a ground attack or refresh hidden unit handles.
            let can_order = observed || hunting || target.is_some_and(|e| e.is_static);
            let reroute = acquired
                || if carpet {
                    refreshed
                } else {
                    can_order && frame - sortie.last_route >= 150 && distance(from, pos) > 800.0
                };
            let route = if reroute { approach_route.clone() } else { Vec::new() };
            // One line per Phoenix still on the sortie, in member order.
            let lines = if carpet { self.carpet_lines(&sortie.job, sortie.members.len()) } else { Vec::new() };
            let disabled = self.dev_off("airstrikes")
                || (hunting && self.dev_off("airspeculate"))
                || (carpet && self.dev_off("carpet"));
            let wave_safe = sortie.wave.is_none_or(|w| {
                distance(w.target, pos) <= wave::MAX_PURSUIT
                    && wave::route_allowed(from, &approach_route, |p| self.graph.is_ally_territory(p))
            });
            let done = disabled
                || !valid
                || !wave_safe
                || troops_gone
                || hatched
                || frame - sortie.launched > sortie_timeout(&sortie.job);
            let mut keep = Vec::new();
            let mut friendly_release = Vec::new();
            for (index, id) in sortie.members.into_iter().enumerate() {
                let line = lines.get(index).copied();
                // With `carpetspare`, napalm must not land on our troops.
                let clear = line.is_none_or(|l| {
                    self.bomber_handler.units.get(&id).is_none_or(|a| distance(a.unit.pos(), l.aim) > 1500.0)
                        || self.carpet_clear(l)
                });
                let Some(a) = self.bomber_handler.units.get_mut(&id) else { continue };
                let u = a.unit;
                // A plane just lifted off a pad may still read 3 (repairing).
                let spent = needs_ammo(u.rules_param_float("noammo", 0.0));
                let forecast_safe = !risk.intercepted
                    && u.def()
                        .is_some_and(|d| sortie.job.flight_safe(a.kind, risk, u.health(), u.max_health(), d.speed()));
                let reachable = sortie.job.kind == MissionKind::Shield
                    || can_hit_submerged(a.kind, sortie.job.kind, target.is_some_and(|e| e.is_submerged()));
                let safe =
                    reachable && clear && sortie.release.allows(id, frame, forecast_safe, u.health(), u.max_health());
                if springai::reserved::is_reserved(self.cb, id) {
                    *self
                        .bomber_handler
                        .utilization
                        .entry(a.kind)
                        .or_default()
                        .aborted
                        .entry("operator")
                        .or_default() += 1;
                    a.returning = true;
                    continue;
                }
                if let Some(l) = line.filter(|_| spent) {
                    friendly_release.push(l);
                }
                if done || spent || !safe {
                    let stats = self.bomber_handler.utilization.entry(a.kind).or_default();
                    if spent {
                        stats.payload_spent += 1;
                    } else {
                        let reason = if !valid {
                            "target_lost"
                        } else if !clear {
                            "friendly_fire"
                        } else if !wave_safe {
                            "wave_depth"
                        } else if !reachable {
                            "submerged_target"
                        } else if !safe {
                            if u.health() < u.max_health() * 0.5 { "damage" } else { "unsafe_route" }
                        } else if frame - sortie.launched > sortie_timeout(&sortie.job) {
                            "timeout"
                        } else if troops_gone {
                            "no_troops"
                        } else if hatched {
                            "hatched"
                        } else if disabled {
                            "disabled"
                        } else {
                            "damage"
                        };
                        *stats.aborted.entry(reason).or_default() += 1;
                    }
                    // Cancel queued attacks even if already over the rally
                    // point (the recovery loop may have no move to issue).
                    u.stop(0);
                    a.returning = true;
                    a.last_order = -1000;
                    self.bomber_handler.cooldown.insert(sortie.job.target, frame);
                    continue;
                }
                if a.kind == Plane::Sparrow {
                    // Guard follows the Odin through its route, instead of
                    // sending the faster jammer ahead to the release point.
                    if let Some(leader) = a.escort.and_then(|id| aircraft.get(&id).copied())
                        && leader.health() > 0.0
                        && leader.rules_param_float("noammo", 0.0) == 0.0
                    {
                        if frame - a.last_order >= 90 {
                            u.guard(leader, 0);
                            a.last_order = frame;
                        }
                        active.insert(id);
                        keep.push(id);
                        continue;
                    }
                    u.stop(0);
                    a.returning = true;
                    a.last_order = -1000;
                    continue;
                }
                if let Some(l) = line.filter(|_| reroute) {
                    Self::carpet_order(u, &approach.route(l.far), l);
                } else if reroute && !route.is_empty() {
                    Self::airstrike_route(u, &route, true, turns);
                    Self::airstrike_attack(u, &sortie.job, target.map(|e| e.unit), pos, OPTION_SHIFT_KEY);
                }
                // Keep the queued safe approach intact. Retarget a moving
                // contact only in the terminal area, at most every three seconds.
                // A carpet's pass is flown as ordered: re-aiming would break it.
                if can_order && !hunting && !carpet && frame - sortie.last_order >= 90 && distance(u.pos(), pos) < 650.0
                {
                    Self::airstrike_attack(u, &sortie.job, target.map(|e| e.unit), pos, 0);
                }
                active.insert(id);
                keep.push(id);
            }
            for l in friendly_release {
                let (n, metal) = self.carpet_exposure(l);
                self.log(&format!(
                    "airstrikes: carpet released at ({:.0}, {:.0}) over {n} friendly ground units ({metal:.0} metal)",
                    l.aim.x, l.aim.z
                ));
            }
            if !keep.is_empty() {
                sortie.members = keep;
                sortie.job.pos = pos;
                if reroute {
                    sortie.last_route = frame;
                    sortie.route = route;
                }
                if frame - sortie.last_order >= 90 {
                    sortie.last_order = frame;
                }
                self.bomber_handler.sorties.push(sortie);
            }
        }
        // Return along a fresh threat-aware route. In enemy territory a direct
        // FIND_PAD order would throw away the safe egress route.
        let ids: Vec<_> = self.bomber_handler.units.keys().copied().filter(|id| !active.contains(id)).collect();
        for id in ids {
            if springai::reserved::is_reserved(self.cb, id) {
                continue;
            }
            let a = &self.bomber_handler.units[&id];
            let u = a.unit;
            let hurt = u.health() < u.max_health() * 0.95;
            let empty = u.rules_param_float("noammo", 0.0) > 0.0;
            let at_home = distance(u.pos(), home) < HOME_RADIUS;
            if !hurt && !empty && self.airstrike_home(u.pos(), home) {
                let a = self.bomber_handler.units.get_mut(&id).unwrap();
                a.returning = false;
                // Over our territory an idle plane is home (`airhome`), and
                // drifts back toward the center without leaving the ready
                // pool, so the planes of a package start together.
                if !at_home && frame - a.last_order >= 30 * 10 {
                    a.last_order = frame;
                    let kind = a.kind;
                    let route = self.airstrike_home_route(u, kind, home, interceptors);
                    if route.is_empty() {
                        u.move_to(home, 0);
                    } else {
                        Self::airstrike_route(u, &route, false, turns);
                    }
                }
                continue;
            }
            if frame - a.last_order < 30 * 10 {
                continue;
            }
            if a.kind == Plane::Sparrow
                && !hurt
                && let Some(leader) = a.escort.and_then(|id| self.bomber_handler.units.get(&id)).map(|a| a.unit)
                && distance(leader.pos(), home) > HOME_RADIUS
            {
                u.guard(leader, 0);
                self.bomber_handler.units.get_mut(&id).unwrap().last_order = frame;
                continue;
            }
            let needs_pad = hurt || empty;
            let route = if !(at_home || needs_pad && self.graph.is_ally_territory(u.pos())) {
                self.airstrike_home_route(u, a.kind, home, interceptors)
            } else {
                vec![]
            };
            let a = self.bomber_handler.units.get_mut(&id).unwrap();
            a.returning = true;
            a.last_order = frame;
            if !route.is_empty() {
                if self.test.as_deref().is_some_and(|t| t.starts_with("airstrike-")) {
                    let pts =
                        |r: &[Vec3]| r.iter().take(4).map(|p| format!("{:.0},{:.0}", p.x, p.z)).collect::<Vec<_>>();
                    let flown = Self::airstrike_flyable(u, &route);
                    self.log(&format!(
                        "airstrike-test: return {id} at {:.0},{:.0} vel {:.1},{:.1} route {:?} flown {:?} ({} of {})",
                        u.pos().x,
                        u.pos().z,
                        u.vel().x,
                        u.vel().z,
                        pts(&route),
                        pts(&flown),
                        flown.len(),
                        route.len()
                    ));
                }
                Self::airstrike_route(u, &route, false, turns);
            }
            if hurt || empty {
                u.execute_custom_command(
                    cmd::FIND_PAD,
                    &[],
                    if route.is_empty() { 0 } else { OPTION_SHIFT_KEY },
                    NO_TIMEOUT,
                );
            }
        }
    }

    fn airstrike_launch(&mut self, frame: i32, home: Vec3) {
        if self.dev_off("airstrikes") {
            return;
        }
        let mut busy: BTreeSet<_> =
            self.bomber_handler.sorties.iter().flat_map(|s| s.members.iter().copied()).collect();
        let mut covered: BTreeSet<_> =
            self.bomber_handler.sorties.iter().flat_map(|s| s.job.targets.iter().copied()).collect();
        let plans = self.bomber_handler.plans.clone();
        // Plans are ordered primary first. Only aircraft actually committed
        // to a sortie are reserved: an unfillable package must not immobilize
        // every aircraft of that type while secondary opportunities expire.
        for plan in plans {
            // Secondary sorties (single-plane statics and opportunities among
            // them) may not crowd out a primary strike: they have their own cap.
            if self.bomber_handler.sorties.len() >= MAX_SORTIES {
                break;
            }
            let job = &plan.job;
            if !job.kind.primary()
                && self.bomber_handler.sorties.iter().filter(|s| !s.job.kind.primary()).count() >= MAX_SECONDARY_SORTIES
            {
                continue;
            }
            if job.targets.iter().any(|id| covered.contains(id))
                || !self.airstrike_hunt_available(job)
                || self.bomber_handler.cooldown.contains_key(&job.target)
            {
                continue;
            }
            // Primary work may take a rearmed aircraft off the pad before its
            // slow repair ends (`padlaunch`); the rest waits for 95%.
            let pad_floor = (job.kind.primary() && !self.dev_off("padlaunch")).then_some(PAD_LAUNCH_HEALTH);
            // Members start wherever they are home, so the package is drawn
            // nearest to where its route begins, and close enough together
            // to arrive together (`airhome`).
            let anchor = plan.route.first().copied().unwrap_or(job.pos);
            let mut selected = None;
            let mut spread_held = false;
            for package in &plan.packages {
                let members = if self.dev_off("airhome") {
                    let mut members = Vec::new();
                    for &(kind, n) in package {
                        let candidates = self.airstrike_ready(kind, &busy, home, anchor, n, pad_floor);
                        if candidates.len() != n {
                            members.clear();
                            break;
                        }
                        members.extend(candidates);
                    }
                    (!members.is_empty()).then_some(members)
                } else {
                    let kinds: Vec<Plane> = package.iter().map(|(k, _)| *k).collect();
                    let candidates = self.airstrike_candidates(&kinds, &busy, home, anchor, pad_floor);
                    let members = pick(package, &candidates, PACK_SPREAD);
                    spread_held |= members.is_none() && pick(package, &candidates, f32::INFINITY).is_some();
                    members
                };
                if let Some(members) = members {
                    selected = Some((members, package.clone()));
                    break;
                }
            }
            if selected.is_none() && spread_held {
                self.bomber_handler.spread_holds += 1;
            }
            // A quarry without counted anti-air (see `Mission::wearable`), unescorted, is
            // worn down by the Raptors that are ready: they keep firing until
            // it dies or flees, so the one-pass package (six for a Revenant)
            // need not be filled. Procurement still buys toward the full one.
            if selected.is_none()
                && wears_down(plan.job.kind, plan.job.wearable, plan.risk)
                && !self.dev_off("raptorwear")
            {
                let full = plan.packages.iter().flatten().filter(|(k, _)| *k == Plane::Raptor).map(|(_, n)| *n).max();
                let ready = self.airstrike_ready(Plane::Raptor, &busy, home, anchor, full.unwrap_or(0), pad_floor);
                if !ready.is_empty() {
                    let n = ready.len();
                    let mut members = ready;
                    let mut package = vec![(Plane::Raptor, n)];
                    // A ready Magpie rides along as in the full package (never
                    // bought or waited for): its EMG slows the quarry, and a
                    // slow quarry is what its gun is good at.
                    if plan.packages.iter().flatten().any(|(k, _)| *k == Plane::Magpie) {
                        let magpie = self.airstrike_ready(Plane::Magpie, &busy, home, anchor, 1, pad_floor);
                        if !magpie.is_empty() {
                            members.extend(magpie);
                            package.insert(0, (Plane::Magpie, 1));
                        }
                    }
                    selected = Some((members, package));
                }
            }
            let Some((members, package)) = selected else { continue };
            // Strafes take only spare Magpies: enough stay idle at home, ours
            // and the firefighters', to answer the next raid.
            if job.kind == MissionKind::Strafe {
                let home_magpies = self
                    .bomber_handler
                    .units
                    .iter()
                    .filter(|(id, a)| {
                        a.kind == Plane::Magpie
                            && !busy.contains(id)
                            && !members.contains(id)
                            && a.unit.rules_param_float("noammo", 0.0) == 0.0
                            && self.airstrike_home(a.unit.pos(), home)
                    })
                    .count()
                    + self.firefighters_idle(Plane::Magpie.name());
                if home_magpies < QRF_MAGPIES {
                    continue;
                }
            }
            // An opportunistic assassination must arrive while the observed
            // window still exists; no procurement or long cross-map pursuit.
            if matches!(job.kind, MissionKind::Assassinate | MissionKind::HeavyAir) {
                let speed = members
                    .iter()
                    .filter_map(|id| self.bomber_handler.units[id].unit.def())
                    .map(|d| d.speed())
                    .fold(f32::INFINITY, f32::min);
                let velocity = self.war.targets.get(&job.target).map_or(Vec3::ZERO, |e| e.unit.vel() * 30.0);
                let window = if job.kind == MissionKind::HeavyAir { 25.0 } else { 12.0 };
                // From where each member actually starts: onto the route at
                // its first waypoint, rather than from home.
                let starts: Vec<Vec3> = members.iter().map(|id| self.bomber_handler.units[id].unit.pos()).collect();
                let length = (starts.iter())
                    .map(|p| plan.length + distance(*p, anchor) - distance(home, anchor))
                    .fold(0.0, f32::max);
                if length / speed.max(1.0) > window
                    || starts.iter().any(|p| intercept_time(*p, job.pos, velocity, speed).is_none_or(|t| t > window))
                {
                    continue;
                }
            }
            if self.airstrike_dispatch(&plan, &members, &package, frame, None) {
                busy.extend(members);
                covered.extend(job.targets.iter().copied());
            }
        }
    }

    fn airstrike_dispatch(
        &mut self,
        plan: &Plan,
        members: &[i32],
        package: &[(Plane, usize)],
        frame: i32,
        wave: Option<WaveFlight>,
    ) -> bool {
        let job = &plan.job;
        let turns = !self.dev_off("airturns");
        let interceptors = if job.kind == MissionKind::Intercept {
            Vec::new()
        } else {
            self.airstrike_interceptors_except((job.kind == MissionKind::HeavyAir).then_some(job.target))
        };
        let exit = self.airstrike_path_risk(job.pos, &plan.return_route, &interceptors);
        if members.iter().any(|id| {
            let a = &self.bomber_handler.units[id];
            // A healthy aircraft may be circling near the rally rather than
            // exactly at home. Check its actual first leg, not an assumed start.
            let risk = round_trip(self.airstrike_path_risk(a.unit.pos(), &plan.route, &interceptors), exit);
            risk.intercepted || !a.unit.def().is_some_and(|d| a.kind.survives(risk, a.unit.health(), d.speed()))
        }) {
            return false;
        }
        // Napalm and implosion blasts land on whoever is under them.
        if !self.airstrike_spares_friends(job, package, &plan.route) {
            return false;
        }
        let route = plan.route.clone();
        if route.is_empty() {
            return false;
        }
        let lines = if job.kind == MissionKind::Carpet {
            let lines = self.carpet_lines(job, members.len());
            if lines.len() != members.len() || !lines.iter().all(|l| self.carpet_clear(*l)) {
                return false;
            }
            lines
        } else {
            Vec::new()
        };
        let carpet_routes: Vec<_> = if lines.is_empty() {
            Vec::new()
        } else {
            let field = self.airstrike_field(self.graph.get_ally_center(), &interceptors);
            lines.iter().map(|l| field.route(l.far)).collect()
        };
        // Lifted off a pad before their repair ended (`padlaunch`).
        let from_pad = (members.iter())
            .filter(|id| self.bomber_handler.units[*id].unit.rules_param_float("noammo", 0.0) == PAD_REPAIRING)
            .count();
        let mut members = members.to_vec();
        let mut package = package.to_vec();
        if job.optional_escort() {
            let busy: BTreeSet<_> =
                self.bomber_handler.sorties.iter().flat_map(|s| s.members.iter().copied()).collect();
            let odins: Vec<_> = members
                .iter()
                .filter_map(|id| {
                    let a = &self.bomber_handler.units[id];
                    (a.kind == Plane::Odin).then_some((*id, a.unit.pos()))
                })
                .collect();
            let escorts: Vec<_> = self
                .bomber_handler
                .units
                .iter()
                .filter(|(id, a)| {
                    a.kind == Plane::Sparrow
                        && !busy.contains(id)
                        && !a.returning
                        && !springai::reserved::is_reserved(self.cb, **id)
                        && a.unit.health() >= a.unit.max_health() * 0.95
                        && a.unit.rules_param_float("noammo", 0.0) == 0.0
                })
                .map(|(&id, a)| (id, a.unit.pos(), a.unit.def().map_or(0.0, |d| d.jammer_radius() as f32)))
                .collect();
            let pairs = escort::pairs(&odins, &escorts);
            let count = pairs.len().min(MAX_WING.saturating_sub(members.len()));
            for (escort, odin) in pairs.into_iter().take(count) {
                self.bomber_handler.units.get_mut(&escort).unwrap().escort = Some(odin);
                members.push(escort);
            }
            if count > 0 {
                package.push((Plane::Sparrow, count));
            }
        }
        for (index, &id) in members.iter().enumerate() {
            let leader =
                self.bomber_handler.units[&id].escort.and_then(|id| self.bomber_handler.units.get(&id)).map(|a| a.unit);
            let a = self.bomber_handler.units.get_mut(&id).unwrap();
            a.returning = false;
            a.last_order = frame;
            if a.kind == Plane::Sparrow {
                if let Some(leader) = leader {
                    a.unit.guard(leader, 0);
                }
            } else if let Some(&l) = lines.get(index) {
                Self::carpet_order(a.unit, &carpet_routes[index], l);
            } else {
                Self::airstrike_route(a.unit, &route, true, turns);
                Self::airstrike_attack(
                    a.unit,
                    job,
                    self.war.targets.get(&job.target).map(|e| e.unit),
                    job.pos,
                    OPTION_SHIFT_KEY,
                );
            }
            self.bomber_handler.utilization.entry(a.kind).or_default().launched += 1;
        }

        self.log(&format!(
            "airstrikes: launch {:?} target={} package={} value={:.0} AA={:.2} exposure={:.2}{}",
            job.kind,
            job.target,
            members.len(),
            job.value,
            plan.risk.aa,
            plan.risk.exposure,
            if from_pad > 0 { format!(" from_pad={from_pad}") } else { String::new() }
        ));
        for l in &lines {
            let (n, metal) = self.carpet_exposure(*l);
            self.log(&format!(
                "airstrikes: carpet line ({:.0}, {:.0}) -> ({:.0}, {:.0}) for {}; {n} friendly ground units under it ({metal:.0} metal)",
                l.near.x,
                l.near.z,
                l.aim.x,
                l.aim.z,
                job.inference.as_ref().map_or("?", |f| f.def.as_str())
            ));
        }
        self.bomber_handler.launched += 1;
        if let Some(fix) = &job.inference {
            self.bomber_handler.speculation.attempted.insert(fix.id, frame);
        }
        self.bomber_handler.sorties.push(Sortie {
            memory: TargetMemory { last_contact: frame, destroyed: false },
            searched_since: None,
            retargeted: false,
            release: release::State::default(),
            wave,
            job: job.clone(),
            members: members.to_vec(),
            launched: frame,
            last_order: frame,
            last_route: frame,
            route,
            return_route: plan.return_route.clone(),
            package: package.to_vec(),
        });
        true
    }

    /// `route` less the waypoints `u` cannot turn onto from its flight now.
    fn airstrike_flyable(u: Unit, route: &[Vec3]) -> Vec<Vec3> {
        let Some(d) = u.def() else { return route.to_vec() };
        let radius = super::turns::turn_radius(
            d.max_rudder(),
            d.max_elevator(),
            d.max_bank(),
            d.front_to_speed(),
            d.speed_to_front(),
        );
        super::turns::flyable(u.pos(), u.vel(), radius, super::turns::arrival_radius(d.turn_radius()), route)
    }

    fn airstrike_route(u: Unit, route: &[Vec3], attacking: bool, turns: bool) {
        // Cut only the bends this airframe cannot turn onto (see `turns`): a
        // heavy bomber would orbit such a waypoint until the next order. Keep
        // every other bend of the planned route.
        let route = if turns { Self::airstrike_flyable(u, route) } else { route.to_vec() };
        // Stop the move queue one cell before impact and let the attack command
        // do the terminal approach.
        let n = if attacking { route.len().saturating_sub(1) } else { route.len() };
        for (i, p) in route.iter().take(n).enumerate() {
            u.move_to(*p, if i == 0 { 0 } else { OPTION_SHIFT_KEY });
        }
        if n == 0 {
            u.stop(0);
        }
    }

    fn airstrike_attack(u: Unit, job: &Mission, target: Option<Unit>, pos: Vec3, options: i16) {
        match job.kind {
            MissionKind::Speculative => {
                // Seek contact, not a blind bomb at the centre of an uncertainty disk.
                u.move_to(pos, options);
            }
            MissionKind::Shield => {
                u.execute_custom_command(cmd::AIR_MANUALFIRE, &[pos.x, pos.y, pos.z], options, NO_TIMEOUT);
            }
            MissionKind::Wind
            | MissionKind::Thermite
            | MissionKind::Turret
            | MissionKind::Static
            | MissionKind::Outpost => {
                u.attack_area(pos, 0.0, options);
            }
            _ => {
                if let Some(t) = target {
                    u.attack(t, options);
                } else {
                    u.attack_area(pos, 0.0, options);
                }
            }
        }
    }

    pub fn airstrikes_status(&self) -> serde_json::Value {
        use serde_json::json;
        let mut status = self.bomber_handler.status(self.frame, self.airstrike_production());
        let busy: BTreeSet<_> = self.bomber_handler.sorties.iter().flat_map(|s| s.members.iter().copied()).collect();
        let home = self.graph.get_ally_center();
        status["speculation_enabled"] = json!(!self.dev_off("airspeculate"));
        status["scout_hunts"] = self.airstrike_scout_status();
        status["fleet"] = json!(self.bomber_handler.units.iter().map(|(&id, a)| {
            let noammo = a.unit.rules_param_float("noammo", 0.0);
            let empty = needs_ammo(noammo);
            let health = a.unit.health() / a.unit.max_health().max(1.0);
            let away = distance(a.unit.pos(), home);
            json!({"id": id, "unit": a.kind.name(), "name": format!("{:?}", a.kind),
                "state": readiness(springai::reserved::is_reserved(self.cb, id), busy.contains(&id), empty, health >= 0.95, !a.returning && self.airstrike_home(a.unit.pos(), home)),
                "health": health, "noammo": empty, "on_pad": noammo == PAD_REPAIRING, "home_distance": away})
        }).collect::<Vec<_>>());
        status
    }
}

/// Both live tools and journal snapshots use the same state projection. Active
/// sorties win over overlapping candidates, including those with a new anchor.
fn mission_view(
    job: &Mission,
    route: &[Vec3],
    return_route: &[Vec3],
    packages: &[Vec<(Plane, usize)>],
    sortie: Option<&Sortie>,
) -> serde_json::Value {
    use serde_json::json;
    json!({
        "id": format!("{:?}:{}", job.kind, job.target),
        "kind": format!("{:?}", job.kind), "label": job.kind.label(), "target": job.target, "targets": job.targets,
        "state": if sortie.is_some() { "in_progress" } else { "available" },
        "return_route": return_route.iter().map(|p| [p.x,p.z]).collect::<Vec<_>>(),
        "wave": sortie.and_then(|s| s.wave.map(|w| w.id)),
        "submerged": job.submerged,
        "primary": job.kind.primary(), "deep": job.deep, "value": job.value,
        "inference": job.inference.as_ref().map(|f| json!({"id": f.id, "def": f.def,
            "pos": [f.pos.x, f.pos.z], "uncertainty": f.uncertainty, "last": f.last})),
        "last_contact": sortie.map(|s| s.memory.last_contact),
        "retargeted": sortie.is_some_and(|s| s.retargeted),
        "seeking_contact": job.kind == MissionKind::Speculative,
        "carpet_lines": job.inference.as_ref().filter(|_| job.kind == MissionKind::Carpet).map(|f| {
            let n = sortie.map_or_else(|| packages.first().map_or(0, |p| p.iter().map(|(_, n)| n).sum()), |s| s.members.len());
            carpet::lines(f, n).iter().map(|l| [[l.near.x, l.near.z], [l.aim.x, l.aim.z]]).collect::<Vec<_>>()
        }),
        "optional_escort": job.optional_escort().then_some(Plane::Sparrow.name()),
        "pos": [job.pos.x, job.pos.z], "seen": job.seen,
        "route": route.iter().map(|p| [p.x, p.z]).collect::<Vec<_>>(),
        "members": sortie.map(|s| s.members.as_slice()).unwrap_or(&[]),
        "launched": sortie.map(|s| s.launched),
        "package_damage_values": matches!(job.kind, MissionKind::Group | MissionKind::Outpost).then(||
            packages.iter().map(|p| job.damage_value(p)).collect::<Vec<_>>()),
        "packages": packages.iter().map(|pkg| pkg.iter().map(|(k,n)| json!({
            "unit": k.name(), "name": format!("{k:?}"), "count": n
        })).collect::<Vec<_>>()).collect::<Vec<_>>()
    })
}

impl BomberHandler {
    fn status(&self, frame: i32, next: Option<&str>) -> serde_json::Value {
        use serde_json::json;
        let covered: BTreeSet<_> = self.sorties.iter().flat_map(|s| s.job.targets.iter().copied()).collect();
        let mut missions: Vec<_> = self
            .sorties
            .iter()
            .map(|s| mission_view(&s.job, &s.route, &s.return_route, std::slice::from_ref(&s.package), Some(s)))
            .collect();
        for p in &self.plans {
            if !p.job.kind.primary() || p.job.targets.iter().any(|id| covered.contains(id)) {
                continue;
            }
            let mut v = mission_view(&p.job, &p.route, &p.return_route, &p.packages, None);
            v["aa"] = json!(p.risk.aa);
            v["exposure"] = json!(p.risk.exposure);
            v["incoming_dps"] = json!(p.risk.aa * 500.0);
            v["ready_volley_damage"] = json!(p.risk.burst);
            missions.push(v);
        }
        json!({"frame": frame, "grounded": self.grounded, "next": next, "aircraft": self.units.len(),
            "ordered": self.orders.len(), "building": self.building.len(),
            "available": missions.len() - self.sorties.len(), "in_progress": self.sorties.len(),
            "missions": missions,
            "collateral_holds": self.collateral_holds.get(),
            "spread_holds": self.spread_holds,
            "combined_strikes": {"checks": self.wave_checks, "last_check": self.last_wave_check,
                "launched": self.waves_launched, "last_launch": self.last_wave_launch},
            "utilization": self.utilization.iter().map(|(k, u)| json!({"unit": k.name(), "name": format!("{k:?}"),
                "aircraft_seconds": u.state_frames.iter().map(|(s, f)| (*s, *f as f64 / 30.0)).collect::<BTreeMap<_,_>>(),
                "aircraft_launched": u.launched, "payload_spent": u.payload_spent, "aborted": u.aborted})).collect::<Vec<_>>(),
            "sorties": self.sorties.iter().map(|s| json!({"kind": format!("{:?}",s.job.kind),
                "target": s.job.target, "members": s.members, "launched": s.launched
            })).collect::<Vec<_>>()})
    }
}

#[cfg(test)]
mod operator_tests {
    use super::*;
    #[test]
    fn active_pursuit_survives_contact_gaps_but_not_confirmed_death() {
        let mut memory = TargetMemory { last_contact: 100, destroyed: false };
        assert!(memory.valid(700, false, false));
        assert!(!memory.valid(701, false, false));
        assert!(memory.valid(710, true, false));
        assert!(memory.valid(1310, false, false));
        assert!(!memory.valid(1311, false, false));
        assert!(memory.valid(5000, false, true));
        memory.destroyed = true;
        assert!(!memory.valid(5001, true, true));
    }

    #[test]
    fn readiness_distinguishes_payload_repair_return_and_operator_control() {
        assert_eq!(readiness(false, false, false, true, true), "ready");
        assert_eq!(readiness(false, false, true, true, true), "rearming");
        assert_eq!(readiness(false, false, false, false, true), "repairing");
        assert_eq!(readiness(false, false, false, true, false), "returning");
        assert_eq!(readiness(false, true, false, true, false), "in_progress");
        assert_eq!(readiness(true, true, false, true, false), "operator");
    }

    #[test]
    fn a_rearmed_plane_repairing_on_the_pad_is_loaded() {
        // Zero-K: 1 flying to a pad, 2 rearming, 3 armed and repairing.
        assert!(!needs_ammo(0.0));
        assert!(needs_ammo(1.0));
        assert!(needs_ammo(2.0));
        assert!(!needs_ammo(PAD_REPAIRING));
        // So a hurt one on the pad reads as repairing, not rearming.
        assert_eq!(readiness(false, false, needs_ammo(PAD_REPAIRING), false, false), "repairing");
    }

    #[test]
    fn a_package_is_drawn_from_planes_that_arrive_together() {
        let c = |id, kind, reach, on_pad| Candidate { id, kind, reach, on_pad, health: 1.0 };
        let likhos = [(Plane::Likho, 2)];
        // One at the edge of our territory near the route, two at home: the
        // nearest pair would arrive 1500 elmos apart, the home pair together.
        let edge =
            [c(1, Plane::Likho, 500.0, false), c(2, Plane::Likho, 2000.0, false), c(3, Plane::Likho, 2100.0, false)];
        assert_eq!(pick(&likhos, &edge, PACK_SPREAD), Some(vec![2, 3]));
        // With a third Likho near the edge one, those two go first.
        let near = [edge[0], edge[1], edge[2], c(4, Plane::Likho, 900.0, false)];
        assert_eq!(pick(&likhos, &near, PACK_SPREAD), Some(vec![1, 4]));
        // Too far apart: held, though the circle-free rule would fill it.
        let apart = [edge[0], edge[1]];
        assert_eq!(pick(&likhos, &apart, PACK_SPREAD), None);
        assert_eq!(pick(&likhos, &apart, f32::INFINITY), Some(vec![1, 2]));
        // In a window, idle planes before those on a pad, then the nearest.
        let pad = [c(5, Plane::Likho, 100.0, true), c(6, Plane::Likho, 300.0, false), c(7, Plane::Likho, 400.0, false)];
        assert_eq!(pick(&likhos, &pad, PACK_SPREAD), Some(vec![6, 7]));
        // Every kind from the same window, no plane twice.
        let mixed = [(Plane::Magpie, 1), (Plane::Raptor, 2)];
        let fleet = [
            c(8, Plane::Raptor, 200.0, false),
            c(9, Plane::Raptor, 300.0, false),
            c(10, Plane::Magpie, 2500.0, false),
            c(11, Plane::Raptor, 2600.0, false),
            c(12, Plane::Raptor, 2700.0, false),
        ];
        assert_eq!(pick(&mixed, &fleet, PACK_SPREAD), Some(vec![10, 11, 12]));
        assert_eq!(pick(&[(Plane::Raptor, 3)], &fleet, f32::INFINITY), Some(vec![8, 9, 11]));
        assert_eq!(pick(&likhos, &[], PACK_SPREAD), None);
    }

    #[test]
    fn snapshots_show_candidate_then_active_then_available_without_duplicate_targets() {
        let job = Mission {
            submerged: false,
            kind: MissionKind::Thermite,
            group: None,
            inference: None,
            target: 5,
            targets: vec![5],
            pos: Vec3::xz(1200.0, 300.0),
            hp: 4000.0,
            value: 4000.0,
            seen: 30,
            deep: true,
            shields: Vec::new(),
            fast: false,
            wearable: false,
        };
        let route = vec![Vec3::xz(500.0, 200.0), job.pos];
        let package = vec![(Plane::Odin, 1), (Plane::Sparrow, 1)];
        let mut h = BomberHandler::default();
        h.plans.push(Plan {
            job: job.clone(),
            return_route: return_route(Vec3::ZERO, &route),
            route: route.clone(),
            packages: vec![package.clone()],
            risk: Risk { aa: 0.0, exposure: 0.0, intercepted: false, burst: 0.0 },
            length: 1400.0,
        });
        for (id, kind) in [
            (6, MissionKind::Static),
            (7, MissionKind::Opportunistic),
            (8, MissionKind::Assassinate),
            (9, MissionKind::Shield),
        ] {
            let mut secondary = h.plans[0].clone();
            secondary.job.kind = kind;
            secondary.job.target = id;
            secondary.job.targets = vec![id];
            h.plans.push(secondary);
        }
        let candidate = h.status(30, Some("bomberassault"));
        assert_eq!(candidate["available"], 1);
        assert_eq!(candidate["missions"].as_array().unwrap().len(), 1);
        assert_eq!(h.plans.len(), 5); // filtering readouts must not remove launch opportunities
        assert_eq!(candidate["missions"][0]["state"], "available");
        assert_eq!(candidate["missions"][0]["pos"], serde_json::json!([1200.0, 300.0]));
        assert_eq!(candidate["missions"][0]["packages"][0][1]["name"], "Sparrow");
        h.sorties.push(Sortie {
            memory: TargetMemory::default(),
            searched_since: None,
            retargeted: false,
            release: release::State::default(),
            wave: None,
            job,
            return_route: return_route(Vec3::ZERO, &route),
            route,
            package,
            members: vec![11, 12],
            launched: 60,
            last_order: 60,
            last_route: 60,
        });
        let active = h.status(90, None);
        assert_eq!(active["missions"].as_array().unwrap().len(), 1);
        assert_eq!(active["available"], 0);
        assert_eq!(active["in_progress"], 1);
        assert_eq!(active["missions"][0]["state"], "in_progress");
        assert_eq!(active["missions"][0]["members"], serde_json::json!([11, 12]));
        assert_eq!(active["missions"][0]["id"], candidate["missions"][0]["id"]);
        h.sorties[0].job.kind = MissionKind::Static;
        let active_secondary = h.status(100, None);
        assert_eq!(active_secondary["missions"].as_array().unwrap().len(), 1);
        assert_eq!(active_secondary["missions"][0]["primary"], false);
        assert_eq!(active_secondary["missions"][0]["state"], "in_progress");
        assert_eq!(active_secondary["in_progress"], 1);
        assert_eq!(active_secondary["available"], 0);
        h.sorties.clear();
        assert_eq!(h.status(120, None)["missions"][0]["state"], "available");
        let mut priority = h.plans[0].clone();
        priority.job.kind = MissionKind::Heavy;
        let key = priority.job.key();
        h.recent.insert(key, priority);
        let mut contact = super::super::tests::enemy(5);
        contact.live = false;
        h.refresh_priority_memory(&[contact.clone()], 300);
        assert!(h.recent.contains_key(&key));
        contact.live = true;
        h.refresh_priority_memory(&[contact.clone()], 1000);
        assert!(h.recent.contains_key(&key));
        h.refresh_priority_memory(&[contact], 300);
        assert!(!h.recent.contains_key(&key));
        h.plans.clear();
        h.grounded = true;
        assert!(h.status(150, None)["missions"].as_array().unwrap().is_empty());
    }
}
