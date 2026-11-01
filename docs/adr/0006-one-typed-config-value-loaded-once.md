# One typed config value, loaded once, strict on unknown keys

The spec says one TOML file holds the machine's settings and that `config validate` "rejects unknown keys". This ADR fixes the rest, because HS-047's `server deploy` will *write* this file and every later story will *read* it: after the file is deployed, renaming a key or moving it into a table is a migration, not an edit.

## The file, and which one is read

The default path is `/etc/holosmith/config.toml` — the only path the spec names, and the right one for a systemd unit running as the `holosmith` service user, which has no `$HOME` worth depending on. XDG is for per-user applications and is rejected for that reason.

A single `--config PATH` argument on the clap root (`global = true`) selects another file, so both `holosmith --config X config show` and `holosmith serve --config X` work and HS-003 needs no second flag. There is no environment variable: one way to name the file is enough, and `$HOLOSMITH_CONFIG` would be a second source of truth that a systemd unit could set invisibly. Precedence is `defaults < file < CLI key overrides`; the overrides (`--port`, `--data-dir`, `--backend`) arrive in HS-003 and are applied to the loaded value, never re-read from the file.

**A missing file is only an error when it was named.** The loader knows whether the path came from `--config` or from the default. Default + missing → the built-in defaults, `ConfigSource::defaults`, exit 0, and `LoadedConfig.path` still reports the path that was tried, so the operator can see what to create. Explicit + missing → exit 1, because `--config /etc/holosmith/confgi.toml` quietly running on defaults is exactly the silent fallback this story exists to prevent. `config edit` is the one exception: it creates the file (with the resolved defaults, comments and all) for either origin, but never creates a directory.

## The layout, settled now

Every key the spec spells flat stays flat — `role`, `turn_timeout_min`, `idle_stop_min`, `max_session_tokens`, `min_free_gb`, `session_disk_soft_gb`, `stopped_ttl_days`, `abort_grace`, `network_default`, `project_backup`, `allowed_hubs`, `hosts_seed`, `docker_host`, plus the keys the spec never named (`control_listen`, `preview_listen`, `control_origin`, `preview_origin`, `allowed_hosts`, `owner`, `data_root`, `projects_root`, `max_live_sessions`). Only the two keys the spec itself spells with a dot become tables: `[merge] targets` and `[publish] remotes`, and the egress allow-list joins them as `[egress] allow`. Grouping the amendment keys into `[limits]` or `[disk]` was rejected: `limits.turn_timeout_min` would make HS-021, HS-022, HS-023, HS-064 and the spec speak a second name for one thing.

A section has two spellings in TOML — `[merge]` on its own line, or `merge = { … }` — and the validator reads both, with positions, because an operator hand-editing under `config edit` will eventually write the second and rejecting valid TOML with "expected a table, found table" is not an answer. What `config show` writes, and what HS-047 deploys, is the header form: the layout is fixed by the writer, not by the reader.

## Two roots, deliberately

`data_root` (default `/var/lib/holosmith`) holds the database, session directories, backups and the secrets file. `projects_root` (default `/srv/holosmith/projects`) holds the git repositories sessions are created against, and it must not equal or nest inside the data root in either direction. The point is that `uninstall --purge` and every delete flow can be incapable of touching a repository. Consequence: `server deploy` (HS-047) must `mkdir -p` both roots.

Both paths are compared after `.` and `..` are folded lexically, so `/srv/../var/lib/holosmith/projects` is caught as nested and `/var/lib/holosmith/../projects` is not falsely rejected. A symlink is not resolved — that needs the filesystem, and a config file is validated whether or not the directories it names exist yet — so a symlinked root is the operator's responsibility, and no path a config file can spell puts a repository inside the data root.

## Validation collects every problem

`config validate` has to reject unknown keys *and* "report every problem at once". Measured on this machine: `toml` 1.1.8 produces an excellent span error but exactly one of them, even with two unknown keys in the document, because `deny_unknown_fields` aborts on the first. So validation is a three-pass design over `toml_edit` (the format-preserving parser from the same project, used *instead of* `toml`, not in addition to it, because `Document::parse` keeps a byte span for every key at every level):

1. A **schema walk** over the parsed document, driven by a `&[KeySpec]` table of `(path, kind, section)`, which collects *every* unknown key and *every* kind mismatch with its own line and column, sorted by position.
2. **Typed extraction**, tolerant by construction: every key the walk accepted is converted into `Config`, every key the walk rejected falls back to its built-in default. That is what lets the semantic pass still run — and still report — on a file that also has unknown or mistyped keys, which is the acceptance's "every problem at once". Then, and only when the walk is clean, the same document is deserialized with `#[serde(deny_unknown_fields)]` as a consistency belt: its failure is appended to the problem list and never swallowed. The belt is not the reporter (the walk is), and what it can catch is narrow: a `KeySpec` row whose *kind* is more permissive than the field it feeds (a `Kind::String` row over an integer field). It is a type check, not a value check — `Origin` is `#[serde(transparent)]`, so serde alone would accept `control_origin = "ftp://x"`, which `Origin::parse` refuses. A setter wired to the wrong field is caught by `every_documented_key_is_read_into_its_field`, which sets all 25 keys to distinct values and asserts each field separately.
3. A **semantic pass** over the typed value for the rules a type cannot express (`max_live_sessions >= 1`, the two roots must not nest, the two origins must differ, `abort_grace` must be a unit-suffixed duration greater than zero, …), which also collects all of its violations. Semantic problems carry a span too, looked up from the document by key path. A rule that compares two keys also knows which of them the document set, so when it compares a built-in default it says so: two listeners on one port are still a problem, but the message must not read as if the file wrote both values.

The honest limit: "every problem at once" means every **semantic** problem — unknown key, wrong type, impossible value. A document that does not parse has no key tree to walk, so a syntax error or a duplicate key is reported alone, with its span.

Two guards keep this from drifting into a second schema. The struct↔table agreement test walks `serde_json::to_value(Config::default())` and compares paths *and* kinds with the `KeySpec` table in both directions, so a field added without a table row fails and a table row without a field fails. And `config show` is rendered from that same table, so the renderer cannot grow keys the validator does not know.

## Unset is a comment, never an omission

TOML has no null, and `toml_edit::ser` silently *drops* a `None` field — measured. Dropping `max_session_tokens` from `config show` would be the silent fallback this story forbids, and HS-023 reads that key as "absent means unlimited", so its absence must be visible. Unset optionals (`max_session_tokens`, `docker_host`, `hosts_seed`, `owner`) are therefore printed as a comment line, `# max_session_tokens unset`, which keeps the output valid TOML: the body re-validates clean and round-trips, and that round-trip is a test rather than a claim.

**Writing that output back is not `config show > config.toml`.** The shell truncates the target before the binary runs, so redirecting onto the file being read destroys a broken file and leaves the defaults behind, which is exactly the silent fallback this story exists to prevent. Write beside it and move it into place: `config show > config.toml.tmp && mv config.toml.tmp config.toml`.

## One typed value, and no second parser

`Config` is built once, at startup, by `Config::load()`, and is passed by reference to everything that needs a setting. No later story parses TOML: HS-003 binds `control_listen`, HS-004 takes `data_root`, HS-005 reads `allowed_hosts`, `control_origin` and `owner`, HS-020 reads `max_live_sessions`, HS-021/022/023 read the runaway keys, HS-039 gets its key file from `Config::secrets_path()` (it is never read here), HS-045/HS-047 re-validate with the same code. A test fails if `toml_edit` is named anywhere under `src/` outside `src/config/`.

## Adding a key later

Six edits, in this order: a field with its default on `Config` (or its section struct), a `KeySpec` row with path, kind and section, a semantic rule in the semantic pass if the value can be impossible, a test for that rule, the count in `the_table_holds_the_documented_keys`, and the key's line in the defaults golden (`tests/golden/config-default.toml`). The golden is the second, independent copy of the key set — `config show` is rendered from the table and compared with it byte for byte — so a hand-kept array of key names in the integration tests would be a third opinion that can only say "you forgot to edit me too", and there is none. This is why HS-002 does not pre-declare keys whose defaults would be guesses (container limits, history window, image toolchains): each belongs to the story that knows its semantics.

## Consequences

- HS-003's `serve` calls `Config::load()` and logs `LoadedConfig::describe()` — `<path>`, or `none at <path>, using defaults`. A missing config file at the default path is not an error; `serve` starts on defaults. HS-002 proves the loader and the reported path; the log line is HS-003's.
- `config show` prints the resolved config as TOML, preceded by `# config_path: <path>` and `# config_source: <defaults|file>`. Both header values are escaped, so a `--config` path with a newline in it cannot end the comment early; the body is rendered whole before anything is printed, so stdout never holds half a document. A path that is not UTF-8 — which only a CLI override (HS-003's `--data-dir`) can produce, since TOML is UTF-8 — is a tool error and exit 1, not a panic. There is no `--json`: the spec asks for neither.
- `server deploy` must create both roots, and must write this file with these key spellings.
- `[egress] allow` ships empty, as decided here. `server deploy` (HS-047) must **seed** it when it writes the file: item 2 requires that the proxy reaches registries when the checkbox is on, and an empty allow-list turns every session's package install into a 403 that looks like a bug.
- `docs/stories/README.md`'s ADR index needs a bullet for this decision; `docs/stories/` is owned by the orchestrator, not by the story that lands the code.
- A broken file is loud: `serve` refuses to start rather than falling back to defaults, which is the whole point of rejecting unknown keys.
