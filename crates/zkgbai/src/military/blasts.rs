//! Keeping our units clear of blasts we know are coming (`nukeclear`): our
//! own nuke in flight (see `nukes`), and every missile Zero-K tells us of,
//! ours or an ally's, as allied players see them: "MISSILE_FIRED:id,team,
//! weapon,x,y,z,frame" (its projectile, owner team, weapon, predicted
//! impact point and frame) and then "MISSILE_DESTROYED:id" (landed, or
//! shot down). Nothing tells us of the enemy's.
//!
//! While one flies, no path goes into its blast, and paths that would end
//! inside end at its rim (see `Pathfinder::no_go`); attack-moves out of it
//! or into it are plain moves; whoever is inside and not leaving is sent
//! out; and those waiting by the rim are not stuck.

use std::collections::BTreeMap;

use springai::Vec3;

use crate::Ai;
use crate::defs::cmd;
use crate::kgbutil::kgb_move_to;
use crate::military::nukes::segment_distance;
use crate::util::distance;

/// Our units keep this far beyond a blast's radius, and stand still within
/// this much more of that on purpose (waiting for it to land; not stuck).
const KEEP_MARGIN: f32 = 100.0;
const CLEAR_WAIT: f32 = 400.0;

/// Frames between looks for our units in a blast to send clear.
const CLEAR_EVERY: i32 = 10;

/// A unit found standing still in a blast after this many sends out is
/// logged (stuck, likely).
const STUCK_SENDS: u32 = 3;

/// A missile not heard of again is taken to be gone this long after it
/// was due to land, frames.
const LATE: i32 = 60;

/// The engine's move order (a unit leaving a blast may have one).
const CMD_MOVE: i32 = 10;

/// Whose blast: our nuke launched at a frame, or a missile Zero-K told us
/// of, by its projectile id.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Blast {
    Ours(i32),
    Missile(i32),
}

/// A missile Zero-K told us of.
struct Missile {
    team: i32,
    weapon: String,
    aim: Vec3,
    radius: f32,
    lands: i32,
}

#[derive(Default)]
pub struct Blasts {
    missiles: BTreeMap<i32, Missile>,
    /// Our units sent clear of each blast, and how many times each.
    cleared: BTreeMap<Blast, BTreeMap<i32, u32>>,
}

/// Whether a unit at `at` moving to `to` leaves the blast at `aim`: it
/// goes, and gets no nearer the aim on its way (within `ON_WAY`).
fn leaving(at: Vec3, to: Vec3, aim: Vec3) -> bool {
    const ON_WAY: f32 = 16.0;
    distance(at, to) > 1.0 && segment_distance(aim, at, to) >= distance(aim, at) - ON_WAY
}

/// "MISSILE_FIRED:id,team,weapon,x,y,z,frame": the projectile id, its
/// owner's team, weapon name, impact point and frame.
fn parse_fired(data: &str) -> Option<(i32, i32, &str, Vec3, i32)> {
    let mut f = data.strip_prefix("MISSILE_FIRED:")?.split(',').map(str::trim);
    let (id, team, weapon) = (f.next()?.parse().ok()?, f.next()?.parse().ok()?, f.next()?);
    let [x, y, z, frame]: [f32; 4] =
        [f.next()?, f.next()?, f.next()?, f.next()?].map(|s| s.parse().unwrap_or(f32::NAN));
    [x, y, z, frame].iter().all(|v| v.is_finite()).then_some((id, team, weapon, Vec3::new(x, y, z), frame as i32))
}

impl Ai {
    /// The blasts to keep clear of now: whose, centre and radius kept.
    fn blasts_now(&self) -> Vec<(Blast, Vec3, f32)> {
        if self.dev_off("nukeclear") {
            return Vec::new();
        }
        let ours = self.nukes_flight().map(|(fired, at, r)| (Blast::Ours(fired), at, r + KEEP_MARGIN));
        let missiles = self.blasts.missiles.iter().map(|(&id, m)| (Blast::Missile(id), m.aim, m.radius + KEEP_MARGIN));
        ours.into_iter().chain(missiles).collect()
    }

    /// Whether a unit at `at` may be standing still to wait out a blast.
    pub fn blasts_waiting(&self, at: Vec3) -> bool {
        self.pathfinder.no_go_near(at, CLEAR_WAIT)
    }

    pub fn blasts_update(&mut self, frame: i32) {
        let late: Vec<i32> =
            (self.blasts.missiles.iter()).filter(|(_, m)| frame > m.lands + LATE).map(|(&id, _)| id).collect();
        for id in late {
            self.blasts_missile_gone(id, "not heard of since it was due");
        }
        let blasts = self.blasts_now();
        self.pathfinder.no_go = blasts.iter().map(|&(_, at, r)| (at, r)).collect();
        if !blasts.is_empty() && (frame + self.offset) % CLEAR_EVERY == 0 {
            for (blast, at, r) in blasts {
                self.blasts_clear(blast, at, r);
            }
        }
    }

    /// Send our units in the blast at `aim` (kept `r` around) clear of it,
    /// but those leaving already (moving away from its aim) and those
    /// orbiting a leader (who goes, or waits by the rim).
    fn blasts_clear(&mut self, blast: Blast, aim: Vec3, r: f32) {
        let (team, frame) = (self.team_id, self.frame);
        let mut sent = Vec::new();
        for u in self.cb.friendly_units_in_cylinder(aim, r) {
            let Some(def) = u.def() else { continue };
            if u.team() != team || u.health() <= 0.0 || u.is_being_built() || def.speed() <= 0.0 {
                continue;
            }
            let at = u.pos();
            let order = u.current_commands().first().map(|c| (c.id(), c.params()));
            let staying = order.as_ref().is_none_or(|(id, p)| match *id {
                cmd::ORBIT => false,
                CMD_MOVE | cmd::RAW_MOVE => p.len() < 3 || !leaving(at, Vec3::new(p[0], p[1], p[2]), aim),
                _ => true,
            });
            if !staying {
                continue;
            }
            let Some(to) = self.pathfinder.no_go_exit(at, at) else { continue };
            kgb_move_to(u, to, 0, frame);
            // Sent out before and found with no orders again: moving still
            // (ZK's raw move may drop its order on the way), or stuck.
            let times = self.blasts.cleared.get(&blast).and_then(|c| c.get(&u.id())).copied().unwrap_or(0);
            if order.is_none() && times == STUCK_SENDS && u.speed() < 0.1 * u.max_speed() {
                let map = self.cb.map();
                self.log(&format!(
                    "blasts: a {} ({}) stands still at ({:.0}, {:.0}), {:.0} from the aim, height {:.0} (the aim {:.0}), \
                     after {times} sends out: stuck? (paralyzed {})",
                    def.name(),
                    u.id(),
                    at.x,
                    at.z,
                    distance(at, aim),
                    map.elevation_at(at.x, at.z),
                    map.elevation_at(aim.x, aim.z),
                    u.is_paralyzed()
                ));
            }
            sent.push((u.id(), def.name(), order.map_or(0, |o| o.0)));
        }
        // Units sent before were taken back in by some order: which.
        let cleared = self.blasts.cleared.entry(blast).or_default();
        let (mut first, mut again): (BTreeMap<String, u32>, BTreeMap<String, u32>) = Default::default();
        for (id, name, order) in sent {
            let times = cleared.entry(id).or_insert(0);
            *times += 1;
            if *times == 1 {
                *first.entry(name).or_insert(0) += 1;
            } else {
                *again.entry(format!("{name} (order {order})")).or_insert(0) += 1;
            }
        }
        let list = |m: &BTreeMap<String, u32>| m.iter().map(|(k, n)| format!("{n} {k}")).collect::<Vec<_>>().join(", ");
        let what = match blast {
            Blast::Ours(_) => "our nuke".to_owned(),
            Blast::Missile(id) => self.blasts.missiles.get(&id).map_or("a missile".to_owned(), |m| m.weapon.clone()),
        };
        let at = format!("({:.0}, {:.0})", aim.x, aim.z);
        if !first.is_empty() {
            self.log(&format!("blasts: sending {} clear of {what} at {at}", list(&first)));
        }
        if !again.is_empty() {
            self.log(&format!("blasts: sending {} clear of {what} at {at} again", list(&again)));
        }
    }

    /// How sending our units clear of `blast` went, for the log; forgets it.
    pub fn blasts_report(&mut self, blast: Blast) -> String {
        let Some(cleared) = self.blasts.cleared.remove(&blast).filter(|c| !c.is_empty()) else {
            return String::new();
        };
        let again = cleared.values().filter(|&&n| n > 1).count();
        let most = cleared.values().copied().max().unwrap_or(0);
        let again = if again > 0 { format!(", {again} again (one {most} times)") } else { String::new() };
        format!("; {} of our units sent clear{again}", cleared.len())
    }

    /// A message from the game: a missile of ours or an ally's fired, or
    /// gone.
    pub fn blasts_lua_message(&mut self, data: &str) {
        if let Some(id) = data.strip_prefix("MISSILE_DESTROYED:").and_then(|s| s.trim().parse().ok()) {
            self.blasts_missile_gone(id, "down");
            return;
        }
        let Some((id, team, weapon, aim, lands)) = parse_fired(data) else { return };
        // Our nuke keeps its own blast (see `nukes`).
        if team == self.team_id && self.nukes_flight().is_some_and(|(_, at, _)| distance(at, aim) < 400.0) {
            return;
        }
        let radius = self.cb.weapon_def_by_name(weapon).map_or(0.0, |w| w.area_of_effect());
        if team != self.team_id {
            self.log(&format!(
                "blasts: an ally's {weapon} (team {team}) lands at ({:.0}, {:.0}) in {:.0} s, blast {radius:.0}",
                aim.x,
                aim.z,
                (lands - self.frame) as f32 / 30.0
            ));
        }
        let weapon = weapon.to_owned();
        self.blasts.missiles.insert(id, Missile { team, weapon, aim, radius, lands });
    }

    /// Whether missile `id` is still on its way.
    pub fn blasts_has_missile(&self, id: i32) -> bool {
        self.blasts.missiles.contains_key(&id)
    }

    /// An ally's nuke on its way with at least `lead` frames to land: its
    /// projectile id and where it lands (a development aid, see `nukes`).
    pub fn blasts_ally_nuke(&self, lead: i32) -> Option<(i32, Vec3)> {
        (self.blasts.missiles.iter())
            .find(|(_, m)| m.team != self.team_id && m.weapon == "staticnuke_crblmssl" && m.lands - self.frame >= lead)
            .map(|(&id, m)| (id, m.aim))
    }

    fn blasts_missile_gone(&mut self, id: i32, how: &str) {
        let Some(m) = self.blasts.missiles.remove(&id) else { return };
        let report = self.blasts_report(Blast::Missile(id));
        if !report.is_empty() || m.team != self.team_id {
            self.log(&format!(
                "blasts: {} of team {} at ({:.0}, {:.0}) {how}{report}",
                m.weapon, m.team, m.aim.x, m.aim.z
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn leaving_the_blast() {
        let aim = Vec3::xz(0.0, 0.0);
        let at = Vec3::xz(300.0, 0.0);
        // Straight out, and out at an angle.
        assert!(leaving(at, Vec3::xz(1200.0, 0.0), aim));
        assert!(leaving(at, Vec3::xz(700.0, 400.0), aim));
        // Square to the aim: never nearer.
        assert!(leaving(at, Vec3::xz(300.0, 1200.0), aim));
        // Across the aim, past it on the way, round it, standing still.
        assert!(!leaving(at, Vec3::xz(-1200.0, 0.0), aim));
        assert!(!leaving(at, Vec3::xz(-300.0, 100.0), aim));
        assert!(!leaving(at, Vec3::xz(0.0, 300.0), aim));
        assert!(!leaving(at, at, aim));
    }

    #[test]
    fn missile_messages() {
        let (id, team, weapon, at, frame) =
            parse_fired("MISSILE_FIRED:5021,1,tacnuke_weapon,2130,85,5080,13627").unwrap();
        assert_eq!((id, team, weapon, frame), (5021, 1, "tacnuke_weapon", 13627));
        assert_eq!((at.x, at.y, at.z), (2130.0, 85.0, 5080.0));
        assert!(parse_fired("MISSILE_FIRED:5021,1,tacnuke_weapon,2130,85").is_none());
        assert!(parse_fired("MISSILE_FIRED:x,1,w,1,2,3,4").is_none());
        assert!(parse_fired("NUKE_EXPLOSION:1,2,3").is_none());
    }
}
