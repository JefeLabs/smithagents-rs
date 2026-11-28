//! Headless checks of Settings -> Source repositories: which repository
//! intents and feedback go to. Commands go to `FakeFeedback`, so no daemon
//! runs; nothing moves until the daemon's answer reaches the model.
//!
//! Kit names are imported one by one: with `test-support` on, a glob import
//! would bring in GPUI's `test` attribute and shadow the built-in `#[test]`.

mod common;

use std::rc::Rc;

use chrono::{DateTime, Utc};
use common::feedback_submit::{FakeFeedback, FeedbackCall};
use common::intents::{model_with, FakeIntents};
use common::ui::open_shell_options;
use gpui_kit::base::Root;
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{AppContext as _, ElementId, Entity, TestAppContext, WindowHandle};
use smithagents_app::model::Action;
use smithagents_app::shell::ShellOptions;
use smithagents_app::shell_state::ShellState;
use smithagents_app::views::settings::{
    repo_meta_tag_id, repo_use_meta_id, META_TAG, REPO_META_ERROR_ID,
};
use smithagents_core::agent::{Repo, SourceControl};
use smithagents_core::feedback::FeedbackSettings;
use smithagents_core::ids::RepoId;

struct Opened {
    window: WindowHandle<Root>,
    state: Entity<ShellState>,
    fake: Rc<FakeFeedback>,
    repos: Vec<Repo>,
    _home: tempfile::TempDir,
}

fn repo(path: &str) -> Repo {
    let at: DateTime<Utc> = "2026-11-20T09:00:00Z".parse().unwrap();
    Repo {
        id: RepoId::new(),
        path: path.into(),
        source_control: Some(SourceControl::Git {
            default_branch: "main".into(),
        }),
        created_at: at,
    }
}

/// Opens the shell, connected, with `count` repositories and the settings
/// loaded as `stored` (`None`: not loaded; `Some(index)`: that repository is
/// stored; `Some(None)`: the loaded setting is empty).
fn open(cx: &mut TestAppContext, count: usize, stored: Option<Option<usize>>) -> Opened {
    let fake = Rc::new(FakeFeedback::default());
    let home = tempfile::tempdir().unwrap();
    let repos: Vec<Repo> = (0..count)
        .map(|i| repo(&format!("/work/repo-{i}")))
        .collect();
    let mut model = model_with(vec![], vec![], repos.clone());
    if let Some(stored) = stored {
        model.apply(Action::FeedbackSettingsLoaded(FeedbackSettings {
            meta_repo_id: stored.map(|i| repos[i].id),
        }));
    }
    let (window, shell) = open_shell_options(
        cx,
        ShellOptions {
            model,
            intent_commands: Some(Rc::new(FakeIntents::default())),
            feedback_commands: Some(fake.clone()),
            app_state: Some(smithagents_app::app_state::AppStateStore::new(home.path())),
            ..Default::default()
        },
        (1200., 1000.),
    );
    let state = shell.read_with(cx, |shell, _| shell.state().clone());
    frame(cx, window);
    Opened {
        window,
        state,
        fake,
        repos,
        _home: home,
    }
}

fn frame(cx: &mut TestAppContext, window: WindowHandle<Root>) {
    cx.run_until_parked();
    cx.update_window(window.into(), |_, window, cx| window.render_frame(cx))
        .unwrap();
}

fn with_window<R>(
    cx: &mut TestAppContext,
    window: WindowHandle<Root>,
    f: impl FnOnce(&mut gpui_kit::Window) -> R,
) -> R {
    let mut out = None;
    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        out = Some(f(window));
    })
    .unwrap();
    out.unwrap()
}

fn click(cx: &mut TestAppContext, window: WindowHandle<Root>, id: impl Into<ElementId>) {
    let id = id.into();
    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click(id, cx);
    })
    .unwrap();
    frame(cx, window);
}

fn label_of(cx: &mut TestAppContext, window: WindowHandle<Root>, id: ElementId) -> Option<String> {
    with_window(cx, window, |window| {
        window
            .try_find(id)
            .and_then(|element| element.value().or(element.label()).map(str::to_owned))
    })
}

fn exists(cx: &mut TestAppContext, window: WindowHandle<Root>, id: ElementId) -> bool {
    with_window(cx, window, |window| window.try_find(id).is_some())
}

fn load(cx: &mut TestAppContext, opened: &Opened, settings: FeedbackSettings) {
    opened.state.update(cx, |state, cx| {
        state.dispatch(Action::FeedbackSettingsLoaded(settings), cx)
    });
    frame(cx, opened.window);
}

#[gpui_kit::test]
fn a_single_repository_is_the_meta_repository_without_a_button(cx: &mut TestAppContext) {
    let opened = open(cx, 1, Some(None));
    let id = opened.repos[0].id;
    assert_eq!(
        label_of(cx, opened.window, repo_meta_tag_id(id)).as_deref(),
        Some("Intents and feedback")
    );
    assert_eq!(META_TAG, "Intents and feedback");
    assert!(!exists(cx, opened.window, repo_use_meta_id(id)));
}

#[gpui_kit::test]
fn the_chosen_repository_is_tagged_and_the_others_offer_the_button(cx: &mut TestAppContext) {
    let opened = open(cx, 2, Some(Some(1)));
    let (first, second) = (opened.repos[0].id, opened.repos[1].id);
    let window = opened.window;
    assert!(exists(cx, window, repo_meta_tag_id(second)));
    assert!(!exists(cx, window, repo_use_meta_id(second)));
    assert!(!exists(cx, window, repo_meta_tag_id(first)));
    assert_eq!(
        label_of(cx, window, repo_use_meta_id(first)).as_deref(),
        Some("Use for intents and feedback")
    );
}

#[gpui_kit::test]
fn no_repository_is_tagged_until_one_is_chosen(cx: &mut TestAppContext) {
    let opened = open(cx, 2, Some(None));
    for repo in &opened.repos {
        assert!(!exists(cx, opened.window, repo_meta_tag_id(repo.id)));
        assert!(exists(cx, opened.window, repo_use_meta_id(repo.id)));
    }
}

#[gpui_kit::test]
fn choosing_puts_the_setting_and_waits_for_the_daemon(cx: &mut TestAppContext) {
    let opened = open(cx, 2, Some(Some(1)));
    let (first, second) = (opened.repos[0].id, opened.repos[1].id);
    let window = opened.window;
    click(cx, window, repo_use_meta_id(first));
    assert_eq!(
        *opened.fake.calls.borrow(),
        [FeedbackCall::SetSettings(FeedbackSettings {
            meta_repo_id: Some(first)
        })]
    );
    // Nothing is optimistic.
    assert!(exists(cx, window, repo_meta_tag_id(second)));
    assert!(!exists(cx, window, repo_meta_tag_id(first)));

    let chosen = FeedbackSettings {
        meta_repo_id: Some(first),
    };
    opened.fake.answer(Ok(chosen.clone()));
    load(cx, &opened, chosen);
    assert!(exists(cx, window, repo_meta_tag_id(first)));
    assert!(!exists(cx, window, repo_meta_tag_id(second)));
    assert!(exists(cx, window, repo_use_meta_id(second)));
}

#[gpui_kit::test]
fn a_failure_is_shown_under_the_list(cx: &mut TestAppContext) {
    let opened = open(cx, 2, Some(None));
    opened.fake.queue(Err(FakeIntents::api_error(
        404,
        "repo_not_found",
        "repo not found",
    )));
    click(cx, opened.window, repo_use_meta_id(opened.repos[0].id));
    assert_eq!(
        label_of(cx, opened.window, ElementId::from(REPO_META_ERROR_ID)).as_deref(),
        Some("Repo not found")
    );
    assert!(!exists(
        cx,
        opened.window,
        repo_meta_tag_id(opened.repos[0].id)
    ));
}

#[gpui_kit::test]
fn nothing_can_be_chosen_before_the_settings_load(cx: &mut TestAppContext) {
    let opened = open(cx, 2, None);
    for repo in &opened.repos {
        assert!(!exists(cx, opened.window, repo_use_meta_id(repo.id)));
        assert!(!exists(cx, opened.window, repo_meta_tag_id(repo.id)));
    }
}
