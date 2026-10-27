//! A trace of the raider squads' decisions, to see which of them flip-flop
//! and why (user, 2026-10-26: squads seen oscillating).
//!
//! Every half second each active raider squad decides afresh, with no
//! memory of its last decision (`raider_assign_raider_squad`): the
//! candidates are the haven-and-back bounce at a threat disc's rim (as solo
//! raiders did, `zkgbai-runs/raider/PYRO-REPLICATION.md`), a straggler's
//! cell tripping the bail-out, target churn between near-equal targets, and
//! other modules (QRF drafts, incursion blocks) taking the squad and letting
//! it go. Each decision is noted here with its branch and target; a flip is
//! a change between going somewhere and falling back to a haven, a target
//! switch is going somewhere else (over [`SWITCH`] elmos away), and blocks
//! and drafts count when they start or stop. A `squads` journal record sums
//! them every [`REPORT`] frames, with the squads that flipped most; with
//! `ZKGBAI_SQUAD_TRACE` set, every decision is a `squad` record too.

use std::collections::BTreeMap;

use serde_json::json;
use springai::Vec3;

use crate::Ai;
use crate::military::wrappers::SquadId;
use crate::util::distance;

/// Frames between summaries.
const REPORT: i32 = 30 * 30;
/// A new target this far from the last counts as a switch, elmos.
const SWITCH: f32 = 400.0;

/// What a squad decided.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Branch {
    /// Moonlighting for the QRF.
    Drafted,
    /// Sent by the operator.
    Operator,
    /// Forming or rallying, to its rally point.
    Rally,
    /// Sent to block an incursion's way.
    Block,
    /// Forming or rallying, falling back to a haven.
    HavenForming,
    /// Active, falling back to a raider haven.
    Haven,
    /// Active, at an enemy (or a sniper sighting).
    Target,
    /// Active, on a scouting task.
    Task,
    /// Active with nothing to do: to the raider rally.
    Idle,
}

impl Branch {
    fn name(self) -> &'static str {
        match self {
            Branch::Drafted => "drafted",
            Branch::Operator => "operator",
            Branch::Rally => "rally",
            Branch::Block => "block",
            Branch::HavenForming => "haven_forming",
            Branch::Haven => "haven",
            Branch::Target => "target",
            Branch::Task => "task",
            Branch::Idle => "idle",
        }
    }

    fn holds(self) -> bool {
        matches!(self, Branch::Haven | Branch::HavenForming)
    }
}

/// Why an active squad fell back.
#[derive(Clone, Copy, Debug, Default)]
pub struct Bail {
    /// Threat at the target, at the worst member's cell, and the squad's own
    /// raider threat at its centre.
    pub at_target: f32,
    pub at_member: f32,
    pub own: f32,
    /// The worst member stood this far from the squad's centre.
    pub straggle: f32,
}

#[derive(Default)]
pub struct SquadTrace {
    last: BTreeMap<SquadId, (Branch, Vec3)>,
    /// Flips per squad since the last summary.
    flips: BTreeMap<SquadId, u32>,
    stats: Stats,
}

#[derive(Default, Clone, Copy)]
struct Stats {
    decisions: u32,
    to_haven: u32,
    from_haven: u32,
    switches: u32,
    block_toggles: u32,
    draft_toggles: u32,
    /// Fall-backs by what tripped them: the target's threat, a member's
    /// cell (a straggler: over 300 from the centre).
    bail_target: u32,
    bail_member: u32,
    bail_straggler: u32,
}

impl Ai {
    /// Note squad `sid`'s decision.
    pub(crate) fn squad_note(&mut self, sid: SquadId, branch: Branch, target: Vec3, pos: Vec3, bail: Option<Bail>) {
        let t = &mut self.squad_trace;
        t.stats.decisions += 1;
        if let Some(b) = bail {
            if b.at_target > b.own {
                t.stats.bail_target += 1;
            } else if b.straggle > 300.0 {
                t.stats.bail_straggler += 1;
            } else {
                t.stats.bail_member += 1;
            }
        }
        if let Some(&(last, last_target)) = t.last.get(&sid) {
            let flipped = last.holds() != branch.holds();
            if flipped {
                if branch.holds() {
                    t.stats.to_haven += 1;
                } else {
                    t.stats.from_haven += 1;
                }
                *t.flips.entry(sid).or_default() += 1;
            } else if !branch.holds() && last == branch && distance(last_target, target) > SWITCH {
                t.stats.switches += 1;
            }
            if (last == Branch::Block) != (branch == Branch::Block) {
                t.stats.block_toggles += 1;
            }
            if (last == Branch::Drafted) != (branch == Branch::Drafted) {
                t.stats.draft_toggles += 1;
            }
        }
        t.last.insert(sid, (branch, target));
        if self.squad_trace_detail {
            let b = bail.unwrap_or_default();
            self.op_emit(
                "squad",
                json!({
                    "sid": sid, "branch": branch.name(), "t": self.frame / 30,
                    "x": pos.x.round(), "z": pos.z.round(), "tx": target.x.round(), "tz": target.z.round(),
                    "at_target": b.at_target, "at_member": b.at_member, "own": b.own, "straggle": b.straggle.round(),
                }),
            );
        }
    }

    /// The summary record (see the module docs).
    pub(crate) fn squad_trace_update(&mut self, frame: i32) {
        if frame % REPORT != (self.offset + 23) % REPORT {
            return;
        }
        let alive: Vec<SquadId> = self.raider_handler.raider_squads.iter().copied().collect();
        let t = &mut self.squad_trace;
        t.last.retain(|sid, _| alive.contains(sid));
        let s = std::mem::take(&mut t.stats);
        let mut worst: Vec<(SquadId, u32)> = std::mem::take(&mut t.flips).into_iter().collect();
        worst.sort_by_key(|(_, n)| std::cmp::Reverse(*n));
        worst.truncate(3);
        if s.decisions == 0 {
            return;
        }
        self.op_emit(
            "squads",
            json!({
                "t": frame / 30, "squads": alive.len(), "decisions": s.decisions,
                "to_haven": s.to_haven, "from_haven": s.from_haven, "switches": s.switches,
                "block_toggles": s.block_toggles, "draft_toggles": s.draft_toggles,
                "bail_target": s.bail_target, "bail_member": s.bail_member, "bail_straggler": s.bail_straggler,
                "worst": worst.iter().map(|(sid, n)| json!([sid, n])).collect::<Vec<_>>(),
            }),
        );
    }
}
