//! Minimal Skirmish AI exercising the springai surface inside a real engine.
//! Logs what it sees and walks idle builders toward the map centre.

use springai::{Callback, Event, SkirmishAi, Vec3, opt};

struct Hello {
    cb: Callback,
    lua_messages: usize,
}

impl SkirmishAi for Hello {
    fn new(cb: Callback) -> Self {
        Hello { cb, lua_messages: 0 }
    }

    fn handle_event(&mut self, event: Event) -> i32 {
        let cb = self.cb;
        match event {
            Event::Init { saved_game } => {
                let map = cb.map();
                let metal = cb.resource_by_name("Metal");
                cb.log(&format!(
                    "[hello] init saved={saved_game} engine={} game={} {} map='{}' {}x{} team={} ally={} defs={} metal={:?} losmip={}",
                    cb.engine_version(),
                    cb.mod_info().human_name(),
                    cb.mod_info().version(),
                    map.name(),
                    map.width(),
                    map.height(),
                    cb.game().my_team(),
                    cb.game().my_ally_team(),
                    cb.unit_defs().len(),
                    metal,
                    cb.mod_info().los_mip_level(),
                ));
                if let Some(d) = cb.unit_def_by_name("cloakraid") {
                    let w = d.weapon_mounts().first().and_then(|m| m.weapon_def());
                    cb.log(&format!(
                        "[hello] cloakraid='{}' cost={:?} speed={} weapon={:?} dmg={:?} params={}",
                        d.human_name(),
                        metal.map(|m| d.cost(m)),
                        d.speed(),
                        w.map(|w| w.name()),
                        w.map(|w| w.damage_types().first().copied()),
                        d.custom_params().len()
                    ));
                }
                cb.call_lua_rules("ai_commander:dyntrainer_support_base");
            }
            Event::Update { frame } if frame % 300 == 0 => {
                let eco = cb.economy();
                let m = cb.resource_by_name("Metal").unwrap();
                cb.log(&format!(
                    "[hello] frame={frame} metal={:.0}/{:.0} income={:.2} units={} enemies={}",
                    eco.current(m),
                    eco.storage(m),
                    eco.income(m),
                    cb.team_units().len(),
                    cb.enemy_units_in_radar_and_los().len()
                ));
            }
            Event::UnitCreated { unit, builder } => {
                cb.log(&format!(
                    "[hello] created {:?} {} by {:?}",
                    unit,
                    unit.def().map(|d| d.name()).unwrap_or_default(),
                    builder
                ));
            }
            Event::UnitIdle { unit } => {
                let map = cb.map();
                let centre = Vec3::xz(map.width() as f32 * 4.0, map.height() as f32 * 4.0);
                let ok = unit.fight(centre, opt::NONE);
                cb.log(&format!("[hello] idle {:?} at {:?} -> fight centre ok={ok}", unit, unit.pos()));
            }
            Event::LuaMessage { data } => {
                self.lua_messages += 1;
                if self.lua_messages <= 5 {
                    cb.log(&format!("[hello] lua message: {}", &data[..data.len().min(160)]));
                }
            }
            Event::Release { reason } => cb.log(&format!("[hello] release reason={reason}")),
            _ => {}
        }
        0
    }
}

springai::export_ai!(Hello);
