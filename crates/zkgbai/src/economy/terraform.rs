//! Terraforming through Zero-K's terraform gadget
//! (`LuaRules/Gadgets/unit_terraform.lua`): an internal command describes
//! the shape and makes the gadget lay out "terraunits" to build; a fallback
//! command per constructor then has it repair them one after another until
//! the shape is done.
//!
//! The AI digs ditches across the chokepoints it fortifies when the enemy's
//! ground army is vehicles or hovers and ours is not: twelve elmos down is
//! too steep for those (their slope limit is 27 degrees) while bots and
//! amphibious units walk straight through. Once dug, the next terrain
//! re-analysis sees the chokepoint closed to the enemy.

use springai::{NO_TIMEOUT, Unit, Vec3};
use zk_terrain::{ChokeKind, Pos, Tier};

use crate::Ai;
use crate::economy::factory::FacKind;
use crate::terrain::move_class;
use crate::util::{OPTION_SHIFT_KEY, distance};

/// The gadget's custom commands.
const CMD_TERRAFORM_INTERNAL: i32 = 39801;
const CMD_RAISE: i32 = 39737;
const CMD_REPAIR: i32 = 40;

/// Terraform kinds (the internal command's first parameter): level to a
/// height, or raise by one.
const LEVEL: f32 = 1.0;
const RAISE: f32 = 2.0;

/// Terraform tags are the AI's own; widgets count from 1.
const FIRST_TAG: i32 = 1 << 20;

/// Frames a worker just ordered to terraform is left to it before its
/// orders tell (they reach the engine a frame or more later).
const TERRAFORM_HOLD: i32 = 60;

/// Whether `unit` is working on a terraform order: the gadget's own, or
/// the repair of a terraunit it laid out, which the gadget orders first.
pub fn is_terraforming(unit: Unit) -> bool {
    let terraunit = |id: f32| {
        Unit::from_id(unit.callback(), id as i32).and_then(|t| t.def()).is_some_and(|d| d.name() == "terraunit")
    };
    (unit.current_commands().iter())
        .any(|c| c.id() == CMD_RAISE || (c.id() == CMD_REPAIR && c.params().first().is_some_and(|&id| terraunit(id))))
}

/// How deep ditches are dug, and how deep one must be in the middle to
/// count as done (vehicles and hovers stop at 8, see the terraform test).
const DITCH_DEPTH: f32 = 12.0;
const DITCH_DONE: f32 = 9.0;

/// Frames between ditch planning, and before a ditch that did not get dug
/// is tried again.
const DITCH_PERIOD: i32 = 30 * 10;
const DITCH_RETRY: i32 = 30 * 120;

/// Ditches are worth digging from this income on.
const DITCH_MIN_INCOME: f32 = 8.0;

/// Workers this far from a ditch are not sent to dig it, elmos.
const DITCH_WORKER_RANGE: f32 = 2500.0;

/// How far a ditch reaches past its chokepoint's gate at either end, elmos:
/// as far as the ground counting as the gate itself.
const DITCH_OVERHANG: f32 = zk_terrain::GATE_MARGIN;

/// A ditch being dug, or tried.
pub struct Ditch {
    /// Middle of its chokepoint.
    pub mid: Vec3,
    /// Ground height there when it was ordered.
    ground: f32,
    worker: Unit,
    ordered: i32,
}

impl Ai {
    /// Dig a ditch across the next chokepoint we fortify, if the enemy's
    /// ground army cannot cross ditches and ours can; check on the one
    /// being dug.
    pub fn eco_dig_ditches(&mut self, frame: i32) {
        if (frame + self.offset) % DITCH_PERIOD != 0 || self.dev_off("ditch") {
            return;
        }
        let map = self.cb.map();
        if let Some(d) = &self.eco.ditch {
            let dug = d.ground - map.elevation_at(d.mid.x, d.mid.z);
            if dug >= DITCH_DONE {
                self.log(&format!("terraform: ditch at ({:.0}, {:.0}) dug {dug:.0} deep", d.mid.x, d.mid.z));
                self.eco.ditched.push(d.mid);
                self.eco.ditch = None;
            } else if frame - d.ordered > 30 * 30 && !is_terraforming(d.worker) {
                self.eco.ditch_tries.push((d.mid, frame));
                self.eco.ditch = None;
            } else {
                return;
            }
        }

        let Some(s) = &self.strategy else { return };
        let vehicle_factory =
            self.fac.factories.values().any(|f| matches!(f.kind(), FacKind::Veh | FacKind::Tank | FacKind::Hover));
        if !matches!(s.tier, Tier::Vehicle | Tier::Hover)
            || vehicle_factory
            || self.eco.adjusted_income < DITCH_MIN_INCOME
        {
            return;
        }
        let Some(tier_map) = self.terrain().and_then(|t| t.tier(s.tier)) else { return };
        let near = |list: &[Vec3], p: Vec3| list.iter().any(|&q| distance(p, q) < 150.0);
        let tried: Vec<Vec3> =
            self.eco.ditch_tries.iter().filter(|&&(_, f)| frame - f < DITCH_RETRY).map(|&(p, _)| p).collect();
        let site = s.fortify.iter().find_map(|f| {
            let choke = &tier_map.chokes[f.choke];
            // Only across a gate with no way round it: units would walk round
            // a ditch over the slopes beside it.
            let home = s.analysis.home_side(tier_map, f.choke)?;
            if choke.kind != ChokeKind::Gate || choke.bypass_into(home).is_finite() {
                return None;
            }
            let [a, b] = choke.gate?;
            // Measured where it is dug: the middle of the gate.
            let (mx, mz) = ((a.x + b.x) / 2.0, (a.z + b.z) / 2.0);
            let mid = Vec3::new(mx, map.elevation_at(mx, mz), mz);
            // On land, not dug or tried lately.
            if mid.y <= 0.0 || near(&self.eco.ditched, mid) || near(&tried, mid) {
                return None;
            }
            let len = a.distance(b).max(1.0);
            let (ux, uz) = ((b.x - a.x) / len, (b.z - a.z) / len);
            let total = len + 2.0 * DITCH_OVERHANG;
            let line: Vec<Vec3> = (0..=(total / 16.0).ceil() as usize)
                .map(|k| {
                    let t = (16.0 * k as f32).min(total) - DITCH_OVERHANG;
                    Vec3::xz(a.x + ux * t, a.z + uz * t)
                })
                .collect();
            Some((mid, line))
        });
        let Some((mid, line)) = site else { return };

        // The nearest worker that is not running from anything.
        let worker = self
            .eco
            .workers
            .values()
            .filter(|w| !w.is_chicken && !w.assigned_plop && !w.unit.is_being_built())
            .map(|w| (distance(w.get_pos(), mid), w.id))
            .filter(|&(d, _)| d <= DITCH_WORKER_RANGE)
            .min_by(|a, b| a.0.total_cmp(&b.0));
        let Some((_, wid)) = worker else { return };
        let Some(w) = self.eco.workers.get_mut(&wid) else { return };
        w.clear_task(&mut self.eco.tasks);
        let unit = w.unit;
        self.terraform_line(&[unit], &line, -DITCH_DEPTH, false);
        self.eco.ditch = Some(Ditch { mid, ground: mid.y, worker: unit, ordered: frame });
        self.log(&format!(
            "terraform: digging a ditch at ({:.0}, {:.0}), {} elmos long",
            mid.x,
            mid.z,
            16 * (line.len() - 1)
        ));
    }

    /// Have `builders` raise the ground along `line` by `height` elmos (a
    /// wall), or lower it with a negative height (a ditch). `queue` adds
    /// the work after the builders' other orders.
    pub fn terraform_line(&mut self, builders: &[Unit], line: &[Vec3], height: f32, queue: bool) {
        self.terraform(builders, line, false, RAISE, height, queue);
    }

    /// Have `builders` level the ground along `line` to `height` (a wall
    /// with a flat top, if raised; done where it is that high already).
    pub fn terraform_line_level(&mut self, builders: &[Unit], line: &[Vec3], height: f32, queue: bool) {
        self.terraform(builders, line, false, LEVEL, height, queue);
    }

    /// Have `builders` raise the ground inside the polygon `area` by
    /// `height` elmos (the gadget slopes its edges like a pyramid).
    pub fn terraform_area(&mut self, builders: &[Unit], area: &[Vec3], height: f32, queue: bool) {
        self.terraform(builders, area, true, RAISE, height, queue);
    }

    /// Have `builders` level the ground inside the polygon `area` to
    /// `height` (a flat top, if raised).
    pub fn terraform_level(&mut self, builders: &[Unit], area: &[Vec3], height: f32, queue: bool) {
        self.terraform(builders, area, true, LEVEL, height, queue);
    }

    /// Keep `builders` out of the economy's assignment for `frames` more
    /// (a raise borrowed for longer than the order's own hold: the Crab
    /// spire's).
    pub(crate) fn eco_hold_terraform(&mut self, builders: &[Unit], frames: i32) {
        for b in builders {
            self.eco.terraform_hold.insert(b.id(), self.frame + frames);
        }
    }

    pub(crate) fn eco_worker_held(&self, id: i32) -> bool {
        self.eco.terraform_hold.get(&id).is_some_and(|&until| until > self.frame)
            || self.eco_in_transit(id)
            || self.logistics_holds(id)
    }

    pub(crate) fn eco_release_terraform(&mut self, builders: &[Unit]) {
        for b in builders {
            self.eco.terraform_hold.remove(&b.id());
        }
    }

    fn terraform(&mut self, builders: &[Unit], line: &[Vec3], area: bool, kind: f32, height: f32, queue: bool) {
        let Some(&first) = builders.first() else { return };
        if line.len() < 2 {
            return;
        }
        let n = line.len() as f32;
        let (cx, cz) = line.iter().fold((0.0, 0.0), |(x, z), p| (x + p.x / n, z + p.z / n));
        self.eco.terraform_tags += 1;
        let tag = (FIRST_TAG + self.eco.terraform_tags) as f32;
        let mut params = vec![
            kind,
            self.team_id as f32,
            cx,
            cz,
            tag,
            if area { 1.0 } else { 0.0 }, // a closed area, or a line
            height,
            n,
            builders.len() as f32,
            0.0, // any volume change
        ];
        params.extend(line.iter().flat_map(|p| [p.x, p.z]));
        params.extend(builders.iter().map(|b| b.id() as f32));
        first.execute_custom_command(CMD_TERRAFORM_INTERNAL, &params, 0, NO_TIMEOUT);
        let y = self.cb.map().elevation_at(cx, cz);
        let opts = if queue { OPTION_SHIFT_KEY } else { 0 };
        for b in builders {
            b.execute_custom_command(CMD_RAISE, &[cx, y, cz, tag], opts, NO_TIMEOUT);
            self.eco.terraform_hold.insert(b.id(), self.frame + TERRAFORM_HOLD);
        }
    }
}

/// Depths of the test ditches, elmos.
const TEST_DEPTHS: [f32; 5] = [4.0, 6.0, 8.0, 10.0, 14.0];

/// Length of a test ditch and the spacing between them, elmos.
const TEST_LENGTH: f32 = 240.0;
const TEST_SPACING: f32 = 160.0;

/// Development aid (`ZKGBAI_TERRAFORM_TEST` in the environment): the
/// commander digs ditches of several depths next to its start and the AI
/// reports, now and then, how steep they came out and which movement tiers
/// can still cross them. The AI does nothing else meanwhile.
#[derive(Default)]
pub struct TerraformTest {
    /// Each ditch's ends and depth.
    ditches: Vec<([Vec3; 2], f32)>,
    /// A spike raised: its middle, and the ground there before.
    spike: Option<(Vec3, f32)>,
}

impl Ai {
    /// Whether the terraform test is on (the economy then stands still).
    pub fn terraform_testing(&self) -> bool {
        self.eco.terraform_test.is_some()
    }

    pub fn terraform_test_update(&mut self, frame: i32) {
        if frame == 150 {
            self.terraform_test_dig();
        } else if frame > 150 && frame % 1800 == 0 {
            self.terraform_test_report();
        }
    }

    fn terraform_test_dig(&mut self) {
        let Some(builder) = self.cb.team_units().into_iter().find(|u| {
            u.def().is_some_and(|d| d.speed() > 0.0 && !d.build_options().is_empty() && d.terraform_speed() > 0.0)
        }) else {
            self.log("terraform-test: no constructor");
            return;
        };
        let start = builder.pos();
        let map = self.cb.map();
        // Towards the middle of the map, so the ditches stay on it.
        let (w, h) = (map.width() as f32 * 8.0, map.height() as f32 * 8.0);
        let dz = if start.z < h / 2.0 { 1.0 } else { -1.0 };
        let dx = if start.x < w / 2.0 { 1.0 } else { -1.0 };
        let mut ditches = Vec::new();
        for (i, &depth) in TEST_DEPTHS.iter().enumerate() {
            let z = start.z + dz * (200.0 + TEST_SPACING * i as f32);
            let (x0, x1) = (start.x + dx * 100.0, start.x + dx * (100.0 + TEST_LENGTH));
            let ends = [Vec3::new(x0, 0.0, z), Vec3::new(x1, 0.0, z)];
            let line: Vec<Vec3> =
                (0..=(TEST_LENGTH / 16.0) as usize).map(|k| Vec3::new(x0 + dx * 16.0 * k as f32, 0.0, z)).collect();
            self.terraform_line(&[builder], &line, -depth, i > 0);
            ditches.push((ends, depth));
        }
        // And a spike (see the watchtowers), beside the ditches.
        let mid = Vec3::xz(start.x - dx * 300.0, start.z + dz * 200.0);
        let square =
            [(-24.0, -24.0), (24.0, -24.0), (24.0, 24.0), (-24.0, 24.0)].map(|(x, z)| Vec3::xz(mid.x + x, mid.z + z));
        let ground = map.elevation_at(mid.x, mid.z);
        let top = square.iter().map(|p| map.elevation_at(p.x, p.z)).fold(ground, f32::max);
        self.terraform_level(&[builder], &square, top + 64.0, true);
        let orders: Vec<i32> = builder.current_commands().iter().map(|c| c.id()).collect();
        self.log(&format!(
            "terraform-test: digging {} ditches from ({:.0}, {:.0}), raising a spike at ({:.0}, {:.0}); orders {orders:?}",
            ditches.len(),
            start.x,
            start.z,
            mid.x,
            mid.z
        ));
        self.eco.terraform_test = Some(TerraformTest { ditches, spike: Some((mid, ground)) });
    }

    fn terraform_test_report(&self) {
        let Some(test) = &self.eco.terraform_test else { return };
        let map = self.cb.map();
        if let Some((mid, ground)) = test.spike {
            let up: Vec<String> =
                (-5..=5).map(|k| format!("{:.0}", map.elevation_at(mid.x + 8.0 * k as f32, mid.z) - ground)).collect();
            self.log(&format!("terraform-test: spike across {up:?}"));
        }
        let slopes = map.slope_map();
        let sw = (map.width() / 2).max(1) as usize;
        let slope_at = |x: f32, z: f32| {
            let (cx, cz) = ((x / 16.0) as usize, (z / 16.0) as usize);
            slopes.get(cz * sw + cx).copied().unwrap_or(0.0)
        };
        let limit = |unit: &str| self.defs.get(unit).and_then(move_class).map_or(f32::NAN, |m| m.max_slope);
        self.log(&format!(
            "terraform-test: slope limits: vehicle {:.3}, hover {:.3}, bot {:.3}, amphib {:.3}",
            limit("vehcon"),
            limit("hovercon"),
            limit("cloakcon"),
            limit("amphcon")
        ));
        for &([a, b], depth) in &test.ditches {
            let (lo, hi) = (a.x.min(b.x), a.x.max(b.x));
            // Steepest cell across the ditch, and the depth reached in the
            // middle of it.
            let mut steepest = 0.0f32;
            let mut x = lo + 32.0;
            while x <= hi - 32.0 {
                let mut dz = -64.0;
                while dz <= 64.0 {
                    steepest = steepest.max(slope_at(x, a.z + dz));
                    dz += 8.0;
                }
                x += 16.0;
            }
            let mid = (lo + hi) / 2.0;
            let profile: Vec<String> = (-6..=6)
                .map(|k| {
                    format!("{:.0}", map.elevation_at(mid, a.z + 8.0 * k as f32) - map.elevation_at(mid, a.z + 80.0))
                })
                .collect();
            // Where in its middle third the terrain model lets each tier
            // straight across.
            let crossing = |tier: Tier| {
                let Some(m) = self.terrain().and_then(|t| t.tier(tier)) else { return "?".to_owned() };
                let third = (hi - lo) / 3.0;
                let open = (0..=8)
                    .map(|k| lo + third + third * k as f32 / 8.0)
                    .filter(|&x| (-8..=8).all(|k| m.is_passable(Pos::new(x, a.z + 8.0 * k as f32))))
                    .count();
                format!("{open}/9")
            };
            self.log(&format!(
                "terraform-test: depth {depth:>4.0}: steepest {steepest:.3}; across {profile:?}; open to vehicle {}, hover {}, bot {}, amphib {}",
                crossing(Tier::Vehicle),
                crossing(Tier::Hover),
                crossing(Tier::Bot),
                crossing(Tier::Amphib)
            ));
        }
    }
}
