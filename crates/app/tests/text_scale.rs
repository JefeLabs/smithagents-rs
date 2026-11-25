//! Every region grows with the text scale: at the largest text size the
//! Settings pane offers (150%, through the real `set_appearance` path), each
//! piece of text lies inside the box drawn for it, nothing overlaps, and the
//! layout math the web engine relies on (the center rectangle, the status
//! bar's height) follows the same computed sizes.
//!
//! Kit names are imported one by one: with `test-support` on, a glob import
//! would bring in GPUI's `test` attribute and shadow the built-in `#[test]`.

mod common;

use std::cell::RefCell;
use std::rc::Rc;

use common::ui::{agent, fake_connection, open_shell_options};
use futures::future::LocalBoxFuture;
use futures::FutureExt as _;
use gpui_kit::base::{test_support, Root};
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{
    px, AppContext as _, Bounds, ElementId, Entity, Pixels, TestAppContext, WindowHandle,
};
use smithagents_app::layout_store::{status_bar_height, title_bar_height};
use smithagents_app::model::{Action, Connection, ShellModel};
use smithagents_app::shell::ShellOptions;
use smithagents_app::shell_state::ShellState;
use smithagents_app::update::{UpdateState, Updater};
use smithagents_app::view_models::{center_rect, LeftPaneMode, StatusMessage};
use smithagents_app::views::agents::{agent_marker_id, agent_row_id, SWATCH_ID};
use smithagents_app::views::center::CONTENT_ID;
use smithagents_app::views::status_bar::{STATUS_BAR_ID, STATUS_TEXT_ID, STATUS_VERSION_ID};
use smithagents_app::views::tabs::tab_title_id;
use smithagents_app::views::title_bar::{TITLE_BAR_ID, TITLE_TEXT_ID, UPDATE_CHIP_ID};
use smithagents_center_host::PixelRect;
use smithagents_core::agent::Agent;

/// The largest text scale the design tokens allow.
const LARGEST: f64 = 1.5;

/// Always finds an update, so the chip shows.
struct UpdateAvailable;

impl Updater for UpdateAvailable {
    fn check(&self) -> LocalBoxFuture<'static, UpdateState> {
        async {
            UpdateState::Available {
                version: "9.9.9".into(),
            }
        }
        .boxed_local()
    }

    fn install(&self, _: String) -> LocalBoxFuture<'static, Result<(), String>> {
        futures::future::pending().boxed_local()
    }
}

/// A connected model with `agents`, each of them open in a tab.
fn connected_with_tabs(agents: &[Agent]) -> ShellModel {
    let mut model = ShellModel::new();
    model.apply(Action::Snapshot {
        agents: agents.to_vec(),
        repos: vec![],
        latest_seq: 1,
    });
    model.apply(Action::ConnectionChanged(Connection::Connected));
    model
}

/// Opens the shell with an update available, the tabs' connection faked
/// and `agents` open in tabs, then sets the text scale to `scale` through
/// the Settings path.
fn open_at(
    cx: &mut TestAppContext,
    agents: &[Agent],
    scale: f64,
) -> (WindowHandle<Root>, Entity<ShellState>) {
    let (window, shell) = open_shell_options(
        cx,
        ShellOptions {
            model: connected_with_tabs(agents),
            updater: Some(Rc::new(UpdateAvailable)),
            connection_info: Some(fake_connection()),
            ..Default::default()
        },
        (1200., 800.),
    );
    let state = shell.read_with(cx, |shell, _| shell.state().clone());
    state.update(cx, |state, cx| {
        for agent in agents {
            state.dispatch(Action::OpenTab(agent.id), cx);
        }
        let mut settings = state.appearance_settings().clone();
        settings.text_scale = scale;
        state.set_appearance(settings, cx);
    });
    cx.run_until_parked();
    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        window.render_frame(cx);
        assert_eq!(
            window.rem_size(),
            px(16. * scale as f32),
            "the text scale reached the window"
        );
    })
    .unwrap();
    (window, state)
}

fn planets() -> Vec<Agent> {
    ["Mercury", "Venus", "Earth", "Mars"]
        .into_iter()
        .map(agent)
        .collect()
}

/// The text painted in the last frame inside the element `id`.
fn texts_within(window: &gpui_kit::Window, id: &ElementId) -> Vec<(String, Bounds<Pixels>)> {
    test_support::texts(window)
        .into_iter()
        .filter(|text| text.path().contains(id))
        .map(|text| (text.text().to_owned(), text.bounds()))
        .collect()
}

/// `inner` lies inside `outer`, edges included, with a little slack for
/// sub-pixel rounding.
fn assert_inside(inner: Bounds<Pixels>, outer: Bounds<Pixels>, what: &str) {
    let slack = px(0.01);
    assert!(
        inner.top() >= outer.top() - slack
            && inner.bottom() <= outer.bottom() + slack
            && inner.left() >= outer.left() - slack
            && inner.right() <= outer.right() + slack,
        "{what}: {inner:?} is not inside {outer:?}"
    );
}

#[gpui_kit::test]
fn agent_rows_hold_their_two_lines_and_never_overlap_at_the_largest_text(cx: &mut TestAppContext) {
    let agents = planets();
    let (window, _) = open_at(cx, &agents, LARGEST);
    cx.update_window(window.into(), |_, window, _| {
        let mut rows = Vec::new();
        for agent in &agents {
            let id = agent_row_id(agent.id);
            let row = window.find(id.clone()).bounds();
            let texts = texts_within(window, &id);
            assert_eq!(texts.len(), 2, "name and state words: {texts:?}");
            for (text, bounds) in texts {
                assert_inside(bounds, row, &text);
            }
            rows.push(row);
        }
        // Same creation time: the list orders them by id, so by position here.
        rows.sort_by(|a, b| a.top().partial_cmp(&b.top()).unwrap());
        for pair in rows.windows(2) {
            assert!(
                pair[0].bottom() <= pair[1].top(),
                "rows overlap: {:?} then {:?}",
                pair[0],
                pair[1]
            );
        }
    })
    .unwrap();
}

#[gpui_kit::test]
fn compact_markers_hold_their_initials_at_the_largest_text(cx: &mut TestAppContext) {
    let agents = planets();
    let (window, state) = open_at(cx, &agents, LARGEST);
    state.update(cx, |state, cx| {
        state.set_left_pane_mode(LeftPaneMode::Compact, cx)
    });
    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        window.render_frame(cx);
        let mut markers = Vec::new();
        for agent in &agents {
            let id = agent_marker_id(agent.id);
            let marker = window.find(id.clone()).bounds();
            let swatch = window.within(id.clone()).find(SWATCH_ID).bounds();
            assert_inside(swatch, marker, "swatch");
            let texts = texts_within(window, &id);
            assert_eq!(texts.len(), 1, "the initial: {texts:?}");
            assert_inside(texts[0].1, swatch, &texts[0].0);
            markers.push(marker);
        }
        markers.sort_by(|a, b| a.top().partial_cmp(&b.top()).unwrap());
        for pair in markers.windows(2) {
            assert!(pair[0].bottom() <= pair[1].top(), "{pair:?}");
        }
    })
    .unwrap();
}

#[gpui_kit::test]
fn the_status_bar_holds_its_text_at_the_largest_text(cx: &mut TestAppContext) {
    let (window, state) = open_at(cx, &[], LARGEST);
    state.update(cx, |state, cx| {
        state.set_status(StatusMessage::error("Something went wrong"), cx)
    });
    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        let bar = window.find(STATUS_BAR_ID).bounds();
        assert_eq!(bar.size.height, status_bar_height(window.rem_size()));
        assert!(bar.size.height > px(24.), "the bar grew: {bar:?}");
        assert_eq!(bar.bottom(), window.viewport_size().height);
        assert_inside(window.find(STATUS_TEXT_ID).bounds(), bar, "message");
        assert_inside(window.find(STATUS_VERSION_ID).bounds(), bar, "version");
    })
    .unwrap();
}

#[gpui_kit::test]
fn the_title_bar_and_its_update_chip_hold_their_text_at_the_largest_text(cx: &mut TestAppContext) {
    let (window, _) = open_at(cx, &[], LARGEST);
    cx.update_window(window.into(), |_, window, _| {
        let bar = window.find(TITLE_BAR_ID).bounds();
        assert_eq!(bar.size.height, title_bar_height(window.rem_size()));
        assert!(bar.size.height > px(34.), "the bar grew: {bar:?}");
        assert_inside(window.find(TITLE_TEXT_ID).bounds(), bar, "title");
        let chip = window.find(UPDATE_CHIP_ID).bounds();
        assert_inside(chip, bar, "chip");
        let label = texts_within(window, &ElementId::from(UPDATE_CHIP_ID));
        assert_eq!(label.len(), 1, "{label:?}");
        assert_inside(label[0].1, chip, &label[0].0);
    })
    .unwrap();
}

#[gpui_kit::test]
fn tab_titles_fit_their_tabs_at_the_largest_text(cx: &mut TestAppContext) {
    let agents = planets();
    let (window, _) = open_at(cx, &agents, LARGEST);
    cx.update_window(window.into(), |_, window, _| {
        for (ix, agent) in agents.iter().enumerate() {
            let tab = window.within("center-tabs").find(ix).bounds();
            let title = window.find(tab_title_id(ix)).bounds();
            assert_inside(title, tab, &agent.name);
        }
    })
    .unwrap();
}

#[gpui_kit::test]
fn the_center_rect_is_the_content_area_at_the_largest_text(cx: &mut TestAppContext) {
    let agents = planets();
    let (window, state) = open_at(cx, &agents, LARGEST);
    let seen: Rc<RefCell<Vec<PixelRect>>> = Rc::default();
    state.update(cx, |state, cx| {
        let seen = seen.clone();
        state.set_center_bounds_listener(move |rect, _| seen.borrow_mut().push(rect), cx);
    });
    cx.run_until_parked();
    let mut expected = None;
    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        let content = window.find(CONTENT_ID).bounds();
        let title = window.find(TITLE_BAR_ID).bounds();
        let bar = window.find(STATUS_BAR_ID).bounds();
        assert!(content.top() >= title.bottom());
        assert_eq!(
            content.bottom(),
            window.viewport_size().height - status_bar_height(window.rem_size()),
            "the content ends at the status bar's top"
        );
        assert_eq!(content.bottom(), bar.top());
        expected = Some(center_rect(
            content.origin.x.into(),
            content.origin.y.into(),
            content.size.width.into(),
            content.size.height.into(),
            window.scale_factor(),
        ));
    })
    .unwrap();
    cx.run_until_parked();
    assert_eq!(seen.borrow().last().copied(), expected);
}
