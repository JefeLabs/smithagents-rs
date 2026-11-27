//! What the API and events say about a plugin.

use smithagents_core::plugin::{MissingTool, PluginState, PluginStatus, ResourceStatus};

use super::{tools, Inner, Live, Managed, Problem, UNREADABLE_STATE};
use crate::plugins::{settings, Origin};

/// Messages shown to users are at most this long.
const MAX_MESSAGE_BYTES: usize = 1024;
const ELLIPSIS: char = '…';

/// Text from a plugin, a guest or an error, made fit for an API response, an
/// event or a log: control characters, line and paragraph separators and
/// bidi marks become spaces, and the text is cut to 1 KiB on a character
/// boundary, ending in an ellipsis when cut. Reads no more than it keeps.
pub(crate) fn clean_message(text: &str) -> String {
    let mut out = String::new();
    for c in text.chars() {
        let c = if c.is_control() || is_separator_or_bidi(c) {
            ' '
        } else {
            c
        };
        if out.len() + c.len_utf8() > MAX_MESSAGE_BYTES {
            while out.len() + ELLIPSIS.len_utf8() > MAX_MESSAGE_BYTES {
                out.pop();
            }
            out.push(ELLIPSIS);
            return out;
        }
        out.push(c);
    }
    out
}

/// [`clean_message`]'s cleaning without its byte cap, for user text the
/// caller bounds by characters itself: every control character, line or
/// paragraph separator and bidirectional override or isolate as a space.
pub(crate) fn clean_text(text: &str) -> String {
    text.chars()
        .map(|c| {
            if c.is_control() || is_separator_or_bidi(c) {
                ' '
            } else {
                c
            }
        })
        .collect()
}

/// U+2028 and U+2029, U+202A to U+202E and U+2066 to U+2069.
fn is_separator_or_bidi(c: char) -> bool {
    matches!(
        c,
        '\u{2028}' | '\u{2029}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}'
    )
}

/// `state` with every message through [`clean_message`], here as well as
/// where it is made, so none can reach a response or an event uncapped.
fn clean_state(state: PluginState) -> PluginState {
    match state {
        PluginState::Disabled => PluginState::Disabled,
        PluginState::Ready => PluginState::Ready,
        PluginState::NeedsAuth { missing, message } => PluginState::NeedsAuth {
            missing,
            message: clean_message(&message),
        },
        PluginState::NeedsSettings { message } => PluginState::NeedsSettings {
            message: clean_message(&message),
        },
        PluginState::NeedsSetup { tools, message } => PluginState::NeedsSetup {
            tools: tools
                .into_iter()
                .map(|t| MissingTool {
                    label: clean_message(&t.label),
                    reason: clean_message(&t.reason),
                    ..t
                })
                .collect(),
            message: clean_message(&message),
        },
        PluginState::Failed { message } => PluginState::Failed {
            message: clean_message(&message),
        },
        PluginState::LoadFailed { message } => PluginState::LoadFailed {
            message: clean_message(&message),
        },
    }
}

/// The plugin's state: unreadable stored state, a load failure, unapproved
/// tools or a stop first
/// (enabling does not clear them), then disabled, then invalid settings, then
/// a problem a run met. A plugin without resources never runs: see
/// [`status`] for its tools.
fn raw_state(plugin: &Managed, live: &Live) -> PluginState {
    if plugin.is_unreadable() {
        return PluginState::LoadFailed {
            message: UNREADABLE_STATE.to_string(),
        };
    }
    if let Err(message) = &plugin.loaded {
        return PluginState::LoadFailed {
            message: message.clone(),
        };
    }
    if plugin.unapproved_tools {
        return PluginState::NeedsSetup {
            tools: tools::unapproved(&plugin.manifest),
            message: tools::UNAPPROVED.to_string(),
        };
    }
    if let Some(Problem::Failed { message }) = &live.problem {
        return PluginState::Failed {
            message: message.clone(),
        };
    }
    if !live.enabled {
        return PluginState::Disabled;
    }
    if let Some(message) = &live.settings_error {
        return PluginState::NeedsSettings {
            message: message.clone(),
        };
    }
    match &live.problem {
        None => PluginState::Ready,
        Some(Problem::NeedsAuth { missing, message }) => PluginState::NeedsAuth {
            missing: missing.clone(),
            message: message.clone(),
        },
        Some(Problem::NeedsSettings { message }) => PluginState::NeedsSettings {
            message: message.clone(),
        },
        Some(Problem::NeedsSetup { tools, message }) => PluginState::NeedsSetup {
            tools: tools.clone(),
            message: message.clone(),
        },
        Some(Problem::Failed { message }) => PluginState::Failed {
            message: message.clone(),
        },
    }
}

/// The plugin's status. A plugin with resources checks its tools before a
/// run, and its state says what the last run found. One without resources
/// never runs, so when it would otherwise be ready, its tools are checked
/// here (a check is trusted for 60 s): a missing one makes it `NeedsSetup`,
/// which is advice and holds nothing back.
pub(super) async fn status(inner: &Inner, plugin: &Managed) -> PluginStatus {
    let (enabled, mut state, resources) = {
        let live = plugin.live();
        let running = live.running.as_ref().map(|r| r.kind.as_str());
        let resources: Vec<_> = live
            .resources
            .iter()
            .map(|r| {
                (
                    r.kind.clone(),
                    r.sync.clone(),
                    running == Some(r.kind.as_str()),
                )
            })
            .collect();
        (live.enabled, raw_state(plugin, &live), resources)
    };
    let manifest = &plugin.manifest;
    if manifest.resources.is_empty() && state == PluginState::Ready {
        let missing = tools::missing_tools(inner, plugin).await;
        if !missing.is_empty() {
            state = PluginState::NeedsSetup {
                message: tools::setup_message(&missing),
                tools: missing,
            };
        }
    }
    let state = clean_state(state);
    let mut statuses = Vec::with_capacity(resources.len());
    for (decl, (kind, sync, syncing)) in manifest.resources.iter().zip(resources) {
        let item_count = match inner.store.count_items(&plugin.id, &kind).await {
            Ok(n) => n,
            Err(e) => {
                tracing::warn!(plugin = %plugin.id, resource = %kind, "cannot count items: {e}");
                0
            }
        };
        statuses.push(ResourceStatus {
            kind,
            label: decl.label.clone(),
            item_count,
            last_run_at: sync.last_run_at,
            last_ok_at: sync.last_ok_at,
            next_due_at: sync.next_due_at,
            last_error: sync.last_error.as_deref().map(clean_message),
            syncing,
        });
    }
    PluginStatus {
        id: plugin.id.clone(),
        name: manifest.name.clone(),
        version: manifest.version.to_string(),
        host: manifest.host.to_string(),
        builtin: plugin.origin == Origin::Builtin,
        enabled,
        state,
        hosts: manifest.hosts.clone(),
        secrets: manifest.secrets.iter().map(|s| s.key.clone()).collect(),
        settings: settings::fields(manifest),
        resources: statuses,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_text_only_loses_control_characters_and_bidi_marks() {
        assert_eq!(clean_message("plain text é"), "plain text é");
        assert_eq!(
            clean_message("a\nb\r\tc\u{1b}[0m\u{7f}\u{85}"),
            "a b  c [0m  "
        );
        assert_eq!(clean_message("x\u{2028}y\u{202e}z\u{2066}"), "x y z ");
    }

    #[test]
    fn long_text_is_cut_to_one_kib_on_a_character_boundary() {
        let exact = "a".repeat(MAX_MESSAGE_BYTES);
        assert_eq!(clean_message(&exact), exact);

        let cut = clean_message(&"a".repeat(MAX_MESSAGE_BYTES + 1));
        assert_eq!(cut.len(), MAX_MESSAGE_BYTES);
        assert!(cut.ends_with(ELLIPSIS));

        // Two-byte characters: the cut never splits one.
        let cut = clean_message(&"é".repeat(MAX_MESSAGE_BYTES));
        assert!(cut.len() <= MAX_MESSAGE_BYTES);
        assert!(cut.ends_with(ELLIPSIS));
        assert!(cut.trim_end_matches(ELLIPSIS).chars().all(|c| c == 'é'));
    }
}
