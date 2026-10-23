//! Initial queues shared with allied players: what an allied human queued
//! in the pregame (Zero-K's `unit_initial_queue` widget, relayed by a game
//! gadget as a Lua message) and our own opening sent back the same way.
//!
//! Received, from the gadget: `IQ|<team>|<type>|<unitDefID>|<x>|<y>|<z>|<face>`
//! for an order (type 1 inserts at the front of the queue, 3 appends, 4
//! replaces the queue with the one item), `IQ|<team>|2|<index>` removes
//! the 1-based index, `IQ|<team>|5` clears the queue. Sent, to the gadget:
//! `IQ|5` then `IQ|3|<unitDefID>|<x>|<y>|<z>|<face>` per item, which is
//! what one widget tells another.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::json;
use springai::Vec3;

use crate::Ai;
use crate::economy::TaskId;
use crate::util::distance;

/// A queued mex this close to a metal spot claims that spot, elmos.
pub const MEX_CLAIM: f32 = 120.0;
/// An ally's claim on a spot lapses at this frame (3:00) if nothing stands
/// there by then.
pub const CLAIM_LAPSES: i32 = 5400;
/// The most items of our opening sent.
const MAX_SENT: usize = 12;
/// The mexes nearest the commander sent as its opening.
const SENT_MEXES: usize = 6;
/// Frames of quiet after an ally's last order before its plan is reported.
const SETTLED: i32 = 150;
/// The widget's "append" message type (what one widget sends another
/// for a shift-queued build).
const APPEND: i32 = 3;

/// One order of an ally's initial queue.
#[derive(Clone, Debug, PartialEq)]
pub struct Plan {
    /// The unit def's name, or its id as text when unknown.
    pub def_name: String,
    pub def_id: i32,
    pub pos: Vec3,
    pub face: i32,
    /// When it arrived.
    pub frame: i32,
}

/// The allies' initial queues, by team, and the state of our own.
#[derive(Default)]
pub struct AllyPlans {
    pub teams: BTreeMap<i32, Vec<Plan>>,
    /// The frame of the last change to any queue.
    pub changed: i32,
    /// Whether the pilot has been told of the allies' plans.
    alerted: bool,
    /// Whether our opening went out.
    pub sent: bool,
    /// Spots (x, z) already logged as left to an ally.
    skipped_logged: BTreeSet<(i32, i32)>,
}

impl AllyPlans {
    /// The team with a mex queued within [`MEX_CLAIM`] of `pos`, if any.
    pub fn mex_claim(&self, pos: Vec3) -> Option<i32> {
        self.teams.iter().find_map(|(&team, plans)| {
            plans.iter().any(|p| p.def_name == "staticmex" && distance(p.pos, pos) <= MEX_CLAIM).then_some(team)
        })
    }
}

/// A parsed `IQ|...` message from an ally.
#[derive(Clone, Debug, PartialEq)]
pub enum IqMessage {
    /// An order: `kind` is the widget's 1 (front), 3 (append) or 4 (replace).
    Order {
        team: i32,
        kind: i32,
        def_id: i32,
        pos: Vec3,
        face: i32,
    },
    /// The 1-based index removed from the queue.
    Remove {
        team: i32,
        index: usize,
    },
    Clear {
        team: i32,
    },
}

/// Parse an ally's message; `None` for anything else or malformed.
pub fn parse_iq(data: &str) -> Option<IqMessage> {
    let rest = data.strip_prefix("IQ|")?;
    let fields: Vec<&str> = rest.split('|').map(str::trim).collect();
    let int = |s: &str| s.parse::<i32>().ok();
    let num = |s: &str| s.parse::<f32>().ok().filter(|v| v.is_finite());
    let team = int(fields.first()?)?;
    let kind = int(fields.get(1)?)?;
    match kind {
        5 => Some(IqMessage::Clear { team }),
        2 => {
            let index = int(fields.get(2)?).filter(|&i| i >= 1)? as usize;
            Some(IqMessage::Remove { team, index })
        }
        1 | 3 | 4 => {
            let [def, x, y, z, face] = fields.get(2..7)? else { return None };
            let def_id = int(def).filter(|&d| d > 0)?;
            let pos = Vec3::new(num(x)?, num(y)?, num(z)?);
            let face = num(face)? as i32;
            Some(IqMessage::Order { team, kind, def_id, pos, face })
        }
        _ => None,
    }
}

impl Ai {
    /// An ally's initial-queue message, relayed by the game.
    pub fn iq_lua_message(&mut self, data: &str) {
        if self.allies.is_empty() {
            return;
        }
        let Some(msg) = parse_iq(data) else { return };
        let team = match msg {
            IqMessage::Order { team, .. } | IqMessage::Remove { team, .. } | IqMessage::Clear { team } => team,
        };
        if !self.allies.contains(&team) {
            return;
        }
        let frame = self.frame;
        self.iq.changed = frame;
        let queue = self.iq.teams.entry(team).or_default();
        match msg {
            IqMessage::Clear { .. } => {
                queue.clear();
                self.op_emit("iq", json!({"team": team, "clear": true}));
            }
            IqMessage::Remove { index, .. } => {
                if index <= queue.len() {
                    queue.remove(index - 1);
                }
                self.op_emit("iq", json!({"team": team, "remove": index}));
            }
            IqMessage::Order { kind, def_id, pos, face, .. } => {
                let def_name = self.defs.name_by_id(def_id).map_or_else(|| def_id.to_string(), str::to_owned);
                let plan = Plan { def_name: def_name.clone(), def_id, pos, face, frame };
                match kind {
                    1 => queue.insert(0, plan),
                    4 => *queue = vec![plan],
                    _ => queue.push(plan),
                }
                self.op_emit("iq", json!({"team": team, "def": def_name, "pos": [pos.x, pos.z], "face": face}));
            }
        }
    }

    /// The team whose queued mex claims the spot at `pos`, while the claim
    /// stands (before [`CLAIM_LAPSES`]; an owned spot never asks).
    pub fn iq_spot_claimed(&self, pos: Vec3) -> Option<i32> {
        if self.allies.is_empty() || self.frame >= CLAIM_LAPSES {
            return None;
        }
        self.iq.mex_claim(pos)
    }

    /// Whether a spot left to an ally was logged before (and note it).
    pub fn iq_note_skipped(&mut self, pos: Vec3) -> bool {
        self.iq.skipped_logged.insert((pos.x as i32, pos.z as i32))
    }

    /// Each economy tick: report the allies' plans to the pilot once they
    /// settle, and send our opening once it is known. Nothing without an
    /// ally.
    pub fn iq_tick(&mut self) {
        if self.allies.is_empty() {
            return;
        }
        if !self.iq.alerted && self.frame - self.iq.changed >= SETTLED {
            let lines: Vec<String> = (self.iq.teams.iter())
                .filter(|(_, plans)| !plans.is_empty())
                .map(|(team, plans)| {
                    let mexes = plans.iter().filter(|p| p.def_name == "staticmex").count();
                    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
                    for p in plans {
                        *counts.entry(&p.def_name).or_default() += 1;
                    }
                    let list: Vec<String> = counts
                        .iter()
                        .map(|(n, &c)| if c > 1 { format!("{n} x{c}") } else { (*n).to_owned() })
                        .collect();
                    format!("ally {team} queued {} orders ({mexes} mexes): {}", plans.len(), list.join(", "))
                })
                .collect();
            if !lines.is_empty() {
                self.iq.alerted = true;
                self.op_alert("iq", 1800, lines.join("; "));
            }
        }
        if !self.iq.sent && !self.eco.commanders.is_empty() && (!self.eco.factory_tasks.is_empty() || self.frame >= 300)
        {
            self.iq_send_opening();
        }
    }

    /// Our commander's opening to the allies: the factory, the mexes
    /// nearest the commander, the energy planned so far.
    fn iq_send_opening(&mut self) {
        self.iq.sent = true;
        let Some(com) = self.eco.commanders.iter().find_map(|id| self.eco.workers.get(id).map(|w| w.get_pos())) else {
            return;
        };
        let task = |ai: &Ai, id: TaskId| {
            let t = ai.eco.tasks.get(id)?;
            let c = t.as_construction()?;
            (c.target.is_none()).then(|| (c.build_type, t.position, c.facing, ai.defs.name(c.build_type).to_owned()))
        };
        let mut items: Vec<(springai::UnitDef, Vec3, i32)> =
            (self.eco.factory_tasks.iter()).filter_map(|&id| task(self, id)).map(|(d, p, f, _)| (d, p, f)).collect();
        let anchor = items.first().map_or(com, |i| i.1);
        let mut mexes: Vec<_> = (self.eco.construction_tasks.iter())
            .filter_map(|&id| task(self, id))
            .filter(|(_, _, _, name)| name == "staticmex")
            .map(|(d, p, f, _)| (distance(p, anchor), d, p, f))
            .collect();
        mexes.sort_by(|a, b| a.0.total_cmp(&b.0));
        items.extend(mexes.into_iter().take(SENT_MEXES).map(|(_, d, p, f)| (d, p, f)));
        items.extend(
            (self.eco.construction_tasks.iter())
                .filter_map(|&id| task(self, id))
                .filter(|(_, _, _, name)| name == "energysolar" || name == "energywind")
                .map(|(d, p, f, _)| (d, p, f)),
        );
        items.truncate(MAX_SENT);
        let map = self.cb.map();
        self.cb.call_lua_rules("IQ|5");
        let mut sent = Vec::new();
        for (def, pos, face) in &items {
            let (x, z) = (pos.x.round() as i32, pos.z.round() as i32);
            let y = map.elevation_at(pos.x, pos.z).max(0.0).round() as i32;
            self.cb.call_lua_rules(&format!("IQ|{APPEND}|{}|{x}|{y}|{z}|{face}", def.id()));
            sent.push(json!({"def": self.defs.name(*def), "pos": [x, z], "face": face}));
        }
        self.log(&format!("iq: sent {} opening items", items.len()));
        self.op_emit("iq", json!({"sent": sent}));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_an_order() {
        let msg = parse_iq("IQ|2|4|404|648|2|3304|1");
        assert_eq!(
            msg,
            Some(IqMessage::Order { team: 2, kind: 4, def_id: 404, pos: Vec3::new(648.0, 2.0, 3304.0), face: 1 })
        );
        let front = parse_iq("IQ|0|1|12|100.0|0|200.0|0").unwrap();
        assert!(matches!(front, IqMessage::Order { team: 0, kind: 1, def_id: 12, face: 0, .. }));
        let append = parse_iq("IQ|3|3|12|100|0|200|3").unwrap();
        assert!(matches!(append, IqMessage::Order { team: 3, kind: 3, def_id: 12, face: 3, .. }));
    }

    #[test]
    fn parses_a_clear_and_a_remove() {
        assert_eq!(parse_iq("IQ|2|5"), Some(IqMessage::Clear { team: 2 }));
        assert_eq!(parse_iq("IQ|2|5|"), Some(IqMessage::Clear { team: 2 }));
        assert_eq!(parse_iq("IQ|1|2|3"), Some(IqMessage::Remove { team: 1, index: 3 }));
        assert_eq!(parse_iq("IQ|1|2|0"), None);
    }

    #[test]
    fn rejects_malformed_messages() {
        for bad in [
            "",
            "IQ",
            "IQ|",
            "IQ|x|5",
            "IQ|2",
            "IQ|2|7|404|1|2|3|0",
            "IQ|2|3|404|1|2",
            "IQ|2|3|404|a|2|3|0",
            "IQ|2|3|0|1|2|3|0",
            "IQ|2|3|404|nan|2|3|0",
            "NUKE_EXPLOSION:1,2,3",
            "iq|2|5",
        ] {
            assert_eq!(parse_iq(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn mex_claims_are_by_distance_and_def() {
        let plan =
            |name: &str, x: f32| Plan { def_name: name.into(), def_id: 1, pos: Vec3::xz(x, 0.0), face: 0, frame: 0 };
        let mut plans = AllyPlans::default();
        plans.teams.insert(2, vec![plan("staticmex", 1000.0), plan("energysolar", 2000.0)]);
        assert_eq!(plans.mex_claim(Vec3::xz(1100.0, 0.0)), Some(2));
        assert_eq!(plans.mex_claim(Vec3::xz(1200.0, 0.0)), None);
        assert_eq!(plans.mex_claim(Vec3::xz(2000.0, 0.0)), None);
    }
}
