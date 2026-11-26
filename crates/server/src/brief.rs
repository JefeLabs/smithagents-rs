//! Composes the instructions an execution agent receives for an intent: a fixed
//! system-prompt append and a Markdown first message built from the intent's
//! accepted content and, for an intent made by Submit, where its files are.

use smithagents_core::intent::{Answer, Intent, IntentKind, Item, ItemStatus};

/// Title of the section that points a feedback intent's agent at its files.
pub const EVIDENCE_TITLE: &str = "Evidence";

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

fn kind_label(kind: IntentKind) -> String {
    kind.wire_name().replace('_', " ")
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

/// `folder` when it is a plain repository-relative path, as the daemon writes
/// it: `/`-separated segments of ASCII letters, digits, `-`, `_` and `.`,
/// none of them empty, `.` or `..`. Anything else is never named.
fn relative_folder(folder: &str) -> Option<&str> {
    let plain = |segment: &str| {
        !matches!(segment, "" | "." | "..")
            && segment
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    };
    folder.split('/').all(plain).then_some(folder)
}

/// The Evidence section of an intent made by Submit (an idea intent has its
/// own brief), naming the folder from the origin the daemon stored.
fn evidence(intent: &Intent) -> Option<String> {
    let origin = intent.origin.as_ref().filter(|o| o.idea.is_none())?;
    let folder = relative_folder(&origin.folder)?;
    Some(format!(
        "## {EVIDENCE_TITLE}\n\nThe feedback files are in `{folder}/` on this branch. Read \
         `issue.md` and look at its images before you start. Leave the folder in place: the \
         pull request carries it."
    ))
}

pub fn compose(intent: &Intent, deps: &[DepSummary]) -> Brief {
    let c = &intent.content;
    let mut parts = vec![format!("# {}\n\nKind: {}", c.title, kind_label(c.kind))];

    let behavior = c.current_behavior.trim();
    if !behavior.is_empty() {
        parts.push(format!("## Current behavior\n\n{behavior}"));
    }
    parts.extend(evidence(intent));
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
