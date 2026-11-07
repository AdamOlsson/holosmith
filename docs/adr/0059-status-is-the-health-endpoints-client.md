# `status` is the health endpoint's client, and `uninstall` owns its own words

HS-050 is the spec's Commands table rows 9–11: `holosmith status`, `holosmith logs [-f] [--session ID]` and
`holosmith uninstall [--purge]`. All three are readers or a teardown of state earlier stories already own, so this
ADR records the readings and the one new shape, not a design invented for the occasion.

## D1 — `status` prints one `key value` line per fact, each with three arms

The shape is `holosmith version`'s (`key value`, one fact per line), and every value has `doctor`'s three arms
(ADR-0054): **measured**, `unknown: <sentence>` and `fail: <sentence>`. A `status` that printed `0` where it could
not look is the one thing the whole build refuses to do — `doctor`'s `unknown` is the precedent, and ADR-0044's
third answer is why a machine with no systemd and no `tailscaled` can still be described successfully.

Lines, in this order, each from the reader that already owned it:

| line | reader |
| --- | --- |
| `role` | the config file |
| `version` | `version::VersionInfo::current()` |
| `uptime` | `GET /api/health` — `Health::of`'s `uptime_s`, `Runtime::uptime_s`'s one owner |
| `sessions <status>` ×6 | the `sessions` table, through a read-only open |
| `proxy hs_egress`, `proxy hs_key` | `proxies::running(config, machine)` |
| `machines` | nothing yet: the registry is HS-068 |

`role` and `version` are **this machine's config and this binary**, never the health payload's copies. Both are
facts a CLI always has, so a `status` on a stopped machine still describes the machine; a line that took them from
the server would print nothing at all precisely when an operator runs `status`. The limit is recorded rather than
hidden: `version` is the version of the binary the operator ran, and a hand-started older `serve` on the same
machine would have its own.

`machines` is `unknown:` with a sentence naming **HS-068**, not `none` and not `0`: "no machine has registered" and
"the thing that would register one does not exist" are different facts, and on a phase-2 machine only the second is
true. `GLOSSARY.md`'s *Health endpoint* already said the command "also asks other machines"; this is that sentence
until there is something to ask.

**The exit code is 1 iff a value is `fail`** — a read that errored (an unreadable database). `unknown` and
`not running` are not failures: a stopped machine is a machine `status` is *for*, and ADR-0054's rule (only `fail`
changes the code) is what makes that expressible.

## D2 — uptime is asked, not recorded

`status` is a **second** process, so the server's start time has to be read from somewhere. The candidates were: a
file `serve` writes at boot, systemd's `ActiveEnterTimestamp` (`systemctl show -p … --value`), and the health
endpoint.

The request wins on three counts and each is a measurement this repository has already made:

- **The fact has one writer.** `Runtime::uptime_s` is the process's own clock, and `GET /api/health` already
  publishes it (`Health::of`); a start file would be a *second* record of the same fact, and the two could disagree
  after a restore or a hand-copied data root.
- **It works for both shapes of `serve`.** A unit and a hand-run process answer the same request; the unit's
  timestamp answers only on a machine whose systemd loaded it, which is not the `--root`, hand-run, or
  no-systemd machine this build supports.
- **No `unsafe`, no `/proc`.** Reading another process's start time from `/proc/<pid>/stat` needs either its pid
  (a file, again) or a `clock_gettime`-style syscall this crate's `unsafe` ban forbids.

The cost is recorded: a machine whose server is not answering has no uptime, and the line says so in its own words
(`uptime unknown: … so this machine is not serving on 127.0.0.1:8080`). That is also the line that answers "is it
running" — the command prints no separate boolean, because a boolean beside a fact that already carries the answer
is a second spelling of it.

## D3 — the counts are six lines, and "live/stopped" is why

The acceptance says "live/stopped session counts", and `GLOSSARY.md` defines **Live** as exactly `starting`, `idle`
and `running` while **Stopped** is one of the other three. So:

- a two-number report would have to put `error` and `deleting` somewhere, and calling an `error` row "stopped" is
  the kind of lie `doctor`'s third arm exists to avoid;
- printing only `stopped` and `live` would hide the `error` rows an operator most needs to see.

One line per `Status`, in `Status::ALL`'s order, with the enum's own words — and the reader is **one** statement,
`SELECT status, count(*) FROM sessions GROUP BY status`, because a status with no rows is absent from the answer and
the enum is the list of statuses. The `live` total stays `live_count`'s (the cap's and the purge refusal's), not a
sum computed in a command.

The read is a new `db::Reader`: a **read-only** open (`SQLITE_OPEN_READ_ONLY`), with `db::recorded_without_migrating`
refactored onto it so the binary has one opener for "read a database without changing it". ADR-0047's promise was
about the *reader*, not about `doctor`; a `status` that migrated a machine's database would be a report with a
write. A missing database is six zeros (a machine that never served holds no sessions), and an unreadable one is
`fail`.

## D4 — `logs`: journald through one child, a session's log through one file

The spec says "Server log from journald", so the server arm is one child: `journalctl -u holosmith.service`, `-f`
appended when asked, **stdio inherited** (following is the child's, and a captured pipe would deliver nothing until
it exited) and **no `-n`** — any window is a truncation this command cannot label, and journalctl's own pager is
what an operator navigates. A child that cannot be started is one sentence from us; a child that exits non-zero is
**its** answer and **its** code, because journald's own "no journal" line explains the machine better than a generic
failure would.

A session's log is `<data>/sessions/<id>/session.log`, written by `exit::Exits::record` (the launch line, plus the
stderr's tail when a container failed), so the reader is that writer's inverse and needs **no program**: `std::fs`,
and `-f` is a poll loop (`TICK` = 200 ms). A `tail` child there would make the command's one unmeasurable
dependency a program a minimal machine may not have, for a file this build owns. The id is checked by
`sessions::is_id` before it becomes a path component — the same rule the API's own route applies — and an id with no
log is one sentence naming the path.

`--session` and `-f` combine, which is the acceptance's only claim about them together.

## D5 — `uninstall` is not a deploy, and does not print its words

ADR-0056's five words (`satisfied`, `needs`, `changed`, `skipped`, `failed`) are a **deploy** contract: a step that
read a state and applied a difference. A teardown's questions are different — was there one, and is it gone — so
`uninstall` owns four words of its own: `removed`, `kept`, `nothing`, `failed`, on lines shaped
`uninstall: <what>: <word>: <sentence>` after a header naming the tree and its privilege. Reusing the deploy's words
would have been a second user of a shape nobody documented for it, and the first thing they would lie about is
`nothing`: a deploy has no word for "there was nothing to do because nothing was ever installed", which is a
teardown's commonest answer and the one the acceptance's idempotency asks for.

## D6 — what it removes, in what order, and what it keeps

1. **The unit** — `systemctl disable --now holosmith.service`, then the file (`rm -f`, `sudo`-prefixed on a system
   install), then `systemctl daemon-reload`. It goes first because it is what would restart anything.
2. **The two proxies** — `docker rm --force hs_egress hs_key`. The read is **existence**, not `running`:
   `--restart unless-stopped` keeps a stopped container's name and its definition, so a teardown that removed only
   running ones would leave two containers behind.
3. **The machine's `egress` network** — read from `docker network ls` matched **exactly** (Docker's own filter is a
   substring match), removed after the containers, because a network with an endpoint refuses to be removed.
4. **The data** — kept, or purged (D7).

It keeps the data root (`app.db`, every session's `session.log`, `secrets/keys.toml`, `secrets/vapid.toml`), the
projects and their worktrees and branches, the two images — and **the sessions' own containers and `agents_*`
networks**, which belong to the rows the kept data describes: a boot's reconcile owns them (ADR-0053), and a plain
`uninstall` that killed a live session would destroy a turn it was never asked about. The `data: kept:` line says
so, and `--purge` is the arm where they go.

**`systemctl`'s answers are remarks, not failures.** An unloaded unit answers `disable --now` non-zero, a machine
whose systemd is not running answers the same way, and neither is a reason to stop a teardown whose deliverable is
the file. The privileged read is `systemctl is-enabled` when no unit file is there — a unit `systemctl enable` linked
from `/run/systemd/system`, or one left loaded after a hand removal, is still this machine's to remove, so the
file's absence is *confirmed* rather than believed.

`--root` is `deploy::Target`, unchanged: it prefixes every installed path, decides privilege, and a unit under
`--root` is not one systemd can read, so that arm asks systemd nothing and its line says so.

## D7 — the purge refuses before it removes anything, and sweeps the rows' own names

`--purge` removes the data root, and because the rows that name them are going, it first removes the containers and
networks the rows name (`s_<id>`, `agents_<id>`) — names derived from an id, never a listing, so nothing an operator
named can be swept by accident. Never a branch, never a worktree, never an image: a purge that removed the
operator's projects would be a delete of something it does not own. A daemon that refuses is counted in the line's
sentence rather than failing the run, because the data root is the deliverable.

The refusal — `live_count() > 0` and no `--force` — happens **before any child runs**, and is a refusal rather than a
report: one sentence on stderr, exit 1, nothing removed. A run that removed the unit and then discovered it could
not purge would leave the machine half uninstalled and the operator with no server. `--force` `requires = "purge"`
at the command line, because a `--force` with nothing to force is a flag nobody meant. The live set is
`live_count`'s three statuses, printed, so the sentence says what it counted.

Idempotency is the acceptance's "a second run changes nothing" in the teardown's spelling: every step reads first,
a machine with nothing installed prints `nothing` and exits **0**, and a failed step prints the resume command (the
command that just ran, with its own flags made absolute) because re-running is the recovery.

## D8 — three cases, one per command, and what this machine cannot measure

`case_status`, `case_logs` and `case_uninstall`, each measuring its own subject rather than a neighbour's:

- `case_status` boots its own `serve` **on the port its config names** — `start_serve` always passes `--port 0`, and
  `status` asks the address in the config, so a shared boot would leave the uptime line honestly `unknown` and the
  case would skip on the thing it exists to catch. It asserts role and version against the boot's own answer, uptime
  against `/api/health`'s `uptime_s`, the six counts against the sessions list's own length, both proxy lines against
  the daemon, and that the machines line names HS-068.
- `case_logs` needs no server: a `journalctl` stand-in on `PATH` records the argv and prints two lines, and a data
  root this case wrote holds a session log. The `-f` *follow* is `logs.rs`'s unit test — a line appended after the
  first read is printed — because a shell case can only prove that the flag is passed on.
- `case_uninstall` drives `--root`: the offline half builds the install tree by hand, removes it, and asserts the
  second run's `nothing`; the online half (daemon and image) creates `hs_egress`, `hs_key` and the `egress` network,
  asserts they go while the data stays, and restores the network the rest of the gate needs; the purge refusal's
  live row comes from a boot of its own over a database this case seeds, and `--force` then takes the tree.

**What this machine cannot measure**: no systemd, so no unit is loaded and `disable --now`/`daemon-reload` are
`tests/uninstall.rs`'s recorders (and a privileged run's unit *file* is untestable here, because `/etc/systemd/system`
is not this user's to write — the case's `--root` arm and `docs/build.md`'s by-hand procedure are the two proofs);
no `journalctl` for a unit, so the server arm's real journal is a stand-in's; and no host registry, so `status`'s
machines line names HS-068 instead of a health it cannot ask for. Those three are the ship note's "said plainly",
not gaps found later.
