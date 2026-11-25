//! Pure view models for the shell: what the views show, computed without
//! GPUI so it can be unit-tested.

use std::path::{Path, PathBuf};

use smithagents_center_host::PixelRect;
use smithagents_client::ClientError;
use smithagents_core::agent::{Agent, AgentState, Repo};
use smithagents_core::ids::{AgentId, RepoId};
use smithagents_theme::state::ThemeChoice;
use smithagents_theme::tokens::{TextScale, ThemeSet};
use unicode_segmentation::UnicodeSegmentation as _;

use crate::link::LinkError;
use crate::model::{Connection, ShellModel};
use crate::update::UpdateState;

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
/// one line, the link's error after a colon), which a transient message never
/// replaces; otherwise the transient message, if any.
pub fn status_to_show(
    connection: &Connection,
    link_error: Option<&str>,
    transient: Option<&StatusMessage>,
) -> Option<StatusMessage> {
    match banner_text(connection, link_error) {
        Some(banner) => {
            let line = banner.replacen('\n', ": ", 1);
            Some(StatusMessage::info(line))
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
    /// Space scope: the daemon's source repositories.
    Repositories,
}

impl SettingsSection {
    /// Every section, in the pane's order (the order is the scroll index).
    pub const ALL: [SettingsSection; 2] =
        [SettingsSection::Appearance, SettingsSection::Repositories];

    /// The section's heading, also its accessible group name.
    pub fn title(self) -> &'static str {
        match self {
            SettingsSection::Appearance => "Appearance",
            SettingsSection::Repositories => "Source repositories",
        }
    }

    /// The persisted spelling (in `app-state.json`).
    pub fn as_str(self) -> &'static str {
        match self {
            SettingsSection::Appearance => "appearance",
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
        match self {
            SettingsSection::Appearance => 0,
            SettingsSection::Repositories => 1,
        }
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
    pub default_branch: String,
}

/// The daemon's repositories by name (ignoring case), then path. Names and
/// paths are shown as they are, spaces and any script included.
pub fn repo_rows(repos: &[Repo]) -> Vec<RepoRowVm> {
    let mut rows: Vec<RepoRowVm> = repos
        .iter()
        .map(|repo| RepoRowVm {
            id: repo.id,
            name: repo_title(&repo.path),
            path: repo.path.display().to_string(),
            default_branch: repo.default_branch.clone(),
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

/// A failed add as one line under the path field: the daemon's own message
/// (first letter capitalized) for an answer it gave, a line for the code when
/// that message is blank, or a generic line for a connection or transport
/// failure. Nothing here prints a request (so never a token).
pub fn repo_error_text(error: &LinkError) -> String {
    match error {
        LinkError::NotConnected => NOT_CONNECTED_TEXT.to_owned(),
        LinkError::Stopped => "The daemon link has stopped".to_owned(),
        LinkError::Client(ClientError::Api { code, message, .. }) => {
            let message = one_line(message);
            if message.is_empty() {
                repo_code_text(code).to_owned()
            } else {
                capitalized(&message)
            }
        }
        LinkError::Client(ClientError::Http(_) | ClientError::Ws(_)) => {
            DAEMON_UNREACHABLE_TEXT.to_owned()
        }
    }
}

/// A line for a daemon error code whose message was blank. The codes are the
/// daemon's (`crates/server/src/api.rs`).
fn repo_code_text(code: &str) -> &'static str {
    match code {
        "path_not_found" => "No directory at that path",
        "not_a_git_repo" => "Not a git repository",
        "bare_repo" => "Bare repositories are not supported",
        "no_default_branch" => "Could not determine the default branch",
        "repo_exists" => "This repository is already added",
        _ => "The daemon could not add the repository",
    }
}

fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn capitalized(text: &str) -> String {
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
