//! Hand-built maps for tests.

use crate::{CELL, Choke, ChokeKind, NONE, Pos, Region, Tier, TierMap};

/// A hand-built map: `#` obstacle, letters regions; chokes are given as
/// (region, region, cell x, cell z, width in cells).
pub fn grid_map(rows: &[&str], chokes: &[(char, char, f32, f32, f32)]) -> TierMap {
    let (w, h) = (rows[0].len(), rows.len());
    let mut region = vec![NONE; w * h];
    for (z, row) in rows.iter().enumerate() {
        for (x, ch) in row.chars().enumerate() {
            if ch != '#' {
                region[z * w + x] = (ch as u8 - b'a') as u32;
            }
        }
    }
    let n = region.iter().filter(|&&r| r != NONE).max().unwrap() + 1;
    // Components: 4-connected passable cells.
    let mut component = vec![NONE; w * h];
    let mut next = 0;
    for start in 0..w * h {
        if region[start] == NONE || component[start] != NONE {
            continue;
        }
        let mut stack = vec![start];
        component[start] = next;
        while let Some(c) = stack.pop() {
            let (x, z) = (c % w, c / w);
            for (dx, dz) in [(-1i32, 0i32), (1, 0), (0, -1), (0, 1)] {
                let (nx, nz) = (x as i32 + dx, z as i32 + dz);
                if nx < 0 || nz < 0 || nx >= w as i32 || nz >= h as i32 {
                    continue;
                }
                let nc = nz as usize * w + nx as usize;
                if region[nc] != NONE && component[nc] == NONE {
                    component[nc] = next;
                    stack.push(nc);
                }
            }
        }
        next += 1;
    }
    let mut regions: Vec<Region> = (0..n as usize)
        .map(|r| {
            let cells: Vec<usize> = (0..w * h).filter(|&c| region[c] == r as u32).collect();
            let (sx, sz) = cells.iter().fold((0.0, 0.0), |(a, b), &c| (a + (c % w) as f32, b + (c / w) as f32));
            let k = cells.len() as f32;
            Region {
                id: r,
                center: Pos::new((sx / k + 0.5) * CELL, (sz / k + 0.5) * CELL),
                cells: cells.len(),
                component: component[cells[0]],
                chokes: Vec::new(),
            }
        })
        .collect();
    let chokes: Vec<Choke> = chokes
        .iter()
        .enumerate()
        .map(|(i, &(a, b, x, z, width))| {
            let (ra, rb) = ((a as u8 - b'a') as usize, (b as u8 - b'a') as usize);
            regions[ra].chokes.push(i);
            regions[rb].chokes.push(i);
            Choke {
                id: i,
                kind: ChokeKind::Gate,
                pos: Pos::new((x + 0.5) * CELL, (z + 0.5) * CELL),
                width: width * CELL,
                gate: None,
                regions: vec![ra.min(rb), ra.max(rb)],
                climb: vec![0.0; 2],
                bypass: vec![f32::INFINITY; 2],
                wall: width * CELL,
            }
        })
        .collect();
    TierMap { tier: Tier::Vehicle, width: w, height: h, regions, chokes, component, region, passable_fraction: 1.0 }
}

/// Scale a layout up: every character becomes a `k` x `k` block.
pub fn scaled(rows: &[&str], k: usize) -> Vec<String> {
    rows.iter()
        .flat_map(|r| {
            let wide: String = r.chars().flat_map(|c| std::iter::repeat_n(c, k)).collect();
            std::iter::repeat_n(wide, k)
        })
        .collect()
}

/// Three rooms a-b-c in a row, and below them two strips d (under a)
/// and e (under c) split by a thin wall; laid out at 4x scale so travel
/// distances are realistic (~1700 x 450 elmos).
pub fn rooms() -> TierMap {
    let rows = scaled(
        &[
            "aaaaaaaa#bbbbbbbb#cccccccc",
            "aaaaaaaa#bbbbbbbb#cccccccc",
            "aaaaaaaaabbbbbbbbbcccccccc",
            "aaaaaaaa#bbbbbbbb#cccccccc",
            "#dd#####################ee",
            "dddddddddddd##eeeeeeeeeeee",
            "dddddddddddd##eeeeeeeeeeee",
        ],
        4,
    );
    let rows: Vec<&str> = rows.iter().map(String::as_str).collect();
    let k = 4.0;
    grid_map(
        &rows,
        &[
            ('a', 'b', 8.0 * k, 2.0 * k, k),
            ('b', 'c', 17.0 * k, 2.0 * k, k),
            ('a', 'd', 1.0 * k, 4.0 * k, 2.0 * k),
            ('c', 'e', 24.0 * k, 4.0 * k, 2.0 * k),
        ],
    )
}

/// Two rooms a and b side by side with a door between them, and a long
/// corridor c below joining their far ends: a ring, so closing the door
/// makes the way round long but not impossible. At 4x scale (~1600 x 320
/// elmos).
pub fn ring() -> TierMap {
    let rows = scaled(
        &[
            "aaaaaaaaaaaa#bbbbbbbbbbbb",
            "aaaaaaaaaaaaabbbbbbbbbbbb",
            "aaaaaaaaaaaa#bbbbbbbbbbbb",
            "a#######################b",
            "ccccccccccccccccccccccccc",
        ],
        4,
    );
    let rows: Vec<&str> = rows.iter().map(String::as_str).collect();
    let k = 4.0;
    grid_map(&rows, &[('a', 'b', 12.0 * k, 1.0 * k, k), ('a', 'c', 0.0, 3.0 * k, k), ('b', 'c', 24.0 * k, 3.0 * k, k)])
}

/// Center of a layout position (in unscaled layout cells).
pub fn at(x: f32, z: f32) -> Pos {
    Pos::new((x * 4.0 + 2.0) * CELL, (z * 4.0 + 2.0) * CELL)
}
