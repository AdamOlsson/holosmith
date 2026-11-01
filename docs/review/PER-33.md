# Review — PER-33 (HS-001)

Fixed point: `a30e03f748f3136bf61a4895f47124b9f0738e48` (HEAD before the work). The change is uncommitted: one tracked modification (`GLOSSARY.md`, +16 lines, no deletions) and 19 untracked files, exactly the 19 named in the brief — no stray edits to `AGENTS.md`, `PROMPT.md`, `bwrap.sh`, `scripts/import-linear.py` or `docs/stories/*`.

**Verification basis.** I had no shell tool in this role, so I did not execute `cargo fmt/clippy/test`, `scripts/build.sh` or `scripts/check-static.sh`; I read what they assert instead. The supervisor should run, on a clean tree after `source ~/.pi/toolchains/env.sh`: `cargo fmt --check`; `cargo clippy --all-targets -- -D warnings`; `cargo test`; `scripts/build.sh`; `scripts/check-static.sh` (a second run of this one to confirm idempotency).

---

## Standards

**Correct (with evidence).**

- Both scripts open with `set -euo pipefail` and every expansion is quoted (`scripts/build.sh:6`, `scripts/check-static.sh:9`); bad arguments exit 2 with a message; `--help` exits 0. They are safe to run twice: `build.sh` uses `mkdir -p` + overwrite `cp`, `check-static.sh` only reads `dist/` (`[ -x "$bin_abs" ] || fail "missing $bin: run scripts/build.sh"`).
- No `.unwrap()`, `panic!`, `todo!` in the crate; the only fallible render path is handled (`src/main.rs:19-22` returns `ExitCode::FAILURE` with a message on stderr). `expect` appears only in tests and for cargo-supplied env (which cannot be absent).
- Naming follows the glossary's new terms: `VersionInfo { version, git_tag, pi_version, schema_version, target, sqlite }` (`src/version.rs:11-18`) matches **Version**, **Git tag**, **Schema version** one-for-one; struct field order = JSON key order = the spec's command description order.
- `Cargo.lock` shape is clean: every `source =` line is `registry+https://github.com/rust-lang/crates.io-index` (53 entries) and the only local package is `holosmith 0.1.0`; no `git+` source anywhere.
- The exactly-one-place guard is genuinely green today: a repo-wide search for the pin literal matches only `pins/pi.txt`, and `docs/build.md`, both ADRs and the four glossary terms all deliberately avoid echoing the number.

**S1 (P1) — the repo's gate policy has no runnable, documented path.**

`Cargo.toml:19-20` declares `unwrap_used = "warn"`. A warning cannot fail a build, and nothing in the repo tells you to promote it:

- `docs/build.md` (65 lines) documents the build and `check-static.sh` and never mentions `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings` or `cargo test`. `AGENTS.md` contains only the Linear-scope section.
- The repo's own documented gate is `scripts/verify.sh`: `docs/stories/README.md:29` ("**Definition of done:** … `scripts/verify.sh` extended with at least one case and green"), `PROMPT.md:14` (`main is never left red … only after the repository's build, test and scripts/verify.sh gates pass`), `PROMPT.md:104`, and `PROMPT.md:31` (`Write down the gates for this repository: build, unit/integration tests, scripts/verify.sh, and any linter the repo defines`). That file does not exist until HS-006 (`docs/stories/phase-0-walking-skeleton.md:107-112`).

So the three gate commands exist only in a plan artifact under `/tmp`, which is not in the repo. Why it matters for the next 79 stories: `unsafe_code = "deny"` is compiler-enforced, but `unwrap_used` is not, and there is no CI until phase 2 — the first story that lands an `.unwrap()` will pass every documented check. Smallest fix: a `## Gates` section in `docs/build.md` listing the three cargo commands and one sentence saying `scripts/check-static.sh` is the assertion `scripts/verify.sh` will call in HS-006.

**S2 (P2) — `--help` is pinned to hardcoded line numbers, in two different styles.**

`scripts/build.sh:29`: `sed -n '2,5p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'` inline in the `-h|--help` arm (no helper). `scripts/check-static.sh:14-16`: a `usage()` function wrapping `sed -n '2,8p'`. Both are correct today (I checked both headers line by line), and both silently print the wrong lines the moment anyone adds or removes a header line — no test covers `--help`. Also a "second pattern for something that already has one", the thing `PROMPT.md:9` forbids. Smallest fix: one `usage()` per script using `sed -n '2,/^$/p'` (or a here-doc), identical in both.

**S3 (P2) — the `PT_INTERP` guard can pass for the wrong reason.**

`scripts/check-static.sh:77`: `if readelf -l "$bin_abs" 2>&1 | grep -q INTERP; then fail …`. Under `set -o pipefail`, `grep -q` exits at the first match and can SIGPIPE `readelf`; the pipeline then reports 141, and the `if` treats it as false — a binary that *does* carry a `PT_INTERP` passes. It does not misfire today only because `readelf -l` output is far below the 64 KiB pipe buffer, i.e. the guard's correctness depends on output size. Smallest fix: `list="$(readelf -l "$bin_abs" 2>&1)"` then `case "$list" in *INTERP*) fail …;; esac` — the same shape already used for `readelf -d` three lines below.

**S4 (P2) — `|| true` merges "readelf failed" with "has a dynamic section".**

`scripts/check-static.sh:80`: `dynamic="$(readelf -d "$bin_abs" 2>&1 || true)"`, then `case "$dynamic" in *"There is no dynamic section in this file"*) ;; *) fail "$bin has a dynamic section: $dynamic" ;;`. A truncated, unreadable or wrong-architecture artifact is reported as "has a dynamic section: <readelf error text>", which is a correct failure with a misleading diagnosis — and the `|| true` is exactly the "swallows failures" pattern in the smell list. Smallest fix: `if ! dynamic="$(readelf -d "$bin_abs" 2>&1)"; then fail "$bin: readelf -d failed: $dynamic"; fi`.

**S5 (P2) — `check-static.sh` cannot tell a fresh artifact from a stale one.**

It asserts linkage, key order and `pi_version` (lines 49-63) but never compares the artifact's `git_tag` with the tree it was built from; the tag is only echoed in the trailing `version --json` line. A `dist/` left over from an older commit passes every assertion. ADR-0005 sells this script as "the evidence that the artifact is what it claims" (`docs/build.md:65`), and `holosmith upgrade <binary>` (spec *Single binary and CLI*) will consume `dist/`. Smallest fix: when `.git` exists, assert the artifact's `git_tag` equals `git describe --tags --always --dirty`; otherwise print one SKIP line, as with qemu.

**S6 (P2) — `dist/` is written non-atomically.**

`scripts/build.sh:` `cp "target/$target/release/holosmith" "dist/$target/holosmith"`. An interrupted or concurrent run leaves a truncated binary at the path the promotion pipeline reads. Smallest fix: `cp` to `dist/$target/holosmith.tmp && mv` (same variable, one line longer).

**S7 (P2) — glossary and wire names diverge on the pin.** Judgement call.

`GLOSSARY.md` **Pinned pi version** ends `_Avoid_: Pi version, agent version, latest pi`, while the code, the `key value` line and the JSON key are all `pi_version` (`src/pins.rs:7`, `src/version.rs:14`, printed by `render_human`). Machine keys being snake_case is right; the risk is that 79 stories will write "pi version" in prose because that is what the field is called, which is precisely the drift the entry exists to prevent. Smallest fix: one clause in the entry — "the machine name is `pi_version`" — rather than renaming anything.

**S8 (P2) — `build.rs` opt-outs make `git_tag` stale on plain `cargo build`.**

`crates/holosmith/build.rs:7-11` emits `rerun-if-changed`/`rerun-if-env-changed`. Emitting any `rerun-if-changed` opts out of cargo's default "rerun when any file in the package changes", so a new `git tag` on an unchanged tree leaves the previously compiled `HOLOSMITH_GIT_TAG` in place. `scripts/build.sh` masks this by exporting a freshly computed value (which `rerun-if-env-changed` does catch), so the release path is sound — the stale case is the convenience path `docs/build.md:42-44` describes. Smallest fix: one sentence in `docs/build.md` ("only `scripts/build.sh` guarantees a fresh tag; a bare `cargo build` may report the tag of the last build").

**S9 (P2) — the ADR index is now stale.**

`docs/stories/README.md:117-121`: "Three decisions were made that the spec does not make. Each has an ADR…" followed by ADR-0001/0002/0003 only. The change adds ADR-0004 and ADR-0005, so that sentence and list are wrong after merge. The plan recorded this as an orchestrator follow-up (`docs/stories/*` was declared out of scope), so it is a known deferral, not an oversight — but it is caused by this diff and should be closed in the fix pass: add the two bullets and change "Three" to "Five".

**S10 (P2) — one speculative ignore rule.**

`.gitignore:19` ignores `/.pi/subagents/`; `.pi/` contains only `APPEND_SYSTEM.md` and `mcp.json`, and the agent's session/subagent state lives under `~/.pi/agent/…` outside the repo. Either the runtime does create it here (keep, but say so in the comment) or it is an ignore rule for a directory nothing produces. Smallest fix: verify once and drop the line, or extend the comment to name who writes it.

---

## Spec

**Correct (with evidence).**

- *Single binary and CLI*, Commands row `holosmith version` — "Version, git tag, pinned pi version, schema version" — is implemented in that order, one per line, exit 0 (`src/version.rs:33-39`, asserted by `src/version.rs:66-76` and `tests/version_cli.rs:29-43`), plus `--json` as single-line JSON with a trailing newline (HS-001 acceptance 2, "machine-readable with `--json`"); `target` and `sqlite` are additions the approved plan accepted (Q4=A), and the human output deliberately stays four lines (Q3 constraint).
- Criterion 3 (one place) is structurally right: `pins/pi.txt` is the only copy, `include_str!` embeds it (`src/pins.rs:4-8`), `build.rs:8` re-runs on it, and `tests/pin_single_source.rs` fails if the literal appears in a second tracked file — verified green repo-wide.
- Criterion 4 is satisfied by construction: `cargo build --release --locked` against a committed registry-only lockfile, no `rust-toolchain.toml` (`docs/build.md:20-22` records why), and `build.rs`'s only subprocess is `git describe`.
- `docs/build.md`/ADR-0005 correctly avoid repeating the pin literal — important, because the guard is a substring search over *all* tracked files.
- V10 is not this story's alone: `docs/stories/README.md:112` assigns it to "HS-001 + HS-003 + HS-004", so the missing web-app embedding and DB open are not HS-001 findings.

**P1 (P2) — acceptance criterion 1 is only partially met, and only ADR-0005 says so.**

`docs/stories/phase-0-walking-skeleton.md:19`: "Build produces `x86_64-unknown-linux-musl` and `aarch64-unknown-linux-musl` artifacts; `file` reports statically linked; **both run in an empty container**." The change discharges the first two clauses on both arches, and substitutes a `bwrap` empty root plus qemu-user for the third. ADR-0005:9 states the limit honestly ("It does not prove anything about a real container runtime; that is the empty-container run … in HS-046") and the approval comment records the deferral, so this is an accepted, visible gap — but the story file's checkbox is untouched, so `docs/stories/` still reads as if it were met. Smallest fix: a one-line parenthetical on that checkbox ("container run deferred to HS-046; see ADR-0005") or a note in `docs/build.md`.

**P2 (P2) — the criterion-3 guard silently no-ops exactly where promotion builds run.**

`tests/pin_single_source.rs:33-38`: `if !root.join(".git").exists() { println!("SKIP no .git in {root:?}…"); return; }`, and lines 39-42 do the same when `git ls-files` fails. libtest captures stdout, so a green `cargo test` in a builder container prints nothing about the guard having been skipped — the failure mode the plan itself called out ("a test is durable"). Smallest fix: when `.git` is absent, fall back to walking the worktree (`target/`, `dist/`, `.git` pruned) instead of returning, so the guard is active everywhere; keep the `Cargo.lock` exclusion.

**P3 (P2) — the same test panics unhelpfully on an empty pin file.**

`contains()` uses `haystack.windows(needle.len())`, so an empty pin panics with "window size must be non-zero" rather than a diagnosable failure. `scripts/check-static.sh:50` guards this (`[ -n "$pin" ] || fail "pins/pi.txt is empty"`); the Rust side does not. Smallest fix: assert `!pin().is_empty()` at the top of `no_other_tracked_file_contains_the_pin`.

**P4 (P2) — criterion 4 has no durable guard.**

"The build fetches nothing at build time except declared dependencies" was proven once, by a manual `bwrap --unshare-net cargo build --offline --locked` run (plan A5), and nothing in the repo re-asserts it: no script runs offline, and nothing rejects a future `source = "git+…"` in `Cargo.lock` or a `build.rs` that reaches the network. Today's shape is correct, so this is about keeping it correct. Smallest fix: one assertion in `scripts/check-static.sh` (`grep -q 'source = "git+' Cargo.lock && fail`) plus a sentence in `docs/build.md`'s Reproducibility section naming the offline command.

**P5 (P2) — the Definition of Done requires a `scripts/verify.sh` case; none exists and nothing records the substitute.**

`docs/stories/README.md:29` and `PROMPT.md:71` ("It implements the slice end to end: code, tests, `scripts/verify.sh` cases …") and `PROMPT.md:14`/`:104` make `scripts/verify.sh` a per-story gate. It does not exist until HS-006, the plan's non-goals explicitly excluded it, and the orchestrator approved that — so HS-001 ships without the gate the repo documents, and so will HS-002…HS-005. `PROMPT.md:9` also forbids "a shortcut around a documented guard", which is what an unrecorded substitution looks like from the outside. Smallest fix: one sentence in `docs/build.md` (or `docs/stories/README.md`) stating that `scripts/check-static.sh` is the pre-`verify.sh` gate HS-006 will call — the same pattern as ADR-0005's honest-limit paragraph, so the deviation is written down rather than implied.

---

## Verdict

**Ship with findings.** The change implements PER-33's acceptance criteria faithfully, honours the approved plan's file scope and non-goals exactly (one tracked edit, 19 untracked files, nothing outside scope), and the two honest limitations — real container runs, and aarch64 hardware — are recorded in ADR-0005 rather than papered over. Nothing here should block the merge: no functional defect was found in the Rust code, the lockfile shape is right, and both scripts are safe to re-run. The fix pass should take **S1** (document the gate commands; the `unwrap_used = "warn"` guard is currently unenforceable), **P2** (make the exactly-one-place guard work without `.git`, since that is the environment promotion builds use), **S5** (a stale-`dist/` blind spot that the promotion pipeline will inherit), and the three one-line doc closures **S9/S8/P5**; the rest are cheap, independently actionable hardening.

*Warning for whoever commits this file:* `tests/pin_single_source.rs` fails if the pinned pi version's literal appears in any second tracked file. This report deliberately never spells it out; keep it that way if you quote the `version` output contract here.
## Resolution

All 15 findings were triaged by a fresh child after the review was saved. Every finding is fixed; none was dismissed. A Resolution row may not quote the `version` output, because the pinned pi version lives in exactly one tracked file (`pins/pi.txt`) and `tests/pin_single_source.rs` fails if the literal appears anywhere else — that is why the demo output for this story is recorded in the Linear issue and not here.

| Finding | Status | Resolution |
|---|---|---|
| **S1** gate policy has no runnable path | **fixed** | New `## Gates` section in `docs/build.md` lists `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test`, says `-D warnings` is what makes the `unwrap_used = "warn"` lint a gate, and names `scripts/check-static.sh` as the artifact gate `scripts/verify.sh` will call (HS-006). |
| **S2** `--help` pinned to hardcoded line numbers, two styles | **fixed** | One `usage()` per script, identical in both: `sed -n '2,/^$/{/^$/d;s/^# \{0,1\}//;p}'` over a header comment block terminated by a blank line; `build.sh`'s inline `sed '2,5p'` is gone. Both `--help` outputs verified. |
| **S3** `PT_INTERP` guard can pass for the wrong reason | **fixed** | `readelf -l` output is captured into `segments` (its own failure is a `FAIL`) and matched with `case "$segments" in *INTERP*) fail …`; no pipeline, so no SIGPIPE-under-`pipefail` false negative. |
| **S4** `\|\| true` merges "readelf failed" with "has a dynamic section" | **fixed** | `if ! dynamic="$(readelf -d …)"; then fail "$bin: readelf -d failed: $dynamic"; fi`, then the existing `case`. `\|\| true` removed. |
| **S5** fresh vs stale `dist/` indistinguishable | **fixed** | `check-static.sh` computes `git describe --tags --always --dirty` when `.git` exists and `FAIL`s if the artifact's `git_tag` differs; one `SKIP git_tag check` line otherwise (builder containers), mirroring the qemu precedent. Proven with a `HOLOSMITH_GIT_TAG=deadbee` artifact. |
| **S6** `dist/` written non-atomically | **fixed** | `cp` to `dist/$target/holosmith.tmp` then `mv -f` onto the final name; sha256/size are read from the final path. |
| **S7** glossary/wire names diverge on the pin | **fixed** | `GLOSSARY.md` **Pinned pi version** now ends "Its machine name is `pi_version`, in the binary's output and in any later config key." Nothing renamed; `_Avoid_: Pi version, …` kept. |
| **S8** `build.rs` opt-outs leave `git_tag` stale on plain `cargo build` | **fixed** | `docs/build.md` **Git tag** states that a bare `cargo build` sets no tag, that `rerun-if-changed` opts out of cargo's default rerun so a new tag on an unchanged tree keeps the old value, and that only `scripts/build.sh` guarantees a fresh tag. |
| **S9** ADR index stale | **fixed** | `docs/stories/README.md`: "Three" → "Five", plus ADR-0004 and ADR-0005 bullets with their consequences. The only edit in any `docs/stories/*` file. |
| **S10** one speculative ignore rule | **fixed** | `/.pi/subagents/` and its comment removed from `.gitignore`: that path is not produced (runtime subagent state lives under `~/.pi/agent/…`, outside the repo), and ~10 full gate runs produced no such directory. |
| **P1** criterion 1 partially met, only ADR-0005 says so | **fixed** | A note in `docs/build.md` (the `## Gates` section): the empty-root run is not a container run, and HS-001's "both run in an empty container" clause stays open until HS-046 (ADR-0005). `docs/stories/*` is out of scope beyond S9. |
| **P2** criterion-3 guard silently no-ops without `.git` | **fixed** | `no_other_tracked_file_contains_the_pin` falls back to walking the worktree (pruning `.git`/`target`/`dist`, not following symlinks) when `git ls-files` is unavailable; the `Cargo.lock` exclusion is kept. Proven with `.git` moved aside: clean tree passes, a probe file containing the pin fails and is named. |
| **P3** test panics unhelpfully on an empty pin | **fixed** | `assert!(!pin().is_empty(), "pins/pi.txt is empty: there is no pin to search for")` at the top of the test; proven with an emptied pin file, which now fails diagnosably. |
| **P4** criterion 4 has no durable guard | **fixed** | `check-static.sh` now fails on a `git+` source in `Cargo.lock`; `docs/build.md` **Reproducibility** names the offline command (`cargo fetch --locked`, then `bwrap … --unshare-net cargo build --release --locked --offline --target <triple>`). Negative test with a synthetic `git+` entry failed as intended. |
| **P5** DoD requires a `scripts/verify.sh` case, nothing records the substitute | **fixed** | The same `docs/build.md` `## Gates` section states that `scripts/check-static.sh` is the assertion `scripts/verify.sh` calls in HS-006 "rather than a second way to check the same thing", so the deviation is written down instead of implied. |

After the fixes: `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test`, `scripts/build.sh`, and two consecutive `scripts/check-static.sh` runs are green; the offline (`--unshare-net --offline --locked`) release build succeeds for both targets; `readelf` was checked directly against `/bin/true` (reports `INTERP` and a dynamic section) to confirm the guards fire rather than merely pass.
