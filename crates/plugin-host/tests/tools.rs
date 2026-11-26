//! Command-line tools: finding them on an injected search path, checking
//! their version, installing them through a fake `mise`, and using one as a
//! secret source. Every executable here is a shell script the test writes
//! into a temporary directory; nothing installed on the machine is found or
//! run. Each test that sets an environment variable uses a name no other test
//! reads or writes.
#![cfg(unix)]

use std::collections::HashMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use smithagents_plugin_host::{
    ConfiguredSecrets, InstallError, MissingReason, Secret, SecretError, SecretSource,
    StaticSecrets, ToolFinder, ToolRun, ToolRunError, ToolSecrets, ToolState, MISE_INSTALL_URL,
};
use smithagents_plugin_protocol::{PluginId, ToolDecl, ToolSource};
use tempfile::TempDir;

fn set_mode(path: &Path, mode: u32) {
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
}

/// Writes an executable shell script `name` into `dir`.
fn script(dir: &Path, name: &str, body: &str) -> PathBuf {
    let path = dir.join(name);
    fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
    set_mode(&path, 0o755);
    path
}

/// A fake `gh` that appends its arguments to `<dir>/gh.log` and prints `out`.
fn fake_gh(dir: &Path, out: &str) -> PathBuf {
    let log = dir.join("gh.log");
    script(
        dir,
        "gh",
        &format!("echo \"$*\" >> '{}'\nprintf '{out}'", log.display()),
    )
}

/// Every invocation the fake `gh` logged, one argument string per run.
fn gh_runs(dir: &Path) -> Vec<String> {
    fs::read_to_string(dir.join("gh.log"))
        .map(|s| s.lines().map(str::to_owned).collect())
        .unwrap_or_default()
}

fn decl(name: &str, min_version: Option<&str>, install: &[(&str, &str)]) -> ToolDecl {
    ToolDecl {
        name: name.into(),
        label: "GitHub CLI".into(),
        min_version: min_version.map(|v| v.parse().unwrap()),
        install_url: Some("https://cli.github.com".into()),
        install: install
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect(),
    }
}

fn finder(dir: &TempDir) -> ToolFinder {
    ToolFinder::new(vec![dir.path().to_path_buf()])
}

// ---------------------------------------------------------------- finding

#[test]
fn a_tool_is_found_only_in_the_listed_directories_first_match_first() {
    let (a, b) = (TempDir::new().unwrap(), TempDir::new().unwrap());
    let in_b = script(b.path(), "gh", "exit 0");

    let both = ToolFinder::new(vec![a.path().into(), b.path().into()]);
    assert_eq!(both.find("gh"), Some(in_b));
    assert_eq!(finder(&a).find("gh"), None);

    let in_a = script(a.path(), "gh", "exit 0");
    assert_eq!(both.find("gh"), Some(in_a));
}

#[test]
fn a_name_that_is_not_a_bare_executable_name_is_never_resolved() {
    let dir = TempDir::new().unwrap();
    fs::create_dir(dir.path().join("sub")).unwrap();
    script(&dir.path().join("sub"), "gh", "exit 0");
    script(dir.path(), "GH", "exit 0");
    script(dir.path(), "gh.exe", "exit 0");
    let f = finder(&dir);
    for name in ["sub/gh", "../gh", "/bin/sh", "", "GH", "gh.exe", ".", ".."] {
        assert_eq!(f.find(name), None, "{name:?}");
    }
}

#[test]
fn only_a_regular_executable_file_not_writable_by_others_counts() {
    let dir = TempDir::new().unwrap();
    let f = finder(&dir);

    let plain = dir.path().join("plain");
    fs::write(&plain, "#!/bin/sh\n").unwrap();
    set_mode(&plain, 0o644);
    assert_eq!(f.find("plain"), None, "not executable");

    fs::create_dir(dir.path().join("folder")).unwrap();
    assert_eq!(f.find("folder"), None, "a directory");

    let open = script(dir.path(), "open", "exit 0");
    set_mode(&open, 0o777);
    assert_eq!(f.find("open"), None, "writable by others");

    std::os::unix::fs::symlink(dir.path().join("nowhere"), dir.path().join("dangling")).unwrap();
    assert_eq!(f.find("dangling"), None, "a broken link");

    // A link to an executable is found under the link's own name: shims
    // such as mise's decide what to run from the name they were called by.
    let target = script(dir.path(), "real", "exit 0");
    let link = dir.path().join("shim");
    std::os::unix::fs::symlink(&target, &link).unwrap();
    assert_eq!(f.find("shim"), Some(link));
}

#[test]
fn relative_directories_are_never_searched() {
    let dir = TempDir::new().unwrap();
    let f = ToolFinder::new(vec![
        PathBuf::from("."),
        PathBuf::from(""),
        PathBuf::from("bin"),
        dir.path().into(),
    ]);
    assert_eq!(f.dirs(), [dir.path().to_path_buf()]);
}

// ---------------------------------------------------------------- checking

#[tokio::test]
async fn a_missing_tool_is_missing() {
    let dir = TempDir::new().unwrap();
    assert_eq!(
        finder(&dir).check(&decl("gh", Some("2.40.0"), &[])).await,
        ToolState::Missing {
            reason: MissingReason::NotFound
        }
    );
}

#[tokio::test]
async fn without_a_min_version_a_found_tool_is_ready_and_is_not_run() {
    let dir = TempDir::new().unwrap();
    let path = fake_gh(dir.path(), "gh version 2.41.0\\n");
    assert_eq!(
        finder(&dir).check(&decl("gh", None, &[])).await,
        ToolState::Ready {
            path,
            version: None
        }
    );
    assert_eq!(gh_runs(dir.path()), Vec::<String>::new());
}

#[tokio::test]
async fn the_first_semver_of_the_version_line_is_compared() {
    let dir = TempDir::new().unwrap();
    let path = fake_gh(
        dir.path(),
        "gh version 2.41.0 (2024-01-09)\\nhttps://github.com/cli/cli/releases/tag/v9.9.9\\n",
    );
    assert_eq!(
        finder(&dir).check(&decl("gh", Some("2.40.0"), &[])).await,
        ToolState::Ready {
            path,
            version: Some(semver::Version::new(2, 41, 0))
        }
    );
    assert_eq!(gh_runs(dir.path()), ["--version"]);
}

#[tokio::test]
async fn a_leading_v_is_read_and_an_equal_version_is_enough() {
    let dir = TempDir::new().unwrap();
    fake_gh(dir.path(), "tool v2.40.0, built today\\n");
    assert!(matches!(
        finder(&dir).check(&decl("gh", Some("2.40.0"), &[])).await,
        ToolState::Ready { version: Some(v), .. } if v == semver::Version::new(2, 40, 0)
    ));
}

#[tokio::test]
async fn an_older_version_is_too_old() {
    let dir = TempDir::new().unwrap();
    fake_gh(dir.path(), "gh version 2.30.1 (2023-05-01)\\n");
    assert_eq!(
        finder(&dir).check(&decl("gh", Some("2.40.0"), &[])).await,
        ToolState::TooOld {
            found: semver::Version::new(2, 30, 1)
        }
    );
}

#[tokio::test]
async fn a_version_that_cannot_be_read_is_missing_only_when_a_minimum_is_set() {
    let dir = TempDir::new().unwrap();
    let path = fake_gh(dir.path(), "gh development build\\n");
    let f = finder(&dir);
    assert_eq!(
        f.check(&decl("gh", Some("2.40.0"), &[])).await,
        ToolState::Missing {
            reason: MissingReason::UnreadableVersion
        }
    );
    assert_eq!(
        f.check(&decl("gh", None, &[])).await,
        ToolState::Ready {
            path,
            version: None
        }
    );

    // Neither is a version the tool failed to print.
    script(dir.path(), "gh", "echo 'gh version 2.41.0'; exit 1");
    assert_eq!(
        f.check(&decl("gh", Some("2.40.0"), &[])).await,
        ToolState::Missing {
            reason: MissingReason::UnreadableVersion
        }
    );
}

// ---------------------------------------------------------------- installing

/// A fake `mise` that logs its arguments, one per line, to `<dir>/mise.log`
/// and then runs `then`.
fn fake_mise(dir: &Path, then: &str) {
    let log = dir.join("mise.log");
    script(
        dir,
        "mise",
        &format!(
            "for a in \"$@\"; do echo \"$a\" >> '{}'; done\n{then}",
            log.display()
        ),
    );
}

fn mise_args(dir: &Path) -> Option<String> {
    fs::read_to_string(dir.join("mise.log")).ok()
}

/// What the fake `mise` runs to "install" the fake `gh`.
fn creates_gh(dir: &Path) -> String {
    let gh = dir.join("gh");
    format!(
        "printf '#!/bin/sh\\necho gh version 2.41.0\\n' > '{0}'; chmod 755 '{0}'",
        gh.display()
    )
}

#[tokio::test]
async fn install_runs_mise_use_global_with_the_spec_then_checks_again() {
    let dir = TempDir::new().unwrap();
    fake_mise(dir.path(), &creates_gh(dir.path()));

    let state = finder(&dir)
        .install(&decl(
            "gh",
            Some("2.40.0"),
            &[("mise", "gh"), ("brew", "gh")],
        ))
        .await
        .unwrap();

    assert_eq!(
        mise_args(dir.path()).as_deref(),
        Some("use\n--global\ngh\n")
    );
    assert_eq!(
        state,
        ToolState::Ready {
            path: dir.path().join("gh"),
            version: Some(semver::Version::new(2, 41, 0))
        }
    );
}

#[tokio::test]
async fn the_spec_reaches_mise_as_one_argument() {
    let dir = TempDir::new().unwrap();
    fake_mise(dir.path(), "exit 0");
    let state = finder(&dir)
        .install(&decl("gh", None, &[("mise", "aqua:cli/cli@2.41.0")]))
        .await
        .unwrap();
    assert_eq!(
        mise_args(dir.path()).as_deref(),
        Some("use\n--global\naqua:cli/cli@2.41.0\n")
    );
    // mise succeeded but put nothing on the search path.
    assert_eq!(
        state,
        ToolState::Missing {
            reason: MissingReason::NotFound
        }
    );
}

#[tokio::test]
async fn without_a_valid_mise_spec_nothing_runs() {
    let dir = TempDir::new().unwrap();
    fake_mise(dir.path(), "exit 0");
    let f = finder(&dir);
    for install in [
        vec![("brew", "gh")],
        vec![],
        vec![("mise", "-g")],
        vec![("mise", "")],
    ] {
        assert_eq!(
            f.install(&decl("gh", None, &install)).await,
            Err(InstallError::NoMiseSpec),
            "{install:?}"
        );
    }
    assert_eq!(mise_args(dir.path()), None, "mise ran");
}

#[tokio::test]
async fn without_mise_the_error_names_its_install_page() {
    let dir = TempDir::new().unwrap();
    let err = finder(&dir)
        .install(&decl("gh", None, &[("mise", "gh")]))
        .await
        .unwrap_err();
    assert_eq!(
        err,
        InstallError::MiseMissing {
            install_url: "https://mise.jdx.dev".into()
        }
    );
    assert_eq!(MISE_INSTALL_URL, "https://mise.jdx.dev");
}

#[tokio::test]
async fn a_failing_install_reports_its_exit_status_and_nothing_it_printed() {
    let dir = TempDir::new().unwrap();
    fake_mise(dir.path(), "echo out-5ecret; echo err-5ecret >&2; exit 3");
    let err = finder(&dir)
        .install(&decl("gh", None, &[("mise", "gh")]))
        .await
        .unwrap_err();
    assert_eq!(err, InstallError::Failed { status: Some(3) });
    for text in [format!("{err:?}"), err.to_string()] {
        assert!(!text.contains("5ecret"), "{text}");
    }
    assert!(err.to_string().contains('3'), "{err}");
}

#[tokio::test(start_paused = true)]
async fn an_install_that_runs_for_ten_minutes_is_stopped() {
    let dir = TempDir::new().unwrap();
    // `exec`, so the process that is killed is the one sleeping.
    fake_mise(dir.path(), "exec sleep 600");
    let started = std::time::Instant::now();
    let virtual_start = tokio::time::Instant::now();

    let err = finder(&dir)
        .install(&decl("gh", None, &[("mise", "gh")]))
        .await
        .unwrap_err();

    assert_eq!(err, InstallError::Timeout);
    // The paused clock moved the ten minutes; the test did not wait them.
    assert!(virtual_start.elapsed() >= Duration::from_secs(600));
    assert!(
        started.elapsed() < Duration::from_secs(60),
        "{:?}",
        started.elapsed()
    );
}

// ---------------------------------------------------------------- secrets

fn id(s: &str) -> PluginId {
    s.parse().unwrap()
}

fn gh_token() -> ToolSource {
    ToolSource {
        tool: "gh".into(),
        args: vec!["auth".into(), "token".into()],
    }
}

fn tool_secrets(dir: &TempDir, inner: Arc<dyn SecretSource>, plugin: &str) -> ToolSecrets {
    let mut secrets = ToolSecrets::new(inner, finder(dir));
    secrets.register(&id(plugin), "token", gh_token());
    secrets
}

fn nothing() -> Arc<dyn SecretSource> {
    Arc::new(StaticSecrets(HashMap::new()))
}

async fn value(source: &dyn SecretSource, plugin: &str, key: &str) -> Option<String> {
    source
        .get(&id(plugin), key)
        .await
        .unwrap()
        .map(|s| s.expose().to_string())
}

#[tokio::test]
async fn a_registered_tool_supplies_the_secret_from_its_first_line() {
    let dir = TempDir::new().unwrap();
    fake_gh(dir.path(), "  tool-5ecret  \\nsecond line\\n");
    let secrets = tool_secrets(&dir, nothing(), "tool-plug");

    assert_eq!(
        value(&secrets, "tool-plug", "token").await.as_deref(),
        Some("tool-5ecret")
    );
    assert_eq!(gh_runs(dir.path()), ["auth token"]);
}

#[tokio::test]
async fn the_tool_value_is_cached() {
    let dir = TempDir::new().unwrap();
    fake_gh(dir.path(), "tool-5ecret\\n");
    let secrets = tool_secrets(&dir, nothing(), "cache-tool");
    for _ in 0..3 {
        assert_eq!(
            value(&secrets, "cache-tool", "token").await.as_deref(),
            Some("tool-5ecret")
        );
    }
    assert_eq!(gh_runs(dir.path()).len(), 1);
}

#[tokio::test]
async fn configured_sources_win_over_the_tool() {
    let dir = TempDir::new().unwrap();
    fake_gh(dir.path(), "tool-5ecret\\n");

    // Any inner source: here fixed values.
    let fixed: Arc<dyn SecretSource> = Arc::new(StaticSecrets(
        [(("fixed-tool".into(), "token".into()), "fixed-5ecret".into())]
            .into_iter()
            .collect(),
    ));
    let secrets = tool_secrets(&dir, fixed, "fixed-tool");
    assert_eq!(
        value(&secrets, "fixed-tool", "token").await.as_deref(),
        Some("fixed-5ecret")
    );

    // secrets.toml.
    std::env::set_var("SMITHAGENTS_TEST_TOOL_FILE_ENTRY_9W2", "file-5ecret");
    let home = TempDir::new().unwrap();
    let path = home.path().join("secrets.toml");
    fs::write(
        &path,
        "[file-tool]\ntoken = { env = \"SMITHAGENTS_TEST_TOOL_FILE_ENTRY_9W2\" }\n",
    )
    .unwrap();
    set_mode(&path, 0o600);
    let configured: Arc<dyn SecretSource> = Arc::new(ConfiguredSecrets::load(home.path()).unwrap());
    let secrets = tool_secrets(&dir, configured, "file-tool");
    assert_eq!(
        value(&secrets, "file-tool", "token").await.as_deref(),
        Some("file-5ecret")
    );

    // The environment fallback.
    std::env::set_var("SMITHAGENTS_SECRET_ENV_TOOL__TOKEN", "env-5ecret");
    let secrets = tool_secrets(&dir, Arc::new(ConfiguredSecrets::default()), "env-tool");
    assert_eq!(
        value(&secrets, "env-tool", "token").await.as_deref(),
        Some("env-5ecret")
    );

    assert_eq!(gh_runs(dir.path()), Vec::<String>::new(), "the tool ran");
}

#[tokio::test]
async fn only_a_registered_plugin_and_key_run_the_tool() {
    let dir = TempDir::new().unwrap();
    fake_gh(dir.path(), "tool-5ecret\\n");
    let secrets = tool_secrets(&dir, nothing(), "reg-tool");

    assert_eq!(value(&secrets, "other-tool", "token").await, None);
    assert_eq!(value(&secrets, "reg-tool", "other").await, None);
    assert_eq!(gh_runs(dir.path()), Vec::<String>::new());
}

#[tokio::test]
async fn a_tool_that_is_not_found_provides_nothing() {
    let dir = TempDir::new().unwrap();
    let secrets = tool_secrets(&dir, nothing(), "gone-tool");
    assert_eq!(value(&secrets, "gone-tool", "token").await, None);
}

#[tokio::test]
async fn a_failing_tool_is_an_error_that_carries_none_of_its_output() {
    let dir = TempDir::new().unwrap();
    script(
        dir.path(),
        "gh",
        "echo out-5ecret; echo err-5ecret >&2; exit 4",
    );
    let secrets = tool_secrets(&dir, nothing(), "fail-tool");

    let err = secrets.get(&id("fail-tool"), "token").await.unwrap_err();

    assert!(matches!(err, SecretError::Command { .. }), "{err:?}");
    for text in [format!("{err:?}"), err.to_string()] {
        assert!(!text.contains("5ecret"), "{text}");
    }

    // Printing nothing is no secret either.
    script(dir.path(), "gh", "exit 0");
    let secrets = tool_secrets(&dir, nothing(), "empty-tool");
    assert_eq!(value(&secrets, "empty-tool", "token").await, None);
}

/// A source that cannot resolve anything.
struct Broken;

#[async_trait]
impl SecretSource for Broken {
    async fn get(&self, _: &PluginId, _: &str) -> Result<Option<Secret>, SecretError> {
        Err(SecretError::NotUnicode {
            name: "SOMETHING".into(),
        })
    }
}

#[tokio::test]
async fn an_error_of_the_configured_sources_is_not_papered_over_by_the_tool() {
    let dir = TempDir::new().unwrap();
    fake_gh(dir.path(), "tool-5ecret\\n");
    let secrets = tool_secrets(&dir, Arc::new(Broken), "broken-tool");

    let err = secrets.get(&id("broken-tool"), "token").await.unwrap_err();

    assert!(matches!(err, SecretError::NotUnicode { .. }), "{err:?}");
    assert_eq!(gh_runs(dir.path()), Vec::<String>::new());
}

// ---------------------------------------------------------------- fix round 1

/// Whether `pid` is still a live process (a zombie counts as gone).
fn alive(pid: &str) -> bool {
    let out = std::process::Command::new("ps")
        .args(["-o", "stat=", "-p", pid.trim()])
        .output()
        .unwrap();
    let stat = String::from_utf8_lossy(&out.stdout);
    let stat = stat.trim();
    !stat.is_empty() && !stat.starts_with('Z')
}

/// Waits on a blocking thread (so a paused clock stays still) until every
/// file in `paths` exists.
async fn files_appear(paths: Vec<PathBuf>) {
    let found = tokio::task::spawn_blocking(move || {
        for _ in 0..2000 {
            if paths.iter().all(|p| p.exists()) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        false
    })
    .await
    .unwrap();
    assert!(found, "gave up waiting for the files");
}

/// The variable names in an `env` dump.
fn env_names(path: &Path) -> Vec<String> {
    fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("{}: {e}", path.display()))
        .lines()
        .filter_map(|l| l.split_once('=').map(|(k, _)| k.to_string()))
        .collect()
}

// J1: no SMITHAGENTS_* variable reaches a program the daemon runs.
#[tokio::test]
async fn no_smithagents_variable_reaches_a_tool_or_mise() {
    std::env::set_var("SMITHAGENTS_SECRET_ENV_LEAK__TOKEN", "leak-5ecret");
    std::env::set_var("SMITHAGENTS_TEST_OTHER_J1", "other-5ecret");
    let dir = TempDir::new().unwrap();
    let d = dir.path().display();
    script(
        dir.path(),
        "gh",
        &format!(
            "case \"$1\" in\n\
             --version) env > '{d}/gh-version.env'; echo gh version 2.41.0 ;;\n\
             auth) env > '{d}/gh-auth.env'; echo tool-token ;;\n\
             esac"
        ),
    );
    fake_mise(dir.path(), &format!("env > '{d}/mise.env'"));
    let f = finder(&dir);

    f.check(&decl("gh", Some("2.40.0"), &[])).await;
    let secrets = tool_secrets(&dir, nothing(), "env-leak");
    assert_eq!(
        value(&secrets, "env-leak", "token").await.as_deref(),
        Some("tool-token")
    );
    f.install(&decl("gh", None, &[("mise", "gh")]))
        .await
        .unwrap();

    for file in ["gh-version.env", "gh-auth.env", "mise.env"] {
        let names = env_names(&dir.path().join(file));
        assert!(names.iter().any(|n| n == "HOME"), "{file}: no HOME");
        assert!(names.iter().any(|n| n == "PATH"), "{file}: no PATH");
        let leaked: Vec<_> = names
            .iter()
            .filter(|n| n.starts_with("SMITHAGENTS_"))
            .collect();
        assert!(leaked.is_empty(), "{file}: {leaked:?}");
    }
}

// J3: search directories and executables others could have written.
#[test]
fn a_directory_others_can_write_is_not_searched() {
    let root = TempDir::new().unwrap();
    for (name, mode) in [("open", 0o777), ("sticky", 0o1777)] {
        let dir = root.path().join(name);
        fs::create_dir(&dir).unwrap();
        script(&dir, "gh", "exit 0");
        set_mode(&dir, mode);
        assert_eq!(
            ToolFinder::new(vec![dir.clone()]).find("gh"),
            None,
            "{mode:o}"
        );
    }
    // Group-writable is allowed for a directory the current user owns.
    let mine = root.path().join("mine");
    fs::create_dir(&mine).unwrap();
    let gh = script(&mine, "gh", "exit 0");
    set_mode(&mine, 0o775);
    assert_eq!(ToolFinder::new(vec![mine]).find("gh"), Some(gh));
}

#[test]
fn an_executable_its_group_can_write_is_refused() {
    let dir = TempDir::new().unwrap();
    let gh = script(dir.path(), "gh", "exit 0");
    set_mode(&gh, 0o775);
    assert_eq!(finder(&dir).find("gh"), None);
    set_mode(&gh, 0o755);
    assert_eq!(finder(&dir).find("gh"), Some(gh));
}

// J4: a timeout kills the command's whole process group.
#[tokio::test(start_paused = true)]
async fn a_timed_out_install_leaves_no_process_behind() {
    let dir = TempDir::new().unwrap();
    let (child, me) = (dir.path().join("child.pid"), dir.path().join("mise.pid"));
    fake_mise(
        dir.path(),
        &format!(
            "sleep 600 &\necho $! > '{}'\necho $$ > '{}'\nwait",
            child.display(),
            me.display()
        ),
    );
    let install = {
        let f = finder(&dir);
        tokio::spawn(async move { f.install(&decl("gh", None, &[("mise", "gh")])).await })
    };
    files_appear(vec![child.clone(), me.clone()]).await;
    let (child, me) = (
        fs::read_to_string(child).unwrap(),
        fs::read_to_string(me).unwrap(),
    );
    assert!(alive(&child) && alive(&me));

    assert_eq!(install.await.unwrap(), Err(InstallError::Timeout));

    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while alive(&child) || alive(&me) {
        assert!(
            std::time::Instant::now() < deadline,
            "left running: mise {me} {}, its child {child} {}",
            alive(&me),
            alive(&child)
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

// J4: at most 64 KiB of output is read.
#[tokio::test]
async fn more_than_64_kib_of_output_is_refused() {
    let dir = TempDir::new().unwrap();
    // 70 000 bytes: "gh version 2.41.0" then filler on the same line.
    script(
        dir.path(),
        "gh",
        "printf 'gh version 2.41.0 '; head -c 70000 /dev/zero | tr '\\0' 'x'; echo",
    );
    assert_eq!(
        finder(&dir).check(&decl("gh", Some("2.40.0"), &[])).await,
        ToolState::Missing {
            reason: MissingReason::UnreadableVersion
        }
    );
    let secrets = tool_secrets(&dir, nothing(), "big-tool");
    let err = secrets.get(&id("big-tool"), "token").await.unwrap_err();
    assert!(matches!(err, SecretError::TooMuchOutput { .. }), "{err:?}");
    assert!(!format!("{err:?} {err}").contains("xxxx"), "{err}");

    // Exactly 64 KiB is fine.
    script(
        dir.path(),
        "gh",
        "printf 'gh version 2.41.0 '; head -c 65517 /dev/zero | tr '\\0' 'x'; echo",
    );
    assert!(matches!(
        finder(&dir).check(&decl("gh", Some("2.40.0"), &[])).await,
        ToolState::Ready { .. }
    ));
}

// J6: an explicit secrets.toml entry decides alone.
#[tokio::test]
async fn an_explicit_entry_that_resolves_to_nothing_is_missing_without_env_or_tool() {
    std::env::set_var("SMITHAGENTS_SECRET_EXPLICIT_TOOL__TOKEN", "env-5ecret");
    let dir = TempDir::new().unwrap();
    fake_gh(dir.path(), "tool-5ecret\\n");
    let home = TempDir::new().unwrap();
    let path = home.path().join("secrets.toml");
    fs::write(
        &path,
        "[explicit-tool]\ntoken = { env = \"SMITHAGENTS_TEST_NEVER_SET_J6\" }\n\
         [empty-tool]\ntoken = { command = [\"true\"] }\n",
    )
    .unwrap();
    set_mode(&path, 0o600);
    let configured: Arc<dyn SecretSource> = Arc::new(ConfiguredSecrets::load(home.path()).unwrap());
    let mut secrets = ToolSecrets::new(Arc::clone(&configured), finder(&dir));
    secrets.register(&id("explicit-tool"), "token", gh_token());
    secrets.register(&id("empty-tool"), "token", gh_token());

    assert_eq!(value(&secrets, "explicit-tool", "token").await, None);
    assert_eq!(value(&secrets, "empty-tool", "token").await, None);
    assert_eq!(gh_runs(dir.path()), Vec::<String>::new(), "the tool ran");
    assert!(configured.is_explicit(&id("explicit-tool"), "token"));
    assert!(!configured.is_explicit(&id("explicit-tool"), "other"));
}

// J7: a mise that cannot start is logged with the OS error kind.
#[derive(Clone, Default)]
struct Logs(Arc<std::sync::Mutex<Vec<u8>>>);

impl std::io::Write for Logs {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Everything this test binary logs (global: see the server tests).
fn logs() -> &'static Logs {
    static LOGS: std::sync::OnceLock<Logs> = std::sync::OnceLock::new();
    LOGS.get_or_init(|| {
        let logs = Logs::default();
        let sink = logs.clone();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(move || sink.clone())
            .with_ansi(false)
            .with_max_level(tracing::Level::TRACE)
            .finish();
        tracing::subscriber::set_global_default(subscriber).expect("the only global subscriber");
        logs
    })
}

#[tokio::test]
async fn a_mise_that_cannot_start_is_logged_with_its_error_kind() {
    let logs = logs();
    let dir = TempDir::new().unwrap();
    // Executable, but its interpreter does not exist: exec fails.
    let mise = dir.path().join("mise");
    fs::write(&mise, "#!/nonexistent/smithagents-no-shell\n").unwrap();
    set_mode(&mise, 0o755);

    let err = finder(&dir)
        .install(&decl("gh", None, &[("mise", "gh")]))
        .await
        .unwrap_err();

    assert_eq!(err, InstallError::Failed { status: None });
    let text = String::from_utf8_lossy(&logs.0.lock().unwrap()).into_owned();
    assert!(
        text.contains("mise could not be started") && text.contains("kind=NotFound"),
        "{text}"
    );
}

// ---------------------------------------------------------------- running

/// A run of `args` in `cwd` with the given variables and a 10 s timeout.
fn run_in<'a>(args: &'a [String], cwd: &'a Path, env: &'a [(&'a str, &'a str)]) -> ToolRun<'a> {
    ToolRun {
        args,
        cwd,
        env,
        timeout: Duration::from_secs(10),
    }
}

#[tokio::test]
async fn run_returns_the_first_line_and_runs_in_the_given_directory() {
    let (tools, work) = (TempDir::new().unwrap(), TempDir::new().unwrap());
    script(tools.path(), "here", "pwd; echo second");
    let f = finder(&tools);

    let line = f.run("here", run_in(&[], work.path(), &[])).await;

    let want = fs::canonicalize(work.path()).unwrap();
    assert_eq!(line, Ok(Some(want.display().to_string())));

    // Printing nothing is no line.
    script(tools.path(), "quiet", "exit 0");
    assert_eq!(
        f.run("quiet", run_in(&[], work.path(), &[])).await,
        Ok(None)
    );
}

#[tokio::test]
async fn run_passes_each_argument_as_it_is_without_a_shell() {
    let (tools, work) = (TempDir::new().unwrap(), TempDir::new().unwrap());
    script(tools.path(), "args", "printf '%s|' \"$@\"; echo");
    let args: Vec<String> = ["a b", "$HOME", "--title=x; rm -rf ~", "*"]
        .map(String::from)
        .to_vec();

    let line = finder(&tools)
        .run("args", run_in(&args, work.path(), &[]))
        .await;

    assert_eq!(
        line.as_ref().map(|l| l.as_deref()),
        Ok(Some("a b|$HOME|--title=x; rm -rf ~|*|"))
    );
}

// Review Focus 3: a tool the daemon runs never sees a `SMITHAGENTS_*`
// variable, neither the daemon's own nor one a caller passes.
#[tokio::test]
async fn run_strips_smithagents_variables_and_passes_the_given_ones() {
    std::env::set_var("SMITHAGENTS_SECRET_RUN_TEST__X", "LEAK-secret");
    let (tools, work) = (TempDir::new().unwrap(), TempDir::new().unwrap());
    let out = work.path().join("env.txt");
    script(tools.path(), "dump", &format!("env > '{}'", out.display()));
    let given = [
        ("GH_PROMPT_DISABLED", "1"),
        ("SMITHAGENTS_GIVEN_RUN_TEST", "LEAK-given"),
    ];

    finder(&tools)
        .run("dump", run_in(&[], work.path(), &given))
        .await
        .unwrap();

    let text = fs::read_to_string(&out).unwrap();
    assert!(text.lines().any(|l| l == "GH_PROMPT_DISABLED=1"), "{text}");
    assert!(text.lines().any(|l| l.starts_with("PATH=")), "{text}");
    let leaked: Vec<_> = text
        .lines()
        .filter(|l| l.starts_with("SMITHAGENTS_"))
        .collect();
    assert!(leaked.is_empty(), "{leaked:?}");
    assert!(!text.contains("LEAK"), "{text}");
}

#[tokio::test]
async fn run_errors_carry_no_output() {
    let (tools, work) = (TempDir::new().unwrap(), TempDir::new().unwrap());
    script(
        tools.path(),
        "noisy",
        "echo LEAK-out; echo LEAK-err >&2; exit 3",
    );
    let f = finder(&tools);

    let err = f
        .run("noisy", run_in(&[], work.path(), &[]))
        .await
        .unwrap_err();
    assert_eq!(err, ToolRunError::Failed { status: Some(3) });
    for text in [format!("{err:?}"), err.to_string()] {
        assert!(!text.contains("LEAK"), "{text}");
    }
    assert!(err.to_string().contains("exit status 3"), "{err}");

    // Not found, or not a bare tool name: nothing runs.
    for name in ["absent", "../noisy", "/bin/sh", ""] {
        assert_eq!(
            f.run(name, run_in(&[], work.path(), &[])).await,
            Err(ToolRunError::NotFound),
            "{name:?}"
        );
    }

    // A directory that does not exist: the tool cannot start there.
    let gone = work.path().join("gone");
    assert_eq!(
        f.run("noisy", run_in(&[], &gone, &[])).await,
        Err(ToolRunError::NotStarted)
    );

    // More than 64 KiB of output.
    script(
        tools.path(),
        "big",
        "head -c 70000 /dev/zero | tr '\\0' 'x'; echo",
    );
    let err = f
        .run("big", run_in(&[], work.path(), &[]))
        .await
        .unwrap_err();
    assert_eq!(err, ToolRunError::TooMuchOutput);
    assert!(!format!("{err:?} {err}").contains("xxxx"), "{err}");

    // Too slow.
    script(tools.path(), "slow", "sleep 5");
    let started = std::time::Instant::now();
    let slow = ToolRun {
        timeout: Duration::from_millis(100),
        ..run_in(&[], work.path(), &[])
    };
    assert_eq!(f.run("slow", slow).await, Err(ToolRunError::Timeout));
    assert!(
        started.elapsed() < Duration::from_secs(4),
        "{:?}",
        started.elapsed()
    );
}
