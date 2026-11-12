# The publish is the host's own `git push` of our branch to somebody else's ref

This is the last route of spec item 12, and the only one in this build whose subject is **not** a ref of ours:
a merge writes `refs/heads/<target>` in the project repository, and a publish writes `refs/heads/<something>` in
a repository this machine does not own, over whatever transport `git remote` names, as the user the server runs
as. ADR-0063 and ADR-0064 already decided what a write to the project repository may assume; ADR-0065 decided
where a session's guards live. This ADR decides what changes when the repository is not ours: what `branch`
names, which names this build will never write, what "refused" means when the remote is the one saying it, and
what "the host's own credentials" is allowed to mean.

Three decisions had a plausible wrong answer and each has a test that goes red when it is taken: the meaning of
`branch` (**D1**), the two pins on the push's argument vector (**D6**), and never force-pushing (**D7**). Two
were *measurements first*: `--porcelain`'s rejection format and `push.followTags`'s effect on a repository whose
tags nobody asked to publish.

## D1 — `branch` is the destination ref; the branch pushed from is always `agent/<id>`

Item 12's sentence is "the server pushes the session branch `agent/<id>`", and `POST …/publish` takes
`{remote, branch?, confirm:true}`. Read together, `branch` can only name the **destination**: the ref pushed from
is fixed, and the refspec is

```text
refs/heads/agent/<id>:refs/heads/<branch>
```

with an absent `branch` meaning the destination is `agent/<id>` too (the ordinary request, and the one the demo
names). The other reading — `branch` is a second *local* branch to push — makes `agent/<id>` optional in a route
whose whole sentence names it, turns the endpoint into a general push endpoint for the repository, and makes
"refuse protected names" meaningless (our branch is never called `main`). It is also the reading in which
`git push origin pr/one` fails for a ref that does not exist locally.

The destination is derived from the session's **id** (`publish::destination`) rather than from a `Worktree` for
one reason: the handler has to ask two config-dependent questions about it — is it protected, is it the merge
target — before it reads the session row at all, and `agent/<id>` is `worktree::BRANCH_PREFIX` plus the id.

## D2 — Protected names are a constant with two arms; the merge target is a second fact

`publish::PROTECTED` is `["main", "master", "release/"]`, item 12's own three, and the predicate has two arms: an
entry ending in `/` is compared against the name's prefix (and the name must be longer, so `release/` itself is
not a branch), every other entry against the whole name. That is `changes::touches_ci`'s shape for its paths, and
for the same measured reason: applying both tests to every entry lets `releaser/x` through one arm and `./main`
through the other.

`merge.targets` is a **second refusal with its own code**, checked after protection, because it is a different
fact: a machine may merge into `trunk` and must still never write `main`, and a destination called `trunk` is not
a protected name. The order is measurable — `branch: "main"` on a machine whose target is `main` answers
`protected_branch`, not `target_not_pushable`, because the constant is true of every machine (the gate's fixture
sets `[merge] targets = ["trunk"]` precisely so the second fact is reachable at all; with the default list the
first would always answer).

## D3 — `confirm` is required, and `false` is a refusal rather than a no-op

The contract writes `confirm:true`, and a publish is the one request in this build that changes something outside
the machine. Two shapes, two codes:

| body | answer |
| --- | --- |
| `confirm` absent, `null` or not a boolean | `422 bad_body` — the body's *shape* (ADR-0064 D5's split) |
| `confirm: false` | `422 confirm_required` — a request that asked **not** to push |

`false` is a decision the client made, so a `200` carrying the three keys would tell it its form was submitted
when it was not. `422` and not `409`: nothing about the machine or the session is in the way — the client's own
form is.

## D4 — The disabled endpoint is `403 publish_disabled`, asked before the row

`publish.remotes` defaults to empty and item 12 says that is what "the endpoint is disabled" means. The
acceptance allowed a `404` or a `403`; this build answers `403` because the route **exists** — `/api/sessions/{id}/publish`
is in the table and being served — and this build's `404 not_found` is "no such endpoint" (its own sentence).

It is asked **before** the session is read, for the reason the drain's `503` is: it is a fact about the process's
config, true of every id, so an unknown id must not be what the route says first. The test asserts it for both a
real id and `0123456789abcdef`.

## D5 — The answer is three keys, and `commit` is the tip read before the push

`{remote, branch, commit}` — `remote` as asked, `branch` the destination's short name, `commit` the commit the
branch pointed at **when the request began**. The commit comes from `changes::basis` and not from the push's own
output: git makes no contract about that output, and a field named `commit` must be measured. A client that wants
more (ahead, behind, `touches_ci`) reads `GET …/changes`, which is unchanged by a publish because the branch did
not move.

No fourth key: `touches_ci` in this answer would be a claim about a diff the route never read (D11).

## D6 — The push carries three pins, and two of them are measurements

```sh
git -C <repo> -c core.hooksPath=/dev/null push --porcelain --no-follow-tags <remote> <refspec>
```

- **`-c core.hooksPath=/dev/null`** is ADR-0064 D2's rule one command further out: a `pre-push` hook is a program
  the host would run with an agent-written branch in its argv, and hooks live in the **common** git directory, so
  the project's own hooks are reachable from the push that never enters the worktree. Measured: a real
  `.git/hooks/pre-push` that `touch`es a marker and exits 1 fired without the flag and never with it.
- **`--no-follow-tags`** because a repository's `push.followTags = true` makes `git push` carry annotated tags
  reachable from the pushed ref **in addition** (measured both ways: the tag landed on the remote without the flag
  and did not with it). A repository config must not decide what this route writes into somebody else's
  repository — ADR-0062's `diff.external` rule, applied to a push. `push.gpgSign` is the same class and is *not*
  pinned: this machine cannot measure a signed push (no key, no signing program), so a machine that configured it
  gets a loud failure rather than a silently different push, and the gap is recorded here instead of hidden.
- **`--porcelain`** is git's documented machine-readable output and is where D7's answer lives.

`remote.<name>.push` is not consulted when a refspec is given on the command line (git's own rule), so a
repository cannot add a second refspec to this push.

## D7 — A rejected push is `409 push_rejected`, read from the porcelain flag, and never retried with force

The refspec has no `+`, the command has no `--force`, `-f`, `--mirror` or `--all`, and a unit test pins the whole
vector plus the absence of any argument starting with `+` — a force-push is not a thing this build can do by
accident, and the moved-on test measures it as a fact about the remote's ref rather than about a flag list: after
a publish, the branch is rewound and the same request is refused **and the remote's ref is byte-for-byte where it
was**.

The classification is read from `--porcelain`'s ref lines, which are stdout and are a documented format:

```text
*	refs/heads/agent/x:refs/heads/agent/x	[new branch]              (exit 0)
=	refs/heads/agent/x:refs/heads/branch-two	[up to date]          (exit 0)
!	refs/heads/agent/x:refs/heads/agent/x	[rejected] (fetch first)  (exit 1)
```

A `!` line is the remote's answer, so it is `PublishError::Rejected { reason }` and the reason word (`fetch
first`, `non-fast-forward`, `stale info`, `remote rejected`) is kept **verbatim for the log** — no decision in
this build branches on its spelling, and a word this build has never seen is still a rejection. A failure with no
`!` line is not a decision anybody made (an unreachable remote, a credential the host does not have) and is a
`500` carrying git's own words, never a refusal invented from prose. This is ADR-0063's "never parse English"
rule stated precisely: `--porcelain` *is* the machine format, exactly as `worktree list --porcelain` and
`status --porcelain` are, and the prose on stderr is not read at all.

## D8 — The child gets `GIT_TERMINAL_PROMPT=0` and nothing else, so the host's own credentials are the host's

"The host's own git credentials" is only meaningful if the push is the host's git: the command inherits the
server's environment (`HOME`, `SSH_AUTH_SOCK`, `GIT_CONFIG_*`) and **sets no credential of its own**. The one
variable added is `GIT_TERMINAL_PROMPT=0`, because a server has no terminal: git's first move when a remote asks
for a username is to prompt on one, and a route that parked a runtime worker on
`Username for 'https://…':` would hang until the operator killed the process. Taking away the keyboard does not
take away the store — credential helpers, `~/.git-credentials` and an `ssh-agent` are all still consulted — and a
missing credential becomes git's own error, which is a `500` with its words.

A unit test pins the whole environment the command sets (`get_envs`), which is both halves at once: exactly one
variable, and no injected credential — but that is a *pin*, not a measurement: `get_envs` reports only what the
command sets explicitly, so adding `.env_clear()` would leave it green while destroying the inheritance. The
**gate** measures the real thing: `case_publish` boots the server with git's `GIT_CONFIG_COUNT=1`,
`GIT_CONFIG_KEY_0=remote.origin.pushurl` and `GIT_CONFIG_VALUE_0=<a second bare repository>`, publishes, and
asserts the ref landed in *that* repository and not at the URL the project's own remote names. Only a push that
inherited the server's environment can put it there, so the arm is the acceptance bullet's proof (the review's
S2, fixed).

## D9 — A remote the config lists but the repository lacks is the same `bad_remote`, its own sentence

`git remote get-url <name>` exits 0 with the URL and **2** when the repository has no such remote (measured;
anything else is a failure, not a `false`). So the pre-check gives the same code as "not listed", with the
sentence that describes its own half — `bad_target`'s two facts, one code, one route over. Without it the same
fact would reach the operator as a `500` carrying `fatal: 'origin' does not appear to be a git repository`, which
describes a machine's problem as if it were a bug.

The ordering is also why an unlisted remote is never handed to git: `publish.remotes`' entries are validated by
`config::is_remote_name`, so a name that reaches the argument vector cannot start with `-`.

## D10 — `changes::basis` is the worktree read, its four refusals in this route's words, and "no commits" is not a refusal

The branch's tip and the four "there is nothing here" facts are the changes view's (`changes::basis`, imported,
`pub(crate)`), answered as this route's `409 no_worktree` with **publish**'s sentences — a refusal
that said "there is nothing to review" would be about another request (ADR-0063 D1, ADR-0008).

A branch with nothing beyond its base commit is **pushable**. Item 12's subject is the branch, and "there is
nothing to publish" is `ahead`'s question — the Changes sheet's, and the to show — not a rule this route
invents. The test publishes a fixture with no session commit and reads the base commit on the remote.

A gone worktree is refused even though the ref alone would be enough to push, because a publish of a session
whose working tree has vanished is not what the operator means; the sentence names the resume, which is what
makes the branch's commits reachable again (`Worktree::ensure` adopts a branch that already exists).

## D11 — `touches_ci` is the changes view's, this route reads none of it, and the warning is that story's

Item 12's bullet — "`touches_ci` is true when the diff changes `.github/`, other CI files or hook directories,
and the UI shows a warning before confirm" — is two facts: the flag, which is `changes::touches_ci` and has been
measured (a directory prefix or a file name, the two arms exclusive, an added file counting), and
the warning, which belongs to a sheet this phase has no screen for. So the route computes neither and the answer
carries no flag; ADR-0064 D10's precedent (the Resolve-with-agent affordance) is the same recording rather than
ticking.

The sentence the confirm step will show, so it is a decision and not a guess:

> this branch changes CI configuration, so publishing it may run workflows in the remote repository

## D12 — Rule 6's 60 seconds stays the hub's

"Returns `{merged_commit, target, strategy}`. The hub timeout for merge and publish is 60 seconds" — the timeout
belongs to the hub's forwarding client (, whose slice already carries "default 5 s; merge and publish use
60 s"), and a server-side ceiling would be a second, guessed answer that a slow remote would hit before the
client's own. ADR-0065 D8 recorded the same for the merge; a push is bounded by the operator's patience and by
`git`'s own transport timeouts, and it cannot prompt (D8).

## D13 — `config::is_ref_name` is the one alphabet for `branch`, and an impossible name is `bad_body`

`merge.targets` is validated by `is_ref_name`, and a publish's `branch` names a ref too — so the function became
`pub(crate)` rather than gaining a second copy (ADR-0063's import-not-copy rule). There is no list to ask about a
destination, so the alphabet **is** the validation, and a name it refuses is the body's shape: `422 bad_body`,
like the merge's empty `message`. An empty string is refused rather than read as "the default" — the default is
the *absent* field, and a client that sent `""` asked for a ref whose name is nothing.

**The review is why the alphabet grew.** `is_ref_name` checked the whole-name rules and the forbidden characters,
but not git's **component** rules, so `a.lock`, `.hidden` and `a/.b` passed it, were not protected and were not a
target, and reached `git push` — which refuses each with `fatal: invalid refspec` **before** contacting a remote
(measured), i.e. a `500` for what is the client's mistake, with a sentence naming the remote. The predicate now
refuses an empty component, a component beginning with `.`, and one ending in `.lock`, which is what
`git check-ref-format` refuses for every case measured here. The cost is stated: `merge.targets` is validated by
the same predicate, so a config naming such a branch is now a config error — which is right, since git would
refuse to create it.

## D14 — The route's order

| # | Question | Answer |
| --- | --- | --- |
| 1 | the body | `bad_body` / `unknown_field` / `branch`'s alphabet (D13) — `parse_body`'s, before the handler does anything |
| 2 | `publish.remotes` is empty | `403 publish_disabled`, before the row (D4) |
| 3 | `confirm` | `422 confirm_required` when false (D3) |
| 4 | `remote` is listed in the config | `422 bad_remote` (D9) |
| 5 | the destination is protected | `422 protected_branch` (D2) |
| 6 | the destination is a `[merge] target` | `422 target_not_pushable` (D2) |
| 7 | the row exists | `404 not_found` |
| 8 | the row has a `base_commit` | `409 no_worktree`, this route's sentence (D10) |
| 9 | the branch's four state facts | `409 no_worktree` (D10) |
| 10 | the remote exists in this repository | `422 bad_remote`, the second sentence (D9) |
| 11 | the push | the three keys, or `409 push_rejected` (D7) |

Steps 1–6 are the request's and are answered before any row is read, which is ADR-0012's order ("a malformed
body is answered the same way whatever the session is") and the reason the disabled endpoint is at 2 — a fact
about the machine, not about the request. Step 1 comes first in **both** senses: it is the body's shape, so a
body whose `branch` is not a ref name is `bad_body` even on a machine whose `publish.remotes` is empty (the
review found the table claiming otherwise, which the code never did). Steps 8–11 run inside one
`spawn_blocking` except for step 8, which is one column, so the ordering is observable as "a refused publish
made no push at all".

## What the gate measures, and what this machine cannot

`case_publish` owns its projects root and its data root, boots `backend = "docker"` over a `_ping`-only socket
(the `case_worktree` trick), re-makes the worktree the failed network step took back, and publishes into a **bare
repository under `$WORK`**: it asserts the 200 and the three keys, `git rev-parse` in the bare repository for
both the ordinary destination and `pr/one`, the four request refusals with the remote's refs unchanged across
them, a rewound branch as `409 push_rejected` with the remote's refs unchanged, and then a second boot with
`[publish] remotes = []` for the `403` — for the row and for an unknown id.

What it cannot measure, said plainly: no container and no real GitHub credential (a bare repository on the host
needs neither, which is the point), no hub (so rule 6's 60 seconds has no client, D12), no UI (the confirm step
and the CI warning are that story's), and no signed push (D6).
