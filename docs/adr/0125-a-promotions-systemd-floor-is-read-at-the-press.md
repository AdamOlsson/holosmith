# A promotion's systemd floor is read at the press, and a version nobody measured refuses nothing

ADR-0110 measured what a promotion's job is: a transient user unit whose two output properties are
`StandardOutput=append:` and `StandardError=append:`, which is systemd **240**'s spelling, quoted there from systemd's
own `NEWS` under `CHANGES WITH 240`. The same ADR recorded the gap that left: nothing in the tree read a version, so a
machine on 239 passed `doctor --quick`, installed, booted and served sessions like any other, and met the floor at the
first press — where `systemd-run` refused the two properties and `update::start` answered with its sentence for a start
that did not take, naming the child's status and first stderr line and ending "this one did not start". Every word of
that is true, and none of it says the machine is older than the floor: the one machine that cannot promote was told it
has a broken launcher.

So the version is read, and it is read **where the press is refused**.

## D1 — at the press, never at boot and never in `doctor`

A 239 installs, boots and serves sessions, and only the job a promotion starts needs the two `append:` properties. A
`doctor` finding would have made that a boot failure rather than a promotion failure, because `doctor --quick` is the
unit's `ExecStartPre`: the same sentence that should stop one press would have restart-looped a machine whose sessions
are fine. `preflight::Check` is untouched, and `doctor` gains no row — `update.rs`'s header already says why it reports
no program for `systemd` at all: what it reads as `systemd` is the unit **directory**.

The three places a press becomes a job ask it, and each says why it must:

- **`POST /api/update`** — after the release tree's `409 not_deployed` and **before** the candidate's five `git`
  children: a machine under the floor cannot promote any tree, so the five children are not worth starting. The refusal
  is a code of its own in the update family, `409 systemd_too_old`.
- **`promote::execute`'s step 0** — the only path `holosmith upgrade` has, beside the two facts that already refuse a
  promotion before anything is asked of the machine.
- **`pass::pass`** — after the candidate is resolved and before the room check, so a machine that cannot promote is not
  asked to spend the gate's three quarters of an hour proving a tree the launcher would then refuse. Its report entry
  is `systemd refused`.

`GET /api/update/candidate` does **not** ask it: that route is the page's, and a second child per page load would buy a
page the ability to hide a button that a press answers properly anyway. The page renders an unknown code as
`code: sentence`, which is what a machine that cannot promote at all until its distribution upgrades should show.

## D2 — one reader, one child, and no `--user`

`update::FLOOR` is `240`, the number ADR-0110 recorded from systemd's own `NEWS` and `docs/requirements.md` states.
`update::below_floor(tools)` asks `Tools::systemctl` **once**:

```text
systemctl --version
```

with stdin null, and reads the major version from the first line's field after the program's name:
`systemd 255 (255.4-1ubuntu8.18)` is 255, and so is a `systemd 255.4` an older release printed. The floor is inclusive —
240 meets it, because 240 is the release that has the two properties.

**No `--user`, deliberately.** `--version` is answered by the client itself, without asking the bus, so it answers on
the machines this tree supports and does not reach systemd on: a `serve` started from cron, one in a container, one
whose user manager is not the one this process would talk to. And the client and the manager it talks to are one
package, so the binary's own version *is* the version of the manager behind it — the question is the package's, and a
connection to the bus would add a way for it to fail without adding anything it could measure.

## D3 — a version nobody measured refuses nothing

A `systemctl` that could not be run, one that exited non-zero, and a first line that is not `systemd <number>` are three
ways of **not knowing**, and they all answer "no refusal". ADR-0044's rule is the reason: *we could not measure* is not
*we measured*, and a press refused because a child would not start would be a machine with a working systemd told its
systemd is too old. So the unmeasurable arms are silent and the press goes on to what the next step says about the
machine.

That rule is what keeps the gate honest, too: the `systemctl` stand-ins `scripts/verify.sh` gives `case_upgrade` and
`case_web_update` answer `--version` with no output, so those cases measure the rest of a promotion while this check
measures nothing — and the same is true of a development container with no `systemctl` on `PATH`.

## D4 — the sentence, and the one move there is

One sentence, in the voice the update family's refusals already use, naming the four things an operator needs: the
version it found, the floor, the half of the machine that is fine, and the only thing that can change it.

```text
this machine's systemd is version 239, and a promotion's job needs 240 or newer: the job's two output properties are
`StandardOutput=append:` and `StandardError=append:`, which a 240 has and this 239 refuses, while everything else on
this machine — the install, the boot and the sessions — works. Nothing in this tree can supply a newer systemd: take
the one your distribution packages, 240 or above, and start the promotion again
```

The code is `systemd_too_old` and the status is `409`, the family's status for a machine that could take the request on
another machine or after a change that is not this request's: `not_deployed` is the same shape — the route exists, the
machine is what refuses, and the sentence names the one act that repairs it. `update::below_floor` is the only place
the sentence is written, so the route, both verbs and a pass cannot spell one version four ways.

## What is not measured

- **A real systemd 239 refusing the two properties.** ADR-0110's measured section already records that the 239 half is
  quoted from `NEWS` rather than measured, because only a 239 can show it; this decision does not change that. What is
  measured here is what the tree does with a version it reads, over stand-ins that answer `239` and `240` and `255`.
- **A whole press on a 239.** The route's refusal is a unit test over a stand-in manager, and the verb's is
  `tests/upgrade.rs` over the same. A press on such a machine would end before the unit was spawned, which is the fact
  both of those assert, and no gate case can run on a machine that is not one.

## Consequences

- A machine under the floor is told the version, at the press, by a `409 systemd_too_old` — and never at boot, so a
  239 keeps serving sessions.
- `doctor`, `preflight` and the unit file are unchanged: nothing about a boot depends on the version.
- The launcher's refusal for a start that did not take keeps its place for every other way `systemd-run` can fail; it
  is no longer the first thing a machine under the floor hears.
- `scripts/verify.sh`'s two systemd stand-ins answer `--version` with nothing, which lands on D3's arm: an unmeasurable
  version, and those cases go on to measure what they were written for.
