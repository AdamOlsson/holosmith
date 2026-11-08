# The merge's guards are the route's, and the branch a `delete_session` merge leaves is the one it landed

HS-056 is the last of spec item 12's merge: rule 1's two guards, rule 5's `delete_session` and rule 6's pair of
timeouts. ADR-0063 and ADR-0064 already decided what a *write* to the project repository is allowed to assume —
its own module, the changes view's rules by import, every answer measured rather than parsed out of git's
English, and one way a ref moves. This ADR decides only what a *session* adds to that: when the route refuses,
what it does after a landing, and which of the two numbers has no client in this build.

Three of the six decisions had a plausible wrong answer, and each of them has a test that goes red when it is
taken: the guard's status set (**D2**), the keep-rule's override (**D5**, **D6**), and the answer to a failed
delete (**D7**).

## D1 — The guards are `http::merge`'s, and `merge()` still knows nothing about a session

`merge::merge` moves a ref between a worktree, a request and the config's target list. ADR-0063 says in as many
words that a session row "is *not* consulted and *not* written", and that "whether the session may be merged at
all (a running turn, uncommitted work) is HS-056's rule 1" — so rule 1 goes where the row is, in the handler, and
`merge()` is unchanged. It keeps exactly one question asked twice, `listed`, because that one is a *precondition*
the module can ask of itself.

The order is the handler's, and each step is a fact about a different thing:

| # | Question | Answer |
| --- | --- | --- |
| 1 | the body | `bad_body` / `unknown_field` / `bad_strategy` — the request's shape |
| 2 | `target` is listed in `merge.targets` | `422 bad_target`, before the row is read (ADR-0063's order) |
| 3 | the row exists | `404 not_found` |
| 4 | **rule 1a**: the row's `status` | `409 session_running` (D2) |
| 5 | the row has a `base_commit` | `409 no_worktree`, this route's sentence |
| 6 | **rule 1b**: the worktree's own work | `409 no_worktree` for `changes::basis`' four facts, or `409 dirty_worktree` |
| 7 | the merge | the three keys, or a refusal |

Both guards sit **before** the blocking task that merges, so "a refused merge creates no `<data>/merge`
directory at all" is true by construction rather than by cleanup: `land` is what makes that directory, and a
refusal never reaches it.

## D2 — Only `running` refuses, and every other state still merges

Rule 1's words are "while a turn runs". `Status::Running` is exactly that, and the other five are not:

| State | Why it merges |
| --- | --- |
| `idle` | a turn has ended; the ordinary case |
| `error` | the provision failed, or the agent exited badly — and this is often the *only* state a session with work ever reaches on a broken machine, so refusing it would hide the merge behind a resume |
| `stopped` | the container is gone; the worktree and the branch are not |
| `starting` | no turn exists; a merge of a half-provisioned session fails on git's terms or wins on its own, and a guard for it would be a second rule the spec does not name |
| `deleting` | the operator asked for the removal; its `Retained` was decided before the removal was spawned, so a merge here can neither lose the branch nor gain one |

`ApiError::not_in_state` already has one code per state, so a guard written over the *other* four would answer
`session_starting`, `session_error`, … — four codes no sentence in the spec asks for. The test that keeps this
honest is the negative half: `every_status_but_running_still_merges` drives one fixture per state through the
table's own `Transitioner`, and an `!= Idle` guard (which is how "not running" gets written by mistake) turns it
red for four of the five. The gate's `case_merge_guards` merges an `error` row for the same reason.

## D3 — `dirty_worktree` *is* a pre-check, and ADR-0063's "no pre-check" is about a different question

ADR-0063 D5 is emphatic that `409 target_dirty` must not be a pre-check: the module runs git and classifies the
failure, because an **unrelated** dirty file in the operator's checkout does not block a fast-forward — git
carries it across — and a `status --porcelain` pre-check turns `an_unrelated_dirty_file_does_not_block_the_merge`
red. That reasoning does not transfer, because the two dirtinesses are not the same subject:

- `target_dirty` is about the **operator's** working tree, and the request is about a *ref*. Git's own merge is a
  better judge of whether that tree is in the way than a status read is.
- `dirty_worktree` is about the **session's** working tree, and the request is about that work: rule 1 says "only
  commits are merged". Nothing about the merge would be refused by git — the branch's uncommitted files are not
  in the branch — so the check cannot be deferred to a git failure, because no failure happens.

So one is a pre-check and the other is a post-mortem, and that is a fact about which worktree holds the question.

The check asks `changes::basis` **first**, then `changes::dirty`. `dirty` alone would answer `500` from `git
status` in a directory that is not there (an `rm -rf`, a pruned registration, a deleted branch); `basis` is the
changes view's own four ordered refusals, already `pub(crate)` for this route, so a session whose worktree has
gone answers this route's `409 no_worktree` with the sentence it has answered all along.
`a_session_with_no_worktree_is_refused_before_the_dirty_check` measures the order.

The definition of work is `changes::dirty`'s — `status --porcelain --untracked-files=normal --no-optional-locks` —
which means an untracked file counts and an **ignored** file does not. `an_ignored_file_does_not_refuse_the_merge`
is the test: the guard must not drift to `--ignored` (which would refuse every build that writes a log) or to
`ls-files --others` (which would refuse a checkout full of build output).

The refusal's sentence names the commit, because rule 1's second half is a UI affordance and a sentence is what
the sheet will render:

> the session has uncommitted work, so nothing was merged; ask the agent to commit it, then merge again

The offer itself is HS-058's (ADR-0064 D10's precedent: there is no screen, so there is nothing to put a button
on). What this ADR can do for that story is name the prompt, so it inherits a decision instead of inventing one:

> Commit every change in the working tree, with a message that describes it, and tell me the commit's hash.

## D4 — `delete_session` is the route's field, not `Request`'s

`merge::Request` is `{target, strategy, message}`, and it stays that: `merge()` moves a ref and a field it was
handed and never read would be a lie in its type. The handler parses the body into `Parsed { request,
delete_session }`, and `true` is no longer refused — this is the last field the route used to answer `422
unsupported` for, and **`unsupported` retires with it** (`D9`).

## D5 — The delete flow's request half is one function, and the merge's fact replaces the *branch* half only

`DELETE /api/sessions/{id}` was a handler body; it is now `sessions::deletion(state, id, force, landed)`, and the
route is its wrapper. Copying the sequence into the merge's handler would have re-derived HS-043's rules by hand,
and ADR-0052's whole point is that the snapshot, the keep-rule and the removal are *one* decision:

1. the row, and the `deleting` re-run;
2. the turn, if one is running (a merge cannot have left one, but a prompt between the guard and here can);
3. the snapshot, which is why a delete can answer `409 unsaved_work`;
4. the keep-rule, **after** the snapshot (that is what writes or removes the wip ref, and computing it before
   makes `wip` null every time — ADR-0052's measured bug);
5. the transition, the answer, and the removal off the task.

`landed: Option<&str>` is the one new parameter: `Some(target)` when the request that is deleting is the request
that just merged this session's work into `target`, `None` for every other caller. It overrides **the branch and
only the branch**:

```text
match (retained, landed) {
    (Known { wip, .. }, Some(target)) => Known { branch: None, wip },   // the work is in the target
    (retained, _)                     => retained,                      // everything else is HS-043's
}
```

The wip half is still the repository's, asked at the same point for the same reason, and `Retained::Unknown`
stays `Unknown` — "cannot tell" must never mean "delete", even when the request is confident about the branch.

**`delete_session` passes `force = false`.** The route has no query, and a `?force` on a merge would be a second
way to lose the work that rule 1 just refused to merge. The price is stated in the tests: a delete that cannot
snapshot leaves the session (D7), and the operator's next `DELETE` is the retry.

## D6 — After a successful merge the branch is not kept, and that is a fact each strategy supplies

Rule 5: "`delete_session: true` runs the delete flow after a successful merge. The branch `agent/<id>` is then
deleted because it is merged."

For `ff-only` and `merge` the second sentence is a git fact: the target *is* the branch's tip, or descends from
it, so `merge-base --is-ancestor agent/<id> <target>` answers yes and even the ordinary keep-rule would not keep
the branch. For **`squash` it is not**: ADR-0064 D11 records that a squash writes a commit whose only parent is
the old target, so the branch's tip is not in the target's history, and `Worktree::retained` answers `branch:
Some(agent/<id>)` — correctly, because the branch holds the session's *commits* and a squash deliberately keeps
them out. That is right for a delete nobody merged, and wrong for a request that says "this has landed, remove the
session".

So the request's own knowledge is the warrant, and what it knows is different per strategy:

| Strategy | What this merge measured | Why the branch is separable |
| --- | --- | --- |
| `ff-only` | the target moved to the branch's tip (`update-ref`/`merge --ff-only` succeeded) | its commits are in the target |
| `merge` | the landing commit has the branch's tip as one parent | its commits are in the target |
| `squash` | the target's tree **is** the branch's tree (`case_merge_strategies` measures the equality) | its *work* is in the target |
| any, as a no-op | `git diff-index --cached --quiet HEAD` said nothing was staged | the branch holds nothing the target lacks |

The cost is named rather than hidden: after a squash the branch's commit messages and its shape are gone. That is
the operator's instruction — the request says "delete the session" and this request is what merged it — and the
alternative (keep a squashed branch after a `delete_session`, so `kept.branch` names it) leaves a branch nobody
asked to keep, on a session that no longer exists to name it.

The contrast is measured twice: `case_merge_guards` deletes after a squash (branch gone) and then, in a second
session, squashes **without** the flag (branch kept, and the ordinary `DELETE`'s `kept.branch` names it). Dropping
the override leaves the first arm green and the second unchanged — so the override is what makes the difference,
and the gate says so.

## D7 — A landed merge answers 200, and a failed delete is logged

If the delete's request half refuses after the landing — a snapshot that cannot be taken, a row that moved under
it — the answer is still `200` with rule 6's three keys. The alternative, answering the delete's `409`, would tell
a client its **work did not land** when the target has already moved, which is the one lie this endpoint must not
tell. The refusal is logged with its code and its sentence, and the row is what says what happened: an `idle` row
means the session survived the request.

The price: a client that asked for a delete and got `200` cannot tell from the answer whether the session went. It
can tell from `GET /api/sessions/{id}` (a row, or `404`), which is what the page already does for every other
state, and adding `kept` to this answer would make the shape depend on the request — ADR-0063's "exactly three
keys" would become "three or four". In practice the interesting case cannot happen: rule 1 refused
`dirty_worktree` a moment earlier, so the worktree is clean, and a clean worktree's snapshot writes no ref and
removes none. A failure here is a machine failure (an unwritable index), not a decision.

## D8 — The 60 seconds has no client in this build, and saying so is the decision

Rule 6's last sentence: "The hub timeout for merge and publish is 60 seconds (the default stays 5)." There is no
hub: forwarding is phase 3 (HS-069 / PER-101), whose slice already carries the line "Default forwarding timeout is
5 s; merge and publish use 60 s" — that story has the number and this one does not. There is also no *client* for
either route: the Changes sheet, its Merge button and the Publish button are HS-058's.

Implementing it here would mean one of two wrong things:

- a blanket `5 s` `fetch` timeout in `web/src/main.ts` — which would cut the **delete's** own bounded wait, since
  a `DELETE` legitimately waits out the abort ladder's `abort_grace` (10 s by default) and then takes a snapshot;
- a `60 s` ceiling on the merge's blocking task — which the spec does not ask for, and which would invent a code
  for "the git work outran the clock", a sentence no rule of item 12 has.

So the pair is recorded, not ticked: HS-069 owns the forwarding client's two numbers and HS-058 owns the buttons'.
The one thing this build can say about it is why a single default would be wrong, which is in
`docs/notes.md`'s landmines so the next agent does not "fix" it by adding a `timeout` to the shared helper.

## D9 — `unsupported` retires with its last field

`422 unsupported` existed for exactly two requests ADR-0063 could not honour — a `strategy` that made a commit,
and `delete_session`. HS-055 built the first and this story builds the second, so the code, its constructor and
its last wire-table row go. An envelope code the server can never send is a promise it does not make, and
`docs/build.md` and `tests/merge.rs` are edited rather than left describing a code nothing produces.

## D10 — `<data>/merge/<id>` is not the delete's to clean

The delete flow removes the worktree, the branch, the network, the container and the session's own directory. It
does not touch `<data>/merge/<id>`: that directory is a merge's, `clear` already removes a leftover from a killed
run before the next landing (ADR-0064 D1), and a delete that swept it would be a second owner of a path with one.
HS-044's reconcile does not mention it either, for the same reason.

## The gate case

`case_merge_guards` boots its own Docker run over its own projects root **and its own data root** (the `_ping`-only
socket of ADR-0064's case), re-makes the worktree the failed network step took back with the step's own command,
and measures six arms: `dirty_worktree` with the target, branch and file untouched and no `<data>/merge` at all; an
ignored file *not* refusing; `session_running` refused and the same row, back in the `error` its failed provision
left, merging; a squash with `delete_session` answering three keys and then taking the row, the worktree and the
branch; a second session's squash without the flag keeping the branch, with the ordinary delete's `kept.branch`
naming it; and a `delete_session` whose snapshot cannot be taken answering `200` and leaving the row.

One write in that case is by hand: the row's `status` column, set with `sqlite3` from `python3`, because a gate may
not run a turn against a `_ping` shim and rule 1's first guard reads one column. `tests/merge.rs` drives the same
row through the server's own `Transitioner`, which is where the state machine is honoured; the case's shortcut is a
fixture, like the worktree it re-makes, and the ADR records it so the next reader does not think a route wrote it.
