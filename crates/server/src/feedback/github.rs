//! The command-line tools of the feedback plugin as Submit sees them (is a
//! tool ready, and if not, why) and the GitHub side of a submission, done
//! with `gh`: is it signed in, is a repository public, and the issue.
//!
//! Tools are only looked up and run through the plugin manager, so a plugin
//! that is turned off or not running has no tool ready, and nothing runs.
//! `gh` runs from a neutral directory (the daemon's home, never a
//! repository) and is told the repository on every call that has one
//! (`--repo=github.com/<owner>/<name>`). Every value from a user is joined to
//! its option with `=` or is in a file, so none is read as an option or a
//! file reference. What `gh` prints is never kept: an answer is used only
//! when it is what it must be, and a failure is a fixed sentence with an
//! exit status at most.
//!
//! Before an issue is created, the issues are searched for one made before
//! (`gh` made it but its answer was lost). The search looks only at issues
//! the signed-in user opened (`--author=@me`): the submission id is public
//! once the branch is pushed (the commit's `Feedback-Id` trailer), so on a
//! public repository anyone could open an issue that names it, which would
//! otherwise be taken for this submission's.

use std::path::Path;
use std::time::Duration;

use smithagents_core::feedback::{
    FeedbackFailure, FeedbackKind, FeedbackStep, SubmissionId, ToolReadiness,
};
use smithagents_plugin_host::{MissingReason, ToolRun, ToolRunError, ToolState};

use super::issue::github_repo;
use crate::git::exit_status_text;
use crate::plugins::{ManagerError, PluginManager, FEEDBACK_PLUGIN_ID};

pub const GH: &str = "gh";
pub const AGENT_BROWSER: &str = "agent-browser";
/// The reason of a tool that is not installed.
pub const NOT_INSTALLED: &str = "not installed";
/// The reason of a `gh` whose `gh auth status` failed.
pub const NOT_SIGNED_IN: &str = "not signed in";
/// The reason of a tool while the feedback plugin is disabled, failed to
/// load or was stopped (the manager does not tell these apart).
const TURNED_OFF: &str = "turned off or not running";

/// The longest one `gh` call of a submission may take.
pub const GH_TIMEOUT: Duration = Duration::from_secs(60);
/// The longest a quick check may take: `gh auth status` and `gh repo view`.
pub const VISIBILITY_TIMEOUT: Duration = Duration::from_secs(5);
/// Added to `gh`'s environment: no prompt, no update check, no colour.
pub const GH_ENV: [(&str, &str); 3] = [
    ("GH_PROMPT_DISABLED", "1"),
    ("GH_NO_UPDATE_NOTIFIER", "1"),
    ("NO_COLOR", "1"),
];

/// Whether the feedback plugin's `tool` is ready; otherwise the reason:
/// `not installed`, `could not read its version`, `too old`, or `turned off
/// or not running` while the plugin is disabled, failed to load or was
/// stopped (the manager does not tell these apart).
pub async fn tool_readiness(plugins: &PluginManager, tool: &str) -> ToolReadiness {
    let reason = match plugins.tool_state(FEEDBACK_PLUGIN_ID, tool).await {
        Ok(ToolState::Ready { .. }) => None,
        Ok(ToolState::Missing {
            reason: MissingReason::NotFound,
        }) => Some(NOT_INSTALLED),
        Ok(ToolState::Missing {
            reason: MissingReason::UnreadableVersion,
        }) => Some("could not read its version"),
        Ok(ToolState::TooOld { .. }) => Some("too old"),
        Err(ManagerError::Disabled) => Some(TURNED_OFF),
        Err(e) => {
            tracing::error!(tool, "could not check a feedback tool: {e}");
            Some("not available")
        }
    };
    not_ready(reason)
}

fn not_ready(reason: Option<&str>) -> ToolReadiness {
    ToolReadiness {
        ready: reason.is_none(),
        reason: reason.map(str::to_owned),
    }
}

/// A repository on github.com, as `gh` is told it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GithubRepo {
    pub owner: String,
    pub name: String,
}

impl GithubRepo {
    /// The github.com repository a remote URL names (see
    /// [`github_repo`]); `None` for any other host.
    pub fn from_remote(url: &str) -> Option<GithubRepo> {
        github_repo(url).map(|(owner, name)| GithubRepo { owner, name })
    }

    /// `github.com/<owner>/<name>`: with its host, so a `GH_HOST` in the
    /// daemon's environment cannot send `gh` to another one.
    pub fn full_name(&self) -> String {
        format!("github.com/{}/{}", self.owner, self.name)
    }

    /// The same repository for any case of its names, as on GitHub.
    pub fn key(&self) -> String {
        format!(
            "{}/{}",
            self.owner.to_ascii_lowercase(),
            self.name.to_ascii_lowercase()
        )
    }

    fn repo_arg(&self) -> String {
        format!("--repo={}", self.full_name())
    }

    /// Whether `url` is an issue's address (see [`is_github_issue_url`])
    /// of this repository (its names in any case).
    pub fn is_issue_url(&self, url: &str) -> bool {
        issue_url_parts(url).is_some_and(|(owner, name)| {
            owner.eq_ignore_ascii_case(&self.owner) && name.eq_ignore_ascii_case(&self.name)
        })
    }
}

/// Whether `url` is exactly `https://github.com/<owner>/<name>/issues/<n>`,
/// of any repository: what `gh issue create` prints for a repository that
/// was renamed or moved is its name now.
pub fn is_github_issue_url(url: &str) -> bool {
    issue_url_parts(url).is_some()
}

/// `(owner, name)` of `https://github.com/<owner>/<name>/issues/<n>`: names
/// of the characters GitHub allows (ASCII letters and digits, `_`, `.`,
/// `-`; never `.` or `..`), a number of 1 to 20 digits, nothing after it.
fn issue_url_parts(url: &str) -> Option<(&str, &str)> {
    let path = url.strip_prefix("https://github.com/")?;
    let parts: Vec<&str> = path.split('/').collect();
    match parts.as_slice() {
        [owner, name, "issues", number]
            if plain_name(owner)
                && plain_name(name)
                && (1..=20).contains(&number.len())
                && number.bytes().all(|b| b.is_ascii_digit()) =>
        {
            Some((*owner, *name))
        }
        _ => None,
    }
}

fn plain_name(name: &str) -> bool {
    (1..=100).contains(&name.len())
        && name != "."
        && name != ".."
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-'))
}

/// How `gh` runs: through the plugin manager (only the feedback plugin's
/// declared tool, never while the plugin is off, without any `SMITHAGENTS_*`
/// variable), in `cwd`, with [`GH_ENV`].
#[derive(Clone, Copy)]
pub struct Gh<'a> {
    pub plugins: &'a PluginManager,
    /// Absolute, and never a repository: the daemon's home.
    pub cwd: &'a Path,
    /// The limit of each call that makes or looks for an issue.
    pub timeout: Duration,
    /// The limit of a quick check (`gh auth status`, `gh repo view`):
    /// [`VISIBILITY_TIMEOUT`].
    pub check_timeout: Duration,
}

impl Gh<'_> {
    /// `gh <args>`; its first line.
    async fn run(
        &self,
        args: &[String],
        timeout: Duration,
    ) -> Result<Option<String>, ManagerError> {
        let run = ToolRun {
            args,
            cwd: self.cwd,
            env: &GH_ENV,
            timeout,
        };
        self.plugins.run_tool(FEEDBACK_PLUGIN_ID, GH, run).await
    }
}

fn args<const N: usize>(fixed: [&str; N]) -> Vec<String> {
    fixed.into_iter().map(str::to_owned).collect()
}

/// `gh` as Submit needs it: the plugin's tool ready (see
/// [`tool_readiness`]) and signed in to github.com, which is `gh auth status
/// --hostname=github.com` succeeding within `gh.check_timeout` (a login on
/// another host does not count either way); otherwise the reason, `not
/// signed in` when that check failed.
pub async fn gh_readiness(gh: &Gh<'_>) -> ToolReadiness {
    let ready = tool_readiness(gh.plugins, GH).await;
    if !ready.ready {
        return ready;
    }
    let check = args(["auth", "status", "--hostname=github.com"]);
    match gh.run(&check, gh.check_timeout).await {
        Ok(_) => ready,
        Err(ManagerError::Tool(ToolRunError::NotFound)) => not_ready(Some(NOT_INSTALLED)),
        Err(ManagerError::Disabled) => not_ready(Some(TURNED_OFF)),
        Err(_) => not_ready(Some(NOT_SIGNED_IN)),
    }
}

/// The sentence of a refusal or a failure because `gh` is not ready for
/// `reason` (one of [`gh_readiness`]'s).
pub fn not_ready_sentence(reason: &str) -> String {
    match reason {
        NOT_INSTALLED => "GitHub CLI (gh) is not installed".to_owned(),
        NOT_SIGNED_IN => "GitHub CLI (gh) is not signed in; run: gh auth login".to_owned(),
        other => format!("GitHub CLI (gh) is not ready: {other}"),
    }
}

/// Whether `repo` is public, by `gh repo view --json=visibility
/// --jq=.visibility -- github.com/<owner>/<name>` within
/// `gh.check_timeout`: `PUBLIC` is `true`, `PRIVATE` and `INTERNAL` are
/// `false`; anything else or a failure is `None`.
pub async fn is_public(gh: &Gh<'_>, repo: &GithubRepo) -> Option<bool> {
    let mut view = args([
        "repo",
        "view",
        "--json=visibility",
        "--jq=.visibility",
        "--",
    ]);
    view.push(repo.full_name());
    match gh.run(&view, gh.check_timeout).await {
        Ok(line) => match line.as_deref() {
            Some("PUBLIC") => Some(true),
            Some("PRIVATE" | "INTERNAL") => Some(false),
            _ => None,
        },
        Err(e) => {
            tracing::warn!(
                step = "visibility",
                code = run_code(&e),
                "gh could not tell whether a repository is public"
            );
            None
        }
    }
}

/// The label an issue of `kind` gets (when the repository has it).
pub fn label_for(kind: FeedbackKind) -> &'static str {
    match kind {
        FeedbackKind::NewFeature | FeedbackKind::Enhancement => "enhancement",
        FeedbackKind::Bug => "bug",
    }
}

/// The issue of submission `id` on `repo` that the signed-in user opened,
/// found by the id the last line of every body Submit sends names: `gh
/// issue list --repo=… --author=@me --state=all --search=<id> in:body
/// --json=url --jq=.[0].url // ""`, within `gh.timeout`. A first line that
/// is an issue of `repo` is that issue; an empty one is none; anything else
/// is `gh_bad_answer` (and nothing may be created then).
pub async fn find_issue(
    gh: &Gh<'_>,
    repo: &GithubRepo,
    id: SubmissionId,
) -> Result<Option<String>, FeedbackFailure> {
    let mut search = args(["issue", "list"]);
    search.push(repo.repo_arg());
    search.extend(args(["--author=@me", "--state=all"]));
    search.push(format!("--search={id} in:body"));
    search.extend(args(["--json=url", r#"--jq=.[0].url // """#]));
    match gh.run(&search, gh.timeout).await {
        Ok(None) => Ok(None),
        Ok(Some(url)) if repo.is_issue_url(&url) => Ok(Some(url)),
        Ok(Some(_)) => Err(bad_answer()),
        Err(e) => Err(issue_failure(e).failure),
    }
}

/// Why [`create_issue`] gave no issue.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotCreated {
    pub failure: FeedbackFailure,
    /// `gh issue create` ran and may have made the issue all the same: it
    /// was stopped (out of time, or by a signal) or printed no issue's
    /// address.
    pub unconfirmed: bool,
}

/// `gh issue create --title=<title> --body-file=<body_file> --repo=…
/// [--label=<label>]` within `gh.timeout`, the label only when `repo` has it
/// (a failed check means none). Its first line must be the address of an
/// issue on github.com, of `repo` or (renamed or moved) of the name it has
/// now ([`is_github_issue_url`]).
pub async fn create_issue(
    gh: &Gh<'_>,
    repo: &GithubRepo,
    title: &str,
    body_file: &Path,
    label: &str,
) -> Result<String, NotCreated> {
    let body_file = body_file_arg(body_file).map_err(|failure| NotCreated {
        failure,
        unconfirmed: false,
    })?;
    let repo_arg = repo.repo_arg();
    let labelled = has_label(gh, &repo_arg, label).await;
    let mut create = args(["issue", "create"]);
    create.push(format!("--title={title}"));
    create.push(body_file);
    create.push(repo_arg);
    if labelled {
        create.push(format!("--label={label}"));
    }
    match gh.run(&create, gh.timeout).await {
        Ok(Some(url)) if is_github_issue_url(&url) => Ok(url),
        Ok(_) => Err(NotCreated {
            failure: bad_answer(),
            unconfirmed: true,
        }),
        Err(e) => Err(issue_failure(e)),
    }
}

/// `--body-file=<path>`; a path that is not UTF-8 cannot be passed, which
/// is the daemon's own failure ([`body_failure`]).
fn body_file_arg(path: &Path) -> Result<String, FeedbackFailure> {
    path.to_str()
        .map(|path| format!("--body-file={path}"))
        .ok_or_else(body_failure)
}

/// The issue's body could not be written or named: `io_error`.
pub fn body_failure() -> FeedbackFailure {
    failure("io_error", "could not write the issue's body".to_owned())
}

/// Whether the repository has the label `label` (lower-case letters, digits
/// and `-`, as [`label_for`] gives): `gh label list --search=<label>
/// --json=name --jq=any(.[]; .name == "<label>")` printing `true`.
async fn has_label(gh: &Gh<'_>, repo_arg: &str, label: &str) -> bool {
    let plain = !label.is_empty()
        && label
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
    if !plain {
        return false;
    }
    let check = vec![
        "label".to_owned(),
        "list".to_owned(),
        format!("--search={label}"),
        "--json=name".to_owned(),
        format!(r#"--jq=any(.[]; .name == "{label}")"#),
        repo_arg.to_owned(),
    ];
    match gh.run(&check, gh.timeout).await {
        Ok(line) => line.as_deref() == Some("true"),
        Err(e) => {
            tracing::warn!(
                step = "create_issue",
                code = run_code(&e),
                "gh could not check the issue's label; the issue gets none"
            );
            false
        }
    }
}

fn failure(code: &str, message: String) -> FeedbackFailure {
    FeedbackFailure {
        step: FeedbackStep::CreateIssue,
        code: code.to_owned(),
        message,
    }
}

fn failed(status: Option<i32>) -> FeedbackFailure {
    failure(
        "gh_failed",
        format!(
            "gh could not create the issue ({})",
            exit_status_text(&status)
        ),
    )
}

fn bad_answer() -> FeedbackFailure {
    failure(
        "gh_bad_answer",
        "gh did not print the issue's address".to_owned(),
    )
}

/// The failure of the step `create_issue` for a `gh` run that failed, and
/// whether `gh` may have done its work all the same (stopped while it ran).
fn issue_failure(e: ManagerError) -> NotCreated {
    let (failure, unconfirmed) = match e {
        ManagerError::Tool(ToolRunError::Timeout) => (
            failure(
                "gh_timeout",
                format!("gh did not finish within {} seconds", GH_TIMEOUT.as_secs()),
            ),
            true,
        ),
        // Ended by a signal.
        ManagerError::Tool(ToolRunError::Failed { status: None }) => (failed(None), true),
        ManagerError::Tool(ToolRunError::Failed { status }) => (failed(status), false),
        ManagerError::Tool(ToolRunError::TooMuchOutput) => (bad_answer(), true),
        ManagerError::Tool(ToolRunError::NotFound) => (
            failure("gh_not_ready", not_ready_sentence(NOT_INSTALLED)),
            false,
        ),
        ManagerError::Disabled => (
            failure("gh_not_ready", not_ready_sentence(TURNED_OFF)),
            false,
        ),
        e => {
            tracing::error!(step = "create_issue", "gh could not be run: {e}");
            (failed(None), false)
        }
    };
    NotCreated {
        failure,
        unconfirmed,
    }
}

/// A code for a `gh` run that failed, for a log.
fn run_code(e: &ManagerError) -> &'static str {
    match e {
        ManagerError::Tool(ToolRunError::Timeout) => "gh_timeout",
        ManagerError::Tool(ToolRunError::NotFound) | ManagerError::Disabled => "gh_not_ready",
        ManagerError::Tool(ToolRunError::TooMuchOutput) => "gh_bad_answer",
        _ => "gh_failed",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn widgets() -> GithubRepo {
        GithubRepo::from_remote("https://github.com/smithagents-test-invalid/widgets.git").unwrap()
    }

    #[test]
    fn only_an_issue_of_the_same_repository_is_an_issue_url() {
        let repo = widgets();
        assert_eq!(
            repo.full_name(),
            "github.com/smithagents-test-invalid/widgets"
        );
        for good in [
            "https://github.com/smithagents-test-invalid/widgets/issues/7",
            "https://github.com/Smithagents-Test-Invalid/WIDGETS/issues/123456",
        ] {
            assert!(repo.is_issue_url(good), "{good}");
        }
        for bad in [
            "",
            "https://example.com/x",
            "http://github.com/smithagents-test-invalid/widgets/issues/7",
            "https://github.com.evil.example/smithagents-test-invalid/widgets/issues/7",
            "https://github.com/other/widgets/issues/7",
            "https://github.com/smithagents-test-invalid/gadgets/issues/7",
            "https://github.com/smithagents-test-invalid/widgets/pull/7",
            "https://github.com/smithagents-test-invalid/widgets/issues/",
            "https://github.com/smithagents-test-invalid/widgets/issues/7x",
            "https://github.com/smithagents-test-invalid/widgets/issues/-7",
            "https://github.com/smithagents-test-invalid/widgets/issues/7/x",
            "https://github.com/smithagents-test-invalid/widgets/issues/7?x=1",
            "https://github.com/smithagents-test-invalid/widgets/issues/123456789012345678901",
        ] {
            assert!(!repo.is_issue_url(bad), "{bad}");
        }
        assert_eq!(GithubRepo::from_remote("https://gitlab.com/o/r.git"), None);
    }

    #[test]
    fn labels_follow_the_kind() {
        assert_eq!(label_for(FeedbackKind::NewFeature), "enhancement");
        assert_eq!(label_for(FeedbackKind::Enhancement), "enhancement");
        assert_eq!(label_for(FeedbackKind::Bug), "bug");
    }

    #[test]
    fn failures_are_fixed_sentences() {
        // The error, its code and sentence, and whether gh may have made the
        // issue all the same.
        let cases = [
            (
                ManagerError::Tool(ToolRunError::Failed { status: Some(1) }),
                "gh_failed",
                "gh could not create the issue (exit status 1)",
                false,
            ),
            (
                ManagerError::Tool(ToolRunError::Failed { status: None }),
                "gh_failed",
                "gh could not create the issue (no exit status)",
                true,
            ),
            (
                ManagerError::Tool(ToolRunError::Timeout),
                "gh_timeout",
                "gh did not finish within 60 seconds",
                true,
            ),
            (
                ManagerError::Tool(ToolRunError::TooMuchOutput),
                "gh_bad_answer",
                "gh did not print the issue's address",
                true,
            ),
            (
                ManagerError::Tool(ToolRunError::NotFound),
                "gh_not_ready",
                "GitHub CLI (gh) is not installed",
                false,
            ),
            (
                ManagerError::Disabled,
                "gh_not_ready",
                "GitHub CLI (gh) is not ready: turned off or not running",
                false,
            ),
            (
                ManagerError::Tool(ToolRunError::NotStarted),
                "gh_failed",
                "gh could not create the issue (no exit status)",
                false,
            ),
        ];
        for (error, code, message, unconfirmed) in cases {
            let NotCreated {
                failure: f,
                unconfirmed: maybe,
            } = issue_failure(error);
            assert_eq!(f.step, FeedbackStep::CreateIssue);
            assert_eq!((f.code.as_str(), f.message.as_str()), (code, message));
            assert_eq!(maybe, unconfirmed, "{message}");
        }
        assert_eq!(
            not_ready_sentence(NOT_SIGNED_IN),
            "GitHub CLI (gh) is not signed in; run: gh auth login"
        );
    }

    #[test]
    fn an_issue_of_any_repository_is_an_issue_url_for_create() {
        for good in [
            "https://github.com/smithagents-test-invalid/widgets/issues/7",
            "https://github.com/someone-else/widgets/issues/7",
            "https://github.com/o/gadgets.rs/issues/1",
            "https://github.com/o_o/r-r/issues/12345678901234567890",
        ] {
            assert!(is_github_issue_url(good), "{good}");
        }
        for bad in [
            "",
            "https://example.com/o/r/issues/1",
            "http://github.com/o/r/issues/1",
            "https://github.com.evil.example/o/r/issues/1",
            "https://github.com/../r/issues/1",
            "https://github.com/o/./issues/1",
            "https://github.com/o r/r/issues/1",
            "https://github.com/o/r%2F/issues/1",
            "https://github.com//r/issues/1",
            "https://github.com/o/r/pull/1",
            "https://github.com/o/r/issues/1/x",
            "https://github.com/o/r/issues/1#c",
        ] {
            assert!(!is_github_issue_url(bad), "{bad}");
        }
    }

    #[test]
    fn a_body_path_that_is_not_utf8_is_the_daemons_failure() {
        use std::ffi::OsStr;
        use std::os::unix::ffi::OsStrExt;
        let path = Path::new(OsStr::from_bytes(b"/tmp/feedback/\xff/body.md"));
        let f = body_file_arg(path).unwrap_err();
        assert_eq!(f.step, FeedbackStep::CreateIssue);
        assert_eq!(
            (f.code.as_str(), f.message.as_str()),
            ("io_error", "could not write the issue's body")
        );
        assert_eq!(
            body_file_arg(Path::new("/tmp/feedback/x/body.md")).unwrap(),
            "--body-file=/tmp/feedback/x/body.md"
        );
    }
}
