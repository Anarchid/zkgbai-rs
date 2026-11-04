//! Banter: what the AI says in chat, and when.
//!
//! Lines come from a lexicon (`data/banter.toml`, built in, with a player's
//! file on top; see [`lexicon`]). When something happens, the AI offers an
//! event with its fields; the most specific line whose conditions hold is
//! held for a moment and said, unless another ZKGBAI in the game spoke
//! first. Each instance remembers what it said, so later lines can follow
//! up earlier ones ("It's over" ... "It was not over").
//!
//! Every ZKGBAI hears every chat line (the engine hands each AI all of them,
//! without saying to whom they went), so the instances in a game coordinate
//! through chat alone, even when hosted by different players: each counts
//! everyone's quips against one shared budget, keeps quiet for a while after
//! any of them, and may answer another's line once. Human chat is never
//! acted on: it may be the enemy team's ally chat. Lines to allies are
//! likewise ignored when heard from an enemy instance.
//!
//! This module is pure (no engine calls): the AI feeds it events, state and
//! heard chat, and says what [`Banter::due`] hands back.

pub mod glue;
pub mod lexicon;

use std::collections::BTreeMap;

pub use lexicon::{Channel, Lexicon, Line};
use lexicon::{Cond, From, Op, Operand, Pattern, split_template};

/// Frames a second.
const FPS: f32 = 30.0;

#[derive(Clone, Debug, PartialEq)]
pub enum Val {
    Num(f64),
    Str(String),
}

impl Val {
    fn truthy(&self) -> bool {
        match self {
            Val::Num(n) => *n != 0.0,
            Val::Str(s) => !s.is_empty(),
        }
    }
}

impl std::convert::From<f64> for Val {
    fn from(n: f64) -> Val {
        Val::Num(n)
    }
}

impl std::convert::From<f32> for Val {
    fn from(n: f32) -> Val {
        Val::Num(n as f64)
    }
}

impl std::convert::From<i32> for Val {
    fn from(n: i32) -> Val {
        Val::Num(n as f64)
    }
}

impl std::convert::From<usize> for Val {
    fn from(n: usize) -> Val {
        Val::Num(n as f64)
    }
}

impl std::convert::From<bool> for Val {
    fn from(b: bool) -> Val {
        Val::Num(if b { 1.0 } else { 0.0 })
    }
}

impl std::convert::From<&str> for Val {
    fn from(s: &str) -> Val {
        Val::Str(s.to_owned())
    }
}

impl std::convert::From<String> for Val {
    fn from(s: String) -> Val {
        Val::Str(s)
    }
}

pub type Fields = BTreeMap<String, Val>;

/// Fields from pairs: `fields([("n", 3.into())])`.
pub fn fields<const N: usize>(pairs: [(&str, Val); N]) -> Fields {
    pairs.into_iter().map(|(k, v)| (k.to_owned(), v)).collect()
}

/// A ZKGBAI in the game: the name its lines carry, its team and alliance.
#[derive(Clone, Debug, PartialEq)]
pub struct Speaker {
    pub tag: String,
    pub team: i32,
    pub ally_team: i32,
}

/// One chat message to send.
#[derive(Clone, Debug, PartialEq)]
pub struct Say {
    pub to: Channel,
    pub text: String,
    /// The line it is (or continues).
    pub id: String,
    pub marker: Option<[f32; 3]>,
}

struct Pending {
    line: usize,
    /// A reply to a line to allies: to allies too.
    to_allies: bool,
    texts: Vec<String>,
    event: String,
    due: i32,
    until: i32,
    spec: f32,
}

struct Mark {
    event: String,
    pos: [f32; 3],
    extra: Fields,
    due: i32,
    until: i32,
}

pub struct Banter {
    pub lex: Lexicon,
    pub budget: u32,
    me: Speaker,
    siblings: Vec<Speaker>,
    rng: u64,
    used: u32,
    last_quip: i32,
    said: Vec<String>,
    heard: Vec<String>,
    nth: BTreeMap<String, u32>,
    spoken: BTreeMap<String, u32>,
    last_event: BTreeMap<String, i32>,
    state: Fields,
    pending: Vec<Pending>,
    marks: Vec<Mark>,
    /// Where the claim "it's over" stands (kept by the AI, see `glue`).
    pub claim: glue::Claim,
}

impl Banter {
    /// `budget`: the lobby option's override of the lexicon's.
    pub fn new(lex: Lexicon, me: Speaker, siblings: Vec<Speaker>, seed: u64, budget: Option<u32>) -> Banter {
        let budget = budget.unwrap_or(lex.settings.budget);
        Banter {
            lex,
            budget,
            me,
            siblings,
            rng: seed | 1,
            used: 0,
            last_quip: i32::MIN / 2,
            said: Vec::new(),
            heard: Vec::new(),
            nth: BTreeMap::new(),
            spoken: BTreeMap::new(),
            last_event: BTreeMap::new(),
            state: Fields::new(),
            pending: Vec::new(),
            marks: Vec::new(),
            claim: glue::Claim::None,
        }
    }

    /// The name this instance's lines carry.
    pub fn tag(&self) -> &str {
        &self.me.tag
    }

    /// Quips said in the game so far, by every ZKGBAI.
    pub fn used(&self) -> u32 {
        self.used
    }

    /// The standing facts lines may test (spots, armies, ...).
    pub fn set_state(&mut self, state: Fields) {
        self.state = state;
    }

    fn rand(&mut self) -> f32 {
        // xorshift64*
        self.rng ^= self.rng >> 12;
        self.rng ^= self.rng << 25;
        self.rng ^= self.rng >> 27;
        (self.rng.wrapping_mul(0x2545_F491_4F6C_DD1D) >> 40) as f32 / (1u64 << 24) as f32
    }

    fn count(list: &[String], pattern: &str) -> usize {
        let p = Pattern::parse(pattern);
        list.iter().filter(|id| p.matches(id)).count()
    }

    fn lookup(&self, name: &str, env: &Fields) -> Option<Val> {
        if let Some(p) = name.strip_prefix("said:") {
            return Some(Val::from(Self::count(&self.said, p)));
        }
        if let Some(p) = name.strip_prefix("heard:") {
            return Some(Val::from(Self::count(&self.heard, p)));
        }
        env.get(name).cloned()
    }

    fn holds(&self, c: &Cond, env: &Fields) -> bool {
        match c {
            Cond::Truthy { name, neg } => self.lookup(name, env).is_some_and(|v| v.truthy()) != *neg,
            Cond::Cmp { name, op, rhs } => {
                let Some(lhs) = self.lookup(name, env) else { return false };
                let rhs = match rhs {
                    Operand::Num(n) => Val::Num(*n),
                    Operand::Word(w) => self.lookup(w, env).unwrap_or_else(|| Val::Str(w.clone())),
                };
                match (lhs, rhs) {
                    (Val::Num(a), Val::Num(b)) => match op {
                        Op::Eq => a == b,
                        Op::Ne => a != b,
                        Op::Lt => a < b,
                        Op::Le => a <= b,
                        Op::Gt => a > b,
                        Op::Ge => a >= b,
                    },
                    (Val::Str(a), Val::Str(b)) => match op {
                        Op::Eq => a.eq_ignore_ascii_case(&b),
                        Op::Ne => !a.eq_ignore_ascii_case(&b),
                        _ => false,
                    },
                    _ => false,
                }
            }
        }
    }

    /// The line said with these fields; `None` if one is missing.
    fn render(line: &Line, env: &Fields) -> Option<Vec<String>> {
        line.say
            .iter()
            .map(|t| {
                let (lits, holes) = split_template(t).ok()?;
                let mut out = lits[0].to_owned();
                for (hole, lit) in holes.iter().zip(&lits[1..]) {
                    out += &format_val(env.get(hole.name)?, hole.format);
                    out += lit;
                }
                Some(out)
            })
            .collect()
    }

    /// The best line among `candidates` (indices) for `env`, with its text.
    fn choose(&mut self, candidates: Vec<usize>, env: &Fields) -> Option<(usize, Vec<String>, f32)> {
        let ok: Vec<(usize, Vec<String>, f32)> = candidates
            .into_iter()
            .filter_map(|i| {
                let line = &self.lex.lines[i];
                if !line.when.iter().all(|c| self.holds(c, env)) {
                    return None;
                }
                Some((i, Self::render(line, env)?, line.specificity()))
            })
            .collect();
        let best = ok.iter().map(|c| c.2).fold(f32::NEG_INFINITY, f32::max);
        let top: Vec<_> = ok.into_iter().filter(|c| c.2 == best).collect();
        let total: f32 = top.iter().map(|c| self.lex.lines[c.0].weight.max(0.0)).sum();
        if top.is_empty() || total <= 0.0 {
            return None;
        }
        let mut r = self.rand() * total;
        for c in &top {
            r -= self.lex.lines[c.0].weight.max(0.0);
            if r <= 0.0 {
                return Some(c.clone());
            }
        }
        top.last().cloned()
    }

    fn usable(&self, line: &Line, frame: i32, reply: bool) -> bool {
        if self.lex.is_off(&line.id) || (!line.repeat && self.said.contains(&line.id)) {
            return false;
        }
        if line.free {
            return true;
        }
        let quiet = (self.lex.settings.quiet * FPS) as i32;
        self.used < self.budget && (reply || frame - self.last_quip >= quiet)
    }

    fn hold(&mut self, frame: i32) -> i32 {
        let (lo, hi) = self.lex.settings.hold;
        // Instances held alike part by team, so two seldom speak at once.
        let jitter = (self.me.team.rem_euclid(8)) * 4;
        frame + ((lo + (hi - lo) * self.rand()) * FPS) as i32 + jitter
    }

    /// Something happened: maybe a line for it, said after a hold.
    pub fn offer(&mut self, frame: i32, event: &str, extra: &Fields) {
        let nth = {
            let n = self.nth.entry(event.to_owned()).or_insert(0);
            *n += 1;
            *n
        };
        let s = &self.lex.settings;
        if let Some(&cool) = s.cooldown.get(event)
            && let Some(&last) = self.last_event.get(event)
            && frame - last < (cool * FPS) as i32
        {
            return;
        }
        if let Some(&limit) = s.limit.get(event)
            && self.spoken.get(event).copied().unwrap_or(0) >= limit
        {
            return;
        }
        self.last_event.insert(event.to_owned(), frame);
        let mut env = self.state.clone();
        env.insert("nth".to_owned(), Val::from(nth as f64));
        env.extend(extra.iter().map(|(k, v)| (k.clone(), v.clone())));
        let candidates: Vec<usize> = (0..self.lex.lines.len())
            .filter(|&i| {
                !self.lex.lines[i].marker
                    && self.lex.lines[i].on == event
                    && self.usable(&self.lex.lines[i], frame, false)
            })
            .collect();
        let Some((line, texts, spec)) = self.choose(candidates, &env) else { return };
        let free = self.lex.lines[line].free;
        // One quip held at a time: a more specific one takes its place.
        if !free && let Some(i) = self.pending.iter().position(|p| !self.lex.lines[p.line].free) {
            if self.pending[i].spec >= spec {
                return;
            }
            self.pending.remove(i);
        }
        let until = frame + (self.lex.settings.expire * FPS) as i32;
        let due = self.hold(frame);
        self.pending.push(Pending { line, to_allies: false, texts, event: event.to_owned(), due, until, spec });
    }

    /// Say a line for the event now, without a hold (resigning: the AI is
    /// about to stop).
    pub fn offer_now(&mut self, frame: i32, event: &str, extra: &Fields) -> Vec<Say> {
        self.offer(frame, event, extra);
        for p in self.pending.iter_mut().filter(|p| p.event == event) {
            p.due = frame;
        }
        self.due(frame)
    }

    /// A chat line came in. Only other ZKGBAIs' lines count.
    pub fn hear(&mut self, frame: i32, text: &str) {
        let Some((tag, body)) = text.strip_prefix('<').and_then(|t| t.split_once("> ")) else { return };
        if tag == self.me.tag {
            return;
        }
        // Only the ZKGBAIs in the start script (a human typing "<x> ..." is
        // a human; so is an older ZKGBAI, which says plain "<ZKGBAI>").
        let Some(allied) = self.siblings.iter().find(|s| s.tag == tag).map(|s| s.ally_team == self.me.ally_team) else {
            return;
        };
        let side = Some(allied);
        // Ambiguous text reads as a reply (which is never answered).
        let lines = self.lex.recognise(body);
        let Some(line) = lines.iter().find(|l| l.is_reply()).or(lines.first()).copied() else { return };
        if (line.to == Channel::Allies || line.marker) && !allied {
            return;
        }
        let (id, free, reply, event) = (line.id.clone(), line.free, line.is_reply(), line.on.clone());
        let to_allies = line.to == Channel::Allies;
        self.heard.push(id.clone());
        // Markers carry tactical intentions, not a shared joke budget or
        // a request that uninvolved allies should answer.
        if line.marker {
            return;
        }
        if free {
            // Another said the moment's free line (glhf): ours is not needed.
            self.pending.retain(|p| p.event != event);
        } else {
            self.used += 1;
            self.last_quip = frame;
            let lines = &self.lex.lines;
            // Someone quipped: ours waits. Someone answered: ours is moot.
            self.pending.retain(|p| lines[p.line].free || (lines[p.line].is_reply() && !reply));
        }
        if reply {
            return;
        }
        let candidates: Vec<usize> = (0..self.lex.lines.len())
            .filter(|&i| {
                let l = &self.lex.lines[i];
                let from_ok = match l.from {
                    From::Any => true,
                    From::Ally => side == Some(true),
                    From::Enemy => side == Some(false),
                };
                l.is_reply()
                    && from_ok
                    && l.answers.as_ref().is_some_and(|a| a.matches(&id))
                    && self.usable(l, frame, true)
            })
            .collect();
        let env = self.state.clone();
        if let Some((line, texts, spec)) = self.choose(candidates, &env) {
            let until = frame + (self.lex.settings.reply_within * FPS) as i32;
            let due = self.hold(frame);
            self.pending.push(Pending { line, to_allies, texts, event: "reply".to_owned(), due, until, spec });
        }
    }

    /// The lines to say now.
    pub fn due(&mut self, frame: i32) -> Vec<Say> {
        let mut out = Vec::new();
        let mut keep = Vec::new();
        for p in std::mem::take(&mut self.pending) {
            if p.due > frame {
                keep.push(p);
                continue;
            }
            let line = &self.lex.lines[p.line];
            if frame > p.until || !self.usable(line, frame, line.is_reply()) {
                continue;
            }
            let to = if p.to_allies { Channel::Allies } else { line.to };
            let (id, free) = (line.id.clone(), line.free);
            out.extend(p.texts.into_iter().map(|text| Say { to, text, id: id.clone(), marker: None }));
            self.said.push(id);
            *self.spoken.entry(p.event).or_insert(0) += 1;
            if !free {
                self.used += 1;
                self.last_quip = frame;
            }
        }
        self.pending = keep;
        self.due_marks(frame, &mut out);
        out
    }

    /// Tactical labels share the lexicon but have their own small per-AI
    /// limit. Banter off silences them; ordinary quiet time does not hide
    /// the other commanders' destinations.
    pub fn mark(&mut self, frame: i32, event: &str, pos: [f32; 3], extra: &Fields) {
        if self.budget == 0 || self.marks.iter().any(|m| m.event == event) {
            return;
        }
        let limit = self.lex.settings.limit.get(event).copied().unwrap_or(2);
        if self.spoken.get(event).copied().unwrap_or(0) >= limit {
            return;
        }
        // Rank among allies gives simultaneous commitments time to hear
        // one another before selecting the next line in the story.
        let rank = self.siblings.iter().filter(|s| s.ally_team == self.me.ally_team && s.team < self.me.team).count();
        self.marks.push(Mark {
            event: event.into(),
            pos,
            extra: extra.clone(),
            due: frame + 30 + rank as i32 * 30,
            until: frame + 30 * 60,
        });
    }

    fn due_marks(&mut self, frame: i32, out: &mut Vec<Say>) {
        for m in std::mem::take(&mut self.marks) {
            if m.due > frame {
                self.marks.push(m);
                continue;
            }
            if self.budget == 0 || frame > m.until {
                continue;
            }
            let mut env = self.state.clone();
            env.extend(m.extra);
            let candidates = (0..self.lex.lines.len())
                .filter(|&i| {
                    let l = &self.lex.lines[i];
                    l.marker && l.on == m.event && !self.lex.is_off(&l.id) && (l.repeat || !self.said.contains(&l.id))
                })
                .collect();
            if let Some((i, texts, _)) = self.choose(candidates, &env) {
                let id = self.lex.lines[i].id.clone();
                // A marker is one label, even when an override has several texts.
                out.push(Say { to: Channel::Allies, text: texts.join(" "), id: id.clone(), marker: Some(m.pos) });
                self.said.push(id);
                *self.spoken.entry(m.event).or_insert(0) += 1;
            }
        }
    }
}

/// A field's value in a text.
fn format_val(v: &Val, format: Option<&str>) -> String {
    match (v, format) {
        (Val::Num(n), Some("k")) if n.abs() >= 1000.0 => {
            let k = format!("{:.1}", n / 1000.0);
            format!("{}k", k.trim_end_matches(".0"))
        }
        (Val::Num(n), _) => format!("{}", n.round() as i64),
        (Val::Str(s), Some("cap")) => {
            let mut c = s.chars();
            c.next().map(|f| f.to_uppercase().chain(c).collect()).unwrap_or_default()
        }
        (Val::Str(s), _) => s.clone(),
    }
}

/// The ZKGBAIs in a game, from the start script: (name, team, ally team)
/// of each AI whose short name is ZKGBAI.
pub fn zkgbais_in_script(script: &str) -> Vec<(String, i32, i32)> {
    let mut ais: Vec<BTreeMap<String, String>> = Vec::new();
    let mut team_ally: BTreeMap<i32, i32> = BTreeMap::new();
    let mut section: Option<(String, BTreeMap<String, String>)> = None;
    let mut depth = 0;
    let mut top = 0;
    for raw in script.lines() {
        let line = raw.trim();
        let lower = line.to_ascii_lowercase();
        if let Some(head) = lower.strip_prefix('[').and_then(|l| l.strip_suffix(']'))
            && (head.starts_with("ai") || head.starts_with("team"))
        {
            section = Some((head.to_owned(), BTreeMap::new()));
            top = depth;
        }
        depth += line.matches('{').count() as i32;
        depth -= line.matches('}').count() as i32;
        if let Some((_, kv)) = section.as_mut()
            && depth == top + 1
            && let Some((k, v)) = line.trim_end_matches(';').split_once('=')
        {
            kv.insert(k.trim().to_ascii_lowercase(), v.trim().to_owned());
        }
        if line.contains('}')
            && depth <= top
            && let Some((head, kv)) = section.take()
        {
            if let Some(n) = head.strip_prefix("team").and_then(|n| n.parse::<i32>().ok()) {
                if let Some(a) = kv.get("allyteam").and_then(|a| a.parse().ok()) {
                    team_ally.insert(n, a);
                }
            } else if head.starts_with("ai") {
                ais.push(kv);
            }
        }
    }
    ais.into_iter()
        .filter(|kv| kv.get("shortname").is_some_and(|s| s.eq_ignore_ascii_case("zkgbai")))
        .filter_map(|kv| {
            let team: i32 = kv.get("team")?.parse().ok()?;
            let name = kv.get("name").cloned().unwrap_or_else(|| format!("ZKGBAI {team}"));
            Some((name, team, team_ally.get(&team).copied().unwrap_or(team)))
        })
        .collect()
}

/// Who this instance is and the other ZKGBAIs: lines carry the AI's lobby
/// name when there is more than one of us (with its team when two share a
/// name), else plain "ZKGBAI".
pub fn speakers(script: &str, my_team: i32, my_ally_team: i32) -> (Speaker, Vec<Speaker>) {
    let all = zkgbais_in_script(script);
    let tag = |name: &str, team: i32| {
        if all.len() <= 1 {
            "ZKGBAI".to_owned()
        } else if all.iter().filter(|a| a.0 == name).count() > 1 {
            format!("{name} #{team}")
        } else {
            name.to_owned()
        }
    };
    let me = Speaker {
        tag: all.iter().find(|a| a.1 == my_team).map_or_else(|| "ZKGBAI".to_owned(), |a| tag(&a.0, a.1)),
        team: my_team,
        ally_team: my_ally_team,
    };
    let siblings = (all.iter().filter(|a| a.1 != my_team))
        .map(|a| Speaker { tag: tag(&a.0, a.1), team: a.1, ally_team: a.2 })
        .collect();
    (me, siblings)
}

#[cfg(test)]
mod tests;
