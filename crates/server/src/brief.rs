//! Composes the instructions an execution agent receives for an intent: a fixed
//! system-prompt append and a Markdown first message built from the intent's
//! accepted content.

use smithagents_core::intent::{Answer, Intent, IntentKind, Item, ItemStatus};

/// How much of a dependency's current behavior is quoted.
const DEP_BEHAVIOR_CHARS: usize = 300;

const SYSTEM_APPEND: &str = "Work only in this worktree. Satisfy every acceptance criterion in \
the intent you are given. Commit your work, and open the pull request when you are done. \
Check `git remote` yourself: a GitHub remote is not required, and if the repository has none, \
leave the commits on your branch, skip the pull request, and say so.";

/// Follows the "Depends on" section of an intent that has dependencies.
const REBASE_ON_DEPENDENCIES: &str = "Before you start: if the repository has a remote, \
fetch it and rebase this branch onto the remote's default branch, so the merged work of \
these dependencies is present.";

/// What the agent needs to know about an intent this one depends on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DepSummary {
    pub slug: String,
    pub title: String,
    pub current_behavior: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Brief {
    pub system_append: String,
    pub first_message: String,
}

fn kind_label(kind: IntentKind) -> &'static str {
    match kind {
        IntentKind::Enhancement => "enhancement",
        IntentKind::Bug => "bug",
        IntentKind::Task => "task",
    }
}

fn accepted(items: &[Item]) -> impl Iterator<Item = &Item> {
    items.iter().filter(|i| i.status == ItemStatus::Accepted)
}

fn section(title: &str, body: Vec<String>) -> Option<String> {
    (!body.is_empty()).then(|| format!("## {title}\n\n{}", body.join("\n")))
}

fn bullets(items: &[Item]) -> Vec<String> {
    accepted(items).map(|i| format!("- {}", i.text)).collect()
}

pub fn compose(intent: &Intent, deps: &[DepSummary]) -> Brief {
    let c = &intent.content;
    let mut parts = vec![format!("# {}\n\nKind: {}", c.title, kind_label(c.kind))];

    let behavior = c.current_behavior.trim();
    if !behavior.is_empty() {
        parts.push(format!("## Current behavior\n\n{behavior}"));
    }
    parts.extend(section("Preconditions", bullets(&c.preconditions)));
    let criteria = accepted(&c.criteria)
        .enumerate()
        .map(|(n, i)| format!("{}. {}", n + 1, i.text))
        .collect();
    parts.extend(section("Acceptance criteria", criteria));
    parts.extend(section("User stories", bullets(&c.stories)));

    let steering = c
        .questions
        .iter()
        .filter_map(|q| match &q.answer {
            Answer::Unanswered => None,
            Answer::Skipped => Some(format!("- {} — no answer; use your judgment", q.text)),
            Answer::Option { option_id } => q
                .options
                .iter()
                .find(|o| &o.id == option_id)
                .map(|o| format!("- {} — {}", q.text, o.label)),
        })
        .collect();
    parts.extend(section("Steering answers", steering));

    let depends = deps
        .iter()
        .map(|d| {
            let behavior: String = d
                .current_behavior
                .chars()
                .take(DEP_BEHAVIOR_CHARS)
                .collect();
            format!("- {} ({}): {}", d.title, d.slug, behavior)
        })
        .collect();
    if let Some(depends) = section("Depends on", depends) {
        // The worktree branches off the local default branch, which may
        // predate the dependencies' merges.
        parts.push(format!("{depends}\n\n{REBASE_ON_DEPENDENCIES}"));
    }

    Brief {
        system_append: SYSTEM_APPEND.to_string(),
        first_message: parts.join("\n\n"),
    }
}
