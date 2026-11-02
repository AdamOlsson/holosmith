# Review — PER-38 (HS-006)

**Fixed point:** `6994c0f` (the last commit before this work; HS-005 shipped at `6c4f02c` +
`6994c0f`), reviewed against the **uncommitted** tree. In-scope files read in full: the new
`scripts/verify.sh`, the `docs/build.md` hunks, and the seams the script depends on —
`crates/holosmith/src/http/{guard,error,app}.rs`, `commands/serve.rs` (the banner and the `--port 0`
origin re-derivation), `commands/version.rs`, `config/mod.rs`/`schema.rs` (`owner`, the defaults),
`scripts/{check-static,build,check-embed}.sh`, the HS-005/HS-006 story slices, ADR-0003/0010 and
`GLOSSARY.md`. The change set is `docs/build.md` plus untracked `scripts/verify.sh` only.

**Verification basis.** This role has no shell, so it did not execute the gates; it read the diff
artifact and the tree. The supervisor ran, after sourcing the toolchain env: `bash -n
scripts/verify.sh`; `cargo fmt --check`; `cargo clippy --all-targets -- -D warnings`; `cargo test`;
`scripts/build.sh`; `scripts/check-embed.sh`; and `scripts/verify.sh` end to end (exit 0, 30
assertions, ~70 s), before and after the fixes below. `shellcheck` is not installed on this machine.

**Verdict:** Standards OK with notes, Spec clean on every acceptance box; no blocker, no should-fix.
Four of the six nits were fixed, one was dismissed on a measurement, one on a documented precedent.


Read: the brief, `/tmp/holosmith/reviews/PER-38-diff.txt`, `scripts/verify.sh` (405 lines), the `docs/build.md` hunks, and the seams the script depends on — `crates/holosmith/src/http/guard.rs`, `http/error.rs`, `http/app.rs` (`is_guard_exempt`), `commands/serve.rs` (banner + `--port 0` origin re-derivation), `commands/version.rs`, `config/mod.rs`/`schema.rs` (`owner`, defaults), `scripts/check-static.sh`, `scripts/build.sh`, `scripts/check-embed.sh`, HS-005/HS-006 story slices, ADR-0003/0010, GLOSSARY, `/tmp/holosmith/plans/PER-38.md`. `watchdog_diff --stat` confirms the change set is `docs/build.md` + untracked `scripts/verify.sh` only — invariants 4 and 5 hold (no other script, `Cargo.*` or `crates/**` touched; no pi literal).

## Standards axis

**Correct (verified, not assumed)**
- Header/`usage`/`repo_root`/arg-parse/`fail`/`printf 'ok …'`/`SKIP …` lines all match the house shape of `scripts/check-static.sh:12-57` byte-for-byte in mechanism, including the `sed -n '2,/^$/{…}'` usage idiom and the `unknown argument` → `exit 2` arm.
- Single cleanup path: `grep -n 'trap|rm -rf'` yields exactly `trap cleanup EXIT` (`scripts/verify.sh:74`) and one `rm -rf "$WORK"` (`:71`). No `RETURN`/`ERR` traps, no second `rm`. A failure cannot leak the server or the dir:
  - every `fail()` is `exit 1` → EXIT trap → `kill -TERM` + `wait` (`:63-65`);
  - `set -e` aborts (e.g. `response="$(answer …)"`) also reach the trap;
  - `stop_serve` clears `serve_pid=""` *before* `check "serve/exit"` (`:344-346`), so an already-stopped server is never signalled again, and a failing exit-code assertion cannot re-kill a reaped PID;
  - `-h` exits before `mktemp`, so no orphan dir on the help path (`:31-46` precede `:54`).
- `cleanup`'s `local status=$?` (`:62`) is the correct capture (no `local x=$(…)` masking), and `return 0` in an EXIT trap preserves the script's status. `set -u`-safe everywhere I checked: `${2:-}`, `${1:-}`, `local -a sent=("$@")` with ≥1 header on every call, globals `port`/`control_origin`/`version`/`schema_version` all assigned in `phase_serve` before the case loop.
- Every `bad_*` case really asserts the code it names. Order in `crates/holosmith/src/http/guard.rs:73-108` is host → origin → client → owner, and each case keeps the higher checks correct:
  - `case_guard_bad_host` (`:238`) — `Host: evil.example` is the only fault (implicit `127.0.0.1` never reached) → `bad_host`;
  - `case_guard_bad_origin` (`:248`) — correct host, non-GET, no `Origin` → `bad_origin` (fires before the client check);
  - `case_guard_bad_client` (`:258`) — correct host, `Origin: $control_origin`, JSON, no client header → `bad_client` (fires before the owner check);
  - `case_guard_bad_owner` (`:270`) — all four preceding values correct **and** the config file genuinely sets `owner` (`:375`, schema key at `config/schema.rs:103`), so `bad_owner` is reachable rather than passing vacuously;
  - `case_guard_passes` (`:283`) — the control origin it sends is the banner's re-derived one (`serve.rs:118-120`, `127.0.0.1:0` → `http://localhost:<port>`), which is what makes this case prove the guard's comparison string, not just 200-vs-404.
- Banner parsing is read, never assumed: `control_listen` → `port` via `${…##*:}` (`:329`), `control_origin` (`:331`), `database … schema …` compared as one exact line (`:210`), each guarded by an `[ -n … ] || fail`. No hard-coded port, no `/var/lib/holosmith`, no `RUSTFLAGS`, no download, no `curl`/`jq`/`python3` (invariants 1, 2, 3, 6 hold).
- Failure/skip ordering matches the docs: `run_phase` names the phase (`FAIL build/failed`, `:88`), `fail` prints the assertion then the EXIT trap prints `work dir kept:` — the EXAMPLES in `docs/build.md:119-120` (`FAIL case_guard_bad_origin/code`) are literally produced by `expect_code`'s `check "$name/code"` (`:110-116`).
- No shellcheck-class problems spotted by reading (quoting of `"$@"`, `"${sent[@]}"`, `"$WORK"/responses/*.http`, `local status=0` before `wait`), and `bash -n` is green per the brief. `shellcheck` is not installed here, so that is a reading-based claim only.

#### Finding S1 — nit — stale comment contradicts the sweep's placement.
`scripts/verify.sh:303-304`: "Box 4, over every answer the cases above saved … **Last in the list**, so nothing answered above is missed." The sweep is no longer in the list — `serve_cases` ends at `case_health_is_exempt` (`:167-179`) and `case_no_cors_header` is called after the loop (`:383`). The array's own comment (`:158-166`) was updated for exactly this move; this one was not. Smallest fix: replace the last sentence with "Runs after the case loop, so nothing answered above is missed."

#### Finding S2 — nit — the `/dev/tcp` connect-failure path is unproven, and if it is fatal the `no_answer` contract is bypassed.
`scripts/verify.sh:144` `exec {fd}<>"/dev/tcp/127.0.0.1/$port" || return 1`. `exec` with redirections only is a special builtin; if bash treats the failed redirection as fatal, the `|| return 1` never runs and the whole script dies with a raw `connect: Connection refused` instead of the intended `FAIL case_x/no_answer` — the one failure mode acceptance 3 cares most about. This is the same idiom as `scripts/check-static.sh:110`, so it is a repo-wide pattern rather than a new defect, and I could not test it (no shell). One-line check for the orchestrator: `bash -c 'exec 3</nonexistent || echo caught'; echo "exit=$?"` — if it prints only `exit=1`, the guard is dead in both scripts.

#### Finding S3 — nit — the waits are bounded per request but not overall.
`scripts/verify.sh:146` uses `timeout 10` per fetch (correct: a timed-out read returns 124 → `return 1` → `FAIL …/no_answer`), but ten cases mean up to ~100 s of dead time if the server accepts and stalls; and the trap's `wait "$serve_pid"` (`:65`) has no `timeout`/`--kill-after`, so a child that ignored `SIGTERM` would wedge the trap instead of exiting. House precedent (`check-static.sh:112`) is identical, so this is a note, not a defect: if wanted, add `-k` to the `timeout` and wrap the trap's `wait` in `timeout 5 wait "$serve_pid" || true`.

**Standards verdict:** no blocker, no should-fix. The script is a faithful, house-styled extension of `check-static.sh`; three nits, two of them (`S1`, `S2`) worth folding in before commit because they are one line each.

## Spec axis

**Correct**
- Acceptance 1 — "builds, runs unit tests, then boots the real binary on a random port with a temp data dir": phases at `scripts/verify.sh:392-403`; `--port 0` (`:317`, kernel port, and it is what triggers the origin re-derivation at `serve.rs:118-120`); `--data-dir "$WORK/data"` (`:317`); the artifact is the shipped musl binary (`:355-360`), and a missing one fails naming the path and `scripts/build.sh` — the plan's stated risk, satisfied.
- Acceptance 2 — health (`:182`, prefix + closing brace, `content-type: application/json`), the four guard refusals plus the passing request and the exempt probe (`:238-301`), the schema version against `version --json` (`:206-214`, which also proves `--data-dir` was honoured because the asserted line carries the full path), then teardown `SIGTERM` + `wait` + assert exit 0 (`:340-347`) — the one assertion `check-static.sh` cannot make (its own comment at `check-static.sh:213-215` says why).
- Acceptance 3 — `FAIL <assertion>` plus a kept work dir holding `config.toml`, `serve.out`, `serve.err` and the answers.
- Acceptance 4 — `docs/build.md:102-105` states the convention in bold, and `docs/stories/README.md:29` already carries the DoD version.
- `docs/build.md`'s prior promises are kept, not weakened: the old text said `check-static.sh` is "the assertion `scripts/verify.sh` calls … rather than a second way to check the same thing" — `scripts/build.sh:84-91` does run `check-static.sh`, so calling `build.sh` once satisfies it and honours the `npm ci`-once constraint (`build.sh:58-60` runs `build-web.sh`); `check-embed.sh` is now called from the gate for the first time (`:398`), which closes the `docs/review/PER-35.md:18` gap. Nothing outside the plan's non-goals landed: no new ADR, no CI, no Rust test, no dependency.
- The CORS sweep's placement is right and defensible: globbing `"$WORK"/responses/*.http` (`:307`) means a case a later story appends is covered automatically.

#### Finding P1 — nit — the failure contract sentence over-promises the kept dir's contents, and its own example disproves it.
`docs/build.md:119-122`: "a failure prints `FAIL <assertion>` (`FAIL build/failed`, …) and keeps the work dir … that dir holds `config.toml`, `serve.out`, `serve.err` and one `.http` and `.body` file per answer." `config.toml` is written at `scripts/verify.sh:375`, inside `phase_serve`, so the sentence's own first example — a `build`/`fmt`/`clippy`/`test`/`embed` failure — keeps a dir containing only the empty `responses/`. Smallest fix: scope the sentence ("for a `serve` failure that dir holds …; a failure before the boot keeps an empty work dir").

#### Finding P2 — nit — garbled cross-reference in the gates section.
`docs/build.md:37-38`: "`scripts/verify.sh` is one command that runs all of it: the three commands above, the artifact build and its checks, and the boot of `## One-command verification` below." Reads as prose mangled by a heading name; the section below is the description, not something you boot. Smallest fix: "…and the boot described under **One-command verification** below."

#### Finding P3 — nit — `RESUME.md:179` is now false.
"`scripts/verify.sh` does not exist yet — HS-006 creates it." It exists as of this change; the line is in the repo and is the first thing the next session reads. Smallest fix: replace with "Gates: `scripts/verify.sh` (fmt, clippy, test, build, embed, serve)."

#### Verification notes (not findings, for the supervisor)
- The story's demo ("red when any guard is removed") will go red at phase 3 (`cargo test`, `tests/guards.rs`), not at phase 6, so it does not exercise the serve cases. If you want recorded evidence that the serve phase fires, the cheapest repro is: build, then `printf x >> web/dist/index.html`, then `scripts/verify.sh --no-build` → `FAIL case_app_shell/index_html` with the work dir kept.
- Confirm `scripts/verify.sh` is committed mode `100755`: `docs/build.md:64` runs it directly, and I have no way to inspect the mode.
- Optional gates if `shellcheck` is installed elsewhere: `shellcheck scripts/verify.sh`.

**Spec verdict:** clean on every acceptance box and on the `docs/build.md` promises it inherits; three documentation-only nits, none blocking.

**Merge verdict: OK with notes.**
## Resolution

No blocker and no should-fix. Four nits fixed, two dismissed with a measurement or a documented
precedent, and both of the reviewer's verification notes answered with evidence.

| Finding | Status | How / why |
| --- | --- | --- |
| **S1** the CORS sweep's comment still said "last in the list" after the sweep was moved out of `serve_cases` | **fixed** | The comment now says it runs after the case loop and why that placement is what makes a case appended later covered. The array's own comment had already been updated for the move; this was the one that was missed. |
| **S2** a failed `exec {fd}<>"/dev/tcp/…"` might be fatal to the shell, which would bypass the `no_answer` contract and print a raw `connect: Connection refused` instead of `FAIL <case>/no_answer` | **dismissed, with a measurement** | Ran it: `bash -c 'f(){ exec {fd}<>"/dev/tcp/127.0.0.1/1" \|\| { echo CAUGHT; return 1; }; }; f'` prints `CAUGHT` and the function returns 1 — a failed redirection with `exec` is not fatal, so `\|\| return 1` runs and the assertion path is intact. The reviewer flagged it as untestable without a shell and asked for exactly this one-liner; it comes back clean, so no code change. Worth recording because `scripts/check-static.sh` uses the same idiom and the same question applies there. |
| **S3** the waits are bounded per request (`timeout 10`) but not overall, and the EXIT trap's `wait` has no deadline, so a child ignoring `SIGTERM` could wedge the trap | **dismissed, with a citation** | `SIGTERM` cannot be ignored indefinitely here: the drain has a 10 s deadline after which open connections are cut and the process still exits 0 (ADR-0007, measured when the seam was built), so the trap's `wait` is bounded by the same 10 s. The per-fetch `timeout 10` matches `scripts/check-static.sh`'s house precedent exactly. Adding a `timeout` around the trap's `wait` would also not work as suggested — `wait` is a shell builtin, so `timeout 5 wait …` cannot wrap it — and hand-rolling a poll-then-`SIGKILL` ladder is more moving parts than a bounded-by-construction wait. |
| **P1** the failure-contract sentence over-promised the kept dir's contents: `config.toml` and the responses only exist once the `serve` phase has run, so its own `FAIL build/failed` example keeps an empty dir | **fixed** | The sentence now scopes the claim: a `serve`-phase failure keeps `config.toml`, `serve.out`, `serve.err` and one `.http`/`.body` per answer; an earlier-phase failure keeps an otherwise empty dir, because nothing has been written yet. |
| **P2** "the boot of `## One-command verification` below" read as prose mangled by a heading name | **fixed** | "…and the boot described under **One-command verification** below." |
| **P3** `RESUME.md` still said `scripts/verify.sh` does not exist yet | **fixed** | The handoff now names `scripts/verify.sh` as the one-command gate, lists the phases it runs, and repeats that every later story adds at least one `case_*`. That line is the first thing the next session reads, so leaving it false would have cost more than the nit it looks like. |

### The reviewer's verification notes

- **"The demo will go red at phase 3, not phase 6."** Correct, and both were performed. A removed
  guard (`if !origin` → `if false && !origin` in `src/http/guard.rs`) fails the harness at
  `FAIL test/failed`, because `--no-build` does not skip `cargo test` — the guards are proved twice
  over. To exercise the serve phase specifically, the artifact was rebuilt with the mutation and the
  source restored, so `dist/` held the stale binary: `scripts/verify.sh --no-build` then failed at
  `FAIL case_guard_bad_origin/code`, `expected 'bad_origin', got 'bad_client'`, `work dir kept:
  <path>`, exit 1, and the kept dir held `config.toml`, `serve.err`, `serve.out`, `data/` and the
  per-answer `.http`/`.body` files including the offending `{"code":"bad_client",…}`. Source
  restored with `git checkout --` before anything was committed.
- **Commit mode must be `100755`.** Confirmed with `git ls-files -s scripts/verify.sh` after staging;
  the file is `100755`.

After the fixes: `bash -n scripts/verify.sh` clean, `cargo fmt --check`, `cargo clippy --all-targets
-- -D warnings`, `cargo test`, `scripts/build.sh`, `scripts/check-embed.sh` and `scripts/verify.sh`
end to end are all green (exit 0, 30 assertions).
