//! SmithAgents native app shell, built on GPUI through `gpui-kit`.
//!
//! [`run`] opens the main window: the docked [`shell::Shell`] under the Kit's
//! `Root`, connected to the local daemon through a [`link::DaemonLink`], with
//! its chat tabs on the given web engine ([`tabs::TabManager`]).
//! `host-desktop` stays a thin `main` that builds the [`link::LinkConfig`],
//! picks the engine and calls [`run`].

pub mod app_state;
pub mod appearance;
pub mod assets;
mod atomic_file;
pub mod layout_store;
pub mod link;
pub mod model;
pub mod settings;
pub mod shell;
pub mod shell_state;
pub mod tabs;
pub mod theme_adapter;
pub mod update;
pub mod view_models;
pub mod views;

use gpui_kit::component::TitleBar;
use gpui_kit::{
    px, size, App, AppContext as _, Bounds, DisplayId, QuitMode, TitlebarOptions, WindowBounds,
    WindowOptions,
};
use smithagents_center_host::CenterTabHost;

use crate::app_state::AppStateStore;
use crate::assets::AppAssets;
use crate::layout_store::{restored_window_bounds, LayoutStore, MIN_WINDOW_SIZE};
use crate::link::LinkConfig;
use crate::model::ShellModel;
use crate::settings::AppSettingsStore;
use crate::shell::{Shell, ShellOptions, TabHostFactory};
use crate::tabs::{page_origin_from, PAGE_ORIGIN_ENV};

/// The main window's title.
pub const WINDOW_TITLE: &str = "SmithAgents";

/// Runs the app until it quits; never returns.
///
/// `link` says where the daemon's home is (the app keeps its own files,
/// such as `app-layout.json`, there too) and how to start the daemon.
/// `tab_host` builds the web engine for the chat tabs
/// ([`tabs::PlaceholderHost`] until a real one lands); it runs once the GPUI
/// app exists, as the main window opens. The engine's
/// [`CenterTabHost::shutdown`] runs once, when the window closes or the app
/// quits. Pages load from `smithagents://app`; a debug build loads them from
/// `SMITHAGENTS_WEB_ORIGIN` when set (the Vite dev server,
/// `http://localhost:5173`).
///
/// The menu bar ([`shell::app_menus`]) has Quit (⌘Q), the pane toggles and
/// Close Tab. The app quits when its last window closes (not the macOS
/// default, which keeps the app alive), on Quit and, on Unix, on the first
/// SIGTERM or SIGINT; each closes the window first, so the layout is saved
/// the same way. The daemon keeps running either way.
pub fn run(
    link: LinkConfig,
    tab_host: impl FnOnce(&mut App) -> Box<dyn CenterTabHost> + 'static,
) -> ! {
    gpui_kit::application()
        // The app's icons, then the Kit's; an icon neither has draws blank.
        .with_assets(AppAssets)
        .with_quit_mode(QuitMode::LastWindowClosed)
        .run(move |cx| {
            gpui_kit::init(cx);
            shell::init(cx);
            cx.set_menus(shell::app_menus());
            #[cfg(unix)]
            quit_on_termination_signals(cx);
            // The shell applies the saved theme as it opens, before the first
            // frame (the Kit's forced light default never paints).
            if let Err(e) = open_main_window(link, Box::new(tab_host), cx) {
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

fn open_main_window(
    link: LinkConfig,
    tab_host: TabHostFactory,
    cx: &mut App,
) -> gpui_kit::Result<()> {
    let page_origin = page_origin_from(
        std::env::var(PAGE_ORIGIN_ENV).ok().as_deref(),
        cfg!(debug_assertions),
    );
    tracing::info!(%page_origin, "chat pages load from this origin");
    let layout = LayoutStore::new(&link.home);
    let app_state = AppStateStore::new(&link.home);
    let settings = AppSettingsStore::new(&link.home);
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
    let options = main_window_options(Some(window_bounds), display_id);
    gpui_kit::open_window(options, cx, |window, cx| {
        cx.new(|cx| {
            Shell::new(
                ShellOptions {
                    model: ShellModel::new(),
                    link: Some(link),
                    layout: Some(layout),
                    settings: Some(settings),
                    system_appearance: None,
                    theme_set: None,
                    commands: None,
                    app_state: Some(app_state),
                    command_timeout: None,
                    updater: None,
                    info_status_duration: None,
                    tab_host: Some(tab_host),
                    page_origin: Some(page_origin),
                    connection_info: None,
                    link_failure: None,
                },
                window,
                cx,
            )
        })
    })?;
    Ok(())
}

/// The main window's options for `window_bounds` on `display_id`: the
/// app-drawn title bar (see `views::title_bar`; the window keeps only the
/// traffic lights and the app's title) and the minimum size
/// ([`MIN_WINDOW_SIZE`]), below which the title and the update chip would
/// meet.
pub fn main_window_options(
    window_bounds: Option<WindowBounds>,
    display_id: Option<DisplayId>,
) -> WindowOptions {
    WindowOptions {
        window_bounds,
        display_id,
        window_min_size: Some(MIN_WINDOW_SIZE),
        titlebar: Some(TitlebarOptions {
            title: Some(WINDOW_TITLE.into()),
            ..TitleBar::title_bar_options()
        }),
        app_owns_titlebar_drag: true,
        ..Default::default()
    }
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
            cx.update(shell::quit_app);
        }
    })
    .detach();
}
