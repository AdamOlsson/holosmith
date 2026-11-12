# The project backup is one verified bundle per project per day, taken by the create

This is spec item 1's `project_backup` setting and the "Known risks" line it answers: several sessions
share one project repository, so an agent that damages the checkout damages every session's base. The
recovery point is a git bundle — the whole repository, every branch and tag, in one file — and the story
fixes four things about it: when it runs, how often, what is kept, and what happens when it fails.

## The bundle's name is its state

`<data>/backups/<project>-<YYYY-MM-DD>.bundle`. "Has today's been taken" is therefore a `stat`, and
nothing else in the system has to remember anything: no column, no marker file, no `last_backup_at`. The
date is UTC, because the day a file is named after has to be one value on a machine whose operator may be
in another zone, and "the machine's own zone" is a setting this system does not have.

The name is also the **sort key** for the retention rule, which is what makes the prune cheap: the last
seven days per project, by name, so no bundle is opened to decide which is oldest. That matters because
the prune exists on a machine that is already short of space.

## The bundle is verified before it is kept, and a corrupt one is deleted

Written aside, then `git bundle verify`, then renamed into place. A file under the final name is
therefore always a file git could read — which is the whole of the story's fourth line. The alternative
order (write it, verify it, delete it if it fails) leaves a window in which a half-written file sits under
a name an operator would trust, and the recovery point is exactly the thing that must not lie.

**The temporary name is per attempt, not per day**, and the review is why: the create's `back_up` runs
outside the create's lock scope, so two creates on one project can be in flight at once. A temporary name
derived from the project and the day alone would let the second `git bundle create` open and truncate the
file the first is about to rename — and if the second then failed, the name that verified would hold a
file rewritten after the check. `<name>.bundle.<pid>.<n>.tmp` costs one atomic counter, and the prune is
unaffected because `is_bundle_of` accepts only exactly `<prefix><day>.bundle`.

A repository with nothing to bundle is a failure, not an empty bundle: `git bundle create --all` refuses
an empty repository, and the module deletes its temporary file and answers an error.

## The create takes it, and every failure is logged and swallowed

The story says the bundle "runs before the first session of each day on a project", so ordering is part of
the requirement: the recovery point must predate the agent's first write. The create therefore takes it
**after** the row is inserted and **before** the provisioner is handed the session — awaited, not spawned,
because "before" is the claim.

Three placements were considered and two rejected:

- **As a fifth provisioning step.** `Step` is spec item 1's four steps; adding one would change the
  progress reports the session screen renders and the state machine's `session.status` payloads, for a
  feature that is off by default.
- **Spawned and forgotten.** Then the bundle may be taken while the agent is already editing, which is
  precisely the state a backup must not be taken from.
- **Before the row is inserted.** Then a create that is refused, retried or finds the machine full pays
  for a bundle nobody asked for. The row's existence is the "a session is really starting" signal.

The price is stated rather than hidden: with `project_backup` on, the first create of a day on a project
waits for `git bundle create --all`, which on a large repository is seconds. That is once per project per
day, and the setting is off by default (ADR-0006's config list already has the key).

Every failure — git refusing, the disk full, the repository absent — is one `warn` line naming the
project and git's words, and the session is created anyway. The story says so, and the alternative turns
a safety net into an outage: a machine whose backups directory cannot be written would stop accepting
work.

The repository is `<projects_root>/<project>` **whatever backend the run uses**, and it is not asked of
the backend. The bundle is of the host checkout — the thing every session's branch lives in and the thing
an agent can damage — so the stub, which mounts nothing and refuses no project, bundles exactly what a
Docker run would.

## What is not decided here

- **No restore path.** A bundle is a file an operator feeds to `git clone`; the system does not offer to
  restore it, and no story asks it to.
- **No timeout on the bundle.** A very large repository makes the create wait for as long as git takes.
  A bound would have to answer "what happens to the create when it expires", and the honest answer —
  create the session and log it — is what happens today on failure anyway; a timeout would only make it
  sooner. Recorded as a follow-up rather than guessed at.
- **No bundle on resume.** The story says "before the first session of each day on a project", and a
  resume is not a first session. A resumed session works in a worktree whose commits are the agent's, and
  the day's bundle already covers the base it started from.

## What the gate shows

`case_backup` boots its own server with `project_backup = true` over a real repository, plants eight
day-named bundles, drives two creates, and asserts: seven kept, the two oldest gone, exactly one bundle
for today, `git bundle verify` accepts it, and it carries a branch. `tests/backup.rs` covers the module
against real bundles — including nine days of them, so the prune is tested against the files it will meet
— and the two create behaviours a gate case cannot show: a failed bundle still answers 202, and
`project_backup = false` writes no directory at all.
