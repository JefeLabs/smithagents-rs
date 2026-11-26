//! Runs a plugin as a WebAssembly component under Wasmtime: the sandbox for
//! third-party plugins.
//!
//! A component reaches the outside world only through the `host` interface
//! (HTTP under the policy, and logging) and a small WASI subset: streams,
//! clocks, randomness and a command-line environment with no variables, no
//! arguments and no stdio. Filesystem, sockets and `wasi:http` are not in the
//! linker, so a component that imports them fails to load. Every call runs
//! in a fresh instance under a memory limit, a CPU budget and a call timeout.

use std::path::Path;
use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use smithagents_plugin_protocol::{
    Auth, ContractError, HostMode, HttpError, HttpMethod, HttpRequest, HttpResponse, Item,
    LogLevel, PluginError, SyncPage, SyncRequest,
};
use wasmtime::component::{Component, HasData, HasSelf, Linker, ResourceTable};
use wasmtime::{
    Cache, CacheConfig, Config, Engine, EngineWeak, ResourceLimiter, Store, Trap, UpdateDeadline,
};
use wasmtime_wasi::{WasiCtx, WasiCtxView, WasiView};

use crate::runtime::{CallError, LoadError, PluginRuntime};
use crate::services::CallHandle;

mod bindings {
    wasmtime::component::bindgen!({
        path: "../plugin-protocol/wit/plugin.wit",
        world: "plugin",
        imports: { default: async },
        exports: { default: async },
    });
}

use bindings::smithagents::plugin::{host, types as wit};
use bindings::PluginPre;

/// How often the epoch advances. The CPU budget is counted in these ticks.
const TICK: Duration = Duration::from_millis(100);
/// The contract's page limits, checked before any JSON is parsed: a guest
/// could otherwise hand the host up to its whole memory to parse.
const MAX_UPSERTS: usize = 500;
const MAX_FIELDS_BYTES: usize = 64 * 1024;
/// Caps on what one instance may create. Every memory and table also counts
/// against the one memory budget.
const MAX_INSTANCES: usize = 64;
const MAX_MEMORIES: usize = 16;
const MAX_TABLES: usize = 64;

/// The one Wasmtime engine of a daemon, with its epoch ticker thread. The
/// ticker stops once the engine and every runtime built on it are dropped.
pub struct WasmEngine {
    engine: Engine,
}

impl WasmEngine {
    /// With `cache_dir`, compiled components are cached on disk there.
    pub fn new(cache_dir: Option<&Path>) -> Result<WasmEngine, LoadError> {
        let mut config = Config::new();
        config.epoch_interruption(true);
        if let Some(dir) = cache_dir {
            let mut cache = CacheConfig::new();
            cache.with_directory(std::path::absolute(dir)?);
            let cache = Cache::new(cache).map_err(|e| engine_error("cache", &e))?;
            config.cache(Some(cache));
        }
        let engine = Engine::new(&config).map_err(|e| engine_error("engine", &e))?;
        spawn_ticker(engine.weak())?;
        Ok(WasmEngine { engine })
    }
}

fn engine_error(what: &str, err: &wasmtime::Error) -> LoadError {
    LoadError::Io(std::io::Error::other(format!(
        "cannot start the WebAssembly {what}: {}",
        describe(err)
    )))
}

/// The error and its causes on one line: some Wasmtime messages span lines.
fn describe(err: &wasmtime::Error) -> String {
    format!("{err:#}")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Advances the epoch every [`TICK`] on its own thread, so busy guests on the
/// async workers cannot hold it back.
fn spawn_ticker(engine: EngineWeak) -> Result<(), LoadError> {
    std::thread::Builder::new()
        .name("smithagents-wasm-epoch".into())
        .spawn(move || loop {
            std::thread::sleep(TICK);
            match engine.upgrade() {
                Some(engine) => engine.increment_epoch(),
                None => return,
            }
        })?;
    Ok(())
}

/// What one call may use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WasmLimits {
    /// Linear memories and tables of one instance, together.
    pub memory_bytes: usize,
    /// Time running guest code; waiting on the host does not count.
    pub cpu: Duration,
    /// Wall time for the whole call.
    pub call_timeout: Duration,
}

impl Default for WasmLimits {
    fn default() -> WasmLimits {
        WasmLimits {
            memory_bytes: 64 * 1024 * 1024,
            cpu: Duration::from_secs(10),
            call_timeout: Duration::from_secs(60),
        }
    }
}

impl WasmLimits {
    /// The CPU budget in whole ticks, at least one.
    fn cpu_ticks(&self) -> u64 {
        let ticks = self.cpu.as_nanos().div_ceil(TICK.as_nanos());
        u64::try_from(ticks).unwrap_or(u64::MAX).max(1)
    }
}

/// A compiled plugin component, ready to instantiate once per call.
pub struct WasmRuntime {
    engine: Engine,
    pre: PluginPre<State>,
    limits: WasmLimits,
}

impl WasmRuntime {
    /// Compiles `component`, links it and instantiates it once to check that
    /// it imports nothing beyond what the host provides.
    pub async fn load(
        engine: &WasmEngine,
        component: &Path,
        limits: WasmLimits,
    ) -> Result<WasmRuntime, LoadError> {
        let bytes = tokio::fs::read(component).await?;
        let engine = engine.engine.clone();
        let compiler = engine.clone();
        // Compiling is CPU-bound and can take seconds.
        let component =
            tokio::task::spawn_blocking(move || Component::from_binary(&compiler, &bytes))
                .await
                .map_err(|e| invalid(e.to_string()))?
                .map_err(|e| invalid(describe(&e)))?;

        let linker = linker(&engine).map_err(|e| invalid(describe(&e)))?;
        let pre = linker
            .instantiate_pre(&component)
            .map_err(|e| LoadError::MissingImport {
                message: describe(&e),
            })?;
        let pre = PluginPre::new(pre).map_err(|e| invalid(describe(&e)))?;
        let runtime = WasmRuntime {
            engine,
            pre,
            limits,
        };

        let mut store = runtime.store(None);
        tokio::time::timeout(
            limits.call_timeout,
            runtime.pre.instantiate_async(&mut store),
        )
        .await
        .map_err(|_| invalid("instantiation timed out".into()))?
        .map_err(|e| invalid(format!("cannot instantiate: {}", describe(&e))))?;
        Ok(runtime)
    }

    /// A fresh store for one call: the memory limit and the CPU budget.
    fn store(&self, call: Option<CallHandle>) -> Store<State> {
        let state = State {
            // No environment, arguments or preopens (the builder adds none);
            // stdin reads as closed and stdout and stderr discard.
            wasi: WasiCtx::builder()
                .stdin(tokio::io::empty())
                .stdout(tokio::io::empty())
                .stderr(tokio::io::empty())
                .build(),
            table: ResourceTable::new(),
            limiter: Limiter {
                left: self.limits.memory_bytes,
            },
            call,
        };
        let mut store = Store::new(&self.engine, state);
        store.limiter(|s| &mut s.limiter);
        // One tick of budget each time the deadline passes while guest code
        // runs; then yield, so a busy guest does not hold an async worker.
        // The next deadline is set after the yield, so time spent waiting on
        // the host or the scheduler is not charged.
        let mut ticks_left = self.limits.cpu_ticks();
        store.set_epoch_deadline(1);
        store.epoch_deadline_callback(move |_| {
            ticks_left = ticks_left.saturating_sub(1);
            Ok(if ticks_left == 0 {
                UpdateDeadline::Interrupt
            } else {
                UpdateDeadline::Yield(1)
            })
        });
        store
    }
}

fn invalid(message: String) -> LoadError {
    LoadError::Invalid { message }
}

#[async_trait]
impl PluginRuntime for WasmRuntime {
    fn mode(&self) -> HostMode {
        HostMode::Wasm
    }

    async fn sync(&self, call: CallHandle, req: SyncRequest) -> Result<SyncPage, CallError> {
        let req = wit::SyncRequest {
            resource: req.resource,
            cursor: req.cursor,
            settings: req.settings.to_string(),
        };
        let run = async {
            let mut store = self.store(Some(call));
            let plugin = self.pre.instantiate_async(&mut store).await?;
            plugin.call_sync(&mut store, &req).await
        };
        // Dropping the call on a timeout drops its store and instance.
        let outcome = tokio::time::timeout(self.limits.call_timeout, run)
            .await
            .map_err(|_| CallError::Timeout)?;
        let page = match outcome {
            Ok(Ok(page)) => page_from_wit(page)?,
            Ok(Err(err)) => return Err(CallError::Plugin(plugin_error_from_wit(err))),
            Err(err) => return Err(call_error(&err)),
        };
        page.validate()?;
        Ok(page)
    }
}

fn call_error(err: &wasmtime::Error) -> CallError {
    if err.downcast_ref::<MemoryLimit>().is_some() {
        return CallError::Limit {
            what: "memory".into(),
        };
    }
    match err.downcast_ref::<Trap>() {
        Some(Trap::Interrupt) => CallError::Limit { what: "cpu".into() },
        Some(trap) => CallError::Crashed {
            message: trap.to_string(),
        },
        None => CallError::Crashed {
            message: describe(err),
        },
    }
}

/// What one call's store holds.
struct State {
    wasi: WasiCtx,
    table: ResourceTable,
    limiter: Limiter,
    /// `None` only for the trial instantiation at load, when no call runs.
    call: Option<CallHandle>,
}

impl WasiView for State {
    fn ctx(&mut self) -> WasiCtxView<'_> {
        WasiCtxView {
            ctx: &mut self.wasi,
            table: &mut self.table,
        }
    }
}

impl wit::Host for State {}

impl host::Host for State {
    async fn http_request(
        &mut self,
        req: host::Request,
    ) -> Result<wit::HttpResponse, wit::HttpError> {
        let Some(call) = &self.call else {
            return Err(wit::HttpError::Denied("no call is running".into()));
        };
        call.http(request_from_wit(req))
            .await
            .map(response_to_wit)
            .map_err(http_error_to_wit)
    }

    async fn log(&mut self, level: wit::LogLevel, message: String) {
        if let Some(call) = &self.call {
            call.log(log_level_from_wit(level), &message);
        }
    }
}

/// The linker: the `host` interface and the WASI subset a Rust `std` guest
/// imports. Nothing else, so filesystem and socket imports fail to link.
fn linker(engine: &Engine) -> wasmtime::Result<Linker<State>> {
    use wasmtime_wasi::cli::{WasiCli, WasiCliView};
    use wasmtime_wasi::clocks::{WasiClocks, WasiClocksView};
    use wasmtime_wasi::p2::bindings::{cli, clocks, random};
    use wasmtime_wasi::random::{WasiRandom, WasiRandomView};
    use wasmtime_wasi_io::bindings::wasi::io;

    let mut l = Linker::new(engine);
    io::error::add_to_linker::<State, HasIo>(&mut l, |s| &mut s.table)?;
    io::poll::add_to_linker::<State, HasIo>(&mut l, |s| &mut s.table)?;
    io::streams::add_to_linker::<State, HasIo>(&mut l, |s| &mut s.table)?;
    clocks::wall_clock::add_to_linker::<State, WasiClocks>(&mut l, State::clocks)?;
    clocks::monotonic_clock::add_to_linker::<State, WasiClocks>(&mut l, State::clocks)?;
    random::random::add_to_linker::<State, WasiRandom>(&mut l, State::random)?;
    cli::exit::add_to_linker::<State, WasiCli>(&mut l, State::cli)?;
    cli::environment::add_to_linker::<State, WasiCli>(&mut l, State::cli)?;
    cli::stdin::add_to_linker::<State, WasiCli>(&mut l, State::cli)?;
    cli::stdout::add_to_linker::<State, WasiCli>(&mut l, State::cli)?;
    cli::stderr::add_to_linker::<State, WasiCli>(&mut l, State::cli)?;
    cli::terminal_input::add_to_linker::<State, WasiCli>(&mut l, State::cli)?;
    cli::terminal_output::add_to_linker::<State, WasiCli>(&mut l, State::cli)?;
    cli::terminal_stdin::add_to_linker::<State, WasiCli>(&mut l, State::cli)?;
    cli::terminal_stdout::add_to_linker::<State, WasiCli>(&mut l, State::cli)?;
    cli::terminal_stderr::add_to_linker::<State, WasiCli>(&mut l, State::cli)?;
    bindings::Plugin::add_to_linker::<State, HasSelf<State>>(&mut l, |s| s)?;
    Ok(l)
}

/// `wasi:io` sees only the resource table (`wasmtime-wasi` keeps its own
/// equivalent private).
struct HasIo;

impl HasData for HasIo {
    type Data<'a> = &'a mut ResourceTable;
}

/// One memory budget for the whole store: every linear memory and table
/// counts against it, so declaring many memories does not multiply the limit.
/// Going over traps with [`MemoryLimit`].
struct Limiter {
    left: usize,
}

#[derive(Debug, thiserror::Error)]
#[error("the plugin exceeded its memory limit")]
struct MemoryLimit;

impl Limiter {
    fn take(&mut self, bytes: usize) -> wasmtime::Result<bool> {
        if bytes > self.left {
            return Err(wasmtime::Error::new(MemoryLimit));
        }
        self.left -= bytes;
        Ok(true)
    }
}

impl ResourceLimiter for Limiter {
    fn memory_growing(
        &mut self,
        current: usize,
        desired: usize,
        maximum: Option<usize>,
    ) -> wasmtime::Result<bool> {
        if maximum.is_some_and(|max| desired > max) {
            return Ok(false);
        }
        self.take(desired.saturating_sub(current))
    }

    fn table_growing(
        &mut self,
        current: usize,
        desired: usize,
        maximum: Option<usize>,
    ) -> wasmtime::Result<bool> {
        if maximum.is_some_and(|max| desired > max) {
            return Ok(false);
        }
        // Wasmtime stores a pointer per table element.
        let elements = desired.saturating_sub(current);
        self.take(elements.saturating_mul(std::mem::size_of::<usize>()))
    }

    fn instances(&self) -> usize {
        MAX_INSTANCES
    }

    fn tables(&self) -> usize {
        MAX_TABLES
    }

    fn memories(&self) -> usize {
        MAX_MEMORIES
    }
}

// ---------------------------------------------------------------- conversions

fn contract(message: &str) -> CallError {
    CallError::Contract {
        message: message.to_string(),
    }
}

fn page_from_wit(page: wit::SyncPage) -> Result<SyncPage, CallError> {
    if page.upserts.len() > MAX_UPSERTS {
        return Err(ContractError::TooManyUpserts(page.upserts.len()).into());
    }
    if page
        .upserts
        .iter()
        .any(|item| item.fields.len() > MAX_FIELDS_BYTES)
    {
        return Err(contract("an item's fields are larger than 64 KiB"));
    }
    Ok(SyncPage {
        upserts: page
            .upserts
            .into_iter()
            .map(item_from_wit)
            .collect::<Result<_, _>>()?,
        deletes: page.deletes,
        next_cursor: page.next_cursor,
        done: page.done,
        snapshot_complete: page.snapshot_complete,
    })
}

fn item_from_wit(item: wit::Item) -> Result<Item, CallError> {
    let fields = serde_json::from_str(&item.fields)
        .map_err(|e| contract(&format!("an item's fields are not JSON: {e}")))?;
    let updated_at = item
        .updated_at
        .map(|t| DateTime::parse_from_rfc3339(&t).map(|t| t.with_timezone(&Utc)))
        .transpose()
        .map_err(|e| {
            contract(&format!(
                "an item's updated-at is not an RFC 3339 time: {e}"
            ))
        })?;
    Ok(Item {
        id: item.id,
        title: item.title,
        url: item.url,
        updated_at,
        version: item.version,
        fields,
    })
}

fn plugin_error_from_wit(err: wit::PluginError) -> PluginError {
    match err {
        wit::PluginError::Auth(message) => PluginError::Auth { message },
        wit::PluginError::RateLimited((retry_after_secs, message)) => PluginError::RateLimited {
            retry_after_secs,
            message,
        },
        wit::PluginError::Settings(message) => PluginError::Settings { message },
        wit::PluginError::Http(message) => PluginError::Http { message },
        wit::PluginError::Other(message) => PluginError::Other { message },
    }
}

fn request_from_wit(req: wit::HttpRequest) -> HttpRequest {
    HttpRequest {
        method: match req.method {
            wit::HttpMethod::Get => HttpMethod::Get,
            wit::HttpMethod::Head => HttpMethod::Head,
            wit::HttpMethod::Post => HttpMethod::Post,
            wit::HttpMethod::Put => HttpMethod::Put,
            wit::HttpMethod::Patch => HttpMethod::Patch,
            wit::HttpMethod::Delete => HttpMethod::Delete,
        },
        url: req.url,
        headers: req.headers,
        body: req.body,
        auth: req.auth.map(|auth| match auth {
            wit::Auth::Bearer(secret) => Auth::Bearer { secret },
            wit::Auth::Basic((user_secret, password_secret)) => Auth::Basic {
                user_secret,
                password_secret,
            },
            wit::Auth::Header((name, secret)) => Auth::Header { name, secret },
        }),
    }
}

fn response_to_wit(resp: HttpResponse) -> wit::HttpResponse {
    wit::HttpResponse {
        status: resp.status,
        headers: resp.headers,
        body: resp.body,
    }
}

fn http_error_to_wit(err: HttpError) -> wit::HttpError {
    match err {
        HttpError::Denied { reason } => wit::HttpError::Denied(reason),
        HttpError::SecretMissing { key } => wit::HttpError::SecretMissing(key),
        HttpError::Timeout => wit::HttpError::Timeout,
        HttpError::TooLarge => wit::HttpError::TooLarge,
        HttpError::Transport { message } => wit::HttpError::Transport(message),
    }
}

fn log_level_from_wit(level: wit::LogLevel) -> LogLevel {
    match level {
        wit::LogLevel::Debug => LogLevel::Debug,
        wit::LogLevel::Info => LogLevel::Info,
        wit::LogLevel::Warn => LogLevel::Warn,
        wit::LogLevel::Error => LogLevel::Error,
    }
}

#[cfg(test)]
mod tests {
    use chrono::{SecondsFormat, TimeZone};

    use super::*;

    fn wit_item(fields: &str, updated_at: Option<&str>) -> wit::Item {
        wit::Item {
            id: "a".into(),
            title: "A".into(),
            url: None,
            updated_at: updated_at.map(str::to_string),
            version: None,
            fields: fields.into(),
        }
    }

    fn wit_page(upserts: Vec<wit::Item>) -> wit::SyncPage {
        wit::SyncPage {
            upserts,
            deletes: Vec::new(),
            next_cursor: None,
            done: true,
            snapshot_complete: true,
        }
    }

    fn is_contract(result: Result<SyncPage, CallError>) -> bool {
        matches!(result, Err(CallError::Contract { .. }))
    }

    #[test]
    fn times_in_the_guest_adapter_format_are_read_back() {
        // The guest adapter writes `to_rfc3339_opts(AutoSi, true)`.
        let at = Utc.with_ymd_and_hms(2026, 11, 26, 9, 30, 0).unwrap()
            + chrono::Duration::milliseconds(250);
        let text = at.to_rfc3339_opts(SecondsFormat::AutoSi, true);
        let page = page_from_wit(wit_page(vec![wit_item("{\"n\":1}", Some(&text))])).unwrap();
        assert_eq!(page.upserts[0].updated_at, Some(at));
        assert_eq!(page.upserts[0].fields["n"], 1);
    }

    #[test]
    fn fields_that_are_not_json_break_the_contract() {
        assert!(is_contract(page_from_wit(wit_page(vec![wit_item(
            "{", None
        )]))));
    }

    #[test]
    fn a_time_that_is_not_rfc_3339_breaks_the_contract() {
        let item = wit_item("{}", Some("yesterday"));
        assert!(is_contract(page_from_wit(wit_page(vec![item]))));
    }

    #[test]
    fn oversized_pages_are_refused_before_parsing() {
        let many = (0..=MAX_UPSERTS).map(|_| wit_item("{", None)).collect();
        assert_eq!(
            page_from_wit(wit_page(many)).unwrap_err(),
            CallError::from(ContractError::TooManyUpserts(MAX_UPSERTS + 1))
        );
        let big = format!("{{\"x\":\"{}\"}}", "y".repeat(MAX_FIELDS_BYTES));
        match page_from_wit(wit_page(vec![wit_item(&big, None)])) {
            Err(CallError::Contract { message }) => {
                assert!(message.contains("64 KiB"), "{message}")
            }
            other => panic!("expected a contract error, got {other:?}"),
        }
    }

    #[test]
    fn every_memory_and_table_draws_on_one_budget() {
        let mut limiter = Limiter { left: 1000 };
        assert!(limiter.memory_growing(0, 600, None).unwrap());
        // A second memory cannot take what the first one used.
        assert!(limiter.memory_growing(0, 600, None).is_err());
        assert!(limiter.memory_growing(0, 300, None).unwrap());
        // 100 bytes left: 12 table elements fit, 13 do not.
        assert!(limiter.table_growing(0, 12, None).unwrap());
        let err = limiter.table_growing(12, 13, None).unwrap_err();
        assert!(err.downcast_ref::<MemoryLimit>().is_some());
        // Past a memory's own maximum, growth fails without a trap.
        assert!(!limiter.memory_growing(0, 10, Some(5)).unwrap());
    }

    #[test]
    fn traps_map_to_call_errors() {
        let cpu = wasmtime::Error::new(Trap::Interrupt);
        assert_eq!(call_error(&cpu), CallError::Limit { what: "cpu".into() });
        let memory = wasmtime::Error::new(MemoryLimit).context("while growing");
        assert_eq!(
            call_error(&memory),
            CallError::Limit {
                what: "memory".into()
            }
        );
        let unreachable = wasmtime::Error::new(Trap::UnreachableCodeReached);
        assert!(matches!(
            call_error(&unreachable),
            CallError::Crashed { .. }
        ));
    }

    #[test]
    fn the_cpu_budget_is_counted_in_whole_ticks() {
        let ticks = |cpu| {
            WasmLimits {
                cpu,
                ..WasmLimits::default()
            }
            .cpu_ticks()
        };
        assert_eq!(ticks(Duration::from_secs(10)), 100);
        assert_eq!(ticks(Duration::from_millis(450)), 5);
        assert_eq!(ticks(Duration::ZERO), 1);
    }
}
