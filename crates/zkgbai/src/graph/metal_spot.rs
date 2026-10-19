use springai::{Unit, Vec3};

/// A graph vertex: one metal spot, with ownership inferred from what has been
/// seen (or built) there.
///
/// Methods of the original that need other spots or links
/// (`isFrontLine`, `isConnected`) are `GraphManager::is_spot_front_line` and
/// `GraphManager::is_spot_connected`.
pub struct MetalSpot {
    /// Index in `GraphManager::metal_spots`.
    pub index: usize,
    pub position: Vec3,
    pub value: f32,
    pub visible: bool,
    pub owned: bool,
    pub touched: bool,
    pub hostile: bool,
    pub enemy_shadowed: bool,
    pub ally_shadowed: bool,
    pub extractor: Option<Unit>,
    pub last_seen: i32,
    /// Unit ids of pylons covering this spot.
    pub pylons: Vec<i32>,
    /// Indices into `GraphManager::links`.
    pub links: Vec<usize>,
}

impl MetalSpot {
    /// A fresh, neutral spot at `(x, y, z)` yielding `m` metal.
    pub fn new(index: usize, x: f32, y: f32, z: f32, m: f32) -> Self {
        MetalSpot {
            index,
            position: Vec3::new(x, y, z),
            value: m,
            visible: false,
            owned: false,
            touched: false,
            hostile: false,
            enemy_shadowed: false,
            ally_shadowed: false,
            extractor: None,
            last_seen: 0,
            pylons: Vec::new(),
            links: Vec::new(),
        }
    }

    pub fn get_pos(&self) -> Vec3 {
        self.position
    }

    pub fn get_value(&self) -> f32 {
        self.value
    }

    pub fn get_last_seen(&self) -> i32 {
        self.last_seen
    }

    /// Number of grid buildings covering this spot.
    pub fn get_pylon_count(&self) -> usize {
        self.pylons.len()
    }
}
