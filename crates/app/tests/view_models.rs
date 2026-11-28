use chrono::{DateTime, Duration, Utc};
use smithagents_app::link::{LinkError, LinkErrorKind, LinkFailure};
use smithagents_app::model::{Action, Connection, ShellModel};
use smithagents_app::tabs::TabError;
use smithagents_app::view_models::{
    agent_initial, agent_rows, banner_text, center_rect, command_error_text, connection_banner,
    new_agent_plan, repo_choices, repo_title, state_dot, state_kind, state_label, state_token,
    tab_error_text, tab_title, BannerSeverity, DotStyle, LeftPaneMode, NewAgentPlan, StateKind,
    DAEMON_SILENT_TEXT, NOT_CONNECTED_TEXT, OPEN_CHAT_FAILED,
};
use smithagents_center_host::HostError;
use smithagents_client::ClientError;
use smithagents_core::agent::{Agent, AgentState, Repo, SourceControl};
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
        branch: Some(format!("agent/{name}")),
        harness_session_id: None,
        state: AgentState::Idle,
        created_at: at,
        closed_at: None,
        intent_id: None,
    }
}

/// A link failure other than a version mismatch.
fn failed(message: &str) -> LinkFailure {
    LinkFailure::Other {
        message: message.into(),
        kind: LinkErrorKind::Unreachable,
    }
}

#[test]
fn no_banner_while_connected() {
    assert_eq!(banner_text(&Connection::Connected, None), None);
    assert_eq!(
        banner_text(&Connection::Connected, Some(&failed("stale error"))),
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
            Some(&failed("could not start /x/smithagentsd: not found"))
        )
        .as_deref(),
        Some("Reconnecting to the local daemon…\ncould not start /x/smithagentsd: not found")
    );
    assert_eq!(
        banner_text(&Connection::Connecting, Some(&failed("refused"))).as_deref(),
        Some("Connecting to the local daemon…\nrefused")
    );
}

#[test]
fn a_blank_error_adds_no_line_and_the_error_is_trimmed() {
    assert_eq!(
        banner_text(&Connection::Reconnecting, Some(&failed("  \n "))).as_deref(),
        Some("Reconnecting to the local daemon…")
    );
    assert_eq!(
        banner_text(&Connection::Connecting, Some(&failed("  refused\n"))).as_deref(),
        Some("Connecting to the local daemon…\nrefused")
    );
}

#[test]
fn a_protocol_mismatch_reads_as_a_version_mismatch_that_names_both_protocols() {
    // Builds share a version; what differs between them is the protocol.
    let failure = LinkFailure::protocol_mismatch("0.1.0", 0, 1);
    assert_eq!(failure.kind(), LinkErrorKind::VersionMismatch);
    let banner = connection_banner(&Connection::Reconnecting, Some(&failure)).unwrap();
    assert_eq!(
        banner.text,
        "A different SmithAgents daemon version is running (0.1.0, protocol 0; this app \
         needs 0.1.0, protocol 1). Quit it to continue."
    );
    assert_eq!(banner.severity, BannerSeverity::Error);
}

#[test]
fn a_version_mismatch_names_both_versions_and_is_an_error_not_a_reconnect() {
    let failure = LinkFailure::VersionMismatch {
        running: "0.0.9".into(),
        expected: "0.1.0".into(),
    };
    assert_eq!(failure.kind(), LinkErrorKind::VersionMismatch);
    let wording = "A different SmithAgents daemon version is running (0.0.9; this app \
                   needs 0.1.0). Quit it to continue.";
    for connection in [Connection::Connecting, Connection::Reconnecting] {
        let banner = connection_banner(&connection, Some(&failure)).unwrap();
        assert_eq!(banner.text, wording, "{connection:?}");
        assert_eq!(banner.severity, BannerSeverity::Error);
        assert!(!banner.text.contains("onnecting"), "{}", banner.text);
    }
    assert_eq!(
        connection_banner(&Connection::Connected, Some(&failure)),
        None
    );
    // Any other failure is a warning under the connection's own line.
    let other = connection_banner(&Connection::Reconnecting, Some(&failed("refused"))).unwrap();
    assert_eq!(other.severity, BannerSeverity::Warning);
    assert_eq!(other.text, "Reconnecting to the local daemon…\nrefused");
    assert_eq!(
        connection_banner(&Connection::Connecting, None)
            .unwrap()
            .severity,
        BannerSeverity::Warning
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
        source_control: Some(SourceControl::Git {
            default_branch: "main".into(),
        }),
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

// ---- Status bar --------------------------------------------------------------

mod status_bar {
    use smithagents_app::model::Connection;
    use smithagents_app::update::UpdateState;
    use smithagents_app::view_models::{
        repository_added_text, status_to_show, update_button, version_text, StatusKind,
        StatusMessage, UpdateButtonVm,
    };

    fn info(text: &str) -> StatusMessage {
        StatusMessage {
            text: text.into(),
            kind: StatusKind::Info,
        }
    }

    fn error(text: &str) -> StatusMessage {
        StatusMessage {
            text: text.into(),
            kind: StatusKind::Error,
        }
    }

    #[test]
    fn nothing_shows_when_connected_and_idle() {
        assert_eq!(status_to_show(&Connection::Connected, None, None), None);
        // A stale link error never shows while connected.
        assert_eq!(
            status_to_show(
                &Connection::Connected,
                Some(&super::failed("refused")),
                None
            ),
            None
        );
    }

    #[test]
    fn the_transient_message_shows_while_connected() {
        let msg = info("Agent Mercury created");
        assert_eq!(
            status_to_show(&Connection::Connected, None, Some(&msg)),
            Some(msg.clone())
        );
        let msg = error("boom");
        assert_eq!(
            status_to_show(&Connection::Connected, None, Some(&msg)),
            Some(msg.clone())
        );
    }

    #[test]
    fn the_connection_wins_over_a_transient_message() {
        let msg = error("boom");
        assert_eq!(
            status_to_show(&Connection::Connecting, None, Some(&msg)),
            Some(info("Connecting to the local daemon…"))
        );
        assert_eq!(
            status_to_show(&Connection::Reconnecting, None, Some(&msg)),
            Some(info("Reconnecting to the local daemon…"))
        );
    }

    #[test]
    fn the_connection_text_carries_the_link_error_on_one_line() {
        let shown = status_to_show(
            &Connection::Reconnecting,
            Some(&super::failed("  refused \n")),
            None,
        )
        .unwrap();
        assert_eq!(shown.kind, StatusKind::Info);
        assert_eq!(shown.text, "Reconnecting to the local daemon…: refused");
        assert!(!shown.text.contains('\n'));
        // A blank error adds nothing.
        assert_eq!(
            status_to_show(&Connection::Connecting, Some(&super::failed("  ")), None)
                .unwrap()
                .text,
            "Connecting to the local daemon…"
        );
    }

    #[test]
    fn a_version_mismatch_is_an_error_in_the_status_bar() {
        let failure = smithagents_app::link::LinkFailure::VersionMismatch {
            running: "0.0.9".into(),
            expected: "0.1.0".into(),
        };
        for connection in [Connection::Connecting, Connection::Reconnecting] {
            let shown = status_to_show(&connection, Some(&failure), Some(&info("hi"))).unwrap();
            assert_eq!(shown.kind, StatusKind::Error);
            assert_eq!(
                shown.text,
                "A different SmithAgents daemon version is running (0.0.9; this app needs \
                 0.1.0). Quit it to continue."
            );
        }
    }

    #[test]
    fn a_message_is_one_line_whatever_the_whitespace() {
        assert_eq!(StatusMessage::error("a\nb\r\n\n  c\t d ").text, "a b c d");
        assert_eq!(StatusMessage::info("  one\ntwo ").text, "one two");
    }

    #[test]
    fn the_version_text_has_a_v_prefix() {
        assert_eq!(version_text("0.1.0"), "v0.1.0");
    }

    #[test]
    fn no_update_button_unless_one_is_available() {
        assert_eq!(update_button(&UpdateState::UpToDate), None);
    }

    #[test]
    fn the_update_button_follows_the_update_state() {
        let v = || "1.2.3".to_owned();
        assert_eq!(
            update_button(&UpdateState::Available { version: v() }),
            Some(UpdateButtonVm {
                label: "Update available".into(),
                enabled: true,
                a11y_label: "Update available: v1.2.3".into(),
            })
        );
        let installing = update_button(&UpdateState::Installing { version: v() }).unwrap();
        assert_eq!(installing.label, "Installing…");
        assert!(!installing.enabled);
        assert!(installing.a11y_label.contains("v1.2.3"));
        let failed = update_button(&UpdateState::Failed {
            version: v(),
            message: "boom".into(),
        })
        .unwrap();
        assert_eq!(failed.label, "Update failed, retry");
        assert!(failed.enabled);
        assert!(failed.a11y_label.contains("v1.2.3"));
        let restart = update_button(&UpdateState::RestartRequired { version: v() }).unwrap();
        assert_eq!(restart.label, "Restart to update");
        assert_eq!(restart.a11y_label, "Restart to update: v1.2.3");
    }

    #[test]
    fn repository_added_names_the_last_path_component() {
        assert_eq!(
            repository_added_text(std::path::Path::new("/src/my-app")),
            "Repository added: my-app"
        );
        assert_eq!(
            repository_added_text(std::path::Path::new("/src/my-app/")),
            "Repository added: my-app"
        );
        assert_eq!(
            repository_added_text(std::path::Path::new("/")),
            "Repository added: /"
        );
    }
}

// ---- Settings pane -----------------------------------------------------------

mod settings_pane {
    use std::path::{Path, PathBuf};

    use chrono::{DateTime, Utc};
    use smithagents_app::link::LinkError;
    use smithagents_app::view_models::{
        font_options, nearest_step, repo_error_text, repo_matches_sent_path, repo_rows,
        stepped_text_scale, text_scale_label, text_scale_steps, theme_options,
        validate_repo_path_input_in, FontOptionVm, RepoRowVm, SettingsSection, ThemeOptionVm,
        ABSOLUTE_PATH_TEXT, CLONE_UNSUPPORTED_TEXT, DAEMON_UNREACHABLE_TEXT, ENTER_A_PATH_TEXT,
        NOT_CONNECTED_TEXT, REPO_ADD_FAILED_TEXT, SYSTEM_THEME_LABEL,
    };
    use smithagents_client::ClientError;
    use smithagents_core::agent::{Repo, SourceControl};
    use smithagents_core::ids::RepoId;
    use smithagents_theme::state::ThemeChoice;
    use smithagents_theme::tokens::{TextScale, ThemeSet};

    fn set() -> &'static ThemeSet {
        ThemeSet::embedded()
    }

    fn shown(options: &[ThemeOptionVm]) -> Vec<(&str, bool)> {
        options
            .iter()
            .map(|o| (o.label.as_str(), o.selected))
            .collect()
    }

    #[test]
    fn theme_options_list_system_first_then_each_theme_by_name() {
        let options = theme_options(set(), &ThemeChoice::System);
        assert_eq!(SYSTEM_THEME_LABEL, "System");
        assert_eq!(
            shown(&options),
            [
                ("System", true),
                ("Smith Dark", false),
                ("Smith Light", false)
            ]
        );
        assert_eq!(options[0].choice, ThemeChoice::System);
        assert_eq!(options[1].choice, ThemeChoice::Id("smith-dark".into()));
        assert_eq!(options[2].choice, ThemeChoice::Id("smith-light".into()));

        let light = theme_options(set(), &ThemeChoice::Id("smith-light".into()));
        assert_eq!(
            shown(&light),
            [
                ("System", false),
                ("Smith Dark", false),
                ("Smith Light", true)
            ]
        );
    }

    #[test]
    fn a_stale_saved_theme_selects_system() {
        let options = theme_options(set(), &ThemeChoice::Id("retired-theme".into()));
        assert_eq!(
            shown(&options),
            [
                ("System", true),
                ("Smith Dark", false),
                ("Smith Light", false)
            ]
        );
        assert_eq!(options.iter().filter(|o| o.selected).count(), 1);
    }

    fn scale(min: f64, max: f64, default: f64) -> TextScale {
        TextScale { min, max, default }
    }

    #[test]
    fn text_steps_run_from_min_to_max_by_five_hundredths() {
        let steps = text_scale_steps(&set().base.text_scale);
        assert_eq!(
            steps,
            [0.85, 0.9, 0.95, 1.0, 1.05, 1.1, 1.15, 1.2, 1.25, 1.3, 1.35, 1.4, 1.45, 1.5]
        );
    }

    #[test]
    fn text_steps_stay_in_bounds_and_always_hold_the_default() {
        for bounds in [
            scale(0.85, 1.5, 1.0),
            scale(0.8, 1.23, 1.13),
            scale(0.9, 0.9, 0.9),
            scale(0.71, 1.0, 0.71),
        ] {
            let steps = text_scale_steps(&bounds);
            assert!(!steps.is_empty(), "{bounds:?}");
            assert!(
                steps
                    .iter()
                    .all(|s| *s >= bounds.min - 1e-9 && *s <= bounds.max + 1e-9),
                "{bounds:?}: {steps:?}"
            );
            assert!(steps.contains(&bounds.default), "{bounds:?}: {steps:?}");
            assert!(steps.windows(2).all(|w| w[0] < w[1]), "sorted, no repeats");
            for s in &steps {
                assert_eq!(*s, (s * 100.0).round() / 100.0, "two decimals: {s}");
            }
        }
        let odd = text_scale_steps(&scale(0.8, 1.23, 1.13));
        assert_eq!(odd.first(), Some(&0.8));
        assert_eq!(odd.last(), Some(&1.23), "the max is reachable");
    }

    #[test]
    fn broken_text_bounds_give_only_the_default() {
        assert_eq!(text_scale_steps(&scale(1.5, 0.85, 1.0)), [1.0]);
        assert_eq!(text_scale_steps(&scale(f64::NAN, 1.5, 1.0)), [1.0]);
        assert_eq!(text_scale_steps(&scale(0.85, 1.5, f64::NAN)).len(), 14);
    }

    #[test]
    fn the_nearest_step_falls_back_to_the_default() {
        let steps = text_scale_steps(&set().base.text_scale);
        assert_eq!(nearest_step(&steps, 1.0, 1.0), 1.0);
        assert_eq!(nearest_step(&steps, 1.02, 1.0), 1.0);
        assert_eq!(nearest_step(&steps, 1.03, 1.0), 1.05);
        assert_eq!(nearest_step(&steps, 0.85, 1.0), 0.85);
        assert_eq!(nearest_step(&steps, 1.5, 1.0), 1.5);
        assert_eq!(nearest_step(&steps, f64::NAN, 1.0), 1.0, "NaN");
        assert_eq!(nearest_step(&steps, f64::INFINITY, 1.0), 1.0);
        assert_eq!(nearest_step(&steps, 0.5, 1.0), 1.0, "below min");
        assert_eq!(nearest_step(&steps, 2.0, 1.0), 1.0, "above max");
        assert_eq!(nearest_step(&[], 1.2, 1.0), 1.0);
    }

    #[test]
    fn a_step_up_or_down_stops_at_the_ends() {
        let steps = text_scale_steps(&set().base.text_scale);
        assert_eq!(stepped_text_scale(&steps, 1.0, true), Some(1.05));
        assert_eq!(stepped_text_scale(&steps, 1.0, false), Some(0.95));
        assert_eq!(stepped_text_scale(&steps, 1.5, true), None);
        assert_eq!(stepped_text_scale(&steps, 0.85, false), None);
        // From between two steps, the nearest one counts as the current one.
        assert_eq!(stepped_text_scale(&steps, 1.04, true), Some(1.1));
        assert_eq!(stepped_text_scale(&[], 1.0, true), None);
    }

    #[test]
    fn the_text_size_reads_as_a_percentage() {
        assert_eq!(text_scale_label(1.0), "100%");
        assert_eq!(text_scale_label(0.85), "85%");
        assert_eq!(text_scale_label(1.15), "115%");
        assert_eq!(text_scale_label(1.5), "150%");
    }

    #[test]
    fn font_options_follow_the_set_and_an_unknown_font_selects_the_first() {
        let shown = |current: &str| -> Vec<FontOptionVm> { font_options(set(), current) };
        assert_eq!(
            shown("mono"),
            [
                FontOptionVm {
                    id: "system".into(),
                    label: "System".into(),
                    selected: false,
                },
                FontOptionVm {
                    id: "mono".into(),
                    label: "Monospace".into(),
                    selected: true,
                },
            ]
        );
        let unknown = shown("comic-sans");
        assert!(unknown[0].selected);
        assert!(!unknown[1].selected);
        let mut empty = set().clone();
        empty.base.fonts.clear();
        assert!(font_options(&empty, "system").is_empty());
    }

    fn repo(path: &str, branch: &str) -> Repo {
        let at: DateTime<Utc> = "2026-11-20T09:00:00Z".parse().unwrap();
        Repo {
            id: RepoId::new(),
            path: path.into(),
            source_control: Some(SourceControl::Git {
                default_branch: branch.into(),
            }),
            created_at: at,
        }
    }

    #[test]
    fn repo_rows_are_sorted_by_name_then_path_and_keep_unicode_and_spaces() {
        let repos = vec![
            repo("/src/Zeta", "main"),
            repo("/src/ünïcode répo", "trunk"),
            repo("/src/alpha", "main"),
            repo("/work/My Project/", "develop"),
            repo("/other/alpha", "main"),
        ];
        let rows = repo_rows(&repos, None);
        let shown: Vec<(&str, &str, &str)> = rows
            .iter()
            .map(|r| (r.name.as_str(), r.path.as_str(), r.source_control.as_str()))
            .collect();
        assert_eq!(
            shown,
            [
                (
                    "alpha",
                    "/other/alpha",
                    "Source control: Git, default branch main"
                ),
                (
                    "alpha",
                    "/src/alpha",
                    "Source control: Git, default branch main"
                ),
                (
                    "My Project",
                    "/work/My Project/",
                    "Source control: Git, default branch develop"
                ),
                (
                    "Zeta",
                    "/src/Zeta",
                    "Source control: Git, default branch main"
                ),
                (
                    "ünïcode répo",
                    "/src/ünïcode répo",
                    "Source control: Git, default branch trunk"
                ),
            ]
        );
        assert_eq!(rows[4].id, repos[1].id);
        let root = repo("/", "main");
        assert_eq!(
            repo_rows(std::slice::from_ref(&root), None),
            [RepoRowVm {
                id: root.id,
                name: "/".into(),
                path: "/".into(),
                source_control: "Source control: Git, default branch main".into(),
                has_source_control: true,
                is_meta: true,
                can_set_meta: false,
            }]
        );
    }

    fn plain(path: &str) -> Repo {
        Repo {
            source_control: None,
            ..repo(path, "main")
        }
    }

    #[test]
    fn a_repository_row_says_its_source_control() {
        let (folder, git_main, git_trunk) = (
            plain("/src/a-folder"),
            repo("/src/b-main", "main"),
            repo("/src/c-trunk", "trunk"),
        );
        let repos = vec![folder.clone(), git_main.clone(), git_trunk.clone()];
        let settings = smithagents_core::feedback::FeedbackSettings {
            meta_repo_id: Some(git_main.id),
        };
        let rows = repo_rows(&repos, Some(&settings));
        let row = |repo: &Repo| rows.iter().find(|r| r.id == repo.id).unwrap().clone();
        assert_eq!(row(&folder).source_control, "Source control: None");
        assert_eq!(
            row(&git_main).source_control,
            "Source control: Git, default branch main"
        );
        assert_eq!(
            row(&git_trunk).source_control,
            "Source control: Git, default branch trunk"
        );
        let folder_row = row(&folder);
        assert!(!folder_row.can_set_meta && !folder_row.is_meta && !folder_row.has_source_control);
        assert!(row(&git_main).is_meta && row(&git_main).has_source_control);
        assert!(row(&git_trunk).can_set_meta && row(&git_trunk).has_source_control);

        // With one repository under source control there is nothing to
        // choose: it is the meta repository, and the folder never is.
        let repos = vec![folder.clone(), git_main.clone()];
        for settings in [None, Some(Default::default()), Some(settings.clone())] {
            let rows = repo_rows(&repos, settings.as_ref());
            let flags: Vec<_> = rows
                .iter()
                .map(|r| (r.id, r.is_meta, r.can_set_meta))
                .collect();
            assert_eq!(
                flags,
                [(folder.id, false, false), (git_main.id, true, false)],
                "{settings:?}"
            );
        }
        // A stored id that names the folder counts as nothing stored.
        let on_folder = smithagents_core::feedback::FeedbackSettings {
            meta_repo_id: Some(folder.id),
        };
        let rows = repo_rows(std::slice::from_ref(&folder), Some(&on_folder));
        assert!(!rows[0].is_meta && !rows[0].can_set_meta);
    }

    #[test]
    fn repo_rows_mark_the_meta_repository_and_what_can_be_chosen() {
        let repos = vec![repo("/src/a", "main"), repo("/src/b", "main")];
        let flags = |settings: Option<&smithagents_core::feedback::FeedbackSettings>| {
            repo_rows(&repos, settings)
                .iter()
                .map(|r| (r.is_meta, r.can_set_meta))
                .collect::<Vec<_>>()
        };
        let stored = |id| smithagents_core::feedback::FeedbackSettings {
            meta_repo_id: Some(id),
        };
        assert_eq!(flags(None), [(false, false), (false, false)]);
        assert_eq!(
            flags(Some(&Default::default())),
            [(false, true), (false, true)]
        );
        assert_eq!(
            flags(Some(&stored(repos[1].id))),
            [(false, true), (true, false)]
        );
        // A stored id whose repository is gone is nothing stored.
        let gone = stored(smithagents_core::ids::RepoId::new());
        assert_eq!(flags(Some(&gone)), [(false, true), (false, true)]);
        let one = &repos[..1];
        assert_eq!(
            repo_rows(one, Some(&Default::default()))
                .iter()
                .map(|r| (r.is_meta, r.can_set_meta))
                .collect::<Vec<_>>(),
            [(true, false)]
        );
    }

    fn api(status: u16, code: &str, message: &str) -> LinkError {
        LinkError::Client(ClientError::Api {
            status,
            code: code.into(),
            message: message.into(),
        })
    }

    /// The daemon's real answers to an add: its codes (`api.rs`) and its
    /// messages, which carry the `ServiceError` prefix (`service.rs`:
    /// `"git: {0}"`, `"store: {0}"`) before the `GitError` / `StoreError`
    /// text (`git.rs`, `store.rs`).
    const DAEMON_ANSWERS: [(u16, &str, &str, &str); 5] = [
        (
            400,
            "path_not_found",
            "git: no directory at /tmp/missing dir",
            "No directory at that path.",
        ),
        (
            400,
            "not_a_git_repo",
            "git: not a git repository",
            "That directory is not a git repository.",
        ),
        (
            400,
            "bare_repo",
            "git: bare repositories are not supported",
            "Bare repositories are not supported.",
        ),
        (
            400,
            "no_default_branch",
            "git: could not determine the default branch",
            "Could not determine the repository's default branch.",
        ),
        (
            409,
            "repo_exists",
            "store: a repo with this path already exists",
            "That repository is already added.",
        ),
    ];

    #[test]
    fn a_known_daemon_code_reads_as_our_own_sentence() {
        for (status, code, message, want) in DAEMON_ANSWERS {
            let text = repo_error_text(&api(status, code, message));
            assert_eq!(text, want, "{code}");
            assert!(!text.contains("git:") && !text.contains("store:"), "{text}");
        }
        // The sentence does not depend on the message.
        assert_eq!(
            repo_error_text(&api(400, "not_a_git_repo", " ")),
            "That directory is not a git repository."
        );
    }

    #[test]
    fn each_new_refusal_has_its_sentence() {
        // The codes of a plain folder and of turning source control on or
        // off, at the status the daemon answers each with
        // (`ServiceError::status_and_code`).
        for (status, code, want) in [
            (
                409,
                "folder_busy",
                "Another agent is working in this folder.",
            ),
            (409, "repo_in_use", "Close this repository's agents first."),
            (
                409,
                "repo_overlaps",
                "That folder overlaps another repository of this Workspace.",
            ),
            (
                409,
                "repo_has_feedback_intents",
                "Resolve this repository's feedback intents first.",
            ),
            (
                409,
                "needs_source_control",
                "That needs a repository with source control.",
            ),
            (400, "git_not_installed", "git is not installed."),
            (
                400,
                "git_identity_missing",
                "git has no name and email to commit with. Set them, then try again.",
            ),
            (
                409,
                "folder_not_empty",
                "A new repository needs a folder that is empty or does not exist yet.",
            ),
            (
                400,
                "not_repo_root",
                "That folder is inside a git repository. Add the repository's own folder.",
            ),
        ] {
            assert_eq!(repo_error_text(&api(status, code, "x")), want, "{code}");
        }
    }

    #[test]
    fn another_daemon_error_shows_its_message_without_the_internal_prefix() {
        for (code, message, want) in [
            (
                "git_error",
                "git: git command failed: fatal: bad object HEAD",
                "Git command failed: fatal: bad object HEAD",
            ),
            (
                "store_error",
                "store: database error: disk I/O error",
                "Database error: disk I/O error",
            ),
            (
                "invalid",
                "invalid JSON: EOF while parsing",
                "Invalid JSON: EOF while parsing",
            ),
            ("internal", "internal error", "Internal error"),
            // Whitespace never makes a second line.
            (
                "git_error",
                "  git:  git command\nfailed \n",
                "Git command failed",
            ),
        ] {
            assert_eq!(
                repo_error_text(&api(500, code, message)),
                want,
                "{message:?}"
            );
        }
    }

    #[test]
    fn a_blank_daemon_message_without_a_sentence_reads_as_a_generic_line() {
        assert_eq!(
            REPO_ADD_FAILED_TEXT,
            "The daemon could not add the repository."
        );
        for message in ["", "  ", "git: ", "store:"] {
            assert_eq!(
                repo_error_text(&api(500, "http_502", message)),
                REPO_ADD_FAILED_TEXT,
                "{message:?}"
            );
        }
    }

    #[test]
    fn a_late_added_repo_matches_the_path_sent() {
        let sent = Path::new("/tmp/My Repo/sub");
        let canonical = Path::new("/private/tmp/My Repo/sub");
        // The same path.
        assert!(repo_matches_sent_path(sent, None, sent));
        // The daemon records the git top level of the canonical path.
        assert!(repo_matches_sent_path(
            sent,
            Some(canonical),
            Path::new("/private/tmp/My Repo")
        ));
        assert!(repo_matches_sent_path(sent, Some(canonical), canonical));
        // Another repository does not match.
        assert!(!repo_matches_sent_path(
            sent,
            Some(canonical),
            Path::new("/private/tmp/My Repo 2")
        ));
        assert!(!repo_matches_sent_path(
            sent,
            None,
            Path::new("/tmp/My Repo")
        ));
    }

    #[test]
    fn connection_and_transport_errors_read_as_generic_lines() {
        assert_eq!(
            repo_error_text(&LinkError::NotConnected),
            NOT_CONNECTED_TEXT
        );
        assert_eq!(
            repo_error_text(&LinkError::Stopped),
            "The daemon link has stopped"
        );
        assert_eq!(DAEMON_UNREACHABLE_TEXT, "Could not reach the daemon");
        // A transport error (an HTTP request that failed before an answer).
        let http = reqwest::Client::new()
            .get("not a url")
            .build()
            .expect_err("an invalid URL is a reqwest error");
        let http = LinkError::Client(ClientError::Http(http));
        assert_eq!(repo_error_text(&http), DAEMON_UNREACHABLE_TEXT);
        assert!(!repo_error_text(&http).contains(&http.to_string()));
        let ws = LinkError::Client(ClientError::Ws(
            tokio_tungstenite::tungstenite::Error::ConnectionClosed,
        ));
        assert_eq!(repo_error_text(&ws), DAEMON_UNREACHABLE_TEXT);
    }

    #[test]
    fn an_unauthorized_answer_never_shows_a_token() {
        // The daemon's 401 message names no token; the text is its message.
        let text = repo_error_text(&api(401, "unauthorized", "missing or invalid bearer token"));
        assert_eq!(text, "Missing or invalid bearer token");
    }

    fn validate(raw: &str) -> Result<PathBuf, &'static str> {
        validate_repo_path_input_in(raw, Some(Path::new("/Users/me")))
    }

    #[test]
    fn a_path_is_trimmed_and_must_not_be_empty() {
        assert_eq!(ENTER_A_PATH_TEXT, "Enter a path");
        assert_eq!(validate(""), Err(ENTER_A_PATH_TEXT));
        assert_eq!(validate("  \t "), Err(ENTER_A_PATH_TEXT));
        assert_eq!(validate("  /src/app  "), Ok(PathBuf::from("/src/app")));
        assert_eq!(
            validate("/tmp/ünïcode répo"),
            Ok(PathBuf::from("/tmp/ünïcode répo"))
        );
    }

    #[test]
    fn a_leading_tilde_is_the_home_directory() {
        assert_eq!(
            validate("~/code/my repo"),
            Ok(PathBuf::from("/Users/me/code/my repo"))
        );
        assert_eq!(validate("~"), Ok(PathBuf::from("/Users/me")));
        assert_eq!(validate("~/"), Ok(PathBuf::from("/Users/me")));
        // Another user's home is not expanded.
        assert_eq!(validate("~other/app"), Err(ABSOLUTE_PATH_TEXT));
        // Without a known home, a tilde path cannot be made absolute.
        assert_eq!(
            validate_repo_path_input_in("~/app", None),
            Err(ABSOLUTE_PATH_TEXT)
        );
    }

    #[test]
    fn a_relative_path_asks_for_an_absolute_one() {
        assert_eq!(ABSOLUTE_PATH_TEXT, "Enter an absolute path");
        for raw in ["src/app", "./app", "../app", "app"] {
            assert_eq!(validate(raw), Err(ABSOLUTE_PATH_TEXT), "{raw}");
        }
    }

    #[test]
    fn a_url_says_cloning_is_not_supported_yet() {
        assert_eq!(
            CLONE_UNSUPPORTED_TEXT,
            "Cloning from a URL is not supported yet. Enter a local path."
        );
        for raw in [
            "https://github.com/acme/app.git",
            "http://example.com/app",
            "git@github.com:acme/app.git",
            "  ssh://git@host/app ",
            "git://host/app",
            "HTTPS://GITHUB.COM/acme/app",
        ] {
            assert_eq!(validate(raw), Err(CLONE_UNSUPPORTED_TEXT), "{raw}");
        }
    }

    #[test]
    fn settings_sections_round_trip_their_persisted_names() {
        for section in SettingsSection::ALL {
            assert_eq!(SettingsSection::parse(section.as_str()), Some(section));
        }
        assert_eq!(SettingsSection::Appearance.as_str(), "appearance");
        assert_eq!(SettingsSection::AgentDefaults.as_str(), "agent_defaults");
        assert_eq!(SettingsSection::Repositories.as_str(), "repositories");
        assert_eq!(SettingsSection::Appearance.title(), "Appearance");
        assert_eq!(SettingsSection::AgentDefaults.title(), "Agent defaults");
        assert_eq!(SettingsSection::Repositories.title(), "Source repositories");
        assert_eq!(SettingsSection::parse("agent-defaults"), None);
        assert_eq!(
            SettingsSection::ALL,
            [
                SettingsSection::Appearance,
                SettingsSection::AgentDefaults,
                SettingsSection::Repositories
            ]
        );
        // The scroll index is the position in `ALL`, the pane's order.
        for (ix, section) in SettingsSection::ALL.into_iter().enumerate() {
            assert_eq!(section.index(), ix);
        }
    }
}

#[test]
fn a_tab_that_did_not_open_reads_as_a_sentence() {
    assert_eq!(OPEN_CHAT_FAILED, "Cannot open the chat");
    assert_eq!(tab_error_text(&TabError::NotConnected), NOT_CONNECTED_TEXT);
    assert_eq!(tab_error_text(&TabError::UnknownAgent), "The agent is gone");
    assert_eq!(
        tab_error_text(&TabError::Host(HostError::Engine(
            "the engine is down".into()
        ))),
        "Web engine error: the engine is down"
    );
    assert!(tab_error_text(&TabError::Host(HostError::InvalidOrigin))
        .starts_with("Page origin must be smithagents://app"));
    // The status line keeps the error's own words after the title.
    assert_eq!(
        format!("{OPEN_CHAT_FAILED}: {}", TabError::NotConnected),
        "Cannot open the chat: not connected to the daemon"
    );
}
