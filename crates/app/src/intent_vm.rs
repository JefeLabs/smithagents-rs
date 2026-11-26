//! View models for the intent views: every label, counter, status and
//! sentence they draw, derived without GPUI so it can be unit-tested.

use std::ops::RangeInclusive;

use chrono::{DateTime, FixedOffset, Utc};
use smithagents_client::ClientError;
use smithagents_core::agent::{AgentState, Repo};
use smithagents_core::ids::{IntentId, ItemId, RepoId};
use smithagents_core::intent::{
    Answer, Content, Intent, IntentKind, IntentState, Item, ItemStatus, Phase, Question,
};

use crate::link::LinkError;
use crate::model::{Connection, ShellModel};
use crate::view_models::{
    capitalized, one_line, state_dot, state_kind, state_label, DotStyle, DAEMON_UNREACHABLE_TEXT,
    NOT_CONNECTED_TEXT,
};

// ---- Kinds -------------------------------------------------------------------

/// The name of a kind on a card or a header.
pub fn kind_label(kind: IntentKind) -> &'static str {
    match kind {
        IntentKind::NewFeature => "New feature",
        IntentKind::Enhancement => "Enhancement",
        IntentKind::Bug => "Bug",
        IntentKind::Refactor => "Refactor",
        IntentKind::Maintenance => "Maintenance",
        IntentKind::Task => "Task",
    }
}

/// One row of the create screen's kind list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KindChoice {
    pub kind: IntentKind,
    pub title: &'static str,
    pub description: &'static str,
    /// The key binding that picks it (GPUI keystroke syntax).
    pub key: &'static str,
    /// The same binding as it is drawn.
    pub shortcut: &'static str,
}

/// The kinds the create screen offers, in order. Task is not offered.
pub const KIND_CHOICES: [KindChoice; 5] = [
    KindChoice {
        kind: IntentKind::NewFeature,
        title: "New feature",
        description: "a new capability",
        key: "alt-f",
        shortcut: "⌥F",
    },
    KindChoice {
        kind: IntentKind::Enhancement,
        title: "Enhancement",
        description: "extend an existing capability",
        key: "alt-e",
        shortcut: "⌥E",
    },
    KindChoice {
        kind: IntentKind::Bug,
        title: "Bug",
        description: "a capability that misbehaves",
        key: "alt-b",
        shortcut: "⌥B",
    },
    KindChoice {
        kind: IntentKind::Refactor,
        title: "Refactor",
        description: "restructure, same behavior",
        key: "alt-r",
        shortcut: "⌥R",
    },
    KindChoice {
        kind: IntentKind::Maintenance,
        title: "Maintenance",
        description: "upgrades, config, housekeeping",
        key: "alt-m",
        shortcut: "⌥M",
    },
];

// ---- State and filter --------------------------------------------------------

/// The state an intent shows: a draft shows its phase.
pub fn intent_state_label(intent: &Intent) -> &'static str {
    match intent.state {
        IntentState::Draft => match intent.phase {
            Phase::Polishing { .. } => "Polishing",
            Phase::Questions => "Questions",
            Phase::Review => "Review",
            Phase::Failed { .. } => "Polish failed",
        },
        IntentState::Complete => "Complete",
        IntentState::Queued => "Queued",
        IntentState::Sent => "Sent",
        IntentState::Resolved => "Resolved",
    }
}

/// The list's filter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum IntentFilterChoice {
    #[default]
    All,
    DraftAndPolishing,
    CompleteAndQueued,
    Sent,
    Resolved,
}

impl IntentFilterChoice {
    pub const ALL: [IntentFilterChoice; 5] = [
        IntentFilterChoice::All,
        IntentFilterChoice::DraftAndPolishing,
        IntentFilterChoice::CompleteAndQueued,
        IntentFilterChoice::Sent,
        IntentFilterChoice::Resolved,
    ];

    pub fn label(self) -> &'static str {
        match self {
            IntentFilterChoice::All => "All",
            IntentFilterChoice::DraftAndPolishing => "Draft and polishing",
            IntentFilterChoice::CompleteAndQueued => "Complete and queued",
            IntentFilterChoice::Sent => "Sent",
            IntentFilterChoice::Resolved => "Resolved",
        }
    }

    /// The short name used in element ids.
    pub fn as_str(self) -> &'static str {
        match self {
            IntentFilterChoice::All => "all",
            IntentFilterChoice::DraftAndPolishing => "draft",
            IntentFilterChoice::CompleteAndQueued => "complete",
            IntentFilterChoice::Sent => "sent",
            IntentFilterChoice::Resolved => "resolved",
        }
    }

    pub fn matches(self, intent: &Intent) -> bool {
        match self {
            IntentFilterChoice::All => true,
            IntentFilterChoice::DraftAndPolishing => intent.state == IntentState::Draft,
            IntentFilterChoice::CompleteAndQueued => {
                matches!(intent.state, IntentState::Complete | IntentState::Queued)
            }
            IntentFilterChoice::Sent => intent.state == IntentState::Sent,
            IntentFilterChoice::Resolved => intent.state == IntentState::Resolved,
        }
    }
}

// ---- Body preview ------------------------------------------------------------

/// How many characters a preview line keeps.
const PREVIEW_CHARS: usize = 200;
/// How many characters of a line [`plain`] strips marks from: plenty for a
/// [`PREVIEW_CHARS`] preview, and a bound on [`without_links`], which scans
/// the rest of the line again for each `[` that opens no link.
const STRIPPED_CHARS: usize = 2_000;

/// The first heading and the first line of text of a Markdown body, as plain
/// text for a card.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BodyPreview {
    pub heading: Option<String>,
    pub line: Option<String>,
}

/// The heading text of `line` when it is a Markdown heading (one to six
/// `#` and a space).
fn heading_of(line: &str) -> Option<&str> {
    let marks = line.chars().take_while(|c| *c == '#').count();
    if !(1..=6).contains(&marks) {
        return None;
    }
    line[marks..].strip_prefix(' ')
}

/// `[text](url)` becomes `text`.
fn without_links(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(open) = rest.find('[') {
        let after = &rest[open + 1..];
        let link = after.find("](").and_then(|close| {
            let url = &after[close + 2..];
            url.find(')').map(|end| (close, close + 2 + end + 1))
        });
        match link {
            Some((close, consumed)) => {
                out.push_str(&rest[..open]);
                out.push_str(&after[..close]);
                rest = &after[consumed..];
            }
            None => {
                out.push_str(&rest[..=open]);
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// A line as plain text: no list or quote marker, links reduced to their
/// text, no emphasis or code marks, trimmed, and cut to [`PREVIEW_CHARS`]
/// characters. Only its first [`STRIPPED_CHARS`] characters are read.
fn plain(line: &str) -> String {
    let line = match line.char_indices().nth(STRIPPED_CHARS) {
        Some((end, _)) => &line[..end],
        None => line,
    };
    let line = line.trim();
    let line = ["- ", "* ", "> "]
        .iter()
        .find_map(|marker| line.strip_prefix(marker))
        .unwrap_or_else(|| {
            let digits = line.chars().take_while(char::is_ascii_digit).count();
            match line[digits..].strip_prefix(". ") {
                Some(rest) if digits > 0 => rest,
                _ => line,
            }
        });
    let line = without_links(line)
        .replace("**", "")
        .replace("__", "")
        .replace('`', "");
    line.trim().chars().take(PREVIEW_CHARS).collect()
}

pub fn body_preview(markdown: &str) -> BodyPreview {
    let mut lines = markdown
        .split('\n')
        .map(|line| line.strip_suffix('\r').unwrap_or(line))
        .filter(|line| !line.trim_start().starts_with("```"));
    let mut heading = None;
    let mut rest = Vec::new();
    for line in lines.by_ref() {
        if let Some(text) = heading_of(line) {
            heading = Some(plain(text));
            break;
        }
        rest.push(line);
    }
    // With a heading, the line is the first one after it; without, the first.
    let candidates: Vec<&str> = if heading.is_some() {
        lines.collect()
    } else {
        rest
    };
    let line = candidates
        .into_iter()
        .map(plain)
        .find(|text| !text.is_empty());
    BodyPreview {
        heading: heading.filter(|text| !text.is_empty()),
        line,
    }
}

// ---- Times -------------------------------------------------------------------

/// When an intent was created, in the given zone: "Nov 26, 2026, 6:15 AM".
pub fn created_text(at: DateTime<Utc>, offset: FixedOffset) -> String {
    at.with_timezone(&offset)
        .format("%b %-d, %Y, %-I:%M %p")
        .to_string()
}

/// How long ago `since` was: "now", "5m", "4h" or "3d".
pub fn age_text(since: DateTime<Utc>, now: DateTime<Utc>) -> String {
    let seconds = (now - since).num_seconds();
    match seconds {
        s if s < 60 => "now".to_owned(),
        s if s < 3600 => format!("{}m", s / 60),
        s if s < 86_400 => format!("{}h", s / 3600),
        s => format!("{}d", s / 86_400),
    }
}

// ---- Cards -------------------------------------------------------------------

/// The dot of an intent. `agent` is the state of the agent a Sent intent
/// started, when the model has it.
pub fn intent_dot(intent: &Intent, agent: Option<AgentState>) -> DotStyle {
    let (token, filled, dimmed) = match intent.state {
        IntentState::Draft => match intent.phase {
            Phase::Polishing { .. } => ("status-info", true, false),
            Phase::Questions | Phase::Review => ("status-warn", true, false),
            Phase::Failed { .. } => ("status-error", true, false),
        },
        IntentState::Complete => ("status-ok", false, false),
        IntentState::Queued => ("muted-foreground", true, true),
        IntentState::Sent => match agent {
            Some(state) => return state_dot(state_kind(state)),
            None => ("muted-foreground", false, false),
        },
        IntentState::Resolved => ("status-ok", true, true),
    };
    DotStyle {
        token,
        filled,
        dimmed,
    }
}

/// One card of the list.
#[derive(Debug, Clone, PartialEq)]
pub struct IntentCardVm {
    pub id: IntentId,
    pub slug: String,
    pub title: String,
    pub meta: String,
    pub preview: BodyPreview,
    pub dot: DotStyle,
    /// The hue of the agent a Sent intent started, when the model has it.
    pub accent_hue: Option<f32>,
    pub a11y_label: String,
}

fn card(model: &ShellModel, intent: &Intent, now: DateTime<FixedOffset>) -> IntentCardVm {
    let agent = intent
        .agent_id
        .filter(|_| intent.state == IntentState::Sent)
        .and_then(|id| model.agent(id));
    let state = intent_state_label(intent);
    let mut meta = vec![
        created_text(intent.created_at, *now.offset()),
        state.to_owned(),
    ];
    if let Some(agent) = agent {
        meta.push(state_label(state_kind(agent.state)).to_owned());
    }
    let since = match intent.state {
        IntentState::Sent => intent.sent_at.unwrap_or(intent.updated_at),
        _ => intent.updated_at,
    };
    meta.push(age_text(since, now.with_timezone(&Utc)));
    IntentCardVm {
        id: intent.id,
        slug: intent.slug.clone(),
        title: intent.content.title.clone(),
        meta: meta.join(" | "),
        // As the detail draws it: a draft still in pass 1 shows its seed.
        preview: body_preview(body_text(intent)),
        dot: intent_dot(intent, agent.map(|a| a.state)),
        accent_hue: agent.map(|a| a.color.h),
        a11y_label: format!("{}, {state}", intent.content.title),
    }
}

/// The cards the list draws, in the model's order, for `filter`.
pub fn intent_cards(
    model: &ShellModel,
    filter: IntentFilterChoice,
    now: DateTime<FixedOffset>,
) -> Vec<IntentCardVm> {
    model
        .intents()
        .into_iter()
        .filter(|intent| filter.matches(intent))
        .map(|intent| card(model, intent, now))
        .collect()
}

/// What the list says instead of cards, if anything.
pub fn list_empty_text(model: &ShellModel, filter: IntentFilterChoice) -> Option<&'static str> {
    if !model.intents_loaded() && model.connection() != Connection::Connected {
        return Some(NOT_CONNECTED_TEXT);
    }
    let intents = model.intents();
    if intents.is_empty() {
        Some("No intents yet. Press + to describe what you want.")
    } else if intents.iter().any(|intent| filter.matches(intent)) {
        None
    } else {
        Some("No intents match this filter.")
    }
}

// ---- Detail ------------------------------------------------------------------

/// The review sections of the detail screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SectionKind {
    Questions,
    Preconditions,
    Criteria,
    Stories,
    DependsOn,
}

impl SectionKind {
    pub const ALL: [SectionKind; 5] = [
        SectionKind::Questions,
        SectionKind::Preconditions,
        SectionKind::Criteria,
        SectionKind::Stories,
        SectionKind::DependsOn,
    ];

    pub fn title(self) -> &'static str {
        match self {
            SectionKind::Questions => "Questions",
            SectionKind::Preconditions => "Preconditions",
            SectionKind::Criteria => "Acceptance criteria",
            SectionKind::Stories => "Stories",
            SectionKind::DependsOn => "Depends on",
        }
    }

    /// The short name used in element ids.
    pub fn as_str(self) -> &'static str {
        match self {
            SectionKind::Questions => "questions",
            SectionKind::Preconditions => "preconditions",
            SectionKind::Criteria => "criteria",
            SectionKind::Stories => "stories",
            SectionKind::DependsOn => "depends_on",
        }
    }
}

/// A section's header: title, counter, status and whether it starts open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SectionVm {
    pub kind: SectionKind,
    pub title: &'static str,
    pub counter: String,
    pub status: String,
    /// Questions still to answer, or items still to review.
    pub pending: u32,
    /// Items dropped (0 for Questions).
    pub skipped: u32,
    pub starts_open: bool,
}

/// The items of an item section; empty for Questions.
pub fn section_items(content: &Content, kind: SectionKind) -> &[Item] {
    match kind {
        SectionKind::Questions => &[],
        SectionKind::Preconditions => &content.preconditions,
        SectionKind::Criteria => &content.criteria,
        SectionKind::Stories => &content.stories,
        SectionKind::DependsOn => &content.depends_on,
    }
}

pub fn section_vm(content: &Content, kind: SectionKind) -> SectionVm {
    let (counter, status, pending, skipped) = if kind == SectionKind::Questions {
        let all = content.questions.len();
        let open = content.questions_open() as usize;
        let status = if open > 0 {
            format!("{open} to answer")
        } else if all == 0 {
            "none".to_owned()
        } else {
            "✓ done".to_owned()
        };
        (
            format!("{} of {all} answered", all - open),
            status,
            open as u32,
            0,
        )
    } else {
        let items = section_items(content, kind);
        let count = |status| items.iter().filter(|i| i.status == status).count();
        let (accepted, pending, dropped) = (
            count(ItemStatus::Accepted),
            count(ItemStatus::Pending),
            count(ItemStatus::Dropped),
        );
        let depends = kind == SectionKind::DependsOn;
        let counter = if depends {
            format!("{accepted} picked")
        } else {
            format!("{accepted} of {} accepted", items.len() - dropped)
        };
        let status = if pending > 0 {
            format!("{pending} to review")
        } else if items.is_empty() || (depends && accepted == 0) {
            "none".to_owned()
        } else {
            "✓ done".to_owned()
        };
        (counter, status, pending as u32, dropped as u32)
    };
    SectionVm {
        kind,
        title: kind.title(),
        counter,
        status,
        pending,
        skipped,
        starts_open: pending > 0,
    }
}

/// What a question shows under it once decided: "→ <label>" or "→ Skipped".
pub fn answer_text(question: &Question) -> Option<String> {
    match &question.answer {
        Answer::Unanswered => None,
        Answer::Skipped => Some("→ Skipped".to_owned()),
        Answer::Option { option_id } => {
            let label = question
                .options
                .iter()
                .find(|option| &option.id == option_id)
                .map_or(option_id.as_str(), |option| option.label.as_str());
            Some(format!("→ {label}"))
        }
    }
}

/// What a draft shows while the daemon polishes it.
pub fn polishing_text(pass: u8) -> String {
    format!("Polishing… (pass {pass} of 2)")
}

/// The body to show: the current behavior, or the seed text when that is
/// blank.
pub fn body_text(intent: &Intent) -> &str {
    if intent.content.current_behavior.trim().is_empty() {
        &intent.seed.text
    } else {
        &intent.content.current_behavior
    }
}

/// Whether the header (title, kind) can be edited: a draft that is not being
/// polished, or a complete intent.
pub fn header_editable(intent: &Intent) -> bool {
    match intent.state {
        IntentState::Draft => !matches!(intent.phase, Phase::Polishing { .. }),
        IntentState::Complete => true,
        IntentState::Queued | IntentState::Sent | IntentState::Resolved => false,
    }
}

/// One depends-on item as the section draws it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DependencyVm {
    pub item_id: ItemId,
    /// The slug of the intent it points at, when the model has that intent.
    pub slug: Option<String>,
    pub title: String,
    pub status: ItemStatus,
}

pub fn dependency_vm(model: &ShellModel, item: &Item) -> DependencyVm {
    let target = item.intent_ref.and_then(|id| model.intent(id));
    DependencyVm {
        item_id: item.id,
        slug: target.map(|intent| intent.slug.clone()),
        title: target.map_or_else(|| item.text.clone(), |intent| intent.content.title.clone()),
        status: item.status,
    }
}

/// Why a queued intent has not started yet.
pub fn queued_reason(model: &ShellModel, intent: &Intent) -> String {
    let waiting: Vec<&str> = intent
        .content
        .depends_on
        .iter()
        .filter(|item| item.status == ItemStatus::Accepted)
        .filter_map(|item| item.intent_ref)
        .filter_map(|id| match model.intent(id) {
            Some(target) if target.state == IntentState::Resolved => None,
            Some(target) => Some(target.slug.as_str()),
            None => Some("another intent"),
        })
        .collect();
    if waiting.is_empty() {
        "Waiting for a free agent slot.".to_owned()
    } else {
        format!("Waiting for {} to be resolved.", waiting.join(", "))
    }
}

/// The daemon's own start of a start error, which the sentence already says.
const DAEMON_START_PREFIX: &str = "could not start the agent:";
/// How many characters of a start error the detail shows.
const START_ERROR_CHARS: usize = 300;

/// Why the daemon could not start an intent's agent, from its
/// `last_start_error`: "The agent could not start: <message>". The message
/// (stored as the daemon got it: any length, any lines) is put on one line,
/// loses the daemon's "could not start the agent:" (once, in any case), and
/// is cut to [`START_ERROR_CHARS`] characters, with "…" when cut. With
/// nothing left, the sentence is "The agent could not start."
pub fn start_error_text(message: &str) -> String {
    let message = one_line(message);
    let message = message
        .get(..DAEMON_START_PREFIX.len())
        .filter(|start| start.eq_ignore_ascii_case(DAEMON_START_PREFIX))
        .map_or(message.as_str(), |start| {
            message[start.len()..].trim_start()
        });
    if message.is_empty() {
        return "The agent could not start.".to_owned();
    }
    let mut kept: String = message.chars().take(START_ERROR_CHARS).collect();
    if kept.len() < message.len() {
        kept.push('…');
    }
    format!("The agent could not start: {kept}")
}

// ---- Create and settings -----------------------------------------------------

/// The repository the create screen starts with: `last_used` while it still
/// exists, else the oldest.
pub fn default_repo(repos: &[Repo], last_used: Option<RepoId>) -> Option<RepoId> {
    last_used
        .filter(|id| repos.iter().any(|repo| repo.id == *id))
        .or_else(|| {
            repos
                .iter()
                .min_by_key(|repo| repo.created_at)
                .map(|r| r.id)
        })
}

/// What a failed intent command says, as one line.
pub fn intent_error_text(error: &LinkError) -> String {
    match error {
        LinkError::NotConnected => NOT_CONNECTED_TEXT.to_owned(),
        LinkError::Stopped => "The daemon link has stopped".to_owned(),
        LinkError::Client(ClientError::Http(_) | ClientError::Ws(_)) => {
            DAEMON_UNREACHABLE_TEXT.to_owned()
        }
        LinkError::Client(ClientError::Api { code, message, .. }) => match intent_code_text(code) {
            Some(text) => text.to_owned(),
            None => {
                let message = one_line(message);
                if message.is_empty() {
                    "The daemon could not do that.".to_owned()
                } else {
                    capitalized(&message)
                }
            }
        },
    }
}

/// Our sentence for a daemon error code an intent command can answer with.
fn intent_code_text(code: &str) -> Option<&'static str> {
    Some(match code {
        "polishing" => "The intent is still being polished.",
        "invalid_state" => "The intent changed, so this is no longer possible.",
        "already_answered" => "That question is already answered.",
        "dependency_cycle" => "Those intents would depend on each other.",
        "dependency_cross_repo" => "A dependency must be in the same repository.",
        "intent_not_found" => "That intent no longer exists.",
        "repo_not_found" => "That repository no longer exists.",
        _ => return None,
    })
}

/// The queue limits Agent defaults offers.
pub const QUEUE_LIMIT_CHOICES: RangeInclusive<u32> = 1..=16;

/// A queue limit as the picker shows it; `None` and `0` mean no limit.
pub fn queue_limit_label(limit: Option<u32>) -> String {
    match limit {
        None | Some(0) => "No limit".to_owned(),
        Some(n) => n.to_string(),
    }
}
