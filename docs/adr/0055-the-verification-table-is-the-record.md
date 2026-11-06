# The verification table's result cells are the record

HS-046 is the story that *runs* phase 1's verification table and closes it, and it is the gate phase 2 waits
for. Two questions had to be settled before a cell could be filled:

1. **Where does the run's record live?** The demo asks for "a filled-in results table in the repo, with every
   failure and its fallback decision", and `HolosmithSpec.md` already owns that table: eleven rows, each with
   the condition that makes it a pass, the fallback to take if it fails, and a `Result` column that ADR-less
   stories (HS-040, HS-041, HS-044) had already been writing their verdicts into. A `docs/verification/phase-1.md`
   would be a second spelling of a table the spec has, and the one thing a second spelling reliably does is rot
   differently from the first.
2. **Does `doctor --full` grow a check per row?** The spec's sentence — "`holosmith doctor --full` automates as
   many as it can" — is a statement about coverage, and coverage is a per-row fact rather than a knob.

## The cells are the record, and a cell names what produced it

`HolosmithSpec.md`'s `Result` column *is* the record; the story filled it and added no second document. A cell
carries three things: the verdict (`**pass**`, `**fail**`, or `**pass, with the fallback applied**` — the words
the column already used), the automation that produced it, and, on a failure, the fallback taken. "The
automation" is always something a reader can run: a `case_*` in `scripts/verify.sh`'s `serve_cases`, a
`tests/*.rs` under the crate, a `scripts/*.sh`, or the by-hand procedure in `docs/build.md`. That is what makes
a cell evidence instead of a memory, and it is what a gate case can check without knowing anything about the
check itself.

The spec's prose above the table carries the two consequences, so a reader who never opens this file still
meets the rule: there is no second document, and repeatability is one command.

## `doctor` grows nothing, and that is the honest per-row answer

| Row | What it needs | Where it lives |
| --- | --- | --- |
| V1 | a container with the spec's four mounts, and a scratch repository | `case_container_mounts`, `tests/container.rs` |
| V2 | a git command in a throwaway repository | **`doctor --quick`'s `merge` finding** — the one row that *is* a check |
| V3–V5 | a real provider key | by hand, `docs/build.md` — HS-040's run, not re-measured here |
| V6 | two real sessions differing in the `network` flag | `case_internet_toggle` |
| V7 | a container on a session network, and the listener | `case_preview`, second half |
| V8 | pi's recorded records | `tests/rpc.rs` and the five fixtures |
| V9 | a killed server, a daemon and an image | `case_reconcile`, `case_reconcile_orphans` |
| V10 | the build | `scripts/build.sh` → `scripts/check-static.sh` |
| V11 | phase 3's trust model | nothing — deferred to HS-070 |

V2 is already `doctor`'s, because comparing two `git merge-tree` exit codes is a question a check can answer
without starting anything. The others are not, for four different reasons, and each reason is one this
repository already holds: V1 needs a container, an image and a scratch repository — the gate's case spells the
four mounts the way the spec does, on purpose, because V1 is measured independently of the binary, while
`tests/container.rs` is what asserts the vector `container::SessionContainer` builds — and a check would be a
third spelling of the mounts plus a mount source the daemon must see (ADR-0035); V6, V7
and V9 need a **session** — a row, a network, a provisioner — and a check that creates one is no longer a check
of whether this machine may serve (ADR-0054); V8 is a fixture replay, which is what `cargo test` is for; V10 is
the build, which the gate's `build` phase already runs on both targets; V3–V5 need a provider key, and a gate
that holds one is not a gate (ADR-0048, ADR-0050). So "as many as it can" is satisfied by *recording* which
automation covers which row, not by widening `doctor` until it can start containers.

A future story that wants a new `doctor` check now has an answer rather than a judgement call: it may be a check
if it needs no session and no key. If it needs either, it is a gate case, and its row says so.

## Repeatability is one command, and the run says what it proved

`scripts/verify.sh` runs every row's automation except V3–V5 in one invocation — the row's case in its `serve`
phase, its cargo test in the `test` phase, its build in the `build` phase — and since this story it prints one
`ok <case>` line per case that measured something: a case that prints `SKIP` withholds its `ok`, because a skip
logged as a pass is exactly the shape this gate may not have on the two rows (V6, V7) whose cases have a
daemon-gated half. Before that, a green run could only say *how many* cases passed, so a reader of the
table could not tell from the log which case belonged to which row; the mapping lived only in prose. Now the
log and the table share their vocabulary, and `case_verification_table` closes the other end: it asserts that
every automation a cell names actually exists, so a renamed or deleted case fails the gate instead of leaving a
verdict pointing at nothing.

`case_verification_table` is the gate's half of the record. It fails when a V1–V10 row still says "not yet run",
when a cell carries no verdict, when a row that says `fail` records no fallback (acceptance bullet 2 asks for the
fallback, not only the verdict), when a cell names an automation the repository does not have, when V11 stops
saying it is deferred to phase 3 (or stops naming HS-070 — the story that will run it), and when the table does
not have eleven rows. That count is the non-vacuity guard in both directions: a broken read (a renamed heading, a
table that moved out of the file) must fail rather than pass over nothing, and a twelfth row is a deliberate edit
here rather than a row the case walks past. It reads every `V<n>` row, so a row a later story adds is measured by
the same rules; it reads `HolosmithSpec.md` and nothing else, so it is not a second copy of the table.

It runs first in `serve_cases`. The gate's own rule is cheapest first, so a table that has rotted fails the serve
phase before any Docker case rather than after ten minutes of them.

## Consequences

- Editing the `Result` column is a design act: it is the only place a verification verdict lives.
- A row that fails gets its fallback column's remedy and, if the change is hard to reverse, its own ADR — the
  table's "If it fails" column is the plan, not a footnote. This story took none: no row failed. It also did not
  re-measure V3–V5 — this machine holds no provider key, and no gate may hold one — so those three cells quote
  HS-040's by-hand run and say so; every other row was run again in this session, on this Docker host.
- V11 stays the one row a phase-2 story may not fill, and its cell names HS-070 so the debt has an owner.
- `doctor` stays a machine-fitness command. A row that needs a session or a key is a gate case's, and the gate's
  log now says which.

## Alternatives rejected

- **A `docs/verification/phase-1.md` report.** A second spelling of the spec's own table; it would need the same
  eleven verdicts and would drift from them silently.
- **A `doctor --verify` mode that runs every row.** It would have to create sessions (V6, V7, V9) and hold a
  provider key (V3–V5), so it would be a second gate with a key in it, and `doctor`'s own contract — one line per
  check, no session, exit 1 iff a finding failed (ADR-0054) — would be gone.
- **Per-row shell scripts beside the gate.** Every automation already lives in one of four places the gate
  reaches; a fifth entry point would be a second way to run the same checks and a second thing to keep green.
