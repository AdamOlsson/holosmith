# The WIP snapshot is a temporary index and a ref, and "nothing to save" removes the ref

Spec item 9's script is the whole design: seed a temporary index from `HEAD`, `git add -A` into it,
`write-tree`, `commit-tree` with the holosmith identity, and point `refs/holosmith/wip/<id>` at the
commit. makes that script a module (`src/snapshot.rs`), a container mode
(`holosmith container snapshot`) and three call sites.

## The temporary index is what keeps the promise

The acceptance says the branch, the working tree and the index are never modified. `git add` is the only
step that would touch the index, so every command that reads or writes it carries
`GIT_INDEX_FILE=<tmp>`; the file is created beside the session (or under the container's `/tmp` for the
CLI) and removed whatever happened. `read-tree HEAD` is load-bearing rather than decoration: it makes a
tracked file the agent **deleted** a staged deletion, which a bare `git add -A` on an empty index would
represent as nothing at all.

**The host runs it, not the container.** Spec item 9 describes `docker exec s_<id> hs-snapshot`; this build
calls the same module in-process from the Docker backend, because a snapshot must work when the container is
already gone or when the session is `stopped`, and the worktree is a host directory either way. The spec's
container spelling remains available as `holosmith container snapshot` with `/work` and `HS_SESSION_ID`, which
is the manual equivalent and the gate's subject.

The snapshot commit's identity is `-c user.name=holosmith -c user.email=holosmith@localhost` on the
command, never a write to a repository or global config. The `commit-tree` has `HEAD` as its parent, so
the ref is a child of the branch's tip and `git diff <branch> refs/holosmith/wip/<id>` is the
uncommitted work.

## A tree equal to HEAD deletes the ref

The ref means "there is work here that the branch does not have". A commit whose tree equals `HEAD`'s
tree would be a second name for nothing, and the keep-rule is written as "the wip ref differs from
the branch". So the tree comparison comes before `commit-tree`, and "equal" is an `update-ref -d`, not a
new commit. A ref that was never there is already the right answer.

## Three call sites, and a failure is reported where it can be

- `Turn::finished` takes the snapshot on every `TurnEnd::Settled`, before the row moves to `idle`. A
  failure is an `error!` line and does not change the turn's ending: the turn did finish, and turning a
  git failure into an `error` row would lose the transcript it just produced.
- `Stopper::stop` takes it before the expected-stop flag and the container stop, because after the stop
  there may be no process left to write the worktree. A failure is an `error!` line and the stop goes
  ahead — the caller asked for a container to stop.
- The delete flow already called `SessionBackend::snapshot` before its transition; it continues to, and
  its `409 unsaved_work` is the one place a failure is a refusal.

`SnapshotError` carries git's own last words and the command, so the log line names what failed; the
backend wraps it with the operation, the way every other backend failure is named.

## What this story does not do

The delete's keep-rules and the recovery sentence are that story's; this story only guarantees that the ref
exists before a delete and that a failed snapshot is visible. A stopped session's worktree is not
removed here, and the snapshot does not fetch or push anything.
