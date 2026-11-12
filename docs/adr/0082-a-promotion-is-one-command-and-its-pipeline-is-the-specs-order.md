# A promotion is one command, and its pipeline is the spec's order

HS-078 is spec item 10's closing sentence — "In v1 these steps are implemented by `holosmith upgrade <binary>` and
`holosmith rollback` (see Single binary and CLI), not by a separate script" — and the acceptance is a list of six
things that have to be true at once: the review gate acknowledged, a host smoke test, a drain, a database backup, a
symlink switch and a restart, **in that order**, with a twenty-second health poll that puts the previous release back
when the candidate does not answer. Everything below is about which process answers what, what a refusal is allowed to
leave behind, and what this machine can and cannot measure.

Two limits are recorded rather than fixed. **No systemd exists here**: the promotion restarts the unit through the
`systemctl` program, and the gate and the demo drive that program with a stand-in, so the restart is a recorded argv
plus the process the stand-in starts — never a boot. And **a rollback across a release that migrated the schema cannot
start the old `serve`**, because the unit runs `ExecStartPre=<exe> doctor --quick` through `current` and `doctor`
refuses a database a newer release wrote (ADR-0047's own refusal). That line is HS-049's and the refusal is a landed
contract, so this story makes the second poll *measure* it and the sentence name it, and leaves the fix to a follow-up.

## D1 — the verb is `holosmith upgrade <BINARY> [--acknowledge-review] [--root DIR] [--wait D] [--config PATH]`

A **host** verb like `build` and `smoke` (ADR-0080 D1, ADR-0081 D1), and the same argument: the spec says item 10's
steps are implemented by this command, so the command is the product and a `scripts/promote.sh` would be a second entry
point to a one-track pipeline.

`--root` is `server deploy`'s: an install tree the invoking user owns, which is how the gate and the demo drive the
whole pipeline without writing `/etc` or needing systemd. It prefixes the three paths this command *writes* — the
release tree (`<root>/opt/holosmith`), the admin socket's directory (`<root>/run/holosmith`) and the unit
(`<root>/etc/systemd/system/holosmith.service`) — and nothing it reads: the data root, the listen addresses and the
daemon come from the config as it is written, because they are facts about the machine.

There is deliberately **no `--dry-run`**. Every refusal in this pipeline is a measurement — a database's recorded
schema, a socket's answer, a candidate's own `version --json` — so a plan that ran none of them would print an order
the machine had not agreed to. What an operator inspects instead is the pipeline's own account: one printed line per
step, in the order the steps ran.

## D2 — a privileged install requires uid 0, and the review gate is a required word

A promotion writes the release tree, the symlink and the unit, and restarts a system service. `server deploy` writes
each of its own files as a separate `sudo`-prefixed child; a promotion that did the same would be a dozen escalations
for one command, each asking for the same credential. So the rule is one check with one sentence: as root, or with
`--root DIR`. Every write is then plain `std::fs` as the owner of the tree it writes, and `deploy`'s `sudo`/`install`
machinery stays where it is.

**The review gate is `--acknowledge-review`, and the pipeline cannot run without it.** Item 10's first step ends "review
the diff first (item 12); that review is the real gate", and a review is a human act; the flag is the *record* of it,
printed with the digest and the tag it acknowledges so the log says which binary was reviewed. Refusing without it is
what makes a promotion that skipped the gate impossible rather than discouraged: there is no other path in this command
to the drain. A check that some review artefact exists was refused — nothing in this build can tell a review from a
file that names one, so it would be an assertion that measured a filename.

## D3 — the candidate is verified from its bytes and from its own `version --json`

Five checks, in this order, before anything is asked of the machine:

1. the file is readable;
2. `builder::artifact_problem` says it is this machine's static build (`e_machine`, `ET_EXEC`, no `PT_INTERP`) — the
   same reader `holosmith build` uses on its own output, so a promotion and a build agree about what an artifact is;
3. `<binary>.sha256` exists and its first field is the digest of these bytes: the two files `build` writes beside each
   other, and a promotion promotes a *build*, not a hand-copied file;
4. `<binary> version --json` is run as **one temporary child**;
5. its `git_tag` is one path segment (`release::tag_problem`).

Step 4 is the one worth arguing. `deploy --ssh` reads the ELF header rather than asking a peer's binary (`remote`
records the rule: "a file that has not run cannot be asked `version --json`"). A candidate *can* run — it is this
machine's build, which step 2 has just proved — and it is the only honest source of the three facts a promotion needs:
the **version** the health poll looks for, the **git tag** the release directory is named after, and the **schema** the
candidate understands. `version --json` is a read: no daemon, no database, no config. And asking the candidate rather
than the promoting binary is what makes a promotion between two *different* releases possible at all.

## D4 — `/opt/holosmith/releases/<tag>/`, and `current` is switched by `rename` of a relative symlink

`release::ROOT` is `/opt/holosmith` — the FHS's home for an add-on application, a sibling of the `/etc/holosmith`,
`/var/lib/holosmith` and `/run/holosmith` the deploy already writes. A release directory is exactly what
`holosmith build --out releases/<tag>` produces: `holosmith` and `holosmith.sha256`, mode 0755 and 0644/0600-format.
One tag holds one binary: a directory that already holds *different* bytes under the tag being promoted is refused with
both digests named, which is `builder::place`'s rule one level out, and for the same reason — one release directory
holding two binaries is the ambiguity a rollback cannot resolve.

A switch creates `current.<pid>.tmp` beside the link and `rename`s it onto `current`: `rename(2)` replaces the old link
in one step, so every reader sees either the old target or the new one, never a moment with no `current`. The target is
written **relative** (`releases/<tag>`), so an install tree can be moved as a whole. A rollback writes back the link
target it read, **verbatim** — including one an operator made absolute — because putting back a different spelling of
it would be a change nobody asked for.

**HS-080 owns what the layout means from here**: pruning old releases to a configured count, `holosmith version`
reporting the tag it was run from, and `doctor` reporting the resolved release path and the link's integrity. What
HS-078 owns, and what is in `src/release.rs`, is the tree's creation, the switch, and the answer "what does the unit
execute".

## D5 — the unit's `ExecStart` is what is live, and a promotion makes it run through `current`

`release::unit_executable` reads the `ExecStart=` line out of the unit file, because that is the program systemd
actually starts — the only fact that says what a machine is running, and the reason a promotion can seed its own
`previous` release:

- **`current` exists**: its target is the previous release, and it must name one of this tree's releases with a binary
  in it. Anything else is refused, because a rollback would have nowhere to go.
- **`current` does not exist** (a machine deployed the old way, never promoted): the unit's binary is copied into
  `releases/<its tag>/` (its own `version --json` names it), and `current` is pointed at it **before** the candidate's
  release directory is written. That is what makes the first promotion's rollback possible, and it is why the seeding
  is not a convenience.
- **no unit at all**: refused. A promotion restarts a service; a machine that has none is a machine that has not been
  deployed.

The promotion then renders the unit through `deploy::unit_text` with `ExecStart=<install>/opt/holosmith/current/holosmith`
and `ExecStartPre=` the same binary, and writes it only when the bytes differ. `deploy`'s step 5 now prefers that same
path (`Release::live`) when `current` resolves, so a later `server deploy` neither undoes a promotion nor rewrites a
unit byte-identically — one function renders the unit's `ExecStart` for both writers, which is the only way the two can
be guaranteed to agree.

## D6 — the smoke run is called in process, before the drain

In process, because ADR-0081 D1 made the smoke run's exit code a *library* answer for exactly this caller: a second
`holosmith` process reading the same config would be a second entry point to one pipeline. Before the drain, because
the candidate is proven while the machine still serves the version it is replacing (spec order), and a candidate that
fails is one nothing was drained for.

The three codes are three different refusals, and the distinction is the one ADR-0081 drew: **0** proceeds; **1**
refuses ("the candidate failed its smoke test", with the run's own log tail in the sentence); **2** refuses with "this
machine cannot judge the candidate". A promotion is a machine-level act, so a machine that cannot run the test cannot
promote — and the promotion exits 1 in both cases, because `upgrade`'s contract is two-valued: 0 only for a candidate
that is live.

## D7 — the drain is the existing client, and its *first line* is acceptance bullet 5's refusal

The client's read loop was lifted out of `commands/drain.rs`'s private `ask` into one `pub(crate)` function both
callers use, so the two bounds (`ANSWER_TIMEOUT` for the first line — "is anything there at all" — then the wait the
operator asked for) are one implementation. The callback that sees each line answers whether to keep reading:
`holosmith drain` always does, so its behaviour and exit code are byte for byte what they were.

**"Draining is already in progress" is not `Ending::Refused`.** `Drain::run` calls `Control::begin()`, and when the
flag was already set it prints `state: already draining` and *runs on* to `Drained` — that is the drain's own
idempotent design (a second drain joins the first), and changing it would change a landed contract. So the promotion
refuses on that first line and stops reading: `drain::ALREADY` is one constant, the server writes it and the client
compares it, and waiting out somebody else's ten-minute drain in order to switch under a machine this command did not
quiet would be the opposite of a refusal.

`Drained` proceeds; `Expired` refuses (a turn outlasted the wait, and the machine stays draining); a failed exchange
refuses with the client's own sentence.

## D8 — the images are built before the drain, and the backup after it

The spec orders no images, so their place is ours, and it is the one that can fail safely: **before** the drain. A
build the daemon refuses then aborts with nothing drained, nothing backed up and nothing switched.

The database backup stays where item 10 step 4 puts it — after the drain — because acceptance bullet 1 lists that
order. The price is recorded: a failure *after* the drain leaves the machine draining, which is the drain's own state
(reads answer, creates and prompts refuse `503 draining`), and the sentence says so and names
`systemctl restart holosmith.service` as the one thing that clears it. Nothing in this build can un-drain a process
(that would be a second request word, and item 10's socket has one), so the honest answer is a sentence, not a retry.

## D9 — the backup is `VACUUM INTO backups/app-<ts>.db`, five kept, over the read-only opener

`db::Reader::snapshot(destination)` runs `VACUUM INTO ?1` on the **read-only** connection (ADR-0047's one read-only
reader): a promotion must not open the database read-write, because that open applies the migrations and would change
the very file the backup is about. Measured: SQLite allows `VACUUM INTO` on a read-only connection, and the destination
is a new file.

`backup::Snapshots` owns the second rotation: the directory (the same `<data>/backups` the project bundles live in), the
name (`app-<YYYYMMDDTHHMMSS>.db`, UTC, fixed width, so the name *is* the sort key — `Backups::prune`'s argument one
type over) and **five** kept, item 10's number beside the project bundles' seven. A snapshot is vacuumed to a temporary
name and `rename`d into place, so the name that exists is always a whole database; a second snapshot of the same second
replaces the first, because two snapshots of one instant differ in nothing and the name is what the rotation sorts by. A
failed backup refuses the promotion **before** anything is switched.

## D10 — the two images are read and built from the *candidate's* values

`images::Expectation`, `Kind` and `read` are parameterised by a `VersionInfo`, so a promotion reads and builds
`holosmith-proxy:<candidate version>`/`agent-image:<candidate version>` with `org.holosmith.binary-sha256` = the
**candidate's** digest and `org.holosmith.pi-version` = the candidate's pin. Every one of those values was `env!` (the
building binary's) before, and for a promotion that is the wrong release: the new boot asks the daemon for *its own*
version's tag, so an image labelled with the promoting binary's version is an image the new release never finds.

`images::outstanding` is one function (the read, proxy first, the read's own sentence as the reason) with two callers —
`server deploy`'s step 3 and the promotion — and it is empty exactly when both images already carry those labels.
Acceptance bullet 4's "only when the binary or a pinned version changed" is therefore one rule measured in both
directions, and the gate's `case_upgrade` measures its wiring end to end by promoting the same candidate twice: the
second promotion's `--build-arg HOLOSMITH_BINARY_SHA256` list is what the first one built, and no `docker build` runs.

## D11 — the health poll is 20 seconds, and it looks for a process younger than the restart

`config.control_listen` — not `control_origin`: no TLS, no `tailscale serve`, the same address `deploy`'s step 7 and
`doctor`'s port check ask — through `http::health::ask`'s own reader, polled to a 20-second deadline (a constant, and a
parameter below it so a test can make it milliseconds, `deploy::wait_for_health`'s shape).

An answer counts when **three** facts hold: `name` is `holosmith`, `version` is the candidate's, and `uptime_s` is no
older than the time since this command issued the restart (plus a second of slack). The third is what makes the poll
falsifiable, and it was a review finding: every binary this repository builds reports the same `CARGO_PKG_VERSION`, so
the version alone is satisfied by the *old* process still answering during a restart. `uptime_s` is already in the
payload (`holosmith status` reads it), and it is the one fact that says *this* process started after we asked.

## D12 — the rollback is the same switch in reverse plus the same restart, and the database is not restored

Before the switch the previous link target is captured; on a failed health check (or a failed restart) it is written
back, the service is restarted, and the poll runs again for the same window. The command exits 1 either way — a
rollback that restored service is still a promotion that failed, and a rollback that did not is the loudest thing this
command can say: the sentence then names `systemctl status`/`journalctl` and the possibility that `doctor --quick` is
what refuses the database (the recorded limit above).

The database is **not** restored. Expand/contract (ADR-0047) is what makes switching `current` back safe, and a restore
would delete the work of every session since the backup. The candidate's release directory survives the rollback: it is
the next attempt's, and it is the thing the operator already reviewed.

## D13 — a session the drain stopped comes back on the next boot

`reconcile::recoveries` gains a second kind of row: `stopped` with `status_reason = drain::SERVER_RESTART` — the
drain's own word, and the only policy that writes it. That row is moved to `starting` with `Trigger::Resume` (the
table's resume row, legal from `stopped`) and re-provisioned, exactly as a live row is; `Recovery::trigger()` chooses
the word per row so the pass's two loops cannot disagree about which rows are being brought back. That is spec item 10
step 6 in its own words — "sessions that were live before the drain return to `idle` on startup" — and acceptance
bullet 3.

It belongs to the **boot**, not to the promotion. A promotion is followed by a restart by construction, and the boot is
what knows the machine is back; nothing else may resurrect a `stopped` row, so the sweeper's, the cap's, the quota's,
the expiry's and the operator's stops stay stopped. The one ending where this does not help is a rollback to a release
built *before* this story, whose `serve` has no such rule: those rows stay `stopped · server_restart`, which HS-079's
own acceptance allows ("sessions survive the rollback in `idle` **or** `stopped`") and the next boot of any release
from this story onwards resumes. Writing the rows from the promotion instead was considered and refused: it would be a
second writer of the state machine's rows for one ending, and the spec says "on startup".

## What this does not measure

- **No systemd.** `systemctl restart` is a child, and the gate and the demo put a stand-in on `PATH` that does what
  systemd would do for this unit (stop what runs, start what `current` names). A real unit booting a real release on a
  real machine is not measured here; `deploy`'s `case_server_service` measures the unit's *bytes*, and HS-080's demo is
  where `systemctl status` shows the unit executing through `current`.
- **No provider call.** The smoke run's own limit (ADR-0081): the key proxy is started and joined, never exercised.
- **No second architecture.** The candidate must be this machine's build, so an aarch64 promotion is a case nobody ran.
- **Two builds of one release cannot be told apart by `/api/health`** beyond `uptime_s`: the payload carries the crate
  version, not the git tag. A promotion between two commits that share a version is verified by "a process younger than
  the restart answered", which is what the line says.
- **The rollback is not a verb.** `holosmith rollback` and "go back one version" are HS-079's; this story has the
  automatic switch-back the acceptance names.
- **The layout's meaning** — pruning, `version` from the release path, `doctor`'s report — is HS-080's.

## Consequences

- `holosmith upgrade` joins `build` and `smoke` as host verbs the spec's Commands table does not spell out; the table's
  `upgrade <binary>` row is now implemented, and `rollback` is the next story's.
- `images`' three readers take a `VersionInfo`, and `Kind::image_of` is the one spelling of an image's tag. Nothing
  about `server deploy`'s behaviour changed: it passes its own `VersionInfo::current()`.
- `deploy`'s step 5 renders `ExecStart` through `current` when a release tree resolves. A machine that has never
  promoted writes exactly the unit it wrote before.
- `reconcile` brings back one more kind of row. `serve`'s boot on a machine that has drained resumes what the drain
  stopped, which is what item 10 step 6 asked for and what the drain's reason word was for.
- One follow-up, recorded rather than fixed: `ExecStartPre=doctor --quick` refuses a newer database, so a rollback
  across a schema-advancing release cannot start the old `serve`. The two claims — ADR-0047's "a rollback must not need
  a restore" and the unit's pre-line — contradict each other, and the fix is HS-045's or HS-049's, not item 10's.
