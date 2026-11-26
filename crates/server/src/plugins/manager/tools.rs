//! The command-line tools a built-in plugin depends on: the check before a
//! run, the install the user asks for, and the secrets a tool prints.
//!
//! Only built-in plugins' tools are honoured. An installed plugin that
//! declares tools never runs: running a program a third-party manifest names
//! needs an approval that does not exist yet. Every message here is made from
//! the manifest's tool names and labels and fixed sentences, never from what
//! a tool printed.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use smithagents_core::plugin::MissingTool;
use smithagents_plugin_host::{MissingReason, SecretSource, ToolFinder, ToolSecrets, ToolState};
use smithagents_plugin_protocol::{Manifest, SecretDecl, ToolDecl};
use tokio::time::Instant;

use super::{clean_message, Inner, LoadedPlugin, Managed, ManagerError, Problem};
use crate::plugins::Origin;

/// How long a check of a plugin's tools is trusted before a run checks
/// again. `sync_now`, enabling, new settings and an install check at once.
const TOOL_CHECK_TTL: Duration = Duration::from_secs(60);

/// The state of an installed plugin that declares tools.
pub(super) const UNAPPROVED: &str = "command-line tools need approval, which is not built yet";

/// What the last check of a plugin's tools found.
pub(super) struct ToolCheck {
    at: Instant,
    missing: Vec<MissingTool>,
}

/// The last check of a plugin's tools, and which checks may still replace
/// it. Lives in the plugin's `Live`.
#[derive(Default)]
pub(super) struct ToolChecks {
    last: Option<ToolCheck>,
    /// Bumped whenever the last check is forgotten: a check that began
    /// before keeps its answer for its own run but does not store it.
    epoch: u64,
}

impl ToolChecks {
    /// `sync_now`, enabling, new settings and an install: the next run
    /// checks again, and checks already running do not store their result.
    pub(super) fn forget(&mut self) {
        self.epoch += 1;
        self.last = None;
    }
}

/// `secrets`, then, for each built-in plugin's `from_tool` secret, the tool
/// it names. An installed plugin's `from_tool` is never registered.
pub(super) fn with_tool_secrets(
    secrets: Arc<dyn SecretSource>,
    plugins: &[LoadedPlugin],
    finder: &ToolFinder,
) -> Arc<dyn SecretSource> {
    let mut wrapped = ToolSecrets::new(secrets, finder.clone());
    for plugin in plugins {
        match plugin.origin {
            Origin::Builtin => {}
            Origin::Installed => continue,
        }
        for decl in &plugin.manifest.secrets {
            if let Some(source) = &decl.from_tool {
                wrapped.register(&plugin.manifest.id, &decl.key, source.clone());
            }
        }
    }
    Arc::new(wrapped)
}

/// The plugin's tools that cannot be used, from a check at most 60 s old.
pub(super) async fn missing_tools(inner: &Inner, plugin: &Managed) -> Vec<MissingTool> {
    if plugin.manifest.tools.is_empty() {
        return Vec::new();
    }
    let cached = plugin
        .live()
        .tool_checks
        .last
        .as_ref()
        .filter(|check| check.at.elapsed() < TOOL_CHECK_TTL)
        .map(|check| check.missing.clone());
    match cached {
        Some(missing) => missing,
        None => check_now(inner, plugin).await,
    }
}

/// Checks every tool of the plugin now and keeps the result, unless the
/// last check was forgotten while this one ran.
async fn check_now(inner: &Inner, plugin: &Managed) -> Vec<MissingTool> {
    let epoch = plugin.live().tool_checks.epoch;
    let mut missing = Vec::new();
    for decl in &plugin.manifest.tools {
        let state = inner.tools.check(decl).await;
        missing.extend(missing_tool(decl, &state));
    }
    let mut live = plugin.live();
    if live.tool_checks.epoch == epoch {
        live.tool_checks.last = Some(ToolCheck {
            at: Instant::now(),
            missing: missing.clone(),
        });
    }
    missing
}

/// What the user is told about a tool that cannot be used, with the
/// manifest's install hints. A version the tool printed is not repeated.
fn missing_tool(decl: &ToolDecl, state: &ToolState) -> Option<MissingTool> {
    let reason = match state {
        ToolState::Ready { .. } => return None,
        ToolState::Missing {
            reason: MissingReason::NotFound,
        } => "not installed".to_string(),
        ToolState::Missing {
            reason: MissingReason::UnreadableVersion,
        } => "could not read its version".to_string(),
        ToolState::TooOld { .. } => match &decl.min_version {
            Some(min) => format!("older than the required version {min}"),
            None => "too old".to_string(),
        },
    };
    Some(MissingTool {
        name: decl.name.clone(),
        label: clean_message(&decl.label),
        reason: clean_message(&reason),
        install_url: decl.install_url.clone(),
        install: decl.install.clone(),
    })
}

/// `needs <label> (<name>): <reason>` for each tool.
pub(super) fn setup_message(tools: &[MissingTool]) -> String {
    tools
        .iter()
        .map(|t| format!("needs {} ({}): {}", t.label, t.name, t.reason))
        .collect::<Vec<_>>()
        .join("; ")
}

/// An installed plugin's declared tools, without its install hints: the
/// daemon does not point the user at what a third-party manifest names.
pub(super) fn unapproved(manifest: &Manifest) -> Vec<MissingTool> {
    manifest
        .tools
        .iter()
        .map(|decl| MissingTool {
            name: decl.name.clone(),
            label: clean_message(&decl.label),
            reason: "needs approval".to_string(),
            install_url: None,
            install: Default::default(),
        })
        .collect()
}

/// Why the plugin needs auth: for each missing secret that `secrets.toml`
/// lists, that its entry has no value (the entry alone decides, so no tool
/// was asked); for each one of a built-in plugin that a tool provides,
/// `<tool label> is not signed in` and the secret's help; for the others,
/// that no value is configured.
pub(super) fn auth_message(
    secrets: &dyn SecretSource,
    plugin: &Managed,
    missing: &[String],
) -> String {
    let mut parts = Vec::new();
    let mut unconfigured = Vec::new();
    for key in missing {
        if secrets.is_explicit(&plugin.manifest.id, key) {
            parts.push(format!("the secrets.toml entry for {key} has no value"));
            continue;
        }
        let decl = plugin.manifest.secrets.iter().find(|s| &s.key == key);
        match decl.and_then(|decl| Some((decl, secret_tool(plugin, decl)?))) {
            Some((decl, tool)) => parts.push(match &decl.help {
                Some(help) => format!("{} is not signed in. {help}", tool.label),
                None => format!("{} is not signed in", tool.label),
            }),
            None => unconfigured.push(key.as_str()),
        }
    }
    match unconfigured.as_slice() {
        [] => {}
        [key] => parts.push(format!("no value is configured for the secret {key}")),
        keys => parts.push(format!(
            "no value is configured for the secrets {}",
            keys.join(", ")
        )),
    }
    parts.join("; ")
}

/// The tool a built-in plugin's secret comes from, if any.
fn secret_tool<'a>(plugin: &'a Managed, decl: &SecretDecl) -> Option<&'a ToolDecl> {
    match plugin.origin {
        Origin::Builtin => {}
        Origin::Installed => return None,
    }
    let source = decl.from_tool.as_ref()?;
    plugin.manifest.tools.iter().find(|t| t.name == source.tool)
}

/// Held while a tool installs, and released however the install ends:
/// success, an error, a timeout or a panic. One install at a time in the
/// whole daemon, whatever the tool: `mise use --global` edits one global
/// configuration file.
struct Installing<'a>(&'a AtomicBool);

impl<'a> Installing<'a> {
    /// `None` while another install runs.
    fn begin(busy: &'a AtomicBool) -> Option<Installing<'a>> {
        busy.compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .ok()
            .map(|_| Installing(busy))
    }
}

impl Drop for Installing<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

/// Installs the built-in plugin's tool `name`, then checks all its tools
/// again: when none is missing the plugin is renewed as by enabling (any
/// problem is looked at again and everything is due now); otherwise it
/// needs setup for what is still missing.
pub(super) async fn install(
    inner: &Inner,
    plugin: &Managed,
    name: &str,
) -> Result<(), ManagerError> {
    match plugin.origin {
        Origin::Builtin => {}
        Origin::Installed => return Err(ManagerError::NotAllowed),
    }
    let decl = plugin
        .manifest
        .tools
        .iter()
        .find(|t| t.name == name)
        .ok_or(ManagerError::UnknownTool)?;
    let _installing =
        Installing::begin(&inner.installing).ok_or(ManagerError::InstallInProgress)?;
    tracing::info!(plugin = %plugin.id, tool = %decl.name, "installing a command-line tool with mise");
    if let Err(e) = inner.tools.install(decl).await {
        // The error carries an exit status at most, never mise's output.
        tracing::warn!(plugin = %plugin.id, tool = %decl.name, "the tool was not installed: {e}");
        return Err(ManagerError::Install(e));
    }
    // A check that began before the install must not store its answer.
    plugin.live().tool_checks.forget();
    let missing = check_now(inner, plugin).await;
    let renewed = {
        let mut live = plugin.live();
        if live.is_failed() {
            false
        } else if missing.is_empty() {
            live.reset(Instant::now(), inner.now());
            live.tool_checks.last = Some(ToolCheck {
                at: Instant::now(),
                missing: Vec::new(),
            });
            true
        } else {
            live.problem = Some(Problem::NeedsSetup {
                message: clean_message(&setup_message(&missing)),
                tools: missing,
            });
            false
        }
    };
    if renewed {
        inner.persist_all(plugin).await;
    }
    inner.wake.notify_one();
    Ok(())
}
