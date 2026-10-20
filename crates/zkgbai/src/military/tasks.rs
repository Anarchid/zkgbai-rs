//! Military task records.

use springai::Vec3;

/// A place where friendly units were attacked, worth defending.
#[derive(Clone, Copy, Debug)]
pub struct DefenseTarget {
    pub position: Vec3,
    pub damage: f32,
    pub frame_issued: i32,
    /// Hit by a water weapon from out of sight in water: what did it may
    /// be under water, where only water weapons reach (see `water`).
    pub under_water: bool,
}

impl DefenseTarget {
    pub fn new(position: Vec3, damage: f32, frame_issued: i32) -> Self {
        Self { position, damage, frame_issued, under_water: false }
    }
}

/// Recently attacked position, used to spread attacks around.
#[derive(Clone, Copy, Debug)]
pub struct TargetMarker {
    pub position: Vec3,
    pub frame: i32,
}

impl TargetMarker {
    pub fn new(position: Vec3, frame: i32) -> Self {
        Self { position, frame }
    }
}

pub type ScoutTaskId = u32;

/// A metal spot raiders should go look at (and kill whatever is there).
#[derive(Clone, Debug)]
pub struct ScoutTask {
    pub id: ScoutTaskId,
    pub target: Vec3,
    /// Metal spot index (into `GraphManager::metal_spots`).
    pub spot: usize,
    pub last_path_check_frame: i32,
    /// Fighter-store ids of assigned raiders.
    pub assigned_raiders: Vec<i32>,
}

impl ScoutTask {
    pub fn new(id: ScoutTaskId, target: Vec3, spot: usize) -> Self {
        Self { id, target, spot, last_path_check_frame: 0, assigned_raiders: Vec::new() }
    }

    pub fn add_raider(&mut self, raider: i32) {
        if !self.assigned_raiders.contains(&raider) {
            self.assigned_raiders.push(raider);
        }
    }

    pub fn remove_raider(&mut self, raider: i32) {
        self.assigned_raiders.retain(|&r| r != raider);
    }

    pub fn has_raider(&self, raider: i32) -> bool {
        self.assigned_raiders.contains(&raider)
    }

    pub fn count_raiders(&self) -> usize {
        self.assigned_raiders.len()
    }
}

/// Scout tasks by id (owned by the raider handler).
pub type ScoutTasks = std::collections::BTreeMap<ScoutTaskId, ScoutTask>;
