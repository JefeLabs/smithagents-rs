//! Pure view models for the shell: what the views show, computed without
//! GPUI so it can be unit-tested.

use std::path::{Path, PathBuf};

use smithagents_center_host::PixelRect;
use smithagents_client::ClientError;
use smithagents_core::agent::{Agent, AgentState, Repo};
use smithagents_core::feedback::{effective_meta_repo, FeedbackSettings};
use smithagents_core::ids::{AgentId, RepoId};
use smithagents_theme::state::ThemeChoice;
use smithagents_theme::tokens::{TextScale, ThemeSet};
use unicode_segmentation::UnicodeSegmentation as _;

use crate::link::{LinkError, LinkFailure};
use crate::model::{Connection, ShellModel};
use crate::tabs::TabError;
use crate::update::UpdateState;

/// How the connection banner is drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BannerSeverity {
    /// Connecting or reconnecting: the link keeps trying by itself.
    Warning,
    /// Something the user must act on (another daemon version runs).
    Error,
}

/// The connection banner above the center.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BannerVm {
    /// The headline, then any detail lines.
    pub text: String,
    pub severity: BannerSeverity,
}

/// What the banner and the status bar say when a daemon of another version
/// runs: the link keeps retrying, but only quitting that daemon helps.
pub fn version_mismatch_text(running: &str, expected: &str) -> String {
    format!(
        "A different SmithAgents daemon version is running ({running}; this app needs \
         {expected}). Quit it to continue."
    )
}

/// The connection banner, or `None` when connected.
///
/// A version mismatch reads as its own error sentence
/// ([`version_mismatch_text`]), not as a reconnect. Otherwise the first line
/// says what the link is doing; a non-blank failure (the link's last connect
/// error) follows, trimmed, on a second line, as a warning.
pub fn connection_banner(
    connection: &Connection,
    failure: Option<&LinkFailure>,
) -> Option<BannerVm> {
    let headline = match connection {
        Connection::Connected => return None,
        Connection::Connecting => "Connecting to the local daemon…",
        Connection::Reconnecting => "Reconnecting to the local daemon…",
    };
    if let Some(LinkFailure::VersionMismatch { running, expected }) = failure {
        return Some(BannerVm {
            text: version_mismatch_text(running, expected),
            severity: BannerSeverity::Error,
        });
    }
    let error = failure.map(LinkFailure::message);
    let text = match error.as_deref().map(str::trim).filter(|e| !e.is_empty()) {
        Some(error) => format!("{headline}\n{error}"),
        None => headline.to_owned(),
    };
    Some(BannerVm {
        text,
        severity: BannerSeverity::Warning,
    })
}

/// The text of [`connection_banner`].
pub fn banner_text(connection: &Connection, failure: Option<&LinkFailure>) -> Option<String> {
    connection_banner(connection, failure).map(|banner| banner.text)
}

// ---- Status bar --------------------------------------------------------------

/// How a status message is drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusKind {
    Info,
    Error,
}

/// One line for the status bar's center.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusMessage {
    pub text: String,
    pub kind: StatusKind,
}

impl StatusMessage {
    /// An `Info` message. All whitespace, newlines included, collapses to
    /// single spaces: the bar shows one line.
    pub fn info(text: impl AsRef<str>) -> Self {
        Self::new(text.as_ref(), StatusKind::Info)
    }

    /// An `Error` message, normalised like [`StatusMessage::info`].
    pub fn error(text: impl AsRef<str>) -> Self {
        Self::new(text.as_ref(), StatusKind::Error)
    }

    fn new(text: &str, kind: StatusKind) -> Self {
        Self {
            text: text.split_whitespace().collect::<Vec<_>>().join(" "),
            kind,
        }
    }
}

/// What the status bar's center shows. While the connection is not
/// `Connected` that is the connection's own status (the banner's wording on
/// one line, the link's error after a colon; an error when the banner is
/// one), which a transient message never replaces; otherwise the transient
/// message, if any.
pub fn status_to_show(
    connection: &Connection,
    failure: Option<&LinkFailure>,
    transient: Option<&StatusMessage>,
) -> Option<StatusMessage> {
    match connection_banner(connection, failure) {
        Some(banner) => {
            let line = banner.text.replacen('\n', ": ", 1);
            Some(match banner.severity {
                BannerSeverity::Warning => StatusMessage::info(line),
                BannerSeverity::Error => StatusMessage::error(line),
            })
        }
        None => transient.cloned(),
    }
}

/// The status line for a repository that was added.
pub fn repository_added_text(path: &Path) -> String {
    let name = path.file_name().map_or_else(
        || path.display().to_string(),
        |n| n.to_string_lossy().into_owned(),
    );
    format!("Repository added: {name}")
}

/// The version as the status bar shows it.
pub fn version_text(version: &str) -> String {
    format!("v{version}")
}

/// The update chip in the title bar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateButtonVm {
    pub label: String,
    pub enabled: bool,
    /// The accessible name, which includes the version.
    pub a11y_label: String,
}

/// The update chip for `state`, or `None` when there is nothing to
/// install.
pub fn update_button(state: &UpdateState) -> Option<UpdateButtonVm> {
    let (label, enabled, version) = match state {
        UpdateState::UpToDate => return None,
        UpdateState::Available { version } => ("Update available", true, version),
        UpdateState::Installing { version } => ("Installing…", false, version),
        UpdateState::Failed { version, .. } => ("Update failed, retry", true, version),
        UpdateState::RestartRequired { version } => ("Restart to update", true, version),
    };
    Some(UpdateButtonVm {
        label: label.to_owned(),
        enabled,
        a11y_label: format!("{label}: {}", version_text(version)),
    })
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

/// How a state's status dot is drawn. The shape is a second cue next to the
/// color, so no state is told apart by color alone (in the dark theme
/// `status-info` and `muted-foreground` are close).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DotStyle {
    /// The theme token the dot (or its ring) paints with.
    pub token: &'static str,
    /// A filled dot; `false` draws a hollow ring.
    pub filled: bool,
    /// Drawn at reduced opacity.
    pub dimmed: bool,
}

/// The status dot of a state:
/// - idle: a hollow ring in `muted-foreground`;
/// - working: a filled `status-info` dot;
/// - quiet (a turn is running but silent): the working dot, dimmed;
/// - done: a filled `status-ok` dot;
/// - dead: a filled `status-error` dot.
///
/// Only the red, yellow and green hues are reserved for status (agent colors
/// avoid them; the agent palette does include blues). `status-info` is the
/// theme's low-chroma info color.
pub fn state_dot(kind: StateKind) -> DotStyle {
    let (token, filled, dimmed) = match kind {
        StateKind::Idle => ("muted-foreground", false, false),
        StateKind::Working => ("status-info", true, false),
        StateKind::Quiet => ("status-info", true, true),
        StateKind::Done => ("status-ok", true, false),
        StateKind::Dead => ("status-error", true, false),
    };
    DotStyle {
        token,
        filled,
        dimmed,
    }
}

/// The theme token a state's status dot paints with ([`state_dot`]).
pub fn state_token(kind: StateKind) -> &'static str {
    state_dot(kind).token
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
/// What a command says when the daemon took the request but never answered
/// in time, or the connection dropped while it waited.
pub const DAEMON_SILENT_TEXT: &str = "The daemon did not answer";
/// What the New agent button says when there is no repository.
pub const NEED_REPO_TEXT: &str = "Add a source repository first";

/// The title of the notification for a chat tab that did not open, and the
/// start of its status line ("Cannot open the chat: <error>").
pub const OPEN_CHAT_FAILED: &str = "Cannot open the chat";

/// A tab that did not open, as the notification's sentence: the error's
/// words with a capital first letter ([`NOT_CONNECTED_TEXT`] when there is
/// no connection). Never carries the token: no [`TabError`] does.
pub fn tab_error_text(error: &TabError) -> String {
    match error {
        TabError::NotConnected => NOT_CONNECTED_TEXT.to_owned(),
        error => {
            let text = error.to_string();
            let mut chars = text.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().chain(chars).collect(),
                None => text,
            }
        }
    }
}

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

// ---- Settings pane -----------------------------------------------------------

/// A collapsible section of the Settings pane, top to bottom.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SettingsSection {
    /// App scope: theme, text size, font.
    Appearance,
    /// Space scope: what the daemon does for every agent, today the limit on
    /// agents working at once.
    AgentDefaults,
    /// Space scope: the daemon's source repositories.
    Repositories,
}

impl SettingsSection {
    /// Every section, in the pane's order (the order is the scroll index).
    pub const ALL: [SettingsSection; 3] = [
        SettingsSection::Appearance,
        SettingsSection::AgentDefaults,
        SettingsSection::Repositories,
    ];

    /// The section's heading, also its accessible group name.
    pub fn title(self) -> &'static str {
        match self {
            SettingsSection::Appearance => "Appearance",
            SettingsSection::AgentDefaults => "Agent defaults",
            SettingsSection::Repositories => "Source repositories",
        }
    }

    /// The persisted spelling (in `app-state.json`).
    pub fn as_str(self) -> &'static str {
        match self {
            SettingsSection::Appearance => "appearance",
            SettingsSection::AgentDefaults => "agent_defaults",
            SettingsSection::Repositories => "repositories",
        }
    }

    /// Reads [`SettingsSection::as_str`]'s spelling; `None` for anything else.
    pub fn parse(text: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|section| section.as_str() == text)
    }

    /// The section's position in the pane.
    pub fn index(self) -> usize {
        Self::ALL
            .iter()
            .position(|section| *section == self)
            .expect("every section is in SettingsSection::ALL")
    }
}

/// What the theme choice calls following the OS appearance.
pub const SYSTEM_THEME_LABEL: &str = "System";

/// One entry of the theme choice.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThemeOptionVm {
    pub choice: ThemeChoice,
    pub label: String,
    pub selected: bool,
}

/// "System" first, then each theme of `set` by its name, in the set's order.
/// Exactly one is selected: `current`, or "System" when `current` names a
/// theme the set no longer has (the controller resolves such a choice to the
/// system pair too).
pub fn theme_options(set: &ThemeSet, current: &ThemeChoice) -> Vec<ThemeOptionVm> {
    let known = match current {
        ThemeChoice::System => None,
        ThemeChoice::Id(id) => set.theme(id).map(|theme| theme.id.as_str()),
    };
    let system = ThemeOptionVm {
        choice: ThemeChoice::System,
        label: SYSTEM_THEME_LABEL.to_owned(),
        selected: known.is_none(),
    };
    std::iter::once(system)
        .chain(set.themes.iter().map(|theme| ThemeOptionVm {
            choice: ThemeChoice::Id(theme.id.clone()),
            label: theme.name.clone(),
            selected: known == Some(theme.id.as_str()),
        }))
        .collect()
}

/// `value` in hundredths, for exact step arithmetic.
fn hundredths(value: f64) -> i64 {
    (value * 100.0).round() as i64
}

/// The text sizes the stepper offers: `min` to `max` in 0.05 steps, each
/// rounded to two decimals, plus `max` itself and the default (clamped into
/// the bounds) when they fall between steps; sorted, without repeats. Bounds
/// that are not finite or are inverted give only the default (or 1.0).
pub fn text_scale_steps(scale: &TextScale) -> Vec<f64> {
    let default = if scale.default.is_finite() {
        scale.default
    } else {
        1.0
    };
    if !(scale.min.is_finite() && scale.max.is_finite() && scale.min <= scale.max) {
        return vec![hundredths(default) as f64 / 100.0];
    }
    let (min, max) = (hundredths(scale.min), hundredths(scale.max));
    let mut steps: Vec<i64> = (min..=max).step_by(5).collect();
    steps.push(max);
    steps.push(hundredths(default).clamp(min, max));
    steps.sort_unstable();
    steps.dedup();
    steps.into_iter().map(|c| c as f64 / 100.0).collect()
}

/// The step closest to `value`; `default` when `value` is not finite, lies
/// outside the steps' range, or there are no steps.
pub fn nearest_step(steps: &[f64], value: f64, default: f64) -> f64 {
    const SLACK: f64 = 1e-9;
    let (Some(first), Some(last)) = (steps.first(), steps.last()) else {
        return default;
    };
    if !value.is_finite() || value < first - SLACK || value > last + SLACK {
        return default;
    }
    steps
        .iter()
        .copied()
        .min_by(|a, b| (a - value).abs().total_cmp(&(b - value).abs()))
        .unwrap_or(default)
}

/// The step after (or before) the one nearest `current`; `None` at either
/// end, so the stepper's button there is disabled.
pub fn stepped_text_scale(steps: &[f64], current: f64, up: bool) -> Option<f64> {
    let nearest = nearest_step(steps, current, f64::NAN);
    let ix = steps.iter().position(|step| *step == nearest)?;
    let next = if up {
        ix.checked_add(1)?
    } else {
        ix.checked_sub(1)?
    };
    steps.get(next).copied()
}

/// A text scale as the stepper shows it: "100%".
pub fn text_scale_label(scale: f64) -> String {
    let scale = if scale.is_finite() { scale } else { 1.0 };
    format!("{}%", hundredths(scale))
}

/// One entry of the font choice.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FontOptionVm {
    pub id: String,
    pub label: String,
    pub selected: bool,
}

/// Each font of `set` by its label; `current`, or the first font when
/// `current` is unknown (as the controller resolves it), is selected.
pub fn font_options(set: &ThemeSet, current: &str) -> Vec<FontOptionVm> {
    let fonts = &set.base.fonts;
    let selected = fonts
        .iter()
        .position(|font| font.id == current)
        .unwrap_or(0);
    fonts
        .iter()
        .enumerate()
        .map(|(ix, font)| FontOptionVm {
            id: font.id.clone(),
            label: font.label.clone(),
            selected: ix == selected,
        })
        .collect()
}

/// One row of the Source repositories list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoRowVm {
    pub id: RepoId,
    /// The folder's name ([`repo_title`]).
    pub name: String,
    /// The full path, for display.
    pub path: String,
    /// `None` for a plain folder.
    pub default_branch: Option<String>,
    /// Intents and feedback go to this repository
    /// ([`effective_meta_repo`]).
    pub is_meta: bool,
    /// The row offers "Use for intents and feedback": not the meta
    /// repository, there is a choice to make, and the settings are loaded.
    pub can_set_meta: bool,
}

/// The daemon's repositories by name (ignoring case), then path. Names and
/// paths are shown as they are, spaces and any script included. `settings`
/// is `None` until the daemon's feedback settings are loaded: then nothing
/// is stored, and no row can be chosen.
pub fn repo_rows(repos: &[Repo], settings: Option<&FeedbackSettings>) -> Vec<RepoRowVm> {
    let meta = effective_meta_repo(settings.and_then(|s| s.meta_repo_id), repos);
    let mut rows: Vec<RepoRowVm> = repos
        .iter()
        .map(|repo| {
            let is_meta = meta == Some(repo.id);
            RepoRowVm {
                id: repo.id,
                name: repo_title(&repo.path),
                path: repo.path.display().to_string(),
                default_branch: repo.default_branch().map(str::to_owned),
                is_meta,
                can_set_meta: settings.is_some() && !is_meta && repos.len() >= 2,
            }
        })
        .collect();
    rows.sort_by(|a, b| {
        a.name
            .to_lowercase()
            .cmp(&b.name.to_lowercase())
            .then_with(|| a.name.cmp(&b.name))
            .then_with(|| a.path.cmp(&b.path))
    });
    rows
}

/// What a failed command says when the request never got an answer (the
/// connection failed or broke). The error itself is not shown: it is
/// transport detail.
pub const DAEMON_UNREACHABLE_TEXT: &str = "Could not reach the daemon";

/// What a failed upload says when a local file to send could not be opened
/// (nothing was sent).
pub const FILE_UNREADABLE_TEXT: &str = "A file to upload could not be opened";

/// A failed add as one line under the path field.
///
/// A daemon answer with a known code reads as our own sentence (the daemon's
/// messages carry its internal `git: ` / `store: ` prefixes, see
/// [`repo_code_text`]). Any other answer shows the daemon's message without
/// that prefix, first letter capitalized, on one line. A connection or
/// transport failure reads as a generic line. Nothing here prints a request
/// (so never a token).
pub fn repo_error_text(error: &LinkError) -> String {
    match error {
        LinkError::NotConnected => NOT_CONNECTED_TEXT.to_owned(),
        LinkError::Stopped => "The daemon link has stopped".to_owned(),
        LinkError::Client(ClientError::Api { code, message, .. }) => match repo_code_text(code) {
            Some(text) => text.to_owned(),
            None => daemon_message_text(message),
        },
        LinkError::Client(ClientError::Http(_) | ClientError::Ws(_)) => {
            DAEMON_UNREACHABLE_TEXT.to_owned()
        }
        LinkError::Client(ClientError::File { .. }) => FILE_UNREADABLE_TEXT.to_owned(),
    }
}

/// Our sentence for a daemon error code an add can answer with. The codes
/// and the messages they replace are the daemon's
/// (`crates/server/src/api.rs`, `service.rs`, `git.rs`, `store.rs`):
///
/// | code | daemon message |
/// |---|---|
/// | `path_not_found` | `git: no directory at <path>` |
/// | `not_a_git_repo` | `git: not a git repository` |
/// | `bare_repo` | `git: bare repositories are not supported` |
/// | `no_default_branch` | `git: could not determine the default branch` |
/// | `repo_exists` | `store: a repo with this path already exists` |
fn repo_code_text(code: &str) -> Option<&'static str> {
    Some(match code {
        "path_not_found" => "No directory at that path.",
        "not_a_git_repo" => "That directory is not a git repository.",
        "bare_repo" => "Bare repositories are not supported.",
        "no_default_branch" => "Could not determine the repository's default branch.",
        "repo_exists" => "That repository is already added.",
        _ => return None,
    })
}

/// What an add without its own sentence says when the daemon's message is
/// blank (or only a prefix).
pub const REPO_ADD_FAILED_TEXT: &str = "The daemon could not add the repository.";

/// The daemon's message for display: one line, without the daemon's
/// internal `git: ` or `store: ` prefix, first letter capitalized.
fn daemon_message_text(message: &str) -> String {
    let message = one_line(message);
    let message = ["git:", "store:"]
        .iter()
        .find_map(|prefix| message.strip_prefix(prefix))
        .map_or(message.as_str(), str::trim_start);
    if message.is_empty() {
        REPO_ADD_FAILED_TEXT.to_owned()
    } else {
        capitalized(message)
    }
}

/// Whether `repo` (a path the daemon reports for a newly added repository) is
/// the one an add was sent for: the same path, or, as the daemon records the
/// git top level of the canonical path, `sent_canonical` inside it.
pub fn repo_matches_sent_path(sent: &Path, sent_canonical: Option<&Path>, repo: &Path) -> bool {
    sent == repo || sent_canonical.is_some_and(|canonical| canonical.starts_with(repo))
}

pub(crate) fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

pub(crate) fn capitalized(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// What the path field says when it is empty.
pub const ENTER_A_PATH_TEXT: &str = "Enter a path";
/// What the path field says about a relative path.
pub const ABSOLUTE_PATH_TEXT: &str = "Enter an absolute path";
/// What the path field says about a URL: the daemon has no clone endpoint.
pub const CLONE_UNSUPPORTED_TEXT: &str =
    "Cloning from a URL is not supported yet. Enter a local path.";
/// What the Source repositories section says when there is none.
pub const NO_REPOS_TEXT: &str = "No repositories yet. Add a local git repository to create agents.";

/// [`validate_repo_path_input_in`] with the user's home directory.
pub fn validate_repo_path_input(raw: &str) -> Result<PathBuf, &'static str> {
    validate_repo_path_input_in(raw, std::env::home_dir().as_deref())
}

/// The path to send to the daemon for the typed `raw`, or the line to show
/// under the field. It trims, rejects an empty field and a URL (no clone
/// yet), expands a leading `~` or `~/` to `home`, and requires an absolute
/// path. Whether the path exists and is a git repository is the daemon's
/// call: its answer is shown the same way.
pub fn validate_repo_path_input_in(
    raw: &str,
    home: Option<&Path>,
) -> Result<PathBuf, &'static str> {
    let text = raw.trim();
    if text.is_empty() {
        return Err(ENTER_A_PATH_TEXT);
    }
    if looks_like_url(text) {
        return Err(CLONE_UNSUPPORTED_TEXT);
    }
    let path = match text.strip_prefix('~') {
        Some(rest) if rest.is_empty() || rest.starts_with('/') => {
            let home = home.ok_or(ABSOLUTE_PATH_TEXT)?;
            match rest.trim_start_matches('/') {
                "" => home.to_path_buf(),
                rest => home.join(rest),
            }
        }
        _ => PathBuf::from(text),
    };
    if !path.is_absolute() {
        return Err(ABSOLUTE_PATH_TEXT);
    }
    Ok(path)
}

/// A git remote rather than a local path: a URL scheme git clones from, or
/// the scp-like `git@host:` form.
fn looks_like_url(text: &str) -> bool {
    let lower = text.to_lowercase();
    ["http://", "https://", "ssh://", "git://", "git@"]
        .iter()
        .any(|prefix| lower.starts_with(prefix))
}
