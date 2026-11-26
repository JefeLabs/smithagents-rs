use chrono::Utc;
use smithagents_core::ids::{IntentId, ItemId, QuestionId, RepoId};
use smithagents_core::intent::{
    Answer, Content, Intent, IntentKind, IntentState, Item, ItemStatus, Phase, Question,
    QuestionOption, Seed, SeedSource,
};
use smithagents_server::brief::{compose, DepSummary};

fn item(text: &str, status: ItemStatus) -> Item {
    Item {
        id: ItemId::new(),
        text: text.into(),
        status,
        edited: false,
        intent_ref: None,
    }
}

fn question(text: &str, answer: Answer) -> Question {
    Question {
        id: QuestionId::new(),
        text: text.into(),
        options: vec![
            QuestionOption {
                id: "a".into(),
                label: "Keep it".into(),
            },
            QuestionOption {
                id: "b".into(),
                label: "Drop it".into(),
            },
        ],
        answer,
    }
}

fn intent(content: Content) -> Intent {
    let now = Utc::now();
    Intent {
        id: IntentId::new(),
        repo_id: RepoId::new(),
        slug: "fix-login".into(),
        state: IntentState::Complete,
        phase: Phase::Review,
        seed: Seed {
            kind: None,
            source: SeedSource::Text,
            text: "x".into(),
        },
        content,
        agent_id: None,
        created_at: now,
        updated_at: now,
        queued_at: None,
        sent_at: None,
        resolved_at: None,
        last_start_error: None,
    }
}

fn fixture() -> Intent {
    intent(Content {
        title: "Fix login".into(),
        kind: IntentKind::Bug,
        current_behavior: "Login fails for ünïcode names.".into(),
        questions: vec![
            question(
                "Keep the old route?",
                Answer::Option {
                    option_id: "a".into(),
                },
            ),
            question("Which logger?", Answer::Skipped),
        ],
        preconditions: vec![item("User exists", ItemStatus::Accepted)],
        criteria: vec![
            item(
                "Login works for `名前` users — **always**",
                ItemStatus::Accepted,
            ),
            item("DROPPED criterion", ItemStatus::Dropped),
            item("PENDING criterion", ItemStatus::Pending),
            item("Errors are shown", ItemStatus::Accepted),
        ],
        stories: vec![item("As a user I can log in", ItemStatus::Accepted)],
        depends_on: vec![],
    })
}

#[test]
fn first_message_golden() {
    let deps = vec![DepSummary {
        slug: "auth-core".into(),
        title: "Auth core".into(),
        current_behavior: format!("{}{}", "a".repeat(300), "TAIL"),
    }];
    let brief = compose(&fixture(), &deps);
    let expected = format!(
        "# Fix login

Kind: bug

## Current behavior

Login fails for ünïcode names.

## Preconditions

- User exists

## Acceptance criteria

1. Login works for `名前` users — **always**
2. Errors are shown

## User stories

- As a user I can log in

## Steering answers

- Keep the old route? — Keep it
- Which logger? — no answer; use your judgment

## Depends on

- Auth core (auth-core): {}

Before you start: if the repository has a remote, fetch it and rebase this branch onto the remote's default branch, so the merged work of these dependencies is present.",
        "a".repeat(300)
    );
    assert_eq!(brief.first_message, expected);
    assert!(!brief.first_message.contains("DROPPED"));
    assert!(!brief.first_message.contains("PENDING"));
}

#[test]
fn empty_sections_are_omitted() {
    let mut i = fixture();
    i.content.preconditions.clear();
    i.content.stories.clear();
    i.content.questions.clear();
    i.content.current_behavior = String::new();
    let msg = compose(&i, &[]).first_message;
    assert_eq!(
        msg,
        "# Fix login\n\nKind: bug\n\n## Acceptance criteria\n\n1. Login works for `名前` users — **always**\n2. Errors are shown"
    );
}

#[test]
fn system_append_covers_the_fixed_instructions() {
    let s = compose(&fixture(), &[]).system_append;
    for needle in [
        "this worktree",
        "acceptance criterion",
        "Commit",
        "pull request",
        "git remote",
    ] {
        assert!(s.contains(needle), "missing {needle:?} in {s}");
    }
}
