# The release tree keeps a count, and says which release is running

HS-080 closes item 10's layout paragraph — "releases live in `releases/<tag>/` with a `current` symlink that systemd
runs" — for the state the paragraph is *about*: **two releases at once**. HS-078 built the tree and the switch, HS-079
gave it an order; what was still missing is what the layout means once there is more than one directory in it — which
one is live, which ones are kept, and how a machine states both.

Two of the acceptance's five bullets are measurements of what HS-078 landed (the directory shape, the atomic switch), so
this story adds three facts and one sentence. The three facts are a count, a report from the running file, and a check.

## D1 — the prune is the promotion's last step, and a prune that cannot delete is a warning

`promote::execute`'s step 6 writes the release and switches, step 7 restarts and polls. The prune is **step 8**, inside
`serve_with`'s success branch, so three things follow and each is a decision:

- a promotion that failed *before* the poll removed nothing, because it never reached step 8;
- a promotion whose candidate never answered removed nothing either — the automatic rollback has just put the previous
  release back, and that tree is what an operator reads while diagnosing it (the candidate's directory is there, the
  rollback target is there, nothing is gone);
- a machine that is now serving a new release pays for the prune.

The four alternatives were considered and refused:

- **At boot.** `serve` would become a second writer of the tree, and a boot is not an operator's act: a machine that
  restarts in a loop would prune to a count nobody chose at a moment nobody asked for. The reconcile pass (ADR-0053) is
  the only thing `serve` does to state it did not create, and that is because a crashed run leaves live rows.
- **In `holosmith build`.** A build writes a release directory without promoting it — on a machine that may not even be
  the one that will run it — so a build that pruned would delete releases on a tree it is not making live.
- **A verb.** `holosmith release prune` is a second entry point to one function and a reason for an operator to run
  half a promotion (a prune with no switch). Item 10's Commands table has no row for it.
- **At the start of a promotion.** A promotion that then failed would have deleted releases for nothing.

**A prune that cannot delete is a warning on stderr and the promotion still exits 0.** The product of `upgrade` is a
candidate that answers `/api/health`, and it does; the leftovers are directories. Exiting 1 would tell an operator whose
machine is serving correctly that its promotion failed — and the sentence they would act on is "roll back". So
`holosmith: warning: upgrade: prune: <sentence>` names the directory it could not delete and what it removed before it
stopped, and `releases::prune` stops at the first failure rather than continuing past it, because a prune that carried
on would report a tree it had not measured.

The line has one shape in both arms, so a script greps a stable prefix:

```
upgrade: prune: kept 2 of 4 (keep_releases = 2); removed: releases/old, releases/older
upgrade: prune: kept 2 of 2 (keep_releases = 5); removed: nothing
```

## D2 — `keep_releases` is the count, its floor is 2, and the two a rollback needs are reserved first

A new config key `keep_releases` (`u32`, default **5**; `KNOWN_KEYS` 36 → 37, the golden file moves in the same
commit). **Below 2 is refused** at load, and that is the acceptance's "never the live one or the rollback target"
spelled as a floor:

> `keep_releases: must be at least 2: a prune always keeps the live release and the one behind it, so a smaller count
> would delete the rollback target (and `0` is not `keep everything` either — raise the number instead)`

`0` and `1` are therefore refused rather than given meanings: `0` reads as "keep everything" to an operator and as
"delete everything" to a loop, and `1` is a tree a rollback cannot go back in — the one thing the layout exists to
prevent. An operator who wants everything raises the number; five, seven and a hundred are all the same code path.

The rule, as `Release::prune(keep)` implements it:

1. **protected** = the tag `current` names, and `Release::older_than` it — the release a rollback would choose;
2. **kept** = the protected ones (they are counted in `keep`, not added to it) plus the newest
   `keep_releases − |protected|` of the rest of `history()`;
3. **removed** = every other `history()` entry.

So the tree holds exactly N afterwards (when it holds at least N), and a tree whose `current` is *not* the newest — an
operator's switch, a hand-seeded tree — still keeps exactly N and never the live one. **"The newest N" alone was
refused** for that reason: it deletes the release the machine is running the moment `current` is not the newest, which
is precisely the state the acceptance's second clause forbids.

`prune` also floors itself at the protected set, so a caller that passed `0` cannot delete the machine's way back; the
config's refusal keeps that unreachable, and the function's own clamp keeps it untrue even if it were reachable.

**A hand-seeded directory that is a release is a release.** `releases/old`, copied in by an operator or written by an
older promotion, is in `history()` and may be pruned like any other — "pruned to a configured count" cannot mean "except
the ones a person put there", or the count would be a suggestion. What is **never** touched is a directory the tree does
not recognise: a name that is not one path segment (`tag_problem`), or a directory with no `holosmith` in it, is not a
release, is not counted, and is not deleted — an operator's `releases/notes/` survives, and the gate asserts it.

## D3 — `version` reports the directory the **running file** is in

`release::running_tag(&Path) -> Option<String>` is the rule, and it is pure: the file is named `holosmith`
(`release::BINARY`), its parent passes `tag_problem`, and its grandparent is named `releases`. `VersionInfo::current()`
asks it about `std::env::current_exe()`, which on Linux reads `/proc/self/exe` — a path the **kernel resolves**, so a
binary systemd started through `current/holosmith` reports the release directory it is really in rather than the link.
That is the acceptance's "so the live version is unambiguous", measured, and it is a **different fact from `git_tag`**:
the tag is what the build was made from, and this is where the file is. They agree on a machine a promotion owns and
disagree exactly where it matters — a hand-seeded `releases/one/holosmith` is a copy of any binary (the gate's fixtures
and `case_rollback`'s `one|two|three` are all copies of the artifact) and reports `one`.

Both shapes carry it, and both are fixed-shape rather than conditional:

- **text**: a fifth line, `release <tag>`, or `release -` when the file is not in a release directory (a build in
  `dist/`, a test binary). The line is always printed, because a script that read `sed -n 's/^release //p'` must not
  have to count lines to know whether this machine has promoted.
- **json**: a seventh key, `"release": "v0.1.0"` or `"release": null`.

The field is `Option<String>` with `#[serde(default)]`, and that is not tidiness: `VersionInfo` is the **reader** in
`promote::ask_version`, which deserialises a *candidate's* `version --json`. A candidate built before this key existed
has no `release` in its output, and a required field would make every older binary unpromotable — a promotion that
refused a file because *this* release added a report. A missing key is `None` there.

## D4 — `doctor`'s `release` finding fails only where the unit executes through the link

`doctor::Tools` gains `release_root` and `unit_path` (the two install-tree paths beside `config_path` and `admin_dir`,
which exist for the same reason: a `--root` run must read *its own* tree), and `deploy` sets both from the `Target`.
`preflight::Check::Release` is the twelfth word, and the finding is asked in **both** depths, because `--quick` is what
the unit runs as `ExecStartPre`.

One function, four arms, and the verdict is not "is the tree tidy" but **"can systemd start this machine"**:

| state | outcome | sentence |
| --- | --- | --- |
| `current` resolves to `releases/<tag>/holosmith` | **pass** | the tag, and the **resolved** path (`std::fs::canonicalize`), plus whether the unit executes through it |
| it does not resolve, and the unit's `ExecStart` names this tree's `current/holosmith` | **fail** | which way it does not (missing / not a symlink / names something outside `releases/<tag>` / names a tag with no binary) and the hand fix, `ln -sfn releases/<newest> <root>/current` — or `rm -rf` first when `current` is a plain entry, because `ln -sfn` refuses a directory |
| it does not resolve, and the unit's `ExecStart` names another program | **pass** | the state, and that the unit runs something else, so nothing here is executed |
| it does not resolve, and no `ExecStart=` can be read (no unit file, or one without the line) | **unknown** | the state, and that there is no unit file to say what systemd would run |

The failure is deliberate, and the argument is the pre-line's own: `ExecStartPre=<exe> doctor --quick` runs the binary
the unit's `ExecStart` names, so a `current` that does not resolve is a machine systemd **cannot exec at all** — the boot
the pre-line exists to protect has already failed. Reporting `pass` there would be the one thing `doctor` must never do:
claim a machine can serve when it cannot. The third arm is the machine that has **not promoted**: `server deploy`
installs a binary and writes `ExecStart=` to it, and the first promotion is what points the unit through `current`, so a
missing or broken link there is a tree nobody switched to — and refusing a boot over it would break the first deploy of
every machine, which is the same argument ADR-0054 makes about `--quick` in general.

**`deploy`'s step 1 exempts `Check::Release`** from refusing, beside `Ports` and `Perms`, and for their reason one
finding over (ADR-0058): a broken `current` is a state step 5 **repairs**. `Release::live(target)` names nothing when
the link does not resolve, so step 5 renders the unit for the binary this run is running — the machine boots again on a
plain path — and a refusal at step 1 would make that repair unreachable. The line gains one clause naming it
(`LINK_NOT_LIVE`). `serve` is unaffected: it refuses on `preflight::check`'s three, never on this list.

`release::Release::live` is the reader for "does it resolve", so there is no second spelling of what is live: the
finding asks `live()` and `release::unit_executable` (the one `ExecStart=` reader, which a promotion already uses to
seed its first release) and nothing else.

## D5 — what this story does not change

The tree's **order** stays `Release::history` (mtime, newest first, tag as the tiebreak) and `older_than` stays one
step back — ADR-0083 D2, unchanged, and the prune is a second reader of it rather than a second order. The **switch**
stays the staged symlink `rename`d onto `current` (ADR-0082), so the acceptance's "atomic, never edit-in-place" is a
sentence and a gate assertion rather than new code. `deploy`'s step 5 keeps preferring `Release::live`, so the release
finding and the deploy agree rather than adding a third rule. And no database is restored by anything here.

## D6 — how it is measured

- **`case_releases`** (new; no daemon, no `python3`, no `SKIP` at all): a hand-built tree of four releases with explicit
  mtimes plus a directory the tree does not recognise and a stray file; the **relative** link; the unit's `ExecStart`
  through `current`; `version` from inside `releases/two/holosmith`, through `current/holosmith`, and from `dist/`
  (`release two`, `release four`, `release -`) plus the json key; and five `doctor --quick` runs, on a `role = "hub"`
  config so that no daemon question is asked — the healthy tree (`release: pass:`, exit 0, the resolved path), a link
  naming a directory that is gone (`release: fail:`, exit 1, the hand fix), `current` replaced by a regular file
  (`release: fail:`, "not a symlink"), a machine whose unit names a plain binary (`release: pass:`, exit 0), and a
  machine with no unit and no tree (`release: unknown:`, exit 0).
- **`case_upgrade`** grows the prune arm, because a prune only happens inside a promotion and that case already has the
  daemon one needs: two old releases with explicit mtimes, `keep_releases = 3`, and after the promotion the printed
  line — asserted to be the **last** line of the run, which is what "step 8" means — the one directory removed, the
  live release and the rollback target still there, and the unrecognised directory untouched.
- **`cargo test`**: `tests/release.rs` (a binary run from a release directory and through `current` reports the
  directory, one outside reports `-`), `src/release.rs`'s own tests (the reservation, the exact count, the unrecognised
  directory, the second prune, the function's floor, and `running_tag`'s arms), `tests/upgrade.rs` (a successful
  promotion prunes to the count and keeps the rollback target; one that rolls back prunes **nothing** — which is what
  makes "after the poll" falsifiable), `tests/doctor.rs` (the four arms), `tests/version_cli.rs` (five lines, seven
  keys).

## Limits recorded rather than fixed

**No systemd here.** `systemctl` is a stand-in in `case_upgrade`/`case_rollback` and in the demo, so "systemd executes
through `current`" is the unit's `ExecStart=` plus the stand-in's argv — never a service that really started.

**A deploy repairs a broken link by writing the unit, not by re-pointing the link.** After a `server deploy` on a
machine whose `current` was broken, the unit names the deployed binary and the tree is still broken; the next promotion
seeds a release from that binary and re-points the link. A `holosmith release --repair` is the verb that would do it in
place, and it is not this story's.
