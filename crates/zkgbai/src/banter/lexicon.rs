//! The banter lexicon: settings and lines, read from TOML (the default in
//! `data/banter.toml`, built in; a player's file on top). See the default
//! file's header for the format.

use std::collections::BTreeMap;

use toml::{Table, Value};

/// Events the AI raises (a line on anything else is never said).
pub const EVENTS: [&str; 21] = [
    "start",
    "conquest",
    "rush",
    "resign",
    "taunt",
    "turned",
    "recovered",
    "hopeless",
    "inferno",
    "burnt",
    "eos",
    "silo_lost",
    "superweapon",
    "shiny",
    "shiny_ours",
    "raid_repelled",
    "raid_cost",
    "comm_escape",
    "comm_kill",
    "reply",
    // Said by nothing yet; lines may wait on it.
    "rescued",
];

#[derive(Clone, Debug, PartialEq)]
pub struct Settings {
    pub budget: u32,
    /// Seconds.
    pub quiet: f32,
    pub hold: (f32, f32),
    pub expire: f32,
    pub reply_within: f32,
    pub off: Vec<Pattern>,
    pub limit: BTreeMap<String, u32>,
    /// Seconds.
    pub cooldown: BTreeMap<String, f32>,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            budget: 6,
            quiet: 90.0,
            hold: (2.0, 5.0),
            expire: 30.0,
            reply_within: 20.0,
            off: Vec::new(),
            limit: BTreeMap::new(),
            cooldown: BTreeMap::new(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Channel {
    All,
    Allies,
    Specs,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum From {
    Any,
    Ally,
    Enemy,
}

/// Line ids: alternatives separated by "|", each exact or ending in "*".
#[derive(Clone, Debug, PartialEq)]
pub struct Pattern(Vec<String>);

impl Pattern {
    pub fn parse(s: &str) -> Pattern {
        Pattern(s.split('|').map(|p| p.trim().to_owned()).filter(|p| !p.is_empty()).collect())
    }

    pub fn matches(&self, id: &str) -> bool {
        self.0.iter().any(|p| match p.strip_suffix('*') {
            Some(prefix) => id.starts_with(prefix),
            None => p == id,
        })
    }

    pub fn is_glob(&self) -> bool {
        self.0.iter().any(|p| p.ends_with('*'))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Op {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Operand {
    Num(f64),
    /// A name if one is defined, else a word.
    Word(String),
}

#[derive(Clone, Debug, PartialEq)]
pub enum Cond {
    /// "name" (non-zero) or "!name" (zero or absent).
    Truthy {
        name: String,
        neg: bool,
    },
    Cmp {
        name: String,
        op: Op,
        rhs: Operand,
    },
}

impl Cond {
    pub fn parse(s: &str) -> Result<Cond, String> {
        let s = s.trim();
        for (tok, op) in [("==", Op::Eq), ("!=", Op::Ne), ("<=", Op::Le), (">=", Op::Ge), ("<", Op::Lt), (">", Op::Gt)]
        {
            if let Some(i) = s.find(tok) {
                let name = s[..i].trim();
                let rhs = s[i + tok.len()..].trim();
                if name.is_empty() || rhs.is_empty() {
                    return Err(format!("condition \"{s}\": a name and a value either side of {tok}"));
                }
                let rhs = rhs.parse::<f64>().map_or_else(|_| Operand::Word(rhs.to_owned()), Operand::Num);
                return Ok(Cond::Cmp { name: name.to_owned(), op, rhs });
            }
        }
        let (neg, name) = match s.strip_prefix('!') {
            Some(n) => (true, n.trim()),
            None => (false, s),
        };
        if name.is_empty() || name.contains(char::is_whitespace) {
            return Err(format!("condition \"{s}\": not a name or a comparison"));
        }
        Ok(Cond::Truthy { name: name.to_owned(), neg })
    }

    pub fn name(&self) -> &str {
        match self {
            Cond::Truthy { name, .. } | Cond::Cmp { name, .. } => name,
        }
    }

    /// How much it makes a line specific: half for a pattern ending in "*".
    pub fn specificity(&self) -> f32 {
        let name = self.name();
        let pattern = name.strip_prefix("said:").or_else(|| name.strip_prefix("heard:"));
        match pattern {
            Some(p) if Pattern::parse(p).is_glob() => 0.5,
            _ => 1.0,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Line {
    pub id: String,
    pub on: String,
    pub say: Vec<String>,
    pub when: Vec<Cond>,
    pub to: Channel,
    pub weight: f32,
    pub free: bool,
    pub marker: bool,
    pub repeat: bool,
    pub answers: Option<Pattern>,
    pub from: From,
}

impl Line {
    pub fn specificity(&self) -> f32 {
        self.when.iter().map(Cond::specificity).sum()
    }

    pub fn is_reply(&self) -> bool {
        self.on == "reply"
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Lexicon {
    pub settings: Settings,
    pub lines: Vec<Line>,
}

pub const DEFAULT: &str = include_str!("../../data/banter.toml");

impl Lexicon {
    /// The built-in lexicon.
    pub fn builtin() -> Lexicon {
        let mut warnings = Vec::new();
        let lex = Lexicon::default().with(DEFAULT, &mut warnings).expect("the built-in banter.toml parses");
        debug_assert!(warnings.is_empty(), "built-in banter.toml: {warnings:?}");
        lex
    }

    /// This lexicon with a file on top: settings it names, lines it adds or
    /// replaces (by id), ids it switches off. Problems that leave the rest
    /// usable (an unknown key, a bad line) are warnings; a file that is not
    /// TOML at all is an error and changes nothing.
    pub fn with(mut self, text: &str, warnings: &mut Vec<String>) -> Result<Lexicon, String> {
        let table: Table = text.parse().map_err(|e: toml::de::Error| e.to_string())?;
        for (key, value) in &table {
            match (key.as_str(), value) {
                ("settings", Value::Table(t)) => self.settings_from(t, warnings),
                ("line", Value::Array(lines)) => {
                    for (i, v) in lines.iter().enumerate() {
                        match v.as_table().ok_or_else(|| "not a table".to_owned()).and_then(|t| line_from(t, warnings))
                        {
                            Ok(line) => match self.lines.iter_mut().find(|l| l.id == line.id) {
                                Some(old) => *old = line,
                                None => self.lines.push(line),
                            },
                            Err(e) => warnings.push(format!("line {} skipped: {e}", i + 1)),
                        }
                    }
                }
                _ => warnings.push(format!("unknown top-level key \"{key}\" (expected [settings] or [[line]])")),
            }
        }
        Ok(self)
    }

    fn settings_from(&mut self, t: &Table, warnings: &mut Vec<String>) {
        let s = &mut self.settings;
        for (key, v) in t {
            let num = v.as_float().or_else(|| v.as_integer().map(|i| i as f64));
            match (key.as_str(), num) {
                ("budget", Some(n)) if n >= 0.0 => s.budget = n as u32,
                ("quiet", Some(n)) if n >= 0.0 => s.quiet = n as f32,
                ("expire", Some(n)) if n >= 0.0 => s.expire = n as f32,
                ("reply_within", Some(n)) if n >= 0.0 => s.reply_within = n as f32,
                ("hold", _) => match v.as_array().map(|a| a.iter().map(as_num).collect::<Vec<_>>()) {
                    Some(a) if a.len() == 2 && a.iter().all(|x| x.is_some_and(|x| x >= 0.0)) => {
                        let (lo, hi) = (a[0].unwrap() as f32, a[1].unwrap() as f32);
                        s.hold = (lo.min(hi), lo.max(hi));
                    }
                    _ => warnings.push("settings.hold: expected [low, high] seconds".to_owned()),
                },
                ("off", _) => match strings(v) {
                    Some(ids) => s.off.extend(ids.iter().map(|i| Pattern::parse(i))),
                    None => warnings.push("settings.off: expected a list of ids".to_owned()),
                },
                ("limit" | "cooldown", _) => match v.as_table() {
                    Some(tt) => {
                        for (event, n) in tt {
                            match as_num(n) {
                                Some(n) if n >= 0.0 => {
                                    if key == "limit" {
                                        s.limit.insert(event.clone(), n as u32);
                                    } else {
                                        s.cooldown.insert(event.clone(), n as f32);
                                    }
                                }
                                _ => warnings.push(format!("settings.{key}.{event}: expected a number")),
                            }
                        }
                    }
                    None => warnings.push(format!("settings.{key}: expected a table of event = number")),
                },
                _ => warnings.push(format!("settings.{key}: unknown key or bad value")),
            }
        }
    }

    pub fn is_off(&self, id: &str) -> bool {
        self.settings.off.iter().any(|p| p.matches(id))
    }

    /// Lines whose first message reads as `text` (the fields aside).
    pub fn recognise(&self, text: &str) -> Vec<&Line> {
        self.lines.iter().filter(|l| l.say.first().is_some_and(|t| template_matches(t, text))).collect()
    }
}

fn as_num(v: &Value) -> Option<f64> {
    v.as_float().or_else(|| v.as_integer().map(|i| i as f64))
}

fn strings(v: &Value) -> Option<Vec<String>> {
    match v {
        Value::String(s) => Some(vec![s.clone()]),
        Value::Array(a) => a.iter().map(|x| x.as_str().map(str::to_owned)).collect(),
        _ => None,
    }
}

fn line_from(t: &Table, warnings: &mut Vec<String>) -> Result<Line, String> {
    let get = |k: &str| t.get(k).and_then(Value::as_str).map(str::to_owned);
    let id = get("id").ok_or("no id")?;
    let on = get("on").ok_or_else(|| format!("{id}: no `on` (the event)"))?;
    let say = t.get("say").and_then(strings).filter(|s| !s.is_empty()).ok_or_else(|| format!("{id}: no `say`"))?;
    if !EVENTS.contains(&on.as_str()) {
        warnings.push(format!("{id}: unknown event \"{on}\" (never said)"));
    }
    for s in &say {
        check_template(s).map_err(|e| format!("{id}: {e}"))?;
    }
    let mut when = Vec::new();
    if let Some(v) = t.get("if") {
        for c in strings(v).ok_or_else(|| format!("{id}: `if` must be a list of conditions"))? {
            when.push(Cond::parse(&c).map_err(|e| format!("{id}: {e}"))?);
        }
    }
    let to = match get("to").as_deref() {
        None | Some("all") => Channel::All,
        Some("allies") => Channel::Allies,
        Some("specs") => Channel::Specs,
        Some(x) => return Err(format!("{id}: `to` is all, allies or specs, not \"{x}\"")),
    };
    let from = match get("from").as_deref() {
        None | Some("any") => From::Any,
        Some("ally") => From::Ally,
        Some("enemy") => From::Enemy,
        Some(x) => return Err(format!("{id}: `from` is any, ally or enemy, not \"{x}\"")),
    };
    let answers = get("answers").map(|a| Pattern::parse(&a));
    if (on == "reply") != answers.is_some() {
        return Err(format!("{id}: a reply (on = \"reply\") needs `answers`, and only a reply has it"));
    }
    let flag = |k: &str| t.get(k).and_then(Value::as_bool).unwrap_or(false);
    let weight = t.get("weight").and_then(as_num).unwrap_or(1.0) as f32;
    for k in t.keys() {
        if !["id", "on", "say", "if", "to", "weight", "free", "repeat", "answers", "from", "marker"]
            .contains(&k.as_str())
        {
            warnings.push(format!("{id}: unknown key \"{k}\""));
        }
    }
    Ok(Line {
        id,
        on,
        say,
        when,
        to,
        weight,
        free: flag("free"),
        marker: flag("marker"),
        repeat: flag("repeat"),
        answers,
        from,
    })
}

/// A field in a text: `{name}`, `{name:k}`, `{name:cap}`.
pub struct Hole<'a> {
    pub name: &'a str,
    pub format: Option<&'a str>,
}

/// The text's literal pieces and the fields between them (one more piece
/// than fields).
pub fn split_template(t: &str) -> Result<(Vec<&str>, Vec<Hole<'_>>), String> {
    let (mut lits, mut holes) = (Vec::new(), Vec::new());
    let mut rest = t;
    loop {
        match rest.find('{') {
            None => {
                if rest.contains('}') {
                    return Err(format!("\"{t}\": a stray }}"));
                }
                lits.push(rest);
                return Ok((lits, holes));
            }
            Some(i) => {
                let close = rest[i..].find('}').ok_or_else(|| format!("\"{t}\": an unclosed {{"))? + i;
                let (name, format) = match rest[i + 1..close].split_once(':') {
                    Some((n, f)) => (n.trim(), Some(f.trim())),
                    None => (rest[i + 1..close].trim(), None),
                };
                if name.is_empty() || !matches!(format, None | Some("k" | "cap")) {
                    return Err(format!("\"{t}\": bad field {{{}}}", &rest[i + 1..close]));
                }
                lits.push(&rest[..i]);
                holes.push(Hole { name, format });
                rest = &rest[close + 1..];
            }
        }
    }
}

fn check_template(t: &str) -> Result<(), String> {
    split_template(t).map(|_| ())
}

/// Whether `text` could be `template` said with some fields.
pub fn template_matches(template: &str, text: &str) -> bool {
    let Ok((lits, _)) = split_template(template) else { return false };
    let (first, last) = (lits[0], lits[lits.len() - 1]);
    if lits.len() == 1 {
        return text == first;
    }
    let Some(mut rest) = text.strip_prefix(first) else { return false };
    for lit in &lits[1..lits.len() - 1] {
        // A field is at least one character.
        match rest.get(1..).and_then(|r| r.find(lit)) {
            Some(i) => rest = &rest[1 + i + lit.len()..],
            None => return false,
        }
    }
    rest.len() > last.len() && rest.ends_with(last)
}
