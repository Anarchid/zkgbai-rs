use springai::{Unit, Vec3};

/// An energy-grid building (pylon, solar, fusion, mex...) linking spots.
pub struct Pylon {
    pub unit: Unit,
    pub radius: i32,
    pub position: Vec3,
    /// Metal spot indices in range.
    pub spots: Vec<usize>,
    /// Unit ids of neighbouring pylons.
    pub neighbours: Vec<i32>,
    /// Link indices in range.
    pub links: Vec<usize>,
}

impl Pylon {
    /// A grid building of link range `radius` standing at `position`.
    pub fn new(unit: Unit, radius: i32, position: Vec3) -> Self {
        Pylon { unit, radius, position, spots: Vec::new(), neighbours: Vec::new(), links: Vec::new() }
    }

    pub fn get_unit_id(&self) -> i32 {
        self.unit.id()
    }

    pub fn add_neighbour(&mut self, id: i32) {
        if !self.neighbours.contains(&id) {
            self.neighbours.push(id);
        }
    }

    pub fn remove_neighbour(&mut self, id: i32) {
        if let Some(i) = self.neighbours.iter().position(|&n| n == id) {
            self.neighbours.remove(i);
        }
    }
}
