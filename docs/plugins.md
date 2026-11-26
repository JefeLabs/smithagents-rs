# Plugins

A plugin lets SmithAgents show records from another service, such as the
repositories, issues and pull requests of GitHub. The daemon (`smithagentsd`)
runs plugins on a schedule, stores what they return, and serves it over its
local API. This guide describes what is built today, how to write a plugin
against it, and what is not built yet.

Contents: [how it works](#how-it-works), [the contract](#the-contract),
[the manifest](#the-manifest), [HTTP policy](#http-policy),
[secrets](#secrets), [command-line tools](#command-line-tools),
[host modes and discovery](#host-modes-and-discovery),
[the API](#the-api), [the GitHub plugin](#the-github-plugin),
[packing](#packing-plugins), [checking with curl](#checking-with-curl),
[limits](#limits), [not built yet](#not-built-yet).

## How it works

Each plugin declares **resources**: kinds of records it can list (for GitHub:
`repository`, `issue`, `pull_request`). For each resource the daemon calls the
plugin's `sync` function with a cursor and the user's settings. The plugin
answers with one page: items to store (`upserts`), ids to remove (`deletes`),
the next cursor, and whether the listing is done. The daemon keeps calling
until the listing is done (at most 20 pages per run), then waits for the
resource's `interval_secs` before the next run.

A plugin never touches the network, the disk or a secret directly. Its only
line to the world is the host's `http` function. The host checks every request
against the manifest and the HTTP policy, attaches the secret itself, and
returns the response. The plugin only names a secret (`Auth::Bearer { secret:
"token" }`); it never sees the value.

## The contract

Rust plugins implement one trait from `smithagents-plugin-protocol`:

```rust
pub trait Plugin: Send + Sync {
    fn sync(&self, host: &dyn HostApi, req: SyncRequest) -> Result<SyncPage, PluginError>;
}

pub trait HostApi {
    fn http(&self, req: HttpRequest) -> Result<HttpResponse, HttpError>;
    fn log(&self, level: LogLevel, message: &str);
}
```

`SyncRequest` carries `resource` (the kind), `cursor` (the `next_cursor` of
the plugin's last page for this resource, or none when there has been none)
and `settings` (a JSON object, the user's settings with the manifest's
defaults).

A `SyncPage` holds:

- `upserts`: items to insert or replace. An `Item` has `id`, `title`, `url`
  (optional), `updated_at` (optional), `version` (optional, any string that
  changes when the record does) and `fields` (a JSON object).
- `deletes`: ids to remove.
- `next_cursor`: stored by the daemon and passed back on the next call,
  **also after a `done` page**: the next run's first call gets it. A plugin
  can keep what it needs between listings there (the GitHub plugin keeps
  each repository's `since` mark in it). Return none to start over.
- `done`: the listing is finished; the run ends, and the next run (one
  interval later) begins a new listing.
- `snapshot_complete`: with `done`, "this listing was the whole truth". The
  daemon then deletes every stored item of this resource that no page of the
  listing returned. Set it only when the plugin really listed everything.
  For this, a listing begins with a call that has no cursor or with the first
  call after a `done` page, and the daemon follows it across runs and
  restarts. A listing whose beginning the daemon did not see deletes nothing,
  whatever the plugin says.

A cursor can outlive a settings change: new settings make the resource due
at once but keep its cursor. A plugin must accept a cursor written under an
older configuration, for instance by noticing it no longer fits and starting
over (the GitHub plugin fingerprints its repository list in the cursor).

A page that returns no progress (not `done`, and the same cursor it was
given) ends the run with an error.

`Item.url`, when present, must be an absolute `http` or `https` URL with a
host, written plainly (no spaces or control characters). A page that breaks a
limit or a shape rule is refused as a whole; see [limits](#limits).

Errors a plugin returns (`PluginError`): `Auth` (the daemon shows the plugin
as needing sign-in, drops the secret values it cached for the plugin, and
stops its runs until settings change or a manual sync), `RateLimited { retry_after_secs }` (the next run waits for the longer
of that time and the resource's interval), `Settings` (the settings are wrong;
the plugin waits for new ones), `Http` and `Other` (the run fails and is
retried with a back-off that starts at 30 seconds, doubles, and stops at 30
minutes).

A plugin that panics or does not answer within 60 seconds is stopped; see
[host modes](#host-modes-and-discovery) for what that means in each mode.

## The manifest

Every plugin directory holds a `plugin.toml` and, for `dylib` and `wasm`
plugins, the entry file it names. The directory name must equal the plugin
`id`.

```toml
id = "github"                 # 2 to 32 chars: [a-z][a-z0-9-]*, no "--", no trailing "-"
name = "GitHub"
version = "0.1.0"             # semantic version
contract = 1                  # the contract version this plugin was written for
host = "dylib"                # "dylib" or "wasm"; "process" and "in-process" are not loadable
entry = "libsmithagents_plugin_github_dylib.dylib"   # a plain file name, no path
hosts = ["api.github.com"]    # the only hosts the plugin may call

[[tools]]                     # command-line programs the daemon runs (built-in plugins only)
name = "gh"
label = "GitHub CLI"
min_version = "2.40.0"        # optional
install_url = "https://cli.github.com"   # optional, https only
install = { mise = "gh", brew = "gh" }   # optional hints; only `mise` is ever run

[[secrets]]
key = "token"
label = "GitHub sign-in"
help = "Run: gh auth login"   # optional
hosts = ["api.github.com"]    # optional when the manifest has exactly one host
from_tool = { tool = "gh", args = ["auth", "token"] }   # optional, built-in plugins only

[[settings]]
key = "repositories"
label = "Repositories (owner/name)"
kind = "list"                 # "text", "list" (of strings) or "bool"
required = true               # optional
default = []                  # optional; must match the kind

[[resources]]
kind = "issue"
label = "Issues"
interval_secs = 300           # 60 to 86400
```

Names the parser enforces (a manifest that breaks one is rejected, and the
daemon lists the directory as not loadable):

| What | Rule |
| --- | --- |
| plugin id | `^[a-z][a-z0-9-]{1,31}$`, no `--`, no trailing `-` |
| secret key | `^[a-z][a-z0-9-]{0,63}$`, no `--`, no trailing `-` |
| setting key, resource kind | `^[a-z][a-z0-9_-]{0,63}$` |
| tool name | `^[a-z][a-z0-9_-]{0,31}$`, a bare executable name, never a path |
| host | a plain DNS name in lower case: at least two labels of `[a-z0-9-]`, none starting or ending in `-`, the last starting with a letter, at most 253 characters. No IP literal in any spelling, no `localhost`, no wildcard, no port |
| `entry` | a plain file name: no `/`, no `\`, no `..` |
| `install_url` | `https`, at most 2048 bytes |
| duplicates | a secret key, setting key, resource kind or tool name may appear once |
| `name`, every `label`, a secret's `help` | not blank, at most 128 characters (`help` 512), no control characters, line or paragraph separators or bidi marks |
| `from_tool.tool` | must name one of the manifest's `[[tools]]` |
| a secret's `hosts` | each must be one of the manifest's `hosts`; may be left out only when the manifest has exactly one host |

The id and the secret key rules are chosen so that the environment variable
for a secret, `SMITHAGENTS_SECRET_<PLUGIN>__<KEY>`, names one plugin and key
only (see [secrets](#secrets)). A manifest file larger than 256 KiB is
refused.

The scheduler starts a resource's next run `interval_secs` after the last one
(sooner after a failure, per the back-off above, and later after a rate
limit); a manual sync (`POST .../sync`) queues a run at once.

## HTTP policy

Every request goes through the host. These rules are fixed in code; no
manifest, setting or environment variable relaxes them.

- HTTPS only, port 443 only. The URL host must equal one of the manifest's
  `hosts` (exact, case-insensitive; no wildcards). A URL with a user name or
  password is refused.
- Redirects are not followed. The 3xx response comes back to the plugin.
- The host resolves the name itself and refuses addresses that are loopback,
  private (`10/8`, `172.16/12`, `192.168/16`), link-local, carrier-grade NAT
  (`100.64/10`), unspecified, multicast, reserved, or in documentation ranges.
  IPv6 is limited to global unicast outside the 6to4, Teredo and documentation
  blocks; IPv4-mapped IPv6 addresses are judged as IPv4.
  `198.18.0.0/15` is allowed on purpose: fake-IP DNS proxies such as Clash and
  Surge answer from that range, and refusing it would break users behind them.
- During a sync only `GET` and `HEAD` are allowed, and a request body is
  refused. (Writes are not built yet.)
- A plugin may not set `Authorization`, `Cookie`, `Host`, `Proxy-*`,
  `Connection`, `Content-Length`, `TE`, `Trailer`, `Transfer-Encoding`,
  `Upgrade` or `Keep-Alive`, nor, during a sync, a method-override header
  (`X-HTTP-Method-Override`, `X-HTTP-Method`, `X-Method-Override`). The
  credential header comes only from `auth`.
- No OS proxy settings, no cookie store.
- At most 50 requests per call; 30 seconds per request; response bodies at
  most 5 MiB; request bodies at most 1 MiB.

A refused request returns `HttpError::Denied` with a reason that never holds a
secret.

Authentication is named, not performed, by the plugin:

```rust
Auth::Bearer { secret: "token".into() }                       // Authorization: Bearer <value>
Auth::Basic { user_secret: "user".into(), password_secret: "pw".into() }
Auth::Header { name: "X-Api-Key".into(), secret: "key".into() }
```

The host checks that the secret is declared in the manifest and that the
request goes to one of the secret's `hosts`, before it resolves the value.

## Secrets

A plugin declares the secrets it needs with `[[secrets]]`. The user supplies
the values; they live outside the plugin and outside the database. The daemon
looks for a value in this order:

1. `<home>/secrets.toml` (default home `~/.smithagents`), a table per plugin
   id. Each key is either `{ env = "NAME" }` (read that environment variable
   of the daemon) or `{ command = ["program", "arg", ...] }` (run it without a
   shell; the first line it prints is the value). A key that appears in the
   file is resolved only from its entry. **An entry is authoritative even when
   it resolves to nothing**: an empty variable or a command that prints
   nothing leaves the secret missing; the daemon does not fall through to the
   variable below or to a tool.
2. The environment variable `SMITHAGENTS_SECRET_<PLUGIN>__<KEY>`: upper case,
   `-` written as `_`, and a **double** underscore between the plugin and the
   key. For plugin `fixture-dylib` and key `api-key`:
   `SMITHAGENTS_SECRET_FIXTURE_DYLIB__API_KEY`.
3. For a built-in plugin, the secret's `from_tool`, if it has one (see
   [command-line tools](#command-line-tools)).

`secrets.toml` must not be writable by group or others (the daemon refuses
it otherwise): `chmod 600 ~/.smithagents/secrets.toml`. It holds pointers, not
values. An example that takes the GitHub token from the GitHub CLI:

```toml
# ~/.smithagents/secrets.toml
[github]
token = { command = ["/opt/homebrew/bin/gh", "auth", "token"] }
```

Use an absolute path for the program: a daemon started from the app may not
have your shell's `PATH`. Resolved values are cached in memory for five
minutes; a plugin's cached values are dropped when the service refuses them
(an `Auth` error) and when the plugin is enabled, given new settings or
synced by hand, so a fresh sign-in counts at once. When a declared secret has
no value, the plugin shows `needs_auth` and is not called; when that secret
has a `secrets.toml` entry, the message says the entry has no value.

The daemon's own environment reaches agents and `git`, except every
`SMITHAGENTS_SECRET_*` variable, which the daemon removes from the processes
it starts. A `{ env = "NAME" }` pointer names a variable that agents still
inherit, so for anything sensitive prefer a `{ command = [...] }` pointer or
the `SMITHAGENTS_SECRET_...` form.

Secrets are bound to hosts. The `hosts` list inside a `[[secrets]]` table
names the hosts the secret may be sent to; with a single manifest host it may
be left out. A request that carries a secret to any other host is refused.

No secret value appears in a log line, an error message, an event, an API
response or a `Debug` output, and a plugin never receives one. The daemon's
own API token is held to the same rule.

## Command-line tools

A built-in plugin may depend on a command-line program, declared with
`[[tools]]`. The daemon, never the plugin, runs it. GitHub uses `gh` only to
sign in: its `token` secret has `from_tool = { tool = "gh", args = ["auth",
"token"] }`, so when no `secrets.toml` entry or environment variable gives a
token, the daemon runs `gh auth token`. All GitHub data comes over HTTPS
through the host.

- **Lookup.** A tool is found by its bare name in a fixed list of absolute
  directories, never through a shell and never relative to the working
  directory: `PATH`, mise's shims directory (`MISE_DATA_DIR`, else
  `$XDG_DATA_HOME/mise`, else `~/.local/share/mise`, then `shims`),
  `/opt/homebrew/bin`, `/usr/local/bin` and `~/.local/bin`. A candidate counts
  only if it is a regular executable file owned by root or the current user
  and not writable by its group or by others. Symbolic links are followed (a
  Homebrew or mise command is a link); their target must pass.
- **Version.** With `min_version`, the daemon runs `<tool> --version` and
  compares the first version in the first line. Without `min_version` the
  tool is not run to check it.
- **State.** When a tool is missing or too old, the plugin's state is
  `needs_setup`, naming the tool, why, its `install_url` and `install` hints.
  Nothing runs until it is fixed. A manual sync checks again.
- **Installing.** Nothing is installed on its own. On an explicit request,
  `POST /v1/plugins/{id}/tools/{name}/install` runs `mise use --global <spec>`
  for the tool's `install.mise` entry (this installs the tool and adds it to
  the user's global mise configuration, so it is on the user's `PATH` as
  well). mise is never installed by the daemon; if it is missing the answer is
  `mise_missing` with the page to install it from. One install runs at a time,
  for at most 10 minutes. Only built-in plugins' tools can be installed.
- **Environment.** Tool and command processes run with stdin closed and
  stderr discarded, in a process group of their own that is killed on timeout
  or when the output is too long, and with every `SMITHAGENTS_*` variable
  removed from their environment (plugin secrets among them).
- **Installed plugins.** `[[tools]]` and `from_tool` are honoured for
  built-in plugins only. An installed plugin that declares a tool shows
  `needs_setup`, because approving a third party's tools is not built yet.

## Host modes and discovery

`host` in the manifest picks how the plugin's code runs.

| Mode | What it is | Who may use it |
| --- | --- | --- |
| `dylib` | A native library, loaded into the daemon's process through a stable ABI (`stabby`). | Built-in plugins only: the directory next to the daemon executable. Third-party code never runs natively in the daemon. |
| `wasm` | A WebAssembly component (`wasm32-wasip2`), run in a sandbox (Wasmtime). | Built-in and installed plugins. This is the mode for third-party plugins. |
| `process` | Reserved. Not built; a manifest that asks for it is refused. | Nobody yet. |
| `in-process` | Parses, but no directory loads it: the daemon refuses it. Tests link a plugin straight into the manager instead. | Tests only. |

**The wasm sandbox.** The linker provides no filesystem, no sockets and no
`wasi:http`: the guest can compute and call `host.http` and `host.log`, and
that is all. Limits per call: 10 seconds of guest CPU (the clock counts time
the guest is executing; time spent waiting on the host's `http` does not
count), 64 MiB of memory (linear memories and tables together), 60 seconds of
wall time. What a guest can make the host hold is capped too: 1024 open WASI
resources and 1 MiB per random-bytes request. A guest that spins or grows
without end is stopped and the run fails; the daemon is not affected. A
component file larger than 64 MiB is refused before it is read.

**A native plugin is not sandboxed.** A `dylib` runs with the daemon's rights.
A panic is caught and reported as a failed call, but a native library that
never returns cannot be stopped; that is why only code we build and ship is
allowed to be `dylib`. After a panic in `dylib` mode the plugin's state
becomes `failed` and stays so until the daemon restarts; the daemon keeps
answering and other plugins are unaffected.

**Where plugins live.**

- Built-in: `<directory of the daemon executable>/plugins/<id>/`. Built-in
  plugins start enabled.
- Installed: `<home>/plugins/<id>/` (home defaults to `~/.smithagents`).
  Installed plugins start disabled and are `wasm` only.

**Discovery's trust rules** (Unix). The anchor is the directory that holds the
daemon executable: whoever can write there can replace the daemon itself, so
checks start at the built-in `plugins` directory and go down, never to its
ancestors. The plugin directory, `plugin.toml`, and the entry file (and for
built-ins also the built-in root, and the directories a symbolic link to the
entry passes through) must be owned by the current user or root and not
writable by group or others. Also:

- the plugin directory must not be a symbolic link;
- the directory name must equal the manifest's `id`;
- an installed plugin may not use the id of anything found in the built-in
  directory (built-in ids are reserved, even when the built-in one is broken);
- an installed plugin may not be `dylib`, and `process` and `in-process` are
  refused for every plugin;
- the installed directory may not overlap the built-in one;
- the built-in directory must not be a symbolic link; if it is, nothing in it
  loads and no installed plugin loads either (the ids it would reserve cannot
  be known);
- the directory and the entry file are checked again just before loading, so
  a file replaced between discovery and load is refused.

A directory that fails a check is skipped and listed with a plain-sentence
reason; one bad plugin never stops the daemon or other plugins. A plugin that
passes discovery but cannot be built into a runtime (for example, an entry
file that is not a component) is listed with state `load_failed`. So is a
plugin whose stored configuration or sync state cannot be read at start
("this plugin's stored state could not be read"): it is listed as disabled,
and the daemon neither runs it nor writes over those rows.

## The API

All routes are under `/v1/plugins`, on the daemon's loopback port, with the
header `Authorization: Bearer <daemon token>`. Errors are JSON:
`{"error": "<code>", "message": "<text>"}`.

| Route | What it does |
| --- | --- |
| `GET /v1/plugins` | Every plugin, sorted by id. |
| `GET /v1/plugins/{id}` | One plugin's status. |
| `POST /v1/plugins/{id}/enable`, `.../disable` | Turns it on or off; enabling starts a sync. |
| `GET /v1/plugins/{id}/settings` | The settings as the user gave them. |
| `PUT /v1/plugins/{id}/settings` | Replaces the settings (a JSON object, at most 64 KiB, checked against the manifest). |
| `POST /v1/plugins/{id}/resources/{kind}/sync` | Queues a run of one resource now. |
| `GET /v1/plugins/{id}/resources/{kind}/items?limit=&after=&q=` | One page of stored items (`limit` 1 to 200, default 50; `after` is the previous page's `next`; `q`, at most 256 characters, keeps the items whose title contains it: titles only, not `fields`). |
| `POST /v1/plugins/{id}/tools/{name}/install` | Installs a built-in plugin's tool with mise (see above). |

A plugin's status carries `id`, `name`, `version`, `host`, `builtin`,
`enabled`, `state`, `hosts`, `secrets` (keys only), `settings` (fields) and
`resources` (each with `item_count`, `last_run_at`, `last_ok_at`,
`next_due_at`, `last_error`, `syncing`). `state` is an object with a `kind`:
`disabled`, `ready`, `needs_auth` (names the missing secrets), `needs_settings`
(a required setting is missing or invalid), `needs_setup` (a tool is missing or
too old), `failed`, or `load_failed`. Messages in states and errors are
cleaned of control characters and cut to 1 KiB.

Error codes: `unauthorized` (401, missing or wrong token);
`plugin_not_found`, `resource_not_found`, `tool_not_found` (404);
`invalid_settings` (400, settings the manifest refuses), `bad_cursor` (400,
malformed `after`), `payload_too_large` (settings body over the limit),
`no_installer` (400, the tool has no mise package); `not_allowed` (403,
installing the tool of an installed plugin); `plugin_disabled` (409, a sync
of a disabled plugin, or enabling or new settings for a plugin whose stored
state could not be read), `sync_in_progress` (409, that resource is syncing
now), `plugin_not_ready` (409, a sync of an enabled plugin that cannot run
now: settings missing or invalid, `failed`, `load_failed`, or tools that need
approval; the message says why), `install_in_progress` (409),
`mise_missing` (409); `install_failed` (502, mise failed or timed out);
`internal` (500).

Events on `/v1/events` include `plugin_updated` (a status changed) and
`resource_synced` (a run stored items: plugin, resource, counts).

## The GitHub plugin

`plugins/github` reads, and never writes, three resources for the repositories
the user lists:

| Resource | Interval | Items |
| --- | --- | --- |
| `repository` | 1 hour | One item per repository: description, private and archived flags, default branch, open-issue and star counts, push time. A complete listing: the `repository` item of a repository removed from settings is deleted (its issues and pull requests are not; see below). |
| `issue` | 5 minutes | Issues (pull requests that the issues endpoint also lists are skipped). Id `owner/name#number`; `version` is `updated_at`; fields include state, author, labels, assignees, comment count. Bodies are not stored. |
| `pull_request` | 5 minutes | Pull requests with state, draft and merged flags, author, labels, requested reviewers, head and base branch names. |

Details that matter when something looks off:

- Setting `repositories` is a list of `owner/name`; names are checked, case
  folded for duplicates, sorted, and at most **50 repositories** are accepted.
- **A repository removed from `repositories` keeps its issues and pull
  requests.** They are listed by update time, never as a complete snapshot,
  so nothing deletes them, and the host cannot yet delete items by
  repository. They stay stored until a host-side delete-by-scope exists.
- Issues and pull requests are listed **newest update first**
  (`sort=updated&direction=desc`), bounded by `since` after the first run. A
  change that lands while a listing is being read can then only repeat an item
  at a page boundary, never skip one. Each repository's `since` mark is
  committed only when its listing has been read to the end.
- Next pages are followed from the `Link` header only when they are on
  `api.github.com`. A `rel="next"` link anywhere else is an **error**, not a
  quiet stop: stopping would commit a mark over pages that were never read.
- Status mapping: 401 is `needs_auth`; 403 or 429 with `Retry-After` or
  `X-RateLimit-Remaining: 0` is a rate limit, waiting until the reset time
  (kept between one minute and one hour); 404 and a moved repository are
  settings errors naming the repository.
- **Sign-in.** Run `gh auth login`, once. With no `secrets.toml` entry and no
  `SMITHAGENTS_SECRET_GITHUB__TOKEN`, the daemon runs `gh auth token` for the
  token. `gh` is used for sign-in only; the plugin makes no call through it. If
  `gh` is not installed the plugin shows `needs_setup` with the install hints,
  and `POST /v1/plugins/github/tools/gh/install` installs it with mise on request. To
  keep the token out of the `gh` store, use `secrets.toml` as shown in
  [secrets](#secrets) or the environment variable instead.
- On a fresh home with no `repositories` setting, `github` is listed as a
  built-in and enabled, with state `needs_settings`. Nothing runs and nothing is
  requested until the setting is given.

## Packing plugins

`cargo plugins pack [--release] [--out <dir>] [--fixtures]` builds the plugin
crates and writes `<out>/<id>/plugin.toml` beside the entry file, the layout
the daemon discovers. Without `--out` the directory is
`target/<debug|release>/plugins`, the one the daemon binary of the same profile
reads (`target/debug/smithagentsd` reads `target/debug/plugins`). Without
`--fixtures` only the shipped plugins are packed (today: `github`); with it,
the test fixtures too.

Details:

- Builds go to `target/plugin-pack`, apart from the workspace's own target
  directory.
- A `dylib` plugin's entry file is named for the platform (`lib<name>.dylib`
  on macOS, `lib<name>.so` on Linux, `<name>.dll` on Windows). The packer
  writes the manifest with `entry` set to the file it actually built, replacing
  whatever the source `plugin.toml` says, and checks the result with the
  manifest parser. That is why `plugins/github/plugin.toml` names the macOS
  file: it is the source form, and the packed form says what is on disk.
- Files are replaced atomically (a daemon reading the directory sees the old
  file or the new, never a part) and given explicit modes: manifest and wasm
  files `0644`, libraries `0755`. They are never group- or world-writable,
  because discovery refuses those.

**Writing a native built-in plugin.** A crate of its own, as in
`plugins/github-dylib`: `crate-type = ["cdylib"]`, a library name that differs
from the logic crate's, dependencies on `smithagents-plugin-protocol` (feature
`dylib`) and on `stabby` itself (the export attribute looks `stabby` up by
name in the crate's dependencies), and one line:

```rust
smithagents_plugin_protocol::export_dylib_plugin!(my_plugin::MyPlugin);
```

Keep the logic in a separate crate with no platform-specific code, so the same
source also builds for `wasm32-wasip2`; `plugins/fixture-wasm` shows the
matching `export_wasm_plugin!` wrapper for a `wasm` plugin. To pack a new
plugin, add it to the list in `tools/plugins/src/lib.rs`.

## Checking with curl

The daemon writes `<home>/run/daemon.json` with its `port` and `token`. With a
home of `~/.smithagents`:

```sh
port=$(jq -r .port ~/.smithagents/run/daemon.json)
token=$(jq -r .token ~/.smithagents/run/daemon.json)

curl -s -H "Authorization: Bearer $token" "http://127.0.0.1:$port/v1/plugins" | jq .
curl -s -H "Authorization: Bearer $token" "http://127.0.0.1:$port/v1/plugins/github" | jq .state

# Give GitHub something to read, then look at what it stored.
curl -s -X PUT -H "Authorization: Bearer $token" -H 'Content-Type: application/json' \
  -d '{"repositories": ["owner/name"]}' "http://127.0.0.1:$port/v1/plugins/github/settings"
curl -s -H "Authorization: Bearer $token" \
  "http://127.0.0.1:$port/v1/plugins/github/resources/issue/items?limit=5" | jq '.items[].title'
```

Treat the token like a password: it grants full control of the local daemon.
Do not paste it into a bug report; it is not in the daemon's log by design.

## Limits

| What | Limit |
| --- | --- |
| One call into a plugin | 60 seconds |
| Wasm guest CPU per call | 10 seconds (epoch ticker every 100 ms, deadline 100 ticks) |
| Wasm memory | 64 MiB |
| HTTP requests per call | 50 |
| One HTTP request | 30 seconds |
| Response body | 5 MiB |
| Request body | 1 MiB |
| Upserts / deletes per page | 500 each |
| `fields` per item | 64 KiB |
| Title | 512 characters |
| Item id, cursor, `url`, `version` | 4 KiB each |
| Pages per run | 20 |
| Settings body | 64 KiB |
| Manifest file | 256 KiB |
| Wasm component file | 64 MiB |
| Log lines per call | 100, then one warning; the rest are dropped |
| One log line | 2 KiB |

A page over a limit, with a duplicate id, an empty id, `fields` that is not an
object, or a bad `url` is refused as a whole and the run fails.

## Not built yet

Be aware before relying on these; they do not exist today.

- Views for plugin data and a settings or sign-in screen in the app. Today a
  plugin is configured through the API (or `curl`) and `secrets.toml`; the
  manifest has no `views`.
- Writes: proposals, approval, conflict detection and audit. During a sync a
  plugin can only read, and `POST` and friends are refused.
- The Keeper as a secrets source (only `secrets.toml`, the environment and
  `from_tool` exist).
- Action prompts.
- A log tail or streaming resource kind.
- The `process` host mode.
- Signing: built-in libraries are not signed or notarized with our Team ID,
  and wasm plugins carry no publisher signature.
- A marketplace and its permission prompt.
- Approval of a third-party plugin's command-line tools: an installed plugin
  that declares `[[tools]]` stays in `needs_setup`.
- GitHub Enterprise hosts and webhook-driven freshness.
- Deleting by scope: the issues and pull requests of a repository removed
  from GitHub's settings stay stored until the host can delete a scope of
  items.
