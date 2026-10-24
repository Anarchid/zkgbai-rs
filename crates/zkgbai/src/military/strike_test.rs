//! Development aid: a staged strike on an anti-nuke, set up with cheats by
//! the AI option `test`, without a war in the way. With `test=anti`, the AI
//! gets four Fusions at its base and an anti-nuke well off to the side
//! (out of sight of them, with turrets by it), and otherwise stands by.
//! With `test=bite`, its factories build nothing but the Widows wanted; it
//! gets a spider factory at once, and once its economy is going (a silo
//! makes the economy guard it with turrets) a nuke silo and a radar by the
//! enemy's base, that sees the Fusions but not the anti-nuke. Both have
//! their storage topped up every 10 s and do not resign. Put against each
//! other, the attacker should fire, be intercepted, find the anti-nuke with
//! Widows, bite it, and get its next missile through. With `test=eos`
//! instead, the attacker gets a radar by the enemy's base that sees the
//! Fusions and then goes, and a missile silo within reach of them: its Eos
//! should count each Fusion they hit gone, unseen, and strike each once.
//! With `test=drop` (hovers) or `test=dropbot` (bots), the attacker gets a
//! radar by the enemy's base and, every few minutes, four Charons and four
//! assaults and riots at home; any landing counts, walkable or not (see
//! `drops`), so each round flies a sortie to the enemy's buildings. With
//! `test=bertha`, the attacker gets five Fusions and a radar at home at
//! once, so its economy goes for a superweapon (a Bertha, mostly) soon.
//! With `test=colony`, it gets a ground constructor and a Charon (or with
//! `test=colonyair` an air constructor) to colonize the free metal far off
//! (see `economy::colonize`). With `test=aafort`, a passive defender gets
//! static and mobile anti-air around its base; with `test=aaprobe`, the
//! attacker keeps flying unarmed Charons over the enemy's base, to be shot
//! at by what it does not see (see `air_intel`; cheats name every attacker,
//! so the fixes are scored against where the shooters truly are). With
//! `test=fort` (or `test=radarfort`: a Lotus by each mex and a radar
//! farther out, which only the radar shadows) and `test=shadowprobe`, see
//! `shadow_probe`. With
//! `test=precision` (against `test=fort` or `test=radarfort`), the
//! attacker gets two each of Glaives, Scythes, Scorchers and Kodachis every
//! two minutes, as solo raiders, and a
//! radar toward the enemy: their precision missions (see
//! `handlers::precision`) are all that should hurt the fort. With
//! `test=deepscout` (against `test=fort`), the attacker gets a cloak
//! factory and two Fusions at once, so it wants a deep scout and has no
//! spider factory: the factory builds the Scythes wanted (see `scouts`).
//! A radar by the enemy's base shows it the fort for a while, and goes. With
//! `test=heavies`, a passive defender gets two Grizzlies and a Cyclops
//! halfway to the enemy, and a factory behind its base; its commander holds
//! its fire back there; with `test=jaeger`, the attacker gets a spider
//! factory and an Owl at once, and an assault squad's worth of Knights and
//! Wardens every few minutes: its spider factory builds the Widows its
//! jaeger squads want (see `jaeger`), which should stun the heavies for the
//! assaults to kill. With `test=farm`, a passive defender gets twelve
//! wind generators, four caretakers and three more caretakers apart by its
//! base: light things together, for the Infernos of a `test=eos` attacker,
//! which has nothing there worth an Eos (see `missiles`): the first two
//! are worth one, the three caretakers are not. With `test=impaler`
//! (against `test=fort`), the attacker gets four Impalers, two Sparrows to
//! show it the fort, and a radar covering it, every few minutes: its
//! batteries (see `impalers`) are what should dismantle the fort. With
//! `test=impalerwar`, an AI plays as ever, and four Impalers join it at
//! minutes 8, 12 and 16 (the only cheats): the same guns for batteries or,
//! with `disable=impalers`, for the artillery handler. With
//! `test=hijack`, from
//! the first minute to the eighth up to three constructors about to start
//! a mex are handed a far mex job just then, as the AI does now and then
//! (its orders reach the engine a frame later, so it starts the mex
//! anyway);
//! every few minutes the log tells how many construction jobs hold a
//! finished building (stuck: whoever takes them repairs it, and builds
//! nothing). With `test=geo`, the AI gets a geothermal plant on the vent
//! nearest its base and upgrades it whatever its income (see
//! `economy::geo`): the morph and the wall box, against `NullAI`. With
//! `test=snipers` (or `test=lances`), a passive defender gets a Phantom
//! (a Lance) posted 1400 elmos out toward the enemy every two and a half
//! minutes, to one side then the other, every second one with a Reaver
//! behind it; with `test=hunt`, the attacker gets a radar toward the
//! enemy and, every two minutes, six Glaives, two Kodachis, a Blitz, a
//! Scorcher and a Sparrow at home, which raid as ever: the strikes (see
//! `strikes`) are what should kill the snipers, measured by `strikes:`
//! lines (kills, metal lost per kill, units sniped outside strikes). With
//! `test=siege` (against `test=fort`, with `enable=siege`), the attacker
//! gets two Tremors, two Emissaries, two Ogres and a Minotaur at home, two
//! Sparrows to show it the fort and a radar covering it, every four
//! minutes: its siege forces (see `taskforce::siege`) should station
//! beyond the fort's guns and dismantle it, measured by the `taskforce:`
//! deploy, undeploy and kill lines.

use springai::Vec3;

use crate::Ai;
use crate::military::missiles;
use crate::util::{OPTION_SHIFT_KEY, distance, get_directional_point};

/// When the attacker gets its silo, and its radar for `eos`.
const SILO_FRAME: i32 = 30 * 300;
const RADAR_FRAME: i32 = 30 * 100;

/// From when to when constructors are handed another job as they start a
/// mex, how many at most, and from when the jobs are looked at, each
/// minute (`hijack`).
const HIJACK_FRAME: i32 = 30 * 60;
const HIJACK_CHECK: i32 = 30 * 480;
const HIJACKS: u32 = 3;
const HIJACK_REPORT: i32 = 30 * 180;
static HIJACKED: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

/// How often probing Charons come.
const PROBE_ROUND: i32 = 30 * 40;

/// How many metal spots the fort holds, and the defenses by them.
const FORT_SPOTS: usize = 8;
const FORT_GUNS: [&str; 8] = [
    "turretlaser",
    "turretmissile",
    "turretriot",
    "turretlaser",
    "turretheavylaser",
    "turretemp",
    "turretgauss",
    "turretheavy",
];

/// When precision raiders come, and how often.
const PRECISION_FRAME: i32 = 30 * 60;
const PRECISION_ROUND: i32 = 30 * 120;

/// When the attacker's assaults come in a jaeger test, and how often.
const JAEGER_ARMY_FRAME: i32 = 30 * 90;
const JAEGER_ARMY_ROUND: i32 = 30 * 180;

/// When the attacker's Impalers come, and how often.
const IMPALER_FRAME: i32 = 30 * 60;
const IMPALER_ROUND: i32 = 30 * 300;

/// When the siege attacker gets its guns, and how often again.
const SIEGE_FRAME: i32 = 30 * 60;
const SIEGE_ROUND: i32 = 30 * 240;

/// When four Impalers join a real war (`impalerwar`): minutes 8, 12, 16
/// (when Rovers play CircuitAI, games go on past 17; earlier drops found
/// no enemy building known and raiders at home).
const IMPALER_WAR: [i32; 3] = [30 * 480, 30 * 720, 30 * 960];

/// How often a sniper team is posted (`snipers`, `lances`), where, and
/// how often the hunters come (`hunt`).
const SNIPER_ROUND: i32 = 30 * 150;
const SNIPER_OUT: f32 = 1400.0;
const SNIPER_ASIDE: f32 = 500.0;
const HUNT_ROUND: i32 = 30 * 120;

/// When the drop rounds start, and how often they come.
const DROP_FRAME: i32 = 30 * 60;
const DROP_ROUND: i32 = 30 * 180;

impl Ai {
    /// Whether this side's cheated units stand where they are posted (the
    /// sniper teams), out of the handlers' hands.
    pub fn strike_test_posted(&self) -> bool {
        matches!(self.test.as_deref(), Some("snipers" | "lances"))
    }

    pub fn strike_test_update(&mut self, frame: i32) {
        if self.test.as_deref() == Some("shadowprobe") {
            self.shadow_probe_update(frame);
        }
        if self.test.as_deref() == Some("hijack") && frame % 30 == 0 && (HIJACK_FRAME..HIJACK_CHECK).contains(&frame) {
            self.strike_test_hijack();
        }
        if frame != 60 && frame % 300 != 0 {
            return;
        }
        let Some(test) = self.test.clone() else { return };
        // Our base: the commander's place (the ally center is unreliable
        // on a passive side, and at the first frames).
        let com = (self.cb.team_units().into_iter()).find(|u| self.defs.name_of(*u).starts_with("dyn")).map(|u| u.pos());
        let base = com.filter(|p| p.x > 0.0 || p.z > 0.0).unwrap_or_else(|| self.graph.get_ally_center());
        let map = self.cb.map();
        let (w, h) = (map.width() as f32 * 8.0, map.height() as f32 * 8.0);
        // The enemy's base, before we have seen it: across the map from ours.
        let enemy = Vec3::xz(w - base.x, h - base.z);
        // Toward the enemy: as known, else as guessed (an unknown enemy
        // center sits on ours, and a direction from a point to itself is
        // NaN: cheated units then land at (0, 0)).
        let known = self.graph.get_enemy_center();
        let toward = if distance(known, base) > 200.0 { known } else { enemy };
        let ahead = |d: f32| get_directional_point(base, toward, d);
        // Off to the side of the way from the enemy, on the map.
        let (dx, dz) = (toward.x - base.x, toward.z - base.z);
        let n = (dx * dx + dz * dz).sqrt().max(1.0);
        let side = [1.0, -1.0]
            .map(|s| Vec3::xz(base.x - s * dz / n * 1800.0, base.z + s * dx / n * 1800.0))
            .into_iter()
            .find(|p| p.x > 100.0 && p.z > 100.0 && p.x < w - 100.0 && p.z < h - 100.0)
            .unwrap_or_else(|| ahead(1800.0));
        let spawn: Vec<(&str, Vec3)> = match (test.as_str(), frame) {
            ("anti", 60) => vec![
                ("staticantinuke", side),
                ("turretheavylaser", Vec3::xz(side.x + 200.0, side.z)),
                ("turretheavylaser", Vec3::xz(side.x - 200.0, side.z)),
                ("turretriot", Vec3::xz(side.x, side.z + 200.0)),
                ("energyfusion", ahead(300.0)),
                ("energyfusion", ahead(500.0)),
                ("energyfusion", ahead(-300.0)),
                ("energyfusion", ahead(-500.0)),
            ],
            ("bite", 60) => vec![("factoryspider", ahead(400.0))],
            ("bite", SILO_FRAME) => vec![("staticnuke", ahead(-500.0)), ("staticradar", enemy)],
            ("eos" | "deepscout", RADAR_FRAME) => vec![("staticradar", enemy)],
            ("deepscout", 60) => {
                vec![("factorycloak", ahead(400.0)), ("energyfusion", ahead(-300.0)), ("energyfusion", ahead(-500.0))]
            }
            ("eos", SILO_FRAME) => vec![(missiles::SILO, get_directional_point(enemy, base, 2500.0))],
            ("bertha", 60) => {
                let mut units = [-600.0, -400.0, -200.0, 200.0, 400.0]
                    .map(|d| ("energyfusion", Vec3::xz(base.x - dz / n * d, base.z + dx / n * d)))
                    .to_vec();
                // Superweapons wait for a radar.
                units.push(("staticradar", ahead(300.0)));
                units
            }
            ("aafort", 60) => {
                let aside = |d: f32, s: f32| {
                    let p = ahead(d);
                    Vec3::xz(p.x - dz / n * s, p.z + dx / n * s)
                };
                vec![
                    ("turretaafar", aside(700.0, 0.0)),
                    ("turretaaheavy", aside(-200.0, 900.0)),
                    ("turretaalaser", aside(400.0, 500.0)),
                    ("turretaalaser", aside(400.0, -500.0)),
                    ("turretaaflak", aside(-400.0, -600.0)),
                    ("vehaa", aside(200.0, -1300.0)),
                    ("vehaa", aside(250.0, -1350.0)),
                    ("vehaa", aside(150.0, -1250.0)),
                ]
            }
            ("fort", 60) => {
                let mut spots: Vec<Vec3> = self.graph.metal_spots.iter().map(|m| m.position).collect();
                spots.sort_by(|a, b| distance(*a, base).total_cmp(&distance(*b, base)));
                let mut units = vec![("planescout", ahead(-100.0)), ("planescout", ahead(-200.0))];
                for (k, &s) in spots.iter().take(FORT_SPOTS).enumerate() {
                    let gun = get_directional_point(s, toward, 140.0);
                    let (gx, gz) = ((gun.x - s.x) / 140.0, (gun.z - s.z) / 140.0);
                    units.push(("staticmex", s));
                    units.push((FORT_GUNS[k % FORT_GUNS.len()], gun));
                    units.push(("energysolar", Vec3::xz(s.x - gz * 110.0, s.z + gx * 110.0)));
                    units.push(("energysolar", Vec3::xz(gun.x + gz * 90.0 + gx * 50.0, gun.z - gx * 90.0 + gz * 50.0)));
                    // A factory beside some, to hide behind.
                    if k % 3 == 1 {
                        units.push((
                            "factorycloak",
                            Vec3::xz(gun.x - gz * 160.0 + gx * 60.0, gun.z + gx * 160.0 + gz * 60.0),
                        ));
                    }
                }
                units
            }
            ("precision", f) if f >= PRECISION_FRAME && (f - PRECISION_FRAME) % PRECISION_ROUND == 0 => {
                let mut units: Vec<(&str, Vec3)> = Vec::new();
                for name in ["cloakraid", "cloakheavyraid", "vehraid", "tankraid"] {
                    units.extend([(name, ahead(-100.0)), (name, ahead(-160.0))]);
                }
                if f == PRECISION_FRAME {
                    units.push(("staticradar", get_directional_point(enemy, base, 1500.0)));
                }
                units
            }
            ("impaler", f) if f >= IMPALER_FRAME && (f - IMPALER_FRAME) % IMPALER_ROUND == 0 => {
                let mut units = vec![("vehheavyarty", ahead(-100.0)); 4];
                units.extend([("planelightscout", ahead(-200.0)); 2]);
                // Covering the fort (out of its guns' reach, a new one each
                // round): our hits on it are heard of.
                units.push(("staticradar", get_directional_point(enemy, base, 2200.0)));
                units
            }
            ("siege", f) if f >= SIEGE_FRAME && (f - SIEGE_FRAME) % SIEGE_ROUND == 0 => {
                let mut units = vec![("tankheavyarty", ahead(-100.0)); 2];
                units.extend([("tankarty", ahead(-160.0)); 2]);
                units.extend([("tankriot", ahead(-220.0)); 2]);
                units.push(("tankassault", ahead(-280.0)));
                units.extend([("planelightscout", ahead(-340.0)); 2]);
                units.push(("staticradar", get_directional_point(enemy, base, 2200.0)));
                units
            }
            ("radarfort", 60) => {
                let mut spots: Vec<Vec3> = self.graph.metal_spots.iter().map(|m| m.position).collect();
                spots.sort_by(|a, b| distance(*a, base).total_cmp(&distance(*b, base)));
                let mut units = vec![("planescout", ahead(-100.0)), ("planescout", ahead(-200.0))];
                for &s in spots.iter().take(FORT_SPOTS) {
                    units.push(("staticmex", s));
                    units.push(("turretlaser", get_directional_point(s, toward, 120.0)));
                    units.push(("staticradar", get_directional_point(s, toward, 270.0)));
                }
                units
            }
            ("heavies", 60) => {
                // Halfway to the enemy; a factory keeps the AI team alive
                // (ZK ends an AI team with no constructor or factory).
                let half = distance(base, enemy) / 2.0;
                vec![
                    ("amphassault", get_directional_point(base, enemy, half)),
                    ("amphassault", get_directional_point(base, enemy, half + 150.0)),
                    ("tankheavyassault", get_directional_point(base, enemy, half - 150.0)),
                    ("factorycloak", get_directional_point(base, enemy, -300.0)),
                ]
            }
            ("farm", 60) => {
                // Twelve wind generators, four caretakers, and three
                // caretakers (too few to be worth an Inferno).
                let mut units = Vec::new();
                for (d, s, name, count) in
                    [(450.0, 0.0, "energywind", 12), (300.0, 450.0, "staticcon", 4), (300.0, -450.0, "staticcon", 3)]
                {
                    let p = ahead(d);
                    let c = Vec3::xz(p.x - dz / n * s, p.z + dx / n * s);
                    for k in 0..count {
                        let (i, j) = ((k / 4) as f32 - 1.0, (k % 4) as f32 - 1.5);
                        units.push((name, Vec3::xz(c.x + 70.0 * i, c.z + 70.0 * j)));
                    }
                }
                units
            }
            ("jaeger", 60) => vec![("factoryspider", ahead(400.0)), ("planescout", ahead(-100.0))],
            ("geo", 60) => {
                let vents = self.eco_geo_vents();
                let near = vents.into_iter().min_by(|a, b| distance(*a, base).total_cmp(&distance(*b, base)));
                near.map(|v| vec![("energygeo", v)]).unwrap_or_default()
            }
            ("jaeger", f) if f >= JAEGER_ARMY_FRAME && (f - JAEGER_ARMY_FRAME) % JAEGER_ARMY_ROUND == 0 => (0..6)
                .map(|k| {
                    (["cloakassault", "cloakriot"][k % 2], get_directional_point(base, enemy, 1200.0 - 30.0 * k as f32))
                })
                .collect(),
            ("aaprobe", f) if f % PROBE_ROUND == 0 && f > 0 => vec![("gunshiptrans", ahead(-200.0)); 2],
            ("colony", 60) => vec![("cloakcon", ahead(-150.0)), ("gunshiptrans", ahead(-250.0))],
            ("colonyair", 60) => vec![("gunshipcon", ahead(-150.0))],
            ("drop" | "dropbot", f) if f >= DROP_FRAME && (f - DROP_FRAME) % DROP_ROUND == 0 => {
                let cargo = if test == "drop" { ["hoverassault", "hoverriot"] } else { ["cloakassault", "cloakriot"] };
                let mut units = vec![("gunshiptrans", ahead(-200.0)); 4];
                units.extend((0..4).map(|k| (cargo[k % 2], ahead(-100.0))));
                if f == DROP_FRAME {
                    units.push(("staticradar", enemy));
                }
                units
            }
            ("impalerwar", f) if IMPALER_WAR.contains(&f) => vec![("vehheavyarty", ahead(-100.0)); 4],
            ("snipers" | "lances", f) if f == 60 || (f > 0 && f % SNIPER_ROUND == 0) => {
                let round = f / SNIPER_ROUND;
                let s = if round % 2 == 0 { 1.0 } else { -1.0 };
                let aside = |d: f32| {
                    let p = ahead(d);
                    Vec3::xz(p.x - dz / n * s * SNIPER_ASIDE, p.z + dx / n * s * SNIPER_ASIDE)
                };
                let sniper = if test == "snipers" { "cloaksnipe" } else { "hoverarty" };
                let mut units = vec![(sniper, aside(SNIPER_OUT))];
                if round % 2 == 1 {
                    units.push(("cloakriot", aside(SNIPER_OUT - 120.0)));
                }
                // A fort about the commander, so the game runs its course
                // (stage1: the hunters' raiders killed it at 4:29).
                if f == 60 {
                    units.push(("factorycloak", ahead(-250.0)));
                    for (d, s) in [(250.0, 0.0), (0.0, 250.0), (0.0, -250.0), (-120.0, 160.0)] {
                        let p = ahead(d);
                        units.push(("turretlaser", Vec3::xz(p.x - dz / n * s, p.z + dx / n * s)));
                    }
                    units.push(("turretriot", ahead(120.0)));
                }
                units
            }
            ("hunt", f) if f == 60 || (f > 0 && f % HUNT_ROUND == 0) => {
                let mut units = vec![("cloakraid", ahead(-100.0)); 6];
                units.extend([("tankraid", ahead(-150.0)); 2]);
                units.extend([("tankheavyraid", ahead(-200.0)), ("vehraid", ahead(-250.0)), ("planelightscout", ahead(-300.0))]);
                if f == 60 {
                    units.push(("staticradar", get_directional_point(enemy, base, 1800.0)));
                }
                units
            }
            _ => Vec::new(),
        };
        // Impaler tests cheat only to spawn (and `impaler` to top up): the
        // engine shows a cheating AI every unit's true place and health,
        // which would make light of what batteries must know by sight.
        let quiet = matches!(test.as_str(), "impaler" | "impalerwar" | "siege");
        let topped = frame % 300 == 0 && test != "impalerwar";
        if frame == 60 || (quiet && (topped || !spawn.is_empty())) {
            self.cb.cheats_set_enabled(true);
        }
        if test == "hijack" && frame >= HIJACK_REPORT && frame % 1800 == 0 {
            self.strike_test_stuck_jobs();
        }
        if (test == "eos" || test == "deepscout") && frame == RADAR_FRAME + 1500 {
            let radars = self.cb.team_units().into_iter().filter(|u| self.defs.name_of(*u) == "staticradar");
            for r in radars.filter(|r| distance(r.pos(), enemy) < 1500.0) {
                r.self_destruct(0);
                self.log("strike-test: the radar goes");
            }
        }
        for (name, at) in spawn {
            let Some(def) = self.defs.get(name) else { continue };
            let site = self.cb.map().find_closest_build_site(def, at, 1200.0, 4, 0);
            let unit = self.cb.cheat_new_unit(def, if site.x >= 0.0 { site } else { at });
            // A short walk, so it goes idle and joins its handler as if
            // built (cheated units get no idle event otherwise). A sniper
            // team stands its post instead, firing at will.
            if let Some(u) = unit.filter(|_| def.speed() > 0.0) {
                if self.strike_test_posted() {
                    u.set_move_state(0, 0);
                    u.set_fire_state(2, 0);
                } else {
                    let p = u.pos();
                    u.move_to(Vec3::new(p.x + 40.0, p.y, p.z + 40.0), 0);
                }
            }
            // Precision raiders go solo (see `handlers::precision`).
            if let Some(u) = unit.filter(|_| test == "precision" && def.speed() > 0.0) {
                let fighter = crate::military::Fighter::new_raider(u, def.cost(self.metal), self.team_id);
                self.raider_add_solo_raider(fighter);
            }
            // (A cheated unit's own position reads (0, 0) this frame.)
            let p = if site.x >= 0.0 { site } else { at };
            self.log(&format!("strike-test: {name} at ({:.0}, {:.0}){}", p.x, p.z, if unit.is_none() { ": refused" } else { "" }));
        }
        if topped {
            for res in [self.metal, self.energy] {
                self.cb.cheat_resource(res, 20000.0);
            }
        }
        if quiet {
            self.cb.cheats_set_enabled(false);
        }
        // Owls keep watch over the fort, and the commander holds its fire
        // (it would shoot the probes; losing it ends the game).
        if matches!(test.as_str(), "fort" | "radarfort" | "heavies" | "farm" | "snipers" | "lances") && frame == 300 {
            for c in self.cb.team_units().into_iter().filter(|u| self.defs.name_of(*u).starts_with("dyn")) {
                c.set_fire_state(0, 0);
                c.set_move_state(0, 0);
                // Out of the way of the heavies' fight.
                if test == "heavies" {
                    c.move_to(get_directional_point(base, enemy, -600.0), 0);
                }
            }
        }
        if (test == "fort" || test == "radarfort") && frame == 60 {
            let mut spots: Vec<Vec3> = self.graph.metal_spots.iter().map(|m| m.position).collect();
            spots.sort_by(|a, b| distance(*a, base).total_cmp(&distance(*b, base)));
            spots.truncate(FORT_SPOTS);
            let owls = self.cb.team_units().into_iter().filter(|u| self.defs.name_of(*u) == "planescout");
            for (k, o) in owls.enumerate() {
                for (j, s) in spots.iter().enumerate() {
                    o.patrol_to(spots[(j + k * 3) % spots.len()], if j == 0 { 0 } else { OPTION_SHIFT_KEY });
                    let _ = s;
                }
            }
        }
        // Probing Charons fly over the enemy's base, this way and that.
        if test == "aaprobe" && frame % 300 == 0 {
            let charons = self.cb.team_units().into_iter().filter(|u| self.defs.name_of(*u) == "gunshiptrans");
            for (k, c) in charons.enumerate() {
                let s = if (k + (frame / 300) as usize).is_multiple_of(2) { 1.0 } else { -1.0 };
                let across = Vec3::xz(enemy.x - s * dz / n * 1500.0, enemy.z + s * dx / n * 1500.0);
                c.move_to(across, 0);
                c.move_to(Vec3::xz(enemy.x + s * dz / n * 1500.0, enemy.z - s * dx / n * 1500.0), OPTION_SHIFT_KEY);
            }
        }
        // The Widows wanted, from an idle spider factory.
        let spiders = self.fac.factories.values().map(|f| f.unit).find(|u| self.defs.name_of(*u) == "factoryspider");
        if test == "bite"
            && let Some(fac) = spiders.filter(|f| f.current_commands().is_empty())
            && self.scouts_want_widow()
            && let Some(widow) = self.defs.get(crate::military::scouts::WIDOW)
        {
            fac.build(widow, fac.pos(), 0, 0);
            self.scouts_ordered();
            self.log("strike-test: a Widow ordered");
        }
        // The Scythes wanted, from an idle cloak factory.
        let cloaks = self.fac.factories.values().map(|f| f.unit).find(|u| self.defs.name_of(*u) == "factorycloak");
        if test == "deepscout"
            && let Some(fac) = cloaks.filter(|f| f.current_commands().is_empty())
            && self.scouts_want_scythe()
            && let Some(scythe) = self.defs.get(crate::military::scouts::SCYTHE)
        {
            fac.build(scythe, fac.pos(), 0, 0);
            self.scouts_ordered_scythe();
            self.log("strike-test: a Scythe ordered");
        }
        if test == "jaeger"
            && let Some(fac) = spiders.filter(|f| f.current_commands().is_empty())
            && self.jaeger_wants(crate::military::scouts::WIDOW)
            && let Some(widow) = self.defs.get(crate::military::scouts::WIDOW)
        {
            fac.build(widow, fac.pos(), 0, 0);
            self.jaeger_ordered(crate::military::scouts::WIDOW);
            self.log("strike-test: a jaeger Widow ordered");
        }
    }

    /// Hand a constructor about to start a mex another, far mex job, as
    /// the AI does when a cheaper job turns up just then: its orders reach
    /// the engine a frame later, so it starts the first mex all the same
    /// (see `test=hijack`).
    fn strike_test_hijack(&mut self) {
        let Some(mex) = self.defs.get("staticmex") else { return };
        let is_mex =
            |t: &crate::economy::WorkerTask| t.as_construction().is_some_and(|c| c.build_type.id() == mex.id());
        let frame = self.frame;
        let eco = &self.eco;
        let pick = eco.workers.values().find_map(|w| {
            let job = w.task.and_then(|t| eco.tasks.get(t)).filter(|t| is_mex(t))?;
            let reach = w.unit.def()?.build_distance() * 1.5;
            if job.as_construction()?.target.is_some() || distance(w.unit.pos(), job.get_pos()) > reach {
                return None;
            }
            let other = (eco.construction_tasks.iter().copied())
                .filter(|&id| {
                    eco.tasks.get(id).is_some_and(|t| is_mex(t) && distance(t.get_pos(), job.get_pos()) > 300.0)
                })
                .min_by(|&a, &b| {
                    let d = |id| eco.tasks.get(id).map_or(f32::MAX, |t| distance(t.get_pos(), job.get_pos()));
                    d(a).total_cmp(&d(b))
                })?;
            Some((w.id, job.get_pos(), other))
        });
        if HIJACKED.load(std::sync::atomic::Ordering::Relaxed) >= HIJACKS {
            return;
        }
        let Some((wid, near, other)) = pick else { return };
        HIJACKED.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let far = self.eco.tasks.get(other).map_or(near, |t| t.get_pos());
        if let Some(w) = self.eco.workers.get_mut(&wid) {
            // It builds the near one (the order the engine has), while the
            // AI has it down for the far one.
            w.unit.build(mex, near, 0, 0);
            w.clear_task(&mut self.eco.tasks);
            w.set_task(&mut self.eco.tasks, other, frame);
        }
        self.log(&format!(
            "strike-test: a constructor about to build the mex at ({:.0}, {:.0}) is given the mex job at ({:.0}, {:.0})",
            near.x, near.z, far.x, far.z
        ));
    }

    /// Log the construction jobs that hold a finished building.
    fn strike_test_stuck_jobs(&self) {
        let stuck: Vec<String> = (self.eco.construction_tasks.iter())
            .filter_map(|&id| self.eco.tasks.get(id))
            .filter_map(|t| {
                let target = t.as_construction()?.target?;
                (target.health() > 0.0 && !target.is_being_built()).then(|| {
                    let (p, q) = (t.get_pos(), target.pos());
                    format!("({:.0}, {:.0}) holding one at ({:.0}, {:.0})", p.x, p.z, q.x, q.z)
                })
            })
            .collect();
        self.log(&format!("strike-test: {} jobs hold a finished building: {}", stuck.len(), stuck.join("; ")));
    }
}
