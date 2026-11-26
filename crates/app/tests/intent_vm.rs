//! The intent view models: every label, counter and sentence the intent views
//! draw, derived without GPUI.

mod common;

use chrono::{DateTime, Duration, FixedOffset, Utc};
use common::intents::{intent, item, model_with, question, FakeIntents};
use common::ui::agent;
use smithagents_app::intent_vm::*;
use smithagents_app::link::LinkError;
use smithagents_app::model::{Action, Connection, ShellModel};
use smithagents_app::view_models::{DotStyle, DAEMON_UNREACHABLE_TEXT, NOT_CONNECTED_TEXT};
use smithagents_core::agent::{AgentState, Repo};
use smithagents_core::ids::{IntentId, RepoId};
use smithagents_core::intent::{Answer, Intent, IntentKind, IntentState, Item, ItemStatus, Phase};

fn utc(text: &str) -> DateTime<Utc> {
    text.parse().unwrap()
}

fn dot(token: &'static str, filled: bool, dimmed: bool) -> DotStyle {
    DotStyle {
        token,
        filled,
        dimmed,
    }
}

fn draft(slug: &str, phase: Phase) -> Intent {
    intent(slug, IntentState::Draft, phase)
}

fn api_error(code: &str, message: &str) -> LinkError {
    FakeIntents::api_error(409, code, message)
}

#[test]
fn kind_labels_and_choices() {
    let labels: Vec<_> = IntentKind::ALL.into_iter().map(kind_label).collect();
    assert_eq!(
        labels,
        [
            "New feature",
            "Enhancement",
            "Bug",
            "Refactor",
            "Maintenance",
            "Task"
        ]
    );
    let rows: Vec<_> = KIND_CHOICES
        .iter()
        .map(|c| (c.kind, c.title, c.description, c.key, c.shortcut))
        .collect();
    assert_eq!(
        rows,
        [
            (
                IntentKind::NewFeature,
                "New feature",
                "a new capability",
                "alt-f",
                "⌥F"
            ),
            (
                IntentKind::Enhancement,
                "Enhancement",
                "extend an existing capability",
                "alt-e",
                "⌥E"
            ),
            (
                IntentKind::Bug,
                "Bug",
                "a capability that misbehaves",
                "alt-b",
                "⌥B"
            ),
            (
                IntentKind::Refactor,
                "Refactor",
                "restructure, same behavior",
                "alt-r",
                "⌥R"
            ),
            (
                IntentKind::Maintenance,
                "Maintenance",
                "upgrades, config, housekeeping",
                "alt-m",
                "⌥M"
            ),
        ]
    );
    assert!(KIND_CHOICES.iter().all(|c| c.kind != IntentKind::Task));
}

fn one_of_each() -> Vec<(&'static str, Intent)> {
    vec![
        ("Polishing", draft("a", Phase::Polishing { pass: 1 })),
        ("Questions", draft("b", Phase::Questions)),
        ("Review", draft("c", Phase::Review)),
        (
            "Polish failed",
            draft(
                "d",
                Phase::Failed {
                    pass: 1,
                    message: "x".into(),
                },
            ),
        ),
        (
            "Complete",
            intent("e", IntentState::Complete, Phase::Review),
        ),
        ("Queued", intent("f", IntentState::Queued, Phase::Review)),
        ("Sent", intent("g", IntentState::Sent, Phase::Review)),
        (
            "Resolved",
            intent("h", IntentState::Resolved, Phase::Review),
        ),
    ]
}

#[test]
fn state_labels() {
    for (label, intent) in one_of_each() {
        assert_eq!(intent_state_label(&intent), label);
    }
}

#[test]
fn filter_membership() {
    use IntentFilterChoice::*;
    let intents = one_of_each();
    let members = |choice: IntentFilterChoice| -> Vec<&str> {
        intents
            .iter()
            .filter(|(_, i)| choice.matches(i))
            .map(|(label, _)| *label)
            .collect()
    };
    assert_eq!(members(All).len(), 8);
    assert_eq!(
        members(DraftAndPolishing),
        ["Polishing", "Questions", "Review", "Polish failed"]
    );
    assert_eq!(members(CompleteAndQueued), ["Complete", "Queued"]);
    assert_eq!(members(Sent), ["Sent"]);
    assert_eq!(members(Resolved), ["Resolved"]);
    let labels: Vec<_> = IntentFilterChoice::ALL
        .iter()
        .map(|c| (c.label(), c.as_str()))
        .collect();
    assert_eq!(
        labels,
        [
            ("All", "all"),
            ("Draft and polishing", "draft"),
            ("Complete and queued", "complete"),
            ("Sent", "sent"),
            ("Resolved", "resolved"),
        ]
    );
}

#[test]
fn meta_line_for_a_sent_intent() {
    let now = DateTime::parse_from_rfc3339("2026-11-26T10:15:00-08:00").unwrap();
    let mut sent = intent("ship-it", IntentState::Sent, Phase::Review);
    sent.created_at = utc("2026-11-26T14:15:00Z");
    sent.sent_at = Some(now.with_timezone(&Utc) - Duration::hours(4));
    let mut a = agent("worker");
    a.state = AgentState::Done;
    sent.agent_id = Some(a.id);

    let model = model_with(vec![sent.clone()], vec![a], vec![]);
    let cards = intent_cards(&model, IntentFilterChoice::All, now);
    assert_eq!(cards[0].meta, "Nov 26, 2026, 6:15 AM | Sent | done | 4h");

    let model = model_with(vec![sent], vec![], vec![]);
    let cards = intent_cards(&model, IntentFilterChoice::All, now);
    assert_eq!(cards[0].meta, "Nov 26, 2026, 6:15 AM | Sent | 4h");

    let mut review = draft("look", Phase::Review);
    review.created_at = utc("2026-11-26T14:15:00Z");
    review.updated_at = now.with_timezone(&Utc) - Duration::minutes(5);
    let model = model_with(vec![review], vec![], vec![]);
    let cards = intent_cards(&model, IntentFilterChoice::All, now);
    assert_eq!(cards[0].meta, "Nov 26, 2026, 6:15 AM | Review | 5m");
}

#[test]
fn created_text_uses_the_offset() {
    let offset = FixedOffset::west_opt(8 * 3600).unwrap();
    assert_eq!(
        created_text(utc("2026-11-26T14:15:00Z"), offset),
        "Nov 26, 2026, 6:15 AM"
    );
    assert_eq!(
        created_text(
            utc("2026-01-05T23:05:00Z"),
            FixedOffset::east_opt(0).unwrap()
        ),
        "Jan 5, 2026, 11:05 PM"
    );
}

#[test]
fn age_steps() {
    let now = utc("2026-11-26T12:00:00Z");
    let age = |d: Duration| age_text(now - d, now);
    assert_eq!(age(Duration::seconds(59)), "now");
    assert_eq!(age(Duration::seconds(60)), "1m");
    assert_eq!(age(Duration::minutes(59)), "59m");
    assert_eq!(age(Duration::minutes(60)), "1h");
    assert_eq!(age(Duration::hours(23)), "23h");
    assert_eq!(age(Duration::hours(24)), "1d");
    assert_eq!(age(Duration::days(3)), "3d");
    assert_eq!(age(Duration::minutes(-5)), "now");
}

#[test]
fn body_preview_reads_the_first_heading_and_line() {
    let preview = body_preview("## What happens\n\nIn the **app**, `x` is [wrong](http://e).\n");
    assert_eq!(preview.heading.as_deref(), Some("What happens"));
    assert_eq!(preview.line.as_deref(), Some("In the app, x is wrong."));
}

#[test]
fn body_preview_makes_lines_plain() {
    for (raw, plain) in [
        ("- item one", "item one"),
        ("* item __two__", "item two"),
        ("> quoted", "quoted"),
        ("12. numbered", "numbered"),
        ("  spaced  ", "spaced"),
    ] {
        assert_eq!(body_preview(raw).line.as_deref(), Some(plain), "{raw}");
    }
}

#[test]
fn body_preview_survives_odd_markdown() {
    assert_eq!(
        body_preview(""),
        BodyPreview {
            heading: None,
            line: None
        }
    );
    assert_eq!(
        body_preview("just text"),
        BodyPreview {
            heading: None,
            line: Some("just text".into())
        }
    );
    assert_eq!(
        body_preview("```\n```"),
        BodyPreview {
            heading: None,
            line: None
        }
    );
    let crlf = body_preview("# Title\r\n\r\nBody line\r\n");
    assert_eq!(crlf.heading.as_deref(), Some("Title"));
    assert_eq!(crlf.line.as_deref(), Some("Body line"));
    let long = body_preview(&"é".repeat(10_000));
    assert_eq!(long.line.unwrap().chars().count(), 200);
    assert_eq!(
        body_preview("# Only a heading"),
        BodyPreview {
            heading: Some("Only a heading".into()),
            line: None
        }
    );
    // No heading: the first non-empty line, even above a later heading.
    let two = body_preview("\n\nfirst\n# later");
    assert_eq!(two.heading.as_deref(), Some("later"));
    // With a heading, the line is the one after it, not one before it.
    assert_eq!(two.line, None);
    // Seven marks is not a heading.
    assert_eq!(body_preview("####### no").heading, None);
}

#[test]
fn a_line_of_brackets_is_cut_before_its_links_are_stripped() {
    // Each `[` without a link used to scan the rest of the line again.
    let brackets = "[".repeat(50_000);
    let preview = body_preview(&brackets);
    assert_eq!(preview.line, Some("[".repeat(200)));
    // A link that starts inside the kept part and ends past it stays text.
    let late_link = format!("[{}](https://example.com)", "a".repeat(2_500));
    let line = body_preview(&late_link).line.unwrap();
    assert_eq!(line, format!("[{}", "a".repeat(199)));
}

#[test]
fn a_polishing_drafts_card_previews_its_seed() {
    let now = DateTime::parse_from_rfc3339("2026-11-26T10:15:00-08:00").unwrap();
    let mut zoom = draft("untitled-intent", Phase::Polishing { pass: 1 });
    zoom.content.title = "Untitled intent".into();
    zoom.seed.text = "Make the zoom remember each site".into();
    let mut crash = draft("untitled-intent-2", Phase::Polishing { pass: 1 });
    crash.content.title = "Untitled intent".into();
    crash.seed.text = "## Crash\nThe app quits when a tab closes".into();
    let mut polished = draft("polished", Phase::Review);
    polished.content.current_behavior = "Today the zoom resets".into();
    // Newest first.
    zoom.updated_at = utc("2026-11-26T15:00:00Z");
    crash.updated_at = utc("2026-11-26T14:00:00Z");
    polished.updated_at = utc("2026-11-26T13:00:00Z");
    let model = model_with(vec![zoom, crash, polished], vec![], vec![]);

    let cards = intent_cards(&model, IntentFilterChoice::All, now);
    let previews: Vec<_> = cards
        .iter()
        .map(|card| {
            (
                card.preview.heading.as_deref(),
                card.preview.line.as_deref(),
            )
        })
        .collect();
    assert_eq!(
        previews,
        [
            (None, Some("Make the zoom remember each site")),
            (Some("Crash"), Some("The app quits when a tab closes")),
            (None, Some("Today the zoom resets")),
        ]
    );
}

#[test]
fn dots_by_state() {
    let dots: Vec<_> = one_of_each()
        .iter()
        .map(|(_, i)| intent_dot(i, None))
        .collect();
    assert_eq!(
        dots,
        [
            dot("status-info", true, false),
            dot("status-warn", true, false),
            dot("status-warn", true, false),
            dot("status-error", true, false),
            dot("status-ok", false, false),
            dot("muted-foreground", true, true),
            dot("muted-foreground", false, false),
            dot("status-ok", true, true),
        ]
    );
    let sent = intent("g", IntentState::Sent, Phase::Review);
    assert_eq!(
        intent_dot(&sent, Some(AgentState::Done)),
        dot("status-ok", true, false)
    );
    assert_eq!(
        intent_dot(&sent, Some(AgentState::Working)),
        dot("status-info", true, false)
    );
}

#[test]
fn cards_are_filtered_and_ordered() {
    let now = DateTime::parse_from_rfc3339("2026-11-26T10:15:00-08:00").unwrap();
    let mut a = agent("worker");
    a.color.h = 210.0;
    let mut sent = intent("sent-one", IntentState::Sent, Phase::Review);
    sent.agent_id = Some(a.id);
    sent.updated_at = utc("2026-11-26T14:00:00Z");
    let mut sent_orphan = intent("sent-orphan", IntentState::Sent, Phase::Review);
    sent_orphan.agent_id = Some(smithagents_core::ids::AgentId::new());
    sent_orphan.updated_at = utc("2026-11-26T13:00:00Z");
    let mut review = draft("review-one", Phase::Review);
    review.content.current_behavior = "# Heading\nBody".into();
    review.updated_at = utc("2026-11-26T15:00:00Z");
    let model = model_with(
        vec![sent.clone(), sent_orphan.clone(), review.clone()],
        vec![a],
        vec![],
    );

    let all = intent_cards(&model, IntentFilterChoice::All, now);
    let slugs: Vec<_> = all.iter().map(|c| c.slug.as_str()).collect();
    assert_eq!(slugs, ["review-one", "sent-one", "sent-orphan"]);
    let hues: Vec<_> = all.iter().map(|c| c.accent_hue).collect();
    assert_eq!(hues, [None, Some(210.0), None]);
    assert_eq!(all[0].title, "Review one");
    assert_eq!(all[0].id, review.id);
    assert_eq!(all[0].a11y_label, "Review one, Review");
    assert_eq!(all[0].preview.heading.as_deref(), Some("Heading"));
    assert_eq!(all[0].preview.line.as_deref(), Some("Body"));
    // The agent is Idle: the idle dot.
    assert_eq!(all[1].dot, dot("muted-foreground", false, false));

    let only_sent = intent_cards(&model, IntentFilterChoice::Sent, now);
    assert_eq!(only_sent.len(), 2);
    assert_eq!(
        only_sent[1].dot,
        dot("muted-foreground", false, false),
        "no agent in the model"
    );
    assert!(intent_cards(&model, IntentFilterChoice::Resolved, now).is_empty());
}

#[test]
fn empty_texts() {
    let never = ShellModel::new();
    assert_eq!(
        list_empty_text(&never, IntentFilterChoice::All),
        Some(NOT_CONNECTED_TEXT)
    );
    let loaded_empty = model_with(vec![], vec![], vec![]);
    assert_eq!(
        list_empty_text(&loaded_empty, IntentFilterChoice::All),
        Some("No intents yet. Press + to describe what you want.")
    );
    let one = model_with(vec![draft("a", Phase::Review)], vec![], vec![]);
    assert_eq!(
        list_empty_text(&one, IntentFilterChoice::Sent),
        Some("No intents match this filter.")
    );
    assert_eq!(list_empty_text(&one, IntentFilterChoice::All), None);
    // Loaded once, then the connection drops: the list still stands.
    let mut dropped = model_with(vec![draft("a", Phase::Review)], vec![], vec![]);
    dropped.apply(Action::ConnectionChanged(Connection::Reconnecting));
    assert_eq!(list_empty_text(&dropped, IntentFilterChoice::All), None);
}

#[test]
fn section_counters_and_status() {
    let mut c = draft("s", Phase::Questions).content;
    c.questions = vec![
        question(
            "a?",
            &["Yes"],
            Answer::Option {
                option_id: "o1".into(),
            },
        ),
        question("b?", &["Yes"], Answer::Skipped),
        question("c?", &["Yes"], Answer::Unanswered),
    ];
    let q = section_vm(&c, SectionKind::Questions);
    assert_eq!(q.title, "Questions");
    assert_eq!(q.counter, "2 of 3 answered");
    assert_eq!(q.status, "1 to answer");
    assert!(q.starts_open);
    assert_eq!((q.pending, q.skipped), (1, 0));

    c.criteria = vec![
        item("a", ItemStatus::Accepted),
        item("b", ItemStatus::Accepted),
        item("c", ItemStatus::Dropped),
    ];
    let s = section_vm(&c, SectionKind::Criteria);
    assert_eq!(s.title, "Acceptance criteria");
    assert_eq!(s.counter, "2 of 2 accepted");
    assert_eq!(s.status, "✓ done");
    assert_eq!(s.skipped, 1);
    assert!(!s.starts_open);

    let stories = section_vm(&c, SectionKind::Stories);
    assert_eq!(stories.title, "Stories");
    assert_eq!(stories.counter, "0 of 0 accepted");
    assert_eq!(stories.status, "none");
    assert!(!stories.starts_open);

    c.preconditions = vec![
        item("p", ItemStatus::Pending),
        item("q", ItemStatus::Accepted),
    ];
    let p = section_vm(&c, SectionKind::Preconditions);
    assert_eq!(p.counter, "1 of 2 accepted");
    assert_eq!(p.status, "1 to review");
    assert!(p.starts_open);

    c.depends_on = vec![item("d", ItemStatus::Pending)];
    let d = section_vm(&c, SectionKind::DependsOn);
    assert_eq!(d.title, "Depends on");
    assert_eq!(
        (d.counter.as_str(), d.status.as_str()),
        ("0 picked", "1 to review")
    );
    c.depends_on = vec![item("d", ItemStatus::Dropped)];
    let d = section_vm(&c, SectionKind::DependsOn);
    assert_eq!(
        (d.counter.as_str(), d.status.as_str()),
        ("0 picked", "none")
    );
    c.depends_on = vec![item("d", ItemStatus::Accepted)];
    let d = section_vm(&c, SectionKind::DependsOn);
    assert_eq!(
        (d.counter.as_str(), d.status.as_str()),
        ("1 picked", "✓ done")
    );

    assert_eq!(
        SectionKind::ALL.map(SectionKind::as_str),
        [
            "questions",
            "preconditions",
            "criteria",
            "stories",
            "depends_on"
        ]
    );
    assert_eq!(section_items(&c, SectionKind::Questions).len(), 0);
    assert_eq!(section_items(&c, SectionKind::Criteria).len(), 3);
    assert_eq!(section_items(&c, SectionKind::DependsOn).len(), 1);
}

#[test]
fn answers() {
    let yes = question(
        "q",
        &["Yes", "No"],
        Answer::Option {
            option_id: "o1".into(),
        },
    );
    assert_eq!(answer_text(&yes).as_deref(), Some("→ Yes"));
    let skipped = question("q", &["Yes"], Answer::Skipped);
    assert_eq!(answer_text(&skipped).as_deref(), Some("→ Skipped"));
    let open = question("q", &["Yes"], Answer::Unanswered);
    assert_eq!(answer_text(&open), None);
}

#[test]
fn polishing_sentence() {
    assert_eq!(polishing_text(1), "Polishing… (pass 1 of 2)");
    assert_eq!(polishing_text(2), "Polishing… (pass 2 of 2)");
}

#[test]
fn body_falls_back_to_the_seed() {
    let mut i = draft("b", Phase::Review);
    assert_eq!(body_text(&i), "Seed of b");
    i.content.current_behavior = "  \n ".into();
    assert_eq!(body_text(&i), "Seed of b");
    i.content.current_behavior = "Real".into();
    assert_eq!(body_text(&i), "Real");
}

#[test]
fn header_is_editable_only_in_draft_and_complete() {
    assert!(header_editable(&draft("a", Phase::Review)));
    assert!(header_editable(&draft("a", Phase::Questions)));
    assert!(header_editable(&draft(
        "a",
        Phase::Failed {
            pass: 1,
            message: "x".into()
        }
    )));
    assert!(!header_editable(&draft("a", Phase::Polishing { pass: 1 })));
    assert!(header_editable(&intent(
        "a",
        IntentState::Complete,
        Phase::Review
    )));
    for state in [
        IntentState::Queued,
        IntentState::Sent,
        IntentState::Resolved,
    ] {
        assert!(!header_editable(&intent("a", state, Phase::Review)));
    }
}

fn depends(on: &Intent) -> Item {
    let mut it = item("dep", ItemStatus::Accepted);
    it.intent_ref = Some(on.id);
    it
}

#[test]
fn queued_reasons() {
    let a = intent("a-one", IntentState::Queued, Phase::Review);
    let b = intent("b-two", IntentState::Sent, Phase::Review);
    let done = intent("c-three", IntentState::Resolved, Phase::Review);
    let mut me = intent("me", IntentState::Queued, Phase::Review);
    me.content.depends_on = vec![depends(&a), depends(&b)];
    let model = model_with(
        vec![a.clone(), b.clone(), done.clone(), me.clone()],
        vec![],
        vec![],
    );
    assert_eq!(
        queued_reason(&model, &me),
        "Waiting for a-one, b-two to be resolved."
    );

    me.content.depends_on = vec![depends(&done)];
    assert_eq!(queued_reason(&model, &me), "Waiting for a free agent slot.");

    let mut pending = depends(&a);
    pending.status = ItemStatus::Pending;
    me.content.depends_on = vec![pending];
    assert_eq!(queued_reason(&model, &me), "Waiting for a free agent slot.");

    let mut ghost = item("dep", ItemStatus::Accepted);
    ghost.intent_ref = Some(IntentId::new());
    me.content.depends_on = vec![ghost];
    assert_eq!(
        queued_reason(&model, &me),
        "Waiting for another intent to be resolved."
    );
}

#[test]
fn dependencies_resolve_through_the_model() {
    let a = intent("a-one", IntentState::Queued, Phase::Review);
    let model = model_with(vec![a.clone()], vec![], vec![]);
    let known = dependency_vm(&model, &depends(&a));
    assert_eq!(known.slug.as_deref(), Some("a-one"));
    assert_eq!(known.title, "A one");
    assert_eq!(known.status, ItemStatus::Accepted);
    let mut ghost = item("Needs the thing", ItemStatus::Pending);
    ghost.intent_ref = Some(IntentId::new());
    let unknown = dependency_vm(&model, &ghost);
    assert_eq!(unknown.slug, None);
    assert_eq!(unknown.title, "Needs the thing");
    assert_eq!(unknown.item_id, ghost.id);
    let plain = dependency_vm(&model, &item("No ref", ItemStatus::Dropped));
    assert_eq!((plain.slug, plain.title.as_str()), (None, "No ref"));
}

fn repo(at: &str) -> Repo {
    Repo {
        id: RepoId::new(),
        path: "/tmp/r".into(),
        default_branch: "main".into(),
        created_at: utc(at),
    }
}

#[test]
fn default_repo_prefers_the_last_used() {
    let old = repo("2026-11-01T00:00:00Z");
    let new = repo("2026-11-20T00:00:00Z");
    let repos = vec![new.clone(), old.clone()];
    assert_eq!(default_repo(&repos, None), Some(old.id));
    assert_eq!(default_repo(&repos, Some(new.id)), Some(new.id));
    assert_eq!(default_repo(&repos, Some(RepoId::new())), Some(old.id));
    assert_eq!(default_repo(&[], Some(new.id)), None);
}

#[test]
fn error_sentences() {
    assert_eq!(
        intent_error_text(&LinkError::NotConnected),
        NOT_CONNECTED_TEXT
    );
    assert_eq!(
        intent_error_text(&LinkError::Stopped),
        "The daemon link has stopped"
    );
    for (code, text) in [
        ("polishing", "The intent is still being polished."),
        (
            "invalid_state",
            "The intent changed, so this is no longer possible.",
        ),
        ("already_answered", "That question is already answered."),
        (
            "dependency_cycle",
            "Those intents would depend on each other.",
        ),
        (
            "dependency_cross_repo",
            "A dependency must be in the same repository.",
        ),
        ("intent_not_found", "That intent no longer exists."),
        ("repo_not_found", "That repository no longer exists."),
    ] {
        assert_eq!(intent_error_text(&api_error(code, "raw daemon text")), text);
    }
    assert_eq!(
        intent_error_text(&api_error("weird", "store:  bad  thing")),
        "Store: bad thing"
    );
    assert_eq!(
        intent_error_text(&api_error("weird", "  \n ")),
        "The daemon could not do that."
    );
}

const START_PREFIX: &str = "The agent could not start: ";

#[test]
fn a_long_start_error_is_one_line_and_cut() {
    // 13 + 1 + 600 characters over two lines, non-ASCII where the cut falls.
    let message = format!("no directory\n\tat /moved/{}", "é".repeat(600));
    let text = start_error_text(&message);
    assert!(!text.contains(['\n', '\t']), "{text:?}");
    let rest = text
        .strip_prefix(START_PREFIX)
        .expect("the sentence's start");
    let cut = rest
        .strip_suffix('…')
        .expect("an ellipsis where it was cut");
    assert_eq!(cut.chars().count(), 300);
    assert!(
        format!("no directory at /moved/{}", "é".repeat(600)).starts_with(cut),
        "the first 300 characters of the message on one line"
    );

    // Exactly 300 characters are kept whole.
    let fits = "a".repeat(300);
    assert_eq!(start_error_text(&fits), format!("{START_PREFIX}{fits}"));
}

#[test]
fn the_daemons_own_prefix_is_not_repeated() {
    assert_eq!(
        start_error_text("could not start the agent: no directory at /moved/repo"),
        "The agent could not start: no directory at /moved/repo"
    );
    assert_eq!(
        start_error_text("Could Not Start The Agent:  git\nfailed"),
        "The agent could not start: git failed"
    );
    // Once only.
    assert_eq!(
        start_error_text("could not start the agent: could not start the agent: x"),
        "The agent could not start: could not start the agent: x"
    );
}

#[test]
fn an_empty_start_error_is_a_plain_sentence() {
    for empty in [
        "",
        "  \n\t ",
        "could not start the agent: ",
        "could not start the agent:",
        "  Could not start the agent:  \n",
    ] {
        assert_eq!(
            start_error_text(empty),
            "The agent could not start.",
            "{empty:?}"
        );
    }
}

#[test]
fn a_short_start_error_is_unchanged() {
    assert_eq!(
        start_error_text("no directory at /moved/repo"),
        "The agent could not start: no directory at /moved/repo"
    );
}

#[test]
fn transport_errors_are_unreachable() {
    let http = reqwest::Client::new()
        .get("not a url")
        .build()
        .expect_err("an invalid URL is a reqwest error");
    let http = LinkError::Client(smithagents_client::ClientError::Http(http));
    assert_eq!(intent_error_text(&http), DAEMON_UNREACHABLE_TEXT);
    let ws = LinkError::Client(smithagents_client::ClientError::Ws(
        tokio_tungstenite::tungstenite::Error::ConnectionClosed,
    ));
    assert_eq!(intent_error_text(&ws), DAEMON_UNREACHABLE_TEXT);
}

#[test]
fn queue_limit_labels() {
    assert_eq!(queue_limit_label(None), "No limit");
    assert_eq!(queue_limit_label(Some(0)), "No limit");
    assert_eq!(queue_limit_label(Some(3)), "3");
    assert_eq!(QUEUE_LIMIT_CHOICES, 1..=16);
}
