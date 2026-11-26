//! Secret sources. Each test that sets an environment variable uses a name no
//! other test reads or writes, so the tests can run in parallel.

use std::collections::HashMap;
use std::path::Path;

use smithagents_plugin_host::{ConfiguredSecrets, SecretError, SecretSource, StaticSecrets};
use smithagents_plugin_protocol::PluginId;

fn id(s: &str) -> PluginId {
    s.parse().unwrap()
}

fn write_secrets(home: &Path, text: &str, mode: u32) {
    let path = home.join("secrets.toml");
    std::fs::write(&path, text).unwrap();
    set_mode(&path, mode);
}

#[cfg(unix)]
fn set_mode(path: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
}

#[cfg(not(unix))]
fn set_mode(_path: &Path, _mode: u32) {}

fn load(text: &str) -> (tempfile::TempDir, ConfiguredSecrets) {
    let home = tempfile::tempdir().unwrap();
    write_secrets(home.path(), text, 0o600);
    let secrets = ConfiguredSecrets::load(home.path()).unwrap();
    (home, secrets)
}

async fn value(source: &dyn SecretSource, plugin: &str, key: &str) -> Option<String> {
    source
        .get(&id(plugin), key)
        .await
        .unwrap()
        .map(|s| s.expose().to_string())
}

/// Neither rendering of `err` contains any of `forbidden`.
fn assert_hides(err: &SecretError, forbidden: &[&str]) {
    let rendered = [format!("{err:?}"), err.to_string()];
    for text in &rendered {
        for value in forbidden {
            assert!(!text.contains(value), "{value:?} leaked into {text:?}");
        }
    }
}

#[tokio::test]
async fn the_environment_fallback_separates_plugin_and_key_with_a_double_underscore() {
    std::env::set_var("SMITHAGENTS_SECRET_MY_PLUG__API_KEY", "fallback-5ecret");
    let home = tempfile::tempdir().unwrap();

    let secrets = ConfiguredSecrets::load(home.path()).unwrap();

    assert_eq!(
        value(&secrets, "my-plug", "api-key").await.as_deref(),
        Some("fallback-5ecret")
    );
}

#[tokio::test]
async fn fallback_names_cannot_collide_across_plugins() {
    // Under a single `_` separator both pairs were SMITHAGENTS_SECRET_GITHUB_API_TOKEN.
    std::env::set_var("SMITHAGENTS_SECRET_GITHUB__API_TOKEN", "for-github");
    std::env::set_var("SMITHAGENTS_SECRET_GITHUB_API__TOKEN", "for-github-api");
    let secrets = ConfiguredSecrets::default();

    assert_eq!(
        value(&secrets, "github", "api-token").await.as_deref(),
        Some("for-github")
    );
    assert_eq!(
        value(&secrets, "github-api", "token").await.as_deref(),
        Some("for-github-api")
    );
}

#[tokio::test]
async fn a_key_outside_the_manifest_rules_has_no_fallback() {
    // `api.key` would otherwise map onto the same name as `api-key`.
    std::env::set_var("SMITHAGENTS_SECRET_DOT_PLUG__API_KEY", "dot-5ecret");
    let secrets = ConfiguredSecrets::default();

    assert_eq!(value(&secrets, "dot-plug", "api.key").await, None);
    assert_eq!(value(&secrets, "dot-plug", "API-KEY").await, None);
    assert_eq!(
        value(&secrets, "dot-plug", "api-key").await.as_deref(),
        Some("dot-5ecret")
    );
}

#[tokio::test]
async fn a_secret_configured_nowhere_is_none() {
    let home = tempfile::tempdir().unwrap();
    let secrets = ConfiguredSecrets::load(home.path()).unwrap();

    assert_eq!(value(&secrets, "nowhere-plug", "token").await, None);
    let empty = ConfiguredSecrets::default();
    assert_eq!(value(&empty, "nowhere-plug", "token").await, None);
}

#[tokio::test]
async fn an_env_entry_reads_the_named_variable_and_wins_over_the_fallback() {
    std::env::set_var("SMITHAGENTS_TEST_ENV_ENTRY_7Q3", "entry-5ecret");
    std::env::set_var("SMITHAGENTS_SECRET_ENV_PLUG__TOKEN", "fallback-5ecret");
    let (_home, secrets) = load(
        r#"
        [env-plug]
        token = { env = "SMITHAGENTS_TEST_ENV_ENTRY_7Q3" }
        "#,
    );

    assert_eq!(
        value(&secrets, "env-plug", "token").await.as_deref(),
        Some("entry-5ecret")
    );
}

#[tokio::test]
async fn an_env_entry_naming_an_unset_variable_is_none() {
    let (_home, secrets) = load(
        r#"
        [unset-plug]
        token = { env = "SMITHAGENTS_TEST_NEVER_SET_4K8" }
        "#,
    );

    assert_eq!(value(&secrets, "unset-plug", "token").await, None);
}

#[cfg(unix)]
#[tokio::test]
async fn a_command_entry_yields_its_first_line_trimmed() {
    let (_home, secrets) = load(
        r#"
        [cmd-plug]
        token = { command = ["sh", "-c", "printf '  cmd-5ecret  \nsecond line\n'"] }
        "#,
    );

    assert_eq!(
        value(&secrets, "cmd-plug", "token").await.as_deref(),
        Some("cmd-5ecret")
    );
}

#[cfg(unix)]
#[tokio::test]
async fn a_command_runs_without_a_shell() {
    // Without a shell, `$HOME` and `;` reach the program as literal text.
    let (_home, secrets) = load(
        r#"
        [noshell-plug]
        token = { command = ["echo", "$HOME;", "x"] }
        "#,
    );

    assert_eq!(
        value(&secrets, "noshell-plug", "token").await.as_deref(),
        Some("$HOME; x")
    );
}

#[cfg(unix)]
#[tokio::test]
async fn a_failing_command_reports_its_status_but_not_its_output() {
    let (_home, secrets) = load(
        r#"
        [fail-plug]
        token = { command = ["sh", "-c", "echo out-5ecret; echo err-5ecret >&2; exit 3"] }
        "#,
    );

    let err = secrets.get(&id("fail-plug"), "token").await.unwrap_err();

    assert!(matches!(err, SecretError::Command { .. }), "{err:?}");
    assert!(err.to_string().contains("exit status: 3"), "{err}");
    assert_hides(&err, &["out-5ecret", "err-5ecret"]);
}

#[cfg(unix)]
#[tokio::test]
async fn a_command_that_cannot_start_is_an_error() {
    let (_home, secrets) = load(
        r#"
        [nocmd-plug]
        token = { command = ["/nonexistent/smithagents-no-such-program"] }
        "#,
    );

    let err = secrets.get(&id("nocmd-plug"), "token").await.unwrap_err();

    assert!(matches!(err, SecretError::Spawn { .. }), "{err:?}");
}

#[cfg(unix)]
#[tokio::test]
async fn the_cache_serves_a_second_read_without_running_the_command() {
    let home = tempfile::tempdir().unwrap();
    let runs = home.path().join("runs.log");
    let text = format!(
        r#"
        [cache-plug]
        token = {{ command = ["sh", "-c", "echo run >> \"$1\"; echo cached-5ecret", "sh", '{}'] }}
        "#,
        runs.display()
    );
    write_secrets(home.path(), &text, 0o600);
    let secrets = ConfiguredSecrets::load(home.path()).unwrap();

    for _ in 0..3 {
        assert_eq!(
            value(&secrets, "cache-plug", "token").await.as_deref(),
            Some("cached-5ecret")
        );
    }

    let log = std::fs::read_to_string(&runs).unwrap();
    assert_eq!(log.lines().count(), 1, "the command ran {log:?}");
}

#[cfg(unix)]
#[test]
fn a_group_or_world_writable_file_is_refused() {
    for mode in [0o666, 0o646, 0o620] {
        let home = tempfile::tempdir().unwrap();
        write_secrets(home.path(), "[perm-plug]\ntoken = { env = \"X\" }\n", mode);

        let err = ConfiguredSecrets::load(home.path())
            .err()
            .unwrap_or_else(|| panic!("mode {mode:o} was accepted"));

        assert!(matches!(err, SecretError::Permissions { .. }), "{err:?}");
        assert!(err.to_string().contains("writable"), "{err}");
    }
}

#[test]
fn a_missing_file_means_no_entries() {
    let home = tempfile::tempdir().unwrap();
    assert!(ConfiguredSecrets::load(home.path()).is_ok());
}

#[test]
fn a_literal_value_in_the_file_is_refused_without_echoing_it() {
    // People will paste the token itself; the error must not repeat it.
    let home = tempfile::tempdir().unwrap();
    for text in [
        "[lit-plug]\ntoken = \"literal-5ecret\"\n",
        "[lit-plug]\ntoken = { value = \"literal-5ecret\" }\n",
        "[lit-plug]\ntoken = \"literal-5ecret\n",
        "[lit-plug]\ntoken = { env = \"literal-5ecret\", command = [\"literal-5ecret\"] }\n",
        "lit-plug = \"literal-5ecret\"\n",
    ] {
        write_secrets(home.path(), text, 0o600);

        let err = ConfiguredSecrets::load(home.path())
            .err()
            .unwrap_or_else(|| panic!("accepted {text:?}"));

        assert!(matches!(err, SecretError::Parse { .. }), "{err:?}");
        assert_hides(&err, &["literal-5ecret"]);
    }
}

#[test]
fn malformed_entries_are_refused() {
    let home = tempfile::tempdir().unwrap();
    for text in [
        "[Not_An_Id]\ntoken = { env = \"X\" }\n",
        "[bad-plug]\ntoken = { command = [] }\n",
        "[bad-plug]\ntoken = { command = [\"prog\", 3] }\n",
        "[bad-plug]\ntoken = { env = \"\" }\n",
        "[bad-plug]\ntoken = {}\n",
    ] {
        write_secrets(home.path(), text, 0o600);
        let err = ConfiguredSecrets::load(home.path()).err();
        assert!(
            matches!(err, Some(SecretError::Parse { .. })),
            "{text:?}: {err:?}"
        );
    }
}

#[tokio::test]
async fn static_secrets_and_the_secret_debug_output() {
    let mut map = HashMap::new();
    map.insert(
        ("static-plug".to_string(), "token".to_string()),
        "static-5ecret".to_string(),
    );
    let secrets = StaticSecrets(map);

    let secret = secrets
        .get(&id("static-plug"), "token")
        .await
        .unwrap()
        .unwrap();

    assert_eq!(secret.expose(), "static-5ecret");
    assert_eq!(format!("{secret:?}"), "Secret(..)");
    assert_eq!(format!("{secret:#?}"), "Secret(..)");
    assert!(secrets
        .get(&id("static-plug"), "other")
        .await
        .unwrap()
        .is_none());
    assert!(secrets
        .get(&id("other-plug"), "token")
        .await
        .unwrap()
        .is_none());
}
