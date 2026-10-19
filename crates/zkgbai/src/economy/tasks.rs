//! Worker tasks: construction, reclaim, repair, combat reclaim.
//!
//! The original shares task objects between several category lists
//! (`constructionTasks`, `porcTasks`, ...) and between workers. Here every
//! task lives in one [`TaskStore`], referenced by [`TaskId`] from the
//! economy's category lists and from [`Worker::task`](crate::economy::Worker).
//! Membership is kept consistent in both directions: a worker's `task` names
//! a task whose `assigned_workers` lists that worker.

use std::collections::BTreeMap;

use springai::{Feature, FeatureDef, Unit, UnitDef, Vec3};

use crate::military::UnitClasses;

pub type TaskId = u32;

/// A job one or more workers can be assigned to.
pub struct WorkerTask {
    /// Store id; 0 for a candidate task not (yet) in the store.
    pub id: TaskId,
    pub position: Vec3,
    /// Worker unit ids.
    pub assigned_workers: Vec<i32>,
    pub kind: TaskKind,
}

/// What a [`WorkerTask`] is about.
pub enum TaskKind {
    Construction(ConstructionTask),
    Reclaim(ReclaimTask),
    Repair(RepairTask),
    CombatReclaim(CombatReclaimTask),
}

/// Build a structure (or finish its nanoframe).
pub struct ConstructionTask {
    pub build_type: UnitDef,
    pub facing: i32,
    /// The nanoframe, once started.
    pub target: Option<Unit>,
    /// For caretakers: the unit the caretaker will assist.
    pub ct_target: Option<Unit>,
    pub frame_issued: i32,
    pub fac_plop: bool,
    /// Unit id of the commander that owns the factory plop.
    pub assigned_plop: i32,
    pub fac_def: bool,
}

/// Reclaim a feature (wreck, tree, ...).
pub struct ReclaimTask {
    pub target: Feature,
    pub def: Option<FeatureDef>,
    pub metal_value: f32,
}

/// Repair (or finish building) a friendly unit.
pub struct RepairTask {
    pub target: Unit,
    pub is_shield_mob: bool,
    pub is_tank: bool,
}

/// Reclaim a unit (an enemy nanoframe, or one of our own units).
pub struct CombatReclaimTask {
    pub target: Unit,
}

impl WorkerTask {
    fn with_kind(position: Vec3, kind: TaskKind) -> Self {
        WorkerTask { id: 0, position, assigned_workers: Vec::new(), kind }
    }

    /// A candidate construction task (the original two- and three-argument
    /// `ConstructionTask` constructors).
    pub fn construction(def: UnitDef, pos: Vec3, facing: i32) -> Self {
        Self::with_kind(
            pos,
            TaskKind::Construction(ConstructionTask {
                build_type: def,
                facing,
                target: None,
                ct_target: None,
                frame_issued: 0,
                fac_plop: false,
                assigned_plop: 0,
                fac_def: false,
            }),
        )
    }

    /// A candidate factory task, possibly a commander's instant plop.
    pub fn construction_plop(def: UnitDef, pos: Vec3, facing: i32, is_plop: bool) -> Self {
        let mut t = Self::construction(def, pos, facing);
        if let TaskKind::Construction(c) = &mut t.kind {
            c.fac_plop = is_plop;
        }
        t
    }

    pub fn reclaim(feat: Feature, metal: f32) -> Self {
        Self::with_kind(
            feat.pos(),
            TaskKind::Reclaim(ReclaimTask { target: feat, def: feat.def(), metal_value: metal }),
        )
    }

    /// `name` is the target's unit-def name.
    pub fn repair(target: Unit, name: &str, unit_classes: &UnitClasses) -> Self {
        let is_shield_mob = unit_classes.shield_mobs.contains(name) && name != "striderfunnelweb";
        let is_tank = name == "tankassault" || name == "tankriot" || name == "tankheavyassault";
        Self::with_kind(target.pos(), TaskKind::Repair(RepairTask { target, is_shield_mob, is_tank }))
    }

    pub fn combat_reclaim(target: Unit) -> Self {
        Self::with_kind(target.pos(), TaskKind::CombatReclaim(CombatReclaimTask { target }))
    }

    /// Where the work happens (reclaim and repair tasks follow their target).
    pub fn get_pos(&self) -> Vec3 {
        match &self.kind {
            TaskKind::Reclaim(r) => {
                let p = r.target.pos();
                if p != Vec3::ZERO { p } else { self.position }
            }
            TaskKind::Repair(r) => r.target.pos(),
            TaskKind::Construction(_) | TaskKind::CombatReclaim(_) => self.position,
        }
    }

    /// The original task equality: construction tasks are equal when type,
    /// position (x/z) and facing match; reclaim by feature; repair and combat
    /// reclaim by target unit.
    pub fn same_as(&self, other: &WorkerTask) -> bool {
        match (&self.kind, &other.kind) {
            (TaskKind::Construction(a), TaskKind::Construction(b)) => {
                a.build_type.id() == b.build_type.id()
                    && self.position.x == other.position.x
                    && self.position.z == other.position.z
                    && a.facing == b.facing
            }
            (TaskKind::Reclaim(a), TaskKind::Reclaim(b)) => a.target.id() == b.target.id(),
            (TaskKind::Repair(a), TaskKind::Repair(b)) => a.target.id() == b.target.id(),
            (TaskKind::CombatReclaim(a), TaskKind::CombatReclaim(b)) => a.target.id() == b.target.id(),
            _ => false,
        }
    }

    pub fn as_construction(&self) -> Option<&ConstructionTask> {
        match &self.kind {
            TaskKind::Construction(c) => Some(c),
            _ => None,
        }
    }

    pub fn as_construction_mut(&mut self) -> Option<&mut ConstructionTask> {
        match &mut self.kind {
            TaskKind::Construction(c) => Some(c),
            _ => None,
        }
    }

    pub fn add_worker(&mut self, worker: i32) {
        if !self.assigned_workers.contains(&worker) {
            self.assigned_workers.push(worker);
        }
    }

    pub fn remove_worker(&mut self, worker: i32) {
        if let Some(i) = self.assigned_workers.iter().position(|&w| w == worker) {
            self.assigned_workers.remove(i);
        }
    }
}

/// Owner of all worker tasks.
pub struct TaskStore {
    tasks: BTreeMap<TaskId, WorkerTask>,
    next_id: TaskId,
}

impl Default for TaskStore {
    fn default() -> Self {
        TaskStore { tasks: BTreeMap::new(), next_id: 1 }
    }
}

impl TaskStore {
    pub fn get(&self, id: TaskId) -> Option<&WorkerTask> {
        self.tasks.get(&id)
    }

    pub fn get_mut(&mut self, id: TaskId) -> Option<&mut WorkerTask> {
        self.tasks.get_mut(&id)
    }

    /// Store a candidate task, assigning it a fresh id.
    pub fn insert(&mut self, mut task: WorkerTask) -> TaskId {
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1).max(1);
        task.id = id;
        self.tasks.insert(id, task);
        id
    }

    pub fn remove(&mut self, id: TaskId) -> Option<WorkerTask> {
        self.tasks.remove(&id)
    }

    /// Whether any task in `list` equals `task` (the original `CONTAINSP`).
    pub fn contains(&self, list: &[TaskId], task: &WorkerTask) -> bool {
        list.iter().any(|id| self.tasks.get(id).is_some_and(|t| t.same_as(task)))
    }
}

/// A caretaker and the factory (or other unit) it assists.
#[derive(Clone, Copy, Debug)]
pub struct Nanotower {
    pub unit: Unit,
    pub target: Unit,
}

#[cfg(test)]
mod tests {
    use super::*;
    use springai::Callback;

    /// A callback handle that is never dereferenced: these tests only touch
    /// ids, which need no engine calls.
    fn cb() -> Callback {
        unsafe { Callback::from_raw(0, std::ptr::NonNull::dangling().as_ptr()) }
    }

    fn def(id: i32) -> UnitDef {
        UnitDef::from_id(cb(), id).unwrap()
    }

    #[test]
    fn construction_equality_uses_type_xz_and_facing() {
        let a = WorkerTask::construction(def(3), Vec3::new(100.0, 5.0, 200.0), 0);
        let same = WorkerTask::construction(def(3), Vec3::new(100.0, 7.0, 200.0), 0);
        let mut plop = WorkerTask::construction_plop(def(3), Vec3::new(100.0, 0.0, 200.0), 0, true);
        plop.as_construction_mut().unwrap().ct_target = Unit::from_id(cb(), 9);
        assert!(a.same_as(&same));
        assert!(a.same_as(&plop));
        assert!(!a.same_as(&WorkerTask::construction(def(4), Vec3::new(100.0, 5.0, 200.0), 0)));
        assert!(!a.same_as(&WorkerTask::construction(def(3), Vec3::new(101.0, 5.0, 200.0), 0)));
        assert!(!a.same_as(&WorkerTask::construction(def(3), Vec3::new(100.0, 5.0, 200.0), 1)));
    }

    #[test]
    fn store_assigns_ids_and_finds_duplicates_per_list() {
        let mut store = TaskStore::default();
        let a = store.insert(WorkerTask::construction(def(1), Vec3::xz(0.0, 0.0), 0));
        let b = store.insert(WorkerTask::construction(def(2), Vec3::xz(0.0, 0.0), 0));
        assert!(a != 0 && b != 0 && a != b);
        assert_eq!(store.get(a).unwrap().id, a);

        let probe = WorkerTask::construction(def(2), Vec3::xz(0.0, 0.0), 0);
        assert!(store.contains(&[a, b], &probe));
        assert!(!store.contains(&[a], &probe));

        store.remove(b);
        assert!(!store.contains(&[a, b], &probe));
    }

    #[test]
    fn workers_are_added_once_and_removed() {
        let mut t = WorkerTask::construction(def(1), Vec3::ZERO, 0);
        t.add_worker(5);
        t.add_worker(5);
        t.add_worker(6);
        assert_eq!(t.assigned_workers, vec![5, 6]);
        t.remove_worker(5);
        t.remove_worker(42);
        assert_eq!(t.assigned_workers, vec![6]);
    }
}
