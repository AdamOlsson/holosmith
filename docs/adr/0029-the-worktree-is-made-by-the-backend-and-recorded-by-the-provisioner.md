# The worktree is the backend's to make and the provisioner's to record

This is spec item 1's list of five host commands. Four decisions follow from making them real for the
first time: who runs git, who writes what it made, what a second provision does, and what a missing thing
means during teardown.

## Git runs in one module, off the runtime's worker

`src/worktree.rs` holds `Worktree` (the repository, `<projects>/.worktrees/<id>` and `agent/<id>`) and the
three operations the story asks for: `ensure`, `ahead`, `remove`. Not inside the Docker backend, because
the worktree is not a Docker thing — it is a host git thing that mounts, snapshots and
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
`Worktree { path, base_commit, created }` for the one that does — and the provisioner writes the two columns
through one new statement, `SessionRepository::set_worktree`.

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
shows.

**And a step that adopted has nothing to undo.** A provision that creates work is undone when a later step
fails, because a provision must not leave a partial resource behind (ADR-0015) — but a worktree the step only
*adopted* was the session's before this provision ran, with the commits the agent has made on it. Deleting it
because the *network* step refused would be data loss dressed as cleanliness: the undo would take back
something it never made, and the branch — the actual work — would go with it. So `ensure` reports which of the
two happened and the provisioner records only the steps that made something (`Provisioned::undoable`). A
**delete** is a different matter and removes an adopted worktree like any other: the session is going away,
and its work was snapshotted first (ADR-0023).

This is the one place where the plan was incomplete rather than wrong: its own acceptance says teardown
runs when a session is deleted, and the resume-ban consequence of undoing an adoption only becomes reachable
at, when the step *after* the worktree can succeed. It is fixed here because the failure is silent, and
`tests/worktree.rs`'s `a_later_step_failure_does_not_take_an_adopted_worktree_back` fails when the rule is
removed.

## Teardown cannot wedge a delete, so "already gone" is success

`remove` asks git whether the worktree is registered and whether the branch exists *before* running either
command, rather than running them and reading git's English: removing a worktree that is not registered
exits 128 and deleting a branch that does not exist exits 1, and both are answers, not failures. A missing
**project repository** is `Ok` too. The delete flow keeps the row `deleting` when a backend fails (ADR-0023),
so a teardown that refused over an absent directory would leave a session nobody can remove — and a project
the operator moved is not a reason to keep one.

A *directory* that exists but is not a registered worktree is left alone: git will not remove it, and an
orphan from a crashed run is the to prune. Every path this story drives leaves the registration clean,
which is what that story starts from.

Two measurements made these rules concrete, and both are in the module's own doc comment: `git worktree add`
resolves a **relative** path against the repository rather than the caller's directory, so every path here is
absolute; and `git worktree list --porcelain` prints the **resolved** path, so a projects root reached through
a symlink only compares equal after `canonicalize`.

## `ahead` is a function, not a column and not an endpoint

`git rev-list --count <base_commit>..agent/<id>` is one call on the value, and its publisher is
`GET /api/sessions/:id/changes` (spec item 12). A column would need a migration and a write after
every commit the agent makes, which nothing observes; computing it per row in the list endpoint would be a
git call per row on a page with a cap of three, and a second read path over the same repository. Its readers
today are its test and that endpoint, which is the same shape as ADR-0028's `Complaint` and the `doctor`.

## The story's demo is only half showable, and that is recorded rather than papered over

the demo is "create a session, see `.worktrees/<id>` with branch `agent/<id>`, commit in it, see `ahead`
increment, delete and see it gone". Nothing can show the middle: the step after the worktree is the session's
network, it belongs and it refuses, so the provisioner undoes the worktree milliseconds after making
it. What the shipped artifact *can* show — and `scripts/verify.sh`'s `case_worktree` asserts — is the row's two
columns, the refusal's own reason, the worktree and branch gone afterwards, the registration pruned, and a
`DELETE ?force=true` that answers 202 and leaves a `404`. What it cannot show is a live worktree with commits
on it, and `ahead` has no publisher until the `GET /changes`. `tests/worktree.rs` is where those two are
proven, against a real repository: `the_worktree_step_makes_the_branch_and_names_the_commit_it_starts_from`
and `ahead_counts_the_commits_the_branch_has_that_the_base_does_not`. A later reader should not read "
Done" as "the demo was performed end to end".
