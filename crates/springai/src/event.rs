use std::ffi::{CStr, c_char, c_void};

use springai_sys as sys;

use crate::{Callback, Unit, Vec3, WeaponDef};

/// An engine event, decoded into owned data.
///
/// Owned decoding lets events be queued (see the re-entrancy handling in
/// [`export`](crate::export)) and keeps raw pointers out of AI code.
#[derive(Clone, Debug)]
pub enum Event {
    Init {
        saved_game: bool,
    },
    Release {
        reason: i32,
    },
    Update {
        frame: i32,
    },
    Message {
        player: i32,
        text: String,
    },
    LuaMessage {
        data: String,
    },
    UnitCreated {
        unit: Unit,
        builder: Option<Unit>,
    },
    UnitFinished {
        unit: Unit,
    },
    UnitIdle {
        unit: Unit,
    },
    UnitMoveFailed {
        unit: Unit,
    },
    UnitDamaged {
        unit: Unit,
        attacker: Option<Unit>,
        damage: f32,
        dir: Vec3,
        weapon: Option<WeaponDef>,
        paralyzer: bool,
    },
    UnitDestroyed {
        unit: Unit,
        attacker: Option<Unit>,
    },
    UnitGiven {
        unit: Unit,
        old_team: i32,
        new_team: i32,
    },
    UnitCaptured {
        unit: Unit,
        old_team: i32,
        new_team: i32,
    },
    EnemyEnterLos {
        enemy: Unit,
    },
    EnemyLeaveLos {
        enemy: Unit,
    },
    EnemyEnterRadar {
        enemy: Unit,
    },
    EnemyLeaveRadar {
        enemy: Unit,
    },
    EnemyDamaged {
        enemy: Unit,
        attacker: Option<Unit>,
        damage: f32,
        dir: Vec3,
        weapon: Option<WeaponDef>,
        paralyzer: bool,
    },
    EnemyDestroyed {
        enemy: Unit,
        attacker: Option<Unit>,
    },
    EnemyCreated {
        enemy: Unit,
    },
    EnemyFinished {
        enemy: Unit,
    },
    WeaponFired {
        unit: Unit,
        weapon: Option<WeaponDef>,
    },
    PlayerCommand {
        units: Vec<Unit>,
        command_topic: i32,
        player: i32,
    },
    CommandFinished {
        unit: Unit,
        command_id: i32,
        command_topic: i32,
    },
    SeismicPing {
        pos: Vec3,
        strength: f32,
    },
    Load {
        file: String,
    },
    Save {
        file: String,
    },
    /// A topic this crate does not know (newer engine) or a malformed event.
    Unknown {
        topic: i32,
    },
}

fn cstr(p: *const c_char) -> String {
    if p.is_null() { String::new() } else { unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned() }
}

impl Event {
    /// Decode an engine event.
    ///
    /// # Safety
    /// `data` must be the event struct the engine passed for `topic`.
    pub unsafe fn decode(cb: Callback, topic: i32, data: *const c_void) -> Event {
        use sys::EventTopic as T;
        macro_rules! evt {
            ($S:ident) => {
                unsafe { &*(data as *const sys::$S) }
            };
        }
        let unit = |id: i32| Unit::from_id(cb, id);
        let wdef = |id: i32| WeaponDef::from_id(cb, id);
        if data.is_null() {
            return Event::Unknown { topic };
        }
        let decoded = match topic {
            t if t == T::EVENT_INIT as i32 => Some(Event::Init { saved_game: evt!(SInitEvent).savedGame }),
            t if t == T::EVENT_RELEASE as i32 => Some(Event::Release { reason: evt!(SReleaseEvent).reason }),
            t if t == T::EVENT_UPDATE as i32 => Some(Event::Update { frame: evt!(SUpdateEvent).frame }),
            t if t == T::EVENT_MESSAGE as i32 => {
                let e = evt!(SMessageEvent);
                Some(Event::Message { player: e.player, text: cstr(e.message) })
            }
            t if t == T::EVENT_LUA_MESSAGE as i32 => {
                Some(Event::LuaMessage { data: cstr(evt!(SLuaMessageEvent).inData) })
            }
            t if t == T::EVENT_UNIT_CREATED as i32 => {
                let e = evt!(SUnitCreatedEvent);
                unit(e.unit).map(|u| Event::UnitCreated { unit: u, builder: unit(e.builder) })
            }
            t if t == T::EVENT_UNIT_FINISHED as i32 => {
                unit(evt!(SUnitFinishedEvent).unit).map(|u| Event::UnitFinished { unit: u })
            }
            t if t == T::EVENT_UNIT_IDLE as i32 => unit(evt!(SUnitIdleEvent).unit).map(|u| Event::UnitIdle { unit: u }),
            t if t == T::EVENT_UNIT_MOVE_FAILED as i32 => {
                unit(evt!(SUnitMoveFailedEvent).unit).map(|u| Event::UnitMoveFailed { unit: u })
            }
            t if t == T::EVENT_UNIT_DAMAGED as i32 => {
                let e = evt!(SUnitDamagedEvent);
                unit(e.unit).map(|u| Event::UnitDamaged {
                    unit: u,
                    attacker: unit(e.attacker),
                    damage: e.damage,
                    dir: unsafe { Vec3::from_ptr(e.dir_posF3) },
                    weapon: wdef(e.weaponDefId),
                    paralyzer: e.paralyzer,
                })
            }
            t if t == T::EVENT_UNIT_DESTROYED as i32 => {
                let e = evt!(SUnitDestroyedEvent);
                unit(e.unit).map(|u| Event::UnitDestroyed { unit: u, attacker: unit(e.attacker) })
            }
            t if t == T::EVENT_UNIT_GIVEN as i32 => {
                let e = evt!(SUnitGivenEvent);
                unit(e.unitId).map(|u| Event::UnitGiven { unit: u, old_team: e.oldTeamId, new_team: e.newTeamId })
            }
            t if t == T::EVENT_UNIT_CAPTURED as i32 => {
                let e = evt!(SUnitCapturedEvent);
                unit(e.unitId).map(|u| Event::UnitCaptured { unit: u, old_team: e.oldTeamId, new_team: e.newTeamId })
            }
            t if t == T::EVENT_ENEMY_ENTER_LOS as i32 => {
                unit(evt!(SEnemyEnterLOSEvent).enemy).map(|u| Event::EnemyEnterLos { enemy: u })
            }
            t if t == T::EVENT_ENEMY_LEAVE_LOS as i32 => {
                unit(evt!(SEnemyLeaveLOSEvent).enemy).map(|u| Event::EnemyLeaveLos { enemy: u })
            }
            t if t == T::EVENT_ENEMY_ENTER_RADAR as i32 => {
                unit(evt!(SEnemyEnterRadarEvent).enemy).map(|u| Event::EnemyEnterRadar { enemy: u })
            }
            t if t == T::EVENT_ENEMY_LEAVE_RADAR as i32 => {
                unit(evt!(SEnemyLeaveRadarEvent).enemy).map(|u| Event::EnemyLeaveRadar { enemy: u })
            }
            t if t == T::EVENT_ENEMY_DAMAGED as i32 => {
                let e = evt!(SEnemyDamagedEvent);
                unit(e.enemy).map(|u| Event::EnemyDamaged {
                    enemy: u,
                    attacker: unit(e.attacker),
                    damage: e.damage,
                    dir: unsafe { Vec3::from_ptr(e.dir_posF3) },
                    weapon: wdef(e.weaponDefId),
                    paralyzer: e.paralyzer,
                })
            }
            t if t == T::EVENT_ENEMY_DESTROYED as i32 => {
                let e = evt!(SEnemyDestroyedEvent);
                unit(e.enemy).map(|u| Event::EnemyDestroyed { enemy: u, attacker: unit(e.attacker) })
            }
            t if t == T::EVENT_ENEMY_CREATED as i32 => {
                unit(evt!(SEnemyCreatedEvent).enemy).map(|u| Event::EnemyCreated { enemy: u })
            }
            t if t == T::EVENT_ENEMY_FINISHED as i32 => {
                unit(evt!(SEnemyFinishedEvent).enemy).map(|u| Event::EnemyFinished { enemy: u })
            }
            t if t == T::EVENT_WEAPON_FIRED as i32 => {
                let e = evt!(SWeaponFiredEvent);
                unit(e.unitId).map(|u| Event::WeaponFired { unit: u, weapon: wdef(e.weaponDefId) })
            }
            t if t == T::EVENT_PLAYER_COMMAND as i32 => {
                let e = evt!(SPlayerCommandEvent);
                let ids = if e.unitIds.is_null() || e.unitIds_size <= 0 {
                    &[][..]
                } else {
                    unsafe { std::slice::from_raw_parts(e.unitIds, e.unitIds_size as usize) }
                };
                Some(Event::PlayerCommand {
                    units: ids.iter().filter_map(|&id| unit(id)).collect(),
                    command_topic: e.commandTopicId,
                    player: e.playerId,
                })
            }
            t if t == T::EVENT_COMMAND_FINISHED as i32 => {
                let e = evt!(SCommandFinishedEvent);
                unit(e.unitId).map(|u| Event::CommandFinished {
                    unit: u,
                    command_id: e.commandId,
                    command_topic: e.commandTopicId,
                })
            }
            t if t == T::EVENT_SEISMIC_PING as i32 => {
                let e = evt!(SSeismicPingEvent);
                Some(Event::SeismicPing { pos: unsafe { Vec3::from_ptr(e.pos_posF3) }, strength: e.strength })
            }
            t if t == T::EVENT_LOAD as i32 => Some(Event::Load { file: cstr(evt!(SLoadEvent).file) }),
            t if t == T::EVENT_SAVE as i32 => Some(Event::Save { file: cstr(evt!(SSaveEvent).file) }),
            _ => None,
        };
        decoded.unwrap_or(Event::Unknown { topic })
    }

    /// Short human-readable event name, for logs and profiling.
    pub fn name(&self) -> &'static str {
        match self {
            Event::Init { .. } => "init",
            Event::Release { .. } => "release",
            Event::Update { .. } => "update",
            Event::Message { .. } => "message",
            Event::LuaMessage { .. } => "luaMessage",
            Event::UnitCreated { .. } => "unitCreated",
            Event::UnitFinished { .. } => "unitFinished",
            Event::UnitIdle { .. } => "unitIdle",
            Event::UnitMoveFailed { .. } => "unitMoveFailed",
            Event::UnitDamaged { .. } => "unitDamaged",
            Event::UnitDestroyed { .. } => "unitDestroyed",
            Event::UnitGiven { .. } => "unitGiven",
            Event::UnitCaptured { .. } => "unitCaptured",
            Event::EnemyEnterLos { .. } => "enemyEnterLOS",
            Event::EnemyLeaveLos { .. } => "enemyLeaveLOS",
            Event::EnemyEnterRadar { .. } => "enemyEnterRadar",
            Event::EnemyLeaveRadar { .. } => "enemyLeaveRadar",
            Event::EnemyDamaged { .. } => "enemyDamaged",
            Event::EnemyDestroyed { .. } => "enemyDestroyed",
            Event::EnemyCreated { .. } => "enemyCreated",
            Event::EnemyFinished { .. } => "enemyFinished",
            Event::WeaponFired { .. } => "weaponFired",
            Event::PlayerCommand { .. } => "playerCommand",
            Event::CommandFinished { .. } => "commandFinished",
            Event::SeismicPing { .. } => "seismicPing",
            Event::Load { .. } => "load",
            Event::Save { .. } => "save",
            Event::Unknown { .. } => "unknown",
        }
    }
}
