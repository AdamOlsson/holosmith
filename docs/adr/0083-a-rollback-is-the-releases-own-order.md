# A rollback is the release tree's own order plus the promotion's restart

 closes item 10's second sentence — "in v1 these steps are implemented by `holosmith upgrade <binary>` and
`holosmith rollback` (see Single binary and CLI), not by a separate script" — with the verb left private
(`promote::rollback`, the automatic switch-back a failed promotion performs). What the verb needed that nothing
recorded is the one thing the acceptance says in four words: **"go back one version"**.

The rollback reuses everything a promotion already owns — the drain client, the atomic switch
(`Release::restore`/`switch`), the `systemctl restart`, the `/api/health` poll whose third fact is `uptime_s`, the
`draining` clause every post-drain refusal carries, and the schema measurement — and adds exactly one new idea, the
**order of a release tree**.

## D1 — `holosmith rollback [--to TAG] [--wait D] [--root DIR] [--config PATH]`, a host verb with no review gate

Item 10's Commands table row, and a host verb like `build`, `smoke` and `upgrade` (ADR-0080/0081/0082 D1): the
command is the product, so there is no `scripts/rollback.sh`.

`--root` and the privilege rule are `upgrade`'s, and they are now *one* function
(`deploy::privilege_problem(target, verb)`) because both verbs write the same three things — the release tree, the
`current` symlink and the unit — and restart the same service. `--wait` defaults to `10m`, the drain's own default,
because the drain is what governs the wait.

There is deliberately **no `--acknowledge-review`**. Item 10's first step ends "review the diff first (item 12); that
review is the real gate", and that is a **pre-promotion** gate: the whole point of a rollback is that a release passed
its health check and turned out to be wrong, so requiring an acknowledgement to undo it would make the emergency brake
unavailable at the only moment it matters. The refusal `upgrade` makes is untouched.

The exit code is **0 only when the target release is confirmed serving** — two-valued like `upgrade`'s, because a
rollback that restored nothing is a failure whatever else it printed.

## D2 — "the previous release" is the newest release **written** before the live one

The tree records no "previous" pointer, and no pointer would be honest either: a promotion writes a release directory
and switches a link, and an operator may `cp` a tree between machines — so a pointer would be a second source of truth
that every writer (promotion, rollback, deploy, an operator's shell) has to keep true.

What a tree *does* record is when each release was written onto it: the directory's modification time, which the kernel
sets when `Release::write` creates the directory (and refreshes when a re-promotion rewrites its digest). So:

- **`Release::history()`** — every directory under `releases/` whose name is one path segment (`tag_problem`) and that
  holds a `holosmith` binary, **newest first** by mtime, with the tag as the tiebreak so the order is total and two
  runs agree. A directory that holds no binary is not a release and is not somewhere a rollback may switch to.
- **`Release::older_than(tag)`** — the entry **after** `tag` in that list, i.e. the newest release strictly older than
  it. `None` when `tag` is the oldest, is the only one, or is not in the list at all — three different trees and three
  different sentences at the caller.
- The verb's default target is `older_than(current)`.

**A name order was refused, and it is worth saying why**: a release directory is named after `git describe`'s answer
(`f2b3886-dirty`, `0.1.0-3-gabc1234`), so sorting by name puts a hash above a version tag, and "one version back"
would mean "the build that happened to sort next".

** prunes by this list**, which is why the order lives in `release` rather than in a verb: `releases/` keeps the
newest N of `history()`, and the same function answers "which release am I running from". Recording a pointer instead
is recorded here as the road not taken: it would make the order a file that a `cp -r` of the install tree leaves
behind, and the month it silently disagrees with the directories is the month a rollback goes somewhere nobody chose.

## D3 — `--to TAG` names any release in the tree except the live one

`--to` exists because "one version" is not always what an operator wants: two steps back is a real thing, and so is
**forward again** after a mistaken rollback. So `--to` accepts any release `history()` lists that `current` does not
name — a *newer* one is legal, and means the same switch. (`upgrade --to` as a verb is not added; a promotion needs a
candidate, a review and a smoke run, and a release already in the tree needs none of them.)

The four refusals, all before the machine is asked anything:

| Tree | Refusal |
| --- | --- |
| no `current` | "has never promoted (`holosmith upgrade <binary>` writes the first release)" |
| `current` is not a symlink | "a rollback switches the `current` link a promotion writes" |
| the link names something outside `releases/<tag>` | "cannot say which release is behind a link to somewhere else" |
| one release, or a tag the tree does not hold | the sentence lists what the tree *does* hold, so an operator can copy one out of it |
| `--to` names the live release | "would be a restart with no release changed (`systemctl restart` is that command)" |

A tag is one path segment (`release::tag_problem`), so `--to releases/one` is refused as a *tag* rather than silently
stripped into one — one rule, one spelling.

## D4 — the schema refusal is the promotion's own measurement, read before the drain

Acceptance bullet 2 — "refuses when the previous release's schema expectation is violated, with a clear explanation" —
is ADR-0082's recorded limit seen from the other side: the unit runs `ExecStartPre=<exe> doctor --quick` through
`current`, and `doctor` refuses a database a newer release wrote (`schema::Verdict::Newer`, ADR-0047), so a rollback
across a schema-advancing release could not start the old `serve` anyway.

So the measurement is not a second one. `promote::refuse_older_schema` was `(config, candidate)`; it is now
`(config, subject, schema)` — `db::recorded_without_migrating` (the **read-only** opener: a rollback must not migrate
the file it is deciding about) against `schema::verdict` — and both verbs call it with their own subject
("the candidate, /path (sha256 …)" / "releases/old (/path)"). The sentence carries the verdict's own words and now
also names the pre-line, because that is the mechanism an operator will meet if they skip the check:

> … reports schema 7 and the database at … records 8: … is older than the database, and migrations are forward-only …
> Nothing was changed — build a release from the current tree (`holosmith build`), or restore a database this release
> understands. **The unit runs `doctor --quick` before `ExecStart`**, so this release would refuse to start on this
> database even if `current` named it.

The refusal happens **before the drain and before the switch**, which is what makes it impossible to skip: the gate
asserts `current` unmoved, no `systemctl restart` recorded and nothing on the health port asked, and
`tests/rollback.rs` asserts the `Steps` trace is **empty**.

## D5 — the drain *joins*, and a post-drain failure says the machine is draining

A promotion refuses a drain it did not start (ADR-0082 D7): switching under a machine it did not quiet would be the
opposite of a refusal. A rollback must not, and the asymmetry is the point:

- the state "draining" is one the rollback is the **fix** for — a promotion that failed after its drain leaves exactly
  that, and an operator's next act is this command;
- `Drain::run` is idempotent by design (a second request joins the first and runs on to `Drained`), so joining costs
  nothing but honesty;
- nothing clears a drain but a restart, which is what a rollback does.

So `Real` gains a policy (`Real::new` for a promotion, `Real::joining` for the verb) and `DrainAnswer::Already` means
*proceed* to one and *refuse* to the other. Both readings are tested.

Everything before the drain changes nothing and says so; everything after it carries `promote::draining`'s clause
(`… — this machine is draining until it is restarted (`systemctl restart holosmith.service`)`), now at three call
sites rather than one. The one write before the drain is the **unit** (D6), and it is safe there for the reason it is
made at all: its bytes are `deploy`'s, it is written only when they differ, and a unit that cannot be written has to
refuse while the machine is still serving rather than after it has been drained for nothing.

## D6 — the unit is fixed, not refused, and by the same writer

`ExecStart` names `<install>/opt/holosmith/current/holosmith` after any promotion or deploy (ADR-0082 D5), so normally
a rollback writes no unit at all. The exception — a `current` that resolves while the unit names a raw binary (a tree
copied onto a machine, a hand-edited unit) — is **rewritten, not refused**: a rollback whose switch the unit does not
follow could not work, and refusing would take the emergency brake away from the machine that needs it. The write is
`promote::write_unit` (the bytes `deploy`'s step 5 renders, byte-compared so a promoted machine writes nothing), which
grew a prefix so the verb's line says `rollback: unit:`.

It is written **before the drain**, unlike the switch: the check is a refusal and a repair that touch no release, and a
unit that cannot be written must refuse while the machine is still serving rather than after a drain it was not needed
for. The refusal with no unit at all is the same shape — a machine that has not been deployed is turned away before
anything is stopped.

A machine with **no unit** is refused before the drain: a rollback restarts a service, and a machine without one has
not been deployed (`server deploy` writes it).

## D7 — one health poll, one restart-and-poll, and a loud ending that does not guess twice

`promote::poll`'s rule — an answer counts only when `name` is `holosmith`, `version` is the target's **and** `uptime_s`
is younger than the restart — is the only thing that makes the poll falsifiable on this machine, where every binary
reports the same `0.1.0` (ADR-0082 D11). So it is one function, and the restart→poll sequence was lifted out of
`serve_with`/`rollback` into `promote::restart_and_poll`, with **three** call sites: a promotion's candidate, a
promotion's automatic switch-back (whose lines stay under `upgrade: rollback:`), and the verb (under `rollback:`).

A rollback whose own poll fails exits 1 with a sentence that says four true things and offers no fifth:

1. the restart's own failure first, when there was one;
2. `systemctl status holosmith.service` and `journalctl -u holosmith.service` — where the service's reason is;
3. the release that is now `current`, plus `holosmith rollback --to <the release it came from>` — which is legal
   because `--to` accepts a *newer* release, and is the way forward again;
4. that the database was not restored, and that a target older than the database would be refused by its own
   `doctor --quick` before `ExecStart`.

The link is **not** put back. A promotion's automatic rollback does move it, because it is undoing a switch nobody
wanted; a verb's rollback has already moved the link to the thing the operator asked for, and a second guess would
leave them unable to say where they are.

## D8 — nothing is written to a row and no container is touched

A rollback drains, so the sessions it stops carry `server_restart` — the drain's own word — and `reconcile`'s existing
rule resumes them on the next boot, whichever release that boot is running (ADR-0082 D13). So acceptance bullet 3
("sessions survive the rollback in `idle` or `stopped`, never `error`") is a claim about **not adding a second path**:
the verb writes no row, stops no container and kills no turn. The one ending where this leaves a session `stopped` is a
boot of a release built, which has no such rule — and `stopped` is one of the two states the bullet
allows.

No backup either: a rollback changes no database, and the promotion that preceded it took the one this state is
recoverable from (`VACUUM INTO backups/app-<ts>.db`, five kept).

## What this does not measure

- **No systemd.** The restart is a child. `case_rollback`'s `systemctl` stand-in stops what runs and starts what
  `current` names, and — the part that matters — records `readlink -f current` **at the moment it restarts**, which is
  how "drains first, points `current` at the previous release, restarts" became a measurement rather than line order:
  a verb that restarted first would record the old target, and the case fails with `case_rollback/switch/before`
  (verified by mutating the code and watching it fail).
- **No second architecture**: a release directory holds this machine's build, because `version --json` has to run.
- **No provider key.** A rollback runs no turn, so nothing here needs one.
- **The order is a filesystem fact.** `cp -a` keeps a tree's order; a `cp -r` gives every release the moment of the
  copy, so the order collapses to the tag tiebreak (a name order, which is the thing D2 refused). Nothing in this
  build can notice, and a recorded pointer was refused rather than added (D2).
- **The verb's own poll failing** is measured by the stand-in choosing not to start a serve
  (`HS_BREAK_RESTART=1`), not by a release that boots and then dies.

## Consequences

- `holosmith rollback` joins `build`, `smoke` and `upgrade` as host verbs; the Commands table's second row is
  implemented and phase 4 has one story left.
- `release` owns the tree's **order** (`history`, `older_than`), and prunes by it rather than inventing a second.
- `promote` owns the sequence both verbs run: `poll`, `restart_and_poll`, `refuse_older_schema`, `draining`,
  `write_unit`, and `Real`'s two policies. Three things about `upgrade`'s output changed, and none of its behaviour:
  the schema refusal's wording ("Nothing was changed — … " and its new `doctor --quick` clause, with the remedy as a
  parameter so each verb offers its own); the prefix on the automatic switch-back's restart and health lines
  (`upgrade: rollback:` instead of `upgrade:`); and the giving-up line for a **restart that never happened**, which
  `serve_with` used to print as "no answer with … in 20s" (a claim about the poll for a poll that never ran) and which
  `poll` now prints only when it really gave up. `case_upgrade` measures the first two; the third is **not measured**,
  because its stand-in never fails a restart, and it is recorded here rather than left as a silent widening.
- `deploy::privilege_problem` is the two release verbs' one privilege rule; `commands::upgrade::is_root` is gone.
- The gate gained `case_rollback` — four arms, **no Docker and no image**, because a rollback runs no smoke test and
  builds nothing — and `tests/rollback.rs` measures the order, the target choice and both endings over the `Steps`
  seam.
