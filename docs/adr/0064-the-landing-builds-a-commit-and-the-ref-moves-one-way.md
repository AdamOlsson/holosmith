# The two landing strategies build a commit in a temporary worktree, then hand it to the one way a ref moves

This is the second half of spec item 12's merge: the `merge` and `squash` strategies (rule 2's temporary
worktree, rule 3's conflict) on top of the `ff-only` and rule 4. ADR-0063 already decided what a *write* to
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

**The two orders are different on purpose.** `ff-only` asks the ancestry question *before* the checkout one,
which is the order and the right one: "the target has commits the branch does not" is a fact about the
request, while `target_checked_out`'s advice ("check out another branch in that worktree and merge again") would
send the operator straight to a second refusal. The two landing strategies ask the checkout question first,
because for them the ancestry question does not exist and a `409 target_checked_out` should cost no temporary
worktree at all. `the_fast_forward_refusal_comes_before_the_checkout_one_and_the_landings_do_not` is the pair:
one repository — a branch the target has moved past, and a target checked out in a third working tree — answers
`not_fast_forward` by the default strategy and `target_checked_out` by `merge`.

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
deleting the flag from `hooks_flag()` turns it and the `a_hook_does_not_run` red.

## D8 — The lock waits; `409 merge_in_progress` is not written

Rule 2 allows "waits or returns 409". the `Merges` already waits, and a `try_lock` refusal would add a
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

## D10 — The fifth acceptance bullet is that story's, and this ADR says so loudly

The bullet reads: *"The UI offers 'Resolve with agent', which prompts the session to merge the target into its
branch and fix conflicts, then retries."*

There is no screen to put it on. The Changes sheet — the file list, the diff, the target and strategy picker, the
Merge button — is, and a phase-2 story that adds a button to a screen the next story rewrites is work
thrown away. set the precedent — ADR-0046 cut the Internet-access checkbox's copy for the same reason,
that it was a new visible element and belonged to the story that owned the form (an earlier development note's
landmine states the rule in as many words). ** implements this affordance against
this route's `409 conflict` and its `conflicts` list**, and the story's acceptance is answered with that pointer
rather than a tick. What builds is everything the affordance needs: a refusal a client can act on, naming
the paths, with a sentence that already says what the prompt should say.

## D11 — A squash keeps the branch, and that makes rule 5 a question has to decide

the keep-rule is `git merge-base --is-ancestor agent/<id> refs/heads/<target>` (`Worktree::merged_into`), and
a **squash** writes a commit whose only parent is the old target — so after a squash the branch's tip is *not* an
ancestor of the target, `merged_into` answers `No`, and `retained` **keeps** `agent/<id>`.

That is the honest answer and not a gap in this story. The branch carries the session's own commits, which the
target does not have: a squash deliberately leaves them out of the target's history, so "this branch's commits
are in the target" is false, and a teardown that deleted it would delete the only place they still exist (the
WIP ref is a tree, not a history). What it does change is rule 5's sentence — "The branch `agent/<id>` is then
deleted because it is merged" — which **** cannot implement by re-asking git after a squash. Its
`delete_session` request already knows a merge just succeeded, so the evidence it needs is the answer it got,
not a second ancestry question; and where it chooses to ask git anyway, the honest version is the *tree*
comparison (`git diff --quiet <branch> <target>`), not ancestry.

`a_squash_keeps_the_branch_a_merge_does_not` asks `Worktree::retained` after each strategy and requires
`branch: None` after a `merge` and `branch: Some(agent/<id>)` after a `squash`. It is the interaction, recorded
as a measurement rather than as prose, so meets it as a red test if it assumes otherwise.

Two smaller record-keeping decisions this story made while the review was read back:

- **`{"strategy": 3}` is now `422 bad_body`** where answered `bad_strategy`: a `strategy` that is not a
  string is the *body's* shape rather than a value outside the vocabulary, which is the create's own split
  (`bad_body` for a numeric `model`). The wire table in `tests/merge.rs` asserts it.
- **`git worktree prune` is what clears a registration whose directory is gone**, and git only calls it
  `prunable` when the record is unreachable — which is why the test that measures it asserts `"prunable "` with
  its reason and a fixture label that does not contain the word. The first version of that test was **vacuous**:
  its fixture was called `strategy-prunable`, and the word was in the listing's own path. that story's
  `a_prunable_registration_is_not_a_checkout_and_the_merge_lands` had the identical defect (fixture
  `merge-prunable`) and is fixed here for the same reason.

## D12 — The gate case walks one repository through all three strategies

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

`case_merge` loses one of its refused requests (`strategy:"squash"` was `422 unsupported`; it is a strategy now)
and gains `strategy:"rebase"` → `422 bad_strategy` on top of the two `bad_target`s and the one `unsupported`, so
the refusal table is still walked end to end — three facts on two codes, which is what its comment now says.

## What this ADR does not decide

Rule 1's two guards (`session_running`, `dirty_worktree`) and `delete_session: true` — still `422 unsupported`
at the time of writing, and built (ADR-0065) —
with no new sentence — are, which also owns the 60-second merge/publish timeout **and, per D11, the
question of what "the branch is deleted because it is merged" means after a squash**, which it cannot answer by
re-asking git. Publish is. The
Changes sheet, the target and strategy picker and the Resolve-with-agent affordance are (D10). The hub is
phase 3. `worktree.rs`'s create refusals are untouched, and the merge's git commands are still unbounded: a
timeout on the *request* is that story's, and an arbitrary bound on a git command the operator asked for would be
this module inventing a policy it was not given.
