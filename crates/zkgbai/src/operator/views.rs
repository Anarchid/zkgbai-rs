//! Read-only tools: what the AI knows, as JSON.
//!
//! Positions are `[x, z]` in elmos; values are rounded for reading.

use serde_json::{Value, json};
use springai::{Resource, Unit, UnitDef, Vec3, reserved};

use super::tools::Args;
use crate::Ai;
use crate::economy::{TaskKind, WorkerTask};
use crate::military::{Enemy, MilitaryManager, Squad, SquadKind};
use crate::util::NULLPOS;
use zk_terrain::{CELL, ChokeKind, Tier};

pub type Out = Result<Value, String>;

pub fn xz(p: Vec3) -> Value {
    json!([p.x.round() as i32, p.z.round() as i32])
}

/// A position, or `null` for the AI's "no position".
fn opt_xz(p: Vec3) -> Value {
    if p == NULLPOS { Value::Null } else { xz(p) }
}

pub fn r1(v: f32) -> Value {
    json!((f64::from(v) * 10.0).round() / 10.0)
}

fn r2(v: f32) -> Value {
    json!((f64::from(v) * 100.0).round() / 100.0)
}

pub fn game_time(frame: i32) -> String {
    let s = frame.max(0) / 30;
    format!("{}:{:02}", s / 60, s % 60)
}

fn resource(ai: &Ai, r: Resource) -> Value {
    let e = ai.cb.economy();
    json!({"current": r1(e.current(r)), "income": r1(e.income(r)), "usage": r1(e.usage(r)), "storage": r1(e.storage(r))})
}

pub(super) fn def_by_id(ai: &Ai, id: i32) -> Option<UnitDef> {
    ai.defs.all().find(|d| d.id() == id)
}

fn squad_kind(k: SquadKind) -> &'static str {
    match k {
        SquadKind::Assault => "assault",
        SquadKind::Raider => "raider",
        SquadKind::Taskforce => "taskforce",
        SquadKind::Hover => "hover",
        SquadKind::Jaeger => "jaeger",
    }
}

/// Display the adopted tactic without changing the squad's structural kind.
pub(super) fn squad_label(s: &Squad) -> &'static str {
    if squad_tactic(s).is_some() { "fireteam" } else { squad_kind(s.kind) }
}

pub(super) fn squad_tactic(s: &Squad) -> Option<&'static str> {
    (s.kind == SquadKind::Assault && s.status == 'a' && s.fire.active()).then(|| s.fire.mode.name())
}

fn squad_status(c: char) -> &'static str {
    match c {
        'f' => "forming",
        'r' => "rallying",
        'a' => "attacking",
        _ => "other",
    }
}

/// What a unit of ours is to the AI, coarsely.
pub fn role(ai: &Ai, u: Unit) -> &'static str {
    let id = u.id();
    if reserved::is_reserved(ai.cb, id) {
        return "reserved";
    }
    if ai.retreat_handler.is_retreating(id) {
        return "retreating";
    }
    if ai.stakeouts.contains(id) {
        return "stakeout";
    }
    // Home defense (see `qrf`): garrisons, its own teams, and squads moonlighting.
    if let Some(m) = ai.qrf.members.get(&id) {
        if ai.qrf_floor_guard(m.team) {
            return "garrison";
        }
        match m.squad {
            None => return "qrf",
            Some(s) if ai.qrf.drafted.contains_key(&s) => return "qrf_moonlight",
            _ => {}
        }
    }
    if ai.airstrike_manages(id) {
        return "airstrike";
    }
    if let Some(w) = ai.eco.workers.get(&id) {
        return if w.is_com { "commander" } else { "worker" };
    }
    if ai.fac.factories.contains_key(&id) {
        return "factory";
    }
    if let Some(f) = ai.fighters.get(&id) {
        let r = &ai.raider_handler;
        let m = &ai.misc_handler;
        return if f.squad.is_some_and(|s| ai.squads.get(s).is_some_and(|s| s.kind == SquadKind::Raider)) {
            "raider_squad"
        } else if f.squad.is_some() {
            "squad"
        } else if r.solo_raiders.contains(&id) || r.kodachis.contains(&id) {
            "raider_solo"
        } else if r.small_raiders.contains(&id) || r.medium_raiders.contains(&id) {
            "raider"
        } else if ai.war.aas.contains(&id) {
            "aa"
        } else if ai.war.hawks.contains(&id) {
            "fighter_plane"
        } else if m.supports.contains(&id) {
            "support"
        } else if m.striders.contains(&id) {
            "strider"
        } else if m.arties.contains(&id) {
            "artillery"
        } else if m.krows.contains(&id) {
            "krow"
        } else if m.loners.contains(&id) {
            "loner"
        } else {
            "fighter"
        };
    }
    if ai.eco.dporcs.contains_key(&id) {
        return "defense";
    }
    if ai.eco.nanos.contains_key(&id) {
        return "caretaker";
    }
    if ai.eco.mexes.contains(&u) {
        return "mex";
    }
    match u.def() {
        Some(d) if d.speed() <= 0.0 => "building",
        _ => "unit",
    }
}

fn task_kind(t: &WorkerTask) -> &'static str {
    match t.kind {
        TaskKind::Construction(_) => "construction",
        TaskKind::Reclaim(_) => "reclaim",
        TaskKind::Repair(_) => "repair",
        TaskKind::CombatReclaim(_) => "combat_reclaim",
    }
}

fn task_value(ai: &Ai, t: &WorkerTask) -> Value {
    let (what, started) = match &t.kind {
        TaskKind::Construction(c) => (ai.defs.name(c.build_type).to_owned(), c.target.map(|u| u.id())),
        TaskKind::Reclaim(r) => (r.def.map(|d| d.name()).unwrap_or_default(), None),
        TaskKind::Repair(r) => (ai.defs.name_of(r.target).to_owned(), Some(r.target.id())),
        TaskKind::CombatReclaim(r) => (ai.defs.name_of(r.target).to_owned(), Some(r.target.id())),
    };
    json!({
        "id": t.id,
        "kind": task_kind(t),
        "what": what,
        "pos": xz(t.get_pos()),
        "workers": t.assigned_workers,
        "unit": started,
        "priority": ai.op_priority(t.id),
        "issue": ai.logistics.issues.get(&t.id).map(|i| i.id),
    })
}

pub fn status(ai: &mut Ai, _: &Args) -> Out {
    let map = ai.cb.map();
    let modules = ai.active();
    let mut v = json!({
        "frame": ai.frame,
        "time": game_time(ai.frame),
        "state": ai.state(),
        "team": ai.team_id,
        "ally_team": ai.ally_team_id,
        "allies": ai.allies,
        "enemy_teams": ai.get_enemy_team_ids(),
        "ffa": ai.is_ffa,
        "map": {"name": map.name(), "size": [map.width() * 8, map.height() * 8]},
        "metal": resource(ai, ai.metal),
        "energy": resource(ai, ai.energy),
        "features_off": ai.features_off(),
        "test": ai.test,
        "run": ai.op.run_id,
        "journal": ai.op.journal_path.as_ref().map(|p| p.to_string_lossy()),
    });
    if modules {
        let owned = ai.graph.get_owned_spots().len();
        let enemy = ai.graph.get_enemy_spots().len();
        v["economy"] = json!({
            "income": r1(ai.eco.effective_income),
            "workers": ai.eco.workers.len(),
            "factories": ai.fac.factories.values().map(|f| ai.defs.name_of(f.unit)).collect::<Vec<_>>(),
            "estalled": ai.eco.estalled,
            "requisition": ai.eco.requisition.as_ref().map(|r| &r.0),
        });
        v["army"] = json!({
            "value": r1(ai.fac.army_value),
            "fighters": ai.fighters.len(),
            "squads": ai.squads.ids().len(),
            "retreating": ai.retreat_handler.num_retreating(),
            "enemy_fighter_value": r1(ai.war.enemy_fighter_value),
            "enemy_porc_value": r1(ai.war.enemy_porc_value),
            "enemy_known": ai.war.targets.len(),
        });
        v["territory"] = json!({
            "fraction": r2(ai.graph.territory_fraction),
            "spots_ours": owned,
            "spots_enemy": enemy,
            "spots_total": ai.graph.metal_spots.len(),
        });
        v["terrain_ready"] = json!(ai.terrain.get().is_some());
    }
    v["operator"] = overrides_value(ai);
    Ok(v)
}

pub fn economy(ai: &mut Ai, _: &Args) -> Out {
    let e = &ai.eco;
    Ok(json!({
        "metal": resource(ai, ai.metal),
        "energy": resource(ai, ai.energy),
        "income": {
            "effective": r1(e.effective_income),
            "effective_metal": r1(e.effective_income_metal),
            "static": r1(e.static_income),
            "reclaim": r1(e.reclaim_income),
            "base": r1(e.base_income),
            "adjusted": r1(e.adjusted_income),
        },
        "expenditure": r1(e.effective_expenditure),
        "greed": r2(e.greed),
        "estalled": e.estalled,
        "max_storage": r1(e.max_storage),
        "mobile_build_power": r1(ai.fac.mobile_bp),
        "workers": e.workers.len(),
        "commanders": e.commanders.len(),
        "idle_workers": e.idlers.len(),
        "tasks": {
            "construction": e.construction_tasks.len(),
            "factory": e.factory_tasks.len(),
            "defense": e.porc_tasks.len(),
            "reclaim": e.reclaim_tasks.len(),
            "energy_reclaim": e.energy_reclaim_tasks.len(),
            "repair": e.repair_tasks.len(),
            "combat_reclaim": e.combat_reclaim_tasks.len(),
        },
        "buildings": {"mexes": e.mexes.len(), "fusions": e.fusions.len(), "defenses": e.porcs.len(), "caretakers": e.nanos.len()},
        "requisition": e.requisition.as_ref().map(|(f, why)| json!({"factory": f, "why": why})),
        "issues": ai.logistics.issues.len(),
        "viable": {"bots": e.bots_viable, "spiders": e.spiders_viable, "ships": e.ships_viable},
        "defended_factory": e.defended_fac,
    }))
}

pub fn workers(ai: &mut Ai, _: &Args) -> Out {
    let list: Vec<Value> = ai
        .eco
        .workers
        .values()
        .map(|w| {
            json!({
                "id": w.id,
                "def": ai.defs.name_of(w.unit),
                "pos": xz(w.unit.pos()),
                "commander": w.is_com,
                "greedy": w.is_greedy,
                "fleeing": w.is_chicken,
                "build_power": r1(w.bp),
                "reserved": reserved::is_reserved(ai.cb, w.id),
                "task": w.task.and_then(|t| ai.eco.tasks.get(t)).map(|t| task_value(ai, t)),
            })
        })
        .collect();
    Ok(json!({"workers": list}))
}

pub fn tasks(ai: &mut Ai, a: &Args) -> Out {
    let kind = a.opt_str("kind")?;
    let limit = a.opt_i32("limit")?.unwrap_or(200).max(1) as usize;
    let all: Vec<&WorkerTask> = ai.eco.tasks.iter().filter(|t| kind.is_none_or(|k| task_kind(t) == k)).collect();
    let list: Vec<Value> = all.iter().take(limit).map(|t| task_value(ai, t)).collect();
    Ok(json!({"total": all.len(), "tasks": list}))
}

pub fn issues(ai: &mut Ai, _: &Args) -> Out {
    Ok(ai.logistics_json())
}

pub fn factories(ai: &mut Ai, _: &Args) -> Out {
    let fm = &ai.fac;
    let facs: Vec<Value> = fm
        .factories
        .values()
        .map(|f| {
            let queue: Vec<&str> = f
                .unit
                .current_commands()
                .iter()
                .filter(|c| c.id() < 0)
                .filter_map(|c| def_by_id(ai, -c.id()))
                .map(|d| ai.defs.name(d))
                .collect();
            json!({
                "id": f.id,
                "def": ai.defs.name_of(f.unit),
                "pos": xz(f.get_pos()),
                "kind": format!("{:?}", f.kind()).to_lowercase(),
                "queue": queue,
                "raider_spam": r1(f.raider_spam),
                "scout_allowance": f.scout_allowance,
            })
        })
        .collect();
    macro_rules! counts {
        ($($f:ident),*) => {{
            let mut m = serde_json::Map::new();
            $( if fm.$f != 0 { m.insert(stringify!($f).trim_start_matches("num_").to_owned(), json!(fm.$f)); } )*
            Value::Object(m)
        }};
    }
    let counts = counts!(
        num_workers,
        num_aas,
        num_erasers,
        num_aspis,
        num_warriors,
        num_zeus,
        num_rockos,
        num_glaives,
        num_scythes,
        num_maces,
        num_scalpels,
        num_reapers,
        num_banishers,
        num_levelers,
        num_ravagers,
        num_scouts,
        num_kodachis,
        num_infis,
        num_ultis,
        num_felons,
        num_rogues,
        num_thugs,
        num_laws,
        num_rackets,
        num_bandits,
        num_funnels,
        num_domis,
        num_venoms,
        num_redbacks,
        num_recluses,
        num_hermits,
        num_ravens,
        num_striders,
        num_heavy_striders
    );
    let orders: Vec<Value> =
        ai.op.over.fac_orders.iter().map(|o| json!({"unit": o.unit, "left": o.left, "factory": o.factory})).collect();
    Ok(json!({
        "factories": facs,
        "values": {
            "army": r1(fm.army_value), "fighters": r1(fm.fighter_value), "assault": r1(fm.assault_value),
            "aa": r1(fm.aa_value), "workers": r1(fm.worker_value), "artillery": r1(fm.arty_value),
        },
        "counts": counts,
        "enemy_seen": {
            "air": fm.enemy_has_air, "drones": fm.enemy_has_drones, "amphibious": fm.enemy_has_amphs,
            "heavy_defense": fm.enemy_has_heavy_porc,
        },
        "operator_orders": orders,
    }))
}

pub fn military(ai: &mut Ai, _: &Args) -> Out {
    let w = &ai.war;
    let r = &ai.raider_handler;
    let s = &ai.squad_handler;
    let m = &ai.misc_handler;
    let frame = ai.frame;
    let defend: Vec<Value> = w
        .defense_targets()
        .map(|(d, predicted)| {
            json!({"pos": xz(d.position), "damage": r1(d.damage), "age": frame - d.frame_issued, "predicted": predicted})
        })
        .collect();
    let incursions: Vec<Value> = ai
        .incursions
        .list
        .iter()
        .map(|i| {
            json!({"tier": format!("{:?}", i.tier), "units": i.units, "protected": r1(i.protected), "exposed": r1(i.exposed)})
        })
        .collect();
    let blocks: Vec<Value> = ai
        .incursions
        .orders
        .iter()
        .map(|(sid, b)| {
            json!({"squad": sid, "tier": format!("{:?}", b.tier), "choke": b.choke, "pos": xz(b.pos),
                   "power": r1(b.power), "slack": r1(b.slack)})
        })
        .collect();
    let i = &ai.intel;
    Ok(json!({
        "enemy": {
            "fighter_value": r1(w.enemy_fighter_value),
            "defense_value": r1(w.enemy_porc_value),
            "light_defense_value": r1(w.enemy_light_porc_value),
            "heavy_defense_value": r1(w.enemy_heavy_porc_value),
            "heavy_value": r1(w.enemy_heavy_value),
            "max_air_value": r1(w.max_enemy_air_value),
            "has_nuke": w.enemy_has_nuke,
            "has_anti_nuke": w.enemy_has_anti_nuke,
            "troll_commander": w.enemy_has_troll_com,
            "slasher_spam": w.slasher_spam,
            "known": w.targets.len(),
            "defenses": w.enemy_porcs.len(),
            "outranging_share": r2(w.outranging_share),
        },
        "ours": {
            "army_value": r1(ai.fac.army_value),
            "available_mobile_threat": r1(w.available_mobile_threat),
            "available_raider_threat": r1(w.available_raider_threat),
        },
        "handlers": {
            "solo_raiders": r.solo_raiders.len(),
            "kodachis": r.kodachis.len(),
            "small_raiders": r.small_raiders.len(),
            "medium_raiders": r.medium_raiders.len(),
            "raider_squads": r.raider_squads.len(),
            "assault_fighters": s.fighters.len(),
            "squads": s.squads.len(),
            "taskforces": s.taskforces.len(),
            "air_squads": s.air_squads.len(),
            "supports": m.supports.len(),
            "loners": m.loners.len(),
            "striders": m.striders.len(),
            "artillery": m.arties.len(),
            "krows": m.krows.len(),
            "mobile_aa": w.aas.len(),
            "fighter_planes": w.hawks.len(),
            "retreating": ai.retreat_handler.num_retreating(),
        },
        "defend": defend,
        "incursions": incursions,
        "blocks": blocks,
        "strikes": ai.strikes_status(),
        "airstrikes": ai.airstrikes_status(),
        "intel": {
            "enemy_produced": r1(i.produced),
            "enemy_income": r1(i.income),
            "killed": r1(i.killed),
            "unaccounted": r1(i.unaccounted()),
            "suspicious": r1(i.suspicious()),
        },
    }))
}

/// The map's regime as the posture reads it: structural openness per
/// movement tier (lanes), the deciding tier, density, and the fixed-defense
/// budget they set (see `crate::posture`).
pub fn regime(ai: &mut Ai, _: &Args) -> Out {
    use crate::posture;
    let p = &ai.posture;
    let levers = ai.posture_levers();
    let decisive = p.decisive().map(|l| l.tier);
    let lanes: Vec<Value> = p
        .lanes
        .iter()
        .map(|l| {
            let (lane, open) = posture::thresholds(l.tier);
            let fronts: Vec<Value> = (l.perimeter.fronts.iter())
                .map(|f| {
                    json!({"pos": [f.center.x.round(), f.center.z.round()], "span": f.span.round(),
                           "width": f.width.round(),
                           "sites": f.sites.iter().map(|s| [s.x.round(), s.z.round()]).collect::<Vec<_>>()})
                })
                .collect();
            json!({
                "tier": l.tier.name(), "decides": decisive == Some(l.tier),
                "openness": r2(l.openness), "perimeter": r2(l.perimeter.openness), "closure": r2(l.closure),
                "laneness": r2(posture::laneness(l.tier, l.openness)), "thresholds": [lane, open],
                "usable_km2": r1(l.usable_km2),
                "spots_ours": l.ours.iter().filter(|&&o| o).count(),
                "spots_kept": l.perimeter.kept.iter().filter(|&&k| k).count(),
                "fronts": fronts,
            })
        })
        .collect();
    let on = ai.posture_on();
    let siege = ai.missiles_siege_readiness();
    Ok(json!({
        "posture": on,
        "measured": !p.lanes.is_empty(),
        "tier": decisive.map(|t| t.name()),
        "enemy_tiers": ai.enemy_ground_tiers().iter().map(|t| t.name()).collect::<Vec<_>>(),
        "laneness": r2(p.laneness()),
        "density": p.density.round(),
        "density_ref": levers.density_ref.round(),
        "missile_silo_density_threshold": siege.threshold.round(),
        "richness": r2(p.richness),
        "share": r2(p.share),
        "cap": r2(levers.cap),
        "standing": p.standing.round(),
        "budget": (p.share * p.standing).round(),
        "defense": p.defense.round(),
        "all_defense": p.all_defense.round(),
        "orders_held": p.orders.len(),
        "front_orders": p.stats.front_orders,
        "point_orders": p.stats.point_orders,
        "stale": p.stats.stale,
        "lanes": lanes,
        "note": "openness: 0 closed off by narrow fronts for free, 1 no lane worth holding (perimeter and closure \
             averaged); laneness: how much the deciding tier's openness counts as lanes; density: standing metal \
             (ours and the enemy's as known) per square kilo-elmo of usable ground (F12 label 1500); share: of our \
             standing metal the posture keeps in its fixed defenses. Density is measured with posture off; \
             missiles also measure lanes without enabling the defense budget. Before lanes are measured, \
             the silo uses the full density threshold.",
    }))
}

pub fn squads(ai: &mut Ai, _: &Args) -> Out {
    let list: Vec<Value> = ai
        .squads
        .ids()
        .into_iter()
        .filter_map(|id| ai.squads.get(id))
        .map(|s| {
            let members: Vec<Value> = s
                .fighters
                .iter()
                .filter_map(|f| ai.fighters.get(f))
                .map(|f| json!([f.id, ai.defs.name_of(f.unit)]))
                .collect();
            json!({
                "id": s.id,
                "kind": squad_kind(s.kind),
                "label": squad_label(s),
                "tactic": squad_tactic(s),
                "air": s.is_air_squad,
                "status": squad_status(s.status),
                "pos": opt_xz(s.get_pos(&ai.fighters)),
                "target": opt_xz(s.target),
                "leader": s.leader,
                "value": r1(s.metal_value),
                "members": members,
                "courage": r2(s.courage),
                "fire": {"mode":s.fire.mode.name(),"cores":s.fire.cores,"range":s.fire.range,"focus":s.fire.focus},
                "operator_target": ai.op_squad_target(s.id).map(xz),
                "intent": ai.op.over.squad_intents.get(&s.id).map(|i| json!({"kind": i.kind.name(), "pos": xz(i.pos), "radius": r1(i.radius), "unit": i.unit, "seconds_left": (i.until - ai.frame) / 30})),
                "block": ai.incursions.orders.get(&s.id).map(|b| xz(b.pos)),
            })
        })
        .collect();
    Ok(json!({"squads": list}))
}

pub fn units(ai: &mut Ai, a: &Args) -> Out {
    let def = a.opt_str("def")?;
    let role_prefix = a.opt_str("role")?;
    let near = match (a.opt_f32("x")?, a.opt_f32("z")?) {
        (Some(x), Some(z)) => Some((Vec3::xz(x, z), a.f32_or("radius", 600.0)?)),
        _ => None,
    };
    let limit = a.opt_i32("limit")?.unwrap_or(300).max(1) as usize;
    let mut total = 0;
    let mut list = Vec::new();
    for u in ai.cb.team_units() {
        let name = ai.defs.name_of(u);
        if def.is_some_and(|d| !name.contains(d)) {
            continue;
        }
        let pos = u.pos();
        if near.is_some_and(|(p, r)| pos.distance_2d(p) > r) {
            continue;
        }
        let role = role(ai, u);
        if role_prefix.is_some_and(|r| !role.starts_with(r)) {
            continue;
        }
        total += 1;
        if list.len() < limit {
            let max = u.max_health();
            list.push(json!({
                "id": u.id(),
                "def": name,
                "pos": xz(pos),
                "health": if max > 0.0 { r2(u.health() / max) } else { Value::Null },
                "role": role,
                "squad": ai.fighters.get(&u.id()).and_then(|f| f.squad),
                "built": !u.is_being_built(),
            }));
        }
    }
    Ok(json!({"total": total, "units": list}))
}

fn enemy_flags(e: &Enemy) -> Vec<&'static str> {
    let mut f = Vec::new();
    for (on, name) in [
        (e.is_static, "static"),
        (e.is_porc, "porc"),
        (e.is_mex, "mex"),
        (e.is_com, "com"),
        (e.is_worker, "worker"),
        (e.is_raider, "raider"),
        (e.is_riot, "riot"),
        (e.is_arty, "arty"),
        (e.is_aa, "aa"),
        (e.can_fly(), "air"),
        (e.is_super_wep, "superweapon"),
        (e.is_major_cancer, "major_cancer"),
        (e.is_minor_cancer, "minor_cancer"),
        (e.is_strong, "strong"),
        (e.visible, "visible"),
        (e.is_radar_visible && !e.visible, "radar"),
        (!e.identified, "unidentified"),
    ] {
        if on {
            f.push(name);
        }
    }
    f
}

pub fn unit(ai: &mut Ai, a: &Args) -> Out {
    let id = a.i32("id")?;
    if let Some(e) = ai.war.targets.get(&id) {
        return Ok(json!({
            "id": id,
            "side": "enemy",
            "def": e.def_name,
            "human": e.ud.map(|d| d.human_name()),
            "pos": xz(e.position),
            "last_seen": e.last_seen,
            "seen_ago": ai.frame - e.last_seen,
            "value": r1(e.value),
            "power": r1(e.power),
            "danger": r1(e.get_danger()),
            "threat_radius": r1(e.threat_radius),
            "health": r1(e.get_health()),
            "flags": enemy_flags(e),
        }));
    }
    let u = Unit::from_id(ai.cb, id).ok_or("bad unit id")?;
    if u.team() != ai.team_id {
        return Err(format!("unit {id} is neither ours nor an enemy the AI knows"));
    }
    let def = u.def();
    let commands: Vec<Value> = u
        .current_commands()
        .iter()
        .take(16)
        .map(|c| {
            let cid = c.id();
            let what = if cid < 0 { def_by_id(ai, -cid).map(|d| ai.defs.name(d).to_owned()) } else { None };
            json!({"id": cid, "build": what, "params": c.params().iter().map(|p| r1(*p)).collect::<Vec<_>>()})
        })
        .collect();
    let mut v = json!({
        "id": id,
        "side": "ours",
        "def": ai.defs.name_of(u),
        "human": def.map(|d| d.human_name()),
        "pos": xz(u.pos()),
        "health": r1(u.health()),
        "max_health": r1(u.max_health()),
        "being_built": u.is_being_built(),
        "build_progress": r2(u.build_progress()),
        "paralyzed": u.is_paralyzed(),
        "cloaked": u.is_cloaked(),
        "experience": r2(u.experience()),
        "role": role(ai, u),
        "reserved": reserved::is_reserved(ai.cb, id),
        "commands": commands,
        "cost": def.map(|d| r1(d.cost(ai.metal))),
    });
    if let Some(f) = ai.fighters.get(&id) {
        v["fighter"] = json!({
            "kind": format!("{:?}", f.kind).to_lowercase(),
            "squad": f.squad,
            "power": r1(f.power),
            "metal_value": r1(f.metal_value),
            "target": opt_xz(f.target),
            "scout_task": f.task,
        });
    }
    if let Some(w) = ai.eco.workers.get(&id) {
        v["worker"] = json!({
            "commander": w.is_com,
            "greedy": w.is_greedy,
            "fleeing": w.is_chicken,
            "build_power": r1(w.bp),
            "task": w.task.and_then(|t| ai.eco.tasks.get(t)).map(|t| task_value(ai, t)),
        });
    }
    Ok(v)
}

pub fn enemies(ai: &mut Ai, a: &Args) -> Out {
    let kind = a.opt_str("kind")?.unwrap_or("all").to_owned();
    let visible = a.bool_or("visible", false);
    let near = match (a.opt_f32("x")?, a.opt_f32("z")?) {
        (Some(x), Some(z)) => Some((Vec3::xz(x, z), a.f32_or("radius", 1000.0)?)),
        _ => None,
    };
    let limit = a.opt_i32("limit")?.unwrap_or(100).max(1) as usize;
    let keep = |e: &Enemy| -> bool {
        let k = match kind.as_str() {
            "all" => true,
            "fighter" => !e.is_static && !e.is_worker && e.can_attack(),
            "porc" | "defense" => e.is_porc,
            "static" | "building" => e.is_static,
            "mex" => e.is_mex,
            "com" | "commander" => e.is_com,
            "worker" => e.is_worker,
            "aa" => e.is_aa,
            "air" => e.can_fly(),
            "radar" | "unidentified" => !e.identified,
            _ => false,
        };
        k && (!visible || e.visible || e.is_radar_visible) && near.is_none_or(|(p, r)| e.position.distance_2d(p) <= r)
    };
    let mut list: Vec<&Enemy> = ai.war.targets.values().filter(|e| keep(e)).collect();
    list.sort_by(|a, b| b.value.total_cmp(&a.value));
    let total = list.len();
    let frame = ai.frame;
    let rows: Vec<Value> = list
        .into_iter()
        .take(limit)
        .map(|e| {
            json!({"id": e.unit_id, "def": e.def_name, "pos": xz(e.position), "value": r1(e.value),
                   "power": r1(e.power), "seen_ago": frame - e.last_seen, "flags": enemy_flags(e)})
        })
        .collect();
    Ok(json!({"total": total, "enemies": rows}))
}

pub fn threat_at(ai: &mut Ai, a: &Args) -> Out {
    let p = Vec3::xz(a.f32("x")?, a.f32("z")?);
    let w = &ai.war;
    let (x, y) = ((p.x / 64.0).round() as i32, (p.z / 64.0).round() as i32);
    let raw: serde_json::Map<String, Value> =
        MilitaryManager::GRIDS.iter().map(|g| ((*g).to_owned(), r1(w.grid_value(g, x, y).unwrap_or(0.0)))).collect();
    Ok(json!({
        "pos": xz(p),
        "threat": r2(w.get_threat(p)),
        "effective_threat": r2(w.get_effective_threat(p)),
        "tactical_threat": r2(w.get_tactical_threat(p)),
        "porc_threat": r2(w.get_porc_threat(p)),
        "aa_threat": r2(w.get_aa_threat(p)),
        "friendly_threat": r2(w.get_total_friendly_threat(p)),
        "value": r1(w.get_value(p)),
        "our_territory": ai.graph.is_ally_territory(p),
        "enemy_territory": ai.graph.is_enemy_territory(p),
        "front_line": ai.graph.is_front_line(p),
        "raw": raw,
    }))
}

pub fn threat_map(ai: &mut Ai, a: &Args) -> Out {
    let layer = a.opt_str("layer")?.unwrap_or("threat").to_owned();
    let max_size = a.opt_i32("max_size")?.unwrap_or(96).clamp(8, 512);
    let w = &ai.war;
    if w.grid_value(&layer, 0, 0).is_none() {
        return Err(format!("unknown layer '{layer}'; layers: {}", MilitaryManager::GRIDS.join(", ")));
    }
    let (gw, gh) = w.grid_size();
    let step = ((gw.max(gh) + max_size - 1) / max_size).max(1);
    let (cw, ch) = ((gw + step - 1) / step, (gh + step - 1) / step);
    let mut max = 0.0f32;
    let rows: Vec<Vec<i64>> = (0..ch)
        .map(|cy| {
            (0..cw)
                .map(|cx| {
                    let mut v = 0.0f32;
                    for y in cy * step..((cy + 1) * step).min(gh) {
                        for x in cx * step..((cx + 1) * step).min(gw) {
                            v = v.max(w.grid_value(&layer, x, y).unwrap_or(0.0));
                        }
                    }
                    max = max.max(v);
                    v.round() as i64
                })
                .collect()
        })
        .collect();
    Ok(json!({"layer": layer, "cell": 64 * step, "width": cw, "height": ch, "max": r1(max), "rows": rows}))
}

pub fn spots(ai: &mut Ai, _: &Args) -> Out {
    let frame = ai.frame;
    let list: Vec<Value> = ai
        .graph
        .metal_spots
        .iter()
        .map(|s| {
            json!({
                "i": s.index, "pos": xz(s.position), "value": r2(s.value), "owned": s.owned, "hostile": s.hostile,
                "visible": s.visible, "touched": s.touched, "enemy_shadowed": s.enemy_shadowed,
                "ally_shadowed": s.ally_shadowed, "extractor": s.extractor.map(|u| u.id()),
                "seen_ago": if s.last_seen > 0 { json!(frame - s.last_seen) } else { Value::Null },
            })
        })
        .collect();
    Ok(json!({"spots": list}))
}

/// The metal-spot graph's Delaunay links: `[spot, spot, ground]`, ground 1
/// where ground forces travel the link directly (territory shadows, the
/// inference of who holds a spot unseen, spread along these only), 0 where
/// not, null before the terrain model has classified them.
pub fn links_value(ai: &Ai) -> Value {
    let g = &ai.graph;
    let known = g.ground_links.len() == g.links.len();
    Value::Array(
        g.links
            .iter()
            .enumerate()
            .map(|(i, l)| json!([l.v0, l.v1, if known { json!(u8::from(g.ground_links[i])) } else { Value::Null }]))
            .collect(),
    )
}

/// Map size, spots, Delaunay links, bases and (optionally) a coarse height
/// grid.
pub fn map_info_value(ai: &Ai, heights: bool) -> Value {
    let map = ai.cb.map();
    let (hw, hh) = (map.width(), map.height());
    let mut v = json!({
        "name": map.name(),
        "size": [hw * 8, hh * 8],
        "spots": ai.graph.metal_spots.iter().map(|s| json!([s.position.x.round() as i32, s.position.z.round() as i32, r2(s.value)])).collect::<Vec<_>>(),
        "links": links_value(ai),
        "start": opt_xz(ai.graph.get_start_pos()),
        "ally_center": opt_xz(ai.graph.get_ally_center()),
        "enemy_center": opt_xz(ai.graph.get_enemy_center()),
        "map_center": opt_xz(ai.graph.get_map_center()),
    });
    if heights {
        let hm = map.height_map();
        // Cells of 64 elmos (8 height squares), fewer on big maps.
        let step = (8 * ((hw.max(hh) + 8 * 256 - 1) / (8 * 256))).max(8);
        let (cw, ch) = ((hw + step - 1) / step, (hh + step - 1) / step);
        let (lo, hi) = (map.min_height(), map.max_height());
        let span = (hi - lo).max(1.0);
        let at = |x: i32, z: i32| hm.get((z.min(hh - 1) * hw + x.min(hw - 1)) as usize).copied().unwrap_or(0.0);
        let rows: Vec<Vec<u8>> = (0..ch)
            .map(|cz| {
                (0..cw)
                    .map(|cx| {
                        let h = at(cx * step + step / 2, cz * step + step / 2);
                        (((h - lo) / span) * 255.0).round().clamp(0.0, 255.0) as u8
                    })
                    .collect()
            })
            .collect();
        v["heights"] = json!({"cell": step * 8, "width": cw, "height": ch, "min": r1(lo), "max": r1(hi), "rows": rows});
    }
    v
}

pub fn map_info(ai: &mut Ai, a: &Args) -> Out {
    Ok(map_info_value(ai, a.bool_or("heights", true)))
}

pub fn terrain(ai: &mut Ai, _: &Args) -> Out {
    let Some(s) = &ai.strategy else {
        return Ok(json!({"ready": ai.terrain.get().is_some(), "strategy": null}));
    };
    let fortify: Vec<Value> = s
        .fortify
        .iter()
        .map(|f| json!({"choke": f.choke, "pos": xz(f.pos), "width": r1(f.width), "score": r1(f.score), "home_region": f.home, "blocked": f.blocked}))
        .collect();
    let a = &s.analysis;
    Ok(json!({
        "ready": true,
        "strategy": {
            "frame": s.frame,
            "tier": format!("{:?}", s.tier),
            "regions": a.values.len(),
            "ally_base_region": a.ally_base,
            "enemy_base_region": a.enemy_base,
            "sheltered_regions": s.sheltered.iter().filter(|&&b| b).count(),
            "fortify": fortify,
            "fortify_tempo_weight": ai.eco_fortify_tempo(),
            "hold": a.hold,
            "seal": a.seal,
            "walls": a.walls.len(),
            "bridges": a.bridges.len(),
            "drops": a.drops.len(),
            "teleports": a.teleports.len(),
        },
    }))
}

/// The terrain's segmentation for one movement tier: a region per cell
/// (run-length rows), the regions with who holds them, and the chokepoints
/// between them. See the `terrain_map` tool.
pub fn terrain_map(ai: &mut Ai, a: &Args) -> Out {
    let terrain = ai.terrain.get().ok_or("the terrain model is not ready yet")?;
    let strategy_tier = ai.strategy.as_ref().map(|s| s.tier);
    let tier = match a.opt_str("tier")? {
        Some(name) => Tier::ALL
            .into_iter()
            .find(|t| t.name().eq_ignore_ascii_case(name))
            .ok_or_else(|| format!("no tier '{name}' (spider, bot, vehicle, hover, ship, amphib)"))?,
        None => strategy_tier.unwrap_or(Tier::Bot),
    };
    let map = terrain.tier(tier).ok_or_else(|| format!("no model for {}", tier.name()))?;
    let (w, h) = (map.width, map.height);
    let max_size = a.opt_i32("max_size")?.unwrap_or(512).clamp(32, 2048) as usize;
    let step = w.max(h).div_ceil(max_size).max(1);
    let (gw, gh) = (w.div_ceil(step), h.div_ceil(step));
    // Region ids, -1 for obstacles: each output cell takes its middle's.
    let id_at = |x: usize, z: usize| -> i64 {
        let (cx, cz) = ((x * step + step / 2).min(w - 1), (z * step + step / 2).min(h - 1));
        map.region_of_cell(cz * w + cx).map_or(-1, |r| r as i64)
    };
    let mut version: u64 = 0xcbf2_9ce4_8422_2325;
    let mut rows = Vec::with_capacity(gh);
    for z in 0..gh {
        let mut row: Vec<i64> = Vec::new();
        for x in 0..gw {
            let id = id_at(x, z);
            version = (version ^ (id as u64).wrapping_add(x as u64 * 31 + z as u64)).wrapping_mul(0x100_0000_01b3);
            match row.len() {
                n if n >= 2 && row[n - 2] == id => row[n - 1] += 1,
                _ => row.extend([id, 1]),
            }
        }
        rows.push(row);
    }
    let strategy = ai.strategy.as_ref().filter(|s| s.tier == tier);
    let regions: Vec<Value> = map
        .regions
        .iter()
        .map(|r| {
            let mut v = json!({"id": r.id, "center": [r.center.x.round() as i32, r.center.z.round() as i32],
                               "cells": r.cells, "component": r.component});
            if let Some(s) = strategy {
                if let Some(hold) = s.occupancy.regions.get(r.id) {
                    v["holder"] = json!(format!("{:?}", hold.holder()).to_lowercase());
                    v["enemy_power"] = r1(hold.enemy_power);
                    v["ally_power"] = r1(hold.ally_power);
                    v["enemy_value"] = r1(hold.enemy_value);
                    v["ally_value"] = r1(hold.ally_value);
                    v["enemy_aa"] = r1(hold.enemy_aa);
                }
                v["sheltered"] = json!(s.sheltered.get(r.id).copied().unwrap_or(false));
            }
            v
        })
        .collect();
    let chokes: Vec<Value> = map
        .chokes
        .iter()
        .map(|c| {
            let mut v = json!({
                "id": c.id,
                "kind": if c.kind == ChokeKind::Gate { "gate" } else { "border" },
                "pos": [c.pos.x.round() as i32, c.pos.z.round() as i32],
                "width": r1(c.width),
                "gate": c.gate.map(|g| g.map(|p| [p.x.round() as i32, p.z.round() as i32])),
                "regions": c.regions,
                "climb": c.climb.iter().map(|&x| r1(x)).collect::<Vec<_>>(),
            });
            if let Some(s) = strategy {
                v["fortify"] = json!(s.fortify.iter().any(|f| f.choke == c.id));
                v["hold"] = json!(s.analysis.hold.contains(&c.id));
                v["seal"] = json!(s.analysis.seal.contains(&c.id));
            }
            v
        })
        .collect();
    // Each metal spot's region (-1 off the model), for reads that pair the
    // spots with the gates: which gates lie on the ways to our spots.
    let spots: Vec<Value> = (ai.graph.metal_spots.iter().enumerate())
        .map(|(i, s)| {
            let (cx, cz) = ((s.position.x / CELL) as usize, (s.position.z / CELL) as usize);
            let region = if cx < w && cz < h { map.region_of_cell(cz * w + cx).map_or(-1, |r| r as i64) } else { -1 };
            json!([i, s.position.x.round() as i32, s.position.z.round() as i32, region])
        })
        .collect();
    let mut v = json!({
        "tier": tier.name(),
        "strategy_tier": strategy_tier.map(|t| t.name()),
        "tiers": terrain.tiers().iter().map(|m| m.tier.name()).collect::<Vec<_>>(),
        "cell": (CELL as usize * step),
        "width": gw,
        "height": gh,
        "grid_version": version.to_string(),
        "regions": regions,
        "chokes": chokes,
        "spots": spots,
        "start": opt_xz(ai.graph.get_start_pos()),
    });
    if a.bool_or("grid", true) {
        v["rows"] = json!(rows);
    }
    Ok(v)
}

pub fn events(ai: &mut Ai, a: &Args) -> Out {
    let kinds: Vec<String> =
        a.opt_str("kinds")?.map(|k| k.split(',').map(|s| s.trim().to_owned()).collect()).unwrap_or_default();
    let grep = a.opt_str("grep")?.map(str::to_owned);
    let limit = a.opt_i32("limit")?.unwrap_or(100).clamp(1, 5000) as usize;
    // Without kinds, the narrative: no ticks and snapshots.
    let keep = |r: &super::bus::Record| {
        // Spectator chat is never shown (see `op_observe`).
        let kind_ok = r.kind != "specchat"
            && if kinds.is_empty() { r.kind != "tick" && r.kind != "snap" } else { kinds.contains(&r.kind) };
        kind_ok && grep.as_ref().is_none_or(|g| r.line.contains(g.as_str()))
    };
    let records = match a.opt_i32("since")? {
        Some(since) => {
            let mut out = Vec::new();
            for r in ai.op.bus.since(since.max(0) as u64, usize::MAX) {
                if keep(&r) {
                    out.push(r);
                    if out.len() == limit {
                        break;
                    }
                }
            }
            out
        }
        None => ai.op.bus.tail(limit, keep),
    };
    let list: Vec<Value> = records.iter().filter_map(|r| serde_json::from_str(&r.line).ok()).collect();
    Ok(json!({"next": ai.op.bus.next_seq(), "on": ai.op.is_on(), "events": list}))
}

fn def_summary(ai: &Ai, d: UnitDef) -> Value {
    json!({
        "name": ai.defs.name(d),
        "human": d.human_name(),
        "cost": r1(d.cost(ai.metal)),
        "health": r1(d.health()),
        "speed": r1(d.speed()),
        "range": r1(d.max_weapon_range()),
        "building": d.speed() <= 0.0,
        "builder": d.is_builder(),
    })
}

pub fn defs(ai: &mut Ai, a: &Args) -> Out {
    if let Some(name) = a.opt_str("name")? {
        let d = ai.defs.get(name).ok_or_else(|| format!("no unit def '{name}'"))?;
        let mut v = def_summary(ai, d);
        v["tooltip"] = json!(d.tooltip());
        v["build_time"] = r1(d.build_time());
        v["build_speed"] = r1(d.build_speed());
        v["los"] = r1(d.los_radius());
        v["radar"] = json!(d.radar_radius());
        v["cloak"] = json!(d.is_able_to_cloak());
        v["flies"] = json!(d.is_able_to_fly());
        v["size"] = json!([d.x_size() * 8, d.z_size() * 8]);
        v["weapons"] = json!(
            d.weapon_mounts()
                .iter()
                .filter_map(|m| m.weapon_def())
                .map(|w| json!({"name": w.name(), "range": r1(w.range())}))
                .collect::<Vec<_>>()
        );
        v["builds"] = json!(d.build_options().iter().map(|b| ai.defs.name(*b)).collect::<Vec<_>>());
        return Ok(v);
    }
    let search = a.opt_str("search")?.unwrap_or("").to_lowercase();
    let mut list: Vec<UnitDef> = ai
        .defs
        .all()
        .filter(|d| ai.defs.name(*d).contains(&search) || d.human_name().to_lowercase().contains(&search))
        .collect();
    list.sort_by_key(|d| ai.defs.name(*d).to_owned());
    let total = list.len();
    let rows: Vec<Value> = list.into_iter().take(60).map(|d| def_summary(ai, d)).collect();
    Ok(json!({"total": total, "defs": rows}))
}

/// Where a unit of ours means to go, and why: a worker's task, or the
/// target of a fighter outside squads (squads have their own target).
fn intent(ai: &Ai, id: i32) -> Value {
    if let Some(t) = ai.eco.workers.get(&id).and_then(|w| w.task).and_then(|t| ai.eco.tasks.get(t)) {
        let p = t.get_pos();
        let what = match &t.kind {
            TaskKind::Construction(c) => ai.defs.name(c.build_type),
            TaskKind::Reclaim(_) => "reclaim",
            TaskKind::Repair(_) => "repair",
            TaskKind::CombatReclaim(_) => "combat_reclaim",
        };
        return json!([p.x.round() as i32, p.z.round() as i32, what]);
    }
    match ai.fighters.get(&id) {
        Some(f) if f.squad.is_none() && f.target != NULLPOS => {
            json!([f.target.x.round() as i32, f.target.z.round() as i32, Value::Null])
        }
        _ => Value::Null,
    }
}

/// Everything's position, compactly (the dashboard's map).
pub fn snapshot_value(ai: &Ai) -> Value {
    let units: Vec<Value> = ai
        .cb
        .team_units()
        .into_iter()
        .map(|u| {
            let max = u.max_health();
            let hp = if max > 0.0 { (u.health() / max * 100.0).round() as i32 } else { 0 };
            let p = u.pos();
            json!([
                u.id(),
                ai.defs.name_of(u),
                p.x.round() as i32,
                p.z.round() as i32,
                hp,
                role(ai, u),
                ai.fighters.get(&u.id()).and_then(|f| f.squad),
                intent(ai, u.id())
            ])
        })
        .collect();
    let frame = ai.frame;
    let enemies: Vec<Value> = ai
        .war
        .targets
        .values()
        .map(|e| {
            let mut flags = 0u32;
            for (i, on) in [
                e.visible,
                e.is_radar_visible,
                e.is_static,
                e.is_porc,
                e.is_mex,
                e.is_com,
                e.is_worker,
                e.is_aa,
                e.can_fly(),
                e.identified,
            ]
            .into_iter()
            .enumerate()
            {
                flags |= u32::from(on) << i;
            }
            json!([
                e.unit_id,
                e.def_name,
                e.position.x.round() as i32,
                e.position.z.round() as i32,
                frame - e.last_seen,
                flags,
                e.value.round() as i32,
                e.threat_radius.round() as i32
            ])
        })
        .collect();
    let squads: Vec<Value> = ai
        .squads
        .ids()
        .into_iter()
        .filter_map(|id| ai.squads.get(id))
        .map(|s| {
            let p = s.get_pos(&ai.fighters);
            let t = ai.op_squad_target(s.id).unwrap_or(s.target);
            json!([
                s.id,
                squad_label(s),
                squad_status(s.status),
                p.x.round() as i32,
                p.z.round() as i32,
                t.x.round() as i32,
                t.z.round() as i32,
                s.fighters.len(),
                s.metal_value.round() as i32,
                squad_tactic(s)
            ])
        })
        .collect();
    let spots: Vec<u8> = ai
        .graph
        .metal_spots
        .iter()
        .map(|s| {
            if s.owned {
                1
            } else if s.hostile {
                2
            } else if s.enemy_shadowed {
                3
            } else if s.ally_shadowed {
                4
            } else {
                0
            }
        })
        .collect();
    let defend: Vec<Value> = ai
        .war
        .defense_targets()
        .map(|(d, predicted)| {
            json!([d.position.x.round() as i32, d.position.z.round() as i32, d.damage.round() as i32, predicted])
        })
        .collect();
    let e = ai.cb.economy();
    let res = |r: Resource| json!([r1(e.current(r)), r1(e.income(r)), r1(e.usage(r)), r1(e.storage(r))]);
    json!({
        "airstrikes": ai.airstrikes_status(),
        "frame": frame,
        "units": units,
        "enemies": enemies,
        "squads": squads,
        "spots": spots,
        "defend": defend,
        "metal": res(ai.metal),
        "energy": res(ai.energy),
        "legend": {
            "units": ["id", "def", "x", "z", "health%", "role", "squad", "intent [x, z, what] or null"],
            "enemies": ["id", "def", "x", "z", "frames since seen", "flags", "value", "threat radius"],
            "enemy_flags": ["visible", "radar", "static", "porc", "mex", "com", "worker", "aa", "air", "identified"],
            "squads": ["id", "label", "status", "x", "z", "target x", "target z", "members", "value", "tactic"],
            "spots": ["free", "ours", "enemy", "enemy territory", "our territory"],
            "defend": ["x", "z", "weight", "predicted"],
            "resources": ["current", "income", "usage", "storage"],
        },
    })
}

pub fn snapshot(ai: &mut Ai, _: &Args) -> Out {
    Ok(snapshot_value(ai))
}

/// Per-second numbers for the stream.
pub fn tick_value(ai: &Ai) -> Value {
    let e = ai.cb.economy();
    let res = |r: Resource| json!([r1(e.current(r)), r1(e.income(r)), r1(e.usage(r)), r1(e.storage(r))]);
    json!({
        "m": res(ai.metal),
        "e": res(ai.energy),
        "income": r1(ai.eco.effective_income),
        "army": ai.fac.army_value.round() as i64,
        "enemy_army": ai.war.enemy_fighter_value.round() as i64,
        "enemy_porc": ai.war.enemy_porc_value.round() as i64,
        "fighters": ai.fighters.len(),
        "workers": ai.eco.workers.len(),
        "squads": ai.squads.ids().len(),
        "territory": r2(ai.graph.territory_fraction),
        "spots": [ai.graph.get_owned_spots().len(), ai.graph.get_enemy_spots().len()],
        "produced": ai.intel.produced.round() as i64,
        "unaccounted": ai.intel.unaccounted().round() as i64,
        "density": ai.posture.density.round() as i64,
        "laneness": r2(ai.posture.laneness()),
        "share": r2(ai.posture.share),
    })
}

pub fn overrides_value(ai: &Ai) -> Value {
    let o = &ai.op.over;
    let frame = ai.frame;
    json!({
        "reserved": reserved::reserved(ai.cb),
        "fac_orders": o.fac_orders.iter().map(|f| json!({"unit": f.unit, "left": f.left, "factory": f.factory})).collect::<Vec<_>>(),
        "requisition": o.requisition,
        "priority_tasks": o.priority_tasks,
        "defend": o.defend.iter().map(|d| json!({"pos": xz(d.pos), "weight": r1(d.weight), "seconds_left": (d.until - frame) / 30})).collect::<Vec<_>>(),
        "squad_targets": o.squad_targets.iter().map(|(s, (p, until))| json!({"squad": s, "pos": xz(*p), "seconds_left": (until - frame) / 30})).collect::<Vec<_>>(),
        "squad_intents": o.squad_intents.iter().map(|(s, i)| json!({"squad": s, "intent": i.kind.name(), "pos": xz(i.pos), "seconds_left": (i.until - frame) / 30})).collect::<Vec<_>>(),
        "features_off": ai.features_off(),
        "dials": {"greed": ai.op_greed(), "line": ai.op.over.line, "buildpower": ai.op_buildpower()},
        "porc_zones": o.porc_zones.iter().map(|z| json!({"pos": xz(z.pos), "radius": r1(z.radius), "seconds_left": (z.until - frame) / 30})).collect::<Vec<_>>(),
    })
}

pub fn overrides(ai: &mut Ai, _: &Args) -> Out {
    Ok(overrides_value(ai))
}

/// The board as a PNG: `image_png_base64` becomes an image content block
/// on the MCP side (see `server::rpc`).
pub fn board(ai: &mut Ai, _: &Args) -> Out {
    let png = ai.board_png_base64();
    if png.is_empty() {
        return Err("could not draw the board".into());
    }
    Ok(json!({"time": game_time(ai.frame), "image_png_base64": png,
              "legend": "grid in thousands of elmos; metal spots: ours blue, enemy red, free white; commander yellow, workers green, fighters cyan, buildings blue; enemies red (orange when remembered), enemy defenses with their reach; squad lines cyan; defended places magenta"}))
}

/// The AI's modules, as the `mute` tool names them, with what each decides.
pub const MODULES: &[(&str, &str)] = &[
    (
        "eco",
        "the economy: workers and their construction, reclaim and repair jobs, mexes, energy, turrets by mexes and links, terraforming",
    ),
    ("fac", "the factories: what each builds next, and which factory the economy wants"),
    ("logistics", "the project tracker: crews for valued construction jobs (no orders of its own)"),
    ("war", "military bookkeeping: enemy tracking, threat maps, targets, mobile anti-air"),
    ("raider", "raiders: solo and squad raids on the enemy's economy, their retreats"),
    ("squad", "assault, shield, hover and air squads: forming, rallying, attacking"),
    ("retreat", "hurt units going home to repair"),
    ("misc", "support units, loners, striders, artillery, nukes' targets"),
    ("bomber", "airstrike missions, bomber sorties and fighter interception"),
    ("scouts", "scouting runs"),
    ("drops", "Charon air drops"),
    ("teleport", "Djinn teleports"),
    ("nukes", "nuke firing"),
    ("missiles", "tactical missile silos"),
    ("strikes", "dedicated assassination waves and anti-Phantom support fire"),
    ("precision", "precision raids behind lone towers"),
    ("jaeger", "jaeger squads (hunting raiders)"),
    ("qrf", "the quick-reaction force: home defense against raids (dedicated teams, moonlighting squads)"),
    ("jumps", "jumpbots' jumps"),
    ("puppies", "Puppies"),
    ("water", "under-water units"),
    ("airIntel", "anti-air placed from hits on our aircraft (no orders)"),
    ("intel", "the estimate of enemy production (no orders)"),
    ("terrain", "the terrain model and strategic analysis (no orders)"),
    ("graph", "the metal-spot graph and territory (no orders)"),
];

/// The modules with the orders each issued and had dropped, and the mutes.
pub fn modules(ai: &mut Ai, _: &Args) -> Out {
    let stats = reserved::order_stats(ai.cb);
    let muted = reserved::muted(ai.cb);
    let list: Vec<Value> = MODULES
        .iter()
        .map(|(name, what)| {
            let (issued, dropped, foreign) = stats
                .iter()
                .filter(|(h, _)| *h == *name || h.starts_with(&format!("{name}.")))
                .fold((0, 0, 0), |(i, d, f), (_, s)| (i + s.issued, d + s.dropped, f + s.foreign));
            let handlers: Vec<Value> = stats
                .iter()
                .filter(|(h, _)| *h == *name || h.starts_with(&format!("{name}.")))
                .map(|(h, s)| json!({"handler": h, "issued": s.issued, "dropped": s.dropped, "foreign": s.foreign}))
                .collect();
            json!({"module": name, "does": what, "muted": reserved::is_muted(ai.cb, name),
                   "orders_issued": issued, "orders_dropped": dropped, "orders_foreign": foreign, "handlers": handlers})
        })
        .collect();
    let other: Vec<Value> = stats
        .iter()
        .filter(|(h, _)| !MODULES.iter().any(|(m, _)| h == m || h.starts_with(&format!("{m}."))))
        .map(|(h, s)| json!({"handler": h, "issued": s.issued, "dropped": s.dropped, "foreign": s.foreign}))
        .collect();
    Ok(json!({"modules": list, "other_handlers": other, "muted": muted,
              "note": "muted modules keep their bookkeeping; only their orders to units are dropped (orders_dropped counts them, orders to reserved units, and orders_foreign: to units another module owns, see military::roles)"}))
}

/// The `n` most expensive handlers, and panics.
pub fn profile_value(ai: &Ai, n: usize) -> Value {
    let mut rows: Vec<_> = ai.profile.iter().collect();
    rows.sort_by_key(|(_, t)| std::cmp::Reverse(t.total));
    let frames = ai.frame.max(1) as f64;
    let total: f64 = ai.profile.values().filter(|t| !t.nested).map(|t| t.total.as_secs_f64()).sum();
    json!({
        "ms_per_frame": (total * 1e3 / frames * 1000.0).round() / 1000.0,
        "handlers": rows.into_iter().take(n).map(|(what, t)| json!({
            "what": what,
            "calls": t.calls,
            "total_ms": (t.total.as_secs_f64() * 1e4).round() / 10.0,
            "max_ms": (t.max.as_secs_f64() * 1e4).round() / 10.0,
            "nested": t.nested,
        })).collect::<Vec<_>>(),
        "panics": ai.panics,
    })
}

pub fn profile(ai: &mut Ai, _: &Args) -> Out {
    Ok(profile_value(ai, 40))
}

#[cfg(test)]
mod fire_labels_tests {
    use super::*;
    use crate::military::fire_squad::Mode;
    #[test]
    fn labels_follow_adoption_and_release_without_changing_kind() {
        let mut s = Squad::new(1, SquadKind::Assault, 0);
        s.status = 'a';
        assert_eq!((squad_label(&s), squad_tactic(&s)), ("assault", None));
        for (mode, label) in [(Mode::Kite, "kiting"), (Mode::Evade, "evading"), (Mode::Commit, "committing")] {
            s.fire.mode = mode;
            assert_eq!((squad_label(&s), squad_tactic(&s)), ("fireteam", Some(label)));
            assert_eq!(squad_kind(s.kind), "assault");
        }
        s.fire = Default::default();
        assert_eq!((squad_label(&s), squad_tactic(&s)), ("assault", None));
        s.kind = SquadKind::Taskforce;
        s.fire.mode = Mode::Kite;
        assert_eq!(squad_label(&s), "taskforce");
    }
}
