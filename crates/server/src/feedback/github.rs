//! The command-line tools of the feedback plugin as Submit sees them: is a
//! tool ready, and if not, why. Only looked up through the plugin manager,
//! so a plugin that is turned off or not running has no tool ready, and
//! nothing runs.

use smithagents_core::feedback::ToolReadiness;
use smithagents_plugin_host::{MissingReason, ToolState};

use crate::plugins::{ManagerError, PluginManager, FEEDBACK_PLUGIN_ID};

pub const GH: &str = "gh";
pub const AGENT_BROWSER: &str = "agent-browser";
/// The reason of a tool that is not installed.
pub const NOT_INSTALLED: &str = "not installed";

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
        Err(ManagerError::Disabled) => Some("turned off or not running"),
        Err(e) => {
            tracing::error!(tool, "could not check a feedback tool: {e}");
            Some("not available")
        }
    };
    ToolReadiness {
        ready: reason.is_none(),
        reason: reason.map(str::to_owned),
    }
}
