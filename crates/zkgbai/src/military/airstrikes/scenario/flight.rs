//! `airstrike-turns` measures how tightly each airplane really turns: one of
//! each, held by the fixture, flies out from our side of the map and is then
//! ordered back home, a full turn at full rudder. `airstrike-track:` lines
//! (every 5 frames here, every 10 in `airstrike-odin`) give flight paths for
//! offline fits against `turns::turn_radius`.
use super::super::turns;
use crate::Ai;
use springai::Vec3;

const PLANES: [&str; 7] = [
    "bomberassault",
    "bomberheavy",
    "bomberprec",
    "bomberriot",
    "planesupport",
    "planeheavyfighter",
    "planelightscout",
];
const OUT: i32 = 120;
const BACK: i32 = 420;
const DONE: i32 = 1500;

impl Ai {
    /// Our aircraft's flight, for offline path fits.
    pub(super) fn airstrike_track(&self, frame: i32) {
        for u in self.cb.team_units() {
            let Some(d) = u.def().filter(|d| d.is_able_to_fly()) else { continue };
            let (p, v) = (u.pos(), u.vel());
            let queue = u.current_commands();
            let order = queue.first().map_or("-".to_owned(), |c| {
                let q = c.params();
                format!("{}@{:.0},{:.0}", c.id(), q.first().unwrap_or(&0.0), q.get(2).unwrap_or(&0.0))
            });
            self.log(&format!(
                "airstrike-track: f={frame} id={} def={} x={:.0} z={:.0} vx={:.2} vz={:.2} noammo={} hp={:.0} model_r={:.0} queue={} first={order}",
                u.id(),
                self.defs.name(d),
                p.x,
                p.z,
                v.x,
                v.z,
                u.rules_param_float("noammo", 0.0),
                100.0 * u.health() / u.max_health().max(1.0),
                turns::turn_radius(
                    d.max_rudder(),
                    d.max_elevator(),
                    d.max_bank(),
                    d.front_to_speed(),
                    d.speed_to_front()
                ),
                queue.len(),
            ));
        }
    }

    pub(super) fn airstrike_turns_scenario(&mut self, frame: i32) {
        if frame > DONE {
            return;
        }
        let map = self.cb.map();
        let (w, h) = (map.width() as f32 * 8.0, map.height() as f32 * 8.0);
        let base = self.graph.get_ally_center();
        let (dx, dz) = (w / 2.0 - base.x, h / 2.0 - base.z);
        let len = (dx * dx + dz * dz).sqrt().max(1.0);
        let (dir, side) = ((dx / len, dz / len), (-dz / len, dx / len));
        let lane = |i: usize, along: f32| {
            let off = (i as f32 - 3.0) * 250.0;
            Vec3::xz(
                (base.x + dir.0 * along + side.0 * off).clamp(200.0, w - 200.0),
                (base.z + dir.1 * along + side.1 * off).clamp(200.0, h - 200.0),
            )
        };
        if frame == 60 {
            self.cb.cheats_set_enabled(true);
            for (i, name) in PLANES.into_iter().enumerate() {
                let Some(d) = self.defs.get(name) else { continue };
                if let Some(u) = self.cb.cheat_new_unit(d, lane(i, 800.0)) {
                    springai::reserved::reserve(self.cb, u.id());
                    springai::reserved::as_owner(|| u.set_fire_state(0, 0));
                    self.log(&format!("airstrike-test: spawned {name} {}", u.id()));
                }
            }
        }
        if frame == OUT || frame == BACK {
            for u in self.cb.team_units() {
                let Some(i) = PLANES.iter().position(|n| *n == self.defs.name_of(u)) else { continue };
                let goal = if frame == OUT { lane(i, 4300.0) } else { lane(i, -400.0) };
                springai::reserved::as_owner(|| u.move_to(goal, 0));
            }
            self.log(&format!("airstrike-turns: {} at {frame}", if frame == OUT { "out" } else { "back" }));
        }
        if frame >= OUT && frame % 5 == 0 {
            self.airstrike_track(frame);
        }
        if frame == DONE {
            self.log("airstrike-turns: done");
        }
    }

    /// `airstrike-strider` (a defender): the held strider hub starts a Dante
    /// beside itself and builds it slowly, a frame that cannot move for
    /// minutes. Progress lines show what the Odin found and when it burned.
    pub(super) fn airstrike_strider_frame(&mut self, frame: i32) {
        if frame == 120
            && let Some(hub) = self.cb.team_units().into_iter().find(|u| self.defs.name_of(*u) == "striderhub")
            && let Some(dante) = self.defs.get("striderdante")
        {
            let near = Vec3::xz(hub.pos().x - 220.0, hub.pos().z);
            let p = self.cb.map().find_closest_build_site(dante, near, 200.0, 1, 0);
            let at = if p.x >= 0.0 { p } else { near };
            let ok = springai::reserved::as_owner(|| hub.build(dante, at, 0, 0));
            self.log(&format!("airstrike-test: hub {} starts striderdante at {:.0},{:.0}: {ok}", hub.id(), at.x, at.z));
        }
        if frame % 150 == 0 {
            for u in self.cb.team_units() {
                let name = self.defs.name_of(u);
                if name.starts_with("strider") && name != "striderhub" {
                    self.log(&format!(
                        "airstrike-test: frame {name} {} built={} progress={:.2} hp={:.0}",
                        u.id(),
                        !u.is_being_built(),
                        u.build_progress(),
                        u.health()
                    ));
                }
            }
        }
    }
}
