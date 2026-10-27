//! Initial queues shared with allies, human or AI: what an ally queued in
//! the pregame (Zero-K's `unit_initial_queue` widget, relayed by a game
//! gadget as a Lua message) and our own opening sent back the same way.
//!
//! Like a player, we announce ours in the pregame: the opening factory where
//! the commander will build it, then the mexes nearest it. That goes out
//! from `init` (the engine creates AIs while the game loads, before the
//! pregame) and again whenever an ally's queue changes which spots are ours.
//! We give way to humans and to AIs of lower team numbers, never to the
//! rest, so two AIs settle instead of yielding to each other in turn. Once
//! the game is on, the opening as actually planned goes out once more, for
//! AI allies (the widget stops drawing queues at the start).
//!
//! Received, from the gadget: `IQ|<team>|<type>|<unitDefID>|<x>|<y>|<z>|<face>`
//! for an order (type 1 inserts at the front of the queue, 3 appends, 4
//! replaces the queue with the one item), `IQ|<team>|2|<index>` removes
//! the 1-based index, `IQ|<team>|5` clears the queue. Sent, to the gadget:
//! `IQ|5` then `IQ|3|<unitDefID>|<x>|<y>|<z>|<face>` per item, which is
//! what one widget tells another.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::json;
use springai::{UnitDef, Vec3};

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
    /// Whether our opening as planned went out (once the game is on).
    pub sent: bool,
    /// Spots (x, z) already logged as left to an ally.
    skipped_logged: BTreeSet<(i32, i32)>,
    /// The allies played by AIs; the others are humans.
    pub ai_allies: BTreeSet<i32>,
    /// Whether the game is on (the pregame is over).
    pub started: bool,
    /// Our orders as last sent, one message each.
    announced: Vec<String>,
}

impl AllyPlans {
    /// The team with a mex queued within [`MEX_CLAIM`] of `pos`, if any.
    pub fn mex_claim(&self, pos: Vec3) -> Option<i32> {
        self.mex_claim_among(pos, |_| true)
    }

    /// [`Self::mex_claim`] among the teams `among` accepts.
    pub fn mex_claim_among(&self, pos: Vec3, among: impl Fn(i32) -> bool) -> Option<i32> {
        self.teams.iter().filter(|&(&team, _)| among(team)).find_map(|(&team, plans)| {
            plans.iter().any(|p| p.def_name == "staticmex" && distance(p.pos, pos) <= MEX_CLAIM).then_some(team)
        })
    }
}

/// The teams the setup script gives to Skirmish AIs.
pub fn ai_teams(script: &str) -> BTreeSet<i32> {
    let mut teams = BTreeSet::new();
    let mut depth = 0;
    let mut in_ai = false;
    for raw in script.lines() {
        let line: String = raw.trim().to_ascii_lowercase().chars().filter(|c| !c.is_whitespace()).collect();
        if line.starts_with("[ai") {
            in_ai = true;
        }
        depth += line.matches('{').count() as i32;
        depth -= line.matches('}').count() as i32;
        if in_ai {
            for kv in line.split([';', '{', '}']) {
                if let Some(team) = kv.strip_prefix("team=").and_then(|t| t.parse().ok()) {
                    teams.insert(team);
                }
            }
            if line.contains('}') && depth <= 1 {
                in_ai = false;
            }
        }
    }
    teams
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
        if !self.iq.started {
            self.iq_announce();
        }
    }

    /// From `init`, once our start position is chosen: where the opening
    /// factory goes, then our pregame announcement. Nothing without an ally.
    pub fn iq_init(&mut self, opening: Option<&str>) {
        if self.allies.is_empty() {
            return;
        }
        let script = self.cb.game().setup_script();
        self.iq.ai_allies = ai_teams(&script).into_iter().filter(|t| self.allies.contains(t)).collect();
        if self.cb.game().current_frame() > 0 {
            // Created mid-game: there is no pregame to announce in.
            self.iq.started = true;
            return;
        }
        let start = self.graph.get_start_pos();
        if start.is_zero() {
            return;
        }
        if let Some(def) = opening.and_then(|f| self.defs.get(f)) {
            self.eco.opening_site = self.eco_first_factory_site(def, start).map(|(pos, face)| (def, pos, face));
        }
        self.iq_announce();
    }

    /// Whether we give way to `team`'s queue: a human's, or a lower-numbered
    /// AI's.
    fn iq_gives_way(&self, team: i32) -> bool {
        !self.iq.ai_allies.contains(&team) || team < self.team_id
    }

    /// Announce our pregame orders: the opening factory, then the mexes
    /// nearest it (nearest the start without one) that no ally we give way
    /// to has queued.
    fn iq_announce(&mut self) {
        let mut orders: Vec<(UnitDef, Vec3, i32)> = self.eco.opening_site.into_iter().collect();
        let anchor = orders.first().map_or(self.graph.get_start_pos(), |o| o.1);
        if anchor.is_zero() {
            return; // no start position chosen
        }
        if let Some(mex) = self.defs.get("staticmex") {
            let mut spots: Vec<Vec3> = (self.graph.metal_spots.iter())
                .map(|s| s.position)
                .filter(|&p| self.iq.mex_claim_among(p, |t| self.iq_gives_way(t)).is_none())
                .collect();
            spots.sort_by(|a, b| distance(*a, anchor).total_cmp(&distance(*b, anchor)));
            orders.extend(spots.into_iter().take(SENT_MEXES).map(|p| (mex, p, 0)));
        }
        self.iq_send(&orders, "pregame");
    }

    /// Send `orders` the way one widget tells another (`IQ|5`, then an
    /// append each), unless they are what we last sent.
    fn iq_send(&mut self, orders: &[(UnitDef, Vec3, i32)], what: &str) {
        let map = self.cb.map();
        let msgs: Vec<String> = (orders.iter())
            .map(|(def, pos, face)| {
                let (x, z) = (pos.x.round() as i32, pos.z.round() as i32);
                let y = map.elevation_at(pos.x, pos.z).max(0.0).round() as i32;
                format!("IQ|{APPEND}|{}|{x}|{y}|{z}|{face}", def.id())
            })
            .collect();
        if msgs == self.iq.announced {
            return;
        }
        self.cb.call_lua_rules("IQ|5");
        for msg in &msgs {
            self.cb.call_lua_rules(msg);
        }
        let sent: Vec<_> = (orders.iter())
            .map(|(def, pos, face)| {
                json!({"def": self.defs.name(*def), "pos": [pos.x.round() as i32, pos.z.round() as i32], "face": face})
            })
            .collect();
        self.log(&format!("iq: sent {} {what} items", orders.len()));
        self.op_emit("iq", json!({"sent": sent, "what": what}));
        self.iq.announced = msgs;
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
    /// settle, and send our opening as planned once it is known. Nothing
    /// without an ally.
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
        let mut items: Vec<(UnitDef, Vec3, i32)> =
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
        self.iq_send(&items, "opening");
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

    #[test]
    fn mex_claims_among_some_teams() {
        let plan = |x: f32| Plan { def_name: "staticmex".into(), def_id: 1, pos: Vec3::xz(x, 0.0), face: 0, frame: 0 };
        let mut plans = AllyPlans::default();
        plans.teams.insert(1, vec![plan(1000.0)]);
        plans.teams.insert(3, vec![plan(1000.0), plan(3000.0)]);
        assert_eq!(plans.mex_claim_among(Vec3::xz(1000.0, 0.0), |t| t != 1), Some(3));
        assert_eq!(plans.mex_claim_among(Vec3::xz(3000.0, 0.0), |t| t < 2), None);
    }

    #[test]
    fn ai_teams_from_the_setup_script() {
        let script = "[GAME]\n{\n\t[TEAM0]\n\t{\n\t\tTeamLeader=0;\n\t\tAllyTeam=0;\n\t}\n\
                      \t[AI0]\n\t{\n\t\tName=Bot;\n\t\tShortName=ZKGBAI;\n\t\tTeam=1;\n\t\tHost=0;\n\
                      \t\t[Options]\n\t\t{\n\t\t\tmute=eco;\n\t\t}\n\t}\n\
                      \t[ai1] { team = 3; shortname = CircuitAI; }\n\
                      \t[PLAYER0]\n\t{\n\t\tTeam=0;\n\t}\n}\n";
        assert_eq!(ai_teams(script), BTreeSet::from([1, 3]));
    }
}
