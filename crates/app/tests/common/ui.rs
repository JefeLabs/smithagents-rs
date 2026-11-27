//! Helpers shared by the headless shell tests (`shell_ui`, `agents_pane`,
//! `status_bar`, `settings_pane`): opening the shell in a test window, test
//! agents, a fake OS appearance, and reading the colors the last frame
//! painted.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use chrono::{DateTime, Utc};
use gpui_kit::base::Root;
use gpui_kit::{
    px, size, App, AppContext as _, Background, Bounds, Entity, Pixels, Point, Quad, Subscription,
    TestAppContext, Window, WindowBounds, WindowHandle, WindowOptions,
};
use smithagents_app::model::ShellModel;
use smithagents_app::shell::{Shell, ShellOptions};
use smithagents_app::shell_state::ConnectionInfoSource;
use smithagents_app::theme_adapter::{srgb_to_hsla, AppearanceListener, SystemAppearanceSource};
use smithagents_core::agent::{Agent, AgentState};
use smithagents_core::ids::{AgentId, RepoId};
use smithagents_core::palette::AgentColor;
use smithagents_theme::native::NativeTheme;
use smithagents_theme::state::{AppearanceSettings, ThemeController, ThemeState};
use smithagents_theme::tokens::{Appearance, ThemeSet};

/// The Smith theme `appearance` resolves to.
pub fn theme_state(appearance: Appearance) -> ThemeState {
    ThemeController::new(
        ThemeSet::embedded().clone(),
        AppearanceSettings::default(),
        appearance,
    )
    .state()
}

pub fn agent(name: &str) -> Agent {
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

/// The daemon port the fake connection seam answers.
pub const TEST_PORT: u16 = 4321;
/// The daemon token the fake connection seam answers.
pub const TEST_TOKEN: &str = "test-token-not-a-real-one";

/// A connection seam (`ShellOptions::connection_info`) answering
/// [`TEST_PORT`] and [`TEST_TOKEN`], as a connected link would, so tabs open
/// without a daemon.
pub fn fake_connection() -> ConnectionInfoSource {
    Rc::new(|| Some((TEST_PORT, TEST_TOKEN.into())))
}

/// Opens the shell, offline (no daemon link, no layout file), in a
/// 1200x800 window.
pub fn open_shell(
    cx: &mut TestAppContext,
    model: ShellModel,
) -> (WindowHandle<Root>, Entity<Shell>) {
    open_shell_options(
        cx,
        ShellOptions {
            model,
            ..Default::default()
        },
        (1200., 800.),
    )
}

type SharedListener = Rc<RefCell<AppearanceListener>>;

/// The OS appearance as a test sets it: [`FakeAppearance::set`] calls every
/// listener the shell registered, as the OS would.
pub struct FakeAppearance {
    current: Cell<Appearance>,
    next_id: Cell<usize>,
    listeners: Rc<RefCell<Vec<(usize, SharedListener)>>>,
}

impl FakeAppearance {
    pub fn new(appearance: Appearance) -> Rc<Self> {
        Rc::new(Self {
            current: Cell::new(appearance),
            next_id: Cell::new(0),
            listeners: Rc::default(),
        })
    }

    /// Flips the OS appearance (listeners run even when it is unchanged, as
    /// the OS may report a change of another kind).
    pub fn set(&self, appearance: Appearance, cx: &mut App) {
        self.current.set(appearance);
        let listeners: Vec<_> = self
            .listeners
            .borrow()
            .iter()
            .map(|(_, listener)| listener.clone())
            .collect();
        for listener in listeners {
            (listener.borrow_mut())(appearance, cx);
        }
    }

    /// How many listeners are registered (dropped subscriptions are gone).
    pub fn listener_count(&self) -> usize {
        self.listeners.borrow().len()
    }
}

impl SystemAppearanceSource for FakeAppearance {
    fn current(&self, _: &App) -> Appearance {
        self.current.get()
    }

    fn observe(&self, _: &Window, listener: AppearanceListener) -> Subscription {
        let id = self.next_id.get();
        self.next_id.set(id + 1);
        self.listeners
            .borrow_mut()
            .push((id, Rc::new(RefCell::new(listener))));
        let listeners = self.listeners.clone();
        Subscription::new(move || listeners.borrow_mut().retain(|(other, _)| *other != id))
    }
}

/// Opens the shell from `options`. Unless `options` brings its own, the OS
/// appearance is a fake set to dark, so the default "System" theme is the
/// dark Smith theme.
pub fn open_shell_options(
    cx: &mut TestAppContext,
    mut options: ShellOptions,
    (width, height): (f32, f32),
) -> (WindowHandle<Root>, Entity<Shell>) {
    if options.system_appearance.is_none() {
        options.system_appearance = Some(FakeAppearance::new(Appearance::Dark));
    }
    cx.update(|cx| {
        gpui_kit::init(cx);
        smithagents_app::shell::init(cx);
        let bounds = Bounds {
            origin: Point::default(),
            size: size(px(width), px(height)),
        };
        let window_options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            ..Default::default()
        };
        let (window, shell) = gpui_kit::open_window(window_options, cx, |window, cx| {
            cx.new(|cx| Shell::new(options, window, cx))
        })
        .expect("open the shell window");
        (window.downcast::<Root>().expect("Base Root"), shell)
    })
}

/// The topmost quad painted over `at` in the last frame.
pub fn quad_at(window: &gpui_kit::Window, at: Point<Pixels>) -> Quad {
    let scale = window.scale_factor();
    let (x, y) = (f32::from(at.x) * scale, f32::from(at.y) * scale);
    window
        .painted_quads()
        .into_iter()
        .filter(|quad| {
            // Borders are painted as full-size quads masked to their edge.
            [quad.bounds, quad.content_mask.bounds].iter().all(|b| {
                x >= b.origin.x.0
                    && x < b.origin.x.0 + b.size.width.0
                    && y >= b.origin.y.0
                    && y < b.origin.y.0 + b.size.height.0
            })
        })
        .max_by_key(|quad| quad.order)
        .expect("a quad is painted there")
}

/// The color of the topmost quad painted over `at`, in the last frame.
pub fn painted_at(window: &gpui_kit::Window, at: Point<Pixels>) -> Background {
    quad_at(window, at).background
}

pub fn token_background(native: &NativeTheme, token: &str) -> Background {
    srgb_to_hsla(native.colors[token]).into()
}

/// The dark Smith theme the tests open the shell in.
pub fn dark_native() -> NativeTheme {
    ThemeSet::embedded()
        .native(&theme_state(Appearance::Dark).theme)
        .unwrap()
}
