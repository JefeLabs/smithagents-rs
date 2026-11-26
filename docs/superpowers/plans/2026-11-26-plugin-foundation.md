# Plugin Foundation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** The daemon loads context-provider plugins, runs them under enforced limits, syncs their records into its store and serves them over its API, with GitHub (read-only: repositories, issues, pull requests) as the first built-in plugin.

**Architecture:** One plugin contract (`smithagents-plugin-protocol`: serde types, a synchronous `Plugin` trait, a `HostApi` the plugin calls back into, and the same contract as a WIT world). Three ways to host it behind one `PluginRuntime` trait in `smithagents-plugin-host`: linked (tests and development), `wasm` (a Wasmtime component, the sandbox for third-party plugins) and `dylib` (a stabby dynamic library in the daemon's process, for built-in plugins). Every plugin reaches the network only through the daemon's `http-request` host function, where the allow-list, the GET/HEAD-only rule for syncs and credential injection live. The server adds discovery, a sync scheduler, three SQLite tables, API routes and two events.

**Tech Stack:** Rust stable (1.100; edition 2021), tokio, axum, sqlx/SQLite, serde, `wasmtime` and `wasmtime-wasi` 51.0.0, `wit-bindgen` 0.63.0, `stabby` 72.1.16 with `libloading` 0.9, `reqwest` 0.13 with a rustls backend, `toml`, `semver`.

**Spec:** `docs/superpowers/specs/2026-11-23-smithagents-architecture-design.md` section 4.5 (Plugin system), section 2 (goals and anti-goals on plugins), section 5 (plugin boundary alternatives). Roadmap: `docs/superpowers/plans/2026-11-24-v1-roadmap.md`.

## Scope

In this plan: the contract, the manifest, the three host modes, host services (HTTP policy, secrets, logging), limits and supervision, discovery, the resource store, the sync scheduler, the daemon API and events, the client methods, a packing tool, a fixture plugin, and the GitHub plugin (read-only).

Not in this plan (each gets its own plan; nothing here may close them off): bottom-pane generic views and the plugin settings and sign-in UI; `views`, `action prompts` and `proposable writes` in the manifest; write proposals, approval and audit; the Keeper secrets provider; the streaming-tail resource kind; the `process` host mode; per-plugin release, signing and notarization; the marketplace. The desktop app shows nothing new after this plan: the result is checked through the API.

## Verified facts (2026-11-26, by compiling and running probes unless marked)

Wasmtime 51.0.0 (MSRV 1.97):
- `Config::async_support` is a deprecated no-op. Async is per call: `bindgen!({ path, world, imports: { default: async }, exports: { default: async } })`, `Linker`, `Plugin::add_to_linker::<State, wasmtime::component::HasSelf<State>>(&mut linker, |s| s)`, `Plugin::instantiate_async(&mut store, &component, &linker).await`, `plugin.call_<export>(&mut store, ..).await`. Host imports are `async fn name(&mut self, ..)` in the generated `<World>Imports` trait. Export wrappers take `&str`, not `String`.
- `wasmtime::Error` is Wasmtime's own type; it converts to `anyhow` only with the `anyhow` feature. Use `wasmtime::Result` and map to our errors.
- WASI view: `WasiView::ctx(&mut self) -> WasiCtxView<'_>` with `WasiCtxView { ctx: &mut WasiCtx, table: &mut ResourceTable }`.
- A plain Rust `wasm32-wasip2` guest built with std imports `wasi:io/{poll,error,streams}`, `wasi:clocks/{monotonic-clock,wall-clock}` and `wasi:cli/{stdin,stdout,stderr,environment,exit,terminal-*}`. `add_to_linker_proxy_interfaces_async` is not enough (instantiation fails on `get-environment`). A hand-picked subset works and leaves out filesystem and sockets, so a guest importing those fails at instantiation:
  ```rust
  struct HasIo; impl HasData for HasIo { type Data<'a> = &'a mut ResourceTable; }
  use wasmtime_wasi_io::bindings::wasi::io;            // crate wasmtime-wasi-io 51.0.0
  io::error::add_to_linker::<State, HasIo>(l, |s| &mut s.table)?;      // also io::poll, io::streams
  clocks::wall_clock::add_to_linker::<State, WasiClocks>(l, State::clocks)?;
  clocks::monotonic_clock::add_to_linker::<State, WasiClocks>(l, State::clocks)?;
  random::random::add_to_linker::<State, WasiRandom>(l, |s| s.random())?;
  // cli::{exit, environment, stdin, stdout, stderr, terminal_input, terminal_output,
  //       terminal_stdin, terminal_stdout, terminal_stderr}::add_to_linker::<State, WasiCli>(l, State::cli)?;
  // paths: wasmtime_wasi::{clocks::{WasiClocks, WasiClocksView}, random::{WasiRandom, WasiRandomView},
  //        cli::{WasiCli, WasiCliView}, p2::bindings::{cli, clocks, random}}
  ```
- Limits combine with async calls: `Config::epoch_interruption(true)`, `Engine::increment_epoch()` from a timer, `store.set_epoch_deadline(n)` (must be set: the default deadline traps at once), `store.epoch_deadline_trap()`; `StoreLimitsBuilder::new().memory_size(bytes).trap_on_grow_failure(true).build()` with `store.limiter(|s| &mut s.limits)`. The epoch trap downcasts to `wasmtime::Trap::Interrupt`; the memory trap is a plain error ("forcing trap when growing memory"), not a `Trap`. Epochs count guest execution only, not time awaiting a host import (reasoned, not measured): wrap the whole call in a tokio timeout.
- `Component::from_file` and `Engine::precompile_component` are safe; `Component::deserialize*` is `unsafe` (not used here). The on-disk cache is safe: `Config::cache(Some(wasmtime::Cache::new(CacheConfig::new()..)?))`, feature `cache`. Whether it hits was not measured.
- Guest: `cargo build --target wasm32-wasip2 --release` emits a component directly on Rust 1.100. `wit_bindgen::generate!({ path, world })`, `impl Guest for T`, `export!(T)`, `crate-type = ["cdylib"]`, wit-bindgen 0.63.0.
- Build cost: about one minute clean and about 16 MB with `wasmtime` `default-features = false` plus `async, component-model, cranelift, runtime, std`, and `wasmtime-wasi` `default-features = false` plus `p2`. That feature set ran async host imports and exports.
- NOT verified: whether `bindgen!` output compiles under `unsafe_code = "forbid"` (Task 4 checks first).

stabby 72.1.16 (the current line; versions are 2^api × 3^abi, not a rewrite), `libloading` 0.9.1, macOS arm64:
- Shared interface: `#[stabby::stabby] pub trait T { extern "C" fn call(&self, input: stabby::vec::Vec<u8>) -> stabby::vec::Vec<u8>; }` and `pub type BoxedT = stabby::dynptr!(stabby::boxed::Box<dyn T + Send + Sync>);` (without `+ Send + Sync` the pointer is not `Send`). Callers must `use <crate>::TDyn;` to call methods.
- Plugin: `#[stabby::export] pub extern "C" fn create() -> BoxedT { Box::new(X).into() }`. This compiles under `#![forbid(unsafe_code)]` in edition 2021.
- Host: `unsafe { libloading::Library::new(path)? }` then `unsafe { lib.get_stabbied::<extern "C" fn() -> BoxedT>(b"create") }` with `use stabby::libloading::StabbyLibrary;`. The host's `libloading` must be 0.9 or `get_stabbied` is "method not found". A mismatched signature or trait is refused at load with a "Report mismatch" error; the report includes crate paths and method order, so host and plugin must build against the same interface crate.
- A panic crossing the boundary aborts the process (exit 134). `std::panic::catch_unwind` inside the plugin's method contains it. The same holds for host callbacks called by the plugin.
- A plugin future that uses tokio panics when the host polls it (the library carries its own tokio). Calls are therefore synchronous bytes in, bytes out, run on `spawn_blocking`.
- NOT verified: behaviour across different rustc versions; unloading a library (we never unload).

## Decisions (my calls; the spec leaves them to the builder)

- **Contract version** is one integer, `CONTRACT_VERSION = 1`. A manifest names the version it was built for; the daemon accepts N and N-1 (only 1 exists now).
- **Calls are synchronous for the plugin.** `Plugin::sync` and `HostApi::http` block from the plugin's point of view in every mode. The host runs linked and dylib calls on `spawn_blocking`; Wasmtime suspends the guest while the async host import runs.
- **One call, one page.** The host calls `sync` repeatedly within a run while `done` is false, at most 20 pages per run; the cursor is opaque to the host and stored between runs.
- **A wasm call gets a fresh instance** (new `Store`, instantiate, call, drop). Plugins hold no state between calls; the compiled component is reused.
- **Limits:** call timeout 60 s; wasm guest CPU 10 s per call (epoch ticker every 100 ms, deadline 100 ticks); wasm memory 64 MiB; at most 50 HTTP requests per call; HTTP timeout 30 s; response body at most 5 MiB; request body at most 1 MiB; at most 500 upserts and 500 deletes per page; `fields` at most 64 KiB per item; title at most 512 characters; ids and cursor at most 4 KiB.
- **HTTP policy:** HTTPS only; the host must equal one of the manifest's `hosts` (exact, case-insensitive; no wildcards, no IP literals, no port other than 443); redirects are not followed (the 3xx is returned to the plugin); addresses that resolve to loopback, private, link-local or unspecified ranges are refused; during a sync only GET and HEAD are allowed; the plugin may not set `Authorization`, `Cookie`, `Host` or `Proxy-*` headers.
- **Secrets never enter a plugin.** A request carries `auth` naming a secret key; the daemon resolves it and adds the header. Sources in this plan: `<home>/secrets.toml` entries of the form `{ env = "NAME" }` or `{ command = ["prog", "arg"] }`, else the environment variable `SMITHAGENTS_SECRET_<PLUGIN>_<KEY>` (upper case, `-` to `_`). The file holds pointers, never secret values. Keeper is a later source.
- **Where plugins live:** built-in plugins in `<directory of the daemon executable>/plugins/<id>/`; installed plugins in `<home>/plugins/<id>/`. `host = "dylib"` is accepted only from the built-in directory. Built-in plugins start enabled; installed plugins start disabled and are enabled through the API (the permission prompt is a later plan).
- **Deletion:** a page lists `deletes`, or the last page of a run sets `snapshot_complete`, and the host removes that resource's items not seen during the run.
- **A plugin that times out in dylib or linked mode** cannot be stopped; it is marked Failed and not called again until the daemon restarts, so stuck threads do not pile up.
- **GitHub:** `api.github.com` only (no Enterprise host yet); the repositories to sync are a plugin setting; closed issues and merged pull requests are kept with their state.

## Global Constraints

- Naming is `smithagents`, never `singularity`.
- `unsafe` appears only in `crates/plugin-loader`. Every other crate keeps `unsafe_code = "forbid"`.
- Third-party plugin code never runs natively in the daemon's process: `dylib` loads only from the built-in directory.
- A plugin never receives a secret value. No secret, and no daemon token, appears in a log line, an error, an event, an API response or a `Debug` output.
- A plugin reaches the network only through `HostApi::http`. The wasm linker provides no filesystem, socket or `wasi:http` interface.
- During a sync a plugin can only read: GET and HEAD.
- Plugins exchange only the contract's serializable types with the daemon, in every mode.
- Exact pins: `wasmtime = "=51.0.0"`, `wasmtime-wasi = "=51.0.0"`, `wasmtime-wasi-io = "=51.0.0"`, `wit-bindgen = "=0.63.0"`, `stabby = "=72.1.16"`, `libloading = "0.9"`.
- Tests make no real network calls and spend no money. The one real-GitHub test is `#[ignore]` and read-only.
- Do not install system software. `rustup` installs the `wasm32-wasip2` target through `rust-toolchain.toml`; no global `rustup update`.
- Migrations `0001_init.sql` and `0002_intents.sql` are never edited.
- Gate for every task: `cargo test --workspace --features smithagents-server/test-util`, `cargo clippy --workspace --all-targets --features smithagents-server/test-util -- -D warnings`, `cargo fmt --all --check`, `cargo themegen check`, and `pgrep -fl smithagentsd` empty afterwards.

## Review Focus

1. **A hostile wasm plugin.** It loops forever, allocates without bound, imports the filesystem or sockets, requests an undeclared host, an IP literal, `http://`, or a declared host that resolves to a private address, or tries POST during a sync. Expected: the call ends with a clear error within its limits, the daemon and other plugins are unaffected, and nothing was sent. (Tests in Tasks 2 and 4.)
2. **Credential leakage.** Expected: the token is attached only to HTTPS requests to declared hosts, is not forwarded on a redirect, and appears in no log, error, event, API response or `Debug`. (Tests in Tasks 2, 9 and 11.)
3. **A built-in plugin panics or hangs.** Expected: the daemon survives, that plugin shows Failed with a message, other plugins keep syncing. (Tests in Tasks 5 and 8.)
4. **Bad data from a plugin.** Oversized pages, items or fields; duplicate ids in one page; `done = false` forever with the same cursor; a cursor that grows. Expected: the page is rejected whole with a contract error, nothing partial is stored, the run stops at the page budget. (Tests in Tasks 3 and 8.)
5. **Restart and upgrade.** The daemon stops mid-run; a plugin upgrade removes a resource or changes its settings schema; GitHub answers 401 or a rate limit. Expected: the stored cursor is the last fully stored page; items of a removed resource are deleted; invalid settings show NeedsSettings; 401 shows NeedsAuth; a rate limit delays the next run by the time GitHub gave. (Tests in Tasks 6, 8 and 10.)

## File Structure

```
crates/plugin-protocol/        contract: types, Manifest, Plugin, HostApi, envelopes, export macros
  wit/plugin.wit               the same contract for WebAssembly
crates/plugin-loader/          the only crate with unsafe: loads a stabby library
crates/plugin-host/            PluginRuntime; linked, wasm, dylib runtimes; HostServices; secrets
crates/core/src/plugin.rs      API types: PluginStatus, PluginState, ResourceStatus, ResourceItem
crates/server/src/plugins/     discovery.rs, manager.rs (state, scheduler), settings.rs
crates/server/migrations/0003_plugins.sql
plugins/fixture/               test plugin logic (rlib)
plugins/fixture-dylib/         cdylib wrapper
plugins/fixture-wasm/          cdylib wrapper, empty on native targets
plugins/fixture-wasm-fs/       a guest that imports the filesystem (must be refused)
plugins/github/                GitHub logic (rlib) and plugin.toml
plugins/github-dylib/          cdylib wrapper
tools/plugins/                 `cargo plugins pack` (lib + bin)
docs/plugins.md                how to write, pack and configure a plugin
```

---

### Task 1: The contract crate

**Files:**
- Create: `crates/plugin-protocol/{Cargo.toml,src/lib.rs,src/types.rs,src/manifest.rs,src/envelope.rs,wit/plugin.wit}`
- Modify: `Cargo.toml` (members `plugins/*`; workspace deps `toml`, `semver`, `base64`), `rust-toolchain.toml` (`targets = ["wasm32-wasip2"]`)
- Test: `crates/plugin-protocol/tests/{manifest.rs,types.rs}`

**Interfaces (Produces):**

```rust
pub const CONTRACT_VERSION: u32 = 1;
pub const MIN_CONTRACT_VERSION: u32 = 1;

pub struct PluginId(String);              // ^[a-z][a-z0-9-]{1,31}$ ; FromStr, Display, serde as string
pub enum HostMode { Dylib, Wasm, Process, InProcess }   // serde: "dylib" | "wasm" | "process" | "in-process"

pub struct Manifest {
    pub id: PluginId, pub name: String, pub version: semver::Version,
    pub contract: u32, pub host: HostMode,
    pub entry: Option<String>,            // file name inside the plugin directory; required for dylib and wasm
    pub hosts: Vec<String>, pub secrets: Vec<SecretDecl>,
    pub settings: Vec<SettingDecl>, pub resources: Vec<ResourceDecl>,
}
pub struct SecretDecl { pub key: String, pub label: String, pub help: Option<String> }
pub struct SettingDecl { pub key: String, pub label: String, pub kind: SettingKind, pub required: bool, pub default: Option<serde_json::Value> }
pub enum SettingKind { Text, List, Bool }
pub struct ResourceDecl { pub kind: String, pub label: String, pub interval_secs: u32 }   // 60..=86_400
impl Manifest {
    pub fn parse(toml_text: &str) -> Result<Manifest, ManifestError>;
    pub fn validate_settings(&self, value: &serde_json::Value) -> Result<serde_json::Value, SettingsError>; // fills defaults
}

pub struct SyncRequest { pub resource: String, pub cursor: Option<String>, pub settings: serde_json::Value }
pub struct SyncPage { pub upserts: Vec<Item>, pub deletes: Vec<String>, pub next_cursor: Option<String>, pub done: bool, pub snapshot_complete: bool }
pub struct Item { pub id: String, pub title: String, pub url: Option<String>, pub updated_at: Option<DateTime<Utc>>, pub version: Option<String>, pub fields: serde_json::Value }
impl SyncPage { pub fn validate(&self) -> Result<(), ContractError>; }   // the limits in Decisions; duplicate ids; fields must be an object

pub enum PluginError { Auth { message: String }, RateLimited { retry_after_secs: u32, message: String }, Settings { message: String }, Http { message: String }, Other { message: String } }

pub enum HttpMethod { Get, Head, Post, Put, Patch, Delete }
pub enum Auth { Bearer { secret: String }, Basic { user_secret: String, password_secret: String }, Header { name: String, secret: String } }
pub struct HttpRequest { pub method: HttpMethod, pub url: String, pub headers: Vec<(String, String)>, pub body: Option<Vec<u8>>, pub auth: Option<Auth> }
pub struct HttpResponse { pub status: u16, pub headers: Vec<(String, String)>, pub body: Vec<u8> }
impl HttpResponse { pub fn header(&self, name: &str) -> Option<&str>; }   // case-insensitive, first match
pub enum HttpError { Denied { reason: String }, SecretMissing { key: String }, Timeout, TooLarge, Transport { message: String } }
pub enum LogLevel { Debug, Info, Warn, Error }

pub trait HostApi { fn http(&self, req: HttpRequest) -> Result<HttpResponse, HttpError>; fn log(&self, level: LogLevel, message: &str); }
pub trait Plugin: Send + Sync { fn sync(&self, host: &dyn HostApi, req: SyncRequest) -> Result<SyncPage, PluginError>; }

// envelope.rs: what crosses the dylib boundary, as JSON; byte bodies are base64 strings.
pub enum PluginCall { Sync { call: u64, request: SyncRequest } }
pub enum PluginReply { Sync(Result<SyncPage, PluginError>), Panicked { message: String }, BadCall { message: String } }
pub enum HostCall { Http { call: u64, request: HttpRequest }, Log { call: u64, level: LogLevel, message: String } }
pub enum HostReply { Http(Result<HttpResponse, HttpError>), Done, BadCall { message: String } }
```

All types derive `Debug, Clone, PartialEq, Serialize, Deserialize`; errors also `thiserror::Error`. Enums use `#[serde(tag = "type", rename_all = "snake_case")]`.

`plugin.toml` shape (unknown keys are an error):

```toml
id = "github"
name = "GitHub"
version = "0.1.0"
contract = 1
host = "dylib"
entry = "libsmithagents_plugin_github.dylib"
hosts = ["api.github.com"]

[[secrets]]
key = "token"
label = "Personal access token"

[[settings]]
key = "repositories"
label = "Repositories (owner/name)"
kind = "list"
required = true

[[resources]]
kind = "issue"
label = "Issues"
interval_secs = 300
```

`wit/plugin.wit` (the exact contract for WebAssembly; `settings`, `fields` and `updated-at` travel as JSON or RFC 3339 strings):

```wit
package smithagents:plugin@1.0.0;

interface types {
  enum log-level { debug, info, warn, error }
  enum http-method { get, head, post, put, patch, delete }
  variant auth {
    bearer(string),
    basic(tuple<string, string>),
    header(tuple<string, string>),
  }
  record http-request { method: http-method, url: string, headers: list<tuple<string, string>>, body: option<list<u8>>, auth: option<auth> }
  record http-response { status: u16, headers: list<tuple<string, string>>, body: list<u8> }
  variant http-error { denied(string), secret-missing(string), timeout, too-large, transport(string) }
  record item { id: string, title: string, url: option<string>, updated-at: option<string>, version: option<string>, fields: string }
  record sync-request { resource: string, cursor: option<string>, settings: string }
  record sync-page { upserts: list<item>, deletes: list<string>, next-cursor: option<string>, done: bool, snapshot-complete: bool }
  variant plugin-error { auth(string), rate-limited(tuple<u32, string>), settings(string), http(string), other(string) }
}

interface host {
  use types.{http-request, http-response, http-error, log-level};
  http-request: func(req: http-request) -> result<http-response, http-error>;
  log: func(level: log-level, message: string);
}

world plugin {
  import host;
  use types.{sync-request, sync-page, plugin-error};
  export sync: func(req: sync-request) -> result<sync-page, plugin-error>;
}
```

- [ ] **Step 1: Write the failing tests.** `tests/manifest.rs`: the sample above parses to the expected `Manifest`; each of these is a distinct `ManifestError`: unknown key, bad id (`"GitHub"`, `"a"`, 33 characters), `contract = 0` and `contract = 2`, `host = "dylib"` or `"wasm"` without `entry`, `entry` containing `/` or `..`, a host with a scheme, a port, a wildcard, an IP literal or upper case that does not normalise, duplicate secret keys, duplicate setting keys, duplicate resource kinds, `interval_secs` 59 and 86_401, a required setting with a default of the wrong kind. `validate_settings`: a missing required key, a wrong kind, an unknown key, and defaults filled in. `tests/types.rs`: `SyncPage::validate` rejects 501 upserts, 501 deletes, a 64 KiB + 1 `fields`, `fields` that is not an object, a 513-character title, a duplicate id, an empty id, a 4 KiB + 1 cursor; every envelope round-trips through JSON with a 3-byte body encoded as base64; `HttpResponse::header("LINK")` finds `link`.
- [ ] **Step 2: Run them.** `cargo test -p smithagents-plugin-protocol`. Expected: compile errors (nothing exists).
- [ ] **Step 3: Implement** the crate as specified. No feature flags yet (Tasks 4 and 5 add `wasm-guest` and `dylib`).
- [ ] **Step 4: Run the tests.** Expected: PASS. Run the gate.
- [ ] **Step 5: Commit.** `feat(plugin-protocol): the plugin contract, manifest and WIT world`

---

### Task 2: Host services: HTTP policy, secrets, logging

**Files:**
- Create: `crates/plugin-host/{Cargo.toml,src/lib.rs,src/services.rs,src/http.rs,src/secrets.rs}`
- Test: `crates/plugin-host/tests/{http_policy.rs,secrets.rs}`

**Interfaces:**
- Consumes: Task 1 types.
- Produces:

```rust
pub struct Secret(String);                       // Debug prints "Secret(..)"; no Display, no Serialize
impl Secret { pub fn expose(&self) -> &str; }
#[async_trait] pub trait SecretSource: Send + Sync {
    async fn get(&self, plugin: &PluginId, key: &str) -> Result<Option<Secret>, SecretError>;
}
pub struct ConfiguredSecrets;                    // secrets.toml, then SMITHAGENTS_SECRET_<PLUGIN>_<KEY>
impl ConfiguredSecrets { pub fn load(home: &Path) -> Result<Self, SecretError>; }
pub struct StaticSecrets(pub HashMap<(String, String), String>);   // tests

pub enum CallPurpose { Sync }
pub struct HttpPolicy { pub allow_insecure_loopback: bool, pub timeout: Duration, pub max_response_bytes: usize, pub max_request_bytes: usize, pub max_requests_per_call: u32 }
impl Default for HttpPolicy { /* the Decisions values; allow_insecure_loopback = false */ }

pub struct HostServices;                         // one per plugin
impl HostServices {
    pub fn new(manifest: &Manifest, secrets: Arc<dyn SecretSource>, policy: HttpPolicy) -> Result<Self, ServicesError>;
    pub fn begin_call(self: &Arc<Self>, purpose: CallPurpose) -> CallHandle;
}
pub struct CallHandle;                           // counts requests for one call
impl CallHandle {
    pub async fn http(&self, req: HttpRequest) -> Result<HttpResponse, HttpError>;
    pub fn log(&self, level: LogLevel, message: &str);   // tracing event with plugin id; message truncated to 2 KiB
}
```

`reqwest`: this crate enables a rustls TLS backend (check the feature name with `cargo info reqwest`; the workspace entry has no TLS on purpose and stays that way), redirects off (`redirect::Policy::none()`), no proxy, no cookie store, a DNS resolver (`reqwest::dns::Resolve`) that drops loopback, private (10/8, 172.16/12, 192.168/16, fc00::/7), link-local, CGNAT (100.64/10) and unspecified addresses and fails if none remain. `allow_insecure_loopback` (tests only; settable only from code, never from a file or the environment) additionally permits `http://127.0.0.1:<any port>` URLs, whatever the manifest's `hosts` say, and skips the address filter for them. Every other rule still applies to those requests.

`secrets.toml`: `[<plugin id>]` tables of `key = { env = "NAME" }` or `key = { command = ["prog", "arg"] }`. Refuse the file if it is group- or world-writable. A command runs without a shell, with a 10 s timeout, stdin closed; the first line of stdout, trimmed, is the secret; a non-zero exit is `SecretError::Command` carrying the exit status only (never stdout or stderr). Resolved values are cached in memory for 5 minutes.

- [ ] **Step 1: Write the failing tests.** `tests/http_policy.rs` against a local `axum` server on 127.0.0.1 with `allow_insecure_loopback`: a GET succeeds and returns status, headers and body; `auth: Bearer{secret:"token"}` arrives as `Authorization: Bearer <value>`; `Basic` and `Header` likewise; a 302 comes back as a 302 and the redirect target receives no request; a body over the cap is `TooLarge`; a slow server is `Timeout`; the 51st request in one call is `Denied`; a new `begin_call` starts at zero. Without the loopback exemption, each of these is `Denied` and the server sees nothing (assert its request counter): `http://` URL, a host not in `hosts`, a host differing only by a trailing dot or by case (case must be allowed, the trailing dot refused), an IP literal, port 8443, a URL with userinfo, POST/PUT/PATCH/DELETE with `CallPurpose::Sync`, a request that sets `Authorization`, `cookie`, `HOST` or `Proxy-Authorization`, an `auth` naming an undeclared secret key (`Denied`), a declared but unresolvable key (`SecretMissing{key}`). The address filter as a unit test on the filter function: each private range, loopback v4 and v6, an IPv4-mapped IPv6 loopback, and a public address. `tests/secrets.rs`: env fallback name for plugin `my-plug`, key `api-key` is `SMITHAGENTS_SECRET_MY_PLUG_API_KEY`; `{ env }` and `{ command }` entries; a failing command's error text contains neither its stdout nor stderr; a world-writable file is refused; the cache serves a second read without running the command (count runs with a script that appends to a file); `format!("{:?}", secret)` and every `SecretError`, `HttpError` and `ServicesError` `Debug`/`Display` produced in these tests do not contain the secret value.
- [ ] **Step 2: Run.** `cargo test -p smithagents-plugin-host`. Expected: compile errors.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run.** Expected: PASS. Gate.
- [ ] **Step 5: Commit.** `feat(plugin-host): host services with the HTTP policy and secret injection`

---

### Task 3: The runtime trait, the linked runtime and the fixture plugin

**Files:**
- Create: `crates/plugin-host/src/{runtime.rs,linked.rs}`, `plugins/fixture/{Cargo.toml,src/lib.rs}`, `crates/plugin-host/tests/{common/mod.rs,common/conformance.rs,linked.rs}`

**Interfaces:**
- Produces:

```rust
pub enum CallError { Plugin(PluginError), Timeout, Crashed { message: String }, Limit { what: String }, Contract { message: String } }
#[async_trait] pub trait PluginRuntime: Send + Sync {
    fn mode(&self) -> HostMode;
    /// Runs one sync call within the call timeout; the page is already validated.
    async fn sync(&self, call: CallHandle, req: SyncRequest) -> Result<SyncPage, CallError>;
}
pub struct LinkedRuntime; impl LinkedRuntime { pub fn new(plugin: Arc<dyn Plugin>, call_timeout: Duration) -> Self; }
```

`LinkedRuntime::sync` runs the plugin on `spawn_blocking` with `catch_unwind`; its `HostApi` blocks on `CallHandle::http` through the runtime handle; a panic is `Crashed`; exceeding the timeout is `Timeout` (the thread is abandoned); an invalid page is `Contract`.

`plugins/fixture` exports `pub struct Fixture;` implementing `Plugin`, selected by `req.resource`:
- `items`: three pages of two items each (`item-1`..`item-6`, cursors `"1"`, `"2"`), last page `done` and `snapshot_complete`; a call with cursor `"3"` returns no upserts and `done`.
- `http`: GETs `settings.url` with `auth: Bearer{secret:"token"}` and returns one item whose fields hold the status and the body as text; an `HttpError` becomes `PluginError::Http` with the error's text.
- `post`: the same with POST.
- `auth`: returns `PluginError::Auth`. `rate`: `RateLimited{retry_after_secs: 120}`.
- `panic`: panics. `sleep`: sleeps `settings.ms` milliseconds then returns an empty done page.
- `huge`: 501 upserts. `dupe`: two items with the same id. `forever`: `done = false` with the same cursor every time.
- `spin`: loops forever. `bloat`: pushes 1 MiB vectors until allocation fails. (Used only by the wasm tests; never call them in linked or dylib mode: a stuck blocking thread hangs the test runtime on drop.)

`tests/common/conformance.rs`: `pub async fn run(runtime: Arc<dyn PluginRuntime>, services: Arc<HostServices>, server: &TestServer)` asserting, in this order: `items` pages and cursors exactly as above; `http` returns the server's body and the server saw the bearer header; `post` yields `PluginError::Http` containing "denied" and the server saw no POST; `auth` and `rate` map to `CallError::Plugin` with the same payload; `panic` is `Crashed` and a following `items` call still works; `sleep` with 3000 ms under a 200 ms timeout is `Timeout`; `huge` and `dupe` are `Contract`.

- [ ] **Step 1: Write the conformance suite and `tests/linked.rs`** (builds a `LinkedRuntime` over `Fixture` with a 200 ms timeout for the sleep case and calls `conformance::run`).
- [ ] **Step 2: Run.** `cargo test -p smithagents-plugin-host --test linked`. Expected: compile errors.
- [ ] **Step 3: Implement** the trait, `LinkedRuntime` and the fixture.
- [ ] **Step 4: Run.** Expected: PASS. Gate.
- [ ] **Step 5: Commit.** `feat(plugin-host): runtime trait, linked runtime and the fixture plugin`

---

### Task 4: The wasm runtime

**Files:**
- Create: `crates/plugin-host/src/wasm.rs`, `plugins/fixture-wasm/{Cargo.toml,src/lib.rs}`, `plugins/fixture-wasm-fs/{Cargo.toml,src/lib.rs}`, `tools/plugins/{Cargo.toml,src/lib.rs,src/main.rs}`, `crates/plugin-host/tests/wasm.rs`
- Modify: `crates/plugin-protocol` (feature `wasm-guest`: `wit-bindgen` and `export_wasm_plugin!`), `.cargo/config.toml` (alias `plugins = "run -p smithagents-plugins-tool --"`)

**Interfaces:**
- Consumes: Tasks 1 to 3.
- Produces:

```rust
pub struct WasmEngine;                       // one per daemon: Engine + epoch ticker task
impl WasmEngine { pub fn new(cache_dir: Option<&Path>) -> Result<Self, LoadError>; }
pub struct WasmLimits { pub memory_bytes: usize, pub cpu: Duration, pub call_timeout: Duration }   // Default = Decisions
pub struct WasmRuntime;
impl WasmRuntime { pub async fn load(engine: &WasmEngine, component: &Path, limits: WasmLimits) -> Result<Self, LoadError>; }
pub enum LoadError { Io(..), Invalid { message: String }, MissingImport { message: String }, Dylib { message: String }, Refused { message: String } }

// plugin-protocol, feature "wasm-guest":
// export_wasm_plugin!(Type) where Type: Plugin + Default. Generates the guest bindings from wit/plugin.wit
// and an adapter: JSON strings <-> serde_json::Value, RFC 3339 <-> DateTime, host imports -> HostApi.

// tools/plugins (lib):
pub struct PackOptions { pub release: bool, pub out_dir: PathBuf, pub fixtures: bool }
pub struct Packed { pub dir: PathBuf, pub ids: Vec<String> }
pub fn pack(opts: &PackOptions) -> Result<Packed, PackError>;   // builds with $CARGO in --target-dir target/plugin-pack
pub fn pack_fixtures_once() -> &'static Packed;                 // for tests; OnceLock
```

`WasmRuntime::load` compiles the component, then does a trial instantiation so a guest importing anything outside the linker (filesystem, sockets, `wasi:http`) fails here with `MissingImport`. The linker holds the WASI subset from Verified facts and the `host` interface; `WasiCtx` has no environment variables, no arguments, no preopens and closed stdio. `sync` builds a fresh `Store` with the limiter and an epoch deadline, instantiates, calls `sync` inside `tokio::time::timeout(call_timeout)`, and maps errors: `Trap::Interrupt` → `Limit{what:"cpu"}`, the memory-growth error → `Limit{what:"memory"}`, any other trap → `Crashed`, timeout → `Timeout`. JSON in `settings` and `fields` that fails to parse is `Contract`.

`plugins/fixture-wasm` and `plugins/fixture-wasm-fs` start with `#![cfg(target_arch = "wasm32")]` so they are empty on native targets and can be workspace members. `fixture-wasm` is `export_wasm_plugin!(smithagents_plugin_fixture::Fixture)`. `fixture-wasm-fs` calls `std::fs::read("x")` in `sync`.

`pack` writes `<out>/<id>/plugin.toml` plus the entry file. In this task it packs the two wasm fixtures (ids `fixture-wasm`, `fixture-wasm-fs`, `host = "wasm"`, `hosts = ["fixture.invalid"]`, secret `token`, setting `url` (text) and `ms` (text), one resource per fixture behaviour); Tasks 5 and 11 add the dylib fixture and GitHub.

- [ ] **Step 1: Check `bindgen!` under the lint.** In a scratch module, expand `wasmtime::component::bindgen!` for `wit/plugin.wit` and build with the workspace lints. If it fails only because of generated `unsafe`, set `unsafe_code = "deny"` for `smithagents-plugin-host` alone with `#[allow(unsafe_code)]` on the one module that holds the macro call, write no `unsafe` by hand, and record this in the task report.
- [ ] **Step 2: Write `tests/wasm.rs`.** Using `pack_fixtures_once()`: `conformance::run` passes for `fixture-wasm` (call timeout 200 ms for the sleep case); `spin` ends as `Limit{what:"cpu"}` in under 3 s with `WasmLimits{cpu: 500 ms, ..}`; `bloat` ends as `Limit{what:"memory"}` and a following `items` call works; `forever` returns pages normally (the page budget is the scheduler's job); loading `fixture-wasm-fs` is `LoadError::MissingImport` whose message names `wasi:filesystem`; loading a text file is `Invalid`; ten concurrent `items` calls on one runtime all succeed; with `cache_dir` set, a second `load` of the same file succeeds (no timing assertion).
- [ ] **Step 3: Run.** `cargo test -p smithagents-plugin-host --test wasm`. Expected: compile errors.
- [ ] **Step 4: Implement** the feature, the two guests, the pack tool and the runtime.
- [ ] **Step 5: Run.** Expected: PASS. Gate, plus `cargo clippy -p smithagents-plugin-fixture-wasm -p smithagents-plugin-fixture-wasm-fs --target wasm32-wasip2 -- -D warnings`.
- [ ] **Step 6: Commit.** `feat(plugin-host): Wasmtime runtime with limits and a minimal WASI linker`

---

### Task 5: The dylib runtime

**Files:**
- Create: `crates/plugin-loader/{Cargo.toml,src/lib.rs}`, `crates/plugin-host/src/dylib.rs`, `plugins/fixture-dylib/{Cargo.toml,src/lib.rs}`, `crates/plugin-host/tests/dylib.rs`, `crates/plugin-loader/tests/load.rs`
- Modify: `crates/plugin-protocol` (feature `dylib`: `stabby`, the two stabby traits, `export_dylib_plugin!`), `tools/plugins` (pack `fixture-dylib`)

**Interfaces:**
- Produces:

```rust
// plugin-protocol, feature "dylib":
#[stabby::stabby] pub trait DylibHost { extern "C" fn call(&self, input: stabby::vec::Vec<u8>) -> stabby::vec::Vec<u8>; }    // HostCall -> HostReply, JSON
#[stabby::stabby] pub trait DylibPlugin { extern "C" fn call(&self, input: stabby::vec::Vec<u8>) -> stabby::vec::Vec<u8>; }  // PluginCall -> PluginReply, JSON
pub type BoxedDylibHost = stabby::dynptr!(stabby::boxed::Box<dyn DylibHost + Send + Sync>);
pub type BoxedDylibPlugin = stabby::dynptr!(stabby::boxed::Box<dyn DylibPlugin + Send + Sync>);
pub type DylibEntry = extern "C" fn(BoxedDylibHost) -> BoxedDylibPlugin;
pub const DYLIB_ENTRY_SYMBOL: &[u8] = b"smithagents_plugin_create_v1";
// export_dylib_plugin!(expr): defines the #[stabby::export] entry; every call runs under catch_unwind and a
// panic becomes PluginReply::Panicked; undecodable input becomes PluginReply::BadCall. The plugin crate
// needs no unsafe.

// plugin-loader (its own [lints]: unsafe_code = "deny", with #[allow] on the two unsafe calls):
pub struct LoadedPlugin;                                   // owns the plugin object; the library is leaked on purpose
pub fn load(path: &Path, host: BoxedDylibHost) -> Result<LoadedPlugin, DylibLoadError>;
impl LoadedPlugin { pub fn call(&self, input: Vec<u8>) -> Vec<u8>; }

// plugin-host:
pub struct DylibRuntime; impl DylibRuntime { pub fn load(path: &Path, call_timeout: Duration) -> Result<Self, LoadError>; }
```

The host's `DylibHost::call` decodes a `HostCall`, finds the active call by its `call` number (a map of live `CallHandle`s; an unknown number is `HostReply::BadCall`), blocks on `CallHandle::http`, and runs under `catch_unwind` (a panic here would abort the daemon). `DylibRuntime::sync` registers the handle, runs `LoadedPlugin::call` on `spawn_blocking`, removes the handle, decodes the reply: `Panicked` → `Crashed`, timeout → `Timeout`. After a `Timeout` the runtime refuses further calls with `Crashed{message:"plugin stopped after a call timed out; restart the daemon"}`.

- [ ] **Step 1: Write the tests.** `plugin-loader/tests/load.rs`: loading the packed `fixture-dylib` and sending a `PluginCall::Sync` for `items` returns a `PluginReply::Sync(Ok(..))`; a missing file, a non-library file and a library without the entry symbol are each a `DylibLoadError` with a readable message; a library exporting the entry with a different signature (a second tiny test cdylib under `crates/plugin-loader/tests/wrong-sig/`, packed by the test through `pack`) is refused with a message containing "mismatch". `plugin-host/tests/dylib.rs`: `conformance::run` passes (the `panic` case proves the process survives); after the `sleep` timeout the next call is the "restart the daemon" error; a `HostCall` with an unknown call number gets `BadCall` (drive it through a fixture resource `stale-call` that reuses the previous call's number).
- [ ] **Step 2: Run.** Expected: compile errors.
- [ ] **Step 3: Implement.** Confirm with `grep -rn "unsafe" crates plugins tools --include=*.rs` that `unsafe` appears only in `crates/plugin-loader/src/lib.rs`.
- [ ] **Step 4: Run.** `cargo test -p smithagents-plugin-loader -p smithagents-plugin-host`. Expected: PASS. Gate.
- [ ] **Step 5: Commit.** `feat(plugin-host): stabby dylib runtime behind a single unsafe loader crate`

---

### Task 6: Discovery and loading

**Files:**
- Create: `crates/server/src/plugins/{mod.rs,discovery.rs}`
- Modify: `crates/server/src/{lib.rs,paths.rs}`, `crates/server/Cargo.toml`
- Test: `crates/server/tests/plugin_discovery.rs`

**Interfaces:**
- Produces:

```rust
impl Paths { pub fn plugins_dir(&self) -> PathBuf; /* <home>/plugins */ pub fn wasm_cache_dir(&self) -> PathBuf; /* <home>/cache/wasm */ }
pub fn builtin_plugins_dir() -> Option<PathBuf>;          // <dir of current_exe>/plugins

pub enum Origin { Builtin, Installed }
pub struct Discovered { pub manifest: Manifest, pub dir: PathBuf, pub origin: Origin }
pub struct Rejected { pub dir: PathBuf, pub reason: String }
pub fn discover(builtin: Option<&Path>, installed: &Path) -> (Vec<Discovered>, Vec<Rejected>);
pub async fn load_runtime(d: &Discovered, engine: &WasmEngine) -> Result<Arc<dyn PluginRuntime>, LoadError>;
```

Rules: one sub-directory per plugin; the directory name must equal the manifest id; `contract` must be within `MIN_CONTRACT_VERSION..=CONTRACT_VERSION`; `host = "dylib"` from `Installed` is rejected ("native plugins can only be built in"); `process` and `in-process` are rejected ("not supported yet"); a built-in id wins over an installed plugin with the same id (the installed one is rejected); the entry file must exist, be a regular file inside the plugin directory after resolving symlinks, and not be group- or world-writable; an unreadable or invalid manifest rejects that directory only. Results are sorted by id.

- [ ] **Step 1: Write the tests** with temp directories and manifests (the fixtures packed by `pack_fixtures_once()` for the load cases): each rule above has one case asserting the `Rejected.reason` text; a valid built-in dylib and a valid installed wasm are both discovered and `load_runtime` returns runtimes whose `mode()` matches; an empty or missing directory yields nothing and no error.
- [ ] **Step 2: Run.** Expected: compile errors.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run.** Expected: PASS. Gate.
- [ ] **Step 5: Commit.** `feat(server): discover and load plugins from the built-in and installed directories`

---

### Task 7: Store and API types

**Files:**
- Create: `crates/server/migrations/0003_plugins.sql`, `crates/core/src/plugin.rs`
- Modify: `crates/server/src/store.rs`, `crates/core/src/{lib.rs,event.rs}`
- Test: `crates/server/tests/plugin_store.rs`, unit tests in `crates/core/src/event.rs`

**Interfaces:**
- Produces:

```sql
CREATE TABLE plugin_config (plugin_id TEXT PRIMARY KEY NOT NULL, enabled INTEGER NOT NULL, settings_json TEXT NOT NULL);
CREATE TABLE resource_sync (plugin_id TEXT NOT NULL, resource TEXT NOT NULL, cursor TEXT, last_run_at TEXT, last_ok_at TEXT,
    last_error TEXT, next_due_at TEXT, failures INTEGER NOT NULL DEFAULT 0, PRIMARY KEY (plugin_id, resource));
CREATE TABLE resource_items (plugin_id TEXT NOT NULL, resource TEXT NOT NULL, item_id TEXT NOT NULL, title TEXT NOT NULL, url TEXT,
    source_updated_at TEXT, version TEXT, fields_json TEXT NOT NULL, synced_at TEXT NOT NULL, run_id TEXT NOT NULL,
    PRIMARY KEY (plugin_id, resource, item_id));
CREATE INDEX resource_items_list ON resource_items (plugin_id, resource, source_updated_at DESC, item_id);
```

```rust
// smithagents-core::plugin (serde; no dependency on plugin-protocol)
pub enum PluginState { Disabled, Ready, NeedsAuth { missing: Vec<String>, message: String }, NeedsSettings { message: String }, Failed { message: String }, LoadFailed { message: String } }
pub struct ResourceStatus { pub kind: String, pub label: String, pub item_count: u64, pub last_run_at: Option<DateTime<Utc>>, pub last_ok_at: Option<DateTime<Utc>>, pub next_due_at: Option<DateTime<Utc>>, pub last_error: Option<String>, pub syncing: bool }
pub struct PluginStatus { pub id: String, pub name: String, pub version: String, pub host: String, pub builtin: bool, pub enabled: bool, pub state: PluginState, pub hosts: Vec<String>, pub secrets: Vec<String>, pub settings: Vec<SettingField>, pub resources: Vec<ResourceStatus> }
pub struct SettingField { pub key: String, pub label: String, pub kind: String, pub required: bool }
pub struct ResourceItem { pub id: String, pub title: String, pub url: Option<String>, pub updated_at: Option<DateTime<Utc>>, pub version: Option<String>, pub fields: serde_json::Value, pub synced_at: DateTime<Utc> }
pub struct ItemsPage { pub items: Vec<ResourceItem>, pub next: Option<String> }

// EventKind gains (TYPES becomes 9 entries):
PluginUpdated(PluginStatus),                                              // "plugin_updated"
ResourceSynced { plugin_id: String, resource: String, upserted: u32, deleted: u32, at: DateTime<Utc> },   // "resource_synced"

// Store
pub struct PluginConfig { pub enabled: bool, pub settings: serde_json::Value }
pub struct SyncState { pub cursor: Option<String>, pub last_run_at: .., pub last_ok_at: .., pub last_error: Option<String>, pub next_due_at: .., pub failures: u32 }
pub async fn plugin_config(&self, id: &str) -> Result<Option<PluginConfig>, StoreError>;
pub async fn set_plugin_config(&self, id: &str, cfg: &PluginConfig) -> Result<(), StoreError>;
pub async fn sync_state(&self, id: &str, resource: &str) -> Result<SyncState, StoreError>;          // default when absent
pub async fn save_sync_state(&self, id: &str, resource: &str, s: &SyncState) -> Result<(), StoreError>;
/// One transaction: upserts, deletes and the new cursor. Returns (upserted, deleted) counting only rows that changed.
pub async fn apply_page(&self, id: &str, resource: &str, run_id: &str, page: &SyncPage, at: DateTime<Utc>) -> Result<(u32, u32), StoreError>;
pub async fn delete_unseen(&self, id: &str, resource: &str, run_id: &str) -> Result<u32, StoreError>;
pub async fn delete_resource(&self, id: &str, resource: &str) -> Result<u32, StoreError>;           // items and sync state
pub async fn list_items(&self, id: &str, resource: &str, limit: u32, after: Option<&str>, q: Option<&str>) -> Result<ItemsPage, StoreError>;
pub async fn count_items(&self, id: &str, resource: &str) -> Result<u64, StoreError>;
pub async fn known_resources(&self, id: &str) -> Result<Vec<String>, StoreError>;
```

`apply_page` counts an upsert as changed when the row is new or its `version`, `title`, `url`, `source_updated_at` or `fields_json` differ; an unchanged row still gets the new `run_id`. `list_items` orders by `source_updated_at DESC NULLS LAST, item_id`; `next` is an opaque keyset token for that order; `q` matches the title case-insensitively with `%` and `_` escaped; `limit` is clamped to 1..=200.

- [ ] **Step 1: Write the tests.** Apply three pages then read back in order across two `list_items` pages with no gaps or repeats; re-applying an identical page reports `(0, 0)`; a changed title reports 1; `deletes` remove rows; `delete_unseen` removes exactly the rows not touched by the run; a failing statement inside `apply_page` leaves items and cursor untouched (force it with a page whose cursor and items are valid but run against a dropped index or a second connection holding a lock; choose whichever is deterministic); `q = "100%"` matches only a literal `100%`; items of two plugins with the same resource kind do not mix; `delete_resource` clears items and state; migration from a database at 0002 keeps existing rows. In `event.rs`: `TYPES` matches `type_name` for all nine kinds and an unknown `type` still fails to deserialize into `EventKind` (the client's skip path is unchanged).
- [ ] **Step 2: Run.** Expected: compile errors.
- [ ] **Step 3: Implement.** Update every exhaustive `match` on `EventKind` in `crates/app`, `crates/client` and `crates/server` so the new kinds are ignored where they are not used.
- [ ] **Step 4: Run.** Expected: PASS. Gate.
- [ ] **Step 5: Commit.** `feat(server): plugin configuration, sync state and resource items in the store`

---

### Task 8: The plugin manager and sync scheduler

**Files:**
- Create: `crates/server/src/plugins/{manager.rs,settings.rs}`
- Test: `crates/server/tests/plugin_manager.rs`

**Interfaces:**
- Consumes: Tasks 2, 3, 6, 7.
- Produces:

```rust
pub struct ManagerConfig { pub max_concurrent: usize /* 4 */, pub pages_per_run: u32 /* 20 */, pub backoff_min: Duration /* 30 s */, pub backoff_max: Duration /* 30 min */, pub http: HttpPolicy }
pub struct LoadedPlugin { pub manifest: Manifest, pub origin: Origin, pub runtime: Result<Arc<dyn PluginRuntime>, String> }
pub struct PluginManager;   // Clone (Arc inside)
impl PluginManager {
    pub async fn start(store: Store, bus: EventBus, secrets: Arc<dyn SecretSource>, plugins: Vec<LoadedPlugin>, cfg: ManagerConfig) -> Result<Self, ManagerError>;
    pub async fn list(&self) -> Vec<PluginStatus>;
    pub async fn get(&self, id: &str) -> Option<PluginStatus>;
    pub async fn set_enabled(&self, id: &str, enabled: bool) -> Result<PluginStatus, ManagerError>;
    pub async fn settings(&self, id: &str) -> Result<serde_json::Value, ManagerError>;
    pub async fn set_settings(&self, id: &str, value: serde_json::Value) -> Result<PluginStatus, ManagerError>;
    pub async fn sync_now(&self, id: &str, resource: &str) -> Result<(), ManagerError>;      // queues a run; Busy if one is in flight
    pub async fn items(&self, id: &str, resource: &str, limit: u32, after: Option<&str>, q: Option<&str>) -> Result<ItemsPage, ManagerError>;
    pub async fn shutdown(&self);                                                            // stops scheduling, waits for runs up to 5 s
    #[cfg(feature = "test-util")] pub async fn run_due(&self);                               // one scheduling pass, awaited to completion
}
pub enum ManagerError { UnknownPlugin, UnknownResource, Disabled, Busy, InvalidSettings(String), Store(StoreError) }
```

Behaviour:
- **Start:** for each plugin, create its `plugin_config` if absent (`enabled` = origin is Builtin; settings = manifest defaults); delete stored resources the manifest no longer declares; a failed load is `LoadFailed`; settings that no longer validate are `NeedsSettings` (the stored value is kept).
- **A run** of one (plugin, resource): a new `run_id`; loop `sync` with the stored cursor; each valid page goes through `apply_page` (items and cursor together); stop when `done`, after `pages_per_run` pages, or on an error. On `done` with `snapshot_complete`, `delete_unseen`. Then `next_due_at = now + interval_secs`, `failures = 0`, publish `ResourceSynced` if anything changed, and `PluginUpdated`. A cursor that did not change between two pages with `done = false` ends the run with the error "the plugin did not advance its cursor".
- **Before a run** the manager resolves every secret the manifest declares; if any is missing the state is NeedsAuth naming those keys and the plugin is not called. (A plugin only sees a missing secret as an HTTP error, so the host has to detect it.)
- **Errors:** `Plugin(Auth)` → state NeedsAuth, no retry until settings or enablement change or `sync_now`; `Plugin(Settings)` → NeedsSettings; `Plugin(RateLimited{retry_after_secs})` → `next_due_at = now + max(retry_after_secs, interval_secs)`, state stays Ready; `Timeout`, `Crashed`, `Limit`, `Contract`, `Plugin(Http|Other)` → `failures += 1`, `next_due_at = now + min(backoff_min * 2^(failures-1), backoff_max)`, `last_error` set; after a `Timeout` or `Crashed` in dylib mode the state is Failed and scheduling for that plugin stops. Pages stored before an error are kept.
- **Scheduling:** one task wakes at the earliest `next_due_at`; at most one run per plugin and `max_concurrent` overall; a disabled plugin is never run; `set_settings` and enabling reset `failures`, clear NeedsAuth/NeedsSettings and make every resource due now. A panic in a run task is caught and recorded as `last_error`; the scheduler keeps going.

- [ ] **Step 1: Write the tests** with `LinkedRuntime` over `Fixture` (plus a scripted test plugin where a sequence of replies is needed), `StaticSecrets`, an in-memory store, `tokio::time::pause`, and `run_due()`: a first run stores six items and emits one `ResourceSynced{upserted: 6}` and `PluginUpdated`; the next due run emits no `ResourceSynced`; `forever` (same cursor every time) stops at its second page with the "did not advance its cursor" error; a scripted plugin that advances its cursor forever stops after 20 pages with those pages stored, its cursor kept, and the next run due one interval later; `huge` stores nothing and records a contract error; an error on page 3 keeps pages 1 and 2 and their cursor; backoff doubles from 30 s and caps at 30 min; `rate` delays by 120 s when the interval is 60 and by the interval when it is 300; `auth` → NeedsAuth and no further runs until `set_settings`; a missing declared secret → NeedsAuth naming the key without calling the plugin; removing a resource from the manifest between two `start`s deletes its items; settings invalid after an "upgrade" → NeedsSettings and no runs; `set_settings` with a bad value is `InvalidSettings` and changes nothing; `sync_now` while running is `Busy`; a disabled plugin never runs and `sync_now` is `Disabled`; two plugins where one always panics: the other keeps its schedule; five plugins due at once run at most four at a time (count with a gate in the scripted plugin); `shutdown` returns while a run sleeps; restarting the manager on the same store resumes from the stored cursor (page 2 of 3 after an interrupted run).
- [ ] **Step 2: Run.** Expected: compile errors.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run** the test file three times. Expected: PASS each time. Gate.
- [ ] **Step 5: Commit.** `feat(server): plugin manager with a supervised sync scheduler`

---

### Task 9: Daemon API, events, startup and the client

**Files:**
- Modify: `crates/server/src/{api.rs,bin/smithagentsd.rs}`, `crates/client/src/api.rs`
- Test: `crates/server/tests/plugin_api.rs`, `crates/client/tests/plugins.rs`

**Interfaces:**
- Produces (all behind the existing bearer token; errors use the existing `ApiError` shape):

| Route | Result |
|---|---|
| `GET /v1/plugins` | `Vec<PluginStatus>` |
| `GET /v1/plugins/{id}` | `PluginStatus`; 404 `plugin_not_found` |
| `POST /v1/plugins/{id}/enable`, `/disable` | `PluginStatus` |
| `GET /v1/plugins/{id}/settings` | the settings object |
| `PUT /v1/plugins/{id}/settings` | `PluginStatus`; 400 `invalid_settings` with the message |
| `POST /v1/plugins/{id}/resources/{kind}/sync` | 202; 404 `resource_not_found`; 409 `sync_in_progress`; 409 `plugin_disabled` |
| `GET /v1/plugins/{id}/resources/{kind}/items?limit=&after=&q=` | `ItemsPage`; 400 `bad_cursor` |

Client: `plugins()`, `plugin(id)`, `set_plugin_enabled(id, bool)`, `plugin_settings(id)`, `set_plugin_settings(id, value)`, `sync_plugin_resource(id, kind)`, `plugin_items(id, kind, limit, after, q)`.

Startup in `smithagentsd`: after the store opens, `discover(builtin_plugins_dir(), paths.plugins_dir())`, log each rejection at warn (directory and reason only), build one `WasmEngine` with `paths.wasm_cache_dir()`, `load_runtime` each, `ConfiguredSecrets::load(home)` (a refused or invalid `secrets.toml` is logged at error and treated as empty; the daemon still starts), `PluginManager::start`. `AppState` gains `plugins: PluginManager`. The idle-exit rule is unchanged: syncing does not keep an idle daemon alive. On shutdown, `plugins.shutdown()` runs before the lockfile is removed.

- [ ] **Step 1: Write the tests.** `plugin_api.rs` (router-level, with a manager over the linked fixture): each route's success shape; each listed error code; every route without the token is 401; `PUT settings` then `GET settings` round-trips; `sync` then the event stream delivers `resource_synced` and `plugin_updated`; items paging with `after`; a garbage `after` is 400; with `StaticSecrets` holding token `s3cr3t-value`, no response body from any route and no event contains that string. `client/tests/plugins.rs` against a real daemon started with a temp home whose `plugins/` holds the packed `fixture-wasm`, using its `items` resource: the client lists the plugin as disabled, enables it, triggers a sync, and reads six items; the daemon's stderr contains no token.
- [ ] **Step 2: Run.** Expected: compile errors.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run.** Expected: PASS. Gate.
- [ ] **Step 5: Commit.** `feat(server): plugin routes, events and startup wiring`

---

### Task 10: The GitHub plugin

**Files:**
- Create: `plugins/github/{Cargo.toml,plugin.toml,src/lib.rs,src/{cursor.rs,repos.rs,issues.rs,pulls.rs,http.rs}}`, `plugins/github/tests/{common/mod.rs,repos.rs,issues.rs,pulls.rs,errors.rs}`, `plugins/github/tests/fixtures/*.json`

**Interfaces:**
- Consumes: Task 1 (`Plugin`, `HostApi`).
- Produces: `pub struct GitHub;` implementing `Plugin` and `Default`; `plugin.toml` with id `github`, `host = "dylib"`, `entry = "libsmithagents_plugin_github.dylib"`, `hosts = ["api.github.com"]`, secret `token`, setting `repositories` (list, required, entries `owner/name`), resources `repository` (3600 s), `issue` (300 s), `pull_request` (300 s).

Requests: every request is a GET with `auth: Bearer{secret:"token"}` and headers `Accept: application/vnd.github+json`, `X-GitHub-Api-Version: 2022-11-28`, `User-Agent: smithagents`.
- `repository`: `GET https://api.github.com/repos/{owner}/{name}` per configured repository, one per call; the last sets `done` and `snapshot_complete`. Item id `owner/name`; fields `description, private, default_branch, open_issues_count, stargazers_count, pushed_at, archived`.
- `issue`: `GET /repos/{o}/{n}/issues?state=all&sort=updated&direction=asc&per_page=100` plus `&since=<ISO 8601>` when the cursor has one for that repository. Entries with a `pull_request` key are skipped. Follow the `Link` header's `rel="next"` URL (only if its host is `api.github.com`) for later pages. Item id `owner/name#number`; `version` = `updated_at`; fields `number, state, state_reason, author, labels, assignees, comments, created_at, closed_at, repository`. When a repository is exhausted, its `since` becomes the newest `updated_at` seen; the run moves to the next repository and is `done` after the last. `snapshot_complete` is false.
- `pull_request`: `GET /repos/{o}/{n}/pulls?state=all&sort=updated&direction=desc&per_page=100`; stop a repository when an entry's `updated_at` is not newer than the stored mark or there is no next page. Item id `owner/name#number`; fields `number, state, draft, merged_at, author, labels, assignees, requested_reviewers, head_ref, base_ref, created_at, closed_at, repository`.
- Cursor: a JSON string `{ "v": 1, "repo": <index>, "next": <url or null>, "marks": { "<owner/name>": "<timestamp>" }, "pending": "<timestamp or null>" }`; a cursor that fails to parse or has another `v` restarts from the beginning. Repositories are processed in sorted order; a repository removed from settings drops its mark.
- Errors: 401 → `Auth` ("GitHub rejected the token"); 403 or 429 with `x-ratelimit-remaining: 0` or a `retry-after` header → `RateLimited` with `retry-after`, else `x-ratelimit-reset` minus the response's `date` header, clamped to 60..=3600; 404 → `Settings` naming the repository ("not found or not visible to this token"); 301 → `Settings` ("repository moved"); other non-2xx → `Http` with the status; invalid JSON → `Other`. Settings entries not matching `^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$` → `Settings`. An empty list → an empty `done` page.

- [ ] **Step 1: Write the tests** with a scripted `HostApi` (`tests/common`) that maps exact URLs to canned responses, records every request, and fails the test on an unexpected URL: repositories across two repos with `snapshot_complete` on the last; issues over two pages via `Link`, PR entries skipped, `since` sent on the second run and equal to the newest `updated_at`; a `Link` next URL on another host is not followed; pull requests stop at the mark; closed and merged states carried in fields; every request carries the three headers, the bearer `auth` and method GET, and no request sets `Authorization` itself; each error mapping above, including both rate-limit forms and the clamp; malformed settings; an unparsable cursor restarts; a repository removed from settings loses its mark; every returned page passes `SyncPage::validate` (a 100-entry fixture with long bodies stays under the limits because bodies are not stored).
- [ ] **Step 2: Run.** `cargo test -p smithagents-plugin-github`. Expected: compile errors.
- [ ] **Step 3: Implement.**
- [ ] **Step 4: Run.** Expected: PASS. Gate.
- [ ] **Step 5: Commit.** `feat(plugins): GitHub plugin for repositories, issues and pull requests (read-only)`

---

### Task 11: Packing GitHub and the end-to-end check

**Files:**
- Create: `plugins/github-dylib/{Cargo.toml,src/lib.rs}`, `crates/server/tests/plugin_e2e.rs`, `docs/plugins.md`
- Modify: `tools/plugins/src/{lib.rs,main.rs}`, `README.md`

**Interfaces:**
- Produces: `cargo plugins pack [--release] [--out <dir>] [--fixtures]` (default out: `target/<profile>/plugins`, the directory the daemon binary reads); `plugins/github-dylib` is `export_dylib_plugin!(smithagents_plugin_github::GitHub)` with `[lib] name = "smithagents_plugin_github"`.

- [ ] **Step 1: Write `plugin_e2e.rs`.** (a) Through the real daemon binary: pack into a temp built-in directory arrangement (copy the daemon binary and the packed `fixture-dylib` beside it), start it with a temp home, and with the client: the fixture is listed as built-in, enabled and `host = "dylib"`; a sync stores six items; the `panic` resource leaves the daemon answering `/v1/health` and the plugin showing a `last_error`. (b) In-process, GitHub against a fake server on loopback that serves the Task 10 fixtures. The plugin's URLs name `api.github.com`, so the test wraps `GitHub` in a `Plugin` adapter whose `HostApi` rewrites `https://api.github.com` to the fake's `http://127.0.0.1:<port>` before passing the request on. Run the manager over a `LinkedRuntime` of that adapter with `allow_insecure_loopback` and `StaticSecrets`, and assert: issues and pull requests are stored and listed through the API, the fake saw `Authorization: Bearer <value>` on every request, a 401 from the fake shows NeedsAuth, a rate-limit response sets `next_due_at` from the header, and the token value appears in no API response, event or captured log. (c) `#[ignore]` `real_github_smoke`: with `GITHUB_TOKEN` and `SMITHAGENTS_TEST_REPO` set, syncs that one repository's metadata through the packed GitHub dylib and prints the item count; read-only.
- [ ] **Step 2: Run.** Expected: failures (no GitHub dylib, no pack entry).
- [ ] **Step 3: Implement** the wrapper crate and the pack entries; write `docs/plugins.md`: the contract and its limits, the manifest reference, the HTTP policy, how secrets are referenced and configured (`secrets.toml` with the `gh auth token` example, file mode 0600), the three host modes and who may use each, `cargo plugins pack`, where plugins live, how to check a plugin with `curl` against the daemon (reading the port and token from the lockfile), and what is not built yet. Add a short "Plugins" section to `README.md` pointing at it.
- [ ] **Step 4: Run.** `cargo plugins pack && cargo test --workspace --features smithagents-server/test-util`. Expected: PASS. Then the manual launch check: `cargo build -p smithagents-server --bin smithagentsd && cargo plugins pack`, start `smithagentsd` with a temp `SMITHAGENTS_HOME`, `curl` `/v1/plugins` with the lockfile's token and confirm `github` is listed as built-in with state NeedsAuth (no token configured) and that stderr holds no panic; stop the daemon; `pgrep -fl smithagentsd` empty. Record the output in the task report. Gate.
- [ ] **Step 5: Commit.** `feat(plugins): pack the GitHub plugin as a built-in dylib; plugin author guide`

---

## Follow-ups (recorded, not built here)

- Bottom-pane generic views and the plugin settings and sign-in UI; `views` in the manifest.
- Write proposals, approval, conflict detection and audit; `proposable writes`; allowing POST and friends under `CallPurpose::Write`.
- Action prompts; the Keeper secrets source; the streaming-tail resource kind behind one adapter (host-side stream consumption works in Wasmtime 51 but its API is still moving).
- Per-plugin release tags and CI; signing built-in dylibs with our Team ID and notarizing; a publisher signature for wasm plugins; the marketplace and its permission prompt.
- The `process` host mode. GitHub Enterprise hosts. Webhook freshness.
- Measure: Wasmtime's on-disk cache hit rate, per-call instantiation cost, and stabby's vtable lookup cost on stable Rust.
- Decide whether syncing should hold an otherwise idle daemon open.
