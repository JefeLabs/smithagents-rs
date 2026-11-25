//! The macOS menu bar, Quit, the tab shortcuts (⌘W, ctrl-tab,
//! ctrl-shift-tab) and the tab host's lifecycle (built by a factory once the
//! app exists, shut down once). Headless (`#[gpui_kit::test]`).
//!
//! Kit names are imported one by one: with `test-support` on, a glob import
//! would bring in GPUI's `test` attribute and shadow the built-in `#[test]`.

mod common;

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use common::ui::{agent, fake_connection, open_shell_options};
use gpui_kit::base::Root;
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{
    Action, App, AppContext as _, Entity, Keystroke, OwnedMenuItem, TestAppContext, WindowHandle,
};
use smithagents_app::layout_store::{saved_left_pane, LayoutStore};
use smithagents_app::model::{Action as ModelAction, Connection, ShellModel};
use smithagents_app::shell::{
    app_menus, CloseActiveTab, NextTab, PreviousTab, Quit, ShellOptions, ToggleAgentsPane,
    ToggleSettingsPane,
};
use smithagents_app::shell_state::ShellState;
use smithagents_app::tabs::SharedHost;
use smithagents_app::view_models::LeftPaneMode;
use smithagents_center_host::{CenterTabHost, HostCall, RecordingHost};
use smithagents_core::agent::Agent;
use smithagents_core::ids::AgentId;

/// A connected model with `agents`, none of them open in a tab.
fn connected(agents: &[Agent]) -> ShellModel {
    let mut model = ShellModel::new();
    model.apply(ModelAction::Snapshot {
        agents: agents.to_vec(),
        repos: vec![],
        latest_seq: 1,
    });
    model.apply(ModelAction::ConnectionChanged(Connection::Connected));
    model
}

struct Opened {
    window: WindowHandle<Root>,
    state: Entity<ShellState>,
    host: Rc<RefCell<RecordingHost>>,
    /// How many times the host factory ran.
    built: Rc<Cell<usize>>,
}

/// Opens the shell with a recording host (built by the factory), a fake
/// daemon connection and `layout`.
fn open(cx: &mut TestAppContext, agents: &[Agent], layout: Option<LayoutStore>) -> Opened {
    let host = SharedHost::new(RecordingHost::default());
    let handle = host.handle();
    let built = Rc::new(Cell::new(0));
    let count = built.clone();
    let (window, shell) = open_shell_options(
        cx,
        ShellOptions {
            model: connected(agents),
            layout,
            connection_info: Some(fake_connection()),
            tab_host: Some(Box::new(move |_: &mut App| {
                count.set(count.get() + 1);
                Box::new(host) as Box<dyn CenterTabHost>
            })),
            ..Default::default()
        },
        (1200., 800.),
    );
    let state = shell.read_with(cx, |shell, _| shell.state().clone());
    drop(shell);
    cx.update_window(window.into(), |_, window, cx| window.render_frame(cx))
        .unwrap();
    Opened {
        window,
        state,
        host: handle,
        built,
    }
}

fn shutdowns(opened: &Opened) -> usize {
    opened
        .host
        .borrow()
        .calls()
        .iter()
        .filter(|call| **call == HostCall::Shutdown)
        .count()
}

fn press(cx: &mut TestAppContext, window: WindowHandle<Root>, key: &str) {
    cx.update_window(window.into(), |_, window, cx| {
        window.press(key, cx);
        window.render_frame(cx);
    })
    .unwrap();
    cx.run_until_parked();
}

fn active(cx: &mut TestAppContext, state: &Entity<ShellState>) -> (Vec<AgentId>, Option<AgentId>) {
    state.read_with(cx, |state, _| {
        (state.model().tabs().to_vec(), state.model().active_tab())
    })
}

fn windows(cx: &mut TestAppContext) -> usize {
    cx.update(|cx| cx.windows().len())
}

#[gpui_kit::test]
fn the_menu_bar_has_quit_view_and_window_entries(cx: &mut TestAppContext) {
    let menus = cx.update(|cx| {
        cx.set_menus(app_menus());
        cx.get_menus().expect("the test platform keeps the menus")
    });
    let summary: Vec<(String, Vec<(String, &'static str)>)> = menus
        .iter()
        .map(|menu| {
            let items = menu
                .items
                .iter()
                .filter_map(|item| match item {
                    OwnedMenuItem::Action { name, action, .. } => {
                        Some((name.clone(), action.name()))
                    }
                    _ => None,
                })
                .collect();
            (menu.name.to_string(), items)
        })
        .collect();
    let item = |name: &str, action: &dyn Action| (name.to_owned(), action.name());
    assert_eq!(
        summary,
        [
            (
                "SmithAgents".to_owned(),
                vec![item("Quit SmithAgents", &Quit)]
            ),
            (
                "View".to_owned(),
                vec![
                    item("Toggle Agents Pane", &ToggleAgentsPane),
                    item("Toggle Settings", &ToggleSettingsPane),
                ]
            ),
            (
                "Window".to_owned(),
                vec![item("Close Tab", &CloseActiveTab)]
            ),
        ]
    );
}

#[gpui_kit::test]
fn every_menu_action_has_its_key(cx: &mut TestAppContext) {
    let _opened = open(cx, &[], None);
    cx.update(|cx| {
        let keymap = cx.key_bindings();
        let keymap = keymap.borrow();
        // What the menu shows: any binding of the action (the macOS menu
        // takes the first when none matches its default context).
        let bound = |action: &dyn Action, keys: &'static str| {
            let typed = [Keystroke::parse(keys).unwrap()];
            keymap
                .bindings_for_action(action)
                .any(|binding| binding.match_keystrokes(&typed) == Some(false))
        };
        assert!(bound(&Quit, "cmd-q"), "cmd-q");
        assert!(bound(&ToggleAgentsPane, "cmd-b"), "cmd-b");
        assert!(bound(&ToggleSettingsPane, "cmd-alt-b"), "cmd-alt-b");
        assert!(bound(&CloseActiveTab, "cmd-w"), "cmd-w");
        assert!(bound(&NextTab, "ctrl-tab"), "ctrl-tab");
        assert!(bound(&PreviousTab, "ctrl-shift-tab"), "ctrl-shift-tab");
    });
}

#[gpui_kit::test]
fn quit_closes_the_window_saves_the_layout_and_shuts_the_host_down_once(cx: &mut TestAppContext) {
    let home = tempfile::tempdir().unwrap();
    let store = LayoutStore::new(home.path());
    let opened = open(cx, &[], Some(store.clone()));
    assert_eq!(opened.built.get(), 1, "the factory ran once");
    // A change whose save is still debounced when Quit comes.
    opened.state.update(cx, |state, cx| {
        state.set_left_pane_mode(LeftPaneMode::Compact, cx)
    });
    let weak = opened.state.downgrade();

    cx.update(|cx| cx.dispatch_action(&Quit));
    cx.run_until_parked();

    assert_eq!(windows(cx), 0, "the window closed");
    let saved = store.load().expect("the layout was saved");
    assert_eq!(
        saved_left_pane(&saved).map(|left| left.mode),
        Some(LeftPaneMode::Compact)
    );
    assert_eq!(shutdowns(&opened), 1, "the host shut down");
    let Opened { state, host, .. } = opened;
    drop(state);
    assert!(weak.upgrade().is_none(), "the state (and its link) is gone");
    cx.quit();
    let count = host
        .borrow()
        .calls()
        .iter()
        .filter(|call| **call == HostCall::Shutdown)
        .count();
    assert_eq!(count, 1, "and only once");
}

#[gpui_kit::test]
fn the_host_shuts_down_once_when_the_window_closes_and_the_app_then_quits(cx: &mut TestAppContext) {
    let opened = open(cx, &[], None);
    assert_eq!(shutdowns(&opened), 0);
    cx.update_window(opened.window.into(), |_, window, _| window.remove_window())
        .unwrap();
    cx.run_until_parked();
    assert_eq!(shutdowns(&opened), 1, "on close");
    cx.quit();
    assert_eq!(shutdowns(&opened), 1, "not again at quit");
}

#[gpui_kit::test]
fn the_host_shuts_down_once_when_the_app_quits_with_the_window_open(cx: &mut TestAppContext) {
    let opened = open(cx, &[], None);
    cx.quit();
    assert_eq!(shutdowns(&opened), 1);
}

#[gpui_kit::test]
fn cmd_w_closes_the_active_tab_and_ctrl_tab_cycles_with_wrap(cx: &mut TestAppContext) {
    let agents = ["Mercury", "Venus", "Earth"].map(agent);
    let [a, b, c] = agents.clone().map(|agent| agent.id);
    let opened = open(cx, &agents, None);
    let window = opened.window;
    opened.state.update(cx, |state, cx| {
        for id in [a, b, c] {
            state.dispatch(ModelAction::OpenTab(id), cx);
        }
    });
    assert_eq!(active(cx, &opened.state), (vec![a, b, c], Some(c)));

    press(cx, window, "ctrl-tab");
    assert_eq!(active(cx, &opened.state).1, Some(a), "wraps forward");
    press(cx, window, "ctrl-shift-tab");
    assert_eq!(active(cx, &opened.state).1, Some(c), "wraps back");
    press(cx, window, "ctrl-shift-tab");
    assert_eq!(active(cx, &opened.state).1, Some(b));

    press(cx, window, "cmd-w");
    assert_eq!(active(cx, &opened.state), (vec![a, c], Some(a)));
    press(cx, window, "cmd-w");
    press(cx, window, "cmd-w");
    assert_eq!(active(cx, &opened.state), (vec![], None));
    let mut available = true;
    cx.update_window(window.into(), |_, window, cx| {
        available = window.is_action_available(&CloseActiveTab, cx);
    })
    .unwrap();
    assert!(!available, "Close Tab is off without a tab");

    // Without a tab ⌘W does nothing, and the window stays.
    press(cx, window, "cmd-w");
    press(cx, window, "ctrl-tab");
    assert_eq!(windows(cx), 1);
    assert_eq!(active(cx, &opened.state), (vec![], None));
}

#[gpui_kit::test]
fn quit_leaves_the_daemon_running(cx: &mut TestAppContext) {
    use rustix::process::{test_kill_process, Pid};
    use smithagents_app::link::LinkConfig;
    use smithagents_client::SpawnOptions;
    use smithagents_core::daemon::read_lockfile;
    use std::time::{Duration, Instant};

    let home = common::TestHome::new();
    let mut link = LinkConfig::new(
        home.path().to_owned(),
        common::daemon_bin().to_owned(),
        env!("CARGO_PKG_VERSION").to_owned(),
    );
    link.spawn = SpawnOptions {
        args: vec!["--claude-bin".into(), common::fake_claude().into()],
        ..SpawnOptions::default()
    };
    let (_window, shell) = open_shell_options(
        cx,
        ShellOptions {
            link: Some(link),
            ..Default::default()
        },
        (1200., 800.),
    );
    let state = shell.read_with(cx, |shell, _| shell.state().clone());
    drop(shell);
    // The link connects on its own thread.
    let deadline = Instant::now() + Duration::from_secs(30);
    while state.read_with(cx, |state, _| {
        state
            .link()
            .and_then(|link| link.connection_info())
            .is_none()
    }) {
        assert!(Instant::now() < deadline, "the link never connected");
        std::thread::sleep(Duration::from_millis(50));
    }
    let pid = read_lockfile(home.path()).unwrap().expect("a daemon").pid;
    let weak = state.downgrade();
    drop(state);

    cx.update(|cx| cx.dispatch_action(&Quit));
    cx.run_until_parked();

    assert_eq!(windows(cx), 0);
    assert!(weak.upgrade().is_none(), "the shell and its link are gone");
    let pid = Pid::from_raw(i32::try_from(pid).unwrap()).unwrap();
    assert!(test_kill_process(pid).is_ok(), "the daemon still runs");
    assert_eq!(
        read_lockfile(home.path()).unwrap().map(|info| info.pid),
        Some(pid.as_raw_nonzero().get() as u32),
        "and still owns its lockfile"
    );
}
