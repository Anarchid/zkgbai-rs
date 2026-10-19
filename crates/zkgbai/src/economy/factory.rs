//! A factory and its production bookkeeping.

use springai::{Unit, Vec3};

/// Which production logic a factory (or strider hub) follows, derived from
/// its unit-def name once at construction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FacKind {
    Cloak,
    Ship,
    Shield,
    Gunship,
    Plane,
    Amph,
    Veh,
    Hover,
    Tank,
    Spider,
    StriderHub,
    /// A factory the AI has no production logic for.
    Other,
}

impl FacKind {
    fn from_name(name: &str) -> Self {
        match name {
            "factorycloak" => FacKind::Cloak,
            "factoryship" => FacKind::Ship,
            "factoryshield" => FacKind::Shield,
            "factorygunship" => FacKind::Gunship,
            "factoryplane" => FacKind::Plane,
            "factoryamph" => FacKind::Amph,
            "factoryveh" => FacKind::Veh,
            "factoryhover" => FacKind::Hover,
            "factorytank" => FacKind::Tank,
            "factoryspider" => FacKind::Spider,
            "striderhub" => FacKind::StriderHub,
            _ => FacKind::Other,
        }
    }
}

#[derive(Clone)]
pub struct Factory {
    pub id: i32,
    pub unit: Unit,
    pub raider_spam: f32,
    pub expensive_raider_spam: f32,
    pub creating_scout: bool,
    pub small_raider_spam: bool,
    pub scout_allowance: i32,
    pub max_scout_allowance: i32,
    pub cost_per_bp: f32,
    pub worker_bp: f32,
    pos: Vec3,
    kind: FacKind,
}

impl Factory {
    /// A newly finished factory named `def_name`. `first_fac` marks the
    /// opening factory (one per merged team), which starts with raider and
    /// scout spam tuned to the map size.
    pub(crate) fn new(unit: Unit, def_name: &str, first_fac: bool, map_diag: f32) -> Self {
        let kind = FacKind::from_name(def_name);
        let mut fac = Factory {
            id: unit.id(),
            unit,
            raider_spam: 0.0,
            expensive_raider_spam: 0.0,
            creating_scout: false,
            small_raider_spam: false,
            scout_allowance: 0,
            max_scout_allowance: 0,
            cost_per_bp: 0.0,
            worker_bp: 0.0,
            pos: unit.pos(),
            kind,
        };

        if first_fac {
            fac.raider_spam = -6.0;
            fac.scout_allowance = 2;
            fac.max_scout_allowance = 2;

            let early_worker = map_diag > 910.0;
            let big_map = map_diag > 1270.0;

            if early_worker {
                fac.scout_allowance = 3;
                fac.max_scout_allowance = 4;
            }

            // if med_map (map_diag > 1080) { max_scout_allowance = 4 }
            if big_map {
                fac.raider_spam = -9.0;
            }

            match kind {
                FacKind::Tank => {
                    fac.scout_allowance = 1;
                    fac.raider_spam = 0.0;
                }
                FacKind::Gunship => fac.raider_spam = -2.0,
                FacKind::Veh => {
                    fac.scout_allowance -= 1;
                    fac.max_scout_allowance += 1;
                    fac.raider_spam = -4.0;
                }
                FacKind::Hover => {
                    fac.scout_allowance -= 1;
                    fac.max_scout_allowance += 1;
                    fac.raider_spam = if big_map { -10.0 } else { -5.0 };
                }
                FacKind::Spider => {
                    fac.max_scout_allowance = 12;
                    fac.scout_allowance = 12;
                }
                FacKind::Plane => fac.raider_spam = -1.0,
                _ => {}
            }
        } else if kind == FacKind::Gunship {
            fac.expensive_raider_spam = -6.0;
        }

        if matches!(kind, FacKind::Gunship | FacKind::Plane | FacKind::StriderHub) {
            fac.scout_allowance = 0;
            fac.max_scout_allowance = 0;
        }
        fac
    }

    pub fn get_unit(&self) -> Unit {
        self.unit
    }

    pub fn get_pos(&self) -> Vec3 {
        self.pos
    }

    pub(crate) fn kind(&self) -> FacKind {
        self.kind
    }

    /// Raise the scout allowance by `n`, capped at the maximum.
    pub(crate) fn add_scout_allowance(&mut self, n: i32) {
        self.scout_allowance = self.max_scout_allowance.min(self.scout_allowance + n);
    }
}
