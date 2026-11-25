//! `smithagentsd`: the SmithAgents daemon.
//!
//! Startup: take the instance lock (exit 3 if another daemon holds it), open
//! the store, bind 127.0.0.1 on a random port, generate the token, write the
//! lockfile, then serve until idle, SIGTERM or ctrl-c. On the way out the
//! lockfile is removed and the process exits 0.

use std::error::Error;
use std::ffi::OsString;
use std::future::Future;
use std::io;
use std::net::Ipv4Addr;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::atomic::AtomicUsize;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::Router;
use smithagents_core::daemon::DaemonInfo;
use smithagents_server::api::{router, AppState};
use smithagents_server::events::EventBus;
use smithagents_server::harness::claude::ClaudeCodeHarness;
use smithagents_server::intents::IntentService;
use smithagents_server::lifecycle::{
    acquire_instance_lock, generate_instance_id, generate_token, remove_lockfile, write_lockfile,
    IdleMonitor, LockError,
};
use smithagents_server::paths::Paths;
use smithagents_server::queue::Scheduler;
use smithagents_server::service::AgentService;
use smithagents_server::store::Store;
use tokio::net::TcpListener;

const USAGE: &str =
    "usage: smithagentsd [--home <dir>] [--claude-bin <path>] [--idle-timeout-secs <n>]";
const EXIT_USAGE: u8 = 2;
const EXIT_ALREADY_RUNNING: u8 = 3;

const DEFAULT_IDLE_TIMEOUT: Duration = Duration::from_secs(600);
/// A running turn with no harness output for this long is shown as quiet.
const QUIET_AFTER: Duration = Duration::from_secs(30);
/// Events kept for clients that reconnect with `since`.
const EVENT_BUFFER: usize = 4096;
/// How long open requests get to finish once shutdown starts.
const SHUTDOWN_GRACE: Duration = Duration::from_secs(5);

#[derive(Debug, PartialEq)]
struct Options {
    /// `None` means `SMITHAGENTS_HOME`, else `~/.smithagents`.
    home: Option<PathBuf>,
    claude_bin: PathBuf,
    idle_timeout: Duration,
}

#[derive(Debug, PartialEq)]
enum Invocation {
    Run(Options),
    Help,
}

fn parse_args(args: impl IntoIterator<Item = OsString>) -> Result<Invocation, String> {
    let mut opts = Options {
        home: None,
        claude_bin: PathBuf::from("claude"),
        idle_timeout: DEFAULT_IDLE_TIMEOUT,
    };
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        let Some(flag) = arg.to_str() else {
            return Err(format!("unknown argument {arg:?}"));
        };
        match flag {
            "-h" | "--help" => return Ok(Invocation::Help),
            "--home" => opts.home = Some(flag_value(&mut args, flag)?.into()),
            "--claude-bin" => opts.claude_bin = flag_value(&mut args, flag)?.into(),
            "--idle-timeout-secs" => {
                let raw = flag_value(&mut args, flag)?;
                let secs = raw
                    .to_str()
                    .and_then(|s| s.parse::<u64>().ok())
                    .ok_or_else(|| format!("{flag} expects whole seconds, got {raw:?}"))?;
                opts.idle_timeout = Duration::from_secs(secs);
            }
            other => return Err(format!("unknown argument '{other}'")),
        }
    }
    Ok(Invocation::Run(opts))
}

fn flag_value(args: &mut impl Iterator<Item = OsString>, flag: &str) -> Result<OsString, String> {
    match args.next() {
        Some(v) if !v.is_empty() => Ok(v),
        _ => Err(format!("{flag} needs a value")),
    }
}

/// `--home` when given, else `SMITHAGENTS_HOME` or `~/.smithagents`, made
/// absolute so stored worktree paths do not depend on the working directory.
fn resolve_paths(home: Option<PathBuf>) -> io::Result<Paths> {
    let home = match home {
        Some(home) => home,
        None => Paths::from_env().home,
    };
    Ok(Paths {
        home: std::path::absolute(home)?,
    })
}

fn main() -> ExitCode {
    let opts = match parse_args(std::env::args_os().skip(1)) {
        Ok(Invocation::Run(opts)) => opts,
        Ok(Invocation::Help) => {
            println!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        Err(msg) => {
            eprintln!("smithagentsd: {msg}\n{USAGE}");
            return ExitCode::from(EXIT_USAGE);
        }
    };
    // Logs go to stderr. Never log the token, and never add request tracing:
    // `/v1/events` accepts the token in its query string.
    tracing_subscriber::fmt()
        .with_writer(io::stderr)
        .with_max_level(tracing::Level::INFO)
        .init();

    let paths = match resolve_paths(opts.home.clone()) {
        Ok(paths) => paths,
        Err(e) => {
            eprintln!("smithagentsd: could not resolve the home directory: {e}");
            return ExitCode::FAILURE;
        }
    };
    // Held until main returns; the lockfile is removed before that.
    let _instance = match acquire_instance_lock(&paths) {
        Ok(lock) => lock,
        Err(LockError::AlreadyRunning) => {
            eprintln!(
                "smithagentsd: another daemon is already running for {}",
                paths.home.display()
            );
            return ExitCode::from(EXIT_ALREADY_RUNNING);
        }
        Err(e) => {
            eprintln!("smithagentsd: {e}");
            return ExitCode::FAILURE;
        }
    };
    let runtime = match tokio::runtime::Runtime::new() {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("smithagentsd: could not start the async runtime: {e}");
            return ExitCode::FAILURE;
        }
    };
    let result = runtime.block_on(run(&paths, opts));
    // Turns still in flight are dropped here, which kills their processes.
    runtime.shutdown_timeout(Duration::from_secs(1));
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            tracing::error!("smithagentsd stopped: {e}");
            ExitCode::FAILURE
        }
    }
}

/// The model for polish passes: unset or blank means the CLI's default.
fn polish_model_from(value: Option<String>) -> Option<String> {
    value.map(|v| v.trim().to_owned()).filter(|v| !v.is_empty())
}

async fn run(paths: &Paths, opts: Options) -> Result<(), Box<dyn Error>> {
    // Registered before the lockfile exists, so a signal cannot leave it behind.
    let signal = shutdown_signal()?;
    paths.ensure_dirs()?;
    let store = Store::open(&paths.db_path()).await?;
    // Turns recorded as running belonged to a previous process and died with it.
    let stale = store.mark_stale_running_agents_dead().await?;
    tracing::info!(
        count = stale,
        "marked agents left working or quiet by a previous daemon as dead"
    );
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await?;
    let port = listener.local_addr()?.port();
    let token = generate_token()?;
    let instance_id = generate_instance_id()?;

    let bus = EventBus::new(EVENT_BUFFER);
    let harness = Arc::new(ClaudeCodeHarness {
        bin: opts.claude_bin,
    });
    let service = AgentService::new(
        store.clone(),
        bus.clone(),
        harness.clone(),
        paths.clone(),
        QUIET_AFTER,
    );
    let intents = IntentService::new(
        store.clone(),
        bus.clone(),
        harness,
        service.clone(),
        paths.clone(),
    )
    .with_polish_model(polish_model_from(
        std::env::var("SMITHAGENTS_POLISH_MODEL").ok(),
    ));
    // Polish passes that were running when the last daemon died are marked
    // failed, so the user can retry them. Must precede the scheduler.
    let recovered = intents.recover().await?;
    tracing::info!(count = recovered, "recovered intents left polishing");
    // Runs a first pass at once, so queued intents whose dependencies
    // resolved while the daemon was down start now.
    let scheduler = Scheduler::start(intents.clone(), service.clone(), store.clone(), bus.clone());
    let ws_clients = Arc::new(AtomicUsize::new(0));
    let state = AppState {
        service: service.clone(),
        intents,
        bus,
        store,
        token: Arc::from(token.as_str()),
        instance_id: Arc::from(instance_id.as_str()),
        ws_clients: Arc::clone(&ws_clients),
        started_at: Instant::now(),
    };
    let info = DaemonInfo {
        pid: std::process::id(),
        port,
        token,
        version: smithagents_core::VERSION.to_owned(),
    };
    write_lockfile(paths, &info)?;
    tracing::info!(
        pid = info.pid,
        port,
        version = %info.version,
        home = %paths.home.display(),
        "listening on 127.0.0.1"
    );

    let idle = IdleMonitor::new(opts.idle_timeout, ws_clients, move || {
        service.running_agent_count()
    });
    let stop = async move {
        tokio::select! {
            () = idle.wait_idle_exit() => tracing::info!("idle, shutting down"),
            name = signal => tracing::info!("received {name}, shutting down"),
        }
    };
    let served = serve(listener, router(state), stop).await;
    // Let a pass in progress finish before the runtime goes away.
    scheduler.shutdown().await;
    remove_lockfile(paths);
    Ok(served?)
}

/// Serves until `stop` resolves, then lets open requests finish for at most
/// [`SHUTDOWN_GRACE`].
async fn serve(
    listener: TcpListener,
    app: Router,
    stop: impl Future<Output = ()> + Send + 'static,
) -> io::Result<()> {
    let (stopping_tx, stopping_rx) = tokio::sync::oneshot::channel::<()>();
    let server = axum::serve(listener, app).with_graceful_shutdown(async move {
        stop.await;
        let _ = stopping_tx.send(());
    });
    let grace_expired = async move {
        match stopping_rx.await {
            Ok(()) => tokio::time::sleep(SHUTDOWN_GRACE).await,
            // The server ended on its own; it is the branch that completes.
            Err(_) => std::future::pending().await,
        }
    };
    tokio::select! {
        served = server => served,
        () = grace_expired => {
            tracing::warn!("requests still open {SHUTDOWN_GRACE:?} after shutdown began; closing them");
            Ok(())
        }
    }
}

/// Resolves with the signal's name on SIGTERM or SIGINT (ctrl-c).
#[cfg(unix)]
fn shutdown_signal() -> io::Result<impl Future<Output = &'static str> + Send + 'static> {
    use tokio::signal::unix::{signal, SignalKind};
    let mut term = signal(SignalKind::terminate())?;
    let mut interrupt = signal(SignalKind::interrupt())?;
    Ok(async move {
        tokio::select! {
            _ = term.recv() => "SIGTERM",
            _ = interrupt.recv() => "SIGINT",
        }
    })
}

/// Resolves with the signal's name on ctrl-c.
#[cfg(not(unix))]
fn shutdown_signal() -> io::Result<impl Future<Output = &'static str> + Send + 'static> {
    Ok(async {
        match tokio::signal::ctrl_c().await {
            Ok(()) => "ctrl-c",
            // Without a working handler, never resolve rather than shut down.
            Err(_) => std::future::pending().await,
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Invocation, String> {
        parse_args(args.iter().map(OsString::from))
    }

    #[test]
    fn defaults_without_flags() {
        assert_eq!(
            parse(&[]),
            Ok(Invocation::Run(Options {
                home: None,
                claude_bin: PathBuf::from("claude"),
                idle_timeout: Duration::from_secs(600),
            }))
        );
    }

    #[test]
    fn all_flags() {
        assert_eq!(
            parse(&[
                "--idle-timeout-secs",
                "5",
                "--home",
                "/h",
                "--claude-bin",
                "/bin/fake"
            ]),
            Ok(Invocation::Run(Options {
                home: Some(PathBuf::from("/h")),
                claude_bin: PathBuf::from("/bin/fake"),
                idle_timeout: Duration::from_secs(5),
            }))
        );
        assert_eq!(parse(&["--help"]), Ok(Invocation::Help));
    }

    #[test]
    fn polish_model_env_value() {
        assert_eq!(polish_model_from(None), None);
        assert_eq!(polish_model_from(Some(String::new())), None);
        assert_eq!(polish_model_from(Some("  ".into())), None);
        assert_eq!(
            polish_model_from(Some(" haiku ".into())),
            Some("haiku".into())
        );
    }

    #[test]
    fn relative_home_is_made_absolute() {
        let paths = resolve_paths(Some(PathBuf::from("rel-home"))).unwrap();
        assert!(paths.home.is_absolute(), "{}", paths.home.display());
        assert_eq!(
            paths.home,
            std::env::current_dir().unwrap().join("rel-home")
        );
        let abs = std::env::temp_dir().join("abs-home");
        assert_eq!(resolve_paths(Some(abs.clone())).unwrap().home, abs);
    }

    #[test]
    fn bad_arguments_are_rejected() {
        for (args, expected) in [
            (&["--bogus"][..], "unknown argument '--bogus'"),
            (&["extra"][..], "unknown argument 'extra'"),
            (&["--home"][..], "--home needs a value"),
            (&["--claude-bin", ""][..], "--claude-bin needs a value"),
            (&["--idle-timeout-secs", "-1"][..], "expects whole seconds"),
            (&["--idle-timeout-secs", "10m"][..], "expects whole seconds"),
        ] {
            let err = parse(args).unwrap_err();
            assert!(err.contains(expected), "{args:?}: {err}");
        }
    }
}
