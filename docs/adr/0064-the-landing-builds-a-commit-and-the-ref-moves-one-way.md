# The two landing strategies build a commit in a temporary worktree, then hand it to the one way a ref moves

HS-055 is the second half of spec item 12's merge: the `merge` and `squash` strategies (rule 2's temporary
worktree, rule 3's conflict) on top of HS-054's `ff-only` and rule 4. ADR-0063 already decided what a *write* to
the project repository is allowed to assume — its own module, the changes view's rules by import, every answer
measured rather than parsed out of git's English, `Merges` as the per-project lock. This ADR decides only what
the two new strategies add, and it reuses rule 4 **unchanged** as the way the target moves: a `merge` commit
descends from the target, so its landing is a fast-forward of the target, and the module does not learn a second
write.

One number changed while this story ran, and D3 records it because the first draft of the plan got it wrong.

## D1 — `<data>/merge/<id>`, detached at the target's commit, and a stale directory is removed rather than refused

Rule 2's own words: "a temporary detached worktree at `<data>/merge/<id>` from the target". `id` is the
**session's**, which needs no minting: two merges of one session cannot run at once (the `Merges` lock), a
second one has nothing to merge, and two sessions of one project are already serialised. The parent
`<data>/merge` is `create_dir_all`'d and carries **no mode of its own** — the data root is 0700 and this is a
directory inside it, so its own permissions protect nothing. That is `backup.rs`'s `<data>/backups`, and the
opposite of `drain`'s socket, whose 0700 directory *is* the authorisation.

The worktree is created with `git worktree add --detach <path> refs/heads/<target>`: `--detach` because the
target may be checked out anywhere at any moment and git refuses to check one branch out twice, and the fully
qualified ref so a tag called `main` cannot be what a merge lands into.

**A leftover path is removed, never refused.** A killed run leaves a directory there, and a `409
merge_in_progress` would be an answer about concurrency to a question that is not about concurrency. The
removal is *measured*, in the sense ADR-0063 made load-bearing:

```text
if path exists:
    git worktree remove --force <path>    # exit status ignored: git may simply not know the path
    if path still exists:
        remove_dir_all <path>             # an operator's rm -rf, a half-written first attempt
git worktree prune                        # unconditional
```

The middle test is `path.exists()`, not git's exit code: git fails to remove a directory it never registered,
and the directory is still ours to delete. `prune` then runs **unconditionally**, because the other leftover
shape is a registration whose directory is gone — git calls it *prunable*, and `worktree add` refuses a path it
holds such a registration for. Prune is a no-op for every registration whose directory still exists, which is
every live worktree on the machine, so no live worktree can lose its registration to a merge.

## D2 — Two commands, one prefix, and the commit is holosmith's

| Strategy | Command, in the temporary worktree |
| --- | --- |
| `merge` | `git -c core.hooksPath=/dev/null -c user.name=holosmith -c user.email=holosmith@localhost merge --no-ff -m <message> refs/heads/agent/<id>` |
| `squash` | `… merge --squash refs/heads/agent/<id>` then `… commit -m <message>` |

`--no-ff` is what makes `merge` a strategy rather than a second spelling of `ff-only`: a branch that *could* be
fast-forwarded still gets a commit with two parents, the old target and the branch's tip. Without it the two
strategies would be one strategy and a different word, and `tests/merge.rs`'s
`a_merge_makes_one_commit_with_two_parents_the_old_target_first` goes red.

The identity is `snapshot::IDENTITY` — the WIP snapshot's pair, reused rather than copied — carried as `-c`
options on the command and never written into a config. A commit this build makes on an operator's behalf is
holosmith's, and a machine whose git has no `user.name` (a fresh container, a bare server) would otherwise fail
to write one at all. `snapshot.rs`'s doc for the constant was widened to say so. It is measured twice over: the
unit test pins the argument vector, and both integration tests read `git log -1 --format='%an <%ae>'` and
require `holosmith <holosmith@localhost>`. Dropping the two `-c user.*` pairs turns both red, naming whoever the
machine's global config does (it is `Adam Olsson` on this machine, which is exactly why the assertion reads the
commit rather than trusting the vector).

## D3 — The message: git's own default was the plan, and a measurement reversed it

The plan's recommendation was git's own default — omit `-m` and let the commit read `Merge branch 'agent/<id>'`,
because the operator's history is not this build's to write prose into. The first run of the story's own test
produced:

```text
Merge branch 'refs/heads/agent/2adf1626ac4c5844' into HEAD
```

The temporary worktree is **detached** and the argument is a **fully qualified ref**, so git's default is a
sentence about this module's scratch directory: `into HEAD`, and the ref spelling a client never sees. That is
not the operator's register; it is this build's internals leaking into their log.

So the build writes both defaults, in one shape, naming both refs:

```text
Merge agent/<id> into <target>
Squash agent/<id> into <target>
```

`merge` had to move because git's default was unusable; `squash` had none at all (`--squash` stops before
committing). One sentence shape for the two is simpler than two inventions, and a request's `message` still
replaces it **verbatim** — `std::process::Command` means no shell, so no quoting question, and there is no
length cap for the reason the create's `prompt` has none.

## D4 — The worktree is removed *before* the target moves

The acceptance is "removed afterwards even on failure", and the sequence that makes it true for every failure is
`create → land → read the new commit → remove → move the target`. The move is the part that fails late:
`409 target_dirty` is only discoverable once `git merge --ff-only` has run in the operator's own checkout, long
after a landing succeeded.

`the_temporary_worktree_is_gone_even_when_the_move_is_refused_after_it_landed` is the test — the landing
succeeds, the move is refused, and `<data>/merge` is empty — and moving the teardown after the move (a `?` on
the update, the obvious shape) turns eight tests red, because a conflicted worktree is left behind too.

The cost, stated rather than hidden: a commit whose move failed is a **dangling object** the repository keeps
until `gc`, and its id is in the log line. The alternative — leaving the worktree for inspection — fails the
acceptance, and inspecting an unreferenced commit is not something a phone can do anyway.

## D5 — A conflict is the index's unmerged paths, and the abort *is* the removal

A real merge that cannot reconcile exits non-zero and leaves the temporary worktree's index unmerged. The paths
are read with `git ls-files --unmerged -z`, **not** `git diff --diff-filter=U`: a diff is where a repository's
own config gets a say (`diff.external` replaces the output with a program's, a textconv driver rewrites a file's
bytes — ADR-0062 measured both on this build), while a conflict is a fact about the index. The `-z` record shape
is `<mode> <sha> <stage>\t<path>\0`, one per stage, so the walk takes what follows the tab and dedupes; that is
`changes::conflict_paths`' shape one command over.

Non-zero exit **with** no unmerged path is not a conflict — it is a `500` carrying git's own words. The two
facts are measured apart for the reason ADR-0063 gives for the fast-forward: `merge` also exits non-zero for "not
something we can merge", and reading a status code as a fact is how a refusal becomes a lie.

`git merge --abort` is **not** run. It restores a working tree that is about to be deleted, so its only effect
would be on a directory that ceases to exist a moment later. What "nothing changes" means is measured about the
*project* repository — the target's ref, the checkout's files and `git status --porcelain` — because the merge
never ran there at all.

The JSON is `409 conflict` plus **one additive key** on the envelope: `"conflicts": ["f.txt", …]`. Rule 3's own
words are "409 `conflict` with the conflicting paths", the envelope's message is a fixed sentence (ADR-0008), and
a phone rendering a list is not a phone rendering a sentence with paths joined by commas. `ErrorBody` gains
`conflicts: Option<Vec<String>>` with `skip_serializing_if`, so every other error's body is byte-identical —
ADR-0052's additive `kept` on the delete's 202 is the same shape, and the test that a non-conflict carries no
`conflicts` key is in `every_bad_request_moves_no_ref`. The paths reach the operator through
`MergeError::Conflict::sentence()` and the log line as well.

## D6 — `merge` and `squash` do not ask the fast-forward question

`ff-only` refuses a target that has commits the branch lacks. That is exactly the case a real merge exists for,
so the ancestry test is asked only by the strategy that cannot reconcile anything. `a_merge_lands_a_branch_the_
target_has_already_moved_past` is the pair: the same repository shape is a `409 not_fast_forward` by the default
strategy and a `200` with two parents by `merge`, in one test. Making the test run for every strategy turns five
tests red.

The move still uses `409 not_fast_forward` when it fails because the ref no longer holds what this module read.
That code names the **ref update**, which is a fast-forward in all three strategies — the commit descends from
the target — so the sentence "the target has commits the session's branch does not, so it cannot be
fast-forwarded" is true of the *move* in that arm, and its next step ("merge the target into the session's
branch and try again") is the right one.

## D7 — Hooks off on all three commands, because hooks live in the common directory

`worktree add` runs `post-checkout`; `merge` runs `post-merge`, `pre-merge-commit`, `prepare-commit-msg` and
`commit-msg`; `commit` runs `pre-commit`, `prepare-commit-msg`, `commit-msg` and `post-commit`. A linked
worktree's hooks are the **common** git directory's, so a project's hook is just as reachable from
`<data>/merge/<id>` as from the checkout — which is why `-c core.hooksPath=/dev/null` is on the shared prefix
rather than on the merge alone. `no_hook_runs_for_either_strategy` writes `post-merge`, `post-checkout`,
`pre-commit` and `commit-msg` scripts into a real repository and asserts no marker appears for either strategy;
deleting the flag from `hooks_flag()` turns it and HS-054's `a_hook_does_not_run` red.

## D8 — The lock waits; `409 merge_in_progress` is not written

Rule 2 allows "waits or returns 409". HS-054's `Merges` already waits, and a `try_lock` refusal would add a
second kind of answer for a request that is not wrong. Nothing changed in the lock.

What *is* new is that the wait is now observable in an outcome, which it was not for `ff-only`: two `merge`s of
one project's two sessions from a barrier both land, and the target's two merge commits are the target and its
first parent. Without the lock both landings are built from the same `old`, one writes first, and the other's
move fails — so `two_merges_of_one_project_serialise_and_both_land` is a client-visible measurement of a rule
ADR-0063 could only measure by the mutex's identity. (The identity test stays: it is the deterministic half.)

## D9 — `<data>` enters as a parameter, and nothing at boot mentions a leftover

`merge()` takes `data_root: &Path` and owns the `merge` subdirectory's name (`MERGE_DIR`), because the name is
the rule. The parent is the state's — `state.config.data_root` — so the handler is where it enters, and `Merges`
stays exactly what it was: the lock, with no filesystem in it. The alternative (a `Merges` that also knew the
data root) would have put two unrelated facts in one type for the sake of one fewer argument.

A leftover `<data>/merge/<id>` is **nothing `doctor` or the startup reconciler should mention**. The reconcile
sweeps worktrees no row owns, and this directory is a host scratch space under the data root, not a session
worktree; a leftover is removed by the next merge of that session (D1). A check would be a check that fires on a
machine whose operator simply killed a server once.

The request is one `Request` value — target, strategy, message — rather than four positional parameters, because
the three fields are the contract's and `http::merge` already builds them from a body: the handler parses into
it, `merge()` consumes it, and `Strategy` is a type because the strategies differ in **which commands run**, not
in a word on the wire.

## D10 — The fifth acceptance bullet is HS-058's, and this ADR says so loudly

The bullet reads: *"The UI offers 'Resolve with agent', which prompts the session to merge the target into its
branch and fix conflicts, then retries."*

There is no screen to put it on. The Changes sheet — the file list, the diff, the target and strategy picker, the
Merge button — is HS-058, and a phase-2 story that adds a button to a screen the next story rewrites is work
thrown away. HS-038 set the precedent (ADR-0046: it left the Internet-access checkbox's copy to whoever next
owned the create form, because "a new element is a UI change"). **HS-058 implements this affordance against
this route's `409 conflict` and its `conflicts` list**, and the story's acceptance is answered with that pointer
rather than a tick. What HS-055 builds is everything the affordance needs: a refusal a client can act on, naming
the paths, with a sentence that already says what the prompt should say.

## D11 — The gate case walks one repository through all three strategies

`case_merge_strategies` boots its own Docker run over its own projects root **and data root** (so `<data>/merge`
is the case's to measure), re-makes the worktree the failed network step took back with the step's own command,
and then:

1. **squash** of three commits → `200`, `strategy: "squash"`, `rev-list --count <base>..main` is `1`,
   `main^` is the old target, and `main^{tree}` equals `agent/<id>^{tree}`. The tree is the assertion `git log
   --oneline` cannot make: one commit that lost a hunk is still one commit.
2. **merge** of two more → `200`, `rev-list --parents -n 1 main` is exactly `<new> <squashed> <tip>`.
3. **a planted conflict** on `f.txt` → `409 conflict`, `"conflicts":["f.txt"]`, the target unmoved, the
   checkout's `status --porcelain` empty, and the operator's file untouched.
4. `<data>/merge` **empty** at the end, measured with `find … -mindepth 1 | wc -l` — a worktree left behind is
   exactly what an `ls` would show, and the case asserts the directory exists first so a merge that never made
   one cannot pass.

`case_merge` loses one of its four refused requests (`strategy:"squash"` was `422 unsupported`; it is a strategy
now) and gains `strategy:"rebase"` → `422 bad_strategy`, so the refusal table is still walked end to end.

## What this ADR does not decide

Rule 1's two guards (`session_running`, `dirty_worktree`) and `delete_session: true` — still `422 unsupported`
with no new sentence — are HS-056, which also owns the 60-second merge/publish timeout. Publish is HS-057. The
Changes sheet, the target and strategy picker and the Resolve-with-agent affordance are HS-058 (D10). The hub is
phase 3. `worktree.rs`'s create refusals are untouched, and the merge's git commands are still unbounded: a
timeout on the *request* is HS-056's, and an arbitrary bound on a git command the operator asked for would be
this module inventing a policy it was not given.
