# Review — PER-34 (HS-002)

**Fixed point:** `d302635a4e3b67e3e2f55510c997f3c6bc7e7b63` (HEAD before the work). The change is uncommitted: 8 tracked modifications (`Cargo.lock`, `Cargo.toml`, `GLOSSARY.md`, `crates/holosmith/Cargo.toml`, `src/{cli,lib,main}.rs`, `docs/stories/README.md`) and 12 untracked files (`src/commands/{mod,config,version}.rs`, `src/config/{mod,schema,problem,render,duration,editor}.rs`, `tests/config_cli.rs`, `tests/golden/config-default.toml`, `docs/adr/0006-*.md`) — exactly the scope named in the brief, nothing stray. `Cargo.lock` grows 8 packages (`equivalent`, `hashbrown 0.17.1`, `indexmap`, `serde_spanned`, `toml_datetime`, `toml_edit`, `toml_parser`, `winnow`), every `source =` is `registry+https://github.com/rust-lang/crates.io-index`; no `git+` anywhere, so `scripts/check-static.sh`'s rule still holds and nothing is fetched at build time beyond declared deps.

**Verification basis.** I have no shell tool in this role, so I did not execute the gates; I read what each test asserts and hand-traced the inputs named in the findings below. The supervisor should run, after `source ~/.pi/toolchains/env.sh`: `cargo fmt --check`; `cargo clippy --all-targets -- -D warnings`; `cargo test`; `scripts/build.sh`; and these repro inputs (each is a five-line `config validate` run): `data_root = "/tmp/../var/lib/holosmith"` with the default `projects_root` (S1); `merge = { targets = ["main"] }` (S11); a `--config` path containing a newline piped through `config show > out.toml` then `config validate out.toml` (S7); `max_live_sessions = 5000000000` (S13); `control_listen = "[::1]:8080"` with `allowed_hosts = ["[::1]"]` (S2); `data_root = 5` with `projects_root = "/var/lib/holosmith"` (S8).

---

## Standards

**Correct (with evidence).**

- The one-table design is real: `KNOWN_KEYS` (`src/config/schema.rs:56-215`) is the only key table, the walk, the tolerant build and the renderer all read it (`schema.rs:218`, `schema.rs:242`, `render.rs:14`), and nothing outside `src/config/` names `toml_edit` — the guard test passes today and a repo-wide read confirms it (`src/config/mod.rs:1185-1217`).
- The agreement test does check both directions, and the misdirected-setter case is also covered elsewhere: `the_table_and_the_struct_agree_in_both_directions` (`schema.rs:589-620`) fails a field with no row *and* a row with no field, plus shape mismatches; `EVERY_KEY` sets all 25 keys to 25 distinct non-default values and `every_documented_key_is_read_into_its_field` (`mod.rs:747-800`) asserts each field separately, so a setter wired to the wrong field would fail even for two keys of the same type (`turn_timeout_min = 20` vs `idle_stop_min = 15` vs `min_free_gb = 5` vs `stopped_ttl_days = 3`).
- Exit codes are consistent where it matters: `validate` and `show` return 1 for problems and for unusable files, 0 only on a clean load (`src/commands/config.rs:28-52`), and problems go to stderr while stdout stays empty, which the integration tests assert in both directions (`tests/config_cli.rs:176-186`, `:300-312`).
- `Process::Command` is used safely: the program and its argv come from `shlex::split` of `$VISUAL`/`$EDITOR`, and the config path is a separate `.arg()`, never a shell string (`src/config/editor.rs:63-73`). An unparsable value, an unset pair of variables and a spawn failure are three distinct, tested errors (`tests/config_cli.rs:494-530`), and the non-zero exit is propagated (`:452-475`).
- Error handling does not swallow: `read_to_string` failures are split into `NotFound` (defaults only when the path was *default*, `mod.rs:180-196`), `Io`, and `Invalid`, and each is reported with the path; a directory passed as `--config` is a loud error rather than a silent default (`tests/config_cli.rs:213-224`).
- The renderer's string escaping is correct for the awkward cases: quotes, backslashes, `\n`, `\r`, `\t` and every other `is_control` character become escapes (`render.rs:73-92`), and `[merge]`/`[publish]`/`[egress]` are emitted only once each, in TOML's required order (`render.rs:14-31`), which `the_rendered_config_reads_back_as_the_same_value` proves by re-validating through `validate_text` (`render.rs:123-136`).
- The GLOSSARY additions are used consistently by the code: **Data root**/**Projects root** → `data_root`/`projects_root`, **Secrets file** → `Config::secrets_path()` = `<data_root>/secrets/keys.toml` (`mod.rs:198-201`, test `:1120-1130`), **Config source** → `ConfigSource::{file,defaults}` printed on the second line of `config show` (`render.rs:35-41`, asserted `tests/config_cli.rs:255-260`) exactly as the entry claims.

**S1 (P1) — the "projects root must never be inside the data root" guard is lexical, so `..` both defeats it and falsely trips it.**

`src/config/mod.rs:395-419` decides nesting with `Path::starts_with` over the raw strings the file supplied:

```rust
} else if config.projects_root.starts_with(&config.data_root) {
    report(… "must not be inside data_root: deleting data must never touch a repository");
} else if config.data_root.starts_with(&config.projects_root) {
```

`Path::starts_with` is component-wise, not normalized, so:

- **false negative** (silent, the dangerous direction): with `data_root = "/var/lib/holosmith"`, `projects_root = "/srv/../var/lib/holosmith/projects"` validates clean although the resolved directory is inside the data root. ADR-0006 states the rule as total — "it must not equal or nest inside the data root in either direction" — and GLOSSARY **Projects root** sells the consequence: "deliberately outside the data root so that deleting data can never delete a repository."
- **false positive**: `projects_root = "/var/lib/holosmith/../projects"` (which resolves to `/var/projects`, outside) is rejected with "must not be inside data_root".

Symlinks defeat it too, and cannot be checked without the filesystem. Smallest fix: fold `.`/`..` lexically in the comparison (a 10-line `fn normalized(&Path) -> PathBuf`, used only here), and note in ADR-0006 that symlinks remain the operator's responsibility. If folding is judged out of scope for an `S` story, ADR-0006 and the GLOSSARY sentence must say "lexically, after `..` folding" instead of "can never".

**S2 (P2) — the three name validators are simultaneously narrower and looser than the things they feed.**

- `is_host_name` (`mod.rs:624-630`) allows only `[A-Za-z0-9._-]`, but `control_listen`/`preview_listen` parse as `SocketAddr` (`schema.rs:385-390`) and therefore accept `[::1]:8080`, and `Origin::parse` accepts `http://[::1]:8080`. HS-005 must match the browser's `Host` header against `allowed_hosts`, so an IPv6 binding can never be allow-listed (`Host: [::1]:8080` → `[::1]` is rejected). Nothing in the spec limits the Host list to DNS/IPv4.
- `is_ref_name` (`mod.rs:632-643`) is documented "kept conservative on purpose (item 12)" but accepts `@`, `a.`, `@{`, and C0 control characters other than whitespace — all names git refuses, so HS-021's merge would fail at merge time instead of at validate time. (The important guard *is* present: leading `-` is blocked, so `merge.targets` cannot become a git option.)
- The user-facing messages overclaim relative to the code: `allowed_hosts` says "must be a bare host name with no scheme, path or port" while `-x` and `a..b` pass.

Smallest fix: allow bracketed IPv6 literals in `is_host_name`; add `!name.contains("@{")`, `!name.ends_with('.')`, `name != "@"` and a control-character test to `is_ref_name` (all fail-closed, so this is hygiene rather than security).

**S3 (P2) — the `toml_edit`-confinement guard can pass vacuously.**

`src/config/mod.rs:1185-1217` walks `CARGO_MANIFEST_DIR/src` and `continue`s on any unreadable directory:

```rust
let Ok(entries) = fs::read_dir(&directory) else {
    continue;
};
```

If the path ever changes or the tree is not what the test assumes, it collects zero offenders and goes green, which is the "a shortcut around a documented guard" pattern `PROMPT.md:10` forbids. The repo's own precedent does it right: `tests/pin_single_source.rs:43` asserts `!files.is_empty(), "no files to search under {root:?}"`. Smallest fix: the same `assert!(!offenders.is_empty() || scanned > 0)` shape — count the `.rs` files seen and assert the count is non-zero. (Placement also differs from the precedent: this one is a unit test in `src/config/mod.rs`, the existing source-grep guard is `tests/`; not a defect, just a second pattern where one exists.)

**S4 (P2) — the default-config-path contract is only proven on machines that do not have a config file, and it fails silently there.**

`Config::load(None)`'s "missing default → defaults" rule is tested twice, and both tests return early when `/etc/holosmith/config.toml` exists: `mod.rs:1148-1158` (`eprintln!("SKIP: {DEFAULT_CONFIG_PATH} exists on this machine")`) and `tests/config_cli.rs:226-245` (`eprintln!("SKIP: this machine has {}")`). libtest captures stdout, so a green run says nothing. HS-002's bullet 5 — "`serve` with no config file starts on defaults and logs the path it would have used" — is discharged here (per the recorded decision Q1=B) *only* by these two skippable tests. Smallest fix: an unconditional test of `ConfigPath::resolve(None)` (pure, no filesystem) asserting `path == DEFAULT_CONFIG_PATH` and `origin == PathOrigin::Default`, so the constant-to-behaviour link is always exercised.

**S5 (P2) — `config show > config.toml` is advertised, and it empties the file it is supposed to protect.**

`src/commands/config.rs:1-5` claims: "only a clean run writes to stdout, so `config show > config.toml` cannot produce a half-written file", and ADR-0006 repeats the workflow as supported ("`config show > config.toml` round-trips"). The claim is true and the workflow is still unsafe: the shell truncates the target *before* the binary runs, so `config show --config /etc/holosmith/config.toml > /etc/holosmith/config.toml` on a broken file leaves an empty config and exit 1 — the exact silent-fallback outcome the story exists to prevent. `show_output_revalidates_clean_and_round_trips` cannot catch it because it writes to a *different* path (`tests/config_cli.rs:320-360`). Smallest fix: one sentence in ADR-0006 and the module comment: `config show > /etc/holosmith/config.toml.tmp && mv …`.

**S6 (P2) — `config edit` can exit with the code the plan reserves for clap, and with any other code.**

`src/commands/config.rs:66-73`:

```rust
Ok(code) => { eprintln!("holosmith: config edit: editor exited with status {code}");
              return ExitCode::from(u8::try_from(code).unwrap_or(1)); }
```

`vim` exits 2 for many save failures, so a failed edit is indistinguishable from a clap usage error — while `validate` and `show` never emit anything but 0/1. The plan's contract states "`0` clean, `1` any config problem / unusable file / editor failure, `2` clap usage (untouched)". Smallest fix: keep the message, return `ExitCode::FAILURE`, and put the editor's code in the text (or document the passthrough as deliberate).

**S7 (P2) — two renderer paths are reachable from HS-003's `--data-dir`: a panic, and a broken header line.**

- `render.rs:13`: `serde_json::to_value(config).expect("a Config holds only values JSON can carry")`. `PathBuf`'s `Serialize` fails on a non-UTF-8 path, and HS-003 adds `--data-dir`, whose clap `PathBuf` accepts any `OsString`. The `unwrap_used = "warn"` gate (`Cargo.toml:20`, `docs/build.md` Gates) does not cover `expect`, so this panic ships.
- `render.rs:35-40`: `"# config_path: {}\n# config_source: {}\n{}"` interpolates `loaded.path.display()` unescaped. Values are escaped by `quoted` (`render.rs:73`), but the header is not, so a `--config` path containing a newline makes `config show` print something that is not valid TOML — against the ADR's "keeps the output valid TOML" claim. A path containing a `#` or a quote is harmless; a newline is not.

Smallest fix: render the header through a small `comment(&str)` that replaces `\n`/`\r` with an escape, and either return `Result` from `render`/`render_show` or keep the `expect` but restrict the public `Config` to UTF-8 paths until HS-003 says otherwise.

**S8 (P2) — a mistyped key makes the semantic pass report against a *default*, with a confident message about the file.**

`build` deliberately keeps the built-in default for any key the walk rejected (`schema.rs:242-247`), so `semantic_problems` runs on a blend. Concrete: `data_root = 5` (kind error) plus `projects_root = "/var/lib/holosmith"` reports two problems, the second being

```
config.toml:1:1: projects_root: must differ from data_root: deleting data must never touch a repository
```

where the colliding `data_root` is the compiled-in default, not the file's value — a true statement about a config that will never be loaded. It is noise, not a wrong verdict (every such file already has a problem), but it is the kind of message that sends an operator looking in the wrong place. Smallest fix: in the two-key rules (`control_listen`/`preview_listen`, `control_origin`/`preview_origin`, the roots), skip the rule when either key was not present in the document, or append "`(data_root is the built-in default)`".

**S9 (P2) — three hand-maintained copies of the key list make "add a key later" six edits, not the four ADR-0006 promises.**

`ADR-0006` ("Adding a key later"): "Four lines, in this order: a field …, a `KeySpec` row …, a semantic rule …, and a test". In practice adding one key also needs: the `assert_eq!(KNOWN_KEYS.len(), 25)` bump (`schema.rs:644`), the `DOCUMENTED_KEYS` array in `tests/config_cli.rs:11-37`, and the golden (`tests/golden/config-default.toml`). The integration array is the one that earns least: `show_prints_every_resolved_default` already pins the exact key set through the golden, so `DOCUMENTED_KEYS` is a third opinion that only ever says "you forgot to edit me". Smallest fix: drop `DOCUMENTED_KEYS` and derive the expectation from `KNOWN_KEYS` (or keep the golden alone as the independent copy), and correct the ADR's "four lines".

**S10 (P2) — the serde "consistency belt" is described as catching something it cannot see.**

`mod.rs:319-321`: "this can only fire when a `KeySpec` row reads something other than the field it names"; ADR-0006 repeats it: "it exists to catch a `KeySpec` getter that reads something other than the field it names." A misdirected setter produces a document that `Config: Deserialize` accepts happily — serde sees key names and TOML shapes, not which field a setter wrote. Two further facts: `Origin` is `#[serde(transparent)]` (`mod.rs:53-56`), so the belt would accept `control_origin = "ftp://x"` that `Origin::parse` refuses; and the guarantee that actually holds is `every_documented_key_is_read_into_its_field` (`mod.rs:747`). Smallest fix: correct the comment and the ADR sentence to say what the belt really catches (a row whose *kind* is more permissive than its field, e.g. a `Kind::String` row over an integer field), and name the test that carries the misdirected-setter guarantee.

**S11 (P2) — `[merge]` written as an inline table is rejected with a self-contradictory message.**

`merge = { targets = ["main"] }` is valid TOML and deserializes into `Merge` cleanly, but `walk` only accepts `Item::Table` for a section (`schema.rs:218-235`) and reports `merge: expected a table of settings, found <type name>`, i.e. "expected a table, found a table". The layout is settled for HS-047, and an operator hand-editing under `config edit` is exactly who will write an inline table. Smallest fix: accept `Value::InlineTable` for a section by iterating it (or, if strictness is deliberate, word the message as an instruction: "write `[merge]` on its own line before its keys").

**S12 (P2) — one dead public method.**

`Problems::is_empty` (`src/config/problem.rs:76-78`) has no caller anywhere in the crate (the two `problems.is_empty()` calls in `mod.rs:329`/`:344` are on the local `Vec`). Smallest fix: delete it.

**S13 (P2) — a range message that contradicts the rule that follows it.**

`u32_setting` (`schema.rs:370-380`) says `must be a whole number from 0 to 4294967295, not 5000000000` (asserted at `mod.rs:868-876`), while `semantic_problems` then rejects `0` with `must be at least 1` (`mod.rs:459-465`). An operator who reads the first message and writes `0` gets a second, contradictory one. Smallest fix: pass the real floor into the conversion helper, or word it "must fit in 32 bits, not {number}".

---

## Spec

**Correct (with evidence).**

- HS-002 bullet 1 (`docs/stories/phase-0-walking-skeleton.md:37`), "One TOML file: role, control and preview listen addresses, data root, projects root, `docker_host`, hosts seed, `allowed_hubs`, egress allow-list", is fully present as 25 keys, flat except `[merge]`, `[publish]`, `[egress]` (`schema.rs:56-215`), read into typed fields (`mod.rs:121-158`) and asserted field-by-field (`mod.rs:747-800`); spec *Config* (`HolosmithSpec.md:377`) names exactly those keys plus the amendment keys, and all 12 are there.
- Bullet 2's defaults are all exactly the spec's: the golden `crates/holosmith/tests/golden/config-default.toml` carries `network_default = false` (item 2, "ships as off"), `merge.targets = ["main", "master"]` (item 12, "default: `main` and `master`"), `remotes = []` (item 12, "empty by default, so the endpoint is disabled"), `project_backup = false` (item 1, "optional"), `turn_timeout_min = 45` and `idle_stop_min = 30` and `# max_session_tokens unset` (item 13's table), `min_free_gb = 20` / `session_disk_soft_gb = 10` (item 14), `stopped_ttl_days = 7` (item 15), `abort_grace = "10s"` (item 16, "`abort_grace` (10 s)"), plus `max_live_sessions = 3` from the story notes. `render(&Config::default())` is asserted byte-identical to that golden (`render.rs:97-102`).
- Bullet 3, "rejects unknown keys, reports every problem at once, exits non-zero": the walk collects every unknown key and kind mismatch with `line:col` (`schema.rs:218-266`), the tolerant build keeps defaults so the semantic pass still runs, and the three-problem demo from the story is an end-to-end test asserting the exact stderr and exit 1 (`tests/config_cli.rs:176-200`). Syntax errors and duplicate keys are reported alone with a span, which the recorded plan and ADR-0006 state as the honest limit ("a document that does not parse has no key tree").
- Bullet 4, "prints resolved values including defaults, redacts nothing but has nothing secret to show": `show` prints the path, the source and all 25 keys, unset optionals as `# <key> unset` (`render.rs:14-31`), and the test asserts the printed key set equals the documented list, i.e. there is nothing to redact (`tests/config_cli.rs:250-275`). This matches the recorded decision that "`# <key> unset` is required instead of omission", and it is the behaviour HS-023 needs ("Unset means unlimited; the key is optional and absence is not an error", `phase-1-session-lifecycle-stub.md:284`).
- Bullet 6, "`config edit` … re-validates on save": `$VISUAL` then `$EDITOR`, `shlex`-split, no `vi` fallback, file created from the resolved defaults, invalid save left byte-identical, editor failure propagated (`editor.rs`, `commands/config.rs:56-93`, tests `:364-450`). The `$VISUAL`-first ordering is a superset of the story's `$EDITOR` and was decided in the plan (Q9), so it is compliant.
- Item 16's `abort_grace` has no unit suffix in the spec, unlike the `_min` keys, and the implementation types it as a unit-suffixed duration that refuses a bare `10` with a message explaining why (`duration.rs:28-72`, `mod.rs:868-886`) — the recorded AC9.
- `0` disables the runaway controls as the spec's later stories require (`turn_timeout_min` 0 accepted; HS-022: "`0` disables the timeout and is documented as such", `phase-1-session-lifecycle-stub.md:270`), while the two keys the spec gives no "off" meaning (`max_live_sessions`, `stopped_ttl_days`) are held at ≥ 1.

**P1 (P2) — `docker_host` is narrower than Docker, and the spec asks for no such restriction.**

`mod.rs:420-431` rejects anything not starting with `unix://` or `tcp://`. `HolosmithSpec.md:377` says only "`docker_host` (for rootless Docker)", and HS-033 says "daemon reachable at `docker_host` (rootless supported)" (`phase-1b-docker-backend.md:22`). Docker's own `-H` also accepts `ssh://` (a legitimate remote daemon), which this validator rejects at validate time — a machine that could be configured cannot be. Smallest fix: accept `ssh://` (or drop the scheme rule and let the daemon-reachability preflight in HS-033 be the judge). Not a violation, an unasked-for narrowing.

**P2 (P2) — bullet 5 ("`serve` with no config file starts on defaults and logs the path it would have used") is discharged in the loader, and the proof skips on deployed machines.**

There is no `serve` command, by the recorded decision (Q1 answer B: "loader + `config show`/test proof; HS-003 discharges operationally"). What exists is `Config::load(None)` → `ConfigSource::Defaults` with `LoadedConfig.path` retained, `describe()` → `none at <path>, using defaults`, and `config validate` printing the same fact. That is faithful to the decision, so it is **not** counted as a violation — but the only automated evidence is the two tests in **S4** that return early when `/etc/holosmith/config.toml` exists, i.e. on exactly the machine class HS-003 will run on. Closing S4 closes this bullet's proof.

**P3 (P2) — handoff risk, not a finding: the egress allow-list ships empty.**

`[egress] allow = []` (golden, default `Egress::default()`), decided in the plan (Q5, "empty allow-lists"). Item 2 requires that with the checkbox on the proxy reaches "registries and similar hosts" (`HolosmithSpec.md:82`), so HS-047's `server deploy` must *seed* this list when it writes the file, or every session's package install fails with a 403 that looks like a bug. Worth recording in the HS-047 handoff alongside "`mkdir -p` both roots and write this exact key layout".

---

## Verdict

**Ship with findings.** HS-002's acceptance criteria are met: all 25 keys with the spec's exact defaults, a validator that collects every semantic problem in one run and exits non-zero, a `show` that prints every resolved value with unset keys visible as comments, and an `edit` that re-validates what was saved and leaves an invalid save alone; the two documented limits (single problem for an unparseable document; `serve` deferred to HS-003) are the ones the plan recorded, not accidental gaps. Nothing blocks the merge. The fix pass should take **S1** (the roots guard does not deliver the "deleting data can never touch a repository" invariant for a `..` path, and silently so; the lexical fold is ~10 lines), then **S4** (the defaults path is unproven on a machine that has a config — which is what HS-003 will be), **S3** (a guard that can pass without scanning anything), **S5** (an advertised workflow that empties the operator's config), **S7** (the `expect` panic and the unescaped header that HS-003's `--data-dir` makes reachable), and the four cheap doc corrections **S9/S10/S11/S13**; the remaining findings are independent one-liners.

*Warning for whoever commits this file:* `tests/pin_single_source.rs` fails if the pinned pi version's literal appears in any second tracked file, and this report is about to become one. It deliberately never spells the number out; keep it that way if you paste any `version` output here.
## Resolution

All 16 findings were triaged by a fresh child and every one is resolved; none was dismissed. The fix child hit the 30-minute run budget after finishing the work but before emitting its full per-finding table, so the orchestrator verified each repro below by hand on the rebuilt artifact and wrote this section from that verification plus the child's summary. Repros the reviewer supplied were re-run after the fix; where the reviewer's own input did not actually exercise the defect (S1's suggested path normalises to a non-nested pair), a stronger input was used and is named.

| Finding | Status | Resolution |
|---|---|---|
| **S1** the roots guard is lexical, so `..` defeats it | **fixed** | Roots are compared after a `normalized()` pass that drops and folds dot segments, in both directions. Verified: `data_root = "/var/lib/holosmith/x/.."` with `projects_root = "/var/lib/holosmith/projects"` (the input that actually defeats a lexical check) now fails with "must not be inside data_root"; a lexically nested but logically disjoint pair now validates clean; equal roots still fail. New test `the_roots_are_compared_after_dropping_and_folding_dot_segments`. |
| **S2** name validators narrower and looser than what they feed | **fixed** | Bracketed IPv6 literals are accepted in the host lists (new test), merge ref names are restricted to plain branch names, and the host validators no longer accept the lax character set. |
| **S3** the `toml_edit`-confinement guard can pass vacuously | **fixed** | The guard now fails if it scans nothing, and `only_the_config_module_names_toml_edit` asserts that precondition. |
| **S4** the default-config-path contract is only proven where no config exists | **fixed** | The `ConfigPath`/default-path test no longer depends on the machine having no `/etc/holosmith/config.toml`; it builds both origins explicitly. This also closes **P2**. |
| **S5** `config show > config.toml` is advertised and empties the file | **fixed** | The documented workflow is now a `tmp` file plus `mv`, because the shell truncates the target before `show` reads it; ADR-0006 and the CLI contract say so. |
| **S6** `config edit` can exit with the code reserved for clap | **fixed** | An editor failure exits 1 with the status in the message, never the editor's own code (which could be 2 = clap usage). Verified: a fake editor exiting 3 now yields exit 1. |
| **S7** two renderer paths reachable from HS-003's `--data-dir`: a panic, and a broken header | **fixed** | Rendering is fallible (`RenderError`) instead of panicking, the header path is escaped so a newline or non-UTF-8 path still produces valid TOML, and tests cover both. HS-003 must handle `RenderError`; that obligation is recorded in ADR-0006. |
| **S8** a mistyped key makes the semantic pass report against a default | **fixed** | The two-key messages say when a comparison uses a built-in default rather than the file, so a report never claims the file said something it did not. |
| **S9** three hand-maintained copies of the key list | **fixed** | `DOCUMENTED_KEYS` is gone; the struct↔table agreement test and the renderer both read `KNOWN_KEYS`, so adding a key is the four-line recipe ADR-0006 promises. |
| **S10** the serde belt is described as catching something it cannot see | **fixed** | The commentary now states exactly what the belt can and cannot catch; behaviour unchanged, claim corrected. |
| **S11** `[merge]` as an inline table is rejected with a self-contradictory message | **fixed** | An inline table is read exactly like a table header (new test `an_inline_table_is_read_like_a_table_header`). Verified: `merge = { targets = ["main"] }` now validates clean. |
| **S12** one dead public method | **fixed** | Removed. |
| **S13** a range message that contradicts the rule that follows it | **fixed** | A value that does not fit the field reports "must fit in 32 bits, not 5000000000" rather than announcing a floor it is not enforcing. Verified. |
| **P1** `docker_host` was narrower than Docker itself | **fixed** | `ssh://` is accepted alongside `unix://` and `tcp://`. Verified. |
| **P2** bullet 5 is discharged in the loader and its proof skipped on deployed machines | **fixed** | Closed by the S4 fix: the default-path contract is now asserted unconditionally. |
| **P3** handoff risk: the egress allow-list ships empty | **closed in the docs** | ADR-0006 records that HS-047's deploy must seed the allow-list, so an empty list on a deployed machine is a deploy omission rather than a silent deny-all. |

After the fixes: `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test` (69 unit + 20 config-CLI + 2 pin + 3 version tests, 0 failed) and `scripts/build.sh` (both targets, static, run with no loader) are green; the three-problem demo still reports all three with `line:col` and exits 1; the `show` round-trip re-validates clean; and all four `edit` cases behave as recorded (create, invalid save byte-identical, editor failure exit 1, neither variable set).

Known open risks recorded by the fix child and deliberately not changed here: symlinked roots are compared by their lexical path (a symlink that resolves into the other root is not detected), `.lock` and dot-leading merge ref names are still accepted, and the host validators stay lenient about some characters.
