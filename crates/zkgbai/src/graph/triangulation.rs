//! Delaunay edges over metal spot positions (the original used poly2tri).

use std::collections::BTreeSet;

use delaunator::{Point, triangulate};

/// Edges of the Delaunay triangulation of `points` (ground-plane `(x, z)`),
/// as index pairs `(a, b)` with `a < b`, sorted and deduplicated.
///
/// Degenerate inputs for which no triangle exists (two points, or all points
/// collinear) yield the path through the points in order along their dominant
/// axis, which is their Delaunay graph. (poly2tri, as used by the original,
/// produced no links at all there.)
pub(super) fn delaunay_edges(points: &[(f64, f64)]) -> BTreeSet<(usize, usize)> {
    let mut edges = BTreeSet::new();
    let mut add = |a: usize, b: usize| {
        if a != b {
            edges.insert((a.min(b), a.max(b)));
        }
    };

    let pts: Vec<Point> = points.iter().map(|&(x, y)| Point { x, y }).collect();
    let triangulation = triangulate(&pts);
    for &[a, b, c] in triangulation.triangles.as_chunks::<3>().0 {
        add(a, b);
        add(a, c);
        add(b, c);
    }

    if triangulation.triangles.is_empty() && points.len() >= 2 {
        let (mut min_x, mut max_x, mut min_y, mut max_y) = (f64::MAX, f64::MIN, f64::MAX, f64::MIN);
        for &(x, y) in points {
            min_x = min_x.min(x);
            max_x = max_x.max(x);
            min_y = min_y.min(y);
            max_y = max_y.max(y);
        }
        let mut order: Vec<usize> = (0..points.len()).collect();
        if max_x - min_x >= max_y - min_y {
            order.sort_by(|&a, &b| points[a].0.total_cmp(&points[b].0).then(points[a].1.total_cmp(&points[b].1)));
        } else {
            order.sort_by(|&a, &b| points[a].1.total_cmp(&points[b].1).then(points[a].0.total_cmp(&points[b].0)));
        }
        for w in order.windows(2) {
            add(w[0], w[1]);
        }
    }
    edges
}
