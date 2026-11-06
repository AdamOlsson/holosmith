# The doctor is the preflight's list, printed, with a depth flag

HS-045 is spec item 10 §1 and §7, V2, and the CLI's `holosmith doctor [--quick or --full]` row:

> `--quick`: Linux with systemd where applicable, Docker reachable, git ≥ 2.38, Tailscale logged in, free disk
> ≥ `min_free_gb`, required ports free. `--full` adds: egress self-test, network-off probe, `hs_egress`/`hs_key`
> up, agent image pi version equals the pinned pi version. **V2 gate:** `git merge-tree --write-tree` exits 0
> for a clean merge and 1 with paths for a conflict; if the git version lacks it, the trial-merge fallback is
> implemented. Exit non-zero on any failure, with one actionable line per failure. `ExecStartPre` uses
> `--quick`, so a broken machine does not start serving sessions.

Item 10 uses the same list three times: `server deploy` step 1 runs it ("Preflight, as `doctor --quick`"), the
unit file runs it as `ExecStartPre`, and step 7 runs `doctor --full` **after** the deploy. So the command is two
depths of one list, and the moment each depth runs in is part of what a check means.

## One sentence per check, and `Complaint` is its failing projection

ADR-0028 already said the sentences must be the same in both readers — "the sentence the operator reads must be
the same one either way" — and it made `preflight::Complaint` the value both print. But `serve` only ever needs
to *refuse*, so the preflight answered failures and nothing else, and a `doctor --quick` that "lists each check
with pass/fail" needs the other arm.

The answer is one value instead of two tables: **`preflight::Finding`** is a check, an `Outcome`, and the
sentence; `preflight::report` is its three checks, and `preflight::check_with` is literally
`.iter().filter_map(Complaint::of)` — the complaint is the finding's failure, carrying the same `String`. Each
check has one function with two arms, so there is no second place a pass-word could be written and drift from
the failure-word. `serve` was not touched and prints exactly what it printed before; `doctor` prints the same
sentences with the outcome word in front (`docker: pass: the daemon at … answers GET /_ping`,
`git: fail: git is 2.37 and holosmith needs 2.38 …`). A test in `tests/doctor.rs` asserts the projection
directly rather than trusting the claim.

**One word table, not two.** `preflight::Check` gains the doctor-only words (`systemd`, `merge`, `tailscale`,
`ports`, `database`, `proxy`, `image`) rather than `doctor` growing a second enum with `docker` in it again — the
same reasoning that put `Check::Network` there for ADR-0044. The word is what an operator greps.

The three preflight checks stay in `preflight`, because `serve` refuses on them; the seven doctor-only checks
live in `src/doctor.rs`, because nothing on the serving path may refuse for them.

## Three outcomes, and only a failure changes the exit code

| Outcome | Meaning | Exit code |
| --- | --- | --- |
| `pass` | the check ran and the machine is fine for it | 0 |
| `fail` | the check ran and the machine is not | 1 |
| `unknown` | the check produced **no verdict about whether `serve` may run here**, and the sentence says what was missing and what does need it | 0 |

That is ADR-0044's rule ("we could not measure is not we measured a leak") applied to the rest of the machine,
and it is what makes the command runnable on the machine this build's gate runs on: no systemd
(`/proc/1/comm` is `bwrap`, `/run/systemd/system` does not exist) and a `tailscale` CLI whose daemon answers
`failed to connect to local tailscaled`. `serve` by hand is a supported way to run (`docs/build.md`), so a check
that refused here would make `doctor` unrunnable on a perfectly good machine — and `server deploy` (HS-049) is
where systemd and Tailscale become deploy prerequisites, which is where a refusal belongs.

Two checks make the vocabulary honest rather than a way to hide:

- **`tailscale` has a failing arm.** The daemon answering and saying the node is logged out (`BackendState` other
  than `Running`) is a *measured* state with a one-command fix, so it is `fail` with `tailscale up` in the
  sentence. Only "the daemon did not answer" and "no CLI" are `unknown`.
- **`systemd` is a directory, not a guess.** `/run/systemd/system` exists exactly while systemd is the init; its
  absence is `unknown`, not `fail`, for the reason above.

**The role decides what is asked, not the backend.** A `hub` runs no sessions, so item 10 §6's `hub deploy` —
"step 1 (without the Docker and git checks)" — is honored by *omitting* `docker`, `git` and `merge` from its
report rather than passing them vacuously; disk, Tailscale, the ports and the database still belong to a hub.
`backend = "stub"` on a dev machine does not remove the Docker check: doctor asks about the machine, and
`--backend` is a per-run flag. No `--skip` flag was invented, and `--quick` is the default (the spec's
`[--quick or --full]` reads as one of the two, and the cheap one is the safe default).

## Ports are two moments, one check

`doctor --quick` runs as `ExecStartPre`, before `serve` binds; item 10 §7 runs `doctor --full` after the deploy
has waited for `/api/health`, when `serve` holds both ports. So "in use" means two different things:

| Depth | Port free | Port in use |
| --- | --- | --- |
| `--quick` | `pass` | `fail` — the start would not bind, and the sentence says to stop the holder (`ss -ltnp` names it) or move the listener |
| `--full` | `pass` | `pass` **when the control port's `/api/health` answers 200** — the machine is serving, which is the state step 7 runs in; otherwise `fail` as above |

One fact decides both port lines: `--full` asks the health endpoint once, and a held preview port is read against
the same answer, because a machine whose control port is serving is a machine whose preview listener bound too
(`serve` binds both or neither). Without that split, a healthy server could never pass `doctor --full` — the
command item 10 §7 exists for would always exit 1.

A `:0` listener is `pass` with a sentence saying there is nothing to hold, which is what the gate's own config
asks for; a test holds a real port to prove the failing arm, and a real listener that answers health to prove the
serving arm.

## The database is read without migrating it

ADR-0047 promised "`serve` continues on a newer database and `doctor` refuses" and left the caller unbuilt.
`Database::open` migrates, so `doctor` could not use it: the question has to be answered *before* anything
writes, and `doctor --quick` is `ExecStartPre`. `db::recorded_without_migrating` is the second opener — SQLite's
own `SQLITE_OPEN_READ_ONLY` flag, then the same two statements `apply_pending` decides on — and
`schema::verdict` answers `Same`/`Older` (both `pass`, with the verdict's own sentence) or `Newer` (`fail` with
`schema::Verdict::sentence`, the string `serve` warns with). `Verdict::sentence`'s "`doctor` (HS-045, not built
yet)" was amended in the same commit, because it is no longer true.

A missing file is `pass`, not a finding: a machine that has never served has no database, and `serve` creates one.
A file that cannot be read is `fail` — `serve` cannot open it either, and "unknown" would hide the one thing the
operator has to fix.

## V2 is a trial, and the trial-merge fallback is not implemented

`git merge-tree --write-tree` is measured, not assumed: a throwaway repository under `std::env::temp_dir()` — one
commit, two branches that diverge on one file, and a third commit on `main` that touches nothing the other side
did — is asked twice. The clean pair must exit 0 with one line (the tree's id); the conflicting pair must exit 1
and name the path. Both wrong answers are `fail` with what git really printed; a trial that could not run at all
(no writable temp dir) is `unknown`. The directory is removed whatever happened.

**The spec's fallback — "a trial merge in a temporary worktree" — is not implemented, and this is the same
decision as ADR-0035's V1 rather than a gap.** The git check refuses anything older than `preflight::MIN_GIT`,
which is the release `--write-tree` arrived in; so a git that lacks the flag fails `doctor` on `git` and never
reaches the trial, which reports `unknown` with the sentence "not measured: the git check above failed … the
trial-merge fallback is not implemented because a git older than 2.38 cannot run item 12's mergeability check at
all". A fallback that can only run on a machine doctor has already refused is dead code with a maintenance cost,
and the merge feature (HS-053+) is where a real host's need would land. A git shim reporting 2.37 is the test
that keeps that sentence true, and it also proves the two findings do not double-count one cause: the run has
exactly one failure.

## What `--full` may start, and what it must leave

`--full` adds four things and starts nothing that outlives it:

- **`network`** is `selftest::run` itself. Its bare probe **is the network-off probe**: a container on a temporary
  internal network that must reach neither the internet, nor the host, nor the egress proxy's alias; the
  proxy-joined pass is the second half (ADR-0044/ADR-0046). The temporary network and the `--rm` probe container
  are removed inside `run`, and the gate asserts the network's absence afterwards.
- **`proxy`** is two lines, `hs_egress` and `hs_key`, read with `Docker::running` and **never started**: `serve`
  owns them (ADR-0045/ADR-0048), a stopped container keeps its name, and the sentence says to restart the server.
- **`image`** is HS-031's own question on demand: `agent::version_of` (a `docker run --rm --network none pi
  --version`) and `agent::version_check`, so the sentence a mismatch produces is the one a session fails with. An
  image that will not run is a failure naming the build step and `agent_image`, which is what makes the failing
  arm provable without building an image.
- They are **not measured** (`unknown`) when the `docker` check failed: one failure, one line, and no cascade of
  four sentences blaming a daemon for the thing that is wrong with the daemon.

`ExecStartPre=holosmith doctor --quick` is HS-049's line to write, and there is no unit file to change here.
What this story makes is the **contract that line relies on**: exit 0 iff no check failed, and one actionable
line per failure. That is recorded rather than claimed as a file written.

## What proves it

`crates/holosmith/tests/doctor.rs` drives every check over the `Tools`/`Machine` seams — shims for git, df and
`tailscale`, a directory for systemd, a fake daemon socket, a real bound port, a real health listener, a real
database in each schema state, and a real git for the trial — and both directions of the projection.
`case_doctor_quick` runs the shipped artifact three times (passing, a held port, an absent daemon) and asserts one
line per check, the exit code and the sentences; `case_doctor_full` boots the artifact's own Docker `serve` and
runs `doctor --full` while the machine serves — ports pass because `/api/health` answers, both proxies are up,
the image's pi is the pinned one, the self-test passes and leaves no network; and `case_schema_newer` gains the
`doctor --quick` run that refuses the schema-99 database ADR-0047 promised it would.
