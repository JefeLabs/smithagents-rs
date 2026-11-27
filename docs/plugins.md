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
[the feedback plugin](#the-feedback-plugin), [packing](#packing-plugins), [checking with curl](#checking-with-curl),
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

**Tools the daemon's own code runs.** A built-in plugin whose behaviour is
daemon code (today the [feedback plugin](#the-feedback-plugin)) has its
tools run through the plugin manager (`run_tool`): only a tool that this
built-in plugin's own manifest declares, with the given arguments, in a given
directory (absolute; a relative one is refused), under the same rules as
every tool above (lookup, no shell, stdin closed, stderr discarded, no
`SMITHAGENTS_*` variable, a timeout, at most 64 KiB of output, the first line
returned). While the plugin is disabled, failed, or the daemon is stopping,
its tools are refused and nothing runs; the feedback tool reports them as
"turned off or not running". A run that fails carries nothing the tool
printed: through the API it is `tool_failed` (502) with a fixed sentence
such as "the tool failed (exit status 1)".

**A plugin without resources** never runs, so nothing checks its tools
before a run. Its tools are checked when its status is read instead (a check
is trusted for 60 seconds): a missing or too old one makes its state
`needs_setup`, naming the tool, with the install hints. For such a plugin
this is advice and holds nothing back.

## Host modes and discovery

`host` in the manifest picks how the plugin's code runs.

| Mode | What it is | Who may use it |
| --- | --- | --- |
| `dylib` | A native library, loaded into the daemon's process through a stable ABI (`stabby`). | Built-in plugins only: the directory next to the daemon executable. Third-party code never runs natively in the daemon. |
| `wasm` | A WebAssembly component (`wasm32-wasip2`), run in a sandbox (Wasmtime). | Built-in and installed plugins. This is the mode for third-party plugins. |
| `process` | Reserved. Not built; a manifest that asks for it is refused. | Nobody yet. |
| `in-process` | A plugin compiled into the daemon, registered from a manifest embedded in the binary. No directory can ask for it. | Built-in plugins inside the daemon, and tests. |

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

- Embedded: compiled into the daemon (today only `feedback`). They are
  listed before every plugin directory and start enabled.
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
- no directory, built-in or installed, may use the id of a plugin compiled
  into the daemon (`feedback`): it is skipped with "a built-in plugin has this
  id" before anything in it is loaded;
- an installed plugin may not be `dylib`, and `process` and `in-process` are
  refused for every plugin directory;
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
plugin one of whose stored rows (its configuration or a resource's sync
state) holds a value the daemon cannot decode at start ("this plugin's stored
state could not be read"): it is listed as disabled, its settings read as the
manifest's defaults, and the daemon neither runs it nor writes over those
rows; disabling it changes nothing. **Enabling it, or saving its settings,
repairs it:** in one transaction its configuration is replaced (enabled with
the manifest's defaults, or the saved settings with the origin's usual
enabled state: built-in on, installed off) and its sync state is dropped, so
it starts again from no cursor. Its stored items stay until its next
complete listing reconciles them. Any other database error while reading a
plugin's rows still stops the daemon at start.

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
of a disabled plugin, including one whose stored state could not be read),
`sync_in_progress` (409, that resource is syncing
now), `plugin_not_ready` (409, a sync of an enabled plugin that cannot run
now: settings missing or invalid, `failed`, `load_failed`, or tools that need
approval; the message says why), `install_in_progress` (409),
`mise_missing` (409); `install_failed` (502, mise failed or timed out);
`tool_failed` (502, a command-line tool the daemon ran for a built-in plugin
failed; a fixed sentence with an exit status at most); `internal` (500).

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

## The feedback plugin

`feedback` is the daemon side of the app's Feedback pane (see
[`docs/feedback.md`](feedback.md)). Its manifest is embedded in the daemon
binary (host mode `in-process`); it is listed with `builtin: true`, starts
enabled, and no plugin directory can take its id. It syncs nothing: it has no
resources, hosts, secrets or settings, only two tools:

```toml
id = "feedback"
name = "Feedback"
version = "0.1.0"
contract = 1
host = "in-process"

[[tools]]
name = "gh"
label = "GitHub CLI"
install_url = "https://cli.github.com"
install = { mise = "gh", brew = "gh" }

[[tools]]
name = "agent-browser"
label = "agent-browser"
install = { mise = "npm:agent-browser" }
```

- **`gh`** is the only tool the daemon runs for it: `gh auth status
  --hostname=github.com` (signed in?), `gh repo view` (is the repository
  public?), `gh label list`, `gh issue list` and `gh issue create` for "A
  GitHub issue", and `gh pr list` and `gh pr create --draft` for "Open pull
  request". It runs from the daemon's home, never inside a repository, always
  with `--repo=github.com/<owner>/<name>`, and with `GH_PROMPT_DISABLED=1`,
  `GH_NO_UPDATE_NOTIFIER=1` and `NO_COLOR=1`. Its searches for an issue or a
  pull request made before look only at the signed-in user's own
  (`--author=@me`).
- **`agent-browser`** is only looked up: "Demo video" is offered while it is
  installed, and the idea intent's agent runs it.
- **State.** `ready`, or `needs_setup` naming a missing tool. This is advice
  (see [a plugin without resources](#command-line-tools)): capture, files and
  the intent path work without either tool, and only the choices that need
  one are unavailable. `POST /v1/plugins/feedback/tools/gh/install` and
  `.../tools/agent-browser/install` install them with mise.
- **Turned off** (`POST /v1/plugins/feedback/disable`), both tools are
  refused; the app says "The feedback plugin is turned off or not running."
  and the intent path still works.
- **What it pushes.** Nothing, except for an issue submission (`send_as:
  github_issue`) and `POST /v1/intents/{id}/pull-request`: then the recorded
  commit itself, to `origin`, never forced, with the user's own git
  credentials (the daemon supplies none). A push is refused (`unpushed_commits`)
  unless everything under the folder commit is already on
  `refs/remotes/origin/*`, so the user's unpushed commits are not published
  with it (what this check cannot see is listed under
  [known gaps](feedback.md#not-built--known-gaps)). On a repository that is
  public, or whose visibility `gh` cannot tell, both are refused (`public_not_acknowledged`) unless the request says
  `public_acknowledged: true`.

All routes are behind the daemon's bearer token. The ones that commit or
push run detached: a client that goes away does not stop them half-way, and
the daemon does not stop for idleness while one runs.

| Route | What it does |
| --- | --- |
| `GET /v1/feedback/targets` | Every repository with `has_github_remote` (its configured `origin` is on github.com) and `is_public` (`true`, `false`, or `null` when unknown), the meta repository, and whether `gh` (signed in) and `agent-browser` are ready, with a `reason` when not. No remote URL is returned. |
| `GET /v1/settings/feedback`, `PUT /v1/settings/feedback` | The meta repository, the one feedback goes to by default: `{"meta_repo_id": "<repo id>" \| null}`. |
| `POST /v1/feedback` | A submission (below). Answers with the outcome: `submission_id`, `repo_id`, `branch`, `commit`, `folder`, `pushed`, `intent`, `issue_url`, and `failed` (`step`, `code`, `message`) when a step after the commit failed. The same id again continues that submission: nothing is committed twice, and only what has not succeeded runs. |
| `POST /v1/feedback/{id}/retry` | Runs what has not succeeded of the recorded submission `id` (the intent, or the push and then the issue); the same answer. |
| `POST /v1/intents/{id}/pull-request` | An idea intent's "Open pull request". Optional body `{"public_acknowledged": true \| false}` (none means `false`). Answers with the intent; `origin.idea.pr_url` holds the pull request. |
| `POST /v1/intents/{id}/implementation` | An idea intent's "Create implementation intent". Optional body `{"public_acknowledged": true \| false}` (none means `false`), as for the pull request: the new intent's agent opens a pull request that carries the idea's files. 201 with the new intent, or with the one made before. |

**The submission** is `multipart/form-data`: the part `request` first, then
`file-<i>` for every attachment and `annotated-<i>` for every screenshot (`i`
is the attachment's 1-based position), each exactly once. The daemon names
every file; a part's own file name is never used as a path. Files are checked
by their first bytes (a screenshot's two parts are PNG; an image is PNG,
JPEG, GIF or WebP; a video has an ISO media `ftyp` box). Parts are written
to a private directory under `<home>/tmp/feedback/` as they stream in,
removed when the request ends, and leftovers are removed at daemon start. The
`request` part:

```json
{
  "submission_id": "<a uuid the client makes, one per draft>",
  "main_idea": "Fix the zoom\n\nThe page jumps when I zoom in.",
  "kind": "bug",
  "repo_id": "<repo id>",
  "send_as": "intent",
  "idea": {"explainer": true, "diagrams": false, "demo_video": false},
  "public_acknowledged": false,
  "app_version": "0.1.0",
  "platform": "macos",
  "attachments": [
    {"kind": "screenshot", "original_name": "screenshot.png",
     "boxes": [{"x": 0.1, "y": 0.2, "w": 0.3, "h": 0.1, "comment": "This jumps"}]}
  ]
}
```

`kind` is `new_feature`, `enhancement` or `bug`; `send_as` is `intent` or
`github_issue`; `idea` (optional) needs `intent` and at least one part;
`public_acknowledged` (optional, `false` when left out) matters for an issue
and for a plain intent (no `idea`) to a repository whose `origin` is on
GitHub; an attachment's `kind` is `screenshot`, `image` or `video`, and only a
screenshot has `boxes`, given as fractions of the image (0 to 1);
`app_version` and `platform` are 1 to 32 of `A-Z a-z 0-9 . _ + -`.

Error codes (errors are `{"error": "<code>", "message": "<text>"}`; every
message is a fixed sentence, never something git or `gh` printed):

- `POST /v1/feedback`, before anything is committed or recorded: `invalid`
  (400: the request or the upload's shape, a box outside its image, an idea
  package with no part or sent as an issue), `unsupported_file` (400: "screenshot
  1 is not a png file"), `payload_too_large` (413: a part, the request part or
  the whole upload over its limit), `repo_not_found` (404); 409
  `default_branch_missing`, `git_identity_missing`, `no_free_name`,
  `name_taken`, `folder_blocked`, `branch_blocked`, `tool_missing` (Demo video
  without `agent-browser`); for an issue `no_github_remote`, `gh_not_ready`,
  `unpushed_commits` and `public_not_acknowledged`; for a plain intent to a
  repository whose `origin` is on GitHub `public_not_acknowledged`; for an
  idea package to one, `unpushed_commits` once the repository has been
  fetched from `origin`. These are checked before a commit is made and before
  one a stopped daemon left is taken up. Also 500 `git_failed`, `io_error`,
  `store_error`. A recorded submission that is continued can also answer
  `intent_not_found` (its intent was deleted) or `repo_not_found` (404).
- `POST /v1/feedback/{id}/retry`: `bad_request` (400, not a submission id);
  `submission_not_found`, `repo_not_found`, `intent_not_found` (404);
  `store_error` (500).
- In `failed` (a 200 answer: the commit exists): step `create_intent` with the
  intent service's code; step `push` with `push_rejected`, `push_auth`,
  `push_no_remote`, `push_timeout`, `push_failed`, `commit_missing` ("The
  feedback commit is no longer in this repository, so it cannot be pushed."),
  `unpushed_commits`, `no_github_remote`, `git_failed` or `io_error`; step
  `create_issue` with `gh_not_ready`, `gh_failed`, `gh_timeout`,
  `gh_bad_answer`, `issue_unconfirmed` ("GitHub has not confirmed whether the
  issue was created. Try again in a minute."), `no_github_remote`,
  `git_failed` or `io_error`.
- `POST /v1/intents/{id}/pull-request`, with nothing pushed: `bad_request`
  (400, a malformed body or id); `intent_not_found`, `repo_not_found` (404);
  409 `not_an_idea`, `invalid_state` (not sent yet, or the agent is still
  working), `idea_agent_missing` ("the idea intent's agent no longer
  exists"), `idea_branch_missing` ("the idea agent's branch no longer
  exists"), `idea_origin_invalid` ("the idea intent's folder or branch is not
  one Submit made"), `idea_out_of_scope` (the agent's branch changes
  files outside the issue folder, lacks the idea's commit, or has more than
  1000 commits on top of it), `idea_lfs` ("The idea's files were committed
  through Git LFS, which this push does not upload. Turn Git LFS off for
  docs/issues and ask the agent to commit them again."), `idea_incomplete`
  ("The idea's issue.md is missing from the agent's branch, so a pull
  request cannot be opened."), `unpushed_commits`, `public_not_acknowledged`,
  `no_github_remote`, `gh_not_ready`; 500 `git_failed` (git could not read
  the repository), `io_error`, `store_error`. Every one of these comes before
  the commit to push is recorded. A failed push or `gh` call is 502 with the
  step's own code: `push_rejected`, `push_auth`, `push_no_remote`,
  `push_timeout`, `push_failed`, `commit_missing` (the push), `gh_failed`,
  `gh_timeout`, `gh_bad_answer`, `gh_not_ready`, `pr_unconfirmed`, or
  `io_error` when the pull request's body could not be written (`gh`); what
  succeeded before it stays done. (On this route `unpushed_commits` and
  `no_github_remote` are 409, and `git_failed` is 500.)
- `POST /v1/intents/{id}/implementation`: `bad_request` (400, a malformed
  body or id); `intent_not_found`, `repo_not_found` (404); 409
  `not_an_idea`, `invalid_state`, `idea_agent_missing`,
  `idea_branch_missing`, `idea_origin_invalid`, `public_not_acknowledged`
  (its repository's `origin` is on GitHub, the body does not acknowledge, and
  `gh` does not say it is private; checked before anything is made); 500
  `git_failed`, `io_error` (git could not read the repository or the agent's
  branch), `store_error`; the intent service's codes when the intent cannot
  be made.
- `PUT /v1/settings/feedback`: `repo_not_found` (404), `bad_request` (400).

The two `unpushed_commits` sentences: "The default branch has commits that
are not on origin. Push them first, so they are not published with this
feedback." and, when the repository has no `refs/remotes/origin/*` at all,
"This repository has never been fetched from origin, so it is not known what
is already published. Run git fetch first." The two `public_not_acknowledged`
sentences: "This repository is public. The screenshots will be visible to
anyone. Confirm that before sending." and, when `gh` cannot tell (also when
it is not installed or not signed in), "This repository may be public. The
screenshots will be visible to anyone. Confirm that before sending." A
request that acknowledges is not asked of `gh` at all. A submission is asked
for the acknowledgement only before its commit (or before a commit a stopped
daemon left is taken up); a recorded submission that is continued or retried
is not asked again. The pull-request route is asked on every call, the
implementation route on every call until its intent is made. An intent
whose agent's worktree cannot be made from its base branch is refused at
Send with 409 `base_branch_failed`, "The intent's base branch could not be
checked out."

Limits: see [limits](#limits).

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
| Feedback: attachments per submission | 10 |
| Feedback: one image (a screenshot and its annotated picture each count as one) | 10 MiB |
| Feedback: one video | 50 MiB |
| Feedback: every file part together | 100 MiB |
| Feedback: the `request` part | 1 MiB |
| Feedback: the whole upload body | 101 MiB (every file part plus 1 MiB) |
| Feedback: boxes per screenshot; a comment; the main idea | 20; 500 characters; 20,000 characters |
| Feedback: the issue body sent to GitHub | 60,000 characters |
| Feedback: a push | 120 seconds |
| Feedback: one `gh` call; `gh auth status` and `gh repo view` | 60 seconds; 5 seconds |
| Feedback: a local git command | 60 seconds |

The feedback sentences print sizes as "MB" (`10 MB`, `100 MB`); they are
binary megabytes, as in this table.

A page over a limit, with a duplicate id, an empty id, `fields` that is not an
object, or a bad `url` is refused as a whole and the run fails.

## Not built yet

Be aware before relying on these; they do not exist today.

- Views for plugin data and a settings or sign-in screen in the app. Today a
  plugin is configured, and a built-in plugin's tools are installed, through
  the API (or `curl`) and `secrets.toml`; the manifest has no `views`.
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
