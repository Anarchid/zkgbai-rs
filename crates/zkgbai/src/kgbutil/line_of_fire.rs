//! Lines of fire over the terrain and the buildings on it: where a gun can
//! hit a unit, and where it cannot (its fire shadow).
//!
//! The engine lets a weapon fire only along a free line
//! (`CWeapon::HaveFreeLineOfFire`): from where it aims from to the target's
//! aim point, terrain blocks the shot unless the ray meets the ground within
//! the weapon's area of effect of the target, and a weapon that avoids
//! friendlies (the default) holds fire rather than shoot through its own
//! side's units' collision volumes. So a gun's own buildings shadow it just
//! as hills do. The volumes are convex and stand on the ground, so a line
//! misses one exactly when it passes over its top surface (a dome for an
//! ellipsoid, a disc for an upright cylinder): that surface is what stands
//! here. Cannons test their arc instead of a line; lobbed shots (high
//! trajectories, vertical launchers) go over everything.

/// Elmos per heightmap square.
pub const SQUARE: f32 = 8.0;
/// Elmos per shadow cell.
pub const CELL: f32 = 16.0;
/// Elmos between samples along a ray.
const STEP: f32 = 8.0;
/// A line passes a sample this much below it.
const GRAZE: f32 = 1.0;

/// The ground for lines of fire: terrain heights and the top surfaces of
/// the buildings' collision volumes.
pub struct FireGround {
    /// Corner heights, `(w + 1) * (h + 1)`, row-major.
    corners: Vec<f32>,
    /// Size in heightmap squares.
    w: i32,
    h: i32,
    /// Highest top surface of a collision volume (absolute) at each
    /// square's centre; `NEG_INFINITY` where none stands.
    tops: Vec<f32>,
}

/// The shape of a collision volume (the engine's, with the axis of a
/// cylinder).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shape {
    Ellipsoid,
    CylinderX,
    CylinderY,
    CylinderZ,
    Box,
}

/// A building's collision volume.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Volume {
    /// Its centre (absolute height `y`).
    pub x: f32,
    pub y: f32,
    pub z: f32,
    /// Half its extent along each axis.
    pub hx: f32,
    pub hy: f32,
    pub hz: f32,
    pub shape: Shape,
}

impl Volume {
    /// Height of its top surface over `(x, z)`; `NEG_INFINITY` off it.
    pub fn top_at(&self, x: f32, z: f32) -> f32 {
        let (u, w) = ((x - self.x) / self.hx.max(0.1), (z - self.z) / self.hz.max(0.1));
        let lift = match self.shape {
            Shape::Ellipsoid if u * u + w * w <= 1.0 => (1.0 - u * u - w * w).sqrt(),
            Shape::CylinderY if u * u + w * w <= 1.0 => 1.0,
            Shape::CylinderX if u.abs() <= 1.0 && w.abs() <= 1.0 => (1.0 - w * w).sqrt(),
            Shape::CylinderZ if u.abs() <= 1.0 && w.abs() <= 1.0 => (1.0 - u * u).sqrt(),
            Shape::Box if u.abs() <= 1.0 && w.abs() <= 1.0 => 1.0,
            _ => return f32::NEG_INFINITY,
        };
        self.y + self.hy * lift
    }
}

/// How a gun's shots fly.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Flight {
    /// Straight (beams, lasers, lightning, missiles).
    Line,
    /// A cannon's low arc at `speed` elmos/s under `gravity` elmos/s².
    Arc { speed: f32, gravity: f32 },
    /// Over everything (high trajectories, vertical launch).
    Lob,
}

/// A gun that may fire at our units.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Gun {
    pub x: f32,
    pub z: f32,
    /// Absolute height its shots start from.
    pub muzzle: f32,
    pub range: f32,
    /// The weapon's `heightMod`: height differences count this much
    /// against its range.
    pub height_mod: f32,
    /// Radius of its area of effect: ground this close to the target does
    /// not stop it firing.
    pub aoe: f32,
    /// Radius of its own body, which does not block it.
    pub body: f32,
    pub flight: Flight,
    /// Whether it holds fire rather than shoot through its own side's
    /// buildings.
    pub avoids_friends: bool,
    /// Whether its shots pass through terrain (flamethrowers).
    pub through_ground: bool,
}

impl Gun {
    /// Whether a target at `(x, z)` with its aim point at absolute height `y`
    /// is within reach (`CWeapon::TestRange`), give or take `margin` elmos.
    pub fn reaches(&self, x: f32, z: f32, y: f32, margin: f32) -> bool {
        let dh = (y - self.muzzle) * self.height_mod;
        let range = self.range + margin;
        let reach = (range * range - dh * dh).max(0.0);
        (x - self.x).powi(2) + (z - self.z).powi(2) <= reach
    }
}

/// Where one gun can hit: for each cell around it, the least height above
/// the ground that a target's aim point needs for the gun to have a free
/// line to it.
pub struct Shadow {
    /// Cell coordinates of the patch's corner, and its side in cells.
    x0: i32,
    z0: i32,
    n: i32,
    need: Vec<f32>,
}

impl FireGround {
    /// From the engine's corner heightmap of a map `w` x `h` squares.
    pub fn new(corners: Vec<f32>, w: i32, h: i32) -> Self {
        let tops = vec![f32::NEG_INFINITY; (w.max(0) * h.max(0)) as usize];
        FireGround { corners, w, h, tops }
    }

    /// Replace the terrain heights (after terraforming); the buildings stay.
    pub fn set_terrain(&mut self, corners: Vec<f32>) {
        self.corners = corners;
    }

    /// Size in elmos.
    pub fn size(&self) -> (f32, f32) {
        (self.w as f32 * SQUARE, self.h as f32 * SQUARE)
    }

    /// Stand `volumes` on the ground, replacing the ones before.
    pub fn set_volumes(&mut self, volumes: impl IntoIterator<Item = Volume>) {
        self.tops.fill(f32::NEG_INFINITY);
        for v in volumes {
            let sx0 = ((v.x - v.hx) / SQUARE).floor().max(0.0) as i32;
            let sz0 = ((v.z - v.hz) / SQUARE).floor().max(0.0) as i32;
            let sx1 = (((v.x + v.hx) / SQUARE).ceil() as i32).min(self.w);
            let sz1 = (((v.z + v.hz) / SQUARE).ceil() as i32).min(self.h);
            for sz in sz0..sz1 {
                for sx in sx0..sx1 {
                    let top = v.top_at((sx as f32 + 0.5) * SQUARE, (sz as f32 + 0.5) * SQUARE);
                    let t = &mut self.tops[(sz * self.w + sx) as usize];
                    *t = t.max(top);
                }
            }
        }
    }

    /// Terrain height at `(x, z)`, interpolated between the square's corners.
    pub fn ground(&self, x: f32, z: f32) -> f32 {
        let fx = (x / SQUARE).clamp(0.0, self.w as f32);
        let fz = (z / SQUARE).clamp(0.0, self.h as f32);
        let (cx, cz) = ((fx as i32).min(self.w - 1).max(0), (fz as i32).min(self.h - 1).max(0));
        let (tx, tz) = (fx - cx as f32, fz - cz as f32);
        let row = self.w + 1;
        let at = |x: i32, z: i32| self.corners.get((z * row + x) as usize).copied().unwrap_or(0.0);
        let top = at(cx, cz) * (1.0 - tx) + at(cx + 1, cz) * tx;
        let bottom = at(cx, cz + 1) * (1.0 - tx) + at(cx + 1, cz + 1) * tx;
        top * (1.0 - tz) + bottom * tz
    }

    /// Where a unit at `(x, z)` stands: the ground, or the sea surface.
    pub fn stand(&self, x: f32, z: f32) -> f32 {
        self.ground(x, z).max(0.0)
    }

    /// Top of the buildings on the square at `(x, z)`; `NEG_INFINITY` if
    /// none.
    pub fn block_top_at(&self, x: f32, z: f32) -> f32 {
        self.block_top(x, z)
    }

    /// Top of the buildings on the square at `(x, z)`, if any.
    fn block_top(&self, x: f32, z: f32) -> f32 {
        if x < 0.0 || z < 0.0 {
            return f32::NEG_INFINITY;
        }
        let (sx, sz) = ((x / SQUARE) as i32, (z / SQUARE) as i32);
        if sx >= self.w || sz >= self.h {
            return f32::NEG_INFINITY;
        }
        self.tops[(sz * self.w + sx) as usize]
    }

    /// Whether a shot along the line from `from` to `to` (absolute heights)
    /// is free: no terrain in the way short of `aoe` elmos before `to`, and
    /// no building in the way but within `skip_from` of `from` (the
    /// shooter's own) or `skip_to` of `to` (the target's).
    pub fn line_is_free(&self, from: [f32; 3], to: [f32; 3], aoe: f32, skip_from: f32, skip_to: f32) -> bool {
        let d = ((to[0] - from[0]).powi(2) + (to[2] - from[2]).powi(2)).sqrt();
        let steps = (d / STEP).ceil() as i32;
        (1..steps).all(|k| {
            let t = k as f32 / steps as f32;
            let (x, z) = (from[0] + (to[0] - from[0]) * t, from[2] + (to[2] - from[2]) * t);
            let y = from[1] + (to[1] - from[1]) * t + GRAZE;
            let along = t * d;
            (d - along <= aoe || self.ground(x, z) <= y)
                && (along <= skip_from || d - along <= skip_to || self.block_top(x, z) <= y)
        })
    }
}

impl Shadow {
    /// Where `gun` can hit, out to its range plus `margin`.
    pub fn compute(ground: &FireGround, gun: &Gun, margin: f32) -> Shadow {
        let reach = gun.range + margin;
        let r = (reach / CELL).ceil() as i32 + 1;
        let (cx, cz) = ((gun.x / CELL) as i32, (gun.z / CELL) as i32);
        let n = 2 * r + 1;
        let mut shadow = Shadow { x0: cx - r, z0: cz - r, n, need: vec![f32::INFINITY; (n * n) as usize] };

        let steps = (reach / STEP).ceil() as usize;
        // Enough rays that neighbouring ones are half a cell apart at the rim.
        let rays = ((std::f32::consts::TAU * reach / (CELL / 2.0)).ceil() as usize).max(8);
        let aoe_steps = (gun.aoe / STEP).ceil() as usize;
        // Per sample along a ray: distance, terrain and building heights.
        let mut dist = vec![0.0f32; steps + 1];
        let mut terrain = vec![0.0f32; steps + 1];
        let mut blocks = vec![f32::NEG_INFINITY; steps + 1];
        // Steepest terrain and building slopes up to each sample.
        let mut terrain_max = vec![f32::NEG_INFINITY; steps + 1];
        let mut blocks_max = vec![f32::NEG_INFINITY; steps + 1];
        for ray in 0..rays {
            let a = ray as f32 / rays as f32 * std::f32::consts::TAU;
            let (dx, dz) = (a.cos(), a.sin());
            for k in 1..=steps {
                let along = k as f32 * STEP;
                let (x, z) = (gun.x + dx * along, gun.z + dz * along);
                dist[k] = along;
                terrain[k] = if gun.through_ground { f32::NEG_INFINITY } else { ground.ground(x, z) };
                blocks[k] =
                    if gun.avoids_friends && along > gun.body { ground.block_top(x, z) } else { f32::NEG_INFINITY };
                terrain_max[k] = terrain_max[k - 1].max((terrain[k] - gun.muzzle) / along);
                blocks_max[k] = blocks_max[k - 1].max((blocks[k] - gun.muzzle) / along);
            }
            for k in 1..=steps {
                let along = dist[k];
                let (x, z) = (gun.x + dx * along, gun.z + dz * along);
                let stand = ground.stand(x, z);
                let need = match gun.flight {
                    Flight::Lob => f32::NEG_INFINITY,
                    Flight::Line => {
                        // The steepest thing in the way: buildings anywhere
                        // before the target, terrain only short of the area
                        // of effect.
                        let mut horizon = blocks_max[k - 1];
                        if k > aoe_steps + 1 {
                            horizon = horizon.max(terrain_max[k - 1 - aoe_steps]);
                        }
                        gun.muzzle + horizon * along - stand
                    }
                    Flight::Arc { speed, gravity } => {
                        // The arc rises k(d - x)x over the chord: the target
                        // must be high enough that the arc clears every
                        // sample in the way, terrain only short of the last
                        // sixteenth of the way (`CCannon::HaveFreeLineOfFire`).
                        let v2 = (speed * speed).max(1.0);
                        let rise = (stand - gun.muzzle) / along;
                        let tan = rise + gravity * along / (2.0 * v2);
                        let bow = gravity * (1.0 + tan * tan) / (2.0 * v2);
                        let mut slope = f32::NEG_INFINITY;
                        for j in 1..k {
                            let x = dist[j];
                            let lift = bow * (along - x);
                            slope = slope.max((blocks[j] - gun.muzzle) / x - lift);
                            if x < 0.9375 * along {
                                slope = slope.max((terrain[j] - gun.muzzle) / x - lift);
                            }
                        }
                        gun.muzzle + slope * along - stand
                    }
                };
                shadow.lower(x, z, need);
            }
        }
        // The gun's own cell.
        shadow.lower(gun.x, gun.z, f32::NEG_INFINITY);
        shadow
    }

    fn index(&self, x: f32, z: f32) -> Option<usize> {
        let (cx, cz) = ((x / CELL).floor() as i32 - self.x0, (z / CELL).floor() as i32 - self.z0);
        (cx >= 0 && cz >= 0 && cx < self.n && cz < self.n).then(|| (cz * self.n + cx) as usize)
    }

    fn lower(&mut self, x: f32, z: f32, need: f32) {
        if let Some(i) = self.index(x, z) {
            self.need[i] = self.need[i].min(need);
        }
    }

    /// The least height above the ground at `(x, z)` that a target's aim
    /// point needs for the gun to hit it; `INFINITY` where it cannot (out of
    /// the patch, or shadowed at any height).
    pub fn need_at(&self, x: f32, z: f32) -> f32 {
        self.index(x, z).map_or(f32::INFINITY, |i| self.need[i])
    }

    /// Cells in the patch, and those among them where a target with its aim
    /// point `aim` above the ground is shadowed though in reach.
    pub fn count(&self, ground: &FireGround, gun: &Gun, aim: f32) -> (usize, usize) {
        let (mut reach, mut shadowed) = (0, 0);
        for cz in 0..self.n {
            for cx in 0..self.n {
                let (x, z) = (((self.x0 + cx) as f32 + 0.5) * CELL, ((self.z0 + cz) as f32 + 0.5) * CELL);
                let stand = ground.stand(x, z);
                if !gun.reaches(x, z, stand + aim, 0.0) {
                    continue;
                }
                reach += 1;
                if self.need[(cz * self.n + cx) as usize] > aim {
                    shadowed += 1;
                }
            }
        }
        (reach, shadowed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A flat map of `n` x `n` squares at height 0, with a ridge of `ridge`
    /// elmos across x = 400..432.
    fn ridged(n: i32, ridge: f32) -> FireGround {
        let row = n + 1;
        let corners = (0..row * row)
            .map(|i| {
                let x = (i % row) as f32 * SQUARE;
                if (400.0..=432.0).contains(&x) { ridge } else { 0.0 }
            })
            .collect();
        FireGround::new(corners, n, n)
    }

    fn lotus(x: f32, z: f32) -> Gun {
        Gun {
            x,
            z,
            muzzle: 30.0,
            range: 460.0,
            height_mod: 0.2,
            aoe: 4.0,
            body: 16.0,
            flight: Flight::Line,
            avoids_friends: true,
            through_ground: false,
        }
    }

    #[test]
    fn terrain_is_interpolated_between_corners() {
        let g = FireGround::new(vec![0.0, 8.0, 16.0, 24.0], 1, 1);
        assert_eq!(g.ground(0.0, 0.0), 0.0);
        assert_eq!(g.ground(4.0, 0.0), 4.0);
        assert_eq!(g.ground(4.0, 4.0), 12.0);
        assert_eq!(g.ground(100.0, 100.0), 24.0);
    }

    #[test]
    fn a_ridge_shadows_the_ground_behind_it() {
        // Lower than the muzzle, so the shadow thins out behind it.
        let g = ridged(128, 20.0);
        let gun = lotus(200.0, 500.0);
        let s = Shadow::compute(&g, &gun, 0.0);
        // In the open before the ridge: hit at ground level.
        assert!(s.need_at(320.0, 500.0) <= 0.0);
        // Right behind the ridge: a raider-sized target is hidden...
        let behind = s.need_at(460.0, 500.0);
        assert!(behind > 14.0, "{behind}");
        // ...but less so farther back, as the shadow thins out.
        assert!(s.need_at(640.0, 500.0) < behind);
        // Out of the patch: never.
        assert_eq!(s.need_at(2000.0, 500.0), f32::INFINITY);
    }

    #[test]
    fn splash_reaches_just_over_a_crest() {
        let g = ridged(128, 40.0);
        let narrow = Shadow::compute(&g, &lotus(200.0, 500.0), 0.0);
        let wide = Shadow::compute(&g, &Gun { aoe: 96.0, ..lotus(200.0, 500.0) }, 0.0);
        // Just past the crest, a splashing gun fires anyway.
        assert!(wide.need_at(448.0, 500.0) < narrow.need_at(448.0, 500.0));
        assert!(wide.need_at(448.0, 500.0) <= 0.0);
    }

    #[test]
    fn own_buildings_shadow_unless_fired_through() {
        let mut g = ridged(128, 0.0);
        g.set_volumes([Volume {
            x: 300.0,
            y: 50.0 / 2.0,
            z: 500.0,
            hx: 24.0,
            hy: 50.0 / 2.0,
            hz: 24.0,
            shape: Shape::Box,
        }]);
        let gun = lotus(200.0, 500.0);
        let s = Shadow::compute(&g, &gun, 0.0);
        assert!(s.need_at(360.0, 500.0) > 20.0);
        // Off to the side of the block: in the open.
        assert!(s.need_at(360.0, 600.0) <= 0.0);
        // A gun that fires through friends (Newton) is not blocked.
        let newton = Shadow::compute(&g, &Gun { avoids_friends: false, ..gun }, 0.0);
        assert!(newton.need_at(360.0, 500.0) <= 0.0);
        // Its own body does not count.
        g.set_volumes([Volume {
            x: 200.0,
            y: 60.0 / 2.0,
            z: 500.0,
            hx: 12.0,
            hy: 60.0 / 2.0,
            hz: 12.0,
            shape: Shape::Box,
        }]);
        assert!(Shadow::compute(&g, &gun, 0.0).need_at(360.0, 500.0) <= 0.0);
    }

    #[test]
    fn arcs_clear_what_lines_do_not() {
        // A low wall halfway: a slow shell's arc goes over what a straight
        // line from the same muzzle hits.
        let mut g = ridged(128, 0.0);
        g.set_volumes([Volume {
            x: 400.0,
            y: 30.0 / 2.0,
            z: 500.0,
            hx: 8.0,
            hy: 30.0 / 2.0,
            hz: 40.0,
            shape: Shape::Box,
        }]);
        let line = lotus(200.0, 500.0);
        let arc = Gun { flight: Flight::Arc { speed: 300.0, gravity: 120.0 }, ..line };
        let lob = Gun { flight: Flight::Lob, ..line };
        let (l, a) = (Shadow::compute(&g, &line, 0.0), Shadow::compute(&g, &arc, 0.0));
        assert!(l.need_at(600.0, 500.0) > 15.0);
        assert!(a.need_at(600.0, 500.0) < l.need_at(600.0, 500.0));
        assert!(Shadow::compute(&g, &lob, 0.0).need_at(600.0, 500.0) <= 0.0);
    }

    #[test]
    fn volumes_stand_by_their_top_surface() {
        let dome = Volume { x: 100.0, y: 10.0, z: 100.0, hx: 20.0, hy: 30.0, hz: 20.0, shape: Shape::Ellipsoid };
        assert_eq!(dome.top_at(100.0, 100.0), 40.0);
        assert!(dome.top_at(116.0, 100.0) < 30.0);
        assert_eq!(dome.top_at(121.0, 100.0), f32::NEG_INFINITY);
        let pole = Volume { shape: Shape::CylinderY, ..dome };
        assert_eq!(pole.top_at(116.0, 100.0), 40.0);
        assert_eq!(pole.top_at(116.0, 116.0), f32::NEG_INFINITY);
        // A Lotus behind a mex-sized dome sees over its shoulder, not its
        // crown.
        let mut g = ridged(128, 0.0);
        g.set_volumes([Volume { x: 300.0, y: 21.0, z: 500.0, hx: 20.0, hy: 29.0, hz: 20.0, shape: Shape::Ellipsoid }]);
        let s = Shadow::compute(&g, &lotus(200.0, 500.0), 0.0);
        assert!(s.need_at(360.0, 500.0) > s.need_at(360.0, 516.0));
    }

    #[test]
    fn reach_shrinks_with_height_difference() {
        let gun = Gun { height_mod: 1.0, ..lotus(0.0, 0.0) };
        assert!(gun.reaches(450.0, 0.0, 30.0, 0.0));
        assert!(!gun.reaches(450.0, 0.0, 230.0, 0.0));
        assert!(gun.reaches(400.0, 0.0, 230.0, 0.0));
    }

    #[test]
    fn our_line_skips_the_target_itself() {
        let mut g = ridged(128, 0.0);
        g.set_volumes([Volume {
            x: 500.0,
            y: 50.0 / 2.0,
            z: 500.0,
            hx: 20.0,
            hy: 50.0 / 2.0,
            hz: 20.0,
            shape: Shape::Box,
        }]);
        let from = [300.0, 15.0, 500.0];
        let to = [500.0, 25.0, 500.0];
        assert!(g.line_is_free(from, to, 0.0, 0.0, 28.0));
        assert!(!g.line_is_free(from, to, 0.0, 0.0, 0.0));
        // Another building in the way blocks.
        g.set_volumes([
            Volume { x: 500.0, y: 50.0 / 2.0, z: 500.0, hx: 20.0, hy: 50.0 / 2.0, hz: 20.0, shape: Shape::Box },
            Volume { x: 400.0, y: 50.0 / 2.0, z: 500.0, hx: 20.0, hy: 50.0 / 2.0, hz: 20.0, shape: Shape::Box },
        ]);
        assert!(!g.line_is_free(from, to, 0.0, 0.0, 28.0));
        // A ridge blocks; within the area of effect it does not.
        let g = ridged(128, 60.0);
        assert!(!g.line_is_free([300.0, 15.0, 500.0], [500.0, 15.0, 500.0], 0.0, 0.0, 0.0));
        assert!(g.line_is_free([300.0, 15.0, 500.0], [440.0, 15.0, 500.0], 48.0, 0.0, 0.0));
    }
}
