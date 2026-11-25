//! SmithAgents native app shell, built on GPUI through `gpui-kit`.
//!
//! [`run`] opens the main window: the docked [`shell::Shell`] under the Kit's
//! `Root`, connected to the local daemon through a [`link::DaemonLink`].
//! `host-desktop` stays a thin `main` that builds the [`link::LinkConfig`]
//! and calls [`run`].

pub mod app_state;
mod atomic_file;
pub mod layout_store;
pub mod link;
pub mod model;
pub mod settings;
pub mod shell;
pub mod shell_state;
pub mod theme_adapter;
pub mod update;
pub mod view_models;
pub mod views;

use gpui_kit::component::TitleBar;
use gpui_kit::{
    px, size, App, AppContext as _, Bounds, QuitMode, TitlebarOptions, WindowBounds, WindowOptions,
};
use smithagents_theme::native::NativeTheme;

use crate::app_state::AppStateStore;
use crate::layout_store::{restored_window_bounds, LayoutStore};
use crate::link::LinkConfig;
use crate::model::ShellModel;
use crate::shell::{Shell, ShellOptions};

/// The main window's title.
pub const WINDOW_TITLE: &str = "SmithAgents";

/// Runs the app until it quits; never returns.
///
/// `link` says where the daemon's home is (the app keeps its own files,
/// such as `app-layout.json`, there too) and how to start the daemon.
///
/// The app quits when its last window closes (not the macOS default, which
/// keeps the app alive) and, on Unix, on the first SIGTERM or SIGINT. The
/// daemon keeps running either way.
pub fn run(link: LinkConfig) -> ! {
    gpui_kit::application()
        // The Kit's bundled icons; without them every icon draws blank.
        .with_assets(gpui_kit::assets::Assets)
        .with_quit_mode(QuitMode::LastWindowClosed)
        .run(move |cx| {
            gpui_kit::init(cx);
            shell::init(cx);
            let native_theme = apply_startup_theme(cx);
            #[cfg(unix)]
            quit_on_termination_signals(cx);
            if let Err(e) = open_main_window(link, native_theme, cx) {
                eprintln!("smithagents: cannot open the main window: {e:#}");
                std::process::exit(1);
            }
            cx.activate(true);
        });
    // On macOS `App::quit` ends in `NSApplication terminate:`, which runs the
    // quit handlers and then exits the process itself, so `run` does not
    // return there. Exit explicitly for platforms where it does.
    std::process::exit(0)
}

/// Applies the Smith theme before the first window opens, replacing the
/// Kit's forced light default, and returns it for agent accents. Persisted
/// settings arrive in a later task.
fn apply_startup_theme(cx: &mut App) -> Option<NativeTheme> {
    use smithagents_theme::state::{AppearanceSettings, ThemeController};
    use smithagents_theme::tokens::{Appearance, ThemeSet};

    let set = ThemeSet::embedded();
    let controller =
        ThemeController::new(set.clone(), AppearanceSettings::default(), Appearance::Dark);
    let state = controller.state();
    theme_adapter::apply_theme(set, &state, None, cx)
}

fn open_main_window(
    link: LinkConfig,
    native_theme: Option<NativeTheme>,
    cx: &mut App,
) -> gpui_kit::Result<()> {
    let layout = LayoutStore::new(&link.home);
    let app_state = AppStateStore::new(&link.home);
    // The saved place, on its display if that display is still attached.
    let placed = layout.load_window().and_then(|saved| {
        let display = cx
            .displays()
            .into_iter()
            .find(|display| display.uuid().ok().map(|uuid| uuid.to_string()) == saved.display)?;
        let bounds = restored_window_bounds(&saved, Some(display.bounds()))?;
        Some((bounds, display.id()))
    });
    let (window_bounds, display_id) = match placed {
        Some((bounds, display)) => (bounds, Some(display)),
        None => (
            WindowBounds::Windowed(Bounds::centered(None, size(px(1200.), px(760.)), cx)),
            None,
        ),
    };
    let options = WindowOptions {
        window_bounds: Some(window_bounds),
        display_id,
        // The shell draws its own title bar (see `views::title_bar`); the
        // window keeps only the traffic lights and the app's title.
        titlebar: Some(TitlebarOptions {
            title: Some(WINDOW_TITLE.into()),
            ..TitleBar::title_bar_options()
        }),
        app_owns_titlebar_drag: true,
        ..Default::default()
    };
    gpui_kit::open_window(options, cx, |window, cx| {
        cx.new(|cx| {
            Shell::new(
                ShellOptions {
                    model: ShellModel::new(),
                    link: Some(link),
                    layout: Some(layout),
                    native_theme,
                    commands: None,
                    app_state: Some(app_state),
                    command_timeout: None,
                    updater: None,
                    info_status_duration: None,
                },
                window,
                cx,
            )
        })
    })?;
    Ok(())
}

/// Turns the first SIGTERM or SIGINT into a normal `App::quit`, so quit
/// handlers run. A second signal, if the quit hangs, terminates the process
/// the default way.
#[cfg(unix)]
fn quit_on_termination_signals(cx: &mut App) {
    use futures::channel::oneshot;
    use signal_hook::consts::{SIGINT, SIGTERM};
    use signal_hook::iterator::Signals;
    use signal_hook::low_level::emulate_default_handler;

    let mut signals = match Signals::new([SIGTERM, SIGINT]) {
        Ok(signals) => signals,
        Err(e) => {
            eprintln!("smithagents: cannot watch termination signals: {e}");
            return;
        }
    };
    let (tx, rx) = oneshot::channel::<i32>();
    let watcher = std::thread::Builder::new()
        .name("smithagents-signals".into())
        .spawn(move || {
            let mut pending = signals.forever();
            if let Some(signal) = pending.next() {
                // The receiver is gone only if the app already quit.
                let _ = tx.send(signal);
            }
            if let Some(signal) = pending.next() {
                if let Err(e) = emulate_default_handler(signal) {
                    eprintln!("smithagents: cannot terminate on signal {signal}: {e}");
                }
            }
        });
    if let Err(e) = watcher {
        eprintln!("smithagents: cannot start the signal watcher: {e}");
        return;
    }
    cx.spawn(async move |cx| {
        if let Ok(signal) = rx.await {
            eprintln!("smithagents: received signal {signal}, quitting");
            cx.update(|cx| cx.quit());
        }
    })
    .detach();
}
