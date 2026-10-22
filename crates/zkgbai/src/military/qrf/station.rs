//! The baseline QRF algorithm, `station`: fire stations. Each team waits
//! at a station (the teams' stations are a value-weighted k-means of our
//! assets), and each raid near our assets gets the nearest team not yet
//! sent, which goes at the raid (a little ahead of it) fighting. Teams with
//! no raid go back to their stations. It chases: the naive way.

use std::collections::BTreeSet;

use springai::Vec3;

use crate::Ai;
use crate::util::distance;

/// Frames between re-placing the stations.
const RESITE: i32 = 30 * 30;

/// Raids within this far of an asset are answered.
const NEAR: f32 = 2000.0;

/// Seconds of a raid's travel a team aims ahead of it.
const LEAD: f32 = 2.0;

/// A team this close to its station stays put.
const AT_STATION: f32 = 200.0;

#[derive(Default)]
pub struct Station {
    pub stations: Vec<Vec3>,
    sited: i32,
}

/// Value-weighted k-means of points (Lloyd's, from the heaviest points).
pub fn k_means(points: &[(Vec3, f32)], k: usize) -> Vec<Vec3> {
    if points.is_empty() || k == 0 {
        return Vec::new();
    }
    let mut sorted: Vec<&(Vec3, f32)> = points.iter().collect();
    sorted.sort_by(|a, b| b.1.total_cmp(&a.1));
    // Spread the first centres: farthest-first from the heaviest.
    let mut centres = vec![sorted[0].0];
    while centres.len() < k.min(points.len()) {
        let far = (points.iter())
            .max_by(|a, b| {
                let da = centres.iter().map(|c| distance(*c, a.0)).fold(f32::INFINITY, f32::min) * a.1;
                let db = centres.iter().map(|c| distance(*c, b.0)).fold(f32::INFINITY, f32::min) * b.1;
                da.total_cmp(&db)
            })
            .map(|p| p.0);
        match far {
            Some(p) => centres.push(p),
            None => break,
        }
    }
    for _ in 0..20 {
        let mut sum = vec![(Vec3::ZERO, 0.0f32); centres.len()];
        for &(p, w) in points {
            let i = (0..centres.len())
                .min_by(|&a, &b| distance(centres[a], p).total_cmp(&distance(centres[b], p)))
                .unwrap_or(0);
            sum[i].0 += p * w;
            sum[i].1 += w;
        }
        for (c, (s, w)) in centres.iter_mut().zip(sum) {
            if w > 0.0 {
                *c = s * (1.0 / w);
            }
        }
    }
    centres
}

impl Ai {
    pub(super) fn qrf_station_update(&mut self, frame: i32) {
        let teams = self.qrf_teams();
        let k = teams.len();
        if self.qrf.station.stations.len() != k || frame - self.qrf.station.sited > RESITE {
            let points: Vec<(Vec3, f32)> = self.qrf.assets.iter().map(|a| (a.pos, a.cost)).collect();
            self.qrf.station.stations = k_means(&points, k);
            self.qrf.station.sited = frame;
        }
        // Raids near our assets, nearest first.
        let mut raids: Vec<(f32, usize)> = (self.qrf.groups.iter().enumerate())
            .filter_map(|(i, g)| {
                let d = self.qrf.assets.iter().map(|a| distance(a.pos, g.center)).fold(f32::INFINITY, f32::min);
                (d < NEAR).then_some((d, i))
            })
            .collect();
        raids.sort_by(|a, b| a.0.total_cmp(&b.0));
        let mut free: BTreeSet<usize> = teams.keys().copied().collect();
        for (_, gi) in raids {
            let g = &self.qrf.groups[gi];
            let aim = g.center + g.vel * LEAD;
            let Some(&t) = free.iter().min_by(|&&a, &&b| {
                let pa = self.qrf_team_pos(&teams[&a]).0;
                let pb = self.qrf_team_pos(&teams[&b]).0;
                distance(pa, aim).total_cmp(&distance(pb, aim))
            }) else {
                break;
            };
            free.remove(&t);
            self.qrf_order_team(&teams[&t], aim, true);
        }
        // The rest go home.
        let stations = self.qrf.station.stations.clone();
        for (i, (&t, ids)) in teams.iter().enumerate() {
            if !free.contains(&t) {
                continue;
            }
            let Some(&s) = stations.get(i) else { continue };
            if distance(self.qrf_team_pos(ids).0, s) > AT_STATION {
                self.qrf_order_team(ids, s, true);
            }
        }
    }
}
