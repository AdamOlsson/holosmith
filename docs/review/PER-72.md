## Review

**Scope and limits.** Fixed point `408a3d8`; the change under review is `/tmp/holosmith/reviews/PER-72-diff.txt` (a `git diff --cached`, 525 lines, 8 files). Reviewed read-only with no shell: nothing was built or run, and committed ranges were not inspected — the artifact is the boundary. Sources read: `AGENTS.md`, `RESUME.md` §1/§2/landmine 28, `docs/adr/0048`, the new `docs/adr/0050`, `docs/stories/phase-1b-docker-backend.md:272-281`, `docs/stories/README.md`, `docs/build.md` (gate chapter + the new section), `HolosmithSpec.md:396-412`, plus `keyproxy.rs`, `key.rs`, `container.rs`, `config/mod.rs`, `scripts/verify.sh`, `scripts/check-static.sh`.

---

## Standards axis

**Correct**

- The `upstream` change is minimal and its doc states the invariant the code now enforces (`keyproxy.rs:245-261`); the new test asserts **bytes**, not a declaration — `ends_with(body)`, `Content-Length: 13` from a 13-byte body, and `request.len() == head_end + 4 + body.len()` (`keyproxy.rs:836-853`). A head-only request cannot satisfy any of the three, so the test fails on the old behaviour. That is the "a frame that declares a length is a claim about bytes in the same value" rule applied correctly.
- The mount test asserts both halves — the directory is a source, the file is **not** — and that `--keys` still names the file (`container.rs:1006-1027`). Regression-proof in both directions.
- The new gate half is genuinely load-bearing: `OPENSSL` is the bare name `"openssl"` (`keyproxy.rs:62`), so the `PATH` shim really is the child process, the canned `200` can only arrive via the shim, and a dropped body fails `provider-body` (`verify.sh:1801-1804`). The case also restores the `anthropic` key before its later `key remove` assertion (`verify.sh:1794-1797`), so the pre-existing assertions keep meaning what they say.
- ADR-0050 exists, is linked at `docs/stories/README.md:168`, and the count moved 49 → 50 (`README.md:117`). Its shape matches its neighbours (problem → decision → the general rule → consequences) and it does not claim more than was measured — including the honest sentence that V3's fallback "is not needed and was not taken".
- Shell portability is clean: no `awk`, no `shellcheck` dependency, `case` globs instead of `grep -c` chains for the mount verdict, `tail -c`/`head -c`/`dd` are portable, `CAPTURE` reaches the shim through the command's environment.

**Findings**

**S1 · P2 · `docs/build.md:268` — the documented "every later story adds at least one `case_*` function and one line in `serve_cases`" rule is not followed.** HS-040 adds a substantial new gate half — a second `proxy key` boot, a TLS-client shim, five assertions — inside the existing `case_key_proxy` (`scripts/verify.sh:1716-1804`); the function list is unchanged (`verify.sh:361`) and no `case_*` function was added. The rule's stated purpose ("a story that adds a route or a behaviour has one obvious place to prove the shipped binary does it") is exactly this case. *Smallest fix:* either name the half `case_key_proxy_request` with one line in `serve_cases` (it needs no server, so it slots anywhere), or — if sharing `case_key_proxy`'s scratch root is the reason — add one clause to `docs/build.md:268` saying a key-free half of an existing case may stay inside it, so the rule and the gate agree.

**S2 · P2 · `docs/build.md:231-235` — the sentence describing `case_key_proxy` is stale in the paragraph the diff edited around.** It still ends "the forwarding half is HS-040's because a gate that needs a real provider key is not a gate". The diff's own new section (`docs/build.md:626-641`) now says the opposite: `case_key_proxy` asserts "the bytes the proxy produces: one `Content-Length`, the body after the blank line, and the stored key swapped in", with no key. *Fix:* extend that clause — "the **real** forward is HS-040's demo; the **bytes** the proxy produces are the gate's, and they need no key".

**S3 · P2 · `crates/holosmith/src/key.rs:6-7` (and `container.rs:618`, `RESUME.md:454`, `GLOSSARY.md:69`) — the docs that name the mount were not all updated with the mount.** `key.rs:6-7`: "`<data>/secrets/keys.toml` … `hs_key` mounts it read-only" — it now mounts the *directory*. `container.rs:618`: "`<data>/secrets/keys.toml`, mounted read-only" on the field itself, two lines below a doc comment that says the directory is what is mounted. `GLOSSARY.md:69` ("`hs_key` is the only container that mounts it") and `RESUME.md:454` carry the same shape. The story changed the mount, wrote ADR-0050 about it, and left the module that owns the store describing the old shape — a later reader who trusts `key.rs`'s module doc re-introduces the file mount. *Smallest fix:* `key.rs:6-7` → "…`hs_key` mounts that **directory** read-only (ADR-0050)"; `container.rs:618` → "the store's path; `mount` mounts its directory"; the `GLOSSARY`/`RESUME` lines get the same clause when next refreshed (`RESUME.md` is refreshed by bookkeeping commits, so it is a note rather than a blocker).

**S4 · P3 · `scripts/verify.sh:2137` + `:2143` — one claim, two assertions.** The new `case` increments `assertions` in its `*"/secrets "*)` pass arm (`:2137`) and the pre-existing `assertions=$((assertions + 1))` after the `esac` (`:2143`) runs too. The mount claim therefore counts twice: the run's total is one higher than the assertions the case makes, and the "six new assertions" is five real ones plus this duplicate. No behaviour is hidden — it misstates coverage in the summary line a reviewer is asked to trust. *Fix:* delete the inner increment (`:2137`); `fail` exits, so the trailing one is only reached on the pass path.

**S5 · P3 · `scripts/verify.sh:2131` — `key_mounts` is not `local`.** Every other scratch name in the case is `local` (`verify.sh:1637`), and PER-70's S8 fixed exactly this pattern for `want`/`running` (`docs/review/PER-70.md:57`). *Fix:* add it to the `local` list.

**S6 · P3 · `crates/holosmith/src/container.rs:647-651` — `mount`'s fallback cannot fire for the case it means to handle.** `Path::parent()` returns `Some("")` for a bare filename, so `map_or_else`'s default is reachable only for `""` and `/`; a bare `keys.toml` would yield `-v "::ro"`. Unreachable today — `Config` refuses a relative `data_root` (`config/mod.rs:744-746`) and `KeyProxy::of` derives the path from it — so this is a note, not a bug: `.parent().filter(|p| !p.as_os_str().is_empty())` would make the fallback true, or delete it.

**S7 · P3 · `scripts/verify.sh:1725-1727` — the shim's comment claims more than the pipe contract guarantees.** "One read is enough, since the proxy writes a whole request in a single write" is true of `forward` (`stdin.write_all(&request)`, `keyproxy.rs:530`) but a pipe `read` may still return short; if `dd bs=8192 count=1` ever does, the failure surfaces as `provider-body` — i.e. it blames the defect the case exists for on a harness race. Low probability (the request is well under `PIPE_BUF`), misleading if it happens. *Fix:* read until the capture's size stops growing, or state the assumption the comment already makes in the assertion's failure text.

---

## Spec axis

**Correct**

- Bullet 5 ("results written into the spec's table") is met in the shape the spec's own intro asks for: every row has a `Result` cell, the rows this story did not run name the story that will (`HolosmithSpec.md:398-410`), and the intro sentence says who fills the column (`:398`).
- Bullet 4 (fallbacks recorded) is met where a fallback applied: V5's cell says the check holds "only while the seeded pi-home has no `auth.json` and the container environment has no provider key — which is what the build does", and `docs/build.md:663-671` records the same as a rule for the build, not a repair. V3 and V4 state that no fallback was needed rather than leaving it implicit.
- V3's cell does the harder, righter thing: it records that the run **first failed** and names the defect ("the proxy forwarded a `Content-Length` with no body"), which is what makes the pass worth anything.
- Not updating `docs/stories/phase-1b-docker-backend.md:272-281` is the repo's norm, not a gap: every story in that file — including Done ones (`:262-268`, HS-039) — still carries `- [ ]` boxes; the ledger is Linear plus `docs/review/`.

**Findings**

**SP1 · P2 · `HolosmithSpec.md:401` — V5 records `pass` beside a Pass condition the run measured false, and nothing rewrites the condition.** The Pass column still reads: "With conflicting values in `models.json`, `auth.json` and the provider environment variable, the key proxy log shows the `models.json` dummy key in use." The measurement is the opposite order: with a competing `auth.json` present the log says `session=- … status=bad-key`, which is what the Result cell says. Recording it as "pass, with the fallback applied" is *honest* — the fallback column is the design change the spec's intro contemplates — but the Pass cell is now a condition the shipped design can never satisfy, and HS-046's bullet ("V1–V10 are each run on a real Docker host", `phase-1b-docker-backend.md:376`) will read a `pass` against it. *Smallest fix:* rewrite the Pass cell to what can hold — "with no `auth.json` in the seeded pi-home and no provider key in the container environment, the log shows the `models.json` dummy in use; a competing `auth.json` reaches the proxy as `bad-key`" — and leave the fallback sentence as it is.

**SP2 · P2 · `HolosmithSpec.md:400` — the V3 cell's "388 thinking characters" disagrees with the recorded run.** The run summary supplied with this review says "434 characters of thinking deltas"; the normative table says 388. One of the two is wrong, and a precise number in a table a later phase is told to trust is the worst place to be wrong. I cannot adjudicate: the demo transcript is in Linear, not in the repository. *Fix:* reconcile against the transcript and, whichever figure survives, say where the number comes from (or say "thinking deltas at `--thinking max`, see the demo transcript") so the cell is checkable rather than a claim on trust.

**SP3 · P3 · `HolosmithSpec.md:410` — V10's cell is a claim about another story's mechanism, and it overstates it.** "not yet run (HS-046); `scripts/verify.sh` builds, links and boots both targets every run" — `build.sh:45` does build both targets, but the non-host target **boots** only when its emulator is installed: `check-static.sh:267-275` prints `SKIP <target> execution: … not found` and continues. On this machine that is `qemu-aarch64-static`. *Fix:* "builds and links both targets every run and boots each where the machine has its emulator (a missing one is one SKIP line, `scripts/check-static.sh`)".

**SP4 · P3 · `HolosmithSpec.md:402` — V4's cell asserts a catalogue fact the repository cannot support.** "answered 29 models for the provider from pi's bundled catalog" does not name the provider, and nothing in this repo shows that pi's bundled catalogue carries an `opencode-go` entry — ADR-0050:44-52 argues the *anthropic-shaped row* needs no `models` array, which is the same assumption. Recording it is fine; naming it costs a clause. *Fix:* "29 models for `opencode-go` from pi's bundled catalogue (so the catalogue carries the provider; the openai-shaped row would need a `models` array — ADR-0050)".

---

## Axis summaries

- **Standards:** 7 findings — 3 × P2 (S1 the `case_*` rule, S2 the stale `case_key_proxy` paragraph, S3 the un-updated mount docs), 4 × P3 (S4 assertion double count, S5 missing `local`, S6 dead fallback, S7 shim read assumption). **Worst: S1/S2/S3** — all documentation-of-record drift the diff itself created or left behind, none a behaviour defect.
- **Spec:** 4 findings — 2 × P2 (SP1 the V5 Pass cell, SP2 the 388-vs-434 number), 2 × P3 (SP3 V10 cell, SP4 V4 catalogue claim). **Worst: SP1** — the row reads `pass` against a condition the measurement falsified, even though the prose beside it is honest.

**No P1 on either axis:** the shipped code does what its docs and the story say, the two HS-039 defects are fixed at their causes (`upstream` returns head+body; `hs_key` mounts `<data>/secrets` and `Store::load` re-reads per request — `keyproxy.rs:378`, `key.rs:231`), and each fix has an assertion that fails without it.

**Merge verdict: OK with notes.** Fix S1/S2/S3 and SP1/SP2 before this reads as Done (all are one-sentence or one-line edits plus one decision on the case boundary); S4–S7 and SP3/SP4 are report-only.

## Test commands a supervisor must run

1. `scripts/verify.sh --no-build` on this tree — confirms the new provider half and the rewritten `key_mount` branch, and shows the final assertion total (by my reading of `verify.sh:2137`+`:2143` it should be 614 honest assertions, printed as 615).
2. `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo fmt --check` (including `the_upstream_request_is_the_head_and_then_the_body` and `the_key_proxy_mounts_the_binary_and_the_directory_the_key_file_lives_in`).
3. Two mutations, to confirm the new assertions are not vacuous: (a) restore `upstream` to head-only → `case_key_proxy/provider-body` must go red; (b) restore `run_args`' mount to `{keys}:{keys}:ro` → the container unit test **and** `case_internet_toggle/key_mount` must go red.
4. Reconcile V3's thinking-character count (388 in `HolosmithSpec.md:400`) against the Linear demo transcript (the summary says 434), and confirm V4's 29 models came from a catalogue entry for `opencode-go`.
## Resolution

Every finding was triaged; nothing was dismissed. The two P2s on the standards axis were documentation debt the
change itself created, and SP1 was the important one: a table that says `pass` beside a condition the run
falsified is exactly the kind of claim this repository refuses.

| # | Finding | Disposition |
| --- | --- | --- |
| S1 | The new gate half lives inside `case_key_proxy`, so no `case_*` function was added | **Fixed** — it is `case_key_proxy_request`, with its own scratch root and its own `serve_cases` line; the gate is now **61 cases** |
| S2 | `docs/build.md` still says the forwarding half is HS-040's *because a gate cannot have a key* | **Fixed** — the sentence now separates the two halves: the real forward is the recorded demo, the **bytes** the proxy hands its TLS client are the gate's |
| S3 | `key.rs`'s module doc, `container.rs`'s field doc and `GLOSSARY.md` still describe a file mount | **Fixed** in all three; `RESUME.md`'s line is corrected in the handoff commit |
| S4 | The mount claim counted twice (`assertions` incremented inside the `case` and again after it) | **Fixed** by the split: each case counts its own assertions, and the run prints 615 honest ones |
| S5 | `key_mounts` was not `local` | **Fixed** — added to the case's `local` list |
| S6 | `mount`'s `map_or_else` fallback could not fire (a bare filename's parent is `Some("")`) | **Fixed** — the parent is filtered for emptiness, so the fallback is reachable |
| S7 | The shim's `dd` assumes one read returns the whole request | **Fixed** in the comment, and the `body` failure sentence names the assumption rather than blaming the proxy |
| SP1 | V5's **Pass** cell was a condition the measurement falsified | **Fixed** — the cell now states the condition that holds (no `auth.json` in the seeded pi-home, no provider key in the environment) and names the measured order beside it; the fallback column is unchanged |
| SP2 | V3's cell said 388 thinking characters while the recorded run printed 434 | **Fixed** — 434, and the cell says it is the recorded run's number |
| SP3 | V10's cell claimed both targets boot every run | **Fixed** — they are built and linked every run and booted where the machine has the emulator |
| SP4 | V4's cell did not name the provider whose catalogue entry it relies on | **Fixed** — `opencode-go`, with the consequence for the openai-shaped row recorded |

**The review's "run these yourself" list, run:** `scripts/verify.sh` on the committed tree — 61 cases, 615
assertions, ok; `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings` and `cargo test` green; and
both mutations it named — restoring `upstream` to head-only makes
`the_upstream_request_is_the_head_and_then_the_body` fail, and restoring the file mount makes
`the_key_proxy_mounts_the_binary_and_the_directory_the_key_file_lives_in` fail. Both were restored and re-run
green.
