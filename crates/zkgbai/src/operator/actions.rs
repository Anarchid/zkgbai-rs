//! Acting tools: what the operator tells the AI (or its units) to do.
//!
//! Each call is journaled as an `op` record with its arguments and result.

use serde_json::json;
use springai::{Event, Unit, Vec3, opt, reserved};

use super::tools::Args;
use super::views::{Out, xz};
use super::{DefendPoint, FacOrder};
use crate::Ai;
use crate::military::{DefenseTarget, SquadKind};

/// Features the `disable` switch knows (see `Ai::dev_off`).
const FEATURES: [&str; 17] = [
    "porc",
    "reach",
    "refuge",
    "teleport",
    "drops",
    "ditch",
    "reanalysis",
    "intel",
    "grid",
    "precision",
    "nukes",
    "missiles",
    "scouts",
    "radar",
    "requisition",
    "colonize",
    "aaintel",
];

/// A ground point at (x, z).
fn ground(ai: &Ai, x: f32, z: f32) -> Vec3 {
    Vec3::new(x, ai.cb.map().elevation_at(x, z), z)
}

fn point(ai: &Ai, a: &Args) -> Result<Option<Vec3>, String> {
    Ok(match (a.opt_f32("x")?, a.opt_f32("z")?) {
        (Some(x), Some(z)) => Some(ground(ai, x, z)),
        (None, None) => None,
        _ => return Err("give both x and z".into()),
    })
}

/// One of our units by id.
fn ours(ai: &Ai, id: i32) -> Option<Unit> {
    Unit::from_id(ai.cb, id).filter(|u| u.team() == ai.team_id && u.max_health() > 0.0)
}

pub fn say(ai: &mut Ai, a: &Args) -> Out {
    let text: String = a.str("text")?.chars().filter(|c| !c.is_control()).take(240).collect();
    ai.say(&text);
    Ok(json!({"said": text}))
}

pub fn mark(ai: &mut Ai, a: &Args) -> Out {
    let p = ground(ai, a.f32("x")?, a.f32("z")?);
    match (a.opt_f32("to_x")?, a.opt_f32("to_z")?) {
        (Some(x), Some(z)) => {
            let to = ground(ai, x, z);
            ai.cb.map().add_line(p, to);
            Ok(json!({"line": [xz(p), xz(to)]}))
        }
        _ => {
            let label = a.opt_str("label")?.unwrap_or("");
            ai.marker(p, label);
            Ok(json!({"marker": xz(p), "label": label}))
        }
    }
}

pub fn note(ai: &mut Ai, a: &Args) -> Out {
    let text = a.str("text")?.to_owned();
    let tag = a.opt_str("tag")?.map(str::to_owned);
    let seq = ai.op.bus.next_seq();
    ai.op_emit("note", json!({"text": text, "tag": tag}));
    Ok(json!({"seq": seq, "journaled": ai.op.journal_path.is_some()}))
}

pub fn feature(ai: &mut Ai, a: &Args) -> Out {
    let name = a.str("name")?.trim().to_owned();
    let off = a.bool_or("off", true);
    if !a.has("off") {
        return Err("'off' is required".into());
    }
    ai.set_feature_off(&name, off);
    let mut v = json!({"features_off": ai.features_off()});
    if !FEATURES.contains(&name.as_str()) {
        v["warning"] = json!(format!("'{name}' is not a known feature: {}", FEATURES.join(", ")));
    }
    Ok(v)
}

pub fn fac_queue(ai: &mut Ai, a: &Args) -> Out {
    if a.bool_or("clear", false) {
        let n = ai.op.over.fac_orders.len();
        ai.op.over.fac_orders.clear();
        return Ok(json!({"cleared": n}));
    }
    let name = a.str("unit")?.to_owned();
    let def = ai.defs.get(&name).ok_or_else(|| format!("no unit def '{name}'"))?;
    let count = a.opt_i32("count")?.unwrap_or(1).clamp(1, 200) as u32;
    let factory = a.opt_i32("factory")?;
    if let Some(f) = factory
        && !ai.fac.factories.contains_key(&f)
    {
        return Err(format!("{f} is not one of our factories"));
    }
    let builders: Vec<i32> = ai
        .fac
        .factories
        .values()
        .filter(|f| factory.is_none_or(|id| id == f.id))
        .filter(|f| f.unit.def().is_some_and(|d| d.build_options().iter().any(|b| b.id() == def.id())))
        .map(|f| f.id)
        .collect();
    ai.op.over.fac_orders.push(FacOrder { unit: name.clone(), left: count, factory });
    let mut v = json!({"queued": name, "count": count, "factories_that_build_it": builders});
    if builders.is_empty() {
        v["warning"] = json!("no factory of ours builds it now; the order waits for one");
    }
    Ok(v)
}

pub fn requisition(ai: &mut Ai, a: &Args) -> Out {
    if a.bool_or("clear", false) {
        let had = ai.op.over.requisition.take();
        return Ok(json!({"cleared": had}));
    }
    let name = a.str("factory")?.to_owned();
    let def = ai.defs.get(&name).ok_or_else(|| format!("no unit def '{name}'"))?;
    if def.speed() > 0.0 || def.build_options().is_empty() {
        return Err(format!("'{name}' is not a factory"));
    }
    ai.op.over.requisition = Some(name.clone());
    let now = ai.op_requisition();
    if let Some(r) = &now {
        ai.eco.requisition = Some(r.clone());
    }
    Ok(json!({"requisition": name, "pending": now.is_some()}))
}

pub fn build(ai: &mut Ai, a: &Args) -> Out {
    let name = a.str("unit")?.to_owned();
    let def = ai.defs.get(&name).ok_or_else(|| format!("no unit def '{name}'"))?;
    if def.speed() > 0.0 {
        return Err(format!("'{name}' is not a building"));
    }
    let pos = ground(ai, a.f32("x")?, a.f32("z")?);
    let facing = a.opt_i32("facing")?.unwrap_or(0).clamp(0, 3);
    let (task, site) = ai.eco_operator_build(def, pos, facing)?;
    let priority = a.bool_or("priority", true);
    if priority {
        ai.op.over.priority_tasks.insert(task);
    }
    Ok(json!({"task": task, "site": xz(site), "priority": priority}))
}

pub fn defend(ai: &mut Ai, a: &Args) -> Out {
    if a.bool_or("clear", false) {
        let n = ai.op.over.defend.len();
        ai.op.over.defend.clear();
        return Ok(json!({"cleared": n}));
    }
    let pos = point(ai, a)?.ok_or("give x and z (or clear)")?;
    let weight = a.f32_or("weight", 2000.0)?.max(0.0);
    let seconds = a.f32_or("seconds", 60.0)?.clamp(1.0, 3600.0);
    let until = ai.frame + (seconds * 30.0) as i32;
    ai.op.over.defend.push(DefendPoint { pos, weight, until });
    ai.war.refresh_defense_target(DefenseTarget::new(pos, weight, ai.frame));
    Ok(json!({"defend": xz(pos), "weight": weight, "seconds": seconds, "points": ai.op.over.defend.len()}))
}

pub fn squad_target(ai: &mut Ai, a: &Args) -> Out {
    let sid = a.i32("squad")?;
    let sid = u32::try_from(sid).map_err(|_| "bad squad id")?;
    if !ai.squads.contains(sid) {
        return Err(format!("no squad {sid} (see squads)"));
    }
    if a.bool_or("clear", false) {
        let had = ai.op.over.squad_targets.remove(&sid).is_some();
        return Ok(json!({"squad": sid, "released": had}));
    }
    let pos = point(ai, a)?.ok_or("give x and z (or clear)")?;
    let seconds = a.f32_or("seconds", 60.0)?.clamp(1.0, 3600.0);
    ai.op.over.squad_targets.insert(sid, (pos, ai.frame + (seconds * 30.0) as i32));
    if ai.squads.get(sid).is_some_and(|s| s.kind == SquadKind::Raider) {
        ai.raider_squad_raid(sid, pos);
    } else {
        ai.squad_set_target(sid, pos);
    }
    Ok(json!({"squad": sid, "target": xz(pos), "seconds": seconds}))
}

pub fn take(ai: &mut Ai, a: &Args) -> Out {
    let (mut taken, mut refused) = (Vec::new(), Vec::new());
    for id in a.ids("units")? {
        match ours(ai, id) {
            Some(u) => {
                reserved::reserve(ai.cb, id);
                reserved::as_owner(|| u.stop(0));
                taken.push(id);
            }
            None => refused.push(id),
        }
    }
    Ok(json!({"taken": taken, "not_ours": refused, "reserved": reserved::reserved(ai.cb).len()}))
}

pub fn release(ai: &mut Ai, a: &Args) -> Out {
    let ids = if a.bool_or("all", false) { reserved::reserved(ai.cb) } else { a.ids("units")? };
    let mut released = Vec::new();
    for id in ids {
        if reserved::release(ai.cb, id) {
            released.push(id);
            // The AI picks it up as it does any unit gone idle.
            if let Some(unit) = ours(ai, id) {
                ai.dispatch(Event::UnitIdle { unit });
            }
        }
    }
    Ok(json!({"released": released, "reserved": reserved::reserved(ai.cb).len()}))
}

pub fn order(ai: &mut Ai, a: &Args) -> Out {
    let ids = a.ids("units")?;
    let cmd = a.str("cmd")?.to_owned();
    let opts = if a.bool_or("queue", false) { opt::SHIFT_KEY } else { opt::NONE };
    let pos = point(ai, a)?;
    let target = match a.opt_i32("target")? {
        Some(t) => Some(Unit::from_id(ai.cb, t).ok_or("bad target id")?),
        None => None,
    };
    let radius = a.f32_or("radius", 300.0)?;
    let value = a.opt_i32("value")?;
    let build = match a.opt_str("unit")? {
        Some(n) => Some(ai.defs.get(n).ok_or_else(|| format!("no unit def '{n}'"))?),
        None => None,
    };
    let facing = a.opt_i32("facing")?.unwrap_or(0).clamp(0, 3);
    let need_pos = || pos.ok_or_else(|| format!("'{cmd}' needs x and z"));
    let need_target = || target.ok_or_else(|| format!("'{cmd}' needs a target"));
    let need_value = || value.ok_or_else(|| format!("'{cmd}' needs a value"));
    // Check the arguments once, before any unit moves.
    match cmd.as_str() {
        "move" | "fight" | "patrol" | "area_attack" | "area_reclaim" => {
            need_pos()?;
        }
        "attack" if target.is_none() => {
            need_pos()?;
        }
        "guard" | "repair" | "reclaim" => {
            need_target()?;
        }
        "build" => {
            need_pos()?;
            build.ok_or("'build' needs a unit def")?;
        }
        "fire_state" | "move_state" | "cloak" | "on_off" => {
            need_value()?;
        }
        "attack" | "stop" | "wait" | "self_destruct" => {}
        other => return Err(format!("unknown command '{other}'")),
    }
    let take = a.bool_or("take", true);
    let (mut accepted, mut refused, mut not_ours) = (Vec::new(), Vec::new(), Vec::new());
    for id in ids {
        let Some(u) = ours(ai, id) else {
            not_ours.push(id);
            continue;
        };
        if take {
            reserved::reserve(ai.cb, id);
        }
        let map = ai.cb.map();
        let ok = reserved::as_owner(|| match cmd.as_str() {
            "move" => u.move_to(pos.unwrap_or_default(), opts),
            "fight" => u.fight(pos.unwrap_or_default(), opts),
            "patrol" => u.patrol_to(pos.unwrap_or_default(), opts),
            "attack" => match target {
                Some(t) => u.attack(t, opts),
                None => u.attack_area(pos.unwrap_or_default(), 0.0, opts),
            },
            "area_attack" => u.attack_area(pos.unwrap_or_default(), radius, opts),
            "guard" => target.is_some_and(|t| u.guard(t, opts)),
            "repair" => target.is_some_and(|t| u.repair(t, opts)),
            "reclaim" => target.is_some_and(|t| u.reclaim_unit(t, opts)),
            "area_reclaim" => u.reclaim_area(pos.unwrap_or_default(), radius, opts),
            "build" => build.is_some_and(|d| {
                let site = map.find_closest_build_site(d, pos.unwrap_or_default(), 300.0, 3, facing);
                u.build(d, site, facing, opts)
            }),
            "stop" => u.stop(opts),
            "wait" => u.wait(opts),
            "fire_state" => u.set_fire_state(value.unwrap_or(2).clamp(0, 2), opts),
            "move_state" => u.set_move_state(value.unwrap_or(1).clamp(0, 2), opts),
            "cloak" => u.cloak(value.unwrap_or(1) != 0, opts),
            "on_off" => u.set_on_off(value.unwrap_or(1) != 0, opts),
            "self_destruct" => u.self_destruct(opts),
            _ => false,
        });
        if ok { accepted.push(id) } else { refused.push(id) }
    }
    Ok(json!({"cmd": cmd, "accepted": accepted, "refused_by_engine": refused, "not_ours": not_ours,
              "reserved": take}))
}
