use chrono::{DateTime, Utc};
use smithagents_app::model::Connection;
use smithagents_app::view_models::{banner_text, center_rect, tab_title, LeftPaneMode};
use smithagents_core::agent::{Agent, AgentState};
use smithagents_core::ids::{AgentId, RepoId};
use smithagents_core::palette::AgentColor;

fn agent(name: &str) -> Agent {
    let at: DateTime<Utc> = "2026-11-24T09:00:00Z".parse().unwrap();
    Agent {
        id: AgentId::new(),
        name: name.into(),
        color: AgentColor {
            l: 0.5,
            c: 0.2,
            h: 100.0,
        },
        repo_id: RepoId::new(),
        worktree_path: "/tmp/wt".into(),
        branch: format!("agent/{name}"),
        harness_session_id: None,
        state: AgentState::Idle,
        created_at: at,
        closed_at: None,
        intent_id: None,
    }
}

#[test]
fn no_banner_while_connected() {
    assert_eq!(banner_text(&Connection::Connected, None), None);
    assert_eq!(
        banner_text(&Connection::Connected, Some("stale error")),
        None,
        "a leftover error never shows once connected"
    );
}

#[test]
fn connecting_banner_without_an_error() {
    assert_eq!(
        banner_text(&Connection::Connecting, None).as_deref(),
        Some("Connecting to the local daemon…")
    );
}

#[test]
fn reconnecting_banner_without_an_error() {
    assert_eq!(
        banner_text(&Connection::Reconnecting, None).as_deref(),
        Some("Reconnecting to the local daemon…")
    );
}

#[test]
fn the_error_goes_on_a_second_line() {
    assert_eq!(
        banner_text(
            &Connection::Reconnecting,
            Some("could not start /x/smithagentsd: not found")
        )
        .as_deref(),
        Some("Reconnecting to the local daemon…\ncould not start /x/smithagentsd: not found")
    );
    assert_eq!(
        banner_text(&Connection::Connecting, Some("refused")).as_deref(),
        Some("Connecting to the local daemon…\nrefused")
    );
}

#[test]
fn a_blank_error_adds_no_line_and_the_error_is_trimmed() {
    assert_eq!(
        banner_text(&Connection::Reconnecting, Some("  \n ")).as_deref(),
        Some("Reconnecting to the local daemon…")
    );
    assert_eq!(
        banner_text(&Connection::Connecting, Some("  refused\n")).as_deref(),
        Some("Connecting to the local daemon…\nrefused")
    );
}

#[test]
fn tab_title_is_the_agent_name() {
    assert_eq!(tab_title(&agent("Mercury")), "Mercury");
    assert_eq!(tab_title(&agent("Venus 2")), "Venus 2");
}

#[test]
fn center_rect_multiplies_by_the_scale() {
    let rect = center_rect(240.0, 64.5, 700.0, 500.25, 2.0);
    assert_eq!(rect.x, 480.0);
    assert_eq!(rect.y, 129.0);
    assert_eq!(rect.w, 1400.0);
    assert_eq!(rect.h, 1000.5);
    assert_eq!(rect.scale, 2.0);
}

#[test]
fn center_rect_at_scale_one_is_unchanged() {
    let rect = center_rect(10.0, 20.0, 30.0, 40.0, 1.0);
    assert_eq!(
        (rect.x, rect.y, rect.w, rect.h, rect.scale),
        (10.0, 20.0, 30.0, 40.0, 1.0)
    );
}

#[test]
fn center_rect_keeps_fractional_scales_exact_enough() {
    let rect = center_rect(100.0, 0.0, 200.0, 100.0, 1.5);
    assert_eq!(
        (rect.x, rect.w, rect.h, rect.scale),
        (150.0, 300.0, 150.0, 1.5)
    );
}

#[test]
fn center_rect_never_reports_a_negative_size_or_a_bad_scale() {
    let rect = center_rect(5.0, 5.0, -10.0, -1.0, 2.0);
    assert_eq!((rect.w, rect.h), (0.0, 0.0));

    for scale in [0.0, -2.0, f32::NAN, f32::INFINITY] {
        let rect = center_rect(1.0, 2.0, 3.0, 4.0, scale);
        assert_eq!(
            (rect.x, rect.y, rect.w, rect.h, rect.scale),
            (1.0, 2.0, 3.0, 4.0, 1.0),
            "scale {scale} falls back to 1"
        );
    }

    let rect = center_rect(f32::NAN, f32::INFINITY, f32::NAN, f32::NEG_INFINITY, 2.0);
    assert_eq!(
        (rect.x, rect.y, rect.w, rect.h),
        (0.0, 0.0, 0.0, 0.0),
        "non-finite input becomes 0"
    );
}

#[test]
fn the_left_pane_starts_expanded_and_toggles_between_two_modes() {
    assert_eq!(LeftPaneMode::default(), LeftPaneMode::Expanded);
    assert_eq!(LeftPaneMode::Expanded.toggled(), LeftPaneMode::Compact);
    assert_eq!(LeftPaneMode::Compact.toggled(), LeftPaneMode::Expanded);
}
