//! Plugins through the client, against the real `smithagentsd`: a packed
//! WebAssembly fixture installed in the home's `plugins/` directory is listed,
//! enabled, synced and read back.

mod common;

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::time::{Duration, Instant};

use smithagents_client::{ensure_daemon_with, ClientError, SpawnOptions};
use smithagents_core::daemon::read_lockfile;
use smithagents_core::plugin::PluginState;
use smithagents_core::VERSION;
use smithagents_plugins_tool::pack_fixtures_once;

use common::{daemon_bin, TestHome};

const PATIENCE: Duration = Duration::from_secs(60);
const SECRET: &str = "s3cr3t-value-for-the-client-test";
const ENTRY: &str = "smithagents_plugin_fixture_wasm.wasm";

fn chmod(path: &Path, mode: u32) {
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
}

/// Installs the packed wasm fixture with a manifest of its own: the packed one
/// declares hostile test resources the scheduler would run once enabled.
fn install_plugin(home: &Path) {
    let dir = home.join("plugins").join("fixture-wasm");
    fs::create_dir_all(&dir).unwrap();
    chmod(home, 0o700);
    chmod(&home.join("plugins"), 0o755);
    let wasm = dir.join(ENTRY);
    fs::copy(
        pack_fixtures_once().dir.join("fixture-wasm").join(ENTRY),
        &wasm,
    )
    .unwrap();
    chmod(&wasm, 0o644);
    let manifest = dir.join("plugin.toml");
    fs::write(
        &manifest,
        format!(
            "id = \"fixture-wasm\"\nname = \"Fixture\"\nversion = \"0.1.0\"\ncontract = 1\n\
             host = \"wasm\"\nentry = \"{ENTRY}\"\nhosts = [\"fixture.invalid\"]\n\
             [[secrets]]\nkey = \"token\"\nlabel = \"Token\"\nhosts = [\"fixture.invalid\"]\n\
             [[settings]]\nkey = \"note\"\nlabel = \"Note\"\nkind = \"text\"\n\
             [[resources]]\nkind = \"items\"\nlabel = \"Items\"\ninterval_secs = 3600\n"
        ),
    )
    .unwrap();
    chmod(&manifest, 0o644);
    chmod(&dir, 0o755);
}

async fn within<T>(what: &str, fut: impl std::future::Future<Output = T>) -> T {
    tokio::time::timeout(PATIENCE, fut)
        .await
        .unwrap_or_else(|_| panic!("{what} did not finish within {PATIENCE:?}"))
}

#[tokio::test]
async fn a_client_lists_enables_syncs_and_reads_a_wasm_plugin() {
    let bin = daemon_bin();
    let home = TestHome::new();
    fs::create_dir_all(home.path()).unwrap();
    install_plugin(home.path());
    let options = SpawnOptions {
        args: vec![],
        env: vec![(
            "SMITHAGENTS_SECRET_FIXTURE_WASM__TOKEN".into(),
            SECRET.into(),
        )],
    };
    let client = within(
        "ensure_daemon_with",
        ensure_daemon_with(home.path(), bin, VERSION, &options),
    )
    .await
    .unwrap_or_else(|e| panic!("{e}; daemon log:\n{}", home.log()));

    // Other plugins may sit beside the daemon binary: look only at ours.
    let all = client.plugins().await.unwrap();
    let ours = all.iter().find(|p| p.id == "fixture-wasm").expect("listed");
    assert!(!ours.enabled);
    assert_eq!(ours.state, PluginState::Disabled);
    assert!(!ours.builtin);
    assert_eq!(ours.host, "wasm");
    assert_eq!(ours.resources.len(), 1);

    let missing = client.plugin("no-such-plugin").await.unwrap_err();
    assert!(
        matches!(&missing, ClientError::Api { status: 404, code, .. } if code == "plugin_not_found"),
        "{missing:?}"
    );
    let refused = client
        .sync_plugin_resource("fixture-wasm", "items")
        .await
        .unwrap_err();
    assert!(
        matches!(&refused, ClientError::Api { status: 409, code, .. } if code == "plugin_disabled"),
        "{refused:?}"
    );

    assert_eq!(
        client.plugin_settings("fixture-wasm").await.unwrap(),
        serde_json::json!({})
    );
    client
        .set_plugin_settings("fixture-wasm", &serde_json::json!({"note": "hi"}))
        .await
        .unwrap();
    assert_eq!(
        client.plugin_settings("fixture-wasm").await.unwrap(),
        serde_json::json!({"note": "hi"})
    );

    let enabled = client
        .set_plugin_enabled("fixture-wasm", true)
        .await
        .unwrap();
    assert!(enabled.enabled);

    // Enabling starts a sync; wait for its six items.
    let deadline = Instant::now() + PATIENCE;
    loop {
        let page = client
            .plugin_items("fixture-wasm", "items", None, None, None)
            .await
            .unwrap();
        if page.items.len() == 6 {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "no six items yet: {page:?}\n{}",
            home.log()
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let status = client.plugin("fixture-wasm").await.unwrap();
    assert_eq!(status.state, PluginState::Ready, "{status:?}");
    assert_eq!(status.resources[0].item_count, 6);

    // A manual sync is accepted (it may find the run already finished).
    client
        .sync_plugin_resource("fixture-wasm", "items")
        .await
        .unwrap();

    // Paging and filtering through the client's parameters.
    let first = client
        .plugin_items("fixture-wasm", "items", Some(4), None, None)
        .await
        .unwrap();
    assert_eq!(first.items.len(), 4);
    let rest = client
        .plugin_items(
            "fixture-wasm",
            "items",
            Some(4),
            first.next.as_deref(),
            None,
        )
        .await
        .unwrap();
    assert_eq!(rest.items.len(), 2);
    assert!(rest.next.is_none());
    let found = client
        .plugin_items("fixture-wasm", "items", None, None, Some("item-3"))
        .await
        .unwrap();
    assert_eq!(found.items.len(), 1);
    let bad = client
        .plugin_items("fixture-wasm", "items", None, Some("zz"), None)
        .await
        .unwrap_err();
    assert!(
        matches!(&bad, ClientError::Api { status: 400, code, .. } if code == "bad_cursor"),
        "{bad:?}"
    );

    // The compile cache is private to this user.
    let cache = home.path().join("cache").join("wasm");
    assert_eq!(
        fs::metadata(&cache).unwrap().permissions().mode() & 0o777,
        0o700
    );

    // Neither the daemon's token nor a secret value reached its log.
    let token = read_lockfile(home.path())
        .unwrap()
        .expect("a lockfile")
        .token;
    let log = home.log();
    assert!(!log.contains(&token), "the token is in the daemon log");
    assert!(!log.contains(SECRET), "a secret is in the daemon log");
}
