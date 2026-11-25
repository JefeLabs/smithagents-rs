//! Pure view models for the shell: what the views show, computed without
//! GPUI so it can be unit-tested.

use smithagents_center_host::PixelRect;
use smithagents_core::agent::Agent;

use crate::model::Connection;

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
