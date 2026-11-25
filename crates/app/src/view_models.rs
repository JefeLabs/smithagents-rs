//! Pure view models for the shell: what the views show, computed without
//! GPUI so it can be unit-tested.

use std::path::Path;

use smithagents_center_host::PixelRect;
use smithagents_client::ClientError;
use smithagents_core::agent::{Agent, AgentState, Repo};
use smithagents_core::ids::{AgentId, RepoId};
use unicode_segmentation::UnicodeSegmentation as _;

use crate::link::LinkError;
use crate::model::{Connection, ShellModel};

/// The connection banner, or `None` when connected.
///
/// The first line says what the link is doing; a non-blank `error` (the
/// link's last connect error) follows, trimmed, on a second line.
pub fn banner_text(connection: &Connection, error: Option<&str>) -> Option<String> {
    let headline = match connection {
        Connection::Connected => return None,
        Connection::Connecting => "Connecting to the local daemon…",
        Connection::Reconnecting => "Reconnecting to the local daemon…",
    };
    match error.map(str::trim).filter(|e| !e.is_empty()) {
        Some(error) => Some(format!("{headline}\n{error}")),
        None => Some(headline.to_owned()),
    }
}

/// The title of an agent's center tab: its name.
pub fn tab_title(agent: &Agent) -> String {
    agent.name.clone()
}

/// The center content area's rectangle for the web engine.
///
/// Input: GPUI's window-relative logical pixels (origin at the top-left of the
/// window's content area) and the window's scale factor. Output: **device
/// pixels**, each coordinate multiplied by the scale, with `scale` carried
/// along so a host that positions views in points can divide it back out.
///
/// Defensive against bad input, so a host never receives a nonsense frame: a
/// non-finite coordinate becomes 0, a negative or non-finite size becomes 0,
/// and a scale that is not a finite positive number is treated as 1.
pub fn center_rect(x: f32, y: f32, w: f32, h: f32, scale: f32) -> PixelRect {
    let finite = |v: f32| if v.is_finite() { f64::from(v) } else { 0.0 };
    let scale = if scale.is_finite() && scale > 0.0 {
        f64::from(scale)
    } else {
        1.0
    };
    PixelRect {
        x: finite(x) * scale,
        y: finite(y) * scale,
        w: finite(w).max(0.0) * scale,
        h: finite(h).max(0.0) * scale,
        scale,
    }
}

/// The left pane's two modes. It is never hidden: compact is a narrow,
/// fixed-width strip (markers only), expanded is the resizable full pane.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum LeftPaneMode {
    #[default]
    Expanded,
    Compact,
}

impl LeftPaneMode {
    /// The other mode: what the left-pane toggle switches to.
    pub fn toggled(self) -> Self {
        match self {
            LeftPaneMode::Expanded => LeftPaneMode::Compact,
            LeftPaneMode::Compact => LeftPaneMode::Expanded,
        }
    }

    /// The persisted spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            LeftPaneMode::Expanded => "expanded",
            LeftPaneMode::Compact => "compact",
        }
    }

    /// Reads [`LeftPaneMode::as_str`]'s spelling; `None` for anything else.
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "expanded" => Some(LeftPaneMode::Expanded),
            "compact" => Some(LeftPaneMode::Compact),
            _ => None,
        }
    }
}

// ---- Agents pane -------------------------------------------------------------

/// What the agents pane shows of an agent's activity. Each kind has one label
/// and one status token, so a state never reads as an agent color.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StateKind {
    Idle,
    Working,
    Quiet,
    Done,
    /// The process died; the next message resumes the session.
    Dead,
}

/// The pane's kind for a daemon state. The chat-only daemon never sets
/// `Blocked`, `Waiting` or `Stopped` (they arrive with Plan 2's queue); until
/// they get kinds of their own they read as idle.
pub fn state_kind(state: AgentState) -> StateKind {
    match state {
        AgentState::Idle | AgentState::Blocked | AgentState::Waiting | AgentState::Stopped => {
            StateKind::Idle
        }
        AgentState::Working => StateKind::Working,
        AgentState::Quiet => StateKind::Quiet,
        AgentState::Done => StateKind::Done,
        AgentState::Dead => StateKind::Dead,
    }
}

/// The words a state shows as.
pub fn state_label(kind: StateKind) -> &'static str {
    match kind {
        StateKind::Idle => "idle",
        StateKind::Working => "working",
        StateKind::Quiet => "quiet",
        StateKind::Done => "done",
        StateKind::Dead => "stopped unexpectedly",
    }
}

/// The theme token a state's status dot paints with. Status colors (red,
/// yellow, green, and the info blue) are reserved; idle and quiet stay muted.
pub fn state_token(kind: StateKind) -> &'static str {
    match kind {
        StateKind::Working => "status-info",
        StateKind::Done => "status-ok",
        StateKind::Dead => "status-error",
        StateKind::Idle | StateKind::Quiet => "muted-foreground",
    }
}

/// One agent as the pane shows it, in either mode.
#[derive(Debug, Clone, PartialEq)]
pub struct AgentRowVm {
    pub id: AgentId,
    pub name: String,
    /// What a compact marker shows: [`agent_initial`] of the name.
    pub initial: String,
    pub state_label: &'static str,
    pub state_kind: StateKind,
    /// The agent color's hue in degrees, unchanged; the theme supplies the
    /// lightness and chroma.
    pub accent_hue: f32,
    pub selected: bool,
    pub has_open_tab: bool,
    /// "<name>, <state label>": the accessibility label, and the compact
    /// marker's tooltip, so colliding initials are told apart.
    pub a11y_label: String,
}

/// Every open agent in the model's order (oldest first).
pub fn agent_rows(model: &ShellModel) -> Vec<AgentRowVm> {
    model
        .agents()
        .into_iter()
        .map(|agent| {
            let kind = state_kind(agent.state);
            AgentRowVm {
                id: agent.id,
                name: agent.name.clone(),
                initial: agent_initial(&agent.name),
                state_label: state_label(kind),
                state_kind: kind,
                accent_hue: agent.color.h,
                selected: model.selected() == Some(agent.id),
                has_open_tab: model.tabs().contains(&agent.id),
                a11y_label: agent_label(&agent.name, kind),
            }
        })
        .collect()
}

/// What a blank name reads as in labels.
const UNNAMED: &str = "Unnamed agent";

/// "<name>, <state label>".
fn agent_label(name: &str, kind: StateKind) -> String {
    let name = if name.trim().is_empty() {
        UNNAMED
    } else {
        name
    };
    format!("{name}, {}", state_label(kind))
}

/// The first grapheme of `name` (after leading blanks), uppercased: "Mercury"
/// and "mars" both give "M", an emoji stays whole. A blank name gives "?".
pub fn agent_initial(name: &str) -> String {
    name.trim_start()
        .graphemes(true)
        .next()
        .map_or_else(|| "?".to_owned(), str::to_uppercase)
}

/// What the New agent button does with the repositories there are.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NewAgentPlan {
    /// No repository yet: send the user to Source repositories.
    NeedRepo,
    /// Exactly one: create in it right away.
    Create(RepoId),
    /// Several: ask. `repos` is the order to list them in: `preselected`
    /// first, then the rest newest first.
    Pick {
        repos: Vec<RepoId>,
        preselected: RepoId,
    },
}

/// Decides the New agent flow. With several repositories the preselected one
/// is `last_used` while it still exists, else the most recently added (by
/// `created_at`; on a tie, the later in `repos`).
pub fn new_agent_plan(repos: &[Repo], last_used: Option<RepoId>) -> NewAgentPlan {
    match repos {
        [] => NewAgentPlan::NeedRepo,
        [only] => NewAgentPlan::Create(only.id),
        _ => {
            let mut sorted: Vec<&Repo> = repos.iter().collect();
            // Reversed, then a stable sort: a tie keeps the later one first.
            sorted.reverse();
            sorted.sort_by_key(|repo| std::cmp::Reverse(repo.created_at));
            let preselected = last_used
                .filter(|id| repos.iter().any(|repo| repo.id == *id))
                .unwrap_or(sorted[0].id);
            let mut order = vec![preselected];
            order.extend(
                sorted
                    .iter()
                    .map(|repo| repo.id)
                    .filter(|id| *id != preselected),
            );
            NewAgentPlan::Pick {
                repos: order,
                preselected,
            }
        }
    }
}

/// One entry of the repository picker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoChoiceVm {
    pub id: RepoId,
    /// The folder's name ([`repo_title`]).
    pub title: String,
    /// The full path, shown as secondary text.
    pub path: String,
    pub preselected: bool,
}

/// The picker's entries for a [`NewAgentPlan::Pick`], in its order. Ids not in
/// `repos` are skipped.
pub fn repo_choices(repos: &[Repo], order: &[RepoId], preselected: RepoId) -> Vec<RepoChoiceVm> {
    order
        .iter()
        .filter_map(|id| repos.iter().find(|repo| repo.id == *id))
        .map(|repo| RepoChoiceVm {
            id: repo.id,
            title: repo_title(&repo.path),
            path: repo.path.display().to_string(),
            preselected: repo.id == preselected,
        })
        .collect()
}

/// A repository's short name: the last component of its path, or the whole
/// path when it has none (such as `/`).
pub fn repo_title(path: &Path) -> String {
    path.file_name().map_or_else(
        || path.display().to_string(),
        |name| name.to_string_lossy().into_owned(),
    )
}

/// What a failed command says when the daemon cannot be reached.
pub const NOT_CONNECTED_TEXT: &str = "Not connected to the daemon";
/// What the New agent button says when there is no repository.
pub const NEED_REPO_TEXT: &str = "Add a source repository first";

/// A failed command as one line for a notification: the daemon's own message
/// when it sent one, else the error.
pub fn command_error_text(error: &LinkError) -> String {
    match error {
        LinkError::NotConnected => NOT_CONNECTED_TEXT.to_owned(),
        LinkError::Stopped => "The daemon link has stopped".to_owned(),
        LinkError::Client(ClientError::Api { message, .. }) if !message.trim().is_empty() => {
            message.trim().to_owned()
        }
        LinkError::Client(_) => error.to_string(),
    }
}
