# The merge writes refs, so it is its own module, and every answer about what happened is measured

HS-054 is spec item 12's `POST /api/sessions/:id/merge` — rules 2 and 4 of the five the item states — and it is
the first route in this build whose work **writes to the project repository**. Everything before it read: the
changes view's summary and unified diff, the worktree's refusals, `doctor`'s merge trial. That is why the
decisions below are less about new domain facts (there are only three: the target's two checks and the
fast-forward test) than about what a write is allowed to assume.

The item's governing sentence is unchanged and is the whole design: "all git work for review and merge runs on
the host in the project repository; containers never merge".

## D1 — A new `src/merge.rs` and `src/http/merge.rs`, importing the changes view's rules rather than copying them

`changes.rs` is a **reader**: its module doc promises "no write, no fetch, no lock", and HS-053's `diff` already
lives there because a patch is a read. A merge writes refs and a working tree, so putting it in that file would
have made its promise false for every later reader of the module.

The honest argument *against* a new file is that "what is this session's diff base" and "what may I merge into"
are the same two facts. That is answered by import rather than by cohabitation:

- `changes::basis` (the four ordered refusals and the branch's tip) and `changes::dirty` (any line of `git status
  --porcelain`, `--untracked-files=normal --no-optional-locks`) became `pub(crate)` and merge calls them;
- `http::changes::refusal` (the one envelope for `ChangesError`) and `http::changes::NO_WORKTREE` became
  `pub(crate)` and the merge handler calls them.

So there is one spelling of each rule and two modules. The alternative — a `Merge` struct hung on `Worktree` —
was rejected for ADR-0061's reason one layer down: the worktree knows a branch and a directory, and "which
branch may receive this one, and is it checked out" is the review-and-merge item's question.

`worktree.rs` gains one reader (`checkouts`) and no behaviour change to a create refusal; `registrations` now
delegates to it, so `git worktree list --porcelain` has one parser rather than two.

## D2 — The body is the contract's four keys, and the two this build cannot honour are **refused**, not ignored

`{target, strategy?, message?, delete_session?}`. `strategy` absent is `ff-only` (item 12's default), an unknown
key is `422 unknown_field` (the create's rule), and a body that is not an object with a string `target` is `422
bad_body` with a merge-specific sentence (ADR-0008: a code may be shared, a sentence may not).

This story builds `ff-only` only. `strategy: "merge" | "squash"` is HS-055's and `delete_session: true` is
HS-056's, so both answer **`422 unsupported`** with a sentence each:

| Request | Code | Why a refusal rather than a silent `ff-only` |
| --- | --- | --- |
| `strategy: "squash"` | `unsupported` | A client asked for a *different repository outcome*; fast-forwarding what was asked to squash is a lie about the work. |
| `delete_session: true` | `unsupported` | The request says "delete the session". An endpoint that merged and kept the row would be ignoring the most destructive thing it was told. |

A `strategy` that is not one of the spec's three words at all (`"rebase"`) is `422 bad_strategy` instead — a
malformed request, not a missing feature, and `unsupported` would promise that HS-055 adds it.

**`message` is accepted, type-unchecked, and read by nothing.** A fast-forward creates no commit, so there is
nothing for a message to name; the field is in the `FIELDS` allow-list because a client that sends the
contract's body must not be told it sent a field this endpoint does not accept. HS-055, which makes a merge
commit, is what will read it. The asymmetry with the two fields above is deliberate: `message` changes nothing
about what happens to the repository or the row, and those two change everything.

## D3 — Request validity before session state, and the create's order

Body → row (`404`) → the row's `base_commit` (`409 no_worktree`) → the target is in `merge.targets` (`422`) →
the four basis refusals → the target branch exists (`422`, second sentence) → the fast-forward test → the write.

The first two questions are the *request's*, and ADR-0012's create established that a malformed request outranks
a session-state refusal. The rest is the changes view's own "cheapest and most fundamental first": a repository
that is gone is a `409` before any read inside it can become a `500`.

Two facts share `bad_target`, on the `cap_reached` precedent: a name the config does not list, and a listed name
the repository has no branch for — one code, two sentences, because the client's move is one and the operator's
next step is the sentence's. The second is the ordinary case, not an error: `[merge] targets` defaults to
`["main", "master"]` because a machine has one of them (HS-043). It is still a `422` and not a `409` because
ADR-0061's rule applies one route over — "a name the repository does not have is not a target" — and the
parameter is what named it.

`target` gets no alphabet of its own here: `merge.targets` is where a branch name is validated
(`config`'s `is_ref_name`), so membership in that list **is** the validation.

## D4 — The ancestry test is the readable check and the compare-and-swap is the race-free one

`git merge-base --is-ancestor refs/heads/<target> refs/heads/agent/<id>`: exit 0 is "the target is behind (or
equal to) the branch, so this is a fast-forward", exit 1 is `409 not_fast_forward`, anything else is a `500`. The
same command HS-043's keep-rule reads, and read the same way — an exit code that *is* an answer, with the codes
named rather than inferred.

The ancestry test is the primary check because it is the one that explains itself: "the target has commits the
branch does not" is a sentence an operator can act on. The compare-and-swap
(`git update-ref refs/heads/<target> <new> <old>`) is what makes the unchecked-out path race-free: the third
argument makes git refuse the write unless the ref still holds the value this module read, which is exactly the
window a test cannot close. When it refuses, the module re-reads the ref — moved → the same
`409 not_fast_forward` (the same fact observed a moment later), unchanged → a `500` with git's words.

**The CAS's failure arm has no integration test, and cannot have one without a race.** What is measured is the
argument vector: `update_ref_arguments`' unit test pins three arguments with `new` before `old`, and a mutation
that drops `old` turns that test red while the unchecked-out integration test stays green — which is precisely why
the vector is a test and not a comment.

## D5 — Three answers about the checkout, because rule 4 names two and this build can see a third

Rule 4 is written for a world with two shapes: not checked out anywhere, or checked out in the main checkout.
`worktree::checkouts` (path plus the branch each checkout holds, `None` for a detached HEAD) can see three:

| Where the target is checked out | What the merge does | Why |
| --- | --- | --- |
| nowhere | `git update-ref` — rule 4's first arm | no working tree to update |
| the project's own checkout | `git merge --ff-only <new>` there — rule 4's second arm | git updates the tree safely, and refuses to overwrite local work |
| anywhere else | `409 target_checked_out` | in practice a session's worktree, where a container is working |

The third is a refusal on purpose. The alternative — updating whichever checkout has the branch — is the same
git command and would "work", but in a session's worktree it moves an agent's files under a running turn, and
this build cannot tell a session's worktree from an operator's second checkout (`<projects>/.worktrees/<id>` and
`/home/you/main-old` are both just registered paths). Writing a working tree under a container is worse than a
refusal whose sentence says what to change. ADR-0029's rule that a worktree operation is the host's does not
help here: the host is what would be writing into the container's `/work`.

## D6 — No dirtiness pre-check: the merge runs, and a failure is classified by re-measuring two facts

The obvious implementation refuses `target_dirty` by reading `git status --porcelain` in the checkout first. It is
wrong: an **unrelated** dirty file is not "local changes that would be overwritten", and git carries one across a
fast-forward happily. A pre-check would refuse a merge the operator can perform, for a reason that is not true.

So: run `git -c core.hooksPath=/dev/null merge --ff-only <new>` and, **only when it exits non-zero**, ask two
questions of our own:

1. is the target still an ancestor of the branch? No → `409 not_fast_forward` (the target moved);
2. does the checkout hold work? Yes → `409 target_dirty`;
3. otherwise → `500` with git's own words.

Nothing parses git's stderr. That is ADR-0028's and ADR-0045's rule (a build never reads another program's
English) applied to the one command in this build that is *allowed* to fail for three different reasons.
`an_unrelated_dirty_file_does_not_block_the_merge` is the test that makes this a decision with a proof: the
pre-check a first draft would have written turns it red.

After a successful merge in a checkout the target is read once more and compared with the commit the answer is
about to name. That is a postcondition rather than a policy: `merged_commit` claims where the target now is, and
a build that has learned to measure its reads should not infer its writes.

## D7 — The answer is exactly three keys, and the row is untouched

`{merged_commit, target, strategy}`, item 12's rule 6. No additive key: HS-043's `kept` had to be *earned* by a
fact no other surface could carry, and everything this story could add (whether anything moved, where the merge
ran) is derivable from the three — `merged_commit` equal to the branch's tip means nothing moved, and
`GET …/changes` says the rest.

**Nothing about the session row changes.** A merge is not a state transition and not an event: the row is
`Transitioner::apply`'s (ADR-0016), and there is no state a merge moves a session to. `GET …/changes` answers
`ahead: 0` afterwards because `ahead` is `rev-list --count <base>..agent/<id>` and the *target* moved — the branch
did not — so what actually changes is `behind` (to `0`) and, for a branch that was behind, `mergeable`. That is
recorded here because "a merge changed nothing in the answer" is the intuitive and wrong reading.

## D8 — One merge per project at a time is a `std` mutex per repository, held inside the blocking task

`merge::Merges` is `Arc<Mutex<HashMap<PathBuf, Arc<Mutex<()>>>>>`, one entry per repository path, created lazily
and never cleaned (bounded by the projects a machine has, which an operator chooses once). It lives on `AppState`
because two requests must share it, and it is taken **inside** the `spawn_blocking` closure, because the whole
merge is synchronous and a `std` mutex held across an `.await` would park a runtime worker.

The lock is taken before the first git read, not before the write: a second merge that read the target *before*
the first wrote would be deciding on a state that no longer exists, and rule 2's sentence is about the decision
as much as about the write.

**The lock's effect is not observable in an outcome, and that is worth saying rather than hiding.** Without it,
two concurrent merges of two sessions would both pass the ancestry test and then one would lose the
compare-and-swap — the same visible result. So what is measured is the lock's identity (one repository answers one
mutex; a fresh mutex per call turns that test red), and the CAS is what actually protects the ref.

## D9 — Hooks never run, on this path too

Rule 2 spells `-c core.hooksPath=/dev/null` for the temporary worktree HS-055 will build. This story applies it
to the fast-forward as well, because the sentence's reason is not about the temporary worktree: "so that your
hooks never run agent-written code". `post-merge` is the one hook a fast-forward can run, and after an agent has
been writing into `/work` a project's hooks are not code this build will execute on the operator's behalf.

`a_hook_does_not_run` writes a real `post-merge` script and asserts its marker never appears; dropping the flag
turns it red. The cost of the flag is nothing: `merge --ff-only` has no hook this build would want.

## D10 — A second merge of the same session is a `200` no-op, by measurement rather than by a special case

After the first merge the target **is** the branch's tip, so the target is still an ancestor of the branch and
`git merge --ff-only <tip>` answers "Already up to date" with exit 0. The answer names the same
`merged_commit`, moves nothing, and appends no reflog entry — all three asserted.

That is the honest shape for a write whose effect is "make the target be this commit": the request is satisfied,
so it succeeds. A `409` here would say a state is wrong when it is exactly right, and an invented second commit
would be a merge this build has no strategy for.

## D11 — The demo's shape is the fixture's shape, and the gate case measures both update paths apart

`case_merge` boots the artifact over a real repository and a `_ping`-only socket, re-makes the worktree the
failed network step took back (`case_changes`'s trick — a fixture, not a re-implementation of the step), and then
walks the same repository through:

1. **unchecked-out** (`git checkout --detach <base>`): the ref moves, `git rev-parse HEAD` does not, and the
   agent's file never appears in the checkout;
2. the four refused requests, with the target compared across all of them;
3. **checked-out** (`git checkout main`): the ref and the working tree both move, and the checkout is clean;
4. `target_dirty` (an untracked file the branch adds) → `409`, the operator's file untouched, then the same
   merge succeeding once the file is out of the way;
5. `not_fast_forward` (a commit on the target) → `409`, both refs and the checkout unchanged.

Steps 1 and 3 are the same merge in two shapes, and they are the case's point: an implementation that always ran
`merge --ff-only` in the project's checkout would fail step 1's "the file did not appear", and one that always
used `update-ref` would fail step 3's "the file is on disk".

## What this ADR does not decide

The `merge` and `squash` strategies and the temporary merge worktree (HS-055), rule 3's conflict arm, the
`delete_session` half and rule 1's two guards (HS-056), publish (HS-057), the Changes sheet (HS-058), the hub's
60-second merge timeout, and any change to `worktree.rs`'s create refusals. The two `unsupported` sentences in
`http/merge.rs` exist only until HS-055 and HS-056 land, and go with them.
