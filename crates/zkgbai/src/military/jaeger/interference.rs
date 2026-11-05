//! Coarse, jaeger-only crossing estimates. Interception time and observation
//! uncertainty remain separate: no contacts is not evidence of empty ground.
//! Pure geometry deliberately ignores terrain shortcuts; callers must check
//! the actual approach for passability and retain local combat checks.

pub(super) const CELL: f32 = 128.0;
pub(super) const MAX_WALK: f32 = 8.0;
const HORIZON: f32 = 15.0;
const CELL_RADIUS: f32 = CELL * std::f32::consts::FRAC_1_SQRT_2;

type Point = [f32; 2];

#[derive(Clone, Copy)]
pub(super) struct Contact {
    pub id: i32,
    pub pos: Point,
    pub speed: f32,
    pub age: f32,
    /// Physical radius, or weapon reach for a static defense.
    pub radius: f32,
}

#[derive(Clone, Copy, Default)]
pub(super) struct Observation {
    pub los: bool,
    pub radar: bool,
    pub age: f32,
}

impl Observation {
    fn uncertainty(self) -> f32 {
        if self.los {
            return 0.0;
        }
        // Recently inspected ground helps briefly; radar is useful but
        // cannot establish the absence of cloaked units.
        let stale = (self.age / 5.0).clamp(0.0, 1.0);
        if self.radar { 0.35 + 0.15 * stale } else { stale }
    }
}

#[derive(Clone, Copy)]
struct Arrival {
    id: i32,
    seconds: f32,
}

const NEVER: Arrival = Arrival { id: -1, seconds: f32::INFINITY };

#[derive(Clone)]
struct Cell {
    // Keep the runner-up so excluding the hunted heavy cannot erase its
    // escort. Separate profiles preserve each hunter's decloak distance.
    arrivals: [[Arrival; 2]; 3],
    uncertainty: f32,
}

#[derive(Default)]
pub(super) struct Map {
    width: usize,
    height: usize,
    cells: Vec<Cell>,
    pub frame: Option<i32>,
}

#[derive(Clone, Copy, Debug)]
pub(super) struct Assessment {
    /// Smallest interception-time surplus anywhere along the crossing.
    pub margin: f32,
    /// Seconds exposed, weighted by local observation uncertainty.
    pub uncertain_seconds: f32,
    /// Longest uninterrupted stretch through poorly observed ground.
    pub blind_seconds: f32,
}

#[derive(Clone, Copy)]
pub(super) struct Crossing {
    pub from: Point,
    pub to: Point,
    pub speed: f32,
    pub hold: f32,
    pub profile: usize,
    pub target: i32,
}

impl Assessment {
    pub fn clear_of_screens(self, continuing: bool) -> bool {
        self.margin >= if continuing { 0.5 } else { 1.0 }
    }

    pub fn allows(self, continuing: bool) -> bool {
        let (uncertain, blind) = if continuing { (2.0, 1.25) } else { (1.5, 1.0) };
        self.clear_of_screens(continuing) && self.uncertain_seconds <= uncertain && self.blind_seconds <= blind
    }
}

fn distance(a: Point, b: Point) -> f32 {
    (a[0] - b[0]).hypot(a[1] - b[1])
}

impl Map {
    pub fn centres(width: f32, height: f32) -> Vec<Point> {
        let w = (width / CELL).ceil() as usize;
        let h = (height / CELL).ceil() as usize;
        (0..w * h).map(|i| [((i % w) as f32 + 0.5) * CELL, ((i / w) as f32 + 0.5) * CELL]).collect()
    }

    pub fn rebuild(
        &mut self,
        size: Point,
        frame: i32,
        decloak: [f32; 3],
        contacts: &[Contact],
        observations: &[Observation],
    ) {
        self.width = (size[0] / CELL).ceil() as usize;
        self.height = (size[1] / CELL).ceil() as usize;
        self.frame = Some(frame);
        self.cells = vec![Cell { arrivals: [[NEVER; 2]; 3], uncertainty: 1.0 }; self.width * self.height];
        if self.cells.is_empty() {
            return;
        }
        for z in 0..self.height {
            for x in 0..self.width {
                let i = z * self.width + x;
                let local = observations.get(i).copied().map_or(1.0, Observation::uncertainty);
                let (mut sum, mut count) = (0.0, 0.0);
                // Fog beside the corridor matters too: a screen may enter
                // from there even when the heavy itself is visible.
                for nz in z.saturating_sub(1)..=(z + 1).min(self.height - 1) {
                    for nx in x.saturating_sub(1)..=(x + 1).min(self.width - 1) {
                        sum += observations.get(nz * self.width + nx).copied().map_or(1.0, Observation::uncertainty);
                        count += 1.0;
                    }
                }
                self.cells[i].uncertainty = 0.5 * local + 0.5 * sum / count;
            }
        }
        let widest = decloak.into_iter().fold(0.0, f32::max);
        for c in contacts {
            let reach = widest + c.radius + CELL_RADIUS + c.speed * (HORIZON + c.age);
            let low = |v: f32| ((v - reach) / CELL).floor().max(0.0) as usize;
            let high = |v: f32, n: usize| (((v + reach) / CELL).floor().max(0.0) as usize).min(n - 1);
            for z in low(c.pos[1])..=high(c.pos[1], self.height) {
                for x in low(c.pos[0])..=high(c.pos[0], self.width) {
                    let p = [(x as f32 + 0.5) * CELL, (z as f32 + 0.5) * CELL];
                    let d = distance(p, c.pos) - c.radius - CELL_RADIUS;
                    for (profile, radius) in decloak.into_iter().enumerate() {
                        let gap = d - radius;
                        let seconds = if gap <= 0.0 {
                            0.0
                        } else if c.speed > 0.0 {
                            (gap / c.speed - c.age).max(0.0)
                        } else {
                            f32::INFINITY
                        };
                        if seconds > HORIZON {
                            continue;
                        }
                        let pair = &mut self.cells[z * self.width + x].arrivals[profile];
                        let arrival = Arrival { id: c.id, seconds };
                        if seconds < pair[0].seconds {
                            pair[1] = pair[0];
                            pair[0] = arrival;
                        } else if seconds < pair[1].seconds {
                            pair[1] = arrival;
                        }
                    }
                }
            }
        }
    }

    /// Sample the whole crossing, including its final attack preparation.
    /// A bad cell cannot disappear into an average of the rest of the route.
    pub fn assess(&self, crossing: Crossing, frame: i32) -> Option<Assessment> {
        let Crossing { from, to, speed, hold, profile, target } = crossing;
        let age = (frame - self.frame?) as f32 / 30.0;
        if !(0.0..=1.5).contains(&age) || speed <= 0.0 || profile >= 3 {
            return None;
        }
        let d = distance(from, to);
        let walk = d / speed;
        if walk > MAX_WALK || hold < 0.0 || walk + hold + 1.5 > HORIZON {
            return None;
        }
        let steps = (d / (CELL * 0.25)).ceil().max(1.0) as usize;
        let mut report = Assessment { margin: f32::INFINITY, uncertain_seconds: 0.0, blind_seconds: 0.0 };
        let mut blind_run: f32 = 0.0;
        for i in 0..=steps {
            let k = i as f32 / steps as f32;
            let p = [from[0] + (to[0] - from[0]) * k, from[1] + (to[1] - from[1]) * k];
            if p[0] < 0.0 || p[1] < 0.0 {
                return None;
            }
            let (x, z) = ((p[0] / CELL) as usize, (p[1] / CELL) as usize);
            if x >= self.width || z >= self.height {
                return None;
            }
            let cell = &self.cells[z * self.width + x];
            let eta = walk * k + if i == steps { hold } else { 0.0 };
            let arrival = cell.arrivals[profile].iter().find(|a| a.id != target).map_or(f32::INFINITY, |a| a.seconds);
            report.margin = report.margin.min(arrival - age - eta);
            let dt = if i == steps { hold } else { walk / steps as f32 };
            report.uncertain_seconds += dt * cell.uncertainty;
            blind_run = if cell.uncertainty >= 0.75 { blind_run + dt } else { 0.0 };
            report.blind_seconds = report.blind_seconds.max(blind_run);
        }
        Some(report)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map(contacts: &[Contact], observation: Observation) -> Map {
        let mut map = Map::default();
        let observations = vec![observation; Map::centres(2048.0, 2048.0).len()];
        map.rebuild([2048.0; 2], 30, [100.0, 220.0, 190.0], contacts, &observations);
        map
    }

    fn crossing(map: &Map, profile: usize, target: i32) -> Assessment {
        map.assess(Crossing { from: [512.0, 1024.0], to: [768.0, 1024.0], speed: 64.0, hold: 2.0, profile, target }, 30)
            .unwrap()
    }

    fn visible() -> Observation {
        Observation { los: true, ..Default::default() }
    }

    #[test]
    fn exposed_heavy_is_not_its_own_screen_but_its_escort_is() {
        let heavy = Contact { id: 1, pos: [1024.0, 1024.0], speed: 50.0, age: 0.0, radius: 30.0 };
        assert!(crossing(&map(&[heavy], visible()), 1, 1).allows(false));
        let glaive = Contact { id: 2, pos: [1024.0, 1152.0], speed: 115.0, age: 0.0, radius: 12.0 };
        assert!(!crossing(&map(&[heavy, glaive], visible()), 1, 1).allows(false));
        // The exclusion also works when the target is the runner-up.
        assert!(!crossing(&map(&[glaive, heavy], visible()), 1, 1).allows(false));
    }

    #[test]
    fn watched_empty_ground_and_empty_fog_are_different() {
        assert!(crossing(&map(&[], visible()), 1, 1).allows(false));
        let fog = map(&[], Observation { age: 60.0, ..Default::default() });
        assert!(!crossing(&fog, 1, 1).allows(false));
        let radar = map(&[], Observation { radar: true, age: 60.0, ..Default::default() });
        assert!(crossing(&radar, 1, 1).uncertain_seconds < crossing(&fog, 1, 1).uncertain_seconds);
        assert!(crossing(&radar, 1, 1).uncertain_seconds > 0.0);
    }

    #[test]
    fn a_fast_patrol_can_intercept_where_a_slow_one_cannot() {
        let patrol = Contact { id: 2, pos: [768.0, 1900.0], speed: 25.0, age: 0.0, radius: 10.0 };
        assert!(crossing(&map(&[patrol], visible()), 1, 1).allows(false));
        assert!(!crossing(&map(&[Contact { speed: 115.0, ..patrol }], visible()), 1, 1).allows(false));
        assert!(!crossing(&map(&[Contact { age: 20.0, ..patrol }], visible()), 1, 1).allows(false));
    }

    #[test]
    fn a_screen_in_the_middle_cannot_be_averaged_away() {
        let block = Contact { id: 2, pos: [1024.0, 1024.0], speed: 0.0, age: 0.0, radius: 10.0 };
        let map = map(&[block], visible());
        let route = map
            .assess(
                Crossing {
                    from: [512.0, 1024.0],
                    to: [1536.0, 1024.0],
                    speed: 160.0,
                    hold: 0.0,
                    profile: 0,
                    target: 1,
                },
                30,
            )
            .unwrap();
        assert!(!route.allows(false));
        assert!(route.margin < 0.0);
    }

    #[test]
    fn profiles_keep_widows_smaller_decloak_distance() {
        let block = Contact { id: 2, pos: [960.0, 1264.0], speed: 0.0, age: 0.0, radius: 0.0 };
        let map = map(&[block], visible());
        let check = |profile| {
            map.assess(
                Crossing { from: [960.0, 960.0], to: [960.0, 960.0], speed: 40.0, hold: 2.0, profile, target: 1 },
                30,
            )
            .unwrap()
        };
        assert!(check(0).allows(false));
        assert!(!check(1).allows(false));
    }

    #[test]
    fn fog_beside_a_visible_corridor_still_counts() {
        let mut map = Map::default();
        let mut observations = vec![Observation { age: 60.0, ..Default::default() }; 16 * 16];
        for x in 0..16 {
            observations[8 * 16 + x] = visible();
        }
        map.rebuild([2048.0; 2], 30, [100.0, 220.0, 190.0], &[], &observations);
        assert!(crossing(&map, 1, 1).uncertain_seconds > 0.0);
    }

    #[test]
    fn long_walks_stale_maps_and_off_map_routes_cannot_authorize_a_sortie() {
        let map = map(&[], visible());
        for (to, frame) in [([1536.0, 1024.0], 30), ([768.0, 1024.0], 90), ([-1.0, 1024.0], 30)] {
            assert!(
                map.assess(
                    Crossing { from: [512.0, 1024.0], to, speed: 64.0, hold: 2.0, profile: 1, target: 1 },
                    frame
                )
                .is_none()
            );
        }
        assert!(
            Map::default()
                .assess(Crossing { from: [0.0; 2], to: [0.0; 2], speed: 40.0, hold: 2.0, profile: 1, target: 1 }, 30)
                .is_none()
        );
    }

    #[test]
    fn continuation_tolerates_small_changes_but_never_an_intercept() {
        let borderline = Assessment { margin: 0.8, uncertain_seconds: 1.8, blind_seconds: 0.0 };
        assert!(!borderline.allows(false));
        assert!(borderline.allows(true));
        assert!(!Assessment { margin: -0.1, ..borderline }.allows(true));
    }

    #[test]
    fn a_new_screen_revokes_an_existing_clear_crossing() {
        let mut map = map(&[], visible());
        assert!(crossing(&map, 1, 1).allows(false));
        let route =
            Crossing { from: [512.0, 1024.0], to: [768.0, 1024.0], speed: 64.0, hold: 2.0, profile: 1, target: 1 };
        let contact = Contact { id: 2, pos: [1100.0, 1100.0], speed: 115.0, age: 0.0, radius: 12.0 };
        map.rebuild([2048.0; 2], 60, [100.0, 220.0, 190.0], &[contact], &vec![visible(); 16 * 16]);
        assert!(!map.assess(route, 60).unwrap().allows(true));
        // Removal and fresh observation restore the opening.
        map.rebuild([2048.0; 2], 90, [100.0, 220.0, 190.0], &[], &vec![visible(); 16 * 16]);
        assert!(map.assess(route, 90).unwrap().allows(false));
    }

    #[test]
    fn uncertainty_shortens_the_crossing_instead_of_banning_every_blind_step() {
        let map = map(&[], Observation { radar: true, age: 60.0, ..Default::default() });
        let short =
            Crossing { from: [512.0, 1024.0], to: [544.0, 1024.0], speed: 64.0, hold: 2.0, profile: 1, target: 1 };
        assert!(map.assess(short, 30).unwrap().allows(false));
        assert!(!map.assess(Crossing { to: [768.0, 1024.0], ..short }, 30).unwrap().allows(false));
    }
}
