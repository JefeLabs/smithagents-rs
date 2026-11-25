//! SmithAgents native app shell, built on GPUI through `gpui-kit`.
//!
//! For now this is the spike window: one window titled "SmithAgents" with a
//! label, mounted under the Kit's `Root`. Later tasks grow it into the
//! docked shell; `host-desktop` stays a thin `main` that calls [`run`].

pub mod link;
pub mod model;

use gpui_kit::component::label::Label;
use gpui_kit::{
    div, px, size, App, AppContext as _, Bounds, Context, IntoElement, ParentElement as _,
    QuitMode, Render, Styled as _, TitlebarOptions, Window, WindowBounds, WindowOptions,
};

/// The main window's title.
pub const WINDOW_TITLE: &str = "SmithAgents";

/// Placeholder content for the main window. The Kit's `Root` paints the
/// theme background, foreground and font around it, so it needs no colors.
struct SpikeView;

impl Render for SpikeView {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .child(Label::new("SmithAgents"))
    }
}

/// Runs the app until it quits; never returns.
///
/// The app quits when its last window closes (not the macOS default, which
/// keeps the app alive) and, on Unix, on the first SIGTERM or SIGINT.
pub fn run() -> ! {
    gpui_kit::application()
        .with_quit_mode(QuitMode::LastWindowClosed)
        .run(|cx| {
            gpui_kit::init(cx);
            #[cfg(unix)]
            quit_on_termination_signals(cx);
            if let Err(e) = open_main_window(cx) {
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

fn open_main_window(cx: &mut App) -> gpui_kit::Result<()> {
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
            None,
            size(px(960.), px(640.)),
            cx,
        ))),
        titlebar: Some(TitlebarOptions {
            title: Some(WINDOW_TITLE.into()),
            ..Default::default()
        }),
        ..Default::default()
    };
    gpui_kit::open_window(options, cx, |_, cx| cx.new(|_| SpikeView))?;
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
