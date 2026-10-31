//! The AI's side of banter: the lexicon and budget it plays with, the
//! standing facts lines test, events from the game, and saying the lines.

use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

use serde_json::json;

use super::{Banter, Channel, Fields, Lexicon, Say, Val, fields, speakers};
use crate::Ai;

/// The budget from the lobby option `banter` (else `ZKGBAI_BANTER`): a
/// number, or off; empty or unreadable: the lexicon's.
pub fn budget_option(v: Option<&str>) -> Option<u32> {
    let v = v?.trim().to_ascii_lowercase();
    match v.as_str() {
        "" => None,
        "off" | "no" | "false" | "none" => Some(0),
        n => n.parse::<f32>().ok().filter(|n| *n >= 0.0).map(|n| n.round() as u32),
    }
}

/// Players' files, in the order they apply: AI/Skirmish/ZKGBAI/banter.toml
/// (every version), then this version's folder.
pub fn player_files(writeable: Option<&str>) -> Vec<PathBuf> {
    let Some(dir) = writeable.map(Path::new) else { return Vec::new() };
    let mut files: Vec<PathBuf> = dir.parent().map(|p| p.join("banter.toml")).into_iter().collect();
    files.push(dir.join("banter.toml"));
    files
}

/// Where the claim "it's over" stands.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Claim {
    #[default]
    None,
    Ahead,
    Turned,
    Recovered,
}

impl Ai {
    pub(crate) fn banter_init(&mut self) {
        let mut warnings = Vec::new();
        let mut lex = Lexicon::builtin();
        for path in player_files(self.cb.writeable_dir().as_deref()) {
            let Ok(text) = std::fs::read_to_string(&path) else { continue };
            match lex.clone().with(&text, &mut warnings) {
                Ok(l) => {
                    lex = l;
                    self.log(&format!("banter: read {}", path.display()));
                }
                Err(e) => self.log(&format!("banter: {} ignored, not TOML: {e}", path.display())),
            }
        }
        for w in warnings {
            self.log(&format!("banter: {w}"));
        }
        let option = self.cb.option("banter").filter(|v| !v.trim().is_empty());
        let budget = budget_option(option.or_else(|| std::env::var("ZKGBAI_BANTER").ok()).as_deref());
        let script = self.cb.game().setup_script();
        let (me, siblings) = speakers(&script, self.team_id, self.ally_team_id);
        let mut h = std::collections::hash_map::DefaultHasher::new();
        (&script, self.team_id).hash(&mut h);
        let b = Banter::new(lex, me, siblings, h.finish(), budget);
        self.log(&format!("banter: speaking as <{}>, {} quips a game shared", b.tag(), b.budget));
        self.banter = Some(b);
    }

    fn banter_state(&self) -> Fields {
        let ospots = self.graph.get_owned_spots().len();
        let espots = self.graph.get_enemy_spots().len();
        let (army, enemy_army) = (self.fac.army_value, self.war.enemy_fighter_value);
        let enemies = self.enemy_teams.len();
        fields([
            ("ospots", ospots.into()),
            ("espots", espots.into()),
            ("army", army.into()),
            ("enemy_army", enemy_army.into()),
            ("enemies", enemies.into()),
            ("allies", self.allies.len().into()),
            ("behind", (ospots < espots).into()),
            ("lobster", (if enemies > 1 { "lobsters" } else { "lobster" }).into()),
            ("min", (self.frame / 1800).into()),
        ])
    }

    pub(crate) fn banter_update(&mut self, frame: i32) {
        if self.banter.is_none() {
            return;
        }
        if frame % 300 == 0 {
            let state = self.banter_state();
            if let Some(b) = self.banter.as_mut() {
                b.set_state(state);
            }
        }
        if frame % 5 == 0 {
            let says = self.banter.as_mut().map(|b| b.due(frame)).unwrap_or_default();
            for s in says {
                self.banter_say(&s);
            }
        }
    }

    pub(crate) fn banter_say(&self, s: &Say) {
        let tag = self.banter.as_ref().map_or("ZKGBAI", |b| b.tag());
        let (prefix, to) = match s.to {
            Channel::All => ("", "all"),
            Channel::Allies => ("a:", "allies"),
            Channel::Specs => ("s:", "specs"),
        };
        self.cb.game().send_text_message(&format!("/say {prefix}<{tag}> {}", s.text), 0);
        self.op_emit("say", json!({"text": s.text, "id": s.id, "to": to}));
    }

    /// Something happened (see the events in `data/banter.toml`).
    pub(crate) fn banter_offer(&mut self, event: &str, extra: Fields) {
        let frame = self.frame;
        if let Some(b) = self.banter.as_mut() {
            b.offer(frame, event, &extra);
        }
    }

    pub(crate) fn banter_hear(&mut self, text: &str) {
        let frame = self.frame;
        if let Some(b) = self.banter.as_mut() {
            b.hear(frame, text);
        }
    }

    /// The game starts.
    pub(crate) fn banter_start(&mut self) {
        if self.banter.is_some() {
            self.banter_offer("start", Fields::new());
        } else {
            self.say("glhf!");
        }
    }

    /// We resign: the gg, said at once.
    pub(crate) fn banter_resign(&mut self) {
        let frame = self.frame;
        match self.banter.as_mut() {
            Some(b) => {
                for s in b.offer_now(frame, "resign", &Fields::new()) {
                    self.banter_say(&s);
                }
            }
            None => self.say("gg!"),
        }
    }

    /// The claim: well ahead (taunt), then behind (turned), then well ahead
    /// again (recovered). Lines on `turned` and `recovered` follow up what
    /// was said, so nothing comes of them if no taunt was.
    pub(crate) fn banter_claim(&mut self, ahead: bool, behind: bool) {
        let Some(b) = self.banter.as_mut() else { return };
        let next = match (b.claim, ahead, behind) {
            (Claim::Turned, true, _) => Some((Claim::Recovered, "recovered")),
            (_, true, _) => Some((Claim::Ahead, "taunt")),
            (Claim::Ahead | Claim::Recovered, false, true) => Some((Claim::Turned, "turned")),
            _ => None,
        };
        if let Some((claim, event)) = next {
            b.claim = claim;
            self.banter_offer(event, Fields::new());
        }
    }

    pub(crate) fn banter_superweapon(&mut self, def_name: &str) {
        let weapon = match def_name {
            "staticnuke" => "nuke",
            "zenith" => "zenith",
            "mahlazer" => "starlight",
            "raveparty" => "drp",
            other => other,
        };
        self.banter_offer("superweapon", fields([("weapon", weapon.into())]));
    }

    /// One of ours died: a silo, or something shiny.
    pub(crate) fn banter_unit_destroyed(&mut self, unit: springai::Unit) {
        if unit.is_being_built() {
            return;
        }
        let Some(name) = unit.def().map(|d| self.defs.name(d).to_owned()) else { return };
        match name.as_str() {
            "energysingu" => self.banter_offer("shiny_ours", fields([("what", "singu".into())])),
            "energyheavygeo" => self.banter_offer("shiny_ours", fields([("what", "geo".into())])),
            crate::military::missiles::SILO => {
                let fired = self.missiles_fired();
                self.banter_offer("silo_lost", fields([("fired", Val::from(fired as f64))]));
            }
            _ => {}
        }
    }
}
