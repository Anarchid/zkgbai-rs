//! Acting tools: what the operator tells the AI (or its units) to do.
//!
//! Each call is journaled as an `op` record with its arguments and result.

use serde_json::{Value, json};
use springai::{Event, NO_TIMEOUT, Unit, Vec3, opt, reserved};

/// Zero-K `LuaRules/Gadgets/unit_field_factory.lua`: copy a factory's
/// build list and pick the unit to build.
const CMD_FIELD_FAC_QUEUELESS: i32 = 38695;

use super::hud::{self, SECTIONS};
use super::tools::Args;
use super::views::{self, Out, r1, xz};
use super::{DefendPoint, FacOrder, Intent, IntentKind, PorcZone};
use crate::Ai;
use crate::logistics::{Kind, Worth};
use crate::military::{DefenseTarget, Fighter, SquadKind};
use crate::util::NULLPOS;

/// Features the `disable` switch knows (see `Ai::dev_off`): name, the group
/// a UI files it under, and what it does. Those off by default (`enable`
/// turns them on) are in `crate::ai::OFF_BY_DEFAULT`. Most act on the AI's
/// next decision; a few only where it decides once (the lanes, the doctrine
/// set).
pub(crate) const FEATURES: &[(&str, &str, &str)] = &[
    ("logistics", "Economy", "the project tracker: construction valued by when it finishes, crews forecast"),
    ("clients", "Economy", "pylons, requisitioned factories and anti-nukes tracked as valued issues"),
    ("pause", "Economy", "an urgent issue holds back the metal of other construction"),
    ("currency", "Economy", "a job's raid danger priced in metal against its crew's bonus"),
    ("pairs", "Economy", "Constables pair up on builds"),
    ("earlycons", "Economy", "an early second Constable in the opening"),
    ("raidmem", "Economy", "workers avoid jobs near raiders seen lately, not only now"),
    ("greedcap", "Economy", "greed held at x1 while energy stalls"),
    ("requisition", "Economy", "modules ask for the factories they need"),
    ("colonize", "Economy", "colonists sent to far free metal"),
    ("grid", "Energy", "the overdrive grid planner: pylons link buildings that need power"),
    ("gridmex", "Energy", "late pylons link the mexes by overdrive gained per pylon (off: pylons near the plants)"),
    ("odreserve", "Energy", "energy kept above mex income for overdrive"),
    ("odphase", "Energy", "the overdrive phase by energy, free spots and fusions (off: two fusions)"),
    ("ecoladder", "Energy", "fusions and singularities opened by income or the map's density"),
    ("stallgate", "Energy", "no storage or superweapon while energy stalls"),
    ("storagegate", "Energy", "storages wait for energy first"),
    ("wind", "Energy", "turbines where they beat solars locally"),
    ("geo", "Energy", "geothermal plants on our vents"),
    ("advgeo", "Energy", "advanced geothermal in place of a singularity"),
    ("geobox", "Energy", "a guard box around an advanced geothermal"),
    ("posture", "Defense", "the defense posture: a fixed-defense budget from lanes and density"),
    ("turretcredit", "Defense", "the home defense counts the posture's turrets against raids"),
    ("porc", "Defense", "no ground defenses in regions the enemy's ground army cannot reach"),
    ("mexporc", "Defense", "a turret by every mex and link"),
    ("ditch", "Defense", "ditches across chokepoints we fortify"),
    (
        "fortify",
        "Defense",
        "shared gate defenses and ground-route leyline priorities, with distance, density and constructor support checks (off: legacy gate planning)",
    ),
    (
        "detour",
        "Defense",
        "ditches by how much longer the enemy's way to our metal gets, priced against workers' jobs; gate turrets priced as light porc; chokepoints held for our own metal only",
    ),
    ("refuge", "Defense", "workers flee raids to refuges ahead of them"),
    ("qrf", "Defense", "the home defense force answering raids"),
    (
        "garrison",
        "Defense",
        "the home defense's floor: a guard per few naked mexes, posted ahead of its sector's first mex",
    ),
    ("stakeouts", "Defense", "scout pickets on the raid lanes"),
    ("anyscout", "Defense", "stakeouts take any factory's scout"),
    (
        "openscouts",
        "Defense",
        "a 1v1 jump, tank or ship opening builds a 200-metal wave of scouts first, then one per 8 raiders",
    ),
    ("firefighters", "Defense", "airborne quick-reaction squads against raids on our eco"),
    ("airfire", "Defense", "an air factory for the firefighters"),
    ("airtempo", "Army", "an air factory weighed up by the clock and the map's size"),
    ("permanence", "Army", "unseen enemy fighters kept on their likely way in"),
    ("reach", "Army", "reachability checks by the terrain model"),
    ("precision", "Army", "raiders shoot from where defenses cannot shoot back"),
    ("squadhyst", "Army", "raider squads keep their rally point a while"),
    ("justenough", "Army", "one raider claim per target; QRF teams sized to the raid"),
    ("venomraid", "Army", "Venoms raid after the opening, Fleas left to the stakeouts"),
    (
        "venomstand",
        "Army",
        "raiding Venoms shoot from inside their range, not from on top of their target (their stun blast hits our own)",
    ),
    ("jumps", "Army", "jumpjets jump"),
    ("lobsters", "Army", "optional Lobster squad throws and one launcher per Grizzly; default on"),
    ("jumppath", "Army", "jumpjets' ways jump cliffs they would walk round"),
    ("puppies", "Army", "Puppies herded onto wreck fields"),
    ("impalers", "Army", "Impaler batteries against what stands still"),
    ("escorts", "Army", "escorts for Impaler batteries (the impaler doctrine)"),
    ("lance", "Army", "Lances in taskforces with a Mace guarding each (the lance doctrine)"),
    (
        "ravager",
        "Army",
        "Ravager packs: in formation, always moving, passing over what they dive, a Ripper screen behind",
    ),
    ("minotaur", "Army", "Minotaur packs, from one Minotaur, an Ogre screen behind"),
    ("scorcher", "Army", "Scorcher packs (the Ravagers' conduct, from the start, no screen)"),
    ("dagger", "Army", "Dagger packs (the Ravagers' conduct, from the start, no screen)"),
    ("blitz", "Army", "Blitz packs (the Ravagers' conduct, from the start, no screen)"),
    ("jaeger", "Army", "anti-heavy hunters once enemy heavies show"),
    ("widowcap", "Army", "jaeger Widows kept to a quarter of the rest of the army's metal"),
    ("pounce", "Army", "jaeger Skuttles jump on any heavy in reach, their mark or not"),
    ("lead", "Army", "jaeger Skuttles jump where their heavy will be when they land"),
    ("ambush", "Army", "jaegers lie in wait on a heavy's way; Widows stun only where the army can finish"),
    ("dodge", "Army", "cloaked jaegers step (Skuttles jump) out of sight of enemies coming their way"),
    ("jaegerreserve", "Army", "the energy reserve asked of the game covers the jaegers' cloaks"),
    ("strikes", "Army", "assassination strikes on Phantoms and Lances"),
    ("fightshadow", "Army", "the fight predictor runs and logs, not acting"),
    ("firesquads", "Army", "skirmisher-heavy assault squads keep formation and range, evade or commit together"),
    ("fightsim", "Army", "the fight predictor drives squads"),
    ("fightmech", "Army", "the fight predictor's mechanics model runs beside it, logged only"),
    ("drops", "Army", "Charon drops"),
    ("teleport", "Army", "Djinn teleports"),
    ("intel", "Intel", "the estimate of enemy production (for anti-nukes)"),
    ("aaintel", "Intel", "unseen anti-air placed from hits on our aircraft"),
    ("scouts", "Intel", "deep scouting with cloaked Widows"),
    ("radar", "Intel", "watchtowers: advanced radar on raised ground"),
    ("reanalysis", "Intel", "the terrain re-analysed now and then (walls, ditches)"),
    ("nukes", "Superweapons", "nuke silos and strikes"),
    ("nukeclear", "Superweapons", "our units keep clear of the blast of our missile in flight"),
    ("missiles", "Superweapons", "tactical missile silos"),
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
    let spec = a.bool_or("spec", false);
    if spec {
        ai.say_spec(&text);
    } else {
        ai.say(&text);
    }
    Ok(json!({"said": text, "spectators_only": spec}))
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
    if !FEATURES.iter().any(|(n, _, _)| *n == name) {
        let names: Vec<&str> = FEATURES.iter().map(|(n, _, _)| *n).collect();
        v["warning"] = json!(format!("'{name}' is not a known feature: {}", names.join(", ")));
    }
    Ok(v)
}

/// Every known feature with its group, what it does, and whether it is off
/// now and by default; then any other name switched off.
pub(crate) fn features_value(ai: &Ai) -> Value {
    let off = ai.features_off();
    let mut list: Vec<Value> = FEATURES
        .iter()
        .map(|(name, group, does)| {
            json!({"name": name, "group": group, "does": does, "off": ai.dev_off(name),
                   "default_off": crate::ai::OFF_BY_DEFAULT.contains(name)})
        })
        .collect();
    for name in off.iter().filter(|n| !FEATURES.iter().any(|(f, _, _)| f == n)) {
        list.push(json!({"name": name, "group": "Other", "does": "", "off": true, "default_off": false}));
    }
    Value::Array(list)
}

pub fn mute(ai: &mut Ai, a: &Args) -> Out {
    let name = a.str("module")?.trim().to_owned();
    if name.is_empty() {
        return Err("'module' is required".into());
    }
    let off = a.bool_or("off", true);
    let changed = if off { reserved::mute(ai.cb, &name) } else { reserved::unmute(ai.cb, &name) };
    let mut v = json!({"module": name, "muted": off, "changed": changed, "all_muted": reserved::muted(ai.cb)});
    if !views::MODULES.iter().any(|(m, _)| *m == name) && !name.contains('.') {
        v["warning"] = json!(format!(
            "'{name}' is not a module name; see `modules` (it mutes any handler starting with '{name}.')"
        ));
    }
    Ok(v)
}

pub fn hud(ai: &mut Ai, a: &Args) -> Out {
    if let Some(list) = a.opt_str("sections")? {
        let list = list.trim();
        let sections: Vec<String> = match list.to_ascii_lowercase().as_str() {
            "all" => SECTIONS.iter().map(|(s, _)| (*s).to_owned()).collect(),
            "none" | "" => Vec::new(),
            _ => list.split(',').map(|s| s.trim().to_ascii_lowercase()).filter(|s| !s.is_empty()).collect(),
        };
        let unknown: Vec<&String> = sections.iter().filter(|s| !SECTIONS.iter().any(|(n, _)| n == s)).collect();
        if !unknown.is_empty() {
            return Err(format!(
                "unknown sections {:?}; sections: {}",
                unknown,
                SECTIONS.iter().map(|(n, _)| *n).collect::<Vec<_>>().join(", ")
            ));
        }
        ai.op.hud.lock().unwrap_or_else(|e| e.into_inner()).sections = sections;
    }
    if let Some(name) = a.opt_str("unpin")? {
        let name = name.trim();
        let mut h = ai.op.hud.lock().unwrap_or_else(|e| e.into_inner());
        if name == "all" {
            h.pins.clear();
        } else {
            h.pins.retain(|p| p.tool != name);
        }
    }
    if let Some(name) = a.opt_str("pin")? {
        let name = name.trim().to_owned();
        let tool = super::tools::find(&name).ok_or_else(|| format!("no tool '{name}'"))?;
        if tool.writes || name == "hud" {
            return Err(format!("'{name}' cannot be pinned: only read tools"));
        }
        let args: Value = match a.opt_str("args")? {
            Some(s) if !s.trim().is_empty() => {
                serde_json::from_str(s).map_err(|e| format!("'args' is not JSON: {e}"))?
            }
            _ => json!({}),
        };
        if !args.is_object() {
            return Err("'args' must be a JSON object".into());
        }
        let mut h = ai.op.hud.lock().unwrap_or_else(|e| e.into_inner());
        h.pins.retain(|p| p.tool != name);
        if h.pins.len() >= 6 {
            return Err("at most 6 pins".into());
        }
        h.pins.push(hud::Pin { tool: name, args });
    }
    let cfg = ai.op.hud.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let text = hud::render(ai, &cfg);
    Ok(json!({"sections": cfg.sections, "text": text,
              "pins": cfg.pins.iter().map(|p| json!({"tool": p.tool, "args": p.args})).collect::<Vec<_>>(),
              "available": SECTIONS.iter().map(|(n, d)| json!({"name": n, "shows": d})).collect::<Vec<_>>()}))
}

pub fn feed(ai: &mut Ai, a: &Args) -> Out {
    let kinds = a.opt_str("kinds")?.map(|k| {
        k.split(',').map(|s| s.trim().to_owned()).filter(|s| !s.is_empty()).collect::<std::collections::BTreeSet<_>>()
    });
    let batch_ms = a.opt_i32("batch_ms")?.map(|v| v.clamp(0, 60_000) as u64);
    let sitrep_s = a.opt_i32("sitrep_s")?.map(|v| v.clamp(0, 3600) as u64);
    let mut f = ai.op.feed.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(k) = kinds {
        f.kinds = k;
    }
    if let Some(b) = batch_ms {
        f.batch_ms = b;
    }
    if let Some(s) = sitrep_s {
        f.sitrep_s = s;
    }
    Ok(json!({"kinds": f.kinds, "batch_ms": f.batch_ms, "sitrep_s": f.sitrep_s,
              "mcpl": ai.op.url.as_ref().map(|u| format!("{}/mcpl", u.replacen("http://", "ws://", 1)))}))
}

pub fn porc_zone(ai: &mut Ai, a: &Args) -> Out {
    if a.bool_or("clear", false) {
        let n = ai.op.over.porc_zones.len();
        ai.op.over.porc_zones.clear();
        return Ok(json!({"cleared": n}));
    }
    if let Some(pos) = point(ai, a)? {
        let radius = a.f32_or("radius", 600.0)?.clamp(50.0, 5000.0);
        let seconds = a.f32_or("seconds", 1800.0)?.clamp(1.0, 7200.0);
        let until = ai.frame + (seconds * 30.0) as i32;
        ai.op.over.porc_zones.push(PorcZone { pos, radius, until });
    }
    let frame = ai.frame;
    Ok(json!({
        "mexporc_off": ai.dev_off("mexporc"),
        "zones": ai.op.over.porc_zones.iter().map(|z| json!({"pos": xz(z.pos), "radius": r1(z.radius), "seconds_left": (z.until - frame) / 30})).collect::<Vec<_>>(),
        "note": if ai.dev_off("mexporc") { "turrets by mexes and links only inside these zones" } else { "feature mexporc is on: zones do nothing until you switch it off" },
    }))
}

pub fn dial(ai: &mut Ai, a: &Args) -> Out {
    let name = a.str("name")?.trim().to_ascii_lowercase();
    match name.as_str() {
        "greed" => {
            if let Some(v) = a.opt_f32("value")? {
                ai.op.over.greed = v.clamp(0.25, 4.0);
            }
        }
        "line" => {
            if let Some(v) = a.opt_f32("value")? {
                ai.op.over.line = v.clamp(0.0, 0.9);
            }
        }
        "buildpower" => {
            if let Some(v) = a.opt_f32("value")? {
                ai.op.over.buildpower = v.clamp(0.5, 3.0);
            }
        }
        key if super::POSTURE_SETTINGS.iter().any(|(k, _, _)| *k == key) => {
            let slot = super::POSTURE_SETTINGS.iter().find(|(k, _, _)| *k == key).map(|s| s.2);
            if let Some(slot) = slot {
                // A negative value puts the lever back to the AI's own.
                *slot(&mut ai.op.over) = match a.opt_f32("value")? {
                    Some(v) if v >= 0.0 => Some(super::posture_clamp(key, v)),
                    _ => None,
                };
            }
        }
        other => {
            return Err(format!(
                "unknown dial '{other}': greed, line, buildpower, defense_floor, defense_lane, defense_density, defense_cap, density_ref"
            ));
        }
    }
    Ok(json!({"dials": {"greed": ai.op_greed(), "line": ai.op.over.line, "buildpower": ai.op_buildpower(),
                        "line_point": ai.op_line_point().map(xz)}}))
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
    if ai.op_posture() {
        if ai.frame < 3600 {
            return Err(
                "posture profile: no production orders before 2:00 (the factory's own opening builds the constructors)"
                    .into(),
            );
        }
        let queued: u32 = ai.op.over.fac_orders.iter().map(|o| o.left).sum();
        if queued + count > 2 {
            return Err(format!("posture profile: at most 2 units queued ahead ({queued} already)"));
        }
    }
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
    if ai.op_posture() && ai.eco.effective_income < 30.0 {
        return Err(format!(
            "posture profile: a second factory only once income is past 30 (now {:.1})",
            ai.eco.effective_income
        ));
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

pub fn issue(ai: &mut Ai, a: &Args) -> Out {
    let task = a.i32("task")?;
    let task = u32::try_from(task).map_err(|_| "bad task id")?;
    if a.bool_or("clear", false) {
        let had = ai.logistics_close(task, "cleared by the operator").is_some();
        return Ok(json!({"task": task, "closed": had}));
    }
    if ai.eco.tasks.get(task).and_then(|t| t.as_construction()).is_none() {
        return Err(format!("no construction task {task} (see tasks)"));
    }
    let value = a.f32_or("value", 1000.0)?.max(0.0);
    let worth = match a.opt_f32("seconds")? {
        Some(s) => Worth::Deadline { value, by: ai.frame as f32 / 30.0 + s.max(0.0) },
        None => Worth::Flat(value),
    };
    let id = ai.logistics_open(task, Kind::Operator, worth, "the operator", 0.0).ok_or("could not open the issue")?;
    Ok(json!({"task": task, "issue": id}))
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

pub fn squad_intent(ai: &mut Ai, a: &Args) -> Out {
    let sid = a.i32("squad")?;
    let sid = u32::try_from(sid).map_err(|_| "bad squad id")?;
    if !ai.squads.contains(sid) {
        return Err(format!("no squad {sid} (see squads)"));
    }
    if let (Some(c), Some(sq)) = (a.opt_f32("courage")?, ai.squads.get_mut(sid)) {
        sq.courage = c.clamp(0.25, 4.0);
    }
    let intent = a.opt_str("intent")?.unwrap_or("").trim().to_ascii_lowercase();
    if intent.is_empty() {
        let courage = ai.squads.get(sid).map_or(1.0, |s| s.courage);
        return Ok(json!({"squad": sid, "courage": courage,
                         "intent": ai.op.over.squad_intents.get(&sid).map(|i| i.kind.name())}));
    }
    if intent == "clear" {
        let had = ai.op.over.squad_intents.remove(&sid).is_some();
        return Ok(json!({"squad": sid, "cleared": had}));
    }
    let kind = match intent.as_str() {
        "hold" | "form" | "guard" => IntentKind::Hold,
        "raid" => IntentKind::Raid,
        "escort" => IntentKind::Escort,
        "attack" => IntentKind::Attack,
        other => return Err(format!("unknown intent '{other}': hold, raid, escort, attack or clear")),
    };
    let seconds = a.f32_or("seconds", 600.0)?.clamp(1.0, 3600.0);
    let radius = a.f32_or("radius", if kind == IntentKind::Raid { 1500.0 } else { 300.0 })?.max(50.0);
    let unit = a.opt_i32("unit")?;
    let pos = match kind {
        IntentKind::Escort => {
            let id = unit.ok_or("'escort' needs the unit to escort (unit)")?;
            ours(ai, id).ok_or_else(|| format!("{id} is not one of our units"))?.pos()
        }
        _ => point(ai, a)?.ok_or("give x and z")?,
    };
    let frame = ai.frame;
    let mut i = Intent { kind, pos, radius, unit, until: frame + (seconds * 30.0) as i32, resolved: NULLPOS };
    i.resolved = pos;
    ai.op.over.squad_intents.insert(sid, i);
    // Drop an explicit target so the intent takes over at once.
    ai.op.over.squad_targets.remove(&sid);
    if ai.squads.get(sid).is_some_and(|s| s.kind == SquadKind::Raider) {
        ai.raider_squad_raid(sid, pos);
    } else {
        ai.squad_set_target(sid, pos);
    }
    let courage = ai.squads.get(sid).map_or(1.0, |s| s.courage);
    Ok(
        json!({"squad": sid, "intent": kind.name(), "pos": xz(pos), "radius": radius, "seconds": seconds, "courage": courage}),
    )
}

pub fn squad_form(ai: &mut Ai, a: &Args) -> Out {
    let ids = a.ids("units")?;
    let kind = match a.opt_str("kind")?.unwrap_or("auto").trim().to_ascii_lowercase().as_str() {
        "assault" => Some(SquadKind::Assault),
        "raider" => Some(SquadKind::Raider),
        "auto" => None,
        other => return Err(format!("unknown kind '{other}': assault, raider or auto")),
    };
    let mut units = Vec::new();
    let mut refused = Vec::new();
    for id in ids {
        match ours(ai, id) {
            Some(u) if u.def().is_some_and(|d| d.speed() > 0.0 && !d.weapon_mounts().is_empty()) => units.push(u),
            _ => refused.push(id),
        }
    }
    if units.is_empty() {
        return Err("no armed mobile units of ours among those".into());
    }
    // Whatever handler had them lets go (as on death); reserved units are
    // handed back so the squad handler can move them.
    for &u in &units {
        reserved::release(ai.cb, u.id());
        ai.retreat_remove_unit(u);
        ai.raider_remove_unit(u);
        ai.squad_remove_unit(u);
        ai.misc_remove_unit(u);
        ai.fighters.remove(&u.id());
    }
    let kind = kind.unwrap_or_else(|| {
        let uc = &ai.unit_classes;
        let raiders = units
            .iter()
            .filter(|u| {
                let n = ai.defs.name_of(**u);
                uc.small_raiders.contains(n) || uc.medium_raiders.contains(n) || uc.solo_raiders.contains(n)
            })
            .count();
        if raiders * 2 >= units.len() { SquadKind::Raider } else { SquadKind::Assault }
    });
    let team = ai.team_id;
    let sid = match kind {
        SquadKind::Assault => {
            let made = ai.squad_adopt(&units);
            made.first().copied().ok_or("could not form the squad")?
        }
        _ => {
            let sid = ai.squads.create(SquadKind::Raider, team);
            if let Some(sq) = ai.squads.get_mut(sid) {
                sq.raider_type = 'm';
                sq.status = 'a';
            }
            ai.raider_handler.raider_squads.push_back(sid);
            let frame = ai.frame;
            for u in &units {
                let cost = u.def().map_or(0.0, |d| d.cost(ai.metal));
                let f = Fighter::new_raider(*u, cost, team);
                let id = f.id;
                ai.fighters.insert(id, f);
                // Known to the raider handler as a squad raider, so its death
                // or departure leaves the squad the usual way.
                ai.raider_handler.medium_raiders.insert(id);
                if let Some(sq) = ai.squads.get_mut(sid) {
                    sq.add_unit(id, &mut ai.fighters, frame);
                }
            }
            // A leader, so the squad has a position from the start.
            if let Some(sq) = ai.squads.get_mut(sid) {
                sq.select_leader(&ai.fighters);
            }
            sid
        }
    };
    if let (Some(c), Some(sq)) = (a.opt_f32("courage")?, ai.squads.get_mut(sid)) {
        sq.courage = c.clamp(0.25, 4.0);
    }
    Ok(json!({"squad": sid, "kind": match kind { SquadKind::Raider => "raider", _ => "assault" },
              "members": units.iter().map(|u| u.id()).collect::<Vec<_>>(), "refused": refused,
              "hint": "give it an intent with squad_intent, or a place with squad_target"}))
}

pub fn squad_disband(ai: &mut Ai, a: &Args) -> Out {
    let sid = a.i32("squad")?;
    let sid = u32::try_from(sid).map_err(|_| "bad squad id")?;
    let Some(mut squad) = ai.squads.remove(sid) else {
        return Err(format!("no squad {sid} (see squads)"));
    };
    let members = squad.disband(&mut ai.fighters);
    ai.raider_handler.raider_squads.retain(|&s| s != sid);
    ai.squad_handler.squads.retain(|&s| s != sid);
    ai.squad_handler.taskforces.retain(|&s| s != sid);
    ai.squad_handler.air_squads.retain(|&s| s != sid);
    for slot in ai.raider_handler.next_squads.iter_mut() {
        if *slot == Some(sid) {
            *slot = None;
        }
    }
    ai.op.over.squad_intents.remove(&sid);
    ai.op.over.squad_targets.remove(&sid);
    // The members are nobody's now; the AI picks them up as if idle.
    let mut released = Vec::new();
    for id in members {
        ai.squad_handler.fighters.remove(&id);
        ai.fighters.remove(&id);
        if let Some(u) = ours(ai, id) {
            ai.dispatch(Event::UnitIdle { unit: u });
            released.push(id);
        }
    }
    Ok(json!({"squad": sid, "disbanded": released}))
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
        "field_fac" => {
            need_target()?;
            build.ok_or("'field_fac' needs the unit to build (unit)")?;
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
            // Zero-K's field factory gadget: CMD_FIELD_FAC_QUEUELESS copies
            // the factory's blueprint and sets the wanted unit in one go.
            "field_fac" => match (target, build) {
                (Some(t), Some(d)) => {
                    u.execute_custom_command(CMD_FIELD_FAC_QUEUELESS, &[t.id() as f32, d.id() as f32], opts, NO_TIMEOUT)
                }
                _ => false,
            },
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
