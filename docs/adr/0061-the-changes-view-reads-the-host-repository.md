# The changes view is a host reader, and "mergeable" is three answers rather than two

HS-052 is spec item 12's first bullet — `GET /api/sessions/:id/changes` returning `{branch, base_commit, head,
target, ahead, behind, dirty, mergeable, files[], touches_ci}` — under the item's own sentence: "all git work for
review and merge runs on the host in the project repository; containers never merge". Almost every part is a
neighbour of something that exists: `worktree.rs` already owns the naming rule, the branch and one of the five
questions, the row already carries `worktree` and `base_commit`, `config.merge.targets` already names the target,
and `doctor` already runs `merge-tree --write-tree` as its merge trial. What this ADR records is the eleven
decisions a later reader could take a different way — including the one about how the *test* is built, because the
first version of this story's own proof could not tell `behind` from `ahead`.

## D1 — A new `src/changes.rs` holds the reads and `src/http/changes.rs` the handler

The git half is `changes::Changes::read(&Worktree, base_commit, targets) -> Result<Changes, ChangesError>`:
synchronous, `std`-only, no backend, exactly the shape of `worktree.rs`. The HTTP half is a small file of its own,
the neighbour precedent being `http/tool_output.rs` and `http/tool_image.rs`; `http/sessions.rs` is 1569 lines and
a session's changes do not belong in it.

The alternative — five methods on `Worktree` — was rejected because the *diff basis* and the JSON are the changes
view's, not the worktree's: `ahead` is a worktree fact that predates this story, whereas "what did the branch change
against its base, and would it merge" is a question about a review. `worktree.rs` therefore gains only visibility
(`registered` and `branch_head` become `pub`, the four git-runner functions `pub(crate)`) and no behaviour. The one
place `Command::new("git")` appears in `src/` stays there.

## D2 — The diff's basis is two-dot, `<base_commit>..<head>`, and the field's name says so

`files[]` is the diff between the two commits the answer names, not "since the merge base". The recorded base is by
construction an ancestor — the worktree step is `git worktree add -b agent/<id> <base>` — so two-dot and three-dot
agree unless an agent rewrote history inside `/work`; and when they disagree, "these two trees" is still the honest
answer to "what did this branch change". The JSON names the field `base_commit` rather than `merge_base` for the
same reason. HS-053's diff and HS-054's merge inherit this basis, so it is recorded rather than left to the next
reader.

## D3 — `head` is the branch's tip, not the worktree's checkout

`refs/heads/agent/<id>` is what `ahead`, `behind` and `files[]` are measured on, because the branch is what a merge
takes. The checkout can differ — a detached `HEAD`, or a turn mid-commit — and it is the thing `dirty` describes.
So `head` is `rev-parse --verify refs/heads/agent/<id>` (`Worktree::branch_head`, public since this story), and a
branch that is not there is a refusal rather than an empty answer, because every other field would then be measured
from nothing.

## D4 — `target` is the first **existing** branch of `[merge] targets`, and `null` when there is none

`merge.targets` defaults to `["main", "master"]` because a machine has one of them (HS-043); the answer is the first
the repository actually has, in config order, which is the spec's "the UI preselects the first". When none exists,
`target` is `null` **and so are `behind` and `mergeable`**: there is no branch to be behind and no question to ask,
and `0`/`false` would be claims about a branch that is not there.

## D5 — `dirty` is any line of `git status --porcelain`, and the flags are part of the decision

An untracked file is work and an ignored file is not, which is exactly the definition the WIP snapshot's `add -A`
(ADR-0049) and the delete's `unsaved_work` already use — so `dirty: true` means "the merge's rule 1 would refuse"
rather than being a second opinion about what work is.

Two flags are not inherited defaults but the answer itself:

- **`--untracked-files=normal`**, because `status.showUntrackedFiles = no` in a repository's config would hide an
  agent's new file and `dirty: false` would then be a lie. `tests/changes.rs`'s
  `a_repository_config_cannot_hide_an_untracked_file` sets that config and would read `false` without the flag
  (mutated and watched red).
- **`--no-optional-locks`**, so a read taken while the agent is committing in the same worktree cannot fail on an
  `index.lock` somebody else holds — a read must not be a participant.

## D6 — `mergeable` is `bool | null` with a sibling `mergeable_reason`, never a `bool` over a fallible question

`merge-tree --write-tree` answers three ways: exit 0 is a clean merge (`true`, no reason), exit 1 is a conflict
(`false`, with the conflicted paths in the reason), and anything else — no target, a git without `--write-tree`, a
killed child, a repository git cannot read — is a question that could **not be asked**, which is `null` with git's
own words. Folding that third arm into `false` is precisely the P1 HS-050's review found in `Docker::running`, and
`doctor`'s `pass`/`fail`/`unknown` are the shape this field follows in JSON.

`mergeable_reason` is an **additive key** on the spec's eleven, the precedent being HS-043's `kept` on the delete's
202: a client that does not know it ignores it, and a client that does can say *why* instead of showing a bare
cross.

`Retained::merges_into` (HS-043) is deliberately **not** reused. That is a different question: "can I lose this
branch?" compares trees against existing targets, and a delete must not lose work. "Would a merge conflict?" is the
spec's `merge-tree --write-tree`, which answers about a *hypothetical* merge of two commits. One build, two
questions, two functions.

## D7 — One `409 no_worktree` code, four sentences; a repository git cannot read is a `500`

| Condition | Status | Code | Sentence |
| --- | --- | --- | --- |
| no such session | 404 | `not_found` | the envelope's |
| the row has no `base_commit` | 409 | `no_worktree` | "this session has no worktree, so there is nothing to review" |
| the worktree directory is gone | 409 | `no_worktree` | "the session's worktree is gone…; resume the session…" |
| git does not register the path (a prune) | 409 | `no_worktree` | "git no longer registers the session's worktree…; resume…" |
| the branch is gone | 409 | `no_worktree` | "the session's branch is gone, so there is nothing to compare" |
| the project's repository is gone | 409 | `no_worktree` | "the project's repository is gone…; restore the project's checkout…" |
| git cannot be run / cannot read the repository | 500 | `internal` | ADR-0008's, with git's words in the log |

One code for the four 409s because the *client's* move is one (show the sentence, offer the next step), and four
sentences because the *operator's* next step differs. `409` rather than `404`: the session and the route both
exist; the thing the view would read is not there. The split with `500` is the project refusals' own rule — a state
of the session is a refusal, a machine problem is not (ADR-0030's reasoning, applied to a read).

The 409 is checked **before** any interpretation of git's answers, in the order cheapest-and-most-fundamental
first: repository, directory, registration, branch. A pruned worktree whose directory is back is the case that
proves the order: every read inside it would otherwise fail with git's "not a git repository" and become a 500.

## D8 — `touches_ci` is one constant and one predicate, and an **added** file counts

`changes::CI_PATHS` is the set: directory prefixes (`.github/`, `.gitlab/`, `.circleci/`, `.buildkite/`,
`.woodpecker/`, `.husky/`, `.githooks/`) and file names matched against the last segment (`.gitlab-ci.yml`,
`.travis.yml`, `.drone.yml`, `.cirrus.yml`, `azure-pipelines.yml`, `bitbucket-pipelines.yml`, `appveyor.yml`,
`codecov.yml`, `Jenkinsfile`). `changes::touches_ci(&[FileChange])` is the predicate. Publish (HS-058) reads both
rather than spelling the warning a second time.

Two details: an **added** CI file counts like a modified one (a new workflow is exactly the danger the warning is
for), and `.git/hooks` is *not* in the set — a diff can never contain a path under `.git/`, so the spec's "hook
directories" means the ones a repository keeps in its own tree, which is what `.husky/` and `.githooks/` are.

## D9 — Every read is host git under `spawn_blocking`, with flags that make the answer the command's

Item 12's sentence is the design: the worktree is the host's directory (ADR-0029), the branch is a ref in the
host's repository, and a container is asked nothing — the handler knows no backend, and the module cannot name one.

The two diff reads are `diff --numstat` and `diff --name-status` (git prints only one of them when both are asked —
measured), each with **`--no-ext-diff --no-textconv --no-renames`** and **`-z`**:

- `--no-ext-diff` so a repository's `diff.external` cannot replace git's counts with somebody's program;
- `--no-textconv` so a `.gitattributes` filter cannot make a binary file look textual;
- `--no-renames` so a rename is one spelling per path (a delete plus an add) rather than `{old => new}` in the path
  cell, which would make the two reads unjoinable — and it is a truthful account of the lines;
- `-z` so `core.quotePath` cannot hand back a quoted path.

A file git cannot count lines in (binary) answers `additions: null, deletions: null`, never `0`: `0` would say "no
lines changed" about a file that changed entirely.

The whole read runs on `tokio::task::spawn_blocking`, for the reason the provisioner and the project refusals use
one: five subprocesses must not park a runtime worker.

## D10 — No lock, and `merge-tree --write-tree` is allowed to write objects

Nothing here writes a working tree or a ref, so there is nothing to serialise against the provisioner's per-project
start lock. `merge-tree --write-tree` *does* write unreferenced tree objects into the repository's object store;
that is the command's documented cost and the reason the spec names it, and it is bounded by git's own pruning. A
read that took the start lock would serialize reviews behind container starts for no benefit.

## D11 — The test is built so that a swapped range cannot pass

The first version of `the_summary_names_every_field_and_counts_both_sides` had **one commit on the branch and one on
the target**, so `ahead == behind == 1`, and mutating `behind`'s range from `<branch>..<target>` to
`<target>..<branch>` left the test green — the very bullet ("behind counts commits on the target the branch lacks")
was unmeasured. The fixture now has **two** commits on the branch and one on the target, asserts `ahead` 2 and
`behind` 1, and the mutation goes red; the gate's `case_changes` was given the same asymmetry and an explicit
`counts_differ` assertion, because it had the same shape.

This is recorded as a decision rather than an anecdote because the same trap applies to any "count each way" pair: a
symmetric fixture cannot distinguish a direction, and the only reliable check is to mutate the code and watch the
test fail.

## What this ADR does not decide

The unified diff (`GET …/changes/diff`, HS-053), `POST …/merge` and its five rules (HS-054 – HS-057), `publish`
(HS-058), the Changes sheet, any write of any kind, and any change to `worktree.rs`'s create refusals. `files[]` is
not a diff and carries no hunks by design — the sheet asks for numbers, and the diff is its own endpoint with its
own size limits.
