use super::lexicon::template_matches;
use super::*;

const SEC: i32 = 30;

fn speaker(tag: &str, team: i32, ally_team: i32) -> Speaker {
    Speaker { tag: tag.to_owned(), team, ally_team }
}

/// A lone instance (no other ZKGBAI in the game).
fn alone(seed: u64) -> Banter {
    Banter::new(Lexicon::builtin(), speaker("ZKGBAI", 0, 0), vec![], seed, None)
}

/// Two instances, A on team 0, B on team 1, enemies unless `allied`.
fn pair(allied: bool) -> (Banter, Banter) {
    let a = speaker("ZKGBAI wip (1)", 0, 0);
    let b = speaker("ZKGBAI wip (2)", 1, if allied { 0 } else { 1 });
    (
        Banter::new(Lexicon::builtin(), a.clone(), vec![b.clone()], 11, None),
        Banter::new(Lexicon::builtin(), b, vec![a], 22, None),
    )
}

fn state(ospots: i32, espots: i32) -> Fields {
    fields([
        ("ospots", ospots.into()),
        ("espots", espots.into()),
        ("enemies", 1.into()),
        ("lobster", "lobster".into()),
        ("behind", (ospots < espots).into()),
    ])
}

/// Run `b` from `from` to `to` frames, collecting what it says.
fn run(b: &mut Banter, from: i32, to: i32) -> Vec<Say> {
    (from..=to).flat_map(|f| b.due(f)).collect()
}

/// Everything `speaker` says reaches `listener` as chat.
fn relay(said: &[Say], tag: &str, listener: &mut Banter, frame: i32) {
    for s in said {
        listener.hear(frame, &format!("<{tag}> {}", s.text));
    }
}

#[test]
fn builtin_lexicon_is_clean() {
    let mut warnings = Vec::new();
    let lex = Lexicon::default().with(lexicon::DEFAULT, &mut warnings).unwrap();
    assert!(warnings.is_empty(), "{warnings:?}");
    let mut ids: Vec<&str> = lex.lines.iter().map(|l| l.id.as_str()).collect();
    ids.sort();
    let n = ids.len();
    ids.dedup();
    assert_eq!(ids.len(), n, "duplicate ids");
    assert_eq!(lex.settings.budget, 6);
    assert_eq!(lex.settings.limit.get("taunt"), Some(&3));
}

#[test]
fn templates_match_what_they_render() {
    assert!(template_matches("Table for {n}.", "Table for 7."));
    assert!(!template_matches("Table for {n}.", "Table for ."));
    assert!(template_matches("{ospots} spots to {espots}. Resignable.", "31 spots to 6. Resignable."));
    assert!(!template_matches("{ospots} spots to {espots}. Resignable.", "Resignable."));
    assert!(template_matches("Resign {lobster}!", "Resign lobsters!"));
    assert!(template_matches("glhf!", "glhf!"));
    assert!(!template_matches("glhf!", "glhf"));
    assert_eq!(format_val(&Val::Num(1240.0), Some("k")), "1.2k");
    assert_eq!(format_val(&Val::Num(1000.0), Some("k")), "1k");
    assert_eq!(format_val(&Val::Num(950.4), Some("k")), "950");
    assert_eq!(format_val(&Val::from("west"), Some("cap")), "West");
}

/// The claim is held to account: whichever taunt was said, the reversal
/// and the gg follow up that one.
#[test]
fn a_taunt_is_followed_up_by_its_own_reversal_and_gg() {
    let follow = [
        ("taunt.help", "having trouble", "gg, took my own advice"),
        ("taunt.time", "ok it was not time", "gg, clock was wrong"),
        ("taunt.over", "ok not over", "gg, now it's over"),
        ("taunt.lobster", "lobster underestimated", "gg, lobster wins"),
    ];
    let mut seen = std::collections::BTreeSet::new();
    for seed in 1..40 {
        let mut b = alone(seed);
        b.set_state(state(30, 6));
        b.offer(1000, "taunt", &Fields::new());
        let said = run(&mut b, 1000, 1000 + 10 * SEC);
        let taunt = said[0].id.clone();
        seen.insert(taunt.clone());
        let (_, turned, gg) = follow.iter().find(|f| f.0 == taunt).expect("a first taunt");

        b.set_state(state(8, 20));
        b.offer(10_000, "turned", &Fields::new());
        let said = run(&mut b, 10_000, 10_000 + 10 * SEC);
        assert_eq!(said.len(), 1, "{taunt}: {said:?}");
        assert_eq!(said[0].text, *turned);

        let said = b.offer_now(20_000, "resign", &Fields::new());
        assert_eq!(said.len(), 1);
        assert_eq!(said[0].text, *gg);
    }
    assert_eq!(seen.len(), 4, "every opening taunt gets drawn: {seen:?}");
}

#[test]
fn a_second_taunt_calls_back_and_the_limit_holds() {
    let mut b = alone(5);
    b.set_state(state(30, 6));
    let mut texts = Vec::new();
    for k in 0..6 {
        let f = 1000 + k * 300 * SEC;
        b.offer(f, "taunt", &Fields::new());
        texts.extend(run(&mut b, f, f + 10 * SEC).into_iter().map(|s| s.id));
    }
    // Three a game (the limit), the later ones following up the first.
    let distinct: std::collections::BTreeSet<_> = texts.iter().collect();
    assert_eq!(distinct.len(), 3, "{texts:?}");
    assert!(
        texts[1..]
            .iter()
            .all(|id| ["taunt.count", "taunt.norush", "taunt.still", "taunt.stilltime"].contains(&id.as_str())),
        "{texts:?}"
    );
}

#[test]
fn quiet_and_budget_are_shared_across_instances() {
    let (mut a, mut b) = pair(false);
    a.set_state(state(30, 6));
    b.set_state(state(6, 30));
    a.offer(1000, "inferno", &fields([("n", 3.into()), ("value", 900.into())]));
    let said = run(&mut a, 1000, 1000 + 10 * SEC);
    assert_eq!(said[0].text, "It's cooking time!");
    relay(&said, a.tag(), &mut b, 1000 + 10 * SEC);
    assert_eq!(b.used(), 1);
    // B, hearing it, keeps quiet a while: its own quip waits.
    b.offer(1000 + 20 * SEC, "shiny", &Fields::new());
    assert!(run(&mut b, 1000 + 20 * SEC, 1000 + 60 * SEC).iter().all(|s| s.id.starts_with("re.")));
}

#[test]
fn an_enemy_taunt_gets_one_answer_and_no_answer_back() {
    let (mut a, mut b) = pair(false);
    a.set_state(state(30, 6));
    b.set_state(state(6, 30));
    {
        let seed_frame = 1000;
        a.offer(seed_frame, "taunt", &Fields::new());
    }
    let said_a = run(&mut a, 1000, 1000 + 10 * SEC);
    let first = said_a[0].id.clone();
    relay(&said_a[..1], a.tag(), &mut b, 1000 + 10 * SEC);
    let said_b = run(&mut b, 1000 + 10 * SEC, 1000 + 30 * SEC);
    assert_eq!(said_b.len(), 1, "{first}: {said_b:?}");
    // B is behind, so the honest answers win where there is one.
    let expected = match first.as_str() {
        "taunt.lobster" => "not a lobster",
        "taunt.time" => "not yet",
        "taunt.over" => "mostly over",
        "taunt.help" => "some",
        x => panic!("unexpected taunt {x}"),
    };
    assert_eq!(said_b[0].text, expected);
    // A hears the answer and lets it be.
    relay(&said_b, b.tag(), &mut a, 1000 + 30 * SEC);
    assert!(run(&mut a, 1000 + 30 * SEC, 1000 + 60 * SEC).is_empty());
    assert_eq!(a.used(), 2);
    assert_eq!(b.used(), 2);
}

#[test]
fn an_ally_seconds_a_taunt() {
    let (mut a, mut b) = pair(true);
    a.set_state(state(30, 6));
    b.set_state(state(30, 6));
    a.offer(1000, "taunt", &Fields::new());
    let said_a = run(&mut a, 1000, 1000 + 10 * SEC);
    relay(&said_a[..1], a.tag(), &mut b, 1000 + 10 * SEC);
    let said_b = run(&mut b, 1000 + 10 * SEC, 1000 + 30 * SEC);
    assert_eq!(said_b.iter().map(|s| s.text.as_str()).collect::<Vec<_>>(), ["seconded"]);
}

#[test]
fn glhf_once_per_side_and_the_enemy_says_u2() {
    let (mut a, mut b) = pair(false);
    a.offer(0, "start", &Fields::new());
    b.offer(0, "start", &Fields::new());
    let mut log = Vec::new();
    for f in 0..=10 * SEC {
        let sa = a.due(f);
        relay(&sa, a.tag(), &mut b, f);
        let sb = b.due(f);
        relay(&sb, b.tag(), &mut a, f);
        log.extend(sa.into_iter().map(|s| ("a", s.text)));
        log.extend(sb.into_iter().map(|s| ("b", s.text)));
    }
    let texts: Vec<&str> = log.iter().map(|l| l.1.as_str()).collect();
    assert_eq!(texts, ["glhf!", "u2"], "{log:?}");
    // Free lines spend no budget.
    assert_eq!(a.used() + b.used(), 0);

    let (mut a, mut b) = pair(true);
    a.offer(0, "start", &Fields::new());
    b.offer(0, "start", &Fields::new());
    let mut texts = Vec::new();
    for f in 0..=10 * SEC {
        let sa = a.due(f);
        relay(&sa, a.tag(), &mut b, f);
        let sb = b.due(f);
        relay(&sb, b.tag(), &mut a, f);
        texts.extend(sa.into_iter().chain(sb).map(|s| s.text));
    }
    assert_eq!(texts, ["glhf!"]);
}

#[test]
fn an_enemy_instance_does_not_act_on_lines_meant_for_allies() {
    let extra = r#"
        [[line]]
        id = "thanks"
        on = "rescued"
        to = "allies"
        say = "Noted. Unnecessary, but appreciated."

        [[line]]
        id = "re.thanks"
        on = "reply"
        answers = "thanks"
        say = "Overheard."
    "#;
    let mut w = Vec::new();
    let lex = Lexicon::builtin().with(extra, &mut w).unwrap();
    let a = speaker("ZKGBAI wip (1)", 0, 0);
    let b = speaker("ZKGBAI wip (2)", 1, 1);
    let mut enemy = Banter::new(lex, b, vec![a.clone()], 3, None);
    enemy.hear(100, "<ZKGBAI wip (1)> Noted. Unnecessary, but appreciated.");
    assert_eq!(enemy.used(), 0);
    assert!(run(&mut enemy, 100, 100 + 30 * SEC).is_empty());
}

#[test]
fn human_chat_is_never_acted_on() {
    let mut b = alone(9);
    b.hear(100, "Resign lobster!");
    b.hear(100, "<someone> Resign lobster!");
    assert_eq!(b.used(), 0);
    assert!(run(&mut b, 100, 100 + 30 * SEC).is_empty());
}

#[test]
fn the_budget_runs_out_but_free_lines_do_not() {
    let mut b = Banter::new(Lexicon::builtin(), speaker("ZKGBAI", 0, 0), vec![], 4, Some(0));
    b.set_state(state(30, 6));
    b.offer(0, "start", &Fields::new());
    b.offer(1000, "taunt", &Fields::new());
    let said: Vec<String> = run(&mut b, 0, 1000 + 10 * SEC).into_iter().map(|s| s.text).collect();
    assert_eq!(said, ["glhf!"]);
    assert_eq!(b.offer_now(5000, "resign", &Fields::new())[0].text, "gg!");
}

#[test]
fn numbers_fill_the_line() {
    let mut b = alone(2);
    b.offer(1000, "inferno", &fields([("n", 2.into()), ("value", 400.into())]));
    run(&mut b, 1000, 1000 + 10 * SEC);
    b.offer(1000 + 200 * SEC, "burnt", &fields([("n", 2.into()), ("metal", 1240.into())]));
    let said = run(&mut b, 1000 + 200 * SEC, 1000 + 210 * SEC);
    assert_eq!(said[0].text, "1.2k metal, well done");
    // A line whose field the event lacks is not a candidate.
    let mut b = alone(2);
    b.offer(1000, "raid_repelled", &fields([("metal", 500.into()), ("nth", 1.into()), ("breached", 0.into())]));
    assert!(run(&mut b, 1000, 1000 + 10 * SEC).is_empty());
}

#[test]
fn a_players_file_tunes_the_builtin() {
    let user = r#"
        [settings]
        budget = 10
        off = ["taunt.lobster", "cook.*"]
        colour = "blue"
        [settings.cooldown]
        taunt = 30

        [[line]]
        id = "taunt.over"
        on = "taunt"
        say = "Over. Resign."

        [[line]]
        id = "taunt.mine"
        on = "taunt"
        say = "Your move."
        mood = "smug"

        [[line]]
        on = "taunt"
        say = "no id"
    "#;
    let mut w = Vec::new();
    let lex = Lexicon::builtin().with(user, &mut w).unwrap();
    assert_eq!(lex.settings.budget, 10);
    assert_eq!(lex.settings.cooldown.get("taunt"), Some(&30.0));
    assert!(lex.is_off("taunt.lobster") && lex.is_off("cook.first") && !lex.is_off("taunt.over"));
    assert_eq!(lex.lines.iter().find(|l| l.id == "taunt.over").unwrap().say, ["Over. Resign."]);
    assert!(lex.lines.iter().any(|l| l.id == "taunt.mine"));
    assert_eq!(w.len(), 3, "{w:?}");
    assert!(Lexicon::builtin().with("budget = [", &mut w).is_err());
}

#[test]
fn speakers_come_from_the_script() {
    let script = r#"
[game]
{
	[ai0]
	{
		team=0;
		host=0;
		name=ZKGBAI wip (1);
		shortname=ZKGBAI;
		[options]
		{
			hivemind=;
		}
	}
	[ai1]
	{
		team=1;
		name=Wires v2 (1);
		shortname=CircuitAI;
	}
	[ai2]
	{
		team=2;
		name=ZKGBAI 0.5.2 (2);
		shortname=ZKGBAI;
	}
	[team0]
	{
		allyteam=0;
	}
	[team1]
	{
		allyteam=1;
	}
	[team2]
	{
		allyteam=1;
	}
}
"#;
    assert_eq!(
        zkgbais_in_script(script),
        vec![("ZKGBAI wip (1)".to_owned(), 0, 0), ("ZKGBAI 0.5.2 (2)".to_owned(), 2, 1)]
    );
    let (me, others) = speakers(script, 0, 0);
    assert_eq!(me.tag, "ZKGBAI wip (1)");
    assert_eq!(others, vec![speaker("ZKGBAI 0.5.2 (2)", 2, 1)]);
    // Two of us named alike: the team tells them apart.
    let twins = script.replace("ZKGBAI 0.5.2 (2)", "ZKGBAI wip (1)");
    let (me, others) = speakers(&twins, 2, 1);
    assert_eq!(me.tag, "ZKGBAI wip (1) #2");
    assert_eq!(others[0].tag, "ZKGBAI wip (1) #0");
    let solo = script.replace("shortname=ZKGBAI;\n\t}\n\t[team0]", "shortname=Other;\n\t}\n\t[team0]");
    let (me, others) = speakers(&solo, 0, 0);
    assert_eq!(me.tag, "ZKGBAI");
    assert!(others.is_empty());
}

#[test]
fn a_resigning_enemy_gets_its_gg_back() {
    let (mut a, mut b) = pair(false);
    a.set_state(state(30, 6));
    b.set_state(state(6, 30));
    b.offer(1000, "taunt", &Fields::new());
    let taunt = run(&mut b, 1000, 1000 + 10 * SEC);
    relay(&taunt, b.tag(), &mut a, 1000 + 10 * SEC);
    run(&mut a, 1000 + 10 * SEC, 1000 + 40 * SEC);
    // B taunted, then lost: its gg follows up the taunt, and A answers.
    let gg = b.offer_now(30_000, "resign", &Fields::new());
    assert!(gg[0].text.starts_with("gg"), "{gg:?}");
    relay(&gg, b.tag(), &mut a, 30_000);
    let said: Vec<String> = run(&mut a, 30_000, 30_000 + 10 * SEC).into_iter().map(|s| s.text).collect();
    assert_eq!(said, ["gg"]);
}

#[test]
fn every_lines_first_message_reads_back_as_that_line() {
    // What a ZKGBAI says must be recognised as the line it is, or replies
    // and the shared budget go wrong.
    let lex = Lexicon::builtin();
    let env: Fields = fields([
        ("n", 7.into()),
        ("value", 900.into()),
        ("metal", 1240.into()),
        ("fired", 9.into()),
        ("target", "Fusion Reactor".into()),
        ("heavy", "Krow".into()),
        ("where", "west".into()),
        ("nth", 3.into()),
        ("ospots", 30.into()),
        ("espots", 6.into()),
        ("lobster", "lobster".into()),
    ]);
    for line in &lex.lines {
        let text = &Banter::render(line, &env).unwrap()[0];
        let read: Vec<&str> = lex.recognise(text).iter().map(|l| l.id.as_str()).collect();
        assert!(read.contains(&line.id.as_str()), "{}: {text:?} reads as {read:?}", line.id);
        assert!(
            read.len() == 1
                || read.iter().all(|id| lex.lines.iter().any(|l| l.id == *id && !l.is_reply()))
                || line.is_reply(),
            "{}: {text:?} is ambiguous: {read:?}",
            line.id
        );
    }
}

/// "Resignable" is said to one's own team, and an allied ZKGBAI answers it
/// there; an enemy one does not hear it at all.
#[test]
fn resignable_stays_within_the_team() {
    for allied in [true, false] {
        let (mut a, mut b) = pair(allied);
        a.set_state(state(5, 30));
        b.set_state(state(5, 30));
        a.offer(1000, "hopeless", &Fields::new());
        let said = run(&mut a, 1000, 1000 + 10 * SEC);
        assert_eq!(said.len(), 1);
        assert_eq!(said[0].to, Channel::Allies);
        assert!(said[0].text.contains("esignable"), "{said:?}");
        relay(&said, a.tag(), &mut b, 1000 + 10 * SEC);
        let answer = run(&mut b, 1000 + 10 * SEC, 1000 + 40 * SEC);
        if allied {
            assert_eq!(
                answer.iter().map(|s| (s.text.as_str(), s.to)).collect::<Vec<_>>(),
                [("ye, resignable", Channel::Allies)]
            );
        } else {
            assert!(answer.is_empty(), "{answer:?}");
            assert_eq!(b.used(), 0);
        }
    }
}

#[test]
fn a_taunt_never_calls_their_game_resignable() {
    let lex = Lexicon::builtin();
    for l in lex.lines.iter().filter(|l| l.on == "taunt" || l.on == "recovered") {
        assert!(l.say.iter().all(|t| !t.to_lowercase().contains("resignable")), "{}: {:?}", l.id, l.say);
    }
}

#[test]
fn conquest_marks_every_committing_ally_with_a_story_and_no_quip_cost() {
    let (mut a, mut b) = pair(true);
    let pa = [100.0, 20.0, 200.0];
    let pb = [500.0, 20.0, 700.0];
    a.mark(0, "conquest", pa, &Fields::new());
    b.mark(0, "conquest", pb, &Fields::new());
    let first = a.due(30);
    assert_eq!(first.len(), 1);
    assert_eq!(first[0].marker, Some(pa));
    assert_eq!(first[0].to, Channel::Allies);
    relay(&first, a.tag(), &mut b, 60);
    let second = b.due(90);
    assert_eq!(second.len(), 1);
    assert_eq!(second[0].id, "conquest.seconded");
    assert_eq!(second[0].marker, Some(pb));
    assert_eq!((a.used(), b.used()), (0, 0));
    // A later real commitment follows through, rather than an uninvolved
    // ally automatically replying to a marker.
    relay(&second, b.tag(), &mut a, 120);
    assert!(a.due(130).is_empty());
}

#[test]
fn markers_obey_off_and_do_not_leak_enemy_intentions_or_steal_quips() {
    let (mut a, mut enemy) = pair(false);
    a.mark(0, "conquest", [1.0, 2.0, 3.0], &Fields::new());
    let marked = a.due(30);
    relay(&marked, a.tag(), &mut enemy, 31);
    enemy.mark(40, "conquest", [4.0, 5.0, 6.0], &Fields::new());
    let independent = enemy.due(70);
    assert!(!independent[0].id.contains("seconded"));
    enemy.budget = 0;
    enemy.mark(80, "rush", [0.0; 3], &fields([("heavy", "Krow".into())]));
    assert!(enemy.due(200).is_empty());
    a.mark(100, "conquest", [1.0; 3], &Fields::new());
    a.due(130);
    a.mark(140, "conquest", [2.0; 3], &Fields::new());
    assert!(a.due(200).is_empty()); // two commitments per game
}

#[test]
fn conquest_story_survives_engine_chat_delivery_delay() {
    let speakers: Vec<_> = (0..3).map(|i| speaker(&format!("ZKGBAI ({i})"), i, 0)).collect();
    let mut bots: Vec<_> = speakers
        .iter()
        .map(|s| {
            Banter::new(
                Lexicon::builtin(),
                s.clone(),
                speakers.iter().filter(|p| p.team != s.team).cloned().collect(),
                11,
                None,
            )
        })
        .collect();
    for b in &mut bots {
        b.mark(0, "conquest", [1.0; 3], &Fields::new());
    }
    for i in 0..3 {
        let frame = 30 + i as i32 * 60;
        let said = bots[i].due(frame);
        assert_eq!(said.len(), 1);
        if i == 1 {
            assert_eq!(said[0].id, "conquest.seconded");
        }
        if i == 2 {
            assert_eq!(said[0].id, "conquest.committee");
        }
        for (j, b) in bots.iter_mut().enumerate() {
            if i != j {
                relay(&said, &speakers[i].tag, b, frame + 30);
            }
        }
    }
}
