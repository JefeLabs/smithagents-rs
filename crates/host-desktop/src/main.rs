//! SmithAgents desktop host: the macOS binary that runs the native shell.

use std::path::PathBuf;

use smithagents_app::link::LinkConfig;
use smithagents_app::tabs::PlaceholderHost;

/// The daemon's executable. It sits next to this one: in a dev build both
/// land in `target/<profile>/`. `cargo run -p host-desktop` does not build
/// it; when it is missing the shell's banner shows the start error.
const DAEMON_BIN: &str = "smithagentsd";

fn main() {
    init_logging();
    // The same home rule the daemon uses, made absolute like the daemon does.
    let home = smithagents_core::daemon::home_from_env();
    let home = std::path::absolute(&home).unwrap_or(home);
    let daemon_bin = daemon_bin();
    tracing::info!("home {}, daemon {}", home.display(), daemon_bin.display());
    // The daemon reports `smithagents_core::VERSION`; the link refuses any
    // other version, so both sides use the same constant. It refuses another
    // `smithagents_core::PROTOCOL` as well, which is what tells two builds of
    // one version apart.
    let config = LinkConfig::new(home, daemon_bin, smithagents_core::VERSION.to_owned());
    // The chat tabs' web engine, built once the GPUI app exists: a
    // placeholder that draws nothing and logs (at debug) until the real
    // engine lands.
    smithagents_app::run(config, |_| Box::new(PlaceholderHost))
}

fn daemon_bin() -> PathBuf {
    let name = format!("{DAEMON_BIN}{}", std::env::consts::EXE_SUFFIX);
    match std::env::current_exe() {
        Ok(exe) => exe.with_file_name(name),
        Err(e) => {
            tracing::warn!("cannot locate this executable ({e}); starting {name} from PATH");
            PathBuf::from(name)
        }
    }
}

/// Logs go to stderr. `SMITHAGENTS_LOG` sets the level (`error`, `warn`,
/// `info`, `debug`, `trace`); the default is `info`.
///
/// Only `tracing` events are printed. The `log` crate is deliberately not
/// bridged (no `tracing-log`/`LogTracer`): tungstenite, under the daemon
/// client's event stream, logs the raw WebSocket handshake, including the
/// `Authorization` header with the daemon token, at `log::trace!`. If `log`
/// is ever bridged, cap it at `debug`.
///
/// So the subscriber is set with `set_global_default`, never with `init()`:
/// `init()` is itself the bridge whenever tracing-subscriber's `tracing-log`
/// feature is on, and any dependency can turn that feature on.
fn init_logging() {
    let level = std::env::var("SMITHAGENTS_LOG")
        .ok()
        .and_then(|level| level.parse::<tracing::Level>().ok())
        .unwrap_or(tracing::Level::INFO);
    let subscriber = tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_max_level(level)
        .finish();
    tracing::subscriber::set_global_default(subscriber)
        .expect("logging is set up once, before anything else");
}

#[cfg(test)]
mod tests {
    #[test]
    fn logging_does_not_forward_the_log_crate() {
        // tungstenite logs the WebSocket handshake, `Authorization` header
        // and daemon token included, through `log` at trace. Whatever
        // features tracing-subscriber was built with, nothing may carry
        // those records into this process's log.
        std::env::set_var("SMITHAGENTS_LOG", "trace");
        super::init_logging();
        assert_eq!(log::max_level(), log::LevelFilter::Off);
        assert!(!log::log_enabled!(log::Level::Error));
    }
}
