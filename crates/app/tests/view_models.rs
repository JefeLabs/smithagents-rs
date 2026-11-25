use chrono::{DateTime, Duration, Utc};
use smithagents_app::link::LinkError;
use smithagents_app::model::{Action, Connection, ShellModel};
use smithagents_app::view_models::{
    agent_initial, agent_rows, banner_text, center_rect, command_error_text, new_agent_plan,
    repo_choices, repo_title, state_dot, state_kind, state_label, state_token, tab_title, DotStyle,
    LeftPaneMode, NewAgentPlan, StateKind, DAEMON_SILENT_TEXT, NOT_CONNECTED_TEXT,
};
use smithagents_client::ClientError;
use smithagents_core::agent::{Agent, AgentState, Repo};
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

// ---- Agents pane -------------------------------------------------------------

/// An agent created `minutes` after a fixed start, with `hue` and `state`.
fn agent_at(name: &str, minutes: i64, hue: f32, state: AgentState) -> Agent {
    let start: DateTime<Utc> = "2026-11-24T09:00:00Z".parse().unwrap();
    Agent {
        created_at: start + Duration::minutes(minutes),
        color: AgentColor {
            l: 0.5,
            c: 0.2,
            h: hue,
        },
        state,
        ..agent(name)
    }
}

fn model_with(agents: Vec<Agent>) -> ShellModel {
    let mut model = ShellModel::new();
    model.apply(Action::Snapshot {
        agents,
        repos: vec![],
        latest_seq: 1,
    });
    model
}

#[test]
fn an_empty_model_has_no_rows() {
    assert!(agent_rows(&ShellModel::new()).is_empty());
}

#[test]
fn rows_follow_the_models_order_oldest_first() {
    // Given out of order; the model sorts by creation.
    let venus = agent_at("Venus", 5, 200.0, AgentState::Idle);
    let mercury = agent_at("Mercury", 1, 260.0, AgentState::Idle);
    let earth = agent_at("Earth", 9, 300.0, AgentState::Idle);
    let model = model_with(vec![venus.clone(), earth.clone(), mercury.clone()]);
    let rows = agent_rows(&model);
    let names: Vec<&str> = rows.iter().map(|r| r.name.as_str()).collect();
    assert_eq!(names, ["Mercury", "Venus", "Earth"]);
    let ids: Vec<AgentId> = rows.iter().map(|r| r.id).collect();
    let model_ids: Vec<AgentId> = model.agents().iter().map(|a| a.id).collect();
    assert_eq!(ids, model_ids, "exactly the model's order");
    assert_eq!(rows[0].id, mercury.id);
}

#[test]
fn the_accent_hue_passes_through_unchanged() {
    let odd = agent_at("Mercury", 0, 287.125, AgentState::Idle);
    let rows = agent_rows(&model_with(vec![odd]));
    assert_eq!(rows[0].accent_hue, 287.125);
}

#[test]
fn selected_and_open_tab_follow_the_model() {
    let mercury = agent_at("Mercury", 0, 200.0, AgentState::Idle);
    let venus = agent_at("Venus", 1, 260.0, AgentState::Idle);
    let mut model = model_with(vec![mercury.clone(), venus.clone()]);
    let flags = |model: &ShellModel| -> Vec<(bool, bool)> {
        agent_rows(model)
            .iter()
            .map(|r| (r.selected, r.has_open_tab))
            .collect()
    };
    assert_eq!(flags(&model), [(false, false), (false, false)]);

    model.apply(Action::SelectAgent(venus.id));
    assert_eq!(flags(&model), [(false, false), (true, false)]);

    // Opening a tab also selects.
    model.apply(Action::OpenTab(mercury.id));
    assert_eq!(flags(&model), [(true, true), (false, false)]);

    model.apply(Action::SelectAgent(venus.id));
    assert_eq!(flags(&model), [(false, true), (true, false)]);
}

#[test]
fn state_labels_and_kinds() {
    assert_eq!(state_label(StateKind::Idle), "idle");
    assert_eq!(state_label(StateKind::Working), "working");
    assert_eq!(state_label(StateKind::Quiet), "quiet");
    assert_eq!(state_label(StateKind::Done), "done");
    assert_eq!(state_label(StateKind::Dead), "stopped unexpectedly");

    assert_eq!(state_kind(AgentState::Idle), StateKind::Idle);
    assert_eq!(state_kind(AgentState::Working), StateKind::Working);
    assert_eq!(state_kind(AgentState::Quiet), StateKind::Quiet);
    assert_eq!(state_kind(AgentState::Done), StateKind::Done);
    assert_eq!(state_kind(AgentState::Dead), StateKind::Dead);
    // Plan 2's queue states have no kind of their own in the chat-only shell.
    for state in [
        AgentState::Blocked,
        AgentState::Waiting,
        AgentState::Stopped,
    ] {
        assert_eq!(state_kind(state), StateKind::Idle, "{state:?}");
    }
}

#[test]
fn a_dead_agent_shows_stopped_unexpectedly() {
    let dead = agent_at("Mars", 0, 200.0, AgentState::Dead);
    let row = agent_rows(&model_with(vec![dead])).remove(0);
    assert_eq!(row.state_kind, StateKind::Dead);
    assert_eq!(row.state_label, "stopped unexpectedly");
    assert_eq!(row.a11y_label, "Mars, stopped unexpectedly");
}

#[test]
fn the_accessibility_label_is_name_comma_state() {
    let working = agent_at("Mercury", 0, 200.0, AgentState::Working);
    let quiet = agent_at("Venus 2", 1, 260.0, AgentState::Quiet);
    let rows = agent_rows(&model_with(vec![working, quiet]));
    assert_eq!(rows[0].a11y_label, "Mercury, working");
    assert_eq!(rows[1].a11y_label, "Venus 2, quiet");
}

#[test]
fn each_state_uses_its_status_token() {
    assert_eq!(state_token(StateKind::Working), "status-info");
    assert_eq!(
        state_token(StateKind::Quiet),
        "status-info",
        "a turn is running"
    );
    assert_eq!(state_token(StateKind::Done), "status-ok");
    assert_eq!(state_token(StateKind::Dead), "status-error");
    assert_eq!(state_token(StateKind::Idle), "muted-foreground");
}

#[test]
fn each_state_has_a_dot_shape_so_color_is_not_the_only_cue() {
    let dot = |token, filled, dimmed| DotStyle {
        token,
        filled,
        dimmed,
    };
    assert_eq!(
        state_dot(StateKind::Idle),
        dot("muted-foreground", false, false),
        "idle: a hollow ring"
    );
    assert_eq!(
        state_dot(StateKind::Working),
        dot("status-info", true, false)
    );
    assert_eq!(
        state_dot(StateKind::Quiet),
        dot("status-info", true, true),
        "quiet: running but silent, a dimmed dot"
    );
    assert_eq!(state_dot(StateKind::Done), dot("status-ok", true, false));
    assert_eq!(state_dot(StateKind::Dead), dot("status-error", true, false));
    let kinds = [
        StateKind::Idle,
        StateKind::Working,
        StateKind::Quiet,
        StateKind::Done,
        StateKind::Dead,
    ];
    for kind in kinds {
        assert_eq!(state_token(kind), state_dot(kind).token, "{kind:?}");
    }
    // The two states sharing a color differ in shape.
    assert_ne!(state_dot(StateKind::Working), state_dot(StateKind::Quiet));
    // Idle is the only hollow one.
    let hollow: Vec<_> = kinds.iter().filter(|k| !state_dot(**k).filled).collect();
    assert_eq!(hollow, [&StateKind::Idle]);
}

#[test]
fn a_silent_daemon_has_its_own_words() {
    assert_eq!(DAEMON_SILENT_TEXT, "The daemon did not answer");
}

#[test]
fn the_initial_is_the_first_grapheme_uppercased() {
    assert_eq!(agent_initial("Mercury"), "M");
    assert_eq!(agent_initial("mars"), "M");
    assert_eq!(agent_initial("  venus"), "V", "leading blanks are skipped");
    assert_eq!(agent_initial("éarth"), "É");
    // A decomposed accent stays with its letter.
    assert_eq!(agent_initial("e\u{301}arth"), "E\u{301}");
    assert_eq!(agent_initial("🚀 Rocket"), "🚀");
    // A ZWJ sequence is one grapheme.
    assert_eq!(agent_initial("👩‍🚀 Astronaut"), "👩‍🚀");
    assert_eq!(agent_initial(""), "?");
    assert_eq!(agent_initial("   "), "?");
}

#[test]
fn colliding_initials_are_told_apart_by_the_label() {
    let mercury = agent_at("Mercury", 0, 200.0, AgentState::Idle);
    let mars = agent_at("Mars", 1, 260.0, AgentState::Working);
    let rows = agent_rows(&model_with(vec![mercury, mars]));
    assert_eq!(rows[0].initial, rows[1].initial);
    assert_ne!(rows[0].a11y_label, rows[1].a11y_label);
}

#[test]
fn an_unnamed_agent_still_has_a_label() {
    let blank = agent_at("", 0, 200.0, AgentState::Idle);
    let row = agent_rows(&model_with(vec![blank])).remove(0);
    assert_eq!(row.initial, "?");
    assert_eq!(row.a11y_label, "Unnamed agent, idle");
}

fn repo(path: &str, minutes: i64) -> Repo {
    let start: DateTime<Utc> = "2026-11-20T09:00:00Z".parse().unwrap();
    Repo {
        id: RepoId::new(),
        path: path.into(),
        default_branch: "main".into(),
        created_at: start + Duration::minutes(minutes),
    }
}

#[test]
fn no_repository_needs_one_first() {
    assert_eq!(new_agent_plan(&[], None), NewAgentPlan::NeedRepo);
    assert_eq!(
        new_agent_plan(&[], Some(RepoId::new())),
        NewAgentPlan::NeedRepo
    );
}

#[test]
fn one_repository_creates_right_away() {
    let only = repo("/src/app", 0);
    assert_eq!(
        new_agent_plan(std::slice::from_ref(&only), None),
        NewAgentPlan::Create(only.id)
    );
    assert_eq!(
        new_agent_plan(std::slice::from_ref(&only), Some(RepoId::new())),
        NewAgentPlan::Create(only.id),
        "a stale last-used id does not matter"
    );
}

#[test]
fn several_repositories_preselect_the_last_used_one() {
    let (older, newer) = (repo("/src/old", 0), repo("/src/new", 10));
    let repos = [older.clone(), newer.clone()];
    assert_eq!(
        new_agent_plan(&repos, Some(older.id)),
        NewAgentPlan::Pick {
            repos: vec![older.id, newer.id],
            preselected: older.id,
        }
    );
}

#[test]
fn without_a_last_used_one_the_newest_repository_is_preselected() {
    let (older, newer) = (repo("/src/old", 0), repo("/src/new", 10));
    // The model's order is not the creation order.
    let repos = [newer.clone(), older.clone()];
    assert_eq!(
        new_agent_plan(&repos, None),
        NewAgentPlan::Pick {
            repos: vec![newer.id, older.id],
            preselected: newer.id,
        }
    );
}

#[test]
fn a_last_used_repository_that_is_gone_falls_back_to_the_newest() {
    let (a, b, c) = (repo("/a", 0), repo("/b", 30), repo("/c", 20));
    let repos = [a.clone(), b.clone(), c.clone()];
    assert_eq!(
        new_agent_plan(&repos, Some(RepoId::new())),
        NewAgentPlan::Pick {
            // The preselected first, then newest first.
            repos: vec![b.id, c.id, a.id],
            preselected: b.id,
        }
    );
}

#[test]
fn repositories_added_at_the_same_time_prefer_the_later_one() {
    let (first, second) = (repo("/a", 7), repo("/b", 7));
    let NewAgentPlan::Pick { preselected, .. } =
        new_agent_plan(&[first.clone(), second.clone()], None)
    else {
        panic!("two repositories pick");
    };
    assert_eq!(preselected, second.id);
}

#[test]
fn the_picker_shows_the_folder_name_and_the_full_path() {
    assert_eq!(
        repo_title("/Users/me/src/smithagents-rs".as_ref()),
        "smithagents-rs"
    );
    assert_eq!(repo_title("/Users/me/src/app/".as_ref()), "app");
    assert_eq!(repo_title("/".as_ref()), "/");
    assert_eq!(repo_title("".as_ref()), "");

    let (a, b) = (repo("/src/first", 0), repo("/src/second", 5));
    let repos = [a.clone(), b.clone()];
    let NewAgentPlan::Pick {
        repos: order,
        preselected,
    } = new_agent_plan(&repos, Some(a.id))
    else {
        panic!("two repositories pick");
    };
    let choices = repo_choices(&repos, &order, preselected);
    let shown: Vec<(&str, &str, bool)> = choices
        .iter()
        .map(|c| (c.title.as_str(), c.path.as_str(), c.preselected))
        .collect();
    assert_eq!(
        shown,
        [
            ("first", "/src/first", true),
            ("second", "/src/second", false)
        ]
    );
    assert_eq!(choices[0].id, a.id);
}

#[test]
fn command_errors_read_as_the_daemons_message() {
    assert_eq!(
        command_error_text(&LinkError::NotConnected),
        NOT_CONNECTED_TEXT
    );
    assert_eq!(NOT_CONNECTED_TEXT, "Not connected to the daemon");
    let api = LinkError::Client(ClientError::Api {
        status: 409,
        code: "worktree_failed".into(),
        message: "cannot create the worktree: branch exists".into(),
    });
    assert_eq!(
        command_error_text(&api),
        "cannot create the worktree: branch exists"
    );
    // A blank daemon message falls back to the whole error.
    let blank = LinkError::Client(ClientError::Api {
        status: 500,
        code: "http_500".into(),
        message: "  ".into(),
    });
    assert_eq!(command_error_text(&blank), blank.to_string());
    assert_eq!(
        command_error_text(&LinkError::Stopped),
        "The daemon link has stopped"
    );
}
