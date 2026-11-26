//! Intents and agent defaults in `ShellModel`: the initial load, the daemon's
//! events and the commands' answers, and an older copy of an intent never
//! replacing a newer one. Also where the shell's state sends intent commands
//! and what clock it reads.
//!
//! Kit names are imported one by one: with `test-support` on, a glob import
//! would bring in GPUI's `test` attribute and shadow the built-in `#[test]`.

mod common;

use std::rc::Rc;

use chrono::{DateTime, Utc};
use common::intents::{fixed_clock, intent, model_with, FakeIntents, IntentCall};
use common::ui::open_shell_options;
use gpui_kit::TestAppContext;
use smithagents_app::model::{Action, ShellModel};
use smithagents_app::shell::ShellOptions;
use smithagents_core::event::{Event, EventKind};
use smithagents_core::ids::IntentId;
use smithagents_core::intent::{AgentDefaults, Intent, IntentState, Phase};

/// `hh:mm` on the day the intents are made.
fn at(time: &str) -> DateTime<Utc> {
    format!("2026-11-26T{time}:00Z").parse().unwrap()
}

/// A Draft in review, last changed at `time`.
fn changed_at(slug: &str, time: &str) -> Intent {
    let mut intent = intent(slug, IntentState::Draft, Phase::Review);
    intent.updated_at = at(time);
    intent
}

fn event(seq: u64, kind: EventKind) -> Action {
    Action::Event(Event {
        seq,
        at: at("13:00"),
        kind,
    })
}

fn slugs(model: &ShellModel) -> Vec<&str> {
    model.intents().iter().map(|i| i.slug.as_str()).collect()
}

fn title(model: &ShellModel, intent: &Intent) -> String {
    model
        .intent(intent.id)
        .expect("the intent")
        .content
        .title
        .clone()
}

#[test]
fn loaded_intents_are_listed_newest_first() {
    let mut model = ShellModel::new();
    assert!(!model.intents_loaded());
    assert!(model.intents().is_empty());
    let revision = model.revision();

    model.apply(Action::IntentsLoaded(vec![
        changed_at("ten", "10:00"),
        changed_at("twelve", "12:00"),
        changed_at("eleven", "11:00"),
    ]));
    assert_eq!(slugs(&model), ["twelve", "eleven", "ten"]);
    assert!(model.intents_loaded());
    assert_eq!(model.revision(), revision + 1);

    // Two changed at the same time are listed by id.
    let (a, b) = (changed_at("a", "10:00"), changed_at("b", "10:00"));
    let by_id = if a.id.0 < b.id.0 {
        ["a", "b"]
    } else {
        ["b", "a"]
    };
    model.apply(Action::IntentsLoaded(vec![b.clone(), a.clone()]));
    assert_eq!(slugs(&model), by_id);
    model.apply(Action::IntentsLoaded(vec![a, b]));
    assert_eq!(slugs(&model), by_id);
}

#[test]
fn an_intent_event_inserts_then_replaces() {
    let mut model = model_with(vec![], vec![], vec![]);
    let first = changed_at("fix-zoom", "10:00");
    let revision = model.revision();
    model.apply(event(2, EventKind::IntentCreated(first.clone())));
    assert_eq!(model.intent(first.id), Some(&first));
    assert_eq!(model.revision(), revision + 1);

    let mut later = first.clone();
    later.updated_at = at("11:00");
    later.content.title = "Fix the zoom".into();
    model.apply(event(3, EventKind::IntentUpdated(later.clone())));
    assert_eq!(model.intent(first.id), Some(&later));
    assert_eq!(model.intents().len(), 1);
    assert_eq!(model.revision(), revision + 2);

    // The same event again, or the same copy in a later event or a command's
    // answer, changes nothing.
    let effects = model.apply(event(3, EventKind::IntentUpdated(later.clone())));
    assert!(effects.is_empty());
    model.apply(event(4, EventKind::IntentUpdated(later.clone())));
    model.apply(Action::IntentUpserted(later.clone()));
    assert_eq!(model.intent(first.id), Some(&later));
    assert_eq!(model.revision(), revision + 2);
}

#[test]
fn an_older_copy_never_replaces_a_newer_one() {
    let mut model = model_with(vec![], vec![], vec![]);
    let mut new = changed_at("fix-zoom", "12:00");
    new.content.title = "new".into();
    model.apply(Action::IntentUpserted(new.clone()));
    assert_eq!(title(&model, &new), "new");

    let mut old = new.clone();
    old.updated_at = at("11:00");
    old.content.title = "old".into();
    let revision = model.revision();
    model.apply(Action::IntentUpserted(old.clone()));
    assert_eq!(title(&model, &new), "new");
    assert_eq!(model.revision(), revision);

    // An event the model has not seen yet carrying the older copy.
    let seq = model.last_seq().expect("connected") + 1;
    model.apply(event(seq, EventKind::IntentUpdated(old)));
    assert_eq!(model.last_seq(), Some(seq));
    assert_eq!(title(&model, &new), "new");
    assert_eq!(model.revision(), revision);
}

#[test]
fn a_reload_replaces_the_list() {
    let (a, b) = (changed_at("a", "10:00"), changed_at("b", "11:00"));
    let mut model = model_with(vec![a.clone()], vec![], vec![]);
    assert_eq!(slugs(&model), ["a"]);
    model.apply(Action::IntentsLoaded(vec![b.clone()]));
    assert_eq!(slugs(&model), ["b"]);
    assert_eq!(model.intent(a.id), None);
    assert!(model.intents_loaded());
}

#[test]
fn agent_defaults_are_kept() {
    let mut model = model_with(vec![], vec![], vec![]);
    assert_eq!(model.agent_defaults(), None);
    let defaults = AgentDefaults {
        queue_limit: Some(3),
    };
    let revision = model.revision();
    model.apply(Action::AgentDefaultsLoaded(defaults.clone()));
    assert_eq!(model.agent_defaults(), Some(&defaults));
    assert_eq!(model.revision(), revision + 1);

    model.apply(Action::AgentDefaultsLoaded(defaults.clone()));
    assert_eq!(model.revision(), revision + 1);
    model.apply(Action::AgentDefaultsLoaded(AgentDefaults::default()));
    assert_eq!(model.agent_defaults(), Some(&AgentDefaults::default()));
}

#[gpui_kit::test]
fn the_state_uses_the_injected_clock_and_intent_commands(cx: &mut TestAppContext) {
    let fake = Rc::new(FakeIntents::default());
    let options = ShellOptions {
        model: model_with(vec![], vec![], vec![]),
        intent_commands: Some(fake.clone()),
        clock: Some(fixed_clock("2026-11-26T10:15:00-08:00")),
        ..Default::default()
    };
    let (_window, shell) = open_shell_options(cx, options, (1200., 800.));
    let state = shell.read_with(cx, |shell, _| shell.state().clone());
    let id = IntentId::new();
    let (now, answer) = state.read_with(cx, |state, _| {
        let commands = state.intent_commands().expect("the injected commands");
        (state.now(), commands.send_intent(id))
    });
    assert_eq!(now.to_rfc3339(), "2026-11-26T10:15:00-08:00");
    assert_eq!(*fake.calls.borrow(), [IntentCall::Send(id)]);
    drop(answer);
}

#[gpui_kit::test]
fn offline_there_are_no_intent_commands_and_the_clock_is_local(cx: &mut TestAppContext) {
    let (_window, shell) = open_shell_options(cx, ShellOptions::default(), (1200., 800.));
    let state = shell.read_with(cx, |shell, _| shell.state().clone());
    let before = Utc::now();
    let (offline, now) = state.read_with(cx, |state, _| {
        (state.intent_commands().is_none(), state.now())
    });
    let after = Utc::now();
    assert!(offline);
    let now = now.with_timezone(&Utc);
    assert!(
        before <= now && now <= after,
        "{before} <= {now} <= {after}"
    );
}
