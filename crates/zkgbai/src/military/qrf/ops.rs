//! The operator's tools for the QRF: see it (`qrf`), set its budget
//! (`qrf_set`), and give teams roles and tactics (`qrf_team`).

use std::collections::BTreeMap;

use serde_json::{Value, json};
use springai::{Unit, Vec3};

use super::live::{MOON, Mode, Role, RoleKind, Tactic};
use crate::Ai;
use crate::operator::tools::Args;
use crate::operator::views::{Out, r1, xz};

fn mode_name(m: Mode) -> String {
    match m {
        Mode::Auto => "auto".into(),
        Mode::Metal(v) => format!("{v:.0} metal"),
        Mode::Share(s) => format!("{:.0}% of the army", s * 100.0),
    }
}

fn team_label(t: usize) -> Value {
    if t >= MOON { json!(format!("m{}", t - MOON)) } else { json!(t) }
}

/// A team id as the operator writes it: `3` (dedicated) or `m12`
/// (moonlighting squad 12).
fn parse_team(a: &Args) -> Result<Option<usize>, String> {
    match a.get("team") {
        None => Ok(None),
        Some(Value::Number(n)) => Ok(n.as_u64().map(|v| v as usize)),
        Some(Value::String(s)) => {
            let s = s.trim();
            match s.strip_prefix('m') {
                Some(sid) => sid.parse::<usize>().map(|v| Some(MOON + v)).map_err(|_| "bad team id".into()),
                None => s.parse::<usize>().map(Some).map_err(|_| "bad team id".into()),
            }
        }
        Some(_) => Err("'team' must be a team id".into()),
    }
}

pub fn view(ai: &mut Ai, _: &Args) -> Out {
    let q = &ai.qrf;
    let frame = ai.frame;
    let b = &q.budget;
    let (raids_at_once, typical) = b.recent();
    let mut teams: BTreeMap<usize, Vec<i32>> = BTreeMap::new();
    for (&id, m) in &q.members {
        teams.entry(m.team).or_default().push(id);
    }
    let at_hand = ai.qrf_teams();
    let teams: Vec<Value> = (teams.iter())
        .map(|(&t, ids)| {
            let mut types: BTreeMap<&str, u32> = BTreeMap::new();
            for id in ids {
                if let Some(m) = q.members.get(id) {
                    *types.entry(ai.defs.name(m.def)).or_default() += 1;
                }
            }
            let here = at_hand.get(&t).cloned().unwrap_or_default();
            let (pos, _) = ai.qrf_team_pos(if here.is_empty() { ids } else { &here });
            let mut v = json!({
                "team": team_label(t),
                "kind": if t >= MOON { "moonlight" } else { "dedicated" },
                "units": ids, "types": types,
                "power": ai.qrf_team_power(ids).round(), "pos": xz(pos),
                "away": ids.len() - here.len(),
            });
            if t >= MOON {
                v["squad"] = json!(t - MOON);
                v["drafted"] = json!(q.drafted.contains_key(&((t - MOON) as u32)));
            }
            if let Some(r) = q.roles.get(&t) {
                v["role"] =
                    json!({"role": r.kind.name(), "pos": xz(r.pos), "radius": r1(r.radius), "tactic": r.tactic.name()});
            }
            if let Some(d) = q.doing.get(&t) {
                v["doing"] = json!({"what": d.what, "at": xz(d.at), "raid_power": d.raid.round(),
                                    "seconds_ago": (frame - d.frame) / 30});
            }
            v
        })
        .collect();
    let raids: Vec<Value> = (q.groups.iter().enumerate())
        .map(|(i, g)| {
            let mut types: BTreeMap<String, u32> = BTreeMap::new();
            for id in &g.members {
                let name = q.tracks.get(id).and_then(|t| t.kind()).map_or("?".to_owned(), |d| {
                    let known = q.tracks.get(id).is_some_and(|t| t.def.is_some());
                    format!("{}{}", ai.defs.name(d), if known { "" } else { "?" })
                });
                *types.entry(name).or_default() += 1;
            }
            let near = q.assets.iter().map(|a| crate::util::distance(a.pos, g.center)).fold(f32::INFINITY, f32::min);
            json!({"raid": i, "units": g.members.len(), "types": types, "power": g.power.round(),
                   "center": xz(g.center), "vel": xz(g.vel), "speed": r1(g.speed),
                   "nearest_asset": if near.is_finite() { json!(near.round()) } else { Value::Null }})
        })
        .collect();
    let s = &q.live_stats;
    Ok(json!({
        "on": q.on, "mode": if q.live { "live" } else if q.on { "staged" } else { "off" },
        "algo": q.algo, "team_size": q.team_size, "moonlight": !q.no_moonlight,
        "budget": {"mode": mode_name(b.mode), "wanted": b.wanted.round(), "have": b.have.round(),
                   "army": b.army.round(), "moonlight": b.moonlight.round(), "cap": b.cap, "gain": b.gain,
                   "reason": b.reason, "recent": {"raids_at_once": raids_at_once, "typical_raid": typical.round()}},
        "teams": teams, "raids": raids,
        "stats": {"recruited": s.recruited, "released": s.released, "drafts": s.drafts,
                  "losses": s.losses, "losses_while_marked": s.losses_marked, "losses_metal": s.losses_metal.round()},
        "hint": "team ids: numbers are dedicated teams, mN the moonlighting raider squad N; qrf_team sets roles",
    }))
}

pub fn set(ai: &mut Ai, a: &Args) -> Out {
    if let Some(d) = a.get("defense") {
        let mode = match d {
            Value::Number(n) => Mode::Metal(n.as_f64().unwrap_or(0.0).max(0.0) as f32),
            Value::String(s) => match s.trim() {
                "auto" => Mode::Auto,
                s => match s.strip_suffix('%') {
                    Some(p) => Mode::Share(
                        p.trim()
                            .parse::<f32>()
                            .map_err(|_| "defense: auto, a metal value or a share like 30%")?
                            .clamp(0.0, 100.0)
                            / 100.0,
                    ),
                    None => Mode::Metal(
                        s.parse::<f32>().map_err(|_| "defense: auto, a metal value or a share like 30%")?.max(0.0),
                    ),
                },
            },
            _ => return Err("defense: auto, a metal value or a share like 30%".into()),
        };
        ai.qrf.budget.mode = mode;
    }
    if let Some(c) = a.opt_f32("cap")? {
        ai.qrf.budget.cap = c.clamp(0.0, 1.0);
    }
    if let Some(g) = a.opt_f32("gain")? {
        ai.qrf.budget.gain = g.clamp(0.0, 10.0);
    }
    if a.has("moonlight") {
        ai.qrf.no_moonlight = !a.bool_or("moonlight", true);
    }
    if let Some(n) = a.opt_i32("team_size")? {
        ai.qrf.team_size = n.clamp(1, 12) as usize;
    }
    if let Some(alg) = a.opt_str("algo")? {
        let alg = alg.trim();
        if !matches!(alg, "mark" | "cover" | "station" | "intent") {
            return Err(format!("unknown algorithm '{alg}': mark, cover, station, intent"));
        }
        ai.qrf.algo = alg.to_owned();
    }
    let mut v = view(ai, a)?;
    if !ai.qrf.on {
        v["warning"] = json!("the QRF is off (feature 'qrf' off, or before 0:10)");
    }
    Ok(json!({"budget": v["budget"], "moonlight": v["moonlight"], "team_size": v["team_size"], "algo": v["algo"],
              "warning": v.get("warning")}))
}

pub fn team(ai: &mut Ai, a: &Args) -> Out {
    if !ai.qrf.live {
        return Err("the QRF is not running in this game".into());
    }
    let mut team = parse_team(a)?;
    if a.has("units") && ai.op_posture() {
        return Err("posture profile: no hands on units (give roles to the QRF's own teams instead)".into());
    }
    if a.has("units") {
        let units: Vec<Unit> = (a.ids("units")?.into_iter())
            .filter_map(|id| Unit::from_id(ai.cb, id))
            .filter(|u| u.team() == ai.team_id && u.def().is_some_and(|d| d.speed() > 0.0 && !d.is_able_to_fly()))
            .collect();
        if units.is_empty() {
            return Err("no ground units of ours among those".into());
        }
        team = Some(ai.qrf_draft_units(&units).ok_or("none of those can be QRF")?);
        // A team of the operator's keeps a role (and is not let go).
        ai.qrf.roles.entry(team.unwrap_or(0)).or_insert(Role {
            kind: RoleKind::Auto,
            pos: Vec3::ZERO,
            radius: 0.0,
            tactic: Tactic::Mark,
        });
    }
    let team = team.ok_or("give a team (see qrf) or units to make one")?;
    if team >= MOON {
        return Err("moonlighting squads have no roles (use squad tools, or draft their units with 'units')".into());
    }
    if !ai.qrf.members.values().any(|m| m.team == team && m.squad.is_none()) {
        return Err(format!("no team {team} (see qrf)"));
    }
    let role = a.opt_str("role")?.map(|r| r.trim().to_ascii_lowercase());
    match role.as_deref() {
        Some("release") => {
            let n = ai.qrf_release_team(team, "released by the operator");
            return Ok(json!({"team": team, "released": n}));
        }
        Some("clear") => {
            ai.qrf.roles.remove(&team);
            return Ok(json!({"team": team, "role": "auto", "note": "the budget may let it go again"}));
        }
        _ => {}
    }
    let kind = match role.as_deref() {
        None => ai.qrf.roles.get(&team).map_or(RoleKind::Auto, |r| r.kind),
        Some("auto") => RoleKind::Auto,
        Some("zone") => RoleKind::Zone,
        Some("post") => RoleKind::Post,
        Some("hold") => RoleKind::Hold,
        Some(other) => return Err(format!("unknown role '{other}': auto, zone, post, hold, clear, release")),
    };
    let old = ai.qrf.roles.get(&team).copied();
    let pos = match (a.opt_f32("x")?, a.opt_f32("z")?) {
        (Some(x), Some(z)) => Vec3::new(x, ai.cb.map().elevation_at(x, z), z),
        (None, None) => match old {
            Some(r) if r.kind != RoleKind::Auto || kind == RoleKind::Auto => r.pos,
            _ if kind == RoleKind::Auto => Vec3::ZERO,
            _ => return Err(format!("role '{}' needs x and z", kind.name())),
        },
        _ => return Err("give both x and z".into()),
    };
    let radius = a.f32_or("radius", old.map_or(1200.0, |r| if r.radius > 0.0 { r.radius } else { 1200.0 }))?.max(100.0);
    let tactic = match a.opt_str("tactic")?.map(|t| t.trim().to_ascii_lowercase()).as_deref() {
        None => old.map_or(Tactic::Mark, |r| r.tactic),
        Some("mark") => Tactic::Mark,
        Some("shadow") => Tactic::Shadow,
        Some("hunt") => Tactic::Hunt,
        Some(other) => return Err(format!("unknown tactic '{other}': mark, shadow, hunt")),
    };
    ai.qrf.roles.insert(team, Role { kind, pos, radius, tactic });
    let members: Vec<i32> =
        ai.qrf.members.iter().filter(|(_, m)| m.team == team && m.squad.is_none()).map(|(&id, _)| id).collect();
    Ok(json!({"team": team, "units": members, "role": kind.name(), "pos": xz(pos), "radius": r1(radius),
              "tactic": tactic.name(), "note": "kept until cleared (role clear) or released (role release)"}))
}
