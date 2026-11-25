//! Headless checks of the Settings view in the right dock: Appearance (live
//! theme, text size and font changes, saved to `app-settings.json`) and
//! Source repositories (the list and the add flow). Commands go to a fake and
//! the OS appearance is a fake, so no daemon runs and the test flips "dark
//! mode" itself.
//!
//! Kit names are imported one by one: with `test-support` on, a glob import
//! would bring in GPUI's `test` attribute and shadow the built-in `#[test]`.

mod common;

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use common::ui::{
    dark_native, open_shell_options, painted_at, theme_state, token_background, FakeAppearance,
};
use futures::channel::oneshot;
use futures::future::LocalBoxFuture;
use futures::FutureExt as _;
use gpui_kit::base::Root;
use gpui_kit::component::dock::DockPlacement;
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::test::{TestQueryExt as _, TestWindowExt as _};
use gpui_kit::{
    point, px, AppContext as _, Background, Bounds, Entity, Keystroke, Pixels, Point, Role,
    TestAppContext, WindowHandle,
};
use smithagents_app::app_state::AppStateStore;
use smithagents_app::link::{AgentCommands, LinkError};
use smithagents_app::model::{Action, Connection, ShellModel};
use smithagents_app::settings::AppSettingsStore;
use smithagents_app::shell::{Shell, ShellOptions, LEFT_PANE_ID, SETTINGS_PANEL};
use smithagents_app::shell_state::{ShellEvent, ShellState};
use smithagents_app::theme_adapter::{region_tokens, Region};
use smithagents_app::view_models::{
    SettingsSection, StatusKind, ABSOLUTE_PATH_TEXT, CLONE_UNSUPPORTED_TEXT, DAEMON_SILENT_TEXT,
    NO_REPOS_TEXT,
};
use smithagents_app::views::agents::NEW_AGENT_ID;
use smithagents_app::views::rail::{RailEntry, RAIL_ID};
use smithagents_app::views::settings::{
    repo_row_id, section_header_id, section_id, ADD_REPO_ID, BROWSE_REPO_ID, FONT_FIELD_ID,
    FONT_LABEL, FONT_OPTIONS_ID, REPOS_EMPTY_ID, REPO_LIST_ID, REPO_PATH_ERROR_ID,
    REPO_PATH_INPUT_ID, REPO_PATH_LABEL, SETTINGS_SCROLL_ID, TEXT_SIZE_DOWN_ID, TEXT_SIZE_ID,
    TEXT_SIZE_LABEL, TEXT_SIZE_UP_ID, TEXT_SIZE_VALUE_ID, THEME_FIELD_ID, THEME_LABEL,
    THEME_OPTIONS_ID,
};
use smithagents_client::ClientError;
use smithagents_core::agent::{Agent, Repo};
use smithagents_core::ids::{AgentId, RepoId};
use smithagents_theme::native::NativeTheme;
use smithagents_theme::state::{AppearanceSettings, ThemeChoice};
use smithagents_theme::tokens::{Appearance, ThemeSet};

/// Commands that record each add. An add answers with the next queued
/// result, or waits until the test answers it (or forever).
#[derive(Default)]
struct FakeRepos {
    adds: RefCell<Vec<PathBuf>>,
    results: RefCell<VecDeque<Result<Repo, LinkError>>>,
    pending: RefCell<Vec<oneshot::Sender<Result<Repo, LinkError>>>>,
}

impl FakeRepos {
    fn answer(&self, result: Result<Repo, LinkError>) {
        let sender = self.pending.borrow_mut().remove(0);
        let _ = sender.send(result);
    }
}

impl AgentCommands for FakeRepos {
    fn create_agent(&self, _: RepoId) -> LocalBoxFuture<'static, Result<Agent, LinkError>> {
        panic!("the settings pane created an agent")
    }

    fn close_agent(&self, _: AgentId) -> LocalBoxFuture<'static, Result<(), LinkError>> {
        panic!("the settings pane closed an agent")
    }

    fn add_repo(&self, path: &Path) -> LocalBoxFuture<'static, Result<Repo, LinkError>> {
        self.adds.borrow_mut().push(path.to_owned());
        if let Some(result) = self.results.borrow_mut().pop_front() {
            return async move { result }.boxed_local();
        }
        let (tx, rx) = oneshot::channel();
        self.pending.borrow_mut().push(tx);
        async move { rx.await.unwrap_or(Err(LinkError::Stopped)) }.boxed_local()
    }
}

fn repo(path: &str) -> Repo {
    let at: DateTime<Utc> = "2026-11-20T09:00:00Z".parse().unwrap();
    Repo {
        id: RepoId::new(),
        path: path.into(),
        default_branch: "main".into(),
        created_at: at,
    }
}

/// A connected model with these repositories.
fn connected(repos: Vec<Repo>) -> ShellModel {
    let mut model = ShellModel::new();
    model.apply(Action::Snapshot {
        agents: vec![],
        repos,
        latest_seq: 1,
    });
    model.apply(Action::ConnectionChanged(Connection::Connected));
    model
}

/// What a test opens the shell with.
struct Setup {
    /// The repositories of the connected model each open starts with.
    repos: Vec<Repo>,
    home: Option<PathBuf>,
    system: Rc<FakeAppearance>,
    commands: Rc<FakeRepos>,
    command_timeout: Option<Duration>,
    size: (f32, f32),
}

impl Setup {
    fn new() -> Self {
        Self {
            repos: vec![],
            home: None,
            system: FakeAppearance::new(Appearance::Dark),
            commands: Rc::new(FakeRepos::default()),
            command_timeout: None,
            size: (1200., 800.),
        }
    }

    fn home(mut self, home: &Path) -> Self {
        self.home = Some(home.to_owned());
        self
    }

    /// Opens the shell and draws a frame.
    fn open(
        &self,
        cx: &mut TestAppContext,
    ) -> (WindowHandle<Root>, Entity<Shell>, Entity<ShellState>) {
        let (window, shell) = open_shell_options(
            cx,
            ShellOptions {
                model: connected(self.repos.clone()),
                settings: self.home.as_deref().map(AppSettingsStore::new),
                app_state: self.home.as_deref().map(AppStateStore::new),
                system_appearance: Some(self.system.clone()),
                commands: Some(self.commands.clone()),
                command_timeout: self.command_timeout,
                ..Default::default()
            },
            self.size,
        );
        let state = shell.read_with(cx, |shell, _| shell.state().clone());
        frame(cx, window);
        (window, shell, state)
    }
}

fn frame(cx: &mut TestAppContext, window: WindowHandle<Root>) {
    cx.run_until_parked();
    cx.update_window(window.into(), |_, window, cx| window.render_frame(cx))
        .unwrap();
}

/// Runs `f` on the window after a fresh frame.
fn with_window<R>(
    cx: &mut TestAppContext,
    window: WindowHandle<Root>,
    f: impl FnOnce(&mut gpui_kit::Window, &mut gpui_kit::App) -> R,
) -> R {
    let mut out = None;
    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        out = Some(f(window, cx));
    })
    .unwrap();
    out.unwrap()
}

/// Clicks the radio labeled `label` in the radio group `group`.
fn choose(cx: &mut TestAppContext, window: WindowHandle<Root>, group: &'static str, label: &str) {
    with_window(cx, window, |window, cx| {
        let target = window.within(group).find_by_label(label);
        window
            .within(group)
            .click(target.path().last().unwrap().clone(), cx);
    });
    frame(cx, window);
}

fn checked(
    cx: &mut TestAppContext,
    window: WindowHandle<Root>,
    group: &'static str,
    label: &str,
) -> bool {
    with_window(cx, window, |window, _| {
        window.within(group).find_by_label(label).checked() == Some(true)
    })
}

fn click(cx: &mut TestAppContext, window: WindowHandle<Root>, id: &'static str) {
    with_window(cx, window, |window, cx| window.click(id, cx));
    frame(cx, window);
}

fn near_bottom_left(bounds: Bounds<Pixels>, inset: f32) -> Point<Pixels> {
    point(
        bounds.origin.x + px(inset),
        bounds.origin.y + bounds.size.height - px(inset),
    )
}

/// The painted surfaces of the left pane, the rail and the right pane.
fn painted(cx: &mut TestAppContext, window: WindowHandle<Root>) -> [Background; 3] {
    with_window(cx, window, |window, cx| {
        window.render_frame(cx);
        let left = window.find(LEFT_PANE_ID).bounds();
        let rail = window.find(RAIL_ID).bounds();
        let right = window.find(SETTINGS_PANEL).bounds();
        [
            painted_at(window, near_bottom_left(left, 20.)),
            painted_at(window, near_bottom_left(rail, 10.)),
            painted_at(window, near_bottom_left(right, 20.)),
        ]
    })
}

/// What the three regions paint in `native`.
fn expected(native: &NativeTheme) -> [Background; 3] {
    [Region::LeftPane, Region::Rail, Region::RightPane]
        .map(|region| token_background(native, region_tokens(region).0))
}

fn light_native() -> NativeTheme {
    ThemeSet::embedded()
        .native(&theme_state(Appearance::Light).theme)
        .unwrap()
}

fn theme(cx: &mut TestAppContext, state: &Entity<ShellState>) -> String {
    state.read_with(cx, |state, _| state.theme_state().theme.clone())
}

fn saved(home: &Path) -> AppearanceSettings {
    AppSettingsStore::new(home).load()
}

/// Counts `ShellEvent::ThemeChanged`.
fn count_theme_changes(
    cx: &mut TestAppContext,
    state: &Entity<ShellState>,
) -> (Rc<Cell<usize>>, gpui_kit::Subscription) {
    let count = Rc::new(Cell::new(0));
    let seen = count.clone();
    let subscription = cx.update(|cx| {
        cx.subscribe(state, move |_, event: &ShellEvent, _| {
            if *event == ShellEvent::ThemeChanged {
                seen.set(seen.get() + 1);
            }
        })
    });
    (count, subscription)
}

// ---- The pane --------------------------------------------------------------

#[gpui_kit::test]
fn the_pane_paints_card_and_shows_both_sections_expanded_with_labeled_controls(
    cx: &mut TestAppContext,
) {
    let (window, _, _) = Setup::new().open(cx);
    assert_eq!(painted(cx, window)[2], expected(&dark_native())[2], "card");
    with_window(cx, window, |window, _| {
        for section in SettingsSection::ALL {
            let group = window.find(section_id(section));
            assert_eq!(group.role(), Some(Role::Group));
            assert_eq!(group.label(), Some(section.title()));
            let header = window.find(section_header_id(section));
            assert_eq!(header.expanded(), Some(true), "{section:?} starts expanded");
            assert_eq!(header.label(), Some(section.title()));
        }
        for label in [THEME_LABEL, TEXT_SIZE_LABEL, FONT_LABEL, REPO_PATH_LABEL] {
            assert!(window.try_find_by_label(label).is_some(), "{label}");
        }
        for (field, label, options, count) in [
            (THEME_FIELD_ID, THEME_LABEL, THEME_OPTIONS_ID, 3),
            (FONT_FIELD_ID, FONT_LABEL, FONT_OPTIONS_ID, 2),
        ] {
            let group = window.find(field);
            assert_eq!(group.role(), Some(Role::Group));
            assert_eq!(group.label(), Some(label));
            let radios = window.within(options).find_all_by_role(Role::RadioButton);
            assert_eq!(radios.len(), count, "{label}");
        }
        assert_eq!(window.find(TEXT_SIZE_VALUE_ID).value(), Some("100%"));
        // The order of the sections.
        let appearance = window
            .find(section_id(SettingsSection::Appearance))
            .bounds();
        let repositories = window
            .find(section_id(SettingsSection::Repositories))
            .bounds();
        assert!(appearance.origin.y < repositories.origin.y);
    });
    assert!(checked(cx, window, THEME_OPTIONS_ID, "System"));
    assert!(checked(cx, window, FONT_OPTIONS_ID, "System"));
}

// ---- Appearance ------------------------------------------------------------

#[gpui_kit::test]
fn choosing_the_light_theme_repaints_the_native_panes_and_saves_the_choice(
    cx: &mut TestAppContext,
) {
    let home = tempfile::tempdir().unwrap();
    let (window, _, state) = Setup::new().home(home.path()).open(cx);
    let dark = painted(cx, window);
    assert_eq!(dark, expected(&dark_native()));

    choose(cx, window, THEME_OPTIONS_ID, "Smith Light");
    assert_eq!(theme(cx, &state), "smith-light");
    let light = painted(cx, window);
    assert_eq!(
        light,
        expected(&light_native()),
        "left pane, rail, right pane"
    );
    assert_ne!(light[0], dark[0]);
    assert_ne!(light[1], dark[1]);
    // gpui-component's own theme switched too (its controls paint from it).
    let framework_background = cx.update(|cx| cx.theme().background);
    assert_eq!(
        Background::from(framework_background),
        token_background(&light_native(), "background")
    );
    assert!(checked(cx, window, THEME_OPTIONS_ID, "Smith Light"));
    assert!(!checked(cx, window, THEME_OPTIONS_ID, "System"));

    assert_eq!(
        saved(home.path()).theme,
        ThemeChoice::Id("smith-light".into())
    );
    let text = std::fs::read_to_string(home.path().join("app-settings.json")).unwrap();
    let json: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(json["theme"], "smith-light");
}

#[gpui_kit::test]
fn system_follows_the_os_appearance_live_and_an_explicit_theme_does_not(cx: &mut TestAppContext) {
    let setup = Setup {
        system: FakeAppearance::new(Appearance::Light),
        ..Setup::new()
    };
    let (window, _, state) = setup.open(cx);
    assert_eq!(
        theme(cx, &state),
        "smith-light",
        "System starts from the OS"
    );
    assert_eq!(painted(cx, window), expected(&light_native()));

    cx.update(|cx| setup.system.set(Appearance::Dark, cx));
    frame(cx, window);
    assert_eq!(theme(cx, &state), "smith-dark", "flipped with the OS");
    assert_eq!(painted(cx, window), expected(&dark_native()));
    assert!(checked(cx, window, THEME_OPTIONS_ID, "System"));

    // An explicit choice stays put when the OS flips.
    choose(cx, window, THEME_OPTIONS_ID, "Smith Light");
    cx.update(|cx| setup.system.set(Appearance::Dark, cx));
    cx.update(|cx| setup.system.set(Appearance::Light, cx));
    cx.update(|cx| setup.system.set(Appearance::Dark, cx));
    frame(cx, window);
    assert_eq!(theme(cx, &state), "smith-light");
    assert_eq!(painted(cx, window), expected(&light_native()));
    // Back to System: the OS (dark now) decides again.
    choose(cx, window, THEME_OPTIONS_ID, "System");
    assert_eq!(theme(cx, &state), "smith-dark");
}

#[gpui_kit::test]
fn the_os_listener_goes_away_with_the_window(cx: &mut TestAppContext) {
    let setup = Setup::new();
    let (window, _, _) = setup.open(cx);
    assert_eq!(setup.system.listener_count(), 1);
    cx.update_window(window.into(), |_, window, _| window.remove_window())
        .unwrap();
    cx.run_until_parked();
    assert_eq!(setup.system.listener_count(), 0);
}

#[gpui_kit::test]
fn the_text_size_steps_change_the_theme_state_and_the_saved_settings(cx: &mut TestAppContext) {
    let home = tempfile::tempdir().unwrap();
    let (window, _, state) = Setup::new().home(home.path()).open(cx);
    let scale = |cx: &mut TestAppContext| state.read_with(cx, |s, _| s.theme_state().text_scale);
    let value = |cx: &mut TestAppContext| {
        with_window(cx, window, |window, _| {
            window.find(TEXT_SIZE_VALUE_ID).value().map(str::to_owned)
        })
    };
    with_window(cx, window, |window, _| {
        let group = window.find(TEXT_SIZE_ID);
        assert_eq!(group.role(), Some(Role::Group));
        assert_eq!(group.label(), Some(TEXT_SIZE_LABEL));
        assert_eq!(window.find(TEXT_SIZE_UP_ID).label(), Some("Larger text"));
        assert_eq!(window.find(TEXT_SIZE_DOWN_ID).label(), Some("Smaller text"));
    });

    click(cx, window, TEXT_SIZE_UP_ID);
    assert_eq!(scale(cx), 1.05);
    assert_eq!(value(cx).as_deref(), Some("105%"));
    assert_eq!(saved(home.path()).text_scale, 1.05);
    // gpui-component's base size follows the scale.
    let font_size = cx.update(|cx| f32::from(cx.theme().font_size));
    assert!((font_size - 16.0 * 1.05).abs() < 0.01, "{font_size}");

    click(cx, window, TEXT_SIZE_DOWN_ID);
    click(cx, window, TEXT_SIZE_DOWN_ID);
    assert_eq!(scale(cx), 0.95);
    assert_eq!(value(cx).as_deref(), Some("95%"));
    assert_eq!(saved(home.path()).text_scale, 0.95);

    // At the largest size the larger button is disabled.
    state.update(cx, |state, cx| {
        let settings = AppearanceSettings {
            text_scale: 1.5,
            ..state.appearance_settings().clone()
        };
        state.set_appearance(settings, cx)
    });
    frame(cx, window);
    with_window(cx, window, |window, _| {
        assert_eq!(window.find(TEXT_SIZE_UP_ID).disabled(), Some(true));
        assert_ne!(window.find(TEXT_SIZE_DOWN_ID).disabled(), Some(true));
    });
    click(cx, window, TEXT_SIZE_UP_ID);
    assert_eq!(scale(cx), 1.5);
}

#[gpui_kit::test]
fn choosing_a_font_changes_the_theme_state_and_the_saved_settings(cx: &mut TestAppContext) {
    let home = tempfile::tempdir().unwrap();
    let (window, _, state) = Setup::new().home(home.path()).open(cx);
    choose(cx, window, FONT_OPTIONS_ID, "Monospace");
    assert_eq!(
        state.read_with(cx, |s, _| s.theme_state().font.clone()),
        "mono"
    );
    assert_eq!(saved(home.path()).font, "mono");
    let family = cx.update(|cx| cx.theme().font_family.to_string());
    assert_eq!(family, "Menlo", "gpui-component paints with the font");
    assert!(checked(cx, window, FONT_OPTIONS_ID, "Monospace"));
    assert!(!checked(cx, window, FONT_OPTIONS_ID, "System"));
}

#[gpui_kit::test]
fn reopening_with_the_same_home_restores_every_choice(cx: &mut TestAppContext) {
    let home = tempfile::tempdir().unwrap();
    let setup = Setup::new().home(home.path());
    let (window, _, state) = setup.open(cx);
    choose(cx, window, THEME_OPTIONS_ID, "Smith Light");
    choose(cx, window, FONT_OPTIONS_ID, "Monospace");
    click(cx, window, TEXT_SIZE_UP_ID);
    let before = state.read_with(cx, |s, _| s.theme_state().clone());

    let (reopened, _, reopened_state) = setup.open(cx);
    assert_eq!(
        reopened_state.read_with(cx, |s, _| s.theme_state().clone()),
        before
    );
    assert!(checked(cx, reopened, THEME_OPTIONS_ID, "Smith Light"));
    assert!(checked(cx, reopened, FONT_OPTIONS_ID, "Monospace"));
    with_window(cx, reopened, |window, _| {
        assert_eq!(window.find(TEXT_SIZE_VALUE_ID).value(), Some("105%"));
    });
    assert_eq!(painted(cx, reopened), expected(&light_native()));
}

#[gpui_kit::test]
fn a_corrupt_settings_file_opens_with_the_defaults(cx: &mut TestAppContext) {
    let home = tempfile::tempdir().unwrap();
    std::fs::write(home.path().join("app-settings.json"), "{not json").unwrap();
    let (window, _, state) = Setup::new().home(home.path()).open(cx);
    assert_eq!(
        state.read_with(cx, |s, _| s.theme_state().clone()),
        theme_state(Appearance::Dark)
    );
    assert_eq!(
        state.read_with(cx, |s, _| s.appearance_settings().clone()),
        AppearanceSettings::default()
    );
    assert!(checked(cx, window, THEME_OPTIONS_ID, "System"));
    assert_eq!(painted(cx, window), expected(&dark_native()));
    // The next change writes a good file over it.
    choose(cx, window, THEME_OPTIONS_ID, "Smith Light");
    assert_eq!(
        saved(home.path()).theme,
        ThemeChoice::Id("smith-light".into())
    );
}

#[gpui_kit::test]
fn a_failed_save_says_so_and_the_change_still_applies(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    // A file where the home directory should be: every write fails.
    let home = dir.path().join("home");
    std::fs::write(&home, "not a directory").unwrap();
    let setup = Setup {
        home: Some(home),
        ..Setup::new()
    };
    let (window, _, state) = setup.open(cx);
    choose(cx, window, THEME_OPTIONS_ID, "Smith Light");
    assert_eq!(theme(cx, &state), "smith-light");
    assert_eq!(painted(cx, window), expected(&light_native()));
    let status = state.read_with(cx, |s, _| s.status_message()).unwrap();
    assert_eq!(status.kind, StatusKind::Error);
    assert!(
        status.text.starts_with("Could not save settings: "),
        "{}",
        status.text
    );
}

#[gpui_kit::test]
fn the_theme_state_follows_every_change_with_one_notification_each(cx: &mut TestAppContext) {
    let setup = Setup::new();
    let (window, _, state) = setup.open(cx);
    let (changes, _subscription) = count_theme_changes(cx, &state);

    choose(cx, window, THEME_OPTIONS_ID, "Smith Light");
    assert_eq!(changes.get(), 1);
    assert_eq!(theme(cx, &state), "smith-light");
    // The chosen theme again: nothing changes.
    choose(cx, window, THEME_OPTIONS_ID, "Smith Light");
    assert_eq!(changes.get(), 1);

    choose(cx, window, THEME_OPTIONS_ID, "Smith Dark");
    assert_eq!(changes.get(), 2);
    // System resolves to Smith Dark too (the OS is dark): saved, no change.
    choose(cx, window, THEME_OPTIONS_ID, "System");
    assert_eq!(changes.get(), 2);
    assert!(checked(cx, window, THEME_OPTIONS_ID, "System"));

    click(cx, window, TEXT_SIZE_UP_ID);
    assert_eq!(changes.get(), 3);
    choose(cx, window, FONT_OPTIONS_ID, "Monospace");
    assert_eq!(changes.get(), 4);
    cx.update(|cx| setup.system.set(Appearance::Light, cx));
    cx.run_until_parked();
    assert_eq!(changes.get(), 5, "an OS flip under System");
    let latest = state.read_with(cx, |s, _| s.theme_state().clone());
    assert_eq!(latest.theme, "smith-light");
    assert_eq!(latest.text_scale, 1.05);
    assert_eq!(latest.font, "mono");
    assert_eq!(latest.appearance, Appearance::Light);
}

// ---- Source repositories -----------------------------------------------------

/// Types `text` into the path field (focusing it first).
fn type_path(cx: &mut TestAppContext, window: WindowHandle<Root>, text: &str) {
    with_window(cx, window, |window, cx| {
        window.click(REPO_PATH_INPUT_ID, cx);
        window.press("cmd-a", cx);
        window.press("backspace", cx);
        window.input(text, cx);
    });
    frame(cx, window);
}

fn press(cx: &mut TestAppContext, window: WindowHandle<Root>, key: &str) {
    with_window(cx, window, |window, cx| window.press(key, cx));
    frame(cx, window);
}

fn path_value(cx: &mut TestAppContext, window: WindowHandle<Root>) -> String {
    with_window(cx, window, |window, _| {
        window
            .find(REPO_PATH_INPUT_ID)
            .value()
            .unwrap_or_default()
            .to_owned()
    })
}

/// The line under the path field, if any.
fn path_error(cx: &mut TestAppContext, window: WindowHandle<Root>) -> Option<String> {
    with_window(cx, window, |window, _| {
        window.try_find(REPO_PATH_ERROR_ID).map(|line| {
            assert_eq!(line.role(), Some(Role::Status));
            line.label().unwrap().to_owned()
        })
    })
}

fn add_disabled(cx: &mut TestAppContext, window: WindowHandle<Root>) -> bool {
    with_window(cx, window, |window, _| {
        window.find(ADD_REPO_ID).disabled() == Some(true)
    })
}

#[gpui_kit::test]
fn no_repository_shows_the_hint(cx: &mut TestAppContext) {
    let (window, _, _) = Setup::new().open(cx);
    with_window(cx, window, |window, _| {
        let hint = window.find(REPOS_EMPTY_ID);
        assert!(hint.visible());
        assert_eq!(hint.value(), Some(NO_REPOS_TEXT));
        assert!(window.try_find(REPO_LIST_ID).is_none());
    });
}

#[gpui_kit::test]
fn the_repositories_are_listed_by_name_with_path_and_branch(cx: &mut TestAppContext) {
    let zeta = repo("/src/zeta");
    let spaced = Repo {
        default_branch: "trunk".into(),
        ..repo("/Users/me/My Projects/ünïcode app")
    };
    let setup = Setup {
        repos: vec![zeta.clone(), spaced.clone()],
        ..Setup::new()
    };
    let (window, _, _) = setup.open(cx);
    with_window(cx, window, |window, _| {
        assert!(window.try_find(REPOS_EMPTY_ID).is_none());
        assert_eq!(window.find(REPO_LIST_ID).role(), Some(Role::List));
        let rows = window.within(REPO_LIST_ID).find_all_by_role(Role::ListItem);
        let labels: Vec<_> = rows
            .iter()
            .map(|row| row.label().unwrap().to_owned())
            .collect();
        // By name: "zeta" sorts before "ünïcode app" (code point order).
        assert_eq!(
            labels,
            [
                "zeta, /src/zeta, default branch main",
                "ünïcode app, /Users/me/My Projects/ünïcode app, default branch trunk",
            ]
        );
        assert!(window.find(repo_row_id(zeta.id)).visible());
        assert!(window.find(repo_row_id(spaced.id)).visible());
    });
}

#[gpui_kit::test]
fn adding_a_path_sends_one_add_disables_the_controls_and_clears_on_success(
    cx: &mut TestAppContext,
) {
    let setup = Setup::new();
    let (window, _, state) = setup.open(cx);
    type_path(cx, window, "  /src/my app ");
    press(cx, window, "enter");
    press(cx, window, "enter");
    click(cx, window, ADD_REPO_ID);
    assert_eq!(
        *setup.commands.adds.borrow(),
        [PathBuf::from("/src/my app")],
        "exactly one add, with the trimmed path"
    );
    assert!(add_disabled(cx, window), "disabled while the add runs");
    with_window(cx, window, |window, _| {
        assert_eq!(window.find(BROWSE_REPO_ID).disabled(), Some(true));
    });
    // Typing while it runs changes nothing.
    with_window(cx, window, |window, cx| window.input("xyz", cx));
    assert_eq!(path_value(cx, window), "  /src/my app ");

    // The daemon answers; the repository itself arrives through the model.
    let added = repo("/src/my app");
    setup.commands.answer(Ok(added.clone()));
    frame(cx, window);
    assert!(!add_disabled(cx, window));
    assert_eq!(path_value(cx, window), "", "the field clears");
    assert_eq!(path_error(cx, window), None);
    with_window(cx, window, |window, _| {
        assert!(
            window.try_find(repo_row_id(added.id)).is_none(),
            "not inserted locally"
        );
    });
    state.update(cx, |state, cx| {
        state.dispatch(Action::RepoAdded(added.clone()), cx)
    });
    frame(cx, window);
    with_window(cx, window, |window, _| {
        assert!(window.find(repo_row_id(added.id)).visible());
        assert!(window.try_find(REPOS_EMPTY_ID).is_none());
    });
    let status = state.read_with(cx, |s, _| s.status_message()).unwrap();
    assert_eq!(status.kind, StatusKind::Info);
    assert_eq!(status.text, "Repository added: my app");

    // A second add works once the first is done.
    type_path(cx, window, "/src/other");
    press(cx, window, "enter");
    assert_eq!(setup.commands.adds.borrow().len(), 2);
}

#[gpui_kit::test]
fn two_enters_before_the_next_frame_still_send_one_add(cx: &mut TestAppContext) {
    let setup = Setup::new();
    let (window, _, _) = setup.open(cx);
    type_path(cx, window, "/src/app");
    // No frame between them: the field is not disabled yet for the second.
    with_window(cx, window, |window, cx| {
        let enter = Keystroke::parse("enter").unwrap();
        window.dispatch_keystroke(enter.clone(), cx);
        window.dispatch_keystroke(enter, cx);
    });
    frame(cx, window);
    assert_eq!(*setup.commands.adds.borrow(), [PathBuf::from("/src/app")]);
}

#[gpui_kit::test]
fn a_daemon_error_shows_its_message_under_the_field_and_keeps_the_text(cx: &mut TestAppContext) {
    let setup = Setup::new();
    setup
        .commands
        .results
        .borrow_mut()
        .push_back(Err(LinkError::Client(ClientError::Api {
            status: 400,
            code: "not_a_git_repo".into(),
            message: "not a git repository".into(),
        })));
    let (window, _, state) = setup.open(cx);
    type_path(cx, window, "/tmp/plain dir");
    press(cx, window, "enter");
    assert_eq!(
        path_error(cx, window).as_deref(),
        Some("Not a git repository")
    );
    assert_eq!(path_value(cx, window), "/tmp/plain dir", "the text stays");
    assert!(!add_disabled(cx, window));
    assert_eq!(
        state.read_with(cx, |s, _| s.status_message()),
        None,
        "shown inline, not in the status bar"
    );
    // Editing the path clears the stale message.
    with_window(cx, window, |window, cx| window.input("x", cx));
    frame(cx, window);
    assert_eq!(path_error(cx, window), None);
}

#[gpui_kit::test]
fn a_tilde_path_is_expanded_to_the_home_directory(cx: &mut TestAppContext) {
    let setup = Setup::new();
    let (window, _, _) = setup.open(cx);
    type_path(cx, window, "~/code/my app");
    press(cx, window, "enter");
    let home = std::env::home_dir().expect("the test runs with a home directory");
    assert_eq!(*setup.commands.adds.borrow(), [home.join("code/my app")]);
}

#[gpui_kit::test]
fn relative_paths_and_urls_are_refused_without_an_add(cx: &mut TestAppContext) {
    let setup = Setup::new();
    let (window, _, _) = setup.open(cx);
    for (text, want) in [
        ("src/app", ABSOLUTE_PATH_TEXT),
        ("https://github.com/acme/app.git", CLONE_UNSUPPORTED_TEXT),
        ("git@github.com:acme/app.git", CLONE_UNSUPPORTED_TEXT),
    ] {
        type_path(cx, window, text);
        click(cx, window, ADD_REPO_ID);
        assert_eq!(path_error(cx, window).as_deref(), Some(want), "{text}");
        assert_eq!(path_value(cx, window), text, "the text stays");
    }
    // An empty field says what to do.
    type_path(cx, window, "");
    press(cx, window, "enter");
    assert_eq!(path_error(cx, window).as_deref(), Some("Enter a path"));
    assert!(setup.commands.adds.borrow().is_empty());
}

#[gpui_kit::test]
fn an_add_the_daemon_never_answers_is_given_up_after_the_timeout(cx: &mut TestAppContext) {
    let setup = Setup {
        command_timeout: Some(Duration::from_secs(5)),
        ..Setup::new()
    };
    let (window, _, _) = setup.open(cx);
    type_path(cx, window, "/src/slow");
    press(cx, window, "enter");
    assert!(add_disabled(cx, window));
    cx.executor().advance_clock(Duration::from_secs(4));
    frame(cx, window);
    assert!(add_disabled(cx, window), "still waiting before the timeout");
    cx.executor().advance_clock(Duration::from_secs(2));
    frame(cx, window);
    assert!(!add_disabled(cx, window), "given up after the timeout");
    assert_eq!(path_error(cx, window).as_deref(), Some(DAEMON_SILENT_TEXT));
    assert_eq!(path_value(cx, window), "/src/slow");
    // The fake still holds the first add; a new one can start.
    press(cx, window, "enter");
    assert_eq!(setup.commands.adds.borrow().len(), 2);
}

#[gpui_kit::test]
fn an_add_waiting_when_the_connection_drops_is_given_up(cx: &mut TestAppContext) {
    let setup = Setup::new();
    let (window, _, state) = setup.open(cx);
    type_path(cx, window, "/src/app");
    press(cx, window, "enter");
    assert!(add_disabled(cx, window));
    state.update(cx, |state, cx| {
        state.dispatch(Action::ConnectionChanged(Connection::Reconnecting), cx)
    });
    frame(cx, window);
    assert!(!add_disabled(cx, window));
    assert_eq!(path_error(cx, window).as_deref(), Some(DAEMON_SILENT_TEXT));
}

#[gpui_kit::test]
fn an_offline_shell_says_it_is_not_connected(cx: &mut TestAppContext) {
    let (window, _) = open_shell_options(
        cx,
        ShellOptions {
            system_appearance: Some(FakeAppearance::new(Appearance::Dark)),
            ..Default::default()
        },
        (1200., 800.),
    );
    frame(cx, window);
    type_path(cx, window, "/src/app");
    press(cx, window, "enter");
    assert_eq!(
        path_error(cx, window).as_deref(),
        Some("Not connected to the daemon")
    );
}

#[gpui_kit::test]
fn browse_fills_the_field_with_the_chosen_folder(cx: &mut TestAppContext) {
    let setup = Setup::new();
    let (window, _, _) = setup.open(cx);
    click(cx, window, BROWSE_REPO_ID);
    assert!(cx.did_prompt_for_paths(), "the native folder picker opened");
    cx.simulate_path_prompt_response(|options| {
        assert!(options.directories && !options.files && !options.multiple);
        Some(vec![PathBuf::from("/Users/me/My Projects/ünïcode app")])
    });
    frame(cx, window);
    assert_eq!(path_value(cx, window), "/Users/me/My Projects/ünïcode app");
    assert!(
        setup.commands.adds.borrow().is_empty(),
        "not added until Add"
    );
    with_window(cx, window, |window, _| {
        assert_eq!(window.find(REPO_PATH_INPUT_ID).focused(), Some(true));
    });
    press(cx, window, "enter");
    assert_eq!(
        *setup.commands.adds.borrow(),
        [PathBuf::from("/Users/me/My Projects/ünïcode app")]
    );

    // A cancelled picker leaves the field alone.
    setup.commands.answer(Err(LinkError::NotConnected));
    frame(cx, window);
    click(cx, window, BROWSE_REPO_ID);
    cx.simulate_path_prompt_response(|_| None);
    frame(cx, window);
    assert_eq!(path_value(cx, window), "/Users/me/My Projects/ünïcode app");
}

// ---- Sections and the focus request ------------------------------------------

#[gpui_kit::test]
fn a_collapsed_section_stays_collapsed_after_a_reopen(cx: &mut TestAppContext) {
    let home = tempfile::tempdir().unwrap();
    let setup = Setup::new().home(home.path());
    let (window, _, _) = setup.open(cx);
    let header = section_header_id(SettingsSection::Appearance);
    with_window(cx, window, |window, cx| window.click(header.clone(), cx));
    frame(cx, window);
    with_window(cx, window, |window, _| {
        assert_eq!(window.find(header.clone()).expanded(), Some(false));
        assert!(window.try_find(THEME_FIELD_ID).is_none(), "content hidden");
        assert!(
            window.try_find(REPO_PATH_INPUT_ID).is_some(),
            "the other stays"
        );
    });
    let text = std::fs::read_to_string(home.path().join("app-state.json")).unwrap();
    let json: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(
        json["collapsed_settings_sections"],
        serde_json::json!(["appearance"])
    );

    let (reopened, _, _) = setup.open(cx);
    with_window(cx, reopened, |window, _| {
        assert_eq!(window.find(header.clone()).expanded(), Some(false));
        assert!(window.try_find(THEME_FIELD_ID).is_none());
    });
    // Expanding again is remembered too.
    with_window(cx, reopened, |window, cx| window.click(header.clone(), cx));
    frame(cx, reopened);
    with_window(cx, reopened, |window, _| {
        assert_eq!(window.find(header.clone()).expanded(), Some(true));
        assert!(window.try_find(THEME_FIELD_ID).is_some());
    });
    assert!(AppStateStore::new(home.path())
        .load()
        .collapsed_sections
        .is_empty());
}

#[gpui_kit::test]
fn new_agent_without_a_repository_opens_settings_on_the_path_field(cx: &mut TestAppContext) {
    let home = tempfile::tempdir().unwrap();
    // A short window, so the sections do not fit and must scroll.
    let setup = Setup {
        size: (1200., 420.),
        ..Setup::new().home(home.path())
    };
    let (window, shell, state) = setup.open(cx);
    let dock = shell.read_with(cx, |shell, _| shell.dock().clone());
    let open = |cx: &mut TestAppContext| {
        dock.read_with(cx, |dock, _| dock.is_dock_open(DockPlacement::Right))
    };
    // Collapse Source repositories, then hide the dock.
    let header = section_header_id(SettingsSection::Repositories);
    with_window(cx, window, |window, cx| window.click(header.clone(), cx));
    frame(cx, window);
    with_window(cx, window, |window, cx| {
        assert_eq!(window.find(header.clone()).expanded(), Some(false));
        // Not scrolled yet: Appearance starts at the top.
        let scroll = window.find(SETTINGS_SCROLL_ID).bounds();
        let appearance = window
            .find(section_id(SettingsSection::Appearance))
            .bounds();
        assert!((appearance.origin.y - scroll.origin.y).abs() < px(0.5));
        window.click(RailEntry::Settings.element_id(), cx);
    });
    frame(cx, window);
    assert!(!open(cx));

    with_window(cx, window, |window, cx| window.click(NEW_AGENT_ID, cx));
    frame(cx, window);
    frame(cx, window);
    assert!(open(cx), "the Settings dock is shown");
    assert!(state.read_with(cx, |s, _| s
        .settings_section_expanded(SettingsSection::Repositories)));
    with_window(cx, window, |window, _| {
        assert_eq!(window.find(header.clone()).expanded(), Some(true));
        let input = window.find(REPO_PATH_INPUT_ID);
        assert_eq!(input.focused(), Some(true), "the path field has the focus");
        assert!(input.visible());
        // Scrolled toward the section's top, as far as the content allows:
        // the whole section is in view and Appearance moved up out of it.
        let scroll = window.find(SETTINGS_SCROLL_ID).bounds();
        let section = window
            .find(section_id(SettingsSection::Repositories))
            .bounds();
        assert!(
            section.origin.y >= scroll.origin.y - px(0.5)
                && section.bottom() <= scroll.bottom() + px(0.5),
            "section {section:?} in scroll area {scroll:?}"
        );
        let appearance = window
            .find(section_id(SettingsSection::Appearance))
            .bounds();
        assert!(
            appearance.origin.y < scroll.origin.y - px(20.),
            "Appearance scrolled up: {appearance:?} vs {scroll:?}"
        );
    });
    // Typing goes to the field.
    with_window(cx, window, |window, cx| window.input("/src/app", cx));
    assert_eq!(path_value(cx, window), "/src/app");
}

#[gpui_kit::test]
fn a_theme_applied_from_outside_is_recorded_and_announced_once(cx: &mut TestAppContext) {
    let (window, _, state) = Setup::new().open(cx);
    let (changes, _subscription) = count_theme_changes(cx, &state);
    let light = theme_state(Appearance::Light);
    cx.update(|cx| {
        smithagents_app::theme_adapter::apply_theme(ThemeSet::embedded(), &light, Some(&state), cx)
    });
    cx.run_until_parked();
    assert_eq!(changes.get(), 1);
    assert_eq!(state.read_with(cx, |s, _| s.theme_state().clone()), light);
    assert_eq!(painted(cx, window), expected(&light_native()));
}
