use std::collections::BTreeSet;

use springai::Vec3;

/// A graph edge between two metal spots.
///
/// Methods of the original that need the endpoint spots or pylons
/// (`isOwned`, `isAllyShadowed`, `isHostile`, `checkConnected`,
/// `getConnectionHead`) are `GraphManager` methods taking the link.
pub struct Link {
    /// Index in `GraphManager::links`.
    pub index: usize,
    /// Metal spot indices.
    pub v0: usize,
    pub v1: usize,
    pub center_pos: Vec3,
    pub length: f32,
    pub connected: bool,
    /// Unit ids of pylons on this link.
    pub pylons: BTreeSet<i32>,
}

impl Link {
    /// A link between spots `v0` and `v1`, located at `pos0` and `pos1`.
    pub fn new(index: usize, v0: usize, v1: usize, pos0: Vec3, pos1: Vec3) -> Self {
        let center_pos = Vec3::new((pos0.x + pos1.x) / 2.0, (pos0.y + pos1.y) / 2.0, (pos0.z + pos1.z) / 2.0);
        let dx = pos0.x - pos1.x;
        let dz = pos0.z - pos1.z;
        Link {
            index,
            v0,
            v1,
            center_pos,
            length: (dx * dx + dz * dz).sqrt(),
            connected: false,
            pylons: BTreeSet::new(),
        }
    }

    pub fn get_pos(&self) -> Vec3 {
        self.center_pos
    }
}
