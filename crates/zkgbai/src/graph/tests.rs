//! Engine-free tests of the graph: triangulation, territory inference and
//! the pylon grid.

use std::collections::BTreeSet;

use springai::{Callback, Unit, Vec3};

use super::triangulation::delaunay_edges;
use super::{GraphManager, SpotData, lua_pos, parse_metal_spots_json};

/// A callback whose function table is empty: any engine call panics
/// ("engine callback ... is missing") instead of touching memory, which
/// proves the tested code is engine-free.
fn fake_cb() -> Callback {
    let table: &'static springai::ffi::SSkirmishAICallback = Box::leak(Box::default());
    // SAFETY: the table lives forever and holds no function pointers.
    unsafe { Callback::from_raw(0, table) }
}

/// A 512x512 (heightmap squares) map with spots at the given `(x, z)`.
fn graph(spots: &[(f32, f32)]) -> GraphManager {
    let mut g = GraphManager::with_map_size(fake_cb(), 512, 512);
    let data: Vec<SpotData> = spots.iter().map(|&(x, z)| SpotData { x, y: 0.0, z, metal: 2.0 }).collect();
    g.build_graph(&data, &mut |_| {});
    g
}

/// Links as sorted spot-index pairs.
fn link_set(g: &GraphManager) -> BTreeSet<(usize, usize)> {
    g.links.iter().map(|l| (l.v0.min(l.v1), l.v0.max(l.v1))).collect()
}

#[test]
fn square_with_center_triangulates_into_spokes() {
    let pts = [(0.0, 0.0), (100.0, 0.0), (0.0, 100.0), (100.0, 100.0), (50.0, 50.0)];
    let edges = delaunay_edges(&pts);
    let expected: BTreeSet<_> = [(0, 1), (0, 2), (1, 3), (2, 3), (0, 4), (1, 4), (2, 4), (3, 4)].into();
    assert_eq!(edges, expected);
}

#[test]
fn degenerate_inputs_link_along_the_line() {
    assert!(delaunay_edges(&[]).is_empty());
    assert!(delaunay_edges(&[(5.0, 5.0)]).is_empty());
    assert_eq!(delaunay_edges(&[(0.0, 0.0), (10.0, 300.0)]), [(0, 1)].into());
    // collinear, given out of order
    let edges = delaunay_edges(&[(0.0, 0.0), (200.0, 20.0), (100.0, 10.0), (300.0, 30.0)]);
    assert_eq!(edges, [(0, 2), (1, 2), (1, 3)].into());
    // vertical line: z is the dominant axis
    let edges = delaunay_edges(&[(7.0, 300.0), (7.0, 0.0), (7.0, 100.0)]);
    assert_eq!(edges, [(1, 2), (0, 2)].into());
}

#[test]
fn grid_of_spots_is_fully_linked() {
    let mut spots = Vec::new();
    for i in 0..5 {
        for j in 0..4 {
            // slightly irregular, like real spot layouts
            spots.push((300.0 * i as f32 + (j * 7) as f32, 400.0 * j as f32 + (i * 13) as f32));
        }
    }
    let g = graph(&spots);
    assert_eq!(g.metal_spots.len(), 20);
    // planar triangulation: 3n - 3 - (hull size); the hull has the 14 border points
    assert_eq!(g.links.len(), 3 * 20 - 3 - 14);
    for (i, ms) in g.metal_spots.iter().enumerate() {
        assert_eq!(ms.index, i);
        assert!(ms.links.len() >= 2, "spot {i} has {} links", ms.links.len());
    }
    for (i, l) in g.links.iter().enumerate() {
        assert_eq!(l.index, i);
        assert!(g.metal_spots[l.v0].links.contains(&i) && g.metal_spots[l.v1].links.contains(&i));
        let (a, b) = (g.metal_spots[l.v0].position, g.metal_spots[l.v1].position);
        assert!((l.length - a.distance_2d(b)).abs() < 1e-3);
        assert_eq!(l.center_pos, Vec3::new((a.x + b.x) / 2.0, 0.0, (a.z + b.z) / 2.0));
        // edges are stored in (x, z) order of their endpoints, like the original
        assert!((a.x as i32, a.z as i32) < (b.x as i32, b.z as i32));
    }
}

#[test]
fn spots_sharing_an_integer_position_keep_only_the_last_one_linked() {
    let g = graph(&[(10.2, 10.7), (500.0, 0.0), (0.0, 500.0), (10.9, 10.1)]);
    assert_eq!(g.metal_spots.len(), 4);
    assert!(g.metal_spots[0].links.is_empty());
    assert_eq!(g.metal_spots[3].links.len(), 2);
    assert_eq!(link_set(&g), [(1, 2), (1, 3), (2, 3)].into());
}

#[test]
fn territory_inference_along_a_chain() {
    // A - B - C - D, 640 elmos apart
    let mut g = graph(&[(640.0, 640.0), (1280.0, 640.0), (1920.0, 640.0), (2560.0, 640.0)]);
    assert_eq!(link_set(&g), [(0, 1), (1, 2), (2, 3)].into());
    let pos = |g: &GraphManager, i: usize| g.metal_spots[i].position;

    g.set_owned(0);
    assert!(g.metal_spots[0].owned && g.metal_spots[0].touched && !g.metal_spots[0].ally_shadowed);
    assert!(g.metal_spots[1].ally_shadowed);
    assert!(!g.metal_spots[2].ally_shadowed);
    assert!(g.is_ally_territory(pos(&g, 0)));
    assert!(!g.is_ally_territory(pos(&g, 3)));
    assert!(g.is_link_owned(&g.links[0]) && !g.is_link_owned(&g.links[1]));
    assert!(g.is_link_ally_shadowed(&g.links[1]));

    g.set_hostile(2);
    assert!(g.metal_spots[2].hostile && !g.metal_spots[2].enemy_shadowed);
    assert!(g.metal_spots[1].enemy_shadowed && g.metal_spots[3].enemy_shadowed);
    assert!(g.is_enemy_territory(pos(&g, 2)));
    assert!(!g.is_enemy_territory(pos(&g, 0)));

    let idx = |v: Vec<&super::MetalSpot>| v.iter().map(|ms| ms.index).collect::<Vec<_>>();
    assert_eq!(idx(g.get_owned_spots()), [0]);
    assert_eq!(idx(g.get_enemy_spots()), [2]);
    assert_eq!(idx(g.get_ally_territory()), [0, 1]);
    assert_eq!(idx(g.get_enemy_territory()), [1, 2, 3]);
    assert_eq!(idx(g.get_neutral_spots()), [1, 3]);
    assert_eq!(idx(g.get_touched_spots()), [0, 2]);
    // B is contested; A borders it through a hostile (enemy-shadowed) link
    assert_eq!(idx(g.get_front_line_spots()), [0, 1]);
    assert_eq!(g.get_closest_front_line_spot(pos(&g, 3)).map(|ms| ms.index), Some(0));
    assert_eq!(g.get_closest_enemy_spot(Vec3::ZERO).map(|ms| ms.index), Some(2));
    assert_eq!(g.get_closest_neutral_spot(Vec3::ZERO).map(|ms| ms.index), Some(1));
    // B is enemy-shadowed now, so A-B is no longer safely ours
    assert!(!g.is_link_owned(&g.links[0]));
    assert!(g.is_link_hostile(&g.links[0]));
    assert!(!g.is_link_ally_shadowed(&g.links[1]));

    // ally territory reaches 25 cells (1600 elmos) around A: D is the closest spot beyond it
    assert_eq!(g.get_closest_battle_front(pos(&g, 0)), pos(&g, 3));

    // the mex at A dies in LOS: A becomes ally-shadowed, territory is unpainted
    g.set_neutral(0);
    assert!(!g.metal_spots[0].owned && g.metal_spots[0].ally_shadowed);
    assert!(!g.is_ally_territory(pos(&g, 0)));
    // B no longer has an owned neighbour, so its ally shadow goes away
    g.clean_shadows(1);
    assert!(!g.metal_spots[1].ally_shadowed && g.metal_spots[1].enemy_shadowed);

    // the enemy mex at C dies in LOS
    g.set_neutral(2);
    assert!(!g.metal_spots[2].hostile && g.metal_spots[2].enemy_shadowed);
    assert!(!g.is_enemy_territory(pos(&g, 2)));
    assert_eq!(g.get_closest_enemy_spot(Vec3::ZERO).map(|ms| ms.index), None);
}

#[test]
fn shadows_spread_along_ground_links_only() {
    // A - B - C - D, with a cliff between B and C
    let mut g = graph(&[(640.0, 640.0), (1280.0, 640.0), (1920.0, 640.0), (2560.0, 640.0)]);
    let cliff = g.links.iter().position(|l| (l.v0.min(l.v1), l.v0.max(l.v1)) == (1, 2)).unwrap();
    g.ground_links = (0..g.links.len()).map(|li| li != cliff).collect();

    g.set_hostile(2);
    assert!(g.metal_spots[3].enemy_shadowed && !g.metal_spots[1].enemy_shadowed);
    g.set_owned(1);
    assert!(g.metal_spots[0].ally_shadowed && !g.metal_spots[2].ally_shadowed);
    // C is hostile across the cliff only: B keeps no enemy shadow
    g.metal_spots[1].enemy_shadowed = true;
    g.clean_shadows(1);
    assert!(!g.metal_spots[1].enemy_shadowed);
}

#[test]
fn closest_queries_on_an_empty_graph() {
    let g = graph(&[]);
    assert!(g.get_closest_spot(Vec3::ZERO).is_none());
    assert!(g.get_closest_front_line_spot(Vec3::ZERO).is_none());
    assert_eq!(g.get_closest_pylon_spot(Vec3::ZERO), Vec3::ZERO);
    assert_eq!(g.get_closest_front_line_link(Vec3::ZERO), Vec3::ZERO);
    // no spots and no enemy center: fall back to the map center
    assert_eq!(g.get_closest_battle_front(Vec3::ZERO), g.get_map_center());
    assert_eq!(g.get_map_center(), Vec3::new(2048.0, 0.0, 2048.0));
}

#[test]
fn pylon_chain_connects_a_link() {
    let cb = fake_cb();
    let unit = |id| Unit::from_id(cb, id).unwrap();
    // two spots 1000 apart; one link
    let mut g = graph(&[(1000.0, 1000.0), (2000.0, 1000.0)]);
    assert_eq!(g.links.len(), 1);
    let at = |x: f32| Vec3::xz(x, 1000.0);

    g.add_pylon(unit(1), 150, at(1100.0)); // covers spot 0
    g.add_pylon(unit(2), 150, at(1900.0)); // covers spot 1
    assert_eq!(g.metal_spots[0].pylons, [1]);
    assert_eq!(g.metal_spots[1].pylons, [2]);
    assert!(g.pylons[&1].neighbours.is_empty());
    assert!(!g.links[0].connected);
    assert!(!g.is_spot_connected(0));

    // extend from the end closest to the asking position
    assert_eq!(g.get_link_connection_head(0, at(900.0)), Some(1));
    assert_eq!(g.get_link_connection_head(0, at(2100.0)), Some(2));

    g.add_pylon(unit(3), 300, at(1500.0)); // bridges the gap
    assert_eq!(g.pylons[&3].neighbours, [1, 2]);
    assert_eq!(g.pylons[&1].neighbours, [3]);
    assert!(g.links[0].connected);
    assert!(g.is_spot_connected(0) && g.is_spot_connected(1));
    assert_eq!(g.get_link_connection_head(0, at(900.0)), None);

    g.remove_pylon(3);
    assert!(!g.pylons.contains_key(&3));
    assert!(g.pylons[&1].neighbours.is_empty() && g.pylons[&2].neighbours.is_empty());
    assert!(!g.links[0].pylons.contains(&3));
    assert!(!g.links[0].connected);

    // a recycled unit id replaces the stale pylon
    g.add_pylon(unit(2), 150, at(1100.0));
    assert_eq!(g.metal_spots[0].pylons, [1, 2]);
    assert!(g.metal_spots[1].pylons.is_empty());
}

#[test]
fn overdrive_sweet_spot_extends_the_grid() {
    let cb = fake_cb();
    let mut g = graph(&[(1000.0, 1000.0), (2000.0, 1000.0)]);
    g.pylon_defs.insert("energypylon".into(), 500);
    assert_eq!(g.get_overdrive_sweet_spot(Vec3::ZERO, "nosuchpylon"), Vec3::ZERO);

    // unowned link: just head for the nearest unconnected spot
    assert_eq!(g.get_overdrive_sweet_spot(Vec3::xz(0.0, 1000.0), "energypylon"), Vec3::xz(1000.0, 1000.0));

    g.set_owned(0);
    g.set_owned(1);
    // no pylons yet: start a chain from the near end towards the link center
    let p = g.get_overdrive_sweet_spot(Vec3::xz(0.0, 1000.0), "energypylon");
    assert!((p.x - 1450.0).abs() < 1e-3 && (p.z - 1000.0).abs() < 1e-3, "{p:?}");

    // with a pylon at the near end: continue from it towards the far end
    g.add_pylon(Unit::from_id(cb, 7).unwrap(), 150, Vec3::xz(1100.0, 1000.0));
    let p = g.get_overdrive_sweet_spot(Vec3::xz(0.0, 1000.0), "energypylon");
    assert!((p.x - (1100.0 + 0.9 * 500.0 + 0.9 * 150.0)).abs() < 1e-3, "{p:?}");
}

#[test]
fn metal_spots_json() {
    let data = parse_metal_spots_json(
        r#"[{"x":1,"y":2,"z":3,"metal":2.5},{"x":1,"z":3,"metal":1},"junk",{"x":"1","y":2,"z":3,"metal":1},{"metal":0.5,"z":-4.5,"y":0,"x":100}]"#,
    )
    .unwrap();
    assert_eq!(
        data,
        [SpotData { x: 1.0, y: 2.0, z: 3.0, metal: 2.5 }, SpotData { x: 100.0, y: 0.0, z: -4.5, metal: 0.5 }]
    );
    assert!(parse_metal_spots_json("{\"x\":1}").unwrap_err().contains("invalid result type"));
    assert!(parse_metal_spots_json("[{").unwrap_err().starts_with("Failed parsing JSON metalmap: "));
}

#[test]
fn lua_positions_use_six_decimals() {
    assert_eq!(lua_pos(Vec3::new(1234.5, 7.0, 88.0)), "1234.500000/88.000000");
    assert_eq!(lua_pos(Vec3::new(0.1, 0.0, -3.25)), "0.100000/-3.250000");
}

#[test]
fn ascii_graph_shows_spots_and_links() {
    let mut g = graph(&[(640.0, 640.0), (1280.0, 1280.0), (1920.0, 640.0)]);
    g.set_owned(0);
    g.set_hostile(2);
    let text = g.render_ascii_graph();
    let rows: Vec<&str> = text.lines().collect();
    assert!(rows.iter().all(|r| r.len() == rows[0].len()));
    // spots at 64-elmo resolution
    assert_eq!(rows[10].as_bytes()[10], b'o');
    assert_eq!(rows[10].as_bytes()[30], b'e');
    assert_eq!(rows[20].as_bytes()[20], b'u');
    // the 45-degree A-B link is drawn with the steep-line character
    assert_eq!(rows[15].as_bytes()[15], b'|');
    // B is both ally- and enemy-shadowed: contested outline
    assert!(text.contains('c'));
}
