# Configuring Holosmith

One TOML file per machine, `<root>/config.toml`, read by every verb (`serve`, `doctor`, `deploy`, `status`,
`build`, `hub host`, …). The root is `HOLOSMITH_ROOT`, default `/var/lib/holosmith` — one directory holding this
file, the data root, the release tree and the projects (ADR-0086). `--config PATH` names another file; a named file
that does not exist is an error, while a missing default path means the built-in defaults.

```bash
holosmith config show       # the resolved config, defaults and all, as valid TOML
holosmith config validate   # every problem in the file, at once
holosmith config edit       # $VISUAL or $EDITOR, then validate what was saved
```

The built-in defaults, the file, and then the command line: `--port` moves the control listener **and** the control
origin's port together, `--data-dir` replaces the data root. Nothing else is a flag (ADR-0006).

Three rules hold for the whole file:

- **An unknown key is a problem**, never ignored — a typo must not silently fall back to a default.
- **Every problem is reported at once**, each as `path:line:column: key: message`, so one run of `config validate`
  is one pass at the file.
- **Unset optionals are printed as comments** (`# owner unset`), never omitted: a key that disappears reads as a
  fallback, and `config show` output re-reads as the same value.

The sections below are the keys by what they decide. Types are TOML types; *duration* means a string with a unit —
`"1500ms"`, `"10s"`, `"2m"` — because a bare number does not say which unit it means.

## Which half of the system this machine runs

| Key | Type | Default | Meaning |
|---|---|---|---|
| `role` | string | `"both"` | `hub`, `server` or `both`. A **server** runs sessions (Docker, git, a projects root). A **hub** forwards to servers and runs no containers. Every role answers the same paths. |
| `backend` | string | `"docker"` | `docker` or `stub`. What `serve` runs unless `--backend` names another for this run. `stub` provisions nothing outside the process, so it needs no daemon and no repository. |

A deploy adds its half to the role: `server deploy` on a `hub` machine leaves it `both`, and the reverse. Nothing is
refused over a role.

## Where it listens, and what it answers to

| Key | Type | Default | Meaning |
|---|---|---|---|
| `control_listen` | address | `"127.0.0.1:8080"` | The control listener: API, event streams, embedded app. Any `host:port`. Port `0` asks the kernel. |
| `preview_listen` | address | `"127.0.0.1:8081"` | The preview listener, a second origin that serves only `/<host>/<id>/*`. Must differ from `control_listen` (two ports of `0` are not a collision). |
| `control_origin` | origin | `"http://localhost:8080"` | The origin a browser reaches the control listener on. Every request that changes something must carry it as its `Origin`, and it is the origin the app is told to use. `http(s)://host[:port]`, no path, query or fragment. |
| `preview_origin` | origin | `"http://localhost:8081"` | The same for the preview listener. Must differ from `control_origin`: a preview must not share the control origin (ADR-0051). |
| `preview_upstream_port` | integer | `3000` | The port a session's own dev server listens on **inside its container**. The preview listener forwards to it over the session's private network; one key for every session, because each container has its own network namespace and the image declares no exposed port. |
| `allowed_hosts` | string array | `[]` | The `Host` values this machine answers. Each is a bare name (`letters`, digits, `.`, `-`, `_`) or a bracketed IPv6 literal — no scheme, no path, no port. `localhost` and `127.0.0.1` are always allowed, so a default build serves its own app out of the box; `[::1]` is not, and a machine that wants it lists it. Duplicates are refused. |
| `owner` | string | unset | The tailnet login requests must come from. When set, a request carrying a `Tailscale-User-Login` header must match it exactly, and two values for that header are refused. A request with no such header — direct access, not behind `tailscale serve` — passes, so this is a check on the proxy in front, not an alternative to tailnet ACLs. |
| `machine_name` | string | `"local"` | What this machine calls itself: the word a push notification's payload carries, the path segment a tap opens (`/hosts/<name>/sessions/<id>`), and `/api/health`'s `name`. Lowercase letters, digits and `-`, at most 63 characters. `local` is the reserved word for *the machine whose page this is*; a multi-machine deployment sets it to the registry row's own name, and a mismatch is a `404 unknown_host` when a notification is tapped. |

The four browser-facing checks — `bad_host`, `bad_origin`, `bad_client`, `bad_owner` — are decided by these keys and
applied to every route but the health probe. There is one more rule they do not cover: a machine accepts forwarded
calls from another machine only if that peer's **tailnet node name** is in `allowed_hubs` (see
[Several machines](#several-machines)).

## Where its files live

| Key | Type | Default | Meaning |
|---|---|---|---|
| `data_root` | path | `"/var/lib/holosmith/data"` | The data root: `app.db` and its WAL, session directories, `secrets/`. Absolute, and must not be equal to or inside `projects_root`. |
| `projects_root` | path | `"/var/lib/holosmith/projects"` | The projects root. Each project is a **repository directory directly inside it** (`<projects_root>/<project>` with a `.git` directory), and session worktrees are made under `<projects_root>/.worktrees/<id>`. Absolute, and must not be equal to, inside, or a parent of `data_root` — deleting data must never touch a repository. |
| `docker_host` | string | unset | The daemon `docker -H` talks to: `unix://…`, `tcp://…` or `ssh://…`. Unset means the client's own default (`DOCKER_HOST`, else `unix:///var/run/docker.sock`). An `ssh://` daemon is a complaint in the preflight rather than a pass, because the checks that need a container cannot run. |

`<data_root>/secrets/keys.toml` (mode 0600) is **not** a config key: `holosmith key set <provider>` owns it, and
`key list` never prints a value. `<data_root>/secrets/vapid.toml` holds the push keys, written once by the first
deploy and never regenerated (ADR-0058).

## What a session's container is

| Key | Type | Default | Meaning |
|---|---|---|---|
| `agent_image` | string | `agent-image:<version>` | The image a session's container runs. Non-empty. The version is this binary's, which is the image a deployed machine already has. |
| `agent_toolchains` | string array | `["python3", "ripgrep"]` | Debian packages the agent image installs **beyond** git, openssl and CA certificates. Empty is legitimate (a machine whose projects need only git); a blank name is refused, because it fails at `apt-get install ""`. The image's `org.holosmith.toolchains` label carries the list it was built with, so a changed list is a rebuild and not a re-tag. |
| `container_cpus` | integer | `2` | Whole CPUs per session container. Docker reads `0` as unlimited, so the floor is 1. |
| `container_memory_mb` | integer | `4096` | MiB of memory per session container. At least 1. |
| `container_pids` | integer | `512` | Process count per session container. At least 1. |
| `container_prefix` | string | `""` | Prepended to every session **container** name, which is otherwise `s_<id>`. Empty, or lowercase letters/digits/`_`/`-`/`.` starting with a letter or digit and **ending in `_`** (so `smoke_` gives `smoke_s_<id>`). This is how a smoke run's names cannot collide with a machine's own. |
| `network_prefix` | string | `""` | The same for session **network** names (`agents_<id>`), with the same alphabet. Two keys because two different sweeps read the two names. |

## What a session may reach

| Key | Type | Default | Meaning |
|---|---|---|---|
| `network_default` | boolean | `false` | Whether a new session's "Internet access" checkbox starts checked. The create request still decides per session. |
| `[egress] allow` | string array | `[]` | The domains the egress proxy lets a session reach, through `CONNECT` on 443. Lowercase bare domains: no scheme, port, path or wildcard; duplicates refused. Empty means a session has **no** Internet. |

A session's network is `--internal`: the only ways out are the key proxy (provider calls, with a per-session dummy
key) and, when Internet access is on, the egress proxy for the allow-list. Provider keys never enter a container
(ADR-0045, ADR-0048).

## When a session stops, and what it costs

| Key | Type | Default | Meaning |
|---|---|---|---|
| `max_live_sessions` | integer | `3` | How many sessions may be live at once (starting, idle or running). At least 1. A create at the cap evicts the least recently used **idle** session, or is refused `409 cap_reached` when nothing is idle (ADR-0024). |
| `idle_stop_min` | integer | `30` | Minutes a session may sit `idle` before the sweeper stops it. **`0` is the off switch** (no sweeper). Minutes are whole, and the sweeper ticks every five seconds. |
| `turn_timeout_min` | integer | `45` | Minutes a turn may run before it is ended and the session returns to `idle`. **`0` is the off switch** (no deadline is built). |
| `abort_grace` | duration | `"10s"` | How long an aborted turn's process gets after its first signal before the abort ladder escalates (ADR-0020). Greater than zero. |
| `max_session_tokens` | integer | unset | A token ceiling across a session's whole life; reaching it stops the session. Unset means unlimited; `0` is refused — unset is how you say "no ceiling" (ADR-0027). |
| `stopped_ttl_days` | integer | `7` | Days a `stopped` session is kept before the expiry sweep deletes it and its worktree. At least 1 (ADR-0070). |
| `min_free_gb` | integer | `20` | Free space, in whole GB, this machine must have on the data root. It is a **preflight** floor — `serve` refuses to boot below it and `doctor` reports it — and the create and the resume answer `507 disk_floor`. `0` disables the floor. |
| `session_disk_soft_gb` | integer | `10` | A per-session soft cap on the worktree's size. Past it the session is stopped with reason `disk_quota`. **`0` is the off switch** (no watchdog). The worktree's size includes `target/` and `node_modules/`, so a large project should raise this rather than rebuild on every resume (ADR-0069). |
| `project_backup` | boolean | `false` | Take one verified backup bundle of the project per day (ADR-0034). |
| `history_window` | integer | `50` | How many messages a client that opens an event stream with no `Last-Event-ID` is replayed, plus a `history.truncated` frame saying where the replay started. At least 1: a client replayed nothing cannot tell an empty session from a cut one. Older history is paged. |

## Git: what a merge and a publish touch

| Key | Type | Default | Meaning |
|---|---|---|---|
| `[merge] targets` | string array | `["main", "master"]` | The branches a session's work may be merged into, written on the host. Each must be a plain branch name (no whitespace or control characters, no `..`, no `@{`, no leading `-`, none of `~^:?*[\`, no empty component, no component beginning with `.` or ending in `.lock`); duplicates refused. Naming a branch a repository does not have is not refused here — it surfaces at the merge. |
| `[publish] remotes` | string array | `[]` | The remotes a session branch may be pushed to. Each is a remote name: letters, digits, `.`, `_`, `-`, not starting with `-`; duplicates refused. Empty **disables** publishing (ADR-0066). |

All git work happens on the host: a session gets a worktree and an `agent/<id>` branch, and the server merges,
diffs, snapshots and publishes. No container owns the repository.

## Several machines

| Key | Type | Default | Meaning |
|---|---|---|---|
| `machine_name` | string | `"local"` | See [above](#where-it-listens-and-what-it-answers-to). On a multi-machine deployment it is the **same string** as the hub's registry row for this machine. |
| `hosts_seed` | path | unset | An optional `hosts.toml` read at every boot and merged into the host registry, the file winning on conflicts, so one file copied to every machine keeps the lists aligned (ADR-0073). Absolute. A missing or malformed file is one `warn` and touches no rows. Unset means no seed is read. |
| `allowed_hubs` | string array | `[]` | Which machines may send this one forwarded calls, by **tailnet node name** (the short name — `hub` out of `hub.tailnet1a2b.ts.net`, not the login). Verified by asking the local daemon about the peer's address, never by trusting a header (ADR-0075). **Empty means "no machine-to-machine check"** — the operator has chosen tailnet ACLs. With a non-empty list, a peer at loopback is always admitted (that is what `tailscale serve` connects from) and any other peer must be named here. |

The registry itself is not in this file: rows are `POST /api/hosts` (`holosmith hub host add <name> <url>`) or the
seed. A row's `url` must be `http(s)` with a MagicDNS name or an address in `100.64.0.0/10`, and a port.

## Releases and upgrades

| Key | Type | Default | Meaning |
|---|---|---|---|
| `keep_releases` | integer | `5` | How many release directories a promotion leaves behind. At least **2**, because a prune always keeps the live release and the one behind it — the rollback target — and those two count towards the number. `0` is not "keep everything" (ADR-0084). |

## Rules that span two keys

Each is reported at the key that broke it, and each mentions when the other value is a built-in default rather than
one the file wrote:

- `preview_listen` must differ from `control_listen`, unless one of them is port `0`.
- `preview_origin` must differ from `control_origin`.
- `data_root` and `projects_root` must be absolute and must not be equal, nested, or one a parent of the other. A `..`
  is folded before they are compared.
- Every list refuses a duplicate.
- `owner` must be non-empty and contain no whitespace (it is a login name).
- Three limits refuse `0` by name, because Docker reads `0` as *unlimited*: `container_cpus`, `container_memory_mb`,
  `container_pids`. The three keys where `0` genuinely means *off* are `idle_stop_min`, `turn_timeout_min` and
  `session_disk_soft_gb`.

## Two worked examples

A single machine that a browser reaches over HTTPS through a TLS terminator you run yourself (Caddy, nginx) — this
is the shape a Headscale tailnet needs, since Headscale issues no node certificates:

```toml
role = "server"
machine_name = "laptop"
control_listen = "127.0.0.1:8080"
preview_listen = "127.0.0.1:8081"
control_origin = "https://holosmith.example.net"
preview_origin = "https://preview.example.net"
allowed_hosts = ["holosmith.example.net", "preview.example.net"]
owner = "adamolsson"
data_root = "/var/lib/holosmith/data"
projects_root = "/var/lib/holosmith/projects"
min_free_gb = 10
session_disk_soft_gb = 5

[egress]
allow = ["crates.io", "index.crates.io", "static.crates.io", "registry.npmjs.org", "github.com"]
```

A pair: an always-on hub and one server behind it. The hub's registry names the server; the server trusts the hub by
node name.

```toml
# hub
role = "hub"
machine_name = "hub"
control_origin = "https://hub.example.net"
preview_origin = "https://hub.example.net:8444"
allowed_hosts = ["hub.example.net"]
owner = "adamolsson"
```

```toml
# server
role = "server"
machine_name = "laptop"
control_origin = "https://laptop.example.net"
preview_origin = "https://laptop.example.net:8444"
allowed_hosts = ["laptop.example.net"]
allowed_hubs = ["hub"]
```

## What a deploy writes, and what it does not

`server deploy` and `hub deploy` render this file from the config **they loaded** and rewrite it when that rendering
differs, so hand edits survive a re-run. A file with a problem in it is a refusal **before** any step runs, never a
silent fallback; only a missing file at the default path means the built-in defaults are used and written. Two files
in the same step are treated differently on purpose: `secrets/keys.toml` is created when absent and never written
over, and `secrets/vapid.toml` is generated once. The deploy never edits either.

The order that works on a fresh machine: run `deploy --dry-run`, run the deploy, edit the config (deploy's own
defaults are `localhost` origins and an empty allow-list), then run the deploy again. Everything else about the
install path is in `docs/build.md`.

## Where the reasons are

The design notes behind these keys are in `docs/adr/`: ADR-0006 (one typed config, defaults < file < CLI), ADR-0010
(the four browser-facing checks), ADR-0044 and ADR-0054 (the preflight), ADR-0073 (the registry and its seed),
ADR-0075 (machine trust), ADR-0084 (the release tree).
