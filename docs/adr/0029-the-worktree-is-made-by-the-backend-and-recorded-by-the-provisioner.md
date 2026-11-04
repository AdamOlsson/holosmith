# The worktree is the backend's to make and the provisioner's to record

HS-025 is spec item 1's list of five host commands. Four decisions follow from making them real for the
first time: who runs git, who writes what it made, what a second provision does, and what a missing thing
means during teardown.

## Git runs in one module, off the runtime's worker

`src/worktree.rs` holds `Worktree` (the repository, `<projects>/.worktrees/<id>` and `agent/<id>`) and the
three operations the story asks for: `ensure`, `ahead`, `remove`. Not inside the Docker backend, because
the worktree is not a Docker thing — it is a host git thing that HS-027 mounts, HS-033 snapshots and HS-061
diffs, and four stories reaching into one backend's private helper would be four places the naming rule
lives.

The module is synchronous and `std`-only, and the backend runs it on `tokio::task::spawn_blocking`. A
subprocess is not a future: `git worktree add` on a large repository takes as long as it takes, and the
provisioner is a `tokio` task whose worker would carry no other work meanwhile. A `JoinError` is reported as
"git did not finish", because a panicked or cancelled task is not a repository that refused.

**No timeout**, which ADR-0028 already recorded for the preflight's two external commands and which applies
for the same reason: `std::process` has no deadline, and a wedged program on a hung network filesystem
wedges any caller. A repository on NFS is not something this build has met.

## The backend makes it; the provisioner writes the row

`SessionBackend::provision` answers `Provisioned` — `Nothing` for the three steps that record nothing, and
`Worktree { path, base_commit }` for the one that does — and the provisioner writes the two columns through
one new statement, `SessionRepository::set_worktree`.

ADR-0019 is the reason: a backend emits frames and cannot write the row, and that rule is what keeps a
forged `session.status` frame from being a state change. A backend with a `SessionRepository` would be able
to move a row, so it gets no repository and answers with what it made instead. The alternative — the
provisioner computing the path and the base itself — would put the naming rule and a `git rev-parse` on the
server side of a line that is otherwise clean.

An enum rather than `Option` fields, so a step that records something cannot answer half of it: a worktree's
path and its base commit are one fact, and `ahead` is measured from the second.

## The step is idempotent: create **or** adopt

The provisioner runs all four steps on every provision, and `POST /resume` re-provisions a session that was
only stopped (ADR-0022): its worktree, its branch and every commit on it are still there. A step that could
only ever run once would be a session that cannot be resumed, so `ensure` has three states and a command for
each:

1. registered and branch present → nothing to do;
2. branch present, nothing registered (a teardown interrupted between its two commands) → attach the
   existing branch, keeping its commits, instead of failing on `-b`;
3. no branch → create it with `-b`, from the row's `base_commit` or, on a first provision, from `HEAD`.

**The row's base commit wins over the branch's current head.** A session that has committed three times must
keep pointing `ahead` at where it started; taking the head would silently reset the number the changes sheet
shows (HS-061).

## Teardown cannot wedge a delete, so "already gone" is success

`remove` asks git whether the worktree is registered and whether the branch exists *before* running either
command, rather than running them and reading git's English: removing a worktree that is not registered
exits 128 and deleting a branch that does not exist exits 1, and both are answers, not failures. A missing
**project repository** is `Ok` too. The delete flow keeps the row `deleting` when a backend fails (ADR-0023),
so a teardown that refused over an absent directory would leave a session nobody can remove — and a project
the operator moved is not a reason to keep one.

A *directory* that exists but is not a registered worktree is left alone: git will not remove it, and an
orphan from a crashed run is HS-044's to prune. Every path this story drives leaves the registration clean,
which is what that story starts from.

Two measurements made these rules concrete, and both are in the module's own doc comment: `git worktree add`
resolves a **relative** path against the repository rather than the caller's directory, so every path here is
absolute; and `git worktree list --porcelain` prints the **resolved** path, so a projects root reached through
a symlink only compares equal after `canonicalize`.

## `ahead` is a function, not a column and not an endpoint

`git rev-list --count <base_commit>..agent/<id>` is one call on the value, and its publisher is
`GET /api/sessions/:id/changes` (spec item 12, HS-061). A column would need a migration and a write after
every commit the agent makes, which nothing observes; computing it per row in the list endpoint would be a
git call per row on a page with a cap of three, and a second read path over the same repository. Its readers
today are its test and that endpoint, which is the same shape as ADR-0028's `Complaint` and HS-045's `doctor`.
